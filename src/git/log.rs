use std::path::Path;

use super::exec::{Git, GitError};
use super::repo::Head;

/// Hash prefix length shown everywhere in the UI. Twelve hex digits is the
/// Linux kernel convention: unique in practice even in very large histories.
pub const SHORT_OID_LEN: usize = 12;

/// Separates fields inside one `git log -z` record. Shared by the graph,
/// detail, and hover parsers. Each parser still cuts the trailer byte its
/// own way: the graph subject is before the trailers, the message is after.
pub(crate) const FIELD_SEPARATOR: u8 = 0x1f;

/// The first `count` fields of `record`, split on [`FIELD_SEPARATOR`].
/// The last field keeps any further separators, which is where a subject or
/// a message is allowed to contain `0x1f`.
pub(crate) fn split_fields(record: &[u8], count: usize) -> Option<Vec<&[u8]>> {
    let mut parts = record.splitn(count, |byte| *byte == FIELD_SEPARATOR);
    let mut fields = Vec::with_capacity(count);
    for _ in 0..count {
        fields.push(parts.next()?);
    }
    Some(fields)
}

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
        .ok_or_else(|| {
            super::exec::rejected(
                &format!("git {}", args.join(" ")),
                "unrecognized log output",
            )
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

    let parts = split_fields(&record, 7)?;
    let oid = parts[0];
    let identity = [parts[1], parts[2], parts[3], parts[4]];
    let authored_at = parts[5];
    let (trailers, message) = split_trailers(parts[6])?;
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

/// One path from `git show --numstat -z`.
///
/// A rename or a copy stores the old path in `previous` and the new one in
/// `path`. A deletion has only `path`. `insertions` and `deletions` are
/// `None` for a binary file, where git prints `-`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitFile {
    pub path: String,
    pub previous: Option<String>,
    pub insertions: Option<u64>,
    pub deletions: Option<u64>,
}

impl CommitFile {
    pub fn is_binary(&self) -> bool {
        self.insertions.is_none() || self.deletions.is_none()
    }

    /// Path passed to `git show -- <path>`. For a deletion this is the path
    /// that was removed; for a rename it is the post-image path.
    pub fn diff_path(&self) -> &str {
        &self.path
    }
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
    /// Files the commit changes against its first parent. Empty when nothing
    /// changed. A merge lists what landed, not a combined diff.
    pub files: Vec<CommitFile>,
    /// Summed from `files`. `None` when that list is empty.
    pub stats: Option<CommitStats>,
    /// Tags that point at this commit, annotated or lightweight. Empty when
    /// the lookup fails, so the rest of the commit still shows.
    pub tags: Vec<String>,
}

/// Ends the message; the short stat follows it.
const RECORD_END: u8 = 0x1d;

const HOVER_FORMAT: &str =
    "--format=%H%x1f%aN%x1f%aE%x1f%an%x1f%ae%x1f%at%x1f%ad%x1f%(trailers:only,unfold)%x1e%B%x1d";

/// Reads one commit for the hover: author, dates, full message, the files it
/// changes and the tags that point at it. Asked only when the hover opens,
/// because `--numstat` over the whole history would be slow.
///
/// `-m --first-parent` makes a merge list what landed on the first parent, as
/// a normal diff. A commit with one parent is unchanged. Tag lookup is a
/// second local command; if it fails the commit is still returned.
pub fn commit_hover(git: &Git, root: &Path, oid: &str) -> Result<CommitHover, GitError> {
    if !is_commit_oid(oid) {
        return Err(super::exec::rejected(
            "git show",
            format!("{oid} is not a commit hash"),
        ));
    }
    let args = [
        "show",
        "--no-color",
        "-m",
        "--first-parent",
        "--numstat",
        "-z",
        "--date=format-local:%Y-%m-%d %H:%M",
        HOVER_FORMAT,
        oid,
        "--",
    ];
    let output = git.require_ok(root, &args)?;
    let mut hover = parse_commit_hover(&output.stdout).ok_or_else(|| {
        super::exec::rejected(
            &format!("git {}", args.join(" ")),
            "unrecognized show output",
        )
    })?;
    hover.tags = commit_tags(git, root, &hover.oid);
    Ok(hover)
}

fn is_commit_oid(oid: &str) -> bool {
    !oid.is_empty() && oid.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Tags that point at `oid`. A failure becomes an empty list.
///
/// `for-each-ref` has no `-z` (not in Git 2.15, and not in 2.53 either).
/// A ref name cannot contain a newline, so one short name per line keeps a
/// comma inside the name.
fn commit_tags(git: &Git, root: &Path, oid: &str) -> Vec<String> {
    let points_at = format!("--points-at={oid}");
    let args = [
        "for-each-ref",
        points_at.as_str(),
        "--format=%(refname:short)",
        "refs/tags",
    ];
    match git.require_ok(root, &args) {
        Ok(output) => parse_tag_names(&output.stdout),
        Err(_) => Vec::new(),
    }
}

/// One tag name per line. A NUL is also a separator, so `%00` in the format
/// is accepted. A comma inside a name stays.
pub(crate) fn parse_tag_names(stdout: &[u8]) -> Vec<String> {
    stdout
        .split(|byte| matches!(byte, 0 | b'\n'))
        .map(|name| {
            let name = name.strip_suffix(b"\r").unwrap_or(name);
            String::from_utf8_lossy(name).trim().to_owned()
        })
        .filter(|name| !name.is_empty())
        .collect()
}

/// Fixed fields split by the first seven unit separators, the trailer block
/// up to the first record separator, the message up to the last group
/// separator, and the numstat after it.
///
/// The group separator ends the message. Numstat paths are not expected to
/// contain that byte; one that does is rejected with the rest of the record.
pub(crate) fn parse_commit_hover(stdout: &[u8]) -> Option<CommitHover> {
    let end = stdout.iter().rposition(|byte| *byte == RECORD_END)?;
    let (record, stat) = (&stdout[..end], &stdout[end + 1..]);
    let parts = split_fields(record, 8)?;
    let oid = parts[0];
    let identity = [parts[1], parts[2], parts[3], parts[4]];
    let unix = parts[5];
    let authored_at = parts[6];
    let (trailers, message) = split_trailers(parts[7])?;
    if oid.is_empty() || !oid.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let authored_unix = std::str::from_utf8(unix).ok()?.trim().parse().ok()?;
    let message = String::from_utf8_lossy(message)
        .trim_end_matches(['\n', '\r'])
        .to_owned();
    let (author, co_authors) = author_and_co_authors(identity, &trailers);
    let files = parse_numstat(stat)?;
    let stats = stats_from_files(&files);
    Some(CommitHover {
        oid: String::from_utf8_lossy(oid).into_owned(),
        author,
        co_authors,
        authored_unix,
        authored_at: String::from_utf8_lossy(authored_at).into_owned(),
        message,
        files,
        stats,
        tags: Vec::new(),
    })
}

/// `added \t deleted \t path \0`, or for a rename `added \t deleted \t \0 old \0 new \0`.
///
/// `git show -z` puts a NUL, then a newline, between the format and this
/// list. Both are skipped. `-` is a binary file. An empty slice is an empty list.
pub(crate) fn parse_numstat(mut bytes: &[u8]) -> Option<Vec<CommitFile>> {
    while bytes
        .first()
        .is_some_and(|byte| matches!(byte, b'\n' | b'\r' | 0))
    {
        bytes = &bytes[1..];
    }
    let mut files = Vec::new();
    while !bytes.is_empty() {
        let (insertions, rest) = take_count(bytes)?;
        let rest = rest.strip_prefix(b"\t")?;
        let (deletions, rest) = take_count(rest)?;
        let rest = rest.strip_prefix(b"\t")?;
        let (path, rest) = split_nul(rest)?;
        let (path, previous, rest) = if path.is_empty() {
            let (old, rest) = split_nul(rest)?;
            let (new, rest) = split_nul(rest)?;
            if old.is_empty() || new.is_empty() {
                return None;
            }
            (new, Some(old), rest)
        } else {
            (path, None, rest)
        };
        files.push(CommitFile {
            path: String::from_utf8_lossy(path).into_owned(),
            previous: previous.map(|old| String::from_utf8_lossy(old).into_owned()),
            insertions,
            deletions,
        });
        bytes = rest;
    }
    Some(files)
}

/// A run of digits, or a single `-` for a binary file.
fn take_count(bytes: &[u8]) -> Option<(Option<u64>, &[u8])> {
    if bytes.first() == Some(&b'-') {
        return Some((None, &bytes[1..]));
    }
    let end = bytes.iter().position(|byte| !byte.is_ascii_digit())?;
    if end == 0 {
        return None;
    }
    let number = std::str::from_utf8(&bytes[..end]).ok()?.parse().ok()?;
    Some((Some(number), &bytes[end..]))
}

fn split_nul(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let end = bytes.iter().position(|byte| *byte == 0)?;
    Some((&bytes[..end], &bytes[end + 1..]))
}

fn stats_from_files(files: &[CommitFile]) -> Option<CommitStats> {
    if files.is_empty() {
        return None;
    }
    let mut insertions = 0u64;
    let mut deletions = 0u64;
    for file in files {
        insertions = insertions.saturating_add(file.insertions.unwrap_or(0));
        deletions = deletions.saturating_add(file.deletions.unwrap_or(0));
    }
    Some(CommitStats {
        files: u64::try_from(files.len()).unwrap_or(u64::MAX),
        insertions,
        deletions,
    })
}

/// One line of a unified diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    Context,
    Added,
    Removed,
    /// A `@@` hunk header. `text` is the whole line.
    Hunk,
    /// `\ No newline at end of file`, without the marker prefix.
    Meta,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

/// The unified diff of one file, or a sentence when there are no lines to paint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub notice: Option<String>,
    pub lines: Vec<DiffLine>,
    /// Widest `text`, counting a tab as four columns. The gutter is extra.
    pub columns: usize,
}

const NOTICE_BINARY: &str = "Binary file.";
const NOTICE_SUBMODULE: &str = "Submodule update.";
const NOTICE_MODE: &str = "File mode changed.";
const NOTICE_EMPTY: &str = "No line changes.";

/// The patch of one path. `path` is the post-image path, or the removed path
/// for a deletion. `:(literal)` keeps a path that starts with `:` from being
/// read as pathspec magic.
pub fn file_diff(git: &Git, root: &Path, oid: &str, path: &str) -> Result<FileDiff, GitError> {
    if !is_commit_oid(oid) {
        return Err(super::exec::rejected(
            "git show",
            format!("{oid} is not a commit hash"),
        ));
    }
    if path.is_empty() || path.as_bytes().contains(&0) {
        return Err(super::exec::rejected("git show", "the path is empty"));
    }
    let spec = format!(":(literal){path}");
    let args = [
        "show",
        "--no-color",
        "--format=",
        "-m",
        "--first-parent",
        "-p",
        oid,
        "--",
        spec.as_str(),
    ];
    let output = git.require_ok(root, &args)?;
    Ok(parse_file_diff(&String::from_utf8_lossy(&output.stdout)))
}

/// Turns one `git show -p` into lines. The `diff --git` preamble is dropped.
/// A binary file, a submodule, or a mode-only change becomes `notice` and no lines.
pub(crate) fn parse_file_diff(text: &str) -> FileDiff {
    if text
        .lines()
        .any(|line| line.starts_with("Binary files ") || line.starts_with("GIT binary patch"))
    {
        return noticed(NOTICE_BINARY);
    }
    if text
        .lines()
        .any(|line| line.contains("Subproject commit") || line.contains(" 160000"))
    {
        return noticed(NOTICE_SUBMODULE);
    }

    let mut saw_mode = false;
    let mut lines = Vec::new();
    let mut old_no = 0u32;
    let mut new_no = 0u32;
    let mut in_hunk = false;
    for line in text.lines() {
        if line.starts_with("old mode ") || line.starts_with("new mode ") {
            saw_mode = true;
        }
        if let Some((old, new)) = hunk_starts(line) {
            in_hunk = true;
            old_no = old;
            new_no = new;
            lines.push(DiffLine {
                kind: DiffKind::Hunk,
                old: None,
                new: None,
                text: line.to_owned(),
            });
            continue;
        }
        if !in_hunk {
            continue;
        }
        let Some(prefix) = line.chars().next() else {
            continue;
        };
        match prefix {
            '+' => {
                lines.push(DiffLine {
                    kind: DiffKind::Added,
                    old: None,
                    new: (new_no > 0).then_some(new_no),
                    text: line[1..].to_owned(),
                });
                new_no = new_no.saturating_add(1);
            }
            '-' => {
                lines.push(DiffLine {
                    kind: DiffKind::Removed,
                    old: (old_no > 0).then_some(old_no),
                    new: None,
                    text: line[1..].to_owned(),
                });
                old_no = old_no.saturating_add(1);
            }
            ' ' => {
                lines.push(DiffLine {
                    kind: DiffKind::Context,
                    old: (old_no > 0).then_some(old_no),
                    new: (new_no > 0).then_some(new_no),
                    text: line[1..].to_owned(),
                });
                old_no = old_no.saturating_add(1);
                new_no = new_no.saturating_add(1);
            }
            '\\' => lines.push(DiffLine {
                kind: DiffKind::Meta,
                old: None,
                new: None,
                text: "No newline at end of file".to_owned(),
            }),
            _ => {}
        }
    }

    if lines.is_empty() {
        return noticed(if saw_mode { NOTICE_MODE } else { NOTICE_EMPTY });
    }
    let columns = lines
        .iter()
        .map(|line| {
            line.text
                .chars()
                .map(|ch| if ch == '\t' { 4 } else { 1 })
                .sum()
        })
        .max()
        .unwrap_or(0);
    FileDiff {
        notice: None,
        lines,
        columns,
    }
}

fn noticed(notice: &str) -> FileDiff {
    FileDiff {
        notice: Some(notice.to_owned()),
        lines: Vec::new(),
        columns: 0,
    }
}

/// `@@ -old[,count] +new[,count] @@`, the first line number of each side.
fn hunk_starts(line: &str) -> Option<(u32, u32)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old_spec, rest) = rest.split_once(' ')?;
    let rest = rest.strip_prefix('+')?;
    let new_spec = rest.split(" @@").next()?;
    Some((spec_start(old_spec)?, spec_start(new_spec)?))
}

fn spec_start(spec: &str) -> Option<u32> {
    spec.split_once(',')
        .map(|(start, _)| start)
        .unwrap_or(spec)
        .parse()
        .ok()
}

pub fn short_oid(oid: &str) -> &str {
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
    let mut commits = parse_log(&output.stdout).ok_or_else(|| {
        super::exec::rejected(
            &format!("git {}", args.join(" ")),
            "unrecognized log output",
        )
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

    fn hover_record(message: &[u8], numstat: &[u8]) -> Vec<u8> {
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
        out.extend_from_slice(numstat);
        out
    }

    #[test]
    fn parses_the_hover_record_and_its_numstat() {
        let stdout = hover_record(
            b"Seal the vault\n\nBody with \x1f, \x1e and \x1d inside.\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\n",
            b"\x00\n10\t2\ta.rs\x0032\t5\tb.rs\x000\t0\tc.rs\x00",
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
            hover.files,
            [
                CommitFile {
                    path: "a.rs".to_owned(),
                    previous: None,
                    insertions: Some(10),
                    deletions: Some(2),
                },
                CommitFile {
                    path: "b.rs".to_owned(),
                    previous: None,
                    insertions: Some(32),
                    deletions: Some(5),
                },
                CommitFile {
                    path: "c.rs".to_owned(),
                    previous: None,
                    insertions: Some(0),
                    deletions: Some(0),
                },
            ]
        );
        assert_eq!(
            hover.stats,
            Some(CommitStats {
                files: 3,
                insertions: 42,
                deletions: 7,
            })
        );
        assert!(hover.tags.is_empty());
    }

    #[test]
    fn numstat_keeps_a_rename_a_binary_and_a_newline_in_the_path() {
        let files =
            parse_numstat(b"\n1\t0\t\x00old name\x00new name\x00-\t-\tbin.dat\x001\t0\ta\nb\x00")
                .expect("parsable");
        assert_eq!(
            files,
            [
                CommitFile {
                    path: "new name".to_owned(),
                    previous: Some("old name".to_owned()),
                    insertions: Some(1),
                    deletions: Some(0),
                },
                CommitFile {
                    path: "bin.dat".to_owned(),
                    previous: None,
                    insertions: None,
                    deletions: None,
                },
                CommitFile {
                    path: "a\nb".to_owned(),
                    previous: None,
                    insertions: Some(1),
                    deletions: Some(0),
                },
            ]
        );
        assert!(files[1].is_binary());
        assert_eq!(files[0].diff_path(), "new name");
    }

    #[test]
    fn empty_numstat_is_an_empty_file_list() {
        assert_eq!(parse_numstat(b""), Some(Vec::new()));
        assert_eq!(parse_numstat(b"\n"), Some(Vec::new()));
        let empty = hover_record(b"Merge branch 'topic'\n", b"\n");
        let hover = parse_commit_hover(&empty).expect("empty");
        assert!(hover.files.is_empty());
        assert_eq!(hover.stats, None);
    }

    #[test]
    fn malformed_numstat_is_rejected() {
        assert_eq!(parse_numstat(b"\nno tabs"), None);
        assert_eq!(parse_numstat(b"\n1\t0\t\0only-old\0"), None);
        assert_eq!(
            parse_commit_hover(&hover_record(b"m", b"\nnot-numstat")),
            None
        );
    }

    #[test]
    fn tag_names_keep_a_comma_and_skip_empty_records() {
        assert_eq!(
            parse_tag_names(b"v1,0\nv0.0.1\n"),
            ["v1,0".to_owned(), "v0.0.1".to_owned()]
        );
        assert_eq!(
            parse_tag_names(b"v1,0\0\nv0.0.1\r\n"),
            ["v1,0".to_owned(), "v0.0.1".to_owned()]
        );
        assert!(parse_tag_names(b"").is_empty());
        assert!(parse_tag_names(b"\n").is_empty());
    }

    #[test]
    fn unified_diff_numbers_lines_and_keeps_the_no_newline_marker() {
        let diff = parse_file_diff(
            "diff --git a/noeol.txt b/noeol.txt\n--- a/noeol.txt\n+++ b/noeol.txt\n@@ -1,2 +1,2 @@\n kept\n-old line\n+new line\n\\ No newline at end of file\n",
        );
        assert_eq!(diff.notice, None);
        assert_eq!(
            diff.lines,
            [
                DiffLine {
                    kind: DiffKind::Hunk,
                    old: None,
                    new: None,
                    text: "@@ -1,2 +1,2 @@".to_owned(),
                },
                DiffLine {
                    kind: DiffKind::Context,
                    old: Some(1),
                    new: Some(1),
                    text: "kept".to_owned(),
                },
                DiffLine {
                    kind: DiffKind::Removed,
                    old: Some(2),
                    new: None,
                    text: "old line".to_owned(),
                },
                DiffLine {
                    kind: DiffKind::Added,
                    old: None,
                    new: Some(2),
                    text: "new line".to_owned(),
                },
                DiffLine {
                    kind: DiffKind::Meta,
                    old: None,
                    new: None,
                    text: "No newline at end of file".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn unified_diff_notices_replace_lines_that_are_not_a_text_hunk() {
        assert_eq!(
            parse_file_diff("Binary files a/bin.dat and b/bin.dat differ\n")
                .notice
                .as_deref(),
            Some("Binary file.")
        );
        assert_eq!(
            parse_file_diff("GIT binary patch\ndelta 1\n")
                .notice
                .as_deref(),
            Some("Binary file.")
        );
        assert_eq!(
            parse_file_diff(
                "index abc..def 160000\n@@ -1 +1 @@\n-Subproject commit abc\n+Subproject commit def\n"
            )
            .notice
            .as_deref(),
            Some("Submodule update.")
        );
        assert_eq!(
            parse_file_diff("old mode 100644\nnew mode 100755\n")
                .notice
                .as_deref(),
            Some("File mode changed.")
        );
        assert_eq!(
            parse_file_diff("diff --git a/x b/x\n").notice.as_deref(),
            Some("No line changes.")
        );
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
