use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
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
    name.get(..4)
        .is_some_and(|prefix| env_names_match(prefix, "GIT_"))
        && !KEPT_GIT_VARS.iter().any(|kept| env_names_match(kept, name))
}

/// Environment names compare as the platform does: without regard to case on
/// Windows, where Git for Windows reads `Git_Dir` as `GIT_DIR`, and exactly
/// elsewhere, where `git_dir` is another variable that git never reads.
pub fn env_names_match(left: &str, right: &str) -> bool {
    if cfg!(windows) {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}

/// `git --version` should be nearly instant. A candidate that does not answer
/// is skipped so one hung executable cannot hide a later, working git.
const PROBE_TIMEOUT: Duration = Duration::from_millis(250);

/// Default [`Git::command_timeout`], so a stalled filesystem cannot pin the
/// worker forever. Long enough for a large local history.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

/// Status and log output for the histories this app reads stay far below this.
/// A larger stdout is refused instead of being used as a path.
const MAX_GIT_OUTPUT: usize = 8 * 1024 * 1024;

/// A git executable that answered `--version` with a supported version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Git {
    pub path: PathBuf,
    pub version: GitVersion,
    /// The limit for [`Git::run`] and [`Git::require_ok`]. A command that
    /// passes it is killed together with the processes it started.
    pub command_timeout: Duration,
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
    #[error("CTHULHU_GIT is set to {0}, which is not an executable file.")]
    BadOverride(PathBuf),
    #[error(
        "Git {0} is too old; version {major}.{minor}.{patch} or newer is required.",
        major = MIN_GIT_VERSION.0,
        minor = MIN_GIT_VERSION.1,
        patch = MIN_GIT_VERSION.2
    )]
    TooOld(GitVersion),
    #[error("{0} is not a directory.")]
    NotADirectory(PathBuf),
    #[error("{0} is not inside a Git repository.")]
    NotARepository(PathBuf),
    #[error(
        "Git refuses to open {0} because it is owned by another user. \
         If you trust it, run: git config --global --add safe.directory {quoted}",
        quoted = shell_single_quote(&.0.to_string_lossy())
    )]
    UnsafeRepository(PathBuf),
    #[error("Could not run git: {0}")]
    Spawn(#[source] io::Error),
    #[error("`{command}` failed (timed out): git exceeded the time limit")]
    TimedOut { command: String, limit: Duration },
    #[error("`{command}` failed (output limit): git output exceeded the limit")]
    OutputLimit { command: String },
    /// Git succeeded, but this app refuses its output or the request.
    #[error("`{command}` failed (rejected): {reason}")]
    Rejected { command: String, reason: String },
    #[error("`{command}` failed ({status}){suffix}", suffix = stderr_suffix(stderr))]
    Command {
        command: String,
        status: ExitStatus,
        stderr: String,
    },
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

fn stderr_suffix(stderr: &str) -> String {
    if stderr.is_empty() {
        String::new()
    } else {
        format!(": {stderr}")
    }
}

pub(crate) fn rejected(command: &str, reason: impl Into<String>) -> GitError {
    GitError::Rejected {
        command: command.to_owned(),
        reason: reason.into(),
    }
}

impl Git {
    /// Asks `path --version` and returns it without checking the version floor.
    pub(crate) fn probe(path: &Path) -> Result<Self, GitError> {
        let output = capture(
            command(path).arg("--version"),
            PROBE_TIMEOUT,
            "git --version",
            None,
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
            command_timeout: COMMAND_TIMEOUT,
        })
    }

    /// Runs `git <args>` in `cwd` without a shell, bounded by
    /// [`Git::command_timeout`].
    ///
    /// `--no-optional-locks` skips locks a read does not need. A command that
    /// must write, such as pull, still takes its required locks. The user's
    /// environment is inherited (credential helpers, ssh-agent) except every
    /// `GIT_*` variable other than [`KEPT_GIT_VARS`].
    pub fn run(&self, cwd: &Path, args: &[&str]) -> Result<GitOutput, GitError> {
        self.run_for(cwd, args, self.command_timeout)
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
            harden(&mut command(&self.path), cwd, args),
            timeout,
            &command_line,
            Some(cwd),
        )
    }

    /// Like [`Git::run`], but a non-zero exit becomes a typed [`GitError`].
    pub fn require_ok(&self, cwd: &Path, args: &[&str]) -> Result<GitOutput, GitError> {
        self.require_ok_for(cwd, args, self.command_timeout)
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

    /// [`Git::require_ok`], then `parse` on stdout. Output that `parse`
    /// refuses is [`GitError::Rejected`] as unrecognized `what` output.
    pub(crate) fn read<T>(
        &self,
        cwd: &Path,
        args: &[&str],
        what: &str,
        parse: impl FnOnce(&[u8]) -> Option<T>,
    ) -> Result<T, GitError> {
        let output = self.require_ok(cwd, args)?;
        parse(&output.stdout).ok_or_else(|| {
            rejected(
                &format!("git {}", args.join(" ")),
                format!("unrecognized {what} output"),
            )
        })
    }
}

/// Which CR [`trim_eol`] removes once git's LF is gone.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Cr {
    /// A CR at the end, whether or not an LF followed it.
    Any,
    /// Only the CR of a CRLF.
    AfterLf,
    /// None: a directory whose name ends in CR keeps it.
    Kept,
}

/// `bytes` without git's line ending: one LF, then one CR as `cr` says.
pub(crate) fn trim_eol(bytes: &[u8], cr: Cr) -> &[u8] {
    let (rest, had_lf) = match bytes.strip_suffix(b"\n") {
        Some(rest) => (rest, true),
        None => (bytes, false),
    };
    let strip_cr = match cr {
        Cr::Any => true,
        Cr::AfterLf => had_lf,
        Cr::Kept => false,
    };
    if strip_cr {
        rest.strip_suffix(b"\r").unwrap_or(rest)
    } else {
        rest
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

/// The PATH a git child gets: `path` without empty or relative entries, then
/// `extra_dirs_that_exist` that are not on it yet.
///
/// Git runs with the repository as its working directory, so an empty or
/// relative entry would find `ssh`, a credential helper or `gpg` inside the
/// repository. Entries already on `path` stay where they are, so a directory
/// the user put first still wins. Existence is the caller's decision: this
/// does not touch the filesystem.
pub fn git_child_path(path: &OsStr, extra_dirs_that_exist: &[PathBuf]) -> OsString {
    let mut dirs: Vec<PathBuf> = std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .collect();
    for extra in extra_dirs_that_exist {
        if !dirs.iter().any(|dir| dir == extra) {
            dirs.push(extra.clone());
        }
    }
    std::env::join_paths(dirs).unwrap_or_else(|_| path.to_os_string())
}

const MACOS_HOMEBREW_BIN: &str = "/opt/homebrew/bin";
const MACOS_USR_LOCAL_BIN: &str = "/usr/local/bin";

/// Homebrew then `/usr/local`, limited to directories `exists` accepts.
///
/// The closure is injected so a test can pretend the directories are there
/// without creating `/opt/homebrew` on the host.
fn macos_gui_path_dirs(exists: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    [MACOS_HOMEBREW_BIN, MACOS_USR_LOCAL_BIN]
        .into_iter()
        .map(PathBuf::from)
        .filter(|dir| exists(dir))
        .collect()
}

/// Sets [`git_child_path`] on `command`, or leaves an inherited PATH that is
/// already that. When nothing usable is left PATH is removed: an empty PATH
/// would search the working directory.
fn apply_child_path(command: &mut Command, parent_path: Option<&OsStr>, extras: &[PathBuf]) {
    let path = git_child_path(parent_path.unwrap_or_default(), extras);
    if parent_path == Some(path.as_os_str()) || (parent_path.is_none() && path.is_empty()) {
        return;
    }
    if path.is_empty() {
        command.env_remove("PATH");
    } else {
        command.env("PATH", path);
    }
}

/// The single place where git processes are built, so every invocation
/// (including `--version`) gets the same environment and window flags.
fn command(program: &Path) -> Command {
    // Finder's PATH is `/usr/bin:/bin:/usr/sbin:/sbin`, which hides Homebrew
    // helpers such as `git-credential-manager` and a Homebrew `ssh`. Append
    // those directories on macOS only, after the user's entries.
    let path_extras = if cfg!(target_os = "macos") {
        macos_gui_path_dirs(|dir| dir.is_dir())
    } else {
        Vec::new()
    };
    command_with_env(program, std::env::vars_os(), &path_extras)
}

/// [`command`] for a parent whose environment is `parent_env`, with
/// `path_extras` appended to its PATH.
///
/// The child still inherits the real environment of this process: only the
/// changes against `parent_env` are recorded on the command, which is why
/// [`command`] passes `std::env::vars_os()`.
fn command_with_env(
    program: &Path,
    parent_env: impl IntoIterator<Item = (OsString, OsString)>,
    path_extras: &[PathBuf],
) -> Command {
    let parent_env: Vec<(OsString, OsString)> = parent_env.into_iter().collect();
    let inherited = |name: &str| {
        parent_env
            .iter()
            .find(|(key, _)| key.to_str().is_some_and(|key| env_names_match(key, name)))
            .map(|(_, value)| value.as_os_str())
    };
    let mut command = Command::new(program);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Remove inherited names before setting the ones this process owns.
    // `env_remove` after `env` would drop `GIT_TERMINAL_PROMPT` too.
    for (key, _) in &parent_env {
        if key.to_str().is_some_and(inherited_git_var_is_cleared) {
            command.env_remove(key);
        }
    }

    command
        .env("GIT_TERMINAL_PROMPT", "0")
        // The "C" locale exists everywhere and keeps the stderr we match on in English.
        .env("LC_ALL", "C")
        .env("LANGUAGE", "C")
        // No lazy fetch of promisor objects, unless the user's own value
        // (removed by the sweep) says otherwise. Git 2.15 ignores the name.
        .env(
            "GIT_NO_LAZY_FETCH",
            inherited("GIT_NO_LAZY_FETCH").unwrap_or(OsStr::new("1")),
        );

    // Windows spells it `Path`; `inherited` compares names as the platform does.
    apply_child_path(&mut command, inherited("PATH"), path_extras);

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

/// Points `command` at `cwd` and puts `args` after the options every
/// [`Git::run`] gets. Command-line options win over any config file, so a
/// repository cannot turn these back on. For its own children, git turns
/// `--no-optional-locks` into `GIT_OPTIONAL_LOCKS=0` and `--no-pager` into
/// `GIT_PAGER=cat` (`git.c`, since 2.15), and [`command_with_env`] removes
/// an inherited value of either.
fn harden<'a>(command: &'a mut Command, cwd: &Path, args: &[&str]) -> &'a mut Command {
    command
        .current_dir(cwd)
        .arg("--no-optional-locks")
        .arg("--no-pager")
        .arg("--no-replace-objects")
        // Empty, not `false`: Git 2.16–2.35 read the value as a hook
        // program and would run a command named `false` on every
        // index read. Empty turns fsmonitor off in every version.
        .args(["-c", "core.fsmonitor="])
        .args(["-c", "log.showSignature=false"])
        .args(args)
}

/// `reader` to its end, or `None` once it has more than `limit` bytes. A read
/// error is not the end: a partial `rev-parse` would pass for a repository root.
fn read_capped(reader: impl Read, limit: usize) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    reader
        .take((limit as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    Ok((bytes.len() <= limit).then_some(bytes))
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

/// `spawn` returns [`io::ErrorKind::NotFound`] when `current_dir` is missing,
/// the same kind as a missing executable. A missing git binary stays
/// [`GitError::Spawn`]. A program that exists, aimed at a workdir that is not
/// a directory, is [`GitError::NotADirectory`].
fn classify_spawn_error(command: &Command, error: io::Error, workdir: Option<&Path>) -> GitError {
    let program_exists = Path::new(command.get_program()).exists();
    if program_exists
        && let Some(dir) = workdir
        && !dir.is_dir()
        && matches!(
            error.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
        )
    {
        return GitError::NotADirectory(dir.to_path_buf());
    }
    GitError::Spawn(error)
}

/// How long to let a reader finish after the child has exited or been killed.
/// Long enough for a pipe to close, short enough that a grandchild holding
/// stdout cannot pin the caller.
const READER_BUDGET: Duration = Duration::from_millis(200);

/// What one pipe gave: [`read_capped`] of stdout (slot 0) or stderr (slot 1).
type Drained = io::Result<Option<Vec<u8>>>;

/// Reads `pipe` on its own thread and sends the result tagged with `slot`.
fn drain(slot: usize, pipe: impl Read + Send + 'static, tx: &mpsc::Sender<(usize, Drained)>) {
    let tx = tx.clone();
    thread::spawn(move || {
        // A send fails only once `capture` has returned without it.
        let _ = tx.send((slot, read_capped(pipe, MAX_GIT_OUTPUT)));
    });
}

fn capture(
    command: &mut Command,
    timeout: Duration,
    command_name: &str,
    workdir: Option<&Path>,
) -> Result<GitOutput, GitError> {
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return Err(classify_spawn_error(command, error, workdir)),
    };
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    // `command` pipes both streams, so neither `take` can come back empty.
    drain(0, child.stdout.take().expect("git stdout is piped"), &tx);
    drain(1, child.stderr.take().expect("git stderr is piped"), &tx);
    let mut slots: [Option<Drained>; 2] = [None, None];
    let past_cap =
        |slots: &[Option<Drained>; 2]| slots.iter().any(|slot| matches!(slot, Some(Ok(None))));

    let started = Instant::now();
    let status = loop {
        if past_cap(&slots) {
            // Stop git now instead of waiting for it to write the rest.
            stop_group(pid, &mut child);
            return Err(output_limit(command_name));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                stop_group(pid, &mut child);
                return Err(timed_out(command_name, timeout));
            }
            Ok(None) => {}
            Err(error) => {
                stop_group(pid, &mut child);
                return Err(GitError::Spawn(error));
            }
        }
        // This thread still holds `tx`, so the channel never disconnects:
        // once both readers are done this waits 5 ms instead of spinning.
        if let Ok((slot, drained)) = rx.recv_timeout(Duration::from_millis(5)) {
            slots[slot] = Some(drained);
        }
    };

    // git has exited, but a grandchild can still hold a pipe. Give the
    // readers a moment, stop the tree, and give them one more.
    let mut stopped = false;
    while slots.iter().any(Option::is_none) {
        match rx.recv_timeout(READER_BUDGET) {
            Ok((slot, drained)) => slots[slot] = Some(drained),
            Err(_) if !stopped => {
                // `setsid` may have moved that grandchild out of git's group.
                super::reap::reap_process_tree(pid);
                stopped = true;
            }
            Err(_) => break,
        }
    }
    let [stdout, stderr] = slots.map(|slot| {
        slot.unwrap_or_else(|| Err(io::Error::other("git output reader did not finish")))
    });
    match (stdout, stderr) {
        (Ok(Some(stdout)), Ok(Some(stderr))) => Ok(GitOutput {
            stdout,
            stderr: String::from_utf8_lossy(&stderr).trim().to_owned(),
            status,
        }),
        (Err(error), _) | (_, Err(error)) => {
            // The child already exited. A grandchild may still hold a pipe.
            stop_group(pid, &mut child);
            Err(GitError::Spawn(error))
        }
        _ => {
            stop_group(pid, &mut child);
            Err(output_limit(command_name))
        }
    }
}

/// Kill the child's process group, including a `setsid` grandchild, then
/// briefly `try_wait` the direct child. `wait` is not used: a process stuck
/// in uninterruptible I/O would pin the caller, which is the stall this
/// timeout exists to bound.
fn stop_group(pid: u32, child: &mut Child) {
    #[cfg(unix)]
    {
        super::reap::reap_process_tree(pid);
    }
    #[cfg(not(unix))]
    {
        // Windows has no process group here. `child.kill()` stops the direct
        // child only; grandchildren are not killed. Job Objects would, and
        // they need unsafe, which this crate forbids. This module has no
        // Windows kill helper to shell out to.
        let _ = (pid, child.kill());
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
/// Newer git reports a `.git` file whose `gitdir:` target is missing as
/// "gitfile does not point to a valid repository" instead of "not a git repository".
fn is_not_a_repository_message(stderr: &str) -> bool {
    stderr.lines().any(|line| {
        let line = line.trim_start().to_ascii_lowercase();
        line.starts_with("fatal: not a git repository")
            || line.starts_with("fatal: gitfile does not point to a valid repository")
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
    use proptest::prelude::*;

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

    /// `git --version` output, and the numbers, `raw` (what the version
    /// displays) and floor check it gives. `None` is not a version.
    type Version = Option<((u32, u32, u32), &'static str, bool)>;

    const VERSIONS: &[(&str, Version)] = &[
        ("git version 2.43.0\n", Some(((2, 43, 0), "2.43.0", true))),
        (
            "git version 2.43.0.windows.1",
            Some(((2, 43, 0), "2.43.0.windows.1", true)),
        ),
        (
            "git version 2.39.5 (Apple Git-154)\n",
            Some(((2, 39, 5), "2.39.5 (Apple Git-154)", true)),
        ),
        (
            "git version 2.14.3 (Apple Git-1)",
            Some(((2, 14, 3), "2.14.3 (Apple Git-1)", false)),
        ),
        // The floor is 2.15.0, and a missing component is zero.
        ("git version 2.15", Some(((2, 15, 0), "2.15", true))),
        ("git version 2.15.", Some(((2, 15, 0), "2.15.", true))),
        ("git version 2.15.0.1", Some(((2, 15, 0), "2.15.0.1", true))),
        ("git version 3.0.0", Some(((3, 0, 0), "3.0.0", true))),
        ("git version 2", Some(((2, 0, 0), "2", false))),
        ("git version 2.", Some(((2, 0, 0), "2.", false))),
        ("git version 2.9.9", Some(((2, 9, 9), "2.9.9", false))),
        ("git version 1.9.1", Some(((1, 9, 1), "1.9.1", false))),
        // A pre-release is not the release that brought `--no-optional-locks`.
        (
            "git version 2.15.0-rc0\n",
            Some(((2, 15, 0), "2.15.0-rc0", false)),
        ),
        (
            "git version 2.20.0-RC1",
            Some(((2, 20, 0), "2.20.0-RC1", false)),
        ),
        (
            "git version 2.20.0-alpha",
            Some(((2, 20, 0), "2.20.0-alpha", false)),
        ),
        (
            "git version 2.20.0-beta.2",
            Some(((2, 20, 0), "2.20.0-beta.2", false)),
        ),
        (
            "git version 2.20.0-pre",
            Some(((2, 20, 0), "2.20.0-pre", false)),
        ),
        (
            "git version 2.16.0\0windows",
            Some(((2, 16, 0), "2.16.0\0windows", true)),
        ),
        ("", None),
        ("\0", None),
        ("git version ", None),
        ("git version \0", None),
        ("git version\n", None),
        ("version 2.43.0", None),
        ("git version abc", None),
        ("git version .1.2", None),
        ("git version 2..1", None),
        // An overflowing component is refused, not skipped.
        ("git version 2.99999999999.0", None),
    ];

    #[test]
    fn git_version_table() {
        for (case, expected) in VERSIONS {
            let got = GitVersion::parse(case).map(|version| {
                (
                    version.numbers(),
                    version.to_string(),
                    version.is_supported(),
                )
            });
            let expected =
                expected.map(|(numbers, raw, supported)| (numbers, raw.to_owned(), supported));
            assert_eq!(got, expected, "{case:?}");
        }
    }

    proptest! {
        #![proptest_config(crate::test_support::proptest_config())]

        #[test]
        fn git_version_parse_never_panics(text in "(?s).{0,64}", digits in "[0-9.]{0,24}") {
            for output in [text.clone(), format!("git version {digits}{text}")] {
                if let Some(version) = GitVersion::parse(&output) {
                    prop_assert_eq!(version.raw.trim(), version.raw.as_str());
                }
            }
        }
    }

    /// `cwd`, args, raw exit status, stderr, and the message of the error.
    /// `{status}` stands for the exit status.
    const FAILURES: &[(&str, &[&str], i32, &str, &str)] = &[
        (
            "/tmp/somewhere",
            &["rev-parse", "--show-toplevel"],
            128,
            "fatal: not a git repository (or any of the parent directories): .git",
            "/tmp/somewhere is not inside a Git repository.",
        ),
        (
            "/tmp/plain",
            &["rev-parse", "--show-toplevel"],
            128,
            "fatal: Not a git repository (or any of the parent directories): .git",
            "/tmp/plain is not inside a Git repository.",
        ),
        (
            "/tmp/broken",
            &["rev-parse", "--is-bare-repository"],
            128,
            "fatal: gitfile does not point to a valid repository: /tmp/broken/.git",
            "/tmp/broken is not inside a Git repository.",
        ),
        (
            "/tmp/a\0b",
            &["status", "arg\0two"],
            1,
            "fatal: not a git repository\0still the same string",
            "/tmp/a\0b is not inside a Git repository.",
        ),
        // The words anywhere but the primary fatal are some other failure.
        (
            "/tmp/plain",
            &["rev-parse"],
            128,
            "fatal: cannot change to 'not a git repository': No such file or directory",
            "`git rev-parse` failed ({status}): \
             fatal: cannot change to 'not a git repository': No such file or directory",
        ),
        (
            "/cwd",
            &["status"],
            1,
            "note: this is not a git repository wording; the fault is permissions",
            "`git status` failed ({status}): \
             note: this is not a git repository wording; the fault is permissions",
        ),
        // The path comes from git's safe.directory hint, then from the quotes.
        (
            "/srv/shared/repo/sub",
            &["rev-parse"],
            128,
            "fatal: detected dubious ownership in repository at '/srv/shared/repo'\n\
             To add an exception for this directory, call:\n\n\
             \tgit config --global --add safe.directory /srv/shared/repo",
            "Git refuses to open /srv/shared/repo because it is owned by another user. \
             If you trust it, run: git config --global --add safe.directory '/srv/shared/repo'",
        ),
        (
            "/cwd",
            &["status", "--porcelain=v2"],
            128,
            "fatal: detected dubious ownership in repository at '/home/not a git repository'\n\
             To add an exception for this directory, call:\n\n\
             \tgit config --global --add safe.directory '/home/not a git repository'",
            "Git refuses to open /home/not a git repository because it is owned by another \
             user. If you trust it, run: git config --global --add safe.directory \
             '/home/not a git repository'",
        ),
        (
            "/cwd",
            &["rev-parse", "--show-toplevel"],
            128,
            "fatal: detected dubious ownership in repository at '/srv/o'brien/repo'\n\
             To add an exception for this directory, call:\n\n\
             \tgit config --global --add safe.directory '/srv/o'\\''brien/repo'",
            "Git refuses to open /srv/o'brien/repo because it is owned by another user. \
             If you trust it, run: git config --global --add safe.directory '/srv/o'\\''brien/repo'",
        ),
        (
            "/cwd",
            &["status"],
            128,
            "fatal: detected dubious ownership in repository at '/tmp/a\0b'",
            "Git refuses to open /tmp/a\0b because it is owned by another user. \
             If you trust it, run: git config --global --add safe.directory '/tmp/a\0b'",
        ),
        // Empty quotes are not a path: the directory git ran in is.
        (
            "/actual/cwd",
            &["status"],
            128,
            "fatal: detected dubious ownership in repository at ''",
            "Git refuses to open /actual/cwd because it is owned by another user. \
             If you trust it, run: git config --global --add safe.directory '/actual/cwd'",
        ),
        (
            "/repo",
            &["status", "--porcelain=v2"],
            9,
            "",
            "`git status --porcelain=v2` failed ({status})",
        ),
        ("", &[], 0, "", "`git ` failed ({status})"),
    ];

    #[test]
    fn classify_failure_table() {
        for &(cwd, args, raw, stderr, message) in FAILURES {
            let case = (cwd, args, stderr);
            let status = status_raw(raw);
            let error = classify_failure(Path::new(cwd), args, status, stderr);
            let expected = message.replace("{status}", &status.to_string());
            assert_eq!(error.to_string(), expected, "{case:?}");
        }
    }

    /// A pipe that answers each `read` with the next step, then EOF.
    struct Steps(&'static [Result<&'static [u8], io::ErrorKind>]);

    impl Read for Steps {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let Some((step, rest)) = self.0.split_first() else {
                return Ok(0);
            };
            self.0 = rest;
            let bytes = step.map_err(io::Error::from)?;
            buf[..bytes.len()].copy_from_slice(bytes);
            Ok(bytes.len())
        }
    }

    /// The reads of a pipe, the limit, and what `read_capped` returns:
    /// the bytes, `None` past the limit, or `Err` for a read error.
    type ReadCase = (
        &'static [Result<&'static [u8], io::ErrorKind>],
        usize,
        Result<Option<&'static [u8]>, ()>,
    );

    const READS: &[ReadCase] = &[
        (&[Ok(b"ab"), Ok(b"cd")], 4, Ok(Some(b"abcd"))),
        (&[Ok(b"abcd"), Ok(b"e")], 4, Ok(None)),
        (&[Ok(b"abcde")], 4, Ok(None)),
        (&[], 0, Ok(Some(b""))),
        // A mid-read error is not EOF; retrying it would end "cleanly".
        (&[Ok(b"x"), Err(io::ErrorKind::Other)], 64, Err(())),
        (&[Ok(b"abcd"), Err(io::ErrorKind::Other)], 4, Err(())),
        // An interrupted read is retried, also with the limit reached.
        (
            &[Ok(b"abcd"), Err(io::ErrorKind::Interrupted)],
            4,
            Ok(Some(b"abcd")),
        ),
    ];

    #[test]
    fn read_capped_table() {
        for &(steps, limit, expected) in READS {
            let got = read_capped(Steps(steps), limit).map_err(drop);
            let expected = expected.map(|bytes| bytes.map(<[u8]>::to_vec));
            assert_eq!(got, expected, "{steps:?} {limit}");
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

    /// An absolute directory on this host: `/usr/bin`, or `C:/usr/bin` on Windows.
    fn abs(dir: &str) -> String {
        if cfg!(windows) {
            format!("C:{dir}")
        } else {
            dir.to_owned()
        }
    }

    fn joined(dirs: &[&str]) -> OsString {
        std::env::join_paths(dirs).expect("PATH")
    }

    /// What a built command does to one variable: `None` leaves the inherited
    /// value, `Some(None)` removes it, `Some(Some(value))` sets it.
    type EnvChange = Option<Option<OsString>>;

    const INHERITED: EnvChange = None;
    const REMOVED: EnvChange = Some(None);

    fn set(value: impl Into<OsString>) -> EnvChange {
        Some(Some(value.into()))
    }

    fn env_change(command: &Command, name: &str) -> EnvChange {
        command
            .get_envs()
            .find(|(key, _)| key.to_str().is_some_and(|key| env_names_match(key, name)))
            .map(|(_, value)| value.map(OsStr::to_os_string))
    }

    #[test]
    fn git_runs_behind_the_hardening_prefix() {
        const PREFIX: [&str; 7] = [
            "--no-optional-locks",
            "--no-pager",
            "--no-replace-objects",
            "-c",
            "core.fsmonitor=",
            "-c",
            "log.showSignature=false",
        ];
        let cwd = Path::new("repo");
        // The caller's argv follows the prefix untouched, so an option-shaped
        // branch after `--` (see `switch_request_table`) stays an operand.
        let args = ["switch", "--no-guess", "--", "-f"];
        let mut command = Command::new("git");
        harden(&mut command, cwd, &args);
        let expected: Vec<&OsStr> = PREFIX.into_iter().chain(args).map(OsStr::new).collect();
        assert_eq!(command.get_args().collect::<Vec<_>>(), expected);
        assert_eq!(command.get_current_dir(), Some(cwd));
    }

    #[test]
    fn git_env_drops_inherited_git_vars_and_relative_path_entries() {
        let other_case = if cfg!(windows) { REMOVED } else { INHERITED };
        // Every case inherits all of these, each set to a hostile value.
        let hostile = [
            ("GIT_DIR", REMOVED),
            ("GIT_EXEC_PATH", REMOVED),
            ("GIT_CONFIG_COUNT", REMOVED),
            ("GIT_CONFIG_KEY_0", REMOVED),
            ("GIT_CONFIG_VALUE_0", REMOVED),
            ("GIT_CONFIG_PARAMETERS", REMOVED),
            ("GIT_TRACE", REMOVED),
            // git sets these for its children from the hardening prefix.
            ("GIT_OPTIONAL_LOCKS", REMOVED),
            ("GIT_PAGER", REMOVED),
            // Git for Windows reads these as `GIT_WORK_TREE` and `GIT_INDEX_FILE`.
            ("Git_Work_Tree", other_case.clone()),
            ("git_index_file", other_case),
            // ssh, askpass and tokens keep working.
            ("GIT_SSH", INHERITED),
            ("GIT_SSH_COMMAND", INHERITED),
            ("GIT_SSH_VARIANT", INHERITED),
            ("GIT_ASKPASS", INHERITED),
            ("git_ssh_command", INHERITED),
            ("GITHUB_TOKEN", INHERITED),
            ("GH_TOKEN", INHERITED),
            ("SSH_AUTH_SOCK", INHERITED),
            ("HOME", INHERITED),
            // Owned names replace the inherited value.
            ("GIT_TERMINAL_PROMPT", set("0")),
            ("LC_ALL", set("C")),
            ("LANGUAGE", set("C")),
        ];
        let dirs = ["/usr/bin", "/bin", "/custom/bin", "/brew/bin"].map(abs);
        let [usr, bin, custom, brew] = dirs.each_ref().map(String::as_str);
        let homebrew = macos_gui_path_dirs(|_| true);
        let on_windows =
            |windows: EnvChange, elsewhere| if cfg!(windows) { windows } else { elsewhere };
        // (also inherited, PATH extras, PATH, GIT_NO_LAZY_FETCH)
        let cases = [
            (vec![], vec![], INHERITED, set("1")),
            // A clean PATH and the user's lazy-fetch choice are inherited.
            (
                vec![("PATH", joined(&[usr])), ("GIT_NO_LAZY_FETCH", "0".into())],
                vec![],
                INHERITED,
                set("0"),
            ),
            // Finder's PATH gains Homebrew after the user's entries.
            (
                vec![("PATH", joined(&[usr, bin]))],
                homebrew,
                set(joined(&[usr, bin, MACOS_HOMEBREW_BIN, MACOS_USR_LOCAL_BIN])),
                set("1"),
            ),
            // An entry already on PATH keeps its place.
            (
                vec![("PATH", joined(&[custom, brew, usr]))],
                vec![brew.into()],
                INHERITED,
                set("1"),
            ),
            // Empty and relative entries would resolve inside the repository.
            (
                vec![("PATH", joined(&[usr, "", "./bin", "bin", bin]))],
                vec![brew.into()],
                set(joined(&[usr, bin, brew])),
                set("1"),
            ),
            (
                vec![("PATH", joined(&[".", usr]))],
                vec![],
                set(joined(&[usr])),
                set("1"),
            ),
            (
                vec![("PATH", joined(&[""]))],
                vec![brew.into()],
                set(joined(&[brew])),
                set("1"),
            ),
            (vec![], vec![brew.into()], set(joined(&[brew])), set("1")),
            // An empty PATH would search the working directory.
            (
                vec![("PATH", joined(&[".", ""]))],
                vec![],
                REMOVED,
                set("1"),
            ),
            // `Path` is the usual Windows spelling.
            (
                vec![
                    ("Path", joined(&[".", usr])),
                    ("Git_No_Lazy_Fetch", "0".into()),
                ],
                vec![],
                on_windows(set(joined(&[usr])), INHERITED),
                on_windows(set("0"), set("1")),
            ),
        ];
        for case in cases {
            let (also_inherited, path_extras, path, lazy_fetch) = &case;
            let parent_env = hostile
                .iter()
                .map(|(name, _)| (OsString::from(name), OsString::from("evil")))
                .chain(
                    also_inherited
                        .iter()
                        .map(|(name, value)| (OsString::from(name), value.clone())),
                );
            let command = command_with_env(Path::new("git"), parent_env, path_extras);
            let expected = hostile.iter().cloned().chain([
                ("PATH", path.clone()),
                ("GIT_NO_LAZY_FETCH", lazy_fetch.clone()),
            ]);
            for (name, change) in expected {
                assert_eq!(env_change(&command, name), change, "{case:?}: {name}");
            }
        }
    }
}
