// Output stage, pass 2 (ADR 0006 § 1, ADR 0005): R'G'B' out16 -> Y'CbCr BT.709 limited range,
// 4:2:0, as NV12 planes: Y' in an R8Unorm target of the Program size, CbCr in an Rg8Unorm
// target of half the size (rounded up).
//
// Chroma is sited like MPEG-2 / H.264 by default ("left"): horizontally on the even luma
// column, vertically between the two rows. Each chroma sample is the [1 2 1] / 4 horizontal
// filter around column 2i, averaged over rows 2j and 2j + 1, edges clamped. The matrix rows
// are in code values (`[r, g, b, offset]`); results are rounded explicitly (see output.wgsl).

struct Matrix {
    y: vec4<f32>,
    cb: vec4<f32>,
    cr: vec4<f32>,
}

@group(0) @binding(0) var<uniform> m: Matrix;
@group(0) @binding(1) var picture: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let xy = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0);
}

fn rgb(p: vec2<i32>) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(picture));
    let q = clamp(p, vec2<i32>(0), size - 1);
    return vec4<f32>(clamp(textureLoad(picture, q, 0).rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}

fn code(row: vec4<f32>, c: vec4<f32>) -> f32 {
    return round(clamp(dot(row, c), 0.0, 255.0)) / 255.0;
}

@fragment
fn fs_luma(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    return vec4<f32>(code(m.y, rgb(vec2<i32>(pos.xy))), 0.0, 0.0, 1.0);
}

@fragment
fn fs_chroma(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let x = i32(pos.x) * 2;
    let y = i32(pos.y) * 2;
    var sum = vec4<f32>(0.0);
    for (var dy = 0; dy < 2; dy++) {
        sum += rgb(vec2<i32>(x - 1, y + dy)) + 2.0 * rgb(vec2<i32>(x, y + dy))
            + rgb(vec2<i32>(x + 1, y + dy));
    }
    let c = sum / 8.0;
    return vec4<f32>(code(m.cb, c), code(m.cr, c), 0.0, 1.0);
}
