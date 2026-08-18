//! The stored workspace: connections, collections and requests.
//!
//! Purely the persisted side. Live transport state belongs to
//! [`crate::session::Sessions`], so nothing here knows about the pipeline.

use std::sync::Arc;

use gpui::{Context, Task};
use rw_core::domain::{Collection, Connection, Request, RequestKind, TransportConfig, Value};
use rw_core::storage::{NewConnection, NewRequest, Storage};

/// Storage handle shared by the app.
///
/// `Arc` on both targets. On wasm the `Storage` trait is `?Send`, so this is an
/// `Arc` over a non-`Send` value — sound because the web build runs
/// single-threaded via `WebPlatform::new(false)`.
pub type SharedStorage = Arc<dyn Storage>;

pub struct Workspace {
    storage: SharedStorage,
    connections: Vec<Connection>,
    collections: Vec<Collection>,
    requests: Vec<Request>,
    loaded: bool,
    error: Option<String>,
}

impl Workspace {
    pub fn new(storage: SharedStorage) -> Self {
        Self {
            storage,
            connections: Vec::new(),
            collections: Vec::new(),
            requests: Vec::new(),
            loaded: false,
            error: None,
        }
    }

    pub fn storage(&self) -> SharedStorage {
        Arc::clone(&self.storage)
    }

    pub fn connections(&self) -> &[Connection] {
        &self.connections
    }

    pub fn collections(&self) -> &[Collection] {
        &self.collections
    }

    pub fn requests(&self) -> &[Request] {
        &self.requests
    }

    /// False until the first load finishes, so views can tell "empty" from
    /// "not read yet".
    pub fn loaded(&self) -> bool {
        self.loaded
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn connection(&self, id: i64) -> Option<&Connection> {
        self.connections.iter().find(|entry| entry.id == id)
    }

    pub fn request(&self, id: i64) -> Option<&Request> {
        self.requests.iter().find(|entry| entry.id == id)
    }

    /// Reads everything from storage. Failures are recorded rather than
    /// propagated: an unreadable workspace should still give a usable window.
    pub fn load(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let storage = self.storage();

        cx.spawn(async move |workspace, cx| {
            let connections = storage.list_connections().await;
            let collections = storage.list_collections().await;
            let requests = storage.list_requests().await;

            workspace
                .update(cx, |workspace, cx| {
                    let mut failures = Vec::new();
                    match connections {
                        Ok(list) => workspace.connections = list,
                        Err(error) => failures.push(format!("connections: {error}")),
                    }
                    match collections {
                        Ok(list) => workspace.collections = list,
                        Err(error) => failures.push(format!("collections: {error}")),
                    }
                    match requests {
                        Ok(list) => workspace.requests = list,
                        Err(error) => failures.push(format!("requests: {error}")),
                    }

                    workspace.loaded = true;
                    workspace.error = (!failures.is_empty()).then(|| failures.join("; "));
                    cx.notify();
                })
                .ok();
        })
    }

    /// Creates an empty topic request and returns it so the caller can open it.
    pub fn create_request(&mut self, cx: &mut Context<Self>) -> Task<Option<Request>> {
        let storage = self.storage();
        let draft = NewRequest {
            name: "New request".into(),
            kind: RequestKind::Topic,
            target: String::new(),
            collection_id: None,
            connection_id: None,
            schema: None,
            input: Value::Struct(Default::default()),
            visualization: None,
        };

        cx.spawn(async move |workspace, cx| {
            let created = storage.create_request(draft).await;
            workspace
                .update(cx, |workspace, cx| {
                    cx.notify();
                    match created {
                        Ok(request) => {
                            workspace.requests.push(request.clone());
                            Some(request)
                        }
                        Err(error) => {
                            workspace.error = Some(format!("create request: {error}"));
                            None
                        }
                    }
                })
                .ok()
                .flatten()
        })
    }

    /// Adds a Dummy connection — synthetic topics, services and actions, so the
    /// app is usable with no robot present.
    pub fn create_dummy_connection(&mut self, cx: &mut Context<Self>) -> Task<Option<Connection>> {
        let storage = self.storage();
        let existing = self
            .connections
            .iter()
            .filter(|entry| entry.name.starts_with("Dummy"))
            .count();
        let name = if existing == 0 {
            "Dummy".to_string()
        } else {
            format!("Dummy {}", existing + 1)
        };

        let draft = NewConnection {
            name,
            config: TransportConfig::Dummy {},
            auto_connect: false,
            color: None,
        };

        cx.spawn(async move |workspace, cx| {
            let created = storage.create_connection(draft).await;
            workspace
                .update(cx, |workspace, cx| {
                    cx.notify();
                    match created {
                        Ok(connection) => {
                            workspace.connections.push(connection.clone());
                            Some(connection)
                        }
                        Err(error) => {
                            workspace.error = Some(format!("create connection: {error}"));
                            None
                        }
                    }
                })
                .ok()
                .flatten()
        })
    }

    pub fn delete_request(&mut self, id: i64, cx: &mut Context<Self>) -> Task<()> {
        let storage = self.storage();
        cx.spawn(async move |workspace, cx| {
            let outcome = storage.delete_request(id).await;
            workspace
                .update(cx, |workspace, cx| {
                    match outcome {
                        Ok(()) => workspace.requests.retain(|entry| entry.id != id),
                        Err(error) => workspace.error = Some(format!("delete request: {error}")),
                    }
                    cx.notify();
                })
                .ok();
        })
    }
}
