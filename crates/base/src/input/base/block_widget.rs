//! Block widgets: a grid drawn in place of a run of whole buffer lines.
//!
//! Kept apart from `element.rs` deliberately. Everything here is downstream of
//! upstream's element code rather than part of it, and `element.rs` is the file
//! upstream changes most -- so the smaller our footprint in it, the cheaper
//! every rebase is. The seam is one call from `layout_block_widgets`.

use gpui::{
    App, Bounds, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, SharedString,
    Styled as _, TextAlign, Window, point, px,
};

use gpui::Entity;

use super::layout::LastLayout;
use super::{InputBaseState, InputModeKind};
use crate::input::element::{InlineWidgetLayout, RIGHT_MARGIN};

use std::ops::Range;

/// A block widget and the visible line it starts on, carried from layout to
/// paint so the widget can be placed at that line's origin.
pub(super) struct BlockWidgetPlacement {
    pub(super) line_index: usize,
    /// Visible lines the widget covers, itself included.
    pub(super) line_count: usize,
    /// Rows of the widget that sit above the viewport, so a table scrolled
    /// partly off the top draws from the row it actually resumes at.
    pub(super) rows_skipped: usize,
    pub(super) widget: crate::input::BlockWidget,
}

/// Draws one block widget: a grid standing in for the lines it covers.
///
/// The cells are laid out by the same rules the reading view uses — the header
/// row reads as a label, the body as data, and each column takes the alignment
/// its delimiter row spelled. Nothing here edits: structural changes to a table
/// are text-to-text commands in the application, so the document stays the only
/// source of truth (spec §4 I1).
pub(super) fn render_block_widget(
    widget: &crate::input::BlockWidget,
    line_index: usize,
    rows_shown: Range<usize>,
    row_height: Pixels,
    style: &crate::input::InputEditorStyle,
) -> impl IntoElement {
    let crate::input::BlockWidgetKind::Table {
        header,
        rows,
        aligns,
    } = &widget.kind;

    let align_of = |column: usize| match aligns.get(column).copied().unwrap_or_default() {
        crate::input::ColumnAlign::Left => TextAlign::Left,
        crate::input::ColumnAlign::Center => TextAlign::Center,
        crate::input::ColumnAlign::Right => TextAlign::Right,
    };

    let cell = |one: &crate::input::TableCell, column: usize, is_header: bool| {
        // Only horizontal padding: a row has to be exactly as tall as the
        // buffer line it stands in for, or the grid outgrows the rows reserved
        // for it and the prose after the table is drawn over.
        let container = gpui::div()
            .flex_1()
            .h(row_height)
            .px(px(6.))
            .overflow_hidden()
            .text_align(align_of(column));
        let container = if is_header {
            container.font_weight(gpui::FontWeight::BOLD)
        } else {
            container
        };
        // The application may own an editor for the cell being edited. It is
        // asked by byte range rather than by row and column, so the widget
        // never has to agree with it about how a table is numbered.
        match style
            .table_cell_renderer
            .as_ref()
            .and_then(|render| render(&one.range))
        {
            Some(editor) => container.child(editor),
            None => container.child(SharedString::from(one.text.clone())),
        }
    };

    let row = |cells: &Vec<crate::input::TableCell>, is_header: bool| {
        let mut line = gpui::div().flex().flex_row().w_full().h(row_height);
        for (column, one) in cells.iter().enumerate() {
            line = line.child(cell(one, column, is_header));
        }
        line
    };

    // The table's rows in buffer order. The delimiter row is scaffolding rather
    // than data: it is one of the buffer lines the widget covers, and its row
    // is where the header's rule is drawn.
    let rule = || {
        gpui::div()
            .h(row_height)
            .w_full()
            .flex()
            .items_center()
            .child(gpui::div().h(px(1.)).w_full().bg(style.border))
    };

    // Only the rows the widget was actually given room for. A table scrolled
    // partly off the top is placed on its first *visible* line and sized to the
    // rows that remain, so drawing all of them would spill past the box and
    // over the text below.
    let mut grid = gpui::div()
        .id(("block-table", line_index))
        .flex()
        .flex_col()
        .w_full();
    for ix in rows_shown {
        grid = match ix {
            0 => grid.child(row(header, true).into_any_element()),
            1 => grid.child(rule().into_any_element()),
            _ => match rows.get(ix - 2) {
                Some(cells) => grid.child(row(cells, false).into_any_element()),
                None => grid,
            },
        };
    }
    grid
}

/// Where one cell of a drawn table sits, and which bytes it stands for.
///
/// A click has to become a buffer offset, and the lines under a widget shape
/// to nothing, so the usual path -- resolve the point against a shaped line --
/// puts the caret at the start of a row wherever the reader clicked. These
/// boxes are what let the point be resolved against the grid instead.
#[derive(Debug, Clone)]
pub(crate) struct CellHitbox {
    pub(crate) bounds: gpui::Bounds<Pixels>,
    pub(crate) range: std::ops::Range<usize>,
}

/// Prepaints each block widget over the rows its lines occupy: full text
/// width, starting at the origin of the line it was reported on, as tall as
/// the rows the display map reserved for it.
///
/// Returns the prepainted elements and where each cell of each table landed.
pub(super) fn layout_block_widgets<M: InputModeKind>(
    state: &Entity<InputBaseState<M>>,
    placements: &[BlockWidgetPlacement],
    bounds: &Bounds<Pixels>,
    last_layout: &LastLayout,
    window: &mut Window,
    cx: &mut App,
) -> Vec<InlineWidgetLayout> {
    if placements.is_empty() {
        return Vec::new();
    }
    // `bounds` arrives already shifted by the scroll offset — `layout_cursor`
    // adds it in place — so adding it again put every widget a screenful to
    // the right of the text it stands for: drawn off-target, unclickable.
    let (masked, style) = {
        let state = state.read(cx);
        (state.masked, state.editor_style.clone())
    };
    if masked {
        return Vec::new();
    }

    let line_height = last_layout.line_height;
    // Row offsets of every visible line, so a widget can be placed at the
    // top of the line it starts on without re-summing the ones above it.
    let mut offsets = Vec::with_capacity(last_layout.lines.len());
    let mut offset_y = last_layout.visible_top;
    for line in last_layout.lines.iter() {
        offsets.push(offset_y);
        offset_y += line.size(line_height).height;
    }

    let mut out = Vec::new();
    for placement in placements {
        let Some(&top) = offsets.get(placement.line_index) else {
            continue;
        };
        // The widget owns every row from its first line to the last one its
        // range covers.
        let height: Pixels = last_layout
            .lines
            .iter()
            .skip(placement.line_index)
            .take(placement.line_count)
            .map(|line| line.size(line_height).height)
            .sum();
        let origin = point(
            bounds.origin.x + last_layout.line_number_width,
            bounds.origin.y + top,
        );
        // Stop short of the right edge by the same margin the text keeps:
        // the scrollbar is drawn over that strip, and a widget running the
        // full width would sit underneath it.
        let width = (bounds.size.width - last_layout.line_number_width - RIGHT_MARGIN).max(px(0.));
        let rows_shown = placement.rows_skipped..placement.rows_skipped + placement.line_count;
        let mut element = render_block_widget(
            &placement.widget,
            placement.line_index,
            rows_shown,
            line_height,
            &style,
        )
        .into_any_element();
        element.prepaint_as_root(origin, gpui::size(width, height).into(), window, cx);
        out.push(InlineWidgetLayout { element });
    }
    out
}

/// Where each cell of `widget` landed inside the box it was drawn in.
///
/// The grid lays its columns out with `flex_1`, so every column of a row is the
/// same width -- that is what makes this computable without asking the layout
/// engine back. If the grid ever stops sharing width equally, this has to learn
/// how it does instead, or clicks will land in the wrong column.
fn cell_hitboxes(
    widget: &crate::input::BlockWidget,
    origin: gpui::Point<Pixels>,
    width: Pixels,
    height: Pixels,
    row_height: Pixels,
    rows_skipped: usize,
    rows_shown: usize,
) -> Vec<CellHitbox> {
    let crate::input::BlockWidgetKind::Table { header, rows, .. } = &widget.kind;

    let columns = header
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0));
    if columns == 0 {
        return Vec::new();
    }
    let column_width = width / columns as f32;

    let mut out = Vec::new();
    for row_ix in rows_skipped..rows_skipped + rows_shown {
        // Row 0 is the header, row 1 the rule under it, and the body follows.
        let cells = match row_ix {
            0 => header,
            1 => continue,
            n => match rows.get(n - 2) {
                Some(cells) => cells,
                None => continue,
            },
        };
        let top = origin.y + row_height * (row_ix - rows_skipped) as f32;
        if top + row_height > origin.y + height {
            break;
        }
        for (column, cell) in cells.iter().enumerate() {
            let left = origin.x + column_width * column as f32;
            out.push(CellHitbox {
                bounds: gpui::Bounds::new(
                    point(left, top),
                    gpui::size(column_width, row_height).into(),
                ),
                range: cell.range.clone(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{BlockWidget, BlockWidgetKind, ColumnAlign, TableCell};

    fn cell(text: &str, range: std::ops::Range<usize>) -> TableCell {
        TableCell {
            text: text.to_owned(),
            range,
        }
    }

    /// A click inside a drawn table has to name the cell it landed in. The
    /// lines under the widget shape to nothing, so without these boxes every
    /// click resolved to the start of a row -- a table could be read but never
    /// aimed at.
    #[test]
    fn every_drawn_cell_gets_a_box_holding_its_bytes() {
        let widget = BlockWidget {
            range: 0..40,
            kind: BlockWidgetKind::Table {
                header: vec![cell("a", 2..3), cell("b", 6..7)],
                rows: vec![vec![cell("1", 26..27), cell("2", 30..31)]],
                aligns: vec![ColumnAlign::Left, ColumnAlign::Left],
            },
        };

        let boxes = cell_hitboxes(
            &widget,
            point(px(0.), px(0.)),
            px(200.),
            px(60.),
            px(20.),
            0,
            3,
        );

        // Header row and one body row; the rule between them owns no cell.
        assert_eq!(boxes.len(), 4, "two cells in each of the two data rows");

        // The columns share the width, and each box holds its own bytes.
        assert_eq!(boxes[0].range, 2..3);
        assert_eq!(boxes[0].bounds.origin.x, px(0.));
        assert_eq!(boxes[1].range, 6..7);
        assert_eq!(boxes[1].bounds.origin.x, px(100.));

        // The body row sits below the header and the rule.
        assert_eq!(boxes[2].range, 26..27);
        assert_eq!(boxes[2].bounds.origin.y, px(40.));
    }

    /// A table scrolled partly off the top draws from the row it resumes at,
    /// and the boxes have to follow -- or a click would name a cell that is
    /// not where the reader sees it.
    #[test]
    fn boxes_follow_a_table_scrolled_off_the_top() {
        let widget = BlockWidget {
            range: 0..40,
            kind: BlockWidgetKind::Table {
                header: vec![cell("a", 2..3)],
                rows: vec![vec![cell("1", 26..27)]],
                aligns: vec![ColumnAlign::Left],
            },
        };

        // Only the body row is on screen: rows 0 and 1 scrolled away.
        let boxes = cell_hitboxes(
            &widget,
            point(px(0.), px(0.)),
            px(100.),
            px(20.),
            px(20.),
            2,
            1,
        );
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].range, 26..27, "the body cell, not the header");
        assert_eq!(
            boxes[0].bounds.origin.y,
            px(0.),
            "drawn at the top of the box"
        );
    }
}
