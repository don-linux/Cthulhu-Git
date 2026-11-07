//! Shared fixtures for the integration tests.
//!
//! Each integration-test crate compiles this file on its own. A helper that
//! one crate does not call is not dead code in the others, so unused items
//! are allowed here.
//!
//! Nothing in this module writes the process environment or the process
//! working directory. Git configuration is applied only to the [`Command`]
//! passed to [`isolate`].

#![allow(dead_code)]

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(not(target_os = "linux"))]
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

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

/// Set by the fake git in `setsid_grandchild_of_a_timed_out_git_is_dead`.
///
/// The value is the pid file. The fake re-execs the discover test binary,
/// which calls [`become_setsid_sleeper`]. macOS does not guarantee perl, and
/// this does not shell out to the `setsid` binary.
pub const SETSID_PIDFILE_VAR: &str = "CTHULHU_HARNESS_SETSID_PIDFILE";

const SETSID_SLEEP: Duration = Duration::from_secs(30);

/// A private directory. The prefix is passed to [`TempDir::with_prefix`].
pub fn scratch(prefix: &str) -> TempDir {
    TempDir::with_prefix(prefix).unwrap_or_else(|error| panic!("temp dir {prefix}: {error}"))
}

/// `dunce::canonicalize`, which drops the Windows `\\?\` prefix.
pub fn canonical(path: &Path) -> PathBuf {
    dunce::canonicalize(path)
        .unwrap_or_else(|error| panic!("canonicalize {}: {error}", path.display()))
}

/// Canonicalizes both paths, then compares them.
#[track_caller]
pub fn assert_same_path(actual: &Path, expected: &Path) {
    assert_eq!(canonical(actual), canonical(expected));
}

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
            "core.fsmonitor=false",
            "-c",
            "credential.helper=",
            "-c",
            "core.autocrlf=false",
            "-c",
        ])
        .arg(hooks_arg);
}

/// `git init -b main` through [`isolate`], then a local `core.autocrlf=false`
/// so later commands that do not pass `-c` still stay on LF.
pub fn init_repo(git: &Path, scratch: &Path) -> PathBuf {
    let root = scratch.join("repo");
    fs::create_dir_all(&root).unwrap_or_else(|error| panic!("repo dir: {error}"));
    run_isolated(git, scratch, &root, &["init", "-b", "main"]);
    run_isolated(
        git,
        scratch,
        &root,
        &["config", "--local", "core.autocrlf", "false"],
    );
    root
}

fn run_isolated(git: &Path, scratch: &Path, cwd: &Path, args: &[&str]) {
    let mut command = Command::new(git);
    command.current_dir(cwd);
    isolate(&mut command, scratch);
    command.args(args);
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("run git {args:?}: {error}"));
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
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
/// The body is POSIX `sh`. A missing shebang is given `#!/bin/sh`. The file
/// is published with a rename so a concurrent exec keeps the previous inode
/// (`ETXTBSY`).
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
    let staged = path.with_extension("tmp");
    let text = if body.starts_with("#!") {
        body.to_owned()
    } else {
        format!("#!/bin/sh\n{body}")
    };
    {
        let mut file = File::create(&staged)
            .unwrap_or_else(|error| panic!("write {}: {error}", staged.display()));
        file.write_all(text.as_bytes())
            .unwrap_or_else(|error| panic!("write {}: {error}", staged.display()));
        let _ = file.sync_all();
    }
    set_executable(&staged);
    fs::rename(&staged, &path)
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

/// Drops ACLs that would grant access despite a mode of `000` or `0555`.
///
/// macOS uses `/bin/chmod -N`. Linux uses `setfacl -b` when that binary is
/// installed. A missing tool is left alone; the caller probes the resulting
/// permission with `File::open` or `OpenOptions`.
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
    let mut file = match File::create(pid_file) {
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
