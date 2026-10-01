//! Property tests for git output parsers and settings persistence.
//!
//! Nothing in this module spawns git. Each property runs 64 cases. Inputs are
//! capped (byte strings at `0..4096`) so the suite stays a fast fuzz harness.
//!
//! A few properties characterize current behavior that the report calls out as
//! a bug. Those cases are also pinned by concrete tests further down; the
//! properties themselves stay true for a UTF-8-safe fix where that is called
//! out, and they do not panic.

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};

use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;

use super::exec::{GitError, GitVersion, MIN_GIT_VERSION, classify_failure};
use super::log::{SHORT_OID_LEN, parse_log, short_oid};
use super::repo::{Head, parse_head};
use crate::settings::{FILE_NAME, MAX_RECENT_REPOSITORIES, Settings};

fn proptest_config() -> ProptestConfig {
    ProptestConfig {
        cases: 64,
        failure_persistence: Some(Box::new(FileFailurePersistence::Off)),
        ..ProptestConfig::default()
    }
}

fn arb_text(max_chars: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..=max_chars)
        .prop_map(|chars| chars.into_iter().collect())
}

fn arb_bytes() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..4096)
}

fn arb_record_bytes(max_len: usize) -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(1u8..=255, 0..max_len)
}

/// Leading dotted numbers, matching `GitVersion::parse`: trailing dots are
/// dropped, missing minor/patch become 0, and a component that does not fit
/// in `u32` rejects the whole version. Components after the third are ignored.
fn components_of_raw(raw: &str) -> Option<(u32, u32, u32)> {
    let end = raw
        .find(|ch: char| !ch.is_ascii_digit() && ch != '.')
        .unwrap_or(raw.len());
    let numeric = raw[..end].trim_end_matches('.');
    let mut parts = numeric.split('.').map(|part| part.parse::<u32>().ok());
    let major = parts.next()??;
    let minor = parts.next().unwrap_or(Some(0))?;
    let patch = parts.next().unwrap_or(Some(0))?;
    Some((major, minor, patch))
}

fn reference_version(output: &str) -> Option<((u32, u32, u32), String)> {
    let raw = output
        .lines()
        .next()?
        .trim()
        .strip_prefix("git version ")?
        .trim()
        .to_owned();
    Some((components_of_raw(&raw)?, raw))
}

fn arb_version_suffix() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => Just(String::new()),
        2 => Just(".windows.1".to_owned()),
        2 => Just(" (Apple Git-154)".to_owned()),
        1 => prop::collection::vec(prop::char::range('a', 'z'), 1..8).prop_map(|chars| {
            let mut suffix = String::from(" ");
            suffix.extend(chars);
            suffix
        }),
    ]
}

fn arb_git_version() -> impl Strategy<Value = GitVersion> {
    (
        any::<u32>(),
        any::<u32>(),
        any::<u32>(),
        0u8..3,
        arb_version_suffix(),
    )
        .prop_map(|(major, minor, patch, width, suffix)| {
            let numeric = match width {
                0 => format!("{major}"),
                1 => format!("{major}.{minor}"),
                _ => format!("{major}.{minor}.{patch}"),
            };
            let output = format!("git version {numeric}{suffix}");
            GitVersion::parse(&output)
                .unwrap_or_else(|| panic!("structured version did not parse: {output:?}"))
        })
}

fn arb_version_input() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => arb_text(128),
        2 => arb_text(64).prop_map(|suffix| format!("git version {suffix}")),
        2 => (0u32..20_000, 0u32..20_000, 0u32..20_000, arb_text(12)).prop_map(
            |(major, minor, patch, suffix)| format!("git version {major}.{minor}.{patch}{suffix}")
        ),
        1 => prop::collection::vec(prop::char::range('0', '9'), 10..20).prop_map(|digits| {
            let digits: String = digits.into_iter().collect();
            format!("git version {digits}.{digits}.0")
        }),
    ]
}

fn arb_status_bytes() -> impl Strategy<Value = Vec<u8>> {
    (
        arb_record_bytes(64),
        arb_record_bytes(48),
        0u8..6,
        any::<bool>(),
        prop::collection::vec(any::<u8>(), 0..32),
    )
        .prop_map(|(oid, name, kind, noise, extra)| {
            // 0 branch, 1 detached, 2 unborn, 3 detached+initial, 4 missing oid, 5 missing head.
            let oid_val = if kind == 2 || kind == 3 {
                b"(initial)".to_vec()
            } else {
                oid
            };
            let head_val = if kind == 1 || kind == 3 {
                b"(detached)".to_vec()
            } else {
                name
            };
            let mut out = Vec::new();
            if kind != 4 {
                out.extend(b"# branch.oid ");
                out.extend(oid_val);
                out.push(0);
            }
            if kind != 5 {
                out.extend(b"# branch.head ");
                out.extend(head_val);
                out.push(0);
            }
            if noise {
                out.extend(extra);
                out.push(0);
            }
            out
        })
}

fn arb_head_bytes() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        3 => arb_bytes(),
        2 => arb_status_bytes(),
    ]
}

fn last_porcelain_headers(stdout: &[u8]) -> (Option<String>, Option<String>) {
    let mut oid = None;
    let mut head = None;
    for record in stdout.split(|byte| *byte == 0) {
        let Some(header) = record.strip_prefix(b"# ") else {
            continue;
        };
        let header = String::from_utf8_lossy(header);
        if let Some(value) = header.strip_prefix("branch.oid ") {
            oid = Some(value.to_owned());
        } else if let Some(value) = header.strip_prefix("branch.head ") {
            head = Some(value.to_owned());
        }
    }
    (oid, head)
}

fn reference_head(stdout: &[u8]) -> Option<Head> {
    let (oid, head) = last_porcelain_headers(stdout);
    let head = head?;
    let oid = oid?;
    Some(match (head.as_str(), oid.as_str()) {
        ("(detached)", "(initial)") => return None,
        ("(detached)", oid) => Head::Detached {
            short_oid: short_oid(oid).to_owned(),
        },
        (_, "(initial)") => Head::Unborn(head),
        _ => Head::Branch(head),
    })
}

fn arb_hex_oid() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop::sample::select(b"0123456789abcdefABCDEF".to_vec()),
        1..64,
    )
    .prop_map(|bytes| String::from_utf8(bytes).expect("hex is ascii"))
}

fn arb_log_records() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(
        (arb_hex_oid(), prop::collection::vec(any::<u8>(), 0..48)),
        0..6,
    )
    .prop_map(|records| {
        let mut out = Vec::new();
        for (oid, summary) in records {
            out.extend(oid.as_bytes());
            out.push(0x1f);
            out.extend(summary);
            out.push(0);
        }
        out
    })
}

fn arb_log_bytes() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        3 => arb_bytes(),
        2 => arb_log_records(),
    ]
}

fn reference_log(stdout: &[u8]) -> Option<Vec<(String, String)>> {
    let mut commits = Vec::new();
    for record in stdout.split(|byte| *byte == 0) {
        if record.is_empty() {
            continue;
        }
        let separator = record.iter().position(|byte| *byte == 0x1f)?;
        let (oid, summary) = record.split_at(separator);
        let summary = &summary[1..];
        if oid.is_empty() || !oid.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        commits.push((
            String::from_utf8_lossy(oid).into_owned(),
            String::from_utf8_lossy(summary).into_owned(),
        ));
    }
    Some(commits)
}

fn arb_stderr() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => arb_text(160),
        1 => prop::collection::vec(any::<u8>(), 0..4096).prop_map(|bytes| {
            String::from_utf8_lossy(&bytes).into_owned()
        }),
        2 => arb_text(80).prop_map(|text| format!("{text}not a git repository{text}")),
        2 => (arb_text(48), arb_text(48)).prop_map(|(path, tail)| {
            format!("fatal: detected dubious ownership in repository at '{path}'\n{tail}")
        }),
        1 => Just(
            "fatal: not a git repository (or any of the parent directories): .git".to_owned(),
        ),
    ]
}

/// Path inside the first `'...'` after `repository at '`, which is what
/// `classify_failure` keeps today (see report G2).
fn dubious_quoted_path(stderr: &str) -> Option<&str> {
    let (_, rest) = stderr.split_once("repository at '")?;
    let (path, _) = rest.split_once('\'')?;
    Some(path)
}

fn arb_path_string() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => arb_text(24),
        1 => (arb_text(8), arb_text(8)).prop_map(|(parent, name)| format!("/{parent}/{name}")),
    ]
}

fn arb_recents() -> impl Strategy<Value = Vec<PathBuf>> {
    (
        prop::collection::vec(arb_path_string(), 0..14),
        any::<bool>(),
        any::<prop::sample::Index>(),
    )
        .prop_map(|(paths, duplicate, index)| {
            let mut paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
            if duplicate && !paths.is_empty() {
                let pick = index.get(&paths).clone();
                paths.insert(0, pick);
            }
            if let Some(first) = paths.first() {
                let mut text = first.display().to_string();
                if !text.ends_with('/') {
                    text.push('/');
                    paths.push(PathBuf::from(text));
                }
            }
            paths
        })
}

fn normalized_recents(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut kept = Vec::new();
    for path in paths {
        if !kept.iter().any(|seen| seen == path) {
            kept.push(path.clone());
        }
    }
    kept.truncate(MAX_RECENT_REPOSITORIES);
    kept
}

fn assert_recents_normalized(recents: &[PathBuf]) {
    assert!(recents.len() <= MAX_RECENT_REPOSITORIES);
    for (index, path) in recents.iter().enumerate() {
        assert!(
            !recents[index + 1..].iter().any(|later| later == path),
            "duplicate recent {}",
            path.display()
        );
    }
}

fn assert_version_order(left: &GitVersion, right: &GitVersion) {
    assert_eq!(left.cmp(right), right.cmp(left).reverse());
    assert_eq!(left == right, left.cmp(right) == Ordering::Equal);
    match left.numbers().cmp(&right.numbers()) {
        Ordering::Equal => assert_eq!(left.cmp(right), left.raw.cmp(&right.raw)),
        numeric => assert_eq!(left.cmp(right), numeric),
    }
    if left.numbers() <= right.numbers() && left.is_supported() {
        assert!(right.is_supported());
    }
    if right.numbers() <= left.numbers() && right.is_supported() {
        assert!(left.is_supported());
    }
}

proptest! {
    #![proptest_config(proptest_config())]

    #[test]
    fn git_version_parse_never_panics_and_matches_numeric_prefix(input in arb_version_input()) {
        assert_eq!(MIN_GIT_VERSION, (2, 15, 0));
        let parsed = GitVersion::parse(&input);
        let expected = reference_version(&input);
        match (parsed, expected) {
            (None, None) => {}
            (Some(parsed), Some((numbers, raw))) => {
                assert_eq!(parsed.numbers(), numbers, "{input:?}");
                assert_eq!(parsed.numbers(), (parsed.major, parsed.minor, parsed.patch));
                assert_eq!(parsed.raw, raw);
                assert!(!parsed.raw.is_empty());
                assert_eq!(parsed.raw.trim(), parsed.raw);
                assert_eq!(parsed.to_string(), parsed.raw);
                assert_eq!(parsed.is_supported(), parsed.numbers() >= (2, 15, 0));
            }
            (Some(parsed), None) => panic!("parse accepted {input:?} as {parsed:?}"),
            (None, Some(expected)) => panic!("parse rejected {input:?}, reference {expected:?}"),
        }
    }

    #[test]
    fn git_version_ordering_matches_numeric_tuple(left in arb_git_version(), right in arb_git_version()) {
        assert_version_order(&left, &right);
        assert_eq!(left.is_supported(), left.numbers() >= (2, 15, 0));
        assert_eq!(right.is_supported(), right.numbers() >= (2, 15, 0));
    }

    #[test]
    fn parse_head_never_panics(bytes in arb_head_bytes()) {
        let parsed = parse_head(&bytes);
        assert_eq!(parsed, reference_head(&bytes));
        if let Some(head) = parsed {
            let rendered = head.to_string();
            match &head {
                Head::Detached { short_oid } => {
                    let (oid, _) = last_porcelain_headers(&bytes);
                    let oid = oid.expect("detached head has an oid");
                    assert!(oid.starts_with(short_oid));
                    // ASCII hex always cuts cleanly. A non-boundary cut currently
                    // keeps the whole oid; see `parse_head_detached_short_oid_is_not_capped_at_twelve`.
                    let end = SHORT_OID_LEN.min(oid.len());
                    if oid.is_char_boundary(end) {
                        assert_eq!(short_oid.len(), end);
                    }
                    if short_oid.is_ascii() {
                        assert!(short_oid.len() <= SHORT_OID_LEN);
                    }
                    assert!(rendered.contains(short_oid));
                }
                Head::Branch(name) | Head::Unborn(name) => {
                    // Empty names are accepted by the parser.
                    assert!(rendered.starts_with(name));
                }
            }
        }
    }

    #[test]
    fn parse_log_never_panics_and_oids_are_hex(bytes in arb_log_bytes()) {
        let parsed = parse_log(&bytes);
        if let Some(commits) = &parsed {
            for commit in commits {
                assert!(!commit.oid.is_empty());
                assert!(commit.oid.bytes().all(|byte| byte.is_ascii_hexdigit()));
                assert!(std::str::from_utf8(commit.summary.as_bytes()).is_ok());
                let short = commit.short_oid();
                assert!(commit.oid.starts_with(short));
                assert!(short.len() <= SHORT_OID_LEN);
                assert!(short.len() <= commit.oid.len());
                assert_eq!(short.len(), SHORT_OID_LEN.min(commit.oid.len()));
            }
        }
        let viewed = parsed.map(|commits| {
            commits
                .into_iter()
                .map(|commit| (commit.oid, commit.summary))
                .collect::<Vec<_>>()
        });
        assert_eq!(viewed, reference_log(&bytes));
    }

    #[test]
    fn short_oid_is_a_char_safe_prefix(text in arb_text(80)) {
        let got = short_oid(&text);
        assert!(text.starts_with(got));
        let end = SHORT_OID_LEN.min(text.len());
        if text.is_char_boundary(end) {
            assert_eq!(got, &text[..end]);
            assert_eq!(got.len(), end);
        }
    }

    #[test]
    fn classify_failure_never_panics(
        cwd in arb_text(80),
        args in prop::collection::vec(arb_text(24), 0..6),
        status in arb_text(32),
        stderr in arb_stderr(),
    ) {
        let cwd_path = Path::new(&cwd);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let error = classify_failure(cwd_path, &arg_refs, status.clone(), &stderr);
        let rendered = error.to_string();
        assert!(!rendered.is_empty());

        if stderr.contains("not a git repository") {
            match &error {
                GitError::NotARepository(path) => {
                    assert_eq!(path, cwd_path);
                    if !cwd.is_empty() {
                        assert!(rendered.contains(&cwd_path.display().to_string()));
                    }
                }
                other => panic!("expected NotARepository, got {other:?}"),
            }
        } else if stderr.contains("detected dubious ownership") {
            let GitError::UnsafeRepository(path) = &error else {
                panic!("expected UnsafeRepository, got {error:?}");
            };
            match dubious_quoted_path(&stderr) {
                // The first quoted slice is a prefix of the path that is kept.
                // Equality is not required: a later apostrophe currently truncates
                // the repository path (report G2), and a fix may keep the rest.
                Some(quoted) if !quoted.is_empty() => {
                    let shown = path.to_string_lossy();
                    assert!(!shown.is_empty());
                    assert!(
                        shown.starts_with(quoted),
                        "path {shown:?} does not start with quoted {quoted:?}"
                    );
                }
                Some(_) => {
                    // An empty quoted path is not required to fall back to cwd.
                    assert!(path.as_os_str().is_empty() || path == cwd_path);
                }
                None => assert_eq!(path, cwd_path),
            }
            if !path.as_os_str().is_empty() {
                assert!(rendered.contains(&path.display().to_string()));
            }
        } else {
            match &error {
                GitError::Failed {
                    command,
                    status: got_status,
                    stderr: got_stderr,
                } => {
                    assert_eq!(got_status, &status);
                    assert_eq!(got_stderr, &stderr);
                    assert_eq!(command, &format!("git {}", args.join(" ")));
                }
                other => panic!("expected Failed, got {other:?}"),
            }
        }
    }

    #[test]
    fn settings_round_trip_normalizes_recents(
        theme in prop::option::of(arb_text(32)),
        last in prop::option::of(arb_path_string()),
        recents in arb_recents(),
        hidden in any::<bool>(),
    ) {
        let settings = Settings {
            theme,
            last_repository: last.map(PathBuf::from),
            recent_repositories: recents,
            history_sidebar_hidden: hidden,
        };
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("nested").join(FILE_NAME);
        settings.save_to(&path).expect("save");
        let loaded = Settings::load_from(&path).expect("load");

        let expected_recents = normalized_recents(&settings.recent_repositories);
        assert_eq!(loaded.theme, settings.theme);
        assert_eq!(loaded.last_repository, settings.last_repository);
        assert_eq!(loaded.history_sidebar_hidden, settings.history_sidebar_hidden);
        assert_eq!(loaded.recent_repositories, expected_recents);
        assert_recents_normalized(&loaded.recent_repositories);

        loaded.save_to(&path).expect("save again");
        let again = Settings::load_from(&path).expect("reload");
        assert_eq!(again, loaded);

        let mut names: Vec<_> = fs::read_dir(path.parent().expect("parent"))
            .expect("read dir")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        names.sort();
        assert_eq!(names, [std::ffi::OsString::from(FILE_NAME)]);
    }

    #[test]
    fn settings_partial_json_uses_defaults_and_ignores_unknown(
        theme in prop::option::of(arb_text(24)),
        last in prop::option::of(arb_path_string()),
        recents in prop::option::of(prop::collection::vec(arb_path_string(), 0..14)),
        hidden in prop::option::of(any::<bool>()),
        unknown_key in "[a-z][a-z0-9]{0,11}",
        unknown_value in any::<i64>(),
    ) {
        let mut object = serde_json::Map::new();
        if let Some(theme) = &theme {
            object.insert("theme".to_owned(), serde_json::Value::String(theme.clone()));
        }
        if let Some(last) = &last {
            object.insert(
                "last_repository".to_owned(),
                serde_json::Value::String(last.clone()),
            );
        }
        if let Some(recents) = &recents {
            object.insert(
                "recent_repositories".to_owned(),
                serde_json::Value::Array(
                    recents
                        .iter()
                        .cloned()
                        .map(serde_json::Value::String)
                        .collect(),
                ),
            );
        }
        if let Some(hidden) = hidden {
            object.insert(
                "history_sidebar_hidden".to_owned(),
                serde_json::Value::Bool(hidden),
            );
        }
        object.insert(
            format!("future_{unknown_key}"),
            serde_json::Value::Number(unknown_value.into()),
        );

        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(FILE_NAME);
        let bytes = serde_json::to_vec(&serde_json::Value::Object(object)).expect("json");
        fs::write(&path, bytes).expect("write");
        let loaded = Settings::load_from(&path).expect("load");

        assert_eq!(loaded.theme, theme);
        assert_eq!(
            loaded.last_repository,
            last.as_ref().map(PathBuf::from)
        );
        assert_eq!(loaded.history_sidebar_hidden, hidden.unwrap_or(false));
        let expected = recents.unwrap_or_default();
        let expected: Vec<PathBuf> = expected.iter().map(PathBuf::from).collect();
        assert_eq!(loaded.recent_repositories, normalized_recents(&expected));
        assert_recents_normalized(&loaded.recent_repositories);
    }
}

#[test]
fn git_version_reference_table() {
    let cases = [
        ("", None),
        ("\0", None),
        ("git version ", None),
        ("git version \0", None),
        ("git version\n", None),
        ("version 2.43.0", None),
        ("git version 2.43.0\n", Some((2, 43, 0))),
        ("git version 2.15", Some((2, 15, 0))),
        ("git version 2", Some((2, 0, 0))),
        ("git version 2.", Some((2, 0, 0))),
        ("git version 2.15.", Some((2, 15, 0))),
        ("git version 2.14.3", Some((2, 14, 3))),
        ("git version 2.15.0.windows.1", Some((2, 15, 0))),
        ("git version 2.39.5 (Apple Git-154)", Some((2, 39, 5))),
        ("git version 2.15.0.1", Some((2, 15, 0))),
        ("git version 2..1", None),
        ("git version .1.2", None),
        ("git version 2.99999999999.0", None),
    ];
    for (input, expected) in cases {
        let parsed = GitVersion::parse(input).map(|version| version.numbers());
        assert_eq!(parsed, expected, "{input:?}");
        assert_eq!(
            parsed,
            reference_version(input).map(|(numbers, _)| numbers),
            "{input:?}"
        );
    }

    let supported = GitVersion::parse("git version 2.15.0").expect("version");
    let older = GitVersion::parse("git version 2.14.3").expect("version");
    assert!(supported.is_supported());
    assert!(!older.is_supported());
    assert!(older < supported);
}

#[test]
fn git_version_empty_and_interior_nul() {
    assert_eq!(GitVersion::parse(""), None);
    assert_eq!(GitVersion::parse("\0"), None);
    assert_eq!(GitVersion::parse("git version \0"), None);

    let parsed = GitVersion::parse("git version 2.16.0\0windows").expect("version");
    assert_eq!(parsed.numbers(), (2, 16, 0));
    assert!(parsed.raw.starts_with("2.16.0"));
    assert!(parsed.is_supported());
    assert_eq!(parsed.to_string(), parsed.raw);

    let floor = GitVersion::parse("git version 2.15.0").expect("version");
    assert!(parsed > floor);
    assert_eq!(parsed.cmp(&floor), parsed.numbers().cmp(&floor.numbers()));
}

#[test]
fn equal_numbers_break_ties_with_raw_only() {
    let plain = GitVersion::parse("git version 2.15.0").expect("version");
    let windows = GitVersion::parse("git version 2.15.0.windows.1").expect("version");
    assert_eq!(plain.numbers(), windows.numbers());
    assert_eq!(plain.numbers(), (2, 15, 0));
    assert!(plain < windows);
    assert_eq!(plain.cmp(&windows), plain.raw.cmp(&windows.raw));
    assert!(plain.is_supported());
    assert!(windows.is_supported());
}

#[test]
fn parse_head_empty_and_interior_nul() {
    assert_eq!(parse_head(b""), None);
    assert_eq!(parse_head(&[0, 0, 0]), None);

    let mut bytes = b"# branch.oid ".to_vec();
    bytes.extend(b"abc\0def");
    bytes.push(0);
    bytes.extend(b"# branch.head ma\0in\0");
    let head = parse_head(&bytes).expect("head");
    assert_eq!(head, Head::Branch("ma".to_owned()));
    assert_eq!(head.to_string(), "ma");

    let unborn = b"# branch.oid (initial)\0# branch.head \0";
    assert_eq!(
        parse_head(unborn),
        Some(Head::Unborn(String::new())),
        "empty branch names are accepted"
    );
    assert_eq!(
        parse_head(unborn).expect("unborn").to_string(),
        " (no commits yet)"
    );
}

#[test]
fn parse_log_empty_and_interior_nul() {
    assert_eq!(parse_log(b""), Some(Vec::new()));
    assert_eq!(parse_log(&[0, 0, 0]), Some(Vec::new()));
    assert_eq!(parse_log(b"ab\0cd\x1fsum"), None);

    let commits = parse_log(b"abcd\x1fsum\0ff\x1fnext").expect("log");
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].oid, "abcd");
    assert_eq!(commits[0].summary, "sum");
    assert_eq!(commits[0].short_oid(), "abcd");
    assert!(std::str::from_utf8(commits[0].summary.as_bytes()).is_ok());
    assert_eq!(commits[1].oid, "ff");
    assert_eq!(commits[1].summary, "next");
    assert!(commits[1].short_oid().len() <= commits[1].oid.len());
    assert!(commits[1].short_oid().len() <= SHORT_OID_LEN);
}

#[test]
fn short_oid_ascii_len_is_min_twelve() {
    let long = "deadbeef".repeat(8);
    for text in ["", "a", "0123456789ab", "0123456789abc", long.as_str()] {
        let got = short_oid(text);
        assert!(text.starts_with(got));
        assert_eq!(got.len(), SHORT_OID_LEN.min(text.len()));
    }
}

/// Candidate bug G1. `str::get` refuses to split a multibyte character, and the
/// fallback returns the whole string instead of a shorter prefix.
#[test]
fn short_oid_mid_character_returns_the_entire_string() {
    let oid = format!("{}é", "a".repeat(11));
    assert_eq!(oid.len(), 13);
    assert!(!oid.is_char_boundary(SHORT_OID_LEN));
    assert_eq!(short_oid(&oid), oid);
    assert_eq!(short_oid(&oid).len(), 13);

    let mut long = oid;
    long.push_str(&"z".repeat(100));
    assert_eq!(short_oid(&long), long);
    assert_eq!(short_oid(&long).len(), 113);
}

/// Same failure as G1, reached through detached `branch.oid` bytes.
#[test]
fn parse_head_detached_short_oid_is_not_capped_at_twelve() {
    let mut stdout = Vec::new();
    stdout.extend(b"# branch.oid ");
    stdout.extend("a".repeat(11).as_bytes());
    stdout.extend([0xc3, 0xa9]);
    stdout.extend(b"zzzz");
    stdout.push(0);
    stdout.extend(b"# branch.head (detached)");
    stdout.push(0);

    let head = parse_head(&stdout).expect("head");
    let Head::Detached { short_oid } = &head else {
        panic!("expected detached, got {head:?}");
    };
    assert!(short_oid.len() > SHORT_OID_LEN, "{short_oid:?}");
    assert!(short_oid.ends_with("zzzz"), "{short_oid:?}");
    assert!(head.to_string().contains(short_oid));
}

#[test]
fn classify_failure_empty_and_interior_nul() {
    let empty = classify_failure(Path::new(""), &[], String::new(), "");
    match &empty {
        GitError::Failed {
            command,
            status,
            stderr,
        } => {
            assert_eq!(command, "git ");
            assert_eq!(status, "");
            assert_eq!(stderr, "");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert_eq!(empty.to_string(), "`git ` failed ()");

    let error = classify_failure(
        Path::new("/tmp/a\0b"),
        &["status", "arg\0two"],
        "exit\0status".to_owned(),
        "fatal: not a git repository\0still the same string",
    );
    assert!(matches!(error, GitError::NotARepository(ref path) if path == Path::new("/tmp/a\0b")));
    assert!(error.to_string().contains("not inside a Git repository"));

    let dubious = classify_failure(
        Path::new("/cwd"),
        &["status"],
        "exit status: 128".to_owned(),
        "fatal: detected dubious ownership in repository at '/tmp/a\0b'",
    );
    assert!(
        matches!(dubious, GitError::UnsafeRepository(ref path) if path == Path::new("/tmp/a\0b"))
    );
    let _ = dubious.to_string();
}

/// Candidate bug G3. The phrase is not anchored, and it wins over dubious ownership.
#[test]
fn not_a_repository_substring_misclassifies_dubious_ownership() {
    let stderr = "\
fatal: detected dubious ownership in repository at '/home/not a git repository'
To add an exception for this directory, call:

\tgit config --global --add safe.directory '/home/not a git repository'";
    let error = classify_failure(
        Path::new("/cwd"),
        &["status", "--porcelain=v2"],
        "exit status: 128".to_owned(),
        stderr,
    );
    assert!(
        matches!(error, GitError::NotARepository(ref path) if path == Path::new("/cwd")),
        "got {error:?}"
    );
    let rendered = error.to_string();
    assert!(rendered.contains("is not inside a Git repository"));
    assert!(
        !rendered.contains("safe.directory"),
        "actionable ownership hint was dropped: {rendered}"
    );

    let mentioned = classify_failure(
        Path::new("/cwd"),
        &["status"],
        "exit status: 1".to_owned(),
        "note: the handbook says this is not a git repository wording; real fault is permissions",
    );
    assert!(matches!(mentioned, GitError::NotARepository(_)));
}

/// Candidate bug G2. Git inserts the path unescaped inside single quotes.
#[test]
fn dubious_ownership_apostrophe_truncates_the_path() {
    let stderr = "\
fatal: detected dubious ownership in repository at '/srv/o'brien/repo'
To add an exception for this directory, call:

\tgit config --global --add safe.directory '/srv/o'\\''brien/repo'";
    let error = classify_failure(
        Path::new("/cwd"),
        &["rev-parse", "--show-toplevel"],
        "exit status: 128".to_owned(),
        stderr,
    );
    let GitError::UnsafeRepository(path) = &error else {
        panic!("expected UnsafeRepository, got {error:?}");
    };
    assert_eq!(path, Path::new("/srv/o"));
    let rendered = error.to_string();
    assert!(rendered.contains("safe.directory /srv/o"));
    assert!(!rendered.contains("brien"));
}

/// Candidate bug G4. `Some("")` skips the cwd fallback.
#[test]
fn dubious_ownership_empty_quotes_keep_an_empty_path() {
    let error = classify_failure(
        Path::new("/actual/cwd"),
        &["status"],
        "exit status: 128".to_owned(),
        "fatal: detected dubious ownership in repository at ''",
    );
    let GitError::UnsafeRepository(path) = &error else {
        panic!("expected UnsafeRepository, got {error:?}");
    };
    assert!(path.as_os_str().is_empty(), "{}", path.display());
    let rendered = error.to_string();
    assert!(rendered.contains("safe.directory"));
    assert!(!rendered.contains("/actual/cwd"));
}
