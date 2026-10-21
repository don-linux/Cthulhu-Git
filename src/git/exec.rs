use std::fmt;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// `--no-optional-locks` needs Git 2.15; porcelain v2 status needs 2.11.
pub const MIN_GIT_VERSION: (u32, u32, u32) = (2, 15, 0);

const VERSION_PREFIX: &str = "git version ";

/// Inherited `GIT_*` names that stay. Everything else starting with `GIT_` is
/// removed: a fixed list cannot cover `GIT_CONFIG_KEY_<n>` or `GIT_EXEC_PATH`.
/// `SSH_AUTH_SOCK`, `HOME`, and `PATH` are not `GIT_` names, so they stay too.
const KEPT_GIT_VARS: &[&str] = &[
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_SSH_VARIANT",
    "GIT_ASKPASS",
];

/// `true` when `name` is an inherited `GIT_*` variable that redirects git,
/// rewrites history, or points at another exec path.
pub fn inherited_git_var_is_cleared(name: &str) -> bool {
    name.starts_with("GIT_") && !KEPT_GIT_VARS.contains(&name)
}

/// `git --version` should be nearly instant. A candidate that does not answer
/// is skipped so one hung executable cannot hide a later, working git.
const PROBE_TIMEOUT: Duration = Duration::from_millis(250);

/// Bound for every other git invocation, so a stalled filesystem cannot pin
/// the worker forever. Long enough for a large local history.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

/// Status and log output for the histories this app reads stay far below this.
/// A larger stdout is refused instead of being used as a path.
const MAX_GIT_OUTPUT: usize = 8 * 1024 * 1024;

/// A git executable that answered `--version` with a supported version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Git {
    pub path: PathBuf,
    pub version: GitVersion,
}

/// Parsed `git --version`. Only the leading dotted numbers are compared;
/// vendor suffixes such as `.windows.1` or `(Apple Git-154)` are kept in `raw`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GitVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub raw: String,
}

#[derive(Debug)]
pub struct GitOutput {
    pub stdout: Vec<u8>,
    pub stderr: String,
    pub status: ExitStatus,
}

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("{}", format_not_found(tried))]
    NotFound { tried: Vec<PathBuf> },
    #[error("CTHULHU_GIT is set to {path}, which is not an executable file.", path = display_path(.0))]
    BadOverride(PathBuf),
    #[error(
        "Git {0} is too old; version {major}.{minor}.{patch} or newer is required.",
        major = MIN_GIT_VERSION.0,
        minor = MIN_GIT_VERSION.1,
        patch = MIN_GIT_VERSION.2
    )]
    TooOld(GitVersion),
    #[error("{path} is not a directory.", path = display_path(.0))]
    NotADirectory(PathBuf),
    #[error("{path} is not inside a Git repository.", path = display_path(.0))]
    NotARepository(PathBuf),
    #[error("{}", format_unsafe(.0))]
    UnsafeRepository(PathBuf),
    #[error("Could not run git: {0}")]
    Spawn(#[source] io::Error),
    #[error("`{command}` failed (timed out): git exceeded the time limit")]
    TimedOut { command: String, limit: Duration },
    #[error("`{command}` failed (output limit): git output exceeded the limit")]
    OutputLimit { command: String },
    #[error("`{command}` failed (rejected): {stderr}")]
    Rejected { command: String, stderr: String },
    #[error("`{command}` failed ({status}){suffix}", suffix = stderr_suffix(stderr))]
    Command {
        command: String,
        status: ExitStatus,
        stderr: String,
    },
}

fn display_path(path: &Path) -> String {
    path.display().to_string()
}

fn format_not_found(tried: &[PathBuf]) -> String {
    let mut message = String::from(
        "Git was not found. Install Git or set CTHULHU_GIT to the full path of the git executable.",
    );
    if !tried.is_empty() {
        let tried: Vec<_> = tried
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        message.push_str(&format!(" Searched: {}", tried.join(", ")));
    }
    message
}

fn format_unsafe(path: &Path) -> String {
    format!(
        "Git refuses to open {0} because it is owned by another user. \
         If you trust it, run: git config --global --add safe.directory {1}",
        path.display(),
        shell_single_quote(&path.to_string_lossy())
    )
}

fn stderr_suffix(stderr: &str) -> String {
    if stderr.is_empty() {
        String::new()
    } else {
        format!(": {stderr}")
    }
}

pub(crate) fn rejected(command: &str, stderr: impl Into<String>) -> GitError {
    GitError::Rejected {
        command: command.to_owned(),
        stderr: stderr.into(),
    }
}

impl Git {
    /// Asks `path --version` and returns it without checking the version floor.
    pub(crate) fn probe(path: &Path) -> Result<Self, GitError> {
        let output = capture(
            command(path).arg("--version"),
            PROBE_TIMEOUT,
            "git --version",
        )?;
        if !output.status.success() {
            return Err(GitError::Command {
                command: "git --version".to_owned(),
                status: output.status,
                stderr: output.stderr.clone(),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let version = GitVersion::parse(&stdout).ok_or_else(|| {
            rejected(
                "git --version",
                format!("unrecognized version output: {}", stdout.trim()),
            )
        })?;

        Ok(Self {
            path: path.to_path_buf(),
            version,
        })
    }

    /// Runs `git <args>` in `cwd` without a shell, bounded by [`COMMAND_TIMEOUT`].
    ///
    /// `--no-optional-locks` skips locks a read does not need. A command that
    /// must write, such as pull, still takes its required locks. The user's
    /// environment is inherited (credential helpers, ssh-agent) except every
    /// `GIT_*` variable other than [`KEPT_GIT_VARS`].
    pub fn run(&self, cwd: &Path, args: &[&str]) -> Result<GitOutput, GitError> {
        self.run_for(cwd, args, COMMAND_TIMEOUT)
    }

    /// Like [`Git::run`], with a caller-chosen limit. Reads keep the default.
    pub fn run_for(
        &self,
        cwd: &Path,
        args: &[&str],
        timeout: Duration,
    ) -> Result<GitOutput, GitError> {
        let command_line = format!("git {}", args.join(" "));
        capture(
            command(&self.path)
                .current_dir(cwd)
                .arg("--no-optional-locks")
                .arg("--no-pager")
                .arg("--no-replace-objects")
                .args(["-c", "core.fsmonitor=false"])
                .args(["-c", "log.showSignature=false"])
                .args(args),
            timeout,
            &command_line,
        )
    }

    /// Like [`Git::run`], but a non-zero exit becomes a typed [`GitError`].
    pub fn require_ok(&self, cwd: &Path, args: &[&str]) -> Result<GitOutput, GitError> {
        self.require_ok_for(cwd, args, COMMAND_TIMEOUT)
    }

    /// Like [`Git::require_ok`], with a caller-chosen limit.
    pub fn require_ok_for(
        &self,
        cwd: &Path,
        args: &[&str],
        timeout: Duration,
    ) -> Result<GitOutput, GitError> {
        let output = self.run_for(cwd, args, timeout)?;

        if output.status.success() {
            return Ok(output);
        }

        Err(classify_failure(cwd, args, output.status, &output.stderr))
    }
}

impl GitVersion {
    /// Parses the first line of `git --version`.
    pub fn parse(output: &str) -> Option<Self> {
        let raw = output
            .lines()
            .next()?
            .trim()
            .strip_prefix(VERSION_PREFIX)?
            .trim();

        let numeric_end = raw
            .find(|ch: char| !ch.is_ascii_digit() && ch != '.')
            .unwrap_or(raw.len());
        let numeric = raw[..numeric_end].trim_end_matches('.');

        let mut parts = numeric.split('.').map(|part| part.parse::<u32>().ok());
        let major = parts.next()??;
        let minor = parts.next().unwrap_or(Some(0))?;
        let patch = parts.next().unwrap_or(Some(0))?;

        Some(Self {
            major,
            minor,
            patch,
            raw: raw.to_owned(),
        })
    }

    pub fn numbers(&self) -> (u32, u32, u32) {
        (self.major, self.minor, self.patch)
    }

    pub fn is_supported(&self) -> bool {
        self.numbers() >= MIN_GIT_VERSION && !is_prerelease(&self.raw)
    }
}

/// `2.15.0-rc0` is not the 2.15.0 release that introduced `--no-optional-locks`.
/// Vendor suffixes (`.windows.1`, `.vfs.0.0`, `(Apple Git-154)`) are that release.
fn is_prerelease(raw: &str) -> bool {
    let rest = raw.trim_start_matches(|ch: char| ch.is_ascii_digit() || ch == '.');
    let rest = rest.to_ascii_lowercase();
    ["-rc", "-alpha", "-beta", "-pre"]
        .iter()
        .any(|marker| rest.starts_with(marker))
}

impl fmt::Display for GitVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

/// The single place where git processes are built, so every invocation
/// (including `--version`) gets the same environment and window flags.
fn command(program: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Remove inherited names before setting the ones this process owns.
    // `env_remove` after `env` would drop `GIT_TERMINAL_PROMPT` too.
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(inherited_git_var_is_cleared) {
            command.env_remove(key);
        }
    }

    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_PAGER", "cat")
        // The "C" locale exists everywhere and keeps the stderr we match on in English.
        .env("LC_ALL", "C")
        .env("LANGUAGE", "C");

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // The child's pid becomes the process-group id, so a timeout can
        // signal git and the helpers it spawned (`ssh`, `git-remote-https`).
        command.process_group(0);
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    command
}

struct CappedRead {
    bytes: Vec<u8>,
    exceeded: bool,
    /// A read error is not EOF. Treating it as EOF would accept a partial
    /// `rev-parse` as a repository root.
    error: Option<io::Error>,
}

fn read_capped(mut reader: impl Read, limit: usize, exceeded_flag: &AtomicBool) -> CappedRead {
    let mut bytes = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let room = limit.saturating_sub(bytes.len());
        if room == 0 {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(_) => {
                    exceeded_flag.store(true, Ordering::Relaxed);
                    return CappedRead {
                        bytes,
                        exceeded: true,
                        error: None,
                    };
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    return CappedRead {
                        bytes,
                        exceeded: false,
                        error: Some(error),
                    };
                }
            }
        }
        let read = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return CappedRead {
                    bytes,
                    exceeded: false,
                    error: Some(error),
                };
            }
        };
        if read > room {
            bytes.extend_from_slice(&buf[..room]);
            exceeded_flag.store(true, Ordering::Relaxed);
            return CappedRead {
                bytes,
                exceeded: true,
                error: None,
            };
        }
        bytes.extend_from_slice(&buf[..read]);
    }
    CappedRead {
        bytes,
        exceeded: false,
        error: None,
    }
}

fn output_limit(command_name: &str) -> GitError {
    GitError::OutputLimit {
        command: command_name.to_owned(),
    }
}

fn timed_out(command_name: &str, limit: Duration) -> GitError {
    GitError::TimedOut {
        command: command_name.to_owned(),
        limit,
    }
}

/// How long to let a reader finish after the child has exited or been killed.
/// Long enough for a pipe to close, short enough that a grandchild holding
/// stdout cannot pin the caller.
const READER_BUDGET: Duration = Duration::from_millis(200);

fn capture(
    command: &mut Command,
    timeout: Duration,
    command_name: &str,
) -> Result<GitOutput, GitError> {
    let mut child = command.spawn().map_err(GitError::Spawn)?;
    let pid = child.id();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| GitError::Spawn(io::Error::other("git stdout was not piped")))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| GitError::Spawn(io::Error::other("git stderr was not piped")))?;
    let exceeded = Arc::new(AtomicBool::new(false));
    let stdout_exceeded = Arc::clone(&exceeded);
    let stderr_exceeded = Arc::clone(&exceeded);
    let stdout_thread =
        thread::spawn(move || read_capped(stdout, MAX_GIT_OUTPUT, &stdout_exceeded));
    let stderr_thread =
        thread::spawn(move || read_capped(stderr, MAX_GIT_OUTPUT, &stderr_exceeded));

    let started = Instant::now();
    let status = loop {
        if exceeded.load(Ordering::Relaxed) {
            stop_group(pid, &mut child);
            let _ = finish_reader(stdout_thread, READER_BUDGET);
            let _ = finish_reader(stderr_thread, READER_BUDGET);
            return Err(output_limit(command_name));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                stop_group(pid, &mut child);
                let _ = finish_reader(stdout_thread, READER_BUDGET);
                let _ = finish_reader(stderr_thread, READER_BUDGET);
                return Err(timed_out(command_name, timeout));
            }
            Ok(None) => thread::sleep(Duration::from_millis(5)),
            Err(error) => {
                stop_group(pid, &mut child);
                let _ = finish_reader(stdout_thread, READER_BUDGET);
                let _ = finish_reader(stderr_thread, READER_BUDGET);
                return Err(GitError::Spawn(error));
            }
        }
    };

    let (stdout, stderr) = finish_both(stdout_thread, stderr_thread, pid);
    if let Some(error) = stdout.error.or(stderr.error) {
        // The child already exited. A grandchild may still hold a pipe.
        stop_group(pid, &mut child);
        return Err(GitError::Spawn(error));
    }
    if stdout.exceeded || stderr.exceeded {
        stop_group(pid, &mut child);
        return Err(output_limit(command_name));
    }

    Ok(GitOutput {
        stdout: stdout.bytes,
        stderr: String::from_utf8_lossy(&stderr.bytes).trim().to_owned(),
        status,
    })
}

fn finish_both(
    stdout_thread: thread::JoinHandle<CappedRead>,
    stderr_thread: thread::JoinHandle<CappedRead>,
    pid: u32,
) -> (CappedRead, CappedRead) {
    let stdout_pending = pending_reader(stdout_thread);
    let stderr_pending = pending_reader(stderr_thread);
    match (
        recv_reader(&stdout_pending, READER_BUDGET),
        recv_reader(&stderr_pending, READER_BUDGET),
    ) {
        (Some(stdout), Some(stderr)) => (stdout, stderr),
        (stdout, stderr) => {
            // The direct child has exited, but a grandchild still holds a pipe.
            signal_group(pid, GroupSignal::Kill);
            let stdout = stdout.unwrap_or_else(|| {
                recv_reader(&stdout_pending, READER_BUDGET).unwrap_or_else(reader_abandoned)
            });
            let stderr = stderr.unwrap_or_else(|| {
                recv_reader(&stderr_pending, READER_BUDGET).unwrap_or_else(reader_abandoned)
            });
            (stdout, stderr)
        }
    }
}

struct PendingRead {
    rx: mpsc::Receiver<thread::Result<CappedRead>>,
}

fn pending_reader(handle: thread::JoinHandle<CappedRead>) -> PendingRead {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(handle.join());
    });
    PendingRead { rx }
}

fn recv_reader(pending: &PendingRead, budget: Duration) -> Option<CappedRead> {
    match pending.rx.recv_timeout(budget) {
        Ok(Ok(read)) => Some(read),
        Ok(Err(_)) => Some(CappedRead {
            bytes: Vec::new(),
            exceeded: false,
            error: Some(io::Error::other("git output reader panicked")),
        }),
        Err(_) => None,
    }
}

fn finish_reader(handle: thread::JoinHandle<CappedRead>, budget: Duration) -> Option<CappedRead> {
    recv_reader(&pending_reader(handle), budget)
}

fn reader_abandoned() -> CappedRead {
    CappedRead {
        bytes: Vec::new(),
        exceeded: false,
        error: Some(io::Error::other("git output reader did not finish")),
    }
}

enum GroupSignal {
    Term,
    Kill,
}

fn signal_group(pid: u32, signal: GroupSignal) {
    #[cfg(unix)]
    {
        let Some(pid) = rustix::process::Pid::from_raw(i32::try_from(pid).unwrap_or(0)) else {
            return;
        };
        if pid == rustix::process::getpid() || pid == rustix::process::getpgrp() {
            return;
        }
        let signal = match signal {
            GroupSignal::Term => rustix::process::Signal::TERM,
            GroupSignal::Kill => rustix::process::Signal::KILL,
        };
        let _ = rustix::process::kill_process_group(pid, signal);
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, signal);
    }
}

/// `SIGTERM`, a short grace, then `SIGKILL` of the child's process group.
/// `wait` is not used: a process stuck in uninterruptible I/O would pin the
/// caller, which is the stall this timeout exists to bound.
fn stop_group(pid: u32, child: &mut Child) {
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
    #[cfg(unix)]
    {
        signal_group(pid, GroupSignal::Term);
        reap(child, Duration::from_millis(50));
        signal_group(pid, GroupSignal::Kill);
    }
    reap(child, Duration::from_millis(50));
}

fn reap(child: &mut Child, budget: Duration) {
    let started = Instant::now();
    while started.elapsed() < budget {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => thread::sleep(Duration::from_millis(5)),
        }
    }
}

pub(crate) fn classify_failure(
    cwd: &Path,
    args: &[&str],
    status: ExitStatus,
    stderr: &str,
) -> GitError {
    if is_not_a_repository_message(stderr) {
        return GitError::NotARepository(cwd.to_path_buf());
    }

    if stderr.contains("detected dubious ownership") {
        let path = dubious_ownership_path(stderr)
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| cwd.to_path_buf());
        return GitError::UnsafeRepository(path);
    }

    GitError::Command {
        command: format!("git {}", args.join(" ")),
        status,
        stderr: stderr.to_owned(),
    }
}

/// The primary fatal, not a path or a sentence that merely contains the words.
fn is_not_a_repository_message(stderr: &str) -> bool {
    stderr.lines().any(|line| {
        line.trim_start()
            .to_ascii_lowercase()
            .starts_with("fatal: not a git repository")
    })
}

/// Path from git's "safe.directory" hint when it is present, otherwise from
/// `repository at '<path>'`. An embedded apostrophe is kept: the hint uses
/// shell quoting (`'\''`), and a single-line fake message closes on its last quote.
fn dubious_ownership_path(stderr: &str) -> Option<PathBuf> {
    if let Some(path) = safe_directory_suggestion(stderr)
        && !path.as_os_str().is_empty()
    {
        return Some(path);
    }

    let (_, rest) = stderr.split_once("repository at '")?;
    if rest.starts_with('\'') {
        return None;
    }
    let rest = rest.trim_end_matches(['\n', '\r']);
    let end = rest.rfind('\'')?;
    let path = &rest[..end];
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

fn safe_directory_suggestion(stderr: &str) -> Option<PathBuf> {
    let (_, after) = stderr.split_once("safe.directory ")?;
    let token = after.trim_start();
    if token.starts_with('\'') {
        return unquote_shell_single(token).map(PathBuf::from);
    }
    let end = token
        .find(|ch: char| ch.is_whitespace())
        .unwrap_or(token.len());
    let path = &token[..end];
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

/// Decodes one single-quoted shell word. `'\''` is an embedded apostrophe.
/// Extra text after the closing quote is ignored.
fn unquote_shell_single(token: &str) -> Option<String> {
    let mut chars = token.chars().peekable();
    if chars.next() != Some('\'') {
        return None;
    }
    let mut out = String::new();
    while let Some(ch) = chars.next() {
        if ch != '\'' {
            out.push(ch);
            continue;
        }
        if chars.peek() == Some(&'\\') {
            chars.next();
            if chars.next() != Some('\'') || chars.next() != Some('\'') {
                return None;
            }
            out.push('\'');
            continue;
        }
        return Some(out);
    }
    None
}

fn shell_single_quote(text: &str) -> String {
    let mut quoted = String::from("'");
    for ch in text.chars() {
        if ch == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_raw(raw: i32) -> ExitStatus {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            ExitStatus::from_raw(raw)
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;
            ExitStatus::from_raw(raw as u32)
        }
    }

    fn version(output: &str) -> GitVersion {
        GitVersion::parse(output).expect("parsable version")
    }

    #[test]
    fn parses_plain_version() {
        let parsed = version("git version 2.43.0\n");
        assert_eq!(parsed.numbers(), (2, 43, 0));
        assert_eq!(parsed.raw, "2.43.0");
    }

    #[test]
    fn parses_windows_suffix() {
        let parsed = version("git version 2.43.0.windows.1");
        assert_eq!(parsed.numbers(), (2, 43, 0));
        assert_eq!(parsed.raw, "2.43.0.windows.1");
        assert!(parsed.is_supported());
    }

    #[test]
    fn apple_suffix_is_not_read_as_a_version_component() {
        let parsed = version("git version 2.39.5 (Apple Git-154)\n");
        assert_eq!(parsed.numbers(), (2, 39, 5));
        assert_eq!(parsed.raw, "2.39.5 (Apple Git-154)");
        assert!(parsed.is_supported());
        assert!(!version("git version 2.14.3 (Apple Git-1)").is_supported());
    }

    #[test]
    fn rejects_empty_headerless_and_non_numeric() {
        for output in [
            "",
            "git version ",
            "git version\n",
            "version 2.43.0",
            "git version abc",
            "git version .1.2",
            "git version 2..1",
        ] {
            assert_eq!(GitVersion::parse(output), None, "{output:?}");
        }
    }

    #[test]
    fn overflowing_component_is_rejected_not_skipped() {
        assert_eq!(GitVersion::parse("git version 2.99999999999.0"), None);
    }

    #[test]
    fn release_candidate_does_not_meet_the_floor() {
        let parsed = version("git version 2.15.0-rc0\n");
        assert_eq!(parsed.numbers(), (2, 15, 0));
        assert!(!parsed.is_supported());
        assert!(version("git version 2.43.0.windows.1").is_supported());
    }

    #[test]
    fn not_a_repository_matches_the_fatal_line_only() {
        let older = classify_failure(
            Path::new("/tmp/plain"),
            &["rev-parse", "--show-toplevel"],
            status_raw(128),
            "fatal: Not a git repository (or any of the parent directories): .git",
        );
        assert!(matches!(older, GitError::NotARepository(_)), "{older:?}");

        let mentioned = classify_failure(
            Path::new("/tmp/plain"),
            &["rev-parse"],
            status_raw(128),
            "fatal: cannot change to 'not a git repository': No such file or directory",
        );
        assert!(
            matches!(mentioned, GitError::Command { .. }),
            "{mentioned:?}"
        );
    }

    #[test]
    fn version_floor_is_2_15() {
        for supported in [
            "2.15.0",
            "2.15",
            "2.15.0.0",
            "2.15.0.windows.1",
            "2.43.0",
            "3.0.0",
        ] {
            assert!(
                version(&format!("git version {supported}")).is_supported(),
                "{supported}"
            );
        }
        for old in ["2.14.3", "1.9.1", "2", "2.9.9"] {
            assert!(
                !version(&format!("git version {old}")).is_supported(),
                "{old}"
            );
        }
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(version("git version 2.9.0") < version("git version 2.15.0"));
        assert!(version("git version 2.15.1") > version("git version 2.15.0"));
    }

    #[test]
    fn not_a_repository_is_typed() {
        let error = classify_failure(
            Path::new("/tmp/somewhere"),
            &["rev-parse", "--show-toplevel"],
            status_raw(128),
            "fatal: not a git repository (or any of the parent directories): .git",
        );
        assert!(
            matches!(error, GitError::NotARepository(path) if path == Path::new("/tmp/somewhere"))
        );
    }

    #[test]
    fn dubious_ownership_reports_repository_root() {
        let stderr = "fatal: detected dubious ownership in repository at '/srv/shared/repo'\n\
                      To add an exception for this directory, call:\n\n\
                      \tgit config --global --add safe.directory /srv/shared/repo";
        let error = classify_failure(
            Path::new("/srv/shared/repo/sub"),
            &["rev-parse"],
            status_raw(128),
            stderr,
        );
        let GitError::UnsafeRepository(path) = &error else {
            panic!("expected UnsafeRepository, got {error:?}");
        };
        assert_eq!(path, Path::new("/srv/shared/repo"));
        assert!(
            error
                .to_string()
                .contains("git config --global --add safe.directory '/srv/shared/repo'")
        );
    }

    #[test]
    fn other_failures_keep_command_status_and_stderr() {
        let status = status_raw(9);
        let error = classify_failure(
            Path::new("/repo"),
            &["status", "--porcelain=v2"],
            status,
            "",
        );
        assert!(matches!(error, GitError::Command { .. }), "{error:?}");
        assert_eq!(
            error.to_string(),
            format!("`git status --porcelain=v2` failed ({status})")
        );
    }

    #[test]
    fn a_read_error_is_not_an_empty_success() {
        struct ThenBoom {
            sent: bool,
        }
        impl Read for ThenBoom {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if !self.sent {
                    self.sent = true;
                    buf[0] = b'x';
                    return Ok(1);
                }
                Err(io::Error::other("mid-read"))
            }
        }
        let flag = AtomicBool::new(false);
        let read = read_capped(ThenBoom { sent: false }, 64, &flag);
        assert!(
            read.error.is_some(),
            "a mid-read error must not look like EOF"
        );
        assert!(!read.exceeded);
        assert!(!flag.load(Ordering::Relaxed));
    }

    #[test]
    fn kept_git_variables_are_ssh_and_askpass_only() {
        for cleared in [
            "GIT_DIR",
            "GIT_EXEC_PATH",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_KEY_0",
            "GIT_CONFIG_PARAMETERS",
            "GIT_TRACE",
        ] {
            assert!(inherited_git_var_is_cleared(cleared), "{cleared}");
        }
        for kept in [
            "GIT_SSH",
            "GIT_SSH_COMMAND",
            "GIT_SSH_VARIANT",
            "GIT_ASKPASS",
            "SSH_AUTH_SOCK",
            "PATH",
            "HOME",
            "GH_TOKEN",
        ] {
            assert!(!inherited_git_var_is_cleared(kept), "{kept}");
        }
    }

    #[test]
    fn error_messages_name_the_override_variable() {
        let error = GitError::BadOverride(PathBuf::from("/nope"));
        assert_eq!(
            error.to_string(),
            "CTHULHU_GIT is set to /nope, which is not an executable file."
        );
    }
}
