use std::collections::HashSet;
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
    let mut seen = HashSet::new();
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
        if seen.insert(person.email_key()) {
            found.push(person);
        }
    }
    found
}

/// `Name <email>`, where the address is the last `<…>` and holds no `>`.
fn parse_name_and_email(value: &str) -> Option<Person> {
    let value = value.trim();
    let inner = value.strip_suffix('>')?;
    // The first `<` after the last `>`. One pass each way: the trailer comes
    // from the repository, so it can be megabytes of brackets.
    let from = inner.rfind('>').map_or(0, |index| index + 1);
    let open = from + inner[from..].find('<')?;
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
///
/// `.mailmap` may have rewritten the author: `recorded` is the identity written
/// in the commit (`%an <%ae>`). Its address becomes an alias, and a trailer
/// naming it is the author too.
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
///
/// `--no-textconv` and `--no-ext-diff` are set so a repository diff driver
/// cannot run while the commit is only being displayed.
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
        "--no-textconv",
        "--no-ext-diff",
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
///
/// `--no-textconv` and `--no-ext-diff` keep a repo-local `diff.<driver>`
/// from executing when the hover opens.
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
        "--no-textconv",
        "--no-ext-diff",
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
        // Not `short`, which prints `tags/v1` when a branch is also named v1.
        "--format=%(refname:lstrip=2)",
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
///
/// `--no-textconv` and `--no-ext-diff` are required here: `git show -p`
/// otherwise runs the repository's textconv and external diff commands.
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
        "--no-textconv",
        "--no-ext-diff",
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
    let header: Vec<&str> = header_lines(text).collect();
    if header
        .iter()
        .any(|line| line.starts_with("Binary files ") || line.starts_with("GIT binary patch"))
    {
        return noticed(NOTICE_BINARY);
    }
    if header.iter().copied().any(is_gitlink_header) {
        return noticed(NOTICE_SUBMODULE);
    }
    let saw_mode = header
        .iter()
        .any(|line| line.starts_with("old mode ") || line.starts_with("new mode "));

    let mut lines = Vec::new();
    let mut old_no = 0u32;
    let mut new_no = 0u32;
    let mut in_hunk = false;
    for line in text.lines() {
        if line.starts_with("diff --git ") {
            in_hunk = false;
            continue;
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

/// The header of every `diff --git` section: its lines before the first hunk.
/// `git show` prints a type change as two sections. Hunk lines are file
/// content and can contain anything, so they are left out.
fn header_lines(text: &str) -> impl Iterator<Item = &str> {
    let mut in_hunk = false;
    text.lines().filter(move |line| {
        if line.starts_with("diff --git ") {
            in_hunk = false;
        } else if hunk_starts(line).is_some() {
            in_hunk = true;
        }
        !in_hunk
    })
}

/// `index a..b 160000`, `new file mode 160000`, and the other mode lines of a
/// submodule (gitlink) entry.
fn is_gitlink_header(line: &str) -> bool {
    [
        "index ",
        "new file mode ",
        "deleted file mode ",
        "old mode ",
        "new mode ",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix))
        && line.ends_with(" 160000")
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
///
/// `--no-textconv` and `--no-ext-diff` are set so a repository diff driver
/// cannot run while the history list is displayed.
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
        "--no-textconv",
        "--no-ext-diff",
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
///
/// `-z` ends a record on NUL, so a NUL inside the subject ends that subject.
/// Bytes after it are another commit only when they contain a field
/// separator. A tail with none is discarded; otherwise that one subject
/// would reject every commit already parsed. A record that fails validation
/// still rejects the whole log.
pub(crate) fn parse_log(stdout: &[u8]) -> Option<Vec<Commit>> {
    let mut commits = Vec::new();
    for record in stdout
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        match parse_log_record(record) {
            Some(commit) => commits.push(commit),
            None if commits.is_empty() || record.contains(&FIELD_SEPARATOR) => return None,
            None => {}
        }
    }
    Some(commits)
}

fn parse_log_record(record: &[u8]) -> Option<Commit> {
    let separator = record.iter().position(|byte| *byte == FIELD_SEPARATOR)?;
    let (oid, summary) = (&record[..separator], &record[separator + 1..]);
    if oid.is_empty() || !oid.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    Some(Commit {
        oid: String::from_utf8_lossy(oid).into_owned(),
        summary: String::from_utf8_lossy(summary).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::test_support::OID_A;

    /// `git log -z --format=%H%x1f%s` output, and the `(oid, summary)` pairs
    /// read from it. `None` rejects the whole log.
    type Log = Option<&'static [(&'static str, &'static str)]>;

    const LOGS: &[(&[u8], Log)] = &[
        (
            b"abcd\x1fAwaken the Old Ones\0ff\x1fInitial commit\0",
            Some(&[("abcd", "Awaken the Old Ones"), ("ff", "Initial commit")]),
        ),
        (b"", Some(&[])),
        (b"\0\0\0", Some(&[])),
        (
            b"abcd\x1fsum\0ff\x1fnext",
            Some(&[("abcd", "sum"), ("ff", "next")]),
        ),
        (b"abcd\x1f\0", Some(&[("abcd", "")])),
        (b"abcd\x1fa\x1fb\0", Some(&[("abcd", "a\u{1f}b")])),
        (b"abcd\x1fcaf\xe9\0", Some(&[("abcd", "caf\u{fffd}")])),
        // `-z` cuts a subject at its NUL. The tail has no separator, so it is
        // the rest of that subject and not a commit.
        (
            b"abcd\x1fsubject with NUL\0 and more\0",
            Some(&[("abcd", "subject with NUL")]),
        ),
        (b"no separator\0", None),
        (b"\x1fsubject without oid\0", None),
        (b"not-hex\x1fsubject\0", None),
        (b"ab\0cd\x1fsum", None),
    ];

    #[test]
    fn parse_log_table() {
        for (stdout, expected) in LOGS {
            let commits = parse_log(stdout).map(|commits| {
                commits
                    .into_iter()
                    .map(|commit| (commit.oid, commit.summary))
                    .collect::<Vec<_>>()
            });
            let expected = expected.map(|pairs| {
                pairs
                    .iter()
                    .map(|(oid, summary)| ((*oid).to_owned(), (*summary).to_owned()))
                    .collect::<Vec<_>>()
            });
            assert_eq!(commits, expected, "{stdout:?}");
        }
    }

    #[test]
    fn short_oid_is_twelve_characters() {
        let long = OID_A.to_owned();
        for (oid, short) in [
            ("", ""),
            ("abc", "abc"),
            ("0123456789ab", "0123456789ab"),
            ("0123456789abc", "0123456789ab"),
            (long.as_str(), "4b825dc642cb"),
        ] {
            let commit = Commit {
                oid: oid.to_owned(),
                summary: String::new(),
            };
            assert_eq!(commit.short_oid(), short, "{oid:?}");
        }
    }

    const ADA_AND_GRACE: &[(&str, &str)] =
        &[("Ada", "ada@example.com"), ("Grace", "grace@example.com")];

    /// Trailer text, and the `(name, email)` pairs read from it in order.
    const CO_AUTHORS: &[(&str, &[(&str, &str)])] = &[
        ("", &[]),
        ("Co-authored", &[]),
        // `concat!` keeps the leading spaces; a `\` line continuation would strip them.
        (
            concat!(
                "Co-authored-by: Ada Voss <ada@miskatonic.test>\r\n",
                "CO-AUTHORED-BY:\tHerb\tWest\t<west@arkham.test>\t\r\n",
                "Co-authored-by: Ada Voss <ADA@miskatonic.test>\n",
                "Co-authored-by: First <Dup@Example.com>\n",
                "co-authored-by: Second <dup@example.com>\n",
                "Co-authored-by: <nameless@arkham.test>\n",
                "Co-authored-by: <>\n",
                "Co-authored-by: Nobody <>\n",
                "Co-authored-by: Nobody < >\n",
                "Co-authored-by: No Address\n",
                "Reviewed-by: Someone <someone@arkham.test>\n",
                "Co-authored-by: Odd <Name> <odd@arkham.test>\n",
                "Co-authored-by: Tagged <user<host@example.com>\n",
                "  Co-authored-by: Indented <in@example.com>\n",
                "Co-authored-by: Ünïcödé Ñame <unicode@arkham.test>",
            ),
            &[
                ("Ada Voss", "ada@miskatonic.test"),
                ("Herb\tWest", "west@arkham.test"),
                ("First", "Dup@Example.com"),
                ("Odd <Name>", "odd@arkham.test"),
                ("Tagged", "user<host@example.com"),
                ("Ünïcödé Ñame", "unicode@arkham.test"),
            ],
        ),
        // The key starts the line, in any case. Whitespace may follow the
        // address; text may not.
        ("  Co-authored-by: A <a@b.co>", &[]),
        ("prefix Co-authored-by: Name <a@b.co>", &[]),
        ("Co-authored-by: Name <a@b.co> trailing", &[]),
        (
            "cO-AuThOrEd-By:Herbert West   <west@arkham.test> \t ",
            &[("Herbert West", "west@arkham.test")],
        ),
        // A bare CR and the Unicode separators end a line, as in VS Code.
        (
            "Co-authored-by: Ada <ada@example.com>\rCo-authored-by: Grace <grace@example.com>",
            ADA_AND_GRACE,
        ),
        (
            "Co-authored-by: Ada <ada@example.com>\n\rCo-authored-by: Grace <grace@example.com>",
            ADA_AND_GRACE,
        ),
        (
            "Co-authored-by: Ada <ada@example.com>\u{2028}Co-authored-by: Grace <grace@example.com>",
            ADA_AND_GRACE,
        ),
        (
            "Co-authored-by: Ada <ada@example.com>\u{2029}Co-authored-by: Grace <grace@example.com>",
            ADA_AND_GRACE,
        ),
    ];

    fn people(pairs: &[(&str, &str)]) -> Vec<Person> {
        pairs
            .iter()
            .map(|(name, email)| Person::new(*name, *email))
            .collect()
    }

    #[test]
    fn parse_co_authors_table() {
        for (text, expected) in CO_AUTHORS {
            assert_eq!(parse_co_authors(text), people(expected), "{text:?}");
        }
    }

    #[test]
    fn parse_co_authors_stays_linear_on_huge_trailers() {
        let name = "N".repeat(60_000);
        let email = format!("{}@x.test", "e".repeat(20_000));
        let line = format!("Co-authored-by: {name} <{email}>");
        assert_eq!(parse_co_authors(&line), [Person::new(name, email)]);
        assert!(parse_co_authors(&"a".repeat(100_000)).is_empty());

        // A megabyte of `<` before the closing `>`, and thousands of distinct
        // co-authors, come from a repository and must parse in linear time.
        let brackets = format!("Co-authored-by: a {}>b>", "<".repeat(1 << 20));
        assert!(parse_co_authors(&brackets).is_empty());
        let many: String = (0..20_000)
            .map(|n| format!("Co-authored-by: N{n} <n{n}@x.test>\n"))
            .collect();
        assert_eq!(parse_co_authors(&many).len(), 20_000);
    }

    proptest! {
        #![proptest_config(crate::test_support::proptest_config())]

        #[test]
        fn parse_co_authors_never_panics(text in "(?s).{0,200}") {
            let _ = parse_co_authors(&text);
        }
    }

    /// The author as recorded (no `.mailmap`), the co-author trailers, and
    /// the author's aliases and the co-authors kept after folding.
    type Fold = (
        (&'static str, &'static str),
        &'static [(&'static str, &'static str)],
        &'static [&'static str],
        &'static [(&'static str, &'static str)],
    );

    const FOLDS: &[Fold] = &[
        // The author's address in any case or padding is the author. The
        // author's name with another address is an alias, once.
        (
            ("Ada Lovelace", "Ada@Example.com"),
            &[
                ("Someone", "ada@example.com"),
                ("Other Name", "  ADA@example.com  "),
                ("ada lovelace", "Alt@Example.com"),
                ("ADA LOVELACE", "alt@example.com"),
                ("Ada Lovelace", "second@example.com"),
                ("Grace Hopper", "grace@example.com"),
            ],
            &["Alt@Example.com", "second@example.com"],
            &[("Grace Hopper", "grace@example.com")],
        ),
        // An empty or blank name is no alias key.
        (
            ("", "author@example.com"),
            &[
                ("", "other@example.com"),
                ("   ", "space@example.com"),
                ("Author", "author@example.com"),
            ],
            &[],
            &[("", "other@example.com"), ("   ", "space@example.com")],
        ),
        (
            ("   ", "ada@example.com"),
            &[("Ada", "alt@example.com")],
            &[],
            &[("Ada", "alt@example.com")],
        ),
        // Names match trimmed, and the author keeps the name as written.
        (
            ("Ada ", "ada@example.com"),
            &[("ada", "alt@example.com")],
            &["alt@example.com"],
            &[],
        ),
        // Without an address, a blank trailer address is the author's.
        (
            ("Ada", ""),
            &[
                ("Grace", "grace@example.com"),
                ("Blank", "  "),
                ("ada", "alt@example.com"),
            ],
            &["alt@example.com"],
            &[("Grace", "grace@example.com")],
        ),
    ];

    #[test]
    fn fold_with_recorded_table() {
        for &((name, email), co_authors, aliases, kept) in FOLDS {
            let mut author = Person::new(name, email);
            let recorded = author.clone();
            let got = fold_with_recorded(&mut author, &recorded, people(co_authors));
            let expected = Person {
                aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
                ..recorded
            };
            assert_eq!((author, got), (expected, people(kept)), "{name:?}");
        }
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
    fn parses_commit_detail_and_keeps_separators_and_nul_in_the_message() {
        let stdout = detail_record(
            OID_A,
            "cthulhu",
            "cthulhu@example.invalid",
            "2026-10-07 18:43",
            b"Second\n\nThe body\x1f stays.\x00 and more",
        );
        assert_eq!(
            parse_commit_detail(&stdout),
            Some(CommitDetail {
                oid: OID_A.to_owned(),
                author: Person::new("cthulhu", "cthulhu@example.invalid"),
                co_authors: Vec::new(),
                authored_at: "2026-10-07 18:43".to_owned(),
                message: "Second\n\nThe body\u{1f} stays.\u{0} and more".to_owned(),
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
        for (diff, notice) in [
            (
                "Binary files a/bin.dat and b/bin.dat differ\n",
                "Binary file.",
            ),
            ("GIT binary patch\ndelta 1\n", "Binary file."),
            (
                "index abc..def 160000\n@@ -1 +1 @@\n-Subproject commit abc\n+Subproject commit def\n",
                "Submodule update.",
            ),
            ("old mode 100644\nnew mode 100755\n", "File mode changed."),
            ("diff --git a/x b/x\n", "No line changes."),
            (
                "new file mode 160000\nindex 0000000..abc\n@@ -0,0 +1 @@\n+Subproject commit abc\n",
                "Submodule update.",
            ),
            // A type change is two sections, and the second header counts too.
            (
                concat!(
                    "diff --git a/x b/x\ndeleted file mode 120000\nindex 1de5659..0000000\n",
                    "--- a/x\n+++ /dev/null\n@@ -1 +0,0 @@\n-target\n\\ No newline at end of file\n",
                    "diff --git a/x b/x\nnew file mode 100644\nindex 0000000..8352675\n",
                    "Binary files /dev/null and b/x differ\n",
                ),
                "Binary file.",
            ),
            (
                concat!(
                    "diff --git a/x b/x\ndeleted file mode 100644\nindex 1de5659..0000000\n",
                    "--- a/x\n+++ /dev/null\n@@ -1 +0,0 @@\n-text\n",
                    "diff --git a/x b/x\nnew file mode 160000\nindex 0000000..abc\n",
                    "--- /dev/null\n+++ b/x\n@@ -0,0 +1 @@\n+Subproject commit abc\n",
                ),
                "Submodule update.",
            ),
        ] {
            assert_eq!(
                parse_file_diff(diff).notice.as_deref(),
                Some(notice),
                "{diff:?}"
            );
        }
        // The markers inside a text file's hunks are content, not a header.
        let text = parse_file_diff(
            "diff --git a/price.txt b/price.txt\nindex abc..def 100644\n@@ -1,2 +1,2 @@\n-price 150000\n+price 160000\n Subproject commit is just text here\n",
        );
        assert_eq!(text.notice, None);
        assert_eq!(text.lines.len(), 4, "hunk header and three lines: {text:?}");
        // The second section's `---`/`+++` header is not a hunk line.
        let type_change = parse_file_diff(concat!(
            "diff --git a/x b/x\ndeleted file mode 120000\n--- a/x\n+++ /dev/null\n",
            "@@ -1 +0,0 @@\n-target\n",
            "diff --git a/x b/x\nnew file mode 100644\n--- /dev/null\n+++ b/x\n",
            "@@ -0,0 +1 @@\n+hello\n",
        ));
        let texts: Vec<&str> = type_change
            .lines
            .iter()
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(
            texts,
            ["@@ -1 +0,0 @@", "target", "@@ -0,0 +1 @@", "hello"],
            "{type_change:?}"
        );
    }

    #[test]
    fn empty_and_malformed_hover_records() {
        for (numstat, files) in [
            (&b""[..], Some(0)),
            (b"\n", Some(0)),
            (b"\nno tabs", None),
            (b"\n1\t0\t\0only-old\0", None),
        ] {
            let parsed = parse_numstat(numstat).map(|files| files.len());
            assert_eq!(parsed, files, "{numstat:?}");
        }
        let merge = parse_commit_hover(&hover_record(b"Merge branch 'topic'\n", b"\n"))
            .expect("a merge with no changes");
        assert!(merge.files.is_empty());
        assert_eq!(merge.stats, None);

        assert_eq!(parse_commit_hover(b""), None);
        assert_eq!(parse_commit_hover(b"no record end"), None);
        assert_eq!(
            parse_commit_hover(&hover_record(b"m", b"\nnot-numstat")),
            None
        );
        let mut bad_time = hover_record(b"m", b"");
        let at = bad_time
            .windows(10)
            .position(|window| window == b"1792598400")
            .expect("time");
        bad_time[at] = b'x';
        assert_eq!(parse_commit_hover(&bad_time), None);
    }
}
