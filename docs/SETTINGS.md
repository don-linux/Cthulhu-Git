# Settings

Map of the settings logic. Update this file when you change it.

## How it works

```text
startup   Settings::default_path() -> Settings::load_from(path)
          missing file -> defaults; unreadable/invalid -> defaults + error banner
decide    command-line folder > settings.last_repository > home screen
change    mutate CthulhuApp.settings -> CthulhuApp::save_settings() -> Settings::save_to(path)
font      a saved terminal_font starts a background scan of installed families
          until the face is registered, the terminal keeps the built-in monospace
          the Terminal page shows one prompt line from a full-height preview shell and ignores input; the preview is not saved
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
| `src/ui/settings.rs` | `show` | Settings sidebar and the Terminal page |
| `src/ui/mod.rs` | `CthulhuApp::font_preview` | Full-height shell on the Terminal page, clipped to one prompt line. Ignores pointer and keyboard. Dropped when settings closes. Not saved |
| `src/ui/fonts.rs` | `FontService` | Lists installed families on a background thread and registers the terminal face |
| `src/ui/mod.rs` | `Action::SetTerminalFont` | Stores the family and saves |
| `src/settings.rs` | `MAX_RECENT_REPOSITORIES` | Recent list size (10) |
| `src/settings.rs` | `SettingsError` | Read / parse / write errors shown to the user |
| `src/ui/mod.rs` | `CthulhuApp::new` | Loads settings, applies the theme, picks the startup screen |
| `src/ui/mod.rs` | `CthulhuApp::poll_opening` | Remembers a repository on success; forgets `last_repository` when it fails to open |
| `src/ui/mod.rs` | `CthulhuApp::ui`, `Action::ToggleBranchesSidebar`, `Action::ToggleDetailSidebar` | Flips `history_sidebar_hidden` or `detail_sidebar_hidden` when the repository view asks |
| `src/ui/mod.rs` | `CthulhuApp::ui`, `Action::ToggleTerminal` | Flips `terminal_hidden` when the repository view asks. Showing the terminal also opens the right sidebar |
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
  "terminal_font": null
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

Only repository roots are stored, never subfolders. Paths that are not valid
Unicode are not stored (JSON strings are UTF-8).

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
