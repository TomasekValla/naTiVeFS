//! Keeps Slint alive while Windows resizes or moves a window.
//!
//! When the frame is dragged with the mouse, Windows runs its own message loop until the button is released. winit
//! still delivers the window's resize and paint events from it, but not the step in which Slint advances its
//! animations and timers and runs `changed` handlers (`update_timers_and_animations`, called from winit's
//! `new_events`). So during a drag the layout glide stood still and then jumped, timers (the moving background, the
//! end of the glide) stopped, and anything kept up to date by a `changed` handler kept its old value.
//!
//! While that loop runs, a Win32 timer (the system loop does dispatch timer messages) does that step and asks for a
//! frame. The window procedure is wrapped the classic way (`SetWindowLongPtrW`): the comctl32 subclass functions are
//! only exported by name from its version 6, which needs an application manifest. Other systems have no such loop:
//! no-op there.

#[cfg(windows)]
mod imp {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use slint::winit_030::WinitWindowAccessor;
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Gdi::{RedrawWindow, RDW_INTERNALPAINT};
    use windows::Win32::UI::WindowsAndMessaging::{
        CallWindowProcW, KillTimer, SetTimer, SetWindowLongPtrW, GWLP_WNDPROC, WM_ENTERSIZEMOVE, WM_EXITSIZEMOVE,
        WM_NCDESTROY, WM_TIMER, WNDPROC,
    };

    const TIMER_ID: usize = 0x6e66_6d6d;
    /// About one frame; Windows rounds timers up to its tick (usually 15.6 ms), resize steps add frames between.
    const TICK_MS: u32 = 8;

    thread_local! {
        /// The window procedure each hooked window had before (winit's), by window handle.
        static PREVIOUS: RefCell<HashMap<isize, isize>> = RefCell::new(HashMap::new());
    }

    fn tick(hwnd: HWND) {
        slint::platform::update_timers_and_animations();
        // SAFETY: `hwnd` is the window this message was sent to, alive while its procedure runs.
        unsafe {
            let _ = RedrawWindow(Some(hwnd), None, None, RDW_INTERNALPAINT);
        }
    }

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        let key = hwnd.0 as isize;
        let Some(previous) = PREVIOUS.with(|p| p.borrow().get(&key).copied()) else {
            return LRESULT(0);
        };
        match msg {
            WM_ENTERSIZEMOVE => {
                // SAFETY: a timer on our own window, killed again when the loop ends.
                unsafe {
                    SetTimer(Some(hwnd), TIMER_ID, TICK_MS, None);
                }
            }
            WM_TIMER if wp.0 == TIMER_ID => {
                tick(hwnd);
                return LRESULT(0);
            }
            WM_EXITSIZEMOVE => {
                // SAFETY: see above.
                unsafe {
                    let _ = KillTimer(Some(hwnd), TIMER_ID);
                }
            }
            WM_NCDESTROY => {
                PREVIOUS.with(|p| p.borrow_mut().remove(&key));
            }
            _ => {}
        }
        // SAFETY: `previous` is the procedure this window had before we wrapped it (a valid WNDPROC).
        unsafe {
            let previous: WNDPROC = std::mem::transmute::<isize, WNDPROC>(previous);
            CallWindowProcW(previous, hwnd, msg, wp, lp)
        }
    }

    /// Hook `window` once (call it again freely, e.g. on every resize: later calls do nothing). Needs the native
    /// window, so before it exists this does nothing and a later call does the work.
    pub fn attach(window: &slint::Window) {
        let Some(raw) = window
            .with_winit_window(|w| match w.window_handle().ok()?.as_raw() {
                RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
                _ => None,
            })
            .flatten()
        else {
            return;
        };
        if PREVIOUS.with(|p| p.borrow().contains_key(&raw)) {
            return;
        }
        let hwnd = HWND(raw as *mut _);
        // SAFETY: our own window, on the thread that owns it. Swapping the procedure sends no message, so the old one
        // is stored before any message can reach the new one, and every message is passed on to it.
        unsafe {
            let wrapped = proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;
            let previous = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, wrapped as usize as isize);
            if previous != 0 {
                PREVIOUS.with(|p| p.borrow_mut().insert(raw, previous));
            }
        }
    }
}

#[cfg(windows)]
pub use imp::attach;

#[cfg(not(windows))]
pub fn attach(_window: &slint::Window) {}
