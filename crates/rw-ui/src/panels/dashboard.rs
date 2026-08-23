//! A dashboard: a named arrangement of live views.
//!
//! Where a request editor is one target and its response, a dashboard is
//! however many views the user puts side by side — of whatever topics, from
//! whatever connections, arranged however they like.
//!
//! It tiles. Every pane fills its share of the width and dragging a handle
//! takes space from its neighbour, which is what the original app did and what
//! a dashboard is for. The library's `ResizablePanelGroup` is that, and it is
//! the whole of the layout here: this panel holds a list of panes and a
//! `ResizableState`, and hands both to it.
//!
//! It is deliberately *not* the dock. `StackPanel` asserts its children are
//! `TabPanel`s or `StackPanel`s, and a `TabPanel` paints the window colour
//! across its area with its title bar above whatever it holds — so a dashboard
//! built on the dock cannot have panes that are cards with their headers
//! inside them. `ResizablePanelGroup` is what `StackPanel` is built on, minus
//! that one assertion.
//!
//! The arrangement — what each pane is watching, and how wide it is — is saved
//! against the dashboard in storage on every change.

use gpui::{
    App, AppContext as _, Axis, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render, SharedString,
    Styled as _, Subscription, Window, div, px,
};
use gpui_component::dock::PanelEvent;
use gpui_component::{
    ActiveTheme as _, IconName, ResizableState, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_resizable, resizable_panel,
};
use rw_core::domain::Dashboard;
use serde::{Deserialize, Serialize};

use crate::actions::{ClosePane, FreezePane, SetPaneConnection, SetPaneTopic, SetPaneView};
use crate::panels::pane::Config;
use crate::panels::{PaneChanged, VizPanel};
use crate::session::RobotWhisperer;
use crate::workspace::Workspace;

/// Bumped when the shape of a saved arrangement changes.
///
/// 1 was a split of tab panels, 2 was tiles at free coordinates, 3 was a split
/// of bare panels the dock refused to hold. This is the first shape this panel
/// owns outright, and none of the three before it describes what it now draws.
const LAYOUT_VERSION: usize = 4;

/// How narrow a pane may be dragged before it stops giving ground.
const MIN_PANE_WIDTH: f32 = 220.;

/// A dashboard as it is stored: what each pane watches, and how wide it was.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Layout {
    #[serde(default)]
    version: usize,
    #[serde(default)]
    panes: Vec<Config>,
    /// One width per pane, in pixels. Empty means "share it out evenly", which
    /// is what a dashboard saved before a handle was ever dragged looks like.
    #[serde(default)]
    widths: Vec<f32>,
}

pub struct DashboardPanel {
    focus_handle: FocusHandle,
    workspace: Entity<Workspace>,
    id: i64,
    name: SharedString,
    /// Every open pane, left to right. This is the layout.
    panes: Vec<Entity<VizPanel>>,
    /// One per open pane, so retargeting a pane saves the dashboard.
    pane_subscriptions: Vec<Subscription>,
    /// The widths, held by the library and dragged by the user.
    sizes: Entity<ResizableState>,
    /// The tab group this panel sits in, so the shell can bring it forward.
    home: crate::docking::Home,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PanelEvent> for DashboardPanel {}

impl DashboardPanel {
    pub fn view(dashboard: &Dashboard, _window: &mut Window, cx: &mut App) -> Entity<Self> {
        let workspace = RobotWhisperer::global(cx).workspace.clone();
        let id = dashboard.id;
        let saved = dashboard.layout.clone();

        cx.new(|cx| {
            let mut panel = Self {
                focus_handle: cx.focus_handle(),
                workspace,
                id,
                name: dashboard.name.clone().into(),
                panes: Vec::new(),
                pane_subscriptions: Vec::new(),
                sizes: cx.new(|_| ResizableState::default()),
                home: Default::default(),
                _subscriptions: Vec::new(),
            };

            let restored = saved
                .as_deref()
                .and_then(|saved| panel.restore(saved, cx))
                .unwrap_or_default();
            // One empty pane, so a new dashboard is something to point rather
            // than a blank rectangle.
            if !restored {
                let first = VizPanel::view(Config::default(), cx);
                panel.watch(&first, cx);
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

    /// Rebuilds a saved arrangement, or leaves the dashboard empty if it cannot.
    ///
    /// Returns whether anything was restored, so the caller knows whether to
    /// put the starting pane in.
    fn restore(&mut self, saved: &str, cx: &mut Context<Self>) -> Option<bool> {
        let layout: Layout = serde_json::from_str(saved).ok()?;
        if layout.version != LAYOUT_VERSION || layout.panes.is_empty() {
            tracing::info!(
                "dashboard {} was saved in an older shape; starting it fresh",
                self.id
            );
            return Some(false);
        }
        for config in layout.panes {
            let pane = VizPanel::view(config, cx);
            self.watch(&pane, cx);
        }
        Some(true)
    }

    /// Saves the dashboard: what each pane watches, and how wide it is.
    fn save(&mut self, cx: &mut Context<Self>) {
        let layout = Layout {
            version: LAYOUT_VERSION,
            panes: self
                .panes
                .iter()
                .map(|pane| pane.read(cx).config())
                .collect(),
            widths: self
                .sizes
                .read(cx)
                .sizes()
                .iter()
                .map(|size| f32::from(*size))
                .collect(),
        };
        let Ok(saved) = serde_json::to_string(&layout) else {
            return;
        };
        let id = self.id;
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.save_dashboard_layout(id, saved, cx)
            })
            .detach();
    }

    /// Takes a pane into the layout and saves when it is retargeted.
    fn watch(&mut self, pane: &Entity<VizPanel>, cx: &mut Context<Self>) {
        self.panes.push(pane.clone());
        self.pane_subscriptions
            .push(cx.subscribe(pane, |this, _, _: &PaneChanged, cx| this.save(cx)));
    }

    /// The pane an action from a pane's own menu is about.
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
    fn add_pane(&mut self, cx: &mut Context<Self>) {
        let pane = VizPanel::view(Config::default(), cx);
        self.watch(&pane, cx);
        // The widths belong to the panes that had them; a new pane means a new
        // share for everyone, which is what clearing asks the group to do.
        self.sizes.update(cx, |sizes, _| sizes.clear());
        self.save(cx);
        cx.notify();
    }

    /// Closes one, unless it is the last — an empty dashboard has nothing to
    /// add a pane from.
    fn close_pane(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.panes.len() <= 1 {
            return;
        }
        let Some(index) = self
            .panes
            .iter()
            .position(|pane| pane.entity_id().as_u64() == id)
        else {
            return;
        };
        self.panes.remove(index);
        drop(self.pane_subscriptions.remove(index));
        self.sizes.update(cx, |sizes, _| sizes.clear());
        self.save(cx);
        cx.notify();
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
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.add_pane(cx))),
        ])
    }
}

impl Render for DashboardPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let panes = self.panes.clone();

        // Nothing above the panes. The dashboard's name is already the tab, and
        // "Add pane" is a panel action the dock draws beside it — a header
        // strip here would repeat the name and spend a row of every pane's
        // height saying nothing.
        div()
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            // Routed here because a pane's menu is drawn by the pane, and this
            // is the first thing above all of them.
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
            .on_action(cx.listener(|this, action: &ClosePane, _, cx| {
                this.close_pane(action.pane, cx);
            }))
            .child(
                h_resizable("dashboard-panes")
                    .with_state(&self.sizes)
                    .axis(Axis::Horizontal)
                    .children(panes.into_iter().map(|pane| {
                        resizable_panel()
                            .size_range(px(MIN_PANE_WIDTH)..Pixels::MAX)
                            .child(pane)
                    })),
            )
    }
}
