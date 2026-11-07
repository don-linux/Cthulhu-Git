// Release builds on Windows must not open a console window next to the GUI.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ui;

use std::env;
use std::path::PathBuf;

use eframe::egui;

fn main() -> eframe::Result {
    let first_argument = env::args_os().nth(1);
    // Finder passes `-psn_…`, and startup_folder treats every leading dash as
    // "not a folder". `--version` has to win over that and leave before
    // eframe, which would open a window.
    if version_flag(first_argument.as_deref()) {
        println!("{}", env!("CARGO_PKG_VERSION"));
        std::process::exit(0);
    }

    // A folder given on the command line (or by the Linux desktop entry's
    // `%f`) opens instead of the last repository. macOS Finder also passes
    // `-psn_…`, which is not a folder.
    let command_line = startup_folder(first_argument);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Cthulhu Git")
            .with_app_id("cthulhu-git")
            .with_icon(window_icon())
            .with_inner_size([860.0, 620.0])
            .with_min_inner_size([520.0, 400.0])
            .with_maximized(true),
        ..Default::default()
    };

    eframe::run_native(
        "Cthulhu Git",
        options,
        Box::new(move |cc| Ok(Box::new(ui::CthulhuApp::new(&cc.egui_ctx, command_line)))),
    )
}

fn version_flag(arg: Option<&std::ffi::OsStr>) -> bool {
    arg.is_some_and(|flag| flag == "--version" || flag == "-V")
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

/// On macOS an empty icon keeps eframe away from the Dock, so it shows
/// `AppIcon.icns` from the app bundle, drawn on Apple's icon grid.
#[cfg(target_os = "macos")]
fn window_icon() -> egui::IconData {
    egui::IconData::default()
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

    #[test]
    fn version_flag_is_exact_so_main_can_exit_before_the_leading_dash_rule() {
        use std::ffi::OsStr;

        assert!(version_flag(Some(OsStr::new("--version"))));
        assert!(version_flag(Some(OsStr::new("-V"))));
        assert!(!version_flag(Some(OsStr::new("--help"))));
        assert!(!version_flag(Some(OsStr::new("-v"))));
        assert!(!version_flag(Some(OsStr::new("-psn_0_12345"))));
        assert!(!version_flag(None));
        // A leading dash is still "not a folder". main checks version_flag first.
        assert!(startup_folder(Some("--version".into())).is_none());
        assert!(startup_folder(Some("-V".into())).is_none());
    }
}
