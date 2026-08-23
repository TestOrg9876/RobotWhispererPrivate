//! Shared presentation helpers.
//!
//! The theme JSON owns colour, radius and base font size. What it cannot express
//! is *composition* — the recurring shapes this app uses. Keeping them here means
//! the section label in the sidebar and the one in the response pane are the same
//! thing rather than two similar-looking accidents.

use gpui::prelude::FluentBuilder as _;
use gpui::{Div, Hsla, ParentElement as _, Rems, SharedString, Styled as _, div, px, rems};
use gpui_component::{ActiveTheme as _, Icon, IconName, StyledExt as _, h_flex, v_flex};
use rw_core::domain::RequestKind;

/// The base font size the design was drawn against.
///
/// The theme sets `font.size` to this, and the library hands it to
/// `Window::set_rem_size`, so one rem *is* this number. Everything below is
/// written as the pixels it was designed at and divided by it, which keeps the
/// intent readable at the call site and still makes the whole interface — not
/// only its text — grow when someone raises the base size.
const DESIGNED_BASE: f32 = 14.0;

/// A length, given the pixels it was designed at.
pub const fn designed(pixels: f32) -> Rems {
    rems(pixels / DESIGNED_BASE)
}

/// A designed length already resolved to pixels.
///
/// For the handful of APIs that take `Pixels` rather than a length — a dialog's
/// width is one — so those still follow the base size instead of being the one
/// thing on screen that does not.
pub fn designed_px(pixels: f32, window: &gpui::Window) -> gpui::Pixels {
    designed(pixels).to_pixels(window.rem_size())
}

/// Scales a designed length by a whole number of steps — an indent by its
/// depth, a list by its rows.
pub fn scaled(length: Rems, times: f32) -> Rems {
    Rems(length.0 * times)
}

/// Height of a standard control, and of the request bar. Comfortable density.
pub const CONTROL_HEIGHT: Rems = designed(36.0);
pub const REQUEST_BAR_HEIGHT: Rems = designed(44.0);
/// Height of the strip along a card's top edge.
pub const CARD_HEADER_HEIGHT: Rems = designed(38.0);

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
///
/// Its top corners are rounded to match. `overflow_hidden` on the card masks to
/// a *rectangle* — it is a scroll clip, not a shape — so a child with a fill of
/// its own paints straight through the card's rounding and leaves two square
/// notches where the corners should be. Inset by the card's one-pixel border so
/// the two arcs are concentric rather than merely close.
pub fn card_header(cx: &gpui::App) -> Div {
    let inner = (cx.theme().radius_lg - px(1.)).max(px(0.));
    h_flex()
        .h(CARD_HEADER_HEIGHT)
        .flex_shrink_0()
        .w_full()
        .items_center()
        .justify_between()
        .gap_3()
        .px_3()
        .bg(cx.theme().muted)
        .rounded_tl(inner)
        .rounded_tr(inner)
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

/// The pill that names a request's kind in a list.
///
/// A tinted fill in the kind's own colour, with the full-strength colour as its
/// text and a hairline of it as the border — legible on both palettes from one
/// value, and a shape rather than three letters of loose mono, which is what
/// made the sidebar read as a log file instead of a list.
pub fn kind_tag(kind: RequestKind, cx: &gpui::App) -> Div {
    let colour = kind_color(kind, cx);
    h_flex()
        .flex_none()
        .w(KIND_WIDTH)
        .justify_center()
        .px_1()
        .py_0p5()
        .rounded_full()
        .bg(colour.opacity(0.14))
        .border_1()
        .border_color(colour.opacity(0.32))
        .text_size(KIND_TEXT)
        .text_color(colour)
        .font_family(cx.theme().mono_font_family.clone())
        .child(kind_short(kind))
}

/// The same pill for something that is not a request kind and has no colour of
/// its own — a dashboard row. Shape without a claim to a hue.
pub fn quiet_tag(label: impl Into<SharedString>, cx: &gpui::App) -> Div {
    h_flex()
        .flex_none()
        .w(KIND_WIDTH)
        .justify_center()
        .px_1()
        .py_0p5()
        .rounded_full()
        .bg(cx.theme().muted)
        .border_1()
        .border_color(cx.theme().border)
        .text_size(KIND_TEXT)
        .text_color(cx.theme().muted_foreground)
        .font_family(cx.theme().mono_font_family.clone())
        .child(label.into())
}

/// One row of a form: what the field is called, what type it is, and — added by
/// the caller — the control that shows or edits it.
///
/// The request editor and the response's pretty view are the same rows: the
/// only difference between filling a message in and reading one back is
/// whether you can type in the box, and a reader should not have to work that
/// out from a different layout.
pub fn field_row(
    path: impl Into<SharedString>,
    type_name: impl Into<SharedString>,
    cx: &gpui::App,
) -> Div {
    h_flex().w_full().gap_3().items_start().child(
        v_flex()
            .w(FIELD_LABEL_WIDTH)
            .flex_shrink_0()
            .gap_0p5()
            // Aligned with the first editor rather than the middle of a
            // list that may be six rows tall.
            .h(CONTROL_HEIGHT)
            .justify_center()
            .child(
                mono(cx)
                    .text_xs()
                    .text_color(cx.theme().foreground)
                    .truncate()
                    .child(path.into()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .truncate()
                    .child(type_name.into()),
            ),
    )
}

/// How wide the name-and-type column of a form row is.
pub const FIELD_LABEL_WIDTH: Rems = designed(220.);

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
pub const KIND_WIDTH: Rems = designed(30.);

/// The gutter left of the kind code, where a collection row draws its
/// disclosure arrow. Reserved on every row so the codes line up whether or not
/// the row can be opened.
pub const KIND_GUTTER: Rems = designed(12.);

/// How big the kind code is set. Smaller than the smallest text step: it is a
/// label on a column, not something anyone reads a sentence of.
pub const KIND_TEXT: Rems = designed(9.);

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
    div().size(designed(6.)).rounded_full().bg(color)
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
///
/// Deliberately wider than [`crate::tree::FIELD_INDENT`]: a collection tree has
/// nothing but nesting to say where a row sits, where a message field carries
/// its own dotted path.
pub const SIDEBAR_INDENT: Rems = designed(14.);

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
        // "Comfortable": 36px controls, 44px request bar — at the base size the
        // design was drawn against, which is what these resolve to when the
        // theme's `font.size` is left alone.
        assert_eq!(CONTROL_HEIGHT.0 * DESIGNED_BASE, 36.0);
        assert_eq!(REQUEST_BAR_HEIGHT.0 * DESIGNED_BASE, 44.0);
    }

    /// The whole point of the rem migration: raise the base and everything
    /// raises with it, in proportion.
    #[test]
    fn every_designed_length_scales_with_the_base() {
        let doubled = DESIGNED_BASE * 2.0;
        assert_eq!(CONTROL_HEIGHT.0 * doubled, 72.0);
        assert_eq!(FIELD_LABEL_WIDTH.0 * doubled, 440.0);
        assert_eq!(SIDEBAR_INDENT.0 * doubled, 28.0);
    }
}
