//! Autosave (ADR 0013 § 8): the show is written [`AUTOSAVE_DELAY`] after its last change and
//! when VMA closes, on the `vma-project` thread (never on `vma-control` nor the render
//! thread). [`AUTOSAVE_KEEP`] files turn in the folder: `autosave-1.vma` is the newest; a new
//! one is started when it is older than [`ROTATE_AFTER`], so the older files keep the state
//! of the show some minutes back.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use vma_engine::{ProjectSnapshot, StateReader};

use crate::{Secrets, to_json, write_atomic};

/// Quiet time after a change before it is written.
pub const AUTOSAVE_DELAY: Duration = Duration::from_secs(2);
/// Files kept.
pub const AUTOSAVE_KEEP: usize = 5;
/// Age of the newest file past which the next save starts a new one.
const ROTATE_AFTER: Duration = Duration::from_secs(60);
/// How often the state is looked at.
const LOOK_EVERY: Duration = Duration::from_millis(250);

/// The autosave thread; [`Autosaver::finish`] (or dropping it) writes what is pending.
#[derive(Debug)]
pub struct Autosaver {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

/// File `n` (1 = newest) of `folder`.
fn file(folder: &Path, n: usize) -> PathBuf {
    folder.join(format!("autosave-{n}.{}", crate::EXTENSION))
}

/// The newest autosave, if any: what « Dernier » opens.
pub fn latest_autosave(folder: &Path) -> Option<PathBuf> {
    autosaves(folder).into_iter().next()
}

/// The autosaves of `folder`, newest first.
pub fn autosaves(folder: &Path) -> Vec<PathBuf> {
    (1..=AUTOSAVE_KEEP)
        .map(|n| file(folder, n))
        .filter(|p| p.exists())
        .collect()
}

impl Autosaver {
    /// Watches `state` and saves the show into `folder`. The show as it is now is not
    /// written: the files of the previous session stay until something changes.
    pub fn spawn(
        state: StateReader,
        folder: PathBuf,
        secrets: Arc<Mutex<Secrets>>,
    ) -> std::io::Result<Self> {
        let (stop, stopped) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("vma-project".into())
            .spawn(move || run(&state, &folder, &secrets, &stopped))?;
        Ok(Self {
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    /// Writes what is pending and stops.
    pub fn finish(mut self) {
        self.shut();
    }

    fn shut(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("the autosave thread panicked");
        }
    }
}

impl Drop for Autosaver {
    fn drop(&mut self) {
        self.shut();
    }
}

fn run(state: &StateReader, folder: &Path, secrets: &Mutex<Secrets>, stopped: &mpsc::Receiver<()>) {
    let mut version = state.load().version;
    let mut saved = state.load().project();
    let mut pending: Option<(ProjectSnapshot, Instant)> = None;
    loop {
        let stopping = match stopped.recv_timeout(LOOK_EVERY) {
            Err(RecvTimeoutError::Timeout) => false,
            Ok(()) | Err(RecvTimeoutError::Disconnected) => true,
        };
        let now = state.load();
        if now.version != version {
            version = now.version;
            let project = now.project();
            if project != saved {
                pending = Some((project, Instant::now()));
            } else {
                pending = None;
            }
        }
        let due = pending
            .as_ref()
            .is_some_and(|(_, since)| stopping || since.elapsed() >= AUTOSAVE_DELAY);
        if due && let Some((project, _)) = pending.take() {
            match save(folder, &project, secrets) {
                Ok(()) => saved = project,
                Err(e) => {
                    tracing::warn!(folder = %folder.display(), error = %e, "autosave failed");
                    // Tried again after the next change.
                    saved = project;
                }
            }
        }
        if stopping {
            return;
        }
    }
}

fn save(folder: &Path, project: &ProjectSnapshot, secrets: &Mutex<Secrets>) -> std::io::Result<()> {
    let mut secrets = secrets.lock().unwrap_or_else(PoisonError::into_inner);
    let text = to_json(project, None, &mut secrets).map_err(std::io::Error::other)?;
    let newest = file(folder, 1);
    let old = std::fs::metadata(&newest)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age >= ROTATE_AFTER);
    if old {
        for n in (1..AUTOSAVE_KEEP).rev() {
            let from = file(folder, n);
            if from.exists() {
                std::fs::rename(&from, file(folder, n + 1))?;
            }
        }
    }
    write_atomic(&newest, text.as_bytes(), false)?;
    secrets.save();
    Ok(())
}
