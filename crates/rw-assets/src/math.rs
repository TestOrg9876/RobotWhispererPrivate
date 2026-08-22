//! The little linear algebra a robot description needs.
//!
//! Column-major 4×4 matrices, matching WGSL and `rw_render::Mat4`, so a pose
//! computed here can be handed to the renderer without conversion.

/// A column-major 4×4 matrix: `m[column][row]`.
/// The 4×4 matrix and the two operations on it, from `rw-tf`. This module had
/// its own identical copy, as did `rw-render::camera`, and `rw-ui` called
/// whichever came to hand. The rest of this file is URDF-specific and stays.
pub use rw_tf::{IDENTITY, Mat4, multiply, transform_point};

/// Applies the rotation part only, which is what a normal needs.
///
/// Correct as long as the transform has no non-uniform scale — true of every
/// URDF joint, which is why this is not the inverse transpose.
pub fn transform_direction(matrix: Mat4, direction: [f32; 3]) -> [f32; 3] {
    let mut out = [0.; 3];
    for (row, slot) in out.iter_mut().enumerate() {
        *slot = matrix[0][row] * direction[0]
            + matrix[1][row] * direction[1]
            + matrix[2][row] * direction[2];
    }
    out
}

pub fn translation(xyz: [f32; 3]) -> Mat4 {
    let mut out = IDENTITY;
    out[3] = [xyz[0], xyz[1], xyz[2], 1.];
    out
}

pub fn scale(factor: [f32; 3]) -> Mat4 {
    let mut out = IDENTITY;
    out[0][0] = factor[0];
    out[1][1] = factor[1];
    out[2][2] = factor[2];
    out
}

/// URDF's fixed-axis roll-pitch-yaw: roll about x, then pitch about y, then
/// yaw about z, each about the *original* axes — which composes as Rz·Ry·Rx.
pub fn from_rpy(rpy: [f32; 3]) -> Mat4 {
    let (sr, cr) = rpy[0].sin_cos();
    let (sp, cp) = rpy[1].sin_cos();
    let (sy, cy) = rpy[2].sin_cos();
    [
        [cy * cp, sy * cp, -sp, 0.],
        [cy * sp * sr - sy * cr, sy * sp * sr + cy * cr, cp * sr, 0.],
        [cy * sp * cr + sy * sr, sy * sp * cr - cy * sr, cp * cr, 0.],
        [0., 0., 0., 1.],
    ]
}

/// A URDF `<origin>`: a translation and an rpy rotation, in that order.
pub fn from_origin(xyz: [f32; 3], rpy: [f32; 3]) -> Mat4 {
    multiply(translation(xyz), from_rpy(rpy))
}

/// Rotation by `angle` radians about an arbitrary axis through the origin,
/// which is what a revolute joint does.
pub fn from_axis_angle(axis: [f32; 3], angle: f32) -> Mat4 {
    let length = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
    if length <= f32::EPSILON {
        return IDENTITY;
    }
    let [x, y, z] = [axis[0] / length, axis[1] / length, axis[2] / length];
    let (sin, cos) = angle.sin_cos();
    let t = 1. - cos;
    [
        [
            t * x * x + cos,
            t * x * y + sin * z,
            t * x * z - sin * y,
            0.,
        ],
        [
            t * x * y - sin * z,
            t * y * y + cos,
            t * y * z + sin * x,
            0.,
        ],
        [
            t * x * z + sin * y,
            t * y * z - sin * x,
            t * z * z + cos,
            0.,
        ],
        [0., 0., 0., 1.],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-5)
    }

    #[test]
    fn the_identity_leaves_a_point_alone() {
        assert_eq!(transform_point(IDENTITY, [1., 2., 3.]), [1., 2., 3.]);
    }

    #[test]
    fn translation_moves_points_but_not_directions() {
        let matrix = translation([10., 0., 0.]);
        assert!(close(transform_point(matrix, [1., 2., 3.]), [11., 2., 3.]));
        assert!(close(
            transform_direction(matrix, [1., 0., 0.]),
            [1., 0., 0.]
        ));
    }

    #[test]
    fn yaw_turns_x_towards_y() {
        let matrix = from_rpy([0., 0., FRAC_PI_2]);
        assert!(close(transform_point(matrix, [1., 0., 0.]), [0., 1., 0.]));
    }

    #[test]
    fn pitch_turns_x_towards_minus_z() {
        let matrix = from_rpy([0., FRAC_PI_2, 0.]);
        assert!(close(transform_point(matrix, [1., 0., 0.]), [0., 0., -1.]));
    }

    #[test]
    fn roll_turns_y_towards_z() {
        let matrix = from_rpy([FRAC_PI_2, 0., 0.]);
        assert!(close(transform_point(matrix, [0., 1., 0.]), [0., 0., 1.]));
    }

    #[test]
    fn rpy_is_applied_about_the_fixed_axes_not_the_moving_ones() {
        // Roll then yaw. Fixed-axis (Rz·Ry·Rx) sends z to +x; the moving-axis
        // reading (Rx·Ry·Rz) would send it to -y, so this tells them apart.
        let matrix = from_rpy([FRAC_PI_2, 0., FRAC_PI_2]);
        assert!(
            close(transform_point(matrix, [0., 0., 1.]), [1., 0., 0.]),
            "got {:?}",
            transform_point(matrix, [0., 0., 1.])
        );
    }

    #[test]
    fn an_origin_rotates_before_it_translates() {
        // The URDF convention: the child frame is rotated in place, then moved.
        let matrix = from_origin([5., 0., 0.], [0., 0., FRAC_PI_2]);
        assert!(close(transform_point(matrix, [1., 0., 0.]), [5., 1., 0.]));
    }

    #[test]
    fn a_revolute_joint_turns_about_its_own_axis() {
        let matrix = from_axis_angle([0., 0., 1.], FRAC_PI_2);
        assert!(close(transform_point(matrix, [1., 0., 0.]), [0., 1., 0.]));
        let matrix = from_axis_angle([0., 1., 0.], FRAC_PI_2);
        assert!(close(transform_point(matrix, [0., 0., 1.]), [1., 0., 0.]));
    }

    #[test]
    fn an_unnormalised_axis_is_normalised_rather_than_scaling_the_rotation() {
        let long = from_axis_angle([0., 0., 7.], FRAC_PI_2);
        let unit = from_axis_angle([0., 0., 1.], FRAC_PI_2);
        for column in 0..4 {
            for row in 0..4 {
                assert!((long[column][row] - unit[column][row]).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn a_degenerate_axis_is_no_rotation_rather_than_a_field_of_nans() {
        assert_eq!(from_axis_angle([0., 0., 0.], 1.), IDENTITY);
    }

    #[test]
    fn composing_transforms_applies_the_right_one_first() {
        let matrix = multiply(translation([0., 0., 1.]), from_rpy([0., 0., FRAC_PI_2]));
        assert!(close(transform_point(matrix, [1., 0., 0.]), [0., 1., 1.]));
    }

    #[test]
    fn scaling_a_mesh_scales_its_points() {
        assert!(close(
            transform_point(scale([2., 3., 4.]), [1., 1., 1.]),
            [2., 3., 4.]
        ));
    }
}
