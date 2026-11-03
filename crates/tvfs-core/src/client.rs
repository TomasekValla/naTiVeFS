//! Async HTTP client for the TVFS API. Sessions use the same `tvfs_token`
//! cookie as the web UI; a native client is not subject to CORS.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use reqwest::cookie::{CookieStore, Jar};
use reqwest::{multipart, Response, StatusCode};
use serde::de::DeserializeOwned;
use url::Url;

use crate::error::{Error, Result};
use crate::models::*;

const SESSION_COOKIE: &str = "tvfs_token";

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    jar: Arc<Jar>,
    base: Url,
}

impl Client {
    pub fn new() -> Result<Self> {
        Self::with_base(crate::base_url())
    }

    pub fn with_base(base: &str) -> Result<Self> {
        let base = Url::parse(base)?;
        let jar = Arc::new(Jar::default());
        let http = reqwest::Client::builder()
            .cookie_provider(Arc::clone(&jar))
            .user_agent(concat!("naTiVeFS/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self { http, jar, base })
    }

    pub fn base(&self) -> &Url {
        &self.base
    }

    pub(crate) fn endpoint(&self, path: &str) -> Result<Url> {
        Ok(self.base.join(path)?)
    }

    /// The underlying HTTP client (shares the cookie jar), for the upload engine.
    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Current session token (value of the `tvfs_token` cookie), if logged in.
    pub fn session_token(&self) -> Option<String> {
        let header = self.jar.cookies(&self.base)?;
        header
            .to_str()
            .ok()?
            .split(';')
            .map(str::trim)
            .find_map(|kv| kv.strip_prefix("tvfs_token=").map(str::to_owned))
    }

    /// Re-install a previously saved session token into the cookie jar.
    pub fn restore_session(&self, token: &str) {
        let secure = if self.base.scheme() == "https" { "; Secure" } else { "" };
        let cookie = format!("{SESSION_COOKIE}={token}; Path=/{secure}");
        self.jar.add_cookie_str(&cookie, &self.base);
    }

    async fn check(resp: Response) -> Result<Response> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        match status {
            StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED => Err(Error::Unauthorized),
            StatusCode::TOO_MANY_REQUESTS => Err(Error::RateLimited),
            _ => {
                let text = resp.text().await.unwrap_or_default();
                let message = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v.get("error").and_then(|e| e.as_str().map(str::to_owned)))
                    // an HTML error page (nginx) says nothing the status does not
                    .unwrap_or(if text.trim_start().starts_with('<') { String::new() } else { text });
                Err(Error::Server { status: status.as_u16(), message })
            }
        }
    }

    async fn json<T: DeserializeOwned>(req: reqwest::RequestBuilder) -> Result<T> {
        let resp = Self::check(req.send().await?).await?;
        Ok(resp.json::<T>().await?)
    }

    // ── session ───────────────────────────────────────────────────────────

    /// `POST /api/auth`. On success the session cookie is in the jar.
    pub async fn login(&self, username: &str, password: &str, remember: bool) -> Result<AuthResponse> {
        let resp = self
            .http
            .post(self.endpoint("api/auth")?)
            .json(&serde_json::json!({ "username": username, "password": password, "remember": remember }))
            .send()
            .await?;
        if resp.status() == StatusCode::FORBIDDEN {
            return Err(Error::BadCredentials);
        }
        let auth: AuthResponse = Self::check(resp).await?.json().await?;
        if auth.valid {
            Ok(auth)
        } else {
            Err(Error::BadCredentials)
        }
    }

    pub async fn logout(&self) -> Result<()> {
        let _ = Self::check(self.http.post(self.endpoint("api/logout")?).send().await?).await?;
        Ok(())
    }

    // ── read ──────────────────────────────────────────────────────────────

    pub async fn status(&self) -> Result<Status> {
        Self::json(self.http.get(self.endpoint("api/status")?)).await
    }

    /// `since = None` (or 0) asks for the full list.
    pub async fn sync(&self, since: Option<u64>) -> Result<SyncResponse> {
        let mut url = self.endpoint("api/user/sync")?;
        if let Some(v) = since.filter(|v| *v > 0) {
            url.query_pairs_mut().append_pair("since", &v.to_string());
        }
        Self::json(self.http.get(url)).await
    }

    /// Download a (small) resource such as a thumbnail with the session cookie.
    /// `url` may be relative to the server (`/files/thumbnails/x.avif`).
    /// Fails with `Error::Server { status: 413, .. }` when it is larger than `max_bytes`.
    pub async fn fetch_bytes(&self, url: &str, max_bytes: u64) -> Result<Bytes> {
        let url = self.base.join(url)?;
        // a local file (`Content::local`, and the images a local Markdown file links to)
        if url.scheme() == "file" {
            let path = url.to_file_path().map_err(|_| Error::Server { status: 400, message: "not a file path".into() })?;
            if tokio::fs::metadata(&path).await?.len() > max_bytes {
                return Err(Error::Server { status: 413, message: "resource too large".into() });
            }
            return Ok(Bytes::from(tokio::fs::read(&path).await?));
        }
        let resp = Self::check(self.http.get(url).timeout(Duration::from_secs(30)).send().await?).await?;
        if resp.content_length().is_some_and(|n| n > max_bytes) {
            return Err(Error::Server { status: 413, message: "resource too large".into() });
        }
        let bytes = resp.bytes().await?;
        if bytes.len() as u64 > max_bytes {
            return Err(Error::Server { status: 413, message: "resource too large".into() });
        }
        Ok(bytes)
    }

    /// `GET` with `Range: bytes=start-(start+len-1)` (the audio player streams files in pieces).
    /// Returns the bytes and the full file size. A server that ignores `Range` answers 200 with the
    /// whole file: then the bytes start at 0, which the returned offset says.
    pub async fn fetch_range(&self, url: &str, start: u64, len: u64) -> Result<RangeData> {
        let url = self.base.join(url)?;
        let end = start + len.max(1) - 1;
        let resp = Self::check(
            self.http.get(url).header(reqwest::header::RANGE, format!("bytes={start}-{end}")).timeout(Duration::from_secs(60)).send().await?,
        )
        .await?;
        let partial = resp.status() == StatusCode::PARTIAL_CONTENT;
        // Content-Range: bytes 0-99/1234
        let total = resp
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit('/').next())
            .and_then(|t| t.parse::<u64>().ok());
        let full_len = resp.content_length();
        let data = resp.bytes().await?;
        Ok(if partial {
            RangeData { offset: start, total, data }
        } else {
            RangeData { offset: 0, total: full_len.or(Some(data.len() as u64)), data }
        })
    }

    /// Download `url` into `dest` (written as `dest.part`, renamed when complete). `progress(done, total)`
    /// runs for every received piece; `cancel` stops the transfer and removes the partial file.
    pub async fn download_to(
        &self,
        url: &str,
        dest: &std::path::Path,
        cancel: tokio_util::sync::CancellationToken,
        mut progress: impl FnMut(u64, Option<u64>) + Send,
    ) -> Result<u64> {
        use futures_util::StreamExt;
        use tokio::io::AsyncWriteExt;
        let url = self.base.join(url)?;
        let resp = Self::check(self.http.get(url).send().await?).await?;
        let total = resp.content_length();
        let part = dest.with_file_name(format!("{}.part", dest.file_name().and_then(|n| n.to_str()).unwrap_or("download")));
        let mut file = tokio::fs::File::create(&part).await?;
        let mut stream = resp.bytes_stream();
        let mut done = 0u64;
        progress(0, total);
        let result: Result<()> = async {
            loop {
                let chunk = tokio::select! {
                    // cancel first: without `biased` a body that is already here could win and finish the download
                    biased;
                    _ = cancel.cancelled() => return Err(Error::Server { status: 0, message: "cancelled".into() }),
                    c = stream.next() => c,
                };
                let Some(chunk) = chunk else { break };
                let chunk = chunk?;
                file.write_all(&chunk).await?;
                done += chunk.len() as u64;
                progress(done, total);
            }
            file.flush().await?;
            Ok(())
        }
        .await;
        drop(file);
        if let Err(e) = result {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(e);
        }
        if total.is_some_and(|t| t != done) {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(Error::Server { status: 0, message: "the download ended early".into() });
        }
        tokio::fs::rename(&part, dest).await?;
        Ok(done)
    }

    /// `POST /api/batch/:id/zip`: the server packs a batch into one ZIP and returns its link.
    pub async fn batch_zip(&self, batch_id: &str) -> Result<String> {
        #[derive(serde::Deserialize)]
        struct ZipResponse {
            link: Option<String>,
        }
        let url = self.endpoint(&format!("api/batch/{}/zip", path_segment(batch_id)))?;
        let r: ZipResponse = Self::json(self.http.post(url).timeout(Duration::from_secs(600))).await?;
        r.link.ok_or_else(|| Error::Server { status: 0, message: "the server did not return a ZIP link".into() })
    }

    // ── upload (low level; orchestration is in `crate::upload`) ───────────

    pub async fn upload_init(&self, req: &UploadInit) -> Result<UploadInitResponse> {
        Self::json(self.http.post(self.endpoint("api/upload/init")?).json(req)).await
    }

    /// Fields are appended in the same order as the web client (`chunk` last).
    pub async fn upload_chunk(
        &self,
        upload_id: &str,
        index: u32,
        data: Bytes,
        is_last: Option<bool>,
    ) -> Result<ChunkAck> {
        let len = data.len() as u64;
        let part = multipart::Part::stream_with_length(reqwest::Body::from(data), len)
            .file_name("chunk")
            .mime_str("application/octet-stream")?;
        let mut form = multipart::Form::new()
            .text("uploadId", upload_id.to_owned())
            .text("chunkIndex", index.to_string())
            .text("chunkSize", len.to_string());
        if let Some(last) = is_last {
            form = form.text("isLastChunk", last.to_string());
        }
        let form = form.part("chunk", part);
        Self::json(self.http.post(self.endpoint("api/upload/chunk")?).multipart(form)).await
    }

    pub async fn upload_complete(&self, req: &UploadComplete) -> Result<UploadCompleteResponse> {
        Self::json(self.http.post(self.endpoint("api/upload/complete")?).json(req)).await
    }

    pub async fn upload_status(&self, upload_id: &str) -> Result<UploadStatus> {
        let url = self.endpoint(&format!("api/upload/status/{}", path_segment(upload_id)))?;
        Self::json(self.http.get(url).timeout(Duration::from_secs(30))).await
    }

    /// `DELETE /api/file/:id`
    pub async fn delete_file(&self, file_id: &str) -> Result<()> {
        let _ = Self::check(self.http.delete(self.endpoint(&format!("api/file/{}", path_segment(file_id)))?).send().await?).await?;
        Ok(())
    }

    /// `DELETE /api/upload/chunks/:uploadId`: wipe what an unfinished upload left on the server.
    pub async fn delete_upload_chunks(&self, upload_id: &str) -> Result<()> {
        let url = self.endpoint(&format!("api/upload/chunks/{}", path_segment(upload_id)))?;
        let _ = Self::check(self.http.delete(url).send().await?).await?;
        Ok(())
    }

    /// `POST /api/upload/batch/init` -> batch id.
    pub async fn batch_init(&self, expiration_minutes: u32) -> Result<String> {
        let body = serde_json::json!({ "expirationMinutes": expiration_minutes });
        let r: BatchInitResponse = Self::json(self.http.post(self.endpoint("api/upload/batch/init")?).json(&body)).await?;
        Ok(r.batch_id)
    }

    /// `POST /api/upload/batch/complete`: builds the landing page.
    pub async fn batch_complete(&self, batch_id: &str) -> Result<BatchCompleteResponse> {
        let body = serde_json::json!({ "batchId": batch_id });
        Self::json(self.http.post(self.endpoint("api/upload/batch/complete")?).json(&body)).await
    }

    /// `GET /api/upload/styled-link/:fileId` (ready a few seconds after an upload).
    pub async fn styled_link(&self, file_id: &str) -> Result<StyledLinkStatus> {
        let url = self.endpoint(&format!("api/upload/styled-link/{}", path_segment(file_id)))?;
        Self::json(self.http.get(url).timeout(Duration::from_secs(30))).await
    }

    /// `POST /api/probe`: ffprobe summary of a `files.tomasekvalla.cz` URL (the web's check page),
    /// returned as the raw body (see `probe::parse`). Errors carry the server's `error` text, or an
    /// empty message when the body was not JSON (the page then shows "Error <status>").
    pub async fn probe(&self, url: &str) -> Result<String> {
        let body = serde_json::json!({ "url": url });
        let resp = self.http.post(self.endpoint("api/probe")?).json(&body).timeout(Duration::from_secs(45)).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if status.is_success() {
            return Ok(text);
        }
        let message = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str().map(str::to_owned)))
            .unwrap_or_default();
        Err(Error::Server { status: status.as_u16(), message })
    }
}

/// Percent-encode one URL path segment (ids come from the server, but never trust them).
pub(crate) fn path_segment(s: &str) -> String {
    use percent_encoding::{AsciiSet, CONTROLS};
    const SEGMENT: &AsciiSet =
        &CONTROLS.add(b' ').add(b'"').add(b'#').add(b'%').add(b'/').add(b'\\').add(b'?').add(b'<').add(b'>').add(b'`').add(b'{').add(b'}');
    percent_encoding::utf8_percent_encode(s, SEGMENT).to_string()
}

/// One piece of a file (see `Client::fetch_range`).
#[derive(Debug, Clone)]
pub struct RangeData {
    /// Where `data` starts in the file.
    pub offset: u64,
    /// Size of the whole file, when the server said it.
    pub total: Option<u64>,
    pub data: Bytes,
}

#[cfg(test)]
mod download_tests {
    use super::*;
    use tokio_util::sync::CancellationToken;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn download_writes_the_file_and_reports_progress() {
        let server = MockServer::start().await;
        let body: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        Mock::given(method("GET")).and(path("/files/download/a.zip")).respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone())).mount(&server).await;
        let client = Client::with_base(&server.uri()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.zip");
        let mut last = (0, None);
        let n = client.download_to("/files/download/a.zip", &dest, CancellationToken::new(), |d, t| last = (d, t)).await.unwrap();
        assert_eq!(n, body.len() as u64);
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert_eq!(last, (body.len() as u64, Some(body.len() as u64)));
        assert!(!dir.path().join("a.zip.part").exists());
    }

    #[tokio::test]
    async fn local_files_are_read_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.md"), "# hi").unwrap();
        std::fs::write(dir.path().join("pic.png"), [1u8, 2, 3]).unwrap();
        let content = crate::content::Content::local(&dir.path().join("notes.md")).unwrap();
        assert_eq!(content.kind, crate::content::ContentKind::Markdown);
        assert_eq!(content.local_path().unwrap(), dir.path().join("notes.md"));
        let client = Client::with_base("https://example.invalid/").unwrap();
        assert_eq!(&client.fetch_bytes(&content.url, 100).await.unwrap()[..], b"# hi");
        // a picture the Markdown file links to, relative to it
        let pic = url::Url::parse(&content.url).unwrap().join("pic.png").unwrap();
        assert_eq!(&client.fetch_bytes(pic.as_str(), 100).await.unwrap()[..], [1, 2, 3]);
        assert!(client.fetch_bytes(&content.url, 2).await.is_err());
    }

    #[tokio::test]
    async fn cancelled_download_leaves_nothing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![1u8; 1000]).set_delay(std::time::Duration::from_millis(300)))
            .mount(&server)
            .await;
        let client = Client::with_base(&server.uri()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("b.bin");
        let token = CancellationToken::new();
        token.cancel();
        assert!(client.download_to("/x", &dest, token, |_, _| {}).await.is_err());
        assert!(!dest.exists());
        assert!(!dir.path().join("b.bin.part").exists());
    }

    /// Serves `body` honouring `Range: bytes=a-b` like nginx.
    struct Ranged(Vec<u8>);
    impl wiremock::Respond for Ranged {
        fn respond(&self, req: &wiremock::Request) -> ResponseTemplate {
            let total = self.0.len();
            let Some(r) = req.headers.get("range").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("bytes=")) else {
                return ResponseTemplate::new(200).set_body_bytes(self.0.clone());
            };
            let (a, b) = r.split_once('-').unwrap();
            let a: usize = a.parse().unwrap();
            let b: usize = b.parse::<usize>().unwrap().min(total - 1);
            ResponseTemplate::new(206)
                .insert_header("Content-Range", format!("bytes {a}-{b}/{total}").as_str())
                .set_body_bytes(self.0[a..=b].to_vec())
        }
    }

    #[tokio::test]
    async fn range_requests() {
        let server = MockServer::start().await;
        let body: Vec<u8> = (0..10_000u32).map(|i| (i % 253) as u8).collect();
        Mock::given(method("GET")).and(path("/files/audio/a.flac")).respond_with(Ranged(body.clone())).mount(&server).await;
        Mock::given(method("GET")).and(path("/plain")).respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone())).mount(&server).await;
        let client = Client::with_base(&server.uri()).unwrap();
        let r = client.fetch_range("/files/audio/a.flac", 1000, 500).await.unwrap();
        assert_eq!((r.offset, r.total), (1000, Some(10_000)));
        assert_eq!(&r.data[..], &body[1000..1500]);
        // past the end: the server trims the range
        let r = client.fetch_range("/files/audio/a.flac", 9_900, 500).await.unwrap();
        assert_eq!(&r.data[..], &body[9_900..]);
        // a server without range support sends everything from 0
        let r = client.fetch_range("/plain", 1000, 500).await.unwrap();
        assert_eq!((r.offset, r.total, r.data.len()), (0, Some(10_000), 10_000));
    }

    #[tokio::test]
    async fn missing_file_is_a_404_without_html() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(404).set_body_string("<html><body>404 Not Found</body></html>")).mount(&server).await;
        let client = Client::with_base(&server.uri()).unwrap();
        match client.fetch_bytes("/files/text/x.md", 1000).await {
            Err(Error::Server { status: 404, message }) => assert!(message.is_empty()),
            other => panic!("unexpected {other:?}"),
        }
    }
}
