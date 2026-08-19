//! Waiting a while, on both targets.
//!
//! GPUI's `BackgroundExecutor::timer` stamps every timer with
//! `std::time::Instant::now()`, and `Instant` does not exist on
//! `wasm32-unknown-unknown` — the call panics with "time not implemented on
//! this platform". So a repaint pump written the obvious way works natively and
//! takes the browser build down the moment a subscription starts.
//!
//! This is the one place that difference lives.

use std::time::Duration;

/// Waits `duration`, then returns.
#[cfg(not(target_family = "wasm"))]
pub async fn sleep(duration: Duration, cx: &gpui::AsyncApp) {
    cx.background_executor().timer(duration).await;
}

/// Waits `duration` using the browser's own timer.
///
/// `cx` is unused here but kept in the signature so callers do not have to know
/// which target they are on.
#[cfg(target_family = "wasm")]
pub async fn sleep(duration: Duration, _cx: &gpui::AsyncApp) {
    gloo_timers::future::TimeoutFuture::new(duration.as_millis() as u32).await;
}
