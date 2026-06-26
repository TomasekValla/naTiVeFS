use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

const MAX_HISTORY: usize = 50;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub filename: String,
    pub link: String,
    pub styled_link: String,
    pub uploaded_at: u64, // unix millis
}

fn history_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("nativefs")
        .join("history.json")
}

pub fn load_history() -> Vec<HistoryEntry> {
    let path = history_path();
    if !path.exists() {
        return Vec::new();
    }
    let data = fs::read_to_string(&path).unwrap_or_default();
    serde_json::from_str(&data).unwrap_or_default()
}

pub fn save_history(history: &[HistoryEntry]) {
    let path = history_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(
        &path,
        serde_json::to_string_pretty(history).unwrap_or_default(),
    );
}

pub fn push_entry(history: &mut Vec<HistoryEntry>, entry: HistoryEntry) {
    history.insert(0, entry);
    history.truncate(MAX_HISTORY);
    save_history(history);
}

pub fn last_entry(history: &[HistoryEntry]) -> Option<&HistoryEntry> {
    history.first()
}
