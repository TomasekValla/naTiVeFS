//! Resume records (the web keeps them in `localStorage` as `tvfs_resume_*`).
//! One JSON file holds all of them; the server's `/api/upload/status` stays the
//! source of truth for which chunks actually landed.
//!
//! Keyed by size + modification time, not by name: anonymizing or renaming a
//! file in the queue must not lose its resume state.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::plan::PlanRuns;

/// Records (and the server's chunks) live this long.
pub const RESUME_TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// Completed indices are only stored while the list is small (the server knows them anyway).
const MAX_STORED_INDICES: usize = 1500;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeRecord {
    /// `None` for small single-request uploads (a "started" marker only).
    pub upload_id: Option<String>,
    pub completed_count: u32,
    #[serde(default)]
    pub completed_chunks: Vec<u32>,
    pub total_chunks: Option<u32>,
    /// Auto-sized chunks were used (then `chunk_plan_runs` holds the byte plan).
    pub dynamic: bool,
    #[serde(default)]
    pub chunk_plan_runs: Option<PlanRuns>,
    #[serde(default)]
    pub done_bytes: u64,
    #[serde(default)]
    pub batch_id: Option<String>,
    pub expiration_minutes: u32,
    pub saved_at: i64,
    pub name: String,
    pub size: u64,
    pub last_modified: i64,
    /// Native only: where the file is, so it can be resumed without picking it again.
    #[serde(default)]
    pub path: Option<PathBuf>,
}

impl ResumeRecord {
    /// Percentage shown on the NOT FINISHED card.
    pub fn percent(&self) -> u32 {
        if self.dynamic && self.size > 0 {
            return ((self.done_bytes as f64 / self.size as f64) * 100.0).round().min(100.0) as u32;
        }
        match self.total_chunks {
            Some(t) if t > 0 => ((self.completed_count as f64 / t as f64) * 100.0).round() as u32,
            _ => 0,
        }
    }

    pub fn expires_at(&self) -> i64 {
        self.saved_at + RESUME_TTL_MS
    }

    pub fn set_completed(&mut self, completed: &[u32]) {
        self.completed_count = completed.len() as u32;
        self.completed_chunks = if completed.len() <= MAX_STORED_INDICES { completed.to_vec() } else { Vec::new() };
    }
}

pub fn resume_key(size: u64, last_modified_ms: i64) -> String {
    format!("{size}|{last_modified_ms}")
}

/// All resume records, persisted to one file.
pub struct ResumeStore {
    path: Option<PathBuf>,
    map: Mutex<BTreeMap<String, ResumeRecord>>,
}

impl ResumeStore {
    /// Load from `path` (missing or broken file = empty).
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let map = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<BTreeMap<String, ResumeRecord>>(&s).ok())
            .unwrap_or_default();
        Self { path: Some(path), map: Mutex::new(map) }
    }

    /// Not persisted (tests).
    pub fn in_memory() -> Self {
        Self { path: None, map: Mutex::new(BTreeMap::new()) }
    }

    /// The record for `key`, if not older than 24 h (stale ones are dropped).
    pub fn get(&self, key: &str, now_ms: i64) -> Option<ResumeRecord> {
        let mut map = self.map.lock().unwrap();
        let rec = map.get(key)?.clone();
        if now_ms - rec.saved_at > RESUME_TTL_MS {
            map.remove(key);
            self.persist(&map);
            return None;
        }
        Some(rec)
    }

    pub fn put(&self, key: &str, rec: ResumeRecord) {
        let mut map = self.map.lock().unwrap();
        map.insert(key.to_owned(), rec);
        self.persist(&map);
    }

    pub fn remove(&self, key: &str) {
        let mut map = self.map.lock().unwrap();
        if map.remove(key).is_some() {
            self.persist(&map);
        }
    }

    /// Every record younger than 24 h, newest first (stale ones are dropped).
    pub fn list(&self, now_ms: i64) -> Vec<(String, ResumeRecord)> {
        let mut map = self.map.lock().unwrap();
        let before = map.len();
        map.retain(|_, r| now_ms - r.saved_at <= RESUME_TTL_MS);
        if map.len() != before {
            self.persist(&map);
        }
        let mut out: Vec<_> = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.1.saved_at));
        out
    }

    fn persist(&self, map: &BTreeMap<String, ResumeRecord>) {
        let Some(path) = &self.path else { return };
        if let Err(e) = write_atomic(path, &serde_json::to_vec(map).unwrap_or_default()) {
            eprintln!("naTiVeFS: could not save upload resume state: {e}");
        }
    }
}

/// Write via a temporary file + rename, so a crash never leaves half a file.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(saved_at: i64) -> ResumeRecord {
        ResumeRecord {
            upload_id: Some("u1".into()),
            completed_count: 0,
            completed_chunks: vec![],
            total_chunks: Some(4),
            dynamic: false,
            chunk_plan_runs: None,
            done_bytes: 0,
            batch_id: None,
            expiration_minutes: 5,
            saved_at,
            name: "a.bin".into(),
            size: 100,
            last_modified: 7,
            path: None,
        }
    }

    #[test]
    fn persists_and_expires() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("resume.json");
        let store = ResumeStore::open(&path);
        let mut r = rec(1_000);
        r.set_completed(&[0, 1]);
        store.put(&resume_key(100, 7), r.clone());
        assert_eq!(r.percent(), 50);

        let reopened = ResumeStore::open(&path);
        assert_eq!(reopened.get("100|7", 2_000), Some(r));
        assert_eq!(reopened.list(2_000).len(), 1);
        // older than 24 h -> gone, also from disk
        assert!(reopened.get("100|7", 1_000 + RESUME_TTL_MS + 1).is_none());
        assert!(ResumeStore::open(&path).list(0).is_empty());
    }

    #[test]
    fn dynamic_percent_uses_bytes_and_large_lists_are_not_stored() {
        let mut r = rec(0);
        r.dynamic = true;
        r.done_bytes = 33;
        assert_eq!(r.percent(), 33);
        let many: Vec<u32> = (0..2000).collect();
        r.set_completed(&many);
        assert_eq!(r.completed_count, 2000);
        assert!(r.completed_chunks.is_empty());
    }

    #[test]
    fn web_record_shape_parses() {
        // a record as the web writes it (camelCase, runs as nested arrays)
        let json = r#"{"uploadId":"1_x","completedCount":2,"completedChunks":[0,1],"totalChunks":null,"dynamic":true,
            "chunkPlanRuns":[[98304,3]],"doneBytes":196608,"batchId":null,"expirationMinutes":10080,"savedAt":5,
            "name":"v.mkv","size":300000,"lastModified":9}"#;
        let r: ResumeRecord = serde_json::from_str(json).unwrap();
        assert_eq!(r.chunk_plan_runs.unwrap().0, vec![(98304, 3)]);
        assert!(r.path.is_none());
    }
}
