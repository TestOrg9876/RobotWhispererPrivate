//! Request panel: pick a connection and a topic, subscribe, and watch the
//! decoded value stream in.
//!
//! The pipeline hands us a `rw_transport::Frame` containing a `CanonicalValue`,
//! so there is no envelope to unpack and no decode step in the UI.

use std::sync::{Arc, Mutex};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, Window, div,
};
use gpui_component::dock::{Panel, PanelEvent, PanelState};
use gpui_component::{
    ActiveTheme as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    label::Label,
    v_flex,
};
use rw_canonical::CanonicalValue;

use crate::session::{RobotWhisperer, Sessions};
use crate::value;

/// Latest frame received for the current subscription.
#[derive(Default)]
struct Latest {
    value: Option<CanonicalValue>,
    schema: Option<SharedString>,
    count: u64,
}

pub struct RequestPanel {
    focus_handle: FocusHandle,
    sessions: Entity<Sessions>,
    /// Which stored request this panel edits, if any.
    request_id: Option<i64>,
    title: SharedString,
    connection: Option<i64>,
    topic: Entity<InputState>,
    /// Written by the subscription callback, which the pipeline may invoke
    /// from a transport thread, and read while rendering. `Arc<Mutex<_>>` rather
    /// than `Rc<RefCell<_>>` because `subscribe_topic` requires `Send` on native.
    latest: Arc<Mutex<Latest>>,
    subscription: Option<String>,
    error: Option<SharedString>,
    _poll: Option<Task<()>>,
}

impl EventEmitter<PanelEvent> for RequestPanel {}

impl RequestPanel {
    pub fn new(
        request_id: Option<i64>,
        title: impl Into<SharedString>,
        connection: Option<i64>,
        topic: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let sessions = RobotWhisperer::global(cx).sessions.clone();
        let topic_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("/topic")
                .default_value(topic)
        });

        cx.observe(&sessions, |_, _, cx| cx.notify()).detach();

        Self {
            focus_handle: cx.focus_handle(),
            sessions,
            request_id,
            title: title.into(),
            connection,
            topic: topic_input,
            latest: Arc::new(Mutex::new(Latest::default())),
            subscription: None,
            error: None,
            _poll: None,
        }
    }

    pub fn view(
        request_id: Option<i64>,
        title: impl Into<SharedString>,
        connection: Option<i64>,
        topic: &str,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| Self::new(request_id, title, connection, topic, window, cx))
    }

    pub fn request_id(&self) -> Option<i64> {
        self.request_id
    }

    fn running(&self) -> bool {
        self.subscription.is_some()
    }

    /// Subscribes to the topic on the selected connection.
    fn start(&mut self, cx: &mut Context<Self>) {
        let topic = self.topic.read(cx).value().trim().to_string();
        if topic.is_empty() {
            self.error = Some("Enter a topic first".into());
            cx.notify();
            return;
        }

        let Some(connection) = self.connection else {
            self.error = Some("Pick a connection first".into());
            cx.notify();
            return;
        };
        let Some(session) = self.sessions.read(cx).session(connection) else {
            self.error = Some("That connection is not connected".into());
            cx.notify();
            return;
        };

        self.error = None;
        *self.latest.lock().expect("latest frame mutex poisoned") = Latest::default();

        let pipeline = self.sessions.read(cx).pipeline();
        let latest = Arc::clone(&self.latest);

        cx.spawn(async move |panel, cx| {
            let outcome = pipeline
                .subscribe_topic(session, &topic, move |_handle, frame, _lossy| {
                    let Ok(mut latest) = latest.lock() else {
                        return;
                    };
                    latest.value = Some(frame.value.clone());
                    latest.schema = Some(frame.schema.name.clone().into());
                    latest.count += 1;
                })
                .await;

            panel
                .update(cx, |panel, cx| {
                    match outcome {
                        Ok(result) => {
                            panel.subscription = Some(result.subscription_id);
                            panel.start_polling(cx);
                        }
                        Err(error) => panel.error = Some(error.to_string().into()),
                    }
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    /// Frames arrive on the transport's thread; repaint on a timer rather than
    /// per frame so a 1 kHz topic cannot drive 1 kHz of layout.
    fn start_polling(&mut self, cx: &mut Context<Self>) {
        self._poll = Some(cx.spawn(async move |panel, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(100))
                    .await;
                if panel.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(subscription) = self.subscription.take() else {
            return;
        };
        self._poll = None;
        let pipeline = self.sessions.read(cx).pipeline();

        cx.spawn(async move |panel, cx| {
            let outcome = pipeline.unsubscribe(&subscription).await;
            panel
                .update(cx, |panel, cx| {
                    if let Err(error) = outcome {
                        panel.error = Some(error.to_string().into());
                    }
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    fn connection_label(&self, cx: &App) -> SharedString {
        let Some(connection) = self.connection else {
            return "No connection".into();
        };
        RobotWhisperer::global(cx)
            .workspace
            .read(cx)
            .connection(connection)
            .map(|entry| SharedString::from(entry.name.clone()))
            .unwrap_or_else(|| "Unknown connection".into())
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let running = self.running();

        h_flex()
            .gap_2()
            .items_center()
            .child(Label::new(self.connection_label(cx)).text_sm())
            .child(div().flex_1().child(Input::new(&self.topic)))
            .child(
                Button::new("toggle-subscription")
                    .small()
                    .when(running, |button| {
                        button.danger().icon(IconName::Close).label("Stop")
                    })
                    .when(!running, |button| {
                        button.primary().icon(IconName::Play).label("Subscribe")
                    })
                    .on_click(cx.listener(|panel, _: &ClickEvent, _, cx| {
                        if panel.running() {
                            panel.stop(cx);
                        } else {
                            panel.start(cx);
                        }
                    })),
            )
            .into_any_element()
    }
}

impl Focusable for RequestPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for RequestPanel {
    fn panel_name(&self) -> &'static str {
        "Request"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.title.clone()
    }

    fn dump(&self, _cx: &App) -> PanelState {
        PanelState::new(self)
    }
}

impl Render for RequestPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let toolbar = self.toolbar(cx);
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let mono = cx.theme().mono_font_family.clone();

        let latest = self.latest.lock().expect("latest frame mutex poisoned");
        let body = match &latest.value {
            Some(value) => div()
                .font_family(mono)
                .text_xs()
                .child(value::preview(value))
                .into_any_element(),
            None => div()
                .text_sm()
                .text_color(muted)
                .child(if self.running() {
                    "Subscribed. Waiting for the first message…"
                } else {
                    "Not subscribed."
                })
                .into_any_element(),
        };

        let status = format!(
            "{} messages{}",
            latest.count,
            latest
                .schema
                .as_ref()
                .map(|schema| format!(" · {schema}"))
                .unwrap_or_default()
        );

        v_flex()
            .id("request-panel")
            .size_full()
            .gap_2()
            .p_3()
            .child(toolbar)
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_xs().text_color(danger).child(error))
            })
            .child(div().text_xs().text_color(muted).child(status))
            .child(
                div()
                    .id("request-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_scroll()
                    .child(body),
            )
    }
}
