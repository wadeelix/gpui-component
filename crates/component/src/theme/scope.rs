//! A theme for one part of the window: a dark sidebar beside a light
//! document, say.
//!
//! The theme is read at two times. Elements take their colors while they are
//! built, and some controls read the theme again when they are laid out and
//! painted. [`ScopedTheme::enter`] covers the first, [`ScopedTheme::wrap`] the
//! second; a part of the window needs both:
//!
//! ```ignore
//! let panel = scoped.enter(cx, |cx| self.render_panel(cx));
//! div().child(scoped.wrap(panel))
//! ```
//!
//! Overlays a scoped part opens — menus, popovers, tooltips — are drawn after
//! the window's tree, outside the scope, in the global theme.

use std::{borrow::BorrowMut, rc::Rc};

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, Global, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, Pixels, Window,
};

use super::{Theme, ThemeConfig};

/// The component themes standing in for the global one; the last is current.
#[derive(Default)]
pub(super) struct ThemeScopes(pub(super) Vec<Rc<Theme>>);

impl Global for ThemeScopes {}

/// A theme for a part of the window, together with its Base projection.
#[derive(Clone)]
pub struct ScopedTheme {
    theme: Rc<Theme>,
    base: Rc<gpui_base::Theme>,
}

impl ScopedTheme {
    /// The application's theme with `config` laid over it. The colors, radius
    /// and syntax highlighting are the config's; the fonts stay the
    /// application's, so the part reads as the same window.
    pub fn new(config: &Rc<ThemeConfig>, cx: &App) -> Self {
        let global = cx.global::<Theme>();
        let mut theme = global.clone();
        theme.apply_config(config);
        theme.font_family = global.font_family.clone();
        theme.font_size = global.font_size;
        theme.mono_font_family = global.mono_font_family.clone();
        theme.mono_font_size = global.mono_font_size;
        let base = Rc::new(theme.base_theme());
        Self {
            theme: Rc::new(theme),
            base,
        }
    }

    /// The scoped theme itself.
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Runs `f` with this theme current, for building a part's elements.
    pub fn enter<C: BorrowMut<App>, R>(&self, cx: &mut C, f: impl FnOnce(&mut C) -> R) -> R {
        self.push(cx.borrow_mut());
        let result = f(cx);
        Self::pop(cx.borrow_mut());
        result
    }

    /// Lays out and paints `child` with this theme current.
    pub fn wrap(&self, child: impl IntoElement) -> ThemeScope {
        ThemeScope {
            scoped: self.clone(),
            child: child.into_any_element(),
        }
    }

    fn push(&self, cx: &mut App) {
        cx.default_global::<ThemeScopes>()
            .0
            .push(self.theme.clone());
        gpui_base::Theme::push_scope(self.base.clone(), cx);
    }

    fn pop(cx: &mut App) {
        gpui_base::Theme::pop_scope(cx);
        if cx.has_global::<ThemeScopes>() {
            cx.global_mut::<ThemeScopes>().0.pop();
        }
    }
}

/// An element whose child is laid out, prepainted and painted with a
/// [`ScopedTheme`] current. Built by [`ScopedTheme::wrap`].
pub struct ThemeScope {
    scoped: ScopedTheme,
    child: AnyElement,
}

impl IntoElement for ThemeScope {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ThemeScope {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.scoped.push(cx);
        let layout_id = self.child.request_layout(window, cx);
        ScopedTheme::pop(cx);
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.scoped.push(cx);
        self.child.prepaint(window, cx);
        ScopedTheme::pop(cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.scoped.push(cx);
        self.child.paint(window, cx);
        ScopedTheme::pop(cx);
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use gpui::{
        Context, Hsla, IntoElement, ParentElement as _, Render, Styled as _, TestAppContext,
        VisualTestContext, Window, canvas, div, px, size,
    };

    use crate::{ActiveTheme as _, Theme, ThemeConfig, ThemeMode};

    use super::ScopedTheme;

    fn dark_config() -> Rc<ThemeConfig> {
        let config: ThemeConfig = serde_json::from_value(serde_json::json!({
            "name": "Scoped Dark",
            "mode": "dark",
            "colors": { "background": "#282a36", "sidebar.background": "#21222c" }
        }))
        .unwrap();
        Rc::new(config)
    }

    #[gpui::test]
    fn the_scope_answers_while_building_and_the_global_after(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::init(cx);
            Theme::change(ThemeMode::Light, None, cx);
            let light = cx.theme().background;
            let scoped = ScopedTheme::new(&dark_config(), cx);

            let inside = scoped.enter(cx, |cx| (cx.theme().background, cx.theme().is_dark()));
            assert_ne!(inside.0, light);
            assert!(inside.1, "the scope is dark");
            assert_eq!(
                scoped.enter(cx, |cx| gpui_base::Theme::global(cx)
                    .tokens
                    .colors
                    .background),
                inside.0,
                "the Base layer sees the scope too"
            );
            assert_eq!(cx.theme().background, light, "the global is back");
            assert!(!cx.theme().is_dark());
        });
    }

    #[gpui::test]
    fn paint_inside_a_wrapped_child_sees_the_scope(cx: &mut TestAppContext) {
        struct Root {
            scoped: ScopedTheme,
            seen: Rc<std::cell::Cell<Option<(Hsla, Hsla)>>>,
        }

        impl Render for Root {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let seen = self.seen.clone();
                div().size_full().child(
                    self.scoped.wrap(
                        canvas(
                            |_, _, _| {},
                            move |_, _, _, cx| {
                                seen.set(Some((
                                    cx.theme().background,
                                    gpui_base::Theme::global(cx).tokens.colors.background,
                                )))
                            },
                        )
                        .size(px(10.)),
                    ),
                )
            }
        }

        cx.update(|cx| {
            crate::init(cx);
            Theme::change(ThemeMode::Light, None, cx);
        });
        let seen = Rc::new(std::cell::Cell::new(None));
        let captured = seen.clone();
        let window = cx.open_window(size(px(100.), px(100.)), move |_, cx| Root {
            scoped: ScopedTheme::new(&dark_config(), cx),
            seen: captured,
        });
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let (component, base) = seen.get().expect("the child painted");
        let light = cx.update(|_, cx| cx.theme().background);
        assert_ne!(component, light, "paint saw the scoped theme");
        assert_eq!(base, component, "and so did the Base layer");
    }
}
