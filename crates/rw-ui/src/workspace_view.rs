//! The window root: title bar, dock, status bar.
//!
//! The body is a `gpui_component::dock::DockArea` and nothing else. Every
//! surface — the request list, request editors, the console — is a `Panel`
//! inside it, which is what gives tabs that drag, reorder, split and restore
//! without this file implementing any of it.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Window, div, px,
};
use gpui_component::dock::{
    DockArea, DockAreaState, DockEvent, DockItem, DockPlacement, PanelState, PanelView,
};
use gpui_component::menu::DropdownMenu as _;
use gpui_component::{
    ActiveTheme as _, IconName, Root, Sizable as _, TitleBar, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    status_bar::StatusBar,
    v_flex,
};

use crate::actions::{
    AddWorldLayer, AddWorldRobot, CommandPalette, Connect, Disconnect, ExportWorkspace,
    ImportWorkspace, ManageConnections, NewDashboard, NewRequest, OpenRecording, OpenSettings,
    PickPaneTopic, PickWorldTopic, RemoveWorldLayer, ReplayRecording, ResetWorldView,
    SaveRecording, SetReplaySpeed, SetWorldAnchor, SetWorldFrame, ShowWorld, ToggleConsole,
    ToggleRecording, ToggleSidebar,
};
use crate::docking::{self, Restored};
use crate::layout;
use crate::palette::{Choice, Entry};
use crate::panels::{
    CollectionsEvent, CollectionsPanel, ConnectionsPanel, ConsolePanel, DashboardPanel,
    PaletteEvent, PaletteView, RequestPanel, SettingsEvent, SettingsView, VizPanel, WelcomeEvent,
    WelcomePanel, WorldPanel,
};
use crate::prefs::Prefs;
use crate::session::{Notice, RobotWhisperer, SessionEvent, Sessions, Status};
use crate::theme::{self, Preference};
use crate::tokens;
use crate::workspace::Workspace;

/// Bumped when the default dock arrangement changes, so stale saved layouts get
/// rebuilt rather than loaded into a shape that no longer exists.
const LAYOUT_VERSION: usize = 5;

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
    /// Kept apart from the rest because restoring a layout replaces the welcome
    /// panel with a rebuilt one, and the old subscription has to go with it.
    _welcome: Option<Subscription>,
    /// The 3D world, if it has been opened.
    world: Option<Entity<WorldPanel>>,
    /// Dashboards currently open, by id.
    dashboards: HashMap<i64, Entity<DashboardPanel>>,
    prefs: Prefs,
    /// The play bar, shown only while a recording is open.
    transport: Entity<crate::transport_bar::TransportBar>,
    _subscriptions: Vec<Subscription>,
}

/// Raises a toast for the notices worth interrupting someone for.
///
/// Two kinds qualify. **Failures**, because something you did did not work and
/// the console is not where you are looking. And **connection lifecycle**,
/// because a robot dropping while you are watching a dashboard is otherwise a
/// colour change in the footer at the other end of the window.
///
/// Nothing else. In particular not [`Notice::Robot`]: `/rosout` comes through
/// this same bus, and a node logging a warning at ten hertz would bury the
/// screen. The console still keeps every one of them, which is what a console
/// is for.
fn toast(notice: &Notice, window: &mut Window, cx: &mut App) {
    use gpui_component::notification::Notification;

    // Bottom right, not the default top right: the right dock's own header
    // lives up there, and a toast that covers the thing it is telling you about
    // is a toast in the wrong place.
    let placement = gpui::Anchor::BottomRight;
    let note = match notice {
        // Keyed on the connection, so a flapping link replaces its own toast
        // rather than stacking one per attempt.
        Notice::Link {
            connection,
            text,
            status,
        } => {
            // Not every state change is worth a toast. Connecting and connected
            // are the expected answer to a button that was just pressed, and
            // the connection's own row turns green where the eye already is —
            // toasting them made twenty screenshots grow a box saying what had
            // plainly just happened.
            let note = match status {
                Status::Failed(_) => Notification::error(text.clone()),
                Status::Disconnected | Status::Reconnecting => Notification::warning(text.clone()),
                Status::Connecting | Status::Connected => return,
            };
            note.id1::<Notice>(gpui::SharedString::from(connection.clone()))
        }
        Notice::Error(text) => Notification::error(text.clone()),
        Notice::Info(_) | Notice::Robot { .. } => return,
    };
    window.push_notification(note.placement(placement), cx);
}

impl WorkspaceView {
    pub fn new(prefs: Prefs, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let global = RobotWhisperer::global(cx);
        let workspace = global.workspace.clone();
        let sessions = global.sessions.clone();

        let collections = CollectionsPanel::view(workspace.clone(), window, cx);
        let console = ConsolePanel::view(window, cx);
        let transport = crate::transport_bar::TransportBar::view(cx);

        let dock = cx.new(|cx| DockArea::new("workspace", Some(LAYOUT_VERSION), window, cx));
        let weak = dock.downgrade();
        let left = DockItem::tab(collections.clone(), &weak, window, cx);
        let bottom = DockItem::tab(console.clone(), &weak, window, cx);
        let welcome = WelcomePanel::view(cx);
        // Deliberately handed to `set_center` bare rather than wrapped in a
        // split. A `TabPanel` with no parent `StackPanel` reports itself
        // locked, and a locked tab strip cannot be dragged apart — which is
        // exactly right here: request editors are tabs, not a canvas. Composing
        // a custom arrangement of live views is what a dashboard is for, and a
        // dashboard has a dock of its own.
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
            // The dock reports every move, drag and resize. Writing the
            // arrangement out each time is cheap enough — it is one small tree
            // and one file — and it means a crash never loses the layout.
            cx.subscribe_in(&dock, window, |this, _, _: &DockEvent, _, cx| {
                this.save_layout(cx)
            }),
            cx.subscribe_in(
                &sessions,
                window,
                |_, _, event: &SessionEvent, window, cx| toast(&event.0, window, cx),
            ),
        ];

        // The saved arrangement names requests by id, so it cannot be rebuilt
        // until storage has said which ones still exist.
        let loaded = workspace.update(cx, |workspace, cx| workspace.load(cx));
        cx.spawn_in(window, async move |view, cx| {
            loaded.await;
            view.update_in(cx, |view, window, cx| view.restore_layout(window, cx))
                .ok();
        })
        .detach();

        Self {
            focus_handle: cx.focus_handle(),
            workspace,
            sessions,
            dock,
            collections,
            open: HashMap::new(),
            connections: None,
            welcome: Some(welcome.clone()),
            world: None,
            dashboards: HashMap::new(),
            prefs,
            transport,
            _welcome: Some(cx.subscribe_in(&welcome, window, Self::on_welcome_event)),
            _subscriptions: subscriptions,
        }
    }

    // ── layout ─────────────────────────────────────────────────────────────────

    /// Writes the centre arrangement out, so the next launch opens where this
    /// one left off.
    fn save_layout(&mut self, cx: &mut Context<Self>) {
        let centre = self.dock.read(cx).dump(cx).center;
        match serde_json::to_value(centre) {
            Ok(centre) => self.prefs.set_layout(LAYOUT_VERSION, centre),
            Err(error) => tracing::warn!("could not serialise the layout: {error}"),
        }
    }

    /// Rebuilds the saved arrangement, minus anything that no longer exists.
    fn restore_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(saved) = self.prefs.layout(LAYOUT_VERSION).cloned() else {
            return;
        };
        let Ok(saved) = serde_json::from_value::<PanelState>(saved) else {
            tracing::warn!("the saved layout is not readable; keeping the default");
            return;
        };
        let live: HashSet<i64> = self
            .workspace
            .read(cx)
            .requests()
            .iter()
            .map(|request| request.id)
            .collect();
        let Some(centre) = layout::prune(&saved, &|id| live.contains(&id)) else {
            return;
        };

        cx.set_global(Restored::default());
        let state = DockAreaState {
            version: Some(LAYOUT_VERSION),
            center: layout::flatten(centre),
            // The sidebar and console are chrome this shell just built. Leaving
            // them out means `load` keeps them rather than rebuilding panels
            // nothing is subscribed to.
            left_dock: None,
            right_dock: None,
            bottom_dock: None,
        };
        if let Err(error) = self
            .dock
            .update(cx, |dock, cx| dock.load(state, window, cx))
        {
            tracing::warn!("could not restore the layout: {error}");
            return;
        }

        let restored = std::mem::take(cx.global_mut::<Restored>());
        self.open = restored.requests.into_iter().collect();
        self.welcome = restored.welcome.clone();
        self._welcome = restored
            .welcome
            .map(|welcome| cx.subscribe_in(&welcome, window, Self::on_welcome_event));
        cx.notify();
    }

    // ── requests ───────────────────────────────────────────────────────────────

    fn open_request(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(request) = self.workspace.read(cx).request(id).cloned() else {
            return;
        };

        // Re-opening an already-open request brings its tab to the front. Asked
        // of the panel's own tab group rather than of the dock, because the
        // dock's item tree still describes the layout the shell built and knows
        // nothing about panes the user has since split off.
        if let Some(existing) = self.open.get(&id).cloned() {
            let home = existing.read(cx).home();
            let panel = Arc::new(existing) as Arc<dyn PanelView>;
            if !docking::reveal(home, panel.clone(), window, cx) {
                self.dock.update(cx, |dock, cx| {
                    dock.remove_panel(panel.clone(), DockPlacement::Center, window, cx);
                    dock.add_panel(panel, DockPlacement::Center, None, window, cx);
                });
            }
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
        let home = panel.read(cx).home();
        let panel = Arc::new(panel) as Arc<dyn PanelView>;
        if !docking::close(home, panel.clone(), window, cx) {
            self.dock.update(cx, |dock, cx| {
                dock.remove_panel(panel, DockPlacement::Center, window, cx)
            });
        }
        cx.notify();
    }

    /// Opens the 3D world, or brings it to the front if it is already open.
    ///
    /// One instance: the robot models are tens of megabytes, and a second world
    /// showing the same arm is a second copy of them.
    fn open_world(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(existing) = self.world.clone() {
            let home = existing.read(cx).home();
            let panel = Arc::new(existing) as Arc<dyn PanelView>;
            if !docking::reveal(home, panel.clone(), window, cx) {
                self.dock.update(cx, |dock, cx| {
                    dock.add_panel(panel, DockPlacement::Center, None, window, cx)
                });
            }
            return;
        }

        let panel = WorldPanel::view(cx);
        self.world = Some(panel.clone());
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
        cx.notify();
    }

    /// Takes the welcome panel out once there is something real to look at.
    fn dismiss_welcome(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(welcome) = self.welcome.take() else {
            return;
        };
        let home = welcome.read(cx).home();
        let panel = Arc::new(welcome) as Arc<dyn PanelView>;
        if !docking::close(home, panel.clone(), window, cx) {
            self.dock.update(cx, |dock, cx| {
                dock.remove_panel(panel, DockPlacement::Center, window, cx)
            });
        }
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
            CollectionsEvent::Complain(message) => self.complain(message.clone(), cx),
            CollectionsEvent::Delete(id) => {
                self.close_request(*id, window, cx);
                self.workspace
                    .update(cx, |workspace, cx| workspace.delete_request(*id, cx))
                    .detach();
            }
            CollectionsEvent::OpenDashboard(id) => self.open_dashboard(*id, window, cx),
            CollectionsEvent::NewDashboard => self.new_dashboard(window, cx),
            CollectionsEvent::DeleteDashboard(id) => {
                self.close_dashboard(*id, window, cx);
                self.workspace
                    .update(cx, |workspace, cx| workspace.delete_dashboard(*id, cx))
                    .detach();
            }
        }
    }

    // ── dashboards ─────────────────────────────────────────────────────────────

    /// Opens a dashboard, or brings it forward if it is already open.
    fn open_dashboard(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(existing) = self.dashboards.get(&id).cloned() {
            let home = existing.read(cx).home();
            let panel = Arc::new(existing) as Arc<dyn PanelView>;
            if !docking::reveal(home, panel.clone(), window, cx) {
                self.dock.update(cx, |dock, cx| {
                    dock.add_panel(panel, DockPlacement::Center, None, window, cx)
                });
            }
            return;
        }
        let Some(dashboard) = self.workspace.read(cx).dashboard(id).cloned() else {
            return;
        };
        let panel = DashboardPanel::view(&dashboard, window, cx);
        self.dashboards.insert(id, panel.clone());
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
        cx.notify();
    }

    fn new_dashboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let created = self
            .workspace
            .update(cx, |workspace, cx| workspace.create_dashboard(cx));
        cx.spawn_in(window, async move |view, cx| {
            let Some(dashboard) = created.await else {
                return;
            };
            view.update_in(cx, |view, window, cx| {
                view.open_dashboard(dashboard.id, window, cx)
            })
            .ok();
        })
        .detach();
    }

    fn close_dashboard(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.dashboards.remove(&id) else {
            return;
        };
        let home = panel.read(cx).home();
        let panel = Arc::new(panel) as Arc<dyn PanelView>;
        if !docking::close(home, panel.clone(), window, cx) {
            self.dock.update(cx, |dock, cx| {
                dock.remove_panel(panel, DockPlacement::Center, window, cx)
            });
        }
        cx.notify();
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
                    .menu("New dashboard", Box::new(NewDashboard))
                    .menu("3D world", Box::new(ShowWorld))
                    .menu("Start or stop recording", Box::new(ToggleRecording))
                    .menu("Replay last recording", Box::new(ReplayRecording))
                    .menu("Save recording…", Box::new(SaveRecording))
                    .menu("Open recording…", Box::new(OpenRecording))
                    .menu("Toggle console", Box::new(ToggleConsole))
                    .separator()
                    .menu("Import workspace…", Box::new(ImportWorkspace))
                    .menu("Export workspace…", Box::new(ExportWorkspace))
                    .separator()
                    .menu("Settings…", Box::new(OpenSettings))
            })
            .into_any_element()
    }

    // ── dialogs ────────────────────────────────────────────────────────────────

    /// Shows the connections panel, docked to the right, or hides it if it is
    /// already showing.
    ///
    /// A panel rather than a dialog: connecting to several ROS systems is
    /// something you do *while* working, watching them come up, not a modal
    /// errand that blocks the window until it is dismissed. Which is also why
    /// it toggles — a dock you can only open is a dock in the way.
    fn open_connections(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panel = self
            .connections
            .get_or_insert_with(|| ConnectionsPanel::view(window, cx))
            .clone();

        self.dock.update(cx, |dock, cx| {
            if dock.has_dock(DockPlacement::Right) {
                dock.toggle_dock(DockPlacement::Right, window, cx);
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
        let settings = SettingsView::view(&self.prefs, window, cx);

        // Everything applies as it is chosen rather than on a Save button: it is
        // a preview you are looking at, and nobody wants to guess.
        cx.subscribe(
            &settings,
            |this, _, event: &SettingsEvent, cx| match event {
                SettingsEvent::ThemeChosen(preference) => {
                    this.apply_theme(preference.clone(), cx);
                }
                SettingsEvent::Changed(settings) => {
                    this.prefs.set_settings(*settings);
                    // The global is what every consumer reads; the preferences
                    // file is only where it comes back from next launch.
                    cx.set_global(*settings);
                    // The transform store is the one consumer that cannot read
                    // the change on its next frame: its subscriptions were
                    // opened once, so it has to be told.
                    let whisperer = RobotWhisperer::global(cx);
                    let (tf, sessions) = (whisperer.tf.clone(), whisperer.sessions.clone());
                    sessions
                        .read(cx)
                        .pipeline()
                        .set_rate_window_ns(settings.rate_window_ns());
                    tf.update(cx, |store, cx| store.resettle(&sessions, cx));
                    cx.notify();
                }
            },
        )
        .detach();

        // Wider than the theme list needed: there is a rail of sections beside
        // the pane now, and a row's explanation is the half that makes its
        // number mean anything.
        window.open_dialog(cx, move |dialog, _window, _cx| {
            dialog.title("Settings").w(px(620.)).child(settings.clone())
        });
        cx.notify();
    }

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_picker(
            "Go to",
            "Search commands, requests and connections",
            self.palette_entries(cx),
            window,
            cx,
        );
    }

    /// The palette, over whatever list the caller wants searched.
    fn open_picker(
        &mut self,
        title: &'static str,
        placeholder: &'static str,
        entries: Vec<Entry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let palette = PaletteView::view(entries, placeholder, window, cx);
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
            dialog.title(title).w(px(620.)).child(palette.clone())
        });
        cx.notify();
    }

    /// Every topic a pane could be pointed at, as palette rows.
    ///
    /// The system's name is the row's detail, so typing either the topic or
    /// the robot narrows the list — which is the thing a flat menu grouped by
    /// system cannot do.
    fn topic_entries(
        &self,
        cx: &App,
        choose: impl Fn(i64, SharedString) -> Choice,
        drawable_only: bool,
    ) -> Vec<Entry> {
        let workspace = self.workspace.read(cx);
        let sessions = self.sessions.read(cx);
        let mut entries = Vec::new();
        for connection in workspace.connections() {
            let Some(discovery) = sessions.discovery(connection.id) else {
                continue;
            };
            for topic in &discovery.topics {
                if drawable_only && !crate::viz::is_drawable(&topic.schema_name) {
                    continue;
                }
                let name = SharedString::from(topic.name.clone());
                entries.push(
                    Entry::new("Topic", name.clone(), choose(connection.id, name))
                        .detail(format!("{}  ·  {}", connection.name, topic.schema_name)),
                );
            }
        }
        entries
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
            Entry::new("Command", "New dashboard", Choice::Command("NewDashboard")),
            Entry::new("Command", "3D world", Choice::Command("ShowWorld")),
            Entry::new(
                "Command",
                "Start or stop recording",
                Choice::Command("ToggleRecording"),
            ),
            Entry::new(
                "Command",
                "Replay last recording",
                Choice::Command("ReplayRecording"),
            ),
            Entry::new(
                "Command",
                "Save recording",
                Choice::Command("SaveRecording"),
            ),
            Entry::new(
                "Command",
                "Open recording",
                Choice::Command("OpenRecording"),
            ),
        ];

        let workspace = self.workspace.read(cx);
        entries.extend(workspace.requests().iter().map(|request| {
            Entry::new("Request", request.name.clone(), Choice::Request(request.id))
                .detail(request.target.clone())
        }));

        entries.extend(workspace.dashboards().iter().map(|dashboard| {
            Entry::new(
                "Dashboard",
                dashboard.name.clone(),
                Choice::Dashboard(dashboard.id),
            )
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
            Choice::Command("NewDashboard") => self.new_dashboard(window, cx),
            Choice::Command("ShowWorld") => self.open_world(window, cx),
            Choice::Dashboard(id) => self.open_dashboard(id, window, cx),
            Choice::Command("ToggleRecording") => self.toggle_recording(cx),
            Choice::Command("ReplayRecording") => self.replay_recording(cx),
            Choice::Command("SaveRecording") => self.save_recording(cx),
            Choice::Command("OpenRecording") => self.open_recording(cx),
            Choice::PaneTopic {
                pane,
                connection,
                topic,
            } => {
                // Called on the pane directly rather than dispatched as an
                // action: the palette is a dialog on the window, so by the
                // time it has closed there is nothing focused inside the
                // dashboard for an action to travel up through.
                self.with_pane(pane, cx, move |pane, cx| {
                    pane.set_connection(connection, cx);
                    pane.set_topic(topic.to_string(), cx);
                });
            }
            Choice::WorldTopic {
                pane,
                connection,
                topic,
            } => self.with_world(pane, cx, |world, cx| {
                world.add_topic(connection, topic.to_string(), cx)
            }),
            Choice::Command(unknown) => tracing::warn!("palette has no handler for {unknown}"),
        }
    }

    // ── record and replay ──────────────────────────────────────────────────────

    /// Starts capturing every subscribed topic, or stops and keeps the result.
    fn toggle_recording(&mut self, cx: &mut Context<Self>) {
        let recorder = RobotWhisperer::global(cx).recorder.clone();
        let recording = recorder.read(cx).is_recording();
        if recording {
            let (captured, count, seconds, full) = recorder.update(cx, |recorder, cx| {
                let captured = recorder.stop(cx);
                (
                    captured,
                    recorder.count(),
                    recorder.seconds(),
                    recorder.is_full(),
                )
            });
            if captured {
                self.say(
                    format!("recorded {count} messages over {seconds:.1}s — save or replay it"),
                    cx,
                );
                if full {
                    self.complain("recording stopped: it reached its message limit", cx);
                }
            } else {
                self.say("recording stopped; nothing had arrived", cx);
            }
        } else {
            let name = format!("recording {}", chrono::Local::now().format("%H:%M:%S"));
            recorder.update(cx, |recorder, cx| recorder.start(name, cx));
            self.say("recording every subscribed topic", cx);
        }
        cx.notify();
    }

    /// Plays the recording just captured, as a connection of its own.
    ///
    /// Without a trip through the disk: the commonest thing to do with a
    /// recording is watch it again straight away, and a save dialog in the
    /// middle of that is a detour.
    fn replay_recording(&mut self, cx: &mut Context<Self>) {
        let recording = RobotWhisperer::global(cx)
            .recorder
            .read(cx)
            .finished()
            .cloned();
        let Some(recording) = recording else {
            return self.complain("there is no recording to play", cx);
        };
        let name = if recording.name.is_empty() {
            "recording".to_string()
        } else {
            recording.name.clone()
        };
        self.say(
            format!(
                "replaying {name}: {} topics, {} messages, {:.1}s",
                recording.topics.len(),
                recording.messages.len(),
                recording.duration_ns() as f64 / 1e9
            ),
            cx,
        );
        self.add_replay_connection(name, recording.write(), cx);
    }

    /// Writes the last recording to a file.
    fn save_recording(&mut self, cx: &mut Context<Self>) {
        let Some(json) = RobotWhisperer::global(cx)
            .recorder
            .read(cx)
            .finished()
            .map(|recording| recording.write())
        else {
            return self.complain("there is no recording to save", cx);
        };
        let directory = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let picked = cx.prompt_for_new_path(&directory, Some("recording.rwrec.json"));

        cx.spawn(async move |view, cx| {
            let path = match picked.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) => return,
                _ => {
                    view.update(cx, |view, cx| {
                        view.complain("could not open a file dialog", cx)
                    })
                    .ok();
                    return;
                }
            };
            let outcome = std::fs::write(&path, json).map_err(|error| error.to_string());
            view.update(cx, |view, cx| match outcome {
                Ok(()) => view.say(format!("recording saved to {}", path.display()), cx),
                Err(error) => view.complain(format!("could not save the recording: {error}"), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Opens a recording as a connection, so requests can be pointed at it.
    fn open_recording(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });

        cx.spawn(async move |view, cx| {
            let paths = match picked.await {
                Ok(Ok(Some(paths))) => paths,
                Ok(Ok(None)) => return,
                _ => {
                    view.update(cx, |view, cx| {
                        view.complain("could not open a file dialog", cx)
                    })
                    .ok();
                    return;
                }
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            // Read and validated here rather than at connect time: a file that
            // is not a recording should say so now, not leave a broken
            // connection sitting in the list.
            let read = std::fs::read_to_string(&path)
                .map_err(|error| error.to_string())
                .and_then(|json| {
                    rw_record::Recording::read(&json)
                        .map(|recording| (recording, json))
                        .map_err(|error| error.to_string())
                });

            view.update(cx, |view, cx| {
                let (recording, json) = match read {
                    Ok(read) => read,
                    Err(error) => {
                        return view.complain(format!("could not open the recording: {error}"), cx);
                    }
                };
                let name = if recording.name.is_empty() {
                    path.file_stem()
                        .map(|stem| stem.to_string_lossy().to_string())
                        .unwrap_or_else(|| "recording".into())
                } else {
                    recording.name.clone()
                };
                view.say(
                    format!(
                        "opened {name}: {} topics, {} messages, {:.1}s",
                        recording.topics.len(),
                        recording.messages.len(),
                        recording.duration_ns() as f64 / 1e9
                    ),
                    cx,
                );
                view.add_replay_connection(name, json, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Stores a recording as a connection and opens it.
    fn add_replay_connection(&mut self, name: String, json: String, cx: &mut Context<Self>) {
        let created = self.workspace.update(cx, |workspace, cx| {
            workspace.create_connection(
                rw_core::storage::NewConnection {
                    name,
                    config: rw_core::domain::TransportConfig::Replay { recording: json },
                    color: None,
                    // Never on its own: a recording that starts playing at
                    // launch is a surprise, not a convenience.
                    auto_connect: false,
                },
                cx,
            )
        });
        cx.spawn(async move |view, cx| {
            let Some(connection) = created.await else {
                return;
            };
            view.update(cx, |view, cx| view.toggle_connection(connection.id, cx))
                .ok();
        })
        .detach();
    }

    // ── import and export ──────────────────────────────────────────────────────

    /// Writes the workspace to a file the user picks.
    fn export(&mut self, cx: &mut Context<Self>) {
        let document = self.workspace.read(cx).document();
        let directory = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let picked = cx.prompt_for_new_path(&directory, Some("robot-whisperer.json"));

        cx.spawn(async move |view, cx| {
            let path = match picked.await {
                Ok(Ok(Some(path))) => path,
                // Cancelling is a decision, not a problem.
                Ok(Ok(None)) => return,
                // No file dialog available — on Linux that means no portal.
                // Saying nothing here is the one outcome nobody can act on.
                Ok(Err(error)) => {
                    view.update(cx, |view, cx| {
                        view.complain(format!("could not open a file dialog: {error}"), cx)
                    })
                    .ok();
                    return;
                }
                Err(_) => {
                    view.update(cx, |view, cx| {
                        view.complain("could not open a file dialog", cx)
                    })
                    .ok();
                    return;
                }
            };
            let outcome = rw_core::portable::to_json(&document)
                .map_err(|error| error.to_string())
                .and_then(|json| std::fs::write(&path, json).map_err(|error| error.to_string()));

            view.update(cx, |view, cx| match outcome {
                Ok(()) => view.say(format!("exported to {}", path.display()), cx),
                Err(error) => view.complain(format!("export failed: {error}"), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Reads a workspace file and merges it into this one.
    fn import(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });

        cx.spawn(async move |view, cx| {
            let paths = match picked.await {
                Ok(Ok(Some(paths))) => paths,
                Ok(Ok(None)) => return,
                Ok(Err(error)) => {
                    view.update(cx, |view, cx| {
                        view.complain(format!("could not open a file dialog: {error}"), cx)
                    })
                    .ok();
                    return;
                }
                Err(_) => {
                    view.update(cx, |view, cx| {
                        view.complain("could not open a file dialog", cx)
                    })
                    .ok();
                    return;
                }
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };

            let read = std::fs::read_to_string(&path)
                .map_err(|error| error.to_string())
                .and_then(|json| {
                    rw_core::portable::from_json(&json).map_err(|error| error.to_string())
                });

            view.update(cx, |view, cx| {
                let document = match read {
                    Ok(document) => document,
                    Err(error) => return view.complain(format!("import failed: {error}"), cx),
                };

                let workspace = view.workspace.read(cx);
                let connections = workspace.connections().to_vec();
                let collections = workspace.collections().to_vec();
                let dashboards = workspace.dashboards().to_vec();
                let plan =
                    rw_core::portable::plan(&document, &connections, &collections, &dashboards);
                let summary = plan.summary();
                if plan.is_empty() {
                    return view.say(summary, cx);
                }

                view.workspace
                    .update(cx, |workspace, cx| workspace.apply_import(plan, cx))
                    .detach();
                view.say(summary, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Puts a line in the console, which is where this app says things.
    fn say(&self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.sessions.update(cx, |sessions, cx| {
            sessions.announce(Notice::Info(message.into()), cx)
        });
    }

    fn complain(&self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.sessions.update(cx, |sessions, cx| {
            sessions.announce(Notice::Error(message.into()), cx)
        });
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

    fn on_export(&mut self, _: &ExportWorkspace, _: &mut Window, cx: &mut Context<Self>) {
        self.export(cx);
    }

    fn on_import(&mut self, _: &ImportWorkspace, _: &mut Window, cx: &mut Context<Self>) {
        self.import(cx);
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

    fn on_new_dashboard(&mut self, _: &NewDashboard, window: &mut Window, cx: &mut Context<Self>) {
        self.new_dashboard(window, cx);
    }

    fn on_show_world(&mut self, _: &ShowWorld, window: &mut Window, cx: &mut Context<Self>) {
        self.open_world(window, cx);
    }

    /// Runs something on a dashboard pane, wherever it lives.
    ///
    /// The id is checked rather than assumed: a palette row or a dock menu can
    /// outlive the pane it was about, and a stale one must do nothing rather
    /// than reach whichever pane now holds that place.
    fn with_pane(
        &mut self,
        pane: u64,
        cx: &mut Context<Self>,
        act: impl FnOnce(&mut VizPanel, &mut Context<VizPanel>),
    ) {
        let dashboards: Vec<Entity<DashboardPanel>> = self.dashboards.values().cloned().collect();
        let found = dashboards
            .into_iter()
            .find_map(|dashboard| dashboard.read(cx).pane_by_id(pane));
        if let Some(pane) = found {
            pane.update(cx, act);
            cx.notify();
        }
    }

    /// Runs something on the world pane an action came from.
    ///
    /// The id is checked rather than assumed: the actions carry it because a
    /// dock menu dispatches from wherever it was clicked, and a stale one from
    /// a pane that has since been closed must do nothing rather than reach the
    /// next world opened.
    fn with_world(
        &mut self,
        pane: u64,
        cx: &mut Context<Self>,
        act: impl FnOnce(&mut WorldPanel, &mut Context<WorldPanel>),
    ) {
        let Some(world) = self
            .world
            .clone()
            .filter(|w| w.entity_id().as_u64() == pane)
        else {
            return;
        };
        world.update(cx, act);
        cx.notify();
    }

    fn on_toggle_recording(&mut self, _: &ToggleRecording, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_recording(cx);
    }

    fn on_replay_recording(&mut self, _: &ReplayRecording, _: &mut Window, cx: &mut Context<Self>) {
        self.replay_recording(cx);
    }

    fn on_save_recording(&mut self, _: &SaveRecording, _: &mut Window, cx: &mut Context<Self>) {
        self.save_recording(cx);
    }

    fn on_open_recording(&mut self, _: &OpenRecording, _: &mut Window, cx: &mut Context<Self>) {
        self.open_recording(cx);
    }

    /// The footer: what is happening right now.
    ///
    /// Live state only. A count of saved requests is already the request list's
    /// tab, and a version string never changes — neither is worth a permanent
    /// strip along the bottom of the window.
    fn status_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let workspace = self.workspace.read(cx);
        let sessions = self.sessions.read(cx);

        let chips: Vec<_> = workspace
            .connections()
            .iter()
            .map(|connection| {
                let id = connection.id;
                let status = sessions.status(id);
                let colour = status_colour(&status, cx);
                let detail = status.detail().unwrap_or(status.label());

                Button::new(("connection", id as usize))
                    .ghost()
                    .xsmall()
                    .tooltip(format!("{}: {detail}", connection.name))
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_center()
                            .child(tokens::status_dot(colour))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().foreground)
                                    .child(connection.name.clone()),
                            ),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.toggle_connection(id, cx)
                    }))
            })
            .collect();
        let nothing_connected = chips.is_empty();
        let (recording, captured) = {
            let recorder = RobotWhisperer::global(cx).recorder.read(cx);
            (recorder.is_recording(), recorder.count())
        };

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
            // Every ROS system, always in view: which are up, which are not, and
            // one click to change that. This is the reason the footer exists.
            // On the left with the toggles, because it is the app's own state;
            // the right is left free for whatever has just gone wrong.
            .left(h_flex().gap_1().items_center().children(chips))
            .when(nothing_connected, |bar| {
                // States the fact rather than repeating the welcome screen's
                // call to action. The footer reports; it does not recruit.
                bar.left(
                    Button::new("add-connection")
                        .ghost()
                        .xsmall()
                        .tooltip("Manage connections")
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("No connections"),
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.open_connections(window, cx)
                        })),
                )
            })
            // Recording is a mode, and a mode with nothing on screen to say it
            // is on is how people end up with a fifty-thousand-message file
            // they did not want.
            .right(
                Button::new("toggle-recording")
                    .ghost()
                    .xsmall()
                    .tooltip(if recording {
                        "Stop recording"
                    } else {
                        "Record every subscribed topic"
                    })
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_center()
                            .child(tokens::status_dot(if recording {
                                cx.theme().danger
                            } else {
                                cx.theme().muted_foreground
                            }))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(if recording {
                                        cx.theme().danger
                                    } else {
                                        cx.theme().muted_foreground
                                    })
                                    .child(if recording {
                                        format!("Recording · {captured}")
                                    } else if captured > 0 {
                                        format!("Recorded {captured}")
                                    } else {
                                        "Record".to_string()
                                    }),
                            ),
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_recording(cx))),
            )
            // A storage failure used to leave the sidebar empty with no
            // explanation, which looks exactly like a click that did nothing.
            .when_some(workspace.error().map(str::to_owned), |bar, error| {
                bar.right(
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
        let menu = self.app_menu();
        let status_bar = self.status_bar(cx);
        // Absent, not empty, when nothing is being replayed: a bar with no
        // height still draws its border.
        let replay_bar = (!self.transport.read(cx).is_empty()).then(|| self.transport.clone());
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
            .on_action(cx.listener(Self::on_export))
            .on_action(cx.listener(Self::on_import))
            .on_action(cx.listener(Self::on_connect))
            .on_action(cx.listener(Self::on_disconnect))
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .on_action(cx.listener(Self::on_toggle_console))
            .on_action(cx.listener(Self::on_new_dashboard))
            .on_action(cx.listener(Self::on_show_world))
            // The world pane's menus are drawn by the dock on its tab strip,
            // outside the pane, so they dispatch up to here — the same route
            // the dashboard's pane actions take.
            .on_action(cx.listener(|this, action: &AddWorldLayer, _, cx| {
                this.with_world(action.pane, cx, |world, cx| {
                    world.add_topic(action.connection, action.topic.to_string(), cx)
                });
            }))
            .on_action(cx.listener(|this, action: &AddWorldRobot, _, cx| {
                this.with_world(action.pane, cx, |world, cx| {
                    world.add_robot(&action.robot, cx)
                });
            }))
            .on_action(cx.listener(|this, action: &RemoveWorldLayer, _, cx| {
                this.with_world(action.pane, cx, |world, cx| {
                    world.remove(action.layer as usize, cx)
                });
            }))
            .on_action(cx.listener(|this, action: &SetWorldFrame, _, cx| {
                this.with_world(action.pane, cx, |world, cx| {
                    world.set_fixed(action.frame.to_string(), cx)
                });
            }))
            .on_action(cx.listener(|this, action: &SetWorldAnchor, _, cx| {
                this.with_world(action.pane, cx, |world, cx| {
                    world.set_anchor(action.layer as usize, action.frame.to_string(), cx)
                });
            }))
            .on_action(cx.listener(|this, action: &ResetWorldView, _, cx| {
                this.with_world(action.pane, cx, |world, cx| world.reset_view(cx));
            }))
            .on_action(cx.listener(|this, action: &PickPaneTopic, window, cx| {
                let pane = action.pane;
                let entries = this.topic_entries(
                    cx,
                    move |connection, topic| Choice::PaneTopic {
                        pane,
                        connection,
                        topic,
                    },
                    false,
                );
                this.open_picker("Point this pane at", "Search topics", entries, window, cx);
            }))
            .on_action(cx.listener(|this, action: &PickWorldTopic, window, cx| {
                let pane = action.pane;
                let entries = this.topic_entries(
                    cx,
                    move |connection, topic| Choice::WorldTopic {
                        pane,
                        connection,
                        topic,
                    },
                    true,
                );
                this.open_picker("Add to the world", "Search topics", entries, window, cx);
            }))
            .on_action(cx.listener(|this, action: &SetReplaySpeed, _, cx| {
                let (connection, hundredths) = (action.connection, action.hundredths);
                this.transport.update(cx, |bar, cx| {
                    bar.set_speed(connection, hundredths, cx);
                });
            }))
            .on_action(cx.listener(Self::on_toggle_recording))
            .on_action(cx.listener(Self::on_replay_recording))
            .on_action(cx.listener(Self::on_save_recording))
            .on_action(cx.listener(Self::on_open_recording))
            .child(
                TitleBar::new()
                    .child(div().flex_1())
                    .child(h_flex().gap_1().items_center().child(menu)),
            )
            .child(div().flex_1().min_h_0().child(self.dock.clone()))
            .children(replay_bar)
            .child(status_bar)
            .children(dialog_layer)
            .children(notification_layer)
    }
}
