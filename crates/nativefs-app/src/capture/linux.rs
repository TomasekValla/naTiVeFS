//! Linux capture through the system `ffmpeg` (installed with mpv on Arch; a package dependency
//! elsewhere): one ffmpeg reads the camera (V4L2) and pipes raw BGRA frames to the app (preview,
//! photo), one reads the microphone (PulseAudio / PipeWire) for the level meter, and a recording
//! runs a third one that encodes the frames from the app plus the microphone into MP4 (H.264 + AAC).
//! No build-time dependency (no C headers, no bindgen).

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::{peak_i16, Device, Event, Frame, Sink, AUDIO_BITRATE, MAX_H, MAX_W, VIDEO_BITRATE};

fn ffmpeg() -> Command {
    let mut c = Command::new("ffmpeg");
    c.args(["-hide_banner", "-nostdin", "-loglevel", "error"]);
    c
}

fn missing_ffmpeg(e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::NotFound {
        "Camera capture needs ffmpeg. Install it (e.g. `sudo pacman -S ffmpeg` or `sudo apt install ffmpeg`).".into()
    } else {
        format!("ffmpeg could not start: {e}")
    }
}

/// `/dev/videoN` capture nodes with their names (`index` 0 = the picture node; 1 = metadata).
pub fn cameras() -> Result<Vec<Device>, String> {
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir("/sys/class/video4linux") else { return Ok(out) };
    let mut nodes: Vec<_> = dir.flatten().map(|e| e.path()).collect();
    nodes.sort();
    for p in nodes {
        let node = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_owned();
        if !node.starts_with("video") {
            continue;
        }
        let index = std::fs::read_to_string(p.join("index")).ok().and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(0);
        if index != 0 {
            continue;
        }
        let name = std::fs::read_to_string(p.join("name")).map(|s| s.trim().to_owned()).unwrap_or_else(|_| node.clone());
        out.push(Device { id: format!("/dev/{node}"), name });
    }
    Ok(out)
}

/// PulseAudio / PipeWire sources (without the ".monitor" loopbacks of outputs).
/// The microphone alone (voice notes): `ffmpeg -f pulse` as 48 kHz stereo 16-bit PCM.
pub struct Mic {
    child: std::process::Child,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Mic {
    pub fn start(id: &str, pcm: super::PcmSink, sink: Sink) -> Result<Self, String> {
        let chosen;
        let src = if id.is_empty() || id == "default" {
            chosen = default_source()?;
            chosen.as_str()
        } else {
            id
        };
        let mut child = ffmpeg()
            .args(["-f", "pulse", "-i", src, "-ac", "2", "-ar", "48000", "-f", "s16le", "pipe:1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| missing_ffmpeg(&e))?;
        let mut out = child.stdout.take().ok_or_else(|| "The microphone gave no sound.".to_owned())?;
        let stop = Arc::new(AtomicBool::new(false));
        let st = stop.clone();
        let thread = std::thread::spawn(move || {
            // 50 ms pieces
            let mut buf = vec![0u8; 48000 / 20 * 4];
            while !st.load(Ordering::Relaxed) && out.read_exact(&mut buf).is_ok() {
                let samples: Vec<i16> = buf.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
                sink(Event::Level(peak_i16(&samples)));
                pcm(&samples, 48000, 2);
            }
        });
        Ok(Self { child, stop, thread: Some(thread) })
    }
}

impl Drop for Mic {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Encode 16-bit PCM to AAC in an M4A file with the system ffmpeg.
pub fn encode_aac(samples: &[i16], rate: u32, ch: u16, kbps: u32, path: &Path) -> Result<(), String> {
    let mut child = ffmpeg()
        .args(["-y", "-f", "s16le", "-ar", &rate.to_string(), "-ac", &ch.to_string(), "-i", "pipe:0", "-c:a", "aac", "-b:a", &format!("{kbps}k")])
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| missing_ffmpeg(&e))?;
    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    child.stdin.take().ok_or_else(|| "ffmpeg has no input".to_owned())?.write_all(&bytes).map_err(|e| e.to_string())?;
    let status = child.wait().map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err("ffmpeg could not encode AAC.".into()) }
}

pub fn microphones() -> Result<Vec<Device>, String> {
    let mut list = vec![Device { id: "default".into(), name: "Default microphone".into() }];
    list.extend(real_sources()?.0);
    Ok(list)
}

/// The real input sources (no ".monitor" loopbacks of outputs) and which one is the system default, if any.
fn real_sources() -> Result<(Vec<Device>, Option<String>), String> {
    let out = ffmpeg().args(["-sources", "pulse"]).stderr(Stdio::null()).output().map_err(|e| missing_ffmpeg(&e))?;
    Ok(parse_sources(&String::from_utf8_lossy(&out.stdout)))
}

fn parse_sources(text: &str) -> (Vec<Device>, Option<String>) {
    let (mut list, mut default) = (Vec::new(), None);
    for line in text.lines() {
        // "* alsa_input.usb-HyperX_SoloCast-00.mono-fallback [HyperX SoloCast Mono]" (`*` = the default)
        let line = line.trim();
        let is_default = line.starts_with('*');
        let line = line.trim_start_matches('*').trim();
        let Some((id, rest)) = line.split_once(' ') else { continue };
        if id.ends_with(".monitor") || !id.contains('.') {
            continue;
        }
        if is_default {
            default = Some(id.to_owned());
        }
        // "[Name] (none)": the name is inside the brackets, ffmpeg may add details after them
        let rest = rest.trim();
        let name = match (rest.find('['), rest.rfind(']')) {
            (Some(a), Some(b)) if b > a => rest[a + 1..b].to_owned(),
            _ => rest.to_owned(),
        };
        list.push(Device { id: id.to_owned(), name });
    }
    (list, default)
}

/// "Default microphone": the system default when it is a real input, else the first microphone. The default can be
/// an output's monitor (e.g. the sound card runs an output-only profile), which would record silence.
fn default_source() -> Result<String, String> {
    let (list, default) = real_sources()?;
    default.or_else(|| list.first().map(|d| d.id.clone())).ok_or_else(|| {
        "No microphone was found. In the system sound settings, check that the microphone is on (the sound card may be set to an output-only profile).".to_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_default_is_skipped() {
        let (list, default) = parse_sources("Auto-detected sources for pulse:\n* alsa_output.pci-0000_05_00.6.analog-stereo.monitor [Monitor of Built-in Audio]\n  alsa_input.pci-0000_05_00.6.analog-stereo [Built-in Audio Analog Stereo]\n");
        assert_eq!(default, None);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "alsa_input.pci-0000_05_00.6.analog-stereo");
    }

    #[test]
    fn only_monitors_means_no_microphone() {
        let (list, default) = parse_sources("* alsa_output.pci-0000_05_00.6.analog-stereo.monitor [Monitor of Built-in Audio]\n");
        assert!(list.is_empty() && default.is_none());
    }

    #[test]
    fn names_drop_ffmpeg_details() {
        let (list, default) = parse_sources("  bluez_output.03_00.1.monitor [Monitor of Headset] (none)\n* bluez_input.03:00:60 [Headset] (none)\n");
        assert_eq!(default.as_deref(), Some("bluez_input.03:00:60"));
        assert_eq!(list[0].name, "Headset");
    }
}

struct Shared {
    stop: AtomicBool,
    latest: Mutex<Option<Frame>>,
    encoder: Mutex<Option<(Child, ChildStdin, PathBuf)>>,
}

pub struct Session {
    shared: Arc<Shared>,
    children: Arc<Mutex<Vec<Child>>>,
    threads: Vec<JoinHandle<()>>,
    size: (u32, u32),
    mic: Option<String>,
}

/// Camera modes tried in order until one delivers frames (MJPEG first: the full-HD modes of USB cameras).
const MODES: [(Option<&str>, u32, u32); 4] = [(Some("mjpeg"), 1920, 1080), (Some("mjpeg"), 1280, 720), (None, 1280, 720), (None, 640, 480)];

impl Session {
    pub fn start(camera: &str, mic: Option<&str>, sink: Sink) -> Result<Self, String> {
        // "Default microphone" like the voice note (never an output monitor); without a microphone: video only
        let resolved = match mic {
            Some(m) if m.is_empty() || m == "default" => default_source().ok(),
            m => m.map(str::to_owned),
        };
        let mic = resolved.as_deref();
        let shared = Arc::new(Shared { stop: AtomicBool::new(false), latest: Mutex::new(None), encoder: Mutex::new(None) });
        let children: Arc<Mutex<Vec<Child>>> = Arc::default();
        let mut threads = Vec::new();
        let mut last_err = String::from("The camera did not start.");
        let mut started = None;
        for (fmt, w, h) in MODES {
            if w > MAX_W || h > MAX_H {
                continue;
            }
            let mut cmd = ffmpeg();
            let size = format!("{w}x{h}");
            let filter = format!("scale={w}:{h},format=bgra");
            cmd.args(["-f", "v4l2", "-framerate", "30", "-video_size", size.as_str()]);
            if let Some(f) = fmt {
                cmd.args(["-input_format", f]);
            }
            cmd.args(["-i", camera, "-vf", filter.as_str(), "-f", "rawvideo", "-pix_fmt", "bgra", "pipe:1"]);
            let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| missing_ffmpeg(&e))?;
            let mut stdout = child.stdout.take().unwrap();
            let stderr = child.stderr.take().unwrap();
            // the first frame within a few seconds means this mode works
            let (tx, rx) = mpsc::channel();
            let frame_len = (w * h * 4) as usize;
            std::thread::spawn(move || {
                let mut buf = vec![0u8; frame_len];
                let r = stdout.read_exact(&mut buf).map(|_| (stdout, buf));
                let _ = tx.send(r);
            });
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(Ok((stdout, first))) => {
                    started = Some((child, stdout, stderr, w, h, first));
                    break;
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let mut msg = String::new();
                    let _ = BufReader::new(stderr).read_to_string(&mut msg);
                    if msg.contains("Permission denied") {
                        last_err = "No permission to use the camera (is your user in the `video` group?).".into();
                    } else if msg.contains("busy") {
                        last_err = "The camera is busy (another app is using it).".into();
                    } else if !msg.trim().is_empty() {
                        last_err = format!("The camera did not start: {}", msg.lines().last().unwrap_or("").trim());
                    }
                }
            }
        }
        let Some((child, mut stdout, stderr, w, h, first)) = started else { return Err(last_err) };
        children.lock().unwrap().push(child);
        sink(Event::Mode(format!("{w} × {h} · 30 fps")));
        // camera frames
        {
            let (sh, sink) = (shared.clone(), sink.clone());
            threads.push(std::thread::spawn(move || {
                let len = (w * h * 4) as usize;
                let mut buf = first;
                loop {
                    let frame = Frame { bgra: Arc::new(std::mem::replace(&mut buf, vec![0u8; len])), width: w, height: h };
                    *sh.latest.lock().unwrap() = Some(frame.clone());
                    // a recording takes every frame
                    if let Some((_, stdin, _)) = sh.encoder.lock().unwrap().as_mut() {
                        if stdin.write_all(&frame.bgra).is_err() {
                            sink(Event::Error("Recording failed (the encoder stopped).".into()));
                        }
                    }
                    sink(Event::Frame(frame));
                    if sh.stop.load(Ordering::Relaxed) || stdout.read_exact(&mut buf).is_err() {
                        break;
                    }
                }
                if !sh.stop.load(Ordering::Relaxed) {
                    let mut msg = String::new();
                    let _ = BufReader::new(stderr).read_to_string(&mut msg);
                    sink(Event::Error(format!("The camera stopped. {}", msg.lines().last().unwrap_or("").trim())));
                }
            }));
        }
        // microphone level meter
        if let Some(src) = mic {
            match ffmpeg()
                .args(["-f", "pulse", "-i", src, "-ac", "2", "-ar", "48000", "-f", "s16le", "pipe:1"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(mut child) => {
                    let mut out = child.stdout.take().unwrap();
                    children.lock().unwrap().push(child);
                    let (sh, sink) = (shared.clone(), sink.clone());
                    threads.push(std::thread::spawn(move || {
                        // 50 ms of 48 kHz stereo 16-bit
                        let mut buf = vec![0u8; 48000 / 20 * 4];
                        while !sh.stop.load(Ordering::Relaxed) && out.read_exact(&mut buf).is_ok() {
                            let pcm: Vec<i16> = buf.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
                            sink(Event::Level(peak_i16(&pcm)));
                        }
                    }));
                }
                Err(e) => sink(Event::Error(format!("The microphone could not start: {e}"))),
            }
        }
        Ok(Self { shared, children, threads, size: (w, h), mic: mic.map(str::to_owned) })
    }

    pub fn latest(&self) -> Option<Frame> {
        self.shared.latest.lock().unwrap().clone()
    }

    pub fn start_recording(&self, path: &Path) -> Result<(), String> {
        let (w, h) = self.size;
        let scale = (w as f64 * h as f64 / (1920.0 * 1080.0)).clamp(0.25, 1.0);
        let vb = format!("{}", (VIDEO_BITRATE as f64 * scale) as u32);
        let buffer = format!("{}", (VIDEO_BITRATE as f64 * scale * 2.0) as u32);
        let size = format!("{w}x{h}");
        let ab = AUDIO_BITRATE.to_string();
        let mut cmd = ffmpeg();
        cmd.args(["-y", "-thread_queue_size", "512", "-f", "rawvideo", "-pix_fmt", "bgra", "-video_size", size.as_str()]);
        cmd.args(["-use_wallclock_as_timestamps", "1", "-i", "pipe:0"]);
        if let Some(src) = &self.mic {
            cmd.args(["-thread_queue_size", "512", "-f", "pulse", "-use_wallclock_as_timestamps", "1", "-i", src.as_str()]);
        }
        cmd.args(["-c:v", "libx264", "-preset", "veryfast", "-b:v", vb.as_str(), "-maxrate", vb.as_str(), "-bufsize", buffer.as_str()]);
        cmd.args(["-pix_fmt", "yuv420p", "-fps_mode", "cfr", "-r", "30"]);
        if self.mic.is_some() {
            cmd.args(["-c:a", "aac", "-b:a", ab.as_str(), "-shortest"]);
        }
        cmd.args(["-movflags", "+faststart"]).arg(path);
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().map_err(|e| missing_ffmpeg(&e))?;
        let stdin = child.stdin.take().unwrap();
        // collect the encoder's complaints for the error message
        if let Some(err) = child.stderr.take() {
            std::thread::spawn(move || {
                for line in BufReader::new(err).lines().map_while(Result::ok) {
                    eprintln!("naTiVeFS: ffmpeg: {line}");
                }
            });
        }
        *self.shared.encoder.lock().unwrap() = Some((child, stdin, path.to_owned()));
        Ok(())
    }

    pub fn stop_recording(&self) -> Result<PathBuf, String> {
        let taken = self.shared.encoder.lock().unwrap().take();
        let Some((mut child, stdin, path)) = taken else { return Err("Not recording.".into()) };
        // end of the video input: ffmpeg finishes the file (the microphone input ends with -shortest)
        drop(stdin);
        let t0 = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(path),
                Ok(Some(status)) => return Err(format!("Finishing the video failed (ffmpeg exit {status}).")),
                Ok(None) if t0.elapsed() > Duration::from_secs(30) => {
                    let _ = child.kill();
                    return Err("Finishing the video timed out.".into());
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => return Err(e.to_string()),
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if self.shared.encoder.lock().unwrap().is_some() {
            let _ = self.stop_recording();
        }
        for mut c in self.children.lock().unwrap().drain(..) {
            let _ = c.kill();
            let _ = c.wait();
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}
