//! `naTiVeFS-link <url>`: decides in milliseconds, without starting the GUI
//! unless it is needed. Registered as a (optional, opt-in) default browser in
//! a later stage; today it already works when invoked by hand or via `tvfs://`.
//!
//! A TVFS link the app cannot take (no answer from a running instance, or a fresh start that never comes up)
//! goes to the browser instead, so a click on a link always opens something.

use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use nativefs_ipc::Message;
use tvfs_core::{config::Config, link};

/// A running instance must handle the link within this time, or it counts as stuck.
const OPEN_TIMEOUT: Duration = Duration::from_secs(4);
/// A freshly started app must answer within this time.
const START_TIMEOUT: Duration = Duration::from_secs(20);
/// Set on everything the fallback starts: a browser command that leads back here must not loop.
const FALLBACK_ENV: &str = "NATIVEFS_LINK_FALLBACK";

/// The app next to this binary: `naTiVeFS(.exe)` as built, `nativefs` in Linux/macOS packages.
fn app_path() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    let named = |n: &str| exe.with_file_name(format!("{n}{}", std::env::consts::EXE_SUFFIX));
    let built = named("naTiVeFS");
    if built.exists() { built } else { named("nativefs") }
}

fn open_in_app(url: &str) -> Result<(), String> {
    match nativefs_ipc::send_ack(&Message::Open(url.to_owned()), OPEN_TIMEOUT) {
        Ok(_) => return Ok(()), // running instance took it
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return Err("naTiVeFS did not answer".into()),
        Err(_) => {} // no instance: start one
    }
    nativefs_ipc::allow_foreground();
    let mut child = Command::new(app_path()).arg(url).spawn().map_err(|e| format!("cannot start naTiVeFS: {e}"))?;
    // the new app opens the link itself during start-up; wait until its UI answers
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            if !status.success() {
                return Err(format!("naTiVeFS exited during start-up ({status})"));
            }
            // exited cleanly = handed the link to an instance that appeared meanwhile; the ping below finds it
        }
        if nativefs_ipc::send_ack(&Message::Ping, Duration::from_secs(2)).is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("naTiVeFS did not finish starting".into());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn open_in_browser(url: &str) -> Result<(), String> {
    nativefs_ipc::allow_foreground();
    Config::load().open_in_browser(url).map_err(|e| e.to_string())
}

/// The fallback: the configured browser, else the system's default one.
fn fallback_browser(url: &str) -> Result<(), String> {
    open_in_browser(url).or_else(|_| opener::open_browser(url).map_err(|e| format!("cannot open a browser: {e}")))
}

fn main() -> ExitCode {
    let Some(arg) = std::env::args().nth(1) else {
        nativefs_ipc::allow_foreground();
        return match Command::new(app_path()).spawn() {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("naTiVeFS-link: cannot start naTiVeFS: {e}");
                ExitCode::FAILURE
            }
        };
    };

    let result = match link::parse_open_request(&arg) {
        Some(req) => open_in_app(&arg).or_else(|why| {
            if std::env::var_os(FALLBACK_ENV).is_some() {
                return Err(format!("{why}; already a fallback, not opening the browser again"));
            }
            eprintln!("naTiVeFS-link: {why}; opening the link in the browser instead");
            // inherited by the browser process (also through the system's default-browser lookup)
            std::env::set_var(FALLBACK_ENV, "1");
            fallback_browser(req.url.as_str()) // always the https form: a tvfs:// link would come straight back here
        }),
        None => open_in_browser(&arg),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("naTiVeFS-link: {e}");
            ExitCode::FAILURE
        }
    }
}
