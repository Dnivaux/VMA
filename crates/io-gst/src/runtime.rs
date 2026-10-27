//! The `vma-io` thread (ADR 0004): owns every pipeline, driven by `vma-control` through the
//! engine's [`IoPort`]. Opening, stopping and restarting pipelines may block: this is the only
//! thread that does it. Pictures themselves go from the GStreamer streaming threads straight
//! into the feeds (see `pipeline.rs`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vma_core::{ColorInfo, InputId};
use vma_engine::{InputSpec, IoEvent, IoEventSender, IoPort, IoRequest};

use crate::IoError;
use crate::feeds::{Feeds, VideoFormat};
use crate::pipeline::{GstInput, Recipe};
use crate::still::{self, StillImage};
use crate::supervisor::retry_delay;

/// How often `vma-io` checks its pipelines (bus, watchdog, retries) without requests.
const POLL: Duration = Duration::from_millis(20);

/// One input backed by a pipeline: running, or waiting to be retried.
#[derive(Debug)]
pub(crate) struct PipelineSlot<F: Feeds> {
    input: InputId,
    recipe: Recipe,
    feeds: F,
    running: Option<GstInput>,
    /// Consecutive failed attempts (reset once pictures flow again).
    attempts: u32,
    retry_at: Instant,
}

impl<F: Feeds> PipelineSlot<F> {
    // Created for capture and file inputs (step 8); exercised by the tests until then.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn new(input: InputId, recipe: Recipe, feeds: F, now: Instant) -> Self {
        Self {
            input,
            recipe,
            feeds,
            running: None,
            attempts: 0,
            retry_at: now,
        }
    }

    /// Starts, watches and restarts the pipeline; reports losses through `emit`.
    pub(crate) fn tick(&mut self, now: Instant, emit: &mut impl FnMut(IoEvent)) {
        match &self.running {
            Some(running) => match running.poll(now) {
                Some(reason) => {
                    self.running = None;
                    self.lost(now, reason, emit);
                }
                None if running.pictures() > 0 => self.attempts = 0,
                None => {}
            },
            None if now >= self.retry_at => {
                match GstInput::start(self.input, &self.recipe, self.feeds.clone()) {
                    Ok(running) => self.running = Some(running),
                    Err(e) => self.lost(now, e.to_string(), emit),
                }
            }
            None => {}
        }
    }

    fn lost(&mut self, now: Instant, reason: String, emit: &mut impl FnMut(IoEvent)) {
        self.attempts = self.attempts.saturating_add(1);
        self.retry_at = now + retry_delay(self.attempts);
        emit(IoEvent::SignalLost {
            input: self.input,
            attempts: self.attempts,
            reason,
        });
    }
}

/// The `vma-io` thread. Dropping it stops every pipeline and joins the thread.
#[derive(Debug)]
pub struct IoRuntime {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    gstreamer: Result<(), String>,
}

impl IoRuntime {
    /// Initialises GStreamer and starts `vma-io` on the engine's port. Without GStreamer
    /// (initialisation failed), still images keep working and capture / file inputs are
    /// reported lost ([`Self::gstreamer_error`]).
    pub fn start(port: IoPort) -> Result<Self, IoError> {
        Self::start_with(port, || gstreamer::init().map_err(|e| e.to_string()))
    }

    pub(crate) fn start_with(
        port: IoPort,
        init: impl FnOnce() -> Result<(), String>,
    ) -> Result<Self, IoError> {
        let gstreamer = init();
        if let Err(e) = &gstreamer {
            tracing::error!(error = %e, "GStreamer unavailable: capture and file inputs disabled");
        }
        let stop = Arc::new(AtomicBool::new(false));
        let worker = Worker {
            port,
            gstreamer: gstreamer.clone(),
            pipelines: Vec::new(),
        };
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("vma-io".into())
            .spawn(move || worker.run(&thread_stop))?;
        Ok(Self {
            stop,
            thread: Some(thread),
            gstreamer,
        })
    }

    /// Why GStreamer could not be initialised, if it could not.
    pub fn gstreamer_error(&self) -> Option<&str> {
        self.gstreamer.as_ref().err().map(String::as_str)
    }
}

impl Drop for IoRuntime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("io thread panicked");
        }
    }
}

struct Worker {
    port: IoPort,
    gstreamer: Result<(), String>,
    pipelines: Vec<PipelineSlot<vma_engine::FeedRegistry>>,
}

impl Worker {
    fn run(mut self, stop: &AtomicBool) {
        while !stop.load(Ordering::Acquire) {
            match self.port.requests.recv_timeout(POLL) {
                Ok(Some(request)) => self.handle(request),
                Ok(None) => {}
                Err(_) => break,
            }
            let now = Instant::now();
            let events = &self.port.events;
            for slot in &mut self.pipelines {
                slot.tick(now, &mut |e| send(events, e));
            }
        }
        // Pipelines stop here, on `vma-io`.
        self.pipelines.clear();
        tracing::info!("io thread stopped");
    }

    fn handle(&mut self, request: IoRequest) {
        match request {
            IoRequest::Open { input, spec } => self.open(input, &spec),
            IoRequest::Close { input } => self.pipelines.retain(|p| p.input != input),
            other => tracing::debug!(request = ?other, "io request not handled yet (step 8)"),
        }
    }

    fn open(&mut self, input: InputId, spec: &InputSpec) {
        let lost = |reason: String| {
            send(
                &self.port.events,
                IoEvent::SignalLost {
                    input,
                    attempts: 0,
                    reason,
                },
            );
        };
        match spec {
            InputSpec::Image { path } => {
                // Decoded once, here (file I/O is allowed on `vma-io`), uploaded once.
                if let Err(e) = open_still(&self.port.feeds, input, path) {
                    lost(e.to_string());
                }
            }
            InputSpec::Capture { .. } | InputSpec::File { .. } => match &self.gstreamer {
                Err(e) => lost(IoError::GstreamerUnavailable(e.clone()).to_string()),
                Ok(()) => lost("capture and file inputs arrive at step 8".into()),
            },
            InputSpec::Color { .. } | InputSpec::TestSource { .. } => {}
        }
    }
}

fn open_still(
    feeds: &vma_engine::FeedRegistry,
    input: InputId,
    path: &std::path::Path,
) -> Result<(), IoError> {
    let StillImage { resolution, rgba } = still::decode_file(path)?;
    let mut sink = Feeds::create(
        feeds,
        input,
        VideoFormat {
            pixel: StillImage::FORMAT,
            resolution,
            color: ColorInfo::RGB_FULL,
            rate: None,
        },
    )?;
    sink.push_packed(&rgba, 0)
        .map_err(|e| IoError::Pipeline(e.to_string()))?;
    tracing::info!(%input, path = %path.display(), %resolution, "still image loaded");
    Ok(())
}

fn send(events: &IoEventSender, event: IoEvent) {
    if let Err(e) = events.send(event) {
        tracing::error!(error = %e, "io event dropped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::tests::{FakeFeeds, init, live};

    /// A8: an erroring pipeline is reported lost, retried after the backoff, and delivers
    /// pictures again through a new feed (the input goes live again on attach).
    #[test]
    fn lost_pipeline_is_reported_and_reconnected() {
        init();
        let feeds = FakeFeeds::default();
        let start = Instant::now();
        let mut slot = PipelineSlot::new(
            InputId(4),
            live(
                "videotestsrc is-live=true ! video/x-raw,framerate=50/1 ! identity error-after=10",
            ),
            feeds.clone(),
            start,
        );
        let mut losses: Vec<(Instant, u32)> = Vec::new();
        while losses.len() < 2 && start.elapsed() < Duration::from_secs(10) {
            let now = Instant::now();
            slot.tick(now, &mut |e| match e {
                IoEvent::SignalLost { attempts, .. } => losses.push((now, attempts)),
            });
            std::thread::sleep(Duration::from_millis(5));
        }
        // Pictures flowed again between the two losses: the attempt count started over.
        assert_eq!(losses.iter().map(|(_, a)| *a).collect::<Vec<_>>(), [1, 1]);
        // Each (re)start delivered pictures through a fresh feed.
        assert_eq!(feeds.count(), 2);
        let gap = losses[1].0 - losses[0].0;
        assert!(gap >= retry_delay(1), "retried after {gap:?}");
    }

    #[test]
    fn failing_start_counts_attempts_with_backoff() {
        init();
        let mut slot = PipelineSlot::new(
            InputId(5),
            live("nosuchelement"),
            FakeFeeds::default(),
            Instant::now(),
        );
        let t0 = Instant::now();
        let mut events = Vec::new();
        for offset in [0, 999, 1000, 2999, 3000, 7999, 8000, 17_999, 18_000] {
            slot.tick(t0 + Duration::from_millis(offset), &mut |e| {
                events.push((offset, e))
            });
        }
        let attempts: Vec<(u64, u32)> = events
            .iter()
            .map(|(o, e)| match e {
                IoEvent::SignalLost { attempts, .. } => (*o, *attempts),
            })
            .collect();
        assert_eq!(
            attempts,
            [(0, 1), (1000, 2), (3000, 3), (8000, 4), (18_000, 5)]
        );
    }

    /// GStreamer missing (initialisation simulated as failed): the runtime still starts,
    /// still images work, a file input is reported lost with the reason.
    #[test]
    fn without_gstreamer_images_still_work() {
        use vma_engine::{Command, EngineConfig, InputStatus, Journal, RenderEngine, Source};
        let gpu = pollster::block_on(vma_compositor::headless::request_gpu())
            .expect("GPU adapter required");
        let config = EngineConfig {
            resolution: vma_core::Resolution::new(320, 180).unwrap(),
            ..EngineConfig::default()
        };
        let (mut engine, _readers) =
            RenderEngine::spawn(gpu.device, gpu.queue, config, Journal::Disabled).unwrap();
        let io = IoRuntime::start_with(engine.take_io_port().unwrap(), || Err("simulated".into()))
            .unwrap();
        assert_eq!(io.gstreamer_error(), Some("simulated"));

        let png = std::env::temp_dir().join(format!("vma-io-nogst-{}.png", std::process::id()));
        image::RgbaImage::from_pixel(4, 4, image::Rgba([1, 2, 3, 255]))
            .save(&png)
            .unwrap();
        let commands = engine.commands();
        for spec in [
            InputSpec::Image { path: png.clone() },
            InputSpec::File {
                path: "clip.mkv".into(),
            },
        ] {
            commands
                .send(Source::Script, Command::AddInput { spec, name: None })
                .unwrap();
        }
        let state = engine.state();
        let end = Instant::now() + Duration::from_secs(5);
        let done = |s: &vma_engine::State| {
            s.inputs.len() == 2
                && matches!(s.inputs[0].status, InputStatus::Live { source: Some(_) })
                && matches!(s.inputs[1].status, InputStatus::SignalLost { .. })
        };
        while !done(&state.load()) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(done(&state.load()), "{:?}", state.load().inputs);
        let _ = std::fs::remove_file(png);
    }
}
