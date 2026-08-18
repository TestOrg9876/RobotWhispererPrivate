//! Collections: the saved requests, which are this app's primary artifact.
//!
//! Postman's model. Requests are named, saved, searched and duplicated; several
//! may target the same service under different names with different payloads.
//! Connections do not appear here — they are environments, selected per request.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::{
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use rw_core::domain::Request;

use crate::tokens;
use crate::workspace::Workspace;

/// What the sidebar asks the shell to do.
#[derive(Debug, Clone)]
pub enum CollectionsEvent {
    Open(i64),
    Duplicate(i64),
    Delete(i64),
    New,
}

pub struct CollectionsPanel {
    focus_handle: FocusHandle,
    workspace: Entity<Workspace>,
    search: Entity<InputState>,
    /// Which request is highlighted, so the row reads as selected.
    selected: Option<i64>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CollectionsEvent> for CollectionsPanel {}
impl EventEmitter<PanelEvent> for CollectionsPanel {}

impl CollectionsPanel {
    pub fn new(workspace: Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search requests"));

        let subscriptions = vec![
            cx.observe(&workspace, |_, _, cx| cx.notify()),
            cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];

        Self {
            focus_handle: cx.focus_handle(),
            workspace,
            search,
            selected: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn view(workspace: Entity<Workspace>, window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self::new(workspace, window, cx))
    }

    /// Marks a request as the selected row. The shell calls this when a request
    /// is opened from anywhere, so the sidebar stays in step.
    pub fn select(&mut self, request: Option<i64>, cx: &mut Context<Self>) {
        self.selected = request;
        cx.notify();
    }

    /// Requests matching the search box. Matching covers name *and* target, so
    /// typing a topic finds every request pointed at it — the main reason to keep
    /// several requests per target.
    fn matches(&self, cx: &App) -> Vec<Request> {
        let query = self.search.read(cx).value().trim().to_lowercase();

        self.workspace
            .read(cx)
            .requests()
            .iter()
            .filter(|request| {
                query.is_empty()
                    || request.name.to_lowercase().contains(&query)
                    || request.target.to_lowercase().contains(&query)
            })
            .cloned()
            .collect()
    }

    fn row(&self, request: &Request, cx: &mut Context<Self>) -> AnyElement {
        let id = request.id;
        let selected = self.selected == Some(id);
        let target = request.target.clone();
        let kind = request.kind;

        h_flex()
            .id(("request", id as usize))
            .h(px(tokens::CONTROL_HEIGHT))
            .w_full()
            .px_2()
            .gap_2()
            .items_center()
            .rounded(cx.theme().radius)
            .when(selected, |row| row.bg(cx.theme().list_active))
            .when(!selected, |row| {
                row.hover(|row| row.bg(cx.theme().list_hover))
            })
            .child(tokens::status_dot(tokens::kind_color(kind, cx)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .truncate()
                            .child(request.name.clone()),
                    )
                    .when(!target.is_empty(), |stack| {
                        stack.child(
                            tokens::mono(cx)
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .truncate()
                                .child(target.clone()),
                        )
                    }),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.selected = Some(id);
                cx.emit(CollectionsEvent::Open(id));
                cx.notify();
            }))
            .into_any_element()
    }

    fn empty_state(&self, cx: &mut Context<Self>) -> AnyElement {
        tokens::empty_state(
            IconName::Inbox,
            "No requests yet",
            "Save a topic, service or action call to reuse it later.",
            cx,
        )
        .child(
            Button::new("empty-new-request")
                .primary()
                .small()
                .icon(IconName::Plus)
                .label("New request")
                .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                    cx.emit(CollectionsEvent::New);
                })),
        )
        .into_any_element()
    }
}

impl Focusable for CollectionsPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for CollectionsPanel {
    fn panel_name(&self) -> &'static str {
        "Collections"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        "Requests"
    }

    fn closable(&self, _cx: &App) -> bool {
        false
    }
}

impl Render for CollectionsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let requests = self.matches(cx);
        let total = self.workspace.read(cx).requests().len();
        let rows: Vec<_> = requests
            .iter()
            .map(|request| self.row(request, cx))
            .collect();
        let searching = !self.search.read(cx).value().trim().is_empty();

        v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .child(
                // Header: title, count, and the primary create action.
                v_flex()
                    .p_3()
                    .gap_3()
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_baseline()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_semibold()
                                            .text_color(cx.theme().foreground)
                                            .child("Requests"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!("{total}")),
                                    ),
                            )
                            .child(
                                Button::new("new-request")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Plus)
                                    .tooltip("New request")
                                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                                        cx.emit(CollectionsEvent::New);
                                    })),
                            ),
                    )
                    .child(Input::new(&self.search).small().cleanable(true)),
            )
            .child(tokens::hairline(cx))
            .child(if rows.is_empty() && !searching {
                self.empty_state(cx)
            } else {
                v_flex()
                    .id("request-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_2()
                    .gap_0p5()
                    .children(rows)
                    .when(requests.is_empty() && searching, |list| {
                        list.child(tokens::empty_state(
                            IconName::Search,
                            "No matches",
                            "No request's name or target contains that text.",
                            cx,
                        ))
                    })
                    .into_any_element()
            })
    }
}
