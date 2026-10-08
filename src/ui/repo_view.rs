//! Repository screen. A top bar with the branch and detail toggles and the
//! repository name, a sidebar of local branches, the commit history in the
//! middle, the latest commit on the right with the terminal beneath it, and a
//! bottom bar with the Home button, the current branch, and the terminal
//! toggle at the right end.

use cthulhu_git::git::{Branch, Head, Upstream};
use eframe::egui::{
    self, Align, Color32, CursorIcon, Frame, Label, Layout, Margin, Pos2, Rect, RichText,
    ScrollArea, Sense, Stroke, Ui, Vec2,
};

use super::icons::{self, Icon};
use super::terminal;
use super::theme::{self, Palette};
use super::widgets;
use super::{Action, ListedBranch, OpenedRepo};

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
                ui.separator();
                let (branch, color) = branch_text(&repo.info.head, &palette);
                icons::icon(ui, Icon::GitBranch, color).on_hover_text("Current branch");
                let (terminal_tint, terminal_hover) = if terminal_open {
                    (palette.accent, "Hide terminal")
                } else {
                    (palette.text_muted, "Show terminal")
                };
                // The remaining width stays with the branch name. The toggle
                // sits on the opposite end of this bar.
                let terminal_toggle = ui
                    .with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let toggle =
                            icons::icon_button(ui, Icon::Terminal, terminal_tint, terminal_hover);
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.add(Label::new(branch).truncate());
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

    ScrollArea::vertical()
        .id_salt(repo.view_id.with("branches"))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for listed in &mut repo.branches {
                branch_row(ui, listed, palette);
            }
        });
}

fn branch_row(ui: &mut Ui, listed: &mut ListedBranch, palette: &Palette) {
    let width = ui.available_width();
    ui.push_id(&listed.branch.name, |ui| {
        ui.set_max_width(width);
        ui.horizontal(|ui| {
            ui.add(egui::Checkbox::without_text(&mut listed.checked));
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

        let indent = ui.spacing().icon_width + ui.spacing().item_spacing.x;
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
    let commits = &repo.history.commits;
    let count = if repo.history.truncated {
        format!("{}+", commits.len())
    } else {
        commits.len().to_string()
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

    if commits.is_empty() {
        ui.label(RichText::new("No commits yet.").color(palette.text_muted));
        return;
    }
    if repo.history.truncated {
        ui.label(
            RichText::new(format!("Showing the latest {} commits.", commits.len()))
                .color(palette.text_muted),
        );
        ui.add_space(4.0);
    }
    let row_height = widgets::commit_line_height(ui);
    ScrollArea::vertical()
        .id_salt(repo.view_id.with("history"))
        .auto_shrink([false, false])
        .show_rows(ui, row_height, commits.len(), |ui, rows| {
            for commit in &commits[rows] {
                ui.add(Label::new(widgets::commit_line(commit, palette, ui.style())).truncate());
            }
        });
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
}
