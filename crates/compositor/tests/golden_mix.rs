//! Golden tests of the Program composition (`Mixer`): solid colours, Cut / Fade in the gamma
//! and linear domains, the `Rgba16Float` → `Rgba8Unorm` chain (spec `compositor.md`, Phase 1:
//! A2, A4, A18 on the compositor side).
//!
//! Expected values are analytic, or derived from the Phase 0 golden test pattern. Needs a GPU
//! adapter (software is enough); fails without one.

// Test code: panicking on failure is the intended behaviour.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stdout
)]

use std::path::PathBuf;
use vma_compositor::{Mix, MixSpace, Mixer, RenderTarget, Source, headless, readback};
use vma_core::{Resolution, Rgb8};

const W: u32 = 320;
const H: u32 = 180;

struct Rig {
    gpu: headless::HeadlessGpu,
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
        let resolution = Resolution::new(W, H).unwrap();
        let mixer = Mixer::new(&gpu.device, resolution);
        let out = RenderTarget::new(&gpu.device, resolution, "out");
        Self { gpu, mixer, out }
    }

    fn render(&self, frame: Mix) -> Vec<u8> {
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        self.mixer
            .encode(&self.gpu.queue, &mut encoder, &frame, self.out.view());
        self.gpu.queue.submit([encoder.finish()]);
        readback::read_rgba8(&self.gpu.device, &self.gpu.queue, &self.out).unwrap()
    }

    /// Renders a mix of two solid colours and checks the picture is uniform; returns its RGB.
    fn mix_colors(&self, a: Rgb8, b: Rgb8, mix: f32, space: MixSpace) -> [u8; 3] {
        let img = self.render(Mix {
            from: Source::Color(a),
            to: Source::Color(b),
            mix,
            space,
        });
        let first: [u8; 4] = img[..4].try_into().unwrap();
        assert!(
            img.as_chunks::<4>().0.iter().all(|p| *p == first),
            "picture not uniform"
        );
        assert_eq!(first[3], 255, "opaque");
        [first[0], first[1], first[2]]
    }
}

fn rgb(c: Rgb8) -> [f64; 3] {
    [c.r, c.g, c.b].map(f64::from)
}

fn assert_near(got: [u8; 3], want: [f64; 3], tol: f64, what: &str) {
    for (g, w) in got.iter().zip(want) {
        assert!(
            (f64::from(*g) - w).abs() <= tol,
            "{what}: got {got:?}, want {want:?} ±{tol}"
        );
    }
}

const PAIRS: [(Rgb8, Rgb8); 5] = [
    (Rgb8::BLACK, Rgb8::WHITE),
    (Rgb8::new(255, 0, 0), Rgb8::new(0, 0, 255)),
    (Rgb8::new(16, 128, 235), Rgb8::new(235, 128, 16)),
    (Rgb8::new(1, 2, 3), Rgb8::new(250, 251, 252)),
    (Rgb8::new(191, 191, 0), Rgb8::new(0, 191, 191)),
];

#[test]
fn every_code_value_survives_the_float_chain() {
    // Colour → Rgba16Float layer → Program16 → Rgba8Unorm is lossless for 8-bit values.
    let rig = Rig::new();
    for k in 0..=255u8 {
        let c = Rgb8::new(k, 255 - k, k / 2);
        let got = rig.mix_colors(c, Rgb8::WHITE, 0.0, MixSpace::Gamma);
        assert_eq!(got, [c.r, c.g, c.b], "code {k}");
    }
}

#[test]
fn fade_at_0_and_100_percent_is_exact() {
    // A4: no residue of the other source at either end, in both domains.
    let rig = Rig::new();
    for space in [MixSpace::Gamma, MixSpace::Linear] {
        for (a, b) in PAIRS {
            assert_eq!(
                rig.mix_colors(a, b, 0.0, space),
                [a.r, a.g, a.b],
                "{space:?}"
            );
            assert_eq!(
                rig.mix_colors(a, b, 1.0, space),
                [b.r, b.g, b.b],
                "{space:?}"
            );
        }
    }
}

#[test]
fn fade_50_percent_gamma_is_the_mean_of_the_codes() {
    // A2: half-way, gamma domain = average of the encoded values (a half code may round
    // either way, hence ±0.5).
    let rig = Rig::new();
    for (a, b) in PAIRS {
        let want = [0, 1, 2].map(|i| (rgb(a)[i] + rgb(b)[i]) / 2.0);
        assert_near(
            rig.mix_colors(a, b, 0.5, MixSpace::Gamma),
            want,
            0.5,
            "gamma 50 %",
        );
    }
}

#[test]
fn fade_50_percent_linear_mixes_display_light() {
    let rig = Rig::new();
    let decode = |v: f64| (v / 255.0).powf(2.4);
    let encode = |l: f64| 255.0 * l.powf(1.0 / 2.4);
    for (a, b) in PAIRS {
        let want = [0, 1, 2].map(|i| encode((decode(rgb(a)[i]) + decode(rgb(b)[i])) / 2.0));
        assert_near(
            rig.mix_colors(a, b, 0.5, MixSpace::Linear),
            want,
            1.0,
            "linear 50 %",
        );
    }
    // Black ↔ white half-way: half the light, i.e. 255 × 0.5^(1/2.4) ≈ 191.
    assert_near(
        rig.mix_colors(Rgb8::BLACK, Rgb8::WHITE, 0.5, MixSpace::Linear),
        [191.0; 3],
        1.0,
        "black/white",
    );
}

#[test]
fn t_bar_at_128_mixes_128_of_255() {
    // A18: the T-Bar value maps linearly to the mix.
    let rig = Rig::new();
    let m = 128.0 / 255.0;
    for (a, b) in PAIRS {
        let want = [0, 1, 2].map(|i| rgb(a)[i] + (rgb(b)[i] - rgb(a)[i]) * m);
        assert_near(
            rig.mix_colors(a, b, m as f32, MixSpace::Gamma),
            want,
            0.5,
            "T-Bar 128",
        );
    }
}

#[test]
fn out_of_range_mix_is_clamped() {
    let rig = Rig::new();
    let (a, b) = PAIRS[1];
    assert_eq!(rig.mix_colors(a, b, -3.0, MixSpace::Gamma), [a.r, a.g, a.b]);
    assert_eq!(rig.mix_colors(a, b, 7.0, MixSpace::Gamma), [b.r, b.g, b.b]);
    assert_eq!(
        rig.mix_colors(a, b, f32::NAN, MixSpace::Gamma),
        [a.r, a.g, a.b]
    );
}

fn golden_pattern() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/test_pattern_320x180_f0.png");
    let img = image::open(&path).unwrap().to_rgba8();
    assert_eq!(img.dimensions(), (W, H));
    img.into_raw()
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let mse = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| (f64::from(x) - f64::from(y)).powi(2))
        .sum::<f64>()
        / a.len() as f64;
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0f64.powi(2) / mse).log10()
    }
}

#[test]
fn test_pattern_through_the_float_chain_matches_the_phase_0_golden() {
    let rig = Rig::new();
    let img = rig.render(Mix::single(Source::TestPattern { frame: 0 }));
    let score = psnr(&img, &golden_pattern());
    eprintln!("pattern via mixer: PSNR {score:.2} dB");
    assert!(score >= 45.0, "PSNR {score:.2} dB");
}

#[test]
fn fade_50_percent_pattern_to_color() {
    // A2 with a picture: half-way between the pattern and a colour, against the analytic
    // average of the golden pattern and that colour.
    let rig = Rig::new();
    let color = Rgb8::new(40, 200, 120);
    let img = rig.render(Mix {
        from: Source::TestPattern { frame: 0 },
        to: Source::Color(color),
        mix: 0.5,
        space: MixSpace::Gamma,
    });
    let golden = golden_pattern();
    let want: Vec<u8> = golden
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| {
            let avg =
                |i: usize, c: u8| ((u16::from(p[i]) + u16::from(c)) as f64 / 2.0).round() as u8;
            [avg(0, color.r), avg(1, color.g), avg(2, color.b), 255]
        })
        .collect();
    let score = psnr(&img, &want);
    eprintln!("fade 50 % pattern → colour: PSNR {score:.2} dB");
    assert!(score >= 45.0, "PSNR {score:.2} dB");
}

#[test]
fn thumbnail_atlas_cells() {
    use vma_compositor::thumbnails::{self, THUMBNAIL_HEIGHT, THUMBNAIL_WIDTH, ThumbnailPainter};
    let gpu = pollster::block_on(headless::request_gpu()).expect("GPU adapter required");
    let painter = ThumbnailPainter::new(&gpu.device);
    let atlas = thumbnails::new_atlas(&gpu.device, "atlas");
    let green = Rgb8::new(10, 200, 30);
    let cells = [
        (0, Source::Color(green)),
        (9, Source::TestPattern { frame: 0 }),
        (63, Source::Color(Rgb8::WHITE)),
        (5, Source::Black),
    ];
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    painter.encode(&gpu.queue, &mut encoder, atlas.view(), 0, cells.into_iter());
    gpu.queue.submit([encoder.finish()]);
    let img = readback::read_rgba8(&gpu.device, &gpu.queue, &atlas).unwrap();
    let aw = atlas.resolution().width();
    let cell = |c: usize| -> Vec<u8> {
        let (x, y, w, h) = thumbnails::cell_rect(c).unwrap();
        (y..y + h)
            .flat_map(|row| {
                let start = ((row * aw + x) * 4) as usize;
                img[start..start + (w * 4) as usize].to_vec()
            })
            .collect()
    };
    let uniform = |c: usize, rgb: [u8; 3]| {
        assert!(
            cell(c)
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| p[..3] == rgb && p[3] == 255),
            "cell {c} is not {rgb:?}"
        );
    };
    uniform(0, [green.r, green.g, green.b]);
    uniform(63, [255; 3]);
    uniform(5, [0; 3]);
    // The pattern at thumbnail size is the 320 × 180 golden.
    assert_eq!((THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT), (W, H));
    let score = psnr(&cell(9), &golden_pattern());
    eprintln!("pattern thumbnail: PSNR {score:.2} dB");
    assert!(score >= 45.0, "PSNR {score:.2} dB");
}
