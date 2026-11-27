//! User settings, stored as one JSON file in the per-user config folder of
//! each OS. The map for changing this module is `docs/SETTINGS.md`.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};

pub const FILE_NAME: &str = "settings.json";
pub const MAX_RECENT_REPOSITORIES: usize = 10;
/// Saved font ids are cut here, on a char boundary. A longer name is not stored raw.
const MAX_FONT_NAME_BYTES: usize = 128;

/// Files written by older or newer versions still load: a missing field takes
/// its value from [`Settings::default`] and unknown ones are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Id of the active theme. `None` or an unknown id means the default theme.
    pub theme: Option<String>,
    /// Root of the repository reopened on launch.
    pub last_repository: Option<PathBuf>,
    /// Repository roots, newest first, without duplicates.
    /// JSON `null` is the empty list, so one null field does not reject the file.
    #[serde(deserialize_with = "null_as_default")]
    pub recent_repositories: Vec<PathBuf>,
    /// Whether the branches sidebar of the repository view is hidden.
    /// Stored negated so the sidebar shows when the field is missing, under
    /// the key that existing files use.
    #[serde(rename = "history_sidebar_hidden")]
    pub branches_sidebar_hidden: bool,
    /// Whether the latest-commit sidebar on the right is hidden.
    /// Stored negated so the sidebar shows when the field is missing.
    pub detail_sidebar_hidden: bool,
    /// Whether the terminal strip in the right sidebar is hidden.
    /// A missing field keeps the terminal hidden, unlike the sidebars.
    pub terminal_hidden: bool,
    /// Interface typeface. `None`, blank, or an unknown id keeps Ubuntu.
    /// An id from the bundled catalog, not a file path or a system family.
    /// A value that is not a string is ignored, so the rest of the file loads.
    #[serde(deserialize_with = "lenient_font")]
    pub ui_font: Option<String>,
    /// Font drawn in the terminal. `None`, blank, or an unknown id keeps
    /// Meslo LGS NF. `builtin` keeps the monospace shipped with the app.
    /// Any other value is an id from the bundled catalog, not a file path.
    /// A value that is not a string is ignored, so the rest of the file loads.
    #[serde(deserialize_with = "lenient_font")]
    pub terminal_font: Option<String>,
    /// Width of each commit history column. A value that is not an object
    /// keeps every default; a bad field keeps only its own default.
    #[serde(deserialize_with = "lenient_columns")]
    pub history_columns: HistoryColumns,
    /// Author avatars are downloaded from GitHub and Gravatar. Off draws
    /// initials only and sends nothing.
    pub author_avatars: bool,
    /// A GitHub token was saved in the system keychain. The token itself is
    /// never written here; this flag only says whether to look for it, so a
    /// user without a token never triggers a keychain prompt.
    pub github_token_in_keyring: bool,
}

/// Width of each commit history column, in thousandths of the history panel.
///
/// The columns are independent: no width is derived from another, and the
/// sum does not have to be 1000. The defaults add up to 1000, so they fill
/// the panel exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HistoryColumns {
    #[serde(deserialize_with = "lenient_per_mille")]
    pub branch: u16,
    #[serde(deserialize_with = "lenient_per_mille")]
    pub graph: u16,
    #[serde(deserialize_with = "lenient_per_mille")]
    pub hash: u16,
    #[serde(deserialize_with = "lenient_per_mille")]
    pub message: u16,
    #[serde(deserialize_with = "lenient_per_mille")]
    pub author: u16,
}

impl HistoryColumns {
    /// Smallest and largest stored width. A full panel is 1000.
    pub const RANGE: std::ops::RangeInclusive<u16> = 20..=1000;

    /// Puts each field outside [`Self::RANGE`] back to its own default.
    pub fn normalized(self) -> Self {
        let defaults = Self::default();
        let keep = |value: u16, default: u16| {
            if Self::RANGE.contains(&value) {
                value
            } else {
                default
            }
        };
        Self {
            branch: keep(self.branch, defaults.branch),
            graph: keep(self.graph, defaults.graph),
            hash: keep(self.hash, defaults.hash),
            message: keep(self.message, defaults.message),
            author: keep(self.author, defaults.author),
        }
    }
}

impl Default for HistoryColumns {
    fn default() -> Self {
        Self {
            branch: 180,
            graph: 140,
            hash: 110,
            message: 370,
            author: 200,
        }
    }
}

/// Anything that is not a whole number in `u16` becomes 0, which
/// [`HistoryColumns::normalized`] replaces with that field's default.
fn lenient_per_mille<'de, D>(deserializer: D) -> Result<u16, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    // A hand-edited `180.0` is still 180.
    let number = value.as_u64().or_else(|| {
        let float = value.as_f64()?;
        (float.is_finite() && float >= 0.0 && float.fract() == 0.0).then_some(float as u64)
    });
    Ok(number
        .and_then(|number| u16::try_from(number).ok())
        .unwrap_or(0))
}

/// A non-string is not a font id. `null` and any other JSON type become
/// `None`, which is the same as a missing field.
fn lenient_font<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::String(text) => Some(text),
        _ => None,
    })
}

fn lenient_columns<'de, D>(deserializer: D) -> Result<HistoryColumns, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value::<HistoryColumns>(value)
        .unwrap_or_default()
        .normalized())
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: None,
            last_repository: None,
            recent_repositories: Vec::new(),
            branches_sidebar_hidden: false,
            detail_sidebar_hidden: false,
            terminal_hidden: true,
            ui_font: None,
            terminal_font: None,
            history_columns: HistoryColumns::default(),
            author_avatars: true,
            github_token_in_keyring: false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("Could not read settings from {0}: {1}")]
    Read(PathBuf, #[source] io::Error),
    #[error(
        "Settings file {0} is invalid ({1}); using the defaults and leaving the file as it is."
    )]
    Parse(PathBuf, #[source] serde_json::Error),
    #[error("Could not save settings to {0}: {1}")]
    Write(PathBuf, #[source] io::Error),
}

impl Settings {
    /// `settings.json` in the config folder the OS assigns to the app:
    /// - Linux: `$XDG_CONFIG_HOME/cthulhu-git/` (default `~/.config/cthulhu-git/`)
    /// - macOS: `~/Library/Application Support/io.github.don-linux.cthulhu-git/`
    /// - Windows: `%APPDATA%\don-linux\cthulhu-git\config\`
    ///
    /// `None` when the OS reports no home folder.
    pub fn default_path() -> Option<PathBuf> {
        project_dirs().map(|dirs| dirs.config_dir().join(FILE_NAME))
    }

    /// A missing file gives the defaults; an unreadable or invalid one is an
    /// error. The bytes are read by [`Self::from_json`].
    pub fn load_from(path: &Path) -> Result<Self, SettingsError> {
        match fs::read(path) {
            Ok(bytes) => Self::from_json(&bytes)
                .map_err(|error| SettingsError::Parse(path.to_path_buf(), error)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(SettingsError::Read(path.to_path_buf(), error)),
        }
    }

    /// Writes [`Self::to_json`] with [`write_atomic`].
    pub fn save_to(&self, path: &Path) -> Result<(), SettingsError> {
        let fail = |error: io::Error| SettingsError::Write(path.to_path_buf(), error);
        let json = self.to_json().map_err(|error| fail(error.into()))?;
        write_atomic(path, &json).map_err(fail)
    }

    /// Settings from the bytes of a settings file. Recents are deduped and
    /// capped, and font ids sanitized, because the file may be hand-edited.
    pub fn from_json(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        // Notepad on Windows writes a UTF-8 BOM in front of otherwise valid JSON.
        let json = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
        let mut settings: Self = serde_json::from_slice(json)?;
        settings.normalize_recent();
        settings.normalize_fonts();
        Ok(settings)
    }

    /// The bytes of a settings file: pretty JSON and a final newline.
    ///
    /// A blank font id and an out-of-range column width are stored the way
    /// [`Self::from_json`] would read them, so a direct field assignment
    /// writes the same file as the setters. Fails only on a path that is not
    /// valid Unicode, which a JSON string cannot hold.
    pub fn to_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        let mut stored = self.clone();
        stored.normalize_fonts();
        stored.history_columns = stored.history_columns.normalized();
        let mut json = serde_json::to_vec_pretty(&stored)?;
        json.push(b'\n');
        Ok(json)
    }

    /// Makes `root` the repository reopened on launch and the first recent one.
    ///
    /// JSON strings must be UTF-8, so a root that is not valid Unicode is not
    /// remembered.
    pub fn remember_repository(&mut self, root: &Path) {
        if root.to_str().is_none() {
            return;
        }
        self.last_repository = Some(root.to_path_buf());
        self.recent_repositories.retain(|recent| recent != root);
        self.recent_repositories.insert(0, root.to_path_buf());
        self.recent_repositories.truncate(MAX_RECENT_REPOSITORIES);
    }

    /// Stops reopening the last repository on launch; it stays in the recents.
    pub fn forget_last_repository(&mut self) {
        self.last_repository = None;
    }

    /// Stores the interface typeface. Blank names, interior newlines, and a
    /// name that is empty after sanitizing clear it.
    pub fn set_ui_font(&mut self, name: Option<String>) {
        self.ui_font = name;
        normalize_font_name(&mut self.ui_font);
    }

    /// Stores the terminal typeface. Blank names, interior newlines, and a
    /// name that is empty after sanitizing clear it.
    pub fn set_terminal_font(&mut self, name: Option<String>) {
        self.terminal_font = name;
        normalize_font_name(&mut self.terminal_font);
    }

    /// A hand-edited file may repeat or pile up entries.
    fn normalize_recent(&mut self) {
        let mut seen = HashSet::new();
        self.recent_repositories
            .retain(|path| seen.insert(path.clone()));
        self.recent_repositories.truncate(MAX_RECENT_REPOSITORIES);
    }

    fn normalize_fonts(&mut self) {
        normalize_font_name(&mut self.ui_font);
        normalize_font_name(&mut self.terminal_font);
    }
}

/// Drops a blank id and stores [`sanitize_font_name`] for the rest.
fn normalize_font_name(slot: &mut Option<String>) {
    let Some(name) = slot.as_deref() else {
        return;
    };
    match sanitize_font_name(name) {
        Some(clean) if clean == name => {}
        other => *slot = other,
    }
}

/// Font id safe to store and to show in a warning.
///
/// Trims, strips bidi controls (U+202A–U+202E and U+2066–U+2069), and keeps
/// at most 128 bytes. An interior newline is rejected, and so is a name that
/// is empty afterwards. The stored value is this sanitized token, or `None`.
pub fn sanitize_font_name(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || has_interior_newline(trimmed) {
        return None;
    }
    let stripped: String = trimmed.chars().filter(|ch| !is_bidi_control(*ch)).collect();
    let stripped = stripped.trim();
    if stripped.is_empty() {
        None
    } else {
        Some(stripped[..stripped.floor_char_boundary(MAX_FONT_NAME_BYTES)].to_owned())
    }
}

fn has_interior_newline(name: &str) -> bool {
    name.contains(['\n', '\r'])
}

fn is_bidi_control(ch: char) -> bool {
    matches!(ch, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// The per-user folders the OS assigns to the app: the config folder holds
/// [`Settings::default_path`], the cache folder the downloaded avatars.
/// `None` when the OS reports no home folder.
pub fn project_dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("io.github", "don-linux", "cthulhu-git")
}

/// Replaces `path` with `bytes`, creating its folder. The bytes go to a temp
/// file next to `path`, reach the disk, and the temp file is renamed over
/// `path`, so a crash never leaves a half-written file. Also used by the
/// avatar cache.
///
/// The temp file has a fresh random name and is created exclusively, so a
/// link planted next to `path` is never opened; rename(2) replaces a symlink
/// at `path` instead of writing through it. On Unix the file is 0600.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    // `keep` clears the temporary attribute Windows gives the file, which
    // would otherwise follow it to `path`. std's rename, unlike tempfile's
    // `persist`, retries with POSIX semantics when Windows refuses to replace
    // the target.
    let temp = file.into_temp_path().keep()?;
    fs::rename(&temp, path).inspect_err(|_| {
        let _ = fs::remove_file(&temp);
    })
}

fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use tempfile::TempDir;

    use super::*;

    fn settings_path(dir: &TempDir) -> PathBuf {
        dir.path().join("nested").join(FILE_NAME)
    }

    fn file_names(dir: &Path) -> Vec<std::ffi::OsString> {
        fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| entry.expect("entry").file_name())
            .collect()
    }

    fn abyss() -> Settings {
        Settings {
            theme: Some("abyss".to_owned()),
            ..Settings::default()
        }
    }

    fn fonts(ui: Option<&str>, terminal: Option<&str>) -> Settings {
        Settings {
            ui_font: ui.map(str::to_owned),
            terminal_font: terminal.map(str::to_owned),
            ..Settings::default()
        }
    }

    /// A last repository that is not one, and recents a user typed by hand.
    fn odd_paths() -> Settings {
        Settings {
            last_repository: Some("/src/readme.txt".into()),
            recent_repositories: vec!["rel/repo".into(), r"C:\repo".into()],
            branches_sidebar_hidden: true,
            ..Settings::default()
        }
    }

    #[test]
    fn round_trip_creates_folders_and_leaves_no_temp_file() {
        let dir = TempDir::new().expect("temp dir");
        let path = settings_path(&dir);
        assert_eq!(
            Settings::load_from(&path).expect("a missing file"),
            Settings::default()
        );
        let mut settings = abyss();
        settings.remember_repository(Path::new("/src/rlyeh"));

        settings.save_to(&path).expect("save");
        settings.save_to(&path).expect("save over an existing file");

        assert_eq!(Settings::load_from(&path).expect("load"), settings);
        assert_eq!(file_names(path.parent().expect("parent")), [FILE_NAME]);
    }

    /// A directory where the file belongs is a read error and a write
    /// error, and the failed save leaves no temp file next to it.
    #[test]
    fn a_directory_at_the_settings_path_is_a_read_and_a_write_error() {
        let dir = TempDir::new().expect("temp dir");
        let path = settings_path(&dir);
        fs::create_dir_all(&path).expect("directory");
        let shown = path.display().to_string();

        let load = Settings::load_from(&path).expect_err("load a directory");
        assert!(matches!(load, SettingsError::Read(..)), "{load:?}");
        assert!(
            load.to_string()
                .starts_with(&format!("Could not read settings from {shown}: ")),
            "{load}"
        );
        let save = Settings::default()
            .save_to(&path)
            .expect_err("save over a directory");
        assert!(matches!(save, SettingsError::Write(..)), "{save:?}");
        assert!(
            save.to_string()
                .starts_with(&format!("Could not save settings to {shown}: ")),
            "{save}"
        );
        assert!(path.is_dir());
        assert_eq!(file_names(path.parent().expect("parent")), [FILE_NAME]);
    }

    /// Every way a file is read. `None` is a parse error.
    #[test]
    fn from_json_fills_defaults_and_normalizes() {
        let defaults = HistoryColumns::default();
        let mut recents: Vec<String> = (0..20).map(|index| format!("/repo-{index}")).collect();
        recents.insert(1, "/repo-0".to_owned());
        let recents = serde_json::json!({ "recent_repositories": recents }).to_string();
        let columns = |history_columns| Settings {
            history_columns,
            ..Settings::default()
        };
        let cases = [
            ("{ not json", None),
            (
                r#"{ "theme": "abyss", "from_the_future": 1 }"#,
                Some(abyss()),
            ),
            // Older files: sidebars show, the terminal stays hidden, fonts,
            // columns and avatars keep their defaults, no keychain lookup.
            // Spelled out: serde fills a missing field from `Settings::default()`.
            (
                r#"{ "recent_repositories": [] }"#,
                Some(Settings {
                    theme: None,
                    last_repository: None,
                    recent_repositories: Vec::new(),
                    branches_sidebar_hidden: false,
                    detail_sidebar_hidden: false,
                    terminal_hidden: true,
                    ui_font: None,
                    terminal_font: None,
                    history_columns: defaults,
                    author_avatars: true,
                    github_token_in_keyring: false,
                }),
            ),
            (r#"{ "theme": "abyss" }"#, Some(abyss())),
            (
                r#"{ "theme": "abyss", "terminal_hidden": false }"#,
                Some(Settings {
                    terminal_hidden: false,
                    ..abyss()
                }),
            ),
            // `null` is no value, and on the recent list an empty list.
            (
                r#"{ "theme": null, "last_repository": null }"#,
                Some(Settings::default()),
            ),
            (
                r#"{ "theme": null, "last_repository": null, "recent_repositories": null }"#,
                Some(Settings::default()),
            ),
            (
                "\u{FEFF}{ \"theme\": \"abyss\", \"last_repository\": \"/src/rlyeh\",
                  \"recent_repositories\": [\"/src/rlyeh\", \"/src/innsmouth\"],
                  \"history_sidebar_hidden\": true }",
                Some(Settings {
                    last_repository: Some("/src/rlyeh".into()),
                    recent_repositories: vec!["/src/rlyeh".into(), "/src/innsmouth".into()],
                    branches_sidebar_hidden: true,
                    ..abyss()
                }),
            ),
            (
                recents.as_str(),
                Some(Settings {
                    recent_repositories: (0..MAX_RECENT_REPOSITORIES)
                        .map(|index| format!("/repo-{index}").into())
                        .collect(),
                    ..Settings::default()
                }),
            ),
            (
                r#"{"theme":null,"last_repository":"/src/readme.txt","recent_repositories":
                    ["rel/repo","rel/repo","C:\\repo","C:\\repo"],"history_sidebar_hidden":true}"#,
                Some(odd_paths()),
            ),
            // Paths compare by component: a trailing slash is the same root.
            (
                r#"{ "recent_repositories": ["/src/rlyeh", "/src/rlyeh/"] }"#,
                Some(Settings {
                    recent_repositories: vec!["/src/rlyeh".into()],
                    ..Settings::default()
                }),
            ),
            // A font id is trimmed; anything else is no font, and the rest loads.
            (
                r#"{ "terminal_font": "  Fira Code  " }"#,
                Some(fonts(None, Some("Fira Code"))),
            ),
            (r#"{ "terminal_font": "   " }"#, Some(Settings::default())),
            (r#"{ "terminal_font": "" }"#, Some(Settings::default())),
            (r#"{ "terminal_font": null }"#, Some(Settings::default())),
            (r#"{"ui_font": 1, "theme": "abyss"}"#, Some(abyss())),
            (
                r#"{"terminal_font": [], "theme": "abyss", "recent_repositories": ["/src/rlyeh"]}"#,
                Some(Settings {
                    recent_repositories: vec!["/src/rlyeh".into()],
                    ..abyss()
                }),
            ),
            (
                r#"{"ui_font": "Comic Sans", "theme": "abyss"}"#,
                Some(Settings {
                    ui_font: Some("Comic Sans".to_owned()),
                    ..abyss()
                }),
            ),
            (
                r#"{"ui_font":"obel\nmono","terminal_font":"\u202EFira","theme":"abyss"}"#,
                Some(Settings {
                    terminal_font: Some("Fira".to_owned()),
                    ..abyss()
                }),
            ),
            // A bad column keeps only its own default.
            (
                r#"{ "history_columns": { "branch": 0, "graph": 5000, "hash": "wide",
                     "message": -3, "author": 250, "future_column": 7 } }"#,
                Some(columns(HistoryColumns {
                    author: 250,
                    ..defaults
                })),
            ),
            (
                r#"{ "history_columns": { "graph": 333 } }"#,
                Some(columns(HistoryColumns {
                    graph: 333,
                    ..defaults
                })),
            ),
            // A whole float is that width; a fraction is no width at all.
            (
                r#"{ "history_columns": { "graph": 180.0, "message": 500.5 } }"#,
                Some(columns(HistoryColumns {
                    graph: 180,
                    ..defaults
                })),
            ),
            (r#"{ "history_columns": null }"#, Some(Settings::default())),
            (r#"{ "history_columns": 12 }"#, Some(Settings::default())),
            (
                r#"{ "history_columns": [1, 2, 3] }"#,
                Some(Settings::default()),
            ),
        ];
        for (json, expected) in cases {
            assert_eq!(
                Settings::from_json(json.as_bytes()).ok(),
                expected,
                "{json:?}"
            );
        }
    }

    /// What `save_to` writes reads back as `expected` (`None`: as saved),
    /// holds exactly the known keys, and so never a token.
    #[test]
    fn to_json_normalizes_and_round_trips() {
        const KEYS: [&str; 11] = [
            "author_avatars",
            "detail_sidebar_hidden",
            "github_token_in_keyring",
            "history_columns",
            "history_sidebar_hidden",
            "last_repository",
            "recent_repositories",
            "terminal_font",
            "terminal_hidden",
            "theme",
            "ui_font",
        ];
        const COLUMNS: [&str; 5] = ["author", "branch", "graph", "hash", "message"];
        let huge = "a".repeat(100_000);
        let cases = [
            (
                Settings {
                    branches_sidebar_hidden: true,
                    ..abyss()
                },
                None,
            ),
            (
                Settings {
                    detail_sidebar_hidden: true,
                    ..abyss()
                },
                None,
            ),
            (
                Settings {
                    terminal_hidden: false,
                    ..abyss()
                },
                None,
            ),
            (abyss(), None),
            (odd_paths(), None),
            (fonts(Some("inter"), Some("JetBrains Mono")), None),
            (
                Settings {
                    history_columns: HistoryColumns {
                        branch: 300,
                        graph: 20,
                        hash: 1000,
                        message: 999,
                        author: 21,
                    },
                    author_avatars: false,
                    github_token_in_keyring: true,
                    ..Settings::default()
                },
                None,
            ),
            // A direct field assignment is stored the way the setters store it.
            (
                fonts(Some("  aspekta  "), Some("  Fira Code  ")),
                Some(fonts(Some("aspekta"), Some("Fira Code"))),
            ),
            (fonts(None, Some("   ")), Some(Settings::default())),
            (
                fonts(Some("  obel-mono \n"), Some("  Fira Code \n")),
                Some(fonts(Some("obel-mono"), Some("Fira Code"))),
            ),
            (
                fonts(Some(huge.as_str()), Some("meslo\u{202E}")),
                Some(fonts(Some(&huge[..128]), Some("meslo"))),
            ),
            (
                Settings {
                    history_columns: HistoryColumns {
                        message: 0,
                        ..HistoryColumns::default()
                    },
                    ..Settings::default()
                },
                Some(Settings::default()),
            ),
        ];
        for (saved, expected) in cases {
            let expected = expected.unwrap_or_else(|| saved.clone());
            let json = saved.to_json().expect("serializable");
            let value: serde_json::Value = serde_json::from_slice(&json).expect("json");
            assert_eq!(
                value,
                serde_json::to_value(&expected).expect("value"),
                "{saved:?}"
            );
            for (object, known) in [
                (&value, &KEYS[..]),
                (&value["history_columns"], &COLUMNS[..]),
            ] {
                let mut keys: Vec<&str> = object
                    .as_object()
                    .expect("object")
                    .keys()
                    .map(String::as_str)
                    .collect();
                keys.sort_unstable();
                assert_eq!(keys, known, "{saved:?}");
            }
            assert_eq!(
                Settings::from_json(&json).expect("load"),
                expected,
                "{saved:?}"
            );
        }
    }

    #[test]
    fn font_setters_store_the_sanitized_id() {
        let huge = "a".repeat(100_000);
        let accented = "é".repeat(100);
        let cases = [
            ("  ", None),
            ("   ", None),
            ("  IBM Plex Mono  ", Some("IBM Plex Mono")),
            ("  obel-mono  ", Some("obel-mono")),
            ("  obel-mono \n", Some("obel-mono")),
            ("  Fira Code \n", Some("Fira Code")),
            ("\u{202E}obel-mono", Some("obel-mono")),
            ("meslo\u{202E}", Some("meslo")),
            ("obel\nmono", None),
            ("fira\rmono", None),
            ("\u{202E}", None),
            (huge.as_str(), Some(&huge[..128])),
            // 128 bytes is 64 two-byte chars.
            (accented.as_str(), Some(&accented[..128])),
        ];
        for (name, expected) in cases {
            let mut settings = Settings::default();
            settings.set_ui_font(Some(name.to_owned()));
            settings.set_terminal_font(Some(name.to_owned()));
            assert_eq!(settings.ui_font.as_deref(), expected, "{name:?}");
            assert_eq!(settings.terminal_font.as_deref(), expected, "{name:?}");
        }
    }

    #[test]
    fn default_history_columns_fill_the_panel() {
        let columns = HistoryColumns::default();
        let sum = columns.branch + columns.graph + columns.hash + columns.message + columns.author;
        assert_eq!(sum, 1000);
    }

    #[test]
    fn remember_moves_to_the_front_caps_and_forget_keeps_the_recents() {
        let mut settings = Settings::default();
        for root in ["/a", "/b", "/a"] {
            settings.remember_repository(Path::new(root));
        }
        assert_eq!(settings.last_repository.as_deref(), Some(Path::new("/a")));
        assert_eq!(
            settings.recent_repositories,
            [PathBuf::from("/a"), PathBuf::from("/b")]
        );

        for index in 0..MAX_RECENT_REPOSITORIES + 5 {
            settings.remember_repository(&PathBuf::from(format!("/repo-{index}")));
        }
        let newest = PathBuf::from(format!("/repo-{}", MAX_RECENT_REPOSITORIES + 4));
        assert_eq!(settings.recent_repositories.len(), MAX_RECENT_REPOSITORIES);
        assert_eq!(settings.recent_repositories[0], newest);

        settings.forget_last_repository();
        assert_eq!(settings.last_repository, None);
        assert_eq!(settings.recent_repositories.len(), MAX_RECENT_REPOSITORIES);
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_roots_are_not_remembered() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let mut settings = Settings::default();
        settings.remember_repository(Path::new(OsStr::from_bytes(b"/caf\xe9")));
        assert_eq!(settings, Settings::default());
    }

    /// Every column, every kind of bad width, next to the smallest and the
    /// largest good one: only the bad column falls back to its default.
    #[test]
    fn one_bad_column_keeps_its_siblings() {
        const NAMES: [&str; 5] = ["branch", "graph", "hash", "message", "author"];
        let defaults = HistoryColumns::default();
        let defaults = [
            defaults.branch,
            defaults.graph,
            defaults.hash,
            defaults.message,
            defaults.author,
        ];
        let bad = [
            serde_json::json!(0),
            serde_json::json!(1001),
            serde_json::json!(null),
            serde_json::json!("wide"),
            serde_json::json!(1.5),
            serde_json::json!(-4),
        ];
        for good in [20, 1000] {
            for which in 0..NAMES.len() {
                for value in &bad {
                    let columns: serde_json::Map<String, serde_json::Value> = NAMES
                        .iter()
                        .enumerate()
                        .map(|(index, name)| {
                            let width = if index == which {
                                value.clone()
                            } else {
                                serde_json::json!(good)
                            };
                            ((*name).to_owned(), width)
                        })
                        .collect();
                    let json = serde_json::json!({ "history_columns": columns }).to_string();
                    let got = Settings::from_json(json.as_bytes())
                        .expect("load")
                        .history_columns;
                    let got = [got.branch, got.graph, got.hash, got.message, got.author];
                    let expected: Vec<u16> = (0..NAMES.len())
                        .map(|index| {
                            if index == which {
                                defaults[index]
                            } else {
                                good
                            }
                        })
                        .collect();
                    assert_eq!(got, expected[..], "{json}");
                }
            }
        }
    }

    fn arb_text(max_chars: usize) -> impl Strategy<Value = String> {
        prop::collection::vec(any::<char>(), 0..=max_chars)
            .prop_map(|chars| chars.into_iter().collect())
    }

    fn arb_path() -> impl Strategy<Value = PathBuf> {
        prop_oneof![
            4 => arb_text(24),
            1 => (arb_text(8), arb_text(8)).prop_map(|(parent, name)| format!("/{parent}/{name}")),
        ]
        .prop_map(PathBuf::from)
    }

    proptest! {
        #![proptest_config(crate::test_support::proptest_config())]

        /// What `to_json` writes, `from_json` reads back with every field
        /// kept, recents deduped and capped, and widths in range; writing
        /// that again changes nothing.
        #[test]
        fn json_round_trip_keeps_fields_and_normalizes_once(
            theme in prop::option::of(arb_text(32)),
            last in prop::option::of(arb_path()),
            recents in prop::collection::vec(arb_path(), 0..14),
            repeat in any::<prop::sample::Index>(),
            flags in any::<[bool; 5]>(),
            widths in any::<[u16; 5]>(),
        ) {
            let [branches, detail, terminal, avatars, in_keyring] = flags;
            let [branch, graph, hash, message, author] = widths;
            let mut recent_repositories = recents;
            if !recent_repositories.is_empty() {
                let again = repeat.get(&recent_repositories).clone();
                recent_repositories.push(again);
            }
            let settings = Settings {
                theme,
                last_repository: last,
                recent_repositories,
                branches_sidebar_hidden: branches,
                detail_sidebar_hidden: detail,
                terminal_hidden: terminal,
                history_columns: HistoryColumns { branch, graph, hash, message, author },
                author_avatars: avatars,
                github_token_in_keyring: in_keyring,
                ..Settings::default()
            };
            let json = settings.to_json().expect("serializable");
            let loaded = Settings::from_json(&json).expect("load");
            prop_assert_eq!(
                &loaded,
                &Settings {
                    recent_repositories: loaded.recent_repositories.clone(),
                    history_columns: settings.history_columns.normalized(),
                    ..settings.clone()
                }
            );
            let recents = &loaded.recent_repositories;
            prop_assert!(recents.len() <= MAX_RECENT_REPOSITORIES);
            for (index, path) in recents.iter().enumerate() {
                prop_assert!(!recents[index + 1..].contains(path), "{path:?} twice");
                prop_assert!(settings.recent_repositories.contains(path));
            }
            let again = loaded.to_json().expect("serializable");
            prop_assert_eq!(Settings::from_json(&again).expect("reload"), loaded);
        }
    }
}
