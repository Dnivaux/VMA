//! `--ptz-load` (Phase 7A, G11): during a soak, a simulated PTZ camera (VISCA over IP on
//! 127.0.0.1, ADR 0018) on the first camera or synthetic input, with two virtual inputs. Every
//! 10 s: the camera off air, a virtual input put in Preview (its preset must reach the camera,
//! once), taken on air, then the other one put in Preview (its camera on air: nothing must be
//! sent, the recall is held), and a short pan on air. Then what the camera received.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use vma_core::InputId;
use vma_engine::{
    Command, CommandSender, Direction, InputSpec, InputStatus, Preset, PtzAction, PtzLink, Source,
    Speed, StateReader, ViscaCommand, may_be_ptz,
};
use vma_ptz::sim::SimCamera;

/// One cycle every this long.
const CYCLE: Duration = Duration::from_secs(10);
/// Steps of a cycle, from its start.
const ON_AIR_AT: Duration = Duration::from_secs(3);
const HELD_AT: Duration = Duration::from_secs(5);
const PAN_AT: Duration = Duration::from_secs(7);
const PAN_FOR: Duration = Duration::from_millis(500);
/// A recall must reach the camera within this long.
const RECALL_WITHIN: Duration = Duration::from_secs(1);

#[derive(Debug, Default)]
pub struct PtzLoadReport {
    pub cycles: u64,
    /// Virtual inputs put in Preview with their camera off air, and the recalls the camera
    /// received for them within 1 s (exactly one each).
    pub recalls_expected: u64,
    pub recalls_received: u64,
    /// … and with their camera on air: held (nothing sent).
    pub held_expected: u64,
    pub held_seen: u64,
    /// Recalls received while none was expected.
    pub unexpected: u64,
    pub pans: u64,
    /// Times the camera was reported unreachable.
    pub unreachable: u64,
    /// Commands the camera received in all, and its sequence errors.
    pub received: usize,
    pub sequence_errors: u32,
    pub refused: Vec<String>,
    pub refused_count: u64,
    pub failure: Option<String>,
    pub elapsed: Duration,
}

impl PtzLoadReport {
    pub fn passed(&self) -> bool {
        self.failure.is_none()
            && self.cycles > 0
            && self.recalls_received == self.recalls_expected
            && self.held_seen == self.held_expected
            && self.unexpected == 0
            && self.unreachable == 0
            && self.sequence_errors == 0
            && self.refused_count == 0
    }
}

pub struct PtzLoad {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<PtzLoadReport>,
}

impl PtzLoad {
    pub fn start(commands: CommandSender, state: StateReader) -> anyhow::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("vma-ptz-load".into())
            .spawn(move || {
                let mut report = PtzLoadReport::default();
                let started = Instant::now();
                match SimCamera::udp() {
                    Ok(camera) => {
                        let mut load = Load {
                            commands: &commands,
                            state: &state,
                            camera: &camera,
                            report: &mut report,
                        };
                        if let Err(e) = load.run(&thread_stop) {
                            load.report.failure = Some(format!("{e:#}"));
                        }
                        report.received = camera.received().len();
                        report.sequence_errors = camera.sequence_errors();
                    }
                    Err(e) => report.failure = Some(format!("caméra simulée : {e}")),
                }
                report.elapsed = started.elapsed();
                report
            })
            .context("PTZ load thread")?;
        Ok(Self { stop, thread })
    }

    pub fn finish(self) -> PtzLoadReport {
        self.stop.store(true, Ordering::Release);
        self.thread.join().unwrap_or_else(|_| PtzLoadReport {
            failure: Some("the PTZ load thread panicked".into()),
            ..PtzLoadReport::default()
        })
    }
}

struct Load<'a> {
    commands: &'a CommandSender,
    state: &'a StateReader,
    camera: &'a SimCamera,
    report: &'a mut PtzLoadReport,
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

    fn recalls(&self) -> usize {
        self.camera
            .commands()
            .iter()
            .filter(|c| matches!(c, ViscaCommand::PresetRecall(_)))
            .count()
    }

    fn link(&self, camera: InputId) -> Option<PtzLink> {
        let state = self.state.load();
        state.input(camera)?.ptz.as_ref().map(|p| p.link.clone())
    }

    fn run(&mut self, stop: &AtomicBool) -> anyhow::Result<()> {
        let started = Instant::now();
        // The camera: the first input that may be one, once live; another input to cut to.
        let (camera, other) = loop {
            let state = self.state.load();
            let camera = state
                .inputs
                .iter()
                .find(|i| may_be_ptz(&i.spec) && matches!(i.status, InputStatus::Live { .. }));
            let other = state
                .inputs
                .iter()
                .find(|i| Some(i.id) != camera.map(|c| c.id) && i.spec.kind().has_video());
            if let (Some(camera), Some(other)) = (camera, other) {
                break (camera.id, other.id);
            }
            anyhow::ensure!(
                started.elapsed() < Duration::from_secs(30),
                "no live camera or synthetic input, and another input, after 30 s"
            );
            if stop.load(Ordering::Acquire) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        self.apply(Command::SetPtzCamera {
            input: camera,
            address: Some(self.camera.address()),
        });
        let reachable_by = Instant::now() + Duration::from_secs(5);
        while self.link(camera) != Some(PtzLink::Reachable) {
            anyhow::ensure!(
                Instant::now() < reachable_by,
                "simulated camera not reachable after 5 s"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let key = self
            .state
            .load()
            .input(camera)
            .map(|i| i.key)
            .context("camera gone")?;
        let mut views = Vec::new();
        for n in [1, 2] {
            let preset = Preset::new(n).context("preset")?;
            let before = self.state.load().inputs.len();
            self.apply(Command::AddInput {
                spec: InputSpec::PtzView {
                    camera: key,
                    preset,
                },
                name: Some(format!("PTZ {n}")),
            });
            let state = self.state.load();
            anyhow::ensure!(state.inputs.len() > before, "virtual input {n} not added");
            let view = state.inputs.last().map(|i| i.id).context("virtual input")?;
            views.push((view, preset));
        }
        // Watches the link for the whole run.
        let mut was_reachable = true;
        let mut k = 0u64;
        while !stop.load(Ordering::Acquire) {
            let at = Instant::now();
            let (view, _) = views.get((k % 2) as usize).copied().context("view")?;
            let (other_view, _) = views.get(((k + 1) % 2) as usize).copied().context("view")?;
            if !self.cycle(at, camera, other, view, other_view, stop) {
                break;
            }
            k += 1;
            let reachable = self.link(camera) == Some(PtzLink::Reachable);
            if was_reachable && !reachable {
                self.report.unreachable += 1;
            }
            was_reachable = reachable;
            if !sleep_until(at + CYCLE, stop) {
                break;
            }
        }
        Ok(())
    }

    /// One cycle; `false` when the soak ends during it.
    fn cycle(
        &mut self,
        at: Instant,
        camera: InputId,
        other: InputId,
        view: InputId,
        other_view: InputId,
        stop: &AtomicBool,
    ) -> bool {
        // Off air, then the view in Preview: one recall.
        self.apply(Command::CutDirect { input: other });
        let before = self.recalls();
        self.apply(Command::SetPreview { input: view });
        self.report.recalls_expected += 1;
        if !sleep_until(Instant::now() + RECALL_WITHIN, stop) {
            return false;
        }
        match self.recalls() - before {
            0 => {}
            1 => self.report.recalls_received += 1,
            more => {
                self.report.recalls_received += 1;
                self.report.unexpected += (more - 1) as u64;
            }
        }
        // On air through the view.
        if !sleep_until(at + ON_AIR_AT, stop) {
            return false;
        }
        self.apply(Command::Cut);
        // The other view in Preview, its camera on air: held, nothing sent.
        if !sleep_until(at + HELD_AT, stop) {
            return false;
        }
        let before = self.recalls();
        self.apply(Command::SetPreview { input: other_view });
        self.report.held_expected += 1;
        if !sleep_until(Instant::now() + RECALL_WITHIN, stop) {
            return false;
        }
        if self.state.load().ptz_held == Some(other_view) {
            self.report.held_seen += 1;
        }
        self.report.unexpected += (self.recalls() - before) as u64;
        // A short pan on air (moves are the operator's: no confirmation).
        if !sleep_until(at + PAN_AT, stop) {
            return false;
        }
        let speed = Speed::DEFAULT;
        self.apply(Command::Ptz {
            input: camera,
            action: PtzAction::Move {
                direction: Direction::Left,
                speed,
            },
        });
        std::thread::sleep(PAN_FOR);
        self.apply(Command::Ptz {
            input: camera,
            action: PtzAction::Move {
                direction: Direction::Stop,
                speed,
            },
        });
        self.report.pans += 1;
        self.report.cycles += 1;
        true
    }
}

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

pub fn write_report(s: &mut String, r: &PtzLoadReport) {
    use std::fmt::Write as _;
    // `write!` into a String cannot fail.
    let _ = writeln!(
        s,
        "## Caméra PTZ (7A, G11)\nUne caméra simulée (VISCA over IP, 127.0.0.1) et deux inputs virtuels ; toutes les {} s : la caméra hors antenne, un input virtuel en Preview (son préréglage doit arriver, une fois, en moins de {} s), à l'antenne {} s après, l'autre en Preview {} s après (caméra à l'antenne : rien ne doit partir), un pan de {} ms à l'antenne.\n",
        CYCLE.as_secs(),
        RECALL_WITHIN.as_secs(),
        ON_AIR_AT.as_secs(),
        HELD_AT.as_secs(),
        PAN_FOR.as_millis()
    );
    if let Some(failure) = &r.failure {
        let _ = writeln!(s, "**Arrêté : {failure}**\n");
    }
    let _ = writeln!(s, "| Mesure | Valeur |\n|---|---|");
    let _ = writeln!(s, "| Cycles | {} |", r.cycles);
    let _ = writeln!(
        s,
        "| **Rappels reçus** (caméra hors antenne) | **{} sur {}** |",
        r.recalls_received, r.recalls_expected
    );
    let _ = writeln!(
        s,
        "| **Rappels retenus** (caméra à l'antenne) | **{} sur {}** |",
        r.held_seen, r.held_expected
    );
    let _ = writeln!(s, "| Rappels en trop | {} |", r.unexpected);
    let _ = writeln!(s, "| Pans à l'antenne | {} |", r.pans);
    let _ = writeln!(s, "| Caméra injoignable | {} fois |", r.unreachable);
    let _ = writeln!(
        s,
        "| Messages reçus par la caméra (sondes comprises) | {} ({} erreurs de séquence) |",
        r.received, r.sequence_errors
    );
    let _ = writeln!(s, "| Commandes refusées | {} |", r.refused_count);
    for refusal in &r.refused {
        let _ = writeln!(s, "| ↳ | {refusal} |");
    }
    let _ = writeln!(
        s,
        "| Durée | {:.0} s |\n| **Résultat** | **{}** |\n",
        r.elapsed.as_secs_f64(),
        if r.passed() { "réussi" } else { "échoué" }
    );
}
