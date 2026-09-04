/// DisplayMap: Public facade for Editor/Input display mapping.
///
/// This combines WrapMap and FoldMap to provide a unified API:
/// - BufferPoint ↔ DisplayPoint conversion
/// - Fold management (candidates, toggle, query)
/// - Automatic projection updates on text/layout changes
use std::ops::Range;

use gpui::{App, Font, Pixels};
use ropey::Rope;

use super::fold_map::FoldMap;
use super::folding::FoldRange;
pub use super::text_wrapper::{LineHeightScale, TableRowSource, WrappingIndent};
use super::text_wrapper::{LineItem, WrapDisplayPoint};
use super::wrap_map::WrapMap;
use super::{BufferPoint, DisplayPoint};
use crate::input::Point as TreeSitterPoint;
use crate::input::display_map::WrapPoint;
use crate::input::rope_ext::RopeExt as _;

/// DisplayMap is the main interface for Editor/Input coordinate mapping.
///
/// It manages the two-layer projection:
/// 1. Buffer → Wrap (soft-wrapping)
/// 2. Wrap → Display (folding)
///
/// Editor/Input only needs to work with BufferPoint and DisplayPoint.
pub struct DisplayMap {
    wrap_map: WrapMap,
    fold_map: FoldMap,
}

impl DisplayMap {
    pub fn new(font: Font, font_size: Pixels, wrap_width: Option<Pixels>) -> Self {
        Self {
            wrap_map: WrapMap::new(font, font_size, wrap_width),
            fold_map: FoldMap::new(),
        }
    }

    // ==================== Core Coordinate Mapping ====================

    /// Convert buffer position to display position
    pub fn buffer_pos_to_display_pos(&self, pos: BufferPoint) -> DisplayPoint {
        // Buffer → Wrap
        let wrap_pos = self.wrap_map.buffer_pos_to_wrap_pos(pos);

        // Wrap → Display
        if let Some(display_row) = self.fold_map.wrap_row_to_display_row(wrap_pos.row) {
            DisplayPoint::new(display_row, wrap_pos.col)
        } else {
            // Cursor is in a folded region, find nearest visible row
            let display_row = self.fold_map.nearest_visible_display_row(wrap_pos.row);
            DisplayPoint::new(display_row, 0) // Column 0 at fold boundary
        }
    }

    /// Convert display position to buffer position
    pub fn display_pos_to_buffer_pos(&self, pos: DisplayPoint) -> BufferPoint {
        // Display → Wrap
        let wrap_row = self.fold_map.display_row_to_wrap_row(pos.row).unwrap_or(0);

        // Wrap → Buffer
        let wrap_pos = WrapPoint::new(wrap_row, pos.col);
        self.wrap_map.wrap_pos_to_buffer_pos(wrap_pos)
    }

    /// Get total number of visible display rows
    #[inline]
    pub fn display_row_count(&self) -> usize {
        self.fold_map.display_row_count()
    }

    /// Get the buffer line for a given display row
    pub fn display_row_to_buffer_line(&self, display_row: usize) -> usize {
        // Display → Wrap
        let wrap_row = self
            .fold_map
            .display_row_to_wrap_row(display_row)
            .unwrap_or(0);

        // Wrap → Buffer line
        self.wrap_map.wrap_row_to_buffer_line(wrap_row)
    }

    /// Get the display row range for a buffer line: [start, end)
    /// Returns None if the buffer line is completely hidden
    pub fn buffer_line_to_display_row_range(&self, line: usize) -> Option<Range<usize>> {
        // Buffer line → Wrap row range
        let wrap_row_range = self.wrap_map.buffer_line_to_wrap_row_range(line);

        // Find first and last visible display rows in this range
        let mut first_display_row = None;
        let mut last_display_row = None;

        for wrap_row in wrap_row_range {
            if let Some(display_row) = self.fold_map.wrap_row_to_display_row(wrap_row) {
                if first_display_row.is_none() {
                    first_display_row = Some(display_row);
                }
                last_display_row = Some(display_row);
            }
        }

        if let (Some(start), Some(end)) = (first_display_row, last_display_row) {
            Some(start..end + 1)
        } else {
            None // Completely folded
        }
    }

    /// Installs the per-line height multiplier; `None` restores uniform rows.
    pub fn set_height_scale(&mut self, scale: Option<LineHeightScale>, cx: &mut App) {
        self.wrap_map.set_height_scale(scale, cx);
    }

    /// Installs the per-line height multiplier and the table-row source
    /// together, rebuilding every line once.
    pub fn set_line_hooks(
        &mut self,
        scale: Option<LineHeightScale>,
        table_rows: Option<TableRowSource>,
        cx: &mut App,
    ) {
        self.wrap_map.set_line_hooks(scale, table_rows, cx);
    }

    /// Re-lays out the lines covering `range` from the current text, for a
    /// change that is not an edit: a table row that starts or stops being
    /// laid out as one, for instance.
    pub fn rewrap(&mut self, range: Range<usize>, cx: &mut App) {
        self.wrap_map.rewrap(range, cx);
    }

    /// Whether every display row is one base line height tall.
    #[inline]
    pub fn is_uniform_height(&self) -> bool {
        self.wrap_map.wrapper().is_uniform_height()
    }

    /// Height of the visible document, in base line heights.
    ///
    /// Folded lines are subtracted: their rows are in the tree but not on the
    /// screen, so a scrollbar built from the raw total would be too long.
    pub fn total_height(&self) -> f32 {
        let wrapper = self.wrap_map.wrapper();
        if self.is_uniform_height() {
            return self.display_row_count() as f32;
        }
        let mut total = wrapper.total_height();
        for line in self.folded_line_range() {
            // Not `buffer_line_height`: that already reports 0 for a hidden
            // line, so subtracting it would subtract nothing. The height being
            // removed is the one the line has in the tree.
            total -= self.raw_line_height(line);
        }
        total
    }

    /// How tall each row of a buffer line is drawn, relative to the base line
    /// height. This is the number the painter must use, so that a line is drawn
    /// at exactly the height the summed tops assumed for it.
    pub fn line_height_scale(&self, line: usize) -> f32 {
        self.wrap_map.wrapper().line_height_scale(line)
    }

    /// Height a buffer line occupies in the tree, whether or not it is drawn.
    fn raw_line_height(&self, line: usize) -> f32 {
        let wrapper = self.wrap_map.wrapper();
        wrapper.line_height_scale(line)
            * wrapper.line(line).map(|l| l.lines_len()).unwrap_or(0) as f32
    }

    /// Height of one buffer line's visible rows, in base line heights. A folded
    /// line has no height, since none of its rows are drawn.
    pub fn buffer_line_height(&self, line: usize) -> f32 {
        if self.is_buffer_line_hidden(line) {
            return 0.;
        }
        let wrapper = self.wrap_map.wrapper();
        let scale = wrapper.line_height_scale(line);
        scale * self.visible_wrap_row_count_for_buffer_line(line) as f32
    }

    /// y of the top of a buffer line, in base line heights, with folded lines
    /// above it contributing nothing.
    pub fn buffer_line_top(&self, line: usize) -> f32 {
        if self.is_uniform_height() {
            return self.buffer_line_to_display_row(line) as f32;
        }
        let wrapper = self.wrap_map.wrapper();
        let mut top = wrapper.line_top(line);
        for folded in self.folded_line_range() {
            if folded < line {
                top -= self.raw_line_height(folded);
            }
        }
        top
    }

    /// The buffer line drawn at `height` (in base line heights).
    pub fn buffer_line_at_height(&self, height: f32) -> usize {
        if self.is_uniform_height() {
            let display_row = height.max(0.) as usize;
            return self.display_row_to_buffer_line(display_row);
        }
        // Folds shift everything below them up, so walking is only correct
        // against tops that already account for them.
        let count = self.buffer_line_count();
        let mut line = self.wrap_map.wrapper().line_at_height(height.max(0.)).0;
        line = line.min(count.saturating_sub(1));
        while line + 1 < count && self.buffer_line_top(line + 1) <= height {
            line += 1;
        }
        while line > 0 && self.buffer_line_top(line) > height {
            line -= 1;
        }
        line
    }

    /// Buffer lines currently hidden inside a fold.
    ///
    /// A fold keeps both its first and last line on screen and hides what is
    /// between them, so this is `start + 1 ..= end - 1` — not the whole range.
    /// Counting the last line as hidden would subtract a height that is still
    /// being drawn.
    fn folded_line_range(&self) -> impl Iterator<Item = usize> + '_ {
        self.folded_ranges()
            .iter()
            .flat_map(|range| (range.start_line + 1)..range.end_line)
    }

    /// Check if a buffer line is completely hidden
    #[inline]
    pub fn is_buffer_line_hidden(&self, line: usize) -> bool {
        self.buffer_line_to_display_row_range(line).is_none()
    }

    /// First display row of a buffer line. If the line is fully folded, returns the
    /// nearest visible display row.
    pub fn buffer_line_to_display_row(&self, line: usize) -> usize {
        match self.buffer_line_to_display_row_range(line) {
            Some(range) => range.start,
            None => {
                let wrap_row = self.wrap_map.buffer_line_to_first_wrap_row(line);
                self.fold_map.nearest_visible_display_row(wrap_row)
            }
        }
    }

    /// Set fold candidates (from tree-sitter/LSP)
    pub fn set_fold_candidates(&mut self, candidates: Vec<FoldRange>) {
        self.fold_map.set_candidates(candidates);
        self.rebuild_fold_projection();
    }

    /// Set a fold at the given start_line (must be in candidates)
    pub fn set_folded(&mut self, start_line: usize, folded: bool) {
        self.fold_map.set_folded(start_line, folded);
        self.rebuild_fold_projection();
    }

    /// Toggle fold at the given start_line
    pub fn toggle_fold(&mut self, start_line: usize) {
        self.fold_map.toggle_fold(start_line);
        self.rebuild_fold_projection();
    }

    /// Check if a line is currently folded
    #[inline]
    pub fn is_folded_at(&self, start_line: usize) -> bool {
        self.fold_map.is_folded_at(start_line)
    }

    /// Check if a line is a fold candidate
    #[inline]
    pub fn is_fold_candidate(&self, start_line: usize) -> bool {
        self.fold_map.is_fold_candidate(start_line)
    }

    /// Get all currently folded ranges
    #[inline]
    pub fn folded_ranges(&self) -> &[FoldRange] {
        self.fold_map.folded_ranges()
    }

    /// Clear all folds
    pub fn clear_folds(&mut self) {
        self.fold_map.clear_folds();
        self.rebuild_fold_projection();
    }

    // ==================== Text and Layout Updates ====================

    /// Adjust folds and candidates for a text edit before updating the wrap map.
    ///
    /// Must be called with the OLD text (before replacement) and the edit range/new_text
    /// so we can compute which old lines were affected.
    pub fn adjust_folds_for_edit(&mut self, old_text: &Rope, range: &Range<usize>, new_text: &str) {
        if self.fold_map.folded_ranges().is_empty() && self.fold_map.fold_candidates().is_empty() {
            return;
        }

        let edit_start_line = old_text.offset_to_point(range.start).row;
        let edit_end_line = old_text.offset_to_point(range.end.min(old_text.len())).row;

        let old_lines_in_range = edit_end_line.saturating_sub(edit_start_line);
        let new_lines_in_range = new_text.chars().filter(|c| *c == '\n').count();
        let line_delta = new_lines_in_range as isize - old_lines_in_range as isize;

        self.fold_map
            .adjust_folds_for_edit(edit_start_line, edit_end_line, line_delta);
    }

    /// Incrementally update fold candidates after a text edit.
    ///
    /// Extracts new fold candidates only within the edited byte range
    /// and merges them with existing (already adjusted) candidates.
    pub fn update_fold_candidates_for_edit(
        &mut self,
        extract_fold_ranges: impl FnOnce(Range<usize>, &Rope) -> Vec<FoldRange>,
        edit_byte_range: Range<usize>,
        new_text: &Rope,
    ) {
        let new_start_line = new_text.offset_to_point(edit_byte_range.start).row;
        let new_end_line = new_text
            .offset_to_point(edit_byte_range.end.min(new_text.len()))
            .row;

        let new_candidates = extract_fold_ranges(edit_byte_range, new_text);
        self.fold_map
            .merge_candidates_for_edit(new_start_line, new_end_line, new_candidates);
    }

    /// Update text (incremental or full)
    pub fn on_text_changed(
        &mut self,
        changed_text: &Rope,
        range: &Range<usize>,
        new_text: &Rope,
        cx: &mut App,
    ) {
        self.wrap_map
            .on_text_changed(changed_text, range, new_text, cx);
        self.rebuild_fold_projection();
    }

    /// Update layout parameters (wrap width or font)
    pub fn on_layout_changed(&mut self, wrap_width: Option<Pixels>, cx: &mut App) {
        self.wrap_map.on_layout_changed(wrap_width, cx);
        self.rebuild_fold_projection();
    }

    /// Set the wrapping indent for continuation lines.
    pub fn set_wrapping_indent(&mut self, wrapping_indent: WrappingIndent, cx: &mut App) {
        self.wrap_map.set_wrapping_indent(wrapping_indent, cx);
        self.rebuild_fold_projection();
    }

    /// Set font parameters
    pub fn set_font(&mut self, font: Font, font_size: Pixels, cx: &mut App) {
        self.wrap_map.set_font(font, font_size, cx);
        self.rebuild_fold_projection();
    }

    /// Ensure text is prepared (initializes wrapper if needed)
    pub fn ensure_text_prepared(&mut self, text: &Rope, cx: &mut App) {
        let did_initialize = self.wrap_map.ensure_text_prepared(text, cx);
        if did_initialize {
            self.rebuild_fold_projection();
        }
    }

    /// Initialize with text
    pub fn set_text(&mut self, text: &Rope, cx: &mut App) {
        self.wrap_map.set_text(text, cx);
        self.rebuild_fold_projection();
    }

    // ==================== Internal Helpers ====================

    /// Rebuild fold projection after wrap_map or fold state changes
    /// Only rebuilds if there are actually folded ranges
    fn rebuild_fold_projection(&mut self) {
        if !self.fold_map.folded_ranges().is_empty() {
            self.fold_map.rebuild(&self.wrap_map);
        } else {
            // No active folds: identity mapping (wrap_row == display_row).
            // Just update cached count so query methods work without Vec allocation.
            self.fold_map
                .mark_dirty_with_wrap_count(self.wrap_map.wrap_row_count());
        }
    }

    // ==================== Wrap Display Point Operations ====================

    /// Convert byte offset to wrap display point (with soft wrap info).
    #[inline]
    pub(crate) fn offset_to_wrap_display_point(&self, offset: usize) -> WrapDisplayPoint {
        self.wrap_map.wrapper().offset_to_display_point(offset)
    }

    /// Convert wrap display point to byte offset.
    #[inline]
    pub(crate) fn wrap_display_point_to_offset(&self, point: WrapDisplayPoint) -> usize {
        self.wrap_map.wrapper().display_point_to_offset(point)
    }

    /// Convert wrap display point to TreeSitterPoint (buffer line/col).
    #[inline]
    pub(crate) fn wrap_display_point_to_point(&self, point: WrapDisplayPoint) -> TreeSitterPoint {
        self.wrap_map.wrapper().display_point_to_point(point)
    }

    /// Convert a wrap row to a display row (skipping folded rows).
    /// Returns None if the wrap row is folded.
    #[inline]
    pub fn wrap_row_to_display_row(&self, wrap_row: usize) -> Option<usize> {
        self.fold_map.wrap_row_to_display_row(wrap_row)
    }

    /// Find the nearest visible display row for a given wrap row.
    #[inline]
    pub fn nearest_visible_display_row(&self, wrap_row: usize) -> usize {
        self.fold_map.nearest_visible_display_row(wrap_row)
    }

    /// Convert a display row to a wrap row.
    #[inline]
    pub fn display_row_to_wrap_row(&self, display_row: usize) -> Option<usize> {
        self.fold_map.display_row_to_wrap_row(display_row)
    }

    /// Get the longest row index (by byte length).
    #[inline]
    pub(crate) fn longest_row(&self) -> usize {
        self.wrap_map.wrapper().longest_row()
    }

    // ==================== Access Methods ====================

    /// Get the line item by buffer row index.
    #[inline]
    pub(crate) fn line(&self, row: usize) -> Option<&LineItem> {
        self.wrap_map.line(row)
    }

    /// Get the rope text
    #[inline]
    pub fn text(&self) -> &Rope {
        self.wrap_map.text()
    }

    /// Calculate how many wrap rows of a buffer line are visible (not folded)
    #[inline]
    pub fn visible_wrap_row_count_for_buffer_line(&self, line: usize) -> usize {
        self.wrap_map
            .visible_wrap_row_count_for_line(line, &self.fold_map)
    }

    /// Get the wrap row count (before folding)
    #[inline]
    pub fn wrap_row_count(&self) -> usize {
        self.wrap_map.wrap_row_count()
    }

    /// Get the buffer line count (logical lines)
    #[inline]
    pub fn buffer_line_count(&self) -> usize {
        self.wrap_map.buffer_line_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{FontFeatures, FontStyle, FontWeight, TestAppContext, px};
    use std::rc::Rc;

    /// A map over `text` whose lines starting with `#` are twice as tall.
    fn map_with_headings(text: &Rope, cx: &mut App) -> DisplayMap {
        let font = Font {
            family: "Arial".into(),
            weight: FontWeight::default(),
            style: FontStyle::Normal,
            features: FontFeatures::default(),
            fallbacks: None,
        };
        let mut map = DisplayMap::new(font, px(14.), None);
        map.set_text(text, cx);
        let source = text.clone();
        map.set_height_scale(
            Some(Rc::new(move |range: &Range<usize>, _: &Rope, _: u64| {
                if range.end > source.len() {
                    return 1.0;
                }
                match source.slice(range.clone()).chars().next() {
                    Some('#') => 2.0,
                    _ => 1.0,
                }
            })),
            cx,
        );
        map
    }

    #[gpui::test]
    fn heights_stack_up_across_the_document(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let text = Rope::from("plain\n# heading\nplain\n");
            let map = map_with_headings(&text, cx);

            assert!(!map.is_uniform_height());
            assert_eq!(map.buffer_line_top(0), 0.);
            assert_eq!(map.buffer_line_top(1), 1.);
            assert_eq!(map.buffer_line_top(2), 3., "below the double-height line");
            assert_eq!(map.buffer_line_height(1), 2.);
            assert_eq!(map.total_height(), 5.);
        });
    }

    #[gpui::test]
    fn a_point_finds_the_line_drawn_at_it(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let text = Rope::from("plain\n# heading\nplain\n");
            let map = map_with_headings(&text, cx);
            // Tops are 0, 1, 3; the heading occupies 1..3.
            for (height, expected) in [(0., 0), (0.9, 0), (1., 1), (2.9, 1), (3., 2)] {
                assert_eq!(map.buffer_line_at_height(height), expected, "at {height}");
            }
        });
    }

    /// A fold hides rows that still exist in the tree. If their heights were
    /// left in, everything below a folded heading would be drawn too low and
    /// the scrollbar would run past the end of the document.
    #[gpui::test]
    fn a_folded_line_takes_its_height_with_it(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let text = Rope::from("# section\n# inner\nbody\ntail\n");
            let mut map = map_with_headings(&text, cx);
            // Four lines plus the empty one after the trailing newline:
            // 2 + 2 + 1 + 1 + 1.
            let unfolded_total = map.total_height();
            assert_eq!(unfolded_total, 7.);

            map.set_fold_candidates(vec![FoldRange {
                start_line: 0,
                end_line: 2,
            }]);
            map.set_folded(0, true);
            assert_eq!(map.folded_ranges().len(), 1, "the fold was applied");

            // A fold keeps its first and last line visible, so only line 1
            // (the inner heading, 2 high) is hidden.
            assert_eq!(map.buffer_line_height(1), 0., "a folded line draws nothing");
            assert_eq!(map.buffer_line_height(2), 1., "the fold's last line stays");
            assert_eq!(map.total_height(), unfolded_total - 2.);
            // Line 2 moves up by exactly what was hidden above it.
            assert_eq!(map.buffer_line_top(2), 2.);
            assert_eq!(map.buffer_line_at_height(2.), 2);
        });
    }

    #[gpui::test]
    fn without_a_scale_every_row_stays_one_high(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let font = Font {
                family: "Arial".into(),
                weight: FontWeight::default(),
                style: FontStyle::Normal,
                features: FontFeatures::default(),
                fallbacks: None,
            };
            let mut map = DisplayMap::new(font, px(14.), None);
            map.set_text(&Rope::from("# one\ntwo\nthree\n"), cx);

            assert!(map.is_uniform_height());
            assert_eq!(map.buffer_line_top(2), 2.);
            assert_eq!(map.buffer_line_height(0), 1., "a heading is not special");
            assert_eq!(map.buffer_line_at_height(2.5), 2);
        });
    }
}
