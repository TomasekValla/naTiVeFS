//! The CLI's own small remembered settings (`cli.toml` next to the app's `config.toml`).
//! The login and the Auto copy options are NOT here: they are shared with the app.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tvfs_core::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    /// Follows the app's theme setting.
    #[default]
    Auto,
    Dark,
    Light,
}

impl ThemeMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Auto => Self::Dark,
            Self::Dark => Self::Light,
            Self::Light => Self::Auto,
        }
    }

    /// Light or dark after resolving `Auto` with the app's setting.
    pub fn is_light(self, app: &Config) -> bool {
        match self {
            Self::Auto => app.light_theme,
            Self::Dark => false,
            Self::Light => true,
        }
    }
}

/// What the Upload entry of the menu opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PickerMode {
    /// The file browser inside the terminal.
    #[default]
    Terminal,
    /// The operating system's own file dialog.
    System,
}

impl PickerMode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Terminal => "terminal browser",
            Self::System => "system dialog",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::Terminal => Self::System,
            Self::System => Self::Terminal,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CliSettings {
    pub theme: ThemeMode,
    /// How the Upload entry picks files.
    pub picker: PickerMode,
    /// Last folder of the file picker.
    pub last_dir: Option<String>,
    /// Last expiry (minutes).
    pub expiry_min: Option<u32>,
}

fn path() -> Option<PathBuf> {
    Config::data_dir().ok().map(|d| d.join("cli.toml"))
}

impl CliSettings {
    pub fn load() -> Self {
        path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        if let Some(p) = path() {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(text) = toml::to_string_pretty(self) {
                // temp file + rename: a crash never leaves a half-written settings file
                let tmp = p.with_extension("toml.tmp");
                if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &p).is_err() {
                    let _ = std::fs::remove_file(&tmp);
                }
            }
        }
    }
}
