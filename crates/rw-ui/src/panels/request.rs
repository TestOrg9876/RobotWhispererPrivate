//! The request editor: a saved, named, reusable call.
//!
//! Layout, top to bottom: the request's name and kind, then the request bar
//! (kind, target, environment, send), then the payload form for services and
//! actions, then the response.

use std::sync::{Arc, Mutex};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, px,
};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::DropdownMenu as _,
    tab::{Tab, TabBar},
    v_flex,
};
use rw_canonical::CanonicalValue;
use rw_core::domain::{Request, RequestKind};

use crate::session::{RobotWhisperer, Sessions};
use crate::tokens;
use crate::value;
use crate::workspace::Workspace;

/// Written by the subscription callback, read while rendering. `Arc<Mutex<_>>`
/// because `subscribe_topic` requires `Send` on native.
#[derive(Default)]
struct Incoming {
    value: Option<CanonicalValue>,
    schema: Option<SharedString>,
    count: u64,
}

/// Why the request cannot run, and what the user can do about it.
///
/// A bare message is not enough here: the commonest reason a request will not
/// send is an environment that exists but is not connected, and telling someone
/// that without offering the one-click fix is just a scolding.
struct Problem {
    message: SharedString,
    /// The environment to connect, when connecting is the fix.
    connect: Option<i64>,
}

impl Problem {
    fn new(message: impl Into<SharedString>) -> Self {
        Self {
            message: message.into(),
            connect: None,
        }
    }

    fn connecting(connection: i64, message: impl Into<SharedString>) -> Self {
        Self {
            message: message.into(),
            connect: Some(connection),
        }
    }
}

/// Which response view is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseTab {
    Raw,
    Visualize,
    Plot,
}

impl ResponseTab {
    const ALL: [Self; 3] = [Self::Raw, Self::Visualize, Self::Plot];

    fn label(self) -> &'static str {
        match self {
            Self::Raw => "Raw",
            Self::Visualize => "Visualize",
            Self::Plot => "Plot",
        }
    }

    /// Position in [`Self::ALL`], which is what `TabBar` selects by.
    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|tab| *tab == self)
            .expect("every variant is listed in ALL")
    }
}

/// Change the request's kind. Carries the discriminant so one action serves the
/// whole menu.
#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct SetKind(pub u8);

/// Point the request at a stored environment.
#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct UseEnvironment(pub i64);

fn kind_from_discriminant(value: u8) -> RequestKind {
    match value {
        1 => RequestKind::Service,
        2 => RequestKind::Action,
        _ => RequestKind::Topic,
    }
}

fn discriminant_of(kind: RequestKind) -> u8 {
    match kind {
        RequestKind::Topic => 0,
        RequestKind::Service => 1,
        RequestKind::Action => 2,
    }
}

pub struct RequestPanel {
    focus_handle: FocusHandle,
    workspace: Entity<Workspace>,
    sessions: Entity<Sessions>,

    /// What is stored, and the edited copy. `dirty` compares them.
    saved: Request,
    draft: Request,

    name: Entity<InputState>,
    target: Entity<InputState>,

    incoming: Arc<Mutex<Incoming>>,
    subscription: Option<String>,
    tab: ResponseTab,
    problem: Option<Problem>,
    _repaint: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PanelEvent> for RequestPanel {}

impl RequestPanel {
    pub fn new(request: &Request, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let global = RobotWhisperer::global(cx);
        let workspace = global.workspace.clone();
        let sessions = global.sessions.clone();

        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Request name")
                .default_value(&request.name)
        });
        let target = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("/topic, /service or /action")
                .default_value(&request.target)
        });

        let subscriptions = vec![
            cx.observe(&sessions, |_, _, cx| cx.notify()),
            cx.subscribe(&name, |this, state, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.draft.name = state.read(cx).value().to_string();
                    cx.notify();
                }
            }),
            cx.subscribe(&target, |this, state, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.draft.target = state.read(cx).value().to_string();
                    cx.notify();
                }
            }),
        ];

        Self {
            focus_handle: cx.focus_handle(),
            workspace,
            sessions,
            saved: request.clone(),
            draft: request.clone(),
            name,
            target,
            incoming: Arc::new(Mutex::new(Incoming::default())),
            subscription: None,
            tab: ResponseTab::Raw,
            problem: None,
            _repaint: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn view(request: &Request, window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self::new(request, window, cx))
    }

    pub fn request_id(&self) -> Option<i64> {
        Some(self.saved.id)
    }

    pub fn kind(&self) -> RequestKind {
        self.draft.kind
    }

    /// Tab label: the request's name, or its target while unnamed.
    pub fn title(&self) -> SharedString {
        let name = self.draft.name.trim();
        if !name.is_empty() {
            return name.to_string().into();
        }
        let target = self.draft.target.trim();
        if target.is_empty() {
            "Untitled".into()
        } else {
            target.to_string().into()
        }
    }

    /// True while the draft differs from what is stored.
    pub fn dirty(&self) -> bool {
        self.draft.name != self.saved.name
            || self.draft.target != self.saved.target
            || self.draft.kind != self.saved.kind
            || self.draft.connection_id != self.saved.connection_id
            || self.draft.input != self.saved.input
    }

    fn running(&self) -> bool {
        self.subscription.is_some()
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if !self.dirty() {
            return;
        }
        self.saved = self.draft.clone();
        let request = self.draft.clone();

        self.workspace
            .update(cx, |workspace, cx| workspace.save_request(request, cx))
            .detach();
        cx.notify();
    }

    // ── running ────────────────────────────────────────────────────────────────

    fn session(&self, cx: &App) -> Option<rw_transport::ConnectionId> {
        self.draft
            .connection_id
            .and_then(|id| self.sessions.read(cx).session(id))
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let target = self.draft.target.trim().to_string();
        if target.is_empty() {
            self.problem = Some(Problem::new("Enter a target first"));
            cx.notify();
            return;
        }
        let Some(session) = self.session(cx) else {
            self.problem = Some(self.why_not_connected(cx));
            cx.notify();
            return;
        };

        self.problem = None;
        *self.incoming.lock().expect("incoming mutex") = Incoming::default();

        let pipeline = self.sessions.read(cx).pipeline();
        let incoming = Arc::clone(&self.incoming);

        cx.spawn(async move |panel, cx| {
            let outcome = pipeline
                .subscribe_topic(session, &target, move |_handle, frame, _lossy| {
                    let Ok(mut incoming) = incoming.lock() else {
                        return;
                    };
                    incoming.value = Some(frame.value.clone());
                    incoming.schema = Some(frame.schema.name.clone().into());
                    incoming.count += 1;
                })
                .await;

            panel
                .update(cx, |panel, cx| {
                    match outcome {
                        Ok(result) => {
                            panel.subscription = Some(result.subscription_id);
                            panel.start_repaint(cx);
                        }
                        Err(error) => panel.problem = Some(Problem::new(error.to_string())),
                    }
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    /// Distinguishes "no environment chosen" from "chosen but not connected",
    /// because only the second one has a fix the panel can offer.
    fn why_not_connected(&self, cx: &App) -> Problem {
        let Some(id) = self.draft.connection_id else {
            return Problem::new("Choose an environment for this request");
        };
        match self.workspace.read(cx).connection(id) {
            Some(connection) => {
                Problem::connecting(id, format!("{} is not connected", connection.name))
            }
            None => Problem::new("This request points at an environment that no longer exists"),
        }
    }

    fn connect(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(connection) = self.workspace.read(cx).connection(id).cloned() else {
            return;
        };
        self.problem = None;
        self.sessions
            .update(cx, |sessions, cx| sessions.connect(&connection, cx))
            .detach();
        cx.notify();
    }

    /// Frames land on a transport thread. Repaint on a timer rather than per
    /// frame, so a 1 kHz topic cannot drive 1 kHz of layout.
    fn start_repaint(&mut self, cx: &mut Context<Self>) {
        self._repaint = Some(cx.spawn(async move |panel, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(100))
                    .await;
                if panel.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(subscription) = self.subscription.take() else {
            return;
        };
        self._repaint = None;
        let pipeline = self.sessions.read(cx).pipeline();

        cx.spawn(async move |panel, cx| {
            let outcome = pipeline.unsubscribe(&subscription).await;
            panel
                .update(cx, |panel, cx| {
                    if let Err(error) = outcome {
                        panel.problem = Some(Problem::new(error.to_string()));
                    }
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    // ── chrome ─────────────────────────────────────────────────────────────────

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let kind = self.draft.kind;
        let dirty = self.dirty();

        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(tokens::kind_tag(kind, cx).child(tokens::kind_label(kind)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .font_semibold()
                    // The name is the largest text in the window: 20px, per the
                    // type scale. `Input` derives its own size, so it is asked
                    // rather than styled from outside.
                    .child(Input::new(&self.name).appearance(false).with_size(px(23.))),
            )
            .when(dirty, |row| {
                row.child(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .child(div().size(px(6.)).rounded_full().bg(cx.theme().warning))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Unsaved"),
                        ),
                )
            })
            .child(
                Button::new("save")
                    .ghost()
                    .small()
                    .icon(IconName::Check)
                    .label("Save")
                    .disabled(!dirty)
                    .tooltip("Save request")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.save(cx))),
            )
            .into_any_element()
    }

    /// The prominent row: kind, target, environment, primary action.
    fn request_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let kind = self.draft.kind;
        let running = self.running();
        let kind_colour = tokens::kind_color(kind, cx);

        let environment = self
            .draft
            .connection_id
            .and_then(|id| {
                self.workspace
                    .read(cx)
                    .connection(id)
                    .map(|connection| connection.name.clone())
            })
            .unwrap_or_else(|| "Environment".to_string());

        let environments: Vec<_> = self
            .workspace
            .read(cx)
            .connections()
            .iter()
            .map(|connection| (connection.id, connection.name.clone()))
            .collect();
        let chosen = self.draft.connection_id;

        h_flex()
            .h(px(tokens::REQUEST_BAR_HEIGHT))
            .w_full()
            .items_center()
            .gap_2()
            .px_2()
            .rounded(cx.theme().radius)
            .bg(cx.theme().secondary)
            .border_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("kind")
                    .ghost()
                    .small()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_center()
                            .child(
                                div()
                                    .text_xs()
                                    .font_medium()
                                    .text_color(kind_colour)
                                    .child(tokens::kind_label(kind)),
                            )
                            .child(Icon::new(IconName::ChevronDown).xsmall()),
                    )
                    .dropdown_menu(move |mut menu, _window, _cx| {
                        for option in [
                            RequestKind::Topic,
                            RequestKind::Service,
                            RequestKind::Action,
                        ] {
                            menu = menu.menu_with_check(
                                tokens::kind_label(option),
                                option == kind,
                                Box::new(SetKind(discriminant_of(option))),
                            );
                        }
                        menu
                    }),
            )
            .child(div().w(px(1.)).h_5().bg(cx.theme().border))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.target).appearance(false)),
            )
            .child(
                Button::new("environment")
                    .ghost()
                    .small()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_center()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(environment),
                            )
                            .child(Icon::new(IconName::ChevronDown).xsmall()),
                    )
                    .dropdown_menu(move |mut menu, _window, _cx| {
                        if environments.is_empty() {
                            return menu.menu(
                                "No environments yet",
                                Box::new(crate::actions::NewConnection),
                            );
                        }
                        for (id, name) in &environments {
                            menu = menu.menu_with_check(
                                name.clone(),
                                chosen == Some(*id),
                                Box::new(UseEnvironment(*id)),
                            );
                        }
                        menu
                    }),
            )
            .child(
                Button::new("send")
                    .when(running, |button| {
                        button.danger().icon(IconName::Pause).label("Stop")
                    })
                    .when(!running, |button| {
                        button.primary().icon(IconName::Play).label(match kind {
                            RequestKind::Topic => "Subscribe",
                            RequestKind::Service => "Call",
                            RequestKind::Action => "Send goal",
                        })
                    })
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        if this.running() {
                            this.stop(cx);
                        } else {
                            this.start(cx);
                        }
                    })),
            )
            .into_any_element()
    }

    fn payload(&self, cx: &mut Context<Self>) -> AnyElement {
        tokens::card(cx)
            .flex_shrink_0()
            .child(tokens::card_header(cx).child(tokens::section_label(
                match self.draft.kind {
                    RequestKind::Service => "Request",
                    _ => "Goal",
                },
                cx,
            )))
            .child(
                tokens::card_body().child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("The schema-driven form arrives with the next milestone."),
                ),
            )
            .into_any_element()
    }

    /// The banner explaining why the request will not run, with the fix attached
    /// when there is one.
    fn problem(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let problem = self.problem.as_ref()?;

        Some(
            h_flex()
                .flex_shrink_0()
                .mt_4()
                .mx_4()
                .px_3()
                .py_2()
                .gap_2()
                .items_center()
                .rounded(cx.theme().radius)
                .bg(cx.theme().danger.opacity(0.12))
                .border_1()
                .border_color(cx.theme().danger.opacity(0.32))
                .child(
                    Icon::new(IconName::CircleX)
                        .small()
                        .text_color(cx.theme().danger),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(problem.message.clone()),
                )
                .when_some(problem.connect, |banner, id| {
                    banner.child(
                        Button::new("connect-environment")
                            .outline()
                            .small()
                            .icon(IconName::Globe)
                            .label("Connect")
                            .on_click(
                                cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.connect(id, cx)
                                }),
                            ),
                    )
                })
                .into_any_element(),
        )
    }

    fn response(&self, cx: &mut Context<Self>) -> AnyElement {
        let active = self.tab;
        let running = self.running();
        let (value, schema, count) = {
            let incoming = self.incoming.lock().expect("incoming mutex");
            (
                incoming.value.clone(),
                incoming.schema.clone(),
                incoming.count,
            )
        };

        // A segmented bar rather than document tabs: these are three views of one
        // response, not three things that can be closed.
        let tabs = TabBar::new("response-views")
            .segmented()
            .selected_index(active.index())
            .children(ResponseTab::ALL.map(|tab| Tab::new().label(tab.label())))
            .on_click(cx.listener(|this, index: &usize, _, cx| {
                if let Some(tab) = ResponseTab::ALL.get(*index) {
                    this.tab = *tab;
                    cx.notify();
                }
            }));

        let stats = h_flex()
            .gap_3()
            .flex_shrink_0()
            .child(tokens::meta("Messages", count.to_string(), cx))
            .when_some(schema, |row, schema| {
                row.child(tokens::meta("Schema", schema, cx))
            });

        let body = match (&value, active) {
            (Some(value), ResponseTab::Raw) => tokens::mono(cx)
                .text_xs()
                .text_color(cx.theme().foreground)
                .child(value::preview(value))
                .into_any_element(),
            (Some(_), _) => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("This view arrives with the visualizer milestone.")
                .into_any_element(),
            (None, _) => tokens::empty_state(
                IconName::Inbox,
                if running {
                    "Waiting for the first message…"
                } else {
                    "Not running"
                },
                if running {
                    "The subscription is open; nothing has arrived yet."
                } else {
                    "Send the request to see its response here."
                },
                cx,
            )
            .into_any_element(),
        };

        tokens::card(cx)
            // Fills whatever the head of the panel leaves, so the response is the
            // pane's main event rather than a box that grows with its content.
            .flex_1()
            .min_h_0()
            .child(tokens::card_header(cx).child(tabs).child(stats))
            .child(
                tokens::card_body()
                    .id("response-body")
                    .overflow_scroll()
                    .child(body),
            )
            .into_any_element()
    }
}

impl Focusable for RequestPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for RequestPanel {
    fn panel_name(&self) -> &'static str {
        "Request"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        RequestPanel::title(self)
    }
}

impl Render for RequestPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.header(cx);
        let bar = self.request_bar(cx);
        let payload = matches!(self.draft.kind, RequestKind::Service | RequestKind::Action)
            .then(|| self.payload(cx));
        let response = self.response(cx);
        let problem = self.problem(cx);

        v_flex()
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .on_action(cx.listener(|this, action: &SetKind, _, cx| {
                this.draft.kind = kind_from_discriminant(action.0);
                cx.notify();
            }))
            .on_action(cx.listener(|this, action: &UseEnvironment, _, cx| {
                this.draft.connection_id = Some(action.0);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &crate::actions::SaveRequest, _, cx| this.save(cx)))
            // The head is fixed: name, kind and target stay put while the
            // response scrolls, which is the whole point of a request bar. It
            // also sits on the panel surface, continuing the tab strip above it,
            // so chrome and canvas are two visibly different bands.
            .child(
                v_flex()
                    .flex_shrink_0()
                    .p_4()
                    .gap_3()
                    .bg(cx.theme().tab_bar)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(header)
                    .child(bar),
            )
            .children(problem)
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .p_4()
                    .gap_3()
                    .children(payload)
                    .child(response),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_discriminants_round_trip() {
        for kind in [
            RequestKind::Topic,
            RequestKind::Service,
            RequestKind::Action,
        ] {
            assert_eq!(kind_from_discriminant(discriminant_of(kind)), kind);
        }
    }

    #[test]
    fn an_unknown_discriminant_falls_back_to_topic() {
        assert_eq!(kind_from_discriminant(200), RequestKind::Topic);
    }

    #[test]
    fn response_tabs_have_distinct_labels() {
        let labels: Vec<_> = ResponseTab::ALL.iter().map(|tab| tab.label()).collect();
        assert_eq!(labels, ["Raw", "Visualize", "Plot"]);
    }
}
