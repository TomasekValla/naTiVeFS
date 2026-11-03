//! Audio devices and output streams. The device thread pulls interleaved left-justified `i32`
//! samples (see `decode`) through a `Fill` callback and converts them to the device's container
//! format; integer samples are only shifted, never scaled, so the output is bit-perfect whenever
//! the device runs at the file's rate and at least its bit depth.

#[cfg(target_os = "linux")]
pub mod alsa;
#[cfg(windows)]
pub mod wasapi;

/// This system's output: WASAPI on Windows, ALSA on Linux; elsewhere no device (`none`).
#[cfg(target_os = "linux")]
pub use alsa as sys;
#[cfg(not(any(windows, target_os = "linux")))]
pub use none as sys;
#[cfg(windows)]
pub use wasapi as sys;

/// Systems without an output backend yet (macOS): no devices, the player reports that it cannot play.
#[cfg(not(any(windows, target_os = "linux")))]
pub mod none {
    use super::DeviceInfo;
    use crate::Error;

    pub fn devices() -> Result<Vec<DeviceInfo>, Error> {
        Ok(Vec::new())
    }
    pub fn endpoint_volume(_device_id: Option<&str>) -> Option<f32> {
        None
    }
    pub fn set_endpoint_volume(_device_id: Option<&str>, _level: f32) {}
}

use crate::decode::SourceFormat;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Stable id for the config (WASAPI endpoint id, ALSA `hw:` name, ...).
    pub id: String,
    pub name: String,
    pub default: bool,
    /// Bluetooth audio is always re-encoded by the BT codec: never bit-perfect.
    pub bluetooth: bool,
    /// The device has a hardware volume control (usable in bit-perfect mode).
    pub hw_volume: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareMode {
    /// The app owns the device; nothing is mixed or resampled.
    Exclusive,
    /// Through the system mixer (resampled to the mixer rate, mixed with other apps).
    Shared,
}

/// The format the device actually runs at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceFormat {
    pub rate: u32,
    pub channels: u16,
    /// Bytes per sample in the device buffer: 2, 3 or 4.
    pub container_bytes: u16,
    /// Valid bits inside the container (24 in a 32-bit container, ...).
    pub valid_bits: u16,
    pub float: bool,
}

/// Why the output is (not) bit-perfect, for the indicator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fidelity {
    BitPerfect,
    /// Lossy source (MP3, AAC, ...): decoded samples reach the device unchanged, but there were no
    /// original samples to preserve.
    LossySource,
    NotBitPerfect(String),
}

/// Pulled by the device thread: fill the slice with interleaved samples in the source channel
/// layout, return how many samples were written (the rest is played as silence).
pub type Fill = Box<dyn FnMut(&mut [i32]) -> usize + Send>;

/// The device-side half of the bit-perfect decision (DSP bypass is decided by the engine).
pub fn fidelity(src: &SourceFormat, dev: &DeviceFormat, mode: ShareMode, device: &DeviceInfo) -> Fidelity {
    if device.bluetooth {
        return Fidelity::NotBitPerfect("Bluetooth re-encodes the audio".into());
    }
    if mode == ShareMode::Shared {
        return Fidelity::NotBitPerfect("shared mode (system mixer)".into());
    }
    if dev.rate != src.rate {
        return Fidelity::NotBitPerfect(format!("resampled to {} Hz", dev.rate));
    }
    match src.bits {
        None => Fidelity::LossySource,
        Some(b) if dev.float || (dev.valid_bits as u32) < b => {
            Fidelity::NotBitPerfect(format!("{b}-bit file on a {}-bit device", dev.valid_bits))
        }
        Some(_) => Fidelity::BitPerfect,
    }
}

/// Exclusive-mode formats to try for a source, best first: (container bytes, valid bits).
/// A 16-bit file in a 24 / 32-bit container is still bit-perfect (the extra low bits are zero).
pub fn exclusive_candidates(bits: Option<u32>) -> &'static [(u16, u16)] {
    match bits {
        Some(b) if b <= 16 => &[(2, 16), (4, 24), (3, 24), (4, 32)],
        Some(b) if b <= 24 => &[(3, 24), (4, 24), (4, 32)],
        Some(_) => &[(4, 32)],
        None => &[(4, 32), (4, 24), (3, 24), (2, 16)],
    }
}

/// Writes `samples` (left-justified i32) into a device buffer of format `f`.
pub fn convert(samples: &[i32], f: &DeviceFormat, out: &mut [u8]) {
    match (f.float, f.container_bytes) {
        (true, _) => {
            for (s, o) in samples.iter().zip(out.as_chunks_mut::<4>().0) {
                o.copy_from_slice(&(*s as f32 / 2_147_483_648.0).to_le_bytes());
            }
        }
        (false, 2) => {
            for (s, o) in samples.iter().zip(out.as_chunks_mut::<2>().0) {
                o.copy_from_slice(&((s >> 16) as i16).to_le_bytes());
            }
        }
        (false, 3) => {
            for (s, o) in samples.iter().zip(out.as_chunks_mut::<3>().0) {
                o.copy_from_slice(&s.to_le_bytes()[1..]);
            }
        }
        _ => {
            for (s, o) in samples.iter().zip(out.as_chunks_mut::<4>().0) {
                o.copy_from_slice(&s.to_le_bytes());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_containers_keep_the_bits() {
        let s16: i16 = -12345;
        let s = (s16 as i32) << 16;
        let mut out = [0u8; 4];
        convert(&[s], &DeviceFormat { rate: 44100, channels: 1, container_bytes: 2, valid_bits: 16, float: false }, &mut out[..2]);
        assert_eq!(&out[..2], &s16.to_le_bytes());
        convert(&[s], &DeviceFormat { rate: 44100, channels: 1, container_bytes: 4, valid_bits: 24, float: false }, &mut out);
        assert_eq!(i32::from_le_bytes(out) >> 16, s16 as i32);
        let s24 = -0x123456i32;
        convert(&[s24 << 8], &DeviceFormat { rate: 96000, channels: 1, container_bytes: 3, valid_bits: 24, float: false }, &mut out[..3]);
        assert_eq!(&out[..3], &s24.to_le_bytes()[..3]);
    }
}
