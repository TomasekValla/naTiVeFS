//! The player engine: one decoder thread that owns the file and the DSP chain, feeds the device
//! thread through the ring and handles commands (play / pause / seek / loop / device / sound
//! controls). Pausing either releases the device (default: other apps get it back) or keeps it open
//! and plays silence. A seek while playing flushes the ring without reopening the device; the
//! decoder seeks sample-exactly. The ring is kept ~100 ms full, so a knob is heard right away.
//! Mono files play as stereo (the same samples on both sides: still bit-perfect, and width / 8D
//! need two channels).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::decode::{Decoder, SourceFormat};
use crate::dsp::{Chain, Params};
use crate::output::{fidelity, DeviceFormat, DeviceInfo, Fidelity, ShareMode};
use crate::ring::{ring, Producer};
#[cfg(not(any(windows, target_os = "linux")))]
use crate::Error;

#[cfg(any(windows, target_os = "linux"))]
use crate::output::sys::Stream;

/// What happens to the device while paused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseBehavior {
    /// Close the device on pause (exclusive mode would otherwise mute every other app).
    Release,
    /// Keep it open and play silence (instant resume).
    Keep,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSettings {
    /// `None` = the system default device.
    pub device: Option<String>,
    pub mode: ShareMode,
    pub pause: PauseBehavior,
}

impl Default for OutputSettings {
    fn default() -> Self {
        Self { device: None, mode: ShareMode::Exclusive, pause: PauseBehavior::Release }
    }
}

/// Where the sound goes right now (for the indicator).
#[derive(Debug, Clone, PartialEq)]
pub struct OutputStatus {
    pub device: DeviceInfo,
    pub format: DeviceFormat,
    pub mode: ShareMode,
    pub fidelity: Fidelity,
    /// Exclusive was asked for but failed; the reason (the stream then runs shared).
    pub fallback: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Device opened (or reopened after a resume / device change).
    Output(OutputStatus),
    /// Device released (paused with `PauseBehavior::Release`, or stopped).
    Released,
    Playing(bool),
    /// The track ended (not sent while looping).
    Ended,
    Error(String),
}

pub type EventSink = Box<dyn Fn(Event) + Send>;

enum Cmd {
    Play,
    Pause,
    Seek(u64),
    SetLoop(bool),
    SetOutput(OutputSettings),
    SetDsp(Box<Params>),
    Quit,
}

/// Output frame `out` (counted since the last seek) plays source frame `src`; later output frames
/// advance at `tempo` source frames each.
#[derive(Debug, Clone, Copy)]
struct Marker {
    out: u64,
    src: u64,
    tempo: f64,
}

/// State read by the UI without asking the decoder thread.
struct Shared {
    /// Output frames taken by the device since the last seek / open.
    consumed: AtomicU64,
    /// Maps output frames back to the track (tempo changes the ratio).
    markers: Mutex<VecDeque<Marker>>,
    playing: AtomicBool,
    /// The decoder reached the end of the file and everything left is queued.
    eof: AtomicBool,
    underruns: AtomicU64,
    /// Flush handshake: the decoder bumps `epoch`, the device thread clears the ring and stores it in
    /// `acked`.
    epoch: AtomicU64,
    acked: AtomicU64,
    paused_output: AtomicBool,
    /// Applied on the device thread so it acts at once.
    muted: AtomicBool,
    /// The DSP chain is changing the samples (not bypassed).
    dsp_active: AtomicBool,
    /// The last samples handed to the device (mono), for the spectrum and the visualizer. The
    /// device thread only `try_lock`s it, so the audio never waits for the UI.
    scope: Mutex<Scope>,
}

/// Ring of recent mono samples.
pub struct Scope {
    buf: Vec<f32>,
    pos: usize,
}

const SCOPE_LEN: usize = 16384;

impl Scope {
    fn push(&mut self, interleaved: &[i32], ch: usize) {
        for fr in interleaved.chunks_exact(ch) {
            let m = fr.iter().map(|s| *s as f32).sum::<f32>() / (ch as f32 * 2_147_483_648.0);
            self.buf[self.pos] = m;
            self.pos = (self.pos + 1) % SCOPE_LEN;
        }
    }

    /// The newest `n` samples, oldest first.
    fn latest(&self, n: usize) -> Vec<f32> {
        let n = n.min(SCOPE_LEN);
        (0..n).map(|i| self.buf[(self.pos + SCOPE_LEN - n + i) % SCOPE_LEN]).collect()
    }
}

impl Shared {
    fn position(&self) -> u64 {
        let c = self.consumed.load(Ordering::Acquire);
        let m = self.markers.lock().unwrap();
        let mk = m.iter().rev().find(|k| k.out <= c).or(m.front()).copied();
        mk.map_or(0, |k| k.src + (c.saturating_sub(k.out) as f64 * k.tempo) as u64)
    }
}

pub struct Player {
    tx: Sender<Cmd>,
    shared: Arc<Shared>,
    info: SourceFormat,
    total: Option<u64>,
    thread: Option<JoinHandle<()>>,
    status: Arc<Mutex<Option<OutputStatus>>>,
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Player {
    /// Takes an opened decoder; nothing plays until `play`.
    pub fn new(decoder: Decoder, settings: OutputSettings, events: EventSink) -> Player {
        let info = decoder.info().clone();
        let total = decoder.total_frames();
        let shared = Arc::new(Shared {
            consumed: AtomicU64::new(0),
            markers: Mutex::new(VecDeque::from([Marker { out: 0, src: 0, tempo: 1.0 }])),
            playing: AtomicBool::new(false),
            eof: AtomicBool::new(false),
            underruns: AtomicU64::new(0),
            epoch: AtomicU64::new(0),
            acked: AtomicU64::new(0),
            paused_output: AtomicBool::new(false),
            muted: AtomicBool::new(false),
            dsp_active: AtomicBool::new(false),
            scope: Mutex::new(Scope { buf: vec![0.0; SCOPE_LEN], pos: 0 }),
        });
        let status = Arc::new(Mutex::new(None));
        let (tx, rx) = mpsc::channel();
        let (sh, st) = (shared.clone(), status.clone());
        let thread = std::thread::Builder::new()
            .name("audio-decode".into())
            .spawn(move || Worker::new(decoder, settings, sh, st, events).run(rx))
            .ok();
        Player { tx, shared, info, total, thread, status }
    }

    pub fn info(&self) -> &SourceFormat {
        &self.info
    }

    pub fn duration_frames(&self) -> Option<u64> {
        self.total
    }

    /// Current position in source frames (what the device has been given, mapped back through the
    /// tempo; WASAPI exclusive adds only its few-millisecond buffer on top).
    pub fn position(&self) -> u64 {
        let p = self.shared.position();
        self.total.map_or(p, |t| p.min(t))
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Acquire)
    }

    pub fn underruns(&self) -> u64 {
        self.shared.underruns.load(Ordering::Relaxed)
    }

    /// The newest `n` mono samples that went to the device (for analysis; silence while paused).
    pub fn scope(&self, n: usize) -> Vec<f32> {
        self.shared.scope.lock().map(|s| s.latest(n)).unwrap_or_default()
    }

    /// The sound controls are changing the samples right now.
    pub fn dsp_active(&self) -> bool {
        self.shared.dsp_active.load(Ordering::Acquire)
    }

    pub fn output(&self) -> Option<OutputStatus> {
        self.status.lock().ok().and_then(|s| s.clone())
    }

    pub fn play(&self) {
        let _ = self.tx.send(Cmd::Play);
    }

    pub fn pause(&self) {
        let _ = self.tx.send(Cmd::Pause);
    }

    pub fn seek(&self, frame: u64) {
        let _ = self.tx.send(Cmd::Seek(frame));
    }

    pub fn set_loop(&self, on: bool) {
        let _ = self.tx.send(Cmd::SetLoop(on));
    }

    pub fn set_muted(&self, on: bool) {
        self.shared.muted.store(on, Ordering::Release);
    }

    /// Sound controls (neutral `Params` = bypass, bit-identical samples).
    pub fn set_dsp(&self, params: Params) {
        let _ = self.tx.send(Cmd::SetDsp(Box::new(params)));
    }

    pub fn set_output(&self, settings: OutputSettings) {
        let _ = self.tx.send(Cmd::SetOutput(settings));
    }
}

struct Worker {
    dec: Decoder,
    /// Source format as played: mono becomes stereo.
    info: SourceFormat,
    mono: bool,
    chain: Option<Chain>,
    settings: OutputSettings,
    shared: Arc<Shared>,
    status: Arc<Mutex<Option<OutputStatus>>>,
    events: EventSink,
    #[cfg(any(windows, target_os = "linux"))]
    stream: Option<Stream>,
    prod: Option<Producer>,
    /// Processed samples not yet in the ring.
    pending: Vec<i32>,
    scratch: Vec<i32>,
    processed: Vec<i32>,
    /// Output frames produced since the last seek.
    out_written: u64,
    /// Bit depth of the device (dither target while the DSP runs).
    dither_bits: u32,
    looping: bool,
    playing: bool,
    ended: bool,
}

/// Ring capacity: one second of audio.
fn ring_samples(info: &SourceFormat) -> usize {
    info.rate as usize * info.channels as usize
}

/// How far ahead the ring is filled: short (100 ms), so the sound controls react at once.
fn fill_target(info: &SourceFormat) -> usize {
    ring_samples(info) / 10
}

/// What the device thread runs: flush on a seek, silence while paused (keep mode), mute.
fn make_fill(sh: Arc<Shared>, mut cons: crate::ring::Consumer, ch: usize) -> crate::output::Fill {
    let mut seen = sh.epoch.load(Ordering::Acquire);
    Box::new(move |out: &mut [i32]| {
        let e = sh.epoch.load(Ordering::Acquire);
        if e != seen {
            // a seek: drop the old audio; this one period stays silent on purpose
            cons.clear();
            seen = e;
            sh.acked.store(e, Ordering::Release);
            return 0;
        }
        if sh.paused_output.load(Ordering::Acquire) {
            return 0;
        }
        let n = cons.pop(out);
        sh.consumed.fetch_add((n / ch) as u64, Ordering::AcqRel);
        if n < out.len() && !sh.eof.load(Ordering::Acquire) {
            sh.underruns.fetch_add(1, Ordering::Relaxed);
        }
        if sh.muted.load(Ordering::Acquire) {
            out[..n].fill(0);
        }
        if let Ok(mut sc) = sh.scope.try_lock() {
            sc.push(&out[..n], ch);
        }
        n
    })
}

impl Worker {
    fn new(dec: Decoder, settings: OutputSettings, shared: Arc<Shared>, status: Arc<Mutex<Option<OutputStatus>>>, events: EventSink) -> Self {
        let mut info = dec.info().clone();
        let mono = info.channels == 1;
        if mono {
            info.channels = 2;
        }
        // the chain is stereo; multichannel files play untouched
        let chain = (info.channels == 2).then(|| Chain::new(info.rate));
        Worker {
            dec,
            info,
            mono,
            chain,
            settings,
            shared,
            status,
            events,
            #[cfg(any(windows, target_os = "linux"))]
            stream: None,
            prod: None,
            pending: Vec::new(),
            scratch: Vec::new(),
            processed: Vec::new(),
            out_written: 0,
            dither_bits: 32,
            looping: false,
            playing: false,
            ended: false,
        }
    }

    fn run(mut self, rx: Receiver<Cmd>) {
        loop {
            // playing: top up the ring every few ms; paused: just wait for commands
            let wait = if self.playing { Duration::from_millis(5) } else { Duration::from_secs(3600) };
            match rx.recv_timeout(wait) {
                Ok(Cmd::Quit) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(cmd) => self.command(cmd),
                Err(RecvTimeoutError::Timeout) => {}
            }
            if self.playing {
                self.pump();
                self.check_end();
            }
        }
        self.close();
    }

    fn command(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Play => self.play(),
            Cmd::Pause => self.pause(),
            Cmd::Seek(frame) => self.seek(frame),
            Cmd::SetLoop(on) => self.looping = on,
            Cmd::SetOutput(s) => {
                self.settings = s;
                if self.stream_open() {
                    // reopen at the current position on the new device / mode
                    let pos = self.shared.position();
                    self.close();
                    self.restart_at(pos);
                    if self.playing {
                        self.open();
                    }
                }
            }
            Cmd::SetDsp(p) => {
                // debug builds: `NATIVEFS_AUDIO_LOG=1` logs what the sound controls send
                if cfg!(debug_assertions) && std::env::var_os("NATIVEFS_AUDIO_LOG").is_some() {
                    eprintln!(
                        "[audio] dsp: volume {:.1} filter {:.1} tempo {:.3} pitch {:.2} nc {:.0} reverb {:.0} echo {:.0} fb {:.0} width {:.0} 8d {:.0} norm {:.1} dB neutral {} chain {}",
                        p.volume, p.filter, p.tempo, p.pitch, p.nightcore, p.reverb, p.echo, p.feedback, p.width, p.rotate, p.norm_db, p.neutral(), self.chain.is_some()
                    );
                }
                if let Some(c) = &mut self.chain {
                    c.set(&p);
                    self.shared.dsp_active.store(!c.bypassed(), Ordering::Release);
                }
            }
            Cmd::Quit => {}
        }
    }

    #[cfg(any(windows, target_os = "linux"))]
    fn stream_open(&self) -> bool {
        self.stream.is_some()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    fn stream_open(&self) -> bool {
        false
    }

    fn set_playing(&mut self, on: bool) {
        if self.playing != on {
            self.playing = on;
            self.shared.playing.store(on, Ordering::Release);
            (self.events)(Event::Playing(on));
        }
    }

    fn play(&mut self) {
        if self.ended {
            // play after the end starts over, like the web player
            self.restart_at(0);
        }
        self.shared.paused_output.store(false, Ordering::Release);
        if !self.stream_open() && !self.open() {
            return;
        }
        self.set_playing(true);
    }

    fn pause(&mut self) {
        if !self.playing {
            return;
        }
        self.set_playing(false);
        match self.settings.pause {
            PauseBehavior::Keep if self.stream_open() => self.shared.paused_output.store(true, Ordering::Release),
            _ => {
                // the ring is lost with the stream: continue later exactly where the device stopped
                let pos = self.shared.position();
                self.close();
                self.restart_at(pos);
            }
        }
    }

    fn seek(&mut self, frame: u64) {
        let frame = self.dec.total_frames().map_or(frame, |t| frame.min(t));
        self.restart_at(frame);
        if self.stream_open() {
            // decode the new spot first, so the ring is refilled right after the flush
            let ahead = fill_target(&self.info);
            while self.pending.len() < ahead && !self.shared.eof.load(Ordering::Acquire) {
                self.decode_more();
            }
            // flush handshake: the device thread empties the ring on its next wake-up
            let e = self.shared.epoch.fetch_add(1, Ordering::AcqRel) + 1;
            let t0 = std::time::Instant::now();
            while self.shared.acked.load(Ordering::Acquire) != e && t0.elapsed() < Duration::from_millis(500) {
                std::thread::sleep(Duration::from_millis(1));
            }
            // old samples played meanwhile do not count for the new position
            self.shared.consumed.store(0, Ordering::Release);
            if self.playing || self.settings.pause == PauseBehavior::Keep {
                self.pump();
            }
        }
    }

    /// Decoder to `frame`, nothing queued; the position reads `frame` from now on.
    fn restart_at(&mut self, frame: u64) {
        self.pending.clear();
        if let Err(e) = self.dec.seek(frame) {
            (self.events)(Event::Error(e.to_string()));
        }
        if let Some(c) = &mut self.chain {
            // reverb / echo tails and the time-stretch history belong to the old spot
            c.reset();
        }
        self.out_written = 0;
        *self.shared.markers.lock().unwrap() = VecDeque::from([Marker { out: 0, src: frame, tempo: 1.0 }]);
        self.shared.consumed.store(0, Ordering::Release);
        self.shared.eof.store(false, Ordering::Release);
        self.ended = false;
    }

    /// Decode until the ring holds `fill_target` (or the file ends).
    fn pump(&mut self) {
        let target = fill_target(&self.info);
        let cap = ring_samples(&self.info).next_power_of_two();
        if self.prod.is_none() {
            // the ring is created with the stream; before that decode ahead into `pending`
            while self.pending.len() < target && !self.shared.eof.load(Ordering::Acquire) {
                self.decode_more();
            }
            return;
        }
        loop {
            let queued = cap.saturating_sub(self.prod.as_ref().map_or(cap, |p| p.free()));
            if queued >= target {
                break;
            }
            if !self.pending.is_empty() {
                let want = (target - queued).min(self.pending.len());
                let n = self.prod.as_mut().map_or(0, |p| p.push(&self.pending[..want]));
                self.pending.drain(..n);
                if n < want {
                    break;
                }
                continue;
            }
            if self.shared.eof.load(Ordering::Acquire) {
                break;
            }
            self.decode_more();
        }
        // forget markers the device has passed (keep the one it is in)
        let c = self.shared.consumed.load(Ordering::Acquire);
        let mut m = self.shared.markers.lock().unwrap();
        while m.len() > 1 && m[1].out <= c {
            m.pop_front();
        }
    }

    fn decode_more(&mut self) {
        let src = self.dec.position();
        self.scratch.clear();
        match self.dec.next(&mut self.scratch) {
            Ok(true) => {
                if self.mono {
                    let m = std::mem::take(&mut self.scratch);
                    self.scratch.extend(m.iter().flat_map(|s| [*s, *s]));
                }
                self.processed.clear();
                let tempo = match &mut self.chain {
                    Some(c) => {
                        c.process(&self.scratch, &mut self.processed, self.dither_bits);
                        self.shared.dsp_active.store(!c.bypassed(), Ordering::Release);
                        c.tempo()
                    }
                    None => {
                        self.processed.extend_from_slice(&self.scratch);
                        1.0
                    }
                };
                let frames = (self.processed.len() / self.info.channels as usize) as u64;
                self.shared.markers.lock().unwrap().push_back(Marker { out: self.out_written, src, tempo });
                self.out_written += frames;
                self.pending.extend_from_slice(&self.processed);
            }
            Ok(false) if self.looping => {
                // seamless loop: the next samples come from the start (their marker says src 0)
                if let Err(e) = self.dec.seek(0) {
                    (self.events)(Event::Error(e.to_string()));
                    self.shared.eof.store(true, Ordering::Release);
                }
            }
            Ok(false) => self.shared.eof.store(true, Ordering::Release),
            Err(e) => {
                (self.events)(Event::Error(e.to_string()));
                self.shared.eof.store(true, Ordering::Release);
            }
        }
    }

    fn check_end(&mut self) {
        // everything decoded and handed to the device
        if self.shared.eof.load(Ordering::Acquire) && self.pending.is_empty() && self.shared.consumed.load(Ordering::Acquire) >= self.out_written {
            let total = self.dec.position();
            self.close();
            self.set_playing(false);
            self.ended = true;
            *self.shared.markers.lock().unwrap() = VecDeque::from([Marker { out: 0, src: total, tempo: 1.0 }]);
            self.shared.consumed.store(0, Ordering::Release);
            (self.events)(Event::Ended);
        }
    }

    #[cfg(any(windows, target_os = "linux"))]
    fn open(&mut self) -> bool {
        let info = self.info.clone();
        self.pump();
        let ch = info.channels as usize;
        let id = self.settings.device.clone();
        let (mut prod, cons) = ring(ring_samples(&info));
        // what was decoded ahead goes in first
        let n = prod.push(&self.pending);
        self.pending.drain(..n);
        let result = match Stream::open(id.as_deref(), &info, self.settings.mode, make_fill(self.shared.clone(), cons, ch)) {
            Ok(s) => Ok((s, None)),
            Err(e) if self.settings.mode == ShareMode::Exclusive => {
                // e.g. the device cannot run at the file's rate: let the system mixer resample instead of silence
                let reason = e.to_string();
                // the ring went with the failed closure: requeue from the current position
                let pos = self.shared.position();
                self.restart_at(pos);
                self.pump();
                let (mut p2, c2) = ring(ring_samples(&info));
                let n = p2.push(&self.pending);
                self.pending.drain(..n);
                prod = p2;
                Stream::open(id.as_deref(), &info, ShareMode::Shared, make_fill(self.shared.clone(), c2, ch)).map(|s| (s, Some(reason)))
            }
            Err(e) => Err(e),
        };
        match result {
            Ok((stream, fallback)) => {
                self.dither_bits = if stream.format.float { 32 } else { stream.format.valid_bits as u32 };
                let status = OutputStatus {
                    fidelity: fidelity(&info, &stream.format, stream.mode, &stream.device),
                    device: stream.device.clone(),
                    format: stream.format,
                    mode: stream.mode,
                    fallback,
                };
                if let Ok(mut s) = self.status.lock() {
                    *s = Some(status.clone());
                }
                self.stream = Some(stream);
                self.prod = Some(prod);
                (self.events)(Event::Output(status));
                true
            }
            Err(e) => {
                (self.events)(Event::Error(e.to_string()));
                false
            }
        }
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    fn open(&mut self) -> bool {
        (self.events)(Event::Error(Error::Device("audio output is not implemented on this system yet".into()).to_string()));
        false
    }

    fn close(&mut self) {
        #[cfg(any(windows, target_os = "linux"))]
        if self.stream.take().is_some() {
            (self.events)(Event::Released);
        }
        self.prod = None;
        self.shared.paused_output.store(false, Ordering::Release);
        // the stream (and its seek handshake) is gone: nothing can be waiting for an ack
        let e = self.shared.epoch.load(Ordering::Acquire);
        self.shared.acked.store(e, Ordering::Release);
    }
}
