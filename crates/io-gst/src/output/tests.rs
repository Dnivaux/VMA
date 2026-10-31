//! Output pipelines (spec `phase-2.md`, step 4), with real GStreamer pipelines and files read
//! back by `decodebin`:
//!
//! - B3: PTS and audio from *n* in the file at the 6 master rates (raw MKV, independent of the
//!   encoders), gap of one period for a frame not read back; with the real engine: flash and
//!   beep aligned in the file;
//! - B4 / B5: MKV and fragmented MP4 readable after the recording process is killed; complete
//!   after a normal stop;
//! - B6 (`io-gst` side): a blocking sink loses its own frames only;
//! - B9: encoder ladder with masked elements, HEVC refused without hardware;
//! - B10 (`io-gst` side): colorimetry in the caps and the container header;
//! - write errors: unwritable path refused at start, disk full fails the output only.
//!
//! The encoded tests use whatever the ladder picks on the machine (NVENC on the dev machine,
//! `openh264enc` in CI).

use super::*;
use crate::pipeline::tests::init;
use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::Mutex;
use vma_core::{OutputId, Resolution};
use vma_engine::{Command, EngineConfig, InputSpec, Journal, OnAir, RenderEngine, Source};

/// The child of the "killed process" tests records while this is set (`mkv|path`).
const CHILD_ENV: &str = "VMA_OUTPUT_CHILD";

fn format(rate: FrameRate, width: u32, height: u32) -> OutputFormat {
    OutputFormat {
        rate,
        resolution: Resolution::new(width, height).unwrap(),
    }
}

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("vma-out-{}-{name}", std::process::id()))
}

fn raw_plan(path: &Path) -> Plan {
    Plan {
        video: Video::Raw,
        audio: Audio::Raw,
        container: Container::Mkv,
        sink: Sink::File(path.to_owned()),
    }
}

fn start(format: OutputFormat, plan: &Plan) -> (GstOutput, OutputFeed) {
    init();
    GstOutput::start(format, plan, &mut encoder::probe_element).unwrap()
}

/// Frame `n` as the engine builds it: black, silent audio from `first_sample` to the start
/// of frame `n + 1`.
fn frame(format: OutputFormat, n: u64, first_sample: u64, discont: bool) -> Arc<OutputFrame> {
    let (w, h) = (
        format.resolution.width() as usize,
        format.resolution.height() as usize,
    );
    let chroma = w.div_ceil(2) * 2 * h.div_ceil(2);
    let mut nv12 = vec![16; w * h];
    nv12.resize(w * h + chroma, 128);
    let rate = format.rate;
    let end = audio_sample_at(rate, n + 1);
    let pts = video_pts_ns(rate, n);
    Arc::new(OutputFrame {
        index: n,
        pts_ns: pts,
        duration_ns: video_pts_ns(rate, n + 1) - pts,
        resolution: format.resolution,
        nv12,
        audio_first_sample: first_sample,
        audio: vec![0.0; (end - first_sample) as usize * AUDIO_CHANNELS],
        discont,
    })
}

/// Feeds frames `from..from + count` except `missing` (not read back: the next frame carries
/// its audio, as in the engine).
fn feed_frames(
    feed: &mut OutputFeed,
    format: OutputFormat,
    from: u64,
    count: u64,
    missing: &[u64],
) -> Vec<u64> {
    let mut next_sample = audio_sample_at(format.rate, from);
    let mut fed = Vec::new();
    for n in from..from + count {
        if missing.contains(&(n - from)) {
            continue;
        }
        feed.consume(frame(format, n, next_sample, n == from));
        next_sample = audio_sample_at(format.rate, n + 1);
        fed.push(n - from);
    }
    fed
}

/// What `decodebin` finds in a file.
#[derive(Debug, Default)]
struct ReadBack {
    video_caps: Option<gst::Caps>,
    /// PTS (ns) and first luma byte of each picture.
    video: Vec<(u64, u8)>,
    /// PTS (ns) and samples per channel of each audio buffer.
    audio: Vec<(u64, u64)>,
    /// Samples (per channel) where a beep starts, counted from the first one in the file.
    beeps: Vec<u64>,
    audio_samples: u64,
    quiet_run: u64,
}

impl ReadBack {
    fn add(&mut self, video: bool, sample: &gst::Sample) {
        let (Some(buffer), Some(caps)) = (sample.buffer(), sample.caps()) else {
            return;
        };
        let pts = buffer.pts().map_or(u64::MAX, gst::ClockTime::nseconds);
        let map = buffer.map_readable().unwrap();
        if video {
            self.video_caps.get_or_insert_with(|| caps.to_owned());
            self.video.push((pts, map.first().copied().unwrap_or(0)));
            return;
        }
        let s = caps.structure(0).unwrap();
        assert_eq!(s.get::<&str>("format").unwrap(), "F32LE", "{caps}");
        let channels = s.get::<i32>("channels").unwrap() as usize;
        let interleaved = s.get::<&str>("layout").unwrap() == "interleaved";
        let samples = map.len() / 4 / channels;
        for i in 0..samples {
            let at = if interleaved { i * channels } else { i };
            let v = f32::from_le_bytes(map[at * 4..at * 4 + 4].try_into().unwrap());
            if v.abs() > 0.05 {
                if self.quiet_run > 1_000 {
                    self.beeps.push(self.audio_samples + i as u64);
                }
                self.quiet_run = 0;
            } else {
                self.quiet_run += 1;
            }
        }
        self.audio.push((pts, samples as u64));
        self.audio_samples += samples as u64;
    }
}

/// Decodes `path` to the end; the error that stopped it early, if any (a truncated file).
fn read_back(path: &Path) -> (ReadBack, Option<String>) {
    init();
    let pipeline = gst::Pipeline::new();
    let src = gst::ElementFactory::make("filesrc")
        .property("location", path.to_str().unwrap())
        .build()
        .unwrap();
    let decode = gst::ElementFactory::make("decodebin").build().unwrap();
    pipeline.add_many([&src, &decode]).unwrap();
    src.link(&decode).unwrap();
    let out = Arc::new(Mutex::new(ReadBack::default()));
    let weak = pipeline.downgrade();
    let shared = Arc::clone(&out);
    decode.connect_pad_added(move |_, pad| {
        let Some(pipeline) = weak.upgrade() else {
            return;
        };
        let caps = pad.current_caps().unwrap();
        let video = caps.structure(0).unwrap().name().starts_with("video/");
        let sink = gst_app::AppSink::builder().sync(false).build();
        pipeline.add(&sink).unwrap();
        sink.sync_state_with_parent().unwrap();
        pad.link(&sink.static_pad("sink").unwrap()).unwrap();
        let shared = Arc::clone(&shared);
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |s| {
                    let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    shared.lock().unwrap().add(video, &sample);
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
    });
    pipeline.set_state(gst::State::Playing).unwrap();
    let bus = pipeline.bus().unwrap();
    let mut error = None;
    for message in bus.iter_timed(gst::ClockTime::from_seconds(30)) {
        match message.view() {
            gst::MessageView::Eos(_) => break,
            gst::MessageView::Error(e) => {
                error = Some(format!("{} ({:?})", e.error(), e.debug()));
                break;
            }
            _ => {}
        }
    }
    pipeline.set_state(gst::State::Null).unwrap();
    drop(pipeline);
    let out = std::mem::take(&mut *out.lock().unwrap());
    (out, error)
}

const MS: u64 = 1_000_000;

/// B3 + B10: raw MKV at the 6 master rates, the output starting mid-run (frame 1 000 + …),
/// frame 10 not read back.
#[test]
fn file_carries_the_pts_and_audio_of_n_at_every_master_rate() {
    for rate in FrameRate::MASTER_RATES {
        let format = format(rate, 64, 36);
        let path = temp(&format!("b3-{}.mkv", rate.num()));
        let (output, mut feed) = start(format, &raw_plan(&path));
        let from = 1_000 + u64::from(rate.num() % 7);
        let fed = feed_frames(&mut feed, format, from, 30, &[10]);
        drop(feed);
        output.stop(Duration::from_secs(5)).unwrap();

        let (back, error) = read_back(&path);
        assert_eq!(error, None, "{rate}");
        // Video: picture k at k × den / num, to the millisecond of Matroska; a hole at 10.
        assert_eq!(back.video.len(), fed.len(), "{rate}");
        for (&(pts, _), &k) in back.video.iter().zip(&fed) {
            let exact = video_pts_ns(rate, k);
            assert!(
                pts.abs_diff(exact) <= MS,
                "{rate}: picture {k} at {pts} ns, not {exact}"
            );
        }
        // Audio: every sample from the first frame to the end of the last one, continuous
        // across the missing picture.
        let samples = audio_sample_at(rate, from + 30) - audio_sample_at(rate, from);
        assert_eq!(back.audio_samples, samples, "{rate}");
        let mut at = 0;
        for &(pts, count) in &back.audio {
            assert!(
                pts.abs_diff(sample_ns(at)) <= MS,
                "{rate}: audio at {pts}, not {at}"
            );
            at += count;
        }
        // B10: BT.709 limited range, chroma sited left, from the Matroska header.
        let caps = back.video_caps.unwrap();
        let s = caps.structure(0).unwrap();
        assert_eq!(s.get::<&str>("colorimetry").unwrap(), "bt709", "{caps}");
        assert_eq!(s.get::<&str>("chroma-site").unwrap(), "mpeg2", "{caps}");
        std::fs::remove_file(&path).unwrap();
    }
}

fn spawn_engine(rate: FrameRate, sync_test: bool) -> RenderEngine {
    let gpu =
        pollster::block_on(vma_compositor::headless::request_gpu()).expect("GPU adapter required");
    let config = EngineConfig {
        rate,
        resolution: Resolution::new(320, 180).unwrap(),
        sync_test,
        ..EngineConfig::default()
    };
    let (engine, _readers) =
        RenderEngine::spawn(gpu.device, gpu.queue, config, Journal::Disabled).unwrap();
    engine
        .commands()
        .send(
            Source::Script,
            Command::AddInput {
                spec: InputSpec::TestSource {
                    rate,
                    resolution: config.resolution,
                },
                name: None,
            },
        )
        .unwrap();
    let feedback = engine.feedback();
    let end = Instant::now() + Duration::from_secs(5);
    while !matches!(feedback.on_air(), OnAir::Input(_)) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
    engine
}

fn wait_delivered(engine: &RenderEngine, id: OutputId, frames: u64) {
    let hub = engine.outputs();
    let end = Instant::now() + Duration::from_secs(10);
    while hub.counters(id).map_or(0, |c| c.delivered) < frames && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// B3 with the real engine and its threads, in sync-test mode: every frame in the file at
/// k × 20 ms, the white flash and the beep start at the same time.
#[test]
fn engine_frames_reach_the_file_with_flash_and_beep_aligned() {
    init();
    let rate = FrameRate::FPS_50;
    let engine = spawn_engine(rate, true);
    let format = format(rate, 320, 180);
    let path = temp("engine.mkv");
    let (output, feed) = start(format, &raw_plan(&path));
    let id = OutputId(1);
    let hub = engine.outputs();
    hub.attach(id, Box::new(feed)).unwrap();
    wait_delivered(&engine, id, 130);
    let counters = hub.counters(id).unwrap();
    hub.detach(id);
    output.stop(Duration::from_secs(5)).unwrap();
    let render = engine.render_stats().snapshot();
    let distribution = hub.distribution();
    drop(engine);

    let (back, error) = read_back(&path);
    assert_eq!(error, None);
    assert_eq!(counters.dropped, 0, "{counters:?}");
    assert_eq!(distribution.pool_exhausted, 0, "{distribution:?}");
    assert_eq!(render.output_dropped, 0, "every frame read back");
    assert!(
        back.video.len() as u64 >= counters.delivered,
        "{counters:?}"
    );
    for (k, &(pts, _)) in back.video.iter().enumerate() {
        let exact = video_pts_ns(rate, k as u64);
        assert!(pts.abs_diff(exact) <= MS, "picture {k} at {pts} ns");
    }
    // Flashes: white pictures, one second apart.
    let flashes: Vec<usize> = back
        .video
        .iter()
        .enumerate()
        .filter(|(_, (_, y))| *y > 200)
        .map(|(k, _)| k)
        .collect();
    assert!(flashes.len() >= 2, "flashes at {flashes:?}");
    assert!(flashes.windows(2).all(|w| w[1] - w[0] == 50), "{flashes:?}");
    assert!(back.beeps.len() >= 2, "beeps at {:?}", back.beeps);
    for (flash, beep) in flashes.iter().zip(&back.beeps) {
        let video = video_pts_ns(rate, *flash as u64);
        let audio = sample_ns(*beep);
        assert!(
            video.abs_diff(audio) <= MS,
            "flash {flash} at {video} ns, beep at {audio} ns"
        );
    }
    std::fs::remove_file(&path).unwrap();
}

/// B6 (`io-gst` side): one output's sink takes 50 ms per buffer; it loses frames (counted),
/// the recording beside it loses none, the render misses no tick, and stopping the slow one
/// does not hang.
#[test]
fn blocking_sink_loses_only_its_own_frames() {
    init();
    let rate = FrameRate::FPS_50;
    let engine = spawn_engine(rate, false);
    let format = format(rate, 320, 180);
    let path = temp("beside-slow.mkv");
    let (fast, fast_feed) = start(format, &raw_plan(&path));
    let slow_plan = Plan {
        sink: Sink::Launch("identity sleep-time=50000 ! fakesink sync=false".into()),
        ..raw_plan(&path)
    };
    let (slow, slow_feed) = start(format, &slow_plan);
    let hub = engine.outputs();
    let missed_before = engine.clock_stats().snapshot().missed;
    hub.attach(OutputId(1), Box::new(fast_feed)).unwrap();
    hub.attach(OutputId(2), Box::new(slow_feed)).unwrap();
    wait_delivered(&engine, OutputId(1), 100);
    let fast_counters = hub.counters(OutputId(1)).unwrap();
    let slow_counters = hub.counters(OutputId(2)).unwrap();
    hub.detach(OutputId(1));
    hub.detach(OutputId(2));
    let missed = engine.clock_stats().snapshot().missed - missed_before;
    let distribution = hub.distribution();

    let stopping = Instant::now();
    let _ = slow.stop(Duration::from_secs(1));
    assert!(
        stopping.elapsed() < Duration::from_secs(2),
        "slow output hung"
    );
    fast.stop(Duration::from_secs(5)).unwrap();
    drop(engine);
    let (back, error) = read_back(&path);
    assert_eq!(error, None);
    eprintln!("fast {fast_counters:?}, slow {slow_counters:?}, {missed} missed");
    assert_eq!(missed, 0, "no missed tick");
    assert_eq!(fast_counters.dropped, 0);
    assert_eq!(distribution.pool_exhausted, 0, "{distribution:?}");
    assert!(slow_counters.dropped > 20, "{slow_counters:?}");
    assert!(back.video.len() as u64 >= fast_counters.delivered);
    std::fs::remove_file(&path).unwrap();
}

/// The child of the "killed process" tests: records H.264 + AAC at 25 fps in real time until
/// it is killed, printing each frame pushed. Does nothing unless started by them.
#[test]
fn child_records_until_killed() {
    let Ok(spec) = std::env::var(CHILD_ENV) else {
        return;
    };
    let (container, path) = spec.split_once('|').unwrap();
    let format = format(FrameRate::FPS_25, 320, 180);
    let settings = RecordSettings {
        container: if container == "mp4" {
            Container::Mp4
        } else {
            Container::Mkv
        },
        ..RecordSettings::new(path.into(), format)
    };
    init();
    let (_output, mut feed) = GstOutput::record(format, &settings).unwrap();
    let start = Instant::now();
    let mut stdout = std::io::stdout();
    // At most a minute: the parent kills it after 3 s.
    for n in 0..25 * 60 {
        feed.consume(frame(format, n, audio_sample_at(format.rate, n), n == 0));
        writeln!(stdout, "pushed {n}").unwrap();
        stdout.flush().unwrap();
        let next = start + Duration::from_nanos(video_pts_ns(format.rate, n + 1));
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
    }
}

/// Starts [`child_records_until_killed`], kills it once it has pushed 3 s of frames, and
/// returns what can be decoded from its file.
fn record_and_kill(container: &str) -> (ReadBack, u64) {
    let path = temp(&format!("killed.{container}"));
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "output::tests::child_records_until_killed",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, format!("{container}|{}", path.display()))
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut pushed = 0;
    for line in std::io::BufReader::new(child.stdout.take().unwrap()).lines() {
        if let Some(n) = line.unwrap().strip_prefix("pushed ") {
            pushed = n.parse::<u64>().unwrap() + 1;
            if pushed >= 75 {
                break;
            }
        }
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(pushed, 75, "the child stopped early");
    let (back, error) = read_back(&path);
    eprintln!(
        "{container}: {} pictures decoded after the kill (error: {error:?})",
        back.video.len()
    );
    std::fs::remove_file(&path).unwrap();
    (back, pushed)
}

/// Decodable up to at most 2 s before the kill.
fn assert_readable_until_kill(back: &ReadBack, pushed: u64) {
    let rate = FrameRate::FPS_25;
    let last = back.video.last().map_or(0, |(pts, _)| *pts);
    let killed = video_pts_ns(rate, pushed);
    assert!(
        last + 2_000 * MS >= killed,
        "last picture at {last} ns, killed at {killed} ns"
    );
    assert!(back.audio_samples > 0, "audio decodable too");
}

/// B4: the process recording an MKV is killed; the file decodes up to ≤ 2 s before.
#[test]
fn mkv_survives_a_killed_process() {
    let (back, pushed) = record_and_kill("mkv");
    assert_readable_until_kill(&back, pushed);
}

/// B5 (crash): a fragmented MP4 decodes up to its last fragment (1 s) after a kill.
#[test]
fn mp4_survives_a_killed_process_up_to_its_last_fragment() {
    let (back, pushed) = record_and_kill("mp4");
    assert_readable_until_kill(&back, pushed);
}

/// B5 (normal stop) and MKV: every picture and the whole audio track after a normal stop,
/// BT.709 signalled (B10); the encoders retained are reported.
#[test]
fn recordings_are_complete_after_a_normal_stop() {
    let format = format(FrameRate::FPS_25, 320, 180);
    for container in [Container::Mkv, Container::Mp4] {
        let path = temp(&format!("normal.{}", container.extension()));
        let settings = RecordSettings {
            container,
            ..RecordSettings::new(path.clone(), format)
        };
        init();
        let (output, mut feed) = GstOutput::record(format, &settings).unwrap();
        eprintln!(
            "{container:?}: video {}, audio {}",
            output.video_encoder().unwrap(),
            output.audio_encoder().unwrap()
        );
        feed_frames(&mut feed, format, 500, 50, &[]);
        drop(feed);
        output.stop(Duration::from_secs(10)).unwrap();
        let (back, error) = read_back(&path);
        assert_eq!(error, None, "{container:?}");
        assert_eq!(back.video.len(), 50, "{container:?}");
        // B10: BT.709 limited range signalled (container header or bitstream).
        let caps = back.video_caps.unwrap();
        let s = caps.structure(0).unwrap();
        assert_eq!(s.get::<&str>("colorimetry").unwrap(), "bt709", "{caps}");
        // AAC works in blocks of 1 024 samples: priming and padding at the ends.
        let samples = audio_sample_at(format.rate, 50);
        assert!(
            back.audio_samples.abs_diff(samples) <= 2048,
            "{container:?}: {} samples, not {samples}",
            back.audio_samples
        );
        std::fs::remove_file(&path).unwrap();
    }
}

/// B9: hardware masked → `openh264enc` (if installed, as in CI), otherwise an explicit
/// refusal listing every rung; HEVC masked → refused (no software HEVC without GPL); nothing
/// falls, and a recording starts normally afterwards.
#[test]
fn masked_hardware_falls_back_or_refuses_explicitly() {
    init();
    let format = format(FrameRate::FPS_25, 320, 180);
    let path = temp("masked.mkv");
    let mut masked = |name: &'static str| {
        if name == "openh264enc" || encoder::AAC.iter().any(|c| c.element == name) {
            encoder::probe_element(name)
        } else {
            Err("masked".to_owned())
        }
    };
    let h264 = Plan::recording(&RecordSettings::new(path.clone(), format));
    match GstOutput::start(format, &h264, &mut masked) {
        Ok((output, _feed)) => {
            let choice = output.video_encoder().unwrap();
            assert_eq!(choice.element, "openh264enc");
            assert_eq!(choice.skipped.len(), encoder::H264.len() - 1);
            assert!(choice.skipped.iter().all(|s| s.reason == "masked"));
        }
        Err(IoError::NoEncoder { what, tried }) => {
            assert!(gst::ElementFactory::find("openh264enc").is_none());
            assert_eq!((what, tried.len()), ("H.264", encoder::H264.len()));
        }
        Err(e) => panic!("{e}"),
    }
    let hevc = Plan::recording(&RecordSettings {
        codec: VideoCodec::Hevc,
        ..RecordSettings::new(path.clone(), format)
    });
    match GstOutput::start(format, &hevc, &mut masked) {
        Err(IoError::NoEncoder { what, tried }) => {
            assert_eq!((what, tried.len()), ("HEVC", encoder::HEVC.len()));
        }
        other => panic!("HEVC without hardware: {other:?}"),
    }
    let (output, feed) = GstOutput::record(format, &RecordSettings::new(path.clone(), format))
        .expect("an H.264 encoder (NVENC on the dev machine, openh264enc in CI)");
    eprintln!("ladder: {}", output.video_encoder().unwrap());
    drop(feed);
    output.stop(Duration::from_secs(5)).unwrap();
    let _ = std::fs::remove_file(&path);
}

/// A recording into a missing folder is refused at start, with the reason.
#[test]
fn unwritable_path_is_refused_at_start() {
    init();
    let format = format(FrameRate::FPS_25, 64, 36);
    let path = temp("missing-folder").join("x.mkv");
    match GstOutput::start(format, &raw_plan(&path), &mut encoder::probe_element) {
        Err(IoError::Pipeline(reason)) => assert!(reason.starts_with("cannot start"), "{reason}"),
        other => panic!("{other:?}"),
    }
}

/// Disk full (`/dev/full`): the output fails with the reason, its feed stops pushing without
/// blocking, and a recording beside it carries on and is finalised.
#[cfg(target_os = "linux")]
#[test]
fn disk_full_fails_the_output_only() {
    let format = format(FrameRate::FPS_25, 320, 180);
    let (mut full, mut full_feed) = start(format, &raw_plan(Path::new("/dev/full")));
    let path = temp("beside-full.mkv");
    let (good, mut good_feed) = start(format, &raw_plan(&path));
    let mut sample = audio_sample_at(format.rate, 0);
    let end = Instant::now() + Duration::from_secs(5);
    let mut n = 0;
    while full.poll().is_none() && Instant::now() < end {
        let f = frame(format, n, sample, n == 0);
        full_feed.consume(Arc::clone(&f));
        good_feed.consume(f);
        sample = audio_sample_at(format.rate, n + 1);
        n += 1;
    }
    let reason = full.poll().expect("disk full not reported").to_owned();
    eprintln!("after {n} frames: {reason}");
    assert!(good.bytes().unwrap() > 0);
    drop((full_feed, good_feed));
    assert!(full.stop(Duration::from_secs(1)).is_err());
    good.stop(Duration::from_secs(5)).unwrap();
    let (back, error) = read_back(&path);
    assert_eq!(error, None);
    assert_eq!(back.video.len() as u64, n);
    std::fs::remove_file(&path).unwrap();
}
