//! Upload statistics: the web's `UploadStatsEngine` (speed/ping samples of a
//! run, this device's average speed, and the "Upload Performance" card).

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::fmt::{fmt_1gb_time, fmt_elapsed_stats, fmt_speed_mbps, fmt_speed_mbs};

/// Average upload speed of this device over every completed upload run
/// (`tvfs_device_upload_history`: count + sum of MB/s).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceHistory {
    pub count: u32,
    #[serde(rename = "sumMB")]
    pub sum_mb: f64,
}

impl DeviceHistory {
    pub fn average_mbs(&self) -> Option<f64> {
        (self.count > 0 && self.sum_mb > 0.0).then(|| self.sum_mb / self.count as f64)
    }
}

/// `DeviceHistory` persisted as a small JSON file.
pub struct HistoryStore {
    path: Option<PathBuf>,
    value: Mutex<DeviceHistory>,
}

impl HistoryStore {
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let value = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        Self { path: Some(path), value: Mutex::new(value) }
    }

    pub fn in_memory() -> Self {
        Self::seeded(DeviceHistory::default())
    }

    /// Not persisted, starting from `value`.
    pub fn seeded(value: DeviceHistory) -> Self {
        Self { path: None, value: Mutex::new(value) }
    }

    pub fn get(&self) -> DeviceHistory {
        *self.value.lock().unwrap()
    }

    fn add(&self, avg_mbs: f64) {
        if !(avg_mbs.is_finite() && avg_mbs > 0.0) {
            return;
        }
        let mut v = self.value.lock().unwrap();
        v.count += 1;
        v.sum_mb += avg_mbs;
        if let Some(p) = &self.path {
            let _ = super::resume::write_atomic(p, &serde_json::to_vec(&*v).unwrap_or_default());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Breakdown {
    pub min: f64,
    pub med: f64,
    pub avg: f64,
    pub max: f64,
}

impl Breakdown {
    fn of(samples: &[f64]) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        let mut s = samples.to_vec();
        s.sort_by(f64::total_cmp);
        let mid = s.len() / 2;
        let med = if s.len() % 2 == 1 { s[mid] } else { (s[mid - 1] + s[mid]) / 2.0 };
        Some(Self { min: s[0], med, avg: mean(&s), max: s[s.len() - 1] })
    }
}

fn mean(s: &[f64]) -> f64 {
    s.iter().sum::<f64>() / s.len() as f64
}

/// Everything the "Upload Performance" card shows.
#[derive(Debug, Clone, PartialEq)]
pub struct StatsReport {
    /// "12.3 s"
    pub elapsed: String,
    /// "41.2 MB/s"
    pub speed_mbs: String,
    /// "329.6 Mbps"
    pub speed_mbps: String,
    /// `(badge, headline, sub)` against the device average; `None` on the first upload.
    pub comparison: Option<(ComparisonKind, String, String, String)>,
    /// "1 GB would upload in **24.9 s**" -> `(prefix, bold part)`; or "Upload completed in X".
    pub fun: (String, String),
    /// Speed min/med/avg/max (MB/s), only with >= 3 samples.
    pub speed: Option<Breakdown>,
    /// Ping min/med/avg/max (ms); with 1-2 samples only `avg` is meaningful (`single`).
    pub ping: Option<Breakdown>,
    pub ping_single: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComparisonKind {
    Faster,
    Slower,
    Same,
}

/// Samples of one upload run (`UploadStatsEngine.start()` .. `finish()`).
#[derive(Debug, Default)]
pub struct RunSamples {
    pub speed: Vec<f64>,
    pub ping: Vec<f64>,
}

impl RunSamples {
    /// MB/s average of this run's samples (`getRecentAvgSpeedMB`).
    pub fn recent_avg(&self) -> Option<f64> {
        (!self.speed.is_empty()).then(|| mean(&self.speed))
    }

    /// `finish()`: build the card and add this run to the device history.
    pub fn finish(&self, history: &HistoryStore, total_bytes: u64, elapsed_s: f64, last_ping: Option<f64>) -> StatsReport {
        let elapsed_s = if elapsed_s > 0.0 { elapsed_s } else { 0.05 };
        let elapsed = fmt_elapsed_stats(elapsed_s);
        let avg = if !self.speed.is_empty() {
            mean(&self.speed)
        } else if total_bytes > 0 {
            total_bytes as f64 / (1024.0 * 1024.0) / elapsed_s
        } else {
            0.0
        };
        let prev = history.get().average_mbs();
        let comparison = match prev {
            Some(prev) if avg > 0.0 => {
                let diff = (((avg - prev) / prev) * 100.0).round() as i64;
                let (kind, badge, headline) = match diff.signum() {
                    1 => (ComparisonKind::Faster, format!("▲ +{diff}%"), format!("{diff}% faster upload speed")),
                    -1 => (ComparisonKind::Slower, format!("▼ {diff}%"), format!("{}% lower upload speed", diff.abs())),
                    _ => (ComparisonKind::Same, format!("• {diff}%"), "Matches your usual upload speed".to_string()),
                };
                let sub = format!("Device average is {} ({})", fmt_speed_mbs(prev), fmt_speed_mbps(prev));
                Some((kind, badge, headline, sub))
            }
            _ => None,
        };
        if avg > 0.0 {
            history.add(avg);
        }
        let fun = if avg > 0.0 {
            ("1 GB would upload in ".to_string(), fmt_1gb_time(1024.0 / avg))
        } else {
            (format!("Upload completed in {elapsed}"), String::new())
        };
        let speed = if self.speed.len() >= 3 { Breakdown::of(&self.speed) } else { None };
        let mut pings = self.ping.clone();
        if pings.is_empty() {
            pings.extend(last_ping);
        }
        StatsReport {
            elapsed,
            speed_mbs: fmt_speed_mbs(avg),
            speed_mbps: fmt_speed_mbps(avg),
            comparison,
            fun,
            speed,
            ping: Breakdown::of(&pings),
            ping_single: !pings.is_empty() && pings.len() < 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_like_the_web() {
        let history = HistoryStore::in_memory();
        let run = RunSamples { speed: vec![10.0, 20.0, 30.0, 40.0], ping: vec![20.0, 40.0] };
        let r = run.finish(&history, 0, 12.34, None);
        assert_eq!(r.elapsed, "12.3 s");
        assert_eq!(r.speed_mbs, "25.0 MB/s");
        assert_eq!(r.speed_mbps, "200.0 Mbps");
        assert!(r.comparison.is_none(), "first upload has no comparison");
        assert_eq!(r.fun.1, "41 s");
        let s = r.speed.unwrap();
        assert_eq!((s.min, s.med, s.avg, s.max), (10.0, 25.0, 25.0, 40.0));
        assert!(r.ping_single);
        assert_eq!(r.ping.unwrap().avg, 30.0);
        assert_eq!(history.get(), DeviceHistory { count: 1, sum_mb: 25.0 });

        let run2 = RunSamples { speed: vec![50.0], ping: vec![] };
        let r2 = run2.finish(&history, 0, 1.0, Some(15.0));
        let (kind, badge, headline, sub) = r2.comparison.unwrap();
        assert_eq!(kind, ComparisonKind::Faster);
        assert_eq!(badge, "▲ +100%");
        assert_eq!(headline, "100% faster upload speed");
        assert_eq!(sub, "Device average is 25.0 MB/s (200.0 Mbps)");
        assert!(r2.speed.is_none());
        assert_eq!(r2.ping.unwrap().avg, 15.0);
    }

    #[test]
    fn history_file_matches_the_web_shape() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("h.json");
        let h = HistoryStore::open(&p);
        h.add(2.5);
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(text, r#"{"count":1,"sumMB":2.5}"#);
        assert_eq!(HistoryStore::open(&p).get().average_mbs(), Some(2.5));
    }
}
