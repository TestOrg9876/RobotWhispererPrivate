//! Every bench payload, through the decoders the app actually uses.
//!
//! The first throughput run produced numbers for point clouds and compressed
//! images that were measurements of nothing: the client sat on "waiting for the
//! first message" while the bridge delivered 9 MiB/s at it, because these
//! payloads were encoded wrongly. Catching that took a fifteen-minute run and a
//! screenshot. It takes a second here.

use rw_canonical::{Dialect as CanonDialect, SchemaKind};
use rw_schema_foxglove::parse_concatenated_with_resolver;
use xtask::load_bridge::Dialect;
use xtask::load_shapes;

/// Decode one preset exactly as `rw-transport-foxglove-ws` would.
fn decode(preset: &str, dialect: Dialect) -> Result<rw_canonical::CanonicalValue, String> {
    let streams = load_shapes::build(preset, 1, 10.0, dialect).map_err(|e| e.to_string())?;
    let stream = &streams[0];

    let canon_dialect = match dialect {
        Dialect::Ros1 => CanonDialect::Ros1,
        Dialect::Ros2 => CanonDialect::Ros2,
    };
    let (schema, resolver) = parse_concatenated_with_resolver(
        &stream.schema_name,
        SchemaKind::Message,
        &stream.schema,
        canon_dialect,
    )
    .map_err(|e| format!("schema parse failed: {e}"))?;

    match dialect {
        Dialect::Ros2 => rw_codec_cdr::decode_message(&stream.payload, schema.primary(), &resolver)
            .map_err(|e| format!("cdr decode failed: {e}")),
        Dialect::Ros1 => {
            let resolver: rw_codec_rosmsg::Resolver = resolver;
            rw_codec_rosmsg::decode_message(&stream.payload, schema.primary(), &resolver)
                .map_err(|e| format!("ros1 decode failed: {e}"))
        }
    }
}

/// The field a decoded message must actually carry, so a decode that "succeeds"
/// by stopping early still fails the test.
fn field<'a>(
    value: &'a rw_canonical::CanonicalValue,
    name: &str,
) -> Option<&'a rw_canonical::CanonicalValue> {
    match value {
        rw_canonical::CanonicalValue::Struct(fields) => fields.get(name),
        _ => None,
    }
}

#[test]
fn every_preset_decodes_in_both_dialects() {
    let mut failures = Vec::new();
    for preset in ["chatter", "pointcloud", "image1080", "image1080c"] {
        for dialect in [Dialect::Ros2, Dialect::Ros1] {
            match decode(preset, dialect) {
                Ok(_) => {}
                Err(err) => failures.push(format!("{preset} / {dialect:?}: {err}")),
            }
        }
    }
    assert!(
        failures.is_empty(),
        "presets failed to decode:\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn point_cloud_carries_its_points() {
    for dialect in [Dialect::Ros2, Dialect::Ros1] {
        let value = decode("pointcloud", dialect).unwrap_or_else(|e| panic!("{dialect:?}: {e}"));
        let width = field(&value, "width").unwrap_or_else(|| panic!("{dialect:?}: no width"));
        assert_eq!(
            format!("{width:?}").contains("60000"),
            true,
            "{dialect:?}: width was {width:?}, so the fields array is misaligned"
        );
        let data = field(&value, "data").unwrap_or_else(|| panic!("{dialect:?}: no data"));
        let len = match data {
            rw_canonical::CanonicalValue::Bytes(b) => b.len(),
            rw_canonical::CanonicalValue::Array(a) => a.len(),
            other => panic!("{dialect:?}: data decoded as {other:?}"),
        };
        assert_eq!(len, 60_000 * 16, "{dialect:?}: wrong point payload length");
    }
}

#[test]
fn compressed_image_carries_its_jpeg() {
    for dialect in [Dialect::Ros2, Dialect::Ros1] {
        let value = decode("image1080c", dialect).unwrap_or_else(|e| panic!("{dialect:?}: {e}"));
        let format = field(&value, "format").unwrap_or_else(|| panic!("{dialect:?}: no format"));
        assert!(
            format!("{format:?}").contains("jpeg"),
            "{dialect:?}: format decoded as {format:?}, so the header is the wrong length"
        );
    }
}

/// The rosbridge transport builds its decoder entirely from
/// `rosapi/message_details`, so an empty typedef makes every message decode to
/// an empty struct. That is what "this message has no fields" on the bench run
/// was: a stub, measured as if it were a client.
#[test]
fn rosapi_typedefs_describe_every_field() {
    for preset in ["chatter", "pointcloud", "image1080", "image1080c"] {
        for dialect in [Dialect::Ros2, Dialect::Ros1] {
            let streams = load_shapes::build(preset, 1, 10.0, dialect).unwrap();
            let details = load_shapes::typedefs(&streams[0]);
            let defs = details["typedefs"].as_array().expect("typedefs array");
            assert!(!defs.is_empty(), "{preset}/{dialect:?}: no typedefs at all");

            let root = &defs[0];
            let names = root["fieldnames"].as_array().unwrap();
            let types = root["fieldtypes"].as_array().unwrap();
            assert!(
                !names.is_empty(),
                "{preset}/{dialect:?}: root typedef has no fields, which the app \
                 reports as 'this message has no fields'"
            );
            assert_eq!(
                names.len(),
                types.len(),
                "{preset}/{dialect:?}: field name and type lists disagree"
            );
            assert_eq!(
                root["fieldarraylen"].as_array().unwrap().len(),
                names.len(),
                "{preset}/{dialect:?}: array-length list is the wrong length"
            );

            // Every type the root names must itself be described, or the
            // transport cannot resolve it.
            let described: Vec<&str> = defs.iter().filter_map(|d| d["type"].as_str()).collect();
            for ty in types.iter().filter_map(|t| t.as_str()) {
                let base = ty.trim_end_matches("[]");
                if base.contains('/') {
                    assert!(
                        described.iter().any(|d| *d == base),
                        "{preset}/{dialect:?}: '{base}' is used but never described"
                    );
                }
            }
        }
    }
}
