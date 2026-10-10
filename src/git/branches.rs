//! Local branches (`refs/heads`) and how each one stands against its upstream.
//!
//! `for-each-ref` does not expand `%xNN` the way `git log --format` does, so
//! the field separator is a literal unit separator in the format string.
//! Branch names cannot contain ASCII control characters, and `%(upstream:track)`
//! is a fixed phrase, so that byte cannot appear in a field.

use std::path::Path;

use super::exec::{Git, GitError};
use super::repo::Head;

const FIELD_SEPARATOR: u8 = 0x1f;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    /// Full hash of the commit this branch points at. `None` when the branch
    /// has no commits yet.
    pub oid: Option<String>,
    /// Commits reachable from the branch tip, including ancestors.
    pub commit_count: u64,
    pub upstream: Upstream,
    /// This branch is what HEAD points at.
    pub current: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Upstream {
    None,
    /// `branch.<name>.merge` is set, but the upstream ref is gone.
    Gone {
        name: String,
    },
    Tracking {
        name: String,
        ahead: u64,
        behind: u64,
    },
}

struct ParsedRef {
    name: String,
    oid: Option<String>,
    upstream: Upstream,
}

/// Lists local branches. `HEAD` on a branch that has no commits yet is included
/// with a count of zero, because that ref does not exist until the first commit.
///
/// Commit counts are one `git rev-list --count` per branch, on the caller's
/// thread. Opening a repository already runs git off the UI thread.
pub fn branches(git: &Git, root: &Path, head: &Head) -> Result<Vec<Branch>, GitError> {
    let separator = FIELD_SEPARATOR as char;
    let format = format!(
        "--format=%(refname:short){separator}%(objectname){separator}%(upstream:short){separator}%(upstream:track)"
    );
    let args = ["for-each-ref", format.as_str(), "refs/heads"];
    let output = git.require_ok(root, &args)?;
    let refs = parse_refs(&output.stdout).ok_or_else(|| GitError::Failed {
        command: format!("git {}", args.join(" ")),
        status: output.status.to_string(),
        stderr: "unrecognized for-each-ref output".to_owned(),
    })?;

    let mut rows = Vec::with_capacity(refs.len());
    for parsed in refs {
        let commit_count = commit_count(git, root, &parsed.name)?;
        rows.push(Branch {
            name: parsed.name,
            oid: parsed.oid,
            commit_count,
            upstream: parsed.upstream,
            current: false,
        });
    }
    Ok(finish(rows, head))
}

fn commit_count(git: &Git, root: &Path, name: &str) -> Result<u64, GitError> {
    let spec = format!("refs/heads/{name}");
    // The revision is a full ref, so it cannot be read as an option. `--`
    // belongs after the revision; putting it first makes git treat the ref as
    // a path and reject the command.
    let args = ["rev-list", "--count", spec.as_str()];
    let output = git.require_ok(root, &args)?;
    parse_count(&output.stdout).ok_or_else(|| GitError::Failed {
        command: format!("git {}", args.join(" ")),
        status: output.status.to_string(),
        stderr: "unrecognized rev-list output".to_owned(),
    })
}

fn parse_count(stdout: &[u8]) -> Option<u64> {
    let mut bytes = stdout.to_vec();
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(&bytes).ok()?.parse().ok()
}

/// `name SEP oid SEP upstream SEP track`, one branch per line.
fn parse_refs(stdout: &[u8]) -> Option<Vec<ParsedRef>> {
    let mut refs = Vec::new();
    for mut line in stdout.split(|byte| *byte == b'\n') {
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        if line.is_empty() {
            continue;
        }
        refs.push(parse_ref_line(line)?);
    }
    Some(refs)
}

fn parse_ref_line(line: &[u8]) -> Option<ParsedRef> {
    let mut parts = line.splitn(4, |byte| *byte == FIELD_SEPARATOR);
    let name = parts.next()?;
    let oid = parts.next()?;
    let upstream = parts.next()?;
    let track = parts.next()?;
    if name.is_empty() {
        return None;
    }
    let oid = if oid.is_empty() {
        None
    } else if oid.iter().all(u8::is_ascii_hexdigit) {
        Some(String::from_utf8_lossy(oid).into_owned())
    } else {
        return None;
    };
    let name = String::from_utf8_lossy(name).into_owned();
    let upstream = String::from_utf8_lossy(upstream).into_owned();
    let track = String::from_utf8_lossy(track).into_owned();
    Some(ParsedRef {
        name,
        oid,
        upstream: classify_upstream(&upstream, &track)?,
    })
}

/// `%(upstream:track)` is empty when there is no upstream and when the branch
/// is even with its upstream. `[gone]` means the upstream ref disappeared.
fn classify_upstream(name: &str, track: &str) -> Option<Upstream> {
    if track == "[gone]" {
        return Some(Upstream::Gone {
            name: name.to_owned(),
        });
    }
    if name.is_empty() && track.is_empty() {
        return Some(Upstream::None);
    }
    let (ahead, behind) = if track.is_empty() {
        (0, 0)
    } else {
        parse_ahead_behind(track)?
    };
    Some(Upstream::Tracking {
        name: name.to_owned(),
        ahead,
        behind,
    })
}

/// Git prints `[ahead N]`, `[behind M]`, or `[ahead N, behind M]`.
fn parse_ahead_behind(track: &str) -> Option<(u64, u64)> {
    let inner = track.strip_prefix('[')?.strip_suffix(']')?;
    if let Some(rest) = inner.strip_prefix("ahead ") {
        if let Some((ahead, behind)) = rest.split_once(", behind ") {
            return Some((ahead.parse().ok()?, behind.parse().ok()?));
        }
        return Some((rest.parse().ok()?, 0));
    }
    let behind = inner.strip_prefix("behind ")?;
    if behind.contains("ahead ") {
        return None;
    }
    Some((0, behind.parse().ok()?))
}

fn finish(mut branches: Vec<Branch>, head: &Head) -> Vec<Branch> {
    for branch in &mut branches {
        branch.current = false;
    }
    let current_name = match head {
        Head::Branch(name) | Head::Unborn(name) => Some(name.as_str()),
        Head::Detached { .. } => None,
    };
    if let Some(name) = current_name {
        match branches.iter_mut().find(|branch| branch.name == name) {
            Some(branch) => branch.current = true,
            None => branches.push(Branch {
                name: name.to_owned(),
                oid: None,
                commit_count: 0,
                upstream: Upstream::None,
                current: true,
            }),
        }
    }
    branches.sort_by(|left, right| {
        right
            .current
            .cmp(&left.current)
            .then_with(|| left.name.cmp(&right.name))
    });
    branches
}

#[cfg(test)]
mod tests {
    use super::*;

    const OID: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

    fn line(name: &str, upstream: &str, track: &str) -> Vec<u8> {
        line_with_oid(name, OID, upstream, track)
    }

    fn line_with_oid(name: &str, oid: &str, upstream: &str, track: &str) -> Vec<u8> {
        let mut out = name.as_bytes().to_vec();
        for field in [oid, upstream, track] {
            out.push(FIELD_SEPARATOR);
            out.extend_from_slice(field.as_bytes());
        }
        out.push(b'\n');
        out
    }

    fn parsed(stdout: &[u8]) -> Vec<ParsedRef> {
        parse_refs(stdout).expect("parsable")
    }

    #[test]
    fn parses_ahead_behind_gone_and_in_sync() {
        let mut stdout = line("feature", "main", "[ahead 1, behind 2]");
        stdout.extend(line("topic", "origin/topic", "[ahead 4]"));
        stdout.extend(line("old", "origin/old", "[behind 3]"));
        stdout.extend(line("even", "origin/even", ""));
        stdout.extend(line("local", "", ""));
        stdout.extend(line("missing", "origin/missing", "[gone]"));

        let refs = parsed(&stdout);
        assert_eq!(refs.len(), 6);
        assert_eq!(refs[0].oid.as_deref(), Some(OID));
        assert_eq!(
            refs[0].upstream,
            Upstream::Tracking {
                name: "main".to_owned(),
                ahead: 1,
                behind: 2
            }
        );
        assert_eq!(
            refs[1].upstream,
            Upstream::Tracking {
                name: "origin/topic".to_owned(),
                ahead: 4,
                behind: 0
            }
        );
        assert_eq!(
            refs[2].upstream,
            Upstream::Tracking {
                name: "origin/old".to_owned(),
                ahead: 0,
                behind: 3
            }
        );
        assert_eq!(
            refs[3].upstream,
            Upstream::Tracking {
                name: "origin/even".to_owned(),
                ahead: 0,
                behind: 0
            }
        );
        assert_eq!(refs[4].upstream, Upstream::None);
        assert_eq!(
            refs[5].upstream,
            Upstream::Gone {
                name: "origin/missing".to_owned()
            }
        );
    }

    #[test]
    fn empty_output_is_no_branches() {
        assert_eq!(parse_refs(b"").map(|refs| refs.len()), Some(0));
        assert_eq!(parse_refs(b"\n").map(|refs| refs.len()), Some(0));
    }

    #[test]
    fn windows_carriage_return_is_stripped() {
        let mut stdout = line("feature", "main", "[ahead 1]");
        stdout.pop();
        stdout.extend_from_slice(b"\r\n");
        let refs = parsed(&stdout);
        assert_eq!(refs[0].name, "feature");
        assert_eq!(
            refs[0].upstream,
            Upstream::Tracking {
                name: "main".to_owned(),
                ahead: 1,
                behind: 0
            }
        );
    }

    #[test]
    fn malformed_lines_are_rejected() {
        assert!(parse_refs(b"no separators\n").is_none());
        assert!(parse_refs(b"\x1fmain\x1f\n").is_none());
        assert!(parse_refs(&line("feature", "main", "[ahead]")).is_none());
        assert!(parse_refs(&line("feature", "main", "ahead 1")).is_none());
        assert!(parse_refs(&line("feature", "main", "[behind 1, ahead 2]")).is_none());
        assert!(parse_refs(&line_with_oid("feature", "not-hex", "main", "")).is_none());
        // The previous three-field record is no longer a complete branch.
        assert!(parse_refs(b"feature\x1fmain\x1f\n").is_none());
    }

    #[test]
    fn empty_oid_is_a_branch_with_no_commit() {
        let refs = parsed(&line_with_oid("main", "", "", ""));
        assert_eq!(refs[0].oid, None);
    }

    #[test]
    fn counts_are_unsigned_integers() {
        assert_eq!(parse_count(b"12\n"), Some(12));
        assert_eq!(parse_count(b"0\r\n"), Some(0));
        assert_eq!(parse_count(b"\n"), None);
        assert_eq!(parse_count(b"12 commits\n"), None);
    }

    fn sample(name: &str, count: u64) -> Branch {
        Branch {
            name: name.to_owned(),
            oid: None,
            commit_count: count,
            upstream: Upstream::None,
            current: false,
        }
    }

    #[test]
    fn current_branch_is_first_and_unborn_is_inserted() {
        let rows = vec![sample("feature", 2), sample("main", 1)];
        let finished = finish(rows, &Head::Branch("feature".to_owned()));
        assert_eq!(finished[0].name, "feature");
        assert!(finished[0].current);
        assert_eq!(finished[1].name, "main");
        assert!(!finished[1].current);

        let unborn = finish(Vec::new(), &Head::Unborn("main".to_owned()));
        assert_eq!(
            unborn,
            vec![Branch {
                name: "main".to_owned(),
                oid: None,
                commit_count: 0,
                upstream: Upstream::None,
                current: true,
            }]
        );
    }

    #[test]
    fn detached_head_marks_no_branch_current() {
        let finished = finish(
            vec![sample("zeta", 1), sample("alpha", 3)],
            &Head::Detached {
                short_oid: "0123456789ab".to_owned(),
            },
        );
        assert_eq!(
            finished
                .iter()
                .map(|branch| branch.name.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "zeta"]
        );
        assert!(finished.iter().all(|branch| !branch.current));
    }
}
