//! Which links an upload produced and which one is the user's default (the app's "Auto copy" rules).

use std::time::Duration;

use serde::Serialize;
use tvfs_core::config::{AutoCopy, Config};
use tvfs_core::models::*;
use tvfs_core::upload::{FileResult, Outcome, RunReport};
use tvfs_core::Client;

#[derive(Debug, Clone, Copy)]
pub struct Prefs {
    pub auto: AutoCopy,
    pub prefer_tool: bool,
    pub image_styled: bool,
}

impl Prefs {
    pub fn from_config(c: &Config) -> Self {
        Self { auto: c.auto_copy, prefer_tool: c.auto_copy_prefer_tool, image_styled: c.auto_copy_image_styled }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LinkItem {
    pub kind: &'static str,
    #[serde(skip)]
    pub label: &'static str,
    pub url: String,
    pub default: bool,
}

/// The link the app would copy for one finished file (`None` = Auto copy is off). A styled link still being
/// computed falls back to the plain one; callers that can wait check `default_waits_for_styled`.
pub fn default_url(r: &UploadCompleteResponse, p: Prefs) -> Option<String> {
    if p.auto == AutoCopy::Off {
        return None;
    }
    if p.prefer_tool {
        if let Some(plain) = r.tool_link() {
            return Some(if p.auto == AutoCopy::Normal { plain.to_owned() } else { r.tool_styled_link().unwrap_or(plain).to_owned() });
        }
    }
    let styled_mode = if r.kind == "image" { p.image_styled } else { p.auto == AutoCopy::Styled };
    if styled_mode {
        Some(r.styled_link.clone().unwrap_or_else(|| r.link.clone()))
    } else {
        Some(r.link.clone())
    }
}

/// True while the default would be the styled link but the server has not produced it yet.
pub fn default_waits_for_styled(r: &UploadCompleteResponse, p: Prefs) -> bool {
    if p.auto == AutoCopy::Off || r.styled_link.is_some() || !r.styled_link_pending || r.file_id.is_empty() {
        return false;
    }
    if p.prefer_tool && r.tool_link().is_some() {
        return false;
    }
    if r.kind == "image" { p.image_styled } else { p.auto == AutoCopy::Styled }
}

/// Every link of one file, the default marked.
pub fn collect(r: &UploadCompleteResponse, p: Prefs) -> Vec<LinkItem> {
    let mut v: Vec<(&'static str, &'static str, Option<&str>)> = vec![
        ("direct", "Direct link", Some(r.link.as_str())),
        ("styled", "Styled link", r.styled_link.as_deref()),
        ("md_reader", "MD reader", r.md_reader_link.as_deref()),
        ("md_reader_styled", "MD reader (styled)", r.md_reader_styled_link.as_deref()),
        ("text_reader", "Text reader", r.text_reader_link.as_deref()),
        ("text_reader_styled", "Text reader (styled)", r.text_reader_styled_link.as_deref()),
        ("player", "Player", r.audio_player_link.as_deref()),
        ("player_styled", "Player (styled)", r.audio_player_styled_link.as_deref()),
    ];
    v.retain(|(_, _, u)| u.is_some_and(|u| !u.is_empty()));
    let def = default_url(r, p);
    let mut marked = false;
    v.into_iter()
        .map(|(kind, label, url)| {
            let url = url.unwrap_or_default().to_owned();
            // the same URL can appear twice (e.g. no styled variant): mark only the first
            let is_def = !marked && def.as_deref() == Some(url.as_str());
            marked |= is_def;
            LinkItem { kind, label, url, default: is_def }
        })
        .collect()
}

/// One uploaded (or failed) file.
#[derive(Debug, Clone)]
pub struct FileOut {
    pub name: String,
    pub result: Result<UploadCompleteResponse, String>,
}

/// What a finished run produced, independent of single / batch / protected.
#[derive(Debug, Clone, Default)]
pub struct Finished {
    pub files: Vec<FileOut>,
    pub batch_link: Option<String>,
    pub batch_styled: Option<String>,
    /// "TVFS Users Only" share link.
    pub share_link: Option<String>,
    pub failed: Option<String>,
}

impl Finished {
    pub fn from_report(report: RunReport, names: &[(String, u64)]) -> Self {
        let mut out = Self::default();
        match report.outcome {
            Outcome::Single(f) => out.files.push(file_out(f)),
            Outcome::Batch { files, batch } => {
                let batch = batch.ok();
                for f in files {
                    let mut fo = file_out(f);
                    if let (Ok(r), Some(b)) = (&mut fo.result, &batch) {
                        if r.styled_link.is_none() {
                            r.styled_link = b.files.iter().find(|x| x.link == r.link).and_then(|x| x.styled_link.clone());
                        }
                    }
                    out.files.push(fo);
                }
                if let Some(b) = batch {
                    out.batch_link = Some(if b.link.is_empty() { b.landing_page.clone().unwrap_or_default() } else { b.link.clone() });
                    out.batch_styled = b.styled_link;
                }
            }
            Outcome::Protected(r) => match r {
                Ok(share) => {
                    out.share_link = Some(share.link.clone());
                    for (i, (name, size)) in names.iter().enumerate() {
                        let tool = share.files.iter().find(|f| f.index as usize == i).and_then(|f| f.tool_link.clone());
                        let r = UploadCompleteResponse { link: share.link.clone(), size: *size, filename: name.clone(), md_reader_link: tool, ..Default::default() };
                        out.files.push(FileOut { name: name.clone(), result: Ok(r) });
                    }
                }
                Err(e) => out.failed = Some(e),
            },
            Outcome::Failed(e) => out.failed = Some(e),
        }
        out
    }

    pub fn ok_bytes(&self) -> u64 {
        self.files.iter().filter_map(|f| f.result.as_ref().ok()).map(|r| r.size).sum()
    }

    pub fn any_failed(&self) -> bool {
        self.failed.is_some() || self.files.iter().any(|f| f.result.is_err())
    }

    pub fn any_ok(&self) -> bool {
        self.files.iter().any(|f| f.result.is_ok())
    }
}

fn file_out(f: FileResult) -> FileOut {
    FileOut { name: f.name, result: f.result }
}

/// Poll `GET /api/upload/styled-link/:id` like the app (once a second, up to ~40 s).
pub async fn poll_styled(client: &Client, file_id: &str) -> Option<String> {
    for _ in 0..41 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Ok(st) = client.styled_link(file_id).await {
            if st.ready {
                return st.styled_link;
            }
        }
    }
    None
}

/// Wait for every pending styled link (agent mode: the answer must be complete).
pub async fn resolve_pending(client: &Client, fin: &mut Finished) {
    let mut jobs = Vec::new();
    for (i, f) in fin.files.iter().enumerate() {
        if let Ok(r) = &f.result {
            if r.styled_link_pending && r.styled_link.is_none() && !r.file_id.is_empty() {
                let (c, id) = (client.clone(), r.file_id.clone());
                jobs.push((i, tokio::spawn(async move { poll_styled(&c, &id).await })));
            }
        }
    }
    for (i, job) in jobs {
        if let (Ok(Some(link)), Ok(r)) = (job.await, &mut fin.files[i].result) {
            r.styled_link = Some(link);
            r.styled_link_pending = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp() -> UploadCompleteResponse {
        UploadCompleteResponse {
            link: "https://x/f/a.png".into(),
            kind: "file".into(),
            styled_link: Some("[a](https://x/f/a.png)".into()),
            ..Default::default()
        }
    }

    fn prefs(auto: AutoCopy) -> Prefs {
        Prefs { auto, prefer_tool: true, image_styled: false }
    }

    #[test]
    fn auto_copy_modes_pick_the_link() {
        let r = resp();
        assert_eq!(default_url(&r, prefs(AutoCopy::Off)), None);
        assert_eq!(default_url(&r, prefs(AutoCopy::Normal)).as_deref(), Some("https://x/f/a.png"));
        assert_eq!(default_url(&r, prefs(AutoCopy::Styled)).as_deref(), Some("[a](https://x/f/a.png)"));
    }

    #[test]
    fn images_follow_their_own_switch() {
        let mut r = resp();
        r.kind = "image".into();
        assert_eq!(default_url(&r, prefs(AutoCopy::Styled)).as_deref(), Some("https://x/f/a.png"));
        let p = Prefs { image_styled: true, ..prefs(AutoCopy::Normal) };
        assert_eq!(default_url(&r, p).as_deref(), Some("[a](https://x/f/a.png)"));
    }

    #[test]
    fn tool_link_wins_when_preferred() {
        let mut r = resp();
        r.md_reader_link = Some("https://x/r/a".into());
        r.md_reader_styled_link = Some("[a](https://x/r/a)".into());
        assert_eq!(default_url(&r, prefs(AutoCopy::Normal)).as_deref(), Some("https://x/r/a"));
        assert_eq!(default_url(&r, prefs(AutoCopy::Styled)).as_deref(), Some("[a](https://x/r/a)"));
        let p = Prefs { prefer_tool: false, ..prefs(AutoCopy::Normal) };
        assert_eq!(default_url(&r, p).as_deref(), Some("https://x/f/a.png"));
    }

    #[test]
    fn collect_marks_exactly_one_default() {
        let mut r = resp();
        r.audio_player_link = Some("https://x/p/a".into());
        let items = collect(&r, prefs(AutoCopy::Styled));
        assert_eq!(items.iter().filter(|i| i.default).count(), 1);
        assert!(items.iter().any(|i| i.kind == "player"));
        assert!(collect(&r, prefs(AutoCopy::Off)).iter().all(|i| !i.default));
    }

    #[test]
    fn pending_styled_is_detected() {
        let mut r = resp();
        r.styled_link = None;
        r.styled_link_pending = true;
        r.file_id = "id1".into();
        assert!(default_waits_for_styled(&r, prefs(AutoCopy::Styled)));
        assert!(!default_waits_for_styled(&r, prefs(AutoCopy::Normal)));
    }
}
