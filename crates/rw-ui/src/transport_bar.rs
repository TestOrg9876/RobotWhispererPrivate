//! The bar you get when a connection is a recording rather than a robot.
//!
//! `ReplayTransport` has had `set_playing`, `set_speed`, `set_looping`, `seek`
//! and a progress channel since record and replay landed, all tested, and
//! nothing in the UI called any of them: opening a bag started it playing and
//! that was the whole of the control anyone had. A bag player without a pause
//! button is not a bag player.
//!
//! One row per open recording — usually exactly one, and two bags open at once
//! genuinely do need two bars. The row is chrome rather than a dock panel: it
//! belongs to the window the way the status bar does, not to a pane someone
//! might close.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div,
};
use gpui_component::menu::DropdownMenu as _;
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::{ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, h_flex};
use gpui_component::{button::Button, button::ButtonVariants as _};
use rw_transport::{ReplayCommand, ReplayProgress};

use crate::actions::SetReplaySpeed;
use crate::session::{RobotWhisperer, Sessions};
use crate::tokens;

/// The speeds offered. Powers of two around 1, which is what every other
/// player offers, because they are the ones you can reason about: "half" and
/// "double" mean something, "1.35×" does not.
pub const SPEEDS: [u32; 6] = [25, 50, 100, 200, 400, 800];

/// One recording's controls.
struct Row {
    connection: i64,
    scrubber: Entity<SliderState>,
    /// True between the first drag event and the release.
    ///
    /// While it is set, arriving progress does not move the thumb: a scrubber
    /// that fights the hand holding it is worse than no scrubber.
    dragging: bool,
    _subscription: Subscription,
}

pub struct TransportBar {
    sessions: Entity<Sessions>,
    rows: Vec<Row>,
    _subscriptions: Vec<Subscription>,
}

impl TransportBar {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let sessions = RobotWhisperer::global(cx).sessions.clone();
        let subscriptions = vec![cx.observe(&sessions, |this, _, cx| {
            this.settle(cx);
            cx.notify();
        })];

        let mut bar = Self {
            sessions,
            rows: Vec::new(),
            _subscriptions: subscriptions,
        };
        bar.settle(cx);
        bar
    }

    pub fn view(cx: &mut App) -> Entity<Self> {
        cx.new(Self::new)
    }

    /// Whether there is anything to show. The bar takes no height otherwise.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Builds a row for each recording, and drops the rows of the ones closed.
    fn settle(&mut self, cx: &mut Context<Self>) {
        let open: Vec<(i64, ReplayProgress)> = self
            .sessions
            .read(cx)
            .connections()
            .filter_map(|(id, live)| live.replay.map(|progress| (id, progress)))
            .collect();

        self.rows
            .retain(|row| open.iter().any(|(id, _)| *id == row.connection));

        for (connection, progress) in open {
            if self.rows.iter().any(|row| row.connection == connection) {
                continue;
            }

            let scrubber = cx.new(|_| {
                SliderState::new()
                    .min(0.)
                    .max(1.)
                    .step(0.001)
                    .default_value(progress.fraction())
            });
            let subscription = cx.subscribe(&scrubber, move |this, _, event: &SliderEvent, cx| {
                let Some(row) = this
                    .rows
                    .iter_mut()
                    .find(|row| row.connection == connection)
                else {
                    return;
                };
                match event {
                    SliderEvent::Change(_) => row.dragging = true,
                    SliderEvent::Release(value) => {
                        row.dragging = false;
                        // Seeking on release rather than on every drag event:
                        // each seek publishes a new position, which would fight
                        // the thumb all the way across.
                        this.send(connection, ReplayCommand::Seek(value.start()), cx);
                    }
                }
            });

            self.rows.push(Row {
                connection,
                scrubber,
                dragging: false,
                _subscription: subscription,
            });
        }

        self.rows.sort_by_key(|row| row.connection);
    }

    /// Moves each thumb to where playback has actually reached.
    ///
    /// Done at render rather than when progress arrives because `set_value`
    /// wants a window, and render is where one is at hand. A row being dragged
    /// is skipped: a scrubber that fights the hand holding it is worse than no
    /// scrubber.
    fn follow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let positions: Vec<(usize, f32)> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| !row.dragging)
            .filter_map(|(index, row)| {
                let progress = self.progress(row.connection, cx)?;
                Some((index, progress.fraction()))
            })
            .collect();

        for (index, fraction) in positions {
            let scrubber = self.rows[index].scrubber.clone();
            if scrubber.read(cx).value().start() == fraction {
                continue;
            }
            scrubber.update(cx, |state, cx| state.set_value(fraction, window, cx));
        }
    }

    fn send(&self, connection: i64, command: ReplayCommand, cx: &mut Context<Self>) {
        self.sessions.update(cx, |sessions, cx| {
            if let Some(task) = sessions.replay_control(connection, command, cx) {
                task.detach();
            }
        });
    }

    /// Applies a speed chosen from the menu.
    pub fn set_speed(&mut self, connection: i64, hundredths: u32, cx: &mut Context<Self>) {
        self.send(
            connection,
            ReplayCommand::Speed(hundredths as f32 / 100.),
            cx,
        );
    }

    fn progress(&self, connection: i64, cx: &App) -> Option<ReplayProgress> {
        self.sessions.read(cx).live(connection)?.replay
    }

    fn name(&self, connection: i64, cx: &App) -> SharedString {
        self.sessions
            .read(cx)
            .live(connection)
            .map(|live| SharedString::from(live.name.clone()))
            .unwrap_or_default()
    }
}

/// A duration as `m:ss`, or `h:mm:ss` once it needs the hours.
///
/// Recording length, not wall-clock time: a bag is a stopwatch, and showing
/// `0:00:07` for seven seconds reads like a clock that has stopped.
pub fn clock(ns: u64) -> String {
    let total = ns / 1_000_000_000;
    let (hours, minutes, seconds) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// What the recording will do when it reaches the end.
pub fn loop_label(looping: bool) -> &'static str {
    if looping { "Looping" } else { "Play once" }
}

/// A speed as it is written on the button: `1×`, `0.5×`, `8×`.
pub fn speed_label(hundredths: u32) -> String {
    if hundredths.is_multiple_of(100) {
        format!("{}×", hundredths / 100)
    } else {
        format!("{}×", f32::from(hundredths as u16) / 100.)
    }
}

impl Render for TransportBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.follow(window, cx);

        let several = self.rows.len() > 1;
        let rows: Vec<_> = self
            .rows
            .iter()
            .filter_map(|row| {
                let connection = row.connection;
                let progress = self.progress(connection, cx)?;
                let hundredths = (progress.speed * 100.).round() as u32;

                Some(
                    h_flex()
                        .id(("replay", connection as usize))
                        .w_full()
                        .h(tokens::designed(34.))
                        .px_3()
                        .gap_3()
                        .items_center()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().secondary)
                        .child(
                            Button::new(("play", connection as usize))
                                .ghost()
                                .xsmall()
                                .icon(if progress.playing {
                                    IconName::Pause
                                } else {
                                    IconName::Play
                                })
                                .tooltip(if progress.playing { "Pause" } else { "Play" })
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    let playing = this
                                        .progress(connection, cx)
                                        .is_some_and(|progress| progress.playing);
                                    this.send(connection, ReplayCommand::Playing(!playing), cx);
                                })),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Slider::new(&row.scrubber).horizontal()),
                        )
                        // The controls right of the scrubber are given fixed
                        // widths so the row does not reflow. Both the clock and
                        // the loop label change width as they change state, and
                        // a button that slides out from under the cursor as you
                        // toggle it is a button you misclick.
                        .child(
                            div()
                                .min_w(tokens::designed(86.))
                                .text_xs()
                                .text_right()
                                .font_family("monospace")
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(
                                    "{} / {}",
                                    clock(progress.at_ns),
                                    clock(progress.duration_ns)
                                )),
                        )
                        .child(
                            div().w(tokens::designed(46.)).child(
                                Button::new(("speed", connection as usize))
                                    .ghost()
                                    .xsmall()
                                    .label(speed_label(hundredths))
                                    .tooltip("Playback speed")
                                    .dropdown_menu(move |mut menu, _window, _cx| {
                                        for option in SPEEDS {
                                            menu = menu.menu_with_check(
                                                speed_label(option),
                                                option == hundredths,
                                                Box::new(SetReplaySpeed {
                                                    connection,
                                                    hundredths: option,
                                                }),
                                            );
                                        }
                                        menu
                                    }),
                            ),
                        )
                        .child(
                            div().w(tokens::designed(78.)).child(
                                Button::new(("loop", connection as usize))
                                    .ghost()
                                    .xsmall()
                                    .selected(progress.looping)
                                    // The label carries the state, the way
                                    // "Freeze" becomes "Frozen at #12" on the
                                    // response strip. A ghost button's selected
                                    // tint is a few percent of grey, which is not
                                    // enough for a control whose whole content is
                                    // whether it is on.
                                    //
                                    // A word rather than an icon, too: every
                                    // symbol for "loop" is also the symbol for
                                    // "retry", and next to a play button those
                                    // mean opposite things.
                                    .label(loop_label(progress.looping))
                                    .tooltip(if progress.looping {
                                        "Stop at the end instead"
                                    } else {
                                        "Start again at the end instead"
                                    })
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        let looping = this
                                            .progress(connection, cx)
                                            .is_some_and(|progress| progress.looping);
                                        this.send(connection, ReplayCommand::Looping(!looping), cx);
                                    })),
                            ),
                        )
                        // Which recording, but only when there is more than
                        // one. With a single bar the status bar beside it
                        // already says the name, and saying it twice is noise.
                        .when(several, |row| {
                            row.child(
                                h_flex()
                                    .gap_1p5()
                                    .items_center()
                                    .max_w(tokens::designed(160.))
                                    .child(
                                        Icon::new(IconName::Inbox)
                                            .xsmall()
                                            .text_color(cx.theme().muted_foreground),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .truncate()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(self.name(connection, cx)),
                                    ),
                            )
                        }),
                )
            })
            .collect();

        div().children(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clock_reads_as_a_stopwatch_until_it_needs_the_hours() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(7_000_000_000), "0:07");
        assert_eq!(clock(95_000_000_000), "1:35");
        assert_eq!(clock(3_600_000_000_000), "1:00:00");
        assert_eq!(clock(3_725_000_000_000), "1:02:05");
    }

    #[test]
    fn a_whole_speed_has_no_decimal_point() {
        assert_eq!(speed_label(100), "1×");
        assert_eq!(speed_label(800), "8×");
        assert_eq!(speed_label(50), "0.5×");
        assert_eq!(speed_label(25), "0.25×");
    }

    #[test]
    fn the_speeds_offered_are_the_ones_you_can_reason_about() {
        // Halves and doubles around the captured rate, and 1 among them: a
        // player you cannot get back to normal speed in is a trap.
        assert!(SPEEDS.contains(&100));
        for pair in SPEEDS.windows(2) {
            assert_eq!(pair[1], pair[0] * 2, "{pair:?} is not a doubling");
        }
    }

    #[test]
    fn the_loop_button_says_what_will_happen_at_the_end() {
        // Not "Loop" in both states: a ghost button's selected tint is a few
        // percent of grey, and the state is the whole content of this control.
        assert_ne!(loop_label(true), loop_label(false));
        assert_eq!(loop_label(true), "Looping");
        assert_eq!(loop_label(false), "Play once");
    }

    #[test]
    fn progress_is_a_fraction_even_for_an_empty_recording() {
        let empty = ReplayProgress {
            at_ns: 0,
            duration_ns: 0,
            playing: false,
            speed: 1.,
            looping: false,
        };
        assert_eq!(empty.fraction(), 0.);

        let half = ReplayProgress {
            at_ns: 50,
            duration_ns: 100,
            ..empty
        };
        assert_eq!(half.fraction(), 0.5);

        let past_the_end = ReplayProgress {
            at_ns: 300,
            duration_ns: 100,
            ..empty
        };
        assert_eq!(past_the_end.fraction(), 1., "clamped rather than overshot");
    }
}
