//! Adversarial checks for settings load and save.
//!
//! Every path is under a `tempfile` directory. These tests never call
//! `Settings::default_path()` and never touch the real config folder.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Barrier, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use cthulhu_git::settings::{FILE_NAME, MAX_RECENT_REPOSITORIES, Settings, SettingsError};
use tempfile::TempDir;

#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};

const FIVE_MIB: usize = 5 * 1024 * 1024;

fn temp_dir() -> TempDir {
    TempDir::with_prefix("cthulhu-settings-adv-").expect("temp dir")
}

fn settings_path(dir: &TempDir) -> PathBuf {
    dir.path().join("nested").join(FILE_NAME)
}

fn write_bytes(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent");
    }
    fs::write(path, bytes).expect("write");
}

fn entry_names(dir: &Path) -> Vec<String> {
    let mut names = fs::read_dir(dir)
        .expect("read dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn pid_temp_path(path: &Path) -> PathBuf {
    path.with_extension(format!("json.{}.tmp", std::process::id()))
}

/// Quote `text` as a JSON string. Windows temp paths contain backslashes.
fn json_string(text: &str) -> String {
    serde_json::to_string(text).expect("json string")
}

#[track_caller]
fn assert_parse(error: &SettingsError) {
    assert!(matches!(error, SettingsError::Parse(..)), "{error:?}");
    let message = error.to_string();
    assert!(
        message.contains("using the defaults until a setting changes."),
        "{message}"
    );
}

#[track_caller]
fn assert_read_or_write(error: &SettingsError) {
    assert!(
        matches!(error, SettingsError::Read(..) | SettingsError::Write(..)),
        "{error:?}"
    );
}

fn flag_torn(torn: &Mutex<Option<String>>, detail: String) {
    let mut slot = torn.lock().expect("torn lock");
    if slot.is_none() {
        let clipped: String = detail.chars().take(240).collect();
        *slot = Some(clipped);
    }
}

struct Finish<'a>(&'a AtomicUsize);

impl Drop for Finish<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Release);
    }
}

fn note_if_torn(path: &Path, torn: &Mutex<Option<String>>) {
    match Settings::load_from(path) {
        Ok(settings) => {
            if settings.recent_repositories.len() > MAX_RECENT_REPOSITORIES {
                flag_torn(
                    torn,
                    format!(
                        "recent list longer than {MAX_RECENT_REPOSITORIES}: {}",
                        settings.recent_repositories.len()
                    ),
                );
            }
        }
        Err(error @ SettingsError::Parse(_, _)) => {
            let bytes = fs::read(path).unwrap_or_default();
            let sample: String = String::from_utf8_lossy(&bytes).chars().take(120).collect();
            flag_torn(
                torn,
                format!("{error}; len {} sample {sample:?}", bytes.len()),
            );
        }
        Err(SettingsError::Read(_, error))
            if error.kind() == ErrorKind::NotFound || error.kind() == ErrorKind::Interrupted => {}
        Err(error) => flag_torn(torn, error.to_string()),
    }
}

#[cfg(unix)]
struct RestoreMode<'a> {
    path: &'a Path,
    mode: u32,
}

#[cfg(unix)]
impl Drop for RestoreMode<'_> {
    fn drop(&mut self) {
        let _ = fs::set_permissions(self.path, fs::Permissions::from_mode(self.mode));
    }
}

#[test]
fn utf8_bom_prefixed_object_loads() {
    let dir = temp_dir();
    let path = settings_path(&dir);
    let mut bytes = b"\xEF\xBB\xBF".to_vec();
    bytes.extend_from_slice(
        br#"{
  "theme": "abyss",
  "last_repository": "/src/rlyeh",
  "recent_repositories": ["/src/rlyeh", "/src/innsmouth"],
  "history_sidebar_hidden": true
}"#,
    );
    write_bytes(&path, &bytes);

    // A failed parse must still leave the user-edited file alone.
    let loaded = Settings::load_from(&path);
    assert_eq!(fs::read(&path).expect("read back"), bytes);
    let loaded = loaded.expect("a UTF-8 BOM before a valid settings object must load");
    assert_eq!(loaded.theme.as_deref(), Some("abyss"));
    assert_eq!(
        loaded.last_repository.as_deref(),
        Some(Path::new("/src/rlyeh"))
    );
    assert_eq!(
        loaded.recent_repositories,
        [PathBuf::from("/src/rlyeh"), PathBuf::from("/src/innsmouth")]
    );
    assert!(loaded.history_sidebar_hidden);
}

#[test]
fn null_option_fields_load_as_none() {
    let dir = temp_dir();
    let path = settings_path(&dir);
    write_bytes(&path, br#"{ "theme": null, "last_repository": null }"#);

    let loaded = Settings::load_from(&path).expect("null option fields");
    assert_eq!(loaded.theme, None);
    assert_eq!(loaded.last_repository, None);
    assert!(loaded.recent_repositories.is_empty());
    assert!(!loaded.history_sidebar_hidden);
    assert!(!loaded.detail_sidebar_hidden);
    assert!(loaded.terminal_hidden);
}

#[test]
fn null_recent_repositories_load_as_defaults() {
    let dir = temp_dir();
    let path = settings_path(&dir);
    // `null` on an option is "no value". `null` on the recent list is the same
    // idea as omitting it: an empty list, not a corrupt file.
    let original = br#"{
  "theme": null,
  "last_repository": null,
  "recent_repositories": null
}"#;
    write_bytes(&path, original);

    let loaded = Settings::load_from(&path);
    assert_eq!(fs::read(&path).expect("read back"), original);
    let loaded = loaded.expect("null fields should load as defaults");
    assert_eq!(loaded, Settings::default());
    assert!(!loaded.history_sidebar_hidden);
}

#[test]
fn wrong_types_are_parse_errors_and_leave_bytes_unchanged() {
    let samples: [&[u8]; 3] = [
        br#"{ "theme": 1 }"#,
        br#"{ "recent_repositories": "/tmp/not-a-list" }"#,
        br#"{ "theme": 7, "recent_repositories": "nope" }"#,
    ];
    for original in samples {
        let dir = temp_dir();
        let path = settings_path(&dir);
        write_bytes(&path, original);

        let error = Settings::load_from(&path).expect_err("wrong types");
        assert_parse(&error);
        assert_eq!(fs::read(&path).expect("read back"), original);
        assert_eq!(
            entry_names(path.parent().expect("parent")),
            vec![FILE_NAME.to_owned()]
        );
    }
}

#[test]
fn five_mib_of_open_braces_is_an_error() {
    let dir = temp_dir();
    let path = settings_path(&dir);
    let bytes = vec![b'{'; FIVE_MIB];
    write_bytes(&path, &bytes);

    let started = Instant::now();
    let loaded = std::panic::catch_unwind(|| Settings::load_from(&path));
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "load took {elapsed:?}; a 5 MiB brace file must fail quickly"
    );
    match loaded {
        Ok(Err(error)) => assert_parse(&error),
        Ok(Ok(settings)) => panic!("deeply nested braces loaded: {settings:?}"),
        Err(_) => panic!("load panicked on a 5 MiB run of '{{'"),
    }
    assert_eq!(fs::read(&path).expect("read back"), bytes);
}

#[test]
fn five_mib_json_string_is_an_error() {
    let dir = temp_dir();
    let path = settings_path(&dir);
    let mut bytes = Vec::with_capacity(FIVE_MIB + 2);
    bytes.push(b'"');
    bytes.resize(FIVE_MIB + 1, b'a');
    bytes.push(b'"');
    write_bytes(&path, &bytes);

    let started = Instant::now();
    let loaded = std::panic::catch_unwind(|| Settings::load_from(&path));
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "load took {elapsed:?}; a 5 MiB JSON string must fail quickly"
    );
    match loaded {
        Ok(Err(error)) => assert_parse(&error),
        Ok(Ok(settings)) => panic!("huge JSON string loaded as settings: {settings:?}"),
        Err(_) => panic!("load panicked on a 5 MiB JSON string"),
    }
    assert_eq!(fs::read(&path).expect("read back"), bytes);
}

/// Contract for a `settings.json` that is a symlink:
/// `save_to` may replace the symlink (rename does not follow links) but must
/// not write through it. A target outside the config directory keeps its
/// original bytes, the save succeeds, and `load_from` on the settings path
/// returns the settings that were just saved.
#[cfg(unix)]
#[test]
fn save_replaces_settings_symlink_without_changing_outside_target() {
    let config = temp_dir();
    let outside = temp_dir();
    let path = config.path().join(FILE_NAME);
    let target = outside.path().join("precious.txt");
    let original = b"precious-outside-target\n";
    assert!(target.is_absolute());
    assert!(!target.starts_with(config.path()));
    fs::write(&target, original).expect("target");
    symlink(&target, &path).expect("symlink");
    assert_eq!(fs::read_link(&path).expect("readlink"), target);

    let mut settings = Settings {
        theme: Some("abyss".to_owned()),
        ..Settings::default()
    };
    settings.remember_repository(Path::new("/src/rlyeh"));

    let result = settings.save_to(&path);
    assert_eq!(
        fs::read(&target).expect("target after save"),
        original,
        "save_to must not modify a symlink target outside the config directory"
    );
    result.expect("save replaces the settings symlink");
    assert_eq!(Settings::load_from(&path).expect("load"), settings);
}

/// A stale `settings.json.<pid>.tmp` that points outside the config directory
/// must not be opened with follow-and-truncate. Save still replaces that
/// temp entry, leaves no temp file, and the settings path loads the new value.
#[cfg(unix)]
#[test]
fn save_does_not_follow_temp_symlink_onto_outside_target() {
    let config = temp_dir();
    let outside = temp_dir();
    let path = config.path().join(FILE_NAME);
    let target = outside.path().join("precious.txt");
    let original = b"precious-outside-target\n";
    assert!(target.is_absolute());
    assert!(!target.starts_with(config.path()));
    fs::write(&target, original).expect("target");

    let tmp = pid_temp_path(&path);
    assert_eq!(
        tmp.file_name().and_then(|name| name.to_str()),
        Some(format!("settings.json.{}.tmp", std::process::id())).as_deref()
    );
    symlink(&target, &tmp).expect("temp symlink");
    assert!(
        fs::symlink_metadata(&tmp)
            .expect("temp meta")
            .file_type()
            .is_symlink()
    );

    let mut settings = Settings {
        theme: Some("abyss".to_owned()),
        ..Settings::default()
    };
    settings.remember_repository(Path::new("/src/rlyeh"));

    let result = settings.save_to(&path);
    assert_eq!(
        fs::read(&target).expect("target after save"),
        original,
        "save_to followed the temp symlink and modified a file outside the config directory"
    );
    result.expect("stale temp symlink is replaced and save succeeds");
    assert!(!tmp.exists(), "temp path must not remain");
    assert_eq!(Settings::load_from(&path).expect("load"), settings);
}

#[test]
fn directory_settings_path_is_read_or_write_error() {
    let dir = temp_dir();
    let path = settings_path(&dir);
    fs::create_dir_all(&path).expect("settings path is a directory");
    assert!(path.is_dir());

    let load_error = Settings::load_from(&path).expect_err("load directory");
    assert_read_or_write(&load_error);
    assert!(
        path.is_dir(),
        "failed load must leave the directory in place"
    );

    let save_error = Settings::default()
        .save_to(&path)
        .expect_err("save directory");
    assert_read_or_write(&save_error);
    assert!(
        path.is_dir(),
        "failed save must leave the directory in place"
    );
}

#[cfg(unix)]
#[test]
fn unwritable_parent_directory_is_a_write_error() {
    let dir = temp_dir();
    let parent = dir.path().join("locked");
    fs::create_dir(&parent).expect("parent");
    let path = parent.join(FILE_NAME);
    let mode = fs::metadata(&parent).expect("meta").permissions().mode();
    // Restored on drop, including assertion failures, so TempDir can remove it.
    let _restore = RestoreMode {
        path: &parent,
        mode,
    };
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o555)).expect("chmod");
    assert_eq!(
        fs::metadata(&parent).expect("meta").permissions().mode() & 0o777,
        0o555
    );

    let error = Settings::default()
        .save_to(&path)
        .expect_err("save into a read-only parent");
    assert!(matches!(error, SettingsError::Write(..)), "{error:?}");
    assert!(!path.exists(), "failed save must not create settings.json");
    assert!(
        !pid_temp_path(&path).exists(),
        "failed save must not leave a temp file"
    );
}

#[test]
fn stale_temp_file_is_overwritten_and_removed() {
    let dir = temp_dir();
    let path = settings_path(&dir);
    let parent = path.parent().expect("parent");
    fs::create_dir_all(parent).expect("parent");
    let tmp = pid_temp_path(&path);
    fs::write(&tmp, b"STALE-TEMP-NOT-JSON").expect("stale temp");

    let mut settings = Settings::default();
    settings.remember_repository(Path::new("/src/rlyeh"));
    settings
        .save_to(&path)
        .expect("save over a stale temp file");

    assert!(!tmp.exists(), "temp file must not remain");
    assert_eq!(entry_names(parent), vec![FILE_NAME.to_owned()]);
    assert_eq!(Settings::load_from(&path).expect("load"), settings);
}

#[test]
fn concurrent_saves_leave_valid_json() {
    let dir = temp_dir();
    let path = settings_path(&dir);
    Settings::default().save_to(&path).expect("seed");

    let start = Barrier::new(2);
    let finished = AtomicUsize::new(0);
    let torn = Mutex::new(None);
    // One 50-save burst often ends on a clean rename, which hides an earlier
    // torn publish. Repeat the burst so a shared temp file cannot slip through.
    const BURSTS: usize = 16;
    thread::scope(|scope| {
        scope.spawn(|| {
            loop {
                note_if_torn(&path, &torn);
                if finished.load(Ordering::Acquire) >= 2 {
                    break;
                }
            }
            note_if_torn(&path, &torn);
        });
        scope.spawn(|| {
            let _done = Finish(&finished);
            start.wait();
            for _burst in 0..BURSTS {
                if torn.lock().expect("torn lock").is_some() {
                    break;
                }
                for iteration in 0..50 {
                    let mut settings = Settings::default();
                    for slot in 0..MAX_RECENT_REPOSITORIES {
                        settings.remember_repository(&PathBuf::from(format!(
                            "/thread-a/iteration-{iteration}/slot-{slot}-{}",
                            "n".repeat(8192)
                        )));
                    }
                    let _ = settings.save_to(&path);
                }
            }
        });
        scope.spawn(|| {
            let _done = Finish(&finished);
            start.wait();
            for _burst in 0..BURSTS {
                if torn.lock().expect("torn lock").is_some() {
                    break;
                }
                for iteration in 0..50 {
                    let mut settings = Settings::default();
                    settings.remember_repository(Path::new(&format!("/b{iteration}")));
                    let _ = settings.save_to(&path);
                }
            }
        });
    });

    let observed = torn.lock().expect("torn lock").clone();
    if let Some(detail) = observed {
        panic!("torn settings observed (lost updates are ok; torn JSON is not): {detail}");
    }
    let loaded = Settings::load_from(&path).expect("final settings are valid json");
    assert!(loaded.recent_repositories.len() <= MAX_RECENT_REPOSITORIES);
}

#[test]
fn hand_written_odd_paths_are_kept() {
    let dir = temp_dir();
    let path = settings_path(&dir);
    let notes = dir.path().join("readme.txt");
    fs::write(&notes, b"not a repository").expect("file");
    assert!(notes.is_file());
    let notes_json = json_string(notes.to_str().expect("utf-8 temp path"));
    let body = format!(
        r#"{{"theme":null,"last_repository":{notes_json},"recent_repositories":["rel/repo","rel/repo","C:\\repo","C:\\repo"],"history_sidebar_hidden":true}}"#
    );
    write_bytes(&path, body.as_bytes());

    let loaded = Settings::load_from(&path).expect("odd paths should load");
    assert_eq!(loaded.last_repository.as_deref(), Some(notes.as_path()));
    assert!(loaded.last_repository.as_ref().expect("last").is_file());
    assert_eq!(
        loaded.recent_repositories,
        [PathBuf::from("rel/repo"), PathBuf::from(r"C:\repo")]
    );
    assert!(loaded.history_sidebar_hidden);

    loaded.save_to(&path).expect("round trip");
    assert_eq!(Settings::load_from(&path).expect("reload"), loaded);
}

#[test]
fn remembering_one_path_twice_keeps_a_single_recent() {
    let mut settings = Settings::default();
    let windows = Path::new(r"C:\repo");
    settings.remember_repository(windows);
    settings.remember_repository(windows);
    assert_eq!(settings.last_repository.as_deref(), Some(windows));
    assert_eq!(settings.recent_repositories, [PathBuf::from(windows)]);

    let relative = Path::new("rel/repo");
    settings.remember_repository(relative);
    settings.remember_repository(relative);
    assert_eq!(settings.last_repository.as_deref(), Some(relative));
    assert_eq!(settings.recent_repositories[0], relative);
    assert_eq!(
        settings
            .recent_repositories
            .iter()
            .filter(|recent| *recent == relative)
            .count(),
        1
    );
    assert_eq!(settings.recent_repositories.len(), 2);
}

#[test]
fn history_sidebar_hidden_round_trips() {
    let dir = temp_dir();
    let path = settings_path(&dir);
    for hidden in [false, true, false] {
        let settings = Settings {
            theme: Some("abyss".to_owned()),
            history_sidebar_hidden: hidden,
            ..Settings::default()
        };
        settings.save_to(&path).expect("save");
        assert_eq!(Settings::load_from(&path).expect("load"), settings);
    }
}

#[test]
fn detail_sidebar_hidden_round_trips() {
    let dir = temp_dir();
    let path = settings_path(&dir);
    for hidden in [false, true, false] {
        let settings = Settings {
            theme: Some("abyss".to_owned()),
            detail_sidebar_hidden: hidden,
            ..Settings::default()
        };
        settings.save_to(&path).expect("save");
        assert_eq!(Settings::load_from(&path).expect("load"), settings);
    }
}

#[test]
fn terminal_hidden_round_trips_and_old_files_stay_hidden() {
    let dir = temp_dir();
    let path = settings_path(&dir);
    write_bytes(&path, br#"{ "theme": "abyss" }"#);
    assert!(
        Settings::load_from(&path)
            .expect("old file")
            .terminal_hidden
    );

    for hidden in [false, true, false] {
        let settings = Settings {
            theme: Some("abyss".to_owned()),
            terminal_hidden: hidden,
            ..Settings::default()
        };
        settings.save_to(&path).expect("save");
        assert_eq!(Settings::load_from(&path).expect("load"), settings);
    }
}
