// One shader for both primitives. Lines project straight through; points are
// spread into a screen-space quad so a distant cloud stays visible instead of
// thinning to single pixels.

struct Uniforms {
    view_projection: mat4x4<f32>,
    viewport: vec2<f32>,
    point_size: f32,
    _padding: f32,
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
