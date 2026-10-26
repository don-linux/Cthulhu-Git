//! Fetch the current branch's upstream, and fast-forward pull.
//!
//! Fetch names one remote-tracking ref. It does not pass `--all` or `--prune`.
//! Pull is `git pull --ff-only --no-rebase`. `--no-rebase` is required:
//! Git 2.15 through 2.33 rebase anyway when `pull.rebase` or
//! `branch.<name>.rebase` is set, and `--ff-only` alone does not stop them.
//! A non-fast-forward leaves the work tree untouched.

use std::path::Path;
use std::time::Duration;

use super::exec::{Git, GitError, rejected};

/// Fetch and pull talk to a network. A local read stays on the shorter limit.
const NETWORK_TIMEOUT: Duration = Duration::from_secs(3 * 60);

/// `git pull --ff-only --no-rebase`, with no remote and no branch: git uses
/// the upstream. `--no-rebase` skips `pull.rebase` and `branch.<name>.rebase`
/// on every Git this app accepts. An explicit `--ff-only` still wins on 2.34+,
/// so a divergence is an error and not a merge commit.
const PULL_FF_ONLY_ARGS: &[&str] = &["pull", "--ff-only", "--no-rebase"];

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
    let request = fetch_request(upstream_short)
        .ok_or_else(|| rejected("git fetch", "the upstream name cannot be fetched"))?;
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

/// `git switch` arrived in 2.23. Older Git uses `git checkout`.
const SWITCH_SINCE: (u32, u32, u32) = (2, 23, 0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SwitchKind {
    /// `git switch --no-guess -- <name>`.
    Switch,
    /// `git checkout <name> --`, only for Git 2.15 through 2.22.
    Checkout,
}

/// A local branch switch that leaves `HEAD` attached to that branch.
///
/// On Git 2.53, `git checkout refs/heads/<name> --` detaches and skips the
/// worktree lock. `git switch --no-guess -- <name>` stays attached, refuses a
/// branch checked out elsewhere, and does not create a branch from
/// `checkout.defaultRemote`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchRequest {
    name: String,
    kind: SwitchKind,
}

impl SwitchRequest {
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Argv after `git`. The branch is one argument after `--` on Git 2.23+,
    /// so a name such as `-switch` is not an option.
    pub fn args(&self) -> Vec<&str> {
        match self.kind {
            SwitchKind::Switch => vec!["switch", "--no-guess", "--", self.name.as_str()],
            SwitchKind::Checkout => vec!["checkout", self.name.as_str(), "--"],
        }
    }
}

/// Builds the switch for a local branch name.
///
/// `None` when the name cannot be a ref under `refs/heads/`, and, on Git older
/// than 2.23, when the name starts with `-` (checkout would read it as an option).
pub fn switch_request(version: (u32, u32, u32), name: &str) -> Option<SwitchRequest> {
    if !branch_name_is_switchable(name) {
        return None;
    }
    let kind = if version >= SWITCH_SINCE {
        SwitchKind::Switch
    } else if name.starts_with('-') {
        return None;
    } else {
        SwitchKind::Checkout
    };
    Some(SwitchRequest {
        name: name.to_owned(),
        kind,
    })
}

/// Checks out a local branch and leaves `HEAD` attached to it.
///
/// No `--force`. A dirty file that would be overwritten, or a branch checked
/// out in another worktree, is an error and the repository stays as it was.
/// This uses the local command limit (60s), not the fetch/pull network limit.
pub fn switch_branch(git: &Git, root: &Path, name: &str) -> Result<(), GitError> {
    let Some(request) = switch_request(git.version.numbers(), name) else {
        return Err(rejected(
            "git switch",
            "the branch name cannot be checked out",
        ));
    };
    // Git 2.15 has no `--no-guess`. Confirm the local ref first so checkout
    // cannot invent a branch from a remote.
    if request.kind == SwitchKind::Checkout {
        let spec = format!("refs/heads/{}", request.name);
        let verified = git.run(root, &["rev-parse", "--verify", "--quiet", spec.as_str()])?;
        if !verified.status.success() {
            return Err(rejected("git switch", "the branch does not exist"));
        }
    }
    let args = request.args();
    git.require_ok(root, &args)?;
    Ok(())
}

/// `git check-ref-format` rules for `refs/heads/<name>`, except a leading `-`.
///
/// `check-ref-format --branch` rejects that dash. A name such as `-switch` is
/// still a legal ref, and `git switch --` can take it.
fn branch_name_is_switchable(name: &str) -> bool {
    if name.is_empty() || name == "@" {
        return false;
    }
    if name.starts_with('/')
        || name.ends_with('/')
        || name.ends_with('.')
        || name.contains("//")
        || name.contains("..")
        || name.contains("@{")
    {
        return false;
    }
    if name.chars().any(ref_char_forbidden) {
        return false;
    }
    name.split('/').all(|component| {
        !component.is_empty() && !component.starts_with('.') && !component.ends_with(".lock")
    })
}

fn ref_char_forbidden(ch: char) -> bool {
    ch.is_ascii_control()
        || ch.is_whitespace()
        || matches!(ch, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
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
        assert_eq!(PULL_FF_ONLY_ARGS, ["pull", "--ff-only", "--no-rebase"]);
    }

    fn assert_switch_args(version: (u32, u32, u32), name: &str, expected: &[&str]) {
        let request = switch_request(version, name).unwrap_or_else(|| panic!("request for {name}"));
        let args = request.args();
        assert_eq!(args, expected, "{name}");
        let flags = &args[..args.len() - 1];
        for forbidden in [
            "-f", "--force", "--detach", "--orphan", "--guess", "--all", "--prune",
        ] {
            assert!(
                !flags.contains(&forbidden),
                "{name} flags include {forbidden}: {args:?}"
            );
        }
    }

    #[test]
    fn checkout_args_prefix_the_branch_after_end_of_options() {
        let modern = (2, 23, 0);
        for name in [
            "main",
            "feature/necronomicon",
            "-switch",
            "--upload-pack=evil",
            "--force",
        ] {
            assert_switch_args(modern, name, &["switch", "--no-guess", "--", name]);
        }
    }

    #[test]
    fn checkout_args_on_git_2_15_use_checkout_and_refuse_a_leading_dash() {
        let old = (2, 15, 0);
        assert_switch_args(old, "main", &["checkout", "main", "--"]);
        assert_switch_args(old, "feature/foo", &["checkout", "feature/foo", "--"]);
        assert!(switch_request(old, "-switch").is_none());
        assert!(switch_request(old, "--upload-pack=evil").is_none());
    }

    #[test]
    fn checkout_args_reject_dotdot_newline_and_check_ref_format() {
        let versions = [(2, 15, 0), (2, 23, 0), (2, 53, 0)];
        let rejected = [
            "",
            " ",
            "ma\nin",
            "..",
            "../../.git",
            "foo/../../.git",
            "foo.lock",
            ".hidden",
            "a@{1}",
            "star*",
            "foo/",
            "@",
        ];
        for version in versions {
            for name in rejected {
                assert!(
                    switch_request(version, name).is_none(),
                    "{name:?} on {version:?}"
                );
            }
            assert!(switch_request(version, "HEAD").is_some());
        }
    }
}
