//! Local files picked for upload (the web's `addFiles` checks).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use super::plan::MAX_FILE_SIZE;

/// A local file in the upload queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadFile {
    pub path: PathBuf,
    /// Name without the extension (what the rename field edits).
    pub base: String,
    /// Extension including the dot, e.g. `.mp4`.
    pub ext: String,
    pub size: u64,
    pub mime: String,
    /// Modification time in ms since the epoch (part of the resume key).
    pub modified_ms: i64,
    /// Folder path inside a picked/dropped folder (`folder/sub`), empty for loose files.
    pub folder: String,
    /// Random 16-character name (`generateAnonName`) when anonymized.
    pub anon_name: Option<String>,
}

impl UploadFile {
    /// The name the server gets.
    pub fn upload_name(&self) -> String {
        self.anon_name.clone().unwrap_or_else(|| format!("{}{}", self.base, self.ext))
    }

    /// Path shown on a "TVFS Users Only" share page (`folder/sub/name.jpg`, like `webkitRelativePath`).
    pub fn share_path(&self) -> String {
        if self.folder.is_empty() {
            self.upload_name()
        } else {
            format!("{}/{}", self.folder, self.upload_name())
        }
    }

    /// Toggle the anonymize button: a fresh random name each time it is switched on.
    pub fn set_anonymized(&mut self, on: bool) {
        self.anon_name = on.then(|| anon_name(&self.ext));
    }

    /// `commitRename`: typing the same extension again is ignored; an empty name keeps the old one.
    pub fn rename(&mut self, raw: &str) {
        let raw = raw.trim();
        let (base, ext) = split_name_ext(raw);
        let name = if !ext.is_empty() && ext.eq_ignore_ascii_case(&self.ext) { base } else { raw.to_owned() };
        if !name.is_empty() {
            self.base = name;
        }
    }
}

/// Why a file was not added (texts as on the web).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejected {
    TooLarge(String),
    NoExtension(String),
    Unreadable(String, String),
}

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Rejected::TooLarge(n) => write!(f, "File \"{n}\" exceeds 10 GB limit"),
            Rejected::NoExtension(n) => write!(
                f,
                "File \"{n}\" has no extension and its type (unknown) could not be detected. Please rename it with an extension before uploading."
            ),
            Rejected::Unreadable(n, e) => write!(f, "File \"{n}\" could not be read: {e}"),
        }
    }
}

/// `splitNameExt`: `a.tar.gz` -> (`a.tar`, `.gz`); a leading or trailing dot is not an extension.
pub fn split_name_ext(name: &str) -> (String, String) {
    match name.rfind('.') {
        Some(d) if d > 0 && d < name.len() - 1 => (name[..d].to_owned(), name[d..].to_owned()),
        _ => (name.to_owned(), String::new()),
    }
}

/// `generateAnonName`: 16 random [a-zA-Z0-9] + extension.
pub fn anon_name(ext: &str) -> String {
    const C: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let s: String = (0..16).map(|_| C[fastrand::usize(..C.len())] as char).collect();
    format!("{s}{ext}")
}

/// MIME type from the extension, like a browser's `File.type` (unknown = octet-stream).
pub fn mime_for(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    // what browsers report for these (the server routes text/markdown to the MD reader)
    if lower.ends_with(".md") || lower.ends_with(".markdown") {
        return "text/markdown".into();
    }
    if lower.ends_with(".txt") {
        return "text/plain".into();
    }
    mime_guess::from_path(name).first_raw().unwrap_or("application/octet-stream").to_owned()
}

/// Check one local file and describe it for the queue.
pub fn inspect(path: &Path, folder: Option<String>) -> Result<UploadFile, Rejected> {
    let display = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
    let meta = std::fs::metadata(path).map_err(|e| Rejected::Unreadable(display.clone(), e.to_string()))?;
    if !meta.is_file() {
        return Err(Rejected::Unreadable(display, "not a file".into()));
    }
    if meta.len() > MAX_FILE_SIZE {
        return Err(Rejected::TooLarge(display));
    }
    let modified_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64);
    let (base, mut ext) = split_name_ext(&display);
    let mut mime = mime_for(&display);
    if ext.is_empty() {
        // no extension: sniff the content (the web uses the browser's MIME type here)
        let sniffed = sniff(path).ok_or_else(|| Rejected::NoExtension(display.clone()))?;
        ext = format!(".{}", sniffed.extension());
        mime = sniffed.mime_type().to_owned();
    }
    let folder = folder.unwrap_or_default();
    Ok(UploadFile { path: path.to_owned(), base, ext, size: meta.len(), mime, modified_ms, folder, anon_name: None })
}

fn sniff(path: &Path) -> Option<infer::Type> {
    let mut buf = [0u8; 8192];
    let mut f = std::fs::File::open(path).ok()?;
    let n = f.read(&mut buf).ok()?;
    infer::get(&buf[..n])
}

/// Every file below `dir` (recursively), with folders relative to `dir`'s parent
/// (`folder/sub`, like the browser's `webkitRelativePath`). Files without
/// an extension are skipped silently, like the web's folder drop.
pub fn walk_folder(dir: &Path) -> (Vec<UploadFile>, Vec<Rejected>) {
    let root = dir.parent().unwrap_or(dir);
    let mut files = Vec::new();
    let mut rejected = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
        entries.sort_by_key(|e| e.file_name());
        // depth-first, in name order
        for e in entries.iter().rev().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())) {
            stack.push(e.path());
        }
        for e in entries.iter().filter(|e| e.file_type().is_ok_and(|t| t.is_file())) {
            let p = e.path();
            let folder = d.strip_prefix(root).unwrap_or(&d).to_string_lossy().replace('\\', "/");
            match inspect(&p, Some(folder)) {
                Ok(f) => files.push(f),
                Err(Rejected::NoExtension(_)) => {}
                Err(r) => rejected.push(r),
            }
        }
    }
    (files, rejected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(split_name_ext("a.tar.gz"), ("a.tar".into(), ".gz".into()));
        assert_eq!(split_name_ext(".bashrc"), (".bashrc".into(), "".into()));
        assert_eq!(split_name_ext("x."), ("x.".into(), "".into()));
        let a = anon_name(".mp4");
        assert_eq!(a.len(), 20);
        assert!(a.ends_with(".mp4") && a[..16].chars().all(|c| c.is_ascii_alphanumeric()));
        assert_eq!(mime_for("Notes.MD"), "text/markdown");
        assert_eq!(mime_for("a.mp4"), "video/mp4");
        assert_eq!(mime_for("a.flac"), "audio/flac");
        assert_eq!(mime_for("a.unknownext"), "application/octet-stream");
    }

    #[test]
    fn inspect_rename_anonymize_and_walk() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().join("pics");
        std::fs::create_dir_all(top.join("sub")).unwrap();
        std::fs::write(top.join("a.txt"), b"hello").unwrap();
        std::fs::write(top.join("sub").join("b.png"), b"\x89PNG\r\n\x1a\n0000").unwrap();
        std::fs::write(top.join("noext"), b"plain words, no magic").unwrap();
        // a PNG without an extension is detected from its content
        std::fs::write(top.join("image"), b"\x89PNG\r\n\x1a\n0000").unwrap();

        let mut f = inspect(&top.join("a.txt"), None).unwrap();
        assert_eq!((f.base.as_str(), f.ext.as_str(), f.size, f.mime.as_str()), ("a", ".txt", 5, "text/plain"));
        f.rename("renamed.TXT");
        assert_eq!(f.upload_name(), "renamed.txt");
        f.rename("  ");
        assert_eq!(f.upload_name(), "renamed.txt");
        f.set_anonymized(true);
        assert_ne!(f.upload_name(), "renamed.txt");
        f.set_anonymized(false);
        assert_eq!(f.upload_name(), "renamed.txt");

        assert!(matches!(inspect(&top.join("noext"), None), Err(Rejected::NoExtension(_))));
        let img = inspect(&top.join("image"), None).unwrap();
        assert_eq!((img.ext.as_str(), img.mime.as_str()), (".png", "image/png"));

        let (files, rejected) = walk_folder(&top);
        assert!(rejected.is_empty());
        let rels: Vec<_> = files.iter().map(|f| f.share_path()).collect();
        assert_eq!(rels, vec!["pics/a.txt", "pics/image.png", "pics/sub/b.png"]);
    }
}
