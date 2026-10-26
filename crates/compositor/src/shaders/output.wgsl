// Output pass: Rgba16Float Program -> Rgba8Unorm, rounded to the nearest code value
// (ADR 0005, no dithering in Phase 1). The rounding is explicit so the result does not depend
// on how a GPU converts floats when storing to a unorm target.

@group(0) @binding(0) var source: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let xy = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let c = clamp(textureLoad(source, vec2<i32>(pos.xy), 0).rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    return vec4<f32>(round(c * 255.0) / 255.0, 1.0);
}
