//! The window root: title bar, body, status bar.
//!
//! The body arrangement is chosen by [`LayoutMode`]. Every surface is a
//! `gpui_component::dock::Panel` in both modes, so `Fixed` and `Docked` host the
//! *same* entities and switching between them is a setting rather than a rewrite.

use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, Styled as _, Subscription, Window, div, px,
};
use gpui_component::dock::{DockArea, DockItem, DockPlacement};
use gpui_component::menu::DropdownMenu as _;
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Root, Sizable as _, StyledExt as _, TitleBar,
    button::{Button, ButtonVariants as _},
    h_flex,
    resizable::{h_resizable, resizable_panel, v_resizable},
    status_bar::StatusBar,
    tab::{Tab, TabBar},
    v_flex,
};

use crate::actions::{
    Connect, Disconnect, NewConnection, NewRequest, ResetLayout, SetTheme, ToggleConsole,
    ToggleSidebar,
};
use crate::panels::{CollectionsEvent, CollectionsPanel, ConsolePanel, RequestPanel};
use crate::prefs::Prefs;
use crate::session::{RobotWhisperer, Sessions, Status};
use crate::theme::{self, Preference};
use crate::tokens;
use crate::workspace::Workspace;

/// Bumped when the default dock arrangement changes, so stale saved layouts get
/// rebuilt rather than loaded into a shape that no longer exists.
const LAYOUT_VERSION: usize = 2;

/// How the body hosts its panels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    /// Postman-shaped: sidebar, request tabs, optional console. Predictable.
    Fixed,
    /// Everything draggable and tabbable through `DockArea`.
    Docked,
}

pub struct WorkspaceView {
    workspace: Entity<Workspace>,
    sessions: Entity<Sessions>,
    collections: Entity<CollectionsPanel>,
    console: Entity<ConsolePanel>,
    /// Open request editors, in tab order.
    open: Vec<Entity<RequestPanel>>,
    active: usize,
    layout: LayoutMode,
    console_open: bool,
    sidebar_open: bool,
    /// Built lazily, and only in `Docked` mode.
    dock: Option<Entity<DockArea>>,
    prefs: Prefs,
    _subscriptions: Vec<Subscription>,
}

impl WorkspaceView {
    pub fn new(prefs: Prefs, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let global = RobotWhisperer::global(cx);
        let workspace = global.workspace.clone();
        let sessions = global.sessions.clone();

        let collections = CollectionsPanel::view(workspace.clone(), window, cx);
        let console = ConsolePanel::view(window, cx);

        let subscriptions = vec![
            cx.observe(&workspace, |_, _, cx| cx.notify()),
            cx.observe(&sessions, |_, _, cx| cx.notify()),
            cx.subscribe_in(&collections, window, Self::on_collections_event),
        ];

        workspace
            .update(cx, |workspace, cx| workspace.load(cx))
            .detach();

        Self {
            workspace,
            sessions,
            collections,
            console,
            open: Vec::new(),
            active: 0,
            layout: LayoutMode::Fixed,
            console_open: false,
            sidebar_open: true,
            dock: None,
            prefs,
            _subscriptions: subscriptions,
        }
    }

    // ── request tabs ───────────────────────────────────────────────────────────

    fn open_request(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self
            .open
            .iter()
            .position(|panel| panel.read(cx).request_id() == Some(id))
        {
            self.activate(index, cx);
            return;
        }

        let Some(request) = self.workspace.read(cx).request(id).cloned() else {
            return;
        };
        let panel = RequestPanel::view(&request, window, cx);
        self.open.push(panel);
        self.activate(self.open.len() - 1, cx);
        self.sync_dock(window, cx);
    }

    /// Selects an open tab, and highlights its request in the sidebar.
    fn activate(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.open.len() {
            return;
        }
        self.active = index;
        let selected = self.open[index].read(cx).request_id();
        self.collections
            .update(cx, |panel, cx| panel.select(selected, cx));
        cx.notify();
    }

    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.open.len() {
            return;
        }
        self.open.remove(index);
        self.activate(self.active.min(self.open.len().saturating_sub(1)), cx);
        self.sync_dock(window, cx);
        cx.notify();
    }

    fn on_collections_event(
        &mut self,
        _panel: &Entity<CollectionsPanel>,
        event: &CollectionsEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            CollectionsEvent::Open(id) => self.open_request(*id, window, cx),
            CollectionsEvent::New => self.new_request(window, cx),
            CollectionsEvent::Duplicate(id) => self.duplicate_request(*id, window, cx),
            CollectionsEvent::Delete(id) => {
                if let Some(index) = self
                    .open
                    .iter()
                    .position(|panel| panel.read(cx).request_id() == Some(*id))
                {
                    self.close_tab(index, window, cx);
                }
                self.workspace
                    .update(cx, |workspace, cx| workspace.delete_request(*id, cx))
                    .detach();
            }
        }
    }

    fn new_request(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let environment = self.default_environment(cx);
        let creating = self
            .workspace
            .update(cx, |workspace, cx| workspace.create_request(cx));

        cx.spawn_in(window, async move |view, window| {
            let Some(mut request) = creating.await else {
                return;
            };
            window
                .update(|window, cx| {
                    view.update(cx, |view, cx| {
                        if let Some(id) = environment {
                            request.connection_id = Some(id);
                            view.workspace
                                .update(cx, |workspace, cx| {
                                    workspace.save_request(request.clone(), cx)
                                })
                                .detach();
                        }
                        view.open_request(request.id, window, cx)
                    })
                    .ok();
                })
                .ok();
        })
        .detach();
    }

    /// The environment a new request should start out pointing at: whichever one
    /// is connected, else the only one there is. Leaving it unset when the choice
    /// is obvious just makes the first send fail.
    fn default_environment(&self, cx: &App) -> Option<i64> {
        let connections = self.workspace.read(cx).connections();
        let sessions = self.sessions.read(cx);
        connections
            .iter()
            .find(|connection| sessions.status(connection.id).is_connected())
            .or_else(|| connections.first())
            .map(|connection| connection.id)
    }

    fn duplicate_request(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let duplicating = self
            .workspace
            .update(cx, |workspace, cx| workspace.duplicate_request(id, cx));

        cx.spawn_in(window, async move |view, window| {
            let Some(request) = duplicating.await else {
                return;
            };
            window
                .update(|window, cx| {
                    view.update(cx, |view, cx| view.open_request(request.id, window, cx))
                        .ok();
                })
                .ok();
        })
        .detach();
    }

    // ── layout ─────────────────────────────────────────────────────────────────

    /// Keeps the dock's contents in step with `open` while in `Docked` mode. A
    /// no-op in `Fixed` mode, which is what makes switching cheap.
    fn sync_dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.layout != LayoutMode::Docked {
            return;
        }
        let dock = self.ensure_dock(window, cx);
        let panels: Vec<Arc<dyn gpui_component::dock::PanelView>> = self
            .open
            .iter()
            .map(|panel| Arc::new(panel.clone()) as Arc<dyn gpui_component::dock::PanelView>)
            .collect();

        dock.update(cx, |dock, cx| {
            let weak = cx.entity().downgrade();
            let centre = DockItem::tabs(panels, &weak, window, cx);
            dock.set_center(centre, window, cx);
        });
    }

    fn ensure_dock(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<DockArea> {
        if let Some(dock) = &self.dock {
            return dock.clone();
        }

        let dock = cx.new(|cx| DockArea::new("workspace", Some(LAYOUT_VERSION), window, cx));
        let weak = dock.downgrade();
        let left = DockItem::tab(self.collections.clone(), &weak, window, cx);
        let bottom = DockItem::tab(self.console.clone(), &weak, window, cx);
        let centre = DockItem::tabs(vec![], &weak, window, cx);

        dock.update(cx, |area, cx| {
            area.set_version(LAYOUT_VERSION, window, cx);
            area.set_center(centre, window, cx);
            area.set_left_dock(left, Some(px(280.)), true, window, cx);
            area.set_bottom_dock(bottom, Some(px(180.)), false, window, cx);
        });

        self.dock = Some(dock.clone());
        dock
    }

    fn body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.layout {
            LayoutMode::Docked => {
                let dock = self.ensure_dock(window, cx);
                self.sync_dock(window, cx);
                dock.into_any_element()
            }
            LayoutMode::Fixed => self.fixed_body(cx),
        }
    }

    fn fixed_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let editor = self.editor_area(cx);
        let console = self.console.clone();
        let console_open = self.console_open;

        let centre = if console_open {
            v_resizable("editor-console")
                .child(resizable_panel().child(editor))
                .child(
                    resizable_panel()
                        .size(px(180.))
                        .size_range(px(100.)..px(420.))
                        .child(console),
                )
                .into_any_element()
        } else {
            editor
        };

        if !self.sidebar_open {
            return centre;
        }

        h_resizable("shell")
            .child(
                resizable_panel()
                    .size(px(280.))
                    .size_range(px(220.)..px(420.))
                    .child(self.collections.clone()),
            )
            .child(resizable_panel().child(centre))
            .into_any_element()
    }

    /// Request tabs plus the active editor.
    fn editor_area(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.open.is_empty() {
            return self.welcome(cx);
        }

        let active = self.active;
        let tabs: Vec<_> = self
            .open
            .iter()
            .enumerate()
            .map(|(index, panel)| {
                let request = panel.read(cx);
                Tab::new()
                    .label(request.title())
                    .prefix(tokens::status_dot(tokens::kind_color(request.kind(), cx)))
                    .suffix(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .when(request.dirty(), |row| {
                                row.child(div().size(px(5.)).rounded_full().bg(cx.theme().warning))
                            })
                            .child(
                                Button::new(("close-tab", index))
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Close)
                                    .tooltip("Close request")
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, window, cx| {
                                            this.close_tab(index, window, cx)
                                        },
                                    )),
                            ),
                    )
            })
            .collect();

        v_flex()
            .size_full()
            .min_w_0()
            .bg(cx.theme().background)
            .child(
                TabBar::new("request-tabs")
                    .selected_index(active)
                    .children(tabs)
                    .on_click(cx.listener(|this, index: &usize, _, cx| this.activate(*index, cx))),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .children(self.open.get(active).cloned()),
            )
            .into_any_element()
    }

    fn welcome(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_5()
            .bg(cx.theme().background)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size_16()
                    .rounded(cx.theme().radius_lg)
                    .bg(cx.theme().secondary)
                    .text_color(cx.theme().primary)
                    .child(Icon::new(IconName::Bot).size_8()),
            )
            .child(
                v_flex()
                    .gap_1p5()
                    .items_center()
                    .child(
                        div()
                            .text_2xl()
                            .font_semibold()
                            .text_color(cx.theme().foreground)
                            .child("Robot Whisperer"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Save a request, point it at an environment, and send it."),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("welcome-new-request")
                            .primary()
                            .icon(IconName::Plus)
                            .label("New request")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.new_request(window, cx);
                            })),
                    )
                    .child(
                        Button::new("welcome-add-environment")
                            .outline()
                            .icon(IconName::Globe)
                            .label("Add environment")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.add_dummy(cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    // ── environments ───────────────────────────────────────────────────────────

    /// Adds the Dummy environment and connects it.
    ///
    /// Adding an environment and then finding every request refuses to run is a
    /// dead end, so creating one connects it. Dummy is local and synthetic, so
    /// there is nothing to ask permission for.
    fn add_dummy(&mut self, cx: &mut Context<Self>) {
        let creating = self
            .workspace
            .update(cx, |workspace, cx| workspace.create_dummy_connection(cx));

        cx.spawn(async move |view, cx| {
            let Some(connection) = creating.await else {
                return;
            };
            view.update(cx, |view, cx| {
                view.sessions
                    .update(cx, |sessions, cx| sessions.connect(&connection, cx))
                    .detach();
            })
            .ok();
        })
        .detach();
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

    /// The environment pill: a status dot, the active environment's name, and a
    /// menu to connect, disconnect or add one.
    fn environment_pill(&self, cx: &mut Context<Self>) -> AnyElement {
        let workspace = self.workspace.read(cx);
        let sessions = self.sessions.read(cx);

        let (label, colour) = workspace
            .connections()
            .iter()
            .map(|connection| (connection.name.clone(), sessions.status(connection.id)))
            .find(|(_, status)| status.is_connected())
            .map(|(name, _)| (name, cx.theme().success))
            .unwrap_or_else(|| match workspace.connections().first() {
                Some(connection) => (
                    connection.name.clone(),
                    status_colour(&sessions.status(connection.id), cx),
                ),
                None => ("No environment".to_string(), cx.theme().muted_foreground),
            });

        let entries: Vec<_> = workspace
            .connections()
            .iter()
            .map(|connection| {
                (
                    connection.id,
                    connection.name.clone(),
                    sessions.status(connection.id).is_connected(),
                )
            })
            .collect();

        Button::new("environment")
            .ghost()
            .small()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(tokens::status_dot(colour))
                    .child(div().text_sm().child(label))
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .xsmall()
                            .text_color(cx.theme().muted_foreground),
                    ),
            )
            .dropdown_menu(move |mut menu, _window, _cx| {
                if entries.is_empty() {
                    menu = menu.menu("Add Dummy environment", Box::new(NewConnection));
                    return menu;
                }
                for (id, name, connected) in &entries {
                    let action: Box<dyn gpui::Action> = if *connected {
                        Box::new(Disconnect(*id))
                    } else {
                        Box::new(Connect(*id))
                    };
                    menu = menu.menu_with_check(
                        format!("{name}{}", if *connected { "" } else { "  — disconnected" }),
                        *connected,
                        action,
                    );
                }
                menu.separator()
                    .menu("Add Dummy environment", Box::new(NewConnection))
            })
            .into_any_element()
    }

    fn theme_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = theme::current(cx);
        let following = self.prefs.theme() == Preference::System;

        Button::new("theme")
            .ghost()
            .small()
            .icon(IconName::Palette)
            .tooltip(format!("Theme: {current}"))
            .dropdown_menu(move |mut menu, _window, cx| {
                menu = menu
                    .menu_with_check(
                        "Match system",
                        following,
                        Box::new(SetTheme("system".into())),
                    )
                    .separator();
                let active = theme::current(cx);
                for name in theme::names() {
                    let selected = !following && active == name;
                    menu = menu.menu_with_check(
                        name.clone(),
                        selected,
                        Box::new(SetTheme(name.into())),
                    );
                }
                menu
            })
            .into_any_element()
    }

    // ── actions ────────────────────────────────────────────────────────────────

    fn on_new_request(&mut self, _: &NewRequest, window: &mut Window, cx: &mut Context<Self>) {
        self.new_request(window, cx);
    }

    fn on_new_connection(&mut self, _: &NewConnection, _: &mut Window, cx: &mut Context<Self>) {
        self.add_dummy(cx);
    }

    fn on_connect(&mut self, action: &Connect, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_connection(action.0, cx);
    }

    fn on_disconnect(&mut self, action: &Disconnect, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_connection(action.0, cx);
    }

    fn on_set_theme(&mut self, action: &SetTheme, _: &mut Window, cx: &mut Context<Self>) {
        let preference = Preference::parse(&action.0);
        self.prefs.set_theme(&preference);
        theme::apply(&preference, cx);
        cx.refresh_windows();
        cx.notify();
    }

    fn on_toggle_sidebar(
        &mut self,
        _: &ToggleSidebar,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.layout {
            LayoutMode::Fixed => self.sidebar_open = !self.sidebar_open,
            LayoutMode::Docked => {
                let dock = self.ensure_dock(window, cx);
                dock.update(cx, |dock, cx| {
                    dock.toggle_dock(DockPlacement::Left, window, cx)
                });
            }
        }
        cx.notify();
    }

    fn on_toggle_console(
        &mut self,
        _: &ToggleConsole,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.layout {
            LayoutMode::Fixed => self.console_open = !self.console_open,
            LayoutMode::Docked => {
                let dock = self.ensure_dock(window, cx);
                dock.update(cx, |dock, cx| {
                    dock.toggle_dock(DockPlacement::Bottom, window, cx)
                });
            }
        }
        cx.notify();
    }

    /// Flips between the fixed and docked shells. Both host the same panels, so
    /// this is a re-render, not a migration.
    fn on_reset_layout(&mut self, _: &ResetLayout, window: &mut Window, cx: &mut Context<Self>) {
        self.layout = match self.layout {
            LayoutMode::Fixed => LayoutMode::Docked,
            LayoutMode::Docked => LayoutMode::Fixed,
        };
        self.sync_dock(window, cx);
        cx.notify();
    }

    fn status_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let workspace = self.workspace.read(cx);
        let connected = self.sessions.read(cx).connected_count();
        let environments = workspace.connections().len();
        let requests = workspace.requests().len();
        let mode = match self.layout {
            LayoutMode::Fixed => "Fixed",
            LayoutMode::Docked => "Docked",
        };

        StatusBar::new()
            .left(
                Button::new("toggle-sidebar")
                    .ghost()
                    .xsmall()
                    .icon(IconName::PanelLeft)
                    .tooltip("Toggle sidebar")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.on_toggle_sidebar(&ToggleSidebar, window, cx);
                    })),
            )
            .left(
                Button::new("toggle-console")
                    .ghost()
                    .xsmall()
                    .icon(IconName::PanelBottom)
                    .tooltip("Toggle console")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.on_toggle_console(&ToggleConsole, window, cx);
                    })),
            )
            .child(tokens::meta("Requests", requests.to_string(), cx))
            .child(tokens::meta(
                "Connected",
                format!("{connected}/{environments}"),
                cx,
            ))
            .right(
                Button::new("layout-mode")
                    .ghost()
                    .xsmall()
                    .label(mode)
                    .tooltip("Switch between the fixed and docked layouts")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.on_reset_layout(&ResetLayout, window, cx);
                    })),
            )
            .right(format!("v{}", env!("CARGO_PKG_VERSION")))
            .into_any_element()
    }
}

fn status_colour(status: &Status, cx: &App) -> gpui::Hsla {
    match status {
        Status::Connected => cx.theme().success,
        Status::Connecting | Status::Reconnecting => cx.theme().warning,
        Status::Failed(_) => cx.theme().danger,
        Status::Disconnected => cx.theme().muted_foreground,
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sheet_layer = Root::render_sheet_layer(window, cx);
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);

        let environment = self.environment_pill(cx);
        let themes = self.theme_menu(cx);
        let body = self.body(window, cx);
        let status_bar = self.status_bar(cx);

        div()
            .id("workspace")
            .relative()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(Self::on_new_request))
            .on_action(cx.listener(Self::on_new_connection))
            .on_action(cx.listener(Self::on_connect))
            .on_action(cx.listener(Self::on_disconnect))
            .on_action(cx.listener(Self::on_set_theme))
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .on_action(cx.listener(Self::on_toggle_console))
            .on_action(cx.listener(Self::on_reset_layout))
            .child(
                v_flex()
                    .size_full()
                    .child(
                        TitleBar::new().child(
                            h_flex()
                                .w_full()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_semibold()
                                        .text_color(cx.theme().foreground)
                                        .child("Robot Whisperer"),
                                )
                                .child(h_flex().gap_1().child(environment).child(themes)),
                        ),
                    )
                    .child(div().flex_1().min_h_0().child(body))
                    .child(status_bar),
            )
            .child(div().absolute().top_8().children(notification_layer))
            .children(dialog_layer)
            .children(sheet_layer)
    }
}
