//! Chunk planning: the web client's constants and formulas (`files_web/upload.html`,
//! "CONSTANTS" and "Automatic chunk management"), plus the run-length plan used
//! to resume auto-sized uploads.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::fmt::to_fixed;

const MIB: u64 = 1024 * 1024;

/// Fixed mode: default parallel streams.
pub const DEFAULT_CHUNK_COUNT: u32 = 3;
/// Fixed mode: default chunk size (MB).
pub const DEFAULT_CHUNK_SIZE_MB: u32 = 30;
pub const MIN_CHUNK_COUNT: u32 = 1;
pub const MAX_CHUNK_COUNT: u32 = 20;
pub const MIN_CHUNK_SIZE_MB: u32 = 1;
/// Infra hard limit: chunk size or connections x size must stay below this (MB).
pub const MAX_TOTAL_MB: u32 = 100;
/// Connections x size above this (but under 100) gets a caution (MB).
pub const CAUTION_TOTAL_MB: u32 = 98;
/// Files above this are always chunked, regardless of settings.
pub const CHUNK_THRESHOLD: u64 = 25 * MIB;
/// Real (non-transient) failures one chunk may have before the upload fails.
pub const MAX_RETRIES: u32 = 12;
/// Largest file the server accepts.
pub const MAX_FILE_SIZE: u64 = 10 * 1024 * MIB;
/// A single chunk may fight a bad link this long before the upload gives up.
pub const TRANSIENT_GIVE_UP: Duration = Duration::from_secs(45 * 60);

pub const AUTO_MIN_CHUNK_BYTES: u64 = 96 * 1024;
pub const AUTO_MAX_CHUNK_BYTES: u64 = 32 * MIB;
/// At/below this -> smallest chunk.
pub const AUTO_SPEED_FLOOR_MBPS: f64 = 2.0;
/// At/above this -> largest chunk.
pub const AUTO_SPEED_CEIL_MBPS: f64 = 200.0;
pub const AUTO_MIN_CONNECTIONS: u32 = 2;

/// How chunk size and parallelism are chosen (the "Chunk configuration" card).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkMode {
    /// "Manage automatically" (default): picked from measured speed, adapting live.
    Auto,
    /// "Fixed": `count` connections x `size_mb` MB.
    Fixed { count: u32, size_mb: u32 },
}

/// Where the speed estimate behind a plan came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanSource {
    /// No data yet: the classic 3 x 30 MB.
    Default,
    /// Samples from earlier in this upload run.
    Session,
    /// This device's average over all past uploads.
    History,
    /// The running upload's own live speed.
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChunkPlan {
    pub size_bytes: u64,
    pub connections: u32,
    pub mbps: Option<f64>,
    pub source: PlanSource,
}

impl ChunkPlan {
    pub fn default_plan() -> Self {
        Self {
            size_bytes: DEFAULT_CHUNK_SIZE_MB as u64 * MIB,
            connections: DEFAULT_CHUNK_COUNT,
            mbps: None,
            source: PlanSource::Default,
        }
    }

    /// The settings card's live line, e.g. "Currently targeting 32 MB × 2 connections (250.0 Mbps, from live, this upload)."
    pub fn describe(&self) -> String {
        let label = match self.source {
            PlanSource::Live => "live, this upload",
            PlanSource::Session => "this session so far",
            PlanSource::History => "this device's past uploads",
            PlanSource::Default => "no data yet — using the default",
        };
        let speed = match self.mbps {
            Some(m) => format!("{} Mbps, from {label}", to_fixed(m, 1)),
            None => label.to_string(),
        };
        format!("Currently targeting {} × {} connections ({speed}).", fmt_chunk_bytes(self.size_bytes), self.connections)
    }
}

/// `Math.round` of JavaScript (halves round up).
fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// Upper bound of auto connections: the device's thread count, clamped to 2..=20.
pub fn auto_max_connections(threads: usize) -> u32 {
    (threads as u32).clamp(AUTO_MIN_CONNECTIONS, MAX_CHUNK_COUNT)
}

/// Logarithmic 0..1 factor between 2 and 200 Mbps.
pub fn auto_speed_factor(mbps: f64) -> f64 {
    if mbps.is_nan() || mbps <= AUTO_SPEED_FLOOR_MBPS {
        return 0.0;
    }
    if mbps >= AUTO_SPEED_CEIL_MBPS {
        return 1.0;
    }
    (mbps / AUTO_SPEED_FLOOR_MBPS).ln() / (AUTO_SPEED_CEIL_MBPS / AUTO_SPEED_FLOOR_MBPS).ln()
}

/// `computeAutoChunkPlan`: bigger chunks and fewer connections the faster the link.
pub fn compute_auto_plan(mbps: f64, source: PlanSource, max_connections: u32) -> ChunkPlan {
    let factor = auto_speed_factor(mbps);
    let span = (AUTO_MAX_CHUNK_BYTES - AUTO_MIN_CHUNK_BYTES) as f64;
    let size_bytes = js_round(AUTO_MIN_CHUNK_BYTES as f64 + span * factor) as u64;
    let max = max_connections.max(AUTO_MIN_CONNECTIONS);
    let connections = js_round(max as f64 - (max - AUTO_MIN_CONNECTIONS) as f64 * factor) as u32;
    ChunkPlan { size_bytes, connections: connections.clamp(AUTO_MIN_CONNECTIONS, max), mbps: Some(mbps), source }
}

/// `establishAutoChunkPlan`: this run's samples, else device history, else the default.
/// Speeds are in MB/s (as the web measures them).
pub fn establish_auto_plan(session_mbs: Option<f64>, history_mbs: Option<f64>, max_connections: u32) -> ChunkPlan {
    match (session_mbs.filter(|v| *v > 0.0), history_mbs.filter(|v| *v > 0.0)) {
        (Some(s), _) => compute_auto_plan(s * 8.0, PlanSource::Session, max_connections),
        (None, Some(h)) => compute_auto_plan(h * 8.0, PlanSource::History, max_connections),
        _ => ChunkPlan::default_plan(),
    }
}

/// Per-request timeout of a chunk (smaller chunks = weaker links get less time).
pub fn chunk_timeout(size_bytes: u64) -> Duration {
    if size_bytes <= 5 * MIB {
        Duration::from_secs(45)
    } else {
        Duration::from_secs(120)
    }
}

/// `uploadMode` sent to `/api/upload/init`.
pub fn upload_mode(chunk_size: u64) -> &'static str {
    if chunk_size <= 5 * MIB {
        "weak"
    } else {
        "safe"
    }
}

/// How often stats are sampled/rendered: 300 ms on fast links, up to 600 ms on slow ones.
pub fn stats_interval(speed_mbs: f64) -> Duration {
    if speed_mbs.is_nan() || speed_mbs <= 0.0 {
        return Duration::from_millis(500);
    }
    let mbps = speed_mbs * 8.0;
    if mbps >= 100.0 {
        return Duration::from_millis(300);
    }
    if mbps <= 5.0 {
        return Duration::from_millis(600);
    }
    let f = (mbps - 5.0) / 95.0;
    Duration::from_millis(js_round(600.0 - f * 300.0) as u64)
}

/// "32 MB" / "1.5 MB" / "96 KB" (`fmtChunkBytes`).
pub fn fmt_chunk_bytes(bytes: u64) -> String {
    if bytes >= MIB {
        let mb = bytes as f64 / MIB as f64;
        format!("{} MB", to_fixed(mb, if bytes.is_multiple_of(MIB) { 0 } else { 1 }))
    } else {
        format!("{} KB", js_round(bytes as f64 / 1024.0) as u64)
    }
}

/// Warning shown under the Fixed inputs (`evaluateChunkConfig`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixedWarning {
    /// The size field alone reaches 100 MB: the value is rejected.
    Prevented,
    /// connections x size >= 100 MB.
    Warn,
    /// connections x size >= 98 MB.
    Caution,
}

impl FixedWarning {
    pub fn text(self) -> &'static str {
        match self {
            FixedWarning::Prevented => "Prevented: Our infrastructure doesn't allow simultaneous uploading of more than 100 MB. Try putting in a lower chunk size for uploads to work.",
            FixedWarning::Warn => "Warning: Errors may occur when the intermediate total exceeds 100 MB. Try lowering the numbers for more optimal results.",
            FixedWarning::Caution => "Caution: The size of chunk in MB is not the final size of request. Overhead containing some additional info is sent with the chunk too and you're risking transfering more than 100 MB at once, which could result in bugs. We strongly advise you to lower the number slightly just in case.",
        }
    }
}

/// `count`/`size_mb` as typed (None = empty or not a number).
pub fn evaluate_fixed(count: Option<u32>, size_mb: Option<u32>) -> Option<FixedWarning> {
    let size = size_mb.filter(|s| *s >= 1)?;
    if size >= MAX_TOTAL_MB {
        return Some(FixedWarning::Prevented);
    }
    let count = count.filter(|c| *c >= 1)?;
    let total = count * size;
    if total >= MAX_TOTAL_MB {
        Some(FixedWarning::Warn)
    } else if total >= CAUTION_TOTAL_MB {
        Some(FixedWarning::Caution)
    } else {
        None
    }
}

/// `commitChunkConfig`: clamp what was typed, falling back to the previous values.
pub fn commit_fixed(count: Option<u32>, size_mb: Option<u32>, prev_count: u32, prev_size: u32) -> (u32, u32) {
    let count = count.map_or(prev_count, |c| c.clamp(MIN_CHUNK_COUNT, MAX_CHUNK_COUNT));
    let size = size_mb.map_or(prev_size, |s| s.clamp(MIN_CHUNK_SIZE_MB, MAX_TOTAL_MB - 1));
    (count, size)
}

/// Run-length encoded byte plan of an auto-sized upload: `[[size, count], ...]` in index order.
/// Claims are strictly back to back, so this is the complete byte plan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PlanRuns(pub Vec<(u64, u32)>);

impl PlanRuns {
    pub fn push(&mut self, size: u64) {
        match self.0.last_mut() {
            Some((s, n)) if *s == size => *n += 1,
            _ => self.0.push((size, 1)),
        }
    }

    /// Byte ranges by chunk index, or `None` when the plan cannot belong to a file of `file_size`.
    pub fn decode(&self, file_size: u64) -> Option<Vec<(u64, u64)>> {
        let mut out = Vec::new();
        let mut pos = 0u64;
        for &(size, n) in &self.0 {
            for _ in 0..n {
                out.push((pos, pos + size));
                pos += size;
            }
        }
        (pos <= file_size).then_some(out)
    }

    pub fn from_ranges(ranges: &[(u64, u64)]) -> Self {
        let mut runs = PlanRuns::default();
        for (s, e) in ranges {
            runs.push(e - s);
        }
        runs
    }
}

/// Byte ranges of a fixed-size upload.
pub fn fixed_ranges(file_size: u64, chunk_size: u64) -> Vec<(u64, u64)> {
    let chunk_size = chunk_size.max(1);
    let n = file_size.div_ceil(chunk_size);
    (0..n).map(|i| (i * chunk_size, ((i + 1) * chunk_size).min(file_size))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_plan_matches_the_web_formula() {
        // floor / ceiling
        let slow = compute_auto_plan(1.0, PlanSource::Live, 16);
        assert_eq!((slow.size_bytes, slow.connections), (96 * 1024, 16));
        let fast = compute_auto_plan(500.0, PlanSource::Live, 16);
        assert_eq!((fast.size_bytes, fast.connections), (32 * MIB, 2));
        // 20 Mbps: factor = ln(10)/ln(100) = 0.5
        let mid = compute_auto_plan(20.0, PlanSource::Live, 16);
        assert!((auto_speed_factor(20.0) - 0.5).abs() < 1e-12);
        let expected = (96.0 * 1024.0 + (32.0 * MIB as f64 - 96.0 * 1024.0) * auto_speed_factor(20.0) + 0.5).floor() as u64;
        assert_eq!(mid.size_bytes, expected);
        assert!(mid.size_bytes.abs_diff(16 * MIB + 48 * 1024) <= 1);
        assert_eq!(mid.connections, 9); // round(16 - 14 * 0.5)
        assert_eq!(auto_max_connections(64), 20);
        assert_eq!(auto_max_connections(1), 2);
    }

    #[test]
    fn plan_source_order() {
        assert_eq!(establish_auto_plan(None, None, 8), ChunkPlan::default_plan());
        assert_eq!(establish_auto_plan(None, Some(3.0), 8).source, PlanSource::History);
        assert_eq!(establish_auto_plan(Some(1.0), Some(3.0), 8).source, PlanSource::Session);
        let p = establish_auto_plan(Some(1.0), None, 8);
        assert_eq!(p.mbps, Some(8.0));
    }

    #[test]
    fn timeouts_modes_and_intervals() {
        assert_eq!(chunk_timeout(5 * MIB), Duration::from_secs(45));
        assert_eq!(chunk_timeout(5 * MIB + 1), Duration::from_secs(120));
        assert_eq!(upload_mode(96 * 1024), "weak");
        assert_eq!(upload_mode(30 * MIB), "safe");
        assert_eq!(stats_interval(0.0), Duration::from_millis(500));
        assert_eq!(stats_interval(20.0), Duration::from_millis(300));
        assert_eq!(stats_interval(0.5), Duration::from_millis(600));
    }

    #[test]
    fn fixed_warnings() {
        assert_eq!(evaluate_fixed(Some(3), Some(30)), None);
        assert_eq!(evaluate_fixed(Some(1), Some(100)), Some(FixedWarning::Prevented));
        assert_eq!(evaluate_fixed(None, Some(100)), Some(FixedWarning::Prevented));
        assert_eq!(evaluate_fixed(Some(2), Some(50)), Some(FixedWarning::Warn));
        assert_eq!(evaluate_fixed(Some(2), Some(49)), Some(FixedWarning::Caution));
        assert_eq!(evaluate_fixed(None, Some(30)), None);
        assert_eq!(commit_fixed(Some(50), Some(150), 3, 30), (20, 99));
        assert_eq!(commit_fixed(None, None, 4, 10), (4, 10));
    }

    #[test]
    fn runs_roundtrip() {
        let mut runs = PlanRuns::default();
        for s in [10, 10, 10, 4, 4, 7] {
            runs.push(s);
        }
        assert_eq!(runs.0, vec![(10, 3), (4, 2), (7, 1)]);
        let ranges = runs.decode(45).unwrap();
        assert_eq!(ranges.len(), 6);
        assert_eq!(ranges[3], (30, 34));
        assert_eq!(ranges[5], (38, 45));
        assert!(runs.decode(44).is_none());
        assert_eq!(PlanRuns::from_ranges(&ranges), runs);
        let json = serde_json::to_string(&runs).unwrap();
        assert_eq!(json, "[[10,3],[4,2],[7,1]]");
    }

    #[test]
    fn fixed_ranges_cover_the_file() {
        assert_eq!(fixed_ranges(10, 4), vec![(0, 4), (4, 8), (8, 10)]);
        assert!(fixed_ranges(0, 4).is_empty());
        assert_eq!(fmt_chunk_bytes(32 * MIB), "32 MB");
        assert_eq!(fmt_chunk_bytes(96 * 1024), "96 KB");
        assert_eq!(fmt_chunk_bytes(MIB + MIB / 2), "1.5 MB");
    }
}
