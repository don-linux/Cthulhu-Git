//! Read-only access to repositories through the system `git` executable.
//!
//! No libgit2 or gitoxide: the app uses whatever git the user already has,
//! so behavior (config, credentials, hooks) matches their terminal.

mod discover;
mod exec;
mod log;
mod repo;

#[cfg(test)]
mod proptest_tests;

pub use discover::{DiscoverInputs, OVERRIDE_VAR, discover_with};
pub use exec::{Git, GitError, GitOutput, GitVersion, MIN_GIT_VERSION};
pub use log::{Commit, History, SHORT_OID_LEN, history};
pub use repo::{Head, RepoInfo, inspect};
