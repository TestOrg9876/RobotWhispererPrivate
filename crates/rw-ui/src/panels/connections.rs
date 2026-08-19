//! Connections: the ROS systems this workspace talks to.
//!
//! Several are connected at the same time — a robot, a simulator, a bag replay
//! — and a request names which one it targets. That is why this is a managed
//! list with live status rather than a single "active" connection.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconName, Selectable as _, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    v_flex,
};
use rw_core::domain::{Connection, TransportConfig};
use rw_core::storage::NewConnection;

use crate::session::{RobotWhisperer, Sessions, Status};
use crate::tokens;
use crate::workspace::Workspace;
use crate::workspace_view::status_colour;

/// The transports a connection can use, in the order the picker offers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    FoxgloveWs,
    Rosbridge,
    Dummy,
}

impl Transport {
    const ALL: [Self; 3] = [Self::FoxgloveWs, Self::Rosbridge, Self::Dummy];

    fn label(self) -> &'static str {
        match self {
            Self::FoxgloveWs => "Foxglove WebSocket",
            Self::Rosbridge => "rosbridge",
            Self::Dummy => "Dummy",
        }
    }

    /// The URL to start from, which is the one nearly everybody wants.
    fn default_url(self) -> &'static str {
        match self {
            Self::FoxgloveWs => "ws://localhost:8765",
            Self::Rosbridge => "ws://localhost:9090",
            Self::Dummy => "",
        }
    }

    fn needs_url(self) -> bool {
        !matches!(self, Self::Dummy)
    }

    fn of(config: &TransportConfig) -> Self {
        match config {
            TransportConfig::FoxgloveWs { .. } => Self::FoxgloveWs,
            TransportConfig::Rosbridge { .. } => Self::Rosbridge,
            // Native ROS 2 has no transport yet, and a recording is opened from
            // a file rather than typed in, so neither can be created here —
            // they arrive by import or by opening a recording.
            TransportConfig::NativeRos2 { .. }
            | TransportConfig::Dummy {}
            | TransportConfig::Replay { .. } => Self::Dummy,
        }
    }

    fn config(self, url: &str) -> TransportConfig {
        match self {
            Self::FoxgloveWs => TransportConfig::FoxgloveWs {
                url: url.to_string(),
                headers: Vec::new(),
            },
            Self::Rosbridge => TransportConfig::Rosbridge {
                url: url.to_string(),
            },
            Self::Dummy => TransportConfig::Dummy {},
        }
    }
}

/// The URL a stored connection points at, for display and for editing.
fn url_of(config: &TransportConfig) -> String {
    match config {
        TransportConfig::FoxgloveWs { url, .. } | TransportConfig::Rosbridge { url } => url.clone(),
        TransportConfig::NativeRos2 { domain_id } => format!("domain {domain_id}"),
        TransportConfig::Dummy {} => "synthetic topics, services and actions".to_string(),
        TransportConfig::Replay { recording } => {
            // The recording is inline, so its size is the only honest summary
            // without parsing it again here.
            format!("recording, {} KB", recording.len() / 1024)
        }
    }
}

/// Which connection is being edited, if any.
enum Editing {
    None,
    /// A new connection that has not been stored yet.
    New,
    Existing(i64),
}

pub struct ConnectionsPanel {
    focus_handle: FocusHandle,
    workspace: Entity<Workspace>,
    sessions: Entity<Sessions>,

    editing: Editing,
    transport: Transport,
    name: Entity<InputState>,
    url: Entity<InputState>,

    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PanelEvent> for ConnectionsPanel {}

impl ConnectionsPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let global = RobotWhisperer::global(cx);
        let workspace = global.workspace.clone();
        let sessions = global.sessions.clone();

        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Robot"));
        let url = cx.new(|cx| InputState::new(window, cx).placeholder("ws://localhost:8765"));

        let subscriptions = vec![
            cx.observe(&workspace, |_, _, cx| cx.notify()),
            cx.observe(&sessions, |_, _, cx| cx.notify()),
        ];

        Self {
            focus_handle: cx.focus_handle(),
            workspace,
            sessions,
            editing: Editing::None,
            transport: Transport::FoxgloveWs,
            name,
            url,
            _subscriptions: subscriptions,
        }
    }

    pub fn view(window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self::new(window, cx))
    }

    // ── editing ────────────────────────────────────────────────────────────────

    fn start_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = Editing::New;
        self.transport = Transport::FoxgloveWs;
        self.set_form("", Transport::FoxgloveWs.default_url(), window, cx);
        cx.notify();
    }

    fn start_editing(
        &mut self,
        connection: &Connection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editing = Editing::Existing(connection.id);
        self.transport = Transport::of(&connection.config);
        let url = match &connection.config {
            TransportConfig::FoxgloveWs { url, .. } | TransportConfig::Rosbridge { url } => {
                url.clone()
            }
            _ => String::new(),
        };
        self.set_form(&connection.name, &url, window, cx);
        cx.notify();
    }

    fn set_form(&mut self, name: &str, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (name, url) = (name.to_string(), url.to_string());
        self.name
            .update(cx, |state, cx| state.set_value(name, window, cx));
        self.url
            .update(cx, |state, cx| state.set_value(url, window, cx));
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        self.editing = Editing::None;
        cx.notify();
    }

    fn choose_transport(
        &mut self,
        transport: Transport,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.transport = transport;
        // Swapping transport swaps the port that goes with it, unless the URL
        // has been edited away from the previous default.
        let current = self.url.read(cx).value().to_string();
        let was_default = Transport::ALL
            .iter()
            .any(|other| other.default_url() == current);
        if current.is_empty() || was_default {
            let url = transport.default_url().to_string();
            self.url
                .update(cx, |state, cx| state.set_value(url, window, cx));
        }
        cx.notify();
    }

    /// Saves the form, and connects when the connection is new — adding one and
    /// then having to connect it separately is a step with no decision in it.
    fn save(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().trim().to_string();
        let url = self.url.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }
        if self.transport.needs_url() && url.is_empty() {
            return;
        }
        let config = self.transport.config(&url);

        match self.editing {
            Editing::None => return,
            Editing::Existing(id) => {
                let Some(mut connection) = self.workspace.read(cx).connection(id).cloned() else {
                    return;
                };
                connection.name = name;
                connection.config = config;
                self.workspace
                    .update(cx, |workspace, cx| {
                        workspace.update_connection(connection, cx)
                    })
                    .detach();
            }
            Editing::New => {
                let draft = NewConnection {
                    name,
                    config,
                    auto_connect: false,
                    color: None,
                };
                let creating = self
                    .workspace
                    .update(cx, |workspace, cx| workspace.create_connection(draft, cx));

                cx.spawn(async move |panel, cx| {
                    let Some(connection) = creating.await else {
                        return;
                    };
                    panel
                        .update(cx, |panel, cx| {
                            panel
                                .sessions
                                .update(cx, |sessions, cx| sessions.connect(&connection, cx))
                                .detach();
                        })
                        .ok();
                })
                .detach();
            }
        }

        self.editing = Editing::None;
        cx.notify();
    }

    fn toggle(&mut self, id: i64, cx: &mut Context<Self>) {
        let connected = self.sessions.read(cx).status(id).is_connected();
        let Some(connection) = self.workspace.read(cx).connection(id).cloned() else {
            return;
        };
        self.sessions
            .update(cx, |sessions, cx| {
                if connected {
                    sessions.disconnect(id, cx)
                } else {
                    sessions.connect(&connection, cx)
                }
            })
            .detach();
        cx.notify();
    }

    fn delete(&mut self, id: i64, cx: &mut Context<Self>) {
        self.sessions
            .update(cx, |sessions, cx| sessions.disconnect(id, cx))
            .detach();
        self.workspace
            .update(cx, |workspace, cx| workspace.delete_connection(id, cx))
            .detach();
        if matches!(self.editing, Editing::Existing(editing) if editing == id) {
            self.editing = Editing::None;
        }
        cx.notify();
    }

    // ── rendering ──────────────────────────────────────────────────────────────

    fn row(&self, connection: &Connection, cx: &mut Context<Self>) -> AnyElement {
        let id = connection.id;
        let status = self.sessions.read(cx).status(id);
        let connected = status.is_connected();
        let detail = status
            .detail()
            .map(str::to_owned)
            .unwrap_or_else(|| url_of(&connection.config));
        let failed = matches!(status, Status::Failed(_));
        let editing = matches!(self.editing, Editing::Existing(open) if open == id);
        let for_edit = connection.clone();

        tokens::card(cx)
            .when(editing, |card| card.border_color(cx.theme().ring))
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_2p5()
                    .gap_3()
                    .items_center()
                    .child(tokens::status_dot(status_colour(&status, cx)))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_sm()
                                    .font_medium()
                                    .text_color(cx.theme().foreground)
                                    .truncate()
                                    .child(connection.name.clone()),
                            )
                            .child(
                                tokens::mono(cx)
                                    .text_xs()
                                    .text_color(if failed {
                                        cx.theme().danger
                                    } else {
                                        cx.theme().muted_foreground
                                    })
                                    .truncate()
                                    .child(detail),
                            ),
                    )
                    .child(
                        Button::new(("toggle", id as usize))
                            .when(connected, |button| button.outline().label("Disconnect"))
                            .when(!connected, |button| button.primary().label("Connect"))
                            .small()
                            .on_click(
                                cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle(id, cx)),
                            ),
                    )
                    .child(
                        Button::new(("edit", id as usize))
                            .ghost()
                            .small()
                            .icon(IconName::Settings2)
                            .tooltip("Edit")
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.start_editing(&for_edit, window, cx)
                            })),
                    )
                    .child(
                        Button::new(("delete", id as usize))
                            .ghost()
                            .small()
                            .icon(IconName::Delete)
                            .tooltip("Remove")
                            .on_click(
                                cx.listener(move |this, _: &ClickEvent, _, cx| this.delete(id, cx)),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let transport = self.transport;
        let title = match self.editing {
            Editing::New => "New connection",
            _ => "Edit connection",
        };

        tokens::card(cx)
            .child(tokens::card_header(cx).child(tokens::section_label(title, cx)))
            .child(
                v_flex()
                    .p_3()
                    .gap_3()
                    .child(h_flex().gap_2().children(Transport::ALL.map(|option| {
                        Button::new(("transport", option as usize))
                            .small()
                            .label(option.label())
                            .selected(option == transport)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.choose_transport(option, window, cx)
                            }))
                    })))
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(tokens::section_label("Name", cx))
                            .child(Input::new(&self.name).small()),
                    )
                    .when(transport.needs_url(), |form| {
                        form.child(
                            v_flex()
                                .gap_1p5()
                                .child(tokens::section_label("URL", cx))
                                .child(Input::new(&self.url).small()),
                        )
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .child(
                                Button::new("cancel")
                                    .ghost()
                                    .small()
                                    .label("Cancel")
                                    .on_click(
                                        cx.listener(|this, _: &ClickEvent, _, cx| this.cancel(cx)),
                                    ),
                            )
                            .child(
                                Button::new("save")
                                    .primary()
                                    .small()
                                    .label("Save")
                                    .disabled(self.name.read(cx).value().trim().is_empty())
                                    .on_click(
                                        cx.listener(|this, _: &ClickEvent, _, cx| this.save(cx)),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }
}

impl Focusable for ConnectionsPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for ConnectionsPanel {
    fn panel_name(&self) -> &'static str {
        "Connections"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        "Connections"
    }
}

impl Render for ConnectionsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let connections = self.workspace.read(cx).connections().to_vec();
        let rows: Vec<_> = connections
            .iter()
            .map(|connection| self.row(connection, cx))
            .collect();
        let editing = !matches!(self.editing, Editing::None);

        v_flex()
            .id("connections")
            .size_full()
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .flex_shrink_0()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .items_center()
                    .justify_between()
                    // The dock draws the panel's name; repeating it here would
                    // be the second heading saying the same word.
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Connect to as many as you need."),
                    )
                    .child(
                        Button::new("add")
                            .primary()
                            .small()
                            .icon(IconName::Plus)
                            .label("Add")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.start_new(window, cx)
                            })),
                    ),
            )
            .child(
                v_flex()
                    .id("connection-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_3()
                    .pb_3()
                    .gap_2()
                    .when(editing, |list| list.child(self.editor(cx)))
                    .children(rows)
                    .when(connections.is_empty() && !editing, |list| {
                        list.child(
                            tokens::empty_state(
                                IconName::Globe,
                                "No connections yet",
                                "Add a Foxglove or rosbridge endpoint, or a Dummy system to try \
                                 things out without a robot.",
                                cx,
                            )
                            .py(px(32.)),
                        )
                    }),
            )
    }
}
