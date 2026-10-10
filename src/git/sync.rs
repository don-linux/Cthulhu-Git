//! Fetch the current branch's upstream, and fast-forward pull.
//!
//! Fetch names one remote-tracking ref. It does not pass `--all` or `--prune`.
//! Pull is `git pull --ff-only`, so a non-fast-forward leaves the work tree
//! untouched.

use std::path::Path;
use std::time::Duration;

use super::exec::{Git, GitError};

/// Fetch and pull talk to a network. A local read stays on the shorter limit.
const NETWORK_TIMEOUT: Duration = Duration::from_secs(3 * 60);

/// `git pull --ff-only`, with no remote and no branch: git uses the upstream.
const PULL_FF_ONLY_ARGS: &[&str] = &["pull", "--ff-only"];

/// One `git fetch -- <remote> <refspec>` for the current branch's upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchRequest {
    remote: String,
    refspec: String,
}

impl FetchRequest {
    pub fn remote(&self) -> &str {
        &self.remote
    }

    pub fn refspec(&self) -> &str {
        &self.refspec
    }

    /// Argv after `git`. `--` keeps a strange remote from being read as an option.
    pub fn args(&self) -> [&str; 4] {
        ["fetch", "--", &self.remote, &self.refspec]
    }
}

/// Builds the fetch for `%(upstream:short)`, such as `origin/main`.
///
/// The remote is the part before the first `/`. The refspec writes that remote
/// branch into its remote-tracking ref, with `+`, so a rewritten upstream still
/// updates. A plain `git fetch origin main` would only fill `FETCH_HEAD`.
///
/// `None` when the name is empty, has no remote, starts like an option, or
/// contains a character that would change the refspec.
pub fn fetch_request(upstream_short: &str) -> Option<FetchRequest> {
    if upstream_short.is_empty()
        || upstream_short.chars().any(refspec_forbidden)
        || upstream_short.split('/').any(str::is_empty)
    {
        return None;
    }
    let (remote, branch) = upstream_short.split_once('/')?;
    if remote.starts_with('-') {
        return None;
    }
    Some(FetchRequest {
        remote: remote.to_owned(),
        refspec: format!("+refs/heads/{branch}:refs/remotes/{remote}/{branch}"),
    })
}

/// ASCII controls, whitespace, and the characters git treats specially in a refspec.
fn refspec_forbidden(ch: char) -> bool {
    ch.is_ascii_control()
        || ch.is_whitespace()
        || matches!(ch, ':' | '?' | '*' | '[' | '\\' | '^' | '~')
}

/// Fetches the upstream named by `upstream_short` into its remote-tracking ref.
pub fn fetch(git: &Git, root: &Path, upstream_short: &str) -> Result<(), GitError> {
    let request = fetch_request(upstream_short).ok_or_else(|| GitError::Failed {
        command: "git fetch".to_owned(),
        status: "rejected".to_owned(),
        stderr: "the upstream name cannot be fetched".to_owned(),
    })?;
    let args = request.args();
    git.require_ok_for(root, &args, NETWORK_TIMEOUT)?;
    Ok(())
}

/// Fast-forwards the current branch to its upstream. Anything else is an error
/// and git leaves the repository as it was.
pub fn pull_ff_only(git: &Git, root: &Path) -> Result<(), GitError> {
    git.require_ok_for(root, PULL_FF_ONLY_ARGS, NETWORK_TIMEOUT)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_request_updates_the_remote_tracking_ref() {
        let request = fetch_request("origin/main").expect("origin/main");
        assert_eq!(request.remote(), "origin");
        assert_eq!(
            request.refspec(),
            "+refs/heads/main:refs/remotes/origin/main"
        );
        assert_eq!(
            request.args(),
            [
                "fetch",
                "--",
                "origin",
                "+refs/heads/main:refs/remotes/origin/main"
            ]
        );
        assert!(
            !request
                .args()
                .iter()
                .any(|arg| { arg.contains("prune") || arg == &"--all" || arg.contains("--all") })
        );
    }

    #[test]
    fn fetch_request_keeps_slashes_inside_the_branch() {
        let request = fetch_request("origin/feature/foo").expect("nested branch");
        assert_eq!(request.remote(), "origin");
        assert_eq!(
            request.refspec(),
            "+refs/heads/feature/foo:refs/remotes/origin/feature/foo"
        );
    }

    #[test]
    fn fetch_request_rejects_a_remote_that_looks_like_an_option() {
        assert!(fetch_request("-origin/main").is_none());
        assert!(fetch_request("--upload-pack=evil/main").is_none());
    }

    #[test]
    fn fetch_request_rejects_a_missing_slash_and_control_characters() {
        assert!(fetch_request("origin").is_none());
        assert!(fetch_request("").is_none());
        assert!(fetch_request("origin/").is_none());
        assert!(fetch_request("/main").is_none());
        assert!(fetch_request("origin/ma\nin").is_none());
        assert!(fetch_request("ori gin/main").is_none());
        assert!(fetch_request("origin/ma:in").is_none());
    }

    #[test]
    fn pull_is_fast_forward_only() {
        assert_eq!(PULL_FF_ONLY_ARGS, ["pull", "--ff-only"]);
    }
}
