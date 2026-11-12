//! Replay and ISO in the engine's state (Phase 6, ADR 0015, ADR 0016): the session (folder,
//! time kept, bit rate), the cameras recorded for the replay, the inputs recorded as ISO, the
//! events. From them, what each input's recorder must do ([`State::record_plans`]): `vma-io`
//! is told when it changes.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use vma_core::InputId;

use crate::command::InputSpec;
use crate::state::{Change, Ending, IoRequest, Rejection, State};

/// Cameras recorded for the replay at most (the reference's 8; 4 measured, `phase-6.md` Q3).
pub const MAX_REPLAY_CAMERAS: usize = 8;
/// Longest « −n s » event.
pub const MAX_MARK_SECONDS: u32 = 120;

/// A replay channel (A now, B later: `phase-6.md` Q5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReplayChannel {
    A,
}

impl std::fmt::Display for ReplayChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::A => "A",
        })
    }
}

/// What a replay channel plays (ADR 0016 § 2): set by the commands; the position itself
/// lives in the player (`vma-io`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayerState {
    pub event: Option<u32>,
    pub speed: u8,
    pub reverse: bool,
    pub playing: bool,
    /// +1 whenever the player must start the event again.
    pub cue: u64,
    /// The camera chosen while playing (`None`: the event's angle).
    pub angle: Option<InputId>,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            event: None,
            speed: vma_replay::MAX_SPEED,
            reverse: false,
            playing: false,
            cue: 0,
            angle: None,
        }
    }
}

/// A « Rejouer » under way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoReplay {
    pub cue: u64,
    /// On air when it started (`None`: nothing was).
    pub back_to: Option<InputId>,
}

/// What the player of a replay input plays.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerPlan {
    /// The segments of the angle played (`None`: nothing to play).
    pub angle: Option<PathBuf>,
    /// The event, on the shared clock.
    pub in_ns: u64,
    pub out_ns: u64,
    pub speed: u8,
    pub reverse: bool,
    pub playing: bool,
    /// A new value: start the event again (its end in reverse).
    pub cue: u64,
}

/// What an input's recorder does (`vma-io` turns it into its encoding pipeline).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordPlan {
    /// The replay's segments: the input's folder in the session.
    pub segments: Option<PathBuf>,
    /// Segments older than this are deleted.
    pub keep_minutes: u32,
    pub kbps: u32,
    pub iso: Option<IsoPlan>,
}

/// An input's ISO file: `vma-io` names it `<name> <date and time> 000.mkv` (`001`… for the
/// next parts), so that a recording started again never overwrites one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsoPlan {
    pub folder: PathBuf,
    /// From the input's name, without what a file name cannot hold.
    pub name: String,
    /// A new file every this many minutes (`None`: one file).
    pub split_minutes: Option<u32>,
}

/// The ISO recordings (the MultiCorder of the reference, `phase-6.md` Q7).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct IsoState {
    pub folder: Option<PathBuf>,
    pub inputs: Vec<InputId>,
    /// A new file every this many minutes (`None`: one file).
    pub split_minutes: Option<u32>,
    pub recording: bool,
}

/// A moment of the recording to play again (ADR 0016 § 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayEvent {
    pub id: u32,
    /// Instants on the shared clock (ns).
    pub in_ns: u64,
    pub out_ns: u64,
    /// The angle it plays from (`None`: the first camera).
    #[serde(default)]
    pub angle: Option<InputId>,
    /// Its speed in percent (`None`: the player's).
    #[serde(default)]
    pub speed: Option<u8>,
    #[serde(default)]
    pub comment: String,
}

/// How an event is marked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mark {
    /// The last `seconds`, ending now (« −5 s », « −10 s »).
    Last { seconds: u32 },
    /// Opens an event now.
    In,
    /// Closes the open event now.
    Out,
    /// Drops the open event.
    Cancel,
}

/// A recorder stopped on an error (a full disk): the show goes on, the UI shows it until
/// the recording is started again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordFailure {
    pub input: InputId,
    pub reason: String,
}

/// The replay in the state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReplayState {
    /// The session folder (dedicated SSD advised).
    pub folder: Option<PathBuf>,
    pub keep_minutes: u32,
    pub kbps: u32,
    pub cameras: Vec<InputId>,
    pub recording: bool,
    pub iso: IsoState,
    pub events: Vec<ReplayEvent>,
    /// An event opened by « Entrée », not closed yet: its in point.
    pub marking: Option<u64>,
    /// Recorders stopped on an error since the recordings were started.
    pub failures: Vec<RecordFailure>,
    /// The replay channel A.
    pub player: PlayerState,
    /// « Rejouer »: its speed (percent) and transition button (Q14: 50 %, button 1).
    pub auto_speed: u8,
    pub auto_button: u8,
    /// « Rejouer » under way: back to that input when the event of that cue ends.
    pub auto: Option<AutoReplay>,
    next_event: u32,
}

impl Default for ReplayState {
    fn default() -> Self {
        Self {
            folder: None,
            // ADR 0015 § 1.
            keep_minutes: 120,
            // All-intra 1080p50 (ADR 0015 § 3).
            kbps: 100_000,
            cameras: Vec::new(),
            recording: false,
            iso: IsoState::default(),
            events: Vec::new(),
            marking: None,
            failures: Vec::new(),
            player: PlayerState::default(),
            auto_speed: 50,
            auto_button: 1,
            auto: None,
            next_event: 1,
        }
    }
}

/// A file name from an input's name: no path separator nor control character.
fn file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    if trimmed.is_empty() {
        "input".into()
    } else {
        trimmed.into()
    }
}

impl State {
    /// What every input's recorder must do now (`None`: not recorded).
    pub fn record_plans(&self) -> Vec<(InputId, Option<RecordPlan>)> {
        let replay = &self.replay;
        self.inputs
            .iter()
            .map(|input| {
                let segments = replay
                    .folder
                    .as_ref()
                    .filter(|_| replay.recording && replay.cameras.contains(&input.id))
                    .map(|f| f.join(input.key.to_string()));
                let iso = replay
                    .iso
                    .folder
                    .as_ref()
                    .filter(|_| replay.iso.recording && replay.iso.inputs.contains(&input.id))
                    .map(|f| IsoPlan {
                        folder: f.clone(),
                        name: file_name(&input.name),
                        split_minutes: replay.iso.split_minutes,
                    });
                let plan = (segments.is_some() || iso.is_some()).then_some(RecordPlan {
                    segments,
                    keep_minutes: replay.keep_minutes,
                    kbps: replay.kbps,
                    iso,
                });
                (input.id, plan)
            })
            .collect()
    }

    /// `vma-io` requests for the recorders whose plan changed since `before`.
    pub(crate) fn record_requests(
        &self,
        before: &[(InputId, Option<RecordPlan>)],
    ) -> Vec<IoRequest> {
        self.record_plans()
            .into_iter()
            .filter(|(id, plan)| {
                let old = before
                    .iter()
                    .find(|(b, _)| b == id)
                    .and_then(|(_, p)| p.as_ref());
                old != plan.as_ref()
            })
            .map(|(input, plan)| IoRequest::Record { input, plan })
            .collect()
    }

    /// Inputs whose pictures come through a GStreamer pipeline: the ones a recorder takes.
    fn video_inputs(&self, inputs: &[InputId]) -> Result<(), Rejection> {
        for id in inputs {
            let input = self.input(*id).ok_or(Rejection::UnknownInput(*id))?;
            let recordable = matches!(
                input.spec,
                InputSpec::Capture { .. }
                    | InputSpec::File { .. }
                    | InputSpec::Synthetic { .. }
                    | InputSpec::SyncSource { .. }
                    | InputSpec::Srt(_)
                    | InputSpec::Ndi { .. }
            );
            if !recordable {
                return Err(Rejection::NotRecordable(*id));
            }
        }
        Ok(())
    }

    pub(crate) fn set_replay_session(
        &mut self,
        folder: Option<&PathBuf>,
        keep_minutes: u32,
        kbps: u32,
    ) -> Result<Change, Rejection> {
        if self.replay.recording {
            return Err(Rejection::ReplayRecording);
        }
        if keep_minutes == 0 || !(1_000..=400_000).contains(&kbps) {
            return Err(Rejection::ReplaySettings);
        }
        self.replay.folder = folder.cloned();
        self.replay.keep_minutes = keep_minutes;
        self.replay.kbps = kbps;
        Ok(Change::changed())
    }

    pub(crate) fn set_replay_cameras(&mut self, inputs: &[InputId]) -> Result<Change, Rejection> {
        if inputs.len() > MAX_REPLAY_CAMERAS {
            return Err(Rejection::TooManyCameras);
        }
        self.video_inputs(inputs)?;
        let before = self.record_plans();
        let mut cameras = inputs.to_vec();
        cameras.dedup();
        self.replay.cameras = cameras;
        Ok(self.recorders_changed(&before))
    }

    pub(crate) fn start_replay(&mut self, on: bool) -> Result<Change, Rejection> {
        if on && (self.replay.folder.is_none() || self.replay.cameras.is_empty()) {
            return Err(Rejection::ReplayNotSet);
        }
        let before = self.record_plans();
        if on {
            self.replay.failures.clear();
        }
        self.replay.recording = on;
        Ok(self.recorders_changed(&before))
    }

    pub(crate) fn set_iso(
        &mut self,
        folder: Option<&PathBuf>,
        inputs: &[InputId],
        split_minutes: Option<u32>,
    ) -> Result<Change, Rejection> {
        if self.replay.iso.recording {
            return Err(Rejection::IsoRecording);
        }
        if split_minutes == Some(0) {
            return Err(Rejection::ReplaySettings);
        }
        self.video_inputs(inputs)?;
        let mut inputs = inputs.to_vec();
        inputs.dedup();
        self.replay.iso = IsoState {
            folder: folder.cloned(),
            inputs,
            split_minutes,
            recording: false,
        };
        Ok(Change::changed())
    }

    pub(crate) fn start_iso(&mut self, on: bool) -> Result<Change, Rejection> {
        if on && (self.replay.iso.folder.is_none() || self.replay.iso.inputs.is_empty()) {
            return Err(Rejection::IsoNotSet);
        }
        let before = self.record_plans();
        if on {
            self.replay.failures.clear();
        }
        self.replay.iso.recording = on;
        Ok(self.recorders_changed(&before))
    }

    fn recorders_changed(&self, before: &[(InputId, Option<RecordPlan>)]) -> Change {
        let mut change = Change::changed();
        change.io = self.record_requests(before);
        change
    }

    /// `at_ns`: when, on the shared clock (stamped by `vma-control`).
    pub(crate) fn mark(&mut self, mark: Mark, at_ns: Option<u64>) -> Result<Change, Rejection> {
        let at = || at_ns.ok_or(Rejection::MarkWithoutTime);
        match mark {
            Mark::Last { seconds } => {
                if seconds == 0 || seconds > MAX_MARK_SECONDS {
                    return Err(Rejection::ReplaySettings);
                }
                let out = at()?;
                let span = u64::from(seconds) * 1_000_000_000;
                self.add_event(out.saturating_sub(span), out);
            }
            Mark::In => self.replay.marking = Some(at()?),
            Mark::Out => {
                let out = at()?;
                let start = self.replay.marking.take().ok_or(Rejection::NoMarkIn)?;
                if out <= start {
                    return Err(Rejection::NoMarkIn);
                }
                self.add_event(start, out);
            }
            Mark::Cancel => {
                self.replay.marking.take().ok_or(Rejection::NoMarkIn)?;
            }
        }
        Ok(Change::changed())
    }

    fn add_event(&mut self, in_ns: u64, out_ns: u64) {
        let id = self.replay.next_event;
        self.replay.next_event += 1;
        self.replay.events.push(ReplayEvent {
            id,
            in_ns,
            out_ns,
            angle: None,
            speed: None,
            comment: String::new(),
        });
    }

    pub(crate) fn edit_event(
        &mut self,
        id: u32,
        angle: Option<InputId>,
        speed: Option<u8>,
        comment: &str,
    ) -> Result<Change, Rejection> {
        if speed.is_some_and(|s| s > 100) {
            return Err(Rejection::ReplaySettings);
        }
        if let Some(angle) = angle
            && !self.replay.cameras.contains(&angle)
        {
            return Err(Rejection::UnknownInput(angle));
        }
        let event = self
            .replay
            .events
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or(Rejection::UnknownEvent(id))?;
        event.angle = angle;
        event.speed = speed;
        event.comment = comment.chars().take(200).collect();
        Ok(Change::changed())
    }

    pub(crate) fn delete_event(&mut self, id: u32) -> Result<Change, Rejection> {
        let before = self.replay.events.len();
        self.replay.events.retain(|e| e.id != id);
        if self.replay.events.len() == before {
            return Err(Rejection::UnknownEvent(id));
        }
        Ok(Change::changed())
    }

    /// The input of replay channel `channel`.
    pub fn replay_input(&self, channel: ReplayChannel) -> Option<InputId> {
        self.inputs
            .iter()
            .find(|i| matches!(i.spec, InputSpec::Replay { channel: c } if c == channel))
            .map(|i| i.id)
    }

    /// The folder of the recording of `camera` in the session.
    fn camera_folder(&self, camera: InputId) -> Option<PathBuf> {
        let folder = self.replay.folder.as_ref()?;
        Some(folder.join(self.input(camera)?.key.to_string()))
    }

    /// What the player of channel A plays now.
    fn player_plan(&self) -> PlayerPlan {
        let player = &self.replay.player;
        let event = player
            .event
            .and_then(|id| self.replay.events.iter().find(|e| e.id == id));
        let angle = player
            .angle
            .or_else(|| event.and_then(|e| e.angle))
            .filter(|a| self.replay.cameras.contains(a))
            .or_else(|| self.replay.cameras.first().copied())
            .and_then(|camera| self.camera_folder(camera));
        PlayerPlan {
            angle,
            in_ns: event.map_or(0, |e| e.in_ns),
            out_ns: event.map_or(0, |e| e.out_ns),
            speed: player.speed,
            reverse: player.reverse,
            playing: player.playing,
            cue: player.cue,
        }
    }

    fn player_change(&self) -> Result<Change, Rejection> {
        let input = self
            .replay_input(ReplayChannel::A)
            .ok_or(Rejection::NoReplayInput(ReplayChannel::A))?;
        let mut change = Change::changed();
        change.io.push(IoRequest::ReplayPlayer {
            input,
            plan: self.player_plan(),
        });
        Ok(change)
    }

    pub(crate) fn play_event(&mut self, id: Option<u32>) -> Result<Change, Rejection> {
        self.replay_input(ReplayChannel::A)
            .ok_or(Rejection::NoReplayInput(ReplayChannel::A))?;
        let event = match id {
            Some(id) => self
                .replay
                .events
                .iter()
                .find(|e| e.id == id)
                .ok_or(Rejection::UnknownEvent(id))?,
            None => self.replay.events.last().ok_or(Rejection::NoEvent)?,
        };
        let id = event.id;
        let player = &mut self.replay.player;
        player.event = Some(id);
        player.playing = true;
        player.cue += 1;
        player.angle = None;
        // The operator takes over: no automatic return.
        self.replay.auto = None;
        self.player_change()
    }

    /// « Rejouer » (ADR 0016 § 5, Q14).
    pub(crate) fn auto_replay(&mut self, id: Option<u32>) -> Result<Change, Rejection> {
        let input = self
            .replay_input(ReplayChannel::A)
            .ok_or(Rejection::NoReplayInput(ReplayChannel::A))?;
        let setting = self.button(self.replay.auto_button)?;
        let on_air = self.program == Some(input);
        if !on_air {
            self.require_idle()?;
            self.playable(setting)?;
        }
        let back_to = match self.replay.auto {
            // « Rejouer » again while one runs: the same way back.
            Some(auto) if on_air => auto.back_to,
            _ => self.program.filter(|p| *p != input),
        };
        let mut change = self.play_event(id)?;
        let player = &mut self.replay.player;
        player.speed = self.replay.auto_speed;
        player.reverse = false;
        self.replay.auto = Some(AutoReplay {
            cue: player.cue,
            back_to,
        });
        // The plan again, with the replay's speed.
        change
            .io
            .retain(|r| !matches!(r, IoRequest::ReplayPlayer { .. }));
        change.io.extend(self.player_change()?.io);
        if !on_air {
            let to_replay = self.start(setting, input, Ending::Direct);
            change.io.extend(to_replay.io);
        }
        Ok(change)
    }

    /// The player reached the end of its event: a « Rejouer » goes back on air.
    pub(crate) fn replay_ended(&mut self, input: InputId, cue: u64) -> Change {
        let Some(auto) = self.replay.auto.filter(|a| a.cue == cue) else {
            return Change::default();
        };
        self.replay.auto = None;
        let mut change = Change::changed();
        let back = auto.back_to.filter(|b| self.input(*b).is_some());
        let on_air = self.program == Some(input) && self.transition.is_none();
        if let (Some(back), true) = (back, on_air)
            && let Ok(setting) = self.button(self.replay.auto_button)
            && self.playable(setting).is_ok()
        {
            change
                .io
                .extend(self.start(setting, back, Ending::Direct).io);
        }
        change
    }

    pub(crate) fn player(
        &mut self,
        set: impl FnOnce(&mut PlayerState),
    ) -> Result<Change, Rejection> {
        self.replay_input(ReplayChannel::A)
            .ok_or(Rejection::NoReplayInput(ReplayChannel::A))?;
        set(&mut self.replay.player);
        self.player_change()
    }

    pub(crate) fn step_player(&mut self, frames: i32) -> Result<Change, Rejection> {
        let mut change = self.player(|p| p.playing = false)?;
        if let Some(input) = self.replay_input(ReplayChannel::A) {
            change.io.push(IoRequest::ReplayStep { input, frames });
        }
        Ok(change)
    }

    /// An input removed: no more a camera nor an ISO input (its recorder stops with it).
    pub(crate) fn forget_recorded(&mut self, input: InputId) {
        self.replay.cameras.retain(|c| *c != input);
        self.replay.iso.inputs.retain(|c| *c != input);
        for event in &mut self.replay.events {
            if event.angle == Some(input) {
                event.angle = None;
            }
        }
        if self.replay.player.angle == Some(input) {
            self.replay.player.angle = None;
        }
        if let Some(auto) = &mut self.replay.auto
            && auto.back_to == Some(input)
        {
            auto.back_to = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Command;
    use crate::state::Format;
    use vma_core::{FrameRate, Resolution, Rgb8};

    fn show() -> State {
        let mut s = State::new(Format {
            rate: FrameRate::FPS_50,
            resolution: Resolution::HD_1080,
        });
        for name in ["Caméra 1", "Caméra/2"] {
            s.apply(&Command::AddInput {
                spec: InputSpec::File {
                    path: format!("/media/{name}.mp4").into(),
                },
                name: Some(name.into()),
            })
            .unwrap();
        }
        s.apply(&Command::AddInput {
            spec: InputSpec::Color {
                color: Rgb8::new(1, 0, 0),
            },
            name: None,
        })
        .unwrap();
        s
    }

    fn records(io: &[IoRequest]) -> Vec<(InputId, Option<RecordPlan>)> {
        io.iter()
            .filter_map(|r| match r {
                IoRequest::Record { input, plan } => Some((*input, plan.clone())),
                _ => None,
            })
            .collect()
    }

    /// F2, Q8: one recorder per input serves the replay and the ISO; `vma-io` is told only
    /// when an input's plan changes.
    #[test]
    fn recorders_follow_the_replay_and_the_iso() {
        let mut s = show();
        let (a, b, color) = (s.inputs[0].id, s.inputs[1].id, s.inputs[2].id);
        assert_eq!(
            s.apply(&Command::SetReplayCameras {
                inputs: vec![a, color]
            }),
            Err(Rejection::NotRecordable(color)),
            "a colour has no pipeline to record"
        );
        assert_eq!(
            s.apply(&Command::StartReplayRecording),
            Err(Rejection::ReplayNotSet)
        );
        s.apply(&Command::SetReplaySession {
            folder: Some("/ssd/match".into()),
            keep_minutes: 90,
            kbps: 80_000,
        })
        .unwrap();
        s.apply(&Command::SetReplayCameras { inputs: vec![a, b] })
            .unwrap();
        let io = s.apply(&Command::StartReplayRecording).unwrap();
        let plans = records(&io);
        assert_eq!(plans.len(), 2, "only the inputs whose plan changed");
        let key = s.inputs[0].key.to_string();
        assert_eq!(
            plans[0],
            (
                a,
                Some(RecordPlan {
                    segments: Some(PathBuf::from("/ssd/match").join(&key)),
                    keep_minutes: 90,
                    kbps: 80_000,
                    iso: None,
                })
            )
        );
        assert_eq!(
            s.apply(&Command::SetReplaySession {
                folder: None,
                keep_minutes: 90,
                kbps: 80_000,
            }),
            Err(Rejection::ReplayRecording)
        );

        s.apply(&Command::SetIso {
            folder: Some("/iso".into()),
            inputs: vec![b],
            split_minutes: Some(30),
        })
        .unwrap();
        let io = s.apply(&Command::StartIso).unwrap();
        let plans = records(&io);
        assert_eq!(plans.len(), 1, "only the input whose plan changed");
        let (input, plan) = &plans[0];
        assert_eq!(*input, b);
        let plan = plan.as_ref().unwrap();
        assert!(plan.segments.is_some(), "still in the replay");
        assert_eq!(
            plan.iso,
            Some(IsoPlan {
                folder: "/iso".into(),
                name: "Caméra_2".into(),
                split_minutes: Some(30),
            }),
            "a file name from its name"
        );

        let io = s.apply(&Command::StopReplayRecording).unwrap();
        let plans = records(&io);
        assert_eq!(plans.len(), 2, "the colour was not recorded");
        assert_eq!(plans[0], (a, None), "no more recorded");
        assert!(plans[1].1.as_ref().unwrap().segments.is_none());
        let io = s.apply(&Command::RemoveInput { input: b }).unwrap();
        assert!(!s.replay.iso.inputs.contains(&b));
        assert!(
            io.iter()
                .any(|r| matches!(r, IoRequest::Close { input } if *input == b))
        );
    }

    /// F3: « −5 s », « Entrée » / « Sortie », cancel; events edited and deleted.
    #[test]
    fn events_are_marked() {
        let mut s = show();
        let a = s.inputs[0].id;
        let s_ns = 1_000_000_000u64;
        s.apply(&Command::ReplayMark {
            mark: Mark::Last { seconds: 5 },
            at_ns: Some(100 * s_ns),
        })
        .unwrap();
        assert_eq!(
            (s.replay.events[0].in_ns, s.replay.events[0].out_ns),
            (95 * s_ns, 100 * s_ns)
        );
        assert_eq!(
            s.apply(&Command::ReplayMark {
                mark: Mark::Last { seconds: 5 },
                at_ns: None,
            }),
            Err(Rejection::MarkWithoutTime)
        );
        assert_eq!(
            s.apply(&Command::ReplayMark {
                mark: Mark::Out,
                at_ns: Some(110 * s_ns),
            }),
            Err(Rejection::NoMarkIn)
        );
        s.apply(&Command::ReplayMark {
            mark: Mark::In,
            at_ns: Some(120 * s_ns),
        })
        .unwrap();
        s.apply(&Command::ReplayMark {
            mark: Mark::Out,
            at_ns: Some(127 * s_ns),
        })
        .unwrap();
        let second = &s.replay.events[1];
        assert_eq!(
            (second.in_ns, second.out_ns, second.id),
            (120 * s_ns, 127 * s_ns, 2)
        );
        s.apply(&Command::ReplayMark {
            mark: Mark::In,
            at_ns: Some(130 * s_ns),
        })
        .unwrap();
        s.apply(&Command::ReplayMark {
            mark: Mark::Cancel,
            at_ns: None,
        })
        .unwrap();
        assert_eq!(s.replay.marking, None);

        assert_eq!(
            s.apply(&Command::EditReplayEvent {
                id: 1,
                angle: Some(a),
                speed: Some(50),
                comment: "But".into(),
            }),
            Err(Rejection::UnknownInput(a)),
            "an angle is a replay camera"
        );
        s.apply(&Command::SetReplayCameras { inputs: vec![a] })
            .unwrap();
        s.apply(&Command::EditReplayEvent {
            id: 1,
            angle: Some(a),
            speed: Some(50),
            comment: "But".into(),
        })
        .unwrap();
        assert_eq!(s.replay.events[0].speed, Some(50));
        s.apply(&Command::DeleteReplayEvent { id: 2 }).unwrap();
        assert_eq!(s.replay.events.len(), 1);
        assert_eq!(
            s.apply(&Command::DeleteReplayEvent { id: 2 }),
            Err(Rejection::UnknownEvent(2))
        );
    }

    fn players(io: &[IoRequest]) -> Vec<PlayerPlan> {
        io.iter()
            .filter_map(|r| match r {
                IoRequest::ReplayPlayer { plan, .. } => Some(plan.clone()),
                _ => None,
            })
            .collect()
    }

    /// ADR 0016: one replay input per channel; « play » cues the last event (or the one
    /// asked for) from its angle, the player keeps its speed and direction; steps pause it.
    #[test]
    fn the_replay_channel_plays_the_events() {
        let mut s = show();
        let (a, b) = (s.inputs[0].id, s.inputs[1].id);
        assert_eq!(
            s.apply(&Command::ReplayPlayEvent { id: None }),
            Err(Rejection::NoReplayInput(ReplayChannel::A))
        );
        let replay = InputSpec::Replay {
            channel: ReplayChannel::A,
        };
        let io = s
            .apply(&Command::AddInput {
                spec: replay.clone(),
                name: None,
            })
            .unwrap();
        let input = s.replay_input(ReplayChannel::A).unwrap();
        assert_eq!(s.input(input).unwrap().name, "Replay A");
        assert!(
            io.iter()
                .any(|r| matches!(r, IoRequest::Open { input: i, .. } if *i == input))
        );
        assert_eq!(
            s.apply(&Command::AddInput {
                spec: replay,
                name: None,
            }),
            Err(Rejection::ReplayChannelTaken(ReplayChannel::A))
        );
        assert_eq!(
            s.apply(&Command::ReplayPlayEvent { id: None }),
            Err(Rejection::NoEvent)
        );
        s.apply(&Command::SetReplaySession {
            folder: Some("/ssd".into()),
            keep_minutes: 60,
            kbps: 50_000,
        })
        .unwrap();
        s.apply(&Command::SetReplayCameras { inputs: vec![a, b] })
            .unwrap();
        let sec = 1_000_000_000u64;
        for at in [100, 200] {
            s.apply(&Command::ReplayMark {
                mark: Mark::Last { seconds: 5 },
                at_ns: Some(at * sec),
            })
            .unwrap();
        }
        s.apply(&Command::EditReplayEvent {
            id: 1,
            angle: Some(b),
            speed: None,
            comment: String::new(),
        })
        .unwrap();
        s.apply(&Command::ReplaySpeed { percent: 50 }).unwrap();
        let plan = players(&s.apply(&Command::ReplayPlayEvent { id: None }).unwrap()).remove(0);
        let (key_a, key_b) = (s.inputs[0].key.to_string(), s.inputs[1].key.to_string());
        assert_eq!(
            plan,
            PlayerPlan {
                angle: Some(PathBuf::from("/ssd").join(&key_a)),
                in_ns: 195 * sec,
                out_ns: 200 * sec,
                speed: 50,
                reverse: false,
                playing: true,
                cue: 1,
            },
            "the last event, from the first camera"
        );
        let plan = players(&s.apply(&Command::ReplayPlayEvent { id: Some(1) }).unwrap()).remove(0);
        assert_eq!(
            plan.angle,
            Some(PathBuf::from("/ssd").join(&key_b)),
            "its angle"
        );
        assert_eq!((plan.in_ns, plan.cue), (95 * sec, 2));

        let plan = players(&s.apply(&Command::ReplayReverse { on: true }).unwrap()).remove(0);
        assert_eq!((plan.reverse, plan.cue), (true, 2), "no new cue");
        assert_eq!(
            s.apply(&Command::ReplaySpeed { percent: 101 }),
            Err(Rejection::ReplaySettings)
        );
        let io = s.apply(&Command::ReplayStep { frames: -1 }).unwrap();
        assert!(!players(&io)[0].playing, "a step pauses");
        assert!(
            io.iter()
                .any(|r| matches!(r, IoRequest::ReplayStep { frames: -1, .. }))
        );
        let plan = players(&s.apply(&Command::ReplayPlay).unwrap()).remove(0);
        assert!(plan.playing && plan.reverse && plan.speed == 50);
    }

    /// A show with a camera on air, a second camera, Replay A, two events.
    fn on_air() -> (State, InputId, InputId, InputId) {
        let mut s = show();
        let (a, b) = (s.inputs[0].id, s.inputs[1].id);
        s.apply(&Command::AddInput {
            spec: InputSpec::Replay {
                channel: ReplayChannel::A,
            },
            name: None,
        })
        .unwrap();
        let replay = s.replay_input(ReplayChannel::A).unwrap();
        s.apply(&Command::SetReplaySession {
            folder: Some("/ssd".into()),
            keep_minutes: 60,
            kbps: 50_000,
        })
        .unwrap();
        s.apply(&Command::SetReplayCameras { inputs: vec![a, b] })
            .unwrap();
        s.apply(&Command::CutDirect { input: a }).unwrap();
        s.apply(&Command::SetPreview { input: b }).unwrap();
        for at in [100u64, 200] {
            s.apply(&Command::ReplayMark {
                mark: Mark::Last { seconds: 5 },
                at_ns: Some(at * 1_000_000_000),
            })
            .unwrap();
        }
        (s, a, b, replay)
    }

    fn done(s: &mut State) {
        let id = s.transition.unwrap().id();
        s.apply_event(&crate::state::Event::TransitionDone { id });
    }

    /// F6, Q14: « Rejouer » takes the last event to Replay A at 50 % with the transition of
    /// button 1 (the Preview untouched), and at its end goes back to the camera on air before.
    #[test]
    fn rejouer_goes_to_the_replay_and_back() {
        let (mut s, a, b, replay) = on_air();
        let io = s.apply(&Command::ReplayAuto { id: None }).unwrap();
        let plan = players(&io).pop().unwrap();
        assert_eq!((plan.speed, plan.playing, plan.cue), (50, true, 1));
        assert_eq!(plan.in_ns, 195_000_000_000, "the last event");
        assert!(s.transition.is_some(), "button 1's transition");
        done(&mut s);
        assert_eq!((s.program, s.preview), (Some(replay), Some(b)));
        assert_eq!(
            s.replay.auto,
            Some(AutoReplay {
                cue: 1,
                back_to: Some(a)
            })
        );
        // The end of another cue changes nothing.
        s.apply_event(&crate::state::Event::ReplayEnded {
            input: replay,
            cue: 7,
        });
        assert!(s.transition.is_none());
        s.apply_event(&crate::state::Event::ReplayEnded {
            input: replay,
            cue: 1,
        });
        assert!(s.transition.is_some(), "back");
        done(&mut s);
        assert_eq!(
            (s.program, s.preview, s.replay.auto),
            (Some(a), Some(b), None)
        );
    }

    /// The operator takes over: cut elsewhere during the replay, or a replay played by hand:
    /// no automatic return. « Rejouer » again on air keeps the way back.
    #[test]
    fn the_operator_keeps_the_last_word() {
        let (mut s, a, b, replay) = on_air();
        s.apply(&Command::ReplayAuto { id: None }).unwrap();
        done(&mut s);
        s.apply(&Command::ReplayAuto { id: Some(1) }).unwrap();
        assert!(s.transition.is_none(), "already on air: no transition");
        assert_eq!(s.replay.auto.unwrap().back_to, Some(a), "the same way back");
        s.apply(&Command::CutDirect { input: b }).unwrap();
        s.apply_event(&crate::state::Event::ReplayEnded {
            input: replay,
            cue: 2,
        });
        assert_eq!(
            (s.program, s.transition),
            (Some(b), None),
            "left where the operator put it"
        );

        s.apply(&Command::ReplayAuto { id: None }).unwrap();
        done(&mut s);
        s.apply(&Command::ReplayPlayEvent { id: Some(1) }).unwrap();
        assert_eq!(s.replay.auto, None, "played by hand");

        assert_eq!(
            s.apply(&Command::SetReplayAuto {
                speed: 25,
                button: 5
            }),
            Err(Rejection::UnknownButton(5))
        );
        s.apply(&Command::SetReplayAuto {
            speed: 25,
            button: 2,
        })
        .unwrap();
        s.apply(&Command::CutDirect { input: a }).unwrap();
        let plan = players(&s.apply(&Command::ReplayAuto { id: None }).unwrap())
            .pop()
            .unwrap();
        assert_eq!(plan.speed, 25);
    }

    /// F5: another camera while playing: the plan's angle changes, not its cue.
    #[test]
    fn another_angle_while_playing() {
        let (mut s, a, b, _) = on_air();
        let first = players(&s.apply(&Command::ReplayPlayEvent { id: None }).unwrap())
            .pop()
            .unwrap();
        let plan = players(&s.apply(&Command::ReplayAngle { input: b }).unwrap())
            .pop()
            .unwrap();
        let key_b = s.input(b).unwrap().key.to_string();
        assert_eq!(plan.angle, Some(PathBuf::from("/ssd").join(key_b)));
        assert_eq!(plan.cue, first.cue, "the same moment, not from the start");
        let replay = s.replay_input(ReplayChannel::A).unwrap();
        assert_eq!(
            s.apply(&Command::ReplayAngle { input: replay }),
            Err(Rejection::NotACamera(replay))
        );
        // A new event plays from its own angle again.
        let next = players(&s.apply(&Command::ReplayPlayEvent { id: None }).unwrap())
            .pop()
            .unwrap();
        assert_ne!(next.angle, plan.angle);
        let _ = a;
    }
}
