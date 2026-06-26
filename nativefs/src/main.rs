mod app;
mod auth;
mod clipboard;
mod config;
mod history;
mod shortcuts;
mod upload;

use app::NativeFS;
use std::sync::{Arc, Mutex};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    TrayIconBuilder,
};

// ── Tray action ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum TrayAction {
    Open,
    UploadFile,
    CopyLastDirect,
    CopyLastStyled,
    Quit,
}

// ── Wrapper that handles tray ─────────────────────────────────────────────────

struct TrayAwareApp {
    inner: NativeFS,
    tray_actions: Arc<Mutex<Vec<TrayAction>>>,
}

impl eframe::App for TrayAwareApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let actions: Vec<TrayAction> = {
            let mut g = self.tray_actions.lock().unwrap();
            std::mem::take(&mut *g)
        };

        for action in actions {
            match action {
                TrayAction::Open => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.request_repaint();
                }
                TrayAction::UploadFile => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    self.inner.open_file_picker();
                    if !self.inner.files.is_empty() {
                        self.inner.start_upload();
                    }
                }
                TrayAction::CopyLastDirect => self.inner.copy_last_direct(),
                TrayAction::CopyLastStyled => self.inner.copy_last_styled(),
                TrayAction::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            }
        }

        self.inner.update(ctx, frame);
    }
}

// ── Icon helpers ──────────────────────────────────────────────────────────────

fn white_icon_16() -> tray_icon::Icon {
    let data = vec![255u8; 16 * 16 * 4];
    tray_icon::Icon::from_rgba(data, 16, 16).unwrap()
}

fn load_tray_icon() -> tray_icon::Icon {
    // If icon.png is present next to the binary, load it.
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()));

    if let Some(dir) = exe_dir {
        let icon_path = dir.join("icon.png");
        if icon_path.exists() {
            if let Ok(bytes) = std::fs::read(&icon_path) {
                if let Ok(img) = image::load_from_memory(&bytes) {
                    let rgba = img.into_rgba8();
                    let (w, h) = rgba.dimensions();
                    if let Ok(icon) = tray_icon::Icon::from_rgba(rgba.into_raw(), w, h) {
                        return icon;
                    }
                }
            }
        }
    }

    white_icon_16()
}

fn load_app_icon() -> egui::IconData {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()));

    if let Some(dir) = exe_dir {
        let icon_path = dir.join("icon.png");
        if icon_path.exists() {
            if let Ok(bytes) = std::fs::read(&icon_path) {
                if let Ok(img) = image::load_from_memory(&bytes) {
                    let rgba = img.into_rgba8();
                    let (w, h) = rgba.dimensions();
                    return egui::IconData {
                        rgba: rgba.into_raw(),
                        width: w,
                        height: h,
                    };
                }
            }
        }
    }

    egui::IconData::default()
}

// ── main ──────────────────────────────────────────────────────────────────────

fn main() {
    // Tray menu
    let tray_menu = Menu::new();

    let item_open        = MenuItem::new("Open",                   true, None);
    let item_upload      = MenuItem::new("Upload file…",           true, None);
    let item_sep1        = PredefinedMenuItem::separator();
    let item_copy_direct = MenuItem::new("Copy last link",         true, None);
    let item_copy_styled = MenuItem::new("Copy last styled link",  true, None);
    let item_sep2        = PredefinedMenuItem::separator();
    let item_quit        = MenuItem::new("Quit",                   true, None);

    tray_menu.append_items(&[
        &item_open,
        &item_upload,
        &item_sep1,
        &item_copy_direct,
        &item_copy_styled,
        &item_sep2,
        &item_quit,
    ]).unwrap();

    let id_open        = item_open.id().clone();
    let id_upload      = item_upload.id().clone();
    let id_copy_direct = item_copy_direct.id().clone();
    let id_copy_styled = item_copy_styled.id().clone();
    let id_quit        = item_quit.id().clone();

    let tray_icon = load_tray_icon();

    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_tooltip("naTiVeFS")
        .with_icon(tray_icon)
        .build()
        .expect("Failed to build tray icon");

    // Shared action queue
    let tray_actions: Arc<Mutex<Vec<TrayAction>>> = Arc::new(Mutex::new(Vec::new()));
    let tray_actions_bg = tray_actions.clone();

    // Background thread polling tray/menu events
    std::thread::spawn(move || {
        let menu_ch = MenuEvent::receiver();
        loop {
            if let Ok(event) = menu_ch.try_recv() {
                let action = if event.id == id_open {
                    Some(TrayAction::Open)
                } else if event.id == id_upload {
                    Some(TrayAction::UploadFile)
                } else if event.id == id_copy_direct {
                    Some(TrayAction::CopyLastDirect)
                } else if event.id == id_copy_styled {
                    Some(TrayAction::CopyLastStyled)
                } else if event.id == id_quit {
                    Some(TrayAction::Quit)
                } else {
                    None
                };

                if let Some(a) = action {
                    tray_actions_bg.lock().unwrap().push(a);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    });

    let app_icon = load_app_icon();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("naTiVeFS")
            .with_inner_size([480.0, 580.0])
            .with_min_inner_size([380.0, 420.0])
            .with_icon(Arc::new(app_icon)),
        ..Default::default()
    };

    eframe::run_native(
        "naTiVeFS",
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(TrayAwareApp {
                inner: NativeFS::new(cc),
                tray_actions,
            }))
        }),
    )
    .expect("eframe failed");
}
