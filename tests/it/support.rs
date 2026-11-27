//! Fixtures shared by the integration tests: isolated git commands, a
//! repository in a temp dir, the one fake git, and process checks.
//!
//! Nothing here writes the process environment or the working directory. Git
//! configuration is applied only to the [`Command`] passed to [`isolate`].

#![allow(
    dead_code,
    reason = "src/ui/adversarial_tests.rs includes this file for `isolate` only"
)]

use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use cthulhu_git::git::{
    Git, GitError, GitVersion, GraphCandidate, Head, HistoryGraph, RepoInfo, branch_graph,
    branches, graph_tips, head_commit_oid, inspect,
};
use tempfile::TempDir;

/// Inherited `GIT_*` variables a child git must still see. Everything else
/// under that prefix is stripped so the runner's environment cannot redirect
/// hooks, config, or the exec path.
const KEPT_GIT_VARS: &[&str] = &[
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_SSH_VARIANT",
    "GIT_ASKPASS",
];

/// Set by the fake git in `setsid` mode. The value is the pid file.
pub const SETSID_PIDFILE_VAR: &str = "CTHULHU_HARNESS_SETSID_PIDFILE";

const SETSID_SLEEP: Duration = Duration::from_secs(30);

/// Points `command` at empty config files under `scratch` and drops inherited
/// `GIT_*` variables other than [`KEPT_GIT_VARS`].
///
/// Also sets `LC_ALL=C`, disables commit signing, fsmonitor, credential
/// helpers, and autocrlf, and points `core.hooksPath` at an empty directory
/// in `scratch`. Author and committer name and email are set so a later
/// commit does not read `~/.gitconfig`.
///
/// Call this before adding the git subcommand. Extra `GIT_*` variables, such
/// as `GIT_AUTHOR_DATE`, must be set after this function: it clears inherited
/// ones first.
pub fn isolate(command: &mut Command, scratch: &Path) {
    let global = scratch.join("empty.gitconfig");
    let system = scratch.join("empty.system.gitconfig");
    let hooks = scratch.join("empty-hooks");
    fs::create_dir_all(&hooks)
        .unwrap_or_else(|error| panic!("hooks dir {}: {error}", hooks.display()));
    fs::write(&global, "").unwrap_or_else(|error| panic!("global gitconfig: {error}"));
    fs::write(&system, "").unwrap_or_else(|error| panic!("system gitconfig: {error}"));

    let inherited: Vec<_> = std::env::vars_os()
        .map(|(key, _)| key)
        .filter(|key| {
            key.to_str()
                .is_some_and(|name| name.starts_with("GIT_") && !KEPT_GIT_VARS.contains(&name))
        })
        .collect();
    for key in inherited {
        command.env_remove(key);
    }

    let hooks_arg = format!("core.hooksPath={}", hooks.display());
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &global)
        .env("GIT_CONFIG_SYSTEM", &system)
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "cthulhu")
        .env("GIT_AUTHOR_EMAIL", "cthulhu@example.invalid")
        .env("GIT_COMMITTER_NAME", "cthulhu")
        .env("GIT_COMMITTER_EMAIL", "cthulhu@example.invalid")
        .args([
            "-c",
            "commit.gpgSign=false",
            "-c",
            "core.fsmonitor=",
            "-c",
            "credential.helper=",
            "-c",
            "core.autocrlf=false",
            "-c",
        ])
        .arg(hooks_arg);
}

/// The system git, found once per test process.
pub fn system_git() -> Git {
    static GIT: OnceLock<Git> = OnceLock::new();
    GIT.get_or_init(|| Git::discover().expect("system git"))
        .clone()
}

/// A repository directory in its own temp dir, driven by the system git.
pub struct Repo {
    pub tmp: TempDir,
    pub root: PathBuf,
    pub git: Git,
}

impl Repo {
    /// An empty directory `name`, not a repository yet.
    pub fn new(name: impl AsRef<OsStr>) -> Self {
        let tmp = TempDir::with_prefix("cthulhu-it-").expect("temp dir");
        let root = tmp.path().join(name.as_ref());
        fs::create_dir(&root).expect("repo dir");
        Self {
            tmp,
            root,
            git: system_git(),
        }
    }

    /// `git init -b main`. The app's git reads `~/.gitconfig`, so the local
    /// config pins what a user's file could change under a test.
    pub fn init(name: impl AsRef<OsStr>) -> Self {
        Self::new(name).initialized(&[])
    }

    /// `git init -b main` with `extra` options, such as `--bare`.
    pub fn initialized(self, extra: &[&str]) -> Self {
        self.git(&[&["init", "-b", "main"], extra].concat());
        self.git(&["config", "--local", "core.autocrlf", "false"]);
        self.git(&["config", "--local", "commit.gpgsign", "false"]);
        self
    }

    /// A git command in `cwd` with [`isolate`] applied.
    pub fn command(&self, cwd: &Path) -> Command {
        let mut command = Command::new(&self.git.path);
        command.current_dir(cwd).stdin(Stdio::null());
        isolate(&mut command, self.tmp.path());
        command
    }

    /// Trimmed stdout of a git command in `cwd` that must succeed.
    pub fn git_in(&self, cwd: &Path, args: &[&str]) -> String {
        let output = self.command(cwd).args(args).output().expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    pub fn git(&self, args: &[&str]) -> String {
        self.git_in(&self.root, args)
    }

    pub fn commit(&self, message: &str) {
        self.git(&["commit", "--allow-empty", "-m", message]);
    }

    /// A git command at a fixed author and committer time, so `--date-order`
    /// is fixed.
    pub fn git_dated(&self, unix: i64, args: &[&str]) -> String {
        let date = format!("{unix} +0000");
        let output = self
            .command(&self.root)
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    pub fn commit_at(&self, message: &str, unix: i64) {
        self.git_dated(unix, &["commit", "--allow-empty", "-m", message]);
    }

    /// Writes `object` with `hash-object --literally`, which keeps bytes
    /// `git commit` refuses, such as a NUL in the message.
    pub fn hash_commit(&self, object: &[u8]) -> String {
        let mut child = self
            .command(&self.root)
            .args([
                "hash-object",
                "--literally",
                "-t",
                "commit",
                "-w",
                "--stdin",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("hash-object");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(object)
            .expect("write commit object");
        let output = child.wait_with_output().expect("hash-object output");
        assert!(output.status.success(), "hash-object failed");
        String::from_utf8(output.stdout)
            .expect("utf8")
            .trim()
            .to_owned()
    }

    pub fn write(&self, relative: &str, contents: &str) {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().expect("parent")).expect("parent dir");
        fs::write(path, contents).expect("write");
    }

    pub fn canonical(&self) -> PathBuf {
        dunce::canonicalize(&self.root).expect("canonical root")
    }

    pub fn info(&self) -> RepoInfo {
        inspect(&self.git, &self.root).expect("inspect")
    }

    /// The graph the window draws when every branch is checked.
    pub fn graph(&self, limit: usize) -> Result<HistoryGraph, GitError> {
        graph(&self.git, &self.info(), limit)
    }
}

/// The window's graph read: every listed branch checked, plus detached `HEAD`.
pub fn graph(git: &Git, info: &RepoInfo, limit: usize) -> Result<HistoryGraph, GitError> {
    let listed = branches(git, &info.root, &info.head)?;
    let detached = match info.head {
        Head::Detached { .. } => Some(head_commit_oid(git, &info.root)?),
        Head::Branch(_) | Head::Unborn(_) => None,
    };
    let candidates: Vec<GraphCandidate<'_>> = listed
        .iter()
        .map(|branch| GraphCandidate {
            name: &branch.name,
            oid: branch.oid.as_deref(),
            current: branch.current,
            committer_time: branch.committer_time,
            checked: true,
        })
        .collect();
    let tips = graph_tips(&candidates, detached.as_deref());
    branch_graph(git, &info.root, &tips, limit)
}

pub fn summaries(graph: &HistoryGraph) -> Vec<&str> {
    graph.rows.iter().map(|row| row.summary.as_str()).collect()
}

/// A [`Git`] for `path` without running it, so a fake that is not about
/// discovery does not have to meet the version probe's time limit.
pub fn git_at(path: PathBuf) -> Git {
    Git {
        path,
        version: GitVersion::parse("git version 2.43.0").expect("version"),
        command_timeout: Duration::from_secs(60),
    }
}

/// The one fake git. Sidecars next to the invoked link choose what it does:
/// `<link>.version` is what `--version` prints, `<link>.mode` what any other
/// command does. A mode that starts a process records its pid in `<link>.pid`.
/// Only shell builtins run before `--version` is answered, so a busy machine
/// still meets the 250 ms probe.
const FAKE_GIT: &str = r#"#!/bin/sh
version=2.43.0
mode=
[ -f "$0.version" ] && read -r version < "$0.version"
[ -f "$0.mode" ] && read -r mode < "$0.mode"
if [ "${1-}" = --version ]; then
  case $version in
    broken) exit 1 ;;
    garbage) printf 'this is not git version output\n\377\376' ;;
    sleep) sleep 30 & echo $! > "$0.pid"; wait ;;
    *) printf 'git version %s\n' "$version" ;;
  esac
  exit 0
fi
case $mode in
  # Outlives any command limit, so the caller must kill the group.
  slow) sleep 120 & echo $! > "$0.pid"; wait ;;
  # 9 MiB, over the 8 MiB output cap.
  bomb) dd if=/dev/zero bs=1048576 count=9 2>/dev/null | tr '\0' A & echo $! > "$0.pid"; wait ;;
  # A grandchild that leaves the process group, then sleeps.
  setsid) CTHULHU_HARNESS_SETSID_PIDFILE="$0.pid" @TEST_EXE@ --exact process::harness_setsid_sleeper --include-ignored --test-threads=1 ;;
  # Exits while a grandchild keeps stdout open.
  writer) sleep 30 & echo $! > "$0.pid" ;;
  fail) cat "$0.stderr" >&2; exit 128 ;;
  argv) printf '%s\0' "$@" > "$0.argv" ;;
  branches)
    case "$*" in
      *for-each-ref*) echo for-each-ref >> "$0.calls"; cat "$0.refs" ;;
      *) echo other >> "$0.calls"; echo 4 ;;
    esac ;;
  *) echo "unexpected fake git arguments: $*" >&2; exit 1 ;;
esac
"#;

/// Links `link` to the shared fake git and writes its sidecars.
#[cfg(unix)]
pub fn fake_git(link: &Path, version: &str, mode: &str) -> PathBuf {
    static SCRIPT: OnceLock<PathBuf> = OnceLock::new();
    let script = SCRIPT.get_or_init(|| {
        let exe = sh_quote(&std::env::current_exe().expect("test binary"));
        shared_script(
            "cthulhu-it-fake-git.sh",
            &FAKE_GIT.replace("@TEST_EXE@", &exe),
        )
    });
    let dir = link.parent().expect("fake git dir");
    fs::create_dir_all(dir).expect("fake git dir");
    std::os::unix::fs::symlink(script, link)
        .unwrap_or_else(|error| panic!("symlink {}: {error}", link.display()));
    fs::write(sidecar(link, ".version"), version).expect("version sidecar");
    fs::write(sidecar(link, ".mode"), mode).expect("mode sidecar");
    link.to_path_buf()
}

/// `<path><suffix>`, next to `path`.
pub fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().expect("file name").to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// A new executable `sh` script at `path`. Only the test that owns `path`
/// runs it.
pub fn write_executable(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().expect("script dir")).expect("script dir");
    fs::write(path, body).expect("write script");
    set_executable(path);
}

/// Runs `work` on its own thread and waits at most `budget` for its result.
/// The thread is not joined: a call that ignores its own limit fails here
/// instead of hanging the test.
pub fn within<T: Send + 'static>(
    case: &str,
    budget: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(work());
    });
    rx.recv_timeout(budget)
        .unwrap_or_else(|error| panic!("{case:?}: no result within {budget:?} ({error})"))
}

/// Re-runs the ignored test `name` of this binary with `configure` applied to
/// the child only, and checks that it ran and passed.
pub fn run_child(name: &str, configure: impl FnOnce(&mut Command)) {
    let mut command = Command::new(std::env::current_exe().expect("test binary"));
    command
        .args([
            "--exact",
            name,
            "--include-ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .stdin(Stdio::null());
    configure(&mut command);
    let Output {
        status,
        stdout,
        stderr,
    } = command.output().expect("spawn child test");
    let stdout = String::from_utf8_lossy(&stdout);
    let stderr = String::from_utf8_lossy(&stderr);
    assert!(status.success(), "{name} failed:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("1 passed"),
        "{name} did not run:\n{stdout}\n{stderr}"
    );
}

/// The pid a fake recorded in `pid_file` is gone within two seconds.
pub fn assert_pid_dead(case: &str, pid_file: &Path) {
    let text = fs::read_to_string(pid_file)
        .unwrap_or_else(|error| panic!("{case:?}: pid file {}: {error}", pid_file.display()));
    let pid: u32 = text
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("{case:?}: pid {text:?}"));
    assert!(pid > 1, "{case:?}: refusing to treat pid {pid} as a child");
    assert!(
        wait_dead(pid, Duration::from_secs(2)),
        "{case:?}: pid {pid} is still alive after the git that started it was stopped"
    );
}

/// Polls every 20ms until `pid` is dead or `budget` elapses.
///
/// On Linux a missing `/proc/<pid>/stat` or state `Z` is dead. On macOS,
/// `kill -0` reporting "no such process" or `ps -o state=` reporting `Z` is
/// dead. A zombie still answers `kill -0`, so the state check is required.
pub fn wait_dead(pid: u32, budget: Duration) -> bool {
    let started = Instant::now();
    loop {
        if !process_is_alive(pid) {
            return true;
        }
        if started.elapsed() >= budget {
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(target_os = "linux")]
fn process_is_alive(pid: u32) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(text) => !matches!(linux_stat_state(&text), Some("Z")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

/// State is the field after the last `)`. `comm` may itself contain `)`.
#[cfg(target_os = "linux")]
fn linux_stat_state(text: &str) -> Option<&str> {
    let (_, rest) = text.rsplit_once(')')?;
    rest.split_whitespace().next()
}

#[cfg(target_os = "macos")]
fn process_is_alive(pid: u32) -> bool {
    let pid_text = pid.to_string();
    let kill = Command::new("/bin/kill")
        .args(["-0", &pid_text])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    if let Ok(kill) = kill
        && !kill.status.success()
    {
        let stderr = String::from_utf8_lossy(&kill.stderr).to_ascii_lowercase();
        if stderr.contains("no such process") {
            return false;
        }
    }
    let Ok(ps) = Command::new("/bin/ps")
        .args(["-o", "state=", "-p", &pid_text])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    else {
        return true;
    };
    let state = String::from_utf8_lossy(&ps.stdout);
    let state = state.trim();
    !state.is_empty() && !state.starts_with('Z')
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_is_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// POSIX single quotes. An embedded apostrophe is `'\''`.
pub fn sh_quote(path: &Path) -> String {
    let text = path.to_str().unwrap_or_else(|| {
        panic!("path is not utf-8: {}", path.display());
    });
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// One executable per `name` under `CARGO_TARGET_TMPDIR`, mode `0755`.
///
/// Test processes run in parallel and exec this file while others publish it,
/// so it is written under a unique temporary name, closed, and then renamed
/// into place: the published inode is never open for writing (`ETXTBSY`).
pub fn shared_script(name: &str, body: &str) -> PathBuf {
    static SCRIPTS: OnceLock<Mutex<std::collections::HashMap<String, PathBuf>>> = OnceLock::new();
    let scripts = SCRIPTS.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let mut guard = scripts.lock().expect("script map");
    if let Some(path) = guard.get(name) {
        return path.clone();
    }
    let root = option_env!("CARGO_TARGET_TMPDIR").unwrap_or_else(|| {
        panic!("CARGO_TARGET_TMPDIR is set when integration tests compile shared_script");
    });
    let path = Path::new(root).join(name);
    let mut staged = tempfile::NamedTempFile::new_in(root)
        .unwrap_or_else(|error| panic!("stage {}: {error}", path.display()));
    staged
        .write_all(body.as_bytes())
        .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
    let staged = staged.into_temp_path();
    set_executable(&staged);
    staged
        .persist(&path)
        .unwrap_or_else(|error| panic!("install {}: {error}", path.display()));
    guard.insert(name.to_owned(), path.clone());
    path
}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|error| panic!("chmod {}: {error}", path.display()));
}

#[cfg(not(unix))]
fn set_executable(path: &Path) {
    let _ = path;
}

/// Drops ACLs that would grant access despite a mode of `0555`.
///
/// macOS uses `/bin/chmod -N`. Linux uses `setfacl -b` when that binary is
/// installed. A missing tool is left alone; the caller probes the resulting
/// permission with `OpenOptions`.
pub fn strip_acls(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("/bin/chmod").arg("-N").arg(path).status();
    }
    #[cfg(target_os = "linux")]
    {
        let setfacl = ["/usr/bin/setfacl", "/bin/setfacl"]
            .into_iter()
            .map(Path::new)
            .find(|candidate| candidate.is_file());
        if let Some(setfacl) = setfacl {
            let _ = Command::new(setfacl).args(["-b", "--"]).arg(path).status();
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = path;
    }
}

/// `setsid(2)`, record `pid`, sleep for 30s.
///
/// The caller must not already be a process-group leader, or `setsid` returns
/// `EPERM`. The fake git runs this binary as a child of the shell, so the
/// shell keeps the group and this process can leave it.
#[cfg(unix)]
pub fn become_setsid_sleeper(pid_file: &Path) {
    if let Err(error) = rustix::process::setsid() {
        eprintln!("setsid failed: {error}");
        std::process::exit(1);
    }
    let pid = std::process::id();
    let mut file = match fs::File::create(pid_file) {
        Ok(file) => file,
        Err(error) => {
            eprintln!("pid file {}: {error}", pid_file.display());
            std::process::exit(1);
        }
    };
    if let Err(error) = writeln!(file, "{pid}") {
        eprintln!("pid file {}: {error}", pid_file.display());
        std::process::exit(1);
    }
    let _ = file.sync_all();
    thread::sleep(SETSID_SLEEP);
}
