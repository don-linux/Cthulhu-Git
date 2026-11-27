//! What a repository or the inherited environment may plant, and the app
//! never runs or follows.
//!
//! Every planted command appends its name to a log. A positive control with
//! plain git proves each one is armed, so a quiet log means the app ignored
//! it, not that the fixture was inert.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use cthulhu_git::git::{
    GitError, Head, branches, commit_hover, fetch, file_diff, glance, head_commit_oid, inspect,
    latest_commit, remotes,
};
use tempfile::TempDir;

use crate::support::{
    Repo, fake_git, git_at, graph, run_child, sh_quote, sidecar, summaries, system_git, within,
    write_executable,
};

/// A script that logs `name` to `log`, then passes its input through so a
/// filter or textconv still produces output.
fn plant(dir: &Path, name: &str, log: &Path) -> String {
    let path = dir.join(format!("{name}.sh"));
    let body = format!("#!/bin/sh\necho {name} >> {}\ncat\n", sh_quote(log));
    write_executable(&path, &body);
    path.display().to_string()
}

/// Every read the window makes, in a repository whose config plants a
/// command everywhere git could run one: clean and process filters (one of
/// them through `include.path`), a textconv and an external diff, a
/// signature program, fsmonitor, and a hook.
#[test]
fn hostile_repository_config_never_runs_on_reads() {
    let repo = Repo::init("hostile");
    for name in ["a.txt", "b.txt", "c.txt"] {
        repo.write(name, "fhtagn\n");
    }
    repo.write(
        ".gitattributes",
        "a.txt filter=cleaner diff=pwn\nb.txt filter=processor\nc.txt filter=included\n",
    );
    repo.git(&["add", "."]);
    repo.git(&["commit", "-m", "first"]);
    let first = repo.git(&["rev-parse", "HEAD"]);
    let tree = repo.git(&["rev-parse", "HEAD^{tree}"]);
    // A signature header is all `log.showSignature` needs to call gpg.
    let signed = repo.hash_commit(
        format!(
            "tree {tree}\nparent {first}\n\
             author cthulhu <cthulhu@example.invalid> 1700000000 +0000\n\
             committer cthulhu <cthulhu@example.invalid> 1700000000 +0000\n\
             gpgsig -----BEGIN PGP SIGNATURE-----\n \n iQEzBAABCAAdFiEE\n -----END PGP SIGNATURE-----\n\n\
             signed\n"
        )
        .as_bytes(),
    );
    repo.git(&["update-ref", "refs/heads/main", &signed]);

    // Planted after the commits, so the fixture's own git never met them.
    let scripts = repo.tmp.path();
    let log = scripts.join("ran.log");
    let hooks = scripts.join("hooks");
    write_executable(
        &hooks.join("post-index-change"),
        &format!("#!/bin/sh\necho hook >> {}\n", sh_quote(&log)),
    );
    let included = scripts.join("included.gitconfig");
    fs::write(
        &included,
        format!(
            "[filter \"included\"]\n\tclean = {}\n[diff \"pwn\"]\n\ttextconv = {}\n\
             [diff]\n\texternal = {}\n[gpg]\n\tprogram = {}\n[log]\n\tshowSignature = true\n\
             [core]\n\tfsmonitor = {}\n\thooksPath = {}\n",
            plant(scripts, "included", &log),
            plant(scripts, "textconv", &log),
            plant(scripts, "external", &log),
            plant(scripts, "gpg", &log),
            plant(scripts, "fsmonitor", &log),
            hooks.display(),
        ),
    )
    .expect("included config");
    let clean = plant(scripts, "clean", &log);
    let process = plant(scripts, "process", &log);
    repo.git(&["config", "filter.cleaner.clean", &clean]);
    repo.git(&["config", "filter.processor.process", &process]);
    repo.git(&["config", "include.path", included.to_str().expect("utf-8")]);
    // An old mtime makes git re-read each file, through its filter.
    for name in ["a.txt", "b.txt", "c.txt"] {
        let file = fs::File::options()
            .write(true)
            .open(repo.root.join(name))
            .expect(name);
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(86_400))
            .expect("mtime");
    }

    let (git, root) = (&repo.git, &repo.root);
    let quiet = |case: &str| {
        let ran = fs::read_to_string(&log).unwrap_or_default();
        assert!(ran.is_empty(), "{case:?} ran a planted command: {ran}");
    };
    let info = inspect(git, root).expect("inspect");
    quiet("inspect");
    assert_eq!(info.head, Head::Branch("main".to_owned()));
    let listed = branches(git, root, &info.head).expect("branches");
    quiet("branches");
    assert_eq!(listed[0].commit_count, 2);
    let latest = latest_commit(git, root, &info.head).expect("latest commit");
    quiet("latest_commit");
    assert_eq!(latest.map(|detail| detail.oid), Some(signed.clone()));
    assert_eq!(head_commit_oid(git, root).expect("head oid"), signed);
    quiet("head_commit_oid");
    assert!(remotes(git, root).expect("remotes").is_empty());
    quiet("remotes");
    let glanced = glance(git, root).expect("glance");
    quiet("glance");
    assert_eq!(glanced.latest.expect("glance commit").subject, "signed");
    let graph = graph(git, &info, 10).expect("graph");
    quiet("branch_graph");
    assert_eq!(summaries(&graph), ["signed", "first"]);
    let hover = commit_hover(git, root, &first).expect("hover");
    quiet("commit_hover");
    assert_eq!(hover.files.len(), 4, "{:?}", hover.files);
    for name in ["a.txt", "b.txt", "c.txt"] {
        let diff = file_diff(git, root, &first, name).expect(name);
        quiet(name);
        assert!(
            diff.lines.iter().any(|line| line.text == "fhtagn"),
            "{name:?}: {diff:?}"
        );
    }

    // Plain git, without the app's options, runs every one of them.
    let controls: [(&[&str], &[&str]); 4] = [
        (
            &["status"],
            &["clean", "process", "included", "fsmonitor", "hook"],
        ),
        (&["log", "-1"], &["gpg"]),
        (&["show", "--format=", &first, "--", "a.txt"], &["textconv"]),
        (&["show", "--ext-diff", "--format=", &first], &["external"]),
    ];
    for (args, planted) in controls {
        let _ = fs::remove_file(&log);
        let _ = Command::new(&git.path)
            .current_dir(root)
            .stdin(Stdio::null())
            .args(args)
            .output();
        let ran = fs::read_to_string(&log).unwrap_or_default();
        for name in planted {
            assert!(
                ran.lines().any(|line| line == *name),
                "{args:?} did not run {name}, so the fixture is not armed: {ran:?}"
            );
        }
    }
}

const CHILD_DIR: &str = "CTHULHU_IT_HOSTILE_DIR";
const TRACES: [&str; 3] = ["GIT_TRACE", "GIT_TRACE2", "GIT_TRACE2_EVENT"];
const HISTORY: [&str; 3] = ["GIT_REPLACE_REF_BASE", "GIT_SHALLOW_FILE", "GIT_GRAFT_FILE"];

/// Every inherited variable that could redirect git, hide or rewrite the
/// history, load config, run a helper, or write a trace, set at once in a
/// child of this binary. Each config source carries its own sentinel, and
/// the child proves with plain git that every variable is live before the
/// app reads, fetches from two remotes, and must come back unaffected.
#[test]
fn hostile_inherited_environment_is_ignored() {
    let repo = Repo::init("target");
    repo.commit("first");
    repo.git(&["branch", "other"]);
    repo.commit("second");
    let head = repo.git(&["rev-parse", "HEAD"]);
    // Nothing listens on port 1, so each fetch reaches git and fails to connect.
    repo.git(&["config", "credential.helper", ""]);
    for (branch, remote, url) in [
        ("main", "origin", "ssh://127.0.0.1:1/cthulhu.git"),
        ("other", "web", "https://127.0.0.1:1/cthulhu.git"),
    ] {
        repo.git(&["remote", "add", remote, url]);
        repo.git(&["config", &format!("branch.{branch}.remote"), remote]);
        let merge = format!("refs/heads/{branch}");
        repo.git(&["config", &format!("branch.{branch}.merge"), &merge]);
    }
    let decoy = Repo::init("decoy");
    decoy.commit("decoy");
    decoy.git(&["switch", "-c", "decoy"]);

    let dir = repo.tmp.path();
    let log = dir.join("ran.log");
    let poke = |path: &Path, name: &str| {
        let body = format!("#!/bin/sh\necho {name} >> {}\nexit 1\n", sh_quote(&log));
        write_executable(path, &body);
        path.display().to_string()
    };
    let ssh = poke(&dir.join("ssh.sh"), "ssh");
    poke(&dir.join("exec/git-remote-https"), "helper");
    let tree = repo.git(&["rev-parse", "HEAD^{tree}"]);
    let evil = repo.git(&["commit-tree", &tree, "-m", "EVIL-REPLACE"]);
    repo.write(&format!(".git/refs/hidden/{head}"), &format!("{evil}\n"));
    for name in ["shallow", "graft"] {
        fs::write(dir.join(name), format!("{head}\n")).expect(name);
    }
    let decoy_root = decoy.canonical().display().to_string();
    for scope in ["global", "system"] {
        let config = format!(
            "[cthulhu]\n\tsentinel = {scope}\n[core]\n\tsshCommand = {ssh}\n\tworktree = {decoy_root}\n"
        );
        fs::write(dir.join(format!("{scope}.gitconfig")), config).expect(scope);
    }
    let parameters = format!(
        "'cthulhu.sentinel'='parameters' 'core.sshcommand'='{ssh}' 'core.worktree'='{decoy_root}'"
    );
    let count = [
        ("cthulhu.sentinel", "count"),
        ("core.sshCommand", ssh.as_str()),
        ("core.worktree", decoy_root.as_str()),
    ];

    let mut env: Vec<(String, String)> = vec![
        (CHILD_DIR.into(), dir.display().to_string()),
        ("GIT_DIR".into(), format!("{decoy_root}/.git")),
        ("GIT_WORK_TREE".into(), decoy_root.clone()),
        ("GIT_CONFIG_PARAMETERS".into(), parameters),
        ("GIT_CONFIG_COUNT".into(), count.len().to_string()),
        (
            "GIT_EXEC_PATH".into(),
            dir.join("exec").display().to_string(),
        ),
        ("GIT_REPLACE_REF_BASE".into(), "refs/hidden/".into()),
        (
            "GIT_SHALLOW_FILE".into(),
            dir.join("shallow").display().to_string(),
        ),
        (
            "GIT_GRAFT_FILE".into(),
            dir.join("graft").display().to_string(),
        ),
    ];
    for (index, (key, value)) in count.iter().enumerate() {
        env.push((format!("GIT_CONFIG_KEY_{index}"), (*key).into()));
        env.push((format!("GIT_CONFIG_VALUE_{index}"), (*value).into()));
    }
    for scope in ["global", "system"] {
        let file = dir.join(format!("{scope}.gitconfig"));
        let name = format!("GIT_CONFIG_{}", scope.to_uppercase());
        env.push((name, file.display().to_string()));
    }
    for (index, name) in TRACES.iter().enumerate() {
        let file = dir.join(format!("trace-{index}"));
        env.push(((*name).into(), file.display().to_string()));
    }
    within("hostile environment", Duration::from_secs(60), move || {
        run_child("hardening::hostile_environment_child", |child| {
            // An inherited ssh command would win over core.sshCommand and
            // leave the ssh half of this test with nothing to catch.
            for name in ["GIT_SSH_COMMAND", "GIT_SSH", "GIT_CONFIG_NOSYSTEM"] {
                child.env_remove(name);
            }
            child.envs(env);
        });
    });
}

#[test]
#[ignore = "run by hostile_inherited_environment_is_ignored"]
fn hostile_environment_child() {
    let Some(dir) = std::env::var_os(CHILD_DIR).map(PathBuf::from) else {
        return;
    };
    let target = dir.join("target");
    let log = dir.join("ran.log");
    let traces = TRACES.map(|name| PathBuf::from(std::env::var_os(name).expect(name)));
    let git = system_git();
    // Plain git in the target, with this environment minus `dropped`.
    let raw = |args: &[&str], dropped: &[&str]| {
        let mut command = Command::new(&git.path);
        command.current_dir(&target).stdin(Stdio::null());
        for name in dropped {
            command.env_remove(name);
        }
        let output = command.args(args).output().expect("plain git");
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    };

    // Every variable is live for plain git.
    raw(&["--version"], &[]);
    for trace in &traces {
        assert!(trace.is_file(), "{trace:?} was not written");
        fs::remove_file(trace).expect("trace");
    }
    let sentinels = raw(&["config", "--get-all", "cthulhu.sentinel"], &TRACES);
    let mut sentinels: Vec<&str> = sentinels.lines().collect();
    sentinels.sort_unstable();
    assert_eq!(sentinels, ["count", "global", "parameters", "system"]);
    let toplevel = raw(&["rev-parse", "--show-toplevel"], &TRACES);
    assert!(toplevel.ends_with("decoy"), "GIT_DIR: {toplevel}");
    let local = [&TRACES[..], &["GIT_DIR", "GIT_WORK_TREE"]].concat();
    for (name, shown) in HISTORY.iter().zip(["EVIL-REPLACE", "second", "second"]) {
        let others = HISTORY.iter().copied().filter(|other| other != name);
        let dropped: Vec<&str> = local.iter().copied().chain(others).collect();
        assert_eq!(raw(&["log", "--format=%s"], &dropped), shown, "{name:?}");
    }
    for (remote, planted) in [("origin", "ssh"), ("web", "helper")] {
        let _ = fs::remove_file(&log);
        raw(&["fetch", remote], &local);
        let ran = fs::read_to_string(&log).unwrap_or_default();
        assert_eq!(ran.trim(), planted, "{remote:?}");
    }
    let _ = fs::remove_file(&log);

    // The app sees the target as it is.
    let info = inspect(&git, &target).expect("inspect");
    assert_eq!(info.root, dunce::canonicalize(&target).expect("target"));
    assert_eq!(info.head, Head::Branch("main".to_owned()));
    let graph = graph(&git, &info, 10).expect("graph");
    assert_eq!(summaries(&graph), ["second", "first"]);
    let latest = latest_commit(&git, &info.root, &info.head).expect("latest commit");
    assert_eq!(
        latest.map(|detail| detail.message),
        Some("second\n".to_owned())
    );
    for branch in ["main", "other"] {
        let fetched = fetch(&git, &info.root, branch);
        assert!(
            matches!(fetched, Err(GitError::Command { .. })),
            "{branch:?}: the fetch did not reach git: {fetched:?}"
        );
    }
    let ran = fs::read_to_string(&log).unwrap_or_default();
    assert!(ran.is_empty(), "the app ran a planted command: {ran}");
    for trace in &traces {
        assert!(!trace.exists(), "the app wrote {trace:?}");
    }
}

/// The `safe.directory` command in the error is one shell word per argument,
/// whatever the path holds: pasted into a shell, it runs git once with the
/// whole path, and nothing else.
#[test]
fn dubious_ownership_suggestion_is_shell_quoted() {
    let tmp = TempDir::with_prefix("cthulhu-dubious-").expect("temp dir");
    let base = tmp.path().display();
    let marker = tmp.path().join("injected");
    let failing = fake_git(&tmp.path().join("fail/git"), "2.43.0", "fail");
    let recorder = fake_git(&tmp.path().join("bin/git"), "2.43.0", "argv");
    let paths = [
        format!("{base}/owned repo"),
        format!("{base}/o'reilly"),
        format!("{base}/owned\n/usr/bin/touch {}", marker.display()),
    ];
    for path in paths {
        let stderr = format!("fatal: detected dubious ownership in repository at '{path}'\n");
        fs::write(sidecar(&failing, ".stderr"), stderr).expect("stderr");
        let error = inspect(&git_at(failing.clone()), tmp.path()).expect_err(&path);
        assert!(
            matches!(error, GitError::UnsafeRepository(_)),
            "{path:?}: {error:?}"
        );
        let message = error.to_string();
        let (_, suggestion) = message
            .split_once("If you trust it, run: ")
            .unwrap_or_else(|| panic!("{path:?}: no command in {message:?}"));

        let _ = fs::remove_file(sidecar(&recorder, ".argv"));
        let _ = Command::new("/bin/sh")
            .args(["-c", suggestion])
            .env("PATH", recorder.parent().expect("bin"))
            .stdin(Stdio::null())
            .status();
        assert!(
            !marker.exists(),
            "{path:?}: {suggestion} ran another command"
        );
        let argv = fs::read(sidecar(&recorder, ".argv")).expect("git ran");
        let args: Vec<&[u8]> = argv
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .collect();
        let expected: [&[u8]; 5] = [
            b"config",
            b"--global",
            b"--add",
            b"safe.directory",
            path.as_bytes(),
        ];
        assert_eq!(args, expected, "{path:?}: {suggestion}");
    }
}
