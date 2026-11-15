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
/// Set on everything the fallback starts: a browser command that leads back here must not loop. It holds the time
/// it was set: a browser started by the fallback keeps it for the rest of its life (and passes it to every later
/// `tvfs://` launch), so it only counts for a short while.
const FALLBACK_ENV: &str = "NATIVEFS_LINK_FALLBACK";
const FALLBACK_WINDOW: Duration = Duration::from_secs(30);

fn unix_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// This process was started by a fallback a moment ago (a browser that hands the link straight back).
fn fallback_just_happened(marker: Option<&str>, now: u64) -> bool {
    marker.and_then(|v| v.trim().parse::<u64>().ok()).is_some_and(|at| now.saturating_sub(at) < FALLBACK_WINDOW.as_secs())
}

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
    let mut child = Command::new(app_path()).arg(url).spawn().map_err(|e| format!("cannot start naTiVeFS: {e}"))?;
    nativefs_ipc::allow_foreground_for(child.id());
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

/// The configured browser (Settings > Links), else the one the router replaced as the default.
fn open_in_browser(url: &str) -> Result<(), String> {
    let cfg = Config::load();
    let cmd = [&cfg.browser, &cfg.router_browser]
        .into_iter()
        .filter_map(|c| c.as_deref().map(str::trim).filter(|c| !c.is_empty()))
        .next()
        .ok_or("no browser configured")?;
    let (program, args) = tvfs_core::config::browser_argv(cmd, url).map_err(|e| e.to_string())?;
    let child = Command::new(program).args(args).spawn().map_err(|e| e.to_string())?;
    // the browser may come to the front (it got the link from the foreground process)
    nativefs_ipc::allow_foreground_for(child.id());
    Ok(())
}

/// The https form of a TVFS link for the browser, marked so the web page does not send it straight back to the app.
fn browser_link(mut url: url::Url) -> String {
    if url.scheme() == "http" {
        let _ = url.set_scheme("https");
    }
    nativefs_ipc::in_browser_url(url.as_str())
}

/// The fallback: the configured browser, else the system's default one.
fn fallback_browser(url: &str) -> Result<(), String> {
    open_in_browser(url).or_else(|_| opener::open_browser(url).map_err(|e| format!("cannot open a browser: {e}")))
}

fn main() -> ExitCode {
    let Some(arg) = std::env::args().nth(1) else {
        return match Command::new(app_path()).spawn() {
            Ok(child) => {
                nativefs_ipc::allow_foreground_for(child.id());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("naTiVeFS-link: cannot start naTiVeFS: {e}");
                ExitCode::FAILURE
            }
        };
    };

    let result = match link::parse_open_request(&arg) {
        Some(req) => open_in_app(&arg).or_else(|why| {
            let now = unix_secs();
            if fallback_just_happened(std::env::var(FALLBACK_ENV).ok().as_deref(), now) {
                return Err(format!("{why}; already a fallback, not opening the browser again"));
            }
            eprintln!("naTiVeFS-link: {why}; opening the link in the browser instead");
            // inherited by the browser process (also through the system's default-browser lookup)
            std::env::set_var(FALLBACK_ENV, now.to_string());
            // always the https form: a tvfs:// link would come straight back here
            fallback_browser(&browser_link(req.url))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_marker_expires() {
        assert!(!fallback_just_happened(None, 1000));
        assert!(fallback_just_happened(Some("990"), 1000));
        assert!(!fallback_just_happened(Some("900"), 1000));
        // the old marker ("1") of earlier versions no longer blocks anything
        assert!(!fallback_just_happened(Some("1"), 1_800_000_000));
        assert!(!fallback_just_happened(Some("junk"), 1000));
    }

    #[test]
    fn browser_links_are_https_and_marked() {
        let u = url::Url::parse("http://files.tomasekvalla.cz/files/audio/a.flac").unwrap();
        assert_eq!(browser_link(u), "https://files.tomasekvalla.cz/files/audio/a.flac?inbrowser=1");
    }
}
