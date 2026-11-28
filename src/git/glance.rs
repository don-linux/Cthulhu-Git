//! A short read of HEAD for the home screen.
//!
//! [`super::repo::inspect`] supplies the branch. One `git log -1` supplies the
//! subject and date. `git rev-list --left-right --count` supplies ahead/behind
//! when the branch tracks an upstream. None of these run `git status`, which
//! would refresh the index and could run clean or process filters.

use std::path::Path;

use super::exec::{Cr, Git, GitError, trim_eol};
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
    // `%s` and not the detail's `%B`: the subject is all a card shows. Date
    // first, so a unit separator inside the subject stays in the subject.
    let latest = log::head_commit(
        git,
        &info.root,
        &info.head,
        "--format=%ad%x1f%s",
        parse_commit_glance,
    )?;
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
    let [when, subject] = log::split_fields(trim_eol(stdout, Cr::AfterLf))?;
    is_local_stamp(when).then(|| CommitGlance {
        when: String::from_utf8_lossy(when).into_owned(),
        subject: String::from_utf8_lossy(subject).into_owned(),
    })
}

/// The shape [`log::LOCAL_DATE`] prints, `YYYY-MM-DD HH:MM`: a digit for
/// every `0` of the template, the template's byte elsewhere.
fn is_local_stamp(stamp: &[u8]) -> bool {
    const TEMPLATE: &[u8] = b"0000-00-00 00:00";
    stamp.len() == TEMPLATE.len()
        && stamp.iter().zip(TEMPLATE).all(|(byte, shape)| match shape {
            b'0' => byte.is_ascii_digit(),
            _ => byte == shape,
        })
}

/// `ahead \t behind`. Anything else in either count, a line ending
/// included, fails to parse.
fn parse_divergence(stdout: &[u8]) -> Option<Divergence> {
    let text = std::str::from_utf8(trim_eol(stdout, Cr::AfterLf)).ok()?;
    let (ahead, behind) = text.split_once('\t')?;
    Some(Divergence {
        ahead: ahead.parse().ok()?,
        behind: behind.parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_glance_table() {
        for (stdout, expected) in [
            // The subject may hold the separator, or be empty.
            (
                &b"2026-11-10 01:23\x1fAwaken\x1fthe sleeper\n"[..],
                Some(("2026-11-10 01:23", "Awaken\u{1f}the sleeper")),
            ),
            (b"2026-11-10 01:23\x1f\n", Some(("2026-11-10 01:23", ""))),
            // Only the CR of a CRLF is git's line ending.
            (
                b"2026-11-10 01:23\x1fA\r\n",
                Some(("2026-11-10 01:23", "A")),
            ),
            (
                b"2026-11-10 01:23\x1fA\r",
                Some(("2026-11-10 01:23", "A\r")),
            ),
            (b"2026-11-10 01:23\n", None),
            (b"\x1fAwaken\n", None),
            (b"yesterday\x1fAwaken\n", None),
            (b"", None),
        ] {
            let expected = expected.map(|(when, subject)| CommitGlance {
                when: when.to_owned(),
                subject: subject.to_owned(),
            });
            assert_eq!(parse_commit_glance(stdout), expected, "{stdout:?}");
        }
    }

    #[test]
    fn divergence_table() {
        for (stdout, expected) in [
            (&b"2\t1\n"[..], Some((2, 1))),
            (b"0\t0", Some((0, 0))),
            (b"", None),
            (b"2\n", None),
            (b"2\t1\t0\n", None),
            (b"2\t-1\n", None),
            (b"ahead\t1\n", None),
            (b"1\t2\nextra\n", None),
        ] {
            let expected = expected.map(|(ahead, behind)| Divergence { ahead, behind });
            assert_eq!(parse_divergence(stdout), expected, "{stdout:?}");
        }
    }
}
