//! The replay functions of the compat API (`api-compat.md` § 4, « Replay ») and the ISO ones
//! (`StartMultiCorder`…). VMA has one replay channel (A), one event list, one angle per event,
//! and no « recorded » mode: the functions about channel B, the 20 lists, the selected event or
//! the recorded mode are not supported.

use vma_engine::{Command, MAX_REPLAY_SPEED, Mark, ReplayChannel};

use super::{CompatError, Ctx, numbered};

/// The commands of `name` (in lower case) if it is a replay or ISO function VMA has.
pub(super) fn resolve(cx: &Ctx<'_>, name: &str) -> Option<Result<Vec<Command>, CompatError>> {
    let one = |c: Command| Some(Ok(vec![c]));
    let state = cx.state;
    let replay = &state.replay;
    let player = &replay.player;
    // `Channel`: the Companion module sends Current, A or B (the documentation gives no list).
    let channel_a = || match cx.call.param("Channel") {
        None => Ok(()),
        Some(c) if c.eq_ignore_ascii_case("current") || c.eq_ignore_ascii_case("a") => Ok(()),
        Some(c) => Err(CompatError::Refused(format!(
            "VMA has one replay channel (A), not {c}"
        ))),
    };
    let with_a = |commands: Vec<Command>| Some(channel_a().map(|()| commands));
    let last_event = || {
        replay
            .events
            .last()
            .ok_or_else(|| CompatError::Refused("no replay event".into()))
    };
    // Speeds of the compat API: 0 to 1 (0.5 = 50 %).
    let speed = |cx: &Ctx<'_>| -> Result<u8, CompatError> {
        let value = cx.value()?;
        let speed: f32 = value
            .trim()
            .parse()
            .map_err(|_| cx.invalid("Value", value))?;
        if !(0.0..=1.0).contains(&speed) {
            return Err(cx.invalid("Value", value));
        }
        Ok((speed * f32::from(MAX_REPLAY_SPEED)).round() as u8)
    };
    let seconds = |cx: &Ctx<'_>| -> Result<u32, CompatError> {
        let value = cx.value()?;
        value
            .trim()
            .parse()
            .ok()
            .filter(|s| *s > 0)
            .ok_or_else(|| cx.invalid("Value", value))
    };
    let mark = |mark: Mark| Some(Ok(vec![Command::ReplayMark { mark, at_ns: None }]));

    match name {
        "replaystartrecording" => return one(Command::StartReplayRecording),
        "replaystoprecording" => return one(Command::StopReplayRecording),
        "replaystartstoprecording" => {
            return one(if replay.recording {
                Command::StopReplayRecording
            } else {
                Command::StartReplayRecording
            });
        }
        "startmulticorder" => return one(Command::StartIso),
        "stopmulticorder" => return one(Command::StopIso),
        "startstopmulticorder" => {
            return one(if replay.iso.recording {
                Command::StopIso
            } else {
                Command::StartIso
            });
        }
        // VMA marks in the live recording only.
        "replaymarkin" | "replaymarkinlive" => return mark(Mark::In),
        "replaymarkout" => return mark(Mark::Out),
        "replaymarkcancel" => return mark(Mark::Cancel),
        "replaymarkinout" | "replaymarkinoutlive" => {
            return Some(seconds(cx).map(|seconds| {
                vec![Command::ReplayMark {
                    mark: Mark::Last { seconds },
                    at_ns: None,
                }]
            }));
        }
        "replayplay" => return with_a(vec![Command::ReplayPlay]),
        "replaypause" => return with_a(vec![Command::ReplayPause]),
        "replayplaypause" => {
            return with_a(vec![if player.playing {
                Command::ReplayPause
            } else {
                Command::ReplayPlay
            }]);
        }
        "replayplayforward" | "replayplaybackward" => {
            let on = name == "replayplaybackward";
            return with_a(vec![Command::ReplayReverse { on }, Command::ReplayPlay]);
        }
        "replaysetdirectionforward" | "replaysetdirectionbackward" => {
            let on = name == "replaysetdirectionbackward";
            return with_a(vec![Command::ReplayReverse { on }]);
        }
        "replaychangedirection" => {
            return with_a(vec![Command::ReplayReverse {
                on: !player.reverse,
            }]);
        }
        "replayplaylastevent" => return with_a(vec![Command::ReplayPlayEvent { id: None }]),
        "replayplaylasteventtooutput" => return with_a(vec![Command::ReplayAuto { id: None }]),
        "replayplayevent" | "replayplayeventtooutput" => {
            // « Event Number where 0 is bottom of list »: 0 is the latest.
            let found = cx
                .number::<usize>("Value")
                .and_then(|n| n.ok_or_else(|| cx.missing("Value")))
                .and_then(|n| {
                    replay
                        .events
                        .iter()
                        .rev()
                        .nth(n)
                        .map(|e| e.id)
                        .ok_or_else(|| CompatError::Refused(format!("no replay event {n}")))
                });
            let id = match found {
                Ok(id) => Some(id),
                Err(e) => return Some(Err(e)),
            };
            return with_a(vec![if name == "replayplayevent" {
                Command::ReplayPlayEvent { id }
            } else {
                Command::ReplayAuto { id }
            }]);
        }
        "replaysetspeed" => {
            return Some(
                channel_a()
                    .and_then(|()| speed(cx))
                    .map(|percent| vec![Command::ReplaySpeed { percent }]),
            );
        }
        "setrateslowmotion" => {
            let input = match cx.input() {
                Ok(input) => input,
                Err(e) => return Some(Err(e)),
            };
            if input.is_some() && input != state.replay_input(ReplayChannel::A) {
                return Some(Err(CompatError::Refused(
                    "the slow motion speed is replay A's".into(),
                )));
            }
            return Some(speed(cx).map(|percent| vec![Command::ReplaySpeed { percent }]));
        }
        "replaychangespeed" => {
            // By how much, from -1 to 1 (the Companion module's range).
            let value = match cx.value() {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(change) = value
                .trim()
                .parse::<f32>()
                .ok()
                .filter(|c| (-1.0..=1.0).contains(c))
            else {
                return Some(Err(cx.invalid("Value", value)));
            };
            let max = f32::from(MAX_REPLAY_SPEED);
            let percent = (f32::from(player.speed) + change * max)
                .clamp(0.0, max)
                .round() as u8;
            return with_a(vec![Command::ReplaySpeed { percent }]);
        }
        "replayjumpframes" => {
            return Some(
                channel_a()
                    .and_then(|()| cx.number::<i32>("Value"))
                    .and_then(|f| f.ok_or_else(|| cx.missing("Value")))
                    .map(|frames| vec![Command::ReplayStep { frames }]),
            );
        }
        "replaydeletelastevent" => {
            return Some(
                channel_a()
                    .and_then(|()| last_event())
                    .map(|e| vec![Command::DeleteReplayEvent { id: e.id }]),
            );
        }
        "replaysetlasteventtext" => {
            return Some(last_event().and_then(|e| {
                Ok(vec![Command::EditReplayEvent {
                    id: e.id,
                    angle: e.angle,
                    speed: e.speed,
                    comment: cx.value()?.to_owned(),
                }])
            }));
        }
        "replaylasteventsinglecameraon" => {
            return Some(last_event().and_then(|e| {
                let camera = cx
                    .number::<usize>("Value")?
                    .ok_or_else(|| cx.missing("Value"))?;
                Ok(vec![Command::EditReplayEvent {
                    id: e.id,
                    angle: Some(nth_camera(cx, camera)?),
                    speed: e.speed,
                    comment: e.comment.clone(),
                }])
            }));
        }
        // Channel A is the only one: nothing to select.
        "replayselectchannela" => return Some(Ok(Vec::new())),
        _ => {}
    }
    // ReplayACamera1–8 and ReplayCamera1–8 (the selected channel: A).
    let camera =
        numbered(name, "replayacamera", 1..=8).or_else(|| numbered(name, "replaycamera", 1..=8))?;
    Some(nth_camera(cx, usize::from(camera)).map(|input| vec![Command::ReplayAngle { input }]))
}

/// Replay camera `n` (from 1), in the order of the replay settings.
fn nth_camera(cx: &Ctx<'_>, n: usize) -> Result<vma_core::InputId, CompatError> {
    n.checked_sub(1)
        .and_then(|i| cx.state.replay.cameras.get(i))
        .copied()
        .ok_or_else(|| CompatError::Refused(format!("no replay camera {n}")))
}
