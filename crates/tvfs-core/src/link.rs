//! Decide what a link means. Accepts `https://files.tomasekvalla.cz/files/<dir>/<name>`
//! and `tvfs://open?url=<percent-encoded https link>`.

use percent_encoding::percent_decode_str;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    Video,
    Audio,
    Text,
    Image,
    Download,
    Batch,
    /// Generated TVFS HTML page (audio player, text/MD reader, ...).
    Player,
    Other,
}

impl LinkKind {
    fn from_dir(dir: &str) -> Self {
        match dir {
            "videos" => Self::Video,
            "audio" => Self::Audio,
            "text" => Self::Text,
            "pictures" => Self::Image,
            "download" => Self::Download,
            "batch" => Self::Batch,
            "players" => Self::Player,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRequest {
    /// Canonical https link on the TVFS host.
    pub url: Url,
    pub kind: LinkKind,
    /// Decoded last path segment.
    pub file_name: Option<String>,
}

pub fn is_tvfs_host(url: &Url) -> bool {
    let Ok(base) = Url::parse(crate::base_url()) else { return false };
    url.host_str().is_some_and(|h| Some(h) == base.host_str())
}

/// `None` = not a TVFS link (the link router should hand it to the browser).
pub fn parse_open_request(raw: &str) -> Option<OpenRequest> {
    let parsed = Url::parse(raw.trim()).ok()?;
    let https = match parsed.scheme() {
        s if s == crate::URL_SCHEME => {
            let inner = parsed.query_pairs().find(|(k, _)| k == "url")?.1.into_owned();
            Url::parse(&inner).ok()?
        }
        "https" | "http" => parsed,
        _ => return None,
    };
    if !is_tvfs_host(&https) || !matches!(https.scheme(), "https" | "http") {
        return None;
    }
    let segments: Vec<&str> = https.path_segments()?.filter(|s| !s.is_empty()).collect();
    let (kind, file_name) = match segments.as_slice() {
        ["files", dir, .., last] => (LinkKind::from_dir(dir), Some(decode(last))),
        ["files", dir] => (LinkKind::from_dir(dir), None),
        _ => (LinkKind::Other, segments.last().map(|s| decode(s))),
    };
    Some(OpenRequest { url: https, kind, file_name })
}

fn decode(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_video() {
        let r = parse_open_request("https://files.tomasekvalla.cz/files/videos/example%20clip.mp4").unwrap();
        assert_eq!(r.kind, LinkKind::Video);
        assert_eq!(r.file_name.as_deref(), Some("example clip.mp4"));
    }

    #[test]
    fn custom_scheme() {
        let enc = "https%3A%2F%2Ffiles.tomasekvalla.cz%2Ffiles%2Faudio%2Fsong.flac";
        let r = parse_open_request(&format!("tvfs://open?url={enc}")).unwrap();
        assert_eq!(r.kind, LinkKind::Audio);
        assert_eq!(r.url.scheme(), "https");
    }

    #[test]
    fn other_hosts_are_not_ours() {
        assert!(parse_open_request("https://example.com/files/videos/x.mp4").is_none());
        assert!(parse_open_request("tvfs://open?url=https%3A%2F%2Fexample.com%2Fa").is_none());
        assert!(parse_open_request("ftp://files.tomasekvalla.cz/x").is_none());
    }

    #[test]
    fn players_and_unknown_paths() {
        let p = parse_open_request("https://files.tomasekvalla.cz/files/players/track.html").unwrap();
        assert_eq!(p.kind, LinkKind::Player);
        let o = parse_open_request("https://files.tomasekvalla.cz/upload.html").unwrap();
        assert_eq!(o.kind, LinkKind::Other);
    }
}
