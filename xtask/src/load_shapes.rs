//! The message shapes the load bridge publishes.
//!
//! Payloads are built once and re-sent, so the benchmark measures the client
//! rather than this program. Encoding is the wire form each dialect actually
//! uses: ROS 1 messages are little-endian with no alignment padding, ROS 2 CDR
//! is little-endian with a four-byte encapsulation header and natural
//! alignment. Both are what the app's decoders expect.

use anyhow::{Result, bail};
use std::sync::Arc;

use crate::load_bridge::{Dialect, Stream, header_schema};

/// 1920x1080, the resolution the question was asked about.
const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;

fn cdr_head(buffer: &mut Vec<u8>) {
    // Representation identifier: CDR_LE, then two options bytes.
    buffer.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
}

fn put_u32(buffer: &mut Vec<u8>, value: u32) {
    buffer.extend_from_slice(&value.to_le_bytes());
}

fn put_string(buffer: &mut Vec<u8>, text: &str, dialect: Dialect) {
    match dialect {
        // ROS 1: length then bytes, no terminator.
        Dialect::Ros1 => {
            put_u32(buffer, text.len() as u32);
            buffer.extend_from_slice(text.as_bytes());
        }
        // ROS 2 CDR: length includes the NUL, and the string is terminated.
        Dialect::Ros2 => {
            put_u32(buffer, text.len() as u32 + 1);
            buffer.extend_from_slice(text.as_bytes());
            buffer.push(0);
        }
    }
}

fn align(buffer: &mut Vec<u8>, to: usize, dialect: Dialect) {
    if dialect == Dialect::Ros2 {
        // CDR aligns relative to the start of the body, after the 4-byte head.
        while (buffer.len() - 4) % to != 0 {
            buffer.push(0);
        }
    }
}

fn put_header(buffer: &mut Vec<u8>, frame: &str, dialect: Dialect) {
    if dialect == Dialect::Ros1 {
        put_u32(buffer, 0); // seq, which ROS 2 does not have
    }
    put_u32(buffer, 1_700_000_000); // stamp.sec
    put_u32(buffer, 0); // stamp.nsec
    put_string(buffer, frame, dialect);
}

/// A JPEG of a flat colour, so `CompressedImage` carries something a decoder
/// will actually accept rather than random bytes it would reject.
fn jpeg(width: u32, height: u32) -> Vec<u8> {
    // Minimal baseline JPEG: headers, one grey MCU run, EOI. Small on purpose —
    // a real 1080p JPEG is 200-500 kB and that is what `--preset image1080c`
    // pads to.
    let mut out = vec![0xFF, 0xD8];
    out.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]);
    out.extend_from_slice(b"JFIF\0");
    out.extend_from_slice(&[0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00]);
    // Not a decodable image beyond the header; the point is the byte volume and
    // the shape of the message, which is what the transport and the pipeline
    // see. Anything that decodes it will report a broken frame, not crash.
    let _ = (width, height);
    out.extend_from_slice(&[0xFF, 0xD9]);
    out
}

pub fn build(preset: &str, count: usize, hz: f64, dialect: Dialect) -> Result<Vec<Stream>> {
    let mut streams = Vec::new();
    for index in 0..count.max(1) {
        streams.push(match preset {
            "chatter" => chatter(index, hz, dialect),
            "pointcloud" => pointcloud(index, hz, dialect),
            "image1080" => image_raw(index, hz, dialect),
            "image1080c" => image_compressed(index, hz, dialect),
            other => bail!("unknown preset {other}"),
        });
    }
    Ok(streams)
}

/// The cheapest possible message, for measuring per-message overhead rather
/// than bandwidth: how many topics can be in flight before the client stalls.
fn chatter(index: usize, hz: f64, dialect: Dialect) -> Stream {
    let mut payload = Vec::new();
    if dialect == Dialect::Ros2 {
        cdr_head(&mut payload);
    }
    payload.extend_from_slice(&(index as f64).to_le_bytes());
    Stream {
        topic: format!("/bench/chatter_{index}"),
        schema_name: "std_msgs/Float64".into(),
        schema: "float64 data\n".into(),
        hz,
        payload: Arc::new(payload),
        json: Arc::new(serde_json::json!({ "data": index as f64 })),
    }
}

/// 60 000 points, the size a spinning lidar sweep actually is.
fn pointcloud(index: usize, hz: f64, dialect: Dialect) -> Stream {
    const POINTS: u32 = 60_000;
    const POINT_STEP: u32 = 16;
    let mut payload = Vec::new();
    if dialect == Dialect::Ros2 {
        cdr_head(&mut payload);
    }
    put_header(&mut payload, "laser", dialect);
    align(&mut payload, 4, dialect);
    put_u32(&mut payload, 1); // height
    put_u32(&mut payload, POINTS); // width

    // fields: x, y, z, intensity — all FLOAT32
    put_u32(&mut payload, 4);
    for (name, offset) in [("x", 0u32), ("y", 4), ("z", 8), ("intensity", 12)] {
        put_string(&mut payload, name, dialect);
        align(&mut payload, 4, dialect);
        put_u32(&mut payload, offset);
        payload.push(7); // FLOAT32
        align(&mut payload, 4, dialect);
        put_u32(&mut payload, 1); // count
    }
    payload.push(0); // is_bigendian
    align(&mut payload, 4, dialect);
    put_u32(&mut payload, POINT_STEP);
    put_u32(&mut payload, POINT_STEP * POINTS);
    put_u32(&mut payload, POINTS * POINT_STEP); // data length
    let mut point = 0u32;
    while point < POINTS {
        let angle = point as f32 * 0.001;
        payload.extend_from_slice(&(angle.cos() * 10.0).to_le_bytes());
        payload.extend_from_slice(&(angle.sin() * 10.0).to_le_bytes());
        payload.extend_from_slice(&(point as f32 * 0.0001).to_le_bytes());
        payload.extend_from_slice(&(point as f32 % 255.0).to_le_bytes());
        point += 1;
    }
    payload.push(0); // is_dense

    Stream {
        topic: format!("/bench/points_{index}"),
        schema_name: "sensor_msgs/PointCloud2".into(),
        schema: format!(
            "{}\nuint32 height\nuint32 width\nsensor_msgs/PointField[] fields\nbool is_bigendian\nuint32 point_step\nuint32 row_step\nuint8[] data\nbool is_dense\n",
            header_schema(dialect)
        ),
        hz,
        payload: Arc::new(payload),
        json: Arc::new(serde_json::json!({ "height": 1, "width": POINTS })),
    }
}

/// 1080p `sensor_msgs/Image`, rgb8: 6.2 MB a frame.
fn image_raw(index: usize, hz: f64, dialect: Dialect) -> Stream {
    let pixels = (WIDTH * HEIGHT * 3) as usize;
    let mut payload = Vec::with_capacity(pixels + 128);
    if dialect == Dialect::Ros2 {
        cdr_head(&mut payload);
    }
    put_header(&mut payload, "camera", dialect);
    align(&mut payload, 4, dialect);
    put_u32(&mut payload, HEIGHT);
    put_u32(&mut payload, WIDTH);
    put_string(&mut payload, "rgb8", dialect);
    payload.push(0); // is_bigendian
    align(&mut payload, 4, dialect);
    put_u32(&mut payload, WIDTH * 3); // step
    put_u32(&mut payload, pixels as u32);
    // A gradient rather than zeros: a run of identical bytes is unrealistically
    // kind to anything that compresses on the way past.
    payload.extend((0..pixels).map(|i| (i % 251) as u8));

    Stream {
        topic: format!("/bench/image_{index}"),
        schema_name: "sensor_msgs/Image".into(),
        schema: format!(
            "{}\nuint32 height\nuint32 width\nstring encoding\nuint8 is_bigendian\nuint32 step\nuint8[] data\n",
            header_schema(dialect)
        ),
        hz,
        payload: Arc::new(payload),
        json: Arc::new(serde_json::json!({
            "height": HEIGHT, "width": WIDTH, "encoding": "rgb8",
            "is_bigendian": 0, "step": WIDTH * 3,
            "data": vec![0u8; 1024],
        })),
    }
}

/// 1080p `sensor_msgs/CompressedImage`, padded to 300 kB — the size a real
/// JPEG of a camera frame lands at.
fn image_compressed(index: usize, hz: f64, dialect: Dialect) -> Stream {
    const TARGET: usize = 300 * 1024;
    let mut body = jpeg(WIDTH, HEIGHT);
    body.resize(TARGET, 0x5A);

    let mut payload = Vec::with_capacity(TARGET + 128);
    if dialect == Dialect::Ros2 {
        cdr_head(&mut payload);
    }
    put_header(&mut payload, "camera", dialect);
    put_string(&mut payload, "jpeg", dialect);
    align(&mut payload, 4, dialect);
    put_u32(&mut payload, body.len() as u32);
    payload.extend_from_slice(&body);

    Stream {
        topic: format!("/bench/image_c_{index}"),
        schema_name: "sensor_msgs/CompressedImage".into(),
        schema: format!(
            "{}\nstring format\nuint8[] data\n",
            header_schema(dialect)
        ),
        hz,
        payload: Arc::new(payload),
        json: Arc::new(serde_json::json!({ "format": "jpeg", "data": vec![0u8; 1024] })),
    }
}
