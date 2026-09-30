//! The system folder dialog, through `rfd`: File Explorer on Windows, Finder
//! (`NSOpenPanel`) on macOS, and the desktop's own dialog through the XDG
//! Desktop Portal on Linux.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use eframe::egui;

pub struct FolderPicker {
    receiver: Receiver<Option<PathBuf>>,
}

impl FolderPicker {
    /// The dialog is created here, on the UI thread, and awaited on a worker
    /// thread, as rfd recommends: macOS needs the main thread to create it,
    /// and waiting on the UI thread would freeze the window.
    pub fn open(ctx: &egui::Context, parent: &eframe::Frame, start: Option<&Path>) -> Self {
        let mut dialog = rfd::AsyncFileDialog::new()
            .set_title("Open repository")
            .set_parent(parent);
        if let Some(start) = start.filter(|start| start.is_dir()) {
            dialog = dialog.set_directory(start);
        }
        let choice = dialog.pick_folder();

        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let picked = pollster::block_on(choice).map(|handle| handle.path().to_path_buf());
            let _ = sender.send(picked);
            ctx.request_repaint();
        });
        Self { receiver }
    }

    /// `None` while the dialog is open, then `Some(folder)`, or `Some(None)`
    /// when the user cancelled.
    pub fn poll(&self) -> Option<Option<PathBuf>> {
        match self.receiver.try_recv() {
            Ok(picked) => Some(picked),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(None),
        }
    }
}
