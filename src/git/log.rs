use std::path::Path;

use super::exec::{Git, GitError};
use super::repo::Head;

/// Hash prefix length shown everywhere in the UI. Twelve hex digits is the
/// Linux kernel convention: unique in practice even in very large histories.
pub const SHORT_OID_LEN: usize = 12;

/// Separates the hash from the subject inside one `git log -z` record.
const FIELD_SEPARATOR: u8 = 0x1f;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub oid: String,
    /// First line of the commit message.
    pub summary: String,
}

/// The newest commits reachable from HEAD, newest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    pub commits: Vec<Commit>,
    /// More commits exist beyond the requested limit.
    pub truncated: bool,
}

impl Commit {
    pub fn short_oid(&self) -> &str {
        short_oid(&self.oid)
    }
}

impl History {
    pub fn latest(&self) -> Option<&Commit> {
        self.commits.first()
    }
}

/// The newest commit, with the fields the detail sidebar shows.
///
/// `message` is the raw commit message (`%B`): subject, a blank line, and the
/// body. Git's trailing record newline is not part of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitDetail {
    pub oid: String,
    pub author_name: String,
    pub author_email: String,
    pub authored_at: String,
    pub message: String,
}

/// Reads `HEAD`'s newest commit. An unborn branch has none, so git is not asked.
///
/// The record is not NUL-terminated. A NUL inside the message stays in
/// `message` instead of cutting the record short.
pub fn latest_commit(
    git: &Git,
    root: &Path,
    head: &Head,
) -> Result<Option<CommitDetail>, GitError> {
    if matches!(head, Head::Unborn(_)) {
        return Ok(None);
    }

    let args = [
        "log",
        "-1",
        "--date=format-local:%Y-%m-%d %H:%M",
        "--format=%H%x1f%an%x1f%ae%x1f%ad%x1f%B",
        "HEAD",
        "--",
    ];
    let output = git.require_ok(root, &args)?;
    parse_commit_detail(&output.stdout)
        .ok_or_else(|| GitError::Failed {
            command: format!("git {}", args.join(" ")),
            status: output.status.to_string(),
            stderr: "unrecognized log output".to_owned(),
        })
        .map(Some)
}

/// Parses one `git log -1 --format=%H%x1f%an%x1f%ae%x1f%ad%x1f%B` record.
/// The first four separators split the fixed fields; the rest is the message,
/// so a unit separator inside the message is kept.
pub(crate) fn parse_commit_detail(stdout: &[u8]) -> Option<CommitDetail> {
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

    let mut parts = record.splitn(5, |byte| *byte == FIELD_SEPARATOR);
    let oid = parts.next()?;
    let author_name = parts.next()?;
    let author_email = parts.next()?;
    let authored_at = parts.next()?;
    let message = parts.next()?;
    if oid.is_empty() || !oid.iter().all(u8::is_ascii_hexdigit) || authored_at.is_empty() {
        return None;
    }
    Some(CommitDetail {
        oid: String::from_utf8_lossy(oid).into_owned(),
        author_name: String::from_utf8_lossy(author_name).into_owned(),
        author_email: String::from_utf8_lossy(author_email).into_owned(),
        authored_at: String::from_utf8_lossy(authored_at).into_owned(),
        message: String::from_utf8_lossy(message).into_owned(),
    })
}

pub(crate) fn short_oid(oid: &str) -> &str {
    oid.get(..SHORT_OID_LEN).unwrap_or(oid)
}

/// Reads up to `limit` commits reachable from HEAD in the repository at `root`.
///
/// `head` comes from [`super::inspect`]; an unborn branch has no commits, so
/// git is not asked at all (it would fail instead of printing nothing).
pub fn history(git: &Git, root: &Path, head: &Head, limit: usize) -> Result<History, GitError> {
    if matches!(head, Head::Unborn(_)) || limit == 0 {
        return Ok(History::default());
    }

    // Git 2.55 parses --max-count as a 32-bit signed integer. A larger value
    // is rejected ("not an integer") instead of listing the commits.
    let max_count = format!(
        "--max-count={}",
        limit.saturating_add(1).min(i32::MAX as usize)
    );
    let args = [
        "log",
        &max_count,
        "-z",
        "--format=%H%x1f%s",
        "--no-color",
        "HEAD",
        "--",
    ];
    let output = git.require_ok(root, &args)?;
    let mut commits = parse_log(&output.stdout).ok_or_else(|| GitError::Failed {
        command: format!("git {}", args.join(" ")),
        status: output.status.to_string(),
        stderr: "unrecognized log output".to_owned(),
    })?;

    let truncated = commits.len() > limit;
    commits.truncate(limit);
    Ok(History { commits, truncated })
}

/// Parses `git log -z --format=%H%x1f%s`: NUL-terminated records of
/// `<oid> 0x1f <subject>`.
pub(crate) fn parse_log(stdout: &[u8]) -> Option<Vec<Commit>> {
    stdout
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .map(|record| {
            let separator = record.iter().position(|byte| *byte == FIELD_SEPARATOR)?;
            let (oid, summary) = (&record[..separator], &record[separator + 1..]);
            if oid.is_empty() || !oid.iter().all(u8::is_ascii_hexdigit) {
                return None;
            }
            Some(Commit {
                oid: String::from_utf8_lossy(oid).into_owned(),
                summary: String::from_utf8_lossy(summary).into_owned(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OID_A: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    const OID_B: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";

    fn record(oid: &str, summary: &[u8]) -> Vec<u8> {
        let mut out = oid.as_bytes().to_vec();
        out.push(FIELD_SEPARATOR);
        out.extend_from_slice(summary);
        out.push(0);
        out
    }

    #[test]
    fn parses_records_in_order() {
        let mut stdout = record(OID_A, b"Awaken the Old Ones");
        stdout.extend(record(OID_B, b"Initial commit"));
        assert_eq!(
            parse_log(&stdout),
            Some(vec![
                Commit {
                    oid: OID_A.to_owned(),
                    summary: "Awaken the Old Ones".to_owned()
                },
                Commit {
                    oid: OID_B.to_owned(),
                    summary: "Initial commit".to_owned()
                },
            ])
        );
    }

    #[test]
    fn empty_output_is_an_empty_history() {
        assert_eq!(parse_log(b""), Some(Vec::new()));
    }

    #[test]
    fn missing_trailing_nul_is_accepted() {
        let mut stdout = record(OID_A, b"Last");
        stdout.pop();
        assert_eq!(parse_log(&stdout).map(|commits| commits.len()), Some(1));
    }

    #[test]
    fn empty_subject_is_kept() {
        let commits = parse_log(&record(OID_A, b"")).expect("parsable");
        assert_eq!(commits[0].summary, "");
    }

    #[test]
    fn subject_may_contain_the_separator() {
        let commits = parse_log(&record(OID_A, b"a\x1fb")).expect("parsable");
        assert_eq!(commits[0].summary, "a\u{1f}b");
    }

    #[test]
    fn non_utf8_subject_is_replaced_not_rejected() {
        let commits = parse_log(&record(OID_A, b"caf\xe9")).expect("parsable");
        assert_eq!(commits[0].summary, "caf\u{fffd}");
    }

    #[test]
    fn malformed_records_are_rejected() {
        assert_eq!(parse_log(b"no separator\0"), None);
        assert_eq!(parse_log(b"\x1fsubject without oid\0"), None);
        assert_eq!(parse_log(b"not-hex\x1fsubject\0"), None);
    }

    fn detail_record(oid: &str, name: &str, email: &str, date: &str, message: &[u8]) -> Vec<u8> {
        let mut out = oid.as_bytes().to_vec();
        for field in [name.as_bytes(), email.as_bytes(), date.as_bytes()] {
            out.push(FIELD_SEPARATOR);
            out.extend_from_slice(field);
        }
        out.push(FIELD_SEPARATOR);
        out.extend_from_slice(message);
        out.push(b'\n');
        out
    }

    #[test]
    fn parses_commit_detail_and_keeps_a_separator_in_the_message() {
        let stdout = detail_record(
            OID_A,
            "cthulhu",
            "cthulhu@example.invalid",
            "2026-10-07 18:43",
            b"Second\n\nThe body\x1f stays.",
        );
        assert_eq!(
            parse_commit_detail(&stdout),
            Some(CommitDetail {
                oid: OID_A.to_owned(),
                author_name: "cthulhu".to_owned(),
                author_email: "cthulhu@example.invalid".to_owned(),
                authored_at: "2026-10-07 18:43".to_owned(),
                message: "Second\n\nThe body\u{1f} stays.".to_owned(),
            })
        );
    }

    #[test]
    fn commit_detail_keeps_a_nul_inside_the_message() {
        let stdout = detail_record(
            OID_A,
            "cthulhu",
            "cthulhu@example.invalid",
            "2026-10-07 18:43",
            b"subject with NUL\x00 and more",
        );
        let detail = parse_commit_detail(&stdout).expect("parsable");
        assert!(detail.message.contains('\u{0}'));
        assert!(detail.message.contains("and more"));
    }

    #[test]
    fn malformed_commit_detail_is_rejected() {
        assert_eq!(parse_commit_detail(b""), None);
        assert_eq!(parse_commit_detail(b"no separators\n"), None);
        assert_eq!(
            parse_commit_detail(&detail_record(
                "not-hex",
                "cthulhu",
                "cthulhu@example.invalid",
                "2026-10-07 18:43",
                b"msg"
            )),
            None
        );
        assert_eq!(
            parse_commit_detail(&detail_record(
                OID_A,
                "cthulhu",
                "cthulhu@example.invalid",
                "",
                b"msg"
            )),
            None
        );
    }

    #[test]
    fn short_oid_is_twelve_characters() {
        let commit = Commit {
            oid: OID_A.to_owned(),
            summary: String::new(),
        };
        assert_eq!(commit.short_oid(), "4b825dc642cb");
        assert_eq!(short_oid("abc"), "abc");
    }
}
