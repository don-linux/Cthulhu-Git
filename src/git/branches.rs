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
    /// Committer time of that commit, seconds since the Unix epoch.
    /// `None` when the branch has no commits yet.
    pub committer_time: Option<i64>,
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
        remote: bool,
    },
    Tracking {
        name: String,
        /// The upstream is a remote-tracking ref (`refs/remotes/…`), which a
        /// fetch can update. A branch that tracks a local branch
        /// (`branch.<name>.remote = .`) has nothing to fetch.
        remote: bool,
        ahead: u64,
        behind: u64,
    },
}

struct ParsedRef {
    name: String,
    oid: Option<String>,
    committer_time: Option<i64>,
    upstream: Upstream,
}

/// Lists local branches. `HEAD` on a branch that has no commits yet is included
/// with a count of zero, because that ref does not exist until the first commit.
///
/// Commit counts are one `git rev-list --count` per branch, on the caller's
/// thread. Opening a repository already runs git off the UI thread.
pub fn branches(git: &Git, root: &Path, head: &Head) -> Result<Vec<Branch>, GitError> {
    let separator = FIELD_SEPARATOR as char;
    // `lstrip=2`, not `short`: `short` prints `heads/v1` for a branch that
    // shares its name with a tag, which is not a branch name.
    let format = format!(
        "--format=%(refname:lstrip=2){separator}%(objectname){separator}%(upstream:short){separator}%(upstream){separator}%(upstream:track){separator}%(committerdate:unix)"
    );
    let args = ["for-each-ref", format.as_str(), "refs/heads"];
    let output = git.require_ok(root, &args)?;
    let refs = parse_refs(&output.stdout).ok_or_else(|| {
        super::exec::rejected(
            &format!("git {}", args.join(" ")),
            "unrecognized for-each-ref output",
        )
    })?;

    let mut rows = Vec::with_capacity(refs.len());
    for parsed in refs {
        let commit_count = commit_count(git, root, &parsed.name)?;
        rows.push(Branch {
            name: parsed.name,
            oid: parsed.oid,
            committer_time: parsed.committer_time,
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
    parse_count(&output.stdout).ok_or_else(|| {
        super::exec::rejected(
            &format!("git {}", args.join(" ")),
            "unrecognized rev-list output",
        )
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

/// `name SEP oid SEP upstream SEP upstream-ref SEP track SEP committer-time`,
/// one branch per line.
/// The committer time is Unix seconds, empty when the ref has no commit.
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
    let mut parts = line.splitn(6, |byte| *byte == FIELD_SEPARATOR);
    let name = parts.next()?;
    let oid = parts.next()?;
    let upstream = parts.next()?;
    let remote = parts.next()?.starts_with(b"refs/remotes/");
    let track = parts.next()?;
    let committer_time = parts.next()?;
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
        committer_time: parse_committer_time(committer_time)?,
        upstream: classify_upstream(&upstream, remote, &track)?,
    })
}

/// Empty when the branch has no commit. Anything that is not an integer rejects
/// the whole record.
fn parse_committer_time(bytes: &[u8]) -> Option<Option<i64>> {
    if bytes.is_empty() {
        return Some(None);
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let digits = text.strip_prefix('-').unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(Some(text.parse().ok()?))
}

/// `%(upstream:track)` is empty when there is no upstream and when the branch
/// is even with its upstream. `[gone]` means the upstream ref disappeared.
fn classify_upstream(name: &str, remote: bool, track: &str) -> Option<Upstream> {
    if track == "[gone]" {
        return Some(Upstream::Gone {
            name: name.to_owned(),
            remote,
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
        remote,
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
                committer_time: None,
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
    use crate::test_support::OID_A;

    /// The upstream ref is derived from the short name: with a `/` it is a
    /// remote-tracking ref, without one a local branch.
    fn line(name: &str, oid: &str, upstream: &str, track: &str, when: &str) -> Vec<u8> {
        let full = match upstream {
            "" => String::new(),
            short if short.contains('/') => format!("refs/remotes/{short}"),
            short => format!("refs/heads/{short}"),
        };
        let mut out = name.as_bytes().to_vec();
        for field in [oid, upstream, &full, track, when] {
            out.push(FIELD_SEPARATOR);
            out.extend_from_slice(field.as_bytes());
        }
        out.push(b'\n');
        out
    }

    fn tracked(name: &str, upstream: &str, track: &str) -> Vec<u8> {
        line(name, OID_A, upstream, track, "1700000000")
    }

    fn tracking(name: &str, remote: bool, ahead: u64, behind: u64) -> Upstream {
        Upstream::Tracking {
            name: name.to_owned(),
            remote,
            ahead,
            behind,
        }
    }

    #[test]
    fn parse_refs_table() {
        let dated = Some(1_700_000_000);
        let mut crlf = tracked("feature", "main", "[ahead 1]");
        crlf.pop();
        crlf.extend_from_slice(b"\r\n");
        let cases = [
            (
                [
                    tracked("feature", "main", "[ahead 1, behind 2]"),
                    tracked("topic", "origin/topic", "[ahead 4]"),
                    tracked("old", "origin/old", "[behind 3]"),
                    tracked("even", "origin/even", ""),
                    tracked("local", "", ""),
                    tracked("missing", "origin/missing", "[gone]"),
                ]
                .concat(),
                Some(vec![
                    ("feature", Some(OID_A), dated, tracking("main", false, 1, 2)),
                    (
                        "topic",
                        Some(OID_A),
                        dated,
                        tracking("origin/topic", true, 4, 0),
                    ),
                    (
                        "old",
                        Some(OID_A),
                        dated,
                        tracking("origin/old", true, 0, 3),
                    ),
                    (
                        "even",
                        Some(OID_A),
                        dated,
                        tracking("origin/even", true, 0, 0),
                    ),
                    ("local", Some(OID_A), dated, Upstream::None),
                    (
                        "missing",
                        Some(OID_A),
                        dated,
                        Upstream::Gone {
                            name: "origin/missing".to_owned(),
                            remote: true,
                        },
                    ),
                ]),
            ),
            (b"".to_vec(), Some(vec![])),
            (b"\n".to_vec(), Some(vec![])),
            (
                crlf,
                Some(vec![(
                    "feature",
                    Some(OID_A),
                    dated,
                    tracking("main", false, 1, 0),
                )]),
            ),
            (
                line("old", OID_A, "", "", "-10"),
                Some(vec![("old", Some(OID_A), Some(-10), Upstream::None)]),
            ),
            // A branch with no commit yet has no oid and no time.
            (
                line("main", "", "", "", ""),
                Some(vec![("main", None, None, Upstream::None)]),
            ),
            (b"no separators\n".to_vec(), None),
            (b"\x1fmain\x1f\n".to_vec(), None),
            (tracked("feature", "main", "[ahead]"), None),
            (tracked("feature", "main", "ahead 1"), None),
            (tracked("feature", "main", "[behind 1, ahead 2]"), None),
            (line("feature", "not-hex", "main", "", "1"), None),
            (line("feature", OID_A, "", "", "yesterday"), None),
            // A record from before the committer time was added is incomplete.
            (format!("feature\x1f{OID_A}\x1f\x1f\n").into_bytes(), None),
            (b"feature\x1fmain\x1f\n".to_vec(), None),
        ];
        for (stdout, expected) in cases {
            let parsed = parse_refs(&stdout).map(|refs| {
                refs.into_iter()
                    .map(|parsed| {
                        (
                            parsed.name,
                            parsed.oid,
                            parsed.committer_time,
                            parsed.upstream,
                        )
                    })
                    .collect::<Vec<_>>()
            });
            let expected = expected.map(|refs| {
                refs.into_iter()
                    .map(|(name, oid, time, upstream)| {
                        (name.to_owned(), oid.map(str::to_owned), time, upstream)
                    })
                    .collect::<Vec<_>>()
            });
            assert_eq!(parsed, expected, "{stdout:?}");
        }
    }

    #[test]
    fn counts_are_unsigned_integers() {
        for (stdout, count) in [
            (&b"12\n"[..], Some(12)),
            (b"0\r\n", Some(0)),
            (b"\n", None),
            (b"12 commits\n", None),
        ] {
            assert_eq!(parse_count(stdout), count, "{stdout:?}");
        }
    }

    fn branch(name: &str, count: u64, current: bool) -> Branch {
        Branch {
            name: name.to_owned(),
            oid: None,
            committer_time: None,
            commit_count: count,
            upstream: Upstream::None,
            current,
        }
    }

    /// The current branch comes first, an unborn one is added, and
    /// detached `HEAD` marks none; the rest keep name order.
    #[test]
    fn finish_puts_the_current_branch_first() {
        // `alpha` comes in marked current from an earlier listing.
        let rows = || vec![branch("zeta", 1, false), branch("alpha", 3, true)];
        for (head, expected) in [
            (
                Head::Branch("zeta".to_owned()),
                vec![branch("zeta", 1, true), branch("alpha", 3, false)],
            ),
            (
                Head::Unborn("main".to_owned()),
                vec![
                    branch("main", 0, true),
                    branch("alpha", 3, false),
                    branch("zeta", 1, false),
                ],
            ),
            (
                Head::Detached {
                    short_oid: "0123456789ab".to_owned(),
                },
                vec![branch("alpha", 3, false), branch("zeta", 1, false)],
            ),
        ] {
            assert_eq!(finish(rows(), &head), expected, "{head:?}");
        }
    }
}
