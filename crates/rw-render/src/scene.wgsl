// One shader for both primitives. Lines project straight through; points are
// spread into a screen-space quad so a distant cloud stays visible instead of
// thinning to single pixels.

struct Uniforms {
    view_projection: mat4x4<f32>,
    viewport: vec2<f32>,
    point_size: f32,
    _padding: f32,
    eye: vec4<f32>,
};

@group(0) @binding(0) var<uniform> uniforms: Uniforms;

struct VertexIn {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) corner: vec2<f32>,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_line(in: VertexIn) -> VertexOut {
    var out: VertexOut;
    out.clip = uniforms.view_projection * vec4<f32>(in.position, 1.0);
    out.color = in.color;
    return out;
}

@vertex
fn vs_point(in: VertexIn) -> VertexOut {
    var out: VertexOut;
    let clip = uniforms.view_projection * vec4<f32>(in.position, 1.0);
    // Offset after projection and scaled by w, so a point keeps the same size
    // on screen however far away it is.
    let offset = in.corner * uniforms.point_size / uniforms.viewport * clip.w;
    out.clip = vec4<f32>(clip.xy + offset, clip.z, clip.w);
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    return in.color;
}

// ── lit surfaces ───────────────────────────────────────────────────────────────
//
// A key light from over the viewer's shoulder, a cooler fill from the opposite
// side, and a little sky-versus-ground ambient. Not physically based; chosen so
// a grey robot on a dark background still reads as a shape.

struct SolidIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec4<f32>,
    // The model matrix, one column per attribute: a mat4 cannot be a single
    // vertex attribute.
    @location(3) model_0: vec4<f32>,
    @location(4) model_1: vec4<f32>,
    @location(5) model_2: vec4<f32>,
    @location(6) model_3: vec4<f32>,
};

struct SolidOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) world: vec3<f32>,
};

@vertex
fn vs_solid(in: SolidIn) -> SolidOut {
    let model = mat4x4<f32>(in.model_0, in.model_1, in.model_2, in.model_3);
    let world = model * vec4<f32>(in.position, 1.0);
    var out: SolidOut;
    out.clip = uniforms.view_projection * world;
    out.color = in.color;
    // No non-uniform scale in a robot description, so the model matrix rotates
    // normals correctly without an inverse transpose.
    out.normal = normalize((model * vec4<f32>(in.normal, 0.0)).xyz);
    out.world = world.xyz;
    return out;
}

@fragment
fn fs_solid(in: SolidOut) -> @location(0) vec4<f32> {
    let normal = normalize(in.normal);
    let view = normalize(uniforms.eye.xyz - in.world);
    // Two-sided: a mesh with inconsistent winding should not have black holes.
    let facing = select(-normal, normal, dot(normal, view) > 0.0);

    let key_dir = normalize(vec3<f32>(0.4, -0.7, 0.8));
    let fill_dir = normalize(vec3<f32>(-0.6, 0.5, 0.2));

    let key = max(dot(facing, key_dir), 0.0);
    let fill = max(dot(facing, fill_dir), 0.0) * 0.35;
    // Sky above, ground below, so an upward face is never as dark as a downward
    // one even where no light reaches it.
    let ambient = mix(0.10, 0.26, facing.z * 0.5 + 0.5);

    let half_vector = normalize(key_dir + view);
    let specular = pow(max(dot(facing, half_vector), 0.0), 48.0) * 0.25;

    let lit = in.color.rgb * (ambient + key + fill) + vec3<f32>(specular);
    return vec4<f32>(lit, in.color.a);
}
