//! Timing shared by the transports.
//!
//! This crate was the binary wire envelope for Tauri IPC: a version byte, a
//! frame kind, length-prefixed fields and an optional CBOR payload, packed on
//! the Rust side and unpacked in the webview. The GPUI app is one process, so
//! there is no IPC and none of that had a caller — `pack_frame_raw`,
//! `unpack_frame`, `UnpackedFrame`, `FrameKind`, `WIRE_VERSION` and the CBOR
//! path went with it, along with `ciborium` and `serde`.
//!
//! What is left is the part every transport still uses: a monotonic-enough
//! clock that works on both targets (`std::time` is not available on
//! `wasm32-unknown-unknown`), and the per-frame timing trace.
#![deny(missing_debug_implementations)]
#![deny(unused_must_use)]

use std::sync::atomic::{AtomicBool, Ordering};

pub const WIRE_VERSION: u8 = 4;

pub const PERF_TRACE_SIZE: usize = 5 * 8;

static PERF_TRACE_ENABLED: AtomicBool = AtomicBool::new(false);

#[inline]
pub fn perf_trace_enabled() -> bool {
    PERF_TRACE_ENABLED.load(Ordering::Relaxed)
}

pub fn set_perf_trace_enabled(enabled: bool) {
    PERF_TRACE_ENABLED.store(enabled, Ordering::Relaxed);
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PerfTrace {
    pub ws_recv_ns: u64,
    pub decode_start_ns: u64,
    pub decode_end_ns: u64,
    pub pack_start_ns: u64,
    pub channel_send_ns: u64,
}

impl PerfTrace {
    pub fn on_ws_recv() -> Self {
        Self {
            ws_recv_ns: now_ns(),
            ..Self::default()
        }
    }
}

#[cfg(not(target_family = "wasm"))]
#[inline]
pub fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(target_family = "wasm")]
#[inline]
pub fn now_ns() -> u64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map(|perf| ((perf.time_origin() + perf.now()) * 1.0e6) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_moves_forward_and_is_not_zero() {
        // The whole reason this crate still exists: `std::time::Instant` does
        // not work on `wasm32-unknown-unknown`, so every transport asks here
        // instead of reaching for the standard library directly.
        let first = now_ns();
        assert!(first > 0, "the clock returned zero");
        let second = now_ns();
        assert!(second >= first, "{second} is before {first}");
    }

    #[test]
    fn a_trace_starts_stamped_at_the_frame_and_nowhere_else() {
        let trace = PerfTrace::on_ws_recv();
        assert!(trace.ws_recv_ns > 0);
        assert_eq!(trace.decode_start_ns, 0);
        assert_eq!(trace.decode_end_ns, 0);
        assert_eq!(trace.pack_start_ns, 0);
        assert_eq!(trace.channel_send_ns, 0);
    }

    #[test]
    fn tracing_is_off_until_it_is_asked_for() {
        // Off by default because it stamps five clocks per frame, and a
        // 200 Hz topic does not want them.
        let was = perf_trace_enabled();
        set_perf_trace_enabled(false);
        assert!(!perf_trace_enabled());
        set_perf_trace_enabled(true);
        assert!(perf_trace_enabled());
        set_perf_trace_enabled(was);
    }
}
