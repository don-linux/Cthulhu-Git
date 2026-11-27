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

/// Builds the fetch from the branch's upstream configuration: the remote
/// (`branch.<name>.remote`), the ref on that remote (`branch.<name>.merge`), and
/// the remote-tracking ref git maps it to (`%(upstream)`).
///
/// `%(upstream:short)` cannot be split into remote and branch: a remote may be
/// called `team/origin`. The refspec writes the remote ref into its tracking
/// ref with `+`, so a rewritten upstream still updates. A plain
/// `git fetch origin main` would only fill `FETCH_HEAD`.
///
/// `None` for a local upstream (`.`), a remote that starts like an option, a
/// merge ref outside `refs/`, a tracking ref outside `refs/remotes/`, or a
/// character that would change the refspec.
pub fn fetch_request(remote: &str, merge: &str, tracking: &str) -> Option<FetchRequest> {
    let plain = |text: &str| !text.is_empty() && !text.chars().any(refspec_forbidden);
    let usable = remote != "."
        && !remote.starts_with('-')
        && plain(remote)
        && merge.starts_with("refs/")
        && plain(merge)
        && tracking.starts_with("refs/remotes/")
        && plain(tracking);
    usable.then(|| FetchRequest {
        remote: remote.to_owned(),
        refspec: format!("+{merge}:{tracking}"),
    })
}

/// ASCII controls, whitespace, and the characters git treats specially in a refspec.
fn refspec_forbidden(ch: char) -> bool {
    ch.is_ascii_control()
        || ch.is_whitespace()
        || matches!(ch, ':' | '?' | '*' | '[' | '\\' | '^' | '~')
}

/// Fetches the upstream of local `branch` into its remote-tracking ref.
pub fn fetch(git: &Git, root: &Path, branch: &str) -> Result<(), GitError> {
    let remote = config_value(git, root, &format!("branch.{branch}.remote"))?;
    let merge = config_value(git, root, &format!("branch.{branch}.merge"))?;
    let tracking = git.require_ok(
        root,
        &[
            "for-each-ref",
            "--format=%(upstream)",
            &format!("refs/heads/{branch}"),
        ],
    )?;
    let tracking = String::from_utf8_lossy(&tracking.stdout);
    let request = remote
        .zip(merge)
        .and_then(|(remote, merge)| fetch_request(&remote, &merge, tracking.trim_end()))
        .ok_or_else(|| {
            rejected(
                "git fetch",
                "the branch has no upstream that can be fetched",
            )
        })?;
    let args = request.args();
    git.require_ok_for(root, &args, NETWORK_TIMEOUT)?;
    Ok(())
}

/// `git config --get <key>`, or `None` when the key is not set.
fn config_value(git: &Git, root: &Path, key: &str) -> Result<Option<String>, GitError> {
    let output = git.run(root, &["config", "--get", key])?;
    if !output.status.success() {
        return Ok(None);
    }
    let value = String::from_utf8_lossy(&output.stdout);
    Ok(Some(value.trim_end_matches(['\n', '\r']).to_owned()))
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

    const MAIN: &str = "refs/heads/main";
    const ORIGIN_MAIN: &str = "refs/remotes/origin/main";

    #[test]
    fn fetch_request_updates_the_remote_tracking_ref() {
        let request = fetch_request("origin", MAIN, ORIGIN_MAIN).expect("origin/main");
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
    fn fetch_request_keeps_slashes_in_the_remote_and_the_branch() {
        for (remote, merge, tracking, refspec) in [
            (
                "origin",
                "refs/heads/feature/foo",
                "refs/remotes/origin/feature/foo",
                "+refs/heads/feature/foo:refs/remotes/origin/feature/foo",
            ),
            (
                "team/origin",
                MAIN,
                "refs/remotes/team/origin/main",
                "+refs/heads/main:refs/remotes/team/origin/main",
            ),
        ] {
            let request = fetch_request(remote, merge, tracking).expect(remote);
            assert_eq!(request.remote(), remote);
            assert_eq!(request.refspec(), refspec);
        }
    }

    #[test]
    fn fetch_request_rejects_what_cannot_be_fetched_or_would_change_the_refspec() {
        for (remote, merge, tracking) in [
            (".", MAIN, ORIGIN_MAIN),
            ("-origin", MAIN, ORIGIN_MAIN),
            ("--upload-pack=evil", MAIN, ORIGIN_MAIN),
            ("", MAIN, ORIGIN_MAIN),
            ("ori gin", MAIN, ORIGIN_MAIN),
            ("origin", "", ORIGIN_MAIN),
            ("origin", "main", ORIGIN_MAIN),
            ("origin", "refs/heads/ma\nin", ORIGIN_MAIN),
            ("origin", "refs/heads/ma:in", ORIGIN_MAIN),
            ("origin", MAIN, ""),
            ("origin", MAIN, "refs/heads/main"),
            ("origin", MAIN, "refs/remotes/origin/m*"),
        ] {
            assert!(
                fetch_request(remote, merge, tracking).is_none(),
                "{remote:?} {merge:?} {tracking:?}"
            );
        }
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
