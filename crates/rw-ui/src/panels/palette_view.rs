//! The command palette's window: a field, a list, and the keyboard.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};

use crate::palette::{Choice, Entry, search};
use crate::tokens;

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
    highlighted: usize,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PaletteEvent> for PaletteView {}

impl PaletteView {
    pub fn new(entries: Vec<Entry>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search commands, requests and connections")
        });

        let subscriptions = vec![cx.subscribe(&query, |this, _, event: &InputEvent, cx| {
            match event {
                InputEvent::Change => {
                    // The list changes under the highlight, so it goes back to
                    // the best match rather than to whatever row is now there.
                    this.highlighted = 0;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.choose(cx),
                _ => {}
            }
        })];

        Self {
            focus_handle: cx.focus_handle(),
            query,
            entries,
            highlighted: 0,
            _subscriptions: subscriptions,
        }
    }

    pub fn view(entries: Vec<Entry>, window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self::new(entries, window, cx))
    }

    /// Focuses the field, so the palette is typed into the moment it opens.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.query.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    fn matches(&self, cx: &App) -> Vec<Entry> {
        search(&self.entries, &self.query.read(cx).value())
    }

    fn choose(&mut self, cx: &mut Context<Self>) {
        let matches = self.matches(cx);
        let Some(entry) = matches.get(self.highlighted.min(matches.len().saturating_sub(1))) else {
            return;
        };
        cx.emit(PaletteEvent::Chose(entry.choice.clone()));
    }

    fn move_highlight(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.matches(cx).len() as isize;
        if count == 0 {
            return;
        }
        self.highlighted = (self.highlighted as isize + delta).rem_euclid(count) as usize;
        cx.notify();
    }

    fn row(&self, index: usize, entry: &Entry, cx: &mut Context<Self>) -> AnyElement {
        let highlighted = index == self.highlighted;
        let choice = entry.choice.clone();

        h_flex()
            .id(("palette", index))
            .h(px(tokens::CONTROL_HEIGHT))
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
                    .w(px(84.))
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
        let matches = self.matches(cx);
        self.highlighted = self.highlighted.min(matches.len().saturating_sub(1));
        let rows: Vec<_> = matches
            .iter()
            .enumerate()
            .map(|(index, entry)| self.row(index, entry, cx))
            .collect();

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
                    .id("palette-list")
                    .max_h(px(360.))
                    .overflow_y_scroll()
                    .pt_2()
                    .gap_0p5()
                    .children(rows)
                    .when(matches.is_empty(), |list| {
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
