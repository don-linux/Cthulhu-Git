//! Settings files on disk: invalid, planted next to, unusable, or raced.
//!
//! Every path is under a `tempfile` directory. Nothing here reads or writes
//! `Settings::default_path()`.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Barrier, Mutex};
use std::thread;

use cthulhu_git::settings::{FILE_NAME, MAX_RECENT_REPOSITORIES, Settings, SettingsError};
use tempfile::TempDir;

#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};

fn temp_dir() -> TempDir {
    TempDir::with_prefix("cthulhu-settings-").expect("temp dir")
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

/// A file that does not load is reported, and left exactly as it was. The
/// 5 MiB rows are deeper and longer than a settings file has any reason to be.
#[test]
fn invalid_files_are_parse_errors_and_stay_untouched() {
    const FIVE_MIB: usize = 5 * 1024 * 1024;
    let mut long_string = vec![b'a'; FIVE_MIB + 2];
    long_string[0] = b'"';
    long_string[FIVE_MIB + 1] = b'"';
    let cases = [
        ("number theme", br#"{ "theme": 1 }"#.to_vec()),
        (
            "string recents",
            br#"{ "recent_repositories": "/tmp/not-a-list" }"#.to_vec(),
        ),
        (
            "both wrong",
            br#"{ "theme": 7, "recent_repositories": "nope" }"#.to_vec(),
        ),
        ("5 MiB of open braces", vec![b'{'; FIVE_MIB]),
        ("5 MiB string", long_string),
    ];
    for (case, original) in cases {
        let dir = temp_dir();
        let path = dir.path().join(FILE_NAME);
        fs::write(&path, &original).expect(case);

        let error = Settings::load_from(&path).expect_err(case);
        assert!(
            matches!(error, SettingsError::Parse(..)),
            "{case:?}: {error:?}"
        );
        assert!(
            error
                .to_string()
                .contains("using the defaults and leaving the file as it is."),
            "{case:?}: {error}"
        );
        assert!(
            fs::read(&path).expect(case) == original,
            "{case:?}: the file changed"
        );
        assert_eq!(entry_names(dir.path()), [FILE_NAME], "{case:?}");
    }
}

/// A save meets an entry planted next to `settings.json`: a symlink at the
/// settings path or at a temp-like name, pointing outside the config
/// directory, or a leftover temp file. The save succeeds, never writes through
/// a link, adds nothing but `settings.json`, and loads back.
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
        assert_eq!(fs::read(&target).expect("target"), original, "{case:?}");
        result.expect(&case);
        let mut expected = vec![FILE_NAME.to_owned(), name.to_owned()];
        expected.dedup();
        assert_eq!(entry_names(config.path()), expected, "{case:?}");
        assert_eq!(
            Settings::load_from(&path).expect(&case),
            settings,
            "{case:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn unwritable_parent_directory_is_a_write_error() {
    struct RestoreMode<'a>(&'a Path, u32);
    impl Drop for RestoreMode<'_> {
        fn drop(&mut self) {
            let _ = fs::set_permissions(self.0, fs::Permissions::from_mode(self.1));
        }
    }

    let dir = temp_dir();
    let parent = dir.path().join("locked");
    fs::create_dir(&parent).expect("parent");
    let path = parent.join(FILE_NAME);
    let mode = fs::metadata(&parent).expect("meta").permissions().mode();
    // Restored on drop, including assertion failures, so TempDir can remove it.
    let _restore = RestoreMode(&parent, mode);
    crate::support::strip_acls(&parent);
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o555)).expect("chmod");
    // Root, or an ACL the test cannot strip, writes despite mode 0555.
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
        "a failed save must leave neither settings.json nor a temp file"
    );
}

/// Two writers and a reader. Lost updates are fine; a reader that ever sees
/// JSON that does not parse, or more recents than the cap, is not.
#[test]
fn concurrent_saves_leave_valid_json() {
    // One burst often ends on a clean rename, which would hide an earlier
    // torn publish, so each writer runs two.
    const BURSTS: usize = 2;
    let dir = temp_dir();
    let path = dir.path().join(FILE_NAME);
    Settings::default().save_to(&path).expect("seed");

    let start = Barrier::new(2);
    let finished = AtomicUsize::new(0);
    let torn = Mutex::new(None::<String>);
    let note_if_torn = || {
        let problem = match Settings::load_from(&path) {
            Ok(settings) if settings.recent_repositories.len() > MAX_RECENT_REPOSITORIES => {
                Some(format!("{} recents", settings.recent_repositories.len()))
            }
            Ok(_) => None,
            Err(SettingsError::Read(_, error))
                if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::Interrupted) =>
            {
                None
            }
            Err(error) => Some(error.to_string().chars().take(240).collect()),
        };
        if let Some(problem) = problem {
            torn.lock().expect("torn lock").get_or_insert(problem);
        }
    };
    // Counts a writer as done even when it panics, so the reader stops and
    // the panic fails the test instead of a reader that spins forever.
    struct Finish<'a>(&'a AtomicUsize);
    impl Drop for Finish<'_> {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Release);
        }
    }
    let writer = |name: &str, recents: usize, padding: usize| {
        let _finish = Finish(&finished);
        start.wait();
        for _ in 0..BURSTS {
            for iteration in 0..50 {
                let mut settings = Settings::default();
                for slot in 0..recents {
                    settings.remember_repository(&PathBuf::from(format!(
                        "/{name}/{iteration}/{slot}-{}",
                        "n".repeat(padding)
                    )));
                }
                let _ = settings.save_to(&path);
            }
        }
    };
    thread::scope(|scope| {
        scope.spawn(|| {
            while finished.load(Ordering::Acquire) < 2 {
                note_if_torn();
            }
            note_if_torn();
        });
        scope.spawn(|| writer("a", MAX_RECENT_REPOSITORIES, 8192));
        scope.spawn(|| writer("b", 1, 0));
    });

    if let Some(problem) = torn.lock().expect("torn lock").take() {
        panic!("a reader saw a torn settings file: {problem}");
    }
    let loaded = Settings::load_from(&path).expect("final settings are valid json");
    assert!(loaded.recent_repositories.len() <= MAX_RECENT_REPOSITORIES);
}
