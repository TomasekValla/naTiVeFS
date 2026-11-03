//! Windows output through WASAPI. Exclusive mode opens the endpoint in exactly the file's sample rate
//! and a container that holds its bit depth, so the Windows mixer, resampler and audio effects (APOs)
//! are bypassed. Shared mode goes through the mixer (`AUTOCONVERTPCM` resamples to the mix rate).
//! Everything COM runs on the device thread (multithreaded apartment); `open` waits for its result.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;

use windows::core::{HRESULT, PWSTR};
use windows::Win32::Devices::FunctionDiscovery::{PKEY_Device_EnumeratorName, PKEY_Device_FriendlyName};
use windows::Win32::Foundation::{CloseHandle, HANDLE, PROPERTYKEY, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE};
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ};
use windows::Win32::System::Threading::{AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW, WaitForSingleObject};

use super::{exclusive_candidates, DeviceFormat, DeviceInfo, Fill, ShareMode};
use crate::decode::SourceFormat;
use crate::Error;

fn com_init() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

fn enumerator() -> Result<IMMDeviceEnumerator, Error> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(|e| Error::Device(e.message()))
}

unsafe fn pwstr_take(p: PWSTR) -> String {
    let s = p.to_string().unwrap_or_default();
    CoTaskMemFree(Some(p.0 as _));
    s
}

unsafe fn prop_string(device: &IMMDevice, key: &PROPERTYKEY) -> String {
    let Ok(store) = device.OpenPropertyStore(STGM_READ) else { return String::new() };
    let Ok(mut v) = store.GetValue(key) else { return String::new() };
    let s = PropVariantToStringAlloc(&v).map(|p| pwstr_take(p)).unwrap_or_default();
    let _ = PropVariantClear(&mut v);
    s
}

unsafe fn device_info(device: &IMMDevice, default_id: &str) -> Option<DeviceInfo> {
    let id = pwstr_take(device.GetId().ok()?);
    let enumerator = prop_string(device, &PKEY_Device_EnumeratorName).to_ascii_uppercase();
    let hw_volume = device
        .Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
        .and_then(|v| v.QueryHardwareSupport())
        .map(|m| m & ENDPOINT_HARDWARE_SUPPORT_VOLUME != 0)
        .unwrap_or(false);
    Some(DeviceInfo {
        name: prop_string(device, &PKEY_Device_FriendlyName),
        default: id == default_id,
        bluetooth: enumerator.starts_with("BTH"),
        hw_volume,
        id,
    })
}

/// Active playback devices (the system default first). Runs on a short-lived thread of its own:
/// the caller may be a UI thread whose COM apartment must stay single-threaded (winit's OLE drag and
/// drop fails once the thread joined the multithreaded apartment).
pub fn devices() -> Result<Vec<DeviceInfo>, Error> {
    std::thread::spawn(devices_mta).join().map_err(|_| Error::Device("device enumeration crashed".into()))?
}

fn devices_mta() -> Result<Vec<DeviceInfo>, Error> {
    com_init();
    unsafe {
        let en = enumerator()?;
        let default_id = en.GetDefaultAudioEndpoint(eRender, eConsole).ok().and_then(|d| d.GetId().ok()).map(|p| pwstr_take(p)).unwrap_or_default();
        let list = en.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE).map_err(|e| Error::Device(e.message()))?;
        let mut out = Vec::new();
        for i in 0..list.GetCount().unwrap_or(0) {
            if let Some(info) = list.Item(i).ok().and_then(|d| device_info(&d, &default_id)) {
                out.push(info);
            }
        }
        out.sort_by_key(|d| !d.default);
        Ok(out)
    }
}

/// The endpoint `device_id` (None = the system default).
unsafe fn find_device(en: &IMMDeviceEnumerator, device_id: Option<&str>) -> Result<IMMDevice, Error> {
    match device_id {
        Some(id) => {
            let w: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
            en.GetDevice(windows::core::PCWSTR(w.as_ptr())).map_err(|_| Error::Device("the selected device is not connected".into()))
        }
        None => en.GetDefaultAudioEndpoint(eRender, eConsole).map_err(|e| Error::Device(hr_message(&e))),
    }
}

/// The device's hardware volume 0..1 (the Windows volume of that endpoint; in exclusive mode it only
/// acts on devices with a hardware volume control, and it never touches the samples).
pub fn endpoint_volume(device_id: Option<&str>) -> Option<f32> {
    let id = device_id.map(str::to_owned);
    std::thread::spawn(move || {
        com_init();
        unsafe {
            let en = enumerator().ok()?;
            let dev = find_device(&en, id.as_deref()).ok()?;
            let vol = dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).ok()?;
            vol.GetMasterVolumeLevelScalar().ok()
        }
    })
    .join()
    .ok()
    .flatten()
}

pub fn set_endpoint_volume(device_id: Option<&str>, level: f32) {
    let id = device_id.map(str::to_owned);
    let _ = std::thread::spawn(move || {
        com_init();
        unsafe {
            if let Ok(en) = enumerator() {
                if let Ok(dev) = find_device(&en, id.as_deref()) {
                    if let Ok(vol) = dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) {
                        let _ = vol.SetMasterVolumeLevelScalar(level.clamp(0.0, 1.0), std::ptr::null());
                    }
                }
            }
        }
    })
    .join();
}

fn wave_format(rate: u32, channels: u16, container_bytes: u16, valid_bits: u16, float: bool) -> WAVEFORMATEXTENSIBLE {
    let block = channels * container_bytes;
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
            nChannels: channels,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * block as u32,
            nBlockAlign: block,
            wBitsPerSample: container_bytes * 8,
            cbSize: 22,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: valid_bits },
        dwChannelMask: match channels {
            1 => 0x4,
            2 => 0x3,
            4 => 0x33,
            6 => 0x3F,
            8 => 0x63F,
            _ => 0,
        },
        SubFormat: if float { KSDATAFORMAT_SUBTYPE_IEEE_FLOAT } else { KSDATAFORMAT_SUBTYPE_PCM },
    }
}

/// A running output stream; dropping it stops playback and releases the device.
pub struct Stream {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub format: DeviceFormat,
    pub mode: ShareMode,
    pub device: DeviceInfo,
    /// Device thread wake-ups that found too little data (audible gaps).
    pub underruns: Arc<AtomicU64>,
    /// Frames handed to the device so far.
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

struct Opened {
    format: DeviceFormat,
    device: DeviceInfo,
}

impl Stream {
    /// Opens `device_id` (None = system default) for `src` and starts pulling samples from `fill`.
    /// Exclusive mode fails when the device cannot run at the file's rate; the caller then decides
    /// whether to fall back to shared mode.
    pub fn open(device_id: Option<&str>, src: &SourceFormat, mode: ShareMode, fill: Fill) -> Result<Stream, Error> {
        let stop = Arc::new(AtomicBool::new(false));
        let underruns = Arc::new(AtomicU64::new(0));
        let frames = Arc::new(AtomicU64::new(0));
        let (tx, rx) = mpsc::channel();
        let (st, un, fr) = (stop.clone(), underruns.clone(), frames.clone());
        let (id, src) = (device_id.map(str::to_owned), src.clone());
        let thread = std::thread::Builder::new()
            .name("audio-out".into())
            .spawn(move || run(id, src, mode, fill, tx, st, un, fr))
            .map_err(|e| Error::Device(e.to_string()))?;
        match rx.recv() {
            Ok(Ok(o)) => Ok(Stream { stop, thread: Some(thread), format: o.format, mode, device: o.device, underruns, frames }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err(Error::Device("the audio thread ended unexpectedly".into())),
        }
    }
}

fn hr_message(e: &windows::core::Error) -> String {
    let code = e.code();
    if code == AUDCLNT_E_DEVICE_IN_USE {
        "the device is used by another app in exclusive mode".into()
    } else if code == AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED {
        "exclusive mode is turned off for this device (Sound settings > device > Advanced)".into()
    } else if code == AUDCLNT_E_DEVICE_INVALIDATED {
        "the device was disconnected".into()
    } else {
        e.message()
    }
}

struct Event(HANDLE);
impl Drop for Event {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Picks and initializes a format. Returns the client (initialized), its format and buffer size.
unsafe fn init_client(device: &IMMDevice, src: &SourceFormat, mode: ShareMode, event: HANDLE) -> Result<(IAudioClient, DeviceFormat), Error> {
    let activate = || device.Activate::<IAudioClient>(CLSCTX_ALL, None).map_err(|e| Error::Device(hr_message(&e)));
    let client = activate()?;
    match mode {
        ShareMode::Exclusive => {
            let mut chosen = None;
            // mono files also try stereo (the same samples on both channels stay bit-perfect)
            let channel_options: &[u16] = if src.channels == 1 { &[1, 2] } else { &[src.channels] };
            'outer: for &ch in channel_options {
                for &(bytes, valid) in exclusive_candidates(src.bits) {
                    let wf = wave_format(src.rate, ch, bytes, valid, false);
                    if client.IsFormatSupported(AUDCLNT_SHAREMODE_EXCLUSIVE, &wf.Format, None) == HRESULT(0) {
                        chosen = Some((wf, DeviceFormat { rate: src.rate, channels: ch, container_bytes: bytes, valid_bits: valid, float: false }));
                        break 'outer;
                    }
                }
            }
            let (wf, format) = chosen.ok_or_else(|| {
                Error::Device(format!("the device does not support {} Hz / {} bit in exclusive mode", src.rate, src.bits.unwrap_or(32)))
            })?;
            let mut period = 0i64;
            client.GetDevicePeriod(Some(&mut period), None).map_err(|e| Error::Device(hr_message(&e)))?;
            let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK;
            let mut client = client;
            if let Err(e) = client.Initialize(AUDCLNT_SHAREMODE_EXCLUSIVE, flags, period, period, &wf.Format, None) {
                if e.code() != AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED {
                    return Err(Error::Device(hr_message(&e)));
                }
                // the period must be a whole number of frames the driver likes: ask and retry once
                let frames = client.GetBufferSize().map_err(|e| Error::Device(hr_message(&e)))? as i64;
                period = (10_000_000 * frames + src.rate as i64 / 2) / src.rate as i64;
                client = activate()?;
                client.Initialize(AUDCLNT_SHAREMODE_EXCLUSIVE, flags, period, period, &wf.Format, None).map_err(|e| Error::Device(hr_message(&e)))?;
            }
            client.SetEventHandle(event).map_err(|e| Error::Device(hr_message(&e)))?;
            Ok((client, format))
        }
        ShareMode::Shared => {
            // float in the source's channel count; Windows converts to the mix format and rate
            let wf = wave_format(src.rate, src.channels, 4, 32, true);
            let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
            // 50 ms buffer: shared mode has the mixer's own latency on top anyway
            client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 500_000, 0, &wf.Format, None).map_err(|e| Error::Device(hr_message(&e)))?;
            client.SetEventHandle(event).map_err(|e| Error::Device(hr_message(&e)))?;
            let mut rate = src.rate;
            if let Ok(mix) = client.GetMixFormat() {
                rate = (*mix).nSamplesPerSec;
                CoTaskMemFree(Some(mix as _));
            }
            Ok((client, DeviceFormat { rate, channels: src.channels, container_bytes: 4, valid_bits: 32, float: true }))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    device_id: Option<String>,
    src: SourceFormat,
    mode: ShareMode,
    mut fill: Fill,
    tx: mpsc::Sender<Result<Opened, Error>>,
    stop: Arc<AtomicBool>,
    underruns: Arc<AtomicU64>,
    frames_out: Arc<AtomicU64>,
) {
    com_init();
    let setup = unsafe {
        (|| -> Result<_, Error> {
            let en = enumerator()?;
            let default_id = en.GetDefaultAudioEndpoint(eRender, eConsole).ok().and_then(|d| d.GetId().ok()).map(|p| pwstr_take(p)).unwrap_or_default();
            let device = find_device(&en, device_id.as_deref())?;
            let info = device_info(&device, &default_id).ok_or_else(|| Error::Device("device not readable".into()))?;
            let event = Event(CreateEventW(None, false, false, None).map_err(|e| Error::Device(e.message()))?);
            let (client, format) = init_client(&device, &src, mode, event.0)?;
            let render: IAudioRenderClient = client.GetService().map_err(|e| Error::Device(hr_message(&e)))?;
            let buffer = client.GetBufferSize().map_err(|e| Error::Device(hr_message(&e)))?;
            Ok((client, render, event, format, info, buffer))
        })()
    };
    let (client, render, event, format, info, buffer) = match setup {
        Ok(s) => s,
        Err(e) => {
            let _ = tx.send(Err(e));
            return;
        }
    };
    let _ = tx.send(Ok(Opened { format, device: info }));

    let mut task = 0u32;
    let mmcss = unsafe { AvSetMmThreadCharacteristicsW(windows::core::w!("Pro Audio"), &mut task) }.ok();
    let src_ch = src.channels as usize;
    let dev_ch = format.channels as usize;
    let bytes_per_frame = dev_ch * format.container_bytes as usize;
    let mut samples = vec![0i32; buffer as usize * src_ch.max(dev_ch)];

    // write `n` frames into the device buffer; mono -> stereo duplicates the channel
    let mut write = |n: u32, first: bool| -> windows::core::Result<()> {
        unsafe {
            let ptr = render.GetBuffer(n)?;
            let out = std::slice::from_raw_parts_mut(ptr, n as usize * bytes_per_frame);
            let want = n as usize * src_ch;
            let got = fill(&mut samples[..want]);
            if got < want && !first {
                underruns.fetch_add(1, Ordering::Relaxed);
            }
            samples[got..want].fill(0);
            if dev_ch == src_ch {
                super::convert(&samples[..want], &format, out);
            } else {
                let dup: Vec<i32> = samples[..want].iter().flat_map(|s| [*s, *s]).collect();
                super::convert(&dup, &format, out);
            }
            render.ReleaseBuffer(n, 0)?;
            frames_out.fetch_add(n as u64, Ordering::Relaxed);
            Ok(())
        }
    };

    let result = (|| -> windows::core::Result<()> {
        unsafe {
            // prefill so the device starts with real data
            let pad = if mode == ShareMode::Shared { client.GetCurrentPadding()? } else { 0 };
            write(buffer - pad, true)?;
            client.Start()?;
            while !stop.load(Ordering::Acquire) {
                if WaitForSingleObject(event.0, 500) != WAIT_OBJECT_0 {
                    continue;
                }
                let n = match mode {
                    // exclusive event mode: every event asks for one whole buffer
                    ShareMode::Exclusive => buffer,
                    ShareMode::Shared => buffer - client.GetCurrentPadding()?,
                };
                if n > 0 {
                    write(n, false)?;
                }
            }
            client.Stop()?;
            Ok(())
        }
    })();
    if let Err(e) = result {
        eprintln!("audio output stopped: {}", hr_message(&e));
    }
    if let Some(h) = mmcss {
        unsafe {
            let _ = AvRevertMmThreadCharacteristics(h);
        }
    }
    drop(event);
}
