//! The 4×4 transform, and the two operations everything does to one.
//!
//! Column-major, which is what the renderer's uniform buffers want and what
//! `Quat::to_mat4` has always produced.
//!
//! Here rather than in the renderer or the asset loader because both had a
//! copy — character for character, down to the loop variable names — and
//! `rw-ui` called whichever one came to hand at each site. This crate has no
//! dependencies of its own, so both can reach it without either reaching the
//! other: an asset loader does not want wgpu, and a renderer does not want a
//! URDF parser.

pub type Mat4 = [[f32; 4]; 4];

pub const IDENTITY: Mat4 = [
    [1., 0., 0., 0.],
    [0., 1., 0., 0.],
    [0., 0., 1., 0.],
    [0., 0., 0., 1.],
];

pub fn multiply(a: Mat4, b: Mat4) -> Mat4 {
    let mut out = [[0.; 4]; 4];
    for (column, source) in b.iter().enumerate() {
        for row in 0..4 {
            out[column][row] = (0..4).map(|k| a[k][row] * source[k]).sum();
        }
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identity_leaves_a_point_alone() {
        assert_eq!(transform_point(IDENTITY, [1., -2., 3.]), [1., -2., 3.]);
        assert_eq!(multiply(IDENTITY, IDENTITY), IDENTITY);
    }

    #[test]
    fn translation_lands_in_the_last_column() {
        // Column-major: the translation is `m[3]`, not `m[*][3]`. Getting this
        // backwards transposes every pose in the app and still renders
        // something, which is why it is pinned here.
        let mut shift = IDENTITY;
        shift[3] = [10., 20., 30., 1.];
        assert_eq!(transform_point(shift, [1., 2., 3.]), [11., 22., 33.]);
    }

    #[test]
    fn multiply_applies_the_right_hand_first() {
        let mut first = IDENTITY;
        first[3] = [1., 0., 0., 1.];
        let mut second = IDENTITY;
        second[3] = [0., 5., 0., 1.];

        // `multiply(second, first)` means "second ∘ first": first moves along
        // x, then second moves along y.
        let composed = multiply(second, first);
        assert_eq!(transform_point(composed, [0., 0., 0.]), [1., 5., 0.]);
    }
}
