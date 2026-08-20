//! A synthetic robot in a synthetic room, so the 3D panes are drivable with no
//! robot present.
//!
//! The point of this world is that **it is only correct through TF**. A robot
//! drives a circuit of a six-metre room with a lidar on its nose; the scan and
//! the cloud arrive in the sensor's own frame, the path and the pose arrive in
//! `map`, and nothing lines up unless the transform tree is read and applied.
//! With the fixed frame set to `map` the walls stand still and the robot moves
//! through them; set it to `base_link` and the room swings round the robot
//! instead. Either picture is a screenshot that proves the tree was used —
//! and before it, everything sat on top of everything else at the origin.
//!
//! The frames are the ones every ROS navigation stack uses, so the shapes here
//! are the shapes a real system publishes:
//!
//! ```text
//! map ──(static)── odom ──(/tf, 10 Hz)── base_link ──(static)── laser
//! ```

use std::collections::BTreeMap;

use rw_canonical::CanonicalValue;

/// Half the width of the room, in metres.
pub const ROOM: f32 = 6.;
/// How high the ceiling is.
pub const CEILING: f32 = 2.5;
/// How far the robot's circuit is from the middle of the room.
const ORBIT: f32 = 2.5;
/// Where the sensor sits on the robot: forward of the axle and above it.
const SENSOR: [f32; 3] = [0.25, 0., 0.35];
/// How long the trail behind the robot is, in ticks.
const TRAIL: i64 = 90;

/// Where the robot is, and which way it is facing, at a given tick.
///
/// A circuit rather than a straight line: a robot going in a straight line
/// looks the same whether or not its rotation was applied, and a turning one
/// does not.
pub fn robot(tick: i64) -> ([f32; 3], f32) {
    let angle = tick as f32 * 0.012;
    (
        [ORBIT * angle.cos(), ORBIT * angle.sin(), 0.],
        // Facing along the circuit, which is a quarter turn from the radius.
        angle + std::f32::consts::FRAC_PI_2,
    )
}

/// Where the lidar is in the room, given the robot's pose.
fn sensor(tick: i64) -> [f32; 3] {
    let (base, yaw) = robot(tick);
    let (sin, cos) = yaw.sin_cos();
    [
        base[0] + SENSOR[0] * cos - SENSOR[1] * sin,
        base[1] + SENSOR[0] * sin + SENSOR[1] * cos,
        base[2] + SENSOR[2],
    ]
}

/// How far a ray from `origin` travels before it meets the room.
///
/// The room is a box: four walls, a floor and a ceiling. Casting from the
/// sensor's real position is what makes the walls stand still once the scan is
/// placed in `map` — a cloud generated around the origin regardless of where
/// the sensor was would ride along with the robot and prove nothing.
fn range_to_wall(origin: [f32; 3], direction: [f32; 3]) -> Option<f32> {
    let mut nearest = f32::INFINITY;
    let planes = [
        (0, if direction[0] > 0. { ROOM } else { -ROOM }),
        (1, if direction[1] > 0. { ROOM } else { -ROOM }),
        (2, if direction[2] > 0. { CEILING } else { 0. }),
    ];
    for (axis, limit) in planes {
        if direction[axis].abs() > 1e-4 {
            let distance = (limit - origin[axis]) / direction[axis];
            if distance > 1e-3 {
                nearest = nearest.min(distance);
            }
        }
    }
    nearest.is_finite().then_some(nearest)
}

/// The transform tree at this tick: `odom → base_link`, the one thing that
/// moves.
pub fn tf(tick: i64, at_ns: u64) -> CanonicalValue {
    let (position, yaw) = robot(tick);
    struct_of([(
        "transforms",
        CanonicalValue::Array(vec![transform_stamped(
            "odom",
            "base_link",
            at_ns,
            position,
            yaw,
        )]),
    )])
}

/// The parts of the tree that never move.
///
/// `map → odom` is the identity here — a real localiser corrects it, and a
/// simulator has nothing to correct — and `base_link → laser` is where the
/// sensor is bolted on. Both are exactly the edges a real system puts on
/// `/tf_static`.
pub fn tf_static(at_ns: u64) -> CanonicalValue {
    struct_of([(
        "transforms",
        CanonicalValue::Array(vec![
            transform_stamped("map", "odom", at_ns, [0.; 3], 0.),
            transform_stamped("base_link", "laser", at_ns, SENSOR, 0.),
        ]),
    )])
}

/// A horizontal ring of ranges, in the sensor's own frame.
pub fn scan(tick: i64, at_ns: u64) -> CanonicalValue {
    const BEAMS: usize = 360;
    let angle_min = -std::f32::consts::PI;
    let increment = std::f32::consts::TAU / BEAMS as f32;
    let origin = sensor(tick);
    let (_, yaw) = robot(tick);

    let mut ranges = Vec::with_capacity(BEAMS);
    for beam in 0..BEAMS {
        // The beam's bearing in the sensor's frame, turned into the room's.
        let bearing = yaw + angle_min + beam as f32 * increment;
        let (sin, cos) = bearing.sin_cos();
        ranges.push(CanonicalValue::F32(
            range_to_wall(origin, [cos, sin, 0.]).unwrap_or(f32::INFINITY),
        ));
    }

    struct_of([
        ("header", header(at_ns, "laser")),
        ("angle_min", CanonicalValue::F32(angle_min)),
        (
            "angle_max",
            CanonicalValue::F32(angle_min + std::f32::consts::TAU),
        ),
        ("angle_increment", CanonicalValue::F32(increment)),
        ("time_increment", CanonicalValue::F32(0.)),
        ("scan_time", CanonicalValue::F32(0.1)),
        ("range_min", CanonicalValue::F32(0.05)),
        (
            "range_max",
            CanonicalValue::F32(ROOM * 2. * std::f32::consts::SQRT_2),
        ),
        ("ranges", CanonicalValue::Array(ranges)),
        ("intensities", CanonicalValue::Array(Vec::new())),
    ])
}

/// Where the robot has been, in `map`.
pub fn path(tick: i64, at_ns: u64) -> CanonicalValue {
    let poses: Vec<CanonicalValue> = (0..TRAIL)
        .rev()
        .map(|back| {
            let (position, yaw) = robot(tick - back);
            pose_stamped(at_ns, position, yaw)
        })
        .collect();
    struct_of([
        ("header", header(at_ns, "map")),
        ("poses", CanonicalValue::Array(poses)),
    ])
}

/// Where the robot is now, in `map`.
pub fn pose(tick: i64, at_ns: u64) -> CanonicalValue {
    let (position, yaw) = robot(tick);
    pose_stamped(at_ns, position, yaw)
}

/// A synthetic lidar sweep, in the sensor's own frame.
///
/// Several rings from below the horizon to above it, so the sweep paints the
/// floor near the robot and the walls further out — enough structure that a
/// mixed-up axis or an unapplied rotation is obvious on sight rather than a
/// blob that looks the same either way.
pub fn cloud(tick: i64, at_ns: u64) -> CanonicalValue {
    const RINGS: usize = 24;
    const PER_RING: usize = 220;
    let origin = sensor(tick);
    let (_, yaw) = robot(tick);

    // x, y, z as float32 with intensity after them: the layout a real driver
    // publishes, so the decoder is exercised rather than humoured.
    let mut data = Vec::with_capacity(RINGS * PER_RING * 16);
    let mut count = 0u64;
    for ring in 0..RINGS {
        let elevation = -0.55 + ring as f32 * 0.045;
        let (dz, horizontal) = elevation.sin_cos();
        for step in 0..PER_RING {
            // The beam's own direction, in the sensor's frame.
            let bearing = step as f32 / PER_RING as f32 * std::f32::consts::TAU;
            let (sin, cos) = bearing.sin_cos();
            let local = [horizontal * cos, horizontal * sin, dz];

            // The same direction in the room, which is where the walls are.
            let (ysin, ycos) = yaw.sin_cos();
            let world = [
                local[0] * ycos - local[1] * ysin,
                local[0] * ysin + local[1] * ycos,
                local[2],
            ];
            let Some(range) = range_to_wall(origin, world) else {
                continue;
            };

            for component in local {
                data.extend_from_slice(&(component * range).to_le_bytes());
            }
            // Near returns come back stronger, which is what intensity means.
            data.extend_from_slice(&(200. / (1. + range)).to_le_bytes());
            count += 1;
        }
    }

    struct_of([
        // The header is what makes this placeable. Without a frame_id a cloud
        // is a heap of numbers that could be anywhere.
        ("header", header(at_ns, "laser")),
        ("height", CanonicalValue::Uint(1)),
        ("width", CanonicalValue::Uint(count)),
        (
            "fields",
            CanonicalValue::Array(vec![
                point_field("x", 0),
                point_field("y", 4),
                point_field("z", 8),
                point_field("intensity", 12),
            ]),
        ),
        ("is_bigendian", CanonicalValue::Bool(false)),
        ("point_step", CanonicalValue::Uint(16)),
        ("row_step", CanonicalValue::Uint(data.len() as u64)),
        ("data", CanonicalValue::Bytes(data)),
        ("is_dense", CanonicalValue::Bool(true)),
    ])
}

/// One `sensor_msgs/PointField`; datatype 7 is FLOAT32.
fn point_field(name: &str, offset: u64) -> CanonicalValue {
    struct_of([
        ("name", CanonicalValue::String(name.into())),
        ("offset", CanonicalValue::Uint(offset)),
        ("datatype", CanonicalValue::Uint(7)),
        ("count", CanonicalValue::Uint(1)),
    ])
}

fn transform_stamped(
    parent: &str,
    child: &str,
    at_ns: u64,
    translation: [f32; 3],
    yaw: f32,
) -> CanonicalValue {
    struct_of([
        ("header", header(at_ns, parent)),
        ("child_frame_id", CanonicalValue::String(child.into())),
        (
            "transform",
            struct_of([
                ("translation", vector(translation)),
                ("rotation", yaw_quaternion(yaw)),
            ]),
        ),
    ])
}

fn pose_stamped(at_ns: u64, position: [f32; 3], yaw: f32) -> CanonicalValue {
    struct_of([
        ("header", header(at_ns, "map")),
        (
            "pose",
            struct_of([
                ("position", vector(position)),
                ("orientation", yaw_quaternion(yaw)),
            ]),
        ),
    ])
}

fn header(at_ns: u64, frame_id: &str) -> CanonicalValue {
    struct_of([
        (
            "stamp",
            CanonicalValue::Time {
                sec: (at_ns / 1_000_000_000) as i32,
                nanosec: (at_ns % 1_000_000_000) as u32,
            },
        ),
        ("frame_id", CanonicalValue::String(frame_id.into())),
    ])
}

fn vector(xyz: [f32; 3]) -> CanonicalValue {
    struct_of([
        ("x", CanonicalValue::F64(xyz[0] as f64)),
        ("y", CanonicalValue::F64(xyz[1] as f64)),
        ("z", CanonicalValue::F64(xyz[2] as f64)),
    ])
}

/// A rotation about z only, which is all a wheeled robot on a floor does.
fn yaw_quaternion(yaw: f32) -> CanonicalValue {
    let (sin, cos) = (yaw / 2.).sin_cos();
    struct_of([
        ("x", CanonicalValue::F64(0.)),
        ("y", CanonicalValue::F64(0.)),
        ("z", CanonicalValue::F64(sin as f64)),
        ("w", CanonicalValue::F64(cos as f64)),
    ])
}

fn struct_of<const N: usize>(fields: [(&str, CanonicalValue); N]) -> CanonicalValue {
    CanonicalValue::Struct(
        fields
            .into_iter()
            .map(|(name, value)| (name.to_string(), value))
            .collect::<BTreeMap<_, _>>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn real(value: &CanonicalValue, path: &str) -> f32 {
        match value.get_path(path) {
            Some(CanonicalValue::F64(number)) => *number as f32,
            Some(CanonicalValue::F32(number)) => *number,
            other => panic!("{path} is {other:?}"),
        }
    }

    #[test]
    fn the_robot_drives_a_circuit_rather_than_standing_still() {
        let (start, _) = robot(0);
        let (later, _) = robot(120);
        let moved = ((later[0] - start[0]).powi(2) + (later[1] - start[1]).powi(2)).sqrt();
        assert!(moved > 1., "the robot only moved {moved} m in 12 seconds");
        for tick in [0, 50, 500, 5000] {
            let (position, _) = robot(tick);
            let radius = (position[0].powi(2) + position[1].powi(2)).sqrt();
            assert!(
                (radius - ORBIT).abs() < 1e-3,
                "tick {tick} left the circuit at {radius} m"
            );
        }
    }

    #[test]
    fn the_robot_faces_the_way_it_is_travelling() {
        let (before, yaw) = robot(100);
        let (after, _) = robot(101);
        let travel = (after[1] - before[1]).atan2(after[0] - before[0]);
        let difference = (travel - yaw).sin().abs();
        assert!(difference < 1e-2, "facing {yaw} while travelling {travel}");
    }

    #[test]
    fn the_moving_edge_is_the_one_a_real_stack_moves() {
        let message = tf(10, 1_000_000_000);
        let CanonicalValue::Array(entries) = message.get_path("transforms").unwrap() else {
            panic!("transforms is not an array");
        };
        assert_eq!(entries.len(), 1, "only base_link moves");
        assert_eq!(
            entries[0].get_path("header.frame_id"),
            Some(&CanonicalValue::String("odom".into()))
        );
        assert_eq!(
            entries[0].get_path("child_frame_id"),
            Some(&CanonicalValue::String("base_link".into()))
        );
    }

    #[test]
    fn the_static_tree_bolts_the_sensor_to_the_robot_and_odom_to_the_map() {
        let message = tf_static(0);
        let CanonicalValue::Array(entries) = message.get_path("transforms").unwrap() else {
            panic!("transforms is not an array");
        };
        let edges: Vec<(String, String)> = entries
            .iter()
            .map(|entry| {
                let parent = entry.get_path("header.frame_id").unwrap();
                let child = entry.get_path("child_frame_id").unwrap();
                match (parent, child) {
                    (CanonicalValue::String(p), CanonicalValue::String(c)) => {
                        (p.clone(), c.clone())
                    }
                    _ => panic!("frames are not strings"),
                }
            })
            .collect();
        assert_eq!(
            edges,
            vec![
                ("map".to_string(), "odom".to_string()),
                ("base_link".to_string(), "laser".to_string()),
            ]
        );
    }

    #[test]
    fn the_whole_tree_composes_into_a_sensor_where_the_maths_says_it_is() {
        // The end-to-end check: feed both messages through the same decoder the
        // app uses and ask the buffer where the sensor ended up.
        let mut tree = rw_tf::Buffer::new();
        let at = 3_000_000_000;
        for (message, is_static) in [(tf(120, at), false), (tf_static(at), true)] {
            let CanonicalValue::Array(entries) = message.get_path("transforms").unwrap() else {
                panic!("transforms is not an array");
            };
            for entry in entries {
                let (CanonicalValue::String(parent), CanonicalValue::String(child)) = (
                    entry.get_path("header.frame_id").unwrap(),
                    entry.get_path("child_frame_id").unwrap(),
                ) else {
                    panic!("frames are not strings");
                };
                let placed = rw_tf::Transform::new(
                    [
                        real(entry, "transform.translation.x"),
                        real(entry, "transform.translation.y"),
                        real(entry, "transform.translation.z"),
                    ],
                    rw_tf::Quat::from_wire(
                        real(entry, "transform.rotation.x"),
                        real(entry, "transform.rotation.y"),
                        real(entry, "transform.rotation.z"),
                        real(entry, "transform.rotation.w"),
                    ),
                );
                if is_static {
                    tree.insert_static(parent, child, placed);
                } else {
                    tree.insert(parent, child, at, placed);
                }
            }
        }

        let placed = tree.lookup("map", "laser", at).expect("the tree answers");
        let expected = sensor(120);
        let got = placed.apply([0.; 3]);
        for axis in 0..3 {
            assert!(
                (got[axis] - expected[axis]).abs() < 1e-4,
                "axis {axis}: tree says {got:?}, the world says {expected:?}"
            );
        }
    }

    #[test]
    fn the_cloud_is_cast_from_where_the_sensor_actually_is() {
        // The property the whole demo rests on: the same wall must land at the
        // same place in `map` however the robot has moved. Two ticks far apart,
        // each cloud placed by its own transform, must agree about the room.
        let far_side = |tick: i64| {
            let origin = sensor(tick);
            // Straight along the room's +x, in the room's frame.
            let range = range_to_wall(origin, [1., 0., 0.]).expect("hits a wall");
            origin[0] + range
        };
        assert!((far_side(0) - ROOM).abs() < 1e-3);
        assert!((far_side(200) - ROOM).abs() < 1e-3);
        assert!(
            (far_side(0) - far_side(200)).abs() < 1e-3,
            "the far wall moved when the robot did"
        );
    }

    #[test]
    fn a_ray_that_meets_nothing_is_none_rather_than_infinity_in_the_data() {
        assert_eq!(range_to_wall([0., 0., 1.], [0., 0., 0.]), None);
    }

    #[test]
    fn the_cloud_carries_the_frame_it_is_in() {
        let message = cloud(10, 5);
        assert_eq!(
            message.get_path("header.frame_id"),
            Some(&CanonicalValue::String("laser".into())),
            "a cloud with no frame is a heap of numbers"
        );
        let CanonicalValue::Bytes(data) = message.get_path("data").unwrap() else {
            panic!("data is not bytes");
        };
        assert!(!data.is_empty());
        assert_eq!(data.len() % 16, 0, "whole points only");
    }

    #[test]
    fn the_scan_is_a_full_turn_of_beams_in_the_sensors_frame() {
        let message = scan(10, 5);
        assert_eq!(
            message.get_path("header.frame_id"),
            Some(&CanonicalValue::String("laser".into()))
        );
        let CanonicalValue::Array(ranges) = message.get_path("ranges").unwrap() else {
            panic!("ranges is not an array");
        };
        assert_eq!(ranges.len(), 360);
        let increment = real(&message, "angle_increment");
        let span = increment * ranges.len() as f32;
        assert!(
            (span - std::f32::consts::TAU).abs() < 1e-4,
            "the beams cover {span} rad"
        );
        // Every beam hits a wall of a closed room, and none is further away
        // than the room's diagonal.
        for range in ranges {
            let CanonicalValue::F32(range) = range else {
                panic!("a range is not a float")
            };
            assert!(
                range.is_finite() && *range > 0.,
                "a beam in a closed room reported {range}"
            );
            assert!(*range < ROOM * 3.);
        }
    }

    #[test]
    fn the_path_is_the_trail_the_robot_has_just_driven() {
        let tick = 500;
        let message = path(tick, 5);
        assert_eq!(
            message.get_path("header.frame_id"),
            Some(&CanonicalValue::String("map".into())),
            "a path is in the map, not in the robot"
        );
        let CanonicalValue::Array(poses) = message.get_path("poses").unwrap() else {
            panic!("poses is not an array");
        };
        assert_eq!(poses.len() as i64, TRAIL);
        // The last pose of the trail is where the robot is now.
        let (here, _) = robot(tick);
        let last = poses.last().unwrap();
        assert!((real(last, "pose.position.x") - here[0]).abs() < 1e-4);
        assert!((real(last, "pose.position.y") - here[1]).abs() < 1e-4);
    }

    #[test]
    fn the_pose_is_the_robot_itself() {
        let message = pose(77, 5);
        let (here, _) = robot(77);
        assert!((real(&message, "pose.position.x") - here[0]).abs() < 1e-4);
        assert_eq!(
            message.get_path("header.frame_id"),
            Some(&CanonicalValue::String("map".into()))
        );
    }

    #[test]
    fn a_yaw_quaternion_is_unit_length_and_only_turns_about_z() {
        for yaw in [0f32, 1.2, -2.9, 6.0] {
            let quaternion = yaw_quaternion(yaw);
            let (x, y, z, w) = (
                real(&quaternion, "x"),
                real(&quaternion, "y"),
                real(&quaternion, "z"),
                real(&quaternion, "w"),
            );
            assert_eq!((x, y), (0., 0.));
            assert!((x * x + y * y + z * z + w * w - 1.).abs() < 1e-5);
        }
    }
}
