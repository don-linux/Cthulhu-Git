# Settings

Map of the settings logic. Update this file when you change it.

## How it works

```text
startup   Startup::system: Settings::default_path() -> load_settings -> Settings::load_from(path)
          missing file -> defaults; unreadable/invalid -> defaults + error banner,
          and nothing is saved that session, so the file is left as it is
          no home folder -> defaults + error banner, nothing is saved
decide    command-line folder > settings.last_repository > home screen
change    mutate CthulhuApp.settings -> CthulhuApp::save_settings() -> Settings::save_to(save_path)
token     github_token_in_keyring -> GitHubAuth::load reads the keychain on a worker thread
columns   history_columns is read every frame; a divider release saves it
font      ui_font and terminal_font are ids of faces compiled into the app
          a missing or unknown ui_font uses Ubuntu
          a missing or unknown terminal_font uses Meslo LGS NF
          terminal_font "builtin" keeps the monospace shipped with the app
          the Terminal page shows the prompt block (up to 8 lines) of a 32-row preview shell and ignores input; the preview is not saved
          catalog and licenses: docs/FONTS.md
```

Saved right away on every change (no timer, no save-on-exit). The write is
atomic (`write_atomic`): a temp file with a fresh random name is created next
to `settings.json`, synced to disk, and renamed over it (`std::fs::rename`
replaces the file on Linux, macOS and Windows, and replaces a symlink instead
of writing through it). On Unix the file is private to the user (mode 0600).

## Where things are

| File | Symbol | What it controls |
| ---- | ------ | ---------------- |
| `src/settings.rs` | `Settings` | The saved fields (serde, `#[serde(default)]`) |
| `src/settings.rs` | `Settings::default_path`, `project_dirs` | Per-OS location; `project_dirs` is the app's `directories::ProjectDirs` (config and cache folders) |
| `src/settings.rs` | `Settings::load_from` | Reads the file; a missing one gives the defaults |
| `src/settings.rs` | `Settings::from_json` | Parses the bytes: skips a UTF-8 BOM, dedupes and caps recents, sanitizes font ids |
| `src/settings.rs` | `Settings::save_to` | Writes `to_json` with `write_atomic` |
| `src/settings.rs` | `Settings::to_json` | The saved bytes: font ids and column widths stored as `from_json` reads them |
| `src/settings.rs` | `write_atomic` | Temp file + sync + rename; the avatar cache uses it too |
| `src/settings.rs` | `remember_repository`, `forget_last_repository` | Last and recent repositories |
| `src/settings.rs` | `set_ui_font` | Trims the interface face id; a blank name clears it |
| `src/settings.rs` | `set_terminal_font` | Trims the terminal face id; a blank name clears it |
| `src/ui/settings.rs` | `show`, `PAGES` | Settings sidebar and the Appearance, Terminal, History, GitHub Auth and About pages |
| `src/ui/mod.rs` | `SettingsScreen::preview` | The preview shell on the Terminal page (`terminal::show_preview`): a 32-row PTY drawn as the prompt block that ends at the cursor, at most 8 lines. Ignores pointer and keyboard. Dropped when settings closes. Not saved |
| `src/ui/fonts.rs` | `FontService` | Registers the bundled interface and terminal faces |
| `src/ui/mod.rs` | `Action::SetUiFont` | Stores the interface face id and saves |
| `src/ui/mod.rs` | `Action::SetTerminalFont` | Stores the terminal face id and saves |
| `src/settings.rs` | `MAX_RECENT_REPOSITORIES` | Recent list size (10) |
| `src/settings.rs` | `SettingsError` | Read / parse / write errors shown to the user |
| `src/ui/mod.rs` | `CthulhuApp::new` | Loads settings, applies the theme, picks the startup screen |
| `src/ui/mod.rs` | `load_settings` | The settings and `save_path`: a file that could not be read leaves no save path, so nothing is written over it |
| `src/ui/mod.rs` | `CthulhuApp::finish_open` | Remembers a repository on success; forgets `last_repository` when it fails to open |
| `src/ui/mod.rs` | `CthulhuApp::ui`, `Action::ToggleBranchesSidebar`, `Action::ToggleDetailSidebar` | Flips `branches_sidebar_hidden` (saved as `history_sidebar_hidden`) or `detail_sidebar_hidden` when the repository view asks |
| `src/ui/mod.rs` | `CthulhuApp::ui`, `Action::ToggleTerminal` | Flips `terminal_hidden` when the repository view asks. Showing the terminal also opens the right sidebar |
| `src/settings.rs` | `HistoryColumns`, `HistoryColumns::normalized` | Width of each history column, validated field by field |
| `src/ui/history.rs` | `show`, `header`, `drag_column`, `reset_column` | `header` draws the column dividers; `show` reads them: a drag goes through `drag_column`, a double click through `reset_column`, and a release or double click returns the new widths |
| `src/ui/mod.rs` | `Action::SetHistoryColumns` | Stores `history_columns` (normalized) and saves, only when it changed |
| `src/ui/mod.rs` | `Action::SetAuthorAvatars` | Stores `author_avatars`, switches the avatar service, saves |
| `src/ui/github_auth.rs` | `GitHubAuth`, `Change::InKeychain` | Reads, saves and removes the GitHub token in the system keychain on a worker thread; reports `InKeychain` so the app stores `github_token_in_keyring`. A failed save leaves the flag alone, so a token saved earlier is still read next launch |
| `src/ui/mod.rs` | `CthulhuApp::poll_github` | Applies `InKeychain` (saves only when the flag changes) and hands the token to the avatar service |
| `src/github/token.rs` | `Keychain`, `load_token`, `save_token`, `delete_token` | The keychain entry, reached through one worker thread so calls run in order. A stored value that is not a token is an error, not "nothing stored". A save that misses its 5 s deadline is never kept: it is skipped, or undone if it lands late, and the entry is deleted when the undo fails. Tests hand `Keychain` an entry from keyring-core's mock store |
| `src/settings.rs` | `lenient_columns`, `lenient_per_mille` | Per-field validation of `history_columns`; `180.0` reads as 180 |
| `src/ui/settings.rs` | `history_page`, `github_page` | The History and GitHub Auth pages |
| `src/ui/mod.rs` | `CthulhuApp::save_settings` | The only caller of `save_to` |

## File location

| OS | Path |
| -- | ---- |
| Linux | `$XDG_CONFIG_HOME/cthulhu-git/settings.json` (default `~/.config/cthulhu-git/`) |
| macOS | `~/Library/Application Support/io.github.don-linux.cthulhu-git/settings.json` |
| Windows | `%APPDATA%\don-linux\cthulhu-git\config\settings.json` |

For manual tests on Linux, point `XDG_CONFIG_HOME` at a temp folder.

```json
{
  "theme": null,
  "last_repository": "/home/me/src/rlyeh",
  "recent_repositories": ["/home/me/src/rlyeh", "/home/me/src/necronomicon"],
  "history_sidebar_hidden": false,
  "detail_sidebar_hidden": false,
  "terminal_hidden": true,
  "ui_font": null,
  "terminal_font": null,
  "history_columns": {
    "branch": 180,
    "graph": 140,
    "hash": 110,
    "message": 370,
    "author": 200
  },
  "author_avatars": true,
  "github_token_in_keyring": false
}
```

## Fields

| Field | Meaning | Default |
| ----- | ------- | ------- |
| `theme` | Theme id (`docs/THEMES.md`); `null` or unknown means the default theme | `null` |
| `last_repository` | Repository root reopened on launch | `null` |
| `recent_repositories` | Repository roots, newest first, no duplicates, at most 10 | `[]` |
| `history_sidebar_hidden` | The branches sidebar is hidden (toggled with the left panel button in the top bar). The Rust field is `branches_sidebar_hidden`; the key keeps this name so existing files load. Negated so a missing field shows the sidebar | `false` |
| `detail_sidebar_hidden` | The commit sidebar on the right is hidden (toggled with the right panel button). Negated so a missing field shows the sidebar | `false` |
| `terminal_hidden` | The terminal strip at the bottom of the right sidebar is hidden (toggled with the terminal button in the bottom bar). A missing field keeps it hidden. Showing it also opens the right sidebar. Its height is not saved | `true` |
| `ui_font` | Interface typeface. `null`, blank, or an unknown id keeps Ubuntu. An id from the bundled catalog (`docs/FONTS.md`), not a file path or a system family. A missing field keeps the default | `null` |
| `terminal_font` | Font drawn in the terminal. `null`, blank, or an unknown id keeps Meslo LGS NF. `builtin` keeps the monospace shipped with the app. Any other value is an id from the bundled catalog, not a file path. A name saved by an older version that scanned the system is unknown and falls back to Meslo LGS NF. A missing field keeps the default | `null` |
| `history_columns` | Width of each commit history column (Branch, Graph, Commit Hash, Commit Message, Author), in thousandths of the history panel, so it follows the window and the open sidebars. Each column is independent: no width is derived from another and the sum need not be 1000. A column never gets narrower than its own content needs; when the columns add up to more than the panel, the history scrolls sideways. Each field outside `20..=1000`, or not a number, falls back to its own default; a value that is not an object keeps every default | `{ "branch": 180, "graph": 140, "hash": 110, "message": 370, "author": 200 }` |
| `author_avatars` | Download author pictures from GitHub and Gravatar (Settings → History). Off draws initials and sends nothing | `true` |
| `github_token_in_keyring` | A GitHub token was saved in the system keychain (Settings → GitHub Auth), so it is read at startup. The token itself is never written to this file. `false` means the keychain is never asked, so a user without a token never sees a keychain prompt | `false` |

Only repository roots are stored, never subfolders. Paths that are not valid
Unicode are not stored (JSON strings are UTF-8).

## GitHub token

Optional and read-only. It is stored with the `keyring` crate under the
service `io.github.don-linux.cthulhu-git` and the user `github.com`: Keychain
on macOS, Credential Manager on Windows, the Secret Service (GNOME Keyring,
KWallet) on Linux. When no keychain answers, the token is used for that
session only and the page says so. It is sent only to `api.github.com`.

## Avatar cache

Pictures are cached in the per-user cache folder (`directories::ProjectDirs`
`cache_dir()/avatars`, for example `~/.cache/cthulhu-git/avatars` on Linux),
one file per address named by the SHA-256 of the trimmed, lowercased
address: `.img` for a picture (kept 7 days) and `.miss` when none was found
(kept 1 day, and cleared when the token changes). Only PNG and JPEG are kept
as `.img`; any other format, GIF included, is a `.miss` and the row shows
initials. A wait for GitHub's rate limit is never written as a `.miss`.
The files are written with `write_atomic`, so they are 0600 on Unix too.
Deleting the folder is safe.

## Add a setting

1. Add the field to `Settings` in `src/settings.rs`, with a doc comment. Its
   type needs a `Default` (older files lack the field and must still load).
2. Read it where needed (usually `CthulhuApp::new` in `src/ui/mod.rs`).
3. Where it changes: mutate `self.settings`, then call `self.save_settings()`.
4. Add rows to the tables in `src/settings.rs`: `from_json_fills_defaults_and_normalizes`
   (a file without the field, and its odd values) and
   `to_json_normalizes_and_round_trips`, whose exact key list must name it.
5. Add it to the Fields table above.

## Rules

- Write only through `CthulhuApp::save_settings` / `Settings::save_to`.
- Never rename or retype a saved key without `#[serde(alias = "old_name")]`
  (or a migration): users would silently lose that value. A Rust field may be
  renamed under `#[serde(rename = "key")]`, as `branches_sidebar_hidden` is.
- Unknown fields are ignored, so a newer file still loads in an older version.
- Secrets are never fields. The GitHub token lives in the system keychain;
  this file only says whether to look there.
