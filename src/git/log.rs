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

    // Git parses --max-count as a signed integer. usize::MAX does not fit and
    // the command fails instead of listing the commits.
    let max_count = format!(
        "--max-count={}",
        limit.saturating_add(1).min(i64::MAX as usize)
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
