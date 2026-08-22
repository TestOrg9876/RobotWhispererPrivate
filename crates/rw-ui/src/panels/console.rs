//! Console: a running log of session and request events.
//!
//! The place you look when something did not happen and you want to know how
//! far it got — a connection that dropped, a subscription that failed. Which is
//! why it timestamps, filters and follows the tail.

use std::collections::VecDeque;

use chrono::{DateTime, Local};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::{
    ActiveTheme as _, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};

use crate::prefs::Settings;
use crate::session::{Notice, RobotWhisperer, SessionEvent, Severity};
use crate::tokens;

struct Line {
    at: DateTime<Local>,
    notice: Notice,
}

impl Line {
    fn text(&self) -> String {
        self.notice.text()
    }

    fn severity(&self) -> Severity {
        self.notice.severity()
    }
}

pub struct ConsolePanel {
    focus_handle: FocusHandle,
    /// A ring: every line past the kept depth drops one off the front, and
    /// `Vec::remove(0)` shifts the whole buffer to do it. Two thousand lines of
    /// `/rosout` at 100 Hz is not a workload worth memmoving through.
    lines: VecDeque<Line>,
    filter: Entity<InputState>,
    /// The quietest severity shown.
    ///
    /// One control that cycles rather than three that are mutually exclusive:
    /// a segmented bar for a three-way choice nobody makes twice a session is
    /// a row of chrome, and the button's own label already says where it is.
    floor: Severity,
    scroll: ScrollHandle,
    /// Whether to keep the newest line in view.
    ///
    /// Following is what you want while watching something happen and exactly
    /// what you do not want while reading back through what already did, so it
    /// switches off the moment the log is scrolled away from the bottom.
    follow: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PanelEvent> for ConsolePanel {}

impl ConsolePanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sessions = RobotWhisperer::global(cx).sessions.clone();
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter"));

        let subscriptions = vec![
            cx.subscribe(&sessions, |this, _, event: &SessionEvent, cx| {
                this.push(event.0.clone(), cx);
                cx.notify();
            }),
            cx.subscribe(&filter, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];

        Self {
            focus_handle: cx.focus_handle(),
            lines: VecDeque::new(),
            filter,
            floor: Severity::Info,
            scroll: ScrollHandle::new(),
            follow: true,
            _subscriptions: subscriptions,
        }
    }

    pub fn view(window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self::new(window, cx))
    }

    /// Adds a line, dropping the oldest once the kept depth is reached.
    ///
    /// Old lines go from the front, so a session left running for a week costs
    /// the same as one left running for a minute.
    fn push(&mut self, notice: Notice, cx: &App) {
        let cap = Settings::get(cx).console_lines.max(1);
        while self.lines.len() >= cap {
            self.lines.pop_front();
        }
        self.lines.push_back(Line {
            at: Local::now(),
            notice,
        });
    }

    /// The lines currently worth showing, oldest first.
    fn visible(&self, cx: &App) -> Vec<&Line> {
        let needle = self.filter.read(cx).value().trim().to_lowercase();
        self.lines
            .iter()
            .filter(|line| line.severity() >= self.floor)
            .filter(|line| needle.is_empty() || line.text().to_lowercase().contains(&needle))
            .collect()
    }

    fn errors(&self) -> usize {
        self.lines
            .iter()
            .filter(|line| line.severity() == Severity::Error)
            .count()
    }

    /// What the level button says, and where it goes next.
    fn floor_label(&self) -> &'static str {
        match self.floor {
            Severity::Info => "All",
            Severity::Warn => "Warnings",
            Severity::Error => "Errors",
        }
    }

    fn next_floor(&self) -> Severity {
        match self.floor {
            Severity::Info => Severity::Warn,
            Severity::Warn => Severity::Error,
            Severity::Error => Severity::Info,
        }
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

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let errors = self.errors();
        h_flex()
            .gap_1p5()
            .items_baseline()
            .child("Console")
            // Errors are the reason anybody opens this, so the count is on the
            // tab where it is visible with the panel collapsed.
            .when(errors > 0, |title| {
                title.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(errors.to_string()),
                )
            })
    }

    fn closable(&self, _cx: &App) -> bool {
        false
    }
}

impl Render for ConsolePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let visible = self.visible(cx);
        let total = self.lines.len();
        let shown = visible.len();
        let floor = self.floor;
        let floor_label = self.floor_label();
        let next_floor = self.next_floor();

        let rows: Vec<_> = visible
            .iter()
            .map(|line| {
                let colour = match line.severity() {
                    Severity::Error => cx.theme().danger,
                    Severity::Warn => cx.theme().warning,
                    Severity::Info => cx.theme().muted_foreground,
                };
                h_flex()
                    .w_full()
                    .gap_3()
                    .items_baseline()
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .opacity(0.6)
                            .child(SharedString::from(
                                line.at.format("%H:%M:%S%.3f").to_string(),
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(colour)
                            .child(SharedString::from(line.text())),
                    )
            })
            .collect();

        if self.follow {
            self.scroll.scroll_to_bottom();
        }

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .flex_shrink_0()
                    .px_2()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .child(div().w(px(200.)).child(Input::new(&self.filter).xsmall()))
                    .child(
                        Button::new("level")
                            .ghost()
                            .xsmall()
                            .label(floor_label)
                            .tooltip("The quietest level shown")
                            .selected(floor != Severity::Info)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.floor = next_floor;
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .child(tokens::meta(
                        "Lines",
                        if shown == total {
                            total.to_string()
                        } else {
                            format!("{shown} of {total}")
                        },
                        cx,
                    ))
                    .child(
                        Button::new("follow")
                            .ghost()
                            .xsmall()
                            .icon(IconName::ArrowDown)
                            .tooltip("Follow the newest line")
                            .selected(self.follow)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.follow = !this.follow;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("clear")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Delete)
                            .tooltip("Clear")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.lines.clear();
                                cx.notify();
                            })),
                    ),
            )
            .child(
                v_flex()
                    .id("console")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .px_2()
                    .pb_2()
                    .gap_0p5()
                    .font_family(cx.theme().mono_font_family.clone())
                    .children(rows)
                    .when(shown == 0, |list| {
                        list.child(
                            div()
                                .p_2()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(if total == 0 {
                                    "No events yet."
                                } else {
                                    "Nothing matches that filter."
                                }),
                        )
                    }),
            )
    }
}
