//! Installing a signed update. Windows: the release's NSIS installer runs silently and starts the new version.
//! Linux archive installs under the home folder: the binaries are replaced in place. Packages, AppImages, macOS and
//! portable copies are updated by hand, so the app only points to the release.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tvfs_core::update::{self, ReleaseInfo};

/// The update found by the last check.
static PENDING: Mutex<Option<ReleaseInfo>> = Mutex::new(None);

pub enum How {
    #[cfg_attr(not(windows), allow(dead_code))]
    Installer { all_users: bool },
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Archive(PathBuf),
    Manual(&'static str),
}

/// A verified update waiting for a moment when restarting bothers nobody.
static READY: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn set_ready(file: PathBuf) {
    *READY.lock().unwrap() = Some(file);
}

pub fn ready() -> Option<PathBuf> {
    READY.lock().unwrap().clone()
}

pub fn remember(info: Option<ReleaseInfo>) {
    *PENDING.lock().unwrap() = info;
}

#[cfg(windows)]
pub fn how() -> How {
    use crate::reg::{self, Root};
    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\naTiVeFS";
    if cfg!(debug_assertions) {
        return How::Manual("Debug builds do not update themselves.");
    }
    let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) else {
        return How::Manual("Download the new version from the release page.");
    };
    for root in [Root::User, Root::Machine] {
        let installed = reg::get_str_in(root, KEY, "InstallLocation").map(PathBuf::from);
        if installed.is_some_and(|p| p.to_string_lossy().eq_ignore_ascii_case(&dir.to_string_lossy())) {
            return How::Installer { all_users: root == Root::Machine };
        }
    }
    How::Manual("This copy was not installed with the installer: download the new version from the release page.")
}

#[cfg(target_os = "linux")]
pub fn how() -> How {
    if cfg!(debug_assertions) {
        return How::Manual("Debug builds do not update themselves.");
    }
    if std::env::var_os("APPIMAGE").is_some() {
        return How::Manual("Download the new AppImage from the release page.");
    }
    let dir = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf));
    match (dir, std::env::var_os("HOME").map(PathBuf::from)) {
        (Some(dir), Some(home)) if dir.starts_with(&home) => How::Archive(dir),
        _ => How::Manual("Update naTiVeFS with your package manager, or run the install command again."),
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn how() -> How {
    How::Manual("Download the new version from the release page.")
}

/// Download and verify the pending update.
pub async fn download(progress: impl FnMut(u64, Option<u64>)) -> Result<PathBuf, String> {
    let info = PENDING.lock().unwrap().clone().ok_or("no update to install")?;
    let dir = std::env::temp_dir().join(format!("nativefs-update-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join(&info.asset_name);
    update::download_verified(&info, &file, progress).await.map_err(|e| e.to_string())?;
    Ok(file)
}

/// Start the installer or replace the binaries. `Ok` means the app should quit now. `hidden`: nobody is looking, so
/// the new version starts in the tray again.
pub fn install(file: &Path, hidden: bool) -> Result<(), String> {
    match how() {
        How::Manual(text) => Err(text.into()),
        how => apply(how, file, hidden),
    }
}

/// Automatic updates never ask for anything: a copy installed for all users needs an administrator prompt.
pub fn silent_possible() -> bool {
    !matches!(how(), How::Manual(_) | How::Installer { all_users: true })
}

#[cfg(windows)]
fn apply(how: How, file: &Path, hidden: bool) -> Result<(), String> {
    use windows::core::HSTRING;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let How::Installer { all_users } = how else { return Err("nothing to run".into()) };
    let (verb, params) = match (all_users, hidden) {
        (true, _) => ("runas", "/S /UPDATE /ALLUSERS"),
        (false, true) => ("open", "/S /UPDATE /HIDDEN"),
        (false, false) => ("open", "/S /UPDATE"),
    };
    let result = unsafe {
        ShellExecuteW(None, &HSTRING::from(verb), &HSTRING::from(file.as_os_str()), &HSTRING::from(params), None, SW_SHOWNORMAL)
    };
    // values above 32 mean success
    if result.0 as isize > 32 { Ok(()) } else { Err("The installer did not start.".into()) }
}

#[cfg(target_os = "linux")]
fn apply(how: How, file: &Path, hidden: bool) -> Result<(), String> {
    use std::process::Command;
    let How::Archive(dir) = how else { return Err("nothing to install".into()) };
    // unpacked beside the binaries, so the final renames stay on one file system
    let tmp = dir.join(format!(".nativefs-update-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let unpack = || -> Result<PathBuf, String> {
        let ok = Command::new("tar").arg("-xzf").arg(file).arg("-C").arg(&tmp).status().is_ok_and(|s| s.success());
        if !ok {
            return Err("The update archive could not be unpacked.".into());
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        std::fs::rename(tmp.join("nativefs"), &exe).map_err(|e| e.to_string())?;
        let link = ["nativefs-link", "naTiVeFS-link"].map(|n| dir.join(n)).into_iter().find(|p| p.exists());
        std::fs::rename(tmp.join("nativefs-link"), link.unwrap_or_else(|| dir.join("nativefs-link"))).map_err(|e| e.to_string())?;
        Ok(exe)
    };
    let result = unpack();
    let _ = std::fs::remove_dir_all(&tmp);
    let exe = result?;
    // the download's own folder, not needed any more
    if let Some(download) = file.parent() {
        let _ = std::fs::remove_dir_all(download);
    }
    // started a moment later: this process must release the instance socket first
    let args = hidden.then_some("--minimized");
    Command::new("sh").args(["-c", "sleep 1; exec \"$0\" \"$@\""]).arg(exe).args(args).spawn().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(not(any(windows, target_os = "linux")))]
fn apply(_how: How, _file: &Path, _hidden: bool) -> Result<(), String> {
    Err("Download the new version from the release page.".into())
}
