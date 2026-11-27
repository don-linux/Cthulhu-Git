//! Fixtures shared by the lib's unit tests.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use proptest::test_runner::Config;

use crate::github::{
    AvatarQuery, AvatarResolver, Candidates, GitHubRepo, HttpError, HttpResponse, RepoContext,
    Transport,
};

/// Hashes of real objects, for records that need a well-formed oid.
pub const OID_A: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
pub const OID_B: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
pub const OID_C: &str = "8b137891791fe96927ad78e64b0aad7bded08eec";

/// The case count comes from `PROPTEST_CASES`: 64 through
/// `.cargo/config.toml`, more from the shell for a deeper run. A failing seed
/// is not written into the tree; the failure becomes a row in a table test.
pub fn proptest_config() -> Config {
    Config {
        failure_persistence: None,
        ..Config::default()
    }
}

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
    fn get_once(&self, url: &str, headers: &[(&str, String)]) -> Result<HttpResponse, HttpError> {
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

/// The `Authorization` header of one recorded request.
pub fn auth_value(headers: &[(String, String)]) -> Option<&str> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("Authorization"))
        .map(|(_, value)| value.as_str())
}

/// Unix time the GitHub tests run at.
pub const NOW: u64 = 1_800_000_000;

/// The commit listing of [`context`]'s repository.
pub fn list_url(per_page: u32, sha: Option<&str>) -> String {
    let mut url =
        format!("https://api.github.com/repos/don-linux/Cthulhu-Git/commits?per_page={per_page}");
    if let Some(sha) = sha {
        url.push_str("&sha=");
        url.push_str(sha);
    }
    url
}

pub fn context() -> RepoContext {
    RepoContext {
        repo: GitHubRepo {
            owner: "don-linux".to_owned(),
            name: "Cthulhu-Git".to_owned(),
        },
        head: "aaa111".to_owned(),
    }
}

/// One commit of a listing: its sha, author address, and the author's
/// avatar when GitHub linked an account.
pub fn commit_json(sha: &str, email: &str, avatar: Option<&str>) -> String {
    let author = match avatar {
        Some(url) => format!(r#"{{"avatar_url":"{url}"}}"#),
        None => "null".to_owned(),
    };
    format!(
        r#"{{"sha":"{sha}","commit":{{"author":{{"email":"{email}"}},"committer":null}},"author":{author},"committer":null}}"#
    )
}

pub fn resolver(fake: &Arc<FakeTransport>) -> AvatarResolver {
    AvatarResolver::new(Arc::clone(fake) as Arc<dyn Transport>)
}

/// Resolves one address in [`context`]'s repository.
pub fn ask(
    resolver: &mut AvatarResolver,
    email: &str,
    authored: Option<&str>,
    now: u64,
) -> Candidates {
    let emails = [email.to_owned()];
    let context = context();
    resolver.resolve(
        &AvatarQuery {
            emails: &emails,
            repo: Some(&context),
            authored,
        },
        now,
    )
}
