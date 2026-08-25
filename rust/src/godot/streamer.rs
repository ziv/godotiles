//! `TerrainStreamer`: the Node3D that owns the store, drives the per-frame loop and exposes the
//! engine to GDScript / C#.

use super::configs::{
    TerrainNetworkConfig, TerrainRenderingConfig, TerrainStreamingConfig, TerrainWorldConfig,
};
use super::gpu::Gpu;
use crate::core::config::RenderingConfig;
use crate::core::frustum::{Frustum, Plane};
use crate::core::lod::TileKey;
use crate::core::store::TileStore;
use godot::classes::{Camera3D, INode3D, Node3D};
use godot::prelude::*;
use std::time::Instant;

/// Streams real-world terrain tiles around a camera and draws them. Add one per world.
#[derive(GodotClass)]
#[class(base = Node3D)]
pub struct TerrainStreamer {
    /// World topology (anchor, zoom range, tile size). Assigning a new one rebuilds the world.
    #[export]
    #[var(set = set_world)]
    world: Option<Gd<TerrainWorldConfig>>,
    /// Streaming policy (radius, thresholds, budgets). Assigning a new one rebuilds the world.
    #[export]
    #[var(set = set_streaming)]
    streaming: Option<Gd<TerrainStreamingConfig>>,
    /// Shader parameters; live — mutate any field at any time.
    #[export]
    rendering: Option<Gd<TerrainRenderingConfig>>,
    /// Download / cache parameters. Assigning a new one rebuilds the world.
    #[export]
    #[var(set = set_network)]
    network: Option<Gd<TerrainNetworkConfig>>,
    /// The camera that drives streaming. Empty: the viewport's current camera.
    #[export]
    camera_path: NodePath,
    /// Large-world rebase input: `absolute = user - world_offset`. Shift it together with your
    /// camera and every user-space node.
    #[export]
    world_offset: Vector3,
    /// Pauses streaming and hides the terrain when false.
    #[export]
    #[var(set = set_enabled)]
    enabled: bool,

    /// True until the first desired set is fully resident (splash screens).
    #[var(no_set)]
    is_loading: bool,
    /// Fraction of the desired set that is resident, in [0, 1].
    #[var(no_set)]
    loading_progress: f32,
    #[var(no_set)]
    resident_count: i32,
    #[var(no_set)]
    desired_count: i32,
    #[var(no_set)]
    loading_count: i32,

    store: Option<TileStore>,
    gpu: Gpu,
    last_rendering: RenderingConfig,
    initialized: bool,
    load_complete_emitted: bool,
    base: Base<Node3D>,
}

#[godot_api]
impl INode3D for TerrainStreamer {
    fn init(base: Base<Node3D>) -> Self {
        Self {
            // defaults exist from construction so `terrain.world.max_zoom = 17` works before the
            // node enters the tree (and the inspector shows a resource, not <empty>)
            world: Some(TerrainWorldConfig::new_gd()),
            streaming: Some(TerrainStreamingConfig::new_gd()),
            rendering: Some(TerrainRenderingConfig::new_gd()),
            network: Some(TerrainNetworkConfig::new_gd()),
            camera_path: NodePath::default(),
            world_offset: Vector3::ZERO,
            enabled: true,
            is_loading: true,
            loading_progress: 0.0,
            resident_count: 0,
            desired_count: 0,
            loading_count: 0,
            store: None,
            gpu: Gpu::new(),
            last_rendering: RenderingConfig::default(),
            initialized: false,
            load_complete_emitted: false,
            base,
        }
    }

    fn ready(&mut self) {
        // run after the app's camera / rebase scripts (default priority 0) so we observe the
        // finished frame state
        self.base_mut().set_process_priority(100);
        self.initialized = true;
        self.rebuild();
    }

    fn process(&mut self, _delta: f64) {
        if !self.enabled {
            return;
        }
        if self.store.is_none() {
            // re-entered the tree after an exit_tree teardown
            if self.initialized {
                self.rebuild();
            }
            if self.store.is_none() {
                return;
            }
        }
        self.frame();
    }

    fn exit_tree(&mut self) {
        self.teardown();
    }
}

#[godot_api]
impl TerrainStreamer {
    /// Emitted once, when the initial desired set became fully resident.
    #[signal]
    fn initial_load_complete();
    /// A tile became resident.
    #[signal]
    fn tile_promoted(zoom: i32, x: i32, z: i32);
    /// A resident tile was evicted.
    #[signal]
    fn tile_evicted(zoom: i32, x: i32, z: i32);
    /// A tile download / decode failed (it is retried at the next desired-set rebuild).
    #[signal]
    fn tile_failed(zoom: i32, x: i32, z: i32, reason: GString);

    /// A sensible initial camera position over the world anchor, `altitude` meters up.
    #[func]
    pub fn initial_position(&self, altitude: f32) -> Vector3 {
        match &self.world {
            Some(w) => w.bind().initial_position(altitude),
            None => Vector3::new(0.0, altitude, 0.0),
        }
    }

    /// Terrain altitude (meters) under a user-space point, or `null` when no resident tile
    /// covers it. O(zoom levels).
    #[func]
    pub fn ground_height(&self, position: Vector3) -> Variant {
        let abs = position - self.world_offset;
        match self
            .store
            .as_ref()
            .and_then(|s| s.ground_height(glam::Vec3::new(abs.x, abs.y, abs.z)))
        {
            Some(h) => h.to_variant(),
            None => Variant::nil(),
        }
    }

    /// Debug: one dictionary per resident tile — `zoom`, `x`, `z`, `center` (user space),
    /// `size`, `desired`, `visible`.
    #[func]
    pub fn get_resident_tiles(&self) -> Array<VarDictionary> {
        let mut out = Array::new();
        let Some(store) = &self.store else {
            return out;
        };
        for (key, info) in store.resident_iter() {
            let mut d = VarDictionary::new();
            d.set("zoom", i32::from(key.zoom));
            d.set("x", key.x);
            d.set("z", key.z);
            d.set(
                "center",
                Vector3::new(
                    (info.abs_x + f64::from(self.world_offset.x)) as f32,
                    0.0,
                    (info.abs_z + f64::from(self.world_offset.z)) as f32,
                ),
            );
            d.set("size", info.size);
            d.set("desired", info.desired);
            d.set("visible", info.visible);
            out.push(&d);
        }
        out
    }

    /// Debug: whether a user-space point is inside the streaming camera's frustum according to
    /// the engine's own culling test (a self-check of the plane sign convention: the camera
    /// position + forward must be inside, position − forward outside).
    #[func]
    pub fn debug_point_in_frustum(&self, position: Vector3) -> bool {
        match self.resolve_camera().and_then(|c| frustum_from_camera(&c)) {
            Some(f) => f.contains_point(glam::Vec3::new(position.x, position.y, position.z)),
            None => false,
        }
    }

    /// Drop every tile and recreate meshes / workers from the current configs.
    #[func]
    pub fn rebuild(&mut self) {
        self.teardown();
        if !self.initialized {
            return;
        }
        let world = self.world_or_default().bind().to_core();
        let streaming = self.streaming_or_default().bind().to_core();
        let network = self.network_or_default().bind().to_core();
        let rendering = self.rendering_or_default().bind().to_core();

        let store = match TileStore::new(world.clone(), streaming, &network) {
            Ok(s) => s,
            Err(e) => {
                godot_error!("godotiles: invalid configuration: {e}");
                self.enabled = false;
                return;
            }
        };
        let scenario = match self.base().get_world_3d() {
            Some(w) => w.get_scenario(),
            None => {
                godot_error!("godotiles: TerrainStreamer must be inside a World3D");
                return;
            }
        };
        self.gpu.set_scenario(scenario);
        self.gpu.build_meshes(&world);
        self.gpu.rebake(self.world_offset);
        self.last_rendering = rendering;
        self.store = Some(store);
        self.is_loading = true;
        self.loading_progress = 0.0;
        self.resident_count = 0;
        self.desired_count = 0;
        self.loading_count = 0;
        self.load_complete_emitted = false;
    }

    #[func]
    pub fn set_fog_color(&mut self, color: Color) {
        self.rendering_or_default().bind_mut().fog_color = color;
    }
    #[func]
    pub fn set_fog_start(&mut self, distance: f32) {
        self.rendering_or_default().bind_mut().fog_start = distance;
    }
    #[func]
    pub fn set_fog_end(&mut self, distance: f32) {
        self.rendering_or_default().bind_mut().fog_end = distance;
    }
    #[func]
    pub fn set_ambient_light(&mut self, color: Color) {
        self.rendering_or_default().bind_mut().ambient_light = color;
    }
    #[func]
    pub fn set_sun_direction(&mut self, direction: Vector3) {
        self.rendering_or_default().bind_mut().sun_direction = direction;
    }
    #[func]
    pub fn set_sun_scale(&mut self, scale: f32) {
        self.rendering_or_default().bind_mut().sun_scale = scale;
    }
    #[func]
    pub fn set_height_scale(&mut self, scale: f32) {
        self.rendering_or_default().bind_mut().height_scale = scale;
    }
    #[func]
    pub fn set_normals_scale(&mut self, scale: f32) {
        self.rendering_or_default().bind_mut().normals_scale = scale;
    }
    #[func]
    pub fn set_skirt_drop(&mut self, drop: f32) {
        self.rendering_or_default().bind_mut().skirt_drop = drop;
    }

    // -- property setters ------------------------------------------------------

    #[func]
    fn set_world(&mut self, world: Option<Gd<TerrainWorldConfig>>) {
        self.world = world;
        self.rebuild_if_live();
    }

    #[func]
    fn set_streaming(&mut self, streaming: Option<Gd<TerrainStreamingConfig>>) {
        self.streaming = streaming;
        self.rebuild_if_live();
    }

    #[func]
    fn set_network(&mut self, network: Option<Gd<TerrainNetworkConfig>>) {
        self.network = network;
        self.rebuild_if_live();
    }

    #[func]
    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        self.gpu.set_visible(enabled);
    }
}

impl TerrainStreamer {
    fn rebuild_if_live(&mut self) {
        if self.initialized && self.base().is_inside_tree() {
            self.rebuild();
        }
    }

    fn teardown(&mut self) {
        self.gpu.free_all();
        // dropping the store joins the workers (an in-flight HTTP read may delay it by its timeout)
        self.store = None;
    }

    fn world_or_default(&mut self) -> Gd<TerrainWorldConfig> {
        self.world
            .get_or_insert_with(TerrainWorldConfig::new_gd)
            .clone()
    }

    fn streaming_or_default(&mut self) -> Gd<TerrainStreamingConfig> {
        self.streaming
            .get_or_insert_with(TerrainStreamingConfig::new_gd)
            .clone()
    }

    fn rendering_or_default(&mut self) -> Gd<TerrainRenderingConfig> {
        self.rendering
            .get_or_insert_with(TerrainRenderingConfig::new_gd)
            .clone()
    }

    fn network_or_default(&mut self) -> Gd<TerrainNetworkConfig> {
        self.network
            .get_or_insert_with(TerrainNetworkConfig::new_gd)
            .clone()
    }

    fn resolve_camera(&self) -> Option<Gd<Camera3D>> {
        if !self.camera_path.is_empty() {
            if let Some(node) = self.base().get_node_or_null(&self.camera_path) {
                if let Ok(cam) = node.try_cast::<Camera3D>() {
                    return Some(cam);
                }
            }
        }
        self.base().get_viewport().and_then(|v| v.get_camera_3d())
    }

    /// The per-frame loop: reconcile → promote → update desired → cull → sync (see plan §6.2).
    fn frame(&mut self) {
        let Some(camera) = self.resolve_camera() else {
            return; // no camera: idle
        };
        let user_pos = camera.get_global_position();
        let abs = user_pos - self.world_offset;
        let abs_cam = glam::Vec3::new(abs.x, abs.y, abs.z);

        let world_offset = self.world_offset;
        let rendering = self.rendering_or_default().bind().to_core();
        let store = self.store.as_mut().expect("store exists while processing");

        // 1. reconcile (uses last frame's visibility)
        let mut evicted = store.reconcile(abs_cam);
        for key in &evicted {
            self.gpu.free(key);
        }

        // 2. drain + budgeted promotion
        store.drain();
        let budget = store.streaming().upload_budget;
        let cap = store.streaming().max_uploads_per_frame;
        let start = Instant::now();
        let mut promoted = Vec::new();
        let world = store.world().clone();
        while promoted.len() < cap && start.elapsed() < budget {
            let Some(payload) = store.next_promotion() else {
                break;
            };
            let key = payload.key;
            let info = store.mark_resident(key, payload.grid.clone());
            if self
                .gpu
                .upload(&world, &rendering, &payload, &info, world_offset)
            {
                promoted.push(key);
            } else {
                godot_warn!("godotiles: failed to create textures for tile {key:?}");
                let _ = store.reconcile_forget(key);
            }
        }

        // 3. movement-gated desired-set rebuild
        store.maybe_update_desired(abs_cam);

        // 4. rebase + cull
        if world_offset != self.gpu.baked_offset() {
            self.gpu.rebake(world_offset);
        }
        let frustum = frustum_from_camera(&camera);
        let keys = store.resident_keys();
        for key in keys {
            let visible = match (&frustum, self.gpu.get(&key)) {
                (Some(f), Some(tile)) => {
                    let t = super::gpu::user_transform(tile.abs_x, tile.abs_z, world_offset).origin;
                    f.intersects_column(t.x, t.z, tile.half)
                }
                _ => true,
            };
            store.set_visible(key, visible);
        }

        // 5. sync rendering params + status
        if rendering != self.last_rendering {
            self.gpu.push_params(&rendering);
            self.last_rendering = rendering;
        }
        let status = store.status();
        let failures = store.take_failures();
        self.is_loading = status.loading;
        self.loading_progress = status.progress;
        self.resident_count = status.resident as i32;
        self.desired_count = status.desired as i32;
        self.loading_count = status.loading_count as i32;

        // signals last: emitting re-enters user code
        for key in promoted {
            self.signals()
                .tile_promoted()
                .emit(i32::from(key.zoom), key.x, key.z);
        }
        for key in evicted.drain(..) {
            self.signals()
                .tile_evicted()
                .emit(i32::from(key.zoom), key.x, key.z);
        }
        for (key, reason) in failures {
            godot_warn!(
                "godotiles: tile {}/{}/{} failed: {reason}",
                key.zoom,
                key.x,
                key.z
            );
            self.signals().tile_failed().emit(
                i32::from(key.zoom),
                key.x,
                key.z,
                &GString::from(&reason),
            );
        }
        if !status.loading && !self.load_complete_emitted {
            self.load_complete_emitted = true;
            self.signals().initial_load_complete().emit();
        }
    }
}

/// Godot frustum planes (world = user space; outward normals) → core frustum.
fn frustum_from_camera(camera: &Gd<Camera3D>) -> Option<Frustum> {
    let planes = camera.get_frustum();
    if planes.len() != 6 {
        return None;
    }
    let mut out = [Plane {
        normal: glam::Vec3::Y,
        d: 0.0,
    }; 6];
    for (i, p) in planes.iter_shared().enumerate() {
        out[i] = Plane {
            normal: glam::Vec3::new(p.normal.x, p.normal.y, p.normal.z),
            d: p.d,
        };
    }
    Some(Frustum { planes: out })
}

#[allow(dead_code)]
fn _key_debug(key: TileKey) -> String {
    format!("{}/{}/{}", key.zoom, key.x, key.z)
}
