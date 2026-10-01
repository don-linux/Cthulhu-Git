//! Adversarial checks for the read-only git boundary.
//!
//! Opening a repository must not execute commands planted in that repository
//! or in the inherited environment, must not hide the real history, and must
//! reject unbounded or ambiguous git output. Tests do not change this process's
//! environment or working directory. Inherited variables are applied only to a
//! child re-exec of this binary.

#![cfg(unix)]

use std::env;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{OnceLock, mpsc};
use std::thread;
use std::time::{Duration, SystemTime};

use cthulhu_git::git::{
    DiscoverInputs, Git, GitError, Head, OVERRIDE_VAR, discover_with, history, inspect,
};
use tempfile::TempDir;

const CHILD_TEST: &str = "child_opens_repo_despite_hostile_environment";
const CHILD_TARGET: &str = "CTHULHU_ADV_A_TARGET";
const CHILD_ROOT: &str = "CTHULHU_ADV_A_ROOT";
const CHILD_BRANCH: &str = "CTHULHU_ADV_A_BRANCH";
const CHILD_ACTION: &str = "CTHULHU_ADV_A_ACTION";
const CHILD_HISTORY: &str = "CTHULHU_ADV_A_HISTORY";
const CHILD_MARKER: &str = "CTHULHU_ADV_A_MARKER";
const CHILD_REQUIRED: &str = "CTHULHU_ADV_A_REQUIRED";
const CHILD_SENTINEL: &str = "CTHULHU_ADV_A_SENTINEL";
const CHILD_CONFIG_KEY: &str = "CTHULHU_ADV_A_CONFIG_KEY";
const CHILD_CONFIG_VALUE: &str = "CTHULHU_ADV_A_CONFIG_VALUE";
const SUGGESTION_MARK: &str = "If you trust it, run: ";

/// Cleared on every setup `git` and on the child before the hostile variables
/// are applied, so one test cannot leak into another through the inherited
/// environment of a helper process.
const CLEARED_ENV: &[&str] = &[
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_KEY_0",
    "GIT_CONFIG_VALUE_0",
    "GIT_CONFIG_KEY_1",
    "GIT_CONFIG_VALUE_1",
    "GIT_CONFIG_KEY_2",
    "GIT_CONFIG_VALUE_2",
    "GIT_CONFIG_KEY_3",
    "GIT_CONFIG_VALUE_3",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_REPLACE_REF_BASE",
    "GIT_SHALLOW_FILE",
    "GIT_GRAFT_FILE",
    "GIT_EXEC_PATH",
    "GIT_TRACE",
    "GIT_TRACE2",
    "GIT_TRACE2_EVENT",
    "GIT_EXTERNAL_DIFF",
    "GIT_DIR",
    "GIT_WORK_TREE",
    OVERRIDE_VAR,
];

const FAKE_GIT_SCRIPT: &str = r#"#!/bin/sh
# One shared fake git. Sidecar files next to the invoked symlink select behavior.
# The quoting test sets this and a PATH that hides the real git; printf is a builtin.
if [ -n "${CTHULHU_ADV_A_ARGV_LOG-}" ]; then
  printf '%s\0' "$@" > "$CTHULHU_ADV_A_ARGV_LOG"
  exit 0
fi
mode=$(cat "$0.mode" 2>/dev/null || echo version)
case "$mode" in
  version)
    echo "git version 2.43.0"
    exit 0
    ;;
  slow-version)
    if [ "$1" = "--version" ]; then
      sleep 3
    fi
    echo "git version 2.43.0"
    exit 0
    ;;
  bomb-rev-parse)
    if [ "$1" = "--version" ]; then
      echo "git version 2.43.0"
      exit 0
    fi
    for arg in "$@"; do
      if [ "$arg" = "--show-toplevel" ]; then
        # 32 MiB of ASCII. Do not raise this; the bound under test is ~32 MiB.
        dd if=/dev/zero bs=1048576 count=32 2>/dev/null | tr '\0' 'A'
        echo
        exit 0
      fi
    done
    echo "unexpected fake git arguments: $*" >&2
    exit 1
    ;;
  fail-stderr)
    if [ "$1" = "--version" ]; then
      echo "git version 2.43.0"
      exit 0
    fi
    if [ -f "$0.stderr" ]; then
      cat "$0.stderr" >&2
    fi
    if [ -f "$0.exit" ]; then
      exit "$(cat "$0.exit")"
    fi
    exit 128
    ;;
  record-argv)
    if [ ! -f "$0.log" ]; then
      echo "missing argv log sidecar" >&2
      exit 1
    fi
    log=$(cat "$0.log")
    printf '%s\0' "$@" > "$log"
    exit 0
    ;;
  *)
    echo "unknown fake git mode: $mode" >&2
    exit 1
    ;;
esac
"#;

fn shared_script() -> &'static Path {
    static SCRIPT_PATH: OnceLock<PathBuf> = OnceLock::new();
    SCRIPT_PATH.get_or_init(|| {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cthulhu-adv-a-fake-git.sh");
        fs::write(&path, FAKE_GIT_SCRIPT).expect("write shared fake git");
        let mut permissions = fs::metadata(&path)
            .expect("fake git metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("chmod fake git");
        path
    })
}

fn sidecar(link: &Path, suffix: &str) -> PathBuf {
    let mut name = link.file_name().expect("fake git name").to_os_string();
    name.push(suffix);
    link.with_file_name(name)
}

fn link_fake(dir: &Path, name: &str) -> PathBuf {
    fs::create_dir_all(dir).expect("fake git dir");
    let path = dir.join(name);
    symlink(shared_script(), &path).expect("symlink fake git");
    path
}

fn write_mode(link: &Path, mode: &str) {
    fs::write(sidecar(link, ".mode"), mode).expect("write fake git mode");
}

fn probe_fake(path: &Path) -> Git {
    let inputs = DiscoverInputs {
        override_path: Some(path.to_path_buf().into()),
        path_var: None,
        exe_name: "git",
        fallbacks: Vec::new(),
    };
    discover_with(&inputs).unwrap_or_else(|error| panic!("probe fake git: {error}"))
}

fn write_executable(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("script directory");
    }
    fs::write(path, body).expect("write script");
    let mut permissions = fs::metadata(path).expect("script metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod script");
}

fn poke_script(dir: &Path, file_name: &str, marker: &Path) -> PathBuf {
    let path = dir.join(file_name);
    write_executable(&path, &format!("#!/bin/sh\ntouch {}\n", marker.display()));
    path
}

fn make_stat_dirty(path: &Path) {
    let file = fs::File::open(path).expect("tracked file");
    file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(86_400))
        .expect("set mtime so status must re-read the file");
}

fn assert_did_not_run(marker: &Path, what: &str) {
    assert!(
        !marker.exists(),
        "{what}\nmarker created at {}",
        marker.display()
    );
}

fn brief_error(error: &GitError) -> String {
    let rendered = format!("{error:?}");
    if rendered.len() > 600 {
        format!("{}… ({} bytes)", &rendered[..600], rendered.len())
    } else {
        rendered
    }
}

struct Fixture {
    tmp: TempDir,
    root: PathBuf,
    git: Git,
}

impl Fixture {
    fn new() -> Self {
        let git = Git::discover().expect("system git");
        let tmp = TempDir::with_prefix("cthulhu-adv-a-").expect("temp dir");
        let root = tmp.path().join("hostile-repo");
        fs::create_dir(&root).expect("repo dir");
        Self { tmp, root, git }
    }

    fn init() -> Self {
        let fixture = Self::new();
        fixture.git(&["init", "-b", "main"]);
        fixture
    }

    fn with_tracked_commit() -> Self {
        let fixture = Self::init();
        fixture.write("tracked.txt", "one\n");
        fixture.git(&["add", "tracked.txt"]);
        fixture.git(&["commit", "-m", "first"]);
        fixture
    }

    fn with_two_commits() -> Self {
        let fixture = Self::with_tracked_commit();
        fixture.write("tracked.txt", "two\n");
        fixture.git(&["add", "tracked.txt"]);
        fixture.git(&["commit", "-m", "second"]);
        fixture
    }

    fn on_branch(name: &str) -> Self {
        let fixture = Self::with_tracked_commit();
        fixture.git(&["switch", "-c", name]);
        fixture
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent dir");
        }
        fs::write(path, contents).expect("write");
    }

    fn git(&self, args: &[&str]) -> String {
        let mut command = Command::new(&self.git.path);
        command
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .env("GIT_AUTHOR_NAME", "cthulhu")
            .env("GIT_AUTHOR_EMAIL", "cthulhu@example.invalid")
            .env("GIT_COMMITTER_NAME", "cthulhu")
            .env("GIT_COMMITTER_EMAIL", "cthulhu@example.invalid")
            .args([
                "-c",
                "commit.gpgSign=false",
                "-c",
                "core.fsmonitor=false",
                "--no-pager",
            ])
            .args(args);
        for var in CLEARED_ENV {
            command.env_remove(var);
        }
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
        fs::canonicalize(&self.root).expect("canonical root")
    }

    fn marker(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }

    fn real_history(&self) -> String {
        self.git(&["--no-replace-objects", "log", "--format=%H %s"])
    }
}

struct ChildCheck<'a> {
    target: &'a Path,
    branch: &'a str,
    action: &'a str,
    history: Option<&'a str>,
    marker: Option<&'a Path>,
    required_var: &'a str,
    /// When set, the child asks system git for this key before opening the repo.
    /// The value must be visible, so a passing test is not a broken fixture.
    config_key: Option<&'a str>,
    config_value: Option<&'a str>,
    extra_env: &'a [(&'a str, String)],
}

fn assert_child_opens(check: &ChildCheck<'_>) {
    let root = fs::canonicalize(check.target).expect("canonical target");
    let sentinel = check
        .target
        .parent()
        .expect("repo parent")
        .join("adv-a-sentinel");
    let _ = fs::remove_file(&sentinel);

    let mut command = Command::new(env::current_exe().expect("test binary"));
    command
        .args([
            "--exact",
            CHILD_TEST,
            "--include-ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .stdin(Stdio::null())
        .env(CHILD_TARGET, check.target)
        .env(CHILD_ROOT, &root)
        .env(CHILD_BRANCH, check.branch)
        .env(CHILD_ACTION, check.action)
        .env(CHILD_REQUIRED, check.required_var)
        .env(CHILD_SENTINEL, &sentinel);
    if let (Some(key), Some(value)) = (check.config_key, check.config_value) {
        command.env(CHILD_CONFIG_KEY, key);
        command.env(CHILD_CONFIG_VALUE, value);
    }
    for var in CLEARED_ENV {
        command.env_remove(var);
    }
    if let Some(history) = check.history {
        command.env(CHILD_HISTORY, history);
    }
    if let Some(marker) = check.marker {
        command.env(CHILD_MARKER, marker);
    }
    for (key, value) in check.extra_env {
        command.env(key, value);
    }

    let output = command.output().expect("spawn child test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "child failed:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("1 passed"),
        "child did not run:\n{stdout}\n{stderr}"
    );
    assert!(
        sentinel.is_file(),
        "child returned without checking the repository:\n{stdout}\n{stderr}"
    );
}

fn config_count(pairs: &[(&str, &str)]) -> Vec<(&'static str, String)> {
    assert!(
        pairs.len() <= 4,
        "CLEARED_ENV only reserves four config slots"
    );
    let mut env = vec![("GIT_CONFIG_COUNT", pairs.len().to_string())];
    let keys = [
        ("GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0"),
        ("GIT_CONFIG_KEY_1", "GIT_CONFIG_VALUE_1"),
        ("GIT_CONFIG_KEY_2", "GIT_CONFIG_VALUE_2"),
        ("GIT_CONFIG_KEY_3", "GIT_CONFIG_VALUE_3"),
    ];
    for (pair, (key_var, value_var)) in pairs.iter().zip(keys) {
        env.push((key_var, pair.0.to_owned()));
        env.push((value_var, pair.1.to_owned()));
    }
    env
}

fn quoted_parameters(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("'{key}'='{value}'"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn hostile_command_config(poke: &Path, decoy_root: &Path) -> Vec<(String, String)> {
    vec![
        ("alias.status".to_owned(), format!("!{}", poke.display())),
        ("core.pager".to_owned(), poke.display().to_string()),
        ("core.fsmonitor".to_owned(), poke.display().to_string()),
        ("core.worktree".to_owned(), decoy_root.display().to_string()),
    ]
}

fn open_and_keep_history(fixture: &Fixture, marker: &Path, what: &str) {
    let info = inspect(&fixture.git, &fixture.root);
    let history = info
        .as_ref()
        .ok()
        .and_then(|info| history(&fixture.git, &info.root, &info.head, 10).ok());
    assert_did_not_run(marker, what);
    let info =
        info.unwrap_or_else(|error| panic!("{what} failed while opening the repository: {error}"));
    assert_eq!(info.root, fixture.canonical_root());
    assert_eq!(info.head, Head::Branch("main".to_owned()));
    let history = history.unwrap_or_else(|| panic!("{what} failed while reading history"));
    assert_eq!(
        history
            .commits
            .iter()
            .map(|commit| commit.summary.as_str())
            .collect::<Vec<_>>(),
        ["first"]
    );
}

fn dubious_stderr(path: &str) -> String {
    format!("fatal: detected dubious ownership in repository at '{path}'\n")
}

fn inspect_fake_stderr(dir: &Path, stderr: &str) -> GitError {
    let fake = link_fake(dir, "fake-git");
    write_mode(&fake, "fail-stderr");
    fs::write(sidecar(&fake, ".stderr"), stderr).expect("stderr fixture");
    fs::write(sidecar(&fake, ".exit"), "128\n").expect("exit fixture");
    let git = probe_fake(&fake);
    let repo = dir.join("workdir");
    fs::create_dir(&repo).expect("workdir");
    inspect(&git, &repo).expect_err("fake git must fail the open")
}

fn assert_suggestion_is_shell_safe(message: &str, expected_path: &str, marker: &Path) {
    let suggestion = message.split_once(SUGGESTION_MARK).map_or_else(
        || panic!("UnsafeRepository display should suggest a command:\n{message}"),
        |(_, command)| command,
    );
    let recorder_dir = marker.parent().expect("marker parent").join("recorder-bin");
    fs::create_dir_all(&recorder_dir).expect("recorder dir");
    let recorder = link_fake(&recorder_dir, "git");
    write_mode(&recorder, "record-argv");
    let log = marker.with_file_name("argv.log");

    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(suggestion)
        .env("PATH", &recorder_dir)
        .env("CTHULHU_ADV_A_ARGV_LOG", &log)
        .stdin(Stdio::null())
        .output()
        .expect("run the suggested command under sh");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !marker.exists(),
        "suggested command is not safe to paste into a shell.\nSuggestion:\n{suggestion}\nsh stderr:\n{stderr}"
    );
    assert!(
        log.is_file(),
        "suggested command did not invoke git once.\nSuggestion:\n{suggestion}\nstatus: {}\nsh stderr:\n{stderr}",
        output.status
    );
    let recorded = fs::read(&log).expect("read argv log");
    let args: Vec<&[u8]> = recorded
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .collect();
    assert_eq!(
        args.first().copied(),
        Some(&b"config"[..]),
        "suggestion:\n{suggestion}"
    );
    assert_eq!(
        args.get(1).copied(),
        Some(&b"--global"[..]),
        "suggestion:\n{suggestion}"
    );
    assert_eq!(
        args.get(2).copied(),
        Some(&b"--add"[..]),
        "suggestion:\n{suggestion}"
    );
    assert_eq!(
        args.get(3).copied(),
        Some(&b"safe.directory"[..]),
        "suggestion:\n{suggestion}"
    );
    let safe_directory = args
        .get(4)
        .map(|part| String::from_utf8_lossy(part).into_owned());
    assert_eq!(
        safe_directory.as_deref(),
        Some(expected_path),
        "safe.directory was not the full path as a single shell word.\nSuggestion:\n{suggestion}\nargv count: {}",
        args.len()
    );
    assert_eq!(
        args.len(),
        5,
        "suggestion gained extra shell words:\n{suggestion}"
    );
}

#[test]
fn normal_repository_inspect_reports_branch_and_root() {
    let fixture = Fixture::on_branch("feature/necronomicon");
    let info = inspect(&fixture.git, &fixture.root).expect("inspect");
    assert_eq!(info.name, "hostile-repo");
    assert_eq!(info.root, fixture.canonical_root());
    assert_eq!(info.head, Head::Branch("feature/necronomicon".to_owned()));
}

#[test]
fn stat_dirty_clean_filter_is_not_executed_on_inspect() {
    let fixture = Fixture::with_tracked_commit();
    let marker = fixture.marker("clean-filter");
    let poke = poke_script(fixture.tmp.path(), "clean-poke.sh", &marker);
    fixture.write(".gitattributes", "* filter=evil\n");
    fixture.git(&[
        "config",
        "--local",
        "filter.evil.clean",
        &poke.display().to_string(),
    ]);
    make_stat_dirty(&fixture.root.join("tracked.txt"));

    let opened = inspect(&fixture.git, &fixture.root);
    assert_did_not_run(
        &marker,
        "inspect ran filter.evil.clean from the repository while reading status",
    );
    let info = opened.expect("inspect should open a repository that defines a clean filter");
    assert_eq!(info.root, fixture.canonical_root());
    assert_eq!(info.head, Head::Branch("main".to_owned()));
}

#[test]
fn stat_dirty_process_filter_is_not_executed_on_inspect() {
    let fixture = Fixture::with_tracked_commit();
    let marker = fixture.marker("process-filter");
    let poke = poke_script(fixture.tmp.path(), "process-poke.sh", &marker);
    fixture.write(".gitattributes", "* filter=evil\n");
    fixture.git(&[
        "config",
        "--local",
        "filter.evil.process",
        &poke.display().to_string(),
    ]);
    make_stat_dirty(&fixture.root.join("tracked.txt"));

    let opened = inspect(&fixture.git, &fixture.root);
    assert_did_not_run(
        &marker,
        "inspect ran filter.evil.process from the repository while reading status",
    );
    let info = opened.expect("inspect should open a repository that defines a process filter");
    assert_eq!(info.root, fixture.canonical_root());
}

#[test]
fn repo_alias_status_is_not_executed() {
    let fixture = Fixture::with_tracked_commit();
    let marker = fixture.marker("alias-status");
    let poke = poke_script(fixture.tmp.path(), "alias-status.sh", &marker);
    fixture.git(&[
        "config",
        "--local",
        "alias.status",
        &format!("!{}", poke.display()),
    ]);

    let opened = inspect(&fixture.git, &fixture.root);
    assert_did_not_run(&marker, "alias.status");
    let info = opened.expect("inspect should ignore alias.status");
    assert_eq!(info.root, fixture.canonical_root());
    assert_eq!(info.head, Head::Branch("main".to_owned()));
}

#[test]
fn repo_alias_rev_parse_is_not_executed() {
    let fixture = Fixture::with_tracked_commit();
    let marker = fixture.marker("alias-rev-parse");
    let poke = poke_script(fixture.tmp.path(), "alias-rev-parse.sh", &marker);
    fixture.git(&[
        "config",
        "--local",
        "alias.rev-parse",
        &format!("!{}", poke.display()),
    ]);

    let opened = inspect(&fixture.git, &fixture.root);
    assert_did_not_run(&marker, "alias.rev-parse");
    let info = opened.expect("inspect should ignore alias.rev-parse");
    assert_eq!(info.root, fixture.canonical_root());
}

#[test]
fn repo_alias_log_is_not_executed() {
    let fixture = Fixture::with_tracked_commit();
    let marker = fixture.marker("alias-log");
    let poke = poke_script(fixture.tmp.path(), "alias-log.sh", &marker);
    fixture.git(&[
        "config",
        "--local",
        "alias.log",
        &format!("!{}", poke.display()),
    ]);
    open_and_keep_history(&fixture, &marker, "alias.log");
}

#[test]
fn include_path_alias_status_is_not_executed() {
    let fixture = Fixture::with_tracked_commit();
    let marker = fixture.marker("include-alias");
    let poke = poke_script(fixture.tmp.path(), "include-alias.sh", &marker);
    let included = fixture.tmp.path().join("alias.gitconfig");
    fs::write(
        &included,
        format!("[alias]\n\tstatus = !{}\n", poke.display()),
    )
    .expect("included config");
    fixture.git(&[
        "config",
        "--local",
        "include.path",
        &included.display().to_string(),
    ]);

    let opened = inspect(&fixture.git, &fixture.root);
    assert_did_not_run(&marker, "include.path alias.status");
    let info = opened.expect("inspect should ignore an alias installed through include.path");
    assert_eq!(info.root, fixture.canonical_root());
    assert_eq!(info.head, Head::Branch("main".to_owned()));
}

#[test]
fn include_path_cannot_install_a_clean_filter() {
    let fixture = Fixture::with_tracked_commit();
    let marker = fixture.marker("include-filter");
    let poke = poke_script(fixture.tmp.path(), "include-filter.sh", &marker);
    let included = fixture.tmp.path().join("filter.gitconfig");
    fs::write(
        &included,
        format!("[filter \"evil\"]\n\tclean = {}\n", poke.display()),
    )
    .expect("included config");
    fixture.write(".gitattributes", "* filter=evil\n");
    fixture.git(&[
        "config",
        "--local",
        "include.path",
        &included.display().to_string(),
    ]);
    make_stat_dirty(&fixture.root.join("tracked.txt"));

    let opened = inspect(&fixture.git, &fixture.root);
    assert_did_not_run(
        &marker,
        "include.path installed filter.evil.clean and inspect executed it",
    );
    let info = opened.expect("inspect should ignore filters reached through include.path");
    assert_eq!(info.root, fixture.canonical_root());
}

#[test]
fn repo_hooks_path_is_not_executed() {
    let fixture = Fixture::with_tracked_commit();
    let marker = fixture.marker("hooks-ran");
    let hooks = fixture.tmp.path().join("hooks");
    let body = format!("#!/bin/sh\ntouch {}\n", marker.display());
    for name in [
        "pre-commit",
        "post-commit",
        "post-checkout",
        "post-merge",
        "post-index-change",
        "reference-transaction",
        "fsmonitor-watchman",
    ] {
        write_executable(&hooks.join(name), &body);
    }
    fixture.git(&[
        "config",
        "--local",
        "core.hooksPath",
        &hooks.display().to_string(),
    ]);
    open_and_keep_history(&fixture, &marker, "core.hooksPath");
}

#[test]
fn repo_core_pager_is_not_executed() {
    let fixture = Fixture::with_tracked_commit();
    let marker = fixture.marker("pager");
    let poke = poke_script(fixture.tmp.path(), "pager.sh", &marker);
    fixture.git(&[
        "config",
        "--local",
        "core.pager",
        &poke.display().to_string(),
    ]);
    open_and_keep_history(&fixture, &marker, "core.pager");
}

#[test]
fn repo_core_fsmonitor_is_not_executed() {
    let fixture = Fixture::with_tracked_commit();
    let marker = fixture.marker("fsmonitor");
    let poke = poke_script(fixture.tmp.path(), "fsmonitor.sh", &marker);
    fixture.git(&[
        "config",
        "--local",
        "core.fsmonitor",
        &poke.display().to_string(),
    ]);
    open_and_keep_history(&fixture, &marker, "core.fsmonitor");
}

#[test]
fn inherited_git_config_count_cannot_run_commands_or_redirect() {
    let target = Fixture::on_branch("feature/necronomicon");
    let decoy = Fixture::on_branch("decoy");
    let marker = target.marker("config-count");
    let poke = poke_script(target.tmp.path(), "count-poke.sh", &marker);
    let pairs = hostile_command_config(&poke, &decoy.canonical_root());
    let pair_refs: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let owned = config_count(&pair_refs);
    let extra: Vec<(&str, String)> = owned;

    assert_child_opens(&ChildCheck {
        target: &target.root,
        branch: "feature/necronomicon",
        action: "inspect",
        history: None,
        marker: Some(&marker),
        required_var: "GIT_CONFIG_COUNT",
        config_key: Some("alias.status"),
        config_value: Some(pairs[0].1.as_str()),
        extra_env: &extra,
    });
}

#[test]
fn inherited_git_config_parameters_cannot_run_commands_or_redirect() {
    let target = Fixture::on_branch("feature/necronomicon");
    let decoy = Fixture::on_branch("decoy");
    let marker = target.marker("config-parameters");
    let poke = poke_script(target.tmp.path(), "parameters-poke.sh", &marker);
    let pairs = hostile_command_config(&poke, &decoy.canonical_root());
    let pair_refs: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let extra = [("GIT_CONFIG_PARAMETERS", quoted_parameters(&pair_refs))];

    assert_child_opens(&ChildCheck {
        target: &target.root,
        branch: "feature/necronomicon",
        action: "inspect",
        history: None,
        marker: Some(&marker),
        required_var: "GIT_CONFIG_PARAMETERS",
        config_key: Some("alias.status"),
        config_value: Some(pairs[0].1.as_str()),
        extra_env: &extra,
    });
}

fn write_hostile_gitconfig(path: &Path, poke: &Path, decoy_root: &Path) {
    fs::write(
        path,
        format!(
            "[alias]\n\tstatus = !{poke}\n[core]\n\tpager = {poke}\n\tfsmonitor = {poke}\n\tworktree = {decoy}\n",
            poke = poke.display(),
            decoy = decoy_root.display()
        ),
    )
    .expect("hostile gitconfig");
}

#[test]
fn inherited_git_config_global_cannot_run_commands_or_redirect() {
    let target = Fixture::on_branch("feature/necronomicon");
    let decoy = Fixture::on_branch("decoy");
    let marker = target.marker("config-global");
    let poke = poke_script(target.tmp.path(), "global-poke.sh", &marker);
    let config = target.tmp.path().join("global.gitconfig");
    write_hostile_gitconfig(&config, &poke, &decoy.canonical_root());
    let extra = [("GIT_CONFIG_GLOBAL", config.display().to_string())];
    let alias = format!("!{}", poke.display());

    assert_child_opens(&ChildCheck {
        target: &target.root,
        branch: "feature/necronomicon",
        action: "inspect",
        history: None,
        marker: Some(&marker),
        required_var: "GIT_CONFIG_GLOBAL",
        config_key: Some("alias.status"),
        config_value: Some(&alias),
        extra_env: &extra,
    });
}

#[test]
fn inherited_git_config_system_cannot_run_commands_or_redirect() {
    let target = Fixture::on_branch("feature/necronomicon");
    let decoy = Fixture::on_branch("decoy");
    let marker = target.marker("config-system");
    let poke = poke_script(target.tmp.path(), "system-poke.sh", &marker);
    let config = target.tmp.path().join("system.gitconfig");
    write_hostile_gitconfig(&config, &poke, &decoy.canonical_root());
    let extra = [("GIT_CONFIG_SYSTEM", config.display().to_string())];
    let alias = format!("!{}", poke.display());

    assert_child_opens(&ChildCheck {
        target: &target.root,
        branch: "feature/necronomicon",
        action: "inspect",
        history: None,
        marker: Some(&marker),
        required_var: "GIT_CONFIG_SYSTEM",
        config_key: Some("alias.status"),
        config_value: Some(&alias),
        extra_env: &extra,
    });
}

#[test]
fn inherited_git_replace_ref_base_does_not_hide_commits() {
    let fixture = Fixture::with_two_commits();
    let expected = fixture.real_history();
    let head = fixture.git(&["rev-parse", "HEAD"]);
    let tree = fixture.git(&["rev-parse", &format!("{head}^{{tree}}")]);
    let evil = fixture.git(&["commit-tree", &tree, "-m", "EVIL-REPLACE"]);
    let namespace = fixture.root.join(".git/refs/hidden");
    fs::create_dir_all(&namespace).expect("replace namespace");
    fs::write(namespace.join(&head), format!("{evil}\n")).expect("replace ref");
    let extra = [("GIT_REPLACE_REF_BASE", "refs/hidden/".to_owned())];

    assert_child_opens(&ChildCheck {
        target: &fixture.root,
        branch: "main",
        action: "history",
        history: Some(&expected),
        marker: None,
        required_var: "GIT_REPLACE_REF_BASE",
        config_key: None,
        config_value: None,
        extra_env: &extra,
    });
}

#[test]
fn inherited_git_shallow_file_does_not_hide_commits() {
    let fixture = Fixture::with_two_commits();
    let expected = fixture.real_history();
    let head = fixture.git(&["rev-parse", "HEAD"]);
    let shallow = fixture.marker("shallow");
    fs::write(&shallow, format!("{head}\n")).expect("shallow file");
    let extra = [("GIT_SHALLOW_FILE", shallow.display().to_string())];

    assert_child_opens(&ChildCheck {
        target: &fixture.root,
        branch: "main",
        action: "history",
        history: Some(&expected),
        marker: None,
        required_var: "GIT_SHALLOW_FILE",
        config_key: None,
        config_value: None,
        extra_env: &extra,
    });
}

#[test]
fn inherited_git_graft_file_does_not_hide_commits() {
    let fixture = Fixture::with_two_commits();
    let expected = fixture.real_history();
    let head = fixture.git(&["rev-parse", "HEAD"]);
    let graft = fixture.marker("graft");
    fs::write(&graft, format!("{head}\n")).expect("graft file");
    let extra = [("GIT_GRAFT_FILE", graft.display().to_string())];

    assert_child_opens(&ChildCheck {
        target: &fixture.root,
        branch: "main",
        action: "history",
        history: Some(&expected),
        marker: None,
        required_var: "GIT_GRAFT_FILE",
        config_key: None,
        config_value: None,
        extra_env: &extra,
    });
}

#[test]
fn inherited_git_exec_path_does_not_run_dashed_commands() {
    let fixture = Fixture::with_tracked_commit();
    let expected = fixture.real_history();
    let marker = fixture.marker("exec-ran");
    let exec_dir = fixture.tmp.path().join("exec-path");
    let body = format!("#!/bin/sh\ntouch {}\n", marker.display());
    for name in ["git-status", "git-rev-parse", "git-log"] {
        write_executable(&exec_dir.join(name), &body);
    }
    let extra = [("GIT_EXEC_PATH", exec_dir.display().to_string())];

    assert_child_opens(&ChildCheck {
        target: &fixture.root,
        branch: "main",
        action: "history",
        history: Some(&expected),
        marker: Some(&marker),
        required_var: "GIT_EXEC_PATH",
        config_key: None,
        config_value: None,
        extra_env: &extra,
    });
}

#[test]
fn inherited_git_trace_does_not_write_a_trace_file() {
    let fixture = Fixture::on_branch("feature/necronomicon");
    let trace = fixture.marker("git.trace");
    let extra = [("GIT_TRACE", trace.display().to_string())];

    assert_child_opens(&ChildCheck {
        target: &fixture.root,
        branch: "feature/necronomicon",
        action: "inspect",
        history: None,
        marker: Some(&trace),
        required_var: "GIT_TRACE",
        config_key: None,
        config_value: None,
        extra_env: &extra,
    });
}

#[test]
fn not_a_repository_substring_inside_a_different_fatal_is_not_that_error() {
    let fixture = Fixture::with_tracked_commit();
    let stderr = "fatal: cannot change to 'not a git repository': No such file or directory\n";
    let error = inspect_fake_stderr(fixture.tmp.path(), stderr);
    match &error {
        GitError::NotARepository(_) => panic!(
            "a fatal whose primary message is not \"not a git repository\" was classified as NotARepository: {}",
            brief_error(&error)
        ),
        GitError::Failed {
            stderr, command, ..
        } => {
            assert!(
                stderr.contains("not a git repository"),
                "fixture stderr lost the substring: {stderr}"
            );
            assert!(
                stderr.contains("cannot change to"),
                "fixture was not the missing-path fatal: {stderr}"
            );
            assert!(
                !stderr
                    .lines()
                    .any(|line| line.trim_start().starts_with("fatal: not a git repository")),
                "fixture accidentally used the primary not-a-repository fatal"
            );
            assert!(
                command.contains("rev-parse"),
                "failure lost the command: {command}"
            );
        }
        other => panic!("expected a generic git failure, got {}", brief_error(other)),
    }
}

#[test]
fn primary_not_a_repository_fatal_is_still_typed() {
    let tmp = TempDir::with_prefix("cthulhu-adv-a-notar-").expect("temp dir");
    let stderr = "fatal: not a git repository (or any of the parent directories): .git\n";
    let error = inspect_fake_stderr(tmp.path(), stderr);
    assert!(
        matches!(error, GitError::NotARepository(_)),
        "the primary not-a-repository fatal should stay typed: {}",
        brief_error(&error)
    );
}

fn assert_dubious_path_is_quoted(path: &str, marker: &Path, dir: &Path) {
    let error = inspect_fake_stderr(dir, &dubious_stderr(path));
    let message = error.to_string();
    assert!(
        matches!(error, GitError::UnsafeRepository(_)),
        "dubious ownership should stay typed and must not panic: {message}"
    );
    assert_suggestion_is_shell_safe(&message, path, marker);
}

#[test]
fn dubious_ownership_path_with_space_is_shell_quoted() {
    let tmp = TempDir::with_prefix("cthulhu-adv-a-quote-").expect("temp dir");
    let path = format!("{}/owned repo", tmp.path().display());
    let marker = tmp.path().join("injection-marker");
    assert_dubious_path_is_quoted(&path, &marker, tmp.path());
}

#[test]
fn dubious_ownership_path_with_quote_is_shell_quoted() {
    let tmp = TempDir::with_prefix("cthulhu-adv-a-quote-").expect("temp dir");
    let path = format!("{}/o'reilly", tmp.path().display());
    let marker = tmp.path().join("injection-marker");
    assert_dubious_path_is_quoted(&path, &marker, tmp.path());
}

#[test]
fn dubious_ownership_path_with_newline_is_shell_quoted() {
    let tmp = TempDir::with_prefix("cthulhu-adv-a-quote-").expect("temp dir");
    let marker = tmp.path().join("injection-marker");
    let path = format!(
        "{}/owned\n/usr/bin/touch {}",
        tmp.path().display(),
        marker.display()
    );
    assert_dubious_path_is_quoted(&path, &marker, tmp.path());
}

#[test]
fn slow_version_probe_does_not_block_discovery() {
    let tmp = TempDir::with_prefix("cthulhu-adv-a-slow-").expect("temp dir");
    let slow_dir = tmp.path().join("slow");
    let fast_dir = tmp.path().join("fast");
    let slow = link_fake(&slow_dir, "git");
    let fast = link_fake(&fast_dir, "git");
    write_mode(&slow, "slow-version");
    write_mode(&fast, "version");
    let inputs = DiscoverInputs {
        override_path: None,
        path_var: Some(env::join_paths([&slow_dir, &fast_dir]).expect("PATH")),
        exe_name: "git",
        fallbacks: Vec::new(),
    };
    let fast_path = fast.clone();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _keep = tmp;
        let found = discover_with(&inputs).map(|git| git.path);
        let _ = tx.send(found);
    });

    match rx.recv_timeout(Duration::from_secs(1)) {
        Ok(Ok(path)) => assert_eq!(
            path, fast_path,
            "discover_with should use the later git that answers --version promptly"
        ),
        Ok(Err(error)) => panic!("discover_with failed: {error}"),
        Err(mpsc::RecvTimeoutError::Timeout) => panic!(
            "discover_with still running after 1s while probing a git --version that sleeps; a later candidate is good"
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("discover thread exited without a result");
        }
    }
}

#[test]
fn rev_parse_output_bomb_is_refused() {
    let tmp = TempDir::with_prefix("cthulhu-adv-a-bomb-").expect("temp dir");
    let fake = link_fake(tmp.path(), "git");
    write_mode(&fake, "bomb-rev-parse");
    let git = probe_fake(&fake);
    let workdir = tmp.path().join("workdir");
    fs::create_dir(&workdir).expect("workdir");

    match inspect(&git, &workdir) {
        Ok(info) => panic!(
            "inspect accepted unbounded rev-parse stdout as a repository root ({} bytes)",
            info.root.as_os_str().len()
        ),
        Err(GitError::Failed { stderr, .. }) if stderr_refuses_unbounded_output(&stderr) => {}
        Err(error) => panic!(
            "32MiB rev-parse stdout must be refused with a typed output-limit error before it is used as a path; got {}",
            brief_error(&error)
        ),
    }
}

fn stderr_refuses_unbounded_output(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    ["limit", "too large", "too long", "exceed", "unbounded"]
        .iter()
        .any(|needle| lower.contains(needle))
}

#[test]
#[ignore = "spawned with a hostile environment by the inherited-env tests"]
fn child_opens_repo_despite_hostile_environment() {
    let Ok(target) = env::var(CHILD_TARGET) else {
        return;
    };
    let required = env::var(CHILD_REQUIRED).expect("parent sets the required variable name");
    assert!(
        env::var_os(&required).is_some(),
        "parent did not export {required}"
    );

    let git = Git::discover().expect("system git");
    if let Ok(key) = env::var(CHILD_CONFIG_KEY) {
        let expected = env::var(CHILD_CONFIG_VALUE).expect("expected config value");
        let output = Command::new(&git.path)
            .args(["config", "--get", &key])
            .stdin(Stdio::null())
            .output()
            .expect("git config");
        assert!(
            output.status.success(),
            "hostile config key {key} was not visible to git: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            expected,
            "git did not load the hostile config value for {key}"
        );
    }

    let info = inspect(&git, Path::new(&target)).expect("inspect target");
    assert_eq!(
        info.root,
        PathBuf::from(env::var(CHILD_ROOT).expect("expected root"))
    );
    assert_eq!(
        info.head,
        Head::Branch(env::var(CHILD_BRANCH).expect("expected branch"))
    );

    if env::var(CHILD_ACTION).expect("action") == "history" {
        let history = history(&git, &info.root, &info.head, 10).expect("history");
        let got = history
            .commits
            .iter()
            .map(|commit| format!("{} {}", commit.oid, commit.summary))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            got,
            env::var(CHILD_HISTORY).expect("expected history"),
            "history did not match the real commits"
        );
    }

    if let Ok(marker) = env::var(CHILD_MARKER) {
        assert!(
            !Path::new(&marker).exists(),
            "hostile inherited environment executed a command or wrote {marker}"
        );
    }

    let sentinel = env::var(CHILD_SENTINEL).expect("sentinel path");
    fs::write(sentinel, "checked\n").expect("write sentinel");
}
