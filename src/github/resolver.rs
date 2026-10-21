//! Which avatar URLs to try for a person, best first.
//!
//! Modeled on VS Code's GitHub extension: no-reply addresses need no API
//! call, then one listing of up to 100 commits maps addresses and commits to
//! accounts, then one commit per remaining author is asked about. Gravatar
//! comes last.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::avatar_url::{direct_avatar_url, gravatar_url, sized};
use super::client::{ApiError, CommitAccounts, RateLimit, commit_accounts};
use super::http::Transport;
use super::remote::GitHubRepo;
use super::token::Token;

/// Minimum wait after a rate-limit answer without a reset time.
const DEFAULT_PAUSE_SECS: u64 = 600;
/// A listing that failed for a passing reason (a timeout, a 500) is tried
/// again for the next person, this many times in all.
const WARM_ATTEMPTS: u8 = 3;

/// The repository a person was seen in, so the API can be asked about them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoContext {
    pub repo: GitHubRepo,
    /// Newest commit on screen. The warm-up lists from there.
    pub head: String,
}

#[derive(Debug, Clone, Copy)]
pub struct AvatarQuery<'a> {
    /// Normalized addresses, primary first ([`crate::git::Person::emails`]).
    pub emails: &'a [String],
    pub repo: Option<&'a RepoContext>,
    /// A commit this person authored, for the one-by-one lookup.
    pub authored: Option<&'a str>,
}

/// The URLs to try, and whether the API could still add one later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidates {
    pub urls: Vec<String>,
    /// The API was paused or failed for a passing reason. When none of
    /// `urls` has a picture, try again later instead of remembering a miss.
    pub provisional: bool,
    /// Unix seconds when the API may be asked again, if it is paused.
    pub retry_at: Option<u64>,
}

/// What the resolver learned that the settings page shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApiStatus {
    pub rate: Option<RateLimit>,
    /// GitHub answered 401 with the token. Requests continue without it.
    pub token_rejected: bool,
    /// Unix seconds until which the API is not asked.
    pub paused_until: Option<u64>,
}

#[derive(Default)]
struct RepoState {
    warmed: bool,
    warm_attempts: u8,
    unavailable: bool,
    /// Address to avatar; `None` when GitHub links no account to it.
    known: HashMap<String, Option<String>>,
    /// Commit to its author's avatar, from every listing.
    by_commit: HashMap<String, Option<String>>,
    asked: HashSet<String>,
}

enum Answer {
    Found(String),
    /// GitHub has nothing more to say about this person.
    Settled,
    /// Paused or failed for now; it may answer later.
    Later,
}

pub struct AvatarResolver {
    transport: Arc<dyn Transport>,
    token: Option<Token>,
    status: ApiStatus,
    repos: HashMap<GitHubRepo, RepoState>,
}

impl AvatarResolver {
    pub fn new(transport: Arc<dyn Transport>) -> Self {
        Self {
            transport,
            token: None,
            status: ApiStatus::default(),
            repos: HashMap::new(),
        }
    }

    pub fn transport(&self) -> Arc<dyn Transport> {
        Arc::clone(&self.transport)
    }

    /// Forgets what the API said, like VS Code does when the session changes:
    /// a private repository may now be readable.
    pub fn set_token(&mut self, token: Option<Token>) {
        self.token = token;
        self.status = ApiStatus::default();
        self.repos.clear();
    }

    pub fn status(&self) -> &ApiStatus {
        &self.status
    }

    /// URLs to try in order. May block on the GitHub API; `now` is Unix seconds.
    pub fn candidates(&mut self, query: &AvatarQuery<'_>, now: u64) -> Vec<String> {
        self.resolve(query, now).urls
    }

    /// [`Self::candidates`], with whether the answer may improve later.
    pub fn resolve(&mut self, query: &AvatarQuery<'_>, now: u64) -> Candidates {
        let mut urls = Vec::new();
        let mut push = |url: String| {
            if !urls.contains(&url) {
                urls.push(url);
            }
        };
        let direct: Vec<String> = query
            .emails
            .iter()
            .filter_map(|email| direct_avatar_url(email))
            .collect();
        let had_direct = !direct.is_empty();
        direct.into_iter().for_each(&mut push);
        let mut provisional = false;
        if !had_direct && let Some(context) = query.repo {
            match self.ask_api(context, query.emails, query.authored, now) {
                Answer::Found(url) => push(url),
                Answer::Settled => {}
                Answer::Later => provisional = true,
            }
        }
        for email in query.emails {
            push(gravatar_url(email));
        }
        Candidates {
            urls,
            provisional,
            retry_at: self.status.paused_until.filter(|until| *until > now),
        }
    }

    fn paused(&self, now: u64) -> bool {
        self.status.paused_until.is_some_and(|until| now < until)
    }

    fn ask_api(
        &mut self,
        context: &RepoContext,
        emails: &[String],
        authored: Option<&str>,
        now: u64,
    ) -> Answer {
        let Some(primary) = emails.first() else {
            return Answer::Settled;
        };
        let repo = &context.repo;
        if let Some(found) = self.lookup(repo, emails) {
            return found.map_or(Answer::Settled, Answer::Found);
        }
        if self.state(repo).unavailable {
            return Answer::Settled;
        }
        if self.paused(now) {
            return Answer::Later;
        }

        if !self.state(repo).warmed {
            let mut listed = self.ask(repo, Some(&context.head), 100, now);
            if matches!(listed, Err(ApiError::UnknownCommit)) {
                // The newest commit was never pushed: list the default branch.
                listed = self.ask(repo, None, 100, now);
            }
            let state = self.state(repo);
            state.warm_attempts = state.warm_attempts.saturating_add(1);
            match &listed {
                Ok(_) => state.warmed = true,
                Err(_) if state.unavailable || state.warm_attempts >= WARM_ATTEMPTS => {
                    state.warmed = true;
                }
                Err(_) => return Answer::Later,
            }
            if let Ok(accounts) = listed {
                self.learn(repo, &accounts);
            }
            if let Some(found) = self.lookup(repo, emails) {
                return found.map_or(Answer::Settled, Answer::Found);
            }
        }

        let Some(authored) = authored.filter(|oid| !oid.is_empty()) else {
            return Answer::Settled;
        };
        if let Some(avatar) = self.state(repo).by_commit.get(authored).cloned() {
            self.state(repo)
                .known
                .insert(primary.clone(), avatar.clone());
            return avatar.map_or(Answer::Settled, |url| Answer::Found(sized(&url)));
        }
        if self.state(repo).unavailable || self.state(repo).asked.contains(primary) {
            return Answer::Settled;
        }
        if self.paused(now) {
            return Answer::Later;
        }
        let accounts = match self.ask(repo, Some(authored), 1, now) {
            Ok(accounts) => accounts,
            Err(ApiError::UnknownCommit | ApiError::NotFound | ApiError::Forbidden) => {
                self.state(repo).asked.insert(primary.clone());
                return Answer::Settled;
            }
            Err(_) => return Answer::Later,
        };
        self.state(repo).asked.insert(primary.clone());
        self.learn(repo, &accounts);
        // `.mailmap` may have rewritten the address git shows, so the listed
        // commit's own author is this person whatever address GitHub saw.
        let Some(avatar) = self.state(repo).by_commit.get(authored).cloned() else {
            return Answer::Settled;
        };
        self.state(repo)
            .known
            .insert(primary.clone(), avatar.clone());
        avatar.map_or(Answer::Settled, |url| Answer::Found(sized(&url)))
    }

    fn state(&mut self, repo: &GitHubRepo) -> &mut RepoState {
        self.repos.entry(repo.clone()).or_default()
    }

    /// `Some(Some(url))` when an address has an avatar, `Some(None)` when
    /// GitHub already said none of them has one, `None` when unknown.
    fn lookup(&mut self, repo: &GitHubRepo, emails: &[String]) -> Option<Option<String>> {
        let state = self.state(repo);
        let mut all_unlinked = !emails.is_empty();
        for email in emails {
            match state.known.get(email) {
                Some(Some(url)) => return Some(Some(sized(url))),
                Some(None) => {}
                None => all_unlinked = false,
            }
        }
        all_unlinked.then_some(None)
    }

    fn learn(&mut self, repo: &GitHubRepo, accounts: &CommitAccounts) {
        let state = self.state(repo);
        for (oid, account) in &accounts.authors {
            if !oid.is_empty() {
                state
                    .by_commit
                    .insert(oid.clone(), account.avatar_url.clone());
            }
        }
        let all = accounts
            .authors
            .iter()
            .map(|(_, account)| account)
            .chain(&accounts.committers);
        for account in all {
            let entry = state.known.entry(account.email.clone()).or_insert(None);
            if entry.is_none() {
                entry.clone_from(&account.avatar_url);
            }
        }
    }

    fn ask(
        &mut self,
        repo: &GitHubRepo,
        sha: Option<&str>,
        per_page: u32,
        now: u64,
    ) -> Result<CommitAccounts, ApiError> {
        let mut result = commit_accounts(
            self.transport.as_ref(),
            self.token.as_ref(),
            repo,
            sha,
            per_page,
            now,
        );
        if matches!(result, Err(ApiError::Unauthorized)) && self.token.is_some() {
            self.token = None;
            self.status.token_rejected = true;
            result = commit_accounts(self.transport.as_ref(), None, repo, sha, per_page, now);
        }
        match &result {
            Ok((_, rate)) => {
                self.status.rate = Some(*rate);
                if rate.remaining == Some(0) {
                    self.status.paused_until = Some(
                        rate.reset_at
                            .unwrap_or(now.saturating_add(DEFAULT_PAUSE_SECS)),
                    );
                }
            }
            Err(ApiError::RateLimited { until }) => {
                self.status.paused_until =
                    Some(until.unwrap_or(now.saturating_add(DEFAULT_PAUSE_SECS)));
            }
            Err(ApiError::NotFound | ApiError::Forbidden | ApiError::Unauthorized) => {
                self.state(repo).unavailable = true;
            }
            Err(_) => {}
        }
        result.map(|(accounts, _)| accounts)
    }
}

#[cfg(test)]
mod tests {
    use super::super::http::fake::FakeTransport;
    use super::*;

    const NOW: u64 = 1_800_000_000;
    const LIST: &str = "https://api.github.com/repos/don-linux/Cthulhu-Git/commits";

    fn context() -> RepoContext {
        RepoContext {
            repo: GitHubRepo {
                owner: "don-linux".to_owned(),
                name: "Cthulhu-Git".to_owned(),
            },
            head: "aaa111".to_owned(),
        }
    }

    fn commit_json(sha: &str, email: &str, avatar: Option<&str>) -> String {
        let author = avatar
            .map(|url| format!(r#"{{"avatar_url": "{url}"}}"#))
            .unwrap_or_else(|| "null".to_owned());
        format!(
            r#"{{"sha": "{sha}", "commit": {{"author": {{"email": "{email}"}}, "committer": null}},
                "author": {author}, "committer": null}}"#
        )
    }

    fn resolver(fake: &Arc<FakeTransport>) -> AvatarResolver {
        AvatarResolver::new(Arc::clone(fake) as Arc<dyn Transport>)
    }

    fn emails(list: &[&str]) -> Vec<String> {
        list.iter().map(|email| (*email).to_owned()).collect()
    }

    #[test]
    fn noreply_and_known_agents_skip_the_api() {
        let fake = Arc::new(FakeTransport::default());
        let mut resolver = resolver(&fake);
        let context = context();
        let people = emails(&["cursoragent@cursor.com"]);
        let urls = resolver.candidates(
            &AvatarQuery {
                emails: &people,
                repo: Some(&context),
                authored: Some("aaa111"),
            },
            NOW,
        );
        assert_eq!(
            urls[0],
            "https://avatars.githubusercontent.com/u/199161495?s=96"
        );
        assert!(urls[1].starts_with("https://gravatar.com/avatar/"));
        assert!(fake.urls().is_empty());
    }

    #[test]
    fn one_listing_answers_every_author_on_it() {
        let fake = Arc::new(FakeTransport::default());
        let body = format!(
            "[{}, {}]",
            commit_json(
                "aaa111",
                "fernando@example.com",
                Some("https://avatars.githubusercontent.com/u/194445529?v=4")
            ),
            commit_json("bbb222", "ghost@example.com", None)
        );
        fake.reply(
            &format!("{LIST}?per_page=100&sha=aaa111"),
            200,
            &[("x-ratelimit-remaining", "59"), ("x-ratelimit-limit", "60")],
            body.as_bytes(),
        );
        let mut resolver = resolver(&fake);
        let context = context();
        let fernando = emails(&["fernando@example.com"]);
        let urls = resolver.candidates(
            &AvatarQuery {
                emails: &fernando,
                repo: Some(&context),
                authored: Some("aaa111"),
            },
            NOW,
        );
        assert_eq!(
            urls[0],
            "https://avatars.githubusercontent.com/u/194445529?v=4&s=96"
        );
        // GitHub already said this address has no account: no second request.
        let ghost = emails(&["ghost@example.com"]);
        let urls = resolver.candidates(
            &AvatarQuery {
                emails: &ghost,
                repo: Some(&context),
                authored: Some("bbb222"),
            },
            NOW,
        );
        assert_eq!(urls.len(), 1);
        assert!(urls[0].starts_with("https://gravatar.com/"));
        assert_eq!(fake.urls().len(), 1);
        assert_eq!(
            resolver.status().rate.and_then(|rate| rate.remaining),
            Some(59)
        );
    }

    #[test]
    fn an_unpushed_head_falls_back_to_the_default_branch_then_one_commit() {
        let fake = Arc::new(FakeTransport::default());
        fake.reply(&format!("{LIST}?per_page=100&sha=aaa111"), 422, &[], b"{}");
        fake.reply(&format!("{LIST}?per_page=100"), 200, &[], b"[]");
        fake.reply(
            &format!("{LIST}?per_page=1&sha=ccc333"),
            200,
            &[],
            format!(
                "[{}]",
                commit_json(
                    "ccc333",
                    "raw-address@example.com",
                    Some("https://avatars.githubusercontent.com/u/7?v=4")
                )
            )
            .as_bytes(),
        );
        let mut resolver = resolver(&fake);
        let context = context();
        let mapped = emails(&["mailmapped@example.com"]);
        let query = AvatarQuery {
            emails: &mapped,
            repo: Some(&context),
            authored: Some("ccc333"),
        };
        let urls = resolver.candidates(&query, NOW);
        assert_eq!(
            urls[0],
            "https://avatars.githubusercontent.com/u/7?v=4&s=96"
        );
        // Asked once per address.
        resolver.candidates(&query, NOW);
        assert_eq!(fake.urls().len(), 3);
    }

    #[test]
    fn rate_limits_pause_the_api_until_the_reset() {
        let fake = Arc::new(FakeTransport::default());
        fake.reply(
            &format!("{LIST}?per_page=100&sha=aaa111"),
            403,
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "1800003600"),
            ],
            b"{}",
        );
        let mut resolver = resolver(&fake);
        let context = context();
        let someone = emails(&["someone@example.com"]);
        let query = AvatarQuery {
            emails: &someone,
            repo: Some(&context),
            authored: Some("ddd444"),
        };
        let urls = resolver.candidates(&query, NOW);
        assert_eq!(urls.len(), 1, "only Gravatar while paused: {urls:?}");
        assert_eq!(resolver.status().paused_until, Some(1_800_003_600));
        resolver.candidates(&query, NOW + 10);
        assert_eq!(fake.urls().len(), 1);
    }

    #[test]
    fn a_rejected_token_is_dropped_and_the_request_retried_without_it() {
        let fake = Arc::new(FakeTransport::default());
        fake.reply(&format!("{LIST}?per_page=100&sha=aaa111"), 401, &[], b"{}");
        let mut resolver = resolver(&fake);
        resolver.set_token(Some(Token::new("ghp_revoked_token_0000000000")));
        let context = context();
        let someone = emails(&["someone@example.com"]);
        resolver.candidates(
            &AvatarQuery {
                emails: &someone,
                repo: Some(&context),
                authored: None,
            },
            NOW,
        );
        assert!(resolver.status().token_rejected);
        let requests = fake.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let has_auth = |index: usize| {
            requests[index]
                .1
                .iter()
                .any(|(name, _)| name == "Authorization")
        };
        assert!(has_auth(0));
        assert!(!has_auth(1));
    }

    #[test]
    fn a_listing_that_fails_for_now_is_tried_again_for_the_next_person() {
        let fake = Arc::new(FakeTransport::default());
        let list = format!("{LIST}?per_page=100&sha=aaa111");
        fake.reply(&list, 500, &[], b"{}");
        let mut resolver = resolver(&fake);
        let context = context();
        let first = emails(&["first@example.com"]);
        let answer = resolver.resolve(
            &AvatarQuery {
                emails: &first,
                repo: Some(&context),
                authored: Some("fff666"),
            },
            NOW,
        );
        assert!(answer.provisional, "a 500 is not a final answer");
        assert_eq!(
            fake.urls(),
            std::slice::from_ref(&list),
            "no per-commit call after a failed listing"
        );

        fake.reply(
            &list,
            200,
            &[],
            format!(
                "[{}]",
                commit_json(
                    "aaa111",
                    "second@example.com",
                    Some("https://avatars.githubusercontent.com/u/9?v=4")
                )
            )
            .as_bytes(),
        );
        let second = emails(&["second@example.com"]);
        let answer = resolver.resolve(
            &AvatarQuery {
                emails: &second,
                repo: Some(&context),
                authored: Some("aaa111"),
            },
            NOW,
        );
        assert!(!answer.provisional);
        assert_eq!(
            answer.urls[0],
            "https://avatars.githubusercontent.com/u/9?v=4&s=96"
        );
        assert_eq!(fake.urls(), [list.clone(), list]);
    }

    #[test]
    fn a_listed_commit_answers_its_mailmapped_author_without_another_call() {
        let fake = Arc::new(FakeTransport::default());
        fake.reply(
            &format!("{LIST}?per_page=100&sha=aaa111"),
            200,
            &[],
            format!(
                "[{}]",
                commit_json(
                    "ccc333",
                    "raw-address@example.com",
                    Some("https://avatars.githubusercontent.com/u/7?v=4")
                )
            )
            .as_bytes(),
        );
        let mut resolver = resolver(&fake);
        let context = context();
        let mapped = emails(&["mailmapped@example.com"]);
        let urls = resolver.candidates(
            &AvatarQuery {
                emails: &mapped,
                repo: Some(&context),
                authored: Some("ccc333"),
            },
            NOW,
        );
        assert_eq!(
            urls[0],
            "https://avatars.githubusercontent.com/u/7?v=4&s=96"
        );
        assert_eq!(fake.urls().len(), 1);
    }

    #[test]
    fn a_paused_api_is_provisional_and_says_when_to_retry() {
        let fake = Arc::new(FakeTransport::default());
        fake.reply(
            &format!("{LIST}?per_page=100&sha=aaa111"),
            429,
            &[("retry-after", "120")],
            b"{}",
        );
        let mut resolver = resolver(&fake);
        let context = context();
        let someone = emails(&["someone@example.com"]);
        let answer = resolver.resolve(
            &AvatarQuery {
                emails: &someone,
                repo: Some(&context),
                authored: Some("ddd444"),
            },
            NOW,
        );
        assert!(answer.provisional);
        assert_eq!(answer.retry_at, Some(NOW + 120));
        let settled = resolver.resolve(
            &AvatarQuery {
                emails: &someone,
                repo: None,
                authored: None,
            },
            NOW,
        );
        assert!(
            !settled.provisional,
            "without a repository there is nothing to wait for"
        );
    }

    #[test]
    fn an_empty_commit_id_is_never_looked_up() {
        let fake = Arc::new(FakeTransport::default());
        fake.reply(&format!("{LIST}?per_page=100&sha=aaa111"), 200, &[], b"[]");
        let mut resolver = resolver(&fake);
        let context = context();
        let someone = emails(&["someone@example.com"]);
        let answer = resolver.resolve(
            &AvatarQuery {
                emails: &someone,
                repo: Some(&context),
                authored: Some(""),
            },
            NOW,
        );
        assert!(!answer.provisional);
        assert_eq!(fake.urls().len(), 1);
    }

    #[test]
    fn a_private_repository_without_a_token_is_not_asked_again() {
        let fake = Arc::new(FakeTransport::default());
        fake.reply(&format!("{LIST}?per_page=100&sha=aaa111"), 404, &[], b"{}");
        let mut resolver = resolver(&fake);
        let context = context();
        for email in ["a@example.com", "b@example.com"] {
            let list = emails(&[email]);
            resolver.candidates(
                &AvatarQuery {
                    emails: &list,
                    repo: Some(&context),
                    authored: Some("eee555"),
                },
                NOW,
            );
        }
        assert_eq!(fake.urls().len(), 1);
    }
}
