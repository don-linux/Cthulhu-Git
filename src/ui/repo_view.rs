//! Repository screen: name, current branch, latest commit and the commit
//! history, collapsed until the user opens it.

use cthulhu_git::git::Head;
use eframe::egui::{
    self, CollapsingHeader, Frame, Label, Margin, RichText, ScrollArea, Stroke, Ui,
};

use super::theme::{self, Palette};
use super::widgets;
use super::{Action, OpenedRepo};

pub fn show(ui: &mut Ui, repo: &OpenedRepo, error: Option<&str>) -> Option<Action> {
    let palette = theme::current(ui.ctx()).palette;
    let mut action = None;

    egui::Panel::top("repo-top-bar")
        .frame(
            Frame::new()
                .fill(palette.surface)
                .stroke(Stroke::new(1.0, palette.border))
                .inner_margin(Margin::symmetric(12, 8)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .button(RichText::new("Home").color(palette.text))
                    .on_hover_text("Open another repository")
                    .clicked()
                {
                    action = Some(Action::Home);
                }
            });
        });

    egui::CentralPanel::default().show(ui, |ui| {
        if let Some(error) = error {
            widgets::error_banner(ui, error);
            ui.add_space(12.0);
        }

        widgets::card(ui, |ui| {
            ui.label(
                RichText::new(&repo.info.name)
                    .size(26.0)
                    .strong()
                    .color(palette.text),
            );
            ui.add_space(12.0);
            widgets::field_row(ui, "Branch", |ui| {
                ui.add(Label::new(branch_text(&repo.info.head, &palette)).truncate());
            });
            ui.add_space(6.0);
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

        ui.add_space(16.0);
        history(ui, repo, &palette);
    });

    action
}

fn history(ui: &mut Ui, repo: &OpenedRepo, palette: &Palette) {
    let commits = &repo.history.commits;
    let count = if repo.history.truncated {
        format!("{}+", commits.len())
    } else {
        commits.len().to_string()
    };

    CollapsingHeader::new(
        RichText::new(format!("Commit history ({count})"))
            .strong()
            .color(palette.text),
    )
    .id_salt(repo.view_id)
    .default_open(false)
    .show(ui, |ui| {
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
            .auto_shrink([false, true])
            .max_height(ui.available_height())
            .show_rows(ui, row_height, commits.len(), |ui, rows| {
                for commit in &commits[rows] {
                    ui.add(
                        Label::new(widgets::commit_line(commit, palette, ui.style())).truncate(),
                    );
                }
            });
    });
}

fn branch_text(head: &Head, palette: &Palette) -> RichText {
    let text = RichText::new(head.to_string());
    match head {
        Head::Branch(_) => text.strong().color(palette.text),
        Head::Unborn(_) => text.italics().color(palette.text_muted),
        Head::Detached { .. } => text.color(palette.warning),
    }
}
