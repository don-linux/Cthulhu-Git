//! Renders `assets/icon.svg`, the single source of the application icon, and
//! records the source revision for the About page.
//!
//! - Every target: `OUT_DIR/icon-256.png`, embedded by `main.rs` as the
//!   window icon. Inside the window, `src/ui/icons.rs` draws the SVG itself.
//! - Windows targets: a multi-size `.ico` plus `VERSIONINFO`, compiled into the
//!   executable so Explorer, the taskbar and Task Manager show the icon and name.

#![expect(
    clippy::expect_used,
    reason = "a build script reports failure by panicking; cargo stops the build and shows the message"
)]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use resvg::{tiny_skia, usvg};

const ICON_SVG: &str = "assets/icon.svg";

/// The sizes Windows asks for across Explorer views, the taskbar and
/// high-DPI scaling.
const WINDOWS_ICON_SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];

/// Both values when the source tree is not a checkout git can read.
const UNKNOWN: &str = "unknown";

fn main() {
    println!("cargo::rerun-if-changed={ICON_SVG}");
    println!("cargo::rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let svg = fs::read(ICON_SVG).expect("read assets/icon.svg");
    let tree = usvg::Tree::from_data(&svg, &usvg::Options::default())
        .unwrap_or_else(|error| panic!("parse {ICON_SVG}: {error}"));

    let png = render(&tree, 256)
        .encode_png()
        .expect("encode the window icon as PNG");
    fs::write(out_dir.join("icon-256.png"), png).expect("write icon-256.png");

    embed_source_revision();

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_windows_resources(&tree, &out_dir);
    }
}

/// tiny-skia keeps premultiplied alpha; ICO files want straight RGBA.
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
    let file = fs::File::create(out_dir.join("cthulhu-git.ico")).expect("create cthulhu-git.ico");
    icon_dir.write(file).expect("write cthulhu-git.ico");

    // The script is the icon and VERSIONINFO only. A longPathAware manifest
    // in the same windres invocation is not optional in practice: if the
    // resource compiler rejects the manifest, the icon and version info are
    // dropped with it. The Windows release job requires both, so the manifest
    // stays out.
    let rc_path = out_dir.join("cthulhu-git.rc");
    fs::write(&rc_path, resource_script()).expect("write cthulhu-git.rc");
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

/// `embed_resource` puts `OUT_DIR` on the resource compiler's include path, so
/// the icon beside the script is found by its bare name.
fn resource_script() -> String {
    let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    let numeric = ["MAJOR", "MINOR", "PATCH"]
        .map(|part| env::var(format!("CARGO_PKG_VERSION_{part}")).expect("version part"))
        .join(",");
    format!(
        r#"1 ICON "cthulhu-git.ico"

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

/// Commit and commit date of the source tree, for the About page.
///
/// A build without a checkout (a packaged tarball) still compiles. Both values
/// are then `unknown`.
fn embed_source_revision() {
    let (commit, date) =
        source_revision().unwrap_or_else(|| (UNKNOWN.to_owned(), UNKNOWN.to_owned()));
    println!("cargo::rustc-env=CTHULHU_GIT_COMMIT={commit}");
    println!("cargo::rustc-env=CTHULHU_GIT_COMMIT_DATE={date}");
}

#[expect(
    clippy::disallowed_methods,
    reason = "a build script cannot use the crate it builds; this reads `git log -1` of the checkout"
)]
fn source_revision() -> Option<(String, String)> {
    // Without its own `.git`, git would describe whichever repository
    // contains this directory, such as a packaging tree.
    if !Path::new(".git").exists() {
        return None;
    }
    // HEAD changes on checkout; logs/HEAD on every commit, reset or checkout,
    // also when the branch ref is packed. In a linked worktree `.git` is a
    // file and neither path exists, so nothing is watched there.
    for watched in [".git/HEAD", ".git/logs/HEAD"] {
        if Path::new(watched).is_file() {
            println!("cargo::rerun-if-changed={watched}");
        }
    }
    // `--no-show-signature` keeps a `log.showSignature` setting from putting
    // gpg output in front of the format.
    let output = Command::new("git")
        .args(["log", "-1", "--no-show-signature", "--format=%H %cs"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let (commit, date) = text.trim().split_once(' ')?;
    Some((sanitize_commit(commit), sanitize_date(date)))
}

fn sanitize_commit(raw: &str) -> String {
    if raw.len() >= 7 && raw.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        raw.to_owned()
    } else {
        UNKNOWN.to_owned()
    }
}

/// `git log --format=%cs` prints `YYYY-MM-DD`.
fn sanitize_date(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let shaped = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit());
    if shaped {
        raw.to_owned()
    } else {
        UNKNOWN.to_owned()
    }
}
