//! The open and save panels on a real Mac (CI's macOS runner has a GUI
//! session), opened through the app's own entry points as the app opens
//! them, over `vocara_services::file_panels`' AppKit backend:
//! `lyrics_panel::browse_lyrics_file` (the lyrics import: one file),
//! `technique_media_section::browse` (the technique examples: any number of
//! clips) and the settings' `pick_save_path` (`SettingsHooks::production`,
//! the diagnostics export's save panel, asked from a worker thread).
//!
//! The app runs inside `-[NSApplication run]`, as iced's event loop (winit)
//! runs it, so `NSApp.isRunning` holds and an open panel takes the app's
//! path: a sheet on the main window. AppKit wants the main thread, so this
//! test has no harness (`harness = false` in Cargo.toml): `main` runs
//! AppKit, and a driver thread runs the steps, handing each piece of AppKit
//! work to the main thread as a run-loop block in the common modes, so it
//! runs inside a sheet's or a modal panel's loop too. Not on the main
//! dispatch queue: the save panel runs modally inside a block on that queue
//! (`dispatch2::run_on_main`, for the worker that waits on it), which holds
//! the queue until the panel closes.
//!
//! What it checks, for each panel:
//! 1. it comes up where the app's come up — an open panel as a sheet on the
//!    main window, the save panel as the app's modal window while the worker
//!    waits — and shows what the call site set: an open panel chooses files,
//!    not directories, one or many, with the call site's extensions as its
//!    allowed file types (as set, or as the type identifiers macOS 11+ may
//!    report them as) and no message (no title was set); the save panel
//!    suggests the call site's file name and allows any type;
//! 2. Escape, typed, closes it as a cancel, and the call site gets nothing
//!    (the lyrics import no message, the examples an empty pick, the save
//!    panel `None`);
//! 3. a file chosen as a user chooses it from the keyboard, the call site
//!    gets exactly that file: an open panel is told the file's full path in
//!    its Go to Folder sheet (⌘⇧G, ⌘A, the path, Return), which takes it to
//!    the file's folder with the file selected, and Return opens it; the
//!    save panel gets a name in its name field (⌘A, the name), a folder
//!    through ⌘⇧G, and Return saves;
//! 4. once it is gone the main window is key again, and the activation
//!    policy is as it was.
//!
//! The keys are typed through the window server, as a keyboard types them:
//! each a `CGEvent` posted at the HID tap (`CGEventPost`), which the system
//! routes to the active app's key window, whichever process draws it. On
//! recent macOS (26 on CI) a panel's UI is a remote view another process
//! serves (`NSRemoteView`, `NSSavePanelService`): an event the app posts to
//! its own queue never reaches it, pointing the panel object at a file
//! (`setDirectoryURL:`) selects nothing, and `-[NSSavePanel ok:]` raises
//! "not implemented". Posting through the window server needs the
//! process's post-event access (TCC: Accessibility or PostEvent, asked of
//! the process responsible for it, on GitHub's hosted runners the agent the
//! job runs under, which the runner image grants Accessibility). The run
//! checks it up front (`CGPreflightPostEventAccess`, and
//! `AXIsProcessTrusted` for the record) and never asks for it
//! (`CGRequestPostEventAccess` would put a system prompt over the panels
//! that nobody answers):
//! - with it, every step is checked strictly: a typed Escape that does not
//!   close a panel, or a typed accept that leaves it up or hands the call
//!   site anything but the chosen file, fails the run;
//! - with Accessibility trusted but post-event access not reported, the
//!   keys are typed all the same, and a step they do not complete is left
//!   unverified, said so (what they do complete is checked strictly);
//! - with neither, nothing is typed: Escape is posted to the app's own
//!   queue (a panel it does not close is closed with its Cancel button's
//!   action, `cancel:`, said so), and every accept is left unverified,
//!   said so.
//!
//! A panel left up by a step that went wrong is closed (typed Escapes,
//! then `cancel:`) so the rest of the run still reports, and the panel
//! calls that may raise (the panel object fronting a remote UI) are
//! guarded: an Objective-C exception is caught and said, not left to abort
//! the process.
//!
//! On a Mac it opens a window and six panels for a minute or two, and types
//! into them through the window server: it takes the keyboard, and must not
//! run while someone is using the Mac. Off macOS it prints a skip line.

#[cfg(target_os = "macos")]
mod macos_support;

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("macos_file_panels: skipped (AppKit is macOS only)");
}

#[cfg(target_os = "macos")]
fn main() {
    macos::run();
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::OnceCell;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::{self, Receiver};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use block2::RcBlock;
    use iced::futures::StreamExt;
    use iced::Task;
    use iced_test::runtime::Action;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{available, MainThreadMarker};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSEventModifierFlags, NSEventType,
        NSOpenPanel, NSSavePanel, NSWindow,
    };
    use objc2_core_foundation::{kCFRunLoopCommonModes, CFRunLoop, CFType};
    use objc2_core_graphics::CGPreflightPostEventAccess;
    use objc2_foundation::{NSDictionary, NSPoint, NSString, NSUserDefaults};
    use objc2_uniform_type_identifiers::UTType;
    use vocara_app::app::library::Library;
    use vocara_app::app::AppEnvironment;
    use vocara_app::services::Services;
    use vocara_app::ui::settings::settings_screen::{PickSavePath, SettingsHooks};
    use vocara_app::ui::song_practice_detail::widgets::lyrics_panel::{
        self, LYRICS_FILE_EXTENSIONS,
    };
    use vocara_app::ui::technique_library::widgets::media::technique_media_section as media_section;
    use vocara_model::models::technique_media::TechniqueMedia;

    use crate::macos_support::{
        fatal, key_event, new_window, note, start, step, Checks, ESCAPE_KEY,
    };

    /// How long the whole run may take.
    const LIMIT: Duration = Duration::from_secs(480);
    /// How long the main thread may take to run a step handed to it.
    const MAIN_TIMEOUT: Duration = Duration::from_secs(30);
    /// How long the main window gets to become key.
    const KEY_TIMEOUT: Duration = Duration::from_secs(15);
    /// How long a panel gets to come up.
    const APPEAR: Duration = Duration::from_secs(15);
    /// How long a panel gets to go once asked to close.
    const CLOSE: Duration = Duration::from_secs(10);
    /// How long an Escape gets to close a panel.
    const ESCAPE_WAIT: Duration = Duration::from_secs(5);
    /// How long the app gets to be active, and the panel key, before keys
    /// are typed to it.
    const FRONT_WAIT: Duration = Duration::from_secs(5);
    /// A panel's remote UI settling before the first key is typed to it.
    const SETTLE: Duration = Duration::from_secs(1);
    /// The Go to Folder sheet coming up after ⌘⇧G.
    const SHEET_WAIT: Duration = Duration::from_secs(2);
    /// A typed path or name settling (the Go to Folder sheet completes
    /// paths as they are typed).
    const TYPED_WAIT: Duration = Duration::from_secs(1);
    /// How long a panel gets to show the folder (and the file) Go to
    /// Folder's Return took it to.
    const GO_WAIT: Duration = Duration::from_secs(5);
    /// How long a typed Return gets to close a panel.
    const RETURN_WAIT: Duration = Duration::from_secs(5);
    /// How long a call site gets to report, once its panel closed.
    const RESULT_WAIT: Duration = Duration::from_secs(10);

    thread_local! {
        /// The app's main window: the main thread's own.
        static MAIN_WINDOW: OnceCell<Retained<NSWindow>> = const { OnceCell::new() };
    }

    /// The main window (on the main thread).
    fn main_window() -> Retained<NSWindow> {
        MAIN_WINDOW
            .with(|cell| cell.get().cloned())
            .unwrap_or_else(|| fatal("the main window is asked for off the main thread"))
    }

    pub fn run() {
        let mtm = MainThreadMarker::new()
            .unwrap_or_else(|| fatal("main is not on the main thread (harness = false?)"));
        start("macos_file_panels", LIMIT);
        // Under the target directory: a plain folder the panels show as any
        // other (the system's temporary folder is under a hidden one).
        let root = tempfile::Builder::new()
            .prefix("vocara-panels-")
            .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
            .unwrap_or_else(|e| fatal(&format!("no temporary folder: {e}")));
        // A panel the app sets no folder for opens where the last one was
        // (AppKit's `NSNavLastRootDirectory`): here, the test's folder, not
        // the runner's Documents, which a panel listing could hold up on a
        // privacy prompt nobody answers. In the registration domain only:
        // nothing is written to the test's defaults.
        let last = NSString::from_str(&root.path().to_string_lossy());
        let key = NSString::from_str("NSNavLastRootDirectory");
        let value: &AnyObject = &last;
        // SAFETY: a dictionary of an NSString key to an NSString value, the
        // property-list types the registration domain takes.
        unsafe {
            NSUserDefaults::standardUserDefaults()
                .registerDefaults(&NSDictionary::from_slices(&[&*key], &[value]));
        }
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        let window = new_window(mtm, "Vocara: panels", NSPoint::new(200.0, 200.0));
        window.makeKeyAndOrderFront(None);
        MAIN_WINDOW.with(|cell| {
            let _ = cell.set(window);
        });
        thread::spawn(move || drive(root));
        // As winit runs it: `NSApp.isRunning` from here on. The driver ends
        // the process.
        app.run();
        fatal("NSApp.run returned before the checks ended");
    }

    fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
        panic
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a panic".to_string())
    }

    /// Runs `f` on the main thread, in whatever loop it is running (NSApp's,
    /// a sheet's, a modal panel's: a run-loop block in the common modes),
    /// and returns what it returns.
    fn on_main<R: Send + 'static>(f: impl FnOnce(MainThreadMarker) -> R + Send + 'static) -> R {
        let (sender, answer) = mpsc::channel::<Result<R, String>>();
        let job = Mutex::new(Some(f));
        let block = RcBlock::new(move || {
            let Some(f) = job.lock().unwrap_or_else(|e| e.into_inner()).take() else {
                return;
            };
            let Some(mtm) = MainThreadMarker::new() else {
                let _ = sender.send(Err("the block ran off the main thread".to_string()));
                return;
            };
            // A panic must not unwind into CoreFoundation.
            let result = catch_unwind(AssertUnwindSafe(|| f(mtm)));
            let _ = sender.send(result.map_err(|panic| panic_message(&*panic)));
        });
        let main = CFRunLoop::main().unwrap_or_else(|| fatal("no main run loop"));
        // SAFETY: `kCFRunLoopCommonModes` is CoreFoundation's immutable mode
        // constant; CFRunLoopPerformBlock copies the block, runs it once on
        // the main run loop (in any of the common modes) and releases it.
        // What the block holds (a mutex of a `Send` closure, a `Sender` of a
        // `Send` value) may be used and dropped on that thread.
        unsafe {
            let mode: Option<&CFType> = kCFRunLoopCommonModes.map(|mode| &**mode);
            main.perform_block(mode, Some(&block));
        }
        main.wake_up();
        match answer.recv_timeout(MAIN_TIMEOUT) {
            Ok(Ok(value)) => value,
            Ok(Err(panic)) => fatal(&format!("a step panicked on the main thread: {panic}")),
            Err(_) => fatal(&format!(
                "the main thread did not run a step within {}s",
                MAIN_TIMEOUT.as_secs()
            )),
        }
    }

    /// Polls `done` (every 50 ms) until it holds, at most `timeout`.
    fn wait_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if done() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Runs `task` to its end on a thread of its own: its outputs come back
    /// on the receiver once it has ended.
    fn run_task<M: Send + 'static>(task: Task<M>) -> Receiver<Vec<M>> {
        let (sender, outputs) = mpsc::channel();
        thread::spawn(move || {
            let Some(stream) = iced_test::runtime::task::into_stream(task) else {
                let _ = sender.send(Vec::new());
                return;
            };
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap_or_else(|e| fatal(&format!("no runtime for the task: {e}")));
            let out = runtime.block_on(
                stream
                    .filter_map(|action| async move {
                        match action {
                            Action::Output(message) => Some(message),
                            _ => None,
                        }
                    })
                    .collect(),
            );
            let _ = sender.send(out);
        });
        outputs
    }

    /// Asks the save panel through `pick` (`suggested` as its name) from a
    /// worker thread, as the export's worker does; its answer comes back on
    /// the receiver.
    fn ask_save(
        pick: PickSavePath,
        suggested: &'static str,
    ) -> Receiver<Result<Option<String>, String>> {
        let (sender, answer) = mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(pick(suggested));
        });
        answer
    }

    /// Makes the main window key, or fails the run: the panels' key checks
    /// mean nothing without one.
    fn make_key() {
        let mut last_ask: Option<Instant> = None;
        let became = wait_until(KEY_TIMEOUT, || {
            let ask = last_ask.is_none_or(|at| at.elapsed() > Duration::from_millis(500));
            if ask {
                last_ask = Some(Instant::now());
            }
            on_main(move |mtm| {
                let app = NSApplication::sharedApplication(mtm);
                let window = main_window();
                if window.isKeyWindow() {
                    return true;
                }
                if ask {
                    activate(&app);
                    window.makeKeyAndOrderFront(None);
                    window.orderFrontRegardless();
                }
                false
            })
        });
        if !became {
            let active = on_main(|mtm| NSApplication::sharedApplication(mtm).isActive());
            fatal(&format!(
                "the main window never became key in {}s (app active: {active}); \
                 the GUI session must let a test app activate and take the keyboard",
                KEY_TIMEOUT.as_secs()
            ));
        }
    }

    /// Asks for the app to be active: as winit asks, and on macOS 14+ with
    /// the newer call too.
    fn activate(app: &NSApplication) {
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
        if available!(macos = 14.0) {
            app.activate();
        }
    }

    /// Typing through the window server, as a keyboard types: each key a
    /// `CGEvent` posted at the HID tap, which the system routes to the
    /// active app's key window as it routes a keyboard's, into a panel's
    /// remote UI too. A modifier goes down before the key it modifies and
    /// up after it, as on a keyboard; each event carries the modifiers held
    /// then. Only typed with post-event access (see the file's docs).
    mod typed {
        use std::thread;
        use std::time::Duration;

        use objc2_core_graphics::{CGEvent, CGEventFlags, CGEventTapLocation};

        use crate::macos_support::{fatal, ESCAPE_KEY};

        /// `kVK_Return`.
        pub const RETURN: u16 = 0x24;
        /// `kVK_Escape`.
        pub const ESCAPE: u16 = ESCAPE_KEY;
        /// `kVK_ANSI_A`.
        pub const A: u16 = 0x00;
        /// `kVK_ANSI_G`.
        pub const G: u16 = 0x05;
        /// `kVK_Command`.
        const COMMAND: u16 = 0x37;
        /// `kVK_Shift`.
        const SHIFT: u16 = 0x38;
        /// Between two events: a fast typist's pace, which a text field
        /// that completes what is typed keeps up with.
        const GAP: Duration = Duration::from_millis(20);

        /// The ANSI key codes of `a` to `z` (`kVK_ANSI_*`).
        const LETTERS: [u16; 26] = [
            0x00, 0x0B, 0x08, 0x02, 0x0E, 0x03, 0x05, 0x04, 0x22, 0x26, 0x28, 0x25, 0x2E, 0x2D,
            0x1F, 0x23, 0x0C, 0x0F, 0x01, 0x11, 0x20, 0x09, 0x0D, 0x07, 0x10, 0x06,
        ];
        /// The ANSI key codes of `0` to `9`.
        const DIGITS: [u16; 10] = [0x1D, 0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1A, 0x1C, 0x19];

        /// Posts one key event: `code` going down or up with `flags` held,
        /// typing `text` if given (else what the layout makes of `code`).
        fn post(code: u16, down: bool, flags: CGEventFlags, text: Option<char>) {
            let event = CGEvent::new_keyboard_event(None, code, down).unwrap_or_else(|| {
                fatal(&format!(
                    "CGEventCreateKeyboardEvent made no event for key code {code}"
                ))
            });
            let event: &CGEvent = &event;
            CGEvent::set_flags(Some(event), flags);
            if let Some(c) = text {
                let mut units = [0u16; 2];
                let units = c.encode_utf16(&mut units);
                // SAFETY: `units` holds `units.len()` UTF-16 code units,
                // alive for the call, which copies them into the event.
                unsafe {
                    CGEvent::keyboard_set_unicode_string(
                        Some(event),
                        units.len() as _,
                        units.as_ptr(),
                    );
                }
            }
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(event));
            thread::sleep(GAP);
        }

        /// Presses and releases `code`, with ⌘ and ⇧ held as asked, typing
        /// `text` if given.
        fn press_typing(code: u16, command: bool, shift: bool, text: Option<char>) {
            let mut flags = CGEventFlags::empty();
            if command {
                flags |= CGEventFlags::MaskCommand;
                post(COMMAND, true, flags, None);
            }
            if shift {
                flags |= CGEventFlags::MaskShift;
                post(SHIFT, true, flags, None);
            }
            post(code, true, flags, text);
            post(code, false, flags, text);
            if shift {
                flags.remove(CGEventFlags::MaskShift);
                post(SHIFT, false, flags, None);
            }
            if command {
                flags.remove(CGEventFlags::MaskCommand);
                post(COMMAND, false, flags, None);
            }
        }

        /// Presses and releases `code` alone.
        pub fn press(code: u16) {
            press_typing(code, false, false, None);
        }

        /// ⌘ and the key `code` (⇧ too if `shift`).
        pub fn command(code: u16, shift: bool) {
            press_typing(code, true, shift, None);
        }

        /// Types `text`, a key for each character: the key a US keyboard
        /// types it with (⇧ held for a capital or an underscore), carrying
        /// the character itself, so that it types the same on any layout.
        pub fn text(text: &str) {
            for c in text.chars() {
                let (code, shift) = us_key(c).unwrap_or((0, false));
                press_typing(code, false, shift, Some(c));
            }
        }

        /// The US layout's key for `c`, and whether ⇧ is held for it.
        fn us_key(c: char) -> Option<(u16, bool)> {
            let lower = c.to_ascii_lowercase();
            let code = match lower {
                'a'..='z' => LETTERS[usize::from(lower as u8 - b'a')],
                '0'..='9' => DIGITS[usize::from(lower as u8 - b'0')],
                '/' => 0x2C,
                '.' => 0x2F,
                '-' => 0x1B,
                ' ' => 0x31,
                '_' => return Some((0x1B, true)),
                '~' => return Some((0x32, true)),
                _ => return None,
            };
            Some((code, c.is_ascii_uppercase()))
        }
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        /// `Boolean AXIsProcessTrusted(void)` (HIServices' AXUIElement.h,
        /// macOS 10.4): whether the process (as TCC sees it: the process
        /// responsible for it) is trusted with Accessibility.
        fn AXIsProcessTrusted() -> u8;
    }

    /// How the run's keys reach a panel, as the process's post-event access
    /// allows (see the file's docs).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Keys {
        /// Typed through the window server, with post-event access: every
        /// step is checked strictly.
        Typed,
        /// Typed through the window server with Accessibility trusted but
        /// post-event access not reported: a step the keys do not complete
        /// is left unverified.
        Tried,
        /// Posted to the app's own queue, with neither: a panel's remote UI
        /// may not take them, and nothing is accepted.
        InApp,
    }

    impl Keys {
        /// Asks what the process may do (never asking for more), and says.
        fn detect() -> Keys {
            let post = CGPreflightPostEventAccess();
            // SAFETY: a HIServices function with no arguments and no
            // preconditions, in the framework the attribute links.
            let trusted = unsafe { AXIsProcessTrusted() } != 0;
            println!(
                "  post-event access: CGPreflightPostEventAccess() = {post}, \
                 AXIsProcessTrusted() = {trusted}"
            );
            let keys = match (post, trusted) {
                (true, _) => Keys::Typed,
                (false, true) => Keys::Tried,
                (false, false) => Keys::InApp,
            };
            match keys {
                Keys::Typed => println!(
                    "  keys are typed through the window server (CGEventPost at the HID tap), \
                     as a keyboard types them: every step is checked strictly"
                ),
                Keys::Tried => note(
                    "CGPreflightPostEventAccess() says this process may not post events, but \
                     it is trusted with Accessibility: keys are typed through the window \
                     server all the same, and a step they do not complete is left unverified, \
                     not failed",
                ),
                Keys::InApp => note(
                    "this process may not post events through the window server: neither \
                     post-event access (CGPreflightPostEventAccess) nor Accessibility \
                     (AXIsProcessTrusted) is granted to it, as TCC sees it (the process \
                     responsible for it). Not asked for: CGRequestPostEventAccess would put a \
                     system prompt over the panels that nobody answers here. Escape is posted \
                     to the app's own queue instead (a panel it does not close is closed with \
                     cancel:), and NO panel's accept is verified on this run",
                ),
            }
            keys
        }

        /// How an Escape is sent, for the checks' wording.
        fn how(self) -> &'static str {
            match self {
                Keys::Typed | Keys::Tried => "typed",
                Keys::InApp => "posted to the app",
            }
        }
    }

    /// Where a panel came up.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Place {
        /// A sheet on the main window.
        Sheet,
        /// The app's modal window.
        Modal,
        /// Another window of the app's.
        Elsewhere,
    }

    /// What a panel showed, read on the main thread.
    #[derive(Debug, Clone)]
    struct Seen {
        place: Place,
        class: String,
        open_panel: bool,
        visible: bool,
        key: bool,
        allowed_types: Option<Vec<String>>,
        choose_files: bool,
        choose_directories: bool,
        multiple: bool,
        message: String,
        name_field: String,
        level: isize,
    }

    /// The panel up now, and where, if one is (on the main thread).
    fn find_panel(app: &NSApplication) -> Option<(Retained<NSSavePanel>, Place)> {
        let sheet = main_window()
            .attachedSheet()
            .and_then(|sheet| sheet.downcast::<NSSavePanel>().ok());
        if let Some(panel) = sheet {
            return Some((panel, Place::Sheet));
        }
        let modal = app
            .modalWindow()
            .and_then(|window| window.downcast::<NSSavePanel>().ok());
        if let Some(panel) = modal {
            return Some((panel, Place::Modal));
        }
        app.windows()
            .to_vec()
            .into_iter()
            .filter_map(|window| window.downcast::<NSSavePanel>().ok())
            .find(|panel| panel.isVisible())
            .map(|panel| (panel, Place::Elsewhere))
    }

    /// Reads what `panel` shows (on the main thread).
    fn describe(panel: &NSSavePanel, place: Place) -> Seen {
        let object: &AnyObject = panel;
        let open = object.downcast_ref::<NSOpenPanel>();
        // Extensions, as the backend sets them (see file_panels/macos.rs).
        #[allow(deprecated)]
        let allowed_types = panel
            .allowedFileTypes()
            .map(|types| types.to_vec().iter().map(|t| t.to_string()).collect());
        Seen {
            place,
            class: object.class().name().to_string_lossy().into_owned(),
            open_panel: open.is_some(),
            visible: panel.isVisible(),
            key: panel.isKeyWindow(),
            allowed_types,
            choose_files: open.is_some_and(|p| p.canChooseFiles()),
            choose_directories: open.is_some_and(|p| p.canChooseDirectories()),
            multiple: open.is_some_and(|p| p.allowsMultipleSelection()),
            message: panel.message().to_string(),
            name_field: panel.nameFieldStringValue().to_string(),
            level: panel.level(),
        }
    }

    /// Waits for a panel to come up (and finish coming in), or fails the
    /// run naming `what`.
    fn wait_for_panel(what: &str) -> Seen {
        let mut seen = None;
        let up = wait_until(APPEAR, || {
            seen = on_main(|mtm| {
                let app = NSApplication::sharedApplication(mtm);
                find_panel(&app).map(|(panel, place)| describe(&panel, place))
            });
            seen.as_ref().is_some_and(|seen| seen.visible)
        });
        if !up {
            fatal(&format!(
                "the {what} never came up in {}s (last seen: {seen:?})",
                APPEAR.as_secs()
            ));
        }
        // Let a sheet finish sliding in (and become key) before keys go to it.
        wait_until(Duration::from_secs(2), || {
            on_main(|mtm| {
                let app = NSApplication::sharedApplication(mtm);
                find_panel(&app).is_some_and(|(panel, _)| panel.isKeyWindow())
            })
        });
        thread::sleep(Duration::from_millis(300));
        let seen = on_main(|mtm| {
            let app = NSApplication::sharedApplication(mtm);
            find_panel(&app).map(|(panel, place)| describe(&panel, place))
        })
        .unwrap_or_else(|| fatal(&format!("the {what} went away by itself")));
        println!("  {what}: {seen:?}");
        if !seen.key {
            note(format!(
                "the {what} is not the key window (level {}): a key may miss it",
                seen.level
            ));
        }
        seen
    }

    /// Whether no panel is up.
    fn closed() -> bool {
        on_main(|mtm| find_panel(&NSApplication::sharedApplication(mtm)).is_none())
    }

    /// Posts `chars` to the panel up now as typed, to the app's own queue
    /// (each a keyDown and a keyUp with key code `code`).
    fn type_into_panel(chars: &str, code: u16) {
        let chars = chars.to_string();
        on_main(move |mtm| {
            let app = NSApplication::sharedApplication(mtm);
            let Some((panel, _)) = find_panel(&app) else {
                return;
            };
            let number = panel.windowNumber();
            for c in chars.chars() {
                let c = c.to_string();
                for ty in [NSEventType::KeyDown, NSEventType::KeyUp] {
                    let event = key_event(ty, &c, code, NSEventModifierFlags::empty(), number);
                    app.postEvent_atStart(&event, false);
                }
            }
        });
    }

    /// Runs `f`, AppKit calls on a panel that may raise: an Objective-C
    /// exception (which no Rust frame can catch, so it aborted the run) is
    /// caught, said with its name and reason, and answered `None`.
    fn guarded<R, F: FnOnce() -> R>(what: &str, f: F) -> Option<R> {
        struct Call<F, R> {
            f: Option<F>,
            out: Option<R>,
        }
        extern "C-unwind" fn run<F: FnOnce() -> R, R>(context: *mut std::ffi::c_void) {
            // SAFETY: `context` is the `Call<F, R>` below, alive and not
            // otherwise borrowed for the whole of `try_catch`.
            let call = unsafe { &mut *context.cast::<Call<F, R>>() };
            if let Some(f) = call.f.take() {
                call.out = Some(f());
            }
        }
        let mut call = Call {
            f: Some(f),
            out: None,
        };
        let mut exception: *mut std::ffi::c_void = std::ptr::null_mut();
        // SAFETY: `run::<F, R>` reads its context as the `Call<F, R>` it is
        // handed; `exception` is written only when one was caught.
        let caught = unsafe {
            objc2_exception_helper::try_catch(
                run::<F, R>,
                (&raw mut call).cast(),
                &raw mut exception,
            )
        };
        if caught == 0 {
            return call.out;
        }
        let said = if exception.is_null() {
            "nil".to_string()
        } else {
            // SAFETY: `try_catch` hands the caught exception over retained
            // (+1); it is an NSException, which answers `description`.
            let exception: Retained<AnyObject> =
                unsafe { Retained::from_raw(exception.cast()) }.expect("not null");
            let description: Retained<NSString> =
                unsafe { objc2::msg_send![&*exception, description] };
            description.to_string()
        };
        note(format!("AppKit raised an exception on {what}: {said}"));
        None
    }

    /// Makes sure typed keys go to the panel up now: the app active (asked
    /// for if not), the panel its key window (asked for if not), then a
    /// moment for the panel's remote UI to settle. `Err` says why not: the
    /// panel is gone, or the app cannot be made active (typed keys would go
    /// to another app). A panel that stays not key is said, and typed to
    /// all the same (its remote UI may hold the keyboard regardless).
    fn front(what: &str) -> Result<(), String> {
        let mut last_ask: Option<Instant> = None;
        let mut state = None;
        wait_until(FRONT_WAIT, || {
            let ask = last_ask.is_none_or(|at| at.elapsed() > Duration::from_millis(500));
            if ask {
                last_ask = Some(Instant::now());
            }
            state = on_main(move |mtm| {
                let app = NSApplication::sharedApplication(mtm);
                let (panel, _) = find_panel(&app)?;
                let (active, key) = (app.isActive(), panel.isKeyWindow());
                if ask && !active {
                    activate(&app);
                }
                if ask && !key {
                    guarded("makeKeyWindow", || panel.makeKeyWindow());
                }
                Some((active, key))
            });
            state.is_none_or(|(active, key)| active && key)
        });
        match state {
            None => Err(format!("the {what} went away before a key was typed")),
            Some((false, _)) => Err(format!(
                "the app is not active after {}s of asking: typed keys would go to another \
                 app, so none were typed",
                FRONT_WAIT.as_secs()
            )),
            Some((true, key)) => {
                if !key {
                    note(format!(
                        "the {what} is not the key window: typing to it all the same"
                    ));
                }
                thread::sleep(SETTLE);
                Ok(())
            }
        }
    }

    /// Escape, as `keys` sends it; whether the panel closed within
    /// `ESCAPE_WAIT`.
    fn escape(keys: Keys) -> bool {
        match keys {
            Keys::Typed | Keys::Tried => typed::press(typed::ESCAPE),
            Keys::InApp => type_into_panel("\u{1b}", ESCAPE_KEY),
        }
        wait_until(ESCAPE_WAIT, closed)
    }

    /// Closes the panel up now with its Cancel button's action, `cancel:`;
    /// fails the run if it stays up.
    fn close_with_cancel(what: &str) {
        on_main(|mtm| {
            if let Some((panel, _)) = find_panel(&NSApplication::sharedApplication(mtm)) {
                // SAFETY: `-[NSSavePanel cancel:]`, an action method; a nil
                // sender is what a programmatic press sends.
                guarded("cancel:", || unsafe { panel.cancel(None) });
            }
        });
        if !wait_until(CLOSE, closed) {
            fatal(&format!("the {what} did not close on cancel:"));
        }
    }

    /// The Escape step: Escape, as `keys` sends it, closes the panel up now
    /// as a cancel. Typed with post-event access, a panel it does not close
    /// fails the check; otherwise that is said. Either way a panel still up
    /// is then closed with `cancel:`.
    fn cancel(checks: &mut Checks, keys: Keys, what: &str) {
        let ready = match keys {
            Keys::InApp => Ok(()),
            Keys::Typed | Keys::Tried => front(what),
        };
        let missed = match ready {
            Ok(()) if escape(keys) => {
                checks.check(true, format!("Escape, {}, closes the {what}", keys.how()));
                return;
            }
            Ok(()) => format!(
                "Escape, {}, did not close the {what} in {}s",
                keys.how(),
                ESCAPE_WAIT.as_secs()
            ),
            Err(why) => format!("Escape could not be typed to the {what}: {why}"),
        };
        if keys == Keys::Typed {
            checks.check(false, missed);
        } else {
            note(format!(
                "{missed}; closing it with cancel:, its Cancel button's action"
            ));
        }
        close_with_cancel(what);
    }

    /// Closes a panel a step left up, so the run goes on: typed Escapes
    /// (the first may only close a Go to Folder sheet), then `cancel:`.
    fn dismiss(keys: Keys, what: &str) {
        if keys != Keys::InApp {
            for _ in 0..2 {
                if closed() || escape(keys) {
                    return;
                }
            }
        }
        if !closed() {
            note(format!("closing the {what} with cancel:"));
            close_with_cancel(what);
        }
    }

    /// Whether `a` and `b` name the same existing file.
    fn same_file(a: &Path, b: &Path) -> bool {
        match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }

    /// The open panel up now's selection, as its object reports it.
    fn selection() -> Vec<PathBuf> {
        on_main(|mtm| {
            let app = NSApplication::sharedApplication(mtm);
            let Some((panel, _)) = find_panel(&app) else {
                return Vec::new();
            };
            let object: &AnyObject = &panel;
            let Some(open) = object.downcast_ref::<NSOpenPanel>() else {
                return Vec::new();
            };
            guarded("URLs", || open.URLs())
                .map(|urls| {
                    urls.to_vec()
                        .into_iter()
                        .filter_map(|url| url.to_file_path())
                        .collect()
                })
                .unwrap_or_default()
        })
    }

    /// The folder the panel up now's object reports it shows.
    fn shown_folder() -> Option<PathBuf> {
        on_main(|mtm| {
            let app = NSApplication::sharedApplication(mtm);
            find_panel(&app)
                .and_then(|(panel, _)| guarded("directoryURL", || panel.directoryURL()))
                .flatten()
                .and_then(|url| url.to_file_path())
        })
    }

    /// Prints what the panel up now's object reports (its folder, its
    /// selection, its name field, whether it is key): for the run's log, as
    /// on recent macOS that object fronts a remote UI it may not follow.
    fn report(what: &str) {
        let seen = on_main(|mtm| {
            let app = NSApplication::sharedApplication(mtm);
            let (panel, _) = find_panel(&app)?;
            let folder = guarded("directoryURL", || panel.directoryURL())
                .flatten()
                .and_then(|url| url.to_file_path());
            let name = guarded("nameFieldStringValue", || {
                panel.nameFieldStringValue().to_string()
            });
            Some((folder, name, panel.isKeyWindow()))
        });
        match seen {
            None => println!("  the {what} is closed"),
            Some((folder, name, key)) => println!(
                "  the {what} reports: folder {folder:?}, chosen {:?}, name field {name:?}, \
                 key {key}",
                selection()
            ),
        }
    }

    /// Brings up the panel's Go to Folder sheet (⌘⇧G), replaces the path it
    /// shows with `path` (⌘A, then typed) and types Return. `Err` if the
    /// panel closed before Return.
    fn go_to(what: &str, path: &str) -> Result<(), String> {
        println!("  typing ⌘⇧G (Go to Folder) to the {what}");
        typed::command(typed::G, true);
        thread::sleep(SHEET_WAIT);
        if closed() {
            return Err(format!("the {what} closed on ⌘⇧G"));
        }
        println!("  typing ⌘A, then {path:?}");
        typed::command(typed::A, false);
        typed::text(path);
        thread::sleep(TYPED_WAIT);
        if closed() {
            return Err(format!("the {what} closed while a path was typed to it"));
        }
        println!("  typing Return");
        typed::press(typed::RETURN);
        Ok(())
    }

    /// Types Return to the panel up now, at most `times` times while it
    /// stays up; whether it closed.
    fn returns_close(what: &str, times: usize) -> bool {
        for _ in 0..times {
            if closed() {
                return true;
            }
            println!("  typing Return");
            typed::press(typed::RETURN);
            if wait_until(RETURN_WAIT, closed) {
                return true;
            }
            report(what);
        }
        closed()
    }

    /// Chooses `file` in the open panel up now as a user does from the
    /// keyboard, and opens it: its full path in the Go to Folder sheet,
    /// whose Return takes the panel to the file's folder with the file
    /// selected, then Return to open it. A second Return is typed if the
    /// first leaves the panel up (the sheet's first Return may only have
    /// taken the path it completed); if the panel is still up then, the
    /// file's name is typed to the list (its type-select) and Return once
    /// more. `Ok` once the panel closed (what it handed the call site is
    /// the caller's check); `Err` says how it stayed up.
    fn accept_open(what: &str, file: &Path) -> Result<(), String> {
        let path = file.to_string_lossy().into_owned();
        go_to(what, &path)?;
        let chosen = || selection().iter().any(|chosen| same_file(chosen, file));
        let shown = wait_until(GO_WAIT, || closed() || chosen());
        report(what);
        if closed() {
            note(format!(
                "the {what} closed on Go to Folder's Return, with the file's path"
            ));
            return Ok(());
        }
        if !shown {
            note(format!(
                "the {what}'s object does not report {} chosen (its UI may still have it)",
                file.display()
            ));
        }
        if returns_close(what, 2) {
            return Ok(());
        }
        let name = file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        note(format!(
            "the {what} is still up: typing {name:?} to its list, then Return"
        ));
        typed::text(&name);
        thread::sleep(TYPED_WAIT);
        report(what);
        if returns_close(what, 1) {
            return Ok(());
        }
        Err(format!(
            "it stayed up after ⌘⇧G, {path:?}, Return, two more Returns, then {name:?} \
             and Return"
        ))
    }

    /// Saves as `name` in `folder` in the save panel up now as a user does
    /// from the keyboard: the name typed over the one suggested (⌘A, then
    /// the name: the name field has the keyboard when the panel opens),
    /// the folder through the Go to Folder sheet, then Return to save (a
    /// second Return if the first leaves the panel up: the sheet's first
    /// Return may only have taken the path it completed). `Ok` once the
    /// panel closed (what it answered is the caller's check); `Err` says
    /// how it stayed up.
    fn accept_save(what: &str, folder: &Path, name: &str) -> Result<(), String> {
        println!("  typing ⌘A, then {name:?} to the {what}'s name field");
        typed::command(typed::A, false);
        typed::text(name);
        thread::sleep(TYPED_WAIT);
        report(what);
        let path = format!("{}/", folder.display());
        go_to(what, &path)?;
        let there = wait_until(GO_WAIT, || {
            closed() || shown_folder().is_some_and(|shown| same_file(&shown, folder))
        });
        report(what);
        if closed() {
            note(format!("the {what} closed on Go to Folder's Return"));
            return Ok(());
        }
        if !there {
            note(format!(
                "the {what}'s object does not report {} as its folder (its UI may still \
                 show it)",
                folder.display()
            ));
        }
        if returns_close(what, 2) {
            return Ok(());
        }
        Err(format!(
            "it stayed up after {name:?}, ⌘⇧G, {path:?}, Return and two more Returns"
        ))
    }

    /// A panel's accept, typed by `accept` as `keys` allow: whether the
    /// panel closed on it, and so whether the call site's answer is to be
    /// checked. One that cannot be typed (no post-event access) or that
    /// leaves the panel up fails the run with post-event access, and is
    /// otherwise left unverified (listed in `unverified`), said so; a panel
    /// left up is closed.
    fn accepted(
        checks: &mut Checks,
        unverified: &mut Vec<String>,
        keys: Keys,
        what: &str,
        accept: impl FnOnce() -> Result<(), String>,
    ) -> bool {
        let outcome = match keys {
            Keys::InApp => Err("this process may not type to it (no post-event access)".into()),
            Keys::Typed | Keys::Tried => front(what).and_then(|()| accept()),
        };
        let why = match outcome {
            Ok(()) => return true,
            Err(why) => why,
        };
        if keys == Keys::Typed {
            checks.check(
                false,
                format!("the {what} takes a file chosen from the keyboard: {why}"),
            );
        } else {
            note(format!("the {what}'s accept is NOT verified here: {why}"));
            unverified.push(format!("the {what}'s accept: {why}"));
        }
        dismiss(keys, what);
        false
    }

    /// Checks that the main window is key again once a panel has gone.
    fn main_window_key_again(checks: &mut Checks, what: &str) {
        let key = wait_until(Duration::from_secs(3), || {
            on_main(|_| main_window().isKeyWindow())
        });
        checks.check(
            key,
            format!("the main window is key again once the {what} is gone"),
        );
    }

    /// The checks an open panel's look gets.
    fn check_open_panel(
        checks: &mut Checks,
        what: &str,
        seen: &Seen,
        multiple: bool,
        types: &[String],
    ) {
        checks.check(
            seen.place == Place::Sheet,
            format!(
                "the {what} is a sheet on the main window (it is {:?})",
                seen.place
            ),
        );
        checks.check(
            seen.open_panel,
            format!("the {what} is an NSOpenPanel ({})", seen.class),
        );
        checks.check(
            seen.choose_files && !seen.choose_directories,
            format!(
                "the {what} chooses files, not directories (files: {}, directories: {})",
                seen.choose_files, seen.choose_directories
            ),
        );
        checks.check(
            seen.multiple == multiple,
            format!(
                "the {what} {} (allowsMultipleSelection: {})",
                if multiple {
                    "chooses many"
                } else {
                    "chooses one"
                },
                seen.multiple
            ),
        );
        checks.check(
            allows_exactly(seen.allowed_types.as_deref().unwrap_or_default(), types),
            format!(
                "the {what} allows the call site's extensions {types:?}, as themselves or \
                 their type identifiers (it allows {:?})",
                seen.allowed_types
            ),
        );
        checks.check(
            seen.message.is_empty(),
            format!(
                "the {what} shows no message, as no title was set (it shows {:?})",
                seen.message
            ),
        );
    }

    /// Whether a panel's allowed file types are `extensions`: the backend
    /// sets the extensions themselves (`setAllowedFileTypes:`), and AppKit
    /// may report each back as is or as the type identifier it stands for
    /// (since macOS 11 the filter is kept as `UTType`s). Every extension
    /// must be there in one form, and nothing else.
    fn allows_exactly(allowed: &[String], extensions: &[String]) -> bool {
        let forms: Vec<Vec<String>> = extensions
            .iter()
            .map(|ext| {
                let mut forms = vec![ext.to_lowercase()];
                if let Some(uti) = UTType::typeWithFilenameExtension(&NSString::from_str(ext)) {
                    forms.push(uti.identifier().to_string().to_lowercase());
                }
                forms
            })
            .collect();
        let allowed: Vec<String> = allowed.iter().map(|t| t.to_lowercase()).collect();
        forms
            .iter()
            .all(|forms| forms.iter().any(|form| allowed.contains(form)))
            && allowed
                .iter()
                .all(|t| forms.iter().any(|forms| forms.contains(t)))
    }

    /// A file of `name` in a new folder `folder` under `root`.
    fn file_in(root: &Path, folder: &str, name: &str) -> PathBuf {
        let dir = root.join(folder);
        std::fs::create_dir_all(&dir)
            .unwrap_or_else(|e| fatal(&format!("cannot make {}: {e}", dir.display())));
        let file = dir.join(name);
        std::fs::write(&file, b"test")
            .unwrap_or_else(|e| fatal(&format!("cannot write {}: {e}", file.display())));
        file
    }

    fn drive(root: tempfile::TempDir) {
        let mut checks = Checks::default();
        // The steps left unverified (without post-event access), said again
        // at the end.
        let mut unverified: Vec<String> = Vec::new();

        step("making the main window key");
        make_key();
        let policy = on_main(|mtm| NSApplication::sharedApplication(mtm).activationPolicy());
        let running = on_main(|mtm| NSApplication::sharedApplication(mtm).isRunning());
        checks.check(
            running,
            "NSApp is running, as under iced: open panels are sheets",
        );

        step("how keys reach the panels");
        let keys = Keys::detect();

        let lyrics_types: Vec<String> = LYRICS_FILE_EXTENSIONS.map(String::from).to_vec();
        let lyrics = "lyrics import's open panel";

        step("the lyrics import's open panel, cancelled with Escape");
        let done = run_task(on_main(|_| lyrics_panel::browse_lyrics_file()));
        let seen = wait_for_panel(lyrics);
        check_open_panel(&mut checks, lyrics, &seen, false, &lyrics_types);
        cancel(&mut checks, keys, lyrics);
        let out = done.recv_timeout(RESULT_WAIT);
        checks.check(
            matches!(&out, Ok(messages) if messages.is_empty()),
            format!("a cancelled lyrics import reports nothing (got {out:?})"),
        );
        main_window_key_again(&mut checks, lyrics);

        step("the lyrics import's open panel, a file chosen from the keyboard");
        let verse = file_in(root.path(), "lyrics", "verse.lrc");
        let done = run_task(on_main(|_| lyrics_panel::browse_lyrics_file()));
        wait_for_panel(lyrics);
        if accepted(&mut checks, &mut unverified, keys, lyrics, || {
            accept_open(lyrics, &verse)
        }) {
            let out = done.recv_timeout(RESULT_WAIT);
            checks.check(
                matches!(
                    &out,
                    Ok(messages) if matches!(
                        messages.as_slice(),
                        [lyrics_panel::Message::ImportLyricsFromFile(path)] if same_file(path, &verse)
                    )
                ),
                format!(
                    "the chosen {} comes back as the import's message (got {out:?})",
                    verse.display()
                ),
            );
        } else {
            let _ = done.recv_timeout(RESULT_WAIT);
        }
        main_window_key_again(&mut checks, lyrics);

        let media_types = TechniqueMedia::picker_extensions();
        let media = "technique examples' open panel";

        step("the technique examples' open panel, cancelled with Escape");
        let done = run_task(on_main(|_| media_section::browse()));
        let seen = wait_for_panel(media);
        check_open_panel(&mut checks, media, &seen, true, &media_types);
        cancel(&mut checks, keys, media);
        let out = done.recv_timeout(RESULT_WAIT);
        checks.check(
            matches!(&out, Ok(messages) if matches!(
                messages.as_slice(),
                [media_section::Message::Picked(paths)] if paths.is_empty()
            )),
            format!("a cancelled browse picks nothing (got {out:?})"),
        );
        main_window_key_again(&mut checks, media);

        step("the technique examples' open panel, a clip chosen from the keyboard");
        let clip = file_in(root.path(), "clips", "clip.wav");
        let done = run_task(on_main(|_| media_section::browse()));
        wait_for_panel(media);
        if accepted(&mut checks, &mut unverified, keys, media, || {
            accept_open(media, &clip)
        }) {
            let out = done.recv_timeout(RESULT_WAIT);
            checks.check(
                matches!(&out, Ok(messages) if matches!(
                    messages.as_slice(),
                    [media_section::Message::Picked(paths)]
                        if matches!(paths.as_slice(), [path] if same_file(Path::new(path), &clip))
                )),
                format!(
                    "the chosen {} comes back as the examples' pick, alone (got {out:?})",
                    clip.display()
                ),
            );
        } else {
            let _ = done.recv_timeout(RESULT_WAIT);
        }
        main_window_key_again(&mut checks, media);

        let services = Services::detached(Arc::new(Library::in_memory()), AppEnvironment::Dev);
        let pick = SettingsHooks::production(&services).pick_save_path;
        let save = "diagnostics export's save panel";
        let suggested = "vocara-diagnostics.zip";

        step("the diagnostics export's save panel, cancelled with Escape");
        let answer = ask_save(Arc::clone(&pick), suggested);
        let seen = wait_for_panel(save);
        checks.check(
            seen.place == Place::Modal,
            format!(
                "the {save} is the app's modal window while the worker waits (it is {:?})",
                seen.place
            ),
        );
        checks.check(
            !seen.open_panel,
            format!("the {save} is a save panel ({})", seen.class),
        );
        checks.check(
            seen.name_field == suggested,
            format!(
                "the {save} suggests {suggested:?} (its name field: {:?})",
                seen.name_field
            ),
        );
        checks.check(
            seen.allowed_types
                .as_ref()
                .is_none_or(|types| types.is_empty()),
            format!(
                "the {save} allows any type (allowed: {:?})",
                seen.allowed_types
            ),
        );
        checks.check(
            seen.message.is_empty(),
            format!("the {save} shows no message (it shows {:?})", seen.message),
        );
        cancel(&mut checks, keys, save);
        let out = answer.recv_timeout(RESULT_WAIT);
        checks.check(
            matches!(out, Ok(Ok(None))),
            format!("a cancelled save panel answers None (got {out:?})"),
        );
        main_window_key_again(&mut checks, save);

        step("the diagnostics export's save panel, a name and a folder chosen from the keyboard");
        let exports = root.path().join("exports");
        std::fs::create_dir_all(&exports)
            .unwrap_or_else(|e| fatal(&format!("cannot make {}: {e}", exports.display())));
        let chosen = "chosen.zip";
        let answer = ask_save(Arc::clone(&pick), suggested);
        wait_for_panel(save);
        if accepted(&mut checks, &mut unverified, keys, save, || {
            accept_save(save, &exports, chosen)
        }) {
            let out = answer.recv_timeout(RESULT_WAIT);
            let lands = |path: &str| {
                let path = Path::new(path);
                path.file_name().is_some_and(|name| name == chosen)
                    && path
                        .parent()
                        .is_some_and(|parent| same_file(parent, &exports))
            };
            checks.check(
                matches!(&out, Ok(Ok(Some(path))) if lands(path)),
                format!(
                    "Return answers the chosen {} (got {out:?})",
                    exports.join(chosen).display()
                ),
            );
        } else {
            let _ = answer.recv_timeout(RESULT_WAIT);
        }
        main_window_key_again(&mut checks, save);

        step("after the panels");
        let after = on_main(|mtm| NSApplication::sharedApplication(mtm).activationPolicy());
        checks.check(
            after == policy,
            format!("the activation policy is as it was ({policy:?}, now {after:?})"),
        );
        if unverified.is_empty() {
            println!("  no step was left unverified (keys: {keys:?})");
        } else {
            for what in &unverified {
                note(format!("NOT verified on this run: {what}"));
            }
        }
        drop(root);
        on_main(move |_| {
            checks.finish();
        });
    }
}
