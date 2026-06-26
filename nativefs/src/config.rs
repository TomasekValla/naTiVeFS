use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server_url: String,
    pub token: String,
    pub chunk_size_mb: u64,
    pub parallel_chunks: usize,
    pub theme: String,
    pub close_behavior: String,
    pub expiration_days: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server_url: "https://files.tomasekvalla.cz".to_string(),
            token: String::new(),
            chunk_size_mb: 30,
            parallel_chunks: 3,
            theme: "dark".to_string(),
            close_behavior: String::new(),
            expiration_days: 7,
        }
    }
}

pub fn config_path() -> PathBuf {
    let base = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("nativefs").join("config.toml")
}

pub fn load_config() -> Config {
    let path = config_path();
    if path.exists() {
        match fs::read_to_string(&path) {
            Ok(content) => toml::from_str(&content).unwrap_or_default(),
            Err(_) => Config::default(),
        }
    } else {
        let cfg = Config::default();
        let _ = save_config(&cfg);
        cfg
    }
}

pub fn save_config(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = toml::to_string_pretty(config)?;
    fs::write(&path, content)?;
    Ok(())
}
