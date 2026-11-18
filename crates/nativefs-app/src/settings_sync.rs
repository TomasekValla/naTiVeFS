//! Settings > Uploads > Sync settings: the settings follow the account, shared with the web's settings sync. Both use
//! the web's localStorage keys and values in the `client` object of `/api/settings`, so a change on the web reaches the
//! app and the other way round. At login the copy on the server wins; later changes here are sent back. Keys the app has
//! no setting for (the web's voice clean-up) are kept as they are.

use serde_json::{Map, Value};
use tvfs_core::config::{AutoCopy, Config, Motion};

use crate::prefs::AppPrefs;

const VOICE_FORMATS: [&str; 5] = ["opus", "aac96", "aac160", "aac192", "flac"];

fn s(v: &str) -> Value {
    Value::String(v.to_owned())
}

fn flag(on: bool) -> Value {
    s(if on { "1" } else { "0" })
}

/// The app's settings in the web's form, on top of `base` (the last copy from the server, so unknown keys survive).
pub fn snapshot(cfg: &Config, prefs: &AppPrefs, base: &Map<String, Value>) -> Map<String, Value> {
    let mut m = base.clone();
    m.insert("theme".into(), s(if cfg.light_theme { "light" } else { "dark" }));
    m.insert("tvfs_glass".into(), s(if cfg.glass { "on" } else { "off" }));
    m.insert("tvfs_layout".into(), s(if prefs.compact { "compact" } else { "normal" }));
    let motion = match cfg.motion {
        Motion::Off => "0",
        Motion::Fps30 => "30",
        Motion::Fps60 => "60",
        Motion::Monitor => "full",
    };
    m.insert("tvfs_bg_motion".into(), s(motion));
    let copy = match cfg.auto_copy {
        AutoCopy::Off => "off",
        AutoCopy::Normal => "normal",
        AutoCopy::Styled => "styled",
    };
    m.insert("tvfs_autocopy_mode".into(), s(copy));
    m.insert("tvfs_autocopy_image_mode".into(), s(if cfg.auto_copy_image_styled { "styled" } else { "normal" }));
    m.insert("tvfs_autocopy_prefer_tool".into(), flag(cfg.auto_copy_prefer_tool));
    m.insert("tvfs_voice_format".into(), s(VOICE_FORMATS[(cfg.voice_format as usize).min(4)]));
    m.insert("tvfs_voice_auto".into(), flag(prefs.voice_auto));
    m.insert("tvfs_voice_gain".into(), s(&((prefs.voice_level * 100.0).round() as i32).clamp(0, 200).to_string()));
    m.insert("tvfs_tvfs_users_only_mode_enabled".into(), flag(cfg.tvfs_users_only));
    m.insert("tvfs_notify".into(), flag(cfg.notify_done));
    m
}

/// Takes the server's copy over this device's settings; returns true when something changed. Values that do not
/// make sense are ignored (the setting stays as it was).
pub fn apply(client: &Map<String, Value>, cfg: &mut Config, prefs: &mut AppPrefs) -> bool {
    let before = (cfg.clone(), *prefs);
    let get = |k: &str| client.get(k).and_then(Value::as_str);
    match get("theme") {
        Some("light") => cfg.light_theme = true,
        Some("dark") => cfg.light_theme = false,
        _ => {}
    }
    match get("tvfs_glass") {
        Some("on") => cfg.glass = true,
        Some("off") => cfg.glass = false,
        _ => {}
    }
    match get("tvfs_layout") {
        Some("compact") => prefs.compact = true,
        Some("normal") => prefs.compact = false,
        _ => {}
    }
    match get("tvfs_bg_motion") {
        Some("0") => cfg.motion = Motion::Off,
        Some("30") => cfg.motion = Motion::Fps30,
        Some("60") => cfg.motion = Motion::Fps60,
        Some("full") => cfg.motion = Motion::Monitor,
        _ => {}
    }
    match get("tvfs_autocopy_mode") {
        Some("off") => cfg.auto_copy = AutoCopy::Off,
        Some("normal") => cfg.auto_copy = AutoCopy::Normal,
        Some("styled") => cfg.auto_copy = AutoCopy::Styled,
        _ => {}
    }
    match get("tvfs_autocopy_image_mode") {
        Some("styled") => cfg.auto_copy_image_styled = true,
        Some("normal") => cfg.auto_copy_image_styled = false,
        _ => {}
    }
    if let Some(v) = get("tvfs_autocopy_prefer_tool").filter(|v| *v == "1" || *v == "0") {
        cfg.auto_copy_prefer_tool = v == "1";
    }
    if let Some(i) = get("tvfs_voice_format").and_then(|v| VOICE_FORMATS.iter().position(|f| *f == v)) {
        cfg.voice_format = i as u8;
    }
    if let Some(v) = get("tvfs_voice_auto").filter(|v| *v == "1" || *v == "0") {
        prefs.voice_auto = v == "1";
    }
    if let Some(pct) = get("tvfs_voice_gain").and_then(|v| v.parse::<i32>().ok()).filter(|p| (0..=200).contains(p)) {
        prefs.voice_level = pct as f32 / 100.0;
    }
    if let Some(v) = get("tvfs_tvfs_users_only_mode_enabled").filter(|v| *v == "1" || *v == "0") {
        cfg.tvfs_users_only = v == "1";
    }
    if let Some(v) = get("tvfs_notify").filter(|v| *v == "1" || *v == "0") {
        cfg.notify_done = v == "1";
    }
    let after = (cfg.clone(), *prefs);
    // compare the synced parts only (Config has no PartialEq)
    snapshot(&before.0, &before.1, &Map::new()) != snapshot(&after.0, &after.1, &Map::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_values_round_trip_and_unknown_keys_survive() {
        let mut cfg = Config::default();
        let mut prefs = AppPrefs::default();
        let mut web = Map::new();
        for (k, v) in [
            ("theme", "light"), ("tvfs_glass", "off"), ("tvfs_layout", "compact"), ("tvfs_bg_motion", "30"),
            ("tvfs_autocopy_mode", "normal"), ("tvfs_autocopy_image_mode", "styled"), ("tvfs_autocopy_prefer_tool", "0"),
            ("tvfs_voice_format", "aac160"), ("tvfs_voice_auto", "1"), ("tvfs_voice_gain", "150"),
            ("tvfs_tvfs_users_only_mode_enabled", "1"), ("tvfs_notify", "1"), ("tvfs_voice_cleanup", "1"),
        ] {
            web.insert(k.into(), s(v));
        }
        assert!(apply(&web, &mut cfg, &mut prefs));
        assert!(cfg.light_theme && !cfg.glass && prefs.compact && cfg.motion == Motion::Fps30);
        assert!(cfg.auto_copy == AutoCopy::Normal && cfg.auto_copy_image_styled && !cfg.auto_copy_prefer_tool);
        assert!(cfg.voice_format == 2 && prefs.voice_auto && (prefs.voice_level - 1.5).abs() < 1e-6);
        assert!(cfg.tvfs_users_only && cfg.notify_done);
        // the same values back, and the web-only key kept
        assert_eq!(snapshot(&cfg, &prefs, &web), web);
        // applying again changes nothing
        assert!(!apply(&web, &mut cfg, &mut prefs));
    }

    #[test]
    fn nonsense_values_are_ignored() {
        let mut cfg = Config::default();
        let mut prefs = AppPrefs::default();
        let mut web = Map::new();
        web.insert("theme".into(), s("purple"));
        web.insert("tvfs_voice_gain".into(), s("999"));
        web.insert("tvfs_bg_motion".into(), Value::Bool(true));
        assert!(!apply(&web, &mut cfg, &mut prefs));
    }
}
