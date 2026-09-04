//! The layout of one table row: its cells as shaped text, placed in columns.
//!
//! A table row is a buffer line the engine lays out as one segment per cell.
//! Everything the engine asks a line's layout -- where an offset is drawn,
//! which offset a point is over, what a selection covers, how tall the row is,
//! how to paint it -- is answered here per cell, from the same shaped lines
//! that are painted. There is no second geometry: the glyphs on screen are the
//! ones the caret and the hit test measure.
//!
//! Offsets are raw bytes relative to the line start; positions are relative to
//! the line's top-left corner, as for every other `LineLayout`.

use std::ops::Range;

use gpui::{
    App, Bounds, Corners, Hsla, Pixels, Point, ShapedLine, Size, TextAlign, Window, fill, point,
    px, size,
};
use smallvec::SmallVec;

use crate::input::display_map::{CELL_PAD, display_to_raw, raw_to_display};
use crate::input::{ColumnAlign, TableRowKind};

/// Colours the row is painted with, taken from the editor style at layout time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TableChrome {
    pub(crate) border: Hsla,
    pub(crate) header_background: Hsla,
    pub(crate) focused_background: Hsla,
}

/// Where a column sits, relative to the line origin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ColumnGeometry {
    pub(crate) x: Pixels,
    pub(crate) width: Pixels,
}

impl ColumnGeometry {
    /// Left edge of the column's text.
    fn text_x(&self) -> Pixels {
        self.x + CELL_PAD
    }

    fn text_width(&self) -> Pixels {
        (self.width - CELL_PAD * 2.).max(px(0.))
    }
}

/// One cell: its bytes, its wrapped rows and the shaped text of each.
#[derive(Debug)]
pub(crate) struct CellLayout {
    /// The cell's text without padding, relative to the line start.
    pub(crate) content: Range<usize>,
    /// Index of the pipe closing the cell, or the line length.
    pub(crate) separator: usize,
    /// Raw byte range of each wrapped row, relative to the line start.
    pub(crate) rows: SmallVec<[Range<usize>; 1]>,
    /// The shaped display text of each row, parallel to `rows`.
    pub(crate) lines: SmallVec<[ShapedLine; 1]>,
    pub(crate) align: ColumnAlign,
}

#[derive(Debug)]
pub(crate) struct TableRowLayout {
    pub(crate) kind: TableRowKind,
    pub(crate) columns: Vec<ColumnGeometry>,
    pub(crate) cells: Vec<CellLayout>,
    /// Concealed raw ranges of the whole line, relative to its start; the
    /// shaped rows are the raw rows with these removed.
    pub(crate) concealed: Vec<Range<usize>>,
    /// Height of one text row inside this table row.
    pub(crate) text_row_height: Pixels,
    /// Text rows the row reserves: its tallest cell's count.
    pub(crate) rows: usize,
    /// The whole table's width.
    pub(crate) width: Pixels,
    /// Raw byte length of the line.
    pub(crate) len: usize,
    /// The cell the selection lies inside, if it lies inside one.
    pub(crate) focused: Option<usize>,
    /// Whether this is the table's first row, which draws the top rule; every
    /// row draws its own bottom rule.
    pub(crate) first: bool,
    pub(crate) chrome: TableChrome,
}

impl TableRowLayout {
    /// The cell an offset belongs to, and the offset clamped into that cell's
    /// content. A byte in padding or on a separator belongs to the nearest
    /// cell edge, so every offset of the line has a place.
    pub(crate) fn cell_of(&self, offset: usize) -> (usize, usize) {
        for (ix, cell) in self.cells.iter().enumerate() {
            if offset < cell.content.start {
                if ix == 0 {
                    return (0, cell.content.start);
                }
                let previous = &self.cells[ix - 1];
                return if offset <= previous.separator {
                    (ix - 1, previous.content.end)
                } else {
                    (ix, cell.content.start)
                };
            }
            if offset <= cell.content.end {
                return (ix, offset);
            }
        }
        let last = self.cells.len().saturating_sub(1);
        let end = self.cells.get(last).map(|c| c.content.end).unwrap_or(0);
        (last, end)
    }

    /// The wrapped row of `cell` holding `offset`, and the display index of
    /// the offset within that row's shaped text.
    fn row_of(&self, cell: &CellLayout, offset: usize, line_end_affinity: bool) -> (usize, usize) {
        let rows = cell.rows.len();
        for (k, row) in cell.rows.iter().enumerate() {
            let is_last = k + 1 == rows;
            let matches = if row.is_empty() {
                offset == row.start
            } else if is_last || line_end_affinity {
                offset >= row.start && offset <= row.end
            } else {
                offset >= row.start && offset < row.end
            };
            if matches {
                return (k, self.display_ix(row, offset));
            }
        }
        let k = rows.saturating_sub(1);
        let row = cell.rows.get(k).cloned().unwrap_or(0..0);
        (k, self.display_ix(&row, row.end))
    }

    fn display_ix(&self, row: &Range<usize>, offset: usize) -> usize {
        raw_to_display(&self.concealed, offset.max(row.start))
            .saturating_sub(raw_to_display(&self.concealed, row.start))
    }

    fn raw_of(&self, row: &Range<usize>, display_ix: usize) -> usize {
        let display_start = raw_to_display(&self.concealed, row.start);
        display_to_raw(&self.concealed, display_start + display_ix).clamp(row.start, row.end)
    }

    fn align_offset(&self, cell_ix: usize, k: usize) -> Pixels {
        let (Some(cell), Some(column)) = (self.cells.get(cell_ix), self.columns.get(cell_ix))
        else {
            return px(0.);
        };
        let line_width = cell.lines.get(k).map(|l| l.width).unwrap_or(px(0.));
        match cell.align {
            ColumnAlign::Left => px(0.),
            ColumnAlign::Center => ((column.text_width() - line_width) / 2.).max(px(0.)),
            ColumnAlign::Right => (column.text_width() - line_width).max(px(0.)),
        }
    }

    pub(crate) fn position_for_index(
        &self,
        offset: usize,
        line_end_affinity: bool,
    ) -> Option<Point<Pixels>> {
        let (cell_ix, offset) = self.cell_of(offset);
        let cell = self.cells.get(cell_ix)?;
        let column = self.columns.get(cell_ix)?;
        let (k, display_ix) = self.row_of(cell, offset, line_end_affinity);
        let line = cell.lines.get(k)?;
        let x = column.text_x() + self.align_offset(cell_ix, k) + line.x_for_index(display_ix);
        Some(point(x, self.text_row_height * k))
    }

    /// The column under `x`, clamped to the outer columns.
    fn column_at(&self, x: Pixels) -> usize {
        let last = self.columns.len().saturating_sub(1);
        self.columns
            .iter()
            .position(|column| x < column.x + column.width)
            .unwrap_or(last)
            .min(last)
    }

    fn text_row_at(&self, cell: &CellLayout, y: Pixels) -> usize {
        let k = (f32::from(y) / f32::from(self.text_row_height))
            .floor()
            .max(0.) as usize;
        k.min(cell.rows.len().saturating_sub(1))
    }

    /// The offset nearest to `pos`, for any point inside the row's height.
    pub(crate) fn closest_index_for_position(&self, pos: Point<Pixels>) -> Option<usize> {
        if pos.y < px(0.) || pos.y >= self.size().height {
            return None;
        }
        let cell_ix = self.column_at(pos.x);
        let cell = self.cells.get(cell_ix)?;
        let k = self.text_row_at(cell, pos.y);
        Some(self.closest_in_row(cell_ix, cell, k, pos.x))
    }

    /// The offset nearest to `x` on the first text row, for single-line hit
    /// testing.
    pub(crate) fn closest_index_for_x(&self, x: Pixels) -> usize {
        let cell_ix = self.column_at(x);
        match self.cells.get(cell_ix) {
            Some(cell) => self.closest_in_row(cell_ix, cell, 0, x),
            None => 0,
        }
    }

    fn closest_in_row(&self, cell_ix: usize, cell: &CellLayout, k: usize, x: Pixels) -> usize {
        let Some(column) = self.columns.get(cell_ix) else {
            return cell.content.start;
        };
        let Some(line) = cell.lines.get(k) else {
            return cell.content.start;
        };
        let local_x = x - column.text_x() - self.align_offset(cell_ix, k);
        let mut ix = line.closest_index_for_x(local_x);
        let is_last = k + 1 == cell.rows.len();
        if !is_last && ix == line.text.len() {
            // A soft-wrapped row cannot hold the caret at its end.
            let c_len = line.text.chars().last().map(|c| c.len_utf8()).unwrap_or(0);
            ix = ix.saturating_sub(c_len);
        }
        let row = cell.rows.get(k).cloned().unwrap_or(cell.content.clone());
        self.raw_of(&row, ix)
    }

    /// The offset of the glyph under `pos`, or `None` off any glyph.
    pub(crate) fn index_for_position(&self, pos: Point<Pixels>) -> Option<usize> {
        if pos.y < px(0.) || pos.y >= self.size().height {
            return None;
        }
        let cell_ix = self.column_at(pos.x);
        let cell = self.cells.get(cell_ix)?;
        let column = self.columns.get(cell_ix)?;
        let k = self.text_row_at(cell, pos.y);
        let line = cell.lines.get(k)?;
        let local_x = pos.x - column.text_x() - self.align_offset(cell_ix, k);
        let ix = line.index_for_x(local_x)?;
        let row = cell.rows.get(k).cloned().unwrap_or(cell.content.clone());
        Some(self.raw_of(&row, ix))
    }

    pub(crate) fn size(&self) -> Size<Pixels> {
        size(self.width, self.text_row_height * self.rows.max(1))
    }

    /// The rectangles a selection covers on this row, relative to the line
    /// origin.
    ///
    /// Inside one cell it is a text selection, row by row of that cell. Once
    /// it reaches past a cell it takes whole cells: from the cell it enters
    /// (or the table's left edge when it comes from before the line) to the
    /// cell it leaves (or the right edge when it runs past the line), the full
    /// height of the row -- how Word shows a selection across cells.
    pub(crate) fn selection_corners(
        &self,
        local: Range<usize>,
        starts_before: bool,
        ends_after: bool,
    ) -> Vec<Corners<Point<Pixels>>> {
        let height = self.size().height;
        let rect = |left: Pixels, top: Pixels, right: Pixels, bottom: Pixels| Corners {
            top_left: point(left, top),
            top_right: point(right, top),
            bottom_left: point(left, bottom),
            bottom_right: point(right, bottom),
        };
        let (start_cell, start_offset) = self.cell_of(local.start);
        let (end_cell, end_offset) = self.cell_of(local.end);

        let inside_one_cell = !starts_before && !ends_after && start_cell == end_cell;
        if inside_one_cell {
            let Some(cell) = self.cells.get(start_cell) else {
                return Vec::new();
            };
            let Some(column) = self.columns.get(start_cell) else {
                return Vec::new();
            };
            let (k_start, _) = self.row_of(cell, start_offset, false);
            let (k_end, _) = self.row_of(cell, end_offset, false);
            let start = self
                .position_for_index(start_offset, false)
                .unwrap_or(point(column.text_x(), px(0.)));
            let end = self
                .position_for_index(end_offset, false)
                .unwrap_or(point(column.text_x(), px(0.)));
            let mut out = Vec::new();
            for k in k_start..=k_end {
                let top = self.text_row_height * k;
                let bottom = top + self.text_row_height;
                let left = if k == k_start {
                    start.x
                } else {
                    column.text_x()
                };
                let right = if k == k_end {
                    end.x.max(left + px(6.))
                } else {
                    column.text_x() + column.text_width()
                };
                out.push(rect(left, top, right, bottom));
            }
            return out;
        }

        let left = if starts_before {
            px(0.)
        } else {
            self.columns.get(start_cell).map(|c| c.x).unwrap_or(px(0.))
        };
        let right = if ends_after {
            self.width
        } else {
            self.columns
                .get(end_cell)
                .map(|c| c.x + c.width)
                .unwrap_or(self.width)
        };
        vec![rect(left, px(0.), right.max(left + px(6.)), height)]
    }

    /// The row's backgrounds: the header's tint and the revealed cell's.
    /// Painted before the selection, which the engine paints before the
    /// text; painted with the text they covered every selection in a table.
    pub(crate) fn paint_background(&self, origin: Point<Pixels>, window: &mut Window) {
        let row_size = self.size();
        if self.kind == TableRowKind::Header {
            window.paint_quad(fill(
                Bounds::new(origin, row_size),
                self.chrome.header_background,
            ));
        }
        if let Some(focused) = self.focused
            && let Some(column) = self.columns.get(focused)
        {
            window.paint_quad(fill(
                Bounds::new(
                    origin + point(column.x, px(0.)),
                    size(column.width, row_size.height),
                ),
                self.chrome.focused_background,
            ));
        }
    }

    /// The row's text and chrome; see [`Self::paint_background`] for what
    /// goes under the selection.
    pub(crate) fn paint(&self, origin: Point<Pixels>, window: &mut Window, cx: &mut App) {
        let row_size = self.size();

        for (cell_ix, cell) in self.cells.iter().enumerate() {
            let Some(column) = self.columns.get(cell_ix) else {
                continue;
            };
            // Clipped to the column: a glyph wider than the wrapper assumed
            // is cut at the border rather than drawn into the next cell.
            let mask = gpui::ContentMask {
                bounds: Bounds::new(
                    origin + point(column.x, px(0.)),
                    size(column.width, row_size.height),
                ),
            };
            window.with_content_mask(Some(mask), |window| {
                for (k, line) in cell.lines.iter().enumerate() {
                    let pos = origin
                        + point(
                            column.text_x() + self.align_offset(cell_ix, k),
                            self.text_row_height * k,
                        );
                    _ = line.paint(pos, self.text_row_height, TextAlign::Left, None, window, cx);
                }
            });
        }

        // Chrome: column separators, the outer rules, and a heavier rule under
        // the delimiter row, which is where a reader expects the header line.
        let border = self.chrome.border;
        let hairline = px(1.);
        for column in &self.columns {
            window.paint_quad(fill(
                Bounds::new(
                    origin + point(column.x, px(0.)),
                    size(hairline, row_size.height),
                ),
                border,
            ));
        }
        window.paint_quad(fill(
            Bounds::new(
                origin + point(row_size.width - hairline, px(0.)),
                size(hairline, row_size.height),
            ),
            border,
        ));
        if self.first {
            window.paint_quad(fill(
                Bounds::new(origin, size(row_size.width, hairline)),
                border,
            ));
        }
        let rule = if self.kind == TableRowKind::Delimiter {
            px(2.)
        } else {
            hairline
        };
        window.paint_quad(fill(
            Bounds::new(
                origin + point(px(0.), row_size.height - rule),
                size(row_size.width, rule),
            ),
            border,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(content: Range<usize>, separator: usize, rows: &[Range<usize>]) -> CellLayout {
        CellLayout {
            content,
            separator,
            rows: rows.iter().cloned().collect(),
            lines: rows
                .iter()
                .map(|r| ShapedLine::default().with_len(r.len()))
                .collect(),
            align: ColumnAlign::Left,
        }
    }

    /// `| abc | de |` laid out in two 50px columns, one text row each.
    fn layout() -> TableRowLayout {
        TableRowLayout {
            kind: TableRowKind::Body,
            columns: vec![
                ColumnGeometry {
                    x: px(0.),
                    width: px(50.),
                },
                ColumnGeometry {
                    x: px(50.),
                    width: px(50.),
                },
            ],
            cells: vec![cell(2..5, 6, &[2..5]), cell(8..10, 11, &[8..10])],
            concealed: Vec::new(),
            text_row_height: px(20.),
            rows: 1,
            width: px(100.),
            len: 12,
            focused: None,
            first: false,
            chrome: TableChrome {
                border: Hsla::default(),
                header_background: Hsla::default(),
                focused_background: Hsla::default(),
            },
        }
    }

    #[test]
    fn every_offset_of_the_line_belongs_to_a_cell() {
        let layout = layout();
        // "| abc | de |": 0 pipe, 1 space, 2..5 abc, 5 space, 6 pipe, 7 space,
        // 8..10 de, 10 space, 11 pipe.
        assert_eq!(layout.cell_of(0), (0, 2), "the leading pipe: cell 0 start");
        assert_eq!(layout.cell_of(1), (0, 2));
        assert_eq!(layout.cell_of(3), (0, 3), "inside abc");
        assert_eq!(layout.cell_of(5), (0, 5), "the end of abc");
        assert_eq!(layout.cell_of(6), (0, 5), "the separator: cell 0 end");
        assert_eq!(
            layout.cell_of(7),
            (1, 8),
            "past the separator: cell 1 start"
        );
        assert_eq!(layout.cell_of(10), (1, 10));
        assert_eq!(layout.cell_of(11), (1, 10), "the trailing pipe: cell 1 end");
        assert_eq!(
            layout.cell_of(40),
            (1, 10),
            "past the line: the last cell's end"
        );
    }

    #[test]
    fn positions_start_at_the_column_s_text_edge() {
        let layout = layout();
        // The test shaped lines have no glyphs, so x is the column's text x.
        assert_eq!(
            layout.position_for_index(2, false),
            Some(point(CELL_PAD, px(0.)))
        );
        assert_eq!(
            layout.position_for_index(9, false),
            Some(point(px(50.) + CELL_PAD, px(0.)))
        );
        assert_eq!(
            layout.position_for_index(0, false),
            Some(point(CELL_PAD, px(0.))),
            "a padding byte draws at its cell's edge"
        );
    }

    #[test]
    fn a_point_resolves_to_the_cell_under_it_and_nothing_outside_the_row() {
        let layout = layout();
        // The test shaped lines have no glyphs, so a point resolves to the
        // cell under it but not to a glyph within it; the headless tests with
        // the deterministic text system pin the glyph.
        let in_cell = |x: f32, cell: Range<usize>| {
            let got = layout
                .closest_index_for_position(point(px(x), px(5.)))
                .expect("inside the row");
            assert!(cell.contains(&got) || got == cell.end, "x {x} -> {got}");
        };
        in_cell(1., 2..5);
        in_cell(30., 2..5);
        in_cell(70., 8..10);
        in_cell(500., 8..10);
        assert_eq!(
            layout.closest_index_for_position(point(px(10.), px(25.))),
            None
        );
        assert_eq!(
            layout.closest_index_for_position(point(px(10.), px(-1.))),
            None
        );
    }

    #[test]
    fn a_selection_inside_one_cell_is_text_and_across_cells_is_whole_cells() {
        let layout = layout();
        let inside = layout.selection_corners(2..4, false, false);
        assert_eq!(inside.len(), 1);
        assert_eq!(inside[0].top_left.x, CELL_PAD);
        assert_eq!(inside[0].bottom_left.y, px(20.));

        let across = layout.selection_corners(3..9, false, false);
        assert_eq!(across.len(), 1);
        assert_eq!(across[0].top_left.x, px(0.), "from the entry cell's edge");
        assert_eq!(across[0].top_right.x, px(100.), "to the exit cell's edge");

        let from_before = layout.selection_corners(0..9, true, false);
        assert_eq!(from_before[0].top_left.x, px(0.));
        assert_eq!(from_before[0].top_right.x, px(100.));

        let through = layout.selection_corners(0..12, true, true);
        assert_eq!(through[0].top_right.x, px(100.));
    }

    #[test]
    fn the_row_is_as_tall_as_its_text_rows() {
        let mut layout = layout();
        layout.rows = 3;
        assert_eq!(layout.size(), size(px(100.), px(60.)));
    }
}
