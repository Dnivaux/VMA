// Solid fill over the current viewport. The colour of cell `c` is `colors[c]`; the draw's
// instance index carries the cell (`draw(0..3, c..c + 1)`).

struct Colors {
    colors: array<vec4<f32>, 64>,
};

@group(0) @binding(0) var<uniform> u: Colors;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) cell: u32,
};

@vertex
fn vs_main(@builtin(vertex_index) i: u32, @builtin(instance_index) cell: u32) -> VertexOut {
    let xy = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return VertexOut(vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0), cell);
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    return u.colors[in.cell];
}
