//! Extra parser cases for authors, co-author trailers and remotes.
//!
//! No git process is spawned. Property cases are capped and failure files are
//! not written into the tree.

use std::collections::HashSet;

use proptest::prelude::*;
use proptest::string::string_regex;

use super::graph::parse_graph_log;
use super::remotes::parse_remote_config;
use super::*;

const COAUTHOR_KEY: &str = "co-authored-by:";

fn norm_email(email: &str) -> String {
    email.trim().to_lowercase()
}

/// JavaScript `^` / `$` with the `m` flag: `\n`, `\r`, U+2028 and U+2029 are
/// line breaks, and `\r\n` is one break.
fn logical_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        let crlf = ch == '\r' && matches!(chars.peek(), Some((_, '\n')));
        if !matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
            continue;
        }
        lines.push(&text[start..index]);
        if crlf {
            chars.next();
        }
        start = chars.peek().map(|(next, _)| *next).unwrap_or(text.len());
    }
    lines.push(&text[start..]);
    lines
}

fn is_js_whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\t' | '\n' | '\u{000B}' | '\u{000C}' | '\r' | ' ' | '\u{00A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// One line against `^Co-authored-by:\s*(.+?)\s*<([^>]+)>\s*$` (case-insensitive
/// key). The address is trimmed, and a whitespace-only address is skipped:
/// the parser treats that as an empty address.
fn match_coauthor_line(line: &str) -> Option<(String, String)> {
    let head = line.get(..COAUTHOR_KEY.len())?;
    if !head.eq_ignore_ascii_case(COAUTHOR_KEY) {
        return None;
    }
    let rest = &line[COAUTHOR_KEY.len()..];
    for (index, ch) in rest.char_indices() {
        if ch != '<' {
            continue;
        }
        let after = &rest[index + 1..];
        let close = after.find('>')?;
        let email = after[..close].trim();
        if email.is_empty() {
            continue;
        }
        if !after[close + 1..].chars().all(is_js_whitespace) {
            continue;
        }
        let name = rest[..index].trim();
        if name.is_empty() {
            continue;
        }
        return Some((name.to_owned(), email.to_owned()));
    }
    None
}

/// VS Code's co-author scan, plus this crate's "one person per address" rule.
fn reference_co_authors(text: &str) -> Vec<Person> {
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    for line in logical_lines(text) {
        let Some((name, email)) = match_coauthor_line(line) else {
            continue;
        };
        if seen.insert(norm_email(&email)) {
            found.push(Person::new(name, email));
        }
    }
    found
}

/// Drops the author's own address and turns a same-name trailer into an alias.
fn fold_spec(author: &Person, people: Vec<Person>) -> (Person, Vec<Person>) {
    let mut author = author.clone();
    let author_key = norm_email(&author.email);
    let author_name = author.name.trim().to_lowercase();
    let mut kept = Vec::new();
    for person in people {
        let key = norm_email(&person.email);
        if key == author_key {
            continue;
        }
        let same_name = !author_name.is_empty() && person.name.trim().to_lowercase() == author_name;
        if same_name {
            let known = std::iter::once(author.email.as_str())
                .chain(author.aliases.iter().map(String::as_str))
                .any(|email| !key.is_empty() && norm_email(email) == key);
            if !known {
                author.aliases.push(person.email);
            }
            continue;
        }
        kept.push(person);
    }
    (author, kept)
}

fn graph_record(
    oid: &str,
    parents: &str,
    name: &str,
    email: &str,
    subject: &str,
    trailers: &str,
) -> Vec<u8> {
    let mut out = Vec::new();
    for part in [
        oid.as_bytes(),
        parents.as_bytes(),
        name.as_bytes(),
        email.as_bytes(),
        name.as_bytes(),
        email.as_bytes(),
    ] {
        out.extend_from_slice(part);
        out.push(0x1f);
    }
    out.extend_from_slice(subject.as_bytes());
    out.push(0x1e);
    out.extend_from_slice(trailers.as_bytes());
    out.push(0);
    out
}

fn render_trailers(people: &[(String, String)], upper: bool, tab: bool, crlf: bool) -> String {
    let key = if upper {
        "CO-AUTHORED-BY:"
    } else {
        "Co-authored-by:"
    };
    let gap = if tab { "\t" } else { " " };
    let nl = if crlf { "\r\n" } else { "\n" };
    let mut out = String::new();
    for (name, email) in people {
        out.push_str(key);
        out.push_str(gap);
        out.push_str(name);
        out.push_str(" <");
        out.push_str(email);
        out.push('>');
        out.push_str("  ");
        out.push_str(nl);
    }
    out
}

fn remote_entry(key: &str, value: &str) -> Vec<u8> {
    let mut out = key.as_bytes().to_vec();
    out.push(b'\n');
    out.extend_from_slice(value.as_bytes());
    out.push(0);
    out
}

fn arb_bytes() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..4096)
}

fn arb_oid() -> impl Strategy<Value = String> {
    string_regex("[0-9a-f]{7,40}").unwrap()
}

fn arb_words() -> impl Strategy<Value = String> {
    (
        string_regex("[A-Za-z][A-Za-z0-9]{0,8}").unwrap(),
        any::<bool>(),
        string_regex("[A-Za-z][A-Za-z0-9]{0,8}").unwrap(),
    )
        .prop_map(|(first, second, extra)| {
            if second {
                format!("{first} {extra}")
            } else {
                first
            }
        })
}

fn arb_email_addr() -> impl Strategy<Value = String> {
    string_regex("[a-z]{1,8}@[a-z]{1,8}\\.[a-z]{2,3}").unwrap()
}

fn arb_field_name() -> impl Strategy<Value = String> {
    prop_oneof![
        1 => Just(String::new()),
        4 => arb_words(),
    ]
}

fn arb_subject() -> impl Strategy<Value = String> {
    (
        string_regex("[A-Za-z0-9 .,'_-]{0,40}").unwrap(),
        any::<bool>(),
    )
        .prop_map(|(text, special)| {
            if special {
                format!("Co-authored-by: Decoy <decoy@example.invalid>\u{1f}{text}")
            } else {
                text
            }
        })
}

fn arb_trailers() -> impl Strategy<Value = Vec<(String, String)>> {
    prop::collection::vec((arb_words(), arb_email_addr()), 0..=4)
}

#[derive(Clone, Debug)]
struct GeneratedCommit {
    oid: String,
    parents: Vec<String>,
    name: String,
    email: String,
    subject: String,
    trailers: Vec<(String, String)>,
    upper_key: bool,
    tab: bool,
    crlf: bool,
    repeat_author_email: bool,
    alias_author_name: bool,
}

fn arb_commit() -> impl Strategy<Value = GeneratedCommit> {
    (
        arb_oid(),
        prop::collection::vec(arb_oid(), 0..=3),
        arb_field_name(),
        arb_email_addr(),
        arb_subject(),
        arb_trailers(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(
            |(
                oid,
                parents,
                name,
                email,
                subject,
                trailers,
                upper_key,
                tab,
                crlf,
                repeat_author_email,
                alias_author_name,
            )| GeneratedCommit {
                oid,
                parents,
                name,
                email,
                subject,
                trailers,
                upper_key,
                tab,
                crlf,
                repeat_author_email,
                alias_author_name,
            },
        )
}

fn arb_remote_name() -> impl Strategy<Value = String> {
    prop::collection::vec(string_regex("[A-Za-z0-9_-]{1,6}").unwrap(), 1..=3)
        .prop_map(|parts| parts.join("."))
}

fn arb_remote_value() -> impl Strategy<Value = String> {
    (
        string_regex("https://example\\.invalid/[A-Za-z0-9]{0,16}").unwrap(),
        0u8..5,
    )
        .prop_map(|(url, kind)| match kind {
            0 => url,
            1 => format!("{url}\r"),
            2 => format!("\r{url}"),
            3 => format!("  {url} \r"),
            _ => format!("https://\r{url}\nmore"),
        })
}

#[test]
fn empty_graph_log_and_bare_nuls_are_empty() {
    assert_eq!(parse_graph_log(b""), Some(Vec::new()));
    assert_eq!(parse_graph_log(b"\0"), Some(Vec::new()));
    assert_eq!(parse_graph_log(b"\0\0"), Some(Vec::new()));
}

#[test]
fn graph_record_keeps_subject_separators_and_folds_the_author() {
    let oid = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    let parent_a = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
    let parent_b = "8b137891791fe96927ad78e64b0aad7bded08eec";
    let subject = "Keep\u{1f}the separator\nCo-authored-by: Decoy <decoy@example.com>";
    let trailers = "\
Co-authored-by: Fernando Diaz <FERNANDO@example.com>\n\
Co-authored-by: fernando diaz <alt@example.com>\n\
Co-authored-by: Cursor Agent <cursoragent@cursor.com>\n\
co-authored-by: Cursor Agent <CURSORAGENT@cursor.com>\n\
Co-authored-by: Other Person <other@example.com>\n";
    let mut stdout = graph_record(
        oid,
        &format!("{parent_a} {parent_b}"),
        "Fernando Diaz",
        "Fernando@Example.com",
        subject,
        trailers,
    );
    stdout.extend(graph_record(
        parent_a,
        "",
        "Root",
        "root@example.com",
        "root",
        "",
    ));

    let commits = parse_graph_log(&stdout).expect("records parse");
    assert_eq!(commits.len(), 2);

    let commit = &commits[0];
    assert_eq!(commit.oid, oid);
    assert_eq!(commit.parents, vec![parent_a, parent_b]);
    assert_eq!(commit.summary, subject);
    assert!(commit.summary.contains('\u{1f}'));
    assert_eq!(commit.author.name, "Fernando Diaz");
    assert_eq!(commit.author.email, "Fernando@Example.com");
    assert_eq!(commit.author.aliases, vec!["alt@example.com".to_owned()]);
    assert_eq!(
        commit.co_authors,
        vec![
            Person::new("Cursor Agent", "cursoragent@cursor.com"),
            Person::new("Other Person", "other@example.com"),
        ]
    );
    assert!(
        commit
            .co_authors
            .iter()
            .all(|person| person.email != "decoy@example.com")
    );

    assert_eq!(commits[1].oid, parent_a);
    assert!(commits[1].parents.is_empty());
    assert_eq!(commits[1].summary, "root");
    assert!(commits[1].co_authors.is_empty());
    assert!(commits[1].author.aliases.is_empty());
}

#[test]
fn parse_remote_config_dotted_names_and_cr_in_values() {
    let mut stdout = Vec::new();
    stdout.extend(remote_entry(
        "remote.my.fork.url",
        "https://github.com/someone/fork\r",
    ));
    stdout.extend(remote_entry(
        "remote.a.b.c.url",
        "git@github.com:org/repo.git\r\nmore",
    ));
    stdout.extend(remote_entry(
        "remote.origin.url",
        "  https://example.invalid/spaced  ",
    ));
    stdout.extend(remote_entry("not.remote", "https://example.invalid/skip"));
    stdout.extend(remote_entry(
        "remote..url",
        "https://example.invalid/empty-name",
    ));
    stdout.extend(remote_entry(
        "remote.cr.mid.url",
        "https://example.invalid/a\rb",
    ));
    stdout.extend(remote_entry("remote.origin.url", ""));

    assert_eq!(
        parse_remote_config(&stdout),
        vec![
            Remote {
                name: "my.fork".to_owned(),
                url: "https://github.com/someone/fork".to_owned(),
            },
            Remote {
                name: "a.b.c".to_owned(),
                url: "git@github.com:org/repo.git\r\nmore".to_owned(),
            },
            Remote {
                name: "origin".to_owned(),
                url: "https://example.invalid/spaced".to_owned(),
            },
            Remote {
                name: "cr.mid".to_owned(),
                url: "https://example.invalid/a\rb".to_owned(),
            },
        ]
    );
}

proptest! {
    #![proptest_config(crate::test_support::proptest_config())]

    #[test]
    fn parse_graph_log_never_panics(bytes in arb_bytes()) {
        let _ = parse_graph_log(&bytes);
    }

    #[test]
    fn parse_graph_log_round_trips_fields_and_coauthors(
        commits in prop::collection::vec(arb_commit(), 1..=3)
    ) {
        let mut stdout = Vec::new();
        let mut rendered = Vec::new();
        for commit in &commits {
            let mut trailers = commit.trailers.clone();
            if commit.repeat_author_email {
                trailers.push(("Someone Else".to_owned(), commit.email.to_uppercase()));
            }
            if commit.alias_author_name {
                trailers.push((
                    commit.name.to_lowercase(),
                    "alias@example.invalid".to_owned(),
                ));
            }
            if let Some((_, email)) = trailers.first().cloned() {
                trailers.push(("Repeat".to_owned(), email.to_uppercase()));
            }
            let trailer_text = render_trailers(
                &trailers,
                commit.upper_key,
                commit.tab,
                commit.crlf,
            );
            stdout.extend(graph_record(
                &commit.oid,
                &commit.parents.join(" "),
                &commit.name,
                &commit.email,
                &commit.subject,
                &trailer_text,
            ));
            rendered.push(trailer_text);
        }

        let parsed = parse_graph_log(&stdout).expect("generated records parse");
        assert_eq!(parsed.len(), commits.len());
        for ((commit, trailer_text), parsed_commit) in commits.iter().zip(rendered).zip(parsed) {
            let (expected_author, expected_coauthors) = fold_spec(
                &Person::new(commit.name.clone(), commit.email.clone()),
                reference_co_authors(&trailer_text),
            );
            assert_eq!(parsed_commit.oid, commit.oid);
            assert_eq!(parsed_commit.parents, commit.parents);
            assert_eq!(parsed_commit.summary, commit.subject);
            assert_eq!(parsed_commit.author.name, commit.name);
            assert_eq!(parsed_commit.author.email, commit.email);
            assert_eq!(parsed_commit.author.aliases, expected_author.aliases);
            assert_eq!(parsed_commit.co_authors, expected_coauthors);
            assert!(
                parsed_commit
                    .co_authors
                    .iter()
                    .all(|person| norm_email(&person.email) != "decoy@example.invalid")
            );
        }
    }

    #[test]
    fn parse_remote_config_never_panics(bytes in arb_bytes()) {
        for remote in parse_remote_config(&bytes) {
            assert!(!remote.name.is_empty());
            assert!(!remote.url.is_empty());
            assert_eq!(remote.url.trim(), remote.url);
            assert!(!remote.name.contains('\n'));
            assert!(!remote.name.contains('\0'));
        }
    }

    #[test]
    fn parse_remote_config_round_trips_dotted_names_and_cr(
        remotes in prop::collection::vec((arb_remote_name(), arb_remote_value()), 0..=4)
    ) {
        let mut stdout = Vec::new();
        stdout.extend(remote_entry("core.bare", "false"));
        stdout.extend_from_slice(b"\xff\xfe\0");
        let mut expected = Vec::new();
        for (name, value) in &remotes {
            stdout.extend(remote_entry(&format!("remote.{name}.url"), value));
            let url = value.trim();
            if !url.is_empty() {
                expected.push(Remote {
                    name: name.clone(),
                    url: url.to_owned(),
                });
            }
        }
        stdout.extend(remote_entry("remote.origin.url", ""));
        assert_eq!(parse_remote_config(&stdout), expected);
    }
}
