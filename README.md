# Cthulhu Git

A small desktop Git client written in Rust with [egui/eframe](https://github.com/emilk/egui).
This first slice finds the Git installed on the machine, then shows the
repository name, its root folder and the current branch for any folder you
point it at.

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

## Build and run

```bash
cargo run -- /path/to/a/repository   # open a specific folder
cargo run                            # use the current directory
cargo build --release                # binary in target/release/cthulhu-git
```

In the window, edit the path and press **Enter** or **Refresh**, or click
**Use current directory**. Any folder inside a repository works; the root is
resolved automatically.

To use a specific Git executable instead of searching for one:

```bash
CTHULHU_GIT=/opt/git/bin/git cargo run -- ~/src/project
```

## What the window shows

| Field      | Example                                            |
| ---------- | -------------------------------------------------- |
| Repository | `cthulhu-demo` (name of the root folder)           |
| Branch     | `main`, `main (no commits yet)` or `Detached HEAD at 21de8d4` |
| Root       | `/tmp/cthulhu-demo`                                |
| Git        | `/usr/bin/git (2.43.0)`                            |

Errors (Git not found, too old, not a folder, not a repository, repository
owned by another user) appear in a red panel and the previous data is cleared.

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

In the UI, each refresh runs on a worker thread and reports back over a
channel, so the window never freezes while Git runs.

## Platform support

Linux, macOS and Windows are supported by design: the platform-specific code
(fallback paths, execute-bit check, `CREATE_NO_WINDOW`, path normalization)
is behind `cfg` and compiles for all three targets.

| Platform | Status |
| -------- | ------ |
| Linux x86_64 | Built, tested and run; screenshots and video captured |
| Windows x86_64 | `cargo check` and `cargo clippy` pass for `x86_64-pc-windows-gnu`; not run on a real machine yet |
| macOS x86_64 | `cargo check` and `cargo clippy` pass for `x86_64-apple-darwin`; not run on a real machine yet |

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

## Limits of this version

- Read-only: no commit, push, stage, diff, history or file list.
- No native folder picker; the path is typed or pasted.
- No timeout for a Git process that hangs (for example on a stalled network
  filesystem); the window stays responsive but keeps showing "Loading…".
- No packaging or installers.

## License

[MIT](LICENSE) © 2026 Fernando Diaz / don-linux
