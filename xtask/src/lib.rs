//! The parts of `xtask` that are worth testing on their own.
//!
//! The load bridge's payload encoders live behind a library target so
//! `tests/load_shapes_decode.rs` can put them through the app's real decoders.
//! A wrong encoding is otherwise invisible until a benchmark run shows an empty
//! pane, which is how the first set of point-cloud numbers came to be
//! meaningless.

pub mod load_bridge;
pub mod load_shapes;
