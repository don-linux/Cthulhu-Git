//! Adversarial checks for settings load and save.
//!
//! Every path is under a `tempfile` directory. These tests never call
//! `Settings::default_path()` and never touch the real config folder.

#[path = "common/mod.rs"]
mod common;

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

#[track_caller]
fn assert_parse(error: &SettingsError) {
    assert!(matches!(error, SettingsError::Parse(..)), "{error:?}");
    let message = error.to_string();
    assert!(
        message.contains("using the defaults and leaving the file as it is."),
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

/// A save meets an entry planted next to `settings.json`: a symlink at the
/// settings path or at a leftover temp name, pointing outside the config
/// directory, or a leftover temp file. The save succeeds, never writes through
/// a link (rename(2) replaces it; the temp file gets a fresh name and is
/// created exclusively), adds nothing but `settings.json`, and loads back.
#[test]
fn save_never_writes_through_a_planted_entry() {
    let leftover = format!("{FILE_NAME}.{}.tmp", std::process::id());
    // (planted name, whether it is a symlink to the outside file)
    #[cfg(unix)]
    let cases = [(FILE_NAME, true), (&*leftover, true), (&*leftover, false)];
    #[cfg(not(unix))]
    let cases = [(&*leftover, false)];
    for (name, link) in cases {
        let config = temp_dir();
        let outside = temp_dir();
        let path = config.path().join(FILE_NAME);
        let target = outside.path().join("precious.txt");
        let original = b"precious-outside-target\n";
        fs::write(&target, original).expect("target");
        let planted = config.path().join(name);
        if link {
            #[cfg(unix)]
            symlink(&target, &planted).expect("symlink");
        } else {
            fs::write(&planted, b"STALE-TEMP-NOT-JSON").expect("leftover temp");
        }

        let mut settings = Settings {
            theme: Some("abyss".to_owned()),
            ..Settings::default()
        };
        settings.remember_repository(Path::new("/src/rlyeh"));
        let result = settings.save_to(&path);

        let case = format!("{name} link={link}");
        assert_eq!(fs::read(&target).expect("target"), original, "{case}");
        result.expect(&case);
        let mut expected = vec![FILE_NAME.to_owned(), name.to_owned()];
        expected.dedup();
        assert_eq!(entry_names(config.path()), expected, "{case}");
        assert_eq!(Settings::load_from(&path).expect(&case), settings, "{case}");
    }
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
    assert_eq!(
        entry_names(path.parent().expect("parent")),
        [FILE_NAME],
        "failed save must not leave a temp file"
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
    common::strip_acls(&parent);
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o555)).expect("chmod");
    assert_eq!(
        fs::metadata(&parent).expect("meta").permissions().mode() & 0o777,
        0o555
    );
    // Filesystem grants access despite mode 0555 (an ACL the test cannot strip).
    let probe = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path);
    if probe.is_ok() {
        let _ = fs::remove_file(&path);
        return;
    }

    let error = Settings::default()
        .save_to(&path)
        .expect_err("save into a read-only parent");
    assert!(matches!(error, SettingsError::Write(..)), "{error:?}");
    assert!(
        entry_names(&parent).is_empty(),
        "failed save must leave neither settings.json nor a temp file"
    );
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
