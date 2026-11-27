//! Extra parser cases for remotes.
//!
//! No git process is spawned.

use proptest::prelude::*;
use proptest::string::string_regex;

use super::remotes::parse_remote_config;
use super::*;

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
