//! "Start with the computer": a per-user autostart entry that runs the app with `--autostart`.
//! Whether that start shows the window or stays in the tray is the `start_minimized` setting, read at startup,
//! so switching it never rewrites the entry.
//!
//! Windows: `HKCU\...\Run` (respects the "Disabled" flag Task Manager writes to `StartupApproved\Run`).
//! Linux: `~/.config/autostart/nativefs.desktop` (XDG). macOS: a LaunchAgent in `~/Library/LaunchAgents`.

pub const ARG: &str = "--autostart";

/// Debug builds use their own entry, so testing never touches the owner's installed app.
#[cfg(debug_assertions)]
const NAME: &str = "naTiVeFS-dev";
#[cfg(not(debug_assertions))]
const NAME: &str = "naTiVeFS";

/// The executable the entry starts: inside an AppImage the AppImage itself, not the temporary mount.
fn exe() -> Result<String, String> {
    #[cfg(target_os = "linux")]
    if let Ok(p) = std::env::var("APPIMAGE") {
        return Ok(p);
    }
    std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).map_err(|e| e.to_string())
}

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::reg;

    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

    pub fn enabled() -> bool {
        if reg::get_str(RUN, NAME).is_none() {
            return false;
        }
        // Task Manager's "Disable": first byte odd = disabled (0x02 enabled, 0x03 disabled)
        !reg::get_bin(APPROVED, NAME).is_some_and(|b| b.first().is_some_and(|f| f & 1 == 1))
    }

    pub fn set(on: bool) -> Result<(), String> {
        if on {
            reg::set_str(RUN, NAME, &format!("\"{}\" {ARG}", exe()?))?;
            // clear a Task Manager "Disabled" mark left from before
            reg::delete_value(APPROVED, NAME);
        } else {
            reg::delete_value(RUN, NAME);
            reg::delete_value(APPROVED, NAME);
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::path::PathBuf;

    fn file() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        let stem = if cfg!(debug_assertions) { "nativefs-dev" } else { "nativefs" };
        Some(base.join("autostart").join(format!("{stem}.desktop")))
    }

    pub fn enabled() -> bool {
        let Some(f) = file() else { return false };
        std::fs::read_to_string(f).is_ok_and(|t| !t.lines().any(|l| l.trim() == "Hidden=true"))
    }

    pub fn set(on: bool) -> Result<(), String> {
        let f = file().ok_or("no home directory")?;
        if !on {
            return match std::fs::remove_file(&f) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
                _ => Ok(()),
            };
        }
        // Exec quoting (desktop entry spec): double quotes, with \ " ` $ escaped
        let mut q = String::from("\"");
        for c in exe()?.chars() {
            if matches!(c, '\\' | '"' | '`' | '$') {
                q.push('\\');
            }
            q.push(c);
        }
        q.push('"');
        let text = format!(
            "[Desktop Entry]\nType=Application\nName={NAME}\nComment=Start naTiVeFS in the background\nExec={q} {ARG}\n\
             Icon=nativefs\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
        );
        if let Some(dir) = f.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&f, text).map_err(|e| e.to_string())
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use std::path::PathBuf;

    fn file() -> Option<PathBuf> {
        let label = if cfg!(debug_assertions) { "cz.tomasekvalla.nativefs.dev" } else { "cz.tomasekvalla.nativefs" };
        Some(PathBuf::from(std::env::var_os("HOME")?).join("Library/LaunchAgents").join(format!("{label}.plist")))
    }

    pub fn enabled() -> bool {
        file().is_some_and(|f| f.exists())
    }

    pub fn set(on: bool) -> Result<(), String> {
        let f = file().ok_or("no home directory")?;
        if !on {
            return match std::fs::remove_file(&f) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
                _ => Ok(()),
            };
        }
        let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let label = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let text = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\"><dict>\n\
             <key>Label</key><string>{label}</string>\n\
             <key>ProgramArguments</key><array><string>{}</string><string>{ARG}</string></array>\n\
             <key>RunAtLoad</key><true/>\n\
             </dict></plist>\n",
            esc(&exe()?)
        );
        if let Some(dir) = f.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&f, text).map_err(|e| e.to_string())
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod imp {
    pub fn enabled() -> bool {
        false
    }
    pub fn set(_on: bool) -> Result<(), String> {
        Err("autostart is not supported on this system".into())
    }
}

pub use imp::{enabled, set};
