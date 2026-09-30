# Cthulhu Git

A small desktop Git client written in Rust with [egui/eframe](https://github.com/emilk/egui).
It uses the Git installed on the machine. Open a repository with the system
folder dialog or from the recent list to see its current branch, latest
commit and commit history.

It is read-only: no commit, push, stage or diff yet.

## Requirements

- [rustup](https://rustup.rs). The toolchain is pinned to Rust **1.98.1** in
  `rust-toolchain.toml`, so the first `cargo` command installs it automatically.
- Git **2.15 or newer** on the machine (the app never bundles Git).
- Linux only: the usual libraries for an OpenGL window on X11 or Wayland.
  On Debian/Ubuntu:

  ```bash
  sudo apt install libxkbcommon-x11-0 libgl1 libegl1 libxcursor1 libxrandr2 libxi6 libx11-xcb1 libgl1-mesa-dri
  ```

- Linux only, for the folder dialog: an XDG Desktop Portal backend
  (`xdg-desktop-portal-gtk`, `-gnome` or `-kde`, installed by every mainstream
  desktop) or, as a fallback, `zenity`.

## Build and run

```bash
cargo run                            # last repository, or the home screen
cargo run -- /path/to/a/repository   # open this folder instead
cargo build --release                # binary in target/release/cthulhu-git
```

To use a specific Git executable instead of searching for one:

```bash
CTHULHU_GIT=/opt/git/bin/git cargo run -- ~/src/project
```

## What the window shows

**Home screen.** An **Open repository…** button that opens the system folder
dialog (File Explorer on Windows, Finder on macOS, the desktop's own dialog
through the XDG Desktop Portal on Linux), and the ten most recent
repositories. Any folder inside a repository works; its root is what gets
remembered.

**Repository view.** A **Home** button to go back and pick another repository,
then:

| Field | Example |
| ----- | ------- |
| Repository | `cthulhu-demo` (name of the root folder) |
| Branch | `main`, `main (no commits yet)` or `Detached HEAD at 21de8d4a3b7c` |
| Latest commit | `21de8d4a3b7c - Rise from the sea` |
| Commit history | Collapsed until clicked; one `hash - summary` line per commit, newest first |

Hashes show 12 hex digits, the Linux kernel convention: short, yet unique
in practice even in very large histories. The history lists the latest 1000
commits and says so when there are more.

**Which screen opens on launch:**

1. A folder passed on the command line (the Linux desktop entry passes one
   with `%f`).
2. Otherwise, the last repository opened. Going Home does not forget it.
3. Otherwise (first launch, or the last repository was moved or deleted),
   the home screen. A failed reopen is shown as an error and forgotten.

Errors (Git not found, too old, not a folder, not a repository, repository
owned by another user, unreadable settings) appear in a red panel.

## Settings

Stored as JSON in the per-user config folder of each OS, resolved with the
[`directories`](https://crates.io/crates/directories) crate:

| OS | File |
| -- | ---- |
| Linux | `$XDG_CONFIG_HOME/cthulhu-git/settings.json` (default `~/.config/cthulhu-git/settings.json`) |
| macOS | `~/Library/Application Support/io.github.don-linux.cthulhu-git/settings.json` |
| Windows | `%APPDATA%\don-linux\cthulhu-git\config\settings.json` |

It holds the theme, the last repository and the recent repositories, and is
rewritten atomically on every change. How to add a setting:
[docs/SETTINGS.md](docs/SETTINGS.md).

## Themes

Colors come from a theme: a palette of named roles (`text`, `text_muted`,
`accent`, `hash`…) that is turned into egui's visuals in one place. Views
never use literal colors. One dark theme ships; there is no theme picker yet.
How to add a theme: [docs/THEMES.md](docs/THEMES.md).

## How the Git layer works

The code lives in `src/git/` and has no GUI dependency, so it is tested
headless.

**Finding Git** (`discover.rs`). The first candidate that exists, is
executable and answers `git --version` with 2.15+ wins. A candidate that fails
does not stop the search.

1. `CTHULHU_GIT`, if set. If it does not point to an executable file, that is
   reported as an error instead of silently falling back.
2. Every absolute directory in `PATH`, looking for `git` (`git.exe` on
   Windows; `git.cmd`/`git.bat` are never used because they need a shell).
   Relative and empty `PATH` entries are skipped so a repository cannot plant
   its own `git.exe` in the current directory.
3. Known install locations, for GUI apps that start with a minimal `PATH`:
   - Linux: `/usr/bin/git`, `/usr/local/bin/git`
   - macOS: `/opt/homebrew/bin/git`, `/usr/local/bin/git`, `/usr/bin/git`
     (the Xcode shim; without the Command Line Tools it fails `--version`
     and is skipped)
   - Windows: `%ProgramFiles%\Git\cmd\git.exe`, `%ProgramFiles%\Git\bin\git.exe`,
     `%ProgramFiles(x86)%\Git\cmd\git.exe`,
     `%LOCALAPPDATA%\Programs\Git\cmd\git.exe`, `%USERPROFILE%\scoop\shims\git.exe`

**Running Git** (`exec.rs`). Every invocation goes through one helper:

- arguments are passed as argv, never through a shell;
- `--no-optional-locks`, `--no-pager`, `core.fsmonitor=false`, and
  `GIT_TERMINAL_PROMPT=0` so Git never touches the index or waits for input;
- `LC_ALL=C` so the messages the app matches on stay in English;
- `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE` and similar variables are
  removed, so launching the app from a hook or a shell that exported them
  cannot redirect it to another repository;
- on Windows the process is created with `CREATE_NO_WINDOW` so no console
  flashes.

Git's `safe.directory` protection is respected: a repository owned by another
user produces an error that includes the exact
`git config --global --add safe.directory <root>` command to trust it.

**Reading the repository** (`repo.rs`). `git rev-parse --show-toplevel` gives
the root (normalized with `dunce`, which also removes Windows `\\?\` prefixes
and converts `C:/...` paths), and one
`git status --porcelain=v2 --branch -z --untracked-files=no` call gives the
branch, detached HEAD or unborn branch.

**Reading the history** (`log.rs`). One
`git log --max-count=1001 -z --format=%H%x1f%s HEAD --` call gives the full
hash and summary of the latest commits; asking for one more than the limit
tells whether there are more. An unborn branch skips the call.

In the UI, opening a repository and the folder dialog both run on worker
threads and report back over a channel, so the window never freezes.

## Platform support

Linux, macOS and Windows are supported by design: the platform-specific code
(fallback paths, execute-bit check, `CREATE_NO_WINDOW`, path normalization)
is behind `cfg` and compiles for all three targets.

| Platform | Status |
| -------- | ------ |
| Linux x86_64 | Built, tested and run; packaged by CI on Ubuntu 24.04 as AppImage, `.deb`, `.rpm` and `.tar.gz` (glibc 2.39+: Ubuntu 24.04, Debian 13, Fedora 40 or later) |
| Windows x86_64 | Built and packaged by CI (`x86_64-pc-windows-msvc`); not run on a real machine yet |
| macOS Apple Silicon and Intel | Built by CI as one universal app (`aarch64` + `x86_64`, macOS 11+); not run on a real machine yet |

The Windows search logic (`git.exe` plus Git for Windows fallbacks) is also
covered by a simulated test that runs on Linux.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
rustup target add x86_64-pc-windows-gnu x86_64-apple-darwin
cargo check --target x86_64-pc-windows-gnu --all-targets
cargo check --target x86_64-apple-darwin --all-targets
```

The crate forbids `unsafe` code. Tests never modify the process environment
(discovery takes its inputs as a `DiscoverInputs` value), so they run in
parallel without locks. The test that proves an inherited `GIT_DIR` is ignored
re-runs the test binary as a child process with that variable set.

## Icon

Every icon comes from one file, [`assets/icon.svg`](assets/icon.svg), rendered
with [resvg](https://github.com/linebender/resvg) at each size a platform
asks for, so nothing is scaled from a bitmap:

- **Window icon** (title bar and taskbar on Linux and Windows): `build.rs`
  renders a 256 px PNG that is embedded in the binary.
- **Windows `.exe`** (Explorer, Start menu, taskbar): `build.rs` builds an
  `.ico` with 16, 20, 24, 32, 40, 48, 64 and 256 px images and embeds it,
  together with the version information shown in the file's properties, with
  [`embed-resource`](https://crates.io/crates/embed-resource).
- **macOS** (Dock, Finder): CI renders
  [`assets/icon-macos.svg`](assets/icon-macos.svg), which places the same
  drawing on Apple's icon grid, into `AppIcon.icns` inside the app bundle.
- **Linux** (application menus): CI renders PNGs from 16 to 512 px into the
  `hicolor` theme, used by the `.deb`, `.rpm`, AppImage and archive together
  with [`packaging/linux/cthulhu-git.desktop`](packaging/linux/cthulhu-git.desktop).
  PNGs are used instead of the SVG because some desktops draw SVG icons
  without the filters the drawing relies on.

To change the icon, edit `assets/icon.svg` and rebuild.

## Releases

Downloads for macOS (universal), Windows x86-64 and Linux x86-64 (AppImage,
`.deb`, `.rpm` and `.tar.gz`) are on the
[releases page](https://github.com/don-linux/Cthulhu-Git/releases).

Only a `v*` tag publishes a release: every pull request to `main` is checked
and built for the three platforms by `.github/workflows/ci.yml`, and pushing a
tag such as `v0.0.1` runs `.github/workflows/release.yml`, which builds the
same packages and publishes them with the notes in `docs/release-notes/`. How
to write those notes and cut a release is in
[docs/HOW-TO-CHANGELOG.md](docs/HOW-TO-CHANGELOG.md).

## Limits of this version

- Read-only: no commit, push, stage, diff or file list.
- The history shows commit summaries only (no author, date or graph) and
  stops at the latest 1000 commits.
- One theme and no settings screen yet.
- No timeout for a Git process that hangs (for example on a stalled network
  filesystem); the window stays responsive but keeps showing "Opening…".
- A repository whose path is not valid Unicode opens, but is not remembered
  (JSON strings must be UTF-8).
- No Windows installer: a portable zip. The Mac app is not signed or
  notarized by Apple.
- Linux packages are x86-64 only and need glibc 2.39 or newer. No Flatpak
  yet.

## License

[MIT](LICENSE) © 2026 Fernando Diaz / don-linux
