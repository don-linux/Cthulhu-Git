//! Repository screen. A top bar with the branch and detail toggles and the
//! repository name, a sidebar of local branches, the commit graph in the
//! middle, the latest commit on the right with the terminal beneath it, and a
//! bottom bar with the Home and Settings buttons, the current branch with
//! fetch and pull, and the terminal toggle at the right end.

use cthulhu_git::git::{Branch, Commit, GraphBranch, GraphRow, Head, Upstream};
use eframe::egui::text::{LayoutJob, TextFormat, TextWrapping};
use eframe::egui::{
    self, Align, Color32, CursorIcon, Frame, Label, Layout, Margin, Pos2, Rect, RichText,
    ScrollArea, Sense, Stroke, TextStyle, Ui, Vec2,
};

use super::icons::{self, Icon};
use super::terminal;
use super::theme::{self, Palette};
use super::widgets;
use super::{Action, ListedBranch, OpenedRepo, SyncKind};

const LANE_DOT: f32 = 8.0;
/// Commit text and branch labels, relative to egui's body size.
const COMMIT_TEXT_SCALE: f32 = 1.15;
const BRANCH_COLUMN_RATIO: f32 = 0.20;
const GRAPH_BUDGET_RATIO: f32 = 0.30;
const BRANCH_COLUMN_MIN: f32 = 72.0;
const BRANCH_COLUMN_MAX: f32 = 220.0;
const LANE_MIN: f32 = 10.0;
const LANE_MAX: f32 = 20.0;
const GAP_AFTER_BRANCH: f32 = 8.0;
const GAP_AFTER_GRAPH: f32 = 16.0;
const LANE_STROKE: f32 = 2.5;
const DOT_OUTER: f32 = 6.5;
const DOT_INNER: f32 = 4.5;
const CONNECTOR_STROKE: f32 = 1.5;
const CHIP_ICON: f32 = 14.0;
const CHIP_PAD: f32 = 4.0;
const ROW_PAD: f32 = 12.0;
const ROW_MIN: f32 = 26.0;
/// Room kept for `hash - message` before the branch column gives up its minimum.
const TEXT_PREFERRED: f32 = 140.0;
const SIDEBAR_DEFAULT_WIDTH: f32 = 320.0;
const SIDEBAR_MIN_WIDTH: f32 = 220.0;
const SIDEBAR_MAX_WIDTH: f32 = 560.0;

pub fn show(
    ui: &mut Ui,
    repo: &mut OpenedRepo,
    error: Option<&str>,
    branches_open: bool,
    detail_open: bool,
    terminal_open: bool,
    syncing: Option<SyncKind>,
) -> Vec<Action> {
    let palette = theme::current(ui.ctx()).palette;
    let mut actions = Vec::new();
    // The toggle buttons and dragging a sidebar edge past its minimum width
    // both flip these; the app saves the change.
    let branches_were_open = branches_open;
    let detail_was_open = detail_open;
    let terminal_was_open = terminal_open;
    let mut branches_open = branches_open;
    let mut detail_open = detail_open;
    let mut terminal_open = terminal_open;
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
                if icons::icon_button(ui, Icon::Settings, palette.text, "Settings").clicked() {
                    actions.push(Action::OpenSettings);
                }
                ui.separator();
                let (branch, color) = branch_text(&repo.info.head, &palette);
                icons::icon(ui, Icon::GitBranch, color).on_hover_text("Current branch");
                let offer = sync_offer(&repo.info.head, &repo.branches);
                let (fetch_on, pull_on, fetch_hover, pull_hover) = match syncing {
                    Some(SyncKind::Fetch) => (false, false, "Fetching…", "Fetching…"),
                    Some(SyncKind::Pull) => (false, false, "Pulling…", "Pulling…"),
                    None => (
                        offer.fetch_upstream.is_some(),
                        offer.pull,
                        offer.fetch_hover,
                        offer.pull_hover,
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
                                ui.add(Label::new(branch).truncate());
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
        .default_size(SIDEBAR_DEFAULT_WIDTH)
        .size_range(SIDEBAR_MIN_WIDTH..=SIDEBAR_MAX_WIDTH)
        .frame(side)
        .show_collapsible(ui, &mut branches_open, |ui| {
            branches_panel(ui, repo, &palette);
        });

    egui::Panel::right("repo-detail-sidebar")
        .resizable(true)
        .default_size(SIDEBAR_DEFAULT_WIDTH)
        .size_range(SIDEBAR_MIN_WIDTH..=SIDEBAR_MAX_WIDTH)
        .frame(side)
        .show_collapsible(ui, &mut detail_open, |ui| {
            detail_column(ui, repo, &palette, terminal_open);
        });

    egui::CentralPanel::default()
        .frame(side_frame(&palette))
        .show(ui, |ui| {
            if let Some(error) = error {
                widgets::error_banner(ui, error);
                ui.add_space(12.0);
            }
            history(ui, repo, &palette);
        });

    if branches_open != branches_were_open {
        actions.push(Action::ToggleBranchesSidebar);
    }
    if detail_open != detail_was_open && !(terminal_open && !terminal_was_open) {
        actions.push(Action::ToggleDetailSidebar);
    }
    if terminal_open != terminal_was_open {
        actions.push(Action::ToggleTerminal);
    }
    if !terminal_open || !detail_open {
        terminal::surrender_focus(ui.ctx(), repo.view_id);
    }
    actions
}

fn detail_column(ui: &mut Ui, repo: &mut OpenedRepo, palette: &Palette, terminal_open: bool) {
    if !terminal_open {
        detail_panel(ui, repo, palette);
        return;
    }

    let available = ui.available_height();
    let (detail_h, splitter_h, terminal_h) = terminal::split_height(ui, repo.view_id, available);
    ui.allocate_ui(Vec2::new(ui.available_width(), detail_h), |ui| {
        ui.set_min_height(detail_h);
        detail_panel(ui, repo, palette);
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

fn detail_panel(ui: &mut Ui, repo: &OpenedRepo, palette: &Palette) {
    ui.add(Label::new(RichText::new("Latest commit").strong().color(palette.text)).truncate());
    ui.add_space(6.0);

    let Some(commit) = &repo.latest else {
        ui.label(RichText::new("No commits yet.").color(palette.text_muted));
        return;
    };

    ScrollArea::vertical()
        .id_salt(repo.view_id.with("detail"))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            detail_field(
                ui,
                palette,
                "Hash",
                RichText::new(&commit.oid).monospace().color(palette.hash),
            );
            let author = format!("{} <{}>", commit.author_name, commit.author_email);
            detail_field(
                ui,
                palette,
                "Author",
                RichText::new(author).color(palette.text),
            );
            detail_field(
                ui,
                palette,
                "Date",
                RichText::new(&commit.authored_at).color(palette.text),
            );
            ui.label(RichText::new("Message").small().color(palette.text_muted));
            ui.add_space(2.0);
            if commit.message.is_empty() {
                ui.label(RichText::new("No message.").color(palette.text_muted));
            } else {
                ui.label(RichText::new(&commit.message).color(palette.text));
            }
        });
}

fn detail_field(ui: &mut Ui, palette: &Palette, label: &str, value: RichText) {
    ui.label(RichText::new(label).small().color(palette.text_muted));
    ui.add_space(2.0);
    ui.add(Label::new(value));
    ui.add_space(10.0);
}

fn history(ui: &mut Ui, repo: &OpenedRepo, palette: &Palette) {
    let rows = &repo.graph.rows;
    let count = if repo.graph.truncated {
        format!("{}+", rows.len())
    } else {
        rows.len().to_string()
    };

    ui.add(
        Label::new(
            RichText::new(format!("Commit history ({count})"))
                .strong()
                .color(palette.text),
        )
        .truncate(),
    );
    ui.add_space(6.0);

    if rows.is_empty() {
        ui.label(RichText::new(empty_history(repo)).color(palette.text_muted));
        return;
    }
    if repo.graph.truncated {
        ui.label(
            RichText::new(format!("Showing the latest {} commits.", rows.len()))
                .color(palette.text_muted),
        );
        ui.add_space(4.0);
    }
    // Measured from the panel on screen, so opening or dragging a sidebar
    // reflows the columns instead of leaving a fixed graph.
    let columns = repo.graph.columns;
    history_header(ui, columns, palette);
    ui.add_space(4.0);
    // Rows touch, so a lane drawn to the bottom of one meets the top of the next.
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let line_height = widgets::commit_line_height(ui, COMMIT_TEXT_SCALE);
        let row_height = (line_height + ROW_PAD).max(ROW_MIN);
        ScrollArea::vertical()
            .id_salt(repo.view_id.with("history"))
            .auto_shrink([false, false])
            .show_rows(ui, row_height, rows.len(), |ui, range| {
                let layout = history_layout(ui.available_width(), columns);
                for row in &rows[range] {
                    graph_row(ui, row, &layout, row_height, palette);
                }
            });
    });
}

/// Branch names, the graph, and the commit text, in that order. Widths come
/// from the panel so the same row fits a wide window and the minimum one.
struct HistoryLayout {
    branch_width: f32,
    lane_width: f32,
    graph_width: f32,
    gap_branch: f32,
    gap_graph: f32,
    text_width: f32,
}

fn history_layout(width: f32, columns: usize) -> HistoryLayout {
    let columns = columns.max(1);
    let width = width.max(0.0);
    let cols = columns as f32;
    let mut gap_branch = GAP_AFTER_BRANCH;
    let mut gap_graph = GAP_AFTER_GRAPH;
    let mut branch = (width * BRANCH_COLUMN_RATIO).clamp(BRANCH_COLUMN_MIN, BRANCH_COLUMN_MAX);
    // The graph's share of the panel, split across the lanes. A lane stays
    // between 10 and 20 px; extra lanes shrink toward 10 before the commit
    // text gives up space.
    let mut lane = ((width * GRAPH_BUDGET_RATIO) / cols).clamp(LANE_MIN, LANE_MAX);

    let total = |branch: f32, lane: f32, gap_branch: f32, gap_graph: f32| {
        branch + lane * cols + gap_branch + gap_graph
    };
    // A narrow center (both sidebars open) would otherwise spend its minimum
    // branch column on names and truncate the hash. Names shrink first.
    let text_now = width - total(branch, lane, gap_branch, gap_graph);
    if text_now < TEXT_PREFERRED {
        branch = (width - lane * cols - gap_branch - gap_graph - TEXT_PREFERRED).max(0.0);
        let after = width - total(branch, lane, gap_branch, gap_graph);
        if after < TEXT_PREFERRED && lane > LANE_MIN {
            let need = TEXT_PREFERRED - after;
            let spare = (lane - LANE_MIN) * cols;
            lane -= need.min(spare) / cols;
        }
    }
    if total(branch, lane, gap_branch, gap_graph) > width {
        let room = (width - branch - gap_branch - gap_graph).max(0.0);
        lane = (room / cols).min(lane);
    }
    if lane < LANE_MIN {
        lane = LANE_MIN;
        if total(branch, lane, gap_branch, gap_graph) > width {
            branch = (width - lane * cols - gap_branch - gap_graph).max(0.0);
        }
    }
    if total(branch, lane, gap_branch, gap_graph) > width {
        let room = (width - branch - gap_branch - gap_graph).max(0.0);
        lane = room / cols;
    }
    if total(branch, lane, gap_branch, gap_graph) > width {
        let overflow = total(branch, lane, gap_branch, gap_graph) - width;
        let gaps = gap_branch + gap_graph;
        if gaps > 0.0 {
            let keep = ((gaps - overflow) / gaps).max(0.0);
            gap_branch *= keep;
            gap_graph *= keep;
        }
    }
    if total(branch, lane, gap_branch, gap_graph) > width {
        let room = (width - branch - gap_branch - gap_graph).max(0.0);
        lane = room / cols;
    }

    let graph = lane * cols;
    HistoryLayout {
        branch_width: branch,
        lane_width: lane,
        graph_width: graph,
        gap_branch,
        gap_graph,
        text_width: (width - branch - graph - gap_branch - gap_graph).max(0.0),
    }
}

fn history_header(ui: &mut Ui, columns: usize, palette: &Palette) {
    let layout = history_layout(ui.available_width(), columns);
    let width = ui.available_width();
    let height = ui.text_style_height(&TextStyle::Small);
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
        let label = |text: &str| RichText::new(text).small().color(palette.text_muted);
        let branch_rect = Rect::from_min_size(
            rect.min,
            Vec2::new(layout.branch_width.min(rect.width()), height),
        );
        let graph_x = (rect.left() + layout.branch_width + layout.gap_branch).min(rect.right());
        // The word may use the gap after the lanes. One lane is narrower than
        // the word, and the commit column still starts after that gap.
        let graph_span = (layout.graph_width + layout.gap_graph).min(rect.right() - graph_x);
        let graph_rect = Rect::from_min_size(
            Pos2::new(graph_x, rect.top()),
            Vec2::new(graph_span, height),
        );
        let text_rect = Rect::from_min_max(
            Pos2::new(graph_rect.right().min(rect.right()), rect.top()),
            rect.max,
        );
        ui.put(branch_rect, Label::new(label("Branch")).truncate());
        ui.put(graph_rect, Label::new(label("Graph")).truncate());
        ui.put(text_rect, Label::new(label("Commit")).truncate());
    });
}

fn empty_history(repo: &OpenedRepo) -> &'static str {
    let selectable = repo
        .branches
        .iter()
        .any(|listed| listed.branch.oid.is_some());
    let selected = repo
        .branches
        .iter()
        .any(|listed| listed.checked && listed.branch.oid.is_some())
        || repo.detached_oid.is_some();
    if selectable && !selected {
        "No branches selected."
    } else {
        "No commits yet."
    }
}

fn graph_row(
    ui: &mut Ui,
    row: &GraphRow,
    layout: &HistoryLayout,
    row_height: f32,
    palette: &Palette,
) {
    let width = ui.available_width();
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        ui.allocate_ui_with_layout(
            Vec2::new(width, row_height),
            Layout::left_to_right(Align::Center).with_main_wrap(false),
            |ui| {
                let (branch_rect, _) = ui.allocate_exact_size(
                    Vec2::new(layout.branch_width, row_height),
                    Sense::hover(),
                );
                let _ = ui
                    .allocate_exact_size(Vec2::new(layout.gap_branch, row_height), Sense::hover());
                let (graph_rect, _) = ui
                    .allocate_exact_size(Vec2::new(layout.graph_width, row_height), Sense::hover());
                let _ =
                    ui.allocate_exact_size(Vec2::new(layout.gap_graph, row_height), Sense::hover());

                let chip = branch_chip(ui, branch_rect, row, palette);
                // The connector goes under the lanes, so a vertical stroke
                // stays continuous where the label's line crosses it.
                {
                    let painter = ui.painter();
                    if let Some(chip) = &chip {
                        let dot_x = lane_center(graph_rect, row.commit_lane, layout.lane_width);
                        let y = graph_rect.center().y;
                        if dot_x - chip.rect.right() > 0.5 {
                            painter.line_segment(
                                [Pos2::new(chip.rect.right(), y), Pos2::new(dot_x, y)],
                                Stroke::new(CONNECTOR_STROKE, palette.graph_connector(chip.color)),
                            );
                        }
                    }
                    paint_lanes(painter, graph_rect, row, palette, layout.lane_width);
                }
                if let Some(chip) = &chip {
                    paint_branch_chip(ui, chip, palette);
                }
                let commit = Commit {
                    oid: row.oid.clone(),
                    summary: row.summary.clone(),
                };
                ui.allocate_ui_with_layout(
                    Vec2::new(layout.text_width, row_height),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.add(
                            Label::new(widgets::commit_line(
                                &commit,
                                palette,
                                ui.style(),
                                COMMIT_TEXT_SCALE,
                            ))
                            .truncate(),
                        );
                    },
                );
            },
        );
    });
}

fn paint_lanes(
    painter: &egui::Painter,
    rect: Rect,
    row: &GraphRow,
    palette: &Palette,
    lane_width: f32,
) {
    let dot = Pos2::new(
        lane_center(rect, row.commit_lane, lane_width),
        rect.center().y,
    );
    for trace in &row.traces {
        let start = match trace.from_lane {
            Some(lane) => Pos2::new(lane_center(rect, lane, lane_width), rect.top()),
            None => dot,
        };
        let end = match trace.to_lane {
            Some(lane) => Pos2::new(lane_center(rect, lane, lane_width), rect.bottom()),
            None => dot,
        };
        if start.distance(end) < 0.5 {
            continue;
        }
        painter.line_segment(
            [start, end],
            Stroke::new(LANE_STROKE, palette.graph_color(trace.color)),
        );
    }
    painter.circle_filled(dot, DOT_OUTER, palette.background);
    painter.circle_filled(dot, DOT_INNER, palette.graph_color(row.commit_color));
}

fn lane_center(rect: Rect, lane: usize, lane_width: f32) -> f32 {
    rect.left() + lane as f32 * lane_width + lane_width / 2.0
}

/// Checked-out branch first, then the sidebar order the row already has.
fn ordered_branches(branches: &[GraphBranch]) -> Vec<&GraphBranch> {
    let mut order: Vec<_> = branches.iter().enumerate().collect();
    order.sort_by_key(|(index, branch)| (!branch.current, *index));
    order.into_iter().map(|(_, branch)| branch).collect()
}

struct BranchChip {
    rect: Rect,
    color: usize,
    current: bool,
    tooltip: String,
    oid: String,
    text: std::sync::Arc<egui::Galley>,
    plus: Option<std::sync::Arc<egui::Galley>>,
}

/// The label sits at the right of the branch column, against the graph.
/// Several tips on one commit show the first name and a `+N`; the tooltip
/// lists every name. The text ellipsizes to the column instead of a fixed
/// character count.
fn branch_chip(ui: &Ui, column: Rect, row: &GraphRow, palette: &Palette) -> Option<BranchChip> {
    let ordered = ordered_branches(&row.branches);
    let primary = *ordered.first()?;
    let color = palette.graph_color(primary.color);
    let mut font = TextStyle::Body.resolve(ui.style());
    font.size *= COMMIT_TEXT_SCALE;

    let extra = ordered.len().saturating_sub(1);
    let plus = if extra > 0 {
        Some(
            ui.painter()
                .layout_no_wrap(format!("+{extra}"), font.clone(), color),
        )
    } else {
        None
    };
    let plus_width = plus.as_ref().map(|galley| galley.size().x).unwrap_or(0.0);
    let plus_gap = if plus.is_some() { CHIP_PAD } else { 0.0 };
    let chrome = CHIP_PAD + CHIP_ICON + CHIP_PAD + plus_gap + plus_width + CHIP_PAD;
    let text_width = (column.width() - chrome).max(0.0);

    let mut job = LayoutJob::single_section(
        primary.name.clone(),
        TextFormat {
            font_id: font,
            color,
            valign: Align::Center,
            ..TextFormat::default()
        },
    );
    job.wrap = TextWrapping {
        max_width: text_width,
        max_rows: 1,
        // Branch names are one token (`feature/alertas-ritual`), so the
        // ellipsis has to be allowed to land inside the word.
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let text = ui.painter().layout_job(job);
    let chip_width = (chrome + text.size().x).min(column.width());
    let chip_height = (text.size().y.max(CHIP_ICON) + CHIP_PAD).min(column.height());
    let rect = Rect::from_min_size(
        Pos2::new(
            column.right() - chip_width,
            column.center().y - chip_height / 2.0,
        ),
        Vec2::new(chip_width, chip_height),
    );
    let tooltip = ordered
        .iter()
        .map(|branch| branch.name.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    Some(BranchChip {
        rect,
        color: primary.color,
        current: primary.current,
        tooltip,
        oid: row.oid.clone(),
        text,
        plus,
    })
}

fn paint_branch_chip(ui: &mut Ui, chip: &BranchChip, palette: &Palette) {
    let color = palette.graph_color(chip.color);
    let icon_rect = Rect::from_center_size(
        Pos2::new(
            chip.rect.left() + CHIP_PAD + CHIP_ICON / 2.0,
            chip.rect.center().y,
        ),
        Vec2::splat(CHIP_ICON),
    );
    {
        let painter = ui.painter();
        painter.rect_filled(chip.rect, 4.0, palette.graph_chip(chip.color, chip.current));
        icons::paint_icon(painter, ui.ctx(), icon_rect, Icon::GitBranch, color);
        let text_x = icon_rect.right() + CHIP_PAD;
        painter.galley(
            Pos2::new(text_x, chip.rect.center().y - chip.text.size().y / 2.0),
            chip.text.clone(),
            color,
        );
        if let Some(plus) = &chip.plus {
            painter.galley(
                Pos2::new(
                    chip.rect.right() - CHIP_PAD - plus.size().x,
                    chip.rect.center().y - plus.size().y / 2.0,
                ),
                plus.clone(),
                color,
            );
        }
    }
    let response = ui.interact(
        chip.rect,
        ui.id().with(("branch-label", chip.oid.as_str())),
        Sense::hover(),
    );
    // Painted text is invisible to accesskit unless the widget carries it.
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Label, true, chip.tooltip.as_str())
    });
    response.on_hover_text(&chip.tooltip);
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
    let font = eframe::egui::TextStyle::Button.resolve(ui.style());
    let text_width = ui.fonts_mut(|fonts| {
        fonts
            .layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER)
            .rect
            .width()
    });
    text_width + 18.0 + ui.spacing().icon_spacing + ui.spacing().button_padding.x * 2.0
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

    #[test]
    fn history_layout_fills_the_width_without_negative_columns() {
        let cases = [
            (1200.0, 1),
            (600.0, 1),
            (600.0, 10),
            (600.0, 40),
            (320.0, 3),
            (80.0, 1),
            (80.0, 12),
            (0.0, 0),
        ];
        for (width, columns) in cases {
            let layout = history_layout(width, columns);
            let sum = layout.branch_width
                + layout.graph_width
                + layout.gap_branch
                + layout.gap_graph
                + layout.text_width;
            assert!(
                (sum - width).abs() < 0.05,
                "width {width} columns {columns} summed to {sum}"
            );
            for (name, value) in [
                ("branch", layout.branch_width),
                ("lane", layout.lane_width),
                ("graph", layout.graph_width),
                ("text", layout.text_width),
                ("gap_branch", layout.gap_branch),
                ("gap_graph", layout.gap_graph),
            ] {
                assert!(value.is_finite() && value >= 0.0, "{name} = {value}");
            }
        }

        let wide = history_layout(1200.0, 1);
        assert!((wide.branch_width - 220.0).abs() < 0.01);
        assert!((wide.lane_width - 20.0).abs() < 0.01);
        assert!((wide.gap_branch - 8.0).abs() < 0.01);
        assert!((wide.gap_graph - 16.0).abs() < 0.01);
        assert!(wide.text_width > 0.0);

        let many = history_layout(600.0, 40);
        assert!((many.lane_width - 10.0).abs() < 0.01);
        assert!(many.text_width + 0.01 >= TEXT_PREFERRED);

        // Both sidebars open in the default window: the hash still fits.
        let cramped = history_layout(196.0, 1);
        assert!(cramped.text_width + 0.01 >= TEXT_PREFERRED);
        assert!((cramped.lane_width - 20.0).abs() < 0.01);
        assert!(cramped.branch_width < BRANCH_COLUMN_MIN);
    }

    #[test]
    fn the_checked_out_branch_is_labeled_before_the_others() {
        let branches = [
            GraphBranch {
                name: "feature".to_owned(),
                color: 1,
                current: false,
            },
            GraphBranch {
                name: "main".to_owned(),
                color: 0,
                current: true,
            },
            GraphBranch {
                name: "topic".to_owned(),
                color: 2,
                current: false,
            },
        ];
        let ordered = ordered_branches(&branches);
        assert_eq!(
            ordered
                .iter()
                .map(|branch| branch.name.as_str())
                .collect::<Vec<_>>(),
            ["main", "feature", "topic"]
        );
    }
}
