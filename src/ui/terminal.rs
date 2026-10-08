//! Interactive shell for the open repository.
//!
//! The PTY is `alacritty_terminal`, not `Git::run`: the user's environment is
//! left alone, and the process is not killed after 60 seconds. It starts the
//! first time the strip is shown and dies with the [`OpenedRepo`](super::OpenedRepo).

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::JoinHandle;

use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg, State};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config as TermConfig, RenderableCursor, Term, TermMode, color};
use alacritty_terminal::tty::{self, Pty};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};
use eframe::egui::{
    self, Align2, Color32, CursorIcon, Event, FontId, Id, Key, Modifiers, Painter, PointerButton,
    Pos2, Rect, RichText, Sense, Stroke, TextStyle, Ui, Vec2,
};

use super::theme::Palette;

const DETAIL_MIN_HEIGHT: f32 = 120.0;
const TERMINAL_MIN_HEIGHT: f32 = 80.0;
const SPLITTER_HEIGHT: f32 = 6.0;
const INITIAL_FRACTION: f32 = 0.40;

/// Which OS the shell choice is for. Tests pass both; the app passes its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostOs {
    Unix,
    Windows,
}

/// Program to launch. `shell` is the Unix `SHELL` variable.
///
/// Windows always uses `powershell.exe`. An empty or non-Unicode `SHELL`
/// falls back to `/bin/sh` because the PTY shell path is a `String`.
pub fn shell_program(host: HostOs, shell: Option<&OsStr>) -> OsString {
    match host {
        HostOs::Windows => OsString::from("powershell.exe"),
        HostOs::Unix => match shell {
            Some(value) if !value.is_empty() && value.to_str().is_some() => value.to_os_string(),
            _ => OsString::from("/bin/sh"),
        },
    }
}

fn this_host() -> HostOs {
    if cfg!(windows) {
        HostOs::Windows
    } else {
        HostOs::Unix
    }
}

/// One shell for the open repository, or the message left after it exits.
pub enum Terminal {
    Live(ShellSession),
    Exited { message: String },
}

pub struct ShellSession {
    term: Arc<FairMutex<Term<Bridge>>>,
    io: EventLoopSender,
    join: Option<JoinHandle<(EventLoop<Pty, Bridge>, State)>>,
    pid: u32,
    size: WindowSize,
    notices: Receiver<Notice>,
    anchor: Option<Point>,
}

struct GridSize {
    columns: usize,
    screen_lines: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }

    fn screen_lines(&self) -> usize {
        self.screen_lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

struct Metrics {
    cols: usize,
    rows: usize,
    cell_w: f32,
    cell_h: f32,
    font: FontId,
}

#[derive(Clone)]
struct Bridge {
    ctx: egui::Context,
    tx: mpsc::Sender<Notice>,
}

enum Notice {
    Write(String),
    Copy(String),
    Exited,
    Color(usize, Arc<dyn Fn(Rgb) -> String + Sync + Send + 'static>),
    Size(Arc<dyn Fn(WindowSize) -> String + Sync + Send + 'static>),
}

impl EventListener for Bridge {
    fn send_event(&self, event: TermEvent) {
        let notice = match event {
            TermEvent::PtyWrite(text) => Some(Notice::Write(text)),
            TermEvent::ClipboardStore(_, text) => Some(Notice::Copy(text)),
            TermEvent::Exit | TermEvent::ChildExit(_) => Some(Notice::Exited),
            TermEvent::ColorRequest(index, format) => Some(Notice::Color(index, format)),
            TermEvent::TextAreaSizeRequest(format) => Some(Notice::Size(format)),
            TermEvent::Wakeup
            | TermEvent::Bell
            | TermEvent::MouseCursorDirty
            | TermEvent::CursorBlinkingChange
            | TermEvent::Title(_)
            | TermEvent::ResetTitle
            | TermEvent::ClipboardLoad(_, _) => None,
        };
        if let Some(notice) = notice {
            let _ = self.tx.send(notice);
        }
        self.ctx.request_repaint();
    }
}

impl Drop for ShellSession {
    fn drop(&mut self) {
        // The PTY child called `setsid`, so its pid is the process group.
        // Alacritty only SIGHUPs that leader; kill the group first so children
        // such as `claude` do not survive the repository closing.
        kill_shell(self.pid);
        let _ = self.io.send(Msg::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl ShellSession {
    fn spawn(cwd: &Path, ctx: &egui::Context, metrics: &Metrics) -> Result<Self, String> {
        let (tx, notices) = mpsc::channel();
        let bridge = Bridge {
            ctx: ctx.clone(),
            tx,
        };
        let grid = GridSize {
            columns: metrics.cols,
            screen_lines: metrics.rows,
        };
        let term = Arc::new(FairMutex::new(Term::new(
            TermConfig::default(),
            &grid,
            bridge.clone(),
        )));
        let program = shell_program(this_host(), std::env::var_os("SHELL").as_deref());
        let program = program
            .to_str()
            .ok_or_else(|| "The shell path is not Unicode.".to_owned())?
            .to_owned();
        let options = tty::Options {
            shell: Some(tty::Shell::new(program, Vec::new())),
            working_directory: Some(cwd.to_path_buf()),
            ..tty::Options::default()
        };
        let size = window_size(metrics, ctx.pixels_per_point());
        let pty = tty::new(&options, size, 0).map_err(|error| spawn_error(&error))?;
        let pid = pty_pid(&pty);
        let event_loop = EventLoop::new(Arc::clone(&term), bridge, pty, false, false)
            .map_err(|error| spawn_error(&error))?;
        let io = event_loop.channel();
        let join = event_loop.spawn();
        Ok(Self {
            term,
            io,
            join: Some(join),
            pid,
            size,
            notices,
            anchor: None,
        })
    }

    fn poll(&mut self, ctx: &egui::Context) -> bool {
        let mut exited = false;
        loop {
            match self.notices.try_recv() {
                Ok(Notice::Write(text)) => self.write(text.as_bytes()),
                Ok(Notice::Copy(text)) => ctx.copy_text(text),
                Ok(Notice::Exited) => exited = true,
                Ok(Notice::Color(index, format)) => {
                    let rgb = self.color_rgb(index);
                    self.write(format(rgb).as_bytes());
                }
                Ok(Notice::Size(format)) => self.write(format(self.size).as_bytes()),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    exited = true;
                    break;
                }
            }
        }
        exited
    }

    fn color_rgb(&self, index: usize) -> Rgb {
        let term = self.term.lock();
        let colors = term.renderable_content().colors;
        if index < color::COUNT
            && let Some(rgb) = colors[index]
        {
            return rgb;
        }
        Rgb { r: 0, g: 0, b: 0 }
    }

    fn write(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let _ = self.io.send(Msg::Input(Cow::Owned(bytes.to_vec())));
    }

    fn resize(&mut self, metrics: &Metrics, pixels_per_point: f32) {
        let size = window_size(metrics, pixels_per_point);
        if size.num_cols == self.size.num_cols && size.num_lines == self.size.num_lines {
            return;
        }
        self.size = size;
        self.term.lock().resize(GridSize {
            columns: metrics.cols,
            screen_lines: metrics.rows,
        });
        // `Msg::Resize` updates the PTY. The event loop does not resize `Term`.
        let _ = self.io.send(Msg::Resize(size));
    }
}

fn spawn_error(error: &io::Error) -> String {
    format!("Could not start the shell: {error}")
}

fn window_size(metrics: &Metrics, pixels_per_point: f32) -> WindowSize {
    WindowSize {
        num_lines: u16::try_from(metrics.rows).unwrap_or(u16::MAX).max(1),
        num_cols: u16::try_from(metrics.cols).unwrap_or(u16::MAX).max(1),
        cell_width: px(metrics.cell_w * pixels_per_point),
        cell_height: px(metrics.cell_h * pixels_per_point),
    }
}

fn px(points: f32) -> u16 {
    let pixels = points.round();
    if pixels <= 1.0 {
        1
    } else if pixels >= f32::from(u16::MAX) {
        u16::MAX
    } else {
        pixels as u16
    }
}

fn pty_pid(pty: &Pty) -> u32 {
    #[cfg(unix)]
    {
        pty.child().id()
    }
    #[cfg(windows)]
    {
        pty.child_watcher()
            .pid()
            .map(std::num::NonZeroU32::get)
            .unwrap_or(0)
    }
}

fn kill_shell(pid: u32) {
    #[cfg(unix)]
    kill_process_group(pid);
    #[cfg(windows)]
    kill_process_tree(pid);
}

#[cfg(unix)]
fn kill_process_group(pid: u32) {
    if pid <= 1 {
        return;
    }
    let Some(pid) = rustix::process::Pid::from_raw(pid as i32) else {
        return;
    };
    if pid == rustix::process::getpid() || pid == rustix::process::getpgrp() {
        return;
    }
    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
}

#[cfg(windows)]
fn kill_process_tree(pid: u32) {
    if pid <= 1 {
        return;
    }
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

pub fn focus_id(view_id: Id) -> Id {
    view_id.with("terminal")
}

pub fn surrender_focus(ctx: &egui::Context, view_id: Id) {
    let id = focus_id(view_id);
    ctx.memory_mut(|memory| memory.surrender_focus(id));
}

/// Apply PTY replies and notice a shell that exited on its own.
pub fn service(ctx: &egui::Context, terminal: &mut Option<Terminal>) {
    let Some(Terminal::Live(session)) = terminal else {
        return;
    };
    if session.poll(ctx) {
        *terminal = Some(Terminal::Exited {
            message: "The shell exited.".to_owned(),
        });
    }
}

/// Paint the strip and, the first time, start the shell.
pub fn show(
    ui: &mut Ui,
    terminal: &mut Option<Terminal>,
    cwd: &Path,
    view_id: Id,
    palette: &Palette,
) {
    let rect = ui.available_rect_before_wrap();
    let metrics = metrics(ui, rect);
    ui.painter().rect_filled(rect, 0.0, palette.background);

    if terminal.is_none() {
        *terminal = start_shell(ui.ctx(), cwd, &metrics);
    }

    let mut restart = false;
    match terminal {
        Some(Terminal::Exited { message }) => {
            restart = exited_prompt(ui, rect, message, palette);
        }
        Some(Terminal::Live(session)) => {
            let response = ui.interact(rect, focus_id(view_id), Sense::click_and_drag());
            let response = response.on_hover_cursor(CursorIcon::Text);
            if response.hovered() && ui.input(|input| input.pointer.primary_pressed()) {
                response.request_focus();
            }
            session.resize(&metrics, ui.pixels_per_point());
            let focused = response.has_focus();
            handle_pointer(ui, session, &response, rect, &metrics);
            if focused {
                handle_keys(ui, session);
            }
            paint(ui.painter(), session, rect, &metrics, palette);
        }
        None => {}
    }
    if restart {
        *terminal = start_shell(ui.ctx(), cwd, &metrics);
    }
}

fn start_shell(ctx: &egui::Context, cwd: &Path, metrics: &Metrics) -> Option<Terminal> {
    match ShellSession::spawn(cwd, ctx, metrics) {
        Ok(session) => Some(Terminal::Live(session)),
        Err(message) => Some(Terminal::Exited { message }),
    }
}

fn exited_prompt(ui: &mut Ui, rect: Rect, message: &str, palette: &Palette) -> bool {
    let mut restart = false;
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space((rect.height() * 0.3).max(0.0));
            ui.label(RichText::new(message).color(palette.text_muted));
            ui.add_space(8.0);
            if ui.button("Start shell again").clicked() {
                restart = true;
            }
        });
    });
    restart
}

fn metrics(ui: &Ui, rect: Rect) -> Metrics {
    let font = TextStyle::Monospace.resolve(ui.style());
    let (cell_w, cell_h) = ui.ctx().fonts_mut(|fonts| {
        (
            fonts.glyph_width(&font, 'M').max(1.0),
            fonts.row_height(&font).max(1.0),
        )
    });
    Metrics {
        cols: ((rect.width() / cell_w).floor() as usize).max(1),
        rows: ((rect.height() / cell_h).floor() as usize).max(1),
        cell_w,
        cell_h,
        font,
    }
}

fn paint(
    painter: &Painter,
    session: &ShellSession,
    rect: Rect,
    metrics: &Metrics,
    palette: &Palette,
) {
    let term = session.term.lock();
    let content = term.renderable_content();
    let offset = content.display_offset;
    let cursor = content.cursor;
    let selection = content.selection;
    let colors = content.colors;
    for indexed in content.display_iter {
        if indexed
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        let row = indexed.point.line.0 + offset as i32;
        let col = indexed.point.column.0;
        if row < 0 || row >= metrics.rows as i32 || col >= metrics.cols {
            continue;
        }
        let selected = selection
            .is_some_and(|range| range.contains_cell(&indexed, cursor.point, cursor.shape));
        let mut fg = resolve_color(indexed.fg, colors, palette);
        let mut bg = resolve_color(indexed.bg, colors, palette);
        if indexed.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if selected {
            bg = palette.selection;
        }
        let on_cursor = cursor.point == indexed.point && cursor.shape != CursorShape::Hidden;
        if on_cursor && cursor.shape == CursorShape::Block {
            std::mem::swap(&mut fg, &mut bg);
        }
        let wide = indexed.flags.contains(Flags::WIDE_CHAR);
        let cell = cell_rect(rect, metrics, col, row as usize, wide);
        if bg != palette.background || selected || on_cursor {
            painter.rect_filled(cell, 0.0, bg);
        }
        if !indexed.flags.contains(Flags::HIDDEN) && indexed.c != ' ' {
            let mut text = String::new();
            text.push(indexed.c);
            if let Some(extra) = indexed.zerowidth() {
                text.extend(extra);
            }
            painter.text(cell.min, Align2::LEFT_TOP, text, metrics.font.clone(), fg);
        }
        if on_cursor && cursor.shape != CursorShape::Block {
            paint_cursor(painter, cell, cursor, fg);
        }
    }
}

fn paint_cursor(painter: &Painter, cell: Rect, cursor: RenderableCursor, color: Color32) {
    let rect = match cursor.shape {
        CursorShape::Underline => {
            Rect::from_min_max(Pos2::new(cell.min.x, cell.max.y - 2.0), cell.max)
        }
        CursorShape::Beam => Rect::from_min_max(cell.min, Pos2::new(cell.min.x + 2.0, cell.max.y)),
        CursorShape::HollowBlock => {
            painter.rect_stroke(cell, 0.0, Stroke::new(1.0, color), egui::StrokeKind::Inside);
            return;
        }
        CursorShape::Block | CursorShape::Hidden => return,
    };
    painter.rect_filled(rect, 0.0, color);
}

fn cell_rect(origin: Rect, metrics: &Metrics, col: usize, row: usize, wide: bool) -> Rect {
    let span = if wide { 2.0 } else { 1.0 };
    let min = Pos2::new(
        origin.min.x + col as f32 * metrics.cell_w,
        origin.min.y + row as f32 * metrics.cell_h,
    );
    Rect::from_min_size(min, Vec2::new(metrics.cell_w * span, metrics.cell_h))
}

fn resolve_color(color: Color, colors: &color::Colors, palette: &Palette) -> Color32 {
    match color {
        Color::Spec(rgb) => Color32::from_rgb(rgb.r, rgb.g, rgb.b),
        Color::Indexed(index) => stored_or_ansi(colors, usize::from(index), index),
        Color::Named(named) => {
            if let Some(rgb) = colors[named] {
                return Color32::from_rgb(rgb.r, rgb.g, rgb.b);
            }
            named_color(named, palette)
        }
    }
}

fn stored_or_ansi(colors: &color::Colors, index: usize, ansi_index: u8) -> Color32 {
    if index < color::COUNT
        && let Some(rgb) = colors[index]
    {
        return Color32::from_rgb(rgb.r, rgb.g, rgb.b);
    }
    let (r, g, b) = ansi_rgb(ansi_index);
    Color32::from_rgb(r, g, b)
}

fn named_color(named: NamedColor, palette: &Palette) -> Color32 {
    match named {
        NamedColor::Foreground | NamedColor::BrightForeground => palette.text,
        NamedColor::Background => palette.background,
        NamedColor::DimForeground => palette.text_muted,
        NamedColor::Cursor => palette.text,
        NamedColor::Black => stored_fallback(0),
        NamedColor::Red => stored_fallback(1),
        NamedColor::Green => stored_fallback(2),
        NamedColor::Yellow => stored_fallback(3),
        NamedColor::Blue => stored_fallback(4),
        NamedColor::Magenta => stored_fallback(5),
        NamedColor::Cyan => stored_fallback(6),
        NamedColor::White => stored_fallback(7),
        NamedColor::BrightBlack => stored_fallback(8),
        NamedColor::BrightRed => stored_fallback(9),
        NamedColor::BrightGreen => stored_fallback(10),
        NamedColor::BrightYellow => stored_fallback(11),
        NamedColor::BrightBlue => stored_fallback(12),
        NamedColor::BrightMagenta => stored_fallback(13),
        NamedColor::BrightCyan => stored_fallback(14),
        NamedColor::BrightWhite => stored_fallback(15),
        NamedColor::DimBlack => stored_fallback(0),
        NamedColor::DimRed => stored_fallback(1),
        NamedColor::DimGreen => stored_fallback(2),
        NamedColor::DimYellow => stored_fallback(3),
        NamedColor::DimBlue => stored_fallback(4),
        NamedColor::DimMagenta => stored_fallback(5),
        NamedColor::DimCyan => stored_fallback(6),
        NamedColor::DimWhite => stored_fallback(7),
    }
}

fn stored_fallback(index: u8) -> Color32 {
    let (r, g, b) = ansi_rgb(index);
    Color32::from_rgb(r, g, b)
}

fn ansi_rgb(index: u8) -> (u8, u8, u8) {
    const ANSI: [[u8; 3]; 16] = [
        [0x1a, 0x1b, 0x26],
        [0xf7, 0x76, 0x8e],
        [0x9e, 0xce, 0x6a],
        [0xe0, 0xaf, 0x68],
        [0x7a, 0xa2, 0xf7],
        [0xbb, 0x9a, 0xf7],
        [0x7d, 0xcf, 0xff],
        [0xc0, 0xca, 0xf5],
        [0x41, 0x48, 0x68],
        [0xff, 0x9e, 0x64],
        [0x73, 0xda, 0xca],
        [0xe0, 0xaf, 0x68],
        [0x2a, 0xc3, 0xde],
        [0xf7, 0x76, 0x8e],
        [0xb4, 0xf9, 0xf8],
        [0xd5, 0xdb, 0xe6],
    ];
    if index < 16 {
        let rgb = ANSI[usize::from(index)];
        return (rgb[0], rgb[1], rgb[2]);
    }
    if index < 232 {
        let cube = index - 16;
        let level = |step: u8| {
            if step == 0 { 0 } else { 55 + step * 40 }
        };
        return (level(cube / 36), level((cube / 6) % 6), level(cube % 6));
    }
    let gray = 8 + 10 * (index - 232);
    (gray, gray, gray)
}

fn handle_pointer(
    ui: &mut Ui,
    session: &mut ShellSession,
    response: &egui::Response,
    rect: Rect,
    metrics: &Metrics,
) {
    let (pressed, released, down, scroll_y, modifiers) = ui.input(|input| {
        let pointer = &input.pointer;
        (
            [
                pointer.button_pressed(PointerButton::Primary),
                pointer.button_pressed(PointerButton::Middle),
                pointer.button_pressed(PointerButton::Secondary),
            ],
            [
                pointer.button_released(PointerButton::Primary),
                pointer.button_released(PointerButton::Middle),
                pointer.button_released(PointerButton::Secondary),
            ],
            pointer.button_down(PointerButton::Primary),
            input.smooth_scroll_delta.y,
            input.modifiers,
        )
    });
    let mouse_mode = session.term.lock().mode().intersects(TermMode::MOUSE_MODE);
    let selecting = !mouse_mode || modifiers.shift;
    let over = response.hovered() || response.dragged();
    let Some(pos) = response
        .interact_pointer_pos()
        .or_else(|| ui.input(|input| input.pointer.hover_pos()))
    else {
        if released[0] {
            session.anchor = None;
        }
        return;
    };
    if !over && scroll_y == 0.0 && !pressed.iter().any(|pressed| *pressed) && !released[0] {
        return;
    }
    let hit = hit_at(rect, pos, metrics, session);

    if selecting {
        if pressed[0] {
            session.anchor = Some(hit.point);
            session.term.lock().selection = None;
        } else if down
            && response.dragged()
            && let Some(anchor) = session.anchor
        {
            let mut term = session.term.lock();
            if term.selection.is_none() {
                term.selection = Some(Selection::new(SelectionType::Simple, anchor, Side::Left));
            }
            if let Some(selection) = term.selection.as_mut() {
                selection.update(hit.point, Side::Right);
            }
        }
        if released[0] {
            session.anchor = None;
        }
    } else if over {
        report_buttons(session, hit.col, hit.row, &pressed, &released, modifiers);
        if response.dragged() && down {
            session.write(&sgr(
                button_code(0, modifiers) + 32,
                hit.col,
                hit.row,
                false,
            ));
        }
    }

    if over && scroll_y != 0.0 {
        ui.input_mut(|input| input.smooth_scroll_delta = Vec2::ZERO);
        scroll_or_report(
            session,
            scroll_y,
            metrics.cell_h,
            hit.col,
            hit.row,
            modifiers,
            mouse_mode,
        );
    }
}

fn report_buttons(
    session: &ShellSession,
    col: usize,
    row: usize,
    pressed: &[bool; 3],
    released: &[bool; 3],
    modifiers: Modifiers,
) {
    for (index, code) in [0_u8, 1, 2].into_iter().enumerate() {
        if pressed[index] {
            session.write(&sgr(button_code(code, modifiers), col, row, false));
        }
        if released[index] {
            session.write(&sgr(button_code(code, modifiers), col, row, true));
        }
    }
}

fn scroll_or_report(
    session: &mut ShellSession,
    scroll_y: f32,
    cell_h: f32,
    col: usize,
    row: usize,
    modifiers: Modifiers,
    mouse_mode: bool,
) {
    let steps = ((scroll_y.abs() / cell_h).round() as i32).clamp(1, 8);
    if mouse_mode {
        let button = if scroll_y > 0.0 { 64 } else { 65 };
        let code = button_code(button, modifiers);
        for _ in 0..steps {
            session.write(&sgr(code, col, row, false));
        }
        return;
    }
    let mut term = session.term.lock();
    if term.mode().contains(TermMode::ALT_SCREEN) {
        return;
    }
    let delta = if scroll_y > 0.0 { steps } else { -steps };
    term.scroll_display(Scroll::Delta(delta));
}

struct CellHit {
    point: Point,
    col: usize,
    row: usize,
}

fn hit_at(rect: Rect, pos: Pos2, metrics: &Metrics, session: &ShellSession) -> CellHit {
    let col = ((pos.x - rect.min.x) / metrics.cell_w).floor();
    let row = ((pos.y - rect.min.y) / metrics.cell_h).floor();
    let col = col.clamp(0.0, (metrics.cols - 1) as f32) as usize;
    let row = row.clamp(0.0, (metrics.rows - 1) as f32) as usize;
    let offset = session.term.lock().renderable_content().display_offset as i32;
    CellHit {
        point: Point::new(Line(row as i32 - offset), Column(col)),
        col,
        row,
    }
}

fn button_code(button: u8, modifiers: Modifiers) -> u8 {
    let mut code = button;
    if modifiers.shift {
        code += 4;
    }
    if modifiers.alt {
        code += 8;
    }
    if modifiers.ctrl {
        code += 16;
    }
    code
}

fn sgr(button: u8, col: usize, row: usize, release: bool) -> Vec<u8> {
    let action = if release { 'm' } else { 'M' };
    format!("\x1b[<{button};{};{}{action}", col + 1, row + 1).into_bytes()
}

fn handle_keys(ui: &mut Ui, session: &mut ShellSession) {
    let modifiers = ui.input(|input| input.modifiers);
    let mut events = ui.input_mut(|input| std::mem::take(&mut input.events));
    let mut kept = Vec::new();
    for event in events.drain(..) {
        if consumes(&event) {
            apply_key(ui.ctx(), session, event, modifiers);
        } else {
            kept.push(event);
        }
    }
    ui.input_mut(|input| input.events = kept);
}

fn consumes(event: &Event) -> bool {
    match event {
        Event::Text(_) | Event::Paste(_) | Event::Copy | Event::Cut => true,
        Event::Key { modifiers, .. } => !(cfg!(target_os = "macos") && modifiers.command),
        _ => false,
    }
}

fn apply_key(ctx: &egui::Context, session: &mut ShellSession, event: Event, modifiers: Modifiers) {
    match event {
        Event::Text(text) => {
            if !modifiers.alt && !modifiers.ctrl {
                session.write(text.as_bytes());
            }
        }
        Event::Paste(text) => session.write(&paste_bytes(&text, bracketed(session))),
        Event::Copy => copy_or_interrupt(ctx, session),
        Event::Cut => {
            if !cfg!(target_os = "macos") {
                session.write(&[0x18]);
            }
        }
        Event::Key {
            key,
            pressed,
            modifiers,
            ..
        } if pressed => {
            if let Some(bytes) = encode_key(session, key, modifiers) {
                session.write(&bytes);
            }
        }
        Event::Key { .. } => {}
        _ => {}
    }
}

fn bracketed(session: &ShellSession) -> bool {
    session
        .term
        .lock()
        .mode()
        .contains(TermMode::BRACKETED_PASTE)
}

fn copy_or_interrupt(ctx: &egui::Context, session: &mut ShellSession) {
    let text = session.term.lock().selection_to_string();
    if let Some(text) = text.filter(|text| !text.is_empty()) {
        ctx.copy_text(text);
        return;
    }
    if !cfg!(target_os = "macos") {
        session.write(&[0x03]);
    }
}

fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let mut payload = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        payload = format!("\x1b[200~{payload}\x1b[201~");
    }
    payload.into_bytes()
}

fn encode_key(session: &ShellSession, key: Key, modifiers: Modifiers) -> Option<Vec<u8>> {
    if key == Key::C && modifiers.ctrl && !modifiers.shift && !modifiers.alt && !modifiers.mac_cmd {
        let selected = session
            .term
            .lock()
            .selection_to_string()
            .is_some_and(|text| !text.is_empty());
        if cfg!(target_os = "macos") || !selected {
            return Some(vec![0x03]);
        }
        return None;
    }
    if let Some(bytes) = special_key(session, key, modifiers) {
        return Some(bytes);
    }
    letter_key(key, modifiers)
}

fn special_key(session: &ShellSession, key: Key, modifiers: Modifiers) -> Option<Vec<u8>> {
    let app_cursor = session.term.lock().mode().contains(TermMode::APP_CURSOR);
    let modified = xterm_mod(modifiers);
    let bytes = match key {
        Key::Enter => enter_bytes(modified),
        Key::Backspace => backspace_bytes(modifiers),
        Key::Tab => tab_bytes(modified),
        Key::Escape => vec![0x1b],
        Key::ArrowUp => arrow(b'A', modified, app_cursor),
        Key::ArrowDown => arrow(b'B', modified, app_cursor),
        Key::ArrowRight => arrow(b'C', modified, app_cursor),
        Key::ArrowLeft => arrow(b'D', modified, app_cursor),
        Key::Home => csi_tilde_or_letter(b'H', modified),
        Key::End => csi_tilde_or_letter(b'F', modified),
        Key::PageUp => csi_tilde(5, modified),
        Key::PageDown => csi_tilde(6, modified),
        Key::Delete => csi_tilde(3, modified),
        Key::Insert => csi_tilde(2, modified),
        Key::F1 => function_key(11, b'P', modified),
        Key::F2 => function_key(12, b'Q', modified),
        Key::F3 => function_key(13, b'R', modified),
        Key::F4 => function_key(14, b'S', modified),
        Key::F5 => csi_tilde(15, modified),
        Key::F6 => csi_tilde(17, modified),
        Key::F7 => csi_tilde(18, modified),
        Key::F8 => csi_tilde(19, modified),
        Key::F9 => csi_tilde(20, modified),
        Key::F10 => csi_tilde(21, modified),
        Key::F11 => csi_tilde(23, modified),
        Key::F12 => csi_tilde(24, modified),
        _ => return None,
    };
    Some(bytes)
}

fn enter_bytes(modified: u8) -> Vec<u8> {
    if modified == 1 {
        vec![b'\r']
    } else {
        format!("\x1b[27;{modified};13~").into_bytes()
    }
}

fn backspace_bytes(modifiers: Modifiers) -> Vec<u8> {
    if modifiers.alt {
        vec![0x1b, 0x7f]
    } else {
        vec![0x7f]
    }
}

fn tab_bytes(modified: u8) -> Vec<u8> {
    if modified == 1 {
        vec![b'\t']
    } else if modified == 2 {
        b"\x1b[Z".to_vec()
    } else {
        format!("\x1b[27;{modified};9~").into_bytes()
    }
}

fn arrow(letter: u8, modified: u8, app_cursor: bool) -> Vec<u8> {
    if modified == 1 {
        if app_cursor {
            vec![0x1b, b'O', letter]
        } else {
            vec![0x1b, b'[', letter]
        }
    } else {
        format!("\x1b[1;{modified}{}", letter as char).into_bytes()
    }
}

fn csi_tilde_or_letter(letter: u8, modified: u8) -> Vec<u8> {
    if modified == 1 {
        vec![0x1b, b'[', letter]
    } else {
        format!("\x1b[1;{modified}{}", letter as char).into_bytes()
    }
}

fn csi_tilde(number: u8, modified: u8) -> Vec<u8> {
    if modified == 1 {
        format!("\x1b[{number}~").into_bytes()
    } else {
        format!("\x1b[{number};{modified}~").into_bytes()
    }
}

fn function_key(number: u8, ss3: u8, modified: u8) -> Vec<u8> {
    if modified == 1 {
        vec![0x1b, b'O', ss3]
    } else {
        format!("\x1b[{number};{modified}~").into_bytes()
    }
}

fn xterm_mod(modifiers: Modifiers) -> u8 {
    let mut value = 1;
    if modifiers.shift {
        value += 1;
    }
    if modifiers.alt {
        value += 2;
    }
    if modifiers.ctrl {
        value += 4;
    }
    value
}

fn letter_key(key: Key, modifiers: Modifiers) -> Option<Vec<u8>> {
    let letter = letter(key)?;
    if modifiers.ctrl {
        let mut bytes = Vec::new();
        if modifiers.alt {
            bytes.push(0x1b);
        }
        bytes.push(letter as u8 - b'a' + 1);
        return Some(bytes);
    }
    if modifiers.alt {
        let ch = if modifiers.shift {
            letter.to_ascii_uppercase()
        } else {
            letter
        };
        let mut bytes = vec![0x1b];
        let mut buf = [0; 4];
        bytes.extend(ch.encode_utf8(&mut buf).as_bytes());
        return Some(bytes);
    }
    None
}

fn letter(key: Key) -> Option<char> {
    Some(match key {
        Key::A => 'a',
        Key::B => 'b',
        Key::C => 'c',
        Key::D => 'd',
        Key::E => 'e',
        Key::F => 'f',
        Key::G => 'g',
        Key::H => 'h',
        Key::I => 'i',
        Key::J => 'j',
        Key::K => 'k',
        Key::L => 'l',
        Key::M => 'm',
        Key::N => 'n',
        Key::O => 'o',
        Key::P => 'p',
        Key::Q => 'q',
        Key::R => 'r',
        Key::S => 's',
        Key::T => 't',
        Key::U => 'u',
        Key::V => 'v',
        Key::W => 'w',
        Key::X => 'x',
        Key::Y => 'y',
        Key::Z => 'z',
        _ => return None,
    })
}

/// Height of the terminal strip. Dragging the value never hides it.
pub fn split_height(ui: &mut Ui, view_id: Id, available: f32) -> (f32, f32, f32) {
    let usable = (available - SPLITTER_HEIGHT).max(0.0);
    let id = view_id.with("terminal-fraction");
    let stored = ui.data(|data| data.get_temp::<f32>(id));
    let fraction = clamp_fraction(stored.unwrap_or(INITIAL_FRACTION), usable);
    let terminal = usable * fraction;
    let detail = usable - terminal;
    (detail, SPLITTER_HEIGHT, terminal)
}

pub fn drag_split(ui: &mut Ui, view_id: Id, available: f32, drag_y: f32) {
    let usable = (available - SPLITTER_HEIGHT).max(1.0);
    let id = view_id.with("terminal-fraction");
    let stored = ui
        .data(|data| data.get_temp::<f32>(id))
        .unwrap_or(INITIAL_FRACTION);
    let current = clamp_fraction(stored, usable) * usable;
    let fraction = clamp_fraction((current - drag_y) / usable, usable);
    ui.data_mut(|data| data.insert_temp(id, fraction));
}

fn clamp_fraction(fraction: f32, usable: f32) -> f32 {
    if usable <= 0.0 {
        return INITIAL_FRACTION;
    }
    let terminal_min = TERMINAL_MIN_HEIGHT.min(usable * 0.5);
    let detail_min = DETAIL_MIN_HEIGHT.min(usable * 0.5);
    let height = (usable * fraction).clamp(terminal_min, (usable - detail_min).max(terminal_min));
    height / usable
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_program_follows_shell_and_uses_powershell_on_windows() {
        assert_eq!(
            shell_program(HostOs::Unix, Some(OsStr::new("/bin/zsh"))),
            OsString::from("/bin/zsh")
        );
        assert_eq!(
            shell_program(HostOs::Unix, Some(OsStr::new(""))),
            OsString::from("/bin/sh")
        );
        assert_eq!(shell_program(HostOs::Unix, None), OsString::from("/bin/sh"));
        assert_eq!(
            shell_program(HostOs::Windows, Some(OsStr::new("/bin/zsh"))),
            OsString::from("powershell.exe")
        );
        assert_eq!(
            shell_program(HostOs::Windows, Some(OsStr::new(""))),
            OsString::from("powershell.exe")
        );
        assert_eq!(
            shell_program(HostOs::Windows, None),
            OsString::from("powershell.exe")
        );
    }

    #[cfg(unix)]
    #[test]
    fn shell_program_ignores_non_unicode_shell() {
        use std::os::unix::ffi::OsStringExt;
        let raw = OsString::from_vec(vec![0xff]);
        assert_eq!(
            shell_program(HostOs::Unix, Some(raw.as_os_str())),
            OsString::from("/bin/sh")
        );
    }
}
