//! Access to repositories through the system `git` executable.
//!
//! Reads do not write. Fetch updates the remote-tracking ref of the current
//! branch, and pull fast-forwards that branch. No libgit2 or gitoxide: the app
//! uses whatever git the user already has, so behavior (config, credentials,
//! hooks) matches their terminal.

mod branches;
mod discover;
mod exec;
mod graph;
mod log;
mod repo;
mod sync;

#[cfg(test)]
mod proptest_tests;

pub use branches::{Branch, Upstream, branches};
pub use discover::{DiscoverInputs, OVERRIDE_VAR, discover_with};
pub use exec::{Git, GitError, GitOutput, GitVersion, MIN_GIT_VERSION};
pub use graph::{
    GraphBranch, GraphCommit, GraphRow, GraphTip, GraphTrace, HistoryGraph, branch_graph,
    head_commit_oid, layout_graph,
};
pub use log::{Commit, CommitDetail, History, SHORT_OID_LEN, history, latest_commit};
pub use repo::{Head, RepoInfo, inspect};
pub use sync::{fetch, fetch_request, pull_ff_only};
