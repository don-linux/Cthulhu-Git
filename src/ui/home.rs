//! First screen. A wide window puts the mark, the version and the actions on
//! the left, and the recent repositories in a fixed-size grid on the right.
//! A narrow window stacks them and scrolls as one page. Recents stay newest
//! first, which is how settings already stores them. Git facts for a card
//! arrive from the app; this screen does not run git.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cthulhu_git::git::{Divergence, RepoGlance};
use eframe::egui::{
    self, Align, Color32, CornerRadius, CursorIcon, Frame, Label, Layout, Margin, Rect, RichText,
    ScrollArea, Sense, Stroke, StrokeKind, Ui, UiBuilder, Vec2,
};

use super::Action;
use super::icons::{self, Icon};
use super::theme;
use super::widgets;

/// Inner width at which the mark and the recents sit side by side. Narrower
/// than this, the list would be too thin to read a path.
const SPLIT_AT: f32 = 780.0;
/// Outer width of the brand column, including its padding.
const BRAND_PANEL: f32 = 420.0;
/// Side of the application icon on this screen. The source is 256 px.
const LOGO_SIDE: f32 = 128.0;
/// Centered column when the window is too narrow to split.
const NARROW_COLUMN: f32 = 420.0;
/// Exact card size. A minimum would still let the first card grow into the
/// space the scroll area has left.
const CARD_SIZE: Vec2 = Vec2::new(300.0, 168.0);
const CARD_GAP: f32 = 12.0;
/// Interface icons are drawn at 18px. See `icons::ICON_SIZE`.
const BRANCH_ICON: f32 = 18.0;
const PENDING: &str = "…";
const UNREADABLE: &str = "Can't read this repository";

/// Git facts for one recent repository. A path missing from
/// [`HomeState::glances`] has not been reported yet.
pub enum Glance {
    Ready(RepoGlance),
    Unreadable,
}

pub struct HomeState<'a> {
    pub recent: &'a [PathBuf],
    pub glances: &'a HashMap<PathBuf, Glance>,
    /// A repository is loading or the folder dialog is open.
    pub busy: bool,
    pub opening: Option<&'a Path>,
    pub error: Option<&'a str>,
}

enum Facts<'a> {
    Pending,
    Unreadable,
    Ready(&'a RepoGlance),
}

pub fn show(ui: &mut Ui, state: &HomeState<'_>) -> Option<Action> {
    // Panels are siblings: a side panel and the central panel have to be added
    // on this parent, with the central panel last, or egui cannot lay them out.
    if ui.available_width() >= SPLIT_AT {
        show_wide(ui, state)
    } else {
        egui::CentralPanel::default()
            .show(ui, |ui| show_narrow(ui, state))
            .inner
    }
}

fn show_wide(ui: &mut Ui, state: &HomeState<'_>) -> Option<Action> {
    let palette = theme::current(ui.ctx()).palette;
    let mut action = egui::Panel::left("home-brand")
        .exact_size(BRAND_PANEL)
        .resizable(false)
        .frame(
            Frame::new()
                .inner_margin(Margin::symmetric(28, 12))
                .fill(palette.background),
        )
        .show(ui, |ui| show_brand(ui, state))
        .inner;
    egui::CentralPanel::default()
        .frame(
            Frame::new()
                .inner_margin(Margin::symmetric(24, 28))
                .fill(palette.background),
        )
        .show(ui, |ui| {
            show_recents(ui, state, &mut action, true);
        });
    action
}

fn show_narrow(ui: &mut Ui, state: &HomeState<'_>) -> Option<Action> {
    let mut action = None;
    ScrollArea::vertical()
        .id_salt("home-narrow")
        .auto_shrink(false)
        .show(ui, |ui| {
            widgets::centered_column(ui, NARROW_COLUMN, |ui| {
                ui.add_space(36.0);
                brand_block(ui, state, &mut action);
                ui.add_space(28.0);
                show_recents(ui, state, &mut action, false);
                ui.add_space(24.0);
            });
        });
    action
}

/// Vertically centers the mark. The block's height is remembered from the
/// previous frame; the first frame sits high and asks for another paint.
fn show_brand(ui: &mut Ui, state: &HomeState<'_>) -> Option<Action> {
    let height_id = egui::Id::new("home-brand-height");
    let known = ui
        .ctx()
        .data(|data| data.get_temp::<f32>(height_id))
        .unwrap_or(0.0);
    let available = ui.available_height();
    let mut action = None;
    if available.is_finite() && known > available {
        ScrollArea::vertical()
            .id_salt("home-brand-scroll")
            .auto_shrink(false)
            .show(ui, |ui| {
                remember_brand(ui, state, &mut action, height_id);
            });
        return action;
    }
    if available.is_finite() && known > 0.0 {
        ui.add_space((available - known) * 0.5);
    }
    remember_brand(ui, state, &mut action, height_id);
    action
}

fn remember_brand(
    ui: &mut Ui,
    state: &HomeState<'_>,
    action: &mut Option<Action>,
    height_id: egui::Id,
) {
    let height = ui
        .scope(|ui| brand_block(ui, state, action))
        .response
        .rect
        .height();
    let previous = ui.ctx().data(|data| data.get_temp::<f32>(height_id));
    ui.ctx()
        .data_mut(|data| data.insert_temp(height_id, height));
    if previous.is_none_or(|previous| (previous - height).abs() > 0.5) {
        ui.ctx().request_repaint();
    }
}

fn brand_block(ui: &mut Ui, state: &HomeState<'_>, action: &mut Option<Action>) {
    let palette = theme::current(ui.ctx()).palette;
    ui.vertical_centered(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        icons::app_logo(ui, LOGO_SIDE);
        ui.add_space(10.0);
        ui.label(
            RichText::new("Cthulhu Git")
                .size(32.0)
                .strong()
                .color(palette.text),
        );
        ui.label(RichText::new(env!("CARGO_PKG_VERSION")).color(palette.text_muted));
        ui.add_space(2.0);
        ui.label(RichText::new("Open a Git repository to get started.").color(palette.text_muted));
        ui.add_space(18.0);
        if widgets::primary_button(ui, "Open repository…", !state.busy).clicked() {
            *action = Some(Action::Browse);
        }
        ui.add_space(2.0);
        if widgets::secondary_button(ui, "Settings", Some(Icon::Settings), true).clicked() {
            *action = Some(Action::OpenSettings);
        }
        ui.add_space(6.0);
        status_line(ui, state);
    });
    if let Some(error) = state.error {
        ui.add_space(8.0);
        widgets::error_banner(ui, error);
    }
}

fn show_recents(ui: &mut Ui, state: &HomeState<'_>, action: &mut Option<Action>, scroll: bool) {
    let palette = theme::current(ui.ctx()).palette;
    ui.label(
        RichText::new("Recent repositories")
            .strong()
            .color(palette.text_muted),
    );
    ui.add_space(8.0);
    if scroll {
        ScrollArea::vertical()
            .id_salt("home-recents")
            .auto_shrink(false)
            .show(ui, |ui| recent_cards(ui, state, action));
    } else {
        recent_cards(ui, state, action);
    }
}

fn recent_cards(ui: &mut Ui, state: &HomeState<'_>, action: &mut Option<Action>) {
    let palette = theme::current(ui.ctx()).palette;
    if state.recent.is_empty() {
        ui.label(
            RichText::new("Repositories you open will appear here.").color(palette.text_muted),
        );
        return;
    }
    // Top-aligned wrap. Centering on the cross axis would stretch the first
    // card to whatever height the scroll area still has free.
    ui.with_layout(
        Layout::left_to_right(Align::Min).with_main_wrap(true),
        |ui| {
            ui.spacing_mut().item_spacing = Vec2::splat(CARD_GAP);
            for path in state.recent {
                let facts = facts(state.glances, path);
                if recent_card(ui, path, facts, !state.busy).clicked() {
                    *action = Some(Action::Open(path.clone()));
                }
            }
        },
    );
}

fn facts<'a>(glances: &'a HashMap<PathBuf, Glance>, path: &Path) -> Facts<'a> {
    match glances.get(path) {
        Some(Glance::Ready(glance)) => Facts::Ready(glance),
        Some(Glance::Unreadable) => Facts::Unreadable,
        None => Facts::Pending,
    }
}

fn recent_card(ui: &mut Ui, path: &Path, facts: Facts<'_>, enabled: bool) -> egui::Response {
    let path_text = path.display().to_string();
    let palette = theme::current(ui.ctx()).palette;
    // Reserve the card on the wrapping layout itself. A nested scope is placed
    // after the fact and never moves a card that does not fit down to the next row.
    let (_, rect) = ui.allocate_space(CARD_SIZE);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let mut response = ui.interact(rect, ui.id().with(&path_text), sense);
    response = response.on_hover_text(&path_text);
    if enabled && ui.rect_contains_pointer(rect) {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, path_text.clone())
    });
    let hovered = enabled && ui.rect_contains_pointer(rect);
    paint_card(ui, rect, hovered, enabled, &palette);
    let inner = card_inner(rect);
    let mut body = ui.new_child(
        UiBuilder::new()
            .id_salt(&path_text)
            .max_rect(inner)
            .layout(Layout::top_down(Align::Min)),
    );
    body.set_clip_rect(body.clip_rect().intersect(inner));
    if !enabled {
        body.disable();
    }
    card_body(&mut body, path, facts, hovered);
    response
}

fn card_inner(rect: Rect) -> Rect {
    rect.shrink2(Vec2::new(14.0, 12.0))
}

fn paint_card(ui: &Ui, rect: Rect, hovered: bool, enabled: bool, palette: &theme::Palette) {
    let mut painter = ui.painter().clone();
    if !enabled {
        painter.multiply_opacity(ui.visuals().disabled_alpha());
    }
    painter.rect(
        rect,
        CornerRadius::same(widgets::CORNER_RADIUS),
        if hovered {
            palette.surface_hover
        } else {
            palette.surface
        },
        Stroke::new(
            1.0,
            if hovered {
                palette.accent
            } else {
                palette.border
            },
        ),
        StrokeKind::Inside,
    );
}

fn card_body(ui: &mut Ui, path: &Path, facts: Facts<'_>, hovered: bool) {
    let palette = theme::current(ui.ctx()).palette;
    ui.spacing_mut().item_spacing.y = 2.0;
    card_line(
        ui,
        RichText::new(display_name(path))
            .strong()
            .color(palette.text),
    );
    card_line(
        ui,
        RichText::new(path.display().to_string())
            .small()
            .color(palette.text_muted),
    );
    ui.add_space(6.0);
    match facts {
        Facts::Pending => {
            branch_line(ui, PENDING, palette.text_muted);
            card_line(ui, muted_small(PENDING, palette.text_muted));
            card_line(ui, RichText::new(PENDING).color(palette.text_muted));
            card_line(ui, muted_small(PENDING, palette.text_muted));
        }
        Facts::Unreadable => {
            card_line(ui, RichText::new(UNREADABLE).color(palette.error));
            card_line(ui, muted_small(" ", palette.text_muted));
            card_line(ui, RichText::new(" "));
            card_line(ui, muted_small(" ", palette.text_muted));
        }
        Facts::Ready(glance) => {
            let branch = if hovered {
                palette.text
            } else {
                palette.text_muted
            };
            branch_line(ui, &glance.head.to_string(), branch);
            match glance.upstream {
                Some(divergence) => {
                    card_line(
                        ui,
                        muted_small(&upstream_line(divergence), palette.text_muted),
                    );
                }
                None => card_line(ui, muted_small(" ", palette.text_muted)),
            }
            match &glance.latest {
                Some(line) => {
                    if line.subject.is_empty() {
                        card_line(ui, RichText::new(" "));
                    } else {
                        card_line(ui, RichText::new(&line.subject).color(palette.text));
                    }
                    card_line(ui, muted_small(&line.when, palette.text_muted));
                }
                None => {
                    card_line(ui, RichText::new(" "));
                    card_line(ui, muted_small(" ", palette.text_muted));
                }
            }
        }
    }
}

fn branch_line(ui: &mut Ui, text: &str, color: Color32) {
    // A fixed row. `ui.horizontal` centers on the cross axis and would grow
    // into whatever height this card still has free.
    let (_, rect) = ui.allocate_space(Vec2::new(ui.available_width(), 22.0));
    let icon_rect = Rect::from_center_size(
        egui::pos2(rect.min.x + BRANCH_ICON * 0.5, rect.center().y),
        Vec2::splat(BRANCH_ICON),
    );
    icons::paint_icon(ui.painter(), ui.ctx(), icon_rect, Icon::GitBranch, color);
    let text_rect = Rect::from_min_max(
        egui::pos2(rect.min.x + BRANCH_ICON + 6.0, rect.min.y),
        rect.max,
    );
    let mut text_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(text_rect)
            .layout(Layout::left_to_right(Align::Min)),
    );
    text_ui.set_clip_rect(text_rect);
    text_ui.add(truncated_label(RichText::new(text).color(color)));
}

fn card_line(ui: &mut Ui, text: RichText) {
    ui.add(truncated_label(text));
}

fn truncated_label(text: RichText) -> Label {
    Label::new(text)
        .selectable(false)
        .sense(Sense::hover())
        .truncate()
}

fn muted_small(text: &str, color: Color32) -> RichText {
    RichText::new(text).small().color(color)
}

fn upstream_line(divergence: Divergence) -> String {
    match (divergence.ahead, divergence.behind) {
        (0, 0) => "in sync".to_owned(),
        (ahead, 0) => format!("{ahead} ahead"),
        (0, behind) => format!("{behind} behind"),
        (ahead, behind) => format!("{ahead} ahead, {behind} behind"),
    }
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
