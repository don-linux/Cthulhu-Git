//! Fetch the current branch's upstream, and fast-forward pull.
//!
//! Fetch names one remote-tracking ref. It does not pass `--all` or `--prune`.
//! Pull is `git pull --ff-only --no-rebase`. `--no-rebase` is required:
//! Git 2.15 through 2.33 rebase anyway when `pull.rebase` or
//! `branch.<name>.rebase` is set, and `--ff-only` alone does not stop them.
//! A non-fast-forward leaves the work tree untouched.

use std::path::Path;
use std::time::Duration;

use super::exec::{Git, GitError, classify_failure, rejected};

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
    // `%(upstream)` comes from the last `remote` and the first `merge`, the
    // pair git itself uses, so the refspec matches the tracking ref.
    let remote = config_values(git, root, &format!("branch.{branch}.remote"))?.pop();
    let merge = config_values(git, root, &format!("branch.{branch}.merge"))?
        .into_iter()
        .next();
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

/// Every value of `key`, in the order git reads them. Empty when the key is
/// not set; any other failure, such as a broken config file, is an error.
fn config_values(git: &Git, root: &Path, key: &str) -> Result<Vec<String>, GitError> {
    let args = ["config", "--get-all", "-z", key];
    let output = git.run(root, &args)?;
    if output.status.code() == Some(1) {
        return Ok(Vec::new());
    }
    if !output.status.success() {
        return Err(classify_failure(root, &args, output.status, &output.stderr));
    }
    let mut values: Vec<String> = output
        .stdout
        .split(|byte| *byte == 0)
        .map(|value| String::from_utf8_lossy(value).into_owned())
        .collect();
    // `-z` ends every value with NUL, so the last piece is empty.
    values.pop();
    Ok(values)
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

    /// Remote, merge ref, tracking ref, and the refspec fetched. `None` is
    /// refused: a local upstream, an option-shaped remote, or anything that
    /// would change the refspec.
    const FETCHES: &[(&str, &str, &str, Option<&str>)] = &[
        (
            "origin",
            MAIN,
            ORIGIN_MAIN,
            Some("+refs/heads/main:refs/remotes/origin/main"),
        ),
        (
            "origin",
            "refs/heads/feature/foo",
            "refs/remotes/origin/feature/foo",
            Some("+refs/heads/feature/foo:refs/remotes/origin/feature/foo"),
        ),
        (
            "team/origin",
            MAIN,
            "refs/remotes/team/origin/main",
            Some("+refs/heads/main:refs/remotes/team/origin/main"),
        ),
        (".", MAIN, ORIGIN_MAIN, None),
        ("-origin", MAIN, ORIGIN_MAIN, None),
        ("--upload-pack=evil", MAIN, ORIGIN_MAIN, None),
        ("", MAIN, ORIGIN_MAIN, None),
        ("ori gin", MAIN, ORIGIN_MAIN, None),
        ("origin", "", ORIGIN_MAIN, None),
        ("origin", "main", ORIGIN_MAIN, None),
        ("origin", "refs/heads/ma\nin", ORIGIN_MAIN, None),
        ("origin", "refs/heads/ma:in", ORIGIN_MAIN, None),
        ("origin", MAIN, "", None),
        ("origin", MAIN, "refs/heads/main", None),
        ("origin", MAIN, "refs/remotes/origin/m*", None),
    ];

    #[test]
    fn fetch_request_table() {
        for &(remote, merge, tracking, refspec) in FETCHES {
            let case = (remote, merge, tracking);
            let request = fetch_request(remote, merge, tracking);
            let got = request
                .as_ref()
                .map(|request| (request.args(), request.remote(), request.refspec()));
            let expected =
                refspec.map(|refspec| (["fetch", "--", remote, refspec], remote, refspec));
            assert_eq!(got, expected, "{case:?}");
        }
    }

    type Version = (u32, u32, u32);

    const OLD: Version = (2, 15, 0);
    const SWITCH: Version = (2, 23, 0);

    /// Git version, branch name, and the argv after `git`. `None` is refused.
    const SWITCHES: &[(Version, &str, Option<&[&str]>)] = &[
        (
            SWITCH,
            "main",
            Some(&["switch", "--no-guess", "--", "main"]),
        ),
        (
            SWITCH,
            "feature/necronomicon",
            Some(&["switch", "--no-guess", "--", "feature/necronomicon"]),
        ),
        (
            SWITCH,
            "HEAD",
            Some(&["switch", "--no-guess", "--", "HEAD"]),
        ),
        // A branch named like an option stays one argument after `--`.
        (
            SWITCH,
            "-switch",
            Some(&["switch", "--no-guess", "--", "-switch"]),
        ),
        (
            SWITCH,
            "--upload-pack=evil",
            Some(&["switch", "--no-guess", "--", "--upload-pack=evil"]),
        ),
        (
            SWITCH,
            "--force",
            Some(&["switch", "--no-guess", "--", "--force"]),
        ),
        (SWITCH, "-f", Some(&["switch", "--no-guess", "--", "-f"])),
        (
            SWITCH,
            "--detach",
            Some(&["switch", "--no-guess", "--", "--detach"]),
        ),
        (
            SWITCH,
            "--orphan",
            Some(&["switch", "--no-guess", "--", "--orphan"]),
        ),
        (
            SWITCH,
            "--guess",
            Some(&["switch", "--no-guess", "--", "--guess"]),
        ),
        (SWITCH, "-B", Some(&["switch", "--no-guess", "--", "-B"])),
        // Before 2.23 checkout would read a leading `-` as an option.
        (OLD, "main", Some(&["checkout", "main", "--"])),
        (OLD, "feature/foo", Some(&["checkout", "feature/foo", "--"])),
        (OLD, "HEAD", Some(&["checkout", "HEAD", "--"])),
        (OLD, "-switch", None),
        (OLD, "--upload-pack=evil", None),
        (OLD, "-f", None),
    ];

    /// Not a ref under `refs/heads/`, on any Git.
    const NOT_BRANCHES: [&str; 12] = [
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

    #[test]
    fn switch_request_table() {
        let refused = NOT_BRANCHES
            .into_iter()
            .flat_map(|name| [OLD, SWITCH, (2, 53, 0)].map(|version| (version, name, None)));
        for (version, name, expected) in SWITCHES.iter().copied().chain(refused) {
            let request = switch_request(version, name);
            let args = request.as_ref().map(SwitchRequest::args);
            assert_eq!(args.as_deref(), expected, "{version:?} {name:?}");
        }
    }
}
