//! Mock-server tests of the upload engine: a fake TVFS (wiremock) that stores
//! chunks and reassembles them, plus a TCP proxy that really drops connections.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::*;
use crate::models::UploadCompleteResponse;
use crate::Client;

const MIB: u64 = 1024 * 1024;

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

/// One part of a multipart body: (field name, file name, data).
type PartData = (String, Option<String>, Vec<u8>);

fn parse_multipart(req: &Request) -> Vec<PartData> {
    let ct = req.headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or_default().to_owned();
    let boundary = ct.split("boundary=").nth(1).expect("multipart boundary").trim_matches('"').to_owned();
    let delim = format!("--{boundary}").into_bytes();
    let body = &req.body;
    let mut starts = Vec::new();
    let mut i = 0;
    while i + delim.len() <= body.len() {
        if body[i..].starts_with(&delim) {
            starts.push(i);
            i += delim.len();
        } else {
            i += 1;
        }
    }
    let mut parts = Vec::new();
    for w in starts.windows(2) {
        let seg = &body[w[0] + delim.len()..w[1]];
        let seg = seg.strip_prefix(b"\r\n").unwrap_or(seg);
        let seg = seg.strip_suffix(b"\r\n").unwrap_or(seg);
        let split = seg.windows(4).position(|x| x == b"\r\n\r\n").expect("part headers");
        let head = String::from_utf8_lossy(&seg[..split]).to_string();
        let data = seg[split + 4..].to_vec();
        let disposition = head.lines().find(|l| l.to_ascii_lowercase().starts_with("content-disposition")).unwrap_or_default().to_owned();
        let attr = |key: &str| {
            disposition
                .split(';')
                .map(str::trim)
                .find_map(|kv| kv.strip_prefix(&format!("{key}=\"")).map(|v| v.trim_end_matches('"').to_owned()))
        };
        parts.push((attr("name").unwrap_or_default(), attr("filename"), data));
    }
    parts
}

fn field(parts: &[PartData], name: &str) -> Option<String> {
    parts.iter().find(|p| p.0 == name).map(|p| String::from_utf8_lossy(&p.2).to_string())
}

#[derive(Default)]
struct Upload {
    init: serde_json::Value,
    chunks: BTreeMap<u32, Vec<u8>>,
    total: Option<u32>,
    flagged_last: Vec<u32>,
}

/// The fake server's state and knobs.
#[derive(Default)]
struct State {
    uploads: Mutex<HashMap<String, Upload>>,
    next_id: AtomicU32,
    /// Assembled files by stored name.
    assembled: Mutex<Vec<(String, Vec<u8>)>>,
    small: Mutex<Vec<Vec<PartData>>>,
    deleted: Mutex<Vec<String>>,
    batch_completed: AtomicU32,
    /// Answer this many chunk requests with 503 (+ Retry-After: 0) first.
    busy_chunks: AtomicU32,
    /// Answer this many chunk requests with 500 first (real failures, burn retries).
    broken_chunks: AtomicU32,
    /// Answer every chunk with 500 (exhaust the retries).
    always_broken: std::sync::atomic::AtomicBool,
    /// Delay every chunk answer.
    chunk_delay_ms: AtomicU32,
    /// `/api/upload/status/:id` answers.
    status: Mutex<HashMap<String, serde_json::Value>>,
    /// Event log: "init <name>", "complete <name>".
    log: Mutex<Vec<(Instant, String)>>,
    chunk_requests: AtomicUsize,
    /// "TVFS Users Only": direct uploads, and chunks by (file, chunk).
    protected_direct: Mutex<Vec<Vec<PartData>>>,
    protected_init: Mutex<Option<String>>,
    protected_chunks: Mutex<BTreeMap<(u32, u32), Vec<u8>>>,
    /// Answer this many `/api/upload` requests with 503 first.
    busy_small: AtomicU32,
}

struct Fake(Arc<State>);

impl Respond for Fake {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let s = &self.0;
        let path = req.url.path().to_owned();
        let method = req.method.as_str().to_owned();
        let ok = |v: serde_json::Value| ResponseTemplate::new(200).set_body_json(v);
        match (method.as_str(), path.as_str()) {
            ("GET", "/api/status") => ok(json!({"backend": "UP", "usedBytes": 0, "tier": 1})),
            ("POST", "/api/upload/batch/init") => ok(json!({"batchId": "batch_1"})),
            ("POST", "/api/upload/batch/complete") => {
                s.batch_completed.fetch_add(1, Ordering::SeqCst);
                ok(json!({"batchId": "batch_1", "link": "https://x/files/batch/batch_1.html", "styledLink": "[2 Files](x)", "files": []}))
            }
            ("POST", "/api/upload/init") => {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                let id = format!("u{}", s.next_id.fetch_add(1, Ordering::SeqCst));
                s.log.lock().unwrap().push((Instant::now(), format!("init {}", body["filename"].as_str().unwrap())));
                let total = body["totalChunks"].as_u64().map(|t| t as u32);
                s.uploads.lock().unwrap().insert(id.clone(), Upload { init: body, total, ..Default::default() });
                ok(json!({"uploadId": id, "recommendedParallel": 4, "message": "Upload initialized"}))
            }
            ("POST", "/api/upload/chunk") => {
                s.chunk_requests.fetch_add(1, Ordering::SeqCst);
                let delay = Duration::from_millis(s.chunk_delay_ms.load(Ordering::SeqCst) as u64);
                if s.always_broken.load(Ordering::SeqCst) {
                    return ResponseTemplate::new(500).set_body_json(json!({"error": "Chunk processing failed"}));
                }
                if s.busy_chunks.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| v.checked_sub(1)).is_ok() {
                    return ResponseTemplate::new(503).insert_header("Retry-After", "0").set_body_json(json!({"error": "busy"}));
                }
                if s.broken_chunks.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| v.checked_sub(1)).is_ok() {
                    return ResponseTemplate::new(500).set_body_json(json!({"error": "Chunk processing failed"}));
                }
                let parts = parse_multipart(req);
                let order: Vec<&str> = parts.iter().map(|p| p.0.as_str()).collect();
                assert_eq!(order.last(), Some(&"chunk"), "chunk must be the last field: {order:?}");
                let id = field(&parts, "uploadId").unwrap();
                let idx: u32 = field(&parts, "chunkIndex").unwrap().parse().unwrap();
                let declared: usize = field(&parts, "chunkSize").unwrap().parse().unwrap();
                let data = parts.iter().find(|p| p.0 == "chunk").unwrap().2.clone();
                assert_eq!(declared, data.len(), "chunkSize must match the body");
                let mut ups = s.uploads.lock().unwrap();
                let up = ups.get_mut(&id).expect("unknown upload id");
                if field(&parts, "isLastChunk").as_deref() == Some("true") {
                    up.flagged_last.push(idx);
                    up.total = Some(idx + 1);
                }
                let dup = up.chunks.insert(idx, data).is_some();
                ok(json!({"success": true, "chunkIndex": idx, "received": up.chunks.len(), "total": up.total, "duplicate": dup}))
                    .set_delay(delay)
            }
            ("POST", "/api/upload/complete") => {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                let id = body["uploadId"].as_str().unwrap().to_owned();
                let mut ups = s.uploads.lock().unwrap();
                let up = ups.remove(&id).expect("complete of unknown upload");
                let total = up.total.expect("total known at complete");
                s.log.lock().unwrap().push((Instant::now(), format!("last {:?} of {total}", up.flagged_last)));
                assert!(matches!(up.init["uploadMode"].as_str(), Some("weak" | "safe")));
                assert_eq!(up.chunks.len() as u32, total, "missing chunks");
                let bytes: Vec<u8> = up.chunks.values().flatten().copied().collect();
                let name = up.init["filename"].as_str().unwrap().to_owned();
                s.log.lock().unwrap().push((Instant::now(), format!("complete {name}")));
                let size = bytes.len();
                s.assembled.lock().unwrap().push((name.clone(), bytes));
                ok(json!({"link": format!("https://x/files/download/{name}"), "styledLinkPending": true, "type": "file",
                    "filename": name, "size": size, "fileId": format!("f_{id}")}))
            }
            ("POST", "/api/protected/upload") => {
                s.protected_direct.lock().unwrap().push(parse_multipart(req));
                ok(json!({"message": "Protected share created", "shareId": "s1", "link": "https://x/files/protected/pages/s1.html",
                    "expiresAt": 99, "files": [{"index": 0, "relativePath": "a.txt", "size": 1, "category": "text", "toolLink": "https://x/t"}]}))
            }
            ("POST", "/api/protected/upload/init") => {
                *s.protected_init.lock().unwrap() = Some(String::from_utf8_lossy(&req.body).to_string());
                ok(json!({"uploadId": "p1", "chunkSize": 4 * MIB, "recommendedParallel": 3}))
            }
            ("POST", "/api/protected/upload/chunk") => {
                let parts = parse_multipart(req);
                let order: Vec<&str> = parts.iter().map(|p| p.0.as_str()).collect();
                assert_eq!(order, vec!["chunk", "uploadId", "fileIndex", "chunkIndex"]);
                let fi: u32 = field(&parts, "fileIndex").unwrap().parse().unwrap();
                let ci: u32 = field(&parts, "chunkIndex").unwrap().parse().unwrap();
                s.protected_chunks.lock().unwrap().insert((fi, ci), parts[0].2.clone());
                ok(json!({"success": true, "fileIndex": fi, "chunkIndex": ci}))
            }
            ("POST", "/api/protected/upload/complete") => {
                assert_eq!(String::from_utf8_lossy(&req.body), "uploadId=p1");
                ok(json!({"shareId": "s2", "link": "https://x/files/protected/pages/s2.html", "expiresAt": 5, "files": []}))
            }
            ("POST", "/api/upload") => {
                if s.busy_small.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| v.checked_sub(1)).is_ok() {
                    return ResponseTemplate::new(503).set_body_json(json!({"error": "busy"}));
                }
                let parts = parse_multipart(req);
                let name = parts.iter().find(|p| p.0 == "file").and_then(|p| p.1.clone()).unwrap();
                let size = parts.iter().find(|p| p.0 == "file").unwrap().2.len();
                s.small.lock().unwrap().push(parts);
                ok(json!({"link": format!("https://x/files/download/{name}"), "type": "file", "filename": name, "size": size, "fileId": "f_small"}))
            }
            ("DELETE", p) if p.starts_with("/api/upload/chunks/") => {
                s.deleted.lock().unwrap().push(p.trim_start_matches("/api/upload/chunks/").to_owned());
                ok(json!({"deleted": true}))
            }
            ("GET", p) if p.starts_with("/api/upload/status/") => {
                let id = p.trim_start_matches("/api/upload/status/");
                ok(s.status.lock().unwrap().get(id).cloned().unwrap_or(json!({"active": false})))
            }
            _ => ResponseTemplate::new(404),
        }
    }
}

async fn fake_server() -> (MockServer, Arc<State>) {
    let server = MockServer::start().await;
    let state = Arc::new(State::default());
    Mock::given(any()).respond_with(Fake(state.clone())).mount(&server).await;
    (server, state)
}

fn data_file(dir: &Path, name: &str, size: u64, seed: u64) -> (PathBuf, Vec<u8>) {
    let mut rng = fastrand::Rng::with_seed(seed);
    let mut bytes = vec![0u8; size as usize];
    rng.fill(&mut bytes);
    let p = dir.join(name);
    std::fs::write(&p, &bytes).unwrap();
    (p, bytes)
}

fn uploader(base: &str, resume: Arc<ResumeStore>) -> Uploader {
    let client = Client::with_base(base).unwrap();
    Uploader::new(client, resume, Arc::new(HistoryStore::in_memory())).with_max_connections(8).with_time_scale(0.01)
}

fn request(files: Vec<UploadFile>, chunking: ChunkMode) -> UploadRequest {
    UploadRequest { files, expiration_minutes: 5, remove_timestamp: false, chunking, protected: false }
}

fn no_events() -> Arc<dyn Fn(Event) + Send + Sync> {
    Arc::new(|_| {})
}

async fn run_upload(up: &Uploader, req: UploadRequest) -> (RunReport, Arc<Run>, usize) {
    let run = Run::new(req.files.iter().map(|f| (f.upload_name(), f.size)), Control::default());
    let max_requests = Arc::new(AtomicUsize::new(0));
    let watcher = {
        let (run, max) = (run.clone(), max_requests.clone());
        tokio::spawn(async move {
            loop {
                max.fetch_max(run.snapshot().requests as usize, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
    };
    let report = up.run(req, run.clone(), no_events()).await;
    watcher.abort();
    (report, run, max_requests.load(Ordering::SeqCst))
}

fn single_ok(r: &RunReport) -> &UploadCompleteResponse {
    match &r.outcome {
        Outcome::Single(FileResult { result: Ok(resp), .. }) => resp,
        other => panic!("expected a successful single upload, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn small_file_goes_in_one_request_with_the_web_fields() {
    let (server, state) = fake_server().await;
    let dir = tempfile::tempdir().unwrap();
    let (p, bytes) = data_file(dir.path(), "photo.png", 300_000, 1);
    let mut f = inspect(&p, None).unwrap();
    f.rename("holiday");
    let resume = Arc::new(ResumeStore::in_memory());
    let up = uploader(&server.uri(), resume.clone());
    let (report, run, _) = run_upload(&up, request(vec![f], ChunkMode::Auto)).await;
    let resp = single_ok(&report);
    assert_eq!(resp.size, 300_000);
    let small = state.small.lock().unwrap();
    let parts = &small[0];
    let names: Vec<&str> = parts.iter().map(|p| p.0.as_str()).collect();
    assert_eq!(names, vec!["file", "anonymize", "removeTimestamp", "expirationMinutes"]);
    assert_eq!(parts[0].1.as_deref(), Some("holiday.png"));
    assert_eq!(parts[0].2, bytes);
    assert_eq!(field(parts, "expirationMinutes").as_deref(), Some("5"));
    assert_eq!(field(parts, "anonymize").as_deref(), Some("false"));
    assert!(resume.list(now_ms()).is_empty(), "marker cleared after success");
    let snap = run.snapshot();
    assert_eq!((snap.done, snap.files[0].0), (300_000, Phase::Done));
    assert!(report.stats.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixed_chunks_reassemble_exactly_and_respect_the_connection_count() {
    let (server, state) = fake_server().await;
    state.chunk_delay_ms.store(15, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let (p, bytes) = data_file(dir.path(), "big.bin", 26 * MIB + 12_345, 2);
    let f = inspect(&p, None).unwrap();
    let up = uploader(&server.uri(), Arc::new(ResumeStore::in_memory()));
    let (report, _run, max_req) = run_upload(&up, request(vec![f], ChunkMode::Fixed { count: 3, size_mb: 1 })).await;
    single_ok(&report);
    let assembled = state.assembled.lock().unwrap();
    assert_eq!(assembled.len(), 1);
    assert!(assembled[0].1 == bytes, "reassembled bytes differ");
    assert!((2..=3).contains(&max_req), "parallel requests: {max_req}");
    // fixed mode: totalChunks at init, no isLastChunk
    assert_eq!(state.chunk_requests.load(Ordering::SeqCst), 27);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_chunks_are_contiguous_and_flag_the_last_one() {
    let (server, state) = fake_server().await;
    let dir = tempfile::tempdir().unwrap();
    let (p, bytes) = data_file(dir.path(), "auto.mkv", 27 * MIB + 7, 3);
    let f = inspect(&p, None).unwrap();
    // slow history (0.1 MB/s) -> the first chunks are 96 KB, then the live speed grows them
    let history = Arc::new(HistoryStore::seeded(stats::DeviceHistory { count: 1, sum_mb: 0.1 }));
    let client = Client::with_base(&server.uri()).unwrap();
    let up = Uploader::new(client, Arc::new(ResumeStore::in_memory()), history).with_max_connections(4).with_time_scale(0.01);
    let (report, _, max_req) = run_upload(&up, request(vec![f], ChunkMode::Auto)).await;
    single_ok(&report);
    assert!(state.assembled.lock().unwrap()[0].1 == bytes);
    assert!(max_req <= 4, "{max_req}");
    let log = state.log.lock().unwrap();
    let last = log.iter().find(|(_, e)| e.starts_with("last ")).unwrap().1.clone();
    let n: u32 = last.rsplit(' ').next().unwrap().parse().unwrap();
    assert!(n > 2, "auto mode used several chunks: {last}");
    assert_eq!(last, format!("last [{}] of {n}", n - 1), "exactly the final chunk is flagged");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transient_and_real_failures_are_retried() {
    let (server, state) = fake_server().await;
    state.busy_chunks.store(4, Ordering::SeqCst);
    state.broken_chunks.store(2, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let (p, bytes) = data_file(dir.path(), "r.bin", 26 * MIB, 4);
    let up = uploader(&server.uri(), Arc::new(ResumeStore::in_memory()));
    let (report, _, _) = run_upload(&up, request(vec![inspect(&p, None).unwrap()], ChunkMode::Fixed { count: 2, size_mb: 5 })).await;
    single_ok(&report);
    assert!(state.assembled.lock().unwrap()[0].1 == bytes);
    // 6 chunks + 4 busy + 2 broken
    assert_eq!(state.chunk_requests.load(Ordering::SeqCst), 12);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chunk_that_keeps_failing_fails_the_file_and_keeps_the_resume_record() {
    let (server, state) = fake_server().await;
    state.always_broken.store(true, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let (p, _) = data_file(dir.path(), "bad.bin", 26 * MIB, 5);
    let resume = Arc::new(ResumeStore::in_memory());
    let up = uploader(&server.uri(), resume.clone());
    let f = inspect(&p, None).unwrap();
    let key = resume::resume_key(f.size, f.modified_ms);
    let (report, _, _) = run_upload(&up, request(vec![f], ChunkMode::Fixed { count: 1, size_mb: 26 })).await;
    let Outcome::Single(FileResult { result: Err(msg), .. }) = report.outcome else { panic!("should fail") };
    assert!(msg.starts_with("Upload failed. Problem chunks: 0."), "{msg}");
    assert!(msg.contains("failed after 12 attempts"), "{msg}");
    assert_eq!(state.chunk_requests.load(Ordering::SeqCst), 13);
    let kept = resume.get(&key, now_ms()).expect("resume record kept for a later retry");
    assert_eq!(kept.completed_count, 0);
    assert!(report.stats.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_sends_only_the_missing_chunks() {
    let (server, state) = fake_server().await;
    let dir = tempfile::tempdir().unwrap();
    let (p, bytes) = data_file(dir.path(), "res.bin", 26 * MIB, 6);
    let f = inspect(&p, None).unwrap();
    let chunk = 2 * MIB;
    let total = f.size.div_ceil(chunk) as u32; // 13
    // the server already has chunks 0..5 of upload "old"
    let mut have = BTreeMap::new();
    for i in 0..5u32 {
        let s = i as u64 * chunk;
        have.insert(i, bytes[s as usize..(s + chunk) as usize].to_vec());
    }
    state.uploads.lock().unwrap().insert(
        "old".into(),
        Upload { init: json!({"filename": "res.bin", "uploadMode": "safe"}), chunks: have, total: Some(total), flagged_last: vec![] },
    );
    state.status.lock().unwrap().insert(
        "old".into(),
        json!({"active": true, "uploadId": "old", "receivedChunks": [0, 1, 2, 3, 4], "totalChunks": total}),
    );
    let resume = Arc::new(ResumeStore::in_memory());
    let key = resume::resume_key(f.size, f.modified_ms);
    resume.put(
        &key,
        ResumeRecord {
            upload_id: Some("old".into()),
            completed_count: 5,
            completed_chunks: vec![0, 1, 2, 3, 4],
            total_chunks: Some(total),
            dynamic: false,
            chunk_plan_runs: None,
            done_bytes: 5 * chunk,
            batch_id: None,
            expiration_minutes: 5,
            saved_at: now_ms(),
            name: "res.bin".into(),
            size: f.size,
            last_modified: f.modified_ms,
            path: None,
        },
    );
    let up = uploader(&server.uri(), resume.clone());
    let (report, _, _) = run_upload(&up, request(vec![f], ChunkMode::Fixed { count: 3, size_mb: 2 })).await;
    single_ok(&report);
    assert_eq!(state.chunk_requests.load(Ordering::SeqCst), (total - 5) as usize, "only the missing chunks");
    assert!(state.log.lock().unwrap().iter().all(|(_, e)| !e.starts_with("init")), "no new init");
    assert!(state.assembled.lock().unwrap()[0].1 == bytes);
    assert!(resume.list(now_ms()).is_empty(), "record cleared after success");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_resume_with_an_unknown_server_chunk_starts_over_cleanly() {
    let (server, state) = fake_server().await;
    let dir = tempfile::tempdir().unwrap();
    let (p, bytes) = data_file(dir.path(), "dyn.bin", 26 * MIB, 7);
    let f = inspect(&p, None).unwrap();
    state.uploads.lock().unwrap().insert("old".into(), Upload::default());
    // the server holds index 9, but the saved plan only knows 0..2
    state.status.lock().unwrap().insert("old".into(), json!({"active": true, "receivedChunks": [0, 9]}));
    let resume = Arc::new(ResumeStore::in_memory());
    let mut runs = plan::PlanRuns::default();
    runs.push(MIB);
    runs.push(MIB);
    resume.put(
        &resume::resume_key(f.size, f.modified_ms),
        ResumeRecord {
            upload_id: Some("old".into()),
            completed_count: 1,
            completed_chunks: vec![0],
            total_chunks: None,
            dynamic: true,
            chunk_plan_runs: Some(runs),
            done_bytes: MIB,
            batch_id: None,
            expiration_minutes: 5,
            saved_at: now_ms(),
            name: "dyn.bin".into(),
            size: f.size,
            last_modified: f.modified_ms,
            path: None,
        },
    );
    let up = uploader(&server.uri(), resume);
    let (report, _, _) = run_upload(&up, request(vec![f], ChunkMode::Auto)).await;
    single_ok(&report);
    assert!(state.log.lock().unwrap().iter().any(|(_, e)| e == "init dyn.bin"), "fresh init");
    assert_eq!(state.deleted.lock().unwrap().as_slice(), ["old"], "old remains wiped");
    let asm = state.assembled.lock().unwrap();
    assert!(asm.iter().any(|(_, b)| *b == bytes));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_kills_everything_and_wipes_the_server_chunks() {
    let (server, state) = fake_server().await;
    state.chunk_delay_ms.store(200, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let (p, _) = data_file(dir.path(), "kill.bin", 30 * MIB, 8);
    let f = inspect(&p, None).unwrap();
    let resume = Arc::new(ResumeStore::in_memory());
    let up = uploader(&server.uri(), resume.clone());
    let control = Control::default();
    let run = Run::new([(f.upload_name(), f.size)], control.clone());
    let killer = {
        let control = control.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            control.cancel();
        })
    };
    let t0 = Instant::now();
    let report = up.run(request(vec![f], ChunkMode::Fixed { count: 2, size_mb: 1 }), run.clone(), no_events()).await;
    killer.await.unwrap();
    assert!(t0.elapsed() < Duration::from_secs(3), "kill must be quick: {:?}", t0.elapsed());
    let Outcome::Single(FileResult { result: Err(msg), .. }) = report.outcome else { panic!() };
    assert_eq!(msg, "Upload killed");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(state.deleted.lock().unwrap().len(), 1, "server chunks wiped");
    assert!(resume.list(now_ms()).is_empty(), "resume record cleared");
    assert!(state.assembled.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pause_stops_at_a_chunk_boundary_and_continues() {
    let (server, state) = fake_server().await;
    state.chunk_delay_ms.store(30, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let (p, bytes) = data_file(dir.path(), "pause.bin", 26 * MIB, 9);
    let f = inspect(&p, None).unwrap();
    let up = uploader(&server.uri(), Arc::new(ResumeStore::in_memory()));
    let control = Control::default();
    let run = Run::new([(f.upload_name(), f.size)], control.clone());
    let pauser = {
        let (control, state) = (control.clone(), state.clone());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            control.set_paused(true);
            tokio::time::sleep(Duration::from_millis(150)).await; // in-flight chunks finish
            let frozen = state.chunk_requests.load(Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(400)).await;
            assert_eq!(state.chunk_requests.load(Ordering::SeqCst), frozen, "nothing new while paused");
            control.set_paused(false);
        })
    };
    let report = up.run(request(vec![f], ChunkMode::Fixed { count: 2, size_mb: 1 }), run, no_events()).await;
    pauser.await.unwrap();
    single_ok(&report);
    assert!(state.assembled.lock().unwrap()[0].1 == bytes);
    assert_eq!(state.chunk_requests.load(Ordering::SeqCst), 26);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_of_two_large_files_overlaps_and_completes_the_batch() {
    let (server, state) = fake_server().await;
    state.chunk_delay_ms.store(10, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let (pa, a) = data_file(dir.path(), "a.bin", 26 * MIB, 10);
    let (pb, b) = data_file(dir.path(), "b.bin", 26 * MIB, 11);
    let (pc, _) = data_file(dir.path(), "c.txt", 1000, 12);
    let files = vec![inspect(&pa, None).unwrap(), inspect(&pb, None).unwrap(), inspect(&pc, None).unwrap()];
    let up = uploader(&server.uri(), Arc::new(ResumeStore::in_memory()));
    let (report, run, max_req) = run_upload(&up, request(files, ChunkMode::Fixed { count: 3, size_mb: 2 })).await;
    let Outcome::Batch { files, batch } = report.outcome else { panic!("batch expected") };
    assert!(batch.is_ok());
    assert!(files.iter().all(|f| f.result.is_ok()), "{files:?}");
    assert_eq!(state.batch_completed.load(Ordering::SeqCst), 1);
    let asm = state.assembled.lock().unwrap();
    assert!(asm.iter().any(|(n, d)| n == "a.bin" && *d == a));
    assert!(asm.iter().any(|(n, d)| n == "b.bin" && *d == b));
    // every request carried the batch id
    for up in state.uploads.lock().unwrap().values() {
        assert_eq!(up.init["batchId"], "batch_1");
    }
    assert_eq!(field(&state.small.lock().unwrap()[0], "batchId").as_deref(), Some("batch_1"));
    // b started before a was assembled (pipelined), and the run never exceeded 3 connections
    let log = state.log.lock().unwrap();
    let at = |e: &str| log.iter().find(|(_, x)| x == e).map(|(t, _)| *t).unwrap();
    assert!(at("init b.bin") < at("complete a.bin"), "{log:?}");
    assert!(max_req <= 3, "{max_req}");
    assert!(run.files.iter().all(|f| f.phase() == Phase::Done));
}

/// A TCP proxy in front of the fake server that cuts the first `drop_first`
/// connections after a few KB of request body: a real dropped connection.
async fn flaky_proxy(target: String, drop_first: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let target = target.trim_start_matches("http://").to_owned();
    let dropped = Arc::new(AtomicUsize::new(0));
    tokio::spawn(async move {
        loop {
            let Ok((mut inbound, _)) = listener.accept().await else { break };
            let target = target.clone();
            let dropped = dropped.clone();
            tokio::spawn(async move {
                if dropped.fetch_add(1, Ordering::SeqCst) < drop_first {
                    let mut buf = vec![0u8; 64 * 1024];
                    let _ = inbound.read(&mut buf).await;
                    let _ = inbound.shutdown().await;
                    return;
                }
                if let Ok(mut out) = TcpStream::connect(&target).await {
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut out).await;
                }
            });
        }
    });
    format!("http://{addr}")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn survives_dropped_connections() {
    let (server, state) = fake_server().await;
    let proxy = flaky_proxy(server.uri(), 3).await;
    let dir = tempfile::tempdir().unwrap();
    let (p, bytes) = data_file(dir.path(), "drop.bin", 26 * MIB, 13);
    let f = inspect(&p, None).unwrap();
    let up = uploader(&proxy, Arc::new(ResumeStore::in_memory()));
    let (report, _, _) = run_upload(&up, request(vec![f], ChunkMode::Fixed { count: 2, size_mb: 4 })).await;
    single_ok(&report);
    assert!(state.assembled.lock().unwrap()[0].1 == bytes);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_stays_flat_bodies_are_streamed() {
    // a 26 MB file in 4 x 8 MB chunks: the engine never holds a chunk in memory,
    // only 256 KiB pieces; checked indirectly through progress, which moves before
    // a chunk is answered.
    let (server, state) = fake_server().await;
    state.chunk_delay_ms.store(300, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let (p, _) = data_file(dir.path(), "flat.bin", 26 * MIB, 14);
    let f = inspect(&p, None).unwrap();
    let up = uploader(&server.uri(), Arc::new(ResumeStore::in_memory()));
    let run = Run::new([(f.upload_name(), f.size)], Control::default());
    let seen_inflight = Arc::new(AtomicUsize::new(0));
    let w = {
        let (run, seen) = (run.clone(), seen_inflight.clone());
        tokio::spawn(async move {
            loop {
                let s = run.snapshot();
                if s.done > 0 && s.done < s.total {
                    seen.store(1, Ordering::SeqCst);
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
    };
    let report = up.run(request(vec![f], ChunkMode::Fixed { count: 2, size_mb: 8 }), run, no_events()).await;
    w.abort();
    single_ok(&report);
    assert_eq!(seen_inflight.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn small_upload_is_retried_when_the_server_did_not_take_it() {
    let (server, state) = fake_server().await;
    state.busy_small.store(2, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let (p, bytes) = data_file(dir.path(), "s.jpg", 5000, 20);
    let up = uploader(&server.uri(), Arc::new(ResumeStore::in_memory()));
    let (report, _, _) = run_upload(&up, request(vec![inspect(&p, None).unwrap()], ChunkMode::Auto)).await;
    single_ok(&report);
    let small = state.small.lock().unwrap();
    assert_eq!(small.len(), 1);
    assert_eq!(small[0][0].2, bytes);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn protected_share_small_goes_in_one_request_with_folder_paths() {
    let (server, state) = fake_server().await;
    let dir = tempfile::tempdir().unwrap();
    let top = dir.path().join("trip");
    std::fs::create_dir_all(top.join("day1")).unwrap();
    data_file(&top, "notes.txt", 100, 21);
    data_file(&top.join("day1"), "a.jpg", 2000, 22);
    let (files, rejected) = walk_folder(&top);
    assert!(rejected.is_empty());
    let up = uploader(&server.uri(), Arc::new(ResumeStore::in_memory()));
    let mut req = request(files, ChunkMode::Auto);
    req.protected = true;
    let (report, run, _) = run_upload(&up, req).await;
    let Outcome::Protected(Ok(share)) = report.outcome else { panic!("{:?}", report.outcome) };
    assert_eq!(share.files[0].tool_link.as_deref(), Some("https://x/t"));
    let direct = state.protected_direct.lock().unwrap();
    let parts = &direct[0];
    let names: Vec<&str> = parts.iter().map(|p| p.0.as_str()).collect();
    assert_eq!(names, vec!["files", "files", "relativePaths", "pairingEnabled", "expirationMinutes"]);
    assert_eq!(field(parts, "relativePaths").as_deref(), Some(r#"["trip/notes.txt","trip/day1/a.jpg"]"#));
    assert_eq!(field(parts, "pairingEnabled").as_deref(), Some("false"));
    assert!(run.files.iter().all(|f| f.phase() == Phase::Done));
    assert_eq!(run.snapshot().done, 2100);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn protected_share_large_is_chunked_per_file() {
    let (server, state) = fake_server().await;
    let dir = tempfile::tempdir().unwrap();
    let (pa, a) = data_file(dir.path(), "big.mkv", 26 * MIB + 3, 23);
    let (pb, _) = data_file(dir.path(), "empty.txt", 0, 24);
    let up = uploader(&server.uri(), Arc::new(ResumeStore::in_memory()));
    let mut req = request(vec![inspect(&pa, None).unwrap(), inspect(&pb, None).unwrap()], ChunkMode::Auto);
    req.protected = true;
    let (report, _, max_req) = run_upload(&up, req).await;
    let Outcome::Protected(Ok(share)) = report.outcome else { panic!("{:?}", report.outcome) };
    assert_eq!(share.share_id, "s2");
    let init = state.protected_init.lock().unwrap().clone().unwrap();
    assert!(init.contains("files=") && init.contains("expirationMinutes=5"), "{init}");
    let chunks = state.protected_chunks.lock().unwrap();
    // 26 MB + 3 in 4 MB chunks = 7 chunks, plus one empty chunk for the empty file
    assert_eq!(chunks.len(), 8);
    let joined: Vec<u8> = chunks.iter().filter(|((f, _), _)| *f == 0).flat_map(|(_, d)| d.clone()).collect();
    assert!(joined == a);
    assert_eq!(chunks.get(&(1, 0)).map(Vec::len), Some(0));
    assert!(max_req <= 3, "server-recommended parallelism: {max_req}");
}
