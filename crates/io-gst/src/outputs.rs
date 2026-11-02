//! Outputs on `vma-io` (Phase 2 steps 5–6): starts, watches and stops one pipeline per output
//! on the requests of `vma-control`, reports each run back to it, and publishes what the
//! outputs are doing (bytes written or sent, bit rate, reconnections) for the statistics.
//!
//! - Stopping never blocks the thread: the feed is detached from the engine's hub (its end of
//!   stream follows), then the pipeline is polled until the file is finalised.
//! - A stream is running once media bytes go out. A lost connection (error, nothing sent for
//!   [`STALL_TIMEOUT`], no connection within [`CONNECT_TIMEOUT`]) destroys its pipeline and
//!   builds a new one after 1, 2, 5, then every 10 s, without end (ADR 0006 § 4, same policy as
//!   the inputs); the recordings are not touched. A recording that fails stays failed.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use gstreamer::glib;
use vma_core::OutputId;
use vma_engine::{IoEvent, OutputHub, OutputSpec, RecordSpec, RtmpSpec, SrtMode, SrtSpec};

use crate::output::{
    GstOutput, OutputFeed, OutputFormat, RecordSettings, SrtSettings, StreamSettings,
};
use crate::supervisor::retry_delay;

/// How long a stopping output may take to finalise its file before it is abandoned.
pub(crate) const STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// Window of the bit-rate measurement.
const RATE_WINDOW: Duration = Duration::from_secs(1);
/// Most attempts at a free file name (`VMA … (2).mkv`, …).
const NAME_ATTEMPTS: u32 = 100;
/// Bytes sent before a stream counts as connected (past the RTMP commands: media flows).
const CONNECTED_BYTES: u64 = 16 * 1024;
/// A stream not connected after this long is lost.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A connected stream that sends nothing for this long is lost.
pub(crate) const STALL_TIMEOUT: Duration = Duration::from_secs(5);

/// What a running output is doing, published 4 times a second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputActivity {
    /// When its pipeline started.
    pub since: Instant,
    /// Bytes written (file) or sent (stream, current connection).
    pub bytes: Option<u64>,
    /// Over the last second, kbit/s.
    pub kbps: Option<u32>,
    /// Streams: connections lost since the start.
    pub reconnections: u32,
}

/// Activity of every running output.
pub(crate) type Activities = Arc<ArcSwap<Vec<(OutputId, OutputActivity)>>>;

/// The folder recordings go to by default: the user's Videos folder (Linux: XDG
/// `VIDEOS`; Windows: the Videos known folder), else the home folder.
pub fn default_record_folder() -> PathBuf {
    glib::user_special_dir(glib::UserDirectory::Videos).unwrap_or_else(glib::home_dir)
}

/// A free file name in `folder`: `VMA AAAA-MM-JJ HH-MM-SS.ext` (local time), with ` (2)`… if
/// it is taken.
pub(crate) fn record_path(folder: &Path, stamp: &str, extension: &str) -> Option<PathBuf> {
    (1..=NAME_ATTEMPTS)
        .map(|n| match n {
            1 => folder.join(format!("VMA {stamp}.{extension}")),
            n => folder.join(format!("VMA {stamp} ({n}).{extension}")),
        })
        .find(|path| !path.exists())
}

/// Local date and time as file names carry it.
fn local_stamp() -> String {
    glib::DateTime::now_local()
        .and_then(|now| now.format("%Y-%m-%d %H-%M-%S"))
        .map(|s| s.to_string())
        .unwrap_or_else(|_| "sans date".to_owned())
}

/// What an output's slot is doing.
#[derive(Debug)]
enum Phase {
    /// Pipeline running since `started`; a stream is `connected` once media goes out.
    Running {
        pipeline: GstOutput,
        started: Instant,
        connected: bool,
    },
    /// A stream between two connections: next attempt at `at`.
    Waiting { at: Instant },
    /// Finalising since `since`.
    Stopping { pipeline: GstOutput, since: Instant },
}

/// How to build the pipeline of an output again.
#[derive(Debug, Clone)]
enum Recipe {
    Record,
    Stream(Target),
}

/// Where a stream goes.
#[derive(Debug, Clone)]
enum Target {
    Rtmp(StreamSettings),
    Srt(SrtSettings),
}

impl Target {
    fn start(&self, format: OutputFormat) -> Result<(GstOutput, OutputFeed), crate::IoError> {
        match self {
            Self::Rtmp(settings) => GstOutput::stream(format, settings),
            Self::Srt(settings) => GstOutput::srt(format, settings),
        }
    }

    /// Where it goes, as shown (no secret).
    fn shown(&self) -> String {
        match self {
            Self::Rtmp(settings) => settings.url.clone(),
            Self::Srt(settings) => match settings.mode {
                SrtMode::Caller => settings.uri(),
                SrtMode::Listener => format!("{} (listening)", settings.uri()),
            },
        }
    }

    /// An SRT listener waits for its receivers: on air as soon as it listens, never « lost »
    /// for want of one.
    fn listens(&self) -> bool {
        matches!(self, Self::Srt(s) if s.mode == SrtMode::Listener)
    }

    fn kbps(&self) -> u32 {
        match self {
            Self::Rtmp(settings) => settings.video_kbps,
            Self::Srt(settings) => settings.video_kbps,
        }
    }
}

/// One output of `vma-io`.
#[derive(Debug)]
struct Slot {
    id: OutputId,
    run: u64,
    recipe: Recipe,
    phase: Phase,
    /// Start of the run.
    since: Instant,
    /// Consecutive lost connections (reset once connected again).
    attempt: u32,
    reconnections: u32,
    encoder: String,
    target: Option<String>,
    /// Start of the bit-rate window and the bytes then.
    window: (Instant, u64),
    kbps: Option<u32>,
    /// Last time the bytes sent changed, and their count (stall watchdog).
    moved: (Instant, u64),
}

impl Slot {
    fn pipeline(&self) -> Option<&GstOutput> {
        match &self.phase {
            Phase::Running { pipeline, .. } | Phase::Stopping { pipeline, .. } => Some(pipeline),
            Phase::Waiting { .. } => None,
        }
    }
}

/// Every output pipeline of `vma-io`.
#[derive(Debug)]
pub(crate) struct Outputs {
    hub: OutputHub,
    format: OutputFormat,
    /// GStreamer could not be initialised: every start fails with this reason.
    unavailable: Option<String>,
    slots: Vec<Slot>,
    activities: Activities,
}

impl Outputs {
    pub(crate) fn new(
        hub: OutputHub,
        format: OutputFormat,
        unavailable: Option<String>,
        activities: Activities,
    ) -> Self {
        Self {
            hub,
            format,
            unavailable,
            slots: Vec::new(),
            activities,
        }
    }

    /// Builds and starts run `run` of `output`. A recording reports `OutputRunning` at once, a
    /// stream once it sends media; `OutputFailed` if the pipeline cannot be built.
    pub(crate) fn start(
        &mut self,
        output: OutputId,
        run: u64,
        spec: &OutputSpec,
        emit: &mut impl FnMut(IoEvent),
    ) {
        if self.slots.iter().any(|s| s.id == output) {
            // Never asked by `vma-control` (it stops first): the old run is finalised.
            tracing::warn!(%output, "output started twice: the previous run is stopped");
            self.stop(output, &mut |_| {});
        }
        let recipe = match spec {
            _ if self.unavailable.is_some() => Err(format!(
                "GStreamer is not available: {}",
                self.unavailable.as_deref().unwrap_or_default()
            )),
            OutputSpec::Record(record) => self.record_settings(record).map(|_| Recipe::Record),
            OutputSpec::Rtmp(rtmp) => self
                .stream_settings(rtmp)
                .map(|s| Recipe::Stream(Target::Rtmp(s))),
            OutputSpec::Srt(srt) => self
                .srt_settings(srt)
                .map(|s| Recipe::Stream(Target::Srt(s))),
            // Shown by the UI: `vma-control` never asks for it.
            OutputSpec::Fullscreen(_) => Err("the full screen has no pipeline".to_owned()),
        };
        let started = recipe.and_then(|recipe| {
            let (pipeline, target) = match (&recipe, spec) {
                (Recipe::Record, OutputSpec::Record(record)) => {
                    let settings = self.record_settings(record)?;
                    let target = settings.path.display().to_string();
                    (self.attach_record(output, &settings)?, target)
                }
                (Recipe::Stream(target), _) => {
                    tracing::info!(%output, to = %target.shown(), kbps = target.kbps(), "starting a stream");
                    (self.attach_stream(output, target)?, target.shown())
                }
                _ => return Err("inconsistent output settings".to_owned()),
            };
            Ok((recipe, pipeline, target))
        });
        match started {
            Ok((recipe, pipeline, target)) => {
                let now = Instant::now();
                let encoder = describe(&pipeline);
                // On air at once: a recording, an SRT listener. A stream that calls waits for
                // its media to go out.
                let record = match &recipe {
                    Recipe::Record => true,
                    Recipe::Stream(target) => target.listens(),
                };
                self.slots.push(Slot {
                    id: output,
                    run,
                    recipe,
                    phase: Phase::Running {
                        pipeline,
                        started: now,
                        connected: record,
                    },
                    since: now,
                    attempt: 0,
                    reconnections: 0,
                    encoder: encoder.clone(),
                    target: Some(target.clone()),
                    window: (now, 0),
                    kbps: None,
                    moved: (now, 0),
                });
                if record {
                    emit(IoEvent::OutputRunning {
                        output,
                        run,
                        encoder,
                        target: Some(target),
                    });
                }
            }
            Err(reason) => {
                tracing::error!(%output, %reason, "output not started");
                emit(IoEvent::OutputFailed {
                    output,
                    run,
                    reason,
                });
            }
        }
        self.publish();
    }

    /// Settings of a recording, with a fresh file name.
    fn record_settings(&self, spec: &RecordSpec) -> Result<RecordSettings, String> {
        let folder = match &spec.folder {
            Some(folder) if !folder.is_dir() => {
                return Err(format!("folder {} does not exist", folder.display()));
            }
            Some(folder) => folder.clone(),
            None => {
                let folder = default_record_folder();
                std::fs::create_dir_all(&folder)
                    .map_err(|e| format!("cannot create {}: {e}", folder.display()))?;
                folder
            }
        };
        let path = record_path(&folder, &local_stamp(), spec.container.extension())
            .ok_or_else(|| format!("no free file name in {}", folder.display()))?;
        let mut settings = RecordSettings::new(path, self.format);
        settings.codec = spec.codec;
        settings.container = spec.container;
        if let Some(kbps) = spec.video_kbps {
            settings.video_kbps = kbps;
        }
        Ok(settings)
    }

    fn stream_settings(&self, spec: &RtmpSpec) -> Result<StreamSettings, String> {
        let key = spec
            .key
            .expose()
            .ok_or("the stream key is missing")?
            .to_owned();
        let mut settings = StreamSettings::new(spec.url.trim().to_owned(), key, self.format);
        if let Some(kbps) = spec.video_kbps {
            settings.video_kbps = kbps;
        }
        Ok(settings)
    }

    fn srt_settings(&self, spec: &SrtSpec) -> Result<SrtSettings, String> {
        let passphrase = spec
            .passphrase
            .expose()
            .ok_or("the SRT passphrase is missing")?
            .to_owned();
        let mut settings = SrtSettings::new(
            spec.mode,
            spec.host.trim().to_owned(),
            spec.port,
            spec.latency_ms,
            passphrase,
            self.format,
        );
        if let Some(kbps) = spec.video_kbps {
            settings.video_kbps = kbps;
        }
        Ok(settings)
    }

    fn attach_record(
        &self,
        output: OutputId,
        settings: &RecordSettings,
    ) -> Result<GstOutput, String> {
        tracing::info!(%output, path = %settings.path.display(), ?settings.codec, ?settings.container, kbps = settings.video_kbps, "starting a recording");
        let (pipeline, feed) =
            GstOutput::record(self.format, settings).map_err(|e| e.to_string())?;
        self.hub
            .attach(output, Box::new(feed))
            .map_err(|e| e.to_string())?;
        Ok(pipeline)
    }

    fn attach_stream(&self, output: OutputId, target: &Target) -> Result<GstOutput, String> {
        let (pipeline, feed) = target.start(self.format).map_err(|e| e.to_string())?;
        self.hub
            .attach(output, Box::new(feed))
            .map_err(|e| e.to_string())?;
        Ok(pipeline)
    }

    /// Detaches `output` from the engine (its end of stream follows); [`Self::tick`] then
    /// waits for the file to be finalised. A stream waiting to reconnect stops at once.
    pub(crate) fn stop(&mut self, output: OutputId, emit: &mut impl FnMut(IoEvent)) {
        self.hub.detach(output);
        let now = Instant::now();
        let Some(index) = self.slots.iter().position(|s| s.id == output) else {
            tracing::debug!(%output, "stop of an output without pipeline");
            return;
        };
        let slot = self.slots.remove(index);
        let Slot { phase, run, .. } = slot;
        let phase = match phase {
            Phase::Running { pipeline, .. } => Phase::Stopping {
                pipeline,
                since: now,
            },
            Phase::Waiting { .. } => {
                emit(IoEvent::OutputStopped { output, run });
                self.publish();
                return;
            }
            stopping @ Phase::Stopping { .. } => stopping,
        };
        self.slots.insert(index, Slot { phase, ..slot });
        // A stopping output has no activity to show any more.
        self.publish();
    }

    /// Watches the pipelines: failures, connections, finalised files, bit rates.
    pub(crate) fn tick(&mut self, now: Instant, emit: &mut impl FnMut(IoEvent)) {
        let before: Vec<_> = self.slots.iter().map(|s| (s.id, s.run)).collect();
        let slots = std::mem::take(&mut self.slots);
        for slot in slots {
            if let Some(slot) = self.advance(slot, now, emit) {
                self.slots.push(slot);
            }
        }
        for slot in &mut self.slots {
            let bytes = slot.pipeline().and_then(GstOutput::bytes).unwrap_or(0);
            let (start, at_start) = slot.window;
            let elapsed = now.saturating_duration_since(start);
            if elapsed >= RATE_WINDOW {
                // Bits per millisecond = kbit/s.
                let bits = bytes.saturating_sub(at_start).saturating_mul(8);
                let ms = u64::try_from(elapsed.as_millis())
                    .unwrap_or(u64::MAX)
                    .max(1);
                slot.kbps = u32::try_from(bits / ms).ok();
                slot.window = (now, bytes);
            }
        }
        let after: Vec<_> = self.slots.iter().map(|s| (s.id, s.run)).collect();
        if after != before {
            self.publish();
        }
    }

    /// One slot, one step; `None` once it is over (stopped or failed, reported).
    fn advance(
        &self,
        mut slot: Slot,
        now: Instant,
        emit: &mut impl FnMut(IoEvent),
    ) -> Option<Slot> {
        let (id, run) = (slot.id, slot.run);
        let stream = matches!(slot.recipe, Recipe::Stream(_));
        match &mut slot.phase {
            Phase::Stopping { pipeline, since } => {
                match pipeline.poll_stop(now.saturating_duration_since(*since), STOP_TIMEOUT) {
                    None => return Some(slot),
                    Some(Ok(())) => emit(IoEvent::OutputStopped { output: id, run }),
                    // A stream cut while stopping cannot be finalised: it is stopped anyway.
                    Some(Err(e)) if stream => {
                        tracing::warn!(output = %id, error = %e, "stream stopped without its end");
                        emit(IoEvent::OutputStopped { output: id, run });
                    }
                    Some(Err(e)) => emit(IoEvent::OutputFailed {
                        output: id,
                        run,
                        reason: e.to_string(),
                    }),
                }
                None
            }
            Phase::Waiting { at } if now < *at => Some(slot),
            Phase::Waiting { .. } => {
                let Recipe::Stream(target) = &slot.recipe else {
                    return None;
                };
                let listens = target.listens();
                match self.attach_stream(id, target) {
                    Ok(pipeline) => {
                        tracing::info!(output = %id, attempt = slot.attempt, "stream reconnecting");
                        slot.phase = Phase::Running {
                            pipeline,
                            started: now,
                            connected: listens,
                        };
                        if listens {
                            slot.attempt = 0;
                            emit(IoEvent::OutputRunning {
                                output: id,
                                run,
                                encoder: slot.encoder.clone(),
                                target: slot.target.clone(),
                            });
                        }
                        slot.moved = (now, 0);
                        slot.window = (now, 0);
                        Some(slot)
                    }
                    Err(reason) => Some(self.lost(slot, now, reason, emit)),
                }
            }
            Phase::Running {
                pipeline,
                started,
                connected,
            } => {
                let failed = pipeline.poll().map(str::to_owned);
                if let Some(reason) = failed {
                    if !stream {
                        self.hub.detach(id);
                        emit(IoEvent::OutputFailed {
                            output: id,
                            run,
                            reason,
                        });
                        return None;
                    }
                    return Some(self.lost(slot, now, reason, emit));
                }
                let listens = matches!(&slot.recipe, Recipe::Stream(t) if t.listens());
                if !stream || listens {
                    return Some(slot);
                }
                let bytes = pipeline.bytes().unwrap_or(0);
                if bytes != slot.moved.1 {
                    slot.moved = (now, bytes);
                }
                if !*connected {
                    if bytes >= CONNECTED_BYTES {
                        *connected = true;
                        slot.attempt = 0;
                        emit(IoEvent::OutputRunning {
                            output: id,
                            run,
                            encoder: slot.encoder.clone(),
                            target: slot.target.clone(),
                        });
                    } else if now.saturating_duration_since(*started) >= CONNECT_TIMEOUT {
                        let reason =
                            format!("no connection within {} s", CONNECT_TIMEOUT.as_secs());
                        return Some(self.lost(slot, now, reason, emit));
                    }
                } else if now.saturating_duration_since(slot.moved.0) >= STALL_TIMEOUT {
                    let reason = format!("nothing sent for {} s", STALL_TIMEOUT.as_secs());
                    return Some(self.lost(slot, now, reason, emit));
                }
                Some(slot)
            }
        }
    }

    /// A stream lost its connection: its pipeline goes, a new one comes after the backoff.
    fn lost(
        &self,
        mut slot: Slot,
        now: Instant,
        reason: String,
        emit: &mut impl FnMut(IoEvent),
    ) -> Slot {
        self.hub.detach(slot.id);
        slot.attempt = slot.attempt.saturating_add(1);
        slot.reconnections = slot.reconnections.saturating_add(1);
        // Dropping the pipeline stops it (state `NULL`).
        slot.phase = Phase::Waiting {
            at: now + retry_delay(slot.attempt),
        };
        slot.kbps = None;
        tracing::warn!(output = %slot.id, attempt = slot.attempt, %reason, "stream lost");
        emit(IoEvent::OutputReconnecting {
            output: slot.id,
            run: slot.run,
            attempt: slot.attempt,
            reason,
        });
        slot
    }

    /// Publishes the activity of the outputs that run (or wait to reconnect).
    pub(crate) fn publish(&self) {
        let all = self
            .slots
            .iter()
            .filter(|s| !matches!(s.phase, Phase::Stopping { .. }))
            .map(|s| {
                (
                    s.id,
                    OutputActivity {
                        since: s.since,
                        bytes: s.pipeline().and_then(GstOutput::bytes),
                        kbps: s.kbps,
                        reconnections: s.reconnections,
                    },
                )
            })
            .collect();
        self.activities.store(Arc::new(all));
    }

    /// Stops every output and waits (at most [`STOP_TIMEOUT`] in all) for their files: on
    /// `vma-io`'s way out, so that a recording running at exit is finalised.
    pub(crate) fn shutdown(&mut self) {
        let mut log = |event| tracing::info!(?event, "output at exit");
        let ids: Vec<_> = self.slots.iter().map(|s| s.id).collect();
        for id in ids {
            self.stop(id, &mut log);
        }
        let deadline = Instant::now() + STOP_TIMEOUT;
        while !self.slots.is_empty() && Instant::now() < deadline {
            self.tick(Instant::now(), &mut log);
            std::thread::sleep(Duration::from_millis(20));
        }
        if !self.slots.is_empty() {
            tracing::error!(left = self.slots.len(), "outputs not finalised at exit");
        }
        self.slots.clear();
        self.publish();
    }
}

/// « nvh264enc + avenc_aac », with the reason of a fallback.
fn describe(pipeline: &GstOutput) -> String {
    let name = |choice: Option<&crate::EncoderChoice>| {
        choice.map_or_else(|| "raw".to_owned(), ToString::to_string)
    };
    format!(
        "{} + {}",
        name(pipeline.video_encoder()),
        name(pipeline.audio_encoder())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IoRuntime;
    use crate::output::tests::{read_back, spawn_engine, spawn_engine_with};
    use vma_core::{Container, FrameRate};
    use vma_engine::{
        Command, EngineConfig, OutputStatus, RenderEngine, RtmpSpec, Secret, Source, SrtMode,
        SrtSpec, State, StateReader, video_pts_ns,
    };

    const REC: OutputId = State::DEFAULT_RECORDING;
    const MS: u64 = 1_000_000;

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vma-rec-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn send(engine: &RenderEngine, command: Command) {
        engine.commands().send(Source::Ui, command).unwrap();
    }

    fn record_into(engine: &RenderEngine, folder: Option<PathBuf>, container: Container) {
        send(
            engine,
            Command::UpdateOutput {
                output: REC,
                spec: OutputSpec::Record(RecordSpec {
                    folder,
                    container,
                    ..RecordSpec::default()
                }),
            },
        );
    }

    /// Waits until `output` reaches a status `want` accepts; returns it.
    fn wait_status(
        state: &StateReader,
        output: OutputId,
        want: impl Fn(&OutputStatus) -> bool,
    ) -> OutputStatus {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            let status = state.load().output(output).map(|o| o.status.clone());
            match status {
                Some(s) if want(&s) => return s,
                s if Instant::now() > end => panic!("{output} still {s:?}"),
                _ => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }

    #[test]
    fn record_button_writes_a_readable_file_into_the_chosen_folder() {
        // Step 5 milestone: Program → file, through the commands only.
        let mut engine = spawn_engine(FrameRate::FPS_50, false);
        let io = IoRuntime::start(engine.take_io_port().unwrap()).unwrap();
        let state = engine.state();
        let dir = folder("button");
        record_into(&engine, Some(dir.clone()), Container::Mkv);
        send(&engine, Command::StartRecording);
        let status = wait_status(&state, REC, |s| {
            !matches!(s, OutputStatus::Starting | OutputStatus::Idle)
        });
        let OutputStatus::Running { encoder, target } = status else {
            panic!("{status:?}");
        };
        let target = PathBuf::from(target.unwrap());
        eprintln!("{encoder} → {}", target.display());
        assert_eq!(target.parent(), Some(dir.as_path()));
        let name = target.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("VMA 20") && name.ends_with(".mkv"),
            "{name}"
        );
        assert!(encoder.contains(" + "), "{encoder}");

        std::thread::sleep(Duration::from_millis(1500));
        let activity = io
            .output_activity(REC)
            .expect("activity of a running output");
        assert!(activity.bytes.unwrap_or(0) > 0, "{activity:?}");
        assert!(activity.kbps.is_some(), "{activity:?}");
        let counters = engine.outputs().counters(REC).unwrap();
        assert!(counters.delivered >= 50, "{counters:?}");

        send(&engine, Command::StopRecording);
        wait_status(&state, REC, |s| *s == OutputStatus::Idle);
        assert_eq!(io.output_activity(REC), None);
        drop(engine);
        drop(io);
        let (back, error) = read_back(&target);
        assert_eq!(error, None);
        assert!(back.video.len() >= 50, "{} pictures", back.video.len());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_recording_running_at_exit_is_finalised() {
        // MP4: only readable as a whole once finalised.
        let mut engine = spawn_engine(FrameRate::FPS_25, false);
        let io = IoRuntime::start(engine.take_io_port().unwrap()).unwrap();
        let state = engine.state();
        let dir = folder("exit");
        record_into(&engine, Some(dir.clone()), Container::Mp4);
        send(&engine, Command::StartRecording);
        let status = wait_status(&state, REC, |s| matches!(s, OutputStatus::Running { .. }));
        let OutputStatus::Running { target, .. } = status else {
            unreachable!()
        };
        std::thread::sleep(Duration::from_millis(800));
        // As the application closes: the engine first, then `vma-io`.
        drop(engine);
        drop(io);
        let (back, error) = read_back(Path::new(&target.unwrap()));
        assert_eq!(error, None);
        assert!(back.video.len() >= 15, "{} pictures", back.video.len());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// B14 with a real recording: the GPU is lost mid-run; the file goes on with black
    /// pictures one period apart until the grace period is over, then it is finalised
    /// (stopped, not failed) and readable to the end.
    #[test]
    fn a_recording_through_a_gpu_loss_goes_black_then_is_finalised() {
        let rate = FrameRate::FPS_25;
        let grace = Duration::from_secs(1);
        let (mut engine, device) = spawn_engine_with(EngineConfig {
            rate,
            device_lost_grace: grace,
            ..EngineConfig::default()
        });
        let io = IoRuntime::start(engine.take_io_port().unwrap()).unwrap();
        let state = engine.state();
        let dir = folder("gpu-lost");
        record_into(&engine, Some(dir.clone()), Container::Mkv);
        send(&engine, Command::StartRecording);
        let status = wait_status(&state, REC, |s| matches!(s, OutputStatus::Running { .. }));
        let OutputStatus::Running { target, .. } = status else {
            unreachable!()
        };
        std::thread::sleep(Duration::from_millis(800));

        device.destroy();
        let end = wait_status(&state, REC, |s| {
            !matches!(s, OutputStatus::Running { .. } | OutputStatus::Stopping)
        });
        assert_eq!(end, OutputStatus::Idle);
        assert!(state.load().device_lost.is_some());
        drop(engine);
        drop(io);

        let (back, error) = read_back(Path::new(&target.unwrap()));
        assert_eq!(error, None);
        // Test pattern, then black (first luma byte, after encoding).
        let switch = back
            .video
            .iter()
            .position(|&(_, y)| y <= 24)
            .expect("no black picture");
        let (rendered, black) = back.video.split_at(switch);
        eprintln!(
            "{} pictures, then {} black; luma {:?} → {:?}",
            rendered.len(),
            black.len(),
            rendered.last(),
            black.first()
        );
        assert!(rendered.len() >= 15, "{} pictures", rendered.len());
        assert!(rendered.iter().all(|&(_, y)| y > 40), "{rendered:?}");
        assert!(black.iter().all(|&(_, y)| y <= 24), "{black:?}");
        let expected = grace.as_secs_f64() * rate.as_f64();
        assert!(
            black.len() as f64 >= expected * 0.8,
            "{} black",
            black.len()
        );
        // A whole number of periods apart (PTS of n). Before the loss a frame may not have
        // been read back (often under WARP); at the loss, those in flight on the GPU are lost;
        // the black ones, made on the CPU, follow each other.
        let period = video_pts_ns(rate, 1);
        for (i, pair) in back.video.windows(2).enumerate() {
            let step = pair[1].0 - pair[0].0;
            let periods = (step as f64 / period as f64).round() as u64;
            assert!(step.abs_diff(periods * period) <= MS, "step {step} at {i}");
            assert!(periods >= 1, "step {step} at {i}");
            let black_part = i >= switch;
            let at_loss = i + 1 == switch;
            assert!(
                !black_part || periods == 1,
                "{periods} periods at {i}, black"
            );
            assert!(!at_loss || periods <= 6, "{periods} periods at the loss");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// B1 tooling: a recording made with `--sync-test` (real encoders), read back by the
    /// sync analyser: one flash a second, each with its beep, within one frame.
    #[test]
    fn sync_test_recording_has_its_beeps_on_its_flashes() {
        let rate = FrameRate::FPS_50;
        let mut engine = spawn_engine(rate, true);
        let io = IoRuntime::start(engine.take_io_port().unwrap()).unwrap();
        let state = engine.state();
        let dir = folder("sync");
        record_into(&engine, Some(dir.clone()), Container::Mkv);
        send(&engine, Command::StartRecording);
        let status = wait_status(&state, REC, |s| matches!(s, OutputStatus::Running { .. }));
        let OutputStatus::Running { target, encoder } = status else {
            unreachable!()
        };
        std::thread::sleep(Duration::from_millis(4_500));
        send(&engine, Command::StopRecording);
        wait_status(&state, REC, |s| *s == OutputStatus::Idle);
        drop(engine);
        drop(io);

        let path = PathBuf::from(target.unwrap());
        let uri = format!("file://{}", path.display()).replace('\\', "/");
        let uri = if uri.starts_with("file:///") {
            uri
        } else {
            uri.replacen("file://", "file:///", 1)
        };
        let analyser = crate::SyncAnalyser::start(&uri, rate).unwrap();
        let end = Instant::now() + Duration::from_secs(20);
        while !analyser.ended() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        let sync = analyser.stop();
        eprintln!("{encoder}: {sync:?}");
        assert!(sync.ended && sync.last_error.is_none(), "{sync:?}");
        assert!(sync.flashes >= 4, "{sync:?}");
        assert_eq!(sync.offsets_ns.len() as u64, sync.flashes, "{sync:?}");
        assert_eq!(sync.unpaired_beeps, 0, "{sync:?}");
        // B1 asks for one frame; the AAC priming is compensated, so only the millisecond of
        // Matroska remains.
        assert!(sync.worst_ns().unwrap() <= 2 * MS as i64, "{sync:?}");
        assert_eq!(sync.video_gaps, 0, "{sync:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Puts a sync source on the Program of an engine at `master`, records `seconds` once its
    /// sound has settled, and reads the file back with the sync analyser.
    fn sync_source_recording(
        name: &str,
        master: FrameRate,
        spec: vma_engine::InputSpec,
        seconds: u64,
    ) -> crate::SyncSnapshot {
        let (mut engine, _device) = spawn_engine_with(EngineConfig {
            rate: master,
            stream_clock: Some(crate::stream_clock()),
            ..EngineConfig::default()
        });
        let io = IoRuntime::start(engine.take_io_port().unwrap()).unwrap();
        let state = engine.state();
        send(&engine, Command::AddInput { spec, name: None });
        let end = Instant::now() + Duration::from_secs(10);
        let input = loop {
            let live = state.load().inputs.iter().find_map(|i| {
                (matches!(i.spec, vma_engine::InputSpec::SyncSource { .. })
                    && matches!(i.status, vma_engine::InputStatus::Live { .. }))
                .then_some(i.id)
            });
            match live {
                Some(id) => break id,
                None if Instant::now() > end => panic!("sync source not live"),
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        };
        send(&engine, Command::CutDirect { input });
        // The latency of its pictures is smoothed over ≈ 1 s.
        std::thread::sleep(Duration::from_secs(3));
        let dir = folder(name);
        record_into(&engine, Some(dir.clone()), Container::Mkv);
        send(&engine, Command::StartRecording);
        let status = wait_status(&state, REC, |s| matches!(s, OutputStatus::Running { .. }));
        let OutputStatus::Running { target, .. } = status else {
            unreachable!()
        };
        std::thread::sleep(Duration::from_secs(seconds));
        let counters = engine.outputs().counters(REC);
        let render = engine.render_stats().snapshot();
        send(&engine, Command::StopRecording);
        wait_status(&state, REC, |s| *s == OutputStatus::Idle);
        let distribution = engine.outputs().distribution();
        let audio = engine.audio_stats();
        drop(engine);
        drop(io);
        eprintln!("{audio:?}\n{distribution:?}\n{counters:?}\n{render:?}");
        assert_eq!(distribution.audio_late, 0, "{distribution:?}");

        let path = PathBuf::from(target.unwrap());
        let uri = format!("file://{}", path.display()).replace('\\', "/");
        let uri = if uri.starts_with("file:///") {
            uri
        } else {
            uri.replacen("file://", "file:///", 1)
        };
        let analyser = crate::SyncAnalyser::start(&uri, master).unwrap();
        let end = Instant::now() + Duration::from_secs(30 + seconds);
        while !analyser.ended() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        let sync = analyser.stop();
        let _ = std::fs::remove_dir_all(dir);
        assert!(sync.ended && sync.last_error.is_none(), "{sync:?}");
        assert_eq!(sync.unpaired_beeps, 0, "{sync:?}");
        assert_eq!(sync.offsets_ns.len() as u64, sync.flashes, "{sync:?}");
        sync
    }

    fn sync_source(rate: FrameRate, drift_ppm: i32, audio_lead_ms: i32) -> vma_engine::InputSpec {
        vma_engine::InputSpec::SyncSource {
            rate,
            sample_rate: 44_100,
            drift_ppm,
            audio_lead_ms,
        }
    }

    /// C1 (BRIEF, Phase 3): lip sync below one frame, sync source → Program → recording read
    /// back, the source at another rate than the master.
    fn lip_sync(name: &str, master: FrameRate, source: FrameRate) {
        let sync = sync_source_recording(name, master, sync_source(source, 0, 0), 5);
        let frame = video_pts_ns(master, 1) as i64;
        eprintln!("{master} ← {source}: {sync:?}");
        assert!(sync.flashes >= 4, "{sync:?}");
        assert!(
            sync.worst_ns().unwrap() < frame,
            "{master} ← {source}: {sync:?}"
        );
    }

    #[test]
    fn lip_sync_25_from_50() {
        lip_sync("c1-25", FrameRate::FPS_25, FrameRate::FPS_50);
    }

    #[test]
    fn lip_sync_29_97_from_59_94() {
        lip_sync("c1-2997", FrameRate::FPS_29_97, FrameRate::FPS_59_94);
    }

    #[test]
    fn lip_sync_30_from_60() {
        lip_sync("c1-30", FrameRate::FPS_30, FrameRate::FPS_60);
    }

    #[test]
    fn lip_sync_50_from_25() {
        lip_sync("c1-50", FrameRate::FPS_50, FrameRate::FPS_25);
    }

    #[test]
    fn lip_sync_59_94_from_29_97() {
        lip_sync("c1-5994", FrameRate::FPS_59_94, FrameRate::FPS_29_97);
    }

    #[test]
    fn lip_sync_60_from_30() {
        lip_sync("c1-60", FrameRate::FPS_60, FrameRate::FPS_30);
    }

    /// C2 (first half; the delay setting comes with the commands of step 4): a source whose
    /// sound is 80 ms early is measured 80 ms early, to a frame.
    #[test]
    fn early_sound_is_measured_early() {
        let master = FrameRate::FPS_50;
        let sync = sync_source_recording("c2", master, sync_source(FrameRate::FPS_50, 0, 80), 5);
        eprintln!("{sync:?}");
        assert!(sync.flashes >= 4, "{sync:?}");
        for &offset in &sync.offsets_ns {
            assert!(
                (offset + 80 * MS as i64).abs() < video_pts_ns(master, 1) as i64,
                "{sync:?}"
            );
        }
    }

    /// C3 (real time): sources at +100 and −100 ppm for 60 s: the sound stays on its pictures
    /// to a frame, without a gap.
    #[test]
    fn drifting_sources_stay_in_sync_for_a_minute() {
        let master = FrameRate::FPS_50;
        for (name, ppm) in [("c3-plus", 100), ("c3-minus", -100)] {
            let sync =
                sync_source_recording(name, master, sync_source(FrameRate::FPS_50, ppm, 0), 60);
            let (first, last) = (
                sync.offsets_ns[0],
                sync.offsets_ns[sync.offsets_ns.len() - 1],
            );
            eprintln!(
                "{ppm} ppm: {} pairs, worst {:.2} ms, first {:.2} ms, last {:.2} ms",
                sync.offsets_ns.len(),
                sync.worst_ns().unwrap() as f64 / 1e6,
                first as f64 / 1e6,
                last as f64 / 1e6
            );
            assert!(sync.flashes >= 55, "{sync:?}");
            assert!(
                sync.worst_ns().unwrap() < video_pts_ns(master, 1) as i64,
                "{sync:?}"
            );
            assert_eq!(sync.video_gaps, 0, "{sync:?}");
        }
    }

    #[test]
    fn refused_starts_fail_the_output_only() {
        let mut engine = spawn_engine(FrameRate::FPS_25, false);
        let _io = IoRuntime::start(engine.take_io_port().unwrap()).unwrap();
        let state = engine.state();
        record_into(
            &engine,
            Some(std::env::temp_dir().join("vma-no-such-folder/x")),
            Container::Mkv,
        );
        send(&engine, Command::StartRecording);
        let status = wait_status(&state, REC, |s| matches!(s, OutputStatus::Failed { .. }));
        assert!(
            format!("{status:?}").contains("does not exist"),
            "{status:?}"
        );

        // The Program keeps running.
        let before = engine.feedback().last_frame();
        std::thread::sleep(Duration::from_millis(100));
        assert!(engine.feedback().last_frame() > before);
    }

    #[test]
    fn file_names_carry_the_date_and_never_overwrite() {
        let dir = folder("names");
        let first = record_path(&dir, "2026-10-31 18-04-05", "mkv").unwrap();
        assert_eq!(first, dir.join("VMA 2026-10-31 18-04-05.mkv"));
        std::fs::write(&first, b"").unwrap();
        let second = record_path(&dir, "2026-10-31 18-04-05", "mkv").unwrap();
        assert_eq!(second, dir.join("VMA 2026-10-31 18-04-05 (2).mkv"));
        assert!(
            local_stamp().len() == "2026-10-31 18-04-05".len(),
            "{}",
            local_stamp()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    // Streaming (Phase 2 step 6).

    /// A local MediaMTX (RTMP only) on its own port, killed on drop.
    struct Server {
        binary: PathBuf,
        config: PathBuf,
        port: u16,
        child: Option<std::process::Child>,
    }

    impl Server {
        /// `None` (test skipped) without `VMA_MEDIAMTX`; the CI sets it, with
        /// `VMA_REQUIRE_MEDIAMTX` so that a missing server fails instead.
        fn start() -> Option<Self> {
            let Some(binary) = std::env::var_os("VMA_MEDIAMTX").map(PathBuf::from) else {
                assert!(
                    std::env::var_os("VMA_REQUIRE_MEDIAMTX").is_none(),
                    "VMA_MEDIAMTX not set"
                );
                eprintln!("skipped: no MediaMTX (set VMA_MEDIAMTX to its binary)");
                return None;
            };
            let port = std::net::TcpListener::bind("127.0.0.1:0")
                .and_then(|l| l.local_addr())
                .unwrap()
                .port();
            let config = std::env::temp_dir()
                .join(format!("vma-mediamtx-{}-{port}.yml", std::process::id()));
            std::fs::write(
                &config,
                format!(
                    "logLevel: warn\nrtsp: false\nhls: false\nwebrtc: false\nsrt: false\n\
                     moq: false\nrtmp: true\nrtmpAddress: 127.0.0.1:{port}\npaths:\n  all_others:\n"
                ),
            )
            .unwrap();
            let mut server = Self {
                binary,
                config,
                port,
                child: None,
            };
            server.run();
            Some(server)
        }

        fn run(&mut self) {
            let child = std::process::Command::new(&self.binary)
                .arg(&self.config)
                // Whatever it writes (certificates…) goes to the temporary folder.
                .current_dir(std::env::temp_dir())
                .stdout(std::process::Stdio::null())
                .spawn()
                .expect("MediaMTX starts");
            self.child = Some(child);
            let end = Instant::now() + Duration::from_secs(10);
            while std::net::TcpStream::connect(("127.0.0.1", self.port)).is_err() {
                assert!(Instant::now() < end, "MediaMTX does not listen");
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        /// The server goes away (connections reset), as a crashed or unreachable one.
        fn kill(&mut self) {
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }

        fn url(&self) -> String {
            format!("rtmp://127.0.0.1:{}/live", self.port)
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.kill();
            let _ = std::fs::remove_file(&self.config);
        }
    }

    /// Video buffers received from `location` over `window`, once the server has the stream
    /// (it announces it after probing the tracks, ≈ 2 s).
    fn received(location: &str, window: Duration) -> u64 {
        use gstreamer as gst;
        use gstreamer::prelude::*;
        use std::sync::atomic::{AtomicU64, Ordering};
        let end = Instant::now() + Duration::from_secs(15);
        loop {
            let description = format!(
                "rtmp2src location={location} ! flvdemux name=d \
                 d.video ! queue ! appsink name=v sync=false emit-signals=false \
                 d.audio ! queue ! fakesink sync=false"
            );
            let pipeline = gst::parse::launch(&description)
                .unwrap()
                .downcast::<gst::Pipeline>()
                .unwrap();
            let count = Arc::new(AtomicU64::new(0));
            let sink = pipeline
                .by_name("v")
                .unwrap()
                .downcast::<gstreamer_app::AppSink>()
                .unwrap();
            let counter = Arc::clone(&count);
            sink.set_callbacks(
                gstreamer_app::AppSinkCallbacks::builder()
                    .new_sample(move |s| {
                        let _ = s.pull_sample();
                        counter.fetch_add(1, Ordering::Relaxed);
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );
            pipeline.set_state(gst::State::Playing).unwrap();
            std::thread::sleep(window);
            let _ = pipeline.set_state(gst::State::Null);
            let n = count.load(Ordering::Relaxed);
            if n > 0 || Instant::now() > end {
                return n;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    fn rtmp(url: &str, key: &str) -> Command {
        Command::AddOutput {
            spec: OutputSpec::Rtmp(RtmpSpec {
                url: url.into(),
                key: Secret::new(key),
                video_kbps: None,
            }),
            name: None,
        }
    }

    #[test]
    fn three_destinations_stream_at_once() {
        // B7: 3 destinations, started together, each received under its key.
        let Some(server) = Server::start() else {
            return;
        };
        crate::pipeline::tests::init();
        let mut engine = spawn_engine(FrameRate::FPS_50, false);
        let io = IoRuntime::start(engine.take_io_port().unwrap()).unwrap();
        let state = engine.state();
        for key in ["s1", "s2", "s3"] {
            send(&engine, rtmp(&server.url(), key));
        }
        send(&engine, Command::StartStreaming { slot: None });
        let streams = [OutputId(2), OutputId(3), OutputId(4)];
        for id in streams {
            let status = wait_status(&state, id, |s| {
                matches!(
                    s,
                    OutputStatus::Running { .. } | OutputStatus::Failed { .. }
                )
            });
            assert!(matches!(status, OutputStatus::Running { .. }), "{status:?}");
        }
        // A while without readers, then each destination read back.
        std::thread::sleep(Duration::from_secs(2));
        for key in ["s1", "s2", "s3"] {
            let n = received(&format!("{}/{key}", server.url()), Duration::from_secs(2));
            eprintln!("{key}: {n} pictures in 2 s");
            assert!(n >= 25, "{key}: {n} pictures in 2 s");
        }
        for id in streams {
            let activity = io.output_activity(id).unwrap();
            assert!(activity.bytes.unwrap_or(0) > 0, "{activity:?}");
            assert_eq!(engine.outputs().counters(id).unwrap().dropped, 0);
        }
        send(&engine, Command::StopStreaming { slot: None });
        for id in streams {
            wait_status(&state, id, |s| *s == OutputStatus::Idle);
        }
    }

    /// B1 tooling, live: a `--sync-test` stream read back from the server by the sync
    /// analyser, as the 1 h soak does: a flash a second, each with its beep, within the
    /// millisecond of FLV; no interruption.
    #[test]
    fn sync_analyser_reads_a_live_stream_back() {
        let Some(server) = Server::start() else {
            return;
        };
        crate::pipeline::tests::init();
        let rate = FrameRate::FPS_50;
        let mut engine = spawn_engine(rate, true);
        let io = IoRuntime::start(engine.take_io_port().unwrap()).unwrap();
        let state = engine.state();
        send(&engine, rtmp(&server.url(), "sync"));
        send(&engine, Command::StartStreaming { slot: None });
        wait_status(&state, OutputId(2), |s| {
            matches!(s, OutputStatus::Running { .. })
        });
        let analyser = crate::SyncAnalyser::start(&format!("{}/sync", server.url()), rate).unwrap();
        // The server announces the stream once it has probed it (≈ 2 s), then 5 flashes.
        let end = Instant::now() + Duration::from_secs(20);
        while analyser.snapshot().offsets_ns.len() < 5 && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
        }
        let sync = analyser.stop();
        drop(engine);
        drop(io);
        eprintln!("{sync:?}");
        assert!(sync.offsets_ns.len() >= 5, "{sync:?}");
        assert!(sync.worst_ns().unwrap() <= 2 * MS as i64, "{sync:?}");
        assert_eq!(sync.unpaired_beeps, 0, "{sync:?}");
        assert!(sync.unpaired_flashes <= 1, "{sync:?}");
        assert_eq!((sync.interruptions, sync.video_gaps), (0, 0), "{sync:?}");
    }

    #[test]
    fn lost_server_reconnects_while_the_recording_runs() {
        // B7: server cut → reconnection after 1 s (then 2, 5, 10 s); the recording beside it
        // goes on without a gap.
        let Some(mut server) = Server::start() else {
            return;
        };
        crate::pipeline::tests::init();
        let rate = FrameRate::FPS_50;
        let mut engine = spawn_engine(rate, false);
        let io = IoRuntime::start(engine.take_io_port().unwrap()).unwrap();
        let state = engine.state();
        let dir = folder("beside-stream");
        record_into(&engine, Some(dir.clone()), Container::Mkv);
        send(&engine, rtmp(&server.url(), "cut"));
        let stream = OutputId(2);
        send(&engine, Command::StartRecording);
        send(&engine, Command::StartStreaming { slot: None });
        let target = match wait_status(&state, REC, |s| matches!(s, OutputStatus::Running { .. })) {
            OutputStatus::Running { target, .. } => PathBuf::from(target.unwrap()),
            _ => unreachable!(),
        };
        wait_status(&state, stream, |s| {
            matches!(s, OutputStatus::Running { .. })
        });
        std::thread::sleep(Duration::from_millis(500));

        server.kill();
        let lost_at = Instant::now();
        let status = wait_status(&state, stream, |s| {
            matches!(s, OutputStatus::Reconnecting { .. })
        });
        eprintln!("lost after {:?}: {status:?}", lost_at.elapsed());
        assert!(matches!(
            status,
            OutputStatus::Reconnecting { attempt: 1, .. }
        ));
        server.run();
        wait_status(&state, stream, |s| {
            matches!(s, OutputStatus::Running { .. })
        });
        let back = lost_at.elapsed();
        eprintln!("running again after {back:?}");
        assert!(back < Duration::from_secs(9), "{back:?}");
        assert!(io.output_activity(stream).unwrap().reconnections >= 1);
        let n = received(&format!("{}/cut", server.url()), Duration::from_secs(2));
        assert!(n >= 25, "{n} pictures after the reconnection");

        let recording = engine.outputs().counters(REC).unwrap();
        assert_eq!(
            recording.dropped, 0,
            "the recording lost nothing: {recording:?}"
        );
        send(&engine, Command::StopRecording);
        send(&engine, Command::StopStreaming { slot: None });
        wait_status(&state, REC, |s| *s == OutputStatus::Idle);
        drop(engine);
        drop(io);
        let (back, error) = read_back(&target);
        assert_eq!(error, None);
        let period = vma_engine::video_pts_ns(rate, 1);
        let gaps = back
            .video
            .windows(2)
            .filter(|w| w[1].0.saturating_sub(w[0].0) > period + 1_000_000)
            .count();
        assert_eq!(
            gaps,
            0,
            "the recording has no gap ({} pictures)",
            back.video.len()
        );
        assert!(back.video.len() as u64 >= recording.delivered);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[derive(Clone, Default)]
    struct Logs(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Logs {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn unreachable_server_is_retried_and_the_key_never_shows() {
        // B13 for streams: GStreamer quotes the location (with the key) in its errors.
        const KEY: &str = "live_5c0ffee9d2a1";
        let logs = Logs::default();
        let writer = logs.clone();
        // Global: `vma-io` logs from its own thread. One process per test with nextest; under
        // `cargo test` another test may have set it first (the logs are then not checked).
        let captured = tracing::subscriber::set_global_default(
            tracing_subscriber::fmt()
                .with_max_level(tracing::Level::TRACE)
                .with_writer(move || writer.clone())
                .finish(),
        )
        .is_ok();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .unwrap()
            .port();
        let mut engine = spawn_engine(FrameRate::FPS_25, false);
        let _io = IoRuntime::start(engine.take_io_port().unwrap()).unwrap();
        let state = engine.state();
        send(&engine, rtmp(&format!("rtmp://127.0.0.1:{port}/live"), KEY));
        send(&engine, Command::StartStreaming { slot: None });
        let stream = OutputId(2);
        let first = wait_status(&state, stream, |s| {
            matches!(
                s,
                OutputStatus::Reconnecting { .. } | OutputStatus::Failed { .. }
            )
        });
        let OutputStatus::Reconnecting { attempt: 1, reason } = &first else {
            panic!("{first:?}");
        };
        eprintln!("{reason}");
        // Retried after 1 s, still unreachable: attempt 2.
        let second = wait_status(&state, stream, |s| {
            matches!(s, OutputStatus::Reconnecting { attempt: 2, .. })
        });
        let journal = serde_json::to_string(&*state.load()).unwrap();
        send(&engine, Command::StopStreaming { slot: None });
        wait_status(&state, stream, |s| *s == OutputStatus::Idle);
        drop(engine);
        for text in [format!("{first:?} {second:?}"), journal] {
            assert!(!text.contains(KEY), "{text}");
        }
        if captured {
            let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
            assert!(logs.contains("stream lost"), "{logs}");
            assert!(!logs.contains(KEY), "{logs}");
        }
    }

    // SRT (Phase 2 step 7).

    /// `srtsink` and `srtsrc` are there; `None` (test skipped) otherwise, except where
    /// `VMA_REQUIRE_SRT` says they must be (CI).
    fn srt_available() -> Option<()> {
        crate::pipeline::tests::init();
        let present = ["srtsink", "srtsrc", "mpegtsmux", "tsdemux"]
            .iter()
            .all(|e| gstreamer::ElementFactory::find(e).is_some());
        if !present {
            assert!(
                std::env::var_os("VMA_REQUIRE_SRT").is_none(),
                "srtsink / srtsrc missing"
            );
            eprintln!("skipped: no SRT elements (gst-plugins-bad)");
            return None;
        }
        Some(())
    }

    fn free_udp_port() -> u16 {
        std::net::UdpSocket::bind("127.0.0.1:0")
            .and_then(|s| s.local_addr())
            .unwrap()
            .port()
    }

    /// A local SRT receiver counting the H.264 pictures it gets.
    struct Receiver {
        pipeline: gstreamer::Pipeline,
        count: Arc<std::sync::atomic::AtomicU64>,
    }

    impl Receiver {
        fn start(uri: &str, passphrase: &str) -> Self {
            use gstreamer as gst;
            use gstreamer::prelude::*;
            let pipeline = gst::parse::launch(&format!(
                "srtsrc name=src uri={uri} ! tsdemux ! video/x-h264 ! queue ! \
                 appsink name=v sync=false"
            ))
            .unwrap()
            .downcast::<gst::Pipeline>()
            .unwrap();
            if !passphrase.is_empty() {
                pipeline
                    .by_name("src")
                    .unwrap()
                    .set_property("passphrase", passphrase);
            }
            let count = Arc::new(std::sync::atomic::AtomicU64::new(0));
            let counter = Arc::clone(&count);
            pipeline
                .by_name("v")
                .unwrap()
                .downcast::<gstreamer_app::AppSink>()
                .unwrap()
                .set_callbacks(
                    gstreamer_app::AppSinkCallbacks::builder()
                        .new_sample(move |s| {
                            let _ = s.pull_sample();
                            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            Ok(gst::FlowSuccess::Ok)
                        })
                        .build(),
                );
            let _ = pipeline.set_state(gst::State::Playing);
            Self { pipeline, count }
        }

        fn count(&self) -> u64 {
            self.count.load(std::sync::atomic::Ordering::Relaxed)
        }

        /// Pictures received over the next `window`.
        fn received(&self, window: Duration) -> u64 {
            let before = self.count();
            std::thread::sleep(window);
            self.count() - before
        }
    }

    impl Drop for Receiver {
        fn drop(&mut self) {
            use gstreamer::prelude::*;
            let _ = self.pipeline.set_state(gstreamer::State::Null);
        }
    }

    fn srt(mode: SrtMode, port: u16, passphrase: &str) -> Command {
        Command::AddOutput {
            spec: OutputSpec::Srt(SrtSpec {
                mode,
                host: "127.0.0.1".into(),
                port,
                latency_ms: 120,
                passphrase: Secret::new(passphrase),
                video_kbps: None,
            }),
            name: None,
        }
    }

    const PASSPHRASE: &str = "correct horse battery";

    #[test]
    fn srt_caller_reaches_its_receiver_and_reconnects() {
        // B8: caller with a passphrase, received and parsed by a local `srtsrc`; the receiver
        // goes away → reconnecting; it comes back → running and received again.
        if srt_available().is_none() {
            return;
        }
        let port = free_udp_port();
        let listen = format!("srt://127.0.0.1:{port}?mode=listener&latency=120");
        let receiver = Receiver::start(&listen, PASSPHRASE);
        let mut engine = spawn_engine(FrameRate::FPS_50, false);
        let io = IoRuntime::start(engine.take_io_port().unwrap()).unwrap();
        let state = engine.state();
        send(&engine, srt(SrtMode::Caller, port, PASSPHRASE));
        let out = OutputId(2);
        send(&engine, Command::StartOutput { output: out });
        let status = wait_status(&state, out, |s| {
            matches!(
                s,
                OutputStatus::Running { .. } | OutputStatus::Failed { .. }
            )
        });
        assert!(matches!(status, OutputStatus::Running { .. }), "{status:?}");
        let n = receiver.received(Duration::from_secs(2));
        eprintln!("caller: {n} pictures in 2 s");
        assert!(n >= 25, "{n} pictures in 2 s");
        assert_eq!(engine.outputs().counters(out).unwrap().dropped, 0);

        drop(receiver);
        let lost = Instant::now();
        let status = wait_status(&state, out, |s| {
            matches!(s, OutputStatus::Reconnecting { .. })
        });
        eprintln!("lost after {:?}: {status:?}", lost.elapsed());
        let receiver = Receiver::start(&listen, PASSPHRASE);
        wait_status(&state, out, |s| matches!(s, OutputStatus::Running { .. }));
        eprintln!("running again after {:?}", lost.elapsed());
        let n = receiver.received(Duration::from_secs(2));
        assert!(n >= 25, "{n} pictures after the reconnection");
        assert!(io.output_activity(out).unwrap().reconnections >= 1);
        send(&engine, Command::StopOutput { output: out });
        wait_status(&state, out, |s| *s == OutputStatus::Idle);
    }

    #[test]
    fn srt_wrong_passphrase_is_refused_and_never_shown() {
        if srt_available().is_none() {
            return;
        }
        const WRONG: &str = "not the right one";
        let port = free_udp_port();
        let receiver = Receiver::start(
            &format!("srt://127.0.0.1:{port}?mode=listener&latency=120"),
            PASSPHRASE,
        );
        let mut engine = spawn_engine(FrameRate::FPS_25, false);
        let _io = IoRuntime::start(engine.take_io_port().unwrap()).unwrap();
        let state = engine.state();
        send(&engine, srt(SrtMode::Caller, port, WRONG));
        let out = OutputId(2);
        send(&engine, Command::StartOutput { output: out });
        let status = wait_status(&state, out, |s| {
            matches!(
                s,
                OutputStatus::Reconnecting { .. }
                    | OutputStatus::Failed { .. }
                    | OutputStatus::Running { .. }
            )
        });
        eprintln!("{status:?}");
        assert!(
            matches!(status, OutputStatus::Reconnecting { .. }),
            "{status:?}"
        );
        let text = format!(
            "{status:?} {}",
            serde_json::to_string(&*state.load()).unwrap()
        );
        assert!(
            !text.contains(WRONG) && !text.contains(PASSPHRASE),
            "{text}"
        );
        assert_eq!(receiver.count(), 0, "nothing decrypted");
    }

    #[test]
    fn srt_listener_waits_for_its_receivers() {
        // B8: VMA listens; on air at once, nothing lost while nobody calls; a caller then
        // receives the stream.
        if srt_available().is_none() {
            return;
        }
        let port = free_udp_port();
        let mut engine = spawn_engine(FrameRate::FPS_50, false);
        let io = IoRuntime::start(engine.take_io_port().unwrap()).unwrap();
        let state = engine.state();
        send(&engine, srt(SrtMode::Listener, port, ""));
        let out = OutputId(2);
        send(&engine, Command::StartOutput { output: out });
        let status = wait_status(&state, out, |s| {
            matches!(
                s,
                OutputStatus::Running { .. } | OutputStatus::Failed { .. }
            )
        });
        let OutputStatus::Running { target, .. } = &status else {
            panic!("{status:?}");
        };
        assert_eq!(
            target.as_deref(),
            Some(format!("srt://127.0.0.1:{port} (listening)").as_str())
        );
        std::thread::sleep(Duration::from_secs(1));
        let alone = engine.outputs().counters(out).unwrap();
        assert!(alone.delivered >= 40 && alone.dropped == 0, "{alone:?}");

        let receiver = Receiver::start(
            &format!("srt://127.0.0.1:{port}?mode=caller&latency=120"),
            "",
        );
        let n = receiver.received(Duration::from_secs(3));
        eprintln!("listener: {n} pictures in 3 s");
        assert!(n >= 25, "{n} pictures in 3 s");
        assert!(io.output_activity(out).unwrap().bytes.unwrap_or(0) > 0);
        assert_eq!(engine.outputs().counters(out).unwrap().dropped, 0);
        drop(receiver);
        // Its receiver gone, a listener stays on air.
        std::thread::sleep(Duration::from_millis(500));
        assert!(matches!(
            state.load().output(out).unwrap().status,
            OutputStatus::Running { .. }
        ));
    }
}
