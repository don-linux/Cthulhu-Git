//! The modal that lists a commit's files and paints one unified diff.
//!
//! The file list comes from the hover read. The patch is a separate `git show
//! -p` of the file that was clicked, so a large commit does not pull every
//! diff up front. egui draws only the visible lines.

use std::path::Path;

use cthulhu_git::git::{self, CommitFile, DiffKind, DiffLine, FileDiff, Git, GitError};
use eframe::egui::{
    self, Align, Color32, FontId, Frame, Label, Layout, Margin, RichText, Role, ScrollArea, Sense,
    Stroke, TextStyle, Ui, Vec2, WidgetInfo,
    text::{LayoutJob, TextFormat},
};

use super::async_cache::{AsyncCache, Cached, GitRead};
use super::theme::Palette;
use super::widgets;

const LIST_WIDTH: f32 = 280.0;
const MODAL_BACKDROP_ALPHA: u8 = 160;

/// One file of one commit.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct DiffKey {
    pub oid: String,
    /// [`CommitFile::diff_path`].
    pub path: String,
}

impl GitRead<FileDiff> for DiffKey {
    const LOST: &'static str = "The diff stopped unexpectedly.";

    fn read(&self, git: &Git, root: &Path) -> Result<FileDiff, GitError> {
        git::file_diff(git, root, &self.oid, &self.path)
    }
}

/// Patches already read, kept after the modal closes so returning to a file
/// does not ask git again, and the one `git show -p` in flight.
pub type DiffCache = AsyncCache<DiffKey, FileDiff>;

/// The commit whose files the modal is showing.
pub struct Changes {
    pub oid: String,
    files: Vec<CommitFile>,
    selected: usize,
    /// The selected file's patch, built when the selection changes instead
    /// of every frame. `None` without files.
    key: Option<DiffKey>,
}

impl Changes {
    pub fn new(oid: String, files: Vec<CommitFile>) -> Self {
        let mut changes = Self {
            oid,
            files,
            selected: 0,
            key: None,
        };
        changes.select(0);
        changes
    }

    fn select(&mut self, index: usize) {
        if let Some(file) = self.files.get(index) {
            self.selected = index;
            self.key = Some(DiffKey {
                oid: self.oid.clone(),
                path: file.diff_path().to_owned(),
            });
        }
    }
}

/// Draws the modal. `true` when it should close.
pub fn show(
    ctx: &egui::Context,
    changes: &mut Changes,
    diffs: &mut DiffCache,
    git: &Git,
    root: &Path,
    palette: &Palette,
) -> bool {
    if let Some(key) = &changes.key {
        diffs.request(key);
    }
    diffs.poll(ctx, git, root);

    let available = ctx.content_rect();
    let width = (available.width() - 80.0).clamp(560.0, 1200.0);
    let height = (available.height() - 80.0).clamp(320.0, 800.0);
    let response = egui::Modal::new(egui::Id::unique(("commit-changes", changes.oid.as_str())))
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
                    ui.label(RichText::new(title).strong().size(18.0));
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
    widgets::selectable_row(ui, &file.path, selected, Margin::symmetric(8, 6), |ui| {
        ui.horizontal(|ui| {
            let counts = 88.0;
            let path_width = (ui.available_width() - counts).max(0.0);
            let name = ui
                .scope(|ui| {
                    ui.set_max_width(path_width);
                    ui.add(
                        Label::new(RichText::new(heading(file)))
                            .truncate()
                            .selectable(false),
                    )
                })
                .inner;
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                file_counts(ui, file, palette);
            });
            name
        })
        .inner
    })
}

/// The file's name in the list and above its patch: `old → new` for a
/// rename.
fn heading(file: &CommitFile) -> String {
    match &file.previous {
        Some(old) => format!("{old} → {}", file.path),
        None => file.path.clone(),
    }
}

fn file_counts(ui: &mut Ui, file: &CommitFile, palette: &Palette) {
    if file.is_binary() {
        ui.label(RichText::new("binary").small().weak());
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
        let (Some(file), Some(key)) = (changes.files.get(changes.selected), &changes.key) else {
            ui.label(RichText::new("No files.").weak());
            return;
        };
        ui.add(
            Label::new(RichText::new(heading(file)).strong())
                .truncate()
                .selectable(false),
        );
        ui.add_space(6.0);
        match diffs.cached(key) {
            Some(Cached::Ready(diff)) => paint_diff(ui, diff, palette),
            Some(Cached::Failed(message)) => {
                ui.label(RichText::new(message).color(palette.error));
            }
            Some(Cached::Pending) | None => {
                ui.label(RichText::new("Loading…").weak());
            }
        }
    });
}

fn paint_diff(ui: &mut Ui, diff: &FileDiff, palette: &Palette) {
    if let Some(notice) = &diff.notice {
        ui.label(RichText::new(notice).weak());
        return;
    }
    if diff.lines.is_empty() {
        ui.label(RichText::new("No line changes.").weak());
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
            for line in &diff.lines[range] {
                let (rect, response) =
                    ui.allocate_exact_size(Vec2::new(width, row_height), Sense::hover());
                // The painted galley is not a widget; the row carries the
                // line's text, so a screen reader and the tests find the words.
                response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &line.text));
                let (fill, job) = line_row(line, palette, font.clone(), digits);
                if fill != Color32::TRANSPARENT {
                    ui.painter().rect_filled(rect, 0.0, fill);
                }
                let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
                ui.painter().galley(rect.left_top(), galley, palette.text);
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

/// One line's fill and text: both line numbers, the sign, then the line.
fn line_row(
    line: &DiffLine,
    palette: &Palette,
    font: FontId,
    digits: usize,
) -> (Color32, LayoutJob) {
    let (fill, sign, sign_color, text) = match line.kind {
        DiffKind::Added => (
            palette.diff_added_fill,
            "+",
            palette.diff_added,
            palette.text,
        ),
        DiffKind::Removed => (
            palette.diff_removed_fill,
            "-",
            palette.diff_removed,
            palette.text,
        ),
        DiffKind::Context => (Color32::TRANSPARENT, " ", palette.text_muted, palette.text),
        DiffKind::Hunk | DiffKind::Meta => (
            Color32::TRANSPARENT,
            " ",
            palette.text_muted,
            palette.text_muted,
        ),
    };
    let number = |number: Option<u32>| match number {
        Some(number) => format!("{number:>digits$}"),
        None => " ".repeat(digits),
    };
    let mut job = LayoutJob::default();
    job.append(
        &format!("{} {} ", number(line.old), number(line.new)),
        8.0,
        text_format(palette.text_muted, font.clone()),
    );
    job.append(sign, 0.0, text_format(sign_color, font.clone()));
    job.append(&line.text, 0.0, text_format(text, font));
    (fill, job)
}

fn text_format(color: Color32, font_id: FontId) -> TextFormat {
    TextFormat {
        font_id,
        color,
        ..Default::default()
    }
}
