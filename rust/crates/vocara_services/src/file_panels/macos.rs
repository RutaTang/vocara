//! macOS: `NSOpenPanel` / `NSSavePanel`, run as rfd 0.18's macOS backend
//! ran them (see the parent module's docs), with no UniformTypeIdentifiers.
//!
//! Every AppKit call here is in the macOS 10.15 SDK, and only one is
//! deprecated: `setAllowedFileTypes:` (macOS 10.3; deprecated in 12 for
//! `setAllowedContentTypes:`, which needs `UTType`, macOS 11).
//! `beginSheetModalForWindow:completionHandler:` is 10.6,
//! `directoryURL` / `nameFieldStringValue` 10.6, `runModal` 10.0,
//! `activationPolicy` 10.6 and `CGShieldingWindowLevel` 10.0.

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::Once;

use block2::RcBlock;
use objc2::rc::{autoreleasepool, Retained};
use objc2::{MainThreadMarker, Message};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSModalResponse, NSModalResponseOK, NSOpenPanel,
    NSSavePanel, NSWindow, NSWindowLevel,
};
use objc2_foundation::{NSArray, NSString, NSThread, NSURL};
use tokio::sync::oneshot;

use super::{Panel, PanelFuture};

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    /// `CGWindowLevel CGShieldingWindowLevel(void)` (CGDirectDisplay.h;
    /// `CGWindowLevel` is `int32_t`): the level of the window that shields
    /// the display, which rfd put its panels at.
    fn CGShieldingWindowLevel() -> i32;
}

/// The save panel, modal on the main thread; blocks the caller until it
/// closes.
pub(super) fn save_file(config: Panel) -> Option<PathBuf> {
    dispatch2::run_on_main(move |mtm| {
        autoreleasepool(|_| {
            let shown = Shown::save(mtm, &config);
            let response = shown.native.panel().runModal();
            shown.picked(response)?.into_iter().next()
        })
    })
}

/// Starts the open panel on the main thread now; the future resolves when
/// it closes.
pub(super) fn pick(config: Panel, multiple: bool) -> PanelFuture<Option<Vec<PathBuf>>> {
    let (sender, picked) = oneshot::channel();
    dispatch2::run_on_main(move |mtm| {
        autoreleasepool(|_| begin(mtm, &config, multiple, sender));
    });
    // A completion handler dropped without being called (the app ending
    // with the sheet up) is a cancel.
    Box::pin(async move { picked.await.ok().flatten() })
}

/// rfd's `ModalFuture::new`, on the main thread: a sheet on the main (else
/// first) window while the app runs, `runModal` otherwise.
fn begin(
    mtm: MainThreadMarker,
    config: &Panel,
    multiple: bool,
    sender: oneshot::Sender<Option<Vec<PathBuf>>>,
) {
    enter_multithreaded_mode();
    let app = NSApplication::sharedApplication(mtm);
    let window = app.mainWindow().or_else(|| app.windows().firstObject());
    let shown = Shown::open(mtm, config, multiple);
    let panel: Retained<NSSavePanel> = shown.native.panel().retain();
    match window {
        Some(window) if app.isRunning() => {
            // The handler holds the panel (and what it restores) until it
            // runs, then lets go of it: AppKit holds the handler while the
            // sheet is up, so holding them past that would be a cycle.
            let pending = Cell::new(Some((shown, sender)));
            let done = RcBlock::new(move |response: NSModalResponse| {
                if let Some((shown, sender)) = pending.take() {
                    let _ = sender.send(shown.picked(response));
                }
            });
            panel.beginSheetModalForWindow_completionHandler(&window, &done);
        }
        _ => {
            let response = panel.runModal();
            let _ = sender.send(shown.picked(response));
        }
    }
}

/// rfd started an `NSThread` before each non-blocking panel: once one has
/// run, Cocoa knows the process is multithreaded (it stays so).
fn enter_multithreaded_mode() {
    static STARTED: Once = Once::new();
    STARTED.call_once(|| NSThread::new().start());
}

/// The AppKit panel.
enum Native {
    Save(Retained<NSSavePanel>),
    Open(Retained<NSOpenPanel>),
}

impl Native {
    fn panel(&self) -> &NSSavePanel {
        match self {
            Native::Save(panel) => panel,
            Native::Open(panel) => panel,
        }
    }
}

/// A panel set up to show, and what rfd's `Panel` put back when it was
/// dropped: the activation policy it changed, the window that was key.
struct Shown {
    native: Native,
    multiple: bool,
    app: Retained<NSApplication>,
    key_window: Option<Retained<NSWindow>>,
    changed_policy: Option<NSApplicationActivationPolicy>,
}

impl Shown {
    /// rfd's `Panel::build_save_file` (a single list of types, see the
    /// parent module's docs).
    fn save(mtm: MainThreadMarker, config: &Panel) -> Self {
        let panel = NSSavePanel::savePanel(mtm);
        configure(&panel, config);
        Self::new(mtm, Native::Save(panel), false)
    }

    /// rfd's `Panel::build_pick_file` / `build_pick_files`.
    fn open(mtm: MainThreadMarker, config: &Panel, multiple: bool) -> Self {
        let panel = NSOpenPanel::openPanel(mtm);
        configure(&panel, config);
        panel.setCanChooseDirectories(false);
        panel.setCanChooseFiles(true);
        if multiple {
            panel.setAllowsMultipleSelection(true);
        }
        Self::new(mtm, Native::Open(panel), multiple)
    }

    /// rfd's `Panel::new`, with its `PolicyManager` and `FocusManager`.
    fn new(mtm: MainThreadMarker, native: Native, multiple: bool) -> Self {
        let app = NSApplication::sharedApplication(mtm);
        let policy = app.activationPolicy();
        let changed_policy = (policy == NSApplicationActivationPolicy::Prohibited).then(|| {
            app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
            policy
        });
        let key_window = app.keyWindow();
        // SAFETY: a CoreGraphics function with no arguments and no
        // preconditions, in the framework the attribute links.
        let level = unsafe { CGShieldingWindowLevel() };
        native.panel().setLevel(level as NSWindowLevel);
        Shown {
            native,
            multiple,
            app,
            key_window,
            changed_policy,
        }
    }

    /// What the panel closed with: the chosen paths on OK (an open panel
    /// with multiple selection: all of them), `None` on cancel.
    fn picked(&self, response: NSModalResponse) -> Option<Vec<PathBuf>> {
        if response != NSModalResponseOK {
            return None;
        }
        match &self.native {
            Native::Open(panel) if self.multiple => Some(
                panel
                    .URLs()
                    .to_vec()
                    .iter()
                    .filter_map(|url| path_of(url))
                    .collect(),
            ),
            native => native
                .panel()
                .URL()
                .and_then(|url| path_of(&url))
                .map(|path| vec![path]),
        }
    }
}

impl Drop for Shown {
    fn drop(&mut self) {
        if let Some(window) = &self.key_window {
            window.makeKeyAndOrderFront(None);
        }
        if let Some(policy) = self.changed_policy {
            self.app.setActivationPolicy(policy);
        }
    }
}

/// Sets `panel` up as `config` says, as rfd's `PanelExt` did.
fn configure(panel: &NSSavePanel, config: &Panel) {
    let types: Vec<Retained<NSString>> = config
        .allowed_file_types()
        .iter()
        .map(|ext| NSString::from_str(ext))
        .collect();
    if !types.is_empty() {
        // Extensions, not `UTType`s: see the module docs.
        #[allow(deprecated)]
        panel.setAllowedFileTypes(Some(&NSArray::from_retained_slice(&types)));
    }
    if let Some(directory) = config.directory.as_ref().and_then(|d| d.to_str()) {
        let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(directory), true);
        panel.setDirectoryURL(Some(&url));
    }
    if let Some(file_name) = &config.file_name {
        panel.setNameFieldStringValue(&NSString::from_str(file_name));
    }
    if let Some(title) = &config.title {
        panel.setMessage(Some(&NSString::from_str(title)));
    }
    if let Some(can) = config.can_create_directories {
        panel.setCanCreateDirectories(can);
    }
}

/// A file URL's path (rfd's `url.path()`).
fn path_of(url: &NSURL) -> Option<PathBuf> {
    url.path().map(|path| PathBuf::from(path.to_string()))
}
