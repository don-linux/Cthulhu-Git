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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HttpError {
    /// Not HTTPS, a host outside [`ALLOWED_HOSTS`], or a URL with credentials or a port.
    #[error("refused to request {0}")]
    NotAllowed(String),
    #[error("too many redirects")]
    TooManyRedirects,
    #[error("the request timed out")]
    TimedOut,
    #[error("could not connect")]
    Connect,
    #[error("the TLS handshake failed: {0}")]
    Tls(String),
    #[error("the response is larger than the limit")]
    TooLarge,
    /// Anything else the transport reported. The message is not matched on.
    #[error("{0}")]
    Transport(String),
}

fn classify_ureq(error: ureq::Error) -> HttpError {
    match error {
        ureq::Error::Timeout(_) => HttpError::TimedOut,
        ureq::Error::ConnectionFailed | ureq::Error::HostNotFound => HttpError::Connect,
        ureq::Error::Tls(message) => HttpError::Tls(message.to_owned()),
        ureq::Error::TlsRequired => HttpError::Tls("TLS is required".to_owned()),
        ureq::Error::Io(error) => classify_io(&error),
        other => HttpError::Transport(other.to_string()),
    }
}

fn classify_io(error: &std::io::Error) -> HttpError {
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
        // ureq's limit counts the bytes on the wire, before gzip is undone,
        // so the decompressed body is capped here.
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
            Err(error) => return Err(classify_io(&error)),
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
    let refuse = || HttpError::NotAllowed(url.to_owned());
    let uri: Uri = url.parse().map_err(|_| refuse())?;
    if uri.scheme_str() != Some("https") {
        return Err(refuse());
    }
    let authority = uri.authority().ok_or_else(refuse)?;
    if authority.as_str().contains('@') || authority.port().is_some() {
        return Err(refuse());
    }
    let host = authority.host().to_ascii_lowercase();
    if ALLOWED_HOSTS.contains(&host.as_str()) {
        Ok(host)
    } else {
        Err(refuse())
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
        Err(HttpError::NotAllowed(location.to_owned()))
    }
}

#[cfg(test)]
pub(crate) mod fake {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;

    /// A URL and the headers sent with it.
    pub type Request = (String, Vec<(String, String)>);

    /// Answers from a table and records every request, headers included.
    #[derive(Default)]
    pub struct FakeTransport {
        pub responses: Mutex<HashMap<String, HttpResponse>>,
        pub requests: Mutex<Vec<Request>>,
    }

    impl FakeTransport {
        pub fn reply(&self, url: &str, status: u16, headers: &[(&str, &str)], body: &[u8]) {
            self.responses.lock().unwrap().insert(
                url.to_owned(),
                HttpResponse {
                    status,
                    headers: headers
                        .iter()
                        .map(|(name, value)| (name.to_ascii_lowercase(), (*value).to_owned()))
                        .collect(),
                    body: body.to_vec(),
                },
            );
        }

        pub fn urls(&self) -> Vec<String> {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .map(|(url, _)| url.clone())
                .collect()
        }
    }

    impl Transport for FakeTransport {
        fn get_once(
            &self,
            url: &str,
            headers: &[(&str, String)],
        ) -> Result<HttpResponse, HttpError> {
            self.requests.lock().unwrap().push((
                url.to_owned(),
                headers
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), value.clone()))
                    .collect(),
            ));
            self.responses
                .lock()
                .unwrap()
                .get(url)
                .cloned()
                .ok_or_else(|| HttpError::Transport(format!("no fake reply for {url}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeTransport;
    use super::*;

    #[test]
    fn only_https_on_known_hosts_and_default_port() {
        assert_eq!(
            allowed_host("https://api.github.com/repos/a/b").as_deref(),
            Ok("api.github.com")
        );
        assert_eq!(
            allowed_host("https://Avatars.GitHubUserContent.com/u/1?s=96").as_deref(),
            Ok("avatars.githubusercontent.com")
        );
        for refused in [
            "http://api.github.com/",
            "https://evil.example/",
            "https://api.github.com.evil.example/",
            "https://user:pass@api.github.com/",
            "https://api.github.com:8443/",
            "file:///etc/passwd",
            "not a url",
        ] {
            assert!(
                matches!(allowed_host(refused), Err(HttpError::NotAllowed(_))),
                "{refused}"
            );
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

    #[test]
    fn the_token_goes_only_to_the_api_host() {
        let fake = FakeTransport::default();
        let api = "https://api.github.com/user";
        let image = "https://avatars.githubusercontent.com/u/1?s=96";
        fake.reply(api, 200, &[], b"{}");
        fake.reply(image, 200, &[], b"png");
        let token = Token::new("ghp_example_token_value");
        get(&fake, api, Some(&token)).expect("api");
        get(&fake, image, Some(&token)).expect("image");
        let requests = fake.requests.lock().unwrap();
        let auth = |index: usize| {
            requests[index]
                .1
                .iter()
                .find(|(name, _)| name == "Authorization")
                .map(|(_, value)| value.clone())
        };
        assert_eq!(auth(0).as_deref(), Some("Bearer ghp_example_token_value"));
        assert_eq!(auth(1), None);
    }

    #[test]
    fn redirects_stay_inside_the_allowed_hosts() {
        let fake = FakeTransport::default();
        fake.reply(
            "https://github.com/don-linux.png?size=96",
            302,
            &[(
                "Location",
                "https://avatars.githubusercontent.com/u/194445529?size=96&v=4",
            )],
            b"",
        );
        fake.reply(
            "https://avatars.githubusercontent.com/u/194445529?size=96&v=4",
            200,
            &[],
            b"png",
        );
        let response =
            get(&fake, "https://github.com/don-linux.png?size=96", None).expect("followed");
        assert_eq!(response.body, b"png");

        fake.reply(
            "https://github.com/leak.png",
            302,
            &[("Location", "https://evil.example/collect")],
            b"",
        );
        assert!(matches!(
            get(&fake, "https://github.com/leak.png", None),
            Err(HttpError::NotAllowed(_))
        ));
        assert!(!fake.urls().iter().any(|url| url.contains("evil")));

        fake.reply(
            "https://github.com/loop.png",
            302,
            &[("Location", "/loop.png")],
            b"",
        );
        assert_eq!(
            get(&fake, "https://github.com/loop.png", None),
            Err(HttpError::TooManyRedirects)
        );
    }
}
