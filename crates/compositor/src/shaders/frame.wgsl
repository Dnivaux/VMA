// Input pass (spec compositor.md, Phase 1, pass 1): the planes of a CPU frame -> R'G'B'
// (ADR 0005), at the frame's native size. Y'CbCr goes through the frame's affine matrix
// (601 / 709, limited / full range); RGB is copied (a Bgra8Unorm plane is already swizzled
// by the sampler). Chroma is taken from the co-sited sample (nearest, no interpolation).
//
// LINEARIZE = false: R'G'B' for a layer of the same size (no round trip through linear).
// LINEARIZE = true: display light (BT.1886 simplified, L = V^2.4) for the scale pass.

struct Frame {
    // Rows of `[r, g, b] = M · [y, cb, cr, 1]`.
    m0: vec4<f32>,
    m1: vec4<f32>,
    m2: vec4<f32>,
    // 0 NV12, 1 I420, 2 YUY2, 3 UYVY, 4 RGBA / BGRA.
    packing: u32,
}

override LINEARIZE: bool = false;

@group(0) @binding(0) var<uniform> f: Frame;
@group(0) @binding(1) var plane0: texture_2d<f32>;
@group(0) @binding(2) var plane1: texture_2d<f32>;
@group(0) @binding(3) var plane2: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let xy = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0);
}

const GAMMA: f32 = 2.4;

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(pos.xy);
    let half = vec2<i32>(p.x / 2, p.y / 2);
    let odd = (p.x & 1) == 1;
    var ycc = vec3<f32>(0.0);
    var rgb: vec3<f32>;
    switch f.packing {
        case 0u {
            ycc = vec3<f32>(textureLoad(plane0, p, 0).r, textureLoad(plane1, half, 0).rg);
        }
        case 1u {
            ycc = vec3<f32>(
                textureLoad(plane0, p, 0).r,
                textureLoad(plane1, half, 0).r,
                textureLoad(plane2, half, 0).r,
            );
        }
        case 2u {
            // Y0 Cb Y1 Cr
            let t = textureLoad(plane0, vec2<i32>(half.x, p.y), 0);
            ycc = vec3<f32>(select(t.r, t.b, odd), t.g, t.a);
        }
        case 3u {
            // Cb Y0 Cr Y1
            let t = textureLoad(plane0, vec2<i32>(half.x, p.y), 0);
            ycc = vec3<f32>(select(t.g, t.a, odd), t.r, t.b);
        }
        default {}
    }
    if f.packing >= 4u {
        rgb = textureLoad(plane0, p, 0).rgb;
    } else {
        let v = vec4<f32>(ycc, 1.0);
        rgb = vec3<f32>(dot(f.m0, v), dot(f.m1, v), dot(f.m2, v));
    }
    rgb = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    if LINEARIZE {
        rgb = pow(rgb, vec3<f32>(GAMMA));
    }
    return vec4<f32>(rgb, 1.0);
}
