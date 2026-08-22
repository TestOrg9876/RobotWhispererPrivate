#![deny(missing_debug_implementations)]

pub mod stats;

use std::collections::HashMap;
use std::sync::Arc;

use rw_canonical::CanonicalValue;
use rw_core::schema::{SchemaKind, SchemaRegistry};
use rw_session::{SubscriptionHandle, SubscriptionManager};
use rw_transport::{
    ActionCancelToken, ActionGoalStream, ConnectionId, Discovery, ReplayCommand, SubscribeOptions,
    Transport, TransportError, TransportResult,
};
use rw_transport_dummy::DummyTransport;
use rw_transport_foxglove_ws::{FoxgloveConfig, FoxgloveTransport};
use rw_transport_replay::ReplayTransport;
use rw_transport_rosbridge::{RosbridgeConfig, RosbridgeTransport};
use tokio::sync::Mutex;
use uuid::Uuid;

use rw_transport::task::{spawn_task, SpawnedTask};

fn spawn_detached<F>(future: F)
where
    F: std::future::Future<Output = ()> + MaybeSend + 'static,
{
    #[cfg(not(target_family = "wasm"))]
    {
        tokio::spawn(future);
    }
    #[cfg(target_family = "wasm")]
    {
        wasm_bindgen_futures::spawn_local(future);
    }
}

#[derive(Clone)]
pub struct CanonicalPipeline {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for CanonicalPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CanonicalPipeline")
            .field("has_schema_registry", &self.inner.schema_registry.is_some())
            .finish_non_exhaustive()
    }
}

struct Inner {
    subscription_manager: SubscriptionManager,
    /// One meter per subscription, behind a plain lock rather than the async
    /// one the rest of this uses: a view asks for a rate while it renders, and
    /// a render cannot await.
    meters: std::sync::Mutex<HashMap<String, stats::Meter>>,
    connections: Mutex<HashMap<ConnectionId, Arc<dyn Transport>>>,
    subscriptions: Mutex<HashMap<String, ActiveSubscription>>,
    action_goals: Mutex<HashMap<String, (ConnectionId, ActionCancelToken)>>,
    schema_registry: Option<Arc<SchemaRegistry>>,
    /// Which registry entry each connection's targets resolved to, keyed by
    /// connection and target name.
    ///
    /// A schema name is not an identity. Two robots can publish
    /// `sensor_msgs/Image` and mean different things by it — different ROS
    /// versions, different distros — and the registry holds both. This is what
    /// lets a view ask for *this* connection's answer rather than for whichever
    /// definition of that name sorted first. Behind a plain lock for the same
    /// reason `meters` is: a render reads it and a render cannot await.
    schemas: std::sync::Mutex<HashMap<(ConnectionId, String), String>>,
    /// The window every meter averages over, in nanoseconds.
    ///
    /// A setting rather than a constant, and held here rather than passed to
    /// each subscription, so that changing it reaches the meters already
    /// running as well as the next one opened.
    rate_window_ns: std::sync::atomic::AtomicU64,
}

#[derive(Debug)]
pub struct ActiveSubscription {
    #[allow(dead_code)]
    pub schema_id: String,
    #[allow(dead_code)]
    pub schema_name: String,
    #[allow(dead_code)]
    pub viz_role: String,
    #[allow(dead_code)]
    forwarder: SpawnedTask,
    _handle: SubscriptionHandle,
}

#[cfg(not(target_family = "wasm"))]
impl Drop for ActiveSubscription {
    fn drop(&mut self) {
        self.forwarder.abort();
    }
}

impl Default for CanonicalPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl CanonicalPipeline {
    pub fn new() -> Self {
        CanonicalPipeline::with_optional_registry(None)
    }

    pub fn with_schema_registry(registry: Arc<SchemaRegistry>) -> Self {
        CanonicalPipeline::with_optional_registry(Some(registry))
    }

    #[cfg_attr(target_family = "wasm", allow(clippy::arc_with_non_send_sync))]
    fn with_optional_registry(schema_registry: Option<Arc<SchemaRegistry>>) -> Self {
        CanonicalPipeline {
            inner: Arc::new(Inner {
                subscription_manager: SubscriptionManager::default(),
                meters: std::sync::Mutex::new(HashMap::new()),
                connections: Mutex::new(HashMap::new()),
                subscriptions: Mutex::new(HashMap::new()),
                action_goals: Mutex::new(HashMap::new()),
                schema_registry,
                schemas: std::sync::Mutex::new(HashMap::new()),
                rate_window_ns: std::sync::atomic::AtomicU64::new(stats::WINDOW_NS),
            }),
        }
    }

    pub fn schema_registry(&self) -> Option<&Arc<SchemaRegistry>> {
        self.inner.schema_registry.as_ref()
    }

    /// The registry entry that describes `target` on `connection`.
    ///
    /// This is the answer a view wants whenever it is about to build a form or
    /// flatten a message: looking the schema up by name instead returns
    /// whichever definition of that name happens to sort first, and with a ROS 1
    /// robot and a ROS 2 robot connected at once that is a coin toss between two
    /// different meanings of `std_msgs/Header`.
    ///
    /// `None` before discovery has described the target, or when the transport
    /// sent no definition for it — the name is then all there is to go on.
    pub fn schema_hash(&self, connection: ConnectionId, target: &str) -> Option<String> {
        self.inner
            .schemas
            .lock()
            .ok()?
            .get(&(connection, target.to_string()))
            .cloned()
    }

    /// Sets the window every rate and bandwidth reading is averaged over.
    ///
    /// Applies to the meters already running, so a topic being watched right
    /// now reports on the new window rather than after a resubscribe.
    pub fn set_rate_window_ns(&self, window_ns: u64) {
        let window_ns = window_ns.max(1);
        self.inner
            .rate_window_ns
            .store(window_ns, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut meters) = self.inner.meters.lock() {
            for meter in meters.values_mut() {
                meter.set_window(window_ns);
            }
        }
    }

    /// The window rates are averaged over, in nanoseconds.
    pub fn rate_window_ns(&self) -> u64 {
        self.inner
            .rate_window_ns
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Forgets what a connection described, when it goes away.
    pub fn forget_schemas(&self, connection: ConnectionId) {
        if let Ok(mut schemas) = self.inner.schemas.lock() {
            schemas.retain(|(owner, _), _| *owner != connection);
        }
    }

    #[allow(dead_code)]
    pub fn subscription_manager(&self) -> &SubscriptionManager {
        &self.inner.subscription_manager
    }

    pub async fn open_foxglove(&self, url: impl Into<String>) -> TransportResult<ConnectionId> {
        let transport = Arc::new(FoxgloveTransport::new(FoxgloveConfig::new(url)));
        transport.connect().await?;
        let id = ConnectionId::new();
        let dyn_transport: Arc<dyn Transport> = transport.clone() as Arc<dyn Transport>;
        self.inner
            .connections
            .lock()
            .await
            .insert(id, dyn_transport.clone());
        self.spawn_schema_watcher(id, dyn_transport);
        Ok(id)
    }

    pub async fn open_dummy(&self) -> TransportResult<ConnectionId> {
        let transport = Arc::new(DummyTransport::new());
        transport.connect().await?;
        let id = ConnectionId::new();
        let dyn_transport: Arc<dyn Transport> = transport.clone() as Arc<dyn Transport>;
        self.inner
            .connections
            .lock()
            .await
            .insert(id, dyn_transport.clone());
        self.spawn_schema_watcher(id, dyn_transport);
        Ok(id)
    }

    /// Opens a recording as a connection.
    ///
    /// The recording is a transport like any other, so requests, panes and the
    /// canonical fan-out all work on it unchanged.
    pub async fn open_replay(
        &self,
        recording: rw_record::Recording,
    ) -> TransportResult<ConnectionId> {
        let transport = Arc::new(ReplayTransport::new(recording));
        transport.connect().await?;
        let id = ConnectionId::new();
        let dyn_transport: Arc<dyn Transport> = transport.clone() as Arc<dyn Transport>;
        self.inner
            .connections
            .lock()
            .await
            .insert(id, dyn_transport.clone());
        self.spawn_schema_watcher(id, dyn_transport);
        Ok(id)
    }

    pub async fn open_rosbridge(&self, url: impl Into<String>) -> TransportResult<ConnectionId> {
        let transport = Arc::new(RosbridgeTransport::new(RosbridgeConfig::new(url)));
        transport.connect().await?;
        let id = ConnectionId::new();
        let dyn_transport: Arc<dyn Transport> = transport.clone() as Arc<dyn Transport>;
        self.inner
            .connections
            .lock()
            .await
            .insert(id, dyn_transport.clone());
        self.spawn_schema_watcher(id, dyn_transport);
        Ok(id)
    }

    fn spawn_schema_watcher(&self, connection: ConnectionId, transport: Arc<dyn Transport>) {
        let Some(registry) = self.inner.schema_registry.clone() else {
            return;
        };
        let inner = Arc::clone(&self.inner);
        let mut discovery_rx = transport.discovery();
        spawn_detached(async move {
            let snapshot = discovery_rx.borrow().clone();
            register_discovery(&registry, &snapshot, connection, &inner).await;
            while discovery_rx.changed().await.is_ok() {
                let snapshot = discovery_rx.borrow().clone();
                register_discovery(&registry, &snapshot, connection, &inner).await;
            }
        });
    }

    pub async fn transport(
        &self,
        connection_id: ConnectionId,
    ) -> TransportResult<Arc<dyn Transport>> {
        self.inner
            .connections
            .lock()
            .await
            .get(&connection_id)
            .cloned()
            .ok_or_else(|| TransportError::Other(format!("unknown connection {connection_id}")))
    }

    /// Changes how a recording is being played.
    ///
    /// Silently does nothing for a live system, which is what the trait's
    /// default does — a connection that is not a recording has no playback to
    /// change, and that is not an error worth reporting.
    pub async fn replay_control(&self, connection_id: ConnectionId, command: ReplayCommand) {
        let transport = self
            .inner
            .connections
            .lock()
            .await
            .get(&connection_id)
            .map(Arc::clone);
        if let Some(transport) = transport {
            transport.replay_control(command).await;
        }
    }

    pub async fn close(&self, connection_id: ConnectionId) -> TransportResult<()> {
        let removed = self.inner.connections.lock().await.remove(&connection_id);
        // What this connection said its targets were is only true while it is
        // open. Reconnecting re-describes them, and a robot reflashed in between
        // is entitled to a different answer.
        self.forget_schemas(connection_id);
        if let Some(transport) = removed {
            transport.disconnect().await?;
        }
        Ok(())
    }

    pub async fn subscribe_topic<F>(
        &self,
        connection_id: ConnectionId,
        topic: &str,
        pack_and_send: F,
    ) -> TransportResult<SubscribeResult>
    where
        F: FnMut(&str, &rw_transport::Frame, bool) + MaybeSend + 'static,
    {
        self.subscribe_topic_with_options(
            connection_id,
            topic,
            SubscribeOptions::default(),
            pack_and_send,
        )
        .await
    }

    pub async fn subscribe_topic_with_options<F>(
        &self,
        connection_id: ConnectionId,
        topic: &str,
        options: SubscribeOptions,
        mut pack_and_send: F,
    ) -> TransportResult<SubscribeResult>
    where
        F: FnMut(&str, &rw_transport::Frame, bool) + MaybeSend + 'static,
    {
        let transport = self.transport(connection_id).await?;
        let handle = self
            .inner
            .subscription_manager
            .subscribe_with_options(connection_id, topic, options, transport.as_ref())
            .await?;
        let schema_id = handle.schema.id.to_string();
        let schema_name = handle.schema.name.clone();
        let viz_role = handle.schema.viz_role.wire_id();
        let subscription_id = Uuid::new_v4().to_string();

        if let Some(latest) = &handle.latest {
            pack_and_send(&subscription_id, latest.as_ref(), true);
        }

        self.inner.meters.lock().expect("meter mutex").insert(
            subscription_id.clone(),
            stats::Meter::with_window(self.rate_window_ns()),
        );

        let mut receiver = handle.receiver.resubscribe();
        let forwarder_id = subscription_id.clone();
        let meters = Arc::clone(&self.inner);
        let forwarder = spawn_task(async move {
            use tokio::sync::broadcast::error::RecvError;
            loop {
                match receiver.recv().await {
                    Ok(frame) => {
                        // Measured here rather than in the consumer: every
                        // subscriber of a topic sees the same frames through
                        // the same fan-out, and a rate that depended on which
                        // pane was looking would be a rate about the UI.
                        if let Ok(mut meters) = meters.meters.lock() {
                            if let Some(meter) = meters.get_mut(&forwarder_id) {
                                meter.observe(rw_wire::now_ns(), frame.as_ref());
                            }
                        }
                        pack_and_send(&forwarder_id, frame.as_ref(), false)
                    }
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!(
                            subscription_id = %forwarder_id,
                            lagged = n,
                            "subscription consumer lagged; frames dropped, continuing",
                        );
                        continue;
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        });

        self.inner.subscriptions.lock().await.insert(
            subscription_id.clone(),
            ActiveSubscription {
                schema_id: schema_id.clone(),
                schema_name: schema_name.clone(),
                viz_role: viz_role.clone(),
                forwarder,
                _handle: handle,
            },
        );
        Ok(SubscribeResult {
            subscription_id,
            schema_id,
            schema_name,
            viz_role,
        })
    }

    /// Publishes one message to a topic.
    pub async fn publish(
        &self,
        connection_id: ConnectionId,
        topic: &str,
        value: CanonicalValue,
    ) -> TransportResult<()> {
        let transport = self.transport(connection_id).await?;
        transport.publish(topic, value).await
    }

    pub async fn call_service(
        &self,
        connection_id: ConnectionId,
        service: &str,
        request: CanonicalValue,
    ) -> TransportResult<CanonicalValue> {
        let transport = self.transport(connection_id).await?;
        transport.call_service(service, request).await
    }

    pub async fn send_action_goal(
        &self,
        connection_id: ConnectionId,
        action: &str,
        goal: CanonicalValue,
    ) -> TransportResult<ActionGoalStream> {
        let transport = self.transport(connection_id).await?;
        let stream = transport.send_action_goal(action, goal).await?;
        self.inner.action_goals.lock().await.insert(
            stream.cancel_token.goal_id.clone(),
            (connection_id, stream.cancel_token.clone()),
        );
        Ok(stream)
    }

    pub async fn cancel_action_goal(&self, goal_id: &str) -> TransportResult<()> {
        let entry = self.inner.action_goals.lock().await.remove(goal_id);
        let Some((connection_id, token)) = entry else {
            return Ok(());
        };
        let transport = self.transport(connection_id).await?;
        transport.cancel_action_goal(&token).await
    }

    pub async fn forget_action_goal(&self, goal_id: &str) {
        self.inner.action_goals.lock().await.remove(goal_id);
    }

    /// What a subscription is doing: rate, bandwidth and latency.
    ///
    /// Synchronous, because the caller is a view in the middle of drawing
    /// itself. `None` for a subscription that has been closed or never opened.
    pub fn stats(&self, subscription_id: &str) -> Option<stats::Stats> {
        let meters = self.inner.meters.lock().ok()?;
        Some(meters.get(subscription_id)?.stats(rw_wire::now_ns()))
    }

    pub async fn unsubscribe(&self, subscription_id: &str) -> TransportResult<()> {
        self.inner
            .meters
            .lock()
            .expect("meter mutex")
            .remove(subscription_id);
        let removed = self
            .inner
            .subscriptions
            .lock()
            .await
            .remove(subscription_id);
        if removed.is_none() {
            return Err(TransportError::Other(format!(
                "unknown subscription {subscription_id}"
            )));
        }
        Ok(())
    }
}

async fn register_discovery(
    registry: &Arc<SchemaRegistry>,
    discovery: &Discovery,
    connection: ConnectionId,
    inner: &Arc<Inner>,
) {
    // Which target each definition belongs to, so the hash it registers under
    // can be recorded against this connection rather than only against a name
    // the whole workspace shares.
    let mut owner: HashMap<&str, Vec<&str>> = HashMap::new();
    for topic in &discovery.topics {
        owner
            .entry(&topic.schema_name)
            .or_default()
            .push(&topic.name);
    }
    for service in &discovery.services {
        owner
            .entry(&service.schema_name)
            .or_default()
            .push(&service.name);
    }
    for action in &discovery.actions {
        owner
            .entry(&action.schema_name)
            .or_default()
            .push(&action.name);
    }

    let mut pending: Vec<(&str, &str, SchemaKind)> = Vec::new();
    for (name, body) in &discovery.dependency_schemas {
        pending.push((name.as_str(), body.as_str(), SchemaKind::Message));
    }
    for topic in &discovery.topics {
        if let Some(def) = topic.schema_definition.as_ref() {
            pending.push((&topic.schema_name, def, SchemaKind::Message));
        }
    }
    for service in &discovery.services {
        if let Some(def) = service.schema_definition.as_ref() {
            pending.push((&service.schema_name, def, SchemaKind::Service));
        }
    }
    for action in &discovery.actions {
        if let Some(def) = action.schema_definition.as_ref() {
            pending.push((&action.schema_name, def, SchemaKind::Action));
        }
    }

    while !pending.is_empty() {
        let mut still = Vec::new();
        let mut progressed = false;
        for (name, body, kind) in pending {
            match register_one(registry, name, body, kind).await {
                Some(hash) => {
                    record_schema(inner, connection, &owner, name, hash);
                    progressed = true;
                }
                None => still.push((name, body, kind)),
            }
        }
        pending = still;
        if !progressed {
            break;
        }
    }
    for (name, body, kind) in &pending {
        match registry.register(name, *kind, body).await {
            Ok(reference) => record_schema(inner, connection, &owner, name, reference.hash),
            Err(err) => tracing::warn!(?err, name, "schema failed to register during discovery"),
        }
    }
}

/// Notes that every target of `schema_name` on this connection is described by
/// the registry entry `hash`.
fn record_schema(
    inner: &Arc<Inner>,
    connection: ConnectionId,
    owner: &HashMap<&str, Vec<&str>>,
    schema_name: &str,
    hash: String,
) {
    let Some(targets) = owner.get(schema_name) else {
        // A dependency rather than a target of its own. It is in the registry
        // and reachable through the definitions that name it.
        return;
    };
    let Ok(mut schemas) = inner.schemas.lock() else {
        return;
    };
    for target in targets {
        schemas.insert((connection, target.to_string()), hash.clone());
    }
}

/// Registers one definition, giving back the hash it landed on.
///
/// The hash rather than a bare success: it is the only thing that tells this
/// connection's `sensor_msgs/Image` from another connection's.
async fn register_one(
    registry: &Arc<SchemaRegistry>,
    name: &str,
    definition: &str,
    kind: SchemaKind,
) -> Option<String> {
    registry
        .register(name, kind, definition)
        .await
        .ok()
        .map(|reference| reference.hash)
}

#[derive(Debug, Clone)]
pub struct SubscribeResult {
    pub subscription_id: String,
    pub schema_id: String,
    pub schema_name: String,
    pub viz_role: String,
}

#[cfg(not(target_family = "wasm"))]
pub trait MaybeSend: Send {}
#[cfg(not(target_family = "wasm"))]
impl<T: Send> MaybeSend for T {}

#[cfg(target_family = "wasm")]
pub trait MaybeSend {}
#[cfg(target_family = "wasm")]
impl<T> MaybeSend for T {}
