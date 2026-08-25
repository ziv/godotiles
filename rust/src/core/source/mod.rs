//! The tile source: background fetching of texture + heightmap + normals per tile, PNG/JPEG
//! decoding, heightmap synthesis above the native zoom, and delivery of whole-tile payloads
//! through channels drained once per frame.
//!
//! The [`native`] backend runs plain `std::thread` workers with blocking HTTP (`ureq`) and an
//! on-disk cache with atomic write-through. Workers never call any engine API.

use super::height::HeightGrid;
use super::lod::TileKey;
use image::{ImageReader, RgbImage, RgbaImage};
use std::io::Cursor;

pub mod native;
pub use native::TileSource;

/// One tile fetch: anchor-relative identity + absolute provider coordinates at `key.zoom` (the
/// caller resolves the anchor; the source knows nothing about world anchoring).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileRequest {
    pub key: TileKey,
    /// Absolute provider tile column at `key.zoom`.
    pub x: i32,
    /// Absolute provider tile row at `key.zoom`.
    pub z: i32,
}

/// A completed tile: all three assets decoded, plus the CPU height grid (built here so the
/// main thread never pays for it).
#[derive(Debug)]
pub struct TilePayload {
    pub key: TileKey,
    /// Decoded satellite imagery.
    pub albedo: RgbaImage,
    /// Decoded (or synthesized) Terrarium heightmap.
    pub height: RgbImage,
    /// Decoded normal map, or the flat default.
    pub normals: RgbImage,
    /// CPU height grid derived from `height` on the worker.
    pub grid: HeightGrid,
}

/// A tile that will not arrive. Every request is answered by exactly one [`TilePayload`] or
/// one of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TileDrop {
    /// The job was cancelled before completing. If the tile is wanted again, re-request
    /// immediately (the camera came back).
    Cancelled(TileKey),
    /// Fetch or decode failed; the string is the reason. Wait for the next desired-set rebuild
    /// rather than hot-retrying a failing provider.
    Failed(TileKey, String),
}

impl TileDrop {
    pub fn key(&self) -> TileKey {
        match self {
            TileDrop::Cancelled(k) | TileDrop::Failed(k, _) => *k,
        }
    }
}

/// The seam the store talks through, so tests can script a fake source.
pub trait Source: Send {
    fn request(&self, req: TileRequest);
    fn cancel(&self, key: TileKey);
    fn drain(&self, ready: &mut Vec<TilePayload>, dropped: &mut Vec<TileDrop>);
}

/// Which of the three per-tile assets.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Kind {
    Texture,
    Heightmap,
    Normals,
}

impl Kind {
    pub(crate) fn url_template(self, cfg: &super::config::NetworkConfig) -> &str {
        match self {
            Kind::Texture => &cfg.texture_url,
            Kind::Heightmap => &cfg.heightmap_url,
            Kind::Normals => &cfg.normals_url,
        }
    }

    pub(crate) fn dir(self) -> &'static str {
        match self {
            Kind::Texture => "texture",
            Kind::Heightmap => "heightmap",
            Kind::Normals => "normals",
        }
    }
}

pub(crate) const CANCELLED: &str = "\u{0}cancelled";

/// Replace the first occurrence of each token (`:zoom:`, `:x:`, `:y:`).
pub fn expand_url(template: &str, zoom: u8, x: i32, z: i32) -> String {
    template
        .replacen(":zoom:", &zoom.to_string(), 1)
        .replacen(":x:", &x.to_string(), 1)
        .replacen(":y:", &z.to_string(), 1)
}

/// Decode PNG or JPEG bytes (Esri serves JPEG despite the extension-less URL).
pub(crate) fn decode_image(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("image sniff: {e}"))?
        .decode()
        .map_err(|e| format!("image decode: {e}"))
}

/// Synthesize the heightmap for (`zoom`, `x`, `z`) from its decoded native ancestor's float
/// heights (`w`×`h`): quadrant-chain upsample from `native + 1` up to `zoom`.
pub(crate) fn synthesize_heightmap(
    parent_floats: &[f32],
    w: usize,
    h: usize,
    native: u8,
    zoom: u8,
    x: i32,
    z: i32,
) -> RgbImage {
    let mut floats = parent_floats.to_vec();
    for level in (native + 1)..=zoom {
        let shift = zoom - level;
        let qx = ((x >> shift) & 1) as usize;
        let qz = ((z >> shift) & 1) as usize;
        floats = super::synth::upsample_quadrant(&floats, w, h, qx, qz);
    }
    super::synth::encode_terrarium(&floats, w as u32, h as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_tokens_replace_first_occurrence_only() {
        assert_eq!(
            expand_url("https://h/t/:zoom:/:y:/:x:", 9, 306, 207),
            "https://h/t/9/207/306"
        );
        assert_eq!(expand_url("x/:x:/:x:", 9, 1, 2), "x/1/:x:");
    }
}
