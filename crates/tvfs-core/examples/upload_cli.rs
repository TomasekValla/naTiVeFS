//! Developer tool: run the upload engine against the real server with the
//! session the app saved (nothing is printed from it).
//!
//! ```text
//! cargo run --release -p tvfs-core --example upload_cli -- [options] <file>...
//!   --fixed CxM      fixed chunking, C connections x M MB (default: auto)
//!   --expiry MIN     expiry preset in minutes (default 5)
//!   --protected      "TVFS Users Only" share
//!   --resume FILE    resume store (default: upload-cli-resume.json next to the binary's cwd)
//!   --kill-after S   cancel the run after S seconds (resume test)
//!   --keep           do not delete the uploaded files afterwards
//!   --delete ID      only delete file ID and exit
//! ```
//! Honours `HTTPS_PROXY` (reqwest default), which is how dropped connections are tested.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tvfs_core::fmt::{fmt_bytes, fmt_speed_mbs, fmt_time};
use tvfs_core::upload::{inspect, ChunkMode, Control, HistoryStore, Outcome, ResumeStore, Run, UploadRequest, Uploader};
use tvfs_core::{session, Client};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let mut files = Vec::new();
    let mut chunking = ChunkMode::Auto;
    let mut expiry = 5u32;
    let mut protected = false;
    let mut resume_path = PathBuf::from("upload-cli-resume.json");
    let mut kill_after: Option<u64> = None;
    let mut keep = false;
    let mut delete: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--fixed" => {
                let v = args.next().expect("--fixed CxM");
                let (c, m) = v.split_once('x').expect("CxM");
                chunking = ChunkMode::Fixed { count: c.parse().unwrap(), size_mb: m.parse().unwrap() };
            }
            "--expiry" => expiry = args.next().unwrap().parse().unwrap(),
            "--protected" => protected = true,
            "--resume" => resume_path = args.next().unwrap().into(),
            "--kill-after" => kill_after = Some(args.next().unwrap().parse().unwrap()),
            "--keep" => keep = true,
            "--delete" => delete = args.next(),
            _ => files.push(PathBuf::from(a)),
        }
    }
    let client = Client::new().unwrap();
    let saved = session::load().ok().flatten().expect("no saved session: log in with the app first");
    client.restore_session(&saved.token);
    if let Some(id) = delete {
        println!("delete {id}: {:?}", client.delete_file(&id).await.map(|_| "ok"));
        return;
    }
    let status = client.status().await.expect("status (session valid?)");
    println!("server up, tier {}, used {}", status.tier, status.used_gb);

    let files: Vec<_> = files.iter().map(|p| inspect(p, None).expect("file")).collect();
    let uploader = Uploader::new(client.clone(), Arc::new(ResumeStore::open(&resume_path)), Arc::new(HistoryStore::in_memory()));
    let control = Control::default();
    let run = Run::new(files.iter().map(|f| (f.upload_name(), f.size)), control.clone());
    let req = UploadRequest { files, expiration_minutes: expiry, remove_timestamp: status.tier == 2, chunking, protected };

    let printer = {
        let run = run.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let s = run.snapshot();
                println!(
                    "{:>5.1}%  {} / {}  {}  eta {}  avg {}  conn {}  {:?}  {}",
                    s.done as f64 * 100.0 / s.total.max(1) as f64,
                    fmt_bytes(s.done),
                    fmt_bytes(s.total),
                    s.speed_mbs.map_or("--".into(), fmt_speed_mbs),
                    s.eta_s.map_or("--".into(), fmt_time),
                    s.avg_mbs.map_or("--".into(), fmt_speed_mbs),
                    s.requests,
                    s.status,
                    s.plan.map(|p| p.describe()).unwrap_or_default(),
                );
            }
        })
    };
    if let Some(secs) = kill_after {
        let control = control.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(secs)).await;
            println!("-- killing the run (simulated crash: the resume record is kept on purpose)");
            std::process::exit(3);
        });
        let _ = control;
    }
    let t0 = std::time::Instant::now();
    let report = uploader.run(req, run, Arc::new(|e| println!("event: {e:?}"))).await;
    printer.abort();
    println!("done in {:.1} s", t0.elapsed().as_secs_f64());
    let mut ids = Vec::new();
    match &report.outcome {
        Outcome::Single(f) => {
            println!("{}: {:?}", f.name, f.result.as_ref().map(|r| (&r.link, r.size)));
            if let Ok(r) = &f.result {
                ids.push(r.file_id.clone());
            }
        }
        Outcome::Batch { files, batch } => {
            for f in files {
                println!("{}: {:?}", f.name, f.result.as_ref().map(|r| (&r.link, r.size)));
                if let Ok(r) = &f.result {
                    ids.push(r.file_id.clone());
                }
            }
            println!("batch: {:?}", batch.as_ref().map(|b| &b.link));
        }
        Outcome::Protected(r) => println!("protected: {:?}", r.as_ref().map(|s| &s.link)),
        Outcome::Failed(e) => println!("failed: {e}"),
    }
    if let Some(st) = report.stats {
        println!("stats: {} at {} ({}), 1 GB in {}", st.elapsed, st.speed_mbs, st.speed_mbps, st.fun.1);
    }
    if !keep {
        for id in ids {
            println!("delete {id}: {:?}", client.delete_file(&id).await.map(|_| "ok"));
        }
    }
}
