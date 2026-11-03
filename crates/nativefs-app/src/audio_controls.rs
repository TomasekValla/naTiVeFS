//! The sound rack of the web player (`audioPlayerClient/player.js`): control definitions (ranges,
//! scales, detents, value text, typed input), groups, knobs that depend on another one, presets.
//! Kept 1:1 with the web so presets sound the same.

use nativefs_audio::dsp::Params;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Id {
    Filter,
    Volume,
    Tempo,
    Pitch,
    Nightcore,
    Reverb,
    Room,
    Echo,
    EchoTime,
    Feedback,
    Width,
    Rotate,
}

/// Rack order (groups left to right, top to bottom).
pub const ORDER: [Id; 12] = [Id::Filter, Id::Volume, Id::Tempo, Id::Pitch, Id::Nightcore, Id::Reverb, Id::Room, Id::Echo, Id::EchoTime, Id::Feedback, Id::Width, Id::Rotate];

/// Knob colours by group (names and descriptions are in `ui/audio.slint`).
pub struct Group {
    pub color: &'static str,
    pub ids: &'static [Id],
}

pub const GROUPS: [Group; 4] = [
    Group { color: "#ff8fd0", ids: &[Id::Filter, Id::Volume] },
    Group { color: "#2fffc4", ids: &[Id::Tempo, Id::Pitch, Id::Nightcore] },
    Group { color: "#b083ff", ids: &[Id::Reverb, Id::Room, Id::Echo, Id::EchoTime, Id::Feedback] },
    Group { color: "#6ea8ff", ids: &[Id::Width, Id::Rotate] },
];

pub struct Control {
    pub id: Id,
    pub name: &'static str,
    pub min: f64,
    pub max: f64,
    pub def: f64,
    pub log: bool,
    /// The arc fills from the default (top) instead of from the minimum.
    pub bipolar: bool,
    pub detents: Vec<f64>,
    /// The value turns "hot" (orange) above this (Volume boost).
    pub warn_above: Option<f64>,
    pub tip: &'static str,
}

/// Echo time detents follow the beat when the BPM is known (1/16, 1/8, dotted 1/8, 1/4).
const NOTE_DIVS: [(f64, &str); 4] = [(0.25, "1/16"), (0.5, "1/8"), (0.75, "1/8."), (1.0, "1/4")];

pub fn echo_detents(bpm: Option<f64>) -> Vec<f64> {
    match bpm.filter(|b| *b > 0.0) {
        Some(b) => NOTE_DIVS.iter().map(|d| 60000.0 / b * d.0).filter(|ms| (40.0..=1200.0).contains(ms)).collect(),
        None => Vec::new(),
    }
}

fn echo_note(ms: f64, bpm: Option<f64>) -> &'static str {
    let Some(b) = bpm.filter(|b| *b > 0.0) else { return "" };
    NOTE_DIVS.iter().find(|d| (60000.0 / b * d.0 - ms).abs() < 1.0).map_or("", |d| d.1)
}

/// The controls for a track with this BPM (Echo time default and detents depend on it).
pub fn controls(bpm: Option<f64>) -> Vec<Control> {
    let echo = echo_detents(bpm);
    let c = |id, name, min, max, def, tip| Control { id, name, min, max, def, log: false, bipolar: false, detents: Vec::new(), warn_above: None, tip };
    vec![
        Control { bipolar: true, ..c(Id::Filter, "Filter", -100.0, 100.0, 0.0, "DJ filter. Left muffles the highs, right thins out the bass.") },
        Control {
            detents: vec![100.0],
            warn_above: Some(100.0),
            ..c(Id::Volume, "Volume", 0.0, 200.0, 100.0, "Loudness. At 100% every track plays about as loud as on Spotify or YouTube (very loud masters are turned down). Above 100% (orange) is a warm, dirty boost, not remembered when you reopen the track.")
        },
        Control { log: true, bipolar: true, ..c(Id::Tempo, "Tempo", 0.5, 2.0, 1.0, "Playback speed. Whether the pitch moves with it is set by Nightcore.") },
        Control { bipolar: true, detents: vec![-12.0, 12.0], ..c(Id::Pitch, "Pitch", -12.0, 12.0, 0.0, "Pitch in semitones without changing the speed.") },
        c(Id::Nightcore, "Nightcore", 0.0, 100.0, 0.0, "How much the pitch follows Tempo. 0% = only the speed changes, 100% = faster is higher (nightcore), slower is deeper (slowed)."),
        c(Id::Reverb, "Reverb", 0.0, 100.0, 0.0, "How much room echo you hear."),
        c(Id::Room, "Room", 0.0, 100.0, 45.0, "Size of the room: small booth to huge hall."),
        c(Id::Echo, "Echo", 0.0, 100.0, 0.0, "How loud the repeats are."),
        Control {
            log: true,
            def: echo.get(1).copied().unwrap_or(375.0),
            detents: echo,
            ..c(Id::EchoTime, "Time", 40.0, 1200.0, 375.0, "Gap between repeats. Snaps to the beat when the BPM is known.")
        },
        c(Id::Feedback, "Repeats", 0.0, 90.0, 35.0, "How many times the echo repeats."),
        Control { detents: vec![100.0], ..c(Id::Width, "Width", 0.0, 200.0, 100.0, "0% is mono, 200% is extra wide.") },
        c(Id::Rotate, "8D", 0.0, 100.0, 0.0, "Sound circles around your head. Best with headphones."),
    ]
}

/// Knobs that only matter once another one is turned up: (knob, parent, hint).
pub const DEPENDS: [(Id, Id, &str); 4] = [
    (Id::Room, Id::Reverb, "Turn up Reverb first. Room sets the size of that reverb."),
    (Id::EchoTime, Id::Echo, "Turn up Echo first. Time sets the gap between its repeats."),
    (Id::Feedback, Id::Echo, "Turn up Echo first. Repeats sets how long it keeps echoing."),
    (Id::Nightcore, Id::Tempo, "Change Tempo first. Nightcore decides how much of that speed change also changes the pitch."),
];

impl Control {
    pub fn to_norm(&self, v: f64) -> f64 {
        if self.log {
            (v / self.min).ln() / (self.max / self.min).ln()
        } else {
            (v - self.min) / (self.max - self.min)
        }
    }

    pub fn value_at(&self, n: f64) -> f64 {
        let n = n.clamp(0.0, 1.0);
        if self.log {
            self.min * (self.max / self.min).powf(n)
        } else {
            self.min + n * (self.max - self.min)
        }
    }

    /// Norm positions the dial snaps to while dragging (detents + the default when it is not the minimum).
    pub fn detent_norms(&self) -> Vec<f64> {
        let mut d: Vec<f64> = self.detents.clone();
        if self.bipolar || self.def != self.min {
            d.push(self.def);
        }
        d.iter().map(|v| self.to_norm(*v)).collect()
    }

    /// The value text under the knob (the web's `fmt`).
    pub fn fmt(&self, v: f64, bpm: Option<f64>) -> String {
        let pct = |v: f64| format!("{}%", v.round());
        match self.id {
            Id::Filter => {
                if v.abs() < 0.5 {
                    "Off".into()
                } else {
                    let (lp, hp, _) = nativefs_audio::dsp::filter_cutoffs(v);
                    if v < 0.0 { format!("LP {}", fmt_hz(lp.unwrap_or(0.0))) } else { format!("HP {}", fmt_hz(hp)) }
                }
            }
            Id::Tempo => format!("{:.2}×", v),
            Id::Pitch => {
                // + 0.0 turns -0 into 0 (JavaScript prints "0.0" there too)
                let r = (v * 10.0).round() / 10.0 + 0.0;
                format!("{}{:.1} st", if r > 0.0 { "+" } else { "" }, r)
            }
            Id::EchoTime => {
                let note = echo_note(v, bpm);
                if note.is_empty() { format!("{} ms", v.round()) } else { format!("{} ms {note}", v.round()) }
            }
            _ => pct(v),
        }
    }

    /// Typed value (the web's `num` / `parse`): "120", "1,5", "75%", "8k", "LP 500 Hz".
    pub fn parse(&self, s: &str, current: f64) -> Option<f64> {
        let n = first_number(s)?;
        Some(match self.id {
            Id::Tempo if s.contains('%') => n / 100.0,
            Id::Filter if s.to_ascii_lowercase().contains('k') || s.to_ascii_lowercase().contains("hz") => {
                let low = s.to_ascii_lowercase();
                let hz = if low.contains('k') { n * 1000.0 } else { n };
                let hp = low.contains("hp") || low.contains("high") || (!low.contains("lp") && !low.contains("low") && current > 0.0);
                if hp {
                    100.0 * (hz / 20.0).ln() / (4000.0f64 / 20.0).ln()
                } else {
                    -100.0 * (hz / 12000.0).ln() / (160.0f64 / 12000.0).ln()
                }
            }
            _ => n,
        })
    }
}

fn fmt_hz(f: f64) -> String {
    if f >= 1000.0 {
        if f >= 10000.0 { format!("{:.0}k", f / 1000.0) } else { format!("{:.1}k", f / 1000.0) }
    } else {
        format!("{} Hz", f.round())
    }
}

fn first_number(s: &str) -> Option<f64> {
    let s = s.replace(',', ".");
    let start = s.find(|c: char| c.is_ascii_digit() || c == '-' || c == '.')?;
    let rest = &s[start..];
    let end = rest.char_indices().skip(1).find(|(_, c)| !(c.is_ascii_digit() || *c == '.')).map_or(rest.len(), |(i, _)| i);
    rest[..end].parse().ok()
}

/// A rack preset (the category rows of the expanded list are laid out in `audio_rack::LINES`).
pub struct Preset {
    pub name: &'static str,
    pub set: &'static [(Id, f64)],
    pub tip: &'static str,
}

/// Echo time "use the dotted eighth" in Dub: resolved with the BPM (the web's `echoDetents[2] || 450`).
pub const DUB_TIME: f64 = -1.0;

pub const PRESETS: [Preset; 21] = [
    Preset { name: "Original", set: &[], tip: "Everything back to how the track really sounds, EQ included." },
    Preset { name: "Nightcore", set: &[(Id::Tempo, 1.25), (Id::Nightcore, 100.0), (Id::Width, 115.0)], tip: "Faster and higher." },
    Preset { name: "Sped up", set: &[(Id::Tempo, 1.15), (Id::Nightcore, 100.0)], tip: "A little faster and higher, the short-video way." },
    Preset { name: "Slowed + Reverb", set: &[(Id::Tempo, 0.82), (Id::Nightcore, 100.0), (Id::Reverb, 38.0), (Id::Room, 72.0)], tip: "Slower, deeper, in a big room." },
    Preset { name: "Daycore", set: &[(Id::Tempo, 0.86), (Id::Nightcore, 100.0)], tip: "Slower and deeper, no reverb." },
    Preset { name: "Vaporwave", set: &[(Id::Tempo, 0.78), (Id::Nightcore, 100.0), (Id::Reverb, 30.0), (Id::Room, 70.0), (Id::Filter, -18.0), (Id::Width, 125.0)], tip: "Slow, dreamy and a bit faded." },
    Preset { name: "Half speed", set: &[(Id::Tempo, 0.5)], tip: "Half the speed, same pitch." },
    Preset { name: "Small room", set: &[(Id::Reverb, 28.0), (Id::Room, 12.0)], tip: "A tight, close room." },
    Preset { name: "Concert hall", set: &[(Id::Reverb, 45.0), (Id::Room, 85.0), (Id::Width, 130.0)], tip: "A big, wide hall." },
    Preset { name: "Dreamy", set: &[(Id::Reverb, 55.0), (Id::Room, 92.0), (Id::Echo, 18.0), (Id::Feedback, 40.0), (Id::Width, 140.0), (Id::Filter, -12.0)], tip: "Floating in a soft cloud." },
    Preset { name: "Echo", set: &[(Id::Echo, 34.0), (Id::Feedback, 45.0)], tip: "Repeats in time with the beat." },
    Preset { name: "Dub", set: &[(Id::Echo, 45.0), (Id::Feedback, 62.0), (Id::EchoTime, DUB_TIME), (Id::Reverb, 12.0), (Id::Filter, -10.0)], tip: "Long, dark repeats on the off-beat." },
    Preset { name: "8D", set: &[(Id::Rotate, 35.0), (Id::Width, 130.0), (Id::Reverb, 18.0), (Id::Room, 55.0)], tip: "The sound circles around your head. Use headphones." },
    Preset { name: "Spin", set: &[(Id::Rotate, 85.0), (Id::Width, 140.0)], tip: "Fast circling, close to dizzy." },
    Preset { name: "Chipmunk", set: &[(Id::Pitch, 7.0)], tip: "Much higher, same speed." },
    Preset { name: "Deep voice", set: &[(Id::Pitch, -5.0)], tip: "Deeper, same speed." },
    Preset { name: "Lo-fi", set: &[(Id::Filter, -42.0), (Id::Tempo, 0.94), (Id::Nightcore, 100.0), (Id::Width, 70.0), (Id::Reverb, 12.0)], tip: "Muffled and a bit slower, like an old tape." },
    Preset { name: "Radio", set: &[(Id::Filter, 38.0), (Id::Width, 0.0)], tip: "Thin and mono, like a small radio." },
    Preset { name: "Underwater", set: &[(Id::Filter, -72.0), (Id::Reverb, 30.0), (Id::Room, 60.0), (Id::Width, 60.0)], tip: "Heard from the bottom of a pool." },
    Preset { name: "Mono", set: &[(Id::Width, 0.0)], tip: "Both ears hear the same thing." },
    Preset { name: "Super wide", set: &[(Id::Width, 190.0)], tip: "Spread far beyond the speakers." },
];

/// The quick row of the web (`buildPresetRows(..., featured)`).
pub const FEATURED: [&str; 5] = ["Original", "Nightcore", "Slowed + Reverb", "8D", "Lo-fi"];

/// Values of preset `i` for every control except Volume (presets never touch the volume).
pub fn preset_values(i: usize, ctrls: &[Control], bpm: Option<f64>) -> Vec<(Id, f64)> {
    let p = &PRESETS[i];
    ctrls
        .iter()
        .filter(|c| c.id != Id::Volume)
        .map(|c| {
            let v = p.set.iter().find(|(id, _)| *id == c.id).map_or(c.def, |(_, v)| *v);
            let v = if v == DUB_TIME { echo_detents(bpm).get(2).copied().unwrap_or(450.0) } else { v };
            (c.id, v)
        })
        .collect()
}

/// Knob values -> DSP parameters (EQ and normalization are set elsewhere).
pub fn apply(values: &[(Id, f64)], p: &mut Params) {
    for (id, v) in values {
        let v = *v;
        match id {
            Id::Filter => p.filter = v,
            Id::Volume => p.volume = v,
            Id::Tempo => p.tempo = v,
            Id::Pitch => p.pitch = v,
            Id::Nightcore => p.nightcore = v,
            Id::Reverb => p.reverb = v,
            Id::Room => p.room = v,
            Id::Echo => p.echo = v,
            Id::EchoTime => p.echo_time = v,
            Id::Feedback => p.feedback = v,
            Id::Width => p.width = v,
            Id::Rotate => p.rotate = v,
        }
    }
}

/// The parent of a dependent knob is "active" (the web's `parentActive`).
pub fn parent_active(parent: Id, value: f64) -> bool {
    if parent == Id::Tempo { (value - 1.0).abs() > 0.004 } else { value > 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctrl(id: Id) -> Control {
        controls(Some(123.0)).into_iter().find(|c| c.id == id).unwrap()
    }

    #[test]
    fn value_texts_match_the_web() {
        assert_eq!(ctrl(Id::Tempo).fmt(1.0, None), "1.00×");
        assert_eq!(ctrl(Id::Pitch).fmt(7.0, None), "+7.0 st");
        assert_eq!(ctrl(Id::Pitch).fmt(-0.04, None), "0.0 st");
        assert_eq!(ctrl(Id::Filter).fmt(0.2, None), "Off");
        assert_eq!(ctrl(Id::Filter).fmt(-100.0, None), "LP 160 Hz");
        assert_eq!(ctrl(Id::Filter).fmt(100.0, None), "HP 4.0k");
        assert_eq!(ctrl(Id::Volume).fmt(150.4, None), "150%");
        let t = ctrl(Id::EchoTime);
        assert_eq!(t.fmt(t.def, Some(123.0)), "244 ms 1/8");
    }

    #[test]
    fn norms_and_typed_values() {
        let t = ctrl(Id::Tempo);
        assert!((t.to_norm(1.0) - 0.5).abs() < 1e-12, "1.00x sits in the middle of the log scale");
        assert_eq!(t.parse("125%", 1.0), Some(1.25));
        assert_eq!(ctrl(Id::Pitch).parse("+3,5 st", 0.0), Some(3.5));
        let f = ctrl(Id::Filter);
        let v = f.parse("LP 160 Hz", 0.0).unwrap();
        assert!((v + 100.0).abs() < 1e-9);
        let v = f.parse("4k", 50.0).unwrap();
        assert!((v - 100.0).abs() < 1e-9, "{v}");
    }

    #[test]
    fn presets_resolve() {
        let c = controls(Some(120.0));
        let dub = preset_values(11, &c, Some(120.0));
        assert_eq!(dub.iter().find(|(id, _)| *id == Id::EchoTime).unwrap().1, 375.0);
        assert!(preset_values(0, &c, None).iter().all(|(id, v)| *v == c.iter().find(|x| x.id == *id).unwrap().def));
        assert!(!preset_values(1, &c, None).iter().any(|(id, _)| *id == Id::Volume));
    }
}
