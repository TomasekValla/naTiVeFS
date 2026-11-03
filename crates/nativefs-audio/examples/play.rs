//! Output test: `play --list`, or `play <file> [--device <id>] [--shared] [--silent] [--secs N] [--hash]`.
//! `--silent` negotiates the file's format but sends digital silence (format / stability tests
//! without sound). `--hash` hashes the samples handed to the device (compare with `decode_raw`).
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use nativefs_audio::decode::Decoder;
use nativefs_audio::output::wasapi::{devices, Stream};
use nativefs_audio::output::{fidelity, ShareMode};
use nativefs_audio::ring::ring;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let value = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned();
    if flag("--list") {
        for d in devices().unwrap() {
            let vol = nativefs_audio::output::wasapi::endpoint_volume(Some(&d.id));
            println!("{d:?} volume {vol:?}");
        }
        return;
    }
    let mut dec = Decoder::open_path(args[0].as_ref()).expect("open");
    let src = dec.info().clone();
    let mode = if flag("--shared") { ShareMode::Shared } else { ShareMode::Exclusive };
    let silent = flag("--silent");
    let secs: f64 = value("--secs").map(|s| s.parse().unwrap()).unwrap_or(10.0);
    let (mut prod, mut cons) = ring(src.rate as usize * src.channels as usize); // 1 s
    let hash = Arc::new(AtomicU64::new(0xcbf29ce484222325));
    let h = hash.clone();
    let fill = Box::new(move |out: &mut [i32]| {
        let n = cons.pop(out);
        // FNV-1a over the samples as they leave the pipeline
        let mut x = h.load(Ordering::Relaxed);
        for s in &out[..n] {
            for b in s.to_le_bytes() {
                x = (x ^ b as u64).wrapping_mul(0x100000001b3);
            }
        }
        h.store(x, Ordering::Relaxed);
        if silent {
            out[..n].fill(0);
        }
        n
    });
    // decode ahead before opening, like the player will
    let mut buf = Vec::new();
    let mut pending: Vec<i32> = Vec::new();
    let mut ended = false;
    let pump = |prod: &mut nativefs_audio::ring::Producer, pending: &mut Vec<i32>, buf: &mut Vec<i32>, dec: &mut Decoder, ended: &mut bool| {
        loop {
            if !pending.is_empty() {
                let n = prod.push(pending);
                pending.drain(..n);
                if !pending.is_empty() {
                    return;
                }
            }
            buf.clear();
            if *ended || !dec.next(buf).expect("decode") {
                *ended = true;
                return;
            }
            std::mem::swap(pending, buf);
        }
    };
    pump(&mut prod, &mut pending, &mut buf, &mut dec, &mut ended);
    let t0 = Instant::now();
    let stream = match Stream::open(value("--device").as_deref(), &src, mode, fill) {
        Ok(s) => s,
        Err(e) => {
            println!("OPEN FAILED: {e}");
            return;
        }
    };
    println!("source {src:?}");
    println!("device {:?}", stream.device);
    println!("format {:?} mode {:?} opened in {:?}", stream.format, stream.mode, t0.elapsed());
    println!("fidelity {:?}", fidelity(&src, &stream.format, stream.mode, &stream.device));
    let start = Instant::now();
    while start.elapsed().as_secs_f64() < secs && !(ended && pending.is_empty()) {
        pump(&mut prod, &mut pending, &mut buf, &mut dec, &mut ended);
        std::thread::sleep(Duration::from_millis(20));
    }
    // let the ring drain: wait until every decoded frame reached the device
    let total = dec.position();
    while ended && stream.frames.load(Ordering::Relaxed) < total && start.elapsed().as_secs_f64() < secs + 3.0 {
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(100));
    let frames = stream.frames.load(Ordering::Relaxed);
    println!("played {frames} frames in {:.2} s ({:.1} frames/s), underruns {}", start.elapsed().as_secs_f64(), frames as f64 / start.elapsed().as_secs_f64(), stream.underruns.load(Ordering::Relaxed));
    drop(stream);
    if flag("--hash") {
        println!("hash {:016x}", hash.load(Ordering::Relaxed));
    }
}
