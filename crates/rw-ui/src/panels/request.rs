//! The request editor: a saved, named, reusable call.
//!
//! Layout, top to bottom: the request's name and kind, then the request bar
//! (kind, target, environment, send), then the payload form for services and
//! actions, then the response.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window,
    deferred, div, px,
};
use gpui_component::WindowExt as _;
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
use rw_core::domain::{HistoryEntry, NewHistoryEntry, Outcome, Request, RequestKind, Value};

use rw_transport::ConnectionId;

use crate::discovery::{self, Suggestion};
use crate::docking::Home;
use crate::form::{self, Field};
use crate::param;
use crate::prefs::Settings;
use crate::runs::{RunState, Runs};
use crate::scene_view::SceneView;
use crate::series::{History, Limits};
use crate::session::{RobotWhisperer, Sessions};
use crate::tokens;
use crate::views::{self, View};
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
        3 => RequestKind::Param,
        _ => RequestKind::Topic,
    }
}

fn discriminant_of(kind: RequestKind) -> u8 {
    match kind {
        RequestKind::Topic => 0,
        RequestKind::Service => 1,
        RequestKind::Action => 2,
        RequestKind::Param => 3,
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

/// What one field of the form is edited with.
///
/// An array is a list of editors rather than one box of commas: `0.2, 0.2,
/// -0.2, -0.2` is a value you have to parse in your head before you can change
/// the third number. The single box survives only as the fallback for an array
/// longer than [`form::MAX_ROWS`], which is data rather than something anybody
/// edits by hand.
enum Inputs {
    One(Entity<InputState>),
    List {
        element: form::Element,
        rows: Vec<Entity<InputState>>,
    },
}

impl Inputs {
    /// Every editor in the field, for the passes that treat them all alike.
    fn each(&self) -> impl Iterator<Item = &Entity<InputState>> {
        match self {
            Inputs::One(input) => std::slice::from_ref(input).iter(),
            Inputs::List { rows, .. } => rows.iter(),
        }
    }
}

/// A node's parameters as they were last read, and what they were declared as.
///
/// The kinds travel with the values because a write has to name the type it is
/// setting and it must be the declared one — a node refuses a `double` where it
/// declared an `integer`, however reasonable the number looks.
struct Parameters {
    values: CanonicalValue,
    kinds: BTreeMap<String, param::Kind>,
    /// Bumped on every read, so the form is rebuilt from what just came back
    /// rather than left showing the previous reading.
    generation: u64,
}

pub struct RequestPanel {
    focus_handle: FocusHandle,
    workspace: Entity<Workspace>,
    sessions: Entity<Sessions>,

    /// What is stored, and the edited copy. `dirty` compares them.
    saved: Request,
    draft: Request,

    target: Entity<InputState>,

    /// Whether the target field's offer list is showing, and which row the
    /// keyboard is on. The offers themselves are derived rather than stored —
    /// discovery arrives asynchronously, and a cached list was simply empty
    /// whenever it landed after the field was focused.
    highlighted: usize,
    offers_open: bool,

    /// The payload form: one input per leaf of the request or goal message,
    /// rebuilt whenever the schema behind the target changes.
    payload: Vec<(Field, Inputs)>,
    /// The schema the current form was built from, so it is only rebuilt when
    /// it actually changes rather than on every render.
    payload_schema: Option<String>,

    /// The last parameter reading, for a request of kind `Param`. `None` until
    /// the node has been read: there is nothing to offer editors for before
    /// that, because what a node declares is not in any schema.
    parameters: Option<Parameters>,

    incoming: Arc<Mutex<Incoming>>,
    activity: Activity,
    /// Shared, so the sidebar can show what this request is doing.
    runs: Entity<Runs>,
    tab: View,
    problem: Option<Problem>,
    /// The 3D pane, built the first time a message turns out to be a point
    /// cloud: most requests never need a GPU and should not open one.
    scene: Option<Entity<SceneView>>,
    /// Which message the scene is showing, so a cloud is uploaded once rather
    /// than on every repaint.
    scene_at: u64,
    /// The foldable tree of the response, built the first time it is looked at:
    /// most requests are watched in the raw view and should not pay for one.
    tree: Option<Entity<crate::tree::TreeView>>,

    /// The message the diff view compares against, and which message it was.
    ///
    /// The live stream keeps running underneath: freezing a reading and
    /// watching it drift is the question people are asking when they stare at
    /// a raw view.
    frozen: Option<(CanonicalValue, u64)>,
    /// What this request has done before, newest first.
    ///
    /// Read from storage rather than accumulated in memory, so it is still
    /// there after the tab is closed and after the app is — which is the whole
    /// difference between this and the live response beside it.
    past: Vec<HistoryEntry>,
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

        let target = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("/topic, /service, /action or /node")
                .default_value(&request.target)
        });

        let subscriptions = vec![
            cx.observe(&sessions, |_, _, cx| cx.notify()),
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
            target,
            highlighted: 0,
            offers_open: false,
            payload: Vec::new(),
            payload_schema: None,
            parameters: None,
            incoming: Arc::new(Mutex::new(Incoming::default())),
            activity: Activity::default(),
            runs,
            tab: View::default(),
            problem: None,
            scene: None,
            scene_at: 0,
            tree: None,
            frozen: None,
            past: Vec::new(),
            home: Home::default(),
            _repaint: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn view(request: &Request, window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let panel = Self::new(request, window, cx);
            // Reopening a request should show what it has already done, which is
            // the difference between history and the live response beside it.
            panel.reload_history(cx);
            panel
        })
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

    /// What the open subscription is doing, if this request has one.
    ///
    /// `ros2 topic hz` is the most-run command in robotics, and every time
    /// someone runs it they leave the tool they were already looking at. The
    /// numbers go where the message count already is rather than in a strip of
    /// their own.
    fn stats(&self, cx: &App) -> Option<rw_pipeline::stats::Stats> {
        let Activity::Subscribed(subscription) = &self.activity else {
            return None;
        };
        let stats = self.sessions.read(cx).pipeline().stats(subscription)?;
        (!stats.is_empty()).then_some(stats)
    }

    /// Records what this request is doing where the sidebar can see it.
    ///
    /// Called from the one place `activity` changes rather than beside each
    /// assignment, so the two cannot drift apart.
    fn publish_state(&mut self, cx: &mut Context<Self>) {
        let state = match (&self.activity, &self.problem) {
            (_, Some(problem)) => RunState::Failed(problem.message.clone()),
            (Activity::Subscribed(handle), None) => RunState::Live(Some(handle.clone().into())),
            (activity, None) if !activity.is_idle() => RunState::Live(None),
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
        //
        // Everything that parsed is kept, and a field that did not is said out
        // loud rather than silently taking the other nine down with it — which
        // is what the old `if let Ok(…)` did, writing back the previous input
        // while the screen still showed what had been typed.
        let (payload, problem) = self.read_form(cx);
        self.draft.input = payload;
        self.problem = problem.map(Problem::new);
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
            RequestKind::Param => self.read_parameters(session, target, cx),
        }
        cx.notify();
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
        // Held for the subscription's whole life rather than checked once, so
        // recording started after a topic was already being watched still
        // captures it.
        let tap = RobotWhisperer::global(cx).recorder.read(cx).tap();
        let topic = target.clone();
        // Read here rather than per message: a subscription callback has the
        // frame and nothing else.
        let limits = crate::series::Limits::current(cx);

        cx.spawn(async move |panel, cx| {
            let outcome = pipeline
                .subscribe_topic(session, &target, move |_handle, frame, _lossy| {
                    tap.observe(
                        &topic,
                        &frame.schema.name,
                        (!frame.schema.definition.is_empty())
                            .then_some(frame.schema.definition.as_str()),
                        &frame.value,
                    );
                    let Ok(mut incoming) = incoming.lock() else {
                        return;
                    };
                    incoming.history.observe(&frame.value, limits);
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

        let sent = request.clone();
        cx.spawn(async move |panel, cx| {
            let outcome = pipeline
                .call_service(session, &target, request.into())
                .await;

            panel
                .update(cx, |panel, cx| {
                    match outcome {
                        Ok(response) => {
                            panel.record(
                                sent.clone(),
                                Outcome::Answered,
                                Some(Value::from(response.clone())),
                                cx,
                            );
                            let mut incoming = panel.incoming.lock().expect("incoming mutex");
                            incoming.history.observe(&response, Limits::current(cx));
                            incoming.value = Some(response);
                            incoming.count += 1;
                        }
                        Err(error) => {
                            let reason = error.to_string();
                            panel.record(sent.clone(), Outcome::Failed { reason }, None, cx);
                            panel.failed(error, cx);
                        }
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

        let sent = goal.clone();
        let limits = crate::series::Limits::current(cx);
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
                incoming.history.observe(&feedback, limits);
                incoming.value = Some(feedback);
                incoming.count += 1;
            }

            let result = stream.result.await;
            panel
                .update(cx, |panel, cx| {
                    match result {
                        Ok(Ok(value)) => {
                            // The result, not the feedback: a goal ends once,
                            // and the entry is about how it ended.
                            panel.record(
                                sent.clone(),
                                Outcome::Answered,
                                Some(Value::from(value.clone())),
                                cx,
                            );
                            let mut incoming = panel.incoming.lock().expect("incoming mutex");
                            incoming.history.observe(&value, Limits::current(cx));
                            incoming.value = Some(value);
                            incoming.count += 1;
                        }
                        Ok(Err(error)) => {
                            let reason = error.to_string();
                            panel.record(sent.clone(), Outcome::Failed { reason }, None, cx);
                            panel.failed(error, cx);
                        }
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

    // ── parameters ─────────────────────────────────────────────────────────────

    /// Reads every parameter a node declares.
    ///
    /// Two calls, which is exactly what `ros2 param dump` is: ask the node what
    /// it has, then ask for the values. Nothing here is a new transport —
    /// parameters are ordinary services on the node, so this works over
    /// rosbridge and Foxglove today.
    fn read_parameters(&mut self, session: ConnectionId, node: String, cx: &mut Context<Self>) {
        let pipeline = self.sessions.read(cx).pipeline();
        self.set_activity(Activity::Calling, cx);

        cx.spawn(async move |panel, cx| {
            let outcome = read_parameters(&pipeline, session, &node).await;
            panel
                .update(cx, |panel, cx| {
                    match outcome {
                        Ok((values, kinds)) => {
                            panel.record(
                                Value::empty_struct(),
                                Outcome::Answered,
                                Some(Value::from(values.clone())),
                                cx,
                            );
                            panel.parameters_arrived(values, kinds, Limits::current(cx));
                        }
                        Err(error) => {
                            let reason = error.to_string();
                            panel.record(
                                Value::empty_struct(),
                                Outcome::Failed { reason },
                                None,
                                cx,
                            );
                            panel.failed(error, cx);
                        }
                    }
                    panel.set_activity(Activity::Idle, cx);
                    panel._repaint = None;
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    /// Files a reading: into the response, so every view already built works on
    /// parameters, and into `parameters`, so the form can be rebuilt from it.
    fn parameters_arrived(
        &mut self,
        values: CanonicalValue,
        kinds: BTreeMap<String, param::Kind>,
        limits: Limits,
    ) {
        let generation = self
            .parameters
            .as_ref()
            .map_or(0, |parameters| parameters.generation)
            + 1;

        {
            let mut incoming = self.incoming.lock().expect("incoming mutex");
            incoming.history.observe(&values, limits);
            incoming.value = Some(values.clone());
            incoming.count += 1;
        }

        self.parameters = Some(Parameters {
            values,
            kinds,
            generation,
        });
    }

    /// Sets the parameters currently in the form on the node, then reads them
    /// back.
    ///
    /// The read back is not politeness: `SetParameters` reports per parameter
    /// whether it was accepted, and a node is free to accept a value and hold a
    /// different one. What it holds afterwards is the answer.
    fn write_parameters(&mut self, cx: &mut Context<Self>) {
        let node = self.draft.target.trim().to_string();
        if node.is_empty() {
            self.problem = Some(Problem::new("Enter a node first"));
            cx.notify();
            return;
        }
        let Some(session) = self.session(cx) else {
            self.problem = Some(self.why_not_connected(cx));
            cx.notify();
            return;
        };
        let values: CanonicalValue = match self.payload_value(cx) {
            Ok(values) => values.into(),
            Err(error) => {
                self.problem = Some(Problem::new(error));
                cx.notify();
                return;
            }
        };

        let kinds = self
            .parameters
            .as_ref()
            .map(|parameters| parameters.kinds.clone())
            .unwrap_or_default();

        self.problem = None;
        self.set_activity(Activity::Calling, cx);
        let pipeline = self.sessions.read(cx).pipeline();

        let written = Value::from(values.clone());
        cx.spawn(async move |panel, cx| {
            let outcome = write_parameters(&pipeline, session, &node, &values, &kinds).await;
            // Read back whatever happened: a partial refusal leaves the node
            // holding a mixture, and showing the old form beside it would be a
            // lie about what the robot has.
            let reading = read_parameters(&pipeline, session, &node).await;

            panel
                .update(cx, |panel, cx| {
                    panel.set_activity(Activity::Idle, cx);
                    panel._repaint = None;
                    if let Ok((values, kinds)) = reading {
                        panel.parameters_arrived(values, kinds, Limits::current(cx));
                    }
                    match outcome {
                        Ok(count) => {
                            panel.record(written.clone(), Outcome::Answered, None, cx);
                            panel.say(format!("set {count} on {node}"), cx);
                        }
                        Err(error) => {
                            let reason = error.to_string();
                            panel.record(written.clone(), Outcome::Failed { reason }, None, cx);
                            panel.failed(error, cx);
                        }
                    }
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    /// The form for a node's parameters: one editor per parameter, shaped by
    /// the type the node declared it with.
    ///
    /// Built from what the node answered rather than from a schema, because a
    /// parameter list is not a message type — two nodes running the same
    /// executable can declare different parameters. `form::fields` still draws
    /// it, so the editors, the parsing and the placeholders are the ones every
    /// other request already uses.
    fn parameter_fields(&self) -> Vec<Field> {
        let Some(parameters) = &self.parameters else {
            return Vec::new();
        };
        let message = param::message_def(&parameters.values, &parameters.kinds);
        form::fields(&message, &|_: &str| None)
    }

    // ── the payload form ───────────────────────────────────────────────────────

    /// The schema name the target implies, from discovery.
    ///
    /// Discovery is the only place that knows which schema a name carries, and
    /// the request stores it so a saved request still shows its form before the
    /// robot is reachable.
    fn payload_schema_name(&self, cx: &App) -> Option<String> {
        // A topic has no form to key on. `None` here is what empties `payload`
        // when a request is switched from a service to a topic, rather than
        // leaving the service's boxes behind an invisible card.
        if self.draft.kind == RequestKind::Topic {
            return None;
        }
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
                RequestKind::Service => named(&discovery.services),
                RequestKind::Action => named(&discovery.actions),
                // A node's parameters are not in any schema, so the key the
                // form is rebuilt on is the reading itself: a fresh one has to
                // replace the boxes, and nothing else may.
                RequestKind::Param => None,
                // Returned above.
                RequestKind::Topic => None,
            }
        });

        if self.draft.kind == RequestKind::Param {
            let generation = self
                .parameters
                .as_ref()
                .map_or(0, |parameters| parameters.generation);
            return Some(format!("{target} #{generation}"));
        }

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

        let from_a_reading = self.draft.kind == RequestKind::Param;
        let fields = if from_a_reading {
            self.parameter_fields()
        } else {
            wanted
                .as_deref()
                .and_then(|name| self.message_for(name, cx))
                .unwrap_or_default()
        };

        // Reading a node replaces what is in the boxes — seeing what it
        // currently holds is the point of reading. Everywhere else the source
        // is what was saved, and text already typed wins over both.
        let stored = match &self.parameters {
            Some(parameters) if from_a_reading => Value::from(parameters.values.clone()),
            _ => self.draft.input.clone(),
        };

        // Existing text survives a rebuild when the leaf is still there, so
        // reconnecting to a robot does not clear a half-filled form.
        let filled: Vec<(String, Vec<String>)> = self
            .payload
            .iter()
            .map(|(field, inputs)| {
                (
                    field.path.clone(),
                    inputs
                        .each()
                        .map(|input| input.read(cx).value().to_string())
                        .collect(),
                )
            })
            .collect();

        self.payload = fields
            .into_iter()
            .map(|field| {
                let typed = (!from_a_reading)
                    .then(|| {
                        filled
                            .iter()
                            .find(|(path, _)| *path == field.path)
                            .map(|(_, texts)| texts.clone())
                    })
                    .flatten();

                // A list gets a row per element — unless the stored value is
                // longer than anyone would edit by hand, in which case the
                // single comma box is the honest way to show it.
                if let form::Editor::List(element) = field.editor {
                    let rows = typed
                        .clone()
                        .or_else(|| form::rows_at(&stored, &field.path, element));
                    if let Some(rows) = rows.filter(|rows| rows.len() <= form::MAX_ROWS) {
                        let rows = rows
                            .into_iter()
                            .map(|text| {
                                Self::editor(form::element_editor(element), Some(text), window, cx)
                            })
                            .collect();
                        return (field, Inputs::List { element, rows });
                    }
                }

                let existing = typed
                    .and_then(|texts| texts.into_iter().next())
                    .or_else(|| form::text_at(&stored, &field.path, field.editor));
                let editor = field.editor;
                (
                    field,
                    Inputs::One(Self::editor(editor, existing, window, cx)),
                )
            })
            .collect();
        self.payload_schema = wanted;
    }

    /// One editor, with whatever text it starts life holding.
    fn editor(
        editor: form::Editor,
        text: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let placeholder = editor.placeholder();
        cx.new(|cx| {
            let state = InputState::new(window, cx).placeholder(placeholder);
            match text {
                Some(text) => state.default_value(text),
                None => state,
            }
        })
    }

    /// The schema chip: the name, and the definition behind it when we have one.
    ///
    /// A plain label when the text is not to hand, so the chip never promises a
    /// click that does nothing.
    fn schema_chip(&self, schema: SharedString, cx: &mut Context<Self>) -> AnyElement {
        let Some((hash, text)) = self.definition_text(&schema, cx) else {
            return tokens::meta("Schema", schema, cx).into_any_element();
        };
        let title = schema.clone();
        Button::new("schema")
            .ghost()
            .xsmall()
            .child(tokens::meta("Schema", schema, cx))
            .tooltip("Show the message definition")
            .on_click(cx.listener(move |_, _: &ClickEvent, window, cx| {
                show_definition(title.clone(), hash.clone(), text.clone(), window, cx);
            }))
            .into_any_element()
    }

    /// The definition text this connection actually sent for `name`.
    ///
    /// `ros2 interface show` answers with whatever is installed on the machine
    /// you run it on. This answers with what the robot in front of you said,
    /// which is the only version that explains the bytes on the wire — and it
    /// is the same lookup the form is built from, so what you read is what the
    /// fields came from.
    fn definition_text(&self, name: &str, cx: &App) -> Option<(String, String)> {
        let pipeline = self.sessions.read(cx).pipeline();
        let registry = pipeline.schema_registry()?.clone();
        let hash = self
            .draft
            .connection_id
            .and_then(|id| self.sessions.read(cx).session(id))
            .and_then(|session| pipeline.schema_hash(session, self.draft.target.trim()));
        let entry = match hash.and_then(|hash| registry.get_by_hash(&hash)) {
            Some(entry) => entry,
            None => registry.get_by_name(name).into_iter().next()?,
        };
        Some((entry.hash.clone(), entry.definition.clone()))
    }

    /// The message definition a form should be built from: a service's request
    /// or an action's goal.
    /// The form's fields for `name`, resolved the way *this* connection
    /// described it.
    ///
    /// Two robots can publish a schema of the same name and mean different
    /// things by it — a ROS 1 `std_msgs/Header` carries `seq` and a ROS 2 one
    /// does not — and the registry holds both the moment both are connected.
    /// So the connection's own entry is asked for first, and the name is only
    /// consulted when discovery never described the target.
    fn message_for(&self, name: &str, cx: &App) -> Option<Vec<Field>> {
        let pipeline = self.sessions.read(cx).pipeline();
        let registry = pipeline.schema_registry()?.clone();

        let hash = self
            .draft
            .connection_id
            .and_then(|id| self.sessions.read(cx).session(id))
            .and_then(|session| pipeline.schema_hash(session, self.draft.target.trim()));
        let definition = match hash.and_then(|hash| registry.get_by_hash(&hash)) {
            Some(definition) => definition,
            None => registry.get_by_name(name).into_iter().next()?,
        };

        let message = match &definition.parsed {
            rw_core::schema::ParsedSchema::Service { request, .. } => request,
            rw_core::schema::ParsedSchema::Action { goal, .. } => goal,
            rw_core::schema::ParsedSchema::Message(message) => message,
        };

        // A nested type comes from the sections this definition arrived with,
        // before the registry is asked at all. Those are the publisher's own
        // copies, and they are the only correct answer for it — resolving
        // `std_msgs/Header` by name here is exactly how one robot's header ends
        // up describing another's.
        let own: std::collections::HashMap<&str, &str> =
            rw_core::schema::parser::split_bundle(&definition.definition)
                .1
                .into_iter()
                .map(|part| (part.name, part.body))
                .collect();
        let own: std::collections::HashMap<String, rw_core::schema::MessageDef> = own
            .into_iter()
            .filter_map(|(type_name, body)| {
                let package = type_name.split('/').next().filter(|part| !part.is_empty());
                let parsed = rw_core::schema::parser::parse_with_package(
                    rw_core::schema::SchemaKind::Message,
                    body,
                    package,
                )
                .ok()?;
                Some((type_name.to_string(), parsed.primary().clone()))
            })
            .collect();

        let lookup = move |type_name: &str| {
            own.get(type_name).cloned().or_else(|| {
                registry
                    .get_by_name(type_name)
                    .into_iter()
                    .next()
                    .map(|definition| definition.parsed.primary().clone())
            })
        };
        Some(form::fields(message, &lookup))
    }

    /// The payload the form currently describes.
    ///
    /// Topics carry nothing, so they get an empty message rather than a special
    /// case at every call site.
    fn payload_value(&self, cx: &App) -> Result<Value, String> {
        let (value, problem) = self.read_form(cx);
        match problem {
            Some(problem) => Err(problem),
            None => Ok(value),
        }
    }

    /// The form's contents, keeping every field that parsed, and the first one
    /// that did not.
    ///
    /// Sending and saving want different things from a half-filled form.
    /// Sending has to refuse it — there is no such thing as most of a message.
    /// Saving must not: a form you have not finished is a perfectly good thing
    /// to come back to, and throwing away the nine fields you got right because
    /// the tenth says "twelve" is the kind of loss nobody forgives.
    fn read_form(&self, cx: &App) -> (Value, Option<String>) {
        let mut leaves = Vec::new();
        let mut problem = None;
        for (field, inputs) in &self.payload {
            let parsed = match inputs {
                Inputs::One(input) => form::parse(field.editor, &input.read(cx).value()),
                Inputs::List { element, rows } => {
                    let texts: Vec<String> = rows
                        .iter()
                        .map(|row| row.read(cx).value().to_string())
                        .collect();
                    form::parse_list(*element, &texts)
                }
            };
            match parsed {
                Ok(Some(value)) => leaves.push((field.path.clone(), value)),
                // An empty box is an absent field, not a zero. It is left out
                // of the value, so clearing one and saving really does clear it.
                Ok(None) => {}
                Err(reason) => {
                    problem.get_or_insert(format!("{}: {reason}", field.path));
                }
            }
        }
        (form::assemble(leaves), problem)
    }

    /// Keeps this run, so it is still there after the next one.
    ///
    /// A service call and an action goal each happen once and are over, which
    /// is the shape history is for. A topic is a subscription: it has no one
    /// answer to keep, and recording every message as its own entry would be
    /// the recorder's job done badly.
    ///
    /// `input` is handed in rather than read off the draft. `draft.input` is
    /// only written when the request is *saved*, so recording that would
    /// remember the arguments of some earlier edit — and "put these back in the
    /// form" would put back something that was never sent.
    fn record(
        &self,
        input: Value,
        outcome: Outcome,
        response: Option<Value>,
        cx: &mut Context<Self>,
    ) {
        // Only the kinds that can show it back. A topic is a subscription and
        // has no discrete runs to keep. A parameter request has runs, but its
        // form *is* its response — there is no response card to hang a History
        // tab on — and writing rows nobody can ever read is worse than not
        // keeping them. If parameter history is wanted it needs somewhere to
        // live on the PARAMETERS card first.
        if !matches!(self.draft.kind, RequestKind::Service | RequestKind::Action) {
            return;
        }
        let entry = NewHistoryEntry {
            request_id: self.saved.id,
            kind: self.draft.kind,
            target: self.draft.target.trim().to_string(),
            connection_id: self.draft.connection_id,
            outcome,
            input,
            response,
        };
        let storage = self.workspace.read(cx).storage();
        let id = self.saved.id;
        // The depth is read here rather than at the write: how many runs are
        // kept is enforced on insert, so a lowered setting takes effect on the
        // next call rather than needing anything to sweep.
        let depth = Settings::get(cx).history_depth;
        cx.spawn(async move |panel, cx| {
            if let Err(error) = storage.record_history(entry, depth).await {
                // Not being able to write the history of a call is not a reason
                // to tell someone their call failed.
                tracing::warn!(?error, "could not record this run");
                return;
            }
            let past = storage.list_history(id, depth).await.unwrap_or_default();
            panel
                .update(cx, |panel, cx| {
                    panel.past = past;
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    /// Re-reads this request's runs from storage.
    fn reload_history(&self, cx: &mut Context<Self>) {
        let storage = self.workspace.read(cx).storage();
        let id = self.saved.id;
        let depth = Settings::get(cx).history_depth;
        cx.spawn(async move |panel, cx| {
            let past = storage.list_history(id, depth).await.unwrap_or_default();
            panel
                .update(cx, |panel, cx| {
                    panel.past = past;
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    /// Puts a past run's arguments back in the form.
    ///
    /// The reason to keep history at all: not to admire what you did, but to do
    /// it again. The response is left alone — this loads the question, it does
    /// not pretend to have re-asked it.
    fn reuse(&mut self, entry: &HistoryEntry, window: &mut Window, cx: &mut Context<Self>) {
        self.draft.input = entry.input.clone();
        for (field, inputs) in &self.payload {
            let Inputs::One(input) = inputs else { continue };
            let text = form::text_at(&entry.input, &field.path, field.editor).unwrap_or_default();
            input.update(cx, |state, cx| state.set_value(text, window, cx));
        }
        self.tab = View::Pretty;
        cx.notify();
    }

    /// This request's past runs, newest first.
    ///
    /// Each row is what you want at a glance: when, whether it worked, and what
    /// came back. Clicking one puts its arguments back in the form.
    fn past_runs(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .id("history")
            .size_full()
            .gap_0p5()
            .children(
                self.past
                    .iter()
                    .enumerate()
                    .map(|(index, entry)| self.past_run(index, entry, cx)),
            )
            .into_any_element()
    }

    fn past_run(&self, index: usize, entry: &HistoryEntry, cx: &mut Context<Self>) -> AnyElement {
        let at = entry.at.with_timezone(&chrono::Local).format("%H:%M:%S");
        let (tint, summary) = match &entry.outcome {
            Outcome::Answered => (
                cx.theme().muted_foreground,
                entry
                    .response
                    .as_ref()
                    .map(|value| one_line(&CanonicalValue::from(value.clone())))
                    .unwrap_or_else(|| "done".to_string()),
            ),
            Outcome::Failed { reason } => (cx.theme().danger, reason.clone()),
        };

        h_flex()
            .id(("run", index))
            .w_full()
            .px_2()
            .py_1()
            .gap_3()
            .items_baseline()
            .rounded(cx.theme().radius)
            .cursor_pointer()
            .hover(|row| row.bg(cx.theme().list_hover))
            .child(
                tokens::mono(cx)
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(at.to_string()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .truncate()
                    .text_color(tint)
                    .child(summary),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                let Some(entry) = this.past.get(index).cloned() else {
                    return;
                };
                this.reuse(&entry, window, cx);
            }))
            .into_any_element()
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

    /// The prominent row: kind, target, environment, primary action.
    fn request_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let offers = self.offers(cx);
        let kind = self.draft.kind;
        let running = self.running();
        let calling = matches!(self.activity, Activity::Calling);
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
                            RequestKind::Param,
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
                    .px_1()
                    .rounded(cx.theme().radius)
                    // The field is a button as much as it is an input — it is
                    // what opens the list of what the robot advertises — and
                    // until now it was the only one in the bar that did not
                    // admit to being pointed at.
                    .hover(|zone| zone.bg(cx.theme().muted))
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
                    // And a click anywhere else puts it away. Blur is what this
                    // used to rely on, and blur never arrives without a window
                    // manager — leaving a list of topics sitting over the pane
                    // with no way to dismiss it.
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                        if this.offers_open {
                            this.offers_open = false;
                            cx.notify();
                        }
                    }))
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
                Button::new("save")
                    .ghost()
                    .small()
                    .icon(IconName::Check)
                    .disabled(!self.dirty())
                    .tooltip("Save request")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.save(cx))),
            )
            .child(
                Button::new("send")
                    // A call in flight is not a stoppable thing: there is
                    // nothing to cancel between the request going out and the
                    // answer coming back. So it stays the primary button it
                    // was and spins, rather than turning into a greyed-out red
                    // Stop that says the one thing you cannot do. `loading` is
                    // as inert as `disabled` and keeps its colours, which is
                    // the whole difference.
                    .when(calling, |button| {
                        button
                            .primary()
                            .icon(IconName::Play)
                            .label(self.activity.stop_label())
                            .loading(true)
                    })
                    .when(running && !calling, |button| {
                        button
                            .danger()
                            .icon(IconName::Pause)
                            .label(self.activity.stop_label())
                    })
                    .when(!running, |button| {
                        button.primary().icon(IconName::Play).label(match kind {
                            RequestKind::Topic => "Subscribe",
                            RequestKind::Service => "Call",
                            RequestKind::Action => "Send goal",
                            RequestKind::Param => "Read",
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
            // Write is disabled until there has been a read: a node
            // will not accept a value for a parameter it has not declared, and
            // the form is how the declared ones become editable at all.
            .when(matches!(kind, RequestKind::Param), |bar| {
                bar.child(
                    Button::new("write")
                        .outline()
                        .icon(IconName::ArrowUp)
                        .label("Write")
                        .disabled(self.payload.is_empty())
                        .tooltip("Set the parameters above on this node")
                        .on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.write_parameters(cx)),
                        ),
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
    fn payload(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        // A topic is a subscription. There is nothing to send it, so there is
        // nothing to fill in — the form only ever existed to compose a message
        // to publish.
        if self.draft.kind == RequestKind::Topic {
            return None;
        }
        let is_param = self.draft.kind == RequestKind::Param;
        // Nothing to fill in, nothing to show. A card saying "this takes no
        // arguments" is a paragraph of chrome explaining an empty box, and it
        // pushes the response — the part anyone is here for — down the pane.
        // `std_srvs/Trigger` and a great many topics take nothing at all.
        //
        // Parameters are the exception: this card is the only thing a parameter
        // request draws, so before a read it says so rather than leaving the
        // pane blank below the bar.
        if self.payload.is_empty() && !is_param {
            return None;
        }

        let title = match self.draft.kind {
            RequestKind::Service => "Request",
            RequestKind::Action => "Goal",
            RequestKind::Param => "Parameters",
            // Returned above; a topic has no form.
            RequestKind::Topic => "",
        };
        // One schema chip on screen. The response header carries it once
        // anything has arrived, and until then this does — the two are the same
        // string, and a hundred pixels apart they read as a stutter.
        //
        // A parameter form has no schema at all: the key it is rebuilt on is
        // the node and the reading, which are the target field and the boxes
        // themselves.
        let schema = match (is_param, self.has_response()) {
            (true, _) | (_, true) => None,
            _ => self.payload_schema.clone(),
        };

        let body = if self.payload.is_empty() {
            tokens::empty_state(
                IconName::Inbox,
                "Nothing read yet",
                "Read to see what this node holds, then change a value and write it back.",
                cx,
            )
            .into_any_element()
        } else {
            v_flex()
                .id("payload-form")
                // A parameter form is the pane, so it takes the height. A
                // message form shares the pane with the response, so it is
                // capped and the response gets the rest.
                .when(is_param, |form| form.flex_1().min_h_0())
                .when(!is_param, |form| form.max_h(px(280.)))
                .overflow_y_scroll()
                .p_3()
                .gap_2()
                .children(
                    self.payload
                        .iter()
                        .enumerate()
                        .map(|(index, (field, inputs))| self.row(index, field, inputs, cx)),
                )
                .into_any_element()
        };

        Some(
            tokens::card(cx)
                // The parameter form is the whole pane, so it takes the height
                // the response card would have. Everywhere else it stays the
                // size of its contents and the response gets the rest.
                .when(is_param, |card| card.flex_1().min_h_0())
                .when(!is_param, |card| card.flex_shrink_0())
                .child(
                    tokens::card_header(cx)
                        .child(tokens::section_label(title, cx))
                        .when_some(schema, |header, schema| {
                            header.child(self.schema_chip(SharedString::from(schema), cx))
                        }),
                )
                .child(body)
                .into_any_element(),
        )
    }

    /// Whether anything has come back yet, which decides who draws the schema.
    fn has_response(&self) -> bool {
        self.incoming
            .lock()
            .expect("incoming mutex")
            .schema
            .is_some()
    }

    /// One leaf of the form: its name, its type, and its editor.
    ///
    /// The label carries the full dotted path rather than only the leaf name.
    /// Flattening `geometry_msgs/PoseStamped` produces two fields called `x` and
    /// three called `sec`, and a column of those is unreadable.
    fn row(
        &self,
        index: usize,
        field: &Field,
        inputs: &Inputs,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let editors = match inputs {
            Inputs::One(input) => div()
                .flex_1()
                .min_w_0()
                .child(Input::new(input).small())
                .into_any_element(),
            Inputs::List { rows, .. } => self.list(index, rows, cx),
        };

        tokens::field_row(field.path.clone(), field.type_name.clone(), cx)
            .child(editors)
            .into_any_element()
    }

    /// A list field: an editor per element, and the two buttons that change how
    /// many there are.
    ///
    /// The index of the field rather than its path, because a click has to find
    /// the field again in a form that may have been rebuilt under it, and the
    /// index is what the row was drawn from.
    fn list(
        &self,
        field: usize,
        rows: &[Entity<InputState>],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let elements = rows.iter().enumerate().map(|(index, input)| {
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .w(px(24.))
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("{index}")),
                )
                .child(div().flex_1().min_w_0().child(Input::new(input).small()))
                .child(
                    Button::new(("drop", index))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip("Remove")
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.drop_element(field, index, cx)
                        })),
                )
        });

        v_flex()
            .flex_1()
            .min_w_0()
            .gap_1()
            .children(elements)
            .child(
                // Indented to the editors rather than centred under them: the
                // index column is 24px and the gap 8.
                h_flex().w_full().pl(px(32.)).child(
                    Button::new(("add", field))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Plus)
                        .label("Add")
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.add_element(field, window, cx)
                        })),
                ),
            )
            .into_any_element()
    }

    /// Adds an empty row to a list field.
    fn add_element(&mut self, field: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, Inputs::List { element, rows })) = self.payload.get(field) else {
            return;
        };
        if rows.len() >= form::MAX_ROWS {
            return;
        }
        let editor = form::element_editor(*element);
        let input = Self::editor(editor, None, window, cx);
        let Some((_, Inputs::List { rows, .. })) = self.payload.get_mut(field) else {
            return;
        };
        rows.push(input);
        cx.notify();
    }

    /// Takes one row out of a list field.
    fn drop_element(&mut self, field: usize, index: usize, cx: &mut Context<Self>) {
        let Some((_, Inputs::List { rows, .. })) = self.payload.get_mut(field) else {
            return;
        };
        if index < rows.len() {
            rows.remove(index);
            cx.notify();
        }
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

    /// Takes on a rename made in the sidebar.
    ///
    /// The editor has no name field of its own any more — the tab above it is
    /// the name — so the stored one is simply adopted, and a rename can never
    /// show up here as an unsaved change.
    fn sync_name(&mut self, cx: &mut Context<Self>) {
        let Some(stored) = self
            .workspace
            .read(cx)
            .request(self.saved.id)
            .map(|request| request.name.clone())
        else {
            return;
        };
        if stored != self.saved.name {
            self.saved.name = stored.clone();
            self.draft.name = stored;
            cx.notify();
        }
    }

    /// Hands the newest message to the 3D pane, building it on first sight.
    ///
    /// What it decodes into is the registry's decision, from the schema's role
    /// — so a scan, a path and a pose all arrive here rather than only a cloud.
    ///
    /// Driven from `render` rather than from the subscription, because it needs
    /// a context that can create an entity and because a pane nobody is looking
    /// at should not be uploading megabytes.
    fn sync_scene(&mut self, cx: &mut Context<Self>) {
        if self.tab != View::Visualize {
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
            crate::tf::tree(self.draft.connection_id, cx).as_ref(),
            Settings::get(cx).point_budget,
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

    /// Points the tree at the newest message, when the tree is what is showing.
    ///
    /// Here rather than in `response` for the same reason the scene is: the
    /// rows are rebuilt once per message, and `response` runs once per frame.
    fn sync_tree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.tab, View::Pretty | View::Visualize) {
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
            return gpui::div().into_any_element();
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

    /// Pins the current message. Pinning again re-pins to the newest.
    fn freeze(&mut self, cx: &mut Context<Self>) {
        let (value, count) = {
            let incoming = self.incoming.lock().expect("incoming mutex");
            (incoming.value.clone(), incoming.count)
        };
        self.frozen = value.map(|value| (value, count));
        // Freezing with nowhere to look at the result is a control that does
        // nothing visible, so it takes the panel to the view it is for.
        self.tab = View::Diff;
        cx.notify();
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
        // The role decides which view the Visualize tab gets, and the stats row
        // below consumes `schema` on its way into the chip.
        let schema_name = schema.clone();

        // Only the views this response can fill. A `std_msgs/String` has no
        // numbers to plot and nothing to draw, and tabs for both would be two
        // places in the strip that open on an apology.
        let role = crate::viz::role_for(schema_name.as_deref().unwrap_or_default());
        let offered = View::offered(
            views::Offers::of(&role, &history, self.frozen.is_some())
                .recorded(!self.past.is_empty()),
        );
        let active = active.or_pretty(&offered);

        // A segmented bar rather than document tabs: these are views of one
        // response, not things that can be closed. Drawn only once something has
        // arrived — before that every tab shows the same empty state, so the row
        // costs height and carries nothing.
        let labels = offered.clone();
        let tabs = value.is_some().then(|| {
            TabBar::new("response-views")
                .segmented()
                .selected_index(active.index_in(&offered))
                .children(labels.iter().map(|tab| Tab::new().label(tab.label())))
                .on_click(cx.listener(move |this, index: &usize, _, cx| {
                    if let Some(tab) = offered.get(*index) {
                        this.tab = *tab;
                        cx.notify();
                    }
                }))
        });

        // "Freeze" only once there is something to freeze, and once there is a
        // pin the chip says which message it holds — which is the one thing a
        // person needs to know to trust what the diff is telling them.
        let pinned = self.frozen.as_ref().map(|(_, at)| *at);
        let stats = h_flex()
            .gap_3()
            .flex_shrink_0()
            .when(value.is_some(), |row| {
                row.child(
                    Button::new("freeze")
                        .ghost()
                        .xsmall()
                        .label(match pinned {
                            Some(at) => SharedString::from(format!("Frozen at #{at}")),
                            None => SharedString::from("Freeze"),
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.freeze(cx))),
                )
            })
            .child(tokens::meta("Messages", count.to_string(), cx))
            .when_some(self.stats(cx), |row, stats| {
                row.when_some(stats.hz_label(), |row, hz| {
                    row.child(tokens::meta("Rate", hz, cx))
                })
                .when_some(stats.bandwidth_label(), |row, rate| {
                    row.child(tokens::meta("Bandwidth", rate, cx))
                })
                .when_some(stats.latency_label(), |row, latency| {
                    row.child(tokens::meta("Latency", latency, cx))
                })
            })
            .when_some(schema, |row, schema| {
                row.child(self.schema_chip(schema, cx))
            });

        let body = match (&value, active) {
            (Some(value), View::Raw) => views::raw(value, cx),
            (Some(_), View::Plot) => views::plot(&history, cx),
            (Some(value), View::Diff) => {
                views::changes(self.frozen.as_ref().map(|(value, _)| value), value, cx)
            }
            (_, View::History) => self.past_runs(cx),
            (Some(_), View::Pretty) => self.tree(cx),
            (Some(value), View::Visualize) => {
                views::visualize(&role, value, self.scene.as_ref(), self.tree(cx), cx)
            }
            (None, _) => tokens::empty_state(
                IconName::Inbox,
                if running {
                    "Waiting for the first message…"
                } else {
                    "Not running"
                },
                match (running, self.draft.kind) {
                    (true, _) => "The subscription is open; nothing has arrived yet.",
                    // A topic is not sent. "Send the request" is the wording for
                    // the three kinds that are.
                    (false, RequestKind::Topic) => "Subscribe to watch this topic.",
                    (false, _) => "Send the request to see its response here.",
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
            // No header at all until something arrives. Before that it is a
            // strip of tabs that all show the same empty state next to the
            // words "Messages 0" — which the empty state under it already
            // says, at length.
            .when_some(tabs, |card, tabs| {
                card.child(tokens::card_header(cx).child(tabs).child(stats))
            })
            .child(
                tokens::card_body()
                    .id("response-body")
                    .overflow_scroll()
                    .child(body),
            )
            .into_any_element()
    }
}

/// A response as one line, for a row in a list.
///
/// `value::preview` pretty-prints across several lines, which is right in a
/// pane and wrong in a row — it pushes the timestamp beside it down to the
/// closing brace. The leaves are already flat, so this is the same information
/// with the shape a list wants.
fn one_line(value: &CanonicalValue) -> String {
    const SHOWN: usize = 4;
    let leaves = crate::value::leaves(value);
    if leaves.is_empty() {
        return crate::value::scalar(value);
    }
    let mut parts: Vec<String> = leaves
        .iter()
        .take(SHOWN)
        .map(|(path, shown)| format!("{path} {shown}"))
        .collect();
    if leaves.len() > SHOWN {
        parts.push(format!("+{} more", leaves.len() - SHOWN));
    }
    parts.join("   ")
}

/// Asks a node what parameters it has and what they hold.
///
/// Free rather than a method because it runs twice — once on its own and once
/// after a write — and a panel that has been closed in between should not stop
/// the second one from finishing.
async fn read_parameters(
    pipeline: &rw_pipeline::CanonicalPipeline,
    session: ConnectionId,
    node: &str,
) -> Result<(CanonicalValue, BTreeMap<String, param::Kind>), String> {
    let listed = pipeline
        .call_service(
            session,
            &param::service(node, param::LIST),
            param::list_request(),
        )
        .await
        .map_err(|error| format!("{node} would not list its parameters: {error}"))?;

    let names = param::decode_list(&listed).ok_or_else(|| {
        format!(
            "{node} answered {}, but not with a list of names",
            param::LIST
        )
    })?;

    if names.is_empty() {
        return Ok((CanonicalValue::Struct(Default::default()), BTreeMap::new()));
    }

    let got = pipeline
        .call_service(
            session,
            &param::service(node, param::GET),
            param::get_request(&names),
        )
        .await
        .map_err(|error| format!("{node} would not give its parameters: {error}"))?;

    let values = param::decode_values(&names, &got).ok_or_else(|| {
        format!(
            "{node} answered with a different number of values than the {} names it was asked for",
            names.len()
        )
    })?;

    Ok((values, param::read_kinds(&names, &got)))
}

/// Sets parameters on a node, and returns how many it took.
///
/// A refusal is reported per parameter, so a call that "succeeded" can still
/// have changed nothing. Naming which ones were refused, and why the node said,
/// is the whole difference between this and `ros2 param set` in a terminal.
async fn write_parameters(
    pipeline: &rw_pipeline::CanonicalPipeline,
    session: ConnectionId,
    node: &str,
    values: &CanonicalValue,
    kinds: &BTreeMap<String, param::Kind>,
) -> Result<String, String> {
    let CanonicalValue::Struct(fields) = values else {
        return Err("Nothing to set".into());
    };
    if fields.is_empty() {
        return Err("Read the node's parameters before setting them".into());
    }
    let names: Vec<String> = fields.keys().cloned().collect();

    let response = pipeline
        .call_service(
            session,
            &param::service(node, param::SET),
            param::set_request(values, kinds),
        )
        .await
        .map_err(|error| format!("{node} would not set its parameters: {error}"))?;

    let refused = param::decode_set_results(&names, &response);
    if refused.is_empty() {
        let count = names.len();
        let plural = if count == 1 {
            "parameter"
        } else {
            "parameters"
        };
        return Ok(format!("{count} {plural}"));
    }

    let reasons: Vec<String> = refused
        .iter()
        .map(|(name, reason)| format!("{name}: {reason}"))
        .collect();
    Err(format!("{node} refused {}", reasons.join(", ")))
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

    /// Saved as an id and nothing more: everything else about the request is
    /// already in storage, and a stale copy here would only contradict it.
    fn dump(&self, _cx: &App) -> gpui_component::dock::PanelState {
        let mut state = gpui_component::dock::PanelState::new(self);
        state.info = crate::layout::request_panel(self.saved.id);
        state
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
        self.sync_name(cx);
        self.sync_scene(cx);
        self.sync_tree(window, cx);
        let bar = self.request_bar(cx);
        // Shown for topics too: a topic request that can only be watched is half
        // a request, and the message you publish is the same form. Absent
        // entirely when the message has no fields to fill in.
        let payload = self.payload(cx);
        // A parameter request has no second half. The form above *is* the
        // reading — same rows, same labels, same values — so a response card
        // under it drew the node's answer twice. The write's outcome comes back
        // as a run result and lands where every other one does.
        let response = (self.draft.kind != RequestKind::Param).then(|| self.response(cx));
        let problem = self.problem(cx);

        v_flex()
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            // Both of these put the offer list away. It is drawn deferred, so
            // it paints over the dropdown that opened the menu these came from
            // — leaving the user looking at a list of topics where they expected
            // their choice to appear.
            .on_action(cx.listener(|this, action: &SetKind, _, cx| {
                this.draft.kind = kind_from_discriminant(action.0);
                this.offers_open = false;
                cx.notify();
            }))
            .on_action(cx.listener(|this, action: &UseEnvironment, _, cx| {
                this.draft.connection_id = Some(action.0);
                this.offers_open = false;
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
            // The head is fixed: kind and target stay put while the response
            // scrolls, which is the whole point of a request bar.
            //
            // Nothing behind it. The bar is already a bordered box of its own,
            // and a tinted strip around it made two nested boxes where there is
            // one control — it floats on the pane like everything else does.
            .child(
                // Only the bar. The request's name is the tab above this, and a
                // headline repeating it cost a row of every editor; renaming
                // lives in the sidebar, where you can see the name you are
                // changing beside its neighbours.
                v_flex().flex_shrink_0().px_4().pt_3().pb_1().child(bar),
            )
            .children(problem)
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .p_4()
                    .gap_3()
                    .children(payload)
                    .children(response),
            )
    }
}

/// Shows a message definition, the way `ros2 interface show` would.
///
/// The whole bundle, nested `MSG:` sections and all, because a `PointCloud2`
/// is not readable without `PointField` beside it — and the CLI makes you run
/// it again per type.
fn show_definition(
    name: SharedString,
    hash: String,
    text: String,
    window: &mut Window,
    cx: &mut App,
) {
    // The hash is the identity the registry keyed this by, and it is how you
    // tell two robots' `std_msgs/Header` apart when both are connected. Short
    // enough to read out, long enough to be unique across one workspace.
    let short: String = hash.chars().take(12).collect();
    window.open_dialog(cx, move |dialog, _window, cx| {
        dialog.title(name.clone()).w(px(640.)).child(
            v_flex()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("as this system described it · {short}")),
                )
                .child(
                    div()
                        .id("definition")
                        .max_h(px(460.))
                        .overflow_y_scroll()
                        .p_3()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().muted)
                        .text_xs()
                        .font_family(cx.theme().mono_font_family.clone())
                        .child(text.clone()),
                ),
        )
    });
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
            RequestKind::Param,
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
        let labels: Vec<_> = View::ALL.iter().map(|tab| tab.label()).collect();
        assert_eq!(
            labels,
            ["Pretty", "Raw", "Visualize", "Plot", "Diff", "History"]
        );
    }
}
