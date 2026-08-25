//! Engine-agnostic configuration: plain Rust mirrors of the four Godot config
//! resources, their defaults, validation, and geo anchoring. Mirrors raytiles'
//! nested `config { world, streaming, rendering, network }`.

use glam::Vec3;
use std::path::PathBuf;
use std::time::Duration;

/// Lowest zoom level the engine supports; `WorldConfig::base_zoom` must not go below it.
pub const MIN_ZOOM: u8 = 9;
/// Highest zoom level the engine supports; `WorldConfig::max_zoom` must not exceed it. Above
/// `NetworkConfig::native_terrain_zoom` heightmaps are synthesized, so only imagery needs to
/// exist natively this deep.
pub const MAX_ZOOM: u8 = 22;
/// Number of zoom levels in `[MIN_ZOOM, MAX_ZOOM]`; sizes every per-zoom array. Slot `i`
/// applies to zoom `base_zoom + i`; slots beyond `max_zoom - base_zoom` are ignored.
pub const ZOOM_LEVELS: usize = (MAX_ZOOM - MIN_ZOOM + 1) as usize; // 14

/// Highest terrain elevation the culling column must cover (Everest, meters).
pub const MAX_WORLD_HEIGHT: f32 = 8848.0;
/// Lowest terrain elevation the culling column must cover (Dead Sea shore, with margin).
pub const MIN_WORLD_HEIGHT: f32 = -450.0;
/// Extra headroom added on top of `MAX_WORLD_HEIGHT` in the culling column.
pub const WORLD_HEIGHT_MARGIN: f32 = 300.0;
/// Quads per mesh side at the base zoom; doubles per zoom.
pub const MIN_RESOLUTION: u32 = 4;
/// Cap on quads per mesh side (provider tiles are 256×256 texels).
pub const MAX_RESOLUTION: u32 = 256;
/// Provider tile resolution; generated fallback / derived assets match it.
pub const TILE_TEXELS: u32 = 256;

const EQUATOR_CIRCUMFERENCE_M: f64 = 40_075_016.686;

/// Default per-zoom subdivision thresholds (meters), z9 → z22.
pub const DEFAULT_THRESHOLDS: [f32; ZOOM_LEVELS] = [
    100_000.0, 80_000.0, 40_000.0, 20_000.0, 10_000.0, 5_000.0, 2_500.0, 1_250.0, 625.0, 312.0,
    156.0, 78.0, 39.0, 20.0,
];

/// World topology. Effectively immutable once streaming started.
#[derive(Clone, Debug, PartialEq)]
pub struct WorldConfig {
    /// Anchor tile X at `base_zoom`: the world origin sits at this tile's corner.
    pub anchor_x: i32,
    /// Anchor tile Z (slippy-map `y`) at `base_zoom`.
    pub anchor_z: i32,
    /// Lowest LOD zoom ever loaded (>= MIN_ZOOM).
    pub base_zoom: u8,
    /// Highest LOD zoom (<= MAX_ZOOM). Defaults to the native terrain ceiling (15); raising it
    /// beyond `NetworkConfig::native_terrain_zoom` opts into synthesized heightmaps and default
    /// normals.
    pub max_zoom: u8,
    /// World size (meters) of one tile at `base_zoom`.
    pub tile_size: f32,
    /// Per-zoom mesh overlap factors, `skirt_overlap[zoom - base_zoom]`. A zero slot produces a
    /// degenerate mesh — `validate` rejects it.
    pub skirt_overlap: [f32; ZOOM_LEVELS],
    /// Generate mipmaps for the albedo texture.
    pub mipmaps: bool,
    /// World-space offset of the anchor point inside its anchor tile; filled by `from_lat_lon`
    /// so the origin sits exactly on the coordinate.
    pub origin_offset: Vec3,
}

impl Default for WorldConfig {
    fn default() -> Self {
        Self {
            anchor_x: 306,
            anchor_z: 207,
            base_zoom: MIN_ZOOM,
            max_zoom: 15,
            tile_size: 66_400.0,
            skirt_overlap: [1.0; ZOOM_LEVELS],
            mipmaps: true,
            origin_offset: Vec3::ZERO,
        }
    }
}

/// Result of the web-mercator anchoring math, kept separate so the Godot resource can apply it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeoAnchor {
    pub anchor_x: i32,
    pub anchor_z: i32,
    pub tile_size: f64,
    pub origin_offset: Vec3,
}

/// Derive the anchor tile (at `MIN_ZOOM`), tile size, and origin offset for a geographic
/// coordinate in degrees (web-mercator, same math as raytiles / bevytiles).
pub fn geo_anchor(lat: f64, lon: f64) -> GeoAnchor {
    let n = 2f64.powi(MIN_ZOOM as i32);
    let lat_rad = lat.to_radians();
    let x = (lon + 180.0) / 360.0 * n;
    let y = (1.0 - (lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n;
    let anchor_x = x.floor() as i32;
    let anchor_z = y.floor() as i32;
    let tile_size = EQUATOR_CIRCUMFERENCE_M * lat_rad.cos() / n;
    GeoAnchor {
        anchor_x,
        anchor_z,
        tile_size,
        origin_offset: Vec3::new(
            ((x - f64::from(anchor_x)) * tile_size) as f32,
            0.0,
            ((y - f64::from(anchor_z)) * tile_size) as f32,
        ),
    }
}

impl WorldConfig {
    /// Anchor the world at a geographic coordinate (degrees). Assumes `base_zoom == MIN_ZOOM`.
    pub fn from_lat_lon(lat: f64, lon: f64) -> Self {
        let g = geo_anchor(lat, lon);
        Self {
            anchor_x: g.anchor_x,
            anchor_z: g.anchor_z,
            tile_size: g.tile_size as f32,
            origin_offset: g.origin_offset,
            ..Default::default()
        }
    }

    /// A sensible initial camera position over the anchor, `altitude` m up.
    pub fn initial_position(&self, altitude: f32) -> Vec3 {
        self.origin_offset + Vec3::Y * altitude
    }

    /// World size (meters) of one tile at `zoom`: halves per level above base.
    pub fn zoom_size(&self, zoom: u8) -> f64 {
        f64::from(self.tile_size) / f64::from(1u32 << (zoom - self.base_zoom))
    }

    /// Index into the per-zoom arrays.
    pub fn zoom_index(&self, zoom: u8) -> usize {
        (zoom - self.base_zoom) as usize
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.base_zoom < MIN_ZOOM {
            return Err(format!(
                "base_zoom {} is below min supported zoom {MIN_ZOOM}",
                self.base_zoom
            ));
        }
        if self.max_zoom > MAX_ZOOM {
            return Err(format!(
                "max_zoom {} is above max supported zoom {MAX_ZOOM}",
                self.max_zoom
            ));
        }
        if self.max_zoom < self.base_zoom {
            return Err(format!(
                "max_zoom {} is below base_zoom {}",
                self.max_zoom, self.base_zoom
            ));
        }
        if self.tile_size <= 0.0 || self.tile_size.is_nan() {
            return Err(format!("tile_size {} must be positive", self.tile_size));
        }
        for i in 0..=self.zoom_index(self.max_zoom) {
            if self.skirt_overlap[i] <= 0.0 || self.skirt_overlap[i].is_nan() {
                return Err(format!(
                    "skirt_overlap[{i}] = {} would produce a degenerate mesh",
                    self.skirt_overlap[i]
                ));
            }
        }
        Ok(())
    }
}

/// Which tiles are kept resident and how aggressively the set updates.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamingConfig {
    /// Radius, in base-zoom tiles, of the disc loaded around the camera.
    pub radius: i32,
    /// Per-zoom subdivision distance thresholds (meters), `thresholds[zoom - base_zoom]`.
    pub thresholds: [f32; ZOOM_LEVELS],
    /// Camera travel (meters) that triggers a desired-set rebuild.
    pub update_distance: f32,
    /// Wall-clock budget per frame for promoting tiles to the GPU.
    pub upload_budget: Duration,
    /// Hard cap on promotions per frame, on top of the budget.
    pub max_uploads_per_frame: usize,
}

impl Default for StreamingConfig {
    fn default() -> Self {
        Self {
            radius: 6,
            thresholds: DEFAULT_THRESHOLDS,
            update_distance: 500.0,
            upload_budget: Duration::from_micros(2000),
            max_uploads_per_frame: 8,
        }
    }
}

impl StreamingConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.radius < 1 {
            return Err(format!("radius {} must be >= 1", self.radius));
        }
        if self.max_uploads_per_frame == 0 {
            return Err("max_uploads_per_frame must be >= 1".into());
        }
        Ok(())
    }
}

/// Rendering / shader parameters. All runtime-mutable.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderingConfig {
    pub fog_start: f32,
    pub fog_end: f32,
    /// RGBA, sRGB components as authored (the shader's `source_color` hint linearizes).
    pub fog_color: [f32; 4],
    pub ambient_light: [f32; 4],
    /// Normalized in the shader; magnitude is irrelevant.
    pub sun_direction: Vec3,
    pub sun_scale: f32,
    pub height_scale: f32,
    pub normals_scale: f32,
    /// Vertical drop (meters) of skirt geometry below tile edges; 0 disables.
    pub skirt_drop: f32,
}

impl Default for RenderingConfig {
    fn default() -> Self {
        Self {
            fog_start: 100_000.0,
            fog_end: 150_000.0,
            fog_color: [0.0, 0.0, 1.0, 1.0],
            ambient_light: [1.0, 1.0, 1.0, 1.0],
            sun_direction: Vec3::new(0.1, 1.0, 0.1),
            sun_scale: 1.0,
            height_scale: 1.0,
            normals_scale: 1.0,
            skirt_drop: 0.0,
        }
    }
}

/// Tile download / cache parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct NetworkConfig {
    /// Number of background download/synthesis worker threads.
    pub threads: usize,
    /// Root of the on-disk cache: `cache_dir/{texture,heightmap,normals}/z/x/y.png`.
    pub cache_dir: PathBuf,
    /// Provider URL templates with `:zoom:`/`:x:`/`:y:` tokens. The Esri texture default uses
    /// `zoom/y/x` order — that swap is intentional.
    pub texture_url: String,
    pub heightmap_url: String,
    pub normals_url: String,
    /// Highest zoom the terrain providers serve natively (Mapzen: 15). Above it heightmaps are
    /// synthesized from ancestors and normals default; no HTTP is attempted for either.
    pub native_terrain_zoom: u8,
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
}

pub const DEFAULT_TEXTURE_URL: &str =
    "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/:zoom:/:y:/:x:";
pub const DEFAULT_HEIGHTMAP_URL: &str =
    "https://s3.amazonaws.com/elevation-tiles-prod/terrarium/:zoom:/:x:/:y:.png";
pub const DEFAULT_NORMALS_URL: &str =
    "https://s3.amazonaws.com/elevation-tiles-prod/normal/:zoom:/:x:/:y:.png";

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            threads: 4,
            cache_dir: PathBuf::from(".cache"),
            texture_url: DEFAULT_TEXTURE_URL.into(),
            heightmap_url: DEFAULT_HEIGHTMAP_URL.into(),
            normals_url: DEFAULT_NORMALS_URL.into(),
            native_terrain_zoom: 15,
            connect_timeout: Duration::from_secs(5),
            read_timeout: Duration::from_secs(3),
        }
    }
}

impl NetworkConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !(MIN_ZOOM..=MAX_ZOOM).contains(&self.native_terrain_zoom) {
            return Err(format!(
                "native_terrain_zoom {} outside [{MIN_ZOOM}, {MAX_ZOOM}]",
                self.native_terrain_zoom
            ));
        }
        if self.threads == 0 {
            return Err("threads must be >= 1".into());
        }
        for (name, url) in [
            ("texture_url", &self.texture_url),
            ("heightmap_url", &self.heightmap_url),
            ("normals_url", &self.normals_url),
        ] {
            if !url.contains("://") {
                return Err(format!("{name} has no scheme: {url}"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn geo_anchors_match_reference_values() {
        let cases = [
            (
                "dolomites",
                46.206889,
                9.497194,
                269,
                181,
                54_168.30,
                27_469.85,
                39_307.58,
            ),
            (
                "grand canyon",
                35.97391,
                -113.76892,
                94,
                201,
                63_343.93,
                12_371.94,
                6_394.49,
            ),
            (
                "negev",
                30.82691969172123,
                34.91386235622426,
                305,
                209,
                67_213.26,
                44_042.89,
                58_796.15,
            ),
            (
                "london", 51.5074, 0.1278, 256, 170, 48_717.25, 8_854.85, 12_328.76,
            ),
        ];
        for (name, lat, lon, ax, az, ts, ox, oz) in cases {
            let g = geo_anchor(lat, lon);
            assert_eq!((g.anchor_x, g.anchor_z), (ax, az), "{name} anchor");
            assert!(
                close(g.tile_size, ts, 0.01),
                "{name} tile_size {}",
                g.tile_size
            );
            assert!(
                close(f64::from(g.origin_offset.x), ox, 0.1),
                "{name} offset x"
            );
            assert!(
                close(f64::from(g.origin_offset.z), oz, 0.1),
                "{name} offset z"
            );
            assert_eq!(g.origin_offset.y, 0.0);
        }
    }

    #[test]
    fn from_lat_lon_keeps_other_defaults() {
        let w = WorldConfig::from_lat_lon(46.206889, 9.497194);
        assert_eq!(w.base_zoom, MIN_ZOOM);
        assert_eq!(w.max_zoom, 15);
        assert!(w.mipmaps);
        assert_eq!(
            w.initial_position(5000.0),
            w.origin_offset + Vec3::Y * 5000.0
        );
        assert!(w.validate().is_ok());
    }

    #[test]
    fn validation_rejects_bad_configs() {
        let world = |f: fn(&mut WorldConfig)| {
            let mut w = WorldConfig::default();
            f(&mut w);
            w.validate()
        };
        assert!(world(|w| w.max_zoom = 8).is_err());
        assert!(world(|w| w.max_zoom = 23).is_err());
        assert!(world(|w| w.skirt_overlap[3] = 0.0).is_err());
        assert!(world(|w| w.skirt_overlap[13] = 0.0).is_ok()); // beyond max_zoom - base_zoom: ignored
        assert!(world(|w| w.tile_size = 0.0).is_err());
        let net = |f: fn(&mut NetworkConfig)| {
            let mut n = NetworkConfig::default();
            f(&mut n);
            n.validate()
        };
        assert!(net(|n| n.native_terrain_zoom = 23).is_err());
        assert!(net(|n| n.texture_url = "no-scheme".into()).is_err());
        assert!(net(|n| n.threads = 0).is_err());
    }

    #[test]
    fn zoom_size_halves_per_level() {
        let w = WorldConfig::default();
        assert_eq!(w.zoom_size(9), 66_400.0);
        assert_eq!(w.zoom_size(10), 33_200.0);
        assert_eq!(w.zoom_size(15), 66_400.0 / 64.0);
    }
}
