//! Discovery against fake git executables.
//!
//! Every fake is a symlink to one shared shell script that prints the version
//! stored next to the link. Writing a fresh executable per test would race with
//! other test threads forking (`ETXTBSY`), so the script is written only once,
//! before any fake is spawned.
#![cfg(unix)]

use std::env;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use cthulhu_git::git::{DiscoverInputs, GitError, discover_with};
use tempfile::TempDir;

const SCRIPT: &str = r#"#!/bin/sh
version=$(cat "$0.version")
[ "$version" = broken ] && exit 1
echo "git version $version"
"#;

fn shared_script() -> &'static Path {
    static SCRIPT_PATH: OnceLock<PathBuf> = OnceLock::new();
    SCRIPT_PATH.get_or_init(|| {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cthulhu-fake-git.sh");
        fs::write(&path, SCRIPT).expect("write fake git script");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    })
}

/// Creates `dir/name` answering `git version <version>`; `"broken"` exits 1.
fn fake(dir: &Path, name: &str, version: &str) -> PathBuf {
    fs::create_dir_all(dir).expect("fake dir");
    let path = dir.join(name);
    fs::write(dir.join(format!("{name}.version")), version).expect("version file");
    symlink(shared_script(), &path).expect("symlink");
    path
}

fn temp() -> TempDir {
    TempDir::with_prefix("cthulhu-discover-").expect("temp dir")
}

fn inputs(path_dirs: &[&Path], fallbacks: Vec<PathBuf>) -> DiscoverInputs {
    DiscoverInputs {
        override_path: None,
        path_var: Some(env::join_paths(path_dirs).expect("joinable PATH")),
        exe_name: "git",
        fallbacks,
    }
}

#[test]
fn path_wins_over_fallbacks() {
    let tmp = temp();
    let path_dir = tmp.path().join("path");
    let on_path = fake(&path_dir, "git", "2.43.0");
    let fallback = fake(&tmp.path().join("fallback"), "git", "2.40.0");

    let git = discover_with(&inputs(&[&path_dir], vec![fallback])).expect("git on PATH");
    assert_eq!(git.path, on_path);
    assert_eq!(git.version.raw, "2.43.0");
}

#[test]
fn fallback_is_used_when_path_has_no_git() {
    let tmp = temp();
    let empty = tmp.path().join("empty");
    fs::create_dir_all(&empty).expect("empty dir");
    let fallback = fake(&tmp.path().join("fallback"), "git", "2.40.0");

    let git = discover_with(&inputs(&[&empty], vec![fallback.clone()])).expect("fallback");
    assert_eq!(git.path, fallback);
}

#[test]
fn override_takes_priority_over_path() {
    let tmp = temp();
    let path_dir = tmp.path().join("path");
    fake(&path_dir, "git", "2.43.0");
    let pinned = fake(&tmp.path().join("pinned"), "my-git", "2.30.1");

    let mut inputs = inputs(&[&path_dir], Vec::new());
    inputs.override_path = Some(pinned.clone().into());
    let git = discover_with(&inputs).expect("override");
    assert_eq!(git.path, pinned);
    assert_eq!(git.version.raw, "2.30.1");
}

#[test]
fn too_old_override_is_rejected() {
    let tmp = temp();
    let pinned = fake(tmp.path(), "git", "2.14.1");
    let mut inputs = inputs(&[], Vec::new());
    inputs.override_path = Some(pinned.into());

    let error = discover_with(&inputs).expect_err("too old");
    assert!(
        matches!(&error, GitError::TooOld(version) if version.numbers() == (2, 14, 1)),
        "{error:?}"
    );
}

#[test]
fn broken_override_reports_the_failure() {
    let tmp = temp();
    let pinned = fake(tmp.path(), "git", "broken");
    let mut inputs = inputs(&[], Vec::new());
    inputs.override_path = Some(pinned.into());

    let error = discover_with(&inputs).expect_err("broken");
    assert!(matches!(&error, GitError::Failed { .. }), "{error:?}");
}

#[test]
fn file_without_execute_bit_is_skipped() {
    let tmp = temp();
    let first = tmp.path().join("first");
    fs::create_dir_all(&first).expect("dir");
    fs::write(first.join("git"), SCRIPT).expect("write");
    fs::set_permissions(first.join("git"), fs::Permissions::from_mode(0o644)).expect("chmod");
    let second = tmp.path().join("second");
    let good = fake(&second, "git", "2.42.0");

    let git = discover_with(&inputs(&[&first, &second], Vec::new())).expect("second");
    assert_eq!(git.path, good);
}

#[test]
fn broken_candidate_falls_through_to_the_next() {
    let tmp = temp();
    let broken = tmp.path().join("broken");
    let working = tmp.path().join("working");
    fake(&broken, "git", "broken");
    let good = fake(&working, "git", "2.43.0");

    let git = discover_with(&inputs(&[&broken, &working], Vec::new())).expect("working");
    assert_eq!(git.path, good);
}

#[test]
fn old_candidate_is_skipped_for_a_newer_one() {
    let tmp = temp();
    let old = tmp.path().join("old");
    let new = tmp.path().join("new");
    fake(&old, "git", "2.9.5");
    let good = fake(&new, "git", "2.43.0");

    let git = discover_with(&inputs(&[&old, &new], Vec::new())).expect("newer");
    assert_eq!(git.path, good);
}

#[test]
fn only_old_candidates_report_too_old() {
    let tmp = temp();
    fake(tmp.path(), "git", "2.9.5");

    let error = discover_with(&inputs(&[tmp.path()], Vec::new())).expect_err("too old");
    assert!(
        matches!(&error, GitError::TooOld(version) if version.raw == "2.9.5"),
        "{error:?}"
    );
}

#[test]
fn relative_path_entry_is_ignored_even_if_it_contains_git() {
    let tmp = temp();
    fake(tmp.path(), "git", "2.43.0");
    let cwd = env::current_dir().expect("cwd");
    let mut relative = PathBuf::new();
    for _ in cwd.components().skip(1) {
        relative.push("..");
    }
    relative.push(tmp.path().strip_prefix("/").expect("absolute temp dir"));
    assert!(relative.is_relative() && relative.join("git").is_file());

    let error = discover_with(&inputs(&[&relative], Vec::new())).expect_err("relative");
    assert!(
        matches!(&error, GitError::NotFound { tried } if tried.is_empty()),
        "{error:?}"
    );
}

#[test]
fn simulated_windows_layout_uses_git_exe_and_fallbacks() {
    let tmp = temp();
    let path_dir = tmp.path().join("path");
    fake(&path_dir, "git", "2.43.0");
    fake(&path_dir, "git.cmd", "2.43.0");
    let program_files_git = fake(
        &tmp.path().join("Program Files").join("Git").join("cmd"),
        "git.exe",
        "2.43.0.windows.1",
    );

    let inputs = DiscoverInputs {
        override_path: None,
        path_var: Some(env::join_paths([&path_dir]).expect("PATH")),
        exe_name: "git.exe",
        fallbacks: vec![program_files_git.clone()],
    };
    let git = discover_with(&inputs).expect("Git for Windows fallback");
    assert_eq!(git.path, program_files_git);
    assert_eq!(git.version.raw, "2.43.0.windows.1");
    assert_eq!(git.version.numbers(), (2, 43, 0));
}
