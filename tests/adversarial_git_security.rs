//! Adversarial checks for the read-only git boundary.
//!
//! Opening a repository must not execute commands planted in that repository
//! or in the inherited environment, must not hide the real history, and must
//! reject unbounded or ambiguous git output. The history graph is a later
//! `branch_graph` log (`--max-count`, not `log -1`) and is held to the same
//! rules. Tests do not change this process's environment or working directory.
//! Inherited variables are applied only to a child re-exec of this binary.

#![cfg(unix)]

use std::env;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use cthulhu_git::git::{
    DiscoverInputs, Git, GitError, GraphTip, Head, OVERRIDE_VAR, branch_graph, discover_with,
    fetch, history, inspect, pull_ff_only, switch_branch,
};
use tempfile::TempDir;

const CHILD_TEST: &str = "child_opens_repo_despite_hostile_environment";
const CHILD_TARGET: &str = "CTHULHU_ADV_A_TARGET";
const CHILD_ROOT: &str = "CTHULHU_ADV_A_ROOT";
const CHILD_BRANCH: &str = "CTHULHU_ADV_A_BRANCH";
const CHILD_ACTION: &str = "CTHULHU_ADV_A_ACTION";
const CHILD_HISTORY: &str = "CTHULHU_ADV_A_HISTORY";
const CHILD_TIP: &str = "CTHULHU_ADV_A_TIP";
const CHILD_MARKER: &str = "CTHULHU_ADV_A_MARKER";
const CHILD_REQUIRED: &str = "CTHULHU_ADV_A_REQUIRED";
const CHILD_SENTINEL: &str = "CTHULHU_ADV_A_SENTINEL";
const CHILD_CONFIG_KEY: &str = "CTHULHU_ADV_A_CONFIG_KEY";
const CHILD_CONFIG_VALUE: &str = "CTHULHU_ADV_A_CONFIG_VALUE";
const SUGGESTION_MARK: &str = "If you trust it, run: ";

/// Any positive limit runs the same `--max-count` log the window asks for.
/// The window's own limit only changes how many commits come back.
const GRAPH_LOG_LIMIT: usize = 10;

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
# The deferred history log is `git log --max-count=…`, not `log -1`.
history_log_argv() {
  is_log=0
  has_max=0
  for arg in "$@"; do
    if [ "$arg" = "log" ]; then
      is_log=1
    fi
    if [ "$arg" = "-1" ]; then
      echo "refusing log -1; this fake is the history log" >&2
      exit 1
    fi
    case "$arg" in
      --max-count=*) has_max=1 ;;
    esac
  done
  if [ "$is_log" != 1 ] || [ "$has_max" != 1 ]; then
    echo "unexpected fake git arguments: $*" >&2
    exit 1
  fi
}

mode=$(cat "$0.mode" 2>/dev/null || echo version)
case "$mode" in
  version)
    echo "git version 2.43.0"
    exit 0
    ;;
  slow-version)
    if [ "$1" = "--version" ]; then
      sleep 30 &
      echo $! > "$0.sleeper"
      wait
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
      if [ "$arg" = "--is-bare-repository" ]; then
        echo false
        exit 0
      fi
      if [ "$arg" = "--show-toplevel" ]; then
        # 32 MiB of ASCII. Do not raise this; the bound under test is ~32 MiB.
        dd if=/dev/zero bs=1048576 count=32 2>/dev/null | tr '\0' 'A' &
        echo $! > "$0.producer"
        wait
        echo
        exit 0
      fi
    done
    echo "unexpected fake git arguments: $*" >&2
    exit 1
    ;;
  detach-writer)
    if [ "$1" = "--version" ]; then
      echo "git version 2.43.0"
      exit 0
    fi
    # Exit 0 while a grandchild keeps stdout open. The caller must still return.
    sleep 30 >&1 &
    echo $! > "$0.writer"
    exit 0
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
  record-command)
    if [ "$1" = "--version" ]; then
      echo "git version 2.43.0"
      exit 0
    fi
    if [ ! -f "$0.log" ]; then
      echo "missing argv log sidecar" >&2
      exit 1
    fi
    log=$(cat "$0.log")
    printf '%s\0' "$@" > "$log"
    exit 0
    ;;
  slow-command)
    if [ "$1" = "--version" ]; then
      echo "git version 2.43.0"
      exit 0
    fi
    sleep 120 &
    echo $! > "$0.sleeper"
    wait
    exit 0
    ;;
  slow-log)
    if [ "$1" = "--version" ]; then
      echo "git version 2.43.0"
      exit 0
    fi
    history_log_argv "$@"
    # Outlives the 60s command timeout so the caller must kill the group and return.
    sleep 120 &
    echo $! > "$0.sleeper"
    wait
    exit 0
    ;;
  bomb-log)
    if [ "$1" = "--version" ]; then
      echo "git version 2.43.0"
      exit 0
    fi
    history_log_argv "$@"
    # 9 MiB is over the 8 MiB cap. Do not shrink this below that cap.
    dd if=/dev/zero bs=1048576 count=9 2>/dev/null | tr '\0' 'A' &
    echo $! > "$0.producer"
    wait
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

fn editor_trap_script(dir: &Path, marker: &Path) -> PathBuf {
    let path = dir.join("editor-trap.sh");
    write_executable(
        &path,
        &format!("#!/bin/sh\ntouch {}\nexit 1\n", marker.display()),
    );
    path
}

/// Runs git in `root` with the same isolation as [`Fixture::git`].
fn repo_git(git: &Git, scratch: &Path, root: &Path, args: &[&str]) -> String {
    let mut command = Command::new(&git.path);
    command
        .current_dir(root)
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
        ]);
    for var in CLEARED_ENV {
        command.env_remove(var);
    }
    let hooks = scratch.join("empty-hooks");
    let global = scratch.join("empty.gitconfig");
    let system = scratch.join("empty.system.gitconfig");
    let _ = fs::create_dir_all(&hooks);
    let _ = fs::write(&global, "");
    let _ = fs::write(&system, "");
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &global)
        .env("GIT_CONFIG_SYSTEM", &system)
        .arg("-c")
        .arg(format!("core.hooksPath={}", hooks.display()))
        .args(args);
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

struct DivergedPullSetup {
    _origin: Fixture,
    local_root: PathBuf,
    _local_tmp: TempDir,
    git: Git,
    head_before: String,
    editor_marker: PathBuf,
}

/// Origin and a clone with diverged histories, upstream tracking, editor traps, and HEAD recorded.
fn diverged_pull_setup(configure: impl Fn(&Path, &Path, &Git)) -> DivergedPullSetup {
    let origin = Fixture::init();
    origin.write("tracked.txt", "base\n");
    origin.git(&["add", "tracked.txt"]);
    origin.git(&["commit", "-m", "base"]);

    let local_tmp = TempDir::with_prefix("cthulhu-adv-pull-").expect("local temp");
    let local_root = local_tmp.path().join("clone");
    let origin_path = fs::canonicalize(&origin.root).expect("origin path");
    repo_git(
        &origin.git,
        origin.tmp.path(),
        origin.tmp.path(),
        &[
            "clone",
            "--",
            origin_path.to_str().expect("utf8"),
            local_root.to_str().expect("utf8"),
        ],
    );

    origin.write("tracked.txt", "origin only\n");
    origin.git(&["add", "tracked.txt"]);
    origin.git(&["commit", "-m", "origin"]);

    fs::write(local_root.join("tracked.txt"), "local only\n").expect("local write");
    repo_git(
        &origin.git,
        local_tmp.path(),
        &local_root,
        &["add", "tracked.txt"],
    );
    repo_git(
        &origin.git,
        local_tmp.path(),
        &local_root,
        &["commit", "-m", "local"],
    );

    let editor_marker = local_tmp.path().join("editor-ran");
    let editor = editor_trap_script(local_tmp.path(), &editor_marker);
    let editor_path = fs::canonicalize(&editor).expect("editor script path");
    repo_git(
        &origin.git,
        local_tmp.path(),
        &local_root,
        &[
            "config",
            "--local",
            "sequence.editor",
            editor_path.to_str().expect("utf8"),
        ],
    );
    repo_git(
        &origin.git,
        local_tmp.path(),
        &local_root,
        &[
            "config",
            "--local",
            "core.editor",
            editor_path.to_str().expect("utf8"),
        ],
    );

    configure(local_tmp.path(), &local_root, &origin.git);

    let head_before = repo_git(
        &origin.git,
        local_tmp.path(),
        &local_root,
        &["rev-parse", "HEAD"],
    );

    let git = origin.git.clone();
    DivergedPullSetup {
        _origin: origin,
        local_root,
        _local_tmp: local_tmp,
        git,
        head_before,
        editor_marker,
    }
}

fn assert_no_rebase_or_merge_state(root: &Path) {
    let git_dir = root.join(".git");
    assert!(
        !git_dir.join("rebase-merge").exists(),
        "rebase-merge should not exist under {}",
        git_dir.display()
    );
    assert!(
        !git_dir.join("rebase-apply").exists(),
        "rebase-apply should not exist under {}",
        git_dir.display()
    );
    assert!(
        !git_dir.join("MERGE_HEAD").exists(),
        "MERGE_HEAD should not exist under {}",
        git_dir.display()
    );
}

fn assert_pull_ff_only_rejects_rebase(setup: &DivergedPullSetup) {
    let result = pull_ff_only(&setup.git, &setup.local_root);
    assert!(
        result.is_err(),
        "divergent pull with rebase config must fail, got {:?}",
        result
    );
    let head_after = repo_git(
        &setup.git,
        setup._local_tmp.path(),
        &setup.local_root,
        &["rev-parse", "HEAD"],
    );
    assert_eq!(head_after, setup.head_before, "HEAD must not move");
    assert_did_not_run(
        &setup.editor_marker,
        "pull must not invoke sequence.editor or core.editor",
    );
    assert_no_rebase_or_merge_state(&setup.local_root);
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

fn assert_recorded_pid_is_dead(path: &Path) {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("missing pid file {}: {error}", path.display()));
    let pid: u32 = text
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("pid in {}: {text:?}", path.display()));
    assert!(pid > 1, "refusing to treat pid {pid} as the grandchild");
    let status = Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("kill -0");
    assert!(
        !status.success(),
        "grandchild pid {pid} is still alive after the git that spawned it was stopped"
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
            ]);
        for var in CLEARED_ENV {
            command.env_remove(var);
        }
        let hooks = self.tmp.path().join("empty-hooks");
        let global = self.tmp.path().join("empty.gitconfig");
        let system = self.tmp.path().join("empty.system.gitconfig");
        let _ = fs::create_dir_all(&hooks);
        let _ = fs::write(&global, "");
        let _ = fs::write(&system, "");
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &global)
            .env("GIT_CONFIG_SYSTEM", &system)
            .arg("-c")
            .arg(format!("core.hooksPath={}", hooks.display()))
            .args(args);
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

fn graph_tip(name: &str, oid: impl Into<String>) -> GraphTip {
    GraphTip {
        name: name.to_owned(),
        oid: oid.into(),
        color: 0,
        principal: true,
        current: true,
    }
}

/// The worker calls `branch_graph`. A one-commit repository must come back whole,
/// and `marker` must not have been created by that log.
fn assert_branch_graph_keeps_commit(fixture: &Fixture, marker: &Path, what: &str) {
    let oid = fixture.git(&["rev-parse", "HEAD"]);
    let loaded = branch_graph(
        &fixture.git,
        &fixture.canonical_root(),
        &[graph_tip("main", oid)],
        GRAPH_LOG_LIMIT,
    );
    assert_did_not_run(marker, what);
    let graph = loaded
        .unwrap_or_else(|error| panic!("{what} failed while reading the history graph: {error}"));
    assert_eq!(
        graph
            .rows
            .iter()
            .map(|row| row.summary.as_str())
            .collect::<Vec<_>>(),
        ["first"],
        "{what} did not keep the real commit"
    );
    assert!(!graph.truncated, "{what} truncated a one-commit history");
}

fn assert_history_log_command(command: &str) {
    let args: Vec<&str> = command.split_whitespace().collect();
    assert!(args.contains(&"log"), "not the history log: {command}");
    assert!(
        args.iter().any(|arg| arg.starts_with("--max-count=")),
        "not the --max-count history log: {command}"
    );
    assert!(
        !args.contains(&"-1"),
        "log -1 is not the deferred history log: {command}"
    );
}

fn assert_git_honors_trace_var(git: &Git, var: &str, dir: &Path) {
    let proof = dir.join(format!("{var}.proof"));
    let _ = fs::remove_file(&proof);
    let output = Command::new(&git.path)
        .arg("--version")
        .stdin(Stdio::null())
        .env_remove("GIT_TRACE")
        .env_remove("GIT_TRACE2")
        .env_remove("GIT_TRACE2_EVENT")
        .env(var, &proof)
        .output()
        .unwrap_or_else(|error| panic!("git {var} proof: {error}"));
    assert!(
        output.status.success(),
        "git {var} proof failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = fs::metadata(&proof).map(|meta| meta.len()).unwrap_or(0);
    let _ = fs::remove_file(&proof);
    assert!(
        bytes > 0,
        "git did not write {var}; branch_graph could pass without the app clearing it"
    );
}

#[test]
fn stat_dirty_clean_filter_is_not_executed_on_branch_graph() {
    let fixture = Fixture::with_tracked_commit();
    let marker = fixture.marker("graph-clean-filter");
    let poke = poke_script(fixture.tmp.path(), "graph-clean-poke.sh", &marker);
    fixture.write(".gitattributes", "* filter=evil\n");
    fixture.git(&[
        "config",
        "--local",
        "filter.evil.clean",
        &poke.display().to_string(),
    ]);
    make_stat_dirty(&fixture.root.join("tracked.txt"));
    assert_branch_graph_keeps_commit(
        &fixture,
        &marker,
        "branch_graph ran filter.evil.clean from the repository while reading the history log",
    );
}

#[test]
fn stat_dirty_process_filter_is_not_executed_on_branch_graph() {
    let fixture = Fixture::with_tracked_commit();
    let marker = fixture.marker("graph-process-filter");
    let poke = poke_script(fixture.tmp.path(), "graph-process-poke.sh", &marker);
    fixture.write(".gitattributes", "* filter=evil\n");
    fixture.git(&[
        "config",
        "--local",
        "filter.evil.process",
        &poke.display().to_string(),
    ]);
    make_stat_dirty(&fixture.root.join("tracked.txt"));
    assert_branch_graph_keeps_commit(
        &fixture,
        &marker,
        "branch_graph ran filter.evil.process from the repository while reading the history log",
    );
}

#[test]
fn repo_alias_log_is_not_executed_on_branch_graph() {
    let fixture = Fixture::with_tracked_commit();
    let marker = fixture.marker("graph-alias-log");
    let poke = poke_script(fixture.tmp.path(), "graph-alias-log.sh", &marker);
    fixture.git(&[
        "config",
        "--local",
        "alias.log",
        &format!("!{}", poke.display()),
    ]);
    assert_branch_graph_keeps_commit(&fixture, &marker, "alias.log during branch_graph");
}

#[test]
fn inherited_git_trace_variables_do_not_write_during_branch_graph() {
    let fixture = Fixture::with_tracked_commit();
    for var in ["GIT_TRACE", "GIT_TRACE2", "GIT_TRACE2_EVENT"] {
        assert_git_honors_trace_var(&fixture.git, var, fixture.tmp.path());
    }
    let oid = fixture.git(&["rev-parse", "HEAD"]);
    let trace = fixture.marker("graph.git.trace");
    let trace2 = fixture.marker("graph.git.trace2");
    let trace2_event = fixture.marker("graph.git.trace2.event");
    let extra = [
        ("GIT_TRACE", trace.display().to_string()),
        ("GIT_TRACE2", trace2.display().to_string()),
        ("GIT_TRACE2_EVENT", trace2_event.display().to_string()),
        (CHILD_TIP, oid),
    ];
    assert_child_opens(&ChildCheck {
        target: &fixture.root,
        branch: "main",
        action: "graph",
        // The graph action reads this as the commit summary, not the full log text.
        history: Some("first"),
        marker: Some(&trace),
        required_var: "GIT_TRACE",
        config_key: None,
        config_value: None,
        extra_env: &extra,
    });
    assert_did_not_run(&trace2, "GIT_TRACE2 during branch_graph");
    assert_did_not_run(&trace2_event, "GIT_TRACE2_EVENT during branch_graph");
}

#[test]
fn slow_branch_graph_log_times_out_and_returns() {
    let tmp = TempDir::with_prefix("cthulhu-adv-a-slow-log-").expect("temp dir");
    let fake = link_fake(tmp.path(), "git");
    write_mode(&fake, "slow-log");
    let git = probe_fake(&fake);
    let workdir = tmp.path().join("workdir");
    fs::create_dir(&workdir).expect("workdir");
    let sleeper = sidecar(&fake, ".sleeper");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let tip = graph_tip("main", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let _ = tx.send(branch_graph(&git, &workdir, &[tip], GRAPH_LOG_LIMIT));
    });
    // `tmp` stays here so the sleeper pid file is not deleted before the assert.
    // The fake sleeps 120s. The command timeout is 60s. Do not join the worker.
    let started = Instant::now();
    match rx.recv_timeout(Duration::from_secs(90)) {
        Ok(Err(GitError::TimedOut { command, limit })) => {
            assert_history_log_command(&command);
            assert_eq!(limit, Duration::from_secs(60));
            assert!(
                started.elapsed() >= Duration::from_secs(50),
                "history log returned in {:?}, before the command timeout",
                started.elapsed()
            );
            assert_recorded_pid_is_dead(&sleeper);
        }
        Ok(Ok(graph)) => panic!(
            "a log that sleeps past the command timeout returned a graph ({} rows)",
            graph.rows.len()
        ),
        Ok(Err(error)) => panic!(
            "a log that sleeps past the command timeout must return TimedOut; got {}",
            brief_error(&error)
        ),
        Err(mpsc::RecvTimeoutError::Timeout) => panic!(
            "branch_graph still running after 90s; a slow history log must hit the command timeout and return"
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("branch_graph thread exited without a result");
        }
    }
    drop(tmp);
}

#[test]
fn branch_graph_log_output_bomb_is_capped() {
    let tmp = TempDir::with_prefix("cthulhu-adv-a-log-bomb-").expect("temp dir");
    let fake = link_fake(tmp.path(), "git");
    write_mode(&fake, "bomb-log");
    let git = probe_fake(&fake);
    let workdir = tmp.path().join("workdir");
    fs::create_dir(&workdir).expect("workdir");
    let producer = sidecar(&fake, ".producer");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let tip = graph_tip("main", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let _ = tx.send(branch_graph(&git, &workdir, &[tip], GRAPH_LOG_LIMIT));
    });
    // `tmp` stays here so the producer pid file is not deleted before the assert.
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Err(GitError::OutputLimit { command })) => {
            assert_history_log_command(&command);
            assert_recorded_pid_is_dead(&producer);
        }
        Ok(Ok(graph)) => panic!(
            "branch_graph accepted unbounded log stdout ({} rows)",
            graph.rows.len()
        ),
        Ok(Err(error)) => panic!(
            "log stdout over 8 MiB must be refused with a typed output-limit error; got {}",
            brief_error(&error)
        ),
        Err(mpsc::RecvTimeoutError::Timeout) => panic!(
            "branch_graph still running after 5s while the history log wrote past the output cap"
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("branch_graph thread exited without a result");
        }
    }
    drop(tmp);
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
        GitError::Command {
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
        let found = discover_with(&inputs).map(|git| git.path);
        let _ = tx.send(found);
    });
    // `tmp` stays here so the sleeper pid file is not deleted before the assert.

    match rx.recv_timeout(Duration::from_secs(1)) {
        Ok(Ok(path)) => {
            assert_eq!(
                path, fast_path,
                "discover_with should use the later git that answers --version promptly"
            );
            assert_recorded_pid_is_dead(&sidecar(&slow, ".sleeper"));
        }
        Ok(Err(error)) => panic!("discover_with failed: {error}"),
        Err(mpsc::RecvTimeoutError::Timeout) => panic!(
            "discover_with still running after 1s while probing a git --version that sleeps; a later candidate is good"
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("discover thread exited without a result");
        }
    }
    drop(tmp);
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
        Err(GitError::OutputLimit { .. }) => {
            assert_recorded_pid_is_dead(&sidecar(&fake, ".producer"));
        }
        Err(error) => panic!(
            "32MiB rev-parse stdout must be refused with a typed output-limit error before it is used as a path; got {}",
            brief_error(&error)
        ),
    }
}

#[test]
fn background_writer_does_not_pin_a_finished_git() {
    let tmp = TempDir::with_prefix("cthulhu-adv-a-writer-").expect("temp dir");
    let fake = link_fake(tmp.path(), "git");
    write_mode(&fake, "detach-writer");
    let git = probe_fake(&fake);
    let writer = sidecar(&fake, ".writer");
    let (tx, rx) = mpsc::channel();
    let cwd = tmp.path().to_path_buf();
    thread::spawn(move || {
        let _ = tx.send(git.run(&cwd, &["status"]).map(|_| ()));
    });
    // `tmp` stays here so the grandchild pid file is not deleted before the assert.
    match rx.recv_timeout(Duration::from_secs(2)) {
        Ok(result) => {
            result.expect("a git that already exited must return, not wait on its grandchild");
        }
        Err(mpsc::RecvTimeoutError::Timeout) => panic!(
            "git exited 0 while a grandchild held stdout, and the caller was still blocked after 2s"
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("git thread ended without a result");
        }
    }
    assert_recorded_pid_is_dead(&writer);
    drop(tmp);
}

#[test]
fn inherited_git_exec_path_cannot_run_remote_helper_on_fetch() {
    let fixture = Fixture::with_tracked_commit();
    fixture.git(&["remote", "add", "origin", "https://127.0.0.1:1/cthulhu.git"]);
    let marker = fixture.marker("remote-helper");
    let exec_dir = fixture.tmp.path().join("exec-path");
    write_executable(
        &exec_dir.join("git-remote-https"),
        &format!("#!/bin/sh\ntouch {}\n", marker.display()),
    );
    let extra = [("GIT_EXEC_PATH", exec_dir.display().to_string())];
    assert_child_opens(&ChildCheck {
        target: &fixture.root,
        branch: "main",
        action: "fetch",
        history: None,
        marker: Some(&marker),
        required_var: "GIT_EXEC_PATH",
        config_key: None,
        config_value: None,
        extra_env: &extra,
    });
}

#[test]
fn inherited_git_config_count_cannot_set_ssh_command_on_fetch() {
    let fixture = Fixture::with_tracked_commit();
    fixture.git(&["remote", "add", "origin", "ssh://127.0.0.1:1/cthulhu.git"]);
    let marker = fixture.marker("ssh-command");
    let poke = poke_script(fixture.tmp.path(), "ssh-command.sh", &marker);
    let command = poke.display().to_string();
    let pairs = [("core.sshCommand", command.as_str())];
    let extra = config_count(&pairs);
    assert_child_opens(&ChildCheck {
        target: &fixture.root,
        branch: "main",
        action: "fetch",
        history: None,
        marker: Some(&marker),
        required_var: "GIT_CONFIG_COUNT",
        config_key: Some("core.sshCommand"),
        config_value: Some(&command),
        extra_env: &extra,
    });
}

fn set_local_config(scratch: &Path, root: &Path, git: &Git, key: &str, value: &str) {
    repo_git(git, scratch, root, &["config", "--local", key, value]);
}

#[test]
fn pull_ff_only_does_not_rebase_when_pull_rebase_is_true() {
    let setup = diverged_pull_setup(|scratch, root, git| {
        set_local_config(scratch, root, git, "pull.rebase", "true");
    });
    assert_pull_ff_only_rejects_rebase(&setup);
}

#[test]
fn pull_ff_only_does_not_rebase_when_pull_rebase_is_merges() {
    let setup = diverged_pull_setup(|scratch, root, git| {
        set_local_config(scratch, root, git, "pull.rebase", "merges");
    });
    assert_pull_ff_only_rejects_rebase(&setup);
}

#[test]
fn pull_ff_only_does_not_rebase_when_pull_rebase_is_m() {
    let setup = diverged_pull_setup(|scratch, root, git| {
        set_local_config(scratch, root, git, "pull.rebase", "m");
    });
    assert_pull_ff_only_rejects_rebase(&setup);
}

#[test]
fn pull_ff_only_does_not_rebase_when_pull_rebase_is_interactive() {
    let setup = diverged_pull_setup(|scratch, root, git| {
        set_local_config(scratch, root, git, "pull.rebase", "interactive");
    });
    assert_pull_ff_only_rejects_rebase(&setup);
}

#[test]
fn pull_ff_only_does_not_rebase_when_pull_rebase_is_i() {
    let setup = diverged_pull_setup(|scratch, root, git| {
        set_local_config(scratch, root, git, "pull.rebase", "i");
    });
    assert_pull_ff_only_rejects_rebase(&setup);
}

#[test]
fn pull_ff_only_does_not_rebase_when_pull_rebase_is_preserve() {
    let setup = diverged_pull_setup(|scratch, root, git| {
        set_local_config(scratch, root, git, "pull.rebase", "preserve");
    });
    assert_pull_ff_only_rejects_rebase(&setup);
}

#[test]
fn pull_ff_only_does_not_rebase_when_branch_main_rebase_is_interactive() {
    let setup = diverged_pull_setup(|scratch, root, git| {
        set_local_config(scratch, root, git, "branch.main.rebase", "interactive");
    });
    assert_pull_ff_only_rejects_rebase(&setup);
}

#[test]
fn pull_ff_only_fast_forwards_when_pull_rebase_is_false_and_behind_upstream() {
    let origin = Fixture::init();
    origin.write("tracked.txt", "base\n");
    origin.git(&["add", "tracked.txt"]);
    origin.git(&["commit", "-m", "base"]);

    let local_tmp = TempDir::with_prefix("cthulhu-adv-pull-ff-").expect("local temp");
    let local_root = local_tmp.path().join("clone");
    let origin_path = fs::canonicalize(&origin.root).expect("origin path");
    repo_git(
        &origin.git,
        origin.tmp.path(),
        origin.tmp.path(),
        &[
            "clone",
            "--",
            origin_path.to_str().expect("utf8"),
            local_root.to_str().expect("utf8"),
        ],
    );

    origin.write("tracked.txt", "origin ahead\n");
    origin.git(&["add", "tracked.txt"]);
    origin.git(&["commit", "-m", "origin ahead"]);

    set_local_config(
        local_tmp.path(),
        &local_root,
        &origin.git,
        "pull.rebase",
        "false",
    );

    let upstream_tip = origin.git(&["rev-parse", "main"]);

    pull_ff_only(&origin.git, &local_root).expect("fast-forward pull");
    let head = repo_git(
        &origin.git,
        local_tmp.path(),
        &local_root,
        &["rev-parse", "HEAD"],
    );
    let upstream = repo_git(
        &origin.git,
        local_tmp.path(),
        &local_root,
        &["rev-parse", "origin/main"],
    );
    assert_eq!(head, upstream_tip);
    assert_eq!(head, upstream);
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
    let action = env::var(CHILD_ACTION).expect("action");
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

    if action == "fetch" {
        // Network failure is fine. The marker is the assertion that matters.
        let _ = fetch(&git, Path::new(&target), "origin/main");
        if let Ok(marker) = env::var(CHILD_MARKER) {
            assert!(
                !Path::new(&marker).exists(),
                "hostile inherited environment executed a command or wrote {marker}"
            );
        }
        let sentinel = env::var(CHILD_SENTINEL).expect("sentinel path");
        fs::write(sentinel, "checked\n").expect("write sentinel");
        return;
    }

    if action == "graph" {
        let oid = env::var(CHILD_TIP).expect("parent sets the tip oid");
        let branch = env::var(CHILD_BRANCH).expect("expected branch");
        // `CHILD_HISTORY` is the commit summary for this action.
        let expected = env::var(CHILD_HISTORY).expect("expected summary");
        let loaded = branch_graph(
            &git,
            Path::new(&target),
            &[graph_tip(&branch, oid)],
            GRAPH_LOG_LIMIT,
        );
        for var in ["GIT_TRACE", "GIT_TRACE2", "GIT_TRACE2_EVENT"] {
            let path = env::var(var).unwrap_or_else(|_| panic!("parent did not export {var}"));
            assert!(!path.is_empty(), "parent exported an empty {var}");
            assert!(
                !Path::new(&path).exists(),
                "hostile {var} wrote {path} during branch_graph"
            );
        }
        let graph = loaded.unwrap_or_else(|error| {
            panic!("branch_graph failed under the hostile environment: {error}")
        });
        assert_eq!(
            graph
                .rows
                .iter()
                .map(|row| row.summary.as_str())
                .collect::<Vec<_>>(),
            [expected.as_str()],
            "branch_graph did not return the real commit"
        );
        assert!(
            !graph.truncated,
            "branch_graph truncated a one-commit history"
        );
    } else {
        let info = inspect(&git, Path::new(&target)).expect("inspect target");
        assert_eq!(
            info.root,
            PathBuf::from(env::var(CHILD_ROOT).expect("expected root"))
        );
        assert_eq!(
            info.head,
            Head::Branch(env::var(CHILD_BRANCH).expect("expected branch"))
        );

        if action == "history" {
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

fn recorded_argv(path: &Path) -> Vec<String> {
    let recorded = fs::read(path).unwrap_or_else(|error| {
        panic!("missing argv log {}: {error}", path.display());
    });
    recorded
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8(part.to_vec()).expect("argv is utf-8"))
        .collect()
}

fn point_argv_log(fake: &Path, log: &Path) {
    fs::write(sidecar(fake, ".log"), log.display().to_string()).expect("argv sidecar");
}

/// `Git::run_for` puts these in front of every command, including switch.
const SWITCH_EXEC_PREFIX: &[&str] = &[
    "--no-optional-locks",
    "--no-pager",
    "--no-replace-objects",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "log.showSignature=false",
];

fn assert_switch_command_argv(args: &[String], name: &str) {
    assert!(
        args.len() >= SWITCH_EXEC_PREFIX.len() + 4,
        "{name} argv is shorter than the exec prefix plus switch: {args:?}"
    );
    let boundary = args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or_else(|| panic!("{name} was not one argument after --: {args:?}"));
    assert!(
        args[..boundary].iter().all(|arg| arg != name),
        "{name} appeared before -- as a bare flag: {args:?}"
    );
    assert!(
        args.iter().all(|arg| arg != "--all" && arg != "--prune"),
        "{name} argv includes --all or --prune: {args:?}"
    );
    let head: Vec<&str> = args[..SWITCH_EXEC_PREFIX.len()]
        .iter()
        .map(String::as_str)
        .collect();
    assert_eq!(head, SWITCH_EXEC_PREFIX, "{name} exec prefixes: {args:?}");
    let tail: Vec<&str> = args[args.len() - 4..].iter().map(String::as_str).collect();
    assert_eq!(tail, ["switch", "--no-guess", "--", name], "{name}");
    assert_eq!(
        args.len(),
        SWITCH_EXEC_PREFIX.len() + 4,
        "{name} argv has extra elements: {args:?}"
    );
}

fn assert_attached_to(fixture: &Fixture, name: &str) {
    switch_branch(&fixture.git, &fixture.root, name).unwrap_or_else(|error| {
        panic!("switch to {name} failed: {error}");
    });
    let info = inspect(&fixture.git, &fixture.root).unwrap_or_else(|error| {
        panic!("inspect after switching to {name} failed: {error}");
    });
    assert_eq!(
        info.head,
        Head::Branch(name.to_owned()),
        "detached HEAD after switching to {name}"
    );
    assert_eq!(
        fixture.git(&["symbolic-ref", "--quiet", "HEAD"]),
        format!("refs/heads/{name}")
    );
}

fn remove_artifact(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("remove {}: {error}", path.display()),
    }
}

fn write_passthrough_filter(path: &Path, marker: &Path, label: &str) {
    let body = format!(
        "#!/bin/sh\ncat\nprintf '%s\\n' '{}' >> '{}'\n",
        label,
        marker.display()
    );
    write_executable(path, &body);
}

fn write_post_checkout(dir: &Path, log: &Path, marker: &Path) {
    fs::create_dir_all(dir).expect("hooks directory");
    let body = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ntouch '{}'\n",
        log.display(),
        marker.display()
    );
    write_executable(&dir.join("post-checkout"), &body);
}

fn assert_filter_appended(path: &Path, label: &str) {
    let text = fs::read_to_string(path).unwrap_or_else(|error| {
        panic!("{label} filter did not append a marker: {error}");
    });
    assert!(
        text.lines().any(|line| line == label),
        "{label} filter marker was {text:?}"
    );
}

fn assert_post_checkout_was_a_branch_switch(log: &Path, marker: &Path) {
    assert!(
        marker.is_file(),
        "post-checkout did not run (marker {} is absent)",
        marker.display()
    );
    let text = fs::read_to_string(log)
        .unwrap_or_else(|error| panic!("post-checkout did not record $*: {error}"));
    let lines: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
    assert!(
        !lines.is_empty(),
        "post-checkout touched a marker but recorded no arguments"
    );
    for line in lines {
        let fields: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(
            fields.get(2).copied(),
            Some("1"),
            "post-checkout third field must be 1, recorded {line:?}"
        );
    }
}

fn ref_names(fixture: &Fixture, pattern: &str) -> String {
    fixture.git(&["for-each-ref", "--format=%(refname)", pattern])
}

#[test]
fn checkout_option_shaped_names_are_one_ref_argument() {
    let tmp = TempDir::with_prefix("cthulhu-adv-a-switch-argv-").expect("temp dir");
    let fake = link_fake(tmp.path(), "git");
    write_mode(&fake, "record-command");
    let argv_log = tmp.path().join("argv.log");
    point_argv_log(&fake, &argv_log);
    let git = probe_fake(&fake);
    let workdir = tmp.path().join("workdir");
    fs::create_dir(&workdir).expect("workdir");

    for name in [
        "-switch",
        "--upload-pack=evil",
        "--force",
        "-f",
        "--detach",
        "--orphan",
        "--guess",
        "-B",
    ] {
        remove_artifact(&argv_log);
        switch_branch(&git, &workdir, name).unwrap_or_else(|error| {
            panic!("switch_branch {name} failed: {}", brief_error(&error));
        });
        assert!(
            argv_log.is_file(),
            "{name} did not spawn git; argv log was not written"
        );
        assert_switch_command_argv(&recorded_argv(&argv_log), name);
    }
}

#[test]
fn checkout_keeps_head_attached_to_the_named_branch() {
    let fixture = Fixture::with_two_commits();
    fixture.git(&["branch", "other", "HEAD~1"]);
    let parent = fixture.git(&["rev-parse", "other"]);
    assert_attached_to(&fixture, "other");

    fixture.git(&["update-ref", "refs/heads/-switch", &parent]);
    assert_attached_to(&fixture, "-switch");

    fixture.git(&["update-ref", "refs/heads/--upload-pack=evil", &parent]);
    assert_attached_to(&fixture, "--upload-pack=evil");
}

#[test]
fn checkout_rejects_dotdot_newline_space_and_ref_escape_before_git_runs() {
    let tmp = TempDir::with_prefix("cthulhu-adv-a-switch-reject-").expect("temp dir");
    let fake = link_fake(tmp.path(), "git");
    write_mode(&fake, "record-command");
    let git = probe_fake(&fake);
    let workdir = tmp.path().join("workdir");
    fs::create_dir(&workdir).expect("workdir");

    for (index, name) in ["", " ", "ma\nin", "..", "../../.git", "foo/../../.git"]
        .into_iter()
        .enumerate()
    {
        let argv_log = tmp.path().join(format!("argv-{index}.log"));
        point_argv_log(&fake, &argv_log);
        let result = switch_branch(&git, &workdir, name);
        assert!(
            matches!(result, Err(GitError::Rejected { .. })),
            "{name:?} must be rejected before git runs, got {}",
            match &result {
                Ok(()) => "Ok(())".to_owned(),
                Err(error) => brief_error(error),
            }
        );
        assert!(
            !argv_log.exists(),
            "{name:?} spawned git; argv log was created at {}",
            argv_log.display()
        );
    }
}

#[test]
fn checkout_does_not_force_overwrite_of_a_dirty_worktree() {
    let fixture = Fixture::with_two_commits();
    fixture.git(&["branch", "other", "HEAD~1"]);
    fixture.write("tracked.txt", "DIRTY\n");
    let marker = fixture.marker("dirty-post-checkout");
    poke_script(&fixture.root.join(".git/hooks"), "post-checkout", &marker);

    let result = switch_branch(&fixture.git, &fixture.root, "other");
    let Err(error) = result else {
        panic!("dirty worktree was checked out");
    };
    match &error {
        GitError::Command {
            command, stderr, ..
        } => {
            assert!(
                stderr.contains("would be overwritten"),
                "stderr lost the overwrite refusal: {stderr}"
            );
            assert!(
                !command.contains("--force"),
                "switch forced the checkout: {command}"
            );
        }
        other => panic!(
            "dirty switch must be a git command failure, got {}",
            brief_error(other)
        ),
    }
    assert_eq!(
        fs::read(fixture.root.join("tracked.txt")).expect("tracked.txt"),
        b"DIRTY\n"
    );
    let info = inspect(&fixture.git, &fixture.root).unwrap_or_else(|error| {
        panic!("inspect after a refused switch failed: {error}");
    });
    assert_eq!(info.head, Head::Branch("main".to_owned()));
    assert_did_not_run(&marker, "post-checkout ran after a refused switch");
}

#[test]
fn repo_alias_checkout_is_not_executed() {
    let fixture = Fixture::with_two_commits();
    let target = fixture.git(&["rev-parse", "HEAD~1"]);
    fixture.git(&["branch", "other", "HEAD~1"]);
    let marker = fixture.marker("alias-checkout");
    let poke = poke_script(fixture.tmp.path(), "alias-checkout.sh", &marker);
    fixture.git(&[
        "config",
        "--local",
        "alias.checkout",
        &format!("!{}", poke.display()),
    ]);

    switch_branch(&fixture.git, &fixture.root, "other").unwrap_or_else(|error| {
        panic!("switch to other failed: {error}");
    });
    assert_did_not_run(&marker, "alias.checkout");
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), target);
}

#[test]
fn include_path_alias_checkout_is_not_executed() {
    let fixture = Fixture::with_two_commits();
    let target = fixture.git(&["rev-parse", "HEAD~1"]);
    fixture.git(&["branch", "other", "HEAD~1"]);
    let marker = fixture.marker("include-alias-checkout");
    let poke = poke_script(fixture.tmp.path(), "include-alias-checkout.sh", &marker);
    let included = fixture.tmp.path().join("alias.gitconfig");
    fs::write(
        &included,
        format!("[alias]\n\tcheckout = !{}\n", poke.display()),
    )
    .expect("included config");
    fixture.git(&[
        "config",
        "--local",
        "include.path",
        &included.display().to_string(),
    ]);

    switch_branch(&fixture.git, &fixture.root, "other").unwrap_or_else(|error| {
        panic!("switch to other failed: {error}");
    });
    assert_did_not_run(&marker, "include.path alias.checkout");
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), target);
}

#[test]
fn include_path_checkout_default_remote_does_not_create_a_branch() {
    let local = Fixture::init();
    local.write("tracked.txt", "local\n");
    local.git(&["add", "tracked.txt"]);
    local.git(&["commit", "-m", "local"]);
    let main_oid = local.git(&["rev-parse", "HEAD"]);

    let origin = Fixture::init();
    origin.write("tracked.txt", "origin shared\n");
    origin.git(&["add", "tracked.txt"]);
    origin.git(&["commit", "-m", "origin"]);
    origin.git(&["branch", "shared"]);

    let evil = Fixture::init();
    evil.write("tracked.txt", "evil shared\n");
    evil.git(&["add", "tracked.txt"]);
    evil.git(&["commit", "-m", "evil"]);
    evil.git(&["branch", "shared"]);

    let origin_tip = origin.git(&["rev-parse", "refs/heads/shared"]);
    let evil_tip = evil.git(&["rev-parse", "refs/heads/shared"]);
    assert_ne!(
        origin_tip, evil_tip,
        "origin and evil must publish different shared commits"
    );

    let origin_path = fs::canonicalize(&origin.root).expect("origin path");
    let evil_path = fs::canonicalize(&evil.root).expect("evil path");
    local.git(&[
        "remote",
        "add",
        "origin",
        origin_path.to_str().expect("utf8 origin"),
    ]);
    local.git(&[
        "remote",
        "add",
        "evil",
        evil_path.to_str().expect("utf8 evil"),
    ]);
    local.git(&["fetch", "origin"]);
    local.git(&["fetch", "evil"]);
    assert_eq!(
        local.git(&["rev-parse", "refs/remotes/origin/shared"]),
        origin_tip
    );
    assert_eq!(
        local.git(&["rev-parse", "refs/remotes/evil/shared"]),
        evil_tip
    );
    assert!(
        ref_names(&local, "refs/heads/shared").is_empty(),
        "fixture already had a local shared branch"
    );

    let included = local.tmp.path().join("checkout.gitconfig");
    fs::write(
        &included,
        "[checkout]\n\tguess = true\n\tdefaultRemote = evil\n",
    )
    .expect("included checkout config");
    local.git(&[
        "config",
        "--local",
        "include.path",
        &included.display().to_string(),
    ]);
    assert_eq!(
        local.git(&["config", "--get", "checkout.defaultRemote"]),
        "evil"
    );
    assert_eq!(
        local.git(&["config", "--bool", "--get", "checkout.guess"]),
        "true"
    );

    let result = switch_branch(&local.git, &local.root, "shared");
    assert!(
        result.is_err(),
        "switch to a remote-only branch must fail, got {result:?}"
    );
    assert!(
        ref_names(&local, "refs/heads/shared").is_empty(),
        "switch created refs/heads/shared from checkout.defaultRemote"
    );
    let info = inspect(&local.git, &local.root).unwrap_or_else(|error| {
        panic!("inspect after refusing shared failed: {error}");
    });
    assert_eq!(info.head, Head::Branch("main".to_owned()));
    assert_eq!(local.git(&["rev-parse", "HEAD"]), main_oid);
}

#[test]
fn checkout_runs_post_checkout_and_smudge_like_the_terminal() {
    // Terminal `git switch` runs post-checkout with third field 1. Leaving a
    // commit that assigns filter=evil runs clean; coming back runs smudge.
    let fixture = Fixture::with_two_commits();
    let clean_marker = fixture.marker("clean-filter");
    let smudge_marker = fixture.marker("smudge-filter");
    let hook_log = fixture.marker("post-checkout-args");
    let hook_marker = fixture.marker("post-checkout");
    write_passthrough_filter(&fixture.tmp.path().join("clean.sh"), &clean_marker, "clean");
    write_passthrough_filter(
        &fixture.tmp.path().join("smudge.sh"),
        &smudge_marker,
        "smudge",
    );
    let hooks = fixture.tmp.path().join("checkout-hooks");
    write_post_checkout(&hooks, &hook_log, &hook_marker);
    fixture.git(&[
        "config",
        "--local",
        "filter.evil.clean",
        &fixture.tmp.path().join("clean.sh").display().to_string(),
    ]);
    fixture.git(&[
        "config",
        "--local",
        "filter.evil.smudge",
        &fixture.tmp.path().join("smudge.sh").display().to_string(),
    ]);
    fixture.git(&[
        "config",
        "--local",
        "core.hooksPath",
        &hooks.display().to_string(),
    ]);
    fixture.write(".gitattributes", "* filter=evil\n");
    fixture.git(&["add", ".gitattributes"]);
    fixture.git(&["commit", "-m", "attributes"]);
    fixture.git(&["branch", "other", "HEAD~1"]);

    for path in [&clean_marker, &smudge_marker, &hook_log, &hook_marker] {
        remove_artifact(path);
    }
    switch_branch(&fixture.git, &fixture.root, "other").unwrap_or_else(|error| {
        panic!("switch to other failed: {error}");
    });
    assert_post_checkout_was_a_branch_switch(&hook_log, &hook_marker);
    assert_filter_appended(&clean_marker, "clean");
    assert_did_not_run(&smudge_marker, "smudge while leaving the filter commit");
    assert_eq!(
        fs::read(fixture.root.join("tracked.txt")).expect("tracked.txt"),
        b"two\n"
    );
    assert!(!fixture.root.join(".gitattributes").exists());

    for path in [&clean_marker, &smudge_marker, &hook_log, &hook_marker] {
        remove_artifact(path);
    }
    switch_branch(&fixture.git, &fixture.root, "main").unwrap_or_else(|error| {
        panic!("switch back to main failed: {error}");
    });
    assert_post_checkout_was_a_branch_switch(&hook_log, &hook_marker);
    assert_filter_appended(&smudge_marker, "smudge");
    assert_did_not_run(&clean_marker, "clean while returning to the filter commit");
    assert_eq!(
        fs::read(fixture.root.join("tracked.txt")).expect("tracked.txt"),
        b"two\n"
    );
    assert_eq!(
        fs::read(fixture.root.join(".gitattributes")).expect(".gitattributes"),
        b"* filter=evil\n"
    );
}

#[test]
fn checkout_does_not_run_core_fsmonitor() {
    let fixture = Fixture::with_two_commits();
    fixture.git(&["branch", "other", "HEAD~1"]);
    let marker = fixture.marker("fsmonitor-checkout");
    let poke = poke_script(fixture.tmp.path(), "fsmonitor-checkout.sh", &marker);
    fixture.git(&[
        "config",
        "--local",
        "core.fsmonitor",
        &poke.display().to_string(),
    ]);

    switch_branch(&fixture.git, &fixture.root, "other").unwrap_or_else(|error| {
        panic!("switch failed: {error}");
    });
    assert_did_not_run(&marker, "core.fsmonitor");
}

#[test]
fn slow_checkout_times_out_at_sixty_seconds() {
    // The fake sleeps for 120s. switch_branch must return at its 60s limit and
    // kill that sleeper. Do not join the worker: a git that ignores the limit
    // would pin this test on the join instead of the 90s receive.
    let tmp = TempDir::with_prefix("cthulhu-adv-a-slow-switch-").expect("temp dir");
    let fake = link_fake(tmp.path(), "git");
    write_mode(&fake, "slow-command");
    let sleeper = sidecar(&fake, ".sleeper");
    let git = probe_fake(&fake);
    let workdir = tmp.path().join("workdir");
    fs::create_dir(&workdir).expect("workdir");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(switch_branch(&git, &workdir, "main"));
    });

    match rx.recv_timeout(Duration::from_secs(90)) {
        Ok(Err(GitError::TimedOut { limit, .. })) => {
            assert_eq!(limit, Duration::from_secs(60));
            assert_recorded_pid_is_dead(&sleeper);
        }
        Ok(Ok(())) => panic!("slow switch_branch returned successfully"),
        Ok(Err(error)) => panic!(
            "slow switch_branch must time out at 60s, got {}",
            brief_error(&error)
        ),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("switch_branch still running after 90s; the command limit is 60 seconds")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("switch thread exited without a result");
        }
    }
    drop(tmp);
}
