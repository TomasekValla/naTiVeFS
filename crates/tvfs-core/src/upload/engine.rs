//! The upload engine (web: `uploadAll`, `uploadStandardFile`, `uploadChunkedFile`,
//! `uploadProtectedShare*`), rebuilt around one shared pool of connections.
//!
//! - Files <= 25 MB go in one request (`/api/upload`), bigger ones in chunks
//!   (`/api/upload/init` -> `/chunk` -> `/complete`), auto-sized from the measured
//!   speed or fixed, exactly with the web's constants (`plan.rs`).
//! - Up to `file_concurrency` (2) files are in flight at once. The next file starts
//!   as soon as every running file has *claimed* all of its chunks, so connections
//!   freed by a file's last chunks (and its server-side assembly) go to the next
//!   one. The connection count of the whole run never exceeds the plan's target,
//!   so in-flight data stays within the infra limit the web warns about.
//! - Bodies are streamed from disk in 256 KiB pieces: memory stays flat.
//! - Transient problems back off for everybody (`backoff.rs`); real failures retry
//!   12 times; resume state is persisted on every claim and every confirmed chunk.

use std::collections::{BTreeSet, VecDeque};
use std::io::SeekFrom;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use futures_util::stream::{FuturesUnordered, StreamExt};
use reqwest::multipart::{Form, Part};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::Notify;
use tokio::task::JoinSet;

use super::backoff::{is_transient_status, retry_delay, Backoff};
use super::file::UploadFile;
use super::plan::*;
use super::resume::{resume_key, ResumeRecord, ResumeStore};
use super::run::{Phase, Run, RunStatus};
use super::stats::{HistoryStore, StatsReport};
use crate::models::*;
use crate::Client;

const PIECE: u64 = 256 * 1024;
/// "TVFS Users Only": shares above this total go chunked, in pieces of this size.
const PROTECTED_CHUNK: u64 = 25 * 1024 * 1024;

/// What to upload and how.
#[derive(Debug, Clone)]
pub struct UploadRequest {
    pub files: Vec<UploadFile>,
    pub expiration_minutes: u32,
    /// Tier 2 accounts get links without the `_<timestamp>` suffix.
    pub remove_timestamp: bool,
    pub chunking: ChunkMode,
    /// "TVFS Users Only" share instead of public links.
    pub protected: bool,
}

/// Progress notifications (besides the polled `Run::snapshot`).
#[derive(Debug, Clone)]
pub enum Event {
    FileStarted(usize),
    FileDone(usize, Box<UploadCompleteResponse>),
    FileFailed(usize, String),
}

#[derive(Debug, Clone)]
pub struct FileResult {
    pub name: String,
    pub result: Result<UploadCompleteResponse, String>,
}

#[derive(Debug, Clone)]
pub enum Outcome {
    Single(FileResult),
    Batch { files: Vec<FileResult>, batch: Result<BatchCompleteResponse, String> },
    Protected(Result<ProtectedShareResponse, String>),
    /// The run could not start (e.g. batch init refused).
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct RunReport {
    pub outcome: Outcome,
    /// The "Upload Performance" card, when at least one file made it.
    pub stats: Option<StatsReport>,
}

pub struct Uploader {
    client: Client,
    resume: Arc<ResumeStore>,
    history: Arc<HistoryStore>,
    max_auto_connections: u32,
    file_concurrency: usize,
    time_scale: f64,
}

#[derive(Debug)]
enum FileError {
    Killed,
    Failed(String),
}

impl FileError {
    fn message(&self) -> String {
        match self {
            FileError::Killed => "Upload killed".into(),
            FileError::Failed(m) => m.clone(),
        }
    }
}

#[derive(Debug)]
enum SendErr {
    Killed,
    Network(String),
    Timeout,
    Http { status: u16, message: String, retry_after: Option<Duration> },
    Auth,
    Io(String),
    Bad(String),
}

impl SendErr {
    fn is_network(&self) -> bool {
        matches!(self, SendErr::Network(_) | SendErr::Timeout)
    }

    fn is_transient(&self) -> bool {
        self.is_network() || matches!(self, SendErr::Http { status, .. } if is_transient_status(*status))
    }

    fn retry_after(&self) -> Option<Duration> {
        match self {
            SendErr::Http { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    fn describe(&self, what: &str) -> String {
        match self {
            SendErr::Killed => "Upload killed".into(),
            SendErr::Network(e) => format!("Network error on {what} ({e})"),
            SendErr::Timeout => format!("Timeout on {what}"),
            SendErr::Http { message, .. } => message.clone(),
            SendErr::Auth => "Not logged in (session expired or missing)".into(),
            SendErr::Io(e) => format!("Could not read the file: {e}"),
            SendErr::Bad(e) => e.clone(),
        }
    }
}

fn from_reqwest(e: reqwest::Error) -> SendErr {
    if e.is_timeout() {
        SendErr::Timeout
    } else {
        SendErr::Network(e.to_string())
    }
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Read a JSON response, mapping non-2xx to `SendErr::Http` with the server's `error` text.
async fn read_json<T: serde::de::DeserializeOwned>(resp: reqwest::Response, fallback: impl Fn(u16) -> String) -> Result<T, SendErr> {
    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return Err(SendErr::Auth);
    }
    let retry_after = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|s| Duration::from_secs(s.min(60)));
    let text = resp.text().await.map_err(from_reqwest)?;
    if !(200..300).contains(&status) {
        let message = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned))
            .unwrap_or_else(|| fallback(status));
        return Err(SendErr::Http { status, message, retry_after });
    }
    serde_json::from_str(&text).map_err(|_| SendErr::Bad("Bad server response".into()))
}

// ── shared run context ──────────────────────────────────────────────────────

struct Ctx {
    client: Client,
    run: Arc<Run>,
    resume: Arc<ResumeStore>,
    backoff: Backoff,
    mode: ChunkMode,
    max_conn: u32,
    /// Auto mode: the plan established at the start of the newest file.
    start_plan: Mutex<ChunkPlan>,
    /// This device's average speed (MB/s) before the run.
    history_avg: Option<f64>,
    expiration: u32,
    remove_timestamp: bool,
    batch_id: Option<String>,
    active: AtomicU32,
    slot_freed: Notify,
    /// A file has claimed all of its chunks (the next one may start).
    claimed: Notify,
    last_online: tokio::sync::Mutex<Option<Instant>>,
    /// Multiplies retry pauses and polling intervals (1.0; tests run faster).
    time_scale: f64,
}

impl Ctx {
    fn pace(&self, d: Duration) -> Duration {
        d.mul_f64(self.time_scale)
    }

    fn live_plan(&self) -> ChunkPlan {
        let mbs = self.run.last_speed_mbs();
        if mbs > 0.0 {
            compute_auto_plan(mbs * 8.0, PlanSource::Live, self.max_conn)
        } else {
            *self.start_plan.lock().unwrap()
        }
    }

    /// Connections the plan wants (fixed: the configured count; auto: live).
    fn target(&self) -> u32 {
        match self.mode {
            ChunkMode::Fixed { count, .. } => count.max(1),
            ChunkMode::Auto => self.live_plan().connections,
        }
    }

    fn effective_target(&self) -> u32 {
        self.target().min(self.backoff.cap().unwrap_or(u32::MAX)).max(1)
    }

    fn cancelled(&self) -> bool {
        self.run.control.is_cancelled()
    }
}

struct Slot(Arc<Ctx>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
        self.0.slot_freed.notify_waiters();
    }
}

/// Wait for a free connection (respects pause, back-pressure and live plan changes).
async fn acquire(ctx: &Arc<Ctx>) -> Option<Slot> {
    let mut paused = ctx.run.control.paused_rx();
    loop {
        let freed = ctx.slot_freed.notified();
        tokio::pin!(freed);
        freed.as_mut().enable();
        if ctx.cancelled() {
            return None;
        }
        if !*paused.borrow_and_update() {
            let target = ctx.effective_target();
            let got = ctx
                .active
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |a| (a < target).then_some(a + 1))
                .is_ok();
            if got {
                return Some(Slot(ctx.clone()));
            }
        }
        tokio::select! {
            _ = &mut freed => {}
            _ = paused.changed() => {}
            _ = ctx.run.control.token().cancelled() => return None,
            // the target moves with the live plan and the back-pressure cap
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
    }
}

async fn sleep_or_cancel(ctx: &Ctx, d: Duration) -> Result<(), SendErr> {
    tokio::select! {
        _ = tokio::time::sleep(d) => Ok(()),
        _ = ctx.run.control.token().cancelled() => Err(SendErr::Killed),
    }
}

async fn wait_while_paused(ctx: &Ctx) -> Result<(), SendErr> {
    let mut rx = ctx.run.control.paused_rx();
    loop {
        if ctx.cancelled() {
            return Err(SendErr::Killed);
        }
        if !*rx.borrow_and_update() {
            return Ok(());
        }
        tokio::select! {
            _ = rx.changed() => {}
            _ = ctx.run.control.token().cancelled() => return Err(SendErr::Killed),
        }
    }
}

/// `waitOutBackoff`: sit out the shared window, then de-synchronise the workers.
async fn wait_out_backoff(ctx: &Ctx) -> Result<(), SendErr> {
    let mut waited = false;
    loop {
        let left = ctx.backoff.remaining();
        if left.is_zero() {
            break;
        }
        waited = true;
        sleep_or_cancel(ctx, left.min(Duration::from_millis(250))).await?;
    }
    if waited {
        sleep_or_cancel(ctx, ctx.pace(Duration::from_millis(fastrand::u64(..400)))).await?;
    }
    Ok(())
}

/// `waitForConnection`: poll `/api/status` every 2 s until the server answers.
/// One worker polls; the others reuse its result.
async fn wait_for_connection(ctx: &Ctx) -> Result<(), SendErr> {
    let mut last = ctx.last_online.lock().await;
    if last.is_some_and(|t| t.elapsed() < ctx.pace(Duration::from_secs(2))) {
        return Ok(());
    }
    loop {
        let ok = tokio::select! {
            r = tokio::time::timeout(Duration::from_secs(10), ctx.client.status()) => matches!(r, Ok(Ok(_))),
            _ = ctx.run.control.token().cancelled() => return Err(SendErr::Killed),
        };
        if ok {
            *last = Some(Instant::now());
            return Ok(());
        }
        sleep_or_cancel(ctx, ctx.pace(Duration::from_secs(2))).await?;
    }
}

/// Bytes streamed by one request attempt, per file (a share request carries several files).
#[derive(Default)]
struct Attempt {
    total: AtomicU64,
    per_file: Mutex<Vec<(usize, u64)>>,
}

impl Attempt {
    fn add(&self, run: &Run, idx: usize, n: u64) {
        self.total.fetch_add(n, Ordering::Relaxed);
        let mut v = self.per_file.lock().unwrap();
        match v.iter_mut().find(|(i, _)| *i == idx) {
            Some((_, b)) => *b += n,
            None => v.push((idx, n)),
        }
        run.add_inflight(idx, n);
    }

    /// The attempt is over: its bytes are no longer "in flight".
    fn release(&self, run: &Run) {
        for (idx, n) in self.per_file.lock().unwrap().drain(..) {
            run.sub_inflight(idx, n);
        }
    }
}

/// A file range as a streamed request body; counts bytes as they go out.
struct BodyCounter {
    run: Arc<Run>,
    file_idx: usize,
    attempt: Arc<Attempt>,
    io_failed: Arc<AtomicBool>,
}

async fn range_body(path: &Path, start: u64, len: u64, c: BodyCounter) -> std::io::Result<reqwest::Body> {
    let mut f = tokio::fs::File::open(path).await?;
    if start > 0 {
        f.seek(SeekFrom::Start(start)).await?;
    }
    let stream = futures_util::stream::try_unfold((f, len, c), |(mut f, left, c)| async move {
        if left == 0 {
            return Ok(None);
        }
        let n = left.min(PIECE) as usize;
        let mut buf = vec![0u8; n];
        if let Err(e) = f.read_exact(&mut buf).await {
            c.io_failed.store(true, Ordering::Relaxed);
            return Err(e);
        }
        c.attempt.add(&c.run, c.file_idx, n as u64);
        Ok::<_, std::io::Error>(Some((Bytes::from(buf), (f, left - n as u64, c))))
    });
    Ok(reqwest::Body::wrap_stream(stream))
}

struct RequestGuard<'a>(&'a Run);

impl<'a> RequestGuard<'a> {
    fn new(run: &'a Run) -> Self {
        run.requests.fetch_add(1, Ordering::Relaxed);
        Self(run)
    }
}

impl Drop for RequestGuard<'_> {
    fn drop(&mut self) {
        self.0.requests.fetch_sub(1, Ordering::Relaxed);
    }
}

/// One file part of a multipart request: `len` bytes of `path` from `start`, streamed.
struct PartSpec<'a> {
    path: &'a Path,
    start: u64,
    len: u64,
    file_idx: usize,
    field: &'static str,
    file_name: String,
    mime: String,
}

/// A multipart request: text fields `before`, the streamed file `parts`, text fields `after`.
#[allow(clippy::too_many_arguments)]
async fn send_multipart<T: serde::de::DeserializeOwned>(
    ctx: &Ctx,
    url: &str,
    before: &[(&'static str, String)],
    parts: &[PartSpec<'_>],
    after: &[(&'static str, String)],
    timeout: Option<Duration>,
    attempt: &Arc<Attempt>,
    what: &str,
) -> Result<T, SendErr> {
    let io_failed = Arc::new(AtomicBool::new(false));
    let mut form = Form::new();
    for (k, v) in before {
        form = form.text(*k, v.clone());
    }
    for p in parts {
        let counter = BodyCounter { run: ctx.run.clone(), file_idx: p.file_idx, attempt: attempt.clone(), io_failed: io_failed.clone() };
        let body = range_body(p.path, p.start, p.len, counter).await.map_err(|e| SendErr::Io(e.to_string()))?;
        let part = Part::stream_with_length(body, p.len)
            .file_name(p.file_name.clone())
            .mime_str(&p.mime)
            .map_err(|e| SendErr::Bad(e.to_string()))?;
        form = form.part(p.field, part);
    }
    for (k, v) in after {
        form = form.text(*k, v.clone());
    }
    let url = ctx.client.endpoint(url).map_err(|e| SendErr::Bad(e.to_string()))?;
    let mut req = ctx.client.http().post(url).multipart(form);
    if let Some(t) = timeout {
        req = req.timeout(t);
    }
    let _guard = RequestGuard::new(&ctx.run);
    let fallback = |status: u16| format!("{what} failed ({status})");
    let result = tokio::select! {
        r = async { read_json::<T>(req.send().await.map_err(from_reqwest)?, fallback).await } => r,
        _ = ctx.run.control.token().cancelled() => Err(SendErr::Killed),
    };
    if io_failed.load(Ordering::Relaxed) && result.is_err() {
        return Err(SendErr::Io("the file changed or became unreadable during the upload".into()));
    }
    result
}

/// A JSON request (`body` = None: GET).
async fn send_json<T: serde::de::DeserializeOwned>(
    ctx: &Ctx,
    path: &str,
    body: Option<&serde_json::Value>,
    timeout: Duration,
    what: &str,
) -> Result<T, SendErr> {
    let url = ctx.client.endpoint(path).map_err(|e| SendErr::Bad(e.to_string()))?;
    let req = match body {
        Some(b) => ctx.client.http().post(url).json(b),
        None => ctx.client.http().get(url),
    }
    .timeout(timeout);
    let _guard = RequestGuard::new(&ctx.run);
    tokio::select! {
        r = async { read_json::<T>(req.send().await.map_err(from_reqwest)?, |s| format!("{what} failed ({s})")).await } => r,
        _ = ctx.run.control.token().cancelled() => Err(SendErr::Killed),
    }
}

/// Retry policy of one request (web: the loop in `uploadOneChunk`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Retry {
    /// Chunks: duplicates are harmless; transient problems never burn the budget.
    Idempotent,
    /// Whole-file requests that create something: only retried when the server
    /// cannot have processed them (not fully sent, or 429/502/503/504).
    Once { size: u64 },
    /// Small control requests (init, resume check): transient problems are waited
    /// out like for chunks, anything else fails at once.
    TransientOnly,
}

/// Run `attempt` until it succeeds, with the web's back-off rules.
async fn with_retries<T, F, Fut>(ctx: &Ctx, what: &str, policy: Retry, attempt: F) -> Result<T, FileError>
where
    F: Fn(Arc<Attempt>) -> Fut,
    Fut: std::future::Future<Output = Result<T, SendErr>>,
{
    let mut retries = 0u32;
    let mut transient_since: Option<Instant> = None;
    loop {
        if wait_while_paused(ctx).await.is_err() || wait_out_backoff(ctx).await.is_err() {
            return Err(FileError::Killed);
        }
        let bytes = Arc::new(Attempt::default());
        let res = attempt(bytes.clone()).await;
        // on success the attempt has already credited the confirmed bytes, so the bar never dips
        bytes.release(&ctx.run);
        let err = match res {
            Ok(v) => return Ok(v),
            Err(e) => e,
        };
        let fully_sent = matches!(policy, Retry::Once { size } if bytes.total.load(Ordering::Relaxed) >= size);
        match &err {
            SendErr::Killed => return Err(FileError::Killed),
            SendErr::Auth => return Err(FileError::Failed(err.describe(what))),
            _ => {}
        }
        let retryable_once = match policy {
            Retry::Idempotent => true,
            Retry::TransientOnly => err.is_transient(),
            Retry::Once { .. } => match &err {
                SendErr::Http { status, .. } => matches!(status, 429 | 502 | 503 | 504),
                SendErr::Network(_) | SendErr::Timeout => !fully_sent,
                _ => false,
            },
        };
        if !retryable_once {
            return Err(FileError::Failed(err.describe(what)));
        }
        if err.is_transient() {
            let since = *transient_since.get_or_insert_with(Instant::now);
            if since.elapsed() > TRANSIENT_GIVE_UP {
                return Err(FileError::Failed(format!(
                    "{what} kept failing for {} min: {}",
                    TRANSIENT_GIVE_UP.as_secs() / 60,
                    err.describe(what)
                )));
            }
            ctx.backoff.note_transient(err.retry_after(), ctx.active.load(Ordering::Acquire));
            ctx.run.set_status(if err.is_network() { RunStatus::Reconnecting } else { RunStatus::Busy });
            if err.is_network() && wait_for_connection(ctx).await.is_err() {
                return Err(FileError::Killed);
            }
            if wait_out_backoff(ctx).await.is_err() {
                return Err(FileError::Killed);
            }
            ctx.run.reset_speed_baseline();
            ctx.run.mark_resumed();
            continue;
        }
        transient_since = None;
        retries += 1;
        if retries > MAX_RETRIES {
            return Err(FileError::Failed(format!("{what} failed after {MAX_RETRIES} attempts: {}", err.describe(what))));
        }
        if sleep_or_cancel(ctx, ctx.pace(retry_delay(retries))).await.is_err() {
            return Err(FileError::Killed);
        }
    }
}

// ── one chunked file ────────────────────────────────────────────────────────

struct Claim {
    index: u32,
    start: u64,
    end: u64,
    is_last: bool,
}

#[derive(Default)]
struct Claims {
    /// Fixed mode: indices still to send.
    queue: VecDeque<u32>,
    /// Byte range of every claimed index.
    ranges: Vec<(u64, u64)>,
    /// Auto mode: ranges from an earlier attempt the server never confirmed.
    redo: VecDeque<u32>,
    next_offset: u64,
    runs: PlanRuns,
    completed: BTreeSet<u32>,
    done_bytes: u64,
}

struct ChunkedFile {
    idx: usize,
    file: UploadFile,
    name: String,
    key: String,
    dynamic: bool,
    total: Option<u32>,
    upload_id: String,
    claims: Mutex<Claims>,
    has_error: AtomicBool,
    first_error: Mutex<Option<String>>,
    failed: Mutex<Vec<u32>>,
}

impl ChunkedFile {
    fn record(&self, ctx: &Ctx, c: &Claims) -> ResumeRecord {
        let completed: Vec<u32> = c.completed.iter().copied().collect();
        let mut rec = ResumeRecord {
            upload_id: Some(self.upload_id.clone()),
            completed_count: 0,
            completed_chunks: Vec::new(),
            total_chunks: self.total,
            dynamic: self.dynamic,
            chunk_plan_runs: self.dynamic.then(|| c.runs.clone()),
            done_bytes: c.done_bytes,
            batch_id: ctx.batch_id.clone(),
            expiration_minutes: ctx.expiration,
            saved_at: now_ms(),
            name: self.name.clone(),
            size: self.file.size,
            last_modified: self.file.modified_ms,
            path: Some(self.file.path.clone()),
        };
        rec.set_completed(&completed);
        rec
    }

    fn persist(&self, ctx: &Ctx, c: &Claims) {
        ctx.resume.put(&self.key, self.record(ctx, c));
    }

    fn claim_next(&self, ctx: &Ctx) -> Option<Claim> {
        let mut c = self.claims.lock().unwrap();
        if !self.dynamic {
            let index = c.queue.pop_front()?;
            let (start, end) = c.ranges[index as usize];
            return Some(Claim { index, start, end, is_last: false });
        }
        if let Some(index) = c.redo.pop_front() {
            let (start, end) = c.ranges[index as usize];
            return Some(Claim { index, start, end, is_last: end >= self.file.size });
        }
        if c.next_offset >= self.file.size {
            return None;
        }
        let plan = ctx.live_plan();
        // the settings card shows the live plan, not just the file-start estimate
        ctx.run.plan.lock().unwrap().replace(plan);
        let size = plan.size_bytes.max(1);
        let start = c.next_offset;
        let end = (start + size).min(self.file.size);
        let index = c.ranges.len() as u32;
        c.ranges.push((start, end));
        c.runs.push(end - start);
        c.next_offset = end;
        // record the plan the moment it is decided: a resumed upload must reuse the exact boundaries
        self.persist(ctx, &c);
        Some(Claim { index, start, end, is_last: end >= self.file.size })
    }

    fn on_chunk_done(&self, ctx: &Ctx, index: u32, len: u64) {
        let mut c = self.claims.lock().unwrap();
        if c.completed.insert(index) {
            c.done_bytes += len;
        }
        self.persist(ctx, &c);
    }
}

async fn chunk_task(ctx: Arc<Ctx>, cf: Arc<ChunkedFile>, claim: Claim, slot: Slot) -> Result<(), FileError> {
    let _slot = slot;
    let len = claim.end - claim.start;
    let what = format!("chunk {}", claim.index);
    let res = with_retries(&ctx, &what, Retry::Idempotent, |sent| {
        let (ctx, cf, what) = (ctx.clone(), cf.clone(), what.clone());
        let (index, start, is_last) = (claim.index, claim.start, claim.is_last);
        async move {
            let mut before = vec![
                ("uploadId", cf.upload_id.clone()),
                ("chunkIndex", index.to_string()),
                // the server rejects a truncated body instead of storing it as "received"
                ("chunkSize", len.to_string()),
            ];
            if cf.dynamic {
                before.push(("isLastChunk", is_last.to_string()));
            }
            let part = PartSpec {
                path: &cf.file.path,
                start,
                len,
                file_idx: cf.idx,
                field: "chunk",
                file_name: "chunk".into(),
                mime: "application/octet-stream".into(),
            };
            let ack: ChunkAck =
                send_multipart(&ctx, "api/upload/chunk", &before, &[part], &[], Some(chunk_timeout(len)), &sent, &what).await?;
            if !ack.success {
                return Err(SendErr::Bad(format!("Chunk {index} returned success=false")));
            }
            // credit before the attempt's in-flight bytes are dropped, so the bar never dips
            ctx.run.add_confirmed(cf.idx, len);
            Ok(())
        }
    })
    .await;
    match res {
        Ok(()) => {
            cf.on_chunk_done(&ctx, claim.index, len);
            ctx.backoff.note_success(ctx.target());
            ctx.run.clear_resuming();
            Ok(())
        }
        Err(e) => {
            cf.has_error.store(true, Ordering::Relaxed);
            if let FileError::Failed(m) = &e {
                cf.failed.lock().unwrap().push(claim.index);
                cf.first_error.lock().unwrap().get_or_insert_with(|| m.clone());
            }
            Err(e)
        }
    }
}

struct FileSlot {
    fully_claimed: AtomicBool,
}

impl FileSlot {
    fn set(&self, ctx: &Ctx) {
        self.fully_claimed.store(true, Ordering::Release);
        ctx.claimed.notify_one();
    }
}

async fn upload_chunked(ctx: Arc<Ctx>, idx: usize, file: UploadFile, slot_state: Arc<FileSlot>) -> Result<UploadCompleteResponse, FileError> {
    let dynamic = ctx.mode == ChunkMode::Auto;
    if dynamic {
        let samples_avg = ctx.run.samples.lock().unwrap().recent_avg();
        let plan = establish_auto_plan(samples_avg, ctx.history_avg, ctx.max_conn);
        *ctx.start_plan.lock().unwrap() = plan;
        ctx.run.plan.lock().unwrap().replace(plan);
    }
    let chunk_size = match ctx.mode {
        ChunkMode::Fixed { size_mb, .. } => size_mb as u64 * 1024 * 1024,
        ChunkMode::Auto => ctx.start_plan.lock().unwrap().size_bytes,
    };
    let total = (!dynamic).then(|| file.size.div_ceil(chunk_size.max(1)) as u32);
    let key = resume_key(file.size, file.modified_ms);
    let name = file.upload_name();
    let mut claims = Claims::default();
    if !dynamic {
        claims.ranges = fixed_ranges(file.size, chunk_size);
        claims.queue = (0..claims.ranges.len() as u32).collect();
    }

    // ── resume check: only when the record matches the current mode ──────
    let saved = ctx.resume.get(&key, now_ms());
    let mut upload_id = None;
    if let Some(rec) = saved.as_ref().filter(|r| r.dynamic == dynamic && (dynamic || r.total_chunks == total)) {
        if let Some(old_id) = rec.upload_id.clone() {
            let path = format!("api/upload/status/{}", crate::client::path_segment(&old_id));
            let status = match with_retries(&ctx, "resume check", Retry::TransientOnly, |_| {
                send_json::<UploadStatus>(&ctx, &path, None, Duration::from_secs(30), "Resume check")
            })
            .await
            {
                Ok(s) => Some(s),
                Err(FileError::Killed) => return Err(FileError::Killed),
                Err(FileError::Failed(_)) => None,
            };
            if let Some(st) = status.filter(|s| s.active) {
                let received: BTreeSet<u32> = st.received_chunks.iter().copied().collect();
                let mut ok = true;
                if dynamic {
                    match rec.chunk_plan_runs.as_ref().and_then(|r| r.decode(file.size)) {
                        // every chunk the server holds must map to a range we know, otherwise the
                        // index would be reused with different bytes and the file would come out corrupt
                        Some(ranges) if received.iter().all(|i| (*i as usize) < ranges.len()) => {
                            claims.next_offset = ranges.iter().map(|r| r.1).max().unwrap_or(0);
                            claims.runs = PlanRuns::from_ranges(&ranges);
                            claims.redo = (0..ranges.len() as u32).filter(|i| !received.contains(i)).collect();
                            claims.ranges = ranges;
                        }
                        _ => ok = false,
                    }
                } else {
                    claims.queue.retain(|i| !received.contains(i));
                }
                if ok {
                    for i in &received {
                        if let Some((s, e)) = claims.ranges.get(*i as usize) {
                            claims.done_bytes += e - s;
                            ctx.run.add_skipped(idx, e - s);
                        }
                    }
                    claims.completed = received.clone();
                    upload_id = Some(old_id);
                    ctx.run.set_status(RunStatus::Resuming { chunks: received.len() as u32, total });
                    ctx.run.mark_resumed();
                } else {
                    claims = Claims::default();
                    // best effort: free the unusable server-side remains
                    let client = ctx.client.clone();
                    tokio::spawn(async move { client.delete_upload_chunks(&old_id).await });
                }
            }
        }
    }
    let resume_failed = saved.is_some() && upload_id.is_none();

    let upload_id = match upload_id {
        Some(id) => id,
        None => {
            if !dynamic {
                claims.ranges = fixed_ranges(file.size, chunk_size);
                claims.queue = (0..claims.ranges.len() as u32).collect();
            }
            let init = UploadInit {
                filename: name.clone(),
                file_size: file.size,
                mime_type: file.mime.clone(),
                total_chunks: total,
                upload_mode: upload_mode(chunk_size).into(),
                expiration_minutes: ctx.expiration,
                batch_id: ctx.batch_id.clone(),
            };
            let body = serde_json::to_value(&init).map_err(|e| FileError::Failed(e.to_string()))?;
            let r: UploadInitResponse = with_retries(&ctx, "upload init", Retry::TransientOnly, |_| {
                send_json(&ctx, "api/upload/init", Some(&body), Duration::from_secs(60), "Init")
            })
            .await?;
            r.upload_id
        }
    };
    ctx.run.upload_ids.lock().unwrap().insert(upload_id.clone());
    if ctx.cancelled() {
        return Err(FileError::Killed);
    }

    let cf = Arc::new(ChunkedFile {
        idx,
        file,
        name,
        key,
        dynamic,
        total,
        upload_id: upload_id.clone(),
        claims: Mutex::new(claims),
        has_error: AtomicBool::new(false),
        first_error: Mutex::new(None),
        failed: Mutex::new(Vec::new()),
    });
    // Persist a marker right away (a slow first chunk must not leave nothing behind),
    // except when an older record exists whose resume failed: do not stomp its progress yet.
    if !resume_failed {
        let c = cf.claims.lock().unwrap();
        cf.persist(&ctx, &c);
    }

    let mut tasks: JoinSet<Result<(), FileError>> = JoinSet::new();
    let mut killed = false;
    loop {
        if cf.has_error.load(Ordering::Relaxed) {
            break;
        }
        let Some(slot) = acquire(&ctx).await else {
            killed = true;
            break;
        };
        let Some(claim) = cf.claim_next(&ctx) else { break };
        tasks.spawn(chunk_task(ctx.clone(), cf.clone(), claim, slot));
        while let Some(done) = tasks.try_join_next() {
            if matches!(done, Ok(Err(FileError::Killed))) {
                killed = true;
            }
        }
    }
    slot_state.set(&ctx);
    while let Some(done) = tasks.join_next().await {
        if matches!(done, Ok(Err(FileError::Killed))) {
            killed = true;
        }
    }
    if killed || ctx.cancelled() {
        ctx.resume.remove(&cf.key);
        let client = ctx.client.clone();
        tokio::spawn(async move { client.delete_upload_chunks(&upload_id).await });
        return Err(FileError::Killed);
    }
    let failed = cf.failed.lock().unwrap().clone();
    if cf.has_error.load(Ordering::Relaxed) || !failed.is_empty() {
        let list: Vec<String> = failed.iter().map(u32::to_string).collect();
        let first = cf.first_error.lock().unwrap().clone().unwrap_or_default();
        return Err(FileError::Failed(format!("Upload failed. Problem chunks: {}.\n{first}", list.join(", "))));
    }

    ctx.run.set_status(RunStatus::Finishing);
    let res = complete(&ctx, &cf).await;
    ctx.run.set_status(RunStatus::Uploading);
    if res.is_ok() {
        ctx.resume.remove(&cf.key);
    }
    res
}

fn init_error(e: crate::Error) -> String {
    match e {
        crate::Error::Server { message, .. } if !message.is_empty() => message,
        crate::Error::Server { .. } => "Init failed".into(),
        other => other.to_string(),
    }
}

/// `/api/upload/complete`. Assembling a multi-GB file can outlast a proxy timeout:
/// then the server finishes anyway, and the file is looked up in the recent list.
async fn complete(ctx: &Ctx, cf: &ChunkedFile) -> Result<UploadCompleteResponse, FileError> {
    let body = UploadComplete {
        upload_id: cf.upload_id.clone(),
        anonymize: false,
        remove_timestamp: ctx.remove_timestamp,
        expiration_minutes: ctx.expiration,
        batch_id: ctx.batch_id.clone(),
    };
    for attempt in 0..3 {
        let req = match ctx.client.endpoint("api/upload/complete") {
            Ok(u) => ctx.client.http().post(u).json(&body).timeout(Duration::from_secs(30 * 60)),
            Err(e) => return Err(FileError::Failed(e.to_string())),
        };
        let res = match req.send().await {
            Ok(r) => read_json::<UploadCompleteResponse>(r, |_| "Assembly failed".into()).await,
            Err(e) => Err(from_reqwest(e)),
        };
        match res {
            Ok(r) => return Ok(r),
            // rate limited / unavailable before the server started: try again
            Err(SendErr::Http { status: 429 | 503, retry_after, .. }) if attempt < 2 => {
                let wait = retry_after.unwrap_or(retry_delay(attempt + 2));
                tokio::time::sleep(ctx.pace(wait)).await;
            }
            // 409 = assembly already running; gateway timeouts / dropped connections: the server may still finish
            Err(e @ (SendErr::Http { status: 409 | 502 | 504 | 520..=524, .. } | SendErr::Network(_) | SendErr::Timeout)) => {
                return find_completed(ctx, cf).await.ok_or_else(|| FileError::Failed(e.describe("assembly")));
            }
            Err(e) => return Err(FileError::Failed(e.describe("assembly"))),
        }
    }
    Err(FileError::Failed("Assembly failed".into()))
}

/// Look for the finished file in `/api/user/sync` for up to 3 minutes.
async fn find_completed(ctx: &Ctx, cf: &ChunkedFile) -> Option<UploadCompleteResponse> {
    let since = now_ms() - ctx.run.elapsed().as_millis() as i64 - 60_000;
    for _ in 0..60 {
        tokio::time::sleep(ctx.pace(Duration::from_secs(3))).await;
        let Ok(SyncResponse::Full { files, .. }) = ctx.client.sync(None).await else { continue };
        let hit = files.into_iter().find(|f| {
            f.name == cf.name && f.size == cf.file.size && f.uploaded_at.is_some_and(|t| t >= since)
        });
        if let Some(f) = hit {
            return Some(UploadCompleteResponse {
                link: f.link,
                kind: f.category,
                filename: f.name,
                size: f.size,
                file_id: f.id,
                styled_link_pending: true,
                ..Default::default()
            });
        }
    }
    None
}

// ── one small file (single request) ─────────────────────────────────────────

async fn upload_small(ctx: Arc<Ctx>, idx: usize, file: UploadFile, slot_state: Arc<FileSlot>) -> Result<UploadCompleteResponse, FileError> {
    let Some(slot) = acquire(&ctx).await else { return Err(FileError::Killed) };
    slot_state.set(&ctx);
    let _slot = slot;
    let key = resume_key(file.size, file.modified_ms);
    let name = file.upload_name();
    // not resumable, but a crash mid-upload shows up as NOT FINISHED instead of vanishing
    ctx.resume.put(
        &key,
        ResumeRecord {
            upload_id: None,
            completed_count: 0,
            completed_chunks: Vec::new(),
            total_chunks: Some(1),
            dynamic: false,
            chunk_plan_runs: None,
            done_bytes: 0,
            batch_id: ctx.batch_id.clone(),
            expiration_minutes: ctx.expiration,
            saved_at: now_ms(),
            name: name.clone(),
            size: file.size,
            last_modified: file.modified_ms,
            path: Some(file.path.clone()),
        },
    );
    // no deadline on the web; here a generous one so a dead connection cannot hang forever
    let timeout = Duration::from_secs(120) + Duration::from_secs(file.size / (32 * 1024));
    let res = with_retries(&ctx, "upload", Retry::Once { size: file.size }, |sent| {
        let (ctx, file, name) = (ctx.clone(), file.clone(), name.clone());
        async move {
            let part = PartSpec {
                path: &file.path,
                start: 0,
                len: file.size,
                file_idx: idx,
                field: "file",
                file_name: name,
                mime: file.mime.clone(),
            };
            let mut after = vec![
                // anonymizing is done client-side by renaming
                ("anonymize", "false".to_string()),
                ("removeTimestamp", ctx.remove_timestamp.to_string()),
                ("expirationMinutes", ctx.expiration.to_string()),
            ];
            if let Some(b) = &ctx.batch_id {
                after.push(("batchId", b.clone()));
            }
            let r: UploadCompleteResponse =
                send_multipart(&ctx, "api/upload", &[], &[part], &after, Some(timeout), &sent, "Upload").await?;
            ctx.run.add_confirmed(idx, file.size);
            Ok(r)
        }
    })
    .await;
    match &res {
        Ok(_) | Err(FileError::Killed) => ctx.resume.remove(&key),
        Err(_) => {}
    }
    res
}

// ── the run ─────────────────────────────────────────────────────────────────

impl Uploader {
    pub fn new(client: Client, resume: Arc<ResumeStore>, history: Arc<HistoryStore>) -> Self {
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
        Self { client, resume, history, max_auto_connections: auto_max_connections(threads), file_concurrency: 2, time_scale: 1.0 }
    }

    /// Shorten every retry pause / poll interval (tests only).
    #[doc(hidden)]
    pub fn with_time_scale(mut self, scale: f64) -> Self {
        self.time_scale = scale;
        self
    }

    /// Override the device thread count used by the auto planner (tests).
    pub fn with_max_connections(mut self, n: u32) -> Self {
        self.max_auto_connections = n.clamp(AUTO_MIN_CONNECTIONS, MAX_CHUNK_COUNT);
        self
    }

    pub fn with_file_concurrency(mut self, n: usize) -> Self {
        self.file_concurrency = n.max(1);
        self
    }

    pub fn resume_store(&self) -> &Arc<ResumeStore> {
        &self.resume
    }

    pub fn history(&self) -> &Arc<HistoryStore> {
        &self.history
    }

    /// The plan the next auto-sized upload would start with (settings card when idle).
    pub fn idle_plan(&self) -> Option<ChunkPlan> {
        self.history.get().average_mbs().map(|h| establish_auto_plan(None, Some(h), self.max_auto_connections))
    }

    /// Upload everything in `req`. `run` must describe the same files in the same order.
    pub async fn run(&self, req: UploadRequest, run: Arc<Run>, events: Arc<dyn Fn(Event) + Send + Sync>) -> RunReport {
        let started = Instant::now();
        // stats sampling + ping, only while the run lasts
        let sampler = {
            let run = run.clone();
            tokio::spawn(async move {
                loop {
                    let next = run.sample();
                    tokio::time::sleep(next).await;
                }
            })
        };
        let pinger = {
            let (run, client) = (run.clone(), self.client.clone());
            tokio::spawn(async move {
                loop {
                    let t0 = Instant::now();
                    if let Ok(Ok(_)) = tokio::time::timeout(Duration::from_secs(10), client.status()).await {
                        let ms = t0.elapsed().as_secs_f64() * 1000.0;
                        run.samples.lock().unwrap().ping.push(ms);
                        *run.last_ping.lock().unwrap() = Some(ms);
                    }
                    tokio::time::sleep(Duration::from_millis(2500)).await;
                }
            })
        };
        let outcome = if req.protected { self.run_protected(&req, &run).await } else { self.run_public(&req, &run, &events).await };
        sampler.abort();
        pinger.abort();
        // upload_ids stay: the server's pending list lags a few seconds behind, the app keeps hiding them
        run.active_keys.lock().unwrap().clear();

        let any_ok = match &outcome {
            Outcome::Single(f) => f.result.is_ok(),
            Outcome::Batch { files, .. } => files.iter().any(|f| f.result.is_ok()),
            Outcome::Protected(r) => r.is_ok(),
            Outcome::Failed(_) => false,
        };
        let stats = any_ok.then(|| {
            let last_ping = run.last_ping();
            run.samples.lock().unwrap().finish(&self.history, run.total_bytes, started.elapsed().as_secs_f64(), last_ping)
        });
        RunReport { outcome, stats }
    }

    fn ctx(&self, req: &UploadRequest, run: &Arc<Run>, batch_id: Option<String>, mode: ChunkMode) -> Arc<Ctx> {
        Arc::new(Ctx {
            client: self.client.clone(),
            run: run.clone(),
            resume: self.resume.clone(),
            backoff: Backoff::with_scale(self.time_scale),
            mode,
            max_conn: self.max_auto_connections,
            start_plan: Mutex::new(establish_auto_plan(None, self.history.get().average_mbs(), self.max_auto_connections)),
            history_avg: self.history.get().average_mbs(),
            expiration: req.expiration_minutes,
            remove_timestamp: req.remove_timestamp,
            batch_id,
            active: AtomicU32::new(0),
            slot_freed: Notify::new(),
            claimed: Notify::new(),
            last_online: tokio::sync::Mutex::new(None),
            time_scale: self.time_scale,
        })
    }

    async fn run_public(&self, req: &UploadRequest, run: &Arc<Run>, events: &Arc<dyn Fn(Event) + Send + Sync>) -> Outcome {
        let n = req.files.len();
        if n == 0 {
            return Outcome::Failed("Select files first".into());
        }
        let multi = n > 1;
        let batch_id = if multi {
            match self.client.batch_init(req.expiration_minutes).await {
                Ok(id) => Some(id),
                Err(e) => return Outcome::Failed(init_error_batch(e)),
            }
        } else {
            None
        };
        let ctx = self.ctx(req, run, batch_id.clone(), req.chunking);
        if ctx.mode == ChunkMode::Auto {
            run.plan.lock().unwrap().replace(*ctx.start_plan.lock().unwrap());
        }
        for f in &req.files {
            run.active_keys.lock().unwrap().insert(resume_key(f.size, f.modified_ms));
        }

        let mut results: Vec<Option<Result<UploadCompleteResponse, String>>> = vec![None; n];
        let mut running = FuturesUnordered::new();
        // (file index, its claim state) of the files in flight
        let mut slots: Vec<(usize, Arc<FileSlot>)> = Vec::new();
        let mut next = 0usize;
        let mut paused = run.control.paused_rx();
        loop {
            // the next file starts once every running one has claimed all of its chunks
            while next < n
                && running.len() < self.file_concurrency
                && !ctx.cancelled()
                && !*paused.borrow_and_update()
                && slots.iter().all(|(_, s)| s.fully_claimed.load(Ordering::Acquire))
            {
                let idx = next;
                next += 1;
                let file = req.files[idx].clone();
                let slot = Arc::new(FileSlot { fully_claimed: AtomicBool::new(false) });
                slots.push((idx, slot.clone()));
                run.file_started(idx);
                events(Event::FileStarted(idx));
                let ctx = ctx.clone();
                running.push(async move {
                    let r = if file.size > CHUNK_THRESHOLD {
                        upload_chunked(ctx, idx, file, slot).await
                    } else {
                        upload_small(ctx, idx, file, slot).await
                    };
                    (idx, r)
                });
            }
            if running.is_empty() && (next >= n || ctx.cancelled()) {
                break;
            }
            let claimed = ctx.claimed.notified();
            let cancelled = ctx.cancelled();
            tokio::select! {
                Some((idx, r)) = running.next() => {
                    slots.retain(|(i, _)| *i != idx);
                    match r {
                        Ok(resp) => {
                            run.files[idx].set_phase(Phase::Done);
                            events(Event::FileDone(idx, Box::new(resp.clone())));
                            results[idx] = Some(Ok(resp));
                        }
                        Err(e) => {
                            run.files[idx].set_phase(Phase::Failed);
                            let msg = e.message();
                            events(Event::FileFailed(idx, msg.clone()));
                            results[idx] = Some(Err(msg));
                        }
                    }
                }
                _ = claimed => {}
                _ = paused.changed() => {}
                // after a kill only the running files are awaited
                _ = run.control.token().cancelled(), if !cancelled => {}
            }
        }
        // files never started because of a kill
        let files: Vec<FileResult> = req
            .files
            .iter()
            .zip(results)
            .map(|(f, r)| FileResult { name: f.upload_name(), result: r.unwrap_or_else(|| Err("Upload killed".into())) })
            .collect();
        match batch_id {
            None => Outcome::Single(files.into_iter().next().expect("one file")),
            Some(id) => {
                let batch = self
                    .client
                    .batch_complete(&id)
                    .await
                    .map_err(|e| format!("Batch complete failed: {}", init_error(e)));
                Outcome::Batch { files, batch }
            }
        }
    }

    /// "TVFS Users Only" share: one request up to 25 MB in total, otherwise the
    /// chunked `/api/protected/upload/*` flow (25 MB chunks, the server's parallelism).
    async fn run_protected(&self, req: &UploadRequest, run: &Arc<Run>) -> Outcome {
        if req.files.is_empty() {
            return Outcome::Failed("Select files first".into());
        }
        for i in 0..req.files.len() {
            run.file_started(i);
        }
        let total: u64 = req.files.iter().map(|f| f.size).sum();
        let result = if total > PROTECTED_CHUNK {
            self.protected_chunked(req, run).await
        } else {
            let ctx = self.ctx(req, run, None, ChunkMode::Fixed { count: 1, size_mb: 25 });
            protected_direct(&ctx, &req.files, total).await
        };
        let phase = if result.is_ok() { Phase::Done } else { Phase::Failed };
        for f in &run.files {
            f.set_phase(phase);
        }
        Outcome::Protected(result.map_err(|e| e.message()))
    }

    async fn protected_chunked(&self, req: &UploadRequest, run: &Arc<Run>) -> Result<ProtectedShareResponse, FileError> {
        let list: Vec<serde_json::Value> = req
            .files
            .iter()
            .map(|f| serde_json::json!({ "relativePath": f.share_path(), "size": f.size, "mimeType": f.mime }))
            .collect();
        let form = [
            ("files", serde_json::Value::Array(list).to_string()),
            ("expirationMinutes", req.expiration_minutes.to_string()),
        ];
        let url = self.client.endpoint("api/protected/upload/init").map_err(|e| FileError::Failed(e.to_string()))?;
        let init = tokio::select! {
            r = async {
                let resp = self.client.http().post(url).form(&form).timeout(Duration::from_secs(60)).send().await.map_err(from_reqwest)?;
                read_json::<ProtectedInitResponse>(resp, |_| "Failed to start upload".into()).await
            } => r,
            _ = run.control.token().cancelled() => return Err(FileError::Killed),
        }
        .map_err(|e| FileError::Failed(e.describe("upload start")))?;
        let chunk = init.chunk_size.filter(|c| *c > 0).unwrap_or(PROTECTED_CHUNK);
        let parallel = init.recommended_parallel.filter(|p| *p > 0).unwrap_or(4);
        let ctx = self.ctx(req, run, None, ChunkMode::Fixed { count: parallel, size_mb: (chunk / (1024 * 1024)) as u32 });

        // every (file, chunk) of the share in one queue; empty files still send one empty chunk
        let mut queue: VecDeque<(usize, u32, u64, u64)> = VecDeque::new();
        for (fi, f) in req.files.iter().enumerate() {
            let n = f.size.div_ceil(chunk).max(1);
            for c in 0..n {
                queue.push_back((fi, c as u32, c * chunk, ((c + 1) * chunk).min(f.size)));
            }
        }
        let upload_id = Arc::new(init.upload_id);
        let files = Arc::new(req.files.clone());
        let failed = Arc::new(AtomicBool::new(false));
        let mut tasks: JoinSet<Result<(), FileError>> = JoinSet::new();
        let mut first_err: Option<FileError> = None;
        while let Some(item) = queue.pop_front() {
            if failed.load(Ordering::Relaxed) {
                break;
            }
            let Some(slot) = acquire(&ctx).await else {
                first_err = Some(FileError::Killed);
                break;
            };
            let (ctx, upload_id, files, failed) = (ctx.clone(), upload_id.clone(), files.clone(), failed.clone());
            tasks.spawn(async move {
                let _slot = slot;
                let (fi, ci, start, end) = item;
                let len = end - start;
                let what = format!("chunk {ci} of {}", files[fi].share_path());
                let r = with_retries(&ctx, &what, Retry::Idempotent, |bytes| {
                    let (ctx, upload_id, files, what) = (ctx.clone(), upload_id.clone(), files.clone(), what.clone());
                    async move {
                        let part = PartSpec {
                            path: &files[fi].path,
                            start,
                            len,
                            file_idx: fi,
                            field: "chunk",
                            file_name: "blob".into(),
                            mime: "application/octet-stream".into(),
                        };
                        let after =
                            [("uploadId", upload_id.to_string()), ("fileIndex", fi.to_string()), ("chunkIndex", ci.to_string())];
                        let _: serde_json::Value = send_multipart(
                            &ctx,
                            "api/protected/upload/chunk",
                            &[],
                            &[part],
                            &after,
                            Some(chunk_timeout(len)),
                            &bytes,
                            &what,
                        )
                        .await?;
                        ctx.run.add_confirmed(fi, len);
                        Ok(())
                    }
                })
                .await;
                if r.is_ok() {
                    ctx.backoff.note_success(ctx.target());
                } else {
                    failed.store(true, Ordering::Relaxed);
                }
                r
            });
            while let Some(done) = tasks.try_join_next() {
                if let Ok(Err(e)) = done {
                    first_err.get_or_insert(e);
                }
            }
        }
        while let Some(done) = tasks.join_next().await {
            if let Ok(Err(e)) = done {
                first_err.get_or_insert(e);
            }
        }
        if ctx.cancelled() {
            return Err(FileError::Killed);
        }
        if let Some(e) = first_err {
            return Err(e);
        }
        run.set_status(RunStatus::Finishing);
        let url = self.client.endpoint("api/protected/upload/complete").map_err(|e| FileError::Failed(e.to_string()))?;
        let resp = self
            .client
            .http()
            .post(url)
            .form(&[("uploadId", upload_id.as_str())])
            .timeout(Duration::from_secs(30 * 60))
            .send()
            .await
            .map_err(|e| FileError::Failed(from_reqwest(e).describe("finalizing")))?;
        let r = read_json::<ProtectedShareResponse>(resp, |_| "Failed to finalize upload".into()).await;
        run.set_status(RunStatus::Uploading);
        r.map_err(|e| FileError::Failed(e.describe("finalizing")))
    }
}

/// Small "TVFS Users Only" share: every file in one multipart request.
async fn protected_direct(ctx: &Arc<Ctx>, files: &[UploadFile], total: u64) -> Result<ProtectedShareResponse, FileError> {
    let paths: Vec<String> = files.iter().map(UploadFile::share_path).collect();
    let rel = serde_json::to_string(&paths).unwrap_or_else(|_| "[]".into());
    with_retries(ctx, "upload", Retry::Once { size: total }, |bytes| {
        let (ctx, rel) = (ctx.clone(), rel.clone());
        async move {
            let parts: Vec<PartSpec> = files
                .iter()
                .enumerate()
                .map(|(i, f)| PartSpec {
                    path: &f.path,
                    start: 0,
                    len: f.size,
                    file_idx: i,
                    field: "files",
                    file_name: f.upload_name(),
                    mime: f.mime.clone(),
                })
                .collect();
            let after = [
                ("relativePaths", rel),
                ("pairingEnabled", "false".to_string()),
                ("expirationMinutes", ctx.expiration.to_string()),
            ];
            let timeout = Duration::from_secs(120) + Duration::from_secs(total / (32 * 1024));
            let r: ProtectedShareResponse =
                send_multipart(&ctx, "api/protected/upload", &[], &parts, &after, Some(timeout), &bytes, "Upload").await?;
            for (i, f) in files.iter().enumerate() {
                ctx.run.add_confirmed(i, f.size);
            }
            Ok(r)
        }
    })
    .await
}

fn init_error_batch(e: crate::Error) -> String {
    match e {
        crate::Error::Server { message, .. } if !message.is_empty() => message,
        crate::Error::Server { .. } => "Batch init failed".into(),
        other => other.to_string(),
    }
}
