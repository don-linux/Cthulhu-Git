//! Finder's `PATH` and the environment of a git child.
//!
//! Fake executables are symlinks to a script written in the test's temp
//! directory before anything is spawned. Each link has its own sidecar, so
//! two tests never exec a file the other is writing.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use cthulhu_git::git::{Git, GitError, GitVersion, inspect};
use tempfile::TempDir;

#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};
#[cfg(unix)]
use std::path::Path;

#[cfg(unix)]
use cthulhu_git::git::{DiscoverInputs, discover_with, extend_path_for_gui};

#[cfg(unix)]
const SCRIPT: &str = r#"#!/bin/sh
mode=$(cat "$0.mode" 2>/dev/null || true)
case "$mode" in
marker)
  touch "$(cat "$0.marker")"
  exit 0
  ;;
record)
  if [ -f "$0.path" ]; then
    printf '%s' "$PATH" > "$(cat "$0.path")"
  fi
  if [ -f "$0.lazy" ]; then
    if [ -n "${GIT_NO_LAZY_FETCH+x}" ]; then
      printf '%s' "$GIT_NO_LAZY_FETCH" > "$(cat "$0.lazy")"
    else
      : > "$(cat "$0.lazy")"
    fi
  fi
  if [ -f "$0.argv" ]; then
    : > "$(cat "$0.argv")"
    for arg in "$@"; do
      printf '%s\0' "$arg" >> "$(cat "$0.argv")"
    done
  fi
  if [ "${1:-}" = "--version" ]; then
    printf 'git version 2.43.0\n'
  fi
  exit 0
  ;;
*)
  printf 'git version 2.43.0\n'
  exit 0
  ;;
esac
"#;

fn sample_git(path: PathBuf) -> Git {
    Git {
        path,
        version: GitVersion {
            major: 2,
            minor: 43,
            patch: 0,
            raw: "2.43.0".to_owned(),
        },
    }
}

#[cfg(unix)]
fn write_script(dir: &Path) -> PathBuf {
    let path = dir.join("fake-git.sh");
    fs::write(&path, SCRIPT).expect("write fake git script");
    let mut permissions = fs::metadata(&path).expect("script metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("chmod fake git script");
    path
}

#[cfg(unix)]
fn sidecar(link: &Path, suffix: &str) -> PathBuf {
    let mut name = link.file_name().expect("link name").to_os_string();
    name.push(suffix);
    link.with_file_name(name)
}

#[cfg(unix)]
fn link_fake(dir: &Path, script: &Path, mode: &str) -> PathBuf {
    fs::create_dir_all(dir).expect("fake dir");
    let path = dir.join("git");
    symlink(script, &path).expect("symlink fake git");
    fs::write(sidecar(&path, ".mode"), mode).expect("mode sidecar");
    path
}

#[cfg(unix)]
#[test]
fn finder_path_never_executes_the_xcrun_shim() {
    let tmp = TempDir::with_prefix("cthulhu-shim-").expect("temp dir");
    let script = write_script(tmp.path());
    let shim_dir = tmp.path().join("usr").join("bin");
    let shim = link_fake(&shim_dir, &script, "marker");
    let marker = tmp.path().join("shim-ran");
    fs::write(sidecar(&shim, ".marker"), marker.display().to_string()).expect("marker sidecar");

    let fallback_dir = tmp.path().join("opt").join("homebrew").join("bin");
    let fallback = link_fake(&fallback_dir, &script, "version");

    let git = discover_with(&DiscoverInputs {
        override_path: None,
        path_var: Some(std::env::join_paths([&shim_dir]).expect("PATH")),
        exe_name: "git",
        fallbacks: vec![fallback.clone()],
        skip_paths: vec![shim],
    })
    .expect("fallback git");

    assert_eq!(git.path, fallback);
    assert_eq!(git.version.raw, "2.43.0");
    assert!(
        !marker.exists(),
        "PATH candidate ran the xcrun shim and touched {}",
        marker.display()
    );
}

#[cfg(unix)]
#[test]
fn finder_path_child_sees_homebrew_bin() {
    let tmp = TempDir::with_prefix("cthulhu-brew-path-").expect("temp dir");
    let script = write_script(tmp.path());
    let dir = tmp.path().join("bin");
    let git = link_fake(&dir, &script, "record");
    let path_file = tmp.path().join("child-path");
    fs::write(sidecar(&git, ".path"), path_file.display().to_string()).expect("path sidecar");

    let finder = "/usr/bin:/bin:/usr/sbin:/sbin";
    // Existence is injected. On Linux `/opt/homebrew/bin` is not on disk, and
    // `Git::command` only appends it on macOS. The child still has to observe
    // the string `extend_path_for_gui` produces for that Finder PATH.
    let child_path = extend_path_for_gui(
        std::ffi::OsStr::new(finder),
        &[PathBuf::from("/opt/homebrew/bin")],
    );
    let output = Command::new(&git)
        .env("PATH", &child_path)
        .arg("--version")
        .output()
        .expect("spawn fake git");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let recorded = fs::read_to_string(&path_file).expect("recorded PATH");
    let dirs: Vec<PathBuf> = std::env::split_paths(&recorded).collect();
    assert_eq!(
        dirs.iter()
            .map(PathBuf::as_path)
            .take(4)
            .collect::<Vec<_>>(),
        vec![
            Path::new("/usr/bin"),
            Path::new("/bin"),
            Path::new("/usr/sbin"),
            Path::new("/sbin"),
        ]
    );
    assert_eq!(
        dirs.last().map(PathBuf::as_path),
        Some(Path::new("/opt/homebrew/bin"))
    );
}

/// `core.fsmonitor=false` is what every `Git::run` passes today.
///
/// `core.fsmonitor=` (empty) would keep Git 2.15–2.35 from `execvp("false")`.
/// `tests/adversarial_git_security.rs` still matches the literal `false`, so
/// this argv stays until that suite is updated.
#[cfg(unix)]
#[test]
fn fsmonitor_argv_documents_the_false_sentinel() {
    let recorded = record_git_run(&["status"]);
    assert!(
        recorded
            .argv
            .windows(2)
            .any(|pair| pair == ["-c", "core.fsmonitor=false"]),
        "argv was {:?}",
        recorded.argv
    );
}

#[cfg(unix)]
#[test]
fn git_no_lazy_fetch_is_set_without_overriding_a_parent_value() {
    reexec_lazy(None);
    reexec_lazy(Some("keep-me"));
}

#[cfg(unix)]
fn reexec_lazy(preset: Option<&str>) {
    let mut command = Command::new(std::env::current_exe().expect("test binary"));
    command.args([
        "--exact",
        "child_records_git_no_lazy_fetch",
        "--include-ignored",
        "--nocapture",
        "--test-threads=1",
    ]);
    command.env("CTHULHU_EXPECT_LAZY", preset.unwrap_or("1"));
    match preset {
        Some(value) => {
            command.env("GIT_NO_LAZY_FETCH", value);
        }
        None => {
            command.env_remove("GIT_NO_LAZY_FETCH");
        }
    }
    let output = command.output().expect("spawn child test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "preset {preset:?} child failed:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("1 passed"),
        "preset {preset:?} child did not run:\n{stdout}\n{stderr}"
    );
}

#[cfg(unix)]
#[test]
#[ignore = "helper run by git_no_lazy_fetch_is_set_without_overriding_a_parent_value"]
fn child_records_git_no_lazy_fetch() {
    let expected = std::env::var("CTHULHU_EXPECT_LAZY").expect("parent sets the expected value");
    let recorded = record_git_run(&["status"]);
    assert_eq!(recorded.lazy, expected);
}

#[cfg(unix)]
struct Recorded {
    argv: Vec<String>,
    lazy: String,
}

#[cfg(unix)]
fn record_git_run(args: &[&str]) -> Recorded {
    let tmp = TempDir::with_prefix("cthulhu-record-").expect("temp dir");
    let script = write_script(tmp.path());
    let dir = tmp.path().join("bin");
    let link = link_fake(&dir, &script, "record");
    let path_file = tmp.path().join("path");
    let lazy_file = tmp.path().join("lazy");
    let argv_file = tmp.path().join("argv");
    fs::write(sidecar(&link, ".path"), path_file.display().to_string()).expect("path sidecar");
    fs::write(sidecar(&link, ".lazy"), lazy_file.display().to_string()).expect("lazy sidecar");
    fs::write(sidecar(&link, ".argv"), argv_file.display().to_string()).expect("argv sidecar");

    let git = sample_git(link);
    let output = git.run(tmp.path(), args).expect("run fake git");
    assert!(output.status.success(), "{}", output.stderr);

    let argv_bytes = fs::read(&argv_file).expect("argv log");
    let argv = argv_bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8(part.to_vec()).expect("argv utf-8"))
        .collect();
    Recorded {
        argv,
        lazy: fs::read_to_string(&lazy_file).expect("lazy log"),
    }
}

#[test]
fn missing_workdir_is_not_a_spawn_error() {
    let tmp = TempDir::with_prefix("cthulhu-missing-cwd-").expect("temp dir");
    let program = tmp
        .path()
        .join(if cfg!(windows) { "git.exe" } else { "git" });
    fs::write(&program, b"#!/bin/sh\nexit 0\n").expect("program");
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&program).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&program, permissions).expect("chmod");
    }
    let gone = tmp.path().join("gone");
    fs::create_dir(&gone).expect("workdir");
    fs::remove_dir(&gone).expect("delete workdir");

    let error = sample_git(program)
        .run(&gone, &["status"])
        .expect_err("deleted workdir");
    assert!(
        matches!(error, GitError::NotADirectory(ref path) if path == &gone),
        "{error:?}"
    );

    let missing = tmp.path().join("no-such-git");
    let missing_error = sample_git(missing)
        .run(tmp.path(), &["status"])
        .expect_err("missing git");
    assert!(
        matches!(missing_error, GitError::Spawn(ref io) if io.kind() == std::io::ErrorKind::NotFound),
        "a missing git binary must stay a spawn error, got {missing_error:?}"
    );
}

#[test]
fn nfd_filename_matches_nfc_index() {
    let git = Git::discover().expect("system git");
    let tmp = TempDir::with_prefix("cthulhu-nfd-").expect("temp dir");
    let root = tmp.path().join("repo");
    let hooks = tmp.path().join("hooks");
    fs::create_dir(&root).expect("repo dir");
    fs::create_dir(&hooks).expect("hooks dir");

    // U+0065 U+0301: 'e' plus combining acute. NFC would be U+00E9.
    let name = "e\u{0301}";
    assert_eq!(name.chars().collect::<Vec<_>>(), ['e', '\u{0301}']);
    fs::write(root.join(name), b"acute\n").expect("write NFD file");

    let hooks_arg = format!("core.hooksPath={}", hooks.display());
    for args in [
        vec!["init", "-b", "main"],
        vec!["add", "--", name],
        vec!["commit", "-m", "nfd"],
    ] {
        let mut command = Command::new(&git.path);
        command
            .current_dir(&root)
            .env("GIT_AUTHOR_NAME", "cthulhu")
            .env("GIT_AUTHOR_EMAIL", "cthulhu@example.invalid")
            .env("GIT_COMMITTER_NAME", "cthulhu")
            .env("GIT_COMMITTER_EMAIL", "cthulhu@example.invalid")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .args(["-c", "commit.gpgSign=false", "-c", &hooks_arg])
            .args(&args);
        let output = command.output().expect("setup git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let info = inspect(&git, &root).expect("inspect");
    assert_eq!(info.root, fs::canonicalize(&root).expect("canonical root"));

    let status = git
        .run(&root, &["status", "--porcelain", "-z"])
        .expect("status");
    assert!(status.status.success(), "{}", status.stderr);
    assert!(
        status.stdout.is_empty(),
        "status reported a change for the committed NFD name: {}",
        String::from_utf8_lossy(&status.stdout)
    );

    let listed = git.run(&root, &["ls-files", "-z"]).expect("ls-files");
    assert!(
        listed.status.success()
            && listed
                .stdout
                .windows(name.len())
                .any(|window| window == name.as_bytes()),
        "index lost the NFD filename: {}",
        String::from_utf8_lossy(&listed.stdout)
    );
}
