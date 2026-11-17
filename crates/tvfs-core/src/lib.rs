//! Core library of naTiVeFS: everything that talks to a TVFS server or
//! decides what a link means. It has no UI code.

pub mod client;
pub mod ai_help;
pub mod config;
pub mod content;
pub mod error;
pub mod fmt;
pub mod hosts;
pub mod limits;
pub mod link;
pub mod markdown;
pub mod models;
pub mod probe;
pub mod session;
pub mod update;
pub mod upload;

pub use client::Client;
pub use error::{Error, Result};

/// The TVFS server this app talks to. Hardcoded by design (TVFS is not meant
/// for third-party self-hosting). Can be overridden at *build* time with the
/// `NATIVEFS_BASE_URL` environment variable (useful for local testing).
pub const DEFAULT_BASE_URL: &str = "https://files.tomasekvalla.cz";

static BASE_URL: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// The TVFS page/API address: a build-time override (local testing), else the address the server announced last
/// time (`hosts.json`, see [`hosts::refresh`]), else [`DEFAULT_BASE_URL`]. Fixed for the life of the process.
pub fn base_url() -> &'static str {
    if let Some(b) = option_env!("NATIVEFS_BASE_URL") {
        return b;
    }
    BASE_URL.get_or_init(|| hosts::cached_page_base().unwrap_or_else(|| DEFAULT_BASE_URL.to_owned()))
}

pub const APP_NAME: &str = "naTiVeFS";
/// Custom URL scheme handled by the app, e.g. `tvfs://open?url=<https link>`.
pub const URL_SCHEME: &str = "tvfs";
/// Upload expiry presets accepted by the server (minutes).
/// Which of them an account may use comes from `GET /api/config` (see [`limits`]): tier 1 up to 7 days, tier 2 up to
/// 14, tier 3 up to 30, the host until midnight.
pub const EXPIRATION_OPTIONS_MIN: [u32; 13] =
    [5, 30, 60, 360, 720, 1440, 2880, 4320, 7200, 10080, 14400, 20160, 43200];
pub const DEFAULT_EXPIRATION_MIN: u32 = 10080;
