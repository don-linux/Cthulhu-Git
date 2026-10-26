//! The modal that lists a commit's files and paints one unified diff.
//!
//! The file list comes from the hover read. The patch is a separate `git show
//! -p` of the file that was clicked, so a large commit does not pull every
//! diff up front. egui draws only the visible lines.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use cthulhu_git::git::{self, CommitFile, DiffKind, DiffLine, FileDiff, Git};
use eframe::egui::{
    self, Align, Color32, CursorIcon, FontId, Frame, Label, Layout, Margin, RichText, ScrollArea,
    Sense, Stroke, TextStyle, Ui, Vec2,
    text::{LayoutJob, TextFormat},
};

use super::theme::Palette;
use super::widgets;

const LIST_WIDTH: f32 = 280.0;
const MODAL_BACKDROP_ALPHA: u8 = 160;
/// Patches kept after the modal closes, so returning to a file does not ask git again.
const MAX_ENTRIES: usize = 256;

enum State {
    Loading,
    Ready(Box<FileDiff>),
    Failed(String),
}

enum CachedPatch<'a> {
    Pending,
    Ready(&'a FileDiff),
    Failed(&'a str),
}

struct Inflight {
    oid: String,
    path: String,
    receiver: Receiver<Result<FileDiff, String>>,
}

/// Patches already read, and the one `git show -p` in flight.
#[derive(Default)]
pub struct DiffCache {
    entries: HashMap<(String, String), State>,
    order: Vec<(String, String)>,
    inflight: Option<Inflight>,
    /// Asked for while another read was running. Only the newest is kept.
    wanted: Option<(String, String)>,
}

impl DiffCache {
    pub fn poll(&mut self, ctx: &egui::Context, root: &Path) {
        if let Some(inflight) = &self.inflight {
            let result = match inflight.receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err("The diff stopped unexpectedly.".to_owned()))
                }
            };
            if let Some(result) = result {
                let state = match result {
                    Ok(diff) => State::Ready(Box::new(diff)),
                    Err(message) => State::Failed(message),
                };
                let key = (inflight.oid.clone(), inflight.path.clone());
                self.inflight = None;
                self.insert(key, state);
            }
        }
        if self.inflight.is_none()
            && let Some((oid, path)) = self.wanted.take()
        {
            self.start(ctx, root, oid, path);
        }
    }

    fn insert(&mut self, key: (String, String), state: State) {
        if !self.entries.contains_key(&key) {
            self.order.push(key.clone());
        }
        self.entries.insert(key, state);
        while self.order.len() > MAX_ENTRIES {
            let old = self.order.remove(0);
            self.entries.remove(&old);
        }
    }

    /// Starts a read unless this file is already stored or in flight.
    pub fn request(&mut self, oid: &str, path: &str) {
        let key = (oid.to_owned(), path.to_owned());
        let busy = self
            .inflight
            .as_ref()
            .is_some_and(|inflight| inflight.oid == oid && inflight.path == path);
        if self.entries.contains_key(&key) || busy {
            return;
        }
        self.wanted = Some(key);
    }

    fn start(&mut self, ctx: &egui::Context, root: &Path, oid: String, path: String) {
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        let root: PathBuf = root.to_path_buf();
        let worker_oid = oid.clone();
        let worker_path = path.clone();
        thread::spawn(move || {
            let result = Git::discover()
                .and_then(|git| git::file_diff(&git, &root, &worker_oid, &worker_path))
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
            ctx.request_repaint();
        });
        self.entries
            .insert((oid.clone(), path.clone()), State::Loading);
        self.inflight = Some(Inflight {
            oid,
            path,
            receiver,
        });
    }

    fn cached(&self, oid: &str, path: &str) -> Option<CachedPatch<'_>> {
        self.entries
            .get(&(oid.to_owned(), path.to_owned()))
            .map(|state| match state {
                State::Loading => CachedPatch::Pending,
                State::Ready(diff) => CachedPatch::Ready(diff),
                State::Failed(message) => CachedPatch::Failed(message),
            })
    }

    /// Stores a finished patch so a test can draw the diff without `git show`.
    #[cfg(test)]
    pub(super) fn store_ready(&mut self, oid: &str, path: &str, diff: FileDiff) {
        self.insert(
            (oid.to_owned(), path.to_owned()),
            State::Ready(Box::new(diff)),
        );
    }
}

/// The commit whose files the modal is showing.
pub struct Changes {
    pub oid: String,
    files: Vec<CommitFile>,
    selected: usize,
}

impl Changes {
    pub fn new(oid: String, files: Vec<CommitFile>) -> Self {
        Self {
            oid,
            files,
            selected: 0,
        }
    }

    fn selected_file(&self) -> Option<&CommitFile> {
        self.files.get(self.selected)
    }

    fn select(&mut self, index: usize) {
        if index < self.files.len() {
            self.selected = index;
        }
    }
}

/// Draws the modal. `true` when it should close.
pub fn show(
    ctx: &egui::Context,
    changes: &mut Changes,
    diffs: &mut DiffCache,
    root: &Path,
    palette: &Palette,
) -> bool {
    if let Some(path) = changes
        .selected_file()
        .map(|file| file.diff_path().to_owned())
    {
        diffs.request(&changes.oid, &path);
    }
    diffs.poll(ctx, root);

    let available = ctx.content_rect();
    let width = (available.width() - 80.0).clamp(560.0, 1200.0);
    let height = (available.height() - 80.0).clamp(320.0, 800.0);
    let response = egui::Modal::new(egui::Id::new(("commit-changes", changes.oid.as_str())))
        .frame(Frame::NONE)
        .backdrop_color(Color32::from_black_alpha(MODAL_BACKDROP_ALPHA))
        .show(ctx, |ui| {
            Frame::popup(ui.style())
                .fill(palette.surface)
                .stroke(Stroke::new(1.0, palette.border))
                .corner_radius(widgets::CORNER_RADIUS)
                .inner_margin(Margin::same(16))
                .show(ui, |ui| {
                    ui.set_min_size(Vec2::new(width, height));
                    ui.set_max_size(Vec2::new(width, height));
                    ui.spacing_mut().item_spacing.y = 8.0;
                    let title = format!("Changes · {}", git::short_oid(&changes.oid));
                    ui.label(RichText::new(title).strong().size(18.0).color(palette.text));
                    let body = ui.available_height();
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        file_list(ui, changes, palette, body);
                        diff_pane(ui, changes, diffs, palette, body);
                    });
                });
        });
    response.should_close()
}

fn file_list(ui: &mut Ui, changes: &mut Changes, palette: &Palette, height: f32) {
    ui.vertical(|ui| {
        ui.set_width(LIST_WIDTH);
        ui.set_height(height);
        ScrollArea::vertical()
            .id_salt(("change-files", changes.oid.as_str()))
            .auto_shrink([false, false])
            .animated(false)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let mut clicked = None;
                for (index, file) in changes.files.iter().enumerate() {
                    if file_row(ui, palette, file, index == changes.selected).clicked() {
                        clicked = Some(index);
                    }
                }
                if let Some(index) = clicked {
                    changes.select(index);
                }
            });
    });
}

fn file_row(ui: &mut Ui, palette: &Palette, file: &CommitFile, selected: bool) -> egui::Response {
    let response = ui
        .scope_builder(
            egui::UiBuilder::new()
                .id_salt(&file.path)
                .sense(Sense::click()),
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
                            let counts = 88.0;
                            let path_width = (ui.available_width() - counts).max(0.0);
                            ui.scope(|ui| {
                                ui.set_max_width(path_width);
                                let label = match &file.previous {
                                    Some(old) => format!("{old} → {}", file.path),
                                    None => file.path.clone(),
                                };
                                ui.add(
                                    Label::new(RichText::new(label).color(palette.text))
                                        .truncate()
                                        .selectable(false),
                                );
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                file_counts(ui, file, palette);
                            });
                        });
                    });
            },
        )
        .response;
    if response.hovered() {
        response.clone().on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

fn file_counts(ui: &mut Ui, file: &CommitFile, palette: &Palette) {
    if file.is_binary() {
        ui.label(RichText::new("binary").small().color(palette.text_muted));
        return;
    }
    if let Some(deletions) = file.deletions.filter(|count| *count > 0) {
        ui.label(
            RichText::new(format!("-{deletions}"))
                .small()
                .color(palette.diff_removed),
        );
    }
    if let Some(insertions) = file.insertions.filter(|count| *count > 0) {
        ui.label(
            RichText::new(format!("+{insertions}"))
                .small()
                .color(palette.diff_added),
        );
    }
}

fn diff_pane(ui: &mut Ui, changes: &Changes, diffs: &DiffCache, palette: &Palette, height: f32) {
    ui.vertical(|ui| {
        ui.set_height(height);
        ui.set_width(ui.available_width());
        let Some(file) = changes.selected_file() else {
            ui.label(RichText::new("No files.").color(palette.text_muted));
            return;
        };
        let heading = match &file.previous {
            Some(old) => format!("{old} → {}", file.path),
            None => file.path.clone(),
        };
        ui.add(
            Label::new(RichText::new(heading).strong().color(palette.text))
                .truncate()
                .selectable(false),
        );
        ui.add_space(6.0);
        match diffs.cached(&changes.oid, file.diff_path()) {
            Some(CachedPatch::Ready(diff)) => paint_diff(ui, diff, palette),
            Some(CachedPatch::Failed(message)) => {
                ui.label(RichText::new(message).color(palette.error));
            }
            Some(CachedPatch::Pending) | None => {
                ui.label(RichText::new("Loading…").color(palette.text_muted));
            }
        }
    });
}

fn paint_diff(ui: &mut Ui, diff: &FileDiff, palette: &Palette) {
    if let Some(notice) = &diff.notice {
        ui.label(RichText::new(notice).color(palette.text_muted));
        return;
    }
    if diff.lines.is_empty() {
        ui.label(RichText::new("No line changes.").color(palette.text_muted));
        return;
    }
    ui.spacing_mut().item_spacing.y = 0.0;
    let row_height = ui.text_style_height(&TextStyle::Monospace);
    let digits = gutter_digits(diff);
    let font = TextStyle::Monospace.resolve(ui.style());
    let advance = ui.fonts_mut(|fonts| fonts.glyph_width(&font, '0')).max(1.0);
    let columns = digits * 2 + 4 + diff.columns;
    let width = (columns as f32 * advance + 16.0).max(ui.available_width());
    ScrollArea::both()
        .id_salt("commit-diff-lines")
        .auto_shrink([false, false])
        .animated(false)
        .show_rows(ui, row_height, diff.lines.len(), |ui, range| {
            ui.set_min_width(width);
            for index in range {
                let line = &diff.lines[index];
                let (rect, _) =
                    ui.allocate_exact_size(Vec2::new(width, row_height), Sense::hover());
                let fill = match line.kind {
                    DiffKind::Added => palette.diff_added_fill,
                    DiffKind::Removed => palette.diff_removed_fill,
                    DiffKind::Context | DiffKind::Hunk | DiffKind::Meta => Color32::TRANSPARENT,
                };
                if fill != Color32::TRANSPARENT {
                    ui.painter().rect_filled(rect, 0.0, fill);
                }
                let job = line_job(line, palette, font.clone(), digits);
                let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
                ui.painter().galley(rect.left_top(), galley, palette.text);
                // The painted galley is not a widget, so the line text is also a
                // label. Tests, and a pointer, can find the words.
                let _ = ui.put(
                    rect,
                    Label::new(
                        RichText::new(&line.text)
                            .monospace()
                            .color(Color32::TRANSPARENT),
                    )
                    .selectable(false),
                );
            }
        });
}

fn gutter_digits(diff: &FileDiff) -> usize {
    let mut max_no = 1u32;
    for line in &diff.lines {
        if let Some(number) = line.old {
            max_no = max_no.max(number);
        }
        if let Some(number) = line.new {
            max_no = max_no.max(number);
        }
    }
    max_no.to_string().len()
}

fn line_job(line: &DiffLine, palette: &Palette, font: FontId, digits: usize) -> LayoutJob {
    let mut job = LayoutJob::default();
    let muted = text_format(palette.text_muted, font.clone());
    let body = text_format(palette.text, font.clone());
    let sign_color = match line.kind {
        DiffKind::Added => palette.diff_added,
        DiffKind::Removed => palette.diff_removed,
        DiffKind::Context | DiffKind::Hunk | DiffKind::Meta => palette.text_muted,
    };
    let old = match line.old {
        Some(number) => format!("{number:>digits$}"),
        None => " ".repeat(digits),
    };
    let new = match line.new {
        Some(number) => format!("{number:>digits$}"),
        None => " ".repeat(digits),
    };
    job.append(&old, 8.0, muted.clone());
    job.append(" ", 0.0, muted.clone());
    job.append(&new, 0.0, muted.clone());
    job.append(" ", 0.0, muted);
    let sign = match line.kind {
        DiffKind::Added => "+",
        DiffKind::Removed => "-",
        DiffKind::Context | DiffKind::Hunk | DiffKind::Meta => " ",
    };
    job.append(sign, 0.0, text_format(sign_color, font.clone()));
    let text_format = match line.kind {
        DiffKind::Hunk | DiffKind::Meta => text_format(palette.text_muted, font),
        DiffKind::Context | DiffKind::Added | DiffKind::Removed => body,
    };
    job.append(&line.text, 0.0, text_format);
    job
}

fn text_format(color: Color32, font_id: FontId) -> TextFormat {
    TextFormat {
        font_id,
        color,
        ..Default::default()
    }
}
