//! The web's animated "screensaver" background (`#screensaver-canvas` in
//! `upload.html`), ported 1:1: the same orbs, drift/pulse motion, gradient stops
//! and `screen` / `multiply` compositing. Slint has no blend modes, so it is
//! rendered on the CPU into a small texture that the GPU stretches over the
//! window (the orbs are soft, a low resolution is invisible after bilinear scaling).
//!
//! A second texture is the same frame through CSS `saturate(150%)`: that is
//! what a glass card (`backdrop-filter: blur() saturate(150%)`) sees behind itself.

use std::f32::consts::TAU;

use slint::{Rgb8Pixel, SharedPixelBuffer};

/// Logical pixels per texture pixel.
pub const DOWNSCALE: f32 = 6.0;
const GLASS_SATURATION: f32 = 1.5;

#[derive(Clone, Copy)]
struct OrbDef {
    rgb: [f32; 3],
    base_r: f32,
    base_opacity: f32,
}

const fn o(r: f32, g: f32, b: f32, base_r: f32, base_opacity: f32) -> OrbDef {
    OrbDef { rgb: [r, g, b], base_r, base_opacity }
}

// Large background orbs (radius is a fraction of the window diagonal).
const DARK_BG: [OrbDef; 6] = [
    o(0.04, 0.54, 0.47, 0.72, 0.66), // deep teal
    o(0.38, 0.10, 0.78, 0.68, 0.62), // rich purple
    o(0.02, 0.40, 0.58, 0.60, 0.54), // blue
    o(0.55, 0.08, 0.55, 0.55, 0.49), // magenta-violet
    o(0.00, 0.62, 0.50, 0.58, 0.45), // green-teal
    o(0.22, 0.05, 0.44, 0.65, 0.58), // indigo dark
];
// Mid orbs: brighter patches.
const DARK_MID: [OrbDef; 5] = [
    o(0.00, 0.90, 0.76, 0.38, 0.56), // bright cyan
    o(0.68, 0.20, 1.00, 0.34, 0.53), // vivid violet
    o(0.00, 0.72, 0.60, 0.32, 0.48), // teal
    o(1.00, 0.30, 0.72, 0.28, 0.37), // hot pink
    o(0.30, 0.80, 1.00, 0.30, 0.40), // sky blue
];
// Near-black spots, multiplied in to punch dark holes.
const DARK_SPOTS: [OrbDef; 2] = [o(0.00, 0.02, 0.04, 0.28, 0.90), o(0.01, 0.00, 0.06, 0.22, 0.85)];
const LIGHT_BG: [OrbDef; 4] = [
    o(0.49, 0.91, 0.83, 0.70, 0.55),
    o(0.76, 0.61, 1.00, 0.65, 0.50),
    o(0.49, 0.82, 1.00, 0.60, 0.45),
    o(0.90, 0.70, 1.00, 0.55, 0.40),
];
const DARK_BASE: [f32; 3] = [0x01 as f32 / 255.0, 0x06 as f32 / 255.0, 0x05 as f32 / 255.0];
const LIGHT_BASE: [f32; 3] = [0xe8 as f32 / 255.0, 0xf7 as f32 / 255.0, 0xf4 as f32 / 255.0];

#[derive(Clone, Copy, PartialEq)]
enum Blend {
    Screen,
    Multiply,
}

struct Orb {
    def: OrbDef,
    blend: Blend,
    x: f32,
    y: f32,
    drift_amp_x: f32,
    drift_amp_y: f32,
    drift_speed_x: f32,
    drift_speed_y: f32,
    phase_x: f32,
    phase_y: f32,
    pulse_speed: f32,
    pulse_phase: f32,
    pulse_amp: f32,
    size_speed: f32,
    size_phase: f32,
    size_amp: f32,
}

/// Tiny xorshift so the layout differs per run like the web's `Math.random()`.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
    fn range(&mut self, a: f32, b: f32) -> f32 {
        a + self.next() * (b - a)
    }
}

pub struct Backdrop {
    light: bool,
    orbs: Vec<Orb>,
    /// Animation time in ms (the web's `t`).
    t: f64,
    rng: Rng,
}

/// Orb centre, opacity and radius at time `t` in window fractions / logical px.
struct Placed {
    cx: f32,
    cy: f32,
    r: f32,
    opacity: f32,
    rgb: [f32; 3],
    blend: Blend,
}

impl Backdrop {
    pub fn new(light: bool, seed: u64) -> Self {
        let mut b = Self { light, orbs: Vec::new(), t: 0.0, rng: Rng(seed | 1) };
        b.init_orbs();
        b
    }

    pub fn set_light(&mut self, light: bool) {
        if self.light != light {
            self.light = light;
            self.init_orbs();
        }
    }

    fn init_orbs(&mut self) {
        let mut orbs = Vec::new();
        let groups: [(&[OrbDef], f32, Blend); 3] = if self.light {
            [(&LIGHT_BG, 0.8, Blend::Multiply), (&[], 1.3, Blend::Screen), (&[], 1.0, Blend::Multiply)]
        } else {
            [(&DARK_BG, 0.8, Blend::Screen), (&DARK_MID, 1.3, Blend::Screen), (&DARK_SPOTS, 1.0, Blend::Multiply)]
        };
        for (defs, speed, blend) in groups {
            for def in defs {
                let r = &mut self.rng;
                orbs.push(Orb {
                    def: *def,
                    blend,
                    x: r.range(0.05, 0.95),
                    y: r.range(0.05, 0.95),
                    drift_amp_x: r.range(0.08, 0.20),
                    drift_amp_y: r.range(0.08, 0.20),
                    drift_speed_x: r.range(0.00010, 0.00030) * speed,
                    drift_speed_y: r.range(0.00008, 0.00025) * speed,
                    phase_x: r.range(0.0, TAU),
                    phase_y: r.range(0.0, TAU),
                    pulse_speed: r.range(0.00025, 0.00060),
                    pulse_phase: r.range(0.0, TAU),
                    pulse_amp: r.range(0.08, 0.22),
                    size_speed: r.range(0.00015, 0.00040),
                    size_phase: r.range(0.0, TAU),
                    size_amp: r.range(0.06, 0.16),
                });
            }
        }
        self.orbs = orbs;
    }

    /// Advance the animation by `dt_ms` (the web advances by the frame delta).
    pub fn advance(&mut self, dt_ms: f64) {
        self.t += dt_ms;
    }

    fn placed(&self, w: f32, h: f32) -> Vec<Placed> {
        // f64 phase math so precision does not degrade after hours of uptime
        let t = self.t;
        let diag = (w * w + h * h).sqrt();
        self.orbs
            .iter()
            .map(|o| {
                let s = |speed: f32, phase: f32| (t * speed as f64 + phase as f64).sin() as f32;
                let c = |speed: f32, phase: f32| (t * speed as f64 + phase as f64).cos() as f32;
                let cx = (o.x + s(o.drift_speed_x, o.phase_x) * o.drift_amp_x) * w;
                let cy = (o.y + c(o.drift_speed_y, o.phase_y) * o.drift_amp_y) * h;
                let op_mod = 1.0 + s(o.pulse_speed, o.pulse_phase) * o.pulse_amp;
                let opacity = (o.def.base_opacity * op_mod).clamp(0.02, 1.0);
                let r_mod = 1.0 + s(o.size_speed, o.size_phase) * o.size_amp;
                Placed { cx, cy, r: o.def.base_r * r_mod * diag, opacity, rgb: o.def.rgb, blend: o.blend }
            })
            .collect()
    }

    /// Render one frame for a window of `w` x `h` logical px.
    /// Returns (page background, saturated copy for glass cards).
    pub fn render(&self, w: f32, h: f32) -> (SharedPixelBuffer<Rgb8Pixel>, SharedPixelBuffer<Rgb8Pixel>) {
        let tw = ((w / DOWNSCALE).ceil() as u32).max(2);
        let th = ((h / DOWNSCALE).ceil() as u32).max(2);
        // texture pixel -> logical px
        let sx = w / tw as f32;
        let sy = h / th as f32;
        let orbs = self.placed(w, h);
        let base = if self.light { LIGHT_BASE } else { DARK_BASE };

        let mut plain = SharedPixelBuffer::<Rgb8Pixel>::new(tw, th);
        let mut glass = SharedPixelBuffer::<Rgb8Pixel>::new(tw, th);
        let (p, g) = (plain.make_mut_slice(), glass.make_mut_slice());
        let n = tw as usize;
        // one row at a time, channels in separate arrays and no branches in the
        // inner loops, so they compile to SIMD
        let xs: Vec<f32> = (0..n).map(|tx| (tx as f32 + 0.5) * sx).collect();
        let (mut rr, mut gg, mut bb) = (vec![0f32; n], vec![0f32; n], vec![0f32; n]);
        let mut alpha = vec![0f32; n];
        for ty in 0..th {
            let py = (ty as f32 + 0.5) * sy;
            rr.fill(base[0]);
            gg.fill(base[1]);
            bb.fill(base[2]);
            // same order as the canvas: each orb is composited over the result so far
            for orb in &orbs {
                let dy = py - orb.cy;
                if dy * dy >= orb.r * orb.r {
                    continue;
                }
                let (dy2, inv_r, op) = (dy * dy, 1.0 / orb.r, orb.opacity);
                for (a, x) in alpha.iter_mut().zip(&xs) {
                    let dx = x - orb.cx;
                    *a = op * stop_alpha_fast((dx * dx + dy2).sqrt() * inv_r);
                }
                let [sr, sg, sb] = orb.rgb;
                match orb.blend {
                    // dst + (screen(dst, src) - dst) * a  ==  dst + src * (1 - dst) * a
                    Blend::Screen => {
                        for (c, a) in rr.iter_mut().zip(&alpha) {
                            *c += sr * (1.0 - *c) * a;
                        }
                        for (c, a) in gg.iter_mut().zip(&alpha) {
                            *c += sg * (1.0 - *c) * a;
                        }
                        for (c, a) in bb.iter_mut().zip(&alpha) {
                            *c += sb * (1.0 - *c) * a;
                        }
                    }
                    // dst + (dst * src - dst) * a  ==  dst * (1 - (1 - src) * a)
                    Blend::Multiply => {
                        for (c, a) in rr.iter_mut().zip(&alpha) {
                            *c *= 1.0 - (1.0 - sr) * a;
                        }
                        for (c, a) in gg.iter_mut().zip(&alpha) {
                            *c *= 1.0 - (1.0 - sg) * a;
                        }
                        for (c, a) in bb.iter_mut().zip(&alpha) {
                            *c *= 1.0 - (1.0 - sb) * a;
                        }
                    }
                }
            }
            let start = ty as usize * n;
            for i in 0..n {
                let c = [rr[i], gg[i], bb[i]];
                p[start + i] = to_px(c);
                g[start + i] = to_px(saturate(c, GLASS_SATURATION));
            }
        }
        (plain, glass)
    }
}

/// `stop_alpha` without branches (sum of hinge functions, same piecewise-linear curve).
#[inline(always)]
fn stop_alpha_fast(t: f32) -> f32 {
    let h = |k: f32| (t - k).max(0.0);
    (1.0 - t - (1.0 / 3.0) * h(0.25) + (1.0 / 3.0) * h(0.55) + 0.5 * h(0.80)).max(0.0)
}

/// Alpha multiplier of the web's radial gradient: 1 / .75@.25 / .35@.55 / .10@.8 / 0@1
/// (reference for `stop_alpha_fast`).
#[cfg(test)]
fn stop_alpha(t: f32) -> f32 {
    const STOPS: [(f32, f32); 5] = [(0.0, 1.0), (0.25, 0.75), (0.55, 0.35), (0.80, 0.10), (1.0, 0.0)];
    for w in STOPS.windows(2) {
        let ((t0, a0), (t1, a1)) = (w[0], w[1]);
        if t <= t1 {
            return a0 + (a1 - a0) * ((t - t0) / (t1 - t0));
        }
    }
    0.0
}

/// CSS `saturate()` filter matrix (Filter Effects spec, feColorMatrix type=saturate).
fn saturate(c: [f32; 3], s: f32) -> [f32; 3] {
    let [r, g, b] = c;
    [
        (0.213 + 0.787 * s) * r + (0.715 - 0.715 * s) * g + (0.072 - 0.072 * s) * b,
        (0.213 - 0.213 * s) * r + (0.715 + 0.285 * s) * g + (0.072 - 0.072 * s) * b,
        (0.213 - 0.213 * s) * r + (0.715 - 0.715 * s) * g + (0.072 + 0.928 * s) * b,
    ]
}

fn to_px(c: [f32; 3]) -> Rgb8Pixel {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    Rgb8Pixel { r: q(c[0]), g: q(c[1]), b: q(c[2]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gradient_stops() {
        assert_eq!(stop_alpha(0.0), 1.0);
        assert!((stop_alpha(0.25) - 0.75).abs() < 1e-6);
        assert!((stop_alpha(0.4) - 0.55).abs() < 1e-6);
        assert_eq!(stop_alpha(1.0), 0.0);
    }

    #[test]
    fn fast_curve_matches_stops() {
        for i in 0..=1200 {
            let t = i as f32 / 1000.0;
            assert!((stop_alpha(t) - stop_alpha_fast(t)).abs() < 1e-5, "t={t}");
        }
    }

    #[test]
    fn saturate_keeps_grey() {
        let g = saturate([0.5, 0.5, 0.5], 1.5);
        assert!(g.iter().all(|v| (v - 0.5).abs() < 1e-3));
    }

    #[test]
    fn renders_both_themes() {
        for light in [false, true] {
            let mut b = Backdrop::new(light, 42);
            b.advance(1234.0);
            let (p, g) = b.render(520.0, 900.0);
            assert_eq!((p.width(), p.height()), (87, 150));
            assert_eq!((g.width(), g.height()), (87, 150));
        }
    }
}

#[cfg(test)]
mod bench {
    #[test]
    #[ignore]
    fn render_cost() {
        let mut b = super::Backdrop::new(false, 42);
        let t = std::time::Instant::now();
        for _ in 0..1000 {
            b.advance(4.0);
            std::hint::black_box(b.render(520.0, 900.0));
        }
        eprintln!("render: {:?} per frame", t.elapsed() / 1000);
    }
}
