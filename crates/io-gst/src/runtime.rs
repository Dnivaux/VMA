//! The `vma-io` thread (ADR 0004): owns every pipeline, driven by `vma-control` through the
//! engine's [`IoPort`]. Opening, stopping and restarting pipelines may block: this is the only
//! thread that does it. Pictures themselves go from the GStreamer streaming threads straight
//! into the feeds (see `pipeline.rs`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use arc_swap::ArcSwap;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vma_core::{ColorInfo, InputId, OutputId};
use vma_engine::{HtmlAction, InputSpec, IoEvent, IoEventSender, IoPort, IoRequest, Timeline};

use crate::IoError;
use crate::audio_devices::{AudioDeviceList, AudioDevices, AudioInputSlot, AudioMonitor};
use crate::devices::{CaptureDevice, DeviceList, Monitor};
use crate::feeds::{Feeds, VideoFormat};
use crate::headphones::{Headphones, HeadphonesStatus};
use crate::html::HtmlFeedsOf;
use crate::mode::CaptureMode;
use crate::ndi::{NdiMonitor, NdiSourceList};
use crate::output::OutputFormat;
use crate::outputs::{Activities, OutputActivity, Outputs};
use crate::pipeline::{Capture, GstInput, Recipe};
use crate::still::{self, StillImage};
use crate::supervisor::retry_delay;
use vma_io_html::{HtmlEvent, HtmlHost, HtmlStatus, PageAction, data_event_script};
use vma_io_ndi::NdiStatus;

/// How often `vma-io` checks its pipelines (bus, watchdog, retries) without requests.
const POLL: Duration = Duration::from_millis(20);
/// How often the position of the video files is published (4 times a second).
const TIMELINE_PERIOD: Duration = Duration::from_millis(250);
/// How often the list of capture devices is read again.
const DEVICES_PERIOD: Duration = Duration::from_secs(1);

/// What a running pipeline reports for the statistics and the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineInfo {
    /// Capture device: the mode it was opened in.
    pub mode: Option<CaptureMode>,
    /// `videoconvert` converts the pictures (the source format is not uploaded as is).
    pub converting: bool,
}

/// Info of every running pipeline.
type Infos = Arc<ArcSwap<Vec<(InputId, PipelineInfo)>>>;

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
    /// Video file: what the operator asked for, applied again after a reconnection.
    playing: bool,
    looping: bool,
}

impl<F: Feeds> PipelineSlot<F> {
    pub(crate) fn new(input: InputId, recipe: Recipe, feeds: F, now: Instant) -> Self {
        Self {
            input,
            recipe,
            feeds,
            running: None,
            attempts: 0,
            retry_at: now,
            playing: false,
            looping: false,
        }
    }

    /// Video file: play or pause (kept for the next pipeline if it is down).
    pub(crate) fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
        if let Some(running) = &mut self.running {
            running.set_playing(playing);
        }
    }

    /// Video file: seek (lost if the pipeline is down: a new one starts at 0).
    pub(crate) fn seek(&mut self, ms: u64) {
        if let Some(running) = &mut self.running {
            running.seek(ms);
        }
    }

    pub(crate) fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
        if let Some(running) = &self.running {
            running.set_looping(looping);
        }
    }

    /// Video file: where it is (`None` for a live source or while the pipeline is down).
    pub(crate) fn timeline(&self) -> Option<Timeline> {
        self.running.as_ref()?.timeline()
    }

    /// What the running pipeline reports (`None` while it is down).
    pub(crate) fn info(&self) -> Option<PipelineInfo> {
        let running = self.running.as_ref()?;
        Some(PipelineInfo {
            mode: running.capture_mode().cloned(),
            converting: running.converting(),
        })
    }

    /// Starts, watches and restarts the pipeline; reports losses through `emit`.
    pub(crate) fn tick(&mut self, now: Instant, emit: &mut impl FnMut(IoEvent)) {
        match &mut self.running {
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
                    Ok(mut running) => {
                        running.set_looping(self.looping);
                        running.set_playing(self.playing);
                        self.running = Some(running);
                    }
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

/// Options of [`IoRuntime::start_with_options`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IoOptions {
    /// Play the headphones (on the system's default device until one is chosen). Off by
    /// default: tests and soaks play nothing on the machine's speakers.
    pub headphones: bool,
}

/// The `vma-io` thread. Dropping it stops every pipeline and joins the thread.
#[derive(Debug)]
pub struct IoRuntime {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    gstreamer: Result<(), String>,
    devices: DeviceList,
    audio_devices: AudioDeviceList,
    ndi: NdiStatus,
    ndi_sources: NdiSourceList,
    html: HtmlStatus,
    headphones: Arc<ArcSwap<HeadphonesStatus>>,
    infos: Infos,
    activities: Activities,
}

impl IoRuntime {
    /// Initialises GStreamer and starts `vma-io` on the engine's port. Without GStreamer
    /// (initialisation failed), still images keep working and capture / file inputs are
    /// reported lost ([`Self::gstreamer_error`]). No headphones.
    pub fn start(port: IoPort) -> Result<Self, IoError> {
        Self::start_with_options(port, IoOptions::default())
    }

    pub fn start_with_options(port: IoPort, options: IoOptions) -> Result<Self, IoError> {
        Self::start_with(
            port,
            options,
            || gstreamer::init().map_err(|e| e.to_string()),
            None,
        )
    }

    pub(crate) fn start_with(
        mut port: IoPort,
        options: IoOptions,
        init: impl FnOnce() -> Result<(), String>,
        // Tests: these audio devices, without a monitor.
        audio_devices: Option<AudioDeviceList>,
    ) -> Result<Self, IoError> {
        let monitor_audio = audio_devices.is_none();
        let gstreamer = init();
        if let Err(e) = &gstreamer {
            tracing::error!(error = %e, "GStreamer unavailable: capture and file inputs disabled");
        }
        // Loads the NDI runtime once, before the device monitors start (ADR 0008 § 3).
        let ndi = match &gstreamer {
            Ok(()) => vma_io_ndi::status(),
            Err(e) => NdiStatus::Unavailable {
                reason: format!("GStreamer unavailable: {e}"),
            },
        };
        let ndi_sources = NdiSourceList::default();
        // `vma-html` next to VMA, or `VMA_HTML` (ADR 0009 § 6): found once, at start.
        let html = vma_io_html::locate_helper();
        if let HtmlStatus::Unavailable { reason } = &html {
            tracing::info!(%reason, "HTML inputs unavailable");
        }
        let html_host = HtmlHost::new(
            html.clone(),
            HtmlFeedsOf {
                feeds: port.feeds.clone(),
                rate: port.format.rate,
            },
            port.format.resolution,
            port.format.rate,
            port.html_ticks.clone(),
        );
        let stop = Arc::new(AtomicBool::new(false));
        let devices = DeviceList::default();
        let infos = Infos::default();
        let activities = Activities::default();
        let outputs = Outputs::new(
            port.outputs.clone(),
            OutputFormat {
                rate: port.format.rate,
                resolution: port.format.resolution,
            },
            gstreamer.clone().err(),
            Arc::clone(&activities),
        );
        let audio_devices = audio_devices.unwrap_or_default();
        let headphones_status = Arc::new(ArcSwap::from_pointee(HeadphonesStatus::default()));
        let headphones = match (port.headphones.take(), &gstreamer) {
            (Some(ring), Ok(())) if options.headphones => Some(Headphones::new(
                ring,
                audio_devices.clone(),
                Arc::clone(&headphones_status),
            )?),
            _ => None,
        };
        let worker = Worker {
            port,
            outputs,
            gstreamer: gstreamer.clone(),
            pipelines: Vec::new(),
            audio_inputs: Vec::new(),
            timelines_at: Instant::now(),
            devices: devices.clone(),
            monitor: None,
            audio_devices: audio_devices.clone(),
            audio_monitor: None,
            monitor_audio,
            ndi: ndi.clone(),
            ndi_sources: ndi_sources.clone(),
            ndi_monitor: None,
            html: html_host,
            headphones,
            devices_at: Instant::now(),
            infos: Arc::clone(&infos),
        };
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("vma-io".into())
            .spawn(move || worker.run(&thread_stop))?;
        Ok(Self {
            stop,
            thread: Some(thread),
            gstreamer,
            devices,
            audio_devices,
            ndi,
            ndi_sources,
            html,
            headphones: headphones_status,
            infos,
            activities,
        })
    }

    /// Why GStreamer could not be initialised, if it could not.
    pub fn gstreamer_error(&self) -> Option<&str> {
        self.gstreamer.as_ref().err().map(String::as_str)
    }

    /// The capture devices connected now (empty without GStreamer). Cheap: the list is kept
    /// up to date by `vma-io`.
    pub fn devices(&self) -> Vec<CaptureDevice> {
        self.devices.infos()
    }

    /// The audio devices connected now: microphones and line inputs, headphones and
    /// speakers (empty without GStreamer).
    pub fn audio_devices(&self) -> AudioDevices {
        self.audio_devices.infos()
    }

    /// Whether NDI® inputs and outputs can be used (found once, at start: restart VMA after
    /// installing the runtime).
    pub fn ndi(&self) -> &NdiStatus {
        &self.ndi
    }

    /// Whether HTML inputs can be used (`vma-html` found at start).
    pub fn html(&self) -> &HtmlStatus {
        &self.html
    }

    /// The NDI® names of the sources seen on the network now (empty without the runtime).
    pub fn ndi_sources(&self) -> Vec<String> {
        self.ndi_sources.names()
    }

    /// What the headphones are doing (default status when they are off).
    pub fn headphones(&self) -> HeadphonesStatus {
        HeadphonesStatus::clone(&self.headphones.load())
    }

    /// What a running output is doing (`None` while it does not run).
    pub fn output_activity(&self, output: OutputId) -> Option<OutputActivity> {
        self.activities
            .load()
            .iter()
            .find(|(id, _)| *id == output)
            .map(|(_, a)| *a)
    }

    /// What the pipeline of `input` reports: capture mode, conversion (`None` while it is
    /// not running, or for an input without a pipeline).
    pub fn pipeline_info(&self, input: InputId) -> Option<PipelineInfo> {
        self.infos
            .load()
            .iter()
            .find(|(id, _)| *id == input)
            .map(|(_, i)| i.clone())
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
    outputs: Outputs,
    gstreamer: Result<(), String>,
    pipelines: Vec<PipelineSlot<vma_engine::FeedRegistry>>,
    /// Sound-only inputs (microphones, line inputs).
    audio_inputs: Vec<AudioInputSlot<vma_engine::FeedRegistry>>,
    timelines_at: Instant,
    devices: DeviceList,
    monitor: Option<Monitor>,
    audio_devices: AudioDeviceList,
    audio_monitor: Option<AudioMonitor>,
    /// `false` in tests that set the audio devices themselves.
    monitor_audio: bool,
    ndi: NdiStatus,
    ndi_sources: NdiSourceList,
    ndi_monitor: Option<NdiMonitor>,
    html: HtmlHost<HtmlFeedsOf>,
    headphones: Option<Headphones>,
    devices_at: Instant,
    infos: Infos,
}

impl Worker {
    fn run(mut self, stop: &AtomicBool) {
        if self.gstreamer.is_ok() {
            match Monitor::start(self.devices.clone()) {
                Ok(monitor) => self.monitor = Some(monitor),
                Err(e) => tracing::error!(error = %e, "no device monitor: no capture device"),
            }
            if self.monitor_audio {
                match AudioMonitor::start(self.audio_devices.clone()) {
                    Ok(monitor) => self.audio_monitor = Some(monitor),
                    Err(e) => tracing::error!(error = %e, "no audio device monitor"),
                }
            }
            if self.ndi.is_available() {
                match NdiMonitor::start(self.ndi_sources.clone()) {
                    Ok(monitor) => self.ndi_monitor = Some(monitor),
                    Err(e) => tracing::error!(error = %e, "no NDI® source monitor"),
                }
            }
        }
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
            for slot in &mut self.audio_inputs {
                slot.tick(now, &mut |e| send(events, e));
            }
            self.html.tick(now, &mut |e| send(events, html_event(e)));
            if let Some(headphones) = &mut self.headphones {
                headphones.tick(now);
            }
            self.outputs.tick(now, &mut |e| send(events, e));
            if now >= self.timelines_at {
                self.timelines_at = now + TIMELINE_PERIOD;
                self.publish_timelines();
                self.outputs.publish();
            }
            if now >= self.devices_at {
                self.devices_at = now + DEVICES_PERIOD;
                if let Some(monitor) = &self.monitor {
                    monitor.refresh();
                }
                if let Some(monitor) = &self.audio_monitor {
                    monitor.refresh();
                }
                if let Some(monitor) = &self.ndi_monitor {
                    monitor.refresh();
                }
            }
        }
        // Pipelines and the monitor stop here, on `vma-io`; recordings are finalised first.
        self.outputs.shutdown();
        self.pipelines.clear();
        self.audio_inputs.clear();
        self.headphones = None;
        self.monitor = None;
        self.audio_monitor = None;
        self.ndi_monitor = None;
        tracing::info!("io thread stopped");
    }

    fn handle(&mut self, request: IoRequest) {
        match request {
            IoRequest::Open { input, spec } => self.open(input, &spec),
            IoRequest::Close { input } => {
                self.html.close(input);
                self.pipelines.retain(|p| p.input != input);
                self.audio_inputs.retain(|p| p.input != input);
                self.publish_timelines();
            }
            IoRequest::Play { input } => self.with(input, |s| s.set_playing(true)),
            IoRequest::Pause { input } => self.with(input, |s| s.set_playing(false)),
            IoRequest::Seek { input, ms } => self.with(input, |s| s.seek(ms)),
            IoRequest::SetLoop { input, enabled } => self.with(input, |s| s.set_looping(enabled)),
            IoRequest::StartOutput {
                output,
                run,
                spec,
                bus,
            } => {
                let events = &self.port.events;
                self.outputs
                    .start(output, run, &spec, bus, &mut |e| send(events, e));
            }
            IoRequest::StopOutput { output } => {
                let events = &self.port.events;
                self.outputs.stop(output, &mut |e| send(events, e));
            }
            IoRequest::LoadStinger { slot, file } => {
                tracing::warn!(%slot, file = %file.display(), "stingers not loaded yet");
            }
            IoRequest::UnloadStinger { slot } => tracing::debug!(%slot, "stinger emptied"),
            IoRequest::Html { input, action } => {
                let action = match action {
                    HtmlAction::Data(json) => PageAction::Script(data_event_script(&json)),
                    HtmlAction::Script(code) => PageAction::Script(code),
                    HtmlAction::Reload => PageAction::Reload,
                    HtmlAction::Navigate(url) => PageAction::Navigate(url),
                    HtmlAction::Back => PageAction::Back,
                    HtmlAction::Forward => PageAction::Forward,
                };
                self.html.page(input, action);
            }
            IoRequest::SetMonitor { device } => match &mut self.headphones {
                Some(headphones) => headphones.set_device(device),
                None => tracing::debug!("headphones off: device not played"),
            },
        }
    }

    fn with(
        &mut self,
        input: InputId,
        f: impl FnOnce(&mut PipelineSlot<vma_engine::FeedRegistry>),
    ) {
        match self.pipelines.iter_mut().find(|p| p.input == input) {
            Some(slot) => f(slot),
            None => tracing::debug!(%input, "playback request for an input without a pipeline"),
        }
    }

    fn publish_timelines(&self) {
        let all: Vec<(InputId, Timeline)> = self
            .pipelines
            .iter()
            .filter_map(|p| p.timeline().map(|t| (p.input, t)))
            .collect();
        if all != *self.port.timelines.load() {
            self.port.timelines.store(all);
        }
        let infos: Vec<(InputId, PipelineInfo)> = self
            .pipelines
            .iter()
            .filter_map(|p| p.info().map(|i| (p.input, i)))
            .collect();
        if infos != **self.infos.load() {
            self.infos.store(Arc::new(infos));
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
            InputSpec::Capture { .. }
            | InputSpec::File { .. }
            | InputSpec::Synthetic { .. }
            | InputSpec::SyncSource { .. }
            | InputSpec::AudioDevice { .. }
            | InputSpec::Srt(_)
                if self.gstreamer.is_err() =>
            {
                let e = self.gstreamer.clone().err().unwrap_or_default();
                lost(IoError::GstreamerUnavailable(e).to_string());
            }
            InputSpec::File { path } => {
                tracing::info!(%input, path = %path.display(), "opening video file");
                self.pipelines.push(PipelineSlot::new(
                    input,
                    Recipe::file(path.clone()),
                    self.port.feeds.clone(),
                    Instant::now(),
                ));
            }
            InputSpec::Synthetic { rate, resolution } => {
                self.pipelines.push(PipelineSlot::new(
                    input,
                    Recipe::synthetic(*rate, *resolution, self.port.ticks),
                    self.port.feeds.clone(),
                    Instant::now(),
                ));
            }
            InputSpec::SyncSource {
                rate,
                sample_rate,
                drift_ppm,
                audio_lead_ms,
                resolution,
            } => {
                self.pipelines.push(PipelineSlot::new(
                    input,
                    Recipe::sync_source(
                        crate::sync_source::SyncSpec {
                            rate: *rate,
                            sample_rate: *sample_rate,
                            drift_ppm: *drift_ppm,
                            audio_lead_ms: *audio_lead_ms,
                            size: resolution
                                .map_or(crate::sync_source::SMALL, |r| (r.width(), r.height())),
                        },
                        self.port.ticks,
                    ),
                    self.port.feeds.clone(),
                    Instant::now(),
                ));
            }
            InputSpec::Capture { device } => {
                tracing::info!(%input, device = %device.0, "opening capture device");
                let capture = Capture {
                    device: device.clone(),
                    devices: self.devices.clone(),
                    resolution: self.port.format.resolution,
                    rate: self.port.format.rate,
                };
                self.pipelines.push(PipelineSlot::new(
                    input,
                    Recipe::capture(capture, self.port.ticks),
                    self.port.feeds.clone(),
                    Instant::now(),
                ));
            }
            InputSpec::AudioDevice { device } => {
                tracing::info!(%input, device = %device.0, "opening audio device");
                self.audio_inputs.push(AudioInputSlot::new(
                    input,
                    device.clone(),
                    self.audio_devices.clone(),
                    self.port.feeds.clone(),
                    Instant::now(),
                ));
            }
            InputSpec::Srt(source) => {
                tracing::info!(%input, uri = %source.uri(), mode = ?source.mode, "opening SRT input");
                self.pipelines.push(PipelineSlot::new(
                    input,
                    Recipe::srt(source.clone(), self.port.ticks),
                    self.port.feeds.clone(),
                    Instant::now(),
                ));
            }
            InputSpec::Ndi { name } => match &self.ndi {
                NdiStatus::Available => {
                    tracing::info!(%input, %name, "opening NDI® input");
                    self.pipelines.push(PipelineSlot::new(
                        input,
                        Recipe::ndi(name.clone(), self.port.ticks),
                        self.port.feeds.clone(),
                        Instant::now(),
                    ));
                }
                // Refused with its reason, no new attempt (ADR 0008 § 3).
                NdiStatus::Unavailable { reason } => lost(reason.clone()),
            },
            InputSpec::Html { url, size, css } => {
                let events = &self.port.events;
                self.html
                    .open(input, url.expose(), *size, css.as_deref(), &mut |e| {
                        send(events, html_event(e))
                    });
            }
            InputSpec::Color { .. } | InputSpec::TestSource { .. } => {}
        }
    }
}

fn html_event(event: HtmlEvent) -> IoEvent {
    match event {
        HtmlEvent::Lost {
            input,
            attempts,
            reason,
        } => IoEvent::SignalLost {
            input,
            attempts,
            reason,
        },
    }
}

fn open_still(
    feeds: &vma_engine::FeedRegistry,
    input: InputId,
    path: &std::path::Path,
) -> Result<(), IoError> {
    let StillImage {
        resolution,
        rgba,
        alpha,
    } = still::decode_file(path)?;
    let mut sink = Feeds::create(
        feeds,
        input,
        VideoFormat {
            pixel: StillImage::FORMAT,
            resolution,
            color: ColorInfo::RGB_FULL,
            alpha,
            rate: None,
        },
    )?;
    sink.push_packed(&rgba, 0)
        .map_err(|e| IoError::Pipeline(e.to_string()))?;
    tracing::info!(%input, path = %path.display(), %resolution, ?alpha, "still image loaded");
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
                other => panic!("{other:?}"),
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

    /// A camera unplugged (its pipeline fails, it leaves the device list) is retried with
    /// "not connected" until it is plugged back, recognised by its identifier, and live again
    /// through a new feed.
    #[test]
    fn unplugged_camera_comes_back_by_its_identifier() {
        use crate::devices::DeviceList;
        use crate::pipeline::tests::{WEBCAM_CAPS, capture_recipe, fake_camera};
        let camera = fake_camera(
            "/dev/video3",
            "videotestsrc is-live=true ! identity error-after=15 ! jpegenc ! capsfilter caps=image/jpeg",
            WEBCAM_CAPS,
        );
        let devices = DeviceList::default();
        devices.set(vec![camera.clone()]);
        let feeds = FakeFeeds::default();
        let mut slot = PipelineSlot::new(
            InputId(6),
            capture_recipe(&camera.info.id.0, &devices, vma_core::Resolution::HD_720),
            feeds.clone(),
            Instant::now(),
        );
        let start = Instant::now();
        let mut losses: Vec<(u32, String)> = Vec::new();
        while feeds.count() < 2 && start.elapsed() < Duration::from_secs(10) {
            slot.tick(Instant::now(), &mut |e| match e {
                IoEvent::SignalLost {
                    attempts, reason, ..
                } => losses.push((attempts, reason)),
                other => panic!("{other:?}"),
            });
            match losses.len() {
                // First loss (the "unplug"): the camera disappears.
                1 => devices.set(Vec::new()),
                // Retried while it is away, then it comes back.
                2 => devices.set(vec![camera.clone()]),
                _ => {}
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            feeds.count(),
            2,
            "live again through a new feed: {losses:?}"
        );
        assert_eq!(losses.len(), 2, "{losses:?}");
        assert_eq!(losses[1].0, 2);
        assert!(losses[1].1.contains("is not connected"), "{losses:?}");
        assert!(slot.info().and_then(|i| i.mode).is_some());
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
                other => panic!("{other:?}"),
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
        let io = IoRuntime::start_with(
            engine.take_io_port().unwrap(),
            IoOptions::default(),
            || Err("simulated".into()),
            None,
        )
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

    /// C10: without the NDI® runtime, an NDI input is lost with the reason at once and never
    /// tried again; the other inputs are not affected; no source is listed.
    #[test]
    fn without_the_ndi_runtime_an_ndi_input_is_refused_with_its_reason() {
        use vma_engine::{Command, EngineConfig, InputStatus, Journal, RenderEngine, Source};
        if vma_io_ndi::status().is_available() {
            return;
        }
        let gpu = pollster::block_on(vma_compositor::headless::request_gpu())
            .expect("GPU adapter required");
        let config = EngineConfig {
            resolution: vma_core::Resolution::new(320, 180).unwrap(),
            ..EngineConfig::default()
        };
        let (mut engine, _readers) =
            RenderEngine::spawn(gpu.device, gpu.queue, config, Journal::Disabled).unwrap();
        let io = IoRuntime::start(engine.take_io_port().unwrap()).unwrap();
        let NdiStatus::Unavailable { reason } = io.ndi().clone() else {
            unreachable!()
        };
        assert!(reason.contains("NDI® runtime not found"), "{reason}");
        let commands = engine.commands();
        for spec in [
            InputSpec::Ndi {
                name: "STUDIO (Caméra 1)".into(),
            },
            InputSpec::Synthetic {
                rate: vma_core::FrameRate::FPS_25,
                resolution: vma_core::Resolution::new(320, 180).unwrap(),
            },
        ] {
            commands
                .send(Source::Script, Command::AddInput { spec, name: None })
                .unwrap();
        }
        let state = engine.state();
        let end = Instant::now() + Duration::from_secs(10);
        let done = |s: &vma_engine::State| {
            s.inputs.len() == 2
                && matches!(
                    s.inputs[0].status,
                    InputStatus::SignalLost { attempts: 0, .. }
                )
                && matches!(s.inputs[1].status, InputStatus::Live { .. })
        };
        while !done(&state.load()) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(done(&state.load()), "{:?}", state.load().inputs);
        // No new attempt: still lost, still 0 attempts, 3 s later (the policy retries at 1 s).
        std::thread::sleep(Duration::from_secs(3));
        assert!(done(&state.load()), "{:?}", state.load().inputs);
        assert!(io.ndi_sources().is_empty());
    }

    /// C12 (CI side, fake devices): a microphone chosen by its identifier goes live; the
    /// headphones play on the device chosen by command; the muted microphone in PFL solo is
    /// silent in the Master and heard in the headphones.
    #[test]
    fn a_microphone_and_the_headphones_through_the_engine() {
        use crate::audio_devices::AudioDeviceList;
        use crate::pipeline::tests::fake_audio_device;
        use vma_engine::{
            Command, EngineConfig, InputStatus, Journal, Level, RenderEngine, SoloMode, Source,
        };

        let mic = fake_audio_device(
            "Audio/Source",
            "alsa_input.test-mic",
            "audiotestsrc is-live=true samplesperbuffer=480 freq=1000 volume=0.5",
        );
        let phones = fake_audio_device(
            "Audio/Sink",
            "alsa_output.test-phones",
            "fakesink sync=true",
        );
        let list = AudioDeviceList::default();
        list.set(vec![mic.clone()], vec![phones.clone()]);

        let gpu = pollster::block_on(vma_compositor::headless::request_gpu())
            .expect("GPU adapter required");
        let config = EngineConfig {
            resolution: vma_core::Resolution::new(320, 180).unwrap(),
            stream_clock: Some(crate::stream_clock()),
            ..EngineConfig::default()
        };
        let (mut engine, _readers) =
            RenderEngine::spawn(gpu.device, gpu.queue, config, Journal::Disabled).unwrap();
        let io = IoRuntime::start_with(
            engine.take_io_port().unwrap(),
            IoOptions { headphones: true },
            || gstreamer::init().map_err(|e| e.to_string()),
            Some(list),
        )
        .unwrap();
        assert_eq!(io.audio_devices().sources, std::slice::from_ref(&mic.info));
        let commands = engine.commands();
        let send = |command| commands.send(Source::Script, command).unwrap();
        send(Command::SetMonitor {
            device: Some(phones.info.id.clone()),
            solo_mode: SoloMode::Pfl,
        });
        send(Command::AddInput {
            spec: InputSpec::AudioDevice {
                device: mic.info.id.clone(),
            },
            name: Some("Micro".into()),
        });
        let state = engine.state();
        let wait = |what: &str, done: &dyn Fn() -> bool| {
            let end = Instant::now() + Duration::from_secs(10);
            while !done() {
                assert!(Instant::now() < end, "{what}: {:?}", state.load().inputs);
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        wait("microphone live", &|| {
            state
                .load()
                .inputs
                .first()
                .is_some_and(|i| i.status == InputStatus::Live { source: None })
        });
        wait("headphones on their device", &|| io.headphones().playing);
        assert_eq!(
            io.headphones().device.as_deref(),
            Some("Son de test alsa_output.test-phones")
        );
        let input = state.load().inputs[0].id;
        let meters = engine.audio_meters();
        let peak = |level| {
            meters
                .read(level)
                .map_or(0.0, |[_, post]: [vma_engine::MeterReading; 2]| {
                    post.peak[0].max(post.peak[1])
                })
        };
        // Unmuted: in the Master (and the headphones, without a solo).
        send(Command::SetInputMute { input, mute: false });
        wait("microphone in the Master", &|| {
            (peak(Level::Bus(vma_core::AudioBus::Master)) - 0.5).abs() < 0.01
        });
        // Muted and in PFL solo: out of the Master, in the headphones.
        send(Command::SetInputMute { input, mute: true });
        send(Command::SetInputSolo { input, solo: true });
        wait("solo in the headphones only", &|| {
            peak(Level::Bus(vma_core::AudioBus::Master)) == 0.0
                && (peak(Level::Headphones) - 0.5).abs() < 0.01
        });
        drop(io);
    }
}
