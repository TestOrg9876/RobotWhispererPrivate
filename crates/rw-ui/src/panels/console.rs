//! Console: a running log of session and request events.

use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Window, div,
};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::{ActiveTheme as _, v_flex};

use crate::session::{Notice, RobotWhisperer, SessionEvent};

/// How many lines to keep. Old lines are dropped from the front so a long
/// session cannot grow without bound.
const CAPACITY: usize = 500;

struct Line {
    notice: Notice,
}

pub struct ConsolePanel {
    focus_handle: FocusHandle,
    lines: Vec<Line>,
}

impl EventEmitter<PanelEvent> for ConsolePanel {}

impl ConsolePanel {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sessions = RobotWhisperer::global(cx).sessions.clone();

        cx.subscribe(&sessions, |this, _, event: &SessionEvent, cx| {
            this.push(event.0.clone());
            cx.notify();
        })
        .detach();

        Self {
            focus_handle: cx.focus_handle(),
            lines: Vec::new(),
        }
    }

    pub fn view(window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self::new(window, cx))
    }

    fn push(&mut self, notice: Notice) {
        if self.lines.len() == CAPACITY {
            self.lines.remove(0);
        }
        self.lines.push(Line { notice });
    }
}

impl Focusable for ConsolePanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for ConsolePanel {
    fn panel_name(&self) -> &'static str {
        "Console"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        "Console"
    }

    fn closable(&self, _cx: &App) -> bool {
        false
    }
}

impl Render for ConsolePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let mono = cx.theme().mono_font_family.clone();

        let lines: Vec<_> = self
            .lines
            .iter()
            .map(|line| {
                let (text, color) = match &line.notice {
                    Notice::Info(text) => (text.clone(), muted),
                    Notice::Error(text) => (text.clone(), danger),
                };
                div().text_xs().text_color(color).child(text)
            })
            .collect();

        div()
            .id("console")
            .size_full()
            .overflow_y_scroll()
            .p_2()
            .font_family(mono)
            .child(if lines.is_empty() {
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("No events yet.")
                    .into_any_element()
            } else {
                v_flex().gap_0p5().children(lines).into_any_element()
            })
    }
}
