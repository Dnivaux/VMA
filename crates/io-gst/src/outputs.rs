//! Outputs on `vma-io` (Phase 2 step 5): starts, watches and stops one pipeline per output
//! on the requests of `vma-control`, reports each run back to it, and publishes what the
//! outputs are doing (bytes written, bit rate) for the statistics.
//!
//! Stopping never blocks the thread: the feed is detached from the engine's hub (its end of
//! stream follows), then the pipeline is polled until the file is finalised.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use gstreamer::glib;
use vma_core::OutputId;
use vma_engine::{IoEvent, OutputHub, OutputSpec, RecordSpec};

use crate::output::{GstOutput, OutputFormat, RecordSettings};

/// How long a stopping output may take to finalise its file before it is abandoned.
pub(crate) const STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// Window of the bit-rate measurement.
const RATE_WINDOW: Duration = Duration::from_secs(1);
/// Most attempts at a free file name (`VMA … (2).mkv`, …).
const NAME_ATTEMPTS: u32 = 100;

/// What a running output is doing, published 4 times a second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputActivity {
    /// When its pipeline started.
    pub since: Instant,
    /// Bytes written (file outputs).
    pub bytes: Option<u64>,
    /// Over the last second, kbit/s.
    pub kbps: Option<u32>,
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

/// One output with a pipeline: running, or finalising.
#[derive(Debug)]
struct Slot {
    id: OutputId,
    run: u64,
    pipeline: GstOutput,
    since: Instant,
    /// Since when it is finalising.
    stopping: Option<Instant>,
    /// Start of the bit-rate window and the bytes then.
    window: (Instant, u64),
    kbps: Option<u32>,
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

    /// Builds and starts run `run` of `output`; reports `OutputRunning` or `OutputFailed`.
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
            self.stop(output);
        }
        let started = match spec {
            _ if self.unavailable.is_some() => Err(format!(
                "GStreamer is not available: {}",
                self.unavailable.as_deref().unwrap_or_default()
            )),
            OutputSpec::Record(record) => self.record(output, record),
            OutputSpec::Rtmp(_) => {
                Err("RTMP streaming is not available yet (Phase 2, step 6)".into())
            }
            OutputSpec::Srt(_) => Err("SRT is not available yet (Phase 2, step 7)".into()),
        };
        match started {
            Ok((pipeline, target)) => {
                let encoder = describe(&pipeline);
                let now = Instant::now();
                self.slots.push(Slot {
                    id: output,
                    run,
                    pipeline,
                    since: now,
                    stopping: None,
                    window: (now, 0),
                    kbps: None,
                });
                emit(IoEvent::OutputRunning {
                    output,
                    run,
                    encoder,
                    target,
                });
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

    fn record(
        &self,
        output: OutputId,
        spec: &RecordSpec,
    ) -> Result<(GstOutput, Option<String>), String> {
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
        let mut settings = RecordSettings::new(path.clone(), self.format);
        settings.codec = spec.codec;
        settings.container = spec.container;
        if let Some(kbps) = spec.video_kbps {
            settings.video_kbps = kbps;
        }
        tracing::info!(%output, path = %path.display(), ?settings.codec, ?settings.container, kbps = settings.video_kbps, "starting a recording");
        let (pipeline, feed) =
            GstOutput::record(self.format, &settings).map_err(|e| e.to_string())?;
        self.hub
            .attach(output, Box::new(feed))
            .map_err(|e| e.to_string())?;
        Ok((pipeline, Some(path.display().to_string())))
    }

    /// Detaches `output` from the engine (its end of stream follows); [`Self::tick`] then
    /// waits for the file to be finalised.
    pub(crate) fn stop(&mut self, output: OutputId) {
        self.hub.detach(output);
        match self.slots.iter_mut().find(|s| s.id == output) {
            Some(slot) if slot.stopping.is_none() => slot.stopping = Some(Instant::now()),
            Some(_) => {}
            None => tracing::debug!(%output, "stop of an output without pipeline"),
        }
        // A stopping output has no activity to show any more.
        self.publish();
    }

    /// Watches the pipelines: failures, finalised files, bit rates.
    pub(crate) fn tick(&mut self, now: Instant, emit: &mut impl FnMut(IoEvent)) {
        let hub = &self.hub;
        let before = self.slots.len();
        self.slots.retain_mut(|slot| {
            let done = match slot.stopping {
                None => slot.pipeline.poll().map(|reason| Err(reason.to_owned())),
                Some(since) => slot
                    .pipeline
                    .poll_stop(now.saturating_duration_since(since), STOP_TIMEOUT)
                    .map(|done| done.map_err(|e| e.to_string())),
            };
            let (id, run) = (slot.id, slot.run);
            match done {
                None => true,
                Some(Ok(())) => {
                    emit(IoEvent::OutputStopped { output: id, run });
                    false
                }
                Some(Err(reason)) => {
                    hub.detach(id);
                    emit(IoEvent::OutputFailed {
                        output: id,
                        run,
                        reason,
                    });
                    false
                }
            }
        });
        for slot in &mut self.slots {
            let bytes = slot.pipeline.bytes().unwrap_or(0);
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
        if self.slots.len() != before {
            self.publish();
        }
    }

    /// Publishes the activity of the running outputs.
    pub(crate) fn publish(&self) {
        let all = self
            .slots
            .iter()
            .filter(|s| s.stopping.is_none())
            .map(|s| {
                (
                    s.id,
                    OutputActivity {
                        since: s.since,
                        bytes: s.pipeline.bytes(),
                        kbps: s.kbps,
                    },
                )
            })
            .collect();
        self.activities.store(Arc::new(all));
    }

    /// Stops every output and waits (at most [`STOP_TIMEOUT`] in all) for their files: on
    /// `vma-io`'s way out, so that a recording running at exit is finalised.
    pub(crate) fn shutdown(&mut self) {
        let ids: Vec<_> = self.slots.iter().map(|s| s.id).collect();
        for id in ids {
            self.stop(id);
        }
        let deadline = Instant::now() + STOP_TIMEOUT;
        while !self.slots.is_empty() && Instant::now() < deadline {
            self.tick(Instant::now(), &mut |event| {
                tracing::info!(?event, "output at exit")
            });
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
    use crate::output::tests::{read_back, spawn_engine};
    use vma_core::{Container, FrameRate};
    use vma_engine::{
        Command, OutputStatus, RenderEngine, RtmpSpec, Secret, Source, State, StateReader,
    };

    const REC: OutputId = State::DEFAULT_RECORDING;

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

        send(
            &engine,
            Command::AddOutput {
                spec: OutputSpec::Rtmp(RtmpSpec {
                    url: "rtmp://127.0.0.1/live".into(),
                    key: Secret::new("k"),
                    video_kbps: None,
                }),
                name: None,
            },
        );
        send(&engine, Command::StartStreaming { slot: None });
        let stream = OutputId(2);
        let status = wait_status(&state, stream, |s| matches!(s, OutputStatus::Failed { .. }));
        assert!(
            format!("{status:?}").contains("not available yet"),
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
}
