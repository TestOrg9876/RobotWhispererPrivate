//! The command palette's window: a field, a list, and the keyboard.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render,
    ScrollStrategy, StatefulInteractiveElement as _, Styled as _, Subscription,
    UniformListScrollHandle, Window, div, uniform_list,
};
use gpui_component::{
    ActiveTheme as _, h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};

use crate::palette::{Choice, Entry, search};
use crate::tokens;

/// How many results are visible before they scroll.
///
/// The list is virtualised, so this is also what decides how many rows are ever
/// built: ten of them, whether the robot advertises twelve topics or twelve
/// hundred. Counted in rows rather than pixels now that a row is a rem-based
/// height — ten rows is the thing that was meant, and it stays ten rows at any
/// base font size.
const VISIBLE_ROWS: f32 = 10.;

/// What the palette decided.
#[derive(Debug, Clone)]
pub enum PaletteEvent {
    Chose(Choice),
    Dismissed,
}

pub struct PaletteView {
    focus_handle: FocusHandle,
    query: Entity<InputState>,
    entries: Vec<Entry>,
    /// What `search` last returned, kept rather than recomputed.
    ///
    /// This used to be a method, and each of its three callers — the render,
    /// the arrow keys, Enter — re-ran the whole search and cloned every hit. On
    /// a robot advertising three hundred topics that was three searches and
    /// three hundred clones per keystroke, to draw the nine rows that fit.
    matches: Vec<Entry>,
    highlighted: usize,
    scroll: UniformListScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PaletteEvent> for PaletteView {}

impl PaletteView {
    pub fn new(
        entries: Vec<Entry>,
        placeholder: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));

        let subscriptions =
            vec![
                cx.subscribe(&query, |this, _, event: &InputEvent, cx| match event {
                    InputEvent::Change => {
                        this.refilter(cx);
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => this.choose(cx),
                    _ => {}
                }),
            ];

        // The field starts empty, so this is the whole list — but it goes
        // through `search` all the same, because the order it puts things in is
        // part of what the palette is.
        let matches = search(&entries, &query.read(cx).value());

        Self {
            focus_handle: cx.focus_handle(),
            query,
            entries,
            matches,
            highlighted: 0,
            scroll: UniformListScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    /// The same list and the same ranking over whatever the caller is
    /// searching — a topic picker is this with a different list in it, not a
    /// second search box with its own rules.
    pub fn view(
        entries: Vec<Entry>,
        placeholder: &'static str,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| Self::new(entries, placeholder, window, cx))
    }

    /// Focuses the field, so the palette is typed into the moment it opens.
    ///
    /// Through the input's own `focus` rather than the window's: it also starts
    /// the caret blinking, which is the difference between a field that looks
    /// ready and one that looks inert.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.query.update(cx, |state, cx| state.focus(window, cx));
    }

    /// Re-runs the search and puts the highlight back on the best match.
    ///
    /// The list changes under the highlight, so it goes back to the top rather
    /// than to whatever row happens to be where the old one was.
    fn refilter(&mut self, cx: &App) {
        self.matches = search(&self.entries, &self.query.read(cx).value());
        self.highlighted = 0;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
    }

    fn choose(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self
            .matches
            .get(self.highlighted.min(self.matches.len().saturating_sub(1)))
        else {
            return;
        };
        cx.emit(PaletteEvent::Chose(entry.choice.clone()));
    }

    fn move_highlight(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.matches.len() as isize;
        if count == 0 {
            return;
        }
        self.highlighted = (self.highlighted as isize + delta).rem_euclid(count) as usize;
        // The highlight can now be well outside the nine rows on screen, since
        // the list only builds what it can see.
        self.scroll
            .scroll_to_item(self.highlighted, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn row(
        &self,
        index: usize,
        highlighted_index: usize,
        entry: &Entry,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let highlighted = index == highlighted_index;
        let choice = entry.choice.clone();

        h_flex()
            .id(("palette", index))
            .h(tokens::CONTROL_HEIGHT)
            .w_full()
            .px_3()
            .gap_3()
            .items_center()
            .rounded(cx.theme().radius)
            .when(highlighted, |row| row.bg(cx.theme().list_active))
            .when(!highlighted, |row| {
                row.hover(|row| row.bg(cx.theme().list_hover))
            })
            .child(
                div()
                    .w(tokens::designed(84.))
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(entry.group),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .truncate()
                    .child(entry.label.clone()),
            )
            .when_some(entry.detail.clone(), |row, detail| {
                row.child(
                    tokens::mono(cx)
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(detail),
                )
            })
            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                cx.emit(PaletteEvent::Chose(choice.clone()));
            }))
            .into_any_element()
    }
}

impl Focusable for PaletteView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PaletteView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.highlighted = self.highlighted.min(self.matches.len().saturating_sub(1));
        let empty = self.matches.is_empty();
        // Cloned into the list's closure, which runs after this returns and so
        // cannot borrow the view. It is the same `Vec<Entry>` the search
        // produced — one allocation, not one per row.
        let matches = self.matches.clone();
        let highlighted = self.highlighted;
        let scroll = self.scroll.clone();
        let rows = cx.entity();
        // Shrunk to what there is, capped at what fits: a palette with three
        // hits should not leave a third of a page of nothing under them.
        let height = tokens::scaled(
            tokens::CONTROL_HEIGHT,
            (self.matches.len() as f32).min(VISIBLE_ROWS),
        );

        v_flex()
            .id("palette")
            .w_full()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                match event.keystroke.key.as_str() {
                    "down" => this.move_highlight(1, cx),
                    "up" => this.move_highlight(-1, cx),
                    "escape" => cx.emit(PaletteEvent::Dismissed),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .child(div().px_1().pb_2().child(Input::new(&self.query)))
            .child(tokens::hairline(cx))
            .child(
                v_flex()
                    .pt_2()
                    .when(!empty, |list| {
                        list.child(
                            uniform_list("palette-list", matches.len(), {
                                move |range, window, cx| {
                                    range
                                        .clone()
                                        .map(|index| {
                                            rows.update(cx, |view, cx| {
                                                view.row(
                                                    index,
                                                    highlighted,
                                                    &matches[index],
                                                    window,
                                                    cx,
                                                )
                                            })
                                        })
                                        .collect::<Vec<_>>()
                                }
                            })
                            .track_scroll(&scroll)
                            .h(height),
                        )
                    })
                    .when(empty, |list| {
                        list.child(
                            div()
                                .p_4()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("Nothing matches that."),
                        )
                    }),
            )
    }
}
