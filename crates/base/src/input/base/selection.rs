use crate::input::InputModeKind;
use std::ops::Range;

use gpui::{Context, Window};
use ropey::Rope;
use sum_tree::Bias;

use super::{InputBaseState, RopeExt as _};
use crate::text_boundary::word_range_from_chars;

impl<M: InputModeKind> InputBaseState<M> {
    /// Select the word at the given offset on double-click.
    ///
    /// The offset is the UTF-8 offset.
    pub(super) fn select_word(&mut self, offset: usize, _: &mut Window, cx: &mut Context<Self>) {
        let Some(range) = TextSelector::word_range(&self.text, offset) else {
            return;
        };
        let range = self.word_within_cell(offset, range);

        self.undo_manager.break_transaction_coalescing();
        self.selected_range = (range.start..range.end).into();
        self.selected_word_range = (!range.is_empty()).then_some(self.selected_range);
        cx.notify()
    }

    /// A word double-clicked in a table cell stays inside the cell: the
    /// text's run of padding spaces and pipes is not a word to the writer.
    /// Past the cell's last word (the click landed after it) that word is
    /// taken; in an empty cell nothing is, and the caret just lands.
    fn word_within_cell(&self, offset: usize, range: Range<usize>) -> Range<usize> {
        let Some(layout) = self.last_layout.as_ref() else {
            return range;
        };
        let row = self.text.offset_to_point(offset.min(self.text.len())).row;
        let Some(table) = layout.line(row).and_then(|line| line.table.as_ref()) else {
            return range;
        };
        let line_start = self.text.line_start_offset(row);
        let Some(local) = offset.checked_sub(line_start) else {
            return range;
        };
        let (cell_ix, clamped) = table.cell_of(local);
        let Some(cell) = table.cells.get(cell_ix) else {
            return range;
        };
        let content = line_start + cell.content.start..line_start + cell.content.end;
        let within = |r: Range<usize>| {
            r.start.max(content.start)..r.end.min(content.end).max(r.start.max(content.start))
        };
        let inside = within(range);
        if !inside.is_empty() || content.is_empty() {
            return if inside.is_empty() {
                let at = (line_start + clamped).clamp(content.start, content.end);
                at..at
            } else {
                inside
            };
        }
        // After the last word: the word ending at the content's end.
        let last = self
            .text
            .clip_offset(content.end.saturating_sub(1), Bias::Left);
        match TextSelector::word_range(&self.text, last) {
            Some(word) => {
                let word = within(word);
                if word.is_empty() {
                    content.end..content.end
                } else {
                    word
                }
            }
            None => content.end..content.end,
        }
    }

    /// Select the line at the given offset on triple-click.
    ///
    /// The offset is the UTF-8 offset.
    pub(super) fn select_line(&mut self, offset: usize, _: &mut Window, cx: &mut Context<Self>) {
        let range = TextSelector::line_range(&self.text, offset);
        self.undo_manager.break_transaction_coalescing();
        self.selected_range = (range.start..range.end).into();
        self.selected_word_range = None;
        cx.notify()
    }
}

struct TextSelector;
impl TextSelector {
    /// Select a line in the given text at the specified offset.
    ///
    /// The offset is the UTF-8 offset.
    ///
    /// Returns the start and end offsets of the selected line.
    pub(crate) fn line_range(text: &Rope, offset: usize) -> Range<usize> {
        let offset = text.clip_offset(offset, Bias::Left);
        let row = text.offset_to_point(offset).row;
        let start = text.line_start_offset(row);
        let end = text.line_end_offset(row);

        start..end
    }

    /// Select a word in the given text at the specified offset.
    ///
    /// The offset is the UTF-8 offset.
    ///
    /// Returns the start and end offsets of the selected word.
    pub(crate) fn word_range(text: &Rope, offset: usize) -> Option<Range<usize>> {
        let offset = text.clip_offset(offset, Bias::Left);
        let Some(char) = text.char_at(offset) else {
            return None;
        };

        let end = offset + char.len_utf8();
        let prev_chars = text.chars_at(offset).reversed().take(128);
        let next_chars = text.chars_at(end).take(128);
        Some(word_range_from_chars(offset, char, prev_chars, next_chars))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    #[test]
    fn test_word_range() {
        use indoc::indoc;

        let rope = Rope::from(indoc! {
            r#"
            test text:
            abcde 中文🎉 test
            hello[()]
            test_connector ____
            Rope
            rök
            grande île
            "#
        });

        let tests = vec![
            (0, 0, Some("test")),
            (0, 4, Some(" ")),
            (1, 0, Some("abcde")),
            (1, 4, Some("abcde")),
            (1, 5, Some(" ")),
            (1, 6, Some("中")),
            (1, 9, Some("文")),
            (1, 13, Some("🎉")),
            (1, 20, Some("test")),
            (2, 5, Some("[")),
            (2, 6, Some("(")),
            (2, 7, Some(")")),
            (2, 8, Some("]")),
            (3, 5, Some("test_connector")),
            (3, 14, Some(" ")),
            (3, 16, Some("____")),
            (4, 0, Some("Rope")),
            (5, 0, Some("rök")),
            (6, 8, Some("île")),
        ];

        for (line, column, expected) in tests {
            let line_start_offset = rope.line_start_offset(line);
            let offset = line_start_offset + column;
            let range = TextSelector::word_range(&rope, offset);

            let actual = range.map(|r| rope.slice(r).to_string());
            let expect = expected.map(|s| s.to_string());
            assert_eq!(actual, expect, "line {}, column {}", line, column);
        }
    }

    #[test]
    fn test_line_range() {
        let rope = Rope::from("first line\nsecond line\nthird");
        let tests = vec![
            (0, 0, "first line"),
            (0, 5, "first line"),
            (1, 3, "second line"),
            (2, 1, "third"),
        ];

        for (line, column, expected) in tests {
            let line_start_offset = rope.line_start_offset(line);
            let offset = line_start_offset + column;
            let range = TextSelector::line_range(&rope, offset);

            let actual = rope.slice(range).to_string();
            assert_eq!(actual, expected, "line {}, column {}", line, column);
        }
    }
}
