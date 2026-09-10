//! A text input field that allows the user to enter text.
//!
//! Based on the `Input` example from the `gpui` crate.
//! https://github.com/zed-industries/zed/blob/main/crates/gpui/examples/input.rs
use gpui::TextAlign;
use gpui::{
    Action, App, AppContext, Bounds, ClipboardItem, Context, Edges, Entity, EntityInputHandler,
    EventEmitter, FocusHandle, Focusable, InteractiveElement as _, IntoElement, KeyBinding,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Point,
    Render, ScrollHandle, ScrollWheelEvent, SharedString, Styled as _, Subscription,
    UTF16Selection, Window, actions, div, point, prelude::FluentBuilder as _, px,
};
use ropey::{Rope, RopeSlice};
use serde::Deserialize;
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;
use sum_tree::Bias;
use unicode_segmentation::*;

use super::{
    DiagnosticSet, DisplayMap, InputContextMenuCapabilities, InputEditorStyle,
    InputHighlighterFactory, MASK_CHAR, MaskPattern, NativeMenu, NumberStep, WrappingIndent,
    blink_cursor::BlinkCursor,
    change::Change,
    cursor::{CursorSelection, Selections},
    element::{EditorScrollbar, EditorScrollbarSnapshot, TextElement},
    kind::InputModeKind,
    mask_pattern::normalize_number_input,
    mode::LayoutMode,
    undo_manager::{EditIntent, UndoManager},
};
use crate::actions::{SelectDown, SelectLeft, SelectRight, SelectUp};
use crate::input::blink_cursor::CURSOR_WIDTH;
use crate::input::movement::MoveDirection;
use crate::input::{
    InputExtras as _, LineHeightScale, Position, RopeExt as _, TableRowSource,
    element::RIGHT_MARGIN, layout::LastLayout,
};
use crate::{AutoScroll, StepAction};

/// Vertical clearance to retain when revealing a text position.
pub(crate) enum ScrollPadding {
    Minimal,
    SurroundingLines,
}

#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = input, no_json)]
pub struct Enter {
    /// Is confirm with secondary.
    pub secondary: bool,
    /// Whether the Shift modifier was held when Enter was pressed.
    pub shift: bool,
}

impl Enter {
    /// Returns true if `action` is a primary `Enter` action (`secondary: false`),
    /// regardless of whether Shift was held.
    pub fn is_primary(action: &dyn Action) -> bool {
        action.partial_eq(&Enter {
            secondary: false,
            shift: false,
        }) || action.partial_eq(&Enter {
            secondary: false,
            shift: true,
        })
    }
}

actions!(
    input,
    [
        Backspace,
        Delete,
        DeleteToBeginningOfLine,
        DeleteToEndOfLine,
        DeleteToPreviousWordStart,
        DeleteToNextWordEnd,
        Indent,
        Outdent,
        IndentInline,
        OutdentInline,
        MoveUp,
        MoveDown,
        MoveLeft,
        MoveRight,
        MoveHome,
        MoveEnd,
        MovePageUp,
        MovePageDown,
        AddCursorAbove,
        AddCursorBelow,
        SelectAll,
        SelectToStartOfLine,
        SelectToEndOfLine,
        SelectToStart,
        SelectToEnd,
        SelectToPreviousWordStart,
        SelectToNextWordEnd,
        ShowCharacterPalette,
        Copy,
        Cut,
        Paste,
        Undo,
        Redo,
        MoveToStartOfLine,
        MoveToEndOfLine,
        MoveToStart,
        MoveToEnd,
        MoveToPreviousWord,
        MoveToNextWord,
        Escape,
        ToggleCodeActions,
        Search,
        Replace,
        GoToDefinition,
    ]
);

#[derive(Clone)]
pub enum InputEvent {
    Change,
    PressEnter {
        secondary: bool,
        shift: bool,
    },
    Focus,
    Blur,
    /// A click on an insertion marker of a table: the "+" the pointer found
    /// on a column boundary of the header's top rule, or on a row's bottom
    /// edge at the table's left. `line_start` is that row's first byte;
    /// `column` is the column to insert before (the column count: after the
    /// last), or none to insert a row below that row.
    TableInsert {
        line_start: usize,
        column: Option<usize>,
    },
}

/// The insertion marker under the pointer, if it is near one: drawn as a
/// "+" the writer can click, over a column boundary or under a row.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TableMarker {
    pub(crate) line_start: usize,
    pub(crate) column: Option<usize>,
    /// Where it is drawn, in window coordinates.
    pub(crate) bounds: Bounds<Pixels>,
}

pub(super) const CONTEXT: &str = "Input";

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some(CONTEXT)),
        KeyBinding::new("shift-backspace", Backspace, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("ctrl-backspace", Backspace, Some(CONTEXT)),
        KeyBinding::new("delete", Delete, Some(CONTEXT)),
        KeyBinding::new("shift-delete", Delete, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-backspace", DeleteToBeginningOfLine, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-delete", DeleteToEndOfLine, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("alt-backspace", DeleteToPreviousWordStart, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-backspace", DeleteToPreviousWordStart, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("alt-delete", DeleteToNextWordEnd, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-delete", DeleteToNextWordEnd, Some(CONTEXT)),
        KeyBinding::new(
            "enter",
            Enter {
                secondary: false,
                shift: false,
            },
            Some(CONTEXT),
        ),
        KeyBinding::new(
            "shift-enter",
            Enter {
                secondary: false,
                shift: true,
            },
            Some(CONTEXT),
        ),
        KeyBinding::new(
            "secondary-enter",
            Enter {
                secondary: true,
                shift: false,
            },
            Some(CONTEXT),
        ),
        KeyBinding::new("escape", Escape, Some(CONTEXT)),
        KeyBinding::new("up", MoveUp, Some(CONTEXT)),
        KeyBinding::new("down", MoveDown, Some(CONTEXT)),
        KeyBinding::new("left", MoveLeft, Some(CONTEXT)),
        KeyBinding::new("right", MoveRight, Some(CONTEXT)),
        KeyBinding::new("pageup", MovePageUp, Some(CONTEXT)),
        KeyBinding::new("pagedown", MovePageDown, Some(CONTEXT)),
        KeyBinding::new("tab", IndentInline, Some(CONTEXT)),
        KeyBinding::new("shift-tab", OutdentInline, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-]", Indent, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-]", Indent, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-[", Outdent, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-[", Outdent, Some(CONTEXT)),
        KeyBinding::new("shift-left", SelectLeft, Some(CONTEXT)),
        KeyBinding::new("shift-right", SelectRight, Some(CONTEXT)),
        KeyBinding::new("shift-up", SelectUp, Some(CONTEXT)),
        KeyBinding::new("shift-down", SelectDown, Some(CONTEXT)),
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        KeyBinding::new("shift-alt-left", SelectLeft, Some(CONTEXT)),
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        KeyBinding::new("shift-alt-right", SelectRight, Some(CONTEXT)),
        // Avoid Ctrl+Alt+arrows on Linux, where desktops may reserve them.
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-alt-up", AddCursorAbove, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-alt-down", AddCursorBelow, Some(CONTEXT)),
        #[cfg(target_os = "windows")]
        KeyBinding::new("ctrl-alt-up", AddCursorAbove, Some(CONTEXT)),
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        KeyBinding::new("shift-alt-up", AddCursorAbove, Some(CONTEXT)),
        #[cfg(target_os = "windows")]
        KeyBinding::new("ctrl-alt-down", AddCursorBelow, Some(CONTEXT)),
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        KeyBinding::new("shift-alt-down", AddCursorBelow, Some(CONTEXT)),
        KeyBinding::new("home", MoveHome, Some(CONTEXT)),
        KeyBinding::new("end", MoveEnd, Some(CONTEXT)),
        KeyBinding::new("shift-home", SelectToStartOfLine, Some(CONTEXT)),
        KeyBinding::new("shift-end", SelectToEndOfLine, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("ctrl-shift-a", SelectToStartOfLine, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("ctrl-shift-e", SelectToEndOfLine, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("shift-cmd-left", SelectToStartOfLine, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("shift-cmd-right", SelectToEndOfLine, Some(CONTEXT)),
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        KeyBinding::new("alt-shift-left", SelectToPreviousWordStart, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-left", SelectToPreviousWordStart, Some(CONTEXT)),
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        KeyBinding::new("alt-shift-right", SelectToNextWordEnd, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-right", SelectToNextWordEnd, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-a", SelectAll, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-a", SelectAll, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-c", Copy, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-c", Copy, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-x", Cut, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-x", Cut, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-v", Paste, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-v", Paste, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("ctrl-a", MoveHome, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-left", MoveHome, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("ctrl-e", MoveEnd, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-right", MoveEnd, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-z", Undo, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-z", Redo, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-up", MoveToStart, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-down", MoveToEnd, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("alt-left", MoveToPreviousWord, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("alt-right", MoveToNextWord, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-left", MoveToPreviousWord, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-right", MoveToNextWord, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-up", SelectToStart, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-down", SelectToEnd, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-z", Undo, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-y", Redo, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-.", ToggleCodeActions, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-.", ToggleCodeActions, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-f", Search, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-f", Search, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-f", Replace, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-h", Replace, Some(CONTEXT)),
    ]);
}

/// The shared text-editing engine behind [`crate::input::InputState`],
/// [`crate::input::TextareaState`] and [`crate::input::EditorState`].
///
/// `M` is the mode marker: it carries no data and only decides which methods
/// exist, so an ordinary input cannot reach the editor's language features.
///
/// The three states are type aliases of this one, which is why this name is
/// public: an alias is only as usable as the type behind it, so hiding this
/// would leave `InputState` unable to do anything. Prefer naming the aliases
/// — write `InputState`, not `InputBaseState<InputMode>`.
pub struct InputBaseState<M: InputModeKind> {
    /// State only this mode needs. See [`InputModeKind::Extras`].
    pub(crate) extras: M::Extras,
    pub(super) focus_handle: FocusHandle,
    pub(super) mode: LayoutMode,
    pub(super) text: Rope,
    pub(super) display_map: DisplayMap,
    pub(super) undo_manager: UndoManager,
    pub(super) search_session: super::SearchSession,
    /// Advances every time search is explicitly invoked. See
    /// [`InputBaseState::search_activation_revision`].
    pub(super) search_activation_revision: u64,
    pub(super) searchable: bool,
    pub(super) replaceable: bool,
    pub(super) soft_wrap: bool,
    pub(super) wrapping_indent: WrappingIndent,
    pub(super) scroll_beyond_last_line: Option<usize>,
    pub(super) cursor_surrounding_lines: Option<usize>,
    pub(super) blink_cursor: Entity<BlinkCursor>,
    pub(super) loading: bool,
    /// The cursors and selections.
    ///
    /// Always contains at least one selection where index 0 is the active cursor.
    pub(super) selections: Selections,
    /// Range for save the selected word, use to keep word range when drag move.
    pub(super) selected_word_range: Option<CursorSelection>,
    /// The marked range is the temporary insert text on IME typing.
    pub(super) ime_marked_range: Option<CursorSelection>,
    pub(super) last_layout: Option<LastLayout>,
    pub(super) last_cursor: Option<usize>,
    /// The input container bounds
    pub(super) input_bounds: Bounds<Pixels>,
    /// The text bounds
    pub(super) last_bounds: Option<Bounds<Pixels>>,
    pub(super) last_selected_range: Option<CursorSelection>,
    pub(super) selecting: bool,
    /// Anchor offset for an in-progress columnar (block) selection.
    pub(super) column_select_start: Option<usize>,
    pub(crate) disabled: bool,
    pub(crate) readonly: bool,
    pub(crate) text_align: TextAlign,
    pub(super) masked: bool,
    pub(super) clean_on_escape: bool,
    pub(super) submit_on_enter: bool,
    pub(super) show_whitespaces: bool,
    /// This flag tells the renderer to prefer the end of the current visual line.
    pub(crate) cursor_line_end_affinity: bool,
    pub(super) pattern: Option<regex::Regex>,
    pub(super) validate: Option<Box<dyn Fn(&str, &mut App) -> bool + 'static>>,
    /// The step strategy for [`super::NumberInput`] to increment/decrement.
    /// See [`Self::step`] and [`Self::step_by`].
    pub(crate) number_step: Option<NumberStep>,
    /// The minimum value for [`super::NumberInput`]. See [`Self::min`].
    pub(crate) number_min: Option<f64>,
    /// The maximum value for [`super::NumberInput`]. See [`Self::max`].
    pub(crate) number_max: Option<f64>,
    pub(crate) scroll_handle: ScrollHandle,
    /// The deferred scroll offset to apply on next layout.
    pub(crate) deferred_scroll_offset: Option<Point<Pixels>>,
    /// The size of the scrollable content.
    pub(crate) scroll_size: gpui::Size<Pixels>,
    pub(super) editor_scrollbar_snapshot: Cell<Option<EditorScrollbarSnapshot>>,
    /// Screen rectangles of the widgets drawn on the last frame. A click inside
    /// one belongs to that widget: the editor's own mouse handler sits on the
    /// container and would otherwise take every click before the widget could,
    /// leaving a checkbox that draws correctly but never toggles.
    pub(super) widget_hitboxes: RefCell<Vec<Bounds<Pixels>>>,
    /// Draws the blocks a highlighter asks for (ADR-0009).
    pub(crate) block_renderer: Option<crate::input::BlockRenderer>,
    /// The table insertion marker the pointer is near, if any.
    pub(crate) table_marker: Option<TableMarker>,
    /// Whether tables offer insertion markers under the pointer at all.
    pub(super) table_handles: bool,
    pub(super) editor_paddings: Edges<Pixels>,
    /// The style this state paints with: what was projected onto it, with
    /// every colour left unset resolved from the palette that is current. It
    /// is rebuilt at the top of every render, which is what keeps it current
    /// when the palette changes after the state was built.
    pub(super) editor_style: InputEditorStyle,
    /// What a consumer projected, kept verbatim so that resolution never
    /// consumes its own output: resolving in place would fill the unset
    /// colours once and then never see them as unset again, which is the same
    /// freeze in a different place.
    projected_editor_style: InputEditorStyle,

    /// The mask pattern for formatting the input text
    pub(crate) mask_pattern: MaskPattern,
    /// Whether the `mask_pattern` was explicitly set (via [`Self::mask_pattern`]
    /// or [`Self::set_mask_pattern`]), to let [`super::NumberInput`] only apply
    /// its default mask when the user has not made an explicit choice.
    pub(super) mask_pattern_set: bool,
    pub(super) placeholder: SharedString,

    /// Diagnostic currently requested by pointer hover; applications render it.
    pub(super) diagnostic_popover: Option<Rc<crate::input::DiagnosticEntry>>,

    context_menu_handler: Option<
        Rc<dyn Fn(NativeMenu, InputContextMenuCapabilities, Point<Pixels>, &mut Window, &mut App)>,
    >,
    pending_context_menu: Option<(Point<Pixels>, usize)>,

    /// Whether the context menu that shows on right-click is enabled.
    ///
    pub(super) enable_context_menu: bool,

    /// A flag to indicate if we are currently inserting a completion item.
    pub(super) completion_inserting: bool,
    pub(super) overlay_action_handler: Option<
        Rc<
            dyn Fn(
                super::InputOverlayKind,
                Box<dyn Action>,
                &mut Window,
                &mut Context<InputBaseState<M>>,
            ) -> bool,
        >,
    >,

    /// A flag to indicate if we have a pending update to the text.
    ///
    /// If true, will call some update (for example LSP, Syntax Highlight) before render.
    _pending_update: bool,
    /// A flag to indicate if we should ignore the next completion event.
    pub(super) silent_replace_text: bool,
    /// A flag to indicate if we should emit InputEvents.
    pub(super) emit_events: bool,

    _subscriptions: Vec<Subscription>,

    pub(super) auto_scroll: AutoScroll,
}

/// Read-only styling data exposed to presentation facades.
///
/// The fields are private and read through the methods below, so that a new
/// one can be added without breaking the facades.
#[derive(Clone)]
pub struct InputPresentation {
    focus_handle: FocusHandle,
    disabled: bool,
    readonly: bool,
    loading: bool,
    masked: bool,
    multi_line: bool,
    code_editor: bool,
    text_align: TextAlign,
    placeholder: SharedString,
    mask_placeholder: Option<String>,
}

impl InputPresentation {
    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled
    }

    pub fn is_readonly(&self) -> bool {
        self.readonly
    }

    /// Returns true if the user is allowed to change the text.
    ///
    /// See also: [`InputBaseState::is_editable`].
    pub fn is_editable(&self) -> bool {
        !self.disabled && !self.readonly
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    pub fn is_masked(&self) -> bool {
        self.masked
    }

    pub fn is_multi_line(&self) -> bool {
        self.multi_line
    }

    pub fn is_code_editor(&self) -> bool {
        self.code_editor
    }

    pub fn text_align(&self) -> TextAlign {
        self.text_align
    }

    pub fn placeholder(&self) -> &SharedString {
        &self.placeholder
    }

    /// The placeholder derived from the mask pattern, e.g.: `(___) ___-____`.
    pub fn mask_placeholder(&self) -> Option<&str> {
        self.mask_placeholder.as_deref()
    }
}

impl<M: InputModeKind> EventEmitter<InputEvent> for InputBaseState<M> {}

impl<M: InputModeKind> InputBaseState<M> {
    #[doc(hidden)]
    pub fn cursor_layout(&self) -> Option<(Bounds<Pixels>, Pixels)> {
        let layout = self.last_layout.as_ref()?;
        Some((layout.cursor_bounds?, layout.line_height))
    }

    pub fn input_bounds(&self) -> Bounds<Pixels> {
        self.input_bounds
    }

    pub fn text_bounds(&self) -> Option<Bounds<Pixels>> {
        self.last_bounds
    }

    pub fn diagnostic_popover(&self) -> Option<Rc<crate::input::DiagnosticEntry>> {
        self.diagnostic_popover.clone()
    }

    pub fn presentation(&self) -> InputPresentation {
        InputPresentation {
            focus_handle: self.focus_handle.clone(),
            disabled: self.disabled,
            readonly: self.readonly,
            loading: self.loading,
            masked: self.masked,
            multi_line: self.is_multi_line(),
            code_editor: self.is_code_editor(),
            text_align: self.text_align,
            placeholder: self.placeholder.clone(),
            mask_placeholder: self.mask_pattern.placeholder(),
        }
    }

    /// Whether this input spans more than one line.
    ///
    /// Answered by the mode marker, which is fixed when the state is built.
    /// [`LayoutMode`] holds the row counts and growth policy, not the kind.
    #[inline]
    /// Whether this input paints scrollbars.
    ///
    /// Only a multi-line input can scroll: a single-line input keeps its
    /// caret in view by moving its own offset, and never has a viewport a
    /// user could drag. Adding the editor scrollbar to every input put a
    /// thumb inside every text field, which is a control the field does not
    /// have.
    pub(crate) fn shows_scrollbar(&self) -> bool {
        self.is_multi_line()
    }

    pub fn is_multi_line(&self) -> bool {
        M::MULTI_LINE
    }

    /// Whether this input is a single-line text field. See [`Self::is_multi_line`].
    #[inline]
    pub fn is_single_line(&self) -> bool {
        !M::MULTI_LINE
    }

    /// Whether this input is a source-code editor.
    #[inline]
    pub fn is_code_editor(&self) -> bool {
        M::CODE_EDITOR
    }

    /// Whether the user is allowed to copy the selection out.
    ///
    /// A masked input keeps its value out of the clipboard.
    pub fn is_copyable(&self) -> bool {
        self.selections.iter().any(|sel| !sel.is_empty()) && !self.masked
    }

    pub fn context_menu_capabilities(&self) -> InputContextMenuCapabilities {
        let (go_to_definition, code_actions) = self.extras.context_menu_capabilities();
        InputContextMenuCapabilities::new()
            .disabled(self.disabled)
            .readonly(self.readonly)
            .code_editor(self.is_code_editor())
            .selection(!self.active_selection().is_empty())
            .masked(self.masked)
            .go_to_definition(go_to_definition)
            .code_actions(code_actions)
    }

    pub fn set_text_align(&mut self, text_align: TextAlign, cx: &mut Context<Self>) {
        if !self.is_single_line() || self.text_align == text_align {
            return;
        }

        self.text_align = text_align;
        cx.notify();
    }

    /// Flip the password mask.
    ///
    /// Setting the mask is a single-line method, but flipping it stays here:
    /// the reveal button is rendered from the generic path, and it can only be
    /// switched on through [`crate::input::InputState`] anyway.
    pub fn toggle_masked(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.masked = !self.masked;
        cx.notify();
    }

    pub fn on_context_menu(
        &mut self,
        handler: Rc<
            dyn Fn(NativeMenu, InputContextMenuCapabilities, Point<Pixels>, &mut Window, &mut App),
        >,
    ) {
        self.context_menu_handler = Some(handler);
    }

    /// Build the engine. Each mode's own `new` sets its layout on top of this.
    fn new_in_mode(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle().tab_stop(true);
        let blink_cursor = cx.new(|_| BlinkCursor::new());
        let undo_manager = UndoManager::new();

        let _subscriptions = vec![
            // Key bindings can consume events before on_key_down. Observe input
            // before action dispatch so every keystroke resets the blink delay.
            cx.intercept_keystrokes({
                let focus_handle = focus_handle.clone();
                let blink_cursor = blink_cursor.downgrade();
                move |_, window, cx| {
                    if focus_handle.is_focused(window) {
                        _ = blink_cursor.update(cx, |cursor, cx| cursor.pause(cx));
                    }
                }
            }),
            // Observe the blink cursor to repaint the view when it changes.
            cx.observe(&blink_cursor, |_, _, cx| cx.notify()),
            // Blink the cursor when the window is active, pause when it's not.
            cx.observe_window_activation(window, |input, window, cx| {
                if window.is_window_active() {
                    let focus_handle = input.focus_handle.clone();
                    if focus_handle.is_focused(window) {
                        input.blink_cursor.update(cx, |blink_cursor, cx| {
                            blink_cursor.start(cx);
                        });
                    }
                }
            }),
            cx.on_focus(&focus_handle, window, Self::on_focus),
            cx.on_blur(&focus_handle, window, Self::on_blur),
        ];

        let text_style = window.text_style();

        Self {
            extras: M::Extras::default(),
            focus_handle: focus_handle.clone(),
            text: "".into(),
            display_map: DisplayMap::new(text_style.font(), window.rem_size(), None),
            search_session: super::SearchSession::default(),
            search_activation_revision: 0,
            searchable: false,
            replaceable: true,
            soft_wrap: true,
            wrapping_indent: WrappingIndent::default(),
            scroll_beyond_last_line: None,
            cursor_surrounding_lines: None,
            blink_cursor,
            undo_manager,
            selections: Selections::default(),
            selected_word_range: None,
            ime_marked_range: None,
            input_bounds: Bounds::default(),
            selecting: false,
            disabled: false,
            readonly: false,
            text_align: TextAlign::Left,
            masked: false,
            clean_on_escape: false,
            submit_on_enter: false,
            show_whitespaces: false,
            loading: false,
            pattern: None,
            validate: None,
            number_step: Some(NumberStep::Fixed(1.)),
            number_min: None,
            number_max: None,
            mode: LayoutMode::default(),
            last_layout: None,
            last_bounds: None,
            last_selected_range: None,
            column_select_start: None,
            last_cursor: None,
            scroll_handle: ScrollHandle::new(),
            scroll_size: gpui::size(px(0.), px(0.)),
            editor_scrollbar_snapshot: Cell::new(None),
            widget_hitboxes: RefCell::new(Vec::new()),
            block_renderer: None,
            table_marker: None,
            table_handles: true,
            editor_paddings: Edges::default(),
            deferred_scroll_offset: None,
            placeholder: SharedString::default(),
            mask_pattern: MaskPattern::default(),
            mask_pattern_set: false,
            editor_style: InputEditorStyle::default(),
            projected_editor_style: InputEditorStyle::default(),
            diagnostic_popover: None,
            context_menu_handler: None,
            pending_context_menu: None,
            enable_context_menu: true,
            completion_inserting: false,
            overlay_action_handler: None,
            silent_replace_text: false,
            emit_events: true,
            _subscriptions,
            _pending_update: false,
            cursor_line_end_affinity: false,
            auto_scroll: AutoScroll::default(),
        }
    }

    /// Sets whether the context menu that shows on right-click is enabled.
    ///
    /// The context menu is enabled by default.
    /// This value is ignored if a custom context menu builder is defined on the input.
    pub fn context_menu(mut self, enable: bool) -> Self {
        self.enable_context_menu = enable;
        self
    }

    pub fn set_context_menu_enabled(&mut self, enabled: bool) {
        self.enable_context_menu = enabled;
    }

    /// Set whether search UI allows replacement, default is true.
    #[doc(hidden)]
    pub fn replaceable(mut self, allow: bool) -> Self {
        self.replaceable = allow;
        self
    }

    /// Set placeholder
    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// Set highlighter language for for [`LayoutMode::CodeEditor`] mode.
    pub fn set_highlighter(
        &mut self,
        new_language: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        match &mut self.mode {
            LayoutMode::CodeEditor {
                language,
                highlighter,
                ..
            } => {
                *language = new_language.into();
                *highlighter.borrow_mut() = None;
            }
            _ => {}
        }
        cx.notify();
    }

    fn reset_highlighter(&mut self, cx: &mut Context<Self>) {
        match &mut self.mode {
            LayoutMode::CodeEditor { highlighter, .. } => {
                *highlighter.borrow_mut() = None;
            }
            _ => {}
        }
        cx.notify();
    }

    /// Install the parser/highlighter adapter used by code-editor mode.
    pub fn set_highlighter_factory(
        &mut self,
        factory: InputHighlighterFactory,
        cx: &mut Context<Self>,
    ) {
        self.mode.set_highlighter_factory(factory);
        self.sync_height_scale(cx);
        self._pending_update = true;
        cx.notify();
    }

    /// Recomputes every line's height from the highlighter.
    ///
    /// Heights are cached in the display map — that is what makes scrolling
    /// cheap — and are only recomputed when the text changes. A highlighter
    /// whose scale depends on something else (a mode toggle, a setting, a
    /// selection) must therefore say when that changed; one that depends only
    /// on the line's own text never needs this.
    ///
    /// Rebuilds every row, so it belongs on a state change, not a frame.
    pub fn refresh_line_heights(&mut self, cx: &mut Context<Self>) {
        self.sync_height_scale(cx);
        cx.notify();
    }

    /// Points the display map at the highlighter's per-line font scale, so a
    /// line the application draws larger also *occupies* more room.
    ///
    /// The highlighter is shared rather than copied: it is replaced in place
    /// when the language changes, and a closure holding a snapshot would keep
    /// scaling by the old one.
    fn sync_height_scale(&mut self, cx: &mut Context<Self>) {
        let Some(highlighter) = self.mode.highlighter().cloned() else {
            return;
        };
        // A line's height is its font scale times whatever extra room it asks
        // for. The two are separate because a heading grows its *text*, while
        // extra room leaves the text under it its own size.
        let scale: LineHeightScale = {
            let highlighter = highlighter.clone();
            Rc::new(move |range: &Range<usize>, text: &Rope, generation: u64| {
                highlighter
                    .borrow()
                    .as_ref()
                    .map(|highlighter| {
                        highlighter.line_font_scale(range)
                            * highlighter.line_height_scale(range, text, generation)
                    })
                    .unwrap_or(1.0)
            })
        };
        // Which lines are table rows, decided by the same highlighter at the
        // same moment: at wrap time, from the text as it will be.
        let table_rows: TableRowSource = Rc::new(move |range: &Range<usize>, text, generation| {
            highlighter
                .borrow()
                .as_ref()
                .and_then(|highlighter| highlighter.table_row(range, text, generation))
        });
        self.display_map
            .set_line_hooks(Some(scale), Some(table_rows), cx);
    }

    /// The window rectangle of column `column` of the table row holding
    /// `offset`, as laid out on the last frame, and the document range of
    /// that cell's content. For tests and the debug channel: the geometry a
    /// click is tested against, made readable from outside the engine.
    #[doc(hidden)]
    pub fn table_cell(
        &self,
        offset: usize,
        column: usize,
    ) -> Option<(Bounds<Pixels>, Range<usize>)> {
        let layout = self.last_layout.as_ref()?;
        let bounds = self.last_bounds?;
        let row = self.text.offset_to_point(offset.min(self.text.len())).row;
        let vi = layout.visible_buffer_lines.iter().position(|&b| b == row)?;
        let line = layout.lines.get(vi)?;
        let table = line.table.as_ref()?;
        let geometry = table.columns.get(column)?;
        let cell = table.cells.get(column)?;
        let line_start = layout.visible_line_byte_offsets[vi];
        let origin = bounds.origin
            + point(
                layout.line_number_width + geometry.x,
                layout.line_height * self.display_map.buffer_line_top(row),
            );
        Some((
            Bounds::new(origin, gpui::size(geometry.width, table.size().height)),
            line_start + cell.content.start..line_start + cell.content.end,
        ))
    }

    /// Where Up or Down would put the caret inside its table cell, if the
    /// cell has a text row there; none at the cell's first or last row, where
    /// the move leaves the table row. For an application that steps between
    /// rows itself and must not do so while the caret can still move inside
    /// the cell.
    pub fn table_step(&self, offset: usize, up: bool) -> Option<usize> {
        let layout = self.last_layout.as_ref()?;
        let row = self.text.offset_to_point(offset.min(self.text.len())).row;
        let table = layout.line(row)?.table.as_ref()?;
        let line_start = self.text.line_start_offset(row);
        let local = table.step_text_row(
            offset.checked_sub(line_start)?,
            up,
            self.active_selection().column_anchor.map(|(x, _)| x),
            self.cursor_line_end_affinity,
        )?;
        Some(line_start + local)
    }

    /// Re-lays out the lines covering `range` from the current text.
    ///
    /// For a change that is not an edit but changes how a line is laid out:
    /// a table that starts or stops being laid out as one because the caret
    /// moved, for instance. Bounded to `range`, unlike
    /// [`Self::refresh_line_heights`].
    /// Registers what draws the blocks a highlighter asks for (ADR-0009).
    pub fn set_block_renderer(
        &mut self,
        renderer: Option<crate::input::BlockRenderer>,
        cx: &mut Context<Self>,
    ) {
        self.block_renderer = renderer;
        cx.notify();
    }

    /// Re-lays out the lines covering `range` after something other than an
    /// edit changed their height -- a block that measured itself -- and keeps
    /// the view still: when they lie above the first visible line, the scroll
    /// offset moves by the height they gained or lost (ADR-0009).
    pub fn rewrap_lines_anchored(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        let anchor = self
            .last_layout
            .as_ref()
            .and_then(|layout| Some((*layout.visible_buffer_lines.first()?, layout.line_height)));
        let before = anchor.map(|(line, _)| self.display_map.buffer_line_top(line));
        self.display_map.rewrap(range, cx);
        if let (Some((line, line_height)), Some(before)) = (anchor, before) {
            let moved = self.display_map.buffer_line_top(line) - before;
            if moved != 0.0 {
                let mut offset = self.scroll_handle.offset();
                offset.y -= line_height * moved;
                self.scroll_handle.set_offset(offset);
            }
        }
        cx.notify();
    }

    pub fn rewrap_lines(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        self.display_map.rewrap(range, cx);
        // Rows may have moved under a marker placed before; it comes back
        // where the pointer is on the next move.
        self.table_marker = None;
        cx.notify();
    }

    /// Install a default adapter without replacing an application-provided one.
    pub fn ensure_highlighter_factory(&mut self, factory: InputHighlighterFactory) {
        self.mode.ensure_highlighter_factory(factory);
    }

    pub fn set_editor_style(&mut self, style: InputEditorStyle) {
        self.editor_style = style.clone();
        self.projected_editor_style = style;
    }

    /// Set presentation padding for multi-line text and its scrollbar layout.
    #[doc(hidden)]
    pub fn set_editor_paddings(&mut self, paddings: Edges<Pixels>) {
        self.editor_paddings = paddings;
    }

    pub fn apply_highlighter_fold_candidates(
        &mut self,
        candidates: Vec<crate::input::FoldRange>,
        cx: &mut Context<Self>,
    ) {
        if self.mode.is_folding() {
            self.display_map.set_fold_candidates(candidates);
        }
        cx.notify();
    }

    #[inline]
    pub fn diagnostics(&self) -> Option<&DiagnosticSet> {
        self.mode.diagnostics()
    }

    #[inline]
    pub fn diagnostics_mut(&mut self) -> Option<&mut DiagnosticSet> {
        self.mode.diagnostics_mut()
    }

    /// Set placeholder
    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.placeholder = placeholder.into();
        cx.notify();
    }

    /// Find which line and sub-line the given offset belongs to, along with the position within that sub-line.
    ///
    /// Returns:
    ///
    /// - The index of the line (zero-based) containing the offset.
    /// - The index of the sub-line (zero-based) within the line containing the offset.
    /// - The position of the offset.
    pub(super) fn line_and_position_for_offset(
        &self,
        offset: usize,
    ) -> (usize, usize, Option<Point<Pixels>>) {
        let Some(last_layout) = &self.last_layout else {
            return (0, 0, None);
        };
        let line_height = last_layout.line_height;

        let mut y_offset = last_layout.visible_top;
        for (vi, line) in last_layout.lines.iter().enumerate() {
            let prev_lines_offset = last_layout.visible_line_byte_offsets[vi];
            let local_offset = offset.saturating_sub(prev_lines_offset);
            if let Some(pos) = line.position_for_index(local_offset, last_layout, false) {
                let sub_line_index = (pos.y / line_height) as usize;
                let adjusted_pos = point(pos.x + last_layout.line_number_width, pos.y + y_offset);
                return (vi, sub_line_index, Some(adjusted_pos));
            }

            y_offset += line.size(line_height).height;
        }
        (0, 0, None)
    }

    /// Set the text of the input field.
    ///
    /// For single-line inputs the caret is placed at the end of the text while
    /// the view is scrolled back to the start, so a long value shows its
    /// beginning instead of its tail (matching HTML `<input>`). Multi-line
    /// inputs reset the selection to `0..0`.
    pub fn set_value(
        &mut self,
        value: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.undo_manager.set_ignoring(true);
        self.emit_events = false;
        self.replace_text(value, window, cx);
        self.undo_manager.set_ignoring(false);
        self.emit_events = true;

        self.reset_selection();
        self.reset_lsp_state();
        self.reset_scroll_to_start();

        self.undo_manager.clear();
        cx.notify();
    }

    /// Replace the entire text content while preserving undo history.
    ///
    /// Unlike [`set_value`](Self::set_value), this method records the
    /// replacement in the undo stack, allowing the user to undo/redo
    /// the change. The selection is placed at the end of the new text
    /// for single-line inputs, or cleared (0..0) for multi-line inputs.
    ///
    /// Use this when programmatically replacing the full text but the
    /// user should still be able to undo the operation — e.g. formatting.
    pub fn replace_all(
        &mut self,
        text: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_text(text, window, cx);
        self.reset_selection();
        self.reset_lsp_state();
        self.reset_scroll_to_start();

        cx.notify();
    }

    /// Perform `f` with the user-facing edit restrictions lifted.
    ///
    /// The `disabled` and `readonly` modes only reject the changes made by the
    /// user, the programmatic APIs must always be able to update the text.
    fn with_edits_allowed(&mut self, f: impl FnOnce(&mut Self)) {
        let (was_disabled, was_readonly) = (self.disabled, self.readonly);
        (self.disabled, self.readonly) = (false, false);
        f(self);
        (self.disabled, self.readonly) = (was_disabled, was_readonly);
    }

    /// Insert text at the current cursor position.
    ///
    /// And the cursor will be moved to the end of inserted text.
    pub fn insert(
        &mut self,
        text: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text: SharedString = text.into();
        self.with_edits_allowed(|this| {
            this.undo_manager.set_pending_intent(EditIntent::Atomic);
            let range_utf16 = this.range_to_utf16(&(this.cursor()..this.cursor()));
            this.replace_text_in_range_silent(Some(range_utf16), &text, window, cx);
            let end = this.active_selection().end;
            this.set_cursor_to(end);
        });
    }

    /// Replace text at the current cursor position.
    ///
    /// And the cursor will be moved to the end of replaced text.
    pub fn replace(
        &mut self,
        text: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text: SharedString = text.into();
        self.with_edits_allowed(|this| {
            this.undo_manager.set_pending_intent(EditIntent::Atomic);
            this.replace_text_in_range_silent(None, &text, window, cx);
            let end = this.active_selection().end;
            this.set_cursor_to(end);
        });
    }

    fn replace_text(
        &mut self,
        text: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text: SharedString = text.into();
        self.with_edits_allowed(|this| {
            this.undo_manager.set_pending_intent(EditIntent::Atomic);
            let range = 0..this.text.chars().map(|c| c.len_utf16()).sum();
            this.replace_text_in_range_silent(Some(range), &text, window, cx);
            // A fresh highlighter for the new document, made now rather than
            // at the next render: a line rebuilt in between (an application
            // re-laying a row out after the change) would find none and lose
            // what the highlighter decides -- a table row laid out as prose.
            this.reset_highlighter(cx);
            this.mode.update_highlighter::<M>(
                super::mode::HighlighterUpdate {
                    selected_range: &(0..0),
                    old_text: &this.text,
                    new_text: &this.text,
                    change_text: "",
                    force: false,
                },
                window,
                cx,
            );
        });
    }

    fn reset_selection(&mut self) {
        self.selections.remove_all_but_active();

        // For single-line inputs the caret is placed at the end of the text
        // (matching HTML `<input>`); multi-line inputs reset the selection to
        // `0..0`.
        if self.is_single_line() {
            let end = self.text.len();
            self.set_cursor_to(end);
        } else {
            self.active_selection_mut().clear();
        }
    }

    fn reset_lsp_state(&mut self) {
        if self.is_code_editor() {
            self._pending_update = true;
            M::reset_language_features(self);
        }
    }

    fn reset_scroll_to_start(&mut self) {
        // Move scroll to the start. For single-line the caret is at the end, so
        // override the cursor-follow scroll for the next painted frame to keep
        // the start visible; the deferred offset is consumed during that paint.
        self.scroll_handle.set_offset(point(px(0.), px(0.)));
        if self.is_single_line() {
            self.deferred_scroll_offset = Some(point(px(0.), px(0.)));
        }
    }

    /// Set with disabled mode.
    ///
    /// See also: [`Self::set_disabled`].
    #[allow(unused)]
    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        if self.disabled == disabled {
            return;
        }

        self.disabled = disabled;
        cx.notify();
    }

    /// Set with read-only mode.
    ///
    /// Unlike [`Self::disabled`], a read-only input keeps the normal appearance,
    /// focus, cursor, selection and copy behavior, it only rejects any change
    /// of the text made by the user.
    ///
    /// See also: [`Self::set_readonly`].
    #[allow(unused)]
    pub(crate) fn readonly(mut self, readonly: bool) -> Self {
        self.readonly = readonly;
        self
    }

    pub fn set_readonly(&mut self, readonly: bool, cx: &mut Context<Self>) {
        if self.readonly == readonly {
            return;
        }

        self.readonly = readonly;
        if readonly {
            self.search_session.replace_mode = false;
        }
        cx.notify();
    }

    /// Returns true if the user is allowed to change the text.
    ///
    /// This is false when the input is `disabled` or `readonly`, the programmatic
    /// APIs (e.g.: [`Self::set_value`], [`Self::insert`]) are not limited by this.
    pub fn is_editable(&self) -> bool {
        !self.disabled && !self.readonly
    }

    /// Set true to clear the input by pressing Escape key.
    pub fn clean_on_escape(mut self) -> Self {
        self.clean_on_escape = true;
        self
    }

    pub fn set_clean_on_escape(&mut self, clean: bool) {
        self.clean_on_escape = clean;
    }

    /// Set true to treat `Enter` as a submit action in multi-line mode,
    /// while `Shift+Enter` inserts a newline.
    ///
    /// Default is `false` (both `Enter` and `Shift+Enter` insert a newline).
    #[doc(hidden)]
    pub fn submit_on_enter(mut self, submit: bool) -> Self {
        self.submit_on_enter = submit;
        self
    }

    pub fn set_submit_on_enter(&mut self, submit: bool, cx: &mut Context<Self>) {
        self.submit_on_enter = submit;
        cx.notify();
    }

    /// Set whether to show whitespace characters.
    #[doc(hidden)]
    pub fn show_whitespaces(mut self, show: bool) -> Self {
        self.show_whitespaces = show;
        self
    }

    /// Update whether to show whitespace characters.
    pub fn set_show_whitespaces(&mut self, show: bool, _: &mut Window, cx: &mut Context<Self>) {
        self.show_whitespaces = show;
        cx.notify();
    }

    /// Empty rows reserved below the last line of content ("scroll
    /// beyond last line"), code-editor mode only. Mirrors VSCode's
    /// `editor.scrollBeyondLastLine` / Zed's `scroll_beyond_last_line`.
    ///
    /// - `None` (default): half the viewport, floored at
    ///   [`BOTTOM_MARGIN_ROWS`] line-heights.
    /// - `Some(0)`: no trailing space; the cursor sits flush with the
    ///   last row at scroll-max.
    /// - `Some(n)`: exactly `n` rows.
    pub fn scroll_beyond_last_line(mut self, rows: Option<usize>) -> Self {
        self.scroll_beyond_last_line = rows;
        self
    }

    /// Update [`Self::scroll_beyond_last_line`] after construction.
    pub fn set_scroll_beyond_last_line(
        &mut self,
        rows: Option<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.scroll_beyond_last_line == rows {
            return;
        }
        self.scroll_beyond_last_line = rows;
        cx.notify();
    }

    /// Minimum number of lines the cursor is kept clear of the viewport's
    /// top/bottom edge before auto-scroll engages. Mirrors VSCode's
    /// `editor.cursorSurroundingLines` / Zed's `vertical_scroll_margin`.
    /// Orthogonal to [`Self::scroll_beyond_last_line`], which sizes the
    /// empty region; this controls the cursor's resting distance from the
    /// edge.
    ///
    /// - `None` (default): [`BOTTOM_MARGIN_ROWS`] lines, falling back to
    ///   one line on small viewports.
    /// - `Some(n)`: exactly `n` lines, clamped to half the viewport.
    pub fn cursor_surrounding_lines(mut self, lines: Option<usize>) -> Self {
        self.cursor_surrounding_lines = lines;
        self
    }

    /// Update [`Self::cursor_surrounding_lines`] after construction.
    pub fn set_cursor_surrounding_lines(
        &mut self,
        lines: Option<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.cursor_surrounding_lines == lines {
            return;
        }
        self.cursor_surrounding_lines = lines;
        cx.notify();
    }

    /// Set the default value of the input field.
    pub fn default_value(mut self, value: impl Into<SharedString>) -> Self {
        let text: SharedString = value.into();
        self.text = Rope::from(self.normalize_input(&text).as_ref());
        if let Some(diagnostics) = self.mode.diagnostics_mut() {
            diagnostics.reset(&self.text)
        }
        // Note: We can't call display_map.set_text here because it needs cx.
        // The text will be set during prepare_if_need in element.rs
        self._pending_update = true;
        self
    }

    /// Return the value of the input field as an owned string.
    ///
    /// The string is materialized on each call. See [`Self::text`] for the
    /// [`Rope`] the state owns, which is borrowed and costs nothing to read.
    pub fn value(&self) -> SharedString {
        SharedString::new(self.text.to_string())
    }

    /// Return the portion of the value within the input field that
    /// is selected by the user, as an owned string.
    ///
    /// The string is materialized on each call. See [`Self::selected_text`]
    /// for the same selection borrowed out of the [`Rope`] the state owns.
    pub fn selected_value(&self) -> SharedString {
        SharedString::new(self.selected_text().to_string())
    }

    /// Return the value without mask.
    pub fn unmask_value(&self) -> SharedString {
        self.mask_pattern.unmask(&self.text.to_string()).into()
    }

    /// Kept so existing render paths keep compiling.
    ///
    /// Configuration used to be collected by a facade and applied here; the
    /// state now configures itself, so this does nothing and can be deleted at
    /// the call site.
    #[doc(hidden)]
    pub fn prepare(&mut self, _: &mut Window, _: &mut Context<Self>) {}

    /// Return the text [`Rope`] of the input field.
    ///
    /// Borrowed from the state, so reading even a large document copies
    /// nothing. See [`Self::value`] when an owned string is wanted.
    pub fn text(&self) -> &Rope {
        &self.text
    }

    /// Return the (0-based) [`Position`] of the cursor.
    pub fn cursor_position(&self) -> Position {
        let offset = self.cursor();
        self.text.offset_to_position(offset)
    }

    /// Set (0-based) [`Position`] of the cursor.
    ///
    /// This will move the cursor to the specified line and column, and update the selection range.
    pub fn set_cursor_position(
        &mut self,
        position: impl Into<Position>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let position: Position = position.into();
        let offset = self.text.position_to_offset(&position);

        self.move_to(offset, None, cx);
        self.update_preferred_column();
        self.focus(window, cx);
    }

    /// Focus the input field.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
        self.blink_cursor.update(cx, |cursor, cx| {
            cursor.start(cx);
        });
    }

    /// Refresh the input, so the next render re-runs syntax highlighting and
    /// the LSP providers, not just a redraw.
    ///
    /// Assigning the `lsp` providers (or other render-affecting state) at
    /// runtime does not take effect until the text next changes. Call this
    /// afterwards to force the refresh on the next render.
    ///
    /// ```ignore
    /// input.update(cx, |state, cx| {
    ///     state.extras.lsp.hover_provider = Some(provider);
    ///     state.refresh(cx);
    /// });
    /// ```
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self._pending_update = true;
        cx.notify();
    }

    pub(super) fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.undo_manager.break_transaction_coalescing();
        self.select_all_cursors_to(|s, sel| s.previous_boundary(sel.cursor_offset()), cx);
    }

    pub(super) fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_all_cursors_to(|s, sel| s.next_boundary(sel.cursor_offset()), cx);
    }

    pub(super) fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.select_vertical(-1, cx);
    }

    pub(super) fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.select_vertical(1, cx);
    }

    /// Extend every selection one row up or down from its head, at the head's
    /// x: Shift+Up and Shift+Down as every editor has them.
    ///
    /// This goes through [`Self::vertical_target`] rather than to the start or
    /// end of the neighbouring line, so the head keeps its column, and inside a
    /// table cell it moves by text row before leaving the row.
    fn select_vertical(&mut self, move_lines: isize, cx: &mut Context<Self>) {
        if self.is_single_line() {
            return;
        }
        self.undo_manager.break_transaction_coalescing();
        self.select_all_cursors_to(
            move |s, sel| {
                let head = sel.cursor_offset();
                let anchor = sel.column_anchor.or_else(|| s.preferred_column_for(head));
                s.vertical_target(head, anchor, s.line_end_affinity_for(sel), move_lines)
                    .0
            },
            cx,
        );
    }

    pub(super) fn on_action_select_all(
        &mut self,
        _: &SelectAll,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_all(window, cx);
    }

    pub(super) fn select_to_start(
        &mut self,
        _: &SelectToStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.undo_manager.break_transaction_coalescing();
        self.select_all_cursors_to(|_, _| 0, cx);
    }

    pub(super) fn select_to_end(
        &mut self,
        _: &SelectToEnd,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.undo_manager.break_transaction_coalescing();
        let end = self.text.len();
        self.select_all_cursors_to(move |_, _| end, cx);
    }

    pub(super) fn select_to_start_of_line(
        &mut self,
        _: &SelectToStartOfLine,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.undo_manager.break_transaction_coalescing();
        self.select_all_cursors_to(
            |s, sel| s.start_of_line_at(sel.cursor_offset(), s.line_end_affinity_for(sel)),
            cx,
        );
    }

    pub(super) fn select_to_end_of_line(
        &mut self,
        _: &SelectToEndOfLine,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.undo_manager.break_transaction_coalescing();
        self.select_all_cursors_to(
            |s, sel| s.end_of_line_at(sel.cursor_offset(), s.line_end_affinity_for(sel)),
            cx,
        );
        // Mirrors MoveEnd: the caret belongs at the end of the visual row it is on.
        self.cursor_line_end_affinity = true;
    }

    pub(super) fn select_to_previous_word(
        &mut self,
        _: &SelectToPreviousWordStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.undo_manager.break_transaction_coalescing();
        self.select_all_cursors_to(
            |s, sel| s.previous_start_of_word_at(sel.cursor_offset()),
            cx,
        );
    }

    pub(super) fn select_to_next_word(
        &mut self,
        _: &SelectToNextWordEnd,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.undo_manager.break_transaction_coalescing();
        self.select_all_cursors_to(|s, sel| s.next_end_of_word_at(sel.cursor_offset()), cx);
    }

    /// Return the start offset of the previous word.
    /// Return the previous start offset of the word before `offset`.
    pub(super) fn previous_start_of_word_at(&self, offset: usize) -> usize {
        if self.masked {
            // The mask replaces every character, so the displayed text has no
            // word boundaries to move or delete by. Collapse the word to the
            // whole text.
            return 0;
        }

        let offset = self.offset_from_utf16(self.offset_to_utf16(offset));
        // FIXME: Avoid to_string
        let left_part = self.text.slice(0..offset).to_string();

        UnicodeSegmentation::split_word_bound_indices(left_part.as_str())
            .rfind(|(_, s)| !s.trim_start().is_empty())
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    /// Return the next end offset of the word after `offset`.
    pub(super) fn next_end_of_word_at(&self, offset: usize) -> usize {
        if self.masked {
            // See `previous_start_of_word_at`.
            return self.text.len();
        }

        let offset = self.offset_from_utf16(self.offset_to_utf16(offset));
        let right_part = self.text.slice(offset..self.text.len()).to_string();

        UnicodeSegmentation::split_word_bound_indices(right_part.as_str())
            .find(|(_, s)| !s.trim_start().is_empty())
            .map(|(i, s)| offset + i + s.len())
            .unwrap_or(self.text.len())
    }

    /// Get start of line byte offset for the given `offset`.
    ///
    /// When soft wrap is active, first press goes to visual line start,
    /// second press (already at visual start) goes to logical line start.
    pub(super) fn start_of_line_at(&self, offset: usize, line_end_affinity: bool) -> usize {
        if self.is_single_line() {
            return 0;
        }

        let row = self.text.offset_to_point(offset).row;
        let logical_start = self.text.line_start_offset(row);

        if self.soft_wrap && self.is_code_editor() {
            let wrap_point = self
                .display_map
                .offset_to_wrap_display_point_with_affinity(offset, line_end_affinity);
            if let Some(line) = self.display_map.line(row)
                && let Some(range) = line.wrapped_lines.get(wrap_point.local_row)
            {
                let visual_start = logical_start + range.start;
                if offset != visual_start {
                    return visual_start;
                }
            }
        }

        logical_start
    }

    /// Get end of line byte offset for the given `offset`.
    ///
    /// When soft wrap is active, first press goes to visual line end,
    /// second press (already at visual end) goes to logical line end.
    pub(super) fn end_of_line_at(&self, offset: usize, line_end_affinity: bool) -> usize {
        if self.is_single_line() {
            return self.text.len();
        }

        let row = self.text.offset_to_point(offset).row;
        let logical_start = self.text.line_start_offset(row);
        let logical_end = self.text.line_end_offset(row);

        if self.soft_wrap && self.is_code_editor() {
            // Use the row the caret is drawn on: at a wrap boundary the raw offset would name
            // the next row, and a second End press would keep walking down instead of falling
            // through to the logical line end.
            let wrap_point = self
                .display_map
                .offset_to_wrap_display_point_with_affinity(offset, line_end_affinity);
            if let Some(line) = self.display_map.line(row)
                && let Some(range) = line.wrapped_lines.get(wrap_point.local_row)
            {
                let visual_end = logical_start + range.end;
                if offset != visual_end {
                    return visual_end;
                }
            }
        }

        logical_end
    }

    /// Get indent string of next line.
    ///
    /// To get current and next line indent, to return more depth one.
    pub(super) fn indent_of_next_line(&mut self) -> String {
        self.indent_of_next_line_at(self.cursor())
    }

    /// Get indent string of the next line, relative to the given `offset`.
    pub(super) fn indent_of_next_line_at(&mut self, offset: usize) -> String {
        if self.is_single_line() {
            return "".into();
        }

        let mut current_indent = String::new();
        let mut next_indent = String::new();
        let line_end_affinity = self.line_end_affinity_at(offset);
        let current_line_start_pos = self.start_of_line_at(offset, line_end_affinity);
        let next_line_start_pos = self.end_of_line_at(offset, line_end_affinity);
        for c in self.text.slice(current_line_start_pos..).chars() {
            if !c.is_whitespace() {
                break;
            }
            if c == '\n' || c == '\r' {
                break;
            }
            current_indent.push(c);
        }

        for c in self.text.slice(next_line_start_pos..).chars() {
            if !c.is_whitespace() {
                break;
            }
            if c == '\n' || c == '\r' {
                break;
            }
            next_indent.push(c);
        }

        if next_indent.len() > current_indent.len() {
            return next_indent;
        } else {
            return current_indent;
        }
    }

    /// Delete every selection as one batch. Collapsed cursors are first
    /// expanded to a deletion range by `collapsed_target` and non-empty
    /// selections delete their own range.
    ///
    /// `collapsed_intent` is the intent to record when every cursor is
    /// collapsed, which is what makes a run of single-character deletes undo
    /// as one gesture. Deleting a real selection is always atomic.
    fn delete_selections(
        &mut self,
        silent: bool,
        collapsed_intent: EditIntent,
        mut collapsed_target: impl FnMut(&mut Self, usize) -> Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_editable() {
            return;
        }
        let cursors: Vec<CursorSelection> = self.selections.iter().copied().collect();
        let intent = if cursors.iter().all(|sel| sel.is_empty()) {
            collapsed_intent
        } else {
            EditIntent::Atomic
        };
        let mut new_selections: Vec<CursorSelection> = Vec::with_capacity(cursors.len());
        for sel in &cursors {
            let range = if sel.is_empty() {
                collapsed_target(self, sel.cursor_offset())
            } else {
                sel.start..sel.end
            };
            let (start, end) = (range.start.min(range.end), range.start.max(range.end));
            let mut selection = *sel;
            selection.start = start;
            selection.end = end;
            new_selections.push(selection);
        }
        // Capture the user's selections before expanding or merging deletion
        // ranges. The edit ranges cannot reconstruct their original carets.
        self.undo_manager.begin_transaction_with(intent);
        self.undo_manager
            .record_selections(cursors.clone(), cursors.clone());
        self.selections.replace_all(new_selections);
        self.undo_manager.set_pending_intent(intent);

        let was_silent = self.silent_replace_text;
        self.silent_replace_text = silent;
        self.replace_text_in_range(None, "", window, cx);
        self.silent_replace_text = was_silent;
        self.undo_manager
            .record_selections(cursors, self.selections.iter().copied().collect());
        self.undo_manager.commit_transaction();
        self.pause_blink_cursor(cx);
    }

    pub(super) fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        // Nothing to delete at the start of the text. Propagate so an ancestor
        // (e.g. a command palette navigating back a level) can act on it.
        // This is harmless when nothing upstream is bound to backspace. With multiple
        // cursors the others can still delete, so only the lone-cursor case
        // propagates.
        if self.selections.is_single() && self.active_selection().is_empty() && self.cursor() == 0 {
            cx.propagate();
            return;
        }

        self.delete_selections(
            false,
            EditIntent::Backspace,
            |s, offset| s.previous_boundary(offset)..offset,
            window,
            cx,
        );
    }

    pub(super) fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        self.delete_selections(
            false,
            EditIntent::DeleteForward,
            |s, offset| offset..s.next_boundary(offset),
            window,
            cx,
        );
    }

    pub(super) fn delete_to_beginning_of_line(
        &mut self,
        _: &DeleteToBeginningOfLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.delete_selections(
            true,
            EditIntent::Atomic,
            |s, offset| {
                let mut start = s.start_of_line_at(offset, s.line_end_affinity_at(offset));
                if start == offset {
                    start = start.saturating_sub(1);
                }
                start..offset
            },
            window,
            cx,
        );
    }

    pub(super) fn delete_to_end_of_line(
        &mut self,
        _: &DeleteToEndOfLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.delete_selections(
            true,
            EditIntent::Atomic,
            |s, offset| {
                let mut end = s.end_of_line_at(offset, s.line_end_affinity_at(offset));
                if end == offset {
                    end = (end + 1).clamp(0, s.text.len());
                }
                offset..end
            },
            window,
            cx,
        );
    }

    pub(super) fn delete_previous_word(
        &mut self,
        _: &DeleteToPreviousWordStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.delete_selections(
            true,
            EditIntent::Atomic,
            |s, offset| s.previous_start_of_word_at(offset)..offset,
            window,
            cx,
        );
    }

    pub(super) fn delete_next_word(
        &mut self,
        _: &DeleteToNextWordEnd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.delete_selections(
            true,
            EditIntent::Atomic,
            |s, offset| offset..s.next_end_of_word_at(offset),
            window,
            cx,
        );
    }

    pub(super) fn enter(&mut self, action: &Enter, window: &mut Window, cx: &mut Context<Self>) {
        if M::handle_context_menu_action(self, Box::new(action.clone()), window, cx) {
            return;
        }

        // Clear inline completion on enter (user chose not to accept it)
        if M::has_inline_completion(self) {
            M::clear_inline_completion(self, cx);
        }

        // In multi-line mode with `submit_on_enter` enabled, a plain `Enter`
        // (without Shift) is treated as submit: propagate the action and emit
        // PressEnter without inserting a newline. `Shift+Enter` still inserts
        // a newline.
        let insert_newline = self.is_multi_line() && (!self.submit_on_enter || action.shift);

        if insert_newline {
            if !self.selections.is_single() {
                // Insert a newline (with per-line indent) at every cursor.
                self.selections.merge_overlapping();
                let selections: Vec<CursorSelection> = self.selections.iter().copied().collect();
                let mut edits: Vec<(Range<usize>, String)> = Vec::with_capacity(selections.len());
                for sel in &selections {
                    let indent = if self.is_code_editor() {
                        self.indent_of_next_line_at(sel.cursor_offset())
                    } else {
                        String::new()
                    };
                    edits.push((sel.start..sel.end, format!("\n{}", indent)));
                }
                edits.sort_by_key(|(range, _)| range.start);
                self.replace_text_in_ranges(&edits, window, cx);
                self.pause_blink_cursor(cx);
            } else {
                // Get current line indent
                let indent = if self.is_code_editor() {
                    self.indent_of_next_line()
                } else {
                    "".to_string()
                };

                // Add newline and indent
                let new_line_text = format!("\n{}", indent);
                self.replace_text_in_range_silent(None, &new_line_text, window, cx);
                self.pause_blink_cursor(cx);
            }
        } else {
            // Single line input or submit-on-enter: just emit the event
            // (e.g.: in a dialog to confirm, or a chat textarea to send).
            self.undo_manager.break_transaction_coalescing();
            cx.propagate();
        }

        cx.emit(InputEvent::PressEnter {
            secondary: action.secondary,
            shift: action.shift,
        });
    }

    pub fn clean(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text("", window, cx);
        self.set_selection(0, 0);
        self.scroll_to(0, None, cx);
    }

    pub(super) fn escape(&mut self, action: &Escape, window: &mut Window, cx: &mut Context<Self>) {
        if M::handle_context_menu_action(self, Box::new(action.clone()), window, cx) {
            return;
        }

        // Collapse extra cursors back to the active one first.
        if !self.selections.is_single() {
            self.undo_manager.break_transaction_coalescing();
            self.selections.remove_all_but_active();
            cx.notify();
            return;
        }

        // Clear inline completion on escape
        if M::has_inline_completion(self) {
            M::clear_inline_completion(self, cx);
            return; // Consume the escape, don't propagate
        }

        if self.ime_marked_range.is_some() {
            self.unmark_text(window, cx);
        }

        if self.clean_on_escape {
            return self.clean(window, cx);
        }

        cx.propagate();
    }

    /// Show the right-click context menu as a native OS menu.
    pub(crate) fn handle_right_click_menu(
        &mut self,
        position: Point<Pixels>,
        offset: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disabled {
            return;
        }
        if crate::GlobalState::is_in_deferred_context(cx) {
            return;
        }

        if !self.active_selection().contains(offset) {
            self.move_to(offset, None, cx);
        }

        if self.is_code_editor() {
            M::on_hover_definition(self, offset, window, cx);
        }

        if let Some(handler) = self.context_menu_handler.clone() {
            let capabilities = self.context_menu_capabilities();
            cx.defer_in(window, move |_, window, cx| {
                handler(NativeMenu::new(), capabilities, position, window, cx);
            });
        }
    }

    pub(super) fn add_cursor_above(
        &mut self,
        _: &AddCursorAbove,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_cursor_vertical(-1, cx);
    }

    pub(super) fn add_cursor_below(
        &mut self,
        _: &AddCursorBelow,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_cursor_vertical(1, cx);
    }

    /// Add a new cursor one display line above (`move_lines < 0`) or below each
    /// existing cursor, preserving the column. Cursors that would not move (at
    /// the first/last display row) or that would duplicate an existing cursor
    /// are skipped.
    fn add_cursor_vertical(&mut self, move_lines: isize, cx: &mut Context<Self>) {
        if !self.is_multi_line() {
            return;
        }

        self.pause_blink_cursor(cx);
        // Changing the cursor set ends the editing gesture that came before it.
        self.undo_manager.break_transaction_coalescing();

        let sources: Vec<(usize, Option<(Pixels, usize)>, bool)> = self
            .selections
            .iter()
            .map(|sel| {
                (
                    sel.cursor_offset(),
                    sel.column_anchor,
                    self.line_end_affinity_for(sel),
                )
            })
            .collect();
        let mut offsets: std::collections::HashSet<usize> =
            sources.iter().map(|(offset, _, _)| *offset).collect();

        let mut newest: Option<usize> = None;
        for (offset, anchor, line_end_affinity) in sources {
            let anchor = anchor.or_else(|| self.preferred_column_for(offset));
            let (target, _) = self.vertical_target(offset, anchor, line_end_affinity, move_lines);
            if target == offset || offsets.contains(&target) {
                continue;
            }
            offsets.insert(target);
            let id = self.selections.generate_id();
            let mut cursor = CursorSelection::new(id, target, target);
            cursor.column_anchor = anchor;
            self.selections.add(cursor);
            newest = Some(target);
        }

        if let Some(newest) = newest {
            self.scroll_to(newest, None, cx);
        }
        cx.notify();
    }

    /// Add an additional collapsed cursor at `offset`.
    ///
    /// Rejected when `offset` lands inside an existing selection or exactly on
    /// an existing cursor.
    pub(super) fn add_cursor_at(&mut self, offset: usize, cx: &mut Context<Self>) {
        if !self.is_multi_line() {
            return;
        }

        for sel in self.selections.iter() {
            if sel.contains(offset) {
                return;
            }
            if sel.is_collapsed() && sel.cursor_offset() == offset {
                return;
            }
        }

        self.undo_manager.break_transaction_coalescing();
        let id = self.selections.generate_id();
        self.selections
            .add(CursorSelection::new(id, offset, offset));
        cx.notify();
    }

    /// Build a columnar (block) selection spanning the rows between the two
    /// offsets, one selection per display row at the same column span.
    pub(super) fn build_columnar_selection(
        &mut self,
        start_offset: usize,
        end_offset: usize,
        cx: &mut Context<Self>,
    ) {
        if !self.is_multi_line() {
            return;
        }

        self.undo_manager.break_transaction_coalescing();
        let (start, end) = if start_offset <= end_offset {
            (start_offset, end_offset)
        } else {
            (end_offset, start_offset)
        };

        let start_point = self.display_map.offset_to_wrap_display_point(start);
        let end_point = self.display_map.offset_to_wrap_display_point(end);

        let start_col = start_point.column;
        let end_col = end_point.column;
        let (start_col, end_col) = if start_col <= end_col {
            (start_col, end_col)
        } else {
            (end_col, start_col)
        };

        let start_row = self
            .display_map
            .wrap_row_to_display_row(start_point.row)
            .unwrap_or_else(|| {
                self.display_map
                    .nearest_visible_display_row(start_point.row)
            });
        let end_row = self
            .display_map
            .wrap_row_to_display_row(end_point.row)
            .unwrap_or_else(|| self.display_map.nearest_visible_display_row(end_point.row));
        let (start_row, end_row) = (start_row.min(end_row), start_row.max(end_row));

        let mut new_selections = Vec::with_capacity(end_row - start_row + 1);
        for row in start_row..=end_row {
            let sel_start = self
                .display_map
                .display_row_column_to_offset(row, start_col);
            let sel_end = self.display_map.display_row_column_to_offset(row, end_col);
            let id = self.selections.generate_id();
            let sel_start = self.text.clip_offset(sel_start, Bias::Left);
            let sel_end = self.text.clip_offset(sel_end, Bias::Left);
            new_selections.push(CursorSelection::new(id, sel_start, sel_end));
        }

        if new_selections.is_empty() {
            let id = self.selections.generate_id();
            new_selections.push(CursorSelection::new(id, end, end));
        }

        self.selections.replace_all(new_selections);
        cx.notify();
    }

    pub(super) fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A widget drawn over the text owns clicks that land on it.
        if self
            .widget_hitboxes
            .borrow()
            .iter()
            .any(|bounds| bounds.contains(&event.position))
        {
            return;
        }

        self.undo_manager.break_transaction_coalescing();
        // Input has its own text selection; suppress the window-level text
        // selection (Root) so it does not start a drag from here.
        crate::global_state::GlobalState::suppress_text_selection(cx);

        // Clear inline completion on any mouse interaction
        M::clear_inline_completion(self, cx);

        // If there have IME marked range and is empty (Means pressed Esc to abort IME typing)
        // Clear the marked range.
        if let Some(ime_marked_range) = &self.ime_marked_range {
            if ime_marked_range.len() == 0 {
                self.ime_marked_range = None;
            }
        }

        self.selecting = true;
        let (offset, line_end_affinity) = self.index_for_mouse_position(event.position);

        if M::on_click(self, event, offset, window, cx) {
            return;
        }

        // Triple click to select line
        if event.button == MouseButton::Left && event.click_count >= 3 {
            self.select_line(offset, window, cx);
            return;
        }

        // Double click to select word
        if event.button == MouseButton::Left && event.click_count == 2 {
            self.select_word(offset, window, cx);
            return;
        }

        // Show Mouse context menu
        if event.button == MouseButton::Right {
            if self.enable_context_menu {
                if !self.active_selection().contains(offset) {
                    self.move_to(offset, None, cx);
                }
                self.pending_context_menu = Some((event.position, offset));
            }
            return;
        }

        // Multi-cursor placement, multi-line only.
        if self.is_multi_line() && event.button == MouseButton::Left {
            if event.modifiers.alt
                && (event.modifiers.shift || (cfg!(target_os = "linux") && event.modifiers.control))
            {
                // Alt+Shift starts a block; Linux also accepts Ghostty's Ctrl+Alt.
                // Mark selecting so the drag handler extends the block.
                self.column_select_start = Some(offset);
                self.selecting = true;
                self.move_to_with_affinity(offset, None, line_end_affinity, cx);
                return;
            } else if event.modifiers.alt {
                self.add_cursor_at(offset, cx);
                // Keep click-to-add behavior, but use this press as the block
                // anchor if the user continues dragging with the left button.
                self.column_select_start = Some(offset);
                return;
            }
        }

        if event.modifiers.shift {
            self.select_to_with_affinity(offset, line_end_affinity, cx);
        } else {
            self.move_to_with_affinity(offset, None, line_end_affinity, cx)
        }
    }

    pub(super) fn on_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button == MouseButton::Right {
            if let Some((position, offset)) = self.pending_context_menu.take() {
                self.handle_right_click_menu(position, offset, window, cx);
            }
        }
        if self.active_selection().is_empty() {
            self.active_selection_mut().reversed = false;
        }
        self.selecting = false;
        self.selected_word_range = None;
        self.column_select_start = None;
        self.auto_scroll.stop();
    }

    pub(super) fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Check if mouse is within bounds
        let within_bounds = self
            .last_bounds
            .as_ref()
            .map(|bounds| bounds.contains(&event.position))
            .unwrap_or(false);

        if !within_bounds {
            // Clear hover when mouse leaves the input
            M::clear_hover_state(self, cx);
            if self.table_marker.take().is_some() {
                cx.notify();
            }
            return;
        }

        let marker = self.marker_at(event.position);
        if marker != self.table_marker {
            self.table_marker = marker;
            cx.notify();
        }

        // Show diagnostic popover on mouse move
        let (offset, _) = self.index_for_mouse_position(event.position);
        M::on_mouse_move(self, offset, event, window, cx);

        if self.is_code_editor() {
            if let Some(diagnostic) = self
                .mode
                .diagnostics()
                .and_then(|set| set.for_offset(offset))
            {
                self.diagnostic_popover = Some(Rc::new(diagnostic.clone()));
                cx.notify();
            } else {
                self.diagnostic_popover = None;
            }
        }
    }

    /// Whether a table shows insertion markers (a "+" near a column boundary
    /// of its top rule or near a row's bottom-left corner) under the
    /// pointer. On by default; off for a writer the markers get in the way
    /// of.
    pub fn table_handles(mut self, handles: bool) -> Self {
        self.table_handles = handles;
        self
    }

    pub fn set_table_handles(&mut self, handles: bool, cx: &mut Context<Self>) {
        self.table_handles = handles;
        if !handles && self.table_marker.take().is_some() {
            cx.notify();
        }
    }

    /// The insertion marker near `position`: on the table's first row a
    /// column boundary of its top rule (one per boundary, the right edge
    /// included), on any row but the delimiter its bottom-left corner. The
    /// reach is wider than the drawn marker, so it appears before the pointer
    /// is on it and does not vanish while the pointer crosses it.
    fn marker_at(&self, position: Point<Pixels>) -> Option<TableMarker> {
        const RADIUS: Pixels = px(8.);
        const REACH: Pixels = px(12.);
        if !self.table_handles {
            return None;
        }
        let bounds = self.last_bounds?;
        let layout = self.last_layout.as_ref()?;
        let left = bounds.origin.x + layout.line_number_width;
        let mut y = bounds.origin.y + layout.visible_top;
        for (vi, line) in layout.lines.iter().enumerate() {
            let height = line.size(layout.line_height).height;
            if let Some(table) = line.table.as_ref()
                && !table.collapsed()
            {
                let line_start = layout.visible_line_byte_offsets[vi];
                let mut candidates: Vec<(Point<Pixels>, Option<usize>)> = Vec::new();
                if table.first {
                    for (ix, column) in table.columns.iter().enumerate() {
                        candidates.push((point(left + column.x, y), Some(ix)));
                    }
                    candidates.push((point(left + table.width, y), Some(table.columns.len())));
                }
                if table.kind != crate::input::TableRowKind::Delimiter {
                    candidates.push((point(left, y + height), None));
                }
                for (center, column) in candidates {
                    if (position.x - center.x).abs() <= REACH
                        && (position.y - center.y).abs() <= REACH
                    {
                        // Whole pixels: a column boundary sits anywhere, and
                        // a circle drawn from a half pixel is blurred on one
                        // side and sharp on the other.
                        let center = center.map(|v| v.round());
                        return Some(TableMarker {
                            line_start,
                            column,
                            bounds: Bounds::new(
                                center - point(RADIUS, RADIUS),
                                gpui::size(RADIUS * 2., RADIUS * 2.),
                            ),
                        });
                    }
                }
            }
            y += height;
        }
        None
    }

    pub(super) fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The marker's place is a window position over a layout the scroll
        // moves; it comes back where the pointer is on the next move.
        if self.table_marker.take().is_some() {
            cx.notify();
        }
        let line_height = self
            .last_layout
            .as_ref()
            .map(|layout| layout.line_height)
            .unwrap_or(window.line_height());
        let delta = event.delta.pixel_delta(line_height);

        let old_offset = self.scroll_handle.offset();
        self.update_scroll_offset(Some(old_offset + delta), cx);

        // Only stop propagation if the offset actually changed
        if self.scroll_handle.offset() != old_offset {
            cx.stop_propagation();
        }

        self.diagnostic_popover = None;
    }

    pub(super) fn update_scroll_offset(
        &mut self,
        offset: Option<Point<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        let mut offset = offset.unwrap_or(self.scroll_handle.offset());
        // In addition to left alignment, a cursor position will be reserved on the right side
        let safe_x_offset = if self.text_align == TextAlign::Left {
            px(0.)
        } else {
            -CURSOR_WIDTH
        };

        let safe_y_range =
            (-self.scroll_size.height + self.input_bounds.size.height).min(px(0.0))..px(0.);
        let safe_x_range = (-self.scroll_size.width + self.input_bounds.size.width + safe_x_offset)
            .min(safe_x_offset)..px(0.);

        offset.y = if self.is_single_line() {
            px(0.)
        } else {
            offset.y.clamp(safe_y_range.start, safe_y_range.end)
        };
        offset.x = offset.x.clamp(safe_x_range.start, safe_x_range.end);
        self.scroll_handle.set_offset(offset);
        cx.notify();
    }

    /// Scroll to make the given offset visible.
    ///
    /// If `direction` is Some, will keep edges at the same side.
    pub(crate) fn scroll_to(
        &mut self,
        offset: usize,
        direction: Option<MoveDirection>,
        cx: &mut Context<Self>,
    ) {
        let padding = if direction.is_some() {
            ScrollPadding::SurroundingLines
        } else {
            ScrollPadding::Minimal
        };
        self.scroll_to_with_padding(offset, direction, padding, cx);
    }

    /// Reveal an offset with independently chosen direction restriction and padding.
    /// Search uses surrounding lines without restricting movement to match order.
    pub(crate) fn scroll_to_with_padding(
        &mut self,
        offset: usize,
        direction: Option<MoveDirection>,
        padding: ScrollPadding,
        cx: &mut Context<Self>,
    ) {
        let Some(last_layout) = self.last_layout.as_ref() else {
            return;
        };
        let Some(bounds) = self.last_bounds.as_ref() else {
            return;
        };

        let mut scroll_offset = self.scroll_handle.offset();
        let was_offset = scroll_offset;
        let line_height = last_layout.line_height;

        let point = self.text.offset_to_point(offset);

        let row = point.row;

        // The top of the row through the summed heights, so a row below a
        // taller line is scrolled to where it is drawn, not a fraction of a
        // row above it.
        let mut row_offset_y = line_height * self.display_map.buffer_line_top(row);

        // For Right alignment use 0 margin: the cursor indicator is clamped inside bounds
        // in layout_cursor, so shifting the text here would cause a first-click visual jump.
        let safety_margin = match last_layout.text_align {
            TextAlign::Left => RIGHT_MARGIN,
            TextAlign::Right => px(0.),
            TextAlign::Center => CURSOR_WIDTH,
        };
        if let Some(line) = last_layout
            .lines
            .get(row.saturating_sub(last_layout.visible_range.start))
        {
            // Check to scroll horizontally and soft wrap lines
            if let Some(pos) = line.position_for_index(point.column, last_layout, false) {
                let bounds_width = bounds.size.width - last_layout.line_number_width;
                let col_offset_x = pos.x;
                row_offset_y += pos.y;
                if col_offset_x - safety_margin < -scroll_offset.x {
                    // If the position is out of the visible area, scroll to make it visible
                    scroll_offset.x = -col_offset_x + safety_margin;
                } else if col_offset_x + safety_margin > -scroll_offset.x + bounds_width {
                    scroll_offset.x = -(col_offset_x - bounds_width + safety_margin);
                }
            }
        }

        // Scroll the row into view. Use the same edge clearance helper as
        // `TextElement::layout_cursor` so both scroll-into-view paths agree
        // (a mismatch flickered on `Down` at end-of-buffer with a small
        // `cursor_surrounding_lines` override).
        let edge_height =
            if matches!(padding, ScrollPadding::SurroundingLines) && self.is_code_editor() {
                super::element::cursor_surrounding_padding(
                    self.mode.is_auto_grow(),
                    self.cursor_surrounding_lines,
                    last_layout.visible_range.len(),
                    line_height,
                )
            } else {
                line_height
            };
        if row_offset_y - edge_height + line_height < -scroll_offset.y {
            // Scroll up
            scroll_offset.y = -row_offset_y + edge_height - line_height;
        } else if row_offset_y + edge_height > -scroll_offset.y + bounds.size.height {
            // Scroll down
            scroll_offset.y = -(row_offset_y - bounds.size.height + edge_height);
        }

        // Avoid necessary scroll, when it was already in the correct position.
        if direction == Some(MoveDirection::Up) {
            scroll_offset.y = scroll_offset.y.max(was_offset.y);
        } else if direction == Some(MoveDirection::Down) {
            scroll_offset.y = scroll_offset.y.min(was_offset.y);
        }

        // Clamp the deferred target into the same safe range that
        // `update_scroll_offset` enforces on persist, so paint never shows an
        // over-scrolled frame before the post-paint clamp pulls it back.
        let safe_y_min = (-self.scroll_size.height + self.input_bounds.size.height).min(px(0.));
        scroll_offset.x = scroll_offset.x.min(px(0.));
        scroll_offset.y = scroll_offset.y.clamp(safe_y_min, px(0.));
        self.deferred_scroll_offset = Some(scroll_offset);
        cx.notify();
    }

    pub(super) fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    /// The text of every non-empty selection, in document order.
    fn selected_texts(&self) -> Vec<String> {
        let mut selections: Vec<CursorSelection> = self
            .selections
            .iter()
            .copied()
            .filter(|sel| !sel.is_empty())
            .collect();
        selections.sort_by_key(|sel| sel.start);
        selections
            .iter()
            .map(|sel| self.text.slice(*sel).to_string())
            .collect()
    }

    pub(super) fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.is_copyable() {
            return;
        }

        let texts = self.selected_texts();

        cx.write_to_clipboard(ClipboardItem::new_string(texts.join("\n")));
    }

    pub(super) fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_copyable() {
            return;
        }

        let texts = self.selected_texts();

        cx.write_to_clipboard(ClipboardItem::new_string(texts.join("\n")));

        self.undo_manager.set_pending_intent(EditIntent::Atomic);
        self.replace_text_in_range_silent(None, "", window, cx);
    }

    pub(super) fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_editable() {
            return;
        }
        let Some(clipboard) = cx.read_from_clipboard() else {
            return;
        };
        let mut new_text = clipboard.text().unwrap_or_default();
        // A paste is one atomic edit, never part of a typing run.
        self.undo_manager.set_pending_intent(EditIntent::Atomic);

        if !self.is_multi_line() {
            new_text = new_text.replace('\n', "");
            self.replace_text_in_range_silent(None, &new_text, window, cx);
            self.scroll_to(self.cursor(), None, cx);
            return;
        }

        // Distribute one clipboard line per selection when the counts match.
        // Otherwise insert the whole clipboard text at each cursor.
        if !self.selections.is_single() {
            self.selections.merge_overlapping();
        }
        let lines: Vec<String> = new_text.split('\n').map(|s| s.to_string()).collect();
        let count = self.selections.len();
        if count > 1 && lines.len() == count {
            let mut selections: Vec<CursorSelection> = self.selections.iter().copied().collect();
            selections.sort_by_key(|sel| sel.start);
            let edits: Vec<(Range<usize>, String)> = selections
                .iter()
                .zip(lines)
                .map(|(sel, line)| (sel.start..sel.end, line))
                .collect();
            self.replace_text_in_ranges(&edits, window, cx);
        } else {
            self.replace_text_in_range_silent(None, &new_text, window, cx);
        }
        self.scroll_to(self.cursor(), None, cx);
    }

    /// The intent of a batch the caller did not label: inserting text at
    /// collapsed cursors is typing, anything else stands on its own.
    fn typing_intent(&self, edits: &[(Range<usize>, String)], new_text: &str) -> EditIntent {
        if !new_text.is_empty()
            && !new_text.contains(['\n', '\r'])
            && edits.iter().all(|(range, _)| range.is_empty())
        {
            EditIntent::Typing
        } else {
            EditIntent::Atomic
        }
    }

    /// Where a cursor stood before an edit made with this intent.
    ///
    /// Backspace and forward delete expand a collapsed cursor over the text
    /// they are about to remove, so the recorded cursor has to collapse back to
    /// the side it came from for undo to restore it where the user left it.
    fn collapse_for_intent(
        intent: EditIntent,
        mut selection: CursorSelection,
        range: &Range<usize>,
    ) -> CursorSelection {
        match intent {
            EditIntent::Backspace => selection.place_at(range.end, None),
            EditIntent::DeleteForward => selection.place_at(range.start, None),
            EditIntent::Typing | EditIntent::Atomic => {}
        }
        selection
    }

    fn push_history(
        &mut self,
        text: &Rope,
        range: &Range<usize>,
        new_text: &str,
        requested_intent: Option<EditIntent>,
        selection_before: CursorSelection,
        selection_after: Option<CursorSelection>,
    ) -> bool {
        if self.undo_manager.is_ignoring() {
            return false;
        }

        let range =
            text.clip_offset(range.start, Bias::Left)..text.clip_offset(range.end, Bias::Right);
        let old_text = text.slice(range.clone()).to_string();
        let new_range = range.start..range.start + new_text.len();

        let intent = requested_intent.unwrap_or_else(|| {
            if range.is_empty()
                && old_text.is_empty()
                && !new_text.is_empty()
                && !new_text.contains(['\n', '\r'])
            {
                EditIntent::Typing
            } else {
                EditIntent::Atomic
            }
        });

        let selection_before = Self::collapse_for_intent(intent, selection_before, &range);
        let selection_after =
            selection_after.unwrap_or_else(|| (new_range.end..new_range.end).into());

        let open_transaction = self.undo_manager.has_open_transaction();
        let recorded = self
            .undo_manager
            .record_transaction(Change::new(range, &old_text, new_range, new_text), intent);
        // A batch records its own cursor sets. This covers a change that is a
        // transaction on its own.
        if recorded && !open_transaction {
            self.undo_manager
                .record_selections(vec![selection_before], vec![selection_after]);
        }
        recorded
    }

    /// Flips a GFM task marker, given the raw byte range of its `[ ]`/`[x]`.
    ///
    /// The widget reports its click by editing the *text*, so the document
    /// stays the only source of truth and one click is one undo step. The
    /// range is re-checked against the text first: it was computed during a
    /// layout that may be a frame or two old, and an edit since then could
    /// have moved it.
    pub fn toggle_task_marker(
        &mut self,
        range: Range<usize>,
        checked: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let replacement = if checked { "[ ]" } else { "[x]" };
        let current = if checked { "[x]" } else { "[ ]" };
        if self.text.slice(range.clone()) != current {
            // The text moved under the widget; do nothing rather than corrupt
            // whatever is there now.
            return;
        }
        // Ticking a box is not a request to edit that line: the replacement
        // moves the cursor to where it wrote, which in a live preview drops
        // that row back to its raw markup under the reader's pointer. The
        // selection is put back where it was.
        let selection = self.selected_range();
        self.with_edits_allowed(|this| {
            this.undo_manager.set_pending_intent(EditIntent::Atomic);
            let range_utf16 = this.range_to_utf16(&range);
            this.replace_text_in_range_silent(Some(range_utf16), replacement, window, cx);
        });
        self.set_selection(selection.start, selection.end);
        cx.notify();
    }

    /// Undoes one step, as `Undo` does, for callers that cannot dispatch an
    /// action — a test asserting that an edit is a *single* undo step, which
    /// is otherwise unobservable from outside.
    pub fn undo_once(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.undo(&Undo, window, cx);
    }

    pub(super) fn undo(&mut self, _: &Undo, window: &mut Window, cx: &mut Context<Self>) {
        self.undo_manager.set_ignoring(true);
        // The manager hands the changes back in reverse application order.
        if let Some(replay) = self.undo_manager.undo() {
            for change in &replay.changes {
                let range_utf16 = self.range_to_utf16(&change.new_range.into());
                self.replace_text_in_range_silent(Some(range_utf16), &change.old_text, window, cx);
            }
            self.restore_selections(replay.selections);
        }
        self.undo_manager.set_ignoring(false);
    }

    pub(super) fn redo(&mut self, _: &Redo, window: &mut Window, cx: &mut Context<Self>) {
        self.undo_manager.set_ignoring(true);
        // Redo replays in forward application order.
        if let Some(replay) = self.undo_manager.redo() {
            for change in &replay.changes {
                let range_utf16 = self.range_to_utf16(&change.old_range.into());
                self.replace_text_in_range_silent(Some(range_utf16), &change.new_text, window, cx);
            }
            self.restore_selections(replay.selections);
        }
        self.undo_manager.set_ignoring(false);
    }

    /// Restore a set of selections captured in a transaction, clamping offsets
    /// to the current text length. `None` leaves the current selections as the
    /// replay left them.
    fn restore_selections(&mut self, selections: Option<Vec<CursorSelection>>) {
        let Some(selections) = selections else {
            return;
        };

        let len = self.text.len();
        let restored: Vec<CursorSelection> = selections
            .into_iter()
            .map(|mut sel| {
                sel.start = sel.start.min(len);
                sel.end = sel.end.min(len);
                sel
            })
            .collect();
        self.selections.replace_all(restored);
        self.selections.merge_overlapping();
    }

    /// Get byte offset of the cursor.
    ///
    /// The offset is the UTF-8 offset.
    pub fn cursor(&self) -> usize {
        if let Some(ime_marked_range) = &self.ime_marked_range {
            return ime_marked_range.end;
        }

        self.selections.active().cursor_offset()
    }

    /// Returns the active selection.
    pub(super) fn active_selection(&self) -> &CursorSelection {
        self.selections.active()
    }

    /// Returns a mutable reference to the active selection.
    pub(super) fn active_selection_mut(&mut self) -> &mut CursorSelection {
        self.selections.active_mut()
    }

    /// Sets the active selection to the given range, keeping its `reversed`
    /// and `column_anchor` state untouched.
    pub(super) fn set_selection(&mut self, start: usize, end: usize) {
        let active = self.active_selection_mut();
        active.start = start;
        active.end = end;
    }

    /// Collapses the active selection to a cursor at the given offset,
    /// clearing `reversed`.
    pub(super) fn set_cursor_to(&mut self, offset: usize) {
        let active = self.active_selection_mut();
        active.start = offset;
        active.end = offset;
        active.reversed = false;
    }

    /// Visible row range in the last laid-out viewport, `None` before first layout.
    pub fn visible_row_range(&self) -> Option<std::ops::Range<usize>> {
        self.last_layout.as_ref().map(|l| l.visible_range.clone())
    }

    /// Current scroll offset of the editor viewport.
    pub fn scroll_offset(&self) -> gpui::Point<gpui::Pixels> {
        self.scroll_handle.offset()
    }

    /// Set scroll offset of the editor viewport.
    ///
    /// The offset will be clamped to the valid range, and applied after the next layout.
    pub fn set_scroll_offset(&mut self, offset: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        self.deferred_scroll_offset = Some(offset);
        cx.notify();
    }

    /// Laid-out line height; `None` before first layout.
    pub fn line_height(&self) -> Option<gpui::Pixels> {
        self.last_layout.as_ref().map(|l| l.line_height)
    }

    /// Returns the active selection as a byte range into the text.
    ///
    /// With multiple cursors, this reads only the active selection.
    ///
    /// The range is empty (`start == end`) when no text is selected; in
    /// that case the offset equals `cursor()`. Byte offsets are measured
    /// in the underlying rope's byte units.
    pub fn selected_range(&self) -> std::ops::Range<usize> {
        (*self.selections.active()).into()
    }

    pub fn select_all(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.undo_manager.break_transaction_coalescing();
        self.selections.remove_all_but_active();
        self.set_selection(0, self.text.len());
        cx.notify();
    }

    /// Set the selected range using UTF-8 byte offsets, removing additional cursors.
    ///
    /// Non-empty ranges expand to character boundaries. Empty ranges remain empty and are
    /// clipped to the preceding character boundary.
    pub fn set_selected_range(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        let end_bias = if range.start == range.end {
            Bias::Left
        } else {
            Bias::Right
        };
        let start = self.text.clip_offset(range.start, Bias::Left);
        let end = self.text.clip_offset(range.end, end_bias);

        self.move_to(start, None, cx);
        self.active_selection_mut().reversed = false;
        self.selected_word_range = None;
        self.select_to(end, cx);
    }

    /// The insets this editor's text sits inside: the gutter it reserved on
    /// the left of the last frame, and the margin it keeps on the right.
    ///
    /// An application that draws its own blocks above or below the editor -- a
    /// property sheet over a note's frontmatter, say -- lines its content up
    /// with the text by taking these, rather than deriving them from the mode
    /// flags: the gutter is measured, and a fold icon or a line number changes
    /// it. Soft wrap lays text out in `width - gutter - RIGHT_MARGIN` and the
    /// run is drawn `gutter` from the left edge, with the glyphs' own bearing
    /// setting the ink a further margin in; taking the margin at each end is
    /// what puts such a block over the text rather than a few pixels outside
    /// it.
    /// `None` until the editor has drawn once: the gutter is a measurement,
    /// and answering `0` before there is anything to measure makes a caller
    /// lay its block out at the wrong place for one frame, then jump.
    pub fn text_insets(&self) -> Option<Edges<Pixels>> {
        let gutter = self.last_layout.as_ref()?.line_number_width;
        Some(Edges {
            top: px(0.),
            right: crate::input::element::RIGHT_MARGIN,
            bottom: px(0.),
            left: gutter + crate::input::element::RIGHT_MARGIN,
        })
    }

    /// The byte offset under a window position, when the position is inside
    /// the editor's last-drawn bounds: for a host that reads the text there
    /// (a link under a modified click, say) without taking the click.
    pub fn offset_at(&self, position: Point<Pixels>) -> Option<usize> {
        let bounds = self.last_bounds.as_ref()?;
        bounds
            .contains(&position)
            .then(|| self.index_for_mouse_position(position).0)
    }

    /// Resolve a mouse position to a byte offset in the text.
    ///
    /// Also reports the caret's line-end affinity for that offset: `true` when the position
    /// landed on the wrap boundary of a non-final visual row, meaning the caret belongs at the
    /// end of that row rather than at the start of the next one. Callers that place or extend a
    /// selection must pass it on, or clicking past the last glyph of a wrapped row leaves a
    /// caret one row below the pointer.
    pub(crate) fn index_for_mouse_position(&self, position: Point<Pixels>) -> (usize, bool) {
        // If the text is empty, always return 0
        if self.text.len() == 0 {
            return (0, false);
        }

        let (Some(bounds), Some(last_layout)) =
            (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return (0, false);
        };

        let line_height = last_layout.line_height;
        let line_number_width = last_layout.line_number_width;

        // TIP: About the IBeam cursor
        //
        // If cursor style is IBeam, the mouse mouse position is in the middle of the cursor (This is special in OS)

        // The position is relative to the bounds of the text input
        //
        // bounds.origin:
        //
        // - included the input padding.
        // - included the scroll offset.
        let inner_position = position - bounds.origin - point(line_number_width, px(0.));

        let mut y_offset = last_layout.visible_top;

        // Traverse visible buffer lines (compact, no hidden entries)
        for (vi, (line_layout, _buffer_line)) in last_layout
            .lines
            .iter()
            .zip(last_layout.visible_buffer_lines.iter())
            .enumerate()
        {
            let line_start_offset = last_layout.visible_line_byte_offsets[vi];

            // Calculate line origin for this display row
            let line_origin = point(px(0.), y_offset);
            let pos = inner_position - line_origin;

            // Return offset by use closest_index_for_x if is single line mode.
            if self.is_single_line() {
                let local_index = line_layout.closest_index_for_x(pos.x, last_layout);
                // A single line never wraps, so there is no boundary to disambiguate.
                return (self.resolve_index(line_start_offset + local_index), false);
            }

            let height = line_layout.size(line_height).height;
            let collapsed = line_layout
                .table
                .as_ref()
                .is_some_and(|table| table.collapsed());

            // A collapsed table row takes no click: the pointer on it belongs
            // to the line above (its rule), or to the line below when there
            // is none above; above the visible top, to the line below.
            if collapsed {
                if pos.y >= px(0.) && pos.y < height && vi > 0 {
                    let (prev_offset, prev_layout, prev_origin_y) = (
                        last_layout.visible_line_byte_offsets[vi - 1],
                        &last_layout.lines[vi - 1],
                        y_offset - last_layout.lines[vi - 1].size(line_height).height,
                    );
                    let prev_height = prev_layout.size(line_height).height;
                    let prev_pos = point(
                        pos.x,
                        (inner_position.y - prev_origin_y).min(prev_height - px(0.5)),
                    );
                    let (local_index, affinity) = prev_layout
                        .closest_index_for_position(prev_pos, last_layout)
                        .unwrap_or((prev_layout.len(), false));
                    return (self.resolve_index(prev_offset + local_index), affinity);
                }
                y_offset += height;
                continue;
            }

            // Check if mouse is in this line's bounds
            if let Some((local_index, line_end_affinity)) =
                line_layout.closest_index_for_position(pos, last_layout)
            {
                return (
                    self.resolve_index(line_start_offset + local_index),
                    line_end_affinity,
                );
            } else if pos.y < px(0.) {
                // Mouse is above this line (above the visible top, or past a
                // collapsed table row): the offset under the same x on this
                // line's first row, or its start.
                let local_index = line_layout
                    .closest_index_for_position(point(pos.x, px(0.)), last_layout)
                    .map(|(ix, _)| ix)
                    .unwrap_or(0);
                return (self.resolve_index(line_start_offset + local_index), false);
            }

            y_offset += height;
        }

        // Mouse is below all visible lines, return end of text
        (self.text.len(), false)
    }

    /// Map a display byte index back to a text offset, undoing the mask expansion when the input
    /// is masked.
    fn resolve_index(&self, index: usize) -> usize {
        if self.masked {
            self.text.char_index_to_offset(index / MASK_CHAR.len_utf8())
        } else {
            index.min(self.text.len())
        }
    }

    /// Returns a y offsetted point for the line origin.
    /// Select the text from the current cursor position to the given offset.
    ///
    /// The offset is the UTF-8 offset.
    ///
    /// Ensure the offset use self.next_boundary or self.previous_boundary to get the correct offset.
    /// Extend a single selection so its moving end lands at `offset`, flipping
    /// `reversed` when the ends cross. When a sticky `word_range` is given the
    /// selection is kept covering it.
    fn extend_selection(
        sel: &mut CursorSelection,
        offset: usize,
        word_range: Option<CursorSelection>,
    ) {
        if sel.reversed {
            sel.start = offset;
        } else {
            sel.end = offset;
        }

        if sel.end < sel.start {
            sel.reversed = !sel.reversed;
            std::mem::swap(&mut sel.start, &mut sel.end);
        }

        if let Some(word_range) = word_range {
            if sel.start > word_range.start {
                sel.start = word_range.start;
            }
            if sel.end < word_range.end {
                sel.end = word_range.end;
            }
        }
    }

    /// Extend only the active selection to `offset`. Used by mouse drag.
    ///
    /// Public so an application can answer a selecting key itself (Shift+Home
    /// inside a table cell selects to the cell's start, not the line's) while
    /// keeping the selection's anchor where the engine has it.
    pub fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.select_to_with_affinity(offset, false, cx);
    }

    /// Like [`Self::select_to`], but also carries the caret's line-end affinity.
    ///
    /// See [`Self::move_to_with_affinity`] for why the affinity travels with the offset. Note
    /// that plain [`Self::select_to`] clears the affinity: every offset it is given came from
    /// the text rather than from a visual position, so the caret has no reason to keep sticking
    /// to the end of a wrapped row.
    pub(crate) fn select_to_with_affinity(
        &mut self,
        offset: usize,
        line_end_affinity: bool,
        cx: &mut Context<Self>,
    ) {
        M::clear_inline_completion(self, cx);

        self.cursor_line_end_affinity = line_end_affinity;
        let offset = offset.clamp(0, self.text.len());
        let word_range = self.selected_word_range;
        Self::extend_selection(self.active_selection_mut(), offset, word_range);

        if self.active_selection().is_empty() {
            self.update_preferred_column();
        }
        cx.notify()
    }

    /// Extend every selection to the offset produced by `f`, then merge any
    /// selections that now overlap. Used by keyboard selection commands.
    fn select_all_cursors_to(
        &mut self,
        f: impl Fn(&Self, &CursorSelection) -> usize,
        cx: &mut Context<Self>,
    ) {
        self.pause_blink_cursor(cx);
        self.undo_manager.break_transaction_coalescing();
        M::clear_inline_completion(self, cx);

        let len = self.text.len();
        let new_selections: Vec<CursorSelection> = self
            .selections
            .iter()
            .map(|sel| {
                let offset = f(self, sel).clamp(0, len);
                let mut new_sel = *sel;
                Self::extend_selection(&mut new_sel, offset, None);
                new_sel
            })
            .collect();
        // Resolve targets using the old caret affinity before clearing it.
        self.cursor_line_end_affinity = false;
        self.selections.replace_all(new_selections);
        self.selections.merge_overlapping();

        if self.active_selection().is_empty() {
            self.update_preferred_column();
        }
        self.scroll_to(self.cursor(), None, cx);
        cx.notify()
    }

    /// Unselects the currently selected text.
    pub fn unselect(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.undo_manager.break_transaction_coalescing();
        let offset = self.cursor();
        self.set_cursor_to(offset);
        cx.notify()
    }

    #[inline]
    pub(super) fn offset_from_utf16(&self, offset: usize) -> usize {
        self.text.offset_utf16_to_offset(offset)
    }

    #[inline]
    pub(super) fn offset_to_utf16(&self, offset: usize) -> usize {
        self.text.offset_to_offset_utf16(offset)
    }

    #[inline]
    pub(crate) fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    #[inline]
    pub(super) fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    /// If offset falls on a hidden (folded) line, clamp backward to the end of
    /// the fold header line (last visible position before the fold).
    fn clamp_offset_to_visible_backward(&self, offset: usize) -> usize {
        let line = self.text.offset_to_point(offset).row;
        if self.display_map.is_buffer_line_hidden(line) {
            for fold in self.display_map.folded_ranges() {
                if line > fold.start_line && line <= fold.end_line {
                    return self.text.line_end_offset(fold.start_line);
                }
            }
        }
        offset
    }

    /// If offset falls on a hidden (folded) line, clamp forward to the start of
    /// the fold end line (first visible position after the fold).
    fn clamp_offset_to_visible_forward(&self, offset: usize) -> usize {
        let line = self.text.offset_to_point(offset).row;
        if self.display_map.is_buffer_line_hidden(line) {
            for fold in self.display_map.folded_ranges() {
                if line > fold.start_line && line <= fold.end_line {
                    return self.text.line_start_offset(fold.end_line);
                }
            }
        }
        offset
    }

    pub(super) fn previous_boundary(&self, offset: usize) -> usize {
        let mut offset = self.text.clip_offset(offset.saturating_sub(1), Bias::Left);
        if let Some(ch) = self.text.char_at(offset) {
            if ch == '\r' {
                offset -= 1;
            }
        }

        self.clamp_offset_to_visible_backward(offset)
    }

    pub(super) fn next_boundary(&self, offset: usize) -> usize {
        let mut offset = self.text.clip_offset(offset + 1, Bias::Right);
        if let Some(ch) = self.text.char_at(offset) {
            if ch == '\r' {
                offset += 1;
            }
        }

        self.clamp_offset_to_visible_forward(offset)
    }

    /// Returns the true to let InputElement to render cursor, when Input is focused and current BlinkCursor is visible.
    pub(crate) fn show_cursor(&self, window: &Window, cx: &App) -> bool {
        (self.focus_handle.is_focused(window) || M::is_context_menu_open(self, cx))
            && !self.disabled
            && self.blink_cursor.read(cx).visible()
            && window.is_window_active()
    }

    fn on_focus(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.blink_cursor.update(cx, |cursor, cx| {
            cursor.start(cx);
        });
        cx.emit(InputEvent::Focus);
    }

    fn on_blur(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if M::is_context_menu_open(self, cx) {
            return;
        }

        self.undo_manager.break_transaction_coalescing();

        // NOTE: Do not cancel select, when blur.
        // Because maybe user want to copy the selected text by AppMenuBar (will take focus handle).

        M::clear_hover_state(self, cx);
        self.diagnostic_popover = None;
        M::clear_inline_completion(self, cx);
        self.blink_cursor.update(cx, |cursor, cx| {
            cursor.stop(cx);
        });
        self.clamp_number_value(window, cx);
        cx.emit(InputEvent::Blur);
        cx.notify();
    }

    /// Clamp the number value to the `min`/`max` range, used on blur.
    ///
    /// Out-of-range values are allowed while typing (e.g. `1` is an
    /// intermediate state of `15` when min is 10), and clamped on blur.
    fn clamp_number_value(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_single_line() {
            return;
        }
        if !matches!(self.mask_pattern, MaskPattern::Number { .. }) {
            return;
        }
        if self.number_min.is_none() && self.number_max.is_none() {
            return;
        }

        let Ok(value) = self.unmask_value().parse::<f64>() else {
            return;
        };

        let clamped = match (self.number_min, self.number_max) {
            (Some(min), _) if value < min => min,
            (_, Some(max)) if value > max => max,
            _ => return,
        };

        // The clamped value must pass the `pattern`/`validate` check,
        // otherwise keep the value as is.
        let new_text = clamped.to_string();
        if !self.is_valid_input(&new_text, cx) {
            return;
        }

        let range = self.range_to_utf16(&(0..self.text.len()));
        self.replace_text_in_range_silent(Some(range), &new_text, window, cx);
    }

    pub(super) fn pause_blink_cursor(&mut self, cx: &mut Context<Self>) {
        self.blink_cursor.update(cx, |cursor, cx| {
            cursor.pause(cx);
        });
    }

    pub(super) fn on_drag_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.text.len() == 0 {
            return;
        }

        if self.last_layout.is_none() {
            return;
        }

        if !self.focus_handle.is_focused(window) {
            return;
        }

        if !self.selecting {
            return;
        }

        self.auto_scroll.last_drag_position = Some(event.position);
        let (offset, line_end_affinity) = self.index_for_mouse_position(event.position);
        if let Some(start) = self.column_select_start {
            self.build_columnar_selection(start, offset, cx);
        } else {
            self.select_to_with_affinity(offset, line_end_affinity, cx);
        }

        if !self.is_single_line() {
            let delta = AutoScroll::compute_delta(event.position.y, self.input_bounds);
            // Input's ScrollHandle uses negative-y-is-down; negate the positive-towards-bottom delta.
            let scroll_delta = delta.map(|d| -d);
            self.auto_scroll.set(scroll_delta, cx, |delta, state, cx| {
                let current = state.scroll_handle.offset();
                state.update_scroll_offset(Some(point(current.x, current.y + delta)), cx);
                if let Some(pos) = state.auto_scroll.last_drag_position {
                    let (offset, line_end_affinity) = state.index_for_mouse_position(pos);
                    if let Some(start) = state.column_select_start {
                        state.build_columnar_selection(start, offset, cx);
                    } else {
                        state.select_to_with_affinity(offset, line_end_affinity, cx);
                    }
                }
            });
        }
    }

    /// Normalize the inserted text before applying it to the input.
    ///
    /// For number inputs (with [`MaskPattern::Number`]), this converts
    /// full-width number characters into their ASCII equivalents,
    /// e.g. `12。5` -> `12.5`.
    fn normalize_input<'a>(&self, new_text: &'a str) -> Cow<'a, str> {
        let normalized = if matches!(self.mask_pattern, MaskPattern::Number { .. }) {
            normalize_number_input(new_text)
        } else {
            Cow::Borrowed(new_text)
        };

        if self.is_single_line() && normalized.contains(['\n', '\r']) {
            Cow::Owned(normalized.replace(['\n', '\r'], ""))
        } else {
            normalized
        }
    }

    pub(crate) fn is_valid_input(&self, new_text: &str, cx: &mut Context<Self>) -> bool {
        if new_text.is_empty() {
            return true;
        }

        if let Some(validate) = &self.validate {
            if !validate(new_text, cx) {
                return false;
            }
        }

        if !self.mask_pattern.is_valid(new_text) {
            return false;
        }

        let Some(pattern) = &self.pattern else {
            return true;
        };

        pattern.is_match(new_text)
    }

    /// Set the mask pattern for formatting the input text.
    ///
    /// The pattern can contain:
    /// - 9: Any digit or dot
    /// - A: Any letter
    /// - *: Any character
    /// - Other characters will be treated as literal mask characters
    ///
    /// Example: "(999)999-999" for phone numbers
    pub fn mask_pattern(mut self, pattern: impl Into<MaskPattern>) -> Self {
        self.mask_pattern = pattern.into();
        self.mask_pattern_set = true;
        if let Some(placeholder) = self.mask_pattern.placeholder() {
            self.placeholder = placeholder.into();
        }
        self
    }

    pub fn set_mask_pattern(
        &mut self,
        pattern: impl Into<MaskPattern>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.mask_pattern = pattern.into();
        self.mask_pattern_set = true;
        if let Some(placeholder) = self.mask_pattern.placeholder() {
            self.placeholder = placeholder.into();
        }
        cx.notify();
    }

    /// Apply the default numeric mask unless the caller explicitly selected a mask.
    pub fn ensure_number_mask(&mut self) {
        if self.mask_pattern_set {
            return;
        }
        self.mask_pattern = MaskPattern::Number {
            separator: None,
            fraction: None,
        };
    }

    pub(super) fn set_input_bounds(&mut self, new_bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        let wrap_width_changed = self.input_bounds.size.width != new_bounds.size.width;
        self.input_bounds = new_bounds;

        // Update display_map wrap_width if changed.
        if let Some(last_layout) = self.last_layout.as_ref() {
            if wrap_width_changed {
                let wrap_width = if !self.soft_wrap {
                    // None to disable wrapping (will use Pixels::MAX)
                    None
                } else {
                    last_layout.wrap_width
                };

                self.display_map.on_layout_changed(wrap_width, cx);
                if self.is_multi_line() {
                    self.mode.update_auto_grow(&self.display_map);
                }
                cx.notify();
            }
        }
    }

    /// Return the active selection's text, borrowed out of the [`Rope`]
    /// the state owns.
    ///
    /// See [`Self::selected_value`] when an owned string is wanted.
    pub fn selected_text(&self) -> RopeSlice<'_> {
        let range_utf16 = self.range_to_utf16(&self.selected_range());
        let range = self.range_from_utf16(&range_utf16);
        self.text.slice(range)
    }

    /// Return the rendered bounds for a UTF-8 byte range in the current input contents.
    ///
    /// Returns `None` when the requested range is not currently laid out or visible.
    pub fn range_to_bounds(&self, range: &Range<usize>) -> Option<Bounds<Pixels>> {
        let Some(last_layout) = self.last_layout.as_ref() else {
            return None;
        };

        let Some(last_bounds) = self.last_bounds else {
            return None;
        };

        let (_, _, start_pos) = self.line_and_position_for_offset(range.start);
        let (_, _, end_pos) = self.line_and_position_for_offset(range.end);

        let Some(start_pos) = start_pos else {
            return None;
        };
        let Some(end_pos) = end_pos else {
            return None;
        };

        Some(Bounds::from_corners(
            last_bounds.origin + start_pos,
            last_bounds.origin + end_pos + point(px(0.), last_layout.line_height),
        ))
    }

    /// Replace text in range in silent.
    ///
    /// This will not trigger any UI interaction, such as auto-completion.
    pub(crate) fn replace_text_in_range_silent(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.silent_replace_text = true;
        self.replace_text_in_range(range_utf16, new_text, window, cx);
        self.silent_replace_text = false;
    }

    /// Apply a batch of edits as one atomic history transaction.
    ///
    /// `edits` are `(byte range in the current pre-edit document, replacement)`
    /// pairs.
    pub(crate) fn replace_text_in_ranges(
        &mut self,
        edits: &[(Range<usize>, String)],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_editable() || edits.is_empty() {
            return;
        }

        // Sort descending by start so applying front-of-vec first edits the
        // highest offsets first, leaving lower offsets unchanged.
        let mut sorted: Vec<(Range<usize>, &str)> = edits
            .iter()
            .map(|(range, text)| (range.clone(), text.as_str()))
            .collect();
        sorted.sort_by_key(|edit| std::cmp::Reverse(edit.0.start));

        #[cfg(debug_assertions)]
        for pair in sorted.windows(2) {
            debug_assert!(
                pair[1].0.end <= pair[0].0.start,
                "replace_text_in_ranges requires disjoint ranges"
            );
        }

        // Wrap multiple edits in one explicit transaction so they undo as a
        // unit. A single edit records directly, which keeps it eligible for
        // the undo manager's typing coalescing.
        let requested_intent = self.undo_manager.take_pending_intent();
        let selection_before = *self.active_selection();
        let original_selections: Vec<CursorSelection> = self.selections.iter().copied().collect();
        // Snapshot the cursors before applying, so undo can restore them. A
        // delete has already expanded them over the text it removes, so they
        // collapse back to where the user left them.
        let selections_before: Vec<CursorSelection> = self
            .selections
            .iter()
            .map(|selection| {
                Self::collapse_for_intent(
                    requested_intent.unwrap_or(EditIntent::Atomic),
                    *selection,
                    &(selection.start..selection.end),
                )
            })
            .collect();
        let group = sorted.len() > 1;
        if group {
            self.undo_manager.begin_transaction();
        }

        let mut recorded = false;
        for (range, new_text) in &sorted {
            let old_text = self.text.clone();
            self.text.replace(range.clone(), new_text);

            M::adjust_annotations(self, range, new_text.len());
            recorded |= self.push_history(
                &old_text,
                range,
                new_text,
                requested_intent,
                selection_before,
                None,
            );

            // Incremental, single-range updates must run per edit.
            self.display_map
                .adjust_folds_for_edit(&old_text, range, new_text);
            self.display_map
                .on_text_changed(&self.text, range, &Rope::from(*new_text), cx);

            self.mode.update_highlighter(
                super::mode::HighlighterUpdate {
                    selected_range: range,
                    old_text: &old_text,
                    new_text: &self.text,
                    change_text: new_text,
                    force: true,
                },
                window,
                cx,
            );

            self.update_fold_candidates_incremental(range, new_text);
        }

        if group {
            self.undo_manager.commit_transaction();
        }

        // One observable update per batch instead of one per edit.
        if let Some(diagnostics) = self.mode.diagnostics_mut() {
            diagnostics.reset(&self.text)
        }
        M::refresh_language_features(self, window, cx);
        self.update_search(cx);

        // Compute the resulting cursors.
        // One collapsed cursor per edit, at the end of its inserted text.
        let mut ascending: Vec<(Range<usize>, &str)> = sorted.clone();
        ascending.sort_by_key(|edit| edit.0.start);
        let text_len = self.text.len();
        let mut delta: isize = 0;
        let mut edit_results = Vec::with_capacity(ascending.len());
        for (range, new_text) in &ascending {
            let offset = ((range.start as isize + delta) as usize + new_text.len()).min(text_len);
            edit_results.push((range.clone(), offset));
            delta += new_text.len() as isize - (range.end as isize - range.start as isize);
        }

        let mut used = vec![false; edit_results.len()];
        let mut new_selections: Vec<CursorSelection> = Vec::with_capacity(ascending.len());
        for selection in original_selections {
            if let Ok(index) = edit_results
                .binary_search_by_key(&(selection.start, selection.end), |(range, _)| {
                    (range.start, range.end)
                })
                && !used[index]
            {
                let offset = edit_results[index].1;
                used[index] = true;
                let mut selection = selection;
                selection.place_at(offset, None);
                new_selections.push(selection);
            }
        }
        for (index, (_, offset)) in edit_results.into_iter().enumerate() {
            if !used[index] {
                new_selections.push(CursorSelection::new(
                    self.selections.generate_id(),
                    offset,
                    offset,
                ));
            }
        }
        self.selections.replace_all(new_selections);
        self.selections.merge_overlapping();

        // Record the cursor snapshots for undo/redo restore.
        let selections_after: Vec<CursorSelection> = self.selections.iter().copied().collect();
        if recorded {
            self.undo_manager
                .record_selections(selections_before, selections_after);
        }

        self.ime_marked_range.take();
        self.update_preferred_column();
        if self.is_multi_line() {
            self.mode.update_auto_grow(&self.display_map);
        }
        if self.emit_events {
            cx.emit(InputEvent::Change);
        }
        cx.notify();
    }

    /// Update fold candidates from tree-sitter syntax tree (full extraction).
    /// Used only on initial load or language changes.
    fn update_fold_candidates(&mut self) {
        if !self.mode.is_folding() {
            return;
        }

        let Some(highlighter_rc) = self.mode.highlighter() else {
            return;
        };

        let highlighter = highlighter_rc.borrow();
        let Some(highlighter) = highlighter.as_ref() else {
            return;
        };

        let fold_ranges = highlighter.fold_ranges(&self.text);
        self.display_map.set_fold_candidates(fold_ranges);
    }

    /// Incrementally update fold candidates after a text edit.
    /// Only traverses the edited region of the syntax tree instead of the full tree.
    fn update_fold_candidates_incremental(&mut self, edit_range: &Range<usize>, new_text: &str) {
        if !self.mode.is_folding() {
            return;
        }

        let Some(highlighter_rc) = self.mode.highlighter() else {
            return;
        };

        let highlighter = highlighter_rc.borrow();
        let Some(highlighter) = highlighter.as_ref() else {
            return;
        };

        // The new byte range in the updated text after the edit
        let new_end = edit_range.start + new_text.len();
        self.display_map.update_fold_candidates_for_edit(
            |range, text| highlighter.fold_ranges_for_edit(range, text),
            edit_range.start..new_end,
            &self.text,
        );
    }
}

impl<M: InputModeKind> EntityInputHandler for InputBaseState<M> {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        adjusted_range.replace(self.range_to_utf16(&range));
        Some(self.text.slice(range).to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range()),
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.ime_marked_range
            .map(|range| self.range_to_utf16(&range.into()))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.ime_marked_range = None;
        self.undo_manager.commit_transaction();
    }

    /// Replace text in range.
    ///
    /// - If the new text is invalid, it will not be replaced.
    /// - If `range_utf16` is not provided, the current selected range will be used.
    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let requested_intent = self.undo_manager.take_pending_intent();
        if !self.is_editable() {
            return;
        }
        let selection_before = *self.active_selection();
        // Committing a composition ends the transaction it opened, whether or
        // not the platform follows up with `unmark_text`.
        let ends_composition = self.ime_marked_range.is_some();

        self.pause_blink_cursor(cx);

        // NOTE: The normalization keeps the UTF-16 length, but may change the
        // UTF-8 byte length, so all the byte-offset calculations below must
        // use the normalized text.
        let new_text = self.normalize_input(new_text);
        let new_text: &str = &new_text;

        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.ime_marked_range.map(|range| {
                let range = self.range_to_utf16(&(range.start..range.end));
                self.range_from_utf16(&range)
            }))
            .unwrap_or(self.selected_range());

        if self.is_multi_line() {
            let multi_cursor = range_utf16.is_none()
                && self.ime_marked_range.is_none()
                && !self.selections.is_single();
            if multi_cursor {
                self.selections.merge_overlapping();
                let mut edits: Vec<(Range<usize>, String)> = self
                    .selections
                    .iter()
                    .map(|sel| (sel.start..sel.end, new_text.to_string()))
                    .collect();
                edits.sort_by_key(|(range, _)| range.start);
                // One keystroke across several cursors is one batch, committed
                // with the intent of the keystroke so a run of them coalesces
                // into one undo just like single-cursor typing.
                let intent =
                    requested_intent.unwrap_or_else(|| self.typing_intent(&edits, new_text));
                self.undo_manager.begin_transaction_with(intent);
                self.undo_manager.set_pending_intent(intent);
                self.replace_text_in_ranges(&edits, window, cx);
                self.undo_manager.commit_transaction();
            } else {
                if range_utf16.is_some() {
                    self.selections.remove_all_but_active();
                }
                if let Some(intent) = requested_intent {
                    self.undo_manager.set_pending_intent(intent);
                }
                self.replace_text_in_ranges(&[(range.clone(), new_text.to_string())], window, cx);
            }
            if ends_composition {
                self.undo_manager.commit_transaction();
            }

            if !self.silent_replace_text {
                M::on_text_typed(self, &range, new_text, window, cx);
            }
            if self.emit_events {
                // The marker was placed on a layout this change replaces; it
                // comes back where the pointer is on the next move. The
                // multi-line path returns here, so it has to clear it itself.
                self.table_marker = None;
            }
            return;
        }

        // Single-line path
        let old_text = self.text.clone();
        self.text.replace(range.clone(), new_text);

        let mut new_offset = (range.start + new_text.len()).min(self.text.len());

        // True if the mask has changed the text, e.g. regrouping the
        // separators or completing a leading dot.
        let mut mask_changed = false;

        if self.is_single_line() {
            let pending_text = self.text.to_string();
            // Check if the new text is valid.
            //
            // Only reject the edit if the old text was valid, to avoid
            // trapping a pre-existing invalid text (e.g. a `default_value`
            // that does not conform), the user can still edit to fix it.
            if !self.is_valid_input(&pending_text, cx)
                && self.is_valid_input(&old_text.to_string(), cx)
            {
                self.text = old_text;
                return;
            }

            if !self.mask_pattern.is_none() {
                let mask_text = self.mask_pattern.mask(&pending_text);
                mask_changed = mask_text.as_str() != pending_text;
                self.text = Rope::from(mask_text.as_str());
                let new_text_len =
                    (new_text.len() + mask_text.len()).saturating_sub(pending_text.len());
                new_offset = (range.start + new_text_len).min(mask_text.len());
            }
        }

        if mask_changed {
            // Masking rewrites the whole document, so ranges recorded against
            // the old text no longer point at anything.
            M::reset_annotations(self);
        } else {
            M::adjust_annotations(self, &range, new_text.len());
        }
        if mask_changed {
            // A segment-based history entry no longer matches the masked
            // document, record a whole-document change instead, so that
            // undo/redo can restore the text exactly.
            self.push_history(
                &old_text,
                &(0..old_text.len()),
                &self.text.to_string(),
                Some(EditIntent::Atomic),
                selection_before,
                Some((new_offset..new_offset).into()),
            );
        } else {
            self.push_history(
                &old_text,
                &range,
                &new_text,
                requested_intent,
                selection_before,
                None,
            );
        }
        if let Some(diagnostics) = self.mode.diagnostics_mut() {
            diagnostics.reset(&self.text)
        }
        // Adjust folds before updating wrap map: remove overlapping folds and shift others
        self.display_map
            .adjust_folds_for_edit(&old_text, &range, new_text);
        self.display_map
            .on_text_changed(&self.text, &range, &Rope::from(new_text), cx);

        self.mode.update_highlighter::<M>(
            super::mode::HighlighterUpdate {
                selected_range: &range,
                old_text: &old_text,
                new_text: &self.text,
                change_text: &new_text,
                force: true,
            },
            window,
            cx,
        );

        self.update_fold_candidates_incremental(&range, new_text);
        M::refresh_language_features(self, window, cx);
        self.set_cursor_to(new_offset);
        self.ime_marked_range.take();
        // A commit ends the IME composition: macOS delivers `insertText:` for
        // the confirmed candidate without a following `unmarkText`, so close
        // the transaction here. Leaving it open would keep merging every later
        // edit into the same change, which then carries the text and selection
        // of the first composition.
        if ends_composition {
            self.undo_manager
                .record_selections(vec![selection_before], vec![*self.active_selection()]);
            self.undo_manager.commit_transaction();
        }
        self.update_preferred_column();
        self.update_search(cx);
        if self.is_multi_line() {
            self.mode.update_auto_grow(&self.display_map);
        }
        if !self.silent_replace_text {
            M::on_text_typed(self, &range, &new_text, window, cx);
        }
        if self.emit_events {
            // The marker was placed on a layout this change replaces; it
            // comes back where the pointer is on the next move.
            self.table_marker = None;
            cx.emit(InputEvent::Change);
        }
        cx.notify();
    }

    /// Mark text is the IME temporary insert on typing.
    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let requested_intent = self.undo_manager.take_pending_intent();
        if !self.is_editable() {
            return;
        }
        let selection_before = *self.active_selection();

        let starts_composition = self.ime_marked_range.is_none();
        if starts_composition {
            self.undo_manager.begin_transaction();
        }

        // Collapse any extra cursors so we never leave stale secondary cursors behind.
        self.selections.remove_all_but_active();

        M::reset_language_features(self);

        // See the same NOTE in `replace_text_in_range`.
        let new_text = self.normalize_input(new_text);
        let new_text: &str = &new_text;

        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.ime_marked_range.map(|range| {
                let range = self.range_to_utf16(&(range.start..range.end));
                self.range_from_utf16(&range)
            }))
            .unwrap_or(self.selected_range());

        let old_text = self.text.clone();
        self.text.replace(range.clone(), new_text);

        if self.is_single_line() {
            let pending_text = self.text.to_string();
            // See the same NOTE in `replace_text_in_range`.
            if !self.is_valid_input(&pending_text, cx)
                && self.is_valid_input(&old_text.to_string(), cx)
            {
                self.text = old_text;
                if starts_composition {
                    self.undo_manager.commit_transaction();
                }
                return;
            }
        }

        M::adjust_annotations(self, &range, new_text.len());
        if let Some(diagnostics) = self.mode.diagnostics_mut() {
            diagnostics.reset(&self.text)
        }
        // Adjust folds before updating wrap map: remove overlapping folds and shift others
        self.display_map
            .adjust_folds_for_edit(&old_text, &range, new_text);
        self.display_map
            .on_text_changed(&self.text, &range, &Rope::from(new_text), cx);

        self.mode.update_highlighter::<M>(
            super::mode::HighlighterUpdate {
                selected_range: &range,
                old_text: &old_text,
                new_text: &self.text,
                change_text: &new_text,
                force: true,
            },
            window,
            cx,
        );

        self.update_fold_candidates_incremental(&range, new_text);
        M::refresh_language_features(self, window, cx);
        if new_text.is_empty() {
            // Cancel selection, when cancel IME input.
            self.set_cursor_to(range.start);
            self.ime_marked_range = None;
        } else {
            self.ime_marked_range = Some((range.start..range.start + new_text.len()).into());
            let new_range = new_selected_range_utf16
                .as_ref()
                .map(|range_utf16| {
                    let new_text = Rope::from(new_text);
                    range.start + new_text.offset_utf16_to_offset(range_utf16.start)
                        ..range.start + new_text.offset_utf16_to_offset(range_utf16.end)
                })
                .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
            self.set_selection(new_range.start, new_range.end);
        }
        if self.is_multi_line() {
            self.mode.update_auto_grow(&self.display_map);
        }
        if self.push_history(
            &old_text,
            &range,
            new_text,
            requested_intent,
            selection_before,
            Some(*self.active_selection()),
        ) {
            self.undo_manager
                .record_selections(vec![selection_before], vec![*self.active_selection()]);
        }
        if new_text.is_empty() {
            self.undo_manager.commit_transaction();
        }
        cx.notify();
    }

    /// Used to position IME candidates.
    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let last_layout = self.last_layout.as_ref()?;
        let line_height = last_layout.line_height;
        let line_number_width = last_layout.line_number_width;
        let range = self.range_from_utf16(&range_utf16);

        let mut start_origin = None;
        let mut end_origin = None;
        let line_number_origin = point(line_number_width, px(0.));
        let mut y_offset = last_layout.visible_top;

        for (vi, line) in last_layout.lines.iter().enumerate() {
            if start_origin.is_some() && end_origin.is_some() {
                break;
            }

            let index_offset = last_layout.visible_line_byte_offsets[vi];

            if start_origin.is_none() {
                if let Some(p) = line.position_for_index(
                    range.start.saturating_sub(index_offset),
                    last_layout,
                    false,
                ) {
                    start_origin = Some(p + point(px(0.), y_offset));
                }
            }

            if end_origin.is_none() {
                if let Some(p) = line.position_for_index(
                    range.end.saturating_sub(index_offset),
                    last_layout,
                    false,
                ) {
                    end_origin = Some(p + point(px(0.), y_offset));
                }
            }

            y_offset += line.size(line_height).height;
        }

        let start_origin = start_origin.unwrap_or_default();
        let mut end_origin = end_origin.unwrap_or_default();
        // Ensure at same line.
        end_origin.y = start_origin.y;

        Some(Bounds::from_corners(
            bounds.origin + line_number_origin + start_origin,
            // + line_height for show IME panel under the cursor line.
            bounds.origin + line_number_origin + point(end_origin.x, end_origin.y + line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let last_layout = self.last_layout.as_ref()?;
        let line_point = self.last_bounds?.localize(&point)?;

        for (vi, line) in last_layout.lines.iter().enumerate() {
            let offset = last_layout.visible_line_byte_offsets[vi];
            if let Some(utf8_index) = line.index_for_position(line_point, last_layout) {
                return Some(self.offset_to_utf16(offset + utf8_index));
            }
        }

        None
    }
}

impl<M: InputModeKind> Focusable for InputBaseState<M> {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl<M: InputModeKind> Render for InputBaseState<M> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Before anything reads it: the element resolves this style during
        // layout and paint, and both happen after this call in the same frame.
        self.editor_style = self
            .projected_editor_style
            .resolved(&crate::Theme::global(cx).tokens);
        let entity = cx.entity();
        if self._pending_update {
            self.mode.update_highlighter::<M>(
                super::mode::HighlighterUpdate {
                    selected_range: &(0..0),
                    old_text: &self.text,
                    new_text: &self.text,
                    change_text: "",
                    force: false,
                },
                window,
                cx,
            );

            self.update_fold_candidates();
            M::refresh_language_features(self, window, cx);
            self._pending_update = false;
        }

        let element = div()
            .id("input-state")
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .when(self.is_editable(), |this| {
                this.on_action(window.listener_for(&entity, InputBaseState::backspace))
                    .on_action(window.listener_for(&entity, InputBaseState::delete))
                    .on_action(
                        window.listener_for(&entity, InputBaseState::delete_to_beginning_of_line),
                    )
                    .on_action(window.listener_for(&entity, InputBaseState::delete_to_end_of_line))
                    .on_action(window.listener_for(&entity, InputBaseState::delete_previous_word))
                    .on_action(window.listener_for(&entity, InputBaseState::delete_next_word))
                    .on_action(window.listener_for(&entity, InputBaseState::enter))
                    .on_action(window.listener_for(&entity, InputBaseState::escape))
                    .on_action(window.listener_for(&entity, InputBaseState::paste))
                    .on_action(window.listener_for(&entity, InputBaseState::cut))
                    .on_action(window.listener_for(&entity, InputBaseState::undo))
                    .on_action(window.listener_for(&entity, InputBaseState::redo))
                    .when(self.is_multi_line(), |this| {
                        this.on_action(window.listener_for(&entity, InputBaseState::indent_inline))
                            .on_action(window.listener_for(&entity, InputBaseState::outdent_inline))
                            .on_action(window.listener_for(&entity, InputBaseState::indent_block))
                            .on_action(window.listener_for(&entity, InputBaseState::outdent_block))
                    })
            })
            .on_action(window.listener_for(&entity, InputBaseState::left))
            .on_action(window.listener_for(&entity, InputBaseState::right))
            .on_action(window.listener_for(&entity, InputBaseState::select_left))
            .on_action(window.listener_for(&entity, InputBaseState::select_right))
            .when(self.is_multi_line(), |this| {
                this.on_action(window.listener_for(&entity, InputBaseState::up))
                    .on_action(window.listener_for(&entity, InputBaseState::down))
                    .on_action(window.listener_for(&entity, InputBaseState::select_up))
                    .on_action(window.listener_for(&entity, InputBaseState::select_down))
                    .on_action(window.listener_for(&entity, InputBaseState::page_up))
                    .on_action(window.listener_for(&entity, InputBaseState::page_down))
                    .on_action(window.listener_for(&entity, InputBaseState::add_cursor_above))
                    .on_action(window.listener_for(&entity, InputBaseState::add_cursor_below))
            })
            .on_action(window.listener_for(&entity, InputBaseState::on_action_select_all))
            .on_action(window.listener_for(&entity, InputBaseState::select_to_start_of_line))
            .on_action(window.listener_for(&entity, InputBaseState::select_to_end_of_line))
            .on_action(window.listener_for(&entity, InputBaseState::select_to_previous_word))
            .on_action(window.listener_for(&entity, InputBaseState::select_to_next_word))
            .on_action(window.listener_for(&entity, InputBaseState::home))
            .on_action(window.listener_for(&entity, InputBaseState::end))
            .on_action(window.listener_for(&entity, InputBaseState::move_to_start))
            .on_action(window.listener_for(&entity, InputBaseState::move_to_end))
            .on_action(window.listener_for(&entity, InputBaseState::move_to_previous_word))
            .on_action(window.listener_for(&entity, InputBaseState::move_to_next_word))
            .on_action(window.listener_for(&entity, InputBaseState::select_to_start))
            .on_action(window.listener_for(&entity, InputBaseState::select_to_end))
            .on_action(window.listener_for(&entity, InputBaseState::show_character_palette))
            .on_action(window.listener_for(&entity, InputBaseState::copy))
            .on_action(window.listener_for(&entity, InputBaseState::on_action_search))
            .on_action(window.listener_for(&entity, InputBaseState::on_action_replace))
            .on_mouse_down(
                MouseButton::Left,
                window.listener_for(&entity, InputBaseState::on_mouse_down),
            )
            .on_mouse_down(
                MouseButton::Right,
                window.listener_for(&entity, InputBaseState::on_mouse_down),
            )
            .on_mouse_up(
                MouseButton::Left,
                window.listener_for(&entity, InputBaseState::on_mouse_up),
            )
            .on_mouse_up(
                MouseButton::Right,
                window.listener_for(&entity, InputBaseState::on_mouse_up),
            )
            .on_mouse_move(window.listener_for(&entity, InputBaseState::on_mouse_move))
            .on_scroll_wheel(window.listener_for(&entity, InputBaseState::on_scroll_wheel))
            .when(self.is_multi_line() && !self.disabled, |this| {
                this.on_modifiers_changed(cx.listener(|_, _, _, cx| cx.notify()))
            })
            .when(!self.disabled, |this| {
                if self.is_multi_line() && window.modifiers().alt {
                    this.cursor_crosshair()
                } else {
                    this.cursor_text()
                }
            })
            .flex_1()
            .when(self.is_multi_line(), |this| this.h_full())
            .flex_grow_1()
            .overflow_x_hidden()
            .when(self.is_multi_line(), |this| {
                this.pt(self.editor_paddings.top)
                    .pr(self.editor_paddings.right)
                    .pb(self.editor_paddings.bottom)
                    .pl(self.editor_paddings.left)
            })
            .child(TextElement::new(entity.clone()).placeholder(self.placeholder.clone()))
            .when(self.shows_scrollbar(), |this| {
                this.child(EditorScrollbar::new(entity.clone()))
            });

        // Actions only one mode handles are registered by that mode, where
        // `Self` is concrete enough to name its own entity type.
        M::register_actions(element, &entity, window)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::theme::Theme;
    use gpui::{HighlightStyle, TestAppContext, VisualTestContext};

    use crate::input::{EditorMode, InputMode, TextareaMode};

    struct TestRoot<M: InputModeKind>(Entity<InputBaseState<M>>);

    impl<M: InputModeKind> Render for TestRoot<M> {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.0.clone())
        }
    }

    struct InputView<M: InputModeKind> {
        input: Entity<InputBaseState<M>>,
        window_handle: gpui::WindowHandle<TestRoot<M>>,
    }

    /// Helper to open a state of one mode in a window for testing.
    impl<M: InputModeKind> InputView<M> {
        fn build_with(
            cx: &mut TestAppContext,
            make: impl FnOnce(&mut Window, &mut Context<InputBaseState<M>>) -> InputBaseState<M>
            + 'static,
        ) -> Self {
            let mut input: Option<Entity<InputBaseState<M>>> = None;

            let window = cx.update(|cx| {
                cx.open_window(Default::default(), |window, cx| {
                    // Set up the theme first
                    cx.set_global(Theme::default());
                    // Initialize input keybindings
                    super::super::init(cx);

                    input = Some(cx.new(|cx| make(window, cx)));

                    cx.new(|_| TestRoot(input.clone().unwrap()))
                })
                .unwrap()
            });

            Self {
                input: input.clone().unwrap(),
                window_handle: window,
            }
        }
    }

    impl InputView<EditorMode> {
        /// An editor state, for the tests that exercise code-editor behavior.
        fn new(cx: &mut TestAppContext) -> Self {
            Self::build_editor(cx, |state| state)
        }

        fn build_editor(
            cx: &mut TestAppContext,
            f: impl FnOnce(InputBaseState<EditorMode>) -> InputBaseState<EditorMode> + 'static,
        ) -> Self {
            Self::build_with(cx, move |window, cx| {
                f(crate::input::EditorState::new(window, cx).language("sql"))
            })
        }
    }

    impl InputView<TextareaMode> {
        fn build_textarea(
            cx: &mut TestAppContext,
            f: impl FnOnce(InputBaseState<TextareaMode>) -> InputBaseState<TextareaMode> + 'static,
        ) -> Self {
            Self::build_with(cx, move |window, cx| {
                f(crate::input::TextareaState::new(window, cx))
            })
        }
    }

    impl InputView<InputMode> {
        /// A single-line state, the default these tests were written against.
        fn build(
            cx: &mut TestAppContext,
            f: impl FnOnce(InputBaseState<InputMode>) -> InputBaseState<InputMode> + 'static,
        ) -> Self {
            Self::build_with(cx, move |window, cx| {
                f(crate::input::InputState::new(window, cx))
            })
        }
    }

    /// A checkbox widget reports its click by editing the text, so the
    /// document stays the only source of truth.
    #[gpui::test]
    fn toggling_a_task_marker_edits_the_text(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let input_view = InputView::new(cx);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_value("- [ ] milk", window, cx);
                state.toggle_task_marker(2..5, false, window, cx);
            })
        });
        assert_eq!(input.read_with(&cx, |state, _| state.value()), "- [x] milk");

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.toggle_task_marker(2..5, true, window, cx)
            })
        });
        assert_eq!(input.read_with(&cx, |state, _| state.value()), "- [ ] milk");
    }

    /// The range came from a layout that may be a frame or two old. If the text
    /// moved under it, editing blind would corrupt whatever is there now — the
    /// one way a decorative widget could damage a note.
    #[gpui::test]
    fn a_stale_task_range_is_refused(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let input_view = InputView::new(cx);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_value("some other text here", window, cx);
                // 2..5 is `me ` now, not a marker.
                state.toggle_task_marker(2..5, false, window, cx);
            })
        });
        assert_eq!(
            input.read_with(&cx, |state, _| state.value()),
            "some other text here",
            "the text was left alone"
        );
    }

    #[gpui::test]
    fn only_a_multi_line_input_paints_scrollbars(cx: &mut TestAppContext) {
        cx.update(crate::init);

        // A single-line input keeps its caret in view by moving its own offset;
        // it has no viewport to drag, so a scrollbar in a text field is a
        // control that does not exist.
        let single = InputView::build(cx, |state| state);
        single
            .input
            .update(cx, |state, _| assert!(!state.shows_scrollbar()));

        let multi = InputView::build_textarea(cx, |state| state);
        multi
            .input
            .update(cx, |state, _| assert!(state.shows_scrollbar()));
    }

    #[gpui::test]
    fn context_menu_handler_is_deferred_and_respects_disabled(cx: &mut TestAppContext) {
        use std::{cell::Cell, rc::Rc};
        cx.update(crate::init);
        let input_view = InputView::new(cx);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;
        let calls = Rc::new(Cell::new(0usize));
        let items = Rc::new(Cell::new(0usize));

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                let calls2 = calls.clone();
                let items2 = items.clone();
                state.on_context_menu(Rc::new(move |menu, _, _, _, _| {
                    calls2.set(calls2.get() + 1);
                    items2.set(menu.items.len());
                }));
                state.handle_right_click_menu(point(px(0.), px(0.)), 0, window, cx);
            })
        });
        assert_eq!(calls.get(), 1);
        assert_eq!(items.get(), 0);

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.disabled = true;
                state.handle_right_click_menu(point(px(0.), px(0.)), 0, window, cx);
            })
        });
        assert_eq!(calls.get(), 1);
    }

    #[gpui::test]
    fn test_readonly_rejects_user_edits_only(cx: &mut TestAppContext) {
        let input_view = InputView::new(cx);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_value("hello", window, cx);
                state.set_readonly(true, cx);
            });
        });

        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                assert!(!state.is_editable());
                assert!(!state.is_replaceable());
            });
        });

        // Typing (and IME) goes through the input handler, it must be rejected.
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, " world", window, cx);
                state.replace_and_mark_text_in_range(None, "あ", None, window, cx);
            });
        });
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| assert_eq!(state.value(), "hello"));
        });

        // The programmatic APIs are not limited by the readonly mode.
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.insert(" world", window, cx);
                state.set_value("changed", window, cx);
            });
        });
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| assert_eq!(state.value(), "changed"));
        });

        // And the user can edit again after leaving the readonly mode.
        // The caret is at the start, because `set_value` has reset the selection.
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_readonly(false, cx);
                state.replace_text_in_range(None, "!", window, cx);
            });
        });
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                assert!(state.is_editable());
                assert_eq!(state.value(), "!changed");
            });
        });
    }

    /// Regression test: `scroll_to` at end-of-buffer must produce a deferred
    /// scroll target within the safe scroll range, so the painted frame
    /// matches what `update_scroll_offset` persists (no jitter). A small
    /// `cursor_surrounding_lines` override used to mismatch the hardcoded
    /// 3-line edge clearance in `scroll_to`, overshooting `safe_y_min`.
    #[gpui::test]
    fn test_scroll_to_eob_does_not_overshoot_safe_range(cx: &mut TestAppContext) {
        let input_view = InputView::new(cx);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        // JetBrains-style: 1 trailing empty row + 1-line cursor surrounding.
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_scroll_beyond_last_line(Some(1), window, cx);
                state.set_cursor_surrounding_lines(Some(1), window, cx);
                let text: String = (1..=50)
                    .map(|i| format!("line {i}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                state.set_value(text, window, cx);
            });
        });
        cx.run_until_parked();

        // Sanity: paint populated `scroll_size` and `input_bounds` — without
        // these, `safe_y_min` below collapses to 0 and the assertion is vacuous.
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                assert!(
                    state.scroll_size.height > px(0.),
                    "scroll_size not populated by initial paint"
                );
                assert!(
                    state.input_bounds.size.height > px(0.),
                    "input_bounds not populated by initial paint"
                );
            });
        });

        // Move cursor to end with downward direction — same code path as a
        // `Down` keystroke at EOB. `scroll_to` runs synchronously inside
        // `move_to`; inspect `deferred_scroll_offset` in the same closure
        // before the next paint consumes and clears it.
        cx.update(|_, cx| {
            input.update(cx, |state, cx| {
                let end = state.text.len();
                state.move_to(end, Some(MoveDirection::Down), cx);

                let deferred = state
                    .deferred_scroll_offset
                    .expect("scroll_to should populate deferred_scroll_offset");
                let safe_y_min =
                    (-state.scroll_size.height + state.input_bounds.size.height).min(px(0.));

                assert!(
                    deferred.y >= safe_y_min,
                    "deferred_scroll_offset.y = {:?} below safe_y_min = {:?} \
                     — paint would jitter (Bug C regression)",
                    deferred.y,
                    safe_y_min,
                );
            });
        });
    }

    #[gpui::test]
    fn test_next_search_match_reveals_with_padding_after_manual_scroll(cx: &mut TestAppContext) {
        assert_search_reveals_with_padding_after_manual_scroll(false, cx);
    }

    #[gpui::test]
    fn test_previous_search_match_reveals_with_padding_after_manual_scroll(
        cx: &mut TestAppContext,
    ) {
        assert_search_reveals_with_padding_after_manual_scroll(true, cx);
    }

    fn assert_search_reveals_with_padding_after_manual_scroll(
        previous: bool,
        cx: &mut TestAppContext,
    ) {
        let input_view = InputView::new(cx);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;
        let text = (0..160)
            .map(|row| {
                if matches!(row, 20 | 60 | 100) {
                    format!("match on row {row}")
                } else {
                    format!("line {row}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let start = text.match_indices("match").nth(1).unwrap().0;
        let expected_match = start..start + "match".len();
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_cursor_surrounding_lines(Some(3), window, cx);
                state.set_value(text, window, cx);
                state.set_search_query("match", true, cx);
                if previous {
                    state.search_session.matcher.next();
                    state.search_session.matcher.next();
                }
            });
        });
        cx.run_until_parked();
        cx.update(|_, cx| {
            input.update(cx, |state, cx| {
                let line_height = state.last_layout.as_ref().unwrap().line_height;
                let y = if previous { px(0.) } else { -line_height * 80. };
                state.set_scroll_offset(point(px(0.), y), cx);
            });
        });
        cx.run_until_parked();
        input.read_with(&cx, |state, _| {
            let visible = state.visible_row_range().unwrap();
            if previous {
                assert!(visible.end <= 60, "target must be below the viewport");
            } else {
                assert!(visible.start > 60, "target must be above the viewport");
            }
        });
        cx.update(|_, cx| {
            input.update(cx, |state, cx| {
                let range = if previous {
                    state.previous_search_match(cx)
                } else {
                    state.next_search_match(cx)
                };
                assert_eq!(range, Some(expected_match));
                assert_eq!(state.search_session.matcher.label(), "2/3");
            });
        });
        cx.run_until_parked();
        input.read_with(&cx, |state, _| {
            assert!(state.visible_row_range().unwrap().contains(&60));
            let line_height = state.last_layout.as_ref().unwrap().line_height;
            let target_y = line_height * 60. + state.scroll_handle.offset().y;
            // Three lines of edge clearance include the matched line itself.
            assert!(target_y >= line_height * 2. - px(0.1));
            assert!(
                target_y + line_height * 3.
                    <= state.last_bounds.as_ref().unwrap().size.height + px(0.1),
                "search must preserve the configured surrounding-line padding"
            );
        });
    }

    #[gpui::test]
    fn test_number_step(cx: &mut TestAppContext) {
        let input = InputView::build(cx, |state| state).input;

        cx.update(|cx| {
            input.update(cx, |_state, cx| {
                assert_eq!(
                    NumberStep::from(5.).value(123., StepAction::Increment, cx),
                    5.
                );

                // The step can differ by direction at a boundary: at 1.0 it
                // is 0.1 going down and 0.5 going up.
                let step = NumberStep::by_value(|value, action, _cx| {
                    let below = match action {
                        StepAction::Increment => value < 1.0,
                        StepAction::Decrement => value <= 1.0,
                    };
                    if below { 0.1 } else { 0.5 }
                });
                assert_eq!(step.value(0.5, StepAction::Increment, cx), 0.1);
                assert_eq!(step.value(1.0, StepAction::Increment, cx), 0.5);
                assert_eq!(step.value(1.0, StepAction::Decrement, cx), 0.1);
                assert_eq!(step.value(2.0, StepAction::Decrement, cx), 0.5);
            });
        });
    }

    #[gpui::test]
    fn test_number_input_normalization(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| {
            state.mask_pattern(MaskPattern::Number {
                separator: None,
                fraction: None,
            })
        });
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        // Full-width digits and the ideographic full stop are normalized,
        // and the cursor is at the end (in normalized bytes, not the
        // original 12 bytes).
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "12。5", window, cx);
            });
        });
        cx.run_until_parked();
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                assert_eq!(state.value(), "12.5");
                let cursor: Range<usize> = state.selected_range();
                assert_eq!(cursor, 4..4);
            });
        });

        // Non-numeric input is rejected.
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "abc", window, cx);
            });
        });
        cx.run_until_parked();
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                assert_eq!(state.value(), "12.5");
            });
        });

        // A bare leading dot is kept as-is (normalized from the ideographic
        // full stop), not completed to "0.", so it stays editable.
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                let range = state.range_to_utf16(&(0..state.text.len()));
                state.replace_text_in_range(Some(range), "。", window, cx);
            });
        });
        cx.run_until_parked();
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                assert_eq!(state.value(), ".");
                let cursor: Range<usize> = state.selected_range();
                assert_eq!(cursor, 1..1);
            });
        });
    }

    #[gpui::test]
    fn test_number_input_normalization_with_separator(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| {
            state.mask_pattern(MaskPattern::Number {
                separator: Some(','),
                fraction: Some(2),
            })
        });
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "1234", window, cx);
            });
        });
        cx.run_until_parked();
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                assert_eq!(state.value(), "1,234");
                assert_eq!(state.unmask_value(), "1234");
            });
        });
    }

    #[gpui::test]
    fn test_number_input_clamp_on_blur(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| {
            state
                .mask_pattern(MaskPattern::Number {
                    separator: None,
                    fraction: None,
                })
                .min(10.)
                .max(100.)
        });
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        // Out-of-range values are allowed while typing, and clamped on blur.
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "1000", window, cx);
                assert_eq!(state.value(), "1000");
                state.clamp_number_value(window, cx);
                assert_eq!(state.value(), "100");

                let range = state.range_to_utf16(&(0..state.text.len()));
                state.replace_text_in_range(Some(range), "1", window, cx);
                assert_eq!(state.value(), "1");
                state.clamp_number_value(window, cx);
                assert_eq!(state.value(), "10");
            });
        });
    }

    #[gpui::test]
    fn test_number_input_undo_with_mask(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| {
            state.mask_pattern(MaskPattern::Number {
                separator: Some(','),
                fraction: None,
            })
        });
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        // When the mask changes the text (regrouping separators), a
        // whole-document change is recorded, so undo/redo can restore it.
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "1234", window, cx);
                assert_eq!(state.value(), "1,234");
                state.replace_text_in_range(None, "5", window, cx);
                assert_eq!(state.value(), "12,345");

                // Each whole-document mask rewrite is an atomic undo step.
                // Before the whole-document history fix, undo produced a
                // corrupted value like "1,2344".
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "1,234");
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "");
                state.redo(&Redo, window, cx);
                assert_eq!(state.value(), "1,234");
                state.redo(&Redo, window, cx);
                assert_eq!(state.value(), "12,345");
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_coalesces_adjacent_typing_transactions(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "a", window, cx);
                state.replace_text_in_range(None, "b", window, cx);
                assert_eq!(state.value(), "ab");

                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "");
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_cursor_movement_splits_typing(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "a", window, cx);
                state.replace_text_in_range(None, "b", window, cx);
                state.left(&MoveLeft, window, cx);
                state.replace_text_in_range(None, "x", window, cx);
                assert_eq!(state.value(), "axb");

                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "ab");
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "");
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_splits_backward_and_forward_delete(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_value("abcd", window, cx);
                state.set_selected_range(2..2, cx);
                state.backspace(&Backspace, window, cx);
                state.delete(&Delete, window, cx);
                assert_eq!(state.value(), "ad");

                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "acd");
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "abcd");
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_coalesces_directional_character_deletes(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_value("abcd", window, cx);
                state.backspace(&Backspace, window, cx);
                state.backspace(&Backspace, window, cx);
                assert_eq!(state.value(), "ab");
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "abcd");
                assert_eq!(state.selected_range(), 4..4);

                state.set_value("abcd", window, cx);
                state.set_selected_range(1..1, cx);
                state.delete(&Delete, window, cx);
                state.delete(&Delete, window, cx);
                assert_eq!(state.value(), "ad");
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "abcd");
                assert_eq!(state.selected_range(), 1..1);
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_atomic_paste_isolated_from_typing(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string("P".to_string()));
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "a", window, cx);
                state.paste(&Paste, window, cx);
                state.replace_text_in_range(None, "b", window, cx);
                assert_eq!(state.value(), "aPb");

                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "aP");
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "a");
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "");
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_programmatic_insert_is_atomic(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "a", window, cx);
                state.insert("P", window, cx);
                state.replace_text_in_range(None, "b", window, cx);
                assert_eq!(state.value(), "aPb");

                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "aP");
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "a");
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_selection_round_trip_splits_typing(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "a", window, cx);
                state.replace_text_in_range(None, "b", window, cx);
                state.select_all(window, cx);
                state.unselect(window, cx);
                state.replace_text_in_range(None, "c", window, cx);

                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "ab");
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "");
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_enter_is_atomic(cx: &mut TestAppContext) {
        let input_view = InputView::build_textarea(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "a", window, cx);
                state.enter(
                    &Enter {
                        secondary: false,
                        shift: false,
                    },
                    window,
                    cx,
                );
                state.replace_text_in_range(None, "b", window, cx);

                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "a\n");
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "a");
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "");
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_single_line_return_commits_the_typing_session(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                for part in ["a", "b", "c"] {
                    state.replace_text_in_range(None, part, window, cx);
                }
                state.enter(
                    &Enter {
                        secondary: false,
                        shift: false,
                    },
                    window,
                    cx,
                );
                for part in ["d", "e", "f"] {
                    state.replace_text_in_range(None, part, window, cx);
                }
                assert_eq!(state.value(), "abcdef");

                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "abc");
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "");
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_submit_on_enter_commits_the_textarea_session(cx: &mut TestAppContext) {
        let input_view = InputView::build_textarea(cx, |state| state.submit_on_enter(true));
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "before submit", window, cx);
                state.enter(
                    &Enter {
                        secondary: false,
                        shift: false,
                    },
                    window,
                    cx,
                );
                state.replace_text_in_range(None, " after submit", window, cx);
                assert_eq!(state.value(), "before submit after submit");

                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "before submit");
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "");
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_blur_commits_the_typing_session(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "before blur", window, cx);
                state.on_blur(window, cx);
                state.on_focus(window, cx);
                state.replace_text_in_range(None, " after focus", window, cx);
                assert_eq!(state.value(), "before blur after focus");

                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "before blur");
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "");
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_keeps_rapid_lines_in_distinct_transactions(cx: &mut TestAppContext) {
        let input_view = InputView::build_textarea(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                let enter = Enter {
                    secondary: false,
                    shift: false,
                };
                state.replace_text_in_range(None, "a", window, cx);
                state.enter(&enter, window, cx);
                state.replace_text_in_range(None, "b", window, cx);
                state.enter(&enter, window, cx);
                state.replace_text_in_range(None, "c", window, cx);
                assert_eq!(state.value(), "a\nb\nc");

                for expected in ["a\nb\n", "a\nb", "a\n", "a", ""] {
                    state.undo(&Undo, window, cx);
                    assert_eq!(state.value(), expected);
                }
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_coalesces_long_unicode_typing_without_a_timer(cx: &mut TestAppContext) {
        let input_view = InputView::build_textarea(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;
        let parts = [
            "The ",
            "quick ",
            "brown fox, ",
            "你好，世界 ",
            "🦀 jumps over 13 lazy dogs.",
        ];
        let expected = parts.concat();

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                for part in parts {
                    state.replace_text_in_range(None, part, window, cx);
                }
                assert_eq!(state.value(), expected);

                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "");
                state.redo(&Redo, window, cx);
                assert_eq!(state.value(), expected);
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_long_multiline_sequence_has_structural_boundaries(
        cx: &mut TestAppContext,
    ) {
        let input_view = InputView::build_textarea(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;
        let enter = Enter {
            secondary: false,
            shift: false,
        };

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                for (index, line) in [
                    "first line with punctuation!",
                    "第二行包含 Unicode 🦀",
                    "third line has several words",
                ]
                .into_iter()
                .enumerate()
                {
                    for chunk in line.split_inclusive(' ') {
                        state.replace_text_in_range(None, chunk, window, cx);
                    }
                    if index < 2 {
                        state.enter(&enter, window, cx);
                    }
                }

                assert_eq!(
                    state.value(),
                    "first line with punctuation!\n第二行包含 Unicode 🦀\nthird line has several words"
                );
                state.undo(&Undo, window, cx);
                assert_eq!(
                    state.value(),
                    "first line with punctuation!\n第二行包含 Unicode 🦀\n"
                );
                state.undo(&Undo, window, cx);
                assert_eq!(
                    state.value(),
                    "first line with punctuation!\n第二行包含 Unicode 🦀"
                );
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "first line with punctuation!\n");
            });
        });
    }

    #[gpui::test]
    fn test_masked_input_keeps_its_value_out_of_the_clipboard(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_value("hunter2", window, cx);
                state.set_masked(true, window, cx);
                state.select_all(window, cx);
                cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into()));

                state.copy(&Copy, window, cx);
                assert_eq!(
                    cx.read_from_clipboard().and_then(|item| item.text()),
                    Some("sentinel".to_string())
                );

                // Cut neither copies nor deletes.
                state.cut(&Cut, window, cx);
                assert_eq!(state.value(), "hunter2");
                assert_eq!(
                    cx.read_from_clipboard().and_then(|item| item.text()),
                    Some("sentinel".to_string())
                );

                // Revealing the value restores both.
                state.set_masked(false, window, cx);
                state.copy(&Copy, window, cx);
                assert_eq!(
                    cx.read_from_clipboard().and_then(|item| item.text()),
                    Some("hunter2".to_string())
                );
            });
        });
    }

    #[gpui::test]
    fn test_masked_input_collapses_word_boundaries(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_value("aaa bbb ccc", window, cx);
                state.set_masked(true, window, cx);
                state.set_selected_range(7..7, cx);

                // The mask hides word boundaries, so a word delete takes
                // everything before the caret and leaves the rest.
                state.delete_previous_word(&DeleteToPreviousWordStart, window, cx);
                assert_eq!(state.value(), " ccc");
                assert_eq!(state.selected_range(), 0..0);

                state.delete_next_word(&DeleteToNextWordEnd, window, cx);
                assert_eq!(state.value(), "");

                // A double click takes the whole value, not one word.
                state.set_value("aaa bbb ccc", window, cx);
                state.select_word(9, window, cx);
                assert_eq!(state.selected_range(), 0..11);

                // Unmasked, the same delete only takes one word.
                state.set_masked(false, window, cx);
                state.set_value("aaa bbb ccc", window, cx);
                state.set_selected_range(11..11, cx);
                state.delete_previous_word(&DeleteToPreviousWordStart, window, cx);
                assert_eq!(state.value(), "aaa bbb ");

                state.set_value("aaa bbb ccc", window, cx);
                state.select_word(9, window, cx);
                assert_eq!(state.selected_range(), 8..11);
            });
        });
    }

    #[gpui::test]
    fn test_masked_input_disables_the_copy_context_menu_items(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_value("hunter2", window, cx);
                state.select_all(window, cx);
                assert!(state.context_menu_capabilities().is_copyable());

                state.set_masked(true, window, cx);
                let capabilities = state.context_menu_capabilities();
                assert!(capabilities.is_masked());
                assert!(capabilities.has_selection());
                assert!(!capabilities.is_copyable());
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_cut_and_repeated_pastes_are_distinct_transactions(
        cx: &mut TestAppContext,
    ) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "alpha beta gamma", window, cx);
                state.set_selected_range(6..10, cx);
                state.cut(&Cut, window, cx);
                assert_eq!(state.value(), "alpha  gamma");

                state.paste(&Paste, window, cx);
                state.paste(&Paste, window, cx);
                assert_eq!(state.value(), "alpha betabeta gamma");

                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "alpha beta gamma");
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "alpha  gamma");
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "alpha beta gamma");
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_word_and_line_deletes_do_not_coalesce(cx: &mut TestAppContext) {
        let input_view = InputView::build_textarea(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_value("one two three\nfour five", window, cx);
                state.set_selected_range(13..13, cx);
                state.delete_previous_word(&DeleteToPreviousWordStart, window, cx);
                assert_eq!(state.value(), "one two \nfour five");
                state.delete_to_end_of_line(&DeleteToEndOfLine, window, cx);
                assert_eq!(state.value(), "one two four five");

                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "one two \nfour five");
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "one two three\nfour five");
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_multiline_replacement_is_one_atomic_transaction(cx: &mut TestAppContext) {
        let input_view = InputView::build_textarea(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "before", window, cx);
                state.set_selected_range(0..6, cx);
                state.replace_text_in_range(None, "line one\nline two\n第三行", window, cx);
                assert_eq!(state.value(), "line one\nline two\n第三行");

                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "before");
                assert_eq!(state.selected_range(), 0..6);
                state.redo(&Redo, window, cx);
                assert_eq!(state.value(), "line one\nline two\n第三行");
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_composition_isolated_from_long_typing(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "prefix ", window, cx);
                state.replace_and_mark_text_in_range(None, "n", None, window, cx);
                state.replace_and_mark_text_in_range(None, "ni", None, window, cx);
                state.replace_and_mark_text_in_range(None, "你", None, window, cx);
                state.unmark_text(window, cx);
                state.replace_text_in_range(None, " suffix", window, cx);
                assert_eq!(state.value(), "prefix 你 suffix");

                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "prefix 你");
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "prefix ");
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "");
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_selected_replacement_is_atomic(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "abc", window, cx);
                state.set_selected_range(1..2, cx);
                state.replace_text_in_range(None, "X", window, cx);
                state.replace_text_in_range(None, "z", window, cx);
                assert_eq!(state.value(), "aXzc");

                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "aXc");
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "abc");
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "");
            });
        });
    }

    #[gpui::test]
    fn test_number_input_leading_dot_editable(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| {
            state.mask_pattern(MaskPattern::Number {
                separator: None,
                fraction: None,
            })
        });
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "1.2", window, cx);

                // Delete the integer part "1": the value keeps the leading dot
                // (".2"), not completed to "0.2", so the digits before the dot
                // stay editable.
                let range = state.range_to_utf16(&(0..1));
                state.replace_text_in_range(Some(range), "", window, cx);
                assert_eq!(state.value(), ".2");
                let cursor: Range<usize> = state.selected_range();
                assert_eq!(cursor, 0..0);

                // The user can type a new integer part.
                state.replace_text_in_range(Some(0..0), "3", window, cx);
                assert_eq!(state.value(), "3.2");
            });
        });
    }

    #[gpui::test]
    fn test_number_input_escape_invalid_text(cx: &mut TestAppContext) {
        // A pre-existing invalid text (e.g. a `default_value` that does not
        // conform) must not trap the user, the edit is allowed to fix it.
        let input_view = InputView::build(cx, |state| {
            state
                .mask_pattern(MaskPattern::Number {
                    separator: None,
                    fraction: None,
                })
                .default_value("1,234")
        });
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                // Delete the last char, the pending text "1,23" is still
                // invalid, but the edit is allowed since the old text was
                // already invalid.
                let range = state.range_to_utf16(&(4..5));
                state.replace_text_in_range(Some(range), "", window, cx);
                assert_eq!(state.value(), "1,23");

                // Once the text becomes valid, the validation works as usual.
                let range = state.range_to_utf16(&(1..2));
                state.replace_text_in_range(Some(range), "", window, cx);
                assert_eq!(state.value(), "123");
                state.replace_text_in_range(None, "a", window, cx);
                assert_eq!(state.value(), "123");
            });
        });
    }

    /// After `set_value` on a single-line input the caret sits at the end (like
    /// HTML `<input>`), yet the view is scrolled back to the start so a long
    /// value shows its beginning instead of its tail.
    #[gpui::test]
    fn test_set_value_single_line_caret_at_end_view_at_start(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        // Long enough to overflow any reasonable single-line input width.
        let value = format!("https://example.com/v1/users?{}", "x=1&".repeat(120));
        let len = value.len();

        // Right after `set_value`, before the next paint consumes the deferred
        // offset: caret is at the end, and the view is forced back to the start.
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_value(value.clone(), window, cx);

                assert_eq!(
                    state.selected_range(),
                    len..len,
                    "single-line caret should be at the end after set_value"
                );
                assert_eq!(
                    state.deferred_scroll_offset,
                    Some(point(px(0.), px(0.))),
                    "the view should be forced back to the start"
                );
            });
        });

        // After a paint, the steady-state view stays at the start (x == 0) even
        // though the caret is at the far end.
        cx.run_until_parked();
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                assert!(
                    state.scroll_size.width > state.input_bounds.size.width,
                    "value must overflow the input width or this test is vacuous"
                );
                assert_eq!(
                    state.scroll_handle.offset().x,
                    px(0.),
                    "long value should display from its start, not its tail"
                );
            });
        });
    }

    /// `replace_all` on a single-line input replaces the text, puts the
    /// caret at the end, and — like `set_value` — snaps the view back to the
    /// start so a long value shows its beginning instead of its tail.
    #[gpui::test]
    fn test_replace_all_single_line(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        // Long enough to overflow any reasonable single-line input width.
        let value = format!("https://example.com/v1/users?{}", "x=1&".repeat(120));
        let len = value.len();

        // Right after `replace_all`, before the next paint consumes the
        // deferred offset: caret is at the end, and the view is forced back
        // to the start.
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_value("hello", window, cx);
                state.replace_all(value.clone(), window, cx);
                assert_eq!(state.value(), value);
                assert_eq!(
                    state.selected_range(),
                    len..len,
                    "single-line caret should be at the end after replace_all"
                );
                assert_eq!(
                    state.scroll_handle.offset(),
                    point(px(0.), px(0.)),
                    "the scroll offset should be reset to the start"
                );
                assert_eq!(
                    state.deferred_scroll_offset,
                    Some(point(px(0.), px(0.))),
                    "single-line should set a deferred scroll offset to keep the start visible"
                );
            });
        });

        // After a paint, the steady-state view stays at the start (x == 0)
        // even though the caret is at the far end.
        cx.run_until_parked();
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                assert!(
                    state.scroll_size.width > state.input_bounds.size.width,
                    "value must overflow the input width or this test is vacuous"
                );
                assert_eq!(
                    state.scroll_handle.offset().x,
                    px(0.),
                    "long value should display from its start, not its tail"
                );
            });
        });
    }

    #[gpui::test]
    fn test_single_line_removes_newlines(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state.default_value("default\nvalue"));
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                assert_eq!(state.value(), "defaultvalue");

                state.set_value("first\nsecond\r\nthird\rfourth", window, cx);
                assert_eq!(state.value(), "firstsecondthirdfourth");

                state.set_value("", window, cx);
                state.insert("a\nb", window, cx);
                assert_eq!(state.value(), "ab");
            });

            cx.write_to_clipboard(ClipboardItem::new_string("a\r\nb\nc\rd".to_string()));
            input.update(cx, |state, cx| {
                state.set_value("", window, cx);
                state.paste(&Paste, window, cx);
                assert_eq!(state.value(), "abcd");
            });
        });

        cx.run_until_parked();
    }

    /// `replace_all` on a multi-line (non-code-editor) input clears the
    /// selection to `0..0` and resets the scroll offset, but does not set a
    /// deferred scroll offset (single-line only).
    #[gpui::test]
    fn test_replace_all_multi_line(cx: &mut TestAppContext) {
        let input_view = InputView::build_textarea(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_value("foo\nbar", window, cx);
                state.replace_all("baz\nqux", window, cx);
                assert_eq!(state.value(), "baz\nqux");
                assert_eq!(
                    state.selected_range(),
                    0..0,
                    "multi-line selection should be cleared after replace_all"
                );
                assert_eq!(
                    state.scroll_handle.offset(),
                    point(px(0.), px(0.)),
                    "the scroll offset should be reset to the start"
                );
                assert!(
                    state.deferred_scroll_offset.is_none(),
                    "multi-line should not set a deferred scroll offset"
                );
            });
        });
    }

    /// Unlike `set_value`, `replace_all` records the change so the user can
    /// undo it back to the previous text and redo to the new text.
    #[gpui::test]
    fn test_replace_all_preserves_undo_history(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                // Seed with a value and clear history so the baseline is clean.
                state.set_value("first", window, cx);
                assert!(
                    !state.undo_manager.has_undos(),
                    "history should be empty after set_value"
                );

                // replace_all records a single undoable change.
                state.replace_all("second", window, cx);
                assert_eq!(state.value(), "second");
                assert!(
                    state.undo_manager.has_undos(),
                    "replace_all should record an undo step"
                );

                // Undo restores the previous text.
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "first");

                // Redo reapplies the replacement.
                state.redo(&Redo, window, cx);
                assert_eq!(state.value(), "second");
            });
        });
    }

    /// `replace_all` on a code editor marks a pending update and resets LSP
    /// state, so diagnostics/completions refresh against the new text.
    #[gpui::test]
    fn test_replace_all_code_editor(cx: &mut TestAppContext) {
        let input_view = InputView::new(cx);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                // Plant a pending-update flag and some LSP state to verify reset.
                state.set_value("select 1", window, cx);
                state._pending_update = false;

                state.replace_all("select 2", window, cx);
                assert_eq!(state.value(), "select 2");
                assert!(
                    state._pending_update,
                    "replace_all on a code editor should request a pending update"
                );
            });
        });
    }

    #[gpui::test]
    fn test_set_selected_range(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state.default_value("hello world"));
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|_, cx| {
            input.update(cx, |s, cx| {
                s.set_selected_range(0..5, cx);
                assert_eq!(s.selected_range(), 0..5);
                assert_eq!(s.selected_text().to_string(), "hello");

                s.set_selected_range(6..11, cx);
                assert_eq!(s.selected_text().to_string(), "world");

                // clamped + collapsed
                s.set_selected_range(100..100, cx);
                assert_eq!(s.selected_range(), 11..11);
            });
        });
    }

    /// A single-edit batch round-trips through undo/redo.
    #[gpui::test]
    fn test_replace_text_in_ranges_single_edit(cx: &mut TestAppContext) {
        let input_view = InputView::build_textarea(cx, |state| state.default_value("hello world"));
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |s, cx| {
                s.replace_text_in_ranges(&[(0..5, "HELLO".to_string())], window, cx);
                assert_eq!(s.value(), "HELLO world");

                s.undo(&Undo, window, cx);
                assert_eq!(s.value(), "hello world");

                s.redo(&Redo, window, cx);
                assert_eq!(s.value(), "HELLO world");
            });
        });
    }

    /// A single undo restores the exact original text and a single redo
    /// re-applies all edits, verifying the back-to-front application ordering.
    #[gpui::test]
    fn test_replace_text_in_ranges_multi_edit_transaction(cx: &mut TestAppContext) {
        let input_view = InputView::build_textarea(cx, |state| state.default_value("aaa bbb ccc"));
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |s, cx| {
                // Two edits at different positions, given in pre-edit
                // coordinates and in arbitrary (non-sorted) order.
                s.replace_text_in_ranges(
                    &[(0..3, "X".to_string()), (8..11, "Y".to_string())],
                    window,
                    cx,
                );
                assert_eq!(s.value(), "X bbb Y");

                // One collapsed cursor per edit, at the end of each inserted text.
                let cursors: Vec<usize> =
                    s.selections.iter().map(|sel| sel.cursor_offset()).collect();
                assert_eq!(cursors, vec![1, 7]);

                // The whole batch is a single undo transaction.
                assert_eq!(s.undo_manager.undo_count(), 1);

                // One undo restores the exact original text.
                s.undo(&Undo, window, cx);
                assert_eq!(s.value(), "aaa bbb ccc");

                // One redo re-applies all edits.
                s.redo(&Redo, window, cx);
                assert_eq!(s.value(), "X bbb Y");
            });
        });
    }

    /// An IME composition (marking then commit) undoes as a single unit.
    #[gpui::test]
    fn test_ime_composition_undoes_as_one_unit(cx: &mut TestAppContext) {
        let input_view = InputView::build_textarea(cx, |state| state.default_value(""));
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |s, cx| {
                // Simulate an IME composition: mark, refine, then commit.
                s.replace_and_mark_text_in_range(None, "n", Some(1..1), window, cx);
                s.replace_and_mark_text_in_range(None, "ni", Some(2..2), window, cx);
                s.replace_text_in_range(None, "你", window, cx);
                assert_eq!(s.value(), "你");

                // The entire composition is one undo transaction.
                assert_eq!(s.undo_manager.undo_count(), 1);

                s.undo(&Undo, window, cx);
                assert_eq!(s.value(), "");

                s.redo(&Redo, window, cx);
                assert_eq!(s.value(), "你");
            });
        });
    }

    /// A keystroke right after a committed composition must be its own undo
    /// entry, not merged into the (finalized) composition transaction.
    #[gpui::test]
    fn test_edit_after_composition_is_separate_undo(cx: &mut TestAppContext) {
        let input_view = InputView::build_textarea(cx, |state| state.default_value(""));
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |s, cx| {
                s.replace_and_mark_text_in_range(None, "n", Some(1..1), window, cx);
                s.replace_text_in_range(None, "你", window, cx);
                assert_eq!(s.value(), "你");
                assert_eq!(s.undo_manager.undo_count(), 1);

                // Typing after the commit is a distinct transaction.
                s.replace_text_in_range(None, "x", window, cx);
                assert_eq!(s.value(), "你x");
                assert_eq!(s.undo_manager.undo_count(), 2);

                s.undo(&Undo, window, cx);
                assert_eq!(s.value(), "你");
                s.undo(&Undo, window, cx);
                assert_eq!(s.value(), "");
            });
        });
    }

    /// Canceling a composition via `unmark_text` closes its transaction so it
    /// does not leak and swallow a later edit.
    #[gpui::test]
    fn test_composition_cancel_via_unmark_does_not_leak(cx: &mut TestAppContext) {
        let input_view = InputView::build_textarea(cx, |state| state.default_value(""));
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |s, cx| {
                // Start a composition, then cancel it via unmark.
                s.replace_and_mark_text_in_range(None, "n", Some(1..1), window, cx);
                s.unmark_text(window, cx);
                let after_cancel = s.undo_manager.undo_count();

                // A later edit is recorded independently.
                s.replace_text_in_range(None, "x", window, cx);
                assert_eq!(s.undo_manager.undo_count(), after_cancel + 1);
            });
        });
    }

    #[gpui::test]
    fn test_set_selected_range_clips_to_utf8_boundaries(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state.default_value("éx"));
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_selected_range(0..1, cx);
                assert_eq!(state.selected_range(), 0..2);
                state.copy(&Copy, window, cx);

                state.set_selected_range(1..1, cx);
                assert_eq!(state.selected_range(), 0..0);
            });
        });
    }

    #[gpui::test]
    fn test_ime_selection_is_relative_to_replacement_start(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state.default_value("你好 "));
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_selected_range(7..7, cx);
                state.replace_and_mark_text_in_range(None, "s", Some(1..1), window, cx);
                state.replace_and_mark_text_in_range(None, "sh", Some(2..2), window, cx);

                assert_eq!(state.value(), "你好 sh");
                assert_eq!(state.selected_range(), 9..9);
                assert_eq!(state.ime_marked_range, Some((7..9).into()));
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_composition_is_one_undo_group(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_value("a", window, cx);
                state.replace_and_mark_text_in_range(None, "s", None, window, cx);
                state.replace_and_mark_text_in_range(None, "sh", None, window, cx);
                state.replace_text_in_range(None, "是", window, cx);
                assert_eq!(state.value(), "a是");

                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "a");
                state.redo(&Redo, window, cx);
                assert_eq!(state.value(), "a是");
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_consecutive_compositions_are_separate_groups(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                // First composition: "jin" -> "今天"
                state.replace_and_mark_text_in_range(None, "j", None, window, cx);
                state.replace_and_mark_text_in_range(None, "jin", None, window, cx);
                state.replace_text_in_range(None, "今天", window, cx);
                // Second composition: "wo" -> "我们"
                state.replace_and_mark_text_in_range(None, "w", None, window, cx);
                state.replace_and_mark_text_in_range(None, "wo", None, window, cx);
                state.replace_text_in_range(None, "我们", window, cx);
                assert_eq!(state.value(), "今天我们");
                assert_eq!(state.selected_range(), 12..12);

                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "今天");
                assert_eq!(state.selected_range(), 6..6);

                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "");
                assert_eq!(state.selected_range(), 0..0);

                state.redo(&Redo, window, cx);
                assert_eq!(state.value(), "今天");
                assert_eq!(state.selected_range(), 6..6);

                state.redo(&Redo, window, cx);
                assert_eq!(state.value(), "今天我们");
                assert_eq!(state.selected_range(), 12..12);
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_typing_after_composition_is_a_separate_group(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_and_mark_text_in_range(None, "n", None, window, cx);
                state.replace_text_in_range(None, "你", window, cx);
                state.undo_manager.set_pending_intent(EditIntent::Typing);
                state.replace_text_in_range(None, "a", window, cx);
                state.undo_manager.set_pending_intent(EditIntent::Typing);
                state.replace_text_in_range(None, "b", window, cx);
                assert_eq!(state.value(), "你ab");

                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "你");

                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "");
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_composition_cancel_leaves_no_entry(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_value("a", window, cx);
                state.replace_and_mark_text_in_range(None, "s", None, window, cx);
                state.replace_and_mark_text_in_range(None, "", None, window, cx);

                assert_eq!(state.value(), "a");
                assert!(!state.undo_manager.has_undos());
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_selection_restored_by_undo_and_redo(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_value("abc", window, cx);
                state.set_selected_range(1..2, cx);
                state.replace_text_in_range(None, "X", window, cx);

                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "abc");
                assert_eq!(state.selected_range(), 1..2);

                state.redo(&Redo, window, cx);
                assert_eq!(state.value(), "aXc");
                assert_eq!(state.selected_range(), 2..2);
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_forward_delete_restores_cursor(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_value("abc", window, cx);
                state.set_selected_range(1..1, cx);
                state.delete(&Delete, window, cx);

                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "abc");
                assert_eq!(state.selected_range(), 1..1);
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_selection_movement_preserves_redo(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "ab", window, cx);
                state.undo(&Undo, window, cx);
                state.set_selected_range(0..0, cx);
                state.redo(&Redo, window, cx);
                assert_eq!(state.value(), "ab");

                state.undo(&Undo, window, cx);
                state.replace_text_in_range(None, "x", window, cx);
                state.redo(&Redo, window, cx);
                assert_eq!(state.value(), "x");
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_noop_edit_preserves_redo(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "a", window, cx);
                state.move_to(0, None, cx);
                state.replace_text_in_range(None, "", window, cx);
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "");
                state.redo(&Redo, window, cx);
                assert_eq!(state.value(), "a");
                assert_eq!(state.cursor(), 1);
            });
        });
    }

    #[gpui::test]
    fn test_cursor_round_trip_stops_typing_coalescing(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "a", window, cx);
                state.left(&MoveLeft, window, cx);
                state.right(&MoveRight, window, cx);
                state.replace_text_in_range(None, "b", window, cx);

                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "a");
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "");
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_noop_edit_breaks_coalescing_without_clearing_history(
        cx: &mut TestAppContext,
    ) {
        let input_view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "alpha", window, cx);
                state.replace_text_in_range(None, "", window, cx);
                state.replace_text_in_range(None, "beta", window, cx);
                assert_eq!(state.value(), "alphabeta");

                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "alpha");
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "");
            });
        });
    }

    #[gpui::test]
    fn test_undo_manager_masked_redo_restores_actual_cursor(cx: &mut TestAppContext) {
        let input_view = InputView::build(cx, |state| {
            state.mask_pattern(MaskPattern::Number {
                separator: Some(','),
                fraction: None,
            })
        });
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_value("12345", window, cx);
                state.set_selected_range(2..2, cx);
                state.replace_text_in_range(None, "9", window, cx);
                let selection_after_edit = state.selected_range();
                assert_ne!(selection_after_edit.end, state.value().len());

                state.undo(&Undo, window, cx);
                state.redo(&Redo, window, cx);
                assert_eq!(state.selected_range(), selection_after_edit);
            });
        });
    }

    /// Unfolding at a position opens exactly the folds hiding it.
    ///
    /// A fold keeps its own first and last line visible, so a position on
    /// either of them opens nothing. Nested folds all open at once, sibling
    /// folds stay closed, and the opened ranges stay fold candidates.
    #[gpui::test]
    fn test_unfold_at(cx: &mut TestAppContext) {
        use crate::input::{FoldRange, Position};

        let view = InputView::<EditorMode>::new(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        let input = view.input;

        // An outer fold over lines 0..=5, a fold nested inside it, and a
        // sibling fold that must never be touched.
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_value("a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl", window, cx);
                state.apply_highlighter_fold_candidates(
                    vec![
                        FoldRange::new(0, 5),
                        FoldRange::new(2, 4),
                        FoldRange::new(7, 10),
                    ],
                    cx,
                );
                state.display_map.set_folded(0, true);
                state.display_map.set_folded(2, true);
                state.display_map.set_folded(7, true);
            });
        });

        // The outer fold's own first and last line stay visible, so neither
        // position opens anything.
        for line in [0, 5] {
            cx.update(|_, cx| {
                input.update(cx, |state, cx| {
                    assert!(!state.display_map.is_buffer_line_hidden(line));
                    assert!(!state.unfold_at(Position::new(line as u32, 0), cx));
                });
                input.read_with(cx, |state, _| {
                    assert!(state.display_map.is_folded_at(0));
                    assert!(state.display_map.is_folded_at(2));
                    assert!(state.display_map.is_folded_at(7));
                });
            });
        }

        // Line 3 is hidden by both the outer and the nested fold, so both
        // open; the sibling fold does not.
        cx.update(|_, cx| {
            input.update(cx, |state, cx| {
                assert!(state.unfold_at(Position::new(3, 0), cx));
            });
            input.read_with(cx, |state, _| {
                assert!(!state.display_map.is_buffer_line_hidden(3));
                assert!(!state.display_map.is_folded_at(0));
                assert!(!state.display_map.is_folded_at(2));
                assert!(state.display_map.is_folded_at(7));
                // The opened ranges are still candidates for refolding.
                assert!(state.display_map.is_fold_candidate(0));
                assert!(state.display_map.is_fold_candidate(2));
            });
        });

        // Nothing is hidden there any more, so a second call is a no-op.
        cx.update(|_, cx| {
            input.update(cx, |state, cx| {
                assert!(!state.unfold_at(Position::new(3, 0), cx));
            });
        });
    }

    /// Losing focus hides the hover popover but keeps the decorations.
    ///
    /// Both used to be dropped by one call, so clicking away threw away
    /// decorations the application had installed and never asked to remove.
    #[gpui::test]
    fn test_blur_keeps_decorations(cx: &mut TestAppContext) {
        let view = InputView::<EditorMode>::new(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        let input = view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_value("select 1", window, cx);
                let _collection = state.create_decorations_collection(
                    vec![crate::input::TextDecoration::new(
                        0..6,
                        gpui::HighlightStyle {
                            font_weight: Some(gpui::FontWeight::BOLD),
                            ..Default::default()
                        },
                    )],
                    cx,
                );
                state.present_hover(
                    0..6,
                    lsp_types::Hover {
                        contents: lsp_types::HoverContents::Scalar(
                            lsp_types::MarkedString::String("docs".into()),
                        ),
                        range: None,
                    },
                    cx,
                );
                assert!(state.hover_popover().is_some());

                state.on_blur(window, cx);

                assert!(
                    state.hover_popover().is_none(),
                    "blur should hide the hover popover"
                );
                let decorations = state.extras.decoration_layers();
                assert!(
                    decorations.iter().any(|layer| !layer.is_empty()),
                    "blur must not discard decorations"
                );
            });
        });
    }

    /// The mode marker is the only source of truth for the kind of input.
    ///
    /// An auto-growing textarea capped at one row used to report itself as
    /// single-line, because the answer was derived from the row counts.
    #[gpui::test]
    fn test_kind_does_not_follow_the_row_count(cx: &mut TestAppContext) {
        let view = InputView::build_textarea(cx, |state| state.auto_grow(1, 1));
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        view.input.read_with(&mut cx, |state, _| {
            assert!(state.is_multi_line());
            assert!(!state.is_single_line());
            assert!(!state.is_code_editor());
        });
    }

    /// Soft wrap is on by default, for every mode that can wrap.
    ///
    /// The default lives in the shared constructor, where a mode-specific
    /// `new` can silently fail to restore it; this pins it down.
    #[gpui::test]
    fn test_soft_wrap_is_enabled_by_default(cx: &mut TestAppContext) {
        let textarea = InputView::build_textarea(cx, |state| state);
        let mut textarea_cx = VisualTestContext::from_window(textarea.window_handle.into(), cx);
        textarea
            .input
            .read_with(&mut textarea_cx, |state, _| assert!(state.soft_wrap));

        let editor = InputView::<EditorMode>::new(cx);
        let mut editor_cx = VisualTestContext::from_window(editor.window_handle.into(), cx);
        editor
            .input
            .read_with(&mut editor_cx, |state, _| assert!(state.soft_wrap));
    }

    /// A highlighter that turns one line into a block (ADR-0009): its text
    /// hidden, its height `scale` lines, a block asked for on it.
    struct BlockHighlighter {
        block_line_start: usize,
        scale: Rc<std::cell::Cell<f32>>,
    }

    impl crate::input::InputHighlighter for BlockHighlighter {
        fn language(&self) -> SharedString {
            "block-test".into()
        }

        fn update(
            &mut self,
            _edit: Option<crate::input::InputEdit>,
            _text: &Rope,
            _folding: bool,
            _window: &mut Window,
            _cx: &mut Context<crate::input::EditorState>,
        ) {
        }

        fn styles(
            &self,
            range: &Range<usize>,
            _resolver: &dyn crate::input::HighlightStyleResolver,
        ) -> Vec<(Range<usize>, HighlightStyle)> {
            vec![(range.clone(), HighlightStyle::default())]
        }

        fn fold_ranges(&self, _text: &Rope) -> Vec<crate::input::FoldRange> {
            Vec::new()
        }

        fn conceals(&self, range: &Range<usize>) -> Vec<Range<usize>> {
            if range.start == self.block_line_start && !range.is_empty() {
                vec![range.clone()]
            } else {
                Vec::new()
            }
        }

        fn line_height_scale(&self, line_range: &Range<usize>, _: &Rope, _: u64) -> f32 {
            if line_range.start == self.block_line_start {
                self.scale.get()
            } else {
                1.0
            }
        }

        fn block_widget(&self, line_range: &Range<usize>) -> Option<crate::input::BlockWidget> {
            (line_range.start == self.block_line_start).then(|| crate::input::BlockWidget {
                id: "img".into(),
                data: Rc::new(()),
            })
        }
    }

    fn block_factory(
        block_line_start: usize,
        scale: Rc<std::cell::Cell<f32>>,
    ) -> crate::input::InputHighlighterFactory {
        Rc::new(move |_lang: &str| {
            Some(Box::new(BlockHighlighter {
                block_line_start,
                scale: scale.clone(),
            }) as Box<dyn crate::input::InputHighlighter>)
        })
    }

    /// A block is drawn over its line at the height the line was given, told
    /// its range and size, and a click inside it does not move the caret.
    #[gpui::test]
    fn test_a_block_is_laid_out_over_its_line_and_owns_its_clicks(cx: &mut TestAppContext) {
        // Line 1, "IMG" (4..7), becomes a block four lines tall.
        let input_view =
            InputView::build_editor(cx, |state| state.default_value("one\nIMG\nthree"));
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;
        let drawn: Rc<RefCell<Vec<crate::input::BlockContext>>> = Rc::default();
        let renderer: crate::input::BlockRenderer = {
            let drawn = drawn.clone();
            Rc::new(move |block, context, _, _| {
                assert_eq!(block.id.as_ref(), "img");
                drawn.borrow_mut().push(context.clone());
                gpui::div().size_full().into_any_element()
            })
        };
        cx.update(|_, cx| {
            input.update(cx, |state, cx| {
                state.set_highlighter_factory(
                    block_factory(4, Rc::new(std::cell::Cell::new(4.0))),
                    cx,
                );
                state.set_block_renderer(Some(renderer), cx);
                state.set_selected_range(0..0, cx);
            });
        });
        cx.run_until_parked();
        // The highlighter exists once the text has been parsed; its heights
        // are read from then on.
        cx.update(|_, cx| input.update(cx, |state, cx| state.refresh_line_heights(cx)));
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let context = drawn.borrow().last().cloned().expect("the block was drawn");
        assert_eq!(context.range, 4..7);
        let (hitbox, line_height) = cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                let hitboxes = state.widget_hitboxes.borrow();
                assert_eq!(hitboxes.len(), 1, "one block, one hitbox");
                (hitboxes[0], state.last_layout.as_ref().unwrap().line_height)
            })
        });
        assert_eq!(context.size.height, line_height * 4.0);
        assert_eq!(hitbox.size.height, line_height * 4.0);

        cx.simulate_mouse_down(
            hitbox.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        cx.simulate_mouse_up(
            hitbox.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        cx.run_until_parked();
        let selected = cx.update(|_, cx| input.read_with(cx, |state, _| state.selected_range()));
        assert_eq!(selected, 0..0, "the click was the block's");
    }

    /// A block above the view that grows moves the scroll offset with it, so
    /// the first visible line stays where it was.
    #[gpui::test]
    fn test_a_block_growing_above_the_view_keeps_the_view_still(cx: &mut TestAppContext) {
        let text: SharedString = (0..200)
            .map(|n| format!("line {n}\n"))
            .collect::<String>()
            .into();
        let input_view =
            InputView::build_editor(cx, move |state| state.default_value(text.clone()));
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;
        let scale = Rc::new(std::cell::Cell::new(1.0));
        // "line 1" starts at byte 7.
        cx.update(|_, cx| {
            input.update(cx, |state, cx| {
                state.set_highlighter_factory(block_factory(7, scale.clone()), cx);
            });
        });
        cx.run_until_parked();
        // The highlighter exists once the text has been parsed; its heights
        // are read from then on.
        cx.update(|_, cx| input.update(cx, |state, cx| state.refresh_line_heights(cx)));
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let line_height = cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                state.last_layout.as_ref().unwrap().line_height
            })
        });
        cx.update(|_, cx| {
            input.update(cx, |state, cx| {
                state.set_scroll_offset(point(px(0.), -line_height * 50.), cx);
            });
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let first = |cx: &mut VisualTestContext| {
            cx.update(|_, cx| {
                input.read_with(cx, |state, _| {
                    state.last_layout.as_ref().unwrap().visible_buffer_lines[0]
                })
            })
        };
        let before = first(&mut cx);
        assert!(before > 10, "scrolled well past the block: {before}");

        let total = |cx: &mut VisualTestContext| {
            cx.update(|_, cx| input.read_with(cx, |state, _| state.display_map.total_height()))
        };
        let total_before = total(&mut cx);
        scale.set(10.0);
        cx.update(|_, cx| {
            input.update(cx, |state, cx| state.rewrap_lines_anchored(7..13, cx));
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(total(&mut cx) - total_before, 9.0, "the block did grow");
        assert_eq!(first(&mut cx), before, "the view did not move");
    }

    /// A highlighter that hides byte ranges and scales one line, for the
    /// concealment (live-preview) layout path.
    struct ConcealHighlighter {
        /// Absolute buffer byte ranges to hide.
        conceal: Vec<Range<usize>>,
        /// Buffer line start offset whose line is drawn at 1.5x font size.
        scaled_line_start: Option<usize>,
    }

    impl crate::input::InputHighlighter for ConcealHighlighter {
        fn language(&self) -> SharedString {
            "conceal-test".into()
        }

        fn update(
            &mut self,
            _edit: Option<crate::input::InputEdit>,
            _text: &Rope,
            _folding: bool,
            _window: &mut Window,
            _cx: &mut Context<crate::input::EditorState>,
        ) {
        }

        fn styles(
            &self,
            range: &Range<usize>,
            _resolver: &dyn crate::input::HighlightStyleResolver,
        ) -> Vec<(Range<usize>, HighlightStyle)> {
            vec![(range.clone(), HighlightStyle::default())]
        }

        fn fold_ranges(&self, _text: &Rope) -> Vec<crate::input::FoldRange> {
            Vec::new()
        }

        fn conceals(&self, range: &Range<usize>) -> Vec<Range<usize>> {
            self.conceal
                .iter()
                .filter(|r| r.start >= range.start && r.end <= range.end)
                .cloned()
                .collect()
        }

        fn line_font_scale(&self, line_range: &Range<usize>) -> f32 {
            if Some(line_range.start) == self.scaled_line_start {
                1.5
            } else {
                1.0
            }
        }
    }

    /// Concealed bytes take no width, caret/hit-testing stay in buffer
    /// offsets, and a scaled line shapes its glyphs larger.
    ///
    /// The test text system gives every glyph 0.6em, so widths are exact.
    #[gpui::test]
    fn test_conceal_and_line_font_scale_layout(cx: &mut TestAppContext) {
        // Line 0: "# Heading" (0..9), conceal "# " (0..2).
        // Line 1: "body"      (10..14), control line.
        // Line 2: "big"       (15..18), scaled 1.5x.
        let input_view =
            InputView::build_editor(cx, |state| state.default_value("# Heading\nbody\nbig"));
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        let factory: crate::input::InputHighlighterFactory = Rc::new(|_lang: &str| {
            Some(Box::new(ConcealHighlighter {
                conceal: vec![0..2],
                scaled_line_start: Some(15),
            }) as Box<dyn crate::input::InputHighlighter>)
        });
        cx.update(|_, cx| {
            input.update(cx, |state, cx| {
                state.set_highlighter_factory(factory, cx);
            });
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                let x_of = |offset: usize| -> f32 {
                    let (_, _, pos) = state.line_and_position_for_offset(offset);
                    pos.expect("offset should be laid out").x.into()
                };
                let close = |a: f32, b: f32| (a - b).abs() < 0.01;

                // Control line "body": one glyph per byte.
                let line1_start = x_of(10);
                let glyph_w = x_of(11) - line1_start;
                assert!(glyph_w > 0., "glyph width must be positive: {glyph_w}");
                assert!(close(x_of(14) - line1_start, 4. * glyph_w));

                // Concealed line: "# " is hidden, so the caret after "# H"
                // (raw offset 3) sits one glyph in, i.e. moved left by the
                // width of the two concealed glyphs.
                let line0_start = x_of(0);
                assert!(close(line0_start, line1_start), "lines start at the same x");
                assert!(
                    close(x_of(3) - line0_start, glyph_w),
                    "offset 3: {} vs {}",
                    x_of(3) - line0_start,
                    glyph_w
                );
                // Offsets inside the hidden range collapse to its start.
                assert!(close(x_of(1), line0_start));
                assert!(close(x_of(2), line0_start));
                // End of the line: 7 visible glyphs ("Heading").
                assert!(close(x_of(9) - line0_start, 7. * glyph_w));

                // Scaled line: glyphs are 1.5x wider.
                let line2_start = x_of(15);
                assert!(
                    close(x_of(16) - line2_start, 1.5 * glyph_w),
                    "scaled glyph: {} vs {}",
                    x_of(16) - line2_start,
                    1.5 * glyph_w
                );
                assert!(close(x_of(18) - line2_start, 3. * 1.5 * glyph_w));

                // Clicking maps display x back to raw (buffer) offsets.
                let last_layout = state.last_layout.as_ref().expect("layout");
                let bounds = state.last_bounds.expect("bounds");
                let line_height = last_layout.line_height;
                let lnw = last_layout.line_number_width;
                let click = |x: f32, row: f32| -> usize {
                    state
                        .index_for_mouse_position(
                            bounds.origin + point(lnw + px(x), line_height * (row + 0.5)),
                        )
                        .0
                };
                // Line 0: before the hidden "# " -> 0; between "H" and "e" -> 3;
                // between "e" and "a" -> 4; far right -> end of line (9).
                assert_eq!(click(0.5, 0.), 0);
                assert_eq!(click(glyph_w + 0.5, 0.), 3);
                assert_eq!(click(2. * glyph_w + 0.5, 0.), 4);
                assert_eq!(click(100. * glyph_w, 0.), 9);
                // Line 1 is unaffected.
                assert_eq!(click(glyph_w + 0.5, 1.), 11);
                // Line 2: scaled glyphs are wider, so the second boundary is
                // at 1.5 glyph widths.
                assert_eq!(click(1.5 * glyph_w + 0.5, 2.), 16);
            });
        });
    }

    /// Parse a cursor spec into `(text, cursor_offsets)`. Non-empty lines are
    /// joined with `\n` plus a trailing `\n`. `|` marks a cursor. Leading
    /// whitespace is kept, so a spec can express indentation.
    fn parse_cursor_spec(input: &str) -> (String, Vec<usize>) {
        let mut full_text = String::new();
        let mut cursor_offsets = Vec::new();
        let non_empty_lines: Vec<&str> = input.lines().filter(|l| !l.is_empty()).collect();

        for (line_idx, line) in non_empty_lines.iter().enumerate() {
            let mut positions = Vec::new();
            let mut text = String::new();
            for ch in line.chars() {
                if ch == '|' {
                    positions.push(text.len());
                } else {
                    text.push(ch);
                }
            }

            if line_idx > 0 {
                full_text.push('\n');
            }
            let line_start = full_text.len();
            for pos in positions {
                cursor_offsets.push(line_start + pos);
            }
            full_text.push_str(&text);
        }
        full_text.push('\n');

        (full_text, cursor_offsets)
    }

    /// Build a multi-line input for multi-cursor tests.
    fn multi_line(cx: &mut TestAppContext) -> InputView<TextareaMode> {
        InputView::build_textarea(cx, |state| state)
    }

    /// Set the text and cursor positions from a spec (see [`parse_cursor_spec`]).
    fn setup_cursors<M: InputModeKind>(
        cx: &mut VisualTestContext,
        input: &Entity<InputBaseState<M>>,
        spec: &str,
    ) {
        let (full_text, offsets) = parse_cursor_spec(spec);
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_value(&full_text, window, cx);
                let selections = offsets
                    .into_iter()
                    .map(|offset| {
                        CursorSelection::new(state.selections.generate_id(), offset, offset)
                    })
                    .collect();
                state.selections.replace_all(selections);
                cx.notify();
            });
        });
    }

    /// The scroll extent and the scroll-to-row arithmetic must go through the
    /// summed heights, not the row count: with one line drawn 1.5x tall,
    /// every row below it sits half a row lower than the count says.
    #[gpui::test]
    fn scrolling_follows_the_summed_heights_past_a_taller_line(cx: &mut TestAppContext) {
        // 200 short lines so the document is taller than the window; line 5
        // is scaled 1.5x.
        let text: String = (0..200)
            .map(|i| format!("line {i}\n"))
            .collect::<Vec<_>>()
            .join("");
        let scaled_line_start = text.find("line 5\n").expect("line 5");
        let input_view = InputView::build_editor(cx, move |state| {
            state
                .default_value(text.as_str())
                .scroll_beyond_last_line(Some(0))
        });
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;

        let factory: crate::input::InputHighlighterFactory = Rc::new(move |_lang: &str| {
            Some(Box::new(ConcealHighlighter {
                conceal: Vec::new(),
                scaled_line_start: Some(scaled_line_start),
            }) as Box<dyn crate::input::InputHighlighter>)
        });
        cx.update(|_, cx| {
            input.update(cx, |state, cx| {
                state.set_highlighter_factory(factory, cx);
            });
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        // The highlighter is created lazily on that first render, after the
        // heights were synced against nothing; an application that installs a
        // factory on an open document asks for a refresh, and so does this.
        cx.update(|_, cx| {
            input.update(cx, |state, cx| state.refresh_line_heights(cx));
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let (line_height, bounds, total_height, top_150) = cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                let layout = state.last_layout.as_ref().expect("layout");
                (
                    layout.line_height,
                    state.last_bounds.expect("bounds"),
                    state.display_map.total_height(),
                    state.display_map.buffer_line_top(150),
                )
            })
        });
        assert_eq!(total_height, 201.5, "one line is half a row taller");
        assert_eq!(top_150, 150.5);

        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                let close = |a: Pixels, b: Pixels| (f32::from(a) - f32::from(b)).abs() < 0.01;
                assert!(
                    close(state.scroll_size.height, line_height * total_height),
                    "scroll extent {:?} vs summed heights {:?}",
                    state.scroll_size.height,
                    line_height * total_height
                );
            });
        });

        // Scrolling to a row below the taller line lands on where that row is
        // drawn: half a row lower than its index says.
        let offset_150 =
            cx.update(|_, cx| input.read_with(cx, |state, _| state.text.line_start_offset(150)));
        cx.update(|_, cx| {
            input.update(cx, |state, cx| state.scroll_to(offset_150, None, cx));
        });
        // The test harness draws at the end of the update, which applies the
        // deferred target and stores the result on the scroll handle.
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                let scrolled = state.scroll_handle.offset();
                let expected = -(line_height * top_150 - bounds.size.height + line_height);
                assert!(
                    (f32::from(scrolled.y) - f32::from(expected)).abs() < 0.01,
                    "scrolled to {:?}, expected {:?}",
                    scrolled.y,
                    expected
                );
            });
        });
    }

    // ---- Table rows laid out per cell -----------------------------------

    use crate::input::display_map::table_test_support::cells_of as table_cells;

    /// A highlighter whose only job is to say which lines are table rows: a
    /// run of lines with a pipe whose second line is dashes.
    struct TableHighlighter;

    impl crate::input::InputHighlighter for TableHighlighter {
        fn language(&self) -> SharedString {
            "table-test".into()
        }

        fn update(
            &mut self,
            _edit: Option<crate::input::InputEdit>,
            _text: &Rope,
            _folding: bool,
            _window: &mut Window,
            _cx: &mut Context<crate::input::EditorState>,
        ) {
        }

        fn styles(
            &self,
            range: &Range<usize>,
            _resolver: &dyn crate::input::HighlightStyleResolver,
        ) -> Vec<(Range<usize>, HighlightStyle)> {
            vec![(range.clone(), HighlightStyle::default())]
        }

        fn fold_ranges(&self, _text: &Rope) -> Vec<crate::input::FoldRange> {
            Vec::new()
        }

        fn table_row(
            &self,
            line_range: &Range<usize>,
            text: &Rope,
            _generation: u64,
        ) -> Option<crate::input::TableRow> {
            use crate::input::TableRowKind;
            let line_of = |r: usize| -> Option<String> {
                (r < text.lines_len()).then(|| text.slice_line(r).to_string())
            };
            let is_table =
                |r: usize| line_of(r).is_some_and(|l| !l.trim().is_empty() && l.contains('|'));
            let row = text.offset_to_point(line_range.start).row;
            if !is_table(row) {
                return None;
            }
            let mut first = row;
            while first > 0 && is_table(first - 1) {
                first -= 1;
            }
            let delimiter = line_of(first + 1)?;
            if !delimiter.contains('-')
                || !delimiter
                    .chars()
                    .all(|c| matches!(c, '|' | '-' | ':' | ' '))
            {
                return None;
            }
            let mut last = first + 1;
            while is_table(last + 1) {
                last += 1;
            }
            let columns = table_cells(&line_of(first)?, None).len();
            let kind = match row - first {
                0 => TableRowKind::Header,
                1 => TableRowKind::Delimiter,
                _ => TableRowKind::Body,
            };
            Some(crate::input::TableRow {
                first_row: first,
                last_row: last,
                kind,
                columns,
                aligns: vec![crate::input::ColumnAlign::Left; columns],
                cells: table_cells(&line_of(row)?, Some(columns)),
            })
        }
    }

    /// prose, a two-column table of three rows, prose.
    const TABLE_DOC: &str = "prose one\n| ab | cd |\n| --- | --- |\n| e | f |\nprose two\n";

    fn table_editor(
        cx: &mut TestAppContext,
        text: &'static str,
    ) -> (VisualTestContext, Entity<InputBaseState<EditorMode>>) {
        cx.update(crate::init);
        let input_view =
            InputView::build_editor(cx, move |state| state.default_value(text).soft_wrap(true));
        let mut cx = VisualTestContext::from_window(input_view.window_handle.into(), cx);
        let input = input_view.input;
        let factory: crate::input::InputHighlighterFactory = Rc::new(|_lang: &str| {
            Some(Box::new(TableHighlighter) as Box<dyn crate::input::InputHighlighter>)
        });
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_highlighter_factory(factory, cx);
                state.focus(window, cx);
            });
        });
        cx.run_until_parked();
        // The highlighter is created on the first render; the heights are
        // synced against it after that.
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.update(|_, cx| {
            input.update(cx, |state, cx| state.refresh_line_heights(cx));
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (cx, input)
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            let _ = window.draw(cx).clear(cx);
        });
    }

    /// Width of one glyph, from the first prose line: the test text system
    /// gives every glyph the same width.
    fn glyph_width(state: &InputBaseState<EditorMode>) -> Pixels {
        let (_, _, a) = state.line_and_position_for_offset(0);
        let (_, _, b) = state.line_and_position_for_offset(1);
        b.expect("offset 1").x - a.expect("offset 0").x
    }

    /// The window point just before the boundary after glyph `k` of cell
    /// `cell` on buffer line `row`, on the first text row of the cell. Just
    /// before, because gpui resolves an x past the last glyph's start to the
    /// line's end.
    fn cell_point(
        state: &InputBaseState<EditorMode>,
        row: usize,
        cell: usize,
        k: usize,
    ) -> Point<Pixels> {
        let layout = state.last_layout.as_ref().expect("layout");
        let bounds = state.last_bounds.expect("bounds");
        let vi = layout
            .visible_buffer_lines
            .iter()
            .position(|&b| b == row)
            .expect("the row is visible");
        let table = layout.lines[vi].table.as_ref().expect("a table row");
        let x = bounds.origin.x
            + layout.line_number_width
            + table.columns[cell].x
            + crate::input::display_map::CELL_PAD
            + glyph_width(state) * k as f32
            - px(0.1);
        let y = bounds.origin.y
            + layout.line_height * state.display_map.buffer_line_top(row)
            + table.text_row_height / 2.;
        point(x, y)
    }

    /// The window point at glyph `k` of prose line `row`.
    fn prose_point(state: &InputBaseState<EditorMode>, row: usize, k: usize) -> Point<Pixels> {
        let layout = state.last_layout.as_ref().expect("layout");
        let bounds = state.last_bounds.expect("bounds");
        point(
            bounds.origin.x + layout.line_number_width + glyph_width(state) * k as f32 + px(0.1),
            bounds.origin.y
                + layout.line_height * state.display_map.buffer_line_top(row)
                + layout.line_height / 2.,
        )
    }

    /// Document offset of the start of cell `cell` on buffer line `row`.
    fn cell_start(state: &InputBaseState<EditorMode>, row: usize, cell: usize) -> usize {
        let layout = state.last_layout.as_ref().expect("layout");
        let vi = layout
            .visible_buffer_lines
            .iter()
            .position(|&b| b == row)
            .expect("the row is visible");
        let table = layout.lines[vi].table.as_ref().expect("a table row");
        layout.visible_line_byte_offsets[vi] + table.cells[cell].content.start
    }

    /// Up and Down move by text row inside a wrapped cell, and leave the
    /// table row only from the cell's first or last one; Up from below lands
    /// on the cell's last row.
    #[gpui::test]
    fn up_and_down_walk_the_text_rows_of_a_cell_before_leaving_it(cx: &mut TestAppContext) {
        let (mut cx, input) = table_editor(cx, TABLE_DOC);
        cx.simulate_resize(gpui::size(px(320.), px(600.)));
        cx.run_until_parked();
        draw(&mut cx);
        // Row 3 is `| e | f |`; forty characters in its first cell wrap it.
        let target = cx.update(|_, cx| input.read_with(cx, |state, _| cell_point(state, 3, 0, 1)));
        cx.simulate_click(target, gpui::Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, &"x".repeat(40), window, cx)
            })
        });
        draw(&mut cx);
        let (cell_range, rows) = cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                let (_, range) = state.table_cell(cell_start(state, 3, 0), 0).unwrap();
                let layout = state.last_layout.as_ref().unwrap();
                let vi = layout
                    .visible_buffer_lines
                    .iter()
                    .position(|&b| b == 3)
                    .unwrap();
                (range, layout.lines[vi].table.as_ref().unwrap().rows)
            })
        });
        assert!(rows >= 3, "the cell wraps to {rows} rows");

        // From the cell's start, Down walks one text row at a time.
        cx.update(|_, cx| {
            input.update(cx, |state, cx| {
                state.set_selected_range(cell_range.start..cell_range.start, cx)
            })
        });
        let mut inside = 0;
        for _ in 0..rows {
            let stays = cx.update(|_, cx| {
                input.read_with(cx, |state, _| {
                    state.table_step(state.cursor(), false).is_some()
                })
            });
            cx.simulate_keystrokes("down");
            cx.run_until_parked();
            let cursor = cx.update(|_, cx| input.read_with(cx, |state, _| state.cursor()));
            if cell_range.contains(&cursor) {
                inside += 1;
                assert!(stays, "table_step agreed the caret stays in the cell");
            } else {
                assert!(!stays, "table_step agreed the caret leaves");
                assert!(
                    cursor > cell_range.end,
                    "down left the table row at {cursor}"
                );
                break;
            }
        }
        assert_eq!(inside, rows - 1, "one Down per text row before leaving");

        // Up from the prose below lands on the cell's last text row.
        let prose_two = TABLE_DOC.len() + 40
            - "prose two
"
            .len()
            + 3;
        cx.update(|_, cx| {
            input.update(cx, |state, cx| {
                state.set_selected_range(prose_two..prose_two, cx)
            })
        });
        cx.simulate_keystrokes("up");
        cx.run_until_parked();
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                let cursor = state.cursor();
                assert!(
                    state.table_step(cursor, true).is_some(),
                    "on a text row below the first: {cursor}"
                );
                assert!(
                    state.table_step(cursor, false).is_none(),
                    "and on the last one"
                );
            })
        });
    }

    /// Shift+Down extends the selection from its head one row down at the
    /// head's x, the anchor staying; inside a wrapped cell by text row.
    #[gpui::test]
    fn shift_down_extends_by_row_at_the_head_s_x(cx: &mut TestAppContext) {
        let (mut cx, input) = table_editor(cx, TABLE_DOC);
        cx.simulate_resize(gpui::size(px(320.), px(600.)));
        cx.run_until_parked();
        draw(&mut cx);
        // Prose first: from the third glyph of `prose one`, two Shift+Right
        // then Shift+Down selects into the header row at the head's x, not
        // the anchor's: glyph 5 lies past the first cell's two glyphs, so the
        // head lands at that cell's end (`ab|`), where glyph 3 would not.
        cx.update(|_, cx| input.update(cx, |state, cx| state.set_selected_range(3..3, cx)));
        cx.simulate_keystrokes("shift-right");
        cx.simulate_keystrokes("shift-right");
        cx.simulate_keystrokes("shift-down");
        cx.run_until_parked();
        let header = TABLE_DOC.find('|').unwrap();
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                let range = state.selected_range();
                assert_eq!(range.start, 3, "the anchor stays");
                assert_eq!(range.end, header + 4, "the head keeps its x: {range:?}");
            })
        });

        // Inside a wrapped cell: by text row, the anchor staying.
        let target = cx.update(|_, cx| input.read_with(cx, |state, _| cell_point(state, 3, 0, 1)));
        cx.simulate_click(target, gpui::Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, &"x".repeat(40), window, cx)
            })
        });
        draw(&mut cx);
        let cell_range = cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                state.table_cell(cell_start(state, 3, 0), 0).unwrap().1
            })
        });
        let anchor = cell_range.start + 2;
        cx.update(|_, cx| {
            input.update(cx, |state, cx| state.set_selected_range(anchor..anchor, cx))
        });
        cx.simulate_keystrokes("shift-down");
        cx.run_until_parked();
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                let range = state.selected_range();
                assert_eq!(range.start, anchor);
                assert!(
                    cell_range.contains(&range.end) && range.end > anchor,
                    "the head moved one text row down inside the cell: {range:?} in {cell_range:?}"
                );
            })
        });
        cx.simulate_keystrokes("shift-up");
        cx.run_until_parked();
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                assert_eq!(state.selected_range(), anchor..anchor, "and back");
            })
        });
    }

    /// A double-click in a table cell selects a word of the cell, never the
    /// padding and pipes around it: after the last word that word, in an
    /// empty cell nothing.
    #[gpui::test]
    fn a_double_click_in_a_cell_selects_a_word_of_the_cell(cx: &mut TestAppContext) {
        let doc = "prose\n| ab cd |   | ef |\n| --- | --- | --- |\n| g | h | i |\n";
        let (mut cx, input) = table_editor(cx, doc);
        let cd = doc.find("cd").unwrap();
        let select = |cx: &mut VisualTestContext, offset: usize| {
            cx.update(|window, cx| {
                input.update(cx, |state, cx| state.select_word(offset, window, cx))
            });
            cx.update(|_, cx| input.read_with(cx, |state, _| state.selected_range()))
        };
        assert_eq!(select(&mut cx, cd), cd..cd + 2, "the word under the click");
        assert_eq!(
            select(&mut cx, cd + 2),
            cd..cd + 2,
            "after the cell's last word, that word"
        );
        let empty = doc.find("|   |").unwrap() + 2;
        assert_eq!(
            select(&mut cx, empty),
            empty..empty,
            "nothing in an empty cell"
        );
        let prose = doc.find("prose").unwrap();
        assert_eq!(
            select(&mut cx, prose + 1),
            prose..prose + 5,
            "prose is as before"
        );
    }

    /// With the handles switched off, the same pointer finds no marker: a
    /// writer who never restructures tables sees no chrome at all.
    #[gpui::test]
    fn no_insertion_marker_with_the_handles_off(cx: &mut TestAppContext) {
        let (mut cx, input) = table_editor(cx, TABLE_DOC);
        let header = TABLE_DOC.find('|').unwrap();
        let (cell0, _) =
            cx.update(|_, cx| input.read_with(cx, |state, _| state.table_cell(header, 0).unwrap()));
        let (cell1, _) =
            cx.update(|_, cx| input.read_with(cx, |state, _| state.table_cell(header, 1).unwrap()));
        let between = point(cell1.origin.x, cell0.origin.y);
        let marker = |cx: &mut VisualTestContext| {
            cx.update(|_, cx| input.read_with(cx, |state, _| state.table_marker.clone()))
        };

        cx.simulate_mouse_move(between, None, gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(marker(&mut cx).is_some(), "on by default");

        cx.update(|_, cx| input.update(cx, |state, cx| state.set_table_handles(false, cx)));
        assert!(
            marker(&mut cx).is_none(),
            "switching off drops the one shown"
        );
        cx.simulate_mouse_move(
            between + point(px(1.), px(1.)),
            None,
            gpui::Modifiers::default(),
        );
        cx.run_until_parked();
        assert!(
            marker(&mut cx).is_none(),
            "and none comes back under the pointer"
        );

        cx.update(|_, cx| input.update(cx, |state, cx| state.set_table_handles(true, cx)));
        cx.simulate_mouse_move(between, None, gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(marker(&mut cx).is_some(), "on again");
    }

    /// A window position inside the editor maps to the byte under it;
    /// outside, to nothing.
    #[gpui::test]
    fn offset_at_reads_the_byte_under_a_position(cx: &mut TestAppContext) {
        let (mut cx, input) = table_editor(cx, TABLE_DOC);
        let header = TABLE_DOC.find('|').unwrap();
        let (cell1, _) =
            cx.update(|_, cx| input.read_with(cx, |state, _| state.table_cell(header, 1).unwrap()));
        let inside = cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                state.offset_at(cell1.origin + point(px(2.), px(2.)))
            })
        });
        let (cell1_start, _) = cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                (state.table_cell(header, 1).unwrap().0.origin, ())
            })
        });
        let _ = cell1_start;
        assert!(inside.is_some_and(|offset| offset > header), "{inside:?}");
        let outside = cx.update(|_, cx| {
            input.read_with(cx, |state, _| state.offset_at(point(px(-100.), px(-100.))))
        });
        assert_eq!(outside, None);
    }

    /// The pointer near a column boundary of a table's top rule, or near a
    /// row's bottom-left corner, finds an insertion marker; a click on it
    /// asks the application to insert there, and moves no caret.
    #[gpui::test]
    fn an_insertion_marker_appears_near_a_boundary_and_its_click_is_an_event(
        cx: &mut TestAppContext,
    ) {
        let (mut cx, input) = table_editor(cx, TABLE_DOC);
        let events: Rc<RefCell<Vec<(usize, Option<usize>)>>> = Rc::new(RefCell::new(Vec::new()));
        let sink = events.clone();
        cx.update(|_, cx| {
            cx.subscribe(&input, move |_, event: &crate::input::InputEvent, _| {
                if let crate::input::InputEvent::TableInsert { line_start, column } = event {
                    sink.borrow_mut().push((*line_start, *column));
                }
            })
            .detach();
        });
        let header = TABLE_DOC.find('|').unwrap();
        let (cell0, _) =
            cx.update(|_, cx| input.read_with(cx, |state, _| state.table_cell(header, 0).unwrap()));
        let (cell1, _) =
            cx.update(|_, cx| input.read_with(cx, |state, _| state.table_cell(header, 1).unwrap()));
        let marker = |cx: &mut VisualTestContext| {
            cx.update(|_, cx| input.read_with(cx, |state, _| state.table_marker.clone()))
        };

        // The boundary between the two columns, on the top rule.
        let between = point(cell1.origin.x, cell0.origin.y);
        cx.simulate_mouse_move(
            between + point(px(3.), px(2.)),
            None,
            gpui::Modifiers::default(),
        );
        cx.run_until_parked();
        let found = marker(&mut cx).expect("a marker near the boundary");
        assert_eq!((found.line_start, found.column), (header, Some(1)));
        assert!(found.bounds.contains(&between));
        assert_eq!(
            found.bounds.origin,
            found.bounds.origin.map(|v| v.round()),
            "drawn from a whole pixel"
        );

        // The right edge is a boundary too (after the last column); the left
        // edge of a body row's bottom is a row marker.
        cx.simulate_mouse_move(
            point(cell1.origin.x + cell1.size.width, cell0.origin.y),
            None,
            gpui::Modifiers::default(),
        );
        cx.run_until_parked();
        assert_eq!(marker(&mut cx).map(|m| m.column), Some(Some(2)));
        let body = TABLE_DOC.find("| e").unwrap();
        let (body_cell, _) =
            cx.update(|_, cx| input.read_with(cx, |state, _| state.table_cell(body, 0).unwrap()));
        cx.simulate_mouse_move(
            point(
                body_cell.origin.x,
                body_cell.origin.y + body_cell.size.height,
            ),
            None,
            gpui::Modifiers::default(),
        );
        cx.run_until_parked();
        let row = marker(&mut cx).expect("a row marker");
        assert_eq!((row.line_start, row.column), (body, None));

        // Away from any boundary: none. Then back, drawn, and clicked.
        cx.simulate_mouse_move(
            point(cell0.origin.x + px(40.), cell0.origin.y + px(10.)),
            None,
            gpui::Modifiers::default(),
        );
        cx.run_until_parked();
        assert!(marker(&mut cx).is_none());
        cx.simulate_mouse_move(between, None, gpui::Modifiers::default());
        cx.run_until_parked();
        draw(&mut cx);
        let before = cx.update(|_, cx| input.read_with(cx, |state, _| state.cursor()));
        cx.simulate_click(between, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(events.borrow().as_slice(), &[(header, Some(1))]);
        let after = cx.update(|_, cx| input.read_with(cx, |state, _| state.cursor()));
        assert_eq!(before, after, "the click was the marker's, not the text's");

        // A change to the text drops the marker until the pointer moves.
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "x", window, cx)
            })
        });
        assert!(
            marker(&mut cx).is_none(),
            "gone with the layout it was placed on"
        );
    }

    /// The cell the last frame outlined on buffer line `row`.
    fn focused_cell(state: &InputBaseState<EditorMode>, row: usize) -> Option<usize> {
        let layout = state.last_layout.as_ref().expect("layout");
        let vi = layout
            .visible_buffer_lines
            .iter()
            .position(|&b| b == row)
            .expect("the row is visible");
        layout.lines[vi]
            .table
            .as_ref()
            .expect("a table row")
            .focused
    }

    /// A space typed at a cell's end puts the caret in the cell's padding:
    /// still the cell, and drawn after the space, so the caret moves as it
    /// does in prose. Without this the space showed only once a letter
    /// followed it, and the keys in between went to the document.
    #[gpui::test]
    fn a_space_typed_at_a_cell_s_end_stays_in_the_cell_and_moves_the_caret(
        cx: &mut TestAppContext,
    ) {
        let (mut cx, input) = table_editor(cx, TABLE_DOC);
        // The end of `cd`, the header's second cell.
        let end = TABLE_DOC.find("cd").unwrap() + 2;
        cx.update(|_, cx| input.update(cx, |state, cx| state.set_selected_range(end..end, cx)));
        draw(&mut cx);
        let x_before = cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                assert_eq!(focused_cell(state, 1), Some(1));
                let (_, _, pos) = state.line_and_position_for_offset(end);
                pos.expect("a position").x
            })
        });

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, " ", window, cx)
            })
        });
        draw(&mut cx);
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                assert_eq!(state.selected_range(), end + 1..end + 1);
                assert_eq!(
                    focused_cell(state, 1),
                    Some(1),
                    "the caret in the padding is still in the cell"
                );
                let (_, range) = state.table_cell(end, 1).expect("the cell");
                assert_eq!(range.end, end + 1, "the drawn text runs out to the caret");
                let (_, _, pos) = state.line_and_position_for_offset(end + 1);
                let x_after = pos.expect("a position").x;
                assert!(
                    (x_after - x_before - glyph_width(state)).abs() < px(0.5),
                    "the caret moved one glyph right: {x_before:?} -> {x_after:?}"
                );
            })
        });

        // With the caret elsewhere the padding is padding again.
        cx.update(|_, cx| input.update(cx, |state, cx| state.set_selected_range(0..0, cx)));
        draw(&mut cx);
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                assert_eq!(focused_cell(state, 1), None);
                let (_, range) = state.table_cell(end, 1).expect("the cell");
                assert_eq!(range.end, end);
            })
        });
    }

    /// Every offset of a table row has a position, and the point it is drawn
    /// at resolves back to it -- or, for a byte in padding or on a pipe, to
    /// the cell edge it was drawn at.
    #[gpui::test]
    fn every_offset_of_a_table_row_round_trips_through_its_position(cx: &mut TestAppContext) {
        let (mut cx, input) = table_editor(cx, TABLE_DOC);
        draw(&mut cx);
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                let layout = state.last_layout.as_ref().unwrap();
                let vi = layout
                    .visible_buffer_lines
                    .iter()
                    .position(|&b| b == 1)
                    .unwrap();
                let line = &layout.lines[vi];
                let table = line.table.as_ref().unwrap();
                for local in 0..=table.len {
                    let pos = line
                        .position_for_index(local, layout, false)
                        .unwrap_or_else(|| panic!("offset {local} has a position"));
                    // Just left of the boundary: gpui resolves a point past the
                    // last glyph's start to the line's end.
                    let probe = pos + point(px(-0.1), table.text_row_height / 2.);
                    let (back, _) = line
                        .closest_index_for_position(probe, layout)
                        .unwrap_or_else(|| panic!("offset {local}: the point is on the row"));
                    let (_, clamped) = table.cell_of(local);
                    assert_eq!(back, clamped, "offset {local} drawn at {pos:?}");
                }
                assert_eq!(
                    line.position_for_index(table.len + 1, layout, false),
                    None,
                    "past the line is not on it"
                );
            });
        });
    }

    /// A click lands the caret in the clicked cell at the clicked glyph, and
    /// the caret is drawn there, in the frame that was clicked.
    #[gpui::test]
    fn a_click_lands_in_the_cell_at_the_clicked_glyph(cx: &mut TestAppContext) {
        let (mut cx, input) = table_editor(cx, TABLE_DOC);
        for (row, cell, k) in [(1, 0, 0), (1, 0, 2), (1, 1, 1), (3, 0, 1), (3, 1, 0)] {
            draw(&mut cx);
            let target =
                cx.update(|_, cx| input.read_with(cx, |state, _| cell_point(state, row, cell, k)));
            cx.simulate_click(target, gpui::Modifiers::default());
            cx.run_until_parked();
            draw(&mut cx);
            cx.update(|_, cx| {
                input.read_with(cx, |state, _| {
                    let expected = cell_start(state, row, cell) + k;
                    assert_eq!(state.cursor(), expected, "row {row} cell {cell} glyph {k}");
                    let (caret, _) = state.cursor_layout().expect("a caret");
                    assert!(
                        (f32::from(caret.origin.x) - f32::from(target.x + px(0.1))).abs() < 0.5,
                        "caret x {:?} vs click x {:?}",
                        caret.origin.x,
                        target.x
                    );
                    let layout = state.last_layout.as_ref().unwrap();
                    let vi = layout
                        .visible_buffer_lines
                        .iter()
                        .position(|&b| b == row)
                        .unwrap();
                    let table = layout.lines[vi].table.as_ref().unwrap();
                    assert!(
                        (f32::from(caret.size.height) - 0.85 * f32::from(table.text_row_height))
                            .abs()
                            < 0.5,
                        "the caret is one text row tall"
                    );
                    assert_eq!(
                        table.focused,
                        Some(cell),
                        "the clicked cell is the focused one"
                    );
                });
            });
        }
    }

    /// A row grows in the same edit that made a cell wrap, before any frame,
    /// and the prose after the table moves down with it; undo takes it back.
    #[gpui::test]
    fn typing_in_a_cell_grows_the_row_before_the_next_frame(cx: &mut TestAppContext) {
        let (mut cx, input) = table_editor(cx, TABLE_DOC);
        // Narrow the window so a cell wraps after a dozen characters.
        cx.simulate_resize(gpui::size(px(320.), px(600.)));
        cx.run_until_parked();
        draw(&mut cx);

        let target = cx.update(|_, cx| input.read_with(cx, |state, _| cell_point(state, 3, 0, 1)));
        cx.simulate_click(target, gpui::Modifiers::default());
        cx.run_until_parked();

        let (prose_top_before, height_before) = cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                (
                    state.display_map.buffer_line_top(4),
                    state.display_map.buffer_line_height(3),
                )
            })
        });
        assert_eq!(height_before, 1.0);

        // Through the input handler, the way a keystroke arrives, and read
        // back in the same update: nothing has drawn yet.
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, &"x".repeat(40), window, cx);
                assert!(
                    state.display_map.buffer_line_height(3) > 1.0,
                    "the row grew inside the edit"
                );
                assert!(
                    state.display_map.buffer_line_top(4) > prose_top_before,
                    "the prose after the table moved down inside the edit"
                );
            });
        });
        draw(&mut cx);
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                let layout = state.last_layout.as_ref().unwrap();
                let vi = layout
                    .visible_buffer_lines
                    .iter()
                    .position(|&b| b == 3)
                    .unwrap();
                let table = layout.lines[vi].table.as_ref().unwrap();
                assert!(table.rows > 1, "the layout reserves the wrapped rows");
                assert_eq!(
                    table.cells[0].lines.len(),
                    table.rows,
                    "the tallest cell has one shaped line per row"
                );
                assert!(state.text.to_string().contains(&"x".repeat(40)));
            });
        });

        // A selection of the whole wrapped cell is drawn as one rectangle per
        // text row of that cell, not as one sliver or one full-width band.
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                let layout = state.last_layout.as_ref().unwrap();
                let vi = layout
                    .visible_buffer_lines
                    .iter()
                    .position(|&b| b == 3)
                    .unwrap();
                let table = layout.lines[vi].table.as_ref().unwrap();
                let start = layout.visible_line_byte_offsets[vi] + table.cells[0].content.start;
                let end = layout.visible_line_byte_offsets[vi] + table.cells[0].content.end;
                let corners =
                    crate::input::element::TextElement::<EditorMode>::match_range_corners(
                        start..end,
                        layout,
                    )
                    .expect("a selection path");
                assert_eq!(corners.len(), table.rows, "one rectangle per text row");
                let column_right = table.columns[0].x + table.columns[0].width;
                for row in &corners {
                    assert!(
                        row.top_right.x <= column_right,
                        "inside the column: {row:?}"
                    );
                    assert!(
                        row.top_right.x > row.top_left.x + px(6.),
                        "wider than a sliver"
                    );
                }
            });
        });

        // And the real keyboard path adds to it.
        cx.simulate_input("yy");
        draw(&mut cx);
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                assert!(state.text.to_string().contains("xyy"));
            });
        });

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.undo_once(window, cx);
                state.undo_once(window, cx);
            });
        });
        cx.run_until_parked();
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                assert_eq!(state.text.to_string(), TABLE_DOC, "undo restored the bytes");
                assert_eq!(
                    state.display_map.buffer_line_height(3),
                    1.0,
                    "and the height"
                );
                assert_eq!(state.display_map.buffer_line_top(4), prose_top_before);
            });
        });
    }

    /// Assert the text and cursor positions match a spec.
    #[track_caller]
    fn assert_cursors<M: InputModeKind>(
        cx: &mut VisualTestContext,
        input: &Entity<InputBaseState<M>>,
        spec: &str,
    ) {
        let (expected_text, mut expected_cursors) = parse_cursor_spec(spec);
        expected_cursors.sort();

        let (actual_text, mut actual_cursors) = input.read_with(cx, |state, _| {
            (
                state.text.to_string(),
                state
                    .selections
                    .iter()
                    .map(|s| s.cursor_offset())
                    .collect::<Vec<_>>(),
            )
        });
        actual_cursors.sort();

        assert_eq!(
            actual_text, expected_text,
            "Text mismatch:\nExpected: {expected_text:?}\nActual:   {actual_text:?}"
        );
        assert_eq!(
            actual_cursors, expected_cursors,
            "Cursor mismatch:\nExpected: {expected_cursors:?}\nActual:   {actual_cursors:?}"
        );
    }

    #[gpui::test]
    fn test_alt_drag_selects_a_block_and_replaces_each_row(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let view = InputView::<EditorMode>::new(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        for modifiers in [
            gpui::Modifiers {
                alt: true,
                ..Default::default()
            },
            gpui::Modifiers {
                alt: true,
                shift: true,
                ..Default::default()
            },
            #[cfg(target_os = "linux")]
            gpui::Modifiers {
                alt: true,
                control: true,
                ..Default::default()
            },
        ] {
            setup_cursors(&mut cx, &view.input, "|abcd\nabcd\nabcd");
            cx.update(|window, cx| {
                view.input.update(cx, |state, cx| state.focus(window, cx));
            });
            let (start, end) = view.input.read_with(&cx, |state, _| {
                let layout = state.last_layout.as_ref().unwrap();
                let origin = state.last_bounds.unwrap().origin;
                let position = |row: usize, col| {
                    let local = layout.lines[row]
                        .position_for_index(col, layout, false)
                        .unwrap();
                    origin
                        + point(
                            layout.line_number_width + local.x,
                            layout.line_height * (row as f32 + 0.5),
                        )
                };
                (position(0, 1), position(2, 3))
            });
            // A cached Ctrl-hover definition must not steal a column gesture.
            cx.update(|_, cx| {
                view.input.update(cx, |state, _| {
                    state.extras.hover_definition.update(
                        0..4,
                        vec![lsp_types::LocationLink {
                            origin_selection_range: None,
                            target_uri: "file:///tmp/column-selection.rs".parse().unwrap(),
                            target_range: Default::default(),
                            target_selection_range: Default::default(),
                        }],
                    );
                });
            });
            cx.simulate_mouse_down(start, MouseButton::Left, modifiers);
            cx.simulate_mouse_move(end, MouseButton::Left, modifiers);
            cx.simulate_mouse_up(end, MouseButton::Left, modifiers);
            view.input.read_with(&cx, |state, _| {
                let ranges: Vec<_> = state
                    .selections
                    .iter()
                    .map(|sel| sel.start..sel.end)
                    .collect();
                assert_eq!(ranges, vec![1..3, 6..8, 11..13]);
            });
            // Moving after release must leave the block intact.
            cx.simulate_mouse_move(start, None, modifiers);
            cx.simulate_keystrokes("x");
            assert_cursors(&mut cx, &view.input, "ax|d\nax|d\nax|d");
        }
    }

    #[gpui::test]
    fn test_alt_drag_extends_upward_from_an_existing_cursor(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let view = InputView::<EditorMode>::new(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        setup_cursors(&mut cx, &view.input, "abcd\nabcd\na|bcd");
        cx.update(|window, cx| {
            view.input.update(cx, |state, cx| state.focus(window, cx));
        });
        let (start, end) = view.input.read_with(&cx, |state, _| {
            let layout = state.last_layout.as_ref().unwrap();
            let origin = state.last_bounds.unwrap().origin;
            let local = layout.lines[0]
                .position_for_index(1, layout, false)
                .unwrap();
            let x = layout.line_number_width + local.x;
            (
                origin + point(x, layout.line_height * 2.5),
                origin + point(x, layout.line_height * 0.5),
            )
        });
        let modifiers = gpui::Modifiers {
            alt: true,
            ..Default::default()
        };
        cx.simulate_mouse_down(start, MouseButton::Left, modifiers);
        cx.simulate_mouse_move(end, MouseButton::Left, modifiers);
        cx.simulate_mouse_up(end, MouseButton::Left, modifiers);
        assert_cursors(&mut cx, &view.input, "a|bcd\na|bcd\na|bcd");
    }

    #[gpui::test]
    fn test_alt_mouse_release_outside_editor_ends_column_selection(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let view = InputView::<EditorMode>::new(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        setup_cursors(&mut cx, &view.input, "|abcd\nabcd");
        let position = view.input.read_with(&cx, |state, _| {
            let layout = state.last_layout.as_ref().unwrap();
            state.last_bounds.unwrap().origin
                + point(layout.line_number_width + px(2.), layout.line_height * 0.5)
        });
        let modifiers = gpui::Modifiers {
            alt: true,
            ..Default::default()
        };
        cx.simulate_mouse_down(position, MouseButton::Left, modifiers);
        cx.simulate_mouse_up(point(px(-100.), px(-100.)), MouseButton::Left, modifiers);
        view.input.read_with(&cx, |state, _| {
            assert!(!state.selecting);
            assert!(state.column_select_start.is_none());
        });
    }

    #[gpui::test]
    fn test_consumed_keystrokes_keep_cursor_visible(cx: &mut TestAppContext) {
        let view = InputView::<EditorMode>::new(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        setup_cursors(&mut cx, &view.input, "a|b");
        cx.update(|window, cx| {
            view.input.update(cx, |state, cx| {
                state.focus(window, cx);
                state.pause_blink_cursor(cx);
            });
        });
        cx.run_until_parked();
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(300));
        cx.run_until_parked();
        view.input.read_with(&cx, |state, cx| {
            assert!(!state.blink_cursor.read(cx).visible());
        });
        // Copy consumes its shortcut without editing text or moving selections.
        for _ in 0..5 {
            #[cfg(target_os = "macos")]
            cx.simulate_keystrokes("cmd-c");
            #[cfg(not(target_os = "macos"))]
            cx.simulate_keystrokes("ctrl-c");
            cx.run_until_parked();
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(200));
            cx.run_until_parked();
            view.input.read_with(&cx, |state, cx| {
                assert!(state.blink_cursor.read(cx).visible());
            });
        }
    }

    #[gpui::test]
    fn test_multi_cursor_actions_reveal_hidden_carets(cx: &mut TestAppContext) {
        let view = InputView::<EditorMode>::new(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        setup_cursors(&mut cx, &view.input, "ab\na|b\nab");
        cx.update(|window, cx| {
            view.input.update(cx, |state, cx| {
                // Start each action in the hidden phase without depending on a
                // key-down listener: actions and text input also arrive directly.
                for action in 0..6 {
                    state.blink_cursor = cx.new(|_| BlinkCursor::new());
                    assert!(!state.blink_cursor.read(cx).visible());
                    match action {
                        0 => state.add_cursor_above(&AddCursorAbove, window, cx),
                        1 => state.add_cursor_below(&AddCursorBelow, window, cx),
                        2 => state.select_up(&SelectUp, window, cx),
                        3 => state.select_down(&SelectDown, window, cx),
                        4 => state.replace_text_in_range(None, "x", window, cx),
                        _ => state.backspace(&Backspace, window, cx),
                    }
                    assert!(state.blink_cursor.read(cx).visible(), "action {action}");
                }
            });
        });
    }

    #[gpui::test]
    fn test_multi_cursor_keyboard_dispatch(cx: &mut TestAppContext) {
        let view = InputView::<EditorMode>::new(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        setup_cursors(&mut cx, &view.input, "ab\na|b\nab");
        cx.update(|window, cx| {
            view.input.update(cx, |state, cx| state.focus(window, cx));
        });
        #[cfg(target_os = "macos")]
        cx.simulate_keystrokes("cmd-alt-up");
        #[cfg(target_os = "windows")]
        cx.simulate_keystrokes("ctrl-alt-up");
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        cx.simulate_keystrokes("alt-shift-up");
        view.input
            .read_with(&cx, |state, _| assert_eq!(state.selections.len(), 2));
        #[cfg(target_os = "macos")]
        cx.simulate_keystrokes("cmd-alt-down");
        #[cfg(target_os = "windows")]
        cx.simulate_keystrokes("ctrl-alt-down");
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        cx.simulate_keystrokes("alt-shift-down");
        view.input
            .read_with(&cx, |state, _| assert_eq!(state.selections.len(), 3));
        cx.simulate_keystrokes("x");
        assert_cursors(&mut cx, &view.input, "ax|b\nax|b\nax|b");
    }

    #[gpui::test]
    fn test_multi_cursor_platform_word_selection_dispatch(cx: &mut TestAppContext) {
        let view = InputView::<EditorMode>::new(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        setup_cursors(&mut cx, &view.input, "one |two\none |two");
        cx.update(|window, cx| {
            view.input.update(cx, |state, cx| state.focus(window, cx));
        });
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        cx.simulate_keystrokes("alt-shift-right");
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        cx.simulate_keystrokes("ctrl-shift-right");
        cx.simulate_keystrokes("x");
        assert_cursors(&mut cx, &view.input, "one x|\none x|");
        setup_cursors(&mut cx, &view.input, "one two|\none two|");
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        cx.simulate_keystrokes("alt-shift-left");
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        cx.simulate_keystrokes("ctrl-shift-left");
        cx.simulate_keystrokes("x");
        assert_cursors(&mut cx, &view.input, "one x|\none x|");
    }

    #[cfg(not(target_os = "macos"))]
    #[gpui::test]
    fn test_multi_cursor_horizontal_selection_dispatch(cx: &mut TestAppContext) {
        let view = InputView::<EditorMode>::new(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        setup_cursors(&mut cx, &view.input, "ab\na|b");
        cx.update(|window, cx| {
            view.input.update(cx, |state, cx| state.focus(window, cx));
        });
        #[cfg(target_os = "windows")]
        cx.simulate_keystrokes("ctrl-alt-up");
        #[cfg(not(target_os = "windows"))]
        cx.simulate_keystrokes("alt-shift-up");
        #[cfg(target_os = "linux")]
        cx.simulate_keystrokes("shift-right");
        #[cfg(not(target_os = "linux"))]
        cx.simulate_keystrokes("alt-shift-right");
        view.input.read_with(&cx, |state, _| {
            assert_eq!(
                state
                    .selections
                    .iter()
                    .map(|s| s.start..s.end)
                    .collect::<Vec<_>>(),
                vec![4..5, 1..2]
            );
        });
        #[cfg(target_os = "linux")]
        cx.simulate_keystrokes("shift-left shift-left");
        #[cfg(not(target_os = "linux"))]
        cx.simulate_keystrokes("alt-shift-left alt-shift-left");
        view.input.read_with(&cx, |state, _| {
            assert_eq!(
                state
                    .selections
                    .iter()
                    .map(|s| s.start..s.end)
                    .collect::<Vec<_>>(),
                vec![3..4, 0..1]
            );
        });
        cx.simulate_keystrokes("x");
        assert_cursors(&mut cx, &view.input, "x|b\nx|b");
    }

    #[gpui::test]
    fn test_multi_cursor_alt_click_dispatch(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let view = InputView::<EditorMode>::new(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        setup_cursors(&mut cx, &view.input, "a|b\nab\nab");
        cx.update(|window, cx| {
            view.input.update(cx, |state, cx| state.focus(window, cx));
        });
        let position = view.input.read_with(&cx, |state, _| {
            let bounds = state.last_bounds.unwrap();
            let layout = state.last_layout.as_ref().unwrap();
            bounds.origin + point(layout.line_number_width + px(2.), layout.line_height * 1.5)
        });
        cx.simulate_click(
            position,
            gpui::Modifiers {
                alt: true,
                ..Default::default()
            },
        );
        view.input
            .read_with(&cx, |state, _| assert_eq!(state.selections.len(), 2));
    }

    #[gpui::test]
    fn test_word_delete_undo_restores_caret(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        setup_cursors(&mut cx, &view.input, "hello|");
        cx.update(|window, cx| {
            view.input.update(cx, |state, cx| {
                state.delete_previous_word(&DeleteToPreviousWordStart, window, cx);
                state.undo(&Undo, window, cx);
                assert_eq!(state.selected_range(), 5..5);
            });
        });
    }

    #[gpui::test]
    fn test_merged_delete_undo_restores_all_carets(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        setup_cursors(&mut cx, &view.input, "a|b|c");
        cx.update(|window, cx| {
            view.input.update(cx, |state, cx| {
                state.backspace(&Backspace, window, cx);
                state.undo(&Undo, window, cx);
            });
        });
        assert_cursors(&mut cx, &view.input, "a|b|c");
    }

    #[gpui::test]
    fn test_shift_end_respects_soft_wrap_end(cx: &mut TestAppContext) {
        let view = InputView::<EditorMode>::new(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        cx.update(|window, cx| {
            view.input.update(cx, |state, cx| {
                state.set_value("abcdef ".repeat(30), window, cx);
                state.display_map.on_layout_changed(Some(px(60.)), cx);
                let line = state.display_map.line(0).unwrap();
                assert!(line.wrapped_lines.len() > 1);
                let boundary = line.wrapped_lines[0].end;
                state.move_to_with_affinity(boundary, None, true, cx);
                state.select_to_end_of_line(&SelectToEndOfLine, window, cx);
                assert_eq!(state.selected_range(), boundary..state.text.len());
            });
        });
    }

    #[gpui::test]
    fn test_outdent_unindented_unicode_is_unchanged(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        setup_cursors(&mut cx, &view.input, "|你好\n|世界");
        cx.update(|window, cx| {
            view.input.update(cx, |state, cx| {
                state.outdent(false, window, cx);
                state.outdent(true, window, cx);
            });
        });
        assert_cursors(&mut cx, &view.input, "|你好\n|世界");
    }

    #[gpui::test]
    fn test_column_selection_stays_on_unicode_boundaries(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        setup_cursors(&mut cx, &view.input, "ab\n你好\ncd");
        cx.update(|window, cx| {
            view.input.update(cx, |state, cx| {
                state.build_columnar_selection(1, 11, cx);
                let text = state.value();
                for sel in state.selections.iter() {
                    assert!(text.is_char_boundary(sel.start));
                    assert!(text.is_char_boundary(sel.end));
                }
                state.replace_text_in_range(None, "X", window, cx);
            });
        });
    }

    #[gpui::test]
    fn test_editor_decorations_follow_typing(cx: &mut TestAppContext) {
        let view = InputView::<EditorMode>::new(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        cx.update(|window, cx| {
            view.input.update(cx, |state, cx| {
                state.set_value("abc def", window, cx);
                state.create_decorations_collection(
                    vec![crate::input::TextDecoration::new(
                        4..7,
                        gpui::HighlightStyle::default(),
                    )],
                    cx,
                );
                state.set_selected_range(0..0, cx);
                state.replace_text_in_range(None, "X", window, cx);
                let layers = state.extras.decoration_layers();
                assert_eq!(layers.into_iter().flatten().next().unwrap().range, 5..8);
            });
        });
    }

    #[gpui::test]
    fn test_block_indent_tracks_all_preceding_edits(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        setup_cursors(&mut cx, &view.input, "|ab\n|cd\n|ef");
        cx.update(|window, cx| {
            view.input
                .update(cx, |state, cx| state.indent(true, window, cx));
        });
        assert_cursors(&mut cx, &view.input, "  |ab\n  |cd\n  |ef");
        cx.update(|window, cx| {
            view.input
                .update(cx, |state, cx| state.outdent(true, window, cx));
        });
        assert_cursors(&mut cx, &view.input, "|ab\n|cd\n|ef");
    }

    #[gpui::test]
    fn test_block_outdent_clamps_cursor_inside_indent(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        setup_cursors(&mut cx, &view.input, "ab\n | cd");
        cx.update(|window, cx| {
            view.input
                .update(cx, |state, cx| state.outdent(true, window, cx));
        });
        assert_cursors(&mut cx, &view.input, "ab\n|cd");
    }

    #[gpui::test]
    fn test_ime_restores_original_selection(cx: &mut TestAppContext) {
        let view = InputView::build(cx, |state| state);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        cx.update(|window, cx| {
            view.input.update(cx, |state, cx| {
                state.set_value("abc", window, cx);
                state.set_selected_range(1..2, cx);
                state.replace_and_mark_text_in_range(None, "ni", None, window, cx);
                state.replace_text_in_range(None, "你", window, cx);
                state.undo(&Undo, window, cx);
                assert_eq!(state.value(), "abc");
                assert_eq!(state.selected_range(), 1..2);
                state.redo(&Redo, window, cx);
                assert_eq!(state.selected_range(), 4..4);
            });
        });
    }

    #[gpui::test]
    fn test_noop_does_not_change_redo_selection(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        cx.update(|window, cx| {
            view.input.update(cx, |state, cx| {
                state.set_value("abc", window, cx);
                state.set_selected_range(1..1, cx);
                state.replace_text_in_range(None, "X", window, cx);
                state.set_selected_range(0..0, cx);
                state.replace_text_in_range(None, "", window, cx);
                state.undo(&Undo, window, cx);
                state.redo(&Redo, window, cx);
                assert_eq!(state.value(), "aXbc");
                assert_eq!(state.selected_range(), 2..2);
            });
        });
    }

    #[gpui::test]
    fn test_multi_cursor_insert_text(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        let input = view.input;

        setup_cursors(&mut cx, &input, "|hello |world|");
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, ">>>", window, cx);
            });
        });
        assert_cursors(&mut cx, &input, ">>>|hello >>>|world>>>|");
    }

    #[gpui::test]
    fn test_multi_cursor_delete_backward(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        let input = view.input;

        setup_cursors(&mut cx, &input, "|islands| cars|");
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.backspace(&Backspace, window, cx);
            });
        });
        // The first cursor has nothing to delete. The others delete an `s`.
        assert_cursors(&mut cx, &input, "|island| car|");
    }

    #[gpui::test]
    fn test_multi_cursor_delete_forward_merges(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        let input = view.input;

        setup_cursors(&mut cx, &input, "hello| |world");
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.delete(&Delete, window, cx);
            });
        });
        // Adjacent deletions merge into a single cursor.
        assert_cursors(&mut cx, &input, "hello|orld");
    }

    #[gpui::test]
    fn test_multi_cursor_multiline_insert_and_delete(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        let input = view.input;

        setup_cursors(&mut cx, &input, "|1\n|2\n|3");
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "a", window, cx);
            });
        });
        assert_cursors(&mut cx, &input, "a|1\na|2\na|3");

        // The whole multi-edit insert is a single undo transaction.
        input.read_with(&cx, |state, _| {
            assert_eq!(state.undo_manager.undo_count(), 1);
        });

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.backspace(&Backspace, window, cx);
            });
        });
        assert_cursors(&mut cx, &input, "|1\n|2\n|3");
    }

    #[gpui::test]
    fn test_add_cursor_below_preserves_column(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        let input = view.input;

        setup_cursors(&mut cx, &input, "ab|cd\nabcd");
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.add_cursor_below(&AddCursorBelow, window, cx);
            });
        });
        assert_cursors(&mut cx, &input, "ab|cd\nab|cd");
    }

    #[gpui::test]
    fn test_add_cursor_at_rejects_duplicates(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        let input = view.input;

        setup_cursors(&mut cx, &input, "he|llo");
        cx.update(|_, cx| {
            input.update(cx, |state, cx| {
                // Duplicate of the existing cursor is rejected.
                state.add_cursor_at(2, cx);
                assert_eq!(state.selections.len(), 1);
                // A distinct offset adds a cursor.
                state.add_cursor_at(4, cx);
                assert_eq!(state.selections.len(), 2);
            });
        });
    }

    #[gpui::test]
    fn test_multi_cursor_undo_redo_restores_selections(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        let input = view.input;

        setup_cursors(&mut cx, &input, "|1\n|2\n|3");
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "a", window, cx);
            });
        });
        assert_cursors(&mut cx, &input, "a|1\na|2\na|3");

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.undo(&Undo, window, cx);
            });
        });
        assert_cursors(&mut cx, &input, "|1\n|2\n|3");

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.redo(&Redo, window, cx);
            });
        });
        assert_cursors(&mut cx, &input, "a|1\na|2\na|3");
    }

    #[gpui::test]
    fn test_multi_cursor_undo_redo_different_line_lengths(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        let input = view.input;

        setup_cursors(&mut cx, &input, "abc123|\nabc12345|\nabc1234567|");
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "a", window, cx);
            });
        });
        assert_cursors(&mut cx, &input, "abc123a|\nabc12345a|\nabc1234567a|");

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.undo(&Undo, window, cx);
            });
        });
        assert_cursors(&mut cx, &input, "abc123|\nabc12345|\nabc1234567|");

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.redo(&Redo, window, cx);
            });
        });
        assert_cursors(&mut cx, &input, "abc123a|\nabc12345a|\nabc1234567a|");
    }

    #[gpui::test]
    fn test_multi_cursor_undo_multiple_inserts(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        let input = view.input;

        setup_cursors(&mut cx, &input, "|1\n|2\n|3");
        for ch in ['a', 'b', 'c'] {
            cx.update(|window, cx| {
                input.update(cx, |state, cx| {
                    state.replace_text_in_range(None, &ch.to_string(), window, cx);
                });
            });
        }
        assert_cursors(&mut cx, &input, "abc|1\nabc|2\nabc|3");

        // The repeated keystrokes form one typing gesture.
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                assert_eq!(state.undo_manager.undo_count(), 1);
                state.undo(&Undo, window, cx);
            });
        });
        assert_cursors(&mut cx, &input, "|1\n|2\n|3");

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.redo(&Redo, window, cx);
            });
        });
        assert_cursors(&mut cx, &input, "abc|1\nabc|2\nabc|3");
    }

    #[gpui::test]
    fn test_multi_cursor_backspace_run_is_one_undo(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        let input = view.input;

        setup_cursors(&mut cx, &input, "abc|1\nabc|2\nabc|3");
        for _ in 0..3 {
            cx.update(|window, cx| {
                input.update(cx, |state, cx| {
                    state.backspace(&Backspace, window, cx);
                });
            });
        }
        assert_cursors(&mut cx, &input, "|1\n|2\n|3");

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                assert_eq!(state.undo_manager.undo_count(), 1);
                state.undo(&Undo, window, cx);
            });
        });
        assert_cursors(&mut cx, &input, "abc|1\nabc|2\nabc|3");
    }

    #[gpui::test]
    fn test_adding_a_cursor_splits_the_typing_gesture(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        let input = view.input;

        setup_cursors(&mut cx, &input, "|1\n2\n3");
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "a", window, cx);
                state.add_cursor_below(&AddCursorBelow, window, cx);
                state.replace_text_in_range(None, "b", window, cx);
            });
        });
        assert_cursors(&mut cx, &input, "ab|1\n2b|\n3");

        // The keystroke after the cursor was added is its own undo entry.
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.undo(&Undo, window, cx);
            });
        });
        assert_cursors(&mut cx, &input, "a|1\n2|\n3");
    }

    #[gpui::test]
    fn test_multi_cursor_indent_is_one_undo(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        let input = view.input;

        setup_cursors(&mut cx, &input, "|1\n|2\n|3");
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.indent_inline(&IndentInline, window, cx);
                assert_eq!(state.undo_manager.undo_count(), 1);
                state.undo(&Undo, window, cx);
            });
        });
        assert_cursors(&mut cx, &input, "|1\n|2\n|3");
    }

    #[gpui::test]
    fn test_multi_cursor_indent_then_outdent_roundtrips(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        let input = view.input;

        // Cursors at line starts.
        setup_cursors(&mut cx, &input, "|1\n|2\n|3");
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.indent(false, window, cx);
            });
        });
        assert_cursors(&mut cx, &input, "  |1\n  |2\n  |3");

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.outdent(false, window, cx);
            });
        });
        assert_cursors(&mut cx, &input, "|1\n|2\n|3");
    }

    #[gpui::test]
    fn test_inline_outdent_only_removes_line_indentation(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        let input = view.input;

        // A mid-line indent lands at the cursor, and the outdent does not
        // take it back: it only ever removes leading line indentation.
        setup_cursors(&mut cx, &input, "1|2\n1|2");
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.indent(false, window, cx);
            });
        });
        assert_cursors(&mut cx, &input, "1  |2\n1  |2");

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.outdent(false, window, cx);
            });
        });
        assert_cursors(&mut cx, &input, "1  |2\n1  |2");

        // A line with leading indentation loses that, wherever the cursor is.
        setup_cursors(&mut cx, &input, "  1|2\n  1|2");
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.outdent(false, window, cx);
            });
        });
        assert_cursors(&mut cx, &input, "1|2\n1|2");
    }

    #[gpui::test]
    fn test_readonly_multi_cursor_commands_leave_state_unchanged(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        let input = view.input;

        setup_cursors(&mut cx, &input, "a|b\nc|d");
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                let before: Vec<_> = state.selections.iter().copied().collect();
                state.set_readonly(true, cx);
                state.backspace(&Backspace, window, cx);
                state.indent_inline(&IndentInline, window, cx);

                assert_eq!(state.value(), "ab\ncd\n");
                assert_eq!(state.selections.iter().copied().collect::<Vec<_>>(), before);
            });
        });
    }

    #[gpui::test]
    fn test_multi_cursor_edit_preserves_the_active_cursor(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        let input = view.input;

        setup_cursors(&mut cx, &input, "x|\n|y");
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                let mut selections: Vec<_> = state.selections.iter().copied().collect();
                selections.swap(0, 1);
                state.selections.replace_all(selections);
                let active_id = state.active_selection().id;
                state.replace_text_in_range(None, "!", window, cx);
                assert_eq!(state.active_selection().id, active_id);
            });
        });
    }

    /// A drag from prose through the table into prose is one contiguous
    /// selection in document order, drawn as one rectangle per table row.
    #[gpui::test(iterations = 20)]
    fn a_drag_from_prose_through_a_table_selects_in_document_order(cx: &mut TestAppContext) {
        let (mut cx, input) = table_editor(cx, TABLE_DOC);
        draw(&mut cx);
        let (from, via, to) = cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                (
                    prose_point(state, 0, 2),
                    cell_point(state, 1, 1, 1),
                    prose_point(state, 4, 3),
                )
            })
        });
        let mods = gpui::Modifiers::default();
        cx.simulate_mouse_down(from, MouseButton::Left, mods);
        cx.simulate_mouse_move(via, Some(MouseButton::Left), mods);
        cx.simulate_mouse_move(to, Some(MouseButton::Left), mods);
        cx.simulate_mouse_up(to, MouseButton::Left, mods);
        cx.run_until_parked();
        draw(&mut cx);

        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                let line4 = state.text.line_start_offset(4);
                let selected = state.selected_range();
                assert_eq!((selected.start, selected.end), (2, line4 + 3));

                let layout = state.last_layout.as_ref().unwrap();
                let corners =
                    crate::input::element::TextElement::<EditorMode>::match_range_corners(
                        selected.start..selected.end,
                        layout,
                    )
                    .expect("a selection path");
                assert_eq!(corners.len(), 5, "prose, three table rows, prose");
                for (ix, row) in corners[1..4].iter().enumerate() {
                    assert_eq!(row.top_left.x, px(0.), "table row {ix} from the left edge");
                    assert_eq!(
                        row.top_right.x,
                        layout.wrap_width.unwrap(),
                        "table row {ix} to the right edge"
                    );
                }
            });
        });
    }

    /// Select-all covers every table row edge to edge.
    #[gpui::test]
    fn select_all_covers_every_table_row_edge_to_edge(cx: &mut TestAppContext) {
        let (mut cx, input) = table_editor(cx, TABLE_DOC);
        draw(&mut cx);
        cx.update(|window, cx| {
            input.update(cx, |state, cx| state.select_all(window, cx));
        });
        draw(&mut cx);
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                let layout = state.last_layout.as_ref().unwrap();
                let corners =
                    crate::input::element::TextElement::<EditorMode>::match_range_corners(
                        0..state.text.len(),
                        layout,
                    )
                    .expect("a selection path");
                let width = layout.wrap_width.unwrap();
                for row in &corners[1..4] {
                    assert_eq!((row.top_left.x, row.top_right.x), (px(0.), width));
                }
            });
        });
    }

    /// Shift-click extends the selection into a cell; inside one cell it is a
    /// text selection, across cells it takes whole cells.
    #[gpui::test]
    fn shift_click_extends_the_selection_across_cells(cx: &mut TestAppContext) {
        let (mut cx, input) = table_editor(cx, TABLE_DOC);
        draw(&mut cx);
        let first = cx.update(|_, cx| input.read_with(cx, |state, _| cell_point(state, 1, 0, 0)));
        cx.simulate_click(first, gpui::Modifiers::default());
        cx.run_until_parked();
        draw(&mut cx);
        let same_cell =
            cx.update(|_, cx| input.read_with(cx, |state, _| cell_point(state, 1, 0, 2)));
        cx.simulate_click(
            same_cell,
            gpui::Modifiers {
                shift: true,
                ..Default::default()
            },
        );
        cx.run_until_parked();
        draw(&mut cx);
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                let start = cell_start(state, 1, 0);
                assert_eq!(
                    (state.selected_range().start, state.selected_range().end),
                    (start, start + 2)
                );
                let layout = state.last_layout.as_ref().unwrap();
                let corners =
                    crate::input::element::TextElement::<EditorMode>::match_range_corners(
                        start..start + 2,
                        layout,
                    )
                    .unwrap();
                assert_eq!(corners.len(), 1);
                let glyph = glyph_width(state);
                assert!(
                    (f32::from(corners[0].top_right.x - corners[0].top_left.x)
                        - 2. * f32::from(glyph))
                    .abs()
                        < 0.5,
                    "two glyphs wide inside the cell"
                );
            });
        });

        let other_row =
            cx.update(|_, cx| input.read_with(cx, |state, _| cell_point(state, 3, 1, 1)));
        cx.simulate_click(
            other_row,
            gpui::Modifiers {
                shift: true,
                ..Default::default()
            },
        );
        cx.run_until_parked();
        draw(&mut cx);
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                let start = cell_start(state, 1, 0);
                let end = cell_start(state, 3, 1) + 1;
                assert_eq!(
                    (state.selected_range().start, state.selected_range().end),
                    (start, end)
                );
                let layout = state.last_layout.as_ref().unwrap();
                let corners =
                    crate::input::element::TextElement::<EditorMode>::match_range_corners(
                        start..end,
                        layout,
                    )
                    .unwrap();
                assert_eq!(corners.len(), 3, "header, delimiter, body");
                let width = layout.wrap_width.unwrap();
                assert_eq!(corners[0].top_left.x, px(0.), "from the first cell's edge");
                assert_eq!(corners[0].top_right.x, width, "past the header's end");
                assert_eq!(corners[2].top_right.x, width, "to the last cell's edge");
            });
        });
    }

    /// Narrowing the window re-wraps the cells: a long cell that fit becomes
    /// several rows, and the table row grows with it.
    #[gpui::test]
    fn resizing_the_window_rewraps_the_cells(cx: &mut TestAppContext) {
        const LONG: &str =
            "| head | h |\n| --- | --- |\n| abcdefghijklmnopqrstuvwxyzabcdefghij | b |\n";
        let (mut cx, input) = table_editor(cx, LONG);
        draw(&mut cx);
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                assert_eq!(
                    state.display_map.buffer_line_height(2),
                    1.0,
                    "wide window: one row"
                );
            });
        });
        cx.simulate_resize(gpui::size(px(320.), px(600.)));
        cx.run_until_parked();
        draw(&mut cx);
        cx.update(|_, cx| {
            input.read_with(cx, |state, _| {
                assert!(
                    state.display_map.buffer_line_height(2) > 1.0,
                    "narrow window: wrapped"
                );
                let layout = state.last_layout.as_ref().unwrap();
                let vi = layout
                    .visible_buffer_lines
                    .iter()
                    .position(|&b| b == 2)
                    .unwrap();
                let table = layout.lines[vi].table.as_ref().unwrap();
                assert_eq!(table.cells[0].lines.len(), table.rows);
                assert_eq!(
                    table.cells[1].lines.len(),
                    1,
                    "the short cell stays one row"
                );
            });
        });
    }

    #[gpui::test]
    fn test_block_indent_outdent_with_selection(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        let input = view.input;

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.set_value("line1\nline2\nline3", window, cx);
                let id = state.selections.generate_id();
                state
                    .selections
                    .replace_all(vec![CursorSelection::new(id, 0, 17)]);
                cx.notify();
            });
        });

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.indent(true, window, cx);
            });
        });
        input.read_with(&cx, |state, _| {
            assert_eq!(state.text.to_string(), "  line1\n  line2\n  line3");
        });

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.outdent(true, window, cx);
            });
        });
        input.read_with(&cx, |state, _| {
            assert_eq!(state.text.to_string(), "line1\nline2\nline3");
        });
    }

    #[gpui::test]
    fn test_multi_cursor_word_movement(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        let input = view.input;

        setup_cursors(
            &mut cx,
            &input,
            "on|e two three\none t|wo three\non|e two three",
        );

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.move_to_next_word(&MoveToNextWord, window, cx);
            });
        });
        assert_cursors(
            &mut cx,
            &input,
            "one| two three\none two| three\none| two three",
        );

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.move_to_previous_word(&MoveToPreviousWord, window, cx);
            });
        });
        assert_cursors(
            &mut cx,
            &input,
            "|one two three\none |two three\n|one two three",
        );

        // Move to end/start of document collapses to a single cursor.
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.move_to_end(&MoveToEnd, window, cx);
            });
        });
        input.read_with(&cx, |state, _| {
            let cursors: Vec<usize> = state.selections.iter().map(|s| s.cursor_offset()).collect();
            assert_eq!(cursors, vec![state.text.len()]);
        });

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.move_to_start(&MoveToStart, window, cx);
            });
        });
        input.read_with(&cx, |state, _| {
            let cursors: Vec<usize> = state.selections.iter().map(|s| s.cursor_offset()).collect();
            assert_eq!(cursors, vec![0]);
        });
    }

    #[gpui::test]
    fn test_multi_cursor_selection_commands(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        let input = view.input;

        setup_cursors(
            &mut cx,
            &input,
            "on|e two three\none t|wo three\non|e two three",
        );
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.select_to_start_of_line(&SelectToStartOfLine, window, cx);
            });
        });
        assert_cursors(
            &mut cx,
            &input,
            "|one two three\n|one two three\n|one two three",
        );

        // Select to document start collapses to the active cursor only.
        setup_cursors(
            &mut cx,
            &input,
            "on|e two three\none t|wo three\non|e two three",
        );
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.select_to_start(&SelectToStart, window, cx);
            });
        });
        input.read_with(&cx, |state, _| {
            let cursors: Vec<usize> = state.selections.iter().map(|s| s.cursor_offset()).collect();
            assert_eq!(cursors, vec![0]);
        });
    }

    #[gpui::test]
    fn test_multi_cursor_replace_selection(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        let input = view.input;

        setup_cursors(&mut cx, &input, "|a\n|b\n|c");
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.select_right(&SelectRight, window, cx);
            });
        });
        input.read_with(&cx, |state, _| {
            let ranges: Vec<_> = state.selections.iter().map(|s| (s.start, s.end)).collect();
            assert_eq!(ranges, vec![(0, 1), (2, 3), (4, 5)]);
        });

        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "x", window, cx);
            });
        });
        assert_cursors(&mut cx, &input, "x|\nx|\nx|");
    }

    #[gpui::test]
    fn test_multi_cursor_escape_collapses(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        let input = view.input;

        setup_cursors(&mut cx, &input, "|a\n|b\n|c");
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.escape(&Escape, window, cx);
            });
        });
        input.read_with(&cx, |state, _| {
            assert_eq!(state.selections.len(), 1);
        });
    }

    #[gpui::test]
    fn test_build_columnar_selection(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        let input = view.input;

        setup_cursors(&mut cx, &input, "abcd\nabcd\nabcd");
        cx.update(|_, cx| {
            input.update(cx, |state, cx| {
                // From row 0 col 1 to row 2 col 3.
                state.build_columnar_selection(1, 13, cx);
                let ranges: Vec<_> = state.selections.iter().map(|s| (s.start, s.end)).collect();
                assert_eq!(ranges, vec![(1, 3), (6, 8), (11, 13)]);
            });
        });
    }

    #[gpui::test]
    fn test_multi_cursor_paste_distributes_lines(cx: &mut TestAppContext) {
        let view = multi_line(cx);
        let mut cx = VisualTestContext::from_window(view.window_handle.into(), cx);
        let input = view.input;

        setup_cursors(&mut cx, &input, "|1\n|2\n|3");
        cx.update(|_, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string("x\ny\nz".to_string()));
        });
        cx.update(|window, cx| {
            input.update(cx, |state, cx| {
                state.paste(&Paste, window, cx);
            });
        });
        // One clipboard line per cursor.
        assert_cursors(&mut cx, &input, "x|1\ny|2\nz|3");
    }
}

/// Methods that only a single-line input offers.
impl InputBaseState<crate::input::InputMode> {
    /// Create a single-line text input state.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_in_mode(window, cx)
    }

    /// Set a custom step function of the [`super::NumberInput`].
    ///
    /// The `f` receives the current value and the [`StepAction`], and returns
    /// the step to apply, so the step can vary with the value.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // At the boundary 1.0 the step is 0.1 going down and 0.5 going up.
    /// InputState::new(window, cx).step_by(|value, action, _cx| match action {
    ///     StepAction::Increment => if value < 1.0 { 0.1 } else { 0.5 },
    ///     StepAction::Decrement => if value <= 1.0 { 0.1 } else { 0.5 },
    /// })
    /// ```
    pub fn step_by(mut self, f: impl Fn(f64, StepAction, &mut App) -> f64 + 'static) -> Self {
        self.number_step = Some(NumberStep::by_value(f));
        self
    }

    /// Set with password masked state.
    pub fn masked(mut self, masked: bool) -> Self {
        self.masked = masked;
        self
    }

    /// Set the password masked state of the input field.
    pub fn set_masked(&mut self, masked: bool, _: &mut Window, cx: &mut Context<Self>) {
        self.masked = masked;
        cx.notify();
    }

    /// Set the regular expression pattern of the input field.
    pub fn pattern(mut self, pattern: regex::Regex) -> Self {
        self.pattern = Some(pattern);
        self
    }

    /// Set the regular expression pattern of the input field with reference.
    pub fn set_pattern(
        &mut self,
        pattern: regex::Regex,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.pattern = Some(pattern);
    }

    /// Set the validation function of the input field.
    pub fn validate(mut self, f: impl Fn(&str, &mut App) -> bool + 'static) -> Self {
        self.validate = Some(Box::new(f));
        self
    }

    pub fn set_validator(
        &mut self,
        validate: impl Fn(&str, &mut App) -> bool + 'static,
        _cx: &mut Context<Self>,
    ) {
        self.validate = Some(Box::new(validate));
    }

    /// Set the step value of the [`super::NumberInput`] for increment/decrement.
    ///
    /// If any of `step`, `min`, `max` is set, the [`super::NumberInput`] will
    /// update the value internally (step by `step`, default 1, clamp to the
    /// `min`/`max` range and emit [`InputEvent::Change`]) instead of emitting
    /// [`super::NumberInputEvent::Step`].
    ///
    /// See also [`Self::step_by`] to calculate the step value
    /// based on the current value.
    pub fn step(mut self, step: impl Into<NumberStep>) -> Self {
        self.number_step = Some(step.into());
        self
    }

    /// Set the minimum value of the [`super::NumberInput`].
    ///
    /// The value will be clamped to the minimum value on stepping and on
    /// blur (only if the clamped value passes the `pattern`/`validate` check).
    /// See also [`Self::step`].
    pub fn min(mut self, min: f64) -> Self {
        self.number_min = Some(min);
        self
    }

    /// Set the maximum value of the [`super::NumberInput`].
    ///
    /// The value will be clamped to the maximum value on stepping and on
    /// blur (only if the clamped value passes the `pattern`/`validate` check).
    /// See also [`Self::step`].
    pub fn max(mut self, max: f64) -> Self {
        self.number_max = Some(max);
        self
    }

    /// Update the step value after construction, `None` to fall back to
    /// emitting [`super::NumberInputEvent::Step`] (if `min`, `max` are unset).
    ///
    /// See [`Self::step`] and [`Self::step_by`].
    pub fn set_step(
        &mut self,
        step: impl Into<Option<NumberStep>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        self.number_step = step.into();
    }

    /// Update the minimum value after construction. See [`Self::min`].
    pub fn set_min(&mut self, min: Option<f64>, _: &mut Window, _: &mut Context<Self>) {
        self.number_min = min;
    }

    /// Update the maximum value after construction. See [`Self::max`].
    pub fn set_max(&mut self, max: Option<f64>, _: &mut Window, _: &mut Context<Self>) {
        self.number_max = max;
    }

    /// Set true to show spinner at the input right.
    pub fn set_loading(&mut self, loading: bool, _: &mut Window, cx: &mut Context<Self>) {
        self.loading = loading;
        cx.notify();
    }
}

/// Methods shared by the two multi-line modes, and reachable on neither a
/// single-line input nor anything else.
impl<M: crate::input::MultiLineMode> InputBaseState<M> {
    /// Set this input is searchable, default is false (Default true for Code Editor).
    #[doc(hidden)]
    pub fn searchable(mut self, searchable: bool) -> Self {
        self.searchable = searchable;
        self
    }

    pub fn set_searchable(&mut self, searchable: bool, cx: &mut Context<Self>) {
        self.searchable = searchable;
        cx.notify();
    }

    /// Set the soft wrap mode, default is true.
    #[doc(hidden)]
    pub fn soft_wrap(mut self, wrap: bool) -> Self {
        self.soft_wrap = wrap;
        self
    }

    /// Update the soft wrap mode, default is true.
    pub fn set_soft_wrap(&mut self, wrap: bool, _: &mut Window, cx: &mut Context<Self>) {
        self.soft_wrap = wrap;
        if wrap {
            let wrap_width = self
                .last_layout
                .as_ref()
                .and_then(|b| b.wrap_width)
                .unwrap_or(self.input_bounds.size.width);

            self.display_map.on_layout_changed(Some(wrap_width), cx);

            // Reset scroll to left 0
            let mut offset = self.scroll_handle.offset();
            offset.x = px(0.);
            self.scroll_handle.set_offset(offset);
        } else {
            self.display_map.on_layout_changed(None, cx);
        }
        cx.notify();
    }

    /// Set how soft-wrapped continuation lines are indented, default is [`WrappingIndent::Same`]
    #[doc(hidden)]
    pub fn wrapping_indent(mut self, wrapping_indent: WrappingIndent) -> Self {
        self.wrapping_indent = wrapping_indent;
        self
    }

    /// Update how soft-wrapped continuation lines are indented.
    pub fn set_wrapping_indent(
        &mut self,
        wrapping_indent: WrappingIndent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.wrapping_indent = wrapping_indent;
        self.display_map.set_wrapping_indent(wrapping_indent, cx);
        cx.notify();
    }
}

/// Methods that only ordinary multi-line text offers.
impl InputBaseState<crate::input::TextareaMode> {
    /// Create a multi-line text state.
    ///
    /// Being multi-line is carried by the mode, not by the layout, so the
    /// default plain-text layout needs no adjustment here.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_in_mode(window, cx)
    }

    pub fn set_auto_grow(&mut self, min_rows: usize, max_rows: usize, cx: &mut Context<Self>) {
        self.mode = LayoutMode::auto_grow(min_rows, max_rows.max(min_rows));
        cx.notify();
    }

    /// Set the number of rows for the multi-line Textarea.
    ///
    /// This is only used when `multi_line` is set to true.
    ///
    /// default: 2
    #[doc(hidden)]
    pub fn rows(mut self, rows: usize) -> Self {
        match &mut self.mode {
            LayoutMode::PlainText { rows: r, .. } | LayoutMode::CodeEditor { rows: r, .. } => {
                *r = rows
            }
            LayoutMode::AutoGrow {
                max_rows: max_r,
                rows: r,
                ..
            } => {
                *r = rows;
                *max_r = rows;
            }
        }
        self
    }

    pub fn set_rows(&mut self, rows: usize, cx: &mut Context<Self>) {
        match &mut self.mode {
            LayoutMode::PlainText { rows: value, .. }
            | LayoutMode::CodeEditor { rows: value, .. } => *value = rows,
            LayoutMode::AutoGrow {
                rows: value,
                max_rows,
                ..
            } => {
                *value = rows;
                *max_rows = rows;
            }
        }
        cx.notify();
    }

    /// Grow with the content from `min_rows` through `max_rows`.
    pub fn auto_grow(mut self, min_rows: usize, max_rows: usize) -> Self {
        self.mode = LayoutMode::auto_grow(min_rows, max_rows);
        self
    }
}

/// Methods that only a source-code editor offers.
impl InputBaseState<crate::input::EditorMode> {
    /// Create a source-code editor state.
    ///
    /// Default options: line numbers on, tab size 2 with soft tabs, indent
    /// guides on, multi-line, and search enabled. Set the language for syntax
    /// highlighting with [`Self::language`]; without one the text is shown
    /// unhighlighted.
    ///
    /// The editor aims at simple code editing or display, not at being a
    /// full-featured code editor. It offers syntax highlighting, auto indent,
    /// line numbers, and handles large text up to about 50K lines.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut state = Self::new_in_mode(window, cx);
        state.mode = LayoutMode::code_editor();
        state.searchable = true;
        state
    }

    /// Set the language to highlight, e.g. `"rust"`.
    ///
    /// See [`Self::set_highlighter`] to change it after construction.
    pub fn language(mut self, language: impl Into<SharedString>) -> Self {
        if let LayoutMode::CodeEditor {
            language: l,
            highlighter,
            ..
        } = &mut self.mode
        {
            *l = language.into();
            *highlighter.borrow_mut() = None;
        }
        self
    }

    /// Set enable/disable code folding.
    ///
    /// Default: true
    #[doc(hidden)]
    pub fn folding(mut self, folding: bool) -> Self {
        if let LayoutMode::CodeEditor { folding: f, .. } = &mut self.mode {
            *f = folding;
        }
        self
    }

    /// Set code folding at runtime.
    ///
    /// When disabling, all existing folds are cleared.
    pub fn set_folding(&mut self, folding: bool, _: &mut Window, cx: &mut Context<Self>) {
        if let LayoutMode::CodeEditor { folding: f, .. } = &mut self.mode {
            *f = folding;
        }
        if !folding {
            self.display_map.clear_folds();
        }
        cx.notify();
    }

    /// Unfold any folded ranges that hide the given position.
    ///
    /// Use this to reveal a position before acting on it (e.g. before
    /// [`Self::set_cursor_position`], which stops at a fold boundary),
    /// without touching folds elsewhere in the buffer. Fold candidates are
    /// kept, so the opened ranges can be folded again from the gutter.
    ///
    /// A fold keeps its own first and last line visible, so a position on
    /// either of them opens nothing. Nested folds all open, since opening
    /// only the outermost would leave the position hidden.
    ///
    /// Returns whether any fold was opened.
    pub fn unfold_at(&mut self, position: impl Into<Position>, cx: &mut Context<Self>) -> bool {
        let offset = self.text.position_to_offset(&position.into());
        let line = self.text.offset_to_point(offset).row;
        // A fold hides start_line + 1 ..= end_line - 1, so a line is hidden
        // exactly when some folded range strictly contains it.
        let covering: Vec<usize> = self
            .display_map
            .folded_ranges()
            .iter()
            .filter(|fold| line > fold.start_line && line < fold.end_line)
            .map(|fold| fold.start_line)
            .collect();
        if covering.is_empty() {
            return false;
        }

        for start_line in covering {
            self.display_map.set_folded(start_line, false);
        }
        cx.notify();
        true
    }

    /// Set enable/disable line number.
    #[doc(hidden)]
    pub fn line_number(mut self, line_number: bool) -> Self {
        if let LayoutMode::CodeEditor { line_number: l, .. } = &mut self.mode {
            *l = line_number;
        }
        self
    }

    /// Set line number.
    pub fn set_line_number(&mut self, line_number: bool, _: &mut Window, cx: &mut Context<Self>) {
        if let LayoutMode::CodeEditor { line_number: l, .. } = &mut self.mode {
            *l = line_number;
        }
        cx.notify();
    }
}
