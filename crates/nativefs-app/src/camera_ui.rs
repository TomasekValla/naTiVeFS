//! The camera window of Create mode: "Take a photo" (JPEG) and "Record a video" (MP4). The result
//! lands in the upload queue of the main window.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slint::{ComponentHandle, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel, Weak};

use crate::capture::{self, Device, Event, Frame, Session};
use crate::{windows, CameraWindow, DeviceItem, MainWindow, Shared};

/// Preview width in pixels (the window shows it scaled; full frames are kept for the photo).
const PREVIEW_W: u32 = 960;

struct Cam {
    window: CameraWindow,
    main: Weak<MainWindow>,
    video: bool,
    session: Option<Session>,
    cameras: Vec<Device>,
    mics: Vec<Device>,
    camera: Option<Device>,
    mic: Option<Device>,
    /// The photo / video waiting for "Use" or "Retake".
    result: Option<PathBuf>,
    rec_started: Option<Instant>,
    rec_timer: slint::Timer,
    /// Bumped on every (re)start, so a slow start of an old session is dropped.
    generation: u64,
}

thread_local! {
    static CAMERA: RefCell<Option<Rc<RefCell<Cam>>>> = const { RefCell::new(None) };
}

fn stamp() -> String {
    chrono::Local::now().format("%Y-%m-%d-%H-%M-%S").to_string()
}

/// Open the camera window (`video` = Record a video). One at a time.
pub fn open(shared: &Arc<Shared>, main: &MainWindow, video: bool) {
    if let Some(c) = CAMERA.with(|c| c.borrow().clone()) {
        let same = c.borrow().video == video;
        if same {
            windows::raise(c.borrow().window.window());
            return;
        }
        // the other mode: close the open one first
        let w = c.borrow().window.clone_strong();
        close(&c);
        let _ = w.hide();
        windows::closed();
    }
    let Ok(window) = CameraWindow::new() else { return };
    window.set_light(shared.config.lock().unwrap().light_theme);
    window.set_video(video);
    window.set_state(0);
    let cam = Rc::new(RefCell::new(Cam {
        window: window.clone_strong(),
        main: main.as_weak(),
        video,
        session: None,
        cameras: Vec::new(),
        mics: Vec::new(),
        camera: None,
        mic: None,
        result: None,
        rec_started: None,
        rec_timer: slint::Timer::default(),
        generation: 0,
    }));
    CAMERA.with(|c| *c.borrow_mut() = Some(cam.clone()));
    wire(shared, &cam);
    windows::attach(window.window(), windows::Hooks::new());
    windows::place(window.window());
    let _ = window.show();

    // device lists take a moment (Media Foundation / ffmpeg): off the UI thread
    let (cfg_cam, cfg_mic) = {
        let cfg = shared.config.lock().unwrap();
        (cfg.camera_device.clone(), cfg.mic_device.clone())
    };
    let weak = window.as_weak();
    std::thread::spawn(move || {
        let cams = capture::cameras();
        let mics = if video { capture::microphones().unwrap_or_default() } else { Vec::new() };
        let _ = slint::invoke_from_event_loop(move || {
            let Some(cam) = current(&weak) else { return };
            match cams {
                Ok(cams) if !cams.is_empty() => {
                    {
                        let mut c = cam.borrow_mut();
                        c.camera = cams.iter().find(|d| d.id == cfg_cam).or(cams.first()).cloned();
                        c.mic = mics.iter().find(|d| d.id == cfg_mic).or(mics.first()).cloned();
                        c.cameras = cams;
                        c.mics = mics;
                        show_devices(&c);
                    }
                    start(&cam);
                }
                Ok(_) => fail(&cam, "No camera was found. Connect one (or choose a file instead)."),
                Err(e) => fail(&cam, &e),
            }
        });
    });
}

fn current(weak: &Weak<CameraWindow>) -> Option<Rc<RefCell<Cam>>> {
    let w = weak.upgrade()?;
    CAMERA.with(|c| c.borrow().clone()).filter(|c| std::ptr::eq(c.borrow().window.window(), w.window()))
}

fn show_devices(c: &Cam) {
    let item = |d: &Device| DeviceItem { id: d.id.as_str().into(), name: d.name.as_str().into() };
    c.window.set_cameras(ModelRc::new(VecModel::from(c.cameras.iter().map(item).collect::<Vec<_>>())));
    c.window.set_mics(ModelRc::new(VecModel::from(c.mics.iter().map(item).collect::<Vec<_>>())));
    c.window.set_camera_name(c.camera.as_ref().map(|d| d.name.as_str()).unwrap_or("").into());
    c.window.set_mic_name(c.mic.as_ref().map(|d| d.name.as_str()).unwrap_or("").into());
}

fn fail(cam: &Rc<RefCell<Cam>>, msg: &str) {
    let c = cam.borrow();
    c.window.set_error(msg.into());
    c.window.set_state(4);
    c.window.set_has_preview(false);
}

/// (Re)start the capture with the chosen devices.
fn start(cam: &Rc<RefCell<Cam>>) {
    let (camera, mic, gen, weak) = {
        let mut c = cam.borrow_mut();
        c.session = None;
        c.generation += 1;
        c.window.set_state(0);
        c.window.set_has_preview(false);
        c.window.set_mode_text("".into());
        let Some(camera) = c.camera.clone() else { return };
        (camera, if c.video { c.mic.clone() } else { None }, c.generation, c.window.as_weak())
    };
    // newest frame waiting for the UI (older ones are skipped, the event loop is never flooded)
    let pending: Arc<Mutex<Option<SharedPixelBuffer<Rgba8Pixel>>>> = Arc::default();
    let scheduled = Arc::new(AtomicBool::new(false));
    let sink: capture::Sink = {
        let weak = weak.clone();
        Arc::new(move |ev: Event| match ev {
            Event::Frame(f) => {
                let (rgba, w, h) = f.preview(PREVIEW_W, true);
                *pending.lock().unwrap() = Some(SharedPixelBuffer::clone_from_slice(&rgba, w, h));
                if !scheduled.swap(true, Ordering::AcqRel) {
                    let (weak, pending, scheduled) = (weak.clone(), pending.clone(), scheduled.clone());
                    let _ = slint::invoke_from_event_loop(move || {
                        scheduled.store(false, Ordering::Release);
                        let Some(buf) = pending.lock().unwrap().take() else { return };
                        let Some(w) = weak.upgrade() else { return };
                        // live view only (the review shows the result)
                        if w.get_state() == 1 || w.get_state() == 2 {
                            w.set_preview_aspect(buf.width() as f32 / buf.height().max(1) as f32);
                            w.set_preview(slint::Image::from_rgba8(buf));
                            w.set_has_preview(true);
                        }
                    });
                }
            }
            Event::Level(l) => {
                let weak = weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = weak.upgrade() {
                        w.set_level(l);
                    }
                });
            }
            Event::Mode(m) => {
                let weak = weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = weak.upgrade() {
                        w.set_mode_text(m.into());
                    }
                });
            }
            Event::Error(e) => {
                let weak = weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = weak.upgrade() {
                        w.set_status(e.into());
                    }
                });
            }
        })
    };
    std::thread::spawn(move || {
        let result = Session::start(&camera.id, mic.as_ref().map(|m| m.id.as_str()), sink);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(cam) = current(&weak) else { return };
            if cam.borrow().generation != gen {
                return;
            }
            match result {
                Ok(s) => {
                    let mut c = cam.borrow_mut();
                    c.session = Some(s);
                    c.window.set_state(1);
                    c.window.set_status("".into());
                    // debug builds: `NATIVEFS_CAMERA_AUTO=<s>` takes the photo / records <s> seconds by itself (tests)
                    #[cfg(debug_assertions)]
                    if let Some(secs) = std::env::var("NATIVEFS_CAMERA_AUTO").ok().and_then(|v| v.parse::<u64>().ok()) {
                        let w = c.window.as_weak();
                        let video = c.video;
                        slint::Timer::single_shot(Duration::from_secs(2), move || {
                            let Some(w) = w.upgrade() else { return };
                            if video {
                                w.invoke_record();
                                let w2 = w.as_weak();
                                slint::Timer::single_shot(Duration::from_secs(secs), move || {
                                    if let Some(w) = w2.upgrade() {
                                        w.invoke_stop();
                                    }
                                });
                            } else {
                                w.invoke_shutter();
                            }
                        });
                    }
                }
                Err(e) => fail(&cam, &e),
            }
        });
    });
}

fn close(cam: &Rc<RefCell<Cam>>) {
    let mut c = cam.borrow_mut();
    c.rec_timer.stop();
    // dropping the session releases the camera (a running recording is finished first)
    let session = c.session.take();
    drop(c);
    if let Some(s) = session {
        std::thread::spawn(move || drop(s));
    }
    CAMERA.with(|x| *x.borrow_mut() = None);
}

fn fmt_len(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

fn show_review(cam: &Rc<RefCell<Cam>>, frame: Option<Frame>, text: String) {
    let c = cam.borrow();
    if let Some(f) = frame {
        let (rgba, w, h) = f.preview(PREVIEW_W * 2, false);
        c.window.set_preview_aspect(w as f32 / h.max(1) as f32);
        c.window.set_preview(slint::Image::from_rgba8(SharedPixelBuffer::clone_from_slice(&rgba, w, h)));
        c.window.set_has_preview(true);
    }
    c.window.set_review_text(text.into());
    c.window.set_state(3);
}

fn wire(shared: &Arc<Shared>, cam: &Rc<RefCell<Cam>>) {
    let w = cam.borrow().window.clone_strong();
    let weak = Rc::downgrade(cam);
    let with = move |f: &dyn Fn(&Rc<RefCell<Cam>>)| {
        if let Some(c) = weak.upgrade() {
            f(&c);
        }
    };
    {
        let (with, shared) = (with.clone(), shared.clone());
        w.on_pick_camera(move |id| {
            with(&|cam| {
                {
                    let mut c = cam.borrow_mut();
                    c.camera = c.cameras.iter().find(|d| d.id == id.as_str()).cloned();
                    show_devices(&c);
                    let mut cfg = shared.config.lock().unwrap();
                    cfg.camera_device = id.to_string();
                    let _ = cfg.save();
                }
                start(cam);
            })
        });
    }
    {
        let (with, shared) = (with.clone(), shared.clone());
        w.on_pick_mic(move |id| {
            with(&|cam| {
                {
                    let mut c = cam.borrow_mut();
                    c.mic = c.mics.iter().find(|d| d.id == id.as_str()).cloned();
                    show_devices(&c);
                    let mut cfg = shared.config.lock().unwrap();
                    cfg.mic_device = id.to_string();
                    let _ = cfg.save();
                }
                start(cam);
            })
        });
    }
    {
        let with = with.clone();
        w.on_shutter(move || {
            with(&|cam| {
                let Some(frame) = cam.borrow().session.as_ref().and_then(|s| s.latest()) else { return };
                let path = capture::capture_dir().join(format!("photo-{}.jpg", stamp()));
                cam.borrow().window.set_status("Saving the photo…".into());
                let weak = cam.borrow().window.as_weak();
                let f2 = frame.clone();
                std::thread::spawn(move || {
                    let r = std::fs::File::create(&path).map_err(|e| e.to_string()).and_then(|file| {
                        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), 92);
                        enc.encode(&f2.to_rgb(), f2.width, f2.height, image::ExtendedColorType::Rgb8).map_err(|e| e.to_string())
                    });
                    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    let _ = slint::invoke_from_event_loop(move || {
                        let Some(cam) = current(&weak) else { return };
                        cam.borrow().window.set_status("".into());
                        match r {
                            Ok(()) => {
                                cam.borrow_mut().result = Some(path);
                                show_review(&cam, Some(frame.clone()), format!("{} × {} · {}", frame.width, frame.height, tvfs_core::fmt::fmt_bytes(size)));
                            }
                            Err(e) => cam.borrow().window.set_status(format!("Could not save the photo: {e}").into()),
                        }
                    });
                });
            })
        });
    }
    {
        let with = with.clone();
        w.on_record(move || {
            with(&|cam| {
                let path = capture::capture_dir().join(format!("video-{}.mp4", stamp()));
                let r = match cam.borrow().session.as_ref() {
                    Some(s) => s.start_recording(&path),
                    None => return,
                };
                match r {
                    Ok(()) => {
                        let mut c = cam.borrow_mut();
                        c.result = Some(path.clone());
                        c.rec_started = Some(Instant::now());
                        c.window.set_state(2);
                        c.window.set_rec_time("0:00".into());
                        c.window.set_rec_size("".into());
                        let weak = c.window.as_weak();
                        c.rec_timer.start(slint::TimerMode::Repeated, Duration::from_millis(500), move || {
                            let Some(cam) = current(&weak) else { return };
                            let (started, path) = {
                                let c = cam.borrow();
                                (c.rec_started, c.result.clone())
                            };
                            let (Some(started), Some(path)) = (started, path) else { return };
                            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                            let c = cam.borrow();
                            c.window.set_rec_time(fmt_len(started.elapsed()).into());
                            c.window.set_rec_size(tvfs_core::fmt::fmt_bytes(size).into());
                            if size >= capture::MAX_RECORDING_BYTES {
                                c.window.set_status("The video reached 5 GB and was stopped.".into());
                                drop(c);
                                cam.borrow().window.invoke_stop();
                            }
                        });
                    }
                    Err(e) => cam.borrow().window.set_status(e.into()),
                }
            })
        });
    }
    {
        let with = with.clone();
        w.on_stop(move || {
            with(&|cam| {
                let mut c = cam.borrow_mut();
                if c.window.get_state() != 2 {
                    return;
                }
                c.rec_timer.stop();
                let len = c.rec_started.take().map(|t| t.elapsed()).unwrap_or_default();
                c.window.set_status("Finishing the video…".into());
                let frame = c.session.as_ref().and_then(|s| s.latest());
                let r = c.session.as_ref().map(|s| s.stop_recording());
                drop(c);
                match r {
                    Some(Ok(path)) => {
                        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                        cam.borrow().window.set_status("".into());
                        show_review(cam, frame, format!("{} · {}", fmt_len(len), tvfs_core::fmt::fmt_bytes(size)));
                    }
                    Some(Err(e)) => {
                        let c = cam.borrow();
                        c.window.set_status(e.into());
                        c.window.set_state(1);
                    }
                    None => {}
                }
            })
        });
    }
    {
        let (with, shared) = (with.clone(), shared.clone());
        w.on_use_result(move || {
            with(&|cam| {
                let (path, main, window) = {
                    let c = cam.borrow();
                    (c.result.clone(), c.main.clone(), c.window.clone_strong())
                };
                let Some(path) = path else { return };
                if let Some(ui) = main.upgrade() {
                    crate::show_window(&ui, &shared);
                    if ui.get_mode() == 3 || ui.get_mode() == 2 {
                        ui.set_mode(0);
                    }
                    crate::upload_ui::add_paths(&ui, vec![path]);
                }
                close(cam);
                let _ = window.hide();
                windows::closed();
            })
        });
    }
    {
        let with = with.clone();
        w.on_retake(move || {
            with(&|cam| {
                let failed = cam.borrow().window.get_state() == 4;
                if let Some(p) = cam.borrow_mut().result.take() {
                    let _ = std::fs::remove_file(p);
                }
                if failed || cam.borrow().session.is_none() {
                    start(cam);
                } else {
                    let c = cam.borrow();
                    c.window.set_review_text("".into());
                    c.window.set_state(1);
                }
            })
        });
    }
    {
        let with = with.clone();
        w.on_pick_file(move || {
            with(&|cam| {
                let (main, video, window) = {
                    let c = cam.borrow();
                    (c.main.clone(), c.video, c.window.clone_strong())
                };
                if let Some(ui) = main.upgrade() {
                    if video {
                        crate::upload_ui::pick(&ui, false, Some(("Videos", &["mp4", "mov", "mkv", "webm", "avi", "m4v"])));
                    } else {
                        crate::upload_ui::pick(&ui, false, Some(("Images", &["jpg", "jpeg", "png", "gif", "webp", "heic", "avif", "bmp"])));
                    }
                }
                close(cam);
                let _ = window.hide();
                windows::closed();
            })
        });
    }
    {
        let weak = Rc::downgrade(cam);
        w.window().on_close_requested(move || {
            if let Some(cam) = weak.upgrade() {
                // a photo / video that was not used is thrown away
                // (taken first: the `if let` would hold the mutable borrow for its whole body)
                let unused = cam.borrow_mut().result.take();
                if let Some(p) = unused {
                    let recording = cam.borrow().window.get_state() == 2;
                    if !recording {
                        let _ = std::fs::remove_file(p);
                    }
                }
                close(&cam);
            }
            windows::closed();
            slint::CloseRequestResponse::HideWindow
        });
    }
}
