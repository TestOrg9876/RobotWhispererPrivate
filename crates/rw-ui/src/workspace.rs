//! The loaded workspace — connections, collections and requests — backed by
//! `rw_core::storage`.
//!
//! This replaces `connectionStore`, `requestsStore` and `collectionStore`. The
//! two RPC shims those went through are gone: this calls storage directly.

use std::sync::Arc;

use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Task};
use rw_core::domain::{Collection, Connection, Request, Value};
use rw_core::storage::Storage;

/// Storage handle shared by the whole app.
///
/// `Arc` on both targets. On wasm the `Storage` trait is `?Send`, so this is an
/// `Arc` over a non-`Send` value — sound because the web build runs
/// single-threaded via `WebPlatform::new(false)`.
pub type SharedStorage = Arc<dyn Storage>;

/// Live transport state for a connection, mirroring `TransportStatus`.
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
    /// Lowercase label, as the old status line rendered it.
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
}

/// A stored connection plus the session opened for it, if any.
#[derive(Debug, Clone)]
pub struct Session {
    pub connection: Connection,
    /// Pipeline session id; empty while not connected.
    pub id: String,
    pub status: Status,
}

impl Session {
    fn idle(connection: Connection) -> Self {
        Self {
            connection,
            id: String::new(),
            status: Status::Disconnected,
        }
    }
}

/// Emitted when the workspace finishes loading or its contents change, so views
/// that cache derived state can refresh.
#[derive(Debug, Clone, Copy)]
pub struct Changed;

/// Connections, collections and requests, loaded from storage.
pub struct Workspace {
    storage: SharedStorage,
    sessions: Vec<Session>,
    collections: Vec<Collection>,
    requests: Vec<Request>,
    loaded: bool,
    error: Option<String>,
}

impl EventEmitter<Changed> for Workspace {}

impl Workspace {
    pub fn new(storage: SharedStorage) -> Self {
        Self {
            storage,
            sessions: Vec::new(),
            collections: Vec::new(),
            requests: Vec::new(),
            loaded: false,
            error: None,
        }
    }

    pub fn storage(&self) -> SharedStorage {
        Arc::clone(&self.storage)
    }

    pub fn sessions(&self) -> &[Session] {
        &self.sessions
    }

    pub fn collections(&self) -> &[Collection] {
        &self.collections
    }

    pub fn requests(&self) -> &[Request] {
        &self.requests
    }

    /// False until the first load finishes, so views can distinguish "empty"
    /// from "not read yet".
    pub fn loaded(&self) -> bool {
        self.loaded
    }

    /// Last storage failure, surfaced in the status bar.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn session(&self, connection: i64) -> Option<&Session> {
        self.sessions
            .iter()
            .find(|session| session.connection.id == connection)
    }

    /// Session id, but only while actually connected — the same guard
    /// `connectionStore.sessionId()` applied.
    pub fn session_id(&self, connection: i64) -> Option<&str> {
        self.session(connection)
            .filter(|session| session.status.is_connected())
            .map(|session| session.id.as_str())
    }

    pub fn status(&self, connection: i64) -> Status {
        self.session(connection)
            .map(|session| session.status.clone())
            .unwrap_or_default()
    }

    pub fn connected(&self) -> impl Iterator<Item = &Session> {
        self.sessions
            .iter()
            .filter(|session| session.status.is_connected())
    }

    pub fn request(&self, id: i64) -> Option<&Request> {
        self.requests.iter().find(|request| request.id == id)
    }

    /// Reads everything from storage.
    ///
    /// Failures are recorded rather than propagated: an unreadable workspace
    /// should still give a usable window, which is what the old store did by
    /// logging and continuing with an empty list.
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
                        Ok(list) => {
                            workspace.sessions = list.into_iter().map(Session::idle).collect();
                        }
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
                    cx.emit(Changed);
                    cx.notify();
                })
                .ok();
        })
    }

    /// Creates a request and returns it, so the caller can open a tab for it.
    pub fn create_request(&mut self, cx: &mut Context<Self>) -> Task<Option<Request>> {
        let storage = self.storage();
        cx.spawn(async move |workspace, cx| {
            let draft = rw_core::storage::NewRequest {
                collection_id: None,
                connection_id: None,
                name: "New request".into(),
                kind: rw_core::domain::RequestKind::Topic,
                target: String::new(),
                schema: None,
                input: Value::Struct(Default::default()),
                visualization: None,
            };

            match storage.create_request(draft).await {
                Ok(request) => workspace
                    .update(cx, |workspace, cx| {
                        workspace.requests.push(request.clone());
                        cx.emit(Changed);
                        cx.notify();
                        Some(request)
                    })
                    .ok()
                    .flatten(),
                Err(error) => {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.error = Some(format!("create request: {error}"));
                            cx.notify();
                        })
                        .ok();
                    None
                }
            }
        })
    }

    /// Deletes a request, removing it from the in-memory list on success.
    pub fn delete_request(&mut self, id: i64, cx: &mut Context<Self>) -> Task<()> {
        let storage = self.storage();
        cx.spawn(async move |workspace, cx| {
            let outcome = storage.delete_request(id).await;
            workspace
                .update(cx, |workspace, cx| {
                    match outcome {
                        Ok(()) => workspace.requests.retain(|request| request.id != id),
                        Err(error) => workspace.error = Some(format!("delete request: {error}")),
                    }
                    cx.emit(Changed);
                    cx.notify();
                })
                .ok();
        })
    }
}

/// App-wide handles, reachable from any view via [`RobotWhisperer::global`].
pub struct RobotWhisperer {
    pub workspace: Entity<Workspace>,
}

impl gpui::Global for RobotWhisperer {}

impl RobotWhisperer {
    /// Installs the global. Call once during init.
    pub fn init(storage: SharedStorage, cx: &mut App) {
        let workspace = cx.new(|_| Workspace::new(storage));
        cx.set_global(Self { workspace });
    }

    pub fn global(cx: &App) -> &Self {
        cx.global::<Self>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_labels_match_the_old_type_union() {
        assert_eq!(Status::Disconnected.label(), "disconnected");
        assert_eq!(Status::Connecting.label(), "connecting");
        assert_eq!(Status::Connected.label(), "connected");
        assert_eq!(Status::Reconnecting.label(), "reconnecting");
        assert_eq!(Status::Failed("boom".into()).label(), "failed");
    }

    #[test]
    fn only_connected_counts_as_connected() {
        assert!(Status::Connected.is_connected());
        for status in [
            Status::Disconnected,
            Status::Connecting,
            Status::Reconnecting,
            Status::Failed("x".into()),
        ] {
            assert!(!status.is_connected(), "{status:?} must not count");
        }
    }

    #[test]
    fn a_fresh_session_starts_disconnected_with_no_id() {
        let now = chrono::Utc::now();
        let connection = Connection {
            id: 7,
            name: "Dummy".into(),
            config: rw_core::domain::TransportConfig::Dummy {},
            auto_connect: false,
            color: None,
            created_at: now,
            updated_at: now,
        };
        let session = Session::idle(connection);
        assert_eq!(session.status, Status::Disconnected);
        assert!(session.id.is_empty());
    }
}
