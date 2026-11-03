//! `naTiVeFS-link <url>`: decides in milliseconds, without starting the GUI
//! unless it is needed. Registered as a (optional, opt-in) default browser in
//! a later stage; today it already works when invoked by hand or via `tvfs://`.

use std::path::PathBuf;
use std::process::{Command, ExitCode};

use nativefs_ipc::Message;
use tvfs_core::{config::Config, link};

/// The app next to this binary: `naTiVeFS(.exe)` as built, `nativefs` in Linux/macOS packages.
fn app_path() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    let named = |n: &str| exe.with_file_name(format!("{n}{}", std::env::consts::EXE_SUFFIX));
    let built = named("naTiVeFS");
    if built.exists() { built } else { named("nativefs") }
}

fn open_in_app(url: &str) -> std::io::Result<()> {
    if nativefs_ipc::send(&Message::Open(url.to_owned())).is_ok() {
        return Ok(()); // running instance took it
    }
    Command::new(app_path()).arg(url).spawn().map(|_| ())
}

fn main() -> ExitCode {
    let Some(arg) = std::env::args().nth(1) else {
        return match Command::new(app_path()).spawn() {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("naTiVeFS-link: cannot start naTiVeFS: {e}");
                ExitCode::FAILURE
            }
        };
    };

    let result = if link::parse_open_request(&arg).is_some() {
        open_in_app(&arg).map_err(|e| e.to_string())
    } else {
        Config::load().open_in_browser(&arg).map_err(|e| e.to_string())
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("naTiVeFS-link: {e}");
            ExitCode::FAILURE
        }
    }
}
