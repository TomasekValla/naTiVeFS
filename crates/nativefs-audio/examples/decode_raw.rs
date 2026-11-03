//! Decodes a file to raw little-endian PCM at its own bit depth (for checking against ffmpeg):
//! `cargo run -p nativefs-audio --example decode_raw -- <in> <out.raw> [seek-frame]`
//! With `SIM_REMOTE=1` the file is read through `RemoteSource` in 512 KiB ranges with 20 ms latency.
use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut dec = if std::env::var_os("SIM_REMOTE").is_some() {
        use nativefs_audio::remote::{Fetched, RemoteSource};
        let body = std::sync::Arc::new(std::fs::read(&args[1]).unwrap());
        let fetch: nativefs_audio::remote::FetchFn = std::sync::Arc::new(move |start, len| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            let end = (start + len).min(body.len() as u64) as usize;
            Ok(Fetched { offset: start, total: Some(body.len() as u64), data: body[start as usize..end].to_vec() })
        });
        let t = std::time::Instant::now();
        let (src, _h) = RemoteSource::open(fetch).unwrap();
        let d = nativefs_audio::decode::Decoder::open(Box::new(src), Some("flac")).expect("open");
        eprintln!("remote open + probe in {:?}", t.elapsed());
        d
    } else {
        nativefs_audio::decode::Decoder::open_path(args[1].as_ref()).expect("open")
    };
    if let Some(f) = args.get(3) {
        dec.seek(f.parse().unwrap()).expect("seek");
    }
    let info = dec.info().clone();
    let bits = info.bits.unwrap_or(32);
    let bytes = bits.div_ceil(8) as usize;
    let mut out = std::io::BufWriter::new(std::fs::File::create(&args[2]).unwrap());
    let mut buf = Vec::new();
    let mut frames = 0u64;
    while { buf.clear(); dec.next(&mut buf).expect("decode") } {
        for s in &buf {
            out.write_all(&(s >> (32 - bytes * 8)).to_le_bytes()[..bytes]).unwrap();
        }
        frames += (buf.len() / info.channels as usize) as u64;
    }
    eprintln!("{info:?} frames={frames} total={:?} pos={}", dec.total_frames(), dec.position());
}
