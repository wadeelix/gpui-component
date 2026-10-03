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

/// The narrowest a column is laid out, padding included: room for a word of
/// a few letters, so a column of empty cells can still be clicked into.
const MIN_COLUMN: Pixels = px(48.);

/// Header cells are drawn bold, a little wider than the text font measures.
const BOLD_SLACK: f32 = 1.1;

/// Where each of `columns` columns sits across `wrap_width`, as `(x, width)`
/// with padding included, given what each column's widest cell measures.
///
/// A table that fits is as wide as its columns want, not the whole text
/// column: each takes its widest cell plus padding. One that does not fit
/// spans `wrap_width`, a column that wants no more than an equal share keeps
/// what it wants, and the rest share what is left in proportion to their
/// wants. Without a measure for every column the columns are equal, as they
/// always were.
///
/// Decided once, at wrap time, and kept with the row: the wrapper, the layout
/// and the hit test all read the same numbers.
pub(crate) fn column_spans(
    wrap_width: Pixels,
    columns: usize,
    natural: &[Pixels],
) -> Vec<(Pixels, Pixels)> {
    let columns = columns.max(1);
    let widths: Vec<Pixels> = if natural.len() != columns {
        vec![wrap_width / columns as f32; columns]
    } else {
        let want: Vec<Pixels> = natural
            .iter()
            .map(|width| (*width + CELL_PAD * 2.).max(MIN_COLUMN))
            .collect();
        let total = want.iter().fold(px(0.), |sum, w| sum + *w);
        if total <= wrap_width {
            want
        } else {
            share(wrap_width, &want)
        }
    };
    let mut x = px(0.);
    widths
        .into_iter()
        .map(|width| {
            let span = (x, width);
            x += width;
            span
        })
        .collect()
}

/// `width` shared out among columns that want more than it in total: those
/// wanting no more than an equal share of what is left keep their want, the
/// rest divide the remainder in proportion to theirs.
fn share(width: Pixels, want: &[Pixels]) -> Vec<Pixels> {
    let mut out: Vec<Option<Pixels>> = vec![None; want.len()];
    let mut left = width;
    loop {
        let open: Vec<usize> = (0..want.len()).filter(|&c| out[c].is_none()).collect();
        if open.is_empty() {
            break;
        }
        let fair = left / open.len() as f32;
        let small: Vec<usize> = open.iter().copied().filter(|&c| want[c] <= fair).collect();
        if small.is_empty() {
            let wanted = open.iter().fold(px(0.), |sum, &c| sum + want[c]);
            for &c in &open {
                out[c] = Some(left * (want[c] / wanted));
            }
            break;
        }
        for c in small {
            out[c] = Some(want[c]);
            left -= want[c];
        }
    }
    out.into_iter().map(|w| w.unwrap_or(MIN_COLUMN)).collect()
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
    /// Per column, its left edge and width across the text area, padding
    /// included (`column_spans`).
    pub(crate) spans: Vec<(Pixels, Pixels)>,
    /// Per column, the widest cell as the application reported it; kept to
    /// tell whether a row still has the shape it was laid out with.
    widest: Vec<String>,
    /// Per cell, the wrapped byte ranges of its content, relative to the line
    /// start. At least one range per cell, empty for an empty cell.
    pub(crate) cell_lines: Vec<SmallVec<[Range<usize>; 1]>>,
    /// Wrap rows the row needs: the tallest cell's count, at least one.
    pub(crate) rows: usize,
}

impl TableRowItem {
    /// Wraps every cell of `row` at its column's text width with `wrap_line`,
    /// the same closure the wrapper wraps prose with, the columns sized from
    /// the row's `widest` with `measure`, which measures in the text font.
    pub(crate) fn build<F>(
        row: &TableRow,
        line: &str,
        line_start: usize,
        wrap_width: Pixels,
        wrap_line: &mut F,
        measure: &mut dyn FnMut(&str) -> Pixels,
    ) -> Self
    where
        F: FnMut(&str, Pixels, usize) -> Vec<gpui::Boundary>,
    {
        let natural: Vec<Pixels> = row
            .widest
            .iter()
            .map(|text| measure(text) * BOLD_SLACK)
            .collect();
        let spans = column_spans(wrap_width, row.columns, &natural);
        let mut cell_lines = Vec::with_capacity(row.cells.len());
        let mut rows = 1;
        for (c, cell) in row.cells.iter().enumerate() {
            let width = spans
                .get(c)
                .map_or(px(1.), |&(_, width)| (width - CELL_PAD * 2.).max(px(1.)));
            let content = clamp_to_line(line, &cell.content);
            let text = &line[content.clone()];
            let mut ranges: SmallVec<[Range<usize>; 1]> = SmallVec::new();
            // A delimiter row's dashes are as long as the column is wide in
            // the source, and never wrap: collapsed, the row must stay one
            // sliver tall; open, a clipped run of dashes reads fine.
            if row.kind == TableRowKind::Delimiter {
                cell_lines.push(smallvec::smallvec![content]);
                continue;
            }
            let mut prev = 0;
            for boundary in wrap_line(text, width, line_start + content.start) {
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
            spans,
            widest: row.widest.clone(),
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
            && self.widest == row.widest
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
    pub(crate) fn wrap_every(text: &str, width: Pixels, _: usize) -> Vec<gpui::Boundary> {
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
            widest: Vec::new(),
        }
    }

    /// Ten pixels a character, as `wrap_every` wraps.
    fn chars(text: &str) -> Pixels {
        px(text.chars().count() as f32 * 10.)
    }

    #[test]
    fn the_row_is_as_tall_as_its_tallest_cell() {
        // Two columns across 100px: 50px each, minus 12px padding = 38px, so
        // three characters per wrap row.
        let line = "| abcdefg | x |";
        let item = TableRowItem::build(
            &row(line, 2),
            line,
            0,
            px(100.),
            &mut wrap_every,
            &mut chars,
        );
        assert_eq!(item.rows, 3, "seven characters at three per row");
        assert_eq!(
            item.cell_lines[0].as_slice(),
            &[2..5, 5..8, 8..9],
            "the first cell's rows, relative to the line"
        );
        assert_eq!(item.cell_lines[1].as_slice(), &[12..13]);
    }

    #[test]
    fn a_delimiter_row_never_wraps_however_long_its_dashes() {
        let line = "| ------------------------ | -------- |";
        let mut delimiter = row(line, 2);
        delimiter.kind = TableRowKind::Delimiter;
        let item = TableRowItem::build(&delimiter, line, 0, px(100.), &mut wrap_every, &mut chars);
        assert_eq!(item.rows, 1);
        assert_eq!(item.cell_lines[0].len(), 1);
    }

    #[test]
    fn an_empty_cell_still_has_one_row_in_the_middle_of_its_padding() {
        let line = "|  | b |";
        let item = TableRowItem::build(
            &row(line, 2),
            line,
            0,
            px(200.),
            &mut wrap_every,
            &mut chars,
        );
        assert_eq!(item.rows, 1);
        assert_eq!(item.cell_lines[0].as_slice(), &[2..2]);
    }

    #[test]
    fn a_missing_cell_is_padded_at_the_line_end() {
        let line = "| a |";
        let item = TableRowItem::build(
            &row(line, 3),
            line,
            0,
            px(300.),
            &mut wrap_every,
            &mut chars,
        );
        assert_eq!(item.cell_lines.len(), 3);
        assert_eq!(item.cell_lines[2].as_slice(), &[5..5]);
    }

    #[test]
    fn a_misreported_span_is_clamped_rather_than_trusted() {
        let line = "| a |";
        let mut bad = row(line, 1);
        bad.cells[0].content = 2..40;
        let item = TableRowItem::build(&bad, line, 0, px(300.), &mut wrap_every, &mut chars);
        assert_eq!(item.cell_lines[0].as_slice(), &[2..5]);
    }

    #[test]
    fn the_same_cells_are_the_same_shape_whatever_the_table_s_extent() {
        let line = "| a | b |";
        let mut r = row(line, 2);
        let item = TableRowItem::build(&r, line, 0, px(300.), &mut wrap_every, &mut chars);
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
        let item = TableRowItem::build(
            &row(&line, 1),
            &line,
            0,
            px(22.),
            &mut wrap_every,
            &mut chars,
        );
        assert_eq!(item.rows, MAX_ROWS_PER_CELL);
        assert_eq!(item.cell_lines[0].len(), MAX_ROWS_PER_CELL);
        assert_eq!(
            item.cell_lines[0].last().unwrap().end,
            2 + 40,
            "the last row runs to the cell's end"
        );
    }

    #[test]
    fn without_a_measure_for_every_column_the_columns_are_equal() {
        assert_eq!(
            column_spans(px(100.), 2, &[]),
            vec![(px(0.), px(50.)), (px(50.), px(50.))]
        );
        assert_eq!(
            column_spans(px(100.), 2, &[px(10.)]),
            vec![(px(0.), px(50.)), (px(50.), px(50.))],
            "one measure for two columns is a misreport, not a size"
        );
    }

    #[test]
    fn a_table_that_fits_is_as_wide_as_its_columns_want() {
        let spans = column_spans(px(1000.), 2, &[px(20.), px(200.)]);
        assert_eq!(
            spans,
            vec![(px(0.), MIN_COLUMN), (MIN_COLUMN, px(200.) + CELL_PAD * 2.)],
            "a narrow column keeps the minimum, the other its text and padding"
        );
    }

    #[test]
    fn a_table_that_does_not_fit_spans_the_width_and_the_wide_columns_share_it() {
        let spans = column_spans(px(300.), 3, &[px(20.), px(1000.), px(1000.)]);
        let widths: Vec<Pixels> = spans.iter().map(|&(_, w)| w).collect();
        assert_eq!(
            widths[0], MIN_COLUMN,
            "the narrow column keeps what it wants"
        );
        assert_eq!(widths[1], widths[2], "equal wants share equally");
        let total = spans.last().map(|&(x, w)| x + w).unwrap();
        assert!(
            (f32::from(total) - 300.).abs() < 0.01,
            "spans the width: {total:?}"
        );
    }

    #[test]
    fn a_narrow_column_does_not_wrap_because_a_wide_one_is_beside_it() {
        // The equal split this replaces gave the two-digit column half of
        // 300px and wrapped the description at fifteen characters.
        let line = "| 12 | a description of thirty-one |";
        let mut numbers = row(line, 2);
        numbers.widest = vec!["12".into(), "a description of thirty-one".into()];
        let item = TableRowItem::build(&numbers, line, 0, px(300.), &mut wrap_every, &mut chars);
        assert!(item.spans[0].1 < px(100.), "{:?}", item.spans);
        assert_eq!(item.cell_lines[0].len(), 1);
        assert!(
            item.cell_lines[1].len() > 1,
            "the long cell takes the wrapping"
        );
    }

    #[test]
    fn a_row_whose_widest_cells_changed_is_a_new_shape() {
        let line = "| a | b |";
        let mut r = row(line, 2);
        r.widest = vec!["a".into(), "b".into()];
        let item = TableRowItem::build(&r, line, 0, px(300.), &mut wrap_every, &mut chars);
        assert!(item.same_shape(&r));
        r.widest[1] = "bbbb".into();
        assert!(
            !item.same_shape(&r),
            "another row's cell grew, so every row moves"
        );
    }
}
