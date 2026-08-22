#![deny(missing_debug_implementations)]
//! A recording, dressed as a ROS system.
//!
//! Replay is a transport rather than a special path through the app, so a
//! request pointed at a recording behaves exactly like one pointed at a robot:
//! the same subscription, the same pipeline, the same panes. Nothing downstream
//! knows the difference, which is the point.
//!
//! What a recording cannot do is answer: there were no service calls or action
//! goals in it, so those are refused rather than faked.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use rw_canonical::{
    canonical_schema_id, viz_role_for_schema, CanonicalSchema, CanonicalValue, Dialect,
    ParsedSchema, SchemaKind,
};
use rw_record::{Cursor, Recording};
use rw_transport::task::{spawn_task, SpawnedTask};
use rw_transport::{
    ActionCancelToken, ActionGoalStream, ConnectionStatus, Discovery, Frame, ReplayCommand,
    Subscription, TopicDescriptor, Transport, TransportError, TransportResult,
};
use tokio::sync::{mpsc, watch, Mutex};

/// How often the clock is stepped. Fine enough that a 60 Hz recording plays
/// back evenly, coarse enough not to spin a core doing nothing.
const TICK_MS: u64 = 10;

async fn sleep_ms(ms: u64) {
    #[cfg(not(target_family = "wasm"))]
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    #[cfg(target_family = "wasm")]
    gloo_timers::future::TimeoutFuture::new(ms.min(i32::MAX as u64) as u32).await;
}

/// How playback is going, so a pane can draw a scrubber over it.
///
/// The transport trait's own type, so the UI can read it through
/// `Arc<dyn Transport>` without knowing a recording from a robot.
pub use rw_transport::ReplayProgress as Progress;

#[derive(Debug)]
pub struct ReplayTransport {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    recording: Recording,
    schemas: HashMap<String, Arc<CanonicalSchema>>,
    status_tx: watch::Sender<ConnectionStatus>,
    status_rx: watch::Receiver<ConnectionStatus>,
    discovery_tx: watch::Sender<Discovery>,
    discovery_rx: watch::Receiver<Discovery>,
    progress_tx: watch::Sender<Progress>,
    progress_rx: watch::Receiver<Progress>,
    cursor: Mutex<Cursor>,
    subscribers: Mutex<HashMap<String, Vec<mpsc::Sender<Frame>>>>,
    player: Mutex<Option<SpawnedTask>>,
}

impl ReplayTransport {
    pub fn new(recording: Recording) -> Self {
        let schemas: HashMap<String, Arc<CanonicalSchema>> = recording
            .topics
            .iter()
            .map(|topic| (topic.name.clone(), Arc::new(schema_for(topic))))
            .collect();

        let discovery = Discovery {
            topics: recording
                .topics
                .iter()
                .map(|topic| TopicDescriptor {
                    name: topic.name.clone(),
                    schema_name: topic.schema_name.clone(),
                    schema_id: schemas.get(&topic.name).map(|schema| schema.id.clone()),
                    schema_definition: topic.schema_definition.clone(),
                })
                .collect(),
            ..Default::default()
        };

        let cursor = Cursor::new(&recording);
        let progress = Progress {
            at_ns: 0,
            duration_ns: cursor.duration_ns(),
            playing: false,
            speed: cursor.speed,
            looping: cursor.looping,
        };

        let (status_tx, status_rx) = watch::channel(ConnectionStatus::Disconnected);
        let (discovery_tx, discovery_rx) = watch::channel(discovery);
        let (progress_tx, progress_rx) = watch::channel(progress);

        Self {
            inner: Arc::new(Inner {
                recording,
                schemas,
                status_tx,
                status_rx,
                discovery_tx,
                discovery_rx,
                progress_tx,
                progress_rx,
                cursor: Mutex::new(cursor),
                subscribers: Mutex::new(HashMap::new()),
                player: Mutex::new(None),
            }),
        }
    }

    /// Watches playback position, for a scrubber.
    pub fn progress(&self) -> watch::Receiver<Progress> {
        self.inner.progress_rx.clone()
    }

    pub async fn set_playing(&self, playing: bool) {
        let mut cursor = self.inner.cursor.lock().await;
        cursor.playing = playing;
        publish_progress(&self.inner, &cursor);
    }

    pub async fn set_speed(&self, speed: f32) {
        let mut cursor = self.inner.cursor.lock().await;
        cursor.speed = speed.clamp(0., 16.);
        publish_progress(&self.inner, &cursor);
    }

    pub async fn set_looping(&self, looping: bool) {
        let mut cursor = self.inner.cursor.lock().await;
        cursor.looping = looping;
        // Published like the rest: a toggle that changes nothing anyone can
        // see is a toggle that looks broken.
        publish_progress(&self.inner, &cursor);
    }

    /// Jumps to a point, as 0..1 of the whole.
    pub async fn seek(&self, progress: f32) {
        let mut cursor = self.inner.cursor.lock().await;
        cursor.seek(progress, &self.inner.recording);
        publish_progress(&self.inner, &cursor);
    }
}

fn publish_progress(inner: &Arc<Inner>, cursor: &Cursor) {
    let _ = inner.progress_tx.send(Progress {
        at_ns: cursor.at_ns(),
        duration_ns: cursor.duration_ns(),
        playing: cursor.playing,
        speed: cursor.speed,
        looping: cursor.looping,
    });
}

/// Rebuilds a topic's schema from what the recording kept of it.
///
/// The definition text is parsed if there is one; without it the topic still
/// replays, it just has no field types to describe. Recording the definition
/// alongside the frames is what makes a file portable — a machine that has
/// never seen the robot can still say what a message is.
fn schema_for(topic: &rw_record::Topic) -> CanonicalSchema {
    let definition = topic.schema_definition.clone().unwrap_or_default();
    let parsed = rw_schema_ros2::parse(SchemaKind::Message, &definition)
        .unwrap_or_else(|_| ParsedSchema::Message(Default::default()));
    CanonicalSchema {
        id: canonical_schema_id(&definition),
        name: topic.schema_name.clone(),
        kind: SchemaKind::Message,
        dialect: Dialect::Custom("replay".into()),
        definition,
        parsed,
        dependencies: vec![],
        viz_role: viz_role_for_schema(&topic.schema_name),
    }
}

#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
impl Transport for ReplayTransport {
    fn replay(&self) -> Option<watch::Receiver<Progress>> {
        Some(self.inner.progress_rx.clone())
    }

    async fn replay_control(&self, command: ReplayCommand) {
        match command {
            ReplayCommand::Playing(playing) => self.set_playing(playing).await,
            ReplayCommand::Speed(speed) => self.set_speed(speed).await,
            ReplayCommand::Looping(looping) => self.set_looping(looping).await,
            ReplayCommand::Seek(fraction) => self.seek(fraction).await,
        }
    }

    async fn connect(&self) -> TransportResult<()> {
        let _ = self.inner.status_tx.send(ConnectionStatus::Connecting);
        let _ = self.inner.status_tx.send(ConnectionStatus::Connected);

        let mut player = self.inner.player.lock().await;
        if player.is_some() {
            return Ok(());
        }
        // Opening a recording starts it: a file that sits silent until a hidden
        // control is found looks broken.
        {
            let mut cursor = self.inner.cursor.lock().await;
            cursor.playing = true;
            publish_progress(&self.inner, &cursor);
        }

        let inner = self.inner.clone();
        *player = Some(spawn_task(async move {
            loop {
                sleep_ms(TICK_MS).await;
                let due: Vec<(String, CanonicalValue, u64)> = {
                    let mut cursor = inner.cursor.lock().await;
                    let due = cursor
                        .advance(TICK_MS * 1_000_000, &inner.recording)
                        .into_iter()
                        .map(|message| {
                            (message.topic.clone(), message.value.clone(), message.at_ns)
                        })
                        .collect();
                    publish_progress(&inner, &cursor);
                    due
                };
                for (topic, value, at_ns) in due {
                    deliver(&inner, &topic, value, at_ns).await;
                }
            }
        }));
        Ok(())
    }

    async fn disconnect(&self) -> TransportResult<()> {
        let mut player = self.inner.player.lock().await;
        // One path for both targets. The local `spawn_task` this replaced
        // returned `()` on wasm, so there was nothing to abort there and the
        // loop outlived every disconnect; `rw_transport::task` carries a
        // cancel channel and stops on drop.
        if let Some(handle) = player.take() {
            rw_transport::task::cancel(handle);
        }
        self.inner.subscribers.lock().await.clear();
        let _ = self.inner.status_tx.send(ConnectionStatus::Disconnected);
        Ok(())
    }

    fn status(&self) -> watch::Receiver<ConnectionStatus> {
        self.inner.status_rx.clone()
    }

    fn discovery(&self) -> watch::Receiver<Discovery> {
        // Re-sent so a subscriber that arrived late still gets the topic list.
        let snapshot = self.inner.discovery_rx.borrow().clone();
        let _ = self.inner.discovery_tx.send(snapshot);
        self.inner.discovery_rx.clone()
    }

    async fn subscribe_topic(&self, topic: &str) -> TransportResult<Subscription> {
        let schema = self
            .inner
            .schemas
            .get(topic)
            .cloned()
            .ok_or_else(|| TransportError::UnknownTopic(topic.to_string()))?;
        let (tx, rx) = mpsc::channel(256);
        self.inner
            .subscribers
            .lock()
            .await
            .entry(topic.to_string())
            .or_default()
            .push(tx);
        Ok(Subscription { frames: rx, schema })
    }

    async fn publish(&self, _topic: &str, _value: CanonicalValue) -> TransportResult<()> {
        Err(TransportError::Other(
            "a recording cannot be published to".into(),
        ))
    }

    async fn call_service(
        &self,
        service: &str,
        _request: CanonicalValue,
    ) -> TransportResult<CanonicalValue> {
        Err(TransportError::UnknownService(service.to_string()))
    }

    async fn send_action_goal(
        &self,
        action: &str,
        _goal: CanonicalValue,
    ) -> TransportResult<ActionGoalStream> {
        Err(TransportError::UnknownAction(action.to_string()))
    }

    async fn cancel_action_goal(&self, _token: &ActionCancelToken) -> TransportResult<()> {
        Err(TransportError::Other(
            "nothing to cancel in a recording".into(),
        ))
    }
}

async fn deliver(inner: &Arc<Inner>, topic: &str, value: CanonicalValue, at_ns: u64) {
    let Some(schema) = inner.schemas.get(topic).cloned() else {
        return;
    };
    let frame = Frame {
        timestamp_ns: at_ns,
        schema,
        value,
        raw: None,
        perf: None,
    };
    let mut subscribers = inner.subscribers.lock().await;
    let Some(slot) = subscribers.get_mut(topic) else {
        return;
    };
    // A receiver that has gone away is dropped; a full queue is a slow pane and
    // the frame is skipped rather than blocking playback.
    slot.retain(|sender| sender.try_send(frame.clone()).is_ok() || !sender.is_closed());
}
