//! The ways a message can be shown.
//!
//! One implementation, used by both the request editor's response section and
//! by a dashboard pane. They are the same views of the same data — a plot in a
//! dashboard that drifted apart from the plot in an editor would be two bugs
//! waiting to disagree with each other.
//!
//! Free functions rather than a trait: each takes a value and gives back an
//! element, and there is no state worth putting behind an interface.

use gpui::{
    AnyElement, App, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, div, px,
};
use gpui_component::chart::LineChart;
use gpui_component::{ActiveTheme as _, IconName, h_flex, v_flex};
use rw_canonical::CanonicalValue;

use crate::scene_view::SceneView;
use crate::series::{History, Series};
use crate::{image, tokens, value};

/// Which view of a message is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    /// The message as text.
    #[default]
    Raw,
    /// A picture, a point cloud or a field table, whichever the message is.
    Visualize,
    /// Every number in the message, over time.
    Plot,
}

impl View {
    pub const ALL: [Self; 3] = [Self::Raw, Self::Visualize, Self::Plot];

    pub fn label(self) -> &'static str {
        match self {
            Self::Raw => "Raw",
            Self::Visualize => "Visualize",
            Self::Plot => "Plot",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|view| *view == self).unwrap_or(0)
    }

    /// The stored form, so a dashboard pane keeps its view across a restart.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Visualize => "visualize",
            Self::Plot => "plot",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "visualize" => Self::Visualize,
            "plot" => Self::Plot,
            _ => Self::Raw,
        }
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
/// An image is shown as an image and a point cloud in 3D; anything else becomes
/// a flat table of leaf paths and values, which beats indented JSON for the
/// thing people actually do here — finding one field in a large message.
///
/// `scene` is the 3D pane, when the host has one and it has points in it.
pub fn visualize(
    value: &CanonicalValue,
    scene: Option<&Entity<SceneView>>,
    cx: &App,
) -> AnyElement {
    if let Some(frame) = image::decode(value) {
        return picture(frame, cx);
    }

    // The scene draws its own controls over itself, so it needs nothing around
    // it but the space.
    if let Some(scene) = scene.filter(|scene| scene.read(cx).point_count() > 0) {
        return div()
            .size_full()
            .min_h_0()
            .child(scene.clone())
            .into_any_element();
    }

    fields(value, cx)
}

/// Every leaf of the message, path beside value.
pub fn fields(value: &CanonicalValue, cx: &App) -> AnyElement {
    let leaves = value::leaves(value);
    if leaves.is_empty() {
        return tokens::empty_state(
            IconName::Inbox,
            "Nothing to show",
            "This message has no fields.",
            cx,
        )
        .into_any_element();
    }

    v_flex()
        .id("fields")
        .size_full()
        .gap_0p5()
        .children(leaves.into_iter().map(|(path, shown)| {
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
                        .child(path),
                )
                .child(
                    tokens::mono(cx)
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(cx.theme().foreground)
                        .child(shown),
                )
        }))
        .into_any_element()
}

fn picture(frame: image::Frame, cx: &App) -> AnyElement {
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
        assert_eq!(labels, ["Raw", "Visualize", "Plot"]);
    }

    #[test]
    fn a_view_survives_a_round_trip_through_its_stored_form() {
        for view in View::ALL {
            assert_eq!(View::parse(view.as_str()), view);
        }
    }

    #[test]
    fn an_unknown_stored_view_falls_back_to_raw() {
        assert_eq!(View::parse("hologram"), View::Raw);
        assert_eq!(View::parse(""), View::Raw);
    }

    #[test]
    fn indices_match_the_order_the_tabs_are_drawn_in() {
        for (index, view) in View::ALL.iter().enumerate() {
            assert_eq!(view.index(), index);
        }
    }
}
