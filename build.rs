//! Renders the SVGs under `assets/`.
//!
//! - Every target: `OUT_DIR/icon-256.png` from `assets/icon.svg`, the single
//!   source of the application icon, embedded by `main.rs` as the window icon.
//! - Every target: `OUT_DIR/icons/<name>.rgba` for each Lucide interface icon
//!   in `assets/icons/`, embedded by `src/ui/icons.rs`.
//! - Windows targets: a multi-size `.ico` plus `VERSIONINFO`, compiled into the
//!   executable so Explorer, the taskbar and Task Manager show the icon and name.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use resvg::{tiny_skia, usvg};

const ICON_SVG: &str = "assets/icon.svg";

/// The sizes Windows asks for across Explorer views, the taskbar and
/// high-DPI scaling.
const WINDOWS_ICON_SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];

const UI_ICONS_DIR: &str = "assets/icons";

/// Interface icons, by file name without `.svg`. Must match `src/ui/icons.rs`.
const UI_ICONS: [&str; 9] = [
    "panel-left",
    "panel-right",
    "house",
    "git-branch",
    "terminal",
    "settings",
    "arrow-left",
    "cloud-download",
    "arrow-down-to-line",
];

/// Side of the rendered interface icons. Drawn at about 18 points, this stays
/// sharp up to 3x display scaling.
const UI_ICON_SIZE: u32 = 64;

fn main() {
    println!("cargo::rerun-if-changed={ICON_SVG}");
    println!("cargo::rerun-if-changed={UI_ICONS_DIR}");
    println!("cargo::rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let tree = load_svg(ICON_SVG, fs::read(ICON_SVG).expect("read assets/icon.svg"));

    let png = render(&tree, 256)
        .encode_png()
        .expect("encode the window icon as PNG");
    fs::write(out_dir.join("icon-256.png"), png).expect("write icon-256.png");

    render_ui_icons(&out_dir);

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_windows_resources(&tree, &out_dir);
    }
}

fn load_svg(path: &str, data: Vec<u8>) -> usvg::Tree {
    usvg::Tree::from_data(&data, &usvg::Options::default())
        .unwrap_or_else(|error| panic!("parse {path}: {error}"))
}

/// Lucide strokes with `currentColor`, which resvg draws black. Rendered
/// white instead, the app tints each icon with a palette color.
fn render_ui_icons(out_dir: &Path) {
    let icons_dir = out_dir.join("icons");
    fs::create_dir_all(&icons_dir).expect("create OUT_DIR/icons");
    for name in UI_ICONS {
        let path = format!("{UI_ICONS_DIR}/{name}.svg");
        let svg = fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path}: {error}"));
        let tree = load_svg(&path, svg.replace("currentColor", "#ffffff").into_bytes());
        let rgba = straight_rgba(&render(&tree, UI_ICON_SIZE));
        fs::write(icons_dir.join(format!("{name}.rgba")), rgba)
            .unwrap_or_else(|error| panic!("write the {name} icon: {error}"));
    }
}

/// tiny-skia keeps premultiplied alpha; ICO files and egui textures want
/// straight RGBA.
fn straight_rgba(pixmap: &tiny_skia::Pixmap) -> Vec<u8> {
    pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let color = pixel.demultiply();
            [color.red(), color.green(), color.blue(), color.alpha()]
        })
        .collect()
}

fn render(tree: &usvg::Tree, size: u32) -> tiny_skia::Pixmap {
    let mut pixmap = tiny_skia::Pixmap::new(size, size).expect("non-zero icon size");
    let scale = size as f32 / tree.size().width();
    resvg::render(
        tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap
}

fn embed_windows_resources(tree: &usvg::Tree, out_dir: &Path) {
    let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);
    for size in WINDOWS_ICON_SIZES {
        let rgba = straight_rgba(&render(tree, size));
        let image = ico::IconImage::from_rgba_data(size, size, rgba);
        // 32-bit BMP for the small sizes (read by every Windows component),
        // PNG for 256 px as Microsoft recommends.
        let entry = if size >= 256 {
            ico::IconDirEntry::encode_as_png(&image)
        } else {
            ico::IconDirEntry::encode_as_bmp(&image)
        }
        .expect("encode icon image");
        icon_dir.add_entry(entry);
    }
    let ico_path = out_dir.join("cthulhu-git.ico");
    let file = fs::File::create(&ico_path).expect("create cthulhu-git.ico");
    icon_dir.write(file).expect("write cthulhu-git.ico");

    let rc_path = out_dir.join("cthulhu-git.rc");
    fs::write(&rc_path, resource_script(&ico_path)).expect("write cthulhu-git.rc");
    // A missing resource compiler only warns, so `cargo check --target
    // x86_64-pc-windows-gnu` keeps working on Linux without MinGW; the Windows
    // CI job verifies the icon is really in the release executable.
    match embed_resource::compile(&rc_path, embed_resource::NONE) {
        embed_resource::CompilationResult::NotAttempted(why) => {
            println!("cargo::warning=Windows icon and version info not embedded: {why}");
        }
        result => result
            .manifest_optional()
            .expect("compile Windows resources"),
    }
}

fn resource_script(ico_path: &Path) -> String {
    let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    let numeric = ["MAJOR", "MINOR", "PATCH"]
        .map(|part| env::var(format!("CARGO_PKG_VERSION_{part}")).expect("version part"))
        .join(",");
    let ico_path = ico_path.display().to_string().replace('\\', "\\\\");
    format!(
        r#"1 ICON "{ico_path}"

1 VERSIONINFO
FILEVERSION {numeric},0
PRODUCTVERSION {numeric},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "don-linux"
      VALUE "FileDescription", "Cthulhu Git"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "cthulhu-git"
      VALUE "LegalCopyright", "Copyright (C) 2026 Fernando Diaz / don-linux. MIT License."
      VALUE "OriginalFilename", "cthulhu-git.exe"
      VALUE "ProductName", "Cthulhu Git"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    )
}
