//! The web player's particle visualizer (`audioPlayerClient/player.js`, "Particle visualizer"):
//! a field of small capsules on loose rings around the player card, coloured by angle and tinted
//! by the key of the music (a chromagram on the circle of fifths). Bass hits travel outward as a
//! pressure wave, a drop fires a big one and spins the field, quiet passages calm it, every
//! direction listens to its own slice of the spectrum, the pointer pushes particles aside.
//! Rendered on the CPU into an RGBA buffer (Slint has no canvas): about 2200 anti-aliased quads.

use std::f32::consts::PI;

use crate::analysis::Spectrum;

const PALETTE: [u32; 10] = [0x2fffc4, 0x36d6ff, 0x4f8dff, 0x7a63ff, 0xa95cff, 0xe05cf0, 0xff6fb1, 0xffa07a, 0xffd27a, 0x7dffa8];
const HUES: usize = 40;
const TIERS: usize = 3;
const BANDS: usize = 48;
const HIST: usize = 240;
/// px / s the bass wave travels outward
const WAVE_SPEED: f32 = 620.0;

fn palette_at(u: f32) -> [f32; 3] {
    let u = u.rem_euclid(1.0);
    let x = u * PALETTE.len() as f32;
    let i = x.floor() as usize % PALETTE.len();
    let f = x - x.floor();
    let c = |v: u32| [((v >> 16) & 255) as f32, ((v >> 8) & 255) as f32, (v & 255) as f32];
    let (a, b) = (c(PALETTE[i]), c(PALETTE[(i + 1) % PALETTE.len()]));
    [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f]
}

fn hsl(h: f32, s: f32, l: f32) -> [f32; 3] {
    let h = h.rem_euclid(360.0) / 360.0;
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let ch = |t: f32| {
        let t = (t + 1.0) % 1.0;
        255.0 * if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [ch(h + 1.0 / 3.0), ch(h), ch(h - 1.0 / 3.0)]
}

/// Small deterministic random numbers for the field layout.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

struct Particle {
    r0: f32,
    th0: f32,
    sz: f32,
    band: usize,
    seed: f32,
    push: f32,
    dpush: f32,
    bucket: usize,
}

/// What the music is doing (the web's `analyse`).
pub struct Analysis {
    spec: Spectrum,
    chroma_spec: Spectrum,
    chroma_bin: Vec<i8>,
    chroma_frame: u32,
    rate: f32,
    band_val: [f32; BANDS],
    band_max: [f32; BANDS],
    hist: [f32; HIST],
    hist_head: usize,
    hist_acc: f32,
    pub bass_smooth: f32,
    prev_bass: f32,
    flux_avg: f32,
    pub onset: f32,
    last_onset: f32,
    pub level: f32,
    lvl_short: f32,
    lvl_long: f32,
    play_time: f32,
    last_drop: f32,
    pub drop: f32,
    calm: f32,
    spin: f32,
    key_x: f32,
    key_y: f32,
    key_hue: f32,
    key_clarity: f32,
    now: f32,
}

impl Analysis {
    pub fn new(rate: u32) -> Self {
        let rate = rate as f32;
        let chroma_spec = Spectrum::with(16384, 0.3);
        let hz = rate / 16384.0;
        let mut chroma_bin = vec![-1i8; 8192];
        for (i, b) in chroma_bin.iter_mut().enumerate() {
            let f = i as f32 * hz;
            if (90.0..4200.0).contains(&f) {
                let midi = 69.0 + 12.0 * (f / 440.0).log2();
                if (midi - midi.round()).abs() < 0.35 {
                    *b = (midi.round() as i32).rem_euclid(12) as i8;
                }
            }
        }
        Analysis {
            spec: Spectrum::new(),
            chroma_spec,
            chroma_bin,
            chroma_frame: 0,
            rate,
            band_val: [0.0; BANDS],
            band_max: [0.2; BANDS],
            hist: [0.0; HIST],
            hist_head: 0,
            hist_acc: 0.0,
            bass_smooth: 0.0,
            prev_bass: 0.0,
            flux_avg: 0.01,
            onset: 0.0,
            last_onset: -1e9,
            level: 0.0,
            lvl_short: 0.0,
            lvl_long: 0.0,
            play_time: 0.0,
            last_drop: -1e9,
            drop: 0.0,
            calm: 1.0,
            spin: 0.0,
            key_x: 0.0,
            key_y: 0.0,
            key_hue: 0.0,
            key_clarity: 0.0,
            now: 0.0,
        }
    }

    fn analyse_chroma(&mut self, samples: &[f32], dt: f32) {
        self.chroma_spec.update(samples);
        let db = &self.chroma_spec.db;
        let mut chroma = [0.0f32; 12];
        for i in 2..db.len() - 2 {
            let pc = self.chroma_bin[i];
            if pc < 0 {
                continue;
            }
            let v = db[i];
            // only spectral peaks count (tones), not the broadband wash of drums
            if v < db[i - 1] || v < db[i + 1] || v < -90.0 {
                continue;
            }
            if v - (db[i - 2] + db[i + 2]) / 2.0 < 6.0 {
                continue;
            }
            chroma[pc as usize] += 10f32.powf(v / 20.0);
        }
        let (mut sx, mut sy, mut tot) = (0.0, 0.0, 0.0);
        for (c, v) in chroma.iter().enumerate() {
            let a = ((c * 7) % 12) as f32 * PI / 6.0;
            sx += v * a.cos();
            sy += v * a.sin();
            tot += v;
        }
        if tot <= 0.0 {
            return;
        }
        let k = 1.0 - (-dt / 0.6).exp();
        self.key_x += (sx / tot - self.key_x) * k;
        self.key_y += (sy / tot - self.key_y) * k;
        self.key_hue = (self.key_y.atan2(self.key_x).to_degrees() + 360.0) % 360.0;
        self.key_clarity = self.key_x.hypot(self.key_y);
    }

    /// One frame. `samples`: the newest mono samples (at least 16384 for the chromagram), `None`
    /// while paused. `bpm` / `speed` set the onset cooldown like the web.
    pub fn update(&mut self, samples: Option<&[f32]>, dt: f32, bpm: Option<f64>, speed: f32) {
        self.now += dt * 1000.0;
        let now = self.now;
        if let Some(samples) = samples {
            let short = &samples[samples.len().saturating_sub(2048)..];
            self.spec.update(short);
            let hz_per_bin = self.rate / 2048.0;
            let mut lvl = 0.0;
            for b in 0..BANDS {
                let edge = |b: usize| ((35.0 * (15000.0f32 / 35.0).powf(b as f32 / BANDS as f32) / hz_per_bin).round() as usize).max(1);
                let lo = edge(b);
                let hi = edge(b + 1).max(lo + 1).min(self.spec.db.len());
                // getByteFrequencyData: -100..-30 dB onto 0..255
                let s: f32 = self.spec.db[lo.min(hi - 1)..hi].iter().map(|d| ((d + 100.0) / 70.0).clamp(0.0, 1.0)).sum();
                let v = s / (hi - lo.min(hi - 1)) as f32;
                self.band_max[b] = v.max(self.band_max[b] * (1.0 - dt * 0.25)).max(0.12);
                let nv = (v / self.band_max[b]).powf(1.6);
                self.band_val[b] += (nv - self.band_val[b]) * if nv > self.band_val[b] { 0.55 } else { 0.12 };
                lvl += v;
            }
            lvl /= BANDS as f32;
            self.level += (lvl - self.level) * 0.1;
            self.play_time += dt;
            self.lvl_short += (lvl - self.lvl_short) * (1.0 - (-dt / 0.3).exp());
            let long_tc = if self.play_time < 4.0 { 0.8 } else { 7.0 };
            self.lvl_long += (lvl - self.lvl_long) * (1.0 - (-dt / long_tc).exp());
            let ratio = self.lvl_short / self.lvl_long.max(0.02);
            self.calm += (((ratio - 0.5) / 0.6).clamp(0.35, 1.2) - self.calm) * (1.0 - (-dt / 0.5).exp());
            self.chroma_frame += 1;
            if self.chroma_frame.is_multiple_of(3) && samples.len() >= 16384 {
                self.analyse_chroma(&samples[samples.len() - 16384..], dt * 3.0);
            }
            let bass = self.band_val[..7].iter().sum::<f32>() / 7.0;
            self.bass_smooth += (bass - self.bass_smooth) * 0.25;
            let flux = (bass - self.prev_bass).max(0.0);
            self.prev_bass = bass;
            self.flux_avg = self.flux_avg * 0.94 + flux * 0.06;
            let cooldown = bpm.filter(|b| *b > 0.0).map_or(160.0, |b| (60000.0 / b as f32 * 0.45 / speed.max(0.1)).max(130.0));
            if flux > self.flux_avg * 2.0 + 0.03 && now - self.last_onset > cooldown {
                self.onset = self.onset.max((0.35 + flux * 3.0).min(1.0));
                self.last_onset = now;
                if self.play_time > 3.0 && ratio > 1.3 && now - self.last_drop > 4000.0 {
                    // the drop
                    self.onset = 1.9;
                    self.drop = 1.0;
                    self.spin = 1.0;
                    self.calm = 1.2;
                    self.last_drop = now;
                }
            }
        } else {
            for v in self.band_val.iter_mut() {
                *v *= 0.92;
            }
            self.bass_smooth *= 0.95;
            self.level *= 0.96;
            self.prev_bass = 0.0;
            self.calm += (0.6 - self.calm) * 0.02;
            self.key_clarity *= 0.99;
        }
        self.onset *= 0.02f32.powf(dt);
        self.drop *= 0.08f32.powf(dt);
        self.spin *= 0.15f32.powf(dt);
        // fixed 60 Hz history of the "pressure"; radius r reads it r / WAVE_SPEED seconds late
        self.hist_acc += dt;
        while self.hist_acc >= 1.0 / 60.0 {
            self.hist_acc -= 1.0 / 60.0;
            self.hist_head = (self.hist_head + 1) % HIST;
            self.hist[self.hist_head] = (self.bass_smooth * 0.45 + self.onset) * self.calm;
        }
    }

    /// The disc pulses on hits (the web's disc scale).
    pub fn disc_scale(&self) -> f32 {
        1.0 + self.onset * 0.05 + self.bass_smooth * 0.03 + self.drop * 0.05
    }

    /// Everything has faded out after a pause (the window may stop redrawing).
    pub fn settled(&self) -> bool {
        self.onset < 1e-3 && self.drop < 1e-3 && self.spin < 1e-3 && self.bass_smooth < 1e-3 && self.hist.iter().all(|h| *h < 1e-3)
    }
}

/// The particle field and its frame buffer.
pub struct Field {
    pub width: usize,
    pub height: usize,
    cx: f32,
    cy: f32,
    rmax: f32,
    particles: Vec<Particle>,
    /// Premultiplied RGBA.
    pub pixels: Vec<u8>,
    t: f32,
    swirl: f32,
}

impl Field {
    /// A field for a `w` x `h` px buffer (`scale` = device pixels per logical px), centred at
    /// (`cx`, `cy`) in logical px.
    pub fn new(w: usize, h: usize, scale: f32, cx: f32, cy: f32) -> Field {
        let (lw, lh) = (w as f32 / scale, h as f32 / scale);
        let rmax = (cx.max(lw - cx)).hypot(cy.max(lh - cy)) + 30.0;
        // constant density: a small window gets fewer particles (the web keeps at least half, which crowds them)
        let target = 2200.0 * (lw * lh / (1600.0 * 900.0)).clamp(0.12, 1.4);
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut spread = 1.0f32;
        let mut rings = Vec::new();
        for _ in 0..8 {
            rings.clear();
            let mut total = 0.0;
            let mut r = 22.0f32;
            while r < rmax {
                let dr = (10.0 + r * 0.045) * spread;
                let n = ((2.0 * PI * r / (dr * 1.45)).floor() as usize).max(6);
                rings.push((r, n, dr));
                total += n as f32;
                r += dr;
            }
            if total <= target * 1.1 {
                break;
            }
            spread *= (total / target).sqrt();
        }
        let mut particles = Vec::new();
        for (r, n, dr) in rings {
            let off = rng.next() * PI * 2.0;
            for i in 0..n {
                let th = off + (i as f32 + (rng.next() - 0.5) * 0.7) * PI * 2.0 / n as f32;
                let rr = r + (rng.next() - 0.5) * dr * 0.6;
                let s = ((rr - rmax * 0.06) / (rmax * 0.5)).clamp(0.0, 1.0);
                let sz = s * s * (3.0 - 2.0 * s);
                let from_bottom = th.cos().atan2(th.sin()).abs() / PI;
                let hue = (((th / (PI * 2.0)) % 1.0 + 1.0) % 1.0 * HUES as f32) as usize % HUES;
                let tier = ((sz * TIERS as f32) as usize).min(TIERS - 1);
                particles.push(Particle {
                    r0: rr,
                    th0: th,
                    sz,
                    band: ((from_bottom * BANDS as f32) as usize).min(BANDS - 1),
                    seed: rng.next() * PI * 2.0,
                    push: 0.0,
                    dpush: 0.0,
                    bucket: tier * HUES + hue,
                });
            }
        }
        Field { width: w, height: h, cx: cx * scale, cy: cy * scale, rmax, particles, pixels: vec![0; w * h * 4], t: 0.0, swirl: 0.0 }
    }

    /// Continue `old`'s animation (a new field after a resize must not restart the motion).
    pub fn carry_from(&mut self, old: &Field) {
        self.t = old.t;
        self.swirl = old.swirl;
    }

    pub fn count(&self) -> usize {
        self.particles.len()
    }

    /// Draw one frame. `mouse`: pointer in buffer px (pushes particles aside), `scale` as in `new`.
    pub fn draw(&mut self, a: &Analysis, dt: f32, scale: f32, mouse: Option<(f32, f32)>) {
        self.t += dt;
        self.pixels.iter_mut().for_each(|p| *p = 0);
        let pal_shift = self.t * 0.006;
        let activity = (a.level * 2.5).min(1.0);
        let key_mix = ((a.key_clarity - 0.08) / 0.27).clamp(0.0, 1.0) * 0.9 * (activity * 2.0).min(1.0);
        let mut colors = [[0.0f32; 4]; HUES * TIERS];
        for tier in 0..TIERS {
            let mix = [0.18, 0.62, 1.0][tier];
            let alpha = [0.32, 0.55, 0.8][tier] * (0.7 + 0.3 * activity);
            for h in 0..HUES {
                let mut c = palette_at(h as f32 / HUES as f32 + pal_shift);
                if key_mix > 0.01 {
                    let kc = hsl(a.key_hue + (h as f32 / HUES as f32 * PI * 2.0 + self.t * 0.15).sin() * 45.0, 0.9, 0.62);
                    for i in 0..3 {
                        c[i] += (kc[i] - c[i]) * key_mix;
                    }
                }
                let g = 150.0;
                colors[tier * HUES + h] = [(g + (c[0] - g) * mix) / 255.0, (g + (c[1] - g) * mix) / 255.0, (g + (c[2] - g) * mix) / 255.0, alpha];
            }
        }
        self.swirl += dt * (0.018 + a.spin * 0.9);
        let (w, h) = (self.width as f32, self.height as f32);
        let amp = 30.0 * (w.min(h) / scale / 800.0).max(1.0);
        for i in 0..self.particles.len() {
            let p = &mut self.particles[i];
            let hi = (a.hist_head + HIST - ((p.r0 / WAVE_SPEED * 60.0) as usize % HIST)) % HIST;
            let wave = a.hist[hi];
            let be = a.band_val[p.band] * a.calm;
            let push = wave * amp * (0.35 + 0.65 * p.sz) + (self.t * 0.9 + p.seed).sin() * 1.6;
            p.dpush += ((push - p.push) - p.dpush) * 0.3;
            p.push = push;
            let th = p.th0 + self.swirl * (0.5 + 0.8 * (1.0 - p.r0 / self.rmax));
            let (sn, cs) = th.sin_cos();
            let r = (p.r0 + push) * scale;
            let mut x = self.cx + cs * r;
            let mut y = self.cy + sn * r;
            if let Some((mx, my)) = mouse {
                let (dx, dy) = (x - mx, y - my);
                let md = dx * dx + dy * dy;
                let rad = 140.0 * scale;
                if md < rad * rad && md > 0.01 {
                    let dd = md.sqrt();
                    let f = 1.0 - dd / rad;
                    let f = f * f * 34.0 * scale;
                    x += dx / dd * f;
                    y += dy / dd * f;
                }
            }
            if x < -20.0 || y < -20.0 || x > w + 20.0 || y > h + 20.0 {
                continue;
            }
            // capsule along the ring, tipping outward while the wave pushes it
            let bend = (p.dpush * 0.5).clamp(-1.5, 1.5);
            let (mut dx, mut dy) = (-sn + cs * bend, cs + sn * bend);
            let dl = (dx * dx + dy * dy).sqrt();
            dx /= dl;
            dy /= dl;
            let hl = (0.6 + 4.6 * p.sz) * (1.0 + 1.6 * be + 0.9 * wave + a.drop * 0.9) * scale;
            let hw = (0.55 + 1.05 * p.sz) * (1.0 + 0.35 * be) * scale;
            let c = colors[p.bucket];
            fill_capsule(&mut self.pixels, self.width, self.height, x, y, dx, dy, hl, hw, c);
        }
    }
}

/// An anti-aliased rectangle centred at (x, y), half length `hl` along (dx, dy), half width `hw`,
/// blended "source over" into premultiplied RGBA.
#[allow(clippy::too_many_arguments)]
fn fill_capsule(px: &mut [u8], w: usize, h: usize, x: f32, y: f32, dx: f32, dy: f32, hl: f32, hw: f32, c: [f32; 4]) {
    let ext = hl * dx.abs() + hw * dy.abs() + 1.0;
    let eyt = hl * dy.abs() + hw * dx.abs() + 1.0;
    let x0 = ((x - ext).floor().max(0.0)) as usize;
    let x1 = ((x + ext).ceil().min(w as f32 - 1.0)) as isize;
    let y0 = ((y - eyt).floor().max(0.0)) as usize;
    let y1 = ((y + eyt).ceil().min(h as f32 - 1.0)) as isize;
    if x1 < x0 as isize || y1 < y0 as isize {
        return;
    }
    for py in y0..=y1 as usize {
        let fy = py as f32 + 0.5 - y;
        let row = py * w;
        for pxi in x0..=x1 as usize {
            let fx = pxi as f32 + 0.5 - x;
            // distance outside the rectangle along and across its axis
            let along = (fx * dx + fy * dy).abs() - hl;
            let across = (-fx * dy + fy * dx).abs() - hw;
            let cov = (0.5 - along).clamp(0.0, 1.0) * (0.5 - across).clamp(0.0, 1.0);
            if cov <= 0.0 {
                continue;
            }
            let a = c[3] * cov;
            let i = (row + pxi) * 4;
            let inv = 1.0 - a;
            px[i] = (c[0] * a * 255.0 + px[i] as f32 * inv) as u8;
            px[i + 1] = (c[1] * a * 255.0 + px[i + 1] as f32 * inv) as u8;
            px[i + 2] = (c[2] * a * 255.0 + px[i + 2] as f32 * inv) as u8;
            px[i + 3] = (a * 255.0 + px[i + 3] as f32 * inv) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_size_like_the_web_and_drawing_is_fast() {
        let mut f = Field::new(1120, 760, 1.0, 260.0, 340.0);
        assert!((1000..3200).contains(&f.count()), "{}", f.count());
        let mut a = Analysis::new(44100);
        let samples: Vec<f32> = (0..16384).map(|i| ((i as f32) * 0.05).sin() * 0.5).collect();
        let t = std::time::Instant::now();
        for _ in 0..30 {
            a.update(Some(&samples), 1.0 / 60.0, Some(123.0), 1.0);
            f.draw(&a, 1.0 / 60.0, 1.0, None);
        }
        let per_frame = t.elapsed() / 30;
        println!("{} particles, {:?} per frame", f.count(), per_frame);
        assert!(f.pixels.iter().any(|p| *p > 0), "something was drawn");
    }

    /// The key colour: a pitch class sits on the circle of fifths (C 0 deg = red, F# 180 = cyan, like
    /// the web), a chord takes the average direction of its notes (C major ~ 50 deg).
    #[test]
    fn key_colour_follows_the_music() {
        let rate = 44100.0f32;
        let tone = |freqs: &[f32]| -> Vec<f32> {
            (0..16384).map(|i| freqs.iter().map(|f| (2.0 * PI * f * i as f32 / rate).sin()).sum::<f32>() * 0.2).collect()
        };
        let hue_of = |samples: &[f32]| {
            let mut a = Analysis::new(rate as u32);
            for _ in 0..300 {
                a.update(Some(samples), 1.0 / 60.0, None, 1.0);
            }
            (a.key_hue, a.key_clarity)
        };
        let dist = |a: f32, b: f32| ((a - b + 540.0) % 360.0 - 180.0).abs();
        // expected direction of a set of pitch classes on the circle of fifths
        let expect = |pcs: &[usize]| {
            let (x, y) = pcs.iter().fold((0.0f32, 0.0f32), |(x, y), pc| {
                let a = ((pc * 7) % 12) as f32 * PI / 6.0;
                (x + a.cos(), y + a.sin())
            });
            (y.atan2(x).to_degrees() + 360.0) % 360.0
        };
        let (c, c_clear) = hue_of(&tone(&[261.63]));
        let (fs, _) = hue_of(&tone(&[369.99]));
        let (cmaj, _) = hue_of(&tone(&[261.63, 329.63, 392.0]));
        let (fsmaj, _) = hue_of(&tone(&[369.99, 466.16, 554.37]));
        println!("C {c:.0}, F# {fs:.0}, C major {cmaj:.0} (expected {:.0}), F# major {fsmaj:.0} (expected {:.0})", expect(&[0, 4, 7]), expect(&[6, 10, 1]));
        assert!(c_clear > 0.3, "a plain tone is a clear key");
        assert!(dist(c, 0.0) < 10.0, "C is red, got {c}");
        assert!(dist(fs, 180.0) < 10.0, "F# is cyan, got {fs}");
        assert!(dist(cmaj, expect(&[0, 4, 7])) < 12.0, "C major {cmaj}");
        assert!(dist(fsmaj, expect(&[6, 10, 1])) < 12.0, "F# major {fsmaj}");
    }

    #[test]
    fn analysis_settles_when_paused() {
        let mut a = Analysis::new(48000);
        let samples: Vec<f32> = (0..16384).map(|i| if i % 4800 < 400 { 0.9 } else { 0.0 }).collect();
        for _ in 0..120 {
            a.update(Some(&samples), 1.0 / 60.0, None, 1.0);
        }
        for _ in 0..600 {
            a.update(None, 1.0 / 60.0, None, 1.0);
        }
        assert!(a.settled());
    }
}
