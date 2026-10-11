// 3D viewer: lambert + fill + rim light, normals-as-colors, unlit lines,
// gradient background. All colours are linear; `params.y` = 1 when the
// target is not sRGB (encode in the shader).

struct Uniforms {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    eye: vec4<f32>,
    line_color: vec4<f32>,
    bg_top: vec4<f32>,
    bg_bottom: vec4<f32>,
    // x: mode (0 shaded, 1 normals, 3 unlit line colour), y: encode sRGB
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

struct VIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
};

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
};

fn encode(c: vec3<f32>) -> vec3<f32> {
    if u.params.y > 0.5 {
        return pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.2));
    }
    return c;
}

@vertex
fn vs_main(v: VIn) -> VOut {
    let w = u.model * vec4<f32>(v.pos, 1.0);
    var o: VOut;
    o.clip = u.view_proj * w;
    o.world = w.xyz;
    o.normal = (u.model * vec4<f32>(v.normal, 0.0)).xyz;
    o.color = v.color;
    return o;
}

@fragment
fn fs_main(i: VOut) -> @location(0) vec4<f32> {
    let mode = u32(u.params.x + 0.5);
    if mode == 3u {
        return vec4<f32>(encode(u.line_color.rgb), u.line_color.a);
    }
    let v = normalize(u.eye.xyz - i.world);
    var n = normalize(i.normal);
    if mode == 1u {
        // object-space-ish normals as display colours (no lighting)
        let c = n * 0.5 + vec3<f32>(0.5);
        if u.params.y > 0.5 {
            return vec4<f32>(c, 1.0);
        }
        return vec4<f32>(pow(c, vec3<f32>(2.2)), 1.0);
    }
    // two-sided: face the viewer (models often have mixed winding)
    if dot(n, v) < 0.0 {
        n = -n;
    }
    let key = normalize(v + vec3<f32>(0.35, 0.6, 0.25));
    let fill_dir = normalize(vec3<f32>(-0.6, 0.25, -0.5));
    let lambert = max(dot(n, key), 0.0);
    let fill = max(dot(n, fill_dir), 0.0) * 0.25;
    let rim = pow(1.0 - max(dot(n, v), 0.0), 3.0) * 0.3;
    let c = i.color * (0.16 + 0.78 * lambert + fill) + vec3<f32>(rim);
    return vec4<f32>(encode(c), 1.0);
}

struct BgOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) t: f32,
};

@vertex
fn vs_bg(@builtin(vertex_index) idx: u32) -> BgOut {
    let uv = vec2<f32>(f32((idx << 1u) & 2u), f32(idx & 2u));
    let p = uv * 2.0 - vec2<f32>(1.0);
    var o: BgOut;
    o.clip = vec4<f32>(p, 1.0, 1.0);
    o.t = p.y * 0.5 + 0.5;
    return o;
}

@fragment
fn fs_bg(i: BgOut) -> @location(0) vec4<f32> {
    let c = mix(u.bg_bottom.rgb, u.bg_top.rgb, clamp(i.t, 0.0, 1.0));
    return vec4<f32>(encode(c), 1.0);
}
