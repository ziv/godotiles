//! Engine-agnostic core: configuration, LOD policy, terrain synthesis, height grids, frustum
//! test, the tile source (worker pool) and the tile lifecycle policy.
//!
//! **Rule:** nothing in this module tree may import `godot::`. This is the future shared core
//! for the raylib / Bevy / Godot / three.js ports.

pub mod config;
pub mod frustum;
pub mod height;
pub mod lod;
pub mod source;
pub mod store;
pub mod synth;
