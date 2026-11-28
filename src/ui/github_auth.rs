//! The optional GitHub token behind the GitHub Auth settings page.
//!
//! Keychain reads and writes, `gh auth token` and the `GET /user` check all
//! run on a worker thread: each can block, and a keychain may show a prompt.

use cthulhu_git::github::{
    self, ApiError, GitHubUser, RateLimit, Token, TokenStoreError, UreqTransport,
};
use eframe::egui::{Context, Id, TextEdit};

use super::clock;
use super::task::Task;

/// The token field, so its undo history can be cleared.
pub fn token_field_id() -> Id {
    Id::unique("settings-github-token")
}

/// Where the token in use lives.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Stored {
    #[default]
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

#[derive(Default)]
pub struct GitHubAuth {
    token: Option<Token>,
    stored: Stored,
    /// The text field. Never logged, and masked in the accessibility tree.
    pub draft: String,
    notice: Option<Notice>,
    user: Option<String>,
    rate: Option<RateLimit>,
    task: Option<Task<Outcome>>,
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
        self.task = Some(Task::spawn(ctx, work));
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
            Outcome::Verified(github::authenticated_user(
                &UreqTransport::default(),
                &token,
                clock::now_unix(),
            ))
        });
    }

    pub fn import_gh(&mut self, ctx: &Context) {
        self.notice = Some(Notice::Info("Asking GitHub CLI…".to_owned()));
        self.run(ctx, || {
            Outcome::Imported(github::gh_cli_token(|name| std::env::var_os(name)))
        });
    }

    /// Fills the field from `GH_TOKEN` or `GITHUB_TOKEN`. Saving stays the
    /// user's choice.
    pub fn use_env(&mut self) {
        if let Some((name, token)) = github::token_from_env(|name| std::env::var_os(name)) {
            token.expose().clone_into(&mut self.draft);
            self.notice = Some(Notice::Info(format!(
                "Filled from {name}. Press Save to keep it."
            )));
        }
    }

    pub fn poll(&mut self, ctx: &Context) -> Vec<Change> {
        let Some(done) = self.task.as_ref().and_then(Task::poll) else {
            return Vec::new();
        };
        self.task = None;
        let Ok(outcome) = done else {
            self.notice = Some(Notice::Error(
                "The request stopped unexpectedly.".to_owned(),
            ));
            return Vec::new();
        };
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
                    // still be in the keychain for the next launch. This one
                    // is not, even after a timeout: the keychain worker puts
                    // the previous value back if the write lands late.
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
                token.expose().clone_into(&mut self.draft);
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

    /// A keychain write that failed leaves `github_token_in_keyring` alone: a
    /// token may still be there for the next launch. A save that timed out is
    /// not kept either: the keychain worker puts the previous value back if
    /// the write lands late.
    #[test]
    fn a_failed_keychain_write_keeps_the_flag() {
        let token = || Token::parse("ghp_abcdefghijklmnopqrstuvwxyz").expect("token");
        let saved = |error| Outcome::Saved(token(), Err(error));
        // (case, outcome, where the token lives after, token handed on, notice)
        let cases = [
            (
                "delete refused",
                Outcome::Removed(Err(TokenStoreError::Locked("locked".to_owned()))),
                Stored::Keychain,
                None,
                "could not delete",
            ),
            (
                "save timed out",
                saved(TokenStoreError::Locked(
                    "the keychain did not answer in time".to_owned(),
                )),
                Stored::Session,
                Some(token()),
                "not saved",
            ),
            (
                "save locked",
                saved(TokenStoreError::Locked(
                    "the collection is locked".to_owned(),
                )),
                Stored::Session,
                Some(token()),
                "not saved",
            ),
            (
                "save refused",
                saved(TokenStoreError::Failed("refused".to_owned())),
                Stored::Session,
                Some(token()),
                "not saved",
            ),
            (
                "no keychain",
                saved(TokenStoreError::Unavailable("no keychain".to_owned())),
                Stored::Session,
                Some(token()),
                "not saved",
            ),
        ];
        let ctx = Context::default();
        for (case, outcome, stored, handed_on, notice) in cases {
            let mut auth = GitHubAuth {
                token: Some(token()),
                stored: Stored::Keychain,
                ..GitHubAuth::default()
            };
            auth.run(&ctx, move || outcome);
            let changes = (0..500)
                .find_map(|_| {
                    let changes = auth.poll(&ctx);
                    let running = auth.busy();
                    if running {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    (!running).then_some(changes)
                })
                .expect("the worker answered");
            assert_eq!(changes, [Change::Token(handed_on)], "{case}");
            assert_eq!(auth.stored(), stored, "{case}");
            let Some(Notice::Error(text)) = auth.notice() else {
                panic!("{case}: expected an error, got {:?}", auth.notice());
            };
            assert!(text.contains(notice), "{case}: {text}");
        }
    }
}
