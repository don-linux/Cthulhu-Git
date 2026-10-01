use std::fmt;
use std::path::{Path, PathBuf};

use super::exec::{Git, GitError};
use super::log::short_oid;

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
///
/// HEAD is read with `symbolic-ref` and `rev-parse`, not `git status`. Status
/// refreshes the index and runs clean/process filters from the repository, so
/// opening a hostile repo would execute commands planted in its config.
pub fn inspect(git: &Git, path: &Path) -> Result<RepoInfo, GitError> {
    let dir = require_directory(path)?;
    let root = repository_root(git, &dir)?;
    let name = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());
    let head = read_head(git, &root)?;
    Ok(RepoInfo { root, name, head })
}

fn require_directory(path: &Path) -> Result<PathBuf, GitError> {
    if !path.is_dir() {
        return Err(GitError::NotADirectory(path.to_path_buf()));
    }
    dunce::canonicalize(path).map_err(|_| GitError::NotADirectory(path.to_path_buf()))
}

fn repository_root(git: &Git, dir: &Path) -> Result<PathBuf, GitError> {
    let bare = git.run(dir, &["rev-parse", "--is-bare-repository"])?;
    if !bare.status.success() {
        return Err(super::exec::classify_failure(
            dir,
            &["rev-parse", "--is-bare-repository"],
            bare.status.to_string(),
            &bare.stderr,
        ));
    }

    if stdout_text(&bare.stdout) == "true" {
        let git_dir = git.require_ok(dir, &["rev-parse", "--absolute-git-dir"])?;
        return git_path(git_dir.stdout);
    }

    show_toplevel(git, dir)
}

fn show_toplevel(git: &Git, dir: &Path) -> Result<PathBuf, GitError> {
    let output = git.require_ok(dir, &["rev-parse", "--show-toplevel"])?;
    git_path(output.stdout)
}

fn git_path(stdout: Vec<u8>) -> Result<PathBuf, GitError> {
    let mut stdout = stdout;
    // Git adds one trailing newline (`\n` on Unix, `\r\n` on Windows). A
    // directory whose name itself ends in CR or LF must keep that byte.
    if stdout.last() == Some(&b'\n') {
        stdout.pop();
    }
    #[cfg(windows)]
    if stdout.last() == Some(&b'\r') {
        stdout.pop();
    }
    if stdout.is_empty() {
        return Err(GitError::Failed {
            command: "git rev-parse".to_owned(),
            status: "exit status: 0".to_owned(),
            stderr: "empty output".to_owned(),
        });
    }

    // Git for Windows prints `C:/...`; canonicalizing gives native separators.
    let root = path_from_bytes(stdout);
    Ok(dunce::canonicalize(&root).unwrap_or(root))
}

fn read_head(git: &Git, root: &Path) -> Result<Head, GitError> {
    let symbolic = git.run(root, &["symbolic-ref", "--quiet", "HEAD"])?;
    if symbolic.status.success() {
        let name = branch_name(&symbolic.stdout);
        let verified = git.run(root, &["rev-parse", "--verify", "--quiet", "HEAD"])?;
        return Ok(if verified.status.success() {
            Head::Branch(name)
        } else {
            Head::Unborn(name)
        });
    }

    // Detached HEAD: `symbolic-ref --quiet` exits 1 and prints nothing.
    if symbolic.status.code() == Some(1) && symbolic.stderr.is_empty() {
        return detached_head(git, root);
    }

    Err(super::exec::classify_failure(
        root,
        &["symbolic-ref", "--quiet", "HEAD"],
        symbolic.status.to_string(),
        &symbolic.stderr,
    ))
}

fn branch_name(stdout: &[u8]) -> String {
    let full = stdout_text(stdout);
    full.strip_prefix("refs/heads/").unwrap_or(&full).to_owned()
}

fn detached_head(git: &Git, root: &Path) -> Result<Head, GitError> {
    let output = git.require_ok(root, &["rev-parse", "--verify", "HEAD"])?;
    let oid = stdout_text(&output.stdout);
    if oid.is_empty() {
        return Err(GitError::Failed {
            command: "git rev-parse --verify HEAD".to_owned(),
            status: output.status.to_string(),
            stderr: "empty output".to_owned(),
        });
    }
    Ok(Head::Detached {
        short_oid: short_oid(&oid).to_owned(),
    })
}

fn stdout_text(stdout: &[u8]) -> String {
    let mut bytes = stdout.to_vec();
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    #[cfg(windows)]
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    String::from_utf8_lossy(&bytes).into_owned()
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
///
/// Production code reads HEAD with `symbolic-ref` instead, so this parser is
/// exercised by the unit and property tests only.
#[cfg(test)]
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
