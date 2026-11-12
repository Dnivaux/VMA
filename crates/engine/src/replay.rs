//! Replay and ISO in the engine's state (Phase 6, ADR 0015, ADR 0016): the session (folder,
//! time kept, bit rate), the cameras recorded for the replay, the inputs recorded as ISO, the
//! events. From them, what each input's recorder must do ([`State::record_plans`]): `vma-io`
//! is told when it changes.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use vma_core::InputId;

use crate::command::InputSpec;
use crate::state::{Change, IoRequest, Rejection, State};

/// Cameras recorded for the replay at most (the reference's 8; 4 measured, `phase-6.md` Q3).
pub const MAX_REPLAY_CAMERAS: usize = 8;
/// Longest « −n s » event.
pub const MAX_MARK_SECONDS: u32 = 120;

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

    /// An input removed: no more a camera nor an ISO input (its recorder stops with it).
    pub(crate) fn forget_recorded(&mut self, input: InputId) {
        self.replay.cameras.retain(|c| *c != input);
        self.replay.iso.inputs.retain(|c| *c != input);
        for event in &mut self.replay.events {
            if event.angle == Some(input) {
                event.angle = None;
            }
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
}
