//! Settings screen. It replaces the whole window, including the repository
//! bottom bar. Back and Home sit at the left of the top bar, and the title
//! at the right. Pages are listed on the left; the chosen page is drawn on
//! the right. Both columns scroll.

use eframe::egui::{
    self, Align, Button, Color32, CursorIcon, Frame, Id, Key, Layout, Margin, RichText, ScrollArea,
    Sense, Stroke, TextEdit, Ui, UiBuilder,
};

use super::Action;
use super::fonts::{self, CatalogPhase, FacePhase, FontService};
use super::icons::{self, Icon};
use super::theme::{self, Palette};
use super::widgets;

const NAV_WIDTH: f32 = 200.0;
const FORM_WIDTH: f32 = 560.0;
const SAMPLE: &str = "The quick brown fox jumps over 0123456789";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Terminal,
}

#[derive(Clone, Default)]
struct FontPicker {
    open: bool,
    query: String,
    page: usize,
}

pub fn show(ui: &mut Ui, terminal_font: Option<&str>, fonts: &FontService) -> Vec<Action> {
    let palette = theme::current(ui.ctx()).palette;
    let mut actions = Vec::new();
    let mut page = ui
        .ctx()
        .data(|data| data.get_temp(page_id()))
        .unwrap_or(Page::Terminal);

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

    egui::Panel::left("settings-nav")
        .exact_size(NAV_WIDTH)
        .resizable(false)
        .frame(nav_frame(&palette))
        .show(ui, |ui| {
            ScrollArea::vertical()
                .id_salt("settings-nav")
                .auto_shrink(false)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    if nav_button(ui, "Terminal", Icon::Terminal, page == Page::Terminal).clicked()
                    {
                        page = Page::Terminal;
                    }
                });
        });

    let view = fonts.view(terminal_font);
    egui::CentralPanel::default().show(ui, |ui| {
        ScrollArea::vertical()
            .id_salt("settings-page")
            .auto_shrink(false)
            .show(ui, |ui| {
                let width = ui.available_width().min(FORM_WIDTH);
                ui.set_min_width(width);
                ui.set_max_width(width);
                match page {
                    Page::Terminal => {
                        terminal_page(ui, &palette, terminal_font, &view, &mut actions)
                    }
                }
            });
    });

    ui.ctx().data_mut(|data| data.insert_temp(page_id(), page));
    actions
}

fn terminal_page(
    ui: &mut Ui,
    palette: &Palette,
    saved: Option<&str>,
    view: &fonts::FontView<'_>,
    actions: &mut Vec<Action>,
) {
    let mut picker = ui
        .ctx()
        .data(|data| data.get_temp(picker_id()))
        .unwrap_or_default();

    ui.add_space(8.0);
    ui.label(
        RichText::new("Terminal Font")
            .size(16.0)
            .strong()
            .color(palette.text),
    );
    ui.label(
        RichText::new(
            "Family used by the terminal. Leave this empty to keep the built-in monospace.",
        )
        .small()
        .color(palette.text_muted),
    );
    ui.add_space(12.0);

    if let Some(name) = font_name_row(ui, saved, &mut picker, actions) {
        actions.push(Action::SetTerminalFont(name));
    }
    ui.add_space(8.0);
    font_hint(ui, palette, view);
    if let FacePhase::Ready { .. } = view.face {
        ui.add_space(8.0);
        let font = fonts::terminal_font_id(ui);
        ui.label(RichText::new(SAMPLE).font(font).color(palette.text));
    }

    if picker.open {
        ui.add_space(16.0);
        font_browser(ui, palette, view, &mut picker, actions);
    }

    ui.ctx()
        .data_mut(|data| data.insert_temp(picker_id(), picker));
}

/// Commits when the field loses focus, including Enter. Escape reverts.
/// Clicking another control reports the loss on the next frame, so the typed
/// text is kept in temporary data until then.
fn font_name_row(
    ui: &mut Ui,
    saved: Option<&str>,
    picker: &mut FontPicker,
    actions: &mut Vec<Action>,
) -> Option<Option<String>> {
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let label = if picker.open {
                "Hide fonts"
            } else {
                "Browse fonts"
            };
            if ui
                .add(Button::new(label))
                .on_hover_cursor(CursorIcon::PointingHand)
                .clicked()
            {
                picker.open = !picker.open;
                if picker.open {
                    actions.push(Action::EnsureFontCatalog);
                }
            }
            font_editor(ui, saved)
        })
        .inner
    })
    .inner
}

fn font_editor(ui: &mut Ui, saved: Option<&str>) -> Option<Option<String>> {
    let saved_text = saved.unwrap_or("");
    let escape = ui.input(|input| input.key_pressed(Key::Escape));
    let mut draft = if escape {
        saved_text.to_owned()
    } else {
        ui.ctx()
            .data(|data| data.get_temp(draft_id()))
            .unwrap_or_else(|| saved_text.to_owned())
    };
    let width = ui.available_width();
    let response = ui.add(
        TextEdit::singleline(&mut draft)
            .id(font_field_id())
            .hint_text("Monospace")
            .desired_width(width),
    );
    if response.has_focus() {
        ui.ctx()
            .data_mut(|data| data.insert_temp(draft_id(), draft.clone()));
        return None;
    }
    ui.ctx().data_mut(|data| data.remove::<String>(draft_id()));
    if response.lost_focus() && !escape {
        let name = normalize_draft(&draft);
        if name.as_deref() != saved {
            return Some(name);
        }
    }
    None
}

fn font_hint(ui: &mut Ui, palette: &Palette, view: &fonts::FontView<'_>) {
    match &view.face {
        FacePhase::Builtin { pending: true } => {
            note(ui, "Looking up installed fonts…", palette.text_muted);
        }
        FacePhase::Missing => {
            note(
                ui,
                "Font not found. The terminal is using the built-in monospace.",
                palette.text_muted,
            );
        }
        FacePhase::Ready { monospace: false } => {
            note(
                ui,
                "This font is not monospace. The terminal grid expects a fixed width.",
                palette.text_muted,
            );
        }
        FacePhase::Unreadable(message) | FacePhase::ScanFailed(message) => {
            note(ui, message, palette.error);
        }
        FacePhase::Builtin { pending: false } | FacePhase::Ready { monospace: true } => {}
    }
}

fn font_browser(
    ui: &mut Ui,
    palette: &Palette,
    view: &fonts::FontView<'_>,
    picker: &mut FontPicker,
    actions: &mut Vec<Action>,
) {
    match view.catalog {
        CatalogPhase::Idle | CatalogPhase::Loading => {
            note(ui, "Looking up installed fonts…", palette.text_muted);
        }
        CatalogPhase::Failed(message) => note(ui, message, palette.error),
        CatalogPhase::Ready(catalog) => {
            let mut query = picker.query.clone();
            ui.add(
                TextEdit::singleline(&mut query)
                    .id_salt("font-search")
                    .hint_text("Search fonts")
                    .desired_width(f32::INFINITY),
            );
            if query != picker.query {
                picker.page = 0;
                picker.query = query;
            }
            ui.add_space(8.0);

            if catalog.names().is_empty() {
                note(ui, "No installed fonts were found.", palette.text_muted);
                return;
            }
            let listed = fonts::page_names(
                catalog.names(),
                &picker.query,
                picker.page,
                fonts::PAGE_SIZE,
            );
            picker.page = listed.page;
            if listed.names.is_empty() {
                note(ui, "No fonts match.", palette.text_muted);
                return;
            }
            for name in &listed.names {
                if font_row(ui, palette, name).clicked() {
                    ui.ctx().data_mut(|data| data.remove::<String>(draft_id()));
                    actions.push(Action::SetTerminalFont(Some(name.clone())));
                }
            }
            ui.add_space(8.0);
            page_controls(ui, picker, listed.page, listed.page_count);
        }
    }
}

fn page_controls(ui: &mut Ui, picker: &mut FontPicker, page: usize, page_count: usize) {
    let can_previous = page > 0;
    let can_next = page + 1 < page_count;
    ui.horizontal(|ui| {
        let mut previous = ui.add_enabled(can_previous, Button::new("Previous"));
        if can_previous {
            previous = previous.on_hover_cursor(CursorIcon::PointingHand);
        }
        if previous.clicked() {
            picker.page = page.saturating_sub(1);
        }
        let label = if page_count == 0 {
            "0 / 0".to_owned()
        } else {
            format!("{} / {}", page + 1, page_count)
        };
        ui.label(label);
        let mut next = ui.add_enabled(can_next, Button::new("Next"));
        if can_next {
            next = next.on_hover_cursor(CursorIcon::PointingHand);
        }
        if next.clicked() {
            picker.page = page + 1;
        }
    });
}

fn font_row(ui: &mut Ui, palette: &Palette, name: &str) -> egui::Response {
    let response = ui
        .scope_builder(UiBuilder::new().id_salt(name).sense(Sense::click()), |ui| {
            let hovered = ui.response().hovered();
            Frame::new()
                .fill(if hovered {
                    palette.surface_hover
                } else {
                    palette.surface
                })
                .corner_radius(widgets::CORNER_RADIUS)
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(
                        egui::Label::new(RichText::new(name).color(palette.text))
                            .selectable(false)
                            .truncate(),
                    );
                });
        })
        .response;
    response.on_hover_cursor(CursorIcon::PointingHand)
}

fn nav_button(ui: &mut Ui, label: &str, icon: Icon, selected: bool) -> egui::Response {
    let palette = theme::current(ui.ctx()).palette;
    let response = ui
        .scope_builder(
            UiBuilder::new().id_salt(label).sense(Sense::click()),
            |ui| {
                let hovered = ui.response().hovered();
                let fill = if selected {
                    palette.surface_active
                } else if hovered {
                    palette.surface_hover
                } else {
                    Color32::TRANSPARENT
                };
                Frame::new()
                    .fill(fill)
                    .corner_radius(widgets::CORNER_RADIUS)
                    .inner_margin(Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let tint = if selected {
                                palette.accent
                            } else {
                                palette.text
                            };
                            icons::icon(ui, icon, tint);
                            ui.add(
                                egui::Label::new(RichText::new(label).color(palette.text))
                                    .selectable(false)
                                    .truncate(),
                            );
                        });
                    });
            },
        )
        .response;
    response.on_hover_cursor(CursorIcon::PointingHand)
}

fn note(ui: &mut Ui, text: &str, color: Color32) {
    ui.label(RichText::new(text).color(color));
}

fn normalize_draft(draft: &str) -> Option<String> {
    let trimmed = draft.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn page_id() -> Id {
    Id::new("settings-page")
}

fn picker_id() -> Id {
    Id::new("settings-font-picker")
}

fn font_field_id() -> Id {
    Id::new("settings-terminal-font")
}

fn draft_id() -> Id {
    font_field_id().with("draft")
}

/// Same chrome as the repository bars.
fn bar_frame(palette: &Palette) -> Frame {
    Frame::new()
        .fill(palette.surface)
        .stroke(Stroke::new(1.0, palette.border))
        .inner_margin(Margin::symmetric(12, 6))
}

fn nav_frame(palette: &Palette) -> Frame {
    Frame::new()
        .fill(palette.surface)
        .inner_margin(Margin::symmetric(8, 8))
}
