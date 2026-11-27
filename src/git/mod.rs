//! Access to repositories through the system `git` executable.
//!
//! Reads do not write. Fetch updates the remote-tracking ref of the current
//! branch, pull fast-forwards that branch, and switch checks out another local
//! branch. No libgit2 or gitoxide: the app uses whatever git the user already
//! has, so behavior (config, credentials, hooks) matches their terminal.

mod branches;
mod discover;
mod exec;
mod glance;
mod graph;
mod log;
#[cfg(unix)]
mod reap;
mod remotes;
mod repo;
mod sync;

pub use branches::{Branch, Upstream, branches};
pub use discover::{DiscoverInputs, OVERRIDE_VAR, discover_with};
pub use exec::{
    Git, GitError, GitOutput, GitVersion, MIN_GIT_VERSION, env_names_match, git_child_path,
    inherited_git_var_is_cleared,
};
pub use glance::{CommitGlance, Divergence, RepoGlance, glance};
pub use graph::{
    GraphBranch, GraphCandidate, GraphCommit, GraphRow, GraphTip, GraphTrace, HistoryGraph,
    LANE_COLORS, branch_graph, graph_tips, head_commit_oid, layout_graph,
};
pub use log::{
    CommitDetail, CommitFile, CommitHover, CommitStats, DiffKind, DiffLine, FileDiff, Person,
    SHORT_OID_LEN, commit_hover, email_key, file_diff, latest_commit, parse_co_authors, short_oid,
};
#[cfg(unix)]
pub use reap::reap_process_tree;
pub use remotes::{Remote, remotes};
pub use repo::{Head, RepoInfo, inspect};
pub use sync::{fetch, pull_ff_only, switch_branch};
