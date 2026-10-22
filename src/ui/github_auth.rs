//! The optional GitHub token behind the GitHub Auth settings page.
//!
//! Keychain reads and writes, `gh auth token` and the `GET /user` check all
//! run on a worker thread: each can block, and a keychain may show a prompt.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use cthulhu_git::github::{
    self, ApiError, GitHubUser, RateLimit, Token, TokenStoreError, UreqTransport,
};
use eframe::egui::{Context, Id, TextEdit};

/// The token field, so its undo history can be cleared.
pub fn token_field_id() -> Id {
    Id::new("settings-github-token")
}

/// Where the token in use lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stored {
    None,
    Keychain,
    /// No keychain could take it: it is forgotten when the app closes.
    Session,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    Info(String),
    Error(String),
}

enum Outcome {
    Loaded(Result<Option<Token>, TokenStoreError>),
    Saved(Token, Result<(), TokenStoreError>),
    Removed(Result<(), TokenStoreError>),
    Verified(Result<(GitHubUser, RateLimit), ApiError>),
    Imported(Result<Token, String>),
}

/// What the app must do after [`GitHubAuth::poll`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Hand this token (or none) to the avatar resolver.
    Token(Option<Token>),
    /// Store `github_token_in_keyring` in the settings.
    InKeychain(bool),
}

pub struct GitHubAuth {
    token: Option<Token>,
    stored: Stored,
    /// The text field. Never logged, and masked in the accessibility tree.
    pub draft: String,
    notice: Option<Notice>,
    user: Option<String>,
    rate: Option<RateLimit>,
    task: Option<Receiver<Outcome>>,
}

impl Default for GitHubAuth {
    fn default() -> Self {
        Self {
            token: None,
            stored: Stored::None,
            draft: String::new(),
            notice: None,
            user: None,
            rate: None,
            task: None,
        }
    }
}

impl GitHubAuth {
    pub fn stored(&self) -> Stored {
        self.stored
    }

    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    pub fn busy(&self) -> bool {
        self.task.is_some()
    }

    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    pub fn user(&self) -> Option<&str> {
        self.user.as_deref()
    }

    pub fn rate(&self) -> Option<RateLimit> {
        self.rate
    }

    fn run(&mut self, ctx: &Context, work: impl FnOnce() -> Outcome + Send + 'static) {
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let _ = sender.send(work());
            ctx.request_repaint();
        });
        self.task = Some(receiver);
    }

    /// Reads the token saved by an earlier session.
    pub fn load(&mut self, ctx: &Context) {
        self.run(ctx, || Outcome::Loaded(github::load_token()));
    }

    /// Saves the pasted token. Something that cannot be a token is refused
    /// before anything leaves the field.
    pub fn save_draft(&mut self, ctx: &Context) {
        let Some(token) = Token::parse(&self.draft) else {
            self.notice = Some(Notice::Error(
                "That does not look like a GitHub token. Paste the whole value, without spaces."
                    .to_owned(),
            ));
            return;
        };
        self.forget_draft(ctx);
        self.run(ctx, move || {
            let result = github::save_token(&token);
            Outcome::Saved(token, result)
        });
    }

    pub fn remove(&mut self, ctx: &Context) {
        self.forget_draft(ctx);
        self.run(ctx, || Outcome::Removed(github::delete_token()));
    }

    /// Drops the typed text, its allocation, and the field's undo history,
    /// where egui keeps earlier versions of the text.
    fn forget_draft(&mut self, ctx: &Context) {
        self.draft = String::new();
        if let Some(mut state) = TextEdit::load_state(ctx, token_field_id()) {
            state.clear_undoer();
            state.store(ctx, token_field_id());
        }
    }

    pub fn verify(&mut self, ctx: &Context) {
        let Some(token) = self.token.clone() else {
            return;
        };
        self.notice = Some(Notice::Info("Checking the token with GitHub…".to_owned()));
        self.run(ctx, move || {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs())
                .unwrap_or(0);
            Outcome::Verified(github::authenticated_user(
                &UreqTransport::default(),
                &token,
                now,
            ))
        });
    }

    pub fn import_gh(&mut self, ctx: &Context) {
        self.notice = Some(Notice::Info("Asking GitHub CLI…".to_owned()));
        self.run(ctx, || Outcome::Imported(github::gh_cli_token()));
    }

    /// Fills the field from `GH_TOKEN` or `GITHUB_TOKEN`. Saving stays the
    /// user's choice.
    pub fn use_env(&mut self) {
        if let Some((name, token)) = github::token_from_env() {
            self.draft = token.expose().to_owned();
            self.notice = Some(Notice::Info(format!(
                "Filled from {name}. Press Save to keep it."
            )));
        }
    }

    pub fn poll(&mut self, ctx: &Context) -> Vec<Change> {
        let Some(receiver) = &self.task else {
            return Vec::new();
        };
        let outcome = match receiver.try_recv() {
            Ok(outcome) => outcome,
            Err(TryRecvError::Empty) => return Vec::new(),
            Err(TryRecvError::Disconnected) => {
                self.task = None;
                self.notice = Some(Notice::Error(
                    "The request stopped unexpectedly.".to_owned(),
                ));
                return Vec::new();
            }
        };
        self.task = None;
        let mut changes = Vec::new();
        match outcome {
            Outcome::Loaded(Ok(Some(token))) => {
                self.token = Some(token.clone());
                self.stored = Stored::Keychain;
                changes.push(Change::Token(Some(token)));
            }
            Outcome::Loaded(Ok(None)) => {
                self.notice = Some(Notice::Info(
                    "The saved token is no longer in the keychain.".to_owned(),
                ));
                changes.push(Change::InKeychain(false));
            }
            Outcome::Loaded(Err(error)) => {
                self.notice = Some(Notice::Error(format!(
                    "Could not read the saved token. {error}"
                )));
            }
            Outcome::Saved(token, result) => {
                self.token = Some(token.clone());
                self.user = None;
                self.rate = None;
                changes.push(Change::Token(Some(token)));
                match result {
                    Ok(()) => {
                        self.stored = Stored::Keychain;
                        changes.push(Change::InKeychain(true));
                        self.verify(ctx);
                    }
                    // The flag is left alone: a token saved earlier may
                    // still be in the keychain for the next launch.
                    Err(error) => {
                        self.stored = Stored::Session;
                        self.notice = Some(Notice::Error(format!(
                            "{error} The token is used until Cthulhu Git closes, and is not saved."
                        )));
                    }
                }
            }
            Outcome::Removed(result) => {
                self.token = None;
                self.user = None;
                self.rate = None;
                changes.push(Change::Token(None));
                match result {
                    Ok(()) => {
                        self.stored = Stored::None;
                        changes.push(Change::InKeychain(false));
                        self.notice = Some(Notice::Info("The token was removed.".to_owned()));
                    }
                    // The secret is still in the keychain. Clearing the flag
                    // would make the next launch skip it.
                    Err(error) => {
                        self.stored = Stored::Keychain;
                        self.notice = Some(Notice::Error(format!(
                            "The token is no longer used, but the keychain could not delete it. {error}"
                        )));
                    }
                }
            }
            Outcome::Verified(Ok((user, rate))) => {
                self.notice = None;
                self.user = Some(user.login);
                self.rate = Some(rate);
            }
            Outcome::Verified(Err(error)) => {
                self.user = None;
                self.notice = Some(Notice::Error(match error {
                    ApiError::Unauthorized => {
                        "GitHub rejected this token. It may be expired or revoked.".to_owned()
                    }
                    other => format!("Could not check the token: {other}."),
                }));
            }
            Outcome::Imported(Ok(token)) => {
                self.draft = token.expose().to_owned();
                self.notice = Some(Notice::Info(
                    "Filled from GitHub CLI. Press Save to keep it.".to_owned(),
                ));
            }
            Outcome::Imported(Err(message)) => {
                self.notice = Some(Notice::Error(message));
            }
        }
        changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Context;

    #[test]
    fn a_failed_delete_does_not_clear_the_keychain_flag() {
        let ctx = Context::default();
        let (sender, receiver) = mpsc::channel();
        sender
            .send(Outcome::Removed(Err(TokenStoreError::Locked(
                "locked".to_owned(),
            ))))
            .expect("send");
        let mut auth = GitHubAuth {
            token: Some(Token::new("ghp_abcdefghijklmnopqrstuvwxyz")),
            stored: Stored::Keychain,
            task: Some(receiver),
            ..GitHubAuth::default()
        };
        let changes = auth.poll(&ctx);
        assert!(
            changes
                .iter()
                .any(|change| matches!(change, Change::Token(None)))
        );
        assert!(
            !changes
                .iter()
                .any(|change| matches!(change, Change::InKeychain(false))),
            "a failed delete must leave github_token_in_keyring set, got {changes:?}"
        );
        assert!(matches!(auth.notice, Some(Notice::Error(_))));
    }
}
