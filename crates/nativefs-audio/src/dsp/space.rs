//! Reverb and echo of the web player (`createEngine`): the sends are high-passed (180 / 220 Hz) so the
//! bass stays dry. Reverb = convolution with a generated noise tail whose length follows Room,
//! normalized like Chromium's `ConvolverNode`; Room changes crossfade between two convolvers. Echo =
//! a delay line with a damped feedback loop; its time glides (tape-style pitch bend).

#![allow(clippy::needless_range_loop)] // per-channel code reads clearer with indices

use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use super::biquad::{Coefs, Kind, State};
use super::Glide;

/// Convolution block (latency of the reverb path, ~21 ms at 48 kHz; the tail is seconds long).
const BLOCK: usize = 1024;

/// Uniformly partitioned overlap-save convolution, stereo (L * irL, R * irR like Web Audio).
struct Convolver {
    fft: Arc<dyn Fft<f64>>,
    ifft: Arc<dyn Fft<f64>>,
    /// Impulse response spectra per channel and partition.
    ir: [Vec<Vec<Complex<f64>>>; 2],
    /// Frequency-domain delay line of input spectra per channel (newest at `head`).
    fdl: [Vec<Vec<Complex<f64>>>; 2],
    head: usize,
    /// Last two input blocks (time domain) per channel.
    hist: [Vec<f64>; 2],
    acc: Vec<Complex<f64>>,
    tmp: Vec<Complex<f64>>,
    out: [Vec<f64>; 2],
}

impl Convolver {
    fn new(planner: &mut FftPlanner<f64>, ir: [Vec<f64>; 2]) -> Self {
        let n = 2 * BLOCK;
        let fft = planner.plan_fft_forward(n);
        let ifft = planner.plan_fft_inverse(n);
        let parts = ir[0].len().div_ceil(BLOCK).max(1);
        let spectra = |h: &[f64]| -> Vec<Vec<Complex<f64>>> {
            (0..parts)
                .map(|p| {
                    let mut buf = vec![Complex::new(0.0, 0.0); n];
                    for (i, v) in h.iter().skip(p * BLOCK).take(BLOCK).enumerate() {
                        buf[i] = Complex::new(*v, 0.0);
                    }
                    fft.process(&mut buf);
                    buf
                })
                .collect()
        };
        let ir = [spectra(&ir[0]), spectra(&ir[1])];
        let empty = || vec![vec![Complex::new(0.0, 0.0); n]; parts];
        Convolver {
            fft,
            ifft,
            ir,
            fdl: [empty(), empty()],
            head: 0,
            hist: [vec![0.0; n], vec![0.0; n]],
            acc: vec![Complex::new(0.0, 0.0); n],
            tmp: vec![Complex::new(0.0, 0.0); n],
            out: [vec![0.0; BLOCK], vec![0.0; BLOCK]],
        }
    }

    /// One block of `BLOCK` frames per channel in, one out (delayed by one block).
    fn block(&mut self, input: [&[f64]; 2]) {
        let n = 2 * BLOCK;
        let parts = self.ir[0].len();
        self.head = (self.head + parts - 1) % parts;
        for c in 0..2 {
            let h = &mut self.hist[c];
            h.copy_within(BLOCK.., 0);
            h[BLOCK..].copy_from_slice(input[c]);
            let slot = &mut self.fdl[c][self.head];
            for (s, v) in slot.iter_mut().zip(h.iter()) {
                *s = Complex::new(*v, 0.0);
            }
            self.fft.process(slot);
            self.acc.iter_mut().for_each(|a| *a = Complex::new(0.0, 0.0));
            for p in 0..parts {
                let x = &self.fdl[c][(self.head + p) % parts];
                let hspec = &self.ir[c][p];
                for ((a, xv), hv) in self.acc.iter_mut().zip(x).zip(hspec) {
                    *a += xv * hv;
                }
            }
            self.tmp.copy_from_slice(&self.acc);
            self.ifft.process(&mut self.tmp);
            let scale = 1.0 / n as f64;
            for (o, v) in self.out[c].iter_mut().zip(&self.tmp[BLOCK..]) {
                *o = v.re * scale;
            }
        }
    }
}

/// The web's `makeImpulse(room)`: noise with a darkening, exponentially decaying tail.
fn make_impulse(room: f64, rate: f64, seed: &mut u64) -> [Vec<f64>; 2] {
    let decay = 0.5 * 10f64.powf(room / 100.0);
    let len = (rate * (decay + 0.05)) as usize;
    let pre = (rate * 0.015) as usize;
    let mut out = [vec![0.0; len], vec![0.0; len]];
    for ch in out.iter_mut() {
        let mut y = 0.0;
        for (i, d) in ch.iter_mut().enumerate().skip(pre) {
            let t = (i - pre) as f64 / rate;
            let a = 0.85 - 0.75 * (t / decay).min(1.0);
            y += a * (rand(seed) * 2.0 - 1.0 - y);
            *d = y * (-6.9 * t / decay).exp();
        }
    }
    // Chromium's ConvolverNode normalization (normalize = true)
    let power: f64 = out.iter().flat_map(|c| c.iter()).map(|v| v * v).sum();
    let mut power = (power / (2.0 * len.max(1) as f64)).sqrt();
    if !power.is_finite() || power < 0.000125 {
        power = 0.000125;
    }
    let scale = 1.0 / power * 10f64.powf(-58.0 * 0.05) * (44100.0 / rate);
    for c in out.iter_mut() {
        c.iter_mut().for_each(|v| *v *= scale);
    }
    out
}

/// xorshift64*: uniform in [0, 1).
fn rand(s: &mut u64) -> f64 {
    *s ^= *s >> 12;
    *s ^= *s << 25;
    *s ^= *s >> 27;
    (s.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64
}

pub struct Space {
    rate: f64,
    planner: FftPlanner<f64>,
    seed: u64,
    // reverb
    rev_hp: Coefs,
    rev_hp_s: [State; 2],
    rev_tone: Coefs,
    rev_tone_s: [State; 2],
    pub rev_send: Glide,
    conv: [Option<Convolver>; 2],
    conv_gain: [Glide; 2],
    active: usize,
    room: f64,
    /// Room change waiting for the knob to rest (the web waits 140 ms).
    pending_room: Option<(f64, usize)>,
    in_block: [Vec<f64>; 2],
    out_block: [Vec<f64>; 2],
    block_pos: usize,
    // echo
    echo_hp: Coefs,
    echo_hp_s: [State; 2],
    pub echo_send: Glide,
    delay: [Vec<f64>; 2],
    dpos: usize,
    pub delay_time: Glide,
    damp: Coefs,
    damp_s: [State; 2],
    pub feedback: Glide,
    pub dry: Glide,
    /// 1 while Echo is up; fades to 0 when it is turned off, taking the repeats still circling
    /// in the feedback loop with it (the web lets them ring on, for minutes at high Repeats).
    echo_return: Glide,
    reverb_pct: f64,
    echo_pct: f64,
}

impl Space {
    pub fn new(rate: f64) -> Self {
        let max_delay = (rate * 2.5) as usize + 4;
        let mut s = Space {
            rate,
            planner: FftPlanner::new(),
            seed: 0x9E37_79B9_7F4A_7C15,
            rev_hp: Coefs::new(Kind::HighPass, 180.0, 0.707, 0.0, rate),
            rev_hp_s: [State::default(); 2],
            rev_tone: Coefs::new(Kind::LowPass, 9000.0, 0.707, 0.0, rate),
            rev_tone_s: [State::default(); 2],
            rev_send: Glide::new(0.0, 0.005, rate),
            conv: [None, None],
            conv_gain: [Glide::new(1.0, 0.08, rate), Glide::new(0.0, 0.08, rate)],
            active: 0,
            room: -1.0,
            pending_room: None,
            in_block: [vec![0.0; BLOCK], vec![0.0; BLOCK]],
            out_block: [vec![0.0; BLOCK], vec![0.0; BLOCK]],
            block_pos: 0,
            echo_hp: Coefs::new(Kind::HighPass, 220.0, 0.707, 0.0, rate),
            echo_hp_s: [State::default(); 2],
            echo_send: Glide::new(0.0, 0.005, rate),
            delay: [vec![0.0; max_delay], vec![0.0; max_delay]],
            dpos: 0,
            delay_time: Glide::new(0.375, 0.06, rate),
            damp: Coefs::new(Kind::LowPass, 4500.0, 0.707, 0.0, rate),
            damp_s: [State::default(); 2],
            feedback: Glide::new(0.35, 0.005, rate),
            dry: Glide::new(1.0, 0.005, rate),
            echo_return: Glide::new(0.0, 0.08, rate),
            reverb_pct: 0.0,
            echo_pct: 0.0,
        };
        s.set_room(45.0, true);
        s
    }

    fn update_dry(&mut self) {
        let r = (self.reverb_pct / 100.0).powf(0.8);
        let e = (self.echo_pct / 100.0).powf(0.8);
        self.dry.set((1.0 - 0.15 * r) * (1.0 - 0.12 * e));
    }

    pub fn set_reverb(&mut self, pct: f64) {
        self.reverb_pct = pct;
        self.rev_send.set((pct / 100.0).powf(0.8) * 0.75);
        self.update_dry();
    }

    pub fn set_echo(&mut self, pct: f64) {
        self.echo_pct = pct;
        self.echo_return.set(if pct > 0.0 { 1.0 } else { 0.0 });
        if pct > 0.0 {
            // straight back on, no fade-in delay for the first repeat
            self.echo_return.jump();
        }
        self.echo_send.set((pct / 100.0).powf(0.8) * 0.6);
        self.update_dry();
    }

    pub fn set_echo_time(&mut self, ms: f64) {
        self.delay_time.set(ms / 1000.0);
    }

    pub fn set_feedback(&mut self, pct: f64) {
        self.feedback.set(pct / 100.0);
    }

    /// New room size: `instant` swaps at once, otherwise after 140 ms without further changes.
    pub fn set_room(&mut self, room: f64, instant: bool) {
        if (room - self.room).abs() < 1e-9 && self.pending_room.is_none() {
            return;
        }
        if instant {
            self.swap_impulse(room, true);
        } else {
            self.pending_room = Some((room, (self.rate * 0.14) as usize));
        }
    }

    fn swap_impulse(&mut self, room: f64, instant: bool) {
        let ir = make_impulse(room, self.rate, &mut self.seed);
        let next = 1 - self.active;
        self.conv[next] = Some(Convolver::new(&mut self.planner, ir));
        self.conv_gain[next].set(1.0);
        self.conv_gain[self.active].set(0.0);
        if instant {
            self.conv_gain[next].jump();
            self.conv_gain[self.active].jump();
            self.conv[self.active] = None;
        }
        self.active = next;
        self.room = room;
    }

    /// Nothing audible comes out of the reverb / echo paths and the dry gain is 1.
    pub fn idle(&self) -> bool {
        self.reverb_pct == 0.0 && self.echo_pct == 0.0 && self.rev_send.settled() && self.echo_send.settled() && self.dry.settled() && self.echo_return.settled()
    }

    /// Interleaved stereo in place: out = dry * x + reverb + echo.
    pub fn process(&mut self, buf: &mut [f64]) {
        for fr in buf.as_chunks_mut::<2>().0 {
            if let Some((room, left)) = self.pending_room {
                if left == 0 {
                    self.pending_room = None;
                    self.swap_impulse(room, false);
                } else {
                    self.pending_room = Some((room, left - 1));
                }
            }
            let (l, r) = (fr[0], fr[1]);
            // reverb send into the block convolver; its output is one block late
            let send = self.rev_send.step();
            let x = [self.rev_hp_s[0].run(&self.rev_hp, l) * send, self.rev_hp_s[1].run(&self.rev_hp, r) * send];
            let i = self.block_pos;
            self.in_block[0][i] = x[0];
            self.in_block[1][i] = x[1];
            let mut wet = [self.out_block[0][i], self.out_block[1][i]];
            self.block_pos += 1;
            if self.block_pos == BLOCK {
                self.block_pos = 0;
                let g = [self.conv_gain[0].value(), self.conv_gain[1].value()];
                let mut sum = [vec![0.0; BLOCK], vec![0.0; BLOCK]];
                for k in 0..2 {
                    if let Some(cv) = &mut self.conv[k] {
                        cv.block([&self.in_block[0], &self.in_block[1]]);
                        for c in 0..2 {
                            for (s, o) in sum[c].iter_mut().zip(&cv.out[c]) {
                                *s += o * g[k];
                            }
                        }
                    }
                }
                self.out_block = sum;
                // the faded-out convolver can go once silent
                let other = 1 - self.active;
                for _ in 0..BLOCK {
                    self.conv_gain[0].step();
                    self.conv_gain[1].step();
                }
                if self.conv[other].is_some() && self.conv_gain[other].value() < 1e-4 {
                    self.conv[other] = None;
                }
            }
            for c in 0..2 {
                wet[c] = self.rev_tone_s[c].run(&self.rev_tone, wet[c]);
            }
            // echo: send -> delay (fractional, linear) -> damp -> mix, damp * feedback -> delay
            let es = self.echo_send.step();
            let dt = self.delay_time.step() * self.rate;
            let ret = self.echo_return.step();
            let fb = self.feedback.step() * ret;
            let len = self.delay[0].len();
            let mut echo = [0.0; 2];
            for c in 0..2 {
                let read = self.dpos as f64 + len as f64 - dt;
                let i0 = read.floor() as usize % len;
                let frac = read - read.floor();
                let d = &self.delay[c];
                let tap = d[i0] + (d[(i0 + 1) % len] - d[i0]) * frac;
                let damped = self.damp_s[c].run(&self.damp, tap);
                echo[c] = damped * ret;
                let input = self.echo_hp_s[c].run(&self.echo_hp, if c == 0 { l } else { r }) * es;
                self.delay[c][self.dpos] = input + damped * fb;
            }
            self.dpos = (self.dpos + 1) % len;
            let dry = self.dry.step();
            fr[0] = l * dry + wet[0] + echo[0];
            fr[1] = r * dry + wet[1] + echo[1];
        }
    }
}
