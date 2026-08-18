//! Browser shell: IndexedDB-backed storage, GPUI's web platform.
#![cfg(target_family = "wasm")]

use std::cell::RefCell;
use std::sync::Arc;

use rw_core::schema::SchemaRegistry;
use rw_core::storage::{IdbStorage, Storage};
use rw_pipeline::CanonicalPipeline;
use wasm_bindgen::prelude::*;

thread_local! {
    /// Keeps the application alive after [`run`] returns.
    ///
    /// On native platforms `Platform::run` blocks for the life of the process
    /// and the `Application` lives in that stack frame. The web platform hands
    /// control back to the browser's event loop instead and returns
    /// immediately — so `Application::run` would drop the entire app the moment
    /// the entry point finished, leaving a page with no canvas and no error.
    /// `run_embedded` returns a handle for exactly this, and it has to be held
    /// somewhere that outlives the call.
    static APPLICATION: RefCell<Option<gpui::ApplicationHandle>> = const { RefCell::new(None) };
}

/// Entry point invoked by the host page once the wasm module is instantiated.
#[wasm_bindgen]
pub async fn run() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    console_log::init_with_level(log::Level::Info).ok();
    tracing_wasm::set_as_global_default();
    gpui_platform::web_init();

    // Start-up is a sequence of awaits — IndexedDB, then the schema registry —
    // any of which can stall with nothing on screen but the host page's
    // "Loading" text. Each stage announces itself so a hang names its cause.
    tracing::info!("opening IndexedDB");
    let storage = Arc::new(
        IdbStorage::open()
            .await
            .map_err(|error| JsValue::from_str(&format!("opening IndexedDB: {error}")))?,
    );

    tracing::info!("selecting a graphics backend");
    // `Auto` probes WebGPU first, and a software adapter (SwiftShader, as used
    // in headless CI) advertises support but renders nothing. WebGL2 is the
    // dependable path there, so it is requested explicitly.
    let platform = std::rc::Rc::new(gpui_web::WebPlatform::new_with_backend(
        false,
        gpui_web::WebBackendPreference::WebGl,
    ));
    let http_client = Arc::new(platform.fetch_http_client());
    let app = gpui::Application::with_platform(platform)
        .with_http_client(http_client)
        .with_assets(rw_ui::assets::Assets);

    tracing::info!("loading the schema registry");
    let storage_dyn: Arc<dyn Storage> = storage.clone();
    let registry = Arc::new(
        SchemaRegistry::new(storage_dyn.clone())
            .await
            .map_err(|error| JsValue::from_str(&format!("schema registry: {error}")))?,
    );
    let pipeline = Arc::new(CanonicalPipeline::with_schema_registry(registry));

    tracing::info!("starting the application");
    let handle = app.run_embedded(move |cx| {
        if let Err(error) = rw_ui::init(storage_dyn, pipeline, cx) {
            tracing::error!("initialisation failed: {error:#}");
            return;
        }
        if let Err(error) = rw_ui::open_window(cx) {
            tracing::error!("could not open a window: {error:#}");
            return;
        }
        tracing::info!("window open");
    });
    APPLICATION.with(|application| *application.borrow_mut() = Some(handle));

    Ok(())
}
