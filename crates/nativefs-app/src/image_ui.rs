//! Image viewer window: PNG, JPEG, WebP, GIF, BMP, ICO, AVIF; animated GIF / WebP play while the
//! window is visible (a single-shot timer per frame, nothing runs when hidden or for still images).

use std::cell::RefCell;
use std::io::Cursor;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use image::AnimationDecoder;
use slint::winit_030::WinitWindowAccessor;
use slint::{ComponentHandle, Rgba8Pixel, SharedPixelBuffer};
use tvfs_core::content::Content;
use tvfs_core::fmt::fmt_bytes;

use crate::{thumbs, windows, ImageWindow, Shared};

const MAX_IMAGE_BYTES: u64 = 200 * 1024 * 1024;
/// Animations whose decoded frames would need more memory than this show their first frame only.
const MAX_ANIMATION_BYTES: usize = 768 * 1024 * 1024;

struct Decoded {
    frames: Vec<(SharedPixelBuffer<Rgba8Pixel>, Duration)>,
    width: u32,
    height: u32,
    animated_but_too_big: bool,
}

fn decode(data: &[u8]) -> Option<Decoded> {
    let one = |img: image::RgbaImage| {
        let (w, h) = img.dimensions();
        Decoded { frames: vec![(SharedPixelBuffer::clone_from_slice(img.as_raw(), w, h), Duration::ZERO)], width: w, height: h, animated_but_too_big: false }
    };
    let frames = |frames: image::Frames| -> Option<Decoded> {
        let mut out = Vec::new();
        let mut bytes = 0usize;
        let (mut w, mut h) = (0, 0);
        for f in frames {
            let f = f.ok()?;
            let (num, den) = f.delay().numer_denom_ms();
            // browsers play 0 / 10 ms GIF delays at 100 ms
            let mut ms = num.checked_div(den).unwrap_or(100);
            if ms <= 10 {
                ms = 100;
            }
            let buf = f.into_buffer();
            (w, h) = buf.dimensions();
            bytes += buf.as_raw().len();
            if bytes > MAX_ANIMATION_BYTES {
                let first = out.into_iter().next()?;
                return Some(Decoded { frames: vec![first], width: w, height: h, animated_but_too_big: true });
            }
            out.push((SharedPixelBuffer::clone_from_slice(buf.as_raw(), w, h), Duration::from_millis(ms as u64)));
        }
        (!out.is_empty()).then_some(Decoded { frames: out, width: w, height: h, animated_but_too_big: false })
    };
    if thumbs::is_avif(data) {
        return thumbs::decode_avif(data).map(one);
    }
    match image::guess_format(data).ok()? {
        image::ImageFormat::Gif => {
            let dec = image::codecs::gif::GifDecoder::new(Cursor::new(data)).ok()?;
            frames(dec.into_frames())
        }
        image::ImageFormat::WebP => {
            let dec = image::codecs::webp::WebPDecoder::new(Cursor::new(data)).ok()?;
            if dec.has_animation() {
                frames(dec.into_frames())
            } else {
                image::load_from_memory(data).ok().map(|i| one(i.into_rgba8()))
            }
        }
        _ => image::load_from_memory(data).ok().map(|i| one(i.into_rgba8())),
    }
}

struct Viewer {
    window: ImageWindow,
    content: Content,
    bytes: Vec<u8>,
    frames: Vec<(slint::Image, Duration)>,
    frame: usize,
    visible: bool,
    timer: Option<slint::Timer>,
}

thread_local! {
    static VIEWERS: RefCell<Vec<Rc<RefCell<Viewer>>>> = const { RefCell::new(Vec::new()) };
}

/// Schedule the next animation frame (only while visible).
fn schedule(v: &Rc<RefCell<Viewer>>) {
    let mut vb = v.borrow_mut();
    if vb.frames.len() < 2 || !vb.visible {
        vb.timer = None;
        return;
    }
    let delay = vb.frames[vb.frame].1;
    let weak = Rc::downgrade(v);
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::SingleShot, delay, move || {
        let Some(v) = weak.upgrade() else { return };
        {
            let mut vb = v.borrow_mut();
            vb.frame = (vb.frame + 1) % vb.frames.len();
            let img = vb.frames[vb.frame].0.clone();
            vb.window.set_frame(img);
        }
        schedule(&v);
    });
    vb.timer = Some(timer);
}

pub fn open(shared: &Arc<Shared>, content: Content) {
    let existing = VIEWERS.with(|l| l.borrow().iter().find(|x| x.borrow().content.url == content.url).cloned());
    if let Some(v) = existing {
        windows::raise(v.borrow().window.window());
        return;
    }
    let Ok(w) = ImageWindow::new() else { return };
    w.set_file_name(content.name.clone().into());
    w.set_local(content.local_path().is_some());
    let v = Rc::new(RefCell::new(Viewer { window: w.clone_strong(), content: content.clone(), bytes: Vec::new(), frames: Vec::new(), frame: 0, visible: true, timer: None }));
    VIEWERS.with(|l| l.borrow_mut().push(v.clone()));

    let toast = {
        let weak = w.as_weak();
        move |text: &str| {
            let weak = weak.clone();
            windows::toast(move |t| { if let Some(w) = weak.upgrade() { w.set_toast(t.into()); } }, text);
        }
    };
    {
        let (shared, vw, toast) = (shared.clone(), Rc::downgrade(&v), toast.clone());
        w.on_download(move || {
            let Some(v) = vw.upgrade() else { return };
            let vb = v.borrow();
            if let Some(path) = vb.content.local_path() {
                crate::files_ui::show_in_folder(&path);
            } else if vb.bytes.is_empty() {
                crate::files_ui::save_as(&shared, &vb.content);
            } else {
                let toast = toast.clone();
                crate::files_ui::save_bytes(&vb.content.name, vb.bytes.clone(), move |m| toast(m));
            }
        });
    }
    {
        let (page, toast) = (content.page.clone(), toast.clone());
        w.on_copy_link(move || {
            crate::copy_to_clipboard(&page);
            toast("Link copied");
        });
    }
    {
        let (shared, page) = (shared.clone(), content.page.clone());
        w.on_open_browser(move || crate::open_external(&page, &shared.config.lock().unwrap()));
    }
    {
        let weak = w.as_weak();
        w.on_toggle_fullscreen(move || {
            if let Some(w) = weak.upgrade() {
                let on = !w.window().is_fullscreen();
                w.window().set_fullscreen(on);
                w.set_fullscreen_on(on);
            }
        });
    }
    let mut hooks = windows::Hooks::new();
    {
        let vw = Rc::downgrade(&v);
        hooks.visible = Box::new(move |on| {
            if let Some(v) = vw.upgrade() {
                v.borrow_mut().visible = on;
                schedule(&v);
            }
        });
    }
    {
        let weak = w.as_weak();
        hooks.paste_link = Some(crate::open::paste_hook(shared, move |m| {
            let weak = weak.clone();
            windows::toast(move |t| { if let Some(w) = weak.upgrade() { w.set_toast(t.into()); } }, m);
        }));
    }
    windows::attach(w.window(), hooks);
    {
        let vw = Rc::downgrade(&v);
        w.window().on_close_requested(move || {
            if let Some(v) = vw.upgrade() {
                v.borrow_mut().timer = None;
                VIEWERS.with(|l| l.borrow_mut().retain(|x| !Rc::ptr_eq(x, &v)));
            }
            crate::windows::closed();
            slint::CloseRequestResponse::HideWindow
        });
    }
    windows::place(w.window());
    let _ = w.show();
    let _ = w.window().with_winit_window(|_| ());

    let (s2, weak, url) = (shared.clone(), w.as_weak(), content.url.clone());
    shared.rt.spawn(async move {
        let fetched = s2.client.fetch_bytes(&url, MAX_IMAGE_BYTES).await;
        let result = match fetched {
            Ok(bytes) => {
                let b = bytes.to_vec();
                let decoded = tokio::task::spawn_blocking({
                    let b = b.clone();
                    move || decode(&b)
                })
                .await
                .ok()
                .flatten();
                Ok((b, decoded))
            }
            Err(e) => Err(e),
        };
        let _ = slint::invoke_from_event_loop(move || {
            let Some(w) = weak.upgrade() else { return };
            let Some(v) = VIEWERS.with(|l| l.borrow().iter().find(|x| std::ptr::eq(x.borrow().window.window(), w.window())).cloned()) else { return };
            match result {
                Ok((bytes, Some(d))) => {
                    let n = d.frames.len();
                    let mut info = format!("{} × {} · {}", d.width, d.height, fmt_bytes(bytes.len() as u64));
                    if n > 1 {
                        info.push_str(&format!(" · {n} frames"));
                    }
                    if d.animated_but_too_big {
                        info.push_str(" · animation too large, first frame only");
                    }
                    w.set_info(info.into());
                    w.set_img_w(d.width as f32);
                    w.set_img_h(d.height as f32);
                    let frames: Vec<(slint::Image, Duration)> = d.frames.into_iter().map(|(b, t)| (slint::Image::from_rgba8(b), t)).collect();
                    w.set_frame(frames[0].0.clone());
                    w.set_status("".into());
                    {
                        let mut vb = v.borrow_mut();
                        vb.bytes = bytes;
                        vb.frames = frames;
                    }
                    schedule(&v);
                }
                Ok((bytes, None)) => {
                    v.borrow_mut().bytes = bytes;
                    w.set_status("naTiVeFS cannot show this image format. Use Download to save it.".into());
                    w.set_status_error(true);
                }
                Err(e) => {
                    let msg = format!("Failed to load: {}", crate::open::describe(&e));
                    w.set_status(msg.into());
                    w.set_status_error(true);
                }
            }
        });
    });
}
