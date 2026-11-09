// Key pass (ADR 0010): a premultiplied Rgba16Float layer (stinger or overlay channel) drawn
// over the Program target by the blend unit (`src + (1 - src.a) * dst`), on the encoded
// values. `opacity` is the channel's transition: it scales the premultiplied value.

struct Uniforms {
    opacity: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var layer: texture_2d<f32>;

// Full-screen triangle.
@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let xy = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let s = clamp(textureLoad(layer, vec2<i32>(pos.xy), 0), vec4<f32>(0.0), vec4<f32>(1.0));
    return s * u.opacity;
}
