//! Avatars and read-only GitHub lookups over HTTPS.
//!
//! No GUI code: requests go through a [`Transport`], which tests replace with
//! a fake, so nothing here needs the network to be tested. Requests only go
//! to GitHub and Gravatar hosts, and the optional token only to
//! `api.github.com`.

#[cfg(test)]
mod adversarial_tests;
mod avatar_url;
mod client;
mod http;
mod remote;
mod resolver;
mod token;

pub use avatar_url::{AVATAR_PX, direct_avatar_url, gravatar_url, sha256_hex};
pub use client::{
    ApiError, CommitAccount, CommitAccounts, FetchError, GitHubUser, RateLimit, authenticated_user,
    commit_accounts, fetch_image,
};
pub use http::{HttpError, HttpResponse, MAX_BODY, Transport, USER_AGENT, UreqTransport};
pub use remote::{GitHubRepo, github_repo, parse_github_url};
pub use resolver::{ApiStatus, AvatarQuery, AvatarResolver, Candidates, RepoContext};
pub use token::{
    TOKEN_TEMPLATE_URL, Token, TokenStoreError, delete_token, gh_cli_token, load_token, save_token,
    token_from_env,
};
