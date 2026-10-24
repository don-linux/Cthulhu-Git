//! The commit history table: Branch, Graph, Commit Hash, Commit Message and
//! Author, in that order.
//!
//! One [`ColumnLayout`] per frame places the header and every row, so each
//! title starts exactly where its column's content starts. The columns are
//! independent: each keeps its own share of the panel and its own minimum,
//! and resizing one never changes another. When they add up to more than the
//! panel, the table scrolls sideways.

use std::sync::Arc;

use cthulhu_git::git::{GraphBranch, GraphRow, Person};
use cthulhu_git::github::{GitHubRepo, RepoContext};
use cthulhu_git::settings::HistoryColumns;
use eframe::egui::text::{LayoutJob, TextFormat, TextWrapping};
use eframe::egui::{
    self, Align, Color32, CursorIcon, FontId, Galley, Id, Label, Layout, Pos2, Rect, RichText,
    ScrollArea, Sense, Stroke, TextStyle, Ui, UiBuilder, Vec2, WidgetInfo, WidgetType,
};

use super::avatars::{self, AvatarService};
use super::hover::{self, CardContext, HoverCache};
use super::icons::{self, Icon};
use super::theme::Palette;
use super::{ListedBranch, OpenedRepo};

/// Commit text and branch labels, relative to egui's body size.
const TEXT_SCALE: f32 = 1.3;
/// Space between a column's edge and its content. Titles use the same, so
/// each title starts where its content does.
pub(super) const CELL_PAD: f32 = 8.0;
const HEADER_HEIGHT: f32 = 30.0;
const ROW_PAD: f32 = 18.0;
const ROW_MIN: f32 = 40.0;
pub(super) const LANE_MIN: f32 = 14.0;
pub(super) const LANE_MAX: f32 = 28.0;
const LANE_STROKE: f32 = 3.0;
const DOT_OUTER: f32 = 8.0;
const DOT_INNER: f32 = 6.0;
const CONNECTOR_STROKE: f32 = 1.5;
const CHIP_ICON: f32 = 16.0;
const CHIP_PAD: f32 = 5.0;
const AVATAR: f32 = 26.0;
/// How far each stacked avatar sits from the previous one.
const AVATAR_STEP: f32 = AVATAR * 0.62;
const MAX_STACKED: usize = 3;
const DIVIDER_GRAB: f32 = 8.0;
const BRANCH_MIN: f32 = 160.0;
const MESSAGE_MIN: f32 = 280.0;
const AUTHOR_MIN: f32 = 180.0;
/// Twelve hex digits ([`cthulhu_git::git::SHORT_OID_LEN`]) set the hash column's minimum.
const HASH_SAMPLE: &str = "0123456789ab";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Column {
    Branch,
    Graph,
    Hash,
    Message,
    Author,
}

impl Column {
    pub(super) const ALL: [Self; 5] = [
        Self::Branch,
        Self::Graph,
        Self::Hash,
        Self::Message,
        Self::Author,
    ];

    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Branch => "Branch",
            Self::Graph => "Graph",
            Self::Hash => "Commit Hash",
            Self::Message => "Commit Message",
            Self::Author => "Author",
        }
    }

    fn index(self) -> usize {
        self as usize
    }

    fn per_mille(self, columns: &HistoryColumns) -> u16 {
        match self {
            Self::Branch => columns.branch,
            Self::Graph => columns.graph,
            Self::Hash => columns.hash,
            Self::Message => columns.message,
            Self::Author => columns.author,
        }
    }

    fn set(self, columns: &mut HistoryColumns, value: u16) {
        let field = match self {
            Self::Branch => &mut columns.branch,
            Self::Graph => &mut columns.graph,
            Self::Hash => &mut columns.hash,
            Self::Message => &mut columns.message,
            Self::Author => &mut columns.author,
        };
        *field = value;
    }
}

/// Where each column sits, from the left edge of the table's content.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ColumnLayout {
    pub left: [f32; 5],
    pub width: [f32; 5],
    /// The columns' sum, or the panel when that is wider, so a row's
    /// highlight still reaches the right edge.
    pub content_width: f32,
    pub lane_width: f32,
}

impl ColumnLayout {
    fn cell(&self, row: Rect, column: Column) -> Rect {
        let index = column.index();
        Rect::from_min_size(
            Pos2::new(row.left() + self.left[index], row.top()),
            Vec2::new(self.width[index], row.height()),
        )
    }
}

/// Each column's minimum, from its own content only.
pub(super) fn column_mins(hash_text_width: f32, lanes: usize) -> [f32; 5] {
    [
        BRANCH_MIN,
        lanes.max(1) as f32 * LANE_MIN + 2.0 * CELL_PAD,
        hash_text_width + 2.0 * CELL_PAD,
        MESSAGE_MIN,
        AUTHOR_MIN,
    ]
}

/// A column is its share of `panel`, or its minimum when that is larger.
/// Nothing is moved between columns.
pub(super) fn layout_columns(
    panel: f32,
    columns: &HistoryColumns,
    mins: [f32; 5],
    lanes: usize,
) -> ColumnLayout {
    let panel = panel.max(0.0);
    let mut left = [0.0; 5];
    let mut width = [0.0; 5];
    let mut x = 0.0;
    for column in Column::ALL {
        let index = column.index();
        let share = panel * f32::from(column.per_mille(columns)) / 1000.0;
        left[index] = x;
        width[index] = share.max(mins[index]);
        x += width[index];
    }
    let graph = width[Column::Graph.index()];
    let lane_width = ((graph - 2.0 * CELL_PAD) / lanes.max(1) as f32).clamp(LANE_MIN, LANE_MAX);
    ColumnLayout {
        left,
        width,
        content_width: x.max(panel),
        lane_width,
    }
}

/// The stored share for a column dragged to `width` points.
pub(super) fn per_mille_for(width: f32, panel: f32) -> u16 {
    if panel <= 0.0 || !width.is_finite() {
        return *HistoryColumns::RANGE.start();
    }
    let range = HistoryColumns::RANGE;
    (width / panel * 1000.0)
        .round()
        .clamp(f32::from(*range.start()), f32::from(*range.end())) as u16
}

fn default_per_mille(column: Column) -> u16 {
    column.per_mille(&HistoryColumns::default())
}

/// Draws the history and returns new column widths when a divider was released
/// or double-clicked, for the app to save.
///
/// When `loading` is set and there are no rows yet, the muted line says the
/// history is still opening. A finished load with no commits keeps
/// "No commits yet."
pub fn show(
    ui: &mut Ui,
    repo: &mut OpenedRepo,
    palette: &Palette,
    saved: HistoryColumns,
    avatars: &mut AvatarService,
    loading: bool,
) -> Option<HistoryColumns> {
    // While this view's log is still running, an empty table is not "no commits".
    let empty = if loading {
        "Opening history…"
    } else {
        empty_history(&repo.branches, repo.detached_oid.is_some())
    };
    repo.hover.poll(ui.ctx(), &repo.info.root);
    let OpenedRepo {
        graph,
        hover,
        github,
        view_id,
        ..
    } = repo;
    let rows = &graph.rows;
    let count = if graph.truncated {
        format!("{}+", rows.len())
    } else {
        rows.len().to_string()
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
    if rows.is_empty() {
        // The column between the sidebars can be narrower than these sentences.
        // Wrap inside it; truncating would drop the line the loading state shows.
        wrapped_line(ui, empty, palette.text_muted);
        return None;
    }
    if graph.truncated {
        ui.label(
            RichText::new(format!("Showing the latest {} commits.", rows.len()))
                .color(palette.text_muted),
        );
        ui.add_space(4.0);
    }

    let live_id = view_id.with("history-columns-live");
    let origin_id = view_id.with("history-origin-x");
    let mut live: HistoryColumns = ui
        .ctx()
        .data(|data| data.get_temp(live_id))
        .unwrap_or(saved);
    let text_font = scaled(ui, TextStyle::Body);
    let mono_font = scaled(ui, TextStyle::Monospace);
    let hash_width = ui
        .painter()
        .layout_no_wrap(
            HASH_SAMPLE.to_owned(),
            mono_font.clone(),
            Color32::PLACEHOLDER,
        )
        .size()
        .x;
    let panel = ui.available_width();
    let mins = column_mins(hash_width, graph.columns);
    let mut layout = layout_columns(panel, &live, mins, graph.columns);
    // The dividers are read before the layout is used, so a drag follows the
    // pointer this frame and the sample at release is kept.
    let previous_origin: Option<f32> = ui.ctx().data(|data| data.get_temp(origin_id));
    let mut dragging = false;
    let mut done = None;
    if let Some(origin) = previous_origin {
        for column in Column::ALL {
            let index = column.index();
            let Some(response) = ui.ctx().read_response(divider_id(*view_id, index)) else {
                continue;
            };
            if response.double_clicked() {
                column.set(&mut live, default_per_mille(column));
                done = Some(live);
            } else if (response.dragged() || response.drag_stopped())
                && let Some(pointer) = response.interact_pointer_pos()
            {
                let wanted = (pointer.x - (origin + layout.left[index])).max(mins[index]);
                column.set(&mut live, share_for(wanted, mins[index], panel));
                if response.drag_stopped() {
                    done = Some(live);
                } else {
                    dragging = true;
                }
            }
        }
        layout = layout_columns(panel, &live, mins, graph.columns);
    }
    let line = ui
        .text_style_height(&TextStyle::Monospace)
        .max(ui.text_style_height(&TextStyle::Body))
        * TEXT_SCALE;
    let row_height = (line + ROW_PAD).max(ROW_MIN);
    let context = github.as_ref().map(|repo| RepoContext {
        repo: repo.clone(),
        head: rows[0].oid.clone(),
    });

    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        let (header_rect, _) =
            ui.allocate_exact_size(Vec2::new(panel, HEADER_HEIGHT), Sense::hover());
        let mut cells = RowContext {
            palette,
            avatars,
            hover,
            github: github.as_ref(),
            repo: context.as_ref(),
            text_font,
            mono_font,
        };
        // Where the rows were placed this frame. The scroll area's returned
        // offset already includes this frame's wheel and scrollbar input,
        // which the rows only follow next frame.
        let mut origin_x = header_rect.left();
        ScrollArea::both()
            .id_salt(view_id.with("history"))
            .auto_shrink([false, false])
            .show_rows(ui, row_height, rows.len(), |ui, range| {
                origin_x = ui.max_rect().left();
                ui.set_min_width(layout.content_width);
                for row in &rows[range] {
                    history_row(ui, row, &layout, row_height, &mut cells);
                }
            });
        ui.ctx()
            .data_mut(|data| data.insert_temp(origin_id, origin_x));
        header(ui, header_rect, origin_x, &layout, *view_id, palette);
    });

    if let Some(columns) = done {
        ui.ctx()
            .data_mut(|data| data.remove::<HistoryColumns>(live_id));
        return Some(columns);
    }
    if dragging {
        ui.ctx().data_mut(|data| data.insert_temp(live_id, live));
        ui.ctx().request_repaint();
    }
    None
}

fn divider_id(view_id: Id, index: usize) -> Id {
    view_id.with(("history-divider", index))
}

/// The share to store for a column dragged to `wanted` points. A column
/// pushed against its minimum stores that minimum's share, so the saved
/// width matches the one on screen. When the panel is narrower than the
/// minimum, that share would be the whole panel or more; the smallest share
/// is stored instead, so a wider window later does not hand the column most
/// of the panel. The minimum holds it either way.
fn share_for(wanted: f32, min: f32, panel: f32) -> u16 {
    if wanted <= min + 0.5 && panel > 0.0 && min / panel * 1000.0 > 1000.0 {
        return *HistoryColumns::RANGE.start();
    }
    per_mille_for(wanted, panel)
}

fn scaled(ui: &Ui, style: TextStyle) -> FontId {
    let mut font = style.resolve(ui.style());
    font.size *= TEXT_SCALE;
    font
}

/// The full sentence, broken so every row stays inside `ui`'s width.
///
/// Word wrap alone lets one token paint past the history column into the
/// detail sidebar. Breaking anywhere keeps the galley in the column without
/// eliding it.
fn wrapped_line(ui: &mut Ui, text: &str, color: Color32) {
    let width = ui.available_width().max(0.0);
    let mut job = LayoutJob::single_section(
        text.to_owned(),
        TextFormat {
            font_id: TextStyle::Body.resolve(ui.style()),
            color,
            valign: ui.text_valign(),
            ..TextFormat::default()
        },
    );
    job.wrap = TextWrapping {
        max_width: width,
        max_rows: usize::MAX,
        break_anywhere: true,
        overflow_character: None,
    };
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    ui.add(Label::new(galley));
}

fn empty_history(branches: &[ListedBranch], detached: bool) -> &'static str {
    let selectable = branches.iter().any(|listed| listed.branch.oid.is_some());
    let selected = branches
        .iter()
        .any(|listed| listed.checked && listed.branch.oid.is_some())
        || detached;
    if selectable && !selected {
        "No branches selected."
    } else {
        "No commits yet."
    }
}

/// Titles at `column.left + CELL_PAD`, and a divider on each column's right
/// edge that resizes that column only. `origin_x` is where the rows start on
/// screen this frame, so titles move with their columns.
fn header(
    ui: &mut Ui,
    rect: Rect,
    origin_x: f32,
    layout: &ColumnLayout,
    view_id: Id,
    palette: &Palette,
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, palette.surface);
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, palette.border),
    );
    for column in Column::ALL {
        let index = column.index();
        let left = origin_x + layout.left[index];
        let width = layout.width[index];
        let title = Rect::from_min_max(
            Pos2::new(left + CELL_PAD, rect.top()),
            Pos2::new(
                (left + width - CELL_PAD).max(left + CELL_PAD),
                rect.bottom(),
            ),
        );
        if title.right() > rect.left() && title.left() < rect.right() {
            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(title)
                    .layout(Layout::left_to_right(Align::Center)),
                |ui| {
                    ui.set_clip_rect(title.intersect(rect));
                    ui.add(
                        Label::new(
                            RichText::new(column.title())
                                .strong()
                                .color(palette.text_muted),
                        )
                        .selectable(false)
                        .truncate(),
                    );
                },
            );
        }

        let x = left + width;
        // The last edge often sits on the panel's right side; keep its grab
        // area inside the clip so all of it can be grabbed.
        let grab_x = if (x - rect.right()).abs() <= DIVIDER_GRAB {
            x.min(rect.right() - DIVIDER_GRAB / 2.0)
        } else {
            x
        };
        // Registered even when scrolled out of view: a drag that leaves the
        // header must still see its release.
        let grab = Rect::from_center_size(
            Pos2::new(grab_x, rect.center().y),
            Vec2::new(DIVIDER_GRAB, rect.height()),
        );
        let response = ui
            .interact(grab, divider_id(view_id, index), Sense::click_and_drag())
            .on_hover_cursor(CursorIcon::ResizeColumn);
        response.widget_info(|| {
            WidgetInfo::labeled(
                WidgetType::Other,
                true,
                format!("Resize {}", column.title()),
            )
        });
        let active = response.hovered() || response.dragged();
        painter.vline(
            x,
            rect.y_range().shrink(6.0),
            Stroke::new(
                if active { 2.0 } else { 1.0 },
                if active {
                    palette.accent
                } else {
                    palette.border
                },
            ),
        );
    }
}

struct RowContext<'a> {
    palette: &'a Palette,
    avatars: &'a mut AvatarService,
    hover: &'a mut HoverCache,
    github: Option<&'a GitHubRepo>,
    repo: Option<&'a RepoContext>,
    text_font: FontId,
    mono_font: FontId,
}

/// `cell` without its padding: where the content and the title start.
fn content(cell: Rect) -> Rect {
    Rect::from_min_max(
        Pos2::new(cell.left() + CELL_PAD, cell.top()),
        Pos2::new(
            (cell.right() - CELL_PAD).max(cell.left() + CELL_PAD),
            cell.bottom(),
        ),
    )
}

fn history_row(
    ui: &mut Ui,
    row: &GraphRow,
    layout: &ColumnLayout,
    row_height: f32,
    cx: &mut RowContext<'_>,
) {
    let (_, rect) = ui.allocate_space(Vec2::new(layout.content_width, row_height));
    if !ui.is_rect_visible(rect) {
        return;
    }
    let row_id = ui.id().with(("history-row", row.oid.as_str()));
    let response = ui.interact(rect, row_id, Sense::hover());
    let palette = cx.palette;
    let fill = if response.hovered() {
        ui.painter().rect_filled(rect, 0.0, palette.surface_hover);
        palette.surface_hover
    } else {
        palette.background
    };

    let branch = content(layout.cell(rect, Column::Branch));
    let graph = layout.cell(rect, Column::Graph);
    let chip = branch_chip(ui, branch, row, palette, &cx.text_font);
    {
        let painter = ui.painter();
        // The connector goes under the lanes, so a vertical stroke stays
        // continuous where it crosses.
        if let Some(chip) = &chip {
            let dot_x = lane_center(graph, row.commit_lane, layout.lane_width);
            if dot_x - chip.rect.right() > 0.5 {
                painter.line_segment(
                    [
                        Pos2::new(chip.rect.right(), rect.center().y),
                        Pos2::new(dot_x, rect.center().y),
                    ],
                    Stroke::new(CONNECTOR_STROKE, palette.graph_connector(chip.color)),
                );
            }
        }
        let lanes = painter.with_clip_rect(graph.intersect(ui.clip_rect()));
        paint_lanes(&lanes, graph, row, palette, layout.lane_width, fill);
    }
    if let Some(chip) = &chip {
        paint_branch_chip(ui, chip, palette);
    }

    text_cell(
        ui,
        content(layout.cell(rect, Column::Hash)),
        cthulhu_git::git::short_oid(&row.oid),
        cx.mono_font.clone(),
        palette.hash,
        row_id.with("hash"),
    );
    text_cell(
        ui,
        content(layout.cell(rect, Column::Message)),
        &row.summary,
        cx.text_font.clone(),
        palette.text,
        row_id.with("message"),
    );
    author_cell(
        ui,
        content(layout.cell(rect, Column::Author)),
        row,
        fill,
        cx,
        row_id.with("author"),
    );

    // The card opens from Author only. A row is wide, and a card on every
    // column would follow the pointer across the whole commit.
    ui.interact(
        layout.cell(rect, Column::Author),
        row_id.with("card"),
        Sense::hover(),
    )
    .on_hover_ui(|ui| {
        hover::card(
            ui,
            row,
            cx.hover,
            cx.avatars,
            &CardContext {
                palette: cx.palette,
                github: cx.github,
                repo: cx.repo,
            },
        );
    });
}

/// One line, ellipsized to `max_width`.
fn elided(ui: &Ui, text: &str, font: FontId, color: Color32, max_width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(
        text.to_owned(),
        TextFormat {
            font_id: font,
            color,
            valign: Align::Center,
            ..TextFormat::default()
        },
    );
    job.wrap = TextWrapping {
        max_width: max_width.max(0.0),
        max_rows: 1,
        // Hashes and branch names are one token, so the ellipsis may land
        // inside a word.
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.painter().layout_job(job)
}

/// Painted text with an accessibility node carrying the full text. Painted
/// instead of a `Label` so a cell does not open its own card. The card
/// belongs to the Author column.
fn text_cell(ui: &mut Ui, rect: Rect, text: &str, font: FontId, color: Color32, id: Id) {
    if rect.width() <= 0.0 {
        return;
    }
    let galley = elided(ui, text, font, color, rect.width());
    let pos = Pos2::new(rect.left(), rect.center().y - galley.size().y / 2.0);
    let text_rect = Rect::from_min_size(pos, galley.size());
    ui.painter()
        .with_clip_rect(rect.intersect(ui.clip_rect()))
        .galley(pos, galley, color);
    ui.interact(text_rect, id, Sense::hover())
        .widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text));
}

fn display_name(person: &Person) -> &str {
    if person.name.trim().is_empty() {
        &person.email
    } else {
        &person.name
    }
}

/// The author and co-authors: up to [`MAX_STACKED`] overlapping avatars, a
/// `+N` for the rest, then every name the column has room for.
fn author_cell(
    ui: &mut Ui,
    rect: Rect,
    row: &GraphRow,
    fill: Color32,
    cx: &mut RowContext<'_>,
    id: Id,
) {
    if rect.width() <= 0.0 {
        return;
    }
    let people: Vec<&Person> = std::iter::once(&row.author)
        .chain(&row.co_authors)
        .collect();
    let shown = people.len().min(MAX_STACKED);
    let center_y = rect.center().y;
    let center_x = |index: usize| rect.left() + AVATAR / 2.0 + index as f32 * AVATAR_STEP;
    let textures: Vec<_> = people[..shown]
        .iter()
        .enumerate()
        .map(|(index, person)| {
            let authored = (index == 0).then_some(row.oid.as_str());
            cx.avatars.texture(ui.ctx(), person, cx.repo, authored)
        })
        .collect();
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    // The author is drawn last, on top of the co-authors.
    for index in (0..shown).rev() {
        avatars::paint_avatar(
            &painter,
            Pos2::new(center_x(index), center_y),
            AVATAR,
            people[index],
            textures[index].as_ref(),
            cx.palette,
            (shown > 1).then_some(fill),
        );
    }
    let mut text_left = center_x(shown - 1) + AVATAR / 2.0 + 6.0;
    let hidden = people.len() - shown;
    if hidden > 0 {
        let more = elided(
            ui,
            &format!("+{hidden}"),
            cx.text_font.clone(),
            cx.palette.text_muted,
            f32::INFINITY,
        );
        let width = more.size().x;
        painter.galley(
            Pos2::new(text_left, center_y - more.size().y / 2.0),
            more,
            cx.palette.text_muted,
        );
        text_left += width + 6.0;
    }
    let names = people
        .iter()
        .map(|person| display_name(person))
        .collect::<Vec<_>>()
        .join(", ");
    let text_rect = Rect::from_min_max(Pos2::new(text_left, rect.top()), rect.max);
    if text_rect.width() > 0.0 {
        let galley = elided(
            ui,
            &names,
            cx.text_font.clone(),
            cx.palette.text,
            text_rect.width(),
        );
        painter.galley(
            Pos2::new(text_left, center_y - galley.size().y / 2.0),
            galley,
            cx.palette.text,
        );
    }
    ui.interact(rect, id, Sense::hover())
        .widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, names.as_str()));
}

fn lane_center(graph: Rect, lane: usize, lane_width: f32) -> f32 {
    graph.left() + CELL_PAD + lane as f32 * lane_width + lane_width / 2.0
}

fn paint_lanes(
    painter: &egui::Painter,
    graph: Rect,
    row: &GraphRow,
    palette: &Palette,
    lane_width: f32,
    fill: Color32,
) {
    let dot = Pos2::new(
        lane_center(graph, row.commit_lane, lane_width),
        graph.center().y,
    );
    for trace in &row.traces {
        let start = match trace.from_lane {
            Some(lane) => Pos2::new(lane_center(graph, lane, lane_width), graph.top()),
            None => dot,
        };
        let end = match trace.to_lane {
            Some(lane) => Pos2::new(lane_center(graph, lane, lane_width), graph.bottom()),
            None => dot,
        };
        if start.distance(end) < 0.5 {
            continue;
        }
        painter.line_segment(
            [start, end],
            Stroke::new(LANE_STROKE, palette.graph_color(trace.color)),
        );
    }
    // At the narrowest lanes the dots shrink so neighbors do not touch.
    let outer = DOT_OUTER.min(lane_width / 2.0 - 1.0);
    let inner = DOT_INNER.min(outer - 1.5).max(1.0);
    painter.circle_filled(dot, outer, fill);
    painter.circle_filled(dot, inner, palette.graph_color(row.commit_color));
}

/// Checked-out branch first, then the sidebar order the row already has.
pub(super) fn ordered_branches(branches: &[GraphBranch]) -> Vec<&GraphBranch> {
    let mut order: Vec<_> = branches.iter().enumerate().collect();
    order.sort_by_key(|(index, branch)| (!branch.current, *index));
    order.into_iter().map(|(_, branch)| branch).collect()
}

struct BranchChip {
    rect: Rect,
    color: usize,
    current: bool,
    names: String,
    oid: String,
    text: Arc<Galley>,
    plus: Option<Arc<Galley>>,
}

/// The label starts at the left of the branch column, under its title.
/// Several tips on one commit show the first name and a `+N`; the hover card
/// lists every name.
fn branch_chip(
    ui: &Ui,
    column: Rect,
    row: &GraphRow,
    palette: &Palette,
    font: &FontId,
) -> Option<BranchChip> {
    let ordered = ordered_branches(&row.branches);
    let primary = *ordered.first()?;
    let color = palette.graph_color(primary.color);
    let extra = ordered.len().saturating_sub(1);
    let plus = (extra > 0).then(|| {
        ui.painter()
            .layout_no_wrap(format!("+{extra}"), font.clone(), color)
    });
    let plus_width = plus.as_ref().map_or(0.0, |galley| galley.size().x);
    let plus_gap = if plus.is_some() { CHIP_PAD } else { 0.0 };
    let chrome = CHIP_PAD + CHIP_ICON + CHIP_PAD + plus_gap + plus_width + CHIP_PAD;
    let text = elided(
        ui,
        &primary.name,
        font.clone(),
        color,
        column.width() - chrome,
    );
    let chip_width = (chrome + text.size().x).min(column.width());
    let chip_height = (text.size().y.max(CHIP_ICON) + CHIP_PAD).min(column.height());
    let rect = Rect::from_min_size(
        Pos2::new(column.left(), column.center().y - chip_height / 2.0),
        Vec2::new(chip_width, chip_height),
    );
    let names = ordered
        .iter()
        .map(|branch| branch.name.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    Some(BranchChip {
        rect,
        color: primary.color,
        current: primary.current,
        names,
        oid: row.oid.clone(),
        text,
        plus,
    })
}

fn paint_branch_chip(ui: &mut Ui, chip: &BranchChip, palette: &Palette) {
    let color = palette.graph_color(chip.color);
    let icon_rect = Rect::from_center_size(
        Pos2::new(
            chip.rect.left() + CHIP_PAD + CHIP_ICON / 2.0,
            chip.rect.center().y,
        ),
        Vec2::splat(CHIP_ICON),
    );
    {
        let painter = ui
            .painter()
            .with_clip_rect(chip.rect.intersect(ui.clip_rect()));
        painter.rect_filled(chip.rect, 4.0, palette.graph_chip(chip.color, chip.current));
        icons::paint_icon(&painter, ui.ctx(), icon_rect, Icon::GitBranch, color);
        let text_x = icon_rect.right() + CHIP_PAD;
        painter.galley(
            Pos2::new(text_x, chip.rect.center().y - chip.text.size().y / 2.0),
            chip.text.clone(),
            color,
        );
        if let Some(plus) = &chip.plus {
            painter.galley(
                Pos2::new(
                    chip.rect.right() - CHIP_PAD - plus.size().x,
                    chip.rect.center().y - plus.size().y / 2.0,
                ),
                plus.clone(),
                color,
            );
        }
    }
    // Painted text is invisible to accesskit unless the widget carries it.
    ui.interact(
        chip.rect,
        ui.id().with(("branch-label", chip.oid.as_str())),
        Sense::hover(),
    )
    .widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, chip.names.as_str()));
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH_TEXT: f32 = 100.0;

    fn mins(lanes: usize) -> [f32; 5] {
        column_mins(HASH_TEXT, lanes)
    }

    fn widths(layout: &ColumnLayout) -> [f32; 5] {
        layout.width
    }

    #[test]
    fn the_defaults_fill_a_wide_panel_exactly() {
        let layout = layout_columns(2000.0, &HistoryColumns::default(), mins(1), 1);
        let sum: f32 = layout.width.iter().sum();
        assert!((sum - 2000.0).abs() < 0.01, "sum {sum}");
        assert!((layout.content_width - 2000.0).abs() < 0.01);
        for index in 1..5 {
            assert!(
                (layout.left[index] - (layout.left[index - 1] + layout.width[index - 1])).abs()
                    < 0.01
            );
        }
    }

    #[test]
    fn changing_one_column_never_changes_another() {
        let base = HistoryColumns::default();
        for panel in [300.0, 900.0, 1600.0, 3000.0] {
            let before = layout_columns(panel, &base, mins(3), 3);
            for column in Column::ALL {
                for value in [20, 50, 400, 1000] {
                    let mut changed = base;
                    column.set(&mut changed, value);
                    let after = layout_columns(panel, &changed, mins(3), 3);
                    for other in Column::ALL.into_iter().filter(|other| *other != column) {
                        assert_eq!(
                            widths(&before)[other.index()],
                            widths(&after)[other.index()],
                            "{column:?}={value} moved {other:?} at {panel}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_narrow_panel_scrolls_instead_of_squeezing() {
        let layout = layout_columns(196.0, &HistoryColumns::default(), mins(2), 2);
        for (index, (width, floor)) in layout.width.iter().zip(mins(2)).enumerate() {
            assert!(width + 0.01 >= floor, "column {index}");
        }
        let sum: f32 = layout.width.iter().sum();
        assert!(layout.content_width > 196.0);
        assert!((layout.content_width - sum).abs() < 0.01);
    }

    #[test]
    fn narrow_columns_leave_free_space_on_the_right() {
        let small = HistoryColumns {
            branch: 20,
            graph: 20,
            hash: 20,
            message: 20,
            author: 20,
        };
        let layout = layout_columns(4000.0, &small, mins(1), 1);
        let sum: f32 = layout.width.iter().sum();
        assert!(sum < 4000.0);
        assert!((layout.content_width - 4000.0).abs() < 0.01);
    }

    #[test]
    fn the_graph_minimum_comes_from_its_own_lanes() {
        let many = layout_columns(600.0, &HistoryColumns::default(), mins(40), 40);
        let graph = many.width[Column::Graph.index()];
        assert!(graph + 0.01 >= 40.0 * LANE_MIN + 2.0 * CELL_PAD);
        assert!((many.lane_width - LANE_MIN).abs() < 0.01);
        let one = layout_columns(2000.0, &HistoryColumns::default(), mins(1), 1);
        assert!((one.lane_width - LANE_MAX).abs() < 0.01);
        assert!(one.width[Column::Graph.index()] > LANE_MAX + 2.0 * CELL_PAD);
    }

    #[test]
    fn layout_survives_zero_and_odd_inputs() {
        for panel in [0.0, -5.0, f32::MIN_POSITIVE] {
            let layout = layout_columns(panel, &HistoryColumns::default(), mins(0), 0);
            for value in layout.width.iter().chain(&layout.left) {
                assert!(value.is_finite() && *value >= 0.0);
            }
            assert!(layout.lane_width >= LANE_MIN);
        }
    }

    #[test]
    fn dragged_widths_become_shares_of_the_panel() {
        assert_eq!(per_mille_for(500.0, 1000.0), 500);
        assert_eq!(per_mille_for(1.0, 1000.0), 20);
        assert_eq!(per_mille_for(5000.0, 1000.0), 1000);
        assert_eq!(per_mille_for(500.0, 0.0), 20);
        assert_eq!(per_mille_for(f32::NAN, 1000.0), 20);
    }

    #[test]
    fn a_column_dragged_to_its_minimum_stores_the_smallest_share() {
        // A 260-point panel cannot hold the 280-point message minimum. Saving
        // 280/260 as 1000 would make the column fill a wider window later.
        assert_eq!(share_for(280.0, MESSAGE_MIN, 260.0), 20);
        let later = layout_columns(
            2000.0,
            &HistoryColumns {
                message: share_for(280.0, MESSAGE_MIN, 260.0),
                ..HistoryColumns::default()
            },
            mins(1),
            1,
        );
        assert!((later.width[Column::Message.index()] - MESSAGE_MIN).abs() < 0.01);
        assert_eq!(share_for(600.0, MESSAGE_MIN, 1000.0), 600);
        // On a panel that fits the minimum, the saved share is the one on screen.
        assert_eq!(share_for(MESSAGE_MIN, MESSAGE_MIN, 1660.0), 169);
    }

    #[test]
    fn titles_read_in_the_requested_order() {
        let titles: Vec<_> = Column::ALL.iter().map(|column| column.title()).collect();
        assert_eq!(
            titles,
            ["Branch", "Graph", "Commit Hash", "Commit Message", "Author"]
        );
    }

    #[test]
    fn the_checked_out_branch_is_labeled_before_the_others() {
        let branches = [
            GraphBranch {
                name: "feature".to_owned(),
                color: 1,
                current: false,
            },
            GraphBranch {
                name: "main".to_owned(),
                color: 0,
                current: true,
            },
            GraphBranch {
                name: "topic".to_owned(),
                color: 2,
                current: false,
            },
        ];
        let ordered = ordered_branches(&branches);
        assert_eq!(
            ordered
                .iter()
                .map(|branch| branch.name.as_str())
                .collect::<Vec<_>>(),
            ["main", "feature", "topic"]
        );
    }
}
