//! The three commands that change a repository: switch, pull and fetch.

use std::fs;
use std::path::{Path, PathBuf};

use cthulhu_git::git::{
    GitError, Head, branches, fetch, latest_commit, pull_ff_only, switch_branch,
};

use crate::support::{Repo, git_at, sh_quote, write_executable};

/// `main` has `a.txt=base` and `keep.txt=same`; `other` changes `a.txt`.
/// HEAD is on `main`.
fn diverged() -> Repo {
    let repo = Repo::init("diverge");
    repo.write("a.txt", "base");
    repo.write("keep.txt", "same");
    repo.git(&["add", "a.txt", "keep.txt"]);
    repo.commit("base");
    repo.git(&["switch", "-c", "other"]);
    repo.write("a.txt", "other");
    repo.git(&["commit", "-am", "other tip"]);
    repo.git(&["switch", "main"]);
    repo
}

fn read(repo: &Repo, name: &str) -> String {
    fs::read_to_string(repo.root.join(name)).expect(name)
}

/// A post-checkout hook that logs its third argument, 1 for a branch switch.
/// It goes through the local `core.hooksPath`, which a user's global one
/// cannot replace.
fn log_post_checkout(repo: &Repo, log: &Path) {
    let hooks = repo.tmp.path().join("hooks");
    let body = format!(
        "#!/bin/sh\necho \"post-checkout $3\" >> {}\n",
        sh_quote(log)
    );
    write_executable(&hooks.join("post-checkout"), &body);
    repo.git(&["config", "core.hooksPath", hooks.to_str().expect("utf-8")]);
}

/// After a switch to `name`, HEAD is attached to it, it is the one current
/// branch and sorts first, and the latest commit is its tip.
fn assert_switched_to(case: &str, repo: &Repo, name: &str) {
    let expected = format!("refs/heads/{name}");
    assert_eq!(
        repo.git(&["symbolic-ref", "--quiet", "HEAD"]),
        expected,
        "{case:?}"
    );
    let info = repo.info();
    assert_eq!(info.head, Head::Branch(name.to_owned()), "{case:?}");
    let rows = branches(&repo.git, &repo.root, &info.head).expect(case);
    assert!(
        rows.first()
            .is_some_and(|row| row.current && row.name == name),
        "{case:?}: {rows:?}"
    );
    assert_eq!(rows.iter().filter(|row| row.current).count(), 1, "{case:?}");
    let detail = latest_commit(&repo.git, &repo.root, &info.head)
        .expect(case)
        .expect(case);
    assert_eq!(detail.oid, repo.git(&["rev-parse", &expected]), "{case:?}");
}

#[test]
fn switch_attaches_head_to_the_named_branch() {
    // (case, branch, setup, extra check)
    type Step = fn(&Repo);
    let cases: [(&str, &str, Step, Step); 8] = [
        (
            "a plain branch",
            "other",
            |_| {},
            |repo| {
                assert_eq!(read(repo, "a.txt"), "other");
            },
        ),
        (
            "a nested name",
            "feature/foo",
            |repo| {
                repo.git(&["branch", "feature/foo"]);
            },
            |_| {},
        ),
        // `git branch` refuses these names; the refs are still branches.
        (
            "a name like an option",
            "-switch",
            |repo| {
                repo.git(&["update-ref", "refs/heads/-switch", "HEAD"]);
            },
            |_| {},
        ),
        (
            "a name like a dangerous option",
            "--upload-pack=evil",
            |repo| {
                repo.git(&["update-ref", "refs/heads/--upload-pack=evil", "other"]);
            },
            |_| {},
        ),
        (
            "an edit the switch does not touch",
            "other",
            |repo| {
                repo.write("keep.txt", "edited");
            },
            |repo| {
                assert_eq!(read(repo, "keep.txt"), "edited");
                assert_eq!(repo.git(&["diff", "--cached", "--", "keep.txt"]), "");
                let diff = repo.git(&["diff", "--", "keep.txt"]);
                assert!(diff.contains("-same") && diff.contains("+edited"), "{diff}");
                assert_eq!(read(repo, "a.txt"), "other");
            },
        ),
        (
            "from an unborn orphan",
            "main",
            |repo| {
                repo.git(&["checkout", "--orphan", "unborn"]);
            },
            |repo| {
                assert_eq!(read(repo, "a.txt"), "base");
                let info = repo.info();
                let rows = branches(&repo.git, &info.root, &info.head).expect("branches");
                assert!(rows.iter().all(|row| row.name != "unborn"), "{rows:?}");
            },
        ),
        (
            "from a detached HEAD",
            "main",
            |repo| {
                repo.git(&["checkout", "--detach", "HEAD"]);
            },
            |_| {},
        ),
        (
            "a packed ref",
            "other",
            |repo| {
                repo.git(&["pack-refs", "--all"]);
                let _ = fs::remove_file(repo.root.join(".git/refs/heads/other"));
            },
            |_| {},
        ),
    ];
    for (case, name, setup, check) in cases {
        let repo = diverged();
        setup(&repo);
        switch_branch(&repo.git, &repo.root, name)
            .unwrap_or_else(|error| panic!("{case:?}: {error}"));
        assert_switched_to(case, &repo, name);
        check(&repo);
    }
}

/// Every refusal leaves HEAD and the files where they were. Git's own refusal
/// is a `Command` error naming the branch, never a forced switch.
#[test]
fn switch_refuses_and_leaves_the_repository_unchanged() {
    // Refused before git runs: this git does not exist, so running it would
    // be a spawn error instead.
    let nowhere = git_at(PathBuf::from("/nonexistent/git"));
    for name in ["", " ", "ma\nin", "..", "../../.git", "foo/../../.git"] {
        let result = switch_branch(&nowhere, Path::new("/nonexistent"), name);
        assert!(
            matches!(result, Err(GitError::Rejected { .. })),
            "{name:?}: {result:?}"
        );
    }

    // (case, branch, setup returning where to switch, expected stderr, extra check)
    type Setup = fn(&Repo) -> PathBuf;
    type Check = fn(&Repo, &Path);
    let cases: [(&str, &str, Setup, Option<&str>, Check); 4] = [
        (
            "a dirty file the switch would overwrite",
            "other",
            |repo| {
                repo.write("a.txt", "dirty");
                log_post_checkout(repo, &repo.tmp.path().join("ran.log"));
                repo.root.clone()
            },
            Some("would be overwritten"),
            |repo, _| {
                assert_eq!(read(repo, "a.txt"), "dirty");
                assert_eq!(repo.git(&["diff", "--cached", "--", "a.txt"]), "");
                assert!(!repo.tmp.path().join("ran.log").exists(), "a hook ran");
            },
        ),
        (
            "a branch checked out in a linked worktree",
            "other",
            |repo| {
                let linked = repo.tmp.path().join("linked");
                repo.git(&["worktree", "add", linked.to_str().expect("utf-8"), "other"]);
                repo.root.clone()
            },
            Some("already used by worktree"),
            |repo, _| {
                let linked = repo.tmp.path().join("linked");
                let head = repo.git_in(&linked, &["symbolic-ref", "--quiet", "HEAD"]);
                assert_eq!(head, "refs/heads/other");
            },
        ),
        (
            "a bare repository",
            "other",
            |repo| {
                let bare = repo.tmp.path().join("demo.git");
                let source = repo.root.to_str().expect("utf-8");
                repo.git(&["clone", "--bare", source, bare.to_str().expect("utf-8")]);
                bare
            },
            Some("must be run in a work tree"),
            |_, bare| {
                let mut entries: Vec<_> = fs::read_dir(bare)
                    .expect("bare dir")
                    .map(|entry| entry.expect("entry").file_name())
                    .collect();
                entries.sort();
                assert!(
                    !entries
                        .iter()
                        .any(|name| name == "index" || name == "a.txt"),
                    "a checkout landed in the bare repository: {entries:?}"
                );
            },
        ),
        // `checkout.defaultRemote` from an included file must not make the
        // switch create a branch from a remote. Older git words this
        // refusal differently, so only the result is checked.
        (
            "a branch only a remote has",
            "shared",
            |repo| {
                for remote in ["origin", "evil"] {
                    let path = repo.tmp.path().join(remote);
                    fs::create_dir(&path).expect("remote dir");
                    repo.git_in(&path, &["init", "-b", "main"]);
                    repo.git_in(&path, &["commit", "--allow-empty", "-m", remote]);
                    repo.git_in(&path, &["branch", "shared"]);
                    repo.git(&["remote", "add", remote, path.to_str().expect("utf-8")]);
                    repo.git(&["fetch", remote]);
                }
                let included = repo.tmp.path().join("checkout.gitconfig");
                fs::write(
                    &included,
                    "[checkout]\n\tguess = true\n\tdefaultRemote = evil\n",
                )
                .expect("included config");
                let included = included.to_str().expect("utf-8");
                repo.git(&["config", "--local", "include.path", included]);
                assert_eq!(repo.git(&["config", "checkout.defaultRemote"]), "evil");
                repo.root.clone()
            },
            None,
            |repo, _| {
                let refs = repo.git(&["for-each-ref", "refs/heads/shared"]);
                assert_eq!(refs, "", "the switch created a local branch");
            },
        ),
    ];
    for (case, name, setup, stderr, check) in cases {
        let repo = diverged();
        let root = setup(&repo);
        let head = repo.git_in(&root, &["rev-parse", "HEAD"]);

        let error = switch_branch(&repo.git, &root, name).expect_err(case);
        if let Some(expected) = stderr {
            let GitError::Command {
                command, stderr, ..
            } = &error
            else {
                panic!("{case:?}: expected git's refusal, got {error:?}");
            };
            assert!(stderr.contains(expected), "{case:?}: {stderr}");
            assert!(command.contains(name), "{case:?}: {command}");
            assert!(!command.contains("--force"), "{case:?}: {command}");
        }
        assert_eq!(
            repo.git_in(&root, &["symbolic-ref", "--quiet", "HEAD"]),
            "refs/heads/main",
            "{case:?}"
        );
        assert_eq!(repo.git_in(&root, &["rev-parse", "HEAD"]), head, "{case:?}");
        check(&repo, &root);
    }
}

/// Pull is a fast-forward or nothing, whatever the repository asks for. Each
/// row starts with neither rebase key set, then sets its own.
#[test]
fn pull_ff_only_never_rebases() {
    let origin = Repo::init("origin");
    origin.write("tracked.txt", "base\n");
    origin.git(&["add", "tracked.txt"]);
    origin.git(&["commit", "-m", "base"]);
    let clone = origin.tmp.path().join("clone");
    let source = origin.root.to_str().expect("utf-8");
    origin.git(&["clone", "--", source, clone.to_str().expect("utf-8")]);
    origin.write("tracked.txt", "origin only\n");
    origin.git(&["commit", "-am", "origin"]);
    fs::write(clone.join("tracked.txt"), "local only\n").expect("local edit");
    origin.git_in(&clone, &["commit", "-am", "local"]);
    // A rebase that started would open one of these.
    let marker = origin.tmp.path().join("editor-ran");
    let editor = origin.tmp.path().join("editor.sh");
    write_executable(
        &editor,
        &format!("#!/bin/sh\ntouch {}\nexit 1\n", sh_quote(&marker)),
    );
    for key in ["sequence.editor", "core.editor"] {
        origin.git_in(&clone, &["config", key, editor.to_str().expect("utf-8")]);
    }
    let head = origin.git_in(&clone, &["rev-parse", "HEAD"]);
    let set_rebase = |settings: &[(&str, &str)]| {
        for key in ["pull.rebase", "branch.main.rebase"] {
            // Exits 5 when the key is not set.
            let _ = origin
                .command(&clone)
                .args(["config", "--unset-all", key])
                .status();
        }
        for (key, value) in settings {
            origin.git_in(&clone, &["config", key, value]);
        }
    };

    let cases: [(&str, &[(&str, &str)]); 8] = [
        ("pull.rebase=true", &[("pull.rebase", "true")]),
        ("pull.rebase=merges", &[("pull.rebase", "merges")]),
        ("pull.rebase=m", &[("pull.rebase", "m")]),
        ("pull.rebase=interactive", &[("pull.rebase", "interactive")]),
        ("pull.rebase=i", &[("pull.rebase", "i")]),
        // Removed from git, but --no-rebase means it is never read.
        ("pull.rebase=preserve", &[("pull.rebase", "preserve")]),
        (
            "branch.main.rebase=interactive",
            &[("branch.main.rebase", "interactive")],
        ),
        (
            "both true",
            &[("pull.rebase", "true"), ("branch.main.rebase", "true")],
        ),
    ];
    for (case, settings) in cases {
        set_rebase(settings);
        let result = pull_ff_only(&origin.git, &clone);
        assert!(
            matches!(result, Err(GitError::Command { .. })),
            "{case:?}: {result:?}"
        );
        assert_eq!(
            origin.git_in(&clone, &["rev-parse", "HEAD"]),
            head,
            "{case:?}"
        );
        assert!(!marker.exists(), "{case:?}: an editor ran");
        for state in ["rebase-merge", "rebase-apply", "MERGE_HEAD"] {
            assert!(
                !clone.join(".git").join(state).exists(),
                "{case:?}: {state}"
            );
        }
    }

    // Control: behind instead of diverged, the same pull fast-forwards.
    origin.git_in(&clone, &["reset", "--hard", "HEAD~1"]);
    set_rebase(&[("pull.rebase", "false")]);
    pull_ff_only(&origin.git, &clone).expect("fast-forward");
    assert_eq!(
        origin.git_in(&clone, &["rev-parse", "HEAD"]),
        origin.git(&["rev-parse", "main"])
    );
}

/// A switch runs the repository's filters and hooks as `git switch` in a
/// terminal does: smudge for the file it writes, then post-checkout.
#[cfg(unix)]
#[test]
fn switch_runs_post_checkout_and_smudge_like_the_terminal() {
    let repo = diverged();
    let log = repo.tmp.path().join("ran.log");
    log_post_checkout(&repo, &log);
    let smudge = repo.tmp.path().join("smudge.sh");
    let body = format!("#!/bin/sh\necho smudge >> {}\ncat\n", sh_quote(&log));
    write_executable(&smudge, &body);
    repo.git(&[
        "config",
        "filter.evil.smudge",
        smudge.to_str().expect("utf-8"),
    ]);
    repo.write(".git/info/attributes", "a.txt filter=evil\n");

    switch_branch(&repo.git, &repo.root, "other").expect("switch");
    assert_eq!(read(&repo, "a.txt"), "other");
    assert_eq!(
        fs::read_to_string(&log).expect("log"),
        "smudge\npost-checkout 1\n"
    );
}

/// Fetch updates the tracking ref git itself maps the upstream to: the
/// remote's name has a `/`, and git's upstream is the first `merge` value.
#[test]
fn fetch_updates_the_tracking_ref_git_names_as_upstream() {
    let origin = Repo::init("origin");
    origin.commit("base");
    origin.git(&["branch", "other"]);
    let clone = origin.tmp.path().join("clone");
    let source = origin.root.to_str().expect("utf-8");
    origin.git(&["clone", "--", source, clone.to_str().expect("utf-8")]);
    // Split at its first `/`, `team/origin/main` would name a remote `team`.
    origin.git_in(&clone, &["remote", "rename", "origin", "team/origin"]);
    // `config --get` would read the last value instead.
    origin.git_in(
        &clone,
        &["config", "--add", "branch.main.merge", "refs/heads/other"],
    );
    origin.commit("origin ahead");

    fetch(&origin.git, &clone, "main").expect("fetch");
    assert_eq!(
        origin.git_in(&clone, &["rev-parse", "refs/remotes/team/origin/main"]),
        origin.git(&["rev-parse", "main"])
    );
}
