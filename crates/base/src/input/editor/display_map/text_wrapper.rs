use gpui::Half;
use std::borrow::Cow;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, Font, LineFragment, Pixels, Point, ShapedLine, Size, TextAlign, Window, point, px, size,
};
use ropey::Rope;
use smallvec::SmallVec;
use sum_tree::{Bias, Dimensions, SumTree};

use crate::input::{
    Point as TreeSitterPoint, RopeExt,
    layout::{LastLayout, WhitespaceIndicators},
};

/// Controls how soft-wrapped continuation lines are indented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WrappingIndent {
    /// Continuation lines start flush-left at the full editor width.
    None,
    /// Continuation lines keep the same indentation as the first line.
    #[default]
    Same,
}

/// A line with soft wrapped lines info.
#[derive(Debug, Clone)]
pub(crate) struct LineItem {
    /// The byte length of the line, without the end `\n`.
    len: usize,
    /// Number of leading characters of the line reserved as indentation for continuation wrapped
    /// lines, when [`WrappingIndent::Same`] is used.
    ///
    /// Zero when [`WrappingIndent::None`] is used or the line is not wrapped.
    pub(crate) indent: u32,
    /// The soft wrapped lines relative byte range (0..len) of this line (Include first line).
    ///
    /// Not contains the line end `\n`.
    pub(crate) wrapped_lines: SmallVec<[Range<usize>; 1]>,
    /// Height of one wrap row of this line, as a multiple of the editor's base
    /// line height — `1.0` for ordinary text, more for a line the application
    /// draws larger (an ATX heading). Every wrap row of a line is equally
    /// tall, so the line's own height is `scale * lines_len()`.
    pub(crate) height_scale: f32,
}

impl LineItem {
    /// Get the bytes length of this line.
    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// Get number of soft wrapped lines of this line (include the first line).
    #[inline]
    pub(crate) fn lines_len(&self) -> usize {
        self.wrapped_lines.len()
    }
}

/// Summary of a subtree of [`LineItem`]s, maintained incrementally by the [`SumTree`].
#[derive(Debug, Clone)]
pub(crate) struct LineSummary {
    /// Number of buffer lines.
    buffer_rows: usize,
    /// Number of wrap rows (sum of each line's `lines_len()`).
    wrap_rows: usize,
    /// Sum of byte lengths of the buffer lines (without the trailing `\n`).
    bytes: usize,
    /// Byte length of the longest line in this subtree.
    max_line_len: usize,
    /// Total height of this subtree in base line heights: the sum over lines of
    /// `height_scale * wrap rows`. Summing a `f32` down the tree is why this is
    /// a dimension rather than something recomputed per frame.
    height: f32,
    /// Buffer row (relative to this subtree) of the first line achieving `max_line_len`.
    longest_row: usize,
}

impl sum_tree::Summary for LineSummary {
    type Context<'a> = &'a ();

    fn zero(_: &()) -> Self {
        LineSummary {
            buffer_rows: 0,
            wrap_rows: 0,
            bytes: 0,
            max_line_len: 0,
            longest_row: 0,
            height: 0.,
        }
    }

    fn add_summary(&mut self, other: &Self, _: &()) {
        // Keep the leftmost row that achieves a strictly greater length
        if other.max_line_len > self.max_line_len {
            self.longest_row = self.buffer_rows + other.longest_row;
            self.max_line_len = other.max_line_len;
        }
        self.buffer_rows += other.buffer_rows;
        self.wrap_rows += other.wrap_rows;
        self.bytes += other.bytes;
        self.height += other.height;
    }
}

impl sum_tree::Item for LineItem {
    type Summary = LineSummary;

    fn summary(&self, _: &()) -> LineSummary {
        LineSummary {
            buffer_rows: 1,
            wrap_rows: self.lines_len(),
            bytes: self.len(),
            max_line_len: self.len(),
            longest_row: 0,
            height: self.height_scale * self.lines_len() as f32,
        }
    }
}

/// Cursor dimension counting buffer rows.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct BufferRows(pub usize);

impl<'a> sum_tree::Dimension<'a, LineSummary> for BufferRows {
    fn zero(_: &()) -> Self {
        BufferRows(0)
    }

    fn add_summary(&mut self, summary: &'a LineSummary, _: &()) {
        self.0 += summary.buffer_rows;
    }
}

/// Cursor dimension counting wrap rows.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct WrapRows(pub usize);

impl<'a> sum_tree::Dimension<'a, LineSummary> for WrapRows {
    fn zero(_: &()) -> Self {
        WrapRows(0)
    }

    fn add_summary(&mut self, summary: &'a LineSummary, _: &()) {
        self.0 += summary.wrap_rows;
    }
}

/// Height multiplier for the line covering a byte range, supplied by the
/// application (see `InputHighlighter::line_font_scale`).
pub type LineHeightScale = Rc<dyn Fn(&Range<usize>) -> f32>;

/// Keeps a scale usable as a height: finite, positive, and not so large that a
/// single line could dominate the document. A hostile or buggy value would
/// otherwise poison every sum in the tree.
fn normalize_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0. {
        scale.min(MAX_HEIGHT_SCALE)
    } else {
        1.0
    }
}

/// Ceiling for one line's height, in base line heights.
const MAX_HEIGHT_SCALE: f32 = 8.0;

/// Cursor dimension accumulating height, in base line heights.
///
/// `Ord` is what a `SumTree` seek needs, and heights are finite and
/// non-negative by construction, so the total order is well defined.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub(crate) struct Height(pub f32);

impl Eq for Height {}

#[allow(clippy::derive_ord_xor_partial_ord)]
impl Ord for Height {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.partial_cmp(other).unwrap_or(std::cmp::Ordering::Equal)
    }
}

impl PartialOrd for Height {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.0.partial_cmp(&other.0)
    }
}

impl<'a> sum_tree::Dimension<'a, LineSummary> for Height {
    fn zero(_: &()) -> Self {
        Height(0.)
    }

    fn add_summary(&mut self, summary: &'a LineSummary, _: &()) {
        self.0 += summary.height;
    }
}

/// Used to prepare the text with soft wrap to be get lines to displayed in the Editor.
///
/// After use lines to calculate the scroll size of the Editor.
pub(crate) struct TextWrapper {
    text: Rope,
    font: Font,
    font_size: Pixels,
    /// If is none, it means the text is not wrapped
    wrap_width: Option<Pixels>,
    wrapping_indent: WrappingIndent,
    /// Per-line height multiplier, supplied by the application. `None` keeps
    /// every line at the base height, which is the uniform behaviour.
    height_scale: Option<LineHeightScale>,
    /// The lines by split \n
    pub(crate) lines: SumTree<LineItem>,

    _initialized: bool,
}

#[allow(unused)]
impl TextWrapper {
    pub(crate) fn new(font: Font, font_size: Pixels, wrap_width: Option<Pixels>) -> Self {
        Self {
            text: Rope::new(),
            font,
            font_size,
            wrap_width,
            wrapping_indent: WrappingIndent::default(),
            height_scale: None,
            lines: SumTree::new(&()),
            _initialized: false,
        }
    }

    #[inline]
    pub(crate) fn set_default_text(&mut self, text: &Rope) {
        self.text = text.clone();
    }

    /// Get reference to the rope text.
    #[inline]
    pub(crate) fn text(&self) -> &Rope {
        &self.text
    }

    /// Get the total number of lines including wrapped lines.
    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.lines.summary().wrap_rows
    }

    /// Get the total number of buffer lines.
    #[inline]
    pub(crate) fn lines_count(&self) -> usize {
        self.lines.summary().buffer_rows
    }

    /// Get the 0-based row index of the longest line (by byte length).
    #[inline]
    pub(crate) fn longest_row(&self) -> usize {
        self.lines.summary().longest_row
    }

    /// Get the line item by buffer row index.
    #[inline]
    pub(crate) fn line(&self, row: usize) -> Option<&LineItem> {
        let mut cursor = self.lines.cursor::<BufferRows>(&());
        cursor.seek(&BufferRows(row), Bias::Right);
        cursor.item()
    }

    /// Iterate buffer lines in order.
    #[inline]
    pub(crate) fn iter_lines(&self) -> impl Iterator<Item = &LineItem> {
        self.lines.iter()
    }

    /// First wrap row of buffer line `row`. Returns the total wrap row count if `row` is
    /// out of range.
    pub(crate) fn buffer_line_to_first_wrap_row(&self, row: usize) -> usize {
        let mut cursor = self.lines.cursor::<Dimensions<BufferRows, WrapRows>>(&());
        cursor.seek(&BufferRows(row), Bias::Right);
        cursor.start().1.0
    }

    /// Wrap row range of buffer line `row`.
    pub(crate) fn buffer_line_to_wrap_row_range(&self, row: usize) -> Range<usize> {
        let mut cursor = self.lines.cursor::<Dimensions<BufferRows, WrapRows>>(&());
        cursor.seek(&BufferRows(row), Bias::Right);
        let start = cursor.start().1.0;
        let len = cursor.item().map(|l| l.lines_len()).unwrap_or(0);
        start..start + len
    }

    /// Buffer line containing wrap row `wrap_row`, clamped to the last line.
    pub(crate) fn wrap_row_to_buffer_line(&self, wrap_row: usize) -> usize {
        let mut cursor = self.lines.cursor::<Dimensions<WrapRows, BufferRows>>(&());
        cursor.seek(&WrapRows(wrap_row), Bias::Right);
        match cursor.item() {
            Some(_) => cursor.start().1.0,
            None => self.lines_count().saturating_sub(1),
        }
    }

    pub(crate) fn set_wrap_width(&mut self, wrap_width: Option<Pixels>, cx: &mut App) {
        if wrap_width == self.wrap_width {
            return;
        }

        self.wrap_width = wrap_width;
        self.update_all(&self.text.clone(), cx);
    }

    pub(crate) fn set_wrapping_indent(&mut self, wrapping_indent: WrappingIndent, cx: &mut App) {
        if wrapping_indent == self.wrapping_indent {
            return;
        }

        self.wrapping_indent = wrapping_indent;
        self.update_all(&self.text.clone(), cx);
    }

    /// Installs the per-line height multiplier and rebuilds every line, since
    /// any of them may now be a different height. Passing `None` returns the
    /// document to uniform rows.
    pub(crate) fn set_height_scale(&mut self, scale: Option<LineHeightScale>, cx: &mut App) {
        self.height_scale = scale;
        self.update_all(&self.text.clone(), cx);
    }

    /// Whether every line is one base height tall. The uniform document must
    /// keep taking the same arithmetic it always did, so callers branch on
    /// this rather than paying for a tree seek per frame.
    pub(crate) fn is_uniform_height(&self) -> bool {
        self.height_scale.is_none()
    }

    /// Total document height, in base line heights.
    pub(crate) fn total_height(&self) -> f32 {
        if self.is_uniform_height() {
            return self.lines.summary().wrap_rows as f32;
        }
        self.lines.summary().height
    }

    /// Height of one buffer line (all of its wrap rows), in base line heights.
    pub(crate) fn line_height_scale(&self, row: usize) -> f32 {
        if self.is_uniform_height() {
            return 1.0;
        }
        self.line(row).map(|line| line.height_scale).unwrap_or(1.0)
    }

    /// y of the top of buffer line `row`, in base line heights.
    pub(crate) fn line_top(&self, row: usize) -> f32 {
        if self.is_uniform_height() {
            return self.buffer_line_to_first_wrap_row(row) as f32;
        }
        let mut cursor = self.lines.cursor::<Dimensions<BufferRows, Height>>(&());
        cursor.seek(&BufferRows(row), Bias::Right);
        cursor.start().1.0
    }

    /// The buffer line whose rows contain `height` (in base line heights), and
    /// how far into that line the point falls.
    pub(crate) fn line_at_height(&self, height: f32) -> (usize, f32) {
        if self.is_uniform_height() {
            let wrap_row = height.max(0.) as usize;
            let row = self.wrap_row_to_buffer_line(wrap_row);
            return (row, height - self.buffer_line_to_first_wrap_row(row) as f32);
        }
        let mut cursor = self.lines.cursor::<Dimensions<Height, BufferRows>>(&());
        cursor.seek(&Height(height.max(0.)), Bias::Right);
        let row = cursor.start().1.0.min(self.lines_count().saturating_sub(1));
        (row, height - self.line_top(row))
    }

    pub(crate) fn set_font(&mut self, font: Font, font_size: Pixels, cx: &mut App) {
        if self.font.eq(&font) && self.font_size == font_size {
            return;
        }

        self.font = font;
        self.font_size = font_size;
        self.update_all(&self.text.clone(), cx);
    }

    pub(crate) fn prepare_if_need(&mut self, text: &Rope, cx: &mut App) -> bool {
        if self._initialized {
            return false;
        }
        self._initialized = true;
        self.update_all(text, cx);
        true
    }

    /// Update the text wrapper and recalculate the wrapped lines.
    ///
    /// If the `text` is the same as the current text, do nothing.
    ///
    /// - `changed_text`: The text [`Rope`] that has changed.
    /// - `range`: The `selected_range` before change.
    /// - `new_text`: The inserted text.
    /// - `force`: Whether to force the update, if false, the update will be skipped if the text is the same.
    /// - `cx`: The application context.
    pub(crate) fn update(
        &mut self,
        changed_text: &Rope,
        range: &Range<usize>,
        new_text: &Rope,
        cx: &mut App,
    ) {
        let mut line_wrapper = cx
            .text_system()
            .line_wrapper(self.font.clone(), self.font_size);
        self._update(
            changed_text,
            range,
            new_text,
            &mut |line_str, wrap_width| {
                line_wrapper
                    .wrap_line(&[LineFragment::text(line_str)], wrap_width)
                    .collect()
            },
        );
    }

    fn _update<F>(
        &mut self,
        changed_text: &Rope,
        range: &Range<usize>,
        new_text: &Rope,
        wrap_line: &mut F,
    ) where
        F: FnMut(&str, Pixels) -> Vec<gpui::Boundary>,
    {
        // Remove the old changed lines.
        let buffer_line_count = self.lines_count();
        let start_row = self.text.offset_to_point(range.start).row;
        let start_row = start_row.min(buffer_line_count.saturating_sub(1));
        let end_row = self.text.offset_to_point(range.end).row;
        let end_row = end_row.min(buffer_line_count.saturating_sub(1));

        // To add the new lines.
        let new_start_row = changed_text.offset_to_point(range.start).row;
        let new_end_row = changed_text
            .offset_to_point(range.start + new_text.len())
            .row;

        let mut new_lines = Vec::with_capacity(new_end_row.saturating_sub(new_start_row) + 1);
        let wrap_width = self.wrap_width;

        // line not contains `\n`.
        for row in new_start_row..=new_end_row {
            let line = changed_text.slice_line(row);
            let mut wrapped_lines = SmallVec::<[Range<usize>; 1]>::new();
            let mut prev_boundary_ix = 0;
            let mut indent_chars = 0;

            // If wrap_width is Pixels::MAX, skip wrapping to disable word wrap
            if let Some(wrap_width) = wrap_width {
                // Borrowed for lines within a single rope chunk.
                let line_str: Cow<str> = line.into();
                match self.wrapping_indent {
                    WrappingIndent::Same => {
                        // Here only have wrapped line, if there is no wrap meet, the `line_wraps`
                        // result will empty.
                        for boundary in wrap_line(&line_str, wrap_width) {
                            wrapped_lines.push(prev_boundary_ix..boundary.ix);
                            prev_boundary_ix = boundary.ix;
                            indent_chars = boundary.next_indent;
                        }
                    }
                    WrappingIndent::None => {
                        // The first visual line keeps the line's leading indentation, so it is
                        // wrapped as is.
                        let boundaries = wrap_line(&line_str, wrap_width);
                        if let Some(first_ix) = boundaries.first().map(|b| b.ix) {
                            wrapped_lines.push(prev_boundary_ix..first_ix);
                            prev_boundary_ix = first_ix;

                            for boundary in wrap_line(&line_str[first_ix..], wrap_width) {
                                let ix = first_ix + boundary.ix;
                                wrapped_lines.push(prev_boundary_ix..ix);
                                prev_boundary_ix = ix;
                            }
                        }
                    }
                }
            }

            // Reset of the line
            if prev_boundary_ix < line.len() || prev_boundary_ix == 0 {
                wrapped_lines.push(prev_boundary_ix..line.len());
            }

            // Asked once per line here, where the line is being (re)built
            // anyway, rather than per frame: a scroll must not re-ask, and an
            // edit only rebuilds the rows it touched.
            // The range is into `changed_text` — the text as it will be after
            // this update — so the application resolves it against the same
            // buffer the engine is building, not a stale snapshot.
            let height_scale = match &self.height_scale {
                Some(scale) => {
                    let start = changed_text.line_start_offset(row);
                    normalize_scale(scale(&(start..start + line.len())))
                }
                None => 1.0,
            };

            new_lines.push(LineItem {
                len: line.len(),
                indent: indent_chars,
                wrapped_lines,
                height_scale,
            });
        }

        if self.lines.is_empty() {
            self.lines = SumTree::from_iter(new_lines, &());
        } else {
            let mut cursor = self.lines.cursor::<BufferRows>(&());
            let mut new_tree = cursor.slice(&BufferRows(start_row), Bias::Right);
            // Skip the replaced rows
            cursor.seek_forward(&BufferRows(end_row + 1), Bias::Right);
            new_tree.extend(new_lines, &());
            // Untouched rows after the edit
            new_tree.append(cursor.suffix(), &());
            drop(cursor);
            self.lines = new_tree;
        }

        self.text = changed_text.clone();
    }

    /// Update the text wrapper and recalculate the wrapped lines.
    ///
    /// If the `text` is the same as the current text, do nothing.
    fn update_all(&mut self, text: &Rope, cx: &mut App) {
        self.update(text, &(0..text.len()), &text, cx);
    }

    /// Return display point (with soft wrap) from the given byte offset in the text.
    ///
    /// Panics if the `offset` is out of bounds.
    pub(crate) fn offset_to_display_point(&self, offset: usize) -> WrapDisplayPoint {
        let row = self.text.offset_to_point(offset).row;
        let start = self.text.line_start_offset(row);

        // Seek to buffer row
        let mut cursor = self.lines.cursor::<Dimensions<BufferRows, WrapRows>>(&());
        cursor.seek(&BufferRows(row), Bias::Right);
        let wrapped_row = cursor.start().1.0;
        let Some(line) = cursor.item() else {
            return WrapDisplayPoint::new(wrapped_row, 0, 0);
        };

        let local_offset = offset.saturating_sub(start);
        for (ix, range) in line.wrapped_lines.iter().enumerate() {
            if range.contains(&local_offset) {
                return WrapDisplayPoint::new(
                    wrapped_row + ix,
                    ix,
                    local_offset.saturating_sub(range.start),
                );
            }
        }

        // Otherwise return the eof of the line.
        let last_range = line.wrapped_lines.last().unwrap_or(&(0..0));
        let ix = line.lines_len().saturating_sub(1);
        return WrapDisplayPoint::new(wrapped_row + ix, ix, last_range.len());
    }

    /// Return byte offset in the text from the given display point (with soft wrap).
    ///
    /// Panics if the `point.row` is out of bounds.
    pub(crate) fn display_point_to_offset(&self, point: WrapDisplayPoint) -> usize {
        // Seek to wrap row `point.row`
        let mut cursor = self.lines.cursor::<Dimensions<WrapRows, BufferRows>>(&());
        cursor.seek(&WrapRows(point.row), Bias::Right);
        let Some(line) = cursor.item() else {
            return self.text.len();
        };
        let wrapped_row = cursor.start().0.0;
        let row = cursor.start().1.0;

        let line_start = self.text.line_start_offset(row);
        let local_row = point.row.saturating_sub(wrapped_row);
        if let Some(range) = line.wrapped_lines.get(local_row) {
            line_start + (range.start + point.column).min(range.end)
        } else {
            // If not found, return the end of the line.
            line_start + line.len()
        }
    }

    pub(crate) fn display_point_to_point(&self, point: WrapDisplayPoint) -> TreeSitterPoint {
        let offset = self.display_point_to_offset(point);
        self.text.offset_to_point(offset)
    }

    pub(crate) fn point_to_display_point(&self, point: TreeSitterPoint) -> WrapDisplayPoint {
        let offset = self.text.point_to_offset(point);
        self.offset_to_display_point(offset)
    }
}

/// A display point within the soft-wrapped text.
///
/// This represents a position in the text after soft-wrapping,
/// with an additional `local_row` field tracking the wrap line
/// within the original buffer line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WrapDisplayPoint {
    /// The 0-based soft wrapped row index in the text.
    pub row: usize,
    /// The 0-based row index in local line (include first line).
    ///
    /// This value only valid when return from [`TextWrapper::offset_to_display_point`], otherwise it will be ignored.
    pub local_row: usize,
    /// The 0-based column byte index in the display line (with soft wrap).
    pub column: usize,
}

impl WrapDisplayPoint {
    pub(crate) fn new(row: usize, local_row: usize, column: usize) -> Self {
        Self {
            row,
            local_row,
            column,
        }
    }
}

/// The layout info of a line with soft wrapped lines.
///
/// Offsets passed to and returned from the index/position helpers are *raw*
/// (buffer) byte offsets relative to the start of the buffer line. The shaped
/// `wrapped_lines` hold the *display* text, which is the raw text with the
/// `concealed` byte ranges removed; the helpers translate between the two.
/// Without conceals both spaces are identical and translation is a no-op.
pub(crate) struct LineLayout {
    /// Total bytes length of the shaped (display) text of this line, i.e. the
    /// sum of the `wrapped_lines` lengths. See [`Self::len`] for the raw length.
    display_len: usize,
    /// Raw byte ranges (relative to the line start, sorted, non-overlapping,
    /// non-empty) removed from the display text.
    concealed: Vec<Range<usize>>,
    /// The soft wrapped lines of this line (Include the first line).
    pub(crate) wrapped_lines: SmallVec<[ShapedLine; 1]>,
    /// Extra left offset applied to continuation wrapped lines, used to reserve the first line's
    /// indentation when [`WrappingIndent::Same`] is used.
    pub(crate) wrap_indent: Pixels,
    pub(crate) longest_width: Pixels,
    pub(crate) whitespace_indicators: Option<WhitespaceIndicators>,
    /// Whitespace indicators: (line_index, x_position, is_tab)
    pub(crate) whitespace_chars: Vec<(usize, Pixels, bool)>,
    /// Height of each of this line's rows, as a multiple of the base line
    /// height. `1.0` unless the application draws this line larger.
    pub(crate) height_scale: f32,
    /// Widgets drawn over this line, with ranges relative to the line start.
    pub(crate) widgets: Vec<crate::input::InlineWidget>,
}

impl LineLayout {
    pub(crate) fn new() -> Self {
        Self {
            height_scale: 1.0,
            widgets: Vec::new(),
            display_len: 0,
            concealed: Vec::new(),
            longest_width: px(0.),
            wrapped_lines: SmallVec::new(),
            wrap_indent: px(0.),
            whitespace_chars: Vec::new(),
            whitespace_indicators: None,
        }
    }

    /// Set the left offset reserved for continuation wrapped lines.
    pub(crate) fn wrap_indent(mut self, wrap_indent: Pixels) -> Self {
        self.wrap_indent = wrap_indent;
        self
    }

    /// The pixel indent applied to the given visual line, relative to the line's
    /// leading text. Only continuation lines (index > 0) are indented.
    #[inline]
    fn line_indent(&self, line_index: usize) -> Pixels {
        if line_index == 0 {
            px(0.)
        } else {
            self.wrap_indent
        }
    }

    pub(crate) fn lines(mut self, wrapped_lines: SmallVec<[ShapedLine; 1]>) -> Self {
        self.set_wrapped_lines(wrapped_lines);
        self
    }

    pub(crate) fn set_wrapped_lines(&mut self, wrapped_lines: SmallVec<[ShapedLine; 1]>) {
        self.display_len = wrapped_lines.iter().map(|l| l.len).sum();
        let width = wrapped_lines
            .iter()
            .map(|l| l.width)
            .max()
            .unwrap_or_default();
        self.longest_width = width;
        self.wrapped_lines = wrapped_lines;
    }

    pub(crate) fn with_whitespaces(mut self, indicators: Option<WhitespaceIndicators>) -> Self {
        self.whitespace_indicators = indicators;
        let Some(indicators) = self.whitespace_indicators.as_ref() else {
            return self;
        };

        let space_indicator_offset = indicators.space.width.half();

        for (line_index, wrapped_line) in self.wrapped_lines.iter().enumerate() {
            for (relative_offset, c) in wrapped_line.text.char_indices() {
                if matches!(c, ' ' | '\t') {
                    let is_tab = c == '\t';
                    let start_x = wrapped_line.x_for_index(relative_offset);
                    let end_x = wrapped_line.x_for_index(relative_offset + c.len_utf8());
                    // Center the indicator in the actual character's space
                    let x_position = if c == ' ' {
                        (start_x + end_x).half() - space_indicator_offset
                    } else {
                        start_x
                    };

                    self.whitespace_chars.push((line_index, x_position, is_tab));
                }
            }
        }
        self
    }

    /// Set the concealed raw byte ranges of this line (relative to the line
    /// start). The ranges are normalized: clamped to be non-empty, sorted, and
    /// merged when overlapping or touching.
    ///
    /// The `wrapped_lines` must have been shaped from the raw text with these
    /// ranges removed; see [`Self::raw_to_display`].
    /// Sets how tall each of this line's rows is drawn, relative to the base
    /// line height. Must match the scale the display map summed, or the line
    /// will be painted at a height the scroll position does not expect.
    pub(crate) fn with_height_scale(mut self, scale: f32) -> Self {
        self.height_scale = scale;
        self
    }

    /// Widgets to draw over this line, with ranges relative to its start.
    pub(crate) fn with_widgets(mut self, widgets: Vec<crate::input::InlineWidget>) -> Self {
        self.widgets = widgets;
        self
    }

    pub(crate) fn with_concealed(mut self, concealed: Vec<Range<usize>>) -> Self {
        self.concealed = normalize_concealed(concealed);
        self
    }

    /// Raw bytes length of this line (display length plus concealed bytes).
    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.display_len + self.concealed.iter().map(|r| r.len()).sum::<usize>()
    }

    /// Translate a raw (buffer) byte offset, relative to the line start, to the
    /// byte offset in the display text (with concealed bytes removed).
    ///
    /// Offsets inside a concealed range collapse to the display position of
    /// that range's start. Offsets beyond the end are shifted by the total
    /// concealed length (so `raw_to_display(len()) == display_len`, and
    /// `len() + 1` maps to `display_len + 1`).
    #[inline]
    pub(crate) fn raw_to_display(&self, raw: usize) -> usize {
        raw_to_display(&self.concealed, raw)
    }

    /// Translate a byte offset in the display text back to a raw (buffer) byte
    /// offset relative to the line start.
    ///
    /// A display offset at the boundary where a concealed range was removed
    /// maps to the *start* of that range (before the hidden bytes).
    #[inline]
    pub(crate) fn display_to_raw(&self, display: usize) -> usize {
        display_to_raw(&self.concealed, display)
    }

    /// Get the position (x, y) for the given index in this line layout.
    ///
    /// - The `offset` is a local byte index in this line layout.
    /// - When `line_end_affinity` is true, an offset at a soft wrap boundary is placed at
    ///   the end of the current visual line rather than the start of the next one.
    /// - The return value is relative to the top-left corner of this line layout, start from (0, 0)
    pub(crate) fn position_for_index(
        &self,
        offset: usize,
        last_layout: &LastLayout,
        line_end_affinity: bool,
    ) -> Option<Point<Pixels>> {
        // Work in display space: the shaped lines do not contain concealed bytes.
        let offset = self.raw_to_display(offset);
        let mut acc_len = 0;
        let mut offset_y = px(0.);

        let x_offset = last_layout.alignment_offset(self.longest_width);

        for (i, line) in self.wrapped_lines.iter().enumerate() {
            let is_last = i + 1 == self.wrapped_lines.len();

            let matches = if line.len == 0 {
                // Empty visual lines still own their boundary offset.
                offset == acc_len
            } else if is_last || line_end_affinity {
                // Inclusive: cursor can sit at end of this visual line.
                offset >= acc_len && offset <= acc_len + line.len
            } else {
                // Exclusive: boundary offset belongs to the next visual line.
                offset >= acc_len && offset < acc_len + line.len
            };

            if matches {
                let x = line.x_for_index(offset.saturating_sub(acc_len))
                    + x_offset
                    + self.line_indent(i);
                return Some(point(x, offset_y));
            }

            // Always advance by actual line length. The last line gets +1 so the
            // cursor can be placed after the final character.
            acc_len += if is_last { line.len + 1 } else { line.len };
            offset_y += self.row_height(last_layout.line_height);
        }

        None
    }

    /// Get the closest index for the given x in this line layout.
    ///
    /// The return value is a raw (buffer) byte offset relative to the line start.
    pub(crate) fn closest_index_for_x(&self, x: Pixels, last_layout: &LastLayout) -> usize {
        let mut acc_len = 0;
        let x_offset = last_layout.alignment_offset(self.longest_width);
        let x = x - x_offset;

        for (i, line) in self.wrapped_lines.iter().enumerate() {
            let is_last = i + 1 == self.wrapped_lines.len();
            let line_indent = self.line_indent(i);
            if x <= line_indent + line.width {
                let mut ix = line.closest_index_for_x(x - line_indent);
                if !is_last && ix == line.text.len() {
                    // For soft wrap line, we can't put the cursor at the end of the line.
                    let c_len = line.text.chars().last().map(|c| c.len_utf8()).unwrap_or(0);
                    ix = ix.saturating_sub(c_len);
                }

                return self.display_to_raw(acc_len + ix);
            }
            acc_len += line.text.len();
        }

        self.display_to_raw(acc_len)
    }

    /// Get the index for the given position (x, y) in this line layout.
    ///
    /// The `pos` is relative to the top-left corner of this line layout, start from (0, 0)
    /// The return value is a local raw (buffer) byte index in this line layout, start from 0.
    pub(crate) fn closest_index_for_position(
        &self,
        pos: Point<Pixels>,
        last_layout: &LastLayout,
    ) -> Option<usize> {
        let mut offset = 0;
        let mut line_top = px(0.);
        let x_offset = last_layout.alignment_offset(self.longest_width);
        for (i, line) in self.wrapped_lines.iter().enumerate() {
            let is_last = i + 1 == self.wrapped_lines.len();
            let line_bottom = line_top + self.row_height(last_layout.line_height);
            if pos.y >= line_top && pos.y < line_bottom {
                let mut ix = line.closest_index_for_x(pos.x - x_offset - self.line_indent(i));
                if !is_last && ix == line.text.len() {
                    // For soft wrap line, we can't put the cursor at the end of the line.
                    let c_len = line.text.chars().last().map(|c| c.len_utf8()).unwrap_or(0);
                    ix = ix.saturating_sub(c_len);
                }
                return Some(self.display_to_raw(offset + ix));
            }

            offset += line.text.len();
            line_top = line_bottom;
        }

        None
    }

    /// Get the index for the given position (x, y) in this line layout, or
    /// `None` when the position is not over a glyph.
    ///
    /// The return value is a local raw (buffer) byte index in this line layout.
    pub(crate) fn index_for_position(
        &self,
        pos: Point<Pixels>,
        last_layout: &LastLayout,
    ) -> Option<usize> {
        let mut offset = 0;
        let mut line_top = px(0.);
        let x_offset = last_layout.alignment_offset(self.longest_width);
        for (i, line) in self.wrapped_lines.iter().enumerate() {
            let line_bottom = line_top + self.row_height(last_layout.line_height);
            if pos.y >= line_top && pos.y < line_bottom {
                let ix = line.index_for_x(pos.x - x_offset - self.line_indent(i))?;
                return Some(self.display_to_raw(offset + ix));
            }

            offset += line.text.len();
            line_top = line_bottom;
        }

        None
    }

    pub(crate) fn size(&self, line_height: Pixels) -> Size<Pixels> {
        let width = self
            .wrapped_lines
            .iter()
            .enumerate()
            .map(|(ix, line)| line.width + self.line_indent(ix))
            .max()
            .unwrap_or(self.longest_width);
        size(
            width,
            self.row_height(line_height) * self.wrapped_lines.len(),
        )
    }

    /// Height of one of this line's rows.
    pub(crate) fn row_height(&self, line_height: Pixels) -> Pixels {
        line_height * self.height_scale
    }

    pub(crate) fn paint(
        &self,
        pos: Point<Pixels>,
        line_height: Pixels,
        text_align: TextAlign,
        align_width: Option<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) {
        for (ix, line) in self.wrapped_lines.iter().enumerate() {
            _ = line.paint(
                pos + point(self.line_indent(ix), self.row_height(line_height) * ix),
                self.row_height(line_height),
                text_align,
                align_width,
                window,
                cx,
            );
        }

        // Paint whitespace indicators
        if let Some(indicators) = self.whitespace_indicators.as_ref() {
            for (line_index, x_position, is_tab) in &self.whitespace_chars {
                let invisible = if *is_tab {
                    indicators.tab.clone()
                } else {
                    indicators.space.clone()
                };

                let origin = point(
                    pos.x + *x_position + self.line_indent(*line_index),
                    pos.y + self.row_height(line_height) * *line_index,
                );

                _ = invisible.paint(origin, line_height, text_align, align_width, window, cx);
            }
        }
    }
}

/// Sort, drop empty, and merge overlapping/touching concealed ranges.
pub(crate) fn normalize_concealed(mut concealed: Vec<Range<usize>>) -> Vec<Range<usize>> {
    concealed.retain(|r| r.end > r.start);
    if concealed.len() <= 1 {
        return concealed;
    }
    concealed.sort_by_key(|r| (r.start, r.end));
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(concealed.len());
    for r in concealed {
        if let Some(last) = merged.last_mut()
            && r.start <= last.end
        {
            last.end = last.end.max(r.end);
        } else {
            merged.push(r);
        }
    }
    merged
}

/// Translate a raw byte offset to a display byte offset, given the sorted,
/// non-overlapping `concealed` raw ranges. See [`LineLayout::raw_to_display`].
fn raw_to_display(concealed: &[Range<usize>], raw: usize) -> usize {
    let mut removed = 0;
    for r in concealed {
        if raw < r.start {
            break;
        }
        if raw < r.end {
            // Inside a concealed range: collapse to its start.
            return r.start - removed;
        }
        removed += r.len();
    }
    raw - removed
}

/// Translate a display byte offset back to a raw byte offset, given the sorted,
/// non-overlapping `concealed` raw ranges. See [`LineLayout::display_to_raw`].
fn display_to_raw(concealed: &[Range<usize>], display: usize) -> usize {
    let mut removed = 0;
    for r in concealed {
        // Display position at which this concealed range was removed.
        let display_start = r.start - removed;
        if display <= display_start {
            break;
        }
        removed += r.len();
    }
    display + removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    use gpui::{Boundary, FontFeatures, FontStyle, FontWeight, px};

    #[test]
    fn test_update() {
        let font = gpui::Font {
            family: "Arial".into(),
            weight: FontWeight::default(),
            style: FontStyle::Normal,
            features: FontFeatures::default(),
            fallbacks: None,
        };

        let mut wrapper = TextWrapper::new(font, px(14.), None);
        let mut text = Rope::from(
            "Hello, 世界!\r\nThis is second line.\nThis is third line.\n这里是第 4 行。",
        );

        fn fake_wrap_line(_line: &str, _wrap_width: Pixels) -> Vec<Boundary> {
            vec![]
        }

        #[track_caller]
        fn assert_wrapper_lines(text: &Rope, wrapper: &TextWrapper, expected_lines: &[&[&str]]) {
            let mut actual_lines = vec![];
            let mut offset = 0;
            for line in wrapper.iter_lines() {
                actual_lines.push(
                    line.wrapped_lines
                        .iter()
                        .map(|range| text.slice(offset + range.start..offset + range.end))
                        .collect::<Vec<_>>(),
                );
                // +1 \n
                offset += line.len() + 1;
            }
            assert_eq!(actual_lines, expected_lines);
        }

        wrapper._update(&text, &(0..text.len()), &text, &mut fake_wrap_line);
        assert_eq!(wrapper.lines_count(), 4);
        assert_wrapper_lines(
            &text,
            &wrapper,
            &[
                &["Hello, 世界!\r"],
                &["This is second line."],
                &["This is third line."],
                &["这里是第 4 行。"],
            ],
        );

        // Add a new text to end
        let range = text.len()..text.len();
        let new_text = "New text";
        text.replace(range.clone(), new_text);
        wrapper._update(&text, &range, &Rope::from(new_text), &mut fake_wrap_line);
        assert_eq!(
            text.to_string(),
            "Hello, 世界!\r\nThis is second line.\nThis is third line.\n这里是第 4 行。New text"
        );
        assert_eq!(wrapper.lines_count(), 4);
        assert_eq!(wrapper.lines_count(), 4);
        assert_wrapper_lines(
            &text,
            &wrapper,
            &[
                &["Hello, 世界!\r"],
                &["This is second line."],
                &["This is third line."],
                &["这里是第 4 行。New text"],
            ],
        );

        // Replace first line `Hello` to `AAA`
        let range = 0..5;
        let new_text = "AAA";
        text.replace(range.clone(), new_text);
        wrapper._update(&text, &range, &Rope::from(new_text), &mut fake_wrap_line);
        assert_eq!(
            text.to_string(),
            "AAA, 世界!\r\nThis is second line.\nThis is third line.\n这里是第 4 行。New text"
        );
        assert_eq!(wrapper.lines_count(), 4);
        assert_wrapper_lines(
            &text,
            &wrapper,
            &[
                &["AAA, 世界!\r"],
                &["This is second line."],
                &["This is third line."],
                &["这里是第 4 行。New text"],
            ],
        );

        // Remove the second line
        let start_offset = text.line_start_offset(1);
        let end_offset = text.line_end_offset(1);
        let range = start_offset..end_offset + 1;
        text.replace(range.clone(), "");
        wrapper._update(&text, &range, &Rope::from(""), &mut fake_wrap_line);
        assert_eq!(
            text.to_string(),
            "AAA, 世界!\r\nThis is third line.\n这里是第 4 行。New text"
        );
        assert_eq!(wrapper.lines_count(), 3);
        assert_wrapper_lines(
            &text,
            &wrapper,
            &[
                &["AAA, 世界!\r"],
                &["This is third line."],
                &["这里是第 4 行。New text"],
            ],
        );

        // Replace the first 2 lines to "This is a new line."
        let range = text.line_start_offset(0)..text.line_end_offset(1) + 1;
        let new_text = "This is a new line.\nThis is new line 2.\n";
        text.replace(range.clone(), new_text);
        wrapper._update(&text, &range, &Rope::from(new_text), &mut fake_wrap_line);
        assert_eq!(
            text.to_string(),
            "This is a new line.\nThis is new line 2.\n这里是第 4 行。New text"
        );
        assert_eq!(wrapper.lines_count(), 3);
        assert_wrapper_lines(
            &text,
            &wrapper,
            &[
                &["This is a new line."],
                &["This is new line 2."],
                &["这里是第 4 行。New text"],
            ],
        );

        // Add a new line at the end
        let range = text.len()..text.len();
        let new_text = "\nThis is a new line at the end.";
        text.replace(range.clone(), new_text);
        wrapper._update(&text, &range, &Rope::from(new_text), &mut fake_wrap_line);
        assert_eq!(
            text.to_string(),
            "This is a new line.\nThis is new line 2.\n这里是第 4 行。New text\nThis is a new line at the end."
        );
        assert_eq!(wrapper.lines_count(), 4);
        assert_wrapper_lines(
            &text,
            &wrapper,
            &[
                &["This is a new line."],
                &["This is new line 2."],
                &["这里是第 4 行。New text"],
                &["This is a new line at the end."],
            ],
        );

        // Add a new line at the beginning
        let range = 0..0;
        let new_text = "This is a new line at the beginning.\n";
        text.replace(range.clone(), new_text);
        wrapper._update(&text, &range, &Rope::from(new_text), &mut fake_wrap_line);
        assert_eq!(
            text.to_string(),
            "This is a new line at the beginning.\nThis is a new line.\nThis is new line 2.\n这里是第 4 行。New text\nThis is a new line at the end."
        );
        assert_eq!(wrapper.lines_count(), 5);
        assert_wrapper_lines(
            &text,
            &wrapper,
            &[
                &["This is a new line at the beginning."],
                &["This is a new line."],
                &["This is new line 2."],
                &["这里是第 4 行。New text"],
                &["This is a new line at the end."],
            ],
        );

        // Remove all to at least one line in `lines`.
        let range = 0..text.len();
        let new_text = "";
        text.replace(range.clone(), new_text);
        wrapper._update(&text, &range, &Rope::from(new_text), &mut fake_wrap_line);
        assert_eq!(text.to_string(), "");
        assert_eq!(wrapper.lines_count(), 1);
        assert_eq!(wrapper.line(0).unwrap().wrapped_lines.as_slice(), [0..0]);

        // Test update_all
        let range = 0..text.len();
        let new_text = "This is a full text.\nThis is a second line.";
        text.replace(range.clone(), new_text);
        wrapper._update(&text, &range, &text, &mut fake_wrap_line);
        assert_eq!(
            text.to_string(),
            "This is a full text.\nThis is a second line."
        );
        assert_eq!(wrapper.lines_count(), 2);
    }

    fn test_font() -> gpui::Font {
        gpui::Font {
            family: "Arial".into(),
            weight: FontWeight::default(),
            style: FontStyle::Normal,
            features: FontFeatures::default(),
            fallbacks: None,
        }
    }

    /// The longest-row summary stays exact when the previously-longest line is shrunk.
    #[test]
    fn test_longest_row_after_shrink() {
        let mut wrapper = TextWrapper::new(test_font(), px(14.), None);
        let mut text = Rope::from("aa\nthis is the longest line\nbb");
        wrapper._update(&text, &(0..text.len()), &text, &mut |_, _| vec![]);
        assert_eq!(wrapper.longest_row(), 1);

        // Shrink line 1 so line 2-equivalent isn't longest.
        // Make line 0 the longest now.
        let start = text.line_start_offset(0);
        let end = text.line_end_offset(0);
        let range = start..end;
        let new_text = "a very very long first line now";
        text.replace(range.clone(), new_text);
        wrapper._update(&text, &range, &Rope::from(new_text), &mut |_, _| vec![]);
        assert_eq!(wrapper.longest_row(), 0);
    }

    /// Editing the last line and deleting everything must keep the tree consistent.
    #[test]
    fn test_edit_last_line_and_full_delete() {
        let mut wrapper = TextWrapper::new(test_font(), px(14.), None);
        let mut text = Rope::from("one\ntwo\nthree");
        wrapper._update(&text, &(0..text.len()), &text, &mut |_, _| vec![]);
        assert_eq!(wrapper.lines_count(), 3);

        // Replace the last line only.
        let start = text.line_start_offset(2);
        let range = start..text.len();
        let new_text = "THREE EDITED";
        text.replace(range.clone(), new_text);
        wrapper._update(&text, &range, &Rope::from(new_text), &mut |_, _| vec![]);
        assert_eq!(wrapper.lines_count(), 3);
        assert_eq!(wrapper.line(2).unwrap().len(), "THREE EDITED".len());

        // Delete everything.
        let range = 0..text.len();
        text.replace(range.clone(), "");
        wrapper._update(&text, &range, &Rope::from(""), &mut |_, _| vec![]);
        assert_eq!(wrapper.lines_count(), 1);
        assert_eq!(wrapper.len(), 1);
        assert_eq!(wrapper.line(0).unwrap().wrapped_lines.as_slice(), [0..0]);
    }

    #[test]
    fn test_wrap_row_buffer_line_boundaries() {
        let mut wrapper = TextWrapper::new(test_font(), px(14.), None);
        wrapper.text = Rope::from("aa\nbbbb\nc");
        wrapper.lines = SumTree::from_iter(
            vec![
                LineItem {
                    len: 2,
                    indent: 0,
                    wrapped_lines: smallvec::smallvec![0..2],
                    height_scale: 1.0,
                },
                LineItem {
                    len: 4,
                    indent: 0,
                    wrapped_lines: smallvec::smallvec![0..2, 2..4],
                    height_scale: 1.0,
                },
                LineItem {
                    len: 1,
                    indent: 0,
                    wrapped_lines: smallvec::smallvec![0..1],
                    height_scale: 1.0,
                },
            ],
            &(),
        );

        assert_eq!(wrapper.lines_count(), 3);
        assert_eq!(wrapper.len(), 4);

        assert_eq!(wrapper.buffer_line_to_first_wrap_row(0), 0);
        assert_eq!(wrapper.buffer_line_to_first_wrap_row(1), 1);
        assert_eq!(wrapper.buffer_line_to_first_wrap_row(2), 3);
        assert_eq!(wrapper.buffer_line_to_first_wrap_row(3), 4);

        assert_eq!(wrapper.buffer_line_to_wrap_row_range(0), 0..1);
        assert_eq!(wrapper.buffer_line_to_wrap_row_range(1), 1..3);
        assert_eq!(wrapper.buffer_line_to_wrap_row_range(2), 3..4);
        assert_eq!(wrapper.buffer_line_to_wrap_row_range(3), 4..4);

        assert_eq!(wrapper.wrap_row_to_buffer_line(0), 0);
        assert_eq!(wrapper.wrap_row_to_buffer_line(1), 1);
        assert_eq!(wrapper.wrap_row_to_buffer_line(2), 1);
        assert_eq!(wrapper.wrap_row_to_buffer_line(3), 2);
        assert_eq!(wrapper.wrap_row_to_buffer_line(4), 2);
    }

    #[test]
    fn test_wrap_row_queries_after_incremental_splice() {
        let mut wrapper = TextWrapper::new(test_font(), px(14.), Some(px(10.)));
        let mut text = Rope::from("aa\nbbbb\nc");
        let mut fake_wrap_line = |line: &str, _wrap_width: Pixels| {
            if line.len() > 2 {
                vec![Boundary {
                    ix: 2,
                    next_indent: 0,
                }]
            } else {
                vec![]
            }
        };

        wrapper._update(&text, &(0..text.len()), &text, &mut fake_wrap_line);
        assert_eq!(wrapper.buffer_line_to_wrap_row_range(0), 0..1);
        assert_eq!(wrapper.buffer_line_to_wrap_row_range(1), 1..3);
        assert_eq!(wrapper.buffer_line_to_wrap_row_range(2), 3..4);

        let range = text.line_start_offset(1)..text.line_end_offset(1);
        let new_text = "dd\neeee";
        text.replace(range.clone(), new_text);
        wrapper._update(&text, &range, &Rope::from(new_text), &mut fake_wrap_line);

        assert_eq!(wrapper.lines_count(), 4);
        assert_eq!(wrapper.len(), 5);
        assert_eq!(wrapper.buffer_line_to_wrap_row_range(0), 0..1);
        assert_eq!(wrapper.buffer_line_to_wrap_row_range(1), 1..2);
        assert_eq!(wrapper.buffer_line_to_wrap_row_range(2), 2..4);
        assert_eq!(wrapper.buffer_line_to_wrap_row_range(3), 4..5);
        assert_eq!(wrapper.wrap_row_to_buffer_line(0), 0);
        assert_eq!(wrapper.wrap_row_to_buffer_line(1), 1);
        assert_eq!(wrapper.wrap_row_to_buffer_line(2), 2);
        assert_eq!(wrapper.wrap_row_to_buffer_line(3), 2);
        assert_eq!(wrapper.wrap_row_to_buffer_line(4), 3);
    }

    #[test]
    fn test_line_layout() {
        let mut line_layout = LineLayout::new();

        let line1 = ShapedLine::default().with_len(100);
        let line2 = ShapedLine::default().with_len(50);
        let wrapped_lines = smallvec::smallvec![line1, line2];
        line_layout.set_wrapped_lines(wrapped_lines);
        assert_eq!(line_layout.len(), 150);
        assert_eq!(line_layout.wrapped_lines.len(), 2);
    }

    #[test]
    fn test_position_for_index_prefers_first_leading_empty_visual_line() {
        let mut line_layout = LineLayout::new();
        line_layout.set_wrapped_lines(smallvec::smallvec![
            ShapedLine::default(),
            ShapedLine::default(),
            ShapedLine::default().with_len(3),
        ]);

        let last_layout = LastLayout {
            visible_range: 0..1,
            visible_buffer_lines: vec![0],
            visible_line_byte_offsets: vec![0],
            visible_top: px(0.),
            visible_range_offset: 0..0,
            lines: Rc::new(vec![]),
            line_height: px(20.),
            wrap_width: None,
            wrapping_indent: WrappingIndent::default(),
            line_number_width: px(0.),
            cursor_bounds: None,
            text_align: TextAlign::Left,
            content_width: px(0.),
        };

        assert_eq!(
            line_layout.position_for_index(0, &last_layout, false),
            Some(point(px(0.), px(0.)))
        );
    }

    fn layout_for(line_height: Pixels) -> LastLayout {
        LastLayout {
            visible_range: 0..1,
            visible_buffer_lines: vec![0],
            visible_line_byte_offsets: vec![0],
            visible_top: px(0.),
            visible_range_offset: 0..0,
            lines: Rc::new(vec![]),
            line_height,
            wrap_width: None,
            wrapping_indent: WrappingIndent::default(),
            line_number_width: px(0.),
            cursor_bounds: None,
            text_align: TextAlign::Left,
            content_width: px(0.),
        }
    }

    /// A taller line's wrapped rows are spaced by *its* height. Stepping by the
    /// base height would stack the second row on top of the first.
    #[test]
    fn wrapped_rows_of_a_tall_line_are_spaced_by_its_own_height() {
        let mut line_layout = LineLayout::new().with_height_scale(2.0);
        line_layout.set_wrapped_lines(smallvec::smallvec![
            ShapedLine::default().with_len(3),
            ShapedLine::default().with_len(3),
        ]);
        let last_layout = layout_for(px(20.));

        // First row at the top, second a full 40px below, not 20.
        assert_eq!(
            line_layout.position_for_index(0, &last_layout, false),
            Some(point(px(0.), px(0.)))
        );
        assert_eq!(
            line_layout
                .position_for_index(4, &last_layout, false)
                .map(|p| p.y),
            Some(px(40.))
        );
        // And the line reports the height those rows actually occupy.
        assert_eq!(line_layout.size(px(20.)).height, px(80.));
        assert_eq!(line_layout.row_height(px(20.)), px(40.));
    }

    /// A click below a tall line's first row must not fall outside the line.
    /// With the base height the line would be treated as 40px tall while it is
    /// drawn 80px tall, so clicks in its lower half would hit nothing.
    #[test]
    fn a_click_anywhere_in_a_tall_line_stays_inside_it() {
        let mut line_layout = LineLayout::new().with_height_scale(2.0);
        line_layout.set_wrapped_lines(smallvec::smallvec![
            ShapedLine::default().with_len(3),
            ShapedLine::default().with_len(3),
        ]);
        let last_layout = layout_for(px(20.));

        // The line is drawn 0..80; every y inside it resolves.
        for y in [0., 39., 40., 79.] {
            assert!(
                line_layout
                    .closest_index_for_position(point(px(0.), px(y)), &last_layout)
                    .is_some(),
                "y = {y} is inside the line as drawn"
            );
        }
        // Past the bottom of the line there is nothing to hit.
        assert!(
            line_layout
                .closest_index_for_position(point(px(0.), px(80.)), &last_layout)
                .is_none()
        );
    }

    /// An ordinary line must keep taking exactly the geometry it did before.
    #[test]
    fn an_unscaled_line_is_unchanged() {
        let mut line_layout = LineLayout::new();
        line_layout.set_wrapped_lines(smallvec::smallvec![
            ShapedLine::default().with_len(3),
            ShapedLine::default().with_len(3),
        ]);
        let last_layout = layout_for(px(20.));

        assert_eq!(line_layout.row_height(px(20.)), px(20.));
        assert_eq!(line_layout.size(px(20.)).height, px(40.));
        assert_eq!(
            line_layout
                .position_for_index(4, &last_layout, false)
                .map(|p| p.y),
            Some(px(20.))
        );
    }

    #[test]
    fn test_offset_to_display_point() {
        let font = gpui::Font {
            family: "Arial".into(),
            weight: FontWeight::default(),
            style: FontStyle::Normal,
            features: FontFeatures::default(),
            fallbacks: None,
        };

        let mut wrapper = TextWrapper::new(font, px(14.), None);
        wrapper.text = Rope::from(
            "Hello, 世界!\r\nThis is second line.\nThis is third line.\n这里是第 4 行。",
        );
        wrapper.lines = SumTree::from_iter(
            vec![
                // range: 0..15
                LineItem {
                    len: Rope::from("Hello, 世界!\r").len(),
                    indent: 0,
                    wrapped_lines: smallvec::smallvec![0..15],
                    height_scale: 1.0,
                },
                // range: 16..36
                LineItem {
                    len: Rope::from("This is second line.\n").len(),
                    indent: 0,
                    wrapped_lines: smallvec::smallvec![0..10, 10..20],
                    height_scale: 1.0,
                },
                // range: 37..56
                LineItem {
                    len: Rope::from("This is third line.\n").len(),
                    indent: 0,
                    wrapped_lines: smallvec::smallvec![0..9, 9..15, 15..20],
                    height_scale: 1.0,
                },
                // range: 57..79
                LineItem {
                    len: Rope::from("这里是第 4 行。").len(),
                    indent: 0,
                    wrapped_lines: smallvec::smallvec![0..22],
                    height_scale: 1.0,
                },
            ],
            &(),
        );

        assert_eq!(
            wrapper.offset_to_display_point(12),
            WrapDisplayPoint::new(0, 0, 12)
        );
        assert_eq!(
            wrapper.offset_to_display_point(15),
            WrapDisplayPoint::new(0, 0, 15)
        );

        assert_eq!(
            wrapper.offset_to_display_point(16),
            WrapDisplayPoint::new(1, 0, 0)
        );
        assert_eq!(
            wrapper.offset_to_display_point(21),
            WrapDisplayPoint::new(1, 0, 5)
        );
        assert_eq!(
            wrapper.offset_to_display_point(27),
            WrapDisplayPoint::new(2, 1, 1)
        );
        assert_eq!(
            wrapper.offset_to_display_point(37),
            WrapDisplayPoint::new(3, 0, 0)
        );
        assert_eq!(
            wrapper.offset_to_display_point(54),
            WrapDisplayPoint::new(5, 2, 2)
        );
        assert_eq!(
            wrapper.offset_to_display_point(59),
            WrapDisplayPoint::new(6, 0, 2)
        );

        assert_eq!(
            wrapper.display_point_to_offset(WrapDisplayPoint::new(6, 0, 2)),
            59
        );
        assert_eq!(
            wrapper.display_point_to_offset(WrapDisplayPoint::new(5, 2, 2)),
            54
        );
        assert_eq!(
            wrapper.display_point_to_offset(WrapDisplayPoint::new(3, 0, 0)),
            37
        );
        assert_eq!(
            wrapper.display_point_to_offset(WrapDisplayPoint::new(2, 1, 1)),
            27
        );
        assert_eq!(
            wrapper.display_point_to_offset(WrapDisplayPoint::new(1, 0, 5)),
            21
        );
        assert_eq!(
            wrapper.display_point_to_offset(WrapDisplayPoint::new(1, 0, 0)),
            16
        );
        assert_eq!(
            wrapper.display_point_to_offset(WrapDisplayPoint::new(0, 0, 15)),
            15
        );
    }

    #[test]
    fn test_wrapping_indent_same_keeps_indent_reserved() {
        let mut wrapper = TextWrapper::new(test_font(), px(14.0), Some(px(10.)));
        wrapper.wrapping_indent = WrappingIndent::Same;
        let text = Rope::from("  abcdefghijklmnopqrstuv");
        let mut fake_wrap_line = |line: &str, _wrap_width: Pixels| {
            if line.starts_with(' ') {
                vec![Boundary {
                    ix: 5,
                    next_indent: 2,
                }]
            } else {
                let mut boundaries = vec![];
                let mut i = 8;
                while i < line.len() {
                    boundaries.push(Boundary {
                        ix: i,
                        next_indent: 0,
                    });
                    i += 8;
                }
                boundaries
            }
        };

        wrapper._update(&text, &(0..text.len()), &text, &mut fake_wrap_line);

        let line = wrapper.line(0).unwrap();
        assert_eq!(line.indent, 2);
        assert_eq!(line.wrapped_lines.as_slice(), [0..5, 5..24]);
    }

    #[test]
    fn test_wrapping_indent_none_continuation_lines_wrapped_at_full_width() {
        let mut wrapper = TextWrapper::new(test_font(), px(14.0), Some(px(10.)));
        wrapper.wrapping_indent = WrappingIndent::None;
        let text = Rope::from("  abcdefghijklmnopqrstuv");
        let mut fake_wrap_line = |line: &str, _wrap_width: Pixels| {
            if line.starts_with(' ') {
                vec![Boundary {
                    ix: 5,
                    next_indent: 2,
                }]
            } else {
                let mut boundaries = vec![];
                let mut i = 8;
                while i < line.len() {
                    boundaries.push(Boundary {
                        ix: i,
                        next_indent: 0,
                    });
                    i += 8;
                }
                boundaries
            }
        };

        wrapper._update(&text, &(0..text.len()), &text, &mut fake_wrap_line);

        let line = wrapper.line(0).unwrap();
        assert_eq!(line.indent, 0);
        assert_eq!(line.wrapped_lines.as_slice(), [0..5, 5..13, 13..21, 21..24]);
    }

    #[test]
    fn test_wrap_indent_offsets_continuation_lines() {
        let mut line_layout = LineLayout::new();
        line_layout.set_wrapped_lines(smallvec::smallvec![
            ShapedLine::default().with_len(5),
            ShapedLine::default().with_len(10),
        ]);

        line_layout = line_layout.wrap_indent(px(20.0));

        let last_layout = LastLayout {
            visible_range: 0..1,
            visible_buffer_lines: vec![0],
            visible_line_byte_offsets: vec![0],
            visible_top: px(0.),
            visible_range_offset: 0..0,
            lines: Rc::new(vec![]),
            line_height: px(20.0),
            wrap_width: Some(px(10.)),
            wrapping_indent: WrappingIndent::Same,
            line_number_width: px(0.),
            cursor_bounds: None,
            text_align: TextAlign::Left,
            content_width: px(0.),
        };

        assert_eq!(
            line_layout.position_for_index(0, &last_layout, false),
            Some(point(px(0.), px(0.))),
        );

        assert_eq!(
            line_layout.position_for_index(6, &last_layout, false),
            Some(point(px(20.), px(20.))),
        )
    }

    #[test]
    fn test_normalize_concealed() {
        assert_eq!(normalize_concealed(vec![]), Vec::<Range<usize>>::new());
        assert_eq!(
            normalize_concealed(vec![3..3, 5..5]),
            Vec::<Range<usize>>::new()
        );
        assert_eq!(
            normalize_concealed(vec![10..12, 0..2, 5..7]),
            vec![0..2, 5..7, 10..12]
        );
        // Overlapping and touching ranges are merged.
        assert_eq!(
            normalize_concealed(vec![0..3, 2..5, 5..6, 8..9]),
            vec![0..6, 8..9]
        );
    }

    #[test]
    fn test_byte_map_identity() {
        let layout =
            LineLayout::new().lines(smallvec::smallvec![ShapedLine::default().with_len(10)]);
        assert_eq!(layout.len(), 10);
        for i in 0..=12 {
            assert_eq!(layout.raw_to_display(i), i);
            assert_eq!(layout.display_to_raw(i), i);
        }
    }

    #[test]
    fn test_byte_map_conceal_at_start() {
        // Raw "# Hello" (7 bytes), concealed "# " (0..2) -> display "Hello" (5 bytes).
        let layout = LineLayout::new()
            .lines(smallvec::smallvec![ShapedLine::default().with_len(5)])
            .with_concealed(vec![0..2]);
        assert_eq!(layout.len(), 7);

        assert_eq!(layout.raw_to_display(0), 0);
        assert_eq!(layout.raw_to_display(1), 0); // inside concealed -> collapse to its start
        assert_eq!(layout.raw_to_display(2), 0);
        assert_eq!(layout.raw_to_display(3), 1);
        assert_eq!(layout.raw_to_display(7), 5);
        // Beyond the end: shifted by the total concealed length.
        assert_eq!(layout.raw_to_display(8), 6);

        assert_eq!(layout.display_to_raw(0), 0); // before the hidden bytes
        assert_eq!(layout.display_to_raw(1), 3);
        assert_eq!(layout.display_to_raw(5), 7);
        assert_eq!(layout.display_to_raw(6), 8);
    }

    #[test]
    fn test_byte_map_conceal_in_middle() {
        // Raw "a **b** c" (9 bytes), concealed "**" at 2..4 and 5..7 -> display "a b c".
        let layout = LineLayout::new()
            .lines(smallvec::smallvec![ShapedLine::default().with_len(5)])
            .with_concealed(vec![5..7, 2..4]);
        assert_eq!(layout.len(), 9);

        let expected_r2d = [0, 1, 2, 2, 2, 3, 3, 3, 4, 5, 6];
        for (raw, display) in expected_r2d.into_iter().enumerate() {
            assert_eq!(layout.raw_to_display(raw), display, "raw {raw}");
        }

        assert_eq!(layout.display_to_raw(0), 0);
        assert_eq!(layout.display_to_raw(1), 1);
        assert_eq!(layout.display_to_raw(2), 2); // boundary -> start of the hidden range
        assert_eq!(layout.display_to_raw(3), 5);
        assert_eq!(layout.display_to_raw(4), 8);
        assert_eq!(layout.display_to_raw(5), 9);
        assert_eq!(layout.display_to_raw(7), 11);

        // Round trip from display space is stable.
        for display in 0..=6 {
            assert_eq!(
                layout.raw_to_display(layout.display_to_raw(display)),
                display
            );
        }
    }

    #[test]
    fn test_byte_map_conceal_at_end() {
        // Raw "Hello   " (8 bytes), concealed trailing 5..8 -> display "Hello".
        let layout = LineLayout::new()
            .lines(smallvec::smallvec![ShapedLine::default().with_len(5)])
            .with_concealed(vec![5..8]);
        assert_eq!(layout.len(), 8);
        assert_eq!(layout.raw_to_display(5), 5);
        assert_eq!(layout.raw_to_display(6), 5);
        assert_eq!(layout.raw_to_display(8), 5);
        assert_eq!(layout.raw_to_display(9), 6);
        assert_eq!(layout.display_to_raw(5), 5);
        assert_eq!(layout.display_to_raw(6), 9);
    }

    #[test]
    fn test_byte_map_position_and_index_use_raw_offsets() {
        // Two visual lines shaped from display text: raw "**ab**cd" (8 bytes)
        // wrapped as raw 0..4 ("**ab" -> display "ab") and 4..8 ("**cd" -> "cd").
        let layout = LineLayout::new()
            .lines(smallvec::smallvec![
                ShapedLine::default().with_len(2),
                ShapedLine::default().with_len(2),
            ])
            .with_concealed(vec![0..2, 4..6]);
        assert_eq!(layout.len(), 8);

        let last_layout = LastLayout {
            visible_range: 0..1,
            visible_buffer_lines: vec![0],
            visible_line_byte_offsets: vec![0],
            visible_top: px(0.),
            visible_range_offset: 0..0,
            lines: Rc::new(vec![]),
            line_height: px(20.),
            wrap_width: Some(px(10.)),
            wrapping_indent: WrappingIndent::None,
            line_number_width: px(0.),
            cursor_bounds: None,
            text_align: TextAlign::Left,
            content_width: px(0.),
        };

        // Raw offsets inside the first wrapped range stay on row 0 ...
        assert_eq!(
            layout
                .position_for_index(0, &last_layout, false)
                .map(|p| p.y),
            Some(px(0.))
        );
        assert_eq!(
            layout
                .position_for_index(3, &last_layout, false)
                .map(|p| p.y),
            Some(px(0.))
        );
        // ... the wrap boundary (raw 4, display 2) belongs to the second row ...
        assert_eq!(
            layout
                .position_for_index(4, &last_layout, false)
                .map(|p| p.y),
            Some(px(20.))
        );
        assert_eq!(
            layout
                .position_for_index(8, &last_layout, false)
                .map(|p| p.y),
            Some(px(20.))
        );
        // ... and past the raw end (the `\n`) there is no position.
        assert_eq!(layout.position_for_index(9, &last_layout, false), None);

        // Hit-testing returns raw offsets (the default shaped line has zero
        // width, so every x maps to display index 0 of the row).
        assert_eq!(
            layout.closest_index_for_position(point(px(0.), px(25.)), &last_layout),
            Some(4)
        );
        assert_eq!(
            layout.index_for_position(point(px(0.), px(25.)), &last_layout),
            None
        );
    }

    /// Builds a wrapper whose scale is driven by the first character of each
    /// line, so a test can say "this line is a heading" without a highlighter.
    /// The scale closure reads through a shared cell so a test can point it at
    /// the edited text, the way the real caller points it at the live buffer.
    fn wrapper_with_scales_from(source: &Rc<std::cell::RefCell<Rope>>) -> TextWrapper {
        let font = gpui::Font {
            family: "Arial".into(),
            weight: FontWeight::default(),
            style: FontStyle::Normal,
            features: FontFeatures::default(),
            fallbacks: None,
        };
        let mut wrapper = TextWrapper::new(font, px(14.), None);
        let text = source.borrow().clone();
        let source = Rc::clone(source);
        wrapper.height_scale = Some(Rc::new(move |range: &Range<usize>| {
            let text = source.borrow();
            if range.end > text.len() {
                return 1.0;
            }
            match text.slice(range.clone()).chars().next() {
                Some('#') => 2.0,
                Some('=') => 1.5,
                _ => 1.0,
            }
        }));
        wrapper._update(&text, &(0..text.len()), &text, &mut |_, _| vec![]);
        wrapper
    }

    fn wrapper_with_scales(text: &Rope) -> TextWrapper {
        wrapper_with_scales_from(&Rc::new(std::cell::RefCell::new(text.clone())))
    }

    #[test]
    fn a_document_without_a_scale_reports_uniform_heights() {
        let font = gpui::Font {
            family: "Arial".into(),
            weight: FontWeight::default(),
            style: FontStyle::Normal,
            features: FontFeatures::default(),
            fallbacks: None,
        };
        let mut wrapper = TextWrapper::new(font, px(14.), None);
        let text = Rope::from("one\ntwo\nthree\n");
        wrapper._update(&text, &(0..text.len()), &text, &mut |_, _| vec![]);

        assert!(wrapper.is_uniform_height());
        assert_eq!(wrapper.line_top(2), 2.0);
        assert_eq!(
            wrapper.total_height(),
            4.0,
            "three lines and the empty last"
        );
        assert_eq!(wrapper.line_at_height(2.5), (2, 0.5));
    }

    #[test]
    fn a_taller_line_pushes_down_everything_after_it() {
        let text = Rope::from("plain\n# heading\nplain\n");
        let wrapper = wrapper_with_scales(&text);

        assert!(!wrapper.is_uniform_height());
        assert_eq!(wrapper.line_height_scale(1), 2.0);
        assert_eq!(wrapper.line_top(0), 0.0);
        assert_eq!(wrapper.line_top(1), 1.0);
        assert_eq!(wrapper.line_top(2), 3.0, "after the double-height line");
        assert_eq!(wrapper.total_height(), 5.0);
    }

    /// The seek is the whole reason heights live in the tree: a scroll position
    /// must land on the row it is actually inside.
    #[test]
    fn a_height_maps_back_to_the_line_containing_it() {
        let text = Rope::from("plain\n# heading\n= sub\nplain\n");
        let wrapper = wrapper_with_scales(&text);
        // Tops: 0, 1, 3, 4.5; total 5.5.
        for (height, expected_row) in [
            (0.0, 0),
            (0.9, 0),
            (1.0, 1),
            (2.9, 1),
            (3.0, 2),
            (4.4, 2),
            (4.5, 3),
        ] {
            let (row, _) = wrapper.line_at_height(height);
            assert_eq!(row, expected_row, "height {height}");
        }
    }

    /// Every line's top must equal the sum of the heights above it. This is the
    /// invariant a caret and a scrollbar both read, so it is checked at every
    /// row rather than at a sampled one.
    #[test]
    fn tops_are_the_running_sum_of_the_heights_before_them() {
        let text = Rope::from("a\n# b\nc\n= d\n# e\nf\n");
        let wrapper = wrapper_with_scales(&text);

        let mut running = 0.0;
        for row in 0..wrapper.lines_count() {
            assert_eq!(wrapper.line_top(row), running, "top of row {row}");
            // And the point just inside the row maps back to it.
            let (found, _) = wrapper.line_at_height(running + 0.01);
            assert_eq!(found, row, "seek into row {row}");
            running +=
                wrapper.line_height_scale(row) * wrapper.line(row).unwrap().lines_len() as f32;
        }
        assert_eq!(wrapper.total_height(), running);
    }

    /// An edit must only rebuild the rows it touched, and the heights of the
    /// untouched rows must survive it — that is what makes this incremental.
    #[test]
    fn an_edit_keeps_the_heights_of_the_lines_it_did_not_touch() {
        let text = Rope::from("# one\nplain\n# three\n");
        let source = Rc::new(std::cell::RefCell::new(text.clone()));
        let mut wrapper = wrapper_with_scales_from(&source);
        assert_eq!(wrapper.total_height(), 6.0);

        // Rewrite the middle line only; the closure now sees the edited text,
        // as the real caller's would.
        let edited = Rope::from("# one\nplain plain\n# three\n");
        *source.borrow_mut() = edited.clone();
        let start = text.line_start_offset(1);
        let end = text.line_end_offset(1);
        wrapper._update(&edited, &(start..end), &edited, &mut |_, _| vec![]);

        assert_eq!(wrapper.line_height_scale(0), 2.0, "heading above survived");
        assert_eq!(wrapper.line_height_scale(1), 1.0);
        assert_eq!(wrapper.line_height_scale(2), 2.0, "heading below survived");
        assert_eq!(wrapper.line_top(2), 3.0);
    }

    /// A scale the application computes from a stale or broken state must not
    /// poison the sums — every seek afterwards would be wrong.
    #[test]
    fn an_impossible_scale_is_refused_rather_than_summed() {
        for bad in [f32::NAN, f32::INFINITY, -1.0, 0.0] {
            assert_eq!(normalize_scale(bad), 1.0, "{bad} should fall back");
        }
        assert_eq!(
            normalize_scale(1e9),
            MAX_HEIGHT_SCALE,
            "clamped, not summed"
        );
        assert_eq!(normalize_scale(2.5), 2.5, "an ordinary scale is kept");
    }
}
