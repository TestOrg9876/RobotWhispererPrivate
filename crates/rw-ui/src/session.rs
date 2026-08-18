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
use rw_transport::{ConnectionId, ConnectionStatus, Discovery};

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
}

/// Something worth putting in the console.
#[derive(Debug, Clone)]
pub enum Notice {
    Info(String),
    Error(String),
}

/// Emitted as sessions change, so the console can log and views can refresh.
#[derive(Debug, Clone)]
pub struct SessionEvent(pub Notice);

/// Owns the pipeline and every open session.
pub struct Sessions {
    pipeline: Arc<CanonicalPipeline>,
    live: HashMap<i64, Live>,
}

impl EventEmitter<SessionEvent> for Sessions {}

impl Sessions {
    pub fn new(pipeline: Arc<CanonicalPipeline>) -> Self {
        Self {
            pipeline,
            live: HashMap::new(),
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

    pub fn connected_count(&self) -> usize {
        self.live
            .values()
            .filter(|live| live.status.is_connected())
            .count()
    }

    /// Opens the transport for `connection` and starts mirroring its status and
    /// discovery channels.
    pub fn connect(&mut self, connection: &Connection, cx: &mut Context<Self>) -> Task<()> {
        let id = connection.id;
        let name = connection.name.clone();
        let config = connection.config.clone();
        let pipeline = self.pipeline();

        self.live.entry(id).or_default().status = Status::Connecting;
        cx.emit(SessionEvent(Notice::Info(format!("connecting to {name}"))));
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
            };

            let session = match opened {
                Ok(session) => session,
                Err(error) => {
                    sessions
                        .update(cx, |sessions, cx| {
                            let reason = error.to_string();
                            sessions.live.entry(id).or_default().status =
                                Status::Failed(reason.clone());
                            cx.emit(SessionEvent(Notice::Error(format!("{name}: {reason}"))));
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
                    cx.emit(SessionEvent(Notice::Info(format!("connected to {name}"))));
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

            futures_util::future::join(statuses, discoveries).await;
        })
    }

    /// Closes the transport and forgets the session.
    pub fn disconnect(&mut self, connection: i64, cx: &mut Context<Self>) -> Task<()> {
        let pipeline = self.pipeline();
        let session = self.live.remove(&connection).and_then(|live| live.session);
        cx.notify();

        cx.spawn(async move |sessions, cx| {
            let Some(session) = session else { return };
            let outcome = pipeline.close(session).await;
            sessions
                .update(cx, |_, cx| {
                    match outcome {
                        Ok(()) => cx.emit(SessionEvent(Notice::Info("disconnected".into()))),
                        Err(error) => cx.emit(SessionEvent(Notice::Error(format!(
                            "close failed: {error}"
                        )))),
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
}

impl gpui::Global for RobotWhisperer {}

impl RobotWhisperer {
    pub fn global(cx: &gpui::App) -> &Self {
        cx.global::<Self>()
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
