//! Camera + microphone capture for Create mode ("Take a photo", "Record a video").
//! Windows: Media Foundation (capture, hardware H.264 + AAC into MP4). Linux: V4L2 camera, cpal
//! microphone, the system `ffmpeg` for encoding. macOS: not yet (the file picker stays).

use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod win;

#[cfg(target_os = "linux")]
use linux as imp;
#[cfg(windows)]
use win as imp;

/// Recordings stop by themselves at this size (owner: max 5 GB).
pub const MAX_RECORDING_BYTES: u64 = 5 * 1024 * 1024 * 1024;
/// Largest camera mode used (owner: up to 1080p30, ~8 Mbit/s).
pub const MAX_W: u32 = 1920;
pub const MAX_H: u32 = 1080;
pub const MAX_FPS: f64 = 30.5;
pub const VIDEO_BITRATE: u32 = 8_000_000;
pub const AUDIO_BITRATE: u32 = 192_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub id: String,
    pub name: String,
}

/// One camera frame, top-down BGRA (alpha 255).
#[derive(Clone)]
pub struct Frame {
    pub bgra: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
}

impl Frame {
    /// RGBA, mirrored (selfie view) and scaled down to at most `max_w` wide, for the preview.
    pub fn preview(&self, max_w: u32, mirror: bool) -> (Vec<u8>, u32, u32) {
        let step = self.width.div_ceil(max_w.max(1)).max(1);
        let (w, h) = (self.width / step, self.height / step);
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        let src = &self.bgra;
        for y in 0..h {
            let row = (y * step * self.width) as usize * 4;
            for x in 0..w {
                let sx = if mirror { self.width - 1 - x * step } else { x * step };
                let i = row + sx as usize * 4;
                out.extend_from_slice(&[src[i + 2], src[i + 1], src[i], 255]);
            }
        }
        (out, w, h)
    }

    /// RGB for JPEG encoding (the saved photo is not mirrored, like a phone's).
    pub fn to_rgb(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity((self.width * self.height * 3) as usize);
        for px in self.bgra.as_chunks::<4>().0 {
            out.extend_from_slice(&[px[2], px[1], px[0]]);
        }
        out
    }
}

pub enum Event {
    /// A new camera frame (full size; the UI scales it for the preview).
    Frame(Frame),
    /// Microphone level 0..1 (peak of the last ~50 ms).
    Level(f32),
    /// The camera mode in use, e.g. "1920 × 1080 · 30 fps".
    Mode(String),
    /// Capture failed or a device went away.
    Error(String),
}

pub type Sink = Arc<dyn Fn(Event) + Send + Sync>;

/// Microphone samples for a voice note: interleaved 16-bit PCM, sample rate, channels.
pub type PcmSink = Arc<dyn Fn(&[i16], u32, u16) + Send + Sync>;

/// The microphone alone (voice notes): PCM flows into `pcm` (levels into `sink`) until it is dropped.
pub struct Mic {
    #[cfg(any(windows, target_os = "linux"))]
    _inner: imp::Mic,
}

impl Mic {
    /// `id` from `microphones()` (empty = the first one).
    pub fn start(id: &str, pcm: PcmSink, sink: Sink) -> Result<Self, String> {
        #[cfg(any(windows, target_os = "linux"))]
        {
            Ok(Self { _inner: imp::Mic::start(id, pcm, sink)? })
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = (id, pcm, sink);
            Err("Voice recording is not supported on this system yet.".into())
        }
    }
}

pub fn cameras() -> Result<Vec<Device>, String> {
    #[cfg(any(windows, target_os = "linux"))]
    {
        imp::cameras()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Err("Camera capture is not supported on this system yet.".into())
    }
}

pub fn microphones() -> Result<Vec<Device>, String> {
    #[cfg(any(windows, target_os = "linux"))]
    {
        imp::microphones()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Err("Microphone capture is not supported on this system yet.".into())
    }
}

/// A running capture: camera frames (and microphone levels) flow into the sink until it is dropped.
pub struct Session {
    #[cfg(any(windows, target_os = "linux"))]
    inner: imp::Session,
}

impl Session {
    /// `camera` / `mic`: device ids from `cameras()` / `microphones()`; `None` = no microphone.
    pub fn start(camera: &str, mic: Option<&str>, sink: Sink) -> Result<Self, String> {
        #[cfg(any(windows, target_os = "linux"))]
        {
            Ok(Self { inner: imp::Session::start(camera, mic, sink)? })
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = (camera, mic, sink);
            Err("Camera capture is not supported on this system yet.".into())
        }
    }

    /// The newest full-size frame (the photo).
    pub fn latest(&self) -> Option<Frame> {
        #[cfg(any(windows, target_os = "linux"))]
        {
            self.inner.latest()
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            None
        }
    }

    /// Start writing an MP4 (H.264 + AAC) to `path`.
    pub fn start_recording(&self, path: &Path) -> Result<(), String> {
        #[cfg(any(windows, target_os = "linux"))]
        {
            self.inner.start_recording(path)
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = path;
            Err("not supported".into())
        }
    }

    /// Finish the file; returns its path.
    pub fn stop_recording(&self) -> Result<PathBuf, String> {
        #[cfg(any(windows, target_os = "linux"))]
        {
            self.inner.stop_recording()
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            Err("not supported".into())
        }
    }
}

/// Encode 16-bit PCM to AAC (`kbps` 96 / 160 / 192) in an M4A file: Media Foundation on Windows, ffmpeg on Linux.
pub fn encode_aac(samples: &[i16], rate: u32, ch: u16, kbps: u32, path: &Path) -> Result<(), String> {
    #[cfg(any(windows, target_os = "linux"))]
    {
        imp::encode_aac(samples, rate, ch, kbps, path)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (samples, rate, ch, kbps, path);
        Err("AAC encoding is not supported on this system yet.".into())
    }
}

/// Where captured files go before they are uploaded.
pub fn capture_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("naTiVeFS-capture");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Peak level of 16-bit PCM samples, 0..1.
pub fn peak_i16(samples: &[i16]) -> f32 {
    samples.iter().map(|s| (*s as i32).unsigned_abs()).max().unwrap_or(0) as f32 / 32768.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_mirrors_and_scales() {
        // 4 x 1 frame: B G R A per pixel, red increasing left to right
        let bgra: Vec<u8> = (0..4u8).flat_map(|i| [0, 0, i * 10, 255]).collect();
        let f = Frame { bgra: Arc::new(bgra), width: 4, height: 1 };
        let (rgba, w, h) = f.preview(4, true);
        assert_eq!((w, h), (4, 1));
        assert_eq!(rgba[0], 30); // mirrored: rightmost pixel first
        let (_, w2, _) = f.preview(2, false);
        assert_eq!(w2, 2);
    }

    #[test]
    fn peak_level() {
        assert_eq!(peak_i16(&[0, -16384, 100]), 0.5);
        assert_eq!(peak_i16(&[]), 0.0);
    }
}
