// Release builds on Windows must not open a console window next to the GUI.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ui;

use std::env;
use std::path::PathBuf;

use eframe::egui;

fn main() -> eframe::Result {
    // A folder given on the command line (or by the Linux desktop entry's
    // `%f`) opens instead of the last repository. macOS Finder also passes
    // `-psn_…`, which is not a folder.
    let command_line = startup_folder(env::args_os().nth(1));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Cthulhu Git")
            .with_app_id("cthulhu-git")
            .with_icon(window_icon())
            .with_inner_size([860.0, 620.0])
            .with_min_inner_size([520.0, 400.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Cthulhu Git",
        options,
        Box::new(move |cc| Ok(Box::new(ui::CthulhuApp::new(&cc.egui_ctx, command_line)))),
    )
}

fn startup_folder(arg: Option<std::ffi::OsString>) -> Option<PathBuf> {
    let arg = arg?;
    if arg.to_string_lossy().starts_with('-') {
        return None;
    }
    let path = PathBuf::from(arg);
    Some(std::path::absolute(&path).unwrap_or(path))
}

/// Without an explicit icon, eframe shows the egui logo in the title bar,
/// taskbar and Dock.
#[cfg(not(target_os = "macos"))]
fn window_icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!(concat!(env!("OUT_DIR"), "/icon-256.png")))
        .expect("icon-256.png rendered by build.rs")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finder_process_serial_number_is_not_a_repository_path() {
        assert!(startup_folder(None).is_none());
        assert!(startup_folder(Some("-psn_0_12345".into())).is_none());
        assert!(startup_folder(Some("--help".into())).is_none());
    }
}

/// On macOS an empty icon keeps eframe away from the Dock, so it shows
/// `AppIcon.icns` from the app bundle, drawn on Apple's icon grid.
#[cfg(target_os = "macos")]
fn window_icon() -> egui::IconData {
    egui::IconData::default()
}
