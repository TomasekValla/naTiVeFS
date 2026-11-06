//! Small TOML config in the per-user config directory.

use std::path::PathBuf;

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// How the animated background ("screensaver" orbs, like the web) is driven.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Motion {
    /// Frozen orbs: rendered once, zero idle cost.
    Off,
    #[serde(rename = "30")]
    Fps30,
    #[serde(rename = "60")]
    Fps60,
    /// One frame per monitor refresh (capped at the refresh rate).
    #[default]
    Monitor,
}

impl Motion {
    pub const ALL: [Motion; 4] = [Motion::Off, Motion::Fps30, Motion::Fps60, Motion::Monitor];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|m| *m == self).unwrap_or(3)
    }

    pub fn from_index(i: usize) -> Self {
        Self::ALL.get(i).copied().unwrap_or_default()
    }
}

/// Which GitHub releases the updater offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    /// Full releases only.
    #[default]
    Stable,
    /// Pre-releases too (0.9.x release candidates).
    Beta,
}

/// Which link is copied to the clipboard when an upload finishes (the web's "Auto copy").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutoCopy {
    Off,
    Normal,
    /// The markdown link with metadata (web default).
    #[default]
    Styled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Command used by the link router to open non-TVFS links (e.g. `brave`, `zen-browser`). `%1` (or `%u` / `%U`)
    /// marks where the link goes; without it the link is appended.
    pub browser: Option<String>,
    /// The browser that was the default before naTiVeFS became it (detected when the router is switched on): its
    /// command, used while `browser` is empty, its name for the UI, and the system's id for it to restore it later
    /// (Windows: the registered application name, Linux: the `.desktop` file, macOS: the bundle id).
    pub router_browser: Option<String>,
    pub router_browser_name: Option<String>,
    pub router_previous_id: Option<String>,
    /// The first-run offer "Use naTiVeFS as the default browser" was answered (shown once).
    pub router_offered: bool,
    /// The user switched the router on (the OS may still need a click to finish it).
    pub router_on: bool,
    /// "Upload to TVFS" / "Open locally in naTiVeFS" in the file manager's menu.
    pub file_menus: bool,
    /// Hide the window instead of quitting when it is closed.
    pub close_to_tray: bool,
    /// Start hidden in the tray (used by autostart).
    pub start_minimized: bool,
    /// Remember the login between runs.
    pub remember_login: bool,
    /// Light theme (the web UI's `light-mode`); dark is the default.
    pub light_theme: bool,
    /// Frosted glass cards (the web's `backdrop-filter`). Off = plain translucent cards.
    pub glass: bool,
    /// Animated background.
    pub motion: Motion,
    /// Animated (smooth) mouse wheel scrolling.
    pub smooth_scroll: bool,
    pub update_channel: UpdateChannel,
    /// Check GitHub for a new version once at startup.
    pub check_updates: bool,
    /// "TVFS Users Only": uploads become login-gated shares instead of public links.
    pub tvfs_users_only: bool,
    /// Chunk configuration: "Manage automatically" (true) or "Fixed".
    pub chunk_auto: bool,
    /// Fixed mode: parallel connections.
    pub chunk_count: u32,
    /// Fixed mode: chunk size in MB.
    pub chunk_size_mb: u32,
    pub auto_copy: AutoCopy,
    /// When a reader / player page exists too, copy that instead of the file link.
    pub auto_copy_prefer_tool: bool,
    /// Images: copy the styled link instead of the plain one.
    pub auto_copy_image_styled: bool,
    /// UI zoom like a browser's (1.0 = 100 %): Ctrl + wheel, Ctrl + plus / minus, Ctrl + 0.
    pub zoom: f32,
    /// Reader font id (the web's `tvfs_reader_font`, e.g. "lora"; "default" = system font).
    pub reader_font: String,
    /// Reader text size index into 0.85 / 1 / 1.15 / 1.35 / 1.6 rem (the web's `tvfs_reader_size`).
    pub reader_size: u8,
    /// Reader light theme (the web's `tvfs_reader_theme`), independent of the app theme.
    pub reader_light: bool,
    /// Zoom of reader / viewer windows (Ctrl + wheel), like `zoom` for the main window.
    pub reader_zoom: f32,
    /// Video player volume in percent (0 - 200).
    pub player_volume: f64,
    /// Keep the pitch when the playback speed changes.
    pub player_pitch_correction: bool,
    /// Hardware video decoding when available.
    pub player_hwdec: bool,
    /// mpv's slower, sharper scalers instead of the fast ones.
    pub player_high_quality: bool,
    /// Statistics overlay detail: 0 off, 1 basic, 2 full, 3 full + graphs.
    pub player_stats: u8,
    /// Zoom of the video player's controls (Ctrl + wheel), like `zoom` for the main window.
    pub player_zoom: f32,
    /// Last camera / microphone used for capture (device names; empty = system default).
    pub camera_device: String,
    pub mic_device: String,
    /// Audio player output device id (empty = system default).
    pub audio_device: String,
    /// WASAPI exclusive / ALSA hw (true) or the system mixer (false).
    pub audio_exclusive: bool,
    /// Close the device while paused (other apps can play meanwhile).
    pub audio_release_on_pause: bool,
    /// Bit-perfect mode: every effect, the EQ, normalization and the app volume are bypassed.
    pub audio_bit_perfect: bool,
    /// Loudness normalization to -14 LUFS (the web's "Normalization" chip).
    pub audio_normalize: bool,
    /// Loop and visualizer switches (remembered like on the web).
    pub audio_loop: bool,
    pub audio_visualizer: bool,
    /// Rack volume 0-100 % (a boost above 100 % is never remembered, like the web).
    pub audio_volume: f64,
    /// The full preset list is open ("More").
    pub audio_presets_open: bool,
    /// The EQ's full preset list is open.
    pub audio_eq_presets_open: bool,
    /// Zoom of the audio player window (Ctrl + wheel), like `zoom` for the main window.
    pub audio_zoom: f32,
    /// Voice note format: 0 Opus 128, 1 AAC 96, 2 AAC 160, 3 AAC 192, 4 FLAC.
    pub voice_format: u8,
    /// The first-run tour was finished or skipped (the web's `tourDone`); the ? button replays it.
    pub audio_tour_done: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            browser: None,
            router_browser: None,
            router_browser_name: None,
            router_previous_id: None,
            router_offered: false,
            router_on: false,
            file_menus: true,
            close_to_tray: true,
            start_minimized: false,
            remember_login: true,
            light_theme: false,
            glass: true,
            motion: Motion::Monitor,
            smooth_scroll: true,
            update_channel: UpdateChannel::Stable,
            check_updates: true,
            tvfs_users_only: false,
            chunk_auto: true,
            chunk_count: 3,
            chunk_size_mb: 30,
            auto_copy: AutoCopy::Styled,
            auto_copy_prefer_tool: true,
            auto_copy_image_styled: false,
            zoom: 1.0,
            reader_font: "default".into(),
            reader_size: 1,
            reader_light: false,
            reader_zoom: 1.0,
            player_volume: 100.0,
            player_pitch_correction: true,
            player_hwdec: true,
            player_high_quality: false,
            player_stats: 2,
            player_zoom: 1.0,
            camera_device: String::new(),
            mic_device: String::new(),
            audio_device: String::new(),
            audio_exclusive: true,
            audio_release_on_pause: true,
            audio_bit_perfect: false,
            audio_normalize: true,
            audio_loop: false,
            audio_visualizer: true,
            audio_volume: 100.0,
            audio_presets_open: false,
            audio_eq_presets_open: false,
            audio_zoom: 1.0,
            audio_tour_done: false,
            voice_format: 0,
        }
    }
}

fn dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("cz", "TomasekValla", "naTiVeFS")
        .ok_or_else(|| Error::Config("no home directory".into()))
}

impl Config {
    /// `NATIVEFS_CONFIG` (debug/testing) points at another file.
    pub fn path() -> Result<PathBuf> {
        if let Some(p) = std::env::var_os("NATIVEFS_CONFIG") {
            return Ok(PathBuf::from(p));
        }
        Ok(dirs()?.config_dir().join("config.toml"))
    }

    /// Missing or unreadable file yields defaults.
    pub fn load() -> Self {
        Self::path()
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Open `url` with the configured browser command (never with the system
    /// default, which may be naTiVeFS itself when the link router is enabled).
    pub fn open_in_browser(&self, url: &str) -> Result<()> {
        let cmd = self
            .browser
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .or_else(|| self.router_browser.as_deref().map(str::trim).filter(|c| !c.is_empty()))
            .ok_or_else(|| Error::Config("no browser configured".into()))?;
        let (program, args) = browser_argv(cmd, url)?;
        std::process::Command::new(program).args(args).spawn()?;
        Ok(())
    }

    /// Directory for the app's own small data files (resume state, upload statistics),
    /// next to `config.toml`.
    pub fn data_dir() -> Result<PathBuf> {
        let p = Self::path()?;
        Ok(p.parent().map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")))
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| Error::Config(e.to_string()))?;
        std::fs::write(path, text)?;
        Ok(())
    }
}

/// Splits a browser command and puts the link where `%1` / `%u` / `%U` (Windows / desktop-entry placeholders) is, or
/// at the end. Other desktop-entry field codes (`%i`, `%c`, ...) are dropped. Refuses commands that start naTiVeFS
/// itself, so the router never hands a link back to itself in a loop.
pub fn browser_argv(cmd: &str, url: &str) -> Result<(String, Vec<String>)> {
    // Windows paths contain backslashes, which POSIX-style splitting would eat: split those by hand
    let parts = if cfg!(windows) { split_windows(&expand_env(cmd)) } else { shlex::split(cmd).unwrap_or_default() };
    let mut placed = false;
    let mut out = Vec::new();
    for p in parts {
        match p.as_str() {
            "%1" | "%u" | "%U" | "%f" | "%F" | "%l" | "%L" => {
                out.push(url.to_owned());
                placed = true;
            }
            s if s.len() == 2 && s.starts_with('%') => {}
            s if s.contains("%1") => {
                out.push(s.replace("%1", url));
                placed = true;
            }
            _ => out.push(p),
        }
    }
    if !placed {
        out.push(url.to_owned());
    }
    if out.len() < 2 {
        return Err(Error::Config("browser command is empty".into()));
    }
    let program = out.remove(0);
    if program.to_ascii_lowercase().contains("nativefs") {
        return Err(Error::Config("configured browser points at naTiVeFS itself".into()));
    }
    Ok((program, out))
}

/// `%LOCALAPPDATA%`-style variables (Windows commands); unknown names and `%1` stay as they are.
#[cfg_attr(not(windows), allow(dead_code))]
fn expand_env(cmd: &str) -> String {
    let mut out = String::new();
    let mut rest = cmd;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let name = after.find('%').map(|j| &after[..j]).filter(|n| {
            !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !n.chars().all(|c| c.is_ascii_digit())
        });
        match name.and_then(|n| std::env::var(n).ok().map(|v| (n, v))) {
            Some((n, v)) => {
                out.push_str(&v);
                rest = &after[n.len() + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Windows command-line splitting for registry commands: double quotes group, backslashes stay.
fn split_windows(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut any) = (false, false);
    for c in cmd.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_command_link_placement() {
        let u = "https://example.com/a?b=1";
        let (p, a) = browser_argv("brave", u).unwrap();
        assert_eq!((p.as_str(), a), ("brave", vec![u.to_owned()]));
        let (_, a) = browser_argv("flatpak run app.zen_browser.zen %u", u).unwrap();
        assert_eq!(a, vec!["run", "app.zen_browser.zen", u]);
        let (_, a) = browser_argv("firefox --new-window %U %i", u).unwrap();
        assert_eq!(a, vec!["--new-window", u]);
        assert!(browser_argv("/opt/nativefs/nativefs-link", u).is_err());
        assert!(browser_argv("   ", u).is_err());
        #[cfg(windows)]
        {
            let (p, a) = browser_argv(r#""C:\Program Files\Google\Chrome\Application\chrome.exe" --single-argument %1"#, u).unwrap();
            assert_eq!(p, r"C:\Program Files\Google\Chrome\Application\chrome.exe");
            assert_eq!(a, vec!["--single-argument", u]);
            let (_, a) = browser_argv(r#""C:\Program Files\Zen Browser\zen.exe" -osint -url "%1""#, u).unwrap();
            assert_eq!(a, vec!["-osint", "-url", u]);
            let local = std::env::var("LOCALAPPDATA").unwrap();
            let (p, _) = browser_argv(r#""%LOCALAPPDATA%\Vivaldi\Application\vivaldi.exe" %1"#, u).unwrap();
            assert_eq!(p, format!(r"{local}\Vivaldi\Application\vivaldi.exe"));
            assert_eq!(expand_env("50% of %NO_SUCH_VAR_X% %1"), "50% of %NO_SUCH_VAR_X% %1");
        }
    }

    #[test]
    fn partial_file_keeps_defaults() {
        let c: Config = toml::from_str("browser = \"brave\"").unwrap();
        assert_eq!(c.browser.as_deref(), Some("brave"));
        assert!(c.close_to_tray);
        assert!(!c.light_theme);
        assert!(c.glass);
        assert_eq!(c.motion, Motion::Monitor);
    }

    #[test]
    fn theme_survives_a_roundtrip() {
        let c = Config { light_theme: true, ..Config::default() };
        let text = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert!(back.light_theme);
    }

    #[test]
    fn appearance_roundtrip() {
        let c = Config { glass: false, motion: Motion::Fps30, update_channel: UpdateChannel::Beta, ..Config::default() };
        let text = toml::to_string_pretty(&c).unwrap();
        assert!(text.contains("motion = \"30\""), "{text}");
        let back: Config = toml::from_str(&text).unwrap();
        assert!(!back.glass);
        assert_eq!(back.motion, Motion::Fps30);
        assert_eq!(back.update_channel, UpdateChannel::Beta);
        assert_eq!(Motion::from_index(Motion::Fps60.index()), Motion::Fps60);
    }

    #[test]
    fn upload_settings_defaults_and_roundtrip() {
        let d = Config::default();
        assert!(d.chunk_auto && !d.tvfs_users_only && d.auto_copy_prefer_tool && !d.auto_copy_image_styled);
        assert_eq!((d.chunk_count, d.chunk_size_mb, d.auto_copy), (3, 30, AutoCopy::Styled));
        let c = Config { auto_copy: AutoCopy::Off, chunk_auto: false, chunk_count: 2, ..Config::default() };
        let back: Config = toml::from_str(&toml::to_string_pretty(&c).unwrap()).unwrap();
        assert_eq!((back.auto_copy, back.chunk_auto, back.chunk_count), (AutoCopy::Off, false, 2));
    }
}
