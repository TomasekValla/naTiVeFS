//! The sound rack's behaviour (the web's `Knob` and preset code): knob values, magnetic detents,
//! dependent ("idle") knobs, value texts and tips for the Slint model, presets that glide the knobs
//! over 520 ms so the sound sweeps with them.

use slint::{Color, Model, ModelRc, VecModel};

use crate::audio_controls::{self as ac, Control, Id, ORDER, PRESETS};
use crate::{KnobData, PresetChip, PresetLine};

/// Preset lines of the expanded list, as the web wraps them in its 526 px column.
const LINES: [(&str, &[usize]); 6] =
    [("", &[0]), ("SPEED", &[1, 2, 3, 4, 5]), ("", &[6]), ("SPACE", &[7, 8, 9, 10, 11]), ("FUN", &[12, 13, 14, 15, 16, 17]), ("", &[18, 19, 20])];

pub struct Rack {
    pub controls: Vec<Control>,
    pub values: [f64; 12],
    pub bpm: Option<f64>,
    pub active_preset: Option<usize>,
    knobs: std::rc::Rc<VecModel<KnobData>>,
    /// Bit-perfect mode on a device with a hardware volume: the Volume knob shows and sets the
    /// device's own volume (0..1) instead of the app's, so the samples stay untouched.
    pub hw_volume: Option<f64>,
}

fn color_of(id: Id) -> Color {
    let hex = ac::GROUPS.iter().find(|g| g.ids.contains(&id)).map_or("#2fffc4", |g| g.color);
    let v = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0x2fffc4);
    Color::from_rgb_u8((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

impl Rack {
    pub fn new(bpm: Option<f64>, volume: f64) -> Rack {
        let controls = ac::controls(bpm);
        let mut values = [0.0; 12];
        for (i, id) in ORDER.iter().enumerate() {
            values[i] = controls.iter().find(|c| c.id == *id).map_or(0.0, |c| c.def);
        }
        values[1] = volume.clamp(0.0, 100.0);
        let mut r = Rack { controls, values, bpm, active_preset: Some(0), knobs: std::rc::Rc::new(VecModel::default()), hw_volume: None };
        let rows: Vec<KnobData> = (0..12).map(|i| r.knob_data(i)).collect();
        r.knobs = std::rc::Rc::new(VecModel::from(rows));
        r
    }

    pub fn model(&self) -> ModelRc<KnobData> {
        ModelRc::from(self.knobs.clone())
    }

    fn control(&self, i: usize) -> &Control {
        self.controls.iter().find(|c| c.id == ORDER[i]).expect("every id has a control")
    }

    fn value_of(&self, id: Id) -> f64 {
        self.values[ORDER.iter().position(|x| *x == id).unwrap_or(0)]
    }

    fn idle(&self, i: usize) -> Option<&'static str> {
        ac::DEPENDS.iter().find(|(k, _, _)| *k == ORDER[i]).filter(|(_, parent, _)| !ac::parent_active(*parent, self.value_of(*parent))).map(|(_, _, hint)| *hint)
    }

    fn knob_data(&self, i: usize) -> KnobData {
        if let (1, Some(v)) = (i, self.hw_volume) {
            return KnobData {
                name: "Volume".into(),
                color: color_of(Id::Volume),
                n: v as f32,
                def_n: v as f32,
                bipolar: false,
                value: format!("Device {}%", (v * 100.0).round()).into(),
                changed: false,
                idle: false,
                hot: false,
                tip: "Bit-perfect mode: this is the device's own hardware volume, so the samples reach it untouched.".into(),
            };
        }
        let c = self.control(i);
        let v = self.values[i];
        let idle = self.idle(i);
        KnobData {
            name: c.name.into(),
            color: color_of(c.id),
            n: c.to_norm(v) as f32,
            def_n: c.to_norm(c.def) as f32,
            bipolar: c.bipolar,
            value: c.fmt(v, self.bpm).into(),
            changed: (v - c.def).abs() > 1e-6,
            idle: idle.is_some(),
            hot: c.warn_above.is_some_and(|w| v > w),
            tip: idle.unwrap_or(c.tip).into(),
        }
    }

    /// Refresh the rows that may have changed (a knob and the ones depending on it).
    fn refresh(&self) {
        for i in 0..12 {
            let d = self.knob_data(i);
            if self.knobs.row_data(i).as_ref() != Some(&d) {
                self.knobs.set_row_data(i, d);
            }
        }
    }

    /// A hand moved knob `i` to the raw position `n` (may be outside 0..1): detents apply.
    pub fn drag(&mut self, i: usize, n: f64) {
        let c = self.control(i);
        let mut n = n.clamp(0.0, 1.0);
        // magnetic detents: a small dead zone around default / centre / beat values
        if let Some(d) = c.detent_norms().into_iter().rfind(|d| (n - d).abs() < 0.018) {
            n = d;
        }
        self.set_norm(i, n, true);
    }

    pub fn set_norm(&mut self, i: usize, n: f64, from_user: bool) {
        let c = self.control(i);
        let mut v = c.value_at(n);
        if !c.log {
            v = (v * 100.0).round() / 100.0;
        }
        self.values[i] = v;
        if from_user {
            self.active_preset = None;
        }
        self.refresh();
    }

    pub fn set_value(&mut self, i: usize, v: f64, from_user: bool) {
        let c = self.control(i);
        let n = c.to_norm(v.clamp(c.min, c.max));
        self.set_norm(i, n, from_user);
    }

    pub fn reset(&mut self, i: usize) {
        let def = self.control(i).def;
        self.set_value(i, def, true);
    }

    /// Bit-perfect mode switched, or the device volume read back (`None` = the app's Volume again).
    pub fn set_hw_volume(&mut self, v: Option<f64>) {
        self.hw_volume = v;
        self.refresh();
    }

    /// Mouse wheel over a knob (Slint: positive = up = more), the web's 0.0011 per pixel.
    pub fn wheel(&mut self, i: usize, delta_px: f64) {
        let n = self.control(i).to_norm(self.values[i]) + delta_px * 0.0011;
        self.set_norm(i, n.clamp(0.0, 1.0), true);
    }

    /// A typed value; ignored when it is not a number.
    pub fn edit(&mut self, i: usize, text: &str) {
        let c = self.control(i);
        if let Some(v) = c.parse(text, self.values[i]) {
            self.set_value(i, v, true);
        } else {
            self.refresh();
        }
    }

    /// Norm positions of preset `p` for every knob except Volume.
    pub fn preset_targets(&self, p: usize) -> Vec<(usize, f64, f64)> {
        let vals = ac::preset_values(p, &self.controls, self.bpm);
        vals.iter()
            .map(|(id, v)| {
                let i = ORDER.iter().position(|x| x == id).unwrap_or(0);
                let c = self.control(i);
                (i, c.to_norm(self.values[i]), c.to_norm(*v))
            })
            .collect()
    }

    pub fn params(&self, p: &mut nativefs_audio::dsp::Params) {
        let vals: Vec<(Id, f64)> = ORDER.iter().copied().zip(self.values.iter().copied()).collect();
        ac::apply(&vals, p);
    }

    pub fn volume(&self) -> f64 {
        self.values[1]
    }

    pub fn quick_chips(&self) -> Vec<PresetChip> {
        ac::FEATURED
            .iter()
            .filter_map(|name| PRESETS.iter().position(|p| p.name == *name))
            .map(|i| PresetChip { name: PRESETS[i].name.into(), index: i as i32, active: self.active_preset == Some(i), tip: PRESETS[i].tip.into() })
            .collect()
    }

    pub fn lines(&self) -> Vec<PresetLine> {
        LINES
            .iter()
            .enumerate()
            .map(|(li, (cat, idx))| PresetLine {
                cat: (*cat).into(),
                indent: li > 0,
                chips: ModelRc::new(VecModel::from(
                    idx.iter()
                        .map(|i| PresetChip { name: PRESETS[*i].name.into(), index: *i as i32, active: self.active_preset == Some(*i), tip: PRESETS[*i].tip.into() })
                        .collect::<Vec<_>>(),
                )),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detents_snap_and_idle_follows_the_parent() {
        let mut r = Rack::new(Some(123.0), 100.0);
        // Room depends on Reverb
        assert!(r.knob_data(6).idle);
        r.set_value(5, 30.0, true);
        assert!(!r.knob_data(6).idle);
        // Tempo snaps to 1.00x near the middle
        r.drag(2, 0.51);
        assert_eq!(r.values[2], 1.0);
        r.drag(2, 0.6);
        assert!(r.values[2] > 1.0);
        assert_eq!(r.active_preset, None);
    }

    #[test]
    fn preset_lines_cover_every_preset_once() {
        let mut all: Vec<usize> = LINES.iter().flat_map(|(_, v)| v.iter().copied()).collect();
        all.sort();
        assert_eq!(all, (0..PRESETS.len()).collect::<Vec<_>>());
    }
}

#[cfg(test)]
mod volume_tests {
    use super::*;
    use nativefs_audio::dsp::{Chain, Params};

    fn level(out: &[i32]) -> f64 {
        let tail = &out[out.len() / 2..];
        (tail.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / tail.len() as f64).sqrt()
    }

    /// The whole path a Volume drag takes: knob -> Params -> chain.
    #[test]
    fn volume_knob_turns_the_level_down() {
        let input: Vec<i32> = (0..88200).map(|i| ((i as f64 * 0.03).sin() * 1e9) as i32).collect();
        let mut rack = Rack::new(None, 100.0);
        let mut levels = Vec::new();
        for n in [0.5, 0.4, 0.25] {
            rack.drag(1, n);
            let mut p = Params::default();
            rack.params(&mut p);
            let mut c = Chain::new(44100);
            c.set(&p);
            let mut out = Vec::new();
            c.process(&input, &mut out, 32);
            levels.push((rack.values[1], p.volume, level(&out) / level(&input)));
        }
        println!("{levels:?}");
        assert_eq!(levels[0].0, 100.0);
        assert!((levels[1].2 - 0.64).abs() < 0.02, "{levels:?}");
        assert!((levels[2].2 - 0.25).abs() < 0.02, "{levels:?}");
    }
}
