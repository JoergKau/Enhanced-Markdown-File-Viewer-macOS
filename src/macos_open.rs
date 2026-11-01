//! macOS "Open With" / double-click integration.
//!
//! # Why this exists
//!
//! On macOS, files opened from Finder are **not** passed as command-line
//! arguments. LaunchServices sends an Apple Event (`kAEOpenDocuments`) to the
//! running application, and AppKit forwards it to the `NSApplicationDelegate`
//! via `application:openURLs:` (or the legacy `application:openFile:`).
//!
//! winit 0.30 installs its own delegate but only implements lifecycle methods
//! (`applicationDidFinishLaunching:`, `applicationWillTerminate:`). It neither
//! implements the open-document methods nor exposes an `Event::Opened`
//! variant (upstream issue rust-windowing/winit#1751). eframe therefore never
//! sees the event.
//!
//! # Approach
//!
//! 1. Before the event loop starts, we swizzle `NSApplication.setDelegate:`.
//! 2. As soon as winit installs its delegate, our replacement adds
//!    `application:openURLs:` and `application:openFile:` to the delegate's
//!    **class** (winit's lifecycle methods stay untouched).
//! 3. The methods convert incoming `NSURL`/`NSString` objects to [`PathBuf`]s
//!    and push them into a global queue.
//! 4. The UI drains the queue once per frame and opens the file.
//!
//! Swizzling `setDelegate:` (rather than polling or replacing the delegate) is
//! deterministic: the hook exists before AppKit can deliver the cold-start
//! Apple Event.
//!
//! The module is deliberately tiny, macOS-only and isolated: no other part of
//! the app depends on Objective-C.

use std::path::PathBuf;

/// Swizzles `NSApplication.setDelegate:` and returns `true` on success.
///
/// Must be called from the main thread before `eframe::run_native`. The
/// swizzle happens once; later calls return the first result.
pub fn install_early() -> bool {
    imp::install_early()
}

/// Fallback: adds the methods to the current delegate (idempotent).
pub fn install() -> bool {
    imp::install()
}

/// Stores the egui context so the delegate callback can wake the UI.
pub fn set_context(ctx: eframe::egui::Context) {
    imp::set_context(ctx)
}

/// Drains the queue of files opened from Finder.
pub fn take_pending() -> Vec<PathBuf> {
    imp::take_pending()
}

#[cfg(target_os = "macos")]
mod imp {
    use objc2::ffi::{
        class_addMethod, class_getInstanceMethod, method_getImplementation,
        method_setImplementation, object_getClass,
    };
    use objc2::runtime::{AnyClass, AnyObject, Imp, Method, Sel};
    use objc2::{class, msg_send};
    use std::ffi::CStr;
    use std::os::raw::c_char;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

    /// Files received from macOS, waiting to be opened by the UI thread.
    static PENDING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    /// egui context used to wake the UI after an Apple Event arrives.
    static REPAINT: Mutex<Option<eframe::egui::Context>> = Mutex::new(None);

    /// Original implementation of `NSApplication.setDelegate:`, stored before
    /// the replacement is installed. A `OnceLock` cannot be poisoned, so the
    /// replacement always reaches winit's implementation.
    static ORIGINAL_SET_DELEGATE: OnceLock<Imp> = OnceLock::new();

    pub fn set_context(ctx: eframe::egui::Context) {
        if let Ok(mut slot) = REPAINT.lock() {
            *slot = Some(ctx);
        }
    }

    /// Swizzles `NSApplication.setDelegate:` once.
    ///
    /// Runs before `eframe::run_native`, so winit's delegate is extended the
    /// moment it is installed — before AppKit delivers any open-document
    /// Apple Event (including cold-start launches).
    pub fn install_early() -> bool {
        static INSTALLED: OnceLock<bool> = OnceLock::new();
        *INSTALLED.get_or_init(swizzle_set_delegate)
    }

    fn swizzle_set_delegate() -> bool {
        // SAFETY: main thread; NSApplication is a singleton. We replace one
        // method implementation and remember the original to call through.
        unsafe {
            let _app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let cls = class!(NSApplication) as *const AnyClass as *mut AnyClass;
            let method: *const Method =
                class_getInstanceMethod(cls, Sel::register(c"setDelegate:"));
            if method.is_null() {
                log::warn!("NSApplication.setDelegate: not found; cannot install open hooks");
                return false;
            }
            let Some(original) = method_getImplementation(method) else {
                return false;
            };
            let _ = ORIGINAL_SET_DELEGATE.set(original);
            let imp: Imp = std::mem::transmute(set_delegate as *const ());
            method_setImplementation(method, imp);
            log::info!("installed macOS setDelegate swizzle");
            true
        }
    }

    /// Replacement for `NSApplication.setDelegate:`.
    unsafe extern "C-unwind" fn set_delegate(
        this: *mut AnyObject,
        cmd: Sel,
        delegate: *mut AnyObject,
    ) {
        // Call the original implementation first (winit stores its delegate).
        if let Some(original) = ORIGINAL_SET_DELEGATE.get() {
            type SetDelegateFn = unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject);
            let f: SetDelegateFn = unsafe { std::mem::transmute(*original) };
            // SAFETY: matches the Objective-C signature of `setDelegate:`.
            unsafe { f(this, cmd, delegate) };
        }
        if !delegate.is_null() {
            // SAFETY: the delegate is a valid Objective-C object.
            unsafe { install_on_class(object_getClass(delegate) as *mut AnyClass) };
        }
    }

    /// Adds the two open-document methods to a delegate class (idempotent).
    unsafe fn install_on_class(cls: *mut AnyClass) {
        // SAFETY: `cls` is a live Objective-C class; the IMPs match the
        // declared type encodings.
        unsafe {
            let urls_imp: Imp = std::mem::transmute(open_urls as *const ());
            let urls_added = class_addMethod(
                cls,
                Sel::register(c"application:openURLs:"),
                urls_imp,
                c"v@:@@".as_ptr(), // void (self, _cmd, NSApplication*, NSArray<NSURL*>*)
            )
            .as_bool();
            let file_imp: Imp = std::mem::transmute(open_file as *const ());
            let file_added = class_addMethod(
                cls,
                Sel::register(c"application:openFile:"),
                file_imp,
                c"B@:@".as_ptr(), // BOOL (self, _cmd, NSString*)
            )
            .as_bool();
            if urls_added || file_added {
                log::info!("installed macOS open-document delegate methods");
            }
        }
    }

    /// Fallback used by `App::new`: extend the delegate if it already exists.
    pub fn install() -> bool {
        // SAFETY: main thread; we only add methods to an existing class.
        unsafe {
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            if app.is_null() {
                return false;
            }
            let delegate: *mut AnyObject = msg_send![app, delegate];
            if delegate.is_null() {
                return false;
            }
            install_on_class(object_getClass(delegate) as *mut AnyClass);
            true
        }
    }

    /// `application:openURLs:` — receives an `NSArray<NSURL *>`.
    unsafe extern "C-unwind" fn open_urls(
        _this: *mut AnyObject,
        _cmd: Sel,
        _app: *mut AnyObject,
        urls: *mut AnyObject,
    ) {
        if urls.is_null() {
            return;
        }
        // SAFETY: AppKit passes an NSArray of NSURL objects for this selector.
        unsafe {
            let count: usize = msg_send![urls, count];
            let mut paths = Vec::with_capacity(count);
            for i in 0..count {
                let url: *mut AnyObject = msg_send![urls, objectAtIndex: i];
                if url.is_null() {
                    continue;
                }
                let path_obj: *mut AnyObject = msg_send![url, path];
                if path_obj.is_null() {
                    continue;
                }
                let utf8: *const c_char = msg_send![path_obj, UTF8String];
                if utf8.is_null() {
                    continue;
                }
                let s = CStr::from_ptr(utf8).to_string_lossy().into_owned();
                if !s.is_empty() {
                    paths.push(PathBuf::from(s));
                }
            }
            push(paths);
        }
    }

    /// Legacy `application:openFile:` — receives a single `NSString` path.
    unsafe extern "C-unwind" fn open_file(
        _this: *mut AnyObject,
        _cmd: Sel,
        _app: *mut AnyObject,
        filename: *mut AnyObject,
    ) -> bool {
        if filename.is_null() {
            return false;
        }
        // SAFETY: AppKit passes an NSString for this selector.
        unsafe {
            let utf8: *const c_char = msg_send![filename, UTF8String];
            if utf8.is_null() {
                return false;
            }
            let s = CStr::from_ptr(utf8).to_string_lossy().into_owned();
            if s.is_empty() {
                return false;
            }
            push(vec![PathBuf::from(s)]);
        }
        true
    }

    fn push(paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        log::info!("macOS open event: {paths:?}");
        if let Ok(mut queue) = PENDING.lock() {
            queue.extend(paths);
        }
        // The Apple Event arrives outside egui's event loop; wake the UI so
        // the queue is drained on the next frame.
        if let Ok(slot) = REPAINT.lock()
            && let Some(ctx) = slot.as_ref()
        {
            ctx.request_repaint();
        }
    }

    pub fn take_pending() -> Vec<PathBuf> {
        PENDING
            .lock()
            .map(|mut queue| std::mem::take(&mut *queue))
            .unwrap_or_default()
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::path::PathBuf;

    pub fn install_early() -> bool {
        false
    }

    pub fn install() -> bool {
        false
    }

    pub fn set_context(_ctx: eframe::egui::Context) {}

    pub fn take_pending() -> Vec<PathBuf> {
        Vec::new()
    }
}
