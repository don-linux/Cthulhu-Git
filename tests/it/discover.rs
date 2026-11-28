//! Finding git: `CTHULHU_GIT`, then `PATH`, then the platform fallbacks,
//! probed for real against fake gits that answer `--version`.

use std::env;
use std::ffi::OsString;
use std::path::Path;

use cthulhu_git::git::{DiscoverInputs, GitError, OVERRIDE_VAR, discover_with};
use tempfile::TempDir;

use crate::support::{fake_git, run_child};

#[derive(Debug)]
enum Expected {
    /// The link that wins, relative to the case's temp dir, and its raw version.
    Found(&'static str, &'static str),
    TooOld(&'static str),
    BadOverride,
    Failed,
    Rejected,
}

struct Case {
    name: &'static str,
    /// Links to the fake git, relative to the temp dir, and their versions.
    fakes: &'static [(&'static str, &'static str)],
    override_path: Option<&'static str>,
    path: &'static [&'static str],
    fallbacks: &'static [&'static str],
    exe_name: &'static str,
    expected: Expected,
}

const CASES: &[Case] = &[
    Case {
        name: "PATH before the fallbacks",
        fakes: &[("path/git", "2.43.0"), ("fallback/git", "2.40.0")],
        override_path: None,
        path: &["path"],
        fallbacks: &["fallback/git"],
        exe_name: "git",
        expected: Expected::Found("path/git", "2.43.0"),
    },
    Case {
        name: "a fallback when PATH has no git",
        fakes: &[("fallback/git", "2.40.0")],
        override_path: None,
        path: &["empty"],
        fallbacks: &["fallback/git"],
        exe_name: "git",
        expected: Expected::Found("fallback/git", "2.40.0"),
    },
    Case {
        name: "the override before PATH, in a directory with a space",
        fakes: &[("path/git", "2.43.0"), ("Program Files/my-git", "2.30.1")],
        override_path: Some("Program Files/my-git"),
        path: &["path"],
        fallbacks: &[],
        exe_name: "git",
        expected: Expected::Found("Program Files/my-git", "2.30.1"),
    },
    Case {
        name: "a release candidate override is too old",
        fakes: &[("pinned/git", "2.15.0-rc0")],
        override_path: Some("pinned/git"),
        path: &[],
        fallbacks: &[],
        exe_name: "git",
        expected: Expected::TooOld("2.15.0-rc0"),
    },
    Case {
        name: "a missing override",
        fakes: &[("path/git", "2.43.0")],
        override_path: Some("pinned/git"),
        path: &["path"],
        fallbacks: &[],
        exe_name: "git",
        expected: Expected::BadOverride,
    },
    Case {
        name: "an override whose --version fails",
        fakes: &[("pinned/git", "broken")],
        override_path: Some("pinned/git"),
        path: &[],
        fallbacks: &[],
        exe_name: "git",
        expected: Expected::Failed,
    },
    Case {
        name: "an override that prints no version",
        fakes: &[("pinned/git", "garbage")],
        override_path: Some("pinned/git"),
        path: &[],
        fallbacks: &[],
        exe_name: "git",
        expected: Expected::Rejected,
    },
    Case {
        name: "a broken candidate falls through",
        fakes: &[("broken/git", "broken"), ("working/git", "2.43.0")],
        override_path: None,
        path: &["broken"],
        fallbacks: &["working/git"],
        exe_name: "git",
        expected: Expected::Found("working/git", "2.43.0"),
    },
    Case {
        name: "an old candidate loses to a newer one",
        fakes: &[("old/git", "2.9.5"), ("new/git", "2.43.0")],
        override_path: None,
        path: &["old", "new"],
        fallbacks: &[],
        exe_name: "git",
        expected: Expected::Found("new/git", "2.43.0"),
    },
    Case {
        name: "only old candidates",
        fakes: &[("old/git", "2.9.5")],
        override_path: None,
        path: &["old"],
        fallbacks: &[],
        exe_name: "git",
        expected: Expected::TooOld("2.9.5"),
    },
    Case {
        name: "Windows names: git.exe, never git or git.cmd",
        fakes: &[
            ("path/git", "2.43.0"),
            ("path/git.cmd", "2.43.0"),
            ("Git/cmd/git.exe", "2.43.0.windows.1"),
        ],
        override_path: None,
        path: &["path"],
        fallbacks: &["Git/cmd/git.exe"],
        exe_name: "git.exe",
        expected: Expected::Found("Git/cmd/git.exe", "2.43.0.windows.1"),
    },
];

#[test]
fn discovery_takes_the_first_supported_git() {
    for case in CASES {
        let name = case.name;
        let tmp = TempDir::with_prefix("cthulhu-discover-").expect("temp dir");
        let at = |relative: &str| tmp.path().join(relative);
        for (link, version) in case.fakes {
            fake_git(&at(link), version, "");
        }
        let inputs = DiscoverInputs {
            override_path: case.override_path.map(|path| at(path).into()),
            path_var: Some(env::join_paths(case.path.iter().map(|dir| at(dir))).expect("PATH")),
            exe_name: case.exe_name,
            fallbacks: case.fallbacks.iter().map(|path| at(path)).collect(),
            skip_paths: Vec::new(),
        };
        match (discover_with(&inputs), &case.expected) {
            (Ok(git), Expected::Found(link, raw)) => {
                assert_eq!(git.path, at(link), "{name:?}");
                assert_eq!(git.version.raw, *raw, "{name:?}");
            }
            (Err(GitError::TooOld(version)), Expected::TooOld(raw)) => {
                assert_eq!(version.raw, *raw, "{name:?}");
            }
            // The message names the variable the user has to fix.
            (Err(error @ GitError::BadOverride(_)), Expected::BadOverride) => {
                // The prefix ends at the space, so a renamed variable cannot
                // hide behind a shared prefix such as `CTHULHU`.
                assert!(
                    error
                        .to_string()
                        .starts_with(&format!("{OVERRIDE_VAR} is set to ")),
                    "{name:?}: {error}"
                );
            }
            (Err(GitError::Command { .. }), Expected::Failed)
            | (Err(GitError::Rejected { .. }), Expected::Rejected) => {}
            (result, expected) => panic!("{name:?}: expected {expected:?}, got {result:?}"),
        }
    }
}

const CHILD_PATH: &str = "CTHULHU_IT_DECOY_PATH";

/// A relative `CTHULHU_GIT` names the file in the working directory, with or
/// without a slash, even when a bare `git` would be found on `PATH`. The
/// working directory belongs to the child, never to this process.
#[test]
fn relative_override_runs_the_git_in_the_working_directory() {
    let tmp = TempDir::with_prefix("cthulhu-discover-").expect("temp dir");
    let cwd = tmp.path().join("cwd");
    let decoy = tmp.path().join("path");
    fake_git(&cwd.join("git"), "2.43.0", "");
    fake_git(&decoy.join("git"), "2.40.0", "");
    let mut path = OsString::from(&decoy);
    path.push(":/usr/bin:/bin");
    run_child("discover::relative_override_child", |child| {
        child
            .current_dir(&cwd)
            .env("PATH", path)
            .env(CHILD_PATH, &decoy);
    });
}

#[test]
#[ignore = "run by relative_override_runs_the_git_in_the_working_directory"]
fn relative_override_child() {
    let Some(decoy) = env::var_os(CHILD_PATH) else {
        return;
    };
    for case in ["git", "./git"] {
        let git = discover_with(&DiscoverInputs {
            override_path: Some(case.into()),
            path_var: Some(decoy.clone()),
            exe_name: "git",
            fallbacks: Vec::new(),
            skip_paths: Vec::new(),
        })
        .expect(case);
        assert_eq!(git.path, Path::new(case), "{case:?}: reported as given");
        assert_eq!(git.version.raw, "2.43.0", "{case:?}: ran the git on PATH");
    }
}
