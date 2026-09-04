use std::{ops::Range, rc::Rc, sync::Arc};

use gpui::{AnyElement, Context, HighlightStyle, Hsla, SharedString, Window};
use ropey::Rope;

use super::{EditorState, FoldRange, InputEdit};

/// Resolves semantic highlight names into renderable GPUI styles.
///
/// Base deliberately knows nothing about a concrete syntax theme. UI crates and
/// applications can provide any resolver, independently of their parser.
pub trait HighlightStyleResolver: Send + Sync {
    fn style(&self, name: &str) -> Option<HighlightStyle>;
}

#[derive(Default)]
struct NoHighlightStyles;

impl HighlightStyleResolver for NoHighlightStyles {
    fn style(&self, _: &str) -> Option<HighlightStyle> {
        None
    }
}

/// Parser-independent syntax highlighting seam consumed by the Base editor.
///
/// Implementations own parsing, incremental state, and language-specific
/// behavior. Base only asks for styled ranges and fold candidates.
pub trait InputHighlighter {
    fn language(&self) -> SharedString;

    fn update(
        &mut self,
        edit: Option<InputEdit>,
        text: &Rope,
        folding: bool,
        window: &mut Window,
        cx: &mut Context<EditorState>,
    );

    /// Return ordered, non-overlapping style runs that fully cover `range`.
    /// Use [`HighlightStyle::default`] for text without a semantic style.
    fn styles(
        &self,
        range: &Range<usize>,
        resolver: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)>;

    fn fold_ranges(&self, text: &Rope) -> Vec<FoldRange>;

    fn fold_ranges_for_edit(&self, range: Range<usize>, text: &Rope) -> Vec<FoldRange> {
        let _ = range;
        self.fold_ranges(text)
    }

    /// Byte ranges inside `range` (buffer offsets) that the editor hides from
    /// display: the text is laid out as if those bytes were not there (zero
    /// width), while hit-testing, caret placement, and selections keep working
    /// in buffer offsets.
    ///
    /// `range` covers exactly one buffer line (without its trailing `\n`) and
    /// is queried once per visible line on every layout. The returned ranges
    /// must be sorted, non-overlapping, within `range`, and on char boundaries.
    /// The engine does not treat the caret line specially: the implementation
    /// is responsible for returning no ranges on the line that holds the caret
    /// (so the raw text shows there) if that is the desired behavior.
    ///
    /// Default: conceal nothing.
    fn conceals(&self, range: &Range<usize>) -> Vec<Range<usize>> {
        let _ = range;
        Vec::new()
    }

    /// Font-size multiplier for the buffer line covering `line_range` (buffer
    /// offsets of the line, without its trailing `\n`), e.g. `1.3` for an H1
    /// line. Rows stay uniformly `line_height` tall; the application picks a
    /// line height that fits the largest scale it returns.
    ///
    /// Default: `1.0` (no change).
    fn line_font_scale(&self, line_range: &Range<usize>) -> f32 {
        let _ = line_range;
        1.0
    }

    /// Inline widgets to draw over the text of the buffer line covering
    /// `line_range`, e.g. a checkbox in place of `[ ]`.
    ///
    /// Each widget names the raw byte range it stands for. The engine draws it
    /// at that range's on-screen position and routes clicks to it; the text
    /// underneath is *not* hidden by this — conceal it from `conceals` if it
    /// should not show, which also keeps the caret-line rule in one place.
    ///
    /// Queried once per visible line on every layout, so an implementation
    /// should be as cheap as `conceals` is.
    ///
    /// Default: no widgets.
    fn inline_widgets(&self, line_range: &Range<usize>) -> Vec<InlineWidget> {
        let _ = line_range;
        Vec::new()
    }

    /// Extra height for the buffer line covering `line_range`, as a multiple of
    /// the base line height, on top of what `line_font_scale` already implies.
    ///
    /// Room without the text growing: a line can ask for more height than
    /// its glyphs take, which is the part `line_font_scale` cannot express.
    /// (Table rows do not use it: their height comes from their cells.)
    ///
    /// Default: `1.0` (the line takes the room its text needs).
    fn line_height_scale(&self, line_range: &Range<usize>) -> f32 {
        let _ = line_range;
        1.0
    }

    /// The table row the buffer line covering `line_range` is, if it is one.
    ///
    /// A GFM pipe table is laid out by the engine as one shaped segment per
    /// cell, so it needs the row's segmentation: which bytes of the line are
    /// which cell. That is the application's parser's business, and it is the
    /// *same* segmentation the application writes through, or the grid and
    /// the write path disagree on a byte.
    ///
    /// Asked at wrap time, inside the edit, with `text` as it will be after
    /// the edit: the answer must come from that text, never from a syntax
    /// tree that is updated later. `generation` changes once per rebuild, so
    /// an implementation can cache one table's shape across the rows of that
    /// rebuild rather than parse the table once per row.
    ///
    /// Default: no line is a table row.
    fn table_row(
        &self,
        line_range: &Range<usize>,
        text: &Rope,
        generation: u64,
    ) -> Option<TableRow> {
        let _ = (line_range, text, generation);
        None
    }
}

/// A widget drawn in place of a range of text (see
/// [`InputHighlighter::inline_widgets`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineWidget {
    /// Raw byte range in the buffer that this widget stands for.
    pub range: Range<usize>,
    pub kind: InlineWidgetKind,
}

/// What an [`InlineWidget`] draws. Deliberately a closed set rather than a
/// callback: the engine has to lay these out during `prepaint`, where an
/// application-supplied element would have nowhere safe to come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineWidgetKind {
    /// A GFM task checkbox. Clicking it asks the application to toggle.
    Checkbox { checked: bool },
}

/// One row of a GFM pipe table, as the application segments it for the
/// engine (see [`InputHighlighter::table_row`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRow {
    /// Buffer rows of the table's first line (the header) and its last line.
    /// Used to rebuild the rows of a table whose shape changed; a row is not
    /// drawn any differently for being first or last.
    pub first_row: usize,
    pub last_row: usize,
    pub kind: TableRowKind,
    /// Columns the table has, as its header spells them.
    pub columns: usize,
    pub aligns: Vec<ColumnAlign>,
    /// Exactly `columns` cells, as byte ranges relative to the line start.
    pub cells: Vec<TableCellSpan>,
}

/// What a table row is to a reader: the header, the delimiter under it, or a
/// body row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableRowKind {
    Header,
    Delimiter,
    Body,
}

/// One cell of a table row, as byte ranges relative to the line start.
///
/// `content` is the cell's text without padding -- an empty cell is an empty
/// range in the middle of its padding, so a keystroke there lands inside the
/// cell. `separator` is the index of the pipe closing the cell, or the line's
/// length when there is none; every byte of the line is inside a cell or
/// between two known separators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableCellSpan {
    pub content: Range<usize>,
    pub separator: usize,
}

/// Column alignment of a GFM pipe table, as its delimiter row spells it
/// (`:---`, `:---:`, `---:`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColumnAlign {
    #[default]
    Left,
    Center,
    Right,
}

pub type InputHighlighterFactory = Rc<dyn Fn(&str) -> Option<Box<dyn InputHighlighter>>>;
pub type SharedHighlightStyleResolver = Arc<dyn HighlightStyleResolver>;
pub type FoldIconRenderer = Rc<dyn Fn(usize, bool) -> AnyElement>;

#[derive(Clone, Copy, Default)]
pub struct DiagnosticColors {
    pub error: Hsla,
    pub warning: Hsla,
    pub info: Hsla,
    pub hint: Hsla,
}

/// Application-owned colors and highlight resolver consumed by editor painting.
#[derive(Clone)]
pub struct InputEditorStyle {
    pub foreground: Hsla,
    pub muted_foreground: Hsla,
    pub background: Hsla,
    pub border: Hsla,
    pub selection: Hsla,
    pub caret: Hsla,
    pub diagnostics: DiagnosticColors,
    pub highlight_styles: SharedHighlightStyleResolver,
    pub editor_invisible: Option<Hsla>,
    pub editor_active_line: Option<Hsla>,
    pub editor_gutter_background: Option<Hsla>,
    pub fold_icon_renderer: Option<FoldIconRenderer>,
}

impl Default for InputEditorStyle {
    fn default() -> Self {
        Self {
            foreground: Hsla::default(),
            muted_foreground: Hsla::default(),
            background: Hsla::default(),
            border: Hsla::default(),
            selection: Hsla::default(),
            caret: Hsla::default(),
            diagnostics: DiagnosticColors::default(),
            highlight_styles: Arc::new(NoHighlightStyles),
            editor_invisible: None,
            editor_active_line: None,
            editor_gutter_background: None,
            fold_icon_renderer: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Rope;

    /// A highlighter that implements only what the trait demands, so the
    /// defaults are what the engine sees.
    struct Bare;

    impl InputHighlighter for Bare {
        fn language(&self) -> SharedString {
            "bare".into()
        }

        fn update(
            &mut self,
            _edit: Option<InputEdit>,
            _text: &Rope,
            _folding: bool,
            _window: &mut Window,
            _cx: &mut Context<EditorState>,
        ) {
        }

        fn styles(
            &self,
            range: &Range<usize>,
            _resolver: &dyn HighlightStyleResolver,
        ) -> Vec<(Range<usize>, HighlightStyle)> {
            vec![(range.clone(), HighlightStyle::default())]
        }

        fn fold_ranges(&self, _text: &Rope) -> Vec<FoldRange> {
            Vec::new()
        }
    }

    /// Table rows are opt-in: a highlighter written before the hook existed
    /// keeps laying every line out as prose.
    #[test]
    fn a_highlighter_that_ignores_the_hook_reports_no_table_rows() {
        assert!(Bare.table_row(&(0..10), &Rope::from("| a |"), 0).is_none());
    }
}
