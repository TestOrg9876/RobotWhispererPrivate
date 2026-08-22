//! The transform tree.
//!
//! Every 3D thing a robot publishes arrives in some frame of its own: a scan in
//! `laser_link`, a map in `map`, an arm in `base_link`. Drawing them together
//! means knowing where each of those frames was at the moment its message was
//! stamped — that is all TF is, and it is the difference between a scene and a
//! pile of things at the origin.
//!
//! Parsing and arithmetic only, in the shape of `rw-assets`: no GPU, no UI, no
//! async, no clock of its own. Times arrive as nanoseconds from the caller.

pub mod buffer;
pub mod mat4;
pub mod quat;

pub use buffer::{Buffer, DEFAULT_WINDOW_NS, LATEST, MAX_SAMPLES, Node, Side, TfError, Transform};
pub use mat4::{IDENTITY, Mat4, multiply, transform_point};
pub use quat::Quat;
