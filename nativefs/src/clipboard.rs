/// Copy a string to the system clipboard via arboard.
/// Returns Ok(()) on success, Err(message) on failure.
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    clipboard.set_text(text).map_err(|e| e.to_string())
}

/// Format a styled Markdown link matching TVFS server output:
/// [original_name - TomasekValla Filestream System](url)
pub fn styled_link(original_name: &str, url: &str) -> String {
    format!("[{original_name} - TomasekValla Filestream System]({url})")
}
