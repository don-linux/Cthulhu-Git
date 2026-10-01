use std::fmt;
use std::path::{Path, PathBuf};

use super::exec::{Git, GitError};
use super::log::short_oid;

const STATUS_ARGS: &[&str] = &[
    "status",
    "--porcelain=v2",
    "--branch",
    "-z",
    "--untracked-files=no",
    "--ignore-submodules",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    Branch(String),
    /// A branch with no commits yet, e.g. right after `git init`.
    Unborn(String),
    Detached {
        short_oid: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInfo {
    pub root: PathBuf,
    pub name: String,
    pub head: Head,
}

impl fmt::Display for Head {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Branch(name) => f.write_str(name),
            Self::Unborn(name) => write!(f, "{name} (no commits yet)"),
            Self::Detached { short_oid } => write!(f, "Detached HEAD at {short_oid}"),
        }
    }
}

/// Resolves the repository containing `path` and reads its name and current branch.
pub fn inspect(git: &Git, path: &Path) -> Result<RepoInfo, GitError> {
    let dir = require_directory(path)?;
    let root = show_toplevel(git, &dir)?;
    let name = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());

    let status = git.require_ok(&root, STATUS_ARGS)?;
    let head = parse_head(&status.stdout).ok_or_else(|| GitError::Failed {
        command: format!("git {}", STATUS_ARGS.join(" ")),
        status: status.status.to_string(),
        stderr: "output has no branch header".to_owned(),
    })?;

    Ok(RepoInfo { root, name, head })
}

fn require_directory(path: &Path) -> Result<PathBuf, GitError> {
    if !path.is_dir() {
        return Err(GitError::NotADirectory(path.to_path_buf()));
    }
    dunce::canonicalize(path).map_err(|_| GitError::NotADirectory(path.to_path_buf()))
}

fn show_toplevel(git: &Git, dir: &Path) -> Result<PathBuf, GitError> {
    let output = git.require_ok(dir, &["rev-parse", "--show-toplevel"])?;
    let mut stdout = output.stdout;
    while stdout
        .last()
        .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
    {
        stdout.pop();
    }
    if stdout.is_empty() {
        return Err(GitError::Failed {
            command: "git rev-parse --show-toplevel".to_owned(),
            status: output.status.to_string(),
            stderr: "empty output".to_owned(),
        });
    }

    // Git for Windows prints `C:/...`; canonicalizing gives native separators.
    let root = path_from_bytes(stdout);
    Ok(dunce::canonicalize(&root).unwrap_or(root))
}

#[cfg(unix)]
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_vec(bytes))
}

#[cfg(not(unix))]
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
}

/// Reads `# branch.oid` / `# branch.head` from `git status --porcelain=v2 --branch -z`.
pub(crate) fn parse_head(stdout: &[u8]) -> Option<Head> {
    let mut oid = None;
    let mut head = None;

    for record in stdout.split(|byte| *byte == 0) {
        let Some(header) = record.strip_prefix(b"# ") else {
            continue;
        };
        let header = String::from_utf8_lossy(header);
        if let Some(value) = header.strip_prefix("branch.oid ") {
            oid = Some(value.to_owned());
        } else if let Some(value) = header.strip_prefix("branch.head ") {
            head = Some(value.to_owned());
        }
    }

    let head = head?;
    let oid = oid?;
    Some(match (head.as_str(), oid.as_str()) {
        ("(detached)", "(initial)") => return None,
        ("(detached)", oid) => Head::Detached {
            short_oid: short_oid(oid).to_owned(),
        },
        (_, "(initial)") => Head::Unborn(head),
        _ => Head::Branch(head),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OID: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

    fn status(records: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for record in records {
            out.extend_from_slice(record.as_bytes());
            out.push(0);
        }
        out
    }

    #[test]
    fn branch_header() {
        let stdout = status(&[
            &format!("# branch.oid {OID}"),
            "# branch.head feature/necronomicon",
            "# branch.upstream origin/feature/necronomicon",
            "# branch.ab +1 -2",
            "1 .M N... 100644 100644 100644 aaaa bbbb README.md",
        ]);
        assert_eq!(
            parse_head(&stdout),
            Some(Head::Branch("feature/necronomicon".to_owned()))
        );
    }

    #[test]
    fn detached_header_uses_short_oid() {
        let stdout = status(&[&format!("# branch.oid {OID}"), "# branch.head (detached)"]);
        assert_eq!(
            parse_head(&stdout),
            Some(Head::Detached {
                short_oid: "4b825dc642cb".to_owned()
            })
        );
    }

    #[test]
    fn unborn_header() {
        let stdout = status(&["# branch.oid (initial)", "# branch.head main"]);
        assert_eq!(parse_head(&stdout), Some(Head::Unborn("main".to_owned())));
    }

    #[test]
    fn missing_headers_are_rejected() {
        assert_eq!(parse_head(b""), None);
        assert_eq!(parse_head(&status(&["# branch.head main"])), None);
        assert_eq!(parse_head(&status(&[&format!("# branch.oid {OID}")])), None);
    }

    #[test]
    fn head_display_matches_ui_wording() {
        assert_eq!(Head::Branch("main".to_owned()).to_string(), "main");
        assert_eq!(
            Head::Unborn("main".to_owned()).to_string(),
            "main (no commits yet)"
        );
        assert_eq!(
            Head::Detached {
                short_oid: "abc1234def56".to_owned()
            }
            .to_string(),
            "Detached HEAD at abc1234def56"
        );
    }
}
