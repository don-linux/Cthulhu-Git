//! Repository screen. A top bar with the history toggle and the repository
//! name, a sidebar with the commit history, the latest commit in the middle,
//! and a bottom bar with the Home button and the current branch.

use cthulhu_git::git::Head;
use eframe::egui::{
    self, Color32, Frame, Label, Margin, Pos2, Rect, RichText, ScrollArea, Stroke, Ui, Vec2,
};

use super::icons::{self, Icon};
use super::theme::{self, Palette};
use super::widgets;
use super::{Action, OpenedRepo};

const SIDEBAR_DEFAULT_WIDTH: f32 = 320.0;
const SIDEBAR_MIN_WIDTH: f32 = 220.0;
const SIDEBAR_MAX_WIDTH: f32 = 560.0;

pub fn show(
    ui: &mut Ui,
    repo: &OpenedRepo,
    error: Option<&str>,
    sidebar_open: bool,
) -> Option<Action> {
    let palette = theme::current(ui.ctx()).palette;
    let mut action = None;
    // The toggle button and dragging the sidebar edge past its minimum width
    // both flip this; the app saves the change.
    let mut open = sidebar_open;

    // Top and bottom bars come first so they span the whole window width.
    egui::Panel::top("repo-top-bar")
        .frame(bar_frame(&palette))
        .show(ui, |ui| top_bar(ui, repo, &palette, &mut open));

    egui::Panel::bottom("repo-bottom-bar")
        .frame(bar_frame(&palette))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if icons::icon_button(ui, Icon::House, palette.text, "Open another repository")
                    .clicked()
                {
                    action = Some(Action::Home);
                }
                ui.separator();
                let (branch, color) = branch_text(&repo.info.head, &palette);
                icons::icon(ui, Icon::GitBranch, color).on_hover_text("Current branch");
                ui.add(Label::new(branch).truncate());
            });
        });

    egui::Panel::left("repo-history-sidebar")
        .resizable(true)
        .default_size(SIDEBAR_DEFAULT_WIDTH)
        .size_range(SIDEBAR_MIN_WIDTH..=SIDEBAR_MAX_WIDTH)
        .frame(
            Frame::new()
                .fill(palette.background)
                .inner_margin(Margin::same(12)),
        )
        .show_collapsible(ui, &mut open, |ui| history(ui, repo, &palette));

    egui::CentralPanel::default().show(ui, |ui| {
        if let Some(error) = error {
            widgets::error_banner(ui, error);
            ui.add_space(12.0);
        }

        widgets::card(ui, |ui| {
            widgets::field_row(ui, "Latest commit", |ui| match repo.history.latest() {
                Some(commit) => {
                    ui.add(
                        Label::new(widgets::commit_line(commit, &palette, ui.style())).truncate(),
                    );
                }
                None => {
                    ui.label(RichText::new("No commits yet").color(palette.text_muted));
                }
            });
        });
    });

    if open != sidebar_open {
        action = Some(Action::ToggleHistorySidebar);
    }
    action
}

fn bar_frame(palette: &Palette) -> Frame {
    Frame::new()
        .fill(palette.surface)
        .stroke(Stroke::new(1.0, palette.border))
        .inner_margin(Margin::symmetric(12, 6))
}

/// The toggle on the left, the name centered on the window (not on the space
/// left by the toggle) and truncated before it would reach the toggle.
fn top_bar(ui: &mut Ui, repo: &OpenedRepo, palette: &Palette, open: &mut bool) {
    ui.horizontal(|ui| {
        let (tint, hover) = if *open {
            (palette.accent, "Hide commit history")
        } else {
            (palette.text_muted, "Show commit history")
        };
        let toggle = icons::icon_button(ui, Icon::PanelLeft, tint, hover);
        if toggle.clicked() {
            *open = !*open;
        }

        let row = ui.max_rect();
        let reserved = toggle.rect.right() - row.left() + ui.spacing().item_spacing.x;
        let name_rect = Rect::from_center_size(
            Pos2::new(row.center().x, toggle.rect.center().y),
            Vec2::new(
                (row.width() - 2.0 * reserved).max(0.0),
                toggle.rect.height(),
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
