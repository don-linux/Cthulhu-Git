//! User settings, stored as one JSON file in the per-user config folder of
//! each OS. The map for changing this module is `docs/SETTINGS.md`.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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

    /// A missing file gives the defaults; an unreadable or invalid one is an error.
    pub fn load_from(path: &Path) -> Result<Self, SettingsError> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(SettingsError::Read(path.to_path_buf(), error)),
        };
        // Notepad on Windows writes a UTF-8 BOM in front of otherwise valid JSON.
        let json = if bytes.starts_with(b"\xEF\xBB\xBF") {
            &bytes[3..]
        } else {
            bytes.as_slice()
        };
        let mut settings: Self = serde_json::from_slice(json)
            .map_err(|error| SettingsError::Parse(path.to_path_buf(), error))?;
        settings.normalize_recent();
        settings.normalize_fonts();
        Ok(settings)
    }

    /// Writes a temporary file next to `path` and renames it over `path`, so
    /// a crash never leaves a half-written file. `std::fs::rename` replaces
    /// the destination on Linux, macOS and Windows alike.
    pub fn save_to(&self, path: &Path) -> Result<(), SettingsError> {
        // One temp name per process. Serializing saves keeps two threads from
        // truncating that same file and publishing a mix of both documents.
        let _guard = save_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let fail = |error: io::Error| SettingsError::Write(path.to_path_buf(), error);

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(fail)?;
        }
        // A blank id is not a font. Writing the normalized copy keeps a
        // direct field assignment consistent with `load_from`.
        let mut stored = self.clone();
        stored.normalize_fonts();
        stored.history_columns = stored.history_columns.normalized();
        let mut json = serde_json::to_vec_pretty(&stored).map_err(|error| fail(error.into()))?;
        json.push(b'\n');

        // The process id keeps two running instances from sharing a temp file.
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        // `create_new` fails on a symlink instead of following it. Unlink first
        // so a planted temp symlink cannot truncate a file outside this directory.
        if fs::symlink_metadata(&tmp).is_ok() {
            fs::remove_file(&tmp).map_err(fail)?;
        }
        let written = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .and_then(|mut file| {
                file.write_all(&json)?;
                file.sync_all()
            });
        if let Err(error) = written.and_then(|()| fs::rename(&tmp, path)) {
            let _ = fs::remove_file(&tmp);
            return Err(fail(error));
        }
        Ok(())
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

fn save_lock() -> &'static Mutex<()> {
    static LOCK: Mutex<()> = Mutex::new(());
    &LOCK
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
                "Settings file {} is invalid ({error}); using the defaults until a setting changes.",
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

    #[test]
    fn corrupt_file_is_an_error() {
        let dir = TempDir::new().expect("temp dir");
        let path = settings_path(&dir);
        fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        fs::write(&path, "{ not json").expect("write");
        let error = Settings::load_from(&path).expect_err("corrupt");
        assert!(matches!(error, SettingsError::Parse(..)), "{error:?}");
    }

    #[test]
    fn missing_and_unknown_fields_are_tolerated() {
        let dir = TempDir::new().expect("temp dir");
        let path = settings_path(&dir);
        fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        fs::write(&path, r#"{ "theme": "abyss", "from_the_future": 1 }"#).expect("write");
        assert_eq!(
            Settings::load_from(&path).expect("load"),
            Settings {
                theme: Some("abyss".to_owned()),
                ..Settings::default()
            }
        );
    }

    #[test]
    fn history_sidebar_shows_unless_saved_hidden() {
        let dir = TempDir::new().expect("temp dir");
        let path = settings_path(&dir);
        fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        fs::write(&path, r#"{ "recent_repositories": [] }"#).expect("write");
        assert!(
            !Settings::load_from(&path)
                .expect("load")
                .history_sidebar_hidden
        );

        let settings = Settings {
            history_sidebar_hidden: true,
            ..Settings::default()
        };
        settings.save_to(&path).expect("save");
        assert!(
            Settings::load_from(&path)
                .expect("load")
                .history_sidebar_hidden
        );
    }

    #[test]
    fn detail_sidebar_shows_unless_saved_hidden() {
        let dir = TempDir::new().expect("temp dir");
        let path = settings_path(&dir);
        fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        fs::write(&path, r#"{ "recent_repositories": [] }"#).expect("write");
        assert!(
            !Settings::load_from(&path)
                .expect("load")
                .detail_sidebar_hidden
        );

        let settings = Settings {
            detail_sidebar_hidden: true,
            ..Settings::default()
        };
        settings.save_to(&path).expect("save");
        assert!(
            Settings::load_from(&path)
                .expect("load")
                .detail_sidebar_hidden
        );
    }

    #[test]
    fn terminal_font_trims_and_old_files_keep_the_default() {
        let dir = TempDir::new().expect("temp dir");
        let path = settings_path(&dir);
        fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        fs::write(&path, r#"{ "theme": "abyss" }"#).expect("write");
        assert_eq!(
            Settings::load_from(&path).expect("old file").terminal_font,
            None
        );

        let settings = Settings {
            terminal_font: Some("JetBrains Mono".to_owned()),
            ..Settings::default()
        };
        settings.save_to(&path).expect("save");
        assert_eq!(Settings::load_from(&path).expect("load"), settings);

        for (json, expected) in [
            (r#"{ "terminal_font": "  Fira Code  " }"#, Some("Fira Code")),
            (r#"{ "terminal_font": "   " }"#, None),
            (r#"{ "terminal_font": "" }"#, None),
            (r#"{ "terminal_font": null }"#, None),
        ] {
            fs::write(&path, json).expect("write");
            assert_eq!(
                Settings::load_from(&path)
                    .expect("load")
                    .terminal_font
                    .as_deref(),
                expected,
                "{json}"
            );
        }

        let padded = Settings {
            terminal_font: Some("  Fira Code  ".to_owned()),
            ..Settings::default()
        };
        padded.save_to(&path).expect("save");
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read")).expect("json");
        assert_eq!(value["terminal_font"], "Fira Code");

        let blank = Settings {
            terminal_font: Some("   ".to_owned()),
            ..Settings::default()
        };
        blank.save_to(&path).expect("save blank");
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read")).expect("json");
        assert!(value["terminal_font"].is_null());
        assert_eq!(
            Settings::load_from(&path).expect("load").terminal_font,
            None
        );

        let mut settings = Settings::default();
        settings.set_terminal_font(Some("  ".to_owned()));
        assert_eq!(settings.terminal_font, None);
        settings.set_terminal_font(Some("  IBM Plex Mono  ".to_owned()));
        assert_eq!(settings.terminal_font.as_deref(), Some("IBM Plex Mono"));
    }

    #[test]
    fn ui_font_trims_and_old_files_keep_the_default() {
        let dir = TempDir::new().expect("temp dir");
        let path = settings_path(&dir);
        fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        fs::write(&path, r#"{ "theme": "abyss" }"#).expect("write");
        assert_eq!(Settings::load_from(&path).expect("old file").ui_font, None);

        let settings = Settings {
            ui_font: Some("inter".to_owned()),
            ..Settings::default()
        };
        settings.save_to(&path).expect("save");
        assert_eq!(Settings::load_from(&path).expect("load"), settings);

        let padded = Settings {
            ui_font: Some("  aspekta  ".to_owned()),
            ..Settings::default()
        };
        padded.save_to(&path).expect("save");
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read")).expect("json");
        assert_eq!(value["ui_font"], "aspekta");

        let mut settings = Settings::default();
        settings.set_ui_font(Some("   ".to_owned()));
        assert_eq!(settings.ui_font, None);
        settings.set_ui_font(Some("  obel-mono  ".to_owned()));
        assert_eq!(settings.ui_font.as_deref(), Some("obel-mono"));
    }

    #[test]
    fn bad_font_field_keeps_the_theme() {
        let dir = TempDir::new().expect("temp dir");
        let path = settings_path(&dir);
        fs::create_dir_all(path.parent().expect("parent")).expect("dir");

        fs::write(&path, r#"{"ui_font": 1, "theme": "abyss"}"#).expect("write");
        let loaded = Settings::load_from(&path).expect("load");
        assert_eq!(loaded.theme.as_deref(), Some("abyss"));
        assert_eq!(loaded.ui_font, None);

        fs::write(
            &path,
            r#"{"terminal_font": [], "theme": "abyss", "recent_repositories": ["/src/rlyeh"]}"#,
        )
        .expect("write");
        let loaded = Settings::load_from(&path).expect("load");
        assert_eq!(loaded.theme.as_deref(), Some("abyss"));
        assert_eq!(loaded.terminal_font, None);
        assert_eq!(
            loaded.recent_repositories,
            vec![PathBuf::from("/src/rlyeh")]
        );

        fs::write(&path, r#"{"ui_font": "Comic Sans", "theme": "abyss"}"#).expect("write");
        let loaded = Settings::load_from(&path).expect("load");
        assert_eq!(loaded.ui_font.as_deref(), Some("Comic Sans"));
        assert_eq!(loaded.theme.as_deref(), Some("abyss"));
    }

    #[test]
    fn hostile_font_ids_round_trip_and_unknown_names_stay_stored() {
        let dir = TempDir::new().expect("temp dir");
        let path = settings_path(&dir);
        let mut settings = Settings::default();
        settings.set_ui_font(Some("  obel-mono \n".to_owned()));
        settings.set_terminal_font(Some("  Fira Code \n".to_owned()));
        settings.save_to(&path).expect("save");

        let loaded = Settings::load_from(&path).expect("load");
        assert_eq!(loaded.ui_font.as_deref(), Some("obel-mono"));
        assert_eq!(loaded.terminal_font.as_deref(), Some("Fira Code"));
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read")).expect("json");
        assert_eq!(value["ui_font"], "obel-mono");
        assert_eq!(value["terminal_font"], "Fira Code");
    }

    #[test]
    fn hostile_font_names_are_stored_sanitized() {
        let dir = TempDir::new().expect("temp dir");
        let path = settings_path(&dir);
        let huge = "a".repeat(100_000);
        let mut settings = Settings::default();
        settings.set_ui_font(Some(huge.clone()));
        assert_eq!(settings.ui_font.as_deref(), Some("a".repeat(128).as_str()));
        assert_ne!(settings.ui_font.as_deref(), Some(huge.as_str()));

        settings.set_terminal_font(Some("\u{202E}obel-mono".to_owned()));
        let stored = settings.terminal_font.clone().expect("sanitized");
        assert_eq!(stored, "obel-mono");
        assert!(!stored.contains('\u{202E}'));

        settings.set_ui_font(Some("obel\nmono".to_owned()));
        assert_eq!(settings.ui_font, None);
        settings.set_terminal_font(Some("fira\rmono".to_owned()));
        assert_eq!(settings.terminal_font, None);
        settings.set_terminal_font(Some("\u{202E}".to_owned()));
        assert_eq!(settings.terminal_font, None);

        let accented = "é".repeat(100);
        settings.set_ui_font(Some(accented));
        let stored = settings.ui_font.clone().expect("truncated");
        assert_eq!(stored.len(), 128);
        assert_eq!(stored, "é".repeat(64));

        settings.set_ui_font(Some(huge));
        settings.set_terminal_font(Some("meslo\u{202E}".to_owned()));
        settings.save_to(&path).expect("save");
        let text = fs::read_to_string(&path).expect("read");
        assert!(!text.contains(&"a".repeat(200)));
        assert!(!text.contains('\u{202E}'));
        let loaded = Settings::load_from(&path).expect("load");
        assert_eq!(loaded.ui_font.as_deref(), Some("a".repeat(128).as_str()));
        assert_eq!(loaded.terminal_font.as_deref(), Some("meslo"));

        fs::write(
            &path,
            "{\"ui_font\":\"obel\\nmono\",\"terminal_font\":\"\\u202EFira\",\"theme\":\"abyss\"}\n",
        )
        .expect("write");
        let loaded = Settings::load_from(&path).expect("load");
        assert_eq!(loaded.theme.as_deref(), Some("abyss"));
        assert_eq!(loaded.ui_font, None);
        assert_eq!(loaded.terminal_font.as_deref(), Some("Fira"));
    }

    #[test]
    fn terminal_stays_hidden_unless_saved_open() {
        let dir = TempDir::new().expect("temp dir");
        let path = settings_path(&dir);
        fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        fs::write(&path, r#"{ "recent_repositories": [] }"#).expect("write");
        assert!(Settings::load_from(&path).expect("load").terminal_hidden);

        for hidden in [false, true, false] {
            let settings = Settings {
                terminal_hidden: hidden,
                ..Settings::default()
            };
            settings.save_to(&path).expect("save");
            assert_eq!(
                Settings::load_from(&path).expect("load").terminal_hidden,
                hidden
            );
        }
    }

    #[test]
    fn older_files_get_the_history_and_avatar_defaults() {
        let dir = TempDir::new().expect("temp dir");
        let path = settings_path(&dir);
        fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        fs::write(&path, r#"{ "theme": "abyss", "terminal_hidden": false }"#).expect("write");
        let loaded = Settings::load_from(&path).expect("load");
        assert_eq!(loaded.history_columns, HistoryColumns::default());
        assert!(loaded.author_avatars);
        assert!(!loaded.github_token_in_keyring);
    }

    #[test]
    fn default_history_columns_fill_the_panel() {
        let columns = HistoryColumns::default();
        let sum = columns.branch + columns.graph + columns.hash + columns.message + columns.author;
        assert_eq!(sum, 1000);
    }

    #[test]
    fn history_columns_round_trip_and_stay_independent() {
        let dir = TempDir::new().expect("temp dir");
        let path = settings_path(&dir);
        let settings = Settings {
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
        };
        settings.save_to(&path).expect("save");
        assert_eq!(Settings::load_from(&path).expect("load"), settings);
        let text = fs::read_to_string(&path).expect("read");
        assert!(
            !text.contains("token\":"),
            "no token value is stored: {text}"
        );
    }

    #[test]
    fn a_bad_column_keeps_only_its_own_default() {
        let dir = TempDir::new().expect("temp dir");
        let path = settings_path(&dir);
        fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        let defaults = HistoryColumns::default();
        fs::write(
            &path,
            r#"{ "history_columns": { "branch": 0, "graph": 5000, "hash": "wide",
                 "message": -3, "author": 250, "future_column": 7 } }"#,
        )
        .expect("write");
        assert_eq!(
            Settings::load_from(&path).expect("load").history_columns,
            HistoryColumns {
                author: 250,
                ..defaults
            }
        );

        fs::write(&path, r#"{ "history_columns": { "graph": 333 } }"#).expect("write");
        assert_eq!(
            Settings::load_from(&path).expect("load").history_columns,
            HistoryColumns {
                graph: 333,
                ..defaults
            }
        );

        for json in [
            r#"{ "history_columns": null }"#,
            r#"{ "history_columns": 12 }"#,
            r#"{ "history_columns": [1, 2, 3] }"#,
        ] {
            fs::write(&path, json).expect("write");
            assert_eq!(
                Settings::load_from(&path).expect(json).history_columns,
                defaults,
                "{json}"
            );
        }

        let out_of_range = Settings {
            history_columns: HistoryColumns {
                message: 0,
                ..defaults
            },
            ..Settings::default()
        };
        out_of_range.save_to(&path).expect("save");
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read")).expect("json");
        assert_eq!(value["history_columns"]["message"], defaults.message);
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
    fn loading_dedupes_and_caps_hand_edited_recents() {
        let dir = TempDir::new().expect("temp dir");
        let path = settings_path(&dir);
        let mut recents: Vec<String> = (0..20).map(|index| format!("/repo-{index}")).collect();
        recents.insert(1, "/repo-0".to_owned());
        fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        fs::write(
            &path,
            serde_json::json!({ "recent_repositories": recents }).to_string(),
        )
        .expect("write");

        let loaded = Settings::load_from(&path).expect("load");
        assert_eq!(loaded.recent_repositories.len(), MAX_RECENT_REPOSITORIES);
        assert_eq!(loaded.recent_repositories[1], PathBuf::from("/repo-1"));
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
