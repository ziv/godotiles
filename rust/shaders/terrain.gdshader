// godotiles terrain shader: Terrarium displacement + normal-map sun lighting + distance fog.
// Port of the raytiles GLSL / bevytiles WGSL pair. Embedded into the extension binary.
shader_type spatial;
render_mode unshaded, cull_back, depth_draw_opaque, fog_disabled;

// albedo: color data → source_color (sRGB→linear on sample), mipmapped + anisotropic
uniform sampler2D albedo_tex : source_color, filter_linear_mipmap_anisotropic, repeat_disable;
// heightmap + normals: RAW bytes. NO source_color, NO hint_normal — or the Terrarium decode is
// gamma-warped garbage
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
// Must stay in lockstep with the CPU decoders (synth.rs, height.rs).
float terrarium_height(vec2 uv) {
    vec3 c = textureLod(height_tex, uv, 0.0).rgb * 255.0;
    return c.r * 256.0 + c.g + c.b / 256.0 - 32768.0;
}

void vertex() {
    VERTEX.y += terrarium_height(UV) * height_scale;

    // edge vertices drop by the skirt amount to hide LOD cracks
    float e = 0.000001;
    float edge = clamp(step(UV.x, e) + step(1.0 - e, UV.x) + step(UV.y, e) + step(1.0 - e, UV.y), 0.0, 1.0);
    VERTEX.y -= skirt_drop * height_scale * edge;

    vec3 world_pos = (MODEL_MATRIX * vec4(VERTEX, 1.0)).xyz;
    cam_dist = distance(world_pos, CAMERA_POSITION_WORLD);
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
