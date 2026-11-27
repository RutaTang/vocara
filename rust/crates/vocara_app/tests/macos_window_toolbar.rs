//! The window's unified toolbar on a real Mac (CI's macOS runner has a GUI
//! session): the app's own window (winit's, made from
//! `vocara_app::app::window_settings` as iced makes it: a hidden,
//! transparent title bar over a full-size content view), given its empty
//! toolbar by `vocara_services::window_toolbar::install`, as the app does
//! once the window has opened.
//!
//! What it checks:
//! 1. the window has the toolbar afterwards (and, from macOS 11, in the
//!    unified style), and a second install leaves it alone;
//! 2. the close button's frame is vertically centred in the window's top
//!    52 px, the pages' toolbar (from macOS 11; on 10.15, which has no
//!    unified style, the offset is printed as a note);
//! 3. a point on the toolbar's empty area, well right of the traffic
//!    lights, hit-tests to the content view (winit's): the transparent
//!    title bar lets clicks through to the app, whose toolbar takes them.
//!
//! It shows a small window for a moment. Off macOS it prints a skip line.

#[cfg(target_os = "macos")]
mod macos_support;

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("macos_window_toolbar: skipped (AppKit is macOS only)");
}

#[cfg(target_os = "macos")]
fn main() {
    macos::run();
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::RefCell;
    use std::time::{Duration, Instant};

    use dispatch2::DispatchQueue;
    use iced::Size;
    use iced_winit::winit;
    use objc2::rc::Retained;
    use objc2::runtime::NSObjectProtocol;
    use objc2::{sel, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSView, NSWindow, NSWindowButton, NSWindowToolbarStyle};
    use objc2_foundation::NSPoint;
    use vocara_services::window_toolbar::{self, UNIFIED_HEIGHT};
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
    use winit::window::{Window, WindowId};

    use crate::macos_support::{fatal, note, start, step, Checks};

    /// The test window's title, which finds its `NSWindow`.
    const TITLE: &str = "vocara window toolbar";

    thread_local! {
        static CHECKS: RefCell<Checks> = RefCell::new(Checks::default());
        static DONE: RefCell<bool> = const { RefCell::new(false) };
    }

    fn check(ok: bool, what: impl Into<String>) {
        CHECKS.with(|checks| {
            checks.borrow_mut().check(ok, what);
        });
    }

    /// The test's window, found by its title.
    fn window(app: &NSApplication) -> Option<Retained<NSWindow>> {
        app.windows()
            .to_vec()
            .into_iter()
            .find(|window| window.title().to_string() == TITLE)
    }

    /// Installs the toolbar and checks it (on the main queue, outside
    /// winit's handler, as the app's update runs).
    fn run_checks() {
        let Some(mtm) = MainThreadMarker::new() else {
            check(false, "the checks run on the main thread");
            DONE.with(|done| *done.borrow_mut() = true);
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        let Some(window) = window(&app) else {
            check(false, "the winit window is found by its title");
            DONE.with(|done| *done.borrow_mut() = true);
            return;
        };
        step("installing the toolbar");
        check(window.toolbar().is_none(), "no toolbar before the install");
        window_toolbar::install();
        let Some(toolbar) = window.toolbar() else {
            check(false, "the window has a toolbar after the install");
            DONE.with(|done| *done.borrow_mut() = true);
            return;
        };
        check(true, "the window has a toolbar after the install");
        let unified = window.respondsToSelector(sel!(toolbarStyle));
        if unified {
            check(
                window.toolbarStyle() == NSWindowToolbarStyle::Unified,
                "the toolbar is in the unified style",
            );
        } else {
            note("no toolbarStyle (macOS 10.15): the older unified title bar");
        }
        window_toolbar::install();
        check(
            window
                .toolbar()
                .is_some_and(|again| std::ptr::eq(&*again, &*toolbar)),
            "a second install keeps the same toolbar",
        );

        step("measuring the traffic lights");
        let Some(close) = window.standardWindowButton(NSWindowButton::CloseButton) else {
            check(false, "the window has a close button");
            DONE.with(|done| *done.borrow_mut() = true);
            return;
        };
        let frame = close.convertRect_toView(close.bounds(), None);
        let window_height = window.frame().size.height;
        // Window coordinates run up from the bottom: the button's centre,
        // down from the window's top.
        let centre = window_height - (frame.origin.y + frame.size.height / 2.0);
        let offset = centre - UNIFIED_HEIGHT / 2.0;
        println!(
            "  close button: {:.1} × {:.1}, its centre {centre:.1} down a {window_height:.0} px window",
            frame.size.width, frame.size.height
        );
        if unified {
            check(
                offset.abs() <= 1.5,
                format!(
                    "the close button is centred in the top {UNIFIED_HEIGHT} px \
                     (its centre {centre:.1} down, {offset:+.1} off)"
                ),
            );
        } else {
            note(format!(
                "on 10.15 the close button's centre is {centre:.1} down"
            ));
        }
        check(
            frame.origin.x + frame.size.width < 80.0,
            format!(
                "the traffic lights clear the toolbar's 80 px inset (the close button ends at {:.1})",
                frame.origin.x + frame.size.width
            ),
        );

        step("hit-testing the toolbar's empty area");
        let Some(content) = window.contentView() else {
            check(false, "the window has a content view");
            DONE.with(|done| *done.borrow_mut() = true);
            return;
        };
        // Window coordinates: 300 px in, the toolbar's middle row.
        let point = NSPoint::new(300.0, window_height - UNIFIED_HEIGHT / 2.0);
        // SAFETY: `superview` is unsafe in objc2-app-kit only because a
        // view's superview is not retained by it; the frame view is the
        // window's own root, which lives as long as the window, and the
        // `Retained` taken here keeps it while it is used.
        let frame_view: Option<Retained<NSView>> = unsafe { content.superview() };
        let hit = frame_view.as_ref().and_then(|root| {
            // `hitTest:` takes a point in the receiver's superview's
            // coordinates; the frame view is the window's root, whose are
            // the window's.
            root.hitTest(point)
        });
        let through = hit.as_ref().is_some_and(|view| {
            // `isDescendantOf:` is true of the view itself too.
            view.isDescendantOf(&content)
        });
        check(
            through,
            format!(
                "a click on the toolbar's empty area reaches the content view (hit {})",
                hit.map_or("nothing".to_string(), |view| view
                    .class()
                    .name()
                    .to_string_lossy()
                    .into_owned())
            ),
        );
        DONE.with(|done| *done.borrow_mut() = true);
    }

    #[derive(Default)]
    struct Test {
        window: Option<Window>,
        scheduled: bool,
        opened: Option<Instant>,
    }

    impl ApplicationHandler for Test {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            // The app's window as iced makes it, smaller.
            let mut settings = vocara_app::app::window_settings();
            settings.size = Size::new(960.0, 600.0);
            settings.min_size = None;
            let attributes =
                iced_winit::conversion::window_attributes(settings, TITLE, 1.0, None, None);
            match event_loop.create_window(attributes) {
                Ok(window) => {
                    self.window = Some(window);
                    self.opened = Some(Instant::now());
                }
                Err(error) => {
                    check(false, format!("winit made a window ({error})"));
                    DONE.with(|done| *done.borrow_mut() = true);
                }
            }
        }

        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            if DONE.with(|done| *done.borrow()) {
                event_loop.exit();
                return;
            }
            // Once the window has been up a moment, as the app's opens.
            let ready = self
                .opened
                .is_some_and(|at| at.elapsed() > Duration::from_millis(300));
            if ready && !self.scheduled {
                self.scheduled = true;
                DispatchQueue::main().exec_async(run_checks);
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + Duration::from_millis(50),
            ));
        }
    }

    pub fn run() {
        start("macos_window_toolbar", Duration::from_secs(60));
        step("opening the window");
        let event_loop = match EventLoop::new() {
            Ok(event_loop) => event_loop,
            Err(error) => fatal(&format!("no event loop ({error})")),
        };
        let mut test = Test::default();
        if let Err(error) = event_loop.run_app(&mut test) {
            check(false, format!("the event loop ran ({error})"));
        }
        CHECKS.with(|checks| checks.borrow().finish());
    }
}
