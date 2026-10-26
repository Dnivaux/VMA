use crate::stats::RenderStats;
use crate::triple::{Consumer, Producer, TripleIndices, triple};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use vma_clock::{ClockStats, FrameClock, SystemTime};
use vma_compositor::{RenderTarget, TestPattern};
use vma_core::{FrameRate, Resolution};

/// Maximum number of Program frames queued on the GPU. Beyond that the tick is skipped
/// (previous frame repeated, `gpu_dropped` incremented) instead of blocking the clock.
const MAX_GPU_IN_FLIGHT: u64 = 2;

/// Errors of the engine crate.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("could not spawn the render thread: {0}")]
    Spawn(#[from] std::io::Error),
}

/// Engine settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineConfig {
    pub rate: FrameRate,
    pub resolution: Resolution,
    /// Busy-wait margin of the master clock before each deadline (see `vma_clock::SystemTime`).
    pub spin_margin: Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            rate: FrameRate::DEFAULT,
            resolution: Resolution::HD_1080,
            spin_margin: SystemTime::DEFAULT_SPIN_MARGIN,
        }
    }
}

/// Program frames shared between the render thread (producer) and one reader.
#[derive(Debug)]
struct ProgramFrames {
    targets: [RenderTarget; 3],
    /// Frame index rendered in each slot.
    frame_index: [AtomicU64; 3],
    indices: TripleIndices,
}

/// Handle on the running render thread. Dropping it stops and joins the thread.
#[derive(Debug)]
pub struct RenderEngine {
    config: EngineConfig,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    clock_stats: Arc<ClockStats>,
    render_stats: Arc<RenderStats>,
}

/// The single consumer of Program frames (typically the UI).
#[derive(Debug)]
pub struct ProgramReader {
    frames: Arc<ProgramFrames>,
    consumer: Consumer,
}

impl RenderEngine {
    /// Starts the render thread on an existing device (shared with the UI, or headless).
    ///
    /// Takes ownership of the device's uncaptured-error handler and device-lost callback:
    /// GPU errors are logged and counted (never a panic), and a lost device stops rendering
    /// without stopping the clock (see [`RenderStatsSnapshot::device_lost`]).
    ///
    /// [`RenderStatsSnapshot::device_lost`]: crate::RenderStatsSnapshot::device_lost
    pub fn spawn(
        device: wgpu::Device,
        queue: wgpu::Queue,
        config: EngineConfig,
    ) -> Result<(Self, ProgramReader), EngineError> {
        let (indices, producer, consumer) = triple();
        let frames = Arc::new(ProgramFrames {
            targets: [0, 1, 2]
                .map(|i| RenderTarget::new(&device, config.resolution, &format!("program[{i}]"))),
            frame_index: Default::default(),
            indices,
        });
        let pattern = TestPattern::new(&device);
        let clock = FrameClock::with_spin_margin(config.rate, config.spin_margin);
        let clock_stats = clock.stats();
        let render_stats = Arc::new(RenderStats::default());
        install_gpu_handlers(&device, &render_stats);
        let stop = Arc::new(AtomicBool::new(false));

        let worker = RenderWorker {
            device,
            queue,
            clock,
            pattern,
            frames: Arc::clone(&frames),
            producer,
            stats: Arc::clone(&render_stats),
            stop: Arc::clone(&stop),
        };
        let thread = std::thread::Builder::new()
            .name("vma-render".into())
            .spawn(move || worker.run())?;

        tracing::info!(rate = %config.rate, resolution = %config.resolution, "render thread started");
        Ok((
            Self {
                config,
                stop,
                thread: Some(thread),
                clock_stats,
                render_stats,
            },
            ProgramReader { frames, consumer },
        ))
    }

    pub fn config(&self) -> EngineConfig {
        self.config
    }

    pub fn clock_stats(&self) -> Arc<ClockStats> {
        Arc::clone(&self.clock_stats)
    }

    pub fn render_stats(&self) -> Arc<RenderStats> {
        Arc::clone(&self.render_stats)
    }

    /// Stops the render thread and waits for it. Also done on drop.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("render thread panicked");
        }
    }
}

impl Drop for RenderEngine {
    fn drop(&mut self) {
        self.stop();
    }
}

impl ProgramReader {
    /// Switches to the most recent published frame, if any.
    ///
    /// **Invariant** (see `triple.rs`): call it once per consumer frame, *before* recording
    /// any GPU work that samples [`Self::current_slot`], and only once all the work of the
    /// previous frame that sampled the previous slot has been submitted to the shared queue.
    /// The engine may start overwriting the previous slot as soon as this returns.
    pub fn update(&mut self) -> bool {
        self.consumer.update(&self.frames.indices)
    }

    /// Slot (0..3) of the frame currently held by the reader.
    pub fn current_slot(&self) -> usize {
        self.consumer.front()
    }

    /// Frame index of the frame currently held by the reader.
    pub fn current_frame_index(&self) -> u64 {
        self.frames
            .frame_index
            .get(self.current_slot())
            .map_or(0, |f| f.load(Ordering::Acquire))
    }

    /// The three Program targets, to register them once with the UI renderer.
    pub fn targets(&self) -> &[RenderTarget; 3] {
        &self.frames.targets
    }
}

fn install_gpu_handlers(device: &wgpu::Device, stats: &Arc<RenderStats>) {
    // The default wgpu handler panics: replace it (rule 2.4).
    let on_error = Arc::clone(stats);
    device.on_uncaptured_error(Arc::new(move |error| {
        on_error.record_gpu_error();
        tracing::error!(%error, "uncaptured GPU error");
    }));
    let on_lost = Arc::clone(stats);
    device.set_device_lost_callback(move |reason, message| {
        tracing::error!(?reason, %message, "GPU device lost: rendering stopped");
        on_lost.set_device_lost(format!("{reason:?}: {message}"));
    });
}

struct RenderWorker {
    device: wgpu::Device,
    queue: wgpu::Queue,
    clock: FrameClock,
    pattern: TestPattern,
    frames: Arc<ProgramFrames>,
    producer: Producer,
    stats: Arc<RenderStats>,
    stop: Arc<AtomicBool>,
}

impl RenderWorker {
    fn run(mut self) {
        let fps_window = u64::from(
            self.clock
                .rate()
                .num()
                .div_ceil(self.clock.rate().den())
                .max(1),
        );
        let mut window_start = Instant::now();
        let mut window_ticks = 0u64;

        while !self.stop.load(Ordering::Acquire) {
            let tick = self.clock.wait_next();
            let started = Instant::now();

            // A lost device accepts no more work: keep the clock (and the stats) running,
            // render nothing. Recovery is the application's job (ADR 0001, « Perte du GPU »).
            if self.stats.is_device_lost() {
                self.stats.record_device_lost_tick();
                continue;
            }

            window_ticks += 1 + tick.missed_before;
            if window_ticks >= fps_window {
                let elapsed = window_start.elapsed().as_secs_f64();
                if elapsed > 0.0 {
                    self.stats.set_measured_fps(window_ticks as f64 / elapsed);
                }
                window_start = started;
                window_ticks = 0;
            }

            // Non-blocking: fires completion callbacks of earlier submissions.
            if let Err(e) = self.device.poll(wgpu::PollType::Poll) {
                tracing::error!(error = %e, "GPU poll failed");
            }
            if self.stats.gpu_in_flight() >= MAX_GPU_IN_FLIGHT {
                self.stats.record_gpu_drop();
                tracing::warn!(index = tick.index, "GPU backpressure: frame repeated");
                continue;
            }

            self.render(tick.index);
            self.stats.record_frame(started.elapsed());
        }
        tracing::info!("render thread stopped");
    }

    fn render(&mut self, frame_index: u64) {
        let slot = self.producer.back();
        let (Some(target), Some(slot_index)) = (
            self.frames.targets.get(slot),
            self.frames.frame_index.get(slot),
        ) else {
            return;
        };
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("program"),
            });
        self.pattern
            .encode(&self.queue, &mut encoder, target.view(), frame_index);

        self.stats.gpu_submitted();
        self.queue.submit([encoder.finish()]);
        let stats = Arc::clone(&self.stats);
        self.queue
            .on_submitted_work_done(move || stats.gpu_completed());

        slot_index.store(frame_index, Ordering::Release);
        self.producer.publish(&self.frames.indices);
    }
}
