// Output stage, pass 1 (ADR 0006 § 1): Rgba16Float Program -> Rgba16Float out16, with the
// fade to black (FTB) and the sync flash. Only the outputs see this pass, never the UI's
// Program view.
//
// The fade is a mix towards black: on the encoded values (gamma) the gain is 1 - p; in linear
// light it is (1 - p)^(1/2.4), computed on the CPU so that 0 % and 100 % stay exact.

struct Uniforms {
    gain: f32,
    // 1 = the whole picture is replaced by white (sync test, ADR 0006 § 5).
    flash: u32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var program: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let xy = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    if u.flash == 1u {
        return vec4<f32>(1.0);
    }
    let c = max(textureLoad(program, vec2<i32>(pos.xy), 0).rgb, vec3<f32>(0.0));
    return vec4<f32>(c * u.gain, 1.0);
}
