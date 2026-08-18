//! Explorer: connections, their discovered topics, and saved requests.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, relative,
};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::{
    ActiveTheme as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    sidebar::{Sidebar, SidebarGroup, SidebarMenu, SidebarMenuItem},
    v_flex,
};

use crate::session::{RobotWhisperer, Sessions, Status};
use crate::workspace::Workspace;

/// Raised when the user picks something the workspace should open.
#[derive(Debug, Clone)]
pub enum ExplorerEvent {
    OpenRequest(i64),
    /// A topic was chosen from a connection's discovery list.
    OpenTopic {
        connection: i64,
        topic: SharedString,
    },
    /// Toolbar asked for a new connection.
    NewConnection,
    /// Toolbar asked for a new request.
    NewRequest,
}

impl EventEmitter<ExplorerEvent> for ExplorerPanel {}
impl EventEmitter<PanelEvent> for ExplorerPanel {}

pub struct ExplorerPanel {
    focus_handle: FocusHandle,
    workspace: Entity<Workspace>,
    sessions: Entity<Sessions>,
    /// Connections whose topic list is expanded.
    expanded: Vec<i64>,
}

impl ExplorerPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let global = RobotWhisperer::global(cx);
        let workspace = global.workspace.clone();
        let sessions = global.sessions.clone();

        cx.observe(&workspace, |_, _, cx| cx.notify()).detach();
        cx.observe(&sessions, |_, _, cx| cx.notify()).detach();

        let _ = window;
        Self {
            focus_handle: cx.focus_handle(),
            workspace,
            sessions,
            expanded: Vec::new(),
        }
    }

    pub fn view(window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self::new(window, cx))
    }

    fn toggle_connection(&mut self, id: i64, cx: &mut Context<Self>) {
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
    }

    fn connections(&self, cx: &mut Context<Self>) -> Vec<SidebarMenuItem> {
        let sessions = self.sessions.read(cx);

        self.workspace
            .read(cx)
            .connections()
            .iter()
            .map(|connection| {
                let id = connection.id;
                let status = sessions.status(id);
                let expanded = self.expanded.contains(&id);

                let topics: Vec<_> = sessions
                    .discovery(id)
                    .map(|discovery| {
                        discovery
                            .topics
                            .iter()
                            .map(|topic| {
                                let name = SharedString::from(topic.name.clone());
                                SidebarMenuItem::new(name.clone()).on_click(cx.listener(
                                    move |_, _: &ClickEvent, _, cx| {
                                        cx.emit(ExplorerEvent::OpenTopic {
                                            connection: id,
                                            topic: name.clone(),
                                        });
                                    },
                                ))
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                SidebarMenuItem::new(connection.name.clone())
                    .icon(status_icon(&status))
                    .default_open(expanded)
                    .click_to_toggle(true)
                    .children(topics)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if this.expanded.contains(&id) {
                            this.expanded.retain(|entry| *entry != id);
                        } else {
                            this.expanded.push(id);
                        }
                        this.toggle_connection(id, cx);
                    }))
            })
            .collect()
    }

    fn requests(&self, cx: &mut Context<Self>) -> Vec<SidebarMenuItem> {
        self.workspace
            .read(cx)
            .requests()
            .iter()
            .map(|request| {
                let id = request.id;
                SidebarMenuItem::new(request.name.clone())
                    .icon(kind_icon(request.kind))
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        cx.emit(ExplorerEvent::OpenRequest(id));
                    }))
            })
            .collect()
    }
}

fn status_icon(status: &Status) -> IconName {
    match status {
        Status::Connected => IconName::CircleCheck,
        Status::Connecting | Status::Reconnecting => IconName::LoaderCircle,
        Status::Failed(_) => IconName::CircleX,
        Status::Disconnected => IconName::Dash,
    }
}

fn kind_icon(kind: rw_core::domain::RequestKind) -> IconName {
    use rw_core::domain::RequestKind::{Action, Service, Topic};
    match kind {
        Topic => IconName::Inbox,
        Service => IconName::Bot,
        Action => IconName::Star,
    }
}

impl Focusable for ExplorerPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for ExplorerPanel {
    fn panel_name(&self) -> &'static str {
        "Explorer"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        "Explorer"
    }

    fn closable(&self, _cx: &App) -> bool {
        false
    }
}

impl Render for ExplorerPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let connections = self.connections(cx);
        let requests = self.requests(cx);
        let empty = connections.is_empty() && requests.is_empty();
        let muted = cx.theme().muted_foreground;

        v_flex()
            .id("explorer")
            .size_full()
            .gap_2()
            .p_2()
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("add-connection")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Plus)
                            .label("Connection")
                            .tooltip("Add a connection")
                            .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                                cx.emit(ExplorerEvent::NewConnection);
                            })),
                    )
                    .child(
                        Button::new("add-request")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Plus)
                            .label("Request")
                            .tooltip("New request")
                            .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                                cx.emit(ExplorerEvent::NewRequest);
                            })),
                    ),
            )
            .child(
                div()
                    .id("explorer-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        Sidebar::new("explorer-tree")
                            .w(relative(1.))
                            .border_0()
                            .child(
                                SidebarGroup::new("Connections")
                                    .child(SidebarMenu::new().children(connections)),
                            )
                            .child(
                                SidebarGroup::new("Requests")
                                    .child(SidebarMenu::new().children(requests)),
                            ),
                    )
                    .when(empty, |this| {
                        this.child(
                            div()
                                .p_2()
                                .text_xs()
                                .text_color(muted)
                                .child("Add a connection to browse topics."),
                        )
                    }),
            )
    }
}
