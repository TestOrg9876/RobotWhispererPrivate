//! A dashboard: a named arrangement of live views.
//!
//! Where a request editor is one target and its response, a dashboard is
//! however many views the user puts side by side — of whatever topics, from
//! whatever connections, arranged however they like.
//!
//! It is a `DockArea` of its own: a split of tab groups, which is the library's
//! tiled arrangement. Panes fill their share of the space, dragging a handle
//! moves space between neighbours, and dragging a pane's tab to an edge splits
//! it — all of that is the dock's, and none of it is written here. The
//! arrangement is saved as the dock serialises it, against the dashboard in
//! storage, on every change.

use std::sync::Arc;

use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Window, div,
};
use gpui_component::dock::ToggleZoom;
use gpui_component::dock::{
    DockArea, DockAreaState, DockEvent, DockItem, PanelEvent, PanelState, PanelView, StackPanel,
};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use rw_core::domain::Dashboard;

use crate::actions::{FreezePane, SetPaneConnection, SetPaneTopic, SetPaneView};
use crate::docking::Restored;
use crate::panels::pane::Config;
use crate::panels::{PaneChanged, VizPanel};
use crate::session::RobotWhisperer;
use crate::tokens;
use crate::workspace::Workspace;

/// Bumped when the shape of a saved arrangement changes. Layouts written by 2
/// (tiles) and 3 and 4 (two attempts at panes outside the dock) describe
/// something this can no longer rebuild.
const LAYOUT_VERSION: usize = 5;

pub struct DashboardPanel {
    focus_handle: FocusHandle,
    workspace: Entity<Workspace>,
    id: i64,
    name: SharedString,
    dock: Entity<DockArea>,
    /// Every open pane, so a menu drawn on a tab strip can be routed back to
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

            // A split of tab groups: the library's tiled layout. One group to
            // begin with, and a pane in it. Wrapped in the split even when
            // there is only one, because a `TabPanel` with no parent
            // `StackPanel` reports itself locked and a locked strip cannot be
            // dragged apart — which is the whole point of a dashboard.
            let first = VizPanel::view(Config::default(), cx);
            let centre = DockItem::h_split(
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
            tracing::info!(
                "dashboard {} was saved in an older shape; starting it fresh",
                self.id
            );
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

    /// The pane an action from a tab strip is about.
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

    /// Adds an empty pane beside the others, for the user to point somewhere.
    ///
    /// Its own tab group added to the split, rather than
    /// `DockArea::add_panel`, which puts the new panel in the first tab group
    /// it finds — a second *tab* behind the first pane instead of a second
    /// pane beside it. A dashboard tiles; that is the whole difference.
    fn add_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(stack) = self.stack(cx) else {
            tracing::warn!("dashboard {} has no split to add a pane to", self.id);
            return;
        };
        let pane = VizPanel::view(Config::default(), cx);
        self.watch(&pane, cx);
        let weak = self.dock.downgrade();
        let group = DockItem::tabs(
            vec![Arc::new(pane) as Arc<dyn PanelView>],
            &weak,
            window,
            cx,
        );
        stack.update(cx, |stack, cx| {
            stack.add_panel(group.view(), None, weak, window, cx)
        });
        self.save(cx);
    }

    /// The split holding this dashboard's tab groups.
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
}

impl DashboardPanel {
    /// The dashboard's own head, as the original app draws it: what this
    /// screen is, then the two things you do to it.
    ///
    /// It was a `+` on the dock's tab strip, four pixels wide and unlabelled,
    /// which is not a top section — it is a control hiding in the chrome. A
    /// dashboard is a whole screen of work and it should say so before you
    /// have to hover anything to find out.
    fn header(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        h_flex()
            .flex_shrink_0()
            .w_full()
            .px(tokens::designed(14.))
            .py_2()
            .gap_3()
            .items_center()
            .justify_between()
            .child(
                h_flex()
                    .min_w_0()
                    .gap_2p5()
                    .items_center()
                    .child(
                        Icon::new(IconName::LayoutDashboard)
                            .size_4()
                            .text_color(cx.theme().primary),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_base()
                            .font_semibold()
                            .text_color(cx.theme().foreground)
                            .child(self.name.clone()),
                    ),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .gap_1p5()
                    .items_center()
                    .child(
                        Button::new("zoom-dashboard")
                            .ghost()
                            .small()
                            .icon(IconName::Maximize)
                            .label("Fullscreen")
                            .on_click(cx.listener(|_, _: &ClickEvent, window, cx| {
                                window.dispatch_action(Box::new(ToggleZoom), cx);
                            })),
                    )
                    .child(
                        Button::new("add-pane")
                            .primary()
                            .small()
                            .icon(IconName::Plus)
                            .label("Add pane")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.add_pane(window, cx)
                            })),
                    ),
            )
            .into_any_element()
    }
}

impl Render for DashboardPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.header(cx);

        v_flex()
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            // Routed here because the menu these come from is drawn by the dock
            // on a tab strip, and this is the first thing above all of them.
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
            .child(header)
            .child(self.dock.clone())
    }
}
