//! The terrain shader source (embedded at compile time) and cached uniform names.

use crate::core::config::RenderingConfig;
use godot::classes::{Shader, ShaderMaterial};
use godot::prelude::*;

/// The Godot shading-language source of the terrain shader.
pub const TERRAIN_SHADER: &str = include_str!("../../shaders/terrain.gdshader");

/// Cached `StringName`s of every uniform (built once; `StringName` construction is not free).
pub struct UniformNames {
    pub albedo_tex: StringName,
    pub height_tex: StringName,
    pub normal_tex: StringName,
    pub fog_color: StringName,
    pub ambient_light: StringName,
    pub sun_direction: StringName,
    pub sun_scale: StringName,
    pub fog_start: StringName,
    pub fog_end: StringName,
    pub height_scale: StringName,
    pub normals_scale: StringName,
    pub skirt_drop: StringName,
}

impl UniformNames {
    pub fn new() -> Self {
        Self {
            albedo_tex: StringName::from("albedo_tex"),
            height_tex: StringName::from("height_tex"),
            normal_tex: StringName::from("normal_tex"),
            fog_color: StringName::from("fog_color"),
            ambient_light: StringName::from("ambient_light"),
            sun_direction: StringName::from("sun_direction"),
            sun_scale: StringName::from("sun_scale"),
            fog_start: StringName::from("fog_start"),
            fog_end: StringName::from("fog_end"),
            height_scale: StringName::from("height_scale"),
            normals_scale: StringName::from("normals_scale"),
            skirt_drop: StringName::from("skirt_drop"),
        }
    }

    /// Push every rendering parameter to one material.
    pub fn apply(&self, material: &mut Gd<ShaderMaterial>, cfg: &RenderingConfig) {
        let color = |c: [f32; 4]| Color::from_rgba(c[0], c[1], c[2], c[3]).to_variant();
        let sun = Vector3::new(
            cfg.sun_direction.x,
            cfg.sun_direction.y,
            cfg.sun_direction.z,
        );
        material.set_shader_parameter(&self.fog_color, &color(cfg.fog_color));
        material.set_shader_parameter(&self.ambient_light, &color(cfg.ambient_light));
        material.set_shader_parameter(&self.sun_direction, &sun.to_variant());
        material.set_shader_parameter(&self.sun_scale, &cfg.sun_scale.to_variant());
        material.set_shader_parameter(&self.fog_start, &cfg.fog_start.to_variant());
        material.set_shader_parameter(&self.fog_end, &cfg.fog_end.to_variant());
        material.set_shader_parameter(&self.height_scale, &cfg.height_scale.to_variant());
        material.set_shader_parameter(&self.normals_scale, &cfg.normals_scale.to_variant());
        material.set_shader_parameter(&self.skirt_drop, &cfg.skirt_drop.to_variant());
    }
}

impl Default for UniformNames {
    fn default() -> Self {
        Self::new()
    }
}

/// Create the shared terrain `Shader` resource from the embedded source.
pub fn create_shader() -> Gd<Shader> {
    let mut shader = Shader::new_gd();
    shader.set_code(TERRAIN_SHADER);
    shader
}
