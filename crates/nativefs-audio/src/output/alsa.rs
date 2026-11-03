//! Linux output through ALSA, loaded at runtime (`libasound.so.2`, present on every desktop Linux; no build-time
//! headers needed). Exclusive = the card's `hw:` device: the app owns it, nothing is mixed or resampled, so the
//! samples reach the DAC unchanged (bit-perfect when the card runs the file's rate and depth). Shared = ALSA's
//! `default` device, which on current desktops is PipeWire (or PulseAudio): mixed with other apps and resampled.
//! The hardware volume is not driven in bit-perfect mode yet (the knob is greyed out like on a device without one).

use std::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void, CStr, CString};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::thread::JoinHandle;

use super::{convert, exclusive_candidates, DeviceFormat, DeviceInfo, Fill, ShareMode};
use crate::decode::SourceFormat;
use crate::Error;

type Pcm = *mut c_void;
type HwParams = *mut c_void;

const STREAM_PLAYBACK: c_int = 0;
const ACCESS_RW_INTERLEAVED: c_int = 3;
const FORMAT_S16_LE: c_int = 2;
const FORMAT_S24_LE: c_int = 6;
const FORMAT_S32_LE: c_int = 10;
const FORMAT_FLOAT_LE: c_int = 14;
const FORMAT_S24_3LE: c_int = 32;

/// The few libasound functions the output needs.
struct Lib {
    _lib: libloading::Library,
    pcm_open: unsafe extern "C" fn(*mut Pcm, *const c_char, c_int, c_int) -> c_int,
    pcm_close: unsafe extern "C" fn(Pcm) -> c_int,
    pcm_prepare: unsafe extern "C" fn(Pcm) -> c_int,
    pcm_drop: unsafe extern "C" fn(Pcm) -> c_int,
    pcm_writei: unsafe extern "C" fn(Pcm, *const c_void, c_ulong) -> c_long,
    pcm_recover: unsafe extern "C" fn(Pcm, c_int, c_int) -> c_int,
    hw_malloc: unsafe extern "C" fn(*mut HwParams) -> c_int,
    hw_free: unsafe extern "C" fn(HwParams),
    hw_any: unsafe extern "C" fn(Pcm, HwParams) -> c_int,
    hw_set_access: unsafe extern "C" fn(Pcm, HwParams, c_int) -> c_int,
    hw_set_format: unsafe extern "C" fn(Pcm, HwParams, c_int) -> c_int,
    hw_test_format: unsafe extern "C" fn(Pcm, HwParams, c_int) -> c_int,
    hw_set_channels: unsafe extern "C" fn(Pcm, HwParams, c_uint) -> c_int,
    hw_set_rate_resample: unsafe extern "C" fn(Pcm, HwParams, c_uint) -> c_int,
    hw_set_rate: unsafe extern "C" fn(Pcm, HwParams, c_uint, c_int) -> c_int,
    hw_set_rate_near: unsafe extern "C" fn(Pcm, HwParams, *mut c_uint, *mut c_int) -> c_int,
    hw_set_buffer_time_near: unsafe extern "C" fn(Pcm, HwParams, *mut c_uint, *mut c_int) -> c_int,
    hw_set_period_time_near: unsafe extern "C" fn(Pcm, HwParams, *mut c_uint, *mut c_int) -> c_int,
    hw_get_period_size: unsafe extern "C" fn(HwParams, *mut c_ulong, *mut c_int) -> c_int,
    hw_params: unsafe extern "C" fn(Pcm, HwParams) -> c_int,
    strerror: unsafe extern "C" fn(c_int) -> *const c_char,
    name_hint: unsafe extern "C" fn(c_int, *const c_char, *mut *mut *mut c_void) -> c_int,
    name_get_hint: unsafe extern "C" fn(*const c_void, *const c_char) -> *mut c_char,
    name_free_hint: unsafe extern "C" fn(*mut *mut c_void) -> c_int,
}

// SAFETY: plain C function pointers into a library that stays loaded for the whole run.
unsafe impl Send for Lib {}
unsafe impl Sync for Lib {}

fn lib() -> Result<&'static Lib, Error> {
    static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();
    LIB.get_or_init(|| unsafe { load() }.map_err(|e| format!("ALSA (libasound.so.2) is not available: {e}")))
        .as_ref()
        .map_err(|e| Error::Device(e.clone()))
}

unsafe fn load() -> Result<Lib, libloading::Error> {
    unsafe {
        let lib = libloading::Library::new("libasound.so.2")?;
        macro_rules! sym {
            ($name:literal) => {
                *lib.get($name)?
            };
        }
        Ok(Lib {
            pcm_open: sym!(b"snd_pcm_open\0"),
            pcm_close: sym!(b"snd_pcm_close\0"),
            pcm_prepare: sym!(b"snd_pcm_prepare\0"),
            pcm_drop: sym!(b"snd_pcm_drop\0"),
            pcm_writei: sym!(b"snd_pcm_writei\0"),
            pcm_recover: sym!(b"snd_pcm_recover\0"),
            hw_malloc: sym!(b"snd_pcm_hw_params_malloc\0"),
            hw_free: sym!(b"snd_pcm_hw_params_free\0"),
            hw_any: sym!(b"snd_pcm_hw_params_any\0"),
            hw_set_access: sym!(b"snd_pcm_hw_params_set_access\0"),
            hw_set_format: sym!(b"snd_pcm_hw_params_set_format\0"),
            hw_test_format: sym!(b"snd_pcm_hw_params_test_format\0"),
            hw_set_channels: sym!(b"snd_pcm_hw_params_set_channels\0"),
            hw_set_rate_resample: sym!(b"snd_pcm_hw_params_set_rate_resample\0"),
            hw_set_rate: sym!(b"snd_pcm_hw_params_set_rate\0"),
            hw_set_rate_near: sym!(b"snd_pcm_hw_params_set_rate_near\0"),
            hw_set_buffer_time_near: sym!(b"snd_pcm_hw_params_set_buffer_time_near\0"),
            hw_set_period_time_near: sym!(b"snd_pcm_hw_params_set_period_time_near\0"),
            hw_get_period_size: sym!(b"snd_pcm_hw_params_get_period_size\0"),
            hw_params: sym!(b"snd_pcm_hw_params\0"),
            strerror: sym!(b"snd_strerror\0"),
            name_hint: sym!(b"snd_device_name_hint\0"),
            name_get_hint: sym!(b"snd_device_name_get_hint\0"),
            name_free_hint: sym!(b"snd_device_name_free_hint\0"),
            _lib: lib,
        })
    }
}

fn err(l: &Lib, what: &str, code: c_int) -> Error {
    // SAFETY: snd_strerror returns a static string.
    let msg = unsafe { CStr::from_ptr((l.strerror)(code)) }.to_string_lossy().into_owned();
    let hint = if code == -16 { " (the device is used by another app; exclusive mode needs it free)" } else { "" };
    Error::Device(format!("{what}: {msg}{hint}"))
}

/// The output devices: one entry per playback device of each card (`hw:` for exclusive, its own name), plus nothing
/// else; shared mode always plays through the system's default device. Read from the kernel's `/proc/asound` lists:
/// ALSA's name hints leave out the `hw:` devices on most distributions. The default is the first output that is not
/// HDMI / DisplayPort (often nothing is connected there).
pub fn devices() -> Result<Vec<DeviceInfo>, Error> {
    let mut out = proc_devices();
    if out.is_empty() {
        out = hinted_devices()?;
    }
    let pick = out.iter().position(|d| !is_display(&d.name)).unwrap_or(0);
    for (i, d) in out.iter_mut().enumerate() {
        d.default = i == pick;
    }
    Ok(out)
}

fn is_display(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("hdmi") || n.contains("displayport")
}

/// `/proc/asound/cards` (" 1 [Generic_1      ]: HDA-Intel - HD-Audio Generic") and `/proc/asound/pcm`
/// ("01-00: ALC236 Analog : ALC236 Analog : playback 1 : capture 1").
fn proc_devices() -> Vec<DeviceInfo> {
    let (Ok(cards), Ok(pcm)) = (std::fs::read_to_string("/proc/asound/cards"), std::fs::read_to_string("/proc/asound/pcm")) else {
        return Vec::new();
    };
    parse_proc(&cards, &pcm)
}

fn parse_proc(cards: &str, pcm: &str) -> Vec<DeviceInfo> {
    // card index -> (id, long name)
    let mut names: Vec<(u32, String, String)> = Vec::new();
    for line in cards.lines() {
        let t = line.trim_start();
        let Some((idx, rest)) = t.split_once(' ') else { continue };
        let Ok(idx) = idx.parse::<u32>() else { continue };
        let Some(rest) = rest.trim_start().strip_prefix('[') else { continue };
        let Some((id, rest)) = rest.split_once(']') else { continue };
        let long = rest.split_once(" - ").map(|(_, n)| n.trim()).unwrap_or("").to_owned();
        names.push((idx, id.trim().to_owned(), long));
    }
    let mut out = Vec::new();
    for line in pcm.lines() {
        let mut fields = line.split(" : ");
        let Some(head) = fields.next() else { continue };
        let Some((cd, name)) = head.split_once(": ") else { continue };
        if !fields.any(|f| f.trim().starts_with("playback")) {
            continue;
        }
        let Some((c, d)) = cd.split_once('-') else { continue };
        let (Ok(c), Ok(d)) = (c.parse::<u32>(), d.parse::<u32>()) else { continue };
        let Some((_, id, long)) = names.iter().find(|n| n.0 == c) else { continue };
        let name = name.trim();
        out.push(DeviceInfo {
            id: format!("CARD={id},DEV={d}"),
            name: if long.is_empty() { name.to_owned() } else { format!("{long} · {name}") },
            default: false,
            bluetooth: false,
            hw_volume: false,
        });
    }
    out
}

/// Fallback when `/proc/asound` is not readable: ALSA's name hints (lists `hw:` devices only on some systems).
fn hinted_devices() -> Result<Vec<DeviceInfo>, Error> {
    let l = lib()?;
    let mut out = Vec::new();
    unsafe {
        let mut hints: *mut *mut c_void = std::ptr::null_mut();
        let iface = CString::new("pcm").unwrap();
        if (l.name_hint)(-1, iface.as_ptr(), &mut hints) < 0 || hints.is_null() {
            return Ok(out);
        }
        let get = |h: *const c_void, key: &str| -> Option<String> {
            let k = CString::new(key).ok()?;
            let p = (l.name_get_hint)(h, k.as_ptr());
            if p.is_null() {
                return None;
            }
            let s = CStr::from_ptr(p).to_string_lossy().into_owned();
            libc_free(p as *mut c_void);
            Some(s)
        };
        let mut i = 0;
        while !(*hints.add(i)).is_null() {
            let h = *hints.add(i);
            i += 1;
            let Some(name) = get(h, "NAME") else { continue };
            if get(h, "IOID").is_some_and(|io| io == "Input") {
                continue;
            }
            // "hw:CARD=PCH,DEV=0": the card itself
            let Some(id) = name.strip_prefix("hw:") else { continue };
            let desc = get(h, "DESC").unwrap_or_default().replace('\n', " · ");
            let lower = desc.to_lowercase();
            out.push(DeviceInfo {
                id: id.to_owned(),
                name: if desc.is_empty() { name.clone() } else { desc },
                default: out.is_empty(),
                bluetooth: lower.contains("bluez") || lower.contains("bluetooth"),
                hw_volume: false,
            });
        }
        (l.name_free_hint)(hints);
    }
    Ok(out)
}

/// Strings from `snd_device_name_get_hint` are freed with the C library's `free`.
unsafe fn libc_free(p: *mut c_void) {
    unsafe extern "C" {
        fn free(p: *mut c_void);
    }
    unsafe { free(p) }
}

/// No hardware volume through ALSA yet (bit-perfect mode then greys the knob out).
pub fn endpoint_volume(_device_id: Option<&str>) -> Option<f32> {
    None
}

pub fn set_endpoint_volume(_device_id: Option<&str>, _level: f32) {}

/// A running output stream; dropping it stops playback and releases the device.
pub struct Stream {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub format: DeviceFormat,
    pub mode: ShareMode,
    pub device: DeviceInfo,
    pub underruns: Arc<AtomicU64>,
    pub frames: Arc<AtomicU64>,
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Handle(Pcm);
// SAFETY: the PCM handle is used by the audio thread only.
unsafe impl Send for Handle {}

impl Stream {
    /// Same contract as the WASAPI stream: exclusive fails when the card cannot take the file's rate / depth,
    /// the engine then falls back to shared.
    pub fn open(device_id: Option<&str>, src: &SourceFormat, mode: ShareMode, fill: Fill) -> Result<Stream, Error> {
        let l = lib()?;
        let devices = devices().unwrap_or_default();
        let device = match mode {
            ShareMode::Exclusive => devices
                .iter()
                .find(|d| device_id.is_some_and(|id| id == d.id))
                .or(devices.iter().find(|d| d.default))
                .cloned()
                .ok_or_else(|| Error::Device("no sound card was found".into()))?,
            ShareMode::Shared => DeviceInfo { id: "default".into(), name: "System default".into(), default: true, bluetooth: false, hw_volume: false },
        };
        let pcm_name = match mode {
            ShareMode::Exclusive => format!("hw:{}", device.id),
            ShareMode::Shared => "default".into(),
        };
        let (pcm, format, period) = unsafe { open_pcm(l, &pcm_name, src, mode)? };
        let stop = Arc::new(AtomicBool::new(false));
        let underruns = Arc::new(AtomicU64::new(0));
        let frames = Arc::new(AtomicU64::new(0));
        let (st, un, fr) = (stop.clone(), underruns.clone(), frames.clone());
        let handle = Handle(pcm);
        let (tx, rx) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("audio-out".into())
            .spawn(move || {
                let handle = handle;
                let _ = tx.send(());
                run(l, handle.0, format, period, fill, st, un, fr);
            })
            .map_err(|e| Error::Device(e.to_string()))?;
        let _ = rx.recv();
        Ok(Stream { stop, thread: Some(thread), format, mode, device, underruns, frames })
    }
}

/// Opens and configures the PCM. Returns the handle, its format and the period in frames.
unsafe fn open_pcm(l: &Lib, name: &str, src: &SourceFormat, mode: ShareMode) -> Result<(Pcm, DeviceFormat, usize), Error> {
    unsafe {
        let cname = CString::new(name).map_err(|e| Error::Device(e.to_string()))?;
        let mut pcm: Pcm = std::ptr::null_mut();
        let r = (l.pcm_open)(&mut pcm, cname.as_ptr(), STREAM_PLAYBACK, 0);
        if r < 0 {
            return Err(err(l, &format!("cannot open {name}"), r));
        }
        let mut hw: HwParams = std::ptr::null_mut();
        (l.hw_malloc)(&mut hw);
        let result = (|| {
            (l.hw_any)(pcm, hw);
            let r = (l.hw_set_access)(pcm, hw, ACCESS_RW_INTERLEAVED);
            if r < 0 {
                return Err(err(l, "interleaved access", r));
            }
            let format = match mode {
                ShareMode::Exclusive => {
                    // the card's own sample format: one of the bit-perfect candidates, at the file's exact rate
                    let (bytes, valid, fmt) = exclusive_candidates(src.bits)
                        .iter()
                        .map(|&(b, v)| (b, v, alsa_format(b, v)))
                        .find(|&(_, _, f)| (l.hw_test_format)(pcm, hw, f) == 0)
                        .ok_or_else(|| Error::Device(format!("the card does not take {}-bit samples", src.bits.unwrap_or(32))))?;
                    let r = (l.hw_set_format)(pcm, hw, fmt);
                    if r < 0 {
                        return Err(err(l, "sample format", r));
                    }
                    (l.hw_set_rate_resample)(pcm, hw, 0);
                    let r = (l.hw_set_rate)(pcm, hw, src.rate, 0);
                    if r < 0 {
                        return Err(err(l, &format!("the card does not run at {} Hz", src.rate), r));
                    }
                    DeviceFormat { rate: src.rate, channels: src.channels, container_bytes: bytes, valid_bits: valid, float: false }
                }
                ShareMode::Shared => {
                    let r = (l.hw_set_format)(pcm, hw, FORMAT_FLOAT_LE);
                    if r < 0 {
                        return Err(err(l, "float samples", r));
                    }
                    let mut rate = src.rate;
                    (l.hw_set_rate_near)(pcm, hw, &mut rate, std::ptr::null_mut());
                    DeviceFormat { rate, channels: src.channels, container_bytes: 4, valid_bits: 32, float: true }
                }
            };
            // mono files also go out as mono; ALSA hw cards that need stereo refuse here and the engine falls back
            let r = (l.hw_set_channels)(pcm, hw, src.channels as c_uint);
            if r < 0 {
                return Err(err(l, &format!("{} channels", src.channels), r));
            }
            // ~100 ms buffer in ~10 ms periods (the engine's ring holds another 100 ms)
            let mut buffer_us: c_uint = 100_000;
            let mut period_us: c_uint = 10_000;
            (l.hw_set_buffer_time_near)(pcm, hw, &mut buffer_us, std::ptr::null_mut());
            (l.hw_set_period_time_near)(pcm, hw, &mut period_us, std::ptr::null_mut());
            let r = (l.hw_params)(pcm, hw);
            if r < 0 {
                return Err(err(l, "applying the format", r));
            }
            let mut period: c_ulong = 0;
            (l.hw_get_period_size)(hw, &mut period, std::ptr::null_mut());
            Ok((format, (period as usize).max(64)))
        })();
        (l.hw_free)(hw);
        match result {
            Ok((format, period)) => {
                (l.pcm_prepare)(pcm);
                Ok((pcm, format, period))
            }
            Err(e) => {
                (l.pcm_close)(pcm);
                Err(e)
            }
        }
    }
}

fn alsa_format(bytes: u16, valid: u16) -> c_int {
    match (bytes, valid) {
        (2, _) => FORMAT_S16_LE,
        (3, _) => FORMAT_S24_3LE,
        (4, 24) => FORMAT_S24_LE,
        _ => FORMAT_S32_LE,
    }
}

#[allow(clippy::too_many_arguments)]
fn run(l: &Lib, pcm: Pcm, format: DeviceFormat, period: usize, mut fill: Fill, stop: Arc<AtomicBool>, underruns: Arc<AtomicU64>, frames: Arc<AtomicU64>) {
    let ch = format.channels as usize;
    let mut samples = vec![0i32; period * ch];
    let mut bytes = vec![0u8; period * ch * format.container_bytes as usize];
    // S24_LE keeps the 24 bits in the low bytes of a 32-bit word: shift the left-justified samples down
    let low24 = !format.float && format.container_bytes == 4 && format.valid_bits == 24;
    while !stop.load(Ordering::Acquire) {
        let n = fill(&mut samples);
        if n < samples.len() {
            samples[n..].fill(0);
            if n == 0 {
                underruns.fetch_add(1, Ordering::Relaxed);
            }
        }
        if low24 {
            for s in samples.iter_mut() {
                *s >>= 8;
            }
        }
        convert(&samples, &format, &mut bytes);
        let mut off = 0usize;
        while off < period && !stop.load(Ordering::Acquire) {
            // SAFETY: `bytes` holds `period` frames in the PCM's format.
            let w = unsafe { (l.pcm_writei)(pcm, bytes.as_ptr().add(off * ch * format.container_bytes as usize) as *const c_void, (period - off) as c_ulong) };
            if w < 0 {
                underruns.fetch_add(1, Ordering::Relaxed);
                // underrun / suspend: recover and go on
                if unsafe { (l.pcm_recover)(pcm, w as c_int, 1) } < 0 {
                    stop.store(true, Ordering::Release);
                }
                continue;
            }
            off += w as usize;
        }
        frames.fetch_add(period as u64, Ordering::Relaxed);
    }
    unsafe {
        (l.pcm_drop)(pcm);
        (l.pcm_close)(pcm);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARDS: &str = " 0 [Generic        ]: HDA-Intel - HD-Audio Generic
                      HD-Audio Generic at 0xfb3c8000 irq 76
 1 [Generic_1      ]: HDA-Intel - HD-Audio Generic
                      HD-Audio Generic at 0xfb3c0000 irq 77
";
    const PCM: &str = "00-03: HDMI 0 : HDMI 0 : playback 1
00-07: HDMI 1 : HDMI 1 : playback 1
01-00: ALC236 Analog : ALC236 Analog : playback 1 : capture 1
02-00: USB Mic : USB Mic : capture 1
";

    #[test]
    fn proc_lists_playback_devices_with_hw_ids() {
        let d = parse_proc(CARDS, PCM);
        let ids: Vec<&str> = d.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["CARD=Generic,DEV=3", "CARD=Generic,DEV=7", "CARD=Generic_1,DEV=0"]);
        assert_eq!(d[2].name, "HD-Audio Generic · ALC236 Analog");
    }

    #[test]
    fn default_skips_hdmi() {
        let mut d = parse_proc(CARDS, PCM);
        let pick = d.iter().position(|d| !is_display(&d.name)).unwrap_or(0);
        for (i, x) in d.iter_mut().enumerate() {
            x.default = i == pick;
        }
        assert!(d[2].default && !d[0].default);
    }
}
