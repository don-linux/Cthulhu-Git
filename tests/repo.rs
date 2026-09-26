//! `inspect` against real temporary repositories and the system git.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cthulhu_git::git::{Git, GitError, Head, RepoInfo, inspect};
use tempfile::TempDir;

struct Fixture {
    _tmp: TempDir,
    root: PathBuf,
    git: Git,
}

impl Fixture {
    fn new() -> Self {
        let git = Git::discover().expect("system git");
        let tmp = TempDir::with_prefix("cthulhu-repo-").expect("temp dir");
        let root = tmp.path().join("cthulhu-demo");
        fs::create_dir(&root).expect("repo dir");
        Self {
            _tmp: tmp,
            root,
            git,
        }
    }

    fn init() -> Self {
        let fixture = Self::new();
        fixture.git(&["init", "-b", "main"]);
        fixture
    }

    fn with_commit() -> Self {
        let fixture = Self::init();
        fixture.write("README.md", "Ph'nglui mglw'nafh\n");
        fixture.git(&["add", "README.md"]);
        fixture.git(&["commit", "-m", "Awaken"]);
        fixture
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().expect("parent")).expect("parent dir");
        fs::write(path, contents).expect("write");
    }

    fn git(&self, args: &[&str]) -> String {
        let output = Command::new(&self.git.path)
            .current_dir(&self.root)
            .env("GIT_AUTHOR_NAME", "cthulhu")
            .env("GIT_AUTHOR_EMAIL", "cthulhu@example.invalid")
            .env("GIT_COMMITTER_NAME", "cthulhu")
            .env("GIT_COMMITTER_EMAIL", "cthulhu@example.invalid")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .args(["-c", "commit.gpgSign=false"])
            .args(args)
            .output()
            .expect("run git");
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

    fn canonical_root(&self) -> PathBuf {
        dunce::canonicalize(&self.root).expect("canonical root")
    }

    fn inspect_at(&self, path: &Path) -> Result<RepoInfo, GitError> {
        inspect(&self.git, path)
    }

    fn inspect(&self) -> RepoInfo {
        self.inspect_at(&self.root).expect("inspect")
    }
}

#[test]
fn discovers_system_git() {
    let git = Git::discover().expect("system git");
    assert!(git.path.is_file());
    assert!(git.version.is_supported(), "{}", git.version);
}

#[test]
fn branch_repository_reports_name_root_and_branch() {
    let fixture = Fixture::with_commit();
    let info = fixture.inspect();
    assert_eq!(info.name, "cthulhu-demo");
    assert_eq!(info.root, fixture.canonical_root());
    assert_eq!(info.head, Head::Branch("main".to_owned()));
}

#[test]
fn branch_names_with_slashes_are_kept() {
    let fixture = Fixture::with_commit();
    fixture.git(&["switch", "-c", "feature/necronomicon"]);
    assert_eq!(
        fixture.inspect().head,
        Head::Branch("feature/necronomicon".to_owned())
    );
}

#[test]
fn detached_head_reports_short_oid() {
    let fixture = Fixture::with_commit();
    let oid = fixture.git(&["rev-parse", "HEAD"]);
    fixture.git(&["checkout", "--detach", "HEAD"]);
    assert_eq!(
        fixture.inspect().head,
        Head::Detached {
            short_oid: oid[..7].to_owned()
        }
    );
}

#[test]
fn empty_repository_is_unborn() {
    let fixture = Fixture::init();
    assert_eq!(fixture.inspect().head, Head::Unborn("main".to_owned()));
}

#[test]
fn subdirectory_resolves_the_parent_root() {
    let fixture = Fixture::with_commit();
    fixture.write("docs/rlyeh/guide.md", "# Guide\n");
    let info = fixture
        .inspect_at(&fixture.root.join("docs/rlyeh"))
        .expect("inspect subdirectory");
    assert_eq!(info.root, fixture.canonical_root());
    assert_eq!(info.name, "cthulhu-demo");
}

#[test]
fn plain_directory_is_not_a_repository() {
    let fixture = Fixture::new();
    let error = fixture.inspect_at(&fixture.root).expect_err("not a repo");
    assert!(matches!(error, GitError::NotARepository(_)), "{error:?}");
    assert!(error.to_string().contains("is not inside a Git repository"));
}

#[test]
fn file_path_is_not_a_directory() {
    let fixture = Fixture::new();
    fixture.write("notes.txt", "The stars are right\n");
    let file = fixture.root.join("notes.txt");
    let error = fixture.inspect_at(&file).expect_err("file");
    assert!(
        matches!(&error, GitError::NotADirectory(path) if *path == file),
        "{error:?}"
    );
}

#[test]
fn missing_path_is_not_a_directory() {
    let fixture = Fixture::new();
    let missing = fixture.root.join("does-not-exist");
    let error = fixture.inspect_at(&missing).expect_err("missing");
    assert!(matches!(error, GitError::NotADirectory(_)), "{error:?}");
}

const CHILD_TARGET_VAR: &str = "CTHULHU_TEST_INSPECT_TARGET";

/// Re-runs this test binary with `GIT_DIR` set in the child only, so the
/// test process environment is never mutated.
#[test]
fn inherited_git_dir_does_not_redirect_inspection() {
    let target = Fixture::with_commit();
    target.git(&["switch", "-c", "feature/necronomicon"]);
    let decoy = Fixture::with_commit();
    decoy.git(&["switch", "-c", "decoy"]);

    let output = Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "child_inspects_target_from_env",
            "--include-ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_TARGET_VAR, &target.root)
        .env("GIT_DIR", decoy.root.join(".git"))
        .env("GIT_WORK_TREE", &decoy.root)
        .output()
        .expect("spawn child test");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "child failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("1 passed"), "child did not run:\n{stdout}");
}

#[test]
#[ignore = "helper run by inherited_git_dir_does_not_redirect_inspection"]
fn child_inspects_target_from_env() {
    let Some(target) = std::env::var_os(CHILD_TARGET_VAR) else {
        return;
    };
    assert!(
        std::env::var_os("GIT_DIR").is_some(),
        "parent must set GIT_DIR"
    );

    let git = Git::discover().expect("system git");
    let info = inspect(&git, Path::new(&target)).expect("inspect target");
    assert_eq!(info.root, dunce::canonicalize(&target).expect("canonical"));
    assert_eq!(info.head, Head::Branch("feature/necronomicon".to_owned()));
}
