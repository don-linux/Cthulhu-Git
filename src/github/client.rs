//! The two REST endpoints the app reads: commit authors and the signed-in user.

use serde::Deserialize;

use super::http::{self, HttpError, HttpResponse, Transport};
use super::remote::GitHubRepo;
use super::token::Token;
use crate::git::email_key;

/// `x-ratelimit-*` from a response. Unauthenticated callers get 60 per hour.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RateLimit {
    pub limit: Option<u32>,
    pub remaining: Option<u32>,
    /// Unix seconds.
    pub reset_at: Option<u64>,
}

impl RateLimit {
    pub fn from_response(response: &HttpResponse) -> Self {
        let header = |name: &str| response.header(name).map(str::trim);
        Self {
            limit: header("x-ratelimit-limit").and_then(|value| value.parse().ok()),
            remaining: header("x-ratelimit-remaining").and_then(|value| value.parse().ok()),
            reset_at: header("x-ratelimit-reset").and_then(|value| value.parse().ok()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiError {
    /// 401: the token is wrong, expired or revoked.
    #[error("GitHub rejected the token")]
    Unauthorized,
    /// 404 or 409: private without a token, gone, or empty.
    #[error("GitHub did not find the repository")]
    NotFound,
    /// 422: GitHub does not have this commit, for example one never pushed.
    #[error("GitHub does not have that commit")]
    UnknownCommit,
    /// The primary or secondary rate limit. `until` is Unix seconds.
    #[error("GitHub's rate limit was reached")]
    RateLimited { until: Option<u64> },
    #[error("GitHub refused the request")]
    Forbidden,
    #[error("GitHub answered {0}")]
    Status(u16),
    #[error("GitHub sent an unexpected answer")]
    Malformed,
    #[error(transparent)]
    Http(#[from] HttpError),
}

/// Maps a status to an error, reading the rate-limit headers GitHub documents
/// for 403 and 429. `now` is Unix seconds.
pub fn check(response: &HttpResponse, now: u64) -> Result<RateLimit, ApiError> {
    let limit = RateLimit::from_response(response);
    let retry_after = response
        .header("retry-after")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|seconds| now.saturating_add(seconds));
    match response.status {
        200..=299 => Ok(limit),
        401 => Err(ApiError::Unauthorized),
        403 | 429 if limit.remaining == Some(0) => Err(ApiError::RateLimited {
            until: limit.reset_at.or(retry_after),
        }),
        403 | 429 if retry_after.is_some() => Err(ApiError::RateLimited { until: retry_after }),
        // GitHub documents waiting at least a minute when a secondary limit
        // comes without `retry-after`; its message names the rate limit.
        429 => Err(ApiError::RateLimited {
            until: Some(now.saturating_add(60)),
        }),
        403 if mentions_rate_limit(&response.body) => Err(ApiError::RateLimited {
            until: Some(now.saturating_add(60)),
        }),
        403 => Err(ApiError::Forbidden),
        404 | 409 => Err(ApiError::NotFound),
        422 => Err(ApiError::UnknownCommit),
        status => Err(ApiError::Status(status)),
    }
}

fn mentions_rate_limit(body: &[u8]) -> bool {
    String::from_utf8_lossy(body)
        .to_ascii_lowercase()
        .contains("rate limit")
}

/// The GitHub account behind one commit address, as the API reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitAccount {
    /// Normalized with [`email_key`].
    pub email: String,
    /// `None` when GitHub links no account to that address.
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitAccounts {
    /// Oid of each listed commit, newest first, with its author's account.
    pub authors: Vec<(String, CommitAccount)>,
    /// Committers too, so a maintainer who only merges is known.
    pub committers: Vec<CommitAccount>,
}

#[derive(Deserialize)]
struct ListedCommit {
    sha: String,
    commit: CommitIdents,
    author: Option<ApiUser>,
    committer: Option<ApiUser>,
}

#[derive(Deserialize)]
struct CommitIdents {
    author: Option<Ident>,
    committer: Option<Ident>,
}

#[derive(Deserialize)]
struct Ident {
    email: Option<String>,
}

#[derive(Deserialize)]
struct ApiUser {
    avatar_url: Option<String>,
}

pub(crate) fn parse_commit_accounts(body: &[u8]) -> Option<CommitAccounts> {
    let listed: Vec<ListedCommit> = serde_json::from_slice(body).ok()?;
    let account = |ident: Option<&Ident>, user: Option<&ApiUser>| {
        let email = email_key(ident?.email.as_deref()?);
        (!email.is_empty()).then(|| CommitAccount {
            email,
            avatar_url: user
                .and_then(|user| user.avatar_url.clone())
                .filter(|url| !url.is_empty()),
        })
    };
    let mut authors = Vec::new();
    let mut committers = Vec::new();
    for commit in &listed {
        if let Some(found) = account(commit.commit.author.as_ref(), commit.author.as_ref()) {
            authors.push((commit.sha.clone(), found));
        }
        if let Some(found) = account(commit.commit.committer.as_ref(), commit.committer.as_ref()) {
            committers.push(found);
        }
    }
    Some(CommitAccounts {
        authors,
        committers,
    })
}

/// `GET /repos/{owner}/{repo}/commits?per_page=N[&sha=…]`. The list endpoint
/// is used instead of `/commits/{ref}` because that one also returns every
/// changed file.
pub fn commit_accounts(
    transport: &dyn Transport,
    token: Option<&Token>,
    repo: &GitHubRepo,
    sha: Option<&str>,
    per_page: u32,
    now: u64,
) -> Result<(CommitAccounts, RateLimit), ApiError> {
    let mut url = format!("{}?per_page={}", repo.commits_api(), per_page.clamp(1, 100));
    if let Some(sha) = sha {
        if sha.is_empty() || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ApiError::UnknownCommit);
        }
        url.push_str("&sha=");
        url.push_str(sha);
    }
    let response = http::get(transport, &url, token)?;
    let limit = check(&response, now)?;
    let accounts = parse_commit_accounts(&response.body).ok_or(ApiError::Malformed)?;
    Ok((accounts, limit))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubUser {
    pub login: String,
}

#[derive(Deserialize)]
struct UserBody {
    login: String,
}

/// `GET /user`: who the token belongs to, and how much of the limit is left.
pub fn authenticated_user(
    transport: &dyn Transport,
    token: &Token,
    now: u64,
) -> Result<(GitHubUser, RateLimit), ApiError> {
    let response = http::get(transport, "https://api.github.com/user", Some(token))?;
    let limit = check(&response, now)?;
    let body: UserBody = serde_json::from_slice(&response.body).map_err(|_| ApiError::Malformed)?;
    Ok((GitHubUser { login: body.login }, limit))
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    /// 404, 410, a refused URL, or a body over the cap. Try the next URL.
    #[error("no picture at that URL")]
    Missing,
    /// Timeout, connect, or TLS. Try the next URL, and do not remember a miss
    /// when every URL failed this way.
    #[error("could not reach the picture's host")]
    Unavailable,
    #[error("the picture could not be downloaded: {0}")]
    Failed(String),
}

/// Downloads an image. Never sends the token.
pub fn fetch_image(transport: &dyn Transport, url: &str) -> Result<Vec<u8>, FetchError> {
    let response = http::get(transport, url, None).map_err(|error| match error {
        HttpError::NotAllowed | HttpError::TooLarge => FetchError::Missing,
        HttpError::TimedOut | HttpError::Connect | HttpError::Tls(_) => FetchError::Unavailable,
        other => FetchError::Failed(other.to_string()),
    })?;
    match response.status {
        200 if !response.body.is_empty() => Ok(response.body),
        200 | 404 | 410 => Err(FetchError::Missing),
        status => Err(FetchError::Failed(format!("HTTP {status}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{FakeTransport, NOW};

    const fn limit(limit: Option<u32>, remaining: Option<u32>, reset_at: Option<u64>) -> RateLimit {
        RateLimit {
            limit,
            remaining,
            reset_at,
        }
    }

    const fn paused(until: Option<u64>) -> Result<RateLimit, ApiError> {
        Err(ApiError::RateLimited { until })
    }

    /// Status, headers, body, and what `check` makes of the response.
    type Check = (
        u16,
        &'static [(&'static str, &'static str)],
        &'static str,
        Result<RateLimit, ApiError>,
    );

    const CHECKS: &[Check] = &[
        (
            200,
            &[
                ("x-ratelimit-limit", "60"),
                ("x-ratelimit-remaining", "59"),
                ("x-ratelimit-reset", "1800000100"),
            ],
            "",
            Ok(limit(Some(60), Some(59), Some(1_800_000_100))),
        ),
        (401, &[], "", Err(ApiError::Unauthorized)),
        (404, &[], "", Err(ApiError::NotFound)),
        (409, &[], "", Err(ApiError::NotFound)),
        (422, &[], "", Err(ApiError::UnknownCommit)),
        (500, &[], "", Err(ApiError::Status(500))),
        (403, &[], "", Err(ApiError::Forbidden)),
        // The primary limit waits for its reset, even with a Retry-After.
        (
            403,
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "1800003600"),
            ],
            "",
            paused(Some(1_800_003_600)),
        ),
        (
            403,
            &[
                ("X-RateLimit-Remaining", "0"),
                ("X-RateLimit-Reset", "1800003600"),
                ("Retry-After", "5"),
            ],
            "",
            paused(Some(1_800_003_600)),
        ),
        (
            403,
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "later"),
                ("retry-after", "1.5"),
            ],
            "",
            paused(None),
        ),
        // A secondary limit waits Retry-After seconds, or a minute.
        (403, &[("retry-after", "30")], "", paused(Some(NOW + 30))),
        (
            403,
            &[("x-ratelimit-remaining", "5"), ("retry-after", " 15 ")],
            "",
            paused(Some(NOW + 15)),
        ),
        (
            429,
            &[("x-ratelimit-remaining", "0"), ("retry-after", "30")],
            "",
            paused(Some(NOW + 30)),
        ),
        (429, &[], "", paused(Some(NOW + 60))),
        (
            429,
            &[("x-ratelimit-remaining", ""), ("retry-after", "-3")],
            "",
            paused(Some(NOW + 60)),
        ),
        (
            403,
            &[("retry-after", "Wed, 21 Oct 2026 18:00:00 GMT")],
            r#"{"message": "You have exceeded a secondary rate limit. Please wait a few minutes."}"#,
            paused(Some(NOW + 60)),
        ),
        // A 403 without a usable limit is a refusal, not a pause.
        (
            403,
            &[("x-ratelimit-remaining", "nope"), ("retry-after", "soon")],
            "",
            Err(ApiError::Forbidden),
        ),
        (
            403,
            &[
                ("x-ratelimit-remaining", "10"),
                ("x-ratelimit-remaining", "0"),
            ],
            "",
            Err(ApiError::Forbidden),
        ),
        (
            403,
            &[
                ("x-ratelimit-remaining", "4294967296"),
                ("x-ratelimit-reset", "1800003600"),
            ],
            "",
            Err(ApiError::Forbidden),
        ),
        // Counters are trimmed u32s; anything else is unknown.
        (
            200,
            &[
                ("x-ratelimit-limit", " 60 "),
                ("x-ratelimit-remaining", "-1"),
                ("x-ratelimit-reset", "1800000100"),
            ],
            "",
            Ok(limit(Some(60), None, Some(1_800_000_100))),
        ),
        (
            200,
            &[("x-ratelimit-remaining", "4294967295")],
            "",
            Ok(limit(None, Some(u32::MAX), None)),
        ),
        (
            200,
            &[
                ("x-ratelimit-limit", "4294967296"),
                ("x-ratelimit-remaining", "4294967296"),
            ],
            "",
            Ok(limit(None, None, None)),
        ),
        (
            200,
            &[("x-ratelimit-remaining", "+12")],
            "",
            Ok(limit(None, Some(12), None)),
        ),
        (
            200,
            &[("x-ratelimit-remaining", "0x10")],
            "",
            Ok(limit(None, None, None)),
        ),
        (
            200,
            &[("x-ratelimit-remaining", "1e2")],
            "",
            Ok(limit(None, None, None)),
        ),
        (
            200,
            &[("x-ratelimit-remaining", "1_0")],
            "",
            Ok(limit(None, None, None)),
        ),
        (
            200,
            &[("x-ratelimit-remaining", "  ")],
            "",
            Ok(limit(None, None, None)),
        ),
        (
            200,
            &[("x-ratelimit-remaining", "zero")],
            "",
            Ok(limit(None, None, None)),
        ),
    ];

    #[test]
    fn check_table() {
        for (status, headers, body, expected) in CHECKS {
            let response = HttpResponse {
                status: *status,
                headers: headers
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                    .collect(),
                body: body.as_bytes().to_vec(),
            };
            assert_eq!(check(&response, NOW), *expected, "{status} {headers:?}");
        }
    }

    fn account(email: &str, avatar_url: Option<&str>) -> CommitAccount {
        CommitAccount {
            email: email.to_owned(),
            avatar_url: avatar_url.map(str::to_owned),
        }
    }

    #[test]
    fn parses_authors_committers_and_unlinked_addresses() {
        let body = br#"[
          {"sha": "aaa", "commit": {"author": {"name": "Fernando", "email": "Fernando@Example.com"},
                                    "committer": {"email": "noreply@github.com"}},
           "author": {"login": "don-linux", "avatar_url": "https://avatars.githubusercontent.com/u/194445529?v=4"},
           "committer": {"login": "web-flow", "avatar_url": "https://avatars.githubusercontent.com/u/19864447?v=4"}},
          {"sha": "bbb", "commit": {"author": {"email": "  Ghost@Example.COM\n"}, "committer": null},
           "author": {"avatar_url": ""}, "committer": null},
          {"sha": "ccc", "commit": {"author": {"email": ""}, "committer": {"email": null}},
           "author": {}, "committer": {"avatar_url": null}},
          {"sha": "ddd", "commit": {"author": {"name": "No Email"}, "committer": {"email": "Keep@Example.com"}},
           "author": {"avatar_url": "https://avatars.githubusercontent.com/u/1"},
           "committer": {"avatar_url": "https://avatars.githubusercontent.com/u/9?v=4"}},
          {"sha": "eee", "extra": [1, 2], "files": [{"filename": "a"}],
           "commit": {"author": {"email": "plain@example.com", "date": "now"}, "committer": null},
           "author": {"login": "p", "avatar_url": "https://avatars.githubusercontent.com/u/2"}},
          {"sha": "fff", "commit": {"author": null, "committer": null}, "author": null, "committer": null}
        ]"#;
        let accounts = parse_commit_accounts(body).expect("parsable");
        assert_eq!(
            accounts.authors,
            [
                (
                    "aaa".to_owned(),
                    account(
                        "fernando@example.com",
                        Some("https://avatars.githubusercontent.com/u/194445529?v=4")
                    )
                ),
                ("bbb".to_owned(), account("ghost@example.com", None)),
                (
                    "eee".to_owned(),
                    account(
                        "plain@example.com",
                        Some("https://avatars.githubusercontent.com/u/2")
                    )
                ),
            ]
        );
        assert_eq!(
            accounts.committers,
            [
                account(
                    "noreply@github.com",
                    Some("https://avatars.githubusercontent.com/u/19864447?v=4")
                ),
                account(
                    "keep@example.com",
                    Some("https://avatars.githubusercontent.com/u/9?v=4")
                ),
            ]
        );

        let long = "a".repeat(20_000);
        let body =
            format!(r#"[{{"sha":"ggg","commit":{{"author":{{"email":"{long}@Example.com"}}}}}}]"#);
        let accounts = parse_commit_accounts(body.as_bytes()).expect("long email");
        assert_eq!(accounts.authors[0].1.email, format!("{long}@example.com"));

        let empty = parse_commit_accounts(b"[]").expect("empty array");
        assert!(empty.authors.is_empty() && empty.committers.is_empty());
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
    }

    #[test]
    fn commit_accounts_builds_the_list_url_and_refuses_odd_shas() {
        let fake = FakeTransport::default();
        let repo = GitHubRepo {
            owner: "don-linux".to_owned(),
            name: "Cthulhu-Git".to_owned(),
        };
        fake.reply(
            "https://api.github.com/repos/don-linux/Cthulhu-Git/commits?per_page=100&sha=f605456",
            200,
            &[("x-ratelimit-remaining", "58")],
            b"[]",
        );
        let (accounts, limit) =
            commit_accounts(&fake, None, &repo, Some("f605456"), 500, NOW).expect("listed");
        assert!(accounts.authors.is_empty());
        assert_eq!(limit.remaining, Some(58));
        assert_eq!(
            commit_accounts(&fake, None, &repo, Some("main&x=1"), 1, NOW),
            Err(ApiError::UnknownCommit)
        );
        assert_eq!(fake.urls().len(), 1);
    }

    /// Two real requests:
    /// `CTHULHU_LIVE_GITHUB=1 cargo test live_github -- --ignored`.
    /// Without the variable even `--include-ignored` sends nothing.
    #[test]
    #[ignore = "uses the network and one of the 60 hourly API calls"]
    fn live_github_listing_and_avatar_redirect() {
        if std::env::var_os("CTHULHU_LIVE_GITHUB").is_none() {
            return;
        }
        let transport = super::super::http::UreqTransport::default();
        let repo = GitHubRepo {
            owner: "don-linux".to_owned(),
            name: "Cthulhu-Git".to_owned(),
        };
        let (accounts, limit) =
            commit_accounts(&transport, None, &repo, None, 5, NOW).expect("listing");
        assert!(!accounts.authors.is_empty());
        assert!(limit.limit.is_some());
        let image = fetch_image(&transport, "https://github.com/cursoragent.png?size=96")
            .expect("redirected avatar");
        assert!(image.len() > 100);
    }

    #[test]
    fn images_are_missing_on_404_and_never_carry_the_token() {
        let fake = FakeTransport::default();
        fake.reply("https://gravatar.com/avatar/abc?s=96&d=404", 404, &[], b"");
        assert_eq!(
            fetch_image(&fake, "https://gravatar.com/avatar/abc?s=96&d=404"),
            Err(FetchError::Missing)
        );
        assert_eq!(
            fetch_image(&fake, "https://evil.example/a.png"),
            Err(FetchError::Missing)
        );
        assert!(fake.urls().iter().all(|url| !url.contains("evil")));
    }
}
