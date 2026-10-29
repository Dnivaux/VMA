//! Golden tests of CPU frames (`InputFrame`, spec `compositor.md`, Phase 1, passes 1 and 2):
//! Y'CbCr → R'G'B' for every format, matrix and range (A7), lossless RGB at the Program size,
//! fit with black bars, scaling in linear light, frames in thumbnails, refused uploads, and
//! the transfer buffer of the upload path (ADR 0004, amendment 1).
//!
//! Expected values are analytic (computed on the CPU from the same definitions). Needs a GPU
//! adapter (software is enough); fails without one.

// Test code: panicking on failure is the intended behaviour.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stdout
)]

use vma_compositor::thumbnails::{self, ThumbnailPainter};
use vma_compositor::{
    FrameError, FramePipelines, InputFrame, Mix, Mixer, Plane, RenderTarget, Source, headless,
    readback,
};
use vma_core::{ColorInfo, Matrix, PixelFormat, Range, Resolution, Rgb8};

const W: u32 = 320;
const H: u32 = 180;

struct Rig {
    gpu: headless::HeadlessGpu,
    pipelines: FramePipelines,
    mixer: Mixer,
    out: RenderTarget,
}

impl Rig {
    fn new() -> Self {
        let gpu = pollster::block_on(headless::request_gpu()).expect("GPU adapter required");
        eprintln!(
            "adapter: {} ({:?}, {:?})",
            gpu.adapter_info.name, gpu.adapter_info.backend, gpu.adapter_info.device_type
        );
        let resolution = res(W, H);
        let pipelines = FramePipelines::new(&gpu.device);
        let mixer = Mixer::new(&gpu.device, resolution);
        let out = RenderTarget::new(&gpu.device, resolution, "out");
        Self {
            gpu,
            pipelines,
            mixer,
            out,
        }
    }

    fn frame(&self, format: PixelFormat, w: u32, h: u32, color: ColorInfo) -> InputFrame {
        InputFrame::new(
            &self.gpu.device,
            &self.gpu.queue,
            &self.pipelines,
            format,
            res(w, h),
            color,
        )
    }

    /// Uploads a packed picture by the real path: staged, copied by an encoder, then waits
    /// until the transfer buffer is mapped again.
    fn upload(&self, frame: &InputFrame, data: &[u8]) {
        frame.stage_packed(data).unwrap();
        self.copy(frame);
    }

    /// Records and submits the copy of the staged picture, then waits for the GPU.
    fn copy(&self, frame: &InputFrame) {
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        frame.encode_upload(&mut encoder);
        self.gpu.queue.submit([encoder.finish()]);
        self.gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
    }

    /// The Program (`Rgba8Unorm`, `W` × `H`) showing `frame` alone.
    fn render(&self, frame: &InputFrame) -> Vec<u8> {
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        self.mixer.encode(
            &self.gpu.queue,
            &mut encoder,
            &Mix::single(Source::Frame(frame)),
            self.out.view(),
        );
        self.gpu.queue.submit([encoder.finish()]);
        readback::read_rgba8(&self.gpu.device, &self.gpu.queue, &self.out).unwrap()
    }
}

fn res(w: u32, h: u32) -> Resolution {
    Resolution::new(w, h).unwrap()
}

fn px(img: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    img[i..i + 4].try_into().unwrap()
}

/// The 8 bars at 75 % (spec `compositor.md`, Phase 0): white, yellow, cyan, green, magenta,
/// red, blue, black; 40 px wide at 320 px (chroma-aligned).
fn bar(x: u32) -> Rgb8 {
    const BARS: [[u8; 3]; 8] = [
        [191, 191, 191],
        [191, 191, 0],
        [0, 191, 191],
        [0, 191, 0],
        [191, 0, 191],
        [191, 0, 0],
        [0, 0, 191],
        [0, 0, 0],
    ];
    let [r, g, b] = BARS[(x * 8 / W) as usize];
    Rgb8::new(r, g, b)
}

/// R'G'B' → Y'CbCr codes `[y, cb, cr]` (the inverse of `ColorInfo::ycbcr_to_rgb`).
fn encode_ycbcr(c: Rgb8, color: ColorInfo) -> [u8; 3] {
    let (kr, kb) = color.matrix.kr_kb();
    let [r, g, b] = [c.r, c.g, c.b].map(|v| f64::from(v) / 255.0);
    let y = kr * r + (1.0 - kr - kb) * g + kb * b;
    let pb = (b - y) / (2.0 * (1.0 - kb));
    let pr = (r - y) / (2.0 * (1.0 - kr));
    let (ys, yo, cs) = match color.range {
        Range::Limited => (219.0, 16.0, 224.0),
        Range::Full => (255.0, 0.0, 255.0),
    };
    [y * ys + yo, 128.0 + pb * cs, 128.0 + pr * cs].map(|v| v.round().clamp(0.0, 255.0) as u8)
}

/// CPU reference of the shader: the codes through `ycbcr_to_rgb`, clamped and rounded.
fn decode_ycbcr(ycc: [u8; 3], color: ColorInfo) -> [u8; 3] {
    let v = ycc.map(|c| f32::from(c) / 255.0);
    color.ycbcr_to_rgb().map(|row| {
        let x = row[0] * v[0] + row[1] * v[1] + row[2] * v[2] + row[3];
        (x.clamp(0.0, 1.0) * 255.0).round() as u8
    })
}

/// A tightly packed `format` picture of `w` × `h` where pixel (x, y) is `ycc(x, y)`
/// (`[y, cb, cr]`, or `[r, g, b]` for RGB formats). Chroma is taken from the even pixel of
/// each pair (and row pair).
fn pack(format: PixelFormat, w: u32, h: u32, ycc: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
    let (hw, hh) = (w.div_ceil(2), h.div_ceil(2));
    let chroma = |x: u32, y: u32| ycc((x * 2).min(w - 1), (y * 2).min(h - 1));
    let mut out = Vec::new();
    if matches!(format, PixelFormat::Nv12 | PixelFormat::I420) {
        for y in 0..h {
            for x in 0..w {
                out.push(ycc(x, y)[0]);
            }
        }
    }
    match format {
        PixelFormat::Nv12 => {
            for y in 0..hh {
                for x in 0..hw {
                    let [_, cb, cr] = chroma(x, y);
                    out.extend([cb, cr]);
                }
            }
        }
        PixelFormat::I420 => {
            for i in [1, 2] {
                for y in 0..hh {
                    for x in 0..hw {
                        out.push(chroma(x, y)[i]);
                    }
                }
            }
        }
        PixelFormat::Yuy2 | PixelFormat::Uyvy => {
            for y in 0..h {
                for x in 0..hw {
                    let [y0, cb, cr] = ycc(x * 2, y);
                    let y1 = ycc((x * 2 + 1).min(w - 1), y)[0];
                    out.extend(if format == PixelFormat::Yuy2 {
                        [y0, cb, y1, cr]
                    } else {
                        [cb, y0, cr, y1]
                    });
                }
            }
        }
        PixelFormat::Rgba => {
            for y in 0..h {
                for x in 0..w {
                    let [r, g, b] = ycc(x, y);
                    out.extend([r, g, b, 255]);
                }
            }
        }
        PixelFormat::Bgra => {
            for y in 0..h {
                for x in 0..w {
                    let [r, g, b] = ycc(x, y);
                    out.extend([b, g, r, 255]);
                }
            }
        }
    }
    assert_eq!(out.len() as u64, format.frame_bytes(w, h), "{format}");
    out
}

const COLORIMETRIES: [ColorInfo; 4] = [
    ColorInfo {
        matrix: Matrix::Bt601,
        range: Range::Limited,
    },
    ColorInfo {
        matrix: Matrix::Bt601,
        range: Range::Full,
    },
    ColorInfo {
        matrix: Matrix::Bt709,
        range: Range::Limited,
    },
    ColorInfo {
        matrix: Matrix::Bt709,
        range: Range::Full,
    },
];

/// A7: 75 % bars in every format × matrix × range (24 combinations; RGB formats ignore the
/// colorimetry) → the expected R'G'B' ±1, with the CPU reference of the decoded codes.
#[test]
fn ycbcr_bars_decode_for_every_format_matrix_and_range() {
    let rig = Rig::new();
    let mut worst_vs_ideal = 0;
    for format in PixelFormat::ALL {
        for color in COLORIMETRIES {
            let ycc = |x: u32, _y: u32| {
                let c = bar(x);
                if format.is_yuv() {
                    encode_ycbcr(c, color)
                } else {
                    [c.r, c.g, c.b]
                }
            };
            let frame = rig.frame(format, W, H, color);
            rig.upload(&frame, &pack(format, W, H, ycc));
            let img = rig.render(&frame);
            for (x, y) in (0..W).flat_map(|x| [0, H / 2, H - 1].map(|y| (x, y))) {
                let got = px(&img, W, x, y);
                let ideal = bar(x);
                let want = if format.is_yuv() {
                    decode_ycbcr(ycc(x, y), color)
                } else {
                    [ideal.r, ideal.g, ideal.b]
                };
                for c in 0..3 {
                    let d = (i16::from(got[c]) - i16::from(want[c])).abs();
                    assert!(
                        d <= 1,
                        "{format} {color:?} ({x}, {y}): got {got:?}, CPU reference {want:?}"
                    );
                    let ideal = [ideal.r, ideal.g, ideal.b][c];
                    worst_vs_ideal =
                        worst_vs_ideal.max((i16::from(got[c]) - i16::from(ideal)).abs());
                }
                assert_eq!(got[3], 255);
            }
        }
    }
    eprintln!("largest deviation from the ideal 191 / 0 bars: {worst_vs_ideal} code(s)");
    assert!(worst_vs_ideal <= 1, "{worst_vs_ideal}");
}

/// Same size as the Program: RGBA and BGRA go through bit-exact (no linear round trip).
#[test]
fn rgb_at_program_size_is_bit_exact() {
    let rig = Rig::new();
    let code = |x: u32, y: u32| {
        [
            (x % 256) as u8,
            ((x + y * 7) % 256) as u8,
            ((y * 3) % 256) as u8,
        ]
    };
    for format in [PixelFormat::Rgba, PixelFormat::Bgra] {
        let frame = rig.frame(format, W, H, ColorInfo::RGB_FULL);
        rig.upload(&frame, &pack(format, W, H, code));
        let img = rig.render(&frame);
        for y in 0..H {
            for x in 0..W {
                let [r, g, b] = code(x, y);
                assert_eq!(px(&img, W, x, y), [r, g, b, 255], "{format} ({x}, {y})");
            }
        }
    }
}

/// Fit: a 4:3 picture in the 16:9 Program → 240 px wide pillars at 1440 / 1920, i.e. 40 px
/// at 320 (±1 px at the edges).
#[test]
fn four_by_three_is_pillarboxed() {
    let rig = Rig::new();
    let frame = rig.frame(PixelFormat::Rgba, 640, 480, ColorInfo::RGB_FULL);
    rig.upload(
        &frame,
        &pack(PixelFormat::Rgba, 640, 480, |_, _| [255, 255, 255]),
    );
    let img = rig.render(&frame);
    for y in [0, H / 2, H - 1] {
        for x in 0..W {
            let got = px(&img, W, x, y);
            match x {
                0..39 | 282.. => assert_eq!(got, [0, 0, 0, 255], "bar ({x}, {y})"),
                41..279 => assert_eq!(got, [255, 255, 255, 255], "picture ({x}, {y})"),
                _ => {}
            }
        }
    }
}

/// Scaling filters display light: a 1 px black / white checkerboard halved gives
/// `255 × 0.5^(1/2.4)` = 191 (half the luminance), not 128.
#[test]
fn halved_checkerboard_is_191_not_128() {
    let rig = Rig::new();
    let frame = rig.frame(PixelFormat::Rgba, W * 2, H * 2, ColorInfo::RGB_FULL);
    let checker = |x: u32, y: u32| [if (x + y).is_multiple_of(2) { 255 } else { 0 }; 3];
    rig.upload(&frame, &pack(PixelFormat::Rgba, W * 2, H * 2, checker));
    let img = rig.render(&frame);
    let want = (255.0 * 0.5f64.powf(1.0 / 2.4)).round() as i16;
    assert_eq!(want, 191);
    for y in 0..H {
        for x in 0..W {
            let got = px(&img, W, x, y);
            for c in &got[..3] {
                assert!(
                    (i16::from(*c) - want).abs() <= 1,
                    "({x}, {y}): {got:?}, want {want}"
                );
            }
        }
    }
}

/// Odd sizes keep their last column and row (chroma planes rounded up): a 321 × 181 frame in
/// a Program of the same size (direct path), with a distinct last column and last row.
#[test]
fn odd_sized_frames_are_complete() {
    let rig = Rig::new();
    let (w, h) = (W + 1, H + 1);
    let mixer = Mixer::new(&rig.gpu.device, res(w, h));
    let out = RenderTarget::new(&rig.gpu.device, res(w, h), "out.odd");
    let color = ColorInfo::BT709_LIMITED;
    let green = encode_ycbcr(Rgb8::new(0, 191, 0), color);
    let blue = encode_ycbcr(Rgb8::new(0, 0, 191), color);
    let ycc = |x: u32, y: u32| {
        if x == w - 1 || y == h - 1 {
            blue
        } else {
            green
        }
    };
    for format in [
        PixelFormat::Nv12,
        PixelFormat::I420,
        PixelFormat::Yuy2,
        PixelFormat::Uyvy,
    ] {
        let frame = rig.frame(format, w, h, color);
        rig.upload(&frame, &pack(format, w, h, ycc));
        let mut encoder = rig.gpu.device.create_command_encoder(&Default::default());
        mixer.encode(
            &rig.gpu.queue,
            &mut encoder,
            &Mix::single(Source::Frame(&frame)),
            out.view(),
        );
        rig.gpu.queue.submit([encoder.finish()]);
        let img = readback::read_rgba8(&rig.gpu.device, &rig.gpu.queue, &out).unwrap();
        for (x, y) in [
            (0, 0),
            (w - 2, h - 2),
            (w - 1, 0),
            (0, h - 1),
            (w - 1, h - 1),
        ] {
            let got = px(&img, w, x, y);
            let want = decode_ycbcr(ycc(x, y), color);
            for c in 0..3 {
                assert!(
                    (i16::from(got[c]) - i16::from(want[c])).abs() <= 1,
                    "{format} ({x}, {y}): {got:?}, want {want:?}"
                );
            }
        }
    }
}

/// A frame in a thumbnail cell: fitted over black, the rest of the atlas untouched.
#[test]
fn frame_thumbnail_is_fitted_in_its_cell() {
    let rig = Rig::new();
    let painter = ThumbnailPainter::new(&rig.gpu.device);
    let atlas = thumbnails::new_atlas(&rig.gpu.device, "atlas");
    // 4:3 red → 240 × 180 inside the 320 × 180 cell, 40 px black on each side.
    let frame = rig.frame(PixelFormat::Rgba, 640, 480, ColorInfo::RGB_FULL);
    rig.upload(
        &frame,
        &pack(PixelFormat::Rgba, 640, 480, |_, _| [255, 0, 0]),
    );
    let cells = [
        (10, Source::Frame(&frame)),
        (11, Source::Color(Rgb8::WHITE)),
    ];
    let mut encoder = rig.gpu.device.create_command_encoder(&Default::default());
    painter.encode(
        &rig.gpu.queue,
        &mut encoder,
        atlas.view(),
        0,
        cells.into_iter(),
    );
    rig.gpu.queue.submit([encoder.finish()]);
    let img = readback::read_rgba8(&rig.gpu.device, &rig.gpu.queue, &atlas).unwrap();
    let aw = atlas.resolution().width();
    let (x0, y0, cw, ch) = thumbnails::cell_rect(10).unwrap();
    for y in [y0, y0 + ch / 2, y0 + ch - 1] {
        for dx in 0..cw {
            let got = px(&img, aw, x0 + dx, y);
            match dx {
                0..39 | 282.. => assert_eq!(got, [0, 0, 0, 255], "bar ({dx}, {y})"),
                41..279 => assert_eq!(got, [255, 0, 0, 255], "picture ({dx}, {y})"),
                _ => {}
            }
        }
    }
    let (x1, y1, w1, h1) = thumbnails::cell_rect(11).unwrap();
    assert_eq!(px(&img, aw, x1, y1), [255, 255, 255, 255]);
    assert_eq!(px(&img, aw, x1 + w1 - 1, y1 + h1 - 1), [255, 255, 255, 255]);
}

/// Bad buffers are refused (typed error, no GPU validation error, no panic).
#[test]
fn bad_uploads_are_refused() {
    let rig = Rig::new();
    let frame = rig.frame(PixelFormat::Nv12, 4, 2, ColorInfo::BT709_LIMITED);
    let y = [16u8; 8];
    let uv = [128u8; 4];
    assert_eq!(
        frame.stage(&[Plane {
            data: &y,
            stride: 4
        }]),
        Err(FrameError::PlaneCount {
            format: PixelFormat::Nv12,
            expected: 2,
            got: 1
        })
    );
    assert_eq!(
        frame.stage(&[
            Plane {
                data: &y,
                stride: 3
            },
            Plane {
                data: &uv,
                stride: 4
            }
        ]),
        Err(FrameError::Stride {
            plane: 0,
            stride: 3,
            row: 4
        })
    );
    assert_eq!(
        frame.stage(&[
            Plane {
                data: &y[..7],
                stride: 4
            },
            Plane {
                data: &uv,
                stride: 4
            }
        ]),
        Err(FrameError::Truncated {
            plane: 0,
            expected: 8,
            got: 7
        })
    );
    assert!(frame.stage_packed(&[0; 11]).is_err());
    // Padded rows are accepted; the last row may stop at its last texel.
    let padded = [16u8; 8 + 4];
    assert_eq!(
        frame.stage(&[
            Plane {
                data: &padded[..12],
                stride: 8
            },
            Plane {
                data: &uv,
                stride: 4
            }
        ]),
        Ok(())
    );
}

/// ADR 0004, amendment 1: once its copy is submitted, the transfer buffer refuses a new
/// picture (`Busy`, nothing written, no wait) until the GPU has run the copy; the picture
/// shown is the one staged before, then the next one.
#[test]
fn transfer_buffer_is_busy_until_the_copy_has_run() {
    let rig = Rig::new();
    let frame = rig.frame(PixelFormat::Nv12, W, H, ColorInfo::BT709_LIMITED);
    let color = ColorInfo::BT709_LIMITED;
    let solid = |c: Rgb8| pack(PixelFormat::Nv12, W, H, move |_, _| encode_ycbcr(c, color));
    let (green, blue) = (Rgb8::new(0, 191, 0), Rgb8::new(0, 0, 191));

    frame.stage_packed(&solid(green)).unwrap();
    let mut encoder = rig.gpu.device.create_command_encoder(&Default::default());
    frame.encode_upload(&mut encoder);
    assert_eq!(
        frame.stage_packed(&solid(blue)),
        Err(FrameError::Busy),
        "recorded, not submitted"
    );
    rig.gpu.queue.submit([encoder.finish()]);
    // Without a poll the mapping cannot have completed.
    assert_eq!(frame.stage_packed(&solid(blue)), Err(FrameError::Busy));
    rig.gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    let want = |c: Rgb8| decode_ycbcr(encode_ycbcr(c, color), color);
    let check = |img: &[u8], c: Rgb8| {
        let got = px(img, W, W / 2, H / 2);
        for i in 0..3 {
            assert!(
                (i16::from(got[i]) - i16::from(want(c)[i])).abs() <= 1,
                "{got:?}, want {:?}",
                want(c)
            );
        }
    };
    check(&rig.render(&frame), green);

    // Mapped again: the next picture is accepted and replaces the first one.
    frame.stage_packed(&solid(blue)).unwrap();
    rig.copy(&frame);
    check(&rig.render(&frame), blue);
}
