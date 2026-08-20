//! `visualization_msgs/Marker` and `MarkerArray`, decoded into things to draw.
//!
//! Replaces `rw-core/src/visualization/marker_array.rs`, which read raw CDR
//! bytes and packed them back into a different byte layout for a JavaScript
//! front end that no longer exists. Markers now arrive as a `CanonicalValue`
//! like everything else, so this reads the tree, beside `cloud.rs` and
//! `image.rs`.
//!
//! Every marker in an array carries its own header, so an array is decoded into
//! *several* pieces rather than one — a publisher is entitled to put a
//! `base_link` arrow and a `map` outline in the same message, and drawing both
//! in one frame is how a scene ends up subtly wrong.
//!
//! Markers are drawn as they arrive rather than accumulated by namespace and
//! id. A viewer that keeps every marker it has ever seen needs the whole
//! lifetime, DELETE and DELETEALL protocol to ever forget one, and the topic
//! this is watching republishes its scene each frame — which is what almost
//! every real publisher does.

use std::sync::Arc;

use rw_assets::{math, mesh::Mesh, shapes};
use rw_canonical::CanonicalValue;
use rw_render::{Content, LineSet, MeshVertex, Points, Solid};

use crate::geometry;
use crate::viz::Piece;

/// The marker types, as `visualization_msgs/Marker` numbers them.
mod kind {
    pub const ARROW: i64 = 0;
    pub const CUBE: i64 = 1;
    pub const SPHERE: i64 = 2;
    pub const CYLINDER: i64 = 3;
    pub const LINE_STRIP: i64 = 4;
    pub const LINE_LIST: i64 = 5;
    pub const CUBE_LIST: i64 = 6;
    pub const SPHERE_LIST: i64 = 7;
    pub const POINTS: i64 = 8;
    pub const TRIANGLE_LIST: i64 = 11;
}

/// `action`: 2 is DELETE and 3 is DELETEALL, neither of which draws anything.
const ADD: i64 = 0;
const MODIFY: i64 = 1;

/// How many entries of a cube or sphere list are drawn.
///
/// Each entry is a draw call, and the geometry behind them is uploaded once and
/// instanced — but ten thousand draw calls a frame is a stall, and a list that
/// long is a point cloud wearing a costume.
const LIST_BUDGET: usize = 2_000;

/// Reads a `Marker` or a `MarkerArray`, if this is one.
pub fn decode(value: &CanonicalValue) -> Option<Vec<Piece>> {
    let markers: Vec<&CanonicalValue> = match value.get_path("markers") {
        Some(CanonicalValue::Array(entries)) => entries.iter().collect(),
        Some(_) => return None,
        // A bare `Marker` is one marker, and is told apart from anything else
        // by carrying the fields a marker carries.
        None if value.get_path("type").is_some() && value.get_path("pose").is_some() => {
            vec![value]
        }
        None => return None,
    };

    Some(
        markers
            .into_iter()
            .filter_map(|marker| {
                let action = marker
                    .get_path("action")
                    .and_then(geometry::whole)
                    .unwrap_or(ADD);
                if action != ADD && action != MODIFY {
                    return None;
                }
                Some(Piece {
                    frame: geometry::frame_id(marker),
                    at_ns: geometry::header_stamp_ns(marker),
                    content: content(marker)?,
                })
            })
            .collect(),
    )
}

/// What one marker draws.
fn content(marker: &CanonicalValue) -> Option<Content> {
    let kind = marker.get_path("type").and_then(geometry::whole)?;
    let color = marker
        .get_path("color")
        .and_then(geometry::rgba)
        .unwrap_or([1., 1., 1., 1.]);
    let scale = marker
        .get_path("scale")
        .and_then(geometry::point)
        .unwrap_or([1.; 3]);
    // The marker's own pose inside its frame. Every type is placed by it; the
    // list types put their entries in the pose's coordinates.
    let placement = geometry::pose(marker).unwrap_or_default().to_mat4();
    let points = points_of(marker);

    match kind {
        kind::POINTS => Some(Content::Points(cloud(marker, &points, color, placement))),
        kind::LINE_STRIP | kind::LINE_LIST => Some(Content::Lines(vec![LineSet {
            points: points
                .iter()
                .map(|point| math::transform_point(placement, *point))
                .collect(),
            color,
            strip: kind == kind::LINE_STRIP,
        }])),
        kind::CUBE => Some(primitive(
            shapes::cuboid(scale),
            color,
            placement,
            ("cube", scale),
        )),
        kind::SPHERE => Some(primitive(
            // A sphere marker's scale is its full diameter on each axis, which
            // is why this is a half and not the value as given.
            shapes::sphere(scale[0].abs() / 2.),
            color,
            placement,
            ("sphere", scale),
        )),
        kind::CYLINDER => Some(primitive(
            shapes::cylinder(scale[0].abs() / 2., scale[2].abs()),
            color,
            placement,
            ("cylinder", scale),
        )),
        kind::CUBE_LIST | kind::SPHERE_LIST => {
            let mesh = if kind == kind::CUBE_LIST {
                shapes::cuboid(scale)
            } else {
                shapes::sphere(scale[0].abs() / 2.)
            };
            let name = if kind == kind::CUBE_LIST {
                "cube-list"
            } else {
                "sphere-list"
            };
            let vertices = Arc::new(lit(&mesh, color));
            let key = key(name, scale, color);
            Some(Content::Solids(
                points
                    .iter()
                    .take(LIST_BUDGET)
                    // One colour for the whole list: a per-entry colour would
                    // need a per-entry upload, which is exactly what sharing
                    // the key avoids. A publisher wanting a colour per entry
                    // has POINTS for that.
                    .map(|point| Solid {
                        key,
                        vertices: Arc::clone(&vertices),
                        transform: math::multiply(placement, math::translation(*point)),
                    })
                    .collect(),
            ))
        }
        kind::TRIANGLE_LIST => {
            let vertices = triangles(&points, marker, color);
            (!vertices.is_empty()).then(|| {
                Content::Solids(vec![Solid {
                    // Triangle soup changes every message, so it is keyed by
                    // what it contains rather than by its shape.
                    key: key("triangles", scale, color) ^ (vertices.len() as u64) << 8,
                    vertices: Arc::new(vertices),
                    transform: placement,
                }])
            })
        }
        kind::ARROW => Some(arrow(&points, scale, color, placement)),
        // Text needs a font atlas and a billboard pipeline, and a mesh marker
        // needs the file fetched off the robot. Neither is drawn rather than
        // being drawn wrongly.
        _ => None,
    }
}

/// An arrow, in whichever of its two forms the publisher used.
///
/// With two points it runs from one to the other; otherwise it runs along the
/// marker's own x for `scale.x`. A line rather than a shaft and a head:
/// `rw_assets::shapes` has no cone, and adding a pipeline's worth of geometry
/// for the head would be a new way to do something the line already says.
fn arrow(points: &[[f32; 3]], scale: [f32; 3], color: [f32; 4], placement: math::Mat4) -> Content {
    let (from, to) = match points {
        [from, to, ..] => (*from, *to),
        _ => ([0.; 3], [scale[0], 0., 0.]),
    };
    Content::Lines(vec![LineSet {
        points: vec![
            math::transform_point(placement, from),
            math::transform_point(placement, to),
        ],
        color,
        strip: true,
    }])
}

/// A POINTS marker, with its per-point colours if it brought any.
fn cloud(
    marker: &CanonicalValue,
    points: &[[f32; 3]],
    color: [f32; 4],
    placement: math::Mat4,
) -> Points {
    let channel = |value: f32| (value.clamp(0., 1.) * 255.) as u8;
    let rgb = per_point_colors(marker, points.len()).map(|colors| {
        colors
            .into_iter()
            .map(|color| [channel(color[0]), channel(color[1]), channel(color[2])])
            .collect()
    });
    Points {
        positions: points
            .iter()
            .map(|point| math::transform_point(placement, *point))
            .collect(),
        // Without per-point colours the marker's own colour is the only thing
        // it said, so it is used for every point rather than replaced by a
        // height ramp the publisher never asked for.
        rgb: rgb.or_else(|| {
            Some(vec![
                [
                    channel(color[0]),
                    channel(color[1]),
                    channel(color[2])
                ];
                points.len()
            ])
        }),
        intensity: None,
        coloring: rw_render::Coloring::Rgb,
    }
}

/// A TRIANGLE_LIST, as lit geometry with a normal per face.
fn triangles(points: &[[f32; 3]], marker: &CanonicalValue, color: [f32; 4]) -> Vec<MeshVertex> {
    let colors = per_point_colors(marker, points.len());
    let mut vertices = Vec::with_capacity(points.len());
    for (face, corners) in points.chunks_exact(3).enumerate() {
        // The message carries no normals, so each face gets its own — which is
        // also what makes the edges read as edges.
        let normal = face_normal(corners);
        for (corner, point) in corners.iter().enumerate() {
            let color = colors
                .as_ref()
                .and_then(|colors| colors.get(face * 3 + corner).copied())
                .unwrap_or(color);
            vertices.push(MeshVertex::new(*point, normal, color));
        }
    }
    vertices
}

fn face_normal(corners: &[[f32; 3]]) -> [f32; 3] {
    let edge = |a: [f32; 3], b: [f32; 3]| [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let (u, v) = (edge(corners[0], corners[1]), edge(corners[0], corners[2]));
    let normal = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
    if length <= f32::EPSILON {
        // A degenerate triangle has no facing. Up is as good an answer as any,
        // and beats a field of NaNs in the vertex buffer.
        return [0., 0., 1.];
    }
    [normal[0] / length, normal[1] / length, normal[2] / length]
}

/// One primitive shape, placed by the marker's pose.
fn primitive(
    mesh: Mesh,
    color: [f32; 4],
    placement: math::Mat4,
    identity: (&str, [f32; 3]),
) -> Content {
    Content::Solids(vec![Solid {
        key: key(identity.0, identity.1, color),
        vertices: Arc::new(lit(&mesh, color)),
        transform: placement,
    }])
}

/// Flattens a mesh into the renderer's vertex format.
fn lit(mesh: &Mesh, color: [f32; 4]) -> Vec<MeshVertex> {
    let mut vertices = Vec::new();
    for part in &mesh.parts {
        for index in &part.indices {
            let index = *index as usize;
            let (Some(position), Some(normal)) =
                (part.positions.get(index), part.normals.get(index))
            else {
                continue;
            };
            vertices.push(MeshVertex::new(*position, *normal, color));
        }
    }
    vertices
}

/// A cache key for a primitive, from what makes its geometry.
///
/// Two markers of the same shape, scale and colour share one upload, which is
/// what turns a two-thousand-entry cube list into one buffer and two thousand
/// matrices. The top bit is set so these can never collide with the robot
/// pane's generation-based keys.
fn key(name: &str, scale: [f32; 3], color: [f32; 4]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    eat(name.as_bytes());
    for value in scale {
        eat(&value.to_le_bytes());
    }
    for value in color {
        eat(&value.to_le_bytes());
    }
    hash | 1 << 63
}

/// The marker's `points` array.
fn points_of(marker: &CanonicalValue) -> Vec<[f32; 3]> {
    match marker.get_path("points") {
        Some(CanonicalValue::Array(entries)) => {
            entries.iter().filter_map(geometry::point).collect()
        }
        _ => Vec::new(),
    }
}

/// The marker's `colors` array, when it has one per point.
///
/// A partly filled `colors` is a publisher bug, and pairing the ones that did
/// arrive with the wrong points would be worse than using the marker's own
/// colour for all of them.
fn per_point_colors(marker: &CanonicalValue, wanted: usize) -> Option<Vec<[f32; 4]>> {
    let CanonicalValue::Array(entries) = marker.get_path("colors")? else {
        return None;
    };
    (entries.len() == wanted && wanted > 0)
        .then(|| {
            entries
                .iter()
                .filter_map(geometry::rgba)
                .collect::<Vec<_>>()
        })
        .filter(|colors| colors.len() == wanted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn map<const N: usize>(fields: [(&str, CanonicalValue); N]) -> CanonicalValue {
        CanonicalValue::Struct(
            fields
                .into_iter()
                .map(|(name, value)| (name.to_string(), value))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    fn xyz(x: f64, y: f64, z: f64) -> CanonicalValue {
        map([
            ("x", CanonicalValue::F64(x)),
            ("y", CanonicalValue::F64(y)),
            ("z", CanonicalValue::F64(z)),
        ])
    }

    fn color(r: f64, g: f64, b: f64, a: f64) -> CanonicalValue {
        map([
            ("r", CanonicalValue::F64(r)),
            ("g", CanonicalValue::F64(g)),
            ("b", CanonicalValue::F64(b)),
            ("a", CanonicalValue::F64(a)),
        ])
    }

    fn identity_pose() -> CanonicalValue {
        map([
            ("position", xyz(0., 0., 0.)),
            (
                "orientation",
                map([
                    ("x", CanonicalValue::F64(0.)),
                    ("y", CanonicalValue::F64(0.)),
                    ("z", CanonicalValue::F64(0.)),
                    ("w", CanonicalValue::F64(1.)),
                ]),
            ),
        ])
    }

    fn marker(kind: i64, extra: Vec<(&str, CanonicalValue)>) -> CanonicalValue {
        let mut fields: BTreeMap<String, CanonicalValue> = BTreeMap::new();
        fields.insert("type".into(), CanonicalValue::Int(kind));
        fields.insert("action".into(), CanonicalValue::Int(ADD));
        fields.insert("pose".into(), identity_pose());
        fields.insert("scale".into(), xyz(1., 1., 1.));
        fields.insert("color".into(), color(1., 0., 0., 1.));
        fields.insert(
            "header".into(),
            map([("frame_id", CanonicalValue::String("map".into()))]),
        );
        for (name, value) in extra {
            fields.insert(name.into(), value);
        }
        CanonicalValue::Struct(fields)
    }

    fn array(markers: Vec<CanonicalValue>) -> CanonicalValue {
        map([("markers", CanonicalValue::Array(markers))])
    }

    fn points(values: &[[f64; 3]]) -> CanonicalValue {
        CanonicalValue::Array(values.iter().map(|p| xyz(p[0], p[1], p[2])).collect())
    }

    #[test]
    fn a_points_marker_becomes_a_cloud_in_the_colour_it_named() {
        let decoded = decode(&array(vec![marker(
            kind::POINTS,
            vec![("points", points(&[[1., 0., 0.], [0., 1., 0.]]))],
        )]))
        .expect("decodes");
        assert_eq!(decoded.len(), 1);
        let Content::Points(cloud) = &decoded[0].content else {
            panic!("expected points, got {:?}", decoded[0].content);
        };
        assert_eq!(cloud.positions, vec![[1., 0., 0.], [0., 1., 0.]]);
        assert_eq!(cloud.rgb, Some(vec![[255, 0, 0], [255, 0, 0]]));
    }

    #[test]
    fn per_point_colours_are_used_when_there_is_one_for_every_point() {
        let decoded = decode(&array(vec![marker(
            kind::POINTS,
            vec![
                ("points", points(&[[0., 0., 0.], [1., 0., 0.]])),
                (
                    "colors",
                    CanonicalValue::Array(vec![color(1., 0., 0., 1.), color(0., 0., 1., 1.)]),
                ),
            ],
        )]))
        .expect("decodes");
        let Content::Points(cloud) = &decoded[0].content else {
            panic!("expected points")
        };
        assert_eq!(cloud.rgb, Some(vec![[255, 0, 0], [0, 0, 255]]));
    }

    #[test]
    fn a_half_filled_colours_array_is_dropped_rather_than_mispaired() {
        let decoded = decode(&array(vec![marker(
            kind::POINTS,
            vec![
                (
                    "points",
                    points(&[[0., 0., 0.], [1., 0., 0.], [2., 0., 0.]]),
                ),
                ("colors", CanonicalValue::Array(vec![color(0., 0., 1., 1.)])),
            ],
        )]))
        .expect("decodes");
        let Content::Points(cloud) = &decoded[0].content else {
            panic!("expected points")
        };
        assert_eq!(
            cloud.rgb,
            Some(vec![[255, 0, 0]; 3]),
            "the marker's own colour, not one blue point and two guesses"
        );
    }

    #[test]
    fn line_strips_and_line_lists_keep_their_difference() {
        for (kind, strip) in [(kind::LINE_STRIP, true), (kind::LINE_LIST, false)] {
            let decoded = decode(&array(vec![marker(
                kind,
                vec![("points", points(&[[0., 0., 0.], [1., 0., 0.]]))],
            )]))
            .expect("decodes");
            let Content::Lines(sets) = &decoded[0].content else {
                panic!("expected lines for type {kind}")
            };
            assert_eq!(sets[0].strip, strip, "type {kind}");
            assert_eq!(sets[0].points.len(), 2);
        }
    }

    #[test]
    fn a_cube_becomes_a_solid_of_the_size_it_asked_for() {
        let decoded = decode(&array(vec![marker(
            kind::CUBE,
            vec![("scale", xyz(2., 4., 6.))],
        )]))
        .expect("decodes");
        let Content::Solids(solids) = &decoded[0].content else {
            panic!("expected solids")
        };
        assert_eq!(solids.len(), 1);
        let extent = solids[0]
            .vertices
            .iter()
            .fold([0f32; 3], |mut widest, vertex| {
                for (axis, extent) in widest.iter_mut().enumerate() {
                    *extent = extent.max(vertex.position[axis].abs() * 2.);
                }
                widest
            });
        assert_eq!(extent, [2., 4., 6.]);
    }

    #[test]
    fn a_cube_list_shares_one_upload_across_every_entry() {
        // The whole reason the cache is keyed the way it is: a thousand cubes
        // must be one buffer and a thousand matrices.
        let decoded = decode(&array(vec![marker(
            kind::CUBE_LIST,
            vec![(
                "points",
                points(&[[0., 0., 0.], [1., 0., 0.], [2., 0., 0.]]),
            )],
        )]))
        .expect("decodes");
        let Content::Solids(solids) = &decoded[0].content else {
            panic!("expected solids")
        };
        assert_eq!(solids.len(), 3);
        assert!(
            solids.iter().all(|solid| solid.key == solids[0].key),
            "the entries must share a cache key"
        );
        let places: Vec<[f32; 3]> = solids
            .iter()
            .map(|solid| rw_render::transform_point(solid.transform, [0.; 3]))
            .collect();
        assert_eq!(places, vec![[0., 0., 0.], [1., 0., 0.], [2., 0., 0.]]);
    }

    #[test]
    fn an_enormous_list_is_capped_rather_than_drawn_a_draw_call_at_a_time() {
        let many: Vec<[f64; 3]> = (0..LIST_BUDGET + 500)
            .map(|index| [index as f64, 0., 0.])
            .collect();
        let decoded = decode(&array(vec![marker(
            kind::CUBE_LIST,
            vec![("points", points(&many))],
        )]))
        .expect("decodes");
        let Content::Solids(solids) = &decoded[0].content else {
            panic!("expected solids")
        };
        assert_eq!(solids.len(), LIST_BUDGET);
    }

    #[test]
    fn a_triangle_list_gets_a_normal_per_face() {
        let decoded = decode(&array(vec![marker(
            kind::TRIANGLE_LIST,
            vec![(
                "points",
                points(&[[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]]),
            )],
        )]))
        .expect("decodes");
        let Content::Solids(solids) = &decoded[0].content else {
            panic!("expected solids")
        };
        assert_eq!(solids[0].vertices.len(), 3);
        for vertex in solids[0].vertices.iter() {
            assert_eq!(vertex.normal, [0., 0., 1.], "a flat triangle faces up");
        }
    }

    #[test]
    fn a_degenerate_triangle_gets_a_facing_rather_than_a_field_of_nans() {
        let decoded = decode(&array(vec![marker(
            kind::TRIANGLE_LIST,
            vec![(
                "points",
                points(&[[0., 0., 0.], [0., 0., 0.], [0., 0., 0.]]),
            )],
        )]))
        .expect("decodes");
        let Content::Solids(solids) = &decoded[0].content else {
            panic!("expected solids")
        };
        assert!(
            solids[0]
                .vertices
                .iter()
                .all(|vertex| vertex.normal.iter().all(|value| value.is_finite()))
        );
    }

    #[test]
    fn each_marker_keeps_the_frame_its_own_header_named() {
        // The reason an array decodes to several pieces: a publisher may put a
        // robot-relative arrow and a world-relative outline in one message.
        let mut in_base = marker(
            kind::LINE_STRIP,
            vec![("points", points(&[[0.; 3], [1., 0., 0.]]))],
        );
        let CanonicalValue::Struct(fields) = &mut in_base else {
            unreachable!()
        };
        fields.insert(
            "header".into(),
            map([("frame_id", CanonicalValue::String("base_link".into()))]),
        );
        let decoded = decode(&array(vec![
            marker(
                kind::LINE_STRIP,
                vec![("points", points(&[[0.; 3], [1., 0., 0.]]))],
            ),
            in_base,
        ]))
        .expect("decodes");
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].frame.as_deref(), Some("map"));
        assert_eq!(decoded[1].frame.as_deref(), Some("base_link"));
    }

    #[test]
    fn a_deleted_marker_draws_nothing() {
        for action in [2i64, 3] {
            let decoded = decode(&array(vec![marker(
                kind::CUBE,
                vec![("action", CanonicalValue::Int(action))],
            )]))
            .expect("decodes");
            assert!(decoded.is_empty(), "action {action} still drew something");
        }
    }

    #[test]
    fn text_and_mesh_markers_are_skipped_rather_than_drawn_wrongly() {
        for kind in [9i64, 10] {
            let decoded = decode(&array(vec![marker(
                kind,
                vec![("text", CanonicalValue::String("hello".into()))],
            )]))
            .expect("decodes");
            assert!(decoded.is_empty(), "type {kind} drew something");
        }
    }

    #[test]
    fn a_marker_is_placed_by_its_own_pose_inside_its_frame() {
        let mut shifted = marker(
            kind::LINE_STRIP,
            vec![("points", points(&[[0.; 3], [1., 0., 0.]]))],
        );
        let CanonicalValue::Struct(fields) = &mut shifted else {
            unreachable!()
        };
        fields.insert(
            "pose".into(),
            map([
                ("position", xyz(10., 0., 0.)),
                (
                    "orientation",
                    map([
                        ("x", CanonicalValue::F64(0.)),
                        ("y", CanonicalValue::F64(0.)),
                        ("z", CanonicalValue::F64(0.)),
                        ("w", CanonicalValue::F64(1.)),
                    ]),
                ),
            ]),
        );
        let decoded = decode(&array(vec![shifted])).expect("decodes");
        let Content::Lines(sets) = &decoded[0].content else {
            panic!("expected lines")
        };
        assert_eq!(sets[0].points, vec![[10., 0., 0.], [11., 0., 0.]]);
    }

    #[test]
    fn a_bare_marker_decodes_as_well_as_an_array_of_them() {
        let decoded = decode(&marker(
            kind::LINE_STRIP,
            vec![("points", points(&[[0.; 3], [1., 0., 0.]]))],
        ))
        .expect("decodes");
        assert_eq!(decoded.len(), 1);
    }

    #[test]
    fn a_message_that_is_not_a_marker_is_refused() {
        assert_eq!(decode(&map([("data", CanonicalValue::Int(1))])), None);
        assert_eq!(
            decode(&map([("markers", CanonicalValue::Int(1))])),
            None,
            "a `markers` field that is not an array is not a marker array"
        );
    }

    #[test]
    fn an_arrow_with_two_points_runs_between_them() {
        let decoded = decode(&array(vec![marker(
            kind::ARROW,
            vec![("points", points(&[[0., 0., 0.], [0., 3., 0.]]))],
        )]))
        .expect("decodes");
        let Content::Lines(sets) = &decoded[0].content else {
            panic!("expected lines")
        };
        assert_eq!(sets[0].points, vec![[0., 0., 0.], [0., 3., 0.]]);
    }

    #[test]
    fn an_arrow_without_points_runs_along_its_own_x_for_its_scale() {
        let decoded = decode(&array(vec![marker(
            kind::ARROW,
            vec![("scale", xyz(2.5, 0.1, 0.1))],
        )]))
        .expect("decodes");
        let Content::Lines(sets) = &decoded[0].content else {
            panic!("expected lines")
        };
        assert_eq!(sets[0].points, vec![[0., 0., 0.], [2.5, 0., 0.]]);
    }
}
