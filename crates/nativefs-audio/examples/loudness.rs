//! `loudness <file>`: integrated loudness (compare with `ffmpeg -i <file> -af ebur128 -f null -`).
fn main() {
    let path = std::env::args().nth(1).expect("file");
    let t = std::time::Instant::now();
    let mut dec = nativefs_audio::decode::Decoder::open_path(path.as_ref()).unwrap();
    println!("{:?} LUFS in {:?}", nativefs_audio::analysis::integrated_loudness(&mut dec), t.elapsed());
}
