// Mix pass: two Rgba16Float layers of the Program size -> the Rgba16Float Program target.
// Values are Rec.709 gamma-encoded R'G'B' (ADR 0005). In the linear domain the mix is done on
// display light, with the BT.1886 simplified EOTF (L = V^2.4) and its exact inverse.

struct Uniforms {
    // 0 = layer a alone, 1 = layer b alone.
    mix: f32,
    // 0 = gamma (mix the encoded values), 1 = linear light.
    linear: u32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var layer_a: texture_2d<f32>;
@group(0) @binding(2) var layer_b: texture_2d<f32>;

// Full-screen triangle.
@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let xy = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0);
}

const GAMMA: f32 = 2.4;

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(pos.xy);
    let a = max(textureLoad(layer_a, p, 0).rgb, vec3<f32>(0.0));
    let b = max(textureLoad(layer_b, p, 0).rgb, vec3<f32>(0.0));
    var rgb: vec3<f32>;
    if u.linear == 1u {
        let l = mix(pow(a, vec3<f32>(GAMMA)), pow(b, vec3<f32>(GAMMA)), u.mix);
        rgb = pow(l, vec3<f32>(1.0 / GAMMA));
    } else {
        rgb = mix(a, b, u.mix);
    }
    return vec4<f32>(rgb, 1.0);
}
