//! Process-wide state.
//!
//! Replaces Tauri's type-keyed managed-state map (five `app.manage()` calls and
//! 28 `State<'_, T>` injections) with one plain struct passed by reference.

use std::path::PathBuf;
use std::sync::Arc;

use rw_core::schema::SchemaRegistry;
use rw_core::storage::{SqliteStorage, Storage};
use rw_core::util::{Clock, SystemClock};
use rw_pipeline::CanonicalPipeline;

use crate::ingest::IngestHub;

pub struct AppState {
    pub storage: Arc<dyn Storage>,
    pub clock: Arc<dyn Clock>,
    pub registry: Arc<SchemaRegistry>,
    pub pipeline: CanonicalPipeline,
    pub hub: IngestHub,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("hub", &self.hub)
            .finish_non_exhaustive()
    }
}

impl AppState {
    pub async fn bootstrap(data_dir: PathBuf) -> anyhow_lite::Result<Arc<Self>> {
        std::fs::create_dir_all(&data_dir)
            .map_err(|err| format!("create app data dir {}: {err}", data_dir.display()))?;
        let db_path = data_dir.join("workspace.db");

        let clock: Arc<dyn Clock> = Arc::new(SystemClock::new());
        let storage: Arc<dyn Storage> = Arc::new(
            SqliteStorage::open(&db_path, clock.clone())
                .map_err(|err| format!("open {}: {err}", db_path.display()))?,
        );
        tracing::info!(path = %db_path.display(), "workspace storage opened");

        let registry = SchemaRegistry::new(storage.clone())
            .await
            .map_err(|err| format!("build schema registry: {err}"))?;
        registry
            .ensure_defaults()
            .await
            .map_err(|err| format!("install bundled schemas: {err}"))?;
        let registry = Arc::new(registry);
        tracing::info!("schema registry bootstrapped");

        let pipeline = CanonicalPipeline::with_schema_registry(registry.clone());

        Ok(Arc::new(AppState {
            storage,
            clock,
            registry,
            pipeline,
            hub: IngestHub::new(),
        }))
    }
}

/// Resolve the per-user data directory. Under Tauri this came from
/// `app.path().app_data_dir()`. The Electron shell passes `--data-dir` (from
/// `app.getPath("userData")`) so the two halves of the app agree on one
/// location; the fallback keeps the daemon runnable standalone, which is what
/// makes the core testable without any frontend at all.
pub fn resolve_data_dir(explicit: Option<PathBuf>) -> PathBuf {
    if let Some(dir) = explicit {
        return dir;
    }
    directories::ProjectDirs::from("com", "mmarfeychuk", "robot-whisperer")
        .map(|dirs| dirs.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".robot-whisperer"))
}

/// Minimal error plumbing so bootstrap can report a readable reason without
/// pulling in a dependency purely for `main`.
pub mod anyhow_lite {
    pub type Result<T> = std::result::Result<T, String>;
}
