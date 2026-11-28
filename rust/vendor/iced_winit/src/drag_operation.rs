//! vocara: what a drag in from another app is told it may do where the
//! pointer is. Not part of upstream iced_winit (see `rust/vendor/README.md`).
//!
//! winit 0.30.13's window delegate answers AppKit's `draggingEntered:` with
//! "copy" and has no `draggingUpdated:`, so the copy badge (the green "+")
//! showed over the whole window, over Home, Settings and every part of a
//! page that takes no drop, and a drop there landed on nothing.
//!
//! The app's drop targets say, while a file hovers, whether a drop where
//! the cursor is now would be theirs ([`reached`]), during the update the
//! event loop brackets with [`open`] and [`close`]: the verdict, for that
//! cursor position. [`accept`] gives the window delegate's class, at
//! runtime (as `gesture` and `insert_text` do for the view), a
//! `draggingUpdated:` that answers "none" only where the verdict is fresh,
//! for the pointer where it is now, and no target took it; anywhere else
//! (the pointer moved since the app last looked, no verdict yet) it answers
//! "copy", as winit always did. So a drop is refused only where the app has
//! seen it would land on nothing, never on a target the pointer reached a
//! moment ago: that drop arrives, and the target takes it.
//!
//! A refused drop slides back to where it came from, as on any Mac, and
//! the drag's end reaches the app as the hover leaving.

use std::cell::RefCell;

use winit::dpi::PhysicalPosition;

/// What a drag may do, as `draggingUpdated:` answers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// The drop is taken here (AppKit's `NSDragOperationCopy`).
    Copy,
    /// Nothing here takes it (`NSDragOperationNone`).
    None,
}

/// How far, in physical pixels, the pointer may be from where the verdict
/// was made for it to still hold.
const FRESH_WITHIN: f64 = 2.0;

/// The app's verdict on a drop at `at` over the view `view`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Verdict {
    view: usize,
    at: PhysicalPosition<f64>,
    taken: bool,
}

#[derive(Debug)]
struct State {
    /// An update the event loop is running for a hover; `Some(reached)`.
    open: Option<bool>,
    verdict: Option<Verdict>,
}

thread_local! {
    /// The main thread's: the event loop updates the interface there, and
    /// AppKit calls `draggingUpdated:` there.
    static STATE: RefCell<State> = const {
        RefCell::new(State {
            open: None,
            verdict: None,
        })
    };
}

fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
    STATE.with(|state| f(&mut state.borrow_mut()))
}

/// The event loop is about to update a window a file hovers over, for the
/// cursor's move there (or the hover's start).
pub fn open() {
    with(|state| state.open = Some(false));
}

/// A drop target would take a drop where the cursor is, in the update
/// [`open`] began. Called by the app's targets; nothing outside one.
pub fn reached() {
    with(|state| {
        if let Some(reached) = state.open.as_mut() {
            *reached = true;
        }
    });
}

/// The update [`open`] began is done: its verdict holds for the cursor at
/// `at` over `view` (a window's view, as `gesture::view_of` names it).
/// Nothing is judged where the cursor is not known.
pub fn close(view: usize, at: Option<PhysicalPosition<f64>>) {
    with(|state| {
        let Some(taken) = state.open.take() else {
            return;
        };
        state.verdict = at.map(|at| Verdict { view, at, taken });
    });
}

/// The drag is over (dropped, or gone from the window): no verdict holds.
pub fn clear() {
    with(|state| {
        state.open = None;
        state.verdict = None;
    });
}

/// What the drag over `view` may do with the pointer at `now` (`None` when
/// it could not be read): "none" only where the app found nothing taking a
/// drop at that very spot.
pub fn operation(view: usize, now: Option<PhysicalPosition<f64>>) -> Operation {
    decide(with(|state| state.verdict), view, now)
}

fn decide(
    verdict: Option<Verdict>,
    view: usize,
    now: Option<PhysicalPosition<f64>>,
) -> Operation {
    match (verdict, now) {
        (Some(v), Some(now))
            if v.view == view
                && !v.taken
                && (v.at.x - now.x).abs() <= FRESH_WITHIN
                && (v.at.y - now.y).abs() <= FRESH_WITHIN =>
        {
            Operation::None
        }
        _ => Operation::Copy,
    }
}

/// Gives `window`'s delegate class `draggingUpdated:` (see the module
/// docs), once; nothing off macOS, or where the class has one of its own.
pub fn accept(window: &winit::window::Window) {
    #[cfg(target_os = "macos")]
    appkit::accept(window);
    #[cfg(not(target_os = "macos"))]
    let _ = window;
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod appkit {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::ptr;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Imp, Sel};
    use objc2::{MainThreadMarker, ffi, msg_send, sel};
    use objc2_app_kit::{NSView, NSWindow};

    use super::Operation;

    /// `draggingUpdated:` as the runtime calls it: an `NSDragOperation`.
    type DraggingUpdated =
        unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> usize;

    const COPY: usize = 1;
    const NONE: usize = 0;

    pub(super) fn accept(window: &winit::window::Window) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        let Some(view) = crate::gesture::view_of(window) else {
            return;
        };
        // SAFETY: winit's AppKit handle is its window's view, alive with the
        // window; this is the main thread.
        let view: &NSView = unsafe { &*(view as *const NSView) };
        let Some(ns_window) = view.window() else {
            return;
        };
        // SAFETY: an `NSWindow`'s delegate, read on the main thread.
        let delegate: Option<Retained<AnyObject>> =
            unsafe { msg_send![&*ns_window, delegate] };
        let Some(delegate) = delegate else {
            return;
        };
        let class = delegate.class();
        // SAFETY: it has the signature of `draggingUpdated:`.
        let ours = unsafe {
            std::mem::transmute::<DraggingUpdated, Imp>(dragging_updated)
        };
        // `- (NSDragOperation)draggingUpdated:(id<NSDraggingInfo>)sender`.
        // `class_addMethod` adds it only where the class has none of its
        // own, which is also what keeps a second window from adding it again.
        // SAFETY: a registered class, changed on the main thread.
        // NO (not added) is the class's own method staying: nothing to do.
        let _added = unsafe {
            ffi::class_addMethod(
                ptr::from_ref(class).cast_mut(),
                sel!(draggingUpdated:),
                ours,
                c"Q@:@".as_ptr(),
            )
        };
    }

    /// `draggingUpdated:` on the window delegate: what the app's verdict
    /// says for the pointer where it is now; "copy" whatever goes wrong.
    unsafe extern "C-unwind" fn dragging_updated(
        _this: *mut AnyObject,
        _cmd: Sel,
        sender: *mut AnyObject,
    ) -> usize {
        catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: AppKit sends a live `NSDraggingInfo`.
            let Some(sender) = (unsafe { sender.as_ref() }) else {
                return COPY;
            };
            // SAFETY: `NSDraggingInfo` answers `draggingDestinationWindow`.
            let window: Option<Retained<NSWindow>> =
                unsafe { msg_send![sender, draggingDestinationWindow] };
            let Some(window) = window else {
                return COPY;
            };
            let Some(view) = window.contentView() else {
                return COPY;
            };
            let now = crate::drop_pointer::position_in(&view);
            match super::operation(ptr::from_ref(&*view) as usize, now) {
                Operation::Copy => COPY,
                Operation::None => NONE,
            }
        }))
        .unwrap_or(COPY)
    }
}

#[cfg(test)]
mod tests {
    use super::{Operation, Verdict, decide};
    use winit::dpi::PhysicalPosition;

    fn at(x: f64, y: f64) -> PhysicalPosition<f64> {
        PhysicalPosition::new(x, y)
    }

    fn verdict(taken: bool) -> Option<Verdict> {
        Some(Verdict {
            view: 7,
            at: at(100.0, 50.0),
            taken,
        })
    }

    #[test]
    fn a_drop_is_refused_only_where_the_app_saw_nothing_take_it() {
        // Seen just there, nothing taking it.
        assert_eq!(decide(verdict(false), 7, Some(at(100.0, 50.0))), Operation::None);
        assert_eq!(decide(verdict(false), 7, Some(at(101.5, 48.5))), Operation::None);
        // A target takes it there.
        assert_eq!(decide(verdict(true), 7, Some(at(100.0, 50.0))), Operation::Copy);
    }

    #[test]
    fn where_the_app_has_not_looked_the_drop_still_goes_through() {
        // The pointer moved on since (onto a target, perhaps).
        assert_eq!(decide(verdict(false), 7, Some(at(140.0, 50.0))), Operation::Copy);
        // Another window's verdict, none yet, or a pointer not read.
        assert_eq!(decide(verdict(false), 8, Some(at(100.0, 50.0))), Operation::Copy);
        assert_eq!(decide(None, 7, Some(at(100.0, 50.0))), Operation::Copy);
        assert_eq!(decide(verdict(false), 7, None), Operation::Copy);
    }

    #[test]
    fn a_verdict_is_made_only_in_an_update_for_a_hover_and_cleared_with_the_drag() {
        // The state is the thread's: each test has its own.
        super::clear();
        super::reached(); // outside an update for a hover: nothing
        super::close(7, Some(at(100.0, 50.0)));
        assert_eq!(super::operation(7, Some(at(100.0, 50.0))), Operation::Copy);

        super::open();
        super::close(7, Some(at(100.0, 50.0)));
        assert_eq!(super::operation(7, Some(at(100.0, 50.0))), Operation::None);

        super::open();
        super::reached();
        super::close(7, Some(at(100.0, 50.0)));
        assert_eq!(super::operation(7, Some(at(100.0, 50.0))), Operation::Copy);

        super::open();
        super::close(7, Some(at(100.0, 50.0)));
        super::clear();
        assert_eq!(super::operation(7, Some(at(100.0, 50.0))), Operation::Copy);
    }
}
