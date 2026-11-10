//! A short read of HEAD for the home screen.
//!
//! [`super::repo::inspect`] supplies the branch. One `git log -1` supplies the
//! subject and date. `git rev-list --left-right --count` supplies ahead/behind
//! when the branch tracks an upstream. None of these run `git status`, which
//! would refresh the index and could run clean or process filters.

use std::path::Path;

use super::exec::{Git, GitError};
use super::log;
use super::repo::Head;

/// Branch, latest commit line, and upstream divergence for one repository.
///
/// `latest` is absent on an unborn branch. `upstream` is absent when HEAD is
/// detached, unborn, or has no upstream. A missing upstream is not an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoGlance {
    pub head: Head,
    pub latest: Option<CommitGlance>,
    pub upstream: Option<Divergence>,
}

/// Subject and local time of the newest commit on HEAD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitGlance {
    pub subject: String,
    /// `YYYY-MM-DD HH:MM` in the local timezone, the same stamp as the detail view.
    pub when: String,
}

/// Commits on HEAD that the upstream does not have, and the reverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Divergence {
    pub ahead: u64,
    pub behind: u64,
}

/// Reads [`RepoGlance`] for the repository that contains `path`.
pub fn glance(git: &Git, path: &Path) -> Result<RepoGlance, GitError> {
    let info = super::repo::inspect(git, path)?;
    let latest = commit_glance(git, &info.root, &info.head)?;
    let upstream = match info.head {
        Head::Branch(_) => upstream_divergence(git, &info.root)?,
        Head::Unborn(_) | Head::Detached { .. } => None,
    };
    Ok(RepoGlance {
        head: info.head,
        latest,
        upstream,
    })
}

fn commit_glance(git: &Git, root: &Path, head: &Head) -> Result<Option<CommitGlance>, GitError> {
    if matches!(head, Head::Unborn(_)) {
        return Ok(None);
    }

    // Date first, so a unit separator inside the subject stays in the subject.
    let args = [
        "log",
        "-1",
        "--date=format-local:%Y-%m-%d %H:%M",
        "--format=%ad%x1f%s",
        "--no-textconv",
        "--no-ext-diff",
        "HEAD",
        "--",
    ];
    let output = git.require_ok(root, &args)?;
    parse_commit_glance(&output.stdout)
        .ok_or_else(|| {
            super::exec::rejected(
                &format!("git {}", args.join(" ")),
                "unrecognized log output",
            )
        })
        .map(Some)
}

fn upstream_divergence(git: &Git, root: &Path) -> Result<Option<Divergence>, GitError> {
    let args = ["rev-list", "--left-right", "--count", "HEAD...@{upstream}"];
    let output = git.run(root, &args)?;
    // No upstream, or the upstream ref is gone. The branch and commit still stand.
    if !output.status.success() {
        return Ok(None);
    }
    parse_divergence(&output.stdout).map(Some).ok_or_else(|| {
        super::exec::rejected(
            &format!("git {}", args.join(" ")),
            "unrecognized rev-list output",
        )
    })
}

fn parse_commit_glance(stdout: &[u8]) -> Option<CommitGlance> {
    if stdout.is_empty() {
        return None;
    }
    let mut record = stdout.to_vec();
    if record.last() == Some(&b'\n') {
        record.pop();
        if record.last() == Some(&b'\r') {
            record.pop();
        }
    }
    let parts = log::split_fields(&record, 2)?;
    let when = parts[0];
    let subject = parts[1];
    if !is_local_stamp(when) {
        return None;
    }
    Some(CommitGlance {
        when: String::from_utf8_lossy(when).into_owned(),
        subject: String::from_utf8_lossy(subject).into_owned(),
    })
}

/// `YYYY-MM-DD HH:MM`, the shape `--date=format-local:%Y-%m-%d %H:%M` prints.
fn is_local_stamp(stamp: &[u8]) -> bool {
    if stamp.len() != 16 {
        return false;
    }
    let digit = |index: usize| stamp[index].is_ascii_digit();
    digit(0)
        && digit(1)
        && digit(2)
        && digit(3)
        && stamp[4] == b'-'
        && digit(5)
        && digit(6)
        && stamp[7] == b'-'
        && digit(8)
        && digit(9)
        && stamp[10] == b' '
        && digit(11)
        && digit(12)
        && stamp[13] == b':'
        && digit(14)
        && digit(15)
}

fn parse_divergence(stdout: &[u8]) -> Option<Divergence> {
    let mut record = stdout.to_vec();
    if record.last() == Some(&b'\n') {
        record.pop();
        if record.last() == Some(&b'\r') {
            record.pop();
        }
    }
    if record.is_empty() || record.contains(&b'\n') || record.contains(&b'\r') {
        return None;
    }
    let text = std::str::from_utf8(&record).ok()?;
    let (ahead, behind) = text.split_once('\t')?;
    if ahead.is_empty() || behind.is_empty() || behind.contains('\t') {
        return None;
    }
    Some(Divergence {
        ahead: ahead.parse().ok()?,
        behind: behind.parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_glance_keeps_a_separator_inside_the_subject() {
        let mut stdout = b"2026-11-10 01:23\x1fAwaken\x1fthe sleeper".to_vec();
        stdout.push(b'\n');
        assert_eq!(
            parse_commit_glance(&stdout),
            Some(CommitGlance {
                when: "2026-11-10 01:23".to_owned(),
                subject: "Awaken\u{1f}the sleeper".to_owned(),
            })
        );
    }

    #[test]
    fn commit_glance_allows_an_empty_subject() {
        assert_eq!(
            parse_commit_glance(b"2026-11-10 01:23\x1f\n"),
            Some(CommitGlance {
                when: "2026-11-10 01:23".to_owned(),
                subject: String::new(),
            })
        );
    }

    #[test]
    fn commit_glance_rejects_a_missing_separator_or_date() {
        assert_eq!(parse_commit_glance(b"2026-11-10 01:23\n"), None);
        assert_eq!(parse_commit_glance(b"\x1fAwaken\n"), None);
        assert_eq!(parse_commit_glance(b"yesterday\x1fAwaken\n"), None);
        assert_eq!(parse_commit_glance(b""), None);
    }

    #[test]
    fn divergence_parses_ahead_and_behind() {
        assert_eq!(
            parse_divergence(b"2\t1\n"),
            Some(Divergence {
                ahead: 2,
                behind: 1
            })
        );
        assert_eq!(
            parse_divergence(b"0\t0"),
            Some(Divergence {
                ahead: 0,
                behind: 0
            })
        );
    }

    #[test]
    fn divergence_rejects_malformed_counts() {
        assert_eq!(parse_divergence(b""), None);
        assert_eq!(parse_divergence(b"2\n"), None);
        assert_eq!(parse_divergence(b"2\t1\t0\n"), None);
        assert_eq!(parse_divergence(b"2\t-1\n"), None);
        assert_eq!(parse_divergence(b"ahead\t1\n"), None);
        assert_eq!(parse_divergence(b"1\t2\nextra\n"), None);
    }
}
