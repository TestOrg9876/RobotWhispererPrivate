//! The window root: title bar, dock, status bar.
//!
//! The body is a `gpui_component::dock::DockArea` and nothing else. Every
//! surface — the request list, request editors, the console — is a `Panel`
//! inside it, which is what gives tabs that drag, reorder, split and restore
//! without this file implementing any of it.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _, Subscription,
    Window, div, px,
};
use gpui_component::dock::{DockArea, DockItem, DockPlacement, PanelView};
use gpui_component::menu::DropdownMenu as _;
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Root, Sizable as _, TitleBar, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    status_bar::StatusBar,
    v_flex,
};

use crate::actions::{
    CommandPalette, Connect, Disconnect, ManageConnections, NewRequest, OpenSettings,
    ToggleConsole, ToggleSidebar,
};
use crate::palette::{Choice, Entry};
use crate::panels::{
    CollectionsEvent, CollectionsPanel, ConnectionsPanel, ConsolePanel, PaletteEvent, PaletteView,
    RequestPanel, SettingsEvent, SettingsView, WelcomeEvent, WelcomePanel,
};
use crate::prefs::Prefs;
use crate::session::{RobotWhisperer, Sessions, Status};
use crate::theme::{self, Preference};
use crate::tokens;
use crate::workspace::Workspace;

/// Bumped when the default dock arrangement changes, so stale saved layouts get
/// rebuilt rather than loaded into a shape that no longer exists.
const LAYOUT_VERSION: usize = 3;

pub struct WorkspaceView {
    /// Held so the shell is always somewhere in the focus chain.
    ///
    /// Actions dispatch from the focused element upwards. With nothing focused
    /// — which is the state right after the window opens, and the permanent
    /// state under a bare X server with no window manager — a keyboard shortcut
    /// bound here never reaches its handler.
    focus_handle: FocusHandle,
    workspace: Entity<Workspace>,
    sessions: Entity<Sessions>,
    dock: Entity<DockArea>,
    collections: Entity<CollectionsPanel>,
    /// Request editors currently open, by request id. The dock owns their order
    /// and which is active; this only answers "is it already open".
    open: HashMap<i64, Entity<RequestPanel>>,
    /// Built the first time connections are opened, and kept so reopening the
    /// dock shows the same panel rather than a fresh one mid-edit.
    connections: Option<Entity<ConnectionsPanel>>,
    /// Shown in the centre while nothing else is, and taken out when the first
    /// request arrives. A panel rather than a special case in `render`, so the
    /// dock stays the only thing that decides what the centre looks like.
    welcome: Option<Entity<WelcomePanel>>,
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

        let dock = cx.new(|cx| DockArea::new("workspace", Some(LAYOUT_VERSION), window, cx));
        let weak = dock.downgrade();
        let left = DockItem::tab(collections.clone(), &weak, window, cx);
        let bottom = DockItem::tab(console.clone(), &weak, window, cx);
        let welcome = WelcomePanel::view(cx);
        let centre = DockItem::tabs(
            vec![Arc::new(welcome.clone()) as Arc<dyn PanelView>],
            &weak,
            window,
            cx,
        );
        dock.update(cx, |area, cx| {
            area.set_center(centre, window, cx);
            area.set_left_dock(left, Some(px(280.)), true, window, cx);
            area.set_bottom_dock(bottom, Some(px(180.)), false, window, cx);
        });

        let subscriptions = vec![
            cx.observe(&workspace, |_, _, cx| cx.notify()),
            cx.observe(&sessions, |_, _, cx| cx.notify()),
            cx.subscribe_in(&collections, window, Self::on_collections_event),
            cx.subscribe_in(&welcome, window, Self::on_welcome_event),
        ];

        workspace
            .update(cx, |workspace, cx| workspace.load(cx))
            .detach();

        Self {
            focus_handle: cx.focus_handle(),
            workspace,
            sessions,
            dock,
            collections,
            open: HashMap::new(),
            connections: None,
            welcome: Some(welcome),
            prefs,
            _subscriptions: subscriptions,
        }
    }

    // ── requests ───────────────────────────────────────────────────────────────

    fn open_request(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(request) = self.workspace.read(cx).request(id).cloned() else {
            return;
        };

        // Re-opening an already-open request has to bring its tab to the front,
        // and `TabPanel::add_panel` returns early for a panel it already holds
        // rather than activating it. Taking it out and putting it back is the
        // only way to do that through the dock's public API.
        if let Some(existing) = self.open.get(&id).cloned() {
            let panel = Arc::new(existing) as Arc<dyn PanelView>;
            self.dock.update(cx, |dock, cx| {
                dock.remove_panel(panel.clone(), DockPlacement::Center, window, cx);
                dock.add_panel(panel, DockPlacement::Center, None, window, cx);
            });
        } else {
            let panel = RequestPanel::view(&request, window, cx);
            self.open.insert(id, panel.clone());
            self.dock.update(cx, |dock, cx| {
                dock.add_panel(
                    Arc::new(panel) as Arc<dyn PanelView>,
                    DockPlacement::Center,
                    None,
                    window,
                    cx,
                )
            });
            self.dismiss_welcome(window, cx);
        }

        self.collections
            .update(cx, |panel, cx| panel.select(Some(id), cx));
        cx.notify();
    }

    fn close_request(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.open.remove(&id) else {
            return;
        };
        self.dock.update(cx, |dock, cx| {
            dock.remove_panel(
                Arc::new(panel) as Arc<dyn PanelView>,
                DockPlacement::Center,
                window,
                cx,
            )
        });
        cx.notify();
    }

    /// Takes the welcome panel out once there is something real to look at.
    fn dismiss_welcome(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(welcome) = self.welcome.take() else {
            return;
        };
        self.dock.update(cx, |dock, cx| {
            dock.remove_panel(
                Arc::new(welcome) as Arc<dyn PanelView>,
                DockPlacement::Center,
                window,
                cx,
            )
        });
    }

    fn on_welcome_event(
        &mut self,
        _panel: &Entity<WelcomePanel>,
        event: &WelcomeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            WelcomeEvent::NewRequest => self.new_request(window, cx),
            WelcomeEvent::ManageConnections => self.open_connections(window, cx),
            WelcomeEvent::CommandPalette => self.open_palette(window, cx),
        }
    }

    fn new_request(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let connection = self.default_connection(cx);
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
                        if let Some(id) = connection {
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

    /// The connection a new request should start out pointing at: whichever one
    /// is connected, else the only one there is.
    fn default_connection(&self, cx: &App) -> Option<i64> {
        let connections = self.workspace.read(cx).connections();
        let sessions = self.sessions.read(cx);
        connections
            .iter()
            .find(|connection| sessions.status(connection.id).is_connected())
            .or_else(|| connections.first())
            .map(|connection| connection.id)
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
                self.close_request(*id, window, cx);
                self.workspace
                    .update(cx, |workspace, cx| workspace.delete_request(*id, cx))
                    .detach();
            }
        }
    }

    // ── connections ────────────────────────────────────────────────────────────

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

    /// The connection status in the title bar.
    ///
    /// Several ROS systems can be connected at once, so this counts them rather
    /// than naming one: which system a *request* talks to is that request's
    /// business, shown in its own bar.
    fn connections_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let workspace = self.workspace.read(cx);
        let sessions = self.sessions.read(cx);
        let total = workspace.connections().len();
        let connected = sessions.connected_count();

        let colour = if total == 0 {
            cx.theme().muted_foreground
        } else if connected == 0 {
            cx.theme().danger
        } else if connected < total {
            cx.theme().warning
        } else {
            cx.theme().success
        };

        let label = match (total, connected) {
            (0, _) => "No connections".to_string(),
            (total, connected) if connected == total => format!("{connected} connected"),
            (total, connected) => format!("{connected}/{total} connected"),
        };

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

        Button::new("connections")
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
                for (id, name, connected) in &entries {
                    let action: Box<dyn gpui::Action> = if *connected {
                        Box::new(Disconnect(*id))
                    } else {
                        Box::new(Connect(*id))
                    };
                    menu = menu.menu_with_check(name.clone(), *connected, action);
                }
                if !entries.is_empty() {
                    menu = menu.separator();
                }
                menu.menu("Manage connections…", Box::new(ManageConnections))
            })
            .into_any_element()
    }

    /// Everything that is not a per-request control, behind one button.
    ///
    /// A theme picker permanently occupying the title bar is a setting wearing a
    /// toolbar button's clothes; it lives in Settings with the other ones.
    fn app_menu(&self) -> AnyElement {
        Button::new("app-menu")
            .ghost()
            .small()
            .icon(IconName::Ellipsis)
            .tooltip("Menu")
            .dropdown_menu(move |menu, _window, _cx| {
                menu.menu("Command palette…", Box::new(CommandPalette))
                    .menu("New request", Box::new(NewRequest))
                    .menu("Manage connections…", Box::new(ManageConnections))
                    .separator()
                    .menu("Toggle request list", Box::new(ToggleSidebar))
                    .menu("Toggle console", Box::new(ToggleConsole))
                    .separator()
                    .menu("Settings…", Box::new(OpenSettings))
            })
            .into_any_element()
    }

    // ── dialogs ────────────────────────────────────────────────────────────────

    /// Shows the connections panel, docked to the right.
    ///
    /// A panel rather than a dialog: connecting to several ROS systems is
    /// something you do *while* working, watching them come up, not a modal
    /// errand that blocks the window until it is dismissed.
    fn open_connections(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panel = self
            .connections
            .get_or_insert_with(|| ConnectionsPanel::view(window, cx))
            .clone();

        self.dock.update(cx, |dock, cx| {
            if dock.has_dock(DockPlacement::Right) {
                if !dock.is_dock_open(DockPlacement::Right, cx) {
                    dock.toggle_dock(DockPlacement::Right, window, cx);
                }
            } else {
                // Created rather than added, so it opens at a width the form
                // actually fits in: `add_panel` would take the dock default.
                let weak = cx.entity().downgrade();
                let item = DockItem::tab(panel, &weak, window, cx);
                dock.set_right_dock(item, Some(px(340.)), true, window, cx);
            }
        });
        cx.notify();
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = SettingsView::view(&self.prefs, cx);

        // The theme applies as it is chosen rather than on a Save button: it is
        // a preview you are looking at, and nobody wants to guess.
        cx.subscribe(&settings, |this, _, event: &SettingsEvent, cx| {
            let SettingsEvent::ThemeChosen(preference) = event;
            this.apply_theme(preference.clone(), cx);
        })
        .detach();

        window.open_dialog(cx, move |dialog, _window, _cx| {
            dialog.title("Settings").w(px(460.)).child(settings.clone())
        });
        cx.notify();
    }

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let palette = PaletteView::view(self.palette_entries(cx), window, cx);
        palette.update(cx, |palette, cx| palette.focus(window, cx));

        cx.subscribe_in(
            &palette,
            window,
            |this, _, event: &PaletteEvent, window, cx| {
                window.close_dialog(cx);
                if let PaletteEvent::Chose(choice) = event {
                    this.run_choice(choice.clone(), window, cx);
                }
            },
        )
        .detach();

        window.open_dialog(cx, move |dialog, _window, _cx| {
            dialog.title("Go to").w(px(620.)).child(palette.clone())
        });
        cx.notify();
    }

    /// Everything the palette can reach. Commands first: with nothing typed the
    /// palette should read as a menu of what this app does.
    fn palette_entries(&self, cx: &App) -> Vec<Entry> {
        let mut entries = vec![
            Entry::new("Command", "New request", Choice::Command("NewRequest")),
            Entry::new(
                "Command",
                "Manage connections",
                Choice::Command("ManageConnections"),
            ),
            Entry::new("Command", "Settings", Choice::Command("OpenSettings")),
            Entry::new(
                "Command",
                "Toggle request list",
                Choice::Command("ToggleSidebar"),
            ),
            Entry::new(
                "Command",
                "Toggle console",
                Choice::Command("ToggleConsole"),
            ),
        ];

        let workspace = self.workspace.read(cx);
        entries.extend(workspace.requests().iter().map(|request| {
            Entry::new("Request", request.name.clone(), Choice::Request(request.id))
                .detail(request.target.clone())
        }));

        let sessions = self.sessions.read(cx);
        entries.extend(workspace.connections().iter().map(|connection| {
            let connected = sessions.status(connection.id).is_connected();
            Entry::new(
                "Connection",
                connection.name.clone(),
                Choice::Connection(connection.id),
            )
            .detail(if connected {
                "connected"
            } else {
                "disconnected"
            })
        }));

        entries
    }

    fn run_choice(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<Self>) {
        match choice {
            Choice::Request(id) => self.open_request(id, window, cx),
            Choice::Connection(id) => self.toggle_connection(id, cx),
            Choice::Command("NewRequest") => self.new_request(window, cx),
            Choice::Command("ManageConnections") => self.open_connections(window, cx),
            Choice::Command("OpenSettings") => self.open_settings(window, cx),
            Choice::Command("ToggleSidebar") => self.on_toggle_sidebar(&ToggleSidebar, window, cx),
            Choice::Command("ToggleConsole") => self.on_toggle_console(&ToggleConsole, window, cx),
            Choice::Command(unknown) => tracing::warn!("palette has no handler for {unknown}"),
        }
    }

    fn apply_theme(&mut self, preference: Preference, cx: &mut Context<Self>) {
        self.prefs.set_theme(&preference);
        theme::apply(&preference, cx);
        cx.refresh_windows();
        cx.notify();
    }

    // ── actions ────────────────────────────────────────────────────────────────

    fn on_new_request(&mut self, _: &NewRequest, window: &mut Window, cx: &mut Context<Self>) {
        self.new_request(window, cx);
    }

    fn on_command_palette(
        &mut self,
        _: &CommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_palette(window, cx);
    }

    fn on_manage_connections(
        &mut self,
        _: &ManageConnections,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_connections(window, cx);
    }

    fn on_open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(window, cx);
    }

    fn on_connect(&mut self, action: &Connect, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_connection(action.0, cx);
    }

    fn on_disconnect(&mut self, action: &Disconnect, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_connection(action.0, cx);
    }

    fn on_toggle_sidebar(
        &mut self,
        _: &ToggleSidebar,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dock.update(cx, |dock, cx| {
            dock.toggle_dock(DockPlacement::Left, window, cx)
        });
        cx.notify();
    }

    fn on_toggle_console(
        &mut self,
        _: &ToggleConsole,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dock.update(cx, |dock, cx| {
            dock.toggle_dock(DockPlacement::Bottom, window, cx)
        });
        cx.notify();
    }

    fn status_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let workspace = self.workspace.read(cx);
        let requests = workspace.requests().len();

        StatusBar::new()
            .left(
                Button::new("toggle-sidebar")
                    .ghost()
                    .xsmall()
                    .icon(IconName::PanelLeft)
                    .tooltip("Toggle request list")
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
            // A storage failure used to leave the sidebar empty with no
            // explanation, which looks exactly like a click that did nothing.
            .when_some(workspace.error().map(str::to_owned), |bar, error| {
                bar.child(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .max_w(px(520.))
                        .child(tokens::status_dot(cx.theme().danger))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().danger)
                                .truncate()
                                .child(error),
                        ),
                )
            })
            .right(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(concat!("v", env!("CARGO_PKG_VERSION"))),
            )
            .into_any_element()
    }
}

/// The colour standing for a connection's state.
pub fn status_colour(status: &Status, cx: &App) -> gpui::Hsla {
    match status {
        Status::Connected => cx.theme().success,
        Status::Connecting | Status::Reconnecting => cx.theme().warning,
        Status::Failed(_) => cx.theme().danger,
        Status::Disconnected => cx.theme().muted_foreground,
    }
}

impl Focusable for WorkspaceView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let connections = self.connections_button(cx);
        let menu = self.app_menu();
        let status_bar = self.status_bar(cx);
        // Dialogs and notifications live on `Root` but are placed by the view:
        // `Root::render` draws neither, so a dialog opened without these is
        // stored and never seen.
        let dialog_layer = Root::render_dialog_layer(_window, cx);
        let notification_layer = Root::render_notification_layer(_window, cx);

        v_flex()
            .size_full()
            .track_focus(&self.focus_handle)
            .key_context("Workspace")
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(Self::on_command_palette))
            .on_action(cx.listener(Self::on_new_request))
            .on_action(cx.listener(Self::on_manage_connections))
            .on_action(cx.listener(Self::on_open_settings))
            .on_action(cx.listener(Self::on_connect))
            .on_action(cx.listener(Self::on_disconnect))
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .on_action(cx.listener(Self::on_toggle_console))
            .child(
                TitleBar::new().child(div().flex_1()).child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(connections)
                        .child(menu),
                ),
            )
            .child(div().flex_1().min_h_0().child(self.dock.clone()))
            .child(status_bar)
            .children(dialog_layer)
            .children(notification_layer)
    }
}
