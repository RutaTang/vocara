//! The quits macOS sends through `NSApplication.terminate:` (port of the
//! Runner's `AppDelegate.applicationShouldTerminate`, and of the exit request
//! `main.dart`'s `AppLifecycleListener` answered).
//!
//! The red close button is only one quit path: Cmd+Q, the app-menu Quit, a
//! Dock quit and logout all go through `terminate:`, which never reaches the
//! window's close request. Left alone, AppKit ends the process at once
//! (winit's application delegate does not implement
//! `applicationShouldTerminate:`), so the app's quit guard — the separation
//! sidecars killed, the in-flight recordings and saves finalized — never ran
//! on the most common way to quit. [`intercept`] puts a `terminate:` in front
//! of AppKit's that asks the app instead ([`QuitRequests::subscribe`]); the
//! app runs its quit guard and then exits by itself, as it does for the close
//! button.
//!
//! As the Runner did, a repeat quit while the guard is still finishing (a
//! second Cmd+Q, which cut off the save the guard waited on) is ignored, for
//! at most [`REPEAT_WINDOW`], a little longer than the guard's own longest
//! wait: a guard that never answers cannot keep the app from quitting, so a
//! quit after that window goes through to AppKit at once.
//!
//! Force-quit / `kill -9` is not interceptable; see `recording_finalizer`.

use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

/// How long a repeat quit is ignored after the first one asked the app
/// (the Runner's 65 s): past it, the guard is taken never to answer.
pub const REPEAT_WINDOW: Duration = Duration::from_secs(65);

/// What a `terminate:` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The app was asked to quit: it runs its quit guard, then exits.
    Asked,
    /// A quit asked less than [`REPEAT_WINDOW`] ago is still under way:
    /// this one is ignored.
    Ignored,
    /// The quit asked [`REPEAT_WINDOW`] ago never finished: AppKit quits now.
    TerminateNow,
}

/// The quit requests and who listens for them; the app uses
/// [`QuitRequests::global`], which the intercepted `terminate:` feeds.
#[derive(Debug, Default)]
pub struct QuitRequests {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    /// When the first quit asked the app (the Runner's `quitRequestedAt`).
    asked_at: Option<Instant>,
    /// Asked while nobody listened: the next listener gets it at once.
    pending: bool,
    listeners: Vec<UnboundedSender<()>>,
}

impl QuitRequests {
    /// The process-wide requests.
    pub fn global() -> &'static QuitRequests {
        static GLOBAL: OnceLock<QuitRequests> = OnceLock::new();
        GLOBAL.get_or_init(QuitRequests::default)
    }

    /// A `terminate:` arrived at `now`: asks the app the first time; then
    /// ignores repeats for [`REPEAT_WINDOW`], and lets one through after.
    pub fn terminate(&self, now: Instant) -> Verdict {
        let mut s = self.lock();
        if let Some(at) = s.asked_at {
            return if now.saturating_duration_since(at) < REPEAT_WINDOW {
                Verdict::Ignored
            } else {
                Verdict::TerminateNow
            };
        }
        s.asked_at = Some(now);
        s.listeners.retain(|listener| listener.send(()).is_ok());
        if s.listeners.is_empty() {
            s.pending = true;
        }
        Verdict::Asked
    }

    /// A listener for the quit requests: the one asked before anybody
    /// listened, if any, then those asked from now on.
    pub fn subscribe(&self) -> UnboundedReceiver<()> {
        let (sender, receiver) = unbounded_channel();
        let mut s = self.lock();
        if std::mem::take(&mut s.pending) {
            let _ = sender.send(());
        }
        s.listeners.push(sender);
        receiver
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Routes `NSApplication.terminate:` through [`QuitRequests::global`] (see
/// the module doc). Once per process; call it before the app runs. Off macOS
/// there is no `terminate:`, and nothing to do.
pub fn intercept() {
    #[cfg(target_os = "macos")]
    macos::intercept();
}

#[cfg(target_os = "macos")]
mod macos {
    use std::sync::OnceLock;
    use std::time::Instant;

    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::sel;

    use super::{QuitRequests, Verdict};

    /// `-(void)terminate:(id)sender`.
    type Terminate = unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject);

    /// AppKit's own `-[NSApplication terminate:]`, which this one replaces.
    static APPKIT_TERMINATE: OnceLock<Imp> = OnceLock::new();

    /// The `terminate:` the app answers: asks the app's quit guard, and
    /// hands over to AppKit only when that guard never answered.
    unsafe extern "C-unwind" fn terminate(app: *mut AnyObject, cmd: Sel, sender: *mut AnyObject) {
        if QuitRequests::global().terminate(Instant::now()) != Verdict::TerminateNow {
            return;
        }
        if let Some(&appkit) = APPKIT_TERMINATE.get() {
            // SAFETY: `appkit` is AppKit's implementation of this very
            // method, `-[NSApplication terminate:]`, called with the
            // receiver, selector and argument this call was sent with.
            unsafe { std::mem::transmute::<Imp, Terminate>(appkit)(app, cmd, sender) }
        }
    }

    pub fn intercept() {
        let Some(class) = AnyClass::get(c"NSApplication") else {
            return;
        };
        let Some(method) = class.instance_method(sel!(terminate:)) else {
            return;
        };
        APPKIT_TERMINATE.get_or_init(|| {
            // SAFETY: `terminate` has the signature of the method it
            // replaces (receiver, selector, one object argument, no result),
            // and it calls the implementation it replaces with the same
            // arguments.
            unsafe { method.set_implementation(std::mem::transmute::<Terminate, Imp>(terminate)) }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_quit_asks_the_app_and_a_repeat_is_ignored() {
        let requests = QuitRequests::default();
        let mut app = requests.subscribe();
        let start = Instant::now();
        assert_eq!(requests.terminate(start), Verdict::Asked);
        assert_eq!(app.try_recv(), Ok(()));
        // A second Cmd+Q while the guard finishes a save does not cut it off.
        let soon = start + Duration::from_secs(5);
        assert_eq!(requests.terminate(soon), Verdict::Ignored);
        assert!(app.try_recv().is_err(), "the app is asked once");
        let almost = start + REPEAT_WINDOW - Duration::from_millis(1);
        assert_eq!(requests.terminate(almost), Verdict::Ignored);
    }

    #[test]
    fn a_guard_that_never_answers_cannot_keep_the_app_from_quitting() {
        let requests = QuitRequests::default();
        let _app = requests.subscribe();
        let start = Instant::now();
        assert_eq!(requests.terminate(start), Verdict::Asked);
        assert_eq!(
            requests.terminate(start + REPEAT_WINDOW),
            Verdict::TerminateNow
        );
    }

    #[test]
    fn a_quit_asked_before_the_app_listens_reaches_it_when_it_does() {
        let requests = QuitRequests::default();
        assert_eq!(requests.terminate(Instant::now()), Verdict::Asked);
        let mut app = requests.subscribe();
        assert_eq!(app.try_recv(), Ok(()));
        // Delivered once: a later listener does not get it again.
        let mut later = requests.subscribe();
        assert!(later.try_recv().is_err());
    }

    #[test]
    fn a_listener_that_has_gone_is_forgotten() {
        let requests = QuitRequests::default();
        drop(requests.subscribe());
        assert_eq!(requests.terminate(Instant::now()), Verdict::Asked);
        // Nobody was left to hear it: the next listener gets it.
        let mut app = requests.subscribe();
        assert_eq!(app.try_recv(), Ok(()));
    }
}
