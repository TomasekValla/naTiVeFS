//! Live state of one upload run, shared between the engine and the UI: byte
//! counters (atomics, updated as the request bodies stream from disk), speed /
//! ETA / average like the web's progress box, pause and cancel.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::plan::{stats_interval, ChunkPlan};
use super::stats::RunSamples;

const MIB: f64 = 1024.0 * 1024.0;

/// Pause / cancel handle of a run (cheap to clone).
#[derive(Clone)]
pub struct Control {
    paused: Arc<watch::Sender<bool>>,
    cancel: CancellationToken,
}

impl Default for Control {
    fn default() -> Self {
        Self { paused: Arc::new(watch::channel(false).0), cancel: CancellationToken::new() }
    }
}

impl Control {
    /// Pause never aborts a request that is already sending: it only stops new
    /// chunks/files from starting, so the upload halts at an exact chunk boundary.
    pub fn set_paused(&self, paused: bool) {
        self.paused.send_replace(paused);
    }

    pub fn is_paused(&self) -> bool {
        *self.paused.borrow()
    }

    /// Kill: abort everything now (chunks already on the server are deleted).
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub(crate) fn token(&self) -> &CancellationToken {
        &self.cancel
    }

    pub(crate) fn paused_rx(&self) -> watch::Receiver<bool> {
        self.paused.subscribe()
    }
}

/// State of one file in the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    Waiting = 0,
    Active = 1,
    Done = 2,
    Failed = 3,
}

impl Phase {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Phase::Active,
            2 => Phase::Done,
            3 => Phase::Failed,
            _ => Phase::Waiting,
        }
    }
}

pub struct FileProgress {
    pub name: String,
    pub size: u64,
    /// Confirmed by the server (including chunks skipped on resume).
    pub(crate) done: AtomicU64,
    /// Streamed in requests that have not been answered yet.
    pub(crate) inflight: AtomicU64,
    phase: AtomicU8,
}

impl FileProgress {
    pub fn phase(&self) -> Phase {
        Phase::from_u8(self.phase.load(Ordering::Relaxed))
    }

    pub(crate) fn set_phase(&self, p: Phase) {
        self.phase.store(p as u8, Ordering::Relaxed);
    }

    pub fn bytes(&self) -> u64 {
        (self.done.load(Ordering::Relaxed) + self.inflight.load(Ordering::Relaxed)).min(self.size)
    }
}

/// What the progress header says (besides "Uploading...").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Uploading,
    /// "Resuming — N/M chunks already uploaded" (M unknown for auto-sized uploads).
    Resuming { chunks: u32, total: Option<u32> },
    /// Network lost: waiting for the connection.
    Reconnecting,
    /// 429/5xx: "Server busy, slowing down".
    Busy,
    /// Assembling on the server.
    Finishing,
}

#[derive(Debug)]
struct Speed {
    base_t: Instant,
    base_bytes: u64,
    avg_base: Option<(f64, u64)>,
    last_mbs: f64,
    eta_s: Option<f64>,
    avg_mbs: Option<f64>,
    have_speed: bool,
    /// Bytes at the previous sample (`None` = never sampled / baseline just reset).
    last_net: Option<u64>,
}

/// Everything the UI shows about a running upload.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub done: u64,
    pub total: u64,
    /// (phase, bytes, size) per file.
    pub files: Vec<(Phase, u64, u64)>,
    /// Current speed (MB/s) once there is a meaningful sample, else "Calculating…".
    pub speed_mbs: Option<f64>,
    pub avg_mbs: Option<f64>,
    pub eta_s: Option<f64>,
    pub elapsed_s: f64,
    pub status: RunStatus,
    /// The progress bar flashes the "resumed" colour for 2 s after a reconnect/resume.
    pub resumed_flash: bool,
    pub paused: bool,
    /// Requests still sending (pause shows "Finishing current chunk…" until 0).
    pub requests: u32,
    pub cancelled: bool,
    /// Current chunk plan (auto: live), for the settings card.
    pub plan: Option<ChunkPlan>,
    /// Index of the newest file that has started (for "Uploading file i/n").
    pub current_file: usize,
}

pub struct Run {
    pub files: Vec<FileProgress>,
    pub total_bytes: u64,
    pub control: Control,
    started: Instant,
    pub(crate) confirmed: AtomicU64,
    pub(crate) inflight: AtomicU64,
    pub(crate) requests: AtomicU32,
    speed: Mutex<Speed>,
    pub(crate) samples: Mutex<RunSamples>,
    pub(crate) plan: Mutex<Option<ChunkPlan>>,
    status: Mutex<(RunStatus, Option<Instant>)>,
    pub(crate) last_ping: Mutex<Option<f64>>,
    current_file: AtomicU32,
    /// Upload ids of this run (hide them from the "OTHER DEVICE" cards, also for a while after the run).
    pub(crate) upload_ids: Mutex<HashSet<String>>,
    /// Resume keys being uploaded right now (hide them from the NOT FINISHED cards).
    pub(crate) active_keys: Mutex<HashSet<String>>,
}

impl Run {
    /// `files` = (name, size) in queue order.
    pub fn new(files: impl IntoIterator<Item = (String, u64)>, control: Control) -> Arc<Self> {
        let files: Vec<FileProgress> = files
            .into_iter()
            .map(|(name, size)| FileProgress {
                name,
                size,
                done: AtomicU64::new(0),
                inflight: AtomicU64::new(0),
                phase: AtomicU8::new(Phase::Waiting as u8),
            })
            .collect();
        let total_bytes = files.iter().map(|f| f.size).sum();
        let now = Instant::now();
        Arc::new(Self {
            files,
            total_bytes,
            control,
            started: now,
            confirmed: AtomicU64::new(0),
            inflight: AtomicU64::new(0),
            requests: AtomicU32::new(0),
            speed: Mutex::new(Speed {
                base_t: now,
                base_bytes: 0,
                avg_base: None,
                last_mbs: 0.0,
                eta_s: None,
                avg_mbs: None,
                have_speed: false,
                last_net: None,
            }),
            samples: Mutex::new(RunSamples::default()),
            plan: Mutex::new(None),
            status: Mutex::new((RunStatus::Uploading, None)),
            last_ping: Mutex::new(None),
            current_file: AtomicU32::new(0),
            upload_ids: Mutex::new(HashSet::new()),
            active_keys: Mutex::new(HashSet::new()),
        })
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn is_active_upload(&self, upload_id: &str) -> bool {
        self.upload_ids.lock().unwrap().contains(upload_id)
    }

    /// Every upload id this run created, finished or not.
    pub fn upload_ids(&self) -> Vec<String> {
        self.upload_ids.lock().unwrap().iter().cloned().collect()
    }

    pub fn is_active_key(&self, key: &str) -> bool {
        self.active_keys.lock().unwrap().contains(key)
    }

    pub fn last_ping(&self) -> Option<f64> {
        *self.last_ping.lock().unwrap()
    }

    /// Live speed in MB/s (`lastKnownSpeedMB`), 0 before the first sample.
    pub fn last_speed_mbs(&self) -> f64 {
        self.speed.lock().unwrap().last_mbs
    }

    pub(crate) fn set_status(&self, s: RunStatus) {
        let mut st = self.status.lock().unwrap();
        st.0 = s;
    }

    /// Back to normal after a reconnect / resume, with the 2 s "resumed" flash.
    pub(crate) fn mark_resumed(&self) {
        let mut st = self.status.lock().unwrap();
        if matches!(st.0, RunStatus::Reconnecting | RunStatus::Busy) {
            st.0 = RunStatus::Uploading;
        }
        st.1 = Some(Instant::now());
    }

    /// The first real chunk of a resumed upload clears "Resuming — ...".
    pub(crate) fn clear_resuming(&self) {
        let mut st = self.status.lock().unwrap();
        if matches!(st.0, RunStatus::Resuming { .. }) {
            st.0 = RunStatus::Uploading;
        }
    }

    pub(crate) fn file_started(&self, idx: usize) {
        self.files[idx].set_phase(Phase::Active);
        self.current_file.fetch_max(idx as u32, Ordering::Relaxed);
    }

    /// Speed measurement restarts after a connection gap so MB/s is not misleadingly low.
    pub(crate) fn reset_speed_baseline(&self) {
        let net = self.confirmed.load(Ordering::Relaxed) + self.inflight.load(Ordering::Relaxed);
        let mut s = self.speed.lock().unwrap();
        s.base_t = Instant::now();
        s.base_bytes = net;
        s.avg_base = None;
        s.last_net = None;
    }

    pub(crate) fn add_inflight(&self, idx: usize, n: u64) {
        self.files[idx].inflight.fetch_add(n, Ordering::Relaxed);
        self.inflight.fetch_add(n, Ordering::Relaxed);
    }

    pub(crate) fn sub_inflight(&self, idx: usize, n: u64) {
        let _ = self.files[idx].inflight.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| Some(v.saturating_sub(n)));
        let _ = self.inflight.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| Some(v.saturating_sub(n)));
    }

    /// Bytes the server confirmed in this run.
    pub(crate) fn add_confirmed(&self, idx: usize, n: u64) {
        self.files[idx].done.fetch_add(n, Ordering::Relaxed);
        self.confirmed.fetch_add(n, Ordering::Relaxed);
    }

    /// Bytes skipped because the server already had them (resume).
    pub(crate) fn add_skipped(&self, idx: usize, n: u64) {
        self.files[idx].done.fetch_add(n, Ordering::Relaxed);
    }

    /// One stats sample (the web does this at every throttled progress render):
    /// speed of this run's own transfer, ETA, average from 2 s in. Returns the
    /// time until the next sample.
    pub(crate) fn sample(&self) -> Duration {
        let net = self.confirmed.load(Ordering::Relaxed) + self.inflight.load(Ordering::Relaxed);
        let done: u64 = self.files.iter().map(FileProgress::bytes).sum();
        let mut s = self.speed.lock().unwrap();
        // the web only updates on progress events: while nothing moves (connection
        // gap, server-side assembly) the numbers stay as they were
        if s.last_net == Some(net) {
            return stats_interval(s.last_mbs);
        }
        s.last_net = Some(net);
        let eff = s.base_t.elapsed().as_secs_f64();
        let transferred = net.saturating_sub(s.base_bytes);
        if eff > 0.5 && transferred > 256 * 1024 {
            let speed_b = transferred as f64 / eff;
            let mbs = speed_b / MIB;
            s.last_mbs = mbs;
            s.have_speed = true;
            self.samples.lock().unwrap().speed.push(mbs);
            let remaining = self.total_bytes.saturating_sub(done) as f64 / speed_b;
            s.eta_s = (remaining.is_finite() && remaining > 0.0).then_some(remaining);
            if s.avg_base.is_none() && eff >= 2.0 {
                s.avg_base = Some((eff, transferred));
            }
            s.avg_mbs = match s.avg_base {
                Some((bt, bb)) if eff - bt > 0.0 => Some((transferred.saturating_sub(bb) as f64 / (eff - bt)) / MIB),
                _ => None,
            };
        } else {
            s.have_speed = false;
            s.eta_s = None;
        }
        stats_interval(s.last_mbs)
    }

    pub fn snapshot(&self) -> Snapshot {
        let files: Vec<(Phase, u64, u64)> = self.files.iter().map(|f| (f.phase(), f.bytes(), f.size)).collect();
        let done = files.iter().map(|f| f.1).sum();
        let s = self.speed.lock().unwrap();
        let (status, resumed_at) = *self.status.lock().unwrap();
        Snapshot {
            done,
            total: self.total_bytes,
            files,
            speed_mbs: s.have_speed.then_some(s.last_mbs),
            avg_mbs: s.avg_mbs,
            eta_s: s.eta_s,
            elapsed_s: self.started.elapsed().as_secs_f64(),
            status,
            resumed_flash: resumed_at.is_some_and(|t| t.elapsed() < Duration::from_secs(2)),
            paused: self.control.is_paused(),
            requests: self.requests.load(Ordering::Relaxed),
            cancelled: self.control.is_cancelled(),
            plan: *self.plan.lock().unwrap(),
            current_file: self.current_file.load(Ordering::Relaxed) as usize,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_needs_time_and_data_and_resets() {
        let run = Run::new([("a".into(), 10 * 1024 * 1024)], Control::default());
        run.add_inflight(0, 1024);
        run.sample();
        assert_eq!(run.snapshot().speed_mbs, None, "too early / too little data");
        {
            let mut s = run.speed.lock().unwrap();
            s.base_t = Instant::now() - Duration::from_secs(4);
        }
        run.add_confirmed(0, 4 * 1024 * 1024);
        run.sub_inflight(0, 1024);
        run.sample();
        let snap = run.snapshot();
        let mbs = snap.speed_mbs.unwrap();
        assert!((mbs - 1.0).abs() < 0.05, "{mbs}");
        assert!(snap.eta_s.unwrap() > 5.0);
        assert_eq!(snap.done, 4 * 1024 * 1024);
        // nothing moved: the numbers stay
        run.sample();
        assert_eq!(run.snapshot().speed_mbs.unwrap(), mbs);
        assert_eq!(run.samples.lock().unwrap().speed.len(), 1);
        run.reset_speed_baseline();
        run.sample();
        assert_eq!(run.snapshot().speed_mbs, None);
        // samples feed the auto planner and the stats card
        assert_eq!(run.samples.lock().unwrap().speed.len(), 1);
    }

    #[test]
    fn status_and_phases() {
        let run = Run::new([("a".into(), 1), ("b".into(), 2)], Control::default());
        run.file_started(1);
        assert_eq!(run.snapshot().current_file, 1);
        assert_eq!(run.files[1].phase(), Phase::Active);
        run.set_status(RunStatus::Reconnecting);
        run.mark_resumed();
        let s = run.snapshot();
        assert_eq!(s.status, RunStatus::Uploading);
        assert!(s.resumed_flash);
        run.control.set_paused(true);
        assert!(run.snapshot().paused);
    }
}
