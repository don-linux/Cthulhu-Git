//! HTTPS GETs to the few hosts avatars come from.
//!
//! Redirects are followed here, not by ureq, so every hop is checked against
//! [`ALLOWED_HOSTS`]. The GitHub token is attached only on `api.github.com`.

use std::time::Duration;

use ureq::http::Uri;

use super::token::Token;

pub const USER_AGENT: &str = concat!("cthulhu-git/", env!("CARGO_PKG_VERSION"));
/// Avatars are a few kilobytes and a page of 100 commits well under a megabyte.
pub const MAX_BODY: u64 = 2 * 1024 * 1024;
pub const API_HOST: &str = "api.github.com";
const API_VERSION: &str = "2022-11-28";
const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REDIRECTS: usize = 3;
/// `github.com` only serves the `/{login}.png` redirect.
const ALLOWED_HOSTS: [&str; 6] = [
    API_HOST,
    "avatars.githubusercontent.com",
    "github.com",
    "gravatar.com",
    "www.gravatar.com",
    "secure.gravatar.com",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    /// Lowercase names.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    /// Not HTTPS, a host outside [`ALLOWED_HOSTS`], or a URL with credentials or a
    /// port. It does not carry the URL: the message reaches the UI, and a refused
    /// URL may hold credentials or a token in its query.
    NotAllowed,
    TooManyRedirects,
    TimedOut,
    Connect,
    Tls(String),
    TooLarge,
    /// Anything else the transport reported. The message is not matched on.
    Transport(String),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAllowed => {
                f.write_str("refused to request a URL that is not plain HTTPS on a GitHub host")
            }
            Self::TooManyRedirects => f.write_str("too many redirects"),
            Self::TimedOut => f.write_str("the request timed out"),
            Self::Connect => f.write_str("could not connect"),
            Self::Tls(message) => write!(f, "the TLS handshake failed: {message}"),
            Self::TooLarge => f.write_str("the response is larger than the limit"),
            Self::Transport(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for HttpError {}

fn classify_ureq(error: ureq::Error) -> HttpError {
    match error {
        ureq::Error::BodyExceedsLimit(_) => HttpError::TooLarge,
        ureq::Error::Timeout(_) => HttpError::TimedOut,
        ureq::Error::ConnectionFailed | ureq::Error::HostNotFound => HttpError::Connect,
        ureq::Error::Tls(message) => HttpError::Tls(message.to_owned()),
        ureq::Error::TlsRequired => HttpError::Tls("TLS is required".to_owned()),
        ureq::Error::Io(error) => classify_io_kind(&error),
        other => HttpError::Transport(other.to_string()),
    }
}

/// The body reader reports ureq's own errors (body limit, timeouts,
/// decompression) wrapped in an io::Error; ureq's `From` unwraps them so they
/// are classified like errors from the request itself.
fn classify_io(error: std::io::Error) -> HttpError {
    classify_ureq(ureq::Error::from(error))
}

fn classify_io_kind(error: &std::io::Error) -> HttpError {
    match error.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => HttpError::TimedOut,
        std::io::ErrorKind::ConnectionRefused
        | std::io::ErrorKind::ConnectionReset
        | std::io::ErrorKind::ConnectionAborted
        | std::io::ErrorKind::NotConnected
        | std::io::ErrorKind::NetworkUnreachable
        | std::io::ErrorKind::HostUnreachable
        | std::io::ErrorKind::AddrNotAvailable => HttpError::Connect,
        _ => HttpError::Transport(error.to_string()),
    }
}

/// One GET that does not follow redirects. [`UreqTransport`] is the real one.
pub trait Transport: Send + Sync {
    fn get_once(&self, url: &str, headers: &[(&str, String)]) -> Result<HttpResponse, HttpError>;
}

pub struct UreqTransport {
    agent: ureq::Agent,
}

impl Default for UreqTransport {
    fn default() -> Self {
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .timeout_global(Some(TIMEOUT))
            .max_redirects(0)
            .http_status_as_error(false)
            .user_agent(USER_AGENT)
            .build();
        Self {
            agent: config.into(),
        }
    }
}

impl Transport for UreqTransport {
    fn get_once(&self, url: &str, headers: &[(&str, String)]) -> Result<HttpResponse, HttpError> {
        let mut request = self.agent.get(url);
        for (name, value) in headers {
            request = request.header(*name, value.as_str());
        }
        let mut response = request.call().map_err(classify_ureq)?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().to_ascii_lowercase(), value.to_owned()))
            })
            .collect();
        // ureq applies the limit to the decoded body (after gzip). read_capped
        // keeps the same cap for any reader handed to it.
        let mut reader = response.body_mut().with_config().limit(MAX_BODY).reader();
        let body = read_capped(&mut reader, MAX_BODY as usize)?;
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

/// Reads at most `limit` bytes; a longer body is an error, not a truncation.
fn read_capped(reader: &mut impl std::io::Read, limit: usize) -> Result<Vec<u8>, HttpError> {
    let mut body = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => return Ok(body),
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(classify_io(error)),
        };
        if body.len() + read > limit {
            return Err(HttpError::TooLarge);
        }
        body.extend_from_slice(&buffer[..read]);
    }
}

/// GETs `url`, following up to three redirects inside [`ALLOWED_HOSTS`].
/// `token` is sent only to [`API_HOST`], so a redirect cannot carry it away.
pub fn get(
    transport: &dyn Transport,
    url: &str,
    token: Option<&Token>,
) -> Result<HttpResponse, HttpError> {
    let mut current = url.to_owned();
    for _ in 0..=MAX_REDIRECTS {
        let host = allowed_host(&current)?;
        let mut headers = Vec::new();
        if host == API_HOST {
            headers.push(("Accept", "application/vnd.github+json".to_owned()));
            headers.push(("X-GitHub-Api-Version", API_VERSION.to_owned()));
            if let Some(token) = token {
                headers.push(("Authorization", format!("Bearer {}", token.expose())));
            }
        }
        let response = transport.get_once(&current, &headers)?;
        if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            let location = response
                .header("location")
                .ok_or_else(|| HttpError::Transport("redirect without a location".to_owned()))?;
            current = resolve_location(&host, location)?;
            continue;
        }
        return Ok(response);
    }
    Err(HttpError::TooManyRedirects)
}

/// The lowercase host of an HTTPS URL on the default port, without
/// credentials, when it is one of [`ALLOWED_HOSTS`].
pub fn allowed_host(url: &str) -> Result<String, HttpError> {
    let uri: Uri = url.parse().map_err(|_| HttpError::NotAllowed)?;
    if uri.scheme_str() != Some("https") {
        return Err(HttpError::NotAllowed);
    }
    let authority = uri.authority().ok_or(HttpError::NotAllowed)?;
    if authority.as_str().contains('@') || authority.port().is_some() {
        return Err(HttpError::NotAllowed);
    }
    let host = authority.host().to_ascii_lowercase();
    if ALLOWED_HOSTS.contains(&host.as_str()) {
        Ok(host)
    } else {
        Err(HttpError::NotAllowed)
    }
}

fn resolve_location(host: &str, location: &str) -> Result<String, HttpError> {
    if location.starts_with("https://") {
        Ok(location.to_owned())
    } else if let Some(rest) = location.strip_prefix("//") {
        Ok(format!("https://{rest}"))
    } else if location.starts_with('/') {
        Ok(format!("https://{host}{location}"))
    } else {
        Err(HttpError::NotAllowed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{FakeTransport, auth_value};

    fn token() -> Token {
        Token::new("ghp_test_token_value_0123456789")
    }

    /// Every request went to an allowed host, and the token went with
    /// each one to the API and with none anywhere else.
    fn assert_token_only_on_api(fake: &FakeTransport, token: &Token, case: &str) {
        let expected = format!("Bearer {}", token.expose());
        for (url, headers) in fake.requests.lock().expect("requests").iter() {
            let host = allowed_host(url).unwrap_or_else(|error| panic!("{case}: {url}: {error}"));
            let auth = auth_value(headers);
            if host == API_HOST {
                assert_eq!(auth, Some(expected.as_str()), "{case}: {url}");
            } else {
                assert_eq!(auth, None, "{case}: {url}");
            }
        }
    }

    #[test]
    fn body_read_errors_are_classified_like_request_errors() {
        use std::io::{Error, ErrorKind};
        // The body reader hands ureq's own errors back wrapped in an io::Error.
        for (error, expected) in [
            (
                ureq::Error::BodyExceedsLimit(MAX_BODY).into_io(),
                HttpError::TooLarge,
            ),
            (
                ureq::Error::Timeout(ureq::Timeout::Global).into_io(),
                HttpError::TimedOut,
            ),
            (Error::from(ErrorKind::TimedOut), HttpError::TimedOut),
            (Error::from(ErrorKind::ConnectionReset), HttpError::Connect),
        ] {
            let shown = format!("{error:?}");
            assert_eq!(classify_io(error), expected, "{shown}");
        }
    }

    #[test]
    fn bodies_are_capped_after_decompression() {
        let big = vec![0u8; 10_000];
        assert_eq!(
            read_capped(&mut big.as_slice(), 10_000).map(|body| body.len()),
            Ok(10_000)
        );
        assert!(matches!(
            read_capped(&mut big.as_slice(), 9_999),
            Err(HttpError::TooLarge)
        ));
    }

    /// A URL and the host it may reach; `None` is refused and never requested.
    const URLS: &[(&str, Option<&str>)] = &[
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
            "https://Avatars.GitHubUserContent.com/u/1?s=96",
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
        ("https://evil.example/", None),
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
        ("https://gïthub.com/x", None),
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
        ("https://api.github.com:8443/", None),
        ("https:///evil.example/", None),
        ("file:///etc/passwd", None),
        ("not a url", None),
        ("", None),
    ];

    #[test]
    fn only_https_on_the_allowed_hosts_is_requested() {
        assert_eq!(
            ALLOWED_HOSTS,
            [
                "api.github.com",
                "avatars.githubusercontent.com",
                "github.com",
                "gravatar.com",
                "www.gravatar.com",
                "secure.gravatar.com",
            ]
        );
        let huge = format!("https://evil.example/{}", "a".repeat(80_000));
        let long = format!("https://github.com/{}", "b".repeat(20_000));
        let urls = URLS
            .iter()
            .copied()
            .chain([(huge.as_str(), None), (long.as_str(), Some("github.com"))]);
        let secret = token();
        for (url, host) in urls {
            match allowed_host(url) {
                Ok(allowed) => assert_eq!(Some(allowed.as_str()), host, "{url}"),
                Err(error) => assert_eq!((error, host), (HttpError::NotAllowed, None), "{url}"),
            }
            let fake = FakeTransport::default();
            let _ = get(&fake, url, Some(&secret));
            let requested: &[&str] = if host.is_some() { &[url] } else { &[] };
            assert_eq!(fake.urls(), requested, "{url}");
            assert_token_only_on_api(&fake, &secret, url);
        }
    }

    /// One canned reply: URL, status, headers.
    type Reply = (&'static str, u16, &'static [(&'static str, &'static str)]);

    /// The first URL, the replies on the way (the last one answers 200
    /// with `ok`), what `get` returns, and every URL it requested.
    type Redirects = (
        &'static [Reply],
        Result<(), HttpError>,
        &'static [&'static str],
    );

    const REDIRECTS: &[Redirects] = &[
        // Three hops, across hosts: the token goes to the API hop only.
        (
            &[
                (
                    "https://github.com/a",
                    302,
                    &[("Location", "//api.github.com/b")],
                ),
                (
                    "https://api.github.com/b",
                    302,
                    &[("Location", "//avatars.githubusercontent.com/u/9")],
                ),
                (
                    "https://avatars.githubusercontent.com/u/9",
                    302,
                    &[("Location", "https://github.com/x.png")],
                ),
                ("https://github.com/x.png", 200, &[]),
            ],
            Ok(()),
            &[
                "https://github.com/a",
                "https://api.github.com/b",
                "https://avatars.githubusercontent.com/u/9",
                "https://github.com/x.png",
            ],
        ),
        // A fourth redirect is one too many.
        (
            &[
                ("https://github.com/r1", 302, &[("Location", "/r2")]),
                ("https://github.com/r2", 302, &[("Location", "/r3")]),
                ("https://github.com/r3", 302, &[("Location", "/r4")]),
                ("https://github.com/r4", 302, &[("Location", "/r5")]),
                ("https://github.com/r5", 200, &[]),
            ],
            Err(HttpError::TooManyRedirects),
            &[
                "https://github.com/r1",
                "https://github.com/r2",
                "https://github.com/r3",
                "https://github.com/r4",
            ],
        ),
        (
            &[(
                "https://github.com/loop.png",
                302,
                &[("Location", "/loop.png")],
            )],
            Err(HttpError::TooManyRedirects),
            &[
                "https://github.com/loop.png",
                "https://github.com/loop.png",
                "https://github.com/loop.png",
                "https://github.com/loop.png",
            ],
        ),
        // A relative location stays on the host that sent it.
        (
            &[
                (
                    "https://api.github.com/old",
                    302,
                    &[("Location", "/new?x=1")],
                ),
                ("https://api.github.com/new?x=1", 200, &[]),
            ],
            Ok(()),
            &[
                "https://api.github.com/old",
                "https://api.github.com/new?x=1",
            ],
        ),
        (
            &[
                ("https://github.com/old", 302, &[("Location", "/new")]),
                ("https://github.com/new", 200, &[]),
            ],
            Ok(()),
            &["https://github.com/old", "https://github.com/new"],
        ),
        (
            &[
                (
                    "https://api.github.com/start",
                    302,
                    &[("Location", "//Avatars.GitHubUserContent.com/u/1")],
                ),
                ("https://Avatars.GitHubUserContent.com/u/1", 200, &[]),
            ],
            Ok(()),
            &[
                "https://api.github.com/start",
                "https://Avatars.GitHubUserContent.com/u/1",
            ],
        ),
        // A downgrade or a foreign host is refused before it is requested.
        (
            &[(
                "https://api.github.com/start",
                302,
                &[("Location", "http://api.github.com/start")],
            )],
            Err(HttpError::NotAllowed),
            &["https://api.github.com/start"],
        ),
        (
            &[(
                "https://api.github.com/start",
                302,
                &[("Location", "http://evil.example/collect")],
            )],
            Err(HttpError::NotAllowed),
            &["https://api.github.com/start"],
        ),
        (
            &[(
                "https://github.com/start",
                302,
                &[("Location", "http://github.com/start")],
            )],
            Err(HttpError::NotAllowed),
            &["https://github.com/start"],
        ),
        (
            &[(
                "https://github.com/leak.png",
                302,
                &[("Location", "https://evil.example/collect")],
            )],
            Err(HttpError::NotAllowed),
            &["https://github.com/leak.png"],
        ),
        (
            &[(
                "https://api.github.com/start",
                302,
                &[("Location", "//evil.example/steal")],
            )],
            Err(HttpError::NotAllowed),
            &["https://api.github.com/start"],
        ),
        (
            &[(
                "https://api.github.com/start",
                302,
                &[("Location", "//user:pass@evil.example/x")],
            )],
            Err(HttpError::NotAllowed),
            &["https://api.github.com/start"],
        ),
        (
            &[(
                "https://api.github.com/start",
                302,
                &[("Location", "///evil.example/x")],
            )],
            Err(HttpError::NotAllowed),
            &["https://api.github.com/start"],
        ),
        (
            &[(
                "https://api.github.com/start",
                302,
                &[("Location", "https://api.github.com.evil.example/x")],
            )],
            Err(HttpError::NotAllowed),
            &["https://api.github.com/start"],
        ),
        (
            &[(
                "https://github.com/start",
                302,
                &[("Location", " https://evil.example/x")],
            )],
            Err(HttpError::NotAllowed),
            &["https://github.com/start"],
        ),
        (
            &[(
                "https://api.github.com/start",
                302,
                &[(
                    "Location",
                    "https://user:pass@evil example/?access_token=pass",
                )],
            )],
            Err(HttpError::NotAllowed),
            &["https://api.github.com/start"],
        ),
        // The first of two locations is the one followed.
        (
            &[(
                "https://api.github.com/start",
                302,
                &[
                    ("Location", "https://evil.example/first"),
                    ("Location", "https://github.com/second"),
                ],
            )],
            Err(HttpError::NotAllowed),
            &["https://api.github.com/start"],
        ),
    ];

    #[test]
    fn redirects_stay_on_the_allowed_hosts_and_keep_the_token_on_the_api() {
        let secret = token();
        for (replies, result, requested) in REDIRECTS {
            let start = replies[0].0;
            let fake = FakeTransport::default();
            for (url, status, headers) in *replies {
                fake.reply(url, *status, headers, b"ok");
            }
            let got = get(&fake, start, Some(&secret)).map(|response| response.body);
            assert_eq!(got, result.clone().map(|()| b"ok".to_vec()), "{start}");
            assert_eq!(fake.urls(), *requested, "{start}");
            assert_token_only_on_api(&fake, &secret, start);
        }

        let fake = FakeTransport::default();
        fake.reply("https://github.com/start", 302, &[], b"");
        assert_eq!(
            get(&fake, "https://github.com/start", None),
            Err(HttpError::Transport(
                "redirect without a location".to_owned()
            ))
        );
    }
}
