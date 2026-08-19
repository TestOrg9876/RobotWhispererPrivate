//! Loopback binary frame stream consumed by the frontend's decoder Web Worker.
//!
//! This is the hot path: every ROS message that reaches the UI passes through
//! here. It is deliberately a raw WebSocket rather than any RPC mechanism, so
//! frames land in a worker thread without touching the renderer's main thread.
//!
//! There is exactly one consumer (the decoder worker), so this is a
//! single-consumer channel rather than a broadcast. That matters: a broadcast
//! hands out `Arc<Vec<u8>>` and every client then has to clone the frame into
//! an owned `Vec` for tungstenite, which is a full memcpy of every frame. A
//! single `mpsc` moves the `Vec` straight through with no copy at all.

use std::sync::{Arc, RwLock};

use tokio::sync::mpsc;

/// Bounded so a stalled consumer applies backpressure as dropped frames rather
/// than unbounded memory growth. Matches the old broadcast capacity.
const INGEST_QUEUE_DEPTH: usize = 2048;

#[derive(Clone, Default)]
pub struct IngestHub {
    sink: Arc<RwLock<Option<mpsc::Sender<Vec<u8>>>>>,
}

impl std::fmt::Debug for IngestHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let connected = self
            .sink
            .read()
            .map(|guard| guard.is_some())
            .unwrap_or(false);
        f.debug_struct("IngestHub")
            .field("connected", &connected)
            .finish()
    }
}

impl IngestHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish one packed frame. Never blocks and never fails loudly: if no
    /// worker is attached yet, or the worker has fallen behind, the frame is
    /// dropped. Dropping is correct here — the UI only ever renders the most
    /// recent value per topic, and stalling the transport to preserve a frame
    /// nobody will draw would be strictly worse.
    pub fn send(&self, frame: Vec<u8>) {
        let guard = match self.sink.read() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(tx) = guard.as_ref() {
            if tx.try_send(frame).is_err() {
                tracing::trace!("ingest: consumer busy or gone, frame dropped");
            }
        }
    }

    /// Attach a consumer, displacing any previous one. The decoder worker
    /// restarts itself on error and reconnects, so a second attach is a normal
    /// event, not a fault: dropping the old sender ends the stale writer loop.
    pub fn attach(&self) -> mpsc::Receiver<Vec<u8>> {
        let (tx, rx) = mpsc::channel(INGEST_QUEUE_DEPTH);
        let mut guard = match self.sink.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        *guard = Some(tx);
        rx
    }
}
