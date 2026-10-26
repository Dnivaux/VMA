//! Golden-frame tests of the test pattern.
//!
//! Reference images live in `<workspace>/tests/golden/`. To (re)generate them after an
//! intentional visual change: `VMA_UPDATE_GOLDEN=1 cargo nextest run -p vma-compositor`
//! then review the PNG diff before committing.
//!
//! Needs a GPU adapter; a software one (lavapipe / WARP) is enough. Fails — does not skip —
//! when no adapter is available, so CI cannot silently pass without running it.

// Test code: panicking on failure is the intended behaviour.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stdout
)]

use std::path::PathBuf;
use vma_compositor::{RenderTarget, TestPattern, headless, readback};
use vma_core::Resolution;

const W: u32 = 320;
const H: u32 = 180;
/// Minimum PSNR against the reference. Flat bars are bit-exact on every adapter tested;
/// the margin covers 1-LSB rounding differences on the ramp between GPUs.
const MIN_PSNR_DB: f64 = 45.0;

fn render(frame_index: u64) -> Vec<u8> {
    let gpu = pollster::block_on(headless::request_gpu()).expect("GPU adapter required");
    eprintln!(
        "adapter: {} ({:?}, {:?})",
        gpu.adapter_info.name, gpu.adapter_info.backend, gpu.adapter_info.device_type
    );
    let target = RenderTarget::new(&gpu.device, Resolution::new(W, H).unwrap(), "golden");
    let pattern = TestPattern::new(&gpu.device);
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    pattern.encode(&gpu.queue, &mut encoder, target.view(), frame_index);
    gpu.queue.submit([encoder.finish()]);
    readback::read_rgba8(&gpu.device, &gpu.queue, &target).unwrap()
}

fn px(img: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    img[i..i + 4].try_into().unwrap()
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden")
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let mse: f64 = a
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

fn check_golden(name: &str, img: &[u8]) {
    let path = golden_dir().join(name);
    if std::env::var_os("VMA_UPDATE_GOLDEN").is_some() {
        image::save_buffer(&path, img, W, H, image::ColorType::Rgba8).unwrap();
        eprintln!("golden updated: {}", path.display());
        return;
    }
    let reference = image::open(&path)
        .unwrap_or_else(|e| panic!("missing golden {} ({e}); see module docs", path.display()))
        .to_rgba8();
    assert_eq!(reference.dimensions(), (W, H));
    let score = psnr(img, reference.as_raw());
    if score < MIN_PSNR_DB {
        let actual = path.with_extension("actual.png");
        image::save_buffer(&actual, img, W, H, image::ColorType::Rgba8).unwrap();
        panic!(
            "{name}: PSNR {score:.2} dB < {MIN_PSNR_DB} dB; actual written to {}",
            actual.display()
        );
    }
    eprintln!("{name}: PSNR {score:.2} dB");
}

#[test]
fn bars_have_exact_75_percent_values() {
    let img = render(0);
    // 0.75 * 255 = 191.25, rounded to 191.
    let expected: [[u8; 3]; 8] = [
        [191, 191, 191], // white
        [191, 191, 0],   // yellow
        [0, 191, 191],   // cyan
        [0, 191, 0],     // green
        [191, 0, 191],   // magenta
        [191, 0, 0],     // red
        [0, 0, 191],     // blue
        [0, 0, 0],       // black
    ];
    for (i, rgb) in expected.iter().enumerate() {
        let x = 20 + 40 * i as u32;
        let p = px(&img, x, 60);
        assert_eq!(&p[..3], rgb, "bar {i} at x={x}");
        assert_eq!(p[3], 255, "opaque");
    }
}

#[test]
fn ramp_is_monotonic_from_black_to_white() {
    let img = render(0);
    let y = 135;
    let mut last = 0u8;
    for x in 0..W {
        let p = px(&img, x, y);
        assert!(p[0] == p[1] && p[1] == p[2], "grey at x={x}");
        assert!(p[0] >= last, "monotonic at x={x}");
        last = p[0];
    }
    assert!(px(&img, 0, y)[0] <= 1);
    assert!(px(&img, W - 1, y)[0] >= 254);
}

#[test]
fn marker_moves_with_frame_index() {
    let marker_x = |img: &[u8]| (0..W).find(|&x| px(img, x, 170)[0] == 255);
    assert_eq!(marker_x(&render(0)), Some(0));
    // Frame 50 = half period: start = 0.5 * (1 - 1/16) * 320 = 150.
    assert_eq!(marker_x(&render(50)), Some(150));
    // Periodic.
    assert_eq!(render(100), render(0));
}

#[test]
fn golden_frame_0() {
    check_golden("test_pattern_320x180_f0.png", &render(0));
}
