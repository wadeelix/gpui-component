use crate::input::InputModeKind;
use gpui::{Context, Window};
use regex::{Regex, RegexBuilder};
use ropey::Rope;
use std::{ops::Range, rc::Rc};

use super::{InputBaseState, Replace, Search, movement::MoveDirection, state::ScrollPadding};

/// How a search query matches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchOptions {
    pub case_sensitive: bool,
    /// Only where the query stands as a whole word.
    pub whole_word: bool,
    /// The query is a regular expression rather than literal text.
    pub regex: bool,
}

/// The most a compiled query may take. A pattern past this is refused with
/// an error rather than allowed to allocate without bound.
const PATTERN_SIZE_LIMIT: usize = 8 * 1024 * 1024;

/// Stateful, presentation-independent search engine used by text inputs.
///
/// Every query compiles to one regular expression, literal text escaped:
/// that gives Unicode case folding (an ASCII-only fold does not find
/// "заметка" for "Заметка"), whole words by `\b`, and regex mode from the
/// same code path.
#[derive(Debug, Clone)]
pub struct SearchMatcher {
    text: Rope,
    pattern: Option<Regex>,
    options: SearchOptions,
    /// Why the query did not compile, in regex mode.
    error: Option<String>,
    /// Only matches inside this byte range count: search within a selection.
    scope: Option<Range<usize>>,
    matched_ranges: Rc<Vec<Range<usize>>>,
    current_match_ix: usize,
    replacing: bool,
    /// The text changed while nobody was looking at the matches; they are
    /// worked out again when someone does.
    stale: bool,
}

#[derive(Debug, Clone)]
pub struct SearchSession {
    pub open: bool,
    pub replace_mode: bool,
    pub case_insensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    /// Whether matches are limited to the selection made when this was turned
    /// on.
    pub in_selection: bool,
    pub query: String,
    pub replacement: String,
    pub anchor_offset: Option<usize>,
    pub matcher: SearchMatcher,
}

impl Default for SearchSession {
    fn default() -> Self {
        Self {
            open: false,
            replace_mode: false,
            case_insensitive: true,
            whole_word: false,
            regex: false,
            in_selection: false,
            query: String::new(),
            replacement: String::new(),
            anchor_offset: None,
            matcher: SearchMatcher::new(),
        }
    }
}

impl SearchSession {
    pub(crate) fn open(&mut self, replace_mode: bool, replaceable: bool) {
        self.open = true;
        self.replace_mode = replace_mode && replaceable;
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
    }

    pub(crate) fn update_query(&mut self, query: impl Into<String>, case_insensitive: bool) {
        let options = SearchOptions {
            case_sensitive: !case_insensitive,
            ..self.options()
        };
        self.update_query_with(query, options);
    }

    pub(crate) fn update_query_with(&mut self, query: impl Into<String>, options: SearchOptions) {
        let query = query.into();
        if self.query == query && self.options() == options {
            return;
        }

        self.query = query;
        self.case_insensitive = !options.case_sensitive;
        self.whole_word = options.whole_word;
        self.regex = options.regex;
        self.matcher.set_query(&self.query, options);
    }

    /// The options the session matches with.
    pub fn options(&self) -> SearchOptions {
        SearchOptions {
            case_sensitive: !self.case_insensitive,
            whole_word: self.whole_word,
            regex: self.regex,
        }
    }
}

impl<M: InputModeKind> InputBaseState<M> {
    /// Open the search session, or re-invoke it if it is already open.
    ///
    /// This is not idempotent: every call advances
    /// [`InputBaseState::search_activation_revision`], and the presentation
    /// layer answers that by re-focusing the search field and selecting its
    /// contents, the same as pressing the shortcut a second time. Call it from
    /// an action or another user gesture, never from a render pass or an
    /// observer that runs every frame — that would re-select the field under
    /// the user on every frame and make it impossible to type.
    pub fn open_search(&mut self, replace_mode: bool, cx: &mut Context<Self>) {
        if !self.searchable {
            return;
        }
        self.search_activation_revision = self.search_activation_revision.wrapping_add(1);
        self.search_session
            .open(replace_mode, self.is_replaceable());
        let selected = self.selected_text().to_string();
        let query = if selected.is_empty() {
            self.search_session.query.clone()
        } else {
            selected
        };
        let query_changed = query != self.search_session.query;
        // A retained query resumes its previous occurrence. Only a new query
        // is anchored to the current viewport.
        self.search_session.anchor_offset = if query_changed {
            self.last_layout
                .as_ref()
                .map(|layout| layout.visible_range_offset.start)
        } else {
            None
        };
        let case_insensitive = self.search_session.case_insensitive;
        self.search_session.update_query(query, case_insensitive);
        self.search_session.matcher.update(&self.text);
        if query_changed && let Some(anchor) = self.search_session.anchor_offset {
            self.search_session.matcher.update_cursor_by_offset(anchor);
        }
        cx.notify();
    }

    pub fn search_session(&self) -> &SearchSession {
        &self.search_session
    }

    /// A counter that advances every time [`InputBaseState::open_search`] runs,
    /// including while the session is already open.
    ///
    /// Re-invoking search leaves the session itself identical, so a presentation
    /// layer that decides what to rebuild by comparing session state cannot see
    /// the second request. Fold this into that comparison to notice it.
    pub fn search_activation_revision(&self) -> u64 {
        self.search_activation_revision
    }

    #[doc(hidden)]
    pub fn set_search_replace_mode(&mut self, replace_mode: bool, cx: &mut Context<Self>) {
        self.search_session.replace_mode = replace_mode && self.is_replaceable();
        cx.notify();
    }

    /// Returns true if the search panel can replace the matches.
    ///
    /// This is false when the input is not `replaceable`, or when it is
    /// `disabled` or `readonly`.
    pub fn is_replaceable(&self) -> bool {
        self.replaceable && self.is_editable()
    }

    pub fn set_search_query(
        &mut self,
        query: impl Into<String>,
        case_insensitive: bool,
        cx: &mut Context<Self>,
    ) {
        self.search_session.update_query(query, case_insensitive);
        self.search_session.matcher.update(&self.text);
        cx.notify();
    }

    /// Sets the query together with how it matches: case, whole words,
    /// regular expression.
    pub fn set_search_options(
        &mut self,
        query: impl Into<String>,
        options: SearchOptions,
        cx: &mut Context<Self>,
    ) {
        self.search_session.update_query_with(query, options);
        self.search_session.matcher.update(&self.text);
        cx.notify();
    }

    /// Limits the search to the current selection, or lifts the limit. A
    /// selection that is empty limits nothing.
    pub fn set_search_in_selection(&mut self, in_selection: bool, cx: &mut Context<Self>) {
        let selected = self.selected_range();
        let scope = (in_selection && !selected.is_empty()).then_some(selected);
        self.search_session.in_selection = scope.is_some();
        self.search_session.matcher.update(&self.text);
        self.search_session.matcher.set_scope(scope);
        cx.notify();
    }

    pub fn close_search(&mut self, cx: &mut Context<Self>) {
        self.search_session.close();
        cx.notify();
    }

    pub fn next_search_match(&mut self, cx: &mut Context<Self>) -> Option<Range<usize>> {
        let range = self.search_session.matcher.next()?;
        // Match order does not describe viewport direction after a manual
        // scroll. Always allow search navigation to reveal the active match.
        self.scroll_to_with_padding(range.end, None, ScrollPadding::SurroundingLines, cx);
        Some(range)
    }

    pub fn previous_search_match(&mut self, cx: &mut Context<Self>) -> Option<Range<usize>> {
        let range = self.search_session.matcher.next_back()?;
        // Match order does not describe viewport direction after a manual
        // scroll. Always allow search navigation to reveal the active match.
        self.scroll_to_with_padding(range.start, None, ScrollPadding::SurroundingLines, cx);
        Some(range)
    }

    pub fn replace_current_search_match(
        &mut self,
        replacement: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.is_replaceable() {
            return false;
        }
        let matcher = &mut self.search_session.matcher;
        let Some(range) = matcher
            .matched_ranges()
            .get(matcher.current_match_index())
            .cloned()
        else {
            return false;
        };
        let replacement = matcher.replacement_for(&range, replacement);
        let next = matcher.peek().unwrap_or_else(|| range.clone());
        let direction = matcher
            .has_next_without_wrap()
            .then_some(MoveDirection::Down);
        if direction.is_none() {
            matcher.set_current_match_index(0);
        }
        matcher.begin_replacement();
        matcher.shift_scope_for(&range, replacement.len());
        let range_utf16 = self.range_to_utf16(&range);
        self.scroll_to(next.end, direction, cx);
        self.replace_text_in_range_silent(Some(range_utf16), &replacement, window, cx);
        true
    }

    pub fn replace_all_search_matches(
        &mut self,
        replacement: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> usize {
        if !self.is_replaceable() {
            return 0;
        }
        let edits = self.search_session.matcher.replacements(replacement);
        if edits.is_empty() {
            return 0;
        }
        let count = edits.len();
        // Where the caret ends up: moved by every replacement before it, so
        // the reader stays where they were rather than at the top of the note.
        let cursor = self.cursor();
        let mut caret = cursor;
        for (range, new_text) in &edits {
            if range.end <= cursor {
                caret = caret + new_text.len() - range.len();
            } else if range.start < cursor {
                caret = caret + range.start + new_text.len() - cursor.min(range.end);
            }
        }
        self.search_session.matcher.begin_replacement();
        self.search_session.matcher.set_scope(None);
        self.search_session.in_selection = false;
        // One transaction of exact edits rather than the whole text replaced:
        // one undo step, and only the edited ranges are parsed again.
        self.replace_text_in_ranges(&edits, window, cx);
        let caret = caret.min(self.text.len());
        self.set_selected_range(caret..caret, cx);
        count
    }

    /// Keeps the matches following the text -- while the panel is open. A
    /// closed panel keeps its query but not its matches: recomputing them was
    /// a scan of the whole document on every keystroke nobody was searching.
    pub(super) fn update_search(&mut self, _cx: &mut gpui::App) {
        if self.search_session.open {
            self.search_session.matcher.update(&self.text);
        } else {
            self.search_session.matcher.defer(&self.text);
        }
    }

    pub(super) fn on_action_search(&mut self, _: &Search, _: &mut Window, cx: &mut Context<Self>) {
        if !self.searchable {
            return;
        }
        self.open_search(false, cx);
    }

    pub(super) fn on_action_replace(
        &mut self,
        _: &Replace,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.searchable {
            return;
        }
        self.open_search(true, cx);
    }
}

impl Default for SearchMatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchMatcher {
    pub fn new() -> Self {
        Self {
            text: "".into(),
            pattern: None,
            options: SearchOptions::default(),
            error: None,
            scope: None,
            matched_ranges: Rc::new(Vec::new()),
            current_match_ix: 0,
            replacing: false,
            stale: false,
        }
    }

    /// Update the source text and recompute matches.
    pub fn update(&mut self, text: &Rope) {
        if !self.stale && self.text.eq(text) {
            self.replacing = false;
            return;
        }
        if !self.replacing {
            // A selection's offsets mean nothing once the text around them
            // changed by anything but a replacement.
            self.scope = None;
        }
        self.text = text.clone();
        self.stale = false;
        self.update_matches();
    }

    /// Takes the new text without matching it. `update` works the matches
    /// out when they are needed again.
    pub fn defer(&mut self, text: &Rope) {
        self.text = text.clone();
        self.scope = None;
        self.stale = true;
    }

    pub fn update_query(&mut self, query: &str, case_insensitive: bool) {
        let options = SearchOptions {
            case_sensitive: !case_insensitive,
            ..self.options
        };
        self.set_query(query, options);
    }

    /// Compiles `query` under `options` and matches it.
    pub fn set_query(&mut self, query: &str, options: SearchOptions) {
        self.options = options;
        self.error = None;
        self.pattern = None;
        if !query.is_empty() {
            match compile(query, options) {
                Ok(pattern) => self.pattern = Some(pattern),
                Err(error) => self.error = Some(error),
            }
        }
        self.stale = false;
        self.update_matches();
    }

    /// Why the query does not compile, when it is an invalid regex.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Limits matches to `scope`, a byte range of the text.
    pub fn set_scope(&mut self, scope: Option<Range<usize>>) {
        self.scope = scope;
        self.update_matches();
    }

    /// Keeps a scope covering the same text after `range` is replaced by
    /// `replacement_len` bytes.
    fn shift_scope_for(&mut self, range: &Range<usize>, replacement_len: usize) {
        if let Some(scope) = self.scope.as_mut()
            && range.end <= scope.end
        {
            scope.end = scope.end + replacement_len - range.len();
        }
    }

    /// What replaces the match at `range`: `template` as written, or in regex
    /// mode with `$1` and `${name}` filled from that match.
    pub fn replacement_for(&self, range: &Range<usize>, template: &str) -> String {
        match (&self.pattern, self.options.regex) {
            (Some(pattern), true) => {
                let text = self.text.to_string();
                expand(pattern, &text, range, template).unwrap_or_else(|| template.to_owned())
            }
            _ => template.to_owned(),
        }
    }

    /// Every match with what replaces it, in order.
    pub fn replacements(&self, template: &str) -> Vec<(Range<usize>, String)> {
        let ranges = self.matched_ranges.as_ref();
        match (&self.pattern, self.options.regex) {
            (Some(pattern), true) => {
                let text = self.text.to_string();
                ranges
                    .iter()
                    .map(|range| {
                        let replacement = expand(pattern, &text, range, template)
                            .unwrap_or_else(|| template.to_owned());
                        (range.clone(), replacement)
                    })
                    .collect()
            }
            _ => ranges
                .iter()
                .map(|range| (range.clone(), template.to_owned()))
                .collect(),
        }
    }

    pub fn matched_ranges(&self) -> Rc<Vec<Range<usize>>> {
        self.matched_ranges.clone()
    }

    pub fn current_match_index(&self) -> usize {
        self.current_match_ix
    }

    pub fn len(&self) -> usize {
        self.matched_ranges.len()
    }

    pub fn is_empty(&self) -> bool {
        self.matched_ranges.is_empty()
    }

    pub fn label(&self) -> String {
        if self.is_empty() {
            "0/0".into()
        } else {
            format!("{}/{}", self.current_match_ix + 1, self.len())
        }
    }

    fn peek(&self) -> Option<Range<usize>> {
        self.next_index()
            .and_then(|ix| self.matched_ranges.get(ix).cloned())
    }

    fn has_next_without_wrap(&self) -> bool {
        self.current_match_ix < self.matched_ranges.len().saturating_sub(1)
    }

    pub fn update_cursor_by_offset(&mut self, offset: usize) {
        for (ix, range) in self.matched_ranges.iter().enumerate() {
            self.current_match_ix = ix;
            if range.contains(&offset) || range.end >= offset {
                return;
            }
        }
    }

    /// Preserve the current logical match while a replacement mutates text.
    pub(crate) fn begin_replacement(&mut self) {
        self.replacing = true;
    }

    fn set_current_match_index(&mut self, index: usize) {
        self.current_match_ix = index.min(self.matched_ranges.len().saturating_sub(1));
    }

    fn next_index(&self) -> Option<usize> {
        if self.is_empty() {
            None
        } else if self.has_next_without_wrap() {
            Some(self.current_match_ix + 1)
        } else {
            Some(0)
        }
    }

    fn update_matches(&mut self) {
        let mut ranges = Vec::new();
        if let Some(pattern) = &self.pattern {
            let text = self.text.to_string();
            let scope = self
                .scope
                .clone()
                .map(|scope| scope.start.min(text.len())..scope.end.min(text.len()))
                .unwrap_or(0..text.len());
            // Over the whole text, so an anchor or a word boundary sees the
            // text around the scope; the scope then filters. An empty match
            // (`^`, `a*`) marks nothing a reader could replace.
            ranges.extend(
                pattern
                    .find_iter(&text)
                    .filter(|found| found.start() < found.end())
                    .filter(|found| found.start() >= scope.start && found.end() <= scope.end)
                    .map(|found| found.range()),
            );
        }
        self.matched_ranges = Rc::new(ranges);
        if !self.replacing || self.is_empty() {
            self.current_match_ix = 0;
        } else {
            self.current_match_ix = self.current_match_ix.min(self.len() - 1);
        }
        self.replacing = false;
    }
}

/// `query` as the one regular expression it matches as.
fn compile(query: &str, options: SearchOptions) -> Result<Regex, String> {
    let body = if options.regex {
        query.to_owned()
    } else {
        regex::escape(query)
    };
    let body = if options.whole_word {
        format!(r"\b(?:{body})\b")
    } else {
        body
    };
    RegexBuilder::new(&body)
        .case_insensitive(!options.case_sensitive)
        .multi_line(true)
        .size_limit(PATTERN_SIZE_LIMIT)
        .build()
        .map_err(|err| err.to_string())
}

/// `template` expanded from the match of `pattern` at exactly `range`.
fn expand(pattern: &Regex, text: &str, range: &Range<usize>, template: &str) -> Option<String> {
    let captures = pattern.captures_at(text, range.start)?;
    let whole = captures.get(0)?;
    if whole.range() != *range {
        return None;
    }
    let mut out = String::new();
    captures.expand(template, &mut out);
    Some(out)
}

impl Iterator for SearchMatcher {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Self::Item> {
        let ix = self.next_index()?;
        self.current_match_ix = ix;
        self.matched_ranges.get(ix).cloned()
    }
}

impl DoubleEndedIterator for SearchMatcher {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.is_empty() {
            return None;
        }
        if self.current_match_ix == 0 {
            self.current_match_ix = self.len();
        }
        self.current_match_ix -= 1;
        self.matched_ranges.get(self.current_match_ix).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_navigates_and_preserves_replacement_position() {
        let mut matcher = SearchMatcher::new();
        matcher.update(&Rope::from("foo FOO foo"));
        matcher.update_query("foo", true);
        assert_eq!(&*matcher.matched_ranges(), &[0..3, 4..7, 8..11]);
        assert_eq!(matcher.next(), Some(4..7));
        assert_eq!(matcher.next_back(), Some(0..3));

        matcher.set_current_match_index(2);
        matcher.begin_replacement();
        matcher.update(&Rope::from("foo FOO bar"));
        assert_eq!(matcher.current_match_index(), 1);
    }

    #[test]
    fn next_wraps_to_start() {
        let mut matcher = SearchMatcher::new();
        matcher.update(&Rope::from(".....aaaaa.....aaaaa.....aaaaa"));
        matcher.update_query("aaaaa", false);
        matcher.set_current_match_index(2);
        assert_eq!(matcher.next(), Some(5..10));
    }

    #[test]
    fn identical_query_keeps_the_current_match() {
        let mut session = SearchSession::default();
        session.update_query("foo", true);
        session.matcher.update(&Rope::from("foo bar foo baz foo"));
        session.matcher.update_cursor_by_offset(12);
        assert_eq!(session.matcher.current_match_index(), 2);

        // Reopening Find and the styled search panel's initial query echo both
        // update the session with the same query. Neither should reset the
        // previously active occurrence.
        session.update_query("foo", true);

        assert_eq!(session.matcher.current_match_index(), 2);
        assert_eq!(session.matcher.label(), "3/3");
    }

    #[test]
    fn replacement_keeps_current_match_index_on_next_match() {
        let mut matcher = SearchMatcher::new();
        matcher.update(&Rope::from("foo foo foo"));
        matcher.update_query("foo", true);
        assert_eq!(matcher.label(), "1/3");

        assert!(matcher.has_next_without_wrap());
        matcher.begin_replacement();
        matcher.update(&Rope::from("bar foo foo"));
        assert_eq!(matcher.current_match_index(), 0);
        assert_eq!(matcher.matched_ranges()[0], 4..7);
        assert_eq!(matcher.label(), "1/2");

        matcher.set_current_match_index(1);
        assert!(!matcher.has_next_without_wrap());
        matcher.set_current_match_index(0);
        matcher.begin_replacement();
        matcher.update(&Rope::from("bar foo bar"));
        assert_eq!(matcher.current_match_index(), 0);
        assert_eq!(matcher.matched_ranges()[0], 4..7);
        assert_eq!(matcher.label(), "1/1");
    }

    #[test]
    fn update_matches_clamps_current_match_index_while_replacing() {
        let mut matcher = SearchMatcher::new();
        matcher.update(&Rope::from("foo foo foo"));
        matcher.update_query("foo", true);
        matcher.set_current_match_index(2);
        matcher.begin_replacement();

        matcher.update(&Rope::from("foo xoo foo"));

        assert_eq!(matcher.len(), 2);
        assert_eq!(matcher.current_match_index(), 1);
        assert_eq!(matcher.label(), "2/2");
    }
}

#[cfg(test)]
mod option_tests {
    use super::*;

    fn matcher(text: &str, query: &str, options: SearchOptions) -> SearchMatcher {
        let mut matcher = SearchMatcher::new();
        matcher.update(&Rope::from(text));
        matcher.set_query(query, options);
        matcher
    }

    fn found(matcher: &SearchMatcher) -> Vec<Range<usize>> {
        matcher.matched_ranges().as_ref().clone()
    }

    /// An ASCII-only fold finds nothing here; the reader of a Russian note
    /// expects both.
    #[test]
    fn case_folds_beyond_ascii() {
        let text = "Заметка и заметка";
        let loose = matcher(text, "заметка", SearchOptions::default());
        assert_eq!(loose.len(), 2);
        let strict = matcher(
            text,
            "заметка",
            SearchOptions {
                case_sensitive: true,
                ..Default::default()
            },
        );
        assert_eq!(found(&strict), [text.rfind("заметка").unwrap()..text.len()]);
    }

    #[test]
    fn whole_word_skips_the_word_inside_another() {
        let options = SearchOptions {
            whole_word: true,
            ..Default::default()
        };
        assert_eq!(
            found(&matcher("cat concat cat.", "cat", options)),
            [0..3, 11..14]
        );
    }

    #[test]
    fn literal_text_is_not_a_pattern() {
        assert_eq!(
            found(&matcher("a.b axb", "a.b", SearchOptions::default())),
            [0..3]
        );
    }

    #[test]
    fn a_regex_matches_and_an_invalid_one_says_why() {
        let options = SearchOptions {
            regex: true,
            ..Default::default()
        };
        assert_eq!(found(&matcher("a1 b22", r"\d+", options)), [1..2, 4..6]);

        let broken = matcher("a1 b22", "(", options);
        assert!(broken.is_empty());
        assert!(broken.error().is_some());

        let fixed = {
            let mut m = broken;
            m.set_query(r"\d", options);
            m
        };
        assert!(fixed.error().is_none(), "a fixed query clears the error");
    }

    /// `^` or `a*` match nothing a reader could see or replace.
    #[test]
    fn empty_matches_are_not_matches() {
        let options = SearchOptions {
            regex: true,
            ..Default::default()
        };
        assert!(matcher("one\ntwo", "^", options).is_empty());
        assert_eq!(found(&matcher("baab", "a*", options)), [1..3]);
    }

    #[test]
    fn a_regex_replacement_fills_its_groups_and_a_literal_one_does_not() {
        let options = SearchOptions {
            regex: true,
            ..Default::default()
        };
        let regex = matcher("ann@home bob@work", r"(\w+)@(\w+)", options);
        assert_eq!(
            regex.replacements("$2 at ${1}"),
            [
                (0..8, "home at ann".to_string()),
                (9..17, "work at bob".to_string())
            ]
        );
        let literal = matcher("ann@home", "@", SearchOptions::default());
        assert_eq!(literal.replacements("$1"), [(3..4, "$1".to_string())]);
    }

    #[test]
    fn a_scope_limits_the_matches_and_an_edit_lifts_it() {
        let mut m = matcher("foo foo foo", "foo", SearchOptions::default());
        m.set_scope(Some(4..11));
        assert_eq!(found(&m), [4..7, 8..11]);
        m.update(&Rope::from("foo foo foo!"));
        assert_eq!(m.len(), 3, "typing ended the selection's scope");
    }

    /// A closed panel's matcher only takes the text; the scan waits until the
    /// matches are asked for.
    #[test]
    fn deferred_text_is_matched_when_asked() {
        let mut m = matcher("foo", "foo", SearchOptions::default());
        m.defer(&Rope::from("foo foo"));
        assert_eq!(m.len(), 1, "nothing was scanned yet");
        m.update(&Rope::from("foo foo"));
        assert_eq!(m.len(), 2);
    }
}
