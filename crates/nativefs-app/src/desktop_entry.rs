//! Linux: the app's own desktop entry and icon, so Wayland compositors (which take a window's icon from the `.desktop`
//! file matching its app id, not from the window) show the logo instead of a generic one. Written per user at every
//! start when missing or out of date, also for unpackaged builds; packages may install system copies under the same id.
//! Debug builds use their own id (`nativefs-dev`), so testing never touches the installed app's entry.

/// The xdg app id of every window; it names the desktop entry (`<id>.desktop`) and the icon.
pub const APP_ID: &str = if cfg!(debug_assertions) { "nativefs-dev" } else { "nativefs" };

#[cfg(target_os = "linux")]
mod imp {
    use super::APP_ID;
    use std::path::{Path, PathBuf};

    const ICON_256: &[u8] = include_bytes!("../../../assets/app-256.png");
    const ICON_64: &[u8] = include_bytes!("../../../assets/app-64.png");

    fn data_home() -> Option<PathBuf> {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    }

    /// Inside an AppImage the AppImage itself, not its temporary mount.
    fn exe() -> Option<String> {
        if let Ok(p) = std::env::var("APPIMAGE") {
            return Some(p);
        }
        std::env::current_exe().ok().map(|p| p.to_string_lossy().into_owned())
    }

    /// Desktop-entry Exec quoting, only when the spec requires it (`xdg-settings` and others do not strip quotes).
    fn quote(s: &str) -> String {
        if !s.is_empty() && !s.chars().any(|c| c.is_whitespace() || "\"'\\><~|&;$*?#()`%".contains(c)) {
            return s.to_owned();
        }
        let mut q = String::from("\"");
        for c in s.chars() {
            if matches!(c, '\\' | '"' | '`' | '$') {
                q.push('\\');
            }
            q.push(c);
        }
        q.push('"');
        q.replace('%', "%%")
    }

    /// Write only when the content differs (no needless disk writes or desktop database refreshes).
    fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<bool, String> {
        if std::fs::read(path).is_ok_and(|old| old == bytes) {
            return Ok(false);
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, bytes).map_err(|e| e.to_string())?;
        Ok(true)
    }

    fn user_files(data: &Path) -> [PathBuf; 3] {
        [
            data.join(format!("icons/hicolor/256x256/apps/{APP_ID}.png")),
            data.join(format!("icons/hicolor/64x64/apps/{APP_ID}.png")),
            data.join(format!("applications/{APP_ID}.desktop")),
        ]
    }

    /// Removes this user's entry and icons (`--cleanup`).
    pub fn remove() {
        let Some(data) = data_home() else { return };
        for f in user_files(&data) {
            let _ = std::fs::remove_file(f);
        }
    }

    pub fn sync() {
        // a package installed the system copy: a user copy would only stay behind after uninstalling it
        if std::env::var_os("APPIMAGE").is_none() && Path::new(&format!("/usr/share/applications/{APP_ID}.desktop")).exists() {
            remove();
            return;
        }
        let (Some(data), Some(exe)) = (data_home(), exe()) else { return };
        let name = if cfg!(debug_assertions) { "naTiVeFS (dev)" } else { "naTiVeFS" };
        // release builds also own the `tvfs://` scheme (see `router::register_scheme`)
        let mime = if cfg!(debug_assertions) { String::new() } else { format!("MimeType=x-scheme-handler/{};\n", tvfs_core::URL_SCHEME) };
        let entry = format!(
            "[Desktop Entry]\nType=Application\nName={name}\nGenericName=TVFS client\n\
             Comment=Upload, share and open files on TVFS\nExec={} %u\nIcon={APP_ID}\nTerminal=false\n\
             Categories=Network;FileTransfer;\nStartupWMClass={APP_ID}\n{mime}",
            quote(&exe)
        );
        let icons = data.join("icons/hicolor");
        let results = [
            write_if_changed(&icons.join(format!("256x256/apps/{APP_ID}.png")), ICON_256),
            write_if_changed(&icons.join(format!("64x64/apps/{APP_ID}.png")), ICON_64),
            write_if_changed(&data.join(format!("applications/{APP_ID}.desktop")), entry.as_bytes()),
        ];
        let mut changed = false;
        for r in results {
            match r {
                Ok(c) => changed |= c,
                Err(e) => eprintln!("naTiVeFS: desktop entry: {e}"),
            }
        }
        if changed {
            let _ = std::process::Command::new("update-desktop-database").arg(data.join("applications")).status();
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    pub fn sync() {}
    pub fn remove() {}
}

pub use imp::{remove, sync};
