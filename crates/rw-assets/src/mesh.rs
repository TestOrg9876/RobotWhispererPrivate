//! What a loaded mesh looks like, whatever file it came out of.

use crate::math::{self, Mat4};

/// One run of triangles sharing a material.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Part {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    /// The material's name in the file, which is what `robots.config.json`
    /// matches its presets against.
    pub material: Option<String>,
    /// The diffuse colour the file gave the material, if any.
    pub color: Option<[f32; 4]>,
}

impl Part {
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Bakes a transform into the geometry.
    ///
    /// Mesh files carry their own units and axis convention in a scene node,
    /// and a robot link expects metres in its own frame. Applying it once at
    /// load time costs nothing per frame afterwards.
    pub fn transformed(mut self, matrix: Mat4) -> Self {
        for position in &mut self.positions {
            *position = math::transform_point(matrix, *position);
        }
        for normal in &mut self.normals {
            *normal = normalize(math::transform_direction(matrix, *normal));
        }
        self
    }
}

/// Everything one mesh file holds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mesh {
    pub parts: Vec<Part>,
}

impl Mesh {
    pub fn is_empty(&self) -> bool {
        self.parts.iter().all(|part| part.indices.is_empty())
    }

    pub fn triangle_count(&self) -> usize {
        self.parts.iter().map(Part::triangle_count).sum()
    }

    /// The box the mesh occupies, for framing a camera on it.
    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        let mut any = false;
        for position in self.parts.iter().flat_map(|part| &part.positions) {
            any = true;
            for axis in 0..3 {
                min[axis] = min[axis].min(position[axis]);
                max[axis] = max[axis].max(position[axis]);
            }
        }
        any.then_some((min, max))
    }
}

pub(crate) fn normalize(v: [f32; 3]) -> [f32; 3] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if length <= f32::EPSILON {
        return [0., 0., 1.];
    }
    [v[0] / length, v[1] / length, v[2] / length]
}

/// The normal of a triangle, for files that carry none of their own.
pub(crate) fn face_normal(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> [f32; 3] {
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    normalize([
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn triangle() -> Part {
        Part {
            positions: vec![[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]],
            normals: vec![[0., 0., 1.]; 3],
            indices: vec![0, 1, 2],
            ..Part::default()
        }
    }

    #[test]
    fn triangles_are_counted_from_the_indices() {
        assert_eq!(triangle().triangle_count(), 1);
        let mesh = Mesh {
            parts: vec![triangle(), triangle()],
        };
        assert_eq!(mesh.triangle_count(), 2);
    }

    #[test]
    fn a_transform_moves_the_positions_and_turns_the_normals() {
        let scaled = triangle().transformed(math::multiply(
            math::translation([0., 0., 5.]),
            math::from_rpy([std::f32::consts::FRAC_PI_2, 0., 0.]),
        ));
        assert_eq!(scaled.positions[0], [0., 0., 5.]);
        // The face normal turned with the mesh, and stayed a unit vector.
        assert!(
            (scaled.normals[0][1] + 1.).abs() < 1e-5,
            "{:?}",
            scaled.normals[0]
        );
    }

    #[test]
    fn scaling_leaves_normals_normalised() {
        let scaled = triangle().transformed(math::scale([0.001, 0.001, 0.001]));
        let normal = scaled.normals[0];
        let length = (normal[0].powi(2) + normal[1].powi(2) + normal[2].powi(2)).sqrt();
        assert!((length - 1.).abs() < 1e-5, "got {length}");
    }

    #[test]
    fn bounds_cover_every_part() {
        let mesh = Mesh {
            parts: vec![
                triangle(),
                triangle().transformed(math::translation([10., 0., 0.])),
            ],
        };
        assert_eq!(mesh.bounds(), Some(([0., 0., 0.], [11., 1., 0.])));
    }

    #[test]
    fn an_empty_mesh_has_no_bounds_rather_than_an_infinite_one() {
        assert_eq!(Mesh::default().bounds(), None);
        assert!(Mesh::default().is_empty());
    }

    #[test]
    fn a_face_normal_points_out_of_a_counter_clockwise_triangle() {
        let normal = face_normal([0., 0., 0.], [1., 0., 0.], [0., 1., 0.]);
        assert!((normal[2] - 1.).abs() < 1e-6, "got {normal:?}");
    }

    #[test]
    fn a_degenerate_triangle_yields_a_usable_normal_rather_than_nans() {
        let normal = face_normal([0., 0., 0.], [0., 0., 0.], [0., 0., 0.]);
        assert!(normal.iter().all(|component| component.is_finite()));
    }
}
