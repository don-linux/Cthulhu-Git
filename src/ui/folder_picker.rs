//! The system folder dialog, through `rfd`: File Explorer on Windows, Finder
//! (`NSOpenPanel`) on macOS, and the desktop's own dialog through the XDG
//! Desktop Portal on Linux.

use std::path::{Path, PathBuf};

use eframe::egui;

use super::task::Task;

/// The chosen folder, or `None` when the user cancelled.
///
/// The dialog is created here, on the UI thread, and awaited on a worker
/// thread, as rfd recommends: macOS needs the main thread to create it, and
/// waiting on the UI thread would freeze the window.
pub fn open(
    ctx: &egui::Context,
    parent: &eframe::Frame,
    start: Option<&Path>,
) -> Task<Option<PathBuf>> {
    let mut dialog = rfd::AsyncFileDialog::new()
        .set_title("Open repository")
        .set_parent(parent);
    if let Some(start) = start.filter(|start| start.is_dir()) {
        dialog = dialog.set_directory(start);
    }
    let choice = dialog.pick_folder();
    Task::spawn(ctx, move || {
        pollster::block_on(choice).map(|handle| handle.path().to_path_buf())
    })
}
