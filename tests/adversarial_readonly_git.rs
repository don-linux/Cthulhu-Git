//! Read-only git commands must not run a repository diff driver.
//!
//! Fixture git is isolated from the runner's config. `Git::command` drops
//! inherited `GIT_*` (except the SSH helpers), so those variables cannot
//! carry isolation into the library. The hover test therefore re-execs with
//! an empty `HOME` and `XDG_CONFIG_HOME`, which the library does keep.

#![cfg(unix)]

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use cthulhu_git::git::{commit_hover, file_diff};
use tempfile::TempDir;

const ISOLATED: &str = "CTHULHU_ADV_D_ISOLATED";

const CLEARED_ENV: &[&str] = &[
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_KEY_0",
    "GIT_CONFIG_VALUE_0",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_EXEC_PATH",
    "GIT_EXTERNAL_DIFF",
];

fn write_executable(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("script directory");
    }
    fs::write(path, body).expect("write script");
    let mut permissions = fs::metadata(path).expect("script metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod script");
}

fn shell_single_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Runs fixture git with a private config. The library's own git does not see
/// these `GIT_CONFIG_*` values: `Git::command` removes them.
fn fixture_git(git: &Path, scratch: &Path, root: &Path, args: &[&str]) -> String {
    let hooks = scratch.join("empty-hooks");
    let global = scratch.join("empty.gitconfig");
    let system = scratch.join("empty.system.gitconfig");
    fs::create_dir_all(&hooks).expect("empty hooks");
    fs::write(&global, "").expect("empty global gitconfig");
    fs::write(&system, "").expect("empty system gitconfig");

    let mut command = Command::new(git);
    command
        .current_dir(root)
        .stdin(Stdio::null())
        .env("GIT_AUTHOR_NAME", "cthulhu")
        .env("GIT_AUTHOR_EMAIL", "cthulhu@example.invalid")
        .env("GIT_COMMITTER_NAME", "cthulhu")
        .env("GIT_COMMITTER_EMAIL", "cthulhu@example.invalid")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &global)
        .env("GIT_CONFIG_SYSTEM", &system)
        .args(["-c", "commit.gpgSign=false", "-c", "core.autocrlf=false"])
        .arg("-c")
        .arg(format!("core.hooksPath={}", hooks.display()));
    for var in CLEARED_ENV {
        if *var != "GIT_CONFIG_GLOBAL" && *var != "GIT_CONFIG_SYSTEM" {
            command.env_remove(var);
        }
    }
    command.args(args);
    let output = command.output().expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("utf8")
        .trim()
        .to_owned()
}

#[test]
fn hover_and_file_diff_do_not_run_textconv() {
    let home = TempDir::new().expect("private home");
    let xdg = TempDir::new().expect("private xdg");
    let mut child = Command::new(env::current_exe().expect("test binary"));
    child
        .args([
            "--exact",
            "child_hover_and_file_diff_do_not_run_textconv",
            "--include-ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ISOLATED, "1")
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", xdg.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_CONFIG_GLOBAL")
        .env_remove("GIT_CONFIG_SYSTEM")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE");
    let output = child.output().expect("spawn isolated child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "textconv child failed:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("1 passed"),
        "textconv child did not run:\n{stdout}\n{stderr}"
    );
}

#[test]
#[ignore = "child of hover_and_file_diff_do_not_run_textconv; isolated config"]
fn child_hover_and_file_diff_do_not_run_textconv() {
    if env::var_os(ISOLATED).is_none() {
        return;
    }

    let scratch = TempDir::new().expect("scratch");
    let root = scratch.path().join("repo");
    fs::create_dir(&root).expect("repo dir");
    let git = cthulhu_git::git::Git::discover().expect("system git");
    let git_bin = git.path.clone();

    fixture_git(&git_bin, scratch.path(), &root, &["init", "-b", "main"]);

    let marker = scratch.path().join("textconv-marker");
    let script = scratch.path().join("pwn.sh");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\nprintf 'pwned\\n' >> {}\ncat\n",
            shell_single_quote(&marker.display().to_string())
        ),
    );
    let script_arg = script.display().to_string();
    fixture_git(
        &git_bin,
        scratch.path(),
        &root,
        &["config", "diff.pwn.textconv", &script_arg],
    );
    fs::write(root.join(".gitattributes"), "* diff=pwn\n").expect("gitattributes");
    fs::write(root.join("note.txt"), "hello\n").expect("tracked file");
    fixture_git(
        &git_bin,
        scratch.path(),
        &root,
        &["add", "note.txt", ".gitattributes"],
    );
    fixture_git(&git_bin, scratch.path(), &root, &["commit", "-m", "one"]);
    let oid = fixture_git(&git_bin, scratch.path(), &root, &["rev-parse", "HEAD"]);
    assert!(!marker.exists(), "marker must start absent");

    let hover = commit_hover(&git, &root, &oid).expect("commit hover parses");
    let diff = file_diff(&git, &root, &oid, "note.txt").expect("file diff parses");

    assert!(!marker.exists(), "textconv ran while reading the commit");
    assert_eq!(hover.oid, oid);
    assert!(
        diff.lines.iter().any(|line| line.text == "hello"),
        "file diff did not contain the committed line: {diff:?}"
    );
}
