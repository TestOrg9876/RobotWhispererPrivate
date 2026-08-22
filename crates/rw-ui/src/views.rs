//! The ways a message can be shown.
//!
//! One implementation, used by both the request editor's response section and
//! by a dashboard pane. They are the same views of the same data — a plot in a
//! dashboard that drifted apart from the plot in an editor would be two bugs
//! waiting to disagree with each other.
//!
//! Free functions rather than a trait: each takes a value and gives back an
//! element, and there is no state worth putting behind an interface.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, div, px,
};
use gpui_component::chart::LineChart;
use gpui_component::{ActiveTheme as _, IconName, h_flex, v_flex};
use rw_canonical::{CanonicalValue, VisualizationRole};

use crate::scene_view::SceneView;
use crate::series::{History, Series};
use crate::viz::{self, Visual};
use crate::{diff, tokens, value};

/// Which view of a message is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    /// The message laid out the way the request form lays one out, foldable
    /// where it nests. What anyone wants to see first, so it is the default.
    #[default]
    Pretty,
    /// The message as text.
    Raw,
    /// A picture, a point cloud or the tree, whichever the message is.
    Visualize,
    /// Every number in the message, over time.
    Plot,
    /// What has changed since a message was pinned.
    Diff,
    /// What this request has done before.
    History,
}

impl View {
    pub const ALL: [Self; 6] = [
        Self::Pretty,
        Self::Raw,
        Self::Visualize,
        Self::Plot,
        Self::Diff,
        Self::History,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Pretty => "Pretty",
            Self::Raw => "Raw",
            Self::Visualize => "Visualize",
            Self::Plot => "Plot",
            Self::Diff => "Diff",
            Self::History => "History",
        }
    }

    /// Where this view sits in a strip drawing `offered`.
    pub fn index_in(self, offered: &[Self]) -> usize {
        offered.iter().position(|view| *view == self).unwrap_or(0)
    }

    /// The stored form, so a dashboard pane keeps its view across a restart.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pretty => "pretty",
            Self::Raw => "raw",
            Self::Visualize => "visualize",
            Self::Plot => "plot",
            Self::Diff => "diff",
            Self::History => "history",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "raw" => Self::Raw,
            "visualize" => Self::Visualize,
            "plot" => Self::Plot,
            "diff" => Self::Diff,
            "history" => Self::History,
            _ => Self::Pretty,
        }
    }

    /// The views this particular response can actually fill.
    ///
    /// Pretty and Raw always: every message can be laid out and every message
    /// can be printed. The other three carry something only sometimes, and a
    /// tab that opens on an apology is a tab that cost a place in the strip to
    /// say nothing.
    pub fn offered(offers: Offers) -> Vec<Self> {
        let mut views = vec![Self::Pretty, Self::Raw];
        if offers.visual {
            views.push(Self::Visualize);
        }
        if offers.plottable {
            views.push(Self::Plot);
        }
        if offers.pinned {
            views.push(Self::Diff);
        }
        if offers.recorded {
            views.push(Self::History);
        }
        views
    }

    /// Falls back to Pretty when this view is no longer on offer.
    ///
    /// A pane sitting on Plot when its topic changes to one with no numbers in
    /// it has to move, and Pretty is the view that is always there.
    pub fn or_pretty(self, offered: &[Self]) -> Self {
        if offered.contains(&self) {
            self
        } else {
            Self::Pretty
        }
    }
}

/// What a response has in it, for deciding which views to offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Offers {
    /// The role gives a picture or something with a place in the world. When it
    /// does not, "Visualize" draws the field tree — which is what "Pretty"
    /// already is, so offering both would be offering one view twice.
    pub visual: bool,
    /// The message has numbers a line chart can hold.
    pub plottable: bool,
    /// Something is pinned, so there is a before to diff against. Nothing is
    /// lost by hiding Diff until then: the Freeze button opens it.
    pub pinned: bool,
    /// This request has run before and the runs were kept. A topic never has
    /// any — a subscription has no discrete runs — so it never offers the tab.
    pub recorded: bool,
}

impl Offers {
    /// What a message of this role, with this history, offers.
    pub fn of(role: &VisualizationRole, history: &History, pinned: bool) -> Self {
        Self {
            visual: viz::visual_for(role) != Visual::Fields,
            plottable: !history.is_empty(),
            pinned,
            recorded: false,
        }
    }

    /// With past runs to look at, which only a request editor knows about.
    pub fn recorded(mut self, recorded: bool) -> Self {
        self.recorded = recorded;
        self
    }
}

/// The message as text.
pub fn raw(value: &CanonicalValue, cx: &App) -> AnyElement {
    tokens::mono(cx)
        .text_xs()
        .text_color(cx.theme().foreground)
        .child(value::preview(value))
        .into_any_element()
}

/// The message as something to look at.
///
/// Which of the three it gets is the registry's decision, taken from the
/// schema's role rather than by trying decoders until one works. Sniffing is
/// what made a `LaserScan` a wall of numbers: it is not an image and not a
/// `PointCloud2`, so it fell through to the table.
///
/// A view the message turns out not to be able to fill falls back to the field
/// table, which is a real answer — a `PointCloud2` whose every point was NaN
/// has nothing to draw, and its fields are what is left to look at.
///
/// `scene` is the 3D pane, when the host has one and there is anything in it,
/// and `fields` is what a message that turns out not to be a picture falls back
/// to — the same tree the Fields view draws, handed in rather than built here
/// so there is one way of showing a message's fields and not two.
pub fn visualize(
    role: &VisualizationRole,
    value: &CanonicalValue,
    scene: Option<&Entity<SceneView>>,
    fields: AnyElement,
    cx: &App,
) -> AnyElement {
    match viz::visual_for(role) {
        Visual::Picture => match viz::picture(value) {
            Some(frame) => picture(frame, cx),
            None => fields,
        },
        // The scene draws its own controls over itself, so it needs nothing
        // around it but the space.
        Visual::World => match scene.filter(|scene| !scene.read(cx).is_empty()) {
            Some(scene) => div()
                .size_full()
                .min_h_0()
                .child(scene.clone())
                .into_any_element(),
            None => fields,
        },
        Visual::Fields => fields,
    }
}

fn picture(frame: crate::image::Frame, cx: &App) -> AnyElement {
    v_flex()
        .size_full()
        .min_h_0()
        .gap_2()
        .items_center()
        .justify_center()
        .child(
            // Sized to the space rather than capped by it. `max_*` alone leaves
            // a small frame at its own pixel size — a 96×64 camera image
            // marooned in the middle of a pane — and the default `Contain` fit
            // keeps the aspect ratio while it grows.
            gpui::img(frame.source)
                .flex_1()
                .min_h_0()
                .w_full()
                .rounded(cx.theme().radius),
        )
        .child(
            tokens::mono(cx)
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(frame.caption),
        )
        .into_any_element()
}

/// What has changed since a message was pinned.
///
/// Only the fields that moved, with a delta on every number. Unchanged fields
/// are not rows — a diff that showed everything would be the raw view again,
/// and finding the one field that moved is the whole reason to freeze.
pub fn changes(pinned: Option<&CanonicalValue>, live: &CanonicalValue, cx: &App) -> AnyElement {
    let Some(pinned) = pinned else {
        return tokens::empty_state(
            IconName::Inbox,
            "Nothing pinned",
            "Freeze the current message, and this shows what changes after it.",
            cx,
        )
        .into_any_element();
    };

    let changes = diff::diff(pinned, live);
    if changes.is_empty() {
        return tokens::empty_state(
            IconName::CircleCheck,
            "Nothing has changed",
            "Every field still reads the way it did when this was pinned.",
            cx,
        )
        .into_any_element();
    }

    v_flex()
        .id("diff")
        .size_full()
        .gap_0p5()
        .children(changes.into_iter().map(|change| change_row(change, cx)))
        .into_any_element()
}

fn change_row(change: diff::Change, cx: &App) -> AnyElement {
    let tint = match change.kind() {
        diff::Kind::Added => cx.theme().success,
        diff::Kind::Removed => cx.theme().danger,
        diff::Kind::Changed => cx.theme().foreground,
    };
    // The arrow is what makes a row read as a change rather than as two values
    // that happen to be next to each other.
    let reading = match (&change.before, &change.after) {
        (Some(before), Some(after)) => format!("{before}  →  {after}"),
        (None, Some(after)) => format!("+  {after}"),
        (Some(before), None) => format!("−  {before}"),
        (None, None) => String::new(),
    };

    h_flex()
        .w_full()
        .py_0p5()
        .gap_4()
        .items_baseline()
        .child(
            tokens::mono(cx)
                .w(px(240.))
                .flex_shrink_0()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .truncate()
                .child(change.path.clone()),
        )
        .child(
            tokens::mono(cx)
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(tint)
                .child(reading),
        )
        // The delta last and right-aligned, so a column of them can be read
        // down without reading any of the values.
        .when_some(change.delta_label(), |row, delta| {
            row.child(
                tokens::mono(cx)
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(cx.theme().primary)
                    .child(delta),
            )
        })
        .into_any_element()
}

/// One line per numeric field, newest sample at the right.
///
/// The x axis is the sample index rather than a wall clock: messages arrive
/// when the robot sends them, and pretending otherwise would draw a smooth line
/// over a gap where nothing came.
pub fn plot(history: &History, cx: &App) -> AnyElement {
    if history.is_empty() {
        return tokens::empty_state(
            IconName::ChartPie,
            "Nothing to plot",
            "This message has no numbers in it, or none that fit on a line chart.",
            cx,
        )
        .into_any_element();
    }

    let palette = tokens::series_colors(cx);
    v_flex()
        .id("plot")
        .size_full()
        .gap_3()
        .children(
            history
                .iter()
                .enumerate()
                .map(|(index, (path, series))| series_row(index, path, series, &palette, cx)),
        )
        .into_any_element()
}

fn series_row(
    index: usize,
    path: &str,
    series: &Series,
    palette: &[gpui::Hsla],
    cx: &App,
) -> AnyElement {
    let stroke = palette[index % palette.len()];
    let caption = match (series.last(), series.range()) {
        (Some(last), Some((low, high))) => format!("{last:.4}  ·  {low:.4} to {high:.4}"),
        _ => String::new(),
    };

    // The x value is the sample's position in the window, as a string because
    // that is what the chart's point scale keys on. It has to be distinct per
    // sample — give them all the same label and every point lands on the same
    // x, which draws the series as a single vertical stroke. The axis itself is
    // off, so the numbers are never shown.
    let points: Vec<(SharedString, f64)> = series
        .samples
        .iter()
        .enumerate()
        .map(|(index, sample)| (SharedString::from(index.to_string()), *sample))
        .collect();

    // Shares the height with its siblings rather than taking a fixed slice: one
    // series alone should fill the pane, and several should divide it. The
    // minimum is what keeps a dozen of them readable, with the pane scrolling
    // past that point.
    v_flex()
        .flex_1()
        .min_h(px(120.))
        .gap_1()
        .child(
            h_flex()
                .flex_shrink_0()
                .gap_2()
                .items_baseline()
                .justify_between()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(tokens::status_dot(stroke))
                        .child(
                            tokens::mono(cx)
                                .text_xs()
                                .text_color(cx.theme().foreground)
                                .child(if path.is_empty() {
                                    SharedString::from("value")
                                } else {
                                    SharedString::from(path.to_string())
                                }),
                        ),
                )
                .child(
                    tokens::mono(cx)
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(caption),
                ),
        )
        .child(
            div().flex_1().min_h_0().w_full().child(
                LineChart::new(points)
                    .x(|(label, _): &(SharedString, f64)| label.clone())
                    .y(|(_, sample): &(SharedString, f64)| *sample)
                    .stroke(stroke)
                    .linear()
                    .x_axis(false),
            ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_labels_are_stable_and_distinct() {
        let labels = View::ALL.map(View::label);
        assert_eq!(
            labels,
            ["Pretty", "Raw", "Visualize", "Plot", "Diff", "History"]
        );
    }

    #[test]
    fn a_view_survives_a_round_trip_through_its_stored_form() {
        for view in View::ALL {
            assert_eq!(View::parse(view.as_str()), view);
        }
    }

    #[test]
    fn an_unknown_stored_view_falls_back_to_the_default() {
        assert_eq!(View::parse("hologram"), View::Pretty);
        assert_eq!(View::parse(""), View::Pretty);
    }

    #[test]
    fn indices_match_the_order_the_tabs_are_drawn_in() {
        for (index, view) in View::ALL.iter().enumerate() {
            assert_eq!(view.index_in(&View::ALL), index);
        }
    }

    /// The index is into the strip actually drawn, not into `ALL` — a pane
    /// offering three views and highlighting the fourth would light the wrong
    /// tab.
    #[test]
    fn an_index_is_into_the_strip_that_is_drawn() {
        let offered = View::offered(Offers::default());
        assert_eq!(offered, vec![View::Pretty, View::Raw]);
        assert_eq!(View::Raw.index_in(&offered), 1);
    }

    #[test]
    fn a_message_with_nothing_in_it_offers_only_the_two_that_always_work() {
        let offers = Offers::of(&VisualizationRole::Text, &History::default(), false);
        assert_eq!(View::offered(offers), vec![View::Pretty, View::Raw]);
    }

    #[test]
    fn a_cloud_offers_the_third_dimension_and_a_plain_string_does_not() {
        assert!(Offers::of(&VisualizationRole::PointCloud2, &History::default(), false).visual);
        assert!(Offers::of(&VisualizationRole::Image, &History::default(), false).visual);
        assert!(!Offers::of(&VisualizationRole::Text, &History::default(), false).visual);
        assert!(!Offers::of(&VisualizationRole::JsonTree, &History::default(), false).visual);
    }

    #[test]
    fn numbers_bring_the_plot_and_a_pin_brings_the_diff() {
        let mut history = History::default();
        history.observe(
            &CanonicalValue::Struct(
                [("data".to_string(), CanonicalValue::F64(1.))]
                    .into_iter()
                    .collect(),
            ),
            crate::series::Limits {
                window: 600,
                fields: 12,
            },
        );
        assert!(Offers::of(&VisualizationRole::Text, &history, false).plottable);

        let offers = Offers::of(&VisualizationRole::PointCloud2, &history, true);
        assert_eq!(
            View::offered(offers),
            vec![
                View::Pretty,
                View::Raw,
                View::Visualize,
                View::Plot,
                View::Diff
            ]
        );
    }

    /// A pane sitting on Plot when its topic changes to one with no numbers has
    /// to move, or it renders an apology where a view used to be.
    #[test]
    fn a_view_no_longer_offered_falls_back_to_pretty() {
        let offered = View::offered(Offers::default());
        assert_eq!(View::Plot.or_pretty(&offered), View::Pretty);
        assert_eq!(View::Diff.or_pretty(&offered), View::Pretty);
        assert_eq!(View::Raw.or_pretty(&offered), View::Raw);
    }
}
