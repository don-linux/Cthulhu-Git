//! Small building blocks shared by the screens. Colors come from the active
//! theme's palette only.

use std::time::Duration;

use eframe::egui::{
    self, Align2, Button, Context, CursorIcon, Frame, Id, Margin, Order, Response, RichText,
    Stroke, Ui, Vec2,
};

use super::icons::{self, Icon};
use super::theme;

pub const CORNER_RADIUS: u8 = 6;

/// Shared by [`primary_button`] and [`secondary_button`] so a pair lines up.
const ACTION_BUTTON: Vec2 = Vec2::new(240.0, 40.0);

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
    .min_size(ACTION_BUTTON);
    ui.add_enabled(enabled, button)
        .on_hover_cursor(CursorIcon::PointingHand)
}

/// A surface button with a border. Same size as [`primary_button`]. The theme's
/// hover colors show through because the fill is not forced to the accent.
/// `icon` sits in front of `text`.
pub fn secondary_button(ui: &mut Ui, text: &str, icon: Option<Icon>, enabled: bool) -> Response {
    let palette = theme::current(ui.ctx()).palette;
    let tint = if enabled {
        palette.text
    } else {
        palette.text_muted
    };
    let label = RichText::new(text).size(16.0).color(tint);
    let button = match icon {
        Some(icon) => Button::image_and_text(icons::tinted(ui, icon, tint), label),
        None => Button::new(label),
    }
    .corner_radius(CORNER_RADIUS)
    .min_size(ACTION_BUTTON);
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

/// How long the "copied" notice stays up. A later copy starts the wait over.
const CLIPBOARD_TOAST_SECS: f64 = 2.0;
/// Clears the bottom bar (about 32px) and leaves a gap above its buttons.
const CLIPBOARD_TOAST_LIFT: f32 = 48.0;

#[derive(Clone)]
struct ClipboardToast {
    notice: String,
    until: f64,
}

fn clipboard_toast_id() -> Id {
    Id::new("cthulhu-clipboard-toast")
}

/// Puts the full hash on the clipboard and posts a short notice for
/// [`show_clipboard_toast`].
pub(super) fn copy_commit_hash(ctx: &Context, oid: &str) {
    ctx.copy_text(oid.to_owned());
    let notice = format!("Copied {}", cthulhu_git::git::short_oid(oid));
    let until = ctx.input(|input| input.time) + CLIPBOARD_TOAST_SECS;
    ctx.data_mut(|data| {
        data.insert_temp(clipboard_toast_id(), ClipboardToast { notice, until });
    });
    ctx.request_repaint();
}

/// Draws the notice from [`copy_commit_hash`], centered just above the bottom
/// bar. The area does not take clicks, so a commit underneath can still be chosen.
pub(super) fn show_clipboard_toast(ctx: &Context) {
    let id = clipboard_toast_id();
    let Some(toast) = ctx.data(|data| data.get_temp::<ClipboardToast>(id)) else {
        return;
    };
    let now = ctx.input(|input| input.time);
    if now >= toast.until {
        ctx.data_mut(|data| data.remove::<ClipboardToast>(id));
        return;
    }
    ctx.request_repaint_after(Duration::from_secs_f64(toast.until - now));
    let palette = theme::current(ctx).palette;
    egui::Area::new(id.with("popup"))
        .order(Order::Foreground)
        .anchor(Align2::CENTER_BOTTOM, Vec2::new(0.0, -CLIPBOARD_TOAST_LIFT))
        .interactable(false)
        .fade_in(false)
        .show(ctx, |ui| {
            Frame::new()
                .fill(palette.surface)
                .stroke(Stroke::new(1.0, palette.border))
                .corner_radius(CORNER_RADIUS)
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.label(RichText::new(&toast.notice).color(palette.text));
                });
        });
}
