//! Wire types of the TVFS HTTP API (`/api/...`). Field names mirror the
//! server in `files_backend/upload.js` and `userSync.js`.

use serde::{Deserialize, Deserializer, Serialize};

/// Accept a number, a numeric string or null for timestamps/sizes.
fn lenient_i64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(match v {
        Some(serde_json::Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Some(serde_json::Value::String(s)) => s.parse::<i64>().ok(),
        _ => None,
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthResponse {
    pub valid: bool,
    #[serde(default)]
    pub tier: u8,
}

/// `GET /api/status`
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    #[serde(default)]
    pub backend: String,
    #[serde(default)]
    pub used_bytes: u64,
    /// e.g. `"12.34 GB"` (the server spells it `usedGB`).
    #[serde(default, rename = "usedGB")]
    pub used_gb: String,
    #[serde(default, rename = "totalGB")]
    pub total_gb: f64,
    #[serde(default)]
    pub tier: u8,
    /// Version counter of the "recently uploaded" list; call `sync` when it changes.
    #[serde(default)]
    pub user_version: Option<u64>,
    /// Unfinished chunked uploads of this user (any device).
    #[serde(default, deserialize_with = "lenient_list")]
    pub pending_uploads: Vec<PendingUpload>,
}

/// A list whose broken entries are skipped instead of failing the whole response.
fn lenient_list<'de, D: Deserializer<'de>, T: serde::de::DeserializeOwned>(d: D) -> Result<Vec<T>, D::Error> {
    let v = Option::<Vec<serde_json::Value>>::deserialize(d)?.unwrap_or_default();
    Ok(v.into_iter().filter_map(|x| serde_json::from_value(x).ok()).collect())
}

/// One entry of `pendingUploads` in `/api/status` (a chunked upload left unfinished, any device).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingUpload {
    pub upload_id: String,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub file_size: u64,
    /// `None` while an auto-sized upload has not sent its last chunk.
    #[serde(default)]
    pub total_chunks: Option<u32>,
    #[serde(default)]
    pub received_chunks: u32,
    #[serde(default, deserialize_with = "lenient_i64")]
    pub created_at: Option<i64>,
    #[serde(default, deserialize_with = "lenient_i64")]
    pub expires_at: Option<i64>,
}

/// One entry of the "recently uploaded" list.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncEntry {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub link: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default, deserialize_with = "lenient_i64")]
    pub uploaded_at: Option<i64>,
    #[serde(default, deserialize_with = "lenient_i64")]
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub thumbnail: Option<String>,
}

/// `GET /api/user/sync[?since=N]`
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum SyncResponse {
    Full { version: u64, files: Vec<SyncEntry> },
    Diff { version: u64, ops: Vec<SyncOp> },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SyncOp {
    Add { entry: SyncEntry },
    Remove {
        #[serde(rename = "fileId")]
        file_id: String,
    },
    #[serde(other)]
    Unknown,
}

/// Local mirror of the server-side recent list; apply sync responses to it.
#[derive(Debug, Default, Clone)]
pub struct RecentFiles {
    pub version: u64,
    pub files: Vec<SyncEntry>,
}

impl RecentFiles {
    pub fn apply(&mut self, resp: SyncResponse) {
        match resp {
            SyncResponse::Full { version, files } => {
                self.version = version;
                self.files = files;
            }
            SyncResponse::Diff { version, ops } => {
                for op in ops {
                    match op {
                        SyncOp::Add { entry } => {
                            self.files.retain(|f| f.id != entry.id);
                            self.files.insert(0, entry);
                        }
                        SyncOp::Remove { file_id } => self.files.retain(|f| f.id != file_id),
                        SyncOp::Unknown => {}
                    }
                }
                self.version = version;
            }
        }
    }
}

/// `POST /api/upload/init`
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadInit {
    pub filename: String,
    pub file_size: u64,
    pub mime_type: String,
    /// `None` = dynamic chunking (server learns the total from `isLastChunk`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_chunks: Option<u32>,
    /// `"weak"` for chunks <= 5 MB, otherwise `"safe"` (what the web sends).
    pub upload_mode: String,
    pub expiration_minutes: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadInitResponse {
    pub upload_id: String,
    #[serde(default)]
    pub recommended_parallel: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkAck {
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub chunk_index: Option<u32>,
    #[serde(default)]
    pub received: Option<u32>,
    #[serde(default)]
    pub total: Option<u32>,
    #[serde(default)]
    pub duplicate: bool,
}

/// `POST /api/upload/complete`
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadComplete {
    pub upload_id: String,
    pub anonymize: bool,
    pub remove_timestamp: bool,
    pub expiration_minutes: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
}

/// Response of `/api/upload/complete` and of the single-request `/api/upload`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadCompleteResponse {
    pub link: String,
    /// `video` / `audio` / `image` / `file`
    #[serde(rename = "type", default)]
    pub kind: String,
    /// Stored name on the server (sanitized, usually with a timestamp).
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub file_id: String,
    /// The styled (markdown) link is computed in the background: poll `/api/upload/styled-link/:fileId`.
    #[serde(default)]
    pub styled_link_pending: bool,
    #[serde(default)]
    pub styled_link: Option<String>,
    #[serde(default)]
    pub text_reader_link: Option<String>,
    #[serde(default)]
    pub text_reader_styled_link: Option<String>,
    #[serde(default)]
    pub md_reader_link: Option<String>,
    #[serde(default)]
    pub md_reader_styled_link: Option<String>,
    #[serde(default)]
    pub audio_player_link: Option<String>,
    #[serde(default)]
    pub audio_player_styled_link: Option<String>,
    #[serde(default)]
    pub track_bpm: Option<f64>,
}

impl UploadCompleteResponse {
    /// Reader (Markdown first) or audio player page, if the server made one.
    pub fn tool_link(&self) -> Option<&str> {
        self.md_reader_link.as_deref().or(self.text_reader_link.as_deref()).or(self.audio_player_link.as_deref())
    }

    pub fn tool_styled_link(&self) -> Option<&str> {
        self.md_reader_styled_link
            .as_deref()
            .or(self.text_reader_styled_link.as_deref())
            .or(self.audio_player_styled_link.as_deref())
    }
}

/// `GET /api/upload/styled-link/:fileId`
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StyledLinkStatus {
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub styled_link: Option<String>,
    #[serde(default)]
    pub thumbnail: Option<String>,
}

/// `POST /api/upload/batch/init`
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchInitResponse {
    pub batch_id: String,
}

/// `POST /api/upload/batch/complete`
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchCompleteResponse {
    #[serde(default)]
    pub batch_id: String,
    /// Landing page (`/files/batch/<id>.html`).
    #[serde(default)]
    pub link: String,
    #[serde(default)]
    pub landing_page: Option<String>,
    #[serde(default)]
    pub styled_link: Option<String>,
    #[serde(default)]
    pub files: Vec<BatchFile>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchFile {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub original_name: String,
    #[serde(default)]
    pub link: String,
    #[serde(default)]
    pub styled_link: Option<String>,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub text_reader_link: Option<String>,
    #[serde(default)]
    pub audio_player_link: Option<String>,
    #[serde(default)]
    pub track_bpm: Option<f64>,
}

/// `POST /api/protected/upload/init` ("TVFS Users Only", chunked)
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtectedInitResponse {
    pub upload_id: String,
    #[serde(default)]
    pub chunk_size: Option<u64>,
    #[serde(default)]
    pub recommended_parallel: Option<u32>,
}

/// Result of a "TVFS Users Only" share (`/api/protected/upload[/complete]`).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtectedShareResponse {
    #[serde(default)]
    pub share_id: String,
    pub link: String,
    #[serde(default, deserialize_with = "lenient_i64")]
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub files: Vec<ProtectedFile>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtectedFile {
    #[serde(default)]
    pub index: u32,
    #[serde(default)]
    pub relative_path: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub category: Option<String>,
    /// Gated reader / player page for text, Markdown and audio files.
    #[serde(default)]
    pub tool_link: Option<String>,
}

/// `GET /api/upload/status/:id` (resume support)
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadStatus {
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub received_chunks: Vec<u32>,
    #[serde(default)]
    pub total_chunks: Option<u32>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_full_and_diff() {
        let full: SyncResponse = serde_json::from_str(
            r#"{"mode":"full","version":3,"files":[{"id":"a","name":"x.mp4","link":"https://f/x","category":"video","size":10,"uploadedAt":1,"expiresAt":"2","thumbnail":null}]}"#,
        )
        .unwrap();
        let mut r = RecentFiles::default();
        r.apply(full);
        assert_eq!((r.version, r.files.len()), (3, 1));
        assert_eq!(r.files[0].expires_at, Some(2));

        let diff: SyncResponse = serde_json::from_str(
            r#"{"mode":"diff","version":5,"ops":[{"v":4,"type":"add","entry":{"id":"b","name":"y"}},{"v":5,"type":"remove","fileId":"a"},{"v":6,"type":"weird"}]}"#,
        )
        .unwrap();
        r.apply(diff);
        assert_eq!(r.version, 5);
        assert_eq!(r.files.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), vec!["b"]);
    }

    #[test]
    fn status_tolerates_missing_fields() {
        let s: Status = serde_json::from_str(r#"{"backend":"UP","usedBytes":5,"usedGB":"0.00 GB","totalGB":32,"tier":2}"#).unwrap();
        assert_eq!(s.used_bytes, 5);
        assert_eq!((s.used_gb.as_str(), s.total_gb), ("0.00 GB", 32.0));
        assert!(s.user_version.is_none());
        assert!(s.pending_uploads.is_empty());
    }

    #[test]
    fn pending_uploads_skip_broken_entries() {
        let s: Status = serde_json::from_str(
            r#"{"pendingUploads":[{"uploadId":"1_a","filename":"big.mkv","fileSize":9,"totalChunks":null,"receivedChunks":3,
                "createdAt":10,"expiresAt":20},{"nope":1}]}"#,
        )
        .unwrap();
        assert_eq!(s.pending_uploads.len(), 1);
        let p = &s.pending_uploads[0];
        assert_eq!((p.upload_id.as_str(), p.total_chunks, p.received_chunks, p.expires_at), ("1_a", None, 3, Some(20)));
    }

    #[test]
    fn complete_response_tool_links() {
        let r: UploadCompleteResponse = serde_json::from_str(
            r#"{"message":"Upload successful","link":"https://f/files/text/n.md","styledLinkPending":true,"type":"file",
                "filename":"n_1.md","size":3,"fileId":"f_1","textReaderLink":null,"mdReaderLink":"https://f/files/text/n_md_tvfsreader.html",
                "mdReaderStyledLink":"[n.md - TVFS MD Reader](x)","audioPlayerLink":null,"trackBpm":null,"thumbnailPending":true}"#,
        )
        .unwrap();
        assert!(r.styled_link_pending);
        assert_eq!(r.tool_link(), Some("https://f/files/text/n_md_tvfsreader.html"));
        assert_eq!(r.tool_styled_link(), Some("[n.md - TVFS MD Reader](x)"));
    }
}
