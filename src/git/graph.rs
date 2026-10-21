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

const FIELD_SEPARATOR: u8 = 0x1f;

/// How many lane colors the history view cycles. Principal is always `0`.
/// The same count lives in the theme palette.
const LANE_COLORS: usize = 8;

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

#[derive(Clone)]
struct Lane {
    /// Commit this lane is waiting to draw. `None` holds the principal column
    /// open until that branch's tip appears.
    next: Option<String>,
    color: usize,
    placeholder: bool,
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
    parse_oid(&output.stdout).ok_or_else(|| GitError::Failed {
        command: format!("git {}", args.join(" ")),
        status: output.status.to_string(),
        stderr: "unrecognized rev-parse output".to_owned(),
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
/// The principal tip is the checked-out branch when it is checked. Detached
/// `HEAD` is principal in its place. Otherwise `main`, then `master`, then
/// `trunk`. Otherwise the newest committer time, and a tie keeps the earlier
/// sidebar row. That tip is color `0`. Every other checked branch keeps a
/// color tied to its sidebar index.
pub fn graph_tips(branches: &[GraphCandidate<'_>], detached_oid: Option<&str>) -> Vec<GraphTip> {
    let detached = detached_oid.filter(|oid| is_oid(oid));
    let current = branches
        .iter()
        .position(|branch| branch.current && branch.checked && branch.oid.is_some_and(is_oid));
    let head_is_principal = detached.is_some() && current.is_none();
    let principal = if head_is_principal {
        None
    } else {
        current.or_else(|| fallback_principal(branches))
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
        });
    }
    if let Some(oid) = detached {
        tips.push(GraphTip {
            name: "HEAD".to_owned(),
            oid: oid.to_owned(),
            color: 0,
            principal: head_is_principal,
        });
    }
    tips
}

/// `main`, then `master`, then `trunk`. Otherwise the newest tip, with the
/// earlier sidebar row winning a tie. Times that are missing sort as oldest.
fn fallback_principal(branches: &[GraphCandidate<'_>]) -> Option<usize> {
    const NAMES: [&str; 3] = ["main", "master", "trunk"];
    for name in NAMES {
        if let Some(index) = branches.iter().position(|branch| {
            branch.checked && branch.oid.is_some_and(is_oid) && branch.name == name
        }) {
            return Some(index);
        }
    }
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

fn lane_color(sidebar_index: usize, principal: bool) -> usize {
    if principal {
        return 0;
    }
    let color = sidebar_index.max(1);
    if color.is_multiple_of(LANE_COLORS) {
        color + 1
    } else {
        color
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
            return Err(GitError::Failed {
                command: "git log".to_owned(),
                status: "invalid tip".to_owned(),
                stderr: format!("{} does not point at a commit hash", tip.name),
            });
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
        "--format=%H%x1f%P%x1f%s".to_owned(),
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
    let mut commits = parse_graph_log(&output.stdout).ok_or_else(|| GitError::Failed {
        command: format!("git {}", args.join(" ")),
        status: output.status.to_string(),
        stderr: "unrecognized log output".to_owned(),
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

/// Parses `git log -z --format=%H%x1f%P%x1f%s`.
///
/// Parents are space-separated. A unit separator inside the summary stays in
/// the summary; the first two separators split the fixed fields.
pub(crate) fn parse_graph_log(stdout: &[u8]) -> Option<Vec<GraphCommit>> {
    stdout
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .map(parse_graph_record)
        .collect()
}

fn parse_graph_record(record: &[u8]) -> Option<GraphCommit> {
    let mut parts = record.splitn(3, |byte| *byte == FIELD_SEPARATOR);
    let oid = parts.next()?;
    let parents = parts.next()?;
    let summary = parts.next()?;
    if !is_oid(&String::from_utf8_lossy(oid)) {
        return None;
    }
    Some(GraphCommit {
        oid: String::from_utf8_lossy(oid).into_owned(),
        summary: String::from_utf8_lossy(summary).into_owned(),
        parents: parse_parents(parents)?,
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
/// lane 0 and keeps that tip's color, even if some other branch was committed
/// more recently. Those other branches use lanes to the right until they reach
/// a commit on the chain.
pub fn layout_graph(commits: &[GraphCommit], tips: &[GraphTip]) -> HistoryGraph {
    let trunk = trunk_oids(commits, tips);
    let spine_color = tips
        .iter()
        .find(|tip| tip.principal)
        .map(|tip| tip.color)
        .unwrap_or(0);
    let mut remaining: HashSet<String> = commits.iter().map(|commit| commit.oid.clone()).collect();
    let mut lanes: Vec<Lane> = Vec::new();
    let mut extra_color = tips
        .iter()
        .map(|tip| tip.color)
        .max()
        .map_or(0, |color| color.saturating_add(1));
    let mut rows = Vec::with_capacity(commits.len());

    for commit in commits {
        remaining.remove(&commit.oid);
        let on_trunk = trunk.contains(&commit.oid);
        if !on_trunk && !trunk.is_empty() && lanes.is_empty() {
            lanes.push(Lane {
                next: None,
                color: spine_color,
                placeholder: true,
            });
        }
        rows.push(layout_row(
            commit,
            &mut lanes,
            &mut extra_color,
            &RowCtx {
                tips,
                remaining: &remaining,
                trunk: &trunk,
                spine_color,
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
    tips: &[GraphTip],
    extra_color: &mut usize,
) -> (usize, bool) {
    if on_trunk {
        if let Some(lane) = lanes.first_mut() {
            if lane.placeholder || lane.next.as_deref() == Some(commit_oid) {
                let spawned = lane.next.as_deref() != Some(commit_oid);
                lane.placeholder = false;
                lane.next = Some(commit_oid.to_owned());
                lane.color = spine_color;
                return (0, spawned);
            }
        } else {
            lanes.push(Lane {
                next: Some(commit_oid.to_owned()),
                color: spine_color,
                placeholder: false,
            });
            return (0, true);
        }
    }

    if let Some(index) = lanes
        .iter()
        .position(|lane| lane.next.as_deref() == Some(commit_oid))
    {
        return (index, false);
    }

    let color = if on_trunk {
        spine_color
    } else {
        tip_color(tips, commit_oid).unwrap_or_else(|| take_color(extra_color))
    };
    lanes.push(Lane {
        next: Some(commit_oid.to_owned()),
        color,
        placeholder: false,
    });
    (lanes.len() - 1, true)
}

struct RowCtx<'a> {
    tips: &'a [GraphTip],
    remaining: &'a HashSet<String>,
    trunk: &'a HashSet<String>,
    spine_color: usize,
}

/// Lane already expecting `oid`, if joining it would not hide a second branch
/// behind another side branch that is only waiting for the principal history.
fn lane_waiting_for(lanes: &[Lane], oid: &str, trunk: &HashSet<String>) -> Option<usize> {
    let index = lanes
        .iter()
        .position(|lane| !lane.placeholder && lane.next.as_deref() == Some(oid))?;
    if trunk.contains(oid) && index != 0 {
        None
    } else {
        Some(index)
    }
}

fn layout_row(
    commit: &GraphCommit,
    lanes: &mut Vec<Lane>,
    extra_color: &mut usize,
    ctx: &RowCtx<'_>,
) -> GraphRow {
    let on_trunk = ctx.trunk.contains(&commit.oid);
    let (lane_idx, spawned) = occupy(
        lanes,
        &commit.oid,
        on_trunk,
        ctx.spine_color,
        ctx.tips,
        extra_color,
    );
    let commit_color = if on_trunk {
        ctx.spine_color
    } else {
        lanes[lane_idx].color
    };
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
        let color = tip_color(ctx.tips, parent).unwrap_or_else(|| take_color(extra_color));
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
        } else if lane.placeholder {
            bottom.push(BottomLane {
                next: None,
                color: lane.color,
                from: None,
                continues_commit: false,
                placeholder: true,
            });
        } else if lane.next.as_deref() == Some(commit.oid.as_str()) {
            merging_from.push(index);
        } else {
            bottom.push(BottomLane {
                next: lane.next.clone(),
                color: lane.color,
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
            color: top[from_index].color,
        });
    }

    *lanes = bottom
        .into_iter()
        .map(|lane| Lane {
            next: lane.next,
            color: lane.color,
            placeholder: lane.placeholder,
        })
        .collect();

    GraphRow {
        oid: commit.oid.clone(),
        summary: commit.summary.clone(),
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

fn tip_color(tips: &[GraphTip], oid: &str) -> Option<usize> {
    tips.iter().find(|tip| tip.oid == oid).map(|tip| tip.color)
}

fn tip_labels(tips: &[GraphTip], oid: &str) -> Vec<GraphBranch> {
    tips.iter()
        .filter(|tip| tip.oid == oid)
        .map(|tip| GraphBranch {
            name: tip.name.clone(),
            color: tip.color,
        })
        .collect()
}

fn take_color(next: &mut usize) -> usize {
    let color = *next;
    *next = next.saturating_add(1);
    color
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    const OID_A: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    const OID_B: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
    const OID_C: &str = "8b137891791fe96927ad78e64b0aad7bded08eec";

    fn record(oid: &str, parents: &str, summary: &[u8]) -> Vec<u8> {
        let mut out = oid.as_bytes().to_vec();
        out.push(FIELD_SEPARATOR);
        out.extend_from_slice(parents.as_bytes());
        out.push(FIELD_SEPARATOR);
        out.extend_from_slice(summary);
        out.push(0);
        out
    }

    fn commit(oid: &str, summary: &str, parents: &[&str]) -> GraphCommit {
        GraphCommit {
            oid: oid.to_owned(),
            summary: summary.to_owned(),
            parents: parents.iter().map(|parent| (*parent).to_owned()).collect(),
        }
    }

    fn tip(name: &str, oid: &str, color: usize) -> GraphTip {
        GraphTip {
            name: name.to_owned(),
            oid: oid.to_owned(),
            color,
            principal: false,
        }
    }

    fn spine(name: &str, oid: &str, color: usize) -> GraphTip {
        let mut tip = tip(name, oid, color);
        tip.principal = true;
        tip
    }

    fn connected(rows: &[GraphRow]) -> bool {
        rows.windows(2).all(|pair| {
            let bottom: BTreeSet<_> = pair[0]
                .traces
                .iter()
                .filter_map(|trace| trace.to_lane)
                .collect();
            let top: BTreeSet<_> = pair[1]
                .traces
                .iter()
                .filter_map(|trace| trace.from_lane)
                .collect();
            bottom == top
        })
    }

    #[test]
    fn parses_parents_and_keeps_a_separator_in_the_summary() {
        let mut stdout = record(OID_A, &format!("{OID_B} {OID_C}"), b"merge\x1f topic");
        stdout.extend(record(OID_B, "", b"root"));
        let commits = parse_graph_log(&stdout).expect("parsable");
        assert_eq!(
            commits,
            vec![
                GraphCommit {
                    oid: OID_A.to_owned(),
                    summary: "merge\u{1f} topic".to_owned(),
                    parents: vec![OID_B.to_owned(), OID_C.to_owned()],
                },
                GraphCommit {
                    oid: OID_B.to_owned(),
                    summary: "root".to_owned(),
                    parents: Vec::new(),
                },
            ]
        );
    }

    #[test]
    fn malformed_graph_records_are_rejected() {
        assert_eq!(parse_graph_log(b""), Some(Vec::new()));
        assert_eq!(parse_graph_log(b"no separator\0"), None);
        assert_eq!(parse_graph_log(&record("not-hex", "", b"x")), None);
        assert_eq!(parse_graph_log(&record(OID_A, "not-a-hash", b"x")), None);
    }

    #[test]
    fn linear_history_is_one_lane() {
        let commits = vec![
            commit("a", "third", &["b"]),
            commit("b", "second", &["c"]),
            commit("c", "first", &[]),
        ];
        let graph = layout_graph(&commits, &[spine("main", "a", 0)]);
        assert!(connected(&graph.rows));
        assert_eq!(graph.columns, 1);
        assert!(graph.rows.iter().all(|row| row.commit_lane == 0));
        assert_eq!(graph.rows[0].branches[0].name, "main");
        assert_eq!(graph.rows[0].commit_color, 0);
        assert!(graph.rows[1].branches.is_empty());
        assert_eq!(
            graph.rows[0].traces,
            vec![GraphTrace {
                from_lane: None,
                to_lane: Some(0),
                color: 0,
            }]
        );
        assert_eq!(
            graph.rows[1].traces,
            vec![GraphTrace {
                from_lane: Some(0),
                to_lane: Some(0),
                color: 0,
            }]
        );
        assert_eq!(
            graph.rows[2].traces,
            vec![GraphTrace {
                from_lane: Some(0),
                to_lane: None,
                color: 0,
            }]
        );
    }

    #[test]
    fn diverging_branches_meet_at_their_parent() {
        // main is newer, so it is drawn first and keeps the left lane.
        let commits = vec![
            commit("main", "on main", &["base"]),
            commit("feature", "on feature", &["base"]),
            commit("base", "base", &[]),
        ];
        let graph = layout_graph(
            &commits,
            &[spine("main", "main", 0), tip("feature", "feature", 3)],
        );
        assert!(connected(&graph.rows));
        assert_eq!(graph.columns, 2);

        let main = &graph.rows[0];
        assert_eq!(main.commit_lane, 0);
        assert_eq!(main.commit_color, 0);
        assert_eq!(main.branches[0].name, "main");

        let feature = &graph.rows[1];
        assert_eq!(feature.commit_lane, 1);
        assert_eq!(feature.commit_color, 3);
        assert!(feature.traces.iter().any(|trace| {
            trace.from_lane.is_none() && trace.to_lane == Some(0) && trace.color == 3
        }));

        let base = &graph.rows[2];
        assert_eq!(base.summary, "base");
        assert_eq!(base.commit_lane, 0);
        assert!(base.branches.is_empty());
    }

    #[test]
    fn a_merge_opens_a_lane_and_shifts_the_ones_to_its_right() {
        let commits = vec![
            commit("m", "merge", &["a", "b"]),
            commit("a", "left", &["c", "d"]),
            commit("b", "right", &["c"]),
            commit("d", "side", &["c"]),
            commit("c", "base", &[]),
        ];
        let graph = layout_graph(&commits, &[spine("main", "m", 0)]);
        assert!(connected(&graph.rows));
        assert!(graph.columns >= 3);

        let merge = &graph.rows[0];
        assert_eq!(merge.commit_lane, 0);
        assert!(merge.traces.iter().any(|trace| trace.to_lane == Some(0)));
        assert!(merge.traces.iter().any(|trace| trace.to_lane == Some(1)));

        let left = &graph.rows[1];
        assert!(
            left.traces
                .iter()
                .any(|trace| { trace.from_lane == Some(1) && trace.to_lane == Some(2) })
        );

        let right = &graph.rows[2];
        assert_eq!(right.summary, "right");
        assert_eq!(right.commit_lane, 2);
        assert!(
            right
                .traces
                .iter()
                .any(|trace| { trace.from_lane.is_none() && trace.to_lane == Some(0) })
        );
    }

    #[test]
    fn two_branches_on_one_commit_share_the_dot() {
        let graph = layout_graph(
            &[commit("a", "shared", &[])],
            &[spine("main", "a", 0), tip("feature", "a", 4)],
        );
        assert_eq!(graph.columns, 1);
        assert_eq!(graph.rows[0].commit_color, 0);
        assert_eq!(
            graph.rows[0]
                .branches
                .iter()
                .map(|branch| branch.name.as_str())
                .collect::<Vec<_>>(),
            ["main", "feature"]
        );
        assert_eq!(graph.rows[0].branches[1].color, 4);
    }

    #[test]
    fn a_parent_outside_the_list_does_not_leave_a_dangling_lane() {
        let graph = layout_graph(
            &[commit("a", "tip", &["missing"])],
            &[spine("main", "a", 0)],
        );
        assert_eq!(graph.columns, 1);
        assert!(graph.rows[0].traces.is_empty());
    }

    #[test]
    fn a_newer_branch_stays_beside_the_principal_spine() {
        // feature is newer, so it is listed first. main still owns lane 0.
        let commits = vec![
            commit("feature", "on feature", &["base"]),
            commit("main", "on main", &["base"]),
            commit("base", "base", &[]),
        ];
        let graph = layout_graph(
            &commits,
            &[spine("main", "main", 0), tip("feature", "feature", 1)],
        );
        assert!(connected(&graph.rows));
        assert_eq!(graph.columns, 2);
        assert_eq!(graph.rows[0].commit_lane, 1);
        assert_eq!(graph.rows[0].commit_color, 1);
        assert_eq!(graph.rows[1].commit_lane, 0);
        assert_eq!(graph.rows[1].commit_color, 0);

        let base = &graph.rows[2];
        assert_eq!(base.commit_lane, 0);
        assert_eq!(base.commit_color, 0);
        assert!(base.traces.iter().any(|trace| {
            trace.from_lane == Some(1) && trace.to_lane.is_none() && trace.color == 1
        }));
    }

    #[test]
    fn a_merged_branch_returns_to_the_principal_lane() {
        let commits = vec![
            commit("merge", "merge", &["mainline", "feature"]),
            commit("mainline", "on main", &["base"]),
            commit("feature", "on feature", &["base"]),
            commit("base", "base", &[]),
        ];
        let graph = layout_graph(
            &commits,
            &[spine("main", "merge", 0), tip("feature", "feature", 1)],
        );
        assert!(connected(&graph.rows));
        assert_eq!(graph.rows[0].commit_lane, 0);
        assert_eq!(graph.rows[0].commit_color, 0);

        let feature = graph
            .rows
            .iter()
            .find(|row| row.summary == "on feature")
            .expect("feature");
        assert_eq!(feature.commit_lane, 1);
        assert_eq!(feature.commit_color, 1);
        assert!(feature.traces.iter().any(|trace| {
            trace.from_lane.is_none() && trace.to_lane == Some(0) && trace.color == 1
        }));

        let base = graph
            .rows
            .iter()
            .find(|row| row.summary == "base")
            .expect("base");
        assert_eq!(base.commit_lane, 0);
        assert_eq!(base.commit_color, 0);
    }

    #[test]
    fn two_side_branches_join_the_same_principal_commit() {
        let commits = vec![
            commit("feature", "feature", &["base"]),
            commit("topic", "topic", &["base"]),
            commit("main", "main", &["base"]),
            commit("base", "base", &[]),
        ];
        let graph = layout_graph(
            &commits,
            &[
                spine("main", "main", 0),
                tip("feature", "feature", 1),
                tip("topic", "topic", 2),
            ],
        );
        assert!(connected(&graph.rows));
        assert_eq!(row_named(&graph, "main").commit_lane, 0);
        assert_eq!(row_named(&graph, "feature").commit_lane, 1);
        assert_eq!(row_named(&graph, "topic").commit_lane, 2);
        let base = row_named(&graph, "base");
        assert_eq!(base.commit_color, 0);
        assert!(
            base.traces
                .iter()
                .any(|trace| trace.from_lane == Some(1) && trace.to_lane.is_none())
        );
        assert!(
            base.traces
                .iter()
                .any(|trace| trace.from_lane == Some(2) && trace.to_lane.is_none())
        );
    }

    #[test]
    fn a_branch_already_on_the_spine_only_adds_its_label() {
        let commits = vec![
            commit("main", "tip", &["old"]),
            commit("old", "old", &["base"]),
            commit("base", "base", &[]),
        ];
        let graph = layout_graph(&commits, &[spine("main", "main", 0), tip("old", "old", 2)]);
        assert!(connected(&graph.rows));
        assert_eq!(graph.columns, 1);
        let old = row_named(&graph, "old");
        assert_eq!(old.commit_lane, 0);
        assert_eq!(old.commit_color, 0);
        assert_eq!(old.branches[0].name, "old");
        assert_eq!(old.branches[0].color, 2);
    }

    #[test]
    fn unrelated_histories_do_not_meet() {
        let commits = vec![
            commit("other", "other tip", &["other-base"]),
            commit("main", "main tip", &["main-base"]),
            commit("other-base", "other base", &[]),
            commit("main-base", "main base", &[]),
        ];
        let graph = layout_graph(
            &commits,
            &[spine("main", "main", 0), tip("other", "other", 1)],
        );
        assert!(connected(&graph.rows));
        assert_eq!(row_named(&graph, "main tip").commit_lane, 0);
        assert_eq!(row_named(&graph, "other tip").commit_lane, 1);
        assert!(graph.rows.iter().all(|row| {
            row.traces
                .iter()
                .all(|trace| match (trace.from_lane, trace.to_lane) {
                    (Some(from), Some(to)) => from == to,
                    (Some(from), None) => from == row.commit_lane,
                    _ => true,
                })
        }));
    }

    fn row_named<'a>(graph: &'a HistoryGraph, summary: &str) -> &'a GraphRow {
        graph
            .rows
            .iter()
            .find(|row| row.summary == summary)
            .unwrap_or_else(|| panic!("missing {summary}"))
    }

    fn choice<'a>(
        name: &'a str,
        oid: &'a str,
        current: bool,
        when: Option<i64>,
        checked: bool,
    ) -> GraphCandidate<'a> {
        GraphCandidate {
            name,
            oid: Some(oid),
            current,
            committer_time: when,
            checked,
        }
    }

    #[test]
    fn the_checked_out_branch_is_principal_even_when_it_is_older() {
        let branches = [
            choice("feature", OID_A, true, Some(1), true),
            choice("main", OID_B, false, Some(50), true),
            choice("topic", OID_C, false, Some(40), true),
        ];
        let tips = graph_tips(&branches, None);
        let principal = tips.iter().find(|tip| tip.principal).expect("principal");
        assert_eq!(principal.name, "feature");
        assert_eq!(principal.color, 0);
        assert_eq!(tips.iter().find(|tip| tip.name == "main").unwrap().color, 1);
        assert_eq!(
            tips.iter().find(|tip| tip.name == "topic").unwrap().color,
            2
        );
    }

    #[test]
    fn unchecking_a_branch_does_not_recolor_the_ones_still_shown() {
        let shown = [
            choice("feature", OID_A, true, Some(1), true),
            choice("main", OID_B, false, Some(50), true),
            choice("topic", OID_C, false, Some(40), true),
        ];
        let hidden = [
            choice("feature", OID_A, true, Some(1), true),
            choice("main", OID_B, false, Some(50), false),
            choice("topic", OID_C, false, Some(40), true),
        ];
        let before = graph_tips(&shown, None);
        let after = graph_tips(&hidden, None);
        assert_eq!(
            before.iter().find(|tip| tip.name == "topic").unwrap().color,
            after.iter().find(|tip| tip.name == "topic").unwrap().color
        );
        assert_eq!(
            after.iter().find(|tip| tip.name == "topic").unwrap().color,
            2
        );
        assert!(after.iter().all(|tip| tip.name != "main"));
    }

    #[test]
    fn an_unchecked_checkout_falls_back_to_main_then_master_then_trunk() {
        let on_feature = [
            choice("feature", OID_A, true, Some(90), false),
            choice("topic", OID_B, false, Some(80), true),
            choice("main", OID_C, false, Some(1), true),
        ];
        assert_eq!(principal_name(&graph_tips(&on_feature, None)), "main");

        let master = [
            choice("feature", OID_A, false, Some(90), true),
            choice("master", OID_B, false, Some(1), true),
        ];
        assert_eq!(principal_name(&graph_tips(&master, None)), "master");

        let trunk = [
            choice("feature", OID_A, false, Some(90), true),
            choice("trunk", OID_B, false, Some(1), true),
        ];
        assert_eq!(principal_name(&graph_tips(&trunk, None)), "trunk");
    }

    #[test]
    fn without_a_default_name_the_newest_tip_is_principal() {
        let branches = [
            choice("old", OID_A, false, Some(10), true),
            choice("new", OID_B, false, Some(40), true),
            choice("mid", OID_C, false, None, true),
        ];
        let tips = graph_tips(&branches, None);
        assert_eq!(principal_name(&tips), "new");
        assert_eq!(tips.iter().find(|tip| tip.principal).unwrap().color, 0);
    }

    #[test]
    fn equal_committer_times_keep_the_earlier_sidebar_row() {
        let branches = [
            choice("beta", OID_A, false, Some(10), true),
            choice("alpha", OID_B, false, Some(10), true),
        ];
        assert_eq!(principal_name(&graph_tips(&branches, None)), "beta");
    }

    #[test]
    fn detached_head_is_the_principal_lane() {
        let branches = [choice("main", OID_A, false, Some(10), true)];
        let tips = graph_tips(&branches, Some(OID_B));
        let head = tips.iter().find(|tip| tip.name == "HEAD").expect("head");
        assert!(head.principal);
        assert_eq!(head.color, 0);
        assert_eq!(head.oid, OID_B);
        let main = tips.iter().find(|tip| tip.name == "main").unwrap();
        assert!(!main.principal);
        assert_eq!(main.color, 1);
    }

    #[test]
    fn unchecked_branches_and_branches_without_commits_are_left_out() {
        let branches = [
            GraphCandidate {
                name: "main",
                oid: None,
                current: true,
                committer_time: None,
                checked: true,
            },
            choice("feature", OID_A, false, Some(10), false),
        ];
        assert!(graph_tips(&branches, None).is_empty());
    }

    #[test]
    fn a_late_sidebar_row_does_not_reuse_the_principal_color() {
        let names = ["b0", "b1", "b2", "b3", "b4", "b5", "b6", "b7", "b8"];
        let oids = [
            OID_A,
            OID_B,
            OID_C,
            "1111111111111111111111111111111111111111",
            "2222222222222222222222222222222222222222",
            "3333333333333333333333333333333333333333",
            "4444444444444444444444444444444444444444",
            "5555555555555555555555555555555555555555",
            "6666666666666666666666666666666666666666",
        ];
        let branches: Vec<GraphCandidate<'_>> = names
            .iter()
            .zip(oids)
            .enumerate()
            .map(|(index, (name, oid))| {
                choice(
                    name,
                    oid,
                    index == 0,
                    Some(index as i64),
                    index == 0 || index == 8,
                )
            })
            .collect();
        let tips = graph_tips(&branches, None);
        let late = tips.iter().find(|tip| tip.name == "b8").unwrap();
        assert!(!late.color.is_multiple_of(LANE_COLORS));
    }

    fn principal_name(tips: &[GraphTip]) -> &str {
        tips.iter()
            .find(|tip| tip.principal)
            .map(|tip| tip.name.as_str())
            .expect("principal")
    }
}
