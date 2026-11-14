//! Session token storage in the OS credential store (Windows Credential
//! Manager, macOS Keychain, Secret Service on Linux). The password itself is
//! never stored, only the session token the server issued.
//!
//! Testing (debug builds only): when `NATIVEFS_CONFIG` points at a throw-away config file, the session lives in a
//! `session.json` next to it instead, so a test run never touches the real login. Release builds always use the
//! credential store: a session token never lands in a plain file there.

use std::path::PathBuf;

use keyring::Entry;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

const SERVICE: &str = "naTiVeFS";
const ACCOUNT: &str = "session";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSession {
    pub username: String,
    pub token: String,
}

fn entry() -> Result<Entry> {
    Entry::new(SERVICE, ACCOUNT).map_err(|e| Error::Keyring(e.to_string()))
}

/// The test-mode session file (debug builds, only when `NATIVEFS_CONFIG` is set).
fn test_file() -> Option<PathBuf> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let cfg = PathBuf::from(std::env::var_os("NATIVEFS_CONFIG")?);
    Some(cfg.parent().map(PathBuf::from).unwrap_or_default().join("session.json"))
}

pub fn save(session: &StoredSession) -> Result<()> {
    let json = serde_json::to_string(session).map_err(|e| Error::Keyring(e.to_string()))?;
    if let Some(p) = test_file() {
        return Ok(std::fs::write(p, json)?);
    }
    entry()?.set_password(&json).map_err(|e| Error::Keyring(e.to_string()))
}

pub fn load() -> Result<Option<StoredSession>> {
    if let Some(p) = test_file() {
        return Ok(std::fs::read_to_string(p).ok().and_then(|j| serde_json::from_str(&j).ok()));
    }
    match entry()?.get_password() {
        Ok(json) => Ok(serde_json::from_str(&json).ok()),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(Error::Keyring(e.to_string())),
    }
}

pub fn clear() -> Result<()> {
    if let Some(p) = test_file() {
        let _ = std::fs::remove_file(p);
        return Ok(());
    }
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(Error::Keyring(e.to_string())),
    }
}
