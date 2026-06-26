use egui::{Context, Key};

#[derive(Debug, Clone, PartialEq)]
pub enum ShortcutAction {
    OpenFilePicker,
    CopyDirectLink,
    CopyStyledLink,
    ClearFiles,
    CycleTheme,
    OpenSettings,
    HideWindow,
}

/// Check all shortcuts; returns the first triggered action if any.
/// Call once per frame after `ctx.input(...)`.
pub fn check_shortcuts(ctx: &Context) -> Option<ShortcutAction> {
    ctx.input(|i| {
        let ctrl = i.modifiers.ctrl;
        let shift = i.modifiers.shift;

        // Ctrl+U — open file picker / upload
        if ctrl && !shift && i.key_pressed(Key::U) {
            return Some(ShortcutAction::OpenFilePicker);
        }

        // Ctrl+Shift+C — copy direct link
        if ctrl && shift && i.key_pressed(Key::C) {
            return Some(ShortcutAction::CopyDirectLink);
        }

        // Ctrl+Shift+S — copy styled link
        if ctrl && shift && i.key_pressed(Key::S) {
            return Some(ShortcutAction::CopyStyledLink);
        }

        // Ctrl+Backspace — clear file list
        if ctrl && !shift && i.key_pressed(Key::Backspace) {
            return Some(ShortcutAction::ClearFiles);
        }

        // Ctrl+T — cycle theme
        if ctrl && !shift && i.key_pressed(Key::T) {
            return Some(ShortcutAction::CycleTheme);
        }

        // Ctrl+, — open settings (comma)
        if ctrl && !shift && i.key_pressed(Key::Comma) {
            return Some(ShortcutAction::OpenSettings);
        }

        // Ctrl+W — hide/close window
        if ctrl && !shift && i.key_pressed(Key::W) {
            return Some(ShortcutAction::HideWindow);
        }

        None
    })
}
