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

/// Every field has a default, so files written by older or newer versions
/// still load: missing fields take the default and unknown ones are ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
        let mut json = serde_json::to_vec_pretty(self).map_err(|error| fail(error.into()))?;
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
