//! One GStreamer pipeline feeding one input (spec `io-gst.md`, « Transport des images »):
//! `<source> ! videoconvert ! appsink`, the appsink limited to the formats the compositor
//! uploads as is (NV12, I420, YUY2, UYVY, RGBA, BGRA). `videoconvert` stays in passthrough
//! for those; any other format is converted (counted).
//!
//! The appsink callback runs on the GStreamer streaming thread: it maps the buffer and pushes
//! its planes into the input's feed (a mapped transfer buffer there, ADR 0004 amendment 1),
//! creating the feed when the format is first known or changes. Bus errors and end of stream
//! reach [`GstInput::poll`] through a sync handler; a watchdog notices a source that stops
//! sending.
//!
//! A video file (`uridecodebin`, « Lecture des fichiers vidéo ») opens paused on its first
//! picture. Every seek is a *segment* seek: at the end the pipeline posts `SEGMENT_DONE`
//! instead of an end of stream. With Loop, the sync handler answers it at once with a
//! non-flushing seek back to 0, so the next loop is queued behind the last pictures of this
//! one (no gap, no black). Without Loop nothing more comes: the feed keeps the last picture.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::*;
use vma_audio::RingWriter;
use vma_clock::TickGrid;
use vma_compositor::Plane;
use vma_core::{ColorInfo, FrameRate, InputId, Matrix, PixelFormat, Range, Resolution};
use vma_engine::{DeviceId, Timeline};

use crate::IoError;
use crate::devices::DeviceList;
use crate::feeds::{Feeds, PictureOut, VideoFormat};
use crate::mode::{self, CaptureMode, Encoding};

/// A source that has not produced its first picture after this long is lost.
pub(crate) const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
/// A running source is lost after `max(MIN_SILENCE, 10 periods)` without a picture.
pub(crate) const MIN_SILENCE: Duration = Duration::from_millis(500);
/// A file has ended once its segment is done and no picture came for
/// `max(END_SETTLE, 3 periods)`: the pictures still queued are shown first.
const END_SETTLE: Duration = Duration::from_millis(100);

/// The formats accepted by the appsink, in order of preference.
const ACCEPTED: [gst_video::VideoFormat; 6] = [
    gst_video::VideoFormat::Nv12,
    gst_video::VideoFormat::I420,
    gst_video::VideoFormat::Yuy2,
    gst_video::VideoFormat::Uyvy,
    gst_video::VideoFormat::Rgba,
    gst_video::VideoFormat::Bgra,
];

fn pixel_format(format: gst_video::VideoFormat) -> Option<PixelFormat> {
    Some(match format {
        gst_video::VideoFormat::Nv12 => PixelFormat::Nv12,
        gst_video::VideoFormat::I420 => PixelFormat::I420,
        gst_video::VideoFormat::Yuy2 => PixelFormat::Yuy2,
        gst_video::VideoFormat::Uyvy => PixelFormat::Uyvy,
        gst_video::VideoFormat::Rgba => PixelFormat::Rgba,
        gst_video::VideoFormat::Bgra => PixelFormat::Bgra,
        _ => return None,
    })
}

/// Format of negotiated caps; colorimetry from the caps, or the default rule when unknown.
pub(crate) fn video_format(info: &gst_video::VideoInfo) -> Option<VideoFormat> {
    let pixel = pixel_format(info.format())?;
    let resolution = Resolution::new(info.width(), info.height()).ok()?;
    let assumed = ColorInfo::assumed(pixel, info.height());
    let color = if pixel.is_yuv() {
        let c = info.colorimetry();
        ColorInfo {
            matrix: match c.matrix() {
                gst_video::VideoColorMatrix::Bt601 => Matrix::Bt601,
                gst_video::VideoColorMatrix::Bt709 => Matrix::Bt709,
                _ => assumed.matrix,
            },
            range: match c.range() {
                gst_video::VideoColorRange::Range0_255 => Range::Full,
                gst_video::VideoColorRange::Range16_235 => Range::Limited,
                _ => assumed.range,
            },
        }
    } else {
        ColorInfo::RGB_FULL
    };
    let fps = info.fps();
    let rate = u32::try_from(fps.numer())
        .ok()
        .zip(u32::try_from(fps.denom()).ok())
        .and_then(|(n, d)| FrameRate::new(n, d).ok());
    Some(VideoFormat {
        pixel,
        resolution,
        color,
        rate,
    })
}

/// What the streaming thread shares with [`GstInput`].
#[derive(Debug)]
struct Shared {
    start: Instant,
    /// Nanoseconds since `start` of the last picture, + 1 (0 = none yet).
    last_picture: AtomicU64,
    pictures: AtomicU64,
    /// Nanoseconds per source frame (0 = unknown).
    period_ns: AtomicU64,
    converting: AtomicBool,
    /// Video files: loop at the end of the segment.
    looping: AtomicBool,
    /// Video files: the last segment was pushed without Loop (pictures may still be queued).
    segment_done: AtomicBool,
}

impl Shared {
    fn since_start(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    /// Time since the last picture (since the start if none came).
    fn silence(&self, now: Instant) -> Duration {
        let since = now.saturating_duration_since(self.start);
        match self.last_picture.load(Ordering::Acquire) {
            0 => since,
            last => since.saturating_sub(Duration::from_nanos(last - 1)),
        }
    }
}

/// Where the pictures of a pipeline come from.
#[derive(Debug, Clone)]
pub(crate) enum Media {
    /// `gst-launch` syntax of the source part, up to (excluded) the conversion and the sink
    /// (synthetic sources).
    Launch(String),
    /// A video file, with a timeline: `uridecodebin`, the first video stream to the sink,
    /// every other stream to a `fakesink` (audio: Phase 3).
    File(PathBuf),
    /// A capture device, looked up by its stable identifier at every (re)start.
    Capture(Capture),
    /// The sync source of the tests (`sync_source`).
    Sync(crate::sync_source::SyncSpec),
    /// An SRT stream (MPEG-TS): `uridecodebin` on `srt://`, live.
    Srt(vma_engine::SrtSource),
    /// An NDI® source, by its name: `ndisrc ! ndisrcdemux`, live.
    Ndi(String),
}

/// A capture device to open in the mode closest to the master format.
#[derive(Debug, Clone)]
pub(crate) struct Capture {
    pub(crate) device: DeviceId,
    pub(crate) devices: DeviceList,
    pub(crate) resolution: Resolution,
    pub(crate) rate: FrameRate,
}

/// How a pipeline is built and supervised.
#[derive(Debug, Clone)]
pub(crate) struct Recipe {
    pub(crate) media: Media,
    /// `true`: the appsink paces by the clock (files); `false`: as fast as pictures come.
    pub(crate) sync: bool,
    /// Whether silence means a lost signal (live sources; not a paused file).
    pub(crate) watchdog: bool,
    /// Waits for its first picture without a limit (an SRT listener waits for its caller).
    pub(crate) patient: bool,
    /// Pictures released just before the master tick they are due on ([`Pacer`]): a network
    /// source hands them out in bursts.
    pub(crate) pace: Option<TickGrid>,
}

impl Recipe {
    /// A video file, played at its own pace, paused or ended without being lost.
    pub(crate) fn file(path: PathBuf) -> Self {
        Self {
            media: Media::File(path),
            sync: true,
            watchdog: false,
            patient: false,
            pace: None,
        }
    }

    /// A synthetic live source: `videotestsrc` at `rate` and `resolution`, in NV12.
    pub(crate) fn synthetic(rate: FrameRate, resolution: Resolution) -> Self {
        Self {
            media: Media::Launch(format!(
                "videotestsrc is-live=true pattern=ball \
                 ! video/x-raw,format=NV12,width={},height={},framerate={}/{}",
                resolution.width(),
                resolution.height(),
                rate.num(),
                rate.den()
            )),
            sync: false,
            watchdog: true,
            patient: false,
            pace: None,
        }
    }

    /// The sync source: live, pictures and sound taken as they come.
    pub(crate) fn sync_source(spec: crate::sync_source::SyncSpec) -> Self {
        Self {
            media: Media::Sync(spec),
            sync: false,
            watchdog: true,
            patient: false,
            pace: None,
        }
    }

    /// A capture device: live (lost when silent), pictures taken as they come.
    pub(crate) fn capture(capture: Capture) -> Self {
        Self {
            media: Media::Capture(capture),
            sync: false,
            watchdog: true,
            patient: false,
            pace: None,
        }
    }

    /// An SRT input: live, lost when silent; a listener waits for its caller as long as it
    /// takes.
    pub(crate) fn srt(source: vma_engine::SrtSource, ticks: TickGrid) -> Self {
        Self {
            patient: source.mode == vma_engine::SrtMode::Listener,
            media: Media::Srt(source),
            sync: false,
            watchdog: true,
            pace: Some(ticks),
        }
    }

    /// An NDI® input: live, lost when silent; a network source, paced like SRT.
    pub(crate) fn ndi(name: String, ticks: TickGrid) -> Self {
        Self {
            media: Media::Ndi(name),
            sync: false,
            watchdog: true,
            patient: false,
            pace: Some(ticks),
        }
    }

    fn has_timeline(&self) -> bool {
        matches!(self.media, Media::File(_))
    }
}

/// What the bus tells [`GstInput::poll`].
#[derive(Debug)]
enum Note {
    Lost(String),
    EndOfStream,
    /// The pipeline reached its target state (prerolled, for a file).
    AsyncDone,
}

/// Playback of a video file, on `vma-io`.
#[derive(Debug)]
struct Player {
    /// The first segment seek is done (after the preroll).
    seeded: bool,
    /// Where the first seek goes (a position asked before the preroll).
    start_ms: u64,
    playing: bool,
    /// The last picture was shown and nothing more comes (no Loop).
    ended: bool,
}

/// A running pipeline. Dropping it stops it (state `NULL`).
#[derive(Debug)]
pub(crate) struct GstInput {
    pipeline: gst::Pipeline,
    shared: Arc<Shared>,
    notes: Receiver<Note>,
    watchdog: bool,
    /// Waits for its first picture without a limit.
    patient: bool,
    /// `Some` for a video file.
    player: Option<Player>,
    /// `Some` for a capture device: the mode it was opened in.
    mode: Option<CaptureMode>,
    /// `Some` for the sync source: its generator thread (stopped before the pipeline).
    generator: Option<crate::sync_source::Generator>,
}

impl GstInput {
    /// Builds and starts the pipeline of `recipe` for `input`; pictures go to `feeds`.
    pub(crate) fn start<F: Feeds>(
        input: InputId,
        recipe: &Recipe,
        feeds: F,
    ) -> Result<Self, IoError> {
        let mut mode = None;
        let mut sync_source = None;
        let (pipeline, convert, sink) = match &recipe.media {
            Media::Sync(spec) => {
                let (pipeline, convert, sink, video, audio) = crate::sync_source::pipeline(spec)?;
                let pad = audio
                    .static_pad("src")
                    .ok_or_else(|| IoError::Pipeline("appsrc without a pad".into()))?;
                match feeds.audio(input) {
                    Some(ring) => crate::audio::link(&pipeline, &pad, ring, false)
                        .map_err(IoError::Pipeline)?,
                    None => discard(&pipeline, &pad),
                }
                sync_source = Some((video, audio, *spec));
                (pipeline, convert, sink)
            }
            Media::Launch(source) => launch_pipeline(source)?,
            Media::File(path) => file_pipeline(path, feeds.audio(input))?,
            Media::Srt(source) => srt_pipeline(source, feeds.audio(input))?,
            Media::Ndi(name) => ndi_pipeline(name, feeds.audio(input))?,
            Media::Capture(capture) => {
                let (source, chosen) = open_device(capture)?;
                let parts = capture_pipeline(source, &chosen)?;
                tracing::info!(%input, device = %capture.device.0, mode = %chosen, "capture mode");
                mode = Some(chosen);
                parts
            }
        };
        // Pictures and sound dated in the master clock's time (ADR 0007 § 1).
        pipeline.use_clock(Some(&crate::clock::master_clock()));
        sink.set_caps(Some(
            &gst_video::VideoCapsBuilder::new()
                .format_list(ACCEPTED)
                .build(),
        ));
        sink.set_max_buffers(1);
        sink.set_drop(true);
        sink.set_sync(recipe.sync);

        let shared = Arc::new(Shared {
            start: Instant::now(),
            last_picture: AtomicU64::new(0),
            pictures: AtomicU64::new(0),
            period_ns: AtomicU64::new(0),
            converting: AtomicBool::new(false),
            looping: AtomicBool::new(false),
            segment_done: AtomicBool::new(false),
        });
        let (notes_tx, notes) = crossbeam_channel::bounded(16);
        if let Some(bus) = pipeline.bus() {
            let weak = pipeline.downgrade();
            let relay_shared = Arc::clone(&shared);
            bus.set_sync_handler(move |_, message| {
                if let gst::MessageView::SegmentDone(_) = message.view() {
                    segment_done(&weak, &relay_shared);
                } else {
                    relay(&notes_tx, message);
                }
                gst::BusSyncReply::Drop
            });
        }
        let timeline = recipe.has_timeline();
        let stream = Arc::new(std::sync::Mutex::new(Stream {
            input,
            feeds,
            out: None,
            next_frame: 0,
            timeline,
            preroll_pts: None,
            shared: Arc::clone(&shared),
            convert,
            pacer: None,
            failed: false,
        }));
        if let Some(ticks) = recipe.pace {
            let (to_release, released) = crossbeam_channel::bounded(PACE_QUEUE);
            let clock = crate::clock::master_clock();
            spawn_release(input, Arc::downgrade(&stream), released, clock.clone())?;
            if let Ok(mut s) = stream.lock() {
                s.pacer = Some(Paced {
                    pacer: Pacer::new(ticks),
                    clock,
                    to_release,
                });
            }
        }
        let preroll = Arc::clone(&stream);
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| on_picture(&stream, sink, false))
                .new_preroll(move |sink| on_picture(&preroll, sink, true))
                .build(),
        );
        // A file opens paused on its first picture; a live source plays at once.
        let target = if timeline {
            gst::State::Paused
        } else {
            gst::State::Playing
        };
        if let Err(e) = pipeline.set_state(target) {
            // Never drop a pipeline outside `NULL` (a missing file fails right here).
            let _ = pipeline.set_state(gst::State::Null);
            return Err(IoError::Pipeline(format!("cannot start: {e}")));
        }
        let generator = match sync_source {
            Some((video, audio, spec)) => {
                match crate::sync_source::start(&pipeline, video, audio, spec) {
                    Ok(g) => Some(g),
                    Err(e) => {
                        let _ = pipeline.set_state(gst::State::Null);
                        return Err(e);
                    }
                }
            }
            None => None,
        };
        Ok(Self {
            generator,
            pipeline,
            shared,
            notes,
            watchdog: recipe.watchdog,
            patient: recipe.patient,
            player: timeline.then_some(Player {
                seeded: false,
                start_ms: 0,
                playing: false,
                ended: false,
            }),
            mode,
        })
    }

    /// The mode a capture device was opened in.
    pub(crate) fn capture_mode(&self) -> Option<&CaptureMode> {
        self.mode.as_ref()
    }

    /// `Some(reason)` once the input is lost: bus error, end of a live stream, or silence.
    /// Also performs the first seek of a video file once it is prerolled.
    pub(crate) fn poll(&mut self, now: Instant) -> Option<String> {
        while let Ok(note) = self.notes.try_recv() {
            match (note, &mut self.player) {
                (Note::Lost(reason), _) => return Some(reason),
                (Note::EndOfStream, None) => return Some("end of stream".into()),
                // A file whose segment seek failed: its end is an end, not a loss.
                (Note::EndOfStream, Some(player)) => player.ended = true,
                (Note::AsyncDone, Some(player)) if !player.seeded => {
                    player.seeded = true;
                    let start = player.start_ms;
                    let playing = player.playing;
                    self.seek_now(start);
                    if playing {
                        self.set_state(gst::State::Playing);
                    }
                }
                (Note::AsyncDone, _) => {}
            }
        }
        if let Some(player) = &mut self.player
            && !player.ended
            && self.shared.segment_done.load(Ordering::Relaxed)
        {
            let period = Duration::from_nanos(self.shared.period_ns.load(Ordering::Relaxed));
            player.ended = self.shared.silence(now) > END_SETTLE.max(period * 3);
        }
        if !self.watchdog {
            return None;
        }
        let since = now.saturating_duration_since(self.shared.start);
        match self.shared.last_picture.load(Ordering::Acquire) {
            0 if since > STARTUP_TIMEOUT && !self.patient => {
                Some(format!("no picture after {STARTUP_TIMEOUT:?}"))
            }
            0 => None,
            last => {
                let period = Duration::from_nanos(self.shared.period_ns.load(Ordering::Relaxed));
                let limit = MIN_SILENCE.max(period * 10);
                let silence = since.saturating_sub(Duration::from_nanos(last - 1));
                (silence > limit).then(|| format!("no picture for {silence:?}"))
            }
        }
    }

    /// Video file: plays (from the start if it had ended) or pauses on the current picture.
    pub(crate) fn set_playing(&mut self, playing: bool) {
        let Some(player) = &mut self.player else {
            return;
        };
        player.playing = playing;
        if !player.seeded {
            return;
        }
        if playing && player.ended {
            self.seek_now(0);
        }
        self.set_state(if playing {
            gst::State::Playing
        } else {
            gst::State::Paused
        });
    }

    /// Video file: accurate seek to `ms` (paused: the picture there is shown).
    pub(crate) fn seek(&mut self, ms: u64) {
        match &mut self.player {
            Some(player) if !player.seeded => player.start_ms = ms,
            Some(_) => self.seek_now(ms),
            None => {}
        }
    }

    /// Video file: loop at the end, or stop on the last picture.
    pub(crate) fn set_looping(&self, looping: bool) {
        self.shared.looping.store(looping, Ordering::Relaxed);
    }

    /// Video file: position, duration, end reached.
    pub(crate) fn timeline(&self) -> Option<Timeline> {
        self.player.as_ref()?;
        let ms = |t: gst::ClockTime| t.mseconds();
        Some(Timeline {
            position_ms: self
                .pipeline
                .query_position::<gst::ClockTime>()
                .map_or(0, ms),
            duration_ms: self.pipeline.query_duration::<gst::ClockTime>().map(ms),
            ended: self.player.as_ref().is_some_and(|p| p.ended),
        })
    }

    fn seek_now(&mut self, ms: u64) {
        self.shared.segment_done.store(false, Ordering::Relaxed);
        if let Some(player) = &mut self.player {
            player.ended = false;
        }
        if let Err(e) = segment_seek(&self.pipeline, ms, true) {
            tracing::warn!(error = %e, ms, "seek failed");
        }
    }

    fn set_state(&self, state: gst::State) {
        if let Err(e) = self.pipeline.set_state(state) {
            tracing::warn!(error = %e, ?state, "state change failed");
        }
    }

    /// Pictures delivered so far.
    pub(crate) fn pictures(&self) -> u64 {
        self.shared.pictures.load(Ordering::Relaxed)
    }

    /// Whether `videoconvert` converts (the source format is not uploaded as is).
    pub(crate) fn converting(&self) -> bool {
        self.shared.converting.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn element(&self, name: &str) -> Option<gst::Element> {
        self.pipeline.by_name(name)
    }
}

impl Drop for GstInput {
    fn drop(&mut self) {
        // The generator pushes into the pipeline: stopped first.
        self.generator = None;
        if let Err(e) = self.pipeline.set_state(gst::State::Null) {
            tracing::warn!(error = %e, "pipeline did not stop cleanly");
        }
    }
}

type Parts = (gst::Pipeline, gst::Element, gst_app::AppSink);

/// `<source> ! videoconvert ! appsink`, from `gst-launch` syntax.
fn launch_pipeline(source: &str) -> Result<Parts, IoError> {
    let description = format!("{source} ! videoconvert name=vma_convert ! appsink name=vma_sink");
    let pipeline = gst::parse::launch(&description)
        .map_err(|e| IoError::Pipeline(e.to_string()))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| IoError::Pipeline("not a pipeline".into()))?;
    let sink = pipeline
        .by_name("vma_sink")
        .and_then(|e| e.downcast::<gst_app::AppSink>().ok())
        .ok_or_else(|| IoError::Pipeline("no appsink".into()))?;
    let convert = pipeline
        .by_name("vma_convert")
        .ok_or_else(|| IoError::Pipeline("no videoconvert".into()))?;
    Ok((pipeline, convert, sink))
}

/// `uridecodebin` → (first video stream) `videoconvert ! appsink`; other streams → `fakesink`.
fn file_pipeline(path: &Path, audio: Option<RingWriter>) -> Result<Parts, IoError> {
    let pipeline_error = |e: &dyn std::fmt::Display| IoError::Pipeline(e.to_string());
    let absolute = std::path::absolute(path).map_err(|e| pipeline_error(&e))?;
    let uri = gst::glib::filename_to_uri(&absolute, None).map_err(|e| pipeline_error(&e))?;
    let decode = gst::ElementFactory::make("uridecodebin")
        .property("uri", uri.as_str())
        .build()
        .map_err(|e| pipeline_error(&e))?;
    let convert = gst::ElementFactory::make("videoconvert")
        .name("vma_convert")
        .build()
        .map_err(|e| pipeline_error(&e))?;
    let sink = gst_app::AppSink::builder().name("vma_sink").build();
    let pipeline = gst::Pipeline::new();
    pipeline
        .add_many([&decode, &convert, sink.upcast_ref()])
        .map_err(|e| pipeline_error(&e))?;
    convert.link(&sink).map_err(|e| pipeline_error(&e))?;

    route_decoded(&decode, &pipeline, &convert, audio, true);
    Ok((pipeline, convert, sink))
}

/// An SRT stream through `uridecodebin` (`srtsrc ! tsdemux ! decoders`), live: the sound is
/// taken as it comes. Mode and latency go in the URI (as for the output); the passphrase is
/// set on `srtsrc` (never in the URI nor in a message). `srtsrc` does not reconnect by
/// itself: a lost stream is restarted by the common policy (1, 2, 5, 10 s).
fn srt_pipeline(
    source: &vma_engine::SrtSource,
    audio: Option<RingWriter>,
) -> Result<Parts, IoError> {
    let pipeline_error = |e: &dyn std::fmt::Display| IoError::Pipeline(e.to_string());
    let Some(passphrase) = source.passphrase.expose().map(str::to_owned) else {
        return Err(IoError::Pipeline(
            "the SRT passphrase is missing (read back from a journal): add the input again".into(),
        ));
    };
    let mode = match source.mode {
        vma_engine::SrtMode::Caller => "caller",
        vma_engine::SrtMode::Listener => "listener",
    };
    let uri = format!("{}?mode={mode}&latency={}", source.uri(), source.latency_ms);
    let decode = gst::ElementFactory::make("uridecodebin")
        .property("uri", uri.as_str())
        .build()
        .map_err(|e| pipeline_error(&e))?;
    decode.connect("source-setup", false, move |values| {
        let element = values.get(1).and_then(|v| v.get::<gst::Element>().ok());
        if let Some(src) = element {
            if src.has_property("passphrase") && !passphrase.is_empty() {
                src.set_property("passphrase", passphrase.as_str());
            }
            if src.has_property("auto-reconnect") {
                src.set_property("auto-reconnect", false);
            }
        }
        None
    });
    // By default `tsdemux` maps the clock of the sender onto ours from the PCR and keeps
    // correcting that mapping from the arrival times (skew): the stream times then slide by
    // tens of milliseconds over the first seconds, and the sound and pictures with them,
    // while the jitter buffer keeps a fixed delay. With `ignore-pcr` (GStreamer ≥ 1.18; the
    // `skew-corrections` switch only exists from 1.28), the times come from the PTS, mapped
    // once: a sender clock that drifts is absorbed by the jitter buffer and the sound's rate.
    if let Some(bin) = decode.downcast_ref::<gst::Bin>() {
        bin.connect_deep_element_added(|_, _, element| {
            if element
                .factory()
                .is_some_and(|f| f.name().as_str() == "tsdemux")
            {
                if element.has_property("ignore-pcr") {
                    element.set_property("ignore-pcr", true);
                } else {
                    tracing::warn!("tsdemux without ignore-pcr: the SRT stream times may slide");
                }
            }
        });
    }
    let convert = gst::ElementFactory::make("videoconvert")
        .name("vma_convert")
        .build()
        .map_err(|e| pipeline_error(&e))?;
    let sink = gst_app::AppSink::builder().name("vma_sink").build();
    let pipeline = gst::Pipeline::new();
    pipeline
        .add_many([&decode, &convert, sink.upcast_ref()])
        .map_err(|e| pipeline_error(&e))?;
    convert.link(&sink).map_err(|e| pipeline_error(&e))?;
    route_decoded(&decode, &pipeline, &convert, audio, false);
    Ok((pipeline, convert, sink))
}

/// An NDI® source through `ndisrc ! ndisrcdemux` (spec `io-gst.md`, « Entrée NDI® »): the
/// plugin is registered and the runtime loaded by `vma_io_ndi` before any NDI input opens.
/// A source that is not on the network fails after `connect-timeout`: lost, then the common
/// policy (1, 2, 5, 10 s) tries again by its name.
fn ndi_pipeline(name: &str, audio: Option<RingWriter>) -> Result<Parts, IoError> {
    let pipeline_error = |e: &dyn std::fmt::Display| IoError::Pipeline(e.to_string());
    let source = gst::ElementFactory::make("ndisrc")
        .property("ndi-name", name)
        .property("connect-timeout", 5_000u32)
        .build()
        .map_err(|e| pipeline_error(&e))?;
    let demux = gst::ElementFactory::make("ndisrcdemux")
        .build()
        .map_err(|e| pipeline_error(&e))?;
    let convert = gst::ElementFactory::make("videoconvert")
        .name("vma_convert")
        .build()
        .map_err(|e| pipeline_error(&e))?;
    let sink = gst_app::AppSink::builder().name("vma_sink").build();
    let pipeline = gst::Pipeline::new();
    pipeline
        .add_many([&source, &demux, &convert, sink.upcast_ref()])
        .map_err(|e| pipeline_error(&e))?;
    source.link(&demux).map_err(|e| pipeline_error(&e))?;
    convert.link(&sink).map_err(|e| pipeline_error(&e))?;
    route_decoded(&demux, &pipeline, &convert, audio, false);
    Ok((pipeline, convert, sink))
}

/// Links the pads `decode` creates: the first video stream to `convert`, the first sound
/// stream to `audio` (if any; `sync`: paced by the clock, a file), any other stream to a
/// `fakesink`.
fn route_decoded(
    decode: &gst::Element,
    pipeline: &gst::Pipeline,
    convert: &gst::Element,
    audio: Option<RingWriter>,
    sync: bool,
) {
    let weak_pipeline = pipeline.downgrade();
    let weak_convert = convert.downgrade();
    // Taken by the first sound stream (`pad-added` runs on a streaming thread, rarely).
    let audio = std::sync::Mutex::new(audio);
    decode.connect_pad_added(move |_, pad| {
        let Some(pipeline) = weak_pipeline.upgrade() else {
            return;
        };
        let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
        let is_video = caps
            .structure(0)
            .is_some_and(|s| s.name().starts_with("video/"));
        let is_audio = caps
            .structure(0)
            .is_some_and(|s| s.name().starts_with("audio/"));
        if is_audio && let Some(ring) = audio.lock().ok().and_then(|mut a| a.take()) {
            if let Err(e) = crate::audio::link(&pipeline, pad, ring, sync) {
                tracing::warn!(error = %e, "cannot link the sound stream");
                discard(&pipeline, pad);
            }
            return;
        }
        let free_video = weak_convert
            .upgrade()
            .and_then(|c| c.static_pad("sink"))
            .filter(|p| !p.is_linked());
        match free_video {
            Some(target) if is_video => {
                if let Err(e) = pad.link(&target) {
                    tracing::warn!(error = %e, "cannot link the video stream");
                }
            }
            _ => discard(&pipeline, pad),
        }
    });
}

/// The source element of a connected device and the mode to open it in.
fn open_device(capture: &Capture) -> Result<(gst::Element, CaptureMode), IoError> {
    let device = capture
        .devices
        .find(&capture.device)
        .ok_or_else(|| IoError::Pipeline(format!("{} is not connected", capture.device.0)))?;
    let caps = device.caps().unwrap_or_else(gst::Caps::new_empty);
    let modes = mode::modes(&caps, capture.resolution, capture.rate);
    let chosen = mode::choose(&modes, capture.resolution, capture.rate)
        .ok_or_else(|| IoError::Pipeline(format!("no usable mode in {caps}")))?;
    let source = device
        .create_element(None)
        .map_err(|e| IoError::Pipeline(e.to_string()))?;
    Ok((source, chosen))
}

/// `<device> ! <mode caps> [! decodebin] ! videoconvert ! appsink`: MJPEG is decoded by
/// whatever `decodebin` finds (`jpegdec`, or a hardware decoder).
pub(crate) fn capture_pipeline(source: gst::Element, mode: &CaptureMode) -> Result<Parts, IoError> {
    let pipeline_error = |e: &dyn std::fmt::Display| IoError::Pipeline(e.to_string());
    let filter = gst::ElementFactory::make("capsfilter")
        .property("caps", mode.caps())
        .build()
        .map_err(|e| pipeline_error(&e))?;
    let convert = gst::ElementFactory::make("videoconvert")
        .name("vma_convert")
        .build()
        .map_err(|e| pipeline_error(&e))?;
    let sink = gst_app::AppSink::builder().name("vma_sink").build();
    let pipeline = gst::Pipeline::new();
    pipeline
        .add_many([&source, &filter, &convert, sink.upcast_ref()])
        .map_err(|e| pipeline_error(&e))?;
    source.link(&filter).map_err(|e| pipeline_error(&e))?;
    convert.link(&sink).map_err(|e| pipeline_error(&e))?;
    match mode.encoding {
        Encoding::Raw(_) => filter.link(&convert).map_err(|e| pipeline_error(&e))?,
        Encoding::Jpeg => {
            let decode = gst::ElementFactory::make("decodebin")
                .build()
                .map_err(|e| pipeline_error(&e))?;
            pipeline.add(&decode).map_err(|e| pipeline_error(&e))?;
            filter.link(&decode).map_err(|e| pipeline_error(&e))?;
            route_decoded(&decode, &pipeline, &convert, None, false);
        }
    }
    Ok((pipeline, convert, sink))
}

/// Links `pad` to a new `fakesink` (paced by the clock, like the video).
pub(crate) fn discard(pipeline: &gst::Pipeline, pad: &gst::Pad) {
    let linked = gst::ElementFactory::make("fakesink")
        .property("sync", true)
        .build()
        .map_err(|e| e.to_string())
        .and_then(|fake| {
            pipeline.add(&fake).map_err(|e| e.to_string())?;
            fake.sync_state_with_parent().map_err(|e| e.to_string())?;
            let target = fake.static_pad("sink").ok_or("fakesink without a pad")?;
            pad.link(&target).map(|_| ()).map_err(|e| e.to_string())
        });
    if let Err(e) = linked {
        tracing::warn!(error = %e, pad = %pad.name(), "cannot discard a stream");
    }
}

/// Accurate segment seek to `ms`, to the end of the stream (`SEGMENT_DONE` there).
fn segment_seek(
    pipeline: &gst::Pipeline,
    ms: u64,
    flush: bool,
) -> Result<(), gst::glib::BoolError> {
    let mut flags = gst::SeekFlags::SEGMENT | gst::SeekFlags::ACCURATE;
    if flush {
        flags |= gst::SeekFlags::FLUSH;
    }
    pipeline.seek(
        1.0,
        flags,
        gst::SeekType::Set,
        Some(gst::ClockTime::from_mseconds(ms)),
        gst::SeekType::None,
        gst::ClockTime::NONE,
    )
}

/// End of a file segment (sync handler, streaming thread): loop with a non-flushing seek to
/// 0, done at once on a GStreamer worker thread (a seek may not run on the streaming thread
/// that posted the message); or mark the end.
fn segment_done(pipeline: &gst::glib::WeakRef<gst::Pipeline>, shared: &Shared) {
    if !shared.looping.load(Ordering::Relaxed) {
        shared.segment_done.store(true, Ordering::Relaxed);
        return;
    }
    if let Some(pipeline) = pipeline.upgrade() {
        pipeline.call_async(|pipeline| {
            if let Err(e) = segment_seek(pipeline, 0, false) {
                tracing::warn!(error = %e, "loop seek failed");
            }
        });
    }
}

/// Bus message → note for [`GstInput::poll`] (sync handler: GStreamer thread, cheap).
fn relay(notes: &Sender<Note>, message: &gst::Message) {
    let note = match message.view() {
        gst::MessageView::Error(e) => Note::Lost(format!(
            "{} ({})",
            e.error(),
            e.debug().map(|d| d.to_string()).unwrap_or_default()
        )),
        gst::MessageView::Eos(_) => Note::EndOfStream,
        gst::MessageView::AsyncDone(_) => Note::AsyncDone,
        _ => return,
    };
    let _ = notes.try_send(note);
}

/// Appsink callback (streaming thread): a picture while playing, or the preroll picture.
fn on_picture<F: Feeds>(
    stream: &std::sync::Mutex<Stream<F>>,
    sink: &gst_app::AppSink,
    preroll: bool,
) -> Result<gst::FlowSuccess, gst::FlowError> {
    // GStreamer calls the two callbacks of one appsink one after the other; the release
    // thread of a paced input takes the lock too, so a picture for it is queued after the
    // lock is released (a full queue waits for that thread).
    let release = stream
        .lock()
        .map_err(|_| gst::FlowError::Error)?
        .on_sample(sink, preroll)?;
    if let Some((queue, release)) = release {
        queue.send(release).map_err(|_| gst::FlowError::Flushing)?;
    }
    Ok(gst::FlowSuccess::Ok)
}

/// State of the appsink callback (streaming thread).
struct Stream<F: Feeds> {
    input: InputId,
    feeds: F,
    out: Option<(VideoFormat, F::Out)>,
    /// Counter of the pictures of a live source.
    next_frame: u64,
    /// Video file: the source frame number comes from the stream time.
    timeline: bool,
    /// PTS of the last preroll picture: played next, it must not be pushed twice.
    preroll_pts: Option<gst::ClockTime>,
    shared: Arc<Shared>,
    convert: gst::Element,
    pacer: Option<Paced>,
    /// The release thread could not hand a picture to the feed: the next callback fails.
    failed: bool,
}

/// The pacing of a network input: arrivals are dated in the appsink callback, which never
/// waits (so they are the real ones), and the pictures go to a release thread.
struct Paced {
    pacer: Pacer,
    clock: gst::Clock,
    to_release: Sender<Release>,
}

/// A picture held until its release time (the sample is reference counted: no copy).
struct Release {
    sample: gst::Sample,
    stream_ns: Option<u64>,
    at_ns: u64,
}

/// Pictures held at most by the release thread of a paced input (a full queue makes the
/// appsink callback wait).
const PACE_QUEUE: usize = 16;

/// The jitter buffer of a network source (SRT, spec `io-gst.md`, « Entrée SRT »): pictures
/// that arrive in bursts are released one at a time, each **just before the master tick it is
/// due on**, so the tick that shows a picture follows from its stream time alone, never from
/// when a thread woke up. Their stream time (and so the sound) does not change.
///
/// A picture of stream time `t` is due on the first tick at or after `t + delay`, and released
/// half a master period before it. The delay covers the latest arrivals (largest arrival −
/// stream time, over the last [`PACE_WINDOW_NS`] to twice that), plus one source period (room
/// for a burst of two pictures), half a master period and [`PACE_HEADROOM_NS`]. It then only
/// changes by events, so that the latency of the pictures (and the lip sync) stays put:
/// - a picture that arrives less than a quarter of a master period before its tick (late) sets
///   it again;
/// - a picture more than [`PACE_JUMP_NS`] earlier or later than the latest arrivals
///   (timestamps jumped, the stream stalled) starts afresh: arrivals forgotten, delay from it;
/// - when the latest arrivals allow a delay shorter by at least one master period, and it has
///   not changed for [`PACE_WINDOW_NS`], it drops by whole periods (one picture skipped, the
///   phase of the ticks kept).
///
/// The arrivals are dated in the appsink callback, which never waits (the release thread
/// does): they are the real ones.
#[derive(Debug)]
struct Pacer {
    ticks: TickGrid,
    /// Presentation delay, in ns (negative: the stream times of the source are ahead of the
    /// arrivals, as with a receiver that dates by presentation).
    delay: Option<i64>,
    arrivals: Window,
    /// When the delay last changed.
    changed_at: u64,
}

/// Longest wait for one picture (safety: a jump of the timestamps starts afresh before).
const PACE_MAX_WAIT: Duration = Duration::from_secs(1);
/// A picture this much earlier or later than the latest arrivals: the stream jumped.
const PACE_JUMP_NS: i64 = 500_000_000;
/// Length of the blocks over which the arrivals are remembered.
const PACE_WINDOW_NS: u64 = 15_000_000_000;
/// Margin of the delay over the latest arrivals (the copy into the feed, a late wake-up).
const PACE_HEADROOM_NS: i64 = 2_000_000;

/// Largest value seen in the current block of [`PACE_WINDOW_NS`] and in the one before.
#[derive(Debug, Default)]
struct Window {
    block_start: u64,
    current: Option<i64>,
    previous: Option<i64>,
}

impl Window {
    fn record(&mut self, now_ns: u64, value: i64) {
        match self.current {
            Some(current) if now_ns.saturating_sub(self.block_start) < PACE_WINDOW_NS => {
                self.current = Some(current.max(value));
            }
            current => {
                self.previous = current;
                self.current = Some(value);
                self.block_start = now_ns;
            }
        }
    }

    /// Largest value remembered (`value` if none).
    fn max(&self, value: i64) -> i64 {
        match (self.current, self.previous) {
            (Some(c), Some(p)) => c.max(p),
            (Some(v), None) | (None, Some(v)) => v,
            (None, None) => value,
        }
    }
}

impl Pacer {
    fn new(ticks: TickGrid) -> Self {
        Self {
            ticks,
            delay: None,
            arrivals: Window::default(),
            changed_at: 0,
        }
    }

    /// When to release the picture of stream time `stream_ns` that arrived at `now_ns` (both
    /// in stream time); `period_ns`: one source period. At or before `now_ns`: at once.
    fn release_at(&mut self, stream_ns: u64, now_ns: u64, period_ns: u64) -> u64 {
        let master = signed(self.ticks.period_ns().max(1));
        let half = master / 2;
        let offset = signed(now_ns) - signed(stream_ns);
        let jumped = self
            .delay
            .is_some_and(|_| (offset - self.arrivals.max(offset)).abs() > PACE_JUMP_NS);
        if jumped {
            self.arrivals = Window::default();
        }
        self.arrivals.record(now_ns, offset);
        let wanted = self.arrivals.max(offset) + signed(period_ns) + half + PACE_HEADROOM_NS;
        let mut delay = match self.delay {
            Some(delay) if !jumped => delay,
            _ => self.set(wanted, now_ns),
        };
        if delay >= wanted + master && now_ns.saturating_sub(self.changed_at) >= PACE_WINDOW_NS {
            delay = self.set(delay - (delay - wanted) / master * master, now_ns);
            tracing::debug!(
                delay_ms = delay as f64 / 1e6,
                "paced input: delay shortened"
            );
        }
        let mut due = self.due(stream_ns, delay);
        if signed(now_ns) + half / 2 > signed(due) {
            delay = self.set(wanted, now_ns);
            due = self.due(stream_ns, delay);
            tracing::debug!(
                delay_ms = delay as f64 / 1e6,
                "paced input: late, delay set again"
            );
        }
        due.saturating_sub(unsigned(half))
    }

    fn set(&mut self, delay: i64, now_ns: u64) -> i64 {
        self.delay = Some(delay);
        self.changed_at = now_ns;
        delay
    }

    /// Stream time of the tick the picture of stream time `stream_ns` is shown on.
    fn due(&self, stream_ns: u64, delay: i64) -> u64 {
        let ticks = self.ticks;
        ticks.tick_ns(ticks.tick_at_or_after(unsigned(signed(stream_ns) + delay)))
    }
}

fn signed(ns: u64) -> i64 {
    i64::try_from(ns).unwrap_or(i64::MAX)
}

fn unsigned(ns: i64) -> u64 {
    u64::try_from(ns).unwrap_or(0)
}

impl<F: Feeds> Stream<F> {
    /// Handles a picture, or returns it with the queue of the release thread (paced input).
    fn on_sample(
        &mut self,
        sink: &gst_app::AppSink,
        preroll: bool,
    ) -> Result<Option<(Sender<Release>, Release)>, gst::FlowError> {
        let sample = if preroll {
            sink.pull_preroll()
        } else {
            sink.pull_sample()
        }
        .map_err(|_| gst::FlowError::Eos)?;
        let (Some(buffer), Some(caps)) = (sample.buffer(), sample.caps()) else {
            return Ok(None);
        };
        let pts = buffer.pts();
        if preroll {
            if pts.is_some() && pts == self.preroll_pts {
                // Prerolled again on the picture already shown (seek to where it was).
                return Ok(None);
            }
            self.preroll_pts = pts;
        } else if let Some(prerolled) = self.preroll_pts.take()
            && pts == Some(prerolled)
        {
            return Ok(None);
        }
        if self.failed {
            return Err(gst::FlowError::Error);
        }
        // The preroll picture of a file is not played yet: no stream time (a live source
        // hands its first picture as a preroll, while playing).
        let stream_ns = (!preroll || !self.timeline)
            .then(|| stream_time(&sample, sink, pts))
            .flatten();
        if let Some(paced) = &mut self.pacer {
            let now = paced.clock.time().nseconds();
            let at_ns = match stream_ns {
                Some(t) => {
                    let period = caps_period_ns(caps).unwrap_or(40_000_000);
                    paced.pacer.release_at(t, now, period)
                }
                None => now,
            };
            let release = Release {
                sample: sample.clone(),
                stream_ns,
                at_ns,
            };
            return Ok(Some((paced.to_release.clone(), release)));
        }
        self.deliver(&sample, stream_ns)?;
        Ok(None)
    }

    /// Hands a picture to the feed (from the appsink callback, or the release thread of a
    /// paced input).
    fn deliver(
        &mut self,
        sample: &gst::Sample,
        stream_ns: Option<u64>,
    ) -> Result<(), gst::FlowError> {
        let (Some(buffer), Some(caps)) = (sample.buffer(), sample.caps()) else {
            return Ok(());
        };
        let pts = buffer.pts();
        let info =
            gst_video::VideoInfo::from_caps(caps).map_err(|_| gst::FlowError::NotNegotiated)?;
        let format = video_format(&info).ok_or(gst::FlowError::NotNegotiated)?;
        if self.out.as_ref().is_none_or(|(f, _)| *f != format) {
            self.on_new_format(format)?;
        }
        let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)
            .map_err(|_| gst::FlowError::Error)?;
        let strides = frame.plane_stride();
        let mut planes = [Plane {
            data: &[],
            stride: 0,
        }; 3];
        let count = format.pixel.plane_count();
        for (i, plane) in planes.iter_mut().enumerate().take(count) {
            let index = u32::try_from(i).map_err(|_| gst::FlowError::Error)?;
            *plane = Plane {
                data: frame.plane_data(index).map_err(|_| gst::FlowError::Error)?,
                stride: strides
                    .get(i)
                    .and_then(|s| u32::try_from(*s).ok())
                    .ok_or(gst::FlowError::Error)?,
            };
        }
        let source_frame = self
            .timeline
            .then(|| stream_frame(sample, pts, format.rate))
            .flatten()
            .unwrap_or(self.next_frame);
        if let Some((_, out)) = &mut self.out
            && let Err(e) = out.push(
                planes.get(..count).unwrap_or_default(),
                source_frame,
                stream_ns,
            )
        {
            tracing::warn!(input = %self.input, error = %e, "picture refused");
        }
        self.next_frame += 1;
        self.shared.pictures.fetch_add(1, Ordering::Relaxed);
        self.shared
            .last_picture
            .store(self.shared.since_start() + 1, Ordering::Release);
        Ok(())
    }

    fn on_new_format(&mut self, format: VideoFormat) -> Result<(), gst::FlowError> {
        let converting = self
            .convert
            .static_pad("sink")
            .and_then(|p| p.current_caps())
            .and_then(|c| gst_video::VideoInfo::from_caps(&c).ok())
            .is_some_and(|i| pixel_format(i.format()) != Some(format.pixel));
        self.shared.converting.store(converting, Ordering::Relaxed);
        let period = format.rate.map_or(0, |r| {
            u64::try_from(r.frame_duration().as_nanos()).unwrap_or(0)
        });
        self.shared.period_ns.store(period, Ordering::Relaxed);
        tracing::info!(input = %self.input, ?format, converting, "source format");
        match self.feeds.create(self.input, format) {
            Ok(out) => {
                self.out = Some((format, out));
                Ok(())
            }
            Err(e) => {
                tracing::error!(input = %self.input, error = %e, "cannot create the feed");
                Err(gst::FlowError::Error)
            }
        }
    }
}

/// One source period from the frame rate in `caps`, if it has one.
fn caps_period_ns(caps: &gst::CapsRef) -> Option<u64> {
    let info = gst_video::VideoInfo::from_caps(caps).ok()?;
    let rate = info.fps();
    let (num, den) = (
        u64::try_from(rate.numer()).ok()?,
        u64::try_from(rate.denom()).ok()?,
    );
    (num > 0).then(|| den * 1_000_000_000 / num)
}

/// The release thread of a paced input: hands each picture to the feed at its release time.
/// Ends with the stream (its sender goes with it).
fn spawn_release<F: Feeds>(
    input: InputId,
    stream: std::sync::Weak<std::sync::Mutex<Stream<F>>>,
    released: Receiver<Release>,
    clock: gst::Clock,
) -> Result<(), IoError> {
    std::thread::Builder::new()
        .name(format!("vma-pace-{input}"))
        .spawn(move || {
            while let Ok(picture) = released.recv() {
                let wait = picture.at_ns.saturating_sub(clock.time().nseconds());
                if wait > 0 {
                    std::thread::sleep(Duration::from_nanos(wait).min(PACE_MAX_WAIT));
                }
                let Some(stream) = stream.upgrade() else {
                    return;
                };
                let Ok(mut stream) = stream.lock() else {
                    return;
                };
                if let Err(e) = stream.deliver(&picture.sample, picture.stream_ns) {
                    tracing::warn!(%input, error = ?e, "paced picture refused");
                    stream.failed = true;
                }
            }
        })
        .map(|_| ())
        .map_err(|e| IoError::Pipeline(format!("cannot start the release thread: {e}")))
}

/// Stream time of a buffer in the master clock's time (ADR 0007 § 1): `base_time` of the
/// pipeline plus the running time of its PTS. A live source dates a buffer by its capture,
/// a file by when it is to be shown.
pub(crate) fn stream_time(
    sample: &gst::Sample,
    element: &impl IsA<gst::Element>,
    pts: Option<gst::ClockTime>,
) -> Option<u64> {
    let segment = sample.segment()?.downcast_ref::<gst::ClockTime>()?;
    let running = segment.to_running_time(pts?)?;
    let base = element.base_time()?;
    Some((base + running).nseconds())
}

/// Frame number in the file of a picture: its stream time, in frames (rounded: containers
/// store times to the millisecond).
fn stream_frame(
    sample: &gst::Sample,
    pts: Option<gst::ClockTime>,
    rate: Option<FrameRate>,
) -> Option<u64> {
    let segment = sample.segment()?.downcast_ref::<gst::ClockTime>()?;
    let time = segment.to_stream_time(pts?)?;
    let rate = rate?;
    let half = rate.frame_offset_nanos(1) / 2;
    Some(rate.frame_at_nanos(u128::from(time.nseconds()) + half))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex;
    use vma_compositor::FrameError;
    use vma_engine::FeedError;

    /// One `create` call of [`FakeFeeds`] and what was pushed into it.
    #[derive(Debug, Default)]
    pub(crate) struct FakeFeed {
        pub(crate) format: Option<VideoFormat>,
        pub(crate) frames: Vec<u64>,
        /// Plane sizes (bytes) of the last picture.
        pub(crate) plane_bytes: Vec<usize>,
        pub(crate) arrivals: Vec<Instant>,
        pub(crate) stream_times: Vec<Option<u64>>,
        /// First byte of each picture (its first luma sample).
        pub(crate) first_bytes: Vec<u8>,
    }

    /// CPU stand-in for the engine's feeds.
    #[derive(Debug, Clone, Default)]
    pub(crate) struct FakeFeeds {
        pub(crate) feeds: Arc<Mutex<Vec<Arc<Mutex<FakeFeed>>>>>,
        /// `Some`: the sound goes to rings, kept here.
        pub(crate) rings: Option<Arc<Mutex<Vec<vma_audio::RingReader>>>>,
    }

    pub(crate) struct FakeOut(Arc<Mutex<FakeFeed>>);

    impl PictureOut for FakeOut {
        fn push(
            &mut self,
            planes: &[Plane<'_>],
            frame: u64,
            stream_ns: Option<u64>,
        ) -> Result<(), FrameError> {
            let mut f = self.0.lock().unwrap();
            f.frames.push(frame);
            f.stream_times.push(stream_ns);
            f.first_bytes.push(
                planes
                    .first()
                    .and_then(|p| p.data.first())
                    .copied()
                    .unwrap_or(0),
            );
            f.plane_bytes = planes.iter().map(|p| p.data.len()).collect();
            f.arrivals.push(Instant::now());
            Ok(())
        }
    }

    impl Feeds for FakeFeeds {
        type Out = FakeOut;
        fn create(&self, _: InputId, format: VideoFormat) -> Result<FakeOut, FeedError> {
            let feed = Arc::new(Mutex::new(FakeFeed {
                format: Some(format),
                ..FakeFeed::default()
            }));
            self.feeds.lock().unwrap().push(Arc::clone(&feed));
            Ok(FakeOut(feed))
        }

        fn audio(&self, _: InputId) -> Option<RingWriter> {
            let rings = self.rings.as_ref()?;
            let (writer, reader) = vma_audio::ring(vma_audio::RING_FRAMES);
            rings.lock().unwrap().push(reader);
            Some(writer)
        }
    }

    impl FakeFeeds {
        pub(crate) fn with_audio() -> Self {
            Self {
                rings: Some(Arc::default()),
                ..Self::default()
            }
        }

        pub(crate) fn count(&self) -> usize {
            self.feeds.lock().unwrap().len()
        }

        pub(crate) fn last(&self) -> Arc<Mutex<FakeFeed>> {
            Arc::clone(self.feeds.lock().unwrap().last().unwrap())
        }
    }

    pub(crate) fn init() {
        gst::init().expect("GStreamer required for io-gst tests");
    }

    pub(crate) fn live(source: &str) -> Recipe {
        Recipe {
            media: Media::Launch(source.into()),
            sync: false,
            watchdog: true,
            patient: false,
            pace: None,
        }
    }

    fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        cond()
    }

    /// ADR 0007 § 1: the pictures of a live source carry their stream time, in the master
    /// clock's time: 20 ms apart, each taken before it arrives, by less than 100 ms.
    #[test]
    fn live_pictures_carry_their_stream_time() {
        init();
        let feeds = FakeFeeds::default();
        let clock = crate::clock::stream_clock();
        let _input = GstInput::start(
            InputId(1),
            &live(
                "videotestsrc is-live=true \
                 ! video/x-raw,format=NV12,width=320,height=180,framerate=50/1",
            ),
            feeds.clone(),
        )
        .unwrap();
        assert!(wait_until(Duration::from_secs(5), || feeds.count() == 1
            && feeds.last().lock().unwrap().frames.len() > 30));
        let f = feeds.last();
        let f = f.lock().unwrap();
        let times: Vec<u64> = f.stream_times.iter().map(|t| t.unwrap()).collect();
        for w in times.windows(2) {
            assert!(w[1].abs_diff(w[0] + 20_000_000) < 2_000_000, "{w:?}");
        }
        for (t, arrival) in times.iter().zip(&f.arrivals) {
            let arrived = clock.at(*arrival);
            assert!(
                *t <= arrived && arrived - t < 100_000_000,
                "{t} vs {arrived}"
            );
        }
    }

    /// `videotestsrc` 1080p50 live: pictures in order, at the source rate (±2 %), format and
    /// colorimetry from the caps.
    #[test]
    fn live_test_source_delivers_in_order_at_its_rate() {
        init();
        let feeds = FakeFeeds::default();
        let mut input = GstInput::start(
            InputId(1),
            &live(
                "videotestsrc is-live=true pattern=ball \
                 ! video/x-raw,format=NV12,width=1920,height=1080,framerate=50/1",
            ),
            feeds.clone(),
        )
        .unwrap();
        assert!(wait_until(Duration::from_secs(5), || feeds.count() == 1
            && feeds.last().lock().unwrap().frames.len() > 10));
        let first = {
            let f = feeds.last();
            let f = f.lock().unwrap();
            (f.frames.len(), *f.arrivals.last().unwrap())
        };
        std::thread::sleep(Duration::from_secs(2));
        assert!(input.poll(Instant::now()).is_none());
        let f = feeds.last();
        let f = f.lock().unwrap();
        let fmt = f.format.unwrap();
        assert_eq!(fmt.pixel, PixelFormat::Nv12);
        assert_eq!(fmt.resolution, Resolution::HD_1080);
        assert_eq!(fmt.rate, Some(FrameRate::FPS_50));
        assert_eq!(fmt.color, ColorInfo::assumed(PixelFormat::Nv12, 1080));
        assert!(
            f.frames.windows(2).all(|w| w[1] == w[0] + 1),
            "out of order"
        );
        let n = (f.frames.len() - first.0) as f64;
        let secs = f
            .arrivals
            .last()
            .unwrap()
            .duration_since(first.1)
            .as_secs_f64();
        let rate = n / secs;
        eprintln!("measured {rate:.2} pictures/s");
        assert!((rate - 50.0).abs() <= 1.0, "{rate:.2} pictures/s");
        assert_eq!(f.plane_bytes, [1920 * 1080, 1920 * 1080 / 2]);
        assert!(!input.converting());
    }

    /// Every accepted format reaches the feed as is; another one goes through videoconvert.
    #[test]
    fn accepted_formats_arrive_without_conversion() {
        init();
        for (caps, want) in [
            ("NV12", PixelFormat::Nv12),
            ("I420", PixelFormat::I420),
            ("YUY2", PixelFormat::Yuy2),
            ("UYVY", PixelFormat::Uyvy),
            ("RGBA", PixelFormat::Rgba),
            ("BGRA", PixelFormat::Bgra),
        ] {
            let feeds = FakeFeeds::default();
            let input = GstInput::start(
                InputId(1),
                &live(&format!(
                    "videotestsrc is-live=true ! video/x-raw,format={caps},width=64,height=36,framerate=50/1"
                )),
                feeds.clone(),
            )
            .unwrap();
            assert!(wait_until(Duration::from_secs(5), || input.pictures() >= 3));
            assert_eq!(
                feeds.last().lock().unwrap().format.unwrap().pixel,
                want,
                "{caps}"
            );
            assert!(!input.converting(), "{caps} converted");
        }
        let feeds = FakeFeeds::default();
        let input = GstInput::start(
            InputId(1),
            &live("videotestsrc is-live=true ! video/x-raw,format=Y444,width=64,height=36"),
            feeds.clone(),
        )
        .unwrap();
        assert!(wait_until(Duration::from_secs(5), || input.pictures() >= 3));
        assert!(input.converting(), "Y444 must be converted");
    }

    /// A8 at the pipeline level: an error in the stream, an end of stream, and silence are
    /// all reported as lost.
    #[test]
    fn errors_end_of_stream_and_silence_are_losses() {
        init();
        let feeds = FakeFeeds::default();
        let mut erroring = GstInput::start(
            InputId(1),
            &live("videotestsrc is-live=true ! identity error-after=10"),
            feeds.clone(),
        )
        .unwrap();
        assert!(wait_until(Duration::from_secs(5), || erroring
            .poll(Instant::now())
            .is_some()));

        let mut ending = GstInput::start(
            InputId(2),
            &live("videotestsrc is-live=true num-buffers=5"),
            feeds.clone(),
        )
        .unwrap();
        let mut reason = None;
        assert!(wait_until(Duration::from_secs(5), || {
            reason = ending.poll(Instant::now());
            reason.is_some()
        }));
        assert_eq!(reason.as_deref(), Some("end of stream"));

        let mut stalling = GstInput::start(
            InputId(3),
            &live("videotestsrc is-live=true ! video/x-raw,framerate=50/1 ! valve name=valve"),
            feeds.clone(),
        )
        .unwrap();
        assert!(wait_until(Duration::from_secs(5), || stalling.pictures() >= 5));
        assert!(stalling.poll(Instant::now()).is_none());
        stalling
            .element("valve")
            .unwrap()
            .set_property("drop", true);
        let stalled = Instant::now();
        let mut reason = None;
        assert!(wait_until(Duration::from_secs(3), || {
            reason = stalling.poll(Instant::now());
            reason.is_some()
        }));
        let after = stalled.elapsed();
        eprintln!("silence detected after {after:?}: {reason:?}");
        // Counted from the last picture, which arrived up to one period (20 ms) before the
        // valve closed.
        let period = Duration::from_millis(20);
        assert!(
            after + period >= MIN_SILENCE && after < MIN_SILENCE * 3,
            "{after:?}"
        );
    }

    /// Writes a test clip: `frames` pictures at 25 fps (64 × 36, MJPEG) and a Vorbis audio
    /// track, in Matroska. Only plugins of `-base` and `-good`, present in CI.
    pub(crate) fn clip(name: &str, frames: u32) -> PathBuf {
        init();
        let path = std::env::temp_dir().join(format!("vma-io-{}-{name}.mkv", std::process::id()));
        let pipeline = gst::parse::launch(&format!(
            "videotestsrc num-buffers={frames} \
             ! video/x-raw,format=I420,width=64,height=36,framerate=25/1 \
             ! jpegenc ! matroskamux name=mux ! filesink name=out \
             audiotestsrc num-buffers={frames} samplesperbuffer=1764 \
             ! audio/x-raw,rate=44100 ! audioconvert ! vorbisenc ! mux."
        ))
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
        pipeline
            .by_name("out")
            .unwrap()
            .set_property("location", path.to_str().unwrap());
        pipeline.set_state(gst::State::Playing).unwrap();
        let message = pipeline
            .bus()
            .unwrap()
            .timed_pop_filtered(
                gst::ClockTime::from_seconds(30),
                &[gst::MessageType::Eos, gst::MessageType::Error],
            )
            .expect("clip not written in time");
        assert!(
            matches!(message.view(), gst::MessageView::Eos(_)),
            "{message:?}"
        );
        pipeline.set_state(gst::State::Null).unwrap();
        path
    }

    /// Polls `input` for `duration`, like `vma-io` does; it must never be lost.
    fn drive(input: &mut GstInput, duration: Duration) {
        let end = Instant::now() + duration;
        while Instant::now() < end {
            assert_eq!(input.poll(Instant::now()), None);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Polls `input` until `cond` holds (at most `timeout`).
    fn drive_until(
        input: &mut GstInput,
        timeout: Duration,
        mut cond: impl FnMut(&GstInput) -> bool,
    ) -> bool {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            assert_eq!(input.poll(Instant::now()), None);
            if cond(input) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        cond(input)
    }

    fn last_frame(feeds: &FakeFeeds) -> Option<u64> {
        (feeds.count() > 0)
            .then(|| feeds.last().lock().unwrap().frames.last().copied())
            .flatten()
    }

    /// A9: a looping file plays loop after loop with continuous frame numbers (modulo its
    /// length) and no gap at the loop point (no black: the input is never lost).
    #[test]
    fn looping_file_has_no_gap() {
        const FRAMES: u64 = 25;
        let path = clip("loop", 25);
        let feeds = FakeFeeds::default();
        let mut input =
            GstInput::start(InputId(1), &Recipe::file(path.clone()), feeds.clone()).unwrap();
        input.set_looping(true);
        assert!(drive_until(&mut input, Duration::from_secs(5), |_| feeds
            .count()
            == 1));
        let started = Instant::now();
        input.set_playing(true);
        let loops = |feeds: &FakeFeeds| {
            let f = feeds.last();
            let f = f.lock().unwrap();
            f.frames.windows(2).filter(|w| w[1] == 0).count()
        };
        assert!(drive_until(&mut input, Duration::from_secs(10), |_| loops(
            &feeds
        ) >= 3
            && last_frame(&feeds) >= Some(5)));
        let f = feeds.last();
        let f = f.lock().unwrap();
        assert_eq!(feeds.count(), 1, "one feed for the whole run");
        assert_eq!(f.format.unwrap().rate, Some(FrameRate::new(25, 1).unwrap()));
        assert_eq!(f.frames.first(), Some(&0), "opens on the first picture");
        for (i, w) in f.frames.windows(2).enumerate() {
            assert_eq!(
                w[1],
                (w[0] + 1) % FRAMES,
                "frames {i}-{}: {:?}",
                i + 1,
                f.frames
            );
        }
        let loops = f.frames.windows(2).filter(|w| w[1] == 0).count();
        assert!(loops >= 3, "{loops} loops in {:?}", started.elapsed());
        // Pictures keep their 40 ms pace across the loop points (tolerance: scheduling).
        let worst = f
            .arrivals
            .windows(2)
            .skip(2)
            .map(|w| w[1] - w[0])
            .max()
            .unwrap();
        eprintln!(
            "{} pictures, {loops} loops, worst interval {worst:?}",
            f.frames.len()
        );
        assert!(worst < Duration::from_millis(100), "gap of {worst:?}");
        drop(f);
        drop(input);
        let _ = std::fs::remove_file(path);
    }

    /// ADR 0007 § 2: the sound of a file reaches its ring at 48 kHz stereo, dated in the
    /// time of its pictures (same media time, same stream time, to 5 ms).
    #[test]
    fn file_sound_reaches_its_ring_on_the_time_of_the_pictures() {
        let path = clip("sound", 75);
        let feeds = FakeFeeds::with_audio();
        let mut input =
            GstInput::start(InputId(1), &Recipe::file(path.clone()), feeds.clone()).unwrap();
        assert!(drive_until(&mut input, Duration::from_secs(5), |_| feeds
            .count()
            == 1));
        input.set_playing(true);
        drive(&mut input, Duration::from_millis(1_500));
        let rings = feeds.rings.as_ref().unwrap().lock().unwrap();
        assert_eq!(rings.len(), 1, "one sound stream");
        let ring = &rings[0];
        let written = ring.written();
        assert!(written > 48_000, "{written} frames in 1.5 s");
        let anchor = ring.anchor().unwrap();
        let mut sound = vec![0.0f32; 2 * 4_800];
        assert!(ring.read(written as i64 - 4_800, &mut sound).is_complete());
        assert!(sound.iter().any(|s| s.abs() > 0.1), "a tone, not silence");
        assert!(
            sound.as_chunks::<2>().0.iter().all(|f| f[0] == f[1]),
            "mono duplicated"
        );
        // Stream time of media time 0, by the sound and by a picture played.
        let audio_origin =
            anchor.time_ns as i128 - (anchor.position as i128 * 1_000_000_000 / 48_000);
        let f = feeds.last();
        let f = f.lock().unwrap();
        let (frame, time) = f
            .frames
            .iter()
            .zip(&f.stream_times)
            .filter_map(|(&k, t)| t.map(|t| (k, t)))
            .next_back()
            .unwrap();
        let video_origin = time as i128 - frame as i128 * 40_000_000;
        let gap = audio_origin - video_origin;
        eprintln!("sound − pictures: {} µs", gap / 1_000);
        assert!(
            gap.abs() < 5_000_000,
            "sound {} µs off its pictures",
            gap / 1_000
        );
        drop(f);
        drop(rings);
        drop(input);
        let _ = std::fs::remove_file(path);
    }

    /// A20: opens paused on its first picture; play, pause, seek, restart; at the end
    /// without Loop the last picture stays (no loss), and Play starts again from the start.
    #[test]
    fn file_playback_controls() {
        let path = clip("controls", 50);
        let feeds = FakeFeeds::default();
        let mut input =
            GstInput::start(InputId(1), &Recipe::file(path.clone()), feeds.clone()).unwrap();
        assert!(drive_until(&mut input, Duration::from_secs(5), |i| i
            .timeline()
            .is_some_and(|t| t.duration_ms.is_some())));
        drive(&mut input, Duration::from_millis(300));
        assert_eq!(feeds.last().lock().unwrap().frames, [0], "paused at open");
        let t = input.timeline().unwrap();
        assert_eq!((t.position_ms, t.ended), (0, false));
        assert!(t.duration_ms.unwrap().abs_diff(2000) <= 40, "{t:?}");

        input.set_playing(true);
        assert!(drive_until(
            &mut input,
            Duration::from_secs(2),
            |_| last_frame(&feeds) >= Some(10)
        ));
        input.set_playing(false);
        drive(&mut input, Duration::from_millis(150));
        let paused_at = last_frame(&feeds);
        drive(&mut input, Duration::from_millis(400));
        assert_eq!(last_frame(&feeds), paused_at, "paused");

        input.seek(1000);
        assert!(drive_until(
            &mut input,
            Duration::from_secs(2),
            |_| last_frame(&feeds) == Some(25)
        ));
        let t = input.timeline().unwrap();
        assert!(t.position_ms.abs_diff(1000) <= 40, "{t:?}");
        input.seek(0);
        assert!(drive_until(
            &mut input,
            Duration::from_secs(2),
            |_| last_frame(&feeds) == Some(0)
        ));

        // To the end, without Loop.
        input.seek(1500);
        input.set_playing(true);
        assert!(drive_until(&mut input, Duration::from_secs(3), |i| i
            .timeline()
            .is_some_and(|t| t.ended)));
        assert_eq!(last_frame(&feeds), Some(49));
        let pictures = input.pictures();
        drive(&mut input, Duration::from_millis(300));
        assert_eq!(input.pictures(), pictures, "nothing after the end");
        assert_eq!(feeds.count(), 1, "the feed (and its last picture) stays");

        // Play after the end starts from the beginning.
        input.set_playing(false);
        input.set_playing(true);
        assert!(drive_until(&mut input, Duration::from_secs(2), |_| {
            let f = feeds.last();
            let f = f.lock().unwrap();
            f.frames.ends_with(&[0, 1, 2])
        }));
        assert!(!input.timeline().unwrap().ended);
        drop(input);
        let _ = std::fs::remove_file(path);
    }

    /// A missing file fails to start (the slot reports it lost and retries); an unreadable
    /// one is lost once the decoder gives up.
    #[test]
    fn missing_or_unreadable_file_is_lost() {
        init();
        assert!(matches!(
            GstInput::start(
                InputId(1),
                &Recipe::file("/nonexistent/vma-missing.mkv".into()),
                FakeFeeds::default(),
            ),
            Err(IoError::Pipeline(_))
        ));
        let junk = std::env::temp_dir().join(format!("vma-io-{}-junk.mkv", std::process::id()));
        std::fs::write(&junk, b"not a video at all").unwrap();
        let mut input = GstInput::start(
            InputId(1),
            &Recipe::file(junk.clone()),
            FakeFeeds::default(),
        )
        .unwrap();
        let mut reason = None;
        assert!(wait_until(Duration::from_secs(5), || {
            reason = input.poll(Instant::now());
            reason.is_some()
        }));
        eprintln!("{reason:?}");
        drop(input);
        let _ = std::fs::remove_file(junk);
    }

    /// A capture device that is not a camera: its element is a `gst-launch` bin (CI runners
    /// have no camera). Lists `caps` like a real device.
    mod fake_camera {
        use std::sync::{LazyLock, Mutex};

        use gstreamer as gst;
        use gstreamer::glib;
        use gstreamer::prelude::*;
        use gstreamer::subclass::prelude::*;

        static CAT: LazyLock<gst::DebugCategory> = LazyLock::new(|| {
            gst::DebugCategory::new("vmafakecamera", gst::DebugColorFlags::empty(), None)
        });

        #[derive(Debug, Default)]
        pub struct Imp {
            pub(super) launch: Mutex<String>,
        }

        #[glib::object_subclass]
        impl ObjectSubclass for Imp {
            const NAME: &'static str = "VmaFakeCamera";
            type Type = FakeCamera;
            type ParentType = gst::Device;
        }

        impl ObjectImpl for Imp {}
        impl GstObjectImpl for Imp {}

        impl DeviceImpl for Imp {
            fn create_element(&self, _: Option<&str>) -> Result<gst::Element, gst::LoggableError> {
                let launch = self.launch.lock().unwrap().clone();
                gst::parse::bin_from_description(&launch, true)
                    .map(|bin| bin.upcast())
                    .map_err(|e| gst::LoggableError::new(*CAT, glib::bool_error!("{e}")))
            }
        }

        glib::wrapper! {
            pub struct FakeCamera(ObjectSubclass<Imp>) @extends gst::Device, gst::Object;
        }
    }

    /// A fake capture device of the kept API: `launch` is what it plays, `caps` what it
    /// announces.
    pub(crate) fn fake_camera(key: &str, launch: &str, caps: &str) -> crate::devices::Found {
        use gstreamer::subclass::prelude::ObjectSubclassIsExt;
        use std::str::FromStr;
        init();
        let mut props = gst::Structure::new_empty("properties");
        props.set("device.api", crate::devices::API);
        props.set("device.path", key);
        let camera: fake_camera::FakeCamera = gst::glib::Object::builder()
            .property("display-name", "Caméra de test")
            .property("device-class", "Video/Source")
            .property("caps", gst::Caps::from_str(caps).unwrap())
            .property("properties", props)
            .build();
        *camera.imp().launch.lock().unwrap() = launch.into();
        let device: gst::Device = camera.upcast();
        let key = crate::devices::identify(&device.properties().unwrap()).unwrap();
        crate::devices::keep(std::iter::once((key, device)))
            .pop()
            .unwrap()
    }

    /// A fake audio device (`Audio/Source` or `Audio/Sink`) of node name `node`: its element
    /// is the `gst-launch` bin `launch`.
    pub(crate) fn fake_audio_device(
        class: &str,
        node: &str,
        launch: &str,
    ) -> crate::devices::Found {
        use gstreamer::subclass::prelude::ObjectSubclassIsExt;
        init();
        let mut props = gst::Structure::new_empty("properties");
        props.set("device.api", "test");
        props.set("node.name", node);
        let fake: fake_camera::FakeCamera = gst::glib::Object::builder()
            .property("display-name", format!("Son de test {node}"))
            .property("device-class", class)
            .property("properties", props)
            .build();
        *fake.imp().launch.lock().unwrap() = launch.into();
        let device: gst::Device = fake.upcast();
        let key = crate::audio_devices::audio_key(&device.properties().unwrap()).unwrap();
        crate::devices::keep(std::iter::once((key, device)))
            .pop()
            .unwrap()
    }

    /// Webcam-like caps: YUY2 too slow at 720p, MJPEG fast enough.
    pub(crate) const WEBCAM_CAPS: &str = "\
        video/x-raw, format=YUY2, width=1280, height=720, framerate=10/1; \
        video/x-raw, format=YUY2, width=640, height=360, framerate=30/1; \
        image/jpeg, width=1280, height=720, framerate=30/1; \
        image/jpeg, width=640, height=360, framerate=30/1";

    /// Plays what the mode asks for: raw from `videotestsrc`, MJPEG through `jpegenc`.
    pub(crate) const WEBCAM_LAUNCH: &str =
        "videotestsrc is-live=true ! jpegenc ! capsfilter caps=image/jpeg";

    pub(crate) fn capture_recipe(id: &str, devices: &DeviceList, resolution: Resolution) -> Recipe {
        Recipe::capture(Capture {
            device: DeviceId(id.into()),
            devices: devices.clone(),
            resolution,
            rate: FrameRate::FPS_25,
        })
    }

    /// The device's mode is chosen from its caps and the master format; MJPEG is decoded
    /// (by `decodebin`) and arrives as I420 without conversion; raw arrives as is.
    #[test]
    fn capture_opens_the_chosen_mode() {
        let mjpeg = fake_camera("/dev/video7", WEBCAM_LAUNCH, WEBCAM_CAPS);
        let raw = fake_camera("/dev/video8", "videotestsrc is-live=true", WEBCAM_CAPS);
        let devices = DeviceList::default();
        devices.set(vec![mjpeg.clone(), raw.clone()]);
        for (found, resolution, encoding, pixel) in [
            (
                &mjpeg,
                Resolution::HD_720,
                Encoding::Jpeg,
                PixelFormat::I420,
            ),
            (
                &raw,
                Resolution::new(640, 360).unwrap(),
                Encoding::Raw("YUY2".into()),
                PixelFormat::Yuy2,
            ),
        ] {
            let feeds = FakeFeeds::default();
            let mut input = GstInput::start(
                InputId(1),
                &capture_recipe(&found.info.id.0, &devices, resolution),
                feeds.clone(),
            )
            .unwrap();
            let mode = input.capture_mode().unwrap().clone();
            assert_eq!(
                (mode.encoding.clone(), mode.resolution),
                (encoding, resolution)
            );
            assert_eq!(mode.rate, FrameRate::new(30, 1).unwrap());
            assert!(wait_until(Duration::from_secs(5), || input.pictures() >= 10));
            assert_eq!(input.poll(Instant::now()), None);
            let format = feeds.last().lock().unwrap().format.unwrap();
            assert_eq!(
                (format.pixel, format.resolution),
                (pixel, resolution),
                "{mode}"
            );
            assert!(!input.converting(), "{mode}");
        }
    }

    /// An unknown (or unplugged) device fails to start: lost, then retried.
    #[test]
    fn a_device_that_is_not_connected_fails_to_start() {
        init();
        let e = GstInput::start(
            InputId(1),
            &capture_recipe(
                "v4l2:/dev/nothing",
                &DeviceList::default(),
                Resolution::HD_1080,
            ),
            FakeFeeds::default(),
        )
        .unwrap_err();
        assert!(e.to_string().contains("is not connected"), "{e}");
    }

    #[test]
    fn a_bad_description_is_an_error() {
        init();
        assert!(matches!(
            GstInput::start(InputId(1), &live("nosuchelement"), FakeFeeds::default()),
            Err(IoError::Pipeline(_))
        ));
    }

    /// A paced source in simulated time: picture `k` of stream time `t_k` arrives
    /// `arrival(k)` after it (the appsink callback never waits) and is published 1 ms after
    /// its release (copy). Returns, per picture: stream time, publication, tick it is shown
    /// on, and the delay after it.
    fn pace(
        source: FrameRate,
        master: FrameRate,
        seconds: u64,
        arrival: impl Fn(u64) -> u64,
    ) -> Vec<(u64, u64, u64, i64)> {
        let ticks = TickGrid {
            origin_ns: 1_000_000_000,
            rate: master,
        };
        let mut pacer = Pacer::new(ticks);
        let period = source.frame_offset_nanos(1) as u64;
        let start = 5_000_000_000;
        let count = u64::from(source.num()) * seconds / u64::from(source.den());
        (0..count)
            .map(|k| {
                let t = start + source.frame_offset_nanos(k) as u64;
                let now = t + arrival(k);
                let published = pacer.release_at(t, now, period).max(now) + 1_000_000;
                let shown = ticks.tick_at_or_after(published);
                (t, published, shown, pacer.delay.unwrap())
            })
            .collect()
    }

    /// C9: pictures that come in pairs (an SRT receiver) are each shown on the tick their
    /// stream time gives, with a quarter of a master period of margin at least, none
    /// overwritten unseen, at a constant latency: the delay never moves after the start
    /// (10 min, no creeping from the waits themselves).
    #[test]
    fn paced_pictures_land_on_the_tick_of_their_stream_time() {
        for (source, master) in [
            (FrameRate::FPS_25, FrameRate::FPS_50),
            (FrameRate::FPS_50, FrameRate::FPS_50),
            (FrameRate::FPS_29_97, FrameRate::FPS_29_97),
            (FrameRate::FPS_25, FrameRate::FPS_25),
            (FrameRate::FPS_59_94, FrameRate::FPS_59_94),
        ] {
            let period = source.frame_offset_nanos(1) as u64;
            // Decoded 30 ms after its stream time, the odd pictures with the next one.
            let paced = pace(source, master, 600, |k| 30_000_000 + (k % 2) * period);
            let half = master.frame_offset_nanos(1) as u64 / 2;
            let ticks = TickGrid {
                origin_ns: 1_000_000_000,
                rate: master,
            };
            let delay = paced[0].3;
            let latency = |&(t, _, shown, _): &(u64, u64, u64, i64)| ticks.tick_ns(shown) - t;
            for (k, picture) in paced.iter().enumerate().skip(1) {
                let (t, published, shown, d) = *picture;
                assert_eq!(d, delay, "{source:?} → {master:?}: delay moved at {k}");
                assert_eq!(shown, ticks.tick_at_or_after(t + delay as u64), "{k}");
                assert!(
                    published + half / 2 <= ticks.tick_ns(shown),
                    "{k}: no margin"
                );
                assert!(shown > paced[k - 1].2, "{k}: overwritten unseen");
                assert!(
                    latency(picture).abs_diff(latency(&paced[1])) <= 1,
                    "{k}: latency moved"
                );
            }
        }
    }

    /// A source faster than the master: two pictures due on the same tick, the later one is
    /// shown (resampling); the tick of each still follows from its stream time.
    #[test]
    fn a_faster_paced_source_is_resampled_by_stream_time() {
        let paced = pace(FrameRate::FPS_50, FrameRate::FPS_25, 60, |_| 10_000_000);
        let ticks = TickGrid {
            origin_ns: 1_000_000_000,
            rate: FrameRate::FPS_25,
        };
        let delay = paced[0].3;
        for &(t, _, shown, d) in &paced {
            assert_eq!(d, delay);
            assert_eq!(shown, ticks.tick_at_or_after(t + delay as u64));
        }
    }

    /// The first picture came late (40 ms more than the others), and a hiccup of the network
    /// makes one picture 100 ms late: that one sets the delay again; once both are forgotten
    /// (two blocks), the delay drops by whole master periods, once, to what the regular
    /// arrivals need. The pictures keep their margin.
    #[test]
    fn the_delay_shortens_by_whole_periods_and_recovers_from_a_late_picture() {
        let master = FrameRate::FPS_50;
        let paced = pace(FrameRate::FPS_50, master, 70, |k| match k {
            0 => 50_000_000,
            500 => 110_000_000,
            _ => 10_000_000,
        });
        let ticks = TickGrid {
            origin_ns: 1_000_000_000,
            rate: master,
        };
        let period = master.frame_offset_nanos(1) as u64;
        let delays: Vec<i64> = paced.iter().map(|p| p.3).collect();
        let changes: Vec<(usize, i64)> = delays
            .windows(2)
            .enumerate()
            .filter(|(_, w)| w[0] != w[1])
            .map(|(k, w)| (k + 1, w[1]))
            .collect();
        // Start: 50 ms + one source period + half a master period + 2 ms. At 500: 110 ms
        // + 32 ms. Both forgotten 30 s after the first arrival: 10 ms + 32 ms.
        assert_eq!(delays[0], 82_000_000);
        assert_eq!(changes, [(500, 142_000_000), (1502, 42_000_000)]);
        assert_eq!(
            (142_000_000 - 42_000_000) % period as i64,
            0,
            "whole periods"
        );
        for (k, &(t, published, shown, d)) in paced.iter().enumerate() {
            if k != 500 {
                assert!(published + period / 4 <= ticks.tick_ns(shown), "{k}");
            }
            assert_eq!(
                shown,
                ticks.tick_at_or_after(t + d as u64).max(shown),
                "{k}"
            );
        }
    }

    /// A receiver that dates its pictures by presentation: they arrive up to 100 ms before
    /// their stream time, in bursts of four. The delay is negative, constant, and each
    /// picture still lands on the tick of its stream time, none overwritten unseen.
    #[test]
    fn pictures_ahead_of_their_stream_time_are_paced_too() {
        let source = FrameRate::FPS_25;
        let period = source.frame_offset_nanos(1) as u64;
        let ticks = TickGrid {
            origin_ns: 1_000_000_000,
            rate: FrameRate::FPS_50,
        };
        let mut pacer = Pacer::new(ticks);
        let mut shown = Vec::new();
        for k in 0..1_500u64 {
            let t = 5_000_000_000 + k * period;
            // Four pictures together, the first one 100 ms ahead of its stream time.
            let now = 5_000_000_000 + (k / 4 * 4 + 3) * period - 100_000_000 - 3 * period;
            let published = pacer.release_at(t, now, period).max(now) + 1_000_000;
            let tick = ticks.tick_at_or_after(published);
            let delay = pacer.delay.unwrap();
            if k > 0 {
                assert_eq!(
                    tick,
                    ticks.tick_at_or_after(t.saturating_add_signed(delay)),
                    "{k}"
                );
            }
            shown.push((tick, delay));
        }
        assert!(shown[1].1 < 0, "{:?}", shown[1]);
        assert!(shown.iter().skip(1).all(|s| s.1 == shown[1].1));
        assert!(shown.windows(2).skip(1).all(|w| w[1].0 > w[0].0));
    }

    /// The timestamps jump 2 s ahead, later the stream stalls 1 s: each time the pacer starts
    /// afresh from the next picture, without waiting for the jump nor keeping the stall in its
    /// delay; the pictures land on the tick of their (new) stream time.
    #[test]
    fn a_jump_of_the_timestamps_or_a_stall_starts_afresh() {
        let source = FrameRate::FPS_50;
        let period = source.frame_offset_nanos(1) as u64;
        let ticks = TickGrid {
            origin_ns: 1_000_000_000,
            rate: FrameRate::FPS_50,
        };
        let mut pacer = Pacer::new(ticks);
        let mut delays = Vec::new();
        for k in 0..2_000u64 {
            let arrival = 5_000_000_000 + k * period + 10_000_000;
            let t = 5_000_000_000 + k * period + if k >= 500 { 2_000_000_000 } else { 0 };
            let now = if k == 1_200 {
                arrival + 1_000_000_000
            } else {
                arrival
            };
            let release = pacer.release_at(t, now, period);
            assert!(
                release.saturating_sub(now) < 100_000_000,
                "{k}: waits too long"
            );
            delays.push(pacer.delay.unwrap());
        }
        let before = delays[499];
        assert_eq!(delays[500], before - 2_000_000_000);
        assert_eq!(delays[1_199], delays[500]);
        assert_eq!(
            delays[1_200],
            delays[500] + 1_000_000_000,
            "the stalled picture"
        );
        assert_eq!(delays[1_201], delays[500], "afresh after the stall");
        assert_eq!(delays[1_999], delays[500]);
    }
}
