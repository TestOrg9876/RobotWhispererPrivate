//! Browser shell: IndexedDB-backed storage, GPUI's web platform.
#![cfg(target_family = "wasm")]

use std::sync::Arc;

use rw_core::storage::IdbStorage;
use wasm_bindgen::prelude::*;

/// Entry point invoked by the host page once the wasm module is instantiated.
#[wasm_bindgen]
pub async fn run() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    console_log::init_with_level(log::Level::Info).ok();
    tracing_wasm::set_as_global_default();
    gpui_platform::web_init();

    let storage = Arc::new(
        IdbStorage::open()
            .await
            .map_err(|error| JsValue::from_str(&format!("opening IndexedDB: {error}")))?,
    );

    // `Auto` probes WebGPU first, and a software adapter (SwiftShader, as used
    // in headless CI) advertises support but renders nothing. WebGL2 is the
    // dependable path there, so it is requested explicitly.
    let platform = std::rc::Rc::new(gpui_web::WebPlatform::new_with_backend(
        false,
        gpui_web::WebBackendPreference::WebGl,
    ));
    let http_client = Arc::new(platform.fetch_http_client());
    let app = gpui::Application::with_platform(platform).with_http_client(http_client);

    app.run(move |cx| {
        if let Err(error) = rw_ui::init(storage, None, cx) {
            tracing::error!("initialisation failed: {error:#}");
            return;
        }
        if let Err(error) = rw_ui::open_window(cx) {
            tracing::error!("could not open a window: {error:#}");
        }
    });

    Ok(())
}
