//! The in-terminal file browser: folders, multi-select, type-to-filter. No drawing here.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Parent,
    Drive,
    Dir,
    File,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub kind: Kind,
    pub size: u64,
}

#[derive(Debug, Default)]
pub struct Picker {
    /// `None` = the list of drives (Windows).
    pub dir: Option<PathBuf>,
    pub entries: Vec<Entry>,
    pub filter: String,
    /// Index into the visible (filtered) list.
    pub cursor: usize,
    pub scroll: usize,
    pub selected: BTreeSet<PathBuf>,
    pub show_hidden: bool,
    pub error: Option<String>,
}

impl Picker {
    pub fn open(dir: &Path) -> Self {
        let mut p = Self::default();
        p.go(Some(dir.to_owned()));
        p
    }

    pub fn go(&mut self, dir: Option<PathBuf>) {
        self.dir = dir;
        self.filter.clear();
        self.cursor = 0;
        self.scroll = 0;
        self.reload();
    }

    pub fn reload(&mut self) {
        self.error = None;
        let mut entries = Vec::new();
        match &self.dir {
            None => {
                for l in b'A'..=b'Z' {
                    let root = PathBuf::from(format!("{}:\\", l as char));
                    if root.exists() {
                        entries.push(Entry { name: format!("{}:", l as char), path: root, kind: Kind::Drive, size: 0 });
                    }
                }
            }
            Some(dir) => {
                if dir.parent().is_some() || cfg!(windows) {
                    entries.push(Entry { name: "..".into(), path: dir.parent().map(Path::to_path_buf).unwrap_or_default(), kind: Kind::Parent, size: 0 });
                }
                match std::fs::read_dir(dir) {
                    Ok(rd) => {
                        let mut rows: Vec<Entry> = rd
                            .filter_map(Result::ok)
                            .filter_map(|e| {
                                let name = e.file_name().to_string_lossy().into_owned();
                                if !self.show_hidden && name.starts_with('.') {
                                    return None;
                                }
                                // a link to a folder is a folder (DirEntry::metadata does not follow links)
                                let meta = if e.file_type().is_ok_and(|t| t.is_symlink()) { std::fs::metadata(e.path()).ok()? } else { e.metadata().ok()? };
                                let kind = if meta.is_dir() { Kind::Dir } else { Kind::File };
                                Some(Entry { name, path: e.path(), kind, size: if meta.is_dir() { 0 } else { meta.len() } })
                            })
                            .collect();
                        rows.sort_by_key(|a| (a.kind != Kind::Dir, a.name.to_lowercase()));
                        entries.extend(rows);
                    }
                    Err(e) => self.error = Some(format!("Cannot open this folder: {e}")),
                }
            }
        }
        self.entries = entries;
        self.cursor = self.cursor.min(self.visible().len().saturating_sub(1));
    }

    /// Indices into `entries` that match the filter (the parent row always stays).
    pub fn visible(&self) -> Vec<usize> {
        let f = self.filter.to_lowercase();
        self.entries.iter().enumerate().filter(|(_, e)| f.is_empty() || e.kind == Kind::Parent || e.name.to_lowercase().contains(&f)).map(|(i, _)| i).collect()
    }

    pub fn current(&self) -> Option<&Entry> {
        self.visible().get(self.cursor).map(|i| &self.entries[*i])
    }

    pub fn move_by(&mut self, delta: isize) {
        let n = self.visible().len();
        if n > 0 {
            self.cursor = (self.cursor as isize + delta).clamp(0, n as isize - 1) as usize;
        }
    }

    pub fn jump(&mut self, end: bool) {
        let n = self.visible().len();
        self.cursor = if end { n.saturating_sub(1) } else { 0 };
    }

    pub fn type_char(&mut self, c: char) {
        self.filter.push(c);
        self.cursor = 0;
        // land on the first real match, not the parent row
        if self.visible().len() > 1 && self.current().is_some_and(|e| e.kind == Kind::Parent) {
            self.cursor = 1;
        }
    }

    pub fn backspace(&mut self) {
        if self.filter.pop().is_some() {
            self.cursor = 0;
        } else {
            self.up();
        }
    }

    /// Is there a folder (or the drive list) above the current one?
    pub fn can_up(&self) -> bool {
        match &self.dir {
            Some(d) => d.parent().is_some() || cfg!(windows),
            None => false,
        }
    }

    pub fn up(&mut self) {
        if let Some(d) = self.dir.clone() {
            match d.parent() {
                Some(p) => {
                    let from = d.file_name().map(|n| n.to_string_lossy().into_owned());
                    self.go(Some(p.to_owned()));
                    if let Some(name) = from {
                        if let Some(pos) = self.visible().iter().position(|i| self.entries[*i].name == name) {
                            self.cursor = pos;
                        }
                    }
                }
                None if cfg!(windows) => self.go(None),
                None => {}
            }
        }
    }

    /// Enter on the current row: open folders / drives, select files.
    pub fn activate(&mut self) {
        let Some(e) = self.current().cloned() else { return };
        match e.kind {
            Kind::Parent => self.up(),
            Kind::Drive | Kind::Dir => self.go(Some(e.path)),
            Kind::File => self.toggle_current(false),
        }
    }

    pub fn toggle_current(&mut self, advance: bool) {
        let Some(e) = self.current().cloned() else { return };
        if matches!(e.kind, Kind::File | Kind::Dir) && !self.selected.remove(&e.path) {
            self.selected.insert(e.path);
        }
        if advance {
            self.move_by(1);
        }
    }

    pub fn select_all_files(&mut self) {
        let paths: Vec<PathBuf> = self.visible().into_iter().map(|i| &self.entries[i]).filter(|e| e.kind == Kind::File).map(|e| e.path.clone()).collect();
        self.selected.extend(paths);
    }

    /// Selected files' total size and the number of selected folders (their size is not counted).
    pub fn selection_summary(&self) -> (usize, u64, usize) {
        let mut files = 0;
        let mut bytes = 0;
        let mut folders = 0;
        for p in &self.selected {
            match std::fs::metadata(p) {
                Ok(m) if m.is_dir() => folders += 1,
                Ok(m) => {
                    files += 1;
                    bytes += m.len();
                }
                Err(_) => {}
            }
        }
        (files, bytes, folders)
    }

    pub fn keep_visible(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + height {
            self.scroll = self.cursor + 1 - height;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        std::fs::write(d.path().join("b.txt"), "hello").unwrap();
        std::fs::write(d.path().join("a.png"), "x").unwrap();
        std::fs::write(d.path().join(".hidden"), "x").unwrap();
        std::fs::write(d.path().join("sub").join("inner.txt"), "x").unwrap();
        d
    }

    #[test]
    fn folders_first_then_files_hidden_skipped() {
        let d = tree();
        let p = Picker::open(d.path());
        let names: Vec<_> = p.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["..", "sub", "a.png", "b.txt"]);
    }

    #[test]
    fn type_to_filter_and_select() {
        let d = tree();
        let mut p = Picker::open(d.path());
        for c in "b.t".chars() {
            p.type_char(c);
        }
        assert_eq!(p.current().unwrap().name, "b.txt");
        p.activate();
        assert_eq!(p.selection_summary(), (1, 5, 0));
        p.backspace();
        assert_eq!(p.filter, "b.");
    }

    #[test]
    fn enter_folder_and_back_remembers_position() {
        let d = tree();
        let mut p = Picker::open(d.path());
        p.cursor = 1;
        p.activate();
        assert_eq!(p.entries.iter().filter(|e| e.kind == Kind::File).count(), 1);
        p.up();
        assert_eq!(p.current().unwrap().name, "sub");
    }

    #[test]
    fn select_all_takes_visible_files_only() {
        let d = tree();
        let mut p = Picker::open(d.path());
        p.select_all_files();
        assert_eq!(p.selected.len(), 2);
        p.toggle_current(false); // the parent row cannot be selected
        assert_eq!(p.selected.len(), 2);
    }
}
