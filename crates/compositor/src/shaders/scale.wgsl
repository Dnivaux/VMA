// Scale pass (spec compositor.md, Phase 1, pass 2): bilinear sampling of a frame in display
// light (= filtering in linear light), re-encoded to R'G'B' (ADR 0005). The viewport is the
// fitted rectangle, computed on the CPU; the letterbox around it is left to the caller.
//
// The source is premultiplied display light (ADR 0010): it is unpremultiplied to re-encode,
// then premultiplied again in R'G'B'. OVER_BLACK (thumbnail atlas) writes it over black,
// opaque.

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// Full-screen triangle over the viewport; uv (0, 0) is the top-left corner of the source.
@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VertexOut {
    let xy = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return VertexOut(vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0), vec2<f32>(xy.x, 1.0 - xy.y));
}

const GAMMA: f32 = 2.4;

override OVER_BLACK: bool = false;

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let s = textureSampleLevel(source, smp, in.uv, 0.0);
    let a = clamp(s.a, 0.0, 1.0);
    if a <= 0.0 {
        return vec4<f32>(0.0, 0.0, 0.0, select(0.0, 1.0, OVER_BLACK));
    }
    let l = max(s.rgb / a, vec3<f32>(0.0));
    let v = pow(l, vec3<f32>(1.0 / GAMMA)) * a;
    return vec4<f32>(v, select(a, 1.0, OVER_BLACK));
}
