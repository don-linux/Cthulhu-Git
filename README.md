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

**Repository view.**

| Where | What | Example |
| ----- | ---- | ------- |
| Top bar, left | Panel button that hides or shows the commit history; the choice is remembered | |
| Top bar, centered | Repository name (name of the root folder; hover for the full path) | `cthulhu-demo` |
| Left sidebar | Commit history, one `hash - summary` line per commit, newest first; drag its edge to resize it | `21de8d4a3b7c - Rise from the sea` |
| Middle | Latest commit | `21de8d4a3b7c - Rise from the sea` |
| Bottom bar, left | House button to go back and pick another repository | |
| Bottom bar | Current branch, next to a branch icon | `main`, `main (no commits yet)` or `Detached HEAD at 21de8d4a3b7c` |

Hashes show 12 hex digits, the Linux kernel convention: short, yet unique
in practice even in very large histories. The history lists the latest 1000
commits and says so when there are more.

**Which screen opens on launch:**

1. A folder passed on the command line (the Linux desktop entry passes one
   with `%f`). An argument that starts with `-`, such as Finder's `-psn_…`,
   is ignored.
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

It holds the theme, the last repository, the recent repositories and whether
the commit history sidebar is hidden, and is
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
   reported as an error instead of silently falling back. A bare name such as
   `git` is run from the working directory (`./git`), not looked up on `PATH`.
   `--version` must answer within a quarter of a second or that candidate is
   skipped. A pre-release such as `2.15.0-rc0` does not meet the 2.15.0 floor;
   vendor suffixes (`.windows.1`, `.vfs`, Apple Git) do.
2. Every absolute directory in `PATH`, looking for `git` (`git.exe` on
   Windows; `git.cmd`/`git.bat` are never used because they need a shell).
   Relative and empty `PATH` entries are skipped so a repository cannot plant
   its own `git.exe` in the current directory. One pair of surrounding quotes
   is removed first, so a quoted absolute directory is still searched.
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
- `--no-optional-locks`, `--no-pager`, `--no-replace-objects`,
  `core.fsmonitor=false`, and `GIT_TERMINAL_PROMPT=0` so Git never touches the
  index, waits for input, or follows replace refs;
- `LC_ALL=C` so the messages the app matches on stay in English;
- `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_REPLACE_REF_BASE`,
  `GIT_SHALLOW_FILE`, `GIT_GRAFT_FILE`, `GIT_TRACE` and similar variables are
  removed, so launching the app from a hook or a shell that exported them
  cannot redirect it to another repository or rewrite the history it shows;
- stdout and stderr are capped at 8 MiB. `git --version` is killed after
  250 ms; any other invocation after 60 s;
- on Windows the process is created with `CREATE_NO_WINDOW` so no console
  flashes.

Git's `safe.directory` protection is respected: a repository owned by another
user produces an error that includes a shell-quoted
`git config --global --add safe.directory <root>` command to trust it.
"not a git repository" is matched only when it is the primary fatal, so a
path that merely contains those words is not mistyped. Git 2.15's capital
"Not a git repository" counts too.

**Reading the repository** (`repo.rs`). `git rev-parse --show-toplevel` gives
the root (normalized with `dunce`, which also removes Windows `\\?\` prefixes
and converts `C:/...` paths). Only the newline Git itself added is stripped,
so a directory whose name ends in CR or LF keeps that byte. A bare repository
uses `--absolute-git-dir` instead. The branch, detached HEAD or unborn branch
comes from `git symbolic-ref` and `git rev-parse`, not from `git status`:
status refreshes the index and would run clean and process filters planted in
the repository.

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
| Windows x86_64 | Tests run in CI; packaged by CI (`x86_64-pc-windows-msvc`). The window has not been run on a real machine yet |
| macOS Apple Silicon and Intel | Tests run in CI. Packaged as one universal app (`aarch64` + `x86_64`, macOS 11+). The window has not been run on a real machine yet |

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

Before a release that moves the Git layer, settings, startup or CI, run an
adversarial pass. The checklist and the file each pass owns are in
[docs/ADVERSARIAL-TESTING.md](docs/ADVERSARIAL-TESTING.md). The 0.0.2 pass is
written up in
[docs/adversarial-reports/v0.0.2.md](docs/adversarial-reports/v0.0.2.md).

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

The interface icons (`panel-left`, `house`, `git-branch`) are unmodified
[Lucide](https://lucide.dev) SVGs in [`assets/icons/`](assets/icons), under
the license in [`assets/icons/LICENSE`](assets/icons/LICENSE). `build.rs`
renders them white at 64 px and the app tints them with the theme's colors.
To add one, put the SVG there, then list it in `UI_ICONS` in `build.rs` and in
`Icon` in `src/ui/icons.rs`.

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
- A Git command that does not exit is killed after 60 seconds (`git --version`
  after 250 ms). The window stays responsive, but that open still fails.
- A deleted repository stays in the recent list until it is opened. The open
  then fails and the entry is not removed.
- Shortening a string at 12 bytes keeps the whole string when that cut would
  split a character. Git object ids are ASCII hex, so real commits are not
  affected.
- A repository whose path is not valid Unicode opens, but is not remembered
  (JSON strings must be UTF-8).
- No Windows installer: a portable zip. The Mac app is not signed or
  notarized by Apple.
- Linux packages are x86-64 only and need glibc 2.39 or newer. No Flatpak
  yet.

## License

[MIT](LICENSE) © 2026 Fernando Diaz / don-linux
