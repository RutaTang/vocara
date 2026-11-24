//! The application delegate's answer to
//! `applicationSupportsSecureRestorableState:` (port of the Runner's
//! `AppDelegate`, which answered YES).
//!
//! AppKit asks the application delegate at launch whether the app's
//! restorable state is to be decoded with secure coding, and macOS 14 and
//! later log a warning when the delegate does not say. The delegate is
//! winit's, which has no such method, and winit sets it while it creates its
//! event loop, before the app gets control. So [`declare_secure`] puts a
//! `setDelegate:` in front of AppKit's: it gives the new delegate's class an
//! `applicationSupportsSecureRestorableState:` answering YES, unless the
//! delegate answers already, and then hands the delegate to AppKit.
//! The method is added first because AppKit notes which methods a delegate
//! has when it is set.
//!
//! The method goes on the delegate's class, so every instance of that class
//! answers YES; the classes the app sets as delegate (winit's, the GUI
//! test's) are its own.
//!
//! `tests/macos_gui.rs` checks this on a Mac (CI's macOS runner): a delegate
//! without the method answers YES once set, through AppKit's `setDelegate:`,
//! and one that answers keeps its answer.

/// Makes every application delegate set from now on answer YES to
/// `applicationSupportsSecureRestorableState:` (see the module doc). Once
/// per process; call it before the app runs, as winit sets its delegate when
/// the event loop is created. Off macOS there is nothing to declare.
pub fn declare_secure() {
    #[cfg(target_os = "macos")]
    macos::declare_secure();
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::CStr;
    use std::sync::OnceLock;

    use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
    use objc2::{msg_send, sel};

    /// `-(void)setDelegate:(id)delegate`.
    type SetDelegate = unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject);

    /// `-(BOOL)applicationSupportsSecureRestorableState:(NSApplication *)app`.
    type SupportsSecureRestorableState =
        unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> Bool;

    /// The type encoding of [`SupportsSecureRestorableState`] (BOOL is
    /// `signed char` on x86_64, `bool` elsewhere).
    #[cfg(target_arch = "x86_64")]
    pub(super) const TYPES: &CStr = c"c@:@";
    #[cfg(not(target_arch = "x86_64"))]
    pub(super) const TYPES: &CStr = c"B@:@";

    /// AppKit's own `-[NSApplication setDelegate:]`, which this one replaces.
    static APPKIT_SET_DELEGATE: OnceLock<Imp> = OnceLock::new();

    /// The answer the added method gives: YES.
    unsafe extern "C-unwind" fn supports_secure_restorable_state(
        _delegate: *mut AnyObject,
        _cmd: Sel,
        _app: *mut AnyObject,
    ) -> Bool {
        Bool::YES
    }

    /// The `setDelegate:` the app's delegates go through: declares secure
    /// restorable state on the delegate, then hands it to AppKit.
    unsafe extern "C-unwind" fn set_delegate(
        app: *mut AnyObject,
        cmd: Sel,
        delegate: *mut AnyObject,
    ) {
        // SAFETY: `delegate` is the argument of a `setDelegate:` message:
        // nil or a pointer to a live object, which the caller keeps alive
        // for the length of the call.
        if let Some(delegate) = unsafe { delegate.as_ref() } {
            answer_yes(delegate);
        }
        if let Some(&appkit) = APPKIT_SET_DELEGATE.get() {
            // SAFETY: `appkit` is AppKit's implementation of this very
            // method, `-[NSApplication setDelegate:]`, called with the
            // receiver, selector and argument this call was sent with.
            unsafe { std::mem::transmute::<Imp, SetDelegate>(appkit)(app, cmd, delegate) }
        }
    }

    /// Gives `delegate`'s class `applicationSupportsSecureRestorableState:`
    /// answering YES, unless the delegate answers it already.
    fn answer_yes(delegate: &AnyObject) {
        let name = sel!(applicationSupportsSecureRestorableState:);
        // SAFETY: an application delegate conforms to NSObject (the
        // NSApplicationDelegate protocol inherits it), whose
        // `-(BOOL)respondsToSelector:(SEL)` this is.
        let answers: bool = unsafe { msg_send![delegate, respondsToSelector: name] };
        if answers {
            return;
        }
        let class = std::ptr::from_ref::<AnyClass>(delegate.class()).cast_mut();
        // SAFETY: `class` is the delegate's live, registered class (the
        // runtime takes its lock to add a method); the implementation has
        // the signature `TYPES` encodes, which is the one AppKit sends the
        // message with (receiver, selector, the application; BOOL result);
        // `TYPES` is a static C string. `class_addMethod` only adds: had
        // the class a method of that name, it would keep it.
        unsafe {
            objc2::ffi::class_addMethod(
                class,
                name,
                std::mem::transmute::<SupportsSecureRestorableState, Imp>(
                    supports_secure_restorable_state,
                ),
                TYPES.as_ptr(),
            );
        }
    }

    pub fn declare_secure() {
        let Some(class) = AnyClass::get(c"NSApplication") else {
            return;
        };
        let Some(method) = class.instance_method(sel!(setDelegate:)) else {
            return;
        };
        APPKIT_SET_DELEGATE.get_or_init(|| {
            // SAFETY: `set_delegate` has the signature of the method it
            // replaces (receiver, selector, one object argument, no result),
            // and it calls the implementation it replaces with the same
            // arguments.
            unsafe {
                method.set_implementation(std::mem::transmute::<SetDelegate, Imp>(set_delegate))
            }
        });
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use objc2::encode::Encode;
    use objc2::runtime::{AnyObject, Bool, Sel};

    #[test]
    fn the_added_method_is_encoded_as_it_is_called() {
        let expected = format!(
            "{}{}{}{}",
            Bool::ENCODING,
            <*mut AnyObject>::ENCODING,
            Sel::ENCODING,
            <*mut AnyObject>::ENCODING
        );
        assert_eq!(super::macos::TYPES.to_str(), Ok(expected.as_str()));
    }
}
