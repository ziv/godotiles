//! GPU-side resources: per-zoom grid meshes, per-tile textures + material + `RenderingServer`
//! instance, transform baking and the custom culling AABB. Main thread only.

use super::shader::{create_shader, UniformNames};
use crate::core::config::{
    RenderingConfig, WorldConfig, MAX_RESOLUTION, MAX_WORLD_HEIGHT, MIN_RESOLUTION,
    MIN_WORLD_HEIGHT, WORLD_HEIGHT_MARGIN,
};
use crate::core::lod::TileKey;
use crate::core::source::TilePayload;
use crate::core::store::ResidentInfo;
use godot::classes::image::Format;
use godot::classes::mesh::{ArrayType, PrimitiveType};
use godot::classes::rendering_server::ShadowCastingSetting;
use godot::classes::{ArrayMesh, Image, ImageTexture, RenderingServer, Shader, ShaderMaterial};
use godot::prelude::*;
use std::collections::HashMap;

/// Everything the GPU holds for one resident tile. The `Gd` handles are ref-counted (dropping
/// the record frees them); the instance RID is not and must be freed explicitly.
pub struct GpuTile {
    pub instance: Rid,
    pub material: Gd<ShaderMaterial>,
    pub albedo: Gd<ImageTexture>,
    pub height: Gd<ImageTexture>,
    pub normals: Gd<ImageTexture>,
    pub abs_x: f64,
    pub abs_z: f64,
    pub half: f32,
}

pub struct Gpu {
    shader: Gd<Shader>,
    names: UniformNames,
    meshes: Vec<Gd<ArrayMesh>>,
    tiles: HashMap<TileKey, GpuTile>,
    scenario: Rid,
    baked_offset: Vector3,
    visible: bool,
}

impl Gpu {
    pub fn new() -> Self {
        Self {
            shader: create_shader(),
            names: UniformNames::new(),
            meshes: Vec::new(),
            tiles: HashMap::new(),
            scenario: Rid::Invalid,
            baked_offset: Vector3::ZERO,
            visible: true,
        }
    }

    /// (Re)build the per-zoom meshes for a world config. Frees nothing else.
    pub fn build_meshes(&mut self, world: &WorldConfig) {
        self.meshes.clear();
        let mut res = MIN_RESOLUTION;
        for zoom in world.base_zoom..=world.max_zoom {
            let idx = world.zoom_index(zoom);
            let extent = (world.zoom_size(zoom) as f32) * world.skirt_overlap[idx];
            self.meshes.push(build_grid_mesh(extent, res));
            res = (res * 2).min(MAX_RESOLUTION);
        }
    }

    pub fn set_scenario(&mut self, scenario: Rid) {
        self.scenario = scenario;
    }

    pub fn baked_offset(&self) -> Vector3 {
        self.baked_offset
    }

    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    pub fn get(&self, key: &TileKey) -> Option<&GpuTile> {
        self.tiles.get(key)
    }

    /// Upload one payload: three textures, a material, an instance with its culling column.
    /// Returns false (and creates nothing) if any image failed to build.
    pub fn upload(
        &mut self,
        world: &WorldConfig,
        rendering: &RenderingConfig,
        payload: &TilePayload,
        info: &ResidentInfo,
        world_offset: Vector3,
    ) -> bool {
        let (Some(albedo), Some(height), Some(normals)) = (
            make_texture(
                payload.albedo.as_raw(),
                payload.albedo.width(),
                payload.albedo.height(),
                world.mipmaps,
            ),
            make_texture(
                &rgb_to_rgba(payload.height.as_raw()),
                payload.height.width(),
                payload.height.height(),
                false,
            ),
            make_texture(
                &rgb_to_rgba(payload.normals.as_raw()),
                payload.normals.width(),
                payload.normals.height(),
                false,
            ),
        ) else {
            return false;
        };

        let mut material = ShaderMaterial::new_gd();
        material.set_shader(&self.shader);
        material.set_shader_parameter(&self.names.albedo_tex, &albedo.to_variant());
        material.set_shader_parameter(&self.names.height_tex, &height.to_variant());
        material.set_shader_parameter(&self.names.normal_tex, &normals.to_variant());
        self.names.apply(&mut material, rendering);

        let key = payload.key;
        let idx = world.zoom_index(key.zoom);
        let half = info.size * 0.5 * world.skirt_overlap[idx];

        let mut rs = RenderingServer::singleton();
        let instance = rs.instance_create();
        rs.instance_set_base(instance, self.meshes[idx].get_rid());
        rs.instance_set_scenario(instance, self.scenario);
        rs.instance_geometry_set_material_override(instance, material.get_rid());
        rs.instance_geometry_set_cast_shadows_setting(instance, ShadowCastingSetting::OFF);
        // displacement happens on the GPU: the mesh AABB is a flat plane and would cull visible
        // mountains. Cover the whole height column (local space).
        rs.instance_set_custom_aabb(
            instance,
            Aabb::new(
                Vector3::new(-half, MIN_WORLD_HEIGHT, -half),
                Vector3::new(
                    2.0 * half,
                    MAX_WORLD_HEIGHT - MIN_WORLD_HEIGHT + WORLD_HEIGHT_MARGIN,
                    2.0 * half,
                ),
            ),
        );
        rs.instance_set_transform(
            instance,
            user_transform(info.abs_x, info.abs_z, world_offset),
        );
        rs.instance_set_visible(instance, self.visible);

        if let Some(old) = self.tiles.insert(
            key,
            GpuTile {
                instance,
                material,
                albedo,
                height,
                normals,
                abs_x: info.abs_x,
                abs_z: info.abs_z,
                half,
            },
        ) {
            // defensive: replacing an existing record must not leak its instance
            rs.free_rid(old.instance);
        }
        true
    }

    pub fn free(&mut self, key: &TileKey) {
        if let Some(tile) = self.tiles.remove(key) {
            RenderingServer::singleton().free_rid(tile.instance);
        }
    }

    pub fn free_all(&mut self) {
        let mut rs = RenderingServer::singleton();
        for (_, tile) in self.tiles.drain() {
            rs.free_rid(tile.instance);
        }
    }

    /// Rebake every instance transform after a large-world rebase (f64 add, single f32 cast).
    pub fn rebake(&mut self, world_offset: Vector3) {
        self.baked_offset = world_offset;
        let mut rs = RenderingServer::singleton();
        for tile in self.tiles.values() {
            rs.instance_set_transform(
                tile.instance,
                user_transform(tile.abs_x, tile.abs_z, world_offset),
            );
        }
    }

    /// Push rendering parameters to every live material (rare event).
    pub fn push_params(&mut self, rendering: &RenderingConfig) {
        for tile in self.tiles.values_mut() {
            self.names.apply(&mut tile.material, rendering);
        }
    }

    pub fn set_visible(&mut self, visible: bool) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        let mut rs = RenderingServer::singleton();
        for tile in self.tiles.values() {
            rs.instance_set_visible(tile.instance, visible);
        }
    }
}

impl Default for Gpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        self.free_all();
    }
}

/// User-space translation of a tile center: `abs + offset` added in f64, cast once.
pub fn user_transform(abs_x: f64, abs_z: f64, world_offset: Vector3) -> Transform3D {
    let ux = (abs_x + f64::from(world_offset.x)) as f32;
    let uz = (abs_z + f64::from(world_offset.z)) as f32;
    Transform3D::new(Basis::IDENTITY, Vector3::new(ux, 0.0, uz))
}

/// Godot's `Image` has no color-space flag: sRGB vs linear is decided by the sampler hint in
/// the shader (`source_color` on the albedo only).
fn make_texture(rgba: &[u8], w: u32, h: u32, mipmaps: bool) -> Option<Gd<ImageTexture>> {
    let data = PackedByteArray::from(rgba);
    let mut img = Image::create_from_data(w as i32, h as i32, false, Format::RGBA8, &data)?;
    if mipmaps {
        img.generate_mipmaps();
    }
    ImageTexture::create_from_image(&img)
}

fn rgb_to_rgba(rgb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgb.len() / 3 * 4);
    for p in rgb.chunks_exact(3) {
        out.extend_from_slice(&[p[0], p[1], p[2], 255]);
    }
    out
}

/// A grid on the XZ plane centered at the origin, `res` quads per side, UVs spanning exactly
/// [0, 1] (the vertex shader samples the heightmap by UV). Front faces are clockwise (Godot).
fn build_grid_mesh(extent: f32, res: u32) -> Gd<ArrayMesh> {
    let n = res + 1;
    let mut verts = Vec::with_capacity((n * n) as usize);
    let mut normals = Vec::with_capacity((n * n) as usize);
    let mut uvs = Vec::with_capacity((n * n) as usize);
    for iz in 0..n {
        for ix in 0..n {
            let u = ix as f32 / res as f32;
            let v = iz as f32 / res as f32;
            verts.push(Vector3::new(
                -extent / 2.0 + u * extent,
                0.0,
                -extent / 2.0 + v * extent,
            ));
            normals.push(Vector3::UP);
            uvs.push(Vector2::new(u, v));
        }
    }
    let mut indices: Vec<i32> = Vec::with_capacity((res * res * 6) as usize);
    for iz in 0..res {
        for ix in 0..res {
            let a = (iz * n + ix) as i32;
            let b = a + 1;
            let c = a + n as i32;
            let d = c + 1;
            indices.extend_from_slice(&[a, b, d, a, d, c]);
        }
    }

    let mut arrays = VarArray::new();
    arrays.resize(ArrayType::MAX.ord() as usize, &Variant::nil());
    arrays.set(
        ArrayType::VERTEX.ord() as usize,
        &PackedVector3Array::from(verts.as_slice()).to_variant(),
    );
    arrays.set(
        ArrayType::NORMAL.ord() as usize,
        &PackedVector3Array::from(normals.as_slice()).to_variant(),
    );
    arrays.set(
        ArrayType::TEX_UV.ord() as usize,
        &PackedVector2Array::from(uvs.as_slice()).to_variant(),
    );
    arrays.set(
        ArrayType::INDEX.ord() as usize,
        &PackedInt32Array::from(indices.as_slice()).to_variant(),
    );

    let mut mesh = ArrayMesh::new_gd();
    mesh.add_surface_from_arrays(PrimitiveType::TRIANGLES, &arrays);
    mesh
}
