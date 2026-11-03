//! Session token storage in the OS credential store (Windows Credential
//! Manager, macOS Keychain, Secret Service on Linux). The password itself is
//! never stored, only the session token the server issued.

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

pub fn save(session: &StoredSession) -> Result<()> {
    let json = serde_json::to_string(session).map_err(|e| Error::Keyring(e.to_string()))?;
    entry()?.set_password(&json).map_err(|e| Error::Keyring(e.to_string()))
}

pub fn load() -> Result<Option<StoredSession>> {
    match entry()?.get_password() {
        Ok(json) => Ok(serde_json::from_str(&json).ok()),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(Error::Keyring(e.to_string())),
    }
}

pub fn clear() -> Result<()> {
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(Error::Keyring(e.to_string())),
    }
}
