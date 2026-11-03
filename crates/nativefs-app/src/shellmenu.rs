//! File manager menus: "Upload to TVFS" (files and folders) and "Open locally in naTiVeFS" (files the viewers can show).
//! They run the app with `--upload <paths>` / `--local <path>`; a running instance takes them over through IPC.
//!
//! Windows: the classic context menu ("Show more options") + Send To, per user (HKCU, no admin).
//! Linux: Dolphin service menu, Nautilus scripts, Nemo actions, Thunar custom actions.
//! macOS: not yet (a Quick Action needs a signed-free Automator bundle; no Mac to build and test it on).

pub const UPLOAD_ARG: &str = "--upload";
pub const LOCAL_ARG: &str = "--local";

const UPLOAD_LABEL: &str = "Upload to TVFS";
const LOCAL_LABEL: &str = "Open locally in naTiVeFS";

/// The program the menus start (inside an AppImage: the AppImage).
fn app_exe() -> Option<String> {
    #[cfg(target_os = "linux")]
    if let Ok(p) = std::env::var("APPIMAGE") {
        return Some(p);
    }
    std::env::current_exe().ok().map(|p| p.to_string_lossy().into_owned())
}

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::reg;

    /// Debug builds use their own verb names and labels, so testing never touches the installed app's menus.
    const VERB: &str = if cfg!(debug_assertions) { "naTiVeFSdev" } else { "naTiVeFS" };
    const DEV: &str = if cfg!(debug_assertions) { " (dev)" } else { "" };

    fn verb_keys() -> Vec<String> {
        let mut keys = vec![
            format!(r"Software\Classes\*\shell\{VERB}.upload"),
            format!(r"Software\Classes\Directory\shell\{VERB}.upload"),
        ];
        for ext in tvfs_core::content::viewer_extensions() {
            keys.push(format!(r"Software\Classes\SystemFileAssociations\.{ext}\shell\{VERB}.local"));
        }
        keys
    }

    fn sendto_link() -> Option<std::path::PathBuf> {
        let appdata = std::env::var_os("APPDATA")?;
        Some(std::path::PathBuf::from(appdata).join(format!(r"Microsoft\Windows\SendTo\{UPLOAD_LABEL}{DEV}.lnk")))
    }

    pub fn installed() -> bool {
        reg::get_str(&format!(r"Software\Classes\*\shell\{VERB}.upload\command"), "").is_some()
    }

    pub fn install() -> Result<(), String> {
        let exe = app_exe().ok_or("the app's path is unknown")?;
        let icon = format!("\"{exe}\",0");
        for key in verb_keys() {
            let local = key.ends_with(".local");
            reg::set_str(&key, "MUIVerb", &format!("{}{DEV}", if local { LOCAL_LABEL } else { UPLOAD_LABEL }))?;
            reg::set_str(&key, "Icon", &icon)?;
            // several selected files start one command each; the first instance collects the rest through IPC
            reg::set_str(&key, "MultiSelectModel", "Player")?;
            let arg = if local { LOCAL_ARG } else { UPLOAD_ARG };
            reg::set_str(&format!(r"{key}\command"), "", &format!("\"{exe}\" {arg} \"%1\""))?;
        }
        // Send To passes every selected file in one command line
        if let Some(lnk) = sendto_link() {
            shortcut(&lnk, &exe, UPLOAD_ARG, "Upload the selected files to TVFS")?;
        }
        Ok(())
    }

    pub fn uninstall() {
        for key in verb_keys() {
            reg::delete_tree(&key);
        }
        if let Some(lnk) = sendto_link() {
            let _ = std::fs::remove_file(lnk);
        }
    }

    /// A `.lnk` shortcut (IShellLink), like the ones Explorer makes.
    fn shortcut(lnk: &std::path::Path, target: &str, args: &str, description: &str) -> Result<(), String> {
        use windows::core::{Interface, HSTRING};
        use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
        use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
        // COM on a short-lived thread of its own (the UI thread's apartment belongs to winit)
        let (lnk, target, args, description) = (lnk.to_owned(), target.to_owned(), args.to_owned(), description.to_owned());
        std::thread::spawn(move || unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
            link.SetPath(&HSTRING::from(target.as_str())).map_err(|e| e.to_string())?;
            link.SetArguments(&HSTRING::from(args.as_str())).map_err(|e| e.to_string())?;
            link.SetDescription(&HSTRING::from(description.as_str())).map_err(|e| e.to_string())?;
            link.SetIconLocation(&HSTRING::from(target.as_str()), 0).map_err(|e| e.to_string())?;
            let file: IPersistFile = link.cast().map_err(|e| e.to_string())?;
            file.Save(&HSTRING::from(lnk.as_os_str()), true).map_err(|e| e.to_string())
        })
        .join()
        .map_err(|_| "shortcut thread panicked".to_string())?
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    const STEM: &str = if cfg!(debug_assertions) { "nativefs-dev" } else { "nativefs" };
    const DEV: &str = if cfg!(debug_assertions) { " (dev)" } else { "" };

    fn home() -> Option<PathBuf> {
        std::env::var_os("HOME").map(PathBuf::from)
    }

    fn data_home() -> Option<PathBuf> {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).or_else(|| Some(home()?.join(".local/share")))
    }

    fn config_home() -> Option<PathBuf> {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).or_else(|| Some(home()?.join(".config")))
    }

    /// Files this module owns (written on install, removed on uninstall). Thunar's shared `uca.xml` is handled apart.
    fn files() -> Vec<PathBuf> {
        let Some(data) = data_home() else { return Vec::new() };
        vec![
            // Dolphin (KDE Frameworks 6, and 5 for older Plasma)
            data.join(format!("kio/servicemenus/{STEM}.desktop")),
            data.join(format!("kservices5/ServiceMenus/{STEM}.desktop")),
            // Nautilus scripts (right click > Scripts)
            data.join(format!("nautilus/scripts/{UPLOAD_LABEL}{DEV}")),
            data.join(format!("nautilus/scripts/{LOCAL_LABEL}{DEV}")),
            // Nemo actions
            data.join(format!("nemo/actions/{STEM}-upload.nemo_action")),
            data.join(format!("nemo/actions/{STEM}-local.nemo_action")),
        ]
    }

    /// Desktop-entry Exec quoting.
    fn quote(s: &str) -> String {
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

    /// POSIX shell quoting.
    fn sh(s: &str) -> String {
        format!("'{}'", s.replace('\'', "'\\''"))
    }

    fn xml(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
    }

    pub fn installed() -> bool {
        files().first().is_some_and(|f| f.exists())
    }

    fn write(path: &PathBuf, text: &str, exec: bool) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, text).map_err(|e| e.to_string())?;
        if exec {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn install() -> Result<(), String> {
        let exe = app_exe().ok_or("the app's path is unknown")?;
        let f = files();
        let exts = tvfs_core::content::viewer_extensions().join(";");
        // Dolphin: one service menu with both actions (KF6 wants it executable)
        let dolphin = format!(
            "[Desktop Entry]\nType=Service\nMimeType=all/allfiles;inode/directory;\nActions=upload;local;\n\
             X-KDE-Priority=TopLevel\n\n\
             [Desktop Action upload]\nName={UPLOAD_LABEL}{DEV}\nIcon=nativefs\nExec={q} {UPLOAD_ARG} %F\n\n\
             [Desktop Action local]\nName={LOCAL_LABEL}{DEV}\nIcon=nativefs\nExec={q} {LOCAL_ARG} %f\n",
            q = quote(&exe)
        );
        write(&f[0], &dolphin, true)?;
        write(&f[1], &dolphin.replace("Type=Service\n", "Type=Service\nServiceTypes=KonqPopupMenu/Plugin\n"), true)?;
        // Nautilus: scripts get the selection in an environment variable, one path per line
        let upload_sh = format!(
            "#!/bin/sh\n# naTiVeFS: upload the selected files and folders to TVFS\n\
             set -f\nIFS='\n'\nexec {} {UPLOAD_ARG} $NAUTILUS_SCRIPT_SELECTED_FILE_PATHS\n",
            sh(&exe)
        );
        let local_sh = format!(
            "#!/bin/sh\n# naTiVeFS: open the first selected file locally\n\
             first=$(printf '%s\\n' \"$NAUTILUS_SCRIPT_SELECTED_FILE_PATHS\" | head -n 1)\nexec {} {LOCAL_ARG} \"$first\"\n",
            sh(&exe)
        );
        write(&f[2], &upload_sh, true)?;
        write(&f[3], &local_sh, true)?;
        // Nemo
        write(
            &f[4],
            &format!("[Nemo Action]\nName={UPLOAD_LABEL}{DEV}\nIcon-Name=nativefs\nExec={q} {UPLOAD_ARG} %F\nSelection=notnone\nExtensions=any;\n", q = quote(&exe)),
            false,
        )?;
        write(
            &f[5],
            &format!("[Nemo Action]\nName={LOCAL_LABEL}{DEV}\nIcon-Name=nativefs\nExec={q} {LOCAL_ARG} %F\nSelection=s\nExtensions={exts};\n", q = quote(&exe)),
            false,
        )?;
        // Thunar: custom actions live in one shared file; ours are replaced by their unique ids
        thunar(Some(&exe))?;
        Ok(())
    }

    fn thunar(exe: Option<&str>) -> Result<(), String> {
        let Some(path) = config_home().map(|c| c.join("Thunar/uca.xml")) else { return Ok(()) };
        let old = std::fs::read_to_string(&path).unwrap_or_default();
        if old.is_empty() && exe.is_none() {
            return Ok(());
        }
        // drop our earlier entries (each `<action>` block holding one of our ids)
        let ids = [format!("{STEM}-upload"), format!("{STEM}-local")];
        let mut kept = String::new();
        let mut rest = old.as_str();
        while let Some(start) = rest.find("<action>") {
            let Some(len) = rest[start..].find("</action>") else { break };
            let end = start + len + "</action>".len();
            kept.push_str(&rest[..start]);
            let block = &rest[start..end];
            if !ids.iter().any(|id| block.contains(&format!("<unique-id>{id}</unique-id>"))) {
                kept.push_str(block);
            }
            rest = &rest[end..];
        }
        kept.push_str(rest);
        let mut text = if kept.contains("</actions>") { kept } else { "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<actions>\n</actions>\n".into() };
        if let Some(exe) = exe {
            let q = sh(exe);
            let ours = format!(
                "<action>\n\t<icon>nativefs</icon>\n\t<name>{}</name>\n\t<unique-id>{}</unique-id>\n\t<command>{} {UPLOAD_ARG} %F</command>\n\t\
                 <description>Upload to TVFS</description>\n\t<patterns>*</patterns>\n\t<directories/>\n\t<audio-files/>\n\t\
                 <image-files/>\n\t<other-files/>\n\t<text-files/>\n\t<video-files/>\n</action>\n\
                 <action>\n\t<icon>nativefs</icon>\n\t<name>{}</name>\n\t<unique-id>{}</unique-id>\n\t<command>{} {LOCAL_ARG} %f</command>\n\t\
                 <description>Open locally in naTiVeFS</description>\n\t<patterns>{}</patterns>\n\t<audio-files/>\n\t<image-files/>\n\t\
                 <other-files/>\n\t<text-files/>\n\t<video-files/>\n</action>\n",
                xml(&format!("{UPLOAD_LABEL}{DEV}")),
                ids[0],
                xml(&q),
                xml(&format!("{LOCAL_LABEL}{DEV}")),
                ids[1],
                xml(&q),
                tvfs_core::content::viewer_extensions().iter().map(|e| format!("*.{e}")).collect::<Vec<_>>().join(";"),
            );
            text = text.replacen("</actions>", &format!("{ours}</actions>"), 1);
        }
        if text == old {
            return Ok(());
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, text).map_err(|e| e.to_string())
    }

    pub fn uninstall() {
        for f in files() {
            let _ = std::fs::remove_file(f);
        }
        let _ = thunar(None);
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    pub fn installed() -> bool {
        false
    }
    pub fn install() -> Result<(), String> {
        Err("File manager menus are not available on this system yet.".into())
    }
    pub fn uninstall() {}
}

pub use imp::{install, installed, uninstall};

/// At startup (release builds): keep the menus pointing at this installation, or remove them when switched off.
pub fn sync(wanted: bool) {
    if cfg!(debug_assertions) && std::env::var_os("NATIVEFS_FILE_MENUS").is_none() {
        return;
    }
    if wanted {
        if let Err(e) = install() {
            eprintln!("naTiVeFS: file manager menus: {e}");
        }
    } else if installed() {
        uninstall();
    }
}
