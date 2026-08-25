//! The four configuration resources exposed to Godot. Each mirrors a plain Rust config struct
//! in `core::config` and converts to it with `to_core()`.

use crate::core::config::{
    geo_anchor, NetworkConfig, RenderingConfig, StreamingConfig, WorldConfig,
    DEFAULT_HEIGHTMAP_URL, DEFAULT_NORMALS_URL, DEFAULT_TEXTURE_URL, DEFAULT_THRESHOLDS,
    ZOOM_LEVELS,
};
use godot::classes::{IResource, ProjectSettings, Resource};
use godot::prelude::*;
use std::path::PathBuf;
use std::time::Duration;

fn packed(values: &[f32]) -> PackedFloat32Array {
    PackedFloat32Array::from(values)
}

/// Copy up to 14 slots from a packed array; missing slots fall back to `defaults` (never to 0).
fn per_zoom(values: &PackedFloat32Array, defaults: &[f32; ZOOM_LEVELS]) -> [f32; ZOOM_LEVELS] {
    let mut out = *defaults;
    for (i, slot) in out.iter_mut().enumerate() {
        if let Some(v) = values.as_slice().get(i) {
            *slot = *v;
        }
    }
    out
}

fn vec3(v: Vector3) -> glam::Vec3 {
    glam::Vec3::new(v.x, v.y, v.z)
}

fn color(c: Color) -> [f32; 4] {
    [c.r, c.g, c.b, c.a]
}

/// World topology: anchor tile, zoom range, tile size, mesh overlap. Effectively immutable once
/// streaming started (assigning a new resource to the streamer rebuilds it).
#[derive(GodotClass)]
#[class(init, base = Resource)]
pub struct TerrainWorldConfig {
    /// Anchor tile X at `base_zoom`: the world origin sits at this tile's corner.
    #[export]
    #[init(val = 306)]
    pub anchor_x: i32,
    /// Anchor tile row (slippy-map `y`) at `base_zoom`.
    #[export]
    #[init(val = 207)]
    pub anchor_z: i32,
    /// Lowest LOD zoom ever loaded.
    #[export(range = (9.0, 22.0))]
    #[init(val = 9)]
    pub base_zoom: i32,
    /// Highest LOD zoom. Above 15 heightmaps are synthesized and normals default.
    #[export(range = (9.0, 22.0))]
    #[init(val = 15)]
    pub max_zoom: i32,
    /// World size (meters) of one tile at `base_zoom`.
    #[export]
    #[init(val = 66_400.0)]
    pub tile_size: f32,
    /// Per-zoom mesh overlap factors (14 slots, `[zoom - base_zoom]`). Must be > 0.
    #[export]
    #[init(val = packed(&[1.0; ZOOM_LEVELS]))]
    pub skirt_overlap: PackedFloat32Array,
    /// Generate mipmaps for the albedo texture.
    #[export]
    #[init(val = true)]
    pub mipmaps: bool,
    /// Offset of the anchor point inside its anchor tile (filled by `from_lat_lon`).
    #[export]
    #[init(val = Vector3::ZERO)]
    pub origin_offset: Vector3,
    base: Base<Resource>,
}

#[godot_api]
impl TerrainWorldConfig {
    /// Anchor the world at a geographic coordinate (degrees): derives the anchor tile, tile
    /// size and origin offset so the world origin sits exactly on the coordinate.
    #[func]
    pub fn from_lat_lon(lat: f64, lon: f64) -> Gd<Self> {
        let g = geo_anchor(lat, lon);
        let mut res = Self::new_gd();
        {
            let mut r = res.bind_mut();
            r.anchor_x = g.anchor_x;
            r.anchor_z = g.anchor_z;
            r.tile_size = g.tile_size as f32;
            r.origin_offset = Vector3::new(g.origin_offset.x, 0.0, g.origin_offset.z);
        }
        res
    }

    /// A sensible initial camera position over the anchor, `altitude` meters up.
    #[func]
    pub fn initial_position(&self, altitude: f32) -> Vector3 {
        self.origin_offset + Vector3::new(0.0, altitude, 0.0)
    }

    pub fn to_core(&self) -> WorldConfig {
        WorldConfig {
            anchor_x: self.anchor_x,
            anchor_z: self.anchor_z,
            base_zoom: self.base_zoom.clamp(0, 255) as u8,
            max_zoom: self.max_zoom.clamp(0, 255) as u8,
            tile_size: self.tile_size,
            skirt_overlap: per_zoom(&self.skirt_overlap, &[1.0; ZOOM_LEVELS]),
            mipmaps: self.mipmaps,
            origin_offset: vec3(self.origin_offset),
        }
    }
}

/// Which tiles are kept resident and how aggressively the set updates.
#[derive(GodotClass)]
#[class(init, base = Resource)]
pub struct TerrainStreamingConfig {
    /// Radius, in base-zoom tiles, of the disc loaded around the camera.
    #[export(range = (1.0, 32.0))]
    #[init(val = 6)]
    pub radius: i32,
    /// Per-zoom subdivision distance thresholds in meters (14 slots, `[zoom - base_zoom]`).
    #[export]
    #[init(val = packed(&DEFAULT_THRESHOLDS))]
    pub thresholds: PackedFloat32Array,
    /// Camera travel (meters) that triggers a desired-set rebuild.
    #[export]
    #[init(val = 500.0)]
    pub update_distance: f32,
    /// Wall-clock budget per frame (milliseconds) for promoting tiles to the GPU.
    #[export]
    #[init(val = 2.0)]
    pub upload_budget_ms: f32,
    /// Hard cap on tile promotions per frame.
    #[export(range = (1.0, 64.0))]
    #[init(val = 8)]
    pub max_uploads_per_frame: i32,
    base: Base<Resource>,
}

#[godot_api]
impl TerrainStreamingConfig {
    pub fn to_core(&self) -> StreamingConfig {
        StreamingConfig {
            radius: self.radius,
            thresholds: per_zoom(&self.thresholds, &DEFAULT_THRESHOLDS),
            update_distance: self.update_distance,
            upload_budget: Duration::from_secs_f64(
                f64::from(self.upload_budget_ms.max(0.0)) / 1000.0,
            ),
            max_uploads_per_frame: self.max_uploads_per_frame.max(1) as usize,
        }
    }
}

/// Shader parameters: fog, sun, scales, skirts. All runtime-mutable — mutate any field and the
/// streamer pushes the change to every live tile on the next frame.
#[derive(GodotClass)]
#[class(init, base = Resource)]
pub struct TerrainRenderingConfig {
    /// Distance (meters) at which fog starts blending in.
    #[export]
    #[init(val = 100_000.0)]
    pub fog_start: f32,
    /// Distance (meters) at which fog fully replaces the terrain color.
    #[export]
    #[init(val = 150_000.0)]
    pub fog_end: f32,
    /// Match this to your sky color for a seamless horizon.
    #[export]
    #[init(val = Color::from_rgb(0.0, 0.0, 1.0))]
    pub fog_color: Color,
    /// World ambient light color; drives day/night/weather changes.
    #[export]
    #[init(val = Color::WHITE)]
    pub ambient_light: Color,
    /// Sun direction; normalized in the shader.
    #[export]
    #[init(val = Vector3::new(0.1, 1.0, 0.1))]
    pub sun_direction: Vector3,
    /// Sun light intensity — contrast between lit and shaded slopes.
    #[export]
    #[init(val = 1.0)]
    pub sun_scale: f32,
    /// Terrain relief exaggeration (drama factor).
    #[export]
    #[init(val = 1.0)]
    pub height_scale: f32,
    /// Normal-map contrast multiplier.
    #[export]
    #[init(val = 1.0)]
    pub normals_scale: f32,
    /// Vertical drop (meters) of skirt geometry below tile edges; 0 disables.
    #[export]
    #[init(val = 0.0)]
    pub skirt_drop: f32,
    base: Base<Resource>,
}

#[godot_api]
impl TerrainRenderingConfig {
    pub fn to_core(&self) -> RenderingConfig {
        RenderingConfig {
            fog_start: self.fog_start,
            fog_end: self.fog_end,
            fog_color: color(self.fog_color),
            ambient_light: color(self.ambient_light),
            sun_direction: vec3(self.sun_direction),
            sun_scale: self.sun_scale,
            height_scale: self.height_scale,
            normals_scale: self.normals_scale,
            skirt_drop: self.skirt_drop,
        }
    }
}

/// Tile download / cache parameters.
#[derive(GodotClass)]
#[class(init, base = Resource)]
pub struct TerrainNetworkConfig {
    /// Number of background download / synthesis worker threads.
    #[export(range = (1.0, 64.0))]
    #[init(val = 4)]
    pub threads: i32,
    /// Root of the on-disk tile cache. Relative paths resolve against the project directory.
    #[export]
    #[init(val = GString::from(".cache"))]
    pub cache_dir: GString,
    /// Imagery URL template with `:zoom:` / `:x:` / `:y:` tokens (Esri uses `zoom/y/x`).
    #[export]
    #[init(val = GString::from(DEFAULT_TEXTURE_URL))]
    pub texture_url: GString,
    /// Terrarium heightmap URL template.
    #[export]
    #[init(val = GString::from(DEFAULT_HEIGHTMAP_URL))]
    pub heightmap_url: GString,
    /// Normal-map URL template.
    #[export]
    #[init(val = GString::from(DEFAULT_NORMALS_URL))]
    pub normals_url: GString,
    /// Highest zoom the terrain providers serve natively (Mapzen: 15).
    #[export(range = (9.0, 22.0))]
    #[init(val = 15)]
    pub native_terrain_zoom: i32,
    /// HTTP connection timeout (seconds).
    #[export]
    #[init(val = 5.0)]
    pub connect_timeout_sec: f32,
    /// HTTP read timeout (seconds).
    #[export]
    #[init(val = 3.0)]
    pub read_timeout_sec: f32,
    base: Base<Resource>,
}

#[godot_api]
impl TerrainNetworkConfig {
    pub fn to_core(&self) -> NetworkConfig {
        let raw = PathBuf::from(self.cache_dir.to_string());
        let cache_dir = if raw.is_absolute() {
            raw
        } else {
            let root = ProjectSettings::singleton()
                .globalize_path("res://")
                .to_string();
            PathBuf::from(root).join(raw)
        };
        NetworkConfig {
            threads: self.threads.max(1) as usize,
            cache_dir,
            texture_url: self.texture_url.to_string(),
            heightmap_url: self.heightmap_url.to_string(),
            normals_url: self.normals_url.to_string(),
            native_terrain_zoom: self.native_terrain_zoom.clamp(0, 255) as u8,
            connect_timeout: Duration::from_secs_f64(f64::from(self.connect_timeout_sec.max(0.0))),
            read_timeout: Duration::from_secs_f64(f64::from(self.read_timeout_sec.max(0.0))),
        }
    }
}

// The IResource impls are required for `#[class(init)]` resources to participate in the engine's
// resource lifecycle; nothing to override.
#[godot_api]
impl IResource for TerrainWorldConfig {}
#[godot_api]
impl IResource for TerrainStreamingConfig {}
#[godot_api]
impl IResource for TerrainRenderingConfig {}
#[godot_api]
impl IResource for TerrainNetworkConfig {}
