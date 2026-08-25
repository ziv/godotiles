//! # godotiles
//!
//! Geo-spatial terrain streaming for Godot 4 — a GDExtension port of the
//! [raytiles](https://github.com/ziv/raytiles) / [bevytiles](https://github.com/ziv/bevytiles)
//! engine. Streams satellite imagery, Terrarium heightmaps and normal maps around a moving
//! camera and renders them as GPU-displaced terrain.
//!
//! - [`core`] — engine-agnostic: config, LOD policy, synthesis, height grids, source, store.
//! - [`godot`] — the Godot shell: `TerrainStreamer` node, config resources, GPU resources.

pub mod core;
pub mod godot;

use ::godot::prelude::*;

struct GodotilesExtension;

#[gdextension]
unsafe impl ExtensionLibrary for GodotilesExtension {}
