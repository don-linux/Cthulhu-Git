//! Fixtures shared by the UI tests.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use cthulhu_git::git::{
    Branch, CommitDetail, Git, GitVersion, GraphCommit, GraphTip, Head, Person, RepoInfo, Upstream,
    layout_graph,
};
use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::NodeT;

use super::{ListedBranch, OpenedRepo, changes, hover};

/// A git that is installed nowhere. Every command it runs fails at once to
/// start, so a fixture repository never reaches the real git.
pub fn absent_git() -> Git {
    Git {
        path: PathBuf::from("/nonexistent/cthulhu-test-git"),
        version: GitVersion::parse("git version 2.43.0").expect("version"),
        command_timeout: Duration::from_secs(5),
    }
}

/// The commit every fixture branch points at.
pub const TIP: &str = "0123456789abcdef0123456789abcdef01234567";

/// A local branch at [`TIP`]. Only the current branch starts checked, as on open.
pub fn listed(name: &str, current: bool) -> ListedBranch {
    ListedBranch {
        branch: Branch {
            name: name.to_owned(),
            oid: Some(TIP.to_owned()),
            committer_time: Some(1_700_000_000),
            commit_count: 1,
            upstream: Upstream::None,
            current,
        },
        checked: current,
    }
}

/// A root commit at [`TIP`] by cthulhu.
pub fn commit(summary: &str) -> GraphCommit {
    GraphCommit {
        oid: TIP.to_owned(),
        summary: summary.to_owned(),
        parents: Vec::new(),
        author: Person::new("cthulhu", "cthulhu@example.invalid"),
        co_authors: Vec::new(),
    }
}

/// Repository `name` on `main`, whose history is `commit` alone, already drawn.
/// Its root does not exist and its git is [`absent_git`], so nothing it starts
/// reaches a real repository.
pub fn opened(name: &str, commit: GraphCommit) -> OpenedRepo {
    let tips = vec![GraphTip {
        name: "main".to_owned(),
        oid: commit.oid.clone(),
        color: 0,
        principal: true,
        current: true,
    }];
    let latest = CommitDetail {
        oid: commit.oid.clone(),
        author: commit.author.clone(),
        co_authors: commit.co_authors.clone(),
        authored_at: "2026-01-01 00:00".to_owned(),
        message: commit.summary.clone(),
    };
    let mut main = listed("main", true);
    main.branch.oid = Some(commit.oid.clone());
    OpenedRepo {
        git: absent_git(),
        info: RepoInfo {
            root: PathBuf::from("/repos").join(name),
            name: name.to_owned(),
            head: Head::Branch("main".to_owned()),
        },
        graph: layout_graph(&[commit], &tips),
        graph_selection: tips.clone(),
        graph_requested: tips,
        detached_oid: None,
        branches: vec![main],
        latest: Some(latest),
        view_id: egui::Id::new(("test-repo", name)),
        terminal: None,
        github: None,
        hover: hover::HoverCache::default(),
        diffs: changes::DiffCache::default(),
        changes: None,
    }
}

/// Every label, value, description, link and placeholder in the
/// accessibility tree, one per line.
pub fn tree_text<State>(harness: &Harness<'_, State>) -> String {
    let mut lines = Vec::new();
    let mut add = |node: &egui_kittest::Node<'_>| {
        let access = node.accesskit_node();
        lines.extend(access.label());
        lines.extend(access.value());
        lines.extend(access.description());
        lines.extend(access.url().map(str::to_owned));
        lines.extend(access.placeholder().map(str::to_owned));
    };
    let root = harness.root();
    add(&root);
    for node in root.children_recursive() {
        add(&node);
    }
    lines.join("\n")
}

/// Every painted text and its clipped bounds.
pub fn painted_texts<State>(harness: &Harness<'_, State>) -> Vec<(egui::Rect, Arc<egui::Galley>)> {
    fn walk(
        clip: egui::Rect,
        shape: &egui::Shape,
        found: &mut Vec<(egui::Rect, Arc<egui::Galley>)>,
    ) {
        match shape {
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    walk(clip, shape, found);
                }
            }
            egui::Shape::Text(text) => {
                found.push((
                    text.visual_bounding_rect().intersect(clip),
                    Arc::clone(&text.galley),
                ));
            }
            _ => {}
        }
    }
    let mut found = Vec::new();
    for clipped in &harness.output().shapes {
        walk(clipped.clip_rect, &clipped.shape, &mut found);
    }
    found
}
