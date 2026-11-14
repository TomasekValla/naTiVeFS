//! Pieces shared by the interface and the agent mode.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tvfs_core::config::Config;
use tvfs_core::upload::{inspect, walk_folder, ChunkMode, HistoryStore, ResumeStore, UploadFile, Uploader};
use tvfs_core::{session, Client};

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Files and folders (recursively) as upload candidates; the second list holds the problems.
pub fn collect_files(paths: &[PathBuf]) -> (Vec<UploadFile>, Vec<String>) {
    let mut files: Vec<UploadFile> = Vec::new();
    let mut problems = Vec::new();
    for p in paths {
        if p.is_dir() {
            let (f, rej) = walk_folder(p);
            files.extend(f);
            problems.extend(rej.into_iter().map(|r| r.to_string()));
            continue;
        }
        match inspect(p, None) {
            Ok(f) => files.push(f),
            Err(e) => problems.push(e.to_string()),
        }
    }
    // the same file twice (e.g. a folder and one of its files) is uploaded once
    let mut seen = std::collections::HashSet::new();
    files.retain(|f| seen.insert(std::fs::canonicalize(&f.path).unwrap_or_else(|_| f.path.clone())));
    (files, problems)
}

pub fn chunk_mode(c: &Config) -> ChunkMode {
    if c.chunk_auto {
        ChunkMode::Auto
    } else {
        ChunkMode::Fixed { count: c.chunk_count.max(1), size_mb: c.chunk_size_mb.max(1) }
    }
}

/// An uploader that shares resume state and speed history with the app.
pub fn new_uploader(client: &Client) -> Uploader {
    let dir = Config::data_dir().ok();
    let resume = match &dir {
        Some(d) => ResumeStore::open(d.join("upload-resume.json")),
        None => ResumeStore::in_memory(),
    };
    let history = match &dir {
        Some(d) => HistoryStore::open(d.join("upload-stats.json")),
        None => HistoryStore::in_memory(),
    };
    Uploader::new(client.clone(), Arc::new(resume), Arc::new(history))
}

/// A client with the saved login restored (the same login the app uses).
pub fn client_with_saved_session() -> Result<Option<(Client, String)>, String> {
    let client = Client::new().map_err(|e| e.to_string())?;
    match session::load().map_err(|e| e.to_string())? {
        Some(s) => {
            client.restore_session(&s.token);
            Ok(Some((client, s.username)))
        }
        None => Ok(None),
    }
}

pub fn display_path(p: &Path) -> String {
    let s = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned()).display().to_string();
    s.strip_prefix(r"\\?\").map(str::to_owned).unwrap_or(s)
}
