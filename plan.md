# godotiles — implementation plan

**Godotiles** is the 3D geospatial engine 🌎 for [Godot 4](https://godotengine.org/): a port of
[raytiles](https://github.com/ziv/raytiles) (C++/raylib, the original) and
[bevytiles](https://github.com/ziv/bevytiles) (Rust/Bevy). It streams satellite imagery,
Terrarium heightmaps and normal maps around a moving camera and renders them as GPU-displaced
terrain, with O(1) ground-height queries, large-world shifting, and terrain synthesis above the
provider's native zoom.

This document is the complete specification for building the project. It is written for an LLM
(or a human) that has **no access to the raytiles / bevytiles sources**: every algorithm,
constant, rule and trap is restated here. Where a Godot capability replaces a hand-rolled
mechanism of the earlier ports, the mapping is explicit; where the earlier design must be kept
because Godot has no equivalent, that is explicit too.

---

## 0. Decisions already made (do not re-litigate)

| Decision | Choice | Why |
|---|---|---|
| Implementation | **Rust GDExtension via [godot-rust / gdext](https://github.com/godot-rust/gdext)** | bevytiles' pure modules (`lod`, `synth`, `height`, `source/native`) and their offline test suites port verbatim; static self-contained binaries (`ureq` + rustls, no OpenSSL); one build tool (`cargo`). |
| Code structure | **Standalone crate now, engine-agnostic core inside it** | The owner plans to extract a shared core crate later (for raytiles, bevytiles, a three.js port). Modules in `rust/src/core/` must not import `godot::*` — they are the future core. |
| Godot version | **Demo/CI on Godot 4.7.x; `compatibility_minimum = 4.4`; gdext feature `api-4-4`** | 4.4 is the oldest release worth supporting (reverse-Z depth, stable GDExtension API); an extension built for 4.4 loads in 4.7. |
| Platforms (v1) | **Desktop: macOS (universal), Linux x86_64, Windows x86_64** | Web (emscripten + nightly Rust, threads/nothreads dual builds) is documented as a later milestone in §14. |
| Sky | **Godot's built-in `ProceduralSkyMaterial`** in the demo; fog color matched to it | raytiles' sky module is not ported. |
| Node/culling model | **`RenderingServer` instances (RIDs) owned by one `TerrainStreamer` node** — not one `MeshInstance3D` per tile | Mirrors raytiles' flat render list; no scene-tree churn; Godot still frustum-culls instances via `instance_set_custom_aabb`. |
| Public config | Four `Resource` subclasses (`TerrainWorldConfig`, `TerrainStreamingConfig`, `TerrainRenderingConfig`, `TerrainNetworkConfig`) exported on the node | Mirrors the nested `config { world, streaming, rendering, network }` of both earlier ports; inspector-editable; swappable at runtime. |
| Data providers | Esri World Imagery (JPEG despite the `.png`-less URL), Mapzen/AWS Terrarium heightmaps, Mapzen normals | Same defaults as both ports. Mind their terms of use; attribution in README. |

Non-goals for v1: web/mobile exports, editor `tool` mode (streaming inside the editor viewport),
C#-specific API sugar (GDExtension classes are already callable from C#), sky module, C ABI /
wasm faces of the core (see §14 for the extraction boundary).

---

## 1. What the engine does

- **Three assets per tile**, XYZ/slippy-map addressed (`zoom/x/y`):
  satellite imagery (Esri: URL order is `zoom/y/x` — a deliberate provider quirk, keep it),
  heightmaps (Mapzen **Terrarium** PNG: `h = r·256 + g + b/256 − 32768` meters), normal maps.
- **Quadtree LOD**: a disc of coarse tiles (zoom 9 by default) around the camera; tiles subdivide
  toward the camera by per-zoom distance thresholds (the camera *altitude* is part of the
  distance, which collapses detail when flying high), capped by the horizon distance for the
  camera's altitude. Supported zoom range 9–22.
- **Disk cache** `cache_dir/{texture,heightmap,normals}/zoom/x/y.png`, write-through, atomic,
  resumable, shared format with raytiles/bevytiles (a cache warmed by one port serves the others).
- **Terrain synthesis above the provider ceiling** (zoom > 15): heightmaps derive from the
  native-zoom ancestor by upsampling in *height space*; normals fall back to a flat default;
  imagery keeps fetching natively. Lineage siblings are back-filled on a low-priority queue.
- **Ground height queries**: CPU-side bilinear height lookup under any XZ point, ±0.5 m.
- **GPU displacement**: flat shared grid meshes displaced in the vertex shader by the heightmap
  texture; fog, sun lighting from the normal map, optional skirts to hide LOD cracks.
- **Large-world shifting**: `absolute = user − world_offset`; the app rebases every few km; the
  engine rebakes tile transforms when the offset changes.
- **Tile lifecycle** with hole-avoidance (a tile is never evicted while it is the only cover for
  its area), cancellation of stale downloads, budgeted GPU promotion, and a loading contract for
  splash screens.

---

## 2. Godot capability map

The most important table in this document. Do not re-implement the left column where the middle
column exists; do not expect Godot to provide the right column.

| Concern | raytiles / bevytiles mechanism | Godot replacement |
|---|---|---|
| Draw list | flat `render_list` (C++) / one ECS entity per tile (Bevy) | **`RenderingServer` instance per resident tile** (`instance_create` + `instance_set_base(mesh)` + `instance_geometry_set_material_override`) in the node's `World3D` scenario. Godot iterates and culls them. |
| Frustum culling for *drawing* | manual 6-plane test / Bevy `Aabb` | `RenderingServer.instance_set_custom_aabb` with the full height column (displacement happens on the GPU, so Godot's mesh AABB is wrong without this). |
| Visibility feedback for *eviction* | last frame's `visible` flag / `ViewVisibility` | Godot does not report per-instance culling results. Keep a small frustum test in Rust: planes from `Camera3D.get_frustum()` (§7.9). |
| Front-to-back sort | manual / Bevy opaque pass | Godot's opaque pass sorts by depth — drop entirely. |
| GPU upload + budget | `LoadTextureFromImage` under a wall-clock budget | `Image.create_from_data` + `ImageTexture.create_from_image` on the main thread, under the same wall-clock + count budget (uploads are synchronous-ish through the render command queue). |
| Shader + uniforms | GLSL strings + `SetShaderValue` / WGSL + `AsBindGroup` | One shared `Shader` (Godot shading language, embedded via `include_str!`), one `ShaderMaterial` per tile carrying its three textures; parameters pushed to all live materials when `TerrainRenderingConfig` changes. |
| sRGB handling | raylib: none; Bevy: `Rgba8Unorm` vs `Rgba8UnormSrgb` | Godot sampler hint **`source_color` on the albedo only**. Heightmap/normal samplers must NOT have `source_color` (nor `hint_normal`) or the Terrarium decode is gamma-warped garbage. |
| Frame orchestration | `streamer::update()` / Bevy system sets | `TerrainStreamer._process()` with `process_priority = 100` so it runs after the app's camera/rebase scripts (§6.2). |
| Config | nested structs / Bevy `Resource`s | Four `Resource` subclasses exported on the node (§5.2). |
| Worker pool | `std::jthread` + condvar / `std::thread` + crossbeam | **Same design, same code** as bevytiles: plain `std::thread` workers + `crossbeam-channel`. Workers never call any Godot API (no `experimental-threads` feature needed). |
| RAII for GPU objects | hand-rolled / `Handle<T>` | `Gd<ImageTexture>` / `Gd<ShaderMaterial>` are ref-counted: dropping the resident record frees them. Instance RIDs are **not** ref-counted: `free_rid` explicitly on eviction and in `exit_tree`. |
| Large-world offset | `world_offset` convention | Same convention; a `world_offset: Vector3` property on the node. Godot's default single-precision build needs it exactly like raylib/Bevy. |
| Ground height | CPU height grids | Same. |
| Camera | raylib `Camera3D` / `TerrainCamera` marker | `camera_path` export, fallback `get_viewport().get_camera_3d()`. |
| Debug overlays | `draw_debug_3d` / labels | Rust exposes `get_resident_tiles()`; the demo draws bounds (`ImmediateMesh`) and labels (`Label`s via `Camera3D.unproject_position`) in GDScript. |

What Godot does **not** give you (keep the earlier design, in `rust/src/core/`):

- the LOD policy (§7.2), the synthesis math (§7.3), the height grids (§7.4),
- the source: HTTP + disk cache + decode + synthesis on worker threads, whole-tile payloads
  through channels (§7.5),
- the tile lifecycle policy: desired/loading/resident bookkeeping, eviction rules, cancel /
  re-request asymmetry, promotion budget, loading contract (§7.6).

---

## 3. Toolchain and dependencies

```
Rust            stable ≥ 1.85 (edition 2021; the owner has 1.95)
Godot           4.7.x editor + export templates for the demo and the headless smoke test
gdext           crate `godot` 0.5.x  (0.5.5 as of Aug 2026), feature "api-4-4"
```

`rust/Cargo.toml`:

```toml
[package]
name = "godotiles"
version = "0.1.0"            # x-release-please-version
edition = "2021"
rust-version = "1.85"
description = "3D geospatial terrain streaming engine for Godot 4 (GDExtension)"
license = "MIT OR Apache-2.0"
repository = "https://github.com/ziv/godotiles"

[lib]
crate-type = ["cdylib", "rlib"]   # cdylib = the GDExtension; rlib = lets `cargo test` link the core

[dependencies]
godot = { version = "0.5", features = ["api-4-4"] }
crossbeam-channel = "0.5"
image = { version = "0.25", default-features = false, features = ["png", "jpeg"] }
ureq = "2"                        # blocking HTTP, rustls by default — runs on OUR worker threads
glam = "0.29"                     # f32/f64 vectors for the engine-agnostic core (no godot types there)

[dev-dependencies]
tempfile = "3"

[profile.release]
opt-level = 3
lto = "thin"

# debug builds of the demo must still decode/synthesize tiles at speed
[profile.dev.package."*"]
opt-level = 3
```

> gdext pins its own supported Godot range per release; if `api-4-4` is not offered by the
> installed `godot` crate version, choose the closest available `api-4-x` and set
> `compatibility_minimum` in the `.gdextension` file to the same version. API names in the code
> samples below are 0.5-era; adjust mechanically against
> <https://godot-rust.github.io/docs/gdext/master/godot/> if a signature moved.

Install: `rustup` (stable), Godot 4.7 editor (macOS: `brew install --cask godot`, or the archive
at <https://godotengine.org/download/archive/>). No emscripten, no SCons, no OpenSSL.

---

## 4. Repository layout

The repository root **is the Godot demo project** (Godot's Asset Library convention: the addon
lives at `addons/<name>` of a project), the Rust crate sits next to it.

```
godotiles/
├── project.godot                  Godot demo project (name "godotiles demo", main scene demo/main.tscn)
├── icon.svg / icon.png            project icon (reuse the raytiles/bevytiles icon)
├── addons/
│   └── godotiles/                 ← the distributable addon (this folder is what users copy)
│       ├── godotiles.gdextension
│       ├── bin/                   prebuilt libraries (git-ignored; CI artifacts / scripts/build.sh output)
│       │   ├── macos/libgodotiles.dylib          (universal: arm64 + x86_64)
│       │   ├── linux-x86_64/libgodotiles.so
│       │   └── windows-x86_64/godotiles.dll
│       ├── LICENSE-MIT, LICENSE-APACHE
│       └── README.md              short usage + attribution
├── demo/
│   ├── main.tscn                  fly demo (Grand Canyon)
│   ├── quick_start.tscn           minimal scene (mirror of raytiles' quick_start.cpp)
│   ├── fly_camera.gd
│   ├── demo.gd                    HUD, loading screen, crash check, rebase, debug toggles
│   └── debug_overlay.gd
├── rust/
│   ├── Cargo.toml, Cargo.lock
│   ├── src/
│   │   ├── lib.rs                 ExtensionLibrary entry + module tree
│   │   ├── core/                  ENGINE-AGNOSTIC — no `godot::` imports allowed (future shared crate)
│   │   │   ├── mod.rs
│   │   │   ├── config.rs          plain Rust config structs + defaults + geo anchoring
│   │   │   ├── lod.rs             pure desired-set policy (+ snapshot tests)
│   │   │   ├── synth.rs           Terrarium float decode / quadrant upsample / carry-safe encode
│   │   │   ├── height.rs          uint16 height grids + bilinear sample + ground_height walk
│   │   │   ├── frustum.rs         6-plane frustum vs height-column AABB test
│   │   │   ├── store.rs           tile lifecycle policy: reconcile / promote / update_desired / status
│   │   │   └── source/
│   │   │       ├── mod.rs         TileRequest / TilePayload / TileDrop, url expansion, decode, synthesis chain
│   │   │       └── native.rs      worker pool + ureq + disk cache + lineage backfill
│   │   └── godot/                 the Godot shell
│   │       ├── mod.rs
│   │       ├── configs.rs         TerrainWorldConfig / TerrainStreamingConfig / TerrainRenderingConfig / TerrainNetworkConfig (Resources)
│   │       ├── streamer.rs        TerrainStreamer (Node3D): frame loop, public API, signals
│   │       ├── gpu.rs             zoom meshes, texture/material creation, RS instances
│   │       └── shader.rs          `include_str!("../../shaders/terrain.gdshader")` + StringName cache
│   ├── shaders/terrain.gdshader   the terrain shader (embedded into the binary at compile time)
│   └── tests/
│       └── source_tests.rs        offline worker-pool integration tests (seeded caches, dead host)
├── tests/godot/
│   └── smoke.gd                   headless SceneTree script: seeded cache → tiles become resident
├── scripts/
│   ├── build.sh                   cargo build → copy into addons/godotiles/bin/<platform>/
│   ├── tiles-cache.mjs            cache pre-warmer (copied from raytiles, same layout)
│   └── tiles-cache-config.json
├── .github/workflows/
│   ├── linux.yml, macos.yml, windows.yml     build + cargo test (+ headless smoke on Linux)
│   ├── release.yml                            on tag: build all, package addons/godotiles.zip
│   └── release-please.yml
├── .gitignore                     /addons/godotiles/bin, /rust/target, /.cache, .godot/, .idea
├── release-please-config.json, .release-please-manifest.json
├── README.md, LICENSE-MIT, LICENSE-APACHE, SECURITY.md, CONTRIBUTING.md
└── plan.md                        this document
```

Rule for the split: **nothing under `rust/src/core/` may `use godot::`**. Vectors there are
`glam::Vec3` / `DVec2`; images are `image::RgbaImage` / `RgbImage` / raw `Vec<u8>`. The Godot
shell converts at its boundary. This is what makes the later core extraction a `git mv`.

---

## 5. Public surface (what GDScript / C# users see)

### 5.1 Quick start (GDScript)

```gdscript
# quick_start.gd — attach to a Node3D root that has a Camera3D child named "Camera3D"
extends Node3D

func _ready() -> void:
    var terrain := TerrainStreamer.new()
    terrain.world = TerrainWorldConfig.from_lat_lon(46.206889, 9.497194)  # the Dolomites
    terrain.rendering.fog_color = Color8(102, 191, 255)                    # match the sky
    terrain.camera_path = $Camera3D.get_path()
    add_child(terrain)

    $Camera3D.near = 1.0
    $Camera3D.far = 400000.0       # see to the horizon (the inspector caps at 4000; code does not)
    $Camera3D.position = terrain.initial_position(5000.0)
    $Camera3D.look_at($Camera3D.position + Vector3(-1000, -300, -1000), Vector3.UP)

func _process(_dt: float) -> void:
    # tiles cache under .cache/ next to the project (first run downloads)
    if $TerrainStreamer.is_loading:
        print("loading %.1f%%" % ($TerrainStreamer.loading_progress * 100.0))
```

Everything else is optional: the node has sensible defaults for a world anchored at tile
(306, 207) @ z9 (the Negev).

### 5.2 Classes

All classes are registered by the GDExtension; nothing needs an autoload or plugin.cfg.

#### `TerrainWorldConfig : Resource` — world topology (immutable once streaming started)

| Property | Type | Default | Meaning |
|---|---|---|---|
| `anchor_x` | int | 306 | Anchor tile X at `base_zoom`; the world origin sits at this tile's corner. |
| `anchor_z` | int | 207 | Anchor tile row (slippy `y`) at `base_zoom`. |
| `base_zoom` | int (range 9..22) | 9 | Lowest LOD zoom ever loaded. |
| `max_zoom` | int (range 9..22) | **15** | Highest LOD zoom. >15 opts into synthesized heightmaps + flat normals. Keep the default at 15 (the lod snapshots pin it). |
| `tile_size` | float | 66400.0 | World size (m) of one tile at `base_zoom`. |
| `skirt_overlap` | PackedFloat32Array (14 slots) | all 1.0 | Per-zoom mesh scale factor `[zoom − base_zoom]`. **A zero slot produces a degenerate mesh** — the node validates `> 0` for every used slot. |
| `mipmaps` | bool | true | Generate mipmaps for the albedo (anisotropic filtering comes from the shader hint). |
| `origin_offset` | Vector3 | (0,0,0) | Offset of the anchor point inside the anchor tile; filled by `from_lat_lon`. |

Static method `TerrainWorldConfig.from_lat_lon(lat: float, lon: float) -> TerrainWorldConfig`
(web-mercator at zoom 9, §7.1). Method `initial_position(altitude: float) -> Vector3` =
`origin_offset + Vector3(0, altitude, 0)` (also available on the node).

#### `TerrainStreamingConfig : Resource` — which tiles are kept resident

| Property | Type | Default |
|---|---|---|
| `radius` | int | 6 — disc radius in base-zoom tiles |
| `thresholds` | PackedFloat32Array (14) | `100000, 80000, 40000, 20000, 10000, 5000, 2500, 1250, 625, 312, 156, 78, 39, 20` — meters, slot `i` = zoom `base_zoom + i` |
| `update_distance` | float | 500.0 — camera travel (m) that triggers a desired-set rebuild |
| `upload_budget_ms` | float | 2.0 — wall-clock budget per frame for promotions |
| `max_uploads_per_frame` | int | 8 — hard cap on promotions per frame |

(raytiles' `near_plane`/`far_plane` are not needed: the frustum comes from the `Camera3D`.)

#### `TerrainRenderingConfig : Resource` — shader parameters, **all runtime-mutable**

| Property | Type | Default |
|---|---|---|
| `fog_start` | float | 100000.0 |
| `fog_end` | float | 150000.0 |
| `fog_color` | Color | `Color(0, 0, 1)` — match to your sky |
| `ambient_light` | Color | `Color(1, 1, 1)` |
| `sun_direction` | Vector3 | `(0.1, 1.0, 0.1)` — normalized in the shader |
| `sun_scale` | float | 1.0 |
| `height_scale` | float | 1.0 — relief exaggeration ("drama") |
| `normals_scale` | float | 1.0 |
| `skirt_drop` | float | 0.0 — vertical drop (m) of edge vertices; 0 disables |

Mutate any field at any time; the node compares against the last-applied values each frame and
pushes changes to every live material (§7.8). Swapping the whole resource also works.

#### `TerrainNetworkConfig : Resource` — download / cache

| Property | Type | Default |
|---|---|---|
| `threads` | int | 4 — worker threads (I/O-bound; more than cores is fine) |
| `cache_dir` | String | `".cache"` — relative paths resolve against the **project directory** (`ProjectSettings.globalize_path("res://")`), not the CWD, so the editor and exported builds agree |
| `texture_url` | String | `https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/:zoom:/:y:/:x:` |
| `heightmap_url` | String | `https://s3.amazonaws.com/elevation-tiles-prod/terrarium/:zoom:/:x:/:y:.png` |
| `normals_url` | String | `https://s3.amazonaws.com/elevation-tiles-prod/normal/:zoom:/:x:/:y:.png` |
| `native_terrain_zoom` | int (9..22) | 15 — above it heightmaps are synthesized, normals default, no HTTP for either |
| `connect_timeout_sec` | float | 5.0 |
| `read_timeout_sec` | float | 3.0 |

URL templates use `:zoom:`, `:x:`, `:y:` tokens (first occurrence replaced). Mapbox example:
`https://api.mapbox.com/v4/mapbox.satellite/:zoom:/:x:/:y:.pngraw?access_token=TOKEN`.
(raytiles' `allow_insecure_tls` is dropped: rustls verifies certificates, full stop.)

#### `TerrainStreamer : Node3D` — the engine

Exported properties (inspector):

| Property | Type | Default | Notes |
|---|---|---|---|
| `world` | TerrainWorldConfig | new default | read at `_ready`; changing it afterwards calls `rebuild()` (drops everything, re-creates meshes) |
| `streaming` | TerrainStreamingConfig | new default | read at `_ready` |
| `rendering` | TerrainRenderingConfig | new default | live |
| `network` | TerrainNetworkConfig | new default | read at `_ready` |
| `camera_path` | NodePath | empty | camera that drives streaming; empty → `get_viewport().get_camera_3d()` |
| `world_offset` | Vector3 | (0,0,0) | large-world rebase input: `absolute = user − world_offset`. Set it together with shifting your camera and every user-space node. |
| `enabled` | bool | true | pauses streaming + drawing (instances hidden) when false |

Read-only properties (`#[var(no_set)]`): `is_loading: bool` (true until the first desired set is
fully serviced), `loading_progress: float` (resident∩desired / desired, 0..1),
`resident_count: int`, `desired_count: int`, `loading_count: int`.

Methods:

| Method | Returns | Notes |
|---|---|---|
| `initial_position(altitude: float)` | Vector3 | `world.origin_offset + (0, altitude, 0)` |
| `ground_height(position: Vector3)` | Variant (float or null) | terrain altitude under a **user-space** point; `null` when no resident tile covers it. Callers fall back to their last value or 0. O(zoom levels). |
| `get_resident_tiles()` | Array[Dictionary] | debug: `{zoom, x, z, center: Vector3 (user space), size: float, desired: bool, visible: bool}` |
| `debug_point_in_frustum(position: Vector3)` | bool | debug: the engine's own frustum test for a user-space point (self-check of the plane sign convention; see §7.9) |
| `rebuild()` | void | drop all tiles, recreate meshes/source from the current configs |
| `set_fog_color(c)`, `set_fog_start(f)`, `set_fog_end(f)`, `set_ambient_light(c)`, `set_sun_direction(v)`, `set_sun_scale(f)`, `set_height_scale(f)`, `set_normals_scale(f)`, `set_skirt_drop(f)` | void | convenience setters that write through to `rendering` |

Signals: `initial_load_complete()`, `tile_promoted(zoom: int, x: int, z: int)`,
`tile_evicted(zoom: int, x: int, z: int)`, `tile_failed(zoom: int, x: int, z: int, reason: String)`.

Frame contract (same as raytiles): every frame the node reads the camera's global position,
converts it to absolute space with `world_offset`, and runs reconcile → promote → update-desired
→ cull → sync. Apply your camera movement **and** any rebase in your own `_process` (default
`process_priority` 0); the node uses `process_priority = 100` so it observes the finished frame
state. `ground_height()` uses the `world_offset` of the most recent frame.

---

## 6. Architecture

### 6.1 Components

```
TerrainStreamer (godot/streamer.rs)            frame conductor; Godot-facing API; owns everything below
  ├── TileStore (core/store.rs)                lifecycle policy: desired / loading / resident sets,
  │     │                                      eviction rules, promotion queue, status, height grids
  │     ├── lod (core/lod.rs)                  pure: desired_tiles(options, abs_position) → keys
  │     ├── HeightGrids (core/height.rs)       uint16 grids, ground_height(abs)
  │     └── TileSource (core/source/native.rs) worker threads: fetch (cache|HTTP) → decode →
  │                                            synthesize → height grid → whole payload on a channel
  ├── Gpu (godot/gpu.rs)                       per-zoom ArrayMesh, per-tile textures + ShaderMaterial +
  │                                            RenderingServer instance; transform baking; custom AABB
  └── frustum (core/frustum.rs)                visibility for eviction, from Camera3D.get_frustum()
```

Ownership is a tree. `TileStore` never sees a Godot type; it hands the shell *ops* (evict these
keys, upload this payload) and receives *facts* back (this key is now resident with this
absolute center; this key is visible).

### 6.2 Per-frame data flow (`TerrainStreamer::process`)

```
1. cam = resolve camera (camera_path or viewport camera); if none → return
   user_pos = cam.global_position ; abs = user_pos − world_offset
2. store.reconcile(abs, |key| gpu.visible[key])           → evicted keys → gpu.free(key)
3. store.drain()                                          (one non-blocking sweep of the source;
                                                          drops clear `loading`; cancelled-but-desired
                                                          keys are re-requested immediately)
   loop while (elapsed < upload_budget_ms && n < max_uploads_per_frame):
        payload = store.next_promotion()?                 (skips no-longer-desired payloads)
        gpu.upload(payload, world_offset)                 (3 textures, material, instance, AABB, transform)
        store.mark_resident(key, grid)                    (sets coverage_dirty)
4. if |abs − last_desired_pos| > update_distance:
        store.update_desired(abs)                         (lod → desired set; cancel stale loading keys;
                                                          request missing keys with absolute provider coords)
5. if world_offset != baked_offset: gpu.rebake_all(world_offset)   (f64 add, then f32 cast)
   frustum = Frustum::from_camera(cam.get_frustum())
   for each resident: gpu.visible[key] = frustum.intersects(user-space column AABB)
6. if rendering config changed: gpu.push_params(rendering)
   update status properties; emit initial_load_complete once
```

Why this order (load-bearing, inherited from raytiles): reconcile uses *last frame's* visibility
(cheap); promote runs before update-desired so freshly drained drops are visible to the
re-request logic; culling runs last so this frame's frustum classifies everything, including
tiles promoted this frame. Status is computed **after** update-desired — see trap §13.3.

Four gates keep steady-state frames near-free:

| Work | Runs when | Guard |
|---|---|---|
| desired-set rebuild + cancels + requests | camera moved > `update_distance` | distance check |
| coverage (`is_covered`) evictions | something was promoted or the desired set changed | `coverage_dirty` — exact, not heuristic: evictions only *remove* cover, which can only flip an answer toward "keep" |
| transform rebake (all instances) | `world_offset` changed | exact float compare vs `baked_offset` |
| shader parameter push (all materials) | rendering config changed | field-wise compare vs last applied |

### 6.3 Coordinate spaces

Two frames, one convention: **`absolute = user − world_offset`**.

| | user space | absolute space |
|---|---|---|
| who lives here | the app's camera, nodes, frustum planes | tile keys, tile centers, lod policy, store state |
| why | keeps floats small near the camera (Godot's default build is single precision) | tile math needs a fixed origin |
| where they meet | the instance transform — baked from `abs + offset` in **f64**, cast to f32 once | the node converts the camera once per frame |

Axes: Y up; tile column `x` → world `+X` (east); tile row `z` (slippy `y`) → world `+Z` (south).
Godot is right-handed, camera forward is `−Z`; that only matters to the demo's fly script.

The app owns rebasing (demo: every 4096 m): shift the camera **and** every user-space node
**and** `world_offset` by the same amount in the same frame.

### 6.4 Tile lifecycle

A tile is keyed by `TileKey { zoom: u8, x: i32, z: i32 }`, **anchor-relative**. Absolute provider
coordinates (`key.x + anchor_x · 2^(zoom − base_zoom)`, same for z) travel inside the
`TileRequest` — the source knows nothing about anchoring; dedup and cancellation are keyed by
`TileKey` end-to-end.

```
Desired ──request()──▶ Loading ──worker finishes all 3 assets──▶ Ready (payload on channel)
   ▲                      │                                          │
   │                      ├── fetch/decode failure ──▶ Dropped(Failed, reason)
   │                      └── cancel flag seen     ──▶ Dropped(Cancelled)
   │                                                                 ▼
   │                                              drained → promotion queue → (budgeted upload) → Resident
   │                                                                 │ no longer desired → payload discarded
   └── Dropped(Cancelled) but desired again → re-request immediately
       Dropped(Failed) → wait for the next desired-set rebuild (never hot-retry a failing server)
Resident ──reconcile rules──▶ evicted (instance freed, textures/material dropped, grid removed)
```

---

## 7. Module specifications

### 7.1 `core/config.rs`

Plain Rust mirrors of the four resources (the shell converts `Gd<Terrain*Config>` → these at
`_ready`/`rebuild`). Constants:

```rust
pub const MIN_ZOOM: u8 = 9;
pub const MAX_ZOOM: u8 = 22;
pub const ZOOM_LEVELS: usize = (MAX_ZOOM - MIN_ZOOM + 1) as usize; // 14
pub const MAX_WORLD_HEIGHT: f32 = 8848.0;     // Everest; sizes the culling column
pub const MIN_WORLD_HEIGHT: f32 = -450.0;     // Dead Sea shore, with margin
pub const MIN_RESOLUTION: u32 = 4;            // quads per side at base zoom
pub const MAX_RESOLUTION: u32 = 256;          // provider tiles are 256×256
pub const TILE_TEXELS: u32 = 256;
const EQUATOR_CIRCUMFERENCE_M: f64 = 40_075_016.686;
```

```rust
pub struct WorldConfig { pub anchor_x: i32, pub anchor_z: i32, pub base_zoom: u8, pub max_zoom: u8,
                         pub tile_size: f32, pub skirt_overlap: [f32; ZOOM_LEVELS], pub mipmaps: bool,
                         pub origin_offset: Vec3 }
pub struct StreamingConfig { pub radius: i32, pub thresholds: [f32; ZOOM_LEVELS], pub update_distance: f32,
                             pub upload_budget: Duration, pub max_uploads_per_frame: usize }
pub struct RenderingConfig { pub fog_start: f32, pub fog_end: f32, pub fog_color: [f32; 4], pub ambient: [f32; 4],
                             pub sun_direction: Vec3, pub sun_scale: f32, pub height_scale: f32,
                             pub normals_scale: f32, pub skirt_drop: f32 }   // PartialEq for the dirty check
pub struct NetworkConfig { pub threads: usize, pub cache_dir: PathBuf, pub texture_url: String,
                           pub heightmap_url: String, pub normals_url: String, pub native_terrain_zoom: u8,
                           pub connect_timeout: Duration, pub read_timeout: Duration }
```

Defaults exactly as in §5.2. Validation (`WorldConfig::validate() -> Result<(), String>`):
`MIN_ZOOM <= base_zoom <= max_zoom <= MAX_ZOOM`; `skirt_overlap[i] > 0` for
`i <= max_zoom − base_zoom`; `MIN_ZOOM <= native_terrain_zoom <= MAX_ZOOM`; `threads >= 1`.
The shell turns an `Err` into `godot_error!` + `enabled = false` (never panic inside Godot).

Geo anchoring (identical math in all ports):

```rust
impl WorldConfig {
    pub fn from_lat_lon(lat: f64, lon: f64) -> Self {
        let n = 2f64.powi(MIN_ZOOM as i32);              // anchor is ALWAYS computed at zoom 9
        let lat_rad = lat.to_radians();
        let x = (lon + 180.0) / 360.0 * n;
        let y = (1.0 - (lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n;
        let anchor_x = x.floor() as i32;
        let anchor_z = y.floor() as i32;
        let tile_size = EQUATOR_CIRCUMFERENCE_M * lat_rad.cos() / n;
        Self { anchor_x, anchor_z, tile_size: tile_size as f32,
               origin_offset: Vec3::new(((x - anchor_x as f64) * tile_size) as f32, 0.0,
                                        ((y - anchor_z as f64) * tile_size) as f32),
               ..Default::default() }
    }
    pub fn initial_position(&self, altitude: f32) -> Vec3 { self.origin_offset + Vec3::Y * altitude }
    pub fn zoom_size(&self, zoom: u8) -> f64 { f64::from(self.tile_size) / f64::from(1u32 << (zoom - self.base_zoom)) }
}
```

Note `from_lat_lon` assumes `base_zoom == MIN_ZOOM` (as both earlier ports do). Tests (pin
these; computed with the formula above):

| place | lat, lon | anchor (x, z) | tile_size (m) | origin_offset (x, z) |
|---|---|---|---|---|
| Dolomites | 46.206889, 9.497194 | (269, 181) | 54 168.30 | (27 469.85, 39 307.58) |
| Grand Canyon (demo) | 35.97391, −113.76892 | (94, 201) | 63 343.93 | (12 371.94, 6 394.49) |
| Negev | 30.82691969, 34.91386236 | (305, 209) | 67 213.26 | (44 042.89, 58 796.15) |
| London | 51.5074, 0.1278 | (256, 170) | 48 717.25 | (8 854.85, 12 328.76) |

(Tolerance: tile_size ±0.01 m, offsets ±0.1 m — the offsets are cast to f32.)

### 7.2 `core/lod.rs` — pure, port exactly

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileKey { pub zoom: u8, pub x: i32, pub z: i32 }

pub struct LodOptions { pub base_zoom: u8, pub max_zoom: u8, pub base_tile_size: f32,
                        pub radius: i32, pub thresholds: [f32; ZOOM_LEVELS] }

const HORIZON_RATIO_M: f64 = 3570.0;   // d ≈ 3.57 km · √h  ⇒  d² = 3570² · h  (h clamped ≥ 1)
pub fn horizon_sq(cam_y: f32) -> f64 { HORIZON_RATIO_M * HORIZON_RATIO_M * f64::from(cam_y.max(1.0)) }

/// squared distance camera → tile center, INCLUDING the camera height (the altitude term is what
/// collapses LOD when flying high)
fn dist_sq_to_tile(cam: Vec3, zoom_size: f64, x: i32, z: i32) -> f64 {
    let cx = (f64::from(x) + 0.5) * zoom_size;  let cz = (f64::from(z) + 0.5) * zoom_size;
    let dx = f64::from(cam.x) - cx;             let dz = f64::from(cam.z) - cz;
    dx * dx + dz * dz + f64::from(cam.y) * f64::from(cam.y)
}
/// XZ-only variant, used by eviction's beyond-horizon rule
pub fn dist_sq_to_tile_xz(cam: Vec3, zoom_size: f64, x: i32, z: i32) -> f64 { /* same without y² */ }
pub fn out_of_horizon(cam: Vec3, zoom_size: f64, key: TileKey) -> bool {
    dist_sq_to_tile_xz(cam, zoom_size, key.x, key.z) > horizon_sq(cam.y)
}

/// Appends the desired keys for `cam` (ABSOLUTE space) to `out`; does not clear `out`;
/// duplicate-free; allocation-free in steady state when the caller reuses the vector.
pub fn desired_tiles(opts: &LodOptions, cam: Vec3, out: &mut Vec<TileKey>)
```

Algorithm:

1. Per zoom `i = zoom − base_zoom`: `size[i] = base_tile_size / 2^i`;
   `threshold_sq[i] = (thresholds[i] as f64)²` — **square in f64** (an f32 square can overflow;
   a CodeQL finding in raytiles).
2. Camera base tile: `floor(cam.x / base_tile_size)`, `floor(cam.z / base_tile_size)` (f64).
   Scan `dx, dz ∈ [−radius, radius]` with `dx² + dz² < (radius − 1)²` (strict).
3. `render_radius_sq = horizon_sq(cam.y)`.
4. Recurse `build(zoom, x, z)`:
   - if `zoom == max_zoom` → **accept**, return;
   - `d = dist_sq_to_tile(cam, size[i], x, z)`;
   - if `d > render_radius_sq` → **reject** (beyond horizon), return;
   - if `d >= threshold_sq[i]` → **accept**, return;
   - else recurse into the 4 children `(zoom+1, 2x+ox, 2z+oz)` for `oz in 0..2, ox in 0..2`.

Order of checks is load-bearing. Known quirks to preserve: the horizon cap is measured to *tile
centers*, so at very low altitude near a tile corner the camera's own base tile can be rejected;
the `y²` term is in the subdivision distance but not in the horizon test.

Tests (`#[cfg(test)]` in the module):

- **structural invariants** over probe positions `xz ∈ {(0,0), (0.5,0.5), (0.25,0.75), (3.3,−2.7), (−1,−1), (7.9,0.1), (−5.55,4.05)} × ts` and altitudes `{2, 500, 5000, 60000}`
  (ts = 66 400): duplicate-free; `base_zoom ≤ zoom ≤ max_zoom`; no key together with an
  ancestor; every key descends from a base tile inside the scanned disc.
- **snapshots** (default options; values identical in C++ and Rust — pin them):

  | camera (absolute) | total keys | detail |
  |---|---|---|
  | (0, 500, 0) | **252** | 0 at z9, **64** at z15; contains `{15,0,0}` and `{15,−1,−1}` |
  | (33200, 5000, 33200) | **252** | **36** at z9, 0 at z15 |
  | (0, 60000, 0) | **117** | **48** at z11, 0 at z15 |

- `max_zoom = 17`, camera (33200, 500, 33200) → at least one key with `zoom > 15`.
- non-default options case: `base_zoom 11, max_zoom 14, base_tile_size 16600, radius 4,
  thresholds [40000, 20000, 10000, 5000, 2500, …]` runs and holds the invariants.

### 7.3 `core/synth.rs` — pure, port exactly

Cardinal rule: **never interpolate Terrarium RGB directly**. `h` is linear in (r,g,b), so GPU
texture filtering is fine, but any *re-encoding* after interpolation must go through floats and a
single fixed-point quantization (per-channel rounding spikes at g-wrap boundaries).

```rust
pub fn decode_terrarium_floats(img: &RgbImage) -> Vec<f32>      // r*256 + g + b/256 − 32768
/// Upsample quadrant (qx, qz ∈ {0,1}) of a w×h grid 2× into a full w×h grid. Texel-center
/// aligned: dst texel i samples src coord q·w/2 + (i + 0.5)/2 − 0.5, bilinear, edges clamped.
/// Two siblings sharing an edge sample adjacent source positions — continuous by construction.
pub fn upsample_quadrant(src: &[f32], w: usize, h: usize, qx: usize, qz: usize) -> Vec<f32>
/// Carry-safe: fixed = clamp(round((h + 32768) · 256), 0, 0xFFFFFF); r = fixed>>16, g = (fixed>>8)&255, b = fixed&255
pub fn encode_terrarium(heights: &[f32], w: u32, h: u32) -> RgbImage
pub fn default_normals(size: u32) -> RgbImage                    // solid (128, 128, 255) → up-normal
```

Tests: round-trip of `[0, 8848, −415, 100.5, 1234.25, 255.99609375, 256.0, −0.00390625]` within
1/256 m; `255.998` encodes to fixed `8454143` or `8454144` exactly; 8×8 ramp `h = 16·x` →
`q0[2] = 12.0`, `q0[3] = 20.0`, `q0[0] = 0.0` (clamped), rows identical; sibling continuity
`q0[w−1] = 52.0`, `q1[0] = 60.0` (differ by exactly half a source step = 8); vertical quadrant
mirror (`h = 100·y`: `top[0] = 0`, `bottom[0] = 175`, `bottom[(h−1)·w] = 300`); a 7-level
z15→z22 chain on a 16×16 gradient re-encodes/decodes within 1/256 m; default normals pixel check.

### 7.4 `core/height.rs`

```rust
pub struct HeightGrid { pub w: u32, pub h: u32, pub samples: Vec<u16> }   // round(h_m) + 32768 ; 128 KB/tile
impl HeightGrid {
    pub fn from_terrarium(img: &RgbImage) -> Self        // decode in f64, round, clamp 0..65535
    pub fn sample(&self, u: f32, v: f32) -> f32          // bilinear; texel centers at (i+0.5)/n; edges clamp;
}                                                        //   fx = u·w − 0.5, fy = v·h − 0.5; returns meters
pub struct HeightGrids(pub HashMap<TileKey, HeightGrid>);
/// `abs` = absolute-space point. Walks zooms max→base, first resident grid containing (x, z) wins.
pub fn ground_height(grids: &HeightGrids, world: &WorldConfig, abs: Vec3) -> Option<f32> {
    for zoom in (world.base_zoom..=world.max_zoom).rev() {
        let size = world.zoom_size(zoom);
        let tx = (f64::from(abs.x) / size).floor() as i32;  let tz = (f64::from(abs.z) / size).floor() as i32;
        if let Some(g) = grids.0.get(&TileKey { zoom, x: tx, z: tz }) {
            let u = ((f64::from(abs.x) - tx as f64 * size) / size) as f32;
            let v = ((f64::from(abs.z) - tz as f64 * size) / size) as f32;
            return Some(g.sample(u, v));
        }
    }
    None
}
```

Grids are built **on the worker** (right after the heightmap decode) so the main thread never
pays for them. Tests: 2×1 grid `[100, 300]` → `sample(0.25, 0.5) = 100`, `(0.75, 0.5) = 300`,
`(0.5, 0.5) = 200`, `(0, 0) = 100`, `(1, 1) = 300`; `from_terrarium` of
`encode_terrarium([0, 8848, −415, 100.5])` → `32768, 32768+8848, 32768−415, 32868|32869`.

### 7.5 `core/source/` — keep the worker design; port bevytiles' `native.rs`

Do **not** use Godot's `WorkerThreadPool`, `HTTPRequest`, `HTTPClient` or `Image` decoding
here: workers must be free of Godot calls (thread-safety rules), and the raytiles pool semantics
(dedup, cancel flags, real-job priority over background synthesis, one drain per frame) are
proven. Plain `std::thread` + `crossbeam-channel` + `ureq` + `image` + `std::fs`.

```rust
pub struct TileRequest { pub key: TileKey, pub x: i32, pub z: i32 }   // x/z = absolute provider coords
pub struct TilePayload { pub key: TileKey,
                         pub albedo: RgbaImage,   // decoded imagery (PNG or JPEG — Esri serves JPEG)
                         pub height: RgbImage,    // Terrarium, fetched or synthesized
                         pub normals: RgbImage,   // fetched or flat default
                         pub grid: HeightGrid }
pub enum TileDrop { Cancelled(TileKey), Failed(TileKey, String) }
pub struct TileSource { /* cfg: Arc<NetworkConfig>, in_flight, derive_done, jobs_tx, derive_tx, payload_rx, drop_rx, workers */ }
impl TileSource {
    pub fn new(cfg: &NetworkConfig) -> Result<Self, String>;   // validates native_terrain_zoom; spawns max(1, threads) workers
    pub fn request(&self, req: TileRequest);                   // no-op while key in flight
    pub fn cancel(&self, key: TileKey);                        // flips the job's AtomicBool
    pub fn drain(&self, ready: &mut Vec<TilePayload>, dropped: &mut Vec<TileDrop>); // try_iter, non-blocking
}
impl Drop for TileSource { /* drop senders, join workers */ }
```

Worker rules (all load-bearing):

- **One job fetches all three assets sequentially**: texture → heightmap → normals; cancel flag
  checked at pickup and between assets. A tile arrives whole or not at all.
- **Dedup**: `Mutex<HashMap<TileKey, Arc<AtomicBool>>>` of in-flight jobs; `request` is a no-op
  while the key is present (pending, executing, or cancelled-but-uncollected). Every request is
  answered **exactly once** — payload or drop — which is what lets the store's `loading` set be a
  plain set.
- **Priority**: a worker always `try_recv`s the job queue first; the background derive queue is
  served only when no real job waits; otherwise `select!` on both.
- **Cache-or-HTTP raw bytes**: path `cache_dir/<kind>/<zoom>/<x>/<z>.png` (`kind ∈ texture |
  heightmap | normals`); on miss GET the expanded URL with `ureq` agent (connect/read timeouts from
  config; `set_follow_location` default on; cap the body at 32 MiB), then **atomic write-through**:
  write `path.tmp<N>` with a process-wide `AtomicU64` counter suffix, then `rename`. The unique
  suffix matters: a background derive task can race a direct request on the same path (the rename
  race itself is benign — identical bytes).
- **Decode** with `image::ImageReader::new(Cursor).with_guessed_format()` (PNG *and* JPEG; the
  cache file keeps the `.png` name regardless of the real format — this matches the other ports
  and the pre-warm script). Albedo → `into_rgba8()`; height/normals → `into_rgb8()`.
- **Heightmap synthesis** (`zoom > native_terrain_zoom`): cache hit → decode. Miss → fetch the
  native ancestor `(native, x >> dz, z >> dz)` with `dz = zoom − native` (cache-or-HTTP, cached as
  usual) → `decode_terrarium_floats` → for `level in native+1..=zoom`:
  `shift = zoom − level; floats = upsample_quadrant(floats, w, h, (x >> shift) & 1, (z >> shift) & 1)`
  → `encode_terrarium` → PNG-encode → atomic cache write → `enqueue_derive` → serve. **No HTTP is
  ever attempted for heightmaps above the native zoom.**
- **Background lineage backfill** (`DeriveTask { zoom, x, z }`, deduped per requested key in
  `derive_done`): re-read the cached native parent; for each level along the target's ancestry
  generate the **4 children of the lineage node** (skip existing files), descend into the lineage
  child. 28 PNGs at z22 — never the full subtree (~21 800 tiles / ~1 GB per parent). Best effort:
  any error → return silently.
- **Normals never fail a tile**: cache file honored at any zoom; above native → in-memory
  `default_normals(256)`, no HTTP, no cache write; at/below native → cache → HTTP → decode, and
  **any** failure (404, timeout, corrupt) → default. Texture/heightmap failures drop the tile with
  a reason string.
- **Shutdown**: dropping `TileSource` drops the senders; workers exit when the queues are empty
  and disconnected; an in-flight HTTP read may delay this by its read timeout. Document it.

Integration tests (`rust/tests/source_tests.rs`, offline; `threads: 1` for deterministic
pickup order; URLs point at a dead host `http://127.0.0.1:9/...`; `connect_timeout` 300 ms;
`tempfile::tempdir()` caches; a `pump_until(pred, 10 s)` harness that drains every 5 ms):

1. cache hit delivers the whole payload without network (texture 2×2 RGBA seeded, gradient
   Terrarium 16×16 seeded, normals unseeded → flat default; grid has 256 samples);
2. missing heightmap → `Failed` drop, no payload;
3. z16 request with only the z15 ancestor seeded → payload heightmap byte-identical to an
   independent `encode(upsample(gradient, 1, 0))`; child PNG cached; all 4 siblings appear on disk
   (backfill);
4. z17 request derives through the chain (quadrants (1,0) then (0,1)), lineage backfill writes
   the 4 z16 children of the ancestor and the 4 z17 children of the lineage z16 node, and a cousin
   outside the lineage is **not** pre-generated;
5. corrupt z15 ancestor bytes → `Failed` drop;
6. two identical requests while in flight → at most one extra answer (racy dedup tolerance);
7. cancel before pickup (queue two jobs on the single worker, cancel the second immediately) →
   `Cancelled` drop, no payload.

### 7.6 `core/store.rs` — engine-agnostic lifecycle policy

This is the module both earlier ports re-implemented and where their subtle bugs lived. It owns
the bookkeeping; the shell owns GPU resources.

```rust
pub struct ResidentInfo { pub abs_x: f64, pub abs_z: f64, pub size: f32, pub desired: bool, pub visible: bool }

pub struct TileStore {
    world: WorldConfig, streaming: StreamingConfig, lod_opts: LodOptions,
    source: TileSource,
    desired: HashSet<TileKey>, loading: HashSet<TileKey>,
    resident: HashMap<TileKey, ResidentInfo>,
    grids: HeightGrids,
    pending: VecDeque<TilePayload>,              // drained but not yet promoted
    ready_scratch: Vec<TilePayload>, dropped_scratch: Vec<TileDrop>, desired_scratch: Vec<TileKey>,
    last_desired_pos: Option<Vec3>,
    coverage_dirty: bool,                        // starts true
    loading_flag: bool,                          // starts true
    failures: Vec<(TileKey, String)>,            // drained by the shell for signals/logging
}
pub struct Status { pub loading: bool, pub progress: f32, pub resident: usize, pub desired: usize, pub loading_count: usize }

impl TileStore {
    pub fn new(world: WorldConfig, streaming: StreamingConfig, network: &NetworkConfig) -> Result<Self, String>;
    pub fn world(&self) -> &WorldConfig;
    pub fn zoom_size(&self, zoom: u8) -> f64;

    /// Evict residents that are NOT desired AND (base zoom | !visible | beyond horizon | covered).
    /// Returns evicted keys (already removed from `resident` and `grids`).
    pub fn reconcile(&mut self, abs_cam: Vec3) -> Vec<TileKey>;

    /// One non-blocking sweep of the source. Drops clear `loading`; failures are recorded;
    /// cancelled-but-desired-again keys (not resident) are re-requested immediately; payloads join `pending`.
    pub fn drain(&mut self);

    /// Next payload that is still desired and not resident (skips/discards others, clearing `loading`).
    pub fn next_promotion(&mut self) -> Option<TilePayload>;

    /// The shell finished uploading `key`. Registers residency, stores the grid, sets coverage_dirty.
    pub fn mark_resident(&mut self, key: TileKey, grid: HeightGrid);

    /// Movement-gated rebuild; returns true if it ran.
    pub fn maybe_update_desired(&mut self, abs_cam: Vec3) -> bool;
    pub fn update_desired(&mut self, abs_cam: Vec3);

    pub fn set_visible(&mut self, key: TileKey, visible: bool);
    pub fn resident_iter(&self) -> impl Iterator<Item = (&TileKey, &ResidentInfo)>;
    pub fn ground_height(&self, abs: Vec3) -> Option<f32>;
    pub fn status(&mut self) -> Status;          // flips `loading_flag` per the contract below
    pub fn take_failures(&mut self) -> Vec<(TileKey, String)>;
}
```

Rules:

- **reconcile**: for each resident `key` not in `desired`, evict when the first true of:
  `key.zoom == base_zoom` (stale base tiles are the horizon); `!info.visible` (last frame's
  frustum result); `out_of_horizon(abs_cam, zoom_size, key)`; else if `!coverage_dirty` keep;
  else `is_covered(key)`. Clear `coverage_dirty` at the end. **Evaluate candidates
  sequentially, coarse-first (sorted by zoom), against the live resident map** — not against a
  snapshot: when a parent and all its children are stale, the parent is covered and goes, and
  the children are then uncovered and stay. A snapshot evaluation evicts both — a hole. (This
  is a refinement over both earlier ports, which evaluated a snapshot.)
- **is_covered(key)** (with `has(zoom,x,z) = resident.contains`): parent
  `(zoom−1, x>>1, z>>1)` if `zoom > base`; all four children `(zoom+1, 2x+{0,1}, 2z+{0,1})` if
  `zoom < max`; grandparent `(zoom−2, x>>2, z>>2)` if `zoom−1 > base`; all sixteen grandchildren
  `(zoom+2, 4x+{0..3}, 4z+{0..3})` if `zoom+1 < max` — rare, but happens when zoom levels are
  skipped during fast movement.
- **drain**: `source.drain(ready, dropped)`; `Failed(key, reason)` → `loading.remove`, push to
  `failures`; `Cancelled(key)` → `loading.remove`, and if `desired.contains(key) &&
  !resident.contains(key)` → `request(key)` right away (the camera came back); payloads →
  `pending.push_back`.
- **next_promotion**: pop from the front; `loading.remove(key)`; skip while
  `!desired.contains(key) || resident.contains(key)`.
- **update_desired**: `lod::desired_tiles` into `desired_scratch` → rebuild `desired`;
  `coverage_dirty = true`; refresh `info.desired` on residents; **cancel once, here** every
  `loading` key not in `desired` (the set changes only here); request every desired key that is
  neither resident nor loading. `request(key)`: `scale = 1 << (zoom − base_zoom)`;
  `TileRequest { key, x: key.x + anchor_x·scale, z: key.z + anchor_z·scale }`; `loading.insert`.
- **maybe_update_desired**: `last_desired_pos` starts `None` (always runs on the first frame);
  otherwise runs when `|abs − last|² > update_distance²`.
- **status / loading contract**: `progress = |desired ∩ resident| / |desired|` (0 while desired
  is empty). `loading_flag` flips to false **only** when `!desired.is_empty() && loading.is_empty()
  && pending.is_empty()`. The `desired` guard is load-bearing: `status` is evaluated after
  `update_desired` in the frame order, but the guard makes it correct regardless (raytiles shipped
  a bug where the flag flipped on frame one before anything was requested).

Unit tests with a fake source: replace `TileSource` behind a small trait
(`trait Source { fn request; fn cancel; fn drain }`) so tests can script payloads/drops:
promotion of an undesired payload is discarded; cancelled-then-desired re-requests; failed does
not re-request until the next rebuild; `loading` never flips before a desired set exists; covered
tile evicted only when `coverage_dirty`; parent/children/grandparent/grandchildren coverage cases.

### 7.7 `godot/configs.rs` — the four Resources

gdext 0.5 shape (one shown; the others follow the tables in §5.2):

```rust
use godot::prelude::*;

#[derive(GodotClass)]
#[class(init, base = Resource)]
pub struct TerrainRenderingConfig {
    #[export] #[init(val = 100_000.0)] pub fog_start: f32,
    #[export] #[init(val = 150_000.0)] pub fog_end: f32,
    #[export] #[init(val = Color::from_rgb(0.0, 0.0, 1.0))] pub fog_color: Color,
    #[export] #[init(val = Color::WHITE)] pub ambient_light: Color,
    #[export] #[init(val = Vector3::new(0.1, 1.0, 0.1))] pub sun_direction: Vector3,
    #[export] #[init(val = 1.0)] pub sun_scale: f32,
    #[export] #[init(val = 1.0)] pub height_scale: f32,
    #[export] #[init(val = 1.0)] pub normals_scale: f32,
    #[export] #[init(val = 0.0)] pub skirt_drop: f32,
    base: Base<Resource>,
}
impl TerrainRenderingConfig {
    pub fn to_core(&self) -> RenderingConfig { /* Color → [f32;4] (sRGB components as given; the shader's `source_color` hint linearizes) */ }
}
```

- Zoom fields: `#[export(range = (9.0, 22.0))] base_zoom: i32` (convert to `u8` in `to_core`).
- Array fields: `#[export] thresholds: PackedFloat32Array` initialized with the 14 defaults;
  `to_core` copies the first 14 values and pads missing slots with the default table (never with 0).
- `TerrainWorldConfig`: `#[func] fn from_lat_lon(lat: f64, lon: f64) -> Gd<Self>` — an
  associated fn without `self` inside a `#[godot_api] impl` is registered as a **static** method
  (`TerrainWorldConfig.from_lat_lon(...)` in GDScript); plus
  `#[func] fn initial_position(&self, altitude: f32) -> Vector3`.
- `TerrainNetworkConfig::to_core` resolves `cache_dir`: if relative, join to
  `ProjectSettings::singleton().globalize_path("res://")`.

### 7.8 `godot/gpu.rs` — meshes, textures, materials, instances

**Per-zoom meshes** (built once at `_ready`/`rebuild`, kept in `Vec<Gd<ArrayMesh>>` indexed
`zoom − base_zoom`): a grid on the XZ plane, centered at the origin, `res` quads per side
(`res = 4` at base zoom, doubling per zoom, capped at 256), extent
`extent = zoom_size(zoom) · skirt_overlap[i]`, **UVs spanning exactly [0, 1]** (the vertex shader
samples the heightmap by UV): vertex `(ix, iz) ∈ [0, res]²` at
`(−extent/2 + ix·extent/res, 0, −extent/2 + iz·extent/res)`, `uv = (ix/res, iz/res)`,
normal `(0, 1, 0)`. Build with `ArrayMesh::add_surface_from_arrays(PrimitiveType::TRIANGLES, arrays)`
where `arrays` is a `VarArray` of size `ArrayType::MAX` with `ARRAY_VERTEX` (PackedVector3Array),
`ARRAY_NORMAL` (PackedVector3Array), `ARRAY_TEX_UV` (PackedVector2Array), `ARRAY_INDEX`
(PackedInt32Array). Indices per quad with corners `a=(ix,iz) b=(ix+1,iz) c=(ix,iz+1) d=(ix+1,iz+1)`:
`[a, b, d, a, d, c]`. Godot's front face is **clockwise**; the shader uses `cull_back`.
Verification step: the terrain must be visible from above — if it is not, reverse each triangle.
(Do not use `PlaneMesh`: its UV orientation and subdivision semantics are not worth verifying.)

**Textures** (main thread only, inside the promotion budget):

```rust
fn make_texture(rgba: &[u8], w: u32, h: u32, mipmaps: bool) -> Option<Gd<ImageTexture>> {
    let data = PackedByteArray::from(rgba);
    let mut img = Image::create_from_data(w as i32, h as i32, false, image::Format::RGBA8, &data)?;
    if mipmaps { img.generate_mipmaps(); }
    ImageTexture::create_from_image(&img)
}
```

All three tiles as RGBA8 (workers convert RGB → RGBA; one code path, native GPU format). Mipmaps
only for the albedo (and only if `world.mipmaps`). Godot's `Image` carries no color-space flag —
sRGB vs linear is decided by the **sampler hint in the shader** (§7.10), not here.

**Materials**: one shared `Gd<Shader>` (created in `_ready` from `include_str!`), one
`Gd<ShaderMaterial>` per tile: `set_shader(&shader)`, then `set_shader_parameter("albedo_tex",
&tex.to_variant())` ×3 and the nine rendering parameters (cache the `StringName`s in
`shader.rs`). `push_params(&RenderingConfig)` iterates all resident materials — rare event.

**Instances** (`RenderingServer::singleton()`, abbreviated `rs`):

```rust
let inst = rs.instance_create();
rs.instance_set_base(inst, mesh.get_rid());
rs.instance_set_scenario(inst, node.get_world_3d().unwrap().get_scenario());
rs.instance_geometry_set_material_override(inst, material.get_rid());
rs.instance_geometry_set_cast_shadows_setting(inst, ShadowCastingSetting::OFF);
rs.instance_set_custom_aabb(inst, Aabb { position: Vector3::new(-half, MIN_WORLD_HEIGHT, -half),
                                         size: Vector3::new(2.0 * half, MAX_WORLD_HEIGHT - MIN_WORLD_HEIGHT + 300.0, 2.0 * half) });
rs.instance_set_transform(inst, Transform3D::new(Basis::IDENTITY, Vector3::new(ux, 0.0, uz)));
```

`half = zoom_size · skirt_overlap / 2` (the AABB is **local** to the instance). The user-space
translation `ux = (abs_x + world_offset.x as f64) as f32` (f64 add, single cast; same for z), with
`abs_x = (key.x as f64 + 0.5) · zoom_size`. Resident record:

```rust
struct GpuTile { instance: Rid, material: Gd<ShaderMaterial>, albedo: Gd<ImageTexture>, height: Gd<ImageTexture>, normals: Gd<ImageTexture> }
```

Eviction: `rs.free_rid(instance)` then drop the record (ref-counted resources free themselves).
`exit_tree` frees every instance (and drops the store, which joins the workers). `enabled = false`
→ `instance_set_visible(inst, false)` for all.

**Rebake**: when `world_offset != baked_offset`, `instance_set_transform` for every resident from
its stored `abs_x/abs_z`. Exact float compare is correct: the app passes the same bits every
frame until it rebases.

### 7.9 `core/frustum.rs`

```rust
pub struct Plane { pub normal: Vec3, pub d: f32 }        // Godot convention: normal·p = d
pub struct Frustum { pub planes: [Plane; 6] }
impl Frustum {
    /// Column AABB test in user space; Godot's frustum planes have OUTWARD normals: a point is inside
    /// iff `normal·p − d <= 0` for all six planes. Test the AABB corner most along each normal.
    pub fn intersects_column(&self, center_x: f32, center_z: f32, half: f32) -> bool {
        let min = Vec3::new(center_x - half, MIN_WORLD_HEIGHT, center_z - half);
        let max = Vec3::new(center_x + half, MAX_WORLD_HEIGHT, center_z + half);
        self.planes.iter().all(|p| {
            let c = Vec3::new(if p.normal.x > 0.0 { min.x } else { max.x },   // the corner LEAST along the normal
                              if p.normal.y > 0.0 { min.y } else { max.y },
                              if p.normal.z > 0.0 { min.z } else { max.z });
            p.normal.dot(c) - p.d <= 0.0
        })
    }
}
```

The shell fills it from `camera.get_frustum()` (an `Array<godot::builtin::Plane>` in world =
user space; order near, far, left, top, right, bottom). Verification test in the shell: the
camera's position + 10·forward must be inside; position − 10·forward must be outside. If the
opposite holds, the sign convention flipped in the engine — invert the comparison, don't guess.

### 7.10 `rust/shaders/terrain.gdshader` — the terrain shader (port of the GLSL/WGSL pair)

```glsl
shader_type spatial;
render_mode unshaded, cull_back, depth_draw_opaque, fog_disabled;

// albedo: color data → source_color (sRGB→linear on sample), mipmapped + anisotropic
uniform sampler2D albedo_tex : source_color, filter_linear_mipmap_anisotropic, repeat_disable;
// heightmap + normals: RAW bytes. NO source_color, NO hint_normal — or the Terrarium decode is gamma-warped garbage
uniform sampler2D height_tex : filter_linear, repeat_disable;
uniform sampler2D normal_tex : filter_linear, repeat_disable;

uniform vec4 fog_color : source_color = vec4(0.0, 0.0, 1.0, 1.0);
uniform vec4 ambient_light : source_color = vec4(1.0);
uniform vec3 sun_direction = vec3(0.1, 1.0, 0.1);
uniform float sun_scale = 1.0;
uniform float fog_start = 100000.0;
uniform float fog_end = 150000.0;
uniform float height_scale = 1.0;
uniform float normals_scale = 1.0;
uniform float skirt_drop = 0.0;

varying float cam_dist;

// Terrarium decode: h = r*256 + g + b/256 − 32768 (channels are 0..1 here).
// Must stay in lockstep with the CPU decoders (synth.rs, height.rs) — one constant set, three places.
float terrarium_height(vec2 uv) {
    vec3 c = textureLod(height_tex, uv, 0.0).rgb * 255.0;   // textureLod: implicit-derivative texture() is not allowed in vertex()
    return c.r * 256.0 + c.g + c.b / 256.0 - 32768.0;
}

void vertex() {
    VERTEX.y += terrarium_height(UV) * height_scale;

    // edge vertices drop by the skirt amount to hide LOD cracks
    float e = 0.000001;
    float edge = clamp(step(UV.x, e) + step(1.0 - e, UV.x) + step(UV.y, e) + step(1.0 - e, UV.y), 0.0, 1.0);
    VERTEX.y -= skirt_drop * height_scale * edge;

    vec3 world_pos = (MODEL_MATRIX * vec4(VERTEX, 1.0)).xyz;
    cam_dist = distance(world_pos, CAMERA_POSITION_WORLD);   // user space, same frame as the instance transform
}

void fragment() {
    vec4 tex = texture(albedo_tex, UV);

    vec3 n = texture(normal_tex, UV).rgb * 2.0 - 1.0;
    n.xy *= normals_scale;
    n = normalize(n);

    float sun = max(dot(n, normalize(sun_direction)), 0.0) * sun_scale;
    vec4 lighting = clamp(ambient_light + vec4(sun), vec4(0.0), vec4(1.0));
    vec4 lit = tex * lighting;

    float fog = clamp((cam_dist - fog_start) / (fog_end - fog_start), 0.0, 1.0);
    ALBEDO = mix(lit, fog_color, fog).rgb;
}
```

Notes:

- `unshaded` keeps Godot's lights, shadows and its own fog out; `fog_disabled` additionally
  ignores the `Environment` fog so the engine's fog is the only one.
- The lighting math is intentionally the raytiles "look" (normal-map `z` treated as up against a
  `y`-up sun vector, additive sun). Port verbatim for parity; do not "fix" it.
- Because `source_color` linearizes the albedo, ambient and fog colors, the multiply happens in
  linear space (raylib multiplied in sRGB space). Expect slightly higher contrast; the demo tunes
  `ambient_light` to `Color8(200, 200, 200)` like bevytiles.
- Bilinear filtering of the Terrarium texture is fine (`h` is linear in the channels); GPU
  filter precision limits the `r` channel to ~1 m steps between texels, which matches the data.
- `CAMERA_POSITION_WORLD` and `MODEL_MATRIX` are available in `vertex()`.

### 7.11 `godot/streamer.rs` — the node

```rust
#[derive(GodotClass)]
#[class(base = Node3D)]
pub struct TerrainStreamer {
    #[export] world: Option<Gd<TerrainWorldConfig>>,
    #[export] streaming: Option<Gd<TerrainStreamingConfig>>,
    #[export] rendering: Option<Gd<TerrainRenderingConfig>>,
    #[export] network: Option<Gd<TerrainNetworkConfig>>,
    #[export] camera_path: NodePath,
    #[export] world_offset: Vector3,
    #[export] enabled: bool,
    #[var(no_set)] is_loading: bool,
    #[var(no_set)] loading_progress: f32,
    #[var(no_set)] resident_count: i32,
    #[var(no_set)] desired_count: i32,
    #[var(no_set)] loading_count: i32,

    store: Option<TileStore>,            // None until _ready succeeded
    gpu: Gpu,                            // meshes, shader, resident GpuTiles, baked_offset, StringNames
    last_rendering: RenderingConfig,
    load_complete_emitted: bool,
    base: Base<Node3D>,
}

#[godot_api]
impl INode3D for TerrainStreamer {
    fn init(base: Base<Node3D>) -> Self { /* the four configs are created with defaults HERE, so
                                             `terrain.world.max_zoom = 17` works on a fresh node
                                             before it enters the tree */ }
    fn ready(&mut self) { self.base_mut().set_process_priority(100); self.rebuild(); }
    fn process(&mut self, _delta: f64) { /* §6.2 */ }
    fn exit_tree(&mut self) { self.teardown(); }
}

#[godot_api]
impl TerrainStreamer {
    #[func] fn initial_position(&self, altitude: f32) -> Vector3;
    #[func] fn ground_height(&self, position: Vector3) -> Variant;   // Variant::nil() when unknown
    #[func] fn get_resident_tiles(&self) -> Array<Dictionary>;
    #[func] fn rebuild(&mut self);
    #[func] fn set_fog_color(&mut self, color: Color);  /* … the other convenience setters … */
    #[signal] fn initial_load_complete();
    #[signal] fn tile_promoted(zoom: i32, x: i32, z: i32);
    #[signal] fn tile_evicted(zoom: i32, x: i32, z: i32);
    #[signal] fn tile_failed(zoom: i32, x: i32, z: i32, reason: GString);
}
```

Behavior details:

- `world`, `streaming`, `network` use custom setters (`#[var(set = set_world)]` etc.): store the
  new resource and, if the node is inside the tree and already initialized, call `rebuild()`.
  `rendering` needs no setter — it is compared field-wise every frame.
- `rebuild()`: teardown (free instances, drop store → joins workers) → read the four configs
  (instantiate defaults for any `None`) → `validate` (on error: `godot_error!`, `enabled = false`,
  return) → `TileStore::new` → build zoom meshes → `last_rendering = rendering.to_core()`.
- Camera resolution each frame: `camera_path` if set and resolves to a `Camera3D`, else
  `get_viewport().get_camera_3d()`; none → skip the frame (idle).
- `ground_height(pos)` → `store.ground_height(pos − world_offset)` → `Variant` (`f32` or nil).
- Emit `tile_failed` for `store.take_failures()` (and `godot_warn!` once per failure);
  `tile_promoted` / `tile_evicted` from the frame loop; `initial_load_complete` on the first
  frame `status.loading` is false.
- Never panic across the FFI boundary: config errors are logged; worker failures are drops.

### 7.12 `lib.rs`

```rust
mod core; mod godot;
struct GodotilesExtension;
#[gdextension]
unsafe impl ExtensionLibrary for GodotilesExtension {}
```

---

## 8. Addon packaging

`addons/godotiles/godotiles.gdextension`:

```ini
[configuration]
entry_symbol = "gdext_rust_init"
compatibility_minimum = 4.4
reloadable = false          ; v1: worker threads make editor hot-reload risky; revisit

[libraries]
macos.debug = "res://addons/godotiles/bin/macos/libgodotiles.dylib"
macos.release = "res://addons/godotiles/bin/macos/libgodotiles.dylib"
linux.debug.x86_64 = "res://addons/godotiles/bin/linux-x86_64/libgodotiles.so"
linux.release.x86_64 = "res://addons/godotiles/bin/linux-x86_64/libgodotiles.so"
windows.debug.x86_64 = "res://addons/godotiles/bin/windows-x86_64/godotiles.dll"
windows.release.x86_64 = "res://addons/godotiles/bin/windows-x86_64/godotiles.dll"
```

`scripts/build.sh [debug|release]` (macOS/Linux; document the two-line PowerShell equivalent):
`cargo build [--release] --manifest-path rust/Cargo.toml`, then copy the produced library into
`addons/godotiles/bin/<platform>/` (macOS in CI: build `aarch64-apple-darwin` and
`x86_64-apple-darwin`, `lipo -create` into one universal dylib; locally a single-arch copy is
fine). Release profile is what ships; debug builds of the extension are for Rust debugging
only.

The addon zip (`godotiles-<version>.zip`) contains `addons/godotiles/**` with all three
platform binaries. Users unzip into their project root; the `.gdextension` is discovered
automatically. macOS note for users: Gatekeeper may quarantine the unsigned dylib
(`xattr -dr com.apple.quarantine addons/godotiles/bin/macos`); document it, plan code-signing later.

---

## 9. Demo project

`project.godot`: name "godotiles demo", main scene `res://demo/main.tscn`, renderer
Forward+ (Mobile also works), window 1280×800, `rendering/textures/default_filters/anisotropic_filtering_level = 4`
(the shader hint uses the project level). Physics/other settings default.

### 9.1 `demo/main.tscn`

```
Main (Node3D) — script demo/demo.gd
├── TerrainStreamer          world = from_lat_lon(35.97391, −113.76892)  (Grand Canyon; the raytiles demo also
│                            ships the Negev 30.82692/34.91386, the Dolomites 46.206889/9.497194, London 51.5074/0.1278)
│                            world.max_zoom = 17 ; world.skirt_overlap = [1.01 ×14]
│                            rendering.fog_color = SKY ; rendering.ambient_light = Color8(200,200,200)
│                            network.threads = 8 ; camera_path = ../Camera3D
├── Camera3D                 script demo/fly_camera.gd ; near 1.0 ; far 400000.0 ; fov 60
├── WorldEnvironment         Environment: background = Sky, Sky.sky_material = ProceduralSkyMaterial
│                            (sky_horizon_color = SKY, ground_horizon_color = SKY), ambient = disabled,
│                            fog disabled (the shader does fog), tonemap = linear
└── UI (CanvasLayer)
    ├── Loading (Label)      centered, 42 px, "Loading... 0%"
    ├── Hud (Label)          top-left, 16 px
    ├── Crash (Label)        centered, 36 px, hidden, "You crashed! Press R to reset."
    └── Help (Label)         bottom-left, 13 px: "A/D roll · Q/E yaw · W/S pitch · +/- throttle · R reset · K labels · L bounds"
```

`SKY = Color8(102, 191, 255)` (raylib's SKYBLUE, parity with the other demos). Because the shader
uses `source_color`, pass the sRGB color as-is; Godot linearizes it.

Since the streamer is configured from code (`from_lat_lon`), `demo.gd` assigns `terrain.world`
in `Main._ready()`. In Godot a parent's `_ready` runs **after** its children's, so by then the
streamer has already initialized with the inspector defaults — that is fine: assigning `world`,
`streaming` or `network` after `_ready` triggers `rebuild()` (§7.11), which is cheap while nothing
is resident yet. (The quick start instead configures the node before `add_child`, avoiding the
rebuild.) Either way the first desired set is requested on the next frame.

### 9.2 `demo/fly_camera.gd` (port of bevytiles' `fly`)

```gdscript
extends Camera3D
## Airplane-style fly camera, always moving forward; rotations around LOCAL axes so yawing while
## banked turns like an aircraft. A/D roll, Q/E yaw, W/S pitch, +/- throttle. Keys set a TARGET
## angular velocity; the actual one eases toward it (frame-rate independent).

const CONTROL_RESPONSE := 5.0
var speed := 120.0                 # m/s
var ang_vel := Vector3.ZERO        # x pitch, y yaw, z roll (rad/s)
var crashed := false
var home: Transform3D

func _ready() -> void:
    home = transform

func _process(dt: float) -> void:
    if crashed:
        return
    var target := Vector3.ZERO
    if Input.is_key_pressed(KEY_A): target.z += 1.2
    if Input.is_key_pressed(KEY_D): target.z -= 1.2
    if Input.is_key_pressed(KEY_Q): target.y += 0.8
    if Input.is_key_pressed(KEY_E): target.y -= 0.8
    if Input.is_key_pressed(KEY_W): target.x -= 0.6
    if Input.is_key_pressed(KEY_S): target.x += 0.6
    if Input.is_key_pressed(KEY_EQUAL) or Input.is_key_pressed(KEY_KP_ADD):
        speed = minf(speed * (1.0 + dt), 3000.0)
    if Input.is_key_pressed(KEY_MINUS) or Input.is_key_pressed(KEY_KP_SUBTRACT):
        speed = maxf(speed * (1.0 - dt), 20.0)
    var blend := 1.0 - exp(-CONTROL_RESPONSE * dt)
    ang_vel = ang_vel.lerp(target, blend)
    rotate_object_local(Vector3(0, 0, 1), ang_vel.z * dt)
    rotate_object_local(Vector3(0, 1, 0), ang_vel.y * dt)
    rotate_object_local(Vector3(1, 0, 0), ang_vel.x * dt)
    position += -global_transform.basis.z * speed * dt     # Godot forward is −Z

func reset(offset: Vector3) -> void:
    transform = home.translated(offset)
    ang_vel = Vector3.ZERO
    crashed = false
```

Initial transform: `initial_position(5000)` looking at `+ Vector3(-1000, -300, -1000)`.

### 9.3 `demo/demo.gd`

- **Rebase** (`_process`, before the streamer runs — priority 0 vs 100): if `|camera.position.x|
  > 4096` shift `camera.position.x` and `terrain.world_offset.x` by `∓4096` (same for z). Nothing
  else lives in user space in the demo; a real game shifts every node.
- **Crash**: `var h = terrain.ground_height(camera.global_position)`; if `h != null and h >
  camera.position.y` → `camera.crashed = true`, show `Crash`. `R` → `camera.reset(terrain.world_offset)`,
  hide `Crash`.
- **Loading**: while `terrain.is_loading` show `Loading` with `loading_progress`; hide afterwards
  (or connect `initial_load_complete`).
- **HUD**: speed, user position, offset, `resident_count`/`desired_count`/`loading_count`, FPS.
- **Debug toggles**: `K` labels, `L` bounds → `debug_overlay.gd` reads
  `terrain.get_resident_tiles()` each frame: bounds as an `ImmediateMesh` of line boxes (`center`,
  `size`, height 1000) in a `MeshInstance3D` with an unshaded green `StandardMaterial3D`; labels as
  pooled `Label`s positioned with `camera.unproject_position(center)` when
  `camera.is_position_behind(center)` is false, green when `desired`, red otherwise.

### 9.4 `demo/quick_start.tscn`

Exactly §5.1: a root `Node3D` with the quick-start script and one `Camera3D`. Serves as the
README example and as the headless smoke test's scene.

---

## 10. Testing

| Layer | Tool | What |
|---|---|---|
| `core/lod`, `synth`, `height`, `config`, `frustum`, `store` | `cargo test` (unit) | pure math snapshots/invariants (§7.2–7.4), store policy with a scripted fake source (§7.6), geo anchors (§7.1) |
| `core/source` | `cargo test` (integration, offline) | the seven scenarios in §7.5 |
| Godot shell | `godot --headless --path . -s tests/godot/smoke.gd` | a `SceneTree` script: seeds a temp cache dir (one z9 texture + Terrarium + normals for the anchor tile, written with `Image.save_png`), configures a `TerrainStreamer` with dead-host URLs and `radius = 2`, `max_zoom = 9`, adds a `Camera3D` at `initial_position(5000)`, pumps `process_frame` up to 600 times, asserts `resident_count >= 1` and `ground_height(initial_position(5000)) != null`, then `quit(0)`; any failure `quit(1)`. Runs on the Linux CI with the dummy renderer (RS instances work headless). |
| Visual checkpoints | `godot --path . -s tests/godot/screenshot.gd -- out.png [wait_s] [agl_m] [pitch_deg] [overlay]` | loads the demo, waits for the initial load, optionally repositions the camera at an AGL altitude (finer tiles + synthesis) and enables the debug overlay, prints the frustum self-check and the visible-tile count, saves the frame — for a human (or an LLM) to inspect against the §12 acceptance lines |

`cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` gate CI like bevytiles.

---

## 11. CI/CD

- `linux.yml` (ubuntu-latest): rustfmt check, clippy, `cargo build --release`, `cargo test`,
  download Godot 4.7.x headless (`godotengine/godot` release asset `Godot_v4.7.x-stable_linux.x86_64.zip`),
  `scripts/build.sh release`, run the smoke test. Upload `addons/godotiles/bin/linux-x86_64` artifact.
- `macos.yml` (macos-latest): add both Apple targets, build both, `lipo`, `cargo test`; artifact.
- `windows.yml` (windows-latest, MSVC): build + test; artifact.
- `release.yml`: on `v*` tags, download the three artifacts, assemble `addons/godotiles/` with
  `bin/`, zip, attach to the GitHub release.
- `release-please.yml`: `release-type: rust`, `bump-minor-pre-major: true`, package path
  `rust/`, extra-files none (the `.gdextension` carries no version). Conventional commits.
- Paths-ignore `README.md` and `docs/` like the sibling repos. Cache with `Swatinem/rust-cache`.

---

## 12. Build order (each step compiles, runs, and is tested before the next)

1. **Skeleton** — crate, `.gdextension`, `TerrainStreamer` with the four config resources,
   `scripts/build.sh`, demo project loads the extension; the node builds the per-zoom meshes and
   draws a hard-coded 3×3 grid of flat tiles with a solid-color material via RS instances.
   *Accept:* Godot prints no GDExtension errors; nine tiles visible from above; `cull_back` winding
   verified; `get_resident_tiles()` returns nine entries.
2. **`core/config` + `core/lod`** with all tests (snapshots pinned to 252/252/117). `update_desired`
   wired; log the desired count as the camera moves. *Accept:* `cargo test` green; count changes
   with altitude as the snapshot table predicts.
3. **`core/source` minimal** — fetch/cache/decode the three assets, payload channel; `promote`
   creates textures + materials; tiles show imagery, **undisplaced**. *Accept:* first real imagery
   on screen; `.cache/` fills with the shared layout; a second run is a cache hit (no network).
4. **Displacement shader** — Terrarium decode, `height_scale`, custom AABB, skirts. *Accept:*
   mountains; `height_scale = 2.0` doubles the relief; tiles do not pop at screen edges; with
   `skirt_drop = 1000` no cracks between zoom levels.
5. **Lifecycle** — reconcile with coverage rules, cancellation, drop handling, status/loading
   contract, `world_offset` rebake, frustum visibility, signals; demo loading screen + rebase
   pattern + HUD. *Accept:* fly for minutes: resident count stays bounded (< ~600 with defaults),
   no holes in the ground, no jitter after rebases, `initial_load_complete` fires once.
6. **`core/height`** — grids from workers, `ground_height`, crash check in the demo. *Accept:*
   flying into a mountain triggers the crash screen; `R` resets.
7. **Synthesis** — `synth.rs`, source integration, lineage backfill, `native_terrain_zoom`,
   ceiling 22; demo `max_zoom = 17`. *Accept:* descending low: sharper imagery, geometry follows
   (smooth magnification of z15), no seams between derived siblings, flat lighting above z15 (by
   design). Cache growth bounded to lineage siblings.
8. **Polish** — rendering setters + live config sync, debug overlay, headless smoke test, CI on
   three platforms, release packaging, README (usage, providers, attribution, memory notes,
   cache pre-warm script), addon README + licenses.

---

## 13. Traps, ranked by blood spilled in the earlier ports (plus the Godot-specific ones)

1. Terrarium is decoded in **three** places (shader, height grid, synthesis) — they must agree.
   One constant set; one test pinning each.
2. **`source_color` on the height/normal samplers** (Godot's equivalent of Bevy's sRGB
   `Image` default) → gamma-warped decode → insane terrain. Albedo only.
3. The initial-loading flip evaluated before the first desired-set build — keep the
   `!desired.is_empty()` guard *and* compute status after `update_desired`.
4. Cancellation keyed inconsistently (raytiles once cancelled with relative coords against a map
   keyed by absolute paths — a silent no-op for months). Key everything by `TileKey`.
5. Eviction without the covered-by check ⇒ holes; with it but without `coverage_dirty` ⇒ wasted
   frame time.
6. f32 for `abs + world_offset` ⇒ vertex jitter far from the anchor. f64 until the final cast.
7. Full-subtree background synthesis ⇒ gigabytes of PNGs. Lineage siblings only.
8. Shared `.tmp` cache filename ⇒ corrupted PNGs when two writers race. Unique suffix.
9. Godot calls from worker threads (Image decode, RenderingServer, scene tree) ⇒ crashes or
   silent corruption. Workers use only `std`, `ureq`, `image`; the main thread does all Godot work.
10. Godot's auto AABB is the flat mesh ⇒ visible mountains culled. `instance_set_custom_aabb` with
    the full column, on every instance, before the first frame.
11. `Camera3D.far` defaults to 4000 m ⇒ the world ends at 4 km. The demo sets 400 000 (code, not
    inspector). Reverse-Z (4.3+) keeps precision fine at that range.
12. Instance RIDs are not ref-counted ⇒ leaked instances survive scene changes and keep drawing.
    `free_rid` on evict, `exit_tree`, and `rebuild`.
13. `process_priority`: if the streamer runs before the app's rebase in the same frame, tiles lag
    one frame behind the camera every rebase (visible pop). Streamer at 100, app at 0.
14. Esri returns JPEG — `with_guessed_format()`, never "decode as PNG".
15. `skirt_overlap` slot 0 ⇒ degenerate mesh. Validate; extend the array when raising `max_zoom`.
16. Relative `cache_dir` resolved against the CWD ⇒ editor and exported build use different
    caches. Resolve against `res://`.
17. `PackedFloat32Array` exports shorter than 14 slots ⇒ index panic across FFI. Pad with defaults.
18. Textures created per tile without dropping the previous record on the (defensive) "already
    resident" path ⇒ slow leak. Replace the whole `GpuTile`.

---

## 14. Future work (out of scope for v1, designed for)

- **Shared core extraction.** Everything under `rust/src/core/` becomes crate `tilecore` (name
  TBD) with features `default = ["native-source"]` so the pure subset (lod, synth, height, config,
  frustum, store policy) compiles for wasm. Faces: Rust API (bevytiles, godotiles), a `cbindgen`
  C ABI over opaque handles + raw RGBA8 buffers (raytiles — note it makes a raylib library depend
  on cargo or on prebuilt static libs), `wasm-bindgen` exports of the pure subset (the three.js
  port keeps `fetch`/Cache-API in JS). The `Source` trait behind `TileStore` (§7.6) is the seam
  for the browser backend (bevytiles' `web.rs`: fetch futures, concurrency cap = `threads`,
  in-memory parent cache, no disk).
- **Web export** of godotiles: nightly Rust + `-Zbuild-std`, emscripten 3.1.74, gdext
  `experimental-wasm`, threads and nothreads dual builds, cross-origin-isolated hosting; source
  backend swap as above.
- **Mobile** (iOS/Android) binaries; touch controls in the demo.
- **Editor tool mode** (`#[class(tool)]`): stream inside the editor viewport for level design.
- **Sky module** as a Godot `sky` shader (rayskies' gradient + FBM clouds).
- **Texture memory**: RGB8/compressed height textures, R16 height textures (skipped in raytiles),
  cache eviction policy for `.cache/`.
- **Normals above the native zoom** derived from the parent (parked in raytiles' greater-zoom plan).
- Code-signing/notarization of the macOS dylib.

---

## Appendix A — defaults (copy verbatim)

| | |
|---|---|
| thresholds z9→z22 (m) | 100000, 80000, 40000, 20000, 10000, 5000, 2500, 1250, 625, 312, 156, 78, 39, 20 |
| tile size @ z9 | 66 400 m (or `40 075 016.686 · cos(lat) / 512` from a geo anchor) |
| radius / update_distance | 6 tiles / 500 m |
| upload budget / cap | 2 ms / 8 tiles per frame |
| mesh resolution | 4 quads/side @ base zoom, doubling per zoom, capped 256 |
| skirt_overlap | 1.0 ×14 (demo: 1.01) ; skirt_drop 0 (demo: try 1000) |
| world height column | −450 … 8848 m (+300 margin) |
| horizon | `d² = 3570² · max(cam.y, 1)` |
| anchor default | tile (306, 207) @ z9 |
| texture URL (Esri) | `https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/:zoom:/:y:/:x:` |
| heightmap URL | `https://s3.amazonaws.com/elevation-tiles-prod/terrarium/:zoom:/:x:/:y:.png` |
| normals URL | `https://s3.amazonaws.com/elevation-tiles-prod/normal/:zoom:/:x:/:y:.png` |
| native terrain zoom | 15 ; supported zooms 9–22 ; `max_zoom` default 15 |
| threads / timeouts | 4 / connect 5 s, read 3 s |
| fog | start 100 km, end 150 km, color blue (demo: sky blue) |
| lod snapshots | (0,500,0) → 252 keys, 64 @z15 · (33200,5000,33200) → 252, 36 @z9 · (0,60000,0) → 117, 48 @z11 |

## Appendix B — Terrarium

`h = r·256 + g + b/256 − 32768` meters. Encode: `fixed = clamp(round((h + 32768)·256), 0, 0xFFFFFF)`,
`r = fixed >> 16`, `g = (fixed >> 8) & 255`, `b = fixed & 255`. Query grid: `u16 = round(h) + 32768`.
Flat normal map: `(128, 128, 255)`.

## Appendix C — cache pre-warming

`scripts/tiles-cache.mjs <anchor_x> <anchor_z>` (copied from raytiles, unchanged): downloads
texture/heightmap/normals for the whole z9…z15 subtree of one anchor tile into `.cache/` with the
exact layout the engine reads (`.cache/{texture,heightmap,normals}/z/x/y.png`). Set
`MAPBOX_TOKEN` for Mapbox imagery. Existing files are skipped (resumable).

## Appendix D — glossary

*absolute space* — fixed-origin tile frame (anchor tile corner = origin). *user space* — the
app's/Godot's frame; `absolute = user − world_offset`. *desired set* — what the LOD policy wants
resident for the current camera. *resident* — uploaded and drawable. *loading* — requested, not
answered. *promotion* — CPU payload → GPU resources. *reconcile* — eviction pass. *lineage* — the
ancestry chain from a derived tile up to its native-zoom ancestor. *native zoom* — the deepest
zoom the terrain provider serves (15).
