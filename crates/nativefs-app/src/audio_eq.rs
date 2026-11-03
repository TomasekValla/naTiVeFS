//! The web player's Parametric EQ (`audioPlayerClient/player.js`, PEQ2 style): 7 bands, shapes,
//! response curves, point dragging with a 0 dB magnet, wheel = width, presets, A/B bypass. The curves
//! use the same biquad formulas as the DSP chain, so what is drawn is what is heard.

use nativefs_audio::dsp::biquad::Coefs;
use nativefs_audio::dsp::{eq_defaults, eq_effective, Band, EqKind};

pub const F0: f64 = 20.0;
pub const F1: f64 = 20000.0;
/// The graph shows -18..+18 dB.
pub const DB: f64 = 18.0;
pub const POINTS: usize = 320;

pub const COLORS: [&str; 7] = ["#ff6fb1", "#ff9d5c", "#ffd36b", "#7dffa8", "#2fffc4", "#4fb2ff", "#a77bff"];

/// Shape menu order, names and icons (24 px viewBox, stroked).
pub const TYPES: [(EqKind, &str, &str); 6] = [
    (EqKind::Peaking, "Peak", "M2 13h5c2 0 2.5-8 5-8s3 8 5 8h5"),
    (EqKind::LowShelf, "Low shelf", "M2 6h5c3 0 4 7 7 7h8"),
    (EqKind::HighShelf, "High shelf", "M2 13h8c3 0 4-7 7-7h5"),
    (EqKind::HighPass, "Low cut", "M3 20c2-9 4-11 7-11h12"),
    (EqKind::LowPass, "High cut", "M2 9h12c3 0 5 2 7 11"),
    (EqKind::Notch, "Notch", "M2 7h7.5c1 0 1.5 12 2.5 12s1.5-12 2.5-12H22"),
];

pub fn icon(kind: EqKind) -> &'static str {
    TYPES.iter().find(|t| t.0 == kind).map_or("", |t| t.2)
}

pub const RANGES: [(f64, f64, &str); 7] =
    [(20.0, 60.0, "SUB"), (60.0, 250.0, "BASS"), (250.0, 500.0, "LOW MID"), (500.0, 2000.0, "MID"), (2000.0, 5000.0, "HIGH MID"), (5000.0, 10000.0, "PRESENCE"), (10000.0, 20000.0, "AIR")];

fn is_cut(k: EqKind) -> bool {
    matches!(k, EqKind::HighPass | EqKind::LowPass)
}

/// A preset band change: (band, kind, freq, gain, q); `None` keeps the default.
type Change = (usize, Option<EqKind>, Option<f64>, Option<f64>, Option<f64>);

pub struct Preset {
    pub name: &'static str,
    pub tip: &'static str,
    pub bands: &'static [Change],
}

const fn c(i: usize, k: Option<EqKind>, f: Option<f64>, g: Option<f64>, q: Option<f64>) -> Change {
    (i, k, f, g, q)
}
use EqKind::*;

pub const PRESETS: [Preset; 18] = [
    Preset { name: "Flat", tip: "No EQ at all.", bands: &[] },
    Preset { name: "Bass boost", tip: "More low end.", bands: &[c(0, None, Some(90.0), Some(6.0), None)] },
    Preset { name: "Sub boost", tip: "Only the deepest rumble, below the kick.", bands: &[c(0, Some(Peaking), Some(45.0), Some(7.0), Some(1.2))] },
    Preset { name: "Phonk bass", tip: "A fat, tight 808 hump.", bands: &[c(0, Some(Peaking), Some(55.0), Some(6.0), Some(1.4)), c(2, None, Some(300.0), Some(-2.0), Some(1.0))] },
    Preset { name: "Warm", tip: "Fuller lows, softer highs.", bands: &[c(0, None, Some(180.0), Some(3.0), None), c(6, None, Some(7000.0), Some(-3.5), None)] },
    Preset { name: "Bright", tip: "More shine in the highs.", bands: &[c(5, None, Some(4500.0), Some(2.5), None), c(6, None, Some(7500.0), Some(6.0), None)] },
    Preset { name: "Air", tip: "A light lift at the very top.", bands: &[c(6, None, Some(12000.0), Some(5.0), None)] },
    Preset { name: "V-shape", tip: "More bass and treble, fewer mids.", bands: &[c(0, None, Some(90.0), Some(5.0), None), c(3, None, None, Some(-3.5), Some(0.7)), c(6, None, Some(9000.0), Some(5.0), None)] },
    Preset { name: "Loudness", tip: "Fuller sound at low listening volume.", bands: &[c(0, None, Some(80.0), Some(6.0), None), c(3, None, None, Some(-1.5), Some(0.6)), c(6, None, Some(10000.0), Some(4.0), None)] },
    Preset { name: "De-mud", tip: "Clears up a boomy, muddy low-mid.", bands: &[c(2, None, Some(300.0), Some(-4.5), Some(1.2))] },
    Preset { name: "Scoop", tip: "Pulls the mids back.", bands: &[c(3, None, Some(600.0), Some(-5.0), Some(0.6))] },
    Preset { name: "Presence", tip: "Vocals and leads step forward.", bands: &[c(4, None, Some(3500.0), Some(4.0), Some(0.9))] },
    Preset { name: "Vocal", tip: "Less rumble, clearer voice.", bands: &[c(0, Some(HighPass), Some(90.0), None, Some(0.71)), c(2, None, Some(300.0), Some(-2.5), None), c(4, None, Some(2800.0), Some(4.0), Some(0.9))] },
    Preset { name: "Bass cut", tip: "Removes the low end (small speakers).", bands: &[c(0, Some(HighPass), Some(120.0), None, Some(0.71))] },
    Preset { name: "Treble cut", tip: "Tames harsh highs.", bands: &[c(6, Some(LowPass), Some(6000.0), None, Some(0.71))] },
    Preset { name: "Telephone", tip: "A narrow phone line.", bands: &[c(0, Some(HighPass), Some(450.0), None, Some(1.0)), c(3, None, Some(1500.0), Some(5.0), Some(0.8)), c(6, Some(LowPass), Some(3200.0), None, Some(1.3))] },
    Preset { name: "Radio", tip: "An old AM radio.", bands: &[c(0, Some(HighPass), Some(250.0), None, Some(0.9)), c(3, None, Some(1800.0), Some(3.0), Some(0.7)), c(6, Some(LowPass), Some(5000.0), None, Some(1.1))] },
    Preset { name: "Next room", tip: "The party is behind the wall.", bands: &[c(0, None, Some(100.0), Some(4.0), None), c(6, Some(LowPass), Some(700.0), None, Some(0.9))] },
];

pub const FEATURED: [&str; 5] = ["Flat", "Bass boost", "V-shape", "Vocal", "Bright"];
/// Expanded list lines as the web wraps them: (category, presets).
pub const LINES: [(&str, &[usize]); 5] = [("", &[0]), ("TONE", &[1, 2, 3, 4, 5, 6]), ("", &[7, 8]), ("FIX", &[9, 10, 11, 12, 13, 14]), ("FUN", &[15, 16, 17])];

pub fn preset_bands(i: usize) -> [Band; 7] {
    let mut bands = eq_defaults();
    for (b, k, f, g, q) in PRESETS[i].bands {
        let band = &mut bands[*b];
        if let Some(k) = k {
            band.kind = *k;
        }
        if let Some(f) = f {
            band.freq = *f;
        }
        if let Some(g) = g {
            band.gain = *g;
        }
        if let Some(q) = q {
            band.q = *q;
        }
    }
    bands
}

pub fn freq_at(k: usize) -> f64 {
    F0 * (F1 / F0).powf(k as f64 / (POINTS - 1) as f64)
}

/// Horizontal position 0..1 of a frequency in the plot (log axis).
pub fn x_of(f: f64) -> f64 {
    (f / F0).ln() / (F1 / F0).ln()
}

pub fn f_of(x: f64) -> f64 {
    F0 * (F1 / F0).powf(x.clamp(0.0, 1.0))
}

/// Vertical position 0..1 (top = +18 dB).
pub fn y_of(db: f64) -> f64 {
    (DB - db) / (2.0 * DB)
}

pub fn db_of(y: f64) -> f64 {
    DB - y * 2.0 * DB
}

/// Where the point sits: its gain, or the resonance for cuts, 0 for a notch.
pub fn band_height(b: &Band) -> f64 {
    if is_cut(b.kind) {
        20.0 * b.q.log10()
    } else if b.kind == Notch {
        0.0
    } else {
        b.gain
    }
}

const NOTE_NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

pub fn note_name(f: f64) -> String {
    let midi = 69.0 + 12.0 * (f / 440.0).log2();
    let n = midi.round() as i64;
    let cents = ((midi - n as f64) * 100.0).round() as i64;
    let name = NOTE_NAMES[n.rem_euclid(12) as usize];
    let oct = n.div_euclid(12) - 1;
    if cents == 0 {
        format!("{name}{oct}")
    } else {
        format!("{name}{oct}{}{cents}¢", if cents > 0 { "+" } else { "" })
    }
}

pub fn fmt_hz(f: f64) -> String {
    if f >= 1000.0 {
        if f >= 10000.0 { format!("{:.0}k", f / 1000.0) } else { format!("{:.1}k", f / 1000.0) }
    } else {
        format!("{} Hz", f.round())
    }
}

fn fmt_db(g: f64) -> String {
    let r = (g * 10.0).round() / 10.0 + 0.0;
    format!("{}{:.1} dB", if r > 0.0 { "+" } else { "" }, r)
}

/// Text of the value box over a dragged / hovered point.
pub fn readout(b: &Band) -> String {
    let mut parts = vec![format!("{} {}", fmt_hz(b.freq), note_name(b.freq))];
    if is_cut(b.kind) {
        parts.push(format!("res {}", fmt_db(20.0 * b.q.log10())));
    } else if b.kind != Notch {
        parts.push(fmt_db(b.gain));
    }
    if matches!(b.kind, Peaking | Notch) {
        parts.push(format!("Q {:.2}", b.q));
    }
    parts.join("  ")
}

pub struct Eq {
    pub bands: [Band; 7],
    pub bypass: bool,
    pub sel: usize,
    pub hover: Option<usize>,
    pub active_preset: Option<usize>,
    rate: f64,
    pub band_db: [Vec<f64>; 7],
    pub total_db: Vec<f64>,
    drag: Option<Drag>,
    /// Plot size in px (the paths are drawn in it).
    pub view: (f64, f64),
}

struct Drag {
    i: usize,
    x0: f64,
    y0: f64,
    f0: f64,
    h0: f64,
    fine: bool,
    moved: bool,
}

impl Eq {
    pub fn new(rate: f64) -> Eq {
        let mut e = Eq {
            bands: eq_defaults(),
            bypass: false,
            sel: 0,
            hover: None,
            active_preset: Some(0),
            rate,
            band_db: std::array::from_fn(|_| vec![0.0; POINTS]),
            total_db: vec![0.0; POINTS],
            drag: None,
            view: (600.0, 260.0),
        };
        e.compute();
        e
    }

    pub fn set_rate(&mut self, rate: f64) {
        self.rate = rate;
        self.compute();
    }

    /// Response of every band and the total, from the exact DSP coefficients.
    pub fn compute(&mut self) {
        self.total_db.iter_mut().for_each(|v| *v = 0.0);
        for (i, b) in self.bands.iter().enumerate() {
            let (kind, f, q, g) = eq_effective(b, self.bypass);
            let c = Coefs::new(kind, f, q, g, self.rate);
            for k in 0..POINTS {
                let db = c.response_db(freq_at(k), self.rate).max(-120.0);
                self.band_db[i][k] = db;
                self.total_db[k] += db;
            }
        }
    }

    /// Anything audible (the web's dot on the EQ button).
    pub fn active(&self) -> bool {
        let d = eq_defaults();
        !self.bypass && self.bands.iter().zip(d.iter()).any(|(b, d)| b.on && (b.kind != d.kind || b.gain.abs() > 0.05 || is_cut(b.kind) || b.kind == Notch))
    }

    /// The point under (px, py) in plot pixels; `w` / `h` = plot size in px.
    pub fn hit(&self, px: f64, py: f64, w: f64, h: f64) -> Option<usize> {
        let mut best = None;
        let mut bd = 24.0;
        for (i, b) in self.bands.iter().enumerate() {
            let d = ((x_of(b.freq) * w - px).powi(2) + (y_of(band_height(b).clamp(-DB, DB)) * h - py).powi(2)).sqrt();
            if d < bd {
                bd = d;
                best = Some(i);
            }
        }
        best
    }

    pub fn press(&mut self, i: usize, px: f64, py: f64, fine: bool) {
        self.sel = i;
        let b = &self.bands[i];
        self.drag = Some(Drag { i, x0: px, y0: py, f0: b.freq, h0: band_height(b), fine, moved: false });
    }

    /// Drag the pressed point to (px, py): frequency + gain (resonance for cuts), Shift = fine,
    /// magnet at 0 dB. Returns false when nothing is being dragged.
    pub fn drag_to(&mut self, px: f64, py: f64, w: f64, h: f64, fine: bool) -> bool {
        let Some(d) = &mut self.drag else { return false };
        let i = d.i;
        if !d.moved && ((px - d.x0).powi(2) + (py - d.y0).powi(2)).sqrt() > 3.0 {
            d.moved = true;
        }
        if fine != d.fine {
            d.x0 = px;
            d.y0 = py;
            d.f0 = self.bands[i].freq;
            d.h0 = band_height(&self.bands[i]);
            d.fine = fine;
        }
        let k = if d.fine { 0.2 } else { 1.0 };
        let (x0, y0, f0, h0) = (d.x0, d.y0, d.f0, d.h0);
        let b = &mut self.bands[i];
        b.freq = f_of(x_of(f0) + (px - x0) / w * k).clamp(F0, F1);
        let hgt = h0 + (db_of(py / h) - db_of(y0 / h)) * k;
        if is_cut(b.kind) {
            b.q = 10f64.powf(hgt / 20.0).clamp(0.5, 8.0);
        } else if b.kind != Notch {
            b.gain = hgt.clamp(-DB, DB);
            if b.gain.abs() < 0.35 && !fine {
                b.gain = 0.0;
            }
        }
        b.on = true;
        self.changed();
        true
    }

    pub fn release(&mut self) {
        self.drag = None;
    }

    pub fn dragging(&self) -> Option<usize> {
        self.drag.as_ref().map(|d| d.i)
    }

    /// Wheel over a point: width (shelves have a fixed slope).
    pub fn wheel_q(&mut self, i: usize, dy_px: f64, fine: bool) -> bool {
        let b = &mut self.bands[i];
        if matches!(b.kind, LowShelf | HighShelf) {
            return false;
        }
        let (lo, hi) = if is_cut(b.kind) { (0.5, 8.0) } else { (0.2, 12.0) };
        // Slint wheel: positive = up = narrower, like the web's negative deltaY
        b.q = (b.q * (dy_px * if fine { 0.0003 } else { 0.0015 }).exp()).clamp(lo, hi);
        self.sel = i;
        self.changed();
        true
    }

    pub fn set_type(&mut self, i: usize, kind: EqKind) {
        let b = &mut self.bands[i];
        if b.kind == kind {
            return;
        }
        b.kind = kind;
        b.on = true;
        if is_cut(kind) {
            b.q = 0.71;
        } else if kind == Notch {
            b.q = 2.0;
        } else if b.q < 0.3 {
            b.q = 1.0;
        }
        self.changed();
    }

    pub fn toggle_on(&mut self, i: usize) {
        self.bands[i].on = !self.bands[i].on;
        self.changed();
    }

    pub fn reset_band(&mut self, i: usize) {
        self.bands[i] = eq_defaults()[i];
        self.changed();
    }

    /// Fader value = the point's height (gain, or resonance for cuts).
    pub fn set_height(&mut self, i: usize, v: f64) {
        let b = &mut self.bands[i];
        if is_cut(b.kind) {
            b.q = 10f64.powf(v / 20.0).clamp(0.5, 8.0);
        } else if b.kind != Notch {
            b.gain = v.clamp(-DB, DB);
        }
        b.on = true;
        self.sel = i;
        self.changed();
    }

    pub fn set_freq(&mut self, i: usize, f: f64) {
        self.bands[i].freq = f.clamp(F0, F1);
        self.bands[i].on = true;
        self.sel = i;
        self.changed();
    }

    pub fn set_q(&mut self, i: usize, q: f64) {
        let b = &mut self.bands[i];
        b.q = if is_cut(b.kind) { q.clamp(0.5, 8.0) } else { q.clamp(0.2, 12.0) };
        b.on = true;
        self.sel = i;
        self.changed();
    }

    pub fn set_bypass(&mut self, on: bool) {
        self.bypass = on;
        self.compute();
    }

    fn changed(&mut self) {
        self.active_preset = None;
        self.compute();
    }
}

/// SVG path of a dB curve in plot pixels (`w` x `h`; x = log frequency, y = -18..+18 dB, clamped to
/// +-24). Slint keeps a viewbox's aspect ratio, so the curves are drawn in the plot's own size.
pub fn curve_path(db: &[f64], w: f64, h: f64) -> String {
    let mut s = String::with_capacity(POINTS * 14);
    for (k, v) in db.iter().enumerate() {
        let x = k as f64 / (POINTS - 1) as f64 * w;
        let y = y_of(v.clamp(-DB - 6.0, DB + 6.0)) * h;
        s.push_str(&format!("{}{x:.2} {y:.2} ", if k == 0 { "M" } else { "L" }));
    }
    s
}

/// The same curve closed down to the 0 dB line (the selected band's tinted area).
pub fn fill_path(db: &[f64], w: f64, h: f64) -> String {
    let mut s = curve_path(db, w, h);
    let zero = y_of(0.0) * h;
    s.push_str(&format!("L{w:.2} {zero:.2} L0 {zero:.2} Z"));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_like_the_web() {
        assert_eq!(note_name(440.0), "A4");
        assert_eq!(note_name(1000.0), "B5+21¢");
        assert_eq!(note_name(60.0), "B1-49¢");
        assert_eq!(note_name(32.703), "C1");
    }

    #[test]
    fn drag_with_magnet_and_response() {
        let mut e = Eq::new(48000.0);
        let (w, h) = (600.0, 300.0);
        let b = e.bands[3];
        let (px, py) = (x_of(b.freq) * w, y_of(0.0) * h);
        assert_eq!(e.hit(px + 5.0, py, w, h), Some(3));
        e.press(3, px, py, false);
        // 6 dB up, a little to the right
        e.drag_to(px + 30.0, py - 6.0 / 36.0 * h, w, h, false);
        assert!((e.bands[3].gain - 6.0).abs() < 1e-9);
        assert!(e.bands[3].freq > 1000.0);
        let peak = e.total_db.iter().cloned().fold(f64::MIN, f64::max);
        assert!((peak - 6.0).abs() < 0.1, "{peak}");
        // back near 0 dB snaps to exactly 0
        e.drag_to(px, py - 0.2 / 36.0 * h, w, h, false);
        assert_eq!(e.bands[3].gain, 0.0);
        assert!(!e.active());
    }

    #[test]
    fn presets_apply() {
        let b = preset_bands(15);
        assert_eq!(b[0].kind, HighPass);
        assert_eq!(b[6].kind, LowPass);
        assert_eq!(b[3].gain, 5.0);
        let mut all: Vec<usize> = LINES.iter().flat_map(|l| l.1.iter().copied()).collect();
        all.sort();
        assert_eq!(all, (0..PRESETS.len()).collect::<Vec<_>>());
    }
}

// ── what the Slint panel shows ──

use slint::{Color, Model as _, ModelRc, VecModel};

use crate::{AudioWindow, EqCol, EqCurve, EqMenuItem, EqPoint, EqTick, KnobData, PresetChip, PresetLine};

pub fn color(i: usize) -> Color {
    let v = u32::from_str_radix(COLORS[i].trim_start_matches('#'), 16).unwrap_or(0xffffff);
    Color::from_rgb_u8((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(v))
}

/// Update a model row by row instead of replacing it: a new model makes Slint rebuild every row,
/// and a fader or knob being dragged in a rebuilt row lost its press after the first pixel.
fn update<T: Clone + PartialEq + 'static>(current: ModelRc<T>, rows: Vec<T>, set: impl FnOnce(ModelRc<T>)) {
    if let Some(vm) = current.as_any().downcast_ref::<VecModel<T>>() {
        if slint::Model::row_count(vm) == rows.len() {
            for (i, r) in rows.into_iter().enumerate() {
                if slint::Model::row_data(vm, i).as_ref() != Some(&r) {
                    slint::Model::set_row_data(vm, i, r);
                }
            }
            return;
        }
    }
    set(model(rows));
}

/// Ruler, grid and axis labels (they only depend on the plot size, which Slint applies).
pub fn show_static(w: &AudioWindow) {
    let tick = |pos: f64, pos2: f64, label: String, focus: bool| EqTick { pos: pos as f32, pos2: pos2 as f32, label: label.into(), focus };
    let notes = (1..=9).map(|o| tick(x_of(32.703 * 2f64.powi(o - 1)), 0.0, format!("C{o}"), false)).collect();
    w.set_eq_notes(model(notes));
    w.set_eq_db(model([-12.0, -6.0, 0.0, 6.0, 12.0].iter().map(|d: &f64| tick(y_of(*d), 0.0, if *d > 0.0 { format!("+{d}") } else { format!("{d}") }, *d == 0.0)).collect()));
    w.set_eq_hz(model(
        [20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0]
            .iter()
            .map(|f: &f64| tick(x_of(*f), 0.0, if *f >= 1000.0 { format!("{}k", f / 1000.0) } else { format!("{f}") }, false))
            .collect(),
    ));
}

/// Mini knob values (0..1) of a band: Freq 20 Hz..20 kHz (log), BW Q 0.2..12 (log).
pub fn freq_norm(f: f64) -> f64 {
    x_of(f)
}
pub fn freq_from_norm(n: f64) -> f64 {
    f_of(n)
}
pub fn q_norm(q: f64) -> f64 {
    (q / 0.2).ln() / (60f64).ln()
}
pub fn q_from_norm(n: f64) -> f64 {
    0.2 * 60f64.powf(n.clamp(0.0, 1.0))
}

/// Everything that changes with the bands, the selection or the hover.
pub fn show(w: &AudioWindow, e: &Eq) {
    let focus = e.dragging().or(e.hover);
    let (vw, vh) = e.view;
    w.set_eq_vw(vw as f32);
    w.set_eq_vh(vh as f32);
    w.set_eq_total(curve_path(&e.total_db, vw, vh).into());
    update(w.get_eq_curves(), 
        (0..7)
            .map(|i| EqCurve {
                path: curve_path(&e.band_db[i], vw, vh).into(),
                fill: fill_path(&e.band_db[i], vw, vh).into(),
                color: color(i),
                strong: i == e.sel || Some(i) == focus,
                on: e.bands[i].on && !e.bypass,
            })
            .collect(),
        |m| w.set_eq_curves(m),
    );
    update(w.get_eq_points(), 
        (0..7)
            .map(|i| {
                let b = &e.bands[i];
                EqPoint {
                    x: x_of(b.freq) as f32,
                    y: y_of(band_height(b).clamp(-DB, DB)) as f32,
                    color: color(i),
                    label: (i + 1).to_string().into(),
                    selected: i == e.sel,
                    focus: Some(i) == focus,
                    dragging: e.dragging() == Some(i),
                    live: b.on && !e.bypass,
                }
            })
            .collect(),
        |m| w.set_eq_points(m),
    );
    let focus_f = focus.map(|i| e.bands[i].freq);
    update(
        w.get_eq_ranges(),
        RANGES.iter().map(|(a, b, l)| EqTick { pos: x_of(*a) as f32, pos2: x_of(*b) as f32, label: (*l).into(), focus: focus_f.is_some_and(|f| f >= *a && f < *b) }).collect(),
        |m| w.set_eq_ranges(m),
    );
    match focus {
        Some(i) => {
            let b = &e.bands[i];
            w.set_eq_readout(readout(b).into());
            w.set_eq_readout_x(x_of(b.freq) as f32);
            w.set_eq_readout_y(y_of(band_height(b).clamp(-DB, DB)) as f32);
            w.set_eq_readout_color(color(i));
        }
        None => w.set_eq_readout("".into()),
    }
    w.set_eq_hovering(e.hover.is_some() || e.dragging().is_some());
    update(w.get_eq_cols(), 
        (0..7)
            .map(|i| {
                let b = &e.bands[i];
                let h = band_height(b).clamp(-DB, DB);
                let shelf = matches!(b.kind, LowShelf | HighShelf);
                let d = eq_defaults()[i];
                let mini = |name: &str, n: f64, def_n: f64, value: String, changed: bool, idle: bool, tip: &str| KnobData {
                    name: name.into(),
                    color: color(i),
                    n: n as f32,
                    def_n: def_n as f32,
                    bipolar: false,
                    value: value.into(),
                    changed,
                    idle,
                    hot: false,
                    tip: tip.into(),
                };
                EqCol {
                    color: color(i),
                    icon: icon(b.kind).into(),
                    pos: y_of(h) as f32,
                    db: h as f32,
                    fader_idle: b.kind == Notch,
                    freq: mini("Freq", freq_norm(b.freq), freq_norm(d.freq), fmt_hz(b.freq), (b.freq - d.freq).abs() > 0.5, false, &format!("Band {} frequency: where in the spectrum it works.", i + 1)),
                    q: mini(
                        "BW",
                        q_norm(b.q),
                        q_norm(1.0),
                        format!("Q {:.2}", b.q),
                        (b.q - 1.0).abs() > 1e-3,
                        shelf,
                        if shelf {
                            "Shelves have a fixed slope, so BW does nothing here."
                        } else {
                            "Band width. Low = broad and gentle, high = narrow and surgical. Same as the wheel on its point."
                        },
                    ),
                    on: b.on && !e.bypass,
                    selected: i == e.sel,
                }
            })
            .collect(),
        |m| w.set_eq_cols(m),
    );
    w.set_eq_bypass(e.bypass);
    w.set_eq_active(e.active());
}

pub fn show_presets(w: &AudioWindow, active: Option<usize>) {
    let chip = |i: usize| PresetChip { name: PRESETS[i].name.into(), index: i as i32, active: active == Some(i), tip: PRESETS[i].tip.into() };
    w.set_eq_quick(model(FEATURED.iter().filter_map(|n| PRESETS.iter().position(|p| p.name == *n)).map(chip).collect()));
    w.set_eq_lines(model(LINES.iter().enumerate().map(|(li, (cat, idx))| PresetLine { cat: (*cat).into(), indent: li > 0, chips: model(idx.iter().map(|i| chip(*i)).collect()) }).collect()));
    // a chosen preset that is not in the quick row shows on the More button
    let hidden = active.filter(|i| !FEATURED.contains(&PRESETS[*i].name));
    w.set_eq_more_label(hidden.map_or("More".to_string(), |i| format!("More · {}", PRESETS[i].name)).into());
}

pub fn menu_items(e: &Eq, i: usize) -> Vec<EqMenuItem> {
    let mut v: Vec<EqMenuItem> = TYPES.iter().map(|(k, name, icon)| EqMenuItem { name: (*name).into(), icon: (*icon).into(), active: e.bands[i].kind == *k }).collect();
    v.push(EqMenuItem {
        name: if e.bands[i].on { "Turn band off" } else { "Turn band on" }.into(),
        icon: "M12 3.5v8M7 6.6a7 7 0 1 0 10 0".into(),
        active: false,
    });
    v
}

/// The web's live spectrum: analyser bins mapped onto the log axis (the loudest bin per point),
/// tilted +3 dB / octave so it reads flat-ish like PEQ2, fast attack, slower fall.
pub struct SpectrumView {
    pub points: Vec<f64>,
}

impl SpectrumView {
    pub fn new() -> Self {
        SpectrumView { points: vec![-100.0; POINTS] }
    }

    pub fn update(&mut self, db: &[f32], rate: f64, dt: f64) {
        let hz = rate / nativefs_audio::analysis::FFT_SIZE as f64;
        for k in 0..POINTS {
            let lo = ((freq_at(k) / hz).floor() as usize).max(1);
            let hi = (((if k + 1 < POINTS { freq_at(k + 1) } else { freq_at(k) }) / hz).floor() as usize).max(lo);
            let top = hi.min(db.len().saturating_sub(1));
            let m = db.get(lo..=top).map_or(-140.0, |s| s.iter().fold(-140.0f64, |m, v| m.max(*v as f64)));
            let v = m + 3.0 * (freq_at(k) / 1000.0).log2();
            let p = &mut self.points[k];
            *p += (v - *p) * if v > *p { 0.6 } else { (dt * 6.0).min(1.0) };
        }
    }

    /// Paused: everything sinks 40 dB / s. Returns false once it is all below the floor.
    pub fn fall(&mut self, dt: f64) -> bool {
        let mut any = false;
        for p in self.points.iter_mut() {
            *p -= dt * 40.0;
            any |= *p > -90.0;
        }
        any
    }

    /// Filled area on its own scale: -90..-10 dBFS over the plot height.
    pub fn path(&self, w: f64, h: f64) -> String {
        if self.points.iter().all(|p| *p <= -90.0) {
            return String::new();
        }
        let mut s = format!("M0 {h:.2} ");
        for (k, p) in self.points.iter().enumerate() {
            let x = k as f64 / (POINTS - 1) as f64 * w;
            let y = (1.0 - ((p + 90.0) / 80.0).clamp(0.0, 1.0)) * h;
            s.push_str(&format!("L{x:.2} {y:.2} "));
        }
        s.push_str(&format!("L{w:.2} {h:.2} Z"));
        s
    }
}
