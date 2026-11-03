//! Renders the web comparison cases through the native chain:
//! `dsp_render <dir>` reads `<dir>/web_input.f32` (interleaved stereo f32, 44.1 kHz) and writes
//! `<dir>/native_<case>.f32` (see the lab's offline render of the web engine).
use nativefs_audio::dsp::{Chain, Params};

fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("dir"));
    let raw = std::fs::read(dir.join("web_input.f32")).unwrap();
    let input: Vec<i32> = raw
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| (f32::from_le_bytes(*b) as f64 * 2_147_483_648.0).round().clamp(i32::MIN as f64, i32::MAX as f64) as i32)
        .collect();
    // the lab page knows the track's BPM (123): the web's Echo time default is the 1/8 note
    let base = Params { echo_time: 60000.0 / 123.0 * 0.5, ..Params::default() };
    let mut eq_1k = base.clone();
    eq_1k.eq[3].gain = 6.0;
    let mut eq_shelf = base.clone();
    eq_shelf.eq[0].gain = 6.0;
    eq_shelf.eq[0].freq = 90.0;
    let cases: Vec<(&str, Params)> = vec![
        ("neutral", base.clone()),
        ("eq_1k_plus6", eq_1k),
        ("eq_lowshelf90_plus6", eq_shelf),
        ("filter_m40", Params { filter: -40.0, ..base.clone() }),
        ("filter_p40", Params { filter: 40.0, ..base.clone() }),
        ("width0", Params { width: 0.0, ..base.clone() }),
        ("width180", Params { width: 180.0, ..base.clone() }),
        ("rotate50", Params { rotate: 50.0, ..base.clone() }),
        ("volume150", Params { volume: 150.0, ..base.clone() }),
        ("volume50", Params { volume: 50.0, ..base.clone() }),
        ("pitch7", Params { pitch: 7.0, ..base.clone() }),
        ("reverb40", Params { reverb: 40.0, room: 70.0, ..base.clone() }),
        ("echo40", Params { echo: 40.0, ..base.clone() }),
    ];
    for (name, p) in cases {
        let mut c = Chain::new(44100);
        c.set(&p);
        c.reset(); // start settled, like the web's instant settings
        let mut out = Vec::new();
        for chunk in input.chunks(4096 * 2) {
            c.process(chunk, &mut out, 32);
        }
        let bytes: Vec<u8> = out.iter().flat_map(|s| ((*s as f64 / 2_147_483_648.0) as f32).to_le_bytes()).collect();
        std::fs::write(dir.join(format!("native_{name}.f32")), bytes).unwrap();
        println!("{name}: {} frames", out.len() / 2);
    }
}
