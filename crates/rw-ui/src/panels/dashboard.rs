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
    App, AppContext as _, Axis, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Window, div,
};
use gpui_component::dock::{
    DockArea, DockAreaState, DockEvent, DockItem, PanelEvent, PanelInfo, PanelState, PanelView,
    StackPanel,
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
///
/// Version 2 was tiles — panes at free coordinates. Version 3 is a split
/// again, of bare panels rather than tab panels, so neither of the two shapes
/// saved before it can be rebuilt into what this now draws.
const LAYOUT_VERSION: usize = 3;

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

            // A split of *bare* panels. Two decisions, and really the same one
            // twice.
            //
            // A split, because a dashboard tiles: every pane fills its share of
            // the space, and dragging an edge takes space from its neighbour.
            // Free coordinates — panes that overlap each other and leave holes
            // — were tried here and were wrong.
            //
            // Bare panels rather than `DockItem::tabs`, because a `TabPanel`
            // paints the window colour across its whole area and puts its title
            // bar above whatever it holds. Nothing between the split and the
            // pane is the only arrangement in which a pane can draw itself as
            // one card with its header inside it.
            let first = VizPanel::view(Config::default(), cx);
            let centre = DockItem::split_with_sizes(
                Axis::Horizontal,
                vec![DockItem::panel(
                    Arc::new(first.clone()) as Arc<dyn PanelView>
                )],
                vec![None],
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
        // The saved string carries no version of its own — the version lives on
        // the `DockAreaState` this builds around it, so it always reads as the
        // current one. A layout written before tiles describes a `StackPanel`,
        // which would load happily and give back the split dashboard this
        // replaced, so it is recognised by its shape and dropped.
        if !matches!(centre.info, PanelInfo::Stack { .. }) {
            tracing::info!(
                "dashboard {} was saved in an older shape; starting it fresh",
                self.id
            );
            return;
        }
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
        // Added to the split itself rather than through `DockArea::add_panel`,
        // which looks for a `TabPanel` among the children and makes one when it
        // finds none — putting the new pane in a tab strip beside the old one
        // instead of beside it in the layout.
        let Some(stack) = self.stack(cx) else {
            tracing::warn!("dashboard {} has no split to add a pane to", self.id);
            return;
        };
        let pane = VizPanel::view(Config::default(), cx);
        self.watch(&pane, cx);
        let dock = self.dock.downgrade();
        stack.update(cx, |stack, cx| {
            stack.add_panel(Arc::new(pane) as Arc<dyn PanelView>, None, dock, window, cx)
        });
        self.save(cx);
    }

    /// The split holding this dashboard's panes.
    ///
    /// Read back from the dock rather than held, because `DockArea::load`
    /// builds a new one every time a saved arrangement is restored and a
    /// stored handle would point at the one it replaced.
    fn stack(&self, cx: &App) -> Option<Entity<StackPanel>> {
        match self.dock.read(cx).center() {
            DockItem::Split { view, .. } => Some(view.clone()),
            _ => None,
        }
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

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.name.clone()
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
