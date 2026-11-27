//! Opening a path: which repository it is, its root, name and HEAD, and the
//! graph the window reads next, across the states a repository can be in.

use std::fs;
use std::path::{Path, PathBuf};

use cthulhu_git::git::{
    GitError, GraphTip, Head, SHORT_OID_LEN, branch_graph, head_commit_oid, inspect,
};

use crate::support::{Repo, graph, summaries};

#[derive(Debug)]
enum Opens {
    /// The root before canonicalizing, HEAD, and the graph's summaries.
    Repo(PathBuf, Head, Vec<&'static str>),
    NotARepository(PathBuf),
    NotADirectory(PathBuf),
}

/// Builds a repository, and returns it with the path to open and what that gives.
type Case = (&'static str, fn() -> (Repo, PathBuf, Opens));

fn awaken(name: &str) -> Repo {
    let repo = Repo::init(name);
    repo.commit("Awaken");
    repo
}

fn on(repo: Repo, head: Head, graph: Vec<&'static str>) -> (Repo, PathBuf, Opens) {
    let root = repo.root.clone();
    (repo, root.clone(), Opens::Repo(root, head, graph))
}

fn branch(name: &str) -> Head {
    Head::Branch(name.to_owned())
}

fn unborn(name: &str) -> Head {
    Head::Unborn(name.to_owned())
}

/// `file://`, because `--depth` is ignored for a plain local path.
fn file_url(path: &Path) -> String {
    let canonical = dunce::canonicalize(path).expect("canonical");
    let text = canonical.to_str().expect("utf-8 path").replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

#[test]
fn opening_a_path_finds_its_repository_head_and_graph() {
    let mut cases: Vec<Case> = vec![
        ("a branch with a slash", || {
            let repo = awaken("cthulhu-demo");
            repo.git(&["switch", "-c", "feature/necronomicon"]);
            on(repo, branch("feature/necronomicon"), vec!["Awaken"])
        }),
        ("a detached HEAD", || {
            let repo = awaken("cthulhu-demo");
            repo.commit("Second");
            repo.git(&["checkout", "--detach", "HEAD~1"]);
            let oid = repo.git(&["rev-parse", "HEAD"]);
            let short_oid = oid[..SHORT_OID_LEN].to_owned();
            on(repo, Head::Detached { short_oid }, vec!["Second", "Awaken"])
        }),
        ("an unborn branch", || {
            on(Repo::init("cthulhu-demo"), unborn("main"), vec![])
        }),
        ("a subdirectory", || {
            let repo = awaken("cthulhu-demo");
            repo.write("docs/rlyeh/guide.md", "# Guide\n");
            let (repo, _, expected) = on(repo, branch("main"), vec!["Awaken"]);
            let inside = repo.root.join("docs/rlyeh");
            (repo, inside, expected)
        }),
        ("a plain directory", || {
            let repo = Repo::new("plain");
            let root = repo.root.clone();
            (repo, root.clone(), Opens::NotARepository(root))
        }),
        ("a file", || {
            let repo = Repo::new("plain");
            repo.write("notes.txt", "The stars are right\n");
            let file = repo.root.join("notes.txt");
            (repo, file.clone(), Opens::NotADirectory(file))
        }),
        ("a missing path", || {
            let repo = Repo::new("plain");
            let missing = repo.root.join("does-not-exist");
            (repo, missing.clone(), Opens::NotADirectory(missing))
        }),
        ("a linked worktree", || {
            let repo = awaken("cthulhu-demo");
            let linked = repo.tmp.path().join("linked");
            repo.git(&["worktree", "add", linked.to_str().expect("utf-8")]);
            let expected = Opens::Repo(linked.clone(), branch("linked"), vec!["Awaken"]);
            (repo, linked, expected)
        }),
        ("a bare clone", || {
            let repo = awaken("cthulhu-demo");
            let bare = repo.tmp.path().join("demo.git");
            let source = repo.root.to_str().expect("utf-8");
            repo.git(&["clone", "--bare", source, bare.to_str().expect("utf-8")]);
            let expected = Opens::Repo(bare.clone(), branch("main"), vec!["Awaken"]);
            (repo, bare, expected)
        }),
        ("an empty bare repository", || {
            let repo = Repo::new("empty.git");
            repo.git(&["init", "--bare", "-b", "main"]);
            on(repo, unborn("main"), vec![])
        }),
        ("a gitfile that points nowhere", || {
            let repo = Repo::new("broken-gitfile");
            let target = if cfg!(windows) {
                r"C:\does\not\exist"
            } else {
                "/does/not/exist"
            };
            repo.write(".git", &format!("gitdir: {target}\n"));
            let root = repo.root.clone();
            (repo, root.clone(), Opens::NotARepository(root))
        }),
        ("HEAD names a branch that does not exist", || {
            let repo = awaken("cthulhu-demo");
            repo.write(".git/HEAD", "ref: refs/heads/missing\n");
            on(repo, unborn("missing"), vec!["Awaken"])
        }),
        ("HEAD is not a ref", || {
            let repo = awaken("cthulhu-demo");
            repo.write(".git/HEAD", "this is not a ref\n");
            let root = repo.root.clone();
            (repo, root.clone(), Opens::NotARepository(root))
        }),
        ("a shallow clone", || {
            let repo = Repo::init("full");
            repo.commit("first");
            repo.commit("second");
            let shallow = repo.tmp.path().join("shallow");
            let url = file_url(&repo.root);
            let args = ["-c", "protocol.file.allow=always", "clone", "--depth", "1"];
            let target = shallow.to_str().expect("utf-8");
            repo.git(&[&args[..], &[url.as_str(), target]].concat());
            let expected = Opens::Repo(shallow.clone(), branch("main"), vec!["second"]);
            (repo, shallow, expected)
        }),
        ("packed refs and no loose ref", || {
            let repo = awaken("cthulhu-demo");
            repo.git(&["pack-refs", "--all"]);
            let _ = fs::remove_file(repo.root.join(".git/refs/heads/main"));
            on(repo, branch("main"), vec!["Awaken"])
        }),
        ("a name with a quote and a space", || {
            let repo = Repo::init("quote's repo");
            repo.commit("kept");
            on(repo, branch("main"), vec!["kept"])
        }),
    ];
    #[cfg(unix)]
    cases.extend([
        // Git ends its output with one newline; the name's own CR or LF stays.
        (
            "a name ending in LF",
            (|| unix_name(b"endnl\n")) as fn() -> _,
        ),
        ("a name ending in CR", || unix_name(b"endcr\r")),
        ("a symlink loop", || {
            let repo = Repo::new("plain");
            let path = repo.tmp.path().join("loop");
            std::os::unix::fs::symlink(&path, &path).expect("self symlink");
            (repo, path.clone(), Opens::NotADirectory(path))
        }),
    ]);
    // APFS refuses a name that is not UTF-8.
    #[cfg(target_os = "linux")]
    cases.push(("a name that is not UTF-8", || unix_name(b"rlyeh_\xff_deep")));

    for (case, setup) in cases {
        let (repo, path, expected) = setup();
        match (inspect(&repo.git, &path), expected) {
            (Ok(info), Opens::Repo(root, head, rows)) => {
                let root = dunce::canonicalize(&root).expect(case);
                let name = root.file_name().expect(case).to_string_lossy();
                assert_eq!(info.root, root, "{case:?}");
                assert_eq!(info.name, name, "{case:?}");
                assert_eq!(info.head, head, "{case:?}");
                let graph = graph(&repo.git, &info, 10)
                    .unwrap_or_else(|error| panic!("{case:?}: graph: {error}"));
                assert_eq!(summaries(&graph), rows, "{case:?}");
                assert!(!graph.truncated, "{case:?}");
            }
            (Err(error), Opens::NotARepository(path)) => {
                let path = dunce::canonicalize(path).expect(case);
                assert!(
                    matches!(&error, GitError::NotARepository(got) if *got == path),
                    "{case:?}: {error:?}"
                );
                assert!(
                    error.to_string().contains("is not inside a Git repository"),
                    "{case:?}: {error}"
                );
            }
            (Err(error), Opens::NotADirectory(path)) => assert!(
                matches!(&error, GitError::NotADirectory(got) if *got == path),
                "{case:?}: {error:?}"
            ),
            (result, expected) => panic!("{case:?}: expected {expected:?}, got {result:?}"),
        }
    }
}

/// `git branch HEAD` is refused; `refs/heads/HEAD` is still a branch. The
/// branch list cannot read it (`%(refname:short)` prints `heads/HEAD`), so the
/// graph is read from HEAD's commit alone.
#[test]
fn a_branch_named_head_opens() {
    let repo = awaken("cthulhu-demo");
    repo.git(&["update-ref", "refs/heads/HEAD", "HEAD"]);
    repo.git(&["symbolic-ref", "HEAD", "refs/heads/HEAD"]);
    let info = repo.info();
    assert_eq!(info.head, branch("HEAD"));
    let oid = head_commit_oid(&repo.git, &info.root).expect("head oid");
    let graph = branch_graph(&repo.git, &info.root, &[head_tip(oid)], 10).expect("graph");
    assert_eq!(summaries(&graph), ["Awaken"]);
}

fn head_tip(oid: String) -> GraphTip {
    GraphTip {
        name: "HEAD".to_owned(),
        oid,
        color: 0,
        principal: true,
        current: true,
    }
}

#[cfg(unix)]
fn unix_name(name: &'static [u8]) -> (Repo, PathBuf, Opens) {
    use std::os::unix::ffi::OsStrExt;
    on(
        Repo::init(std::ffi::OsStr::from_bytes(name)),
        unborn("main"),
        vec![],
    )
}

/// Inside `.git` there is no work tree. Either the parent repository opens,
/// or the error says why; never a misleading root.
#[test]
fn dot_git_directory_resolves_to_parent_or_clear_error() {
    let repo = awaken("cthulhu-demo");
    let dot_git = repo.root.join(".git");
    match inspect(&repo.git, &dot_git) {
        Ok(info) => {
            assert_eq!(info.root, repo.canonical());
            assert_eq!(info.head, branch("main"));
            assert_eq!(info.name, "cthulhu-demo");
        }
        Err(error) => {
            let message = error.to_string().to_lowercase();
            assert!(
                message.contains("work tree")
                    || message.contains("not inside a git repository")
                    || message.contains("not a directory"),
                "expected the parent repository or a clear error, got {error}"
            );
        }
    }
}

/// The detached tip still verifies, so the repository opens. The parent
/// object is gone, and that failure belongs to the graph read alone.
#[test]
fn detached_head_opens_when_the_history_walk_fails() {
    let repo = Repo::init("detached-missing-parent");
    repo.commit("parent");
    repo.commit("child");
    let parent = repo.git(&["rev-parse", "HEAD~1"]);
    repo.git(&["checkout", "--detach", "HEAD"]);
    let tip = repo.git(&["rev-parse", "HEAD"]);
    let git_dir = repo.git(&["rev-parse", "--absolute-git-dir"]);
    let object = Path::new(&git_dir)
        .join("objects")
        .join(&parent[..2])
        .join(&parent[2..]);
    fs::remove_file(&object).expect("the parent must be a loose object");

    let info = repo.info();
    let detached = Head::Detached {
        short_oid: tip[..SHORT_OID_LEN].to_owned(),
    };
    assert_eq!(info.head, detached);

    let error =
        branch_graph(&repo.git, &info.root, &[head_tip(tip)], 10).expect_err("missing parent");
    assert!(
        matches!(error, GitError::Command { .. } | GitError::Rejected { .. }),
        "a graph failure is not about the open: {error:?}"
    );
    assert!(
        !error.to_string().contains("is not inside a Git repository"),
        "{error}"
    );
}
