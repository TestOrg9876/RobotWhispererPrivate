//! Decoding a ROS image message into something that can be drawn.
//!
//! `sensor_msgs/Image` carries raw pixels in whatever encoding the driver
//! chose, and `sensor_msgs/CompressedImage` carries a JPEG or PNG. Both end up
//! as RGBA for GPUI.
//!
//! The conversion is pure and tested; only [`Frame`] touches GPUI.

use std::sync::Arc;

use gpui::{ImageSource, RenderImage};
use image_crate::RgbaImage;
use rw_canonical::CanonicalValue;

/// Refuse anything larger than this rather than allocating it.
///
/// 8K RGBA is 132 MB; a malformed header claiming 4 billion pixels should be a
/// blank panel, not a dead process.
const MAX_PIXELS: usize = 8192 * 8192;

/// A decoded frame, ready to draw.
pub struct Frame {
    pub source: ImageSource,
    /// What it was, for the line under the picture.
    pub caption: String,
}

/// The parts of an image message that matter, pulled out of the canonical value.
#[derive(Debug, Clone, PartialEq)]
pub struct Raw {
    pub width: usize,
    pub height: usize,
    pub encoding: String,
    pub step: usize,
    pub data: Vec<u8>,
}

/// Reads an image message, if this is one.
pub fn decode(value: &CanonicalValue) -> Option<Frame> {
    if let Some(raw) = raw(value) {
        let caption = format!("{}×{}  {}", raw.width, raw.height, raw.encoding);
        let rgba = to_rgba(&raw)?;
        return Some(Frame {
            source: source(rgba),
            caption,
        });
    }

    let (format, bytes) = compressed(value)?;
    let decoded = image_crate::load_from_memory(&bytes).ok()?.into_rgba8();
    let caption = format!("{}×{}  {format}", decoded.width(), decoded.height());
    Some(Frame {
        source: source(decoded),
        caption,
    })
}

fn source(rgba: RgbaImage) -> ImageSource {
    let (width, height) = (rgba.width(), rgba.height());
    // GPUI's render images are BGRA; the crate hands back RGBA.
    let mut buffer = rgba.into_raw();
    for pixel in buffer.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let bgra = RgbaImage::from_raw(width, height, buffer)
        .expect("the buffer came from an image of this size");
    ImageSource::Render(Arc::new(RenderImage::new(vec![image_crate::Frame::new(
        bgra,
    )])))
}

/// Pulls the fields of a `sensor_msgs/Image` out of a message.
pub fn raw(value: &CanonicalValue) -> Option<Raw> {
    let CanonicalValue::Struct(fields) = value else {
        return None;
    };

    let width = number(fields.get("width")?)?;
    let height = number(fields.get("height")?)?;
    let encoding = match fields.get("encoding")? {
        CanonicalValue::String(inner) => inner.clone(),
        _ => return None,
    };
    let data = bytes(fields.get("data")?)?;
    // `step` is the row stride in bytes, which is not always width × pixel size:
    // some drivers pad rows. Falling back to the tight value is right for the
    // ones that do not send it.
    let step = fields
        .get("step")
        .and_then(number)
        .filter(|step| *step > 0)
        .unwrap_or_else(|| data.len() / height.max(1));

    // Checked, because the whole point of the cap is to survive a header
    // claiming a size that does not fit in a `usize` in the first place.
    let pixels = width.checked_mul(height)?;
    (width > 0 && height > 0 && pixels <= MAX_PIXELS).then_some(Raw {
        width,
        height,
        encoding,
        step,
        data,
    })
}

/// Pulls the format and payload out of a `sensor_msgs/CompressedImage`.
fn compressed(value: &CanonicalValue) -> Option<(String, Vec<u8>)> {
    let CanonicalValue::Struct(fields) = value else {
        return None;
    };
    let format = match fields.get("format")? {
        CanonicalValue::String(inner) => inner.clone(),
        _ => return None,
    };
    // A message with `format` and `data` but no `width` is the compressed one;
    // requiring the absence of `width` keeps this from claiming raw images.
    if fields.contains_key("width") {
        return None;
    }
    Some((format, bytes(fields.get("data")?)?))
}

fn number(value: &CanonicalValue) -> Option<usize> {
    match value {
        CanonicalValue::Uint(inner) => usize::try_from(*inner).ok(),
        CanonicalValue::Int(inner) => usize::try_from(*inner).ok(),
        _ => None,
    }
}

fn bytes(value: &CanonicalValue) -> Option<Vec<u8>> {
    match value {
        CanonicalValue::Bytes(inner) => Some(inner.clone()),
        // A transport that decodes `uint8[]` as an array of numbers rather than
        // a blob is not wrong, and its images should still be visible.
        CanonicalValue::Array(items) => items
            .iter()
            .map(|item| match item {
                CanonicalValue::Uint(inner) => u8::try_from(*inner).ok(),
                CanonicalValue::Int(inner) => u8::try_from(*inner).ok(),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

/// Converts raw pixels to RGBA, or gives up on an encoding we do not know.
///
/// The encodings here are the ones cameras and depth sensors actually publish
/// on a ROS system. Anything else shows as unsupported rather than as noise.
pub fn to_rgba(raw: &Raw) -> Option<RgbaImage> {
    let channels = match raw.encoding.as_str() {
        "rgb8" | "bgr8" => 3,
        "rgba8" | "bgra8" => 4,
        "mono8" | "8UC1" => 1,
        "mono16" | "16UC1" => 2,
        _ => return None,
    };
    let row_bytes = raw.width.checked_mul(channels)?;
    if raw.step < row_bytes {
        return None;
    }
    if raw.data.len() < raw.step.checked_mul(raw.height)? {
        return None;
    }

    let mut out = Vec::with_capacity(raw.width.checked_mul(raw.height)?.checked_mul(4)?);
    for y in 0..raw.height {
        let row = &raw.data[y * raw.step..][..row_bytes];
        for pixel in row.chunks_exact(channels) {
            let [r, g, b, a] = match (raw.encoding.as_str(), pixel) {
                ("rgb8", [r, g, b]) => [*r, *g, *b, 255],
                ("bgr8", [b, g, r]) => [*r, *g, *b, 255],
                ("rgba8", [r, g, b, a]) => [*r, *g, *b, *a],
                ("bgra8", [b, g, r, a]) => [*r, *g, *b, *a],
                ("mono8" | "8UC1", [grey]) => [*grey, *grey, *grey, 255],
                // 16-bit depth, shown as its high byte: the full range is not
                // visible on screen anyway, and this at least shows the shape.
                ("mono16" | "16UC1", [_low, high]) => [*high, *high, *high, 255],
                _ => return None,
            };
            out.extend_from_slice(&[r, g, b, a]);
        }
    }

    RgbaImage::from_raw(raw.width as u32, raw.height as u32, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn message(fields: &[(&str, CanonicalValue)]) -> CanonicalValue {
        CanonicalValue::Struct(
            fields
                .iter()
                .map(|(name, value)| (name.to_string(), value.clone()))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    fn image_message(encoding: &str, width: usize, height: usize, data: Vec<u8>) -> CanonicalValue {
        message(&[
            ("width", CanonicalValue::Uint(width as u64)),
            ("height", CanonicalValue::Uint(height as u64)),
            ("encoding", CanonicalValue::String(encoding.into())),
            ("data", CanonicalValue::Bytes(data)),
        ])
    }

    #[test]
    fn rgb_becomes_rgba_with_full_alpha() {
        let raw = raw(&image_message("rgb8", 2, 1, vec![1, 2, 3, 4, 5, 6])).expect("an image");
        let rgba = to_rgba(&raw).expect("converted");
        assert_eq!(rgba.as_raw(), &[1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    fn bgr_channels_are_put_back_in_order() {
        let raw = raw(&image_message("bgr8", 1, 1, vec![10, 20, 30])).expect("an image");
        let rgba = to_rgba(&raw).expect("converted");
        assert_eq!(rgba.as_raw(), &[30, 20, 10, 255]);
    }

    #[test]
    fn mono_becomes_grey() {
        let raw = raw(&image_message("mono8", 2, 1, vec![7, 200])).expect("an image");
        let rgba = to_rgba(&raw).expect("converted");
        assert_eq!(rgba.as_raw(), &[7, 7, 7, 255, 200, 200, 200, 255]);
    }

    #[test]
    fn padded_rows_are_skipped_rather_than_read_as_pixels() {
        // Drivers pad rows to an alignment; reading the padding as pixels is how
        // an image comes out sheared.
        let value = message(&[
            ("width", CanonicalValue::Uint(2)),
            ("height", CanonicalValue::Uint(2)),
            ("encoding", CanonicalValue::String("mono8".into())),
            ("step", CanonicalValue::Uint(4)),
            (
                "data",
                CanonicalValue::Bytes(vec![1, 2, 99, 99, 3, 4, 99, 99]),
            ),
        ]);
        let raw = raw(&value).expect("an image");
        assert_eq!(raw.step, 4);
        let rgba = to_rgba(&raw).expect("converted");
        assert_eq!(
            rgba.as_raw(),
            &[1, 1, 1, 255, 2, 2, 2, 255, 3, 3, 3, 255, 4, 4, 4, 255]
        );
    }

    #[test]
    fn a_missing_step_falls_back_to_a_tight_stride() {
        let raw = raw(&image_message("rgb8", 2, 2, vec![0; 12])).expect("an image");
        assert_eq!(raw.step, 6);
    }

    #[test]
    fn an_unknown_encoding_is_declined_rather_than_guessed() {
        let raw = raw(&image_message("bayer_rggb8", 2, 1, vec![0; 2])).expect("an image");
        assert!(to_rgba(&raw).is_none());
    }

    #[test]
    fn short_data_is_declined() {
        // A truncated message must not be read past its end.
        let raw = raw(&image_message("rgb8", 4, 4, vec![0; 8])).expect("an image");
        assert!(to_rgba(&raw).is_none());
    }

    #[test]
    fn an_absurd_size_is_refused_before_it_is_allocated() {
        let value = image_message("rgb8", usize::MAX / 2, 4, vec![0; 8]);
        assert!(raw(&value).is_none());
    }

    #[test]
    fn a_message_that_is_not_an_image_is_not_one() {
        assert!(raw(&message(&[("data", CanonicalValue::Bytes(vec![1]))])).is_none());
        assert!(raw(&CanonicalValue::F64(1.0)).is_none());
    }

    #[test]
    fn data_sent_as_an_array_of_numbers_still_reads() {
        // Some transports decode `uint8[]` as numbers rather than a blob, and
        // those images should still be visible.
        let value = message(&[
            ("width", CanonicalValue::Uint(1)),
            ("height", CanonicalValue::Uint(1)),
            ("encoding", CanonicalValue::String("rgb8".into())),
            (
                "data",
                CanonicalValue::Array(vec![
                    CanonicalValue::Uint(9),
                    CanonicalValue::Uint(8),
                    CanonicalValue::Uint(7),
                ]),
            ),
        ]);
        let raw = raw(&value).expect("an image");
        assert_eq!(to_rgba(&raw).expect("converted").as_raw(), &[9, 8, 7, 255]);
    }

    #[test]
    fn a_compressed_image_is_told_apart_from_a_raw_one() {
        let jpeg = message(&[
            ("format", CanonicalValue::String("jpeg".into())),
            ("data", CanonicalValue::Bytes(vec![0xff, 0xd8, 0xff])),
        ]);
        assert_eq!(
            compressed(&jpeg),
            Some(("jpeg".to_string(), vec![0xff, 0xd8, 0xff]))
        );
        // A raw image has `width`, and must not be taken for a compressed one.
        assert!(compressed(&image_message("rgb8", 1, 1, vec![0; 3])).is_none());
    }
}
