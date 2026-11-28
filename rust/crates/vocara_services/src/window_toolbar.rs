//! The window's unified toolbar on macOS (`rust/DESIGN.md`, Toolbar).
//!
//! The app's window is frameless: winit hides its title, makes its title
//! bar transparent and lets the content view fill the whole window
//! (`fullSizeContentView`), and every page draws its own 52 px toolbar at
//! its top. Alone, that title bar is 28 px tall, so the traffic lights sat
//! high in the toolbar. [`install`] gives the window an empty `NSToolbar`
//! in the unified style (`NSWindowToolbarStyleUnified`): AppKit then makes
//! the title bar 52 px tall and centres the close, minimise and zoom
//! buttons in it, and the page's toolbar lines up with them. The toolbar
//! holds no items and draws nothing (the title bar is transparent, its
//! separator off): clicks on its area still reach the content view
//! (`titlebarAppearsTransparent`'s click-through), where the app's toolbar
//! takes them (its buttons, and its empty area's drag and double click).
//!
//! `setToolbarStyle:` and `setTitlebarSeparatorStyle:` are macOS 11; on
//! 10.15, the app's minimum, they are skipped (asked with
//! `respondsToSelector:`), and the empty toolbar with its baseline
//! separator off gives the older unified title and toolbar (38 px, the
//! traffic lights a little high in the 52 px toolbar). Everything else here
//! is in 10.15 (`NSToolbar`'s `initWithIdentifier:`, `-[NSWindow
//! setToolbar:]`).
//!
//! The app calls [`install`] once its window has opened, on the main
//! thread (where iced runs `update` on macOS), as it installs the menu bar
//! (`app_menu`). Off macOS, and off the main thread, it does nothing.
//! `vocara_app`'s `tests/macos_window_toolbar.rs` checks it on a Mac.

/// The toolbar's identifier (its autosave name; it saves nothing).
pub const TOOLBAR_IDENTIFIER: &str = "VocaraUnifiedToolbar";

/// The height the toolbar makes the title bar on macOS 11 and later, which
/// the pages' toolbars match.
pub const UNIFIED_HEIGHT: f64 = 52.0;

/// Gives every window of the app that is frameless as the app's is (titled,
/// a full-size content view, a transparent title bar) and has no toolbar
/// yet an empty unified toolbar. Idempotent; main thread only; nothing off
/// macOS.
pub fn install() {
    #[cfg(target_os = "macos")]
    macos::install();
}

#[cfg(target_os = "macos")]
pub use macos::install_on;

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::CStr;

    use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
    use objc2::{sel, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSApplication, NSTitlebarSeparatorStyle, NSToolbar, NSWindow, NSWindowStyleMask,
        NSWindowToolbarStyle,
    };
    use objc2_foundation::{NSObjectProtocol, NSString};

    use super::TOOLBAR_IDENTIFIER;

    pub fn install() {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        for window in app.windows().to_vec() {
            let mask = window.styleMask();
            let frameless = mask.contains(NSWindowStyleMask::Titled)
                && mask.contains(NSWindowStyleMask::FullSizeContentView)
                && window.titlebarAppearsTransparent();
            if frameless {
                install_on(&window);
            }
        }
    }

    /// `-(BOOL)mouseDownCanMoveWindow`.
    type CanMoveWindow = unsafe extern "C-unwind" fn(*mut AnyObject, Sel) -> Bool;

    /// The type encoding of [`CanMoveWindow`] (BOOL is `signed char` on
    /// x86_64, `bool` elsewhere).
    #[cfg(target_arch = "x86_64")]
    const CAN_MOVE_TYPES: &CStr = c"c@:";
    #[cfg(not(target_arch = "x86_64"))]
    const CAN_MOVE_TYPES: &CStr = c"B@:";

    /// The content view's answer: no mouse-down on it moves the window.
    unsafe extern "C-unwind" fn cannot_move_window(_view: *mut AnyObject, _cmd: Sel) -> Bool {
        Bool::NO
    }

    /// Makes a mouse-down on `window`'s content view never move the window
    /// by itself. In a transparent, full-size title bar AppKit moves the
    /// window from a mouse-down on any view that answers
    /// `mouseDownCanMoveWindow` YES, which a view that is not opaque does by
    /// default (winit's does): a drag on a control under the title bar (the
    /// ruler's playhead, the zoom slider) dragged the window along. The
    /// app's toolbar moves the window itself, from its empty area only
    /// (`toolbar::drag_region`, `-performWindowDragWithEvent:`).
    /// `class_addMethod` adds to the view's own class (winit's) and keeps a
    /// method the class defines itself: idempotent.
    fn keep_window_still(window: &NSWindow) {
        let Some(view) = window.contentView() else {
            return;
        };
        let class = std::ptr::from_ref::<AnyClass>(view.class()).cast_mut();
        // SAFETY: `class` is the content view's live, registered class (the
        // runtime takes its lock to add a method); the implementation has
        // the signature `CAN_MOVE_TYPES` encodes, the one AppKit sends
        // `mouseDownCanMoveWindow` with (receiver, selector; BOOL result);
        // `CAN_MOVE_TYPES` is a static C string.
        unsafe {
            objc2::ffi::class_addMethod(
                class,
                sel!(mouseDownCanMoveWindow),
                std::mem::transmute::<CanMoveWindow, Imp>(cannot_move_window),
                CAN_MOVE_TYPES.as_ptr(),
            );
        }
    }

    /// Gives `window` an empty unified toolbar, unless it has a toolbar
    /// already; returns whether it gave one. Either way, a mouse-down on its
    /// content no longer moves the window by itself ([`keep_window_still`]).
    pub fn install_on(window: &NSWindow) -> bool {
        keep_window_still(window);
        if window.toolbar().is_some() {
            return false;
        }
        let mtm = window.mtm();
        let toolbar =
            NSToolbar::initWithIdentifier(mtm.alloc(), &NSString::from_str(TOOLBAR_IDENTIFIER));
        // No line under it on 10.15 (the property does nothing from 11).
        #[allow(deprecated)]
        toolbar.setShowsBaselineSeparator(false);
        window.setToolbar(Some(&toolbar));
        // macOS 11+: the unified style (the 52 px title bar, the traffic
        // lights centred in it) and no separator under it.
        if window.respondsToSelector(sel!(setToolbarStyle:)) {
            window.setToolbarStyle(NSWindowToolbarStyle::Unified);
        }
        if window.respondsToSelector(sel!(setTitlebarSeparatorStyle:)) {
            window.setTitlebarSeparatorStyle(NSTitlebarSeparatorStyle::None);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_the_main_thread_or_off_macos_it_does_nothing() {
        // A test thread is not AppKit's main thread: nothing to install,
        // and nothing breaks.
        std::thread::spawn(install).join().expect("no panic");
        assert_eq!(UNIFIED_HEIGHT, 52.0);
        assert!(!TOOLBAR_IDENTIFIER.is_empty());
    }
}
