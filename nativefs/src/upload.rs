use reqwest::{multipart, Client};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, Semaphore};

// ── Progress events sent to UI ────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum UploadEvent {
    Progress {
        file_id: usize,
        bytes_done: u64,
        bytes_total: u64,
    },
    Complete {
        file_id: usize,
        link: String,
        styled_link: String,
        filename: String,
    },
    Failed {
        file_id: usize,
        error: String,
    },
}

// ── API response types ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct InitResponse {
    #[serde(rename = "uploadId")]
    upload_id: String,
}

#[derive(Debug, Deserialize)]
struct ChunkResponse {
    success: Option<bool>,
    received: Option<u32>,
    total: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct CompleteResponse {
    link: Option<String>,
    #[serde(rename = "styledLink")]
    styled_link: Option<String>,
    filename: Option<String>,
    error: Option<String>,
}

// ── Resume state persisted to OS temp ────────────────────────────────────────

fn resume_path(upload_id: &str) -> PathBuf {
    std::env::temp_dir().join(format!("nativefs_{upload_id}.resume"))
}

fn save_resume(upload_id: &str, received_chunks: &[u32]) {
    let path = resume_path(upload_id);
    let _ = std::fs::write(
        &path,
        serde_json::to_string(received_chunks).unwrap_or_default(),
    );
}

fn load_resume(upload_id: &str) -> Vec<u32> {
    let path = resume_path(upload_id);
    let data = std::fs::read_to_string(&path).unwrap_or_default();
    serde_json::from_str(&data).unwrap_or_default()
}

fn clear_resume(upload_id: &str) {
    let _ = std::fs::remove_file(resume_path(upload_id));
}

// ── Main upload entry point ───────────────────────────────────────────────────

pub struct UploadTask {
    pub file_id: usize,
    pub path: PathBuf,
    pub anonymize: bool,
    pub expiration_days: u32,
    pub batch_id: Option<String>,
}

pub async fn upload_file(
    task: UploadTask,
    server_url: String,
    token: String,
    chunk_size_mb: u64,
    parallel: usize,
    tx: mpsc::UnboundedSender<UploadEvent>,
) {
    let result = do_upload(&task, &server_url, &token, chunk_size_mb, parallel, &tx).await;

    match result {
        Ok((link, styled_link, filename)) => {
            let _ = tx.send(UploadEvent::Complete {
                file_id: task.file_id,
                link,
                styled_link,
                filename,
            });
        }
        Err(e) => {
            let _ = tx.send(UploadEvent::Failed {
                file_id: task.file_id,
                error: e,
            });
        }
    }
}

async fn do_upload(
    task: &UploadTask,
    server_url: &str,
    token: &str,
    chunk_size_mb: u64,
    parallel: usize,
    tx: &mpsc::UnboundedSender<UploadEvent>,
) -> Result<(String, String, String), String> {
    let client = Client::builder()
        .cookie_store(true)
        .build()
        .map_err(|e| e.to_string())?;

    let file_data = tokio::fs::read(&task.path)
        .await
        .map_err(|e| format!("Cannot read file: {e}"))?;

    let file_size = file_data.len() as u64;
    let chunk_size = chunk_size_mb * 1024 * 1024;
    let total_chunks = ((file_size + chunk_size - 1) / chunk_size).max(1) as u32;

    let original_name = task
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();

    let mime = mime_guess::from_path(&task.path)
        .first_or_octet_stream()
        .to_string();

    // ── INIT ──────────────────────────────────────────────────────────────────
    let init_url = format!("{server_url}/api/upload/init");
    let init_params = [
        ("filename", original_name.as_str()),
        ("fileSize", &file_size.to_string()),
        ("mimeType", &mime),
        ("totalChunks", &total_chunks.to_string()),
        ("uploadMode", "fast"),
        ("expirationDays", &task.expiration_days.to_string()),
        ("batchId", task.batch_id.as_deref().unwrap_or("")),
    ];

    let init_resp = client
        .post(&init_url)
        .header("Cookie", format!("tvfs_token={token}"))
        .form(&init_params)
        .send()
        .await
        .map_err(|e| format!("Init request failed: {e}"))?;

    if !init_resp.status().is_success() {
        return Err(format!("Init failed: HTTP {}", init_resp.status()));
    }

    let init_body: InitResponse = init_resp
        .json()
        .await
        .map_err(|e| format!("Init parse error: {e}"))?;

    let upload_id = init_body.upload_id;

    // ── Check for resumable chunks ────────────────────────────────────────────
    let already_done: Vec<u32> = load_resume(&upload_id);

    // ── Chunk upload with parallelism ─────────────────────────────────────────
    let semaphore = Arc::new(Semaphore::new(parallel));
    let bytes_done = Arc::new(Mutex::new(
        already_done.len() as u64 * chunk_size.min(file_size),
    ));
    let file_data = Arc::new(file_data);
    let received = Arc::new(Mutex::new(already_done.clone()));

    let mut handles = Vec::new();

    for chunk_index in 0..total_chunks {
        if already_done.contains(&chunk_index) {
            continue;
        }

        let sem = semaphore.clone();
        let client2 = client.clone();
        let server_url2 = server_url.to_string();
        let token2 = token.to_string();
        let upload_id2 = upload_id.clone();
        let file_data2 = file_data.clone();
        let bytes_done2 = bytes_done.clone();
        let received2 = received.clone();
        let tx2 = tx.clone();
        let file_id = task.file_id;

        let start = (chunk_index as u64 * chunk_size) as usize;
        let end = ((start as u64 + chunk_size).min(file_size)) as usize;

        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();

            let chunk_data = file_data2[start..end].to_vec();
            let chunk_len = chunk_data.len() as u64;

            let chunk_url = format!("{server_url2}/api/upload/chunk");
            let form = multipart::Form::new()
                .text("uploadId", upload_id2.clone())
                .text("chunkIndex", chunk_index.to_string())
                .part(
                    "chunk",
                    multipart::Part::bytes(chunk_data)
                        .file_name(format!("chunk_{chunk_index}"))
                        .mime_str("application/octet-stream")
                        .unwrap(),
                );

            let resp = client2
                .post(&chunk_url)
                .header("Cookie", format!("tvfs_token={token2}"))
                .multipart(form)
                .send()
                .await;

            match resp {
                Ok(r) if r.status().is_success() => {
                    let mut bd = bytes_done2.lock().await;
                    *bd += chunk_len;
                    let _ = tx2.send(UploadEvent::Progress {
                        file_id,
                        bytes_done: *bd,
                        bytes_total: file_size,
                    });

                    let mut recv = received2.lock().await;
                    recv.push(chunk_index);
                    save_resume(&upload_id2, &recv);

                    Ok(())
                }
                Ok(r) => Err(format!("Chunk {chunk_index} failed: HTTP {}", r.status())),
                Err(e) => Err(format!("Chunk {chunk_index} network error: {e}")),
            }
        });

        handles.push(handle);
    }

    for handle in handles {
        handle
            .await
            .map_err(|e| format!("Task join error: {e}"))??;
    }

    // ── COMPLETE ──────────────────────────────────────────────────────────────
    let complete_url = format!("{server_url}/api/upload/complete");
    let complete_params = [
        ("uploadId", upload_id.as_str()),
        ("anonymize", if task.anonymize { "true" } else { "false" }),
        ("removeTimestamp", "false"),
        ("expirationDays", &task.expiration_days.to_string()),
        ("batchId", task.batch_id.as_deref().unwrap_or("")),
    ];

    let complete_resp = client
        .post(&complete_url)
        .header("Cookie", format!("tvfs_token={token}"))
        .form(&complete_params)
        .send()
        .await
        .map_err(|e| format!("Complete request failed: {e}"))?;

    if !complete_resp.status().is_success() {
        let status = complete_resp.status();
        let body = complete_resp.text().await.unwrap_or_default();
        return Err(format!("Complete failed HTTP {status}: {body}"));
    }

    let body: CompleteResponse = complete_resp
        .json()
        .await
        .map_err(|e| format!("Complete parse error: {e}"))?;

    if let Some(err) = body.error {
        return Err(err);
    }

    clear_resume(&upload_id);

    let link = body.link.unwrap_or_default();
    let styled_link = body.styled_link.unwrap_or_default();
    let filename = body.filename.unwrap_or(original_name);

    Ok((link, styled_link, filename))
}

// ── Batch helpers ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct BatchInitResponse {
    #[serde(rename = "batchId")]
    pub batch_id: String,
}

pub async fn batch_init(
    client: &Client,
    server_url: &str,
    token: &str,
    expiration_days: u32,
) -> Result<String, String> {
    let url = format!("{server_url}/api/upload/batch/init");
    let resp = client
        .post(&url)
        .header("Cookie", format!("tvfs_token={token}"))
        .form(&[("expirationDays", expiration_days.to_string())])
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let body: BatchInitResponse = resp.json().await.map_err(|e| e.to_string())?;
    Ok(body.batch_id)
}

#[derive(Debug, Deserialize)]
pub struct BatchCompleteResponse {
    pub link: Option<String>,
    #[serde(rename = "styledLink")]
    pub styled_link: Option<String>,
}

pub async fn batch_complete(
    client: &Client,
    server_url: &str,
    token: &str,
    batch_id: &str,
) -> Result<(String, String), String> {
    let url = format!("{server_url}/api/upload/batch/complete");
    let resp = client
        .post(&url)
        .header("Cookie", format!("tvfs_token={token}"))
        .form(&[("batchId", batch_id)])
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let body: BatchCompleteResponse = resp.json().await.map_err(|e| e.to_string())?;
    Ok((
        body.link.unwrap_or_default(),
        body.styled_link.unwrap_or_default(),
    ))
}
