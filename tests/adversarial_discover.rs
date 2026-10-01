//! Adversarial discovery cases: relative overrides, version edges, and
//! simulated Windows / macOS search inputs.
//!
//! Fakes are symlinks to one shared script (unique name under
//! `CARGO_TARGET_TMPDIR`) so forking tests do not race on `ETXTBSY`.
//! These tests never change the parent process environment or cwd. The
//! relative-override cases re-exec a child when they must `chdir`.
#![cfg(unix)]

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{OnceLock, mpsc};
use std::thread;
use std::time::Duration;

use cthulhu_git::git::{DiscoverInputs, GitError, GitVersion, OVERRIDE_VAR, discover_with};
use tempfile::TempDir;

const SCRIPT: &str = r#"#!/bin/sh
version=$(cat "$0.version")
[ "$version" = broken ] && exit 1
if [ "$version" = sleep ]; then
  sleep 3
  printf 'git version 2.43.0\n'
  exit 0
fi
if [ "$version" = garbage ]; then
  printf 'this is not git version output\n'
  printf '\377\376'
  exit 0
fi
printf 'git version %s\n' "$version"
"#;

const CHILD_CWD_VAR: &str = "CTHULHU_ADV_DISCOVER_CWD";
const CHILD_PATH_GIT_VAR: &str = "CTHULHU_ADV_DISCOVER_PATH_GIT";

fn shared_script() -> &'static Path {
    static SCRIPT_PATH: OnceLock<PathBuf> = OnceLock::new();
    SCRIPT_PATH.get_or_init(|| {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cthulhu-adv-discover-fake.sh");
        // Rename into place so a concurrent exec keeps the previous inode.
        let tmp = path.with_extension("sh.tmp");
        {
            let mut file = fs::File::create(&tmp).expect("write fake git script");
            file.write_all(SCRIPT.as_bytes())
                .expect("write fake git script");
        }
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).expect("chmod");
        fs::rename(&tmp, &path).expect("install fake git script");
        path
    })
}

/// Creates `dir/name` answering `git version <version>`.
///
/// `"broken"` exits 1. `"sleep"` sleeps 3s then prints 2.43.0. `"garbage"`
/// prints non-version stdout (including a non-UTF-8 byte) and exits 0.
/// A version string may itself contain a trailing `\r`.
fn fake(dir: &Path, name: &str, version: &str) -> PathBuf {
    fs::create_dir_all(dir).expect("fake dir");
    let path = dir.join(name);
    fs::write(dir.join(format!("{name}.version")), version).expect("version file");
    let _ = fs::remove_file(&path);
    symlink(shared_script(), &path).expect("symlink");
    path
}

fn temp() -> TempDir {
    TempDir::with_prefix("cthulhu-adv-discover-").expect("temp dir")
}

fn inputs(path_dirs: &[&Path], fallbacks: Vec<PathBuf>) -> DiscoverInputs {
    DiscoverInputs {
        override_path: None,
        path_var: Some(env::join_paths(path_dirs).expect("joinable PATH")),
        exe_name: "git",
        fallbacks,
    }
}

fn assert_child_ok(output: &std::process::Output, label: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{label} child failed:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("1 passed"),
        "{label} child did not run:\n{stdout}\n{stderr}"
    );
}

fn child_path(path_git_dir: &Path) -> OsString {
    let mut path = OsString::from(path_git_dir);
    path.push(":/usr/bin:/bin");
    path
}

fn reexec_child(test_name: &str, cwd: &Path, path_git_dir: &Path) -> std::process::Output {
    Command::new(env::current_exe().expect("test binary"))
        .args([
            "--exact",
            test_name,
            "--include-ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_CWD_VAR, cwd)
        .env(CHILD_PATH_GIT_VAR, path_git_dir)
        // `Command::new("git")` searches the process PATH, not DiscoverInputs.
        // System directories stay so the fake script can run `cat`, with the
        // decoy git directory first.
        .env("PATH", child_path(path_git_dir))
        .env_remove(OVERRIDE_VAR)
        .output()
        .expect("spawn child test")
}

#[test]
fn override_var_is_cthulhu_git() {
    assert_eq!(OVERRIDE_VAR, "CTHULHU_GIT");
}

/// Bare `git` has no slash, so a correct override must run the executable in
/// the child cwd (`2.43.0`) rather than a different git found via PATH.
#[test]
fn relative_bare_override_uses_cwd_git_not_path() {
    let tmp = temp();
    let cwd = tmp.path().join("cwd");
    let path_dir = tmp.path().join("path");
    fake(&cwd, "git", "2.43.0");
    fake(&path_dir, "git", "2.40.0");

    let output = reexec_child("child_relative_bare_override_uses_cwd_git", &cwd, &path_dir);
    assert_child_ok(&output, "bare relative override");
}

#[test]
#[ignore = "helper run by relative_bare_override_uses_cwd_git_not_path; chdir is not allowed in the parent test process"]
fn child_relative_bare_override_uses_cwd_git() {
    let Some(cwd) = env::var_os(CHILD_CWD_VAR) else {
        return;
    };
    let path_dir = env::var_os(CHILD_PATH_GIT_VAR).expect("PATH git dir");
    env::set_current_dir(&cwd).expect("chdir");

    let git = discover_with(&DiscoverInputs {
        override_path: Some(OsString::from("git")),
        path_var: Some(path_dir),
        exe_name: "git",
        fallbacks: Vec::new(),
    })
    .expect("bare relative override");

    assert_eq!(
        git.path,
        PathBuf::from("git"),
        "override must be probed as given"
    );
    assert_eq!(
        git.version.raw,
        "2.43.0",
        "bare CTHULHU_GIT=git must run ./git in the cwd (2.43.0), not the different git on PATH (2.40.0); got {} from {}",
        git.version.raw,
        git.path.display()
    );
}

/// A relative override that contains a slash is cwd-relative for both
/// `metadata` and `Command::new`.
#[test]
fn relative_slash_override_uses_cwd_git() {
    let tmp = temp();
    let cwd = tmp.path().join("cwd");
    let path_dir = tmp.path().join("path");
    fake(&cwd, "git", "2.43.0");
    fake(&path_dir, "git", "2.40.0");

    let output = reexec_child(
        "child_relative_slash_override_uses_cwd_git",
        &cwd,
        &path_dir,
    );
    assert_child_ok(&output, "slash relative override");
}

#[test]
#[ignore = "helper run by relative_slash_override_uses_cwd_git; chdir is not allowed in the parent test process"]
fn child_relative_slash_override_uses_cwd_git() {
    let Some(cwd) = env::var_os(CHILD_CWD_VAR) else {
        return;
    };
    let path_dir = env::var_os(CHILD_PATH_GIT_VAR).expect("PATH git dir");
    env::set_current_dir(&cwd).expect("chdir");

    let git = discover_with(&DiscoverInputs {
        override_path: Some(OsString::from("./git")),
        path_var: Some(path_dir),
        exe_name: "git",
        fallbacks: Vec::new(),
    })
    .expect("slash relative override");

    assert_eq!(git.version.raw, "2.43.0", "{git:?}");
    assert_eq!(git.path, PathBuf::from("./git"));
}

#[test]
fn absolute_override_with_space_in_path_works() {
    let tmp = temp();
    let pinned = fake(
        &tmp.path().join("Program Files").join("Git"),
        "git",
        "2.43.0",
    );
    let mut inputs = inputs(&[], Vec::new());
    inputs.override_path = Some(pinned.clone().into());

    let git = discover_with(&inputs).expect("override path containing a space");
    assert_eq!(git.path, pinned);
    assert_eq!(git.version.raw, "2.43.0");
}

#[test]
fn override_symlink_to_directory_is_bad_override() {
    let tmp = temp();
    let dir = tmp.path().join("real-dir");
    fs::create_dir(&dir).expect("dir");
    let link = tmp.path().join("git-link");
    symlink(&dir, &link).expect("symlink to directory");

    let mut inputs = inputs(&[], Vec::new());
    inputs.override_path = Some(link.clone().into());
    let error = discover_with(&inputs).expect_err("symlink to a directory");
    assert!(
        matches!(error, GitError::BadOverride(path) if path == link),
        "symlink-to-directory override must be BadOverride"
    );
}

#[test]
fn sleeping_override_is_bounded_to_about_one_second() {
    let tmp = temp();
    let pinned = fake(tmp.path(), "git", "sleep");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let inputs = DiscoverInputs {
            override_path: Some(pinned.into()),
            path_var: None,
            exe_name: "git",
            fallbacks: Vec::new(),
        };
        let _ = tx.send(discover_with(&inputs));
    });

    match rx.recv_timeout(Duration::from_secs(1)) {
        Ok(Ok(git)) => panic!(
            "FINDING: CTHULHU_GIT whose --version sleeps 3s returned a version within 1s ({}); discovery must fail or still be bounded",
            git.version.raw
        ),
        Ok(Err(_)) => {}
        Err(mpsc::RecvTimeoutError::Timeout) => panic!(
            "FINDING: CTHULHU_GIT --version that sleeps 3s was still running after 1s; discovery must fail or return within about 1s and must not wait out a hung git"
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("discovery thread ended without a result");
        }
    }
}

#[test]
fn release_candidate_2_15_0_rc0_parse_does_not_meet_the_floor() {
    match GitVersion::parse("git version 2.15.0-rc0\n") {
        None => {}
        Some(version) => assert!(
            !version.is_supported(),
            "2.15.0-rc0 must not count as 2.15.0 final; parsed as {} {:?}",
            version.raw,
            version.numbers()
        ),
    }
}

#[test]
fn release_candidate_2_15_0_rc0_override_does_not_meet_the_floor() {
    let tmp = temp();
    let pinned = fake(tmp.path(), "git", "2.15.0-rc0");
    let mut inputs = inputs(&[], Vec::new());
    inputs.override_path = Some(pinned.into());
    match discover_with(&inputs) {
        Ok(git) => panic!(
            "2.15.0-rc0 must not count as 2.15.0 final; discovery accepted {} {:?}",
            git.version.raw,
            git.version.numbers()
        ),
        Err(GitError::TooOld(version)) => {
            assert!(!version.is_supported(), "{version:?}");
            assert!(version.raw.contains("rc"), "{version:?}");
        }
        Err(GitError::Failed { .. }) => {}
        Err(error) => panic!("expected 2.15.0-rc0 to be rejected, got {error:?}"),
    }
}

#[test]
fn windows_vfs_and_crlf_versions_are_supported() {
    let cases = [
        (
            "git version 2.43.0.windows.1\n",
            "2.43.0.windows.1",
            (2, 43, 0),
        ),
        ("git version 2.39.2.vfs.0.0\n", "2.39.2.vfs.0.0", (2, 39, 2)),
        ("git version 2.43.0\r\n", "2.43.0", (2, 43, 0)),
    ];
    for (output, raw, numbers) in cases {
        let parsed = GitVersion::parse(output).unwrap_or_else(|| panic!("parse {output:?}"));
        assert_eq!(parsed.raw, raw, "{output:?}");
        assert_eq!(parsed.numbers(), numbers, "{output:?}");
        assert!(parsed.is_supported(), "{output:?}");
    }

    let tmp = temp();
    for version in ["2.43.0.windows.1", "2.39.2.vfs.0.0", "2.43.0\r"] {
        let pinned = fake(tmp.path(), "git", version);
        let mut inputs = inputs(&[], Vec::new());
        inputs.override_path = Some(pinned.into());
        let git = discover_with(&inputs).unwrap_or_else(|error| panic!("{version:?}: {error:?}"));
        assert!(git.version.is_supported(), "{version:?} -> {git:?}");
        assert!(
            !git.version.raw.contains('\r'),
            "CR must not remain in the parsed version: {git:?}"
        );
        if version.ends_with('\r') {
            assert_eq!(git.version.raw, "2.43.0");
            assert_eq!(git.version.numbers(), (2, 43, 0));
        } else {
            assert_eq!(git.version.raw, version);
        }
    }
}

#[test]
fn garbage_stdout_is_an_error_not_a_panic() {
    assert_eq!(GitVersion::parse("this is not git version output\n"), None);
    assert_eq!(GitVersion::parse("\u{fffd}\n"), None);

    let tmp = temp();
    let pinned = fake(tmp.path(), "git", "garbage");
    let mut inputs = inputs(&[], Vec::new());
    inputs.override_path = Some(pinned.into());
    let error = discover_with(&inputs).expect_err("garbage stdout must be an error");
    assert!(
        matches!(error, GitError::Failed { .. }),
        "garbage stdout must be Failed, not a panic or another variant: {error:?}"
    );
}

#[test]
fn windows_path_with_trailing_slash_finds_git_exe() {
    let tmp = temp();
    let path_dir = tmp.path().join("Git").join("cmd");
    let git_exe = fake(&path_dir, "git.exe", "2.43.0.windows.1");
    let mut element = path_dir.into_os_string();
    element.push("/");
    assert!(Path::new(&element).is_absolute());

    let found = discover_with(&DiscoverInputs {
        override_path: None,
        path_var: Some(element),
        exe_name: "git.exe",
        fallbacks: Vec::new(),
    })
    .expect("trailing-slash PATH element");
    assert_eq!(found.path, git_exe);
    assert_eq!(found.version.raw, "2.43.0.windows.1");
    assert_eq!(found.version.numbers(), (2, 43, 0));
}

/// Some Windows launchers hand PATH over with the quotes still in the element.
/// The unquoted path is absolute and must be searched.
#[test]
fn quoted_absolute_path_element_is_unquoted_and_searched() {
    let tmp = temp();
    let path_dir = tmp.path().join("Program Files").join("Git").join("cmd");
    let git_exe = fake(&path_dir, "git.exe", "2.43.0");
    let quoted = format!("\"{}\"", path_dir.display());
    assert!(!Path::new(&quoted).is_absolute());
    assert!(Path::new(&quoted[1..quoted.len() - 1]).is_absolute());

    let found = discover_with(&DiscoverInputs {
        override_path: None,
        path_var: Some(OsString::from(&quoted)),
        exe_name: "git.exe",
        fallbacks: Vec::new(),
    })
    .expect("quoted absolute PATH element must be unquoted and searched");
    assert_eq!(found.path, git_exe);
    assert_eq!(found.version.raw, "2.43.0");
}

#[test]
fn very_long_absolute_path_does_not_panic() {
    let tmp = temp();
    let prefix_len = tmp.path().as_os_str().len();
    let name_len = 200usize.saturating_sub(prefix_len + 1).max(1);
    assert!(name_len < 255, "component would exceed NAME_MAX");
    let path_dir = tmp.path().join("n".repeat(name_len));
    assert!(path_dir.as_os_str().len() >= 200, "{}", path_dir.display());
    let git_exe = fake(&path_dir, "git.exe", "2.43.0");

    let found = discover_with(&DiscoverInputs {
        override_path: None,
        path_var: Some(path_dir.into_os_string()),
        exe_name: "git.exe",
        fallbacks: Vec::new(),
    })
    .expect("200-character absolute PATH element");
    assert_eq!(found.path, git_exe);
    assert_eq!(found.version.raw, "2.43.0");
}

/// Simulated Xcode shim: the first fallback exits 1 and the next one wins.
#[test]
fn broken_xcode_shim_falls_through_to_the_next_fallback() {
    let tmp = temp();
    let shim = fake(&tmp.path().join("usr").join("bin"), "git", "broken");
    let brew = fake(
        &tmp.path().join("opt").join("homebrew").join("bin"),
        "git",
        "2.43.0",
    );
    let git = discover_with(&DiscoverInputs {
        override_path: None,
        path_var: None,
        exe_name: "git",
        fallbacks: vec![shim, brew.clone()],
    })
    .expect("second fallback after a broken shim");
    assert_eq!(git.path, brew);
    assert_eq!(git.version.raw, "2.43.0");
}
