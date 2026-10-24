# Cthulhu Git

A small desktop Git client written in Rust with [egui/eframe](https://github.com/emilk/egui).
It uses the Git installed on the machine. Open a repository with the system
folder dialog or from the recent list to see its current branch, latest
commit and commit history.

Fetch and fast-forward pull are available. There is no commit, push, stage or diff yet.

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
| Top bar, left | Panel button that hides or shows the branches sidebar; the choice is remembered | |
| Top bar, centered | Repository name (name of the root folder; hover for the full path) | `cthulhu-demo` |
| Top bar, right | Panel button that hides or shows the latest-commit sidebar; the choice is remembered | |
| Left sidebar | Local branches with a checkbox each; checked branches are drawn in the graph | `main · 12 commits · origin/main · in sync` |
| Middle | Commit history as a table: **Branch**, **Graph**, **Commit Hash**, **Commit Message**, **Author** | `main │ ● │ 21de8d4a3b7c │ Rise from the sea │ (AV) Ada Voss` |
| Right sidebar | Latest commit: hash, author and co-authors with their avatars, date, message; the terminal beneath it | |
| Bottom bar, left | House and Settings buttons | |
| Bottom bar | Current branch, next to a branch icon, with Fetch and Pull | `main`, `main (no commits yet)` or `Detached HEAD at 21de8d4a3b7c` |
| Bottom bar, right | Terminal button. Showing the terminal also opens the right sidebar | |

**The history table.** Every column title starts exactly where that column's
content starts. Each column has its own width, saved as a share of the
history panel, and its own minimum; drag the divider at a column's right edge
to resize only that column, and double-click it to restore that column. When
the columns are wider than the panel, the table scrolls sideways. The Author
column shows the author's avatar, then up to two co-authors from
`Co-authored-by:` trailers stacked beside it (`+N` for more), then the names.
Resting the pointer on a commit opens a card like VS Code's Source Control
Graph hover, hung from the column under the pointer (it stays there while the
pointer moves into the card): avatar, author with a mail link when the address
is a plain `local@domain`, relative and exact date, co-authors, the full
message (`#123`, `GH-123` and `owner/repo#123` linked when the repository is
on GitHub), the branches on that commit, files changed with insertions and
deletions when git prints a short stat, the full hash with a Copy button, and
Open on GitHub when a GitHub remote was found.

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

It holds the theme, the last repository, the recent repositories, which
sidebars are hidden, the terminal font, the history column widths, whether
author avatars are downloaded, and whether a GitHub token sits in the system
keychain (never the token itself). It is rewritten atomically on every change.
How to add a setting: [docs/SETTINGS.md](docs/SETTINGS.md).

## Network and privacy

Git commands are the only thing that reaches your remotes. Besides them, the
app makes HTTPS requests for author avatars, and only while Settings → History
→ *Show author avatars* is on (the default):

| Request | Sent to | Carries |
| ------- | ------- | ------- |
| Avatar of `id+login@users.noreply.github.com`, or of `cursoragent@cursor.com` | `avatars.githubusercontent.com` | The id in the address, or Cursor Agent's known id |
| Avatar of an older `login@users.noreply.github.com` | `github.com` (`/{login}.png`), then its redirect | The login |
| Commit authors, for a repository on GitHub (`upstream`, then `origin`, then the first GitHub remote) | `api.github.com` | Owner, repository and a commit hash; the token, if one is set |
| Avatar of every address, after the above | `gravatar.com` | The SHA-256 of the trimmed, lowercased address, never the address |

Redirects stay on `api.github.com`, `avatars.githubusercontent.com`,
`github.com`, `gravatar.com`, `www.gravatar.com` and `secure.gravatar.com`,
and the token is sent only to `api.github.com`. Without a token, the GitHub
API allows 60 requests an hour; one listing answers up to 100 commits, and
pictures are cached for a week, so the limit is rarely reached. At the limit
the app waits until `x-ratelimit-reset` (a minute for a secondary limit
without `retry-after`, ten minutes when no time is given) and draws initials;
that wait is not remembered as "no avatar".

**GitHub Auth (optional).** Settings → GitHub Auth takes a read-only token for
private repositories and a limit of 5,000 requests an hour. The page links to
GitHub's fine-grained token form with `contents=read`, which GitHub fills in
as *Contents: read-only* plus *Metadata: read-only*. The token is stored in
Keychain (macOS), Credential Manager (Windows) or the Secret Service (Linux),
never in `settings.json`; when no keychain answers it is used for that
session only. It can be imported from GitHub CLI (`gh auth token`) or from
`GH_TOKEN` or `GITHUB_TOKEN`. The page also lists read-only features the same
token could enable later.

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
   - macOS: `/opt/homebrew/bin/git`, `/usr/local/bin/git`, then the Command
     Line Tools or Xcode `git` when that file exists. `/usr/bin/git` is the
     Xcode stub that opens the installer, so it is tried only after one of
     those developer copies is present.
   - Windows: `%ProgramFiles%\Git\cmd\git.exe`, `%ProgramFiles%\Git\bin\git.exe`,
     `%ProgramFiles(x86)%\Git\cmd\git.exe`,
     `%LOCALAPPDATA%\Programs\Git\cmd\git.exe`, `%USERPROFILE%\scoop\shims\git.exe`

**Running Git** (`exec.rs`). Every invocation goes through one helper:

- arguments are passed as argv, never through a shell;
- `--no-optional-locks`, `--no-pager`, `--no-replace-objects`,
  `core.fsmonitor=false`, and `GIT_TERMINAL_PROMPT=0` so Git does not wait
  for input or follow replace refs. Optional locks are skipped; a fast-forward
  pull still updates the index and working tree;
- `LC_ALL=C` so the messages the app matches on stay in English;
- every inherited `GIT_*` variable is removed, except `GIT_SSH`,
  `GIT_SSH_COMMAND`, `GIT_SSH_VARIANT`, and `GIT_ASKPASS`. A fixed list cannot
  cover `GIT_EXEC_PATH` or `GIT_CONFIG_KEY_<n>`. `SSH_AUTH_SOCK`, `HOME`, and
  `PATH` stay. Launching the app from a hook cannot redirect it to another
  repository, choose `core.sshCommand`, or run a planted `git-remote-*`;
- stdout and stderr are capped at 8 MiB. `git --version` is killed after
  250 ms, reads after 60 s, and fetch or pull after 3 minutes. On Linux and
  macOS the limit signals the whole process group, so a helper git spawned
  does not keep the pipe open;
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

**Reading the history** (`graph.rs`, `log.rs`). One
`git log --date-order -z --format=%H%x1f%P%x1f%aN%x1f%aE%x1f%an%x1f%ae%x1f%s%x1e%(trailers:only,unfold) --no-color --max-count=1001 <tips> --`
call gives the hash, parents, author after `.mailmap` (`%aN`, `%aE`) and as
recorded (`%an`, `%ae`), summary and trailer block of the latest commits of
the checked branches; asking for one more than the limit tells whether there
are more. Co-authors are the `Co-authored-by:` lines of the trailer block,
matched like VS Code does; one that names the author again (under either
identity) only adds an address to the author. The hover asks
`git show --no-color --shortstat --date=format-local:%Y-%m-%d %H:%M --format=%H%x1f%aN%x1f%aE%x1f%an%x1f%ae%x1f%at%x1f%ad%x1f%(trailers:only,unfold)%x1e%B%x1d <oid> --`
the first time a commit's card opens. An unborn branch skips the calls.

**GitHub and avatars** (`src/github/`). No GUI code: every request goes through
a `Transport`. Tests pass a fake one, and under `cfg!(test)` the window uses
`AvatarService::disabled()`, so `cargo test` does not touch the network. The
ignored `live_github` test does, and only with `CTHULHU_LIVE_GITHUB=1`. `remote.rs` picks the GitHub remote, `avatar_url.rs` derives URLs
that need no API call, `client.rs` reads the REST API, `resolver.rs` decides
which URLs to try for a person, and `token.rs` keeps the optional token.

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

The two cross checks need a C compiler for the target: `ring` (TLS for the
avatar requests, through `ureq` and `rustls`) builds C code in its build
script. For Windows, put `x86_64-w64-mingw32-gcc` on `PATH` (MinGW-w64); for
macOS, Apple clang with an SDK (osxcross). The host's gcc cannot stand in.
Without them, the Windows and macOS CI jobs, which build natively, are the
check for those targets.

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

The interface icons (`panel-left`, `house`, `folder`, `git-branch`, `github`, `info`, `key-round`, …) are unmodified
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

- No commit, push, stage, diff or file list. Fetch, fast-forward pull, and switching the current local branch are the writes. Switching uses `git switch --no-guess` on Git 2.23 and newer, so HEAD stays attached to that branch and a remote of the same name is not turned into a new local branch. Git 2.15 through 2.22 use `git checkout` after the local ref is confirmed, and a branch name that starts with `-` is refused. There is no `--force`: a dirty file that would be overwritten, or a branch checked out in another worktree, leaves the repository unchanged. The switch updates the work tree, so smudge filters and the `post-checkout` hook run, as they do in a terminal.
- The history stops at the latest 1000 commits. Dates and file stats are in
  the hover card, not in a column.
- A co-author is not looked up on the GitHub API by a commit of their own
  (the API names commit authors only). They still get a picture from a
  no-reply address, the Cursor Agent account, Gravatar, or an address the
  100-commit listing already mapped (authors and committers).
- GIF avatars are not decoded (only PNG and JPEG). They show initials, and
  that is remembered for a day.
- A merge commit's hover has no file stats (as in VS Code).
- One theme and no theme picker yet.
- A Git read that does not exit is killed after 60 seconds (`git --version`
  after 250 ms). Fetch and pull are killed after 3 minutes. The window stays
  responsive, but that command still fails.
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
