//! Pure LOD policy: computes the desired tile set for a camera position. Port of raytiles'
//! `lod.hpp` / bevytiles' `lod.rs` — no engine dependencies, no I/O, no state; the unit tests
//! (including exact snapshots) depend on that.

use super::config::ZOOM_LEVELS;
use glam::Vec3;

/// Anchor-relative tile identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileKey {
    /// Zoom level in `[base_zoom, max_zoom]`.
    pub zoom: u8,
    /// Tile column, relative to the world anchor at this zoom.
    pub x: i32,
    /// Tile row (slippy-map `y`, world `z`), anchor-relative.
    pub z: i32,
}

/// The subset of the configuration the desired-set policy needs. Derived values (per-zoom
/// sizes, squared thresholds, horizon radius) are computed inside [`desired_tiles`] so the
/// policy stays stateless.
#[derive(Clone, Debug)]
pub struct LodOptions {
    pub base_zoom: u8,
    pub max_zoom: u8,
    /// World size (meters) of one tile at `base_zoom`.
    pub base_tile_size: f32,
    /// Radius, in base-zoom tiles, of the disc scanned around the camera.
    pub radius: i32,
    /// Plain meters; squared internally (in f64 — an f32 square can overflow).
    pub thresholds: [f32; ZOOM_LEVELS],
}

/// Distance to the horizon from height h: d ≈ 3.57 km · √h  ⇒  d² = ratio²·h.
const HORIZON_RATIO_M: f64 = 3570.0;

/// Squared horizon distance for a camera altitude (clamped to >= 1 m).
pub fn horizon_sq(cam_y: f32) -> f64 {
    HORIZON_RATIO_M * HORIZON_RATIO_M * f64::from(cam_y.max(1.0))
}

/// Squared distance from the camera to a tile center, including the camera height — the
/// altitude term is what collapses LOD when flying high.
fn dist_sq_to_tile(cam: Vec3, zoom_size: f64, x: i32, z: i32) -> f64 {
    let cx = (f64::from(x) + 0.5) * zoom_size;
    let cz = (f64::from(z) + 0.5) * zoom_size;
    let dx = f64::from(cam.x) - cx;
    let dz = f64::from(cam.z) - cz;
    dx * dx + dz * dz + f64::from(cam.y) * f64::from(cam.y)
}

/// XZ-only squared distance (used by eviction's beyond-horizon rule).
pub fn dist_sq_to_tile_xz(cam: Vec3, zoom_size: f64, x: i32, z: i32) -> f64 {
    let cx = (f64::from(x) + 0.5) * zoom_size;
    let cz = (f64::from(z) + 0.5) * zoom_size;
    let dx = f64::from(cam.x) - cx;
    let dz = f64::from(cam.z) - cz;
    dx * dx + dz * dz
}

/// True when `key`'s center lies beyond the horizon for the camera's altitude (XZ distance
/// only) — used by eviction's beyond-horizon rule.
pub fn out_of_horizon(cam: Vec3, zoom_size: f64, key: TileKey) -> bool {
    dist_sq_to_tile_xz(cam, zoom_size, key.x, key.z) > horizon_sq(cam.y)
}

/// Appends the desired tile keys for `cam` (absolute space) into `out`. Does not clear `out`;
/// the produced keys are duplicate-free. Reuse the vector across calls so steady-state rebuilds
/// allocate nothing.
pub fn desired_tiles(opts: &LodOptions, cam: Vec3, out: &mut Vec<TileKey>) {
    let levels = (opts.max_zoom - opts.base_zoom) as usize + 1;
    let mut sizes = [0f64; ZOOM_LEVELS];
    let mut thresholds_sq = [0f64; ZOOM_LEVELS];
    for i in 0..levels {
        sizes[i] = f64::from(opts.base_tile_size) / f64::from(1u32 << i);
        let th = f64::from(opts.thresholds[i]);
        thresholds_sq[i] = th * th;
    }

    let base_size = f64::from(opts.base_tile_size);
    let cam_tile_x = (f64::from(cam.x) / base_size).floor() as i32;
    let cam_tile_z = (f64::from(cam.z) / base_size).floor() as i32;
    let r = opts.radius;
    let allowed_radius = (r - 1) * (r - 1);
    let render_radius_sq = horizon_sq(cam.y);

    struct Ctx<'a> {
        opts: &'a LodOptions,
        sizes: &'a [f64; ZOOM_LEVELS],
        thresholds_sq: &'a [f64; ZOOM_LEVELS],
        cam: Vec3,
        render_radius_sq: f64,
    }

    // Order of checks is load-bearing (accept at max zoom → reject beyond horizon → accept when
    // far enough → subdivide); the snapshot tests pin it.
    fn build(ctx: &Ctx, out: &mut Vec<TileKey>, zoom: u8, x: i32, z: i32) {
        if zoom == ctx.opts.max_zoom {
            out.push(TileKey { zoom, x, z });
            return;
        }
        let idx = (zoom - ctx.opts.base_zoom) as usize;
        let d = dist_sq_to_tile(ctx.cam, ctx.sizes[idx], x, z);
        if d > ctx.render_radius_sq {
            return;
        }
        if d >= ctx.thresholds_sq[idx] {
            out.push(TileKey { zoom, x, z });
            return;
        }
        for oz in 0..2 {
            for ox in 0..2 {
                build(ctx, out, zoom + 1, x * 2 + ox, z * 2 + oz);
            }
        }
    }

    let ctx = Ctx {
        opts,
        sizes: &sizes,
        thresholds_sq: &thresholds_sq,
        cam,
        render_radius_sq,
    };
    for dx in -r..=r {
        for dz in -r..=r {
            if dx * dx + dz * dz < allowed_radius {
                build(&ctx, out, opts.base_zoom, cam_tile_x + dx, cam_tile_z + dz);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::DEFAULT_THRESHOLDS;
    use std::collections::HashSet;

    fn defaults() -> LodOptions {
        LodOptions {
            base_zoom: 9,
            max_zoom: 15,
            base_tile_size: 66_400.0,
            radius: 6,
            thresholds: DEFAULT_THRESHOLDS,
        }
    }

    fn run(opts: &LodOptions, cam: Vec3) -> Vec<TileKey> {
        let mut out = Vec::new();
        desired_tiles(opts, cam, &mut out);
        out
    }

    fn probes() -> Vec<Vec3> {
        let ts = 66_400.0f32;
        let xz = [
            (0.0, 0.0),
            (0.5 * ts, 0.5 * ts),
            (0.25 * ts, 0.75 * ts),
            (3.3 * ts, -2.7 * ts),
            (-ts, -ts),
            (7.9 * ts, 0.1 * ts),
            (-5.55 * ts, 4.05 * ts),
        ];
        let alts = [2.0f32, 500.0, 5_000.0, 60_000.0];
        xz.iter()
            .flat_map(|&(x, z)| alts.iter().map(move |&y| Vec3::new(x, y, z)))
            .collect()
    }

    fn check_invariants(opts: &LodOptions) {
        for cam in probes() {
            let keys = run(opts, cam);
            let set: HashSet<_> = keys.iter().copied().collect();
            assert_eq!(set.len(), keys.len(), "duplicates at {cam:?}");
            let base = f64::from(opts.base_tile_size);
            let ccx = (f64::from(cam.x) / base).floor() as i32;
            let ccz = (f64::from(cam.z) / base).floor() as i32;
            let r = opts.radius;
            for k in &keys {
                assert!(k.zoom >= opts.base_zoom && k.zoom <= opts.max_zoom);
                // no key together with an ancestor
                let (mut x, mut z) = (k.x, k.z);
                for zoom in (opts.base_zoom..k.zoom).rev() {
                    x >>= 1;
                    z >>= 1;
                    assert!(
                        !set.contains(&TileKey { zoom, x, z }),
                        "{k:?} has resident ancestor at {zoom}"
                    );
                }
                // every key descends from a base tile inside the scanned disc
                let (dx, dz) = (x - ccx, z - ccz);
                assert!(
                    dx * dx + dz * dz < (r - 1) * (r - 1),
                    "{k:?} outside the disc"
                );
            }
        }
    }

    #[test]
    fn structural_invariants_default_options() {
        check_invariants(&defaults());
    }

    #[test]
    fn structural_invariants_non_default_options() {
        let mut th = [0f32; ZOOM_LEVELS];
        th[..5].copy_from_slice(&[40_000.0, 20_000.0, 10_000.0, 5_000.0, 2_500.0]);
        let opts = LodOptions {
            base_zoom: 11,
            max_zoom: 14,
            base_tile_size: 16_600.0,
            radius: 4,
            thresholds: th,
        };
        check_invariants(&opts);
    }

    // Exact regression pins for default options; byte-for-byte identical to the raytiles (C++)
    // and bevytiles snapshot suites — cross-port behavioral equivalence.
    #[test]
    fn snapshots() {
        let opts = defaults();
        let count = |keys: &[TileKey], zoom: u8| keys.iter().filter(|k| k.zoom == zoom).count();

        let a = run(&opts, Vec3::new(0.0, 500.0, 0.0));
        assert_eq!(a.len(), 252);
        assert_eq!(count(&a, 9), 0);
        assert_eq!(count(&a, 15), 64);
        assert!(a.contains(&TileKey {
            zoom: 15,
            x: 0,
            z: 0
        }));
        assert!(a.contains(&TileKey {
            zoom: 15,
            x: -1,
            z: -1
        }));

        let b = run(&opts, Vec3::new(33_200.0, 5_000.0, 33_200.0));
        assert_eq!(b.len(), 252);
        assert_eq!(count(&b, 9), 36);
        assert_eq!(count(&b, 15), 0); // y² term keeps max zoom out at 5 km altitude

        let c = run(&opts, Vec3::new(0.0, 60_000.0, 0.0));
        assert_eq!(c.len(), 117);
        assert_eq!(count(&c, 11), 48); // camera-height term caps refinement at z11
        assert_eq!(count(&c, 15), 0);
    }

    #[test]
    fn high_zoom_reachable_when_low_over_tile_center() {
        let mut opts = defaults();
        opts.max_zoom = 17;
        let keys = run(&opts, Vec3::new(33_200.0, 500.0, 33_200.0));
        assert!(keys.iter().any(|k| k.zoom > 15));
        check_invariants(&opts);
    }

    #[test]
    fn horizon_helpers() {
        assert_eq!(horizon_sq(0.0), horizon_sq(1.0));
        assert_eq!(horizon_sq(4.0), 4.0 * 3570.0 * 3570.0);
        // at 1 m the horizon is 3.57 km: the camera's own base tile center is 47 km away
        let own = TileKey {
            zoom: 9,
            x: 0,
            z: 0,
        };
        assert!(out_of_horizon(Vec3::new(0.0, 1.0, 0.0), 66_400.0, own));
        assert!(!out_of_horizon(
            Vec3::new(33_200.0, 1.0, 33_200.0),
            66_400.0,
            own
        ));
        // at 5 km the horizon is 252 km
        assert!(!out_of_horizon(Vec3::new(0.0, 5_000.0, 0.0), 66_400.0, own));
        assert!(out_of_horizon(
            Vec3::new(0.0, 5_000.0, 0.0),
            66_400.0,
            TileKey {
                zoom: 9,
                x: 10,
                z: 0
            }
        ));
    }
}
