//! The processes git starts: time limits, output caps, and what is left
//! running afterwards.
//!
//! Each call runs on its own thread with a deadline ([`within`]), so a call
//! that ignores its own limit fails instead of hanging the suite.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::time::Duration;

use cthulhu_git::git::GitError;
use tempfile::TempDir;

#[cfg(unix)]
use crate::support::{
    SETSID_PIDFILE_VAR, assert_pid_dead, become_setsid_sleeper, fake_git, sidecar, within,
};
use crate::support::{git_at, write_executable};
#[cfg(unix)]
use cthulhu_git::git::{DiscoverInputs, Git, discover_with, inspect};

#[cfg(unix)]
fn temp() -> TempDir {
    TempDir::with_prefix("cthulhu-process-").expect("temp dir")
}

/// A `--version` that never answers is killed, with what it started, after
/// the 250 ms probe limit.
#[cfg(unix)]
#[test]
fn a_hung_version_probe_is_killed() {
    let case = "sleeping override";
    let tmp = temp();
    let hung = fake_git(&tmp.path().join("hung/git"), "sleep", "");
    let inputs = DiscoverInputs {
        override_path: Some(hung.clone().into()),
        path_var: None,
        exe_name: "git",
        fallbacks: Vec::new(),
        skip_paths: Vec::new(),
    };
    let found = within(case, Duration::from_secs(5), move || {
        discover_with(&inputs).map(|git| git.path)
    });
    assert!(found.is_err(), "{case:?}: {found:?}");
    assert_pid_dead(case, &sidecar(&hung, ".pid"));
}

/// A command past its limit is `TimedOut`, and its process group is gone,
/// including a grandchild that left it with `setsid`.
#[cfg(unix)]
#[test]
fn timeout_kills_the_process_group() {
    type Call = fn(&Git, &Path) -> Result<(), GitError>;
    // (case, fake mode, limit in ms, call, start of the timed-out command)
    let cases: [(&str, &str, u64, Call, &str); 1] = [(
        "setsid grandchild",
        "setsid",
        2000,
        |git, root| inspect(git, root).map(drop),
        "git rev-parse --is-bare-repository",
    )];
    for (case, mode, limit, call, command_start) in cases {
        let tmp = temp();
        let fake = fake_git(&tmp.path().join("git"), "2.43.0", mode);
        let limit = Duration::from_millis(limit);
        let git = Git {
            command_timeout: limit,
            ..git_at(fake.clone())
        };
        let root = tmp.path().to_path_buf();
        match within(case, Duration::from_secs(30), move || call(&git, &root)) {
            Err(GitError::TimedOut {
                command,
                limit: hit,
            }) => {
                assert!(command.starts_with(command_start), "{case:?}: {command}");
                assert_eq!(hit, limit, "{case:?}");
            }
            other => panic!("{case:?}: expected TimedOut, got {other:?}"),
        }
        assert_pid_dead(case, &sidecar(&fake, ".pid"));
    }
}

#[cfg(unix)]
#[test]
#[ignore = "re-exec target of the fake git in setsid mode"]
fn harness_setsid_sleeper() {
    if let Some(path) = std::env::var_os(SETSID_PIDFILE_VAR) {
        become_setsid_sleeper(Path::new(&path));
    }
}

/// `spawn` reports a missing working directory with the same error kind as a
/// missing program. Only a missing program is a failure to run git.
#[test]
fn missing_workdir_is_not_a_spawn_error() {
    let tmp = TempDir::with_prefix("cthulhu-process-").expect("temp dir");
    let program = tmp
        .path()
        .join(if cfg!(windows) { "git.exe" } else { "git" });
    write_executable(&program, "#!/bin/sh\nexit 0\n");
    let gone = tmp.path().join("gone");
    fs::create_dir(&gone).expect("workdir");
    fs::remove_dir(&gone).expect("delete workdir");

    let error = git_at(program)
        .run(&gone, &["status"])
        .expect_err("deleted workdir");
    assert!(
        matches!(&error, GitError::NotADirectory(path) if path == &gone),
        "{error:?}"
    );
    let error = git_at(tmp.path().join("no-such-git"))
        .run(tmp.path(), &["status"])
        .expect_err("missing git");
    assert!(
        matches!(&error, GitError::Spawn(io) if io.kind() == ErrorKind::NotFound),
        "{error:?}"
    );
}
