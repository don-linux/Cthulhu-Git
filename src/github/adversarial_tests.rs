//! Extra cases for URL rules, remotes, the API client and the resolver,
//! all through the fake transport.
//!
//! Nothing here opens a socket or touches the system keychain.

use std::sync::Arc;

use proptest::prelude::*;
use proptest::string::string_regex;
use proptest::test_runner::FileFailurePersistence;

use super::client::{check, parse_commit_accounts};
use super::http::fake::FakeTransport;
use super::http::{allowed_host, get};
use super::*;

const ALLOWED: &[&str] = &[
    "api.github.com",
    "avatars.githubusercontent.com",
    "github.com",
    "gravatar.com",
    "www.gravatar.com",
    "secure.gravatar.com",
];

const NOW: u64 = 1_800_000_000;
const LIST: &str = "https://api.github.com/repos/don-linux/Cthulhu-Git/commits";

fn proptest_config() -> ProptestConfig {
    ProptestConfig {
        cases: 64,
        failure_persistence: Some(Box::new(FileFailurePersistence::Off)),
        ..ProptestConfig::default()
    }
}

fn token() -> Token {
    Token::new("ghp_test_token_value_0123456789")
}

fn response(status: u16, headers: &[(&str, &str)]) -> HttpResponse {
    HttpResponse {
        status,
        headers: headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
        body: Vec::new(),
    }
}

fn auth_value(headers: &[(String, String)]) -> Option<&str> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("Authorization"))
        .map(|(_, value)| value.as_str())
}

fn assert_token_only_on_api(fake: &FakeTransport, token: &Token) {
    let requests = fake.requests.lock().expect("requests");
    for (url, headers) in requests.iter() {
        let host = allowed_host(url).unwrap_or_else(|error| {
            panic!("requested a refused url {url}: {error}");
        });
        assert!(ALLOWED.contains(&host.as_str()), "{url} reached {host}");
        let expected = format!("Bearer {}", token.expose());
        match auth_value(headers) {
            Some(value) => {
                assert_eq!(host, "api.github.com", "{url}");
                assert_eq!(value, expected);
            }
            None => assert_ne!(host, "api.github.com", "{url} dropped the token"),
        }
    }
}

/// URL, status, headers and body of one canned reply.
type Reply<'a> = (&'a str, u16, &'a [(&'a str, &'a str)], &'a [u8]);

fn script(replies: &[Reply<'_>]) -> FakeTransport {
    let fake = FakeTransport::default();
    for (url, status, headers, body) in replies {
        fake.reply(url, *status, headers, body);
    }
    fake
}

fn context() -> RepoContext {
    RepoContext {
        repo: GitHubRepo {
            owner: "don-linux".to_owned(),
            name: "Cthulhu-Git".to_owned(),
        },
        head: "aaa111".to_owned(),
    }
}

fn resolver(fake: &Arc<FakeTransport>) -> AvatarResolver {
    AvatarResolver::new(Arc::clone(fake) as Arc<dyn Transport>)
}

fn commit_json(sha: &str, email: &str, avatar: Option<&str>) -> String {
    let author = match avatar {
        Some(url) => format!(r#"{{"avatar_url":"{url}"}}"#),
        None => "null".to_owned(),
    };
    format!(
        r#"{{"sha":"{sha}","commit":{{"author":{{"email":"{email}"}},"committer":null}},"author":{author},"committer":null}}"#
    )
}

fn list_url(per_page: u32, sha: Option<&str>) -> String {
    let mut url = format!("{LIST}?per_page={per_page}");
    if let Some(sha) = sha {
        url.push_str("&sha=");
        url.push_str(sha);
    }
    url
}

#[test]
fn allowed_host_rejects_tricky_urls() {
    let cases = [
        ("https://github.com/x", Some("github.com")),
        ("https://GITHUB.COM/x", Some("github.com")),
        ("https://API.GITHUB.COM/repos/a/b", Some("api.github.com")),
        (
            "https://api.github.com#@evil.example",
            Some("api.github.com"),
        ),
        ("https://github.com/foo%2Fbar", Some("github.com")),
        ("https://github.com/\\@evil.example", Some("github.com")),
        (
            "https://Avatars.GitHubUserContent.com/u/1",
            Some("avatars.githubusercontent.com"),
        ),
        ("https://gravatar.com/avatar/abc", Some("gravatar.com")),
        (
            "https://www.gravatar.com/avatar/abc",
            Some("www.gravatar.com"),
        ),
        (
            "https://secure.gravatar.com/avatar/abc",
            Some("secure.gravatar.com"),
        ),
        ("https://github.com./x", None),
        ("https://api.github.com.", None),
        ("https://[::1]/", None),
        ("https://[::1]:443/", None),
        ("https://127.0.0.1/", None),
        ("https://2130706433/", None),
        ("https://api.github.com@evil.example/", None),
        ("https://api.github.com%2e%2e@evil.example/", None),
        ("https://user:pass@api.github.com/", None),
        ("https://evil.example#@api.github.com", None),
        ("https://evil.example?x=@api.github.com", None),
        ("https://xn--gthub-cza.com/x", None),
        ("https://xn--80ak6aa92e.com/", None),
        ("https://github%2ecom/", None),
        ("https://%67ithub.com/", None),
        ("https://api.github.com%00.evil.example/", None),
        ("https://api.github.com%2fevil.example/", None),
        ("https://github.com\\@evil.example/", None),
        ("https://evil.example\\@api.github.com/", None),
        ("https://github.com/x y", None),
        (" https://github.com/x", None),
        ("https:// github.com/x", None),
        ("https://api.github.com /x", None),
        ("https://api.github.com\n.evil.example/", None),
        ("https://api.github.com\t.evil.example/", None),
        ("https://api.github.com.evil.example/", None),
        ("https://www.github.com/", None),
        ("http://api.github.com/", None),
        ("http://github.com/x", None),
        ("https://api.github.com:443/x", None),
        ("https:///evil.example/", None),
        ("file:///etc/passwd", None),
        ("not a url", None),
        ("", None),
    ];

    let secret = token();
    for (url, expect) in cases {
        match allowed_host(url) {
            Ok(host) => {
                assert!(ALLOWED.contains(&host.as_str()), "{url} reached {host}");
                assert_eq!(Some(host.as_str()), expect, "{url}");
            }
            Err(HttpError::NotAllowed(_)) => assert_eq!(expect, None, "{url} was refused"),
            Err(other) => panic!("{url} -> {other}"),
        }

        let fake = FakeTransport::default();
        let _ = get(&fake, url, Some(&secret));
        if expect.is_none() {
            assert!(
                fake.urls().is_empty(),
                "{url} was requested: {:?}",
                fake.urls()
            );
        } else {
            assert_eq!(fake.urls(), vec![url.to_owned()]);
            assert_token_only_on_api(&fake, &secret);
        }
    }

    let huge = format!("https://evil.example/{}", "a".repeat(80_000));
    assert!(allowed_host(&huge).is_err());
    let fake = FakeTransport::default();
    assert!(get(&fake, &huge, Some(&secret)).is_err());
    assert!(fake.urls().is_empty());

    let long_ok = format!("https://github.com/{}", "b".repeat(20_000));
    assert_eq!(allowed_host(&long_ok).as_deref(), Ok("github.com"));
    let fake = FakeTransport::default();
    let _ = get(&fake, &long_ok, Some(&secret));
    assert_eq!(fake.urls(), vec![long_ok]);
    assert_token_only_on_api(&fake, &secret);
}

#[test]
fn redirect_chain_sends_the_token_only_to_the_api() {
    let secret = token();
    let fake = script(&[
        (
            "https://github.com/a",
            302,
            &[("Location", "//api.github.com/b")],
            b"",
        ),
        (
            "https://api.github.com/b",
            302,
            &[("Location", "//avatars.githubusercontent.com/u/9")],
            b"",
        ),
        (
            "https://avatars.githubusercontent.com/u/9",
            302,
            &[("Location", "https://github.com/x.png")],
            b"",
        ),
        ("https://github.com/x.png", 200, &[], b"png"),
    ]);
    let response = get(&fake, "https://github.com/a", Some(&secret)).expect("followed");
    assert_eq!(response.body, b"png");
    assert_eq!(
        fake.urls(),
        [
            "https://github.com/a",
            "https://api.github.com/b",
            "https://avatars.githubusercontent.com/u/9",
            "https://github.com/x.png",
        ]
    );
    assert_token_only_on_api(&fake, &secret);
}

#[test]
fn relative_redirects_stay_on_the_same_host() {
    let secret = token();
    let api = script(&[
        (
            "https://api.github.com/old",
            302,
            &[("Location", "/new?x=1")],
            b"",
        ),
        ("https://api.github.com/new?x=1", 200, &[], b"{}"),
    ]);
    assert_eq!(
        get(&api, "https://api.github.com/old", Some(&secret))
            .expect("api")
            .body,
        b"{}"
    );
    assert_token_only_on_api(&api, &secret);

    let web = script(&[
        ("https://github.com/old", 302, &[("Location", "/new")], b""),
        ("https://github.com/new", 200, &[], b"ok"),
    ]);
    assert_eq!(
        get(&web, "https://github.com/old", Some(&secret))
            .expect("web")
            .body,
        b"ok"
    );
    assert_token_only_on_api(&web, &secret);

    let mixed = script(&[
        (
            "https://api.github.com/start",
            302,
            &[("Location", "//Avatars.GitHubUserContent.com/u/1")],
            b"",
        ),
        (
            "https://Avatars.GitHubUserContent.com/u/1",
            200,
            &[],
            b"png",
        ),
    ]);
    assert_eq!(
        get(&mixed, "https://api.github.com/start", Some(&secret))
            .expect("case")
            .body,
        b"png"
    );
    assert_token_only_on_api(&mixed, &secret);
}

#[test]
fn http_downgrade_and_foreign_redirects_are_not_requested() {
    let secret = token();
    let cases: &[(&str, &str)] = &[
        (
            "https://api.github.com/start",
            "http://api.github.com/start",
        ),
        (
            "https://api.github.com/start",
            "http://evil.example/collect",
        ),
        ("https://github.com/start", "http://github.com/start"),
        ("https://api.github.com/start", "//evil.example/steal"),
        ("https://api.github.com/start", "//user:pass@evil.example/x"),
        ("https://api.github.com/start", "///evil.example/x"),
        (
            "https://api.github.com/start",
            "https://api.github.com.evil.example/x",
        ),
        ("https://github.com/start", " https://evil.example/x"),
    ];
    for (start, location) in cases {
        let fake = script(&[(start, 302, &[("Location", location)], b"")]);
        let error = get(&fake, start, Some(&secret)).expect_err(location);
        assert!(
            matches!(error, HttpError::NotAllowed(_)),
            "{location} -> {error}"
        );
        assert_eq!(fake.urls(), vec![(*start).to_owned()], "{location}");
        assert_token_only_on_api(&fake, &secret);
    }

    let conflicting = script(&[(
        "https://api.github.com/start",
        302,
        &[
            ("Location", "https://evil.example/first"),
            ("Location", "https://github.com/second"),
        ],
        b"",
    )]);
    assert!(matches!(
        get(&conflicting, "https://api.github.com/start", Some(&secret)),
        Err(HttpError::NotAllowed(_))
    ));
    assert_eq!(
        conflicting.urls(),
        vec!["https://api.github.com/start".to_owned()]
    );
    assert_token_only_on_api(&conflicting, &secret);
}

#[test]
fn check_conflicting_headers_and_garbage_numbers() {
    assert_eq!(
        check(
            &response(
                403,
                &[
                    ("X-RateLimit-Remaining", "0"),
                    ("X-RateLimit-Reset", "1800003600"),
                    ("Retry-After", "5"),
                ],
            ),
            NOW,
        ),
        Err(ApiError::RateLimited {
            until: Some(1_800_003_600),
        })
    );
    assert_eq!(
        check(
            &response(
                429,
                &[("x-ratelimit-remaining", "0"), ("retry-after", "30")]
            ),
            NOW,
        ),
        Err(ApiError::RateLimited {
            until: Some(NOW + 30),
        })
    );
    assert_eq!(
        check(
            &response(
                403,
                &[("x-ratelimit-remaining", "5"), ("retry-after", " 15 ")]
            ),
            NOW,
        ),
        Err(ApiError::RateLimited {
            until: Some(NOW + 15),
        })
    );
    assert_eq!(
        check(
            &response(
                403,
                &[("x-ratelimit-remaining", "nope"), ("retry-after", "soon")]
            ),
            NOW,
        ),
        Err(ApiError::Forbidden)
    );
    assert_eq!(
        check(
            &response(429, &[("x-ratelimit-remaining", ""), ("retry-after", "-3")]),
            NOW,
        ),
        Err(ApiError::RateLimited {
            until: Some(NOW + 60),
        })
    );
    assert_eq!(
        check(
            &response(
                403,
                &[
                    ("x-ratelimit-remaining", "0"),
                    ("x-ratelimit-reset", "later"),
                    ("retry-after", "1.5"),
                ],
            ),
            NOW,
        ),
        Err(ApiError::RateLimited { until: None })
    );
    assert_eq!(
        check(
            &response(
                200,
                &[
                    ("x-ratelimit-limit", " 60 "),
                    ("x-ratelimit-remaining", "-1"),
                    ("x-ratelimit-reset", "1800000100"),
                ],
            ),
            NOW,
        ),
        Ok(RateLimit {
            limit: Some(60),
            remaining: None,
            reset_at: Some(1_800_000_100),
        })
    );
    assert_eq!(
        check(
            &response(200, &[("x-ratelimit-remaining", "4294967295")]),
            NOW,
        )
        .expect("fits")
        .remaining,
        Some(u32::MAX)
    );
    assert_eq!(
        check(
            &response(
                403,
                &[
                    ("x-ratelimit-remaining", "10"),
                    ("x-ratelimit-remaining", "0"),
                ],
            ),
            NOW,
        ),
        Err(ApiError::Forbidden)
    );
    assert_eq!(
        check(&response(200, &[("x-ratelimit-remaining", "+12")]), NOW)
            .expect("leading plus")
            .remaining,
        Some(12)
    );
    for garbage in ["0x10", "1e2", "1_0", "  ", "zero"] {
        assert_eq!(
            check(&response(200, &[("x-ratelimit-remaining", garbage)]), NOW)
                .expect(garbage)
                .remaining,
            None,
            "{garbage}"
        );
    }
}

#[test]
fn rate_limit_counters_above_u32_max_are_not_truncated() {
    let parsed = check(
        &response(
            200,
            &[
                ("x-ratelimit-limit", "4294967296"),
                ("x-ratelimit-remaining", "4294967296"),
            ],
        ),
        NOW,
    )
    .expect("200");
    assert_eq!(parsed.limit, None);
    assert_eq!(parsed.remaining, None);
    assert_eq!(
        check(
            &response(
                403,
                &[
                    ("x-ratelimit-remaining", "4294967296"),
                    ("x-ratelimit-reset", "1800003600"),
                ],
            ),
            NOW,
        ),
        Err(ApiError::Forbidden)
    );
}

#[test]
fn parse_commit_accounts_nulls_missing_fields_and_non_arrays() {
    let body = br#"[
        {"sha":"aaa","commit":{"author":null,"committer":{"email":null}},"author":null,"committer":{"avatar_url":null}},
        {"sha":"bbb","commit":{"author":{"email":"  TeSt@Example.COM\n"},"committer":{"email":"Keep@Example.com"}},"author":{"avatar_url":""},"committer":{"avatar_url":"https://avatars.githubusercontent.com/u/9?v=4"}},
        {"sha":"ccc","commit":{"author":{"name":"No Email"},"committer":{}},"author":{"avatar_url":"https://avatars.githubusercontent.com/u/1"},"committer":{}},
        {"sha":"ddd","extra":[1,2],"commit":{"author":{"email":"plain@example.com","date":"now"},"committer":null},"author":{"login":"p","avatar_url":"https://avatars.githubusercontent.com/u/2"},"files":[{"filename":"a"}]}
    ]"#;
    let accounts = parse_commit_accounts(body).expect("parsed");
    assert_eq!(
        accounts.authors,
        vec![
            (
                "bbb".to_owned(),
                CommitAccount {
                    email: "test@example.com".to_owned(),
                    avatar_url: None,
                }
            ),
            (
                "ddd".to_owned(),
                CommitAccount {
                    email: "plain@example.com".to_owned(),
                    avatar_url: Some("https://avatars.githubusercontent.com/u/2".to_owned()),
                }
            ),
        ]
    );
    assert_eq!(
        accounts.committers,
        vec![CommitAccount {
            email: "keep@example.com".to_owned(),
            avatar_url: Some("https://avatars.githubusercontent.com/u/9?v=4".to_owned()),
        }]
    );

    let empty = parse_commit_accounts(b"[]").expect("empty array");
    assert!(empty.authors.is_empty());
    assert!(empty.committers.is_empty());

    for body in [
        &b"null"[..],
        b"{}",
        b"\"[]\"",
        b"0",
        b"false",
        b"",
        b"[{]",
        b"not json",
        br#"{"message":"Not Found"}"#,
        br#"[{"commit":{"author":{"email":"a@b.co"}}}]"#,
        br#"[{"sha":null,"commit":{"author":{"email":"a@b.co"}}}]"#,
        b"[null]",
        br#"[{"sha":"abc","commit":null}]"#,
    ] {
        assert!(parse_commit_accounts(body).is_none(), "{body:?}");
    }

    let long_local = "a".repeat(20_000);
    let long = format!(
        r#"[{{"sha":"eee","commit":{{"author":{{"email":"{long_local}@Example.com"}}}}}}]"#
    );
    let accounts = parse_commit_accounts(long.as_bytes()).expect("long email");
    assert_eq!(accounts.authors.len(), 1);
    assert_eq!(
        accounts.authors[0].1.email,
        format!("{long_local}@example.com")
    );
}

#[test]
fn parse_commit_accounts_huge_array() {
    const COUNT: usize = 2_048;
    let mut body = String::from("[");
    for index in 0..COUNT {
        if index > 0 {
            body.push(',');
        }
        body.push_str(&commit_json(
            &format!("{index:040x}"),
            &format!("person{index}@example.com"),
            Some("https://avatars.githubusercontent.com/u/5"),
        ));
    }
    body.push(']');
    let accounts = parse_commit_accounts(body.as_bytes()).expect("huge");
    assert_eq!(accounts.authors.len(), COUNT);
    assert_eq!(accounts.authors[0].1.email, "person0@example.com");
    assert_eq!(
        accounts.authors[COUNT - 1].1.email,
        format!("person{}@example.com", COUNT - 1)
    );
    assert!(accounts.committers.is_empty());
}

#[test]
fn same_author_across_many_commits_makes_one_request() {
    let fake = Arc::new(FakeTransport::default());
    let mut body = String::from("[");
    for index in 0..30 {
        if index > 0 {
            body.push(',');
        }
        body.push_str(&commit_json(
            &format!("{index:040x}"),
            "Person@Example.com",
            Some("https://avatars.githubusercontent.com/u/42?v=4"),
        ));
    }
    body.push(']');
    let warm = list_url(100, Some("aaa111"));
    fake.reply(
        &warm,
        200,
        &[("x-ratelimit-remaining", "50")],
        body.as_bytes(),
    );
    let mut resolver = resolver(&fake);
    let context = context();
    let emails = vec!["person@example.com".to_owned()];
    for index in 0..30 {
        let sha = format!("{index:040x}");
        let urls = resolver.candidates(
            &AvatarQuery {
                emails: &emails,
                repo: Some(&context),
                authored: Some(&sha),
            },
            NOW,
        );
        assert_eq!(
            urls[0],
            "https://avatars.githubusercontent.com/u/42?v=4&s=96"
        );
    }
    assert_eq!(fake.urls(), vec![warm]);
}

#[test]
fn coauthor_without_authored_sha_does_not_lookup_per_author() {
    let fake = Arc::new(FakeTransport::default());
    let warm = list_url(100, Some("aaa111"));
    fake.reply(
        &warm,
        200,
        &[("x-ratelimit-remaining", "40")],
        format!("[{}]", commit_json("aaa111", "author@example.com", None)).as_bytes(),
    );
    let mut resolver = resolver(&fake);
    let context = context();
    for email in ["co1@example.com", "co2@example.com", "co3@example.com"] {
        let emails = vec![email.to_owned()];
        let urls = resolver.candidates(
            &AvatarQuery {
                emails: &emails,
                repo: Some(&context),
                authored: None,
            },
            NOW,
        );
        assert!(
            urls.iter().all(|url| url.contains("gravatar.com")),
            "{urls:?}"
        );
    }
    assert_eq!(fake.urls(), vec![warm.clone()]);
    assert!(fake.urls().iter().all(|url| {
        !url.split('&')
            .any(|part| part == "per_page=1" || part.ends_with("?per_page=1"))
    }));

    let one = list_url(1, Some("ccc333"));
    fake.reply(
        &one,
        200,
        &[],
        format!(
            "[{}]",
            commit_json(
                "ccc333",
                "raw@example.com",
                Some("https://avatars.githubusercontent.com/u/7?v=4")
            )
        )
        .as_bytes(),
    );
    let emails = vec!["co1@example.com".to_owned()];
    let urls = resolver.candidates(
        &AvatarQuery {
            emails: &emails,
            repo: Some(&context),
            authored: Some("ccc333"),
        },
        NOW,
    );
    assert_eq!(
        urls[0],
        "https://avatars.githubusercontent.com/u/7?v=4&s=96"
    );
    assert_eq!(fake.urls(), vec![warm, one]);
}

#[test]
fn pause_expires_at_reset_and_allows_a_new_request() {
    let fake = Arc::new(FakeTransport::default());
    let warm = list_url(100, Some("aaa111"));
    let reset = 1_800_003_600u64;
    fake.reply(
        &warm,
        403,
        &[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", &reset.to_string()),
        ],
        b"{}",
    );
    let one = list_url(1, Some("bbb222"));
    fake.reply(
        &one,
        200,
        &[("x-ratelimit-remaining", "20")],
        format!(
            "[{}]",
            commit_json(
                "bbb222",
                "person@example.com",
                Some("https://avatars.githubusercontent.com/u/8?v=4")
            )
        )
        .as_bytes(),
    );
    let mut resolver = resolver(&fake);
    let context = context();
    let emails = vec!["person@example.com".to_owned()];
    let query = AvatarQuery {
        emails: &emails,
        repo: Some(&context),
        authored: Some("bbb222"),
    };
    let urls = resolver.candidates(&query, NOW);
    assert_eq!(urls.len(), 1, "{urls:?}");
    assert!(urls[0].contains("gravatar.com"));
    assert_eq!(resolver.status().paused_until, Some(reset));
    resolver.candidates(&query, reset - 1);
    assert_eq!(fake.urls(), vec![warm.clone()]);
    // After the reset the listing is tried again first, then the one commit.
    fake.reply(&warm, 200, &[("x-ratelimit-remaining", "21")], b"[]");
    let urls = resolver.candidates(&query, reset);
    assert_eq!(
        urls[0],
        "https://avatars.githubusercontent.com/u/8?v=4&s=96"
    );
    assert_eq!(fake.urls(), vec![warm.clone(), warm, one]);
}

#[test]
fn set_token_clears_learned_authors_and_status() {
    let fake = Arc::new(FakeTransport::default());
    let warm = list_url(100, Some("aaa111"));
    fake.reply(
        &warm,
        200,
        &[("x-ratelimit-remaining", "40"), ("x-ratelimit-limit", "60")],
        format!(
            "[{}]",
            commit_json(
                "aaa111",
                "person@example.com",
                Some("https://avatars.githubusercontent.com/u/1?v=4")
            )
        )
        .as_bytes(),
    );
    let mut resolver = resolver(&fake);
    let context = context();
    let emails = vec!["person@example.com".to_owned()];
    let query = AvatarQuery {
        emails: &emails,
        repo: Some(&context),
        authored: Some("aaa111"),
    };
    let urls = resolver.candidates(&query, NOW);
    assert_eq!(
        urls[0],
        "https://avatars.githubusercontent.com/u/1?v=4&s=96"
    );
    assert_eq!(
        resolver.status().rate.and_then(|rate| rate.remaining),
        Some(40)
    );

    let fresh = Token::new("ghp_brand_new_token_value_012345");
    resolver.set_token(Some(fresh.clone()));
    assert_eq!(resolver.status(), &ApiStatus::default());

    fake.reply(
        &warm,
        200,
        &[("x-ratelimit-remaining", "7")],
        format!(
            "[{}]",
            commit_json(
                "aaa111",
                "person@example.com",
                Some("https://avatars.githubusercontent.com/u/2?v=4")
            )
        )
        .as_bytes(),
    );
    let urls = resolver.candidates(&query, NOW);
    assert_eq!(
        urls[0],
        "https://avatars.githubusercontent.com/u/2?v=4&s=96"
    );
    assert_eq!(fake.urls().len(), 2);
    let requests = fake.requests.lock().expect("requests");
    let expected = format!("Bearer {}", fresh.expose());
    assert!(auth_value(&requests[0].1).is_none());
    assert_eq!(auth_value(&requests[1].1), Some(expected.as_str()));
}

#[test]
fn set_token_clears_a_rate_limit_pause() {
    let fake = Arc::new(FakeTransport::default());
    let warm = list_url(100, Some("aaa111"));
    fake.reply(
        &warm,
        403,
        &[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1800003600"),
        ],
        b"{}",
    );
    let mut resolver = resolver(&fake);
    resolver.set_token(Some(token()));
    let context = context();
    let emails = vec!["person@example.com".to_owned()];
    let query = AvatarQuery {
        emails: &emails,
        repo: Some(&context),
        authored: None,
    };
    resolver.candidates(&query, NOW);
    assert_eq!(resolver.status().paused_until, Some(1_800_003_600));
    assert_eq!(fake.urls().len(), 1);
    resolver.set_token(None);
    assert_eq!(resolver.status(), &ApiStatus::default());
    resolver.candidates(&query, NOW);
    assert_eq!(fake.urls().len(), 2, "pause must not survive set_token");
}

#[test]
fn noreply_addresses_never_hit_the_api() {
    let fake = Arc::new(FakeTransport::default());
    let mut resolver = resolver(&fake);
    let context = context();
    let people = [
        (
            "194445529+don-linux@users.noreply.github.com",
            "https://avatars.githubusercontent.com/u/194445529?s=96",
        ),
        (
            "don-linux@users.noreply.github.com",
            "https://github.com/don-linux.png?size=96",
        ),
        (
            "49699333+dependabot[bot]@users.noreply.github.com",
            "https://avatars.githubusercontent.com/u/49699333?s=96",
        ),
    ];
    for (email, direct) in people {
        let emails = vec![email.to_owned()];
        let urls = resolver.candidates(
            &AvatarQuery {
                emails: &emails,
                repo: Some(&context),
                authored: Some("aaa111"),
            },
            NOW,
        );
        assert_eq!(urls[0], direct, "{email}");
        assert!(urls[1].starts_with("https://gravatar.com/avatar/"));
    }
    assert!(fake.urls().is_empty());

    let warm = list_url(100, Some("aaa111"));
    fake.reply(&warm, 200, &[], b"[]");
    let emails = vec!["person@example.com".to_owned()];
    resolver.candidates(
        &AvatarQuery {
            emails: &emails,
            repo: Some(&context),
            authored: None,
        },
        NOW,
    );
    assert_eq!(fake.urls(), vec![warm]);
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
    #![proptest_config(proptest_config())]

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
