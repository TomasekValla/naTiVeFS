//! The full-screen interface: terminal setup, the event loop, and a text-dump helper for tests.

mod app;
mod picker;
mod theme;
mod ui;

use std::io::{stdout, Stdout};
use std::sync::mpsc::{channel, RecvTimeoutError};

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::Terminal;

use crate::settings::ThemeMode;
use app::{App, Msg};

fn enter() -> std::io::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste)?;
    Terminal::new(CrosstermBackend::new(stdout()))
}

/// Safe to call more than once; also used by the panic hook.
fn leave() {
    let _ = execute!(stdout(), DisableBracketedPaste, DisableMouseCapture, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        leave();
    }
}

pub fn run(theme: Option<ThemeMode>) -> i32 {
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("naTiVeFS-cli: {e}");
            return 1;
        }
    };
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        leave();
        previous(info);
    }));
    let mut terminal = match enter() {
        Ok(t) => t,
        Err(e) => {
            leave();
            eprintln!("naTiVeFS-cli: cannot take over the terminal: {e}");
            return 1;
        }
    };
    let _guard = Guard;
    let (tx, rx) = channel::<Msg>();
    // one thread turns terminal events into messages; it ends with the process
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            while let Ok(ev) = event::read() {
                let msg = match ev {
                    Event::Key(k) if k.kind != KeyEventKind::Release => Msg::Key(k),
                    Event::Mouse(m) => Msg::Mouse(m),
                    Event::Paste(s) => Msg::Paste(s),
                    Event::Resize(..) => Msg::Resize,
                    _ => continue,
                };
                if tx.send(msg).is_err() {
                    break;
                }
            }
        });
    }
    let mut app = App::new(rt.handle().clone(), tx, theme);
    let result = (|| -> std::io::Result<()> {
        loop {
            if app.dirty {
                app.dirty = false;
                terminal.draw(|f| ui::draw(&mut app, f))?;
            }
            if app.quit {
                return Ok(());
            }
            let first = match app.next_tick() {
                Some(d) => rx.recv_timeout(d),
                None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match first {
                Ok(m) => app.update(m),
                Err(RecvTimeoutError::Timeout) => app.on_tick(),
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
            while let Ok(m) = rx.try_recv() {
                app.update(m);
            }
            // a tick that came due while messages were handled
            if app.next_tick().is_some_and(|d| d.is_zero()) {
                app.on_tick();
            }
        }
    })();
    drop(_guard);
    if let Err(e) = result {
        eprintln!("naTiVeFS-cli: {e}");
        return 1;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn dump(app: &mut App, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| ui::draw(app, f)).unwrap();
        let buf = t.backend().buffer().clone();
        let mut s = String::new();
        for y in 0..h {
            let line: String = (0..w).map(|x| buf[(x, y)].symbol().to_owned()).collect();
            s.push_str(line.trim_end());
            s.push('\n');
        }
        s
    }

    fn test_app() -> (App, tokio::runtime::Runtime) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let (tx, _rx) = channel();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("NATIVEFS_CONFIG", dir.path().join("config.toml"));
        std::mem::forget(dir);
        let mut app = App::new(rt.handle().clone(), tx, Some(ThemeMode::Dark));
        let files = tempfile::tempdir().unwrap();
        std::fs::create_dir(files.path().join("holiday-photos")).unwrap();
        std::fs::write(files.path().join("notes.md"), "hello").unwrap();
        std::fs::write(files.path().join("clip.mp4"), vec![0u8; 5_000_000]).unwrap();
        app.picker = picker::Picker::open(files.path());
        std::mem::forget(files);
        (app, rt)
    }

    #[test]
    fn more_screens_draw() {
        use crate::links::{FileOut, Finished};
        use tvfs_core::models::UploadCompleteResponse;
        let (mut app, _rt) = test_app();
        app.user = Some("demo".into());
        app.status = app::StatusInfo { checked: true, online: true, latency_ms: Some(31), used: "4.20 GB".into(), tier: 1 };
        app.picker.selected.insert(app.picker.entries.iter().find(|e| e.name == "clip.mp4").unwrap().path.clone());
        app.screen = app::Screen::Picker;
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.screen, app::Screen::Options);
        let mut all = dump(&mut app, 100, 30);
        let run = tvfs_core::upload::Run::new([("clip.mp4".to_string(), 5_000_000u64), ("notes.md".to_string(), 5u64)], Default::default());
        app.up = Some(app::UploadState { run, control: Default::default(), confirm_cancel: false });
        app.screen = app::Screen::Uploading;
        all.push_str(&dump(&mut app, 100, 30));
        app.up = None;
        let r = UploadCompleteResponse { link: "https://files.example/files/text/notes.md".into(), size: 5, styled_link: Some("[notes.md](https://files.example/files/text/notes.md)".into()), md_reader_link: Some("https://files.example/files/reader/notes.md".into()), md_reader_styled_link: Some("[notes.md](https://files.example/files/reader/notes.md)".into()), ..Default::default() };
        app.result.fin = Finished { files: vec![FileOut { name: "notes.md".into(), result: Ok(r) }, FileOut { name: "bad.bin".into(), result: Err("Server busy".into()) }], ..Default::default() };
        app.result.elapsed = 1.5;
        app.screen = app::Screen::Result;
        all.push_str(&dump(&mut app, 100, 30));
        assert!(all.contains("(default)") && all.contains("FAILED") && all.contains("Link expires after") && all.contains("Uploading"), "{all}");
        app.recent.entries = vec![tvfs_core::models::SyncEntry { id: "1".into(), name: "photo.png".into(), link: "https://x/p.png".into(), category: "image".into(), size: 1234567, uploaded_at: None, expires_at: Some(crate::common::now_ms() + 86_400_000), thumbnail: None, voice_note: false }];
        app.screen = app::Screen::Recent;
        all.push_str(&dump(&mut app, 100, 30));
        app.cli.theme = ThemeMode::Light;
        app.theme = theme::Theme::new(true, true);
        all.push_str(&dump(&mut app, 100, 30));
        if let Some(p) = std::env::var_os("CLI_DUMP") {
            std::fs::write(p, &all).unwrap();
        }
    }

    #[test]
    fn screens_draw_and_navigate() {
        let (mut app, _rt) = test_app();
        let login = dump(&mut app, 100, 30);
        assert!(login.contains("Log in to TVFS") && login.contains("Username") && login.contains("not logged in"), "{login}");
        // pretend to be logged in and walk the screens
        app.user = Some("demo".into());
        app.status.checked = true;
        app.status.online = true;
        app.status.latency_ms = Some(42);
        app.status.used = "12.34 GB".into();
        app.screen = app::Screen::Menu;
        let menu = dump(&mut app, 100, 30);
        assert!(menu.contains("Upload files") && menu.contains("Recent uploads") && menu.contains("● online") && menu.contains("42 ms"), "{menu}");
        app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        let picker = dump(&mut app, 100, 30);
        assert!(picker.contains("Selected: 0 file(s)"), "{picker}");
        println!("{picker}");
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.screen, app::Screen::Menu);
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        let settings = dump(&mut app, 100, 30);
        assert!(settings.contains("Auto copy after upload"), "{settings}");
        // tiny terminals do not panic
        dump(&mut app, 30, 8);
        dump(&mut app, 60, 14);
    }
}
