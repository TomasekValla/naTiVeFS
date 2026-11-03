//! Shared plumbing of the extra windows (reader, viewers, player, ...): the same raw-input
//! handling as the main window (smooth wheel, middle-button autoscroll, Ctrl + wheel zoom),
//! visibility tracking, and where new windows open.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::winit_030::{winit, EventResult, WinitWindowAccessor};

use crate::{next_zoom, WHEEL_STEP, ZOOM_PIXELS_PER_STEP};

/// Receives the pointer position in physical px (`None` = it left the window).
pub type CursorHook = Box<dyn Fn(Option<(f32, f32)>)>;

/// What a window lets the shared input code do with it.
pub struct Hooks {
    /// Store the wheel step before Slint delivers the event (`wheel-dy` / `wheel-smooth`).
    pub wheel: Box<dyn Fn(f32, bool)>,
    /// Scroll by this many logical px right now (middle-button autoscroll).
    pub autoscroll_by: Box<dyn Fn(f32)>,
    /// Whether there is anything to scroll (autoscroll only starts then).
    pub can_scroll: Box<dyn Fn() -> bool>,
    /// Ctrl + wheel: +1 / -1 zoom step (`None` = the window does not zoom).
    pub zoom: Option<Box<dyn Fn(i32)>>,
    /// The system scale changed (other monitor / display settings): put the zoom on top again.
    pub rezoom: Option<Box<dyn Fn()>>,
    /// The window became visible (true) or hidden / minimized (false).
    pub visible: Box<dyn Fn(bool)>,
    /// Files dropped on the window (`None` = ignore drops).
    pub dropped: Option<Box<dyn Fn(std::path::PathBuf)>>,
    /// Ctrl+Alt+V: open the TVFS link on the clipboard.
    pub paste_link: Option<Box<dyn Fn()>>,
    /// The mouse moved over the window (the player shows its controls).
    pub moved: Option<Box<dyn Fn()>>,
    /// Pointer position in physical px, `None` when it left the window (the audio visualizer).
    pub cursor: Option<CursorHook>,
    /// The window got a new size in physical px. While the frame is dragged Windows runs a modal loop in which no
    /// timer fires, so animations draw from here to stay alive.
    pub resized: Option<Box<dyn Fn(u32, u32)>>,
}

impl Hooks {
    pub fn new() -> Self {
        Self {
            wheel: Box::new(|_, _| {}),
            autoscroll_by: Box::new(|_| {}),
            can_scroll: Box::new(|| false),
            zoom: None,
            rezoom: None,
            visible: Box::new(|_| {}),
            dropped: None,
            paste_link: None,
            moved: None,
            cursor: None,
            resized: None,
        }
    }
}

#[derive(Default)]
struct Auto {
    active: bool,
    cursor: (f32, f32),
    anchor: (f32, f32),
    started: Option<Instant>,
    moved_far: bool,
    last_tick: Option<Instant>,
    timer: Option<slint::Timer>,
}

const DEAD_ZONE: f32 = 10.0;

fn speed(dy: f32) -> f32 {
    let d = dy.abs() - DEAD_ZONE;
    if d <= 0.0 {
        return 0.0;
    }
    dy.signum() * (6.0 * d + 0.035 * d * d)
}

/// Install the raw input handler on `window`.
pub fn attach(window: &slint::Window, hooks: Hooks) {
    let hooks = Rc::new(hooks);
    let auto = Rc::new(RefCell::new(Auto::default()));
    let zoom_key = Rc::new(Cell::new((false, 0.0f32)));
    let visible = Rc::new(Cell::new(true));
    let ctrl_alt = Rc::new(Cell::new(false));
    window.on_winit_window_event(move |w, ev| {
        use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent as E};
        let scale = w.scale_factor();
        let active = auto.borrow().active;
        let stop = |a: &mut Auto| {
            a.active = false;
            a.timer = None;
        };
        match ev {
            E::CursorMoved { position, .. } => {
                let mut a = auto.borrow_mut();
                a.cursor = (position.x as f32 / scale, position.y as f32 / scale);
                if a.active && (a.cursor.1 - a.anchor.1).abs() > DEAD_ZONE * 1.5 {
                    a.moved_far = true;
                }
                drop(a);
                if let Some(m) = &hooks.moved {
                    m();
                }
                if let Some(c) = &hooks.cursor {
                    c(Some((position.x as f32, position.y as f32)));
                }
                EventResult::Propagate
            }
            E::CursorLeft { .. } => {
                if let Some(c) = &hooks.cursor {
                    c(None);
                }
                EventResult::Propagate
            }
            E::ModifiersChanged(m) => {
                let held = if cfg!(target_os = "macos") { m.state().super_key() } else { m.state().control_key() };
                zoom_key.set((held, 0.0));
                ctrl_alt.set(m.state().control_key() && m.state().alt_key());
                EventResult::Propagate
            }
            E::KeyboardInput { event, .. } if !active && is_paste_link(event, ctrl_alt.get()) && hooks.paste_link.is_some() => {
                if let Some(p) = &hooks.paste_link {
                    p();
                }
                EventResult::PreventDefault
            }
            E::ScaleFactorChanged { .. } if hooks.rezoom.is_some() => {
                // Slint applies the new system scale first, the zoom goes on top after it
                let hooks = hooks.clone();
                slint::Timer::single_shot(Duration::ZERO, move || {
                    if let Some(r) = &hooks.rezoom {
                        r();
                    }
                });
                EventResult::Propagate
            }
            E::MouseWheel { delta, .. } if zoom_key.get().0 && hooks.zoom.is_some() => {
                let step = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y.signum() as i32,
                    MouseScrollDelta::PixelDelta(p) => {
                        let (held, acc) = zoom_key.get();
                        let acc = acc + p.y as f32;
                        let s = (acc / ZOOM_PIXELS_PER_STEP).trunc();
                        zoom_key.set((held, acc - s * ZOOM_PIXELS_PER_STEP));
                        s as i32
                    }
                };
                if step != 0 {
                    if let Some(z) = &hooks.zoom {
                        z(step.signum());
                    }
                }
                EventResult::PreventDefault
            }
            E::MouseWheel { delta, .. } => {
                if active {
                    stop(&mut auto.borrow_mut());
                }
                let (dy, smooth) = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (-y * WHEEL_STEP, true),
                    MouseScrollDelta::PixelDelta(p) => (-(p.y as f32) / scale, false),
                };
                (hooks.wheel)(dy, smooth);
                EventResult::Propagate
            }
            E::MouseInput { state, button, .. } => {
                if active {
                    let mut a = auto.borrow_mut();
                    match (state, button) {
                        (ElementState::Released, MouseButton::Middle) => {
                            if a.moved_far || a.started.is_some_and(|t| t.elapsed() > Duration::from_millis(400)) {
                                stop(&mut a);
                            }
                        }
                        (ElementState::Pressed, _) => stop(&mut a),
                        _ => {}
                    }
                    return EventResult::PreventDefault;
                }
                if *button == MouseButton::Middle && *state == ElementState::Pressed && (hooks.can_scroll)() {
                    let hooks2 = hooks.clone();
                    let auto2 = auto.clone();
                    let mut a = auto.borrow_mut();
                    a.active = true;
                    a.anchor = a.cursor;
                    a.started = Some(Instant::now());
                    a.moved_far = false;
                    a.last_tick = Some(Instant::now());
                    let timer = slint::Timer::default();
                    timer.start(slint::TimerMode::Repeated, Duration::from_millis(8), move || {
                        let step = {
                            let mut a = auto2.borrow_mut();
                            let now = Instant::now();
                            let dt = a.last_tick.map_or(0.0, |t| now.duration_since(t).as_secs_f32()).min(0.05);
                            a.last_tick = Some(now);
                            speed(a.cursor.1 - a.anchor.1) * dt
                        };
                        if step != 0.0 {
                            (hooks2.autoscroll_by)(step);
                        }
                    });
                    a.timer = Some(timer);
                    return EventResult::PreventDefault;
                }
                EventResult::Propagate
            }
            E::KeyboardInput { event, .. } if active => {
                if event.state == ElementState::Pressed {
                    stop(&mut auto.borrow_mut());
                }
                EventResult::PreventDefault
            }
            E::Focused(false) if active => {
                stop(&mut auto.borrow_mut());
                EventResult::Propagate
            }
            E::Occluded(hidden) => {
                if visible.replace(!hidden) == *hidden {
                    (hooks.visible)(!hidden);
                }
                EventResult::Propagate
            }
            E::Resized(size) => {
                let now = size.width > 0 && size.height > 0;
                if visible.replace(now) != now {
                    (hooks.visible)(now);
                }
                if let (true, Some(r)) = (now, &hooks.resized) {
                    r(size.width, size.height);
                }
                EventResult::Propagate
            }
            E::DroppedFile(path) => {
                if let Some(d) = &hooks.dropped {
                    d(path.clone());
                }
                EventResult::Propagate
            }
            _ => EventResult::Propagate,
        }
    });
}

/// Ctrl+Alt+V by the physical key (on many layouts Ctrl+Alt is AltGr, which types another character).
pub fn is_paste_link(event: &winit::event::KeyEvent, ctrl_alt: bool) -> bool {
    use winit::keyboard::{KeyCode, PhysicalKey};
    ctrl_alt && event.state == winit::event::ElementState::Pressed && !event.repeat && event.physical_key == PhysicalKey::Code(KeyCode::KeyV)
}

thread_local! {
    /// Extra windows that are open (the main window is not counted).
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static ON_LAST_CLOSED: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
}

/// Called when the last extra window closes (the app quits then if the main window is not shown).
pub fn on_last_closed(f: impl Fn() + 'static) {
    ON_LAST_CLOSED.with(|c| *c.borrow_mut() = Some(Box::new(f)));
}

pub fn live() -> usize {
    LIVE.with(Cell::get)
}

/// Every extra window calls this when it is closed.
pub fn closed() {
    let n = LIVE.with(|l| {
        let n = l.get().saturating_sub(1);
        l.set(n);
        n
    });
    if n == 0 {
        ON_LAST_CLOSED.with(|c| {
            if let Some(f) = c.borrow().as_ref() {
                f();
            }
        });
    }
}

/// Zoom a window like a browser: its scale factor becomes system scale x `zoom`.
pub fn apply_zoom(window: &slint::Window, zoom: f32) {
    let Some(system) = window.with_winit_window(|w| w.scale_factor() as f32) else { return };
    let sf = system * zoom;
    if (window.scale_factor() - sf).abs() > 1e-4 {
        let physical = window.size();
        window.dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: sf });
        if physical.width > 0 && physical.height > 0 {
            window.dispatch_event(slint::platform::WindowEvent::Resized { size: physical.to_logical(sf) });
        }
    }
    window.request_redraw();
}

/// The next zoom level after a Ctrl + wheel / plus / minus step (0 = back to 100 %).
pub fn zoom_after(cur: f32, dir: i32) -> f32 {
    next_zoom(cur, dir)
}

/// The config field that remembers a window type's zoom.
pub type ZoomField = fn(&mut tvfs_core::config::Config) -> &mut f32;

/// Browser-like zoom for a viewer window, remembered in `field`: Ctrl + wheel and a new system scale are handled here,
/// Ctrl + plus / minus / 0 by the window's key handler (`zoom_step`). The saved zoom is applied once the window is shown.
pub fn zoomable<T: slint::ComponentHandle + 'static>(hooks: &mut Hooks, w: &T, shared: &std::sync::Arc<crate::Shared>, field: ZoomField) {
    let (weak, sh) = (w.as_weak(), shared.clone());
    hooks.zoom = Some(Box::new(move |dir| {
        if let Some(w) = weak.upgrade() {
            zoom_step(w.window(), &sh, field, dir);
        }
    }));
    let reapply = {
        let (weak, sh) = (w.as_weak(), shared.clone());
        move || {
            if let Some(w) = weak.upgrade() {
                let zoom = *field(&mut sh.config.lock().unwrap());
                apply_zoom(w.window(), zoom.clamp(crate::ZOOM_LEVELS[0], crate::ZOOM_LEVELS[crate::ZOOM_LEVELS.len() - 1]));
            }
        }
    };
    let first = reapply.clone();
    let alive = w.as_weak();
    ZOOMABLE.with(|z| {
        let mut z = z.borrow_mut();
        z.retain(|(alive, _)| alive());
        z.push((Box::new(move || alive.upgrade().is_some()), Box::new(reapply.clone())));
    });
    hooks.rezoom = Some(Box::new(reapply));
    // the window is shown by the caller right after this
    slint::Timer::single_shot(Duration::ZERO, first);
}

/// +1 / -1 = next / previous zoom level, 0 = back to 100 %; saved in the config.
pub fn zoom_step(window: &slint::Window, shared: &crate::Shared, field: ZoomField, dir: i32) {
    let zoom = {
        let mut cfg = shared.config.lock().unwrap();
        let f = field(&mut cfg);
        *f = next_zoom(*f, dir);
        let zoom = *f;
        let _ = cfg.save();
        zoom
    };
    apply_zoom(window, zoom);
    crate::refresh_viewer_zooms(shared);
}

thread_local! {
    /// Open zoomable windows: (still open?, re-apply the saved zoom), for a reset from the settings page.
    #[allow(clippy::type_complexity)]
    static ZOOMABLE: RefCell<Vec<(Box<dyn Fn() -> bool>, Box<dyn Fn()>)>> = const { RefCell::new(Vec::new()) };
}

/// Settings page "Reset": back to 100 % in the config and in every open window of that kind.
pub fn reset_zoom(shared: &crate::Shared, field: ZoomField) {
    {
        let mut cfg = shared.config.lock().unwrap();
        *field(&mut cfg) = 1.0;
        let _ = cfg.save();
    }
    // the closures re-read the config, so windows of the other kinds keep their zoom
    ZOOMABLE.with(|z| {
        for (alive, reapply) in z.borrow().iter() {
            if alive() {
                reapply();
            }
        }
    });
}

thread_local! {
    /// How many extra windows were opened (each new one is offset a little, like a cascade).
    static OPENED: Cell<i32> = const { Cell::new(0) };
}

/// Place a new window: debug builds honour `NATIVEFS_VIEWER_POS=x,y` (tests on the second monitor),
/// otherwise the system picks; successive windows cascade by 28 px.
pub fn place(window: &slint::Window) {
    LIVE.with(|l| l.set(l.get() + 1));
    let n = OPENED.with(|o| {
        let n = o.get();
        o.set(n + 1);
        n
    });
    #[cfg(debug_assertions)]
    if let Some((x, y)) = std::env::var("NATIVEFS_VIEWER_POS").ok().and_then(|v| {
        let (x, y) = v.split_once(',')?;
        Some((x.trim().parse::<i32>().ok()?, y.trim().parse::<i32>().ok()?))
    }) {
        let off = (n % 6) * 28;
        window.set_position(slint::PhysicalPosition::new(x + off, y + off));
    }
    let _ = (n, window);
}

/// Bring a window to the front (when the same file is opened again).
pub fn raise(window: &slint::Window) {
    window.set_minimized(false);
    window.with_winit_window(|w| w.focus_window());
}

/// A short message at the bottom of a window that disappears by itself.
pub fn toast(set: impl Fn(&str) + 'static, text: &str) {
    set(text);
    thread_local! {
        static SEQ: Cell<u64> = const { Cell::new(0) };
    }
    let id = SEQ.with(|s| {
        s.set(s.get() + 1);
        s.get()
    });
    slint::Timer::single_shot(Duration::from_millis(1500), move || {
        if SEQ.with(|s| s.get()) == id {
            set("");
        }
    });
}
