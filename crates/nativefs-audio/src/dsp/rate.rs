//! Tempo and pitch: the web player's `tvfs-pitch` worklet (WSOLA + cubic resampling) generalised.
//! On the web the media element does the tempo (resampling, or the browser's own time-stretch) and
//! the worklet only shifts the pitch; natively both happen here. Output tempo `T`, pitch factor `P`:
//! WSOLA stretches time by `P / T` (pieces re-laid where they continue best, pitch untouched), then
//! the stretched stream is read `P` times faster (cubic). `P == T` (record-style speed, Nightcore
//! 100 %) skips WSOLA: a clean resample, like the browser with `preservesPitch` off.

use std::f64::consts::PI;

const RING: usize = 1 << 17;
const MASK: usize = RING - 1;
const FRAME: usize = 1536;
const HOP: usize = FRAME / 2;
const SEEK: i64 = 448;
const DECIM: i64 = 4;

/// The worklet's 2nd-order Butterworth low-pass (anti-aliasing before reading faster).
#[derive(Clone, Copy, Default)]
struct LowPass {
    b0: f64,
    b1: f64,
    a1: f64,
    a2: f64,
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl LowPass {
    fn set(&mut self, f_norm: f64) {
        let w = 2.0 * PI * f_norm.min(0.49);
        let (sin, cw) = w.sin_cos();
        let al = sin / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + al;
        self.b0 = (1.0 - cw) / 2.0 / a0;
        self.b1 = (1.0 - cw) / a0;
        self.a1 = -2.0 * cw / a0;
        self.a2 = (1.0 - al) / a0;
    }

    #[inline]
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b0 * (x + self.x2) + self.b1 * self.x1 - self.a1 * self.y1 - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Catmull-Rom interpolation between y1 and y2.
#[inline]
fn cubic(y0: f64, y1: f64, y2: f64, y3: f64, f: f64) -> f64 {
    y1 + 0.5 * f * (y2 - y0 + f * (2.0 * y0 - 5.0 * y1 + 4.0 * y2 - y3 + f * (3.0 * (y1 - y2) + y3 - y0)))
}

fn hann() -> Vec<f64> {
    (0..FRAME).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f64 / FRAME as f64).cos()).collect()
}

pub struct Rate {
    hann: Vec<f64>,
    inp: [Vec<f64>; 2],
    str_: [Vec<f64>; 2],
    ola: [Vec<f64>; 2],
    aa: [[LowPass; 2]; 2],
    aa_for: f64,
    /// Input samples written (per channel).
    in_count: i64,
    /// Input position of the next WSOLA piece.
    ana_pos: f64,
    prev_pos: i64,
    /// Stretched samples written / read position.
    s_w: i64,
    rp: f64,
    pub tempo: f64,
    pub pitch: f64,
    stretching: bool,
}

impl Default for Rate {
    fn default() -> Self {
        Self::new()
    }
}

impl Rate {
    pub fn new() -> Self {
        let mut r = Rate {
            hann: hann(),
            inp: [vec![0.0; RING], vec![0.0; RING]],
            str_: [vec![0.0; RING], vec![0.0; RING]],
            ola: [vec![0.0; FRAME], vec![0.0; FRAME]],
            aa: [[LowPass::default(); 2]; 2],
            aa_for: 0.0,
            in_count: 0,
            ana_pos: 0.0,
            prev_pos: -1,
            s_w: 0,
            rp: 1.0,
            tempo: 1.0,
            pitch: 1.0,
            stretching: false,
        };
        r.reset();
        r
    }

    /// Forget everything (after a seek). A few zero samples lead in, so the first WSOLA search has
    /// history behind it.
    pub fn reset(&mut self) {
        for c in 0..2 {
            self.inp[c].fill(0.0);
            self.str_[c].fill(0.0);
            self.ola[c].fill(0.0);
        }
        for ch in self.aa.iter_mut() {
            for f in ch.iter_mut() {
                f.x1 = 0.0;
                f.x2 = 0.0;
                f.y1 = 0.0;
                f.y2 = 0.0;
            }
        }
        self.in_count = SEEK;
        self.ana_pos = SEEK as f64;
        self.prev_pos = -1;
        self.s_w = SEEK;
        self.rp = SEEK as f64;
    }

    /// Neutral: nothing to do (the chain then skips this stage).
    pub fn idle(&self) -> bool {
        self.tempo == 1.0 && self.pitch == 1.0
    }

    fn set_aa(&mut self, read_rate: f64) {
        if read_rate > 1.0001 && self.aa_for != read_rate {
            for ch in self.aa.iter_mut() {
                for f in ch.iter_mut() {
                    f.set(0.45 / read_rate);
                }
            }
            self.aa_for = read_rate;
        }
    }

    /// One WSOLA piece into the stretched stream. `advance` = input advance per stretched sample.
    fn piece(&mut self, advance: f64, read_rate: f64) {
        let nominal = self.ana_pos.round() as i64;
        let mut best = nominal;
        if self.prev_pos >= 0 {
            // the new piece's first half should continue the previous piece's second half
            let reference = self.prev_pos + HOP as i64;
            let (il, ir) = (&self.inp[0], &self.inp[1]);
            let corr = |c: i64, step: usize| {
                let (mut xy, mut yy) = (0.0, 1e-9);
                let mut i = 0;
                while i < HOP {
                    let ra = ((reference + i as i64) as usize) & MASK;
                    let rb = ((c + i as i64) as usize) & MASK;
                    let a = il[ra] + ir[ra];
                    let b = il[rb] + ir[rb];
                    xy += a * b;
                    yy += b * b;
                    i += step;
                }
                xy / yy.sqrt()
            };
            let mut best_score = f64::NEG_INFINITY;
            let mut d = -SEEK;
            while d <= SEEK {
                let s = corr(nominal + d, DECIM as usize);
                if s > best_score {
                    best_score = s;
                    best = nominal + d;
                }
                d += DECIM;
            }
            let coarse = best;
            best_score = f64::NEG_INFINITY;
            for c in (coarse - DECIM + 1)..(coarse + DECIM) {
                let s = corr(c, 1);
                if s > best_score {
                    best_score = s;
                    best = c;
                }
            }
        }
        let aa = read_rate > 1.0001;
        for c in 0..2 {
            let (inp, ola, st) = (&self.inp[c], &mut self.ola[c], &mut self.str_[c]);
            for (i, o) in ola.iter_mut().enumerate() {
                *o += inp[((best + i as i64) as usize) & MASK] * self.hann[i];
            }
            let f = &mut self.aa[c];
            for i in 0..HOP {
                let v = if aa {
                    let t = f[0].run(ola[i]);
                    f[1].run(t)
                } else {
                    ola[i]
                };
                st[((self.s_w + i as i64) as usize) & MASK] = v;
            }
            ola.copy_within(HOP.., 0);
            ola[FRAME - HOP..].fill(0.0);
        }
        self.s_w += HOP as i64;
        self.prev_pos = best;
        self.ana_pos += HOP as f64 * advance;
    }

    /// Interleaved stereo in, interleaved stereo appended to `out`.
    pub fn process(&mut self, input: &[f64], out: &mut Vec<f64>) {
        let read_rate = self.pitch;
        let stretch = self.pitch / self.tempo;
        let stretching = (stretch - 1.0).abs() > 1e-4;
        if stretching != self.stretching {
            // switching between pure resampling and WSOLA: continue from the read position
            self.stretching = stretching;
            let at = self.rp.floor() as i64;
            if stretching {
                // WSOLA restarts at the input sample being read now
                self.ana_pos = at as f64;
                self.prev_pos = -1;
                self.s_w = at;
                for c in 0..2 {
                    self.ola[c].fill(0.0);
                    for k in 0..4 {
                        let i = ((at + k) as usize) & MASK;
                        self.str_[c][i] = self.inp[c][i];
                    }
                }
                self.s_w = at + 4;
            } else {
                // resampling reads the input directly; map the stretched read position back
                let back = (self.s_w - at) as f64 / stretch.max(1e-6);
                self.rp = self.in_count as f64 - back;
            }
        }
        self.set_aa(read_rate);
        for fr in input.as_chunks::<2>().0 {
            let w = (self.in_count as usize) & MASK;
            if stretching {
                self.inp[0][w] = fr[0];
                self.inp[1][w] = fr[1];
            } else {
                // without WSOLA the input is the stream that is read: anti-alias it here
                let aa = read_rate > 1.0001;
                let (l, r) = if aa {
                    let tl = self.aa[0][0].run(fr[0]);
                    let tr = self.aa[1][0].run(fr[1]);
                    (self.aa[0][1].run(tl), self.aa[1][1].run(tr))
                } else {
                    (fr[0], fr[1])
                };
                self.inp[0][w] = l;
                self.inp[1][w] = r;
            }
            self.in_count += 1;
        }
        loop {
            let i = self.rp.floor() as i64;
            let avail = if stretching {
                while i + 3 > self.s_w && self.ana_pos + (SEEK as f64) + FRAME as f64 <= self.in_count as f64 {
                    self.piece(1.0 / stretch, read_rate);
                }
                self.s_w
            } else {
                self.in_count
            };
            if i + 3 > avail {
                break;
            }
            let src = if stretching { &self.str_ } else { &self.inp };
            let f = self.rp - i as f64;
            let idx = |k: i64| ((i + k) as usize) & MASK;
            for s in src {
                out.push(cubic(s[idx(-1)], s[idx(0)], s[idx(1)], s[idx(2)], f));
            }
            self.rp += read_rate;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, secs: f64, rate: f64) -> Vec<f64> {
        (0..(secs * rate) as usize).flat_map(|i| {
            let v = (2.0 * PI * freq * i as f64 / rate).sin() * 0.5;
            [v, v]
        }).collect()
    }

    /// Dominant frequency by zero crossings of the left channel.
    fn freq_of(buf: &[f64], rate: f64) -> f64 {
        let l: Vec<f64> = buf.iter().step_by(2).copied().collect();
        let skip = l.len() / 4;
        let part = &l[skip..l.len() - skip];
        let crossings = part.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        crossings as f64 / (part.len() as f64 / rate)
    }

    fn run(tempo: f64, pitch: f64) -> (f64, f64) {
        let rate = 44100.0;
        let input = sine(440.0, 4.0, rate);
        let mut r = Rate::new();
        r.tempo = tempo;
        r.pitch = pitch;
        let mut out = Vec::new();
        for chunk in input.chunks(4096 * 2) {
            r.process(chunk, &mut out);
        }
        let dur = out.len() as f64 / 2.0 / rate;
        (freq_of(&out, rate), dur)
    }

    #[test]
    fn varispeed_moves_pitch_and_tempo() {
        let (f, d) = run(1.25, 1.25);
        assert!((f - 550.0).abs() < 3.0, "{f}");
        assert!((d - 4.0 / 1.25).abs() < 0.05, "{d}");
    }

    #[test]
    fn time_stretch_keeps_pitch() {
        let (f, d) = run(0.8, 1.0);
        assert!((f - 440.0).abs() < 3.0, "{f}");
        assert!((d - 4.0 / 0.8).abs() < 0.1, "{d}");
    }

    #[test]
    fn pitch_shift_keeps_tempo() {
        let (f, d) = run(1.0, 2f64.powf(7.0 / 12.0));
        assert!((f - 440.0 * 2f64.powf(7.0 / 12.0)).abs() < 4.0, "{f}");
        assert!((d - 4.0).abs() < 0.1, "{d}");
    }
}
