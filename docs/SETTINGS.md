# Settings

Map of the settings logic. Update this file when you change it.

## How it works

```text
startup   Settings::default_path() -> Settings::load_from(path)
          missing file -> defaults; unreadable/invalid -> defaults + error banner
decide    command-line folder > settings.last_repository > home screen
change    mutate CthulhuApp.settings -> CthulhuApp::save_settings() -> Settings::save_to(path)
token     github_token_in_keyring -> GitHubAuth::load reads the keychain on a worker thread
columns   history_columns is read every frame; a divider release saves it
font      a saved terminal_font starts a background scan of installed families
          until the face is registered, the terminal keeps the built-in monospace
          the Terminal page shows one prompt line from a 240-point preview shell and ignores input; the preview is not saved
```

Saved right away on every change (no timer, no save-on-exit). The write is
atomic: a temp file next to `settings.json` is renamed over it
(`std::fs::rename` replaces the file on Linux, macOS and Windows).

## Where things are

| File | Symbol | What it controls |
| ---- | ------ | ---------------- |
| `src/settings.rs` | `Settings` | The saved fields (serde, `#[serde(default)]`) |
| `src/settings.rs` | `Settings::default_path` | Per-OS location (via `directories::ProjectDirs`) |
| `src/settings.rs` | `Settings::load_from` | Read + parse; dedupes and caps recents |
| `src/settings.rs` | `Settings::save_to` | Atomic write |
| `src/settings.rs` | `remember_repository`, `forget_last_repository` | Last and recent repositories |
| `src/settings.rs` | `set_terminal_font` | Trims the terminal family; a blank name clears it |
| `src/ui/settings.rs` | `show` | Settings sidebar and the Terminal, History and GitHub Auth pages |
| `src/ui/mod.rs` | `CthulhuApp::font_preview` | A 240-point preview shell on the Terminal page, clipped to one prompt line. Ignores pointer and keyboard. Dropped when settings closes. Not saved |
| `src/ui/fonts.rs` | `FontService` | Lists installed families on a background thread and registers the terminal face |
| `src/ui/mod.rs` | `Action::SetTerminalFont` | Stores the family and saves |
| `src/settings.rs` | `MAX_RECENT_REPOSITORIES` | Recent list size (10) |
| `src/settings.rs` | `SettingsError` | Read / parse / write errors shown to the user |
| `src/ui/mod.rs` | `CthulhuApp::new` | Loads settings, applies the theme, picks the startup screen |
| `src/ui/mod.rs` | `CthulhuApp::poll_opening` | Remembers a repository on success; forgets `last_repository` when it fails to open |
| `src/ui/mod.rs` | `CthulhuApp::ui`, `Action::ToggleBranchesSidebar`, `Action::ToggleDetailSidebar` | Flips `history_sidebar_hidden` or `detail_sidebar_hidden` when the repository view asks |
| `src/ui/mod.rs` | `CthulhuApp::ui`, `Action::ToggleTerminal` | Flips `terminal_hidden` when the repository view asks. Showing the terminal also opens the right sidebar |
| `src/settings.rs` | `HistoryColumns`, `HistoryColumns::normalized` | Width of each history column, validated field by field |
| `src/ui/history.rs` | `show`, `header` | Dragging a column divider; releasing it or double-clicking it returns the new widths |
| `src/ui/mod.rs` | `Action::SetHistoryColumns` | Stores `history_columns` (normalized) and saves, only when it changed |
| `src/ui/mod.rs` | `Action::SetAuthorAvatars` | Stores `author_avatars`, switches the avatar service, saves |
| `src/ui/github_auth.rs` | `GitHubAuth`, `Change::InKeychain` | Reads, saves and removes the GitHub token in the system keychain on a worker thread; reports `InKeychain` so the app stores `github_token_in_keyring`. A failed save leaves the flag alone, so a token saved earlier is still read next launch |
| `src/ui/mod.rs` | `CthulhuApp::poll_github` | Applies `InKeychain` (saves only when the flag changes) and hands the token to the avatar service |
| `src/github/token.rs` | `load_token`, `save_token`, `delete_token` | The keychain entry. A stored value that is not a token is an error, not "nothing stored" |
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
| `history_sidebar_hidden` | The branches sidebar is hidden (toggled with the left panel button in the top bar). The JSON name is historical: this flag used to hide the commit list, which is now the center of the window. Negated so a missing field shows the sidebar | `false` |
| `detail_sidebar_hidden` | The latest-commit sidebar on the right is hidden (toggled with the right panel button). Negated so a missing field shows the sidebar | `false` |
| `terminal_hidden` | The terminal strip at the bottom of the right sidebar is hidden (toggled with the terminal button in the bottom bar). A missing field keeps it hidden. Showing it also opens the right sidebar. Its height is not saved | `true` |
| `terminal_font` | Font family drawn in the terminal. `null`, blank, or an unknown name keeps the built-in monospace. A family name, not a file path. A missing field keeps the default | `null` |
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
Deleting the folder is safe.

## Add a setting

1. Add the field to `Settings` in `src/settings.rs`, with a doc comment. Its
   type needs a `Default` (older files lack the field and must still load).
2. Read it where needed (usually `CthulhuApp::new` in `src/ui/mod.rs`).
3. Where it changes: mutate `self.settings`, then call `self.save_settings()`.
4. Add a test in `src/settings.rs` (round trip, and loading a file without it).
5. Add it to the Fields table above.

## Rules

- Write only through `CthulhuApp::save_settings` / `Settings::save_to`.
- Never rename or retype a field without `#[serde(alias = "old_name")]` (or a
  migration): users would silently lose that value.
- Unknown fields are ignored, so a newer file still loads in an older version.
- Secrets are never fields. The GitHub token lives in the system keychain;
  this file only says whether to look there.
