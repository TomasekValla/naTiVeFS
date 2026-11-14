//! One-time confirmation codes for the AI agent mode.
//!
//! Step 1 of an agent upload issues a code bound to a hash of the exact file list (paths, sizes,
//! modification times) and the options. Step 2 must present that code: it is single use, valid for
//! 10 minutes, and refused when anything it was bound to changed. Codes live in the app's data dir.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tvfs_core::upload::UploadFile;

pub const VALID_MS: i64 = 10 * 60 * 1000;
const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const MAX_ENTRIES: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Entry {
    code: String,
    binding: String,
    expires_ms: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConfirmError {
    /// No such code (typo, already used, or never issued).
    Unknown,
    Expired,
    /// The code is valid but was issued for other files or options.
    Mismatch,
}

impl std::fmt::Display for ConfirmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown => write!(f, "Confirmation code not recognised (wrong, or already used). Nothing was uploaded. Run step 1 again to get a new code."),
            Self::Expired => write!(f, "Confirmation code expired (codes are valid for 10 minutes). Nothing was uploaded. Run step 1 again to get a new code."),
            Self::Mismatch => write!(f, "The files or options changed since this code was issued. Nothing was uploaded. Run step 1 again to get a new code."),
        }
    }
}

fn store_path(dir: &Path) -> PathBuf {
    dir.join("cli-confirm.json")
}

fn load(dir: &Path) -> Vec<Entry> {
    std::fs::read_to_string(store_path(dir)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save(dir: &Path, entries: &[Entry]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(store_path(dir), serde_json::to_string(entries).unwrap_or_else(|_| "[]".into()))
}

/// Hash of the exact file list and options a code is bound to.
pub fn binding(files: &[UploadFile], expire_min: u32, protected: bool) -> String {
    let mut data = String::new();
    for f in files {
        data.push_str(&format!("{}\u{1}{}\u{1}{}\u{1}{}\n", f.path.display(), f.size, f.modified_ms, f.upload_name()));
    }
    data.push_str(&format!("expire={expire_min};protected={protected}"));
    sha256_hex(data.as_bytes())
}

fn normalize(code: &str) -> String {
    code.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_uppercase()
}

/// Issue a code for `binding`; an older code for the same binding is replaced.
pub fn issue(dir: &Path, binding: &str, now_ms: i64) -> std::io::Result<String> {
    let mut entries: Vec<Entry> = load(dir).into_iter().filter(|e| e.expires_ms > now_ms && e.binding != binding).collect();
    let raw: String = (0..8).map(|_| ALPHABET[fastrand::usize(..ALPHABET.len())] as char).collect();
    let code = format!("{}-{}", &raw[..4], &raw[4..]);
    entries.push(Entry { code: code.clone(), binding: binding.to_owned(), expires_ms: now_ms + VALID_MS });
    if entries.len() > MAX_ENTRIES {
        entries.remove(0);
    }
    save(dir, &entries)?;
    Ok(code)
}

/// Check and use up a code.
pub fn consume(dir: &Path, code: &str, binding: &str, now_ms: i64) -> Result<(), ConfirmError> {
    let wanted = normalize(code);
    let mut entries = load(dir);
    let Some(pos) = entries.iter().position(|e| normalize(&e.code) == wanted) else { return Err(ConfirmError::Unknown) };
    if entries[pos].expires_ms <= now_ms {
        entries.remove(pos);
        let _ = save(dir, &entries);
        return Err(ConfirmError::Expired);
    }
    if entries[pos].binding != binding {
        return Err(ConfirmError::Mismatch);
    }
    entries.remove(pos);
    let _ = save(dir, &entries);
    Ok(())
}

// ── SHA-256 (small, self-contained: no extra dependency for one hash) ──────────────────────────

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be,
    0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa,
    0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85,
    0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
    0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f,
    0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

pub fn sha256_hex(data: &[u8]) -> String {
    let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, c) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, size: u64, mtime: i64) -> UploadFile {
        UploadFile {
            path: PathBuf::from(name),
            base: name.trim_end_matches(".txt").into(),
            ext: ".txt".into(),
            size,
            mime: "text/plain".into(),
            modified_ms: mtime,
            folder: String::new(),
            anon_name: None,
            voice_note: false,
        }
    }

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn code_is_single_use_and_bound() {
        let dir = tempfile::tempdir().unwrap();
        let b = binding(&[file("a.txt", 5, 1)], 1440, false);
        let code = issue(dir.path(), &b, 1000).unwrap();
        assert_eq!(code.len(), 9);
        let other = binding(&[file("a.txt", 6, 1)], 1440, false);
        assert_eq!(consume(dir.path(), &code, &other, 2000), Err(ConfirmError::Mismatch));
        // the lowercase / dash-less form works, and only once
        assert_eq!(consume(dir.path(), &code.to_lowercase().replace('-', ""), &b, 2000), Ok(()));
        assert_eq!(consume(dir.path(), &code, &b, 2000), Err(ConfirmError::Unknown));
    }

    #[test]
    fn codes_expire_after_ten_minutes() {
        let dir = tempfile::tempdir().unwrap();
        let b = binding(&[file("a.txt", 5, 1)], 60, false);
        let code = issue(dir.path(), &b, 0).unwrap();
        assert_eq!(consume(dir.path(), &code, &b, VALID_MS + 1), Err(ConfirmError::Expired));
        assert_eq!(consume(dir.path(), &code, &b, 1), Err(ConfirmError::Unknown));
        let code = issue(dir.path(), &b, 0).unwrap();
        assert_eq!(consume(dir.path(), &code, &b, VALID_MS - 1), Ok(()));
    }

    #[test]
    fn binding_covers_files_mtime_and_options() {
        let a = binding(&[file("a.txt", 5, 1)], 60, false);
        assert_ne!(a, binding(&[file("a.txt", 5, 2)], 60, false));
        assert_ne!(a, binding(&[file("a.txt", 5, 1)], 120, false));
        assert_ne!(a, binding(&[file("a.txt", 5, 1)], 60, true));
        assert_ne!(a, binding(&[file("b.txt", 5, 1)], 60, false));
        assert_eq!(a, binding(&[file("a.txt", 5, 1)], 60, false));
    }

    #[test]
    fn wrong_code_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(consume(dir.path(), "ZZZZ-ZZZZ", "x", 0), Err(ConfirmError::Unknown));
    }
}
