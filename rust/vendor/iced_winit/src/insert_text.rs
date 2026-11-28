//! vocara: text macOS inserts with no key press and no composition, which
//! winit drops. Not part of upstream iced_winit (see `rust/vendor/README.md`).
//!
//! winit 0.30.13's view (`WinitView`, `platform_impl/macos/view.rs`) commits
//! the text of an `insertText:replacementRange:` only while it has marked
//! text, an input method's composition (CJK through Pinyin, a dead key).
//! Typed text reaches the app another way: `keyDown:` asks the input
//! context (which calls `insertText:` with no marked text) and then sends
//! the key press as `KeyboardInput`, its text included. So text inserted
//! outside a key press with nothing marked — Edit ▸ Emoji & Symbols (the
//! Character Viewer), dictation — reached nothing, and no field showed it.
//!
//! [`accept`] gives each window's view class, at runtime (once per class, as
//! `gesture`'s touches do), a `keyDown:` that notes a key press is being
//! handled while it calls through to the class's own, and an
//! `insertText:replacementRange:` whose first step is the class's own:
//! winit handles every insertion exactly as before, a composition's commit
//! included. Only then is the text winit dropped recorded ([`dropped`]
//! decides), and the run loop takes the records before each event it
//! handles ([`take`]) and hands each to its window's interface as an input
//! method commit, the event winit sends for a composition's text. Off macOS
//! nothing is recorded.

/// The text of an insertion winit's view has just handled, when winit
/// dropped it and nothing else carries it: `None` while a key press is
/// handled (`keyDown:` sends the text as the key press's), while there is
/// marked text (winit committed the text: it keeps the marked text until the
/// next key press, so it is there after the call), for a replacement
/// (macOS's press-and-hold accents replace the letter typed, which winit
/// does not do), and for a control character (which winit drops on
/// purpose).
pub fn dropped(
    text: &str,
    in_key_down: bool,
    has_marked_text: bool,
    replaces: bool,
) -> Option<&str> {
    let control = text.chars().next().is_none_or(char::is_control);
    (!in_key_down && !has_marked_text && !replaces && !control).then_some(text)
}

/// Text winit's view dropped, as [`dropped`] found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Insertion {
    /// The view it went to, by address: a window's, as `gesture::view_of`
    /// gives it.
    pub view: usize,
    /// What was inserted.
    pub text: String,
}

/// The insertions winit dropped since the last call, in order, on this
/// thread (the main one).
pub fn take() -> Vec<Insertion> {
    #[cfg(target_os = "macos")]
    {
        appkit::take()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

/// Gives `window`'s view class the two methods (see the module docs), once
/// per class; nothing off macOS. The run loop calls it on each window it
/// makes.
pub fn accept(window: &winit::window::Window) {
    #[cfg(target_os = "macos")]
    appkit::accept(window);
    #[cfg(not(target_os = "macos"))]
    let _ = window;
}

/// The AppKit side: the methods added to the view's class, and what they
/// record.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod appkit {
    use super::Insertion;

    use std::cell::{Cell, RefCell};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::ptr;

    use objc2::MainThreadMarker;
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::{ffi, msg_send, sel};
    use objc2_app_kit::NSView;
    use objc2_foundation::{NSAttributedString, NSNotFound, NSRange, NSString};

    /// `keyDown:`'s implementation, as the runtime calls it.
    type KeyDown =
        unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject);

    /// `insertText:replacementRange:`'s.
    type InsertText = unsafe extern "C-unwind" fn(
        *mut AnyObject,
        Sel,
        *mut AnyObject,
        NSRange,
    );

    thread_local! {
        /// How many `keyDown:`s are being handled now.
        static IN_KEY_DOWN: Cell<usize> = const { Cell::new(0) };
        /// The insertions recorded since they were last taken.
        static DROPPED: RefCell<Vec<Insertion>> =
            const { RefCell::new(Vec::new()) };
        /// Each class given one of the methods, the method's selector, and
        /// the implementation the class had, which it calls through to.
        static INSTALLED: RefCell<Vec<(&'static AnyClass, Sel, Option<Imp>)>> =
            const { RefCell::new(Vec::new()) };
    }

    pub(super) fn take() -> Vec<Insertion> {
        DROPPED.with(|dropped| std::mem::take(&mut *dropped.borrow_mut()))
    }

    pub(super) fn accept(window: &winit::window::Window) {
        let Some(view) = crate::gesture::view_of(window) else {
            return;
        };
        if MainThreadMarker::new().is_none() {
            return;
        }
        // SAFETY: winit's AppKit handle is its window's view, which lives
        // as long as the window; this is the main thread.
        let view: &NSView = unsafe { &*(view as *const NSView) };
        let class = view.class();
        // SAFETY: each has the signature of the method it stands for.
        let (key_down, insert_text) = unsafe {
            (
                std::mem::transmute::<KeyDown, Imp>(key_down),
                std::mem::transmute::<InsertText, Imp>(insert_text),
            )
        };
        install(class, sel!(keyDown:), key_down);
        install(class, sel!(insertText:replacementRange:), insert_text);
    }

    /// Gives `class` `ours` for `selector`, unless it has it already (its
    /// own or inherited: calling through to itself, it would never end), or
    /// has no such method at all (nothing to call through to).
    fn install(class: &'static AnyClass, selector: Sel, ours: Imp) {
        let Some(method) = class.instance_method(selector) else {
            return;
        };
        let current = method.implementation();
        if ptr::fn_addr_eq(current, ours) {
            return;
        }
        let target = ptr::from_ref(class).cast_mut();
        // SAFETY: a registered class, changed on the main thread, given the
        // types of the method it has (or inherits) for the selector.
        let previous = unsafe {
            let types = ffi::method_getTypeEncoding(ptr::from_ref(method));
            if ffi::class_addMethod(target, selector, ours, types).as_bool() {
                // The one it inherited.
                Some(current)
            } else {
                // Its own: replaced, and called through.
                ffi::class_replaceMethod(target, selector, ours, types)
            }
        };
        INSTALLED.with(|installed| {
            installed.borrow_mut().push((class, selector, previous));
        });
    }

    /// What `selector` had before on `class`, or the nearest class above it
    /// that was given it.
    fn previous(mut class: &'static AnyClass, selector: Sel) -> Option<Imp> {
        loop {
            let found = INSTALLED.with(|installed| {
                installed
                    .borrow()
                    .iter()
                    .find(|(c, s, _)| ptr::eq(*c, class) && *s == selector)
                    .map(|(_, _, previous)| *previous)
            });
            if let Some(previous) = found {
                return previous;
            }
            class = class.superclass()?;
        }
    }

    /// The class of the object `this`.
    fn class_of(this: *mut AnyObject) -> Option<&'static AnyClass> {
        // SAFETY: the runtime sends the method to a live object.
        unsafe { this.as_ref() }.map(AnyObject::class)
    }

    /// Counts a key press out however its handling ends.
    struct KeyPressed;

    impl KeyPressed {
        fn start() -> Self {
            IN_KEY_DOWN.with(|depth| depth.set(depth.get() + 1));
            KeyPressed
        }
    }

    impl Drop for KeyPressed {
        fn drop(&mut self) {
            IN_KEY_DOWN.with(|depth| depth.set(depth.get().saturating_sub(1)));
        }
    }

    /// `keyDown:` on a view of a class given it: the class's own, noted as
    /// running.
    unsafe extern "C-unwind" fn key_down(
        this: *mut AnyObject,
        cmd: Sel,
        event: *mut AnyObject,
    ) {
        let Some(previous) = class_of(this).and_then(|c| previous(c, cmd))
        else {
            return;
        };
        let _pressed = KeyPressed::start();
        // SAFETY: the implementation the class had for this selector,
        // called as the runtime would have.
        unsafe {
            std::mem::transmute::<Imp, KeyDown>(previous)(this, cmd, event);
        }
    }

    /// `insertText:replacementRange:` on a view of a class given it: the
    /// class's own first, then what it dropped recorded.
    unsafe extern "C-unwind" fn insert_text(
        this: *mut AnyObject,
        cmd: Sel,
        string: *mut AnyObject,
        replacement: NSRange,
    ) {
        let Some(previous) = class_of(this).and_then(|c| previous(c, cmd))
        else {
            return;
        };
        // SAFETY: as in `key_down`.
        unsafe {
            std::mem::transmute::<Imp, InsertText>(previous)(
                this,
                cmd,
                string,
                replacement,
            );
        }
        // Nothing of the recording may unwind into AppKit.
        let _ = catch_unwind(AssertUnwindSafe(|| {
            record(this, string, replacement);
        }));
    }

    /// Records the text of `string` (an `NSString` or an
    /// `NSAttributedString`, as `NSTextInputClient` is sent) when winit's
    /// view, `this`, dropped it.
    fn record(
        this: *mut AnyObject,
        string: *mut AnyObject,
        replacement: NSRange,
    ) {
        // SAFETY: the runtime sends the method to a live object, with a
        // live argument (or nil).
        let (Some(view), Some(string)) =
            (unsafe { this.as_ref() }, unsafe { string.as_ref() })
        else {
            return;
        };
        let text = if let Some(attributed) =
            string.downcast_ref::<NSAttributedString>()
        {
            attributed.string().to_string()
        } else if let Some(plain) = string.downcast_ref::<NSString>() {
            plain.to_string()
        } else {
            return;
        };
        // SAFETY: winit's view is an `NSTextInputClient`, which answers
        // `-(BOOL)hasMarkedText`.
        let has_marked_text: bool = unsafe { msg_send![view, hasMarkedText] };
        let in_key_down = IN_KEY_DOWN.with(Cell::get) > 0;
        let replaces = replacement.location != NSNotFound as usize;
        if let Some(text) =
            super::dropped(&text, in_key_down, has_marked_text, replaces)
        {
            let insertion = Insertion {
                view: ptr::from_ref(view) as usize,
                text: text.to_string(),
            };
            DROPPED.with(|dropped| dropped.borrow_mut().push(insertion));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::dropped;

    #[test]
    fn only_text_winit_dropped_is_committed() {
        // Emoji & Symbols: no key press, nothing marked.
        assert_eq!(dropped("🎤", false, false, false), Some("🎤"));
        // Typed: the key press carries it.
        assert_eq!(dropped("a", true, false, false), None);
        // A composition's commit, in a key press or by a click on a
        // candidate: winit commits it.
        assert_eq!(dropped("你好", true, true, false), None);
        assert_eq!(dropped("你好", false, true, false), None);
        // A press-and-hold accent replacing the letter typed.
        assert_eq!(dropped("é", false, false, true), None);
        // Control characters, and nothing.
        assert_eq!(dropped("\n", false, false, false), None);
        assert_eq!(dropped("", false, false, false), None);
    }
}
