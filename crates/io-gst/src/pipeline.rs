//! One GStreamer pipeline feeding one input (spec `io-gst.md`, « Transport des images »):
//! `<source> ! videoconvert ! appsink`, the appsink limited to the formats the compositor
//! uploads as is (NV12, I420, YUY2, UYVY, RGBA, BGRA). `videoconvert` stays in passthrough
//! for those; any other format is converted (counted).
//!
//! The appsink callback runs on the GStreamer streaming thread: it maps the buffer and pushes
//! its planes into the input's feed (`write_texture` there, ADR 0004), creating the feed when
//! the format is first known or changes. Bus errors and end of stream reach [`GstInput::poll`]
//! through a sync handler; a watchdog notices a source that stops sending.
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
}

impl Recipe {
    /// A video file, played at its own pace, paused or ended without being lost.
    pub(crate) fn file(path: PathBuf) -> Self {
        Self {
            media: Media::File(path),
            sync: true,
            watchdog: false,
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
        }
    }

    /// A capture device: live (lost when silent), pictures taken as they come.
    pub(crate) fn capture(capture: Capture) -> Self {
        Self {
            media: Media::Capture(capture),
            sync: false,
            watchdog: true,
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
    /// `Some` for a video file.
    player: Option<Player>,
    /// `Some` for a capture device: the mode it was opened in.
    mode: Option<CaptureMode>,
}

impl GstInput {
    /// Builds and starts the pipeline of `recipe` for `input`; pictures go to `feeds`.
    pub(crate) fn start<F: Feeds>(
        input: InputId,
        recipe: &Recipe,
        feeds: F,
    ) -> Result<Self, IoError> {
        let mut mode = None;
        let (pipeline, convert, sink) = match &recipe.media {
            Media::Launch(source) => launch_pipeline(source)?,
            Media::File(path) => file_pipeline(path)?,
            Media::Capture(capture) => {
                let (source, chosen) = open_device(capture)?;
                let parts = capture_pipeline(source, &chosen)?;
                tracing::info!(%input, device = %capture.device.0, mode = %chosen, "capture mode");
                mode = Some(chosen);
                parts
            }
        };
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
        }));
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
        Ok(Self {
            pipeline,
            shared,
            notes,
            watchdog: recipe.watchdog,
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
            0 if since > STARTUP_TIMEOUT => Some(format!("no picture after {STARTUP_TIMEOUT:?}")),
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
fn file_pipeline(path: &Path) -> Result<Parts, IoError> {
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

    route_decoded(&decode, &pipeline, &convert);
    Ok((pipeline, convert, sink))
}

/// Links the pads `decode` creates: the first video stream to `convert`, any other stream
/// (audio: Phase 3, or another video stream) to a `fakesink`.
fn route_decoded(decode: &gst::Element, pipeline: &gst::Pipeline, convert: &gst::Element) {
    let weak_pipeline = pipeline.downgrade();
    let weak_convert = convert.downgrade();
    decode.connect_pad_added(move |_, pad| {
        let Some(pipeline) = weak_pipeline.upgrade() else {
            return;
        };
        let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
        let is_video = caps
            .structure(0)
            .is_some_and(|s| s.name().starts_with("video/"));
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
            route_decoded(&decode, &pipeline, &convert);
        }
    }
    Ok((pipeline, convert, sink))
}

/// Links `pad` to a new `fakesink` (paced by the clock, like the video).
fn discard(pipeline: &gst::Pipeline, pad: &gst::Pad) {
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
    // Uncontended: GStreamer calls the two callbacks of one appsink one after the other.
    let mut stream = stream.lock().map_err(|_| gst::FlowError::Error)?;
    stream.on_sample(sink, preroll)
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
}

impl<F: Feeds> Stream<F> {
    fn on_sample(
        &mut self,
        sink: &gst_app::AppSink,
        preroll: bool,
    ) -> Result<gst::FlowSuccess, gst::FlowError> {
        let sample = if preroll {
            sink.pull_preroll()
        } else {
            sink.pull_sample()
        }
        .map_err(|_| gst::FlowError::Eos)?;
        let (Some(buffer), Some(caps)) = (sample.buffer(), sample.caps()) else {
            return Ok(gst::FlowSuccess::Ok);
        };
        let pts = buffer.pts();
        if preroll {
            if pts.is_some() && pts == self.preroll_pts {
                // Prerolled again on the picture already shown (seek to where it was).
                return Ok(gst::FlowSuccess::Ok);
            }
            self.preroll_pts = pts;
        } else if let Some(prerolled) = self.preroll_pts.take()
            && pts == Some(prerolled)
        {
            return Ok(gst::FlowSuccess::Ok);
        }
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
            .then(|| stream_frame(&sample, pts, format.rate))
            .flatten()
            .unwrap_or(self.next_frame);
        if let Some((_, out)) = &mut self.out
            && let Err(e) = out.push(planes.get(..count).unwrap_or_default(), source_frame)
        {
            tracing::warn!(input = %self.input, error = %e, "picture refused");
        }
        self.next_frame += 1;
        self.shared.pictures.fetch_add(1, Ordering::Relaxed);
        self.shared
            .last_picture
            .store(self.shared.since_start() + 1, Ordering::Release);
        Ok(gst::FlowSuccess::Ok)
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
    }

    /// CPU stand-in for the engine's feeds.
    #[derive(Debug, Clone, Default)]
    pub(crate) struct FakeFeeds {
        pub(crate) feeds: Arc<Mutex<Vec<Arc<Mutex<FakeFeed>>>>>,
    }

    pub(crate) struct FakeOut(Arc<Mutex<FakeFeed>>);

    impl PictureOut for FakeOut {
        fn push(&mut self, planes: &[Plane<'_>], frame: u64) -> Result<(), FrameError> {
            let mut f = self.0.lock().unwrap();
            f.frames.push(frame);
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
    }

    impl FakeFeeds {
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
}
