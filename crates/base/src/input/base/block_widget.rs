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

    let cell = |text: &String, column: usize, is_header: bool| {
        // A row is exactly as tall as the buffer line it stands in for: the
        // engine reserved that much and no more, so anything taller is drawn
        // over whatever follows the table.
        //
        // `min_w_0` keeps a long cell from growing sideways -- a flex item's
        // minimum size is its content by default, so without it one long cell
        // pushes the columns beside it off the table. It wraps instead, and
        // `overflow_hidden` keeps the wrapped remainder inside the row rather
        // than letting it spill onto the line below.
        //
        // Text can therefore be cut off, which is the honest trade for a grid
        // that cannot choose its own height: the document still holds every
        // character, the reading pane shows them all, and clicking the table
        // drops it back to source. A row that silently overlapped the next one
        // was worse -- it corrupted the display of text that was not part of
        // the table at all.
        let cell = gpui::div()
            .flex_1()
            .min_w_0()
            .h(row_height)
            .overflow_hidden()
            .px(px(6.))
            .text_align(align_of(column));
        let cell = if is_header {
            cell.font_weight(gpui::FontWeight::BOLD)
        } else {
            cell
        };
        cell.child(SharedString::from(text.clone()))
    };

    let row = |cells: &Vec<String>, is_header: bool| {
        let mut line = gpui::div().flex().flex_row().w_full().h(row_height);
        for (column, text) in cells.iter().enumerate() {
            line = line.child(cell(text, column, is_header));
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
        .w_full()
        // The box the engine prepaints this into is exactly the rows it
        // reserved. Clipping to it means a grid that wants more room loses its
        // last rows rather than drawing them over the prose after the table.
        .h_full()
        .overflow_hidden();
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

/// Prepaints each block widget over the rows its lines occupy: full text
/// width, starting at the origin of the line it was reported on, as tall as
/// the rows the display map reserved for it.
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
