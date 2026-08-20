//! A dashboard: a named arrangement of live views.
//!
//! Where a request editor is one target and its response, a dashboard is
//! however many views the user puts side by side — of whatever topics, from
//! whatever connections, arranged however they like. That arrangement is a dock
//! of its own, nested inside this panel, which is why request tabs can stay
//! plain tabs: composing a layout is what a dashboard is *for*, so it is the
//! only place that offers it.
//!
//! The arrangement is saved as the dock serialises it, against the dashboard in
//! storage, on every change.

use std::sync::Arc;

use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Window, div,
};
use gpui_component::dock::{
    DockArea, DockAreaState, DockEvent, DockItem, DockPlacement, PanelEvent, PanelState, PanelView,
};
use gpui_component::{
    ActiveTheme as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
};
use rw_core::domain::Dashboard;

use crate::actions::{FreezePane, SetPaneConnection, SetPaneTopic, SetPaneView};
use crate::docking::Restored;
use crate::panels::pane::Config;
use crate::panels::{PaneChanged, VizPanel};
use crate::session::RobotWhisperer;
use crate::workspace::Workspace;

/// Bumped when the shape of a saved arrangement changes.
const LAYOUT_VERSION: usize = 1;

pub struct DashboardPanel {
    focus_handle: FocusHandle,
    workspace: Entity<Workspace>,
    id: i64,
    name: SharedString,
    dock: Entity<DockArea>,
    /// Every open pane, so a menu drawn on the tab strip can be routed back to
    /// the one it belongs to.
    panes: Vec<Entity<VizPanel>>,
    /// One per open pane, so retargeting a pane saves the dashboard.
    pane_subscriptions: Vec<Subscription>,
    /// The tab group this panel sits in, so the shell can bring it forward.
    home: crate::docking::Home,
    /// Whether this panel's tab is the one showing in its group.
    ///
    /// Recorded rather than asked for: the chip is drawn by [`Panel::title`],
    /// which the dock calls from inside its own update, and reading the tab
    /// group from there is a double lease. `set_active` is dispatched outside
    /// that update precisely so a panel can keep this.
    tab_active: bool,

    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PanelEvent> for DashboardPanel {}

impl DashboardPanel {
    pub fn view(dashboard: &Dashboard, window: &mut Window, cx: &mut App) -> Entity<Self> {
        let workspace = RobotWhisperer::global(cx).workspace.clone();
        let id = dashboard.id;
        let saved = dashboard.layout.clone();

        cx.new(|cx| {
            let dock = cx.new(|cx| DockArea::new("dashboard", Some(LAYOUT_VERSION), window, cx));
            let weak = dock.downgrade();

            // Wrapped in a split even when there is one pane: a `TabPanel` with
            // no parent `StackPanel` reports itself locked, and a locked tab
            // strip cannot be dragged apart — which is the whole point here.
            let first = VizPanel::view(Config::default(), cx);
            let centre = DockItem::v_split(
                vec![DockItem::tabs(
                    vec![Arc::new(first.clone()) as Arc<dyn PanelView>],
                    &weak,
                    window,
                    cx,
                )],
                &weak,
                window,
                cx,
            );
            dock.update(cx, |area, cx| area.set_center(centre, window, cx));

            let mut panel = Self {
                focus_handle: cx.focus_handle(),
                workspace,
                id,
                name: dashboard.name.clone().into(),
                dock: dock.clone(),
                panes: Vec::new(),
                pane_subscriptions: Vec::new(),
                home: Default::default(),
                tab_active: false,
                _subscriptions: vec![cx.subscribe_in(
                    &dock,
                    window,
                    |this: &mut Self, _, _: &DockEvent, _, cx| this.save(cx),
                )],
            };
            panel.watch(&first, cx);

            // Restored after the default is in place, so a layout that will not
            // parse leaves a usable dashboard rather than an empty one.
            if let Some(saved) = saved {
                panel.restore(&saved, window, cx);
            }
            panel
        })
    }

    pub fn id(&self) -> i64 {
        self.id
    }

    pub fn home(&self) -> Option<gpui::WeakEntity<gpui_component::dock::TabPanel>> {
        self.home.tab_panel()
    }

    pub fn set_name(&mut self, name: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.name = name.into();
        cx.notify();
    }

    /// Rebuilds a saved arrangement.
    fn restore(&mut self, saved: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(centre) = serde_json::from_str::<PanelState>(saved) else {
            tracing::warn!("dashboard {} has an unreadable layout", self.id);
            return;
        };
        cx.set_global(Restored::default());
        let state = DockAreaState {
            version: Some(LAYOUT_VERSION),
            center: centre,
            left_dock: None,
            right_dock: None,
            bottom_dock: None,
        };
        if let Err(error) = self
            .dock
            .update(cx, |dock, cx| dock.load(state, window, cx))
        {
            tracing::warn!("dashboard {}: {error}", self.id);
            return;
        }
        // Panes rebuilt by the deserializer have no route back here, so they
        // leave themselves in the global and are claimed now.
        let restored = std::mem::take(cx.global_mut::<Restored>());
        self.pane_subscriptions.clear();
        self.panes.clear();
        for pane in restored.panes {
            self.watch(&pane, cx);
        }
        cx.notify();
    }

    /// Saves the dashboard when a pane is retargeted, not only when the layout
    /// moves — the dock has no idea a pane changed what it is watching.
    fn watch(&mut self, pane: &Entity<VizPanel>, cx: &mut Context<Self>) {
        self.panes.push(pane.clone());
        self.pane_subscriptions
            .push(cx.subscribe(pane, |this, _, _: &PaneChanged, cx| this.save(cx)));
    }

    /// The pane an action from the tab strip is about.
    fn pane(&self, id: u64) -> Option<&Entity<VizPanel>> {
        self.panes
            .iter()
            .find(|pane| pane.entity_id().as_u64() == id)
    }

    /// One of this dashboard's panes, by the id an action or a palette row
    /// carries.
    pub fn pane_by_id(&self, id: u64) -> Option<Entity<VizPanel>> {
        self.pane(id).cloned()
    }

    /// Adds an empty pane, for the user to point somewhere.
    fn add_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = VizPanel::view(Config::default(), cx);
        self.watch(&pane, cx);
        self.dock.update(cx, |dock, cx| {
            dock.add_panel(
                Arc::new(pane) as Arc<dyn PanelView>,
                DockPlacement::Center,
                None,
                window,
                cx,
            )
        });
        self.save(cx);
    }

    /// Writes the arrangement out.
    fn save(&mut self, cx: &mut Context<Self>) {
        let centre = self.dock.read(cx).dump(cx).center;
        let Ok(layout) = serde_json::to_string(&centre) else {
            return;
        };
        let id = self.id;
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.save_dashboard_layout(id, layout, cx)
            })
            .detach();
    }
}

impl Focusable for DashboardPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl gpui_component::dock::Panel for DashboardPanel {
    fn panel_name(&self) -> &'static str {
        "Dashboard"
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        gpui_component::h_flex().child(
            crate::tokens::tab_chip(self.tab_active, cx)
                .child(gpui_component::Icon::new(IconName::LayoutDashboard).xsmall())
                .child(self.name.clone()),
        )
    }

    /// The dock's own tab is transparent here; the chip inside it is what shows
    /// selection, so the panel has to be told.
    fn set_active(&mut self, active: bool, _window: &mut Window, cx: &mut Context<Self>) {
        if self.tab_active != active {
            self.tab_active = active;
            cx.notify();
        }
    }

    fn on_added_to(
        &mut self,
        tab_panel: gpui::WeakEntity<gpui_component::dock::TabPanel>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.home.moved_to(tab_panel);
    }

    /// Rendered by the dock beside this panel's tab, so adding a pane costs no
    /// height inside the dashboard itself.
    fn toolbar_buttons(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Vec<Button>> {
        Some(vec![
            Button::new("add-pane")
                .ghost()
                .xsmall()
                .icon(IconName::Plus)
                .tooltip("Add pane")
                .on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| this.add_pane(window, cx)),
                ),
        ])
    }
}

impl Render for DashboardPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Nothing but the dock. The dashboard's name is already the tab above
        // it, and "Add pane" is a panel action the dock renders beside that
        // tab — a header strip here would repeat the name and spend a row of
        // the pane's height saying nothing.
        div()
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            // Routed here because the menu these come from is drawn by the dock
            // on the tab strip, and this is the first thing above both.
            .on_action(cx.listener(|this, action: &SetPaneConnection, _, cx| {
                if let Some(pane) = this.pane(action.pane).cloned() {
                    pane.update(cx, |pane, cx| pane.set_connection(action.connection, cx));
                }
            }))
            .on_action(cx.listener(|this, action: &SetPaneTopic, _, cx| {
                if let Some(pane) = this.pane(action.pane).cloned() {
                    pane.update(cx, |pane, cx| pane.set_topic(action.topic.to_string(), cx));
                }
            }))
            .on_action(cx.listener(|this, action: &SetPaneView, _, cx| {
                if let Some(pane) = this.pane(action.pane).cloned() {
                    pane.update(cx, |pane, cx| pane.set_view(&action.view, cx));
                }
            }))
            .on_action(cx.listener(|this, action: &FreezePane, _, cx| {
                if let Some(pane) = this.pane(action.pane).cloned() {
                    pane.update(cx, |pane, cx| pane.freeze(cx));
                }
            }))
            .child(self.dock.clone())
    }
}
