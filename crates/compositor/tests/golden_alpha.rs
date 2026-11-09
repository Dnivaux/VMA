//! Golden tests of alpha and keys (spec `phase-4.md`: D2, D1 on the compositor side;
//! ADR 0010): RGBA frames with straight or premultiplied alpha, keyed over a known Program as
//! the stinger or an overlay channel, with their opacity, in their order.
//!
//! Expected values are analytic: `key + (1 − α) · program` on the encoded values, within one
//! code. The `Rgba16Float` chain alone loses less than 0.1 code; the blend unit's precision is
//! the GPU's (WARP, CI Windows: 0.62 code off for 191 at α 128 over 128). Needs a GPU adapter
//! (software is enough); fails without one.

// Test code: panicking on failure is the intended behaviour.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stdout
)]

use vma_compositor::{
    FramePipelines, InputFrame, Key, Keys, Mix, MixSpace, Mixer, RenderTarget, Source, headless,
    readback,
};
use vma_core::{AlphaMode, ColorInfo, PixelFormat, Resolution, Rgb8};

const W: u32 = 320;
const H: u32 = 180;

/// Within this of the exact value: one of the two nearest codes (blend unit precision).
const EXACT: f64 = 1.0;
/// Through the scale pass (display light and back): one code.
const SCALED: f64 = 1.0;

struct Rig {
    gpu: headless::HeadlessGpu,
    pipelines: FramePipelines,
    program: Mixer,
    preview: Mixer,
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
        Self {
            pipelines: FramePipelines::new(&gpu.device),
            program: Mixer::with_keys(&gpu.device, resolution),
            preview: Mixer::new(&gpu.device, resolution),
            out: RenderTarget::new(&gpu.device, resolution, "out"),
            gpu,
        }
    }

    /// A uniform RGBA frame of `w` × `h`, uploaded by the real path.
    fn frame(&self, w: u32, h: u32, rgba: [u8; 4], alpha: AlphaMode) -> InputFrame {
        let frame = InputFrame::new(
            &self.gpu.device,
            &self.gpu.queue,
            &self.pipelines,
            PixelFormat::Rgba,
            res(w, h),
            ColorInfo::RGB_FULL,
            alpha,
        );
        frame.stage_packed(&rgba.repeat((w * h) as usize)).unwrap();
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        frame.encode_upload(&mut encoder);
        self.gpu.queue.submit([encoder.finish()]);
        self.gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        frame
    }

    fn render_with(&self, mixer: &Mixer, mix: Mix, keys: Keys) -> Vec<u8> {
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        mixer.encode_keyed(&self.gpu.queue, &mut encoder, &mix, &keys, self.out.view());
        self.gpu.queue.submit([encoder.finish()]);
        readback::read_rgba8(&self.gpu.device, &self.gpu.queue, &self.out).unwrap()
    }

    /// The Program: `background` with `keys` over it.
    fn render(&self, background: Rgb8, keys: Keys) -> Vec<u8> {
        self.render_with(&self.program, Mix::single(Source::Color(background)), keys)
    }

    /// Renders and checks the picture is uniform; returns its RGB.
    fn uniform(&self, background: Rgb8, keys: Keys) -> [u8; 3] {
        let img = self.render(background, keys);
        let first = px(&img, 0, 0);
        assert!(
            img.as_chunks::<4>().0.iter().all(|p| *p == first),
            "picture not uniform"
        );
        assert_eq!(first[3], 255, "the Program is opaque");
        [first[0], first[1], first[2]]
    }
}

fn res(w: u32, h: u32) -> Resolution {
    Resolution::new(w, h).unwrap()
}

fn px(img: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    img[i..i + 4].try_into().unwrap()
}

fn rgb_of(p: [u8; 4]) -> [u8; 3] {
    [p[0], p[1], p[2]]
}

fn codes(c: Rgb8) -> [f64; 3] {
    [c.r, c.g, c.b].map(f64::from)
}

/// `key` (premultiplied, codes) of alpha `a` (0–1) over `below` (codes).
fn over(key: [f64; 3], a: f64, below: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| key[i] + (1.0 - a) * below[i])
}

/// A straight colour `c` of alpha `a` (0–255), premultiplied, in codes, times `opacity`.
fn premultiplied(c: Rgb8, a: u8, opacity: f64) -> ([f64; 3], f64) {
    let a = f64::from(a) / 255.0 * opacity;
    (codes(c).map(|v| v * a), a)
}

fn assert_near(got: [u8; 3], want: [f64; 3], tol: f64, what: &str) {
    for (g, w) in got.iter().zip(want) {
        assert!(
            (f64::from(*g) - w).abs() <= tol,
            "{what}: got {got:?}, want {want:?} ±{tol}"
        );
    }
}

fn overlay(channel: usize, key: Key) -> Keys {
    let mut keys = Keys::default();
    keys.overlays[channel] = Some(key);
    keys
}

const COLORS: [Rgb8; 4] = [
    Rgb8::WHITE,
    Rgb8::new(255, 0, 0),
    Rgb8::new(16, 128, 235),
    Rgb8::new(191, 191, 0),
];
const BACKGROUNDS: [Rgb8; 3] = [Rgb8::BLACK, Rgb8::new(0, 0, 255), Rgb8::new(128, 64, 32)];
/// α = 0, ¼, ½, ¾ and 1.
const ALPHAS: [u8; 5] = [0, 64, 128, 191, 255];

#[test]
fn straight_alpha_is_keyed_over_the_program() {
    // D2: every channel and the stinger give `c·α + (1 − α)·background`.
    let rig = Rig::new();
    for c in COLORS {
        for a in ALPHAS {
            let frame = rig.frame(W, H, [c.r, c.g, c.b, a], AlphaMode::Straight);
            let key = Key {
                source: Source::Frame(&frame),
                opacity: 1.0,
            };
            for bg in BACKGROUNDS {
                let (k, ka) = premultiplied(c, a, 1.0);
                let want = over(k, ka, codes(bg));
                for channel in 0..4 {
                    let got = rig.uniform(bg, overlay(channel, key));
                    let what = format!("overlay {} {c:?} α {a} over {bg:?}", channel + 1);
                    assert_near(got, want, EXACT, &what);
                }
                let stinger = Keys {
                    stinger: Some(key),
                    ..Keys::default()
                };
                let got = rig.uniform(bg, stinger);
                assert_near(
                    got,
                    want,
                    EXACT,
                    &format!("stinger {c:?} α {a} over {bg:?}"),
                );
            }
        }
    }
}

#[test]
fn premultiplied_alpha_is_taken_as_is() {
    // D2: a premultiplied frame (as a web page gives it) keys like the same straight colour.
    let rig = Rig::new();
    for c in COLORS {
        for a in ALPHAS {
            let p = |v: u8| (f64::from(v) * f64::from(a) / 255.0).round() as u8;
            let frame = rig.frame(W, H, [p(c.r), p(c.g), p(c.b), a], AlphaMode::Premultiplied);
            let key = Key {
                source: Source::Frame(&frame),
                opacity: 1.0,
            };
            for bg in BACKGROUNDS {
                let k = [p(c.r), p(c.g), p(c.b)].map(f64::from);
                let want = over(k, f64::from(a) / 255.0, codes(bg));
                let got = rig.uniform(bg, overlay(0, key));
                assert_near(got, want, EXACT, &format!("{c:?} α {a} over {bg:?}"));
            }
        }
    }
}

#[test]
fn opaque_frames_ignore_their_alpha_byte() {
    // Phases 1–3: an RGBA frame read as opaque covers the Program whatever its 4th byte.
    let rig = Rig::new();
    let frame = rig.frame(W, H, [16, 128, 235, 0], AlphaMode::Opaque);
    let key = Key {
        source: Source::Frame(&frame),
        opacity: 1.0,
    };
    assert_eq!(rig.uniform(Rgb8::WHITE, overlay(2, key)), [16, 128, 235]);
    // In the Program mix too (no key).
    let img = rig.render_with(
        &rig.program,
        Mix::single(Source::Frame(&frame)),
        Keys::default(),
    );
    assert_eq!(px(&img, 7, 7), [16, 128, 235, 255]);
}

#[test]
fn a_frame_with_alpha_in_the_program_mix_is_over_black() {
    let rig = Rig::new();
    let c = Rgb8::new(200, 100, 50);
    let frame = rig.frame(W, H, [c.r, c.g, c.b, 128], AlphaMode::Straight);
    let img = rig.render_with(
        &rig.program,
        Mix::single(Source::Frame(&frame)),
        Keys::default(),
    );
    let (k, _) = premultiplied(c, 128, 1.0);
    assert_near(rgb_of(px(&img, 100, 100)), k, EXACT, "over black");
    assert_eq!(px(&img, 100, 100)[3], 255);
}

#[test]
fn the_opacity_of_a_channel_scales_its_key() {
    // D2: an overlay in a Fade at 50 % (and at other points), opaque and half transparent.
    let rig = Rig::new();
    let bg = Rgb8::new(0, 0, 255);
    for a in [255, 128] {
        let c = Rgb8::new(255, 255, 0);
        let frame = rig.frame(W, H, [c.r, c.g, c.b, a], AlphaMode::Straight);
        for opacity in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let key = Key {
                source: Source::Frame(&frame),
                opacity,
            };
            let (k, ka) = premultiplied(c, a, f64::from(opacity));
            let got = rig.uniform(bg, overlay(1, key));
            let what = format!("α {a} at opacity {opacity}");
            assert_near(got, over(k, ka, codes(bg)), EXACT, &what);
        }
    }
    // Out of range or NaN: clamped, NaN draws nothing.
    let frame = rig.frame(W, H, [255, 255, 255, 255], AlphaMode::Straight);
    let key = |opacity| Key {
        source: Source::Frame(&frame),
        opacity,
    };
    assert_eq!(rig.uniform(bg, overlay(0, key(2.0))), [255, 255, 255]);
    assert_eq!(rig.uniform(bg, overlay(0, key(-1.0))), [0, 0, 255]);
    assert_eq!(rig.uniform(bg, overlay(0, key(f32::NAN))), [0, 0, 255]);
}

#[test]
fn keys_are_stacked_stinger_then_channels_1_to_4() {
    // D1: each layer over the one below, in that order whatever is active.
    let rig = Rig::new();
    let bg = Rgb8::new(128, 64, 32);
    let layers = [
        ([255, 0, 0], 191),
        ([0, 255, 0], 128),
        ([0, 0, 255], 64),
        ([255, 255, 255], 128),
        ([16, 128, 235], 191),
    ];
    let frames: Vec<InputFrame> = layers
        .iter()
        .map(|&([r, g, b], a)| rig.frame(W, H, [r, g, b, a], AlphaMode::Straight))
        .collect();
    let key = |i: usize| {
        Some(Key {
            source: Source::Frame(&frames[i]),
            opacity: 1.0,
        })
    };
    let all = Keys {
        stinger: key(0),
        overlays: [key(1), key(2), key(3), key(4)],
    };
    let want = layers.iter().fold(codes(bg), |below, &([r, g, b], a)| {
        let (k, ka) = premultiplied(Rgb8::new(r, g, b), a, 1.0);
        over(k, ka, below)
    });
    assert_near(rig.uniform(bg, all), want, EXACT, "all five");
    // Swapping two channels changes the picture: the order is the channel's, not the call's.
    let swapped = Keys {
        stinger: key(0),
        overlays: [key(1), key(2), key(4), key(3)],
    };
    assert_ne!(rig.uniform(bg, swapped), rig.uniform(bg, all));
    // Channels 2 and 4 only: 1 and 3 are skipped, nothing else changes.
    let some = Keys {
        stinger: None,
        overlays: [None, key(2), None, key(4)],
    };
    let want = [2, 4].iter().fold(codes(bg), |below, &i| {
        let ([r, g, b], a) = layers[i];
        let (k, ka) = premultiplied(Rgb8::new(r, g, b), a, 1.0);
        over(k, ka, below)
    });
    assert_near(rig.uniform(bg, some), want, EXACT, "channels 2 and 4");
}

#[test]
fn keys_stay_over_a_program_fade() {
    // D1: a Fade of the Program changes what is under the overlay, never the overlay.
    let rig = Rig::new();
    let (a, b) = (Rgb8::BLACK, Rgb8::WHITE);
    let c = Rgb8::new(255, 0, 0);
    let frame = rig.frame(W, H, [c.r, c.g, c.b, 128], AlphaMode::Straight);
    let keys = overlay(
        3,
        Key {
            source: Source::Frame(&frame),
            opacity: 1.0,
        },
    );
    for mix in [0.0, 0.5, 1.0] {
        let img = rig.render_with(
            &rig.program,
            Mix {
                from: Source::Color(a),
                to: Source::Color(b),
                mix,
                space: MixSpace::Gamma,
            },
            keys,
        );
        let below = [0, 1, 2].map(|i| codes(a)[i] + (codes(b)[i] - codes(a)[i]) * f64::from(mix));
        let (k, ka) = premultiplied(c, 128, 1.0);
        let what = format!("Fade at {mix}");
        assert_near(rgb_of(px(&img, 160, 90)), over(k, ka, below), EXACT, &what);
    }
}

#[test]
fn a_mixer_without_keys_ignores_them() {
    // The Preview mixer: no key layer, overlays not drawn (Q5).
    let rig = Rig::new();
    let frame = rig.frame(W, H, [255, 255, 255, 255], AlphaMode::Straight);
    let key = Some(Key {
        source: Source::Frame(&frame),
        opacity: 1.0,
    });
    let keys = Keys {
        stinger: key,
        overlays: [key; 4],
    };
    let img = rig.render_with(
        &rig.preview,
        Mix::single(Source::Color(Rgb8::new(0, 0, 255))),
        keys,
    );
    assert_eq!(px(&img, 10, 10), [0, 0, 255, 255]);
}

#[test]
fn the_letterbox_of_a_key_is_transparent() {
    // A 4:3 key in a 16:9 Program: the Program shows in the pillarbox (black in a Program
    // layer, transparent in a key), the scaled frame keeps its alpha.
    let rig = Rig::new();
    let bg = Rgb8::new(0, 0, 255);
    let c = Rgb8::new(255, 255, 0);
    for a in [255, 128] {
        let frame = rig.frame(64, 48, [c.r, c.g, c.b, a], AlphaMode::Straight);
        let key = Key {
            source: Source::Frame(&frame),
            opacity: 1.0,
        };
        let img = rig.render(bg, overlay(0, key));
        // 4:3 in 320 × 180: 240 wide, bars of 40 px.
        assert_eq!(rgb_of(px(&img, 10, 90)), [0, 0, 255], "left bar, α {a}");
        assert_eq!(rgb_of(px(&img, 310, 90)), [0, 0, 255], "right bar, α {a}");
        let (k, ka) = premultiplied(c, a, 1.0);
        let what = format!("scaled centre, α {a}");
        assert_near(
            rgb_of(px(&img, 160, 90)),
            over(k, ka, codes(bg)),
            SCALED,
            &what,
        );
    }
    // The same frame in the Program mix: black bars, as in Phases 1–3.
    let frame = rig.frame(64, 48, [c.r, c.g, c.b, 255], AlphaMode::Straight);
    let img = rig.render_with(
        &rig.program,
        Mix::single(Source::Frame(&frame)),
        Keys::default(),
    );
    assert_eq!(px(&img, 10, 90), [0, 0, 0, 255]);
}

#[test]
fn a_scaled_key_has_no_fringe() {
    // Premultiplied filtering: at the edge between transparent and opaque, the colour never
    // takes the colour of the transparent pixels (here black with α = 0).
    let rig = Rig::new();
    let (w, h) = (32, 18);
    let frame = InputFrame::new(
        &rig.gpu.device,
        &rig.gpu.queue,
        &rig.pipelines,
        PixelFormat::Rgba,
        res(w, h),
        ColorInfo::RGB_FULL,
        AlphaMode::Straight,
    );
    // Left half white opaque, right half black transparent.
    let data: Vec<u8> = (0..w * h)
        .flat_map(|i| {
            if i % w < w / 2 {
                [255, 255, 255, 255]
            } else {
                [0, 0, 0, 0]
            }
        })
        .collect();
    frame.stage_packed(&data).unwrap();
    let mut encoder = rig.gpu.device.create_command_encoder(&Default::default());
    frame.encode_upload(&mut encoder);
    rig.gpu.queue.submit([encoder.finish()]);
    let key = Key {
        source: Source::Frame(&frame),
        opacity: 1.0,
    };
    // Over white, a white key: any darkening at the edge would be a fringe.
    let img = rig.render(Rgb8::WHITE, overlay(0, key));
    for x in 0..W {
        assert_eq!(rgb_of(px(&img, x, 90)), [255, 255, 255], "x = {x}");
    }
}
