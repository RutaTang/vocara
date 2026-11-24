//! The platform's open and save panels: the part of `rfd` (the file-dialog
//! crate) the app used, with the same builder API, so a call site reads as
//! it did with rfd's `FileDialog` and `AsyncFileDialog`.
//!
//! # Why not rfd on macOS
//!
//! rfd 0.18's macOS backend filters save panels with `UTType` and
//! `setAllowedContentTypes:`, unguarded, and so links
//! `UniformTypeIdentifiers.framework` (through
//! `objc2-uniform-type-identifiers`), which exists only from macOS 11: a
//! binary that links it does not even load on 10.15, the Flutter app's
//! minimum. The macOS backend here (`macos.rs`) is built on
//! `NSOpenPanel` / `NSSavePanel` alone and filters with
//! `setAllowedFileTypes:` (extensions), as rfd's own open panels did:
//! deprecated since macOS 12, available since 10.3, still honoured. Every
//! other target keeps rfd (`portable.rs`), so the Linux build and its tests
//! work as before.
//!
//! # Threading: as rfd ran them on macOS
//!
//! - [`FileDialog::save_file`] blocks. It runs the panel on the main thread
//!   as an application-modal window (`runModal`): at once from the main
//!   thread, else the calling thread waits on a synchronous dispatch to the
//!   main queue (`dispatch2::run_on_main`, as rfd's `run_on_main`), so the
//!   main thread must be running the app's event loop, as iced's is whenever
//!   a worker asks. The app calls it from export workers.
//! - [`AsyncFileDialog::pick_file`] / [`AsyncFileDialog::pick_files`] set
//!   the panel up and start it at the call, on the main thread (dispatched
//!   there synchronously from any other), and return a future of what was
//!   picked. As rfd's `ModalFuture`: a sheet on the app's main window (else
//!   its first window) with `beginSheetModalForWindow:completionHandler:`,
//!   which returns at once, so iced's event loop keeps running and the
//!   completion handler, on the main thread, resolves the future; with no
//!   window or no running app, `runModal` on the main thread instead, and
//!   the future is ready when it returns. The app calls them from `update`.
//!
//! As rfd's, every macOS panel is set to the shielding window level, an
//! app whose activation policy is "prohibited" is made an accessory while
//! the panel is up, and the window that was key is made key again when the
//! panel closes.
//!
//! # Configuration
//!
//! What a call site sets is what rfd applied on macOS: the title is the
//! panel's `message` (macOS panels show no title bar), the directory its
//! `directoryURL`, the file name its `nameFieldStringValue`, the filters'
//! extensions together its allowed file types, `pick_files` its multiple
//! selection, `set_can_create_directories` its New Folder button (left at
//! AppKit's default, on, when not set). rfd's one macOS extra, the format
//! pop-up of a save panel given two or more filters, is not built: no
//! panel of the app's has one (the filters are merged, as for an open
//! panel).
//!
//! # Tests
//!
//! [`fake::FakeFilePanels`] stands in for the platform's panels while a test
//! holds it: it records each panel's [`Request`] and answers with what the
//! test queued (a cancel by default), so a test can check what a call site
//! asks for without a window.

pub mod fake;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod portable;
#[cfg(test)]
mod tests;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

#[cfg(target_os = "macos")]
use macos as backend;
#[cfg(not(target_os = "macos"))]
use portable as backend;

/// What a picking panel resolves to: what was picked, or `None` on cancel.
pub type PanelFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// A named group of file extensions (without the dot), as rfd's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    pub name: String,
    pub extensions: Vec<String>,
}

/// A panel's configuration: what the builder methods set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Panel {
    /// Shown as the panel's message on macOS.
    pub title: Option<String>,
    /// The directory it opens in.
    pub directory: Option<PathBuf>,
    /// The name it suggests (a save panel's name field).
    pub file_name: Option<String>,
    pub filters: Vec<Filter>,
    /// The New Folder button; `None` leaves the platform's default.
    pub can_create_directories: Option<bool>,
}

impl Panel {
    /// Every filter's extensions, in order: the allowed file types of a
    /// macOS panel. Empty when no filter names any (AppKit raises for an
    /// empty list, so none is set then).
    pub fn allowed_file_types(&self) -> Vec<String> {
        self.filters
            .iter()
            .flat_map(|filter| filter.extensions.iter().cloned())
            .collect()
    }

    fn add_filter(&mut self, name: impl Into<String>, extensions: &[impl ToString]) {
        self.filters.push(Filter {
            name: name.into(),
            extensions: extensions.iter().map(ToString::to_string).collect(),
        });
    }

    fn set_directory(&mut self, path: &Path) {
        // As rfd: an empty path is no directory.
        self.directory = if path.as_os_str().is_empty() {
            None
        } else {
            Some(path.to_path_buf())
        };
    }
}

/// What a panel was asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// [`FileDialog::save_file`].
    SaveFile,
    /// [`AsyncFileDialog::pick_file`].
    PickFile,
    /// [`AsyncFileDialog::pick_files`].
    PickFiles,
}

/// A panel as it was asked for: what [`fake::FakeFilePanels`] records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub kind: Kind,
    pub panel: Panel,
}

/// A file an [`AsyncFileDialog`] picked (rfd's `FileHandle`, path only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHandle(PathBuf);

impl FileHandle {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

/// The builder methods both dialogs share, as rfd's.
macro_rules! builder {
    ($dialog:ident) => {
        impl $dialog {
            pub fn new() -> Self {
                Self::default()
            }

            /// Adds a filter: a name and its extensions, without the dot.
            pub fn add_filter(
                mut self,
                name: impl Into<String>,
                extensions: &[impl ToString],
            ) -> Self {
                self.panel.add_filter(name, extensions);
                self
            }

            /// The directory the panel opens in (an empty path: none).
            pub fn set_directory(mut self, path: impl AsRef<Path>) -> Self {
                self.panel.set_directory(path.as_ref());
                self
            }

            /// The file name the panel suggests.
            pub fn set_file_name(mut self, file_name: impl Into<String>) -> Self {
                self.panel.file_name = Some(file_name.into());
                self
            }

            /// The panel's title (its message on macOS).
            pub fn set_title(mut self, title: impl Into<String>) -> Self {
                self.panel.title = Some(title.into());
                self
            }

            /// Whether the panel offers New Folder.
            pub fn set_can_create_directories(mut self, can: bool) -> Self {
                self.panel.can_create_directories = Some(can);
                self
            }

            /// What the panel is set up to show.
            pub fn panel(&self) -> &Panel {
                &self.panel
            }
        }
    };
}

/// A panel that blocks the calling thread until it closes (rfd's
/// `FileDialog`). See the module docs for where it runs.
#[derive(Debug, Clone, Default)]
pub struct FileDialog {
    panel: Panel,
}

builder!(FileDialog);

impl FileDialog {
    /// The save panel: the path to save to, or `None` on cancel. Blocks
    /// until the panel closes.
    pub fn save_file(self) -> Option<PathBuf> {
        if let Some(answer) = fake::answer(Kind::SaveFile, &self.panel) {
            return answer.and_then(|paths| paths.into_iter().next());
        }
        backend::save_file(self.panel)
    }
}

/// A panel that does not block: the call starts it, the future resolves
/// when it closes (rfd's `AsyncFileDialog`). See the module docs for where
/// it runs.
#[derive(Debug, Clone, Default)]
pub struct AsyncFileDialog {
    panel: Panel,
}

builder!(AsyncFileDialog);

impl AsyncFileDialog {
    /// The open panel for one file: the file, or `None` on cancel.
    pub fn pick_file(self) -> PanelFuture<Option<FileHandle>> {
        let picking = self.pick(Kind::PickFile);
        Box::pin(async move {
            picking
                .await
                .and_then(|paths| paths.into_iter().next())
                .map(FileHandle)
        })
    }

    /// The open panel for one or more files: the files, or `None` on
    /// cancel.
    pub fn pick_files(self) -> PanelFuture<Option<Vec<FileHandle>>> {
        let picking = self.pick(Kind::PickFiles);
        Box::pin(async move {
            picking
                .await
                .map(|paths| paths.into_iter().map(FileHandle).collect())
        })
    }

    /// Starts the panel now (a fake answers at once).
    fn pick(self, kind: Kind) -> PanelFuture<Option<Vec<PathBuf>>> {
        if let Some(answer) = fake::answer(kind, &self.panel) {
            return Box::pin(std::future::ready(answer));
        }
        backend::pick(self.panel, kind == Kind::PickFiles)
    }
}
