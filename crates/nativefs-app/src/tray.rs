//! Always-resident tray icon with a small control centre: open the window, the audio player's media controls,
//! open a local file, upload, open the TVFS link on the clipboard, voice note, settings, quit.
//! Linux uses the StatusNotifierItem protocol (ksni, no GTK needed); Windows and macOS use `tray-icon`.

use std::any::Any;
use std::sync::Arc;

/// What a tray click asks the app to do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    Show,
    PlayPause,
    Stop,
    OpenLocal,
    Upload,
    ClipboardLink,
    VoiceNote,
    Settings,
    Quit,
}

/// Called from the tray's own thread (Linux) or the UI thread (Windows / macOS).
pub type Callback = Arc<dyn Fn(Action) + Send + Sync + 'static>;

/// The audio player shown in the menu (the one that plays, else the newest).
#[derive(Debug, Clone, PartialEq)]
pub struct Media {
    pub title: String,
    pub playing: bool,
}

/// Fixed entries after the media block: (label, action); `None` = separator.
const ENTRIES: [Option<(&str, Action)>; 8] = [
    Some(("Open local file...", Action::OpenLocal)),
    Some(("Upload files...", Action::Upload)),
    Some(("Open TVFS link from clipboard", Action::ClipboardLink)),
    Some(("New voice note...", Action::VoiceNote)),
    None,
    Some(("Settings", Action::Settings)),
    None,
    Some(("Quit", Action::Quit)),
];

fn media_title(m: &Media) -> String {
    let mut t: String = m.title.chars().take(48).collect();
    if m.title.chars().count() > 48 {
        t.push_str("...");
    }
    t
}

fn tray_rgba() -> (Vec<u8>, u32, u32) {
    let img = image::load_from_memory(include_bytes!("../../../assets/tray-64.png"))
        .expect("embedded tray icon is valid")
        .into_rgba8();
    let (w, h) = img.dimensions();
    (img.into_raw(), w, h)
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use ksni::{blocking::Handle, blocking::TrayMethods, menu::StandardItem, Icon, MenuItem, Tray};
    use std::sync::Mutex;

    pub struct AppTray {
        on: Callback,
        icon: Vec<Icon>,
        media: Option<Media>,
    }

    static HANDLE: Mutex<Option<Handle<AppTray>>> = Mutex::new(None);

    fn item(label: impl Into<String>, a: Action, enabled: bool) -> MenuItem<AppTray> {
        StandardItem { label: label.into(), enabled, activate: Box::new(move |t: &mut AppTray| (t.on)(a)), ..Default::default() }
            .into()
    }

    impl Tray for AppTray {
        fn id(&self) -> String {
            "naTiVeFS".into()
        }
        fn title(&self) -> String {
            "naTiVeFS".into()
        }
        fn icon_pixmap(&self) -> Vec<Icon> {
            self.icon.clone()
        }
        fn activate(&mut self, _x: i32, _y: i32) {
            (self.on)(Action::Show);
        }
        fn menu(&self) -> Vec<MenuItem<Self>> {
            let mut m = vec![item("Open naTiVeFS", Action::Show, true), MenuItem::Separator];
            if let Some(media) = &self.media {
                m.push(item(media_title(media), Action::Show, false));
                m.push(item(if media.playing { "Pause" } else { "Play" }, Action::PlayPause, true));
                m.push(item("Stop", Action::Stop, true));
                m.push(MenuItem::Separator);
            }
            for e in ENTRIES {
                m.push(match e {
                    Some((label, a)) => item(label, a, true),
                    None => MenuItem::Separator,
                });
            }
            m
        }
    }

    pub fn start(on: Callback) -> Option<Box<dyn Any>> {
        let (rgba, w, h) = tray_rgba();
        let mut argb = Vec::with_capacity(rgba.len());
        for px in rgba.chunks_exact(4) {
            argb.extend_from_slice(&[px[3], px[0], px[1], px[2]]); // ARGB32, network byte order
        }
        let tray = AppTray { on, icon: vec![Icon { width: w as i32, height: h as i32, data: argb }], media: None };
        match tray.spawn() {
            Ok(handle) => {
                *HANDLE.lock().unwrap() = Some(handle.clone());
                Some(Box::new(handle))
            }
            Err(e) => {
                eprintln!("naTiVeFS: tray unavailable: {e}");
                None
            }
        }
    }

    static LAST: Mutex<Option<Media>> = Mutex::new(None);

    pub fn set_media(media: Option<Media>) {
        // called on every player state change: talk to D-Bus only when the menu really changes
        let mut last = LAST.lock().unwrap();
        if *last == media {
            return;
        }
        *last = media.clone();
        if let Some(h) = HANDLE.lock().unwrap().as_ref() {
            h.update(|t| t.media = media);
        }
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
mod imp {
    use super::*;
    use std::cell::RefCell;
    use tray_icon::menu::{IsMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    /// The menu and its media block (title, Play / Pause, Stop, separator), inserted at position 2 while a player is open.
    struct Items {
        menu: Menu,
        title: MenuItem,
        play: MenuItem,
        stop: MenuItem,
        sep: PredefinedMenuItem,
        media: Option<Media>,
    }

    thread_local! {
        static ITEMS: RefCell<Option<Items>> = const { RefCell::new(None) };
    }

    pub fn start(on: Callback) -> Option<Box<dyn Any>> {
        let menu = Menu::new();
        let mut ids: Vec<(MenuId, Action)> = Vec::new();
        let open = MenuItem::new("Open naTiVeFS", true, None);
        ids.push((open.id().clone(), Action::Show));
        menu.append_items(&[&open, &PredefinedMenuItem::separator()]).ok()?;
        for e in ENTRIES {
            match e {
                Some((label, a)) => {
                    let it = MenuItem::new(label, true, None);
                    ids.push((it.id().clone(), a));
                    menu.append(&it).ok()?;
                }
                None => menu.append(&PredefinedMenuItem::separator()).ok()?,
            }
        }
        let title = MenuItem::new("", false, None);
        let play = MenuItem::new("Play", true, None);
        let stop = MenuItem::new("Stop", true, None);
        ids.push((play.id().clone(), Action::PlayPause));
        ids.push((stop.id().clone(), Action::Stop));

        {
            let on = on.clone();
            MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                if let Some((_, a)) = ids.iter().find(|(id, _)| *id == e.id) {
                    on(*a);
                }
            }));
        }
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                on(Action::Show);
            }
        }));

        let (rgba, w, h) = tray_rgba();
        let icon = Icon::from_rgba(rgba, w, h).ok()?;
        let tray = match TrayIconBuilder::new()
            .with_menu(Box::new(menu.clone()))
            .with_menu_on_left_click(false)
            .with_tooltip(if cfg!(debug_assertions) { "naTiVeFS (dev build)" } else { "naTiVeFS" })
            .with_icon(icon)
            .build()
        {
            Ok(tray) => tray,
            Err(e) => {
                eprintln!("naTiVeFS: tray unavailable: {e}");
                return None;
            }
        };
        let sep = PredefinedMenuItem::separator();
        ITEMS.with(|i| *i.borrow_mut() = Some(Items { menu, title, play, stop, sep, media: None }));
        Some(Box::new(tray))
    }

    /// Must run on the UI thread (where the tray was created).
    pub fn set_media(media: Option<Media>) {
        ITEMS.with(|i| {
            let mut i = i.borrow_mut();
            let Some(it) = i.as_mut() else { return };
            if it.media == media {
                return;
            }
            let block: [&dyn IsMenuItem; 4] = [&it.title, &it.play, &it.stop, &it.sep];
            match (&it.media, &media) {
                (None, Some(_)) => {
                    let _ = it.menu.insert_items(&block, 2);
                }
                (Some(_), None) => {
                    for b in block {
                        let _ = it.menu.remove(b);
                    }
                }
                _ => {}
            }
            if let Some(m) = &media {
                it.title.set_text(media_title(m));
                it.play.set_text(if m.playing { "Pause" } else { "Play" });
            }
            it.media = media;
        });
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
mod imp {
    use super::*;
    pub fn start(_on: Callback) -> Option<Box<dyn Any>> {
        None
    }
    pub fn set_media(_media: Option<Media>) {}
}

/// Returns a guard that must be kept alive for the icon to stay visible, or
/// `None` when no tray is available (e.g. GNOME without an AppIndicator extension).
pub use imp::{set_media, start};
