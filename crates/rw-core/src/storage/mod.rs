use crate::domain::{
    Collection, Connection, Dashboard, HistoryEntry, NewHistoryEntry, Request, RequestKind,
    SchemaRef, TransportConfig, Value, Visualization,
};
use crate::ids::{CollectionId, ConnectionId, DashboardId, RequestId};
use crate::schema::SchemaDefinition;
use crate::CoreResult;
use serde::{Deserialize, Serialize};

pub mod export;
#[cfg(feature = "sqlite")]
pub mod sqlite;

#[cfg(all(target_family = "wasm", feature = "wasm-storage"))]
pub mod indexeddb;

pub use export::{export_workspace, import_workspace, ImportConflict, ImportMode, ImportReport};
#[cfg(all(target_family = "wasm", feature = "wasm-storage"))]
pub use indexeddb::IdbStorage;
#[cfg(feature = "sqlite")]
pub use sqlite::SqliteStorage;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewRequest {
    pub name: String,
    pub kind: RequestKind,
    pub target: String,
    pub collection_id: Option<CollectionId>,
    pub connection_id: Option<ConnectionId>,
    pub schema: Option<SchemaRef>,
    pub input: Value,
    #[serde(default)]
    pub visualization: Option<Visualization>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewCollection {
    pub name: String,
    pub parent_id: Option<CollectionId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewDashboard {
    pub name: String,
    pub layout: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewConnection {
    pub name: String,
    pub config: TransportConfig,
    pub auto_connect: bool,
    pub color: Option<String>,
}

#[cfg(not(target_family = "wasm"))]
#[async_trait::async_trait]
pub trait Storage: Send + Sync {
    async fn list_requests(&self) -> CoreResult<Vec<Request>>;
    async fn list_requests_by_connection(
        &self,
        connection_id: ConnectionId,
    ) -> CoreResult<Vec<Request>>;
    async fn get_request(&self, id: RequestId) -> CoreResult<Option<Request>>;
    async fn create_request(&self, draft: NewRequest) -> CoreResult<Request>;
    async fn update_request(&self, request: &Request) -> CoreResult<()>;
    async fn delete_request(&self, id: RequestId) -> CoreResult<()>;

    async fn list_collections(&self) -> CoreResult<Vec<Collection>>;
    async fn create_collection(&self, draft: NewCollection) -> CoreResult<Collection>;
    async fn update_collection(&self, collection: &Collection) -> CoreResult<()>;
    async fn delete_collection(&self, id: CollectionId) -> CoreResult<()>;

    async fn list_dashboards(&self) -> CoreResult<Vec<Dashboard>>;
    async fn create_dashboard(&self, draft: NewDashboard) -> CoreResult<Dashboard>;
    async fn update_dashboard(&self, dashboard: &Dashboard) -> CoreResult<()>;
    async fn delete_dashboard(&self, id: DashboardId) -> CoreResult<()>;

    async fn list_connections(&self) -> CoreResult<Vec<Connection>>;
    async fn get_connection(&self, id: ConnectionId) -> CoreResult<Option<Connection>>;
    async fn create_connection(&self, draft: NewConnection) -> CoreResult<Connection>;
    async fn update_connection(&self, connection: &Connection) -> CoreResult<()>;
    async fn delete_connection(&self, id: ConnectionId) -> CoreResult<()>;

    async fn put_schema(&self, definition: &SchemaDefinition) -> CoreResult<()>;
    async fn get_schema(&self, hash: &str) -> CoreResult<Option<SchemaDefinition>>;
    async fn list_schemas(&self) -> CoreResult<Vec<SchemaDefinition>>;

    /// Records a run. Trims that request's history to `cap` on the way in, so
    /// the ceiling holds without anything having to sweep.
    async fn record_history(&self, entry: NewHistoryEntry, cap: usize) -> CoreResult<HistoryEntry>;
    /// A request's runs, newest first.
    async fn list_history(
        &self,
        request_id: RequestId,
        limit: usize,
    ) -> CoreResult<Vec<HistoryEntry>>;
    async fn clear_history(&self, request_id: RequestId) -> CoreResult<()>;

    async fn clear_all(&self) -> CoreResult<()>;
}

#[cfg(target_family = "wasm")]
#[async_trait::async_trait(?Send)]
pub trait Storage {
    async fn list_requests(&self) -> CoreResult<Vec<Request>>;
    async fn list_requests_by_connection(
        &self,
        connection_id: ConnectionId,
    ) -> CoreResult<Vec<Request>>;
    async fn get_request(&self, id: RequestId) -> CoreResult<Option<Request>>;
    async fn create_request(&self, draft: NewRequest) -> CoreResult<Request>;
    async fn update_request(&self, request: &Request) -> CoreResult<()>;
    async fn delete_request(&self, id: RequestId) -> CoreResult<()>;

    async fn list_collections(&self) -> CoreResult<Vec<Collection>>;
    async fn create_collection(&self, draft: NewCollection) -> CoreResult<Collection>;
    async fn update_collection(&self, collection: &Collection) -> CoreResult<()>;
    async fn delete_collection(&self, id: CollectionId) -> CoreResult<()>;

    async fn list_dashboards(&self) -> CoreResult<Vec<Dashboard>>;
    async fn create_dashboard(&self, draft: NewDashboard) -> CoreResult<Dashboard>;
    async fn update_dashboard(&self, dashboard: &Dashboard) -> CoreResult<()>;
    async fn delete_dashboard(&self, id: DashboardId) -> CoreResult<()>;

    async fn list_connections(&self) -> CoreResult<Vec<Connection>>;
    async fn get_connection(&self, id: ConnectionId) -> CoreResult<Option<Connection>>;
    async fn create_connection(&self, draft: NewConnection) -> CoreResult<Connection>;
    async fn update_connection(&self, connection: &Connection) -> CoreResult<()>;
    async fn delete_connection(&self, id: ConnectionId) -> CoreResult<()>;

    async fn put_schema(&self, definition: &SchemaDefinition) -> CoreResult<()>;
    async fn get_schema(&self, hash: &str) -> CoreResult<Option<SchemaDefinition>>;
    async fn list_schemas(&self) -> CoreResult<Vec<SchemaDefinition>>;

    /// Records a run. Trims that request's history to `cap` on the way in, so
    /// the ceiling holds without anything having to sweep.
    async fn record_history(&self, entry: NewHistoryEntry, cap: usize) -> CoreResult<HistoryEntry>;
    /// A request's runs, newest first.
    async fn list_history(
        &self,
        request_id: RequestId,
        limit: usize,
    ) -> CoreResult<Vec<HistoryEntry>>;
    async fn clear_history(&self, request_id: RequestId) -> CoreResult<()>;

    async fn clear_all(&self) -> CoreResult<()>;
}
