//! What the centre shows when nothing is open.
//!
//! A dock panel rather than a special case in the shell: it lives in the centre
//! tab strip like everything else, and is removed once a request takes its
//! place.

use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    kbd::Kbd,
    v_flex,
};

use crate::actions::CommandPalette;

/// What the welcome screen asks the shell to do.
#[derive(Debug, Clone, Copy)]
pub enum WelcomeEvent {
    NewRequest,
    ManageConnections,
    CommandPalette,
}

pub struct WelcomePanel {
    focus_handle: FocusHandle,
}

impl EventEmitter<WelcomeEvent> for WelcomePanel {}
impl EventEmitter<PanelEvent> for WelcomePanel {}

impl WelcomePanel {
    pub fn view(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self {
            focus_handle: cx.focus_handle(),
        })
    }
}

impl Focusable for WelcomePanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for WelcomePanel {
    fn panel_name(&self) -> &'static str {
        "Welcome"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        "Welcome"
    }
}

impl Render for WelcomePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Read from the keymap rather than written out, so the hint stays correct
        // on whichever platform this is and if the binding ever changes.
        let shortcut = window
            .highest_precedence_binding_for_action(&CommandPalette)
            .and_then(|binding| binding.keystrokes().first().map(|key| key.inner().clone()));

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_6()
            .bg(cx.theme().background)
            .child(
                v_flex()
                    .items_center()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size_16()
                            .rounded(cx.theme().radius_lg)
                            .bg(cx.theme().secondary)
                            .text_color(cx.theme().primary)
                            .child(Icon::new(IconName::Bot).size_8()),
                    )
                    .child(
                        v_flex()
                            .gap_1p5()
                            .items_center()
                            .child(
                                div()
                                    .text_2xl()
                                    .font_semibold()
                                    .text_color(cx.theme().foreground)
                                    .child("Robot Whisperer"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(
                                        "Connect to your ROS systems, then save the calls you \
                                         make against them.",
                                    ),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("welcome-connect")
                            .primary()
                            .icon(IconName::Globe)
                            .label("Connect to a ROS system")
                            .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                                cx.emit(WelcomeEvent::ManageConnections)
                            })),
                    )
                    .child(
                        Button::new("welcome-new-request")
                            .outline()
                            .icon(IconName::Plus)
                            .label("New request")
                            .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                                cx.emit(WelcomeEvent::NewRequest)
                            })),
                    ),
            )
            .child(
                // The palette is the fastest way to everything, and nobody finds
                // a keyboard shortcut that is never shown.
                h_flex()
                    .id("welcome-palette")
                    .gap_2()
                    .items_center()
                    .px_3()
                    .py_1p5()
                    .rounded(cx.theme().radius)
                    .hover(|hint| hint.bg(cx.theme().list_hover))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Search everything"),
                    )
                    .children(shortcut.map(Kbd::new))
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                        cx.emit(WelcomeEvent::CommandPalette)
                    })),
            )
            .child(div().h(px(24.)))
    }
}
