//! Scripted engine test: `engine_test <file> [--device <id>] [--mute]` prints events and positions
//! (`--mute` sends silence to the device: no sound, same timing).
use std::time::{Duration, Instant};

use nativefs_audio::decode::Decoder;
use nativefs_audio::engine::{OutputSettings, PauseBehavior, Player};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let device = args.iter().position(|a| a == "--device").and_then(|i| args.get(i + 1)).cloned();
    let dec = Decoder::open_path(args[0].as_ref()).unwrap();
    let t0 = Instant::now();
    let p = Player::new(dec, OutputSettings { device: device.clone(), ..Default::default() }, Box::new(move |e| println!("{:6.2}s event {e:?}", t0.elapsed().as_secs_f64())));
    if args.iter().any(|a| a == "--mute") {
        p.set_muted(true);
    }
    let rate = p.info().rate as f64;
    let total = p.duration_frames().unwrap() as f64 / rate;
    let pos = |p: &Player| p.position() as f64 / rate;
    let step = |what: &str, p: &Player, wait: f64| {
        std::thread::sleep(Duration::from_secs_f64(wait));
        println!("{:6.2}s {what}: position {:.3} s, playing {}, underruns {}", t0.elapsed().as_secs_f64(), pos(p), p.is_playing(), p.underruns());
    };
    println!("track {total:.2} s");
    p.play();
    step("after 2 s play", &p, 2.0);
    p.pause();
    step("paused 1 s (release)", &p, 1.0);
    p.play();
    step("resumed 1 s", &p, 1.0);
    p.seek((60.0 * rate) as u64);
    step("seek 60 s, +0.5 s", &p, 0.5);
    p.seek(((total - 1.0) * rate) as u64);
    step("seek end-1 s, +1.6 s", &p, 1.6);
    p.set_loop(true);
    p.play();
    std::thread::sleep(Duration::from_millis(300));
    p.seek(((total - 1.0) * rate) as u64);
    step("loop: seek end-1 s, +1.5 s (should wrap to ~0.5)", &p, 1.5);
    // tempo 1.25 with record-style pitch: the track time must run 1.25x faster
    p.set_dsp(nativefs_audio::dsp::Params { tempo: 1.25, nightcore: 100.0, ..Default::default() });
    let a = pos(&p);
    step("tempo 1.25x, +2 s (track time should move ~2.5 s)", &p, 2.0);
    println!("        track time moved {:.2} s, dsp active {}", pos(&p) - a, p.dsp_active());
    p.set_dsp(nativefs_audio::dsp::Params { tempo: 0.8, ..Default::default() });
    let a = pos(&p);
    step("time-stretch 0.8x, +2 s (track time should move ~1.6 s)", &p, 2.0);
    println!("        track time moved {:.2} s", pos(&p) - a);
    p.set_dsp(nativefs_audio::dsp::Params::default());
    step("neutral again, +0.5 s", &p, 0.5);
    println!("        dsp active {}", p.dsp_active());
    p.set_output(OutputSettings { device, pause: PauseBehavior::Keep, ..Default::default() });
    step("output reopened (keep on pause), +0.5 s", &p, 0.5);
    p.pause();
    step("paused 1 s (keep)", &p, 1.0);
    p.play();
    step("resumed 0.5 s", &p, 0.5);
    drop(p);
    println!("{:6.2}s dropped", t0.elapsed().as_secs_f64());
}
