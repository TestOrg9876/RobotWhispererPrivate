//! Robot descriptions and the meshes they name.
//!
//! Everything here is parsing and arithmetic: nothing touches a GPU, and
//! nothing here decides how a robot looks — only what shape it is and where its
//! parts are. `rw-render` draws what this produces.

pub mod catalog;
pub mod collada;
pub mod kinematics;
pub mod math;
pub mod mesh;
pub mod obj;
pub mod shapes;
pub mod urdf;
