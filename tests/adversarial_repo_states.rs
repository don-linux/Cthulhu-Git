//! Adversarial repository states for `inspect`, `history`, and `switch_branch`.
//!
//! Fixtures shell out to the system git. Author identity is set on those
//! commands only, so the test process environment stays untouched.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use cthulhu_git::git::{
    Branch, Git, GitError, Head, History, RepoInfo, SHORT_OID_LEN, branches, history, inspect,
    latest_commit, switch_branch,
};
use tempfile::TempDir;

struct Repo {
    tmp: TempDir,
    root: PathBuf,
    git: Git,
}

impl Repo {
    fn init_named(name: impl AsRef<OsStr>) -> Self {
        let git = Git::discover().expect("system git");
        let tmp = TempDir::with_prefix("cthulhu-adv-").expect("temp dir");
        let root = tmp.path().join(name.as_ref());
        fs::create_dir(&root).expect("repo dir");
        let repo = Self { tmp, root, git };
        let cwd = repo.root.clone();
        repo.run(&cwd, &["init", "-b", "main"]);
        repo
    }

    fn init_sha256() -> Self {
        let git = Git::discover().expect("system git");
        let tmp = TempDir::with_prefix("cthulhu-adv-").expect("temp dir");
        let root = tmp.path().join("sha256-demo");
        fs::create_dir(&root).expect("repo dir");
        let repo = Self { tmp, root, git };
        let cwd = repo.root.clone();
        repo.run(&cwd, &["init", "--object-format=sha256", "-b", "main"]);
        repo
    }

    fn init_bare() -> Self {
        let git = Git::discover().expect("system git");
        let tmp = TempDir::with_prefix("cthulhu-adv-").expect("temp dir");
        let root = tmp.path().join("empty.git");
        let repo = Self { tmp, root, git };
        let cwd = repo.tmp.path().to_path_buf();
        let spec = repo.root.to_str().expect("utf8 bare path").to_owned();
        repo.run(&cwd, &["init", "--bare", "-b", "main", &spec]);
        repo
    }

    fn with_commit() -> Self {
        let repo = Self::init_named("cthulhu-demo");
        repo.commit("Awaken");
        repo
    }

    fn command(&self, cwd: &Path) -> Command {
        let mut command = Command::new(&self.git.path);
        command
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "cthulhu")
            .env("GIT_AUTHOR_EMAIL", "cthulhu@example.invalid")
            .env("GIT_COMMITTER_NAME", "cthulhu")
            .env("GIT_COMMITTER_EMAIL", "cthulhu@example.invalid")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.tmp.path().join("empty.gitconfig"))
            .env(
                "GIT_CONFIG_SYSTEM",
                self.tmp.path().join("empty.system.gitconfig"),
            )
            .args(["-c", "commit.gpgSign=false"]);
        let hooks = self.tmp.path().join("empty-hooks");
        let _ = fs::create_dir_all(&hooks);
        let _ = fs::write(self.tmp.path().join("empty.gitconfig"), "");
        let _ = fs::write(self.tmp.path().join("empty.system.gitconfig"), "");
        command
            .arg("-c")
            .arg(format!("core.hooksPath={}", hooks.display()));
        command
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> std::process::Output {
        let output = self.command(cwd).args(args).output().expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn git_at(&self, cwd: &Path, args: &[&str]) -> String {
        let output = self.run(cwd, args);
        String::from_utf8(output.stdout)
            .expect("utf8")
            .trim()
            .to_owned()
    }

    fn git(&self, args: &[&str]) -> String {
        self.git_at(&self.root, args)
    }

    fn commit(&self, message: &str) {
        self.git(&["commit", "--allow-empty", "-m", message]);
    }

    fn commit_bytes(&self, message: &[u8]) {
        let path = self.tmp.path().join("message.txt");
        fs::write(&path, message).expect("write message");
        let path = path.to_str().expect("utf8 message path").to_owned();
        self.git(&["commit", "--allow-empty", "-F", &path]);
    }

    fn commit_at(&self, message: &str, unix: i64) {
        let date = format!("{unix} +0000");
        let cwd = self.root.clone();
        let output = self
            .command(&cwd)
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .args(["commit", "--allow-empty", "-m", message])
            .output()
            .expect("commit");
        assert!(
            output.status.success(),
            "git commit failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn commit_in(&self, cwd: &Path, message: &str) {
        let cwd = cwd.to_path_buf();
        let output = self
            .command(&cwd)
            .args(["commit", "--allow-empty", "-m", message])
            .output()
            .expect("commit");
        assert!(
            output.status.success(),
            "git commit failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn add(&self, path: &str) {
        self.git(&["add", "--", path]);
    }

    fn write(&self, name: &str, contents: &str) {
        fs::write(self.root.join(name), contents).expect("write");
    }

    fn canonical(&self) -> PathBuf {
        dunce::canonicalize(&self.root).expect("canonical root")
    }

    fn info(&self) -> RepoInfo {
        inspect(&self.git, &self.root).expect("inspect")
    }

    fn open_history(&self, limit: usize) -> Result<History, GitError> {
        let info = inspect(&self.git, &self.root)?;
        history(&self.git, &info.root, &info.head, limit)
    }

    /// `git commit` and plain `git hash-object` reject a NUL in the message.
    /// `--literally` stores the bytes anyway.
    fn hash_commit_literally(&self, object: &[u8]) -> String {
        let path = self.tmp.path().join("commit-object");
        fs::write(&path, object).expect("write commit object");
        let file = File::open(&path).expect("open commit object");
        let output = self
            .command(&self.root)
            .args([
                "hash-object",
                "--literally",
                "-t",
                "commit",
                "-w",
                "--stdin",
            ])
            .stdin(Stdio::from(file))
            .output()
            .expect("hash-object");
        assert!(
            output.status.success(),
            "git hash-object --literally failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("utf8")
            .trim()
            .to_owned()
    }
}

fn assert_subject_preserved(summary: &str) {
    let repo = Repo::init_named("subjects");
    let mut message = summary.as_bytes().to_vec();
    message.push(b'\n');
    repo.commit_bytes(&message);
    let hist = repo
        .open_history(5)
        .expect("weird but valid subject must not fail history");
    assert_eq!(hist.commits.len(), 1);
    assert_eq!(hist.commits[0].summary, summary);
    assert!(!hist.truncated);
}

/// A bare repo is a repository. Opening it yields a branch/HEAD, or a typed
/// error that says the repository is bare — not "not a git repository".
fn assert_bare_open(result: Result<RepoInfo, GitError>, canonical: &Path, head: Head) {
    match result {
        Ok(info) => {
            assert_eq!(info.root, canonical);
            assert_eq!(info.head, head);
            let expected_name = canonical
                .file_name()
                .expect("bare repo name")
                .to_string_lossy();
            assert_eq!(info.name, expected_name);
        }
        Err(error) => {
            let message = error.to_string();
            assert!(
                !matches!(error, GitError::NotARepository(_)),
                "bare repository must not be NotARepository: {message}"
            );
            assert!(
                !message.contains("is not inside a Git repository"),
                "bare repository must not be described as missing: {message}"
            );
            assert!(
                message.to_ascii_lowercase().contains("bare"),
                "expected a usable RepoInfo or an error that acknowledges a bare repository, got {message}"
            );
        }
    }
}

#[test]
fn nul_byte_in_commit_subject_stays_in_history() {
    // "nul" is a reserved device name on Windows, so the directory cannot be called that.
    let repo = Repo::init_named("nul-subject");
    repo.commit("normal parent");
    let parent = repo.git(&["rev-parse", "HEAD"]);
    let tree = repo.git(&["write-tree"]);
    let mut body = format!(
        "tree {tree}\nparent {parent}\n\
         author cthulhu <cthulhu@example.invalid> 1 +0000\n\
         committer cthulhu <cthulhu@example.invalid> 1 +0000\n\n"
    )
    .into_bytes();
    body.extend_from_slice(b"subject with NUL\x00 and more\n");
    let oid = repo.hash_commit_literally(&body);
    repo.git(&["update-ref", "refs/heads/main", &oid]);

    let hist = repo.open_history(10).unwrap_or_else(|error| {
        panic!("history must stay Ok when a commit subject contains NUL, got {error}")
    });
    assert_eq!(hist.commits.len(), 2, "{hist:?}");
    assert_eq!(hist.commits[0].oid, oid);
    assert!(
        hist.commits[0].summary.starts_with("subject with NUL"),
        "summary may truncate at the NUL but must keep the prefix, got {:?}",
        hist.commits[0].summary
    );
    assert_eq!(hist.commits[1].summary, "normal parent");
    assert!(!hist.truncated);
}

#[test]
fn carriage_return_subject_is_preserved() {
    assert_subject_preserved("line with CR\r inside");
}

#[test]
fn bidi_override_subject_is_preserved() {
    assert_subject_preserved("before \u{202E} after");
}

#[test]
fn ascii_control_subject_is_preserved() {
    assert_subject_preserved("\u{1}\u{7}\u{1b}\u{7f} controls");
}

#[test]
fn one_mebibyte_subject_is_returned() {
    let summary = "A".repeat(1024 * 1024);
    assert_subject_preserved(&summary);
}

#[test]
fn sha256_branch_and_history_use_full_oids() {
    let repo = Repo::init_sha256();
    assert_eq!(repo.git(&["rev-parse", "--show-object-format"]), "sha256");
    repo.commit("hashed");
    let oid = repo.git(&["rev-parse", "HEAD"]);
    assert_eq!(oid.len(), 64, "{oid}");
    assert!(oid.chars().all(|ch| ch.is_ascii_hexdigit()), "{oid}");

    let info = repo.info();
    assert_eq!(info.head, Head::Branch("main".to_owned()));

    let hist = repo.open_history(10).expect("sha256 history");
    assert_eq!(hist.commits.len(), 1);
    assert_eq!(hist.commits[0].oid, oid);
    assert_eq!(hist.commits[0].oid.len(), 64);
    assert_eq!(hist.commits[0].short_oid(), &oid[..SHORT_OID_LEN]);
    assert_eq!(SHORT_OID_LEN, 12);
    assert_eq!(hist.commits[0].summary, "hashed");
}

#[test]
fn linked_worktree_reports_its_own_root() {
    let repo = Repo::with_commit();
    let linked = repo.tmp.path().join("linked");
    let linked_arg = linked.to_str().expect("utf8").to_owned();
    repo.git(&["worktree", "add", &linked_arg]);

    let info = inspect(&repo.git, &linked).expect("inspect worktree");
    let linked_root = dunce::canonicalize(&linked).expect("canonical worktree");
    assert_eq!(info.root, linked_root);
    assert_ne!(info.root, repo.canonical());
    assert_eq!(info.name, "linked");
    assert_eq!(info.head, Head::Branch("linked".to_owned()));
}

#[test]
fn submodule_reports_submodule_root() {
    let super_repo = Repo::init_named("super");
    super_repo.commit("super initial");

    let origin = super_repo.tmp.path().join("origin");
    fs::create_dir(&origin).expect("origin dir");
    super_repo.run(&origin, &["init", "-b", "main"]);
    super_repo.commit_in(&origin, "module initial");

    let url = format!(
        "file://{}",
        dunce::canonicalize(&origin)
            .expect("canonical origin")
            .display()
    );
    let super_root = super_repo.root.clone();
    super_repo.git_at(
        &super_root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            &url,
            "module",
        ],
    );
    super_repo.commit("add module");

    let module = super_repo.root.join("module");
    let info = inspect(&super_repo.git, &module).expect("inspect submodule");
    let module_root = dunce::canonicalize(&module).expect("canonical module");
    assert_eq!(info.root, module_root);
    assert_ne!(info.root, super_repo.canonical());
    assert_eq!(info.name, "module");
    assert_eq!(info.head, Head::Branch("main".to_owned()));
}

#[test]
fn bare_clone_is_not_reported_as_not_a_repository() {
    let repo = Repo::with_commit();
    let bare = repo.tmp.path().join("demo.git");
    let bare_arg = bare.to_str().expect("utf8").to_owned();
    let src = repo.root.to_str().expect("utf8").to_owned();
    let cwd = repo.tmp.path().to_path_buf();
    repo.git_at(&cwd, &["clone", "--bare", &src, &bare_arg]);
    assert_eq!(
        repo.git_at(&bare, &["rev-parse", "--is-bare-repository"]),
        "true"
    );

    let canonical = dunce::canonicalize(&bare).expect("canonical bare");
    assert_bare_open(
        inspect(&repo.git, &bare),
        &canonical,
        Head::Branch("main".to_owned()),
    );
}

#[test]
fn bare_init_is_not_reported_as_not_a_repository() {
    let repo = Repo::init_bare();
    assert_eq!(
        repo.git(&["rev-parse", "--is-bare-repository"]),
        "true",
        "fixture is not bare"
    );
    assert_bare_open(
        inspect(&repo.git, &repo.root),
        &repo.canonical(),
        Head::Unborn("main".to_owned()),
    );
}

#[test]
fn dot_git_directory_resolves_to_parent_or_clear_error() {
    let repo = Repo::with_commit();
    let dot_git = repo.root.join(".git");
    assert!(dot_git.is_dir());

    match inspect(&repo.git, &dot_git) {
        Ok(info) => {
            assert_eq!(info.root, repo.canonical());
            assert_eq!(info.head, Head::Branch("main".to_owned()));
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

#[test]
fn broken_gitfile_is_a_typed_error() {
    let repo = Repo::init_named("gitfile-host");
    let broken = repo.tmp.path().join("broken-gitfile");
    fs::create_dir(&broken).expect("dir");
    fs::write(broken.join(".git"), "gitdir: /does/not/exist\n").expect("gitfile");

    let error = inspect(&repo.git, &broken).expect_err("missing gitdir");
    // inspect canonicalizes first. macOS rewrites /var to /private/var, and
    // Windows may rewrite the temp path, so either form is the same directory.
    let canonical = dunce::canonicalize(&broken).unwrap_or_else(|_| broken.clone());
    assert!(
        matches!(
            &error,
            GitError::NotARepository(path) if path == &broken || path == &canonical
        ) || matches!(error, GitError::Command { .. } | GitError::Rejected { .. }),
        "typed error, got {error:?}"
    );
    let _ = error.to_string();
}

#[test]
fn missing_head_target_is_unborn_or_typed_error() {
    let repo = Repo::with_commit();
    fs::write(repo.root.join(".git/HEAD"), "ref: refs/heads/missing\n").expect("write HEAD");

    match inspect(&repo.git, &repo.root) {
        Ok(info) => {
            assert!(
                matches!(info.head, Head::Unborn(_) | Head::Detached { .. }),
                "missing HEAD target must not look like a normal branch, got {:?}",
                info.head
            );
            if matches!(info.head, Head::Unborn(_)) {
                assert_eq!(
                    history(&repo.git, &info.root, &info.head, 5).expect("unborn history"),
                    History::default()
                );
            }
        }
        Err(error) => {
            assert!(
                matches!(
                    error,
                    GitError::Command { .. }
                        | GitError::Rejected { .. }
                        | GitError::NotARepository(_)
                ),
                "typed error, got {error:?}"
            );
            let _ = error.to_string();
        }
    }
}

#[test]
fn garbage_head_is_typed_error_or_unborn() {
    let repo = Repo::with_commit();
    fs::write(repo.root.join(".git/HEAD"), "this is not a ref\n").expect("write HEAD");

    match inspect(&repo.git, &repo.root) {
        Ok(info) => {
            assert!(
                matches!(info.head, Head::Unborn(_) | Head::Detached { .. }),
                "garbage HEAD must not look like a normal branch, got {:?}",
                info.head
            );
            let hist = history(&repo.git, &info.root, &info.head, 5);
            if let Err(error) = hist {
                let _ = error.to_string();
            }
        }
        Err(error) => {
            assert!(
                matches!(
                    error,
                    GitError::Command { .. }
                        | GitError::Rejected { .. }
                        | GitError::NotARepository(_)
                ),
                "typed error, got {error:?}"
            );
            let _ = error.to_string();
        }
    }
}

#[test]
fn branch_literally_named_head() {
    // `git branch HEAD` is rejected. `refs/heads/HEAD` is still a real branch.
    let repo = Repo::with_commit();
    repo.git(&["update-ref", "refs/heads/HEAD", "HEAD"]);
    repo.git(&["symbolic-ref", "HEAD", "refs/heads/HEAD"]);

    let info = repo.info();
    assert_eq!(info.head, Head::Branch("HEAD".to_owned()));
    let hist = repo
        .open_history(10)
        .expect("history of the branch named HEAD");
    assert_eq!(hist.commits.len(), 1);
    assert_eq!(hist.commits[0].summary, "Awaken");
    assert_eq!(hist.commits[0].short_oid().len(), SHORT_OID_LEN);
}

#[test]
fn shallow_clone_returns_only_the_tip() {
    // `--depth` is ignored for local-path clones; `file://` makes a real shallow clone.
    let repo = Repo::init_named("full");
    repo.commit("first");
    repo.commit("second");
    let shallow = repo.tmp.path().join("shallow");
    let url = format!("file://{}", repo.canonical().display());
    let shallow_arg = shallow.to_str().expect("utf8").to_owned();
    let cwd = repo.tmp.path().to_path_buf();
    repo.git_at(
        &cwd,
        &[
            "-c",
            "protocol.file.allow=always",
            "clone",
            "--depth",
            "1",
            &url,
            &shallow_arg,
        ],
    );
    assert!(
        shallow.join(".git/shallow").is_file(),
        "clone was not shallow"
    );

    let info = inspect(&repo.git, &shallow).expect("inspect shallow");
    let hist = history(&repo.git, &info.root, &info.head, 10).expect("shallow history");
    assert_eq!(hist.commits.len(), 1);
    assert_eq!(hist.commits[0].summary, "second");
    assert!(!hist.truncated);
}

#[test]
fn packed_refs_without_loose_ref_still_opens() {
    let repo = Repo::with_commit();
    let oid = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&["pack-refs", "--all"]);
    let loose = repo.root.join(".git/refs/heads/main");
    if loose.exists() {
        fs::remove_file(&loose).expect("remove loose ref");
    }
    assert!(!loose.exists(), "loose ref must be gone");
    let packed = fs::read_to_string(repo.root.join(".git/packed-refs")).expect("packed-refs");
    assert!(packed.contains(&oid), "{packed}");

    let info = repo.info();
    assert_eq!(info.head, Head::Branch("main".to_owned()));
    let hist = repo.open_history(10).expect("packed-refs history");
    assert_eq!(hist.commits.len(), 1);
    assert_eq!(hist.commits[0].oid, oid);
    assert_eq!(hist.commits[0].summary, "Awaken");
    assert_eq!(hist.commits[0].short_oid(), &oid[..SHORT_OID_LEN]);
}

#[cfg(unix)]
fn assert_directory_name_kept(name: &OsStr) {
    use std::os::unix::ffi::OsStrExt;

    let repo = Repo::init_named(name);
    let canonical = repo.canonical();
    assert!(
        canonical.as_os_str().as_bytes().ends_with(name.as_bytes()),
        "fixture directory does not end with the requested name"
    );
    let info = inspect(&repo.git, &repo.root).unwrap_or_else(|error| {
        panic!("inspect must open this repository and keep its real root, got {error}")
    });
    assert_eq!(
        info.root, canonical,
        "root must be the canonical path, not one with a trailing CR/LF stripped"
    );
    let expected_name = canonical.file_name().expect("file name").to_string_lossy();
    assert_eq!(info.name, expected_name);
    assert_eq!(info.head, Head::Unborn("main".to_owned()));
}

#[cfg(unix)]
#[test]
fn directory_ending_in_newline_keeps_canonical_root() {
    use std::os::unix::ffi::OsStrExt;
    assert_directory_name_kept(OsStr::from_bytes(b"endnl\n"));
}

#[cfg(unix)]
#[test]
fn directory_ending_in_carriage_return_keeps_canonical_root() {
    use std::os::unix::ffi::OsStrExt;
    assert_directory_name_kept(OsStr::from_bytes(b"endcr\r"));
}

#[test]
fn directory_with_quote_and_space_keeps_name_and_root() {
    let repo = Repo::init_named("quote's repo");
    repo.commit("kept");
    let info = repo.info();
    assert_eq!(info.name, "quote's repo");
    assert_eq!(info.root, repo.canonical());
    assert_eq!(info.head, Head::Branch("main".to_owned()));
}

// APFS rejects a directory name that is not valid UTF-8. Linux accepts the byte.
#[cfg(target_os = "linux")]
#[test]
fn non_utf8_directory_name_inspects() {
    use std::os::unix::ffi::OsStrExt;

    let repo = Repo::init_named(OsStr::from_bytes(b"rlyeh_\xff_deep"));
    let info = inspect(&repo.git, &repo.root).expect("non-utf8 directory name");
    assert_eq!(info.root, repo.canonical());
    assert!(!info.name.is_empty());
    assert_eq!(info.head, Head::Unborn("main".to_owned()));
    assert_eq!(
        history(&repo.git, &info.root, &info.head, 5).expect("history"),
        History::default()
    );
}

#[cfg(unix)]
#[test]
fn symlink_loop_is_not_a_directory() {
    use std::os::unix::fs::symlink;

    let git = Git::discover().expect("system git");
    let tmp = TempDir::with_prefix("cthulhu-adv-").expect("temp dir");
    let path = tmp.path().join("loop");
    symlink(&path, &path).expect("self symlink");

    let error = inspect(&git, &path).expect_err("symlink loop");
    assert!(
        matches!(&error, GitError::NotADirectory(got) if got == &path),
        "expected NotADirectory, got {error:?}"
    );
}

/// Restores the mode of `.git` before `TempDir` cleanup. Locals drop in
/// reverse declaration order, so the guard is created after the repo.
#[cfg(unix)]
struct ModeRestore {
    path: PathBuf,
    mode: u32,
}

#[cfg(unix)]
impl ModeRestore {
    fn lock_down(path: &Path) -> Self {
        use std::os::unix::fs::PermissionsExt;

        let mode = fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
        fs::set_permissions(path, fs::Permissions::from_mode(0o000)).expect("chmod 000");
        Self {
            path: path.to_path_buf(),
            mode,
        }
    }
}

#[cfg(unix)]
impl Drop for ModeRestore {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(self.mode));
    }
}

#[cfg(unix)]
#[test]
fn unreadable_git_dir_is_a_typed_error() {
    let repo = Repo::init_named("noperm");
    let _restore = ModeRestore::lock_down(&repo.root.join(".git"));

    let error = inspect(&repo.git, &repo.root).expect_err("unreadable .git");
    assert!(
        matches!(
            error,
            GitError::NotARepository(_)
                | GitError::Command { .. }
                | GitError::Rejected { .. }
                | GitError::Spawn(_)
        ),
        "typed error, got {error:?}"
    );
    let _ = error.to_string();
}

#[test]
fn history_limit_one_on_two_commits_is_truncated() {
    let repo = Repo::init_named("limits");
    repo.commit("first");
    repo.commit("second");

    let hist = repo.open_history(1).expect("history");
    assert_eq!(hist.commits.len(), 1);
    assert!(hist.truncated);
    assert_eq!(hist.commits[0].summary, "second");
}

#[test]
fn history_limit_usize_max_returns_every_commit() {
    // `limit == 0` skips git. A wrapping increment would ask for `--max-count=0`
    // and hide every commit; the commits must still come back.
    let repo = Repo::init_named("huge-limit");
    repo.commit("first");
    repo.commit("second");

    let hist = repo.open_history(usize::MAX).unwrap_or_else(|error| {
        panic!("usize::MAX must not wrap to --max-count=0 or fail git, got {error}")
    });
    assert_eq!(
        hist.commits.len(),
        2,
        "usize::MAX must not be passed to git as 0"
    );
    assert!(!hist.truncated);
    assert_eq!(hist.commits[0].summary, "second");
    assert_eq!(hist.commits[1].summary, "first");
}

#[test]
fn merge_commit_is_listed_once_newest_first() {
    let repo = Repo::init_named("merge");
    repo.write("a.txt", "a\n");
    repo.add("a.txt");
    repo.commit_at("base", 1_000_000_000);
    repo.git(&["switch", "-c", "side"]);
    repo.write("b.txt", "b\n");
    repo.add("b.txt");
    repo.commit_at("side", 1_000_000_100);
    repo.git(&["switch", "main"]);
    repo.write("c.txt", "c\n");
    repo.add("c.txt");
    repo.commit_at("mainline", 1_000_000_200);

    let date = "1000000300 +0000";
    let cwd = repo.root.clone();
    let output = repo
        .command(&cwd)
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .args(["merge", "--no-ff", "side", "-m", "Merge branch side"])
        .output()
        .expect("merge");
    assert!(
        output.status.success(),
        "git merge failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let hist = repo.open_history(10).expect("merge history");
    let summaries: Vec<&str> = hist
        .commits
        .iter()
        .map(|commit| commit.summary.as_str())
        .collect();
    assert_eq!(summaries, ["Merge branch side", "mainline", "side", "base"]);
    assert_eq!(
        summaries
            .iter()
            .filter(|summary| **summary == "Merge branch side")
            .count(),
        1
    );
}

/// `main` has `a.txt=base` and `keep.txt=same`. `other` has `a.txt=other`
/// and the same `keep.txt`. HEAD is back on `main`.
fn repo_with_diverged_a_txt() -> Repo {
    let repo = Repo::init_named("diverge");
    repo.write("a.txt", "base");
    repo.write("keep.txt", "same");
    repo.add("a.txt");
    repo.add("keep.txt");
    repo.commit("base");
    repo.git(&["branch", "other"]);
    repo.git(&["switch", "other"]);
    repo.write("a.txt", "other");
    repo.add("a.txt");
    repo.commit("other tip");
    repo.git(&["switch", "main"]);
    repo
}

/// After `switch_branch`, HEAD is attached to `name`, that row is the only
/// current branch and sorts first, and the latest commit is that branch tip.
fn assert_switched_to(repo: &Repo, name: &str) -> Vec<Branch> {
    let expected_ref = format!("refs/heads/{name}");
    assert_eq!(
        repo.git(&["symbolic-ref", "--quiet", "HEAD"]),
        expected_ref,
        "HEAD must be attached to {name}"
    );

    let info = repo.info();
    assert_eq!(info.head, Head::Branch(name.to_owned()));

    let rows = branches(&repo.git, &repo.root, &info.head)
        .unwrap_or_else(|error| panic!("branches after switching to {name}: {error}"));
    assert!(
        rows.first()
            .is_some_and(|row| row.current && row.name == name),
        "current branch {name} must sort first, got {rows:?}"
    );
    assert_eq!(
        rows.iter().filter(|row| row.current).count(),
        1,
        "one current branch, got {rows:?}"
    );

    let tip = repo.git(&["rev-parse", &expected_ref]);
    let detail = latest_commit(&repo.git, &repo.root, &info.head)
        .unwrap_or_else(|error| panic!("latest commit on {name}: {error}"))
        .unwrap_or_else(|| panic!("latest commit on {name} must be Some"));
    assert_eq!(detail.oid, tip);
    rows
}

/// A refused switch is git's own failure: [`GitError::Command`], not
/// `Rejected` or `NotARepository`. The command line includes `name` and
/// never `--force`. Stderr stays English because the library sets `LC_ALL=C`.
fn assert_command_failure(error: GitError, name: &str) -> String {
    let display = error.to_string();
    match error {
        GitError::Command {
            command, stderr, ..
        } => {
            assert!(
                display.contains(name) || command.contains(name),
                "branch name {name} missing from `{display}` / `{command}`"
            );
            assert!(!command.contains("--force"), "{command}");
            assert!(!display.contains("--force"), "{display}");
            stderr
        }
        GitError::Rejected { .. } => panic!("expected Command, got Rejected: {display}"),
        GitError::NotARepository(path) => {
            panic!(
                "expected Command, got NotARepository({}): {display}",
                path.display()
            )
        }
        other => panic!("expected Command, got {other:?}: {display}"),
    }
}

fn relative_paths(root: &Path) -> Vec<String> {
    let mut paths = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries =
            fs::read_dir(&dir).unwrap_or_else(|error| panic!("read {}: {error}", dir.display()));
        let mut children = Vec::new();
        for entry in entries {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .expect("path under root")
                .to_string_lossy()
                .into_owned();
            if entry.file_type().expect("file type").is_dir() {
                children.push(path);
            }
            paths.push(relative);
        }
        stack.extend(children);
    }
    paths.sort();
    paths
}

fn paths_named<'a>(paths: &'a [String], name: &str) -> Vec<&'a str> {
    paths
        .iter()
        .filter(|path| Path::new(path).file_name().and_then(|item| item.to_str()) == Some(name))
        .map(String::as_str)
        .collect()
}

/// Every `index` and `filename` under the temp dir belongs to the source
/// worktree. The bare clone must not gain either.
fn assert_checkout_files_stay_in_source(paths: &[String], filename: &str) {
    let tracked = paths_named(paths, filename);
    assert!(
        tracked
            .iter()
            .any(|path| Path::new(path) == Path::new("source").join(filename)),
        "committed {filename} missing from the source worktree: {tracked:?}"
    );
    assert!(
        tracked
            .iter()
            .all(|path| Path::new(path).starts_with("source")),
        "{filename} appeared outside the source worktree: {tracked:?}"
    );
    let indexes = paths_named(paths, "index");
    assert!(
        indexes
            .iter()
            .all(|path| Path::new(path).starts_with("source")),
        "index appeared outside the source worktree: {indexes:?}"
    );
}

fn switch_ok(repo: &Repo, name: &str) {
    switch_branch(&repo.git, &repo.root, name)
        .unwrap_or_else(|error| panic!("switch to {name} must succeed, got {error}"));
}

#[test]
fn checkout_nested_branch_attaches_symbolic_head() {
    let repo = Repo::with_commit();
    repo.git(&["branch", "feature/foo"]);
    switch_ok(&repo, "feature/foo");

    let rows = assert_switched_to(&repo, "feature/foo");
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].name, "feature/foo");
    assert!(rows[0].current, "{rows:?}");
    assert_eq!(rows[1].name, "main");
    assert!(!rows[1].current, "{rows:?}");
}

#[test]
fn checkout_refuses_overlapping_dirty_file() {
    let repo = repo_with_diverged_a_txt();
    repo.write("a.txt", "dirty");

    let error = switch_branch(&repo.git, &repo.root, "other").expect_err("overlapping dirty file");
    let stderr = assert_command_failure(error, "other");
    assert!(
        stderr.contains("would be overwritten"),
        "english overwrite refusal, got {stderr}"
    );

    assert_eq!(fs::read(repo.root.join("a.txt")).expect("a.txt"), b"dirty");
    // `Repo::git` trims, which would hide the leading space in porcelain.
    // An empty index diff and a worktree diff keep the edit unstaged.
    assert_eq!(repo.git(&["diff", "--cached", "--", "a.txt"]), "");
    let diff = repo.git(&["diff", "--", "a.txt"]);
    assert!(diff.contains("-base"), "{diff}");
    assert!(diff.contains("+dirty"), "{diff}");
    assert_eq!(
        repo.git(&["symbolic-ref", "--quiet", "HEAD"]),
        "refs/heads/main"
    );
}

#[test]
fn checkout_keeps_uncommitted_edit_when_paths_differ_elsewhere() {
    let repo = repo_with_diverged_a_txt();
    let other_tip = repo.git(&["rev-parse", "refs/heads/other"]);
    repo.write("keep.txt", "edited");

    switch_ok(&repo, "other");
    assert_switched_to(&repo, "other");
    assert_eq!(
        fs::read(repo.root.join("keep.txt")).expect("keep.txt"),
        b"edited"
    );
    assert_eq!(repo.git(&["diff", "--cached", "--", "keep.txt"]), "");
    let diff = repo.git(&["diff", "--", "keep.txt"]);
    assert!(diff.contains("-same"), "{diff}");
    assert!(diff.contains("+edited"), "{diff}");
    assert_eq!(fs::read(repo.root.join("a.txt")).expect("a.txt"), b"other");

    let info = repo.info();
    let detail = latest_commit(&repo.git, &repo.root, &info.head)
        .expect("latest commit")
        .expect("other has a commit");
    assert_eq!(detail.oid, other_tip);
}

#[test]
fn checkout_refuses_branch_checked_out_in_linked_worktree() {
    let repo = Repo::with_commit();
    repo.git(&["branch", "topic"]);
    let linked = repo.tmp.path().join("linked");
    let linked_arg = linked.to_str().expect("utf8").to_owned();
    repo.git(&["worktree", "add", &linked_arg, "topic"]);

    let error = switch_branch(&repo.git, &repo.root, "topic").expect_err("worktree lock");
    let stderr = assert_command_failure(error, "topic");
    assert!(
        stderr.contains("already used by worktree"),
        "english worktree refusal, got {stderr}"
    );
    assert_eq!(
        repo.git(&["symbolic-ref", "--quiet", "HEAD"]),
        "refs/heads/main"
    );
    assert_eq!(
        repo.git_at(&linked, &["symbolic-ref", "--quiet", "HEAD"]),
        "refs/heads/topic"
    );
}

#[test]
fn checkout_of_dash_switch_attaches_head() {
    // `git branch -- -switch` is rejected. `refs/heads/-switch` is still a real branch.
    let repo = Repo::with_commit();
    repo.git(&["update-ref", "refs/heads/-switch", "HEAD"]);
    switch_ok(&repo, "-switch");

    assert_eq!(
        repo.git(&["symbolic-ref", "--quiet", "HEAD"]),
        "refs/heads/-switch"
    );
    assert_eq!(repo.info().head, Head::Branch("-switch".to_owned()));
    assert_switched_to(&repo, "-switch");
}

#[test]
fn checkout_from_orphan_unborn_attaches_main() {
    let repo = Repo::init_named("orphan");
    repo.write("a.txt", "kept");
    repo.add("a.txt");
    repo.commit("initial");
    repo.git(&["checkout", "--orphan", "unborn"]);

    assert_eq!(
        repo.git(&["symbolic-ref", "--quiet", "HEAD"]),
        "refs/heads/unborn"
    );
    assert_eq!(repo.info().head, Head::Unborn("unborn".to_owned()));
    let bytes = fs::read(repo.root.join("a.txt")).expect("a.txt");

    switch_ok(&repo, "main");
    let rows = assert_switched_to(&repo, "main");
    assert!(
        rows.iter().any(|row| row.name == "main" && row.current),
        "{rows:?}"
    );
    assert!(
        rows.iter().all(|row| row.name != "unborn"),
        "synthetic unborn row remained: {rows:?}"
    );
    assert_eq!(fs::read(repo.root.join("a.txt")).expect("a.txt"), bytes);
}

#[test]
fn checkout_from_detached_head_attaches_main() {
    let repo = Repo::with_commit();
    repo.git(&["checkout", "--detach", "HEAD"]);
    let detached = repo.info();
    assert!(
        matches!(detached.head, Head::Detached { .. }),
        "expected a detached HEAD, got {:?}",
        detached.head
    );

    switch_ok(&repo, "main");
    assert_switched_to(&repo, "main");
    assert_eq!(repo.info().head, Head::Branch("main".to_owned()));
    assert!(
        latest_commit(&repo.git, &repo.root, &repo.info().head)
            .expect("latest commit")
            .is_some()
    );
}

#[test]
fn sha256_checkout_attaches_head() {
    let repo = Repo::init_sha256();
    repo.commit("hashed");
    repo.git(&["branch", "other"]);
    switch_ok(&repo, "other");
    assert_switched_to(&repo, "other");

    assert_eq!(repo.git(&["rev-parse", "--show-object-format"]), "sha256");
    let tip = repo.git(&["rev-parse", "refs/heads/other"]);
    assert_eq!(tip.len(), 64, "{tip}");
    assert!(tip.chars().all(|ch| ch.is_ascii_hexdigit()), "{tip}");

    let detail = latest_commit(&repo.git, &repo.root, &repo.info().head)
        .expect("latest commit")
        .expect("sha256 branch has a commit");
    assert_eq!(detail.oid.len(), 64, "{}", detail.oid);
    assert!(
        detail.oid.chars().all(|ch| ch.is_ascii_hexdigit()),
        "{}",
        detail.oid
    );
    assert_eq!(detail.oid, tip);
}

#[test]
fn packed_ref_checkout_attaches_destination() {
    let repo = Repo::with_commit();
    repo.git(&["branch", "topic"]);
    let tip = repo.git(&["rev-parse", "refs/heads/topic"]);
    repo.git(&["pack-refs", "--all"]);
    let loose = repo.root.join(".git/refs/heads/topic");
    if loose.exists() {
        fs::remove_file(&loose).expect("remove loose ref");
    }
    assert!(!loose.exists(), "loose ref must be gone");
    let packed = fs::read_to_string(repo.root.join(".git/packed-refs")).expect("packed-refs");
    assert!(packed.contains(&tip), "{packed}");

    switch_ok(&repo, "topic");
    assert_switched_to(&repo, "topic");
    assert_eq!(
        repo.git(&["symbolic-ref", "--quiet", "HEAD"]),
        "refs/heads/topic"
    );
}

#[test]
fn bare_checkout_fails_cleanly_inside_the_temp_dir() {
    let repo = Repo::init_named("source");
    repo.write("tracked.txt", "tracked");
    repo.add("tracked.txt");
    repo.commit("track a file");
    repo.git(&["branch", "feature"]);

    let bare = repo.tmp.path().join("demo.git");
    let bare_arg = bare.to_str().expect("utf8").to_owned();
    let src = repo.root.to_str().expect("utf8").to_owned();
    let cwd = repo.tmp.path().to_path_buf();
    repo.git_at(&cwd, &["clone", "--bare", &src, &bare_arg]);
    assert_eq!(
        repo.git_at(&bare, &["rev-parse", "--is-bare-repository"]),
        "true"
    );
    repo.git_at(&bare, &["rev-parse", "--verify", "refs/heads/feature"]);

    let before = relative_paths(repo.tmp.path());
    assert_checkout_files_stay_in_source(&before, "tracked.txt");

    let error = switch_branch(&repo.git, &bare, "feature").expect_err("bare switch");
    let stderr = assert_command_failure(error, "feature");
    assert!(
        stderr.contains("work tree")
            || stderr.contains("this operation must be run in a work tree"),
        "english bare-repo refusal, got {stderr}"
    );

    assert_eq!(
        repo.git_at(&bare, &["symbolic-ref", "--quiet", "HEAD"]),
        "refs/heads/main"
    );
    let after = relative_paths(repo.tmp.path());
    assert_eq!(
        after, before,
        "switch must not create files in the temp dir"
    );
    assert_checkout_files_stay_in_source(&after, "tracked.txt");
}

#[test]
fn checkout_does_not_wipe_superproject_or_submodule() {
    let super_repo = Repo::init_named("super");
    super_repo.commit("super initial");

    let origin = super_repo.tmp.path().join("origin");
    fs::create_dir(&origin).expect("origin dir");
    super_repo.run(&origin, &["init", "-b", "main"]);
    fs::write(origin.join("mod.txt"), "from-module\n").expect("mod.txt");
    super_repo.git_at(&origin, &["add", "--", "mod.txt"]);
    super_repo.commit_in(&origin, "module initial");

    let url = format!(
        "file://{}",
        dunce::canonicalize(&origin)
            .expect("canonical origin")
            .display()
    );
    super_repo.git_at(
        &super_repo.root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            &url,
            "module",
        ],
    );
    super_repo.commit("add module");

    super_repo.write("README", "main-readme");
    super_repo.add("README");
    super_repo.commit("readme on main");
    let gitlink = super_repo.git(&["rev-parse", "HEAD:module"]);

    super_repo.git(&["switch", "-c", "other"]);
    let other_readme = "other-readme";
    super_repo.write("README", other_readme);
    super_repo.add("README");
    super_repo.commit("readme on other");
    assert_eq!(super_repo.git(&["rev-parse", "HEAD:module"]), gitlink);
    super_repo.git(&["switch", "main"]);
    assert_eq!(
        fs::read_to_string(super_repo.root.join("README")).expect("README"),
        "main-readme"
    );
    assert_eq!(super_repo.git(&["rev-parse", "HEAD:module"]), gitlink);

    let module = super_repo.root.join("module");
    let module_head = super_repo.git_at(&module, &["rev-parse", "HEAD"]);
    let mod_txt = fs::read(module.join("mod.txt")).expect("mod.txt");

    switch_branch(&super_repo.git, &super_repo.root, "other").unwrap_or_else(|error| {
        panic!("switch other must keep the superproject and submodule, got {error}")
    });
    assert_switched_to(&super_repo, "other");
    assert_eq!(
        fs::read_to_string(super_repo.root.join("README")).expect("README"),
        other_readme
    );
    assert_eq!(super_repo.git(&["rev-parse", "HEAD:module"]), gitlink);
    assert!(
        super_repo.root.join(".git").is_dir(),
        "superproject .git must stay a directory"
    );
    assert!(module.join(".git").exists(), "submodule .git disappeared");
    assert_eq!(
        super_repo.git_at(&module, &["rev-parse", "HEAD"]),
        module_head
    );
    assert_eq!(fs::read(module.join("mod.txt")).expect("mod.txt"), mod_txt);
}
