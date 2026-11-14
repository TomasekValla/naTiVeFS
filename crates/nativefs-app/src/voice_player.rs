//! Voice note player: a voice note (see `Content::voice_note`, or a recording of the app's own voice recorder)
//! opens in this small chat-message window instead of the full audio player. Only speed and volume are offered.
//! The loudness is always normalized to -16 LUFS (gain clamped to -12..+18 dB) by the engine's DSP chain, whose
//! limiter keeps boosted quiet notes below full scale; Bit-perfect and exclusive output never apply here.
//! The frame timer runs only while playing and visible; paused or hidden nothing redraws.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use nativefs_audio::decode::Decoder;
use nativefs_audio::dsp::Params;
use nativefs_audio::engine::{Event, OutputSettings, PauseBehavior, Player};
use nativefs_audio::output::ShareMode;
use slint::{ComponentHandle, ModelRc, VecModel};
use tvfs_core::content::Content;

use crate::{windows, Shared, VoicePlayerWindow};

const TARGET_LUFS: f64 = -16.0;
const MIN_GAIN_DB: f64 = -12.0;
const MAX_GAIN_DB: f64 = 18.0;
const BARS: usize = 56;
const MAX_BYTES: u64 = 64 * 1024 * 1024;
const SPEEDS: [f64; 4] = [1.0, 1.25, 1.5, 2.0];

/// Normalization gain in dB for a measured loudness (0 when unknown).
pub fn gain_db_for(lufs: Option<f64>) -> f64 {
    lufs.map_or(0.0, |l| (TARGET_LUFS - l).clamp(MIN_GAIN_DB, MAX_GAIN_DB))
}

/// `voice-note-2026-11-14T13-05-22-123Z.opus` (the recorder's names) -> a local date and time.
fn date_from_name(name: &str) -> Option<String> {
    use chrono::{Local, NaiveDateTime, TimeZone};
    let i = name.find("voice-note-")? + "voice-note-".len();
    let stamp = name.get(i..i + 19)?;
    let naive = NaiveDateTime::parse_from_str(stamp, "%Y-%m-%dT%H-%M-%S").ok()?;
    let local = Local.from_utc_datetime(&naive);
    Some(local.format("%-d %b %Y, %H:%M").to_string())
}

/// Ogg Opus is the recorder's default format, but the engine's decoder (Symphonia) has no Opus codec: decode it with
/// libopus to a 16-bit WAV in memory, which the engine plays like any other file. Other formats are returned as they are.
fn prepare(bytes: Vec<u8>) -> Result<(Vec<u8>, Option<&'static str>), String> {
    if !(bytes.starts_with(b"OggS") && bytes.windows(8).take(80).any(|w| w == b"OpusHead")) {
        return Ok((bytes, None));
    }
    let err = |e: opus::Error| format!("Opus: {e}");
    let mut reader = ogg::PacketReader::new(std::io::Cursor::new(&bytes));
    let head = reader.read_packet().map_err(|e| e.to_string())?.ok_or("empty Opus file")?;
    if head.data.len() < 19 {
        return Err("broken Opus header".into());
    }
    let ch = head.data[9].clamp(1, 2) as usize;
    let pre_skip = u16::from_le_bytes([head.data[10], head.data[11]]) as usize;
    let mut dec = opus::Decoder::new(48000, if ch == 1 { opus::Channels::Mono } else { opus::Channels::Stereo }).map_err(err)?;
    let mut pcm: Vec<i16> = Vec::new();
    let mut frame = vec![0i16; 5760 * ch];
    let mut index = 0;
    while let Some(pk) = reader.read_packet().map_err(|e| e.to_string())? {
        index += 1;
        // the second packet is the tags
        if index == 1 && pk.data.starts_with(b"OpusTags") {
            continue;
        }
        let n = dec.decode(&pk.data, &mut frame, false).map_err(err)?;
        pcm.extend_from_slice(&frame[..n * ch]);
    }
    pcm.drain(..(pre_skip * ch).min(pcm.len()));
    let data = pcm.len() as u32 * 2;
    let mut w = Vec::with_capacity(44 + data as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&(ch as u16).to_le_bytes());
    w.extend_from_slice(&48000u32.to_le_bytes());
    w.extend_from_slice(&(48000 * ch as u32 * 2).to_le_bytes());
    w.extend_from_slice(&(ch as u16 * 2).to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data.to_le_bytes());
    for v in &pcm {
        w.extend_from_slice(&v.to_le_bytes());
    }
    Ok((w, Some("wav")))
}

fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0).floor() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

pub enum Source {
    Remote(Content),
    Local(PathBuf),
}

impl Source {
    fn key(&self) -> String {
        match self {
            Source::Remote(c) => c.url.clone(),
            Source::Local(p) => p.to_string_lossy().into_owned(),
        }
    }
    fn name(&self) -> String {
        match self {
            Source::Remote(c) => c.name.clone(),
            Source::Local(p) => p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        }
    }
}

/// Is this local file a recording of the app's voice recorder (or forced by the debug switch)?
pub fn is_own_recording(path: &std::path::Path) -> bool {
    if cfg!(debug_assertions) && std::env::var_os("NATIVEFS_VOICE_PLAYER").is_some() {
        return true;
    }
    let named = path.file_name().is_some_and(|n| n.to_string_lossy().starts_with("voice-note-"));
    named && path.parent().is_some_and(|d| d == crate::capture::capture_dir())
}

struct Loaded {
    player: Player,
    bars: Vec<f32>,
    gain_db: f64,
}

struct State {
    window: VoicePlayerWindow,
    source: Source,
    player: Option<Player>,
    rate: f64,
    total: f64,
    speed: usize,
    volume: f32,
    muted: bool,
    gain_db: f64,
    visible: bool,
    timer: Option<slint::Timer>,
}

thread_local! {
    static VOICES: RefCell<Vec<Rc<RefCell<State>>>> = const { RefCell::new(Vec::new()) };
}

fn find(w: &VoicePlayerWindow) -> Option<Rc<RefCell<State>>> {
    VOICES.with(|l| l.borrow().iter().find(|s| std::ptr::eq(s.borrow().window.window(), w.window())).cloned())
}

pub fn open(shared: &Arc<Shared>, source: Source) {
    let key = source.key();
    if let Some(s) = VOICES.with(|l| l.borrow().iter().find(|s| s.borrow().source.key() == key).cloned()) {
        windows::raise(s.borrow().window.window());
        return;
    }
    let Ok(w) = VoicePlayerWindow::new() else { return };
    let name = source.name();
    let date = date_from_name(&name).or_else(|| match &source {
        Source::Local(p) => std::fs::metadata(p).and_then(|m| m.modified()).ok().map(|t| {
            chrono::DateTime::<chrono::Local>::from(t).format("%-d %b %Y, %H:%M").to_string()
        }),
        Source::Remote(_) => None,
    });
    w.set_date(date.unwrap_or_default().into());
    w.set_status("Loading...".into());
    let state = Rc::new(RefCell::new(State {
        window: w.clone_strong(),
        source,
        player: None,
        rate: 48000.0,
        total: 0.0,
        speed: 0,
        volume: 1.0,
        muted: false,
        gain_db: 0.0,
        visible: true,
        timer: None,
    }));
    VOICES.with(|l| l.borrow_mut().push(state.clone()));
    wire(&w, &state);

    let mut hooks = windows::Hooks::new();
    {
        let sw = Rc::downgrade(&state);
        hooks.visible = Box::new(move |on| {
            if let Some(s) = sw.upgrade() {
                s.borrow_mut().visible = on;
                schedule(&s);
            }
        });
    }
    windows::attach(w.window(), hooks);
    {
        let sw = Rc::downgrade(&state);
        w.window().on_close_requested(move || {
            if let Some(s) = sw.upgrade() {
                {
                    let mut sb = s.borrow_mut();
                    sb.timer = None;
                    // dropping the player stops playback and releases the device
                    sb.player = None;
                }
                VOICES.with(|l| l.borrow_mut().retain(|x| !Rc::ptr_eq(x, &s)));
            }
            windows::closed();
            slint::CloseRequestResponse::HideWindow
        });
    }
    windows::place(w.window());
    let _ = w.show();
    windows::raise(w.window());
    load(&state, shared);
}

fn wire(w: &VoicePlayerWindow, state: &Rc<RefCell<State>>) {
    let sw = Rc::downgrade(state);
    macro_rules! on {
        ($setter:ident, |$s:ident $(, $a:ident)*| $body:expr) => {{
            let sw = sw.clone();
            w.$setter(move |$($a),*| {
                if let Some($s) = sw.upgrade() {
                    $body
                }
            });
        }};
    }
    on!(on_play_pause, |s| play_pause(&s));
    on!(on_seek, |s, at| seek_to(&s, at as f64));
    on!(on_seek_by, |s, dt| {
        let (pos, total) = {
            let sb = s.borrow();
            (sb.player.as_ref().map_or(0.0, |p| p.position() as f64 / sb.rate), sb.total)
        };
        if total > 0.0 {
            seek_to(&s, ((pos + dt as f64) / total).clamp(0.0, 1.0));
        }
    });
    on!(on_set_speed, |s, i| {
        s.borrow_mut().speed = (i.max(0) as usize).min(SPEEDS.len() - 1);
        s.borrow().window.set_speed_index(i.clamp(0, 3));
        apply(&s);
    });
    on!(on_set_volume, |s, v| {
        {
            let mut sb = s.borrow_mut();
            sb.volume = v;
            sb.muted = false;
            sb.window.set_volume(v);
            sb.window.set_muted(false);
        }
        apply(&s);
    });
    on!(on_toggle_mute, |s| {
        {
            let mut sb = s.borrow_mut();
            sb.muted = !sb.muted;
            let m = sb.muted;
            sb.window.set_muted(m);
        }
        apply(&s);
    });
}

/// Send speed, volume and the normalization gain to the engine. Pitch stays (the engine time-stretches).
fn apply(state: &Rc<RefCell<State>>) {
    let sb = state.borrow();
    let Some(p) = &sb.player else { return };
    p.set_muted(sb.muted);
    p.set_dsp(Params {
        tempo: SPEEDS[sb.speed],
        // the engine applies its own perceptual curve up to 100 %; above that a clean boost (up to +6 dB at 200 %)
        // goes into the normalization gain, so it passes the limiter like on the web instead of the music saturation
        volume: (sb.volume.min(1.0) as f64) * 100.0,
        norm_db: sb.gain_db + if sb.volume > 1.0 { 20.0 * (sb.volume as f64).log10() } else { 0.0 },
        ..Params::default()
    });
}

fn output_settings(shared: &Shared) -> OutputSettings {
    let cfg = shared.config.lock().unwrap();
    OutputSettings {
        device: (!cfg.audio_device.is_empty()).then(|| cfg.audio_device.clone()),
        // shared mode and release on pause: the normalization is never bypassed, and the device is free when paused
        mode: ShareMode::Shared,
        pause: PauseBehavior::Release,
    }
}

fn load(state: &Rc<RefCell<State>>, shared: &Arc<Shared>) {
    let weak = state.borrow().window.as_weak();
    let source = match &state.borrow().source {
        Source::Remote(c) => Err(c.clone()),
        Source::Local(p) => Ok(p.clone()),
    };
    let settings = output_settings(shared);
    let shared = shared.clone();
    let events = {
        let weak = weak.clone();
        Box::new(move |e: Event| {
            let _ = weak.upgrade_in_event_loop(move |w| {
                if let Some(s) = find(&w) {
                    on_event(&s, e);
                }
            });
        })
    };
    std::thread::Builder::new()
        .name("voice-open".into())
        .spawn(move || {
            let result: Result<Loaded, String> = (|| {
                let (bytes, name) = match source {
                    Ok(path) => {
                        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        (std::fs::read(&path).map_err(|e| e.to_string())?, name)
                    }
                    Err(c) => {
                        let b = shared.rt.block_on(shared.client.fetch_bytes(&c.url, MAX_BYTES)).map_err(|e| crate::open::describe(&e))?;
                        (b.to_vec(), c.name)
                    }
                };
                let ext = std::path::Path::new(&name).extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
                let (bytes, wav) = prepare(bytes)?;
                let ext = wav.map(str::to_owned).or(ext);
                // one pass for the loudness and the waveform, a second decoder plays
                let mut scan = Decoder::open_bytes(bytes.clone(), ext.as_deref()).map_err(|e| e.to_string())?;
                let (lufs, bars) = nativefs_audio::analysis::voice_scan(&mut scan, BARS);
                let dec = Decoder::open_bytes(bytes, ext.as_deref()).map_err(|e| e.to_string())?;
                Ok(Loaded { player: Player::new(dec, settings, events), bars, gain_db: gain_db_for(lufs) })
            })();
            let _ = weak.upgrade_in_event_loop(move |w| {
                if let Some(s) = find(&w) {
                    loaded(&s, result);
                }
            });
        })
        .ok();
}

fn loaded(state: &Rc<RefCell<State>>, result: Result<Loaded, String>) {
    let w = state.borrow().window.clone_strong();
    let l = match result {
        Ok(l) => l,
        Err(e) => {
            w.set_status(format!("Cannot play this voice note: {e}").into());
            w.set_status_error(true);
            return;
        }
    };
    let rate = l.player.info().rate as f64;
    let total = l.player.duration_frames().unwrap_or(0) as f64 / rate;
    w.set_bars(ModelRc::new(VecModel::from(l.bars)));
    w.set_total(fmt_time(total).into());
    w.set_status("".into());
    w.set_ready(true);
    {
        let mut sb = state.borrow_mut();
        sb.player = Some(l.player);
        sb.rate = rate;
        sb.total = total;
        sb.gain_db = l.gain_db;
        // debug builds: `NATIVEFS_AUDIO_MUTE=1` plays muted, `NATIVEFS_AUDIO_NOPLAY=1` does not start
        if cfg!(debug_assertions) && std::env::var_os("NATIVEFS_AUDIO_MUTE").is_some() {
            sb.muted = true;
            w.set_muted(true);
        }
    }
    apply(state);
    let noplay = cfg!(debug_assertions) && std::env::var_os("NATIVEFS_AUDIO_NOPLAY").is_some();
    if !noplay {
        play_pause(state);
    }
}

fn play_pause(state: &Rc<RefCell<State>>) {
    let sb = state.borrow();
    let Some(p) = &sb.player else { return };
    if p.is_playing() {
        p.pause();
    } else {
        if sb.window.get_progress() >= 0.999 {
            p.seek(0);
        }
        // one voice note at a time
        let others: Vec<_> = VOICES.with(|l| l.borrow().iter().filter(|x| !Rc::ptr_eq(x, state)).cloned().collect());
        for o in others {
            if let Some(op) = &o.borrow().player {
                op.pause();
            }
        }
        p.play();
    }
}

fn seek_to(state: &Rc<RefCell<State>>, at: f64) {
    {
        let sb = state.borrow();
        let Some(p) = &sb.player else { return };
        p.seek((at * sb.total * sb.rate) as u64);
    }
    // paused: show the new position now (while playing the timer does it)
    tick(state, Some(at));
}

fn on_event(state: &Rc<RefCell<State>>, e: Event) {
    let w = state.borrow().window.clone_strong();
    match e {
        Event::Playing(on) => {
            w.set_playing(on);
            schedule(state);
            tick(state, None);
        }
        Event::Ended => {
            w.set_playing(false);
            tick(state, None);
            schedule(state);
        }
        Event::Error(m) => {
            w.set_status(m.into());
            w.set_status_error(true);
        }
        Event::Output(_) | Event::Released => {}
    }
}

/// The frame timer runs only while playing and visible.
fn schedule(state: &Rc<RefCell<State>>) {
    let run = {
        let sb = state.borrow();
        sb.visible && sb.player.as_ref().is_some_and(Player::is_playing)
    };
    if !run {
        state.borrow_mut().timer = None;
        return;
    }
    if state.borrow().timer.is_some() {
        return;
    }
    let sw = Rc::downgrade(state);
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(50), move || {
        if let Some(s) = sw.upgrade() {
            tick(&s, None);
        }
    });
    state.borrow_mut().timer = Some(timer);
}

fn tick(state: &Rc<RefCell<State>>, at: Option<f64>) {
    let sb = state.borrow();
    let Some(p) = &sb.player else { return };
    let total = sb.total;
    let pos = at.map_or_else(|| p.position() as f64 / sb.rate, |a| a * total);
    let progress = if total > 0.0 { (pos / total).clamp(0.0, 1.0) } else { 0.0 };
    sb.window.set_progress(progress as f32);
    sb.window.set_elapsed(fmt_time(pos).into());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_follows_the_target_and_clamps() {
        assert_eq!(gain_db_for(None), 0.0);
        assert!((gain_db_for(Some(-20.0)) - 4.0).abs() < 1e-9);
        assert_eq!(gain_db_for(Some(-50.0)), 18.0);
        assert_eq!(gain_db_for(Some(-2.0)), -12.0);
        assert!((gain_db_for(Some(-10.0)) + 6.0).abs() < 1e-9);
    }

    #[test]
    fn opus_notes_decode_to_wav() {
        let rec = crate::voice::Recording { samples: (0..48000).map(|i| ((i as f32 * 0.05).sin() * 8000.0) as i16).collect(), rate: 48000, channels: 1 };
        let ogg = crate::voice::encode_opus(&rec).unwrap();
        let (wav, ext) = prepare(ogg).unwrap();
        assert_eq!(ext, Some("wav"));
        let mut d = Decoder::open_bytes(wav, Some("wav")).unwrap();
        assert!(d.total_frames().unwrap() >= 47000);
        let (lufs, bars) = nativefs_audio::analysis::voice_scan(&mut d, 10);
        assert!(lufs.is_some() && bars.len() == 10);
        // not Opus: untouched
        assert_eq!(prepare(vec![1, 2, 3]).unwrap(), (vec![1, 2, 3], None));
    }

    /// A quiet note is boosted by the clamped gain and the limiter keeps a hot peak under full scale.
    #[test]
    fn boost_is_limited_below_full_scale() {
        let tone = |amp: f64| -> Vec<i32> {
            (0..96000).flat_map(|i| { let v = ((i as f64 * 0.05).sin() * amp * 2_147_483_647.0) as i32; [v, v] }).collect()
        };
        let rms = |v: &[i32]| (v.iter().map(|s| (*s as f64 / 2_147_483_648.0).powi(2)).sum::<f64>() / v.len() as f64).sqrt();
        let run = |input: &[i32], db: f64| {
            let mut c = nativefs_audio::dsp::Chain::new(48000);
            c.set(&Params { norm_db: db, ..Params::default() });
            let mut out = Vec::new();
            c.process(input, &mut out, 32);
            out
        };
        let quiet = tone(0.02);
        let out = run(&quiet, 18.0);
        let ratio = rms(&out[out.len() / 2..]) / rms(&quiet[quiet.len() / 2..]);
        assert!((ratio - 10f64.powf(18.0 / 20.0)).abs() < 0.2, "{ratio}");
        let hot = tone(0.5);
        let out = run(&hot, 18.0);
        let peak = out.iter().map(|v| (*v as f64).abs()).fold(0.0, f64::max) / 2_147_483_648.0;
        assert!(peak <= 1.0, "{peak}");
    }

    #[test]
    fn recorder_names_give_a_date() {
        assert!(date_from_name("voice-note-2026-11-14T13-05-22-123Z.opus").is_some());
        assert!(date_from_name("song.mp3").is_none());
    }
}
