//! Width + 8D, ported from the web player's `tvfs-stereo` worklet: both are computed on the full
//! signal, but only their difference from the input is added back, high-passed at 150 Hz, so the
//! bass stays centred and untouched (no phase shift that would grow peaks out of clipped masters).
//! Width 100 % with 8D off is the input sample for sample.

use std::f64::consts::{FRAC_PI_4, PI, SQRT_2};

/// The worklet's 2nd-order Butterworth high-pass (bilinear, Q = 1/sqrt 2).
#[derive(Clone, Copy)]
struct HighPass {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl HighPass {
    fn new(f: f64, rate: f64) -> Self {
        let w = 2.0 * PI * f / rate;
        let (sin, cw) = w.sin_cos();
        let al = sin / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + al;
        let b0 = (1.0 + cw) / 2.0 / a0;
        HighPass { b0, b1: -(1.0 + cw) / a0, b2: b0, a1: -2.0 * cw / a0, a2: (1.0 - al) / a0, x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0 }
    }

    #[inline]
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2 - self.a1 * self.y1 - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

const BASS_FREQ: f64 = 150.0;

pub struct Stereo {
    rate: f64,
    hp: [[HighPass; 2]; 2],
    phase: f64,
    width: f64,
    depth: f64,
    dark_l: f64,
    dark_r: f64,
    /// Targets (the worklet's k-rate parameters).
    pub target_width: f64,
    pub target_depth: f64,
    pub rot_rate: f64,
}

impl Stereo {
    pub fn new(rate: f64) -> Self {
        let hp = HighPass::new(BASS_FREQ, rate);
        Stereo { rate, hp: [[hp; 2]; 2], phase: 0.0, width: 1.0, depth: 0.0, dark_l: 0.0, dark_r: 0.0, target_width: 1.0, target_depth: 0.0, rot_rate: 0.1 }
    }

    /// The controls: width 0-200 %, 8D 0-100 % (as in the web's `E.set`).
    pub fn set(&mut self, width_pct: f64, rotate_pct: f64) {
        self.target_width = width_pct / 100.0;
        self.target_depth = (rotate_pct / 30.0).min(1.0);
        self.rot_rate = 0.05 + 0.3 * (rotate_pct / 100.0).powf(1.5);
    }

    /// Neutral and settled: the output equals the input.
    pub fn idle(&self) -> bool {
        self.target_width == 1.0 && self.target_depth == 0.0 && (self.width - 1.0).abs() < 1e-6 && self.depth < 1e-6
    }

    /// Interleaved stereo in place.
    pub fn process(&mut self, buf: &mut [f64]) {
        let d_phase = 2.0 * PI * self.rot_rate / self.rate;
        let dark_a = 1.0 - (-2.0 * PI * 3500.0 / self.rate).exp();
        let (tw, td) = (self.target_width, self.target_depth);
        let [hp_l, hp_r] = &mut self.hp;
        for fr in buf.as_chunks_mut::<2>().0 {
            let (l, r) = (fr[0], fr[1]);
            self.width += (tw - self.width) * 0.002;
            self.depth += (td - self.depth) * 0.001;
            let m = (l + r) / 2.0;
            let sd = (l - r) / 2.0 * self.width;
            self.phase += d_phase;
            if self.phase > 2.0 * PI {
                self.phase -= 2.0 * PI;
            }
            let pos = self.phase.sin() * self.depth;
            let back = (-self.phase.cos()).max(0.0) * self.depth;
            let a = (pos + 1.0) * FRAC_PI_4;
            let mut ml = m * a.cos() * SQRT_2;
            let mut mr = m * a.sin() * SQRT_2;
            self.dark_l += dark_a * (ml - self.dark_l);
            self.dark_r += dark_a * (mr - self.dark_r);
            ml += (self.dark_l - ml) * back * 0.55;
            mr += (self.dark_r - mr) * back * 0.55;
            let tl = hp_l[0].run(ml + sd - l);
            let tr = hp_r[0].run(mr - sd - r);
            fr[0] = l + hp_l[1].run(tl);
            fr[1] = r + hp_r[1].run(tr);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_is_transparent() {
        let mut s = Stereo::new(44100.0);
        let mut buf: Vec<f64> = (0..20000).map(|i| ((i * 37 % 200) as f64 / 100.0 - 1.0) * 0.5).collect();
        let orig = buf.clone();
        s.process(&mut buf);
        let err = buf.iter().zip(&orig).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
        assert!(err < 1e-12, "{err}");
    }

    #[test]
    fn mono_at_zero_width() {
        let mut s = Stereo::new(44100.0);
        s.set(0.0, 0.0);
        // 5 kHz, opposite polarity in L/R: the side signal (nearly) disappears once the width has
        // settled. Like the web worklet it is not perfect: the 150 Hz high-pass still turns the phase
        // a little (at 1 kHz only about -7.5 dB of the side goes away).
        let mut buf = Vec::new();
        for i in 0..44100 {
            let v = (2.0 * PI * 5000.0 * i as f64 / 44100.0).sin() * 0.5;
            buf.extend_from_slice(&[v, -v]);
        }
        s.process(&mut buf);
        let tail = &buf[buf.len() - 2000..];
        let peak = tail.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        assert!(peak < 0.06, "{peak}");
    }
}
