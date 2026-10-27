use crate::command::MixDomain;
use crate::control::{
    self, CommandSender, ControlInputs, ControlStats, Controller, InputStatsReader, IoEventSender,
    IoRequests, StateReader, command_channel, io_channels,
};
use crate::feed::{ATTACH_CAPACITY, FeedRegistry};
use crate::journal::{self, Journal, JournalStats};
use crate::plan::plan_channel;
use crate::sequencer::{Composition, RenderFeedback, Sequencer};
use crate::sources::{RenderSource, SOURCE_CAPACITY, SourceTable};
use crate::state::{Format, State};
use crate::stats::RenderStats;
use crate::triple::{Consumer, Producer, TripleIndices, triple};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use vma_clock::{ClockStats, FrameClock, SystemTime};
use vma_compositor::thumbnails::{self, ATLAS_CELLS, ThumbnailPainter};
use vma_compositor::{FramePipelines, Mix, MixSpace, Mixer, RenderTarget, Source};
use vma_core::{FrameRate, InputId, Resolution};

/// Maximum number of Program frames queued on the GPU. Beyond that the tick is skipped
/// (previous frame repeated, `gpu_dropped` incremented) instead of blocking the clock.
const MAX_GPU_IN_FLIGHT: u64 = 2;

/// Errors of the engine crate.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("could not spawn an engine thread: {0}")]
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

/// Frames of one output (Program, Preview, thumbnail atlas) shared between the render
/// thread (producer) and one reader.
#[derive(Debug)]
struct OutputFrames {
    targets: [RenderTarget; 3],
    /// Frame index rendered in each slot.
    frame_index: [AtomicU64; 3],
    /// Thumbnail atlas only: input shown in each cell of each slot (`id + 1`, 0 = none).
    cells: [[AtomicU32; ATLAS_CELLS]; 3],
    indices: TripleIndices,
}

impl OutputFrames {
    fn new(targets: [RenderTarget; 3], indices: TripleIndices) -> Self {
        Self {
            targets,
            frame_index: Default::default(),
            cells: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU32::new(0))),
            indices,
        }
    }
}

/// Producer side of an output.
struct Output {
    frames: Arc<OutputFrames>,
    producer: Producer,
}

fn output(targets: [RenderTarget; 3]) -> (Output, FrameReader) {
    let (indices, producer, consumer) = triple();
    let frames = Arc::new(OutputFrames::new(targets, indices));
    (
        Output {
            frames: Arc::clone(&frames),
            producer,
        },
        FrameReader { frames, consumer },
    )
}

/// The engine's outputs for the UI. Each reader is the single consumer of its frames.
#[derive(Debug)]
pub struct Readers {
    pub program: FrameReader,
    pub preview: FrameReader,
    /// Thumbnail atlas, refreshed every other tick (see `vma_compositor::thumbnails`).
    pub thumbnails: FrameReader,
}

/// Handle on the running engine: the `vma-render` and `vma-control` threads (and
/// `vma-journal` if enabled). Dropping it stops and joins them.
#[derive(Debug)]
pub struct RenderEngine {
    config: EngineConfig,
    stop: Arc<AtomicBool>,
    /// Render first, then control, then journal (it ends when control drops its sender).
    threads: Vec<JoinHandle<()>>,
    clock_stats: Arc<ClockStats>,
    render_stats: Arc<RenderStats>,
    control_stats: Arc<ControlStats>,
    journal_stats: Arc<JournalStats>,
    feedback: Arc<RenderFeedback>,
    commands: CommandSender,
    state: StateReader,
    feeds: FeedRegistry,
    input_stats: InputStatsReader,
    io_port: Option<IoPort>,
}

/// What `vma-io` needs from the engine: the requests of `vma-control` (open, close, play…),
/// a way to report events, and the feed registry. Taken once ([`RenderEngine::take_io_port`]).
#[derive(Debug)]
pub struct IoPort {
    pub requests: IoRequests,
    pub events: IoEventSender,
    pub feeds: FeedRegistry,
}

/// The single consumer of one output's frames (typically the UI).
#[derive(Debug)]
pub struct FrameReader {
    frames: Arc<OutputFrames>,
    consumer: Consumer,
}

impl RenderEngine {
    /// Starts the engine on an existing device (shared with the UI, or headless): the
    /// render thread, the control thread that owns the state, and the journal writer.
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
        journal: Journal,
    ) -> Result<(Self, Readers), EngineError> {
        let targets = |name: &str| {
            [0, 1, 2]
                .map(|i| RenderTarget::new(&device, config.resolution, &format!("{name}[{i}]")))
        };
        let (program, program_reader) = output(targets("program"));
        let (preview, preview_reader) = output(targets("preview"));
        let (thumbs, thumbs_reader) =
            output([0, 1, 2].map(|i| thumbnails::new_atlas(&device, &format!("thumbnails[{i}]"))));
        let mixers = [
            Mixer::new(&device, config.resolution),
            Mixer::new(&device, config.resolution),
        ];
        let painter = ThumbnailPainter::new(&device);
        let clock = FrameClock::with_spin_margin(config.rate, config.spin_margin);
        let clock_stats = clock.stats();
        let render_stats = Arc::new(RenderStats::default());
        install_gpu_handlers(&device, &render_stats);
        let stop = Arc::new(AtomicBool::new(false));
        let feedback = Arc::new(RenderFeedback::default());
        let control_stats = Arc::new(ControlStats::default());
        let journal_stats = Arc::new(JournalStats::default());

        let mut journal_thread = None;
        let journal_sender = match journal {
            Journal::Disabled => None,
            Journal::Directory(dir) => {
                let (sender, thread) = journal::spawn_writer(dir, Arc::clone(&journal_stats))?;
                journal_thread = Some(thread);
                Some(sender)
            }
        };
        let (plan_writer, plan_reader) = plan_channel();
        let (sources_tx, sources_rx) = crossbeam_channel::bounded(SOURCE_CAPACITY);
        let (retired_tx, retired_rx) = crossbeam_channel::bounded(SOURCE_CAPACITY);
        let (attach_tx, attach_rx) = crossbeam_channel::bounded(ATTACH_CAPACITY);
        let feeds = FeedRegistry::new(
            device.clone(),
            queue.clone(),
            FramePipelines::new(&device),
            attach_tx,
        );
        let ((io_tx, io_requests), (io_events, io_events_rx)) = io_channels();
        let (mut controller, state) = Controller::new(
            State::new(Format {
                rate: config.rate,
                resolution: config.resolution,
            }),
            plan_writer,
            Arc::clone(&feedback),
            journal_sender,
            Arc::clone(&control_stats),
            sources_tx,
        );
        controller.set_io(io_tx);
        let input_stats = controller.input_stats();
        let (commands, command_rx) = command_channel();
        let control_thread = control::spawn(
            controller,
            ControlInputs {
                commands: command_rx,
                attach: attach_rx,
                retired: retired_rx,
                io_events: io_events_rx,
            },
            Arc::clone(&stop),
        )?;

        let worker = RenderWorker {
            device,
            queue,
            clock,
            mixers,
            painter,
            program,
            preview,
            thumbs,
            sequencer: Sequencer::new(plan_reader),
            sources: SourceTable::new(sources_rx, retired_tx),
            feedback: Arc::clone(&feedback),
            stats: Arc::clone(&render_stats),
            stop: Arc::clone(&stop),
        };
        let render_thread = match std::thread::Builder::new()
            .name("vma-render".into())
            .spawn(move || worker.run())
        {
            Ok(thread) => thread,
            Err(error) => {
                stop.store(true, Ordering::Release);
                for thread in [Some(control_thread), journal_thread].into_iter().flatten() {
                    let _ = thread.join();
                }
                return Err(error.into());
            }
        };

        tracing::info!(rate = %config.rate, resolution = %config.resolution, "engine started");
        Ok((
            Self {
                config,
                stop,
                threads: [Some(render_thread), Some(control_thread), journal_thread]
                    .into_iter()
                    .flatten()
                    .collect(),
                clock_stats,
                render_stats,
                control_stats,
                journal_stats,
                feedback,
                commands,
                state,
                feeds: feeds.clone(),
                input_stats,
                io_port: Some(IoPort {
                    requests: io_requests,
                    events: io_events,
                    feeds,
                }),
            },
            Readers {
                program: program_reader,
                preview: preview_reader,
                thumbnails: thumbs_reader,
            },
        ))
    }

    /// Sends commands to the engine (clonable).
    pub fn commands(&self) -> CommandSender {
        self.commands.clone()
    }

    /// Reads the latest state snapshot (clonable).
    pub fn state(&self) -> StateReader {
        self.state.clone()
    }

    /// Creates picture feeds for inputs fed from outside the engine (clonable).
    pub fn feeds(&self) -> FeedRegistry {
        self.feeds.clone()
    }

    /// The link for `vma-io` (`io-gst`); `None` once taken. Without it, the requests of
    /// `vma-control` are dropped (logged) once the channel is full.
    pub fn take_io_port(&mut self) -> Option<IoPort> {
        self.io_port.take()
    }

    /// Per-input statistics of the picture feeds (clonable).
    pub fn input_stats(&self) -> InputStatsReader {
        self.input_stats.clone()
    }

    pub fn control_stats(&self) -> Arc<ControlStats> {
        Arc::clone(&self.control_stats)
    }

    pub fn journal_stats(&self) -> Arc<JournalStats> {
        Arc::clone(&self.journal_stats)
    }

    /// What the render thread did last (tick, plan version, what is on air).
    pub fn feedback(&self) -> Arc<RenderFeedback> {
        Arc::clone(&self.feedback)
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

    /// Stops the engine threads and waits for them. Also done on drop.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        for thread in self.threads.drain(..) {
            let name = thread.thread().name().unwrap_or("engine").to_owned();
            if thread.join().is_err() {
                tracing::error!(thread = %name, "engine thread panicked");
            }
        }
    }
}

impl Drop for RenderEngine {
    fn drop(&mut self) {
        self.stop();
    }
}

impl FrameReader {
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

    /// The three targets, to register them once with the UI renderer.
    pub fn targets(&self) -> &[RenderTarget; 3] {
        &self.frames.targets
    }

    /// Thumbnail atlas: the cell showing `input` in the frame currently held, if any.
    pub fn cell_of(&self, input: InputId) -> Option<usize> {
        let wanted = input.0.checked_add(1)?;
        self.frames
            .cells
            .get(self.current_slot())?
            .iter()
            .position(|c| c.load(Ordering::Acquire) == wanted)
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

fn render_source_to_compositor(source: &RenderSource, frame: u64) -> Source<'_> {
    match source {
        RenderSource::Color(c) => Source::Color(*c),
        RenderSource::TestPattern => Source::TestPattern { frame },
        RenderSource::Feed(reader) => reader.current().map_or(Source::Black, Source::Frame),
    }
}

/// Publishes the back slot of `out` as frame `frame`.
fn publish(out: &mut Output, frame: u64) {
    if let Some(index) = out.frames.frame_index.get(out.producer.back()) {
        index.store(frame, Ordering::Release);
    }
    out.producer.publish(&out.frames.indices);
}

struct RenderWorker {
    device: wgpu::Device,
    queue: wgpu::Queue,
    clock: FrameClock,
    /// Program, Preview.
    mixers: [Mixer; 2],
    painter: ThumbnailPainter,
    program: Output,
    preview: Output,
    thumbs: Output,
    sequencer: Sequencer,
    sources: SourceTable,
    feedback: Arc<RenderFeedback>,
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
            // Resolved on every tick, even one that renders nothing, so transitions keep
            // their timing. Plan first, then the sources it may reference (see `sources.rs`).
            let composition = self.sequencer.tick(tick.index, &self.feedback);
            self.sources.drain();
            // Every feed takes its latest picture once per tick, rendered or not, so the
            // per-input statistics count master ticks.
            self.sources.advance(started);

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

            self.render(&composition);
            self.stats.record_frame(started.elapsed());
        }
        tracing::info!("render thread stopped");
    }

    fn source(&self, input: Option<InputId>, frame: u64) -> Source<'_> {
        match input.and_then(|id| self.sources.get(id)) {
            Some(source) => render_source_to_compositor(source, frame),
            // No input, or one without pictures yet: black.
            None => Source::Black,
        }
    }

    /// Renders Program, Preview and (every other tick) the thumbnails in one submission,
    /// then publishes them.
    fn render(&mut self, composition: &Composition) {
        let frame = composition.frame;
        let program = Mix {
            from: self.source(composition.from, frame),
            to: self.source(composition.to, frame),
            mix: composition.mix,
            space: match composition.domain {
                MixDomain::Gamma => MixSpace::Gamma,
                MixDomain::Linear => MixSpace::Linear,
            },
        };
        let preview = Mix::single(self.source(composition.preview, frame));
        let with_thumbnails = frame.is_multiple_of(2);

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tick"),
            });
        for ((mixer, out), mix) in self
            .mixers
            .iter()
            .zip([&self.program, &self.preview])
            .zip([program, preview])
        {
            if let Some(target) = out.frames.targets.get(out.producer.back()) {
                mixer.encode(&self.queue, &mut encoder, &mix, target.view());
            }
        }
        let thumbs_slot = self.thumbs.producer.back();
        if with_thumbnails
            && let (Some(atlas), Some(cells)) = (
                self.thumbs.frames.targets.get(thumbs_slot),
                self.thumbs.frames.cells.get(thumbs_slot),
            )
        {
            let sources = self
                .sources
                .iter()
                .map(|(slot, _, source)| (slot, render_source_to_compositor(source, frame)));
            self.painter
                .encode(&self.queue, &mut encoder, atlas.view(), frame, sources);
            // Visible to the reader through the publish below (Release / Acquire).
            for value in cells {
                value.store(0, Ordering::Relaxed);
            }
            for (slot, id, _) in self.sources.iter() {
                if let Some(value) = cells.get(slot) {
                    value.store(id.0.saturating_add(1), Ordering::Relaxed);
                }
            }
        }

        self.stats.gpu_submitted();
        self.queue.submit([encoder.finish()]);
        let stats = Arc::clone(&self.stats);
        self.queue
            .on_submitted_work_done(move || stats.gpu_completed());

        publish(&mut self.program, frame);
        publish(&mut self.preview, frame);
        if with_thumbnails {
            publish(&mut self.thumbs, frame);
        }
    }
}
