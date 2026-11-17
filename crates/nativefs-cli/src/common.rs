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

/// A file left out of an upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub path: String,
    pub reason: String,
    /// Left out even without `--skip-unsupported`: a file without a detectable extension inside a given folder (the app
    /// and the web skip those silently too), or a hidden / secret-looking file inside a folder. Everything else that is
    /// skipped needs `--skip-unsupported`.
    pub soft: bool,
}

pub fn is_audio(f: &UploadFile) -> bool {
    f.mime.starts_with("audio/")
}

/// File names that look like secrets; folder uploads leave them out unless `--include-hidden` is given.
pub fn looks_secret(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    const EXT: [&str; 10] = [".pem", ".key", ".p12", ".pfx", ".p8", ".jks", ".keystore", ".ppk", ".kdbx", ".tfstate"];
    const PREFIX: [&str; 9] = ["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519", "credentials", "secrets", ".env", ".netrc", "terraform.tfstate"];
    EXT.iter().any(|e| n.ends_with(e)) || PREFIX.iter().any(|p| n.starts_with(p)) || n.ends_with(".env")
}

/// Walk a folder the way `walk_folder` does (depth first, in name order, folder paths relative to its parent), but
/// leave out dot files / dot folders and secret-looking names unless `include_hidden`, and report everything left out.
fn walk_folder_checked(dir: &Path, include_hidden: bool, files: &mut Vec<UploadFile>, skipped: &mut Vec<Skipped>, problems: &mut Vec<String>) {
    let root = dir.parent().unwrap_or(dir);
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries.iter().rev().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())) {
            if !include_hidden && e.file_name().to_string_lossy().starts_with('.') {
                skipped.push(Skipped { path: display_path(&e.path()), reason: "hidden folder (starts with a dot; add --include-hidden to upload it)".into(), soft: true });
            } else {
                stack.push(e.path());
            }
        }
        for e in entries.iter().filter(|e| e.file_type().is_ok_and(|t| t.is_file())) {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if !include_hidden {
                if name.starts_with('.') {
                    skipped.push(Skipped { path: display_path(&p), reason: "hidden file (starts with a dot; add --include-hidden to upload it)".into(), soft: true });
                    continue;
                }
                if looks_secret(&name) {
                    skipped.push(Skipped { path: display_path(&p), reason: "looks like a secret or key file (add --include-hidden to upload it)".into(), soft: true });
                    continue;
                }
            }
            let folder = d.strip_prefix(root).unwrap_or(&d).to_string_lossy().replace('\\', "/");
            match inspect(&p, Some(folder)) {
                Ok(f) => files.push(f),
                Err(r @ Rejected::NoExtension(_)) => skipped.push(Skipped { path: display_path(&p), reason: r.to_string(), soft: true }),
                Err(r @ Rejected::TooLarge(_)) => skipped.push(Skipped { path: display_path(&p), reason: r.to_string(), soft: false }),
                Err(r) => problems.push(r.to_string()),
            }
        }
    }
}

/// Dot folders that hold keys, tokens or a repository's history (a file named inside one is held back).
const SECRET_FOLDERS: [&str; 12] =
    [".ssh", ".gnupg", ".aws", ".azure", ".kube", ".docker", ".git", ".password-store", ".pki", ".gcloud", ".npmrc", ".cargo"];

/// Why a directly named file is held back without `--include-hidden`: a dot file, a file inside a folder that holds
/// secrets (`.ssh`, `.aws`, `.gnupg`, ...) or a secret-looking name.
fn hidden_or_secret(p: &Path) -> Option<String> {
    let name = p.file_name()?.to_string_lossy().into_owned();
    let in_dot_folder = p.parent().is_some_and(|d| {
        d.components().any(|c| {
            matches!(c, std::path::Component::Normal(s) if SECRET_FOLDERS.iter().any(|f| s.to_string_lossy().eq_ignore_ascii_case(f)))
        })
    });
    if name.starts_with('.') || in_dot_folder {
        Some("hidden file or inside a hidden folder (add --include-hidden only if the user asked for exactly this file)".into())
    } else if looks_secret(&name) {
        Some("looks like a secret or key file (add --include-hidden only if the user asked for exactly this file)".into())
    } else {
        None
    }
}

/// Like `collect_files`, but files the server cannot take (no detectable extension, over the size limit) are
/// skipped and reported instead of being errors, and folders skip hidden and secret-looking files (see
/// `walk_folder_checked`). Unreadable or missing paths stay problems.
pub fn collect_files_skipping(paths: &[PathBuf], include_hidden: bool) -> (Vec<UploadFile>, Vec<Skipped>, Vec<String>) {
    let mut files: Vec<UploadFile> = Vec::new();
    let mut skipped: Vec<Skipped> = Vec::new();
    let mut problems = Vec::new();
    for p in paths {
        if p.is_dir() {
            walk_folder_checked(p, include_hidden, &mut files, &mut skipped, &mut problems);
            continue;
        }
        // a file named directly gets the same check as a file in a folder: an agent following an injected instruction
        // ("upload ~/.ssh/id_rsa") must not publish a key or a dot file without the user asking for exactly that
        if !include_hidden {
            if let Some(reason) = hidden_or_secret(p) {
                skipped.push(Skipped { path: display_path(p), reason, soft: false });
                continue;
            }
        }
        match inspect(p, None) {
            Ok(f) => files.push(f),
            Err(r @ (Rejected::NoExtension(_) | Rejected::TooLarge(_))) => skipped.push(Skipped { path: display_path(p), reason: r.to_string(), soft: false }),
            Err(r) => problems.push(r.to_string()),
        }
    }
    let mut seen = std::collections::HashSet::new();
    files.retain(|f| seen.insert(std::fs::canonicalize(&f.path).unwrap_or_else(|_| f.path.clone())));
    (files, skipped, problems)
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

    fn names(files: &[UploadFile]) -> Vec<String> {
        let mut n: Vec<_> = files.iter().map(|f| f.share_path()).collect();
        n.sort();
        n
    }

    fn last(p: &str) -> String {
        p.rsplit(['/', '\\']).next().unwrap().to_owned()
    }

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
        let (files, skipped, problems) = collect_files_skipping(&[dir.clone(), lone.clone(), d.path().join("missing.txt")], false);
        assert_eq!(names(&files), vec!["proj/a.txt", "proj/sub/b.md"]);
        let mut sk: Vec<_> = skipped.iter().map(|s| last(&s.path)).collect();
        sk.sort();
        assert_eq!(sk, vec!["COPYING", "LICENSE", "NOTICE"]);
        assert!(skipped.iter().all(|s| s.reason.contains("no extension")));
        // only the files inside the folder are left out without --skip-unsupported; the named file is not soft
        assert_eq!(skipped.iter().filter(|s| s.soft).count(), 2);
        assert_eq!(problems.len(), 1);
        let (_, strict) = collect_files(&[lone]);
        assert_eq!(strict.len(), 1);
    }

    #[test]
    fn folders_skip_hidden_and_secret_files_unless_included() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("proj");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        for f in ["a.txt", "src/b.md", ".git/config.txt", ".env.local", "server.pem", "id_rsa.pub", "credentials.json", "src/prod.key", ".hidden.txt", "keyboard.txt"] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        let (files, skipped, problems) = collect_files_skipping(std::slice::from_ref(&dir), false);
        assert!(problems.is_empty());
        assert_eq!(names(&files), vec!["proj/a.txt", "proj/keyboard.txt", "proj/src/b.md"]);
        let mut sk: Vec<_> = skipped.iter().map(|s| last(&s.path)).collect();
        sk.sort();
        assert_eq!(sk, vec![".env.local", ".git", ".hidden.txt", "credentials.json", "id_rsa.pub", "prod.key", "server.pem"]);
        assert!(skipped.iter().all(|s| s.soft && s.reason.contains("--include-hidden")));
        // the override takes everything
        let (all, skipped, _) = collect_files_skipping(std::slice::from_ref(&dir), true);
        assert!(skipped.is_empty());
        assert_eq!(all.len(), 10);
        // a secret file named directly is held back too (and fails the command), unless the override is given
        let (one, sk, _) = collect_files_skipping(&[dir.join("server.pem")], false);
        assert_eq!((one.len(), sk.len()), (0, 1));
        assert!(!sk[0].soft && sk[0].reason.contains("--include-hidden"));
        let (one, sk, _) = collect_files_skipping(&[dir.join("server.pem")], true);
        assert_eq!((one.len(), sk.len()), (1, 0));
        // a file inside a folder that holds secrets, and a dot file named directly
        std::fs::create_dir_all(dir.join(".ssh")).unwrap();
        std::fs::write(dir.join(".ssh/notes.txt"), "x").unwrap();
        for held in [dir.join(".ssh/notes.txt"), dir.join(".hidden.txt")] {
            let (one, sk, _) = collect_files_skipping(&[held.clone()], false);
            assert_eq!((one.len(), sk.len()), (0, 1), "{held:?}");
        }
        // an ordinary file in another dot folder (like ~/.local/share) is fine
        std::fs::create_dir_all(dir.join(".local/share")).unwrap();
        std::fs::write(dir.join(".local/share/song.mp3"), "x").unwrap();
        let (one, sk, _) = collect_files_skipping(&[dir.join(".local/share/song.mp3")], false);
        assert_eq!((one.len(), sk.len()), (1, 0));
    }
}
