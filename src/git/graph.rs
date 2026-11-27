//! Commit graph for the branches the user has switched on.
//!
//! `git log --date-order` lists children before their parents. [`layout_graph`]
//! turns that list into lanes. The principal branch keeps the left lane for its
//! whole first-parent chain, including the history it shares with everyone
//! else. Another branch opens a lane to the right and runs there only while
//! its commits are not on that chain. The lanes meet on the commit where they
//! join.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::exec::{Git, GitError};
use super::log::{Person, author_and_co_authors};

/// Between the subject and the trailer block: a different byte from the
/// field separator, so a unit separator in either one stays put.
const TRAILERS_START: u8 = 0x1e;

/// Oid, parents, author after and before `.mailmap`, subject, then the
/// trailer block. `%(trailers)` with `only` and `unfold` exists since Git
/// 2.15, the minimum this app accepts.
const GRAPH_FORMAT: &str =
    "--format=%H%x1f%P%x1f%aN%x1f%aE%x1f%an%x1f%ae%x1f%s%x1e%(trailers:only,unfold)";

/// How many lane colors the history view cycles. Principal is always `0`.
/// The theme paints `color % LANE_COLORS`, so this is the palette length too.
pub const LANE_COLORS: usize = 8;

/// A branch tip to include in the graph. `color` comes from the sidebar index
/// so a branch keeps its color when a different one is switched off. The
/// principal tip is the exception: it is always color `0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphTip {
    pub name: String,
    pub oid: String,
    pub color: usize,
    /// This tip owns the left lane and the color of the shared history.
    pub principal: bool,
    /// Checked out. Detached `HEAD` counts. Its label is drawn first.
    pub current: bool,
}

/// One sidebar row [`graph_tips`] can turn into a [`GraphTip`].
pub struct GraphCandidate<'a> {
    pub name: &'a str,
    pub oid: Option<&'a str>,
    pub current: bool,
    pub committer_time: Option<i64>,
    pub checked: bool,
}

/// One commit, parents first-parent first, as `git log` prints them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphCommit {
    pub oid: String,
    pub summary: String,
    pub parents: Vec<String>,
    /// `%aN <%aE>`, so `.mailmap` applies.
    pub author: Person,
    /// From the commit's `Co-authored-by` trailers, without the author.
    pub co_authors: Vec<Person>,
}

/// The rows the history view paints, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HistoryGraph {
    pub rows: Vec<GraphRow>,
    pub truncated: bool,
    /// Widest row, so the hash column lines up down the list.
    pub columns: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphRow {
    pub oid: String,
    pub summary: String,
    pub author: Person,
    pub co_authors: Vec<Person>,
    /// Branch tips that point at this commit, in sidebar order.
    pub branches: Vec<GraphBranch>,
    /// Lane of the dot, in this row's top index space.
    pub commit_lane: usize,
    pub commit_color: usize,
    pub traces: Vec<GraphTrace>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphBranch {
    pub name: String,
    pub color: usize,
    /// Checked out. The history draws this label first and a little stronger.
    pub current: bool,
}

/// A stroke on one row.
///
/// `from_lane` is where the line enters at the top (`None` starts at the
/// commit dot). `to_lane` is where it leaves at the bottom (`None` ends at
/// the dot). Both index spaces share x positions: a line that leaves at lane
/// 2 meets the next row's lane 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphTrace {
    pub from_lane: Option<usize>,
    pub to_lane: Option<usize>,
    pub color: usize,
}

/// A lane is either holding the principal column open, or waiting for one commit.
/// Both at once used to be representable.
#[derive(Clone)]
enum Lane {
    Placeholder { color: usize },
    Waiting { next: String, color: usize },
}

impl Lane {
    fn color(&self) -> usize {
        match self {
            Self::Placeholder { color } | Self::Waiting { color, .. } => *color,
        }
    }

    fn next(&self) -> Option<&str> {
        match self {
            Self::Waiting { next, .. } => Some(next.as_str()),
            Self::Placeholder { .. } => None,
        }
    }

    fn is_placeholder(&self) -> bool {
        matches!(self, Self::Placeholder { .. })
    }
}

struct BottomLane {
    next: Option<String>,
    color: usize,
    from: Option<usize>,
    continues_commit: bool,
    placeholder: bool,
}

struct ExtraParent {
    parent: String,
    color: usize,
    into_top: Option<usize>,
}

/// Full hash of `HEAD`. Used when HEAD is detached, so that commit stays in
/// the graph with no branch checkbox of its own.
pub fn head_commit_oid(git: &Git, root: &Path) -> Result<String, GitError> {
    let args = ["rev-parse", "--verify", "HEAD"];
    let output = git.require_ok(root, &args)?;
    parse_oid(&output.stdout).ok_or_else(|| {
        super::exec::rejected(
            &format!("git {}", args.join(" ")),
            "unrecognized rev-parse output",
        )
    })
}

fn parse_oid(stdout: &[u8]) -> Option<String> {
    let mut bytes = stdout.to_vec();
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Checked branches, in sidebar order, plus detached `HEAD`.
///
/// The principal tip is `main`, then `master`, then `trunk`, when that branch
/// is checked. Otherwise detached `HEAD`. Otherwise the newest committer time,
/// and a tie keeps the earlier sidebar row. Being checked out does not win a
/// lane. That tip is color `0`. Every other checked branch keeps a color tied
/// to its sidebar index. Detached `HEAD`, when it is not principal, is colored
/// as a row after the last branch.
pub fn graph_tips(branches: &[GraphCandidate<'_>], detached_oid: Option<&str>) -> Vec<GraphTip> {
    let detached = detached_oid.filter(|oid| is_oid(oid));
    let named = named_principal(branches);
    let head_is_principal = named.is_none() && detached.is_some();
    let principal = if head_is_principal {
        None
    } else {
        named.or_else(|| newest_principal(branches))
    };

    let mut tips = Vec::new();
    for (index, branch) in branches.iter().enumerate() {
        if !branch.checked {
            continue;
        }
        let Some(oid) = branch.oid.filter(|oid| is_oid(oid)) else {
            continue;
        };
        let is_principal = principal == Some(index);
        tips.push(GraphTip {
            name: branch.name.to_owned(),
            oid: oid.to_owned(),
            color: lane_color(index, is_principal),
            principal: is_principal,
            current: branch.current,
        });
    }
    if let Some(oid) = detached {
        tips.push(GraphTip {
            name: "HEAD".to_owned(),
            oid: oid.to_owned(),
            color: if head_is_principal {
                0
            } else {
                lane_color(branches.len(), false)
            },
            principal: head_is_principal,
            current: true,
        });
    }
    tips
}

/// `main`, then `master`, then `trunk`, when that branch is checked and has a commit.
fn named_principal(branches: &[GraphCandidate<'_>]) -> Option<usize> {
    const NAMES: [&str; 3] = ["main", "master", "trunk"];
    for name in NAMES {
        if let Some(index) = branches.iter().position(|branch| {
            branch.checked && branch.oid.is_some_and(is_oid) && branch.name == name
        }) {
            return Some(index);
        }
    }
    None
}

/// The newest checked tip. The earlier sidebar row wins a tie. A missing time
/// sorts as oldest.
fn newest_principal(branches: &[GraphCandidate<'_>]) -> Option<usize> {
    branches
        .iter()
        .enumerate()
        .filter(|(_, branch)| branch.checked && branch.oid.is_some_and(is_oid))
        .max_by(|left, right| {
            left.1
                .committer_time
                .cmp(&right.1.committer_time)
                .then_with(|| right.0.cmp(&left.0))
        })
        .map(|(index, _)| index)
}

/// Painted slot for a sidebar row. Principal is always 0. Other rows use
/// slots 1 through 7, and the eighth non-principal row repeats slot 1.
/// `sidebar_index.max(1)` made index 0 and index 1 the same slot.
fn lane_color(sidebar_index: usize, principal: bool) -> usize {
    if principal {
        0
    } else {
        1 + sidebar_index % (LANE_COLORS - 1)
    }
}

/// Commits reachable from `tips`, newest first, laid out into lanes.
///
/// An empty tip list or a limit of zero does not run git. Passing the same
/// commit twice (two branches on one tip) keeps both labels and asks git once.
pub fn branch_graph(
    git: &Git,
    root: &Path,
    tips: &[GraphTip],
    limit: usize,
) -> Result<HistoryGraph, GitError> {
    if tips.is_empty() || limit == 0 {
        return Ok(HistoryGraph::default());
    }

    for tip in tips {
        if !is_oid(&tip.oid) {
            return Err(super::exec::rejected(
                "git log",
                format!("{} does not point at a commit hash", tip.name),
            ));
        }
    }

    // Git 2.55 parses --max-count as a 32-bit signed integer.
    let max_count = format!(
        "--max-count={}",
        limit.saturating_add(1).min(i32::MAX as usize)
    );
    let mut owned = vec![
        "log".to_owned(),
        "--date-order".to_owned(),
        "-z".to_owned(),
        GRAPH_FORMAT.to_owned(),
        "--no-color".to_owned(),
        max_count,
    ];
    let mut seen = HashSet::new();
    for tip in tips {
        if seen.insert(tip.oid.as_str()) {
            owned.push(tip.oid.clone());
        }
    }
    owned.push("--".to_owned());
    let args: Vec<&str> = owned.iter().map(String::as_str).collect();

    let output = git.require_ok(root, &args)?;
    let mut commits = parse_graph_log(&output.stdout).ok_or_else(|| {
        super::exec::rejected(
            &format!("git {}", args.join(" ")),
            "unrecognized log output",
        )
    })?;
    let truncated = commits.len() > limit;
    commits.truncate(limit);

    let mut graph = layout_graph(&commits, tips);
    graph.truncated = truncated;
    Ok(graph)
}

fn is_oid(oid: &str) -> bool {
    !oid.is_empty() && oid.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Parses `git log -z` records written with [`GRAPH_FORMAT`].
///
/// Parents are space-separated. The first six unit separators split the
/// fixed fields and the last record separator starts the trailer block, so a
/// unit separator inside the summary stays in the summary.
pub(crate) fn parse_graph_log(stdout: &[u8]) -> Option<Vec<GraphCommit>> {
    stdout
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .map(parse_graph_record)
        .collect()
}

fn parse_graph_record(record: &[u8]) -> Option<GraphCommit> {
    let parts = super::log::split_fields(record, 7)?;
    let oid = parts[0];
    let parents = parts[1];
    let identity = [parts[2], parts[3], parts[4], parts[5]];
    let rest = parts[6];
    let split = rest.iter().rposition(|byte| *byte == TRAILERS_START)?;
    let (summary, trailers) = (&rest[..split], &rest[split + 1..]);
    if !is_oid(&String::from_utf8_lossy(oid)) {
        return None;
    }
    let (author, co_authors) = author_and_co_authors(identity, &String::from_utf8_lossy(trailers));
    Some(GraphCommit {
        oid: String::from_utf8_lossy(oid).into_owned(),
        summary: String::from_utf8_lossy(summary).into_owned(),
        parents: parse_parents(parents)?,
        author,
        co_authors,
    })
}

fn parse_parents(bytes: &[u8]) -> Option<Vec<String>> {
    if bytes.is_empty() {
        return Some(Vec::new());
    }
    let mut parents = Vec::new();
    for parent in bytes.split(|byte| *byte == b' ') {
        if !is_oid(&String::from_utf8_lossy(parent)) {
            return None;
        }
        parents.push(String::from_utf8_lossy(parent).into_owned());
    }
    Some(parents)
}

/// Assigns a lane to every commit. `commits` must already be newest first,
/// with every parent after its children. Parents that are not in the list
/// (the history was cut off) end their lane instead of drawing a line to
/// nowhere.
///
/// When a tip is [`GraphTip::principal`], its first-parent chain stays in
/// lane 0, even if some other branch was committed more recently. Those other
/// branches use lanes to the right until they reach a commit on the chain.
///
/// Color is not the lane. The principal chain, and every commit reachable
/// from it that no selected branch claimed, uses the principal color. Each
/// other selected branch keeps the color of its own first-parent chain.
pub fn layout_graph(commits: &[GraphCommit], tips: &[GraphTip]) -> HistoryGraph {
    let trunk = trunk_oids(commits, tips);
    let spine_color = tips
        .iter()
        .find(|tip| tip.principal)
        .map(|tip| tip.color)
        .unwrap_or(0);
    let colors = commit_colors(commits, tips, &trunk, spine_color);
    let mut remaining: HashSet<String> = commits.iter().map(|commit| commit.oid.clone()).collect();
    let mut lanes: Vec<Lane> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());

    for commit in commits {
        remaining.remove(&commit.oid);
        let on_trunk = trunk.contains(&commit.oid);
        if !on_trunk && !trunk.is_empty() && lanes.is_empty() {
            lanes.push(Lane::Placeholder { color: spine_color });
        }
        rows.push(layout_row(
            commit,
            &mut lanes,
            &RowCtx {
                tips,
                remaining: &remaining,
                trunk: &trunk,
                spine_color,
                colors: &colors,
            },
        ));
    }

    let columns = rows.iter().map(row_span).max().unwrap_or(0);
    HistoryGraph {
        rows,
        truncated: false,
        columns,
    }
}

fn row_span(row: &GraphRow) -> usize {
    let mut max = row.commit_lane;
    for trace in &row.traces {
        if let Some(lane) = trace.from_lane {
            max = max.max(lane);
        }
        if let Some(lane) = trace.to_lane {
            max = max.max(lane);
        }
    }
    max.saturating_add(1)
}

fn trunk_oids(commits: &[GraphCommit], tips: &[GraphTip]) -> HashSet<String> {
    let Some(start) = tips
        .iter()
        .find(|tip| tip.principal)
        .map(|tip| tip.oid.as_str())
    else {
        return HashSet::new();
    };
    let mut by_oid: HashMap<&str, &GraphCommit> = HashMap::new();
    for commit in commits {
        by_oid.insert(commit.oid.as_str(), commit);
    }
    if !by_oid.contains_key(start) {
        return HashSet::new();
    }
    let mut trunk = HashSet::new();
    let mut cursor = Some(start.to_owned());
    while let Some(oid) = cursor {
        if !trunk.insert(oid.clone()) {
            break;
        }
        cursor = by_oid.get(oid.as_str()).and_then(|commit| {
            commit
                .parents
                .first()
                .filter(|parent| by_oid.contains_key(parent.as_str()))
                .cloned()
        });
    }
    trunk
}

/// Lane of `commit_oid`, and whether no line enters that lane from the row above.
fn occupy(
    lanes: &mut Vec<Lane>,
    commit_oid: &str,
    on_trunk: bool,
    spine_color: usize,
    commit_color: usize,
) -> (usize, bool) {
    if on_trunk {
        if let Some(lane) = lanes.first_mut() {
            if lane.is_placeholder() || lane.next() == Some(commit_oid) {
                let spawned = lane.next() != Some(commit_oid);
                *lane = Lane::Waiting {
                    next: commit_oid.to_owned(),
                    color: spine_color,
                };
                return (0, spawned);
            }
        } else {
            lanes.push(Lane::Waiting {
                next: commit_oid.to_owned(),
                color: spine_color,
            });
            return (0, true);
        }
    }

    if let Some(index) = lanes
        .iter()
        .position(|lane| lane.next() == Some(commit_oid))
    {
        return (index, false);
    }

    lanes.push(Lane::Waiting {
        next: commit_oid.to_owned(),
        color: commit_color,
    });
    (lanes.len() - 1, true)
}

struct RowCtx<'a> {
    tips: &'a [GraphTip],
    remaining: &'a HashSet<String>,
    trunk: &'a HashSet<String>,
    spine_color: usize,
    colors: &'a HashMap<String, usize>,
}

/// Lane already expecting `oid`, if joining it would not hide a second branch
/// behind another side branch that is only waiting for the principal history.
fn lane_waiting_for(lanes: &[Lane], oid: &str, trunk: &HashSet<String>) -> Option<usize> {
    let index = lanes
        .iter()
        .position(|lane| !lane.is_placeholder() && lane.next() == Some(oid))?;
    if trunk.contains(oid) && index != 0 {
        None
    } else {
        Some(index)
    }
}

fn layout_row(commit: &GraphCommit, lanes: &mut Vec<Lane>, ctx: &RowCtx<'_>) -> GraphRow {
    let on_trunk = ctx.trunk.contains(&commit.oid);
    let commit_color = ctx
        .colors
        .get(&commit.oid)
        .copied()
        .unwrap_or(ctx.spine_color);
    let (lane_idx, spawned) = occupy(lanes, &commit.oid, on_trunk, ctx.spine_color, commit_color);
    let top = lanes.clone();

    let parents = kept_parents(&commit.parents, ctx.remaining);
    let first = parents.first();
    // The principal chain keeps its parent. A side branch joins a lane that
    // already expects its parent, except when that lane is another side branch
    // waiting for the same principal commit: both stay open until they meet it.
    let merges_into = if on_trunk {
        None
    } else {
        first.and_then(|parent| {
            lane_waiting_for(&top, parent, ctx.trunk).filter(|index| *index != lane_idx)
        })
    };

    let mut seen = HashSet::new();
    if let Some(parent) = first {
        seen.insert(parent.clone());
    }
    let mut extras = Vec::new();
    for parent in parents.iter().skip(1) {
        if !seen.insert(parent.clone()) {
            continue;
        }
        let into_top = lane_waiting_for(&top, parent, ctx.trunk);
        if into_top.is_some() && into_top == merges_into {
            continue;
        }
        // A merge stroke takes the color of the parent it is joining, so a
        // selected branch stays visible up to the merge commit.
        let color = ctx.colors.get(parent).copied().unwrap_or(ctx.spine_color);
        extras.push(ExtraParent {
            parent: parent.clone(),
            color,
            into_top,
        });
    }

    let mut merging_from = Vec::new();
    let mut bottom = Vec::new();
    for (index, lane) in top.iter().enumerate() {
        if index == lane_idx {
            if merges_into.is_none()
                && let Some(parent) = first
            {
                bottom.push(BottomLane {
                    next: Some(parent.clone()),
                    color: commit_color,
                    from: Some(index),
                    continues_commit: true,
                    placeholder: false,
                });
            }
            for extra in &extras {
                if extra.into_top.is_none() {
                    bottom.push(BottomLane {
                        next: Some(extra.parent.clone()),
                        color: extra.color,
                        from: None,
                        continues_commit: false,
                        placeholder: false,
                    });
                }
            }
        } else if lane.is_placeholder() {
            bottom.push(BottomLane {
                next: None,
                color: lane.color(),
                from: None,
                continues_commit: false,
                placeholder: true,
            });
        } else if lane.next() == Some(commit.oid.as_str()) {
            merging_from.push(index);
        } else {
            bottom.push(BottomLane {
                next: lane.next().map(str::to_owned),
                color: lane.color(),
                from: Some(index),
                continues_commit: false,
                placeholder: false,
            });
        }
    }

    let mut traces = Vec::new();
    let mut top_to_bottom = vec![None; top.len()];
    for (index, lane) in bottom.iter().enumerate() {
        if lane.placeholder {
            continue;
        }
        if let Some(from) = lane.from {
            top_to_bottom[from] = Some(index);
        }
        let from_lane = if lane.continues_commit && spawned {
            None
        } else {
            lane.from
        };
        traces.push(GraphTrace {
            from_lane,
            to_lane: Some(index),
            color: lane.color,
        });
    }

    if let Some(target) = merges_into {
        if let Some(index) = top_to_bottom[target] {
            traces.push(GraphTrace {
                from_lane: None,
                to_lane: Some(index),
                color: commit_color,
            });
        }
        if !spawned {
            traces.push(GraphTrace {
                from_lane: Some(lane_idx),
                to_lane: None,
                color: commit_color,
            });
        }
    } else if first.is_none() && !spawned {
        traces.push(GraphTrace {
            from_lane: Some(lane_idx),
            to_lane: None,
            color: commit_color,
        });
    }

    for extra in &extras {
        let Some(target) = extra.into_top else {
            continue;
        };
        if let Some(index) = top_to_bottom[target] {
            traces.push(GraphTrace {
                from_lane: None,
                to_lane: Some(index),
                color: extra.color,
            });
        }
    }

    for from_index in merging_from {
        traces.push(GraphTrace {
            from_lane: Some(from_index),
            to_lane: None,
            color: top[from_index].color(),
        });
    }

    *lanes = bottom
        .into_iter()
        .map(|lane| match lane.next {
            Some(next) if !lane.placeholder => Lane::Waiting {
                next,
                color: lane.color,
            },
            _ => Lane::Placeholder { color: lane.color },
        })
        .collect();

    GraphRow {
        oid: commit.oid.clone(),
        summary: commit.summary.clone(),
        author: commit.author.clone(),
        co_authors: commit.co_authors.clone(),
        branches: tip_labels(ctx.tips, &commit.oid),
        commit_lane: lane_idx,
        commit_color,
        traces,
    }
}

fn kept_parents(parents: &[String], remaining: &HashSet<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut kept = Vec::new();
    for parent in parents {
        if !remaining.contains(parent) || !seen.insert(parent.clone()) {
            continue;
        }
        kept.push(parent.clone());
    }
    kept
}

fn tip_labels(tips: &[GraphTip], oid: &str) -> Vec<GraphBranch> {
    tips.iter()
        .filter(|tip| tip.oid == oid)
        .map(|tip| GraphBranch {
            name: tip.name.clone(),
            color: tip.color,
            current: tip.current,
        })
        .collect()
}

/// Color of every commit in `commits`.
///
/// The principal first-parent chain is the spine color. Other selected
/// branches then claim their own first-parent chain, oldest tip first, and
/// stop where a commit is already colored: a branch grown from another keeps
/// only the commits that are its alone. Anything still uncolored but reachable
/// from the principal (a merged branch that is not selected) takes the spine
/// color too. A commit that only a side branch brought in inherits the color
/// of the newest child that names it as a parent.
fn commit_colors(
    commits: &[GraphCommit],
    tips: &[GraphTip],
    trunk: &HashSet<String>,
    spine_color: usize,
) -> HashMap<String, usize> {
    let mut by_oid: HashMap<&str, &GraphCommit> = HashMap::new();
    for commit in commits {
        by_oid.insert(commit.oid.as_str(), commit);
    }
    let mut colors: HashMap<String, usize> = HashMap::new();
    for oid in trunk {
        colors.insert(oid.clone(), spine_color);
    }

    let mut claim: Vec<&GraphTip> = tips.iter().filter(|tip| !tip.principal).collect();
    claim.sort_by_key(|tip| {
        let at = commits
            .iter()
            .position(|commit| commit.oid == tip.oid)
            .unwrap_or(0);
        std::cmp::Reverse(at)
    });
    for tip in claim {
        let mut cursor = Some(tip.oid.as_str());
        while let Some(oid) = cursor {
            if colors.contains_key(oid) || !by_oid.contains_key(oid) {
                break;
            }
            colors.insert(oid.to_owned(), tip.color);
            cursor = by_oid
                .get(oid)
                .and_then(|commit| commit.parents.first().map(String::as_str));
        }
    }

    if let Some(start) = tips.iter().find(|tip| tip.principal) {
        let mut stack = vec![start.oid.as_str()];
        let mut seen = HashSet::new();
        while let Some(oid) = stack.pop() {
            if !seen.insert(oid) {
                continue;
            }
            let Some(commit) = by_oid.get(oid) else {
                continue;
            };
            colors.entry(oid.to_owned()).or_insert(spine_color);
            for parent in &commit.parents {
                if by_oid.contains_key(parent.as_str()) {
                    stack.push(parent.as_str());
                }
            }
        }
    }

    for commit in commits {
        let Some(&color) = colors.get(&commit.oid) else {
            continue;
        };
        for parent in &commit.parents {
            if by_oid.contains_key(parent.as_str()) {
                colors.entry(parent.clone()).or_insert(color);
            }
        }
    }
    colors
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::super::log::FIELD_SEPARATOR;
    use super::*;
    use crate::test_support::{OID_A, OID_B, OID_C};

    /// One record in [`GRAPH_FORMAT`]. `identity` is `%aN`, `%aE`, `%an`,
    /// `%ae`: the author after and before `.mailmap`.
    fn record(
        oid: &str,
        parents: &str,
        identity: [&str; 4],
        summary: &str,
        trailers: &str,
    ) -> Vec<u8> {
        let mut out = oid.as_bytes().to_vec();
        for field in [parents].into_iter().chain(identity) {
            out.push(FIELD_SEPARATOR);
            out.extend_from_slice(field.as_bytes());
        }
        out.push(FIELD_SEPARATOR);
        out.extend_from_slice(summary.as_bytes());
        out.push(TRAILERS_START);
        out.extend_from_slice(trailers.as_bytes());
        out.push(0);
        out
    }

    const CTHULHU: [&str; 4] = [
        "cthulhu",
        "cthulhu@example.invalid",
        "cthulhu",
        "cthulhu@example.invalid",
    ];

    fn person(name: &str, email: &str, aliases: &[&str]) -> Person {
        Person {
            aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
            ..Person::new(name, email)
        }
    }

    #[test]
    fn parses_graph_records_and_folds_their_authors() {
        let stdout = [
            // A trailer-shaped subject is not a trailer. A trailer with the
            // author's address is the author; one with the author's name
            // adds an alias.
            record(
                OID_A,
                &format!("{OID_B} {OID_C}"),
                [
                    "Fernando Diaz",
                    "Fernando@Example.com",
                    "Fernando Diaz",
                    "Fernando@Example.com",
                ],
                "Keep\u{1f}the separator\nCo-authored-by: Decoy <decoy@example.com>",
                "Co-authored-by: Fernando Diaz <FERNANDO@example.com>\n\
                 Co-authored-by: fernando diaz <alt@example.com>\n\
                 Co-authored-by: Cursor Agent <cursoragent@cursor.com>\n\
                 co-authored-by: Cursor Agent <CURSORAGENT@cursor.com>\n\
                 Co-authored-by: Other Person <other@example.com>\n",
            ),
            // `.mailmap` renamed the author: the recorded address is an
            // alias, and a trailer with the recorded name is the author too.
            record(
                OID_B,
                "",
                [
                    "Great Old One",
                    "great@rlyeh.test",
                    "cthulhu",
                    "cthulhu@example.invalid",
                ],
                "Rise\u{1e}again",
                "Co-authored-by: cthulhu <cthulhu@example.invalid>\n\
                 Co-authored-by: cthulhu <second@example.invalid>\n\
                 Co-authored-by: Tag\u{1f}ged <tag@example.invalid>\n",
            ),
            record(OID_C, "", CTHULHU, "root", ""),
        ]
        .concat();
        let commits = parse_graph_log(&stdout).expect("parsable");
        assert_eq!(
            commits,
            [
                GraphCommit {
                    oid: OID_A.to_owned(),
                    summary: "Keep\u{1f}the separator\nCo-authored-by: Decoy <decoy@example.com>"
                        .to_owned(),
                    parents: vec![OID_B.to_owned(), OID_C.to_owned()],
                    author: person(
                        "Fernando Diaz",
                        "Fernando@Example.com",
                        &["alt@example.com"]
                    ),
                    co_authors: vec![
                        Person::new("Cursor Agent", "cursoragent@cursor.com"),
                        Person::new("Other Person", "other@example.com"),
                    ],
                },
                GraphCommit {
                    oid: OID_B.to_owned(),
                    summary: "Rise\u{1e}again".to_owned(),
                    parents: Vec::new(),
                    author: person(
                        "Great Old One",
                        "great@rlyeh.test",
                        &["cthulhu@example.invalid", "second@example.invalid"],
                    ),
                    co_authors: vec![Person::new("Tag\u{1f}ged", "tag@example.invalid")],
                },
                GraphCommit {
                    oid: OID_C.to_owned(),
                    summary: "root".to_owned(),
                    parents: Vec::new(),
                    author: person("cthulhu", "cthulhu@example.invalid", &[]),
                    co_authors: Vec::new(),
                },
            ]
        );

        let graph = layout_graph(&commits, &[spine("main", OID_A, 0)]);
        assert_eq!(graph.rows[0].author, commits[0].author);
        assert_eq!(graph.rows[0].co_authors, commits[0].co_authors);
    }

    #[test]
    fn empty_and_malformed_graph_logs() {
        let cases = [
            (b"".to_vec(), Some(0)),
            (b"\0".to_vec(), Some(0)),
            (b"\0\0".to_vec(), Some(0)),
            (b"no separator\0".to_vec(), None),
            (record("not-hex", "", CTHULHU, "x", ""), None),
            (record(OID_A, "not-a-hash", CTHULHU, "x", ""), None),
            // A three-field record has no author or trailer block.
            (format!("{OID_A}\x1f\x1fsummary\0").into_bytes(), None),
        ];
        for (stdout, expected) in cases {
            let parsed = parse_graph_log(&stdout).map(|commits| commits.len());
            assert_eq!(parsed, expected, "{stdout:?}");
        }
    }

    proptest! {
        #![proptest_config(crate::test_support::proptest_config())]

        #[test]
        fn parse_graph_log_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
            let _ = parse_graph_log(&bytes);
        }
    }

    fn commit(oid: &str, summary: &str, parents: &[&str]) -> GraphCommit {
        GraphCommit {
            oid: oid.to_owned(),
            summary: summary.to_owned(),
            parents: parents.iter().map(|parent| (*parent).to_owned()).collect(),
            author: Person::new("cthulhu", "cthulhu@example.invalid"),
            co_authors: Vec::new(),
        }
    }

    fn tip(name: &str, oid: &str, color: usize) -> GraphTip {
        GraphTip {
            name: name.to_owned(),
            oid: oid.to_owned(),
            color,
            principal: false,
            current: false,
        }
    }

    fn spine(name: &str, oid: &str, color: usize) -> GraphTip {
        let mut tip = tip(name, oid, color);
        tip.principal = true;
        tip
    }

    /// A tip: name, commit, color, checked out. `main` is principal.
    type LaneTip = (&'static str, &'static str, usize, bool);

    /// One row: commit, labels as `name:color` (`*` when checked out),
    /// lane, color, and traces as `from>to:color`, where `.` is the dot.
    type Row = (&'static str, &'static str, usize, usize, &'static str);

    /// What the case shows, commits newest first as `(oid, parents)` (the
    /// oid doubles as the summary), the tips, the column count, and the rows.
    type Layout = (
        &'static str,
        &'static [(&'static str, &'static [&'static str])],
        &'static [LaneTip],
        usize,
        &'static [Row],
    );

    const LAYOUTS: &[Layout] = &[
        (
            "a linear history is one lane",
            &[("c3", &["c2"]), ("c2", &["c1"]), ("c1", &[])],
            &[("main", "c3", 0, false)],
            1,
            &[
                ("c3", "main:0", 0, 0, ".>0:0"),
                ("c2", "", 0, 0, "0>0:0"),
                ("c1", "", 0, 0, "0>.:0"),
            ],
        ),
        (
            "a parent outside the list leaves no dangling lane",
            &[("a", &["missing"])],
            &[("main", "a", 0, false)],
            1,
            &[("a", "main:0", 0, 0, "")],
        ),
        (
            "two branches on one commit share the dot, the checked-out one marked",
            &[("a", &[])],
            &[("main", "a", 0, false), ("feature", "a", 4, true)],
            1,
            &[("a", "main:0 feature:4*", 0, 0, "")],
        ),
        (
            "a branch already on the spine only adds its label",
            &[("main", &["old"]), ("old", &["base"]), ("base", &[])],
            &[("main", "main", 0, false), ("old", "old", 2, false)],
            1,
            &[
                ("main", "main:0", 0, 0, ".>0:0"),
                ("old", "old:2", 0, 0, "0>0:0"),
                ("base", "", 0, 0, "0>.:0"),
            ],
        ),
        (
            "diverging branches meet at their parent",
            &[("main", &["base"]), ("feature", &["base"]), ("base", &[])],
            &[("main", "main", 0, false), ("feature", "feature", 3, false)],
            2,
            &[
                ("main", "main:0", 0, 0, ".>0:0"),
                ("feature", "feature:3", 1, 3, "0>0:0 .>0:3"),
                ("base", "", 0, 0, "0>.:0"),
            ],
        ),
        (
            "a newer branch stays beside the principal spine",
            &[("feature", &["base"]), ("main", &["base"]), ("base", &[])],
            &[("main", "main", 0, false), ("feature", "feature", 1, false)],
            2,
            &[
                ("feature", "feature:1", 1, 1, ".>1:1"),
                ("main", "main:0", 0, 0, ".>0:0 1>1:1"),
                ("base", "", 0, 0, "0>.:0 1>.:1"),
            ],
        ),
        (
            "a merge opens a lane and shifts the ones to its right",
            &[
                ("m", &["a", "b"]),
                ("a", &["c", "d"]),
                ("b", &["c"]),
                ("d", &["c"]),
                ("c", &[]),
            ],
            &[("main", "m", 0, false)],
            3,
            &[
                ("m", "main:0", 0, 0, ".>0:0 .>1:0"),
                ("a", "", 0, 0, "0>0:0 .>1:0 1>2:0"),
                ("b", "", 2, 0, "0>0:0 1>1:0 .>0:0 2>.:0"),
                ("d", "", 1, 0, "0>0:0 .>0:0 1>.:0"),
                ("c", "", 0, 0, "0>.:0"),
            ],
        ),
        (
            "a merged branch returns to the principal lane",
            &[
                ("merge", &["mainline", "feature"]),
                ("mainline", &["base"]),
                ("feature", &["base"]),
                ("base", &[]),
            ],
            &[
                ("main", "merge", 0, false),
                ("feature", "feature", 1, false),
            ],
            2,
            &[
                ("merge", "main:0", 0, 0, ".>0:0 .>1:1"),
                ("mainline", "", 0, 0, "0>0:0 1>1:1"),
                ("feature", "feature:1", 1, 1, "0>0:0 .>0:1 1>.:1"),
                ("base", "", 0, 0, "0>.:0"),
            ],
        ),
        (
            "two side branches join the same principal commit",
            &[
                ("feature", &["base"]),
                ("topic", &["base"]),
                ("main", &["base"]),
                ("base", &[]),
            ],
            &[
                ("main", "main", 0, false),
                ("feature", "feature", 1, false),
                ("topic", "topic", 2, false),
            ],
            3,
            &[
                ("feature", "feature:1", 1, 1, ".>1:1"),
                ("topic", "topic:2", 2, 2, "1>1:1 .>2:2"),
                ("main", "main:0", 0, 0, ".>0:0 1>1:1 2>2:2"),
                ("base", "", 0, 0, "0>.:0 1>.:1 2>.:2"),
            ],
        ),
        (
            "unrelated histories do not meet",
            &[
                ("other", &["other-base"]),
                ("main", &["main-base"]),
                ("other-base", &[]),
                ("main-base", &[]),
            ],
            &[("main", "main", 0, false), ("other", "other", 1, false)],
            2,
            &[
                ("other", "other:1", 1, 1, ".>1:1"),
                ("main", "main:0", 0, 0, ".>0:0 1>1:1"),
                ("other-base", "", 1, 1, "0>0:0 1>.:1"),
                ("main-base", "", 0, 0, "0>.:0"),
            ],
        ),
        (
            "merged history is one color when only the principal is selected",
            &[("m", &["a", "b"]), ("a", &["c"]), ("b", &["c"]), ("c", &[])],
            &[("main", "m", 0, false)],
            2,
            &[
                ("m", "main:0", 0, 0, ".>0:0 .>1:0"),
                ("a", "", 0, 0, "0>0:0 1>1:0"),
                ("b", "", 1, 0, "0>0:0 .>0:0 1>.:0"),
                ("c", "", 0, 0, "0>.:0"),
            ],
        ),
        (
            "a selected branch keeps its color through two merges",
            &[
                ("m2", &["m1", "f2"]),
                ("f2", &["f1"]),
                ("m1", &["base", "f1"]),
                ("f1", &["base"]),
                ("base", &[]),
            ],
            &[("main", "m2", 0, false), ("feature", "f2", 1, false)],
            2,
            &[
                ("m2", "main:0", 0, 0, ".>0:0 .>1:1"),
                ("f2", "feature:1", 1, 1, "0>0:0 1>1:1"),
                ("m1", "", 0, 0, "0>0:0 1>1:1 .>1:1"),
                ("f1", "", 1, 1, "0>0:0 .>0:1 1>.:1"),
                ("base", "", 0, 0, "0>.:0"),
            ],
        ),
        (
            "an unselected branch merged into a selected one takes its color",
            &[
                ("feature", &["merge"]),
                ("main", &["base"]),
                ("merge", &["fline", "side"]),
                ("side", &["base"]),
                ("fline", &["base"]),
                ("base", &[]),
            ],
            &[("main", "main", 0, false), ("feature", "feature", 1, false)],
            3,
            &[
                ("feature", "feature:1", 1, 1, ".>1:1"),
                ("main", "main:0", 0, 0, ".>0:0 1>1:1"),
                ("merge", "", 1, 1, "0>0:0 1>1:1 .>2:1"),
                ("side", "", 2, 1, "0>0:0 1>1:1 .>0:1 2>.:1"),
                ("fline", "", 1, 1, "0>0:0 .>0:1 1>.:1"),
                ("base", "", 0, 0, "0>.:0"),
            ],
        ),
        (
            "a branch based on another selected one colors only its own commits",
            &[
                ("feature", &["topic"]),
                ("topic", &["base"]),
                ("main", &["base"]),
                ("base", &[]),
            ],
            &[
                ("main", "main", 0, false),
                ("feature", "feature", 1, false),
                ("topic", "topic", 2, false),
            ],
            2,
            &[
                ("feature", "feature:1", 1, 1, ".>1:1"),
                ("topic", "topic:2", 1, 2, "1>1:2"),
                ("main", "main:0", 0, 0, ".>0:0 1>1:2"),
                ("base", "", 0, 0, "0>.:0 1>.:2"),
            ],
        ),
        (
            "without a principal no lane is held open",
            &[("f", &["base"]), ("t", &["base"]), ("base", &[])],
            &[("feature", "f", 1, false), ("topic", "t", 2, false)],
            2,
            &[
                ("f", "feature:1", 0, 1, ".>0:1"),
                ("t", "topic:2", 1, 2, "0>0:1 .>0:2"),
                ("base", "", 0, 2, "0>.:2"),
            ],
        ),
        (
            "tips outside the window hold no lane and get no label",
            &[("a", &["b"]), ("b", &[])],
            &[
                ("main", "gone", 0, false),
                ("feature", "a", 3, false),
                ("old", "lost", 5, false),
            ],
            1,
            &[("a", "feature:3", 0, 3, ".>0:3"), ("b", "", 0, 3, "0>.:3")],
        ),
        (
            "a second parent continues the lane when the first is outside the window",
            &[("tip", &["m"]), ("m", &["missing", "b"]), ("b", &[])],
            &[("main", "tip", 0, false)],
            1,
            &[
                ("tip", "main:0", 0, 0, ".>0:0"),
                ("m", "", 0, 0, "0>0:0"),
                ("b", "", 0, 0, "0>.:0"),
            ],
        ),
        (
            "a repeated parent is drawn once",
            &[
                ("m", &["a", "b", "b", "a"]),
                ("a", &["c"]),
                ("b", &["c"]),
                ("c", &[]),
            ],
            &[("main", "m", 0, false)],
            2,
            &[
                ("m", "main:0", 0, 0, ".>0:0 .>1:0"),
                ("a", "", 0, 0, "0>0:0 1>1:0"),
                ("b", "", 1, 0, "0>0:0 .>0:0 1>.:0"),
                ("c", "", 0, 0, "0>.:0"),
            ],
        ),
    ];

    fn traces(spec: &str) -> Vec<GraphTrace> {
        let lane = |text: &str| (text != ".").then(|| text.parse().expect("lane"));
        spec.split_whitespace()
            .map(|trace| {
                let (lanes, color) = trace.split_once(':').expect("color");
                let (from, to) = lanes.split_once('>').expect("lanes");
                GraphTrace {
                    from_lane: lane(from),
                    to_lane: lane(to),
                    color: color.parse().expect("color"),
                }
            })
            .collect()
    }

    fn labels(row: &GraphRow) -> String {
        let label = |branch: &GraphBranch| {
            let current = if branch.current { "*" } else { "" };
            format!("{}:{}{current}", branch.name, branch.color)
        };
        row.branches.iter().map(label).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn layout_table() {
        for &(case, commits, tips, columns, rows) in LAYOUTS {
            let commits: Vec<GraphCommit> = commits
                .iter()
                .map(|(oid, parents)| commit(oid, oid, parents))
                .collect();
            let tips: Vec<GraphTip> = tips
                .iter()
                .map(|&(name, oid, color, current)| GraphTip {
                    principal: name == "main",
                    current,
                    ..tip(name, oid, color)
                })
                .collect();
            let graph = layout_graph(&commits, &tips);
            assert_eq!(graph.columns, columns, "{case}");
            let got: Vec<_> = graph
                .rows
                .iter()
                .map(|row| {
                    let shape = (row.commit_lane, row.commit_color, row.traces.clone());
                    (row.summary.as_str(), labels(row), shape)
                })
                .collect();
            let expected: Vec<_> = rows
                .iter()
                .map(|&(summary, labels, lane, color, spec)| {
                    (summary, labels.to_owned(), (lane, color, traces(spec)))
                })
                .collect();
            assert_eq!(got, expected, "{case}");
        }
    }

    /// A sidebar row: name, tip, checked out, committer time, checked.
    type Candidate = (&'static str, Option<&'static str>, bool, Option<i64>, bool);
    /// A graph tip: name, color, principal, checked out. Its oid is the
    /// candidate's, or the detached one for `HEAD`.
    type Tip = (&'static str, usize, bool, bool);

    /// What the case shows, the sidebar, detached `HEAD`, and the tips.
    type TipsCase = (
        &'static str,
        &'static [Candidate],
        Option<&'static str>,
        &'static [Tip],
    );

    const TIPS: &[TipsCase] = &[
        (
            "a checked main is principal, not the checked-out branch",
            &[
                ("feature", Some(OID_A), true, Some(1), true),
                ("main", Some(OID_B), false, Some(50), true),
                ("topic", Some(OID_C), false, Some(40), true),
            ],
            None,
            &[
                ("feature", 1, false, true),
                ("main", 0, true, false),
                ("topic", 3, false, false),
            ],
        ),
        (
            "unchecking a branch does not recolor the ones still shown",
            &[
                ("feature", Some(OID_A), true, Some(1), true),
                ("main", Some(OID_B), false, Some(50), true),
                ("topic", Some(OID_C), false, Some(40), false),
            ],
            None,
            &[("feature", 1, false, true), ("main", 0, true, false)],
        ),
        (
            "being checked out does not win the lane",
            &[
                ("feature", Some(OID_A), true, Some(1), true),
                ("topic", Some(OID_B), false, Some(40), true),
            ],
            None,
            &[("feature", 1, false, true), ("topic", 0, true, false)],
        ),
        (
            "an unchecked checkout falls back to main",
            &[
                ("feature", Some(OID_A), true, Some(90), false),
                ("topic", Some(OID_B), false, Some(80), true),
                ("main", Some(OID_C), false, Some(1), true),
            ],
            None,
            &[("topic", 2, false, false), ("main", 0, true, false)],
        ),
        (
            "then master",
            &[
                ("feature", Some(OID_A), false, Some(90), true),
                ("master", Some(OID_B), false, Some(1), true),
            ],
            None,
            &[("feature", 1, false, false), ("master", 0, true, false)],
        ),
        (
            "then trunk",
            &[
                ("feature", Some(OID_A), false, Some(90), true),
                ("trunk", Some(OID_B), false, Some(1), true),
            ],
            None,
            &[("feature", 1, false, false), ("trunk", 0, true, false)],
        ),
        (
            "without a default name the newest tip, and no time is oldest",
            &[
                ("old", Some(OID_A), false, Some(10), true),
                ("new", Some(OID_B), false, Some(40), true),
                ("mid", Some(OID_C), false, None, true),
            ],
            None,
            &[
                ("old", 1, false, false),
                ("new", 0, true, false),
                ("mid", 3, false, false),
            ],
        ),
        (
            "a tie keeps the earlier sidebar row",
            &[
                ("beta", Some(OID_A), false, Some(10), true),
                ("alpha", Some(OID_B), false, Some(10), true),
            ],
            None,
            &[("beta", 0, true, false), ("alpha", 2, false, false)],
        ),
        (
            "a checked main is principal ahead of detached HEAD",
            &[("main", Some(OID_A), false, Some(10), true)],
            Some(OID_B),
            &[("main", 0, true, false), ("HEAD", 2, false, true)],
        ),
        (
            "detached HEAD is principal without a checked default branch",
            &[("feature", Some(OID_A), false, Some(10), true)],
            Some(OID_B),
            &[("feature", 1, false, false), ("HEAD", 0, true, true)],
        ),
        (
            "detached HEAD takes the slot after the last row",
            &[
                ("feature", Some(OID_A), true, Some(5), true),
                ("bugfix", Some(OID_B), false, Some(4), true),
                ("main", Some(OID_C), false, Some(1), true),
            ],
            Some("7777777777777777777777777777777777777777"),
            &[
                ("feature", 1, false, true),
                ("bugfix", 2, false, false),
                ("main", 0, true, false),
                ("HEAD", 4, false, true),
            ],
        ),
        (
            "unchecked branches and branches without commits are left out",
            &[
                ("main", None, true, None, true),
                ("feature", Some(OID_A), false, Some(10), false),
            ],
            None,
            &[],
        ),
        (
            "the eighth row after the principal repeats slot 1, never slot 0",
            &[
                ("b0", Some(OID_A), false, Some(1), true),
                ("b1", Some(OID_B), false, Some(1), true),
                ("b2", Some(OID_C), false, Some(1), true),
                ("b3", Some(OID_C), false, Some(1), true),
                ("b4", Some(OID_C), false, Some(1), true),
                ("b5", Some(OID_C), false, Some(1), true),
                ("b6", Some(OID_C), false, Some(1), true),
                ("b7", Some(OID_C), false, Some(1), true),
                ("main", Some(OID_A), true, Some(9), true),
            ],
            None,
            &[
                ("b0", 1, false, false),
                ("b1", 2, false, false),
                ("b2", 3, false, false),
                ("b3", 4, false, false),
                ("b4", 5, false, false),
                ("b5", 6, false, false),
                ("b6", 7, false, false),
                ("b7", 1, false, false),
                ("main", 0, true, true),
            ],
        ),
    ];

    #[test]
    fn graph_tips_table() {
        for &(case, rows, detached, expected) in TIPS {
            let candidates: Vec<GraphCandidate> = rows
                .iter()
                .map(
                    |&(name, oid, current, committer_time, checked)| GraphCandidate {
                        name,
                        oid,
                        current,
                        committer_time,
                        checked,
                    },
                )
                .collect();
            let oid_of = |name: &str| match name {
                "HEAD" => detached,
                _ => rows.iter().find(|row| row.0 == name).and_then(|row| row.1),
            };
            let expected: Vec<GraphTip> = expected
                .iter()
                .map(|&(name, color, principal, current)| GraphTip {
                    name: name.to_owned(),
                    oid: oid_of(name).expect("tip oid").to_owned(),
                    color,
                    principal,
                    current,
                })
                .collect();
            assert_eq!(graph_tips(&candidates, detached), expected, "{case}");
        }
    }
}
