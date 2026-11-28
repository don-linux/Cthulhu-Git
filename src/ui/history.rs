//! The commit history table: Branch, Graph, Commit Hash, Commit Message and
//! Author, in that order.
//!
//! One [`ColumnLayout`] per frame places the header and every row, so each
//! title starts exactly where its column's content starts. The columns are
//! independent: each keeps its own share of the panel and its own minimum,
//! and resizing one never changes another. When they add up to more than the
//! panel, the table scrolls sideways.

use std::sync::Arc;
use std::time::Duration;

use cthulhu_git::git::{GraphBranch, GraphRow, Person};
use cthulhu_git::github::{GitHubRepo, RepoContext};
use cthulhu_git::settings::HistoryColumns;
use eframe::egui::text::{LayoutJob, TextFormat, TextWrapping};
use eframe::egui::{
    self, Align, Color32, CursorIcon, FontId, Galley, Id, Label, Layout, Mesh, Pos2, Rect,
    RichText, Role, ScrollArea, Sense, Stroke, TextStyle, Ui, UiBuilder, Vec2, WidgetInfo,
};

use super::avatars::{self, AvatarService};
use super::hover::{self, CardContext, HoverCache};
use super::icons::{self, Icon};
use super::theme::Palette;
use super::{ListedBranch, OpenedRepo};
use super::{text, widgets};

/// Commit text and branch labels, relative to egui's body size.
const TEXT_SCALE: f32 = 1.3;
/// Space between a column's edge and its content. Titles use the same, so
/// each title starts where its content does.
pub(super) const CELL_PAD: f32 = 8.0;
const HEADER_HEIGHT: f32 = 30.0;
const ROW_PAD: f32 = 18.0;
const ROW_MIN: f32 = 40.0;
const LANE_MIN: f32 = 14.0;
const LANE_MAX: f32 = 28.0;
const LANE_STROKE: f32 = 3.0;
const DOT_OUTER: f32 = 8.0;
const DOT_INNER: f32 = 6.0;
const CONNECTOR_STROKE: f32 = 1.5;
const CHIP_ICON: f32 = 16.0;
const CHIP_PAD: f32 = 5.0;
/// Extra fill on the branch tag while its line is lit. The tag stays a steady
/// fill; only the lane carries the traveling crest.
const CHIP_LIT_BOOST: u8 = 40;
/// Halo around a lit lane, measured from the stroke's center. On the narrowest
/// lane the next stroke starts 5.5 points away, so this does not reach it.
const GLOW_RADIUS: f32 = 5.0;
/// One crest travels this far down the branch. About sixteen rows, so a
/// typical window shows a single crest.
const GLOW_WAVELENGTH: f32 = 640.0;
/// How wide the crest is. A couple of rows stay bright, then it fades.
const GLOW_SIGMA: f32 = 110.0;
/// Seconds for the crest to travel one [`GLOW_WAVELENGTH`], from newer
/// commits toward older ones.
const GLOW_PERIOD: f64 = 3.2;
/// Alpha at the stroke's edge when the crest is elsewhere. The opaque stroke
/// covers the center of the ribbon, so this is the glow that actually shows.
const GLOW_ALPHA_BASE: u8 = 96;
/// Alpha at the stroke's edge where the crest is.
const GLOW_ALPHA_PEAK: u8 = 188;
/// Middle of the falloff outside the stroke. Full alpha holds to the stroke
/// edge; the halo is clear at [`GLOW_RADIUS`].
const GLOW_SHOULDER: f32 = 3.2;
/// Share of the local alpha at [`GLOW_SHOULDER`].
const GLOW_EDGE_SHARE: f32 = 0.45;
/// Distance between samples along a segment, so a diagonal lane change
/// carries the crest instead of wearing one flat tint.
const GLOW_SAMPLE: f32 = 10.0;
/// The spark in front of the traveling light. Narrow, so it reads as a spot
/// on the line rather than a wash over the whole branch.
const GLOW_CREST_FRONT: f32 = 18.0;
/// Tail the spark leaves behind it. Longer than the front, so the light is
/// clearly moving down the branch.
const GLOW_CREST_TAIL: f32 = 46.0;
/// How far the spark reaches from the stroke center. Short of the next lane,
/// whose stroke starts 5.5 points away on the narrowest lane.
const GLOW_CREST_RADIUS: f32 = 4.0;
/// Mix toward white at the spark's center. The stroke is already the lane
/// color, so a same-color glow painted on top of it would not show.
const GLOW_CREST_WHITE: f32 = 0.45;
/// Alpha at the spark's center. The edges fade to clear.
const GLOW_CREST_ALPHA: u8 = 210;
const GLOW_FRAME: Duration = Duration::from_millis(16);
/// Seconds a lane and its tag take to light up or go out.
pub(super) const GLOW_FADE: f32 = 0.3;
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
    const ALL: [Self; 5] = [
        Self::Branch,
        Self::Graph,
        Self::Hash,
        Self::Message,
        Self::Author,
    ];

    fn title(self) -> &'static str {
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
struct ColumnLayout {
    left: [f32; 5],
    width: [f32; 5],
    /// The columns' sum, or the panel when that is wider, so a row's
    /// highlight still reaches the right edge.
    content_width: f32,
    lane_width: f32,
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
fn layout_columns(
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

/// What [`show`] asks the repository screen to keep after this frame.
pub struct HistoryOutput {
    /// New column widths when a divider was released or double-clicked.
    pub columns: Option<HistoryColumns>,
    /// A different commit was chosen. A closed detail sidebar should open.
    /// Clicking the commit that is already chosen does not set this.
    pub reveal_detail: bool,
}

/// Draws the history.
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
) -> HistoryOutput {
    // While this view's log is still running, an empty table is not "no commits".
    let empty = if loading {
        "Opening history…"
    } else {
        empty_history(&repo.branches, repo.detached_oid.is_some())
    };
    repo.hover.poll(ui.ctx(), &repo.git, &repo.info.root);
    let OpenedRepo {
        graph,
        hover,
        github,
        view_id,
        selected_commit,
        lit_branch: lit_name,
        ..
    } = repo;
    let rows = &graph.rows;
    let count = if graph.truncated {
        format!("{}+", rows.len())
    } else {
        rows.len().to_string()
    };
    ui.add(Label::new(RichText::new(format!("Commit history ({count})")).strong()).truncate());
    ui.add_space(6.0);
    // A reload that has not landed yet keeps the commit the user chose.
    retain_selected(ui.ctx(), selected_commit, rows, loading);
    let lit = lit_branch(lit_name, rows);
    if rows.is_empty() {
        // The column between the sidebars can be narrower than these sentences.
        // Wrap inside it; truncating would drop the line the loading state shows.
        ui.add(Label::new(text::wrapped(ui, RichText::new(empty).weak())));
        return HistoryOutput {
            columns: None,
            reveal_detail: false,
        };
    }
    if graph.truncated {
        ui.label(RichText::new(format!("Showing the latest {} commits.", rows.len())).weak());
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
                live = reset_column(live, column);
                done = Some(live);
            } else if (response.dragged() || response.drag_stopped())
                && let Some(pointer) = response.interact_pointer_pos()
            {
                let width = pointer.x - (origin + layout.left[index]);
                live = drag_column(live, column, width, mins[index], panel);
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
    let time = ui.input(|input| input.time);
    // One fade per lane color, asked every frame from the first: a lane lit
    // from dark fades in, and the lane it replaces fades out at the same time.
    let glows: Vec<(usize, f32)> = (0..cthulhu_git::git::LANE_COLORS)
        .filter_map(|color| {
            let on = lit.as_ref().is_some_and(|lit| lit.color == color);
            let id = view_id.with(("history-glow-fade", color));
            let fade = ui.ctx().animate_bool_with_time(id, on, GLOW_FADE);
            (fade > 0.0).then_some((color, fade))
        })
        .collect();

    let (click, glowed) = ui
        .scope(|ui| {
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
                lit,
                glows,
                time,
                selected: selected_commit.as_deref(),
                click: None,
                glowed: false,
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
                    let start = range.start;
                    for (offset, row) in rows[range].iter().enumerate() {
                        history_row(ui, row, &layout, row_height, start + offset, &mut cells);
                    }
                });
            ui.ctx()
                .data_mut(|data| data.insert_temp(origin_id, origin_x));
            header(ui, header_rect, origin_x, &layout, *view_id, palette);
            (cells.click, cells.glowed)
        })
        .inner;
    // The crest moves only while it is on screen and someone can watch it:
    // a lit lane scrolled away, or a window in the background, costs no frames.
    if glowed && ui.input(|input| input.focused) {
        ui.ctx().request_repaint_after(GLOW_FRAME);
    }
    let reveal_detail = click.is_some_and(|click| {
        // The sidebar and the lanes are already drawn. One more frame shows the click.
        ui.ctx().request_repaint();
        apply_click(click, selected_commit, lit_name)
    });

    let columns = if let Some(columns) = done {
        ui.ctx()
            .data_mut(|data| data.remove::<HistoryColumns>(live_id));
        Some(columns)
    } else if dragging {
        ui.ctx().data_mut(|data| data.insert_temp(live_id, live));
        ui.ctx().request_repaint();
        None
    } else {
        None
    };
    HistoryOutput {
        columns,
        reveal_detail,
    }
}

fn divider_id(view_id: Id, index: usize) -> Id {
    view_id.with(("history-divider", index))
}

/// `columns` with `column` dragged to `width` points. A column pushed
/// against its minimum stores that minimum's share, so the saved width
/// matches the one on screen; a minimum exactly as wide as the panel stores
/// the whole panel. When the panel is narrower than the minimum, that share
/// would be more than the whole panel; the smallest share is stored instead,
/// so a wider window later does not hand the column most of the panel. The
/// minimum holds it either way.
fn drag_column(
    mut columns: HistoryColumns,
    column: Column,
    width: f32,
    min: f32,
    panel: f32,
) -> HistoryColumns {
    let wanted = width.max(min);
    let share = if wanted <= min + 0.5 && min > panel {
        *HistoryColumns::RANGE.start()
    } else {
        per_mille_for(wanted, panel)
    };
    column.set(&mut columns, share);
    columns
}

/// `columns` with `column` back at its default share.
fn reset_column(mut columns: HistoryColumns, column: Column) -> HistoryColumns {
    column.set(&mut columns, column.per_mille(&HistoryColumns::default()));
    columns
}

fn scaled(ui: &Ui, style: TextStyle) -> FontId {
    let mut font = style.resolve(ui.style());
    font.size *= TEXT_SCALE;
    font
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
                        Label::new(RichText::new(column.title()).weak())
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
            WidgetInfo::labeled(Role::Unknown, true, format!("Resize {}", column.title()))
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
    lit: Option<LitBranch>,
    /// Lane colors with light on them, and how far each has lit up.
    glows: Vec<(usize, f32)>,
    time: f64,
    /// Commit whose row stays filled. `None` is the latest commit in the sidebar.
    selected: Option<&'a str>,
    /// This frame's click, applied once every row is drawn, so the whole
    /// table paints the state the frame started with.
    click: Option<RowClick>,
    /// A painted row carried the lit lane's light this frame.
    glowed: bool,
}

enum RowClick {
    /// A hash or message cell: that commit goes to the sidebar.
    Commit(String),
    /// The author cell: the sidebar goes back to the latest commit.
    Author,
    /// A branch tag: its lane lights or goes out, and the sidebar goes back
    /// to the latest commit.
    Branch(String),
}

/// Branch whose lane is lit. The name is what the tag toggles; the color is
/// the lane strokes the crest travels along.
struct LitBranch {
    name: String,
    color: usize,
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
    row_index: usize,
    cx: &mut RowContext<'_>,
) {
    let (_, rect) = ui.allocate_space(Vec2::new(layout.content_width, row_height));
    if !ui.is_rect_visible(rect) {
        return;
    }
    let row_id = ui.scope_id().with(("history-row", row.oid.as_str()));
    let palette = cx.palette;
    // The pointer, not a hover widget: the hash and message click sensors sit
    // on top of the row and must not take this highlight away.
    let selected = cx.selected == Some(row.oid.as_str());
    let hovered = ui.rect_contains_pointer(rect);
    let fill = if selected {
        palette.selection
    } else if hovered {
        palette.surface_hover
    } else {
        palette.background
    };
    if selected || hovered {
        ui.painter().rect_filled(rect, 0.0, fill);
    }

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
        let glows: Vec<LaneGlow> = (cx.glows.iter())
            .map(|&(color, fade)| LaneGlow {
                time: cx.time,
                color,
                // `origin + screen_y` is the content Y, so the crest stays on
                // the same commits when the table scrolls.
                origin: row_index as f32 * row_height - rect.top(),
                fade,
            })
            .collect();
        cx.glowed |= paint_lanes(&lanes, graph, row, palette, layout.lane_width, fill, &glows);
    }
    if let Some(chip) = &chip {
        let lit = cx.lit.as_ref().is_some_and(|lit| lit.name == chip.name);
        let lit = (ui.ctx()).animate_bool_with_time(row_id.with("tag-lit"), lit, GLOW_FADE);
        if paint_branch_chip(ui, chip, palette, lit) {
            cx.click = Some(RowClick::Branch(chip.name.clone()));
        }
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
    // column would follow the pointer across the whole commit. A click here
    // leaves the chosen commit and returns the sidebar to the latest one.
    let author = ui.interact(
        layout.cell(rect, Column::Author),
        row_id.with("card"),
        Sense::CLICK,
    );
    if author.clicked() {
        cx.click = Some(RowClick::Author);
    }
    hover::show_card(
        &author,
        row,
        cx.hover,
        cx.avatars,
        &CardContext {
            palette: cx.palette,
            github: cx.github,
            repo: cx.repo,
        },
    );

    // The accessible name stays on the painted glyphs. These sensors cover
    // the whole column, padding included, and nothing outside it. A later
    // click on the same commit changes nothing else. Only the hash column
    // copies, and only on a double click.
    let hash = click_commit(
        ui,
        layout.cell(rect, Column::Hash),
        row_id.with("pick-hash"),
        &row.oid,
        true,
    );
    let message = click_commit(
        ui,
        layout.cell(rect, Column::Message),
        row_id.with("pick-message"),
        &row.oid,
        false,
    );
    if hash || message {
        cx.click = Some(RowClick::Commit(row.oid.clone()));
    }
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
    // Hashes and branch names are one token, so the ellipsis may land
    // inside a word.
    job.wrap = TextWrapping::truncate_at_width(max_width.max(0.0));
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
        .widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
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
        .widget_info(|| WidgetInfo::labeled(Role::Label, true, names.as_str()));
}

fn lane_center(graph: Rect, lane: usize, lane_width: f32) -> f32 {
    graph.left() + CELL_PAD + lane as f32 * lane_width + lane_width / 2.0
}

/// The lit lane. `origin + screen_y` is how far a point sits down the branch.
#[derive(Clone, Copy)]
struct LaneGlow {
    time: f64,
    color: usize,
    origin: f32,
    /// How far the lane has lit up, from 0 (out) to 1. Every alpha scales by it.
    fade: f32,
}

/// The commit dot's center, the radius of the background ring it sits in,
/// and its own radius. At the narrowest lanes the dots shrink so neighbors
/// do not touch.
fn commit_dot(graph: Rect, row: &GraphRow, lane_width: f32) -> (Pos2, f32, f32) {
    let center = Pos2::new(
        lane_center(graph, row.commit_lane, lane_width),
        graph.center().y,
    );
    let outer = DOT_OUTER.min(lane_width / 2.0 - 1.0);
    (center, outer, DOT_INNER.min(outer - 1.5).max(1.0))
}

/// Returns whether the row carried any of the lit lane's light.
fn paint_lanes(
    painter: &egui::Painter,
    graph: Rect,
    row: &GraphRow,
    palette: &Palette,
    lane_width: f32,
    fill: Color32,
    glows: &[LaneGlow],
) -> bool {
    let (dot, outer, inner) = commit_dot(graph, row, lane_width);
    let (mut halo, mut crest) = (Mesh::default(), Mesh::default());
    for &glow in glows {
        let color = palette.graph_color(glow.color);
        let (lane_halo, lane_crest) = lane_glow(graph, row, lane_width, glow, color);
        halo.append(lane_halo);
        crest.append(lane_crest);
    }
    let glowed = !halo.is_empty() || !crest.is_empty();
    if !halo.is_empty() {
        painter.add(halo);
    }
    for trace in &row.traces {
        let (start, end) = trace_ends(graph, trace.from_lane, trace.to_lane, dot, lane_width);
        if start.distance(end) < 0.5 {
            continue;
        }
        painter.line_segment(
            [start, end],
            Stroke::new(LANE_STROKE, palette.graph_color(trace.color)),
        );
    }
    painter.circle_filled(dot, outer, fill);
    painter.circle_filled(dot, inner, palette.graph_color(row.commit_color));
    // After the stroke and the dot, so the spark sits on the line the eye
    // follows. The halo underneath stays the steady glow.
    if !crest.is_empty() {
        painter.add(crest);
    }
    glowed
}

/// The light on the lit lane in one row, in `color`: the halo, painted under
/// the strokes and the dot, and the spark, painted over them.
fn lane_glow(
    graph: Rect,
    row: &GraphRow,
    lane_width: f32,
    glow: LaneGlow,
    color: Color32,
) -> (Mesh, Mesh) {
    let (dot, outer, inner) = commit_dot(graph, row, lane_width);
    let (mut halo, mut crest) = (Mesh::default(), Mesh::default());
    for trace in row.traces.iter().filter(|trace| trace.color == glow.color) {
        let (start, end) = trace_ends(graph, trace.from_lane, trace.to_lane, dot, lane_width);
        let on_edge = [trace.from_lane.is_some(), trace.to_lane.is_some()];
        add_glow_segment(&mut halo, [start, end], on_edge, color, glow);
        add_crest_segment(&mut crest, [start, end], on_edge, color, glow);
    }
    if row.commit_color == glow.color {
        // The commit circle is opaque, so the aura is the band outside it.
        // `gap` is measured from the dot's edge, and it stops short of the
        // next lane's stroke.
        let gap = lane_width / 2.0 - outer - 0.5;
        if gap >= 1.0 {
            add_glow_disc(
                &mut halo,
                dot,
                outer,
                outer + GLOW_RADIUS.min(gap),
                color,
                glow_alpha(glow.time, glow.origin + dot.y),
                glow.fade,
            );
        }
        // The dot's light stays inside the colored circle, so it does not
        // reach the next lane.
        let lobe = crest_lobe(glow.time, glow.origin + dot.y);
        if lobe > 0.02 {
            add_crest_disc(&mut crest, dot, inner, color, lobe, glow.fade);
        }
    }
    (halo, crest)
}

fn trace_ends(
    graph: Rect,
    from_lane: Option<usize>,
    to_lane: Option<usize>,
    dot: Pos2,
    lane_width: f32,
) -> (Pos2, Pos2) {
    let start = match from_lane {
        Some(lane) => Pos2::new(lane_center(graph, lane, lane_width), graph.top()),
        None => dot,
    };
    let end = match to_lane {
        Some(lane) => Pos2::new(lane_center(graph, lane, lane_width), graph.bottom()),
        None => dot,
    };
    (start, end)
}

/// How far the bright point has traveled down the branch. The cycle stays in
/// f64 so a long session does not hitch the light.
fn glow_travel(time: f64) -> f32 {
    let cycles = (time / GLOW_PERIOD).rem_euclid(1.0);
    (cycles as f32) * GLOW_WAVELENGTH
}

/// Alpha of the halo at `distance` points down the branch. `time` is seconds.
/// The bright band is a wide gaussian, so the tube stays lit between passes.
fn glow_alpha(time: f64, distance: f32) -> u8 {
    let along = (distance - glow_travel(time)).rem_euclid(GLOW_WAVELENGTH);
    let delta = along.min(GLOW_WAVELENGTH - along);
    let lobe = (-0.5 * (delta / GLOW_SIGMA).powi(2)).exp();
    let alpha = f32::from(GLOW_ALPHA_BASE) + lobe * f32::from(GLOW_ALPHA_PEAK - GLOW_ALPHA_BASE);
    alpha
        .round()
        .clamp(f32::from(GLOW_ALPHA_BASE), f32::from(GLOW_ALPHA_PEAK)) as u8
}

/// Strength of the spark at `distance`. One in front of the head, and a longer
/// tail behind it, where the light has already passed.
fn crest_lobe(time: f64, distance: f32) -> f32 {
    let travel = glow_travel(time);
    let ahead = (distance - travel).rem_euclid(GLOW_WAVELENGTH);
    let behind = (travel - distance).rem_euclid(GLOW_WAVELENGTH);
    let (delta, sigma) = if ahead <= behind {
        (ahead, GLOW_CREST_FRONT)
    } else {
        (behind, GLOW_CREST_TAIL)
    };
    (-0.5 * (delta / sigma).powi(2)).exp()
}

fn glow_color(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

fn glow_share(alpha: u8, share: f32) -> u8 {
    (f32::from(alpha) * share).round() as u8
}

/// A band along `start`→`end`, sampled every [`GLOW_SAMPLE`] or closer. Each
/// sample reads its `light` once; `shade` colors each stop across the band
/// from that light and the stop's share. A segment with no length, or with
/// no sample that `shows`, adds nothing.
fn ribbon<T: Copy>(
    mesh: &mut Mesh,
    [start, end]: [Pos2; 2],
    on_edge: [bool; 2],
    stops: &[(f32, f32)],
    light: impl Fn(Pos2) -> T,
    shows: impl Fn(T) -> bool,
    shade: impl Fn(T, f32) -> Color32,
) {
    let delta = end - start;
    let length = delta.length();
    if length < 0.5 {
        return;
    }
    let steps = (length / GLOW_SAMPLE).ceil().max(1.0) as usize;
    let tangent = delta / length;
    let normal = Vec2::new(-tangent.y, tangent.x);
    let samples: Vec<(Pos2, T)> = (0..=steps)
        .map(|step| {
            let point = start + delta * (step as f32 / steps as f32);
            (point, light(point))
        })
        .collect();
    let crosses = samples.iter().any(|&(_, light)| shows(light));
    if !crosses {
        return;
    }
    let first = mesh.vertices.len() as u32;
    for (step, (point, light)) in samples.into_iter().enumerate() {
        // An end on the row's top or bottom edge is cut along that edge, where
        // the next row's ribbon starts square to its own lane. A cut square to
        // a lane change would leave a notch without light at the joint. Such
        // a segment always runs down the row, so `normal.x` is never zero.
        let along_edge = (step == 0 && on_edge[0]) || (step == steps && on_edge[1]);
        for &(offset, share) in stops {
            let across = if along_edge {
                Vec2::new(offset / normal.x, 0.0)
            } else {
                normal * offset
            };
            mesh.colored_vertex(point + across, shade(light, share));
        }
    }
    stitch(mesh, first, steps as u32, stops.len() as u32, false);
}

/// Rings of `steps` vertices around `center`, each `base * scale` from it in
/// one color, and the bands between neighboring rings.
fn rings(mesh: &mut Mesh, center: Pos2, base: f32, steps: usize, rings: &[(f32, Color32)]) {
    let first = mesh.vertices.len() as u32;
    for &(scale, color) in rings {
        for step in 0..steps {
            let angle = step as f32 / steps as f32 * std::f32::consts::TAU;
            let (sin, cos) = angle.sin_cos();
            mesh.colored_vertex(center + Vec2::new(cos, sin) * base * scale, color);
        }
    }
    let bands = rings.len().saturating_sub(1) as u32;
    stitch(mesh, first, bands, steps as u32, true);
}

/// Two triangles for each pair of neighbors in `bands` + 1 rows of `width`
/// vertices from `first`, joining each row to the next. A `closed` row also
/// joins its last vertex to its first.
fn stitch(mesh: &mut Mesh, first: u32, bands: u32, width: u32, closed: bool) {
    let pairs = if closed {
        width
    } else {
        width.saturating_sub(1)
    };
    for band in 0..bands {
        let row = first + band * width;
        let next = row + width;
        for index in 0..pairs {
            let right = (index + 1) % width;
            mesh.add_triangle(row + index, next + index, row + right);
            mesh.add_triangle(row + right, next + index, next + right);
        }
    }
}

/// The halo along `start`→`end`. Full alpha out to the stroke edge, since the
/// stroke is painted on top of the center, then a falloff that clears before
/// the next lane.
fn add_glow_segment(
    mesh: &mut Mesh,
    ends: [Pos2; 2],
    on_edge: [bool; 2],
    color: Color32,
    glow: LaneGlow,
) {
    // Offset from the stroke center, and the share of this sample's alpha.
    const STOPS: [(f32, f32); 6] = [
        (-GLOW_RADIUS, 0.0),
        (-GLOW_SHOULDER, GLOW_EDGE_SHARE),
        (-LANE_STROKE / 2.0, 1.0),
        (LANE_STROKE / 2.0, 1.0),
        (GLOW_SHOULDER, GLOW_EDGE_SHARE),
        (GLOW_RADIUS, 0.0),
    ];
    ribbon(
        mesh,
        ends,
        on_edge,
        &STOPS,
        |point| glow_alpha(glow.time, glow.origin + point.y),
        |_| true,
        |alpha, share| glow_color(color, glow_share(alpha, share * glow.fade)),
    );
}

/// Ring outside the commit dot. `inner` is the opaque circle; `outer` is where
/// the aura fades out. `peak` is the alpha at the circle's edge when fully lit.
fn add_glow_disc(
    mesh: &mut Mesh,
    center: Pos2,
    inner: f32,
    outer: f32,
    color: Color32,
    peak: u8,
    fade: f32,
) {
    if outer < inner + 0.5 {
        return;
    }
    let tint = |share| glow_color(color, glow_share(peak, share * fade));
    let aura = [
        (inner, tint(1.0)),
        ((inner + outer) * 0.5, tint(GLOW_EDGE_SHARE)),
        (outer, tint(0.0)),
    ];
    // On a base of 1, each scale is exactly the radius it names.
    rings(mesh, center, 1.0, 16, &aura);
}

fn crest_color(lane: Color32, lobe: f32, share: f32) -> Color32 {
    let mix = (lobe * GLOW_CREST_WHITE).clamp(0.0, 1.0);
    let channel = |value: u8| -> u8 {
        let colored = f32::from(value);
        (colored + (255.0 - colored) * mix).round() as u8
    };
    let alpha = (lobe * f32::from(GLOW_CREST_ALPHA) * share)
        .round()
        .clamp(0.0, 255.0) as u8;
    Color32::from_rgba_unmultiplied(
        channel(lane.r()),
        channel(lane.g()),
        channel(lane.b()),
        alpha,
    )
}

/// A short light along `start`→`end`. A segment the spark does not reach
/// adds nothing.
fn add_crest_segment(
    mesh: &mut Mesh,
    ends: [Pos2; 2],
    on_edge: [bool; 2],
    color: Color32,
    glow: LaneGlow,
) {
    // Offset from the stroke center, and the share of this sample's alpha.
    const STOPS: [(f32, f32); 5] = [
        (-GLOW_CREST_RADIUS, 0.0),
        (-LANE_STROKE / 2.0, 0.7),
        (0.0, 1.0),
        (LANE_STROKE / 2.0, 0.7),
        (GLOW_CREST_RADIUS, 0.0),
    ];
    ribbon(
        mesh,
        ends,
        on_edge,
        &STOPS,
        |point| crest_lobe(glow.time, glow.origin + point.y),
        |lobe| lobe > 0.02,
        |lobe, share| crest_color(color, lobe, share * glow.fade),
    );
}

/// Soft spot on the commit dot, fading at `radius`.
fn add_crest_disc(
    mesh: &mut Mesh,
    center: Pos2,
    radius: f32,
    color: Color32,
    lobe: f32,
    fade: f32,
) {
    if radius < 0.5 {
        return;
    }
    let tint = |share: f32| crest_color(color, lobe, share * fade);
    let spot = [(0.0, tint(1.0)), (0.55, tint(0.45)), (1.0, tint(0.0))];
    rings(mesh, center, radius, 12, &spot);
}

/// Returns whether a different commit was chosen. A second click on the
/// chosen commit changes nothing.
fn apply_click(click: RowClick, selected: &mut Option<String>, lit: &mut Option<String>) -> bool {
    match click {
        RowClick::Commit(oid) => {
            let chosen = selected.as_ref() != Some(&oid);
            *selected = Some(oid);
            chosen
        }
        RowClick::Author => {
            *selected = None;
            false
        }
        RowClick::Branch(name) => {
            toggle_lit(lit, &name);
            *selected = None;
            false
        }
    }
}

/// Forgets a chosen commit that is no longer in `rows`. An empty table that
/// is still loading keeps it, so a reload does not snap back to the latest.
/// The sidebar is already drawn, so forgetting takes one more frame.
fn retain_selected(
    ctx: &egui::Context,
    selected: &mut Option<String>,
    rows: &[GraphRow],
    loading: bool,
) {
    let kept =
        |oid: &mut String| rows.iter().any(|row| row.oid == *oid) || (loading && rows.is_empty());
    if selected.take_if(|oid| !kept(oid)).is_some() {
        ctx.request_repaint();
    }
}

/// Click target for one hash or message cell. Returns whether it was clicked.
///
/// `copy_on_double_click` copies the full hash on the second click of a double
/// click. That second click is also a normal click, and it finds the commit
/// already chosen.
fn click_commit(ui: &mut Ui, cell: Rect, id: Id, oid: &str, copy_on_double_click: bool) -> bool {
    if cell.width() <= 0.0 {
        return false;
    }
    let response = ui
        .interact(cell, id, Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand);
    let response = if copy_on_double_click {
        response.on_hover_text("Double-click to copy the full hash")
    } else {
        response
    };
    if copy_on_double_click && response.double_clicked() {
        widgets::copy_commit_hash(ui.ctx(), oid);
    }
    response.clicked()
}

/// Lights `name`, or puts it out when it is the lit branch.
fn toggle_lit(lit: &mut Option<String>, name: &str) {
    *lit = if lit.as_deref() == Some(name) {
        None
    } else {
        Some(name.to_owned())
    };
}

/// The lit branch and its lane color. A branch no row carries is forgotten.
fn lit_branch(name: &mut Option<String>, rows: &[GraphRow]) -> Option<LitBranch> {
    let found = name.as_ref().and_then(|name| {
        rows.iter()
            .flat_map(|row| &row.branches)
            .find(|branch| branch.name == *name)
    });
    let Some(branch) = found else {
        *name = None;
        return None;
    };
    Some(LitBranch {
        name: branch.name.clone(),
        color: branch.color,
    })
}

/// Checked-out branch first, then the sidebar order the row already has.
fn ordered_branches(branches: &[GraphBranch]) -> Vec<&GraphBranch> {
    let mut order: Vec<_> = branches.iter().enumerate().collect();
    order.sort_by_key(|(index, branch)| (!branch.current, *index));
    order.into_iter().map(|(_, branch)| branch).collect()
}

struct BranchChip {
    rect: Rect,
    color: usize,
    current: bool,
    /// The name drawn on the tag. A click lights this branch, not the `+N` tips.
    name: String,
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
        name: primary.name.clone(),
        names,
        oid: row.oid.clone(),
        text,
        plus,
    })
}

/// `true` when the tag was clicked.
/// `lit` is how far the tag's lane has lit up, from 0 to 1.
fn paint_branch_chip(ui: &mut Ui, chip: &BranchChip, palette: &Palette, lit: f32) -> bool {
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
        painter.rect_filled(
            chip.rect,
            4.0,
            chip_fill(palette, chip.color, chip.current, lit),
        );
        icons::paint_icon(&painter, icon_rect, Icon::GitBranch, color);
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
    let response = ui
        .interact(
            chip.rect,
            ui.scope_id().with(("branch-label", chip.oid.as_str())),
            Sense::click(),
        )
        .on_hover_cursor(CursorIcon::PointingHand);
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, chip.names.as_str()));
    response.clicked()
}

fn chip_fill(palette: &Palette, color: usize, current: bool, lit: f32) -> Color32 {
    let fill = palette.graph_chip(color, current);
    let lane = palette.graph_color(color);
    let boost = (f32::from(CHIP_LIT_BOOST) * lit).round() as u8;
    Color32::from_rgba_unmultiplied(lane.r(), lane.g(), lane.b(), fill.a().saturating_add(boost))
}

#[cfg(test)]
mod tests {
    use cthulhu_git::git::GraphTrace;
    use proptest::prelude::*;

    use super::*;

    /// Width of the hash sample the rows below are computed with.
    const HASH_TEXT: f32 = 100.0;

    fn columns([branch, graph, hash, message, author]: [u16; 5]) -> HistoryColumns {
        HistoryColumns {
            branch,
            graph,
            hash,
            message,
            author,
        }
    }

    proptest! {
        #![proptest_config(crate::ui::test_support::proptest_config())]

        /// Each column is its share of the panel, or its minimum when that is
        /// larger, laid end to end. The lanes fit in the graph column, and
        /// changing one share never moves another column.
        #[test]
        fn each_column_is_its_share_or_its_minimum(
            panel in -10.0_f32..4000.0,
            shares in proptest::array::uniform5(HistoryColumns::RANGE),
            lanes in 0_usize..64,
            hash in 0.0_f32..400.0,
            changed in 0_usize..5,
            value in HistoryColumns::RANGE,
        ) {
            let mins = column_mins(hash, lanes);
            let layout = layout_columns(panel, &columns(shares), mins, lanes);
            let mut x = 0.0;
            for column in Column::ALL {
                let index = column.index();
                let share = panel.max(0.0) * f32::from(shares[index]) / 1000.0;
                let (width, min) = (layout.width[index], mins[index]);
                let case = format!("{column:?} is {width}, share {share}, minimum {min}");
                prop_assert_eq!(layout.left[index], x, "{} starts off its neighbor", case);
                prop_assert!(width >= min && width >= share, "{case}");
                prop_assert!(width == share || width == min, "{case}");
                x += width;
            }
            prop_assert_eq!(layout.content_width, x.max(panel.max(0.0)));
            let room = layout.width[Column::Graph.index()] - 2.0 * CELL_PAD;
            let lane = layout.lane_width;
            prop_assert!((LANE_MIN..=LANE_MAX).contains(&lane), "lane width {lane}");
            let lanes_width = lanes.max(1) as f32 * lane;
            prop_assert!(lanes_width <= room + 0.01, "{lanes} lanes of {lane} in {room}");

            let mut moved = columns(shares);
            Column::ALL[changed].set(&mut moved, value);
            let after = layout_columns(panel, &moved, mins, lanes);
            for column in Column::ALL.into_iter().filter(|column| column.index() != changed) {
                let index = column.index();
                prop_assert_eq!(after.width[index], layout.width[index], "{:?} moved", column);
            }
        }
    }

    /// (panel, shares, lanes, the columns' sum, lane width). The defaults fill
    /// a wide panel exactly. Shares of 20 leave most of a wide panel free. A
    /// narrow panel keeps every minimum and scrolls. Forty lanes shrink to the
    /// narrowest lane, one grows to the widest. No panel at all still lays out
    /// every minimum.
    #[test]
    fn layouts_at_the_edges() {
        let defaults = HistoryColumns::default();
        for (panel, shares, lanes, sum, lane_width) in [
            (2000.0, defaults, 1, 2000.0, LANE_MAX),
            (4000.0, columns([20; 5]), 1, 816.0, LANE_MAX),
            (196.0, defaults, 2, 780.0, LANE_MIN),
            (600.0, defaults, 40, 1312.0, LANE_MIN),
            (0.0, defaults, 0, 766.0, LANE_MIN),
            (-5.0, defaults, 0, 766.0, LANE_MIN),
        ] {
            let case = format!("panel {panel}, {lanes} lanes, {shares:?}");
            let layout = layout_columns(panel, &shares, column_mins(HASH_TEXT, lanes), lanes);
            let total: f32 = layout.width.iter().sum();
            assert!(
                (total - sum).abs() < 0.01,
                "{case}: columns add up to {total}"
            );
            assert!(
                (layout.content_width - total.max(panel)).abs() < 0.01,
                "{case}: content is {}",
                layout.content_width
            );
            assert_eq!(layout.lane_width, lane_width, "{case}");
        }
    }

    /// (dragged width, minimum, panel, stored share). Only the dragged
    /// column's share changes.
    #[test]
    fn a_dragged_width_is_stored_as_a_share_of_the_panel() {
        let start = columns([100, 200, 300, 400, 500]);
        for (width, min, panel, stored) in [
            (500.0, 0.0, 1000.0, 500),
            (1.0, 0.0, 1000.0, 20),
            (5000.0, 0.0, 1000.0, 1000),
            (500.0, 0.0, 0.0, 20),
            (f32::NAN, 0.0, 1000.0, 20),
            (600.0, MESSAGE_MIN, 1000.0, 600),
            // On a panel that fits the minimum, the saved share is the one on
            // screen, also when the pointer went past the minimum.
            (MESSAGE_MIN, MESSAGE_MIN, 1660.0, 169),
            (100.0, MESSAGE_MIN, 1660.0, 169),
            // A 260-point panel cannot hold the 280-point message minimum.
            // Saving 280/260 as 1000 would make the column fill a wider
            // window later.
            (MESSAGE_MIN, MESSAGE_MIN, 260.0, 20),
            // A panel exactly as wide as the minimum still fits it.
            (MESSAGE_MIN, MESSAGE_MIN, MESSAGE_MIN, 1000),
        ] {
            assert_eq!(
                drag_column(start, Column::Message, width, min, panel),
                HistoryColumns {
                    message: stored,
                    ..start
                },
                "{width} of a {panel} panel, minimum {min}"
            );
        }
    }

    /// (branches: has a commit and is checked, detached HEAD, sentence).
    #[test]
    fn an_empty_history_says_why() {
        for (branches, detached, sentence) in [
            (&[][..], false, "No commits yet."),
            (&[(false, false)], false, "No commits yet."),
            (
                &[(true, false), (false, true)],
                false,
                "No branches selected.",
            ),
            (&[(true, false)], true, "No commits yet."),
            (&[(true, true)], false, "No commits yet."),
        ] {
            let listed: Vec<ListedBranch> = branches
                .iter()
                .map(|&(born, checked)| {
                    let mut listed = crate::ui::test_support::listed("b", checked);
                    listed.branch.oid = born.then(|| "a".repeat(40));
                    listed
                })
                .collect();
            assert_eq!(
                empty_history(&listed, detached),
                sentence,
                "{branches:?}, detached {detached}"
            );
        }
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

    #[test]
    fn glow_alpha_holds_a_floor_and_a_crest_that_travels_down_the_branch() {
        let mut time = 0.0;
        while time < GLOW_PERIOD * 2.0 {
            let mut distance = 0.0;
            while distance <= GLOW_WAVELENGTH {
                let alpha = glow_alpha(time, distance);
                assert!(
                    (GLOW_ALPHA_BASE..=GLOW_ALPHA_PEAK).contains(&alpha),
                    "alpha {alpha} at time {time}, distance {distance}"
                );
                distance += 20.0;
            }
            time += 0.1;
        }

        assert_eq!(glow_alpha(0.0, 0.0), GLOW_ALPHA_PEAK);
        assert_eq!(glow_alpha(GLOW_PERIOD, GLOW_WAVELENGTH), GLOW_ALPHA_PEAK);
        // Half a period moves the crest half a wavelength down the branch.
        assert_eq!(
            glow_alpha(GLOW_PERIOD / 2.0, GLOW_WAVELENGTH / 2.0),
            GLOW_ALPHA_PEAK
        );

        // Half a wavelength is the far side of the wave. The tail sits on the
        // floor, or one step above it when the gaussian rounds up.
        let dim = glow_alpha(0.0, GLOW_WAVELENGTH / 2.0);
        assert!(
            (GLOW_ALPHA_BASE..=GLOW_ALPHA_BASE + 1).contains(&dim),
            "half a wavelength should be the floor, got {dim}"
        );
        let shoulder = glow_alpha(0.0, GLOW_SIGMA);
        assert!(
            dim < shoulder && shoulder < GLOW_ALPHA_PEAK,
            "the crest should fall off between the peak and the floor"
        );
    }

    #[test]
    fn crest_lobe_is_a_spark_that_travels_down_the_branch() {
        assert!((crest_lobe(0.0, 0.0) - 1.0).abs() < 1.0e-5);
        assert!(
            crest_lobe(0.0, GLOW_WAVELENGTH / 2.0) < 0.01,
            "half a wavelength should be dark"
        );
        assert!(
            (crest_lobe(GLOW_PERIOD / 2.0, GLOW_WAVELENGTH / 2.0) - 1.0).abs() < 1.0e-5,
            "after half a period the spark is half a wavelength further down"
        );
        // The same distance behind the spark is brighter than ahead of it.
        let ahead = crest_lobe(0.0, 36.0);
        let behind = crest_lobe(0.0, -36.0);
        assert!(
            behind > ahead && ahead > 0.0,
            "the tail should outlast the front, ahead {ahead}, behind {behind}"
        );
    }

    type Trace = (Option<usize>, Option<usize>, usize);
    type GlowRow = (&'static str, f32, f64, f32, [usize; 2], &'static [Trace]);
    const STRAIGHT: &[Trace] = &[(Some(0), Some(0), 0), (Some(1), None, 2)];
    const LANE_CHANGE: &[Trace] = &[(Some(1), Some(0), 0)];
    /// Into the dot and out to both parents, past another color's lane, and
    /// a trace with no length.
    const MERGE: &[Trace] = &[
        (Some(0), None, 0),
        (None, Some(0), 0),
        (None, Some(1), 0),
        (Some(2), Some(2), 3),
        (None, None, 0),
    ];
    const DOT: &[Trace] = &[(Some(1), None, 0), (None, Some(1), 0)];
    /// Rows lit on lane color 0, whose dot sits 140 points down the branch:
    /// (case, lane width, seconds, fade, dot lane and color, traces as from
    /// lane, to lane and color). At 0.7 s the crest is on the dot, at 0.425 s
    /// its front reaches only the upper half, at 2.3 s it is half a
    /// wavelength away. On 20-point lanes the aura is narrower than the halo;
    /// on the narrowest there is no room for it. Half lit is the dot halfway
    /// through its fade.
    const GLOW_ROWS: [GlowRow; 8] = [
        ("straight", LANE_MAX, 0.7, 1.0, [1, 2], STRAIGHT),
        ("lane change", LANE_MAX, 0.7, 1.0, [2, 2], LANE_CHANGE),
        ("merge", 20.0, 0.7, 1.0, [0, 0], MERGE),
        ("dot", LANE_MAX, 0.7, 1.0, [1, 0], DOT),
        ("narrow", LANE_MIN, 0.7, 1.0, [1, 0], DOT),
        ("crest edge", LANE_MAX, 0.425, 1.0, [1, 0], DOT),
        ("crest away", LANE_MAX, 2.3, 1.0, [1, 0], DOT),
        ("half lit", LANE_MAX, 0.7, 0.5, [1, 0], DOT),
    ];
    /// Per row above: the halo's and the crest's vertex and index counts,
    /// then a digest of each mesh's exact bits. One row per line, as the
    /// test prints them.
    #[rustfmt::skip]
    const GOLDEN: [([[usize; 2]; 2], [u64; 2]); 8] = [
        ([[30, 120], [25, 96]], [0xd7e20f28027a9a35, 0x43f38e51d12d739a]), // straight
        ([[36, 150], [30, 120]], [0x96903933ab41dd26, 0x3de8a17a1344d847]), // lane change
        ([[108, 402], [86, 312]], [0xe4bc37ab4ec5b458, 0x3b0028a3706a8351]), // merge
        ([[84, 312], [66, 240]], [0x1aaa1a4a360d428d, 0xa97ffe770013a1c9]), // dot
        ([[36, 120], [66, 240]], [0x11281c45b3196845, 0xf7cad7d9de74f997]), // narrow
        ([[84, 312], [15, 48]], [0xb61e34c53fef13b9, 0x239961336da8b433]), // crest edge
        ([[84, 312], [0, 0]], [0x8a735e8ee3cdcab1, 0xcbf29ce484222325]), // crest away
        ([[84, 312], [66, 240]], [0x22a1d5b5c9f8f2ed, 0xc7b7a38f1a4a5066]), // half lit
    ];

    /// FNV-1a over each vertex's position, UV and color bits, then the indices.
    fn mesh_digest(mesh: &Mesh) -> u64 {
        let words = |vertex: &egui::epaint::Vertex| {
            let [x, y, u, v] = [vertex.pos.x, vertex.pos.y, vertex.uv.x, vertex.uv.y];
            let color = u32::from_le_bytes(vertex.color.to_array());
            [x, y, u, v].map(f32::to_bits).into_iter().chain([color])
        };
        (mesh.vertices.iter().flat_map(words))
            .chain(mesh.indices.iter().copied())
            .flat_map(u32::to_le_bytes)
            .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
            })
    }

    /// The graph cell of a row lit on lane color 0 at `time` and `fade`, whose
    /// dot sits 140 points down the branch, and the row with that dot and
    /// `traces`.
    fn lit_row(
        [lane, color]: [usize; 2],
        traces: &[Trace],
        time: f64,
        fade: f32,
    ) -> (Rect, GraphRow, LaneGlow) {
        let traces = traces
            .iter()
            .map(|&(from_lane, to_lane, color)| GraphTrace {
                from_lane,
                to_lane,
                color,
            });
        let row = GraphRow {
            oid: String::new(),
            summary: String::new(),
            author: Person::default(),
            co_authors: Vec::new(),
            branches: Vec::new(),
            commit_lane: lane,
            commit_color: color,
            traces: traces.collect(),
        };
        let glow = LaneGlow {
            time,
            color: 0,
            origin: 70.0,
            fade,
        };
        let graph = Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(150.0, 40.0));
        (graph, row, glow)
    }

    /// Every vertex, color and index of the lit lane's light at fixed times.
    /// The digests hold the exact bits of glibc's `expf` and `sinf`; another
    /// libm may round a last bit differently, so other systems check counts.
    #[test]
    fn lane_glow_meshes_match_the_golden() {
        let exact = cfg!(all(target_os = "linux", target_env = "gnu"));
        let (mut now, mut changed) = (String::new(), String::new());
        for ((case, lane_width, time, fade, dot, traces), golden) in
            GLOW_ROWS.into_iter().zip(GOLDEN)
        {
            let (graph, row, glow) = lit_row(dot, traces, time, fade);
            let color = Color32::from_rgb(64, 160, 224);
            let meshes = lane_glow(graph, &row, lane_width, glow, color);
            let meshes = [("halo", meshes.0), ("crest", meshes.1)];
            let counts = meshes
                .each_ref()
                .map(|(_, mesh)| [mesh.vertices.len(), mesh.indices.len()]);
            let digests = meshes.each_ref().map(|(_, mesh)| mesh_digest(mesh));
            let [halo, crest] = digests;
            now += &format!("({counts:?}, [{halo:#018x}, {crest:#018x}]), // {case}\n");
            if counts == golden.0 && (!exact || digests == golden.1) {
                continue;
            }
            changed += &format!("\n{case}: counts {:?}, now {counts:?}\n", golden.0);
            for (name, mesh) in &meshes {
                for (index, vertex) in mesh.vertices.iter().enumerate() {
                    let (x, y, color) = (vertex.pos.x, vertex.pos.y, vertex.color.to_array());
                    changed += &format!("{name} {index}: ({x:?}, {y:?}) {color:?}\n");
                }
                changed += &format!("{name} indices: {:?}\n", mesh.indices);
            }
        }
        assert!(
            changed.is_empty(),
            "the glow changed:{changed}\nGOLDEN is now:\n{now}"
        );
    }

    /// The halo goes under the strokes and the dots, the crest over them. A
    /// trace with no length paints no stroke.
    #[test]
    fn paint_lanes_puts_the_halo_under_the_strokes_and_the_crest_on_top() {
        let palette = &crate::ui::theme::DEFAULT_THEME.palette;
        let (graph, row, glow) = lit_row([0, 0], MERGE, 0.7, 1.0);
        let (halo, crest) = lane_glow(graph, &row, 20.0, glow, palette.graph_color(0));
        let mut output = egui::Context::default().run_ui(egui::RawInput::default(), |ui| {
            let fill = Color32::BLACK;
            paint_lanes(ui.painter(), graph, &row, palette, 20.0, fill, &[glow]);
        });
        output.textures_delta.clear();
        let painted: Vec<&str> = (output.shapes.iter())
            .map(|clipped| match &clipped.shape {
                egui::Shape::Mesh(mesh) if **mesh == halo => "halo",
                egui::Shape::Mesh(mesh) if **mesh == crest => "crest",
                egui::Shape::LineSegment { .. } => "stroke",
                egui::Shape::Circle(_) => "dot",
                _ => "other",
            })
            .collect();
        let order = [
            "halo", "stroke", "stroke", "stroke", "stroke", "dot", "dot", "crest",
        ];
        assert_eq!(painted, order);
    }
}
