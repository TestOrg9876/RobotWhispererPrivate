//! Live transport sessions: opening connections, tracking status, and
//! mirroring discovery into the UI.
//!
//! `rw_pipeline::CanonicalPipeline` already keeps one upstream subscription per
//! `(connection, topic)` and ref-counts a zero-copy fan-out, so this layer only
//! has to own the connection lifecycle and republish `watch` channels as GPUI
//! notifications.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{Context, Entity, EventEmitter, Task};
use rw_core::domain::{Connection, TransportConfig};
use rw_pipeline::CanonicalPipeline;
use rw_transport::{ConnectionId, ConnectionStatus, Discovery, ReplayCommand, ReplayProgress};

/// Where a connection currently is in its lifecycle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
    Failed(String),
}

impl Status {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Reconnecting => "reconnecting",
            Self::Failed(_) => "failed",
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected)
    }

    /// Detail worth showing next to the label, if any.
    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::Failed(reason) => Some(reason),
            _ => None,
        }
    }
}

impl From<ConnectionStatus> for Status {
    fn from(status: ConnectionStatus) -> Self {
        match status {
            ConnectionStatus::Disconnected => Self::Disconnected,
            ConnectionStatus::Connecting => Self::Connecting,
            ConnectionStatus::Connected => Self::Connected,
            ConnectionStatus::Reconnecting => Self::Reconnecting,
            ConnectionStatus::Failed(reason) => Self::Failed(reason),
        }
    }
}

/// A connection's live state.
#[derive(Debug, Default)]
pub struct Live {
    pub status: Status,
    pub session: Option<ConnectionId>,
    pub discovery: Discovery,
    /// The connection's name, kept here so events can say which system they are
    /// about without the session store having to reach into the workspace.
    pub name: String,
    /// Where playback has reached, for a connection that is a recording.
    ///
    /// `None` on a live system — there is nothing to scrub — which is also how
    /// the transport bar knows whether to appear at all.
    pub replay: Option<ReplayProgress>,
}

/// Something worth putting in the console.
#[derive(Debug, Clone)]
pub enum Notice {
    Info(String),
    Error(String),
    /// A connection changing state.
    ///
    /// Its own variant rather than an `Info` with a recognisable sentence in
    /// it. This is the one thing worth interrupting someone for — a robot
    /// dropping while they are looking at a dashboard is otherwise a colour
    /// change in the footer — and deciding that by matching on message text
    /// would break the first time someone reworded one.
    Link {
        connection: String,
        text: String,
        /// The state it moved to, which is what decides whether this is worth
        /// interrupting for. Carried rather than a bare severity so the
        /// decision is about what happened, not about how loudly it was
        /// phrased.
        status: Status,
    },
    /// A line the robot itself wrote, off `/rosout`.
    ///
    /// The same route as the app's own notices rather than a console of its
    /// own: "did my request go out before that node complained" is a question
    /// about one ordering, and two windows cannot answer it.
    Robot {
        connection: String,
        entry: crate::log::Entry,
    },
}

/// How loud a notice is, for the console's filter and its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warn,
    Error,
}

impl Notice {
    pub fn text(&self) -> String {
        match self {
            Self::Info(text) | Self::Error(text) | Self::Link { text, .. } => text.clone(),
            Self::Robot { connection, entry } => format!("{connection}  {}", entry.text()),
        }
    }

    pub fn severity(&self) -> Severity {
        match self {
            Self::Info(_) => Severity::Info,
            Self::Error(_) => Severity::Error,
            Self::Link { status, .. } => match status {
                Status::Failed(_) => Severity::Error,
                _ => Severity::Info,
            },
            Self::Robot { entry, .. } => match entry.level {
                crate::log::Level::Debug | crate::log::Level::Info => Severity::Info,
                crate::log::Level::Warn => Severity::Warn,
                crate::log::Level::Error | crate::log::Level::Fatal => Severity::Error,
            },
        }
    }
}

/// Emitted as sessions change, so the console can log and views can refresh.
#[derive(Debug, Clone)]
pub struct SessionEvent(pub Notice);

/// Owns the pipeline and every open session.
pub struct Sessions {
    pipeline: Arc<CanonicalPipeline>,
    live: HashMap<i64, Live>,
    /// Connections whose `/rosout` is already being followed, so discovery
    /// updating twenty times a second does not open twenty subscriptions.
    watching_log: std::collections::HashSet<i64>,
}

impl EventEmitter<SessionEvent> for Sessions {}

impl Sessions {
    pub fn new(pipeline: Arc<CanonicalPipeline>) -> Self {
        Self {
            pipeline,
            live: HashMap::new(),
            watching_log: std::collections::HashSet::new(),
        }
    }

    pub fn pipeline(&self) -> Arc<CanonicalPipeline> {
        Arc::clone(&self.pipeline)
    }

    pub fn live(&self, connection: i64) -> Option<&Live> {
        self.live.get(&connection)
    }

    pub fn status(&self, connection: i64) -> Status {
        self.live
            .get(&connection)
            .map(|live| live.status.clone())
            .unwrap_or_default()
    }

    pub fn session(&self, connection: i64) -> Option<ConnectionId> {
        self.live.get(&connection).and_then(|live| live.session)
    }

    pub fn discovery(&self, connection: i64) -> Option<&Discovery> {
        self.live.get(&connection).map(|live| &live.discovery)
    }

    /// Every connection that has been opened, live state and all.
    pub fn connections(&self) -> impl Iterator<Item = (i64, &Live)> {
        self.live.iter().map(|(id, live)| (*id, live))
    }

    pub fn connected_count(&self) -> usize {
        self.live
            .values()
            .filter(|live| live.status.is_connected())
            .count()
    }

    /// Changes how a recording is being played.
    ///
    /// Fire-and-forget: the transport publishes the new state on its progress
    /// channel, which is already being mirrored, so the bar redraws from what
    /// actually happened rather than from what was asked for.
    pub fn replay_control(
        &self,
        connection: i64,
        command: ReplayCommand,
        cx: &mut Context<Self>,
    ) -> Option<Task<()>> {
        let session = self.session(connection)?;
        let pipeline = self.pipeline();
        Some(cx.spawn(async move |_, _| {
            pipeline.replay_control(session, command).await;
        }))
    }

    /// Puts a line in the console.
    ///
    /// The console already listens to this store for connection events, so
    /// everything the app has to say arrives by one route and in one order.
    pub fn announce(&mut self, notice: Notice, cx: &mut Context<Self>) {
        cx.emit(SessionEvent(notice));
    }

    /// Opens the transport for `connection` and starts mirroring its status and
    /// discovery channels.
    pub fn connect(&mut self, connection: &Connection, cx: &mut Context<Self>) -> Task<()> {
        let id = connection.id;
        let name = connection.name.clone();
        let config = connection.config.clone();
        let pipeline = self.pipeline();

        let live = self.live.entry(id).or_default();
        live.status = Status::Connecting;
        live.name = name.clone();
        cx.emit(SessionEvent(Notice::Link {
            connection: name.clone(),
            text: format!("connecting to {name}"),
            status: Status::Connecting,
        }));
        cx.notify();

        cx.spawn(async move |sessions, cx| {
            let opened = match &config {
                TransportConfig::Dummy {} => pipeline.open_dummy().await,
                TransportConfig::FoxgloveWs { url, .. } => {
                    pipeline.open_foxglove(url.clone()).await
                }
                TransportConfig::Rosbridge { url } => pipeline.open_rosbridge(url.clone()).await,
                TransportConfig::NativeRos2 { .. } => Err(rw_transport::TransportError::Other(
                    "native ROS 2 is not implemented yet".into(),
                )),
                TransportConfig::Replay { recording } => {
                    match rw_record::Recording::read(recording) {
                        Ok(recording) => pipeline.open_replay(recording).await,
                        Err(error) => Err(rw_transport::TransportError::Other(format!(
                            "could not read the recording: {error}"
                        ))),
                    }
                }
            };

            let session = match opened {
                Ok(session) => session,
                Err(error) => {
                    sessions
                        .update(cx, |sessions, cx| {
                            let reason = error.to_string();
                            sessions.live.entry(id).or_default().status =
                                Status::Failed(reason.clone());
                            cx.emit(SessionEvent(Notice::Link {
                                connection: name.clone(),
                                text: format!("{name}: {reason}"),
                                status: Status::Failed(reason.clone()),
                            }));
                            cx.notify();
                        })
                        .ok();
                    return;
                }
            };

            let transport = match pipeline.transport(session).await {
                Ok(transport) => transport,
                Err(error) => {
                    sessions
                        .update(cx, |sessions, cx| {
                            sessions.live.entry(id).or_default().status =
                                Status::Failed(error.to_string());
                            cx.notify();
                        })
                        .ok();
                    return;
                }
            };

            sessions
                .update(cx, |sessions, cx| {
                    let live = sessions.live.entry(id).or_default();
                    live.session = Some(session);
                    live.status = Status::Connected;
                    cx.emit(SessionEvent(Notice::Link {
                        connection: name.clone(),
                        text: format!("connected to {name}"),
                        status: Status::Connected,
                    }));
                    cx.notify();
                })
                .ok();

            // Mirror the transport's watch channels. One loop each: `select`
            // would hold a mutable borrow of both receivers across the await,
            // which blocks reading their values afterwards.
            let mut status_rx = transport.status();
            let mut discovery_rx = transport.discovery();

            // `changed()` only fires on *subsequent* sends, so seed from the
            // current value first — a transport that published its discovery
            // before we subscribed would otherwise look like it had none.
            let initial_status = Status::from(status_rx.borrow().clone());
            let initial_discovery = discovery_rx.borrow().clone();
            sessions
                .update(cx, |sessions, cx| {
                    if let Some(live) = sessions.live.get_mut(&id) {
                        live.status = initial_status;
                        live.discovery = initial_discovery;
                    }
                    sessions.follow_log(id, cx);
                    cx.notify();
                })
                .ok();

            let statuses = {
                let sessions = sessions.clone();
                let mut cx = cx.clone();
                async move {
                    while status_rx.changed().await.is_ok() {
                        let status = Status::from(status_rx.borrow().clone());
                        let alive = sessions
                            .update(&mut cx, |sessions, cx| {
                                let Some(live) = sessions.live.get_mut(&id) else {
                                    return false;
                                };
                                live.status = status;
                                cx.notify();
                                true
                            })
                            .unwrap_or(false);
                        if !alive {
                            break;
                        }
                    }
                }
            };

            let discoveries = {
                let sessions = sessions.clone();
                let mut cx = cx.clone();
                async move {
                    while discovery_rx.changed().await.is_ok() {
                        let discovery = discovery_rx.borrow().clone();
                        let alive = sessions
                            .update(&mut cx, |sessions, cx| {
                                let Some(live) = sessions.live.get_mut(&id) else {
                                    return false;
                                };
                                live.discovery = discovery;
                                sessions.follow_log(id, cx);
                                cx.notify();
                                true
                            })
                            .unwrap_or(false);
                        if !alive {
                            break;
                        }
                    }
                }
            };

            // A third loop only when this transport is a recording. Seeded the
            // same way: playback starts on connect, so the first `changed()`
            // may already be behind.
            let playback = {
                let sessions = sessions.clone();
                let mut cx = cx.clone();
                let progress = transport.replay();
                async move {
                    let Some(mut progress_rx) = progress else {
                        return;
                    };
                    loop {
                        let progress = *progress_rx.borrow();
                        let alive = sessions
                            .update(&mut cx, |sessions, cx| {
                                let Some(live) = sessions.live.get_mut(&id) else {
                                    return false;
                                };
                                live.replay = Some(progress);
                                cx.notify();
                                true
                            })
                            .unwrap_or(false);
                        if !alive || progress_rx.changed().await.is_err() {
                            break;
                        }
                    }
                }
            };

            futures_util::future::join3(statuses, discoveries, playback).await;
        })
    }

    /// Subscribes to `/rosout` on a connection that advertises it.
    ///
    /// The same reasoning as `/tf`: nobody wants to remember to turn on the
    /// robot's log, and a console that shows only this app's own events is
    /// half a console. Idempotent — called on every discovery update.
    fn follow_log(&mut self, connection: i64, cx: &mut Context<Self>) {
        if self.watching_log.contains(&connection) {
            return;
        }
        let Some(live) = self.live.get(&connection) else {
            return;
        };
        let Some(session) = live.session else { return };
        if !live
            .discovery
            .topics
            .iter()
            .any(|topic| topic.name == crate::log::TOPIC)
        {
            return;
        }
        self.watching_log.insert(connection);

        let name = live.name.clone();
        let pipeline = self.pipeline();
        let (sender, receiver) = std::sync::mpsc::channel();
        cx.spawn(async move |sessions, cx| {
            let opened = pipeline
                .subscribe_topic(session, crate::log::TOPIC, move |_handle, frame, _lossy| {
                    if let Some(entry) = crate::log::decode(&frame.value) {
                        sender.send(entry).ok();
                    }
                })
                .await;
            if let Err(error) = opened {
                tracing::warn!("could not follow {}: {error}", crate::log::TOPIC);
                sessions
                    .update(cx, |sessions, _| {
                        sessions.watching_log.remove(&connection);
                    })
                    .ok();
                return;
            }
            // Frames arrive off the UI thread and an event can only be emitted
            // on it, so they queue and are drained on this side.
            loop {
                crate::tick::sleep(std::time::Duration::from_millis(100), cx).await;
                let drained: Vec<crate::log::Entry> = receiver.try_iter().collect();
                let alive = sessions
                    .update(cx, |sessions, cx| {
                        if !sessions.live.contains_key(&connection) {
                            return false;
                        }
                        for entry in drained {
                            cx.emit(SessionEvent(Notice::Robot {
                                connection: name.clone(),
                                entry,
                            }));
                        }
                        true
                    })
                    .unwrap_or(false);
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    /// Closes the transport and forgets the session.
    pub fn disconnect(&mut self, connection: i64, cx: &mut Context<Self>) -> Task<()> {
        let pipeline = self.pipeline();
        let removed = self.live.remove(&connection);
        self.watching_log.remove(&connection);
        let name = removed
            .as_ref()
            .map(|live| live.name.clone())
            .unwrap_or_default();
        let session = removed.and_then(|live| live.session);
        cx.notify();

        cx.spawn(async move |sessions, cx| {
            let Some(session) = session else { return };
            let outcome = pipeline.close(session).await;
            sessions
                .update(cx, |_, cx| {
                    match outcome {
                        // Named, because with several systems connected at once
                        // a bare "disconnected" says nothing worth logging.
                        Ok(()) => cx.emit(SessionEvent(Notice::Link {
                            connection: name.clone(),
                            text: format!("disconnected from {name}"),
                            status: Status::Disconnected,
                        })),
                        Err(error) => cx.emit(SessionEvent(Notice::Link {
                            connection: name.clone(),
                            text: format!("{name}: close failed: {error}"),
                            status: Status::Failed(error.to_string()),
                        })),
                    }
                    cx.notify();
                })
                .ok();
        })
    }
}

/// Handles reachable from any view.
pub struct RobotWhisperer {
    pub workspace: Entity<crate::workspace::Workspace>,
    pub sessions: Entity<Sessions>,
    /// What each request is doing, so the sidebar can show it without owning the
    /// panels that know.
    pub runs: Entity<crate::runs::Runs>,
    /// The graphics device every 3D pane draws with.
    pub gpu: Entity<crate::gpu::Gpu>,
    /// Captures what arrives while recording is on.
    pub recorder: Entity<crate::recorder::Recorder>,
    /// One transform tree per connection: where every frame of every robot is.
    pub tf: Entity<crate::tf::TfStore>,
}

impl gpui::Global for RobotWhisperer {}

impl RobotWhisperer {
    pub fn global(cx: &gpui::App) -> &Self {
        cx.global::<Self>()
    }

    /// The globals, if they have been installed yet.
    ///
    /// For the handful of callers that run either side of `init` — the menu bar
    /// is built from application state, and is also what a bare `gpui` test
    /// harness has none of.
    pub fn try_global(cx: &gpui::App) -> Option<&Self> {
        cx.try_global::<Self>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_status_maps_onto_ui_status() {
        assert_eq!(Status::from(ConnectionStatus::Connected), Status::Connected);
        assert_eq!(
            Status::from(ConnectionStatus::Disconnected),
            Status::Disconnected
        );
        assert_eq!(
            Status::from(ConnectionStatus::Failed("nope".into())),
            Status::Failed("nope".into())
        );
    }

    #[test]
    fn only_connected_reports_connected() {
        assert!(Status::Connected.is_connected());
        for status in [
            Status::Disconnected,
            Status::Connecting,
            Status::Reconnecting,
            Status::Failed("x".into()),
        ] {
            assert!(!status.is_connected(), "{status:?}");
        }
    }

    #[test]
    fn only_failure_carries_detail() {
        assert_eq!(Status::Failed("boom".into()).detail(), Some("boom"));
        assert_eq!(Status::Connected.detail(), None);
        assert_eq!(Status::Connecting.detail(), None);
    }

    #[test]
    fn labels_are_stable() {
        assert_eq!(Status::Disconnected.label(), "disconnected");
        assert_eq!(Status::Connecting.label(), "connecting");
        assert_eq!(Status::Connected.label(), "connected");
        assert_eq!(Status::Reconnecting.label(), "reconnecting");
        assert_eq!(Status::Failed(String::new()).label(), "failed");
    }
}
