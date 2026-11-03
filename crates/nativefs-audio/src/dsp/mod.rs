//! The sound controls of the TVFS web player (`audioPlayerClient/player.js`, `createEngine`), ported:
//! tempo / pitch / Nightcore, 7-band EQ, DJ filter, width + 8D, reverb, echo, loudness normalization,
//! volume with a limiter and the > 100 % saturating boost. Same order and maths as the web graph,
//! processed in f64. With every control neutral the chain is skipped entirely, so the samples stay
//! bit-identical (the web graph is only "transparent to -69 dB").

pub mod biquad;
pub mod rate;
pub mod space;
pub mod stereo;

use biquad::{Coefs, Kind, State};

/// The web's `setTargetAtTime`: exponential approach with time constant `tc` (0 = jump).
#[derive(Debug, Clone, Copy)]
pub struct Glide {
    value: f64,
    target: f64,
    coef: f64,
}

impl Glide {
    pub fn new(value: f64, tc: f64, rate: f64) -> Self {
        let coef = if tc <= 0.0 { 1.0 } else { 1.0 - (-1.0 / (tc * rate)).exp() };
        Glide { value, target: value, coef }
    }

    pub fn set(&mut self, target: f64) {
        self.target = target;
    }

    pub fn jump(&mut self) {
        self.value = self.target;
    }

    pub fn value(&self) -> f64 {
        self.value
    }

    pub fn settled(&self) -> bool {
        self.value == self.target
    }

    #[inline]
    pub fn step(&mut self) -> f64 {
        if self.value != self.target {
            self.value += (self.target - self.value) * self.coef;
            if (self.target - self.value).abs() < 1e-9 {
                self.value = self.target;
            }
        }
        self.value
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EqKind {
    Peaking,
    LowShelf,
    HighShelf,
    /// "Low cut" (high-pass)
    HighPass,
    /// "High cut" (low-pass)
    LowPass,
    Notch,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    pub kind: EqKind,
    pub freq: f64,
    pub gain: f64,
    pub q: f64,
    pub on: bool,
}

/// The web's `EQ_DEFAULTS`: low shelf 60, peaks 150 / 400 / 1k / 2.5k / 6k, high shelf 12k.
pub fn eq_defaults() -> [Band; 7] {
    let b = |kind, freq| Band { kind, freq, gain: 0.0, q: 1.0, on: true };
    [
        b(EqKind::LowShelf, 60.0),
        b(EqKind::Peaking, 150.0),
        b(EqKind::Peaking, 400.0),
        b(EqKind::Peaking, 1000.0),
        b(EqKind::Peaking, 2500.0),
        b(EqKind::Peaking, 6000.0),
        b(EqKind::HighShelf, 12000.0),
    ]
}

/// What the engine gets for a band (the web's `eqEffective`): off / bypassed bands become an exact
/// identity; low / high cuts take their Q as resonance in dB.
pub fn eq_effective(b: &Band, bypass: bool) -> (Kind, f64, f64, f64) {
    if bypass || !b.on {
        return (Kind::Peaking, b.freq, 1.0, 0.0);
    }
    match b.kind {
        EqKind::Peaking => (Kind::Peaking, b.freq, b.q, b.gain),
        EqKind::LowShelf => (Kind::LowShelf, b.freq, b.q, b.gain),
        EqKind::HighShelf => (Kind::HighShelf, b.freq, b.q, b.gain),
        EqKind::HighPass => (Kind::HighPass, b.freq, 20.0 * b.q.log10(), 0.0),
        EqKind::LowPass => (Kind::LowPass, b.freq, 20.0 * b.q.log10(), 0.0),
        EqKind::Notch => (Kind::Notch, b.freq, b.q, 0.0),
    }
}

/// Control values exactly as the web knobs show them.
#[derive(Debug, Clone, PartialEq)]
pub struct Params {
    /// 0-200 %, > 100 = boost with saturation
    pub volume: f64,
    /// -100 (low-pass) .. 100 (high-pass)
    pub filter: f64,
    /// 0.5-2x
    pub tempo: f64,
    /// semitones -12..12
    pub pitch: f64,
    /// 0-100 %: how much of the tempo change reaches the pitch
    pub nightcore: f64,
    pub reverb: f64,
    pub room: f64,
    pub echo: f64,
    pub echo_time: f64,
    pub feedback: f64,
    pub width: f64,
    pub rotate: f64,
    pub eq: [Band; 7],
    pub eq_bypass: bool,
    /// Loudness normalization in dB (0 or negative).
    pub norm_db: f64,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            volume: 100.0,
            filter: 0.0,
            tempo: 1.0,
            pitch: 0.0,
            nightcore: 0.0,
            reverb: 0.0,
            room: 45.0,
            echo: 0.0,
            echo_time: 375.0,
            feedback: 35.0,
            width: 100.0,
            rotate: 0.0,
            eq: eq_defaults(),
            eq_bypass: false,
            norm_db: 0.0,
        }
    }
}

impl Params {
    /// The web's `applyRates`: tempo T, pitch factor P = T^(nightcore) * 2^(st/12).
    pub fn rates(&self) -> (f64, f64) {
        let t = self.tempo;
        let p = t.powf(self.nightcore / 100.0) * 2f64.powf(self.pitch / 12.0);
        // the web treats < 6e-4 in log as "no shift"
        let p = if (p / t).ln().abs() < 6e-4 { t } else { p };
        (t, if (p.ln()).abs() < 6e-4 { 1.0 } else { p })
    }

    /// Every control neutral: the chain would not change a single sample.
    pub fn neutral(&self) -> bool {
        let (t, p) = self.rates();
        t == 1.0
            && p == 1.0
            && self.volume == 100.0
            && self.filter == 0.0
            && self.reverb == 0.0
            && self.echo == 0.0
            && self.width == 100.0
            && self.rotate == 0.0
            && self.norm_db == 0.0
            && self.eq.iter().all(|b| eq_effective(b, self.eq_bypass).3 == 0.0 && matches!(eq_effective(b, self.eq_bypass).0, Kind::Peaking | Kind::LowShelf | Kind::HighShelf))
    }
}

/// The web's `filterCutoffs`: low-pass / high-pass cutoff and resonance of the DJ filter.
pub fn filter_cutoffs(v: f64) -> (Option<f64>, f64, f64) {
    let u = v.abs() / 100.0;
    let q = 0.707 + 0.9 * u;
    if v < 0.0 {
        (Some(12000.0 * (160.0f64 / 12000.0).powf(u)), 10.0, q)
    } else if v > 0.0 {
        (None, 20.0 * (4000.0f64 / 20.0).powf(u), q)
    } else {
        (None, 10.0, 0.707)
    }
}

/// The web's saturation knee: linear up to 1 at <= 100 %, softer the further above.
fn sat_knee(volume: f64) -> f64 {
    if volume <= 100.0 {
        1.0
    } else {
        0.97 - 0.37 * ((volume - 100.0) / 100.0).min(1.0)
    }
}

#[inline]
fn soft_clip(x: f64, knee: f64) -> f64 {
    let a = x.abs();
    let y = if a <= knee {
        a
    } else if knee >= 1.0 {
        1.0
    } else {
        knee + (1.0 - knee) * ((a - knee) / (1.0 - knee)).tanh()
    };
    y.copysign(x)
}

/// Biquad whose parameters glide; coefficients are recomputed every 32 samples while moving.
struct GlideFilter {
    kind: Kind,
    freq: Glide,
    q: Glide,
    gain: Glide,
    coefs: Coefs,
    state: [State; 2],
    countdown: usize,
    rate: f64,
}

impl GlideFilter {
    fn new(kind: Kind, freq: f64, q: f64, gain: f64, tc: f64, rate: f64) -> Self {
        GlideFilter {
            kind,
            freq: Glide::new(freq, tc, rate),
            q: Glide::new(q, tc, rate),
            gain: Glide::new(gain, tc, rate),
            coefs: Coefs::new(kind, freq, q, gain, rate),
            state: [State::default(); 2],
            countdown: 0,
            rate,
        }
    }

    /// A shape change sets values at once (Q means different things per type), like the web.
    fn set(&mut self, kind: Kind, freq: f64, q: f64, gain: f64) {
        self.freq.set(freq);
        self.q.set(q);
        self.gain.set(gain);
        if kind != self.kind {
            self.kind = kind;
            self.freq.jump();
            self.q.jump();
            self.gain.jump();
            self.coefs = Coefs::new(kind, freq, q, gain, self.rate);
        }
    }

    fn moving(&self) -> bool {
        !(self.freq.settled() && self.q.settled() && self.gain.settled())
    }

    #[inline]
    fn run(&mut self, l: f64, r: f64) -> (f64, f64) {
        if self.moving() {
            let (f, q, g) = (self.freq.step(), self.q.step(), self.gain.step());
            if self.countdown == 0 || !self.moving() {
                self.coefs = Coefs::new(self.kind, f, q, g, self.rate);
                self.countdown = 32;
            }
            self.countdown -= 1;
        }
        (self.state[0].run(&self.coefs, l), self.state[1].run(&self.coefs, r))
    }
}

/// Look-ahead peak limiter at 0 dBFS (the web uses a DynamicsCompressor at 0 dB, ratio 20, 2 ms
/// attack, 150 ms release, but Chrome's release is adaptive and much faster in practice: a 30 ms
/// release matches the web's levels within 0.2 dB on boosted loud masters, 150 ms was up to 1.1 dB
/// quieter). Min-hold over the look-ahead window, then a box average of the same
/// length: the gain is at the target by the time a peak leaves the delay line, so nothing passes
/// above 0 dBFS, and with nothing to limit the gain is exactly 1.
struct Limiter {
    look: usize,
    delay: Vec<[f64; 2]>,
    pos: usize,
    /// Required gains of the window (for the running minimum).
    need: Vec<f64>,
    env: f64,
    release: f64,
    box_sum: f64,
    box_buf: Vec<f64>,
}

impl Limiter {
    fn new(rate: f64) -> Self {
        let look = ((rate * 0.002) as usize).max(1);
        Limiter {
            look,
            delay: vec![[0.0; 2]; look],
            pos: 0,
            need: vec![1.0; look],
            env: 1.0,
            release: 1.0 - (-1.0 / (0.03 * rate)).exp(),
            box_sum: look as f64,
            box_buf: vec![1.0; look],
        }
    }

    #[inline]
    fn run(&mut self, l: f64, r: f64) -> (f64, f64) {
        let peak = l.abs().max(r.abs());
        let need = if peak > 1.0 { 1.0 / peak } else { 1.0 };
        self.need[self.pos] = need;
        // window minimum (the window is short: 2 ms)
        let wmin = self.need.iter().copied().fold(1.0, f64::min);
        self.env = if wmin < self.env { wmin } else { self.env + (wmin - self.env) * self.release };
        self.box_sum += self.env - self.box_buf[self.pos];
        self.box_buf[self.pos] = self.env;
        let g = (self.box_sum / self.look as f64).min(1.0);
        let [dl, dr] = self.delay[self.pos];
        self.delay[self.pos] = [l, r];
        self.pos = (self.pos + 1) % self.look;
        (dl * g, dr * g)
    }
}

/// The whole chain for one track (stereo).
pub struct Chain {
    rate: f64,
    params: Params,
    rate_stage: rate::Rate,
    eq: Vec<GlideFilter>,
    dj_lp: GlideFilter,
    dj_hp: GlideFilter,
    dj_dry: Glide,
    dj_wet: Glide,
    stereo: stereo::Stereo,
    space: space::Space,
    norm: Glide,
    volume: Glide,
    drive: Glide,
    knee: f64,
    limiter: Limiter,
    /// 0 = bypassed (bit-identical), 1 = processed; fades over 10 ms between the two.
    mix: Glide,
    active: bool,
    scratch: Vec<f64>,
    dry: Vec<f64>,
    seed: u64,
}

impl Chain {
    pub fn new(rate: u32) -> Self {
        let rate = rate as f64;
        let eq = eq_defaults()
            .iter()
            .map(|b| {
                let (k, f, q, g) = eq_effective(b, false);
                GlideFilter::new(k, f, q, g, 0.02, rate)
            })
            .collect();
        Chain {
            rate,
            params: Params::default(),
            rate_stage: rate::Rate::new(),
            eq,
            dj_lp: GlideFilter::new(Kind::LowPass, rate / 2.0, 0.707, 0.0, 0.02, rate),
            dj_hp: GlideFilter::new(Kind::HighPass, 10.0, 0.707, 0.0, 0.02, rate),
            dj_dry: Glide::new(1.0, 0.012, rate),
            dj_wet: Glide::new(0.0, 0.012, rate),
            stereo: stereo::Stereo::new(rate),
            space: space::Space::new(rate),
            norm: Glide::new(1.0, 0.05, rate),
            volume: Glide::new(1.0, 0.005, rate),
            drive: Glide::new(1.0, 0.005, rate),
            knee: 1.0,
            limiter: Limiter::new(rate),
            mix: Glide::new(0.0, 0.01 / 3.0, rate),
            active: false,
            scratch: Vec::new(),
            dry: Vec::new(),
            seed: 0x2545_F491_4F6C_DD1D,
        }
    }

    pub fn params(&self) -> &Params {
        &self.params
    }

    /// Takes new control values (glides like the web's `E.set`).
    pub fn set(&mut self, p: &Params) {
        let old = std::mem::replace(&mut self.params, p.clone());
        let (t, pf) = p.rates();
        self.rate_stage.tempo = t;
        self.rate_stage.pitch = pf;
        for (f, b) in self.eq.iter_mut().zip(p.eq.iter()) {
            let (k, fr, q, g) = eq_effective(b, p.eq_bypass);
            f.set(k, fr, q, g);
        }
        if p.filter != old.filter || !self.active {
            let (lp, hp, q) = filter_cutoffs(p.filter);
            self.dj_lp.set(Kind::LowPass, lp.unwrap_or(self.rate / 2.0), q, 0.0);
            self.dj_hp.set(Kind::HighPass, hp, q, 0.0);
            self.dj_wet.set(if p.filter == 0.0 { 0.0 } else { 1.0 });
            self.dj_dry.set(if p.filter == 0.0 { 1.0 } else { 0.0 });
        }
        self.stereo.set(p.width, p.rotate);
        self.space.set_reverb(p.reverb);
        self.space.set_echo(p.echo);
        self.space.set_echo_time(p.echo_time);
        self.space.set_feedback(p.feedback);
        if p.room != old.room {
            self.space.set_room(p.room, false);
        }
        self.norm.set(10f64.powf(p.norm_db / 20.0));
        let v = p.volume;
        self.volume.set(if v <= 100.0 { (v / 100.0).powi(2) } else { 1.0 });
        self.drive.set(if v > 100.0 { 10f64.powf((v - 100.0) / 100.0 * 14.0 / 20.0) } else { 1.0 });
        self.knee = sat_knee(v);
        let neutral = p.neutral();
        if neutral != !self.active {
            if !neutral && !self.active {
                self.active = true;
            }
            self.mix.set(if neutral { 0.0 } else { 1.0 });
        }
    }

    /// After a seek: drop the history (tails, WSOLA state).
    pub fn reset(&mut self) {
        let p = self.params.clone();
        let rate = self.rate as u32;
        *self = Chain::new(rate);
        self.set(&p);
        if !p.neutral() {
            self.mix.jump();
        }
        self.space.set_room(p.room, true);
    }

    /// Nothing is being changed: the caller passes the samples through untouched.
    pub fn bypassed(&self) -> bool {
        !self.active
    }

    /// Tempo of the output relative to the input.
    pub fn tempo(&self) -> f64 {
        if self.active { self.rate_stage.tempo } else { 1.0 }
    }

    /// Interleaved stereo i32 in (left-justified), interleaved stereo i32 appended to `out`.
    /// `dither_bits`: depth of the device (TPDF dither at its LSB; 32 = none).
    pub fn process(&mut self, input: &[i32], out: &mut Vec<i32>, dither_bits: u32) {
        if !self.active {
            out.extend_from_slice(input);
            return;
        }
        const S: f64 = 2_147_483_648.0;
        self.dry.clear();
        self.dry.extend(input.iter().map(|s| *s as f64 / S));
        // 1. tempo / pitch (may change the length)
        self.scratch.clear();
        let rate_idle = self.rate_stage.idle();
        if rate_idle {
            self.scratch.extend_from_slice(&self.dry);
        } else {
            self.rate_stage.process(&self.dry, &mut self.scratch);
        }
        let mut buf = std::mem::take(&mut self.scratch);
        // 2. EQ, 3. DJ filter
        for fr in buf.as_chunks_mut::<2>().0 {
            let (mut l, mut r) = (fr[0], fr[1]);
            for f in self.eq.iter_mut() {
                (l, r) = f.run(l, r);
            }
            let (wet, dry) = (self.dj_wet.step(), self.dj_dry.step());
            let (fl, fr_) = self.dj_lp.run(l, r);
            let (hl, hr) = self.dj_hp.run(fl, fr_);
            fr[0] = l * dry + hl * wet;
            fr[1] = r * dry + hr * wet;
        }
        // 4. width + 8D, 5. reverb + echo
        if !self.stereo.idle() {
            self.stereo.process(&mut buf);
        }
        self.space.process(&mut buf);
        // 6. normalization, volume, limiter, boost + saturation, then the bypass crossfade
        let lsb = if dither_bits < 32 { (1u64 << (32 - dither_bits)) as f64 } else { 0.0 };
        let same_length = buf.len() == self.dry.len();
        for (i, fr) in buf.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let g = self.norm.step() * self.volume.step();
            let (l, r) = self.limiter.run(fr[0] * g, fr[1] * g);
            let d = self.drive.step();
            let (mut l, mut r) = (soft_clip(l * d, self.knee), soft_clip(r * d, self.knee));
            let m = self.mix.step();
            if m < 1.0 && same_length {
                l = self.dry[2 * i] + (l - self.dry[2 * i]) * m;
                r = self.dry[2 * i + 1] + (r - self.dry[2 * i + 1]) * m;
            }
            fr[0] = l;
            fr[1] = r;
        }
        for v in &buf {
            let mut x = v * S;
            if lsb > 0.0 {
                // TPDF dither, then round to the device's grid
                let n = (rand(&mut self.seed) - rand(&mut self.seed)) * lsb;
                x = ((x + n) / lsb).round() * lsb;
            } else {
                x = x.round();
            }
            out.push(x.clamp(i32::MIN as f64, i32::MAX as f64) as i32);
        }
        self.scratch = buf;
        if self.mix.value() == 0.0 && self.mix.settled() && self.params.neutral() && self.space.idle() {
            // faded back to the bypass: from here on the samples are untouched again
            self.active = false;
            let p = self.params.clone();
            let rate = self.rate as u32;
            *self = Chain::new(rate);
            self.params = p;
        }
    }
}

fn rand(s: &mut u64) -> f64 {
    *s ^= *s >> 12;
    *s ^= *s << 25;
    *s ^= *s >> 27;
    (s.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(n: usize, amp: f64) -> Vec<i32> {
        (0..n).flat_map(|i| {
            let v = ((i as f64 * 0.05).sin() * amp * 2_147_483_647.0) as i32;
            [v, v / 2]
        }).collect()
    }

    #[test]
    fn neutral_is_bit_identical() {
        let mut c = Chain::new(44100);
        c.set(&Params::default());
        let input = tone(10000, 0.9);
        let mut out = Vec::new();
        c.process(&input, &mut out, 16);
        assert_eq!(out, input);
    }

    #[test]
    fn normalization_turns_down_and_back() {
        let mut c = Chain::new(44100);
        c.set(&Params { norm_db: -6.0, ..Params::default() });
        let input = tone(44100, 0.5);
        let mut out = Vec::new();
        c.process(&input, &mut out, 32);
        // after the fade and the glide: about half the level
        let tail = &out[out.len() - 2000..];
        let src = &input[input.len() - 2000..];
        let ratio = tail.iter().map(|v| (*v as f64).abs()).sum::<f64>() / src.iter().map(|v| (*v as f64).abs()).sum::<f64>();
        assert!((ratio - 0.501).abs() < 0.01, "{ratio}");
        // back to neutral: after the fade the output is bit-identical again
        c.set(&Params::default());
        let mut out2 = Vec::new();
        c.process(&input, &mut out2, 32);
        assert!(c.bypassed());
        let mut out3 = Vec::new();
        c.process(&input, &mut out3, 32);
        assert_eq!(out3, input);
    }

    #[test]
    fn limiter_keeps_boosts_below_full_scale() {
        let mut c = Chain::new(44100);
        let mut p = Params::default();
        p.eq[3].gain = 12.0;
        c.set(&p);
        let input = tone(44100, 0.95);
        let mut out = Vec::new();
        c.process(&input, &mut out, 32);
        let peak = out.iter().map(|v| (*v as f64).abs()).fold(0.0, f64::max) / 2_147_483_648.0;
        assert!(peak <= 1.0, "{peak}");
    }

    #[test]
    fn echo_off_silences_the_repeats() {
        let mut c = Chain::new(44100);
        c.set(&Params { echo: 40.0, feedback: 90.0, ..Params::default() });
        // a short burst, then silence: only the echo keeps sounding
        let mut input = tone(4410, 0.5);
        input.resize(44100 * 2 * 2, 0);
        let mut out = Vec::new();
        c.process(&input, &mut out, 32);
        let peak = |o: &[i32]| o.iter().map(|v| (*v as f64).abs()).fold(0.0, f64::max) / 2_147_483_648.0;
        assert!(peak(&out[out.len() - 44100..]) > 0.01, "the repeats ring on while Echo is up");
        c.set(&Params { echo: 0.0, feedback: 90.0, ..Params::default() });
        let silence = vec![0i32; 44100 * 2];
        let mut out2 = Vec::new();
        c.process(&silence, &mut out2, 32);
        assert!(peak(&out2[out2.len() - 22050..]) < 1e-4, "{}", peak(&out2[out2.len() - 22050..]));
    }

    #[test]
    fn rates_follow_the_web() {
        let p = Params { tempo: 1.25, nightcore: 100.0, ..Params::default() };
        assert_eq!(p.rates(), (1.25, 1.25));
        let p = Params { tempo: 0.5, ..Params::default() };
        assert_eq!(p.rates(), (0.5, 1.0));
        let p = Params { pitch: 12.0, ..Params::default() };
        assert!((p.rates().1 - 2.0).abs() < 1e-12);
    }
}
