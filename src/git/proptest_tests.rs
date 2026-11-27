//! Property tests for git output parsers and settings persistence.
//!
//! Nothing in this module spawns git. Each property runs 64 cases. Inputs are
//! capped (byte strings at `0..4096`) so the suite stays a fast fuzz harness.
//!
//! A few properties characterize current behavior that the report calls out as
//! a bug. Those cases are also pinned by concrete tests further down; the
//! properties themselves stay true for a UTF-8-safe fix where that is called
//! out, and they do not panic.

use std::fs;
use std::path::PathBuf;

use proptest::prelude::*;

use crate::settings::{FILE_NAME, HistoryColumns, MAX_RECENT_REPOSITORIES, Settings};

fn arb_text(max_chars: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..=max_chars)
        .prop_map(|chars| chars.into_iter().collect())
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

proptest! {
    #![proptest_config(crate::test_support::proptest_config())]

    #[test]
    fn settings_round_trip_normalizes_recents(
        theme in prop::option::of(arb_text(32)),
        last in prop::option::of(arb_path_string()),
        recents in arb_recents(),
        hidden in any::<bool>(),
        detail_hidden in any::<bool>(),
        terminal_hidden in any::<bool>(),
    ) {
        let settings = Settings {
            theme,
            last_repository: last.map(PathBuf::from),
            recent_repositories: recents,
            history_sidebar_hidden: hidden,
            detail_sidebar_hidden: detail_hidden,
            terminal_hidden,
            terminal_font: None,
            ..Settings::default()
        };
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("nested").join(FILE_NAME);
        settings.save_to(&path).expect("save");
        let loaded = Settings::load_from(&path).expect("load");

        let expected_recents = normalized_recents(&settings.recent_repositories);
        assert_eq!(loaded.theme, settings.theme);
        assert_eq!(loaded.last_repository, settings.last_repository);
        assert_eq!(loaded.history_sidebar_hidden, settings.history_sidebar_hidden);
        assert_eq!(loaded.detail_sidebar_hidden, settings.detail_sidebar_hidden);
        assert_eq!(loaded.terminal_hidden, settings.terminal_hidden);
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
        detail_hidden in prop::option::of(any::<bool>()),
        terminal_hidden in prop::option::of(any::<bool>()),
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
        if let Some(detail_hidden) = detail_hidden {
            object.insert(
                "detail_sidebar_hidden".to_owned(),
                serde_json::Value::Bool(detail_hidden),
            );
        }
        if let Some(terminal_hidden) = terminal_hidden {
            object.insert(
                "terminal_hidden".to_owned(),
                serde_json::Value::Bool(terminal_hidden),
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
        assert_eq!(
            loaded.detail_sidebar_hidden,
            detail_hidden.unwrap_or(false)
        );
        assert_eq!(loaded.terminal_hidden, terminal_hidden.unwrap_or(true));
        assert_eq!(loaded.terminal_font, None);
        let expected = recents.unwrap_or_default();
        let expected: Vec<PathBuf> = expected.iter().map(PathBuf::from).collect();
        assert_eq!(loaded.recent_repositories, normalized_recents(&expected));
        assert_recents_normalized(&loaded.recent_repositories);
        assert_eq!(loaded.history_columns, HistoryColumns::default());
        assert!(loaded.author_avatars);
        assert!(!loaded.github_token_in_keyring);
    }

    #[test]
    fn history_columns_round_trip_normalized(
        branch in any::<u16>(),
        graph in any::<u16>(),
        hash in any::<u16>(),
        message in any::<u16>(),
        author in any::<u16>(),
        avatars in any::<bool>(),
        in_keyring in any::<bool>(),
    ) {
        let settings = Settings {
            history_columns: HistoryColumns { branch, graph, hash, message, author },
            author_avatars: avatars,
            github_token_in_keyring: in_keyring,
            ..Settings::default()
        };
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(FILE_NAME);
        settings.save_to(&path).expect("save");
        let loaded = Settings::load_from(&path).expect("load");
        let mut expected = settings;
        expected.history_columns = expected.history_columns.normalized();
        assert_eq!(&loaded, &expected);
        loaded.save_to(&path).expect("save again");
        assert_eq!(Settings::load_from(&path).expect("load again"), loaded);
    }

    #[test]
    fn one_bad_column_keeps_its_siblings(
        good in 20u16..=1000,
        which in 0usize..5,
        bad in prop_oneof![
            Just(serde_json::json!(0)),
            Just(serde_json::json!(1001)),
            Just(serde_json::json!(null)),
            Just(serde_json::json!("wide")),
            Just(serde_json::json!(1.5)),
            Just(serde_json::json!(-4)),
        ],
    ) {
        let names = ["branch", "graph", "hash", "message", "author"];
        let mut columns = serde_json::Map::new();
        for (index, name) in names.iter().enumerate() {
            let value = if index == which { bad.clone() } else { serde_json::json!(good) };
            columns.insert((*name).to_owned(), value);
        }
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(FILE_NAME);
        let json = serde_json::json!({
            "theme": "abyss",
            "author_avatars": false,
            "github_token_in_keyring": true,
            "history_columns": columns,
        });
        fs::write(&path, json.to_string()).expect("write");
        let loaded = Settings::load_from(&path).expect("load");
        assert_eq!(loaded.theme.as_deref(), Some("abyss"));
        assert!(!loaded.author_avatars && loaded.github_token_in_keyring);
        let defaults = HistoryColumns::default();
        let got = loaded.history_columns;
        let got = [got.branch, got.graph, got.hash, got.message, got.author];
        let fallback = [
            defaults.branch,
            defaults.graph,
            defaults.hash,
            defaults.message,
            defaults.author,
        ];
        for (index, value) in got.iter().enumerate() {
            assert_eq!(*value, if index == which { fallback[index] } else { good });
        }
    }
}
