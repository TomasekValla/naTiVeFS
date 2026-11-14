//! Pieces shared by the interface and the agent mode.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tvfs_core::config::Config;
use tvfs_core::upload::{inspect, walk_folder, ChunkMode, HistoryStore, Rejected, ResumeStore, UploadFile, Uploader};
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

/// A file left out of an upload by `--skip-unsupported`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub path: String,
    pub reason: String,
    /// Left out because it has no detectable extension and sat inside a given folder (the app and the web skip those
    /// silently too); everything else that is skipped needs `--skip-unsupported`.
    pub in_folder_no_ext: bool,
}

/// Like `collect_files`, but files the server cannot take (no detectable extension, over the size limit) are
/// skipped and reported instead of being errors. Unreadable or missing paths stay problems.
pub fn collect_files_skipping(paths: &[PathBuf]) -> (Vec<UploadFile>, Vec<Skipped>, Vec<String>) {
    let mut files: Vec<UploadFile> = Vec::new();
    let mut skipped: Vec<Skipped> = Vec::new();
    let mut problems = Vec::new();
    let judge = |p: &Path, r: Rejected, in_folder: bool, skipped: &mut Vec<Skipped>, problems: &mut Vec<String>| match r {
        Rejected::NoExtension(_) | Rejected::TooLarge(_) => {
            let in_folder_no_ext = in_folder && matches!(r, Rejected::NoExtension(_));
            skipped.push(Skipped { path: display_path(p), reason: r.to_string(), in_folder_no_ext })
        }
        Rejected::Unreadable(..) => problems.push(r.to_string()),
    };
    for p in paths {
        if p.is_dir() {
            let (f, _) = walk_folder(p);
            // `walk_folder` drops what it rejects (without a trace for files lacking an extension): find those again
            let kept: std::collections::HashSet<PathBuf> = f.iter().map(|x| x.path.clone()).collect();
            files.extend(f);
            let mut all = Vec::new();
            list_files(p, &mut all);
            all.sort();
            for q in all.into_iter().filter(|q| !kept.contains(q)) {
                if let Err(r) = inspect(&q, None) {
                    judge(&q, r, true, &mut skipped, &mut problems);
                }
            }
            continue;
        }
        match inspect(p, None) {
            Ok(f) => files.push(f),
            Err(r) => judge(p, r, false, &mut skipped, &mut problems),
        }
    }
    let mut seen = std::collections::HashSet::new();
    files.retain(|f| seen.insert(std::fs::canonicalize(&f.path).unwrap_or_else(|_| f.path.clone())));
    (files, skipped, problems)
}

fn list_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.filter_map(Result::ok) {
        match e.file_type() {
            Ok(t) if t.is_dir() => list_files(&e.path(), out),
            Ok(t) if t.is_file() => out.push(e.path()),
            _ => {}
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_unsupported_lists_what_it_leaves_out() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("proj");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        std::fs::write(dir.join("sub").join("b.md"), "x").unwrap();
        std::fs::write(dir.join("LICENSE"), "plain text without extension").unwrap();
        std::fs::write(dir.join("sub").join("NOTICE"), "more").unwrap();
        let lone = d.path().join("COPYING");
        std::fs::write(&lone, "text").unwrap();
        let (files, skipped, problems) = collect_files_skipping(&[dir.clone(), lone.clone(), d.path().join("missing.txt")]);
        let mut names: Vec<_> = files.iter().map(|f| f.upload_name()).collect();
        names.sort();
        assert_eq!(names, vec!["a.txt", "b.md"]);
        let mut sk: Vec<_> = skipped.iter().map(|s| s.path.rsplit(['/', '\\']).next().unwrap().to_owned()).collect();
        sk.sort();
        assert_eq!(sk, vec!["COPYING", "LICENSE", "NOTICE"]);
        assert!(skipped.iter().all(|s| s.reason.contains("no extension")));
        // a missing path is still an error, not a silent skip
        assert_eq!(problems.len(), 1);
        // strict collection keeps its behaviour: a loose file without extension is a problem
        let (_, strict) = collect_files(&[lone]);
        assert_eq!(strict.len(), 1);
    }
}
