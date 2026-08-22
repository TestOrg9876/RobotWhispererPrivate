//! Console: a running log of session and request events.
//!
//! The place you look when something did not happen and you want to know how
//! far it got — a connection that dropped, a subscription that failed. Which is
//! why it timestamps, filters and follows the tail.

use std::collections::VecDeque;

use chrono::Local;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Rems, Render, ScrollStrategy,
    SharedString, Styled as _, Subscription, UniformListScrollHandle, Window, div, uniform_list,
};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::menu::DropdownMenu as _;
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

/// One line, with everything it needs to be drawn worked out on arrival.
///
/// The console re-renders on every notice, so anything computed in `render` is
/// computed for the whole buffer at the rate the robot talks. Formatting two
/// thousand timestamps and lowercasing two thousand messages per frame is the
/// difference between a log pane and a stutter.
/// Show every line at this severity or louder. Carries the discriminant so one
/// action serves the whole menu, the way the request kind menu does.
#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct SetConsoleFloor(pub u8);

fn floor_from_discriminant(value: u8) -> Severity {
    match value {
        1 => Severity::Warn,
        2 => Severity::Error,
        _ => Severity::Info,
    }
}

fn discriminant_of(floor: Severity) -> u8 {
    match floor {
        Severity::Info => 0,
        Severity::Warn => 1,
        Severity::Error => 2,
    }
}

struct Line {
    at: SharedString,
    text: SharedString,
    /// The message lowercased, kept beside it so a keystroke in the filter
    /// scans the buffer instead of allocating a copy of it.
    lower: String,
    severity: Severity,
}

/// One row of the log. Fixed, because the list only virtualises when every row
/// is the same height — which is also why a long line truncates rather than
/// wrapping.
const ROW_HEIGHT: Rems = tokens::designed(20.);

pub struct ConsolePanel {
    focus_handle: FocusHandle,
    /// A ring: every line past the kept depth drops one off the front, and
    /// `Vec::remove(0)` shifts the whole buffer to do it. Two thousand lines of
    /// `/rosout` at 100 Hz is not a workload worth memmoving through.
    lines: VecDeque<Line>,
    filter: Entity<InputState>,
    /// The quietest severity shown.
    ///
    /// A menu rather than a segmented bar, for the reason the original comment
    /// here gave: three mutually exclusive buttons for a choice nobody makes
    /// twice a session is a row of chrome. It used to *cycle* instead, which
    /// spends the same pixel and hides both the options and the way back.
    floor: Severity,
    scroll: UniformListScrollHandle,
    /// Whether to keep the newest line in view.
    ///
    /// Following is what you want while watching something happen and exactly
    /// what you do not want while reading back through what already did, so it
    /// is a button rather than something the pane decides.
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
            scroll: UniformListScrollHandle::new(),
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
        let text = notice.text();
        self.lines.push_back(Line {
            at: SharedString::from(Local::now().format("%H:%M:%S%.3f").to_string()),
            lower: text.to_lowercase(),
            text: SharedString::from(text),
            severity: notice.severity(),
        });
    }

    /// The lines currently worth showing, oldest first.
    fn visible(&self, cx: &App) -> Vec<&Line> {
        let needle = self.filter.read(cx).value().trim().to_lowercase();
        self.lines
            .iter()
            .filter(|line| line.severity >= self.floor)
            .filter(|line| needle.is_empty() || line.lower.contains(&needle))
            .collect()
    }

    fn errors(&self) -> usize {
        self.lines
            .iter()
            .filter(|line| line.severity == Severity::Error)
            .count()
    }

    /// What the level menu says, for the button and for each of its entries.
    fn floor_label(floor: Severity) -> &'static str {
        match floor {
            Severity::Info => "All",
            Severity::Warn => "Warnings",
            Severity::Error => "Errors",
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
        let floor_label = Self::floor_label(floor);

        // Only what a row needs to draw itself, so the list can build the ten
        // rows on screen and leave the other two thousand alone.
        let rows: Vec<(SharedString, SharedString, Hsla)> = visible
            .iter()
            .map(|line| {
                let colour = match line.severity {
                    Severity::Error => cx.theme().danger,
                    Severity::Warn => cx.theme().warning,
                    Severity::Info => cx.theme().muted_foreground,
                };
                (line.at.clone(), line.text.clone(), colour)
            })
            .collect();
        let timestamp = cx.theme().muted_foreground;

        if self.follow && shown > 0 {
            self.scroll
                .scroll_to_item(shown - 1, ScrollStrategy::Bottom);
        }

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .on_action(cx.listener(|this, action: &SetConsoleFloor, _, cx| {
                this.floor = floor_from_discriminant(action.0);
                cx.notify();
            }))
            .child(
                h_flex()
                    .flex_shrink_0()
                    .px_2()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .w(tokens::designed(200.))
                            .child(Input::new(&self.filter).xsmall()),
                    )
                    .child(
                        Button::new("level")
                            .ghost()
                            .xsmall()
                            .label(floor_label)
                            // The button's own caret, which sits after the
                            // label. `icon` puts it in front, where it reads as
                            // part of the value rather than as "there is more".
                            .dropdown_caret(true)
                            .tooltip("The quietest level shown")
                            .selected(floor != Severity::Info)
                            .dropdown_menu(move |mut menu, _window, _cx| {
                                for option in [Severity::Info, Severity::Warn, Severity::Error] {
                                    menu = menu.menu_with_check(
                                        Self::floor_label(option),
                                        option == floor,
                                        Box::new(SetConsoleFloor(discriminant_of(option))),
                                    );
                                }
                                menu
                            }),
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
                    .flex_1()
                    .min_h_0()
                    .px_2()
                    .pb_2()
                    .font_family(cx.theme().mono_font_family.clone())
                    .when(shown > 0, |pane| {
                        pane.child(
                            uniform_list("console", rows.len(), move |range, _window, _cx| {
                                range
                                    .clone()
                                    .map(|index| {
                                        let (at, text, colour) = rows[index].clone();
                                        h_flex()
                                            .h(ROW_HEIGHT)
                                            .w_full()
                                            .gap_3()
                                            .items_center()
                                            .child(
                                                div()
                                                    .flex_shrink_0()
                                                    .text_xs()
                                                    .text_color(timestamp)
                                                    .child(at),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .text_xs()
                                                    .truncate()
                                                    .text_color(colour)
                                                    .child(text),
                                            )
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .track_scroll(&self.scroll)
                            .size_full(),
                        )
                    })
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
