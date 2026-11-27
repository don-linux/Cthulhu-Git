//! User settings, stored as one JSON file in the per-user config folder of
//! each OS. The map for changing this module is `docs/SETTINGS.md`.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};

pub const FILE_NAME: &str = "settings.json";
pub const MAX_RECENT_REPOSITORIES: usize = 10;
/// Saved font ids are cut here, on a char boundary. A longer name is not stored raw.
const MAX_FONT_NAME_BYTES: usize = 128;

/// Every field has a default, so files written by older or newer versions
/// still load: missing fields take the default and unknown ones are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Id of the active theme. `None` or an unknown id means the default theme.
    pub theme: Option<String>,
    /// Root of the repository reopened on launch.
    pub last_repository: Option<PathBuf>,
    /// Repository roots, newest first, without duplicates.
    /// JSON `null` is the empty list, so one null field does not reject the file.
    #[serde(default, deserialize_with = "null_as_default")]
    pub recent_repositories: Vec<PathBuf>,
    /// Whether the branches sidebar of the repository view is hidden.
    /// The JSON name is historical: this flag used to hide the commit list,
    /// which is now the center of the window. Stored negated so the sidebar
    /// shows when the field is missing.
    pub history_sidebar_hidden: bool,
    /// Whether the latest-commit sidebar on the right is hidden.
    /// Stored negated so the sidebar shows when the field is missing.
    pub detail_sidebar_hidden: bool,
    /// Whether the terminal strip in the right sidebar is hidden.
    /// A missing field keeps the terminal hidden, unlike the sidebars.
    /// `#[serde(default)]` on the struct fills a missing bool with `false`,
    /// so this field names its own default.
    #[serde(default = "default_terminal_hidden")]
    pub terminal_hidden: bool,
    /// Interface typeface. `None`, blank, or an unknown id keeps Ubuntu.
    /// An id from the bundled catalog, not a file path or a system family.
    /// A value that is not a string is ignored, so the rest of the file loads.
    #[serde(default, deserialize_with = "lenient_font")]
    pub ui_font: Option<String>,
    /// Font drawn in the terminal. `None`, blank, or an unknown id keeps
    /// Meslo LGS NF. `builtin` keeps the monospace shipped with the app.
    /// Any other value is an id from the bundled catalog, not a file path.
    /// A value that is not a string is ignored, so the rest of the file loads.
    #[serde(default, deserialize_with = "lenient_font")]
    pub terminal_font: Option<String>,
    /// Width of each commit history column. A value that is not an object
    /// keeps every default; a bad field keeps only its own default.
    #[serde(deserialize_with = "lenient_columns")]
    pub history_columns: HistoryColumns,
    /// Author avatars are downloaded from GitHub and Gravatar. Off draws
    /// initials only and sends nothing.
    #[serde(default = "default_author_avatars")]
    pub author_avatars: bool,
    /// A GitHub token was saved in the system keychain. The token itself is
    /// never written here; this flag only says whether to look for it, so a
    /// user without a token never triggers a keychain prompt.
    pub github_token_in_keyring: bool,
}

/// Older settings files omit the field. The terminal stays hidden until the
/// user shows it.
fn default_terminal_hidden() -> bool {
    true
}

fn default_author_avatars() -> bool {
    true
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
            history_sidebar_hidden: false,
            detail_sidebar_hidden: false,
            terminal_hidden: default_terminal_hidden(),
            ui_font: None,
            terminal_font: None,
            history_columns: HistoryColumns::default(),
            author_avatars: default_author_avatars(),
            github_token_in_keyring: false,
        }
    }
}

#[derive(Debug)]
pub enum SettingsError {
    Read(PathBuf, io::Error),
    Parse(PathBuf, serde_json::Error),
    Write(PathBuf, io::Error),
}

impl Settings {
    /// `settings.json` in the config folder the OS assigns to the app:
    /// - Linux: `$XDG_CONFIG_HOME/cthulhu-git/` (default `~/.config/cthulhu-git/`)
    /// - macOS: `~/Library/Application Support/io.github.don-linux.cthulhu-git/`
    /// - Windows: `%APPDATA%\don-linux\cthulhu-git\config\`
    ///
    /// `None` when the OS reports no home folder.
    pub fn default_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("io.github", "don-linux", "cthulhu-git")
            .map(|dirs| dirs.config_dir().join(FILE_NAME))
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
        let mut seen = Vec::with_capacity(self.recent_repositories.len());
        self.recent_repositories.retain(|path| {
            let new = !seen.contains(path);
            if new {
                seen.push(path.clone());
            }
            new
        });
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
        Some(truncate_to_bytes(stripped, MAX_FONT_NAME_BYTES))
    }
}

fn has_interior_newline(name: &str) -> bool {
    name.contains(['\n', '\r'])
}

fn is_bidi_control(ch: char) -> bool {
    matches!(ch, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

fn truncate_to_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
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

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(path, error) => {
                write!(
                    f,
                    "Could not read settings from {}: {error}",
                    path.display()
                )
            }
            Self::Parse(path, error) => write!(
                f,
                "Settings file {} is invalid ({error}); using the defaults and leaving the file as it is.",
                path.display()
            ),
            Self::Write(path, error) => {
                write!(f, "Could not save settings to {}: {error}", path.display())
            }
        }
    }
}

impl std::error::Error for SettingsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read(_, error) | Self::Write(_, error) => Some(error),
            Self::Parse(_, error) => Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn settings_path(dir: &TempDir) -> PathBuf {
        dir.path().join("nested").join(FILE_NAME)
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
            history_sidebar_hidden: true,
            ..Settings::default()
        }
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = TempDir::new().expect("temp dir");
        assert_eq!(
            Settings::load_from(&settings_path(&dir)).expect("load"),
            Settings::default()
        );
    }

    #[test]
    fn round_trip_creates_folders_and_leaves_no_temp_file() {
        let dir = TempDir::new().expect("temp dir");
        let path = settings_path(&dir);
        let mut settings = Settings {
            theme: Some("abyss".to_owned()),
            ..Settings::default()
        };
        settings.remember_repository(Path::new("/src/rlyeh"));

        settings.save_to(&path).expect("save");
        settings.save_to(&path).expect("save over an existing file");

        assert_eq!(Settings::load_from(&path).expect("load"), settings);
        let files: Vec<_> = fs::read_dir(path.parent().expect("parent"))
            .expect("read dir")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(files, [FILE_NAME]);
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
            (
                r#"{ "recent_repositories": [] }"#,
                Some(Settings::default()),
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
                    history_sidebar_hidden: true,
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
                    history_sidebar_hidden: true,
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
    fn remember_moves_to_front_without_duplicates() {
        let mut settings = Settings::default();
        settings.remember_repository(Path::new("/a"));
        settings.remember_repository(Path::new("/b"));
        settings.remember_repository(Path::new("/a"));
        assert_eq!(settings.last_repository.as_deref(), Some(Path::new("/a")));
        assert_eq!(
            settings.recent_repositories,
            [PathBuf::from("/a"), PathBuf::from("/b")]
        );
    }

    #[test]
    fn recents_are_capped() {
        let mut settings = Settings::default();
        for index in 0..MAX_RECENT_REPOSITORIES + 5 {
            settings.remember_repository(&PathBuf::from(format!("/repo-{index}")));
        }
        assert_eq!(settings.recent_repositories.len(), MAX_RECENT_REPOSITORIES);
        assert_eq!(
            settings.recent_repositories[0],
            PathBuf::from(format!("/repo-{}", MAX_RECENT_REPOSITORIES + 4))
        );
    }

    #[test]
    fn forgetting_keeps_the_recents() {
        let mut settings = Settings::default();
        settings.remember_repository(Path::new("/a"));
        settings.forget_last_repository();
        assert_eq!(settings.last_repository, None);
        assert_eq!(settings.recent_repositories, [PathBuf::from("/a")]);
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

    #[test]
    fn default_path_is_an_absolute_settings_file() {
        if let Some(path) = Settings::default_path() {
            assert!(path.is_absolute(), "{}", path.display());
            assert_eq!(path.file_name(), Some(FILE_NAME.as_ref()));
            assert!(path.to_string_lossy().contains("cthulhu-git"));
        }
    }
}
