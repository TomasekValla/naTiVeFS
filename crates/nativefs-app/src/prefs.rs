//! App-only preferences that the shared config (`tvfs-core`'s `Config`, also read by the command line client) has
//! no fields for yet: kept beside it in `app-prefs.json`. Missing or broken files give the defaults.

use std::path::PathBuf;

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(default)]
pub struct AppPrefs {
    /// The voice recorder's input level, 0.0 - 2.0 (0 - 200 %).
    pub voice_level: f32,
    /// The voice recorder's automatic level.
    pub voice_auto: bool,
    /// Settings > Appearance > Layout: Compact (less space around and between the cards).
    pub compact: bool,
}

impl Default for AppPrefs {
    fn default() -> Self {
        Self { voice_level: 1.0, voice_auto: false, compact: false }
    }
}

fn path() -> Option<PathBuf> {
    tvfs_core::config::Config::data_dir().ok().map(|d| d.join("app-prefs.json"))
}

pub fn load() -> AppPrefs {
    let p: AppPrefs = path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    AppPrefs { voice_level: if p.voice_level.is_finite() { p.voice_level.clamp(0.0, 2.0) } else { 1.0 }, ..p }
}

/// Change some preferences and save them.
pub fn update(f: impl FnOnce(&mut AppPrefs)) {
    let mut p = load();
    f(&mut p);
    let Some(path) = path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&path, serde_json::to_string_pretty(&p).unwrap_or_default()) {
        eprintln!("naTiVeFS: could not save {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_and_broken_files_give_defaults() {
        let p: AppPrefs = serde_json::from_str(r#"{"compact": true}"#).unwrap();
        assert_eq!(p, AppPrefs { compact: true, ..AppPrefs::default() });
        assert!(serde_json::from_str::<AppPrefs>("nonsense").is_err());
    }
}
