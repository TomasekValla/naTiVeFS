//! Non-interactive mode for AI agents. Never touches the terminal UI, never asks for credentials.
//!
//! Step 1 (no `--confirm`): prints a summary, the public-upload notice and a one-time code, uploads nothing.
//! Step 2 (`--confirm <CODE>`): checks the code and uploads.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use tvfs_core::config::Config;
use tvfs_core::fmt::{fmt_bytes, fmt_speed_mbs, fmt_time};
use tvfs_core::upload::{Control, Run, UploadRequest};

use crate::args::AgentArgs;
use crate::common::*;
use crate::confirm;
use crate::expiry;
use crate::links::{self, Finished, Prefs};

pub const EXIT_OK: i32 = 0;
pub const EXIT_FAILED: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_NOT_LOGGED_IN: i32 = 3;
pub const EXIT_BAD_CODE: i32 = 4;

pub const NOT_LOGGED_IN: &str = "Not logged in. Open naTiVeFS or run naTiVeFS-cli once to log in.";

pub fn notice(count: usize, size: &str, protected: bool) -> String {
    if protected {
        format!(
            "You are about to upload {count} file(s) ({size}) to TVFS as a \"TVFS Users Only\" share: every logged-in TVFS user with the link can open them. Never upload secrets, credentials, keys or personal data unless the user explicitly asked for exactly this file. By running the command again with `--confirm <CODE>` you, the AI agent, confirm that you understand this and that the user wants this upload."
        )
    } else {
        format!(
            "You are about to upload {count} file(s) ({size}) to the PUBLIC internet (TVFS). Anyone with the link can open them. Never upload secrets, credentials, keys or personal data unless the user explicitly asked for exactly this file. By running the command again with `--confirm <CODE>` you, the AI agent, confirm that you understand this and that the user wants this upload."
        )
    }
}

fn fail(json_mode: bool, code: i32, status: &str, msg: &str) -> i32 {
    if json_mode {
        println!("{}", json!({ "status": status, "error": msg }));
    } else {
        eprintln!("{msg}");
    }
    code
}

pub fn run(a: AgentArgs) -> i32 {
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => return fail(a.json, EXIT_FAILED, "error", &e.to_string()),
    };
    rt.block_on(run_async(a))
}

async fn run_async(a: AgentArgs) -> i32 {
    let (files, problems) = collect_files(&a.paths);
    if !problems.is_empty() {
        return fail(a.json, EXIT_USAGE, "invalid_files", &problems.join("\n"));
    }
    if files.is_empty() {
        return fail(a.json, EXIT_USAGE, "invalid_files", "No files to upload (the folders are empty or hold only files without an extension).");
    }
    let cfg = Config::load();
    let protected = cfg.tvfs_users_only;

    let (client, _user) = match client_with_saved_session() {
        Ok(Some(c)) => c,
        Ok(None) => return fail(a.json, EXIT_NOT_LOGGED_IN, "not_logged_in", NOT_LOGGED_IN),
        Err(e) => return fail(a.json, EXIT_FAILED, "error", &format!("Could not read the saved login: {e}")),
    };
    let status = match client.status().await {
        Ok(s) => s,
        Err(e) if e.is_auth() => return fail(a.json, EXIT_NOT_LOGGED_IN, "not_logged_in", NOT_LOGGED_IN),
        Err(e) => return fail(a.json, EXIT_FAILED, "error", &format!("Cannot reach TVFS: {e}")),
    };

    let total: u64 = files.iter().map(|f| f.size).sum();
    let binding = confirm::binding(&files, a.expire_min, protected);
    let dir = match Config::data_dir() {
        Ok(d) => d,
        Err(e) => return fail(a.json, EXIT_FAILED, "error", &e.to_string()),
    };

    let Some(code) = a.confirm.as_deref() else {
        return step_one(&a, &files, total, &binding, &dir, protected);
    };
    if let Err(e) = confirm::consume(&dir, code, &binding, now_ms()) {
        return fail(a.json, EXIT_BAD_CODE, "bad_confirmation", &e.to_string());
    }
    upload(&a, files, client, &cfg, status.tier == 2, protected).await
}

fn step_one(a: &AgentArgs, files: &[tvfs_core::upload::UploadFile], total: u64, binding: &str, dir: &std::path::Path, protected: bool) -> i32 {
    let code = match confirm::issue(dir, binding, now_ms()) {
        Ok(c) => c,
        Err(e) => return fail(a.json, EXIT_FAILED, "error", &format!("Could not store the confirmation code: {e}")),
    };
    let size = fmt_bytes(total);
    let note = notice(files.len(), &size, protected);
    let expiry_label = expiry::label(a.expire_min);
    if a.json {
        let list: Vec<_> = files.iter().map(|f| json!({ "path": display_path(&f.path), "name": f.upload_name(), "size": f.size })).collect();
        println!(
            "{}",
            json!({
                "status": "confirmation_required",
                "uploaded": false,
                "file_count": files.len(),
                "total_size": total,
                "total_size_text": size,
                "expires_in": expiry_label,
                "users_only": protected,
                "files": list,
                "notice": note,
                "confirm_code": code,
                "code_valid_minutes": 10,
            })
        );
    } else {
        println!("NOTHING UPLOADED YET - confirmation required.\n");
        println!("Files ({}), total {}, link expires in {}:", files.len(), size, expiry_label);
        for f in files.iter().take(50) {
            println!("  {} ({})", display_path(&f.path), fmt_bytes(f.size));
        }
        if files.len() > 50 {
            println!("  ... and {} more", files.len() - 50);
        }
        println!("\n{note}\n");
        println!("Confirmation code (single use, valid 10 minutes): {code}");
    }
    EXIT_OK
}

async fn upload(a: &AgentArgs, files: Vec<tvfs_core::upload::UploadFile>, client: tvfs_core::Client, cfg: &Config, tier2: bool, protected: bool) -> i32 {
    let names: Vec<(String, u64)> = files.iter().map(|f| (f.upload_name(), f.size)).collect();
    let total: u64 = names.iter().map(|n| n.1).sum();
    let uploader = new_uploader(&client);
    let run = Run::new(names.clone(), Control::default());
    let req = UploadRequest { files, expiration_minutes: a.expire_min, remove_timestamp: tier2, chunking: chunk_mode(cfg), protected };

    // a progress line on stderr every few seconds keeps long uploads from looking stuck
    let ticker = {
        let run = run.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                let s = run.snapshot();
                eprintln!("uploading: {} / {}", fmt_bytes(s.done), fmt_bytes(s.total));
            }
        })
    };
    let t0 = Instant::now();
    let report = uploader.run(req, run, Arc::new(|_| {})).await;
    ticker.abort();
    let mut fin = Finished::from_report(report, &names);
    links::resolve_pending(&client, &mut fin).await;
    let elapsed = t0.elapsed().as_secs_f64();
    let sent = if fin.any_ok() { fin.ok_bytes().max(if protected { total } else { 0 }) } else { 0 };
    let avg = if elapsed > 0.0 { sent as f64 / 1_048_576.0 / elapsed } else { 0.0 };
    let prefs = Prefs::from_config(cfg);
    if a.json {
        println!("{}", render_json(&fin, prefs, elapsed, avg));
    } else {
        print!("{}", render_text(&fin, prefs, elapsed, avg));
    }
    if fin.any_failed() {
        EXIT_FAILED
    } else {
        EXIT_OK
    }
}

pub fn render_text(fin: &Finished, prefs: Prefs, elapsed: f64, avg: f64) -> String {
    let mut out = String::new();
    if let Some(e) = &fin.failed {
        out.push_str(&format!("Upload failed: {e}\n"));
    }
    for f in &fin.files {
        match &f.result {
            Ok(r) => {
                out.push_str(&format!("{} ({})\n", f.name, fmt_bytes(r.size)));
                for l in links::collect(r, prefs) {
                    out.push_str(&format!("  {}: {}{}\n", l.label, l.url, if l.default { " (default)" } else { "" }));
                }
            }
            Err(e) => out.push_str(&format!("{} FAILED: {e}\n", f.name)),
        }
    }
    if let Some(b) = &fin.batch_link {
        out.push_str(&format!("Batch page (all files): {b}\n"));
    }
    if let Some(b) = &fin.batch_styled {
        out.push_str(&format!("Batch page, styled: {b}\n"));
    }
    if let Some(s) = &fin.share_link {
        out.push_str(&format!("Share link (TVFS users only): {s}\n"));
    }
    out.push_str(&format!("Total time {}, average speed {}\n", fmt_time(elapsed), fmt_speed_mbs(avg)));
    out
}

pub fn render_json(fin: &Finished, prefs: Prefs, elapsed: f64, avg: f64) -> serde_json::Value {
    let files: Vec<_> = fin
        .files
        .iter()
        .map(|f| match &f.result {
            Ok(r) => {
                let items = links::collect(r, prefs);
                let def = items.iter().find(|i| i.default).map(|i| i.url.clone());
                json!({ "name": f.name, "size": r.size, "ok": true, "links": items, "default_link": def })
            }
            Err(e) => json!({ "name": f.name, "ok": false, "error": e }),
        })
        .collect();
    json!({
        "status": if fin.any_failed() { "failed" } else { "uploaded" },
        "uploaded": fin.any_ok(),
        "files": files,
        "batch_link": fin.batch_link,
        "batch_styled_link": fin.batch_styled,
        "share_link": fin.share_link,
        "error": fin.failed,
        "elapsed_seconds": (elapsed * 100.0).round() / 100.0,
        "average_mb_per_second": (avg * 100.0).round() / 100.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::links::FileOut;
    use tvfs_core::config::AutoCopy;
    use tvfs_core::models::UploadCompleteResponse;

    fn fin() -> Finished {
        let r = UploadCompleteResponse {
            link: "https://x/f/a.png".into(),
            size: 2048,
            styled_link: Some("[a](https://x/f/a.png)".into()),
            ..Default::default()
        };
        Finished {
            files: vec![FileOut { name: "a.png".into(), result: Ok(r) }, FileOut { name: "b.txt".into(), result: Err("boom".into()) }],
            ..Default::default()
        }
    }

    #[test]
    fn notice_is_the_agreed_text() {
        let n = notice(2, "1.00 MB", false);
        assert!(n.starts_with("You are about to upload 2 file(s) (1.00 MB) to the PUBLIC internet (TVFS)."));
        assert!(n.contains("`--confirm <CODE>`"));
    }

    #[test]
    fn json_output_marks_default_and_errors() {
        let p = Prefs { auto: AutoCopy::Styled, prefer_tool: true, image_styled: false };
        let v = render_json(&fin(), p, 2.0, 1.0);
        assert_eq!(v["status"], "failed");
        assert_eq!(v["files"][0]["default_link"], "[a](https://x/f/a.png)");
        assert_eq!(v["files"][0]["links"][1]["default"], true);
        assert_eq!(v["files"][1]["ok"], false);
    }

    #[test]
    fn text_output_marks_default() {
        let p = Prefs { auto: AutoCopy::Normal, prefer_tool: true, image_styled: false };
        let t = render_text(&fin(), p, 2.0, 1.0);
        assert!(t.contains("Direct link: https://x/f/a.png (default)"));
        assert!(t.contains("b.txt FAILED: boom"));
        assert!(t.contains("Total time 2s, average speed 1.0 MB/s"));
    }
}
