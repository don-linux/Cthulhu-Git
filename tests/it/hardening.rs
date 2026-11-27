//! What a repository or the inherited environment may plant, and the app
//! never runs or follows.
//!
//! Every planted command appends its name to a log. A positive control with
//! plain git proves each one is armed, so a quiet log means the app ignored
//! it, not that the fixture was inert.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use cthulhu_git::git::{
    Head, branches, commit_hover, file_diff, glance, head_commit_oid, inspect, latest_commit,
    remotes,
};

use crate::support::{Repo, graph, sh_quote, summaries, write_executable};

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
