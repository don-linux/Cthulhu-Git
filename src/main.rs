// Release builds on Windows must not open a console window next to the GUI.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ui;

use std::env;
use std::path::PathBuf;

use eframe::egui;

fn main() -> eframe::Result {
    // A folder given on the command line (or by the Linux desktop entry's
    // `%f`) opens instead of the last repository.
    let command_line = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .map(|path| std::path::absolute(&path).unwrap_or(path));

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

/// Without an explicit icon, eframe shows the egui logo in the title bar,
/// taskbar and Dock.
#[cfg(not(target_os = "macos"))]
fn window_icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!(concat!(env!("OUT_DIR"), "/icon-256.png")))
        .expect("icon-256.png rendered by build.rs")
}

/// On macOS an empty icon keeps eframe away from the Dock, so it shows
/// `AppIcon.icns` from the app bundle, drawn on Apple's icon grid.
#[cfg(target_os = "macos")]
fn window_icon() -> egui::IconData {
    egui::IconData::default()
}
