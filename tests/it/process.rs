//! The processes git starts: time limits, output caps, and what is left
//! running afterwards.
//!
//! Each call runs on its own thread with a deadline ([`within`]), so a call
//! that ignores its own limit fails instead of hanging the suite.

use std::fs;
use std::io::ErrorKind;

use cthulhu_git::git::GitError;
use tempfile::TempDir;

use crate::support::{git_at, write_executable};
// The fake git is a shell script, so every case that needs it is Unix-only.
#[cfg(unix)]
use {
    crate::support::{
        SETSID_PIDFILE_VAR, assert_pid_dead, become_setsid_sleeper, fake_git, sidecar, within,
    },
    cthulhu_git::git::{
        DiscoverInputs, Git, GraphTip, branch_graph, discover_with, inspect, switch_branch,
    },
    std::path::Path,
    std::time::Duration,
};

#[cfg(unix)]
fn temp() -> TempDir {
    TempDir::with_prefix("cthulhu-process-").expect("temp dir")
}

/// How each case reaches git, so the same fake is met through different
/// entry points.
#[cfg(unix)]
type Call = fn(&Git, &Path) -> Result<(), GitError>;

#[cfg(unix)]
fn read_graph(git: &Git, root: &Path) -> Result<(), GitError> {
    let tip = GraphTip {
        name: "main".to_owned(),
        oid: "a".repeat(40),
        color: 0,
        principal: true,
        current: true,
    };
    branch_graph(git, root, &[tip], 10).map(drop)
}

/// A `--version` that never answers is killed, with what it started, after
/// the 250 ms probe limit, and the search moves on to the next candidate.
#[cfg(unix)]
#[test]
fn a_hung_version_probe_is_killed_and_skipped() {
    for (case, pinned) in [("sleeping override", true), ("sleeping PATH entry", false)] {
        let tmp = temp();
        let hung = fake_git(&tmp.path().join("hung/git"), "sleep", "");
        let fast = fake_git(&tmp.path().join("fast/git"), "2.43.0", "");
        let dirs = [&hung, &fast].map(|git| git.parent().expect("dir").to_path_buf());
        let inputs = DiscoverInputs {
            override_path: pinned.then(|| hung.clone().into()),
            path_var: Some(std::env::join_paths(dirs).expect("PATH")),
            exe_name: "git",
            fallbacks: Vec::new(),
            skip_paths: Vec::new(),
        };
        let found = within(case, Duration::from_secs(5), move || {
            discover_with(&inputs).map(|git| git.path)
        });
        if pinned {
            assert!(found.is_err(), "{case:?}: {found:?}");
        } else {
            assert_eq!(found.ok(), Some(fast), "{case:?}");
        }
        assert_pid_dead(case, &sidecar(&hung, ".pid"));
    }
}

/// A command past its limit is `TimedOut`, and its process group is gone,
/// including a grandchild that left it with `setsid`.
#[cfg(unix)]
#[test]
fn timeout_kills_the_process_group() {
    // (case, fake mode, limit in ms, call, start of the timed-out command)
    let cases: [(&str, &str, u64, Call, &str); 3] = [
        (
            "history log",
            "slow",
            500,
            read_graph,
            "git log --date-order",
        ),
        (
            "switch",
            "slow",
            500,
            |git, root| switch_branch(git, root, "main"),
            "git switch --no-guess -- main",
        ),
        // Long enough for the re-exec'd sleeper to call setsid first.
        (
            "setsid grandchild",
            "setsid",
            2000,
            |git, root| inspect(git, root).map(drop),
            "git rev-parse --is-bare-repository",
        ),
    ];
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

/// Output past the 8 MiB cap is refused before it is used, from a path
/// (`rev-parse`) or from the history log, and the writer is stopped.
#[cfg(unix)]
#[test]
fn output_past_the_cap_is_refused() {
    let cases: [(&str, Call, &str); 2] = [
        (
            "repository root",
            |git, root| inspect(git, root).map(drop),
            "git rev-parse",
        ),
        ("history log", read_graph, "git log --date-order"),
    ];
    for (case, call, command_start) in cases {
        let tmp = temp();
        let fake = fake_git(&tmp.path().join("git"), "2.43.0", "bomb");
        let git = git_at(fake.clone());
        let root = tmp.path().to_path_buf();
        match within(case, Duration::from_secs(5), move || call(&git, &root)) {
            Err(GitError::OutputLimit { command }) => {
                assert!(command.starts_with(command_start), "{case:?}: {command}");
            }
            other => panic!("{case:?}: expected OutputLimit, got {other:?}"),
        }
        assert_pid_dead(case, &sidecar(&fake, ".pid"));
    }
}

/// Git exits while a grandchild still holds its stdout. The call does not
/// wait for the grandchild. Stopping git's group ends a writer in it, and the
/// call returns git's result. A writer that left the group with `setsid` is
/// out of reach, and the call fails instead of returning a partial output.
#[cfg(unix)]
#[test]
fn background_writer_does_not_pin_a_finished_git() {
    for (case, mode, in_group) in [
        ("detached writer", "writer", true),
        ("setsid writer", "setsid-writer", false),
    ] {
        let tmp = temp();
        let fake = fake_git(&tmp.path().join("git"), "2.43.0", mode);
        let git = git_at(fake.clone());
        let cwd = tmp.path().to_path_buf();
        let result = within(case, Duration::from_secs(8), move || {
            git.run(&cwd, &["status"]).map(drop)
        });
        let pid_file = sidecar(&fake, ".pid");
        if in_group {
            assert!(result.is_ok(), "{case:?}: {result:?}");
            assert_pid_dead(case, &pid_file);
        } else {
            assert!(
                matches!(&result, Err(GitError::Spawn(_))),
                "{case:?}: {result:?}"
            );
            // Nothing of git's reaches it, so the test stops it.
            let pid: i32 = fs::read_to_string(&pid_file)
                .expect("pid file")
                .trim()
                .parse()
                .expect("pid");
            let sleeper = rustix::process::Pid::from_raw(pid).filter(|_| pid > 1);
            let _ = rustix::process::kill_process(
                sleeper.expect("a child pid"),
                rustix::process::Signal::KILL,
            );
        }
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
