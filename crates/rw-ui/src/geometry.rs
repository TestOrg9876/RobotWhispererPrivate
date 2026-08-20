//! The geometry messages every 3D view is built out of.
//!
//! `geometry_msgs` is the vocabulary the rest of ROS is written in: a header
//! saying which frame and when, a point, a quaternion, a pose. Every message
//! this app draws is made of those four things, so they are read in one place
//! and read the same way — `cloud.rs`, `tf.rs`, `marker.rs` and the world pane
//! all come through here rather than each growing its own `x`-`y`-`z` reader.
//!
//! Pure and tested; nothing here touches the GPU or GPUI.

use rw_canonical::CanonicalValue;
use rw_render::{Axis, Coloring, LineSet, Points};
use rw_tf::{Quat, Transform};

/// How long a pose's axis arms are drawn, in metres.
///
/// Half a metre reads at the scale a mobile robot lives at without swamping it.
/// Fixed rather than a setting: a number nobody would think to change is a
/// setting that only costs a row of a menu.
pub const AXIS_LENGTH: f32 = 0.5;

/// `header.frame_id`: which frame the rest of the message is expressed in.
///
/// The single most important field in a robotics message, and the one whose
/// absence means a message cannot honestly be drawn anywhere.
pub fn frame_id(value: &CanonicalValue) -> Option<String> {
    let frame = text(value.get_path("header.frame_id")?)?;
    // A leading slash is ROS 1's spelling of the same frame. tf2 strips it, so
    // `/base_link` and `base_link` have to resolve to one frame here too.
    let trimmed = frame.trim_start_matches('/');
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// `header.stamp`, in nanoseconds.
///
/// The canonical model has a `Time` variant, but a header decoded from JSON by
/// a bridge arrives as an ordinary struct — and ROS 1 spells the fields
/// `secs`/`nsecs` while ROS 2 spells them `sec`/`nanosec`. All three are the
/// same instant, so all three are read.
pub fn stamp_ns(value: &CanonicalValue) -> Option<u64> {
    let (sec, nanosec) = match value {
        CanonicalValue::Time { sec, nanosec } => (i64::from(*sec), u64::from(*nanosec)),
        CanonicalValue::Struct(_) => {
            let sec = value
                .get_path("sec")
                .or_else(|| value.get_path("secs"))
                .and_then(whole)?;
            let nanosec = value
                .get_path("nanosec")
                .or_else(|| value.get_path("nsecs"))
                .and_then(whole)
                .unwrap_or(0);
            (sec, u64::try_from(nanosec).ok()?)
        }
        _ => return None,
    };
    // A negative stamp is before the epoch, which no robot's clock means; it is
    // refused rather than wrapped round into the far future.
    u64::try_from(sec)
        .ok()?
        .checked_mul(1_000_000_000)?
        .checked_add(nanosec)
}

/// The stamp on a message's header, if it has one.
pub fn header_stamp_ns(value: &CanonicalValue) -> Option<u64> {
    stamp_ns(value.get_path("header.stamp")?)
}

pub fn text(value: &CanonicalValue) -> Option<String> {
    match value {
        CanonicalValue::String(inner) => Some(inner.clone()),
        _ => None,
    }
}

/// A number, however the codec spelled it.
///
/// Non-finite values are refused rather than passed on: a NaN coordinate draws
/// nothing but takes the whole frame it belongs to with it.
pub fn real(value: &CanonicalValue) -> Option<f32> {
    let number = match value {
        CanonicalValue::F64(inner) => *inner as f32,
        CanonicalValue::F32(inner) => *inner,
        CanonicalValue::Int(inner) => *inner as f32,
        CanonicalValue::Uint(inner) => *inner as f32,
        _ => return None,
    };
    number.is_finite().then_some(number)
}

pub fn whole(value: &CanonicalValue) -> Option<i64> {
    match value {
        CanonicalValue::Int(inner) => Some(*inner),
        CanonicalValue::Uint(inner) => i64::try_from(*inner).ok(),
        CanonicalValue::F64(inner) => Some(*inner as i64),
        CanonicalValue::F32(inner) => Some(*inner as i64),
        _ => None,
    }
}

/// A `geometry_msgs/Point` or `Vector3` — the same three fields either way.
pub fn point(value: &CanonicalValue) -> Option<[f32; 3]> {
    Some([
        real(value.get_path("x")?)?,
        real(value.get_path("y")?)?,
        real(value.get_path("z")?)?,
    ])
}

/// A `geometry_msgs/Quaternion`.
pub fn quaternion(value: &CanonicalValue) -> Option<Quat> {
    Some(Quat::from_wire(
        real(value.get_path("x")?)?,
        real(value.get_path("y")?)?,
        real(value.get_path("z")?)?,
        real(value.get_path("w")?)?,
    ))
}

/// A rigid placement, whichever of its two spellings arrived.
///
/// `geometry_msgs/Transform` calls them `translation` and `rotation`;
/// `geometry_msgs/Pose` calls the same two things `position` and
/// `orientation`. They are the same six numbers and there is no reason for two
/// readers.
pub fn rigid(value: &CanonicalValue) -> Option<Transform> {
    let translation = value
        .get_path("translation")
        .or_else(|| value.get_path("position"))?;
    let rotation = value
        .get_path("rotation")
        .or_else(|| value.get_path("orientation"))?;
    Some(Transform::new(point(translation)?, quaternion(rotation)?))
}

/// A `std_msgs/ColorRGBA`.
///
/// An all-zero colour is what an unfilled message carries, and drawing a marker
/// invisible because its publisher forgot the alpha helps nobody — so a fully
/// transparent colour is taken as opaque white.
pub fn rgba(value: &CanonicalValue) -> Option<[f32; 4]> {
    let channel = |name: &str| value.get_path(name).and_then(real).unwrap_or(0.);
    let color = [channel("r"), channel("g"), channel("b"), channel("a")];
    if color[3] <= 0. {
        return Some([1., 1., 1., 1.]);
    }
    Some(color)
}

/// A pose, wherever in the message it is hiding.
///
/// `PoseStamped` puts it at `pose`, `Odometry` at `pose.pose`,
/// `PoseWithCovarianceStamped` likewise, and a bare `Pose` is the message. One
/// function for all four, because from a viewer's point of view they are one
/// thing: a place and a facing.
pub fn pose(value: &CanonicalValue) -> Option<Transform> {
    value
        .get_path("pose.pose")
        .and_then(rigid)
        .or_else(|| value.get_path("pose").and_then(rigid))
        .or_else(|| rigid(value))
}

/// A pose drawn as the triad every robotics tool draws it as.
pub fn axes(value: &CanonicalValue) -> Option<Vec<Axis>> {
    Some(vec![Axis {
        transform: pose(value)?.to_mat4(),
        length: AXIS_LENGTH,
    }])
}

/// A `nav_msgs/Path`, as the line strip it describes.
pub fn trail(value: &CanonicalValue, color: [f32; 4]) -> Option<LineSet> {
    let CanonicalValue::Array(entries) = value.get_path("poses")? else {
        return None;
    };
    let points: Vec<[f32; 3]> = entries
        .iter()
        .filter_map(|entry| Some(pose(entry)?.translation))
        .collect();
    Some(LineSet {
        points,
        color,
        strip: true,
    })
}

/// A `sensor_msgs/LaserScan`, as the ring of points it describes.
///
/// The message is a start angle, a step, and a list of distances — the points
/// themselves are never on the wire, which is why a scan drawn without this
/// conversion is a flat list of numbers.
pub fn scan(value: &CanonicalValue) -> Option<Points> {
    let angle_min = real(value.get_path("angle_min")?)?;
    let increment = real(value.get_path("angle_increment")?)?;
    let CanonicalValue::Array(ranges) = value.get_path("ranges")? else {
        return None;
    };
    // A driver that reports a minimum of zero means "no minimum"; the defaults
    // are chosen so a message missing either bound is not silently emptied.
    let range_min = value.get_path("range_min").and_then(real).unwrap_or(0.);
    let range_max = value
        .get_path("range_max")
        .and_then(real)
        .filter(|max| *max > 0.)
        .unwrap_or(f32::INFINITY);

    let intensities = match value.get_path("intensities") {
        Some(CanonicalValue::Array(values)) if values.len() == ranges.len() => {
            Some(values.iter().filter_map(real).collect::<Vec<f32>>())
        }
        _ => None,
    };
    let intensities = intensities.filter(|values| values.len() == ranges.len());

    let mut positions = Vec::with_capacity(ranges.len());
    let mut kept_intensity = intensities
        .as_ref()
        .map(|_| Vec::with_capacity(ranges.len()));
    for (index, range) in ranges.iter().enumerate() {
        // A beam that hit nothing is reported as infinity, NaN, or a value
        // outside the sensor's stated range. Drawing any of those puts a point
        // at the origin — a false wall right where the robot is.
        let Some(range) = real(range).filter(|range| *range >= range_min && *range <= range_max)
        else {
            continue;
        };
        let (sin, cos) = (angle_min + index as f32 * increment).sin_cos();
        positions.push([range * cos, range * sin, 0.]);
        if let (Some(kept), Some(values)) = (kept_intensity.as_mut(), intensities.as_ref()) {
            kept.push(values[index]);
        }
    }

    Some(Points {
        positions,
        rgb: None,
        intensity: kept_intensity,
        coloring: Coloring::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    pub(crate) fn map<const N: usize>(fields: [(&str, CanonicalValue); N]) -> CanonicalValue {
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

    fn identity_rotation() -> CanonicalValue {
        map([
            ("x", CanonicalValue::F64(0.)),
            ("y", CanonicalValue::F64(0.)),
            ("z", CanonicalValue::F64(0.)),
            ("w", CanonicalValue::F64(1.)),
        ])
    }

    #[test]
    fn a_frame_id_is_read_and_ros_ones_leading_slash_is_dropped() {
        let with = map([(
            "header",
            map([("frame_id", CanonicalValue::String("/base_link".into()))]),
        )]);
        let without = map([(
            "header",
            map([("frame_id", CanonicalValue::String("base_link".into()))]),
        )]);
        assert_eq!(frame_id(&with).as_deref(), Some("base_link"));
        assert_eq!(frame_id(&without), frame_id(&with));
    }

    #[test]
    fn a_message_with_no_frame_has_none_rather_than_an_empty_name() {
        assert_eq!(frame_id(&map([])), None);
        let blank = map([(
            "header",
            map([("frame_id", CanonicalValue::String(String::new()))]),
        )]);
        assert_eq!(frame_id(&blank), None);
    }

    #[test]
    fn a_stamp_is_read_from_all_three_shapes_it_arrives_in() {
        let shapes = [
            CanonicalValue::Time {
                sec: 7,
                nanosec: 250_000_000,
            },
            map([
                ("sec", CanonicalValue::Int(7)),
                ("nanosec", CanonicalValue::Uint(250_000_000)),
            ]),
            map([
                ("secs", CanonicalValue::Int(7)),
                ("nsecs", CanonicalValue::Int(250_000_000)),
            ]),
        ];
        for shape in shapes {
            assert_eq!(stamp_ns(&shape), Some(7_250_000_000), "{shape:?}");
        }
    }

    #[test]
    fn a_negative_stamp_is_refused_rather_than_wrapped() {
        assert_eq!(
            stamp_ns(&CanonicalValue::Time {
                sec: -5,
                nanosec: 0
            }),
            None,
            "not eighteen billion seconds in the future"
        );
    }

    #[test]
    fn both_spellings_of_a_rigid_placement_read_the_same() {
        let transform = map([
            ("translation", xyz(1., 2., 3.)),
            ("rotation", identity_rotation()),
        ]);
        let pose = map([
            ("position", xyz(1., 2., 3.)),
            ("orientation", identity_rotation()),
        ]);
        assert_eq!(rigid(&transform), rigid(&pose));
        assert_eq!(rigid(&pose).unwrap().translation, [1., 2., 3.]);
    }

    #[test]
    fn a_pose_is_found_wherever_the_message_hid_it() {
        let bare = map([
            ("position", xyz(1., 0., 0.)),
            ("orientation", identity_rotation()),
        ]);
        let stamped = map([("pose", bare.clone())]);
        // Odometry, which wraps the pose again for its covariance.
        let odometry = map([("pose", map([("pose", bare.clone())]))]);
        for message in [&bare, &stamped, &odometry] {
            assert_eq!(
                pose(message).map(|placed| placed.translation),
                Some([1., 0., 0.]),
                "{message:?}"
            );
        }
    }

    #[test]
    fn a_transparent_colour_is_taken_as_opaque_rather_than_drawn_invisible() {
        let unfilled = map([
            ("r", CanonicalValue::F64(0.)),
            ("g", CanonicalValue::F64(0.)),
            ("b", CanonicalValue::F64(0.)),
            ("a", CanonicalValue::F64(0.)),
        ]);
        assert_eq!(rgba(&unfilled), Some([1., 1., 1., 1.]));
        let real_colour = map([
            ("r", CanonicalValue::F64(1.)),
            ("g", CanonicalValue::F64(0.5)),
            ("b", CanonicalValue::F64(0.)),
            ("a", CanonicalValue::F64(0.8)),
        ]);
        assert_eq!(rgba(&real_colour), Some([1., 0.5, 0., 0.8]));
    }

    fn ranges(values: &[f32]) -> CanonicalValue {
        CanonicalValue::Array(values.iter().map(|v| CanonicalValue::F32(*v)).collect())
    }

    fn a_scan(values: &[f32]) -> CanonicalValue {
        map([
            ("angle_min", CanonicalValue::F32(0.)),
            (
                "angle_increment",
                CanonicalValue::F32(std::f32::consts::FRAC_PI_2),
            ),
            ("range_min", CanonicalValue::F32(0.1)),
            ("range_max", CanonicalValue::F32(30.)),
            ("ranges", ranges(values)),
        ])
    }

    #[test]
    fn a_scan_becomes_the_ring_of_points_its_angles_describe() {
        // Four beams a quarter turn apart, at one metre.
        let points = scan(&a_scan(&[1., 1., 1., 1.])).expect("decodes");
        assert_eq!(points.positions.len(), 4);
        let close = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-5);
        assert!(close(points.positions[0], [1., 0., 0.]));
        assert!(close(points.positions[1], [0., 1., 0.]));
        assert!(close(points.positions[2], [-1., 0., 0.]));
        assert!(close(points.positions[3], [0., -1., 0.]));
    }

    #[test]
    fn beams_that_hit_nothing_are_dropped_rather_than_drawn_at_the_robot() {
        // Infinity, NaN, and a return past the sensor's own maximum: all three
        // would otherwise put a false wall exactly where the robot is.
        let points = scan(&a_scan(&[1., f32::INFINITY, f32::NAN, 500.])).expect("decodes");
        assert_eq!(points.positions.len(), 1);
        assert_eq!(points.positions[0], [1., 0., 0.]);
    }

    #[test]
    fn a_return_inside_the_sensors_blind_spot_is_dropped_too() {
        let points = scan(&a_scan(&[0.01, 2.])).expect("decodes");
        assert_eq!(points.positions.len(), 1, "0.01 m is below range_min");
    }

    #[test]
    fn a_scan_keeps_its_intensities_lined_up_with_the_beams_that_survived() {
        let mut fields = a_scan(&[1., f32::INFINITY, 3.]);
        let CanonicalValue::Struct(map) = &mut fields else {
            unreachable!()
        };
        map.insert("intensities".into(), ranges(&[10., 20., 30.]));
        let points = scan(&fields).expect("decodes");
        assert_eq!(points.positions.len(), 2);
        assert_eq!(
            points.intensity,
            Some(vec![10., 30.]),
            "the dropped beam took its intensity with it"
        );
    }

    #[test]
    fn a_scan_with_mismatched_intensities_drops_them_rather_than_mispairing() {
        let mut fields = a_scan(&[1., 2., 3.]);
        let CanonicalValue::Struct(map) = &mut fields else {
            unreachable!()
        };
        map.insert("intensities".into(), ranges(&[10.]));
        assert_eq!(scan(&fields).expect("decodes").intensity, None);
    }

    #[test]
    fn a_message_that_is_not_a_scan_is_refused() {
        assert_eq!(scan(&map([("data", CanonicalValue::Int(1))])), None);
    }

    #[test]
    fn a_path_becomes_a_strip_through_its_poses() {
        let step = |x: f64| {
            map([(
                "pose",
                map([
                    ("position", xyz(x, 0., 0.)),
                    ("orientation", identity_rotation()),
                ]),
            )])
        };
        let path = map([(
            "poses",
            CanonicalValue::Array(vec![step(0.), step(1.), step(2.)]),
        )]);
        let strip = trail(&path, [1., 1., 1., 1.]).expect("decodes");
        assert!(strip.strip, "a path continues from point to point");
        assert_eq!(strip.points, vec![[0., 0., 0.], [1., 0., 0.], [2., 0., 0.]]);
    }

    #[test]
    fn a_pose_becomes_one_triad_at_the_place_it_names() {
        let message = map([(
            "pose",
            map([
                ("position", xyz(3., 4., 5.)),
                ("orientation", identity_rotation()),
            ]),
        )]);
        let axes = axes(&message).expect("decodes");
        assert_eq!(axes.len(), 1);
        assert_eq!(
            rw_render::transform_point(axes[0].transform, [0.; 3]),
            [3., 4., 5.]
        );
        assert_eq!(axes[0].length, AXIS_LENGTH);
    }
}
