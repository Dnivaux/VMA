//! `--replay-load <dossier>` (Phase 6, F7): during a soak, the replay at work. The recordable
//! inputs are recorded for the replay and as ISO files in that folder; every 30 s, « −5 s »
//! then « Rejouer » (the BRIEF's two actions) at 50, 25 or 75 %, another angle 2 s later, and
//! in one cycle out of three 1.5 s played backwards; each cycle checks that the Program is back
//! on the camera it left. Then what the recorders, the player, the disk and the processor did.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use vma_core::InputId;
use vma_engine::{
    Command, CommandSender, InputSpec, InputStatus, MAX_REPLAY_CAMERAS, Mark, ReplayChannel,
    Source, StateReader,
};

/// One replay every this long.
const CYCLE: Duration = Duration::from_secs(30);
/// Before the first one: inputs live, and more than the 5 s marked recorded.
const WARM_UP: Duration = Duration::from_secs(10);
/// The event marked: the last seconds.
const MARKED_S: u32 = 5;
/// « Rejouer »'s speeds, in turn.
const SPEEDS: [u8; 3] = [50, 25, 75];
/// Another angle this long after « Rejouer ».
const ANGLE_AT: Duration = Duration::from_secs(2);
/// Backwards from 3 s to 4.5 s, one cycle in three.
const REVERSE_AT: Duration = Duration::from_secs(3);
const FORWARD_AT: Duration = Duration::from_millis(4_500);
/// The Program must be back on its camera by then.
const BACK_BY: Duration = Duration::from_secs(28);
/// Recording minutes kept: old segments are deleted during the soak (F2).
const KEEP_MINUTES: u32 = 5;
/// The product's bit rate (all-intra 1080p50).
const KBPS: u32 = 100_000;
/// Linux counts process times in these (`USER_HZ`, fixed by its ABI).
const USER_HZ: f64 = 100.0;

/// What the replay load did.
#[derive(Debug, Default)]
pub struct ReplayLoadReport {
    pub cameras: usize,
    /// Cycles started (« −5 s » + « Rejouer »).
    pub cycles: u64,
    /// Cycles checked (not cut short by the end of the soak), and those whose Program came
    /// back on the camera it left.
    pub checked: u64,
    pub returned: u64,
    pub angles: u64,
    pub reverses: u64,
    /// Commands refused or not answered (the first ones).
    pub refused: Vec<String>,
    pub refused_count: u64,
    /// Why it could not start, if it did not.
    pub failure: Option<String>,
    /// Per camera: its name and what its recorder did.
    pub recorders: Vec<(String, Option<vma_io_gst::RecordActivity>)>,
    pub player: Option<vma_io_gst::ReplayActivity>,
    /// The replay's folder at the end, and the ISO files.
    pub replay_bytes: u64,
    pub iso_bytes: u64,
    pub iso_files: u64,
    pub elapsed: Duration,
    /// Processor time of the whole process over the run (Linux).
    pub cpu: Option<Duration>,
}

impl ReplayLoadReport {
    /// Every cycle came back, nothing refused, no picture lost by a recorder or the player.
    pub fn passed(&self) -> bool {
        let recorders_ok = !self.recorders.is_empty()
            && self.recorders.iter().all(|(_, r)| {
                r.as_ref()
                    .is_some_and(|r| r.dropped == 0 && r.failure.is_none())
            });
        let player_ok = self.player.is_some_and(|p| p.missed == 0 && p.failed == 0);
        self.failure.is_none()
            && self.checked > 0
            && self.returned == self.checked
            && self.refused_count == 0
            && recorders_ok
            && player_ok
    }
}

pub struct ReplayLoad {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<ReplayLoadReport>,
}

impl ReplayLoad {
    pub fn start(
        commands: CommandSender,
        state: StateReader,
        io: Option<vma_io_gst::IoProbe>,
        folder: PathBuf,
    ) -> anyhow::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("vma-replay-load".into())
            .spawn(move || {
                let mut report = ReplayLoadReport::default();
                let started = Instant::now();
                let cpu_start = process_cpu();
                let mut load = Load {
                    commands: &commands,
                    state: &state,
                    report: &mut report,
                };
                if let Err(e) = load.run(&folder, &thread_stop) {
                    load.report.failure = Some(format!("{e:#}"));
                }
                measure(&mut report, &state, io.as_ref(), &folder);
                report.elapsed = started.elapsed();
                report.cpu = cpu_start
                    .zip(process_cpu())
                    .map(|(a, b)| b.saturating_sub(a));
                report
            })
            .context("replay load thread")?;
        Ok(Self { stop, thread })
    }

    pub fn finish(self) -> ReplayLoadReport {
        self.stop.store(true, Ordering::Release);
        self.thread.join().unwrap_or_else(|_| ReplayLoadReport {
            failure: Some("the replay load thread panicked".into()),
            ..ReplayLoadReport::default()
        })
    }
}

struct Load<'a> {
    commands: &'a CommandSender,
    state: &'a StateReader,
    report: &'a mut ReplayLoadReport,
}

impl Load<'_> {
    /// Sends `command` and waits for its outcome; a refusal is counted.
    fn apply(&mut self, command: Command) -> bool {
        let what = format!("{command:?}");
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let sent = self
            .commands
            .send_then(Source::Script, command, move |_, outcome| {
                let _ = tx.send(match outcome {
                    vma_engine::Outcome::Ok => None,
                    vma_engine::Outcome::Rejected(reason) => Some(reason.clone()),
                });
            });
        let refusal = match sent {
            Err(e) => Some(e.to_string()),
            Ok(_) => match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(refusal) => refusal,
                Err(_) => Some("no answer in 2 s".into()),
            },
        };
        match refusal {
            None => true,
            Some(reason) => {
                self.report.refused_count += 1;
                if self.report.refused.len() < 10 {
                    self.report.refused.push(format!("{what} : {reason}"));
                }
                false
            }
        }
    }

    fn run(&mut self, folder: &Path, stop: &AtomicBool) -> anyhow::Result<()> {
        let started = Instant::now();
        // The cameras: the recordable inputs, once live (a recorder needs a pipeline).
        let cameras = loop {
            let state = self.state.load();
            let recordable: Vec<_> = state
                .inputs
                .iter()
                .filter(|i| i.spec.is_recordable())
                .take(MAX_REPLAY_CAMERAS)
                .collect();
            if !recordable.is_empty()
                && recordable
                    .iter()
                    .all(|i| matches!(i.status, InputStatus::Live { .. }))
            {
                break recordable.iter().map(|i| i.id).collect::<Vec<InputId>>();
            }
            anyhow::ensure!(
                started.elapsed() < Duration::from_secs(30),
                "no recordable input live after 30 s"
            );
            if stop.load(Ordering::Acquire) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        self.report.cameras = cameras.len();
        std::fs::create_dir_all(folder)
            .with_context(|| format!("cannot create {}", folder.display()))?;
        for command in [
            Command::SetReplaySession {
                folder: Some(folder.join("replay")),
                keep_minutes: KEEP_MINUTES,
                kbps: KBPS,
            },
            Command::SetReplayCameras {
                inputs: cameras.clone(),
            },
            Command::SetIso {
                folder: Some(folder.join("iso")),
                inputs: cameras.clone(),
                split_minutes: None,
            },
            Command::AddInput {
                spec: InputSpec::Replay {
                    channel: ReplayChannel::A,
                },
                name: None,
            },
            Command::StartReplayRecording,
            Command::StartIso,
            // A camera on air, for « Rejouer » to come back to.
            Command::CutDirect {
                input: cameras.first().copied().context("no camera")?,
            },
        ] {
            anyhow::ensure!(self.apply(command.clone()), "set-up refused: {command:?}");
        }

        let first = Instant::now() + WARM_UP;
        let mut cycle = 0u64;
        while !stop.load(Ordering::Acquire) {
            let at = first + CYCLE * u32::try_from(cycle).unwrap_or(u32::MAX);
            if !sleep_until(at, stop) {
                break;
            }
            self.cycle(cycle, at, &cameras, stop);
            cycle += 1;
        }
        Ok(())
    }

    fn cycle(&mut self, k: u64, at: Instant, cameras: &[InputId], stop: &AtomicBool) {
        let left = self.state.load().program;
        let nth = |n: u64, len: usize| usize::try_from(n).unwrap_or(0) % len.max(1);
        let speed = SPEEDS.get(nth(k, SPEEDS.len())).copied().unwrap_or(50);
        // Each cycle ends back on air: one more refusal here is reported, not fatal.
        let _ = self.apply(Command::SetReplayAuto { speed, button: 1 });
        let marked = self.apply(Command::ReplayMark {
            mark: Mark::Last { seconds: MARKED_S },
            at_ns: None,
        });
        if !(marked && self.apply(Command::ReplayAuto { id: None })) {
            return;
        }
        self.report.cycles += 1;
        if !sleep_until(at + ANGLE_AT, stop) {
            return;
        }
        let Some(&angle) = cameras.get(nth(k + 1, cameras.len())) else {
            return;
        };
        if self.apply(Command::ReplayAngle { input: angle }) {
            self.report.angles += 1;
        }
        if k % 3 == 2 {
            if !sleep_until(at + REVERSE_AT, stop) {
                return;
            }
            if self.apply(Command::ReplayReverse { on: true }) {
                self.report.reverses += 1;
            }
            if !sleep_until(at + FORWARD_AT, stop) {
                return;
            }
            let _ = self.apply(Command::ReplayReverse { on: false });
        }
        if !sleep_until(at + BACK_BY, stop) {
            return;
        }
        self.report.checked += 1;
        let state = self.state.load();
        let on_a_camera = left.is_some_and(|l| cameras.contains(&l));
        if state.replay.auto.is_none() && on_a_camera && state.program == left {
            self.report.returned += 1;
        } else if self.report.refused.len() < 10 {
            self.report
                .refused
                .push(format!("cycle {k} : pas revenu à l'antenne d'avant"));
        }
    }
}

/// The report's « Replay (F7) » section.
pub fn write_report(s: &mut String, r: &ReplayLoadReport) {
    use std::fmt::Write as _;
    let mb = |bytes: u64| bytes as f64 / 1e6;
    let secs = r.elapsed.as_secs_f64().max(1e-9);
    // `write!` into a String cannot fail.
    let _ = writeln!(
        s,
        "## Replay (F7)\n{} caméras enregistrées pour le replay ({KEEP_MINUTES} min gardées, {} Mb/s) et en ISO ; toutes les {} s : « −{MARKED_S} s » puis « Rejouer » (vitesses {} % tour à tour, transition du bouton 1), un autre angle {} s après, l'arrière pendant 1,5 s un cycle sur trois.\n",
        r.cameras,
        KBPS / 1000,
        CYCLE.as_secs(),
        SPEEDS.map(|v| v.to_string()).join(", "),
        ANGLE_AT.as_secs()
    );
    if let Some(failure) = &r.failure {
        let _ = writeln!(s, "**Arrêté : {failure}**\n");
    }
    let _ = writeln!(s, "| Mesure | Valeur |\n|---|---|");
    let _ = writeln!(
        s,
        "| Cycles (« −{MARKED_S} s » + « Rejouer ») | {} |",
        r.cycles
    );
    let _ = writeln!(
        s,
        "| **Revenus à la caméra d'avant**, sur les cycles vérifiés (pas coupés par la fin) | **{} sur {}** |",
        r.returned, r.checked
    );
    let _ = writeln!(
        s,
        "| Changements d'angle · passages en arrière | {} · {} |",
        r.angles, r.reverses
    );
    let _ = writeln!(s, "| **Commandes refusées** | **{}** |", r.refused_count);
    match r.player {
        Some(p) => {
            let _ = writeln!(
                s,
                "| Lecteur : images montrées · décodées | {} · {} |\n| **Lecteur : en retard · ticks manqués · illisibles** | **{} · {} · {}** |",
                p.shown, p.decoded, p.late, p.missed, p.failed
            );
        }
        None => {
            let _ = writeln!(s, "| Lecteur | pas de compteurs |");
        }
    }
    let _ = writeln!(
        s,
        "| Dossier du replay à la fin | {:.0} Mo (les segments de plus de {KEEP_MINUTES} min sont effacés) |",
        mb(r.replay_bytes)
    );
    let _ = writeln!(
        s,
        "| ISO | {:.0} Mo en {} fichiers, {:.1} Mo/s |",
        mb(r.iso_bytes),
        r.iso_files,
        mb(r.iso_bytes) / secs
    );
    let _ = writeln!(
        s,
        "| | Le débit suit l'image : des sources de test simples se compressent bien plus qu'une caméra (au plus {} Mb/s par caméra ici) |",
        KBPS / 1000
    );
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    match r.cpu {
        Some(cpu) => {
            let _ = writeln!(
                s,
                "| Processeur (tout le processus) | {:.0} % d'un cœur en moyenne ({cores} cœurs) |",
                100.0 * cpu.as_secs_f64() / secs
            );
        }
        None => {
            let _ = writeln!(s, "| Processeur | non mesuré sur cet OS |");
        }
    }
    let _ = writeln!(
        s,
        "\n| Caméra | Encodeur | Offertes | Écrites | **Perdues** | Son perdu | Arrêt |\n|---|---|---|---|---|---|---|"
    );
    for (name, activity) in &r.recorders {
        match activity {
            Some(a) => {
                let _ = writeln!(
                    s,
                    "| {name} | {} | {} | {} | **{}** | {} | {} |",
                    a.encoder.unwrap_or("?"),
                    a.offered,
                    a.written,
                    a.dropped,
                    a.sound_dropped,
                    a.failure.as_deref().unwrap_or("non")
                );
            }
            None => {
                let _ = writeln!(s, "| {name} | — | pas d'enregistreur | | | | |");
            }
        }
    }
    if !r.refused.is_empty() {
        let _ = writeln!(s, "\nRefus et cycles sans retour (les premiers) :");
        for line in &r.refused {
            let _ = writeln!(s, "- {line}");
        }
    }
    s.push('\n');
}

/// Sleeps until `at`; `false` if asked to stop meanwhile.
fn sleep_until(at: Instant, stop: &AtomicBool) -> bool {
    loop {
        if stop.load(Ordering::Acquire) {
            return false;
        }
        let left = at.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        std::thread::sleep(left.min(Duration::from_millis(100)));
    }
}

/// The recorders, the player and the folders at the end.
fn measure(
    report: &mut ReplayLoadReport,
    state: &StateReader,
    io: Option<&vma_io_gst::IoProbe>,
    folder: &Path,
) {
    let state = state.load();
    report.recorders = state
        .replay
        .cameras
        .iter()
        .map(|camera| {
            let name = state
                .input(*camera)
                .map_or_else(|| camera.to_string(), |i| i.name.clone());
            (name, io.and_then(|io| io.recording(*camera)))
        })
        .collect();
    report.player = state
        .replay_input(ReplayChannel::A)
        .and_then(|input| io.and_then(|io| io.replay(input)));
    report.replay_bytes = folder_size(&folder.join("replay")).0;
    (report.iso_bytes, report.iso_files) = folder_size(&folder.join("iso"));
}

/// Bytes and files under `dir`.
fn folder_size(dir: &Path) -> (u64, u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    entries
        .flatten()
        .fold((0, 0), |(bytes, files), entry| match entry.metadata() {
            Ok(m) if m.is_dir() => {
                let (b, f) = folder_size(&entry.path());
                (bytes + b, files + f)
            }
            Ok(m) => (bytes + m.len(), files + 1),
            Err(_) => (bytes, files),
        })
}

/// Processor time used by this process so far: user + system (Linux; `None` elsewhere).
fn process_cpu() -> Option<Duration> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // After the command name in parentheses: state is field 3, utime 14, stime 15.
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    let ticks: f64 = fields.get(11)?.parse::<f64>().ok()? + fields.get(12)?.parse::<f64>().ok()?;
    Some(Duration::from_secs_f64(ticks / USER_HZ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_process_time_is_read_on_linux() {
        if cfg!(target_os = "linux") {
            assert!(process_cpu().is_some());
        }
    }

    #[test]
    fn folders_are_measured() {
        let dir = std::env::temp_dir().join(format!("vma-replay-load-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("a")).unwrap_or_default();
        std::fs::write(dir.join("x"), [0u8; 10]).unwrap_or_default();
        std::fs::write(dir.join("a/y"), [0u8; 5]).unwrap_or_default();
        assert_eq!(folder_size(&dir), (15, 2));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(folder_size(&dir), (0, 0));
    }
}
