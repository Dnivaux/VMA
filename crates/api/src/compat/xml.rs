//! The state as the compat API's XML (`docs/specs/api-compat.md` § 3): the elements of the
//! public documentation, and those the Companion module needs or it drops the document.
//! Values still to be checked against the reference's own output (Q3) are marked « Q3 ».

use std::fmt::Write as _;

use vma_core::AudioBus;
use vma_engine::{
    Effect, InputSpec, InputStatus, MAX_REPLAY_SPEED, OutputSpec, State, TransitionSetting,
};

use super::functions::db_to_volume;
use super::{EDITION, ROOT, VERSION};

/// `&`, `<`, `>`, `"` and `'` escaped, for text and attribute values.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Control characters other than tab and line ends are not allowed in XML 1.0.
            c if c.is_control() && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

fn yes(b: bool) -> &'static str {
    if b { "True" } else { "False" }
}

/// Type names (Q3: as the reference writes them).
fn kind(spec: &InputSpec) -> &'static str {
    match spec {
        InputSpec::Capture { .. } => "Capture",
        InputSpec::File { .. } => "Video",
        InputSpec::Image { .. } => "Image",
        InputSpec::Color { .. }
        | InputSpec::TestSource { .. }
        | InputSpec::Synthetic { .. }
        | InputSpec::SyncSource { .. } => "Colour",
        InputSpec::AudioDevice { .. } => "Audio",
        InputSpec::Srt(_) => "SRT",
        InputSpec::Ndi { .. } => "NDI",
        InputSpec::Html { .. } => "Browser",
        InputSpec::Replay { .. } => "Replay",
    }
}

fn effect(setting: TransitionSetting) -> String {
    match setting.effect {
        Effect::Fade => "Fade".into(),
        Effect::Cut => "Cut".into(),
        Effect::Stinger(slot) => format!("Stinger{}", slot.number()),
    }
}

/// Bus element names (Q3).
fn bus_element(bus: AudioBus) -> &'static str {
    match bus {
        AudioBus::Master => "master",
        AudioBus::A => "busA",
        AudioBus::B => "busB",
    }
}

fn bus_letter(bus: AudioBus) -> &'static str {
    match bus {
        AudioBus::Master => "M",
        AudioBus::A => "A",
        AudioBus::B => "B",
    }
}

/// An element of the state: its attributes, its text, its children.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Element {
    pub name: &'static str,
    pub attrs: Vec<(&'static str, String)>,
    pub text: Option<String>,
    pub children: Vec<Element>,
}

impl Element {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            ..Self::default()
        }
    }

    fn text(name: &'static str, text: impl ToString) -> Self {
        Self {
            name,
            text: Some(text.to_string()),
            ..Self::default()
        }
    }

    fn attr(mut self, name: &'static str, value: impl ToString) -> Self {
        self.attrs.push((name, value.to_string()));
        self
    }

    fn child(mut self, child: Self) -> Self {
        self.children.push(child);
        self
    }

    fn write(&self, x: &mut String) {
        x.push('<');
        x.push_str(self.name);
        for (name, value) in &self.attrs {
            // `write!` into a String cannot fail.
            let _ = write!(x, " {name}=\"{}\"", escape(value));
        }
        if self.text.is_none() && self.children.is_empty() {
            x.push_str("/>");
            return;
        }
        x.push('>');
        if let Some(text) = &self.text {
            x.push_str(&escape(text));
        }
        for child in &self.children {
            child.write(x);
        }
        let _ = write!(x, "</{}>", self.name);
    }

    /// The document.
    pub fn to_xml(&self) -> String {
        let mut x = String::with_capacity(4096);
        self.write(&mut x);
        x
    }

    /// A value by an XPath of the simple kind boxes use (`phase-5.md` Q14):
    /// `root/inputs/input[2]/@title`, `root/preview`, `root/inputs/input[@number='3']`. An
    /// element gives its text, `@name` an attribute.
    pub fn query(&self, path: &str) -> Option<String> {
        let mut steps = path.trim().trim_start_matches('/').split('/');
        if steps.next()? != self.name {
            return None;
        }
        let mut at = self;
        for step in steps {
            if let Some(attr) = step.strip_prefix('@') {
                return at
                    .attrs
                    .iter()
                    .find(|(n, _)| *n == attr)
                    .map(|(_, v)| v.clone());
            }
            let (name, filter) = match step.split_once('[') {
                Some((name, rest)) => (name, Some(rest.strip_suffix(']')?)),
                None => (step, None),
            };
            let mut candidates = at.children.iter().filter(|c| c.name == name);
            at = match filter {
                None => candidates.next()?,
                Some(f) if f.starts_with('@') => {
                    let (attr, value) = f.strip_prefix('@')?.split_once('=')?;
                    let value = value.trim_matches(|c| c == '\'' || c == '"');
                    candidates.find(|c| c.attrs.iter().any(|(n, v)| *n == attr && v == value))?
                }
                Some(n) => candidates.nth(n.trim().parse::<usize>().ok()?.checked_sub(1)?)?,
            };
        }
        Some(at.text.clone().unwrap_or_default())
    }
}

/// The state as an element tree.
pub fn state_tree(state: &State) -> Element {
    let number = |id| {
        state
            .inputs
            .iter()
            .position(|i| Some(i.id) == id)
            .map_or(0, |n| n + 1)
    };
    let mut inputs = Element::new("inputs");
    for (n, input) in state.inputs.iter().enumerate() {
        let audio = input.audio;
        let playing = input.playback.is_some_and(|p| p.playing)
            || matches!(input.status, InputStatus::Live { .. }) && input.playback.is_none();
        let buses: Vec<&str> = AudioBus::ALL
            .into_iter()
            .filter(|b| audio.is_some_and(|a| a.sends.get(b.index()) == Some(&true)))
            .map(bus_letter)
            .collect();
        let mut element = Element::new("input")
            .attr("key", input.key)
            .attr("number", n + 1)
            .attr("type", kind(&input.spec))
            .attr("title", &input.name)
            .attr("shortTitle", &input.name)
            .attr("state", if playing { "Running" } else { "Paused" })
            .attr("position", 0)
            .attr("duration", 0)
            .attr("loop", yes(input.playback.is_some_and(|p| p.looping)))
            .attr("muted", yes(audio.is_some_and(|a| a.mute)))
            .attr(
                "volume",
                format!("{:.0}", audio.map_or(100.0, |a| db_to_volume(a.gain))),
            )
            .attr("balance", 0)
            .attr("solo", yes(audio.is_some_and(|a| a.solo)))
            .attr("audiobusses", buses.join(","))
            .attr("meterF1", 0)
            .attr("meterF2", 0)
            .attr("gainDb", 0)
            .attr("selectedIndex", 1);
        element.text = Some(input.name.clone());
        if matches!(input.spec, InputSpec::Replay { .. }) {
            element = element.child(replay(state));
        }
        inputs = inputs.child(element);
    }
    let mut overlays = Element::new("overlays");
    for (n, overlay) in state.overlays.iter().enumerate() {
        let mut element = Element::new("overlay").attr("number", n + 1);
        element.text = overlay
            .input
            .filter(|_| overlay.on)
            .map(|i| number(Some(i)).to_string());
        overlays = overlays.child(element);
    }
    let mut transitions = Element::new("transitions");
    for (n, button) in state.buttons.iter().enumerate() {
        transitions = transitions.child(
            Element::new("transition")
                .attr("number", n + 1)
                .attr("effect", effect(*button))
                .attr("duration", button.duration_ms),
        );
    }
    let on =
        |f: fn(&OutputSpec) -> bool| state.outputs.iter().any(|o| f(&o.spec) && o.status.is_on());
    let mut audio = Element::new("audio");
    for bus in AudioBus::ALL {
        let set = state
            .audio
            .buses
            .get(bus.index())
            .copied()
            .unwrap_or_default();
        let mut element = Element::new(bus_element(bus))
            .attr("volume", format!("{:.0}", db_to_volume(set.gain)))
            .attr("muted", yes(set.mute))
            .attr("meterF1", 0)
            .attr("meterF2", 0);
        element = if bus == AudioBus::Master {
            element.attr("headphonesVolume", 100)
        } else {
            element.attr("solo", "False").attr("sendToMaster", "True")
        };
        audio = audio.child(element);
    }
    Element::new(ROOT)
        .child(Element::text("version", VERSION))
        .child(Element::text("edition", EDITION))
        .child(inputs)
        .child(overlays)
        .child(Element::text("preview", number(state.preview)))
        .child(Element::text("active", number(state.program)))
        .child(Element::text("fadeToBlack", yes(state.ftb.active)))
        .child(transitions)
        .child(Element::text(
            "recording",
            yes(on(|s| matches!(s, OutputSpec::Record(_)))),
        ))
        .child(Element::text("external", "False"))
        .child(Element::text(
            "streaming",
            yes(on(|s| {
                matches!(s, OutputSpec::Rtmp(_) | OutputSpec::Srt(_))
            })),
        ))
        .child(Element::text("playList", "False"))
        .child(Element::text(
            "multiCorder",
            yes(state.replay.iso.recording),
        ))
        .child(Element::text(
            "fullscreen",
            yes(on(OutputSpec::is_fullscreen)),
        ))
        .child(audio)
}

/// The `replay` element of the Replay input, as the Companion module reads it: channel A
/// only (`channelMode` `A`), one event list, speeds from 0 to 1, the camera as its number among
/// the replay cameras. No « live » mode, and no timecode yet (the position is the player's,
/// in `vma-io`): empty.
fn replay(state: &State) -> Element {
    let replay = &state.replay;
    let camera = state
        .replay_camera()
        .and_then(|c| replay.cameras.iter().position(|x| *x == c))
        .map_or(1, |n| n + 1);
    let speed = format!(
        "{}",
        f32::from(replay.player.speed) / f32::from(MAX_REPLAY_SPEED)
    );
    Element::new("replay")
        .attr("live", "False")
        .attr("recording", yes(replay.recording))
        .attr("channelMode", "A")
        .attr("events", 1)
        .attr("eventsA", 1)
        .attr("cameraA", camera)
        .attr("speed", &speed)
        .attr("speedA", &speed)
        .child(Element::new("timecode"))
        .child(Element::new("timecodeA"))
        .child(Element::new("timecodeB"))
}

/// The XML document of `state`.
pub fn state_xml(state: &State) -> String {
    state_tree(state).to_xml()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_paths_are_answered() {
        let tree = Element::new("r").child(Element::text("preview", 3)).child(
            Element::new("inputs")
                .child(
                    Element::text("input", "A")
                        .attr("number", 1)
                        .attr("title", "A"),
                )
                .child(Element::text("input", "B & C").attr("number", 2)),
        );
        assert_eq!(tree.query("r/preview").as_deref(), Some("3"));
        assert_eq!(tree.query("/r/inputs/input[2]").as_deref(), Some("B & C"));
        assert_eq!(tree.query("r/inputs/input[1]/@title").as_deref(), Some("A"));
        assert_eq!(
            tree.query("r/inputs/input[@number='2']").as_deref(),
            Some("B & C")
        );
        assert_eq!(tree.query("r/inputs/input[3]"), None);
        assert_eq!(tree.query("r/inputs/input[0]"), None);
        assert_eq!(tree.query("other/preview"), None);
        assert_eq!(tree.query("r/inputs/input[1]/@nothing"), None);
        assert_eq!(
            tree.to_xml(),
            "<r><preview>3</preview><inputs><input number=\"1\" title=\"A\">A</input>\
             <input number=\"2\">B &amp; C</input></inputs></r>"
        );
    }

    #[test]
    fn text_is_escaped() {
        assert_eq!(
            escape(r#"A&B <"c"> 'd'"#),
            "A&amp;B &lt;&quot;c&quot;&gt; &apos;d&apos;"
        );
        assert_eq!(escape("bell\u{7}"), "bell");
    }
}
