//! Track analysis for files the server did not analyse (local files): integrated loudness (EBU R128,
//! the same measurement as the server's `ffmpeg ebur128`), used by loudness normalization.

use ebur128::{EbuR128, Mode};

use crate::decode::Decoder;

/// Integrated loudness in LUFS of the rest of `dec` (decodes it to the end). `None` for silence or
/// unsupported channel counts.
pub fn integrated_loudness(dec: &mut Decoder) -> Option<f64> {
    let info = dec.info().clone();
    let mut meter = EbuR128::new(info.channels as u32, info.rate, Mode::I).ok()?;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match dec.next(&mut buf) {
            Ok(true) => meter.add_frames_i32(&buf).ok()?,
            Ok(false) => break,
            Err(_) => return None,
        }
    }
    meter.loudness_global().ok().filter(|l| l.is_finite())
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
