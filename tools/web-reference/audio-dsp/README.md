# Native DSP vs the web player's engine

1. Start the lab (a local copy of the TVFS web audio player, `node sandbox/server.js`), open `/new.html`.
2. Run `render_web.js` in the page console (or through the browser tool): it renders the same 6 s of the track through
   `TVFSPlayer.createEngine` in an `OfflineAudioContext` for every case and POSTs `web_input.f32` and `web_<case>.f32`
   (interleaved stereo f32) to a local receiver on port 5199 that writes them into a folder.
3. `cargo run -p nativefs-audio --example dsp_render -- <folder>` renders the same cases natively (`native_<case>.f32`).
4. `python -I compare.py <folder>`: aligns each pair (cross-correlation) and prints the difference (dB below the signal),
   RMS, peak and side/mid level.

Result 2026-11-01 (Montagem Nada Tropica, -4.7 LUFS master, 30 ms limiter release): levels within 0.17 dB in every
case; volume 50 / 150 % -97 / -102 dB; width -39 dB; EQ / filter / 8D / echo -20 to -30 dB (limiter dynamics);
pitch and reverb only by level (WSOLA alignment and the random reverb tail differ by design). Neutral: the web is
-69 dB off its input, native is bit-identical.
