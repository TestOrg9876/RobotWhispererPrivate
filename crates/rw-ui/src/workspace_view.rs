//! The window root: title bar, dock area, status bar.
//!
//! Layout is a `DockArea`, so every surface is a `Panel` that can be dragged,
//! tabbed, zoomed and closed, and the whole arrangement serialises through
//! `DockAreaState`.

use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, Styled as _, Subscription, Window, div, px,
};
use gpui_component::dock::{DockArea, DockItem, DockPlacement};
use gpui_component::menu::DropdownMenu as _;
use gpui_component::{
    IconName, Root, Sizable as _, StyledExt as _, TitleBar,
    button::{Button, ButtonVariants as _},
    h_flex,
    status_bar::StatusBar,
    v_flex,
};

use crate::actions::{NewConnection, NewRequest, ResetLayout, ToggleConsole, ToggleExplorer};
use crate::panels::{ConsolePanel, ExplorerEvent, ExplorerPanel, RequestPanel};
use crate::session::{RobotWhisperer, Sessions};
use crate::theme;
use crate::workspace::Workspace;

/// Bumped when the default arrangement changes, so stale saved layouts are
/// rebuilt rather than loaded into a shape that no longer exists.
const LAYOUT_VERSION: usize = 1;

pub struct WorkspaceView {
    dock: Entity<DockArea>,
    workspace: Entity<Workspace>,
    sessions: Entity<Sessions>,
    explorer: Entity<ExplorerPanel>,
    _subscriptions: Vec<Subscription>,
}

impl WorkspaceView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let global = RobotWhisperer::global(cx);
        let workspace = global.workspace.clone();
        let sessions = global.sessions.clone();

        let dock = cx.new(|cx| DockArea::new("workspace", Some(LAYOUT_VERSION), window, cx));
        let explorer = ExplorerPanel::view(window, cx);
        let console = ConsolePanel::view(window, cx);

        Self::install_default_layout(&dock, &explorer, &console, window, cx);

        let subscriptions = vec![
            cx.observe(&workspace, |_, _, cx| cx.notify()),
            cx.observe(&sessions, |_, _, cx| cx.notify()),
            cx.subscribe_in(&explorer, window, Self::on_explorer_event),
        ];

        workspace
            .update(cx, |workspace, cx| workspace.load(cx))
            .detach();

        Self {
            dock,
            workspace,
            sessions,
            explorer,
            _subscriptions: subscriptions,
        }
    }

    /// Explorer on the left, console at the bottom, requests in the centre.
    fn install_default_layout(
        dock: &Entity<DockArea>,
        explorer: &Entity<ExplorerPanel>,
        console: &Entity<ConsolePanel>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let weak = dock.downgrade();

        let centre = DockItem::tabs(vec![], &weak, window, cx);
        let left = DockItem::tab(explorer.clone(), &weak, window, cx);
        let bottom = DockItem::tab(console.clone(), &weak, window, cx);

        dock.update(cx, |dock, cx| {
            dock.set_version(LAYOUT_VERSION, window, cx);
            dock.set_center(centre, window, cx);
            dock.set_left_dock(left, Some(px(280.)), true, window, cx);
            dock.set_bottom_dock(bottom, Some(px(160.)), false, window, cx);
        });
    }

    fn on_explorer_event(
        &mut self,
        _explorer: &Entity<ExplorerPanel>,
        event: &ExplorerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ExplorerEvent::OpenRequest(id) => {
                let Some(request) = self.workspace.read(cx).request(*id).cloned() else {
                    return;
                };
                let panel = RequestPanel::view(
                    Some(request.id),
                    request.name.clone(),
                    request.connection_id,
                    &request.target,
                    window,
                    cx,
                );
                self.add_to_centre(panel, window, cx);
            }
            ExplorerEvent::NewConnection => self.on_new_connection(&NewConnection, window, cx),
            ExplorerEvent::NewRequest => self.on_new_request(&NewRequest, window, cx),
            ExplorerEvent::OpenTopic { connection, topic } => {
                let panel =
                    RequestPanel::view(None, topic.clone(), Some(*connection), topic, window, cx);
                self.add_to_centre(panel, window, cx);
            }
        }
    }

    fn add_to_centre(
        &mut self,
        panel: Entity<RequestPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dock.update(cx, |dock, cx| {
            dock.add_panel(Arc::new(panel), DockPlacement::Center, None, window, cx);
        });
        cx.notify();
    }

    fn on_new_request(&mut self, _: &NewRequest, window: &mut Window, cx: &mut Context<Self>) {
        let creating = self
            .workspace
            .update(cx, |workspace, cx| workspace.create_request(cx));

        cx.spawn_in(window, async move |view, window| {
            let Some(request) = creating.await else {
                return;
            };
            window
                .update(|window, cx| {
                    view.update(cx, |view, cx| {
                        let panel = RequestPanel::view(
                            Some(request.id),
                            request.name.clone(),
                            request.connection_id,
                            &request.target,
                            window,
                            cx,
                        );
                        view.add_to_centre(panel, window, cx);
                    })
                    .ok();
                })
                .ok();
        })
        .detach();
    }

    /// Adds a Dummy connection, which needs no robot and is how the app is
    /// meant to be tried for the first time.
    fn on_new_connection(
        &mut self,
        _: &NewConnection,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace
            .update(cx, |workspace, cx| workspace.create_dummy_connection(cx))
            .detach();
    }

    fn on_toggle_explorer(
        &mut self,
        _: &ToggleExplorer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dock.update(cx, |dock, cx| {
            dock.toggle_dock(DockPlacement::Left, window, cx);
        });
    }

    fn on_toggle_console(
        &mut self,
        _: &ToggleConsole,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dock.update(cx, |dock, cx| {
            dock.toggle_dock(DockPlacement::Bottom, window, cx);
        });
    }

    fn on_reset_layout(&mut self, _: &ResetLayout, window: &mut Window, cx: &mut Context<Self>) {
        let console = ConsolePanel::view(window, cx);
        Self::install_default_layout(&self.dock, &self.explorer, &console, window, cx);
        cx.notify();
    }

    fn theme_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = theme::current(cx);

        Button::new("theme")
            .ghost()
            .xsmall()
            .icon(IconName::Palette)
            .label(current)
            .tooltip("Change theme")
            .dropdown_menu(move |mut menu, _window, cx| {
                for name in theme::names() {
                    let selected = theme::current(cx) == name;
                    menu = menu.menu_with_check(
                        name.clone(),
                        selected,
                        Box::new(crate::actions::SetTheme(name.into())),
                    );
                }
                menu
            })
            .into_any_element()
    }

    fn status_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let workspace = self.workspace.read(cx);
        let connected = self.sessions.read(cx).connected_count();
        let connections = workspace.connections().len();
        let requests = workspace.requests().len();

        StatusBar::new()
            .left(
                Button::new("toggle-explorer")
                    .ghost()
                    .xsmall()
                    .icon(IconName::PanelLeft)
                    .tooltip("Toggle explorer")
                    .on_click(cx.listener(|view, _: &ClickEvent, window, cx| {
                        view.on_toggle_explorer(&ToggleExplorer, window, cx);
                    })),
            )
            .left(
                Button::new("toggle-console")
                    .ghost()
                    .xsmall()
                    .icon(IconName::PanelBottom)
                    .tooltip("Toggle console")
                    .on_click(cx.listener(|view, _: &ClickEvent, window, cx| {
                        view.on_toggle_console(&ToggleConsole, window, cx);
                    })),
            )
            .child(format!("{connected}/{connections} connected"))
            .child(format!("{requests} requests"))
            .right(format!("v{}", env!("CARGO_PKG_VERSION")))
            .into_any_element()
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sheet_layer = Root::render_sheet_layer(window, cx);
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);
        let theme_menu = self.theme_menu(cx);
        let status_bar = self.status_bar(cx);

        div()
            .id("workspace")
            .relative()
            .size_full()
            .on_action(cx.listener(Self::on_new_request))
            .on_action(cx.listener(Self::on_new_connection))
            .on_action(cx.listener(Self::on_toggle_explorer))
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
                                .child(div().font_semibold().child("Robot Whisperer"))
                                .child(theme_menu),
                        ),
                    )
                    .child(div().flex_1().min_h_0().child(self.dock.clone()))
                    .child(status_bar),
            )
            .child(div().absolute().top_8().children(notification_layer))
            .children(dialog_layer)
            .children(sheet_layer)
    }
}
