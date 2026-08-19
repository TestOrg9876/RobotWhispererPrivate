use idb::{
    Database, DatabaseEvent, Factory, IndexParams, KeyPath, ObjectStoreParams, TransactionMode,
};
use wasm_bindgen::JsValue;

use crate::domain::{Collection, Connection, Dashboard, Request};
use crate::ids::{CollectionId, ConnectionId, DashboardId, RequestId};
use crate::schema::SchemaDefinition;
use crate::storage::{NewCollection, NewConnection, NewDashboard, NewRequest, Storage};
use crate::{CoreError, CoreResult};

const DB_NAME: &str = "RobotWhispererWorkspace";
// Bumped when a store is added: the browser only runs `upgrade` when the
// version changes, so an existing database would otherwise have no dashboards
// store and every read of one would fail.
const DB_VERSION: u32 = 2;

const REQUESTS: &str = "requests";
const COLLECTIONS: &str = "collections";
const CONNECTIONS: &str = "connections";
const SCHEMAS: &str = "schemas";
const DASHBOARDS: &str = "dashboards";

#[derive(Debug)]
pub struct IdbStorage {
    db: Database,
}

impl IdbStorage {
    pub async fn open() -> CoreResult<Self> {
        let factory = Factory::new().map_err(idb_err)?;
        let mut request = factory.open(DB_NAME, Some(DB_VERSION)).map_err(idb_err)?;
        request.on_upgrade_needed(|event| {
            if let Err(err) = upgrade(event) {
                tracing::error!(?err, "idb upgrade failed");
            }
        });
        let db = request.await.map_err(idb_err)?;
        Ok(IdbStorage { db })
    }

    fn rw(&self, stores: &[&str]) -> CoreResult<idb::Transaction> {
        self.db
            .transaction(stores, TransactionMode::ReadWrite)
            .map_err(idb_err)
    }

    fn ro(&self, stores: &[&str]) -> CoreResult<idb::Transaction> {
        self.db
            .transaction(stores, TransactionMode::ReadOnly)
            .map_err(idb_err)
    }
}

fn upgrade(event: idb::event::VersionChangeEvent) -> CoreResult<()> {
    let db = event.database().map_err(idb_err)?;

    let mut params = ObjectStoreParams::new();
    params.auto_increment(true);
    params.key_path(Some(KeyPath::new_single("id")));
    db.create_object_store(REQUESTS, params.clone())
        .map_err(idb_err)?;
    db.create_object_store(COLLECTIONS, params.clone())
        .map_err(idb_err)?;

    let connections = db
        .create_object_store(CONNECTIONS, params)
        .map_err(idb_err)?;
    let mut idx_params = IndexParams::new();
    idx_params.unique(true);
    connections
        .create_index("name", KeyPath::new_single("name"), Some(idx_params))
        .map_err(idb_err)?;

    // `create_object_store` fails if it already exists, which it will for a
    // database upgraded from version 1 — but only for the stores that were
    // there before. This one is new either way, so a plain create is right for
    // a fresh database and for an upgrade alike.
    let mut dashboard_params = ObjectStoreParams::new();
    dashboard_params.auto_increment(true);
    dashboard_params.key_path(Some(KeyPath::new_single("id")));
    db.create_object_store(DASHBOARDS, dashboard_params)
        .map_err(idb_err)?;

    let mut schema_params = ObjectStoreParams::new();
    schema_params.key_path(Some(KeyPath::new_single("hash")));
    db.create_object_store(SCHEMAS, schema_params)
        .map_err(idb_err)?;
    Ok(())
}

fn idb_err<E: std::fmt::Display>(err: E) -> CoreError {
    CoreError::Storage(format!("idb: {err}"))
}

fn to_js<T: serde::Serialize>(value: &T) -> CoreResult<JsValue> {
    serde_wasm_bindgen::to_value(value).map_err(|err| CoreError::Storage(err.to_string()))
}

fn from_js<T: serde::de::DeserializeOwned>(value: JsValue) -> CoreResult<T> {
    serde_wasm_bindgen::from_value(value).map_err(|err| CoreError::Storage(err.to_string()))
}

/// A transaction that is waiting to finish.
///
/// IndexedDB commits a transaction as soon as control returns to the event loop
/// with no request outstanding. GPUI's executor resumes a future in a *later*
/// task than the one the request's success event ran in, so by the time our code
/// continues the transaction has already committed — and the explicit
/// `IDBTransaction.commit()` this code used to call then threw
/// `InvalidStateError`, failing every write in the browser build while the data
/// had in fact been stored.
///
/// Two rules follow, and this type exists to make the first hard to get wrong:
///
/// 1. The `complete` handler has to be attached *before* anything is awaited, or
///    the event fires with nobody listening and the wait never ends. Taking the
///    transaction by value here means the call has to come before the awaits.
/// 2. Every request in one transaction has to be **issued** before the first
///    await. A request issued afterwards runs against a transaction that is
///    already finished. Multi-step work therefore reads in one transaction and
///    writes in another.
struct Finishing(idb::TransactionFuture);

/// Attaches the completion handlers. Call this after issuing every request in
/// the transaction and before awaiting any of them.
fn finishing(tx: idb::Transaction) -> Finishing {
    Finishing(std::future::IntoFuture::into_future(tx))
}

impl Finishing {
    async fn wait(self) -> CoreResult<()> {
        match self.0.await.map_err(idb_err)? {
            idb::TransactionResult::Committed => Ok(()),
            idb::TransactionResult::Aborted => {
                Err(CoreError::Storage("idb: transaction aborted".into()))
            }
        }
    }
}

#[async_trait::async_trait(?Send)]
impl Storage for IdbStorage {
    async fn list_requests(&self) -> CoreResult<Vec<Request>> {
        let tx = self.ro(&[REQUESTS])?;
        let store = tx.object_store(REQUESTS).map_err(idb_err)?;
        let all = store
            .get_all(None, None)
            .map_err(idb_err)?
            .await
            .map_err(idb_err)?;
        let mut out = Vec::with_capacity(all.len());
        for entry in all {
            out.push(from_js(entry)?);
        }
        Ok(out)
    }

    async fn list_requests_by_connection(
        &self,
        connection_id: ConnectionId,
    ) -> CoreResult<Vec<Request>> {
        let all = self.list_requests().await?;
        Ok(all
            .into_iter()
            .filter(|r| r.connection_id == Some(connection_id))
            .collect())
    }

    async fn get_request(&self, id: RequestId) -> CoreResult<Option<Request>> {
        let tx = self.ro(&[REQUESTS])?;
        let store = tx.object_store(REQUESTS).map_err(idb_err)?;
        let value = store
            .get(JsValue::from_f64(id as f64))
            .map_err(idb_err)?
            .await
            .map_err(idb_err)?;
        match value {
            Some(v) => Ok(Some(from_js(v)?)),
            None => Ok(None),
        }
    }

    async fn create_request(&self, draft: NewRequest) -> CoreResult<Request> {
        let now = chrono::Utc::now();
        let candidate = Request {
            id: 0,
            collection_id: draft.collection_id,
            connection_id: draft.connection_id,
            name: draft.name,
            kind: draft.kind,
            target: draft.target,
            schema: draft.schema,
            input: draft.input,
            visualization: draft.visualization,
            created_at: now,
            updated_at: now,
        };
        let tx = self.rw(&[REQUESTS])?;
        let store = tx.object_store(REQUESTS).map_err(idb_err)?;
        let js = to_js(&candidate)?;
        let _ = js_sys::Reflect::delete_property(
            js.unchecked_ref::<js_sys::Object>(),
            &JsValue::from_str("id"),
        );
        let added = store.add(&js, None).map_err(idb_err)?;
        let finishing = finishing(tx);
        let key = added.await.map_err(idb_err)?;
        let id = key
            .as_f64()
            .ok_or_else(|| CoreError::Storage("non-numeric key".into()))? as i64;
        finishing.wait().await?;
        Ok(Request { id, ..candidate })
    }

    async fn update_request(&self, request: &Request) -> CoreResult<()> {
        let mut updated = request.clone();
        updated.updated_at = chrono::Utc::now();
        let tx = self.rw(&[REQUESTS])?;
        let store = tx.object_store(REQUESTS).map_err(idb_err)?;
        let written = store.put(&to_js(&updated)?, None).map_err(idb_err)?;
        let finishing = finishing(tx);
        written.await.map_err(idb_err)?;
        finishing.wait().await
    }

    async fn delete_request(&self, id: RequestId) -> CoreResult<()> {
        let tx = self.rw(&[REQUESTS])?;
        let store = tx.object_store(REQUESTS).map_err(idb_err)?;
        let written = store
            .delete(idb::Query::from(JsValue::from_f64(id as f64)))
            .map_err(idb_err)?;
        let finishing = finishing(tx);
        written.await.map_err(idb_err)?;
        finishing.wait().await
    }

    async fn list_dashboards(&self) -> CoreResult<Vec<Dashboard>> {
        let tx = self.ro(&[DASHBOARDS])?;
        let store = tx.object_store(DASHBOARDS).map_err(idb_err)?;
        let all = store
            .get_all(None, None)
            .map_err(idb_err)?
            .await
            .map_err(idb_err)?;
        let mut out = Vec::with_capacity(all.len());
        for entry in all {
            out.push(from_js(entry)?);
        }
        Ok(out)
    }

    async fn create_dashboard(&self, draft: NewDashboard) -> CoreResult<Dashboard> {
        let now = chrono::Utc::now();
        let candidate = Dashboard {
            id: 0,
            name: draft.name,
            layout: draft.layout,
            created_at: now,
            updated_at: now,
        };
        let tx = self.rw(&[DASHBOARDS])?;
        let store = tx.object_store(DASHBOARDS).map_err(idb_err)?;
        let js = to_js(&candidate)?;
        // The key is auto-assigned, so the placeholder has to go or the store
        // takes the zero as the id.
        let _ = js_sys::Reflect::delete_property(
            js.unchecked_ref::<js_sys::Object>(),
            &JsValue::from_str("id"),
        );
        let added = store.add(&js, None).map_err(idb_err)?;
        let finishing = finishing(tx);
        let key = added.await.map_err(idb_err)?;
        let id = key
            .as_f64()
            .ok_or_else(|| CoreError::Storage("non-numeric key".into()))? as i64;
        finishing.wait().await?;
        Ok(Dashboard { id, ..candidate })
    }

    async fn update_dashboard(&self, dashboard: &Dashboard) -> CoreResult<()> {
        let mut payload = dashboard.clone();
        payload.updated_at = chrono::Utc::now();
        let tx = self.rw(&[DASHBOARDS])?;
        let store = tx.object_store(DASHBOARDS).map_err(idb_err)?;
        let written = store.put(&to_js(&payload)?, None).map_err(idb_err)?;
        let finishing = finishing(tx);
        written.await.map_err(idb_err)?;
        finishing.wait().await
    }

    async fn delete_dashboard(&self, id: DashboardId) -> CoreResult<()> {
        let tx = self.rw(&[DASHBOARDS])?;
        let store = tx.object_store(DASHBOARDS).map_err(idb_err)?;
        let removed = store
            .delete(idb::Query::Key(JsValue::from_f64(id as f64)))
            .map_err(idb_err)?;
        let finishing = finishing(tx);
        removed.await.map_err(idb_err)?;
        finishing.wait().await
    }

    async fn list_collections(&self) -> CoreResult<Vec<Collection>> {
        let tx = self.ro(&[COLLECTIONS])?;
        let store = tx.object_store(COLLECTIONS).map_err(idb_err)?;
        let all = store
            .get_all(None, None)
            .map_err(idb_err)?
            .await
            .map_err(idb_err)?;
        let mut out = Vec::with_capacity(all.len());
        for entry in all {
            out.push(from_js(entry)?);
        }
        Ok(out)
    }

    async fn create_collection(&self, draft: NewCollection) -> CoreResult<Collection> {
        let now = chrono::Utc::now();
        let candidate = Collection {
            id: 0,
            parent_id: draft.parent_id,
            name: draft.name,
            created_at: now,
        };
        let tx = self.rw(&[COLLECTIONS])?;
        let store = tx.object_store(COLLECTIONS).map_err(idb_err)?;
        let js = to_js(&candidate)?;
        let _ = js_sys::Reflect::delete_property(
            js.unchecked_ref::<js_sys::Object>(),
            &JsValue::from_str("id"),
        );
        let added = store.add(&js, None).map_err(idb_err)?;
        let finishing = finishing(tx);
        let key = added.await.map_err(idb_err)?;
        let id = key
            .as_f64()
            .ok_or_else(|| CoreError::Storage("non-numeric key".into()))? as i64;
        finishing.wait().await?;
        Ok(Collection { id, ..candidate })
    }

    async fn update_collection(&self, collection: &Collection) -> CoreResult<()> {
        let tx = self.rw(&[COLLECTIONS])?;
        let store = tx.object_store(COLLECTIONS).map_err(idb_err)?;
        let written = store.put(&to_js(collection)?, None).map_err(idb_err)?;
        let finishing = finishing(tx);
        written.await.map_err(idb_err)?;
        finishing.wait().await
    }

    async fn delete_collection(&self, id: CollectionId) -> CoreResult<()> {
        // Read first, in its own transaction. Working out which collections
        // descend from this one needs the whole list, and a request issued after
        // that read would land on a transaction the browser had already
        // finished — see [`Finishing`].
        let collections = self.list_collections().await?;
        let mut doomed = vec![id];
        let mut queue = vec![id];
        while let Some(parent) = queue.pop() {
            for collection in &collections {
                if collection.parent_id == Some(parent) && !doomed.contains(&collection.id) {
                    doomed.push(collection.id);
                    queue.push(collection.id);
                }
            }
        }

        let orphaned: Vec<Request> = self
            .list_requests()
            .await?
            .into_iter()
            .filter(|request| {
                request
                    .collection_id
                    .is_some_and(|collection| doomed.contains(&collection))
            })
            .map(|request| Request {
                collection_id: None,
                ..request
            })
            .collect();

        // Then write, issuing every request before the first await.
        let tx = self.rw(&[COLLECTIONS, REQUESTS])?;
        let collection_store = tx.object_store(COLLECTIONS).map_err(idb_err)?;
        let request_store = tx.object_store(REQUESTS).map_err(idb_err)?;

        let mut writes = Vec::with_capacity(orphaned.len() + doomed.len());
        for request in &orphaned {
            writes.push(request_store.put(&to_js(request)?, None).map_err(idb_err)?);
        }
        let mut deletes = Vec::with_capacity(doomed.len());
        for collection in doomed {
            deletes.push(
                collection_store
                    .delete(idb::Query::from(JsValue::from_f64(collection as f64)))
                    .map_err(idb_err)?,
            );
        }

        let finishing = finishing(tx);
        for write in writes {
            write.await.map_err(idb_err)?;
        }
        for delete in deletes {
            delete.await.map_err(idb_err)?;
        }
        finishing.wait().await
    }

    async fn list_connections(&self) -> CoreResult<Vec<Connection>> {
        let tx = self.ro(&[CONNECTIONS])?;
        let store = tx.object_store(CONNECTIONS).map_err(idb_err)?;
        let all = store
            .get_all(None, None)
            .map_err(idb_err)?
            .await
            .map_err(idb_err)?;
        let mut out = Vec::with_capacity(all.len());
        for entry in all {
            out.push(from_js(entry)?);
        }
        Ok(out)
    }

    async fn get_connection(&self, id: ConnectionId) -> CoreResult<Option<Connection>> {
        let tx = self.ro(&[CONNECTIONS])?;
        let store = tx.object_store(CONNECTIONS).map_err(idb_err)?;
        let value = store
            .get(JsValue::from_f64(id as f64))
            .map_err(idb_err)?
            .await
            .map_err(idb_err)?;
        match value {
            Some(v) => Ok(Some(from_js(v)?)),
            None => Ok(None),
        }
    }

    async fn create_connection(&self, draft: NewConnection) -> CoreResult<Connection> {
        let now = chrono::Utc::now();
        let candidate = Connection {
            id: 0,
            name: draft.name,
            config: draft.config,
            auto_connect: draft.auto_connect,
            color: draft.color,
            created_at: now,
            updated_at: now,
        };
        let tx = self.rw(&[CONNECTIONS])?;
        let store = tx.object_store(CONNECTIONS).map_err(idb_err)?;
        let js = to_js(&candidate)?;
        let _ = js_sys::Reflect::delete_property(
            js.unchecked_ref::<js_sys::Object>(),
            &JsValue::from_str("id"),
        );
        let added = store.add(&js, None).map_err(idb_err)?;
        let finishing = finishing(tx);
        let key = added.await.map_err(idb_err)?;
        let id = key
            .as_f64()
            .ok_or_else(|| CoreError::Storage("non-numeric key".into()))? as i64;
        finishing.wait().await?;
        Ok(Connection { id, ..candidate })
    }

    async fn update_connection(&self, connection: &Connection) -> CoreResult<()> {
        let mut updated = connection.clone();
        updated.updated_at = chrono::Utc::now();
        let tx = self.rw(&[CONNECTIONS])?;
        let store = tx.object_store(CONNECTIONS).map_err(idb_err)?;
        let written = store.put(&to_js(&updated)?, None).map_err(idb_err)?;
        let finishing = finishing(tx);
        written.await.map_err(idb_err)?;
        finishing.wait().await
    }

    async fn delete_connection(&self, id: ConnectionId) -> CoreResult<()> {
        // Read, then write: see [`Finishing`] and `delete_collection`.
        let detached: Vec<Request> = self
            .list_requests()
            .await?
            .into_iter()
            .filter(|request| request.connection_id == Some(id))
            .map(|request| Request {
                connection_id: None,
                ..request
            })
            .collect();

        let tx = self.rw(&[CONNECTIONS, REQUESTS])?;
        let connection_store = tx.object_store(CONNECTIONS).map_err(idb_err)?;
        let request_store = tx.object_store(REQUESTS).map_err(idb_err)?;

        let mut writes = Vec::with_capacity(detached.len());
        for request in &detached {
            writes.push(request_store.put(&to_js(request)?, None).map_err(idb_err)?);
        }
        let removed = connection_store
            .delete(idb::Query::from(JsValue::from_f64(id as f64)))
            .map_err(idb_err)?;

        let finishing = finishing(tx);
        for write in writes {
            write.await.map_err(idb_err)?;
        }
        removed.await.map_err(idb_err)?;
        finishing.wait().await
    }

    async fn put_schema(&self, definition: &SchemaDefinition) -> CoreResult<()> {
        let tx = self.rw(&[SCHEMAS])?;
        let store = tx.object_store(SCHEMAS).map_err(idb_err)?;
        let written = store.put(&to_js(definition)?, None).map_err(idb_err)?;
        let finishing = finishing(tx);
        written.await.map_err(idb_err)?;
        finishing.wait().await
    }

    async fn get_schema(&self, hash: &str) -> CoreResult<Option<SchemaDefinition>> {
        let tx = self.ro(&[SCHEMAS])?;
        let store = tx.object_store(SCHEMAS).map_err(idb_err)?;
        let value = store
            .get(JsValue::from_str(hash))
            .map_err(idb_err)?
            .await
            .map_err(idb_err)?;
        match value {
            Some(v) => Ok(Some(from_js(v)?)),
            None => Ok(None),
        }
    }

    async fn list_schemas(&self) -> CoreResult<Vec<SchemaDefinition>> {
        let tx = self.ro(&[SCHEMAS])?;
        let store = tx.object_store(SCHEMAS).map_err(idb_err)?;
        let all = store
            .get_all(None, None)
            .map_err(idb_err)?
            .await
            .map_err(idb_err)?;
        let mut out = Vec::with_capacity(all.len());
        for entry in all {
            out.push(from_js(entry)?);
        }
        Ok(out)
    }

    async fn clear_all(&self) -> CoreResult<()> {
        let tx = self.rw(&[REQUESTS, COLLECTIONS, CONNECTIONS, SCHEMAS, DASHBOARDS])?;
        let mut clears = Vec::new();
        for store_name in [REQUESTS, COLLECTIONS, CONNECTIONS, SCHEMAS, DASHBOARDS] {
            clears.push(
                tx.object_store(store_name)
                    .map_err(idb_err)?
                    .clear()
                    .map_err(idb_err)?,
            );
        }

        let finishing = finishing(tx);
        for clear in clears {
            clear.await.map_err(idb_err)?;
        }
        finishing.wait().await
    }
}

use wasm_bindgen::JsCast as _;
