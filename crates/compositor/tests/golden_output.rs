//! Golden tests of the output stage (`OutputStage`, ADR 0006 § 1, spec `phase-2.md`): FTB at
//! 0 / 50 / 100 % in both domains with the UI's Program view untouched (B11, compositor side),
//! the sync flash, and the conversion to NV12 Y'CbCr BT.709 limited range (B10, compositor
//! side): 75 % bars, every grey code, chroma siting and odd sizes against a CPU reference, and
//! the round trip through the input decoder.
//!
//! Expected values are analytic (computed on the CPU from the same definitions), plus the
//! published 75 % bar values. Needs a GPU adapter (software is enough); fails without one.

// Test code: panicking on failure is the intended behaviour.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stdout
)]

use vma_compositor::{
    FramePipelines, InputFrame, Mix, MixSpace, Mixer, OutputLook, OutputStage, Plane, RenderTarget,
    Source, headless, readback,
};
use vma_core::{ColorInfo, PixelFormat, Resolution, Rgb8};

/// Picture of one tick: the UI's Program view and what the outputs receive.
struct Tick {
    /// The UI's Program view (`Rgba8Unorm`), from the mixer.
    ui: Vec<u8>,
    /// `out16` converted to `Rgba8Unorm` (full-screen output).
    out: Vec<u8>,
    /// NV12 luma, one byte per pixel.
    y: Vec<u8>,
    /// NV12 chroma, Cb Cr per sample, half size rounded up.
    uv: Vec<u8>,
}

struct Rig {
    gpu: headless::HeadlessGpu,
    pipelines: FramePipelines,
    mixer: Mixer,
    stage: OutputStage,
    ui: RenderTarget,
    out: RenderTarget,
    resolution: Resolution,
}

impl Rig {
    fn new(w: u32, h: u32) -> Self {
        let gpu = pollster::block_on(headless::request_gpu()).expect("GPU adapter required");
        eprintln!(
            "adapter: {} ({:?}, {:?})",
            gpu.adapter_info.name, gpu.adapter_info.backend, gpu.adapter_info.device_type
        );
        let resolution = Resolution::new(w, h).unwrap();
        let mixer = Mixer::new(&gpu.device, resolution);
        let stage = OutputStage::new(&gpu.device, mixer.program());
        Self {
            pipelines: FramePipelines::new(&gpu.device),
            ui: RenderTarget::new(&gpu.device, resolution, "ui"),
            out: RenderTarget::new(&gpu.device, resolution, "out"),
            gpu,
            mixer,
            stage,
            resolution,
        }
    }

    fn tick(&self, mix: Mix, look: OutputLook) -> Tick {
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        self.mixer
            .encode(&self.gpu.queue, &mut encoder, &mix, self.ui.view());
        self.stage.encode(&self.gpu.queue, &mut encoder, look);
        self.stage.encode_rgba8(&mut encoder, self.out.view());
        self.gpu.queue.submit([encoder.finish()]);
        let read = |t| readback::read_bytes(&self.gpu.device, &self.gpu.queue, t).unwrap();
        Tick {
            ui: read(&self.ui),
            out: read(&self.out),
            y: read(self.stage.luma()),
            uv: read(self.stage.chroma()),
        }
    }

    fn color(&self, c: Rgb8, look: OutputLook) -> Tick {
        self.tick(Mix::single(Source::Color(c)), look)
    }

    /// An RGBA picture of the Program size, uploaded by the real path.
    fn rgba_frame(&self, data: &[u8]) -> InputFrame {
        let frame = InputFrame::new(
            &self.gpu.device,
            &self.gpu.queue,
            &self.pipelines,
            PixelFormat::Rgba,
            self.resolution,
            ColorInfo::RGB_FULL,
        );
        frame.stage_packed(data).unwrap();
        self.copy(&frame);
        frame
    }

    /// An NV12 BT.709 limited-range picture of the Program size.
    fn nv12_frame(&self, y: &[u8], uv: &[u8]) -> InputFrame {
        let (w, _) = self.size();
        let frame = InputFrame::new(
            &self.gpu.device,
            &self.gpu.queue,
            &self.pipelines,
            PixelFormat::Nv12,
            self.resolution,
            ColorInfo::BT709_LIMITED,
        );
        frame
            .stage(&[
                Plane { data: y, stride: w },
                Plane {
                    data: uv,
                    stride: w.div_ceil(2) * 2,
                },
            ])
            .unwrap();
        self.copy(&frame);
        frame
    }

    fn copy(&self, frame: &InputFrame) {
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        frame.encode_upload(&mut encoder);
        self.gpu.queue.submit([encoder.finish()]);
        self.gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
    }

    fn size(&self) -> (u32, u32) {
        (self.resolution.width(), self.resolution.height())
    }
}

fn look(fade_to_black: f32, space: MixSpace) -> OutputLook {
    OutputLook {
        fade_to_black,
        space,
        flash: false,
    }
}

/// Every pixel of an RGBA8 picture has this RGB, opaque.
fn assert_uniform(img: &[u8], want: [u8; 3], what: &str) {
    for (i, p) in img.as_chunks::<4>().0.iter().enumerate() {
        assert_eq!(*p, [want[0], want[1], want[2], 255], "{what}, pixel {i}");
    }
}

/// BT.709 limited-range code values of an R'G'B' colour (0–1), unrounded.
fn ycbcr([r, g, b]: [f64; 3]) -> [f64; 3] {
    let (kr, kb) = (0.2126, 0.0722);
    let y = kr * r + (1.0 - kr - kb) * g + kb * b;
    [
        16.0 + 219.0 * y,
        128.0 + 224.0 * (b - y) / (2.0 * (1.0 - kb)),
        128.0 + 224.0 * (r - y) / (2.0 * (1.0 - kr)),
    ]
}

fn unit(c: [u8; 3]) -> [f64; 3] {
    c.map(|v| f64::from(v) / 255.0)
}

/// Tolerance on a code value computed by the GPU, against the exact value: rounding (0.5) plus
/// the `Rgba16Float` storage of the Program and `out16`. Vulkan lets a GPU store half floats
/// rounded toward zero (NVIDIA does: 46 / 255 gives Y' 55, not 56 for 55.506): below 1, one
/// f16 step is at most 2^-11, i.e. 0.11 code after the ×219 / ×224 of the matrix.
const TOL: f64 = 0.5 + 0.125;

fn assert_codes(got: [u8; 3], want: [f64; 3], tol: f64, what: &str) {
    for (g, w) in got.iter().zip(want) {
        assert!(
            (f64::from(*g) - w).abs() <= tol,
            "{what}: got {got:?}, want {want:?} ±{tol}"
        );
    }
}

const COLORS: [Rgb8; 6] = [
    Rgb8::WHITE,
    Rgb8::new(255, 0, 0),
    Rgb8::new(0, 255, 0),
    Rgb8::new(0, 0, 255),
    Rgb8::new(16, 128, 235),
    Rgb8::new(191, 191, 0),
];

#[test]
fn ftb_at_0_percent_is_the_program() {
    // B11, 0 %: the outputs receive the Program unchanged, test pattern included.
    let rig = Rig::new(320, 180);
    for space in [MixSpace::Gamma, MixSpace::Linear] {
        let tick = rig.tick(
            Mix::single(Source::TestPattern { frame: 17 }),
            look(0.0, space),
        );
        assert_eq!(tick.out, tick.ui, "{space:?}");
    }
}

#[test]
fn ftb_at_100_percent_is_black_and_leaves_the_ui_alone() {
    // B11, 100 %: black outputs; the UI's Program view still shows the Program.
    let rig = Rig::new(320, 180);
    for space in [MixSpace::Gamma, MixSpace::Linear] {
        for c in COLORS {
            let tick = rig.color(c, look(1.0, space));
            assert_uniform(&tick.out, [0, 0, 0], "outputs");
            assert_uniform(&tick.ui, [c.r, c.g, c.b], "UI view");
            assert!(tick.y.iter().all(|&v| v == 16), "luma black");
            assert!(tick.uv.iter().all(|&v| v == 128), "chroma neutral");
        }
    }
}

#[test]
fn ftb_at_50_percent() {
    // B11, 50 %: gamma = half the code values; linear = half the display light.
    let rig = Rig::new(64, 36);
    for c in COLORS {
        let codes = [c.r, c.g, c.b].map(f64::from);
        let gamma = rig.color(c, look(0.5, MixSpace::Gamma));
        let linear = rig.color(c, look(0.5, MixSpace::Linear));
        let first = |img: &[u8]| [img[0], img[1], img[2]];
        assert_codes(first(&gamma.out), codes.map(|v| v / 2.0), 0.5, "gamma");
        assert_codes(
            first(&linear.out),
            codes.map(|v| v * 0.5f64.powf(1.0 / 2.4)),
            0.5,
            "linear",
        );
        let [r, g, b] = first(&gamma.out);
        assert_uniform(&gamma.out, [r, g, b], "gamma uniform");
        assert_uniform(&gamma.ui, [c.r, c.g, c.b], "UI view");
    }
}

#[test]
fn flash_is_white_and_wins_over_the_fade() {
    let rig = Rig::new(64, 36);
    let tick = rig.tick(
        Mix::single(Source::TestPattern { frame: 0 }),
        OutputLook {
            fade_to_black: 1.0,
            space: MixSpace::Gamma,
            flash: true,
        },
    );
    assert_uniform(&tick.out, [255, 255, 255], "outputs");
    assert!(tick.y.iter().all(|&v| v == 235), "luma white");
    assert!(tick.uv.iter().all(|&v| v == 128), "chroma neutral");
    assert_ne!(tick.ui, tick.out, "the UI view is not flashed");
}

#[test]
fn every_grey_code_maps_to_limited_range() {
    // B10: R' = G' = B' = k → Y' = round(16 + 219 k / 255) (see `TOL`), Cb = Cr = 128 exactly.
    let rig = Rig::new(16, 8);
    for k in 0..=255u8 {
        let tick = rig.color(Rgb8::new(k, k, k), OutputLook::CLEAN);
        let want = ycbcr(unit([k; 3]))[0];
        let first = tick.y[0];
        assert!(
            (f64::from(first) - want).abs() <= TOL,
            "code {k}: luma {first} vs {want}"
        );
        assert!(tick.y.iter().all(|&v| v == first), "code {k}: luma uniform");
        assert!(tick.uv.iter().all(|&v| v == 128), "code {k}: chroma");
    }
}

#[test]
fn bars_are_bt709_limited_range() {
    // B10: the 75 % bars of the test pattern (code 191) through the whole chain, against the
    // analytic values and the published BT.709 75 % bar values, ±1.
    const BARS: [([u8; 3], [f64; 3]); 8] = [
        ([191, 191, 191], [180.0, 128.0, 128.0]),
        ([191, 191, 0], [168.0, 44.0, 136.0]),
        ([0, 191, 191], [145.0, 147.0, 44.0]),
        ([0, 191, 0], [133.0, 63.0, 52.0]),
        ([191, 0, 191], [63.0, 193.0, 204.0]),
        ([191, 0, 0], [51.0, 109.0, 212.0]),
        ([0, 0, 191], [28.0, 212.0, 120.0]),
        ([0, 0, 0], [16.0, 128.0, 128.0]),
    ];
    let rig = Rig::new(320, 180);
    let tick = rig.tick(
        Mix::single(Source::TestPattern { frame: 0 }),
        OutputLook::CLEAN,
    );
    for (i, (rgb, published)) in BARS.iter().enumerate() {
        // Centre of bar i (40 px wide), in the top two thirds.
        let (x, y) = (40 * i + 20, 60);
        let (cx, cy) = (x / 2, y / 2);
        let got = [
            tick.y[y * 320 + x],
            tick.uv[(cy * 160 + cx) * 2],
            tick.uv[(cy * 160 + cx) * 2 + 1],
        ];
        assert_codes(got, ycbcr(unit(*rgb)), TOL, &format!("bar {i}, analytic"));
        assert_codes(got, *published, 1.0, &format!("bar {i}, published"));
    }
}

/// Deterministic pseudo-random RGBA picture (opaque).
fn noise(w: u32, h: u32, seed: u32) -> Vec<u8> {
    let mut s = seed;
    (0..w * h)
        .flat_map(|_| {
            let mut next = || {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s >> 24) as u8
            };
            [next(), next(), next(), 255]
        })
        .collect()
}

#[test]
fn nv12_matches_the_cpu_reference_with_left_siting_and_odd_sizes() {
    // Chroma sample (i, j) = [1 2 1] / 4 around luma column 2i, averaged over rows 2j and
    // 2j + 1, edges clamped (MPEG-2 / H.264 "left" siting); odd sizes keep a last sample.
    for (w, h) in [(8, 4), (7, 5), (13, 3)] {
        let rig = Rig::new(w, h);
        let rgba = noise(w, h, w * 31 + h);
        let frame = rig.rgba_frame(&rgba);
        let tick = rig.tick(Mix::single(Source::Frame(&frame)), OutputLook::CLEAN);
        assert_eq!(tick.ui, rgba, "{w}×{h}: RGBA reaches the Program unchanged");

        let at = |x: i64, y: i64| {
            let x = x.clamp(0, i64::from(w) - 1) as usize;
            let y = y.clamp(0, i64::from(h) - 1) as usize;
            let i = (y * w as usize + x) * 4;
            unit([rgba[i], rgba[i + 1], rgba[i + 2]])
        };
        for y in 0..h {
            for x in 0..w {
                let got = tick.y[(y * w + x) as usize];
                let want = ycbcr(at(x.into(), y.into()))[0];
                assert!(
                    (f64::from(got) - want).abs() <= TOL,
                    "{w}×{h} luma ({x}, {y}): {got} vs {want}"
                );
            }
        }
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        assert_eq!(tick.uv.len(), (cw * ch * 2) as usize, "{w}×{h} chroma size");
        for j in 0..ch {
            for i in 0..cw {
                let (x, y) = (i64::from(2 * i), i64::from(2 * j));
                let mut sum = [0.0; 3];
                for (dx, weight) in [(-1, 1.0), (0, 2.0), (1, 1.0)] {
                    for dy in 0..2 {
                        let c = at(x + dx, y + dy);
                        for k in 0..3 {
                            sum[k] += c[k] * weight / 8.0;
                        }
                    }
                }
                let want = ycbcr(sum);
                let k = ((j * cw + i) * 2) as usize;
                let got = [tick.uv[k], tick.uv[k + 1]];
                for (g, w) in got.iter().zip(&want[1..]) {
                    assert!(
                        (f64::from(*g) - w).abs() <= TOL,
                        "chroma ({i}, {j}): {got:?} vs {want:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_vertical_edge_on_an_even_column_takes_a_quarter_of_its_left_neighbour() {
    // Readable case of the siting: red up to column 3, blue from column 4. Chroma sample 2
    // (luma column 4) sees ¼ red + ¾ blue; sample 1 (column 2) sees red only.
    let (w, h) = (8, 2);
    let rig = Rig::new(w, h);
    let rgba: Vec<u8> = (0..w * h)
        .flat_map(|i| {
            if i % w < 4 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            }
        })
        .collect();
    let frame = rig.rgba_frame(&rgba);
    let tick = rig.tick(Mix::single(Source::Frame(&frame)), OutputLook::CLEAN);
    let chroma = |i: usize| [tick.uv[2 * i], tick.uv[2 * i + 1]];
    let want = |c: [f64; 3]| {
        let [_, cb, cr] = ycbcr(c);
        [cb.round() as u8, cr.round() as u8]
    };
    assert_eq!(chroma(1), want([1.0, 0.0, 0.0]));
    assert_eq!(chroma(2), want([0.25, 0.0, 0.75]));
    assert_eq!(chroma(3), want([0.0, 0.0, 1.0]));
}

#[test]
fn nv12_round_trips_through_the_input_decoder() {
    // What we send decodes back (A7 decoder, BT.709 limited) to the Program, ±2 codes:
    // 8-bit limited range cannot hold every R'G'B' triplet.
    let rig = Rig::new(64, 36);
    let (w, h) = rig.size();
    for c in COLORS.iter().chain(&[Rgb8::BLACK, Rgb8::new(1, 2, 3)]) {
        let sent = rig.color(*c, OutputLook::CLEAN);
        let frame = rig.nv12_frame(&sent.y, &sent.uv);
        let back = rig.tick(Mix::single(Source::Frame(&frame)), OutputLook::CLEAN);
        assert_eq!(back.ui.len(), (w * h * 4) as usize);
        for p in back.ui.as_chunks::<4>().0 {
            assert_codes(
                [p[0], p[1], p[2]],
                [c.r, c.g, c.b].map(f64::from),
                2.0,
                &format!("{c:?}"),
            );
        }
    }
}
