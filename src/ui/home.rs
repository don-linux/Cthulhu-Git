//! First screen: open a repository with the system folder dialog, or pick a
//! recent one.

use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText, ScrollArea, Ui};

use super::Action;
use super::theme;
use super::widgets;

const CONTENT_WIDTH: f32 = 560.0;

pub struct HomeState<'a> {
    pub recent: &'a [PathBuf],
    /// A repository is loading or the folder dialog is open.
    pub busy: bool,
    pub opening: Option<&'a Path>,
    pub error: Option<&'a str>,
}

pub fn show(ui: &mut Ui, state: &HomeState<'_>) -> Option<Action> {
    let palette = theme::current(ui.ctx()).palette;
    let mut action = None;

    egui::CentralPanel::default().show(ui, |ui| {
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            widgets::centered_column(ui, CONTENT_WIDTH, |ui| {
                ui.add_space(56.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("Cthulhu Git")
                            .size(32.0)
                            .strong()
                            .color(palette.text),
                    );
                    ui.label(
                        RichText::new("Open a Git repository to get started.")
                            .color(palette.text_muted),
                    );
                    ui.add_space(24.0);

                    if widgets::primary_button(ui, "Open repository…", !state.busy).clicked() {
                        action = Some(Action::Browse);
                    }

                    ui.add_space(12.0);
                    status_line(ui, state);
                });

                if let Some(error) = state.error {
                    ui.add_space(8.0);
                    widgets::error_banner(ui, error);
                }

                ui.add_space(28.0);
                ui.label(
                    RichText::new("Recent repositories")
                        .strong()
                        .color(palette.text_muted),
                );
                ui.add_space(8.0);

                if state.recent.is_empty() {
                    ui.label(
                        RichText::new("Repositories you open will appear here.")
                            .color(palette.text_muted),
                    );
                }
                for path in state.recent {
                    if widgets::list_row(
                        ui,
                        &display_name(path),
                        &path.display().to_string(),
                        !state.busy,
                    )
                    .clicked()
                    {
                        action = Some(Action::Open(path.clone()));
                    }
                    ui.add_space(6.0);
                }
                ui.add_space(24.0);
            });
        });
    });

    action
}

/// Always takes the same height, so the page does not jump while loading.
fn status_line(ui: &mut Ui, state: &HomeState<'_>) {
    let palette = theme::current(ui.ctx()).palette;
    let text = match state.opening {
        Some(path) => format!("Opening {}…", display_name(path)),
        None if state.busy => "Waiting for the folder dialog…".to_owned(),
        None => {
            ui.add_space(ui.spacing().interact_size.y);
            return;
        }
    };
    let galley = egui::WidgetText::from(RichText::new(text).color(palette.text_muted)).into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        ui.available_width(),
        egui::TextStyle::Body,
    );
    let spinner = ui.spacing().interact_size.y;
    let width = spinner + ui.spacing().item_spacing.x + galley.size().x;
    ui.horizontal(|ui| {
        ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
        ui.add(egui::Spinner::new().size(spinner));
        ui.label(galley);
    });
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}
