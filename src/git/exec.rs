use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

/// `--no-optional-locks` needs Git 2.15; porcelain v2 status needs 2.11.
pub const MIN_GIT_VERSION: (u32, u32, u32) = (2, 15, 0);

const VERSION_PREFIX: &str = "git version ";

/// Variables that make git operate on a repository other than the one in `cwd`,
/// e.g. when the app is launched from a hook or from a shell that exported them.
const REPO_LOCATION_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_PREFIX",
];

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

#[derive(Debug)]
pub enum GitError {
    NotFound {
        tried: Vec<PathBuf>,
    },
    BadOverride(PathBuf),
    TooOld(GitVersion),
    NotADirectory(PathBuf),
    NotARepository(PathBuf),
    UnsafeRepository(PathBuf),
    Spawn(io::Error),
    Failed {
        command: String,
        status: String,
        stderr: String,
    },
}

impl Git {
    /// Asks `path --version` and returns it without checking the version floor.
    pub(crate) fn probe(path: &Path) -> Result<Self, GitError> {
        let output = capture(command(path).arg("--version"))?;
        let failed = |stderr: String| GitError::Failed {
            command: "git --version".to_owned(),
            status: output.status.to_string(),
            stderr,
        };

        if !output.status.success() {
            return Err(failed(output.stderr.clone()));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let version = GitVersion::parse(&stdout)
            .ok_or_else(|| failed(format!("unrecognized version output: {}", stdout.trim())))?;

        Ok(Self {
            path: path.to_path_buf(),
            version,
        })
    }

    /// Runs `git <args>` in `cwd` without a shell and without touching the index.
    ///
    /// The user's environment is inherited (credential helpers, ssh-agent) except
    /// for variables that would redirect git to another repository.
    pub fn run(&self, cwd: &Path, args: &[&str]) -> Result<GitOutput, GitError> {
        capture(
            command(&self.path)
                .current_dir(cwd)
                .arg("--no-optional-locks")
                .arg("--no-pager")
                .args(["-c", "core.fsmonitor=false"])
                .args(["-c", "log.showSignature=false"])
                .args(args),
        )
    }

    /// Like [`Git::run`], but a non-zero exit becomes a typed [`GitError`].
    pub fn require_ok(&self, cwd: &Path, args: &[&str]) -> Result<GitOutput, GitError> {
        let output = self.run(cwd, args)?;

        if output.status.success() {
            return Ok(output);
        }

        Err(classify_failure(
            cwd,
            args,
            output.status.to_string(),
            &output.stderr,
        ))
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
        self.numbers() >= MIN_GIT_VERSION
    }
}

impl fmt::Display for GitVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { tried } => {
                write!(
                    f,
                    "Git was not found. Install Git or set CTHULHU_GIT to the full path of the git executable."
                )?;
                if !tried.is_empty() {
                    let tried: Vec<_> = tried
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect();
                    write!(f, " Searched: {}", tried.join(", "))?;
                }
                Ok(())
            }
            Self::BadOverride(path) => write!(
                f,
                "CTHULHU_GIT is set to {}, which is not an executable file.",
                path.display()
            ),
            Self::TooOld(version) => {
                let (major, minor, patch) = MIN_GIT_VERSION;
                write!(
                    f,
                    "Git {version} is too old; version {major}.{minor}.{patch} or newer is required."
                )
            }
            Self::NotADirectory(path) => write!(f, "{} is not a directory.", path.display()),
            Self::NotARepository(path) => {
                write!(f, "{} is not inside a Git repository.", path.display())
            }
            Self::UnsafeRepository(path) => write!(
                f,
                "Git refuses to open {0} because it is owned by another user. \
                 If you trust it, run: git config --global --add safe.directory {0}",
                path.display()
            ),
            Self::Spawn(error) => write!(f, "Could not run git: {error}"),
            Self::Failed {
                command,
                status,
                stderr,
            } => {
                write!(f, "`{command}` failed ({status})")?;
                if !stderr.is_empty() {
                    write!(f, ": {stderr}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for GitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(error) => Some(error),
            _ => None,
        }
    }
}

/// The single place where git processes are built, so every invocation
/// (including `--version`) gets the same environment and window flags.
fn command(program: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_PAGER", "cat")
        // The "C" locale exists everywhere and keeps the stderr we match on in English.
        .env("LC_ALL", "C")
        .env("LANGUAGE", "C");

    for var in REPO_LOCATION_VARS {
        command.env_remove(var);
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    command
}

fn capture(command: &mut Command) -> Result<GitOutput, GitError> {
    let output = command.output().map_err(GitError::Spawn)?;
    Ok(GitOutput {
        stdout: output.stdout,
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        status: output.status,
    })
}

pub(crate) fn classify_failure(
    cwd: &Path,
    args: &[&str],
    status: String,
    stderr: &str,
) -> GitError {
    if stderr.contains("not a git repository") {
        return GitError::NotARepository(cwd.to_path_buf());
    }

    if stderr.contains("detected dubious ownership") {
        let path = dubious_ownership_path(stderr).unwrap_or_else(|| cwd.to_path_buf());
        return GitError::UnsafeRepository(path);
    }

    GitError::Failed {
        command: format!("git {}", args.join(" ")),
        status,
        stderr: stderr.to_owned(),
    }
}

/// Extracts `<path>` from "fatal: detected dubious ownership in repository at '<path>'".
fn dubious_ownership_path(stderr: &str) -> Option<PathBuf> {
    let (_, rest) = stderr.split_once("repository at '")?;
    let (path, _) = rest.split_once('\'')?;
    Some(PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            "exit status: 128".to_owned(),
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
            "exit status: 128".to_owned(),
            stderr,
        );
        let GitError::UnsafeRepository(path) = &error else {
            panic!("expected UnsafeRepository, got {error:?}");
        };
        assert_eq!(path, Path::new("/srv/shared/repo"));
        assert!(
            error
                .to_string()
                .contains("git config --global --add safe.directory /srv/shared/repo")
        );
    }

    #[test]
    fn other_failures_keep_command_status_and_stderr() {
        let error = classify_failure(
            Path::new("/repo"),
            &["status", "--porcelain=v2"],
            "signal: 9 (SIGKILL)".to_owned(),
            "",
        );
        assert_eq!(
            error.to_string(),
            "`git status --porcelain=v2` failed (signal: 9 (SIGKILL))"
        );
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
