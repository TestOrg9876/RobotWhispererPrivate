//! JSON-RPC control plane.
//!
//! Replaces Tauri's `invoke` dispatch table. The method names and argument
//! shapes are deliberately identical to the old `#[tauri::command]` surface so
//! the TypeScript side changes transport without changing call sites.
//!
//! Note on argument casing: Tauri silently converted camelCase keys from JS to
//! snake_case Rust parameters. Nothing does that for us any more, so every
//! params struct below carries an explicit `rename_all = "camelCase"`. Getting
//! this wrong fails as a missing-field error at runtime, not at compile time,
//! which is exactly why it is spelled out rather than inferred.

use std::sync::Arc;

use rw_core::domain::{Collection, Connection, Request, SchemaRef, WorkspaceFile};
use rw_core::ids::{CollectionId, ConnectionId as WsConnectionId, RequestId};
use rw_core::schema::{SchemaDefinition, SchemaKind, SchemaSummary};
use rw_core::storage::{
    export_workspace, import_workspace, ImportMode, NewCollection, NewConnection, NewRequest,
};
use rw_core::CoreError;
use rw_transport::{ConnectionId, SubscribeOptions};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};

use crate::state::AppState;
use crate::wire::pack_value_frame;

// ---------------------------------------------------------------------------
// Envelope types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RpcRequest {
    pub id: Option<u64>,
    pub method: String,
    #[serde(default)]
    pub params: Json,
}

#[derive(Debug, Clone, Serialize)]
pub struct RpcError {
    pub kind: String,
    pub message: String,
}

impl RpcError {
    fn new(kind: &str, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid", message)
    }

    fn transport(message: impl Into<String>) -> Self {
        Self::new("transport", message)
    }
}

impl From<CoreError> for RpcError {
    fn from(error: CoreError) -> Self {
        let kind = match &error {
            CoreError::Storage(_) => "storage",
            CoreError::Schema(_) => "schema",
            CoreError::Transport(_) => "transport",
            CoreError::NotFound(_) => "not_found",
            CoreError::InvalidArgument(_) => "invalid_argument",
            CoreError::Conflict(_) => "conflict",
            CoreError::Io(_) => "io",
            CoreError::Serde(_) => "serde",
        };
        Self::new(kind, error.to_string())
    }
}

pub type RpcResult = Result<Json, RpcError>;

/// Outbound half of one connected RPC client. Pushes (action envelopes, status
/// and discovery updates) are routed back to the client that asked for them.
pub type ClientTx = tokio::sync::mpsc::UnboundedSender<String>;

fn push(tx: &ClientTx, value: Json) {
    if let Ok(text) = serde_json::to_string(&value) {
        let _ = tx.send(text);
    }
}

// ---------------------------------------------------------------------------
// Params
// ---------------------------------------------------------------------------

fn parse<T: serde::de::DeserializeOwned>(params: Json) -> Result<T, RpcError> {
    serde_json::from_value(params).map_err(|err| RpcError::invalid(format!("params: {err}")))
}

fn ok<T: Serialize>(value: T) -> RpcResult {
    serde_json::to_value(value).map_err(|err| RpcError::new("serde", err.to_string()))
}

fn connection_id(raw: &str) -> Result<ConnectionId, RpcError> {
    raw.parse::<uuid::Uuid>()
        .map(ConnectionId)
        .map_err(|err| RpcError::invalid(format!("connection id: {err}")))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UrlParams {
    url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionParams {
    connection_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubscribeParams {
    connection_id: String,
    topic: String,
    #[serde(default)]
    options: Option<SubscribeOptionsParams>,
}

/// Inner keys stay snake_case: this object is built by `optionsToBackend()` in
/// `pipelineRpc.shared` and has always been sent in snake_case.
#[derive(Debug, Default, Deserialize)]
struct SubscribeOptionsParams {
    #[serde(default)]
    target_hz: Option<f32>,
    #[serde(default)]
    queue_length: Option<u32>,
    #[serde(default)]
    fields: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct SubscribeResponse {
    subscription_id: String,
    schema_id: String,
    schema_name: String,
    viz_role: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnsubscribeParams {
    subscription_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CallServiceParams {
    connection_id: String,
    service: String,
    request_json: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActionGoalParams {
    connection_id: String,
    action: String,
    goal_json: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoalIdParams {
    goal_id: String,
}

#[derive(Debug, Deserialize)]
struct IdParams<T> {
    id: T,
}

#[derive(Debug, Deserialize)]
struct NameParams {
    name: String,
}

#[derive(Debug, Deserialize)]
struct HashParams {
    hash: String,
}

#[derive(Debug, Deserialize)]
struct RegisterSchemaParams {
    name: String,
    kind: SchemaKind,
    definition: String,
}

#[derive(Debug, Deserialize)]
struct EnabledParams {
    enabled: bool,
}

#[derive(Debug, Deserialize)]
struct ImportParams {
    file: WorkspaceFile,
    mode: ImportMode,
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

pub async fn dispatch(
    state: &Arc<AppState>,
    tx: &ClientTx,
    method: &str,
    params: Json,
) -> RpcResult {
    match method {
        // -- pipeline ------------------------------------------------------
        "pipeline_open_foxglove" => {
            let p: UrlParams = parse(params)?;
            let id = state
                .pipeline
                .open_foxglove(p.url)
                .await
                .map_err(|e| RpcError::transport(e.to_string()))?;
            ok(id.to_string())
        }
        "pipeline_open_rosbridge" => {
            let p: UrlParams = parse(params)?;
            let id = state
                .pipeline
                .open_rosbridge(p.url)
                .await
                .map_err(|e| RpcError::transport(e.to_string()))?;
            ok(id.to_string())
        }
        "pipeline_open_dummy" => {
            let id = state
                .pipeline
                .open_dummy()
                .await
                .map_err(|e| RpcError::transport(e.to_string()))?;
            ok(id.to_string())
        }
        "pipeline_close" => {
            let p: ConnectionParams = parse(params)?;
            state
                .pipeline
                .close(connection_id(&p.connection_id)?)
                .await
                .map_err(|e| RpcError::transport(e.to_string()))?;
            Ok(Json::Null)
        }
        "pipeline_discovery" => {
            let p: ConnectionParams = parse(params)?;
            let transport = state
                .pipeline
                .transport(connection_id(&p.connection_id)?)
                .await
                .map_err(|e| RpcError::transport(e.to_string()))?;
            let discovery = transport.discovery().borrow().clone();
            let snapshot = rw_transport::discovery_to_json(&discovery);
            // Returned as a JSON *string*, matching the old command, because
            // the TypeScript caller still does `JSON.parse` on it.
            ok(serde_json::to_string(&snapshot)
                .map_err(|e| RpcError::new("serde", e.to_string()))?)
        }
        "pipeline_status" => {
            let p: ConnectionParams = parse(params)?;
            let transport = state
                .pipeline
                .transport(connection_id(&p.connection_id)?)
                .await
                .map_err(|e| RpcError::transport(e.to_string()))?;
            let status = transport.status().borrow().clone();
            ok(
                serde_json::to_string(&status)
                    .map_err(|e| RpcError::new("serde", e.to_string()))?,
            )
        }
        // Push-based status and discovery. Native used to poll `getDiscovery`
        // on a 750ms timer because Tauri's shell had no push channel wired up;
        // the WASM host always had this. Now both do.
        "pipeline_watch" => {
            let p: ConnectionParams = parse(params)?;
            let cid = connection_id(&p.connection_id)?;
            let transport = state
                .pipeline
                .transport(cid)
                .await
                .map_err(|e| RpcError::transport(e.to_string()))?;

            let mut status_rx = transport.status();
            let status_tx = tx.clone();
            let status_id = p.connection_id.clone();
            tokio::spawn(async move {
                while status_rx.changed().await.is_ok() {
                    let status = status_rx.borrow().clone();
                    push(
                        &status_tx,
                        json!({ "push": "status", "connectionId": status_id, "status": status }),
                    );
                }
            });

            let mut discovery_rx = transport.discovery();
            let discovery_tx = tx.clone();
            let discovery_id = p.connection_id.clone();
            tokio::spawn(async move {
                while discovery_rx.changed().await.is_ok() {
                    let snapshot = rw_transport::discovery_to_json(&discovery_rx.borrow().clone());
                    push(
                        &discovery_tx,
                        json!({
                            "push": "discovery",
                            "connectionId": discovery_id,
                            "snapshot": snapshot,
                        }),
                    );
                }
            });

            Ok(Json::Null)
        }
        "pipeline_subscribe_topic" => {
            let p: SubscribeParams = parse(params)?;
            let cid = connection_id(&p.connection_id)?;
            let raw = p.options.unwrap_or_default();
            let fields = raw.fields.clone().filter(|f| !f.is_empty());
            let opts = SubscribeOptions {
                target_hz: raw.target_hz,
                queue_length: raw.queue_length,
            };
            let hub = state.hub.clone();
            let mut payload_hint: usize = 4096;
            let response = state
                .pipeline
                .subscribe_topic_with_options(cid, &p.topic, opts, move |handle, frame, replay| {
                    if let Some(packed) =
                        pack_value_frame(handle, frame, replay, &fields, &mut payload_hint)
                    {
                        hub.send(packed);
                    }
                })
                .await
                .map_err(|e| RpcError::transport(e.to_string()))?;
            ok(SubscribeResponse {
                subscription_id: response.subscription_id,
                schema_id: response.schema_id,
                schema_name: response.schema_name,
                viz_role: response.viz_role,
            })
        }
        "pipeline_unsubscribe" => {
            let p: UnsubscribeParams = parse(params)?;
            state
                .pipeline
                .unsubscribe(&p.subscription_id)
                .await
                .map_err(|e| RpcError::transport(e.to_string()))?;
            Ok(Json::Null)
        }
        "pipeline_call_service" => {
            let p: CallServiceParams = parse(params)?;
            let cid = connection_id(&p.connection_id)?;
            let request: rw_canonical::CanonicalValue = serde_json::from_str(&p.request_json)
                .map_err(|e| RpcError::invalid(format!("request_json: {e}")))?;
            let response = state
                .pipeline
                .call_service(cid, &p.service, request)
                .await
                .map_err(|e| RpcError::transport(e.to_string()))?;
            ok(serde_json::to_string(&response)
                .map_err(|e| RpcError::invalid(format!("response_json: {e}")))?)
        }
        "pipeline_send_action_goal" => {
            let p: ActionGoalParams = parse(params)?;
            let cid = connection_id(&p.connection_id)?;
            let goal: rw_canonical::CanonicalValue = serde_json::from_str(&p.goal_json)
                .map_err(|e| RpcError::invalid(format!("goal_json: {e}")))?;
            let stream = state
                .pipeline
                .send_action_goal(cid, &p.action, goal)
                .await
                .map_err(|e| RpcError::transport(e.to_string()))?;
            let goal_id = stream.cancel_token.goal_id.clone();

            // Feedback and result used to travel over a Tauri `Channel`. They
            // are now pushes on this socket, carrying the same four envelope
            // kinds (feedback / result / error / closed) in the same order.
            let fb_tx = tx.clone();
            let fb_goal = goal_id.clone();
            let mut feedback_rx = stream.feedback;
            tokio::spawn(async move {
                while let Some(value) = feedback_rx.recv().await {
                    push(
                        &fb_tx,
                        json!({
                            "push": "action",
                            "goalId": fb_goal,
                            "envelope": { "kind": "feedback", "value": value },
                        }),
                    );
                }
            });

            let result_tx = tx.clone();
            let result_goal = goal_id.clone();
            let result_rx = stream.result;
            let pipeline = state.pipeline.clone();
            tokio::spawn(async move {
                let envelope = match result_rx.await {
                    Ok(Ok(value)) => json!({ "kind": "result", "value": value }),
                    Ok(Err(err)) => json!({ "kind": "error", "message": err.to_string() }),
                    Err(_) => json!({ "kind": "error", "message": "action result channel closed" }),
                };
                push(
                    &result_tx,
                    json!({ "push": "action", "goalId": result_goal, "envelope": envelope }),
                );
                push(
                    &result_tx,
                    json!({
                        "push": "action",
                        "goalId": result_goal,
                        "envelope": { "kind": "closed" },
                    }),
                );
                pipeline.forget_action_goal(&result_goal).await;
            });

            ok(goal_id)
        }
        "pipeline_cancel_action_goal" => {
            let p: GoalIdParams = parse(params)?;
            state
                .pipeline
                .cancel_action_goal(&p.goal_id)
                .await
                .map_err(|e| RpcError::transport(e.to_string()))?;
            Ok(Json::Null)
        }

        // -- workspace -----------------------------------------------------
        "list_requests" => ok(state.storage.list_requests().await?),
        "get_request" => {
            let p: IdParams<RequestId> = parse(params)?;
            ok(state.storage.get_request(p.id).await?)
        }
        "create_request" => {
            let draft: NewRequest = field(params, "draft")?;
            ok(state.storage.create_request(draft).await?)
        }
        "update_request" => {
            let request: Request = field(params, "request")?;
            state.storage.update_request(&request).await?;
            let refreshed = state.storage.get_request(request.id).await?;
            ok(refreshed.ok_or_else(|| {
                RpcError::new(
                    "not_found",
                    format!("request {} disappeared mid-update", request.id),
                )
            })?)
        }
        "delete_request" => {
            let p: IdParams<RequestId> = parse(params)?;
            state.storage.delete_request(p.id).await?;
            Ok(Json::Null)
        }
        "list_collections" => ok(state.storage.list_collections().await?),
        "create_collection" => {
            let draft: NewCollection = field(params, "draft")?;
            ok(state.storage.create_collection(draft).await?)
        }
        "update_collection" => {
            let collection: Collection = field(params, "collection")?;
            state.storage.update_collection(&collection).await?;
            Ok(Json::Null)
        }
        "delete_collection" => {
            let p: IdParams<CollectionId> = parse(params)?;
            state.storage.delete_collection(p.id).await?;
            Ok(Json::Null)
        }
        "list_connections" => ok(state.storage.list_connections().await?),
        "get_connection" => {
            let p: IdParams<WsConnectionId> = parse(params)?;
            ok(state.storage.get_connection(p.id).await?)
        }
        "create_connection" => {
            let draft: NewConnection = field(params, "draft")?;
            ok(state.storage.create_connection(draft).await?)
        }
        "update_connection" => {
            let connection: Connection = field(params, "connection")?;
            state.storage.update_connection(&connection).await?;
            Ok(Json::Null)
        }
        "delete_connection" => {
            let p: IdParams<WsConnectionId> = parse(params)?;
            state.storage.delete_connection(p.id).await?;
            Ok(Json::Null)
        }
        "clear_workspace_storage" => {
            state.storage.clear_all().await?;
            Ok(Json::Null)
        }
        "export_workspace_command" => {
            let file = export_workspace(state.storage.as_ref(), state.clock.clone()).await?;
            ok(serde_json::to_string_pretty(&file)
                .map_err(|e| RpcError::new("serde", e.to_string()))?)
        }
        "import_workspace_command" => {
            let p: ImportParams = parse(params)?;
            ok(import_workspace(state.storage.as_ref(), p.file, p.mode).await?)
        }

        // -- schema --------------------------------------------------------
        "list_schemas_summary" => {
            let summaries: Vec<SchemaSummary> = state.registry.list_summaries();
            ok(summaries)
        }
        "get_schema_by_hash" => {
            let p: HashParams = parse(params)?;
            let found: Option<SchemaDefinition> = state.registry.get_by_hash(&p.hash);
            ok(found)
        }
        "list_schemas_by_name" => {
            let p: NameParams = parse(params)?;
            let found: Vec<SchemaDefinition> = state.registry.get_by_name(&p.name);
            ok(found)
        }
        "register_schema" => {
            let p: RegisterSchemaParams = parse(params)?;
            let reference: SchemaRef = state
                .registry
                .register(&p.name, p.kind, &p.definition)
                .await?;
            ok(reference)
        }

        // -- diagnostics ---------------------------------------------------
        "set_perf_trace_enabled" => {
            let p: EnabledParams = parse(params)?;
            rw_wire::set_perf_trace_enabled(p.enabled);
            Ok(Json::Null)
        }
        "perf_trace_enabled" => ok(rw_wire::perf_trace_enabled()),

        other => Err(RpcError::invalid(format!("unknown method {other}"))),
    }
}

/// Pull a single named field out of a params object and deserialize it. The
/// workspace commands take one payload each (`draft`, `request`, ...), which is
/// how they were already being sent.
fn field<T: serde::de::DeserializeOwned>(params: Json, name: &str) -> Result<T, RpcError> {
    let value = params
        .get(name)
        .cloned()
        .ok_or_else(|| RpcError::invalid(format!("missing field `{name}`")))?;
    serde_json::from_value(value).map_err(|err| RpcError::invalid(format!("{name}: {err}")))
}
