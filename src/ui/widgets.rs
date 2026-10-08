//! Small building blocks shared by the screens. Colors come from the active
//! theme's palette only.

use cthulhu_git::git::Commit;
use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{
    self, Align, Button, CursorIcon, Frame, Margin, Response, RichText, Sense, Stroke, Style,
    TextStyle, Ui, UiBuilder, Vec2,
};

use super::theme::{self, Palette};

pub const CORNER_RADIUS: u8 = 6;

/// Lays out `add_contents` in a column at most `max_width` wide, centered
/// horizontally in the available space.
pub fn centered_column<R>(
    ui: &mut Ui,
    max_width: f32,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> R {
    let width = ui.available_width().min(max_width);
    let side = (ui.available_width() - width) / 2.0;
    ui.horizontal_top(|ui| {
        ui.add_space(side);
        ui.vertical(|ui| {
            ui.set_width(width);
            add_contents(ui)
        })
        .inner
    })
    .inner
}

pub fn primary_button(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    let palette = theme::current(ui.ctx()).palette;
    let button = Button::new(
        RichText::new(text)
            .size(16.0)
            .color(palette.on_accent)
            .strong(),
    )
    .fill(palette.accent)
    .corner_radius(CORNER_RADIUS)
    .min_size(Vec2::new(220.0, 40.0));
    ui.add_enabled(enabled, button)
        .on_hover_cursor(CursorIcon::PointingHand)
}

pub fn error_banner(ui: &mut Ui, message: &str) {
    let palette = theme::current(ui.ctx()).palette;
    Frame::new()
        .fill(palette.surface)
        .stroke(Stroke::new(1.0, palette.error))
        .corner_radius(CORNER_RADIUS)
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(message).color(palette.error));
        });
}

/// A full-width clickable row with a title and a muted second line.
pub fn list_row(ui: &mut Ui, title: &str, detail: &str, enabled: bool) -> Response {
    let palette = theme::current(ui.ctx()).palette;
    let mut builder = UiBuilder::new().id_salt(detail).sense(Sense::click());
    if !enabled {
        builder = builder.disabled();
    }
    let response = ui
        .scope_builder(builder, |ui| {
            let hovered = enabled && ui.response().hovered();
            Frame::new()
                .fill(if hovered {
                    palette.surface_hover
                } else {
                    palette.surface
                })
                .stroke(Stroke::new(
                    1.0,
                    if hovered {
                        palette.accent
                    } else {
                        palette.border
                    },
                ))
                .corner_radius(CORNER_RADIUS)
                .inner_margin(Margin::symmetric(14, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(
                        egui::Label::new(RichText::new(title).strong().color(palette.text))
                            .selectable(false)
                            .truncate(),
                    );
                    ui.add(
                        egui::Label::new(RichText::new(detail).small().color(palette.text_muted))
                            .selectable(false)
                            .truncate(),
                    );
                });
        })
        .response;
    if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

/// `<12-character hash> - <summary>`, the hash in monospace.
pub fn commit_line(commit: &Commit, palette: &Palette, style: &Style) -> LayoutJob {
    let format = |text_style: TextStyle, color| TextFormat {
        font_id: text_style.resolve(style),
        color,
        valign: Align::Center,
        ..TextFormat::default()
    };
    let mut job = LayoutJob::default();
    job.append(
        commit.short_oid(),
        0.0,
        format(TextStyle::Monospace, palette.hash),
    );
    job.append(" - ", 0.0, format(TextStyle::Body, palette.text_muted));
    job.append(&commit.summary, 0.0, format(TextStyle::Body, palette.text));
    job
}

/// Height of one [`commit_line`], for virtualized lists.
pub fn commit_line_height(ui: &Ui) -> f32 {
    ui.text_style_height(&TextStyle::Monospace)
        .max(ui.text_style_height(&TextStyle::Body))
}
