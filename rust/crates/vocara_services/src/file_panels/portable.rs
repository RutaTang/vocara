//! Every target but macOS: rfd's dialogs, set up as the app set them up
//! before this module existed.

use std::path::PathBuf;

use super::{Panel, PanelFuture};

/// Applies `panel` to an rfd dialog of either kind (they share the
/// builder methods).
macro_rules! configure {
    ($dialog:expr, $panel:expr) => {{
        let panel: Panel = $panel;
        let mut dialog = $dialog;
        for filter in &panel.filters {
            dialog = dialog.add_filter(filter.name.clone(), &filter.extensions);
        }
        if let Some(directory) = &panel.directory {
            dialog = dialog.set_directory(directory);
        }
        if let Some(file_name) = panel.file_name {
            dialog = dialog.set_file_name(file_name);
        }
        if let Some(title) = panel.title {
            dialog = dialog.set_title(title);
        }
        if let Some(can) = panel.can_create_directories {
            dialog = dialog.set_can_create_directories(can);
        }
        dialog
    }};
}

pub(super) fn save_file(panel: Panel) -> Option<PathBuf> {
    configure!(rfd::FileDialog::new(), panel).save_file()
}

pub(super) fn pick(panel: Panel, multiple: bool) -> PanelFuture<Option<Vec<PathBuf>>> {
    let dialog = configure!(rfd::AsyncFileDialog::new(), panel);
    if multiple {
        let picking = dialog.pick_files();
        Box::pin(async move {
            picking
                .await
                .map(|files| files.iter().map(|f| f.path().to_path_buf()).collect())
        })
    } else {
        let picking = dialog.pick_file();
        Box::pin(async move { picking.await.map(|f| vec![f.path().to_path_buf()]) })
    }
}
