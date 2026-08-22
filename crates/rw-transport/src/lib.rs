#![deny(missing_debug_implementations)]
#![deny(unused_must_use)]

pub mod action;
pub mod task;
pub mod time;

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use rw_canonical::{CanonicalSchema, CanonicalValue, Dialect, SchemaId};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

pub mod perf {
    pub use rw_wire::{now_ns, perf_trace_enabled, set_perf_trace_enabled, PerfTrace};
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("transport not connected")]
    NotConnected,
    #[error("unknown topic '{0}'")]
    UnknownTopic(String),
    #[error("unknown service '{0}'")]
    UnknownService(String),
    #[error("unknown action '{0}'")]
    UnknownAction(String),
    #[error("schema unavailable for '{0}': {1}")]
    Schema(String, String),
    #[error("codec error: {0}")]
    Codec(String),
    #[error("transport error: {0}")]
    Other(String),
    #[error("transport closed")]
    Closed,
}

pub type TransportResult<T> = Result<T, TransportError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConnectionId(pub Uuid);

impl ConnectionId {
    pub fn new() -> Self {
        ConnectionId(Uuid::new_v4())
    }
}

impl Default for ConnectionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub timestamp_ns: u64,
    pub schema: Arc<CanonicalSchema>,
    pub value: CanonicalValue,
    pub raw: Option<Arc<[u8]>>,
    pub perf: Option<rw_wire::PerfTrace>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
    Failed(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discovery {
    pub topics: Vec<TopicDescriptor>,
    pub services: Vec<TargetDescriptor>,
    pub actions: Vec<TargetDescriptor>,
    pub dialect: Option<Dialect>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub dependency_schemas: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TopicDescriptor {
    pub name: String,
    pub schema_name: String,
    pub schema_id: Option<SchemaId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_definition: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TargetDescriptor {
    pub name: String,
    pub schema_name: String,
    pub schema_id: Option<SchemaId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_definition: Option<String>,
}

#[derive(Debug)]
pub struct Subscription {
    pub frames: mpsc::Receiver<Frame>,
    pub schema: Arc<CanonicalSchema>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SubscribeOptions {
    pub target_hz: Option<f32>,
    pub queue_length: Option<u32>,
}

pub fn default_target_hz_for_schema(schema_name: &str) -> Option<f32> {
    let lower = schema_name.to_ascii_lowercase();
    if lower.contains("image")
        || lower.contains("compressedimage")
        || lower.contains("foxglove.rawimage")
    {
        return Some(30.0);
    }
    if lower.contains("pointcloud") || lower.contains("occupancygrid") {
        return Some(15.0);
    }
    if lower.contains("marker") || lower.contains("tfmessage") || lower.contains("posearray") {
        return Some(30.0);
    }
    if lower.contains("jointstate") || lower.contains("imu") || lower.contains("dynamicjointstate")
    {
        return Some(200.0);
    }
    Some(60.0)
}

impl SubscribeOptions {
    pub fn with_default_for_schema(mut self, schema_name: &str) -> Self {
        if self.target_hz.is_none() {
            self.target_hz = default_target_hz_for_schema(schema_name);
        }
        self
    }

    pub fn rosbridge_throttle_ms(&self) -> Option<u32> {
        match self.target_hz {
            Some(hz) if hz > 0.0 => {
                let ms = (1000.0 / hz).round() as i64;
                Some(ms.clamp(0, u32::MAX as i64) as u32)
            }
            Some(_) => Some(0),
            None => None,
        }
    }

    pub fn min_interval_ns(&self) -> Option<u64> {
        match self.target_hz {
            Some(hz) if hz > 0.0 => Some((1_000_000_000.0 / hz as f64) as u64),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct ActionGoalStream {
    pub feedback: mpsc::Receiver<CanonicalValue>,
    pub result: tokio::sync::oneshot::Receiver<TransportResult<CanonicalValue>>,
    pub cancel_token: ActionCancelToken,
}

#[derive(Debug, Clone)]
pub struct ActionCancelToken {
    pub action: String,
    pub goal_id: String,
}

/// Where playback of a recording has reached.
///
/// A recording is a transport like any other, which is what lets requests,
/// panes and the canonical fan-out work on it unchanged — but it is the one
/// kind of transport that can be paused, sped up and seeked, because the data
/// already exists. This is that difference, and it is `None` on every live
/// transport: a robot has no duration and cannot be rewound.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReplayProgress {
    pub at_ns: u64,
    pub duration_ns: u64,
    pub playing: bool,
    /// 1.0 is the rate it was captured at.
    pub speed: f32,
    /// Whether it starts again at the end.
    pub looping: bool,
}

impl ReplayProgress {
    /// How far through, as 0..1. Zero for a recording with no duration.
    pub fn fraction(&self) -> f32 {
        if self.duration_ns == 0 {
            return 0.;
        }
        (self.at_ns as f64 / self.duration_ns as f64).clamp(0., 1.) as f32
    }
}

/// One change to how a recording is being played.
///
/// A single command rather than four methods, so that a transport which is not
/// a recording says so once.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReplayCommand {
    Playing(bool),
    /// Multiplier on the captured rate.
    Speed(f32),
    Looping(bool),
    /// Jump to a point, as 0..1 of the whole.
    Seek(f32),
}

#[cfg(not(target_family = "wasm"))]
#[async_trait]
pub trait Transport: Send + Sync + fmt::Debug {
    async fn connect(&self) -> TransportResult<()>;

    async fn disconnect(&self) -> TransportResult<()>;

    fn status(&self) -> watch::Receiver<ConnectionStatus>;

    fn discovery(&self) -> watch::Receiver<Discovery>;

    async fn subscribe_topic(&self, topic: &str) -> TransportResult<Subscription>;

    async fn subscribe_topic_with_options(
        &self,
        topic: &str,
        _options: SubscribeOptions,
    ) -> TransportResult<Subscription> {
        self.subscribe_topic(topic).await
    }

    async fn publish(&self, topic: &str, value: CanonicalValue) -> TransportResult<()>;

    async fn call_service(
        &self,
        service: &str,
        request: CanonicalValue,
    ) -> TransportResult<CanonicalValue>;

    async fn send_action_goal(
        &self,
        action: &str,
        goal: CanonicalValue,
    ) -> TransportResult<ActionGoalStream>;

    async fn cancel_action_goal(&self, token: &ActionCancelToken) -> TransportResult<()>;

    /// Playback state, if this transport is a recording.
    ///
    /// `None` — the default — means a live system: there is nothing to scrub.
    fn replay(&self) -> Option<watch::Receiver<ReplayProgress>> {
        None
    }

    /// Changes how the recording is being played. Ignored by a live transport.
    async fn replay_control(&self, _command: ReplayCommand) {}
}

#[cfg(target_family = "wasm")]
#[async_trait(?Send)]
pub trait Transport: fmt::Debug {
    async fn connect(&self) -> TransportResult<()>;
    async fn disconnect(&self) -> TransportResult<()>;
    fn status(&self) -> watch::Receiver<ConnectionStatus>;
    fn discovery(&self) -> watch::Receiver<Discovery>;
    async fn subscribe_topic(&self, topic: &str) -> TransportResult<Subscription>;
    async fn subscribe_topic_with_options(
        &self,
        topic: &str,
        _options: SubscribeOptions,
    ) -> TransportResult<Subscription> {
        self.subscribe_topic(topic).await
    }
    async fn publish(&self, topic: &str, value: CanonicalValue) -> TransportResult<()>;
    async fn call_service(
        &self,
        service: &str,
        request: CanonicalValue,
    ) -> TransportResult<CanonicalValue>;
    async fn send_action_goal(
        &self,
        action: &str,
        goal: CanonicalValue,
    ) -> TransportResult<ActionGoalStream>;
    async fn cancel_action_goal(&self, token: &ActionCancelToken) -> TransportResult<()>;

    /// Playback state, if this transport is a recording.
    ///
    /// `None` — the default — means a live system: there is nothing to scrub.
    fn replay(&self) -> Option<watch::Receiver<ReplayProgress>> {
        None
    }

    /// Changes how the recording is being played. Ignored by a live transport.
    async fn replay_control(&self, _command: ReplayCommand) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_and_discovery_roundtrip() {
        for status in [
            ConnectionStatus::Disconnected,
            ConnectionStatus::Connecting,
            ConnectionStatus::Connected,
            ConnectionStatus::Reconnecting,
            ConnectionStatus::Failed("oops".into()),
        ] {
            let json = serde_json::to_string(&status).unwrap();
            let back: ConnectionStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, back);
        }

        let discovery = Discovery {
            topics: vec![TopicDescriptor {
                name: "/scan".into(),
                schema_name: "sensor_msgs/LaserScan".into(),
                schema_id: Some(SchemaId::new("abc")),
                schema_definition: None,
            }],
            services: vec![],
            actions: vec![],
            dialect: Some(Dialect::Foxglove),
            dependency_schemas: std::collections::HashMap::new(),
        };
        let json = serde_json::to_string(&discovery).unwrap();
        let back: Discovery = serde_json::from_str(&json).unwrap();
        assert_eq!(discovery, back);
    }
}
