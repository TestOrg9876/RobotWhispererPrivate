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

    /// Records a failure and logs it.
    ///
    /// Storage failures used to be recorded and never seen — the sidebar simply
    /// stayed empty and the app looked like it had ignored the click. Every
    /// write path goes through here, so a failure always reaches both the log
    /// and the status bar.
    fn fail(&mut self, doing: &str, error: impl std::fmt::Display) {
        let message = format!("{doing}: {error}");
        tracing::error!("{message}");
        self.error = Some(message);
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
                    if !failures.is_empty() {
                        workspace.fail("loading the workspace", failures.join("; "));
                    }
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
                            workspace.fail("create request", error);
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
                            workspace.fail("create connection", error);
                            None
                        }
                    }
                })
                .ok()
                .flatten()
        })
    }

    /// Adds a connection to a ROS system.
    ///
    /// Several can be connected at once — that is the point of them — so this
    /// makes no attempt to be "the" connection.
    pub fn create_connection(
        &mut self,
        draft: NewConnection,
        cx: &mut Context<Self>,
    ) -> Task<Option<Connection>> {
        let storage = self.storage();
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
                            workspace.fail("create connection", error);
                            None
                        }
                    }
                })
                .ok()
                .flatten()
        })
    }

    pub fn update_connection(
        &mut self,
        connection: Connection,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let storage = self.storage();
        if let Some(existing) = self
            .connections
            .iter_mut()
            .find(|entry| entry.id == connection.id)
        {
            *existing = connection.clone();
        }
        cx.notify();

        cx.spawn(async move |workspace, cx| {
            let outcome = storage.update_connection(&connection).await;
            workspace
                .update(cx, |workspace, cx| {
                    if let Err(error) = outcome {
                        workspace.fail("save connection", error);
                        cx.notify();
                    }
                })
                .ok();
        })
    }

    pub fn delete_connection(&mut self, id: i64, cx: &mut Context<Self>) -> Task<()> {
        let storage = self.storage();
        cx.spawn(async move |workspace, cx| {
            let outcome = storage.delete_connection(id).await;
            workspace
                .update(cx, |workspace, cx| {
                    match outcome {
                        Ok(()) => {
                            workspace.connections.retain(|entry| entry.id != id);
                            // Requests pointing at it are detached by storage;
                            // mirror that here rather than re-reading everything.
                            for request in &mut workspace.requests {
                                if request.connection_id == Some(id) {
                                    request.connection_id = None;
                                }
                            }
                        }
                        Err(error) => workspace.fail("delete connection", error),
                    }
                    cx.notify();
                })
                .ok();
        })
    }

    // ── folders ────────────────────────────────────────────────────────────────

    pub fn create_collection(
        &mut self,
        name: String,
        parent: Option<i64>,
        cx: &mut Context<Self>,
    ) -> Task<Option<Collection>> {
        let storage = self.storage();
        cx.spawn(async move |workspace, cx| {
            let created = storage
                .create_collection(rw_core::storage::NewCollection {
                    parent_id: parent,
                    name,
                })
                .await;
            workspace
                .update(cx, |workspace, cx| {
                    cx.notify();
                    match created {
                        Ok(collection) => {
                            workspace.collections.push(collection.clone());
                            Some(collection)
                        }
                        Err(error) => {
                            workspace.fail("create collection", error);
                            None
                        }
                    }
                })
                .ok()
                .flatten()
        })
    }

    pub fn rename_collection(&mut self, id: i64, name: String, cx: &mut Context<Self>) -> Task<()> {
        let storage = self.storage();
        let Some(collection) = self
            .collections
            .iter_mut()
            .find(|collection| collection.id == id)
        else {
            return Task::ready(());
        };
        collection.name = name;
        let updated = collection.clone();
        cx.notify();

        cx.spawn(async move |workspace, cx| {
            let outcome = storage.update_collection(&updated).await;
            workspace
                .update(cx, |workspace, cx| {
                    if let Err(error) = outcome {
                        workspace.fail("rename collection", error);
                        cx.notify();
                    }
                })
                .ok();
        })
    }

    /// Deletes a collection, keeping what was inside it.
    ///
    /// The requests move up to where the collection was rather than going with
    /// it: a collection is a way of arranging work, and removing the arrangement
    /// should never remove the work.
    pub fn delete_collection(&mut self, id: i64, cx: &mut Context<Self>) -> Task<()> {
        let storage = self.storage();
        let parent = self
            .collections
            .iter()
            .find(|collection| collection.id == id)
            .and_then(|collection| collection.parent_id);

        let orphans: Vec<Request> = self
            .requests
            .iter_mut()
            .filter(|request| request.collection_id == Some(id))
            .map(|request| {
                request.collection_id = parent;
                request.clone()
            })
            .collect();
        let children: Vec<Collection> = self
            .collections
            .iter_mut()
            .filter(|collection| collection.parent_id == Some(id))
            .map(|collection| {
                collection.parent_id = parent;
                collection.clone()
            })
            .collect();
        self.collections.retain(|collection| collection.id != id);
        cx.notify();

        cx.spawn(async move |workspace, cx| {
            let mut failure = None;
            // Re-parented first: deleting the collection before its contents
            // are moved out is what a cascading delete in the database would
            // turn into lost requests.
            for request in orphans {
                if let Err(error) = storage.update_request(&request).await {
                    failure = Some(error);
                    break;
                }
            }
            for child in children {
                if failure.is_some() {
                    break;
                }
                if let Err(error) = storage.update_collection(&child).await {
                    failure = Some(error);
                }
            }
            if failure.is_none()
                && let Err(error) = storage.delete_collection(id).await
            {
                failure = Some(error);
            }

            workspace
                .update(cx, |workspace, cx| {
                    if let Some(error) = failure {
                        workspace.fail("delete collection", error);
                    }
                    cx.notify();
                })
                .ok();
        })
    }

    /// Moves a collection under another, or out to the root with `None`.
    ///
    /// Whether the move is *allowed* is the tree's business — this only writes
    /// it, so the check has one home rather than two that can disagree.
    pub fn move_collection(
        &mut self,
        collection: i64,
        parent: Option<i64>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let storage = self.storage();
        let Some(found) = self
            .collections
            .iter_mut()
            .find(|entry| entry.id == collection)
        else {
            return Task::ready(());
        };
        found.parent_id = parent;
        let moved = found.clone();
        cx.notify();

        cx.spawn(async move |workspace, cx| {
            let outcome = storage.update_collection(&moved).await;
            workspace
                .update(cx, |workspace, cx| {
                    if let Err(error) = outcome {
                        workspace.fail("move collection", error);
                        cx.notify();
                    }
                })
                .ok();
        })
    }

    /// Moves a request into a collection, or out to the root with `None`.
    pub fn move_request(
        &mut self,
        request: i64,
        folder: Option<i64>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let storage = self.storage();
        let Some(found) = self.requests.iter_mut().find(|entry| entry.id == request) else {
            return Task::ready(());
        };
        found.collection_id = folder;
        let moved = found.clone();
        cx.notify();

        cx.spawn(async move |workspace, cx| {
            let outcome = storage.update_request(&moved).await;
            workspace
                .update(cx, |workspace, cx| {
                    if let Err(error) = outcome {
                        workspace.fail("move request", error);
                        cx.notify();
                    }
                })
                .ok();
        })
    }

    // ── import and export ──────────────────────────────────────────────────────

    /// The workspace as a shareable document.
    pub fn document(&self) -> rw_core::portable::Document {
        rw_core::portable::export(&self.connections, &self.collections, &self.requests)
    }

    /// Applies an import plan: connections first, so the requests that name
    /// them can be bound as they are created.
    ///
    /// One task rather than several, because a half-applied import — the
    /// connections in, the requests not — is a state nobody asked for.
    pub fn apply_import(
        &mut self,
        plan: rw_core::portable::Plan,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let storage = self.storage();
        let here = self.collections.clone();

        cx.spawn(async move |workspace, cx| {
            // Collections first, parents before children, so a request can be
            // put in the one it names. The plan already ordered them by depth.
            let mut made: Vec<(String, i64)> = Vec::new();
            for path in plan.new_collections {
                let (parent_path, name) = match path.rsplit_once('/') {
                    Some((parent, name)) => (Some(parent.to_string()), name.to_string()),
                    None => (None, path.clone()),
                };
                let parent_id = parent_path.and_then(|parent| {
                    made.iter()
                        .find(|(created, _)| *created == parent)
                        .map(|(_, id)| *id)
                });

                match storage
                    .create_collection(rw_core::storage::NewCollection { parent_id, name })
                    .await
                {
                    Ok(collection) => made.push((path, collection.id)),
                    Err(error) => {
                        workspace
                            .update(cx, |workspace, cx| {
                                workspace.fail("import collection", error);
                                cx.notify();
                            })
                            .ok();
                        return;
                    }
                }
            }

            let mut created: Vec<Connection> = Vec::new();

            for portable in plan.new_connections {
                let draft = NewConnection {
                    name: portable.name,
                    config: portable.config,
                    auto_connect: portable.auto_connect,
                    color: None,
                };
                match storage.create_connection(draft).await {
                    Ok(connection) => created.push(connection),
                    Err(error) => {
                        workspace
                            .update(cx, |workspace, cx| {
                                workspace.fail("import connection", error);
                                cx.notify();
                            })
                            .ok();
                        return;
                    }
                }
            }

            // A request whose connection came in with this document could not be
            // resolved when the plan was made, because that connection did not
            // exist yet. Resolve it now against what was just created.
            let mut requests = Vec::new();
            for (portable, existing) in plan.new_requests {
                let connection_id = existing.or_else(|| {
                    let name = portable.connection.as_ref()?;
                    created
                        .iter()
                        .find(|connection| connection.name == *name)
                        .map(|connection| connection.id)
                });

                // One that was already here is not in `made`, so it is resolved
                // from what the workspace holds.
                let collection_id = portable.collection.as_ref().and_then(|path| {
                    made.iter()
                        .find(|(created, _)| created == path)
                        .map(|(_, id)| *id)
                        .or_else(|| existing_collection(&here, path))
                });

                let draft = NewRequest {
                    collection_id,
                    connection_id,
                    name: portable.name,
                    kind: portable.kind,
                    target: portable.target,
                    schema: portable.schema,
                    input: portable.input,
                    visualization: None,
                };
                match storage.create_request(draft).await {
                    Ok(request) => requests.push(request),
                    Err(error) => {
                        workspace
                            .update(cx, |workspace, cx| {
                                workspace.fail("import request", error);
                                cx.notify();
                            })
                            .ok();
                        return;
                    }
                }
            }

            workspace
                .update(cx, |workspace, cx| {
                    workspace.connections.extend(created);
                    workspace.requests.extend(requests);
                    cx.notify();
                })
                .ok();
        })
    }

    /// Writes an edited request back to storage and to the in-memory list.
    pub fn save_request(&mut self, request: Request, cx: &mut Context<Self>) -> Task<()> {
        let storage = self.storage();
        // Update optimistically so the sidebar renames immediately; a failure
        // surfaces in `error` rather than silently reverting under the cursor.
        if let Some(existing) = self
            .requests
            .iter_mut()
            .find(|entry| entry.id == request.id)
        {
            *existing = request.clone();
        }
        cx.notify();

        cx.spawn(async move |workspace, cx| {
            let outcome = storage.update_request(&request).await;
            workspace
                .update(cx, |workspace, cx| {
                    if let Err(error) = outcome {
                        workspace.fail("save request", error);
                        cx.notify();
                    }
                })
                .ok();
        })
    }

    /// Copies a request under a new name, which is how you keep several calls to
    /// the same service with different payloads.
    pub fn duplicate_request(&mut self, id: i64, cx: &mut Context<Self>) -> Task<Option<Request>> {
        let storage = self.storage();
        let Some(source) = self.request(id).cloned() else {
            return Task::ready(None);
        };

        let draft = NewRequest {
            name: format!("{} copy", source.name),
            kind: source.kind,
            target: source.target.clone(),
            collection_id: source.collection_id,
            connection_id: source.connection_id,
            schema: source.schema.clone(),
            input: source.input.clone(),
            visualization: source.visualization.clone(),
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
                            workspace.fail("duplicate request", error);
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
                        Err(error) => workspace.fail("delete request", error),
                    }
                    cx.notify();
                })
                .ok();
        })
    }
}

/// Finds the collection at a `Robot/Arm` path among the ones already stored.
///
/// Walked segment by segment rather than matched on the leaf name: two
/// collections in different places can share a name, and the path is what
/// distinguishes them.
fn existing_collection(collections: &[Collection], path: &str) -> Option<i64> {
    let mut parent = None;
    let mut found = None;
    for name in path.split('/').filter(|part| !part.is_empty()) {
        let collection = collections
            .iter()
            .find(|entry| entry.parent_id == parent && entry.name == name)?;
        parent = Some(collection.id);
        found = Some(collection.id);
    }
    found
}
