//! `inspect` against real temporary repositories and the system git.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cthulhu_git::git::{
    CommitFile, CommitStats, DiffKind, Git, GitError, GraphCandidate, GraphTip, Head, History,
    Person, Remote, RepoInfo, SHORT_OID_LEN, Upstream, branch_graph, branches, commit_hover,
    file_diff, graph_tips, head_commit_oid, history, inspect, latest_commit, pull_ff_only, remotes,
};
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
        let mut command = Command::new(&self.git.path);
        command
            .current_dir(&self.root)
            .env("GIT_AUTHOR_NAME", "cthulhu")
            .env("GIT_AUTHOR_EMAIL", "cthulhu@example.invalid")
            .env("GIT_COMMITTER_NAME", "cthulhu")
            .env("GIT_COMMITTER_EMAIL", "cthulhu@example.invalid")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .args(["-c", "commit.gpgSign=false"]);
        without_user_git_config(&mut command, self._tmp.path());
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

    fn canonical_root(&self) -> PathBuf {
        dunce::canonicalize(&self.root).expect("canonical root")
    }

    fn inspect_at(&self, path: &Path) -> Result<RepoInfo, GitError> {
        inspect(&self.git, path)
    }

    fn inspect(&self) -> RepoInfo {
        self.inspect_at(&self.root).expect("inspect")
    }

    fn commit(&self, message: &str) {
        self.git(&["commit", "--allow-empty", "-m", message]);
    }

    fn commit_at(&self, message: &str, date: &str) {
        let mut command = Command::new(&self.git.path);
        command
            .current_dir(&self.root)
            .env("GIT_AUTHOR_NAME", "cthulhu")
            .env("GIT_AUTHOR_EMAIL", "cthulhu@example.invalid")
            .env("GIT_COMMITTER_NAME", "cthulhu")
            .env("GIT_COMMITTER_EMAIL", "cthulhu@example.invalid")
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .args(["-c", "commit.gpgSign=false"]);
        without_user_git_config(&mut command, self._tmp.path());
        command.args(["commit", "--allow-empty", "-m", message]);
        let output = command.output().expect("run git");
        assert!(
            output.status.success(),
            "commit {message:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn history(&self, limit: usize) -> History {
        let info = self.inspect();
        history(&self.git, &info.root, &info.head, limit).expect("history")
    }
}

fn without_user_git_config(command: &mut Command, scratch: &Path) {
    let global = scratch.join("empty.gitconfig");
    let system = scratch.join("empty.system.gitconfig");
    let hooks = scratch.join("empty-hooks");
    let _ = fs::create_dir_all(&hooks);
    let _ = fs::write(&global, "");
    let _ = fs::write(&system, "");
    command
        .env("GIT_CONFIG_GLOBAL", &global)
        .env("GIT_CONFIG_SYSTEM", &system)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("-c")
        .arg(format!("core.hooksPath={}", hooks.display()));
}

#[test]
fn divergent_pull_with_rebase_config_does_not_rewrite_head() {
    let origin = Fixture::init();
    origin.write("README.md", "base\n");
    origin.git(&["add", "README.md"]);
    origin.git(&["commit", "-m", "base"]);

    let local_tmp = TempDir::with_prefix("cthulhu-pull-").expect("temp dir");
    let local_root = local_tmp.path().join("local");
    let mut clone = Command::new(&origin.git.path);
    clone
        .env("GIT_AUTHOR_NAME", "cthulhu")
        .env("GIT_AUTHOR_EMAIL", "cthulhu@example.invalid")
        .env("GIT_COMMITTER_NAME", "cthulhu")
        .env("GIT_COMMITTER_EMAIL", "cthulhu@example.invalid")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE");
    without_user_git_config(&mut clone, origin._tmp.path());
    clone.args([
        "clone",
        "--",
        origin.root.to_str().expect("utf8 origin"),
        local_root.to_str().expect("utf8 local"),
    ]);
    let cloned = clone.output().expect("clone");
    assert!(
        cloned.status.success(),
        "clone failed: {}",
        String::from_utf8_lossy(&cloned.stderr)
    );

    let local = Fixture {
        _tmp: local_tmp,
        root: local_root,
        git: origin.git.clone(),
    };
    origin.write("README.md", "origin moved\n");
    origin.git(&["add", "README.md"]);
    origin.git(&["commit", "-m", "origin"]);
    local.write("README.md", "local moved\n");
    local.git(&["add", "README.md"]);
    local.git(&["commit", "-m", "local"]);
    local.git(&["config", "pull.rebase", "true"]);
    local.git(&["config", "branch.main.rebase", "true"]);

    let head = local.git(&["rev-parse", "HEAD"]);
    let error = pull_ff_only(&local.git, &local.root).expect_err("divergent pull");
    assert!(
        matches!(error, GitError::Command { .. }),
        "pull must fail without rewriting, got {error:?}"
    );
    assert_eq!(local.git(&["rev-parse", "HEAD"]), head);
    let git_dir = local.root.join(".git");
    assert!(!git_dir.join("rebase-merge").exists());
    assert!(!git_dir.join("rebase-apply").exists());
    assert!(!git_dir.join("MERGE_HEAD").exists());
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
            short_oid: oid[..SHORT_OID_LEN].to_owned()
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

#[test]
fn history_lists_newest_first_with_full_oid() {
    let fixture = Fixture::with_commit();
    fixture.commit("Dream in R'lyeh");
    fixture.commit("Rise from the sea\n\nThe body is not part of the summary.");
    let head = fixture.git(&["rev-parse", "HEAD"]);

    let history = fixture.history(100);
    let summaries: Vec<_> = history
        .commits
        .iter()
        .map(|commit| commit.summary.as_str())
        .collect();
    assert_eq!(
        summaries,
        ["Rise from the sea", "Dream in R'lyeh", "Awaken"]
    );
    assert!(!history.truncated);

    let latest = history.latest().expect("latest commit");
    assert_eq!(latest.oid, head);
    assert_eq!(latest.short_oid(), &head[..SHORT_OID_LEN]);
}

#[test]
fn history_is_truncated_at_the_limit() {
    let fixture = Fixture::with_commit();
    fixture.commit("Second");
    fixture.commit("Third");

    let history = fixture.history(2);
    assert_eq!(history.commits.len(), 2);
    assert!(history.truncated);
    assert_eq!(history.commits[0].summary, "Third");

    assert!(!fixture.history(3).truncated);
}

#[test]
fn history_follows_a_detached_head() {
    let fixture = Fixture::with_commit();
    fixture.commit("Second");
    fixture.git(&["checkout", "--detach", "HEAD~1"]);
    let summaries: Vec<_> = fixture
        .history(10)
        .commits
        .into_iter()
        .map(|commit| commit.summary)
        .collect();
    assert_eq!(summaries, ["Awaken"]);
}

#[test]
fn empty_repository_has_empty_history() {
    let fixture = Fixture::init();
    assert_eq!(fixture.history(10), History::default());
}

#[test]
fn unborn_repository_lists_its_branch_and_has_no_commit_detail() {
    let fixture = Fixture::init();
    let info = fixture.inspect();
    let listed = branches(&fixture.git, &info.root, &info.head).expect("branches");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "main");
    assert!(listed[0].current);
    assert_eq!(listed[0].commit_count, 0);
    assert_eq!(listed[0].oid, None);
    assert_eq!(listed[0].committer_time, None);
    assert_eq!(listed[0].upstream, Upstream::None);
    assert_eq!(
        latest_commit(&fixture.git, &info.root, &info.head).expect("detail"),
        None
    );
}

#[test]
fn branches_report_commit_counts_and_ahead_behind() {
    let fixture = Fixture::with_commit();
    fixture.git(&["branch", "feature"]);
    fixture.git(&["switch", "feature"]);
    fixture.commit("Second\n\nThe body is not part of the summary.");
    fixture.git(&["branch", "--set-upstream-to=main"]);

    let info = fixture.inspect();
    let listed = branches(&fixture.git, &info.root, &info.head).expect("branches");
    assert_eq!(listed.len(), 2);

    let feature = &listed[0];
    assert_eq!(feature.name, "feature");
    assert!(feature.current);
    assert_eq!(feature.commit_count, 2);
    assert_eq!(
        feature.upstream,
        Upstream::Tracking {
            name: "main".to_owned(),
            ahead: 1,
            behind: 0,
        }
    );

    let main = &listed[1];
    assert_eq!(main.name, "main");
    assert!(!main.current);
    assert_eq!(main.commit_count, 1);
    assert_eq!(main.upstream, Upstream::None);
    assert!(feature.oid.is_some());
    assert_ne!(feature.oid, main.oid);
    assert!(feature.committer_time.is_some());
    assert!(main.committer_time.is_some());

    let detail = latest_commit(&fixture.git, &info.root, &info.head)
        .expect("detail")
        .expect("commit");
    assert_eq!(detail.oid, fixture.git(&["rev-parse", "HEAD"]));
    assert_eq!(detail.author.name, "cthulhu");
    assert_eq!(detail.author.email, "cthulhu@example.invalid");
    assert!(detail.co_authors.is_empty());
    assert!(
        detail
            .message
            .starts_with("Second\n\nThe body is not part of the summary."),
        "{:?}",
        detail.message
    );
    let expected_date = fixture.git(&[
        "log",
        "-1",
        "--date=format-local:%Y-%m-%d %H:%M",
        "--format=%ad",
    ]);
    assert_eq!(detail.authored_at, expected_date);
}

#[test]
fn graph_and_latest_commit_read_co_author_trailers() {
    let fixture = Fixture::with_commit();
    fixture.commit(
        "Seal the vault\n\nThe body mentions Co-authored-by: inline, which is not a trailer.\n\n\
         Co-authored-by: Ada Voss <ada@miskatonic.test>\n\
         Co-authored-by: cthulhu <old-address@example.invalid>\n\
         Co-authored-by: Ada Voss <ADA@miskatonic.test>",
    );

    let info = fixture.inspect();
    let listed = branches(&fixture.git, &info.root, &info.head).expect("branches");
    let graph = branch_graph(&fixture.git, &info.root, &tips_of(&listed), 10).expect("graph");
    let sealed = graph
        .rows
        .iter()
        .find(|row| row.summary == "Seal the vault")
        .expect("sealed row");
    assert_eq!(sealed.author.name, "cthulhu");
    assert_eq!(sealed.author.email, "cthulhu@example.invalid");
    assert_eq!(sealed.author.aliases, ["old-address@example.invalid"]);
    assert_eq!(
        sealed.co_authors,
        [Person::new("Ada Voss", "ada@miskatonic.test")]
    );
    let awaken = graph
        .rows
        .iter()
        .find(|row| row.summary == "Awaken")
        .expect("awaken row");
    assert!(awaken.co_authors.is_empty());
    assert!(awaken.author.aliases.is_empty());

    let detail = latest_commit(&fixture.git, &info.root, &info.head)
        .expect("detail")
        .expect("commit");
    assert_eq!(detail.co_authors, sealed.co_authors);
    assert_eq!(detail.author, sealed.author);
}

#[test]
fn commit_hover_reads_the_message_dates_and_short_stat() {
    let fixture = Fixture::with_commit();
    fixture.write(
        "README.md",
        "Ph'nglui mglw'nafh\nCthulhu R'lyeh\nwgah'nagl fhtagn\n",
    );
    fixture.write("notes/one.md", "one\n");
    fixture.git(&["add", "-A"]);
    fixture.git(&[
        "commit",
        "-m",
        "Chant\n\nLonger body.\n\nCo-authored-by: Ada Voss <ada@miskatonic.test>",
    ]);
    let info = fixture.inspect();
    let oid = fixture.git(&["rev-parse", "HEAD"]);
    let hover = commit_hover(&fixture.git, &info.root, &oid).expect("hover");
    assert_eq!(hover.oid, oid);
    assert_eq!(hover.author.name, "cthulhu");
    assert_eq!(
        hover.co_authors,
        [Person::new("Ada Voss", "ada@miskatonic.test")]
    );
    assert!(hover.message.starts_with("Chant\n\nLonger body."));
    assert!(!hover.message.ends_with('\n'));
    assert_eq!(
        hover.stats,
        Some(CommitStats {
            files: 2,
            insertions: 3,
            deletions: 0,
        })
    );
    let unix: i64 = fixture
        .git(&["log", "-1", "--format=%at"])
        .parse()
        .expect("unix");
    assert_eq!(hover.authored_unix, unix);

    let empty_oid = {
        fixture.commit("Nothing changes");
        fixture.git(&["rev-parse", "HEAD"])
    };
    let empty = commit_hover(&fixture.git, &info.root, &empty_oid).expect("empty commit");
    assert_eq!(empty.stats, None);

    assert!(commit_hover(&fixture.git, &info.root, "--output=/tmp/x").is_err());
}

#[test]
fn commit_hover_reads_a_root_commit_a_merge_and_an_annotated_tag() {
    let fixture = Fixture::with_commit();
    let info = fixture.inspect();
    let root = fixture.git(&["rev-parse", "HEAD"]);
    fixture.git(&["tag", "-a", "v1,0", "-m", "annotated", &root]);

    let hover = commit_hover(&fixture.git, &info.root, &root).expect("root");
    assert_eq!(hover.tags, ["v1,0".to_owned()]);
    assert_eq!(
        hover.files,
        [CommitFile {
            path: "README.md".to_owned(),
            previous: None,
            insertions: Some(1),
            deletions: Some(0),
        }]
    );
    let diff = file_diff(&fixture.git, &info.root, &root, "README.md").expect("patch");
    assert!(
        diff.lines
            .iter()
            .any(|line| line.kind == DiffKind::Added && line.text.contains("Ph'nglui")),
        "{diff:?}"
    );

    fixture.git(&["checkout", "-b", "topic"]);
    fixture.write("topic.txt", "from the topic\n");
    fixture.git(&["add", "topic.txt"]);
    fixture.git(&["commit", "-m", "topic work"]);
    fixture.git(&["checkout", "main"]);
    fixture.git(&["merge", "--no-ff", "-m", "Merge topic", "topic"]);
    let merge = fixture.git(&["rev-parse", "HEAD"]);
    let merged = commit_hover(&fixture.git, &info.root, &merge).expect("merge");
    assert!(
        merged.files.iter().any(|file| file.path == "topic.txt"),
        "{:?}",
        merged.files
    );
    assert!(merged.stats.is_some_and(|stats| stats.files >= 1));
    assert!(merged.tags.is_empty());
}

#[test]
fn remotes_are_listed_in_config_order() {
    let fixture = Fixture::with_commit();
    let info = fixture.inspect();
    assert!(remotes(&fixture.git, &info.root).expect("none").is_empty());

    fixture.git(&[
        "remote",
        "add",
        "origin",
        "git@github.com:don-linux/Cthulhu-Git.git",
    ]);
    fixture.git(&[
        "remote",
        "add",
        "my.fork",
        "https://github.com/someone/fork.git",
    ]);
    let listed = remotes(&fixture.git, &info.root).expect("remotes");
    assert_eq!(
        listed,
        [
            Remote {
                name: "origin".to_owned(),
                url: "git@github.com:don-linux/Cthulhu-Git.git".to_owned(),
            },
            Remote {
                name: "my.fork".to_owned(),
                url: "https://github.com/someone/fork.git".to_owned(),
            },
        ]
    );
}

#[test]
fn graph_authors_follow_the_mailmap() {
    let fixture = Fixture::with_commit();
    fixture.write(
        ".mailmap",
        "Great Old One <great@rlyeh.test> cthulhu <cthulhu@example.invalid>\n",
    );
    let info = fixture.inspect();
    let listed = branches(&fixture.git, &info.root, &info.head).expect("branches");
    let graph = branch_graph(&fixture.git, &info.root, &tips_of(&listed), 10).expect("graph");
    assert_eq!(graph.rows[0].author.name, "Great Old One");
    assert_eq!(graph.rows[0].author.email, "great@rlyeh.test");
    // The recorded address stays as an alias, for avatars and for folding.
    assert_eq!(graph.rows[0].author.aliases, ["cthulhu@example.invalid"]);

    fixture.commit("Rise\n\nCo-authored-by: cthulhu <cthulhu@example.invalid>");
    let listed = branches(&fixture.git, &info.root, &info.head).expect("branches");
    let graph = branch_graph(&fixture.git, &info.root, &tips_of(&listed), 10).expect("graph");
    let risen = graph
        .rows
        .iter()
        .find(|row| row.summary == "Rise")
        .expect("rise row");
    assert!(
        risen.co_authors.is_empty(),
        "the mailmapped author is not their own co-author: {:?}",
        risen.co_authors
    );
}

fn tips_of(listed: &[cthulhu_git::git::Branch]) -> Vec<GraphTip> {
    let candidates: Vec<GraphCandidate<'_>> = listed
        .iter()
        .map(|branch| GraphCandidate {
            name: branch.name.as_str(),
            oid: branch.oid.as_deref(),
            current: branch.current,
            committer_time: branch.committer_time,
            checked: true,
        })
        .collect();
    graph_tips(&candidates, None)
}

#[test]
fn graph_shows_where_a_branch_leaves_main() {
    let fixture = Fixture::init();
    fixture.commit_at("Awaken", "2020-01-01T00:00:00");
    fixture.git(&["switch", "-c", "feature"]);
    fixture.commit_at("Feature", "2020-01-02T00:00:00");
    fixture.git(&["switch", "main"]);
    fixture.commit_at("On main", "2020-01-03T00:00:00");

    let info = fixture.inspect();
    let listed = branches(&fixture.git, &info.root, &info.head).expect("branches");
    let tips = tips_of(&listed);
    let graph = branch_graph(&fixture.git, &info.root, &tips, 100).expect("graph");

    let main = graph
        .rows
        .iter()
        .find(|row| row.branches.iter().any(|branch| branch.name == "main"))
        .expect("main tip");
    let feature = graph
        .rows
        .iter()
        .find(|row| row.branches.iter().any(|branch| branch.name == "feature"))
        .expect("feature tip");
    assert_eq!(main.oid, fixture.git(&["rev-parse", "main"]));
    assert_eq!(main.commit_lane, 0);
    assert_eq!(main.commit_color, 0);
    assert_eq!(feature.oid, fixture.git(&["rev-parse", "feature"]));
    assert_eq!(feature.commit_lane, 1);
    assert_eq!(feature.commit_color, 2);
    assert!(
        feature
            .traces
            .iter()
            .any(|trace| trace.from_lane.is_none() && trace.to_lane == Some(0))
    );

    let feature_at = graph
        .rows
        .iter()
        .position(|row| row.summary == "Feature")
        .expect("feature row");
    assert_eq!(graph.rows[feature_at + 1].summary, "Awaken");
    assert_eq!(graph.rows[feature_at + 1].commit_lane, 0);
    assert!(graph.rows[feature_at + 1].branches.is_empty());

    let limited = branch_graph(&fixture.git, &info.root, &tips, 1).expect("limited graph");
    assert!(limited.truncated);
    assert_eq!(limited.rows.len(), 1);
    assert_eq!(limited.rows[0].summary, "On main");

    let empty = branch_graph(&fixture.git, &info.root, &[], 10).expect("no tips");
    assert!(empty.rows.is_empty());
}

#[test]
fn a_newer_feature_joins_main_instead_of_taking_its_lane() {
    let fixture = Fixture::init();
    fixture.commit_at("Base", "2020-01-01T00:00:00");
    fixture.git(&["branch", "feature"]);
    fixture.commit_at("On main", "2020-01-02T00:00:00");
    fixture.git(&["switch", "feature"]);
    fixture.commit_at("On feature", "2020-01-03T00:00:00");
    fixture.git(&["switch", "main"]);

    let info = fixture.inspect();
    let listed = branches(&fixture.git, &info.root, &info.head).expect("branches");
    let feature_time = listed
        .iter()
        .find(|branch| branch.name == "feature")
        .and_then(|branch| branch.committer_time);
    let main_time = listed
        .iter()
        .find(|branch| branch.name == "main")
        .and_then(|branch| branch.committer_time);
    assert!(feature_time > main_time);

    let tips = tips_of(&listed);
    assert!(
        tips.iter()
            .any(|tip| tip.name == "main" && tip.principal && tip.color == 0)
    );
    let graph = branch_graph(&fixture.git, &info.root, &tips, 20).expect("graph");
    let main_row = graph
        .rows
        .iter()
        .find(|row| row.branches.iter().any(|branch| branch.name == "main"))
        .expect("main");
    let feature_row = graph
        .rows
        .iter()
        .find(|row| row.branches.iter().any(|branch| branch.name == "feature"))
        .expect("feature");
    assert_eq!(main_row.commit_lane, 0);
    assert_eq!(main_row.commit_color, 0);
    assert_eq!(feature_row.commit_lane, 1);
    assert_ne!(feature_row.commit_color, 0);

    let base = graph
        .rows
        .iter()
        .find(|row| row.summary == "Base")
        .expect("base");
    assert_eq!(base.commit_lane, 0);
    assert_eq!(base.commit_color, 0);
    assert!(base.traces.iter().any(|trace| {
        trace.from_lane == Some(feature_row.commit_lane) && trace.to_lane.is_none()
    }));
}

#[test]
fn detached_head_stays_on_the_graph() {
    let fixture = Fixture::with_commit();
    fixture.commit("Second");
    fixture.git(&["checkout", "--detach", "HEAD~1"]);

    let info = fixture.inspect();
    let listed = branches(&fixture.git, &info.root, &info.head).expect("branches");
    assert!(listed.iter().all(|branch| !branch.current));
    let head = head_commit_oid(&fixture.git, &info.root).expect("head oid");
    assert_eq!(head, fixture.git(&["rev-parse", "HEAD"]));
    let candidates: Vec<GraphCandidate<'_>> = listed
        .iter()
        .map(|branch| GraphCandidate {
            name: branch.name.as_str(),
            oid: branch.oid.as_deref(),
            current: branch.current,
            committer_time: branch.committer_time,
            checked: true,
        })
        .collect();
    let tips = graph_tips(&candidates, Some(head.as_str()));
    // main is still checked, so it keeps the blue lane. Detached HEAD stays
    // on the graph beside it.
    assert!(
        tips.iter()
            .any(|tip| tip.name == "main" && tip.principal && tip.color == 0)
    );
    assert!(
        tips.iter()
            .any(|tip| tip.name == "HEAD" && !tip.principal && tip.current)
    );
    let graph = branch_graph(&fixture.git, &info.root, &tips, 10).expect("graph");
    assert!(
        graph
            .rows
            .iter()
            .any(|row| row.branches.iter().any(|branch| branch.name == "main"))
    );
    assert!(
        graph
            .rows
            .iter()
            .any(|row| row.branches.iter().any(|branch| branch.name == "HEAD"))
    );
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
