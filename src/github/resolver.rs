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
    use super::*;
    use crate::test_support::{
        FakeTransport, NOW, ask, auth_value, commit_json, context, list_url, resolver,
    };

    fn listing(commits: &[String]) -> Vec<u8> {
        format!("[{}]", commits.join(",")).into_bytes()
    }

    /// A noreply address with an id or a login, and a known agent, go
    /// straight to the avatar host with Gravatar second; the API is not
    /// asked for them, and is for anyone else.
    #[test]
    fn noreply_and_known_agents_skip_the_api() {
        let fake = Arc::new(FakeTransport::default());
        let mut resolver = resolver(&fake);
        for (email, direct) in [
            (
                "194445529+don-linux@users.noreply.github.com",
                "https://avatars.githubusercontent.com/u/194445529?s=96",
            ),
            (
                "49699333+dependabot[bot]@users.noreply.github.com",
                "https://avatars.githubusercontent.com/u/49699333?s=96",
            ),
            (
                "don-linux@users.noreply.github.com",
                "https://github.com/don-linux.png?size=96",
            ),
            (
                "cursoragent@cursor.com",
                "https://avatars.githubusercontent.com/u/199161495?s=96",
            ),
        ] {
            let urls = ask(&mut resolver, email, Some("aaa111"), NOW).urls;
            assert_eq!(urls[0], direct, "{email}");
            assert!(
                urls[1].starts_with("https://gravatar.com/avatar/"),
                "{email}"
            );
        }
        assert!(fake.urls().is_empty());

        let warm = list_url(100, Some("aaa111"));
        fake.reply(&warm, 200, &[], b"[]");
        ask(&mut resolver, "person@example.com", None, NOW);
        assert_eq!(fake.urls(), [warm]);
    }

    #[test]
    fn one_listing_answers_every_author_on_it() {
        let fake = Arc::new(FakeTransport::default());
        fake.reply(
            &list_url(100, Some("aaa111")),
            200,
            &[("x-ratelimit-remaining", "59"), ("x-ratelimit-limit", "60")],
            &listing(&[
                commit_json(
                    "aaa111",
                    "fernando@example.com",
                    Some("https://avatars.githubusercontent.com/u/194445529?v=4"),
                ),
                commit_json("bbb222", "ghost@example.com", None),
                // An address keeps the first avatar listed for it.
                commit_json(
                    "ccc333",
                    "fernando@example.com",
                    Some("https://avatars.githubusercontent.com/u/1?v=4"),
                ),
            ]),
        );
        let mut resolver = resolver(&fake);
        let context = context();
        let fernando = ["fernando@example.com".to_owned()];
        let query = AvatarQuery {
            emails: &fernando,
            repo: Some(&context),
            authored: Some("aaa111"),
        };
        assert_eq!(
            resolver.candidates(&query, NOW)[0],
            "https://avatars.githubusercontent.com/u/194445529?v=4&s=96"
        );
        // GitHub already said this address has no account: no second request.
        let urls = ask(&mut resolver, "ghost@example.com", Some("bbb222"), NOW).urls;
        assert_eq!(urls.len(), 1);
        assert!(urls[0].starts_with("https://gravatar.com/"));
        assert_eq!(fake.urls().len(), 1);
        assert_eq!(
            resolver.status().rate.and_then(|rate| rate.remaining),
            Some(59)
        );
    }

    /// A listed commit answers its author even when `.mailmap` changed the
    /// address the history shows.
    #[test]
    fn a_listed_commit_answers_its_mailmapped_author_without_another_call() {
        let fake = Arc::new(FakeTransport::default());
        fake.reply(
            &list_url(100, Some("aaa111")),
            200,
            &[],
            &listing(&[commit_json(
                "ccc333",
                "raw-address@example.com",
                Some("https://avatars.githubusercontent.com/u/7?v=4"),
            )]),
        );
        let mut resolver = resolver(&fake);
        let urls = ask(&mut resolver, "mailmapped@example.com", Some("ccc333"), NOW).urls;
        assert_eq!(
            urls[0],
            "https://avatars.githubusercontent.com/u/7?v=4&s=96"
        );
        assert_eq!(fake.urls().len(), 1);
    }

    #[test]
    fn an_unpushed_head_falls_back_to_the_default_branch_then_one_commit() {
        let fake = Arc::new(FakeTransport::default());
        fake.reply(&list_url(100, Some("aaa111")), 422, &[], b"{}");
        fake.reply(&list_url(100, None), 200, &[], b"[]");
        fake.reply(
            &list_url(1, Some("ccc333")),
            200,
            &[],
            &listing(&[commit_json(
                "ccc333",
                "raw-address@example.com",
                Some("https://avatars.githubusercontent.com/u/7?v=4"),
            )]),
        );
        let mut resolver = resolver(&fake);
        let urls = ask(&mut resolver, "mailmapped@example.com", Some("ccc333"), NOW).urls;
        assert_eq!(
            urls[0],
            "https://avatars.githubusercontent.com/u/7?v=4&s=96"
        );
        // Asked once per address.
        ask(&mut resolver, "mailmapped@example.com", Some("ccc333"), NOW);
        assert_eq!(fake.urls().len(), 3);
    }

    /// A co-author has no commit of their own to look up: only the listing
    /// is asked, never one request per person.
    #[test]
    fn a_co_author_without_an_authored_commit_is_not_looked_up() {
        let fake = Arc::new(FakeTransport::default());
        let warm = list_url(100, Some("aaa111"));
        fake.reply(
            &warm,
            200,
            &[("x-ratelimit-remaining", "40")],
            &listing(&[commit_json("aaa111", "author@example.com", None)]),
        );
        let mut resolver = resolver(&fake);
        for email in ["co1@example.com", "co2@example.com", "co3@example.com"] {
            let urls = ask(&mut resolver, email, None, NOW).urls;
            assert!(
                urls.iter().all(|url| url.contains("gravatar.com")),
                "{urls:?}"
            );
        }
        assert_eq!(fake.urls(), std::slice::from_ref(&warm));

        let one = list_url(1, Some("ccc333"));
        fake.reply(
            &one,
            200,
            &[],
            &listing(&[commit_json(
                "ccc333",
                "raw@example.com",
                Some("https://avatars.githubusercontent.com/u/7?v=4"),
            )]),
        );
        let urls = ask(&mut resolver, "co1@example.com", Some("ccc333"), NOW).urls;
        assert_eq!(
            urls[0],
            "https://avatars.githubusercontent.com/u/7?v=4&s=96"
        );
        assert_eq!(fake.urls(), [warm, one]);
    }

    /// While paused only Gravatar is offered and nothing is requested; at
    /// the reset the listing is tried again first, then the one commit.
    #[test]
    fn a_rate_limit_pauses_the_api_until_the_reset() {
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
            &listing(&[commit_json(
                "bbb222",
                "person@example.com",
                Some("https://avatars.githubusercontent.com/u/8?v=4"),
            )]),
        );
        let mut resolver = resolver(&fake);
        let urls = ask(&mut resolver, "person@example.com", Some("bbb222"), NOW).urls;
        assert_eq!(urls.len(), 1, "{urls:?}");
        assert!(urls[0].contains("gravatar.com"));
        assert_eq!(resolver.status().paused_until, Some(reset));
        ask(
            &mut resolver,
            "person@example.com",
            Some("bbb222"),
            reset - 1,
        );
        assert_eq!(fake.urls(), std::slice::from_ref(&warm));

        fake.reply(&warm, 200, &[("x-ratelimit-remaining", "21")], b"[]");
        let urls = ask(&mut resolver, "person@example.com", Some("bbb222"), reset).urls;
        assert_eq!(
            urls[0],
            "https://avatars.githubusercontent.com/u/8?v=4&s=96"
        );
        assert_eq!(fake.urls(), [warm.clone(), warm, one]);
    }

    #[test]
    fn a_paused_api_is_provisional_and_says_when_to_retry() {
        let fake = Arc::new(FakeTransport::default());
        fake.reply(
            &list_url(100, Some("aaa111")),
            429,
            &[("retry-after", "120")],
            b"{}",
        );
        let mut resolver = resolver(&fake);
        let answer = ask(&mut resolver, "someone@example.com", Some("ddd444"), NOW);
        assert!(answer.provisional);
        assert_eq!(answer.retry_at, Some(NOW + 120));
        let someone = ["someone@example.com".to_owned()];
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

    /// A new token, or none, forgets the learned accounts, the status and
    /// a pause, and the next listing carries the new token.
    #[test]
    fn set_token_starts_over() {
        let fake = Arc::new(FakeTransport::default());
        let warm = list_url(100, Some("aaa111"));
        let avatar = |id: u32| format!("https://avatars.githubusercontent.com/u/{id}?v=4");
        let learned = |id: u32| {
            listing(&[commit_json(
                "aaa111",
                "person@example.com",
                Some(&avatar(id)),
            )])
        };
        fake.reply(
            &warm,
            200,
            &[("x-ratelimit-remaining", "40"), ("x-ratelimit-limit", "60")],
            &learned(1),
        );
        let mut resolver = resolver(&fake);
        let urls = ask(&mut resolver, "person@example.com", Some("aaa111"), NOW).urls;
        assert_eq!(urls[0], format!("{}&s=96", avatar(1)));
        assert_eq!(
            resolver.status().rate.and_then(|rate| rate.remaining),
            Some(40)
        );

        let fresh = Token::new("ghp_brand_new_token_value_012345");
        resolver.set_token(Some(fresh.clone()));
        assert_eq!(resolver.status(), &ApiStatus::default());
        fake.reply(
            &warm,
            403,
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "1800003600"),
            ],
            b"{}",
        );
        ask(&mut resolver, "person@example.com", Some("aaa111"), NOW);
        assert_eq!(resolver.status().paused_until, Some(1_800_003_600));

        resolver.set_token(None);
        assert_eq!(resolver.status(), &ApiStatus::default());
        fake.reply(&warm, 200, &[("x-ratelimit-remaining", "7")], &learned(2));
        let urls = ask(&mut resolver, "person@example.com", Some("aaa111"), NOW).urls;
        assert_eq!(urls[0], format!("{}&s=96", avatar(2)));

        let requests = fake.requests.lock().expect("requests");
        let auth: Vec<_> = requests
            .iter()
            .map(|(_, headers)| auth_value(headers))
            .collect();
        let bearer = format!("Bearer {}", fresh.expose());
        assert_eq!(auth, [None, Some(bearer.as_str()), None]);
    }

    #[test]
    fn a_rejected_token_is_dropped_and_the_request_retried_without_it() {
        let fake = Arc::new(FakeTransport::default());
        fake.reply(&list_url(100, Some("aaa111")), 401, &[], b"{}");
        let mut resolver = resolver(&fake);
        resolver.set_token(Some(Token::new("ghp_revoked_token_0000000000")));
        ask(&mut resolver, "someone@example.com", None, NOW);
        assert!(resolver.status().token_rejected);
        let requests = fake.requests.lock().expect("requests");
        let auth: Vec<_> = requests
            .iter()
            .map(|(_, headers)| auth_value(headers).is_some())
            .collect();
        assert_eq!(auth, [true, false]);
    }

    #[test]
    fn a_listing_that_fails_for_now_is_tried_again_for_the_next_person() {
        let fake = Arc::new(FakeTransport::default());
        let list = list_url(100, Some("aaa111"));
        fake.reply(&list, 500, &[], b"{}");
        let mut resolver = resolver(&fake);
        let answer = ask(&mut resolver, "first@example.com", Some("fff666"), NOW);
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
            &listing(&[commit_json(
                "aaa111",
                "second@example.com",
                Some("https://avatars.githubusercontent.com/u/9?v=4"),
            )]),
        );
        let answer = ask(&mut resolver, "second@example.com", Some("aaa111"), NOW);
        assert!(!answer.provisional);
        assert_eq!(
            answer.urls[0],
            "https://avatars.githubusercontent.com/u/9?v=4&s=96"
        );
        assert_eq!(fake.urls(), [list.clone(), list]);
    }

    /// One listing, and nothing more, for an empty commit id and for a
    /// private repository without a token.
    #[test]
    fn an_empty_commit_id_and_a_private_repository_are_asked_once() {
        for (status, authored) in [(200, ""), (404, "eee555")] {
            let fake = Arc::new(FakeTransport::default());
            fake.reply(&list_url(100, Some("aaa111")), status, &[], b"[]");
            let mut resolver = resolver(&fake);
            for email in ["a@example.com", "b@example.com"] {
                let answer = ask(&mut resolver, email, Some(authored), NOW);
                assert!(!answer.provisional, "{status} {email}");
            }
            assert_eq!(fake.urls().len(), 1, "{status}");
        }
    }
}
