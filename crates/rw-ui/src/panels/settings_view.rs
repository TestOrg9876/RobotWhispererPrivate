//! Settings: the things that are chosen once and then left alone.
//!
//! The theme picker used to be a button permanently occupying the title bar,
//! which is a setting wearing a toolbar button's clothes. It lives here.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};

use crate::prefs::Prefs;
use crate::theme::{self, Preference};
use crate::tokens;

/// Emitted when a setting changes, so the shell can persist it.
#[derive(Debug, Clone)]
pub enum SettingsEvent {
    ThemeChosen(Preference),
}

pub struct SettingsView {
    focus_handle: FocusHandle,
    preference: Preference,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl SettingsView {
    pub fn view(prefs: &Prefs, cx: &mut App) -> Entity<Self> {
        let preference = prefs.theme();
        cx.new(|cx| Self {
            focus_handle: cx.focus_handle(),
            preference,
        })
    }

    fn choose(&mut self, preference: Preference, cx: &mut Context<Self>) {
        self.preference = preference.clone();
        cx.emit(SettingsEvent::ThemeChosen(preference));
        cx.notify();
    }

    /// One theme, shown as itself: the swatches are the theme's own colours, so
    /// the list is a preview rather than a column of names.
    fn swatch(&self, preference: Preference, label: &str, cx: &mut Context<Self>) -> AnyElement {
        let chosen = self.preference == preference;

        h_flex()
            .id(gpui::SharedString::from(format!("theme:{label}")))
            .h(px(tokens::CONTROL_HEIGHT))
            .w_full()
            .px_3()
            .gap_3()
            .items_center()
            .rounded(cx.theme().radius)
            .when(chosen, |row| row.bg(cx.theme().list_active))
            .when(!chosen, |row| {
                row.hover(|row| row.bg(cx.theme().list_hover))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .child(label.to_string()),
            )
            .when(chosen, |row| {
                row.child(
                    Icon::new(IconName::Check)
                        .small()
                        .text_color(cx.theme().primary),
                )
            })
            .on_click(
                cx.listener(move |this, _: &ClickEvent, _, cx| this.choose(preference.clone(), cx)),
            )
            .into_any_element()
    }
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let system = self.swatch(Preference::System, "Match system", cx);
        let themes: Vec<_> = theme::names()
            .into_iter()
            .map(|name| {
                let label = name.clone();
                self.swatch(Preference::Named(name), &label, cx)
            })
            .collect();

        v_flex().id("settings").w_full().gap_3().child(
            v_flex()
                .gap_1()
                .child(tokens::section_label("Theme", cx))
                .child(
                    v_flex()
                        .id("theme-list")
                        .max_h(px(340.))
                        .overflow_y_scroll()
                        .gap_0p5()
                        .child(system)
                        .children(themes),
                ),
        )
    }
}
