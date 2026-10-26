//! Command journal: one JSON line per command (accepted or refused) and per state event,
//! written by a dedicated `vma-journal` thread (spec `commands.md`, « Journal »).
//!
//! `vma-control` formats the line and hands it over a bounded channel; when the channel is
//! full the line is dropped and counted: real time first. A journal can be replayed onto a
//! fresh state ([`replay`]) and must give the same state back.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::SystemTime;

use crossbeam_channel::{Receiver, Sender, TrySendError};
use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandId, Source};
use crate::state::{Event, Rejection, State};

/// Lines waiting to be written before new ones are dropped.
const JOURNAL_CAPACITY: usize = 4096;

/// Where the journal goes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Journal {
    #[default]
    Disabled,
    /// `commands-YYYY-MM-DD.jsonl` files (UTC date) in this directory, created if needed.
    Directory(PathBuf),
}

/// Who caused a journal line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Ui,
    Api,
    Script,
    /// A state event reported by the render thread.
    Render,
}

impl From<Source> for Origin {
    fn from(source: Source) -> Self {
        match source {
            Source::Ui => Self::Ui,
            Source::Api => Self::Api,
            Source::Script => Self::Script,
        }
    }
}

/// Result of a command, as journaled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Ok,
    Rejected(String),
}

impl From<&Result<(), Rejection>> for Outcome {
    fn from(result: &Result<(), Rejection>) -> Self {
        match result {
            Ok(()) => Self::Ok,
            Err(r) => Self::Rejected(r.to_string()),
        }
    }
}

/// One journal line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalLine {
    /// UTC wall-clock time, RFC 3339 with milliseconds (for humans; never used for timing).
    pub utc: String,
    /// Last tick processed by the render thread when the line was written.
    pub frame: u64,
    pub source: Origin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<CommandId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Command>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<Event>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Outcome>,
    /// State version after the line.
    pub version: u64,
}

/// A formatted line and the UTC day of the file it belongs to.
#[derive(Debug)]
pub(crate) struct JournalEntry {
    pub day: String,
    pub line: String,
}

/// Counters of the journal (readable from any thread).
#[derive(Debug, Default)]
pub struct JournalStats {
    written: AtomicU64,
    dropped: AtomicU64,
    write_errors: AtomicU64,
}

impl JournalStats {
    pub fn written(&self) -> u64 {
        self.written.load(Ordering::Relaxed)
    }

    /// Lines lost because the writer could not keep up.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Lines lost to I/O errors (logged).
    pub fn write_errors(&self) -> u64 {
        self.write_errors.load(Ordering::Relaxed)
    }
}

/// `vma-control`'s end of the journal.
#[derive(Debug)]
pub(crate) struct JournalSender {
    tx: Sender<JournalEntry>,
    stats: Arc<JournalStats>,
}

impl JournalSender {
    pub(crate) fn new(tx: Sender<JournalEntry>, stats: Arc<JournalStats>) -> Self {
        Self { tx, stats }
    }

    /// Never blocks: a full channel drops the line.
    pub(crate) fn send(&self, line: &JournalLine) {
        let day = line.utc.get(..10).unwrap_or("unknown").to_owned();
        let text = match serde_json::to_string(line) {
            Ok(text) => text,
            Err(error) => {
                tracing::error!(%error, "journal line not serialisable");
                self.stats.write_errors.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };
        match self.tx.try_send(JournalEntry { day, line: text }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {
                self.stats.write_errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Starts the `vma-journal` writer thread for `dir`. It stops once every sender is dropped.
pub(crate) fn spawn_writer(
    dir: PathBuf,
    stats: Arc<JournalStats>,
) -> std::io::Result<(JournalSender, JoinHandle<()>)> {
    let (tx, rx) = crossbeam_channel::bounded(JOURNAL_CAPACITY);
    let writer_stats = Arc::clone(&stats);
    let thread = std::thread::Builder::new()
        .name("vma-journal".into())
        .spawn(move || write_loop(&dir, &rx, &writer_stats))?;
    Ok((JournalSender::new(tx, stats), thread))
}

fn write_loop(dir: &Path, rx: &Receiver<JournalEntry>, stats: &JournalStats) {
    let mut current: Option<(String, BufWriter<File>)> = None;
    while let Ok(entry) = rx.recv() {
        if current.as_ref().is_none_or(|(day, _)| *day != entry.day) {
            current = open_day(dir, &entry.day).map(|f| (entry.day.clone(), f));
        }
        let Some((_, file)) = current.as_mut() else {
            stats.write_errors.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        if let Err(error) = writeln!(file, "{}", entry.line) {
            tracing::error!(%error, "journal write failed");
            stats.write_errors.fetch_add(1, Ordering::Relaxed);
            current = None;
            continue;
        }
        stats.written.fetch_add(1, Ordering::Relaxed);
        // Flush when caught up, so the file is complete whenever the mixer is idle.
        if rx.is_empty()
            && let Err(error) = file.flush()
        {
            tracing::error!(%error, "journal flush failed");
            current = None;
        }
    }
    if let Some((_, mut file)) = current
        && let Err(error) = file.flush()
    {
        tracing::error!(%error, "journal flush failed");
    }
}

fn open_day(dir: &Path, day: &str) -> Option<BufWriter<File>> {
    let path = dir.join(format!("commands-{day}.jsonl"));
    let opened = std::fs::create_dir_all(dir)
        .and_then(|()| OpenOptions::new().create(true).append(true).open(&path));
    match opened {
        Ok(file) => {
            tracing::info!(path = %path.display(), "journal file opened");
            Some(BufWriter::new(file))
        }
        Err(error) => {
            tracing::error!(%error, path = %path.display(), "cannot open the journal file");
            None
        }
    }
}

/// UTC time as RFC 3339 with milliseconds: `2026-10-27T14:03:12.481Z`.
pub(crate) fn utc_rfc3339(t: SystemTime) -> String {
    let since = t.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    let secs = since.as_secs();
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60,
        since.subsec_millis()
    )
}

/// Days since 1970-01-01 → (year, month, day), proleptic Gregorian (H. Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// Why a replay failed.
#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("line {line}: {source}")]
    Parse {
        line: usize,
        source: serde_json::Error,
    },
    #[error("line {line}: replay diverged ({detail})")]
    Diverged { line: usize, detail: String },
}

/// Replays a journal (JSON Lines) onto `initial` and returns the final state. Checks, line
/// by line, that each command gets the same outcome and the state the same version.
pub fn replay(initial: State, journal: &str) -> Result<State, ReplayError> {
    let mut state = initial;
    for (index, text) in journal.lines().enumerate() {
        let line = index + 1;
        if text.trim().is_empty() {
            continue;
        }
        let entry: JournalLine =
            serde_json::from_str(text).map_err(|source| ReplayError::Parse { line, source })?;
        let diverged = |detail: String| ReplayError::Diverged { line, detail };
        if let Some(command) = &entry.command {
            let result = state.apply(command).map(|_| ());
            let outcome = Outcome::from(&result);
            if entry.result.as_ref() != Some(&outcome) {
                return Err(diverged(format!(
                    "journal says {:?}, replay gives {outcome:?}",
                    entry.result
                )));
            }
        }
        if let Some(event) = &entry.event {
            state.apply_event(event);
        }
        if state.version != entry.version {
            return Err(diverged(format!(
                "version {} instead of {}",
                state.version, entry.version
            )));
        }
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn utc_format() {
        let at = |ms| SystemTime::UNIX_EPOCH + Duration::from_millis(ms);
        assert_eq!(utc_rfc3339(at(0)), "1970-01-01T00:00:00.000Z");
        assert_eq!(utc_rfc3339(at(951_782_400_000)), "2000-02-29T00:00:00.000Z");
        assert_eq!(
            utc_rfc3339(at(1_793_028_660_481)),
            "2026-10-26T15:31:00.481Z"
        );
    }

    #[test]
    fn line_shape_matches_the_spec() {
        let line = JournalLine {
            utc: "2026-10-27T14:03:12.481Z".into(),
            frame: 5321,
            source: Origin::Ui,
            id: Some(CommandId(17)),
            command: Some(Command::Transition { button: 1 }),
            event: None,
            result: Some(Outcome::Ok),
            version: 42,
        };
        assert_eq!(
            serde_json::to_string(&line).unwrap(),
            r#"{"utc":"2026-10-27T14:03:12.481Z","frame":5321,"source":"ui","id":17,"command":{"Transition":{"button":1}},"result":"Ok","version":42}"#
        );
    }

    #[test]
    fn writer_appends_lines_to_the_day_file_and_counts_drops() {
        let dir = std::env::temp_dir().join(format!("vma-journal-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let stats = Arc::new(JournalStats::default());
        let (sender, thread) = spawn_writer(dir.clone(), Arc::clone(&stats)).unwrap();
        let mut line = JournalLine {
            utc: "2026-10-27T23:59:59.999Z".into(),
            frame: 1,
            source: Origin::Script,
            id: Some(CommandId(1)),
            command: Some(Command::Cut),
            event: None,
            result: Some(Outcome::Rejected("no input in Preview".into())),
            version: 0,
        };
        sender.send(&line);
        line.utc = "2026-10-28T00:00:00.000Z".into();
        sender.send(&line);
        drop(sender);
        thread.join().unwrap();
        let day1 = std::fs::read_to_string(dir.join("commands-2026-10-27.jsonl")).unwrap();
        let day2 = std::fs::read_to_string(dir.join("commands-2026-10-28.jsonl")).unwrap();
        assert_eq!((day1.lines().count(), day2.lines().count()), (1, 1));
        let back: JournalLine = serde_json::from_str(day2.trim()).unwrap();
        assert_eq!(back, line);
        assert_eq!((stats.written(), stats.dropped()), (2, 0));
        std::fs::remove_dir_all(&dir).unwrap();

        // No writer: a full channel drops lines, never blocks.
        let stats = Arc::new(JournalStats::default());
        let (tx, _rx) = crossbeam_channel::bounded(1);
        let sender = JournalSender::new(tx, Arc::clone(&stats));
        for _ in 0..5 {
            sender.send(&line);
        }
        assert_eq!(stats.dropped(), 4);
    }
}
