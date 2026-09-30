// Release builds on Windows must not open a console window next to the GUI.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;

use std::env;
use std::path::PathBuf;

use eframe::egui;

fn main() -> eframe::Result {
    let initial = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .or_else(|| env::current_dir().ok())
        .unwrap_or_default();
    let initial = std::path::absolute(&initial).unwrap_or(initial);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Cthulhu Git")
            .with_app_id("cthulhu-git")
            .with_icon(window_icon())
            .with_inner_size([780.0, 400.0])
            .with_min_inner_size([460.0, 320.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Cthulhu Git",
        options,
        Box::new(move |cc| Ok(Box::new(app::CthulhuApp::new(&cc.egui_ctx, &initial)))),
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
