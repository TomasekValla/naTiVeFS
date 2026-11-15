//! macOS: opening the app from the Dock, Launchpad, Spotlight or Finder while it already runs (often only in the menu
//! bar, after its window was closed) does not start a second copy: AppKit asks the running one to "reopen" through its
//! app delegate. winit has no hook for that, so the delegate method is added to winit's delegate class at runtime and
//! shows the main window. Without it the click did nothing visible.
//!
//! Written against winit 0.30.13, whose delegate does not implement this selector (`class_addMethod` never replaces
//! a method; if a winit update adds it, the hook is skipped with a message). Recheck after updating winit.

use std::ffi::CStr;
use std::sync::OnceLock;

use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{class, msg_send, sel};

static ON_REOPEN: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// `- (BOOL)applicationShouldHandleReopen:(NSApplication *)app hasVisibleWindows:(BOOL)flag`
/// A panic must never unwind into AppKit's (not unwind-safe) frames: it is caught here, and the plain `extern "C"`
/// ABI would abort instead of unwinding if one ever got through.
unsafe extern "C" fn should_handle_reopen(_this: *mut AnyObject, _cmd: Sel, _app: *mut AnyObject, _visible: Bool) -> Bool {
    if let Some(f) = ON_REOPEN.get() {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f())).is_err() {
            eprintln!("naTiVeFS: showing the window on reopen failed");
        }
    }
    // handled here: AppKit's own default would try to open an untitled document
    Bool::NO
}

/// Call once on the main thread after the event loop runs (winit sets its app delegate when the loop is created).
pub fn install(on_reopen: impl Fn() + Send + Sync + 'static) {
    if ON_REOPEN.set(Box::new(on_reopen)).is_err() {
        return;
    }
    // BOOL is a C bool on Apple Silicon ("B") and a signed char on Intel ("c")
    let types: &CStr = if cfg!(target_arch = "aarch64") { c"B@:@B" } else { c"c@:@c" };
    // SAFETY: plain Objective-C runtime calls on the main thread; the added method's signature matches `types` and the
    // selector, and the class (winit's delegate) does not implement the selector itself, so nothing is replaced
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if app.is_null() {
            return;
        }
        let delegate: *mut AnyObject = msg_send![app, delegate];
        if delegate.is_null() {
            return;
        }
        let cls = (*delegate).class() as *const AnyClass as *mut AnyClass;
        let imp: Imp = std::mem::transmute(
            should_handle_reopen as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject, Bool) -> Bool,
        );
        let added = objc2::ffi::class_addMethod(cls, sel!(applicationShouldHandleReopen:hasVisibleWindows:), imp, types.as_ptr());
        if !added.as_bool() {
            eprintln!("naTiVeFS: could not hook the Dock reopen event");
        }
    }
}
