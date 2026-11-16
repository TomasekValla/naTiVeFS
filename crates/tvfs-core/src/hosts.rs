//! Which addresses belong to TVFS, and the one check every URL from outside goes through.
//!
//! TVFS moves from `files.tomasekvalla.cz` to `tvfs.tomasekvalla.cz` (pages and API) and
//! `tvfs-usercontent.tomasekvalla.cz` (uploaded files); old links keep working. Every URL that comes
//! from a link, a generated page, the sync list or a share becomes a [`TrustedUrl`] first: https on a
//! TVFS host, default port, no user info. Anything else is not TVFS and is never fetched with the
//! session or treated as a TVFS file.

use std::path::PathBuf;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};
use url::Url;

/// Every host TVFS has used: the old address, the page/API host and the user-content host.
pub const KNOWN_HOSTS: [&str; 3] = ["files.tomasekvalla.cz", "tvfs.tomasekvalla.cz", "tvfs-usercontent.tomasekvalla.cz"];

/// Hosts that serve the TVFS pages and the API (old and new address). The login cookie belongs to these, never to the
/// user-content host.
pub const PAGE_HOSTS: [&str; 2] = ["files.tomasekvalla.cz", "tvfs.tomasekvalla.cz"];

/// Where the session cookie is kept: the API base, plus the other TVFS page host when the base is one of them (a link
/// to a Users Only file may name either). A local test server gets only itself.
pub fn session_origins(base: &Url) -> Vec<Url> {
    let mut out = vec![base.clone()];
    let base_host = base.host_str().unwrap_or("").to_ascii_lowercase();
    if PAGE_HOSTS.contains(&base_host.as_str()) {
        for h in PAGE_HOSTS {
            if h != base_host {
                if let Ok(u) = Url::parse(&format!("https://{h}/")) {
                    out.push(u);
                }
            }
        }
    }
    out
}

/// Hosts the server announces later (`GET /api/hosts`) are accepted only under this domain.
const SERVER_HOST_SUFFIX: &str = ".tomasekvalla.cz";

static SERVER_HOSTS: RwLock<Vec<String>> = RwLock::new(Vec::new());

/// Adds the hosts the server announced. Names outside the TVFS domain or with odd characters are ignored.
pub fn set_server_hosts<S: AsRef<str>>(hosts: &[S]) {
    let clean: Vec<String> = hosts
        .iter()
        .map(|h| h.as_ref().trim().to_ascii_lowercase())
        .filter(|h| {
            h.ends_with(SERVER_HOST_SUFFIX)
                && h.len() <= 100
                && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
                && !h.contains("..")
        })
        .collect();
    if let Ok(mut w) = SERVER_HOSTS.write() {
        *w = clean;
    }
}

/// What `GET /api/hosts` says: `page` = upload page, generated pages and API; `content` = uploaded files;
/// `known` = every TVFS host, old and new.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HostsInfo {
    pub page: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub known: Vec<String>,
}

fn cache_file() -> Option<PathBuf> {
    crate::config::Config::data_dir().ok().map(|d| d.join("hosts.json"))
}

/// `https://<host>` when `page` is a TVFS page host the app may use as its API address (a known page host or one the
/// server announced under the TVFS domain); never anything else, so the server cannot send the app elsewhere.
/// Does not call [`crate::base_url`] (it runs while that is being set up).
fn accepted_page(page: &str) -> Option<String> {
    let u = Url::parse(page.trim()).ok()?;
    let host = u.host_str()?.to_ascii_lowercase();
    let ok_host = PAGE_HOSTS.contains(&host.as_str()) || SERVER_HOSTS.read().is_ok_and(|v| v.contains(&host));
    let clean = u.scheme() == "https" && u.port().is_none() && u.username().is_empty() && u.password().is_none();
    (ok_host && clean && host != KNOWN_HOSTS[2]).then(|| format!("https://{host}"))
}

/// The API address remembered from the last [`refresh`] (read once at start).
pub(crate) fn cached_page_base() -> Option<String> {
    let text = std::fs::read_to_string(cache_file()?).ok()?;
    let info: HostsInfo = serde_json::from_str(&text).ok()?;
    set_server_hosts(&info.known);
    accepted_page(&info.page)
}

/// Asks the server where TVFS lives now, learns its hosts and remembers the page/API address for the next start
/// (the running process keeps its address, so a session is never split between two hosts). Returns true when the
/// next start will use a different address.
pub async fn refresh(client: &crate::Client) -> crate::Result<bool> {
    let info = client.hosts().await?;
    set_server_hosts(&info.known);
    let Some(page) = accepted_page(&info.page) else { return Ok(false) };
    if let Some(path) = cache_file() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&path, serde_json::to_string(&info).unwrap_or_default());
    }
    Ok(page.trim_end_matches('/') != crate::base_url().trim_end_matches('/'))
}

/// The host of the build-time base address (only differs from the known hosts in test builds).
fn base() -> Option<Url> {
    Url::parse(crate::base_url()).ok()
}

/// True for a host name that is TVFS (any of its addresses, old or new).
pub fn is_tvfs_host_name(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    KNOWN_HOSTS.contains(&h.as_str())
        || base().is_some_and(|b| b.host_str().is_some_and(|bh| bh.eq_ignore_ascii_case(&h)))
        || SERVER_HOSTS.read().is_ok_and(|v| v.iter().any(|x| *x == h))
}

/// What a URL may point at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Allow {
    /// Any page on a TVFS host (a link the user opened).
    AnyPath,
    /// Only a TVFS file: `/files/...` or `/api/protected/...` (a URL read from a page or a list).
    File,
}

/// A URL checked to be TVFS: https, a TVFS host, default port, no user info (and for [`Allow::File`]
/// a file path). Test builds whose base address is a local server also accept that server as it is.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrustedUrl(Url);

impl TrustedUrl {
    /// A link the user opened: any path on a TVFS host. `http://` is upgraded to `https://`.
    pub fn link(raw: &str) -> Option<Self> {
        Self::from_url(Url::parse(raw.trim()).ok()?, Allow::AnyPath)
    }

    /// A file URL taken from a page, a sync list or a share; relative URLs are resolved against `page`.
    pub fn file(raw: &str, page: Option<&Url>) -> Option<Self> {
        let raw = raw.trim();
        let url = match Url::parse(raw) {
            Ok(u) => u,
            Err(url::ParseError::RelativeUrlWithoutBase) => page?.join(raw).ok()?,
            Err(_) => return None,
        };
        Self::from_url(url, Allow::File)
    }

    pub fn from_url(mut u: Url, allow: Allow) -> Option<Self> {
        if !u.username().is_empty() || u.password().is_some() || u.cannot_be_a_base() {
            return None;
        }
        let host = u.host_str()?.to_ascii_lowercase();
        let local_base = base().filter(|b| {
            b.host_str().is_some_and(|bh| bh.eq_ignore_ascii_case(&host)) && !KNOWN_HOSTS.contains(&host.as_str())
        });
        if let Some(b) = local_base {
            // a test server is used exactly as configured (scheme and port included)
            if u.scheme() != b.scheme() || u.port_or_known_default() != b.port_or_known_default() {
                return None;
            }
        } else {
            if !is_tvfs_host_name(&host) {
                return None;
            }
            match u.scheme() {
                "https" => {}
                // an http link is opened as https; never on another port (the session must not leave TLS)
                "http" if u.port().is_none() => u.set_scheme("https").ok()?,
                _ => return None,
            }
            if u.port().is_some_and(|p| p != 443) {
                return None;
            }
            u.set_port(None).ok()?;
        }
        if allow == Allow::File && !is_file_path(u.path()) {
            return None;
        }
        Some(Self(u))
    }

    pub fn as_url(&self) -> &Url {
        &self.0
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn into_url(self) -> Url {
        self.0
    }

    /// A Users Only file (needs the session cookie).
    pub fn is_protected(&self) -> bool {
        self.0.path().starts_with("/api/protected/")
    }
}

impl std::fmt::Display for TrustedUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.as_str().fmt(f)
    }
}

fn is_file_path(path: &str) -> bool {
    (path.starts_with("/files/") || path.starts_with("/api/protected/")) && !path.contains("/../")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_three_hosts_are_tvfs() {
        for h in KNOWN_HOSTS {
            let u = format!("https://{h}/files/audio/a.flac");
            assert!(TrustedUrl::file(&u, None).is_some(), "{u}");
        }
        assert!(TrustedUrl::link("https://example.com/files/a").is_none());
        assert!(TrustedUrl::link("https://files.tomasekvalla.cz.evil.com/files/a").is_none());
        assert!(TrustedUrl::link("https://evilfiles.tomasekvalla.cz/files/a").is_none());
    }

    #[test]
    fn http_becomes_https_but_never_another_port_or_user_info() {
        let u = TrustedUrl::link("http://files.tomasekvalla.cz/files/videos/a.mp4").unwrap();
        assert_eq!(u.as_str(), "https://files.tomasekvalla.cz/files/videos/a.mp4");
        assert!(TrustedUrl::link("http://files.tomasekvalla.cz:81/api/protected/x/file/0").is_none());
        assert!(TrustedUrl::link("https://files.tomasekvalla.cz:8443/files/a").is_none());
        assert_eq!(TrustedUrl::link("https://tvfs.tomasekvalla.cz:443/upload.html").unwrap().as_str(), "https://tvfs.tomasekvalla.cz/upload.html");
        assert!(TrustedUrl::link("https://user:pw@files.tomasekvalla.cz/files/a").is_none());
    }

    #[test]
    fn file_urls_from_pages_must_be_tvfs_files() {
        for bad in [
            "file:///C:/Windows/win.ini",
            "file://attacker-host/share/x.md",
            "http://192.168.1.1/x",
            "http://127.0.0.1:8080/files/a",
            "https://tvfs.tomasekvalla.cz/upload.html",
            "https://tvfs.tomasekvalla.cz/admin.html",
            "javascript:alert(1)",
            "data:text/plain,hi",
        ] {
            assert!(TrustedUrl::file(bad, None).is_none(), "{bad}");
        }
        let page = Url::parse("https://tvfs.tomasekvalla.cz/files/text/r.html").unwrap();
        let rel = TrustedUrl::file("/files/text/notes.md", Some(&page)).unwrap();
        assert_eq!(rel.as_str(), "https://tvfs.tomasekvalla.cz/files/text/notes.md");
        assert!(TrustedUrl::file("https://tvfs.tomasekvalla.cz/api/protected/ab/file/0", None).unwrap().is_protected());
    }

    #[test]
    fn the_api_address_is_only_ever_a_tvfs_page_host() {
        assert_eq!(accepted_page("https://tvfs.tomasekvalla.cz").as_deref(), Some("https://tvfs.tomasekvalla.cz"));
        assert_eq!(accepted_page("https://files.tomasekvalla.cz/").as_deref(), Some("https://files.tomasekvalla.cz"));
        for bad in ["https://tvfs-usercontent.tomasekvalla.cz", "https://evil.com", "http://tvfs.tomasekvalla.cz",
            "https://tvfs.tomasekvalla.cz:8443", "https://u:p@tvfs.tomasekvalla.cz", "garbage"] {
            assert!(accepted_page(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn server_hosts_only_under_the_tvfs_domain() {
        set_server_hosts(&["new.tomasekvalla.cz", "evil.com", "x.tomasekvalla.cz.evil.com", "bad host.tomasekvalla.cz"]);
        assert!(is_tvfs_host_name("new.tomasekvalla.cz"));
        assert!(!is_tvfs_host_name("evil.com"));
        assert!(!is_tvfs_host_name("x.tomasekvalla.cz.evil.com"));
        set_server_hosts::<&str>(&[]);
        assert!(!is_tvfs_host_name("new.tomasekvalla.cz"));
    }
}
