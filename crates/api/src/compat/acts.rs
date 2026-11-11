//! Activators (`ACTS`, `api-compat.md` § 5): values from 0 to 1 that the TCP API gives on
//! request and, to a client that subscribed, whenever they change.

use std::collections::BTreeMap;

use vma_core::AudioBus;
use vma_engine::{OutputSpec, State};

use super::functions::db_to_volume;

/// Name and input number (from 1) of an activator.
pub type ActKey = (String, Option<usize>);

/// Every activator of `state` with its value.
pub fn activators(state: &State) -> BTreeMap<ActKey, f32> {
    let mut acts = BTreeMap::new();
    let flag = |b: bool| if b { 1.0 } else { 0.0 };
    let mut put = |name: &str, input: Option<usize>, value: f32| {
        acts.insert((name.to_owned(), input), value);
    };
    for (n, input) in state.inputs.iter().enumerate() {
        let number = Some(n + 1);
        put("Input", number, flag(state.program == Some(input.id)));
        put(
            "InputPreview",
            number,
            flag(state.preview == Some(input.id)),
        );
        if let Some(playback) = input.playback {
            put("InputPlaying", number, flag(playback.playing));
        }
        if let Some(audio) = input.audio {
            let send = |bus: AudioBus| flag(audio.sends.get(bus.index()) == Some(&true));
            put("InputVolume", number, db_to_volume(audio.gain) / 100.0);
            put("InputAudio", number, flag(!audio.mute));
            put("InputSolo", number, flag(audio.solo));
            put("InputAudioAuto", number, flag(audio.follow_video));
            put("InputMasterAudio", number, send(AudioBus::Master));
            put("InputBusAAudio", number, send(AudioBus::A));
            put("InputBusBAudio", number, send(AudioBus::B));
        }
    }
    for (n, overlay) in state.overlays.iter().enumerate() {
        let shown = overlay
            .input
            .and_then(|i| state.inputs.iter().position(|x| x.id == i))
            .map(|p| p + 1);
        if shown.is_some() {
            put(&format!("Overlay{}", n + 1), shown, flag(overlay.on));
        }
    }
    for (bus, prefix) in [
        (AudioBus::Master, "Master"),
        (AudioBus::A, "BusA"),
        (AudioBus::B, "BusB"),
    ] {
        let set = state
            .audio
            .buses
            .get(bus.index())
            .copied()
            .unwrap_or_default();
        put(
            &format!("{prefix}Volume"),
            None,
            db_to_volume(set.gain) / 100.0,
        );
        put(&format!("{prefix}Audio"), None, flag(!set.mute));
        if bus != AudioBus::Master {
            put(&format!("{prefix}Solo"), None, 0.0);
        }
    }
    put("MasterHeadphones", None, 1.0);
    let on =
        |f: fn(&OutputSpec) -> bool| state.outputs.iter().any(|o| f(&o.spec) && o.status.is_on());
    put("FadeToBlack", None, flag(state.ftb.active));
    put(
        "Recording",
        None,
        flag(on(|s| matches!(s, OutputSpec::Record(_)))),
    );
    put(
        "Streaming",
        None,
        flag(on(|s| {
            matches!(s, OutputSpec::Rtmp(_) | OutputSpec::Srt(_))
        })),
    );
    put("Fullscreen", None, flag(on(OutputSpec::is_fullscreen)));
    put("External", None, 0.0);
    put("MultiCorder", None, 0.0);
    acts
}

/// `Input 3 1`, `FadeToBlack 0`, `InputVolume 2 0.5`.
pub fn act_text((name, input): &ActKey, value: f32) -> String {
    match input {
        Some(n) => format!("{name} {n} {value}"),
        None => format!("{name} {value}"),
    }
}

/// The answer to `ACTS <name> [input]`: the text after `ACTS OK`, or the error after
/// `ACTS ER`. Without input, `Input`, `InputPreview` and `Overlay1`–`4` give the input they
/// have now.
pub fn query(state: &State, name: &str, input: Option<usize>) -> Result<String, String> {
    let acts = activators(state);
    // Named by input even when none has them now.
    const BY_INPUT: [&str; 6] = [
        "Input",
        "InputPreview",
        "Overlay1",
        "Overlay2",
        "Overlay3",
        "Overlay4",
    ];
    let exact = acts
        .keys()
        .map(|(n, _)| n.as_str())
        .chain(BY_INPUT)
        .find(|n| n.eq_ignore_ascii_case(name))
        .map(str::to_owned)
        .ok_or_else(|| format!("Unknown activator {name}"))?;
    let key = match input {
        Some(n) => (exact, Some(n)),
        None if acts.contains_key(&(exact.clone(), None)) => (exact, None),
        // The input it has now: the one at 1 (Program, Preview, an overlay that is in).
        None => acts
            .iter()
            .find(|((n, i), v)| *n == exact && i.is_some() && **v >= 1.0)
            .map(|(k, _)| k.clone())
            .ok_or_else(|| "No Input".to_owned())?,
    };
    acts.get(&key)
        .map(|v| act_text(&key, *v))
        .ok_or_else(|| "No Input".to_owned())
}

/// What changed from `before` to `now`, as `ACTS OK` texts.
pub fn changes(before: &BTreeMap<ActKey, f32>, now: &BTreeMap<ActKey, f32>) -> Vec<String> {
    now.iter()
        .filter(|(k, v)| before.get(*k) != Some(*v))
        .map(|(k, v)| act_text(k, *v))
        .collect()
}
