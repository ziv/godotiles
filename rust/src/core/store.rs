//! The tile lifecycle policy, engine-agnostic: which tiles should exist, which are being fetched,
//! which are resident; eviction rules with hole-avoidance; the promotion queue; the loading
//! contract. The engine shell owns GPU resources and talks to this through ops and facts:
//! `reconcile` returns keys to free, `next_promotion` hands out payloads to upload,
//! `mark_resident` records a finished upload, `set_visible` feeds back culling results.

use super::config::{NetworkConfig, StreamingConfig, WorldConfig};
use super::height::{ground_height, HeightGrid, HeightGrids};
use super::lod::{self, LodOptions, TileKey};
use super::source::{Source, TileDrop, TilePayload, TileRequest, TileSource};
use glam::Vec3;
use std::collections::{HashMap, HashSet, VecDeque};

/// What the store knows about a resident tile (the shell keeps the GPU handles).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResidentInfo {
    /// Absolute tile center X (f64 — precision far from the anchor).
    pub abs_x: f64,
    /// Absolute tile center Z.
    pub abs_z: f64,
    /// World size of the tile in meters.
    pub size: f32,
    /// Desired-set membership (for debug overlays).
    pub desired: bool,
    /// Last frame's frustum result, fed back by the shell.
    pub visible: bool,
}

/// Initial-load status for splash screens plus counters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Status {
    /// True during the initial load only.
    pub loading: bool,
    /// Fraction of the desired set that is resident, [0, 1].
    pub progress: f32,
    pub resident: usize,
    pub desired: usize,
    pub loading_count: usize,
}

pub struct TileStore<S: Source = TileSource> {
    world: WorldConfig,
    streaming: StreamingConfig,
    lod_opts: LodOptions,
    source: S,
    desired: HashSet<TileKey>,
    loading: HashSet<TileKey>,
    resident: HashMap<TileKey, ResidentInfo>,
    grids: HeightGrids,
    pending: VecDeque<TilePayload>,
    ready_scratch: Vec<TilePayload>,
    dropped_scratch: Vec<TileDrop>,
    desired_scratch: Vec<TileKey>,
    last_desired_pos: Option<Vec3>,
    coverage_dirty: bool,
    loading_flag: bool,
    failures: Vec<(TileKey, String)>,
}

impl TileStore<TileSource> {
    /// Validates the configs and spawns the worker pool.
    pub fn new(
        world: WorldConfig,
        streaming: StreamingConfig,
        network: &NetworkConfig,
    ) -> Result<Self, String> {
        world.validate()?;
        streaming.validate()?;
        let source = TileSource::new(network)?;
        Ok(Self::with_source(world, streaming, source))
    }
}

impl<S: Source> TileStore<S> {
    pub fn with_source(world: WorldConfig, streaming: StreamingConfig, source: S) -> Self {
        let lod_opts = LodOptions {
            base_zoom: world.base_zoom,
            max_zoom: world.max_zoom,
            base_tile_size: world.tile_size,
            radius: streaming.radius,
            thresholds: streaming.thresholds,
        };
        Self {
            world,
            streaming,
            lod_opts,
            source,
            desired: HashSet::new(),
            loading: HashSet::new(),
            resident: HashMap::new(),
            grids: HeightGrids::default(),
            pending: VecDeque::new(),
            ready_scratch: Vec::new(),
            dropped_scratch: Vec::new(),
            desired_scratch: Vec::new(),
            last_desired_pos: None,
            coverage_dirty: true,
            loading_flag: true,
            failures: Vec::new(),
        }
    }

    pub fn world(&self) -> &WorldConfig {
        &self.world
    }

    pub fn streaming(&self) -> &StreamingConfig {
        &self.streaming
    }

    pub fn zoom_size(&self, zoom: u8) -> f64 {
        self.world.zoom_size(zoom)
    }

    // -- frame steps ---------------------------------------------------------

    /// Evict residents that are NOT desired AND (base zoom | not visible last frame | beyond the
    /// horizon | covered by a resident parent / all children / grandparent / all grandchildren).
    /// The covered-by rule protects against holes in the ground; its checks are gated on
    /// `coverage_dirty`. Returns the evicted keys (already removed from the store).
    pub fn reconcile(&mut self, abs_cam: Vec3) -> Vec<TileKey> {
        // candidates coarse-first, evaluated sequentially against the LIVE map: when a parent
        // and its children are all stale, the parent goes and the children survive (a snapshot
        // evaluation would evict both — a hole)
        let mut candidates: Vec<TileKey> = self
            .resident
            .keys()
            .filter(|k| !self.desired.contains(k))
            .copied()
            .collect();
        candidates.sort();
        let mut evict = Vec::new();
        for key in candidates {
            let info = self.resident[&key];
            let remove = if key.zoom == self.world.base_zoom
                || !info.visible
                || lod::out_of_horizon(abs_cam, self.world.zoom_size(key.zoom), key)
            {
                true
            } else if !self.coverage_dirty {
                false
            } else {
                self.is_covered(key)
            };
            if remove {
                self.resident.remove(&key);
                self.grids.0.remove(&key);
                evict.push(key);
            }
        }
        self.coverage_dirty = false;
        evict
    }

    fn is_covered(&self, key: TileKey) -> bool {
        let has = |zoom: u8, x: i32, z: i32| self.resident.contains_key(&TileKey { zoom, x, z });
        let (base, max) = (self.world.base_zoom, self.world.max_zoom);
        if key.zoom > base && has(key.zoom - 1, key.x >> 1, key.z >> 1) {
            return true;
        }
        if key.zoom < max {
            let (cx, cz) = (key.x * 2, key.z * 2);
            if has(key.zoom + 1, cx, cz)
                && has(key.zoom + 1, cx + 1, cz)
                && has(key.zoom + 1, cx, cz + 1)
                && has(key.zoom + 1, cx + 1, cz + 1)
            {
                return true;
            }
        }
        // grandparent / grandchildren: rare, but happens when zoom levels are skipped by
        // distance-based loading during fast movement
        if key.zoom > base + 1 && has(key.zoom - 2, key.x >> 2, key.z >> 2) {
            return true;
        }
        if key.zoom + 1 < max {
            let (cx, cz) = (key.x * 4, key.z * 4);
            return (0..4).all(|ox| (0..4).all(|oz| has(key.zoom + 2, cx + ox, cz + oz)));
        }
        false
    }

    /// One non-blocking sweep of the source. Drops clear `loading` (failures are recorded for
    /// the shell; a cancelled-but-desired-again key is re-requested immediately); payloads join
    /// the promotion queue.
    pub fn drain(&mut self) {
        self.ready_scratch.clear();
        self.dropped_scratch.clear();
        self.source
            .drain(&mut self.ready_scratch, &mut self.dropped_scratch);
        let dropped = std::mem::take(&mut self.dropped_scratch);
        for d in &dropped {
            match d {
                TileDrop::Failed(key, reason) => {
                    self.loading.remove(key);
                    self.failures.push((*key, reason.clone()));
                }
                TileDrop::Cancelled(key) => {
                    self.loading.remove(key);
                    if self.desired.contains(key) && !self.resident.contains_key(key) {
                        self.request(*key);
                    }
                }
            }
        }
        self.dropped_scratch = dropped;
        self.dropped_scratch.clear();
        self.pending.extend(self.ready_scratch.drain(..));
    }

    /// Next payload that is still desired and not resident; payloads that are no longer wanted
    /// are discarded (their `loading` entry cleared).
    pub fn next_promotion(&mut self) -> Option<TilePayload> {
        while let Some(payload) = self.pending.pop_front() {
            let key = payload.key;
            self.loading.remove(&key);
            if !self.desired.contains(&key) || self.resident.contains_key(&key) {
                continue;
            }
            return Some(payload);
        }
        None
    }

    /// The shell finished uploading `key`: register residency and its height grid.
    pub fn mark_resident(&mut self, key: TileKey, grid: HeightGrid) -> ResidentInfo {
        let size = self.world.zoom_size(key.zoom);
        let info = ResidentInfo {
            abs_x: (f64::from(key.x) + 0.5) * size,
            abs_z: (f64::from(key.z) + 0.5) * size,
            size: size as f32,
            desired: self.desired.contains(&key),
            visible: false, // decided by the cull pass later this frame
        };
        self.resident.insert(key, info);
        self.grids.0.insert(key, grid);
        self.coverage_dirty = true; // a new resident can cover parent / children
        info
    }

    /// Movement-gated desired-set rebuild; returns true when it ran.
    pub fn maybe_update_desired(&mut self, abs_cam: Vec3) -> bool {
        let d = self.streaming.update_distance;
        let moved = match self.last_desired_pos {
            None => true,
            Some(last) => abs_cam.distance_squared(last) > d * d,
        };
        if moved {
            self.last_desired_pos = Some(abs_cam);
            self.update_desired(abs_cam);
        }
        moved
    }

    /// Run the pure LOD policy, cancel loading keys that fell out of the set (once, here — the
    /// set only changes in this function), and request the missing ones.
    pub fn update_desired(&mut self, abs_cam: Vec3) {
        self.desired_scratch.clear();
        lod::desired_tiles(&self.lod_opts, abs_cam, &mut self.desired_scratch);
        self.desired.clear();
        self.desired.extend(self.desired_scratch.iter().copied());
        self.coverage_dirty = true;

        for (key, info) in self.resident.iter_mut() {
            info.desired = self.desired.contains(key);
        }

        let stale: Vec<TileKey> = self
            .loading
            .iter()
            .filter(|k| !self.desired.contains(k))
            .copied()
            .collect();
        for key in stale {
            self.source.cancel(key);
        }

        let missing: Vec<TileKey> = self
            .desired
            .iter()
            .filter(|k| !self.resident.contains_key(k) && !self.loading.contains(k))
            .copied()
            .collect();
        for key in missing {
            self.request(key);
        }
    }

    fn request(&mut self, key: TileKey) {
        let scale = 1i32 << (key.zoom - self.world.base_zoom);
        self.source.request(TileRequest {
            key,
            x: key.x + self.world.anchor_x * scale,
            z: key.z + self.world.anchor_z * scale,
        });
        self.loading.insert(key);
    }

    // -- facts & queries -----------------------------------------------------

    pub fn set_visible(&mut self, key: TileKey, visible: bool) {
        if let Some(info) = self.resident.get_mut(&key) {
            info.visible = visible;
        }
    }

    pub fn resident_iter(&self) -> impl Iterator<Item = (&TileKey, &ResidentInfo)> {
        self.resident.iter()
    }

    pub fn resident_keys(&self) -> Vec<TileKey> {
        self.resident.keys().copied().collect()
    }

    pub fn is_resident(&self, key: TileKey) -> bool {
        self.resident.contains_key(&key)
    }

    pub fn is_desired(&self, key: TileKey) -> bool {
        self.desired.contains(&key)
    }

    pub fn is_loading_key(&self, key: TileKey) -> bool {
        self.loading.contains(&key)
    }

    /// Terrain altitude under an ABSOLUTE-space point.
    pub fn ground_height(&self, abs: Vec3) -> Option<f32> {
        ground_height(&self.grids, &self.world, abs)
    }

    /// Initial-loading contract: `loading` flips only once a desired set EXISTS and is fully
    /// serviced (nothing in flight, nothing awaiting upload).
    pub fn status(&mut self) -> Status {
        let progress = if self.desired.is_empty() {
            0.0
        } else {
            let have = self
                .desired
                .iter()
                .filter(|k| self.resident.contains_key(k))
                .count();
            have as f32 / self.desired.len() as f32
        };
        if self.loading_flag
            && !self.desired.is_empty()
            && self.loading.is_empty()
            && self.pending.is_empty()
        {
            self.loading_flag = false;
        }
        Status {
            loading: self.loading_flag,
            progress,
            resident: self.resident.len(),
            desired: self.desired.len(),
            loading_count: self.loading.len(),
        }
    }

    pub fn take_failures(&mut self) -> Vec<(TileKey, String)> {
        std::mem::take(&mut self.failures)
    }

    /// Undo a `mark_resident` whose GPU upload failed: the key is forgotten (it will be requested
    /// again at the next desired-set rebuild). Returns whether it was resident.
    pub fn reconcile_forget(&mut self, key: TileKey) -> bool {
        self.grids.0.remove(&key);
        self.resident.remove(&key).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::synth;
    use std::sync::{Arc, Mutex};

    /// A scripted source: records requests/cancels, hands out whatever the test queues.
    #[derive(Default)]
    struct Script {
        requests: Vec<TileRequest>,
        cancels: Vec<TileKey>,
        ready: Vec<TilePayload>,
        dropped: Vec<TileDrop>,
    }

    #[derive(Clone, Default)]
    struct Fake(Arc<Mutex<Script>>);

    impl Source for Fake {
        fn request(&self, req: TileRequest) {
            self.0.lock().unwrap().requests.push(req);
        }
        fn cancel(&self, key: TileKey) {
            self.0.lock().unwrap().cancels.push(key);
        }
        fn drain(&self, ready: &mut Vec<TilePayload>, dropped: &mut Vec<TileDrop>) {
            let mut s = self.0.lock().unwrap();
            ready.append(&mut s.ready);
            dropped.append(&mut s.dropped);
        }
    }

    fn payload(key: TileKey, h: f32) -> TilePayload {
        let height = synth::encode_terrarium(&[h; 4], 2, 2);
        TilePayload {
            key,
            albedo: image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 255])),
            grid: HeightGrid::from_terrarium(&height),
            height,
            normals: synth::default_normals(2),
        }
    }

    fn store(fake: &Fake) -> TileStore<Fake> {
        let streaming = StreamingConfig {
            radius: 2, // tiny disc: 1 base tile
            ..Default::default()
        };
        TileStore::with_source(WorldConfig::default(), streaming, fake.clone())
    }

    fn promote_all(store: &mut TileStore<Fake>) -> Vec<TileKey> {
        store.drain();
        let mut out = Vec::new();
        while let Some(p) = store.next_promotion() {
            out.push(p.key);
            store.mark_resident(p.key, p.grid);
        }
        out
    }

    #[test]
    fn requests_carry_absolute_provider_coords() {
        let fake = Fake::default();
        let mut s = store(&fake);
        s.update_desired(Vec3::new(33_200.0, 60_000.0, 33_200.0)); // high: coarse tiles only
        let reqs = fake.0.lock().unwrap().requests.clone();
        assert!(!reqs.is_empty());
        for r in &reqs {
            let scale = 1 << (r.key.zoom - 9);
            assert_eq!(r.x, r.key.x + 306 * scale);
            assert_eq!(r.z, r.key.z + 207 * scale);
            assert!(s.is_loading_key(r.key));
        }
    }

    #[test]
    fn loading_flag_never_flips_before_a_desired_set_exists() {
        let fake = Fake::default();
        let mut s = store(&fake);
        assert!(s.status().loading);
        s.drain();
        assert!(s.status().loading);
        s.update_desired(Vec3::new(33_200.0, 60_000.0, 33_200.0));
        assert!(s.status().loading);
        let keys: Vec<TileKey> = fake
            .0
            .lock()
            .unwrap()
            .requests
            .iter()
            .map(|r| r.key)
            .collect();
        for k in &keys {
            fake.0.lock().unwrap().ready.push(payload(*k, 10.0));
        }
        let promoted = promote_all(&mut s);
        assert_eq!(promoted.len(), keys.len());
        let st = s.status();
        assert!(!st.loading);
        assert_eq!(st.progress, 1.0);
        assert_eq!(st.resident, keys.len());
    }

    #[test]
    fn undesired_payload_is_discarded_and_failed_is_not_retried() {
        let fake = Fake::default();
        let mut s = store(&fake);
        s.update_desired(Vec3::new(33_200.0, 60_000.0, 33_200.0));
        let keys: Vec<TileKey> = fake
            .0
            .lock()
            .unwrap()
            .requests
            .iter()
            .map(|r| r.key)
            .collect();
        let stray = TileKey {
            zoom: 9,
            x: 100,
            z: 100,
        };
        fake.0.lock().unwrap().ready.push(payload(stray, 1.0));
        fake.0
            .lock()
            .unwrap()
            .dropped
            .push(TileDrop::Failed(keys[0], "boom".into()));
        let promoted = promote_all(&mut s);
        assert!(promoted.is_empty());
        assert!(!s.is_resident(stray));
        assert!(!s.is_loading_key(keys[0]));
        assert_eq!(s.take_failures(), vec![(keys[0], "boom".to_string())]);
        // no immediate re-request of the failed key
        let n = fake.0.lock().unwrap().requests.len();
        assert_eq!(n, keys.len());
        // but a desired rebuild requests it again
        s.update_desired(Vec3::new(33_200.0, 60_000.0, 33_200.0));
        assert_eq!(fake.0.lock().unwrap().requests.len(), n + 1);
    }

    #[test]
    fn cancelled_but_desired_again_is_re_requested_immediately() {
        let fake = Fake::default();
        let mut s = store(&fake);
        s.update_desired(Vec3::new(33_200.0, 60_000.0, 33_200.0));
        let key = fake.0.lock().unwrap().requests[0].key;
        fake.0
            .lock()
            .unwrap()
            .dropped
            .push(TileDrop::Cancelled(key));
        let before = fake.0.lock().unwrap().requests.len();
        s.drain();
        assert!(s.is_loading_key(key));
        assert_eq!(fake.0.lock().unwrap().requests.len(), before + 1);
    }

    #[test]
    fn stale_loading_keys_are_cancelled_on_rebuild() {
        let fake = Fake::default();
        let mut s = store(&fake);
        s.update_desired(Vec3::new(33_200.0, 60_000.0, 33_200.0));
        // move far away: everything previously loading falls out of the set
        s.update_desired(Vec3::new(50.0 * 66_400.0, 60_000.0, 50.0 * 66_400.0));
        let cancels = fake.0.lock().unwrap().cancels.clone();
        assert!(!cancels.is_empty());
        assert!(cancels.iter().all(|k| !s.is_desired(*k)));
    }

    fn resident_visible(s: &mut TileStore<Fake>, key: TileKey) {
        s.mark_resident(key, payload(key, 0.0).grid);
        s.set_visible(key, true);
    }

    #[test]
    fn coverage_rules() {
        let fake = Fake::default();
        let mut s = store(&fake);
        let cam = Vec3::new(10.0, 500.0, 10.0); // horizon ≈ 80 km: every tile below is in range
        let child = |x, z| TileKey { zoom: 11, x, z };
        let parent = TileKey {
            zoom: 10,
            x: 0,
            z: 0,
        };

        // a lone, visible, undesired parent: nothing covers it → kept (no holes)
        resident_visible(&mut s, parent);
        assert!(s.reconcile(cam).is_empty());
        assert!(s.is_resident(parent));

        // children under a resident parent are covered → evicted, the parent stays
        for (x, z) in [(0, 0), (1, 0), (0, 1)] {
            resident_visible(&mut s, child(x, z));
        }
        let mut evicted = s.reconcile(cam);
        evicted.sort();
        assert_eq!(
            evicted,
            vec![child(0, 0), child(1, 0), child(0, 1)].tap_sort()
        );
        assert!(s.is_resident(parent));

        // all four children resident: the parent is covered → coarse-first eviction takes the
        // parent and the children survive (a snapshot evaluation would have evicted both)
        for (x, z) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            resident_visible(&mut s, child(x, z));
        }
        assert_eq!(s.reconcile(cam), vec![parent]);
        assert_eq!(s.status().resident, 4);
        // and stay while visible, in range and uncovered
        assert!(s.reconcile(cam).is_empty());

        // invisible → evicted without thinking
        s.set_visible(child(0, 0), false);
        assert_eq!(s.reconcile(cam), vec![child(0, 0)]);

        // a stale base-zoom tile is evicted without thinking — and because eviction is
        // sequential, the grandchildren it would have covered are re-checked against the live
        // map and survive
        let gp = TileKey {
            zoom: 9,
            x: 0,
            z: 0,
        };
        resident_visible(&mut s, gp);
        assert_eq!(s.reconcile(cam), vec![gp]);
        assert_eq!(s.status().resident, 3);
        for k in [child(1, 0), child(0, 1), child(1, 1)] {
            s.set_visible(k, false);
        }
        assert_eq!(s.reconcile(cam).len(), 3);

        // a resident (non-base) grandparent covers its grandchildren
        let gp10 = TileKey {
            zoom: 10,
            x: 0,
            z: 0,
        };
        resident_visible(&mut s, gp10);
        let gc = |x, z| TileKey { zoom: 12, x, z };
        for k in [gc(0, 0), gc(1, 0), gc(2, 3), gc(3, 3)] {
            resident_visible(&mut s, k);
        }
        let mut evicted = s.reconcile(cam);
        evicted.sort();
        assert_eq!(
            evicted,
            vec![gc(0, 0), gc(1, 0), gc(2, 3), gc(3, 3)].tap_sort()
        );
        assert_eq!(s.reconcile(cam), Vec::<TileKey>::new());
        s.set_visible(gp10, false);
        assert_eq!(s.reconcile(cam), vec![gp10]);
        assert_eq!(s.status().resident, 0);

        // grandchildren cover their grandparent (z10 ← 16 × z12)
        resident_visible(&mut s, parent);
        for ox in 0..4 {
            for oz in 0..4 {
                resident_visible(
                    &mut s,
                    TileKey {
                        zoom: 12,
                        x: ox,
                        z: oz,
                    },
                );
            }
        }
        assert_eq!(s.reconcile(cam), vec![parent]);
        assert_eq!(s.status().resident, 16);

        // beyond the horizon → evicted
        let far = TileKey {
            zoom: 11,
            x: 40,
            z: 40,
        }; // center ≈ 950 km away
        resident_visible(&mut s, far);
        assert_eq!(s.reconcile(cam), vec![far]);
    }

    trait TapSort {
        fn tap_sort(self) -> Self;
    }
    impl TapSort for Vec<TileKey> {
        fn tap_sort(mut self) -> Self {
            self.sort();
            self
        }
    }

    #[test]
    fn ground_height_uses_grids_of_residents_only() {
        let fake = Fake::default();
        let mut s = store(&fake);
        let key = TileKey {
            zoom: 9,
            x: 0,
            z: 0,
        };
        s.mark_resident(key, payload(key, 123.0).grid);
        assert_eq!(s.ground_height(Vec3::new(100.0, 0.0, 100.0)), Some(123.0));
        s.set_visible(key, false);
        s.reconcile(Vec3::new(100.0, 10.0, 100.0));
        assert_eq!(s.ground_height(Vec3::new(100.0, 0.0, 100.0)), None);
    }

    #[test]
    fn movement_gate() {
        let fake = Fake::default();
        let mut s = store(&fake);
        assert!(s.maybe_update_desired(Vec3::new(0.0, 100.0, 0.0)));
        assert!(!s.maybe_update_desired(Vec3::new(10.0, 100.0, 0.0)));
        assert!(s.maybe_update_desired(Vec3::new(600.0, 100.0, 0.0)));
    }
}
