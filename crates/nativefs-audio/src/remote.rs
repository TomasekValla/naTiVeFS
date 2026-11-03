//! A file streamed over HTTP in pieces (`Range` requests), seekable for the decoder. A background
//! thread downloads the file in order, starting where the decoder reads, so playback starts after
//! the first piece and a seek fetches the new spot first. The pieces stay in memory (the whole file
//! once it is complete, so seeking back never downloads again).

use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use symphonia::core::io::MediaSource;

/// Size of one request.
pub const CHUNK: u64 = 512 * 1024;

/// First pause before a failed request is retried (doubles each time, 4 retries).
#[cfg(not(test))]
const RETRY_DELAY: Duration = Duration::from_millis(500);
#[cfg(test)]
const RETRY_DELAY: Duration = Duration::from_millis(5);

/// One answered request: `data` starts at `offset`; `total` is the file size when the server said it.
pub struct Fetched {
    pub offset: u64,
    pub total: Option<u64>,
    pub data: Vec<u8>,
}

/// Blocking `fetch(start, len)`: the app implements it with its HTTP client (session cookie included).
pub type FetchFn = Arc<dyn Fn(u64, u64) -> Result<Fetched, String> + Send + Sync>;

struct Cache {
    chunks: Vec<Option<Arc<[u8]>>>,
    total: u64,
    /// The chunk the reader needs next: the downloader continues from here.
    want: usize,
    error: Option<String>,
    closed: bool,
}

struct Shared {
    cache: Mutex<Cache>,
    cond: Condvar,
}

/// Download progress for the seek bar.
pub struct Progress {
    pub total: u64,
    /// Downloaded byte ranges `[start, end)`, sorted.
    pub ranges: Vec<(u64, u64)>,
}

/// Handle kept by the UI for progress; the source itself goes to the decoder.
#[derive(Clone)]
pub struct RemoteHandle(Arc<Shared>);

impl RemoteHandle {
    pub fn progress(&self) -> Progress {
        let c = self.0.cache.lock().unwrap();
        let mut ranges: Vec<(u64, u64)> = Vec::new();
        for (i, ch) in c.chunks.iter().enumerate() {
            if let Some(ch) = ch {
                let (a, b) = (i as u64 * CHUNK, i as u64 * CHUNK + ch.len() as u64);
                match ranges.last_mut() {
                    Some(last) if last.1 == a => last.1 = b,
                    _ => ranges.push((a, b)),
                }
            }
        }
        Progress { total: c.total, ranges }
    }

    pub fn complete(&self) -> bool {
        self.0.cache.lock().unwrap().chunks.iter().all(Option::is_some)
    }

    /// The whole file, once it is downloaded (for "Download" without downloading it again).
    pub fn bytes(&self) -> Option<Vec<u8>> {
        let c = self.0.cache.lock().unwrap();
        let mut out = Vec::with_capacity(c.total as usize);
        for ch in &c.chunks {
            out.extend_from_slice(ch.as_ref()?);
        }
        Some(out)
    }
}

impl Drop for RemoteSource {
    fn drop(&mut self) {
        let mut c = self.shared.cache.lock().unwrap();
        c.closed = true;
        self.shared.cond.notify_all();
    }
}

pub struct RemoteSource {
    shared: Arc<Shared>,
    pos: u64,
    total: u64,
}

impl RemoteSource {
    /// Fetches the first piece (to learn the size) and starts the downloader.
    pub fn open(fetch: FetchFn) -> Result<(RemoteSource, RemoteHandle), String> {
        let first = fetch(0, CHUNK)?;
        let total = first.total.ok_or("the server did not say how big the file is")?;
        let n = total.div_ceil(CHUNK) as usize;
        let mut chunks: Vec<Option<Arc<[u8]>>> = vec![None; n];
        store(&mut chunks, &first);
        let shared = Arc::new(Shared { cache: Mutex::new(Cache { chunks, total, want: 0, error: None, closed: false }), cond: Condvar::new() });
        let sh = shared.clone();
        std::thread::Builder::new().name("audio-download".into()).spawn(move || download(sh, fetch)).map_err(|e| e.to_string())?;
        Ok((RemoteSource { shared: shared.clone(), pos: 0, total }, RemoteHandle(shared)))
    }
}

/// Puts a response into the chunk table (a 200 answer may carry the whole file).
fn store(chunks: &mut [Option<Arc<[u8]>>], f: &Fetched) {
    let mut off = f.offset;
    let mut rest = &f.data[..];
    while !rest.is_empty() {
        let i = (off / CHUNK) as usize;
        if i >= chunks.len() {
            break;
        }
        let take = (CHUNK - off % CHUNK).min(rest.len() as u64) as usize;
        if off.is_multiple_of(CHUNK) && (take as u64 == CHUNK || i == chunks.len() - 1) && chunks[i].is_none() {
            chunks[i] = Some(Arc::from(&rest[..take]));
        }
        off += take as u64;
        rest = &rest[take..];
    }
}

fn download(sh: Arc<Shared>, fetch: FetchFn) {
    loop {
        // next missing chunk from where the reader is, then the ones before it
        let next = {
            let c = sh.cache.lock().unwrap();
            if c.closed {
                return;
            }
            let n = c.chunks.len();
            match (c.want..n).chain(0..c.want.min(n)).find(|&i| c.chunks[i].is_none()) {
                Some(i) => i,
                // complete: nothing left to do
                None => return,
            }
        };
        let mut tries = 0;
        let result = loop {
            match fetch(next as u64 * CHUNK, CHUNK) {
                Ok(f) => break Ok(f),
                Err(e) if tries < 4 => {
                    tries += 1;
                    std::thread::sleep(RETRY_DELAY * (1 << tries));
                    if sh.cache.lock().unwrap().closed {
                        return;
                    }
                    let _ = e;
                }
                Err(e) => break Err(e),
            }
        };
        let mut c = sh.cache.lock().unwrap();
        match result {
            Ok(f) => store(&mut c.chunks, &f),
            Err(e) => {
                c.error = Some(e);
                sh.cond.notify_all();
                return;
            }
        }
        sh.cond.notify_all();
    }
}

impl Read for RemoteSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.total || buf.is_empty() {
            return Ok(0);
        }
        let i = (self.pos / CHUNK) as usize;
        let mut c = self.shared.cache.lock().unwrap();
        c.want = i;
        let chunk = loop {
            if let Some(ch) = &c.chunks[i] {
                break ch.clone();
            }
            if let Some(e) = &c.error {
                return Err(std::io::Error::other(e.clone()));
            }
            c = self.shared.cond.wait(c).unwrap();
        };
        drop(c);
        let off = (self.pos % CHUNK) as usize;
        let n = buf.len().min(chunk.len() - off);
        buf[..n].copy_from_slice(&chunk[off..off + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for RemoteSource {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let p = match to {
            SeekFrom::Start(p) => p as i64,
            SeekFrom::End(d) => self.total as i64 + d,
            SeekFrom::Current(d) => self.pos as i64 + d,
        };
        if p < 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek before the start"));
        }
        self.pos = p as u64;
        if self.pos < self.total {
            let mut c = self.shared.cache.lock().unwrap();
            c.want = (self.pos / CHUNK) as usize;
        }
        Ok(self.pos)
    }
}

impl MediaSource for RemoteSource {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn serve(body: Vec<u8>, calls: Arc<AtomicUsize>, ranged: bool) -> FetchFn {
        Arc::new(move |start, len| {
            calls.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(2));
            if !ranged {
                return Ok(Fetched { offset: 0, total: Some(body.len() as u64), data: body.clone() });
            }
            let end = (start + len).min(body.len() as u64) as usize;
            Ok(Fetched { offset: start, total: Some(body.len() as u64), data: body[start as usize..end].to_vec() })
        })
    }

    #[test]
    fn reads_and_seeks_like_a_file() {
        let body: Vec<u8> = (0..(CHUNK as usize * 5 + 1234)).map(|i| (i * 7 % 251) as u8).collect();
        for ranged in [true, false] {
            let calls = Arc::new(AtomicUsize::new(0));
            let (mut src, handle) = RemoteSource::open(serve(body.clone(), calls.clone(), ranged)).unwrap();
            assert_eq!(src.byte_len(), Some(body.len() as u64));
            // across a chunk border
            src.seek(SeekFrom::Start(CHUNK * 3 - 10)).unwrap();
            let mut buf = vec![0; 100];
            src.read_exact(&mut buf).unwrap();
            assert_eq!(&buf[..], &body[(CHUNK * 3 - 10) as usize..(CHUNK * 3 + 90) as usize]);
            // the tail and everything
            src.seek(SeekFrom::End(-5)).unwrap();
            let mut tail = Vec::new();
            src.read_to_end(&mut tail).unwrap();
            assert_eq!(&tail[..], &body[body.len() - 5..]);
            src.seek(SeekFrom::Start(0)).unwrap();
            let mut all = Vec::new();
            src.read_to_end(&mut all).unwrap();
            assert_eq!(all, body);
            assert!(handle.complete());
            assert_eq!(handle.progress().ranges, vec![(0, body.len() as u64)]);
            assert_eq!(handle.bytes().unwrap(), body);
            if !ranged {
                assert_eq!(calls.load(Ordering::Relaxed), 1, "a 200 answer already holds the whole file");
            }
        }
    }

    #[test]
    fn errors_reach_the_reader() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let fetch: FetchFn = Arc::new(move |start, len| {
            if c.fetch_add(1, Ordering::Relaxed) == 0 {
                Ok(Fetched { offset: start, total: Some(CHUNK * 3), data: vec![1; len as usize] })
            } else {
                Err("offline".into())
            }
        });
        let (mut src, _h) = RemoteSource::open(fetch).unwrap();
        src.seek(SeekFrom::Start(CHUNK * 2)).unwrap();
        let mut buf = [0u8; 10];
        assert!(src.read(&mut buf).is_err());
    }
}
