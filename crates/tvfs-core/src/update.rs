//! Update checks against GitHub Releases of `TomasekValla/naTiVeFS`.
//!
//! Releases are signed with a free minisign key (not an OS code-signing
//! certificate). The public key below is baked into the app; until it is set,
//! every update is refused (fail closed).

use std::path::Path;

use minisign_verify::{PublicKey, Signature};
use semver::Version;
use serde::Deserialize;

use crate::error::{Error, Result};

pub const GITHUB_REPO: &str = "TomasekValla/naTiVeFS";
/// Base64 public key from `minisign.pub` (the line starting with `RW`).
pub const UPDATE_PUBLIC_KEY: &str = "RWSQa45JZ1TSMKJ+gVjv+fsOf40z6FcRFMCMz2CplMxl40yszLg0URRu";

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    pub version: Version,
    pub notes: String,
    pub page_url: String,
    pub asset_name: String,
    pub asset_url: String,
    pub signature_url: String,
}

/// Release asset name for the platform this binary was built for.
pub fn platform_asset_name() -> Option<&'static str> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("naTiVeFS-linux-x64-portable.tar.gz")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("naTiVeFS-linux-arm64-portable.tar.gz")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("naTiVeFS-windows-x64-setup.exe")
    } else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
        Some("naTiVeFS-windows-arm64-setup.exe")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("naTiVeFS-macos-apple-silicon.dmg")
    } else {
        None
    }
}

pub fn parse_tag(tag: &str) -> Option<Version> {
    Version::parse(tag.trim().trim_start_matches('v')).ok()
}

fn http() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("naTiVeFS/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(60))
        .build()?)
}

/// Result of an update check.
#[derive(Debug, Clone)]
pub enum CheckOutcome {
    /// A newer release with a signed build for this platform exists.
    Available(ReleaseInfo),
    /// The newest release on the channel is not newer than this build.
    UpToDate { latest: Version },
    /// Newer releases exist but none carries a build for this platform
    /// (e.g. the old pre-rewrite naTiVeFS releases).
    NoBuild { tag: String },
    /// The repository has no (matching) releases at all.
    NoReleases,
    /// This target has no release builds.
    Unsupported,
}

fn info_from(rel: GhRelease, version: Version, asset_name: &str) -> Option<ReleaseInfo> {
    let sig_name = format!("{asset_name}.minisig");
    let find = |n: &str| rel.assets.iter().find(|a| a.name == n).map(|a| a.browser_download_url.clone());
    let (asset_url, signature_url) = (find(asset_name)?, find(&sig_name)?);
    Some(ReleaseInfo {
        version,
        notes: rel.body.unwrap_or_default(),
        page_url: rel.html_url,
        asset_name: asset_name.to_owned(),
        asset_url,
        signature_url,
    })
}

/// Pick the outcome from a release list (newest first, as GitHub returns it).
fn decide(releases: Vec<GhRelease>, current: &Version, beta: bool, asset_name: &str) -> CheckOutcome {
    let mut candidates: Vec<(Version, GhRelease)> = releases
        .into_iter()
        .filter(|r| !r.draft && (beta || !r.prerelease))
        .filter_map(|r| parse_tag(&r.tag_name).map(|v| (v, r)))
        .collect();
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    let Some(newest) = candidates.first().map(|(v, _)| v.clone()) else { return CheckOutcome::NoReleases };
    let mut newer_without_build = None;
    for (v, r) in candidates {
        if v <= *current {
            break;
        }
        let tag = r.tag_name.clone();
        if let Some(info) = info_from(r, v, asset_name) {
            return CheckOutcome::Available(info);
        }
        newer_without_build.get_or_insert(tag);
    }
    match newer_without_build {
        Some(tag) => CheckOutcome::NoBuild { tag },
        None => CheckOutcome::UpToDate { latest: newest },
    }
}

/// Ask GitHub for the newest release on the given channel.
pub async fn check(current: &str, beta: bool) -> Result<CheckOutcome> {
    let current = parse_tag(current).ok_or_else(|| Error::Update(format!("bad current version {current}")))?;
    let Some(asset_name) = platform_asset_name() else { return Ok(CheckOutcome::Unsupported) };
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases?per_page=30");
    let releases: Vec<GhRelease> = http()?
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(decide(releases, &current, beta, asset_name))
}

/// `Ok(None)` = up to date (or no build for this platform).
pub async fn check_latest(current: &str) -> Result<Option<ReleaseInfo>> {
    Ok(match check(current, false).await? {
        CheckOutcome::Available(info) => Some(info),
        _ => None,
    })
}

pub fn verify_signature(data: &[u8], signature_text: &str, public_key_b64: &str) -> Result<()> {
    verify_signed(data, signature_text, public_key_b64).map(|_| ())
}

/// Checks the signature and returns the signed (trusted) comment.
fn verify_signed(data: &[u8], signature_text: &str, public_key_b64: &str) -> Result<String> {
    if public_key_b64.is_empty() {
        return Err(Error::Update("no update public key configured; refusing to install".into()));
    }
    let pk = PublicKey::from_base64(public_key_b64).map_err(|e| Error::Update(format!("bad public key: {e}")))?;
    let sig = Signature::decode(signature_text).map_err(|e| Error::Update(format!("bad signature file: {e}")))?;
    pk.verify(data, &sig, false).map_err(|e| Error::Update(format!("signature check failed: {e}")))?;
    Ok(sig.trusted_comment().to_owned())
}

/// The signed comment must name this exact file and the offered version, and that version must be newer than the
/// running one: a genuinely signed older build or another signed file cannot be installed as an update.
/// (Releases sign with `-t "timestamp:<t> file:<asset> version:<tag>"`, see release.yml.)
pub fn check_signed_comment(comment: &str, asset_name: &str, offered: &Version, current: &Version) -> Result<()> {
    let tokens: Vec<&str> = comment.split_whitespace().collect();
    if !tokens.contains(&format!("file:{asset_name}").as_str()) {
        return Err(Error::Update("the signature is for a different file".into()));
    }
    let signed = tokens
        .iter()
        .find_map(|t| t.strip_prefix("version:"))
        .and_then(parse_tag)
        .ok_or_else(|| Error::Update("the signature does not name a version".into()))?;
    if signed != *offered {
        return Err(Error::Update(format!("the signature is for version {signed}, not {offered}")));
    }
    if signed <= *current {
        return Err(Error::Update(format!("version {signed} is not newer than this one ({current})")));
    }
    Ok(())
}

/// Signature + signed comment of a release file (see [`check_signed_comment`]).
pub fn verify_release(data: &[u8], signature_text: &str, info: &ReleaseInfo, current: &str) -> Result<()> {
    let current = parse_tag(current).ok_or_else(|| Error::Update(format!("bad current version {current}")))?;
    let comment = verify_signed(data, signature_text, UPDATE_PUBLIC_KEY)?;
    check_signed_comment(&comment, &info.asset_name, &info.version, &current)
}

/// Download the release file and its signature, verify both (see [`verify_release`]), then write `dest` as a new file
/// (an existing file or link there is an error). Returns the signature text, so the file can be checked again right
/// before it runs. `progress` gets the bytes so far and the total size when the server tells it.
pub async fn download_verified(
    info: &ReleaseInfo,
    dest: &Path,
    current: &str,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<String> {
    // no overall time limit here (a large file on a slow line), only for a connection that stops answering
    let client = reqwest::Client::builder()
        .user_agent(concat!("naTiVeFS/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(30))
        .read_timeout(std::time::Duration::from_secs(60))
        .build()?;
    let mut response = client.get(&info.asset_url).send().await?.error_for_status()?;
    let total = response.content_length();
    let mut data = Vec::with_capacity(total.unwrap_or(0) as usize);
    while let Some(chunk) = response.chunk().await? {
        data.extend_from_slice(&chunk);
        progress(data.len() as u64, total);
    }
    let sig = client.get(&info.signature_url).send().await?.error_for_status()?.text().await?;
    verify_release(&data, &sig, info, current)?;
    let mut f = tokio::fs::OpenOptions::new().write(true).create_new(true).open(dest).await?;
    tokio::io::AsyncWriteExt::write_all(&mut f, &data).await?;
    tokio::io::AsyncWriteExt::flush(&mut f).await?;
    Ok(sig)
}

/// The downloaded file again, right before it is run or unpacked: what lies on disk now must still be the signed file.
pub fn reverify_file(path: &Path, signature_text: &str, info: &ReleaseInfo, current: &str) -> Result<()> {
    let data = std::fs::read(path)?;
    verify_release(&data, signature_text, info, current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags() {
        assert_eq!(parse_tag("v1.2.3"), Some(Version::new(1, 2, 3)));
        assert!(parse_tag("v1.2.3") > parse_tag("1.2.2"));
        assert!(parse_tag("nightly").is_none());
    }

    fn rel(tag: &str, pre: bool, assets: &[&str]) -> GhRelease {
        GhRelease {
            tag_name: tag.into(),
            html_url: format!("https://example/{tag}"),
            body: None,
            draft: false,
            prerelease: pre,
            assets: assets
                .iter()
                .map(|n| GhAsset { name: (*n).into(), browser_download_url: format!("https://example/{n}") })
                .collect(),
        }
    }

    #[test]
    fn decide_outcomes() {
        let a = "naTiVeFS-windows-x64.zip";
        let sig = "naTiVeFS-windows-x64.zip.minisig";
        let cur = Version::new(0, 1, 0);
        // only old-app releases without our assets
        let old = vec![rel("v1.1.2", false, &["old.exe"]), rel("v1.1.0", true, &[])];
        assert!(matches!(decide(old, &cur, false, a), CheckOutcome::NoBuild { tag } if tag == "v1.1.2"));
        assert!(matches!(decide(vec![], &cur, false, a), CheckOutcome::NoReleases));
        // pre-release is only offered on the beta channel
        let pre = vec![rel("v0.9.1", true, &[a, sig])];
        assert!(matches!(decide(pre, &cur, false, a), CheckOutcome::NoReleases));
        let pre = vec![rel("v0.9.1", true, &[a, sig])];
        assert!(matches!(decide(pre, &cur, true, a), CheckOutcome::Available(i) if i.version == Version::new(0, 9, 1)));
        // a signature is required
        let unsigned = vec![rel("v1.0.0", false, &[a])];
        assert!(matches!(decide(unsigned, &cur, false, a), CheckOutcome::NoBuild { .. }));
        let same = vec![rel("v0.1.0", false, &[a, sig])];
        assert!(matches!(decide(same, &cur, false, a), CheckOutcome::UpToDate { .. }));
    }

    #[test]
    fn signed_comment_must_name_file_and_a_newer_offered_version() {
        let v = |s: &str| parse_tag(s).unwrap();
        let ok = "timestamp:1700000000	file:naTiVeFS-windows-x64-setup.exe	version:v1.2.0";
        assert!(check_signed_comment(ok, "naTiVeFS-windows-x64-setup.exe", &v("v1.2.0"), &v("v1.1.0")).is_ok());
        // another file signed with the same key
        assert!(check_signed_comment(ok, "naTiVeFS-linux-x64-portable.tar.gz", &v("v1.2.0"), &v("v1.1.0")).is_err());
        // an old genuinely signed build under a newer tag (downgrade)
        let old = "timestamp:1	file:naTiVeFS-windows-x64-setup.exe	version:v1.0.0";
        assert!(check_signed_comment(old, "naTiVeFS-windows-x64-setup.exe", &v("v1.3.0"), &v("v1.1.0")).is_err());
        // not newer than the running version
        assert!(check_signed_comment(ok, "naTiVeFS-windows-x64-setup.exe", &v("v1.2.0"), &v("v1.2.0")).is_err());
        // old signatures without a version
        let legacy = "timestamp:1	file:naTiVeFS-windows-x64-setup.exe	hashed";
        assert!(check_signed_comment(legacy, "naTiVeFS-windows-x64-setup.exe", &v("v1.2.0"), &v("v1.1.0")).is_err());
        // a prefix of the file name is not the file
        let pre = "file:naTiVeFS-windows-x64-setup.exe.bak version:v1.2.0";
        assert!(check_signed_comment(pre, "naTiVeFS-windows-x64-setup.exe", &v("v1.2.0"), &v("v1.1.0")).is_err());
    }

    #[test]
    fn fails_closed_without_key() {
        assert!(verify_signature(b"data", "sig", "").is_err());
    }
}
