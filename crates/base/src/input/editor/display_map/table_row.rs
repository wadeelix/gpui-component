//! A table row inside the display map: its cells wrapped at their column's
//! width, in the same pass that wraps every other line.
//!
//! The row's height is `max` over its cells of the wrap rows a cell needs, and
//! it is decided here, at edit time, with the wrapper's own line wrapper. Nothing
//! measures a drawn grid afterwards: the shaped rows `layout_lines` produces are
//! exactly the ranges stored here, so what is drawn cannot exceed what was
//! reserved. Two records of one height were the root of every defect of the
//! measured-height experiment, and this keeps one.

use std::ops::Range;

use gpui::{Pixels, px};
use smallvec::SmallVec;

use crate::input::{ColumnAlign, TableCellSpan, TableRow, TableRowKind};

/// Padding inside a cell, on each side of its text.
///
/// The one place horizontal table geometry starts from: the wrapper wraps a
/// cell's text at the width this leaves, and the layout places the text this
/// far in from the column's edge. A second constant somewhere else is how the
/// first cell editor's hit boxes drifted from its grid by exactly the padding.
pub(crate) const CELL_PAD: Pixels = px(6.);

/// A cell taller than this many wrap rows is reserved this much room and clipped
/// beyond it. A narrow window with many columns makes every cell wrap at almost
/// every character; without a ceiling one such row would dwarf the document.
const MAX_ROWS_PER_CELL: usize = 32;

/// Width the text of one cell wraps at, for a table of `columns` columns laid
/// out across `wrap_width`. Columns are equal: a pure function of two numbers,
/// so the wrapper, the layout and the hit test cannot disagree on it.
pub(crate) fn cell_text_width(wrap_width: Pixels, columns: usize) -> Pixels {
    let columns = columns.max(1) as f32;
    (wrap_width / columns - CELL_PAD * 2.).max(px(1.))
}

/// The wrap-time shape of one table row: what `layout_lines` shapes and what
/// the summed heights were built from.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TableRowItem {
    pub(crate) kind: TableRowKind,
    pub(crate) columns: usize,
    pub(crate) aligns: Vec<ColumnAlign>,
    /// Exactly `columns` cells, relative to the line start.
    pub(crate) cells: Vec<TableCellSpan>,
    /// Per cell, the wrapped byte ranges of its content, relative to the line
    /// start. At least one range per cell, empty for an empty cell.
    pub(crate) cell_lines: Vec<SmallVec<[Range<usize>; 1]>>,
    /// Wrap rows the row needs: the tallest cell's count, at least one.
    pub(crate) rows: usize,
}

impl TableRowItem {
    /// Wraps every cell of `row` at the column's text width with `wrap_line`,
    /// the same closure the wrapper wraps prose with.
    pub(crate) fn build<F>(
        row: &TableRow,
        line: &str,
        wrap_width: Pixels,
        wrap_line: &mut F,
    ) -> Self
    where
        F: FnMut(&str, Pixels) -> Vec<gpui::Boundary>,
    {
        let width = cell_text_width(wrap_width, row.columns);
        let mut cell_lines = Vec::with_capacity(row.cells.len());
        let mut rows = 1;
        for cell in &row.cells {
            let content = clamp_to_line(line, &cell.content);
            let text = &line[content.clone()];
            let mut ranges: SmallVec<[Range<usize>; 1]> = SmallVec::new();
            let mut prev = 0;
            for boundary in wrap_line(text, width) {
                if boundary.ix > prev && boundary.ix <= text.len() {
                    ranges.push(content.start + prev..content.start + boundary.ix);
                    prev = boundary.ix;
                }
            }
            if prev < text.len() || ranges.is_empty() {
                ranges.push(content.start + prev..content.end);
            }
            // Past the ceiling the last kept row takes the rest of the cell:
            // nothing is shaped or drawn for it, and an offset in it resolves
            // to that row's end rather than to a row below the table.
            if ranges.len() > MAX_ROWS_PER_CELL {
                ranges.truncate(MAX_ROWS_PER_CELL);
                if let Some(last) = ranges.last_mut() {
                    last.end = content.end;
                }
            }
            rows = rows.max(ranges.len());
            cell_lines.push(ranges);
        }
        Self {
            kind: row.kind,
            columns: row.columns,
            aligns: row.aligns.clone(),
            cells: row.cells.clone(),
            cell_lines,
            rows,
        }
    }

    /// Whether `row` would build the same item: the same cells in the same
    /// columns. The rows of a table an edit did not touch keep their items
    /// when this holds, so a keystroke in one cell re-wraps one row.
    pub(crate) fn same_shape(&self, row: &TableRow) -> bool {
        self.kind == row.kind
            && self.columns == row.columns
            && self.aligns == row.aligns
            && self.cells == row.cells
    }
}

/// A span the application reported, kept inside the line and on char
/// boundaries; a misreport must not panic the layout.
fn clamp_to_line(line: &str, range: &Range<usize>) -> Range<usize> {
    let mut start = range.start.min(line.len());
    let mut end = range.end.min(line.len()).max(start);
    while start > 0 && !line.is_char_boundary(start) {
        start -= 1;
    }
    while end < line.len() && !line.is_char_boundary(end) {
        end += 1;
    }
    start..end
}

/// Test doubles shared by the engine's table tests: a cell splitter and a
/// wrapper that reason in characters rather than glyphs.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// Cells of one table line: the text between pipes, trimmed, an empty
    /// cell mid-padding, padded or cut to `columns` when given.
    pub(crate) fn cells_of(line: &str, columns: Option<usize>) -> Vec<TableCellSpan> {
        let mut cells = Vec::new();
        let mut start = usize::from(line.starts_with('|'));
        for (ix, ch) in line.char_indices().skip(start) {
            if ch == '|' {
                let span = &line[start..ix];
                let lead = span.len() - span.trim_start().len();
                let trail = span.trim_end().len();
                let lead = if trail == 0 { span.len() / 2 } else { lead };
                cells.push(TableCellSpan {
                    start,
                    content: start + lead..start + trail.max(lead),
                    separator: ix,
                });
                start = ix + 1;
            }
        }
        if start < line.len() && !line[start..].trim().is_empty() {
            cells.push(TableCellSpan {
                start,
                content: start..line.len(),
                separator: line.len(),
            });
        }
        if let Some(columns) = columns {
            while cells.len() < columns {
                cells.push(TableCellSpan {
                    start: line.len(),
                    content: line.len()..line.len(),
                    separator: line.len(),
                });
            }
            cells.truncate(columns);
        }
        cells
    }

    /// Wraps at every `width / 10` bytes, so a test can reason in characters.
    pub(crate) fn wrap_every(text: &str, width: Pixels) -> Vec<gpui::Boundary> {
        let per_row = (f32::from(width) / 10.).floor().max(1.) as usize;
        let mut out = Vec::new();
        let mut ix = per_row;
        while ix < text.len() {
            out.push(gpui::Boundary { ix, next_indent: 0 });
            ix += per_row;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{cells_of, wrap_every};
    use super::*;

    fn row(line: &str, columns: usize) -> TableRow {
        TableRow {
            first_row: 0,
            last_row: 0,
            kind: TableRowKind::Body,
            columns,
            aligns: vec![ColumnAlign::Left; columns],
            cells: cells_of(line, Some(columns)),
        }
    }

    #[test]
    fn the_row_is_as_tall_as_its_tallest_cell() {
        // Two columns across 100px: 50px each, minus 12px padding = 38px, so
        // three characters per wrap row.
        let line = "| abcdefg | x |";
        let item = TableRowItem::build(&row(line, 2), line, px(100.), &mut wrap_every);
        assert_eq!(item.rows, 3, "seven characters at three per row");
        assert_eq!(
            item.cell_lines[0].as_slice(),
            &[2..5, 5..8, 8..9],
            "the first cell's rows, relative to the line"
        );
        assert_eq!(item.cell_lines[1].as_slice(), &[12..13]);
    }

    #[test]
    fn an_empty_cell_still_has_one_row_in_the_middle_of_its_padding() {
        let line = "|  | b |";
        let item = TableRowItem::build(&row(line, 2), line, px(200.), &mut wrap_every);
        assert_eq!(item.rows, 1);
        assert_eq!(item.cell_lines[0].as_slice(), &[2..2]);
    }

    #[test]
    fn a_missing_cell_is_padded_at_the_line_end() {
        let line = "| a |";
        let item = TableRowItem::build(&row(line, 3), line, px(300.), &mut wrap_every);
        assert_eq!(item.cell_lines.len(), 3);
        assert_eq!(item.cell_lines[2].as_slice(), &[5..5]);
    }

    #[test]
    fn a_misreported_span_is_clamped_rather_than_trusted() {
        let line = "| a |";
        let mut bad = row(line, 1);
        bad.cells[0].content = 2..40;
        let item = TableRowItem::build(&bad, line, px(300.), &mut wrap_every);
        assert_eq!(item.cell_lines[0].as_slice(), &[2..5]);
    }

    #[test]
    fn the_same_cells_are_the_same_shape_whatever_the_table_s_extent() {
        let line = "| a | b |";
        let mut r = row(line, 2);
        let item = TableRowItem::build(&r, line, px(300.), &mut wrap_every);
        r.first_row = 7;
        r.last_row = 9;
        assert!(item.same_shape(&r));
        r.columns = 3;
        assert!(!item.same_shape(&r));
    }

    #[test]
    fn a_cell_past_the_ceiling_keeps_its_bytes_in_its_last_row() {
        // 40 characters at one per row: 40 rows wanted, 32 kept.
        let text = "x".repeat(40);
        let line = format!("| {text} |");
        let item = TableRowItem::build(&row(&line, 1), &line, px(22.), &mut wrap_every);
        assert_eq!(item.rows, MAX_ROWS_PER_CELL);
        assert_eq!(item.cell_lines[0].len(), MAX_ROWS_PER_CELL);
        assert_eq!(
            item.cell_lines[0].last().unwrap().end,
            2 + 40,
            "the last row runs to the cell's end"
        );
    }

    #[test]
    fn cell_width_is_a_pure_function_of_the_table_s_width_and_columns() {
        assert_eq!(cell_text_width(px(100.), 2), px(38.));
        assert_eq!(cell_text_width(px(10.), 200), px(1.), "never zero");
    }
}
