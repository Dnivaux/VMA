//! The state as the compat API's XML (`docs/specs/api-compat.md` § 3): the elements of the
//! public documentation, and those the Companion module needs or it drops the document.
//! Values still to be checked against the reference's own output (Q3) are marked « Q3 ».

use std::fmt::Write as _;

use vma_core::AudioBus;
use vma_engine::{Effect, InputSpec, InputStatus, OutputSpec, State, TransitionSetting};

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

/// The XML document of `state`.
pub fn state_xml(state: &State) -> String {
    let mut x = String::with_capacity(1024 + 512 * state.inputs.len());
    let number = |id| {
        state
            .inputs
            .iter()
            .position(|i| Some(i.id) == id)
            .map_or(0, |n| n + 1)
    };
    // `write!` into a String cannot fail.
    let _ = write!(
        x,
        "<{ROOT}><version>{VERSION}</version><edition>{EDITION}</edition>"
    );
    x.push_str("<inputs>");
    for (n, input) in state.inputs.iter().enumerate() {
        let audio = input.audio;
        let playing = input.playback.is_some_and(|p| p.playing)
            || matches!(input.status, InputStatus::Live { .. }) && input.playback.is_none();
        let buses: Vec<&str> = AudioBus::ALL
            .into_iter()
            .filter(|b| audio.is_some_and(|a| a.sends.get(b.index()) == Some(&true)))
            .map(bus_letter)
            .collect();
        let title = escape(&input.name);
        let _ = write!(
            x,
            "<input key=\"{key}\" number=\"{number}\" type=\"{kind}\" title=\"{title}\" \
             shortTitle=\"{title}\" state=\"{state}\" position=\"0\" duration=\"0\" \
             loop=\"{looping}\" muted=\"{muted}\" volume=\"{volume:.0}\" balance=\"0\" \
             solo=\"{solo}\" audiobusses=\"{buses}\" meterF1=\"0\" meterF2=\"0\" \
             gainDb=\"0\" selectedIndex=\"1\">{title}</input>",
            key = input.key,
            number = n + 1,
            kind = kind(&input.spec),
            state = if playing { "Running" } else { "Paused" },
            looping = yes(input.playback.is_some_and(|p| p.looping)),
            muted = yes(audio.is_some_and(|a| a.mute)),
            volume = audio.map_or(100.0, |a| db_to_volume(a.gain)),
            solo = yes(audio.is_some_and(|a| a.solo)),
            buses = buses.join(","),
        );
    }
    x.push_str("</inputs><overlays>");
    for (n, overlay) in state.overlays.iter().enumerate() {
        let shown = overlay
            .input
            .filter(|_| overlay.on)
            .map(|i| number(Some(i)));
        match shown {
            Some(input) => {
                let _ = write!(x, "<overlay number=\"{}\">{input}</overlay>", n + 1);
            }
            None => {
                let _ = write!(x, "<overlay number=\"{}\"/>", n + 1);
            }
        }
    }
    let _ = write!(
        x,
        "</overlays><preview>{}</preview><active>{}</active><fadeToBlack>{}</fadeToBlack>",
        number(state.preview),
        number(state.program),
        yes(state.ftb.active)
    );
    x.push_str("<transitions>");
    for (n, button) in state.buttons.iter().enumerate() {
        let _ = write!(
            x,
            "<transition number=\"{}\" effect=\"{}\" duration=\"{}\"/>",
            n + 1,
            effect(*button),
            button.duration_ms
        );
    }
    x.push_str("</transitions>");
    let on =
        |f: fn(&OutputSpec) -> bool| state.outputs.iter().any(|o| f(&o.spec) && o.status.is_on());
    let _ = write!(
        x,
        "<recording>{}</recording><external>False</external><streaming>{}</streaming>\
         <playList>False</playList><multiCorder>False</multiCorder>\
         <fullscreen>{}</fullscreen>",
        yes(on(|s| matches!(s, OutputSpec::Record(_)))),
        yes(on(|s| matches!(
            s,
            OutputSpec::Rtmp(_) | OutputSpec::Srt(_)
        ))),
        yes(on(OutputSpec::is_fullscreen)),
    );
    x.push_str("<audio>");
    for bus in AudioBus::ALL {
        let set = state
            .audio
            .buses
            .get(bus.index())
            .copied()
            .unwrap_or_default();
        let _ = write!(
            x,
            "<{name} volume=\"{volume:.0}\" muted=\"{muted}\" meterF1=\"0\" meterF2=\"0\"",
            name = bus_element(bus),
            volume = db_to_volume(set.gain),
            muted = yes(set.mute),
        );
        if bus == AudioBus::Master {
            x.push_str(" headphonesVolume=\"100\"");
        } else {
            x.push_str(" solo=\"False\" sendToMaster=\"True\"");
        }
        x.push_str("/>");
    }
    let _ = write!(x, "</audio></{ROOT}>");
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_escaped() {
        assert_eq!(
            escape(r#"A&B <"c"> 'd'"#),
            "A&amp;B &lt;&quot;c&quot;&gt; &apos;d&apos;"
        );
        assert_eq!(escape("bell\u{7}"), "bell");
    }
}
