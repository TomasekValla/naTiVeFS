//! "Upload finished" notifications (Settings > Uploads > Notify when done). Like the web, they appear only while the
//! app is not in front: a finished or failed upload the user is not looking at.

/// Windows identity of the toasts. An app without an installer package needs it registered under
/// `HKCU\Software\Classes\AppUserModelId\<id>` (name + icon), else Windows shows the toast as "Windows PowerShell".
#[cfg(windows)]
const AUMID: &str = if cfg!(debug_assertions) { "TomasekValla.naTiVeFS.Dev" } else { "TomasekValla.naTiVeFS" };

#[cfg(windows)]
fn aumid_key() -> String {
    format!(r"Software\Classes\AppUserModelId\{AUMID}")
}

/// Registers (on) or removes (off) the toast identity. Also removed by `--cleanup` (the uninstaller).
pub fn register(on: bool) {
    #[cfg(windows)]
    {
        if !on {
            crate::reg::delete_tree(&aumid_key());
            return;
        }
        const ICON: &[u8] = include_bytes!("../../../assets/app-256.png");
        let icon = tvfs_core::config::private_dir("notify").ok().map(|d| d.join("naTiVeFS.png"));
        if let Some(path) = &icon {
            if std::fs::read(path).ok().as_deref() != Some(ICON) {
                let _ = std::fs::write(path, ICON);
            }
        }
        let key = aumid_key();
        let _ = crate::reg::set_str(&key, "DisplayName", "naTiVeFS");
        if let Some(path) = icon {
            let _ = crate::reg::set_str(&key, "IconUri", &path.to_string_lossy());
        }
    }
    #[cfg(not(windows))]
    let _ = on;
}

/// The web's `notifyUploadsDone` texts: "Upload finished" / "3 uploads finished" / "Upload failed" /
/// "2 finished, 1 failed"; the body is the file name for a single file, else "TVFS".
pub fn texts(ok: usize, failed: usize, single_name: Option<&str>) -> (String, String) {
    let title = if failed == 0 {
        if ok == 1 { "Upload finished".to_owned() } else { format!("{ok} uploads finished") }
    } else if ok == 0 {
        "Upload failed".to_owned()
    } else {
        format!("{ok} finished, {failed} failed")
    };
    let body = single_name.filter(|_| ok + failed == 1).unwrap_or("TVFS").to_owned();
    (title, body)
}

/// Shows the notification (on its own thread: the system call may take a moment).
pub fn uploads_done(ok: usize, failed: usize, single_name: Option<&str>) {
    if ok + failed == 0 {
        return;
    }
    let (title, body) = texts(ok, failed, single_name);
    std::thread::spawn(move || {
        let mut n = notify_rust::Notification::new();
        n.summary(&title).body(&body);
        #[cfg(windows)]
        n.app_id(AUMID);
        #[cfg(not(windows))]
        n.appname("naTiVeFS").icon("nativefs");
        if let Err(e) = n.show() {
            eprintln!("naTiVeFS: notification not shown: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::texts;

    #[test]
    fn texts_like_the_web() {
        assert_eq!(texts(1, 0, Some("a.mp4")), ("Upload finished".into(), "a.mp4".into()));
        assert_eq!(texts(3, 0, Some("a.mp4")), ("3 uploads finished".into(), "TVFS".into()));
        assert_eq!(texts(0, 1, Some("a.mp4")), ("Upload failed".into(), "a.mp4".into()));
        assert_eq!(texts(2, 1, None), ("2 finished, 1 failed".into(), "TVFS".into()));
    }
}
