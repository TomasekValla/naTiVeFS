//! Core library of naTiVeFS: everything that talks to a TVFS server or
//! decides what a link means. It has no UI code.

pub mod client;
pub mod config;
pub mod content;
pub mod error;
pub mod fmt;
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

pub fn base_url() -> &'static str {
    option_env!("NATIVEFS_BASE_URL").unwrap_or(DEFAULT_BASE_URL)
}

pub const APP_NAME: &str = "naTiVeFS";
/// Custom URL scheme handled by the app, e.g. `tvfs://open?url=<https link>`.
pub const URL_SCHEME: &str = "tvfs";
/// Upload expiry presets accepted by the server (minutes).
pub const EXPIRATION_OPTIONS_MIN: [u32; 12] =
    [5, 30, 60, 360, 720, 1440, 2880, 4320, 7200, 10080, 14400, 20160];
pub const DEFAULT_EXPIRATION_MIN: u32 = 10080;
