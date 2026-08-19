//! Frame packing for the ingest stream.
//!
//! Ported unchanged from the former Tauri command layer. The byte layout here
//! is a contract with `src/lib/workers/decoderCore.ts` (`WIRE_VERSION = 4`) and
//! with the end-to-end perf trace, so every field, flag and the trailing
//! timestamp patch must stay exactly as they were.

use rw_core::visualization::image::image_value_to_rgba;
use rw_wire::{
    flags as wire_flags, now_ns, pack_frame_raw, pack_frame_with_cbor_perf, perf_trace_enabled,
    FrameKind,
};

pub fn pack_value_frame(
    handle: &str,
    frame: &rw_transport::Frame,
    is_replay: bool,
    fields: &Option<Vec<String>>,
    payload_hint: &mut usize,
) -> Option<Vec<u8>> {
    let mut flags = 0u16;
    if is_replay {
        flags |= wire_flags::STALE_REPLAY;
    }
    let perf_enabled = perf_trace_enabled();
    let mut perf = frame.perf;
    if perf_enabled {
        if let Some(trace) = perf.as_mut() {
            trace.pack_start_ns = now_ns();
        }
    }

    let image = if frame.schema.viz_role.wire_id() == "image" {
        image_value_to_rgba(&frame.value)
    } else {
        None
    };

    let mut packed = match image {
        Some(image) => pack_frame_raw(
            handle,
            frame.timestamp_ns,
            FrameKind::Image,
            flags,
            &pack_image_payload(&image),
            perf,
        ),
        None => {
            let bytes = match pack_cbor_value(handle, frame, flags, fields, *payload_hint, perf) {
                Ok(bytes) => bytes,
                Err(err) => {
                    tracing::warn!(?err, "canonical value -> cbor envelope failed");
                    return None;
                }
            };
            *payload_hint = (*payload_hint * 7 / 8) + (bytes.len() / 8);
            bytes
        }
    };

    if perf_enabled && perf.is_some() {
        stamp_channel_send(&mut packed);
    }
    Some(packed)
}

fn pack_image_payload(image: &rw_core::visualization::image::ImageRgba) -> Vec<u8> {
    let mut payload = Vec::with_capacity(16 + image.rgba.len());
    payload.extend_from_slice(&image.width.to_le_bytes());
    payload.extend_from_slice(&image.height.to_le_bytes());
    payload.extend_from_slice(&[0u8; 8]);
    payload.extend_from_slice(&image.rgba);
    payload
}

fn pack_cbor_value(
    handle: &str,
    frame: &rw_transport::Frame,
    flags: u16,
    fields: &Option<Vec<String>>,
    payload_hint: usize,
    perf: Option<rw_wire::PerfTrace>,
) -> Result<Vec<u8>, rw_wire::CborPackError> {
    match fields {
        Some(selected) => pack_frame_with_cbor_perf(
            handle,
            frame.timestamp_ns,
            FrameKind::Value,
            flags,
            &rw_canonical::ProjectedValue::new(&frame.value, selected),
            payload_hint,
            perf,
        ),
        None => pack_frame_with_cbor_perf(
            handle,
            frame.timestamp_ns,
            FrameKind::Value,
            flags,
            &frame.value,
            payload_hint,
            perf,
        ),
    }
}

/// Stamps the moment the frame is handed to the transport into the last 8 bytes
/// of the perf tail. Kept under the historical name (`channel_send`) that the
/// TypeScript decoder still uses for this field.
fn stamp_channel_send(packed: &mut [u8]) {
    let tail = packed.len().saturating_sub(8);
    packed[tail..tail + 8].copy_from_slice(&now_ns().to_le_bytes());
}
