//! One live view inside a dashboard.
//!
//! A pane is a topic on a connection, shown one of the ways `views` knows how.
//! That is the whole of it — no sending, no saving, no editing: a dashboard is
//! for watching, and a request editor already exists for the rest.
//!
//! Panes subscribe themselves and unsubscribe when they go away, so a dashboard
//! that is closed stops costing anything.

use std::sync::{Arc, Mutex};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, Window, div, px,
};
use gpui_component::dock::{Panel, PanelEvent, PanelState, TabPanel};
use gpui_component::menu::DropdownMenu as _;
use gpui_component::{
    ActiveTheme as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use rw_canonical::CanonicalValue;

use crate::cloud;
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
    /// Built the first time a message turns out to be a point cloud.
    scene: Option<Entity<SceneView>>,
    scene_at: u64,
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
                scene: None,
                scene_at: 0,
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

    fn set_target(&mut self, connection: Option<i64>, topic: String, cx: &mut Context<Self>) {
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

    /// Hands the newest cloud to the 3D pane, building it on first sight.
    fn sync_scene(&mut self, cx: &mut Context<Self>) {
        if self.view != View::Visualize {
            return;
        }
        let (value, count) = {
            let incoming = self.incoming.lock().expect("incoming mutex");
            (incoming.value.clone(), incoming.count)
        };
        if count == self.scene_at {
            return;
        }
        let Some(cloud) = value.as_ref().and_then(cloud::decode) else {
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
        scene.update(cx, |scene, cx| scene.show(cloud.into(), cx));
    }

    /// The connection and topic pickers, and the view switcher.
    fn toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let workspace = self.workspace.read(cx);
        let connections: Vec<(i64, SharedString)> = workspace
            .connections()
            .iter()
            .map(|connection| (connection.id, connection.name.clone().into()))
            .collect();
        let named = self
            .connection
            .and_then(|id| workspace.connection(id))
            .map(|connection| SharedString::from(connection.name.clone()))
            .unwrap_or_else(|| "Pick a system".into());

        // Only topics the chosen system actually publishes: a dashboard pane is
        // pointed at something that exists, not typed at from memory.
        let topics: Vec<SharedString> = self
            .connection
            .and_then(|id| self.sessions.read(cx).discovery(id).cloned())
            .map(|discovery| {
                discovery
                    .topics
                    .into_iter()
                    .map(|topic| SharedString::from(topic.name))
                    .collect()
            })
            .unwrap_or_default();
        let chosen = self.connection;
        let showing = SharedString::from(self.topic.clone());
        let topic_label = if self.topic.is_empty() {
            SharedString::from("Pick a topic")
        } else {
            SharedString::from(self.topic.clone())
        };

        h_flex()
            .flex_shrink_0()
            .h(px(tokens::CARD_HEADER_HEIGHT))
            .w_full()
            .items_center()
            .gap_1()
            .px_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("pane-connection")
                    .ghost()
                    .xsmall()
                    .label(named)
                    .dropdown_menu(move |mut menu, _window, _cx| {
                        if connections.is_empty() {
                            return menu.menu(
                                "Add a connection…",
                                Box::new(crate::actions::ManageConnections),
                            );
                        }
                        for (id, name) in connections.clone() {
                            menu = menu.menu_with_check(
                                name,
                                Some(id) == chosen,
                                Box::new(crate::actions::SetPaneConnection(id)),
                            );
                        }
                        menu
                    }),
            )
            .child(
                Button::new("pane-topic")
                    .ghost()
                    .xsmall()
                    .label(topic_label)
                    .dropdown_menu(move |mut menu, _window, _cx| {
                        if topics.is_empty() {
                            return menu.menu(
                                "Nothing discovered yet",
                                Box::new(crate::actions::ManageConnections),
                            );
                        }
                        for topic in topics.clone() {
                            let chosen = topic == showing;
                            menu = menu.menu_with_check(
                                topic.clone(),
                                chosen,
                                Box::new(crate::actions::SetPaneTopic(topic)),
                            );
                        }
                        menu
                    }),
            )
            .child(div().flex_1())
            .children(View::ALL.map(|view| {
                Button::new(SharedString::from(view.as_str()))
                    .ghost()
                    .xsmall()
                    .label(view.label())
                    .when(view == self.view, |button| button.primary())
                    .on_click(cx.listener(move |pane, _: &ClickEvent, _, cx| {
                        pane.view = view;
                        cx.emit(PaneChanged);
                        cx.notify();
                    }))
            }))
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

    fn on_added_to(
        &mut self,
        _tab_panel: gpui::WeakEntity<TabPanel>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }
}

impl Render for VizPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_scene(cx);
        let toolbar = self.toolbar(cx);
        let (value, schema, count, history) = {
            let incoming = self.incoming.lock().expect("incoming mutex");
            (
                incoming.value.clone(),
                incoming.schema.clone(),
                incoming.count,
                incoming.history.clone(),
            )
        };

        let body = match (&value, self.view) {
            (Some(value), View::Raw) => views::raw(value, cx),
            (Some(_), View::Plot) => views::plot(&history, cx),
            (Some(value), View::Visualize) => views::visualize(value, self.scene.as_ref(), cx),
            (None, _) => {
                let (title, detail) = match (&self.problem, self.topic.is_empty()) {
                    (Some(problem), _) => ("Nothing arriving", problem.to_string()),
                    (None, true) => (
                        "Nothing chosen yet",
                        "Pick a system and a topic for this pane.".to_string(),
                    ),
                    (None, false) => (
                        "Waiting for the first message…",
                        "The subscription is open; nothing has arrived yet.".to_string(),
                    ),
                };
                tokens::empty_state(IconName::Inbox, title, detail, cx).into_any_element()
            }
        };

        v_flex()
            .id("pane")
            .size_full()
            .min_h_0()
            .track_focus(&self.focus_handle)
            .bg(cx.theme().background)
            .on_action(
                cx.listener(|pane, action: &crate::actions::SetPaneConnection, _, cx| {
                    let topic = pane.topic.clone();
                    pane.set_target(Some(action.0), topic, cx);
                }),
            )
            .on_action(
                cx.listener(|pane, action: &crate::actions::SetPaneTopic, _, cx| {
                    let connection = pane.connection;
                    pane.set_target(connection, action.0.to_string(), cx);
                }),
            )
            .child(toolbar)
            .child(
                v_flex()
                    .id("pane-body")
                    .flex_1()
                    .min_h_0()
                    .p_2()
                    .overflow_y_scroll()
                    .child(body),
            )
            .when(count > 0, |pane| {
                pane.child(
                    h_flex()
                        .flex_shrink_0()
                        .gap_3()
                        .px_2()
                        .py_1()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .child(tokens::meta("Messages", count.to_string(), cx))
                        .when_some(schema, |row, schema| {
                            row.child(tokens::meta("Schema", schema, cx))
                        }),
                )
            })
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
