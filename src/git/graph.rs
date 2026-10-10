//! Commit graph for the branches the user has switched on.
//!
//! `git log --date-order` lists children before their parents. [`layout_graph`]
//! turns that list into lanes: a branch tip opens a lane, the first parent
//! keeps it, and another parent opens a lane to the right. When a parent is
//! already expected in some other lane, the two lanes meet on that commit.

use std::collections::HashSet;
use std::path::Path;

use super::exec::{Git, GitError};

const FIELD_SEPARATOR: u8 = 0x1f;

/// A branch tip to include in the graph. `color` comes from the sidebar order
/// so a branch keeps its color when a different one is switched off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphTip {
    pub name: String,
    pub oid: String,
    pub color: usize,
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
    next: String,
    color: usize,
}

struct BottomLane {
    next: String,
    color: usize,
    from: Option<usize>,
    continues_commit: bool,
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
pub fn layout_graph(commits: &[GraphCommit], tips: &[GraphTip]) -> HistoryGraph {
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
        rows.push(layout_row(
            commit,
            tips,
            &remaining,
            &mut lanes,
            &mut extra_color,
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

fn layout_row(
    commit: &GraphCommit,
    tips: &[GraphTip],
    remaining: &HashSet<String>,
    lanes: &mut Vec<Lane>,
    extra_color: &mut usize,
) -> GraphRow {
    let spawned = !lanes.iter().any(|lane| lane.next == commit.oid);
    if spawned {
        let color = tip_color(tips, &commit.oid).unwrap_or_else(|| take_color(extra_color));
        lanes.push(Lane {
            next: commit.oid.clone(),
            color,
        });
    }
    let lane_idx = lanes
        .iter()
        .position(|lane| lane.next == commit.oid)
        .expect("the commit occupies a lane");
    let commit_color = lanes[lane_idx].color;
    let top = lanes.clone();

    let parents = kept_parents(&commit.parents, remaining);
    let first = parents.first();
    let merges_into = first.and_then(|parent| top.iter().position(|lane| lane.next == *parent));

    let mut seen = HashSet::new();
    if let Some(parent) = first {
        seen.insert(parent.clone());
    }
    let mut extras = Vec::new();
    for parent in parents.iter().skip(1) {
        if !seen.insert(parent.clone()) {
            continue;
        }
        let into_top = top.iter().position(|lane| lane.next == *parent);
        if into_top.is_some() && into_top == merges_into {
            continue;
        }
        let color = tip_color(tips, parent).unwrap_or_else(|| take_color(extra_color));
        extras.push(ExtraParent {
            parent: parent.clone(),
            color,
            into_top,
        });
    }

    let mut bottom = Vec::new();
    for (index, lane) in top.iter().enumerate() {
        if index == lane_idx {
            if merges_into.is_none()
                && let Some(parent) = first
            {
                bottom.push(BottomLane {
                    next: parent.clone(),
                    color: commit_color,
                    from: Some(index),
                    continues_commit: true,
                });
            }
            for extra in &extras {
                if extra.into_top.is_none() {
                    bottom.push(BottomLane {
                        next: extra.parent.clone(),
                        color: extra.color,
                        from: None,
                        continues_commit: false,
                    });
                }
            }
        } else {
            bottom.push(BottomLane {
                next: lane.next.clone(),
                color: lane.color,
                from: Some(index),
                continues_commit: false,
            });
        }
    }

    let mut traces = Vec::new();
    let mut top_to_bottom = vec![None; top.len()];
    for (index, lane) in bottom.iter().enumerate() {
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

    *lanes = bottom
        .into_iter()
        .map(|lane| Lane {
            next: lane.next,
            color: lane.color,
        })
        .collect();

    GraphRow {
        oid: commit.oid.clone(),
        summary: commit.summary.clone(),
        branches: tip_labels(tips, &commit.oid),
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
        }
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
        let graph = layout_graph(&commits, &[tip("main", "a", 0)]);
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
            &[tip("main", "main", 0), tip("feature", "feature", 3)],
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
        let graph = layout_graph(&commits, &[tip("main", "m", 0)]);
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
            &[tip("main", "a", 0), tip("feature", "a", 4)],
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
        let graph = layout_graph(&[commit("a", "tip", &["missing"])], &[tip("main", "a", 0)]);
        assert_eq!(graph.columns, 1);
        assert!(graph.rows[0].traces.is_empty());
    }
}
