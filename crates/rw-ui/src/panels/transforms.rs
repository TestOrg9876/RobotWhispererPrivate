//! The transform tree, as `view_frames` would draw it and `tf_monitor` would
//! read it.
//!
//! Two of the most-run commands in robotics, and both of them make you leave
//! whatever you were looking at, wait for a PDF or a stream of text, and come
//! back. The data was already here: `Buffer::tree()` has returned frame,
//! parent, depth, static, sample count and newest stamp since it was written,
//! and its only caller picked out the root and threw the rest away.
//!
//! What a person actually asks of a transform tree is one of three things —
//! what frames exist, who is whose parent, and *is one of them stale* — so the
//! age of each edge is the column that earns its place. A frame that stopped
//! publishing looks exactly like one that never did until you can see how long
//! ago it last spoke.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div,
};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::{ActiveTheme as _, IconName, h_flex, v_flex};

use crate::session::{RobotWhisperer, Sessions};
use crate::tokens;

/// How long an edge can go unheard before it is called out, in nanoseconds.
///
/// Five seconds is `tf2`'s own default transform tolerance for a "not found"
/// and about where a person starts to wonder. A static edge is never stale:
/// `/tf_static` is published once and means it forever.
const STALE_NS: u64 = 5_000_000_000;

/// One row: a frame, and what is known about the edge above it.
struct Row {
    frame: SharedString,
    depth: usize,
    is_static: bool,
    samples: usize,
    /// How long since the newest sample on that edge. `None` for a root, which
    /// has no edge above it, and for a static one, which cannot go stale.
    age_ns: Option<u64>,
}

pub struct TransformsPanel {
    focus_handle: FocusHandle,
    sessions: Entity<Sessions>,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PanelEvent> for TransformsPanel {}

impl TransformsPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let sessions = RobotWhisperer::global(cx).sessions.clone();
        let tf = RobotWhisperer::global(cx).tf.clone();
        let subscriptions = vec![
            cx.observe(&sessions, |_, _, cx| cx.notify()),
            cx.observe(&tf, |_, _, cx| cx.notify()),
        ];
        Self {
            focus_handle: cx.focus_handle(),
            sessions,
            scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    pub fn view(cx: &mut App) -> Entity<Self> {
        cx.new(Self::new)
    }

    /// Every connected system that has a tree, and its rows.
    fn systems(&self, cx: &App) -> Vec<(SharedString, Vec<Row>)> {
        let mut systems = Vec::new();
        for (id, live) in self.sessions.read(cx).connections() {
            let Some(tree) = crate::tf::tree(Some(id), cx) else {
                continue;
            };
            let nodes = tree.nodes();
            if nodes.is_empty() {
                continue;
            }

            // Age is measured against the newest stamp in the whole tree rather
            // than against the wall clock. A robot's stamps come from its own
            // clock — a simulator counting from zero, a bag replayed from 2019
            // — and subtracting those from this machine's clock reports every
            // frame as decades stale.
            let now_ns = nodes.iter().filter_map(|node| node.newest_ns).max();

            let rows = nodes
                .into_iter()
                .map(|node| Row {
                    frame: SharedString::from(node.frame),
                    depth: node.depth,
                    is_static: node.is_static,
                    samples: node.samples,
                    age_ns: match (
                        node.parent.is_some() && !node.is_static,
                        now_ns,
                        node.newest_ns,
                    ) {
                        (true, Some(now), Some(newest)) => Some(now.saturating_sub(newest)),
                        _ => None,
                    },
                })
                .collect();
            systems.push((SharedString::from(live.name.clone()), rows));
        }
        systems
    }
}

/// An age as a person reads it: `0.3 s ago`, `12 s ago`, `4 min ago`.
///
/// "ago" rather than a column header saying "last heard". A bare `0.0` next to
/// a frame name could be a rate, a distance or a count, and one word is cheaper
/// than a header row that would then have to line up with everything under it.
pub fn ago(ns: u64) -> String {
    let seconds = ns as f64 / 1e9;
    if seconds < 10. {
        format!("{seconds:.1} s ago")
    } else if seconds < 90. {
        format!("{seconds:.0} s ago")
    } else {
        format!("{:.0} min ago", seconds / 60.)
    }
}

impl Render for TransformsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let systems = self.systems(cx);
        if systems.is_empty() {
            return v_flex()
                .size_full()
                .bg(cx.theme().background)
                .child(tokens::empty_state(
                    IconName::Frame,
                    "No transforms yet",
                    "Connect a system that publishes /tf and its frames appear here.",
                    cx,
                ))
                .into_any_element();
        }

        let several = systems.len() > 1;
        let mut body = v_flex().w_full().gap_3();
        for (name, rows) in systems {
            let mut section = v_flex().w_full().gap_0p5();
            // Which system, but only when there is more than one: with a single
            // robot connected its name is already in the status bar, and saying
            // it twice is a heading that carries nothing.
            if several {
                section = section.child(tokens::section_label(name, cx));
            }
            for row in rows {
                let stale = row.age_ns.is_some_and(|age| age > STALE_NS);
                section = section.child(
                    h_flex()
                        // Capped rather than full width: right-aligning the age
                        // against a 1400px pane puts it a hand's width from the
                        // frame it belongs to, and a column you have to track
                        // across the screen is a column you misread.
                        .w_full()
                        .max_w(tokens::designed(520.))
                        .py_0p5()
                        .gap_2()
                        .items_baseline()
                        // Indentation is the tree. A line-drawing gutter would
                        // be prettier and would cost a column on every row to
                        // say what the offset already says.
                        .pl(tokens::scaled(tokens::designed(16.), row.depth as f32))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .font_family("monospace")
                                .text_color(if stale {
                                    cx.theme().danger
                                } else {
                                    cx.theme().foreground
                                })
                                .child(row.frame),
                        )
                        .when(row.is_static, |line| {
                            // Said once, on the frames it applies to. A column
                            // that reads "dynamic" on every other row would be
                            // a column of the word "dynamic".
                            line.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("static"),
                            )
                        })
                        .when(row.samples > 0 && !row.is_static, |line| {
                            line.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{} held", row.samples)),
                            )
                        })
                        .when_some(row.age_ns, |line, age| {
                            line.child(
                                div()
                                    .min_w(tokens::designed(76.))
                                    .text_xs()
                                    .text_right()
                                    .font_family("monospace")
                                    .text_color(if stale {
                                        cx.theme().danger
                                    } else {
                                        cx.theme().muted_foreground
                                    })
                                    .child(ago(age)),
                            )
                        }),
                );
            }
            body = body.child(section);
        }

        // Filling the pane plainly rather than floating a card inside it: this
        // is a dock panel, and the pane the dock gives it is already the card.
        // The console beside it does the same.
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(
                div()
                    .id("transforms")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .px_3()
                    .py_2()
                    .child(body),
            )
            .into_any_element()
    }
}

impl Focusable for TransformsPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for TransformsPanel {
    fn panel_name(&self) -> &'static str {
        "Transforms"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        "Transforms"
    }

    fn closable(&self, _cx: &App) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_age_reads_the_way_a_person_says_it() {
        assert_eq!(ago(0), "0.0 s ago");
        assert_eq!(ago(300_000_000), "0.3 s ago");
        assert_eq!(ago(9_400_000_000), "9.4 s ago");
        assert_eq!(ago(12_000_000_000), "12 s ago");
        assert_eq!(ago(240_000_000_000), "4 min ago");
    }

    #[test]
    fn five_seconds_is_where_an_edge_is_called_stale() {
        // tf2's own default transform tolerance, and about where a person
        // starts to wonder whether the publisher died.
        assert_eq!(STALE_NS, 5_000_000_000);
    }
}
