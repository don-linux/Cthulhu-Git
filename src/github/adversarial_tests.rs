//! Extra cases for URL rules, remotes, the API client and the resolver,
//! all through the fake transport.
//!
//! Nothing here opens a socket or touches the system keychain.

use std::sync::Arc;

use proptest::prelude::*;
use proptest::string::string_regex;

use super::client::{FetchError, fetch_image};
use super::http::{allowed_host, get};
use super::*;
use crate::test_support::{
    FakeTransport, NOW, auth_value, commit_json, context, list_url, resolver,
};

const ALLOWED: &[&str] = &[
    "api.github.com",
    "avatars.githubusercontent.com",
    "github.com",
    "gravatar.com",
    "www.gravatar.com",
    "secure.gravatar.com",
];

fn token() -> Token {
    Token::new("ghp_test_token_value_0123456789")
}

#[test]
fn hostile_author_avatar_urls_never_leave_the_allowlist_or_leak_tokens_in_errors() {
    const SENTINEL: &str = "ghp_SentinelTokenValue0000";
    let avatars = [
        "https://evil.example/a.png",
        "http://avatars.githubusercontent.com/u/1",
        "https://ghp_SentinelTokenValue0000@evil.example/a.png",
        "https://avatars.githubusercontent.com.evil.example/u/1",
    ];
    let context = context();
    let email = "person@example.com";
    let emails = vec![email.to_owned()];

    for (index, avatar) in avatars.iter().enumerate() {
        let fake = Arc::new(FakeTransport::default());
        let sha = format!("{index:040x}");
        let warm = list_url(100, Some("aaa111"));
        fake.reply(
            &warm,
            200,
            &[("x-ratelimit-remaining", "50")],
            format!("[{}]", commit_json(&sha, email, Some(avatar))).as_bytes(),
        );
        let mut resolver = resolver(&fake);
        let urls = resolver.candidates(
            &AvatarQuery {
                emails: &emails,
                repo: Some(&context),
                authored: Some(&sha),
            },
            NOW,
        );
        assert!(!urls.is_empty(), "case {index}");
        let transport = fake.clone() as Arc<dyn Transport>;
        for url in &urls {
            if let Err(FetchError::Failed(message)) = fetch_image(transport.as_ref(), url) {
                assert!(
                    !message.contains(SENTINEL),
                    "fetch_image leaked the sentinel in {message}"
                );
            }
            if let Err(error) = get(transport.as_ref(), url, None) {
                let message = error.to_string();
                assert!(
                    !message.contains(SENTINEL),
                    "get leaked the sentinel in {message}"
                );
            }
        }
        for (url, _) in fake.requests.lock().expect("requests").iter() {
            let host = allowed_host(url).unwrap_or_else(|error| {
                panic!("requested a refused url {url}: {error}");
            });
            assert!(ALLOWED.contains(&host.as_str()), "{url} reached {host}");
        }
        assert!(
            fake.urls().iter().all(|url| {
                allowed_host(url).is_ok_and(|host| ALLOWED.contains(&host.as_str()))
            }),
            "case {index}: {:?}",
            fake.urls()
        );
    }
}

#[test]
fn token_parse_bounds_and_debug_hide_the_value() {
    let value = "a".repeat(20);
    let parsed = Token::parse(&format!(" \t{value}\n")).expect("trimmed");
    assert_eq!(parsed.expose(), value);
    assert!(!parsed.expose().chars().any(|ch| ch.is_whitespace()));
    assert_eq!(format!("{parsed:?}"), "Token(…)");
    assert!(!format!("{parsed:?}").contains(&value));
    assert!(!format!("{:?}", Some(parsed)).contains(&value));
    assert!(Token::parse(&"a".repeat(19)).is_none());
    assert!(Token::parse(&"a".repeat(255)).is_some());
    assert!(Token::parse(&"a".repeat(256)).is_none());
    assert!(Token::parse("ghp_token with a space inside").is_none());
}

fn arb_token_input() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => prop::collection::vec(any::<char>(), 0..=80).prop_map(|chars| chars.into_iter().collect()),
        2 => (string_regex("[A-Za-z0-9_-]{20,80}").unwrap(), any::<bool>()).prop_map(
            |(token, pad)| {
                if pad {
                    format!(" \t{token}\r\n")
                } else {
                    token
                }
            }
        ),
        1 => string_regex("[A-Za-z0-9_-]{10,30} [A-Za-z0-9_-]{10,30}").unwrap(),
    ]
}

fn arb_urlish() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => prop::collection::vec(any::<u8>(), 0..512).prop_map(|bytes| {
            String::from_utf8_lossy(&bytes).into_owned()
        }),
        1 => string_regex("https://[A-Za-z0-9.-]{1,40}/[A-Za-z0-9._~-]{0,20}").unwrap(),
    ]
}

proptest! {
    #![proptest_config(crate::test_support::proptest_config())]

    #[test]
    fn allowed_host_only_returns_allowlisted_hosts(url in arb_urlish()) {
        if let Ok(host) = allowed_host(&url) {
            prop_assert!(ALLOWED.contains(&host.as_str()), "{url} -> {host}");
        }
    }

    #[test]
    fn get_does_not_authorize_a_random_url(url in arb_urlish()) {
        let fake = FakeTransport::default();
        let secret = token();
        let expected = format!("Bearer {}", secret.expose());
        let _ = get(&fake, &url, Some(&secret));
        let requests = fake.requests.lock().expect("requests");
        for (requested, headers) in requests.iter() {
            let host = allowed_host(requested).unwrap_or_else(|error| {
                panic!("{requested} was sent ({error})");
            });
            prop_assert!(ALLOWED.contains(&host.as_str()), "{requested}");
            match auth_value(headers) {
                Some(value) => {
                    prop_assert_eq!(host, "api.github.com");
                    prop_assert_eq!(value, expected.as_str());
                }
                None => prop_assert_ne!(host, "api.github.com"),
            }
        }
    }

    #[test]
    fn token_parse_output_has_no_whitespace_and_debug_hides_it(input in arb_token_input()) {
        let trimmed = input.trim();
        let plausible = (20..=255).contains(&trimmed.len())
            && trimmed
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
        match Token::parse(&input) {
            Some(parsed) => {
                prop_assert!(plausible, "{trimmed:?}");
                prop_assert_eq!(parsed.expose(), trimmed);
                prop_assert!(parsed.expose().chars().all(|ch| !ch.is_whitespace()));
                let debug = format!("{parsed:?}");
                let wrapped = format!("Some({parsed:?})");
                prop_assert_eq!(&debug, "Token(…)");
                prop_assert!(!debug.contains(parsed.expose()));
                prop_assert!(!wrapped.contains(trimmed));
            }
            None => prop_assert!(!plausible, "{trimmed:?}"),
        }
    }
}
