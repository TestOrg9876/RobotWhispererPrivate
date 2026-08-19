//! The request editor: a saved, named, reusable call.
//!
//! Layout, top to bottom: the request's name and kind, then the request bar
//! (kind, target, environment, send), then the payload form for services and
//! actions, then the response.

use std::sync::{Arc, Mutex};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window,
    deferred, div, px,
};
use gpui_component::chart::LineChart;
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::DropdownMenu as _,
    tab::{Tab, TabBar},
    v_flex,
};
use rw_canonical::CanonicalValue;
use rw_core::domain::{Request, RequestKind, Value};
use rw_transport::ConnectionId;

use crate::discovery::{self, Suggestion};
use crate::docking::Home;
use crate::form::{self, Field};
use crate::image;
use crate::runs::{RunState, Runs};
use crate::series::{History, Series};
use crate::session::{RobotWhisperer, Sessions};
use crate::tokens;
use crate::value;
use crate::workspace::Workspace;

/// Written by the subscription callback, read while rendering. `Arc<Mutex<_>>`
/// because `subscribe_topic` requires `Send` on native.
#[derive(Default)]
struct Incoming {
    value: Option<CanonicalValue>,
    schema: Option<SharedString>,
    count: u64,
    /// Numeric fields over time, for the plot. Accumulated as messages land
    /// rather than derived at render: by then the earlier ones are gone.
    history: History,
}

/// Why the request cannot run, and what the user can do about it.
///
/// A bare message is not enough here: the commonest reason a request will not
/// send is an environment that exists but is not connected, and telling someone
/// that without offering the one-click fix is just a scolding.
struct Problem {
    message: SharedString,
    /// The environment to connect, when connecting is the fix.
    connect: Option<i64>,
}

impl Problem {
    fn new(message: impl Into<SharedString>) -> Self {
        Self {
            message: message.into(),
            connect: None,
        }
    }

    fn connecting(connection: i64, message: impl Into<SharedString>) -> Self {
        Self {
            message: message.into(),
            connect: Some(connection),
        }
    }
}

/// Which response view is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseTab {
    Raw,
    Visualize,
    Plot,
}

impl ResponseTab {
    const ALL: [Self; 3] = [Self::Raw, Self::Visualize, Self::Plot];

    fn label(self) -> &'static str {
        match self {
            Self::Raw => "Raw",
            Self::Visualize => "Visualize",
            Self::Plot => "Plot",
        }
    }

    /// Position in [`Self::ALL`], which is what `TabBar` selects by.
    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|tab| *tab == self)
            .expect("every variant is listed in ALL")
    }
}

/// Change the request's kind. Carries the discriminant so one action serves the
/// whole menu.
#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct SetKind(pub u8);

/// Point the request at a stored environment.
#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = robot_whisperer, no_json)]
pub struct UseEnvironment(pub i64);

fn kind_from_discriminant(value: u8) -> RequestKind {
    match value {
        1 => RequestKind::Service,
        2 => RequestKind::Action,
        _ => RequestKind::Topic,
    }
}

fn discriminant_of(kind: RequestKind) -> u8 {
    match kind {
        RequestKind::Topic => 0,
        RequestKind::Service => 1,
        RequestKind::Action => 2,
    }
}

/// What the request currently has in flight.
///
/// One enum rather than a set of `Option`s: a request is subscribed *or*
/// calling *or* running a goal, never two at once, and the Send button's label,
/// its variant and what Stop does all follow from this.
#[derive(Debug, Default)]
enum Activity {
    #[default]
    Idle,
    /// A topic subscription, identified for unsubscribing.
    Subscribed(String),
    /// A service call, awaiting its response.
    Calling,
    /// An action goal, identified for cancelling.
    Goal(String),
}

impl Activity {
    fn is_idle(&self) -> bool {
        matches!(self, Activity::Idle)
    }

    /// What the primary button says while this is happening.
    fn stop_label(&self) -> &'static str {
        match self {
            Activity::Subscribed(_) => "Stop",
            Activity::Calling => "Calling…",
            Activity::Goal(_) => "Cancel",
            Activity::Idle => "Stop",
        }
    }
}

pub struct RequestPanel {
    focus_handle: FocusHandle,
    workspace: Entity<Workspace>,
    sessions: Entity<Sessions>,

    /// What is stored, and the edited copy. `dirty` compares them.
    saved: Request,
    draft: Request,

    name: Entity<InputState>,
    target: Entity<InputState>,

    /// Whether the target field's offer list is showing, and which row the
    /// keyboard is on. The offers themselves are derived rather than stored —
    /// discovery arrives asynchronously, and a cached list was simply empty
    /// whenever it landed after the field was focused.
    highlighted: usize,
    offers_open: bool,

    /// The payload form: one input per leaf of the request or goal message,
    /// rebuilt whenever the schema behind the target changes.
    payload: Vec<(Field, Entity<InputState>)>,
    /// The schema the current form was built from, so it is only rebuilt when
    /// it actually changes rather than on every render.
    payload_schema: Option<String>,

    incoming: Arc<Mutex<Incoming>>,
    activity: Activity,
    /// Shared, so the sidebar can show what this request is doing.
    runs: Entity<Runs>,
    tab: ResponseTab,
    problem: Option<Problem>,
    /// The tab group this editor is sitting in, which changes whenever the user
    /// drags its tab somewhere else.
    home: Home,
    _repaint: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PanelEvent> for RequestPanel {}

impl RequestPanel {
    pub fn new(request: &Request, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (workspace, sessions, runs) = {
            let global = RobotWhisperer::global(cx);
            (
                global.workspace.clone(),
                global.sessions.clone(),
                global.runs.clone(),
            )
        };

        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Request name")
                .default_value(&request.name)
        });
        let target = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("/topic, /service or /action")
                .default_value(&request.target)
        });

        let subscriptions = vec![
            cx.observe(&sessions, |_, _, cx| cx.notify()),
            cx.subscribe(&name, |this, state, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.draft.name = state.read(cx).value().to_string();
                    cx.notify();
                }
            }),
            cx.subscribe_in(
                &target,
                window,
                |this, state, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        this.draft.target = state.read(cx).value().to_string();
                        this.offers_open = true;
                        cx.notify();
                    }
                    InputEvent::Focus => {
                        this.offers_open = true;
                        cx.notify();
                    }
                    InputEvent::Blur => {
                        this.offers_open = false;
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => {
                        // Enter takes the highlighted offer while the list is
                        // showing, and otherwise means "run this request" — one
                        // key doing the obvious thing in both states.
                        if !this.accept_offer(window, cx) {
                            this.start(cx);
                        }
                    }
                },
            ),
        ];

        Self {
            focus_handle: cx.focus_handle(),
            workspace,
            sessions,
            saved: request.clone(),
            draft: request.clone(),
            name,
            target,
            highlighted: 0,
            offers_open: false,
            payload: Vec::new(),
            payload_schema: None,
            incoming: Arc::new(Mutex::new(Incoming::default())),
            activity: Activity::default(),
            runs,
            tab: ResponseTab::Raw,
            problem: None,
            home: Home::default(),
            _repaint: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn view(request: &Request, window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self::new(request, window, cx))
    }

    pub fn request_id(&self) -> Option<i64> {
        Some(self.saved.id)
    }

    pub fn kind(&self) -> RequestKind {
        self.draft.kind
    }

    /// Tab label: the request's name, or its target while unnamed.
    pub fn title(&self) -> SharedString {
        let name = self.draft.name.trim();
        if !name.is_empty() {
            return name.to_string().into();
        }
        let target = self.draft.target.trim();
        if target.is_empty() {
            "Untitled".into()
        } else {
            target.to_string().into()
        }
    }

    /// True while the draft differs from what is stored.
    /// The tab group this editor is in, so the shell can reveal or close it
    /// after the user has dragged it into a pane of their own making.
    pub fn home(&self) -> Option<gpui::WeakEntity<gpui_component::dock::TabPanel>> {
        self.home.tab_panel()
    }

    pub fn dirty(&self) -> bool {
        self.draft.name != self.saved.name
            || self.draft.target != self.saved.target
            || self.draft.kind != self.saved.kind
            || self.draft.connection_id != self.saved.connection_id
            || self.draft.input != self.saved.input
    }

    // ── discovery offers ───────────────────────────────────────────────────────

    /// How many offers fit without the list swallowing the response.
    const MAX_OFFERS: usize = 8;

    /// What discovery has to offer for the target as it currently reads.
    ///
    /// Discovery replaces the connections tree: rather than browsing a robot's
    /// topics somewhere else and copying a name across, the field that needs the
    /// name offers it.
    ///
    /// Derived rather than stored. Discovery arrives asynchronously, so a cached
    /// list was empty whenever it landed after the field was focused, and every
    /// edit to the request became another place that had to remember to refresh
    /// it.
    fn offers(&self, cx: &App) -> Vec<Suggestion> {
        if !self.offers_open {
            return Vec::new();
        }
        self.draft
            .connection_id
            .and_then(|id| {
                self.sessions.read(cx).discovery(id).map(|discovery| {
                    discovery::suggestions(
                        discovery,
                        self.draft.kind,
                        &self.draft.target,
                        Self::MAX_OFFERS,
                    )
                })
            })
            .unwrap_or_default()
    }

    /// The row the keyboard is on, clamped to what is actually offered — the
    /// list changes under the highlight as the query is typed.
    fn highlighted(&self, offers: &[Suggestion]) -> usize {
        self.highlighted.min(offers.len().saturating_sub(1))
    }

    /// Takes the highlighted offer. Returns false when there was nothing to take,
    /// so the caller can fall back to whatever the key normally does.
    fn accept_offer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let offers = self.offers(cx);
        let Some(offer) = offers.get(self.highlighted(&offers)).cloned() else {
            return false;
        };
        self.set_target(&offer.name, window, cx);
        true
    }

    fn set_target(&mut self, target: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.draft.target = target.to_string();
        self.offers_open = false;
        self.highlighted = 0;

        let value = target.to_string();
        self.target
            .update(cx, |state, cx| state.set_value(value, window, cx));
        cx.notify();
    }

    fn move_highlight(&mut self, delta: isize, cx: &mut Context<Self>) {
        let offers = self.offers(cx);
        if offers.is_empty() {
            return;
        }
        let from = self.highlighted(&offers) as isize;
        self.highlighted = (from + delta).rem_euclid(offers.len() as isize) as usize;
        cx.notify();
    }

    fn running(&self) -> bool {
        !self.activity.is_idle()
    }

    /// Records what this request is doing where the sidebar can see it.
    ///
    /// Called from the one place `activity` changes rather than beside each
    /// assignment, so the two cannot drift apart.
    fn publish_state(&mut self, cx: &mut Context<Self>) {
        let state = match (&self.activity, &self.problem) {
            (_, Some(problem)) => RunState::Failed(problem.message.clone()),
            (activity, None) if !activity.is_idle() => RunState::Live,
            _ => RunState::Idle,
        };
        let id = self.saved.id;
        self.runs.update(cx, |runs, cx| runs.set(id, state, cx));
    }

    /// Sets the activity and publishes it in one step.
    fn set_activity(&mut self, activity: Activity, cx: &mut Context<Self>) {
        self.activity = activity;
        self.publish_state(cx);
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        // The form is the truth for the payload, so it is collected before the
        // comparison rather than tracked field by field.
        if let Ok(payload) = self.payload_value(cx) {
            self.draft.input = payload;
        }
        if !self.dirty() {
            return;
        }
        self.saved = self.draft.clone();
        let request = self.draft.clone();

        self.workspace
            .update(cx, |workspace, cx| workspace.save_request(request, cx))
            .detach();
        cx.notify();
    }

    // ── running ────────────────────────────────────────────────────────────────

    fn session(&self, cx: &App) -> Option<rw_transport::ConnectionId> {
        self.draft
            .connection_id
            .and_then(|id| self.sessions.read(cx).session(id))
    }
    /// Runs the request, in whichever way its kind means.
    fn start(&mut self, cx: &mut Context<Self>) {
        let target = self.draft.target.trim().to_string();
        if target.is_empty() {
            self.problem = Some(Problem::new("Enter a target first"));
            cx.notify();
            return;
        }
        let Some(session) = self.session(cx) else {
            self.problem = Some(self.why_not_connected(cx));
            cx.notify();
            return;
        };

        let payload = match self.payload_value(cx) {
            Ok(payload) => payload,
            Err(error) => {
                self.problem = Some(Problem::new(error));
                cx.notify();
                return;
            }
        };

        self.problem = None;
        self.publish_state(cx);
        *self.incoming.lock().expect("incoming mutex") = Incoming::default();
        self.start_repaint(cx);

        match self.draft.kind {
            RequestKind::Topic => self.subscribe(session, target, cx),
            RequestKind::Service => self.call(session, target, payload, cx),
            RequestKind::Action => self.send_goal(session, target, payload, cx),
        }
        cx.notify();
    }

    /// Sends the payload to the topic, once.
    ///
    /// Separate from `start`, because publishing is not a thing you stop: it
    /// happens and it is over, and a request can be subscribed while it does.
    fn publish(&mut self, cx: &mut Context<Self>) {
        let target = self.draft.target.trim().to_string();
        if target.is_empty() {
            self.problem = Some(Problem::new("Enter a target first"));
            cx.notify();
            return;
        }
        let Some(session) = self.session(cx) else {
            self.problem = Some(self.why_not_connected(cx));
            cx.notify();
            return;
        };
        let message = match self.payload_value(cx) {
            Ok(message) => message,
            Err(error) => {
                self.problem = Some(Problem::new(error));
                cx.notify();
                return;
            }
        };

        self.problem = None;
        self.publish_state(cx);
        let pipeline = self.sessions.read(cx).pipeline();

        cx.spawn(async move |panel, cx| {
            let outcome = pipeline.publish(session, &target, message.into()).await;
            panel
                .update(cx, |panel, cx| {
                    match outcome {
                        Ok(()) => panel.say(format!("published to {target}"), cx),
                        Err(error) => panel.failed(error, cx),
                    }
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    /// Puts a line in the console, which is where this app says things.
    fn say(&self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.sessions.update(cx, |sessions, cx| {
            sessions.announce(crate::session::Notice::Info(message.into()), cx)
        });
    }

    fn subscribe(&mut self, session: ConnectionId, target: String, cx: &mut Context<Self>) {
        let pipeline = self.sessions.read(cx).pipeline();
        let incoming = Arc::clone(&self.incoming);

        cx.spawn(async move |panel, cx| {
            let outcome = pipeline
                .subscribe_topic(session, &target, move |_handle, frame, _lossy| {
                    let Ok(mut incoming) = incoming.lock() else {
                        return;
                    };
                    incoming.history.observe(&frame.value);
                    incoming.value = Some(frame.value.clone());
                    incoming.schema = Some(frame.schema.name.clone().into());
                    incoming.count += 1;
                })
                .await;

            panel
                .update(cx, |panel, cx| {
                    match outcome {
                        Ok(result) => {
                            panel.set_activity(Activity::Subscribed(result.subscription_id), cx)
                        }
                        Err(error) => panel.failed(error, cx),
                    }
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    fn call(
        &mut self,
        session: ConnectionId,
        target: String,
        request: Value,
        cx: &mut Context<Self>,
    ) {
        let pipeline = self.sessions.read(cx).pipeline();
        self.set_activity(Activity::Calling, cx);

        cx.spawn(async move |panel, cx| {
            let outcome = pipeline
                .call_service(session, &target, request.into())
                .await;

            panel
                .update(cx, |panel, cx| {
                    match outcome {
                        Ok(response) => {
                            let mut incoming = panel.incoming.lock().expect("incoming mutex");
                            incoming.history.observe(&response);
                            incoming.value = Some(response);
                            incoming.count += 1;
                        }
                        Err(error) => panel.failed(error, cx),
                    }
                    // A call is over the moment it answers; there is nothing to
                    // stop afterwards.
                    panel.set_activity(Activity::Idle, cx);
                    panel._repaint = None;
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    /// Sends a goal and follows it: feedback replaces the shown value as it
    /// arrives, and the result replaces it once.
    fn send_goal(
        &mut self,
        session: ConnectionId,
        target: String,
        goal: Value,
        cx: &mut Context<Self>,
    ) {
        let pipeline = self.sessions.read(cx).pipeline();
        let incoming = Arc::clone(&self.incoming);

        cx.spawn(async move |panel, cx| {
            let mut stream = match pipeline
                .send_action_goal(session, &target, goal.into())
                .await
            {
                Ok(stream) => stream,
                Err(error) => {
                    panel
                        .update(cx, |panel, cx| {
                            panel.failed(error, cx);
                            panel.set_activity(Activity::Idle, cx);
                            cx.notify();
                        })
                        .ok();
                    return;
                }
            };

            let goal_id = stream.cancel_token.goal_id.clone();
            if panel
                .update(cx, |panel, cx| {
                    panel.set_activity(Activity::Goal(goal_id.clone()), cx);
                    cx.notify();
                })
                .is_err()
            {
                return;
            }

            // Feedback and the result arrive on separate channels. Draining
            // feedback first and then awaiting the result is safe because the
            // feedback channel closes when the goal finishes.
            while let Some(feedback) = stream.feedback.recv().await {
                let Ok(mut incoming) = incoming.lock() else {
                    break;
                };
                incoming.history.observe(&feedback);
                incoming.value = Some(feedback);
                incoming.count += 1;
            }

            let result = stream.result.await;
            panel
                .update(cx, |panel, cx| {
                    match result {
                        Ok(Ok(value)) => {
                            let mut incoming = panel.incoming.lock().expect("incoming mutex");
                            incoming.history.observe(&value);
                            incoming.value = Some(value);
                            incoming.count += 1;
                        }
                        Ok(Err(error)) => panel.failed(error, cx),
                        // The sender was dropped: the goal was cancelled, or the
                        // transport went away. Neither is worth an error banner.
                        Err(_) => {}
                    }
                    panel.set_activity(Activity::Idle, cx);
                    panel._repaint = None;
                    cx.notify();
                })
                .ok();

            pipeline.forget_action_goal(&goal_id).await;
        })
        .detach();
    }

    // ── the payload form ───────────────────────────────────────────────────────

    /// The schema name the target implies, from discovery.
    ///
    /// Discovery is the only place that knows which schema a name carries, and
    /// the request stores it so a saved request still shows its form before the
    /// robot is reachable.
    fn payload_schema_name(&self, cx: &App) -> Option<String> {
        let target = self.draft.target.trim();
        if target.is_empty() {
            return self.draft.schema.as_ref().map(|schema| schema.name.clone());
        }

        let discovered = self.draft.connection_id.and_then(|id| {
            let sessions = self.sessions.read(cx);
            let discovery = sessions.discovery(id)?;
            let named = |entries: &[rw_transport::TargetDescriptor]| {
                entries
                    .iter()
                    .find(|entry| entry.name == target)
                    .map(|entry| entry.schema_name.clone())
            };
            match self.draft.kind {
                RequestKind::Topic => discovery
                    .topics
                    .iter()
                    .find(|topic| topic.name == target)
                    .map(|topic| topic.schema_name.clone()),
                RequestKind::Service => named(&discovery.services),
                RequestKind::Action => named(&discovery.actions),
            }
        });

        discovered.or_else(|| self.draft.schema.as_ref().map(|schema| schema.name.clone()))
    }

    /// Rebuilds the form when the schema behind the target changes.
    ///
    /// Called from `render` because the schema depends on discovery, the target
    /// text and the kind, and keeping three separate places in step with it was
    /// exactly the bug the derived offer list already had.
    fn sync_payload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = self.payload_schema_name(cx);
        if wanted == self.payload_schema {
            return;
        }

        let fields = wanted
            .as_deref()
            .and_then(|name| self.message_for(name, cx))
            .unwrap_or_default();

        // Existing text survives a rebuild when the leaf is still there, so
        // reconnecting to a robot does not clear a half-filled form.
        let filled: Vec<(String, String)> = self
            .payload
            .iter()
            .map(|(field, input)| (field.path.clone(), input.read(cx).value().to_string()))
            .collect();

        self.payload = fields
            .into_iter()
            .map(|field| {
                let existing = filled
                    .iter()
                    .find(|(path, _)| *path == field.path)
                    .map(|(_, text)| text.clone())
                    .or_else(|| form::text_at(&self.draft.input, &field.path, field.editor));
                let placeholder = field.editor.placeholder();
                let input = cx.new(|cx| {
                    let state = InputState::new(window, cx).placeholder(placeholder);
                    match existing {
                        Some(text) => state.default_value(text),
                        None => state,
                    }
                });
                (field, input)
            })
            .collect();
        self.payload_schema = wanted;
    }

    /// The message definition a form should be built from: a service's request
    /// or an action's goal.
    fn message_for(&self, name: &str, cx: &App) -> Option<Vec<Field>> {
        let pipeline = self.sessions.read(cx).pipeline();
        let registry = pipeline.schema_registry()?.clone();
        let definition = registry.get_by_name(name).into_iter().next()?;

        let message = match &definition.parsed {
            rw_core::schema::ParsedSchema::Service { request, .. } => request,
            rw_core::schema::ParsedSchema::Action { goal, .. } => goal,
            rw_core::schema::ParsedSchema::Message(message) => message,
        };

        let lookup = move |type_name: &str| {
            registry
                .get_by_name(type_name)
                .into_iter()
                .next()
                .map(|definition| definition.parsed.primary().clone())
        };
        Some(form::fields(message, &lookup))
    }

    /// The payload the form currently describes.
    ///
    /// Topics carry nothing, so they get an empty message rather than a special
    /// case at every call site.
    fn payload_value(&self, cx: &App) -> Result<Value, String> {
        let mut leaves = Vec::new();
        for (field, input) in &self.payload {
            match form::parse(field.editor, &input.read(cx).value()) {
                Ok(Some(value)) => leaves.push((field.path.clone(), value)),
                Ok(None) => {}
                Err(reason) => return Err(format!("{}: {reason}", field.path)),
            }
        }
        Ok(form::assemble(leaves))
    }

    fn failed(&mut self, error: impl std::fmt::Display, cx: &mut Context<Self>) {
        self.problem = Some(Problem::new(error.to_string()));
        self.publish_state(cx);
        cx.notify();
    }

    /// Distinguishes "no environment chosen" from "chosen but not connected",
    /// because only the second one has a fix the panel can offer.
    fn why_not_connected(&self, cx: &App) -> Problem {
        let Some(id) = self.draft.connection_id else {
            return Problem::new("Choose an environment for this request");
        };
        match self.workspace.read(cx).connection(id) {
            Some(connection) => {
                Problem::connecting(id, format!("{} is not connected", connection.name))
            }
            None => Problem::new("This request points at an environment that no longer exists"),
        }
    }

    fn connect(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(connection) = self.workspace.read(cx).connection(id).cloned() else {
            return;
        };
        self.problem = None;
        self.sessions
            .update(cx, |sessions, cx| sessions.connect(&connection, cx))
            .detach();
        cx.notify();
    }

    /// Frames land on a transport thread. Repaint on a timer rather than per
    /// frame, so a 1 kHz topic cannot drive 1 kHz of layout.
    fn start_repaint(&mut self, cx: &mut Context<Self>) {
        self._repaint = Some(cx.spawn(async move |panel, cx| {
            loop {
                // Not `background_executor().timer`: that panics on wasm. See
                // `crate::tick`.
                crate::tick::sleep(std::time::Duration::from_millis(100), cx).await;
                if panel.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));
    }
    /// Stops whatever is in flight. A call in progress cannot be stopped — the
    /// service will answer or the transport will fail — so it is left alone.
    fn stop(&mut self, cx: &mut Context<Self>) {
        let pipeline = self.sessions.read(cx).pipeline();

        let task = match std::mem::take(&mut self.activity) {
            Activity::Subscribed(subscription) => cx.spawn(async move |panel, cx| {
                let outcome = pipeline.unsubscribe(&subscription).await;
                panel
                    .update(cx, |panel, cx| {
                        if let Err(error) = outcome {
                            panel.failed(error, cx);
                        }
                    })
                    .ok();
            }),
            Activity::Goal(goal_id) => cx.spawn(async move |panel, cx| {
                let outcome = pipeline.cancel_action_goal(&goal_id).await;
                panel
                    .update(cx, |panel, cx| {
                        if let Err(error) = outcome {
                            panel.failed(error, cx);
                        }
                    })
                    .ok();
            }),
            other => {
                // Nothing to stop; put back what was there.
                self.activity = other;
                return;
            }
        };

        self._repaint = None;
        self.publish_state(cx);
        task.detach();
        cx.notify();
    }

    // ── chrome ─────────────────────────────────────────────────────────────────

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let dirty = self.dirty();

        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .font_semibold()
                    // The name is the largest text in the window: 20px, per the
                    // type scale. `Input` derives its own size, so it is asked
                    // rather than styled from outside.
                    .child(Input::new(&self.name).appearance(false).with_size(px(23.))),
            )
            .when(dirty, |row| {
                row.child(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .child(div().size(px(6.)).rounded_full().bg(cx.theme().warning))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Unsaved"),
                        ),
                )
            })
            .child(
                Button::new("save")
                    .ghost()
                    .small()
                    .icon(IconName::Check)
                    .label("Save")
                    .disabled(!dirty)
                    .tooltip("Save request")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.save(cx))),
            )
            .into_any_element()
    }

    /// The prominent row: kind, target, environment, primary action.
    fn request_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let offers = self.offers(cx);
        let kind = self.draft.kind;
        let running = self.running();
        let kind_colour = tokens::kind_color(kind, cx);

        let connection = self
            .draft
            .connection_id
            .and_then(|id| {
                self.workspace
                    .read(cx)
                    .connection(id)
                    .map(|connection| connection.name.clone())
            })
            .unwrap_or_else(|| "Connection".to_string());

        let connections: Vec<_> = self
            .workspace
            .read(cx)
            .connections()
            .iter()
            .map(|connection| (connection.id, connection.name.clone()))
            .collect();
        let chosen = self.draft.connection_id;

        h_flex()
            .relative()
            .h(px(tokens::REQUEST_BAR_HEIGHT))
            .w_full()
            .items_center()
            .gap_2()
            .px_2()
            .rounded(cx.theme().radius)
            .bg(cx.theme().secondary)
            .border_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("kind")
                    .ghost()
                    .small()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_center()
                            .child(
                                div()
                                    .text_xs()
                                    .font_medium()
                                    .text_color(kind_colour)
                                    .child(tokens::kind_label(kind)),
                            )
                            .child(Icon::new(IconName::ChevronDown).xsmall()),
                    )
                    .dropdown_menu(move |mut menu, _window, _cx| {
                        for option in [
                            RequestKind::Topic,
                            RequestKind::Service,
                            RequestKind::Action,
                        ] {
                            menu = menu.menu_with_check(
                                tokens::kind_label(option),
                                option == kind,
                                Box::new(SetKind(discriminant_of(option))),
                            );
                        }
                        menu
                    }),
            )
            .child(div().w(px(1.)).h_5().bg(cx.theme().border))
            .child(
                div()
                    .id("target")
                    .flex_1()
                    .min_w_0()
                    // Clicking the field shows what the robot advertises, which
                    // is the whole replacement for browsing a connections tree.
                    // Focus alone is not enough to hang this on: GPUI only
                    // reports focus while the window is active, and a window
                    // manager is not something an app can assume.
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.offers_open = true;
                            cx.notify();
                        }),
                    )
                    .child(Input::new(&self.target).appearance(false)),
            )
            .child(
                Button::new("connection")
                    .ghost()
                    .small()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_center()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(connection),
                            )
                            .child(Icon::new(IconName::ChevronDown).xsmall()),
                    )
                    .dropdown_menu(move |mut menu, _window, _cx| {
                        if connections.is_empty() {
                            return menu.menu(
                                "Add a connection…",
                                Box::new(crate::actions::ManageConnections),
                            );
                        }
                        for (id, name) in &connections {
                            menu = menu.menu_with_check(
                                name.clone(),
                                chosen == Some(*id),
                                Box::new(UseEnvironment(*id)),
                            );
                        }
                        menu
                    }),
            )
            .child(
                Button::new("send")
                    .when(running, |button| {
                        button
                            .danger()
                            .icon(IconName::Pause)
                            .label(self.activity.stop_label())
                            .disabled(matches!(self.activity, Activity::Calling))
                    })
                    .when(!running, |button| {
                        button.primary().icon(IconName::Play).label(match kind {
                            RequestKind::Topic => "Subscribe",
                            RequestKind::Service => "Call",
                            RequestKind::Action => "Send goal",
                        })
                    })
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        if this.running() {
                            this.stop(cx);
                        } else {
                            this.start(cx);
                        }
                    })),
            )
            // Publishing sits beside subscribing rather than replacing it: the
            // same saved request is how you watch a topic *and* how you drive
            // it, and needing two requests for one topic would be silly.
            .when(matches!(kind, RequestKind::Topic), |bar| {
                bar.child(
                    Button::new("publish")
                        .outline()
                        .icon(IconName::ArrowUp)
                        .label("Publish")
                        .tooltip("Send the message above to this topic, once")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.publish(cx))),
                )
            })
            .when(!offers.is_empty(), |bar| {
                bar.child(self.offer_list(&offers, cx))
            })
            .into_any_element()
    }

    /// The offers, floating under the request bar.
    ///
    /// Deferred so it paints above the response card rather than being clipped
    /// by it, and absolutely positioned so opening it does not push the layout
    /// down — the same shape a browser's address bar uses.
    fn offer_list(&self, offers: &[Suggestion], cx: &mut Context<Self>) -> AnyElement {
        let highlighted = self.highlighted(offers);

        let rows = offers.iter().enumerate().map(|(index, offer)| {
            let name = offer.name.clone();
            h_flex()
                .id(("offer", index))
                .h(px(tokens::CONTROL_HEIGHT))
                .w_full()
                .px_3()
                .gap_3()
                .items_center()
                .justify_between()
                .rounded(cx.theme().radius)
                .when(index == highlighted, |row| row.bg(cx.theme().list_active))
                .when(index != highlighted, |row| {
                    row.hover(|row| row.bg(cx.theme().list_hover))
                })
                .child(
                    tokens::mono(cx)
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(cx.theme().foreground)
                        .truncate()
                        .child(offer.name.clone()),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(offer.schema.clone()),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.set_target(&name, window, cx)
                }))
        });

        deferred(
            div().absolute().top_full().left_0().right_0().pt_1().child(
                v_flex()
                    .p_1()
                    .gap_0p5()
                    .bg(cx.theme().popover)
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded(cx.theme().radius_lg)
                    .shadow_lg()
                    .children(rows),
            ),
        )
        .into_any_element()
    }
    /// The schema-driven form: a message to publish, a service's request, or an
    /// action's goal.
    fn payload(&self, cx: &mut Context<Self>) -> AnyElement {
        let title = match self.draft.kind {
            RequestKind::Topic => "Message",
            RequestKind::Service => "Request",
            RequestKind::Action => "Goal",
        };
        let schema = self.payload_schema.clone();

        let body = if self.payload.is_empty() {
            tokens::card_body()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(match &schema {
                            // A schema with no fields is a real thing —
                            // `std_srvs/Trigger` takes nothing — and saying so
                            // is better than an empty box.
                            Some(_) => "This one takes no arguments.",
                            None => "Pick a target to see what it takes.",
                        }),
                )
                .into_any_element()
        } else {
            v_flex()
                .id("payload-form")
                .max_h(px(280.))
                .overflow_y_scroll()
                .p_3()
                .gap_2()
                .children(
                    self.payload
                        .iter()
                        .map(|(field, input)| self.row(field, input, cx)),
                )
                .into_any_element()
        };

        tokens::card(cx)
            .flex_shrink_0()
            .child(
                tokens::card_header(cx)
                    .child(tokens::section_label(title, cx))
                    .when_some(schema, |header, schema| {
                        header.child(tokens::meta("Schema", schema, cx))
                    }),
            )
            .child(body)
            .into_any_element()
    }

    /// One leaf of the form: its name, its type, and its editor.
    ///
    /// The label carries the full dotted path rather than only the leaf name.
    /// Flattening `geometry_msgs/PoseStamped` produces two fields called `x` and
    /// three called `sec`, and a column of those is unreadable.
    fn row(&self, field: &Field, input: &Entity<InputState>, cx: &App) -> AnyElement {
        h_flex()
            .w_full()
            .gap_3()
            .items_center()
            .child(
                v_flex()
                    .w(px(220.))
                    .flex_shrink_0()
                    .gap_0p5()
                    .child(
                        tokens::mono(cx)
                            .text_xs()
                            .text_color(cx.theme().foreground)
                            .truncate()
                            .child(field.path.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(field.type_name.clone()),
                    ),
            )
            .child(div().flex_1().min_w_0().child(Input::new(input).small()))
            .into_any_element()
    }

    /// The banner explaining why the request will not run, with the fix attached
    /// when there is one.
    fn problem(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let problem = self.problem.as_ref()?;

        Some(
            h_flex()
                .flex_shrink_0()
                .mt_4()
                .mx_4()
                .px_3()
                .py_2()
                .gap_2()
                .items_center()
                .rounded(cx.theme().radius)
                .bg(cx.theme().danger.opacity(0.12))
                .border_1()
                .border_color(cx.theme().danger.opacity(0.32))
                .child(
                    Icon::new(IconName::CircleX)
                        .small()
                        .text_color(cx.theme().danger),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(problem.message.clone()),
                )
                .when_some(problem.connect, |banner, id| {
                    banner.child(
                        Button::new("connect-environment")
                            .outline()
                            .small()
                            .icon(IconName::Globe)
                            .label("Connect")
                            .on_click(
                                cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.connect(id, cx)
                                }),
                            ),
                    )
                })
                .into_any_element(),
        )
    }

    /// The message as something readable.
    ///
    /// An image is shown as an image; anything else becomes a flat table of
    /// leaf paths and values, which beats indented JSON for the thing people
    /// actually do here — finding one field in a large message.
    fn visualize(&self, value: &CanonicalValue, cx: &App) -> AnyElement {
        if let Some(image) = image::decode(value) {
            return self.image(image, cx);
        }

        let leaves = value::leaves(value);
        if leaves.is_empty() {
            return tokens::empty_state(
                IconName::Inbox,
                "Nothing to show",
                "This message has no fields.",
                cx,
            )
            .into_any_element();
        }

        v_flex()
            .id("fields")
            .size_full()
            .gap_0p5()
            .children(leaves.into_iter().map(|(path, shown)| {
                h_flex()
                    .w_full()
                    .py_0p5()
                    .gap_4()
                    .items_baseline()
                    .child(
                        tokens::mono(cx)
                            .w(px(240.))
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(path),
                    )
                    .child(
                        tokens::mono(cx)
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(cx.theme().foreground)
                            .child(shown),
                    )
            }))
            .into_any_element()
    }

    fn image(&self, image: image::Frame, cx: &App) -> AnyElement {
        v_flex()
            .size_full()
            .gap_2()
            .items_center()
            .child(
                gpui::img(image.source)
                    .max_w_full()
                    .max_h(px(420.))
                    .rounded(cx.theme().radius),
            )
            .child(
                tokens::mono(cx)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(image.caption),
            )
            .into_any_element()
    }

    /// One line per numeric field, newest sample at the right.
    ///
    /// The x axis is the sample index rather than a wall clock: messages arrive
    /// when the robot sends them, and pretending otherwise would draw a smooth
    /// line over a gap where nothing came.
    fn plot(&self, history: &History, cx: &App) -> AnyElement {
        if history.is_empty() {
            return tokens::empty_state(
                IconName::ChartPie,
                "Nothing to plot",
                "This message has no numbers in it, or none that fit on a line chart.",
                cx,
            )
            .into_any_element();
        }

        let palette = tokens::series_colors(cx);
        let charts = history
            .iter()
            .enumerate()
            .map(|(index, (path, series))| self.series_row(index, path, series, &palette, cx));

        v_flex()
            .id("plot")
            .size_full()
            .gap_3()
            .children(charts)
            .into_any_element()
    }

    fn series_row(
        &self,
        index: usize,
        path: &str,
        series: &Series,
        palette: &[gpui::Hsla],
        cx: &App,
    ) -> AnyElement {
        let stroke = palette[index % palette.len()];
        let caption = match (series.last(), series.range()) {
            (Some(last), Some((low, high))) => {
                format!("{last:.4}  ·  {low:.4} to {high:.4}")
            }
            _ => String::new(),
        };

        // The x value is the sample's position in the window, as a string
        // because that is what the chart's point scale keys on. It has to be
        // distinct per sample — give them all the same label and every point
        // lands on the same x, which draws the series as a single vertical
        // stroke. The axis itself is off, so the numbers are never shown.
        let points: Vec<(SharedString, f64)> = series
            .samples
            .iter()
            .enumerate()
            .map(|(index, sample)| (SharedString::from(index.to_string()), *sample))
            .collect();

        v_flex()
            .flex_shrink_0()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .items_baseline()
                    .justify_between()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(tokens::status_dot(stroke))
                            .child(
                                tokens::mono(cx)
                                    .text_xs()
                                    .text_color(cx.theme().foreground)
                                    .child(if path.is_empty() {
                                        SharedString::from("value")
                                    } else {
                                        SharedString::from(path.to_string())
                                    }),
                            ),
                    )
                    .child(
                        tokens::mono(cx)
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(caption),
                    ),
            )
            .child(
                div().h(px(96.)).w_full().child(
                    LineChart::new(points)
                        .x(|(label, _): &(SharedString, f64)| label.clone())
                        .y(|(_, sample): &(SharedString, f64)| *sample)
                        .stroke(stroke)
                        .linear()
                        .x_axis(false),
                ),
            )
            .into_any_element()
    }

    fn response(&self, cx: &mut Context<Self>) -> AnyElement {
        let active = self.tab;
        let running = self.running();
        let (value, schema, count, history) = {
            let incoming = self.incoming.lock().expect("incoming mutex");
            (
                incoming.value.clone(),
                incoming.schema.clone(),
                incoming.count,
                incoming.history.clone(),
            )
        };

        // A segmented bar rather than document tabs: these are three views of one
        // response, not three things that can be closed.
        let tabs = TabBar::new("response-views")
            .segmented()
            .selected_index(active.index())
            .children(ResponseTab::ALL.map(|tab| Tab::new().label(tab.label())))
            .on_click(cx.listener(|this, index: &usize, _, cx| {
                if let Some(tab) = ResponseTab::ALL.get(*index) {
                    this.tab = *tab;
                    cx.notify();
                }
            }));

        let stats = h_flex()
            .gap_3()
            .flex_shrink_0()
            .child(tokens::meta("Messages", count.to_string(), cx))
            .when_some(schema, |row, schema| {
                row.child(tokens::meta("Schema", schema, cx))
            });

        let body = match (&value, active) {
            (Some(value), ResponseTab::Raw) => tokens::mono(cx)
                .text_xs()
                .text_color(cx.theme().foreground)
                .child(value::preview(value))
                .into_any_element(),
            (Some(_), ResponseTab::Plot) => self.plot(&history, cx),
            (Some(value), ResponseTab::Visualize) => self.visualize(value, cx),
            (None, _) => tokens::empty_state(
                IconName::Inbox,
                if running {
                    "Waiting for the first message…"
                } else {
                    "Not running"
                },
                if running {
                    "The subscription is open; nothing has arrived yet."
                } else {
                    "Send the request to see its response here."
                },
                cx,
            )
            .into_any_element(),
        };

        tokens::card(cx)
            // Fills whatever the head of the panel leaves, so the response is the
            // pane's main event rather than a box that grows with its content.
            .flex_1()
            .min_h_0()
            .child(tokens::card_header(cx).child(tabs).child(stats))
            .child(
                tokens::card_body()
                    .id("response-body")
                    .overflow_scroll()
                    .child(body),
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

    fn on_added_to(
        &mut self,
        tab_panel: gpui::WeakEntity<gpui_component::dock::TabPanel>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.home.moved_to(tab_panel);
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_2()
            .items_center()
            .child(tokens::status_dot(tokens::kind_color(self.draft.kind, cx)))
            .child(RequestPanel::title(self))
    }

    fn title_suffix(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        self.dirty().then(|| {
            div()
                .size(px(5.))
                .rounded_full()
                .bg(cx.theme().warning)
                .into_any_element()
        })
    }
}

impl Render for RequestPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_payload(window, cx);
        let header = self.header(cx);
        let bar = self.request_bar(cx);
        // Shown for topics too: a topic request that can only be watched is half
        // a request, and the message you publish is the same form.
        let payload = Some(self.payload(cx));
        let response = self.response(cx);
        let problem = self.problem(cx);

        v_flex()
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .on_action(cx.listener(|this, action: &SetKind, _, cx| {
                this.draft.kind = kind_from_discriminant(action.0);
                cx.notify();
            }))
            .on_action(cx.listener(|this, action: &UseEnvironment, _, cx| {
                this.draft.connection_id = Some(action.0);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &crate::actions::SaveRequest, _, cx| this.save(cx)))
            // A single-line input ignores the arrow keys and Escape, so the panel
            // takes them for the offer list rather than binding actions that
            // would collide with the rest of the app.
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if this.offers(cx).is_empty() {
                    return;
                }
                match event.keystroke.key.as_str() {
                    "down" => this.move_highlight(1, cx),
                    "up" => this.move_highlight(-1, cx),
                    "escape" => {
                        this.offers_open = false;
                        cx.notify();
                    }
                    _ => return,
                }
                cx.stop_propagation();
            }))
            // The head is fixed: name, kind and target stay put while the
            // response scrolls, which is the whole point of a request bar. It
            // also sits on the panel surface, continuing the tab strip above it,
            // so chrome and canvas are two visibly different bands.
            .child(
                v_flex()
                    .flex_shrink_0()
                    .p_4()
                    .gap_3()
                    .bg(cx.theme().tab_bar)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(header)
                    .child(bar),
            )
            .children(problem)
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .p_4()
                    .gap_3()
                    .children(payload)
                    .child(response),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_discriminants_round_trip() {
        for kind in [
            RequestKind::Topic,
            RequestKind::Service,
            RequestKind::Action,
        ] {
            assert_eq!(kind_from_discriminant(discriminant_of(kind)), kind);
        }
    }

    #[test]
    fn an_unknown_discriminant_falls_back_to_topic() {
        assert_eq!(kind_from_discriminant(200), RequestKind::Topic);
    }

    #[test]
    fn response_tabs_have_distinct_labels() {
        let labels: Vec<_> = ResponseTab::ALL.iter().map(|tab| tab.label()).collect();
        assert_eq!(labels, ["Raw", "Visualize", "Plot"]);
    }
}
