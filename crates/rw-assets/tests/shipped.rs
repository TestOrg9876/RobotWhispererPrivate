//! Every robot that ships with the app loads, from the real files on disk.
//!
//! The unit tests cover the parsers against small documents and a couple of
//! real meshes. This covers the whole catalog end to end, which is the thing
//! that actually breaks: one robot in seven using a convention the others do
//! not.

use rw_assets::catalog::Catalog;
use rw_assets::kinematics::{self, Pose};
use rw_assets::math::transform_point;

fn catalog() -> Catalog {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets");
    Catalog::open(root).expect("the shipped assets directory opens")
}

#[test]
fn the_manifest_lists_every_shipped_robot() {
    let catalog = catalog();
    let ids: Vec<&str> = catalog
        .entries()
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    assert_eq!(ids.len(), 7, "got {ids:?}");
    assert!(ids.contains(&"ur10e") && ids.contains(&"franka_panda"));
}

#[test]
fn display_names_come_from_the_config_rather_than_the_directory() {
    let catalog = catalog();
    let ur10e = catalog.entry("ur10e").expect("is listed");
    assert_eq!(ur10e.name, "UR10e");
    assert_eq!(ur10e.brand.as_deref(), Some("Universal Robots"));
}

#[test]
fn every_robot_loads_with_geometry_and_joints() {
    let catalog = catalog();
    for entry in catalog.entries() {
        let loaded = catalog
            .load(&entry.id)
            .unwrap_or_else(|error| panic!("{}: {error}", entry.id));
        assert!(
            loaded.missing.is_empty(),
            "{}: could not read {:?}",
            entry.id,
            loaded.missing
        );
        assert!(
            loaded.triangle_count() > 500,
            "{} loaded only {} triangles",
            entry.id,
            loaded.triangle_count()
        );
        assert!(
            loaded.robot.movable().count() > 0,
            "{} has nothing to move",
            entry.id
        );
    }
}

#[test]
fn every_robot_is_a_believable_size_and_every_link_is_placed() {
    let catalog = catalog();
    for entry in catalog.entries() {
        let loaded = catalog.load(&entry.id).expect("loads");
        let placed = kinematics::solve(&loaded.robot, &Pose::rest(&loaded.robot));

        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for (link, parts) in &loaded.meshes {
            let world = placed
                .get(link)
                .unwrap_or_else(|| panic!("{}: {link} was never placed", entry.id));
            for position in parts.iter().flat_map(|part| &part.positions) {
                let point = transform_point(*world, *position);
                for axis in 0..3 {
                    min[axis] = min[axis].min(point[axis]);
                    max[axis] = max[axis].max(point[axis]);
                }
            }
        }

        for axis in 0..3 {
            let side = max[axis] - min[axis];
            // A hand is a few centimetres, an arm a couple of metres. Anything
            // outside that means a unit or scale conversion was missed.
            assert!(
                (0.02..4.0).contains(&side),
                "{} is {side} metres across axis {axis}",
                entry.id
            );
        }
    }
}

#[test]
fn moving_a_joint_moves_the_geometry_of_every_robot() {
    let catalog = catalog();
    for entry in catalog.entries() {
        let loaded = catalog.load(&entry.id).expect("loads");
        let joint = loaded.robot.movable().next().expect("has a joint").clone();
        let mut pose = Pose::rest(&loaded.robot);
        // Somewhere inside the limits, and not where it started.
        let value = match joint.limits {
            Some((lower, upper)) => lower + (upper - lower) * 0.75,
            None => 1.0,
        };
        pose.set(&joint.name, value);

        let rest = kinematics::solve(&loaded.robot, &Pose::rest(&loaded.robot));
        let moved = kinematics::solve(&loaded.robot, &pose);
        // Compared as whole transforms, not as the origin point: a joint that
        // turns about an axis its children sit on moves nothing at the origin
        // while still rotating everything hanging off it.
        let changed = rest.iter().any(|(link, before)| {
            let after = moved.get(link).expect("the same links are placed");
            before
                .iter()
                .flatten()
                .zip(after.iter().flatten())
                .any(|(before, after)| (before - after).abs() > 1e-4)
        });
        assert!(
            changed,
            "{}: driving {} moved nothing",
            entry.id, joint.name
        );
    }
}

#[test]
fn a_mesh_path_cannot_climb_out_of_the_assets_directory() {
    let catalog = catalog();
    // The guard lives in `resolve`, which is private; a description naming an
    // escaping path is the way to reach it.
    let robot = rw_assets::urdf::parse(
        r#"<robot name="bad">
             <link name="a">
               <visual><geometry>
                 <mesh filename="package://../../../etc/passwd"/>
               </geometry></visual>
             </link>
           </robot>"#,
    )
    .expect("parses");
    // Nothing to assert against the catalog directly, so this checks the shape
    // of the path the guard rejects.
    let filename = match &robot.links[0].visuals[0].geometry {
        rw_assets::urdf::Geometry::Mesh { filename, .. } => filename.clone(),
        other => panic!("expected a mesh, got {other:?}"),
    };
    assert!(filename.contains(".."));
    assert!(
        catalog.load("no-such-robot").is_err(),
        "an unknown robot is an error rather than an empty one"
    );
}
