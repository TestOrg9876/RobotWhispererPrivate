//! What a message is, and what can honestly be done with it.
//!
//! `rw_canonical::viz_role_for_schema` has classified every schema since the
//! rewrite and nothing read it: `views::visualize` sniffed instead — try to
//! decode an image, then try to decode a cloud, then give up and show a field
//! table. Sniffing is why a `LaserScan` looked like a wall of numbers and a
//! `Path` looked like nothing at all: neither is an image and neither is a
//! `PointCloud2`, so both fell through to the table.
//!
//! This is the registry that was never rebuilt. A schema name gives a role, a
//! role gives the views a topic can offer and the geometry it decodes into, and
//! anything unrecognised falls back to the field table — which is a real answer
//! and not a failure.

use rw_canonical::{CanonicalValue, VisualizationRole};
use rw_render::{Content, Layer};

use crate::tf::Tree;
use crate::{cloud, geometry, image, marker};

/// The colour a message's own geometry is drawn in when it did not name one.
///
/// Paths and poses carry no colour of their own, and RViz's answer — pick one
/// and let the user change it — needs a settings tree to hold the choice. One
/// legible colour is the better trade at this size.
const TRAIL: [f32; 4] = [0.36, 0.72, 0.98, 1.];

/// What "Visualize" means for a particular message.
///
/// Deliberately three and not fifteen: the role says what the message *is*, and
/// this says which of the three ways of looking at one applies. A pane offering
/// a choice between "PoseStamped view" and "Odometry view" would be a menu
/// describing the schema registry rather than the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visual {
    /// A picture.
    Picture,
    /// Something with a place in the world.
    World,
    /// The flat table of leaf paths and values.
    Fields,
}

impl Visual {
    pub fn label(self) -> &'static str {
        match self {
            Self::Picture => "Image",
            Self::World => "3D",
            Self::Fields => "Fields",
        }
    }
}

/// Which of the three a message of this role gets.
///
/// Every role maps to one; nothing falls through to a guess.
pub fn visual_for(role: &VisualizationRole) -> Visual {
    match role {
        VisualizationRole::Image | VisualizationRole::CompressedImage => Visual::Picture,
        VisualizationRole::PointCloud2
        | VisualizationRole::LaserScan
        | VisualizationRole::Marker
        | VisualizationRole::MarkerArray
        | VisualizationRole::Path
        | VisualizationRole::Pose
        | VisualizationRole::PoseStamped
        | VisualizationRole::Odometry
        | VisualizationRole::Tf => Visual::World,
        VisualizationRole::Plot { .. } | VisualizationRole::Text | VisualizationRole::JsonTree => {
            Visual::Fields
        }
    }
}

/// The role a topic's schema has, by name.
pub fn role_for(schema: &str) -> VisualizationRole {
    rw_canonical::viz_role_for_schema(schema)
}

/// Whether a topic of this schema can be put in a world pane at all.
pub fn is_drawable(schema: &str) -> bool {
    visual_for(&role_for(schema)) == Visual::World
}

/// One decoded thing, and where it belongs.
///
/// A message can decode to several: a `MarkerArray` may carry markers in
/// different frames, and each has to be placed by its own. The world pane makes
/// one layer per piece.
#[derive(Debug, Clone, PartialEq)]
pub struct Piece {
    /// The frame the geometry is expressed in, from `header.frame_id`.
    ///
    /// `None` when the message carries no header at all — then there is nothing
    /// to look up, and the pane draws it in the fixed frame and says so rather
    /// than pretending it knows.
    pub frame: Option<String>,
    /// The message's own stamp, for the transform lookup. `None` asks the
    /// buffer for its newest, which is what a live view wants.
    pub at_ns: Option<u64>,
    pub content: Content,
}

/// Decodes a message into the pieces a world pane can draw.
///
/// `None` when this role has no geometry in it at all; an empty list when it
/// does but this particular message was empty — a `/tf` with nothing in it, a
/// scan where every beam missed.
pub fn draw(role: &VisualizationRole, value: &CanonicalValue) -> Option<Vec<Piece>> {
    let simple = |content: Content| {
        vec![Piece {
            frame: geometry::frame_id(value),
            at_ns: geometry::header_stamp_ns(value),
            content,
        }]
    };

    match role {
        VisualizationRole::PointCloud2 => {
            Some(simple(Content::Points(cloud::decode(value)?.into())))
        }
        VisualizationRole::LaserScan => Some(simple(Content::Points(geometry::scan(value)?))),
        VisualizationRole::Marker | VisualizationRole::MarkerArray => marker::decode(value),
        VisualizationRole::Path => {
            Some(simple(Content::Lines(vec![geometry::trail(value, TRAIL)?])))
        }
        VisualizationRole::Pose | VisualizationRole::PoseStamped | VisualizationRole::Odometry => {
            Some(simple(Content::Axes(geometry::axes(value)?)))
        }
        // A `/tf` message drawn as geometry is every frame it mentions, as a
        // triad. The tree itself is fed from the store rather than from here —
        // this is the view of it, not the source.
        VisualizationRole::Tf => Some(tf_triads(value)),
        _ => None,
    }
}

/// Every frame a `TFMessage` mentions, as a triad in its own parent's frame.
fn tf_triads(value: &CanonicalValue) -> Vec<Piece> {
    let Some(entries) = crate::tf::decode(value) else {
        return Vec::new();
    };
    entries
        .into_iter()
        .map(|entry| Piece {
            frame: Some(entry.parent),
            at_ns: Some(entry.at_ns).filter(|at| *at > 0),
            content: Content::Axes(vec![rw_render::Axis {
                transform: entry.transform.to_mat4(),
                length: geometry::AXIS_LENGTH,
            }]),
        })
        .collect()
}

/// Whether a message really is a picture, for the picture view.
pub fn picture(value: &CanonicalValue) -> Option<image::Frame> {
    image::decode(value)
}

/// A piece resolved into a pane's fixed frame — or the reason it could not be.
#[derive(Debug, Clone)]
pub struct Placed {
    pub layer: Layer,
    /// The frame the piece came in, for the layer's row.
    pub frame: Option<String>,
    /// Why the layer is not drawn. `None` when it is.
    pub problem: Option<String>,
}

/// Resolves decoded pieces into `fixed`, using a connection's transform tree.
///
/// The one place this happens, so a single-topic pane and the world pane cannot
/// disagree about where something is. A piece that will not resolve comes back
/// hidden and carrying the reason rather than placed at the origin — a layer
/// silently drawn in the wrong place is the failure mode this whole crate
/// exists to prevent, and it is worse than a layer that is missing, because
/// nothing about it looks wrong.
pub fn place(pieces: Vec<Piece>, fixed: &str, tree: Option<&Tree>) -> Vec<Placed> {
    pieces
        .into_iter()
        .map(|piece| {
            let mut layer = Layer::new(piece.content);
            let Some(frame) = piece.frame.clone() else {
                // The message named no frame at all. There is nothing to look
                // up, so it is drawn as it arrived — which is what every viewer
                // does with an unstamped message, and it is not an error.
                return Placed {
                    layer,
                    frame: None,
                    problem: None,
                };
            };
            if frame == fixed {
                return Placed {
                    layer,
                    frame: Some(frame),
                    problem: None,
                };
            }
            let Some(tree) = tree else {
                layer.visible = false;
                return Placed {
                    layer,
                    frame: Some(frame.clone()),
                    problem: Some(format!(
                        "`{frame}` cannot be placed in `{fixed}`: this system has \
                         published no transforms yet"
                    )),
                };
            };
            match tree.lookup(fixed, &frame, piece.at_ns.unwrap_or(rw_tf::LATEST)) {
                Ok(transform) => {
                    layer.transform = transform.to_mat4();
                    Placed {
                        layer,
                        frame: Some(frame),
                        problem: None,
                    }
                }
                Err(error) => {
                    layer.visible = false;
                    Placed {
                        layer,
                        frame: Some(frame),
                        problem: Some(error.to_string()),
                    }
                }
            }
        })
        .collect()
}

/// Everything a single-topic 3D view does with a new message.
///
/// The message's own frame is the fixed frame, so the ordinary case — one
/// topic, one frame — needs no transform tree at all, while a marker array
/// spread across several frames still resolves against one. One function
/// because a dashboard pane and a request editor showing the same topic must
/// draw the same picture.
pub fn layers_for(
    role: &VisualizationRole,
    value: &CanonicalValue,
    tree: Option<&Tree>,
) -> Option<Vec<Layer>> {
    let pieces = draw(role, value)?;
    let fixed = pieces.iter().find_map(|piece| piece.frame.clone());
    Some(
        place(pieces, fixed.as_deref().unwrap_or_default(), tree)
            .into_iter()
            .map(|placed| placed.layer)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Every role the schema registry can produce.
    fn every_role() -> Vec<VisualizationRole> {
        vec![
            VisualizationRole::Image,
            VisualizationRole::CompressedImage,
            VisualizationRole::MarkerArray,
            VisualizationRole::Marker,
            VisualizationRole::PointCloud2,
            VisualizationRole::LaserScan,
            VisualizationRole::Pose,
            VisualizationRole::PoseStamped,
            VisualizationRole::Path,
            VisualizationRole::Odometry,
            VisualizationRole::Tf,
            VisualizationRole::Plot {
                field_path: "data".into(),
            },
            VisualizationRole::Text,
            VisualizationRole::JsonTree,
        ]
    }

    #[test]
    fn every_role_maps_to_a_view() {
        // The registry's whole promise: nothing falls through to a guess.
        for role in every_role() {
            let visual = visual_for(&role);
            assert!(
                !visual.label().is_empty(),
                "{role:?} produced no view at all"
            );
        }
    }

    #[test]
    fn an_unknown_schema_falls_back_to_the_field_table() {
        assert_eq!(
            visual_for(&role_for("some_vendor/PrivateThing")),
            Visual::Fields
        );
        assert_eq!(visual_for(&role_for("")), Visual::Fields);
    }

    #[test]
    fn the_roles_that_go_in_a_world_pane_are_the_ones_with_geometry_in_them() {
        for schema in [
            "sensor_msgs/PointCloud2",
            "sensor_msgs/LaserScan",
            "sensor_msgs/msg/LaserScan",
            "visualization_msgs/MarkerArray",
            "nav_msgs/Path",
            "nav_msgs/Odometry",
            "geometry_msgs/PoseStamped",
            "tf2_msgs/TFMessage",
        ] {
            assert!(is_drawable(schema), "{schema} should be drawable");
        }
        for schema in [
            "sensor_msgs/Image",
            "std_msgs/String",
            "std_msgs/Float64",
            "some_vendor/PrivateThing",
        ] {
            assert!(!is_drawable(schema), "{schema} should not be drawable");
        }
    }

    #[test]
    fn a_picture_is_not_offered_a_world_and_a_world_is_not_offered_a_picture() {
        assert_eq!(visual_for(&role_for("sensor_msgs/Image")), Visual::Picture);
        assert_eq!(
            visual_for(&role_for("sensor_msgs/CompressedImage")),
            Visual::Picture
        );
        assert_eq!(
            visual_for(&role_for("sensor_msgs/PointCloud2")),
            Visual::World
        );
    }

    #[test]
    fn a_number_is_a_field_table_rather_than_a_failed_3d_view() {
        // std_msgs/Float64 has a Plot role, and the point of the fallback is
        // that "there is nothing to draw" is a real answer.
        assert_eq!(visual_for(&role_for("std_msgs/Float64")), Visual::Fields);
        assert_eq!(visual_for(&role_for("std_msgs/String")), Visual::Fields);
    }

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

    fn header(frame: &str) -> CanonicalValue {
        map([
            ("frame_id", CanonicalValue::String(frame.into())),
            (
                "stamp",
                CanonicalValue::Time {
                    sec: 4,
                    nanosec: 500_000_000,
                },
            ),
        ])
    }

    #[test]
    fn a_scan_decodes_into_points_in_the_frame_its_header_named() {
        let message = map([
            ("header", header("laser")),
            ("angle_min", CanonicalValue::F32(0.)),
            ("angle_increment", CanonicalValue::F32(1.)),
            ("range_min", CanonicalValue::F32(0.)),
            ("range_max", CanonicalValue::F32(10.)),
            (
                "ranges",
                CanonicalValue::Array(vec![CanonicalValue::F32(1.)]),
            ),
        ]);
        let pieces = draw(&VisualizationRole::LaserScan, &message).expect("decodes");
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].frame.as_deref(), Some("laser"));
        assert_eq!(pieces[0].at_ns, Some(4_500_000_000));
        assert!(matches!(pieces[0].content, Content::Points(_)));
    }

    #[test]
    fn a_path_decodes_into_a_line_in_the_frame_its_header_named() {
        let step = |x: f64| {
            map([(
                "pose",
                map([
                    ("position", xyz(x, 0., 0.)),
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
            )])
        };
        let message = map([
            ("header", header("map")),
            ("poses", CanonicalValue::Array(vec![step(0.), step(1.)])),
        ]);
        let pieces = draw(&VisualizationRole::Path, &message).expect("decodes");
        assert_eq!(pieces[0].frame.as_deref(), Some("map"));
        let Content::Lines(sets) = &pieces[0].content else {
            panic!("expected lines")
        };
        assert!(sets[0].strip);
        assert_eq!(sets[0].points.len(), 2);
    }

    #[test]
    fn a_tf_message_draws_every_frame_it_mentions_in_that_frames_parent() {
        let entry = |parent: &str, child: &str| {
            map([
                ("header", header(parent)),
                ("child_frame_id", CanonicalValue::String(child.into())),
                (
                    "transform",
                    map([
                        ("translation", xyz(1., 0., 0.)),
                        (
                            "rotation",
                            map([
                                ("x", CanonicalValue::F64(0.)),
                                ("y", CanonicalValue::F64(0.)),
                                ("z", CanonicalValue::F64(0.)),
                                ("w", CanonicalValue::F64(1.)),
                            ]),
                        ),
                    ]),
                ),
            ])
        };
        let message = map([(
            "transforms",
            CanonicalValue::Array(vec![entry("map", "odom"), entry("odom", "base")]),
        )]);
        let pieces = draw(&VisualizationRole::Tf, &message).expect("decodes");
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0].frame.as_deref(), Some("map"));
        assert_eq!(pieces[1].frame.as_deref(), Some("odom"));
        for piece in &pieces {
            let Content::Axes(axes) = &piece.content else {
                panic!("expected axes")
            };
            assert_eq!(
                rw_render::transform_point(axes[0].transform, [0.; 3]),
                [1., 0., 0.]
            );
        }
    }

    #[test]
    fn a_role_with_no_geometry_in_it_draws_nothing_at_all() {
        let message = map([("data", CanonicalValue::F64(1.))]);
        assert_eq!(draw(&VisualizationRole::JsonTree, &message), None);
        assert_eq!(draw(&VisualizationRole::Text, &message), None);
        assert_eq!(draw(&VisualizationRole::Image, &message), None);
    }

    #[test]
    fn a_message_that_claims_a_role_it_cannot_meet_is_refused_rather_than_drawn_empty() {
        let nonsense = map([("data", CanonicalValue::Int(1))]);
        assert_eq!(draw(&VisualizationRole::LaserScan, &nonsense), None);
        assert_eq!(draw(&VisualizationRole::Path, &nonsense), None);
        assert_eq!(draw(&VisualizationRole::PointCloud2, &nonsense), None);
    }

    #[test]
    fn a_message_with_no_header_has_no_frame_rather_than_a_guessed_one() {
        let message = map([
            ("angle_min", CanonicalValue::F32(0.)),
            ("angle_increment", CanonicalValue::F32(1.)),
            (
                "ranges",
                CanonicalValue::Array(vec![CanonicalValue::F32(1.)]),
            ),
        ]);
        let pieces = draw(&VisualizationRole::LaserScan, &message).expect("decodes");
        assert_eq!(pieces[0].frame, None);
        assert_eq!(pieces[0].at_ns, None);
    }
}
