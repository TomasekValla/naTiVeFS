//! Compiles the Slint UI and generates `$OUT_DIR/about.rs`: build information and
//! the list of open-source crates linked into the app (from `cargo metadata`).

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::process::Command;

fn main() {
    // the Slint compiler recurses deeply on the big windows; Windows gives the main thread only 1 MB of stack
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| slint_build::compile("ui/app.slint").expect("failed to compile Slint UI"))
        .expect("spawn the Slint compiler")
        .join()
        .expect("the Slint compiler failed");

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    copy_libmpv(&manifest_dir);
    let root = Path::new(&manifest_dir).join("../..");
    println!("cargo:rerun-if-changed={}", root.join("Cargo.lock").display());
    if root.join(".git/HEAD").exists() {
        println!("cargo:rerun-if-changed={}", root.join(".git/HEAD").display());
    }

    let libs = match dependency_licenses(&manifest_dir) {
        Ok(libs) => libs,
        Err(e) => {
            println!("cargo:warning=open-source library list unavailable: {e}");
            Vec::new()
        }
    };

    let target = std::env::var("TARGET").unwrap_or_default();
    let profile = std::env::var("PROFILE").unwrap_or_default();
    let git = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(&root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default();

    let mut out = String::new();
    out.push_str(&format!("pub const TARGET: &str = {target:?};\n"));
    out.push_str(&format!("pub const PROFILE: &str = {profile:?};\n"));
    out.push_str(&format!("pub const GIT_HASH: &str = {git:?};\n"));
    out.push_str(&format!("pub const BUILD_DATE: &str = {:?};\n", today_utc()));
    out.push_str("/// (name, version, license, source url)\npub const LIBS: &[(&str, &str, &str, &str)] = &[\n");
    for (name, version, license, url) in &libs {
        out.push_str(&format!("    ({name:?}, {version:?}, {license:?}, {url:?}),\n"));
    }
    // not crates: the video player library and the bundled fonts
    for (name, version, license, url) in [
        ("libmpv (video player)", "git e471496c56", "GPL-2.0-or-later", "https://github.com/mpv-player/mpv"),
        ("libmpv Windows build", "20261021", "GPL-2.0-or-later", "https://github.com/shinchiro/mpv-winbuild-cmake"),
        ("Inter, Syne, Space Grotesk, JetBrains Mono, Manrope, Sora, Outfit, Plus Jakarta Sans, Poppins, Lora, Merriweather, PT Serif, EB Garamond, Atkinson Hyperlegible, Lexend, Space Mono, Quicksand, Noto Sans JP (fonts)", "", "OFL-1.1", "https://github.com/google/fonts"),
        ("OpenDyslexic (font)", "0.910.12", "OFL-1.1", "https://github.com/antijingoist/opendyslexic"),
    ] {
        out.push_str(&format!("    ({name:?}, {version:?}, {license:?}, {url:?}),\n"));
    }
    out.push_str("];\n");
    let dest = Path::new(&std::env::var("OUT_DIR").unwrap()).join("about.rs");
    std::fs::write(dest, out).unwrap();
}

/// Normal (non-build, non-dev) dependencies of this package for the target being built.
fn dependency_licenses(manifest_dir: &str) -> Result<Vec<(String, String, String, String)>, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let target = std::env::var("TARGET").map_err(|e| e.to_string())?;
    let run = |offline: bool| {
        let mut cmd = Command::new(&cargo);
        cmd.args(["metadata", "--format-version", "1", "--locked", "--filter-platform", &target]);
        if offline {
            cmd.arg("--offline");
        }
        cmd.current_dir(manifest_dir).output()
    };
    // offline first (everything is normally downloaded by now), online as a fallback
    let output = match run(true) {
        Ok(o) if o.status.success() => o,
        _ => run(false).map_err(|e| e.to_string())?,
    };
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    let meta: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;

    let workspace: HashSet<&str> = meta["workspace_members"]
        .as_array()
        .ok_or("no workspace_members")?
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    let packages: BTreeMap<&str, &serde_json::Value> = meta["packages"]
        .as_array()
        .ok_or("no packages")?
        .iter()
        .filter_map(|p| Some((p["id"].as_str()?, p)))
        .collect();
    let nodes: BTreeMap<&str, &serde_json::Value> = meta["resolve"]["nodes"]
        .as_array()
        .ok_or("no resolve graph")?
        .iter()
        .filter_map(|n| Some((n["id"].as_str()?, n)))
        .collect();
    let me = packages
        .iter()
        .find(|(_, p)| p["name"] == "nativefs-app")
        .map(|(id, _)| *id)
        .ok_or("nativefs-app not found")?;

    // Walk normal dependency edges only (skips build scripts' and tests' deps).
    let mut seen = HashSet::new();
    let mut stack = vec![me];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(node) = nodes.get(id) else { continue };
        for dep in node["deps"].as_array().into_iter().flatten() {
            let normal = dep["dep_kinds"].as_array().into_iter().flatten().any(|k| k["kind"].is_null());
            if let (true, Some(pkg)) = (normal, dep["pkg"].as_str()) {
                stack.push(pkg);
            }
        }
    }

    let mut libs: Vec<_> = seen
        .into_iter()
        .filter(|id| !workspace.contains(id))
        .filter_map(|id| packages.get(id))
        .map(|p| {
            let s = |k: &str| p[k].as_str().unwrap_or_default().to_owned();
            let license = if p["license"].is_string() { s("license") } else { "see source".to_owned() };
            let url = if p["repository"].is_string() { s("repository") } else { format!("https://crates.io/crates/{}", s("name")) };
            (s("name"), s("version"), license.replace(" / ", " OR ").replace('/', " OR "), url)
        })
        .collect();
    libs.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()).then(a.1.cmp(&b.1)));
    libs.dedup();
    Ok(libs)
}

/// `YYYY-MM-DD` (UTC) without extra dependencies.
fn today_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Windows: put `libmpv-2.dll` (fetched by `tools/fetch-libmpv.sh` into `vendor/libmpv/win-<arch>/`) next to the
/// executable, so `cargo run` and the release folder find the video player. Missing DLL = the player shows a
/// "libmpv was not found" message; nothing else breaks.
fn copy_libmpv(manifest_dir: &str) {
    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.contains("windows") {
        return;
    }
    let arch = if target.starts_with("aarch64") { "aarch64" } else { "x86_64" };
    let src = Path::new(manifest_dir).join("../../vendor/libmpv").join(format!("win-{arch}")).join("libmpv-2.dll");
    println!("cargo:rerun-if-changed={}", src.display());
    if !src.exists() {
        println!("cargo:warning=libmpv-2.dll missing ({}): run tools/fetch-libmpv.sh for the video player", src.display());
        return;
    }
    // OUT_DIR = target/<profile>/build/<pkg>/out -> target/<profile>
    let out = std::env::var("OUT_DIR").unwrap();
    let Some(profile_dir) = Path::new(&out).ancestors().nth(3) else { return };
    let dest = profile_dir.join("libmpv-2.dll");
    let same = std::fs::metadata(&dest).ok().map(|m| m.len()) == std::fs::metadata(&src).ok().map(|m| m.len());
    if !same {
        if let Err(e) = std::fs::copy(&src, &dest) {
            println!("cargo:warning=could not copy libmpv-2.dll: {e}");
        }
    }
}
