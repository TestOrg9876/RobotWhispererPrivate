//! Native shell: SQLite-backed storage, a real window, tokio for transports.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use rw_core::schema::SchemaRegistry;
use rw_core::storage::{SqliteStorage, Storage};
use rw_core::util::SystemClock;
use rw_pipeline::CanonicalPipeline;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rw_ui=info,rw_pipeline=info".into()),
        )
        .init();

    // `SqliteStorage` reaches for `tokio::task::spawn_blocking`, and the
    // transports need a reactor too, but GPUI runs its own executor. Enter a
    // tokio context on the main thread and hold it for the life of the app so
    // `Handle::current()` resolves from GPUI's foreground tasks.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the tokio runtime")?;
    let _runtime_guard = runtime.enter();

    let storage = Arc::new(
        SqliteStorage::open(&database_path()?, Arc::new(SystemClock))
            .context("opening the workspace database")?,
    );

    // The schema registry is async to build and both the pipeline and the UI
    // need it, so it is created before the app loop rather than inside it.
    let storage_dyn: Arc<dyn Storage> = storage.clone();
    let registry = Arc::new(
        runtime
            .block_on(SchemaRegistry::new(storage_dyn.clone()))
            .context("loading the schema registry")?,
    );
    let pipeline = Arc::new(CanonicalPipeline::with_schema_registry(registry));

    gpui_platform::application()
        .with_assets(rw_ui::assets::Assets)
        .run(move |cx| {
            if let Err(error) = rw_ui::init(storage_dyn, pipeline, None, cx) {
                tracing::error!("initialisation failed: {error:#}");
                cx.quit();
                return;
            }
            if let Err(error) = rw_ui::open_window(cx) {
                tracing::error!("could not open a window: {error:#}");
                cx.quit();
            }
        });

    Ok(())
}

/// `$XDG_DATA_HOME/robot-whisperer/workspace.db`, falling back to `$HOME/.local/share`.
fn database_path() -> Result<std::path::PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|home| std::path::PathBuf::from(home).join(".local").join("share"))
        })
        .context("neither XDG_DATA_HOME nor HOME is set")?;

    let directory = base.join("robot-whisperer");
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("creating {}", directory.display()))?;
    Ok(directory.join("workspace.db"))
}
