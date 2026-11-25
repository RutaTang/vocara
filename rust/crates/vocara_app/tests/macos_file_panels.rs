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
//! 2. Escape, posted to the panel as typed, closes it as a cancel, and the
//!    call site gets nothing (the lyrics import no message, the examples an
//!    empty pick, the save panel `None`). A panel the posted key does not
//!    reach is closed with its Cancel button's action (`cancel:`), and the
//!    run says so;
//! 3. accepted with a file chosen — an open panel pointed at the file, which
//!    selects it, the save panel given a folder and a name — through the OK
//!    button's action (`ok:`), it hands the call site that file. A panel
//!    that cannot be made to choose a file here is cancelled instead, and
//!    the run says the accept went unverified;
//! 4. once it is gone the main window is key again, and the activation
//!    policy is as it was.
//!
//! On a Mac it opens a window and six panels for a minute or so, and takes
//! the keyboard. Off macOS it prints a skip line.

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
    use objc2_foundation::{NSDictionary, NSPoint, NSString, NSUserDefaults, NSURL};
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
    const LIMIT: Duration = Duration::from_secs(300);
    /// How long the main thread may take to run a step handed to it.
    const MAIN_TIMEOUT: Duration = Duration::from_secs(30);
    /// How long the main window gets to become key.
    const KEY_TIMEOUT: Duration = Duration::from_secs(15);
    /// How long a panel gets to come up.
    const APPEAR: Duration = Duration::from_secs(15);
    /// How long a panel gets to go once asked to close.
    const CLOSE: Duration = Duration::from_secs(10);
    /// How long a posted Escape gets to close a panel.
    const ESCAPE_WAIT: Duration = Duration::from_secs(5);
    /// How long a panel gets to show the file or folder it was pointed at.
    const SELECT_WAIT: Duration = Duration::from_secs(10);
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
                    // As winit asks, and on macOS 14+ with the newer call too.
                    #[allow(deprecated)]
                    app.activateIgnoringOtherApps(true);
                    if available!(macos = 14.0) {
                        app.activate();
                    }
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
                "the {what} is not the key window (level {}): a posted key may miss it",
                seen.level
            ));
        }
        seen
    }

    /// Whether no panel is up.
    fn closed() -> bool {
        on_main(|mtm| find_panel(&NSApplication::sharedApplication(mtm)).is_none())
    }

    /// Posts `chars` to the panel up now as typed (each a keyDown and a
    /// keyUp with key code `code`).
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

    /// Sends the panel up now its OK button's action, `ok:`. Whether AppKit
    /// took it without raising.
    fn press_ok() -> bool {
        on_main(|mtm| {
            let Some((panel, _)) = find_panel(&NSApplication::sharedApplication(mtm)) else {
                return true;
            };
            // SAFETY: `-[NSSavePanel ok:]`, an action method; a nil sender
            // is what a programmatic press sends.
            guarded("ok:", || unsafe { panel.ok(None) }).is_some()
        })
    }

    /// Closes the panel up now as a cancel: Escape, posted as typed, else
    /// its Cancel button's action (`cancel:`), said so. Fails the run if it
    /// stays up.
    fn cancel(checks: &mut Checks, what: &str) {
        type_into_panel("\u{1b}", ESCAPE_KEY);
        if wait_until(ESCAPE_WAIT, closed) {
            checks.check(true, format!("Escape closes the {what}"));
            return;
        }
        note(format!(
            "the posted Escape did not close the {what} in {}s; \
             closing it with cancel:, its Cancel button's action",
            ESCAPE_WAIT.as_secs()
        ));
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

    /// Whether `a` and `b` name the same existing file.
    fn same_file(a: &Path, b: &Path) -> bool {
        match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }

    /// The open panel up now's selection, as paths.
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

    /// Chooses `file` in the open panel up now and presses OK: pointed at
    /// the file, the panel selects it once it has listed its folder; if it
    /// does not, the file's name is typed into its list (the list's
    /// type-select). OK is pressed only once the panel reports the file
    /// chosen: an open panel with nothing chosen raises an Objective-C
    /// exception on `ok:` (macOS 26), which no Rust frame can catch. Whether
    /// it closed on the file; `false` leaves the panel up for the caller to
    /// cancel, its accept unverified (said so).
    fn accept_open(file: &Path) -> bool {
        let target = file.to_path_buf();
        on_main(move |mtm| {
            let app = NSApplication::sharedApplication(mtm);
            if let (Some((panel, _)), Some(url)) =
                (find_panel(&app), NSURL::from_file_path(&target))
            {
                guarded("setDirectoryURL:", || panel.setDirectoryURL(Some(&url)));
            }
        });
        let chosen = || selection().iter().any(|chosen| same_file(chosen, file));
        let mut selected = wait_until(SELECT_WAIT, chosen);
        if !selected {
            note(format!(
                "pointed at {}, the open panel chose nothing: typing its name into the list",
                file.display()
            ));
            let name = file
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            type_into_panel(&name, 0);
            selected = wait_until(SELECT_WAIT, chosen);
        }
        if !selected {
            note(format!(
                "the open panel reports no selection of {}: OK is not pressed",
                file.display()
            ));
            return false;
        }
        press_ok() && wait_until(CLOSE, closed)
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

        step("making the main window key");
        make_key();
        let policy = on_main(|mtm| NSApplication::sharedApplication(mtm).activationPolicy());
        let running = on_main(|mtm| NSApplication::sharedApplication(mtm).isRunning());
        checks.check(
            running,
            "NSApp is running, as under iced: open panels are sheets",
        );

        let lyrics_types: Vec<String> = LYRICS_FILE_EXTENSIONS.map(String::from).to_vec();
        let lyrics = "lyrics import's open panel";

        step("the lyrics import's open panel, cancelled with Escape");
        let done = run_task(on_main(|_| lyrics_panel::browse_lyrics_file()));
        let seen = wait_for_panel(lyrics);
        check_open_panel(&mut checks, lyrics, &seen, false, &lyrics_types);
        cancel(&mut checks, lyrics);
        let out = done.recv_timeout(RESULT_WAIT);
        checks.check(
            matches!(&out, Ok(messages) if messages.is_empty()),
            format!("a cancelled lyrics import reports nothing (got {out:?})"),
        );
        main_window_key_again(&mut checks, lyrics);

        step("the lyrics import's open panel, a file chosen");
        let verse = file_in(root.path(), "lyrics", "verse.lrc");
        let done = run_task(on_main(|_| lyrics_panel::browse_lyrics_file()));
        wait_for_panel(lyrics);
        if accept_open(&verse) {
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
            note(format!(
                "the {lyrics} could not be made to choose a file here: its accept is NOT verified"
            ));
            cancel(&mut checks, lyrics);
            let _ = done.recv_timeout(RESULT_WAIT);
        }
        main_window_key_again(&mut checks, lyrics);

        let media_types = TechniqueMedia::picker_extensions();
        let media = "technique examples' open panel";

        step("the technique examples' open panel, cancelled with Escape");
        let done = run_task(on_main(|_| media_section::browse()));
        let seen = wait_for_panel(media);
        check_open_panel(&mut checks, media, &seen, true, &media_types);
        cancel(&mut checks, media);
        let out = done.recv_timeout(RESULT_WAIT);
        checks.check(
            matches!(&out, Ok(messages) if matches!(
                messages.as_slice(),
                [media_section::Message::Picked(paths)] if paths.is_empty()
            )),
            format!("a cancelled browse picks nothing (got {out:?})"),
        );
        main_window_key_again(&mut checks, media);

        step("the technique examples' open panel, a clip chosen");
        let clip = file_in(root.path(), "clips", "clip.wav");
        let done = run_task(on_main(|_| media_section::browse()));
        wait_for_panel(media);
        if accept_open(&clip) {
            let out = done.recv_timeout(RESULT_WAIT);
            checks.check(
                matches!(&out, Ok(messages) if matches!(
                    messages.as_slice(),
                    [media_section::Message::Picked(paths)]
                        if matches!(paths.as_slice(), [path] if same_file(Path::new(path), &clip))
                )),
                format!(
                    "the chosen {} comes back as the examples' pick (got {out:?})",
                    clip.display()
                ),
            );
        } else {
            note(format!(
                "the {media} could not be made to choose a file here: its accept is NOT verified"
            ));
            cancel(&mut checks, media);
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
        cancel(&mut checks, save);
        let out = answer.recv_timeout(RESULT_WAIT);
        checks.check(
            matches!(out, Ok(Ok(None))),
            format!("a cancelled save panel answers None (got {out:?})"),
        );
        main_window_key_again(&mut checks, save);

        step("the diagnostics export's save panel, a folder and a name chosen");
        let exports = root.path().join("exports");
        std::fs::create_dir_all(&exports)
            .unwrap_or_else(|e| fatal(&format!("cannot make {}: {e}", exports.display())));
        let chosen = "chosen.zip";
        let answer = ask_save(Arc::clone(&pick), suggested);
        wait_for_panel(save);
        let folder = exports.clone();
        on_main(move |mtm| {
            let app = NSApplication::sharedApplication(mtm);
            if let (Some((panel, _)), Some(url)) =
                (find_panel(&app), NSURL::from_directory_path(&folder))
            {
                guarded("setDirectoryURL:", || panel.setDirectoryURL(Some(&url)));
                guarded("setNameFieldStringValue:", || {
                    panel.setNameFieldStringValue(&NSString::from_str(chosen));
                });
            }
        });
        let shows_folder = wait_until(SELECT_WAIT, || {
            let folder = on_main(|mtm| {
                let app = NSApplication::sharedApplication(mtm);
                find_panel(&app)
                    .and_then(|(panel, _)| guarded("directoryURL", || panel.directoryURL()))
                    .flatten()
                    .and_then(|url| url.to_file_path())
            });
            folder.is_some_and(|folder| same_file(&folder, &exports))
        });
        if !shows_folder {
            note(format!(
                "the {save} does not report {} as its folder yet; pressing OK all the same",
                exports.display()
            ));
        }
        if press_ok() && wait_until(CLOSE, closed) {
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
                    "OK answers the chosen {} (got {out:?})",
                    exports.join(chosen).display()
                ),
            );
        } else {
            note(format!(
                "OK left the {save} up: its accept is NOT verified here"
            ));
            cancel(&mut checks, save);
            let _ = answer.recv_timeout(RESULT_WAIT);
        }
        main_window_key_again(&mut checks, save);

        step("after the panels");
        let after = on_main(|mtm| NSApplication::sharedApplication(mtm).activationPolicy());
        checks.check(
            after == policy,
            format!("the activation policy is as it was ({policy:?}, now {after:?})"),
        );
        drop(root);
        on_main(move |_| {
            checks.finish();
        });
    }
}
