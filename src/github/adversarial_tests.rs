//! Hostile input against the GitHub code as a whole, through the fake
//! transport: avatar URLs a listing hands back, random URLs, and errors
//! that must never show the user's token.
//!
//! Nothing here opens a socket or touches the system keychain.

use std::sync::Arc;

use proptest::prelude::*;
use proptest::string::string_regex;

use super::client::{FetchError, commit_accounts, fetch_image};
use super::http::{allowed_host, get};
use super::*;
use crate::test_support::{
    FakeTransport, NOW, auth_value, commit_json, context, list_url, resolver,
};

/// A user's token; no other test input contains it.
const SECRET: &str = "ghp_SentinelTokenValue0000aaaa";

#[test]
fn hostile_author_avatar_urls_never_leave_the_allowlist() {
    let userinfo = format!("https://{SECRET}@evil.example/a.png");
    let avatars = [
        "https://evil.example/a.png",
        "http://avatars.githubusercontent.com/u/1",
        userinfo.as_str(),
        "https://avatars.githubusercontent.com.evil.example/u/1",
    ];
    let context = context();
    let emails = ["person@example.com".to_owned()];
    for (index, avatar) in avatars.into_iter().enumerate() {
        let fake = Arc::new(FakeTransport::default());
        let sha = format!("{index:040x}");
        fake.reply(
            &list_url(100, Some("aaa111")),
            200,
            &[("x-ratelimit-remaining", "50")],
            format!("[{}]", commit_json(&sha, &emails[0], Some(avatar))).as_bytes(),
        );
        let query = AvatarQuery {
            emails: &emails,
            repo: Some(&context),
            authored: Some(&sha),
        };
        let urls = resolver(&fake).resolve(&query, NOW).urls;
        assert!(!urls.is_empty(), "{avatar}");
        for url in &urls {
            if let Err(FetchError::Failed(message)) = fetch_image(fake.as_ref(), url) {
                assert!(!message.contains(SECRET), "{avatar}: {message}");
            }
            if let Err(error) = get(fake.as_ref(), url, None) {
                assert!(!error.to_string().contains(SECRET), "{avatar}: {error}");
            }
        }
        for url in fake.urls() {
            assert!(allowed_host(&url).is_ok(), "{avatar}: requested {url}");
        }
    }
}

/// Neither a refused or broken request nor a GitHub error answer carries
/// the token into its message, even when a URL or a body holds it.
#[test]
fn errors_never_show_the_token() {
    let token = Token::parse(SECRET).expect("token");
    let fake = FakeTransport::default();
    let leaky = format!("https://evil.example/?access_token={SECRET}");
    fake.reply(
        "https://api.github.com/away",
        302,
        &[("Location", &leaky)],
        b"",
    );
    fake.reply(
        "https://api.github.com/loop",
        302,
        &[("Location", "/loop")],
        b"",
    );
    fake.reply("https://api.github.com/bare", 302, &[], b"");
    let mut shown = Vec::new();
    for url in [
        leaky.as_str(),
        "https://api.github.com/away",
        "https://api.github.com/loop",
        "https://api.github.com/bare",
    ] {
        let error = get(&fake, url, Some(&token)).expect_err(url);
        shown.push(format!("{error} {error:?}"));
    }
    let repo = context().repo;
    for status in [200, 401, 403, 404, 422, 500] {
        fake.reply(&list_url(100, None), status, &[], SECRET.as_bytes());
        let error =
            commit_accounts(&fake, Some(&token), &repo, None, 100, NOW).expect_err("no listing");
        shown.push(format!("{error} {error:?}"));
    }
    for message in shown {
        assert!(!message.contains(SECRET), "{message}");
    }
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

    /// No URL makes `get` panic, request a refused host, or send the
    /// token anywhere but the API. The allowlist itself is pinned in http.rs.
    #[test]
    fn get_does_not_authorize_a_random_url(url in arb_urlish()) {
        let fake = FakeTransport::default();
        let token = Token::parse(SECRET).expect("token");
        let _ = get(&fake, &url, Some(&token));
        let expected = format!("Bearer {SECRET}");
        for (requested, headers) in fake.requests.lock().expect("requests").iter() {
            let host = allowed_host(requested);
            prop_assert!(host.is_ok(), "{requested} was sent");
            let on_api = host.as_deref() == Ok("api.github.com");
            prop_assert_eq!(auth_value(headers), on_api.then_some(expected.as_str()));
        }
    }
}
