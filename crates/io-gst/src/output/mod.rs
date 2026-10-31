//! Outputs (ADR 0006, Phase 2 step 4): one GStreamer pipeline per output, fed by the engine's
//! distribution.
//!
//! [`GstOutput::record`] builds and starts the pipeline of a recording and returns its
//! [`OutputFeed`], the [`OutputConsumer`] to attach to the engine's `OutputHub`:
//!
//! ```text
//! appsrc (NV12, BT.709 limited, chroma left) ─► videoconvert ─► encoder ─► parser ─┐
//!                                                                                  ├─► mux ─► filesink
//! appsrc (F32 48 kHz stereo) ─► audioconvert ─► AAC encoder ─► aacparse ───────────┘
//! ```
//!
//! - Timestamps come from the frame index *n* only: frame *k* of the output (counted from the
//!   first frame it received) has `video_pts_ns(k)`, audio sample *s* has `s / 48 kHz`. A frame
//!   the engine could not read back leaves a gap of one period; the audio stays continuous.
//! - No clock (ADR 0006 § 3): the engine paces the frames; the muxer waits for both streams.
//! - The feed pushes with `block=true` into short `appsrc` queues: a slow output blocks its
//!   own thread, and the engine counts the frames it then loses (B6).
//! - Stop: detach the feed from the hub (dropping it sends EOS once its last frame is pushed),
//!   then [`GstOutput::stop`] waits for the file to be finalised.

mod encoder;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;

use vma_core::{Container, FrameRate, Resolution, VideoCodec};
use vma_engine::{
    AUDIO_CHANNELS, AUDIO_RATE, OutputConsumer, OutputFrame, audio_sample_at, video_pts_ns,
};

use crate::IoError;
pub(crate) use encoder::list as list_skipped;
use encoder::{Candidate, VideoTuning};
pub use encoder::{EncoderChoice, Skipped};

/// Video bit rate of a recording at 1080p50 (`phase-2.md` § 6), scaled with the pixel rate.
const RECORD_KBPS_1080P50: u64 = 40_000;
const AUDIO_KBPS: u32 = 160;
/// Pictures queued in the video `appsrc` before the feed blocks.
const VIDEO_QUEUE: u64 = 2;
/// Audio blocks queued in the audio `appsrc` before the feed blocks.
const AUDIO_QUEUE: u64 = 4;

/// The master format the outputs receive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputFormat {
    pub rate: FrameRate,
    pub resolution: Resolution,
}

/// A local recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordSettings {
    pub path: PathBuf,
    pub codec: VideoCodec,
    pub container: Container,
    /// Variable video bit rate, kbit/s.
    pub video_kbps: u32,
    /// Frames between key frames.
    pub gop_frames: u32,
    pub audio_kbps: u32,
}

impl RecordSettings {
    /// Defaults of `phase-2.md` § 6: H.264 in MKV, ≈ 40 Mb/s at 1080p50 (scaled with the pixel
    /// rate), a key frame every second, AAC 160 kb/s.
    pub fn new(path: PathBuf, format: OutputFormat) -> Self {
        let pixels = u64::from(format.resolution.width()) * u64::from(format.resolution.height());
        let (num, den) = (u64::from(format.rate.num()), u64::from(format.rate.den()));
        let kbps = RECORD_KBPS_1080P50 * pixels * num / (1920 * 1080 * 50 * den.max(1));
        Self {
            path,
            codec: VideoCodec::default(),
            container: Container::default(),
            video_kbps: u32::try_from(kbps.max(500)).unwrap_or(u32::MAX),
            gop_frames: u32::try_from(num.div_ceil(den.max(1))).unwrap_or(1),
            audio_kbps: AUDIO_KBPS,
        }
    }
}

/// What a pipeline is made of (crate-internal: tests also use raw streams and other sinks).
#[derive(Debug, Clone)]
pub(crate) struct Plan {
    pub video: Video,
    pub audio: Audio,
    pub container: Container,
    pub sink: Sink,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Video {
    Encoded {
        codec: VideoCodec,
        tuning: VideoTuning,
    },
    /// I420 as is (MKV only): PTS tests independent of the encoders.
    #[cfg(test)]
    Raw,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Audio {
    Aac {
        kbps: u32,
    },
    /// F32 as is (MKV only).
    #[cfg(test)]
    Raw,
}

#[derive(Debug, Clone)]
pub(crate) enum Sink {
    File(PathBuf),
    /// `gst-launch` description of the sink bin (tests: a slow or failing sink).
    #[cfg(test)]
    Launch(String),
}

impl Plan {
    fn recording(settings: &RecordSettings) -> Self {
        Self {
            video: Video::Encoded {
                codec: settings.codec,
                tuning: VideoTuning {
                    kbps: settings.video_kbps,
                    cbr: false,
                    gop: settings.gop_frames.max(1),
                },
            },
            audio: Audio::Aac {
                kbps: settings.audio_kbps,
            },
            container: settings.container,
            sink: Sink::File(settings.path.clone()),
        }
    }
}

/// What the bus tells [`GstOutput`].
#[derive(Debug)]
enum Note {
    Failed(String),
    Finished,
}

/// A running output pipeline. Dropping it stops it at once (state `NULL`, file not finalised
/// for MP4: use [`Self::stop`]).
#[derive(Debug)]
pub struct GstOutput {
    pipeline: gst::Pipeline,
    notes: Receiver<Note>,
    video_src: gst_app::AppSrc,
    audio_src: gst_app::AppSrc,
    sink: Option<gst::Element>,
    video_encoder: Option<EncoderChoice>,
    audio_encoder: Option<EncoderChoice>,
    failure: Option<String>,
    /// The end of stream reached the sink: the file is finalised.
    finished: bool,
}

impl GstOutput {
    /// Starts a recording to `settings.path`; frames go in through the returned feed. Fails
    /// (nothing started) without a usable encoder, or if the file cannot be opened.
    pub fn record(
        format: OutputFormat,
        settings: &RecordSettings,
    ) -> Result<(Self, OutputFeed), IoError> {
        Self::start(
            format,
            &Plan::recording(settings),
            &mut encoder::probe_element,
        )
    }

    pub(crate) fn start(
        format: OutputFormat,
        plan: &Plan,
        probe: &mut dyn FnMut(&'static str) -> Result<gst::Element, String>,
    ) -> Result<(Self, OutputFeed), IoError> {
        let pipeline = gst::Pipeline::new();
        // Paced by the engine, never by a clock (ADR 0006 § 3).
        pipeline.use_clock(None::<&gst::Clock>);
        let (video_src, video_tail, video_encoder) = video_branch(&pipeline, format, plan, probe)?;
        let (audio_src, audio_tail, audio_encoder) = audio_branch(&pipeline, plan, probe)?;
        let mux = match plan.container {
            Container::Mkv => {
                let mux = element("matroskamux")?;
                // No index to write at the end: readable up to the last block after a crash.
                mux.set_property("streamable", true);
                mux.set_property("writing-app", "VMA");
                mux
            }
            Container::Mp4 => {
                let mux = element("mp4mux")?;
                // Fragments of 1 s: readable up to the last one after a crash.
                mux.set_property("fragment-duration", 1000u32);
                encoder::set(&mux, "fragment-mode", "first-moov-then-finalise");
                mux
            }
        };
        let sink = match &plan.sink {
            Sink::File(path) => {
                let sink = element("filesink")?;
                sink.set_property("location", path.to_string_lossy().as_ref());
                // Each block reaches the file at once (B4: little is lost to a crash).
                encoder::set(&sink, "buffer-mode", "unbuffered");
                sink.set_property("sync", false);
                sink
            }
            #[cfg(test)]
            Sink::Launch(description) => gst::parse::bin_from_description(description, true)
                .map_err(|e| IoError::Pipeline(e.to_string()))?
                .upcast(),
        };
        add(&pipeline, &[&mux, &sink])?;
        link_pad(&video_tail, &mux, "video_%u")?;
        link_pad(&audio_tail, &mux, "audio_%u")?;
        mux.link(&sink)
            .map_err(|e| IoError::Pipeline(format!("mux → sink: {e}")))?;

        let (notes_tx, notes) = crossbeam_channel::bounded(16);
        if let Some(bus) = pipeline.bus() {
            let sources = [video_src.downgrade(), audio_src.downgrade()];
            bus.set_sync_handler(move |_, message| {
                if let gst::MessageView::Error(_) = message.view() {
                    // Downstream stopped: wake a feed blocked on a full `appsrc` (its push
                    // then fails and the feed closes) instead of leaving it blocked.
                    for src in sources.iter().filter_map(|s| s.upgrade()) {
                        let _ = src.end_of_stream();
                    }
                }
                relay(&notes_tx, message);
                gst::BusSyncReply::Drop
            });
        }
        if let Err(e) = pipeline.set_state(gst::State::Playing) {
            let _ = pipeline.set_state(gst::State::Null);
            let reason = notes
                .try_iter()
                .find_map(|n| match n {
                    Note::Failed(reason) => Some(reason),
                    Note::Finished => None,
                })
                .unwrap_or_else(|| e.to_string());
            return Err(IoError::Pipeline(format!("cannot start: {reason}")));
        }
        let file_sink = matches!(plan.sink, Sink::File(_)).then(|| sink.clone());
        tracing::info!(
            video = %video_encoder.as_ref().map_or("raw".into(), ToString::to_string),
            audio = %audio_encoder.as_ref().map_or("raw".into(), ToString::to_string),
            container = ?plan.container,
            "output started"
        );
        let feed = OutputFeed {
            video: video_src.clone(),
            audio: audio_src.clone(),
            rate: format.rate,
            origin: None,
            audio_started: false,
            closed: false,
        };
        let output = Self {
            pipeline,
            notes,
            video_src,
            audio_src,
            sink: file_sink,
            video_encoder,
            audio_encoder,
            failure: None,
            finished: false,
        };
        Ok((output, feed))
    }

    /// The video encoder retained (`None`: raw video).
    pub fn video_encoder(&self) -> Option<&EncoderChoice> {
        self.video_encoder.as_ref()
    }

    /// The AAC encoder retained (`None`: raw audio).
    pub fn audio_encoder(&self) -> Option<&EncoderChoice> {
        self.audio_encoder.as_ref()
    }

    /// `Some(reason)` once the output has failed (write error, disk full, encoder error). The
    /// pipeline is then stopped; the rest of the program carries on.
    pub fn poll(&mut self) -> Option<&str> {
        while self.failure.is_none() {
            match self.notes.try_recv() {
                Ok(Note::Failed(reason)) => {
                    tracing::error!(%reason, "output failed");
                    let _ = self.pipeline.set_state(gst::State::Null);
                    self.failure = Some(reason);
                }
                Ok(Note::Finished) => self.finished = true,
                Err(_) => break,
            }
        }
        self.failure.as_deref()
    }

    /// Bytes written so far (file outputs).
    pub fn bytes(&self) -> Option<u64> {
        // Asked of the sink pad: `filesink` answers there in bytes.
        let pad = self.sink.as_ref()?.static_pad("sink")?;
        let position = pad.query_position::<gst::format::Bytes>()?;
        Some(*position)
    }

    /// Finalises the output: waits for the end of stream sent when the feed is dropped (at
    /// most half of `timeout`), sends it itself otherwise, then waits for the muxer to finish
    /// the file. `Err` if it failed or was not finalised in time (the pipeline stops anyway).
    pub fn stop(mut self, timeout: Duration) -> Result<(), IoError> {
        if let Some(reason) = self.poll() {
            return Err(IoError::Pipeline(reason.to_owned()));
        }
        let start = Instant::now();
        let mut forced = false;
        while !self.finished {
            let elapsed = start.elapsed();
            if !forced && elapsed >= timeout / 2 {
                tracing::warn!("output feed still attached: ending the stream here");
                let _ = self.video_src.end_of_stream();
                let _ = self.audio_src.end_of_stream();
                forced = true;
            }
            let Some(left) = timeout.checked_sub(elapsed) else {
                return Err(IoError::Pipeline(format!(
                    "not finalised within {timeout:?}"
                )));
            };
            match self.notes.recv_timeout(left.min(Duration::from_millis(20))) {
                Ok(Note::Finished) => self.finished = true,
                Ok(Note::Failed(reason)) => return Err(IoError::Pipeline(reason)),
                Err(_) => {}
            }
        }
        tracing::info!("output finalised");
        Ok(())
    }
}

impl Drop for GstOutput {
    fn drop(&mut self) {
        if let Err(e) = self.pipeline.set_state(gst::State::Null) {
            tracing::warn!(error = %e, "output pipeline did not stop cleanly");
        }
    }
}

/// The engine's end of one output pipeline: pushes each frame into the two `appsrc`. Runs on
/// the output's own thread (`vma-out-<id>`). Dropping it ends the stream (EOS).
#[derive(Debug)]
pub struct OutputFeed {
    video: gst_app::AppSrc,
    audio: gst_app::AppSrc,
    rate: FrameRate,
    origin: Option<Origin>,
    audio_started: bool,
    /// The pipeline refused a buffer (stopping, failed): nothing more is pushed.
    closed: bool,
}

/// The first frame this output received: its time 0.
#[derive(Debug, Clone, Copy)]
struct Origin {
    frame: u64,
    sample: u64,
}

impl OutputConsumer for OutputFeed {
    fn consume(&mut self, frame: Arc<OutputFrame>) {
        if self.closed {
            return;
        }
        let rate = self.rate;
        let origin = *self.origin.get_or_insert(Origin {
            frame: frame.index,
            sample: audio_sample_at(rate, frame.index),
        });
        let Some(k) = frame.index.checked_sub(origin.frame) else {
            return;
        };
        let audio = audio_buffer(&frame, origin, !self.audio_started);
        match video_buffer(Arc::clone(&frame), rate, k) {
            Ok(buffer) => self.push(true, buffer),
            Err(e) => tracing::warn!(error = %e, "output picture not wrapped"),
        }
        if let Some(buffer) = audio {
            self.audio_started = true;
            self.push(false, buffer);
        }
    }
}

impl OutputFeed {
    fn push(&mut self, video: bool, buffer: gst::Buffer) {
        if self.closed {
            return;
        }
        let src = if video { &self.video } else { &self.audio };
        if let Err(e) = src.push_buffer(buffer) {
            if !matches!(e, gst::FlowError::Flushing | gst::FlowError::Eos) {
                tracing::warn!(error = ?e, "output refused a buffer: feed closed");
            }
            self.closed = true;
        }
    }
}

impl Drop for OutputFeed {
    fn drop(&mut self) {
        let _ = self.video.end_of_stream();
        let _ = self.audio.end_of_stream();
    }
}

/// The NV12 bytes of a shared frame, wrapped without a copy.
struct Nv12(Arc<OutputFrame>);

impl AsRef<[u8]> for Nv12 {
    fn as_ref(&self) -> &[u8] {
        &self.0.nv12
    }
}

/// Picture `k` of the output.
fn video_buffer(
    frame: Arc<OutputFrame>,
    rate: FrameRate,
    k: u64,
) -> Result<gst::Buffer, gst::glib::BoolError> {
    let (width, height) = (frame.resolution.width(), frame.resolution.height());
    let luma = frame.luma_stride();
    let offsets = [0, luma * height as usize];
    let strides = [stride(luma), stride(frame.chroma_stride())];
    let pts = video_pts_ns(rate, k);
    let duration = video_pts_ns(rate, k.saturating_add(1)).saturating_sub(pts);
    let mut buffer = gst::Buffer::from_slice(Nv12(frame));
    let b = buffer.make_mut();
    b.set_pts(gst::ClockTime::from_nseconds(pts));
    b.set_duration(gst::ClockTime::from_nseconds(duration));
    b.set_offset(k);
    if k == 0 {
        b.set_flags(gst::BufferFlags::DISCONT);
    }
    gst_video::VideoMeta::add_full(
        b,
        gst_video::VideoFrameFlags::empty(),
        gst_video::VideoFormat::Nv12,
        width,
        height,
        &offsets,
        &strides,
    )?;
    Ok(buffer)
}

fn stride(bytes: usize) -> i32 {
    i32::try_from(bytes).unwrap_or(i32::MAX)
}

/// The audio of `frame` from the output's first sample on (`None`: nothing left).
fn audio_buffer(frame: &OutputFrame, origin: Origin, first: bool) -> Option<gst::Buffer> {
    let skip = origin
        .sample
        .saturating_sub(frame.audio_first_sample)
        .min(frame.audio_samples());
    let count = frame.audio_samples() - skip;
    if count == 0 {
        return None;
    }
    let start = frame.audio_first_sample + skip - origin.sample;
    let channels = AUDIO_CHANNELS as u64;
    let samples = frame.audio.get(usize::try_from(skip * channels).ok()?..)?;
    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let pts = sample_ns(start);
    let mut buffer = gst::Buffer::from_mut_slice(bytes);
    let b = buffer.make_mut();
    b.set_pts(gst::ClockTime::from_nseconds(pts));
    b.set_duration(gst::ClockTime::from_nseconds(
        sample_ns(start + count).saturating_sub(pts),
    ));
    b.set_offset(start);
    b.set_offset_end(start + count);
    if first || frame.discont {
        b.set_flags(gst::BufferFlags::DISCONT);
    }
    Some(buffer)
}

/// Time of audio sample `sample`, in ns (rounded up, like [`video_pts_ns`]).
fn sample_ns(sample: u64) -> u64 {
    let ns = u128::from(sample) * 1_000_000_000;
    u64::try_from(ns.div_ceil(u128::from(AUDIO_RATE))).unwrap_or(u64::MAX)
}

/// `appsrc (NV12) ! videoconvert ! encoder ! parser` or `… ! videoconvert ! I420`.
fn video_branch(
    pipeline: &gst::Pipeline,
    format: OutputFormat,
    plan: &Plan,
    probe: &mut dyn FnMut(&'static str) -> Result<gst::Element, String>,
) -> Result<(gst_app::AppSrc, gst::Element, Option<EncoderChoice>), IoError> {
    let resolution = format.resolution;
    let info = gst_video::VideoInfo::builder(
        gst_video::VideoFormat::Nv12,
        resolution.width(),
        resolution.height(),
    )
    .fps(gst::Fraction::new(
        i32::try_from(format.rate.num()).unwrap_or(i32::MAX),
        i32::try_from(format.rate.den()).unwrap_or(1),
    ))
    // ADR 0005: Y'CbCr BT.709 limited range, chroma sited left (MPEG-2 / H.264).
    .colorimetry(&gst_video::VideoColorimetry::new(
        gst_video::VideoColorRange::Range16_235,
        gst_video::VideoColorMatrix::Bt709,
        gst_video::VideoTransferFunction::Bt709,
        gst_video::VideoColorPrimaries::Bt709,
    ))
    .chroma_site(gst_video::VideoChromaSite::MPEG2)
    .build()
    .map_err(|e| IoError::Pipeline(format!("video format: {e}")))?;
    let caps = info
        .to_caps()
        .map_err(|e| IoError::Pipeline(format!("video caps: {e}")))?;
    let frame_bytes = u64::try_from(info.size()).unwrap_or(u64::MAX);
    let src = appsrc("vma_video", &caps, frame_bytes.saturating_mul(VIDEO_QUEUE));
    let convert = element("videoconvert")?;
    let mut chain: Vec<gst::Element> = vec![src.clone().upcast(), convert];
    let choice = match plan.video {
        Video::Encoded { codec, tuning } => {
            let (encoder, candidate, choice) =
                pick(encoder::video_ladder(codec), probe, codec_name(codec))?;
            encoder::tune_video(&encoder, candidate.family, tuning);
            chain.push(encoder);
            chain.push(element(match codec {
                VideoCodec::H264 => "h264parse",
                VideoCodec::Hevc => "h265parse",
            })?);
            Some(choice)
        }
        #[cfg(test)]
        Video::Raw => {
            let filter = element("capsfilter")?;
            filter.set_property(
                "caps",
                gst_video::VideoCapsBuilder::new()
                    .format(gst_video::VideoFormat::I420)
                    .build(),
            );
            chain.push(filter);
            None
        }
    };
    chain.push(element("queue")?);
    let refs: Vec<&gst::Element> = chain.iter().collect();
    add(pipeline, &refs)?;
    gst::Element::link_many(&refs).map_err(|e| IoError::Pipeline(format!("video branch: {e}")))?;
    let tail = chain
        .last()
        .cloned()
        .ok_or_else(|| IoError::Pipeline("empty video branch".into()))?;
    Ok((src, tail, choice))
}

/// `appsrc (F32 48 kHz stereo) ! audioconvert ! AAC encoder ! aacparse`, or the raw samples.
fn audio_branch(
    pipeline: &gst::Pipeline,
    plan: &Plan,
    probe: &mut dyn FnMut(&'static str) -> Result<gst::Element, String>,
) -> Result<(gst_app::AppSrc, gst::Element, Option<EncoderChoice>), IoError> {
    let caps = gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("layout", "interleaved")
        .field("rate", i32::try_from(AUDIO_RATE).unwrap_or(48_000))
        .field("channels", i32::try_from(AUDIO_CHANNELS).unwrap_or(2))
        .field("channel-mask", gst::Bitmask::new(0x3))
        .build();
    // One block is one frame's worth (≤ 1 601 samples at 29.97p): a few blocks of slack.
    let block = u64::from(AUDIO_RATE) / 25 * AUDIO_CHANNELS as u64 * 4;
    let src = appsrc("vma_audio", &caps, block * AUDIO_QUEUE);
    let mut chain: Vec<gst::Element> = vec![src.clone().upcast()];
    let choice = match plan.audio {
        Audio::Aac { kbps } => {
            let (encoder, _, choice) = pick(encoder::AAC, probe, "AAC")?;
            encoder::tune_audio(&encoder, kbps);
            chain.push(element("audioconvert")?);
            chain.push(encoder);
            chain.push(element("aacparse")?);
            Some(choice)
        }
        #[cfg(test)]
        Audio::Raw => None,
    };
    chain.push(element("queue")?);
    let refs: Vec<&gst::Element> = chain.iter().collect();
    add(pipeline, &refs)?;
    gst::Element::link_many(&refs).map_err(|e| IoError::Pipeline(format!("audio branch: {e}")))?;
    let tail = chain
        .last()
        .cloned()
        .ok_or_else(|| IoError::Pipeline("empty audio branch".into()))?;
    Ok((src, tail, choice))
}

fn pick(
    ladder: &[Candidate],
    probe: &mut dyn FnMut(&'static str) -> Result<gst::Element, String>,
    what: &'static str,
) -> Result<(gst::Element, Candidate, EncoderChoice), IoError> {
    encoder::select(ladder, probe).map_err(|tried| IoError::NoEncoder { what, tried })
}

fn codec_name(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "H.264",
        VideoCodec::Hevc => "HEVC",
    }
}

/// An `appsrc` in time format, timestamps set by the feed, blocking beyond `max_bytes`.
fn appsrc(name: &str, caps: &gst::Caps, max_bytes: u64) -> gst_app::AppSrc {
    gst_app::AppSrc::builder()
        .name(name)
        .caps(caps)
        .format(gst::Format::Time)
        .is_live(true)
        .do_timestamp(false)
        .block(true)
        .max_bytes(max_bytes)
        .build()
}

fn element(name: &str) -> Result<gst::Element, IoError> {
    gst::ElementFactory::make(name)
        .build()
        .map_err(|_| IoError::Pipeline(format!("GStreamer element {name} missing")))
}

fn add(pipeline: &gst::Pipeline, elements: &[&gst::Element]) -> Result<(), IoError> {
    pipeline
        .add_many(elements.iter().copied())
        .map_err(|e| IoError::Pipeline(e.to_string()))
}

/// Links `from` to a new request pad of `mux` (by template, not by caps: they are not known
/// yet behind the queue).
fn link_pad(from: &gst::Element, mux: &gst::Element, template: &str) -> Result<(), IoError> {
    let sink = mux
        .request_pad_simple(template)
        .ok_or_else(|| IoError::Pipeline(format!("muxer has no {template} pad")))?;
    let src = from
        .static_pad("src")
        .ok_or_else(|| IoError::Pipeline("branch has no src pad".into()))?;
    src.link(&sink)
        .map(|_| ())
        .map_err(|e| IoError::Pipeline(format!("branch → mux: {e:?}")))
}

fn relay(notes: &Sender<Note>, message: &gst::Message) {
    let note = match message.view() {
        gst::MessageView::Error(e) => Note::Failed(format!(
            "{} ({})",
            e.error(),
            e.debug().map(|d| d.to_string()).unwrap_or_default()
        )),
        gst::MessageView::Eos(_) => Note::Finished,
        _ => return,
    };
    let _ = notes.try_send(note);
}
