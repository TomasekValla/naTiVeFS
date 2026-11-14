//! Non-interactive mode for AI agents. Never touches the terminal UI, never asks for credentials.
//!
//! Step 1 (no `--confirm`): prints a summary, the public-upload notice and a one-time code, uploads nothing.
//! Step 2 (`--confirm <CODE>`): checks the code and uploads.
//! `--delete <file_id>` follows the same two steps for removing one of the user's own uploads.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use tvfs_core::config::Config;
use tvfs_core::models::RecentFiles;
use tvfs_core::fmt::{fmt_bytes, fmt_speed_mbs, fmt_time};
use tvfs_core::upload::{protected_limit_message, Control, Run, UploadRequest, PROTECTED_MAX_FILES};

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

/// Text for the error when files are rejected; points at `--skip-unsupported` when that would help.
pub fn problems_message(problems: &[String]) -> String {
    let mut m = problems.join("\n");
    if problems.iter().any(|p| p.contains("has no extension") || p.contains("exceeds")) {
        m.push_str("\nNothing was uploaded. To leave out files TVFS cannot take (listed as \"skipped\") instead of failing, add --skip-unsupported.");
    }
    m
}

async fn run_async(a: AgentArgs) -> i32 {
    if let Some(id) = a.delete.clone() {
        return delete_flow(&a, &id).await;
    }
    let (mut files, mut skipped, mut problems) = collect_files_skipping(&a.paths, a.include_hidden);
    if !a.skip_unsupported {
        // strict (default): a named file or a too large file the server cannot take fails the command; files without an
        // extension, hidden files and secret-looking files inside a folder are left out like in the app, but listed as
        // skipped instead of vanishing
        let (keep, refuse): (Vec<_>, Vec<_>) = skipped.into_iter().partition(|s| s.soft);
        problems.extend(refuse.into_iter().map(|s| format!("{}: {}", s.path, s.reason)));
        skipped = keep;
    }
    if !problems.is_empty() {
        return fail(a.json, EXIT_USAGE, "invalid_files", &problems_message(&problems));
    }
    if files.is_empty() {
        let mut msg = String::from("No files to upload (the folders are empty or hold only files without an extension).");
        for s in &skipped {
            msg.push_str(&format!("\nSkipped: {} - {}", s.path, s.reason));
        }
        return fail(a.json, EXIT_USAGE, "invalid_files", &msg);
    }
    let cfg = Config::load();
    let protected = a.users_only.unwrap_or(cfg.tvfs_users_only);
    if protected && files.len() > PROTECTED_MAX_FILES {
        return fail(a.json, EXIT_USAGE, "invalid_files", &protected_limit_message(files.len()));
    }
    if a.voice_note {
        if protected {
            return fail(a.json, EXIT_USAGE, "invalid_files", "--voice-note is for public uploads only, but this upload is a TVFS Users Only share (add --public to override the setting).");
        }
        if !files.iter().any(is_audio) {
            return fail(a.json, EXIT_USAGE, "invalid_files", "--voice-note needs at least one audio file among the files to upload.");
        }
        for f in files.iter_mut().filter(|f| is_audio(f)) {
            f.voice_note = true;
        }
    }

    let (client, user) = match client_with_saved_session() {
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
    let binding = confirm::binding(&files, a.expire_min, protected, a.anonymize, &user);
    let dir = match Config::data_dir() {
        Ok(d) => d,
        Err(e) => return fail(a.json, EXIT_FAILED, "error", &e.to_string()),
    };

    let Some(code) = a.confirm.as_deref() else {
        return step_one(&a, &files, &skipped, total, &binding, &dir, protected);
    };
    if let Err(e) = confirm::consume(&dir, code, &binding, now_ms()) {
        return fail(a.json, EXIT_BAD_CODE, "bad_confirmation", &e.to_string());
    }
    if a.anonymize {
        for f in &mut files {
            f.set_anonymized(true);
        }
    }
    upload(&a, files, &skipped, client, &cfg, status.tier == 2, protected).await
}

fn step_one(a: &AgentArgs, files: &[tvfs_core::upload::UploadFile], skipped: &[Skipped], total: u64, binding: &str, dir: &std::path::Path, protected: bool) -> i32 {
    let code = match confirm::issue(dir, binding, now_ms()) {
        Ok(c) => c,
        Err(e) => return fail(a.json, EXIT_FAILED, "error", &format!("Could not store the confirmation code: {e}")),
    };
    let size = fmt_bytes(total);
    let note = notice(files.len(), &size, protected);
    let expiry_label = expiry::label(a.expire_min);
    if a.json {
        let list: Vec<_> = files.iter().map(|f| json!({ "path": display_path(&f.path), "name": f.upload_name(), "size": f.size, "voice_note": f.voice_note })).collect();
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
                "anonymize": a.anonymize,
                "files": list,
                "skipped": skipped_json(skipped),
                "notice": note,
                "confirm_code": code,
                "code_valid_minutes": 10,
            })
        );
    } else {
        println!("NOTHING UPLOADED YET - confirmation required.\n");
        println!("Files ({}), total {}, link expires in {}:", files.len(), size, expiry_label);
        // every file, never a truncated list: the user (or the agent reporting to them) must see exactly what goes out
        for f in files.iter() {
            println!("  {} ({}){}", display_path(&f.path), fmt_bytes(f.size), if f.voice_note { " [voice note]" } else { "" });
        }
        if files.len() > 20 {
            println!("Total: {} files, {}.", files.len(), size);
        }
        if a.anonymize {
            println!("File names will be replaced by random names (extensions kept).");
        }
        print!("{}", skipped_text(skipped));
        println!("\n{note}\n");
        println!("Confirmation code (single use, valid 10 minutes): {code}");
    }
    EXIT_OK
}

async fn upload(a: &AgentArgs, files: Vec<tvfs_core::upload::UploadFile>, skipped: &[Skipped], client: tvfs_core::Client, cfg: &Config, tier2: bool, protected: bool) -> i32 {
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
        println!("{}", render_json(&fin, prefs, elapsed, avg, skipped));
    } else {
        print!("{}", render_text(&fin, prefs, elapsed, avg, skipped));
    }
    if fin.any_failed() {
        EXIT_FAILED
    } else {
        EXIT_OK
    }
}

/// Name of one of the user's recent uploads: `Ok(None)` = the sync list does not know the id, `Err` = the list could
/// not be fetched.
async fn recent_name(client: &tvfs_core::Client, id: &str) -> Result<Option<String>, String> {
    let resp = client.sync(None).await.map_err(|e| e.to_string())?;
    let mut rf = RecentFiles::default();
    rf.apply(resp);
    Ok(rf.files.into_iter().find(|f| f.id == id).map(|f| f.name))
}

pub fn delete_notice(id: &str, name: Option<&str>) -> String {
    let what = match name {
        Some(n) => format!("the uploaded file \"{n}\" (id {id})"),
        None => format!("the uploaded file with id {id} (name unknown: it is not in the recent uploads; --force-unknown-id was given)"),
    };
    format!(
        "You are about to PERMANENTLY DELETE {what} from TVFS. Its links stop working and this cannot be undone. A deletion always needs the user's approval: show the user what will be deleted and run the command again with `--confirm <CODE>` only after they said yes to exactly this deletion."
    )
}

async fn delete_flow(a: &AgentArgs, id: &str) -> i32 {
    let (client, _user) = match client_with_saved_session() {
        Ok(Some(c)) => c,
        Ok(None) => return fail(a.json, EXIT_NOT_LOGGED_IN, "not_logged_in", NOT_LOGGED_IN),
        Err(e) => return fail(a.json, EXIT_FAILED, "error", &format!("Could not read the saved login: {e}")),
    };
    if let Err(e) = client.status().await {
        return if e.is_auth() {
            fail(a.json, EXIT_NOT_LOGGED_IN, "not_logged_in", NOT_LOGGED_IN)
        } else {
            fail(a.json, EXIT_FAILED, "error", &format!("Cannot reach TVFS: {e}"))
        };
    }
    let dir = match Config::data_dir() {
        Ok(d) => d,
        Err(e) => return fail(a.json, EXIT_FAILED, "error", &e.to_string()),
    };
    let binding = confirm::delete_binding(id);
    let looked_up = recent_name(&client, id).await;
    let Some(code) = a.confirm.as_deref() else {
        // step 1 only goes on for an id the user's own recent list knows (a wrong or injected id is refused)
        let name = match looked_up {
            Err(e) => return fail(a.json, EXIT_FAILED, "error", &format!("Could not check the id against your uploads (is TVFS reachable?): {e}")),
            Ok(None) if !a.force_unknown_id => {
                return fail(
                    a.json,
                    EXIT_USAGE,
                    "unknown_id",
                    &format!("The id {id} is not among your recent uploads, so nothing was prepared. Check the id (the file_id of an upload); if you are sure, add --force-unknown-id."),
                )
            }
            Ok(n) => n,
        };
        let code = match confirm::issue(&dir, &binding, now_ms()) {
            Ok(c) => c,
            Err(e) => return fail(a.json, EXIT_FAILED, "error", &format!("Could not store the confirmation code: {e}")),
        };
        let note = delete_notice(id, name.as_deref());
        if a.json {
            println!(
                "{}",
                json!({ "status": "confirmation_required", "deleted": false, "file_id": id, "name": name, "notice": note, "confirm_code": code, "code_valid_minutes": 10 })
            );
        } else {
            println!("NOTHING DELETED YET - confirmation required.\n");
            println!("{note}\n");
            println!("Confirmation code (single use, valid 10 minutes): {code}");
        }
        return EXIT_OK;
    };
    if let Err(e) = confirm::consume(&dir, code, &binding, now_ms()) {
        return fail(a.json, EXIT_BAD_CODE, "bad_confirmation", &e.to_string());
    }
    let name = looked_up.ok().flatten();
    match client.delete_file(id).await {
        Ok(()) => {
            if a.json {
                println!("{}", json!({ "status": "deleted", "deleted": true, "file_id": id, "name": name }));
            } else {
                println!("Deleted {} (id {id}).", name.as_deref().map(|n| format!("\"{n}\"")).unwrap_or_else(|| "the file".into()));
            }
            EXIT_OK
        }
        Err(e) if e.is_auth() => fail(a.json, EXIT_NOT_LOGGED_IN, "not_logged_in", NOT_LOGGED_IN),
        Err(e) => fail(a.json, EXIT_FAILED, "failed", &format!("The server did not delete the file: {e}")),
    }
}

pub fn skipped_json(skipped: &[Skipped]) -> serde_json::Value {
    skipped.iter().map(|s| json!({ "path": s.path, "reason": s.reason })).collect::<Vec<_>>().into()
}

pub fn skipped_text(skipped: &[Skipped]) -> String {
    if skipped.is_empty() {
        return String::new();
    }
    let mut out = format!("\nSkipped ({}, not uploaded):\n", skipped.len());
    for s in skipped {
        out.push_str(&format!("  {} - {}\n", s.path, s.reason));
    }
    out
}

pub fn render_text(fin: &Finished, prefs: Prefs, elapsed: f64, avg: f64, skipped: &[Skipped]) -> String {
    let mut out = String::new();
    if let Some(e) = &fin.failed {
        out.push_str(&format!("Upload failed: {e}\n"));
    }
    for f in &fin.files {
        match &f.result {
            Ok(r) => {
                out.push_str(&format!("{} ({})\n", f.name, fmt_bytes(r.size)));
                if !r.file_id.is_empty() {
                    out.push_str(&format!("  id: {}\n", r.file_id));
                }
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
    out.push_str(&skipped_text(skipped));
    out.push_str(&format!("Total time {}, average speed {}\n", fmt_time(elapsed), fmt_speed_mbs(avg)));
    out
}

pub fn render_json(fin: &Finished, prefs: Prefs, elapsed: f64, avg: f64, skipped: &[Skipped]) -> serde_json::Value {
    let files: Vec<_> = fin
        .files
        .iter()
        .map(|f| match &f.result {
            Ok(r) => {
                let items = links::collect(r, prefs);
                let def = items.iter().find(|i| i.default).map(|i| i.url.clone());
                let file_id = (!r.file_id.is_empty()).then(|| r.file_id.clone());
                json!({ "name": f.name, "file_id": file_id, "size": r.size, "ok": true, "links": items, "default_link": def })
            }
            Err(e) => json!({ "name": f.name, "ok": false, "error": e }),
        })
        .collect();
    json!({
        "status": if !fin.any_failed() { "uploaded" } else if fin.any_ok() { "partial" } else { "failed" },
        "uploaded": fin.any_ok(),
        "files": files,
        "skipped": skipped_json(skipped),
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
        let v = render_json(&fin(), p, 2.0, 1.0, &[]);
        // one file uploaded, one failed: not "failed" as a whole
        assert_eq!(v["status"], "partial");
        assert_eq!(v["files"][0]["default_link"], "[a](https://x/f/a.png)");
        assert_eq!(v["files"][0]["links"][1]["default"], true);
        assert_eq!(v["files"][1]["ok"], false);
    }

    #[test]
    fn json_has_file_id_and_skipped() {
        let mut f = fin();
        if let Ok(r) = &mut f.files[0].result {
            r.file_id = "id42".into();
        }
        let p = Prefs { auto: AutoCopy::Normal, prefer_tool: true, image_styled: false };
        let sk = [Skipped { path: "/x/LICENSE".into(), reason: "no extension".into(), soft: true }];
        let v = render_json(&f, p, 1.0, 1.0, &sk);
        assert_eq!(v["files"][0]["file_id"], "id42");
        assert_eq!(v["skipped"][0]["path"], "/x/LICENSE");
        assert_eq!(v["skipped"][0]["reason"], "no extension");
        let t = render_text(&f, p, 1.0, 1.0, &sk);
        assert!(t.contains("  id: id42\n") && t.contains("Skipped (1, not uploaded):") && t.contains("/x/LICENSE - no extension"), "{t}");
        // no skipped files: an empty list, no extra text
        assert_eq!(render_json(&f, p, 1.0, 1.0, &[])["skipped"], serde_json::json!([]));
        assert!(!render_text(&f, p, 1.0, 1.0, &[]).contains("Skipped"));
    }

    #[test]
    fn strict_error_mentions_the_skip_flag() {
        let m = problems_message(&["File \"LICENSE\" has no extension and its type (unknown) could not be detected.".into()]);
        assert!(m.contains("--skip-unsupported"), "{m}");
        assert!(!problems_message(&["File \"a\" could not be read: x".into()]).contains("--skip-unsupported"));
    }

    #[test]
    fn delete_notice_names_the_file_when_known() {
        assert!(delete_notice("abc", Some("a.png")).contains("\"a.png\" (id abc)"));
        assert!(delete_notice("abc", None).contains("id abc (name unknown"));
        assert!(delete_notice("abc", None).contains("always needs the user's approval"));
    }

    #[test]
    fn text_output_marks_default() {
        let p = Prefs { auto: AutoCopy::Normal, prefer_tool: true, image_styled: false };
        let t = render_text(&fin(), p, 2.0, 1.0, &[]);
        assert!(t.contains("Direct link: https://x/f/a.png (default)"));
        assert!(t.contains("b.txt FAILED: boom"));
        assert!(t.contains("Total time 2s, average speed 1.0 MB/s"));
    }
}
