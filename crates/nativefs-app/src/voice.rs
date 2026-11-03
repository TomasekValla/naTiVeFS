//! "Record a voice note" in Create mode (the web's voice recorder inside the drop zone): the microphone
//! records 16-bit PCM into memory (a live waveform), "Listen" plays it back through the audio engine (a temporary
//! WAV; the waveform of the whole take shows the progress, a click plays from there), and
//! "Use recording" encodes it in the chosen format and queues the file for upload. Formats (owner): Opus
//! 128 kbit/s in Ogg (default, plays in browsers), AAC 96 / 160 / 192 kbit/s in M4A, FLAC.

use std::cell::RefCell;
use std::rc::Rc;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::{capture, upload_ui, MainWindow, Shared};

/// Recordings stop by themselves after this long (they are kept in memory: 48 kHz stereo is ~11 MB a minute).
const MAX_SECONDS: u64 = 30 * 60;

/// Interleaved 16-bit PCM.
#[derive(Debug, Clone, Default)]
pub struct Recording {
    pub samples: Vec<i16>,
    pub rate: u32,
    pub channels: u16,
}

impl Recording {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1) as usize
    }
}

struct Voice {
    mic: Option<capture::Mic>,
    buf: Arc<Mutex<Recording>>,
    recording: Arc<AtomicBool>,
    started: Instant,
    timer: Option<slint::Timer>,
    take: Option<Arc<Recording>>,
    wav: Option<PathBuf>,
    preview: Option<nativefs_audio::engine::Player>,
    /// Updates the review's progress and time while the preview plays.
    preview_timer: Option<slint::Timer>,
    /// The waveform's rows (updated in place, never rebuilt).
    bars: Rc<VecModel<f32>>,
}

thread_local! {
    static VOICE: RefCell<Option<Voice>> = const { RefCell::new(None) };
}

/// Bars in the waveform.
const BARS: usize = 40;

fn fmt_time(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn status(ui: &MainWindow, state: i32, text: &str, error: bool) {
    ui.set_voice_state(state);
    ui.set_voice_status(text.into());
    ui.set_voice_error(error);
}

/// A peak (0..1) as a bar height: decibels over a 50 dB range, so quiet speech still shows.
fn bar_height(peak: f32) -> f32 {
    let db = 20.0 * peak.max(1e-6).log10();
    ((db + 50.0) / 50.0).clamp(0.08, 1.0)
}

fn set_bars(bars: &VecModel<f32>, values: impl Iterator<Item = f32>) {
    for (i, v) in values.take(BARS).enumerate() {
        if (bars.row_data(i).unwrap_or(-1.0) - v).abs() > 1e-4 {
            bars.set_row_data(i, v);
        }
    }
}

/// While recording: the newest level enters on the right, the rest moves left.
fn push_level(peak: f32) {
    VOICE.with(|v| {
        let v = v.borrow();
        let Some(v) = v.as_ref() else { return };
        if !v.recording.load(Ordering::Relaxed) {
            return;
        }
        let old: Vec<f32> = (1..BARS).map(|i| v.bars.row_data(i).unwrap_or(0.08)).collect();
        set_bars(&v.bars, old.into_iter().chain(std::iter::once(bar_height(peak))));
    });
}

/// The whole recording as bars (the review's waveform).
fn take_bars(rec: &Recording) -> Vec<f32> {
    let ch = rec.channels.max(1) as usize;
    let frames = rec.frames().max(1);
    (0..BARS)
        .map(|b| {
            let (a, z) = (b * frames / BARS, ((b + 1) * frames / BARS).max(b * frames / BARS + 1).min(frames));
            bar_height(capture::peak_i16(&rec.samples[a * ch..z * ch]))
        })
        .collect()
}

/// "Record a voice note": open the recorder and the microphone.
pub fn open(ui: &MainWindow, shared: &Arc<Shared>) {
    if VOICE.with(|v| v.borrow().is_some()) {
        return;
    }
    ui.set_voice_format(shared.config.lock().unwrap().voice_format.min(4) as i32);
    ui.set_voice_time("0:00".into());
    ui.set_voice_listening(false);
    ui.set_voice_progress(0.0);
    let bars = Rc::new(VecModel::from(vec![0.08f32; BARS]));
    ui.set_voice_bars(ModelRc::from(bars.clone()));
    status(ui, 1, "Requesting microphone…", false);
    let buf = Arc::new(Mutex::new(Recording::default()));
    let recording = Arc::new(AtomicBool::new(false));
    let pcm: capture::PcmSink = {
        let (buf, recording) = (buf.clone(), recording.clone());
        Arc::new(move |samples: &[i16], rate: u32, channels: u16| {
            if recording.load(Ordering::Relaxed) {
                let mut b = buf.lock().unwrap();
                b.rate = rate;
                b.channels = channels;
                b.samples.extend_from_slice(samples);
            }
        })
    };
    let sink: capture::Sink = {
        let weak = ui.as_weak();
        Arc::new(move |ev| {
            let weak = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = weak.upgrade() else { return };
                match ev {
                    capture::Event::Level(l) => push_level(l),
                    capture::Event::Error(e) => status(&ui, ui.get_voice_state().max(2), &e, true),
                    _ => {}
                }
            });
        })
    };
    let mic_id = shared.config.lock().unwrap().mic_device.clone();
    let (mic, result) = match capture::Mic::start(&mic_id, pcm, sink) {
        Ok(mic) => (Some(mic), Ok(())),
        Err(e) => (None, Err(e)),
    };
    VOICE.with(|v| {
        *v.borrow_mut() = Some(Voice {
            mic,
            buf,
            recording,
            started: Instant::now(),
            timer: None,
            take: None,
            wav: None,
            preview: None,
            preview_timer: None,
            bars,
        })
    });
    match result {
        Ok(()) => status(ui, 2, "Ready when you are", false),
        // the web: "Microphone access denied or unavailable." with only Cancel left
        Err(e) => status(ui, 1, &e, true),
    }
}

/// The recorder's buttons: 0 start, 1 stop, 2 use, 3 discard & retry, 4 cancel, 5 listen / pause.
pub fn action(ui: &MainWindow, shared: &Arc<Shared>, what: i32) {
    match what {
        0 => start(ui),
        1 => stop(ui),
        2 => use_recording(ui, shared),
        3 => discard(ui),
        4 => close(ui),
        5 => listen(ui),
        _ => {}
    }
}

fn start(ui: &MainWindow) {
    VOICE.with(|v| {
        let mut v = v.borrow_mut();
        let Some(v) = v.as_mut() else { return };
        if v.mic.is_none() {
            return;
        }
        *v.buf.lock().unwrap() = Recording::default();
        set_bars(&v.bars, std::iter::repeat(0.08));
        v.started = Instant::now();
        v.recording.store(true, Ordering::Relaxed);
        let timer = slint::Timer::default();
        let weak = ui.as_weak();
        let started = v.started;
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(250), move || {
            let Some(ui) = weak.upgrade() else { return };
            let secs = started.elapsed().as_secs();
            ui.set_voice_time(fmt_time(secs).into());
            if secs >= MAX_SECONDS {
                stop(&ui);
            }
        });
        v.timer = Some(timer);
    });
    ui.set_voice_time("0:00".into());
    status(ui, 3, "Recording…", false);
}

fn stop(ui: &MainWindow) {
    let take = VOICE.with(|v| {
        let mut v = v.borrow_mut();
        let v = v.as_mut()?;
        v.recording.store(false, Ordering::Relaxed);
        v.timer = None;
        let rec = std::mem::take(&mut *v.buf.lock().unwrap());
        Some(rec)
    });
    let Some(rec) = take else { return };
    if rec.frames() == 0 {
        status(ui, 2, "Nothing was recorded (the microphone sent no sound).", true);
        return;
    }
    let wav = capture::capture_dir().join(format!("voice-preview-{}.wav", std::process::id()));
    let _ = write_wav(&rec, &wav);
    let bars = take_bars(&rec);
    let total = rec.frames() as f64 / rec.rate.max(1) as f64;
    VOICE.with(|v| {
        if let Some(v) = v.borrow_mut().as_mut() {
            set_bars(&v.bars, bars.into_iter());
            v.take = Some(Arc::new(rec));
            v.wav = Some(wav);
        }
    });
    ui.set_voice_progress(0.0);
    ui.set_voice_time(format!("0:00 / {}", fmt_time(total.round() as u64)).into());
    status(ui, 4, "Recording ready — listen back or use it", false);
}

/// Length of the take in seconds.
fn take_seconds() -> f64 {
    VOICE.with(|v| v.borrow().as_ref().and_then(|v| v.take.as_ref().map(|t| t.frames() as f64 / t.rate.max(1) as f64))).unwrap_or(0.0)
}

/// Review: progress bar and "position / length" while the preview plays.
fn show_position(ui: &MainWindow) {
    let total = take_seconds();
    let pos = VOICE.with(|v| {
        let v = v.borrow();
        let v = v.as_ref()?;
        let p = v.preview.as_ref()?;
        let rate = v.take.as_ref()?.rate.max(1) as f64;
        Some(p.position() as f64 / rate)
    });
    let pos = pos.unwrap_or(0.0).min(total);
    ui.set_voice_progress(if total > 0.0 { (pos / total) as f32 } else { 0.0 });
    ui.set_voice_time(format!("{} / {}", fmt_time(pos as u64), fmt_time(total.round() as u64)).into());
}

/// The preview ended (or failed): back to the start, not playing.
fn preview_ended(ui: &MainWindow) {
    VOICE.with(|v| {
        if let Some(v) = v.borrow_mut().as_mut() {
            v.preview = None;
            v.preview_timer = None;
        }
    });
    ui.set_voice_listening(false);
    ui.set_voice_progress(0.0);
    let total = take_seconds();
    ui.set_voice_time(format!("0:00 / {}", fmt_time(total.round() as u64)).into());
}

/// The preview player (created on the first Listen), through the system mixer so other apps keep playing.
fn ensure_preview(ui: &MainWindow) -> bool {
    if VOICE.with(|v| v.borrow().as_ref().is_some_and(|v| v.preview.is_some())) {
        return true;
    }
    let Some(wav) = VOICE.with(|v| v.borrow().as_ref().and_then(|v| v.wav.clone())) else { return false };
    let dec = match nativefs_audio::decode::Decoder::open_path(&wav) {
        Ok(d) => d,
        Err(e) => {
            status(ui, 4, &format!("Cannot play the recording: {e}"), true);
            return false;
        }
    };
    let settings = nativefs_audio::engine::OutputSettings {
        device: None,
        mode: nativefs_audio::output::ShareMode::Shared,
        pause: nativefs_audio::engine::PauseBehavior::Keep,
    };
    let weak = ui.as_weak();
    let events: nativefs_audio::engine::EventSink = Box::new(move |ev| {
        if matches!(ev, nativefs_audio::engine::Event::Ended | nativefs_audio::engine::Event::Error(_)) {
            let weak = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    preview_ended(&ui);
                }
            });
        }
    });
    let player = nativefs_audio::engine::Player::new(dec, settings, events);
    // debug builds: `NATIVEFS_AUDIO_MUTE` keeps the preview silent (tests)
    if cfg!(debug_assertions) && std::env::var_os("NATIVEFS_AUDIO_MUTE").is_some() {
        player.set_muted(true);
    }
    let timer = slint::Timer::default();
    let weak = ui.as_weak();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(50), move || {
        if let Some(ui) = weak.upgrade() {
            show_position(&ui);
        }
    });
    VOICE.with(|v| {
        if let Some(v) = v.borrow_mut().as_mut() {
            v.preview = Some(player);
            v.preview_timer = Some(timer);
        }
    });
    true
}

/// Listen / Pause.
fn listen(ui: &MainWindow) {
    if ui.get_voice_listening() {
        VOICE.with(|v| {
            if let Some(p) = v.borrow().as_ref().and_then(|v| v.preview.as_ref()) {
                p.pause();
            }
        });
        ui.set_voice_listening(false);
        return;
    }
    if !ensure_preview(ui) {
        return;
    }
    VOICE.with(|v| {
        if let Some(p) = v.borrow().as_ref().and_then(|v| v.preview.as_ref()) {
            p.play();
        }
    });
    ui.set_voice_listening(true);
}

/// A click on the review's waveform: play from there.
pub fn seek(ui: &MainWindow, at: f32) {
    if !ensure_preview(ui) {
        return;
    }
    VOICE.with(|v| {
        let v = v.borrow();
        let Some(v) = v.as_ref() else { return };
        let (Some(p), Some(t)) = (v.preview.as_ref(), v.take.as_ref()) else { return };
        p.seek((t.frames() as f64 * at.clamp(0.0, 1.0) as f64) as u64);
        p.play();
    });
    ui.set_voice_listening(true);
    show_position(ui);
}

fn discard(ui: &MainWindow) {
    preview_ended(ui);
    VOICE.with(|v| {
        if let Some(v) = v.borrow_mut().as_mut() {
            v.take = None;
            set_bars(&v.bars, std::iter::repeat(0.08));
            if let Some(w) = v.wav.take() {
                let _ = std::fs::remove_file(w);
            }
        }
    });
    ui.set_voice_time("0:00".into());
    status(ui, 2, "Ready when you are", false);
}

/// Close the recorder: the microphone, the preview and the temporary WAV go away.
pub fn close(ui: &MainWindow) {
    let v = VOICE.with(|v| v.borrow_mut().take());
    if let Some(mut v) = v {
        v.recording.store(false, Ordering::Relaxed);
        v.timer = None;
        v.preview_timer = None;
        v.preview = None;
        if let Some(w) = v.wav.take() {
            let _ = std::fs::remove_file(w);
        }
        // the microphone thread joins here
        drop(v.mic.take());
    }
    ui.set_voice_state(0);
    ui.set_voice_listening(false);
}

fn use_recording(ui: &MainWindow, shared: &Arc<Shared>) {
    preview_ended(ui);
    let Some(take) = VOICE.with(|v| v.borrow().as_ref().and_then(|v| v.take.clone())) else { return };
    let format = ui.get_voice_format().clamp(0, 4) as usize;
    status(ui, 5, "Encoding…", false);
    let weak = ui.as_weak();
    let _ = shared;
    std::thread::spawn(move || {
        let result = encode(&take, format, &capture::capture_dir());
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            match result {
                Ok(path) => {
                    close(&ui);
                    upload_ui::add_paths(&ui, vec![path]);
                }
                Err(e) => status(&ui, 4, &format!("Encoding failed: {e}"), true),
            }
        });
    });
}

/// The chosen format was changed (remembered).
pub fn format_picked(shared: &Arc<Shared>, i: i32) {
    let mut cfg = shared.config.lock().unwrap();
    cfg.voice_format = i.clamp(0, 4) as u8;
    let _ = cfg.save();
}

/// Encode `rec` in `format` (0 Opus 128, 1-3 AAC 96 / 160 / 192, 4 FLAC) into `dir`, named like the web's
/// `voice-note-<ISO time>`.
pub fn encode(rec: &Recording, format: usize, dir: &Path) -> Result<PathBuf, String> {
    let stamp = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S-%3fZ");
    let ext = match format {
        0 => "opus",
        4 => "flac",
        _ => "m4a",
    };
    let path = dir.join(format!("voice-note-{stamp}.{ext}"));
    match format {
        0 => std::fs::write(&path, encode_opus(rec)?).map_err(|e| e.to_string())?,
        4 => std::fs::write(&path, encode_flac(rec)?).map_err(|e| e.to_string())?,
        n => {
            let kbps = [96, 160, 192][n.clamp(1, 3) - 1];
            capture::encode_aac(&rec.samples, rec.rate, rec.channels, kbps, &path)?;
        }
    }
    Ok(path)
}

/// Linear resampling (only when the microphone could not deliver 48 kHz for Opus; plenty for speech).
fn resample(samples: &[i16], ch: usize, from: u32, to: u32) -> Vec<i16> {
    let frames = samples.len() / ch;
    if frames == 0 || from == to {
        return samples.to_vec();
    }
    let out_frames = (frames as u64 * to as u64 / from as u64) as usize;
    let mut out = Vec::with_capacity(out_frames * ch);
    for i in 0..out_frames {
        let pos = i as f64 * from as f64 / to as f64;
        let a = (pos as usize).min(frames - 1);
        let b = (a + 1).min(frames - 1);
        let t = pos - a as f64;
        for c in 0..ch {
            let v = samples[a * ch + c] as f64 * (1.0 - t) + samples[b * ch + c] as f64 * t;
            out.push(v.round() as i16);
        }
    }
    out
}

/// Ogg Opus (RFC 7845): OpusHead + OpusTags, then 20 ms packets at 48 kHz, 128 kbit/s.
pub fn encode_opus(rec: &Recording) -> Result<Vec<u8>, String> {
    let ch = rec.channels.clamp(1, 2) as usize;
    let pcm = resample(&rec.samples, ch, rec.rate, 48000);
    let total = pcm.len() / ch;
    let err = |e: opus::Error| format!("Opus: {e}");
    let mut enc = opus::Encoder::new(48000, if ch == 1 { opus::Channels::Mono } else { opus::Channels::Stereo }, opus::Application::Audio).map_err(err)?;
    enc.set_bitrate(opus::Bitrate::Bits(128_000)).map_err(err)?;
    let pre_skip = enc.get_lookahead().map_err(err)?.max(0) as u64;
    let serial = 0x5654_4653;
    let mut out = Vec::new();
    {
        let mut w = ogg::PacketWriter::new(&mut out);
        let mut head = b"OpusHead".to_vec();
        head.push(1);
        head.push(ch as u8);
        head.extend_from_slice(&(pre_skip as u16).to_le_bytes());
        head.extend_from_slice(&rec.rate.to_le_bytes());
        head.extend_from_slice(&0i16.to_le_bytes());
        head.push(0);
        w.write_packet(head, serial, ogg::PacketWriteEndInfo::EndPage, 0).map_err(|e| e.to_string())?;
        let vendor = b"naTiVeFS";
        let mut tags = b"OpusTags".to_vec();
        tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        tags.extend_from_slice(vendor);
        tags.extend_from_slice(&0u32.to_le_bytes());
        w.write_packet(tags, serial, ogg::PacketWriteEndInfo::EndPage, 0).map_err(|e| e.to_string())?;
        const FRAME: usize = 960;
        let mut packet = vec![0u8; 4000];
        let mut frame = vec![0i16; FRAME * ch];
        let mut done = 0usize;
        while done < total {
            let n = (total - done).min(FRAME);
            frame.fill(0);
            frame[..n * ch].copy_from_slice(&pcm[done * ch..(done + n) * ch]);
            let len = enc.encode(&frame, &mut packet).map_err(err)?;
            done += n;
            let last = done >= total;
            let info = if last { ogg::PacketWriteEndInfo::EndStream } else { ogg::PacketWriteEndInfo::NormalPacket };
            w.write_packet(packet[..len].to_vec(), serial, info, pre_skip + done as u64).map_err(|e| e.to_string())?;
        }
    }
    Ok(out)
}

/// FLAC, 16 bit at the recording's rate. The frames are encoded one by one: flacenc 0.4's own loop pads the last
/// block to the full block size (extra silence, and the MD5 no longer matches what decoders output).
pub fn encode_flac(rec: &Recording) -> Result<Vec<u8>, String> {
    use flacenc::component::BitRepr;
    use flacenc::error::Verify;
    use flacenc::source::Fill;
    let e = |e: &dyn std::fmt::Debug| format!("FLAC: {e:?}");
    let config = flacenc::config::Encoder::default().into_verified().map_err(|x| e(&x))?;
    let ch = rec.channels.max(1) as usize;
    // the totals and block sizes are filled in frame by frame; MD5 stays zero ("not computed", allowed by the format)
    let info = flacenc::component::StreamInfo::new(rec.rate as usize, ch, 16).map_err(|x| e(&x))?;
    let mut stream = flacenc::component::Stream::with_stream_info(info);
    let samples: Vec<i32> = rec.samples.iter().map(|s| *s as i32).collect();
    for (k, chunk) in samples.chunks(config.block_size * ch).enumerate() {
        let mut fb = flacenc::source::FrameBuf::with_size(ch, chunk.len() / ch).map_err(|x| e(&x))?;
        fb.fill_interleaved(chunk).map_err(|x| e(&x))?;
        let frame = flacenc::encode_fixed_size_frame(&config, &fb, k, stream.stream_info()).map_err(|x| e(&x))?;
        stream.add_frame(frame);
    }
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream.write(&mut sink).map_err(|x| e(&x))?;
    let mut out = sink.as_slice().to_vec();
    // STREAMINFO min block size (bytes 8-9, after "fLaC" and the block header) = the fixed size: a shorter last
    // block is allowed, but min != max marks a variable-block stream and decoders (Symphonia) then misread it
    if samples.len() > config.block_size * ch && out.len() > 12 {
        out.copy_within(10..12, 8);
    }
    Ok(out)
}

/// 16-bit PCM WAV (the preview the audio engine plays).
pub fn write_wav(rec: &Recording, path: &Path) -> std::io::Result<()> {
    let data = rec.samples.len() as u32 * 2;
    let ch = rec.channels.max(1);
    let mut b = Vec::with_capacity(44 + data as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&ch.to_le_bytes());
    b.extend_from_slice(&rec.rate.to_le_bytes());
    b.extend_from_slice(&(rec.rate * ch as u32 * 2).to_le_bytes());
    b.extend_from_slice(&(ch * 2).to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data.to_le_bytes());
    for s in &rec.samples {
        b.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, secs: f64) -> Recording {
        let n = (rate as f64 * secs) as usize;
        let mut samples = Vec::with_capacity(n * 2);
        for i in 0..n {
            let v = ((i as f64 * 440.0 * std::f64::consts::TAU / rate as f64).sin() * 12000.0) as i16;
            samples.extend_from_slice(&[v, v]);
        }
        Recording { samples, rate, channels: 2 }
    }

    #[test]
    fn opus_is_ogg_with_the_right_length() {
        let rec = tone(48000, 1.0);
        let bytes = encode_opus(&rec).unwrap();
        assert_eq!(&bytes[..4], b"OggS");
        assert!(bytes.windows(8).any(|w| w == b"OpusHead"));
        // ~128 kbit/s for one second, plus headers
        assert!((14_000..20_000).contains(&bytes.len()), "{}", bytes.len());
        // the last page's granule = pre-skip + 48000
        let last = bytes.windows(4).rposition(|w| w == b"OggS").unwrap();
        let granule = u64::from_le_bytes(bytes[last + 6..last + 14].try_into().unwrap());
        assert!((48000..49000).contains(&granule), "{granule}");
    }

    #[test]
    fn opus_resamples_44100() {
        let bytes = encode_opus(&tone(44100, 0.5)).unwrap();
        let last = bytes.windows(4).rposition(|w| w == b"OggS").unwrap();
        let granule = u64::from_le_bytes(bytes[last + 6..last + 14].try_into().unwrap());
        assert!((24000..25000).contains(&granule), "{granule}");
    }

    #[cfg(windows)]
    #[test]
    fn aac_m4a_plays_back() {
        let rec = tone(48000, 1.0);
        let path = std::env::temp_dir().join(format!("nativefs-voice-test-{}.m4a", std::process::id()));
        capture::encode_aac(&rec.samples, rec.rate, rec.channels, 160, &path).unwrap();
        let size = std::fs::metadata(&path).unwrap().len();
        let mut dec = nativefs_audio::decode::Decoder::open_path(&path).unwrap();
        let mut n = 0usize;
        let mut chunk = Vec::new();
        while dec.next(&mut chunk).unwrap() {
            n += chunk.len();
            chunk.clear();
        }
        let _ = std::fs::remove_file(&path);
        // ~160 kbit/s for one second, one second of stereo back (the encoder adds a little priming)
        assert!((15_000..30_000).contains(&size), "{size}");
        assert!((96_000..100_000).contains(&n), "{n}");
    }

    #[test]
    fn flac_decodes_back_bit_exact() {
        let rec = tone(48000, 0.5);
        let bytes = encode_flac(&rec).unwrap();
        let path = std::env::temp_dir().join(format!("nativefs-voice-test-{}.flac", std::process::id()));
        std::fs::write(&path, bytes).unwrap();
        let mut dec = nativefs_audio::decode::Decoder::open_path(&path).unwrap();
        let (mut got, mut chunk) = (Vec::new(), Vec::new());
        while dec.next(&mut chunk).unwrap() {
            got.extend(chunk.iter().map(|s| s >> 16));
            chunk.clear();
        }
        let _ = std::fs::remove_file(&path);
        let want: Vec<i32> = rec.samples.iter().map(|s| *s as i32).collect();
        assert_eq!(got.len(), want.len());
        assert_eq!(got, want);
    }
}
