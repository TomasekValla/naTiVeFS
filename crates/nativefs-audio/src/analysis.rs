//! Track analysis for files the server did not analyse (local files): integrated loudness (EBU R128,
//! the same measurement as the server's `ffmpeg ebur128`), used by loudness normalization.

use ebur128::{EbuR128, Mode};

use crate::decode::Decoder;

/// Integrated loudness in LUFS of the rest of `dec` (decodes it to the end). `None` for silence or
/// unsupported channel counts.
pub fn integrated_loudness(dec: &mut Decoder) -> Option<f64> {
    scan(dec, |_| {}).0
}

/// Decodes the rest of `dec` once, feeding the loudness meter and `each` with every decoded chunk.
/// Returns the integrated loudness (`None` on silence, decode errors or unsupported channel counts) and whether
/// the whole file decoded.
fn scan(dec: &mut Decoder, mut each: impl FnMut(&[i32])) -> (Option<f64>, bool) {
    let info = dec.info().clone();
    let mut meter = EbuR128::new(info.channels as u32, info.rate, Mode::I).ok();
    let mut buf = Vec::new();
    let complete = loop {
        buf.clear();
        match dec.next(&mut buf) {
            Ok(true) => {
                if meter.as_mut().is_some_and(|m| m.add_frames_i32(&buf).is_err()) {
                    meter = None;
                }
                each(&buf);
            }
            Ok(false) => break true,
            Err(_) => break false,
        }
    };
    let lufs = meter.filter(|_| complete).and_then(|m| m.loudness_global().ok()).filter(|l| l.is_finite());
    (lufs, complete)
}

/// One pass over `dec` for a voice note: integrated loudness plus `bars` waveform values (0..1). The values are the
/// RMS of ~46 ms blocks, the largest one per bar, scaled so a high percentile (not the single loudest block) is 1:
/// one click or pop does not flatten the rest of the note. `None` loudness on decode errors.
pub fn voice_scan(dec: &mut Decoder, bars: usize) -> (Option<f64>, Vec<f32>) {
    let info = dec.info().clone();
    let ch = (info.channels as usize).max(1);
    let block = (info.rate as usize / 21).max(256) * ch;
    let rms = |chunk: &[i32]| {
        let sum: f64 = chunk.iter().map(|v| (*v as f64 / 2_147_483_648.0).powi(2)).sum();
        (sum / chunk.len().max(1) as f64).sqrt() as f32
    };
    let (mut pending, mut levels) = (Vec::<i32>::new(), Vec::<f32>::new());
    let (lufs, _) = scan(dec, |buf| {
        // whole blocks straight from the chunk; only the remainder is kept (no shifting of a growing buffer)
        let mut rest = buf;
        if !pending.is_empty() {
            let take = (block - pending.len()).min(rest.len());
            pending.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if pending.len() == block {
                levels.push(rms(&pending));
                pending.clear();
            }
        }
        let mut blocks = rest.chunks_exact(block);
        levels.extend(blocks.by_ref().map(rms));
        pending.extend_from_slice(blocks.remainder());
    });
    if !pending.is_empty() {
        levels.push(rms(&pending));
    }
    (lufs, waveform(&levels, bars))
}

/// Block levels -> `bars` values (the largest level per bar), normalised to the 95th percentile of the bars.
fn waveform(levels: &[f32], bars: usize) -> Vec<f32> {
    let bars = bars.max(1);
    let mut out: Vec<f32> = (0..bars)
        .map(|i| {
            let (a, b) = (i * levels.len() / bars, ((i + 1) * levels.len() / bars).max(i * levels.len() / bars + 1));
            levels.get(a..b.min(levels.len())).map_or(0.0, |s| s.iter().copied().fold(0.0, f32::max))
        })
        .collect();
    let mut sorted = out.clone();
    sorted.sort_by(f32::total_cmp);
    let top = sorted[((sorted.len() - 1) as f32 * 0.95).round() as usize];
    if top > 0.0 {
        out.iter_mut().for_each(|v| *v = (*v / top).min(1.0));
    }
    out
}

/// The web's `AnalyserNode` (fftSize 2048, Blackman window, smoothingTimeConstant 0.55,
/// `getFloatFrequencyData`): magnitudes per bin in dB.
pub struct Spectrum {
    fft: std::sync::Arc<dyn rustfft::Fft<f32>>,
    window: Vec<f32>,
    smooth: Vec<f32>,
    buf: Vec<rustfft::num_complex::Complex<f32>>,
    pub db: Vec<f32>,
    smoothing: f32,
}

pub const FFT_SIZE: usize = 2048;

impl Default for Spectrum {
    fn default() -> Self {
        Self::new()
    }
}

impl Spectrum {
    pub fn new() -> Self {
        Self::with(FFT_SIZE, 0.55)
    }

    /// Any power-of-two size and smoothing (the visualizer's chromagram uses 16384 / 0.3).
    pub fn with(n: usize, smoothing: f32) -> Self {
        let window = (0..n)
            .map(|i| {
                let x = i as f32 / n as f32;
                0.42 - 0.5 * (2.0 * std::f32::consts::PI * x).cos() + 0.08 * (4.0 * std::f32::consts::PI * x).cos()
            })
            .collect();
        Spectrum {
            fft: rustfft::FftPlanner::new().plan_fft_forward(n),
            window,
            smooth: vec![0.0; n / 2],
            buf: vec![rustfft::num_complex::Complex::new(0.0, 0.0); n],
            db: vec![-140.0; n / 2],
            smoothing,
        }
    }

    pub fn size(&self) -> usize {
        self.buf.len()
    }

    /// Analyse the newest `FFT_SIZE` samples (fewer are zero-padded at the start).
    pub fn update(&mut self, samples: &[f32]) {
        let n = self.buf.len();
        let off = n.saturating_sub(samples.len());
        for i in 0..n {
            let v = if i >= off { samples[i - off] } else { 0.0 };
            self.buf[i] = rustfft::num_complex::Complex::new(v * self.window[i], 0.0);
        }
        self.fft.process(&mut self.buf);
        for k in 0..n / 2 {
            let m = self.buf[k].norm() / n as f32;
            self.smooth[k] = self.smoothing * self.smooth[k] + (1.0 - self.smoothing) * m;
            self.db[k] = if self.smooth[k] > 0.0 { 20.0 * self.smooth[k].log10() } else { -140.0 };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_click_does_not_flatten_the_waveform() {
        let mut levels = vec![0.1f32; 400];
        levels[200] = 1.0;
        let bars = waveform(&levels, 40);
        assert_eq!(bars.len(), 40);
        // the click is clipped to the top, the speech stays at full height instead of 0.1
        assert!(bars.iter().all(|b| (*b - 1.0).abs() < 1e-6), "{bars:?}");
        assert_eq!(waveform(&[], 8), vec![0.0; 8]);
    }

    #[test]
    fn sine_peak_where_the_web_analyser_puts_it() {
        let rate = 48000.0;
        let f = 1000.0;
        let s: Vec<f32> = (0..FFT_SIZE).map(|i| (2.0 * std::f32::consts::PI * f * i as f32 / rate).sin()).collect();
        let mut sp = Spectrum::new();
        for _ in 0..40 {
            sp.update(&s);
        }
        let bin = (f / (rate / FFT_SIZE as f32)).round() as usize;
        let peak = sp.db.iter().cloned().fold(f32::MIN, f32::max);
        assert!((sp.db[bin] - peak).abs() < 1.5, "peak near the 1 kHz bin");
        // full-scale sine: |X| / N with a Blackman window = 0.42 / 2 -> about -13.5 dB
        assert!((peak + 13.5).abs() < 1.5, "{peak}");
    }
}
