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
    self, Align, Button, Color32, CursorIcon, FontSelection, Frame, Label, Layout, Margin, Pos2,
    Rect, RichText, ScrollArea, Sense, Stroke, Ui, Vec2, WidgetText, containers::menu::MenuButton,
};

use super::avatars::{self, AvatarService};
use super::changes;
use super::history;
use super::hover::{self, CachedCommit};
use super::icons::{self, Icon};
use super::terminal;
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

/// What the repository screen reads besides the repository itself.
pub struct ViewState<'a> {
    pub error: Option<&'a str>,
    pub branches_open: bool,
    pub detail_open: bool,
    pub terminal_open: bool,
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
    repo.diffs.poll(ui.ctx(), &repo.info.root);
    // The toggle buttons and dragging a sidebar edge past its minimum width
    // both flip these; the app saves the change.
    let branches_were_open = state.branches_open;
    let detail_was_open = state.detail_open;
    let terminal_was_open = state.terminal_open;
    let mut branches_open = state.branches_open;
    let mut detail_open = state.detail_open;
    let mut terminal_open = state.terminal_open;
    let syncing = state.syncing;
    let sidebar_default =
        (ui.available_width() * SIDEBAR_DEFAULT_SHARE).clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
    // Replies from the shell have to land even while the strip is hidden.
    terminal::service(ui.ctx(), &mut repo.terminal);

    // Top and bottom bars come first so they span the whole window width.
    egui::Panel::top("repo-top-bar")
        .frame(bar_frame(&palette))
        .show(ui, |ui| {
            top_bar(ui, repo, &palette, &mut branches_open, &mut detail_open);
        });

    egui::Panel::bottom("repo-bottom-bar")
        .frame(bar_frame(&palette))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if icons::icon_button(ui, Icon::House, palette.text, "Open another repository")
                    .clicked()
                {
                    actions.push(Action::Home);
                }
                let folder_tint = if state.busy {
                    palette.text_muted
                } else {
                    palette.text
                };
                if icons::icon_button_enabled(
                    ui,
                    Icon::Folder,
                    folder_tint,
                    "Open a folder",
                    !state.busy,
                )
                .clicked()
                {
                    actions.push(Action::Browse);
                }
                if icons::icon_button(ui, Icon::Settings, palette.text, "Settings").clicked() {
                    actions.push(Action::OpenSettings);
                }
                ui.separator();
                let (branch_label, branch_color) = branch_text(&repo.info.head, &palette);
                let offer = sync_offer(&repo.info.head, &repo.branches);
                let (fetch_on, pull_on, fetch_hover, pull_hover, branch_on, branch_hover) =
                    match syncing {
                        Some(SyncKind::Fetch) => {
                            (false, false, "Fetching…", "Fetching…", false, "Fetching…")
                        }
                        Some(SyncKind::Pull) => {
                            (false, false, "Pulling…", "Pulling…", false, "Pulling…")
                        }
                        Some(SyncKind::Checkout) => (
                            false,
                            false,
                            "Switching…",
                            "Switching…",
                            false,
                            "Switching…",
                        ),
                        None => (
                            offer.fetch_upstream.is_some(),
                            offer.pull,
                            offer.fetch_hover,
                            offer.pull_hover,
                            true,
                            "Switch branch",
                        ),
                    };
                let (terminal_tint, terminal_hover) = if terminal_open {
                    (palette.accent, "Hide terminal")
                } else {
                    (palette.text_muted, "Show terminal")
                };
                // The name truncates before fetch and pull, so a long branch
                // cannot push those buttons off the bar. The terminal stays
                // at the right end, and the spare width sits between them.
                let terminal_toggle = ui
                    .with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let toggle =
                            icons::icon_button(ui, Icon::Terminal, terminal_tint, terminal_hover);
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            let reserve = sync_control_width(ui, "Fetch")
                                + sync_control_width(ui, "Pull")
                                + ui.spacing().item_spacing.x;
                            let max_name = (ui.available_width() - reserve).max(0.0);
                            ui.scope(|ui| {
                                ui.set_max_width(max_name);
                                if let Some(name) = branch_menu(
                                    ui,
                                    repo,
                                    branch_label,
                                    branch_color,
                                    branch_on,
                                    branch_hover,
                                ) {
                                    actions.push(Action::Checkout(name));
                                }
                            });
                            if sync_labeled(
                                ui,
                                Icon::CloudDownload,
                                "Fetch",
                                fetch_on,
                                palette.text,
                                palette.text_muted,
                                fetch_hover,
                            ) && let Some(upstream) = offer.fetch_upstream.clone()
                            {
                                actions.push(Action::Fetch(upstream));
                            }
                            if sync_labeled(
                                ui,
                                Icon::ArrowDownToLine,
                                "Pull",
                                pull_on,
                                palette.text,
                                palette.text_muted,
                                pull_hover,
                            ) {
                                actions.push(Action::Pull);
                            }
                        });
                        toggle
                    })
                    .inner;
                if terminal_toggle.clicked() {
                    terminal_open = !terminal_open;
                    // The strip lives in the right sidebar, so showing it opens that too.
                    if terminal_open {
                        detail_open = true;
                    }
                }
            });
        });

    let side = side_frame(&palette);
    egui::Panel::left("repo-branches-sidebar")
        .resizable(true)
        .default_size(sidebar_default)
        .size_range(SIDEBAR_MIN_WIDTH..=SIDEBAR_MAX_WIDTH)
        .frame(side)
        .show_collapsible(ui, &mut branches_open, |ui| {
            branches_panel(ui, repo, &palette);
        });

    egui::Panel::right("repo-detail-sidebar")
        .resizable(true)
        .default_size(sidebar_default)
        .size_range(SIDEBAR_MIN_WIDTH..=SIDEBAR_MAX_WIDTH)
        .frame(side)
        .show_collapsible(ui, &mut detail_open, |ui| {
            detail_column(ui, repo, &palette, terminal_open, avatars);
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
            if history.reveal_detail && !detail_open {
                detail_open = true;
            }
        });

    if branches_open != branches_were_open {
        actions.push(Action::ToggleBranchesSidebar);
    }
    if detail_open != detail_was_open && !(terminal_open && !terminal_was_open) {
        actions.push(Action::ToggleDetailSidebar);
    }
    close_changes_for_other_commit(ui.ctx(), repo);
    if let Some(mut changes) = repo.changes.take() {
        let close = changes::show(
            ui.ctx(),
            &mut changes,
            &mut repo.diffs,
            &repo.info.root,
            &palette,
        );
        if !close {
            repo.changes = Some(changes);
        }
    }

    if terminal_open != terminal_was_open {
        actions.push(Action::ToggleTerminal);
    }
    if !terminal_open || !detail_open {
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
            true,
        );
    });
}

fn bar_frame(palette: &Palette) -> Frame {
    Frame::new()
        .fill(palette.surface)
        .stroke(Stroke::new(1.0, palette.border))
        .inner_margin(Margin::symmetric(12, 6))
}

fn side_frame(palette: &Palette) -> Frame {
    Frame::new()
        .fill(palette.background)
        .inner_margin(Margin::same(12))
}

/// The branch toggle on the left, the detail toggle on the right, and the name
/// centered on the window. The name is truncated before it would reach either
/// toggle.
fn top_bar(
    ui: &mut Ui,
    repo: &OpenedRepo,
    palette: &Palette,
    branches_open: &mut bool,
    detail_open: &mut bool,
) {
    ui.horizontal(|ui| {
        let (branches_tint, branches_hover) = if *branches_open {
            (palette.accent, "Hide branches")
        } else {
            (palette.text_muted, "Show branches")
        };
        let branches_toggle =
            icons::icon_button(ui, Icon::PanelLeft, branches_tint, branches_hover);
        if branches_toggle.clicked() {
            *branches_open = !*branches_open;
        }

        let (detail_tint, detail_hover) = if *detail_open {
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
            *detail_open = !*detail_open;
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
            Label::new(
                RichText::new(&repo.info.name)
                    .size(15.0)
                    .strong()
                    .color(palette.text),
            )
            .truncate(),
        )
        .on_hover_text(repo.info.root.display().to_string());
    });
}

fn branches_panel(ui: &mut Ui, repo: &mut OpenedRepo, palette: &Palette) {
    ui.add(
        Label::new(
            RichText::new(format!("Branches ({})", repo.branches.len()))
                .strong()
                .color(palette.text),
        )
        .truncate(),
    );
    ui.add_space(6.0);

    if repo.branches.is_empty() {
        ui.label(RichText::new("No branches.").color(palette.text_muted));
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
            ui.add(egui::Checkbox::without_text(&mut listed.checked));
            let (dot, _) = ui.allocate_exact_size(Vec2::splat(LANE_DOT), Sense::hover());
            if let Some(color) = lane {
                ui.painter().circle_filled(
                    dot.center(),
                    LANE_DOT / 2.0,
                    palette.graph_color(color),
                );
            }
            let mut name = RichText::new(&listed.branch.name).color(palette.text);
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
            if response.clicked() {
                listed.checked = !listed.checked;
            }
        });

        let indent = ui.spacing().icon_width + LANE_DOT + ui.spacing().item_spacing.x * 2.0;
        ui.horizontal(|ui| {
            ui.add_space(indent);
            ui.add(
                Label::new(
                    RichText::new(branch_metrics(&listed.branch))
                        .small()
                        .color(palette.text_muted),
                )
                .truncate(),
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
        Upstream::Gone { name } if name.is_empty() => format!("{commits} · upstream gone"),
        Upstream::Gone { name } => format!("{commits} · {name} · upstream gone"),
        Upstream::Tracking {
            name,
            ahead,
            behind,
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
    let selected = history::selected_commit(ui.ctx(), repo.view_id);
    let title = if selected.is_some() {
        "Selected commit"
    } else {
        "Latest commit"
    };
    ui.add(Label::new(RichText::new(title).strong().color(palette.text)).truncate());
    ui.add_space(6.0);

    if selected.is_none() && repo.latest.is_none() {
        ui.label(RichText::new("No commits yet.").color(palette.text_muted));
        return;
    }
    if let Some(oid) = selected
        .as_deref()
        .or(repo.latest.as_ref().map(|commit| commit.oid.as_str()))
    {
        repo.hover.request(oid);
    }

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
        && let Some(oid) = selected
            .as_deref()
            .or(repo.latest.as_ref().map(|commit| commit.oid.as_str()))
        && let Some(CachedCommit::Ready(hover)) = repo.hover.cached(oid)
        && !hover.files.is_empty()
    {
        repo.changes = Some(changes::Changes::new(oid.to_owned(), hover.files.clone()));
    }
}

/// Drops the diff modal when the sidebar moves to a different commit.
fn close_changes_for_other_commit(ctx: &egui::Context, repo: &mut OpenedRepo) {
    let shown = history::selected_commit(ctx, repo.view_id)
        .or_else(|| repo.latest.as_ref().map(|commit| commit.oid.clone()));
    if repo
        .changes
        .as_ref()
        .is_some_and(|changes| Some(changes.oid.as_str()) != shown.as_deref())
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
        ui.label(RichText::new("Author").small().color(palette.text_muted));
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
            ui.label(
                RichText::new("Co-authors")
                    .small()
                    .color(palette.text_muted),
            );
            ui.add_space(2.0);
            for person in body.co_authors {
                person_line(ui, palette, avatars, person, context.as_ref(), None);
            }
            ui.add_space(10.0);
        }
    }
    ui.label(RichText::new("Date").small().color(palette.text_muted));
    ui.add_space(2.0);
    match body.date {
        DetailDate::Known { absolute, unix } => {
            let text = match unix {
                Some(unix) => {
                    format!(
                        "{} · {absolute}",
                        hover::relative_time(hover::now_unix(), unix)
                    )
                }
                None => absolute.to_owned(),
            };
            sidebar_text(ui, RichText::new(text).color(palette.text));
        }
        DetailDate::Pending => {
            ui.label(RichText::new("Loading…").color(palette.text_muted));
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
    ui.label(RichText::new("Message").small().color(palette.text_muted));
    ui.add_space(2.0);
    match body.message {
        Some("") => {
            ui.label(RichText::new("No message.").color(palette.text_muted));
        }
        Some(message) => hover::message_with_links(ui, message, github, palette),
        None => {
            ui.label(RichText::new("Loading…").color(palette.text_muted));
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
    let button = Button::new(RichText::new("View changes").color(palette.text))
        .fill(palette.surface)
        .stroke(Stroke::new(1.0, palette.border))
        .corner_radius(widgets::CORNER_RADIUS);
    ui.add(button).on_hover_cursor(CursorIcon::PointingHand)
}

fn hash_block(ui: &mut Ui, palette: &Palette, oid: &str, github: Option<&GitHubRepo>) {
    ui.label(RichText::new("Hash").small().color(palette.text_muted));
    ui.add_space(2.0);
    let hash = place_sidebar_text(ui, RichText::new(oid).monospace().color(palette.hash), true)
        .on_hover_text("Copy the full commit hash")
        .on_hover_cursor(CursorIcon::PointingHand);
    if hash.clicked() {
        widgets::copy_commit_hash(ui.ctx(), oid);
    }
    if let Some(github) = github {
        ui.add_space(4.0);
        ui.hyperlink_to("Open on GitHub", github.commit_url(oid));
    }
    ui.add_space(10.0);
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
                sidebar_text(ui, RichText::new(&person.name).color(palette.text));
            }
            hover::email_link(ui, &person.email, palette);
        });
    });
}

fn sidebar_text(ui: &mut Ui, text: impl Into<WidgetText>) {
    let _ = place_sidebar_text(ui, text, false);
}

/// Draws `text` inside the width this sidebar was given.
///
/// A commit hash and a name with an address are single tokens. Letting either extend
/// grows the sidebar up to the remaining window and leaves the history column
/// with no room. Breaking inside the token keeps the whole string visible.
///
/// `clickable` makes the line a click target and turns text selection off, so
/// the click is the action.
fn place_sidebar_text(ui: &mut Ui, text: impl Into<WidgetText>, clickable: bool) -> egui::Response {
    let width = ui.available_width().max(0.0);
    let mut job = std::sync::Arc::unwrap_or_clone(text.into().into_layout_job(
        ui.style(),
        FontSelection::Default,
        ui.text_valign(),
    ));
    job.wrap.max_width = width;
    job.wrap.break_anywhere = true;
    job.wrap.max_rows = usize::MAX;
    job.wrap.overflow_character = None;
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    let mut label = Label::new(galley);
    if clickable {
        label = label.selectable(false).sense(Sense::click());
    }
    ui.add(label)
}

struct SyncOffer {
    fetch_upstream: Option<String>,
    pull: bool,
    fetch_hover: &'static str,
    pull_hover: &'static str,
}

fn unavailable(fetch_hover: &'static str, pull_hover: &'static str) -> SyncOffer {
    SyncOffer {
        fetch_upstream: None,
        pull: false,
        fetch_hover,
        pull_hover,
    }
}

/// Fetch when the current branch has an upstream name git can be given.
/// Pull only while that upstream is still tracking.
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
    match &listed.branch.upstream {
        Upstream::Tracking { name, .. } => {
            let fetchable = cthulhu_git::git::fetch_request(name).is_some();
            SyncOffer {
                fetch_upstream: fetchable.then_some(name.clone()),
                pull: true,
                fetch_hover: if fetchable {
                    "Fetch current branch"
                } else {
                    "This branch has no upstream."
                },
                pull_hover: "Pull current branch (fast-forward only)",
            }
        }
        Upstream::Gone { name } => {
            let fetchable = cthulhu_git::git::fetch_request(name).is_some();
            SyncOffer {
                fetch_upstream: fetchable.then_some(name.clone()),
                pull: false,
                fetch_hover: if fetchable {
                    "Fetch current branch"
                } else {
                    "This branch has no upstream."
                },
                pull_hover: "The upstream branch is gone.",
            }
        }
        Upstream::None => unavailable(
            "This branch has no upstream.",
            "Pull needs a branch with an upstream.",
        ),
    }
}

/// Width of an icon-and-word control, so the branch name stops before it.
fn sync_control_width(ui: &Ui, text: &str) -> f32 {
    icons::icon_text_button_width(ui, text)
}

/// Icon and word, one click. Dimmed, and not clickable, when the action cannot run.
fn sync_labeled(
    ui: &mut Ui,
    icon: Icon,
    word: &str,
    enabled: bool,
    tint: Color32,
    muted: Color32,
    hover: &str,
) -> bool {
    let color = if enabled { tint } else { muted };
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
        Head::Branch(_) => (text.strong().color(palette.text), palette.text),
        Head::Unborn(_) => (text.italics().color(palette.text_muted), palette.text_muted),
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
                },
            )),
            "3 commits · origin/main · upstream gone"
        );
        assert_eq!(
            branch_metrics(&branch(
                3,
                Upstream::Gone {
                    name: String::new(),
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

    #[test]
    fn sync_offer_fetches_and_pulls_a_tracking_branch() {
        let offer = sync_offer(
            &Head::Branch("main".to_owned()),
            &[listed(Upstream::Tracking {
                name: "origin/main".to_owned(),
                ahead: 0,
                behind: 1,
            })],
        );
        assert_eq!(offer.fetch_upstream.as_deref(), Some("origin/main"));
        assert!(offer.pull);
        assert_eq!(offer.fetch_hover, "Fetch current branch");
        assert_eq!(offer.pull_hover, "Pull current branch (fast-forward only)");
    }

    #[test]
    fn sync_offer_fetches_a_gone_upstream_without_pulling() {
        let offer = sync_offer(
            &Head::Branch("main".to_owned()),
            &[listed(Upstream::Gone {
                name: "origin/main".to_owned(),
            })],
        );
        assert_eq!(offer.fetch_upstream.as_deref(), Some("origin/main"));
        assert!(!offer.pull);
        assert_eq!(offer.pull_hover, "The upstream branch is gone.");
    }

    #[test]
    fn sync_offer_disables_both_without_an_upstream_or_a_branch() {
        let none = sync_offer(&Head::Branch("main".to_owned()), &[listed(Upstream::None)]);
        assert!(none.fetch_upstream.is_none());
        assert!(!none.pull);

        let detached = sync_offer(
            &Head::Detached {
                short_oid: "abc".to_owned(),
            },
            &[],
        );
        assert!(detached.fetch_upstream.is_none());
        assert!(!detached.pull);
        assert_eq!(
            detached.fetch_hover,
            "Detached HEAD has no branch to fetch."
        );
    }
}
