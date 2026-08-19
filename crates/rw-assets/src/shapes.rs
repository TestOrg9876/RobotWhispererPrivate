//! The primitive shapes a URDF can name instead of a mesh file.
//!
//! Built to the URDF conventions: a box is centred on its link's origin, a
//! cylinder runs along z and is centred on it too, and a sphere is centred.
//! Getting that wrong shows up as a part sunk half into the floor.

use crate::mesh::{Mesh, Part, normalize};

/// How many segments go round a cylinder or sphere. Enough that the silhouette
/// reads as round at the size these appear on screen.
const SEGMENTS: usize = 24;
const RINGS: usize = 12;

/// A box of the given full extents, centred on the origin.
pub fn cuboid(size: [f32; 3]) -> Mesh {
    let [x, y, z] = [size[0] / 2., size[1] / 2., size[2] / 2.];
    let mut part = Part::default();
    // Each face separately, so every vertex carries its own face normal and the
    // edges stay sharp.
    let faces: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
        ([0., 0., 1.], [1., 0., 0.], [0., 1., 0.]),
        ([0., 0., -1.], [0., 1., 0.], [1., 0., 0.]),
        ([1., 0., 0.], [0., 1., 0.], [0., 0., 1.]),
        ([-1., 0., 0.], [0., 0., 1.], [0., 1., 0.]),
        ([0., 1., 0.], [0., 0., 1.], [1., 0., 0.]),
        ([0., -1., 0.], [1., 0., 0.], [0., 0., 1.]),
    ];
    for (normal, u, v) in faces {
        let centre = [normal[0] * x, normal[1] * y, normal[2] * z];
        let u = [u[0] * x, u[1] * y, u[2] * z];
        let v = [v[0] * x, v[1] * y, v[2] * z];
        let base = part.positions.len() as u32;
        for (su, sv) in [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)] {
            part.positions.push([
                centre[0] + u[0] * su + v[0] * sv,
                centre[1] + u[1] * su + v[1] * sv,
                centre[2] + u[2] * su + v[2] * sv,
            ]);
            part.normals.push(normal);
        }
        part.indices
            .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    Mesh { parts: vec![part] }
}

/// A cylinder along z, centred on the origin, with flat caps.
pub fn cylinder(radius: f32, length: f32) -> Mesh {
    let half = length / 2.;
    let mut part = Part::default();
    let angle = |segment: usize| segment as f32 / SEGMENTS as f32 * std::f32::consts::TAU;

    for segment in 0..SEGMENTS {
        let (s0, c0) = angle(segment).sin_cos();
        let (s1, c1) = angle(segment + 1).sin_cos();
        let base = part.positions.len() as u32;
        for (x, y, z, normal) in [
            (c0 * radius, s0 * radius, -half, [c0, s0, 0.]),
            (c1 * radius, s1 * radius, -half, [c1, s1, 0.]),
            (c1 * radius, s1 * radius, half, [c1, s1, 0.]),
            (c0 * radius, s0 * radius, half, [c0, s0, 0.]),
        ] {
            part.positions.push([x, y, z]);
            part.normals.push(normalize(normal));
        }
        part.indices
            .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    for (z, normal, flip) in [(half, [0., 0., 1.], false), (-half, [0., 0., -1.], true)] {
        let centre = part.positions.len() as u32;
        part.positions.push([0., 0., z]);
        part.normals.push(normal);
        for segment in 0..=SEGMENTS {
            let (sin, cos) = angle(segment).sin_cos();
            part.positions.push([cos * radius, sin * radius, z]);
            part.normals.push(normal);
        }
        for segment in 0..SEGMENTS {
            let (a, b) = (centre + 1 + segment as u32, centre + 2 + segment as u32);
            // The bottom cap winds the other way, so both faces point outward.
            if flip {
                part.indices.extend([centre, b, a]);
            } else {
                part.indices.extend([centre, a, b]);
            }
        }
    }

    Mesh { parts: vec![part] }
}

/// A sphere centred on the origin.
pub fn sphere(radius: f32) -> Mesh {
    let mut part = Part::default();
    for ring in 0..=RINGS {
        let phi = ring as f32 / RINGS as f32 * std::f32::consts::PI;
        let (sin_phi, cos_phi) = phi.sin_cos();
        for segment in 0..=SEGMENTS {
            let theta = segment as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            let (sin, cos) = theta.sin_cos();
            let normal = [sin_phi * cos, sin_phi * sin, cos_phi];
            part.positions
                .push([normal[0] * radius, normal[1] * radius, normal[2] * radius]);
            part.normals.push(normal);
        }
    }
    let stride = SEGMENTS as u32 + 1;
    for ring in 0..RINGS as u32 {
        for segment in 0..SEGMENTS as u32 {
            let a = ring * stride + segment;
            part.indices
                .extend([a, a + stride, a + 1, a + 1, a + stride, a + stride + 1]);
        }
    }
    Mesh { parts: vec![part] }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }

    /// Every normal points away from the origin, which for a shape centred on
    /// the origin is what "outward" means.
    fn faces_outward(mesh: &Mesh) -> bool {
        mesh.parts.iter().all(|part| {
            part.positions
                .iter()
                .zip(&part.normals)
                .all(|(position, normal)| dot(*position, *normal) >= -1e-4)
        })
    }

    #[test]
    fn a_box_is_centred_on_the_origin() {
        let mesh = cuboid([2., 4., 6.]);
        assert_eq!(mesh.bounds(), Some(([-1., -2., -3.], [1., 2., 3.])));
        assert_eq!(mesh.triangle_count(), 12);
    }

    #[test]
    fn a_boxs_faces_point_outward() {
        assert!(faces_outward(&cuboid([1., 1., 1.])));
    }

    #[test]
    fn a_cylinder_runs_along_z_and_is_centred_on_it() {
        let mesh = cylinder(0.5, 2.);
        let (min, max) = mesh.bounds().expect("has bounds");
        assert!((min[2] + 1.).abs() < 1e-5 && (max[2] - 1.).abs() < 1e-5);
        assert!((max[0] - 0.5).abs() < 0.01, "radius came out {}", max[0]);
    }

    #[test]
    fn a_cylinders_side_normals_point_away_from_its_axis() {
        let mesh = cylinder(1., 2.);
        let part = &mesh.parts[0];
        for (position, normal) in part.positions.iter().zip(&part.normals) {
            // Only the side wall, whose normals have no z component.
            if normal[2].abs() < 0.5 {
                assert!(
                    dot([position[0], position[1], 0.], [normal[0], normal[1], 0.]) > 0.,
                    "a side normal pointed inward at {position:?}"
                );
            }
        }
    }

    #[test]
    fn a_sphere_has_the_radius_it_was_given() {
        let mesh = sphere(2.);
        for position in mesh.parts.iter().flat_map(|part| &part.positions) {
            let distance = dot(*position, *position).sqrt();
            assert!((distance - 2.).abs() < 1e-4, "got {distance}");
        }
    }

    #[test]
    fn a_spheres_normals_point_outward() {
        assert!(faces_outward(&sphere(1.)));
    }

    #[test]
    fn every_shape_indexes_only_vertices_it_has() {
        for mesh in [cuboid([1., 1., 1.]), cylinder(1., 1.), sphere(1.)] {
            for part in &mesh.parts {
                assert_eq!(part.indices.len() % 3, 0);
                assert!(
                    part.indices
                        .iter()
                        .all(|index| (*index as usize) < part.positions.len()),
                    "an index pointed past the end of the vertex list"
                );
                assert_eq!(part.positions.len(), part.normals.len());
            }
        }
    }

    #[test]
    fn a_zero_sized_shape_is_still_well_formed() {
        let mesh = cuboid([0., 0., 0.]);
        assert_eq!(mesh.triangle_count(), 12);
        assert!(
            mesh.parts[0]
                .positions
                .iter()
                .all(|position| position.iter().all(|c| c.is_finite()))
        );
    }
}
