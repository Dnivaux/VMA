//! « Master de l'instant » (`phase-6.md` Q12, ADR 0016): while the replay records, the Master
//! is written into the session beside the cameras' sound, in the same format
//! (`<session>/master/sound`), dated by `vma-audio` by the moment of the pictures on air, so
//! that the player can play it on the recorded instants of any camera.
//!
//! Its own thread: the disk never holds `vma-io`, and `vma-audio` only writes into a ring. A
//! write that fails (a full disk) stops this recording alone, reported in the log and in
//! [`MasterRecordStats`]; the cameras' recorders on the same disk report their own failures.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use vma_audio::{CHANNELS, RingReader, SAMPLE_RATE};
use vma_replay::SegmentWriter;

use crate::record::SEGMENT_NS;

/// The ring is read this often: a run of sound of about this length each time.
const POLL: Duration = Duration::from_millis(20);
/// Longest run written at once (frames, 0.5 s): after a stall the backlog is cut in runs.
const MAX_RUN: usize = SAMPLE_RATE as usize / 2;

/// Counters of the Master's recording.
#[derive(Debug, Default)]
pub struct MasterRecordStats {
    /// Frames written to the session.
    pub frames: AtomicU64,
    /// Frames lost: overwritten in the ring before they were written (the disk stalled for
    /// longer than the ring holds).
    pub lost: AtomicU64,
    /// The recording stopped on an error.
    pub failed: AtomicBool,
}

impl MasterRecordStats {
    pub fn snapshot(&self) -> MasterRecordActivity {
        MasterRecordActivity {
            frames: self.frames.load(Ordering::Relaxed),
            lost: self.lost.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
        }
    }
}

/// A copy of [`MasterRecordStats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MasterRecordActivity {
    pub frames: u64,
    pub lost: u64,
    pub failed: bool,
}

/// The Master being recorded into `folder`.
pub(crate) struct MasterRecorder {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<RingReader>>,
    stats: Arc<MasterRecordStats>,
}

impl std::fmt::Debug for MasterRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MasterRecorder").finish_non_exhaustive()
    }
}

impl MasterRecorder {
    /// Records from now on what `ring` receives into `folder`, older sound than `keep_ns`
    /// deleted. On an error the ring is handed back, unless no thread could be started (it
    /// went with the thread's closure).
    pub(crate) fn start(
        ring: RingReader,
        folder: PathBuf,
        keep_ns: u64,
    ) -> Result<Self, (Option<RingReader>, String)> {
        let writer = match SegmentWriter::sound(&folder, SEGMENT_NS, Some(keep_ns)) {
            Ok(w) => w,
            Err(e) => return Err((Some(ring), e.to_string())),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(MasterRecordStats::default());
        let (stop_in, stats_in) = (Arc::clone(&stop), Arc::clone(&stats));
        // From now, not from when the thread gets going.
        let from = ring.written();
        // The ring comes back through the thread's result.
        let thread = std::thread::Builder::new()
            .name("vma-replay-master".into())
            .spawn(move || run(ring, from, writer, &stop_in, &stats_in))
            .map_err(|e| (None, e.to_string()))?;
        Ok(Self {
            stop,
            thread: Some(thread),
            stats,
        })
    }

    pub(crate) fn stats(&self) -> Arc<MasterRecordStats> {
        Arc::clone(&self.stats)
    }

    /// Writes what is left and stops; the ring back (`None` if the thread panicked).
    pub(crate) fn finish(mut self) -> Option<RingReader> {
        self.stop.store(true, Ordering::Release);
        let thread = self.thread.take()?;
        match thread.join() {
            Ok(ring) => Some(ring),
            Err(_) => {
                tracing::error!("the Master's recorder panicked");
                None
            }
        }
    }
}

impl Drop for MasterRecorder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("the Master's recorder panicked");
        }
    }
}

fn run(
    ring: RingReader,
    from: u64,
    mut writer: SegmentWriter,
    stop: &AtomicBool,
    stats: &MasterRecordStats,
) -> RingReader {
    let mut next = from;
    let mut samples = vec![0.0f32; MAX_RUN * CHANNELS];
    let mut bytes = Vec::with_capacity(MAX_RUN * CHANNELS * 4);
    let mut failed = false;
    loop {
        let stopping = stop.load(Ordering::Acquire);
        let written = ring.written();
        // Fallen behind the ring (a long stall): on from what it still holds.
        let oldest = written.saturating_sub(ring.capacity());
        if next < oldest {
            stats.lost.fetch_add(oldest - next, Ordering::Relaxed);
            next = oldest;
        }
        while !failed && next < written {
            let Some(anchor) = ring.anchor() else {
                break;
            };
            let frames = (written - next).min(MAX_RUN as u64) as usize;
            let Some(run) = samples.get_mut(..frames * CHANNELS) else {
                break;
            };
            let outcome = ring.read(next as i64, run);
            stats
                .lost
                .fetch_add(outcome.overwritten as u64, Ordering::Relaxed);
            // The date of a frame from the latest block's: the blocks of `vma-audio` follow
            // each other, their dates too (a change of the input on air moves them by a few
            // milliseconds, which the player takes as it takes the jitter of a camera's).
            let offset = (i128::from(next) - i128::from(anchor.position)) * 1_000_000_000
                / i128::from(SAMPLE_RATE);
            let at_ns = u64::try_from(i128::from(anchor.time_ns) + offset).unwrap_or(0);
            bytes.clear();
            bytes.extend(run.iter().flat_map(|v| v.to_le_bytes()));
            if let Err(e) = writer.write(at_ns, &bytes) {
                tracing::error!(error = %e, "the Master's recording stopped");
                stats.failed.store(true, Ordering::Relaxed);
                failed = true;
                break;
            }
            stats.frames.fetch_add(frames as u64, Ordering::Relaxed);
            next += frames as u64;
        }
        if stopping {
            break;
        }
        std::thread::sleep(POLL);
    }
    ring
}

#[cfg(test)]
mod tests {
    use super::*;
    use vma_replay::SegmentReader;

    /// What the ring receives is written into the session with the dates of its blocks, run
    /// after run, and the ring comes back at the end.
    #[test]
    fn the_master_is_written_with_its_dates() {
        let dir = std::env::temp_dir().join(format!("vma-master-record-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (mut w, r) = vma_audio::ring(vma_audio::RING_FRAMES);
        // Before the start: not recorded.
        w.push(1_000, &[9.0; 960 * 2]);
        let recorder = MasterRecorder::start(r, dir.clone(), 60_000_000_000).unwrap();
        let start = 5_000_000_000u64;
        for n in 0..50u64 {
            // 20 ms blocks, the value telling the block.
            w.push(start + n * 20_000_000, &vec![n as f32; 960 * 2]);
            std::thread::sleep(Duration::from_millis(5));
        }
        let stats = recorder.stats();
        let ring = recorder.finish();
        assert!(ring.is_some(), "the ring is handed back");
        assert_eq!(stats.frames.load(Ordering::Relaxed), 50 * 960);
        assert_eq!(stats.lost.load(Ordering::Relaxed), 0);
        let reader = SegmentReader::open_sound(&dir);
        let mut frames = 0u64;
        for entry in reader.entries() {
            let run = reader.read(entry).unwrap();
            let first = f32::from_le_bytes(run[..4].try_into().unwrap());
            // Its date is the one of the block it starts in, plus its place in that block.
            let block = first as u64;
            let into = entry.at_ns - (start + block * 20_000_000);
            assert!(into < 20_000_000, "run at {} in block {block}", entry.at_ns);
            assert_eq!(
                (frames % 960) * 1_000_000_000 / 48_000,
                into,
                "a run dated where it starts"
            );
            frames += u64::from(entry.len) / 8;
        }
        assert_eq!(frames, 50 * 960, "no frame before the start, none lost");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
