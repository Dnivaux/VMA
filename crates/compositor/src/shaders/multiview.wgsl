// MultiView (ADR 0014): one quad per instance, read from a uniform array. A quad is a picture
// (texture), a solid rectangle, a title (texture alpha × colour), a tally border (the edges
// of the rectangle only) or a stereo meter (two bars).

struct Quad {
    // Destination rectangle in pixels: x0, y0, x1, y1.
    dst: vec4<f32>,
    // Source rectangle in texture coordinates: u0, v0, u1, v1.
    src: vec4<f32>,
    color: vec4<f32>,
    // x: mode (0 picture, 1 solid, 2 title, 3 border, 4 meter); y: border thickness in
    // pixels, or the left level; z: the right level.
    params: vec4<f32>,
};

struct Scene {
    // x, y: size of the target in pixels.
    size: vec4<f32>,
    quads: array<Quad, 192>,
};

@group(0) @binding(0) var<uniform> scene: Scene;
@group(1) @binding(0) var picture: texture_2d<f32>;
@group(1) @binding(1) var linear: sampler;

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    // Position inside the quad, 0–1.
    @location(1) local: vec2<f32>,
    @location(2) @interpolate(flat) index: u32,
};

@vertex
fn vs_main(@builtin(vertex_index) v: u32, @builtin(instance_index) i: u32) -> VsOut {
    let q = scene.quads[i];
    // Two triangles: corners 0 1 2, 2 1 3 of the unit square.
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    let c = corners[v];
    let px = mix(q.dst.xy, q.dst.zw, c);
    let ndc = vec2<f32>(px.x / scene.size.x * 2.0 - 1.0, 1.0 - px.y / scene.size.y * 2.0);
    var out: VsOut;
    out.position = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = mix(q.src.xy, q.src.zw, c);
    out.local = c;
    out.index = i;
    return out;
}

// Meter scale: -60 dBFS at the bottom, 0 dBFS at the top.
fn meter_height(level: f32) -> f32 {
    if (level <= 0.001) {
        return 0.0;
    }
    let db = 20.0 * log2(level) / log2(10.0);
    return clamp((db + 60.0) / 60.0, 0.0, 1.0);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let q = scene.quads[in.index];
    let mode = u32(q.params.x);
    let texel = textureSampleLevel(picture, linear, in.uv, 0.0);
    if (mode == 0u) {
        return vec4<f32>(texel.rgb, 1.0);
    }
    if (mode == 2u) {
        // White text on transparent: its alpha over what is under it.
        return vec4<f32>(q.color.rgb * texel.a, texel.a);
    }
    if (mode == 3u) {
        let size = q.dst.zw - q.dst.xy;
        let px = in.local * size;
        let t = q.params.y;
        if (px.x >= t && px.y >= t && px.x <= size.x - t && px.y <= size.y - t) {
            discard;
        }
        return vec4<f32>(q.color.rgb, 1.0);
    }
    if (mode == 4u) {
        // Left bar on the left half, right bar on the right half, with a gap between.
        let left = in.local.x < 0.5;
        let x = select(in.local.x - 0.5, in.local.x, left) * 2.0;
        if (x > 0.8) {
            return vec4<f32>(0.0, 0.0, 0.0, 0.0);
        }
        let level = meter_height(select(q.params.z, q.params.y, left));
        let height = 1.0 - in.local.y;
        if (height > level) {
            return vec4<f32>(0.08, 0.08, 0.08, 0.85);
        }
        // Green to -18, yellow to -6, red above.
        let green = vec3<f32>(0.20, 0.80, 0.30);
        let yellow = vec3<f32>(0.95, 0.80, 0.20);
        let red = vec3<f32>(0.90, 0.20, 0.20);
        let rgb = select(select(red, yellow, height < 0.9), green, height < 0.7);
        return vec4<f32>(rgb, 1.0);
    }
    // Solid.
    return q.color;
}
