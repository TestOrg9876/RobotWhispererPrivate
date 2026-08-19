//! An orbit camera, and the matrices it produces.
//!
//! Pure arithmetic, deliberately kept apart from anything that touches a GPU:
//! this is the part that is wrong in ways a screenshot cannot show, so it is
//! the part with tests.

/// A 4×4 matrix in column-major order, which is what WGSL expects.
pub type Mat4 = [[f32; 4]; 4];

/// The transform that changes nothing, for a solid that needs no placing.
pub const IDENTITY: Mat4 = [
    [1., 0., 0., 0.],
    [0., 1., 0., 0.],
    [0., 0., 1., 0.],
    [0., 0., 0., 1.],
];

/// A camera that looks at a point and orbits around it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    /// The point being looked at.
    pub target: [f32; 3],
    /// Distance from the target.
    pub distance: f32,
    /// Rotation about the up axis, in radians.
    pub yaw: f32,
    /// Elevation above the horizontal plane, in radians.
    pub pitch: f32,
    /// Vertical field of view, in radians.
    pub fov: f32,
}

/// How close to straight up or down the camera may be pointed.
///
/// Exactly overhead, the view direction and the up axis are parallel and the
/// view matrix has no defined orientation — the picture spins on its own.
const PITCH_LIMIT: f32 = std::f32::consts::FRAC_PI_2 - 0.01;

/// ROS is Z-up: x forward, y left, z up. A viewer that assumed Y-up would show
/// every robot lying on its side.
const UP: [f32; 3] = [0., 0., 1.];

impl Default for Camera {
    fn default() -> Self {
        Self {
            target: [0., 0., 0.],
            distance: 8.,
            // Looking from behind and above, which is how a robot is usually
            // drawn on first sight.
            yaw: -std::f32::consts::FRAC_PI_4,
            pitch: 0.45,
            fov: std::f32::consts::FRAC_PI_4,
        }
    }
}

impl Camera {
    /// Where the camera is, derived from the orbit rather than stored, so the
    /// two can never disagree.
    pub fn eye(&self) -> [f32; 3] {
        let (sin_pitch, cos_pitch) = self.pitch.sin_cos();
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        [
            self.target[0] + self.distance * cos_pitch * cos_yaw,
            self.target[1] + self.distance * cos_pitch * sin_yaw,
            self.target[2] + self.distance * sin_pitch,
        ]
    }

    /// Turns the camera by a drag, in radians.
    pub fn orbit(&mut self, delta_yaw: f32, delta_pitch: f32) {
        self.yaw += delta_yaw;
        self.pitch = (self.pitch + delta_pitch).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }

    /// Moves the camera towards or away from the target.
    ///
    /// Multiplicative, so one wheel click covers the same proportion of the
    /// distance whether the camera is a metre away or a hundred.
    pub fn zoom(&mut self, factor: f32) {
        self.distance = (self.distance * factor).clamp(0.05, 5000.);
    }

    /// Frames an axis-aligned box: centres on it and backs off far enough to
    /// see all of it.
    pub fn frame(&mut self, min: [f32; 3], max: [f32; 3]) {
        self.target = [
            (min[0] + max[0]) / 2.,
            (min[1] + max[1]) / 2.,
            (min[2] + max[2]) / 2.,
        ];
        let extent = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
        let radius = (extent[0] * extent[0] + extent[1] * extent[1] + extent[2] * extent[2])
            .sqrt()
            .max(0.1)
            / 2.;
        // The half-angle is what the radius has to fit inside, and a little
        // margin keeps the outermost points off the edge of the pane.
        self.distance = (radius / (self.fov / 2.).tan() * 1.2).clamp(0.05, 5000.);
    }

    /// The combined view-projection matrix for a pane of this shape.
    pub fn view_projection(&self, aspect: f32) -> Mat4 {
        multiply(
            perspective(self.fov, aspect.max(0.01), 0.05, 10_000.),
            look_at(self.eye(), self.target, UP),
        )
    }
}

/// A right-handed look-at matrix.
pub fn look_at(eye: [f32; 3], target: [f32; 3], up: [f32; 3]) -> Mat4 {
    let forward = normalize(sub(target, eye));
    let right = normalize(cross(forward, up));
    let true_up = cross(right, forward);
    [
        [right[0], true_up[0], -forward[0], 0.],
        [right[1], true_up[1], -forward[1], 0.],
        [right[2], true_up[2], -forward[2], 0.],
        [-dot(right, eye), -dot(true_up, eye), dot(forward, eye), 1.],
    ]
}

/// A perspective matrix mapping depth to 0..1, which is what wgpu clips against
/// — the OpenGL -1..1 convention would throw away the near half of the scene.
pub fn perspective(fov: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
    let focal = 1. / (fov / 2.).tan();
    [
        [focal / aspect, 0., 0., 0.],
        [0., focal, 0., 0.],
        [0., 0., far / (near - far), -1.],
        [0., 0., near * far / (near - far), 0.],
    ]
}

/// Applies a transform to a point.
pub fn transform_point(matrix: Mat4, point: [f32; 3]) -> [f32; 3] {
    let mut out = [0.; 3];
    for (row, slot) in out.iter_mut().enumerate() {
        *slot = matrix[0][row] * point[0]
            + matrix[1][row] * point[1]
            + matrix[2][row] * point[2]
            + matrix[3][row];
    }
    out
}

pub fn multiply(a: Mat4, b: Mat4) -> Mat4 {
    let mut out = [[0.; 4]; 4];
    for (column, source) in b.iter().enumerate() {
        for row in 0..4 {
            out[column][row] = (0..4).map(|k| a[k][row] * source[k]).sum();
        }
    }
    out
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let length = dot(v, v).sqrt();
    if length <= f32::EPSILON {
        return [0., 0., 1.];
    }
    [v[0] / length, v[1] / length, v[2] / length]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Applies a matrix to a point, the way the vertex shader will.
    fn project(matrix: Mat4, point: [f32; 3]) -> [f32; 4] {
        let mut out = [0.; 4];
        for row in 0..4 {
            out[row] = matrix[0][row] * point[0]
                + matrix[1][row] * point[1]
                + matrix[2][row] * point[2]
                + matrix[3][row];
        }
        out
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn the_eye_sits_at_the_orbit_distance_from_the_target() {
        let camera = Camera {
            target: [1., 2., 3.],
            distance: 10.,
            ..Camera::default()
        };
        let eye = camera.eye();
        let offset = sub(eye, camera.target);
        assert!(close(dot(offset, offset).sqrt(), 10.), "got {eye:?}");
    }

    #[test]
    fn pitch_never_reaches_straight_up() {
        let mut camera = Camera::default();
        camera.orbit(0., 100.);
        assert!(camera.pitch < std::f32::consts::FRAC_PI_2);
        camera.orbit(0., -100.);
        assert!(camera.pitch > -std::f32::consts::FRAC_PI_2);
    }

    #[test]
    fn yaw_is_free_to_turn_all_the_way_round() {
        let mut camera = Camera {
            yaw: 0.,
            ..Camera::default()
        };
        camera.orbit(std::f32::consts::TAU, 0.);
        assert!(close(camera.yaw, std::f32::consts::TAU));
    }

    #[test]
    fn zooming_is_proportional_and_bounded() {
        let mut camera = Camera {
            distance: 10.,
            ..Camera::default()
        };
        camera.zoom(0.5);
        assert!(close(camera.distance, 5.));
        for _ in 0..200 {
            camera.zoom(0.5);
        }
        assert!(camera.distance >= 0.05, "got {}", camera.distance);
        for _ in 0..200 {
            camera.zoom(2.);
        }
        assert!(camera.distance <= 5000., "got {}", camera.distance);
    }

    #[test]
    fn the_target_is_at_the_centre_of_the_view() {
        let camera = Camera::default();
        let clip = project(camera.view_projection(1.5), camera.target);
        assert!(close(clip[0] / clip[3], 0.), "x was {}", clip[0] / clip[3]);
        assert!(close(clip[1] / clip[3], 0.), "y was {}", clip[1] / clip[3]);
    }

    #[test]
    fn depth_lands_in_the_range_wgpu_clips_against() {
        let matrix = perspective(std::f32::consts::FRAC_PI_4, 1., 1., 100.);
        let near = project(matrix, [0., 0., -1.]);
        let far = project(matrix, [0., 0., -100.]);
        assert!(
            close(near[2] / near[3], 0.),
            "near was {}",
            near[2] / near[3]
        );
        assert!(close(far[2] / far[3], 1.), "far was {}", far[2] / far[3]);
    }

    #[test]
    fn a_framed_box_fits_inside_the_view() {
        let mut camera = Camera::default();
        camera.frame([-2., -2., 0.], [2., 2., 1.]);
        assert!(close(camera.target[0], 0.) && close(camera.target[1], 0.));
        let matrix = camera.view_projection(1.);
        for corner in [[-2., -2., 0.], [2., 2., 1.], [-2., 2., 1.], [2., -2., 0.]] {
            let clip = project(matrix, corner);
            assert!(clip[3] > 0., "corner {corner:?} ended up behind the camera");
            let (x, y) = (clip[0] / clip[3], clip[1] / clip[3]);
            assert!(
                x.abs() <= 1. && y.abs() <= 1.,
                "corner {corner:?} projected to {x}, {y}, outside the pane"
            );
        }
    }

    #[test]
    fn framing_a_single_point_still_produces_a_usable_distance() {
        let mut camera = Camera::default();
        camera.frame([1., 1., 1.], [1., 1., 1.]);
        assert!(camera.distance >= 0.05, "got {}", camera.distance);
        assert!(camera.distance.is_finite());
    }

    #[test]
    fn the_camera_is_z_up() {
        // A point directly above the target must project above its centre, not
        // to one side: the whole ROS convention rests on this.
        let camera = Camera {
            target: [0., 0., 0.],
            pitch: 0.,
            yaw: 0.,
            ..Camera::default()
        };
        let clip = project(camera.view_projection(1.), [0., 0., 1.]);
        assert!(clip[1] / clip[3] > 0.1, "up was not up: {clip:?}");
    }

    #[test]
    fn multiplying_by_the_identity_changes_nothing() {
        let identity = [
            [1., 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., 1., 0.],
            [0., 0., 0., 1.],
        ];
        let matrix = perspective(1., 1.5, 0.1, 50.);
        assert_eq!(multiply(identity, matrix), matrix);
        assert_eq!(multiply(matrix, identity), matrix);
    }
}
