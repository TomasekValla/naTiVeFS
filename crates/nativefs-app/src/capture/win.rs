//! Windows capture with Media Foundation: a source reader per device (camera frames converted to
//! RGB32 by MF, microphone as 16-bit PCM), and a sink writer for MP4 recording (H.264 through the
//! GPU's hardware encoder when there is one, AAC audio). Sees real webcams and Windows virtual cameras
//! (e.g. a phone through Phone Link); DirectShow-only devices (OBS Virtual Camera) are not visible to MF.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::{Interface, GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::E_ACCESSDENIED;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};

use super::{peak_i16, Device, Event, Frame, PcmSink, Sink, AUDIO_BITRATE, MAX_FPS, MAX_H, MAX_W, VIDEO_BITRATE};

/// COM objects used from the capture threads (all created in the multithreaded apartment).
struct Mt<T>(T);
unsafe impl<T> Send for Mt<T> {}

fn init_thread() -> Result<(), String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| format!("Media Foundation: {e}"))
    }
}

fn describe(e: &windows::core::Error, what: &str) -> String {
    if e.code() == E_ACCESSDENIED {
        format!("Access to the {what} is blocked. Allow it in Windows Settings → Privacy & security → {}.", if what == "camera" { "Camera" } else { "Microphone" })
    } else if e.code().0 as u32 == 0xC00D3704 {
        // MF_E_HW_MFT_FAILED_START_STREAMING / device busy
        format!("The {what} is busy (another app is using it).")
    } else {
        format!("The {what} could not be started: {}", e.message())
    }
}

unsafe fn devices(kind: GUID) -> windows::core::Result<Vec<IMFActivate>> {
    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.unwrap();
        attrs.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &kind)?;
        let mut arr: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        MFEnumDeviceSources(&attrs, &mut arr, &mut count)?;
        let mut out = Vec::new();
        for i in 0..count as usize {
            if let Some(a) = (*arr.add(i)).take() {
                out.push(a);
            }
        }
        if !arr.is_null() {
            CoTaskMemFree(Some(arr as *const _));
        }
        Ok(out)
    }
}

unsafe fn string(a: &IMFActivate, key: &GUID) -> String {
    unsafe {
        let mut p = PWSTR::null();
        let mut len = 0u32;
        if a.GetAllocatedString(key, &mut p, &mut len).is_err() || p.is_null() {
            return String::new();
        }
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

fn list(kind: GUID, id_key: &GUID) -> Result<Vec<Device>, String> {
    let (tx, rx) = mpsc::channel();
    let id_key = *id_key;
    // a thread of its own: the UI thread is a single-threaded apartment
    std::thread::spawn(move || {
        let r = init_thread().and_then(|_| unsafe {
            let acts = devices(kind).map_err(|e| e.message())?;
            Ok(acts.iter().map(|a| Device { id: string(a, &id_key), name: string(a, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME) }).collect())
        });
        let _ = tx.send(r);
    });
    rx.recv_timeout(Duration::from_secs(10)).map_err(|_| "listing devices timed out".to_owned())?
}

pub fn cameras() -> Result<Vec<Device>, String> {
    list(MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK)
}

pub fn microphones() -> Result<Vec<Device>, String> {
    list(MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_AUDCAP_GUID, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_AUDCAP_ENDPOINT_ID)
}

unsafe fn open_reader(kind: GUID, id_key: &GUID, id: &str, what: &str) -> Result<(IMFMediaSource, IMFSourceReader), String> {
    unsafe {
        let acts = devices(kind).map_err(|e| e.message())?;
        let act = acts.iter().find(|a| string(a, id_key) == id).or(acts.first()).ok_or_else(|| format!("No {what} was found."))?;
        let source: IMFMediaSource = act.ActivateObject().map_err(|e| describe(&e, what))?;
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 2).map_err(|e| e.message())?;
        let attrs = attrs.unwrap();
        // RGB32 out of any camera format (YUY2 / NV12 / MJPEG)
        let _ = attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1);
        let reader = MFCreateSourceReaderFromMediaSource(&source, &attrs).map_err(|e| describe(&e, what))?;
        Ok((source, reader))
    }
}

fn unpack(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, v as u32)
}

/// The camera mode: the biggest one up to 1080p, at the frame rate closest to 30 fps.
unsafe fn pick_video_mode(reader: &IMFSourceReader) -> Option<IMFMediaType> {
    unsafe {
        let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        let mut best: Option<(u64, i64, IMFMediaType)> = None;
        for i in 0.. {
            let Ok(t) = reader.GetNativeMediaType(stream, i) else { break };
            let (w, h) = unpack(t.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0));
            let (num, den) = unpack(t.GetUINT64(&MF_MT_FRAME_RATE).unwrap_or(0));
            let fps = if den > 0 { num as f64 / den as f64 } else { 0.0 };
            if w == 0 || w > MAX_W || h > MAX_H || fps > MAX_FPS {
                continue;
            }
            let area = w as u64 * h as u64;
            // area first, then frames per second (15 fps modes lose against 30 fps ones of the same size)
            let score_fps = (fps * 100.0) as i64;
            if best.as_ref().is_none_or(|b| (area, score_fps) > (b.0, b.1)) {
                best = Some((area, score_fps, t));
            }
        }
        best.map(|b| b.2)
    }
}

struct Writer {
    writer: IMFSinkWriter,
    video: u32,
    audio: Option<u32>,
    path: PathBuf,
    start: Instant,
    video_base: Option<(i64, i64)>,
    audio_base: Option<(i64, i64)>,
}

impl Writer {
    /// Sample time on the recording's clock: each stream starts at the moment its first sample arrived.
    fn retime(base: &mut Option<(i64, i64)>, start: Instant, ts: i64) -> i64 {
        let (first, offset) = *base.get_or_insert_with(|| (ts, (start.elapsed().as_nanos() / 100) as i64));
        (ts - first + offset).max(0)
    }
}

struct Shared {
    stop: AtomicBool,
    latest: Mutex<Option<Frame>>,
    writer: Mutex<Option<Mt<Writer>>>,
    /// The microphone's PCM type (input of the AAC stream).
    audio_type: Mutex<Option<Mt<IMFMediaType>>>,
}

enum Cmd {
    Start(PathBuf, mpsc::Sender<Result<(), String>>),
    Stop(mpsc::Sender<Result<PathBuf, String>>),
}

pub struct Session {
    shared: Arc<Shared>,
    cmd: mpsc::Sender<Cmd>,
    threads: Vec<JoinHandle<()>>,
}

impl Session {
    pub fn start(camera: &str, mic: Option<&str>, sink: Sink) -> Result<Self, String> {
        let shared = Arc::new(Shared { stop: AtomicBool::new(false), latest: Mutex::new(None), writer: Mutex::new(None), audio_type: Mutex::new(None) });
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let mut threads = Vec::new();
        // microphone first, so its format is known before a recording starts
        if let Some(mic) = mic {
            let (ready_tx, ready_rx) = mpsc::channel();
            let (sh, sink2, mic) = (shared.clone(), sink.clone(), mic.to_owned());
            threads.push(std::thread::Builder::new().name("nativefs-mic".into()).spawn(move || audio_thread(&mic, sh, sink2, ready_tx)).map_err(|e| e.to_string())?);
            if let Ok(Err(e)) = ready_rx.recv_timeout(Duration::from_secs(10)) {
                sink(Event::Error(e));
            }
        }
        let (ready_tx, ready_rx) = mpsc::channel();
        let (sh, cam) = (shared.clone(), camera.to_owned());
        threads.push(
            std::thread::Builder::new()
                .name("nativefs-camera".into())
                .spawn(move || video_thread(&cam, sh, sink, ready_tx, cmd_rx))
                .map_err(|e| e.to_string())?,
        );
        match ready_rx.recv_timeout(Duration::from_secs(15)) {
            Ok(Ok(())) => Ok(Self { shared, cmd: cmd_tx, threads }),
            Ok(Err(e)) => {
                shared.stop.store(true, Ordering::Relaxed);
                Err(e)
            }
            Err(_) => {
                shared.stop.store(true, Ordering::Relaxed);
                Err("The camera did not start (timeout).".into())
            }
        }
    }

    pub fn latest(&self) -> Option<Frame> {
        self.shared.latest.lock().unwrap().clone()
    }

    pub fn start_recording(&self, path: &Path) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.cmd.send(Cmd::Start(path.to_owned(), tx)).map_err(|_| "the camera stopped".to_owned())?;
        rx.recv_timeout(Duration::from_secs(15)).map_err(|_| "the recorder did not start".to_owned())?
    }

    pub fn stop_recording(&self) -> Result<PathBuf, String> {
        let (tx, rx) = mpsc::channel();
        self.cmd.send(Cmd::Stop(tx)).map_err(|_| "the camera stopped".to_owned())?;
        rx.recv_timeout(Duration::from_secs(60)).map_err(|_| "finishing the video timed out".to_owned())?
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        // a recording that is still open gets finished, so the file is playable
        if let Some(w) = self.shared.writer.lock().unwrap().take() {
            unsafe {
                let _ = w.0.writer.Finalize();
            }
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn video_thread(id: &str, sh: Arc<Shared>, sink: Sink, ready: mpsc::Sender<Result<(), String>>, cmds: mpsc::Receiver<Cmd>) {
    if let Err(e) = init_thread() {
        let _ = ready.send(Err(e));
        return;
    }
    let opened = unsafe {
        open_reader(MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, id, "camera").and_then(|(source, reader)| {
            let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
            let native = pick_video_mode(&reader).ok_or("The camera offers no usable video mode.")?;
            reader.SetCurrentMediaType(stream, None, &native).map_err(|e| describe(&e, "camera"))?;
            let out = MFCreateMediaType().map_err(|e| e.message())?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| e.message())?;
            out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).map_err(|e| e.message())?;
            if let Ok(fs) = native.GetUINT64(&MF_MT_FRAME_SIZE) {
                let _ = out.SetUINT64(&MF_MT_FRAME_SIZE, fs);
            }
            if let Ok(fr) = native.GetUINT64(&MF_MT_FRAME_RATE) {
                let _ = out.SetUINT64(&MF_MT_FRAME_RATE, fr);
            }
            reader.SetCurrentMediaType(stream, None, &out).map_err(|e| describe(&e, "camera"))?;
            let current = reader.GetCurrentMediaType(stream).map_err(|e| e.message())?;
            Ok((source, reader, current))
        })
    };
    let (source, reader, current) = match opened {
        Ok(v) => v,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let (w, h) = unpack(unsafe { current.GetUINT64(&MF_MT_FRAME_SIZE) }.unwrap_or(0));
    let (num, den) = unpack(unsafe { current.GetUINT64(&MF_MT_FRAME_RATE) }.unwrap_or(30 << 32 | 1));
    let fps = if den > 0 { num as f64 / den as f64 } else { 30.0 };
    sink(Event::Mode(format!("{w} × {h} · {} fps", (fps * 100.0).round() / 100.0)));
    let _ = ready.send(Ok(()));
    let frame_len = (10_000_000.0 / fps.max(1.0)) as i64;
    let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

    while !sh.stop.load(Ordering::Relaxed) {
        // commands from the UI (start / stop recording)
        while let Ok(cmd) = cmds.try_recv() {
            match cmd {
                Cmd::Start(path, reply) => {
                    let r = unsafe { make_writer(&path, &current, w, h, num, den, &sh) };
                    let _ = reply.send(r);
                }
                Cmd::Stop(reply) => {
                    let taken = sh.writer.lock().unwrap().take();
                    let r = match taken {
                        Some(wr) => unsafe { wr.0.writer.Finalize().map(|_| wr.0.path.clone()).map_err(|e| format!("Finishing the video failed: {}", e.message())) },
                        None => Err("Not recording.".into()),
                    };
                    let _ = reply.send(r);
                }
            }
        }
        let mut flags = 0u32;
        let mut ts = 0i64;
        let mut sample: Option<IMFSample> = None;
        let r = unsafe { reader.ReadSample(stream, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample)) };
        if let Err(e) = r {
            sink(Event::Error(format!("The camera stopped: {}", e.message())));
            break;
        }
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            sink(Event::Error("The camera stopped.".into()));
            break;
        }
        let Some(sample) = sample else { continue };
        if let Some(frame) = unsafe { to_frame(&sample, w, h) } {
            *sh.latest.lock().unwrap() = Some(frame.clone());
            sink(Event::Frame(frame));
        }
        let mut guard = sh.writer.lock().unwrap();
        if let Some(wr) = guard.as_mut() {
            let wr = &mut wr.0;
            let t = Writer::retime(&mut wr.video_base, wr.start, ts);
            unsafe {
                let _ = sample.SetSampleTime(t);
                let _ = sample.SetSampleDuration(frame_len);
                if let Err(e) = wr.writer.WriteSample(wr.video, &sample) {
                    sink(Event::Error(format!("Recording failed: {}", e.message())));
                }
            }
        }
    }
    drop(reader);
    unsafe {
        let _ = source.Shutdown();
    }
}

/// The RGB32 sample as a top-down BGRA frame.
unsafe fn to_frame(sample: &IMFSample, w: u32, h: u32) -> Option<Frame> {
    unsafe {
        let buf = sample.ConvertToContiguousBuffer().ok()?;
        let row = w as usize * 4;
        let mut out = vec![0u8; row * h as usize];
        if let Ok(b2) = buf.cast::<IMF2DBuffer>() {
            let mut p: *mut u8 = std::ptr::null_mut();
            let mut pitch = 0i32;
            b2.Lock2D(&mut p, &mut pitch).ok()?;
            // scanline 0 is the top row; a negative pitch walks up through memory (bottom-up buffer)
            for y in 0..h as usize {
                let src = p.offset(y as isize * pitch as isize);
                std::ptr::copy_nonoverlapping(src, out.as_mut_ptr().add(y * row), row);
            }
            let _ = b2.Unlock2D();
        } else {
            let mut p: *mut u8 = std::ptr::null_mut();
            let mut len = 0u32;
            buf.Lock(&mut p, None, Some(&mut len)).ok()?;
            let n = out.len().min(len as usize);
            std::ptr::copy_nonoverlapping(p, out.as_mut_ptr(), n);
            let _ = buf.Unlock();
        }
        for px in out.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
        Some(Frame { bgra: Arc::new(out), width: w, height: h })
    }
}

unsafe fn make_writer(path: &Path, video_in: &IMFMediaType, w: u32, h: u32, num: u32, den: u32, sh: &Shared) -> Result<(), String> {
    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 3).map_err(|e| e.message())?;
        let attrs = attrs.unwrap();
        let _ = attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1);
        let _ = attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1);
        let _ = attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4);
        let wide: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
        let writer = MFCreateSinkWriterFromURL(PCWSTR(wide.as_ptr()), None, &attrs).map_err(|e| format!("Cannot create the video file: {}", e.message()))?;

        let vout = MFCreateMediaType().map_err(|e| e.message())?;
        vout.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| e.message())?;
        vout.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264).map_err(|e| e.message())?;
        // 8 Mbit/s at 1080p30, less for smaller modes
        let bitrate = (VIDEO_BITRATE as f64 * (w as f64 * h as f64 / (1920.0 * 1080.0)).clamp(0.25, 1.0)) as u32;
        vout.SetUINT32(&MF_MT_AVG_BITRATE, bitrate).map_err(|e| e.message())?;
        vout.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(|e| e.message())?;
        vout.SetUINT64(&MF_MT_FRAME_SIZE, (w as u64) << 32 | h as u64).map_err(|e| e.message())?;
        vout.SetUINT64(&MF_MT_FRAME_RATE, (num as u64) << 32 | den.max(1) as u64).map_err(|e| e.message())?;
        vout.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, 1u64 << 32 | 1).map_err(|e| e.message())?;
        let video = writer.AddStream(&vout).map_err(|e| format!("No H.264 encoder: {}", e.message()))?;
        writer.SetInputMediaType(video, video_in, None).map_err(|e| format!("The H.264 encoder does not take the camera format: {}", e.message()))?;

        let mut audio = None;
        if let Some(pcm) = sh.audio_type.lock().unwrap().as_ref() {
            let pcm = &pcm.0;
            let rate = pcm.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).unwrap_or(48000);
            let ch = pcm.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).unwrap_or(2);
            let aout = MFCreateMediaType().map_err(|e| e.message())?;
            let _ = aout.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio);
            let _ = aout.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC);
            let _ = aout.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16);
            let _ = aout.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate);
            let _ = aout.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, ch);
            // the AAC encoder takes 12000 / 16000 / 20000 / 24000 bytes per second
            let _ = aout.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AUDIO_BITRATE / 8);
            match writer.AddStream(&aout).and_then(|i| writer.SetInputMediaType(i, pcm, None).map(|_| i)) {
                Ok(i) => audio = Some(i),
                Err(e) => eprintln!("naTiVeFS: audio is not recorded: {}", e.message()),
            }
        }
        writer.BeginWriting().map_err(|e| format!("Recording could not start: {}", e.message()))?;
        *sh.writer.lock().unwrap() = Some(Mt(Writer { writer, video, audio, path: path.to_owned(), start: Instant::now(), video_base: None, audio_base: None }));
        Ok(())
    }
}

/// The microphone alone (voice notes).
pub struct Mic {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Mic {
    pub fn start(id: &str, pcm: PcmSink, sink: Sink) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel();
        let (st, id) = (stop.clone(), id.to_owned());
        let thread = std::thread::Builder::new()
            .name("nativefs-voice".into())
            .spawn(move || mic_thread(&id, st, pcm, sink, ready_tx))
            .map_err(|e| e.to_string())?;
        let mic = Self { stop, thread: Some(thread) };
        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(mic),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("The microphone did not start (timeout).".into()),
        }
    }
}

impl Drop for Mic {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// 16-bit PCM from the microphone: 48 kHz when the reader converts to it (Opus wants 48 kHz), otherwise the device rate.
fn mic_thread(id: &str, stop: Arc<AtomicBool>, pcm_out: PcmSink, sink: Sink, ready: mpsc::Sender<Result<(), String>>) {
    if let Err(e) = init_thread() {
        let _ = ready.send(Err(e));
        return;
    }
    let opened = unsafe {
        open_reader(MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_AUDCAP_GUID, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_AUDCAP_ENDPOINT_ID, id, "microphone").and_then(|(source, reader)| {
            let stream = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
            let native = reader.GetNativeMediaType(stream, 0).map_err(|e| e.message())?;
            let native_rate = native.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).unwrap_or(48000);
            let ch = native.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).unwrap_or(2).clamp(1, 2);
            let mut last = String::new();
            for rate in [48000, native_rate] {
                let pcm = MFCreateMediaType().map_err(|e| e.message())?;
                let _ = pcm.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio);
                let _ = pcm.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM);
                let _ = pcm.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16);
                let _ = pcm.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate);
                let _ = pcm.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, ch);
                let _ = pcm.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, ch * 2);
                let _ = pcm.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, rate * ch * 2);
                match reader.SetCurrentMediaType(stream, None, &pcm) {
                    Ok(()) => return Ok((source, reader, rate, ch as u16)),
                    Err(e) => last = describe(&e, "microphone"),
                }
            }
            Err(last)
        })
    };
    let (source, reader, rate, ch) = match opened {
        Ok(v) => v,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    let stream = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
    let mut last_level = Instant::now();
    let mut peak = 0f32;
    while !stop.load(Ordering::Relaxed) {
        let mut flags = 0u32;
        let mut sample: Option<IMFSample> = None;
        if unsafe { reader.ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut sample)) }.is_err() {
            sink(Event::Error("The microphone stopped.".into()));
            break;
        }
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            break;
        }
        let Some(sample) = sample else { continue };
        unsafe {
            if let Ok(buf) = sample.ConvertToContiguousBuffer() {
                let mut p: *mut u8 = std::ptr::null_mut();
                let mut len = 0u32;
                if buf.Lock(&mut p, None, Some(&mut len)).is_ok() {
                    let samples = std::slice::from_raw_parts(p as *const i16, len as usize / 2);
                    peak = peak.max(peak_i16(samples));
                    pcm_out(samples, rate, ch);
                    let _ = buf.Unlock();
                }
            }
        }
        if last_level.elapsed() >= Duration::from_millis(50) {
            sink(Event::Level(peak));
            peak = 0.0;
            last_level = Instant::now();
        }
    }
    drop(reader);
    unsafe {
        let _ = source.Shutdown();
    }
}

/// Encode 16-bit PCM to AAC in an M4A file (the voice note's AAC formats). `kbps`: 96 / 160 / 192
/// (the Windows AAC encoder takes 12000 / 16000 / 20000 / 24000 bytes per second).
pub fn encode_aac(samples: &[i16], rate: u32, ch: u16, kbps: u32, path: &Path) -> Result<(), String> {
    init_thread()?;
    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 1).map_err(|e| e.message())?;
        let attrs = attrs.unwrap();
        let _ = attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4);
        let wide: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
        let writer = MFCreateSinkWriterFromURL(PCWSTR(wide.as_ptr()), None, &attrs).map_err(|e| format!("Cannot create the file: {}", e.message()))?;
        let ch32 = ch as u32;
        let aout = MFCreateMediaType().map_err(|e| e.message())?;
        let _ = aout.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio);
        let _ = aout.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC);
        let _ = aout.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16);
        let _ = aout.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate);
        let _ = aout.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, ch32);
        let _ = aout.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, kbps * 1000 / 8);
        let pcm = MFCreateMediaType().map_err(|e| e.message())?;
        let _ = pcm.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio);
        let _ = pcm.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM);
        let _ = pcm.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16);
        let _ = pcm.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate);
        let _ = pcm.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, ch32);
        let _ = pcm.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, ch32 * 2);
        let _ = pcm.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, rate * ch32 * 2);
        let idx = writer.AddStream(&aout).map_err(|e| format!("No AAC encoder for this format: {}", e.message()))?;
        writer.SetInputMediaType(idx, &pcm, None).map_err(|e| format!("The AAC encoder does not take this format: {}", e.message()))?;
        writer.BeginWriting().map_err(|e| e.message())?;
        // 100 ms pieces
        let piece = (rate as usize / 10) * ch as usize;
        let mut frames_done = 0u64;
        for chunk in samples.chunks(piece.max(ch as usize)) {
            let bytes = chunk.len() * 2;
            let buf = MFCreateMemoryBuffer(bytes as u32).map_err(|e| e.message())?;
            let mut p: *mut u8 = std::ptr::null_mut();
            buf.Lock(&mut p, None, None).map_err(|e| e.message())?;
            std::ptr::copy_nonoverlapping(chunk.as_ptr() as *const u8, p, bytes);
            let _ = buf.Unlock();
            buf.SetCurrentLength(bytes as u32).map_err(|e| e.message())?;
            let sample = MFCreateSample().map_err(|e| e.message())?;
            sample.AddBuffer(&buf).map_err(|e| e.message())?;
            let frames = (chunk.len() / ch as usize) as u64;
            let _ = sample.SetSampleTime((frames_done * 10_000_000 / rate as u64) as i64);
            let _ = sample.SetSampleDuration((frames * 10_000_000 / rate as u64) as i64);
            writer.WriteSample(idx, &sample).map_err(|e| e.message())?;
            frames_done += frames;
        }
        writer.Finalize().map_err(|e| format!("Finishing the file failed: {}", e.message()))
    }
}

fn audio_thread(id: &str, sh: Arc<Shared>, sink: Sink, ready: mpsc::Sender<Result<(), String>>) {
    if let Err(e) = init_thread() {
        let _ = ready.send(Err(e));
        return;
    }
    let opened = unsafe {
        open_reader(MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_AUDCAP_GUID, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_AUDCAP_ENDPOINT_ID, id, "microphone").and_then(|(source, reader)| {
            let stream = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
            let native = reader.GetNativeMediaType(stream, 0).map_err(|e| e.message())?;
            let rate = native.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).unwrap_or(48000);
            let ch = native.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).unwrap_or(2).clamp(1, 2);
            // 16-bit PCM at the device rate (what the AAC encoder takes); 44.1 / 48 kHz only
            let rate = if rate == 44100 { 44100 } else { 48000 };
            let pcm = MFCreateMediaType().map_err(|e| e.message())?;
            let _ = pcm.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio);
            let _ = pcm.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM);
            let _ = pcm.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16);
            let _ = pcm.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate);
            let _ = pcm.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, ch);
            let _ = pcm.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, ch * 2);
            let _ = pcm.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, rate * ch * 2);
            reader.SetCurrentMediaType(stream, None, &pcm).map_err(|e| describe(&e, "microphone"))?;
            let current = reader.GetCurrentMediaType(stream).map_err(|e| e.message())?;
            Ok((source, reader, current))
        })
    };
    let (source, reader, current) = match opened {
        Ok(v) => v,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    *sh.audio_type.lock().unwrap() = Some(Mt(current));
    let _ = ready.send(Ok(()));
    let stream = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
    let mut last_level = Instant::now();
    let mut peak = 0f32;
    while !sh.stop.load(Ordering::Relaxed) {
        let mut flags = 0u32;
        let mut ts = 0i64;
        let mut sample: Option<IMFSample> = None;
        if unsafe { reader.ReadSample(stream, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample)) }.is_err() {
            sink(Event::Error("The microphone stopped.".into()));
            break;
        }
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            break;
        }
        let Some(sample) = sample else { continue };
        // level meter (~20 updates a second)
        unsafe {
            if let Ok(buf) = sample.ConvertToContiguousBuffer() {
                let mut p: *mut u8 = std::ptr::null_mut();
                let mut len = 0u32;
                if buf.Lock(&mut p, None, Some(&mut len)).is_ok() {
                    let pcm = std::slice::from_raw_parts(p as *const i16, len as usize / 2);
                    peak = peak.max(peak_i16(pcm));
                    let _ = buf.Unlock();
                }
            }
        }
        if last_level.elapsed() >= Duration::from_millis(50) {
            sink(Event::Level(peak));
            peak = 0.0;
            last_level = Instant::now();
        }
        let mut guard = sh.writer.lock().unwrap();
        if let Some(wr) = guard.as_mut() {
            let wr = &mut wr.0;
            if let Some(idx) = wr.audio {
                let t = Writer::retime(&mut wr.audio_base, wr.start, ts);
                unsafe {
                    let _ = sample.SetSampleTime(t);
                    let _ = wr.writer.WriteSample(idx, &sample);
                }
            }
        }
    }
    drop(reader);
    unsafe {
        let _ = source.Shutdown();
    }
}
