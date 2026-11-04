//! Link router: "Use naTiVeFS as the default browser" (opt-in). The OS hands every web link to `naTiVeFS-link`,
//! which opens TVFS links in the app and passes everything else to the real browser: the one the user picked, else
//! the one that was the default before (detected when the router is switched on). Also the `tvfs://` scheme.
//!
//! Windows: registered per user like any browser (`StartMenuInternet` + `RegisteredApplications` + a ProgId);
//! Windows lets only the user pick the default, so switching on opens Settings on naTiVeFS's page ("Set default")
//! and switching off opens it on the previous browser's page. Linux: a `.desktop` file + `xdg-settings`.
//! macOS: best effort (the app bundle declares the schemes, the user picks it in System Settings).

/// A browser the user has installed.
#[derive(Clone, Debug, PartialEq)]
pub struct Browser {
    pub name: String,
    /// Command with `%1` (Windows) or a desktop-entry field code where the link goes.
    pub command: String,
    /// What the OS calls it (to make it the default again).
    pub id: String,
}

/// Where the router stands right now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum State {
    /// naTiVeFS is not registered as a browser.
    Off,
    /// Registered, but another browser is still the default (the user has not finished it in the system settings).
    Registered,
    /// naTiVeFS is the default browser.
    Default,
    /// Registered; the OS does not tell us which browser is the default (macOS).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Unknown,
}

/// After switching on / off: what the user still has to do.
pub enum Next {
    Done,
    /// A system settings page was opened; the text says what to click there.
    FinishInSettings(&'static str),
}

/// The command the OS runs for a link: `naTiVeFS-link` next to the app (inside an AppImage: the AppImage with `--link`).
fn link_command() -> Option<(String, Vec<String>)> {
    #[cfg(target_os = "linux")]
    if let Ok(p) = std::env::var("APPIMAGE") {
        return Some((p, vec!["--link".into()]));
    }
    let exe = std::env::current_exe().ok()?;
    let named = |n: &str| exe.with_file_name(format!("{n}{}", std::env::consts::EXE_SUFFIX));
    let link = [named("naTiVeFS-link"), named("nativefs-link")].into_iter().find(|p| p.exists())?;
    Some((link.to_string_lossy().into_owned(), Vec::new()))
}

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::reg::{self, Root};

    /// Debug builds register under their own names, so testing never touches the owner's installed app.
    const APP: &str = if cfg!(debug_assertions) { "naTiVeFS-dev" } else { "naTiVeFS" };
    const PROGID: &str = if cfg!(debug_assertions) { "naTiVeFSdevURL" } else { "naTiVeFSURL" };
    const SMI: &str = r"Software\Clients\StartMenuInternet";
    const REGAPPS: &str = r"Software\RegisteredApplications";
    const CHOICE: &str = r"Software\Microsoft\Windows\Shell\Associations\UrlAssociations\https\UserChoice";
    /// Newer Windows 11 builds keep the user's pick in `UserChoiceLatest\ProgId` and leave the older `UserChoice`
    /// stale (the old key may still name a browser picked long ago), so the new one is read first.
    const CHOICE_LATEST: &str = r"Software\Microsoft\Windows\Shell\Associations\UrlAssociations\https\UserChoiceLatest\ProgId";

    fn smi_key() -> String {
        format!(r"{SMI}\{APP}")
    }

    /// The command Windows runs for a ProgId (`"...chrome.exe" --single-argument %1`).
    fn progid_command(progid: &str) -> Option<String> {
        let key = format!(r"Software\Classes\{progid}\shell\open\command");
        reg::get_str_in(Root::User, &key, "").or_else(|| reg::get_str_in(Root::Machine, &key, ""))
    }

    /// Every registered browser (Chrome, Edge, Brave, Firefox, Zen, Vivaldi, Opera, ...), per user and for all users.
    pub fn installed() -> Vec<Browser> {
        let mut out: Vec<Browser> = Vec::new();
        for root in [Root::User, Root::Machine] {
            for app in reg::names(root, REGAPPS, true) {
                if app.eq_ignore_ascii_case("naTiVeFS") || app.eq_ignore_ascii_case("naTiVeFS-dev") {
                    continue;
                }
                let Some(caps) = reg::get_str_in(root, REGAPPS, &app) else { continue };
                // only browsers: they claim https
                let Some(progid) = reg::get_str_in(root, &format!(r"{caps}\URLAssociations"), "https") else { continue };
                let Some(command) = progid_command(&progid) else { continue };
                let name = reg::get_str_in(root, &caps, "ApplicationName")
                    .filter(|n| !n.starts_with('@'))
                    .unwrap_or_else(|| app.clone());
                // the id says where it is registered: Settings needs registeredAppUser / registeredAppMachine
                let id = format!("{}:{app}", if root == Root::User { "user" } else { "machine" });
                if !out.iter().any(|b| b.name == name) {
                    out.push(Browser { name, command, id });
                }
            }
        }
        out.sort_by_key(|b| b.name.to_lowercase());
        out
    }

    fn default_progid() -> Option<String> {
        reg::get_str(CHOICE_LATEST, "ProgId").filter(|p| !p.is_empty()).or_else(|| reg::get_str(CHOICE, "ProgId"))
    }

    /// The browser that handles https links now (None when it is naTiVeFS or unknown).
    pub fn current_default() -> Option<Browser> {
        let progid = default_progid()?;
        if progid == PROGID {
            return None;
        }
        // find its registered name through the ProgId it claims
        for root in [Root::User, Root::Machine] {
            for app in reg::names(root, REGAPPS, true) {
                let Some(caps) = reg::get_str_in(root, REGAPPS, &app) else { continue };
                if reg::get_str_in(root, &format!(r"{caps}\URLAssociations"), "https").as_deref() == Some(progid.as_str()) {
                    let name = reg::get_str_in(root, &caps, "ApplicationName").filter(|n| !n.starts_with('@')).unwrap_or(app.clone());
                    let id = format!("{}:{app}", if root == Root::User { "user" } else { "machine" });
                    return Some(Browser { name, command: progid_command(&progid)?, id });
                }
            }
        }
        Some(Browser { name: progid.clone(), command: progid_command(&progid)?, id: String::new() })
    }

    pub fn state() -> State {
        if reg::get_str(REGAPPS, APP).is_none() {
            State::Off
        } else if default_progid().as_deref() == Some(PROGID) {
            State::Default
        } else {
            State::Registered
        }
    }

    fn settings_page(id: &str) {
        // debug builds: `NATIVEFS_NO_SETTINGS=1` skips opening Settings (tests must not pop windows on the main monitor)
        if cfg!(debug_assertions) && std::env::var_os("NATIVEFS_NO_SETTINGS").is_some() {
            eprintln!("router: would open Settings for {id:?}");
            return;
        }
        let uri = match id.split_once(':') {
            Some(("user", app)) => format!("ms-settings:defaultapps?registeredAppUser={app}"),
            Some(("machine", app)) => format!("ms-settings:defaultapps?registeredAppMachine={app}"),
            _ => "ms-settings:defaultapps".into(),
        };
        if let Err(e) = opener::open(&uri) {
            eprintln!("naTiVeFS: could not open {uri}: {e}");
        }
    }

    pub fn register() -> Result<(), String> {
        let (link, _) = link_command().ok_or("naTiVeFS-link.exe was not found next to the app")?;
        let app_exe = std::env::current_exe().map_err(|e| e.to_string())?.to_string_lossy().into_owned();
        let icon = format!("\"{app_exe}\",0");
        let open = format!("\"{link}\" \"%1\"");
        let classes = format!(r"Software\Classes\{PROGID}");
        reg::set_str(&classes, "", "naTiVeFS link")?;
        reg::set_str(&classes, "URL Protocol", "")?;
        reg::set_str(&format!(r"{classes}\DefaultIcon"), "", &icon)?;
        reg::set_str(&format!(r"{classes}\shell\open\command"), "", &open)?;
        let smi = smi_key();
        reg::set_str(&smi, "", APP)?;
        reg::set_str(&format!(r"{smi}\DefaultIcon"), "", &icon)?;
        reg::set_str(&format!(r"{smi}\shell\open\command"), "", &format!("\"{link}\""))?;
        let caps = format!(r"{smi}\Capabilities");
        reg::set_str(&caps, "ApplicationName", APP)?;
        reg::set_str(&caps, "ApplicationIcon", &icon)?;
        reg::set_str(
            &caps,
            "ApplicationDescription",
            "Opens TVFS links in naTiVeFS and passes every other link to your real browser.",
        )?;
        for scheme in ["http", "https"] {
            reg::set_str(&format!(r"{caps}\URLAssociations"), scheme, PROGID)?;
        }
        reg::set_str(REGAPPS, APP, &caps)
    }

    pub fn unregister() {
        reg::delete_value(REGAPPS, APP);
        reg::delete_tree(&smi_key());
        reg::delete_tree(&format!(r"Software\Classes\{PROGID}"));
    }

    pub fn enable() -> Result<Next, String> {
        register()?;
        if state() == State::Default {
            return Ok(Next::Done);
        }
        settings_page(&format!("user:{APP}"));
        Ok(Next::FinishInSettings("In Windows Settings, click \"Set default\" next to naTiVeFS."))
    }

    pub fn disable(previous: Option<&str>) -> Result<Next, String> {
        if state() != State::Default {
            unregister();
            return Ok(Next::Done);
        }
        // Windows lets only the user switch: open the previous browser's page; the registration is removed once
        // naTiVeFS is no longer the default (`tidy`), so links keep working meanwhile
        settings_page(previous.unwrap_or(""));
        Ok(Next::FinishInSettings("In Windows Settings, click \"Set default\" for your browser."))
    }

    /// The `tvfs://` scheme, kept pointing at this installation (release builds only).
    pub fn register_scheme() {
        if cfg!(debug_assertions) {
            return;
        }
        let Some((link, _)) = link_command() else { return };
        let key = format!(r"Software\Classes\{}", tvfs_core::URL_SCHEME);
        let open = format!("\"{link}\" \"%1\"");
        if reg::get_str(&format!(r"{key}\shell\open\command"), "").as_deref() == Some(open.as_str()) {
            return;
        }
        let _ = reg::set_str(&key, "", "URL:TVFS link");
        let _ = reg::set_str(&key, "URL Protocol", "");
        let _ = reg::set_str(&format!(r"{key}\shell\open\command"), "", &open);
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    const DESKTOP: &str = if cfg!(debug_assertions) { "nativefs-link-dev.desktop" } else { "nativefs-link.desktop" };

    fn data_home() -> Option<PathBuf> {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    }

    fn app_dirs() -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = data_home().into_iter().collect();
        let sys = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
        dirs.extend(sys.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
        // Flatpak and Snap browsers
        dirs.push(PathBuf::from("/var/lib/flatpak/exports/share"));
        if let Some(h) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(h).join(".local/share/flatpak/exports/share"));
        }
        dirs.push(PathBuf::from("/var/lib/snapd/desktop"));
        dirs.into_iter().map(|d| d.join("applications")).collect()
    }

    /// `Name=` and `Exec=` of a desktop entry's main section, if it handles https links.
    fn parse(path: &std::path::Path) -> Option<(String, String)> {
        let text = std::fs::read_to_string(path).ok()?;
        let (mut name, mut exec, mut https, mut main, mut hidden) = (None, None, false, false, false);
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                main = line == "[Desktop Entry]";
                continue;
            }
            if !main {
                continue;
            }
            if let Some(v) = line.strip_prefix("Name=") {
                name.get_or_insert(v.to_owned());
            } else if let Some(v) = line.strip_prefix("Exec=") {
                exec.get_or_insert(v.to_owned());
            } else if let Some(v) = line.strip_prefix("MimeType=") {
                https = v.split(';').any(|m| m == "x-scheme-handler/https");
            } else if line == "NoDisplay=true" || line == "Hidden=true" {
                hidden = true;
            }
        }
        (https && !hidden).then_some((name?, exec?))
    }

    fn find(id: &str) -> Option<Browser> {
        app_dirs().into_iter().find_map(|d| parse(&d.join(id))).map(|(name, command)| Browser { name, command, id: id.to_owned() })
    }

    pub fn installed() -> Vec<Browser> {
        let mut out: Vec<Browser> = Vec::new();
        for dir in app_dirs() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let id = e.file_name().to_string_lossy().into_owned();
                if !id.ends_with(".desktop") || id.starts_with("nativefs") || out.iter().any(|b| b.id == id) {
                    continue;
                }
                if let Some((name, command)) = parse(&e.path()) {
                    out.push(Browser { name, command, id });
                }
            }
        }
        out.sort_by_key(|b| b.name.to_lowercase());
        out
    }

    fn xdg_default() -> Option<String> {
        let o = Command::new("xdg-settings").args(["get", "default-web-browser"]).output().ok()?;
        let id = String::from_utf8_lossy(&o.stdout).trim().to_owned();
        (!id.is_empty()).then_some(id)
    }

    pub fn current_default() -> Option<Browser> {
        let id = xdg_default()?;
        if id.starts_with("nativefs") {
            return None;
        }
        find(&id)
    }

    fn file() -> Option<PathBuf> {
        Some(data_home()?.join("applications").join(DESKTOP))
    }

    pub fn state() -> State {
        if !file().is_some_and(|f| f.exists()) {
            State::Off
        } else if xdg_default().as_deref() == Some(DESKTOP) {
            State::Default
        } else {
            State::Registered
        }
    }

    /// Desktop-entry quoting, only when needed: `xdg-settings` resolves the first word of `Exec=` with `command -v`
    /// and does not strip quotes, so a quoted path makes it fail ("file missing") although the entry is valid.
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
        q
    }

    pub fn register() -> Result<(), String> {
        let (prog, args) = link_command().ok_or("nativefs-link was not found next to the app")?;
        let mut exec = quote(&prog);
        for a in args {
            exec.push(' ');
            exec.push_str(&a);
        }
        let text = format!(
            "[Desktop Entry]\nType=Application\nName=naTiVeFS (link router)\n\
             Comment=Opens TVFS links in naTiVeFS and every other link in your browser\nExec={exec} %u\nIcon=nativefs\n\
             Terminal=false\nNoDisplay=true\nCategories=Network;WebBrowser;\n\
             MimeType=x-scheme-handler/http;x-scheme-handler/https;x-scheme-handler/{};\n",
            tvfs_core::URL_SCHEME
        );
        let f = file().ok_or("no home directory")?;
        if let Some(dir) = f.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&f, text).map_err(|e| e.to_string())?;
        let _ = Command::new("update-desktop-database").arg(f.parent().unwrap_or(&f)).status();
        Ok(())
    }

    pub fn unregister() {
        if let Some(f) = file() {
            let _ = std::fs::remove_file(f);
        }
    }

    /// `xdg-settings set default-web-browser`, falling back to `xdg-mime` for the http(s) handlers when the script
    /// refuses the entry (it validates `Exec=` more strictly than the desktop does).
    fn set_default(id: &str) -> bool {
        if Command::new("xdg-settings").args(["set", "default-web-browser", id]).status().is_ok_and(|s| s.success()) {
            return true;
        }
        Command::new("xdg-mime")
            .args(["default", id, "x-scheme-handler/http", "x-scheme-handler/https"])
            .status()
            .is_ok_and(|s| s.success())
    }

    pub fn enable() -> Result<Next, String> {
        register()?;
        let ok = set_default(DESKTOP);
        if ok && state() == State::Default {
            Ok(Next::Done)
        } else {
            Err("The desktop did not accept naTiVeFS as the default browser (xdg-settings failed). Pick \"naTiVeFS (link router)\" as the web browser in your system settings.".into())
        }
    }

    pub fn disable(previous: Option<&str>) -> Result<Next, String> {
        if let Some(prev) = previous.filter(|p| !p.is_empty()) {
            set_default(prev);
        }
        if state() == State::Default {
            return Err("Could not give the default back to your previous browser: pick it in your system settings.".into());
        }
        unregister();
        Ok(Next::Done)
    }

    /// `tvfs://` links: the app's own `.desktop` entry claims the scheme (installed by the packages).
    pub fn register_scheme() {
        if cfg!(debug_assertions) {
            return;
        }
        let scheme = format!("x-scheme-handler/{}", tvfs_core::URL_SCHEME);
        let o = Command::new("xdg-mime").args(["query", "default", &scheme]).output();
        if o.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim().is_empty()) {
            let _ = Command::new("xdg-mime").args(["default", "nativefs.desktop", &scheme]).status();
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    /// Common browsers by app name; the command uses `open -a`.
    const KNOWN: [&str; 12] = [
        "Safari", "Google Chrome", "Microsoft Edge", "Brave Browser", "Firefox", "Zen", "Vivaldi", "Opera", "LibreWolf",
        "Chromium", "Waterfox", "Floorp",
    ];

    pub fn installed() -> Vec<Browser> {
        let home = std::env::var("HOME").unwrap_or_default();
        KNOWN
            .iter()
            .filter(|n| {
                [format!("/Applications/{n}.app"), format!("{home}/Applications/{n}.app")].iter().any(|p| std::path::Path::new(p).exists())
            })
            .map(|n| Browser { name: (*n).into(), command: format!("open -a \"{n}\""), id: (*n).into() })
            .collect()
    }

    pub fn current_default() -> Option<Browser> {
        None
    }

    pub fn state() -> State {
        State::Unknown
    }

    pub fn enable() -> Result<Next, String> {
        let _ = std::process::Command::new("open").arg("x-apple.systempreferences:com.apple.Desktop-Settings.extension").status();
        Ok(Next::FinishInSettings("In System Settings > Desktop & Dock, choose naTiVeFS as the default web browser."))
    }

    pub fn disable(_previous: Option<&str>) -> Result<Next, String> {
        let _ = std::process::Command::new("open").arg("x-apple.systempreferences:com.apple.Desktop-Settings.extension").status();
        Ok(Next::FinishInSettings("In System Settings > Desktop & Dock, choose your browser as the default again."))
    }

    pub fn register_scheme() {}
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod imp {
    use super::*;
    pub fn installed() -> Vec<Browser> {
        Vec::new()
    }
    pub fn current_default() -> Option<Browser> {
        None
    }
    pub fn state() -> State {
        State::Off
    }
    pub fn enable() -> Result<Next, String> {
        Err("Not supported on this system.".into())
    }
    pub fn disable(_previous: Option<&str>) -> Result<Next, String> {
        Ok(Next::Done)
    }
    pub fn register_scheme() {}
}

pub use imp::{current_default, disable, enable, installed, register_scheme, state};

/// Switching on: remember the current default browser (never naTiVeFS itself) so other links go there, then register.
pub fn switch_on(cfg: &mut tvfs_core::config::Config) -> Result<Next, String> {
    if let Some(b) = current_default() {
        cfg.router_browser = Some(b.command);
        cfg.router_browser_name = Some(b.name);
        cfg.router_previous_id = Some(b.id);
    }
    if cfg.browser.as_deref().map(str::trim).unwrap_or("").is_empty() && cfg.router_browser.is_none() {
        return Err("Pick the browser for other links first (no default browser was found to pass them to).".into());
    }
    enable()
}

/// The uninstaller: remove this user's browser registration.
pub fn cleanup() {
    #[cfg(any(windows, target_os = "linux"))]
    imp::unregister();
}

pub fn switch_off(cfg: &tvfs_core::config::Config) -> Result<Next, String> {
    disable(cfg.router_previous_id.as_deref())
}

/// At startup: keep `tvfs://` pointing here, and remove a Windows registration left after the user gave the default
/// back to another browser.
pub fn tidy(router_wanted: bool) {
    register_scheme();
    #[cfg(windows)]
    if !router_wanted && state() == State::Registered {
        imp::unregister();
    }
    let _ = router_wanted;
}
