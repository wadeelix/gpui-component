use crate::input::InputModeKind;
use gpui::{Context, Pixels, Point, Window};

use crate::input::{
    InputBaseState, MoveDown, MoveEnd, MoveHome, MoveLeft, MovePageDown, MovePageUp, MoveRight,
    MoveToEnd, MoveToNextWord, MoveToPreviousWord, MoveToStart, MoveUp, RopeExt as _,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MoveDirection {
    Up,
    Down,
}

impl<M: InputModeKind> InputBaseState<M> {
    /// Called after moving the cursor. Updates preferred_column if we know where the cursor now is.
    pub(super) fn update_preferred_column(&mut self) {
        self.preferred_column = self.preferred_column_at(self.cursor());
    }

    /// The x and column of `offset` on the last frame, if it was drawn.
    fn preferred_column_at(&self, offset: usize) -> Option<(Pixels, usize)> {
        let last_layout = self.last_layout.as_ref()?;
        let point = self.text.offset_to_point(offset);
        let line = last_layout.line(point.row)?;
        let pos = line.position_for_index(point.column, last_layout, false)?;
        Some((pos.x, point.column))
    }

    /// Move the cursor to the given offset.
    ///
    /// The offset is the UTF-8 offset.
    ///
    /// Ensure the offset use self.next_boundary or self.previous_boundary to get the correct offset.
    pub(crate) fn move_to(
        &mut self,
        offset: usize,
        direction: Option<MoveDirection>,
        cx: &mut Context<Self>,
    ) {
        self.undo_manager.break_transaction_coalescing();
        let offset = offset.clamp(0, self.text.len());
        self.cursor_line_end_affinity = false;
        self.selected_range = (offset..offset).into();
        self.scroll_to(offset, direction, cx);
        self.pause_blink_cursor(cx);
        self.update_preferred_column();
        M::hide_context_menu(self, cx);
        M::clear_inline_completion(self, cx);
        cx.notify()
    }

    /// The offset `move_lines` rows from `offset`, at the preferred x when
    /// there is one: what Up, Down and their selecting forms land on.
    ///
    /// A table row is one wrap row of several text rows: inside a cell the
    /// caret moves by text row, and leaves the table row only from the
    /// cell's first or last one (Word). Otherwise it moves by display row;
    /// Up into a table row lands on its last text row, as it would on the
    /// last wrap row of prose.
    fn vertical_offset(
        &self,
        offset: usize,
        move_lines: isize,
        preferred: Option<(Pixels, usize)>,
    ) -> Option<usize> {
        let last_layout = self.last_layout.as_ref()?;

        if move_lines.abs() == 1 {
            let point = self.text.offset_to_point(offset);
            let line_start = self.text.line_start_offset(point.row);
            let inside = last_layout.line(point.row).and_then(|line| {
                line.table.as_ref()?.step_text_row(
                    offset.checked_sub(line_start)?,
                    move_lines < 0,
                    preferred.map(|(x, _)| x),
                    self.cursor_line_end_affinity,
                )
            });
            if let Some(local) = inside {
                return Some(line_start + local);
            }
        }

        let mut display_point = self.display_map.offset_to_wrap_display_point(offset);

        // Convert wrap row → display row (skips folded rows), move, then convert back
        let current_display_row = self
            .display_map
            .wrap_row_to_display_row(display_point.row)
            .unwrap_or_else(|| {
                self.display_map
                    .nearest_visible_display_row(display_point.row)
            });
        let max_display_row = self.display_map.display_row_count().saturating_sub(1);
        let mut target_display_row = current_display_row
            .saturating_add_signed(move_lines)
            .min(max_display_row);
        // A collapsed table row (a delimiter row under the header's rule) is
        // no place for the caret: step over it, as the eye does.
        let step = move_lines.signum();
        for _ in 0..2 {
            let wrap_row = self
                .display_map
                .display_row_to_wrap_row(target_display_row)
                .unwrap_or(display_point.row);
            let mut probe = self.display_map.offset_to_wrap_display_point(offset);
            probe.row = wrap_row;
            probe.column = 0;
            let buffer_row = self.display_map.wrap_display_point_to_point(probe).row;
            let collapsed = last_layout
                .line(buffer_row)
                .and_then(|line| line.table.as_ref())
                .is_some_and(|table| table.collapsed());
            let next = target_display_row.saturating_add_signed(step);
            if !collapsed || next > max_display_row || (step < 0 && target_display_row == 0) {
                break;
            }
            target_display_row = next;
        }
        let target_wrap_row = self
            .display_map
            .display_row_to_wrap_row(target_display_row)
            .unwrap_or(display_point.row);

        display_point.row = target_wrap_row;
        display_point.column = 0;
        let mut new_offset = self.display_map.wrap_display_point_to_offset(display_point);

        if let Some((preferred_x, column)) = preferred {
            // Get display point again to update local_row.
            let mut next_display_point = self.display_map.offset_to_wrap_display_point(new_offset);
            next_display_point.column = 0;
            let next_point = self
                .display_map
                .wrap_display_point_to_point(next_display_point);
            let line_start_offset = self.text.line_start_offset(next_point.row);

            // If in visible range, prefer to use position to get column.
            if let Some(line) = last_layout.line(next_point.row) {
                let y = match line.table.as_ref() {
                    Some(table) if move_lines < 0 => {
                        table.size().height - table.text_row_height / 2.
                    }
                    _ => next_display_point.local_row * last_layout.line_height,
                };
                if let Some(x) =
                    line.closest_index_for_position(Point { x: preferred_x, y }, last_layout)
                {
                    new_offset = line_start_offset + x;
                }
            } else {
                // Not in visible range, use column directly.
                let max_line_len = self.text.slice_line(next_point.row).len();
                new_offset = line_start_offset + column.min(max_line_len);
            }
        }
        Some(new_offset)
    }

    /// Move the cursor vertically by one line (up or down) while preserving the column if possible.
    ///
    /// move_lines: Number of lines to move vertically (positive for down, negative for up).
    pub(super) fn move_vertical(
        &mut self,
        move_lines: isize,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_single_line() {
            return;
        }
        let was_preferred_column = self.preferred_column;
        let Some(new_offset) =
            self.vertical_offset(self.cursor(), move_lines, was_preferred_column)
        else {
            return;
        };

        self.pause_blink_cursor(cx);
        let direction = if move_lines < 0 {
            MoveDirection::Up
        } else {
            MoveDirection::Down
        };
        self.move_to(new_offset, Some(direction), cx);
        // Keep the preferred column across repeated presses; without one,
        // the one `move_to` just took from the new position stands.
        if was_preferred_column.is_some() {
            self.preferred_column = was_preferred_column;
        }
        cx.notify();
    }

    /// Extend the selection one row up or down from its head, at the head's
    /// x, the anchor staying: Shift+Up and Shift+Down as every editor has
    /// them. Inside a table cell the head moves by text row.
    pub(super) fn select_vertical(&mut self, move_lines: isize, cx: &mut Context<Self>) {
        if self.is_single_line() {
            return;
        }
        let head = self.cursor();
        let preferred = self
            .preferred_column
            .or_else(|| self.preferred_column_at(head));
        let Some(new_offset) = self.vertical_offset(head, move_lines, preferred) else {
            return;
        };
        self.undo_manager.break_transaction_coalescing();
        self.pause_blink_cursor(cx);
        self.select_to(new_offset, cx);
        self.preferred_column = preferred;
        cx.notify();
    }

    pub(super) fn left(&mut self, _: &MoveLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.pause_blink_cursor(cx);
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor()), None, cx);
        } else {
            self.move_to(self.selected_range.start, None, cx)
        }
    }

    pub(super) fn right(&mut self, _: &MoveRight, _: &mut Window, cx: &mut Context<Self>) {
        self.pause_blink_cursor(cx);
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), None, cx);
        } else {
            self.move_to(self.selected_range.end, None, cx)
        }
    }

    pub(super) fn up(&mut self, action: &MoveUp, window: &mut Window, cx: &mut Context<Self>) {
        if M::handle_context_menu_action(self, Box::new(action.clone()), window, cx) {
            return;
        }

        if self.is_single_line() {
            return;
        }

        if !self.selected_range.is_empty() {
            self.move_to(
                self.previous_boundary(self.selected_range.start.saturating_sub(1)),
                Some(MoveDirection::Up),
                cx,
            );
        }
        self.pause_blink_cursor(cx);
        self.move_vertical(-1, window, cx);
    }

    pub(super) fn down(&mut self, action: &MoveDown, window: &mut Window, cx: &mut Context<Self>) {
        if M::handle_context_menu_action(self, Box::new(action.clone()), window, cx) {
            return;
        }

        if self.is_single_line() {
            return;
        }

        if !self.selected_range.is_empty() {
            self.move_to(
                self.next_boundary(self.selected_range.end.saturating_sub(1)),
                Some(MoveDirection::Down),
                cx,
            );
        }

        self.pause_blink_cursor(cx);
        self.move_vertical(1, window, cx);
    }

    pub(super) fn page_up(&mut self, _: &MovePageUp, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_single_line() {
            return;
        }

        let Some(last_layout) = &self.last_layout else {
            return;
        };

        let display_lines = (self.input_bounds.size.height / last_layout.line_height) as isize;
        self.move_vertical(-display_lines, window, cx);
    }

    pub(super) fn page_down(
        &mut self,
        _: &MovePageDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_single_line() {
            return;
        }

        let Some(last_layout) = &self.last_layout else {
            return;
        };

        let display_lines = (self.input_bounds.size.height / last_layout.line_height) as isize;
        self.move_vertical(display_lines, window, cx);
    }

    pub(super) fn home(&mut self, _: &MoveHome, _: &mut Window, cx: &mut Context<Self>) {
        self.pause_blink_cursor(cx);
        let offset = self.start_of_line();
        self.move_to(offset, Some(MoveDirection::Up), cx);
    }

    pub(super) fn end(&mut self, _: &MoveEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.pause_blink_cursor(cx);
        let offset = self.end_of_line();
        self.move_to(offset, Some(MoveDirection::Down), cx);
        self.cursor_line_end_affinity = true;
    }

    pub(super) fn move_to_start(
        &mut self,
        _: &MoveToStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_to(0, None, cx);
    }

    pub(super) fn move_to_end(&mut self, _: &MoveToEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.text.len(), None, cx);
    }

    pub(super) fn move_to_previous_word(
        &mut self,
        _: &MoveToPreviousWord,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let offset = self.previous_start_of_word();
        self.move_to(offset, None, cx);
    }

    pub(super) fn move_to_next_word(
        &mut self,
        _: &MoveToNextWord,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let offset = self.next_end_of_word();
        self.move_to(offset, None, cx);
    }
}
