//! Triangle geometry: robot links, and anything else with a surface.
//!
//! Geometry is uploaded once and kept, keyed by an id the caller chooses,
//! because a robot's meshes do not change — only the transforms that place
//! them do, and those are a few dozen matrices a frame rather than megabytes.

use std::sync::Arc;

use crate::camera::Mat4;

/// One vertex of a lit surface.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MeshVertex {
    pub position: [f32; 3],
    _pad: f32,
    pub normal: [f32; 3],
    _pad2: f32,
    pub color: [f32; 4],
}

impl MeshVertex {
    pub fn new(position: [f32; 3], normal: [f32; 3], color: [f32; 4]) -> Self {
        Self {
            position,
            _pad: 0.,
            normal,
            _pad2: 0.,
            color,
        }
    }
}

/// One drawable piece, and where it currently is.
#[derive(Debug, Clone, PartialEq)]
pub struct Solid {
    /// Identifies the geometry so it is uploaded once. Two solids sharing a key
    /// must have identical vertices — it is what the buffer cache is keyed on.
    pub key: u64,
    /// Shared, so re-posing a robot each frame costs a matrix rather than a
    /// copy of its meshes.
    pub vertices: Arc<Vec<MeshVertex>>,
    /// Where the piece sits in the world.
    pub transform: Mat4,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mesh_vertex_is_laid_out_for_the_shader() {
        // Three vec4-aligned slots: position, normal, colour. The padding is
        // what keeps the attribute offsets in `lib.rs` correct.
        assert_eq!(std::mem::size_of::<MeshVertex>(), 48);
        let vertex = MeshVertex::new([1., 2., 3.], [0., 0., 1.], [1., 1., 1., 1.]);
        let bytes = bytemuck::bytes_of(&vertex);
        assert_eq!(&bytes[0..4], &1f32.to_ne_bytes());
        assert_eq!(&bytes[16..20], &0f32.to_ne_bytes());
        assert_eq!(&bytes[32..36], &1f32.to_ne_bytes());
    }
}
