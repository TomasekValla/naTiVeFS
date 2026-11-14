//! Tests of the navigation (Back row, Left / Backspace), the system dialog mode and the upload options screen.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use super::app::{self, App, Msg};
use super::tests::{dump, test_app};

fn key(app: &mut App, c: KeyCode) {
    app.on_key(KeyEvent::new(c, KeyModifiers::NONE));
}

#[test]
fn left_and_backspace_go_back_and_the_back_row_works() {
    let (mut app, _rt) = test_app();
    app.user = Some("demo".into());
    app.screen = app::Screen::Menu;
    // Settings: Backspace goes back (Left changes the value there); Up from the first row reaches the Back row
    key(&mut app, KeyCode::Char('s'));
    key(&mut app, KeyCode::Backspace);
    assert_eq!(app.screen, app::Screen::Menu);
    key(&mut app, KeyCode::Char('s'));
    key(&mut app, KeyCode::Up);
    assert_eq!(app.back_focus, Some(app::Screen::Settings));
    let d = dump(&mut app, 100, 30);
    assert!(d.contains("‹ Back") && d.contains("Esc/⌫ back"), "{d}");
    key(&mut app, KeyCode::Enter);
    assert_eq!((app.screen, app.back_focus), (app::Screen::Menu, None));
    // Picker: Up from the first row reaches the Back row, Down leaves it, Left goes back
    key(&mut app, KeyCode::Char('u'));
    assert_eq!(app.screen, app::Screen::Picker);
    key(&mut app, KeyCode::Up);
    assert_eq!(app.back_focus, Some(app::Screen::Picker));
    key(&mut app, KeyCode::Down);
    assert_eq!(app.back_focus, None);
    key(&mut app, KeyCode::Up);
    key(&mut app, KeyCode::Left);
    assert_eq!(app.screen, app::Screen::Menu);
    // Left climbs the folders first, and leaves the picker when there is nothing above
    key(&mut app, KeyCode::Char('u'));
    app.picker.go(None);
    key(&mut app, KeyCode::Left);
    assert_eq!(app.screen, app::Screen::Menu);
    // Result, Recent
    app.screen = app::Screen::Result;
    key(&mut app, KeyCode::Left);
    assert_eq!(app.screen, app::Screen::Menu);
    app.screen = app::Screen::Recent;
    key(&mut app, KeyCode::Backspace);
    assert_eq!(app.screen, app::Screen::Menu);
    // a pending delete question still takes every key (Left must not leave the screen)
    app.screen = app::Screen::Recent;
    app.recent.pending_delete = Some(("1".into(), "a.png".into()));
    key(&mut app, KeyCode::Left);
    assert_eq!(app.screen, app::Screen::Recent);
    // Options: Backspace goes back
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("a.txt");
    std::fs::write(&f, "x").unwrap();
    app.open_paths(vec![f]);
    assert_eq!(app.screen, app::Screen::Options);
    key(&mut app, KeyCode::Backspace);
    assert_eq!(app.screen, app::Screen::Picker);
    // the Back row also answers the mouse
    app.screen = app::Screen::Recent;
    app.recent.pending_delete = None;
    dump(&mut app, 100, 30);
    let back = app.hits.iter().find(|(_, h)| *h == app::Hit::Back).unwrap().0;
    app.update(Msg::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: back.x + 2, row: back.y, modifiers: KeyModifiers::NONE }));
    assert_eq!(app.screen, app::Screen::Menu);
}

#[test]
fn system_dialog_mode_opens_the_dialog_from_the_menu() {
    let (mut app, _rt) = test_app();
    app.user = Some("demo".into());
    app.screen = app::Screen::Menu;
    app.cli.picker = crate::settings::PickerMode::System;
    key(&mut app, KeyCode::Char('u'));
    assert_eq!(app.screen, app::Screen::SystemPick);
    assert!(app.dialog_open);
    let d = dump(&mut app, 100, 30);
    assert!(d.contains("system file dialog is open") && d.contains("choose a folder instead") && d.contains("terminal file browser"), "{d}");
    // a second open while one is up is refused
    key(&mut app, KeyCode::Char('f'));
    assert!(app.toast.as_ref().is_some_and(|t| t.0.contains("already open")));
    app.update(Msg::PickCancelled);
    assert!(!app.dialog_open && app.screen == app::Screen::SystemPick);
    // the way back to the terminal browser
    key(&mut app, KeyCode::Char('t'));
    assert_eq!(app.screen, app::Screen::Picker);
    // a chosen file goes straight to the options; Back returns to the dialog screen
    app.screen = app::Screen::SystemPick;
    app.dialog_open = true;
    let f = tempfile::tempdir().unwrap();
    let file = f.path().join("a.txt");
    std::fs::write(&file, "x").unwrap();
    app.update(Msg::PickedDirect(vec![file]));
    assert_eq!(app.screen, app::Screen::Options);
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.screen, app::Screen::SystemPick);
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.screen, app::Screen::Menu);
    // the terminal setting keeps the old behaviour
    app.cli.picker = crate::settings::PickerMode::Terminal;
    key(&mut app, KeyCode::Char('u'));
    assert_eq!(app.screen, app::Screen::Picker);
    // and the picker setting has its row
    app.screen = app::Screen::Settings;
    assert!(dump(&mut app, 120, 30).contains("Pick files with"));
}

#[test]
fn voice_note_row_only_with_audio_and_users_only_limit() {
    let (mut app, _rt) = test_app();
    app.user = Some("demo".into());
    app.screen = app::Screen::Menu;
    let d = tempfile::tempdir().unwrap();
    let (txt, mp3) = (d.path().join("a.txt"), d.path().join("note.mp3"));
    std::fs::write(&txt, "x").unwrap();
    std::fs::write(&mp3, "x").unwrap();
    app.open_paths(vec![txt.clone()]);
    assert!(!app.opts.has_audio() && app.opts.rows() == 3);
    assert!(!dump(&mut app, 100, 30).contains("voice note"));
    app.screen = app::Screen::Menu;
    app.picker.selected.clear();
    app.open_paths(vec![txt, mp3]);
    assert!(app.opts.has_audio() && app.opts.rows() == 4);
    app.opts.users_only = false;
    assert!(dump(&mut app, 100, 30).contains("[ ] Upload as voice note"));
    for _ in 0..3 {
        key(&mut app, KeyCode::Down);
    }
    assert_eq!(app.opts.row, 3);
    key(&mut app, KeyCode::Char(' '));
    assert!(app.opts.voice_note);
    assert!(dump(&mut app, 100, 30).contains("[x] Upload as voice note"));
    app.opts.users_only = true;
    assert!(dump(&mut app, 100, 30).contains("public uploads only"));
    // more than 200 files in a Users Only share is refused before anything starts
    let one = app.opts.files[0].clone();
    app.opts.files = vec![one; 201];
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.screen, app::Screen::Options);
    assert!(app.toast.as_ref().is_some_and(|t| t.2 && t.0.contains("at most 200 files")), "{:?}", app.toast.as_ref().map(|t| &t.0));
    assert!(dump(&mut app, 100, 30).contains("at most 200 files"));
}

#[test]
fn options_keep_the_chips_and_the_public_warning_on_small_terminals() {
    let (mut app, _rt) = test_app();
    app.user = Some("demo".into());
    app.screen = app::Screen::Menu;
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("a.txt");
    std::fs::write(&f, "x").unwrap();
    app.open_paths(vec![f]);
    let one = app.opts.files[0].clone();
    app.opts.files = vec![one; 12];
    app.opts.users_only = false;
    for (w, h) in [(60u16, 20u16), (45, 18), (100, 16)] {
        let d = dump(&mut app, w, h);
        assert!(d.contains("14d") && d.contains("5m"), "{w}x{h}\n{d}");
        assert!(d.contains("The link will be public") && d.contains("start the upload"), "{w}x{h}\n{d}");
    }
}
