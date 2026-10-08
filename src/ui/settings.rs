//! Settings screen. It replaces the whole window, including the repository
//! bottom bar. The body is empty. Back and Home sit at the left of the top
//! bar, and the title at the right.

use eframe::egui::{self, Align, Frame, Layout, Margin, RichText, Stroke, Ui};

use super::Action;
use super::icons::{self, Icon};
use super::theme::{self, Palette};

pub fn show(ui: &mut Ui) -> Vec<Action> {
    let palette = theme::current(ui.ctx()).palette;
    let mut actions = Vec::new();

    egui::Panel::top("settings-top-bar")
        .frame(bar_frame(&palette))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if icons::icon_button(ui, Icon::ArrowLeft, palette.text, "Back").clicked() {
                    actions.push(Action::CloseSettings);
                }
                if icons::icon_button(ui, Icon::House, palette.text, "Open another repository")
                    .clicked()
                {
                    actions.push(Action::Home);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new("Settings")
                            .size(15.0)
                            .strong()
                            .color(palette.text),
                    );
                });
            });
        });

    egui::CentralPanel::default().show(ui, |_ui| {});

    actions
}

/// Same chrome as the repository bars.
fn bar_frame(palette: &Palette) -> Frame {
    Frame::new()
        .fill(palette.surface)
        .stroke(Stroke::new(1.0, palette.border))
        .inner_margin(Margin::symmetric(12, 6))
}
