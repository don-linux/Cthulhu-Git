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

/// Someone named on a commit: its author, or a `Co-authored-by` trailer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Person {
    pub name: String,
    pub email: String,
    /// Other addresses of the same person, from a co-author trailer that
    /// names the author again. Avatars try them after `email`.
    pub aliases: Vec<String>,
}

impl Person {
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            email: email.into(),
            aliases: Vec::new(),
        }
    }

    /// Trimmed and lowercased, the form duplicates and avatars are matched on.
    pub fn email_key(&self) -> String {
        email_key(&self.email)
    }

    /// `email` first, then the aliases, without duplicates.
    pub fn emails(&self) -> Vec<String> {
        let mut keys: Vec<String> = Vec::new();
        for email in std::iter::once(&self.email).chain(&self.aliases) {
            let key = email_key(email);
            if !key.is_empty() && !keys.contains(&key) {
                keys.push(key);
            }
        }
        keys
    }
}

pub fn email_key(email: &str) -> String {
    email.trim().to_lowercase()
}

/// `Co-authored-by: Name <email>` lines, in order, one per address.
///
/// Matches what VS Code's Git extension accepts: the key is case-insensitive,
/// the line may sit anywhere in `text`, and a line without a name or an
/// address is skipped.
pub fn parse_co_authors(text: &str) -> Vec<Person> {
    const KEY: &str = "co-authored-by:";
    let mut found: Vec<Person> = Vec::new();
    // A bare CR and the Unicode separators end a line too, as they do for
    // the `m` flag of VS Code's regular expression.
    for line in text.split(['\n', '\r', '\u{2028}', '\u{2029}']) {
        let Some(head) = line.get(..KEY.len()) else {
            continue;
        };
        if !head.eq_ignore_ascii_case(KEY) {
            continue;
        }
        let Some(person) = parse_name_and_email(&line[KEY.len()..]) else {
            continue;
        };
        let key = person.email_key();
        if !found.iter().any(|known| known.email_key() == key) {
            found.push(person);
        }
    }
    found
}

/// `Name <email>`, where the address is the last `<…>` and holds no `>`.
fn parse_name_and_email(value: &str) -> Option<Person> {
    let value = value.trim();
    let inner = value.strip_suffix('>')?;
    let open = inner
        .char_indices()
        .filter(|(_, c)| *c == '<')
        .map(|(index, _)| index)
        .find(|&index| !inner[index + 1..].contains('>'))?;
    let name = inner[..open].trim();
    let email = inner[open + 1..].trim();
    if name.is_empty() || email.is_empty() {
        return None;
    }
    Some(Person::new(name, email))
}

/// Co-authors that are not the author. A trailer with one of the author's
/// addresses is dropped; one with the author's name but another address adds
/// that address to `author.aliases`.
pub fn fold_co_authors(author: &mut Person, co_authors: Vec<Person>) -> Vec<Person> {
    let recorded = author.clone();
    fold_with_recorded(author, &recorded, co_authors)
}

/// [`fold_co_authors`] when `.mailmap` may have rewritten the author:
/// `recorded` is the identity written in the commit (`%an <%ae>`). Its address
/// becomes an alias, and a trailer naming it is the author too.
pub(crate) fn fold_with_recorded(
    author: &mut Person,
    recorded: &Person,
    co_authors: Vec<Person>,
) -> Vec<Person> {
    let recorded_key = recorded.email_key();
    if !recorded_key.is_empty() && !author.emails().contains(&recorded_key) {
        author.aliases.push(recorded.email.clone());
    }
    let names: Vec<String> = [&author.name, &recorded.name]
        .iter()
        .map(|name| name.trim().to_lowercase())
        .filter(|name| !name.is_empty())
        .collect();
    let mut kept = Vec::new();
    let author_key = author.email_key();
    for person in co_authors {
        let key = person.email_key();
        if key == author_key || author.emails().contains(&key) {
            continue;
        }
        if names.contains(&person.name.trim().to_lowercase()) {
            author.aliases.push(person.email);
            continue;
        }
        kept.push(person);
    }
    kept
}

/// The mapped author (`%aN <%aE>`) and the recorded one (`%an <%ae>`), with
/// the co-authors from `trailers` folded into them.
pub(crate) fn author_and_co_authors(fields: [&[u8]; 4], trailers: &str) -> (Person, Vec<Person>) {
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    let mut author = Person::new(text(fields[0]), text(fields[1]));
    let recorded = Person::new(text(fields[2]), text(fields[3]));
    let co_authors = fold_with_recorded(&mut author, &recorded, parse_co_authors(trailers));
    (author, co_authors)
}

/// Ends `%(trailers:only,unfold)`, which comes before the free-form message.
const TRAILERS_END: u8 = 0x1e;

/// `trailers <0x1e> rest`, split at the first record separator.
fn split_trailers(bytes: &[u8]) -> Option<(String, &[u8])> {
    let end = bytes.iter().position(|byte| *byte == TRAILERS_END)?;
    Some((
        String::from_utf8_lossy(&bytes[..end]).into_owned(),
        &bytes[end + 1..],
    ))
}

/// The newest commit, with the fields the detail sidebar shows.
///
/// `message` is the raw commit message (`%B`): subject, a blank line, and the
/// body. Git's trailing record newline is not part of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitDetail {
    pub oid: String,
    /// `%aN <%aE>`, so `.mailmap` applies.
    pub author: Person,
    /// From the `Co-authored-by` trailers, without the author: the same
    /// block the history reads, so both list the same people.
    pub co_authors: Vec<Person>,
    pub authored_at: String,
    pub message: String,
}

const DETAIL_FORMAT: &str =
    "--format=%H%x1f%aN%x1f%aE%x1f%an%x1f%ae%x1f%ad%x1f%(trailers:only,unfold)%x1e%B";

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
        DETAIL_FORMAT,
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

/// Parses one [`DETAIL_FORMAT`] record. The first six unit separators split
/// the fixed fields, the first record separator ends the trailer block, and
/// the rest is the message, so separators inside the message are kept.
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

    let mut parts = record.splitn(7, |byte| *byte == FIELD_SEPARATOR);
    let oid = parts.next()?;
    let identity = [parts.next()?, parts.next()?, parts.next()?, parts.next()?];
    let authored_at = parts.next()?;
    let (trailers, message) = split_trailers(parts.next()?)?;
    if oid.is_empty() || !oid.iter().all(u8::is_ascii_hexdigit) || authored_at.is_empty() {
        return None;
    }
    let message = String::from_utf8_lossy(message).into_owned();
    let (author, co_authors) = author_and_co_authors(identity, &trailers);
    Some(CommitDetail {
        oid: String::from_utf8_lossy(oid).into_owned(),
        author,
        co_authors,
        authored_at: String::from_utf8_lossy(authored_at).into_owned(),
        message,
    })
}

/// `N files changed, N insertions(+), N deletions(-)`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommitStats {
    pub files: u64,
    pub insertions: u64,
    pub deletions: u64,
}

/// Everything the history hover shows that the graph log does not carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitHover {
    pub oid: String,
    pub author: Person,
    pub co_authors: Vec<Person>,
    /// Author time, Unix seconds.
    pub authored_unix: i64,
    /// Author time in the local zone, `YYYY-MM-DD HH:MM`.
    pub authored_at: String,
    /// Full message, without git's trailing newline.
    pub message: String,
    /// `None` for a merge or a commit that changes nothing.
    pub stats: Option<CommitStats>,
}

/// Ends the message; the short stat follows it.
const RECORD_END: u8 = 0x1d;

const HOVER_FORMAT: &str =
    "--format=%H%x1f%aN%x1f%aE%x1f%an%x1f%ae%x1f%at%x1f%ad%x1f%(trailers:only,unfold)%x1e%B%x1d";

/// Reads one commit for the hover: author, dates, full message and the
/// short stat, like VS Code's `git show -s --shortstat`. Asked only when the
/// hover opens, because `--shortstat` over the whole history would be slow.
pub fn commit_hover(git: &Git, root: &Path, oid: &str) -> Result<CommitHover, GitError> {
    if oid.is_empty() || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(GitError::Failed {
            command: "git show".to_owned(),
            status: "invalid oid".to_owned(),
            stderr: format!("{oid} is not a commit hash"),
        });
    }
    let args = [
        "show",
        "--no-color",
        "--shortstat",
        "--date=format-local:%Y-%m-%d %H:%M",
        HOVER_FORMAT,
        oid,
        "--",
    ];
    let output = git.require_ok(root, &args)?;
    parse_commit_hover(&output.stdout).ok_or_else(|| GitError::Failed {
        command: format!("git {}", args.join(" ")),
        status: output.status.to_string(),
        stderr: "unrecognized show output".to_owned(),
    })
}

/// Fixed fields split by the first seven unit separators, the trailer block
/// up to the first record separator, the message up to the last group
/// separator, and the short stat after it.
pub(crate) fn parse_commit_hover(stdout: &[u8]) -> Option<CommitHover> {
    let end = stdout.iter().rposition(|byte| *byte == RECORD_END)?;
    let (record, stat) = (&stdout[..end], &stdout[end + 1..]);
    let mut parts = record.splitn(8, |byte| *byte == FIELD_SEPARATOR);
    let oid = parts.next()?;
    let identity = [parts.next()?, parts.next()?, parts.next()?, parts.next()?];
    let unix = parts.next()?;
    let authored_at = parts.next()?;
    let (trailers, message) = split_trailers(parts.next()?)?;
    if oid.is_empty() || !oid.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let authored_unix = std::str::from_utf8(unix).ok()?.trim().parse().ok()?;
    let message = String::from_utf8_lossy(message)
        .trim_end_matches(['\n', '\r'])
        .to_owned();
    let (author, co_authors) = author_and_co_authors(identity, &trailers);
    Some(CommitHover {
        oid: String::from_utf8_lossy(oid).into_owned(),
        author,
        co_authors,
        authored_unix,
        authored_at: String::from_utf8_lossy(authored_at).into_owned(),
        message,
        stats: parse_shortstat(&String::from_utf8_lossy(stat)),
    })
}

/// Git prints this line in English because every call runs with `LC_ALL=C`.
pub(crate) fn parse_shortstat(text: &str) -> Option<CommitStats> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| line.contains("changed"))?;
    let mut stats = CommitStats::default();
    let mut saw_files = false;
    for part in line.split(',') {
        let part = part.trim();
        let (number, rest) = part.split_once(' ')?;
        let number: u64 = number.parse().ok()?;
        if rest.starts_with("file") {
            stats.files = number;
            saw_files = true;
        } else if rest.starts_with("insertion") {
            stats.insertions = number;
        } else if rest.starts_with("deletion") {
            stats.deletions = number;
        }
    }
    saw_files.then_some(stats)
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

    /// What `%(trailers:only)` prints for these test messages: their
    /// `Co-authored-by` lines.
    fn trailer_lines(message: &[u8]) -> Vec<u8> {
        String::from_utf8_lossy(message)
            .lines()
            .filter(|line| line.to_lowercase().starts_with("co-authored-by:"))
            .flat_map(|line| format!("{line}\n").into_bytes())
            .collect()
    }

    fn detail_record(oid: &str, name: &str, email: &str, date: &str, message: &[u8]) -> Vec<u8> {
        let mut out = oid.as_bytes().to_vec();
        for field in [
            name.as_bytes(),
            email.as_bytes(),
            name.as_bytes(),
            email.as_bytes(),
            date.as_bytes(),
        ] {
            out.push(FIELD_SEPARATOR);
            out.extend_from_slice(field);
        }
        out.push(FIELD_SEPARATOR);
        out.extend_from_slice(&trailer_lines(message));
        out.push(TRAILERS_END);
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
                author: Person::new("cthulhu", "cthulhu@example.invalid"),
                co_authors: Vec::new(),
                authored_at: "2026-10-07 18:43".to_owned(),
                message: "Second\n\nThe body\u{1f} stays.".to_owned(),
            })
        );
    }

    #[test]
    fn commit_detail_lists_co_authors_without_the_author() {
        let stdout = detail_record(
            OID_A,
            "Fernando Diaz",
            "fernando@example.invalid",
            "2026-10-07 18:43",
            b"Subject\n\nBody\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\n\
              Co-authored-by: Fernando Diaz <don-linux@users.noreply.github.com>\n\
              co-authored-by: Fernando Diaz <FERNANDO@example.invalid>",
        );
        let detail = parse_commit_detail(&stdout).expect("parsable");
        assert_eq!(
            detail.co_authors,
            [Person::new("Cursor Agent", "cursoragent@cursor.com")]
        );
        assert_eq!(
            detail.author.aliases,
            ["don-linux@users.noreply.github.com"]
        );
        assert_eq!(
            detail.author.emails(),
            [
                "fernando@example.invalid",
                "don-linux@users.noreply.github.com"
            ]
        );
    }

    #[test]
    fn co_author_lines_follow_the_vs_code_rule() {
        let text = "Co-authored-by: Ada Voss <ada@miskatonic.test>\n\
                    CO-AUTHORED-BY:Herbert West   <west@arkham.test>   \n\
                    Co-authored-by: Ada Voss <ADA@miskatonic.test>\n\
                    Co-authored-by: <nameless@arkham.test>\n\
                    Co-authored-by: No Address\n\
                    Co-authored-by: Empty <>\n\
                    Reviewed-by: Someone <someone@arkham.test>\n\
                    Co-authored-by: Odd <Name> <odd@arkham.test>\n\
                    Co-authored-by: Ünïcödé Ñame <unicode@arkham.test>";
        let people = parse_co_authors(text);
        assert_eq!(
            people,
            [
                Person::new("Ada Voss", "ada@miskatonic.test"),
                Person::new("Herbert West", "west@arkham.test"),
                Person::new("Odd <Name>", "odd@arkham.test"),
                Person::new("Ünïcödé Ñame", "unicode@arkham.test"),
            ]
        );
    }

    #[test]
    fn co_author_key_must_start_the_line() {
        assert!(parse_co_authors("  Co-authored-by: A <a@b>").is_empty());
        assert!(parse_co_authors("Co-authored").is_empty());
        assert!(parse_co_authors("").is_empty());
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

    fn hover_record(message: &[u8], stat: &[u8]) -> Vec<u8> {
        let mut out = OID_A.as_bytes().to_vec();
        for field in [
            b"Ada Voss".as_slice(),
            b"ada@miskatonic.test",
            b"Ada Voss",
            b"ada@miskatonic.test",
            b"1792598400",
            b"2026-10-21 11:10",
        ] {
            out.push(FIELD_SEPARATOR);
            out.extend_from_slice(field);
        }
        out.push(FIELD_SEPARATOR);
        out.extend_from_slice(&trailer_lines(message));
        out.push(TRAILERS_END);
        out.extend_from_slice(message);
        out.push(RECORD_END);
        out.extend_from_slice(stat);
        out
    }

    #[test]
    fn parses_the_hover_record_and_its_short_stat() {
        let stdout = hover_record(
            b"Seal the vault\n\nBody with \x1f, \x1e and \x1d inside.\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\n",
            b"\n\n 3 files changed, 42 insertions(+), 7 deletions(-)\n",
        );
        let hover = parse_commit_hover(&stdout).expect("parsable");
        assert_eq!(hover.oid, OID_A);
        assert_eq!(hover.author, Person::new("Ada Voss", "ada@miskatonic.test"));
        assert_eq!(
            hover.co_authors,
            [Person::new("Cursor Agent", "cursoragent@cursor.com")]
        );
        assert_eq!(hover.authored_unix, 1_792_598_400);
        assert_eq!(hover.authored_at, "2026-10-21 11:10");
        assert!(
            hover
                .message
                .starts_with("Seal the vault\n\nBody with \u{1f}")
        );
        assert!(hover.message.ends_with("<cursoragent@cursor.com>"));
        assert_eq!(
            hover.stats,
            Some(CommitStats {
                files: 3,
                insertions: 42,
                deletions: 7,
            })
        );
    }

    #[test]
    fn short_stat_variants() {
        assert_eq!(
            parse_shortstat(" 1 file changed, 1 insertion(+)"),
            Some(CommitStats {
                files: 1,
                insertions: 1,
                deletions: 0,
            })
        );
        assert_eq!(
            parse_shortstat(" 2 files changed, 5 deletions(-)\n"),
            Some(CommitStats {
                files: 2,
                insertions: 0,
                deletions: 5,
            })
        );
        assert_eq!(parse_shortstat(""), None);
        assert_eq!(parse_shortstat("\n\n"), None);
        assert_eq!(parse_shortstat("x files changed"), None);
        let merge = hover_record(b"Merge branch 'topic'\n", b"\n");
        assert_eq!(parse_commit_hover(&merge).expect("merge").stats, None);
    }

    #[test]
    fn malformed_hover_records_are_rejected() {
        assert_eq!(parse_commit_hover(b""), None);
        assert_eq!(parse_commit_hover(b"no record end"), None);
        let mut bad_time = hover_record(b"m", b"");
        let at = bad_time
            .windows(10)
            .position(|window| window == b"1792598400")
            .expect("time");
        bad_time[at] = b'x';
        assert_eq!(parse_commit_hover(&bad_time), None);
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
