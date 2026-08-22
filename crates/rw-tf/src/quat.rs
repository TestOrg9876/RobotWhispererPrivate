//! Rotations, as quaternions.
//!
//! `rw_assets::math` already has matrices, and they are the right thing to hand
//! a GPU — but they are the wrong thing to store a *sample* in. A transform
//! that arrives at 10 Hz has to be interpolated to the moment a scan was taken,
//! and blending two rotation matrices component by component gives a matrix
//! that is no longer a rotation: it shears, and everything hanging off that
//! frame quietly grows or shrinks. Quaternions blend along the arc between two
//! orientations instead, which is what `slerp` is.
//!
//! The convention here is the one `geometry_msgs/Quaternion` uses: `(x, y, z)`
//! is the vector part and `w` the scalar, and a rotation composes as
//! `outer * inner`.

/// A rotation. Unit length by construction of everything that builds one; a
/// non-unit quaternion arriving off the wire is normalised on the way in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quat {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

impl Default for Quat {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Quat {
    /// No rotation at all.
    pub const IDENTITY: Self = Self {
        x: 0.,
        y: 0.,
        z: 0.,
        w: 1.,
    };

    pub const fn new(x: f32, y: f32, z: f32, w: f32) -> Self {
        Self { x, y, z, w }
    }

    /// Reads the four components a `geometry_msgs/Quaternion` carries, and
    /// normalises them.
    ///
    /// Publishers get this wrong often enough that it is not worth trusting: an
    /// all-zero quaternion is the single most common malformed rotation on a
    /// ROS graph — it is what a default-constructed message holds — and it is
    /// taken as no rotation rather than as a field of NaNs.
    pub fn from_wire(x: f32, y: f32, z: f32, w: f32) -> Self {
        Self::new(x, y, z, w).normalized()
    }

    /// Rotation by `angle` radians about an axis through the origin.
    pub fn from_axis_angle(axis: [f32; 3], angle: f32) -> Self {
        let length = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
        if !length.is_finite() || length <= f32::EPSILON {
            return Self::IDENTITY;
        }
        let (sin, cos) = (angle / 2.).sin_cos();
        Self {
            x: axis[0] / length * sin,
            y: axis[1] / length * sin,
            z: axis[2] / length * sin,
            w: cos,
        }
    }

    /// The fixed-axis roll-pitch-yaw a URDF `<origin>` uses, as a quaternion.
    pub fn from_rpy(rpy: [f32; 3]) -> Self {
        let (sr, cr) = (rpy[0] / 2.).sin_cos();
        let (sp, cp) = (rpy[1] / 2.).sin_cos();
        let (sy, cy) = (rpy[2] / 2.).sin_cos();
        Self {
            x: sr * cp * cy - cr * sp * sy,
            y: cr * sp * cy + sr * cp * sy,
            z: cr * cp * sy - sr * sp * cy,
            w: cr * cp * cy + sr * sp * sy,
        }
    }

    pub fn dot(self, other: Self) -> f32 {
        self.x * other.x + self.y * other.y + self.z * other.z + self.w * other.w
    }

    pub fn length(self) -> f32 {
        self.dot(self).sqrt()
    }

    /// Unit length. A degenerate or non-finite quaternion becomes the identity
    /// rather than a division by zero that would poison everything downstream
    /// of it.
    pub fn normalized(self) -> Self {
        let length = self.length();
        if !length.is_finite() || length <= 1e-9 {
            return Self::IDENTITY;
        }
        Self {
            x: self.x / length,
            y: self.y / length,
            z: self.z / length,
            w: self.w / length,
        }
    }

    /// The opposite rotation. For a unit quaternion this is also the inverse.
    pub fn conjugate(self) -> Self {
        Self {
            x: -self.x,
            y: -self.y,
            z: -self.z,
            w: self.w,
        }
    }

    /// `self` applied *after* `inner`.
    pub fn multiply(self, inner: Self) -> Self {
        Self {
            x: self.w * inner.x + self.x * inner.w + self.y * inner.z - self.z * inner.y,
            y: self.w * inner.y - self.x * inner.z + self.y * inner.w + self.z * inner.x,
            z: self.w * inner.z + self.x * inner.y - self.y * inner.x + self.z * inner.w,
            w: self.w * inner.w - self.x * inner.x - self.y * inner.y - self.z * inner.z,
        }
    }

    /// Turns a vector by this rotation.
    pub fn rotate(self, vector: [f32; 3]) -> [f32; 3] {
        // v + 2 * q_vec × (q_vec × v + w · v): the same result as building the
        // matrix, at a third of the multiplies.
        let u = [self.x, self.y, self.z];
        let t = [
            2. * (u[1] * vector[2] - u[2] * vector[1]),
            2. * (u[2] * vector[0] - u[0] * vector[2]),
            2. * (u[0] * vector[1] - u[1] * vector[0]),
        ];
        [
            vector[0] + self.w * t[0] + u[1] * t[2] - u[2] * t[1],
            vector[1] + self.w * t[1] + u[2] * t[0] - u[0] * t[2],
            vector[2] + self.w * t[2] + u[0] * t[1] - u[1] * t[0],
        ]
    }

    /// Blends along the arc between two orientations.
    ///
    /// The short way round: `q` and `-q` are the same rotation, so a pair whose
    /// dot product is negative would otherwise be interpolated the long way and
    /// send a robot's wrist round the back of its arm. Very close orientations
    /// fall back to a straight blend, where the arc is shorter than the
    /// precision of the angle between them.
    pub fn slerp(self, other: Self, t: f32) -> Self {
        let t = t.clamp(0., 1.);
        let mut end = other;
        let mut cos = self.dot(other);
        if cos < 0. {
            end = Self {
                x: -end.x,
                y: -end.y,
                z: -end.z,
                w: -end.w,
            };
            cos = -cos;
        }

        const PARALLEL: f32 = 0.9995;
        if cos > PARALLEL {
            return Self {
                x: self.x + (end.x - self.x) * t,
                y: self.y + (end.y - self.y) * t,
                z: self.z + (end.z - self.z) * t,
                w: self.w + (end.w - self.w) * t,
            }
            .normalized();
        }

        let angle = cos.clamp(-1., 1.).acos();
        let sin = angle.sin();
        let (near, far) = (((1. - t) * angle).sin() / sin, (t * angle).sin() / sin);
        Self {
            x: self.x * near + end.x * far,
            y: self.y * near + end.y * far,
            z: self.z * near + end.z * far,
            w: self.w * near + end.w * far,
        }
        .normalized()
    }

    /// The rotation as a column-major 4×4, which is the same concrete type as
    /// `rw_assets::math::Mat4` and `rw_render::Mat4`.
    pub fn to_mat4(self) -> crate::Mat4 {
        let Self { x, y, z, w } = self;
        let (xx, yy, zz) = (x * x, y * y, z * z);
        let (xy, xz, yz) = (x * y, x * z, y * z);
        let (wx, wy, wz) = (w * x, w * y, w * z);
        [
            [1. - 2. * (yy + zz), 2. * (xy + wz), 2. * (xz - wy), 0.],
            [2. * (xy - wz), 1. - 2. * (xx + zz), 2. * (yz + wx), 0.],
            [2. * (xz + wy), 2. * (yz - wx), 1. - 2. * (xx + yy), 0.],
            [0., 0., 0., 1.],
        ]
    }

    /// The angle, in radians, between two orientations. Used by the tests, and
    /// by anything wanting to say how far a frame moved.
    pub fn angle_to(self, other: Self) -> f32 {
        (2. * self.dot(other).abs().clamp(0., 1.).acos()).abs()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::{FRAC_PI_2, PI};

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-5)
    }

    #[test]
    fn the_identity_leaves_a_vector_alone() {
        assert!(close(Quat::IDENTITY.rotate([1., 2., 3.]), [1., 2., 3.]));
    }

    #[test]
    fn a_quarter_turn_about_z_takes_x_to_y() {
        let quat = Quat::from_axis_angle([0., 0., 1.], FRAC_PI_2);
        assert!(close(quat.rotate([1., 0., 0.]), [0., 1., 0.]));
    }

    #[test]
    fn composing_applies_the_inner_rotation_first() {
        let yaw = Quat::from_axis_angle([0., 0., 1.], FRAC_PI_2);
        let roll = Quat::from_axis_angle([1., 0., 0.], FRAC_PI_2);
        // Roll takes y to z; the yaw then leaves z alone.
        assert!(close(yaw.multiply(roll).rotate([0., 1., 0.]), [0., 0., 1.]));
        // The other order: yaw takes y to -x, roll then leaves -x alone.
        assert!(close(
            roll.multiply(yaw).rotate([0., 1., 0.]),
            [-1., 0., 0.]
        ));
    }

    #[test]
    fn the_conjugate_undoes_the_rotation() {
        let quat = Quat::from_rpy([0.3, -0.7, 1.1]);
        let there_and_back = quat.conjugate().rotate(quat.rotate([1., 2., 3.]));
        assert!(close(there_and_back, [1., 2., 3.]), "{there_and_back:?}");
    }

    #[test]
    fn the_matrix_agrees_with_rotating_the_vector_directly() {
        let quat = Quat::from_rpy([0.4, 1.2, -0.8]);
        let matrix = quat.to_mat4();
        for point in [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.], [1., -2., 3.]] {
            let by_matrix = [
                matrix[0][0] * point[0] + matrix[1][0] * point[1] + matrix[2][0] * point[2],
                matrix[0][1] * point[0] + matrix[1][1] * point[1] + matrix[2][1] * point[2],
                matrix[0][2] * point[0] + matrix[1][2] * point[1] + matrix[2][2] * point[2],
            ];
            assert!(close(by_matrix, quat.rotate(point)), "{point:?}");
        }
    }

    #[test]
    fn rpy_matches_the_matrix_form_rw_assets_already_uses() {
        // The URDF convention, so a quaternion built here and a matrix built
        // there place a link identically.
        for rpy in [
            [0.3, 0., 0.],
            [0., -0.9, 0.],
            [0., 0., 2.1],
            [0.4, 1.1, -0.6],
        ] {
            let quat = Quat::from_rpy(rpy);
            let (sr, cr) = rpy[0].sin_cos();
            let (sp, cp) = rpy[1].sin_cos();
            let (sy, cy) = rpy[2].sin_cos();
            let expected = [
                [cy * cp, sy * cp, -sp],
                [cy * sp * sr - sy * cr, sy * sp * sr + cy * cr, cp * sr],
                [cy * sp * cr + sy * sr, sy * sp * cr - cy * sr, cp * cr],
            ];
            for (axis, column) in expected.iter().enumerate() {
                let mut unit = [0.; 3];
                unit[axis] = 1.;
                assert!(close(quat.rotate(unit), *column), "{rpy:?} axis {axis}");
            }
        }
    }

    #[test]
    fn slerp_stays_on_the_unit_sphere_all_the_way_across() {
        let start = Quat::from_axis_angle([0., 0., 1.], 0.);
        let end = Quat::from_axis_angle([0., 0., 1.], 2.5);
        for step in 0..=20 {
            let blend = start.slerp(end, step as f32 / 20.);
            assert!(
                (blend.length() - 1.).abs() < 1e-5,
                "length {} at step {step}",
                blend.length()
            );
        }
    }

    #[test]
    fn slerp_halfway_is_the_half_angle_not_the_average_of_the_components() {
        // The distinguishing case: a lerp of these two would land well short of
        // 45°, which is exactly the error that makes a robot's arm look bent.
        let start = Quat::IDENTITY;
        let end = Quat::from_axis_angle([0., 0., 1.], FRAC_PI_2);
        let middle = start.slerp(end, 0.5);
        let half_angle = FRAC_PI_2 / 2.;
        assert!(close(
            middle.rotate([1., 0., 0.]),
            [half_angle.cos(), half_angle.sin(), 0.]
        ));
    }

    #[test]
    fn slerp_takes_the_short_way_round() {
        let start = Quat::from_axis_angle([0., 0., 1.], 0.);
        // The same rotation as `start`, written with every sign flipped.
        let negated = Quat::new(-start.x, -start.y, -start.z, -start.w);
        let middle = start.slerp(negated, 0.5);
        assert!(
            middle.angle_to(start) < 1e-3,
            "blending a rotation with itself moved it by {} rad",
            middle.angle_to(start)
        );

        // And across a genuine 170°, the blend must not go the 190° way.
        let far = Quat::from_axis_angle([0., 0., 1.], PI * 170. / 180.);
        let flipped = Quat::new(-far.x, -far.y, -far.z, -far.w);
        let short = Quat::IDENTITY.slerp(far, 0.5);
        let same = Quat::IDENTITY.slerp(flipped, 0.5);
        assert!(
            short.angle_to(same) < 1e-3,
            "the sign of the target changed which way round the blend went"
        );
    }

    #[test]
    fn slerp_ends_are_the_endpoints() {
        let start = Quat::from_rpy([0.1, 0.2, 0.3]);
        let end = Quat::from_rpy([-1.1, 0.9, 2.2]);
        assert!(start.slerp(end, 0.).angle_to(start) < 1e-5);
        assert!(start.slerp(end, 1.).angle_to(end) < 1e-5);
    }

    #[test]
    fn a_blend_beyond_the_ends_is_clamped_rather_than_extrapolated() {
        let start = Quat::IDENTITY;
        let end = Quat::from_axis_angle([0., 0., 1.], 1.);
        assert!(start.slerp(end, 5.).angle_to(end) < 1e-5);
        assert!(start.slerp(end, -5.).angle_to(start) < 1e-5);
    }

    #[test]
    fn a_default_constructed_message_is_no_rotation_rather_than_nans() {
        // All-zero is what an unfilled `geometry_msgs/Quaternion` holds, and it
        // is on more ROS graphs than anyone would like.
        let quat = Quat::from_wire(0., 0., 0., 0.);
        assert_eq!(quat, Quat::IDENTITY);
        assert!(close(quat.rotate([1., 2., 3.]), [1., 2., 3.]));
    }

    #[test]
    fn an_unnormalised_quaternion_off_the_wire_is_normalised() {
        let quat = Quat::from_wire(0., 0., 3., 3.);
        assert!((quat.length() - 1.).abs() < 1e-6);
        assert!(close(quat.rotate([1., 0., 0.]), [0., 1., 0.]));
    }

    #[test]
    fn a_non_finite_quaternion_is_refused_rather_than_spread() {
        assert_eq!(Quat::from_wire(f32::NAN, 0., 0., 1.), Quat::IDENTITY);
        assert_eq!(
            Quat::from_axis_angle([f32::INFINITY, 0., 0.], 1.),
            Quat::IDENTITY
        );
    }

    #[test]
    fn a_degenerate_axis_is_no_rotation() {
        assert_eq!(Quat::from_axis_angle([0., 0., 0.], 1.5), Quat::IDENTITY);
    }
}
