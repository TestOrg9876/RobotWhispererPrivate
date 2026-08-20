//! Shared presentation helpers.
//!
//! The theme JSON owns colour, radius and base font size. What it cannot express
//! is *composition* — the recurring shapes this app uses. Keeping them here means
//! the section label in the sidebar and the one in the response pane are the same
//! thing rather than two similar-looking accidents.

use gpui::prelude::FluentBuilder as _;
use gpui::{Div, Hsla, ParentElement as _, SharedString, Styled as _, div, px};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex, tag::Tag, v_flex,
};
use rw_core::domain::RequestKind;

/// Height of a standard control, and of the request bar. Comfortable density.
pub const CONTROL_HEIGHT: f32 = 36.0;
pub const REQUEST_BAR_HEIGHT: f32 = 44.0;
/// Height of the strip along a card's top edge.
pub const CARD_HEADER_HEIGHT: f32 = 38.0;

/// An elevated card: the surface that carries one titled section of content.
///
/// `GroupBox` is deliberately not used for these. Its default `Normal` variant
/// paints no background, no border and no padding — which is what made the first
/// pass read as flat — and neither `Fill` nor `Outline` can host a header strip
/// flush with the card's top edge. The response section needs exactly that, so
/// its view switcher and message counters read as part of the card rather than
/// floating above it.
pub fn card(cx: &gpui::App) -> Div {
    v_flex()
        // Clipped so the header strip's corners follow the card's radius.
        .overflow_hidden()
        .bg(cx.theme().group_box)
        .border_1()
        .border_color(cx.theme().border)
        .rounded(cx.theme().radius_lg)
        .when(cx.theme().shadow, |card| card.shadow_sm())
}

/// The strip along a card's top edge, holding its title and controls.
///
/// One step further up the elevation ramp than the card body, which is what
/// separates chrome from content without spending a second border on it.
pub fn card_header(cx: &gpui::App) -> Div {
    h_flex()
        .h(px(CARD_HEADER_HEIGHT))
        .flex_shrink_0()
        .w_full()
        .items_center()
        .justify_between()
        .gap_3()
        .px_3()
        .bg(cx.theme().muted)
        .border_b_1()
        .border_color(cx.theme().border)
}

/// A card's content area: scrolls internally so the card keeps its bounds.
pub fn card_body() -> Div {
    v_flex().flex_1().min_h_0().p_3().gap_3()
}

/// A small uppercase section label — the quiet heading used above lists and
/// inside cards.
pub fn section_label(text: impl Into<SharedString>, cx: &gpui::App) -> Div {
    div()
        .text_xs()
        .font_medium()
        .text_color(cx.theme().muted_foreground)
        .child(text.into().to_uppercase())
}

/// The coloured pill identifying a request's kind, replacing the old `TypeBadge`.
///
/// A tag rather than an icon: `Topic`/`Service`/`Action` are words worth reading,
/// and three similar glyphs are not distinguishable at a glance.
pub fn kind_tag(kind: RequestKind, cx: &gpui::App) -> Tag {
    // A tinted fill with the full-strength colour as text and border: legible on
    // both the light and dark palettes without a second set of values.
    let colour = kind_color(kind, cx);
    Tag::custom(colour.opacity(0.14), colour, colour.opacity(0.32))
        .rounded_full()
        .small()
}

/// Short label for a request kind, as shown in the tag and the kind selector.
pub fn kind_label(kind: RequestKind) -> &'static str {
    match kind {
        RequestKind::Topic => "TOPIC",
        RequestKind::Service => "SERVICE",
        RequestKind::Action => "ACTION",
        RequestKind::Param => "PARAM",
    }
}

/// The short label that names a request's kind in a list.
///
/// Four characters at most, in a monospaced face and a fixed-width column, so
/// every name after it starts at the same x — a ragged left edge is what makes a
/// long list tiring to scan. The full word is [`kind_label`], for the selector
/// where there is room for it.
pub fn kind_short(kind: RequestKind) -> &'static str {
    match kind {
        RequestKind::Topic => "TOP",
        RequestKind::Service => "SERV",
        RequestKind::Action => "ACT",
        RequestKind::Param => "PARM",
    }
}

/// How wide the kind column is: four monospaced characters and a little air.
pub const KIND_WIDTH: f32 = 30.;

/// Colour standing for a request kind, taken from the palette's `base.*` slots so
/// it moves with the theme instead of being hard-coded here.
pub fn kind_color(kind: RequestKind, cx: &gpui::App) -> Hsla {
    match kind {
        RequestKind::Topic => cx.theme().blue,
        RequestKind::Service => cx.theme().green,
        RequestKind::Action => cx.theme().magenta,
        RequestKind::Param => cx.theme().yellow,
    }
}

/// A status dot — 6px, semantic colour, used instead of tinting a whole row.
pub fn status_dot(color: Hsla) -> Div {
    div().size(px(6.)).rounded_full().bg(color)
}

/// One entry in a metadata strip: dim label, normal value.
pub fn meta(label: impl Into<SharedString>, value: impl Into<SharedString>, cx: &gpui::App) -> Div {
    h_flex()
        .gap_1()
        .text_xs()
        .child(
            div()
                .text_color(cx.theme().muted_foreground)
                .child(label.into()),
        )
        .child(div().text_color(cx.theme().foreground).child(value.into()))
}

/// Monospace wrapper for anything that is data rather than interface text.
pub fn mono(cx: &gpui::App) -> Div {
    div().font_family(cx.theme().mono_font_family.clone())
}

/// The designed "nothing here yet" block: an icon tile, a heading and one line of
/// explanation, centred in whatever space it is given.
///
/// Returned as a `Div` so callers can append a call to action — the sidebar
/// offers "New request", the response pane has nothing to offer and appends
/// nothing.
pub fn empty_state(
    icon: IconName,
    title: impl Into<SharedString>,
    detail: impl Into<SharedString>,
    cx: &gpui::App,
) -> Div {
    v_flex()
        .flex_1()
        .min_h_0()
        .items_center()
        .justify_center()
        .gap_3()
        .px_6()
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .size_11()
                .rounded(cx.theme().radius_lg)
                // `muted` rather than `secondary`: an empty state appears both on
                // panels and inside cards, and `secondary` *is* the card colour,
                // so the tile would vanish on exactly one of the two.
                .bg(cx.theme().muted)
                .text_color(cx.theme().muted_foreground)
                .child(Icon::new(icon).size_5()),
        )
        .child(
            v_flex()
                .gap_1()
                .items_center()
                .child(
                    div()
                        .text_sm()
                        .font_medium()
                        .text_color(cx.theme().foreground)
                        .child(title.into()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .text_center()
                        .child(detail.into()),
                ),
        )
}

/// A hairline horizontal rule, dimmer than a `Separator`, for use inside cards.
pub fn hairline(cx: &gpui::App) -> Div {
    div().h(px(1.)).w_full().bg(cx.theme().border)
}

/// Colours for one plotted series each, in the order they are handed out.
///
/// Ordered so neighbours stay apart in hue: a plot's lines are told apart by
/// colour alone, and two greens next to each other are two lines nobody can
/// separate. Read from the theme's chart ramp so they suit whichever theme is
/// on rather than being a fixed set that fights half of them.
pub fn series_colors(cx: &gpui::App) -> Vec<gpui::Hsla> {
    let theme = cx.theme();
    vec![
        theme.chart_1,
        theme.chart_3,
        theme.chart_5,
        theme.chart_2,
        theme.chart_4,
    ]
}

/// How far one level of nesting indents a sidebar row.
///
/// Enough to read as a level at a glance without walking the deepest rows off
/// the right of a narrow sidebar. This is the whole indentation: no guide lines.
pub const INDENT: f32 = 14.;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_labels_are_stable_and_distinct() {
        let labels = [
            kind_label(RequestKind::Topic),
            kind_label(RequestKind::Service),
            kind_label(RequestKind::Action),
            kind_label(RequestKind::Param),
        ];
        assert_eq!(labels, ["TOPIC", "SERVICE", "ACTION", "PARAM"]);
        let mut unique: Vec<_> = labels.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 4, "kind labels must be distinguishable");
    }

    #[test]
    fn density_constants_match_the_agreed_scale() {
        // "Comfortable": 36px controls, 44px request bar.
        assert_eq!(CONTROL_HEIGHT, 36.0);
        assert_eq!(REQUEST_BAR_HEIGHT, 44.0);
    }
}
