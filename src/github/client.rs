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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// 401: the token is wrong, expired or revoked.
    Unauthorized,
    /// 404 or 409: private without a token, gone, or empty.
    NotFound,
    /// 422: GitHub does not have this commit, for example one never pushed.
    UnknownCommit,
    /// The primary or secondary rate limit. `until` is Unix seconds.
    RateLimited {
        until: Option<u64>,
    },
    Forbidden,
    Status(u16),
    Malformed,
    Http(HttpError),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => f.write_str("GitHub rejected the token"),
            Self::NotFound => f.write_str("GitHub did not find the repository"),
            Self::UnknownCommit => f.write_str("GitHub does not have that commit"),
            Self::RateLimited { .. } => f.write_str("GitHub's rate limit was reached"),
            Self::Forbidden => f.write_str("GitHub refused the request"),
            Self::Status(status) => write!(f, "GitHub answered {status}"),
            Self::Malformed => f.write_str("GitHub sent an unexpected answer"),
            Self::Http(error) => write!(f, "{error}"),
        }
    }
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
    let response = http::get(transport, &url, token).map_err(ApiError::Http)?;
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
    let response =
        http::get(transport, "https://api.github.com/user", Some(token)).map_err(ApiError::Http)?;
    let limit = check(&response, now)?;
    let body: UserBody = serde_json::from_slice(&response.body).map_err(|_| ApiError::Malformed)?;
    Ok((GitHubUser { login: body.login }, limit))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// 404, 410, a refused URL, or a body over the cap. Try the next URL.
    Missing,
    /// Timeout, connect, or TLS. Try the next URL, and do not remember a miss
    /// when every URL failed this way.
    Unavailable,
    Failed(String),
}

/// Downloads an image. Never sends the token.
pub fn fetch_image(transport: &dyn Transport, url: &str) -> Result<Vec<u8>, FetchError> {
    let response = http::get(transport, url, None).map_err(|error| match error {
        HttpError::NotAllowed(_) | HttpError::TooLarge => FetchError::Missing,
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
    use super::super::http::fake::FakeTransport;
    use super::*;

    const NOW: u64 = 1_800_000_000;

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

    #[test]
    fn statuses_map_to_the_documented_meanings() {
        assert_eq!(
            check(
                &response(
                    200,
                    &[
                        ("x-ratelimit-limit", "60"),
                        ("x-ratelimit-remaining", "59"),
                        ("x-ratelimit-reset", "1800000100"),
                    ]
                ),
                NOW
            ),
            Ok(RateLimit {
                limit: Some(60),
                remaining: Some(59),
                reset_at: Some(1_800_000_100),
            })
        );
        assert_eq!(check(&response(401, &[]), NOW), Err(ApiError::Unauthorized));
        assert_eq!(
            check(
                &response(
                    403,
                    &[
                        ("x-ratelimit-remaining", "0"),
                        ("x-ratelimit-reset", "1800003600")
                    ]
                ),
                NOW
            ),
            Err(ApiError::RateLimited {
                until: Some(1_800_003_600)
            })
        );
        assert_eq!(
            check(&response(403, &[("retry-after", "30")]), NOW),
            Err(ApiError::RateLimited {
                until: Some(NOW + 30)
            })
        );
        assert_eq!(
            check(&response(429, &[]), NOW),
            Err(ApiError::RateLimited {
                until: Some(NOW + 60)
            })
        );
        assert_eq!(check(&response(403, &[]), NOW), Err(ApiError::Forbidden));
        let mut secondary = response(403, &[("retry-after", "Wed, 21 Oct 2026 18:00:00 GMT")]);
        secondary.body =
            br#"{"message": "You have exceeded a secondary rate limit. Please wait a few minutes."}"#
                .to_vec();
        assert_eq!(
            check(&secondary, NOW),
            Err(ApiError::RateLimited {
                until: Some(NOW + 60)
            })
        );
        assert_eq!(check(&response(404, &[]), NOW), Err(ApiError::NotFound));
        assert_eq!(check(&response(409, &[]), NOW), Err(ApiError::NotFound));
        assert_eq!(
            check(&response(422, &[]), NOW),
            Err(ApiError::UnknownCommit)
        );
        assert_eq!(check(&response(500, &[]), NOW), Err(ApiError::Status(500)));
    }

    #[test]
    fn parses_authors_committers_and_unlinked_addresses() {
        let body = br#"[
          {"sha": "aaa", "commit": {"author": {"name": "Fernando", "email": "Fernando@Example.com"},
                                    "committer": {"email": "noreply@github.com"}},
           "author": {"login": "don-linux", "avatar_url": "https://avatars.githubusercontent.com/u/194445529?v=4"},
           "committer": {"login": "web-flow", "avatar_url": "https://avatars.githubusercontent.com/u/19864447?v=4"}},
          {"sha": "bbb", "commit": {"author": {"email": "ghost@example.com"}, "committer": null},
           "author": null, "committer": null},
          {"sha": "ccc", "commit": {"author": {"email": ""}, "committer": {}}, "author": {}, "committer": {}}
        ]"#;
        let accounts = parse_commit_accounts(body).expect("parsable");
        assert_eq!(
            accounts.authors,
            [
                (
                    "aaa".to_owned(),
                    CommitAccount {
                        email: "fernando@example.com".to_owned(),
                        avatar_url: Some(
                            "https://avatars.githubusercontent.com/u/194445529?v=4".to_owned()
                        ),
                    }
                ),
                (
                    "bbb".to_owned(),
                    CommitAccount {
                        email: "ghost@example.com".to_owned(),
                        avatar_url: None,
                    }
                ),
            ]
        );
        assert_eq!(accounts.committers.len(), 1);
        assert_eq!(accounts.committers[0].email, "noreply@github.com");
        assert!(parse_commit_accounts(b"{\"message\": \"Not Found\"}").is_none());
        assert!(parse_commit_accounts(b"not json").is_none());
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
