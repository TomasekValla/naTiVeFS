//! Biquad filters with the exact coefficient formulas of the Web Audio `BiquadFilterNode` (the web
//! player is built from them), including its quirks: the Q of low-pass / high-pass is a resonance
//! in dB, shelves ignore Q (slope 1), a peaking filter at 0 dB is an exact identity.

use std::f64::consts::PI;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    LowPass,
    HighPass,
    Peaking,
    LowShelf,
    HighShelf,
    Notch,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coefs {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Coefs {
    pub const IDENTITY: Coefs = Coefs { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 };

    /// `q` and `gain_db` as the Web Audio parameters `Q` and `gain`.
    pub fn new(kind: Kind, freq: f64, q: f64, gain_db: f64, rate: f64) -> Coefs {
        let nyq = rate / 2.0;
        let f = (freq / nyq).clamp(0.0, 1.0);
        let w0 = PI * f;
        let (sin, cos) = w0.sin_cos();
        let a = 10f64.powf(gain_db / 40.0);
        let norm = |b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64| Coefs { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 };
        match kind {
            Kind::LowPass | Kind::HighPass => {
                if f >= 1.0 {
                    // at Nyquist: low-pass passes everything, high-pass nothing
                    return if kind == Kind::LowPass { Coefs::IDENTITY } else { Coefs { b0: 0.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 } };
                }
                if f <= 0.0 {
                    return if kind == Kind::LowPass { Coefs { b0: 0.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 } } else { Coefs::IDENTITY };
                }
                // Q in dB (resonance)
                let alpha = sin / (2.0 * 10f64.powf(q / 20.0));
                if kind == Kind::LowPass {
                    norm((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
                } else {
                    norm((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
                }
            }
            Kind::Peaking => {
                if f <= 0.0 || f >= 1.0 || q <= 0.0 {
                    return if q <= 0.0 && f > 0.0 && f < 1.0 { Coefs { b0: a * a, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 } } else { Coefs::IDENTITY };
                }
                let alpha = sin / (2.0 * q);
                norm(1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a)
            }
            Kind::LowShelf | Kind::HighShelf => {
                if f >= 1.0 {
                    return if kind == Kind::LowShelf { Coefs { b0: a * a, ..Coefs::IDENTITY } } else { Coefs::IDENTITY };
                }
                if f <= 0.0 {
                    return if kind == Kind::LowShelf { Coefs::IDENTITY } else { Coefs { b0: a * a, ..Coefs::IDENTITY } };
                }
                // shelf slope S = 1
                let alpha = sin / 2.0 * 2f64.sqrt();
                let k = 2.0 * a.sqrt() * alpha;
                if kind == Kind::LowShelf {
                    norm(
                        a * ((a + 1.0) - (a - 1.0) * cos + k),
                        2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                        a * ((a + 1.0) - (a - 1.0) * cos - k),
                        (a + 1.0) + (a - 1.0) * cos + k,
                        -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                        (a + 1.0) + (a - 1.0) * cos - k,
                    )
                } else {
                    norm(
                        a * ((a + 1.0) + (a - 1.0) * cos + k),
                        -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                        a * ((a + 1.0) + (a - 1.0) * cos - k),
                        (a + 1.0) - (a - 1.0) * cos + k,
                        2.0 * ((a - 1.0) - (a + 1.0) * cos),
                        (a + 1.0) - (a - 1.0) * cos - k,
                    )
                }
            }
            Kind::Notch => {
                if f <= 0.0 || f >= 1.0 || q <= 0.0 {
                    return if q <= 0.0 && f > 0.0 && f < 1.0 { Coefs { b0: 0.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 } } else { Coefs::IDENTITY };
                }
                let alpha = sin / (2.0 * q);
                norm(1.0, -2.0 * cos, 1.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
            }
        }
    }

    pub fn is_identity(&self) -> bool {
        *self == Coefs::IDENTITY || (self.b0 == 1.0 && self.b1 == self.a1 && self.b2 == self.a2)
    }

    /// Magnitude response in dB at `freq` (for the EQ curve).
    pub fn response_db(&self, freq: f64, rate: f64) -> f64 {
        let w = 2.0 * PI * freq / rate;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -(self.b1 * s1 + self.b2 * s2);
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -(self.a1 * s1 + self.a2 * s2);
        10.0 * ((nr * nr + ni * ni) / (dr * dr + di * di)).log10()
    }
}

/// Transposed direct form II state for one channel.
#[derive(Debug, Clone, Copy, Default)]
pub struct State {
    z1: f64,
    z2: f64,
}

impl State {
    #[inline]
    pub fn run(&mut self, c: &Coefs, x: f64) -> f64 {
        let y = c.b0 * x + self.z1;
        self.z1 = c.b1 * x - c.a1 * y + self.z2;
        self.z2 = c.b2 * x - c.a2 * y;
        y
    }

    pub fn reset(&mut self) {
        *self = State::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peaking_at_zero_db_is_exact_identity() {
        let c = Coefs::new(Kind::Peaking, 1000.0, 1.0, 0.0, 44100.0);
        let mut s = State::default();
        for i in 0..1000 {
            let x = ((i * 7919) % 2001) as f64 / 1000.0 - 1.0;
            assert_eq!(s.run(&c, x), x);
        }
    }

    #[test]
    fn responses() {
        let r = 48000.0;
        let peak = Coefs::new(Kind::Peaking, 1000.0, 1.0, 6.0, r);
        assert!((peak.response_db(1000.0, r) - 6.0).abs() < 1e-9);
        let lp = Coefs::new(Kind::LowPass, 1000.0, 0.0, 0.0, r);
        // Q = 0 dB means a linear Q of 1: exactly 0 dB at the cutoff, a small bump below it
        assert!(lp.response_db(1000.0, r).abs() < 1e-6);
        assert!(lp.response_db(100.0, r).abs() < 0.15);
        assert!(lp.response_db(10000.0, r) < -35.0);
        let ls = Coefs::new(Kind::LowShelf, 100.0, 1.0, 6.0, r);
        assert!((ls.response_db(10.0, r) - 6.0).abs() < 0.05);
        assert!((ls.response_db(100.0, r) - 3.0).abs() < 0.01);
        let hs = Coefs::new(Kind::HighShelf, 5000.0, 1.0, -6.0, r);
        assert!((hs.response_db(20000.0, r) + 6.0).abs() < 0.2);
        let n = Coefs::new(Kind::Notch, 1000.0, 1.0, 0.0, r);
        assert!(n.response_db(1000.0, r) < -100.0);
    }
}
