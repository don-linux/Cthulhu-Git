//! Kittest coverage for the commit-history table: title order and alignment,
//! horizontal scroll, divider drag, double-click reset, virtualization,
//! cells that must stay inside their columns, and the opening line while
//! history is still loading.

use cthulhu_git::git::{
    Branch, Commit, CommitDetail, GraphBranch, GraphCommit, GraphRow, GraphTip, Head, HistoryGraph,
    Person, RepoInfo, SHORT_OID_LEN, Upstream, layout_graph,
};
use cthulhu_git::settings::HistoryColumns;
use eframe::egui::accesskit::Role;
use eframe::egui::{self, Modifiers, MouseWheelUnit, PointerButton, Pos2, TouchPhase};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use super::avatars::AvatarService;
use super::hover::HoverCache;
use super::repo_view::{self, ViewState};
use super::theme;
use super::{Action, ListedBranch, OpenedRepo};

const PIXELS_PER_POINT: f32 = 1.0;
const HEIGHT: f32 = 640.0;
/// The app's minimum inner size, and the window the narrow-history kittest uses.
const MIN_WINDOW: egui::Vec2 = egui::Vec2::new(520.0, 400.0);
/// Titles and cells are aligned when their left edges differ by at most a point.
const TOL: f32 = 1.0;
const TITLES: [&str; 5] = ["Branch", "Graph", "Commit Hash", "Commit Message", "Author"];

struct Screen {
    repo: OpenedRepo,
    columns: HistoryColumns,
    branches_open: bool,
    detail_open: bool,
    /// This repository's history graph is still being read.
    history_loading: bool,
    actions: Vec<Action>,
}

struct RowText<'a> {
    summary: &'a str,
    short_oid: &'a str,
    author: &'a str,
    branch: &'a str,
}

struct Edge {
    title: &'static str,
    title_left: f32,
    content_left: f32,
}

fn screen(
    repo: OpenedRepo,
    columns: HistoryColumns,
    branches_open: bool,
    detail_open: bool,
) -> Screen {
    Screen {
        repo,
        columns,
        branches_open,
        detail_open,
        history_loading: false,
        actions: Vec::new(),
    }
}

fn harness(size: egui::Vec2, screen: Screen) -> Harness<'static, Screen> {
    Harness::builder()
        .with_size(size)
        .with_pixels_per_point(PIXELS_PER_POINT)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, screen| {
                theme::apply(ui.ctx(), theme::DEFAULT_THEME);
                let mut avatars = AvatarService::disabled();
                let produced = repo_view::show(
                    ui,
                    &mut screen.repo,
                    &ViewState {
                        error: None,
                        branches_open: screen.branches_open,
                        detail_open: screen.detail_open,
                        terminal_open: false,
                        syncing: None,
                        columns: screen.columns,
                        history_loading: screen.history_loading,
                    },
                    &mut avatars,
                );
                for action in &produced {
                    if let Action::SetHistoryColumns(columns) = action {
                        screen.columns = *columns;
                    }
                }
                screen.actions.extend(produced);
            },
            screen,
        )
}

fn person(name: &str, email: &str) -> Person {
    Person::new(name, email)
}

fn short_oid(oid: &str) -> &str {
    &oid[..oid.len().min(SHORT_OID_LEN)]
}

fn tip(oid: &str) -> GraphTip {
    GraphTip {
        name: "main".to_owned(),
        oid: oid.to_owned(),
        color: 0,
        principal: true,
        current: true,
    }
}

fn opened(name: &str, commits: &[Commit], truncated: bool) -> OpenedRepo {
    let author = person("Ada Lovelace", "ada@example.invalid");
    let mut graph = linear_graph(commits, &author);
    graph.truncated = truncated;
    let oid = commits.first().map(|commit| commit.oid.clone());
    let tips = oid.iter().map(|oid| tip(oid)).collect::<Vec<_>>();
    OpenedRepo {
        info: RepoInfo {
            root: std::path::PathBuf::from("/repos").join(name),
            name: name.to_owned(),
            head: Head::Branch("main".to_owned()),
        },
        graph,
        graph_selection: tips.clone(),
        graph_requested: tips,
        detached_oid: None,
        branches: vec![ListedBranch {
            branch: Branch {
                name: "main".to_owned(),
                oid,
                committer_time: Some(1_700_000_000),
                commit_count: commits.len() as u64,
                upstream: Upstream::None,
                current: true,
            },
            checked: true,
        }],
        latest: commits.first().map(|commit| CommitDetail {
            oid: commit.oid.clone(),
            author: author.clone(),
            co_authors: Vec::new(),
            authored_at: "2026-01-01 00:00".to_owned(),
            message: commit.summary.clone(),
        }),
        view_id: egui::Id::new(("agent-history", name)),
        terminal: None,
        github: None,
        hover: HoverCache::default(),
    }
}

fn opened_graph(name: &str, graph: HistoryGraph) -> OpenedRepo {
    let mut repo = opened(
        name,
        &[Commit {
            oid: graph
                .rows
                .first()
                .map(|row| row.oid.clone())
                .unwrap_or_else(|| "0".repeat(40)),
            summary: "placeholder".to_owned(),
        }],
        false,
    );
    if let Some(row) = graph.rows.first() {
        repo.latest = Some(CommitDetail {
            oid: row.oid.clone(),
            author: row.author.clone(),
            co_authors: row.co_authors.clone(),
            authored_at: "2026-01-01 00:00".to_owned(),
            message: row.summary.clone(),
        });
    }
    repo.graph = graph;
    repo
}

/// One lane, each commit the parent of the one above it, `main` on the newest.
fn linear_graph(commits: &[Commit], author: &Person) -> HistoryGraph {
    let inputs: Vec<GraphCommit> = commits
        .iter()
        .enumerate()
        .map(|(index, commit)| GraphCommit {
            oid: commit.oid.clone(),
            summary: commit.summary.clone(),
            parents: commits
                .get(index + 1)
                .map(|parent| vec![parent.oid.clone()])
                .unwrap_or_default(),
            author: author.clone(),
            co_authors: Vec::new(),
        })
        .collect();
    let tips = commits
        .first()
        .map(|commit| tip(&commit.oid))
        .into_iter()
        .collect::<Vec<_>>();
    layout_graph(&inputs, &tips)
}

/// Branch list and latest commit are already known; the graph has no rows.
///
/// Opening a repository installs the current branch, which already has
/// commits, before the history worker fills the table. With
/// `history_loading` the empty table is still opening. Without it, the same
/// empty table is "No commits yet."
fn opened_while_history_loads(name: &str) -> OpenedRepo {
    let mut repo = opened(
        name,
        &[Commit {
            oid: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            summary: "field notes from the estuary".to_owned(),
        }],
        false,
    );
    repo.graph = HistoryGraph::default();
    repo
}

fn one_commit(summary: &str) -> OpenedRepo {
    opened(
        summary,
        &[Commit {
            oid: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            summary: summary.to_owned(),
        }],
        false,
    )
}

fn row_text(repo: &OpenedRepo) -> (String, String, String, String) {
    let row = &repo.graph.rows[0];
    let branch = row
        .branches
        .iter()
        .map(|branch| branch.name.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let author = std::iter::once(row.author.name.as_str())
        .chain(row.co_authors.iter().map(|person| person.name.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    (
        row.summary.clone(),
        short_oid(&row.oid).to_owned(),
        author,
        branch,
    )
}

fn message_min() -> f32 {
    super::history::column_mins(0.0, 1)[super::history::Column::Message as usize]
}

fn label_rects(harness: &Harness<'_, Screen>, exact: &str) -> Vec<egui::Rect> {
    harness
        .query_all_by_role(Role::Label)
        .filter(|node| node.value().as_deref() == Some(exact))
        .map(|node| node.rect())
        .collect()
}

fn brief(value: &str) -> String {
    let mut chars = value.chars();
    let short: String = chars.by_ref().take(48).collect();
    if chars.next().is_some() {
        format!("{short}…")
    } else {
        short
    }
}

fn labels(harness: &Harness<'_, Screen>) -> String {
    let mut lines: Vec<_> = harness
        .query_all_by_role(Role::Label)
        .map(|node| {
            let rect = node.rect();
            format!(
                "{:?} @ ({:.1}, {:.1})",
                brief(&node.value().unwrap_or_default()),
                rect.left(),
                rect.center().y
            )
        })
        .collect();
    let total = lines.len();
    lines.truncate(30);
    if total > lines.len() {
        lines.push(format!("… {total} labels"));
    }
    lines.join("\n")
}

fn header_row(harness: &Harness<'_, Screen>) -> Vec<(&'static str, egui::Rect)> {
    // "Author" is also the detail sidebar's caption. Every other title is
    // unique, so its row identifies the history header.
    let anchor_y = TITLES.iter().find_map(|title| {
        if *title == "Author" {
            return None;
        }
        label_rects(harness, title)
            .into_iter()
            .next()
            .map(|rect| rect.center().y)
    });
    let Some(anchor_y) = anchor_y else {
        return Vec::new();
    };
    TITLES
        .into_iter()
        .filter_map(|title| {
            label_rects(harness, title)
                .into_iter()
                .find(|rect| (rect.center().y - anchor_y).abs() <= 6.0)
                .map(|rect| (title, rect))
        })
        .collect()
}

fn header_names(headers: &[(&'static str, egui::Rect)]) -> String {
    headers
        .iter()
        .map(|(name, rect)| format!("{name}@{:.1}", rect.left()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn column_text<'a>(title: &str, row: &RowText<'a>) -> Option<&'a str> {
    match title {
        "Branch" => Some(row.branch),
        "Commit Hash" => Some(row.short_oid),
        "Commit Message" => Some(row.summary),
        "Author" => Some(row.author),
        _ => None,
    }
}

fn closest_x(rects: &[egui::Rect], title_left: f32) -> Option<egui::Rect> {
    rects.iter().copied().min_by(|left, right| {
        (left.left() - title_left)
            .abs()
            .total_cmp(&(right.left() - title_left).abs())
    })
}

fn content_rect(
    harness: &Harness<'_, Screen>,
    text: &str,
    title: egui::Rect,
    row_y: Option<f32>,
) -> egui::Rect {
    let rects = label_rects(harness, text);
    let filtered: Vec<_> = match row_y {
        Some(row_y) => rects
            .into_iter()
            .filter(|rect| (rect.center().y - row_y).abs() <= 22.0)
            .collect(),
        None => rects
            .into_iter()
            .filter(|rect| rect.center().y >= title.bottom() - 2.0)
            .collect(),
    };
    closest_x(&filtered, title.left()).unwrap_or_else(|| {
        panic!(
            "no cell {text:?} for a title at ({:.1}, {:.1})\n{}",
            title.left(),
            title.center().y,
            labels(harness)
        )
    })
}

/// Visible titles, in canonical order, and the left edge of each text cell.
/// Graph has no text label; it is ordered between the branch chip and the hash.
fn aligned_edges(
    harness: &Harness<'_, Screen>,
    case: &str,
    row: &RowText<'_>,
    prefix: bool,
    author_off_screen: bool,
) -> Vec<Edge> {
    let headers = header_row(harness);
    assert!(
        !headers.is_empty(),
        "{case}: no history titles\n{}",
        labels(harness)
    );
    let names: Vec<_> = headers.iter().map(|(name, _)| *name).collect();
    let mut previous = -1isize;
    for name in &names {
        let index = TITLES.iter().position(|title| title == name).unwrap() as isize;
        assert!(
            index > previous,
            "{case}: titles out of order: {}",
            header_names(&headers)
        );
        previous = index;
    }
    if prefix {
        let end = TITLES
            .iter()
            .position(|title| *title == names[names.len() - 1])
            .unwrap();
        assert_eq!(
            names.as_slice(),
            &TITLES[..=end],
            "{case}: visible titles should be a left-to-right prefix ({})",
            header_names(&headers)
        );
    }
    if author_off_screen {
        assert!(
            !names.contains(&"Author"),
            "{case}: Author should sit past the right edge, visible {}",
            header_names(&headers)
        );
    }
    for pair in headers.windows(2) {
        assert!(
            pair[0].1.left() < pair[1].1.left(),
            "{case}: {} is not left of {} ({})",
            pair[0].0,
            pair[1].0,
            header_names(&headers)
        );
    }

    let mut row_y = None;
    let mut edges = Vec::new();
    for (name, title) in &headers {
        let Some(text) = column_text(name, row) else {
            continue;
        };
        let content = content_rect(harness, text, *title, row_y);
        row_y.get_or_insert(content.center().y);
        assert!(
            (title.left() - content.left()).abs() <= TOL,
            "{case}: {name} title starts at {:.2} but its cell at {:.2}",
            title.left(),
            content.left()
        );
        assert!(
            title.bottom() <= content.top() + TOL,
            "{case}: {name} title bottom {:.2} should sit above its cell {:.2}",
            title.bottom(),
            content.top()
        );
        edges.push(Edge {
            title: name,
            title_left: title.left(),
            content_left: content.left(),
        });
    }

    if let (Some(branch), Some(graph), Some(hash)) = (
        headers.iter().find(|(name, _)| *name == "Branch"),
        headers.iter().find(|(name, _)| *name == "Graph"),
        headers.iter().find(|(name, _)| *name == "Commit Hash"),
    ) {
        let chip = content_rect(harness, row.branch, branch.1, row_y);
        let hash_cell = content_rect(harness, row.short_oid, hash.1, row_y);
        let graph_column_left = graph.1.left() - super::history::CELL_PAD;
        assert!(
            chip.right() <= graph_column_left + TOL,
            "{case}: branch chip right {:.2} crosses the graph column at {graph_column_left:.2}",
            chip.right()
        );
        assert!(
            graph.1.left() < hash_cell.left(),
            "{case}: graph title {:.2} is not left of the hash {:.2}",
            graph.1.left(),
            hash_cell.left()
        );
    }
    edges
}

fn pointer_over_history(harness: &Harness<'_, Screen>, height: f32) -> Pos2 {
    let headers = header_row(harness);
    let title = headers
        .iter()
        .find(|(name, _)| *name == "Commit Hash" || *name == "Branch" || *name == "Graph")
        .map(|(_, rect)| *rect)
        .unwrap_or_else(|| panic!("no visible column\n{}", labels(harness)));
    // A few points into the title keeps the pointer over the history body
    // and off the sidebars, below the single commit row.
    Pos2::new(title.left() + 8.0, height - 120.0)
}

/// Horizontal wheel. `Start` then `Move` makes egui apply `delta_x` on this
/// frame instead of smoothing a notch over later frames. Negative `delta_x`
/// scrolls the table to the right.
fn wheel_x(harness: &mut Harness<'_, Screen>, at: Pos2, delta_x: f32) {
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(at));
    harness.input_mut().events.push(egui::Event::MouseWheel {
        unit: MouseWheelUnit::Point,
        delta: egui::Vec2::ZERO,
        phase: TouchPhase::Start,
        modifiers: Modifiers::default(),
    });
    harness.input_mut().events.push(egui::Event::MouseWheel {
        unit: MouseWheelUnit::Point,
        delta: egui::vec2(delta_x, 0.0),
        phase: TouchPhase::Move,
        modifiers: Modifiers::default(),
    });
    harness.step();
}

fn move_pointer(harness: &mut Harness<'_, Screen>, at: Pos2) {
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(at));
}

fn set_button(harness: &mut Harness<'_, Screen>, at: Pos2, pressed: bool) {
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos: at,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::default(),
    });
}

fn drag(harness: &mut Harness<'_, Screen>, from: Pos2, to: Pos2) {
    move_pointer(harness, from);
    harness.step();
    set_button(harness, from, true);
    harness.step();
    for step in 1..=4 {
        let t = step as f32 / 4.0;
        move_pointer(harness, from + (to - from) * t);
        harness.step();
    }
    set_button(harness, to, false);
    harness.step();
    // `history::show` reads the divider's response from the previous frame,
    // so the release is turned into `SetHistoryColumns` one frame later.
    harness.step();
}

fn click_at(harness: &mut Harness<'_, Screen>, at: Pos2) {
    move_pointer(harness, at);
    set_button(harness, at, true);
    set_button(harness, at, false);
    harness.step();
}

fn action_name(action: &Action) -> String {
    match action {
        Action::Browse => "Browse".to_owned(),
        Action::Open(path) => format!("Open({})", path.display()),
        Action::Home => "Home".to_owned(),
        Action::OpenSettings => "OpenSettings".to_owned(),
        Action::CloseSettings => "CloseSettings".to_owned(),
        Action::ToggleBranchesSidebar => "ToggleBranchesSidebar".to_owned(),
        Action::ToggleDetailSidebar => "ToggleDetailSidebar".to_owned(),
        Action::ToggleTerminal => "ToggleTerminal".to_owned(),
        Action::SetTerminalFont(font) => format!("SetTerminalFont({font:?})"),
        Action::EnsureFontCatalog => "EnsureFontCatalog".to_owned(),
        Action::Fetch(upstream) => format!("Fetch({upstream})"),
        Action::Pull => "Pull".to_owned(),
        Action::SetHistoryColumns(columns) => format!("SetHistoryColumns({columns:?})"),
        Action::SetAuthorAvatars(on) => format!("SetAuthorAvatars({on})"),
    }
}

fn take_column_change(harness: &mut Harness<'_, Screen>) -> HistoryColumns {
    let actions = std::mem::take(&mut harness.state_mut().actions);
    let changes: Vec<_> = actions
        .iter()
        .filter_map(|action| match action {
            Action::SetHistoryColumns(columns) => Some(*columns),
            _ => None,
        })
        .collect();
    let described = actions
        .iter()
        .map(action_name)
        .collect::<Vec<_>>()
        .join(", ");
    assert_eq!(
        changes.len(),
        1,
        "expected one SetHistoryColumns, actions: {described}"
    );
    let others: Vec<_> = actions
        .iter()
        .filter(|action| !matches!(action, Action::SetHistoryColumns(_)))
        .map(action_name)
        .collect();
    assert!(
        others.is_empty(),
        "unexpected actions beside SetHistoryColumns: {}",
        others.join(", ")
    );
    changes[0]
}

fn title_left(headers: &[(&'static str, egui::Rect)], name: &str) -> f32 {
    headers
        .iter()
        .find(|(title, _)| *title == name)
        .unwrap_or_else(|| panic!("missing title {name}: {}", header_names(headers)))
        .1
        .left()
}

/// Panel width implied by the default message share. Title gaps are the
/// column widths; the author divider's grab shifts inward on the panel edge.
fn panel_width(headers: &[(&'static str, egui::Rect)]) -> f32 {
    let defaults = HistoryColumns::default();
    let message = title_left(headers, "Author") - title_left(headers, "Commit Message");
    let panel = message * 1000.0 / f32::from(defaults.message);
    let branch = title_left(headers, "Graph") - title_left(headers, "Branch");
    let expected_branch = panel * f32::from(defaults.branch) / 1000.0;
    assert!(
        (branch - expected_branch).abs() <= TOL,
        "branch width {branch:.2} does not match its default share of panel {panel:.2}"
    );
    panel
}

fn assert_left_titles_stayed(
    before: &[(&'static str, egui::Rect)],
    after: &[(&'static str, egui::Rect)],
) {
    for name in ["Branch", "Graph", "Commit Hash", "Commit Message"] {
        let earlier = title_left(before, name);
        let later = title_left(after, name);
        assert!(
            (earlier - later).abs() <= TOL,
            "{name} title moved from {earlier:.2} to {later:.2}"
        );
    }
}

fn assert_scrolled_together(before: &[Edge], after: &[Edge], expect_movement: bool) {
    let mut compared = 0usize;
    for earlier in before {
        let Some(later) = after.iter().find(|edge| edge.title == earlier.title) else {
            continue;
        };
        assert!(
            (later.title_left - later.content_left).abs() <= TOL,
            "{} drifted: title {:.2}, cell {:.2}",
            later.title,
            later.title_left,
            later.content_left
        );
        let title_delta = later.title_left - earlier.title_left;
        let content_delta = later.content_left - earlier.content_left;
        assert!(
            (title_delta - content_delta).abs() <= TOL,
            "{} title moved {:.2} but its cell moved {:.2}",
            earlier.title,
            title_delta,
            content_delta
        );
        if expect_movement {
            assert!(
                title_delta < -TOL,
                "{} should have moved left when the table scrolled right (delta {title_delta:.2})",
                earlier.title
            );
        }
        compared += 1;
    }
    assert!(
        compared >= 1,
        "no column was visible both before and after the scroll"
    );
}

#[test]
fn titles_line_up_with_their_cells_at_several_widths() {
    let summary = "field notes from the estuary";
    for width in [2600.0, 1400.0, 900.0] {
        for branches_open in [false, true] {
            for detail_open in [false, true] {
                let repo = one_commit(summary);
                let (summary, short_oid, author, branch) = row_text(&repo);
                let row = RowText {
                    summary: &summary,
                    short_oid: &short_oid,
                    author: &author,
                    branch: &branch,
                };
                let harness = harness(
                    egui::vec2(width, HEIGHT),
                    screen(repo, HistoryColumns::default(), branches_open, detail_open),
                );
                let case = format!("width {width}, branches {branches_open}, detail {detail_open}");
                let _ = aligned_edges(
                    &harness,
                    &case,
                    &row,
                    true,
                    width == 900.0 && branches_open && detail_open,
                );
            }
        }
    }
}

#[test]
fn titles_stay_aligned_after_the_history_scrolls_right() {
    let summary = "field notes from the estuary";
    let repo = one_commit(summary);
    let (summary, short_oid, author, branch) = row_text(&repo);
    let row = RowText {
        summary: &summary,
        short_oid: &short_oid,
        author: &author,
        branch: &branch,
    };
    // Both sidebars on a 900px window force the minimums past the panel,
    // so the table can actually scroll sideways.
    let mut harness = harness(
        egui::vec2(900.0, HEIGHT),
        screen(repo, HistoryColumns::default(), true, true),
    );
    let before = aligned_edges(&harness, "before scroll", &row, true, true);
    let at = pointer_over_history(&harness, HEIGHT);
    wheel_x(&mut harness, at, -80.0);
    // The wheel is consumed while the rows of this frame are already placed.
    // One more frame paints the rows at the offset the header just adopted.
    move_pointer(&mut harness, Pos2::new(2.0, 2.0));
    harness.step();
    let after = aligned_edges(&harness, "after scroll", &row, false, false);
    assert_scrolled_together(&before, &after, true);
}

#[test]
fn titles_match_cells_on_the_frame_the_wheel_is_applied() {
    let summary = "field notes from the estuary";
    let repo = one_commit(summary);
    let (summary, short_oid, author, branch) = row_text(&repo);
    let row = RowText {
        summary: &summary,
        short_oid: &short_oid,
        author: &author,
        branch: &branch,
    };
    let mut harness = harness(
        egui::vec2(900.0, HEIGHT),
        screen(repo, HistoryColumns::default(), true, true),
    );
    let before = aligned_edges(&harness, "before wheel frame", &row, true, false);
    let at = pointer_over_history(&harness, HEIGHT);
    wheel_x(&mut harness, at, -80.0);
    // The wheel changes the scroll offset after the rows are placed, and the
    // header uses that same placement. Titles and cells must still agree on
    // this frame; the movement shows up on the next one.
    let during = aligned_edges(&harness, "wheel frame", &row, false, false);
    assert_scrolled_together(&before, &during, false);
}

#[test]
fn dragging_the_message_divider_widens_only_that_column() {
    let mut harness = harness(
        egui::vec2(1700.0, HEIGHT),
        screen(
            one_commit("field notes from the estuary"),
            HistoryColumns::default(),
            false,
            false,
        ),
    );
    let before = header_row(&harness);
    let panel = panel_width(&before);
    let from = harness
        .get_by_label("Resize Commit Message")
        .rect()
        .center();
    harness.state_mut().actions.clear();
    drag(&mut harness, from, from + egui::vec2(120.0, 0.0));

    let changed = take_column_change(&mut harness);
    let defaults = HistoryColumns::default();
    assert_eq!(changed.branch, defaults.branch);
    assert_eq!(changed.graph, defaults.graph);
    assert_eq!(changed.hash, defaults.hash);
    assert_eq!(changed.author, defaults.author);
    assert!(
        changed.message > defaults.message,
        "message per-mille {} should grow past {}",
        changed.message,
        defaults.message
    );

    let after = header_row(&harness);
    assert_left_titles_stayed(&before, &after);
    let moved = title_left(&after, "Author") - title_left(&before, "Author");
    let slop = panel / 1000.0 + TOL;
    assert!(
        (moved - 120.0).abs() <= slop,
        "Author moved {moved:.2}px when the divider moved 120px (panel {panel:.1}, slop {slop:.2})"
    );
}

struct Shrink {
    changed: HistoryColumns,
    panel: f32,
    before: Vec<(&'static str, egui::Rect)>,
    after: Vec<(&'static str, egui::Rect)>,
    travel: f32,
}

fn drag_message_below_its_minimum() -> Shrink {
    let mut harness = harness(
        egui::vec2(1700.0, HEIGHT),
        screen(
            one_commit("field notes from the estuary"),
            HistoryColumns::default(),
            false,
            false,
        ),
    );
    let before = header_row(&harness);
    let panel = panel_width(&before);
    let minimum = message_min();
    let from = harness
        .get_by_label("Resize Commit Message")
        .rect()
        .center();
    let column_left = title_left(&before, "Commit Message") - super::history::CELL_PAD;
    let to = Pos2::new(column_left - 80.0, from.y);
    assert!(
        from.x - to.x > minimum + 50.0,
        "the drag should ask for less than the message minimum"
    );
    harness.state_mut().actions.clear();
    drag(&mut harness, from, to);
    let changed = take_column_change(&mut harness);
    let after = header_row(&harness);
    Shrink {
        changed,
        panel,
        before,
        after,
        travel: to.x - from.x,
    }
}

#[test]
fn dragging_below_the_minimum_keeps_the_painted_width() {
    let shrink = drag_message_below_its_minimum();
    let defaults = HistoryColumns::default();
    assert_eq!(shrink.changed.branch, defaults.branch);
    assert_eq!(shrink.changed.graph, defaults.graph);
    assert_eq!(shrink.changed.hash, defaults.hash);
    assert_eq!(shrink.changed.author, defaults.author);
    assert_left_titles_stayed(&shrink.before, &shrink.after);
    let width = title_left(&shrink.after, "Author") - title_left(&shrink.after, "Commit Message");
    let minimum = message_min();
    assert!(
        width + TOL >= minimum,
        "message column is {width:.2}px, minimum is {minimum:.1}px"
    );
    let author_delta = title_left(&shrink.after, "Author") - title_left(&shrink.before, "Author");
    assert!(author_delta < -TOL, "Author did not move left");
    assert!(
        author_delta > shrink.travel + 50.0,
        "Author followed the pointer past the minimum (moved {author_delta:.1}, pointer {:.1})",
        shrink.travel
    );
}

#[test]
fn dragging_below_the_minimum_stores_the_minimum_share() {
    let shrink = drag_message_below_its_minimum();
    let minimum = message_min();
    let implied = super::history::per_mille_for(minimum, shrink.panel);
    assert!(
        shrink.changed.message >= implied,
        "message per-mille {} is below the minimum width's share {implied} (min {minimum:.1}px, panel {:.1}px)",
        shrink.changed.message,
        shrink.panel
    );
    assert!(
        shrink.changed.message < HistoryColumns::default().message,
        "stored message {} did not shrink",
        shrink.changed.message
    );
}

#[test]
fn double_clicking_a_divider_resets_only_that_column() {
    let columns = HistoryColumns {
        branch: 160,
        graph: 100,
        hash: 120,
        message: 520,
        author: 160,
    };
    let mut harness = harness(
        egui::vec2(1700.0, HEIGHT),
        screen(
            one_commit("field notes from the estuary"),
            columns,
            false,
            false,
        ),
    );
    let before = header_row(&harness);
    let at = harness
        .get_by_label("Resize Commit Message")
        .rect()
        .center();
    harness.state_mut().actions.clear();
    move_pointer(&mut harness, at);
    harness.step();
    click_at(&mut harness, at);
    click_at(&mut harness, at);
    // The double-click is on the second click's response, which `show` reads
    // on the following frame and paints in the same pass.
    harness.step();

    let changed = take_column_change(&mut harness);
    assert_eq!(
        changed,
        HistoryColumns {
            message: HistoryColumns::default().message,
            ..columns
        }
    );

    let after = header_row(&harness);
    assert_left_titles_stayed(&before, &after);
    let before_width = title_left(&before, "Author") - title_left(&before, "Commit Message");
    let after_width = title_left(&after, "Author") - title_left(&after, "Commit Message");
    let expected = before_width * f32::from(HistoryColumns::default().message) / 520.0;
    assert!(
        (after_width - expected).abs() <= before_width * 0.02 + 2.0,
        "reset message width {after_width:.1}px, expected about {expected:.1}px from {before_width:.1}px"
    );
    let author_delta = title_left(&after, "Author") - title_left(&before, "Author");
    assert!(
        author_delta < -20.0,
        "Author should move left when the message column resets, moved {author_delta:.1}"
    );
}

#[test]
fn a_thousand_commits_stay_virtualized() {
    let commits: Vec<Commit> = (0..1000)
        .map(|index| Commit {
            oid: format!("{index:040x}"),
            summary: format!("row-{index:04}"),
        })
        .collect();
    let repo = opened("bulk-history", &commits, true);
    let (summary, short_oid, author, branch) = row_text(&repo);
    let row = RowText {
        summary: &summary,
        short_oid: &short_oid,
        author: &author,
        branch: &branch,
    };
    let harness = harness(
        egui::vec2(1400.0, HEIGHT),
        screen(repo, HistoryColumns::default(), false, false),
    );
    assert!(
        harness
            .query_by_label_contains("Commit history (1000+)")
            .is_some()
    );
    assert!(
        harness
            .query_by_label_contains("Showing the latest 1000 commits.")
            .is_some()
    );
    let _ = aligned_edges(&harness, "virtualized history", &row, true, false);
    let rendered = harness.query_all_by_label_contains("row-").count();
    assert!(
        (1..40).contains(&rendered),
        "expected a few dozen message labels, found {rendered}"
    );
    assert!(harness.query_by_label("row-0000").is_some());
    assert!(harness.query_by_label("row-0400").is_none());
    assert!(harness.query_by_label("row-0999").is_none());
}

#[test]
fn long_cells_stay_inside_their_columns() {
    let oid = "abcdef0123456789abcdef0123456789abcdef01";
    let summary = format!("{}{}", "字".repeat(80), "W".repeat(400));
    let branch_names = [
        format!("feature/{}", "m".repeat(400)),
        format!("topic/{}", "n".repeat(300)),
        format!("wip/{}", "p".repeat(300)),
        format!("experiment/{}", "q".repeat(300)),
    ];
    let author = person(
        "Ada Lovelace of the Analytical Engine",
        "ada@example.invalid",
    );
    let co_authors: Vec<Person> = (0..8)
        .map(|index| {
            person(
                &format!("Co-author {index} with an unnecessarily long display name"),
                &format!("co{index}@example.invalid"),
            )
        })
        .collect();
    let branches = branch_names
        .iter()
        .enumerate()
        .map(|(index, name)| GraphBranch {
            name: name.clone(),
            color: index,
            current: index == 0,
        })
        .collect::<Vec<_>>();
    let graph = HistoryGraph {
        rows: vec![GraphRow {
            oid: oid.to_owned(),
            summary: summary.clone(),
            author: author.clone(),
            co_authors: co_authors.clone(),
            branches,
            commit_lane: 0,
            commit_color: 0,
            traces: Vec::new(),
        }],
        truncated: false,
        columns: 1,
    };
    let repo = opened_graph("long-cells", graph);
    let (summary, short_oid, author_label, branch_label) = row_text(&repo);
    let row = RowText {
        summary: &summary,
        short_oid: &short_oid,
        author: &author_label,
        branch: &branch_label,
    };
    // Shares of 20 sit under every minimum, so each column is as narrow as
    // it is allowed to be and the long text has to be elided.
    let columns = HistoryColumns {
        branch: 20,
        graph: 20,
        hash: 20,
        message: 20,
        author: 20,
    };
    let harness = harness(
        egui::vec2(1600.0, HEIGHT),
        screen(repo, columns, false, false),
    );
    let headers = header_row(&harness);
    assert_eq!(
        headers.len(),
        TITLES.len(),
        "narrow shares on a wide window should still show every title: {}",
        header_names(&headers)
    );
    let _ = aligned_edges(&harness, "long cells", &row, true, false);

    let branch = title_left(&headers, "Branch");
    let graph = title_left(&headers, "Graph");
    let message = title_left(&headers, "Commit Message");
    let author_title = title_left(&headers, "Author");
    let author_edge = harness.get_by_label("Resize Author").rect().center().x;
    let row_y = content_rect(
        &harness,
        &summary,
        headers
            .iter()
            .find(|(name, _)| *name == "Commit Message")
            .unwrap()
            .1,
        None,
    )
    .center()
    .y;

    let chip = content_rect(
        &harness,
        &branch_label,
        headers
            .iter()
            .find(|(name, _)| *name == "Branch")
            .unwrap()
            .1,
        Some(row_y),
    );
    let hash_cell = content_rect(
        &harness,
        &short_oid,
        headers
            .iter()
            .find(|(name, _)| *name == "Commit Hash")
            .unwrap()
            .1,
        Some(row_y),
    );
    let message_cell = content_rect(
        &harness,
        &summary,
        headers
            .iter()
            .find(|(name, _)| *name == "Commit Message")
            .unwrap()
            .1,
        Some(row_y),
    );
    let author_cell = content_rect(
        &harness,
        &author_label,
        headers
            .iter()
            .find(|(name, _)| *name == "Author")
            .unwrap()
            .1,
        Some(row_y),
    );

    let graph_left = graph - super::history::CELL_PAD;
    let message_left = message - super::history::CELL_PAD;
    let author_left = author_title - super::history::CELL_PAD;
    assert!(
        chip.right() <= graph_left + TOL,
        "branch chip right {:.2} crosses the graph column at {graph_left:.2}",
        chip.right()
    );
    assert!(
        hash_cell.right() <= message_left + TOL,
        "hash cell right {:.2} crosses the message column at {message_left:.2}",
        hash_cell.right()
    );
    assert!(
        message_cell.right() <= author_left + TOL,
        "message cell right {:.2} crosses the author column at {author_left:.2}",
        message_cell.right()
    );
    assert!(
        author_cell.right() <= author_edge + TOL,
        "author cell right {:.2} crosses the author column edge at {author_edge:.2}",
        author_cell.right()
    );
    assert!(
        chip.left() + TOL >= branch - super::history::CELL_PAD,
        "branch chip starts in the previous column"
    );
    assert!(
        message_cell.width() < 400.0,
        "the long summary was not elided (accessibility width {:.1})",
        message_cell.width()
    );
}

struct PaintedText {
    clip: egui::Rect,
    bounds: egui::Rect,
    elided: bool,
}

fn painted_text(harness: &Harness<'_, Screen>, exact: &str) -> Vec<PaintedText> {
    let mut found = Vec::new();
    for clipped in &harness.output().shapes {
        collect_painted_text(&clipped.shape, clipped.clip_rect, exact, &mut found);
    }
    found
}

fn collect_painted_text(
    shape: &egui::Shape,
    clip: egui::Rect,
    exact: &str,
    found: &mut Vec<PaintedText>,
) {
    match shape {
        egui::Shape::Text(text) if text.galley.text() == exact => {
            found.push(PaintedText {
                clip,
                bounds: text.galley.rect.translate(text.pos.to_vec2()),
                elided: text.galley.elided,
            });
        }
        egui::Shape::Vec(shapes) => {
            for shape in shapes {
                collect_painted_text(shape, clip, exact, found);
            }
        }
        _ => {}
    }
}

fn assert_inside_panel(what: &str, rect: egui::Rect, panel: egui::Rect, window: egui::Vec2) {
    assert!(
        rect.width() > 1.0 && rect.height() > 1.0,
        "{what} has no extent ({rect:?})"
    );
    assert!(
        rect.left() >= panel.left() - TOL
            && rect.right() <= panel.right() + TOL
            && rect.top() >= panel.top() - TOL
            && rect.bottom() <= panel.bottom() + TOL,
        "{what} is clipped or drawn past the panel (rect {rect:?}, panel {panel:?})"
    );
    assert!(
        rect.left() >= -TOL && rect.right() <= window.x + TOL,
        "{what} overflows the minimum window width {:.0} (rect {rect:?})",
        window.x
    );
}

fn sole_clip(harness: &Harness<'_, Screen>, exact: &str) -> egui::Rect {
    let painted = painted_text(harness, exact);
    assert_eq!(
        painted.len(),
        1,
        "expected one painted {exact:?}, found {}\n{}",
        painted.len(),
        labels(harness)
    );
    painted[0].clip
}

/// The history panel is the gap between the branch sidebar and the detail
/// sidebar. An empty graph returns before any column is drawn, so those
/// clips are the panel.
fn history_panel(harness: &Harness<'_, Screen>) -> egui::Rect {
    let branches = sole_clip(harness, "Branches (1)");
    let detail = sole_clip(harness, "Latest commit");
    egui::Rect::from_min_max(
        egui::pos2(branches.right(), branches.top()),
        egui::pos2(detail.left(), branches.bottom()),
    )
}

/// The opening line has to be fully inside the history panel at the minimum
/// window, and actually painted there.
fn assert_opening_line_inside_panel(harness: &Harness<'_, Screen>, window: egui::Vec2) {
    let exact = "Opening history…";
    let line = harness.get_by_label(exact).rect();
    let panel = history_panel(harness);
    assert_inside_panel(exact, line, panel, window);

    let painted = painted_text(harness, exact);
    match painted.as_slice() {
        [painted] => {
            assert!(
                !painted.elided,
                "{exact} is truncated (line {line:?}, clip {:?})",
                painted.clip
            );
            assert_inside_panel(exact, painted.bounds, panel, window);
        }
        [] => panic!(
            "{exact} is clipped away: laid out at {line:?}, history panel is {panel:?}\n{}",
            labels(harness)
        ),
        _ => panic!(
            "expected one painted {exact:?}, found {}\n{}",
            painted.len(),
            labels(harness)
        ),
    }
}

fn assert_titles_do_not_move(
    loading: &[(&'static str, egui::Rect)],
    idle: &[(&'static str, egui::Rect)],
) {
    let loading_names: Vec<_> = loading.iter().map(|(name, _)| *name).collect();
    let idle_names: Vec<_> = idle.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        loading_names,
        idle_names,
        "column titles changed while history was opening: loading {}, idle {}",
        header_names(loading),
        header_names(idle)
    );
    let mut previous = -1isize;
    for name in &loading_names {
        let index = TITLES
            .iter()
            .position(|title| title == name)
            .unwrap_or_else(|| panic!("unexpected title {name}")) as isize;
        assert!(
            index > previous,
            "titles out of left-to-right order: {}",
            header_names(loading)
        );
        previous = index;
    }
    for pair in loading.windows(2) {
        assert!(
            pair[0].1.left() < pair[1].1.left(),
            "{} is not left of {}",
            pair[0].0,
            pair[1].0
        );
        let idle_gap = title_left(idle, pair[1].0) - title_left(idle, pair[0].0);
        let loading_gap = pair[1].1.left() - pair[0].1.left();
        assert!(
            (loading_gap - idle_gap).abs() <= TOL,
            "{} to {} gap changed from {idle_gap:.2} to {loading_gap:.2}",
            pair[0].0,
            pair[1].0
        );
    }
}

#[test]
fn opening_history_stays_inside_the_minimum_window() {
    // Same 520×400 window as the narrow-history kittest, with both sidebars
    // open. An empty graph draws no columns, so the opening line itself has
    // to stay inside the history panel.
    let mut loading_screen = screen(
        opened_while_history_loads("opening"),
        HistoryColumns::default(),
        true,
        true,
    );
    loading_screen.history_loading = true;
    let loading = harness(MIN_WINDOW, loading_screen);
    let idle = harness(
        MIN_WINDOW,
        screen(
            opened_while_history_loads("settled"),
            HistoryColumns::default(),
            true,
            true,
        ),
    );

    assert!(
        loading.query_by_label("Commit history (0)").is_some(),
        "header should stay Commit history (0) while the graph is empty\n{}",
        labels(&loading)
    );
    assert!(
        loading.query_by_label("Opening history…").is_some(),
        "missing opening line\n{}",
        labels(&loading)
    );
    assert!(
        loading.query_by_label("No commits yet.").is_none(),
        "a loading history is not an empty repository\n{}",
        labels(&loading)
    );
    assert!(
        idle.query_by_label("No commits yet.").is_some(),
        "the same empty repo should say there are no commits once loading is finished\n{}",
        labels(&idle)
    );
    assert!(
        idle.query_by_label("Opening history…").is_none(),
        "a finished history should not stay on the opening line\n{}",
        labels(&idle)
    );

    assert_opening_line_inside_panel(&loading, MIN_WINDOW);

    let loading_titles = header_row(&loading);
    let idle_titles = header_row(&idle);
    if !loading_titles.is_empty() || !idle_titles.is_empty() {
        assert_titles_do_not_move(&loading_titles, &idle_titles);
    }
}

#[test]
fn hovering_a_panel_toggle_does_not_grow_the_top_bar() {
    // Branches open, detail closed: the two top-bar icons say different things,
    // so each hover has to surface its own label.
    let mut harness = harness(
        egui::Vec2::new(1100.0, HEIGHT),
        screen(
            one_commit("still bar"),
            HistoryColumns::default(),
            true,
            false,
        ),
    );
    let header = "Commit history (1)";
    let header_y = label_rects(&harness, header)
        .first()
        .unwrap_or_else(|| panic!("missing {header}\n{}", labels(&harness)))
        .top();
    let mut toggles = panel_toggle_rects(&harness);
    toggles.sort_by(|left, right| left.center().x.total_cmp(&right.center().x));
    assert_eq!(
        toggles.len(),
        2,
        "expected the two panel icons\n{}",
        labels(&harness)
    );

    // Longer than egui's 0.5s tooltip delay at the harness step of 1/60s.
    let steps_until_tip = 45;
    let tips = ["Hide branches", "Show commit details"];
    for (before, tip) in toggles.iter().zip(tips) {
        let height = before.height();
        harness.hover_at(before.center());
        harness.run_steps(steps_until_tip);

        let header_after = label_rects(&harness, header);
        assert!(
            (header_after[0].top() - header_y).abs() <= TOL,
            "{tip} moved the history header from {header_y:.2} to {:.2}",
            header_after[0].top()
        );
        let after = panel_toggle_rects(&harness)
            .into_iter()
            .find(|rect| (rect.center().x - before.center().x).abs() < 8.0)
            .unwrap_or_else(|| panic!("lost the {tip} toggle\n{}", labels(&harness)));
        assert!(
            (after.height() - height).abs() <= TOL,
            "{tip} changed the button height from {height:.2} to {:.2}",
            after.height()
        );
        assert!(
            harness.query_by_label(tip).is_some(),
            "missing hover label {tip:?}\n{}",
            labels(&harness)
        );
    }
}

/// The two icon buttons in the top bar, not the ones in the bottom bar.
fn panel_toggle_rects(harness: &Harness<'_, Screen>) -> Vec<egui::Rect> {
    let buttons: Vec<_> = harness
        .query_all_by_role(Role::Button)
        .map(|node| node.rect())
        .collect();
    let top = buttons
        .iter()
        .map(|rect| rect.top())
        .fold(f32::MAX, f32::min);
    buttons
        .into_iter()
        .filter(|rect| rect.top() < top + 4.0)
        .collect()
}
