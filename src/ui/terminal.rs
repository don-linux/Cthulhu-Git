//! Interactive shell for the open repository.
//!
//! The PTY is `alacritty_terminal`, not `Git::run`. The child is not killed
//! after 60 seconds. It does not inherit `GIT_*` (except ssh and askpass) or
//! `GH_TOKEN` / `GITHUB_TOKEN`. It is given `TERM` and `COLORTERM` so `clear`
//! and Ctrl+L erase the screen. It starts the first time the strip is shown
//! and dies with the [`OpenedRepo`](super::OpenedRepo).

use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

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
    Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2,
};

use super::theme::Palette;

const DETAIL_MIN_HEIGHT: f32 = 120.0;
const TERMINAL_MIN_HEIGHT: f32 = 80.0;
const SPLITTER_HEIGHT: f32 = 6.0;
const INITIAL_FRACTION: f32 = 0.40;

/// Which OS the shell choice is for. Tests pass each; the app passes its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostOs {
    Linux,
    Macos,
    Windows,
}

/// Program and arguments for the PTY child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellArgv {
    pub program: String,
    pub args: Vec<String>,
}

/// Argv for the embedded shell.
///
/// macOS is a login shell (`-l`) so `/etc/zprofile` and `~/.zprofile` run.
/// Linux stays non-login, so an interactive bash still reads `~/.bashrc`.
/// An empty or non-Unicode `SHELL` uses `passwd_shell`, then `/bin/sh`.
/// `unset` is applied with `/usr/bin/env -u`, which can remove names.
/// `Options.env` can only add them.
pub fn shell_argv(
    host: HostOs,
    shell: Option<&OsStr>,
    passwd_shell: Option<&str>,
    unset: &[&str],
) -> Result<ShellArgv, String> {
    let argv = bare_shell(host, shell, passwd_shell)?;
    if host == HostOs::Windows || unset.is_empty() {
        return Ok(argv);
    }
    let mut args = Vec::with_capacity(unset.len() * 2 + 2 + argv.args.len());
    for name in unset {
        if !env_name_is_safe(name) {
            continue;
        }
        args.push("-u".to_owned());
        args.push((*name).to_owned());
    }
    if args.is_empty() {
        return Ok(argv);
    }
    args.push("--".to_owned());
    args.push(argv.program);
    args.extend(argv.args);
    Ok(ShellArgv {
        program: "/usr/bin/env".to_owned(),
        args,
    })
}

fn bare_shell(
    host: HostOs,
    shell: Option<&OsStr>,
    passwd_shell: Option<&str>,
) -> Result<ShellArgv, String> {
    if host == HostOs::Windows {
        return Ok(ShellArgv {
            program: "powershell.exe".to_owned(),
            args: Vec::new(),
        });
    }
    let program = chosen_unix_shell(shell, passwd_shell);
    let args = if host == HostOs::Macos {
        vec!["-l".to_owned()]
    } else {
        Vec::new()
    };
    Ok(ShellArgv { program, args })
}

fn chosen_unix_shell(shell: Option<&OsStr>, passwd_shell: Option<&str>) -> String {
    if let Some(value) = shell
        && !value.is_empty()
        && let Some(text) = value.to_str()
    {
        return text.to_owned();
    }
    passwd_shell
        .filter(|shell| !shell.is_empty())
        .unwrap_or("/bin/sh")
        .to_owned()
}

fn env_name_is_safe(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn this_host() -> HostOs {
    if cfg!(windows) {
        HostOs::Windows
    } else if cfg!(target_os = "macos") {
        HostOs::Macos
    } else {
        HostOs::Linux
    }
}

fn vars_to_unset() -> Vec<String> {
    let mut names: Vec<String> = std::env::vars()
        .map(|(name, _)| name)
        .filter(|name| cthulhu_git::git::inherited_git_var_is_cleared(name))
        .collect();
    for name in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if std::env::var_os(name).is_some() {
            names.push(name.to_owned());
        }
    }
    names.sort();
    names.dedup();
    names
}

#[cfg(unix)]
fn passwd_shell() -> Option<String> {
    let uid = rustix::process::getuid().as_raw().to_string();
    let text = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in text.lines() {
        let mut parts = line.splitn(7, ':');
        let _name = parts.next()?;
        let _password = parts.next()?;
        let id = parts.next()?;
        let _gid = parts.next()?;
        let _gecos = parts.next()?;
        let _home = parts.next()?;
        let shell = parts.next()?;
        if id == uid && !shell.is_empty() {
            return Some(shell.to_owned());
        }
    }
    None
}

#[cfg(not(unix))]
fn passwd_shell() -> Option<String> {
    None
}

fn ensure_cwd(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        rustix::fs::access(path, rustix::fs::Access::EXEC_OK)
            .map_err(|error| format!("Cannot enter {}: {error}", path.display()))
    }
    #[cfg(not(unix))]
    {
        if path.is_dir() {
            Ok(())
        } else {
            Err(format!("Cannot enter {}.", path.display()))
        }
    }
}

/// Environment applied only to the PTY child.
///
/// `clear` and the shell binding for Ctrl+L (`clear-screen`) write the
/// terminfo `clear` capability. A parent `TERM` of `dumb`, or an empty one,
/// writes nothing, so the screen stays as it was. Alacritty's `setup_env`
/// chooses the same name and also sets `COLORTERM`, but it stores both on the
/// whole process. `tty::Options.env` overrides them for the child only.
fn shell_env() -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert(
        "TERM".to_owned(),
        term_name(alacritty_terminfo_installed()).to_owned(),
    );
    env.insert("COLORTERM".to_owned(), "truecolor".to_owned());
    env
}

/// `alacritty` when that terminfo entry is installed, otherwise `xterm-256color`.
fn term_name(alacritty_installed: bool) -> &'static str {
    if alacritty_installed {
        "alacritty"
    } else {
        "xterm-256color"
    }
}

fn alacritty_terminfo_installed() -> bool {
    terminfo_installed("alacritty")
}

/// Same lookup as `alacritty_terminal::tty::setup_env`: `TERMINFO`, otherwise
/// `~/.terminfo`, then `TERMINFO_DIRS`, `PREFIX`, and the system databases.
fn terminfo_installed(name: &str) -> bool {
    let Some(first) = name.chars().next() else {
        return false;
    };
    let letter = first.to_string();
    let hex = format!("{:x}", u32::from(first));
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::var_os("TERMINFO") {
        dirs.push(PathBuf::from(dir));
    } else if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
    {
        dirs.push(PathBuf::from(home).join(".terminfo"));
    }
    if let Ok(list) = std::env::var("TERMINFO_DIRS") {
        dirs.extend(list.split(':').map(PathBuf::from));
    }
    if let Ok(prefix) = std::env::var("PREFIX") {
        let prefix = PathBuf::from(prefix);
        dirs.push(prefix.join("etc/terminfo"));
        dirs.push(prefix.join("lib/terminfo"));
        dirs.push(prefix.join("share/terminfo"));
    }
    dirs.push(PathBuf::from("/etc/terminfo"));
    dirs.push(PathBuf::from("/lib/terminfo"));
    dirs.push(PathBuf::from("/usr/share/terminfo"));
    dirs.push(PathBuf::from("/boot/system/data/terminfo"));
    dirs.iter()
        .any(|dir| dir.join(&letter).join(name).exists() || dir.join(&hex).join(name).exists())
}

/// One shell for the open repository, or the message left after it exits.
pub enum Terminal {
    /// `tty::new` is running off the egui thread. A second frame must not start another.
    Starting(StartingShell),
    Live(ShellSession),
    Exited {
        message: String,
    },
}

pub(crate) struct StartingShell {
    rx: Receiver<Result<ShellSession, String>>,
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

#[derive(Clone)]
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
        // Join on a side thread. Waiting here would freeze the window, and a
        // pid that the reader already reaped must not be signaled.
        let pid = self.pid;
        let join = self.join.take();
        let _ = self.io.send(Msg::Shutdown);
        thread::spawn(move || {
            stop_session(pid);
            if let Some(join) = join {
                let _ = join.join();
            }
        });
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
        let unset = vars_to_unset();
        let unset_refs: Vec<&str> = unset.iter().map(String::as_str).collect();
        let argv = shell_argv(
            this_host(),
            std::env::var_os("SHELL").as_deref(),
            passwd_shell().as_deref(),
            &unset_refs,
        )?;
        let options = tty::Options {
            shell: Some(tty::Shell::new(argv.program, argv.args)),
            working_directory: Some(cwd.to_path_buf()),
            env: shell_env(),
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

/// `true` when `pid` is a live process we are allowed to signal.
/// A reaped pid, pid 1, and this process are not.
fn should_signal(pid: u32) -> bool {
    #[cfg(unix)]
    {
        if pid <= 1 {
            return false;
        }
        let Some(parsed) = rustix::process::Pid::from_raw(pid as i32) else {
            return false;
        };
        if parsed == rustix::process::getpid() || parsed == rustix::process::getpgrp() {
            return false;
        }
        rustix::process::test_kill_process(parsed).is_ok()
    }
    #[cfg(not(unix))]
    {
        pid > 1
    }
}

/// Stop the shell's session. A reaped leader is left alone so the pid cannot
/// be recycled into an unrelated process. The caller's own session is never
/// signaled: the PTY child called `setsid`, so its session is not ours.
fn stop_session(leader: u32) {
    #[cfg(unix)]
    {
        if !should_signal(leader) {
            return;
        }
        let mine = session_of(std::process::id());
        let Some(session) = session_of(leader) else {
            signal_group(leader, rustix::process::Signal::KILL);
            return;
        };
        if mine == Some(session) {
            return;
        }
        let members = processes_in_session(session);
        for pid in &members {
            signal_pid(*pid, rustix::process::Signal::HUP);
        }
        thread::sleep(Duration::from_millis(50));
        for pid in members {
            signal_pid(pid, rustix::process::Signal::KILL);
        }
    }
    #[cfg(windows)]
    kill_process_tree(leader);
}

#[cfg(unix)]
fn signal_pid(pid: u32, signal: rustix::process::Signal) {
    if !should_signal(pid) {
        return;
    }
    let Some(pid) = rustix::process::Pid::from_raw(pid as i32) else {
        return;
    };
    let _ = rustix::process::kill_process(pid, signal);
}

#[cfg(unix)]
fn signal_group(pid: u32, signal: rustix::process::Signal) {
    let Some(pid) = rustix::process::Pid::from_raw(pid as i32) else {
        return;
    };
    if pid == rustix::process::getpid() || pid == rustix::process::getpgrp() {
        return;
    }
    let _ = rustix::process::kill_process_group(pid, signal);
}

/// Session id from a `/proc/<pid>/stat` line. `comm` may contain spaces and
/// parentheses, so the split is the last `)`.
#[cfg(target_os = "linux")]
fn session_from_stat(text: &str) -> Option<i32> {
    let rest = text.rsplit_once(')')?.1;
    let mut fields = rest.split_whitespace();
    let _state = fields.next()?;
    let _ppid = fields.next()?;
    let _pgrp = fields.next()?;
    fields.next()?.parse().ok()
}

#[cfg(unix)]
fn session_of(pid: u32) -> Option<i32> {
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        session_from_stat(&text)
    }
    #[cfg(target_os = "macos")]
    {
        session_of_ps(pid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

#[cfg(target_os = "macos")]
fn session_of_ps(pid: u32) -> Option<i32> {
    let output = std::process::Command::new("/bin/ps")
        .args(["-o", "sess=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}

#[cfg(target_os = "linux")]
fn processes_in_session(session: i32) -> Vec<u32> {
    let mut pids = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return pids;
    };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        if session_of(pid) == Some(session) {
            pids.push(pid);
        }
    }
    pids
}

#[cfg(target_os = "macos")]
fn processes_in_session(session: i32) -> Vec<u32> {
    let Ok(output) = std::process::Command::new("/bin/ps")
        .args(["-ax", "-o", "pid=,sess="])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let Ok(text) = String::from_utf8(output.stdout) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid = parts.next()?.parse::<u32>().ok()?;
            let sess = parts.next()?.parse::<i32>().ok()?;
            (sess == session).then_some(pid)
        })
        .collect()
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn processes_in_session(_session: i32) -> Vec<u32> {
    Vec::new()
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
    adopt_starting(terminal);
    let Some(Terminal::Live(session)) = terminal else {
        return;
    };
    if session.poll(ctx) {
        *terminal = Some(Terminal::Exited {
            message: "The shell exited.".to_owned(),
        });
    }
}

fn adopt_starting(terminal: &mut Option<Terminal>) {
    let Some(Terminal::Starting(starting)) = terminal else {
        return;
    };
    match starting.rx.try_recv() {
        Ok(Ok(session)) => *terminal = Some(Terminal::Live(session)),
        Ok(Err(message)) => *terminal = Some(Terminal::Exited { message }),
        Err(TryRecvError::Empty) => {}
        Err(TryRecvError::Disconnected) => {
            *terminal = Some(Terminal::Exited {
                message: "Could not start the shell.".to_owned(),
            });
        }
    }
}

/// Screen the settings preview keeps. The widget draws one line; a one-row
/// PTY scrolls the prompt off that line and leaves the cursor.
const PREVIEW_SCREEN_HEIGHT: f32 = 240.0;

/// Paint the strip and, the first time, start the shell.
///
/// When `interactive` is false the shell still runs, so the prompt uses the
/// current face, but pointer and keyboard are ignored. The widget is clipped
/// to one line. The PTY stays [`PREVIEW_SCREEN_HEIGHT`] tall.
pub fn show(
    ui: &mut Ui,
    terminal: &mut Option<Terminal>,
    cwd: &Path,
    view_id: Id,
    palette: &Palette,
    interactive: bool,
) {
    let visible = ui.available_rect_before_wrap();
    let grid = if interactive {
        visible
    } else {
        Rect::from_min_size(
            visible.min,
            Vec2::new(visible.width().max(1.0), PREVIEW_SCREEN_HEIGHT),
        )
    };
    let metrics = metrics(ui, grid);
    let painter = ui.painter().with_clip_rect(visible);
    painter.rect_filled(visible, 0.0, palette.background);

    adopt_starting(terminal);
    if terminal.is_none() {
        *terminal = Some(begin_shell(ui.ctx(), cwd, &metrics));
        adopt_starting(terminal);
    }
    if matches!(terminal, Some(Terminal::Starting(_))) {
        ui.ctx().request_repaint();
        painter.text(
            visible.center(),
            Align2::CENTER_CENTER,
            "Starting shell…",
            metrics.font.clone(),
            palette.text_muted,
        );
    }

    if !interactive {
        surrender_focus(ui.ctx(), view_id);
    }

    let mut restart = false;
    match terminal {
        Some(Terminal::Exited { message }) => {
            restart = exited_prompt(ui, visible, message, palette, interactive);
        }
        Some(Terminal::Live(session)) => {
            if interactive {
                let response = ui.interact(visible, focus_id(view_id), Sense::click_and_drag());
                let response = response.on_hover_cursor(CursorIcon::Text);
                if response.hovered() && ui.input(|input| input.pointer.primary_pressed()) {
                    response.request_focus();
                }
                session.resize(&metrics, ui.pixels_per_point());
                let focused = response.has_focus();
                handle_pointer(ui, session, &response, visible, &metrics);
                if focused {
                    handle_keys(ui, session);
                }
            } else {
                session.resize(&metrics, ui.pixels_per_point());
            }
            paint(&painter, session, visible, &metrics, palette);
        }
        Some(Terminal::Starting(_)) | None => {}
    }
    if restart {
        *terminal = Some(begin_shell(ui.ctx(), cwd, &metrics));
    }
}

fn begin_shell(ctx: &egui::Context, cwd: &Path, metrics: &Metrics) -> Terminal {
    if let Err(message) = ensure_cwd(cwd) {
        return Terminal::Exited { message };
    }
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    let cwd = cwd.to_path_buf();
    let metrics = metrics.clone();
    thread::spawn(move || {
        let result = ShellSession::spawn(&cwd, &ctx, &metrics);
        // A dropped receiver means the repository closed first. Drop the
        // session on this worker, not on the egui thread.
        let _ = tx.send(result);
    });
    Terminal::Starting(StartingShell { rx })
}

fn exited_prompt(
    ui: &mut Ui,
    rect: Rect,
    message: &str,
    palette: &Palette,
    interactive: bool,
) -> bool {
    if !interactive {
        let font = egui::TextStyle::Body.resolve(ui.style());
        ui.painter().text(
            rect.left_top(),
            Align2::LEFT_TOP,
            message,
            font,
            palette.text_muted,
        );
        return false;
    }
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
    let font = super::fonts::terminal_font_id(ui);
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
    fn shell_argv_follows_shell_login_and_unset() {
        let zsh = shell_argv(HostOs::Linux, Some(OsStr::new("/bin/zsh")), None, &[]).expect("zsh");
        assert_eq!(zsh.program, "/bin/zsh");
        assert!(zsh.args.is_empty());

        let login =
            shell_argv(HostOs::Macos, Some(OsStr::new("/bin/zsh")), None, &[]).expect("login");
        assert_eq!(login.program, "/bin/zsh");
        assert_eq!(login.args, ["-l"]);

        let scrubbed = shell_argv(
            HostOs::Macos,
            Some(OsStr::new("/bin/zsh")),
            None,
            &["GIT_DIR", "GH_TOKEN"],
        )
        .expect("scrubbed");
        assert_eq!(scrubbed.program, "/usr/bin/env");
        assert_eq!(
            scrubbed.args,
            ["-u", "GIT_DIR", "-u", "GH_TOKEN", "--", "/bin/zsh", "-l"]
        );

        let passwd =
            shell_argv(HostOs::Linux, None, Some("/bin/bash"), &["GIT_EXEC_PATH"]).expect("passwd");
        assert_eq!(passwd.program, "/usr/bin/env");
        assert_eq!(passwd.args, ["-u", "GIT_EXEC_PATH", "--", "/bin/bash"]);

        let fallback =
            shell_argv(HostOs::Linux, Some(OsStr::new("")), None, &[]).expect("fallback");
        assert_eq!(fallback.program, "/bin/sh");

        let windows = shell_argv(
            HostOs::Windows,
            Some(OsStr::new("/bin/zsh")),
            None,
            &["GIT_DIR"],
        )
        .expect("windows");
        assert_eq!(windows.program, "powershell.exe");
        assert!(windows.args.is_empty());
    }

    #[test]
    fn shell_argv_skips_an_unsafe_unset_name() {
        let argv = shell_argv(
            HostOs::Linux,
            Some(OsStr::new("/bin/sh")),
            None,
            &["GIT_DIR;touch /tmp/x", "GH_TOKEN"],
        )
        .expect("argv");
        assert_eq!(argv.args, ["-u", "GH_TOKEN", "--", "/bin/sh"]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn session_from_stat_uses_the_last_paren() {
        assert_eq!(session_from_stat("12 (bash) S 1 12 99 0"), Some(99));
        assert_eq!(session_from_stat("12 (weir)d) S 1 12 42 0"), Some(42));
    }

    #[cfg(unix)]
    #[test]
    fn should_signal_refuses_init_and_this_process() {
        assert!(!should_signal(0));
        assert!(!should_signal(1));
        assert!(!should_signal(std::process::id()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stop_session_kills_a_background_job_that_ignores_hup() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let sleeper = dir.path().join("sleeper");
        let leader_file = dir.path().join("leader");
        let script = format!(
            "trap '' HUP; sleep 30 & echo $! > '{}'; echo $$ > '{}'; wait",
            sleeper.display(),
            leader_file.display()
        );
        let mut child = std::process::Command::new("setsid")
            .args(["/bin/sh", "-c", &script])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("setsid");
        let leader = wait_for_pid(&leader_file);
        let sleep_pid = wait_for_pid(&sleeper);
        stop_session(leader);
        let started = std::time::Instant::now();
        while started.elapsed() < Duration::from_secs(1)
            && (process_is_running(sleep_pid) || process_is_running(leader))
        {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!process_is_running(sleep_pid), "background sleep survived");
        assert!(!process_is_running(leader), "session leader survived");
        let _ = child.wait();
    }

    #[cfg(target_os = "linux")]
    fn process_is_running(pid: u32) -> bool {
        let Ok(text) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        let Some((_, rest)) = text.rsplit_once(')') else {
            return false;
        };
        let state = rest.split_whitespace().next().unwrap_or("");
        matches!(state, "R" | "S" | "D" | "T")
    }

    #[cfg(target_os = "linux")]
    fn wait_for_pid(path: &Path) -> u32 {
        let started = std::time::Instant::now();
        loop {
            if let Ok(text) = std::fs::read_to_string(path)
                && let Ok(pid) = text.trim().parse::<u32>()
                && pid > 1
            {
                return pid;
            }
            if started.elapsed() > Duration::from_secs(2) {
                panic!("pid file {} was not written", path.display());
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn missing_cwd_is_not_a_usable_shell_directory() {
        let missing = std::env::temp_dir().join("cthulhu-missing-cwd-does-not-exist");
        let error = ensure_cwd(&missing).expect_err("missing");
        assert!(error.contains("Cannot enter"), "{error}");
    }

    #[test]
    fn term_name_prefers_alacritty_when_that_terminfo_exists() {
        assert_eq!(term_name(true), "alacritty");
        assert_eq!(term_name(false), "xterm-256color");
    }

    #[test]
    fn ctrl_l_is_form_feed() {
        assert_eq!(
            letter_key(
                Key::L,
                Modifiers {
                    ctrl: true,
                    ..Modifiers::NONE
                }
            ),
            Some(vec![0x0c])
        );
    }

    #[cfg(unix)]
    #[test]
    fn shell_program_ignores_non_unicode_shell() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let raw = OsString::from_vec(vec![0xff]);
        assert_eq!(
            shell_argv(HostOs::Linux, Some(raw.as_os_str()), None, &[])
                .expect("argv")
                .program,
            "/bin/sh"
        );
    }
}
