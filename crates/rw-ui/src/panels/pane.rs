//! One live view inside a dashboard.
//!
//! A pane is a topic on a connection, shown one of the ways `views` knows how.
//! That is the whole of it — no sending, no saving, no editing: a dashboard is
//! for watching, and a request editor already exists for the rest.
//!
//! Panes subscribe themselves and unsubscribe when they go away, so a dashboard
//! that is closed stops costing anything.

use std::sync::{Arc, Mutex};

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, Window, div,
};
use gpui_component::dock::{Panel, PanelEvent, PanelState, TabPanel};
use gpui_component::menu::DropdownMenu as _;
use gpui_component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use rw_canonical::CanonicalValue;

use crate::panels::collections::Dragged;
use crate::panels::drop;
use crate::scene_view::SceneView;
use crate::series::History;
use crate::session::{RobotWhisperer, Sessions};
use crate::tokens;
use crate::views::{self, View};
use crate::workspace::Workspace;

/// What has arrived, written by the subscription and read by the pane.
#[derive(Default)]
struct Incoming {
    value: Option<CanonicalValue>,
    schema: Option<SharedString>,
    count: u64,
    history: History,
}

/// How a pane is stored inside a dashboard's layout.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<i64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub topic: String,
    #[serde(default)]
    pub view: String,
}

pub struct VizPanel {
    focus_handle: FocusHandle,
    workspace: Entity<Workspace>,
    sessions: Entity<Sessions>,
    connection: Option<i64>,
    topic: String,
    view: View,
    incoming: Arc<Mutex<Incoming>>,
    /// The open subscription, so it can be closed when the topic changes.
    subscription: Option<String>,
    problem: Option<SharedString>,
    /// The message the diff view compares against, and which message it was.
    ///
    /// Pinned rather than copied on every frame: the live stream keeps running
    /// underneath, which is the whole point — freezing a value and watching it
    /// drift is the question people are asking when they stare at a raw view.
    frozen: Option<(CanonicalValue, u64)>,
    /// Built the first time a message turns out to be a point cloud.
    scene: Option<Entity<SceneView>>,
    scene_at: u64,
    /// The foldable tree of the message, built the first time it is looked at.
    tree: Option<Entity<crate::tree::TreeView>>,

    _repaint: Option<Task<()>>,
    _work: Option<Task<()>>,
}

/// Emitted so the dashboard can save its layout when a pane is retargeted.
pub struct PaneChanged;
impl EventEmitter<PaneChanged> for VizPanel {}
impl EventEmitter<PanelEvent> for VizPanel {}

impl VizPanel {
    pub fn view(config: Config, cx: &mut App) -> Entity<Self> {
        let (workspace, sessions) = {
            let global = RobotWhisperer::global(cx);
            (global.workspace.clone(), global.sessions.clone())
        };
        cx.new(|cx| {
            let mut pane = Self {
                focus_handle: cx.focus_handle(),
                workspace,
                sessions,
                connection: config.connection,
                topic: config.topic,
                view: View::parse(&config.view),
                incoming: Arc::new(Mutex::new(Incoming::default())),
                subscription: None,
                problem: None,
                frozen: None,
                scene: None,
                scene_at: 0,
                tree: None,
                _repaint: None,
                _work: None,
            };
            // A pane restored from a saved dashboard is already pointed at
            // something, and should start showing it without being asked.
            pane.resubscribe(cx);
            pane
        })
    }

    pub fn config(&self) -> Config {
        Config {
            connection: self.connection,
            topic: self.topic.clone(),
            view: self.view.as_str().to_string(),
        }
    }

    pub fn set_connection(&mut self, connection: i64, cx: &mut Context<Self>) {
        let topic = self.topic.clone();
        self.set_target(Some(connection), topic, cx);
    }

    pub fn set_topic(&mut self, topic: String, cx: &mut Context<Self>) {
        let connection = self.connection;
        self.set_target(connection, topic, cx);
    }

    /// Pins the current message. Pinning again re-pins to the newest.
    pub fn freeze(&mut self, cx: &mut Context<Self>) {
        let (value, count) = {
            let incoming = self.incoming.lock().expect("incoming mutex");
            (incoming.value.clone(), incoming.count)
        };
        self.frozen = value.map(|value| (value, count));
        // Freezing with nowhere to look at the result is a control that does
        // nothing visible, so it takes the pane to the view it is for.
        self.view = View::Diff;
        cx.emit(PaneChanged);
        cx.notify();
    }

    pub fn set_view(&mut self, view: &str, cx: &mut Context<Self>) {
        self.view = View::parse(view);
        cx.emit(PaneChanged);
        cx.notify();
    }

    /// Points the pane at a topic on a connection in one step.
    ///
    /// Both at once rather than one after the other: a drop knows both, and
    /// setting them separately would resubscribe twice — once to the new topic
    /// on the old connection, which is a subscription to something nobody asked
    /// for.
    pub fn set_target(&mut self, connection: Option<i64>, topic: String, cx: &mut Context<Self>) {
        self.connection = connection;
        self.topic = topic;
        self.resubscribe(cx);
        cx.emit(PaneChanged);
        cx.notify();
    }

    /// Closes any open subscription and opens the one this pane now wants.
    fn resubscribe(&mut self, cx: &mut Context<Self>) {
        let pipeline = self.sessions.read(cx).pipeline();
        if let Some(handle) = self.subscription.take() {
            cx.background_spawn(async move {
                pipeline.unsubscribe(&handle).await.ok();
            })
            .detach();
        }
        *self.incoming.lock().expect("incoming mutex") = Incoming::default();
        self.scene_at = 0;
        self.problem = None;
        // A pin belongs to the topic it was taken from; keeping it across a
        // retarget would diff two different messages against each other.
        self.frozen = None;

        let (Some(connection), false) = (self.connection, self.topic.trim().is_empty()) else {
            self._repaint = None;
            return;
        };
        let Some(session) = self.sessions.read(cx).session(connection) else {
            self.problem = Some("That system is not connected.".into());
            self._repaint = None;
            return;
        };

        let pipeline = self.sessions.read(cx).pipeline();
        let incoming = Arc::clone(&self.incoming);
        let topic = self.topic.clone();
        let recorder = RobotWhisperer::global(cx).recorder.read(cx).tap();
        let captured = topic.clone();

        self.start_repaint(cx);
        self._work = Some(cx.spawn(async move |pane, cx| {
            let opened = pipeline
                .subscribe_topic(session, &topic, move |_handle, frame, _lossy| {
                    recorder.observe(
                        &captured,
                        &frame.schema.name,
                        (!frame.schema.definition.is_empty())
                            .then_some(frame.schema.definition.as_str()),
                        &frame.value,
                    );
                    let Ok(mut incoming) = incoming.lock() else {
                        return;
                    };
                    incoming.history.observe(&frame.value);
                    incoming.schema = Some(frame.schema.name.clone().into());
                    incoming.value = Some(frame.value.clone());
                    incoming.count += 1;
                })
                .await;
            pane.update(cx, |pane, cx| {
                match opened {
                    Ok(opened) => pane.subscription = Some(opened.subscription_id),
                    Err(error) => pane.problem = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Frames arrive off the UI thread, so the pane wakes itself to draw them.
    fn start_repaint(&mut self, cx: &mut Context<Self>) {
        self._repaint = Some(cx.spawn(async move |pane, cx| {
            loop {
                crate::tick::sleep(std::time::Duration::from_millis(100), cx).await;
                if pane.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));
    }

    /// Hands the newest message to the 3D pane, building it on first sight.
    ///
    /// What it decodes into is the registry's decision, from the schema's role
    /// — so a scan, a path and a pose all arrive here rather than only a cloud.
    /// Points the tree at the newest message, when the tree is what is showing.
    ///
    /// Here rather than in `render` for the same reason the scene is: the rows
    /// are rebuilt once per message, and a pane paints ten times a second.
    fn sync_tree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.view, View::Pretty | View::Visualize) {
            return;
        }
        let (value, count) = {
            let incoming = self.incoming.lock().expect("incoming mutex");
            (incoming.value.clone(), incoming.count)
        };
        let Some(value) = value else { return };
        let tree = match &self.tree {
            Some(tree) => tree.clone(),
            None => {
                let tree = crate::tree::TreeView::view(cx);
                self.tree = Some(tree.clone());
                tree
            }
        };
        tree.update(cx, |tree, cx| tree.show(&value, count, window, cx));
    }

    /// The tree, ready to draw, with folding wired back to it.
    fn tree(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(tree) = self.tree.clone() else {
            return div().into_any_element();
        };
        let folding = tree.clone();
        crate::tree::render(
            &tree,
            move |path, window, cx| {
                let path = path.to_string();
                folding.update(cx, |tree, cx| tree.toggle(&path, window, cx));
            },
            cx,
        )
    }

    /// The views this pane's topic can actually fill.
    ///
    /// The same decision the request editor's strip makes, from the same
    /// function, because a pane and an editor showing one topic must offer the
    /// same things.
    fn offered(&self) -> Vec<View> {
        let incoming = self.incoming.lock().expect("incoming mutex");
        if incoming.value.is_none() {
            return Vec::new();
        }
        View::offered(views::Offers::of(
            &crate::viz::role_for(incoming.schema.as_deref().unwrap_or_default()),
            &incoming.history,
            self.frozen.is_some(),
        ))
    }

    fn sync_scene(&mut self, cx: &mut Context<Self>) {
        if self.view != View::Visualize {
            return;
        }
        let (value, schema, count) = {
            let incoming = self.incoming.lock().expect("incoming mutex");
            (
                incoming.value.clone(),
                incoming.schema.clone(),
                incoming.count,
            )
        };
        if count == self.scene_at {
            return;
        }
        let (Some(value), Some(schema)) = (value, schema) else {
            return;
        };
        let Some(layers) = crate::viz::layers_for(
            &crate::viz::role_for(&schema),
            &value,
            crate::tf::tree(self.connection, cx).as_ref(),
        ) else {
            return;
        };
        self.scene_at = count;
        let scene = match &self.scene {
            Some(scene) => scene.clone(),
            None => {
                let scene = SceneView::view(cx);
                self.scene = Some(scene.clone());
                scene
            }
        };
        scene.update(cx, |scene, cx| scene.set_layers(layers, cx));
    }

    /// The systems this pane could be pointed at.
    fn systems(&self, cx: &App) -> Vec<(i64, SharedString)> {
        self.workspace
            .read(cx)
            .connections()
            .iter()
            .map(|connection| (connection.id, SharedString::from(connection.name.clone())))
            .collect()
    }

    /// How many topics the picker would have to offer.
    ///
    /// Across every connected system rather than only this pane's, because
    /// choosing a topic in the picker sets the system too — requiring one
    /// first was two decisions where the data only needs one.
    fn offered_topics(&self, cx: &App) -> usize {
        let workspace = self.workspace.read(cx);
        let sessions = self.sessions.read(cx);
        workspace
            .connections()
            .iter()
            .filter_map(|connection| sessions.discovery(connection.id))
            .map(|discovery| discovery.topics.len())
            .sum()
    }

    /// The two pickers, shown in the body while the pane is still empty.
    ///
    /// Only then. Once a pane is showing something, its topic is the tab's
    /// title and its settings are on the tab strip — a permanent control row
    /// would repeat the one and duplicate the other.
    fn pickers(&self, cx: &mut Context<Self>) -> AnyElement {
        let pane = cx.entity_id().as_u64();
        let systems = self.systems(cx);
        let topic_count = self.offered_topics(cx);
        let chosen = self.connection;
        let named = self
            .connection
            .and_then(|id| self.workspace.read(cx).connection(id))
            .map(|connection| SharedString::from(connection.name.clone()))
            .unwrap_or_else(|| "System".into());
        let showing = SharedString::from(self.topic.clone());

        h_flex()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap_1()
            .child(
                Button::new("pick-system")
                    .ghost()
                    .xsmall()
                    .label(named)
                    .dropdown_menu(move |mut menu, _window, _cx| {
                        if systems.is_empty() {
                            return menu.menu(
                                "Add a connection…",
                                Box::new(crate::actions::ManageConnections),
                            );
                        }
                        for (id, name) in systems.clone() {
                            menu = menu.menu_with_check(
                                name,
                                Some(id) == chosen,
                                Box::new(crate::actions::SetPaneConnection {
                                    pane,
                                    connection: id,
                                }),
                            );
                        }
                        menu
                    }),
            )
            .child(
                Button::new("pick-topic")
                    .ghost()
                    .xsmall()
                    .label(if showing.is_empty() {
                        SharedString::from("Topic")
                    } else {
                        showing.clone()
                    })
                    // Straight to the searchable picker rather than a menu of
                    // every topic — a robot with three hundred of them is the
                    // case this has to survive. With nothing connected there is
                    // nothing to search, so it offers the one thing that would
                    // help instead.
                    .on_click(cx.listener(move |_, _, window, cx| {
                        if topic_count == 0 {
                            window.dispatch_action(Box::new(crate::actions::ManageConnections), cx);
                        } else {
                            window.dispatch_action(
                                Box::new(crate::actions::PickPaneTopic { pane }),
                                cx,
                            );
                        }
                    })),
            )
            .into_any_element()
    }
}

impl Focusable for VizPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for VizPanel {
    fn panel_name(&self) -> &'static str {
        "Pane"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        if self.topic.is_empty() {
            SharedString::from("New pane")
        } else {
            SharedString::from(self.topic.clone())
        }
    }

    fn dump(&self, _cx: &App) -> PanelState {
        let mut state = PanelState::new(self);
        state.info = gpui_component::dock::PanelInfo::panel(
            serde_json::to_value(self.config()).unwrap_or_default(),
        );
        state
    }

    /// The message count, beside the tab's title.
    ///
    /// Small, dim and out of the way — enough to see a pane is alive without
    /// spending a row of it on a status strip.
    fn title_suffix(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let count = self.incoming.lock().expect("incoming mutex").count;
        // The rate beside the count, when there is one: two short readings in
        // the space a tab suffix already takes, rather than a status strip
        // inside the pane that would cost a row of it forever.
        let rate = self
            .subscription
            .as_ref()
            .and_then(|handle| self.sessions.read(cx).pipeline().stats(handle))
            .and_then(|stats| stats.hz_label());
        (count > 0).then(|| {
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(match rate {
                    Some(rate) => SharedString::from(format!("{}  {rate}", compact(count))),
                    None => compact(count),
                })
                .into_any_element()
        })
    }

    /// Everything this pane can be told, on the menu the dock already draws
    /// beside the tab. Flat: a submenu to reach a topic is two clicks and a
    /// hunt for something that should be one click and a read.
    fn dropdown_menu(
        &mut self,
        mut menu: gpui_component::menu::PopupMenu,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_component::menu::PopupMenu {
        let pane = cx.entity_id().as_u64();
        let systems = self.systems(cx);
        let topics = self.offered_topics(cx);
        // Only the views this message can fill, and none at all before the
        // first one arrives — a menu offering Plot on a topic with no numbers
        // in it is a line that exists to disappoint.
        let offered = self.offered();
        let active = self.view.or_pretty(&offered);
        for view in &offered {
            menu = menu.menu_with_check(
                view.label(),
                *view == active,
                Box::new(crate::actions::SetPaneView {
                    pane,
                    view: view.as_str().into(),
                }),
            );
        }
        // One entry, whose wording says whether there is already a pin — the
        // alternative is two entries, one of which is always inert. Nothing has
        // arrived means nothing to pin, so there is no entry at all.
        if !offered.is_empty() {
            menu = menu.separator().menu(
                if self.frozen.is_some() {
                    "Freeze again"
                } else {
                    "Freeze"
                },
                Box::new(crate::actions::FreezePane { pane }),
            );
        }
        if !systems.is_empty() {
            menu = menu.separator();
            for (id, name) in systems {
                menu = menu.menu_with_check(
                    name,
                    Some(id) == self.connection,
                    Box::new(crate::actions::SetPaneConnection {
                        pane,
                        connection: id,
                    }),
                );
            }
        }
        // One searchable entry rather than every topic: a flat list works on
        // the twelve a simulator publishes and not at all on the three hundred
        // a real robot does, and the palette already ranks and already takes
        // the keyboard.
        if topics > 0 {
            menu = menu.separator().menu(
                SharedString::from(match self.topic.is_empty() {
                    true => format!("Choose a topic…  ({topics} available)"),
                    false => format!("Change topic…  ({topics} available)"),
                }),
                Box::new(crate::actions::PickPaneTopic { pane }),
            );
        }
        menu
    }

    fn on_added_to(
        &mut self,
        _tab_panel: gpui::WeakEntity<TabPanel>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }
}

impl Render for VizPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_scene(cx);
        self.sync_tree(window, cx);
        let (value, schema, history) = {
            let incoming = self.incoming.lock().expect("incoming mutex");
            (
                incoming.value.clone(),
                incoming.schema.clone(),
                incoming.history.clone(),
            )
        };

        let showing = self.view.or_pretty(&self.offered());
        let body = match (&value, showing) {
            (Some(value), View::Raw) => views::raw(value, cx),
            (Some(_), View::Plot) => views::plot(&history, cx),
            (Some(value), View::Diff) => {
                views::changes(self.frozen.as_ref().map(|(value, _)| value), value, cx)
            }
            (Some(_), View::Pretty) => self.tree(cx),
            (Some(value), View::Visualize) => views::visualize(
                &crate::viz::role_for(schema.as_deref().unwrap_or_default()),
                value,
                self.scene.as_ref(),
                self.tree(cx),
                cx,
            ),
            // A pane with nothing in it offers the two pickers, where there is
            // room for them and nothing else to show. Otherwise one dim line:
            // a pane is often small, and the full empty state — icon tile,
            // heading, explanation — would fill it.
            (None, _) if self.topic.is_empty() && self.problem.is_none() => self.pickers(cx),
            (None, _) => div()
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .p_3()
                .text_xs()
                .text_center()
                .text_color(cx.theme().muted_foreground)
                .child(match &self.problem {
                    Some(problem) => SharedString::from(problem.to_string()),
                    None => SharedString::from("Waiting for the first message…"),
                })
                .into_any_element(),
        };

        // The same material as everywhere else: content sits on an elevated
        // card with a hairline border, inset from the pane's edge. Without it a
        // dashboard reads as bare text on the window while every other surface
        // in the app is a card, which is what made it look unfinished.
        // A topic dragged out of the sidebar lands here and retargets the
        // pane. The whole pane is the target rather than a strip of it: at the
        // sizes a dashboard pane comes in, anything smaller is a game.
        let workspace = self.workspace.clone();

        v_flex()
            .id("pane")
            .size_full()
            .min_h_0()
            .p_2()
            .track_focus(&self.focus_handle)
            .bg(cx.theme().background)
            .child(
                tokens::card(cx)
                    .id("pane-drop")
                    .flex_1()
                    .min_h_0()
                    // The card rather than the pane around it: the card is what
                    // fills the pane, so tinting anything else lights up an
                    // eight-pixel frame and calls it an affordance.
                    .drag_over::<Dragged>(move |style, dragged: &Dragged, _, cx| {
                        match drop::target_of_drag(dragged, workspace.read(cx)) {
                            Some(_) => style.bg(cx.theme().drop_target),
                            None => style,
                        }
                    })
                    .on_drop(cx.listener(|this, dragged: &Dragged, _, cx| {
                        let Some(target) = drop::target_of_drag(dragged, this.workspace.read(cx))
                        else {
                            return;
                        };
                        // A request with no environment of its own still names
                        // a topic, and the pane keeps the one it already has.
                        let connection = target.connection.or(this.connection);
                        this.set_target(connection, target.topic, cx);
                    }))
                    .child(
                        tokens::card_body()
                            .id("pane-body")
                            .overflow_scroll()
                            .child(body),
                    ),
            )
    }
}

/// A message count short enough to sit beside a tab title.
fn compact(count: u64) -> SharedString {
    match count {
        0..=999 => SharedString::from(count.to_string()),
        1_000..=999_999 => SharedString::from(format!("{:.1}k", count as f64 / 1e3)),
        _ => SharedString::from(format!("{:.1}M", count as f64 / 1e6)),
    }
}

/// Closes the subscription when the pane goes away.
impl Drop for VizPanel {
    fn drop(&mut self) {
        // Nothing to await on here, and the pipeline drops the fan-out when its
        // last subscriber goes, so a handle left behind costs one entry until
        // the connection closes. Logged rather than ignored so it is findable.
        if let Some(handle) = &self.subscription {
            tracing::debug!("pane closed with subscription {handle} still open");
        }
    }
}

/// Turns a stored pane back into one.
pub fn config_of(info: &gpui_component::dock::PanelInfo) -> Config {
    match info {
        gpui_component::dock::PanelInfo::Panel(value) => {
            serde_json::from_value(value.clone()).unwrap_or_default()
        }
        _ => Config::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pane_config_survives_a_round_trip() {
        let config = Config {
            connection: Some(3),
            topic: "/scan".into(),
            view: "plot".into(),
        };
        let json = serde_json::to_value(&config).expect("serialises");
        assert_eq!(
            config_of(&gpui_component::dock::PanelInfo::panel(json)),
            config
        );
    }

    #[test]
    fn a_pane_that_was_never_pointed_anywhere_restores_as_empty() {
        let empty = gpui_component::dock::PanelInfo::panel(serde_json::json!({}));
        assert_eq!(config_of(&empty), Config::default());
    }

    #[test]
    fn something_that_is_not_a_pane_config_restores_as_empty_rather_than_failing() {
        let wrong = gpui_component::dock::PanelInfo::panel(serde_json::json!("nonsense"));
        assert_eq!(config_of(&wrong), Config::default());
        assert_eq!(
            config_of(&gpui_component::dock::PanelInfo::tabs(0)),
            Config::default()
        );
    }
}
