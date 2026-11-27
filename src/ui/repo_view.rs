//! Repository screen. A top bar with the branch and detail toggles and the
//! repository name, a sidebar of local branches, the commit history in the
//! middle ([`super::history`]), the chosen commit on the right (the latest,
//! until a hash or message is clicked) with the terminal beneath it, and a
//! bottom bar with Home, a folder shortcut and Settings, the current branch
//! (a menu that checks out another local branch), fetch and pull, and the
//! terminal toggle at the right end.

use cthulhu_git::git::{Branch, CommitFile, CommitStats, GraphBranch, Head, Person, Upstream};
use cthulhu_git::github::{GitHubRepo, RepoContext};
use cthulhu_git::settings::HistoryColumns;
use eframe::egui::{
    self, Align, Button, Color32, CursorIcon, Frame, Label, Layout, Margin, Pos2, Rect, RichText,
    ScrollArea, Sense, Stroke, Ui, Vec2, WidgetText, containers::menu::MenuButton,
};

use super::avatars::{self, AvatarService};
use super::changes;
use super::clock;
use super::history;
use super::hover::{self, CachedCommit};
use super::icons::{self, Icon};
use super::terminal;
use super::text;
use super::theme::{self, Palette};
use super::widgets;
use super::{Action, ListedBranch, OpenedRepo, SyncKind};

const LANE_DOT: f32 = 8.0;
/// Each sidebar's default share of the window, so the history keeps a fair
/// part of a small window and a large one does not waste the sides.
const SIDEBAR_DEFAULT_SHARE: f32 = 0.22;
const SIDEBAR_MIN_WIDTH: f32 = 220.0;
const SIDEBAR_MAX_WIDTH: f32 = 560.0;
const DETAIL_AVATAR: f32 = 28.0;

/// Which sidebars and the terminal strip are open.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Panels {
    pub branches: bool,
    pub detail: bool,
    pub terminal: bool,
}

/// What the repository screen reads besides the repository itself.
pub struct ViewState<'a> {
    pub error: Option<&'a str>,
    pub panels: Panels,
    pub syncing: Option<SyncKind>,
    pub columns: HistoryColumns,
    /// This repository's history graph is still being read.
    pub history_loading: bool,
    /// A folder dialog or a repository open is already in progress.
    pub busy: bool,
}

pub fn show(
    ui: &mut Ui,
    repo: &mut OpenedRepo,
    state: &ViewState<'_>,
    avatars: &mut AvatarService,
) -> Vec<Action> {
    let palette = theme::current(ui.ctx()).palette;
    let mut actions = Vec::new();
    repo.diffs.poll(ui.ctx(), &repo.git, &repo.info.root);
    // The toggle buttons and dragging a sidebar edge past its minimum width
    // both change these; the app saves the change.
    let mut panels = state.panels;
    let sidebar_default =
        (ui.available_width() * SIDEBAR_DEFAULT_SHARE).clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
    // Replies from the shell have to land even while the strip is hidden.
    terminal::service(ui.ctx(), &mut repo.terminal);

    // Top and bottom bars come first so they span the whole window width.
    egui::Panel::top("repo-top-bar")
        .frame(widgets::bar_frame(&palette))
        .show(ui, |ui| {
            top_bar(ui, repo, &palette, &mut panels);
        });

    egui::Panel::bottom("repo-bottom-bar")
        .frame(widgets::bar_frame(&palette))
        .show(ui, |ui| {
            bottom_bar(ui, repo, state, &palette, &mut panels, &mut actions);
        });

    let side = side_frame(&palette);
    egui::Panel::left("repo-branches-sidebar")
        .resizable(true)
        .default_size(sidebar_default)
        .size_range(SIDEBAR_MIN_WIDTH..=SIDEBAR_MAX_WIDTH)
        .frame(side)
        .show_collapsible(ui, &mut panels.branches, |ui| {
            branches_panel(ui, repo, &palette);
        });

    egui::Panel::right("repo-detail-sidebar")
        .resizable(true)
        .default_size(sidebar_default)
        .size_range(SIDEBAR_MIN_WIDTH..=SIDEBAR_MAX_WIDTH)
        .frame(side)
        .show_collapsible(ui, &mut panels.detail, |ui| {
            detail_column(ui, repo, &palette, panels.terminal, avatars);
        });

    egui::CentralPanel::default()
        .frame(side_frame(&palette))
        .show(ui, |ui| {
            if let Some(error) = state.error {
                widgets::error_banner(ui, error);
                ui.add_space(12.0);
            }
            let history = history::show(
                ui,
                repo,
                &palette,
                state.columns,
                avatars,
                state.history_loading,
            );
            if let Some(columns) = history.columns {
                actions.push(Action::SetHistoryColumns(columns));
            }
            // Choosing a commit opens the sidebar. A commit that is already
            // chosen does not, so closing the panel by hand stays closed.
            if history.reveal_detail {
                panels.detail = true;
            }
        });

    if panels != state.panels {
        actions.push(Action::SetPanels(panels));
    }
    close_changes_for_other_commit(repo);
    if let Some(mut changes) = repo.changes.take() {
        let close = changes::show(
            ui.ctx(),
            &mut changes,
            &mut repo.diffs,
            &repo.git,
            &repo.info.root,
            &palette,
        );
        if !close {
            repo.changes = Some(changes);
        }
    }

    if !panels.terminal || !panels.detail {
        terminal::surrender_focus(ui.ctx(), repo.view_id);
    }
    widgets::show_clipboard_toast(ui.ctx());
    actions
}

fn detail_column(
    ui: &mut Ui,
    repo: &mut OpenedRepo,
    palette: &Palette,
    terminal_open: bool,
    avatars: &mut AvatarService,
) {
    if !terminal_open {
        detail_panel(ui, repo, palette, avatars);
        return;
    }

    let available = ui.available_height();
    let (detail_h, splitter_h, terminal_h) = terminal::split_height(ui, repo.view_id, available);
    ui.allocate_ui(Vec2::new(ui.available_width(), detail_h), |ui| {
        ui.set_min_height(detail_h);
        detail_panel(ui, repo, palette, avatars);
    });
    let splitter = ui.allocate_response(Vec2::new(ui.available_width(), splitter_h), Sense::drag());
    if splitter.hovered() || splitter.dragged() {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
    }
    let y = splitter.rect.center().y;
    ui.painter()
        .hline(splitter.rect.x_range(), y, Stroke::new(1.0, palette.border));
    if splitter.dragged() {
        terminal::drag_split(ui, repo.view_id, available, splitter.drag_delta().y);
    }
    ui.allocate_ui(Vec2::new(ui.available_width(), terminal_h), |ui| {
        ui.set_min_height(terminal_h);
        terminal::show(
            ui,
            &mut repo.terminal,
            &repo.info.root,
            repo.view_id,
            palette,
        );
    });
}

/// Home, the folder dialog and Settings; the current branch, whose menu
/// switches to another; Fetch and Pull; the terminal toggle at the right end.
fn bottom_bar(
    ui: &mut Ui,
    repo: &OpenedRepo,
    state: &ViewState<'_>,
    palette: &Palette,
    panels: &mut Panels,
    actions: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        widgets::home_and_folder(ui, state.busy, actions);
        if icons::icon_button(ui, Icon::Settings, palette.text, "Settings").clicked() {
            actions.push(Action::OpenSettings);
        }
        ui.separator();
        let (branch_label, branch_color) = branch_text(&repo.info.head, palette);
        let offer = match state.syncing {
            Some(kind) => unavailable(kind.progress_label(), kind.progress_label()),
            None => sync_offer(&repo.info.head, &repo.branches),
        };
        let branch_hover = state
            .syncing
            .map_or("Switch branch", SyncKind::progress_label);
        let (terminal_tint, terminal_hover) = if panels.terminal {
            (palette.accent, "Hide terminal")
        } else {
            (palette.text_muted, "Show terminal")
        };
        // The name truncates before fetch and pull, so a long branch
        // cannot push those buttons off the bar. The terminal stays
        // at the right end, and the spare width sits between them.
        let terminal_toggle = ui
            .with_layout(Layout::right_to_left(Align::Center), |ui| {
                let toggle = icons::icon_button(ui, Icon::Terminal, terminal_tint, terminal_hover);
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    let reserve = icons::icon_text_button_width(ui, "Fetch")
                        + icons::icon_text_button_width(ui, "Pull")
                        + ui.spacing().item_spacing.x;
                    let max_name = (ui.available_width() - reserve).max(0.0);
                    ui.scope(|ui| {
                        ui.set_max_width(max_name);
                        let enabled = state.syncing.is_none();
                        if let Some(name) =
                            branch_menu(ui, repo, branch_label, branch_color, enabled, branch_hover)
                        {
                            actions.push(Action::Checkout(name));
                        }
                    });
                    let fetch = offer.fetch_branch.is_some();
                    if sync_labeled(
                        ui,
                        palette,
                        Icon::CloudDownload,
                        "Fetch",
                        fetch,
                        offer.fetch_hover,
                    ) && let Some(branch) = offer.fetch_branch.clone()
                    {
                        actions.push(Action::Fetch(branch));
                    }
                    if sync_labeled(
                        ui,
                        palette,
                        Icon::ArrowDownToLine,
                        "Pull",
                        offer.pull,
                        offer.pull_hover,
                    ) {
                        actions.push(Action::Pull);
                    }
                });
                toggle
            })
            .inner;
        if terminal_toggle.clicked() {
            panels.terminal = !panels.terminal;
            // The strip lives in the right sidebar, so showing it opens that too.
            if panels.terminal {
                panels.detail = true;
            }
        }
    });
}

impl SyncKind {
    /// What the bar says while this runs, on every control it turns off.
    fn progress_label(self) -> &'static str {
        match self {
            Self::Fetch => "Fetching…",
            Self::Pull => "Pulling…",
            Self::Checkout => "Switching…",
        }
    }
}

fn side_frame(palette: &Palette) -> Frame {
    Frame::new()
        .fill(palette.background)
        .inner_margin(Margin::same(12))
}

/// The branch toggle on the left, the detail toggle on the right, and the name
/// centered on the window. The name is truncated before it would reach either
/// toggle.
fn top_bar(ui: &mut Ui, repo: &OpenedRepo, palette: &Palette, panels: &mut Panels) {
    ui.horizontal(|ui| {
        let (branches_tint, branches_hover) = if panels.branches {
            (palette.accent, "Hide branches")
        } else {
            (palette.text_muted, "Show branches")
        };
        let branches_toggle =
            icons::icon_button(ui, Icon::PanelLeft, branches_tint, branches_hover);
        if branches_toggle.clicked() {
            panels.branches = !panels.branches;
        }

        let (detail_tint, detail_hover) = if panels.detail {
            (palette.accent, "Hide commit details")
        } else {
            (palette.text_muted, "Show commit details")
        };
        let detail_toggle = ui
            .with_layout(Layout::right_to_left(Align::Center), |ui| {
                icons::icon_button(ui, Icon::PanelRight, detail_tint, detail_hover)
            })
            .inner;
        if detail_toggle.clicked() {
            panels.detail = !panels.detail;
        }

        let row = ui.max_rect();
        let spacing = ui.spacing().item_spacing.x;
        let left_reserved = branches_toggle.rect.right() - row.left() + spacing;
        let right_reserved = row.right() - detail_toggle.rect.left() + spacing;
        let reserved = left_reserved.max(right_reserved);
        let name_rect = Rect::from_center_size(
            Pos2::new(row.center().x, branches_toggle.rect.center().y),
            Vec2::new(
                (row.width() - 2.0 * reserved).max(0.0),
                branches_toggle.rect.height(),
            ),
        );
        ui.put(
            name_rect,
            Label::new(RichText::new(&repo.info.name).size(15.0).strong()).truncate(),
        )
        .on_hover_text(repo.info.root.display().to_string());
    });
}

fn branches_panel(ui: &mut Ui, repo: &mut OpenedRepo, palette: &Palette) {
    ui.add(
        Label::new(RichText::new(format!("Branches ({})", repo.branches.len())).strong())
            .truncate(),
    );
    ui.add_space(6.0);

    if repo.branches.is_empty() {
        ui.label(RichText::new("No branches.").weak());
        return;
    }

    let tips = super::selected_tips(&repo.branches, repo.detached_oid.as_deref());
    ScrollArea::vertical()
        .id_salt(repo.view_id.with("branches"))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for listed in &mut repo.branches {
                let lane = tips
                    .iter()
                    .find(|tip| tip.name == listed.branch.name)
                    .map(|tip| tip.color);
                branch_row(ui, listed, lane, palette);
            }
        });
}

fn branch_row(ui: &mut Ui, listed: &mut ListedBranch, lane: Option<usize>, palette: &Palette) {
    let width = ui.available_width();
    ui.push_id(&listed.branch.name, |ui| {
        ui.set_max_width(width);
        ui.horizontal(|ui| {
            let checkbox = ui.add(egui::Checkbox::without_text(&mut listed.checked));
            let (dot, _) = ui.allocate_exact_size(Vec2::splat(LANE_DOT), Sense::hover());
            if let Some(color) = lane {
                ui.painter().circle_filled(
                    dot.center(),
                    LANE_DOT / 2.0,
                    palette.graph_color(color),
                );
            }
            let mut name = RichText::new(&listed.branch.name);
            if listed.branch.current {
                name = name.strong();
            }
            let response = ui
                .add(
                    Label::new(name)
                        .truncate()
                        .selectable(false)
                        .sense(Sense::click()),
                )
                .on_hover_cursor(CursorIcon::PointingHand);
            // The branch name beside the checkbox is its accessible name. The
            // empty label `without_text` sets would be read instead, so clear it.
            ui.ctx().accesskit_node_builder(checkbox.id, |node| {
                node.clear_label();
                node.push_labelled_by(response.id.accesskit_id());
            });
            if response.clicked() {
                listed.checked = !listed.checked;
            }
        });

        let indent = ui.spacing().icon_width + LANE_DOT + ui.spacing().item_spacing.x * 2.0;
        ui.horizontal(|ui| {
            ui.add_space(indent);
            ui.add(
                Label::new(RichText::new(branch_metrics(&listed.branch)).small().weak()).truncate(),
            );
        });
    });
    ui.add_space(8.0);
}

fn branch_metrics(branch: &Branch) -> String {
    let commits = match branch.commit_count {
        1 => "1 commit".to_owned(),
        count => format!("{count} commits"),
    };
    match &branch.upstream {
        Upstream::None => format!("{commits} · no upstream"),
        Upstream::Gone { name, .. } if name.is_empty() => format!("{commits} · upstream gone"),
        Upstream::Gone { name, .. } => format!("{commits} · {name} · upstream gone"),
        Upstream::Tracking {
            name,
            ahead,
            behind,
            ..
        } => {
            let relation = match (*ahead, *behind) {
                (0, 0) => "in sync".to_owned(),
                (ahead, 0) => format!("{ahead} ahead"),
                (0, behind) => format!("{behind} behind"),
                (ahead, behind) => format!("{ahead} ahead, {behind} behind"),
            };
            if name.is_empty() {
                format!("{commits} · {relation}")
            } else {
                format!("{commits} · {name} · {relation}")
            }
        }
    }
}

fn detail_panel(
    ui: &mut Ui,
    repo: &mut OpenedRepo,
    palette: &Palette,
    avatars: &mut AvatarService,
) {
    let selected = repo.selected_commit.clone();
    let title = if selected.is_some() {
        "Selected commit"
    } else {
        "Latest commit"
    };
    ui.add(Label::new(RichText::new(title).strong()).truncate());
    ui.add_space(6.0);

    let Some(shown) = shown_oid(repo) else {
        ui.label(RichText::new("No commits yet.").weak());
        return;
    };
    repo.hover.request(&shown);

    let open_changes = ScrollArea::vertical()
        .id_salt(repo.view_id.with("detail"))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if let Some(oid) = selected.as_deref() {
                paint_selected(ui, repo, palette, avatars, oid)
            } else {
                paint_latest(ui, repo, palette, avatars)
            }
        })
        .inner;
    if open_changes
        && let Some(CachedCommit::Ready(hover)) = repo.hover.cached(&shown)
        && !hover.files.is_empty()
    {
        let files = hover.files.clone();
        repo.changes = Some(changes::Changes::new(shown, files));
    }
}

/// The commit in the sidebar: the one chosen in the history, else the
/// latest. A click in the history changes it within the frame, so it is
/// read where it is needed.
fn shown_oid(repo: &OpenedRepo) -> Option<String> {
    repo.selected_commit
        .clone()
        .or_else(|| repo.latest.as_ref().map(|commit| commit.oid.clone()))
}

/// Drops the diff modal when the sidebar moves to a different commit.
fn close_changes_for_other_commit(repo: &mut OpenedRepo) {
    let shown = shown_oid(repo);
    if repo
        .changes
        .as_ref()
        .is_some_and(|changes| Some(&changes.oid) != shown.as_ref())
    {
        repo.changes = None;
    }
}

/// The latest commit. Its date and message are already known; the hover read
/// only adds the relative time and the short stat.
fn paint_latest(
    ui: &mut Ui,
    repo: &OpenedRepo,
    palette: &Palette,
    avatars: &mut AvatarService,
) -> bool {
    let Some(commit) = repo.latest.as_ref() else {
        return false;
    };
    let cached = repo.hover.cached(&commit.oid);
    let (unix, stats, failed) = hover_extras(cached.as_ref());
    let (tags, files) = hover_files(cached.as_ref());
    let branches = repo
        .graph
        .rows
        .iter()
        .find(|row| row.oid == commit.oid)
        .map(|row| row.branches.as_slice())
        .unwrap_or(&[]);
    paint_commit(
        ui,
        repo.github.as_ref(),
        palette,
        avatars,
        CommitBody {
            oid: &commit.oid,
            author: Some(&commit.author),
            co_authors: &commit.co_authors,
            message: Some(&commit.message),
            date: DetailDate::Known {
                absolute: &commit.authored_at,
                unix,
            },
            notice: failed,
            branches,
            tags,
            files,
            stats,
        },
    )
}

/// A commit chosen from its hash or message. The row paints immediately; the
/// date stays on "Loading…" until `git show` returns.
fn paint_selected(
    ui: &mut Ui,
    repo: &OpenedRepo,
    palette: &Palette,
    avatars: &mut AvatarService,
    oid: &str,
) -> bool {
    let row = repo.graph.rows.iter().find(|row| row.oid == oid);
    let (ready, failed) = match repo.hover.cached(oid) {
        Some(CachedCommit::Ready(hover)) => (Some(hover), None),
        Some(CachedCommit::Failed(message)) => (None, Some(message)),
        Some(CachedCommit::Pending) | None => (None, None),
    };
    let author = ready
        .map(|hover| &hover.author)
        .or_else(|| row.map(|row| &row.author));
    let co_authors = ready
        .map(|hover| hover.co_authors.as_slice())
        .or_else(|| row.map(|row| row.co_authors.as_slice()))
        .unwrap_or(&[]);
    let message = ready
        .map(|hover| hover.message.as_str())
        .or_else(|| row.map(|row| row.summary.as_str()));
    let date = if let Some(hover) = ready {
        DetailDate::Known {
            absolute: &hover.authored_at,
            unix: Some(hover.authored_unix),
        }
    } else if let Some(message) = failed {
        DetailDate::Failed(message)
    } else {
        DetailDate::Pending
    };
    let stats = ready.and_then(|hover| hover.stats);
    let tags = ready.map(|hover| hover.tags.as_slice()).unwrap_or(&[]);
    let files = ready.map(|hover| hover.files.as_slice()).unwrap_or(&[]);
    let branches = row.map(|row| row.branches.as_slice()).unwrap_or(&[]);
    paint_commit(
        ui,
        repo.github.as_ref(),
        palette,
        avatars,
        CommitBody {
            oid,
            author,
            co_authors,
            message,
            date,
            notice: None,
            branches,
            tags,
            files,
            stats,
        },
    )
}

fn hover_extras<'a>(
    cached: Option<&'a CachedCommit<'a>>,
) -> (Option<i64>, Option<CommitStats>, Option<&'a str>) {
    match cached {
        Some(CachedCommit::Ready(hover)) => (Some(hover.authored_unix), hover.stats, None),
        Some(CachedCommit::Failed(message)) => (None, None, Some(message)),
        Some(CachedCommit::Pending) | None => (None, None, None),
    }
}

fn hover_files<'a>(cached: Option<&'a CachedCommit<'a>>) -> (&'a [String], &'a [CommitFile]) {
    match cached {
        Some(CachedCommit::Ready(hover)) => (hover.tags.as_slice(), hover.files.as_slice()),
        Some(CachedCommit::Failed(_) | CachedCommit::Pending) | None => (&[], &[]),
    }
}

struct CommitBody<'a> {
    oid: &'a str,
    author: Option<&'a Person>,
    co_authors: &'a [Person],
    message: Option<&'a str>,
    date: DetailDate<'a>,
    /// Hover failed after the date was already known, so the date stays.
    notice: Option<&'a str>,
    branches: &'a [GraphBranch],
    tags: &'a [String],
    files: &'a [CommitFile],
    stats: Option<CommitStats>,
}

enum DetailDate<'a> {
    Known {
        absolute: &'a str,
        unix: Option<i64>,
    },
    Pending,
    Failed(&'a str),
}

/// `true` when View changes was clicked.
fn paint_commit(
    ui: &mut Ui,
    github: Option<&GitHubRepo>,
    palette: &Palette,
    avatars: &mut AvatarService,
    body: CommitBody<'_>,
) -> bool {
    let context = github.map(|github| RepoContext {
        repo: github.clone(),
        head: body.oid.to_owned(),
    });
    hash_block(ui, palette, body.oid, github);
    if let Some(author) = body.author {
        ui.label(RichText::new("Author").small().weak());
        ui.add_space(2.0);
        person_line(
            ui,
            palette,
            avatars,
            author,
            context.as_ref(),
            Some(body.oid),
        );
        ui.add_space(10.0);
        if !body.co_authors.is_empty() {
            ui.label(RichText::new("Co-authors").small().weak());
            ui.add_space(2.0);
            for person in body.co_authors {
                person_line(ui, palette, avatars, person, context.as_ref(), None);
            }
            ui.add_space(10.0);
        }
    }
    ui.label(RichText::new("Date").small().weak());
    ui.add_space(2.0);
    match body.date {
        DetailDate::Known { absolute, unix } => {
            let text = match unix {
                Some(unix) => {
                    format!(
                        "{} · {absolute}",
                        hover::relative_time(clock::now_unix(), unix)
                    )
                }
                None => absolute.to_owned(),
            };
            sidebar_text(ui, RichText::new(text));
        }
        DetailDate::Pending => {
            ui.label(RichText::new("Loading…").weak());
        }
        DetailDate::Failed(message) => {
            sidebar_text(ui, RichText::new(message).color(palette.error));
        }
    }
    if let Some(notice) = body.notice {
        ui.add_space(2.0);
        sidebar_text(ui, RichText::new(notice).color(palette.error));
    }
    ui.add_space(10.0);
    ui.label(RichText::new("Message").small().weak());
    ui.add_space(2.0);
    match body.message {
        Some("") => {
            ui.label(RichText::new("No message.").weak());
        }
        Some(message) => hover::message_with_links(ui, message, github, palette),
        None => {
            ui.label(RichText::new("Loading…").weak());
        }
    }
    if !body.branches.is_empty() {
        ui.add_space(10.0);
        hover::branch_chips(ui, body.branches, palette);
    }
    if !body.tags.is_empty() {
        ui.add_space(10.0);
        hover::tag_chips(ui, body.tags, palette);
    }
    if let Some(stats) = body.stats {
        ui.add_space(10.0);
        hover::commit_stats_line(ui, stats, palette);
    }
    if body.files.is_empty() {
        return false;
    }
    ui.add_space(10.0);
    view_changes_button(ui, palette).clicked()
}

fn view_changes_button(ui: &mut Ui, palette: &Palette) -> egui::Response {
    let button = Button::new("View changes")
        .fill(palette.surface)
        .stroke(Stroke::new(1.0, palette.border))
        .corner_radius(widgets::CORNER_RADIUS);
    ui.add(button).on_hover_cursor(CursorIcon::PointingHand)
}

fn hash_block(ui: &mut Ui, palette: &Palette, oid: &str, github: Option<&GitHubRepo>) {
    ui.label(RichText::new("Hash").small().weak());
    ui.add_space(2.0);
    // The click is the action, so the text is not selectable.
    let hash = sidebar_label(ui, RichText::new(oid).monospace().color(palette.hash))
        .selectable(false)
        .sense(Sense::click());
    let hash = ui
        .add(hash)
        .on_hover_text("Copy the full commit hash")
        .on_hover_cursor(CursorIcon::PointingHand);
    if hash.clicked() {
        widgets::copy_commit_hash(ui.ctx(), oid);
    }
    if let Some(github) = github {
        ui.add_space(4.0);
        open_on_github(ui, palette, &github.commit_url(oid));
    }
    ui.add_space(10.0);
}

/// GitHub mark beside the commit link. The mark opens the same page.
fn open_on_github(ui: &mut Ui, palette: &Palette, url: &str) {
    ui.horizontal(|ui| {
        let gap = ui.spacing().icon_spacing;
        ui.spacing_mut().item_spacing.x = gap;
        let mark = ui
            .add(icons::tinted(ui, Icon::GitHub, palette.accent).sense(Sense::click()))
            .on_hover_cursor(CursorIcon::PointingHand);
        ui.hyperlink_to("Open on GitHub", url);
        let new_tab = mark.clicked_with_open_in_background();
        if new_tab || mark.clicked() {
            ui.open_url(egui::OpenUrl {
                url: url.to_owned(),
                new_tab,
            });
        }
    });
}

/// Avatar, then the name and the address. The address is a mail link when it
/// is a plain `local@domain`.
fn person_line(
    ui: &mut Ui,
    palette: &Palette,
    avatars: &mut AvatarService,
    person: &Person,
    context: Option<&RepoContext>,
    authored: Option<&str>,
) {
    let texture = avatars.texture(ui.ctx(), person, context, authored);
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(DETAIL_AVATAR), Sense::hover());
        avatars::paint_avatar(
            ui.painter(),
            rect.center(),
            DETAIL_AVATAR,
            person,
            texture.as_ref(),
            palette,
            None,
        );
        ui.vertical(|ui| {
            if !person.name.trim().is_empty() {
                sidebar_text(ui, RichText::new(&person.name));
            }
            hover::email_link(ui, &person.email, palette);
        });
    });
}

/// `text` inside the width this sidebar was given.
///
/// A commit hash and a name with an address are single tokens. Letting either extend
/// grows the sidebar up to the remaining window and leaves the history column
/// with no room. Breaking inside the token keeps the whole string visible.
fn sidebar_label(ui: &Ui, text: impl Into<WidgetText>) -> Label {
    Label::new(text::wrapped(ui, text))
}

fn sidebar_text(ui: &mut Ui, text: impl Into<WidgetText>) {
    ui.add(sidebar_label(ui, text));
}

struct SyncOffer {
    /// The local branch whose upstream Fetch updates.
    fetch_branch: Option<String>,
    pull: bool,
    fetch_hover: &'static str,
    pull_hover: &'static str,
}

fn unavailable(fetch_hover: &'static str, pull_hover: &'static str) -> SyncOffer {
    SyncOffer {
        fetch_branch: None,
        pull: false,
        fetch_hover,
        pull_hover,
    }
}

/// Fetch when the current branch's upstream is a remote-tracking ref. Pull
/// while the upstream is still there, remote or local.
fn sync_offer(head: &Head, branches: &[ListedBranch]) -> SyncOffer {
    let (Head::Branch(name) | Head::Unborn(name)) = head else {
        return unavailable(
            "Detached HEAD has no branch to fetch.",
            "Pull needs a branch with an upstream.",
        );
    };
    let Some(listed) = branches.iter().find(|listed| listed.branch.name == *name) else {
        return unavailable(
            "This branch has no upstream.",
            "Pull needs a branch with an upstream.",
        );
    };
    let fetch = |remote: bool| {
        if remote {
            (Some(name.clone()), "Fetch current branch")
        } else {
            (
                None,
                "The upstream is a local branch; there is nothing to fetch.",
            )
        }
    };
    match &listed.branch.upstream {
        Upstream::Tracking { remote, .. } => {
            let (fetch_branch, fetch_hover) = fetch(*remote);
            SyncOffer {
                fetch_branch,
                pull: true,
                fetch_hover,
                pull_hover: "Pull current branch (fast-forward only)",
            }
        }
        Upstream::Gone { remote, .. } => {
            let (fetch_branch, fetch_hover) = fetch(*remote);
            SyncOffer {
                fetch_branch,
                pull: false,
                fetch_hover,
                pull_hover: "The upstream branch is gone.",
            }
        }
        Upstream::None => unavailable(
            "This branch has no upstream.",
            "Pull needs a branch with an upstream.",
        ),
    }
}

/// Icon and word, one click. Dimmed, and not clickable, when the action cannot run.
fn sync_labeled(
    ui: &mut Ui,
    palette: &Palette,
    icon: Icon,
    word: &str,
    enabled: bool,
    hover: &str,
) -> bool {
    let color = if enabled {
        palette.text
    } else {
        palette.text_muted
    };
    icons::icon_text_button(ui, icon, word, color, hover, enabled).clicked()
}

/// Icon and branch name. Click opens the local branches; choosing another one
/// checks it out. The current row is marked and does not emit a checkout.
fn branch_menu(
    ui: &mut Ui,
    repo: &OpenedRepo,
    label: RichText,
    color: Color32,
    enabled: bool,
    hover: &str,
) -> Option<String> {
    let button = Button::image_and_text(icons::tinted(ui, Icon::GitBranch, color), label)
        .frame_when_inactive(false)
        .truncate();
    let (response, picked) = if enabled {
        let (response, inner) = MenuButton::from_button(button).ui(ui, |ui| pick_branch(ui, repo));
        (response, inner.and_then(|inner| inner.inner))
    } else {
        (ui.add_enabled(false, button), None)
    };
    let response = response.on_hover_text(hover);
    if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand);
    }
    picked
}

fn pick_branch(ui: &mut Ui, repo: &OpenedRepo) -> Option<String> {
    if repo.branches.is_empty() {
        ui.label("No branches.");
        return None;
    }
    ui.set_min_width(160.0);
    let mut chosen = None;
    for listed in &repo.branches {
        let current = listed.branch.current;
        if ui.selectable_label(current, &listed.branch.name).clicked() && !current {
            ui.close();
            chosen = Some(listed.branch.name.clone());
        }
    }
    chosen
}

/// The branch name, and the color its icon shares with it.
fn branch_text(head: &Head, palette: &Palette) -> (RichText, Color32) {
    let text = RichText::new(head.to_string());
    match head {
        Head::Branch(_) => (text.strong(), palette.text),
        Head::Unborn(_) => (text.italics().weak(), palette.text_muted),
        Head::Detached { .. } => (text.color(palette.warning), palette.warning),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn branch(count: u64, upstream: Upstream) -> Branch {
        Branch {
            name: "main".to_owned(),
            oid: None,
            committer_time: None,
            commit_count: count,
            upstream,
            current: true,
        }
    }

    #[test]
    fn metrics_describe_count_and_upstream() {
        assert_eq!(
            branch_metrics(&branch(0, Upstream::None)),
            "0 commits · no upstream"
        );
        assert_eq!(
            branch_metrics(&branch(1, Upstream::None)),
            "1 commit · no upstream"
        );
        assert_eq!(
            branch_metrics(&branch(
                12,
                Upstream::Tracking {
                    name: "origin/main".to_owned(),
                    remote: true,
                    ahead: 2,
                    behind: 1,
                },
            )),
            "12 commits · origin/main · 2 ahead, 1 behind"
        );
        assert_eq!(
            branch_metrics(&branch(
                4,
                Upstream::Tracking {
                    name: "origin/main".to_owned(),
                    remote: true,
                    ahead: 0,
                    behind: 0,
                },
            )),
            "4 commits · origin/main · in sync"
        );
        assert_eq!(
            branch_metrics(&branch(
                3,
                Upstream::Tracking {
                    name: "origin/main".to_owned(),
                    remote: true,
                    ahead: 2,
                    behind: 0,
                },
            )),
            "3 commits · origin/main · 2 ahead"
        );
        assert_eq!(
            branch_metrics(&branch(
                3,
                Upstream::Gone {
                    name: "origin/main".to_owned(),
                    remote: true,
                },
            )),
            "3 commits · origin/main · upstream gone"
        );
        assert_eq!(
            branch_metrics(&branch(
                3,
                Upstream::Gone {
                    name: String::new(),
                    remote: false,
                },
            )),
            "3 commits · upstream gone"
        );
    }

    fn listed(upstream: Upstream) -> ListedBranch {
        ListedBranch {
            branch: branch(1, upstream),
            checked: true,
        }
    }

    /// (head, the listed branch's upstream, fetch branch, pull, fetch hover,
    /// pull hover). Fetch needs a remote-tracking upstream; pull needs one
    /// that still exists, remote or local. An unborn branch is a branch. A
    /// head whose branch is not listed, or that is no branch, offers neither.
    #[test]
    fn sync_offer_fetches_a_remote_upstream_and_pulls_while_it_exists() {
        const FETCH: &str = "Fetch current branch";
        const LOCAL: &str = "The upstream is a local branch; there is nothing to fetch.";
        const PULL: &str = "Pull current branch (fast-forward only)";
        const GONE: &str = "The upstream branch is gone.";
        const NO_UPSTREAM: &str = "This branch has no upstream.";
        const NEEDS_UPSTREAM: &str = "Pull needs a branch with an upstream.";
        const DETACHED: &str = "Detached HEAD has no branch to fetch.";
        let main = || Head::Branch("main".to_owned());
        let tracking = |remote| {
            Some(Upstream::Tracking {
                name: "origin/main".to_owned(),
                remote,
                ahead: 0,
                behind: 1,
            })
        };
        let gone = |remote| {
            Some(Upstream::Gone {
                name: "origin/main".to_owned(),
                remote,
            })
        };
        let unborn = Head::Unborn("main".to_owned());
        let detached = Head::Detached {
            short_oid: "abc".to_owned(),
        };
        #[rustfmt::skip]
        let cases = [
            (main(), tracking(true), Some("main"), true, FETCH, PULL),
            (main(), gone(true), Some("main"), false, FETCH, GONE),
            (main(), tracking(false), None, true, LOCAL, PULL),
            (main(), gone(false), None, false, LOCAL, GONE),
            (unborn, tracking(true), Some("main"), true, FETCH, PULL),
            (main(), Some(Upstream::None), None, false, NO_UPSTREAM, NEEDS_UPSTREAM),
            (main(), None, None, false, NO_UPSTREAM, NEEDS_UPSTREAM),
            (detached, None, None, false, DETACHED, NEEDS_UPSTREAM),
        ];
        for (head, upstream, fetch, pull, fetch_hover, pull_hover) in cases {
            let branches: Vec<_> = upstream.clone().map(listed).into_iter().collect();
            let offer = sync_offer(&head, &branches);
            assert_eq!(
                (
                    offer.fetch_branch.as_deref(),
                    offer.pull,
                    offer.fetch_hover,
                    offer.pull_hover
                ),
                (fetch, pull, fetch_hover, pull_hover),
                "{head:?} with {upstream:?}"
            );
        }
    }
}
