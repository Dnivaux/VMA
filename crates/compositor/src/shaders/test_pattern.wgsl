// Test pattern for the Program output. Values are Rec.709 gamma-encoded R'G'B', full range.
//
// Layout (top to bottom):
//   [0, 2/3)    8 vertical colour bars at 75 % amplitude (100/0/75/0):
//               white, yellow, cyan, green, magenta, red, blue, black
//   [2/3, 5/6)  horizontal grey ramp, 0 -> 1
//   [5/6, 1]    black band with a white marker (1/16 of the width) moving left to right

struct Uniforms {
    phase: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

// Full-screen triangle, uv (0,0) at the top-left.
@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    let xy = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: VsOut;
    out.pos = vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2<f32>(xy.x, 1.0 - xy.y);
    return out;
}

const BAR_LEVEL: f32 = 0.75;

fn bar_color(index: u32) -> vec3<f32> {
    // Bit pattern per bar (R, G, B): W Y C G M R B K
    switch index {
        case 0u: { return vec3<f32>(1.0, 1.0, 1.0); }
        case 1u: { return vec3<f32>(1.0, 1.0, 0.0); }
        case 2u: { return vec3<f32>(0.0, 1.0, 1.0); }
        case 3u: { return vec3<f32>(0.0, 1.0, 0.0); }
        case 4u: { return vec3<f32>(1.0, 0.0, 1.0); }
        case 5u: { return vec3<f32>(1.0, 0.0, 0.0); }
        case 6u: { return vec3<f32>(0.0, 0.0, 1.0); }
        default: { return vec3<f32>(0.0, 0.0, 0.0); }
    }
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = in.uv;
    var rgb: vec3<f32>;
    if uv.y < 2.0 / 3.0 {
        let index = min(u32(uv.x * 8.0), 7u);
        rgb = bar_color(index) * BAR_LEVEL;
    } else if uv.y < 5.0 / 6.0 {
        rgb = vec3<f32>(uv.x);
    } else {
        let width = 1.0 / 16.0;
        let start = u.phase * (1.0 - width);
        let inside = uv.x >= start && uv.x < start + width;
        rgb = select(vec3<f32>(0.0), vec3<f32>(1.0), inside);
    }
    return vec4<f32>(rgb, 1.0);
}
