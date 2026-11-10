//! Interactive shell for the open repository.
//!
//! The PTY is `alacritty_terminal`, not `Git::run`. The child is not killed
//! after 60 seconds. On Unix it does not inherit `GIT_*` (except ssh and
//! askpass) or `GH_TOKEN` / `GITHUB_TOKEN`. Windows ConPTY merges
//! `Options.env` with the parent environment, so those names cannot be
//! removed there. It is given `TERM` and `COLORTERM` so `clear`
//! and Ctrl+L erase the screen. It starts the first time the strip is shown
//! and dies with the [`OpenedRepo`](super::OpenedRepo).

use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};
#[cfg(unix)]
use std::time::Duration;

use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg, State};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{
    Config as TermConfig, Osc52, RenderableCursor, Term, TermMode, color,
};
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
/// A relative, empty, non-Unicode, missing, or non-executable `SHELL` uses
/// an absolute executable `passwd_shell`. macOS then tries `/bin/zsh`.
/// `/bin/sh` is the last resort.
/// `unset` is applied with `/usr/bin/env -u`, which can remove names.
/// `Options.env` can only add them. On Windows that map is merged into the
/// parent environment, so `unset` is not applied.
pub fn shell_argv(
    host: HostOs,
    shell: Option<&OsStr>,
    passwd_shell: Option<&str>,
    unset: &[&str],
) -> Result<ShellArgv, String> {
    let argv = bare_shell(host, shell, passwd_shell)?;
    if host == HostOs::Windows || unset.is_empty() {
        // Windows `tty::Options.env` cannot drop inherited names.
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
        // `Command` searches the working directory before `PATH`. A bare
        // `powershell.exe` would run a binary planted in the repository.
        return windows_shell_argv(
            std::env::var("ProgramFiles").ok().as_deref(),
            std::env::var("SystemRoot").ok().as_deref(),
            std::env::var("COMSPEC").ok().as_deref(),
            |path| Path::new(path).is_file(),
        );
    }
    let program = chosen_unix_shell(host, shell, passwd_shell);
    let args = if host == HostOs::Macos {
        vec!["-l".to_owned()]
    } else {
        Vec::new()
    };
    Ok(ShellArgv { program, args })
}

fn chosen_unix_shell(host: HostOs, shell: Option<&OsStr>, passwd_shell: Option<&str>) -> String {
    chosen_unix_shell_with(host, shell, passwd_shell, unix_shell_is_executable)
}

fn chosen_unix_shell_with(
    host: HostOs,
    shell: Option<&OsStr>,
    passwd_shell: Option<&str>,
    executable: impl Fn(&str) -> bool,
) -> String {
    let shell_text = shell.and_then(OsStr::to_str);
    let candidates = [
        shell_text,
        passwd_shell,
        (host == HostOs::Macos).then_some("/bin/zsh"),
        Some("/bin/sh"),
    ];
    for candidate in candidates.into_iter().flatten() {
        if unix_shell_is_absolute(candidate) && executable(candidate) {
            return candidate.to_owned();
        }
    }
    "/bin/sh".to_owned()
}

/// Absolute and executable. A missing path or a mode without an execute bit
/// falls through so the PTY is not pointed at a file `execve` will reject.
fn unix_shell_is_executable(path: &str) -> bool {
    if !unix_shell_is_absolute(path) {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        // Permission bits are a Unix property. Callers on this target still
        // require an absolute path via [`unix_shell_is_absolute`].
        let _ = path;
        true
    }
}

/// PowerShell, as a pure function of the Windows environment.
///
/// Order is `%ProgramFiles%\PowerShell\7\pwsh.exe`, then
/// `%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe`.
/// Relative paths are skipped. `COMSPEC` is not a candidate: a relative
/// value would search the repository, and an absolute one is often `cmd.exe`.
fn windows_shell_argv(
    program_files: Option<&str>,
    system_root: Option<&str>,
    _comspec: Option<&str>,
    exists: impl Fn(&str) -> bool,
) -> Result<ShellArgv, String> {
    let candidates = [
        program_files.map(|root| windows_join(root, r"PowerShell\7\pwsh.exe")),
        system_root
            .map(|root| windows_join(root, r"System32\WindowsPowerShell\v1.0\powershell.exe")),
    ];
    for candidate in candidates.into_iter().flatten() {
        if windows_path_is_absolute(&candidate) && exists(&candidate) {
            return Ok(ShellArgv {
                program: candidate,
                args: vec!["-NoLogo".to_owned()],
            });
        }
    }
    Err("Could not find an absolute PowerShell.".to_owned())
}

fn windows_join(base: &str, child: &str) -> String {
    let base = base.trim_end_matches(['\\', '/']);
    format!("{base}\\{child}")
}

/// Drive path (`C:\...`) or UNC (`\\server\share`). `Path::is_absolute` is
/// false for both when this code is tested on Unix.
fn windows_path_is_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
    {
        return true;
    }
    bytes.len() >= 3 && bytes[0] == b'\\' && bytes[1] == b'\\'
}

#[cfg(any(windows, test))]
fn windows_taskkill_path(
    system_root: Option<&str>,
    exists: impl Fn(&str) -> bool,
) -> Result<String, String> {
    let Some(root) = system_root.filter(|root| windows_path_is_absolute(root)) else {
        return Err("SystemRoot is unset or not absolute.".to_owned());
    };
    let path = windows_join(root, r"System32\taskkill.exe");
    if windows_path_is_absolute(&path) && exists(&path) {
        Ok(path)
    } else {
        Err(format!("taskkill is not at {path}."))
    }
}

/// Absolute Unix path. On Unix this is [`Path::is_absolute`].
///
/// The Linux shell choice is also unit-tested on Windows, where `Path` does
/// not treat `/bin/sh` as absolute.
fn unix_shell_is_absolute(text: &str) -> bool {
    if cfg!(unix) {
        Path::new(text).is_absolute()
    } else {
        text.starts_with('/')
    }
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
    shell_env_from(
        alacritty_terminfo_installed(),
        std::env::var("LANG").ok().as_deref(),
        std::env::var("LC_ALL").ok().as_deref(),
        std::env::var("LC_CTYPE").ok().as_deref(),
    )
}

/// `TERM` and `COLORTERM` for the PTY child.
///
/// A Dock launch leaves `LANG`, `LC_ALL`, and `LC_CTYPE` unset, and the shell
/// then treats the stream as ASCII. An existing locale is left alone so the
/// child inherits it.
fn shell_env_from(
    alacritty_installed: bool,
    lang: Option<&str>,
    lc_all: Option<&str>,
    lc_ctype: Option<&str>,
) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert("TERM".to_owned(), term_name(alacritty_installed).to_owned());
    env.insert("COLORTERM".to_owned(), "truecolor".to_owned());
    if locale_is_unset(lang, lc_all, lc_ctype) {
        env.insert("LANG".to_owned(), "en_US.UTF-8".to_owned());
    }
    env
}

fn locale_is_unset(lang: Option<&str>, lc_all: Option<&str>, lc_ctype: Option<&str>) -> bool {
    fn blank(value: Option<&str>) -> bool {
        value.is_none_or(|text| text.is_empty())
    }
    blank(lang) && blank(lc_all) && blank(lc_ctype)
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
    /// Set once the PTY child has exited. A later `TIOCSWINSZ` on the hung-up
    /// slave makes alacritty call `process::exit`.
    child_exited: bool,
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
        // Stop the session before the PTY is closed. Dropping the PTY
        // SIGHUPs the leader and waits for it, which reparents a `setsid`
        // child before that child can be reaped.
        let pid = self.pid;
        let join = self.join.take();
        let io = self.io.clone();
        thread::spawn(move || {
            stop_session(pid);
            let _ = io.send(Msg::Shutdown);
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
            term_config(),
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
            child_exited: false,
        })
    }

    fn poll(&mut self, ctx: &egui::Context) -> bool {
        let mut exited = false;
        loop {
            match self.notices.try_recv() {
                Ok(Notice::Write(text)) => self.write(text.as_bytes()),
                Ok(Notice::Copy(text)) => ctx.copy_text(text),
                Ok(Notice::Exited) => {
                    self.child_exited = true;
                    exited = true;
                }
                Ok(Notice::Color(index, format)) => {
                    let rgb = self.color_rgb(index);
                    self.write(format(rgb).as_bytes());
                }
                Ok(Notice::Size(format)) => self.write(format(self.size).as_bytes()),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.child_exited = true;
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
        // `Msg::Resize` runs `TIOCSWINSZ` on the event-loop thread. alacritty
        // exits the process when that ioctl fails: EINVAL for a 0-size, EIO
        // after the slave hangs up. Compare cell pixels too, so a move
        // between 1x and 2x still updates `ws_xpixel`.
        if self.child_exited || !child_can_be_resized(self.pid) {
            return;
        }
        let size = window_size(metrics, pixels_per_point);
        if !pty_size_is_safe(size) || same_window_size(self.size, size) {
            return;
        }
        self.size = size;
        self.term.lock().resize(GridSize {
            columns: usize::from(size.num_cols),
            screen_lines: usize::from(size.num_lines),
        });
        let _ = self.io.send(Msg::Resize(size));
    }
}

/// OSC 52 is off. A program in the shell must not write the clipboard.
fn term_config() -> TermConfig {
    TermConfig {
        osc52: Osc52::Disabled,
        ..TermConfig::default()
    }
}

fn spawn_error(error: &io::Error) -> String {
    format!("Could not start the shell: {error}")
}

fn window_size(metrics: &Metrics, pixels_per_point: f32) -> WindowSize {
    let num_lines = u16::try_from(metrics.rows).unwrap_or(u16::MAX).max(1);
    let num_cols = u16::try_from(metrics.cols).unwrap_or(u16::MAX).max(1);
    WindowSize {
        num_lines,
        num_cols,
        cell_width: clamp_cell_px(num_cols, px(metrics.cell_w * pixels_per_point)),
        cell_height: clamp_cell_px(num_lines, px(metrics.cell_h * pixels_per_point)),
    }
}

fn px(points: f32) -> u16 {
    // `as u16` turns NaN and infinities into 0. A zero cell size makes
    // `TIOCSWINSZ` fail and alacritty exit the process.
    if !points.is_finite() {
        return 1;
    }
    let pixels = points.round();
    if !pixels.is_finite() || pixels <= 1.0 {
        1
    } else if pixels >= f32::from(u16::MAX) {
        u16::MAX
    } else {
        pixels as u16
    }
}

/// `cols * cell_px` is stored in a `u16` by alacritty's `Winsize`. Clamp the
/// product so it cannot wrap.
fn clamp_cell_px(cells: u16, cell_px: u16) -> u16 {
    let cells = u32::from(cells.max(1));
    let max_cell = u16::try_from(u32::from(u16::MAX) / cells).unwrap_or(1);
    cell_px.min(max_cell.max(1)).max(1)
}

fn same_window_size(left: WindowSize, right: WindowSize) -> bool {
    left.num_cols == right.num_cols
        && left.num_lines == right.num_lines
        && left.cell_width == right.cell_width
        && left.cell_height == right.cell_height
}

fn pty_size_is_safe(size: WindowSize) -> bool {
    let cols = u32::from(size.num_cols);
    let rows = u32::from(size.num_lines);
    let width = u32::from(size.cell_width);
    let height = u32::from(size.cell_height);
    cols >= 1
        && rows >= 1
        && width >= 1
        && height >= 1
        && cols * width <= u32::from(u16::MAX)
        && rows * height <= u32::from(u16::MAX)
}

fn child_can_be_resized(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unix_child_is_alive(pid)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

#[cfg(unix)]
fn unix_child_is_alive(pid: u32) -> bool {
    if pid <= 1 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        linux_pid_is_alive(pid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        should_signal(pid)
    }
}

#[cfg(target_os = "linux")]
fn linux_pid_is_alive(pid: u32) -> bool {
    let Ok(text) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let Some((_, rest)) = text.rsplit_once(')') else {
        return false;
    };
    let state = rest.split_whitespace().next().unwrap_or("");
    // A zombie still owns the pid, but the slave has hung up.
    !state.is_empty() && !matches!(state, "Z" | "X" | "x")
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
#[cfg(unix)]
fn should_signal(pid: u32) -> bool {
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

/// Stop the shell and its descendants. A reaped leader is left alone so the
/// pid cannot be recycled into an unrelated process. The caller's own session
/// is never signaled. Descendants are still reaped when the leader shares
/// that session, including a child that called `setsid`.
fn stop_session(leader: u32) {
    #[cfg(unix)]
    {
        if !should_signal(leader) {
            return;
        }
        let mine = session_of(std::process::id());
        // Snapshot before anyone is killed. A `setsid` child is absent here
        // and is reaped from the process tree instead.
        let members = match session_of(leader) {
            Some(session) if Some(session) != mine => Some(processes_in_session(session)),
            _ => None,
        };
        cthulhu_git::git::reap_process_tree(leader);
        if let Some(members) = members {
            for pid in &members {
                signal_pid(*pid, rustix::process::Signal::HUP);
            }
            thread::sleep(Duration::from_millis(50));
            for pid in members {
                signal_pid(pid, rustix::process::Signal::KILL);
            }
        }
    }
    #[cfg(windows)]
    {
        let _ = kill_process_tree(leader);
    }
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
fn kill_process_tree(pid: u32) -> Result<(), String> {
    if pid <= 1 {
        return Ok(());
    }
    // A bare `taskkill` searches the working directory and flashes a console.
    let program = windows_taskkill_path(std::env::var("SystemRoot").ok().as_deref(), |path| {
        Path::new(path).is_file()
    })?;
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let status = std::process::Command::new(&program)
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|error| format!("Could not run {program}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("taskkill exited with {status}"))
    }
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

/// Rows the settings preview keeps on the PTY. The widget draws only the
/// prompt block. A short screen scrolls a multi-line prompt away before it
/// can be measured, and a pixel height shrinks to a handful of rows when the
/// terminal face is large.
const PREVIEW_SCREEN_ROWS: usize = 32;

/// Most lines the settings preview draws. A longer block keeps the rows
/// nearest the cursor, so a noisy startup file does not take the page.
const PREVIEW_MAX_LINES: usize = 8;

/// Rows the settings preview should reserve for the current prompt.
///
/// One row until the shell is on screen. A live shell reports the prompt
/// block that ends at the cursor.
pub fn preview_line_count(terminal: &Option<Terminal>) -> usize {
    let Some(Terminal::Live(session)) = terminal else {
        return 1;
    };
    let term = session.term.lock();
    prompt_span(&term).len().max(1)
}

/// Screen rows of the prompt block, ending at the cursor.
///
/// A blank row ends the walk upward. Starship prints one before the prompt,
/// and a shell leaves one after command output. That blank stays out of the
/// preview. A block longer than [`PREVIEW_MAX_LINES`] keeps the rows nearest
/// the cursor.
fn prompt_span<T: EventListener>(term: &Term<T>) -> Range<usize> {
    let rows = term.screen_lines();
    if rows == 0 {
        return 0..1;
    }
    let content = term.renderable_content();
    let offset = content.display_offset as i32;
    let cursor_line = content.cursor.point.line.0;
    let mut filled = vec![false; rows];
    for indexed in content.display_iter {
        if indexed
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        let row = indexed.point.line.0 + offset;
        if row < 0 || row >= rows as i32 {
            continue;
        }
        if cell_holds_prompt(indexed.cell) {
            filled[row as usize] = true;
        }
    }
    let cursor_row = (cursor_line + offset).clamp(0, rows as i32 - 1) as usize;
    prompt_rows(&filled, cursor_row, PREVIEW_MAX_LINES)
}

/// A cell the preview treats as prompt content.
///
/// A colored or inverse space still counts. Powerline prompts pad with those.
fn cell_holds_prompt(cell: &Cell) -> bool {
    if cell.flags.contains(Flags::HIDDEN) {
        return false;
    }
    if cell.flags.contains(Flags::INVERSE) || cell.c != ' ' {
        return true;
    }
    if cell.zerowidth().is_some_and(|extra| !extra.is_empty()) {
        return true;
    }
    !matches!(cell.bg, Color::Named(NamedColor::Background))
}

/// `filled[row]` is set when that screen row has prompt content.
///
/// The range always includes `cursor_row`. It walks up through filled rows
/// and stops at the first blank. `max_lines` keeps the end nearest the cursor.
fn prompt_rows(filled: &[bool], cursor_row: usize, max_lines: usize) -> Range<usize> {
    if filled.is_empty() {
        return 0..1;
    }
    let cursor = cursor_row.min(filled.len() - 1);
    let max_lines = max_lines.max(1);
    let mut start = cursor;
    while start > 0 && filled[start - 1] {
        start -= 1;
    }
    let shown = cursor + 1 - start;
    if shown > max_lines {
        start = cursor + 1 - max_lines;
    }
    start..cursor + 1
}

/// Paint the strip and, the first time, start the shell.
///
/// When `interactive` is false the shell still runs, so the prompt uses the
/// current face, but pointer and keyboard are ignored. The widget shows the
/// prompt block. The PTY stays [`PREVIEW_SCREEN_ROWS`] tall so that block is
/// not scrolled away.
pub fn show(
    ui: &mut Ui,
    terminal: &mut Option<Terminal>,
    cwd: &Path,
    view_id: Id,
    palette: &Palette,
    interactive: bool,
) {
    let visible = ui.available_rect_before_wrap();
    let mut metrics = metrics(ui, visible);
    if !interactive {
        metrics.rows = PREVIEW_SCREEN_ROWS;
    }
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
            let mut origin = visible;
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
                let span = {
                    let term = session.term.lock();
                    prompt_span(&term)
                };
                origin = Rect::from_min_size(
                    Pos2::new(
                        visible.min.x,
                        visible.min.y - span.start as f32 * metrics.cell_h,
                    ),
                    Vec2::new(
                        visible.width().max(1.0),
                        metrics.rows as f32 * metrics.cell_h,
                    ),
                );
                // A column change reflows the prompt after the box was sized.
                let reserved = visible.height() / metrics.cell_h;
                if (reserved - span.len() as f32).abs() > 0.5 {
                    ui.ctx().request_repaint();
                }
            }
            paint(&painter, session, origin, &metrics, palette);
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
    // egui emits `Key` before `Text`. Option+key produces a composed
    // character; writing ESC+letter as well would discard it.
    let composed = events
        .iter()
        .any(|event| matches!(event, Event::Text(text) if option_text_is_composed(text)));
    let mut kept = Vec::new();
    for event in events.drain(..) {
        if composed && alt_letter_key_is_redundant(&event) {
            continue;
        }
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
            if let Some(bytes) = text_input_bytes(&text, modifiers) {
                session.write(&bytes);
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
    // Bracketed paste ends at ESC [201~. A paste that contains that sequence
    // is then typed as a command. C0 controls other than tab and newline are
    // removed before the markers are added. CR is the newline the PTY uses.
    let sanitized: String = text
        .chars()
        .filter(|ch| paste_char_is_allowed(*ch))
        .collect();
    let mut payload = sanitized.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        payload = format!("\x1b[200~{payload}\x1b[201~");
    }
    payload.into_bytes()
}

/// Bytes for `Event::Text`. Option's composed character is that text.
/// A plain ASCII letter with Alt still belongs to the ESC+letter path.
fn text_input_bytes(text: &str, modifiers: Modifiers) -> Option<Vec<u8>> {
    if modifiers.ctrl || text.is_empty() {
        return None;
    }
    if modifiers.alt && !option_text_is_composed(text) {
        return None;
    }
    Some(text.as_bytes().to_vec())
}

fn option_text_is_composed(text: &str) -> bool {
    let mut chars = text.chars();
    match chars.next() {
        Some(ch) if chars.next().is_none() && ch.is_ascii_alphabetic() => false,
        Some(_) => true,
        None => false,
    }
}

fn alt_letter_key_is_redundant(event: &Event) -> bool {
    match event {
        Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => modifiers.alt && !modifiers.ctrl && letter(*key).is_some(),
        _ => false,
    }
}

fn paste_char_is_allowed(ch: char) -> bool {
    match u32::from(ch) {
        0x09 | 0x0A | 0x0D => true,
        code if code < 0x20 => false,
        _ => true,
    }
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
        let bash =
            shell_argv(HostOs::Linux, Some(OsStr::new("/bin/bash")), None, &[]).expect("bash");
        assert_eq!(bash.program, "/bin/bash");
        assert!(bash.args.is_empty());

        let login =
            shell_argv(HostOs::Macos, Some(OsStr::new("/bin/bash")), None, &[]).expect("login");
        assert_eq!(login.program, "/bin/bash");
        assert_eq!(login.args, ["-l"]);

        let scrubbed = shell_argv(
            HostOs::Macos,
            Some(OsStr::new("/bin/bash")),
            None,
            &["GIT_DIR", "GH_TOKEN"],
        )
        .expect("scrubbed");
        assert_eq!(scrubbed.program, "/usr/bin/env");
        assert_eq!(
            scrubbed.args,
            ["-u", "GIT_DIR", "-u", "GH_TOKEN", "--", "/bin/bash", "-l"]
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
            Some(OsStr::new("/bin/bash")),
            None,
            &["GIT_DIR"],
        );
        assert_windows_shell(windows);
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

    #[test]
    fn shell_argv_rejects_a_relative_shell() {
        for relative in ["./sh", "bash", "bin/zsh", "../sh"] {
            let argv =
                shell_argv(HostOs::Linux, Some(OsStr::new(relative)), None, &[]).expect("relative");
            assert_eq!(argv.program, "/bin/sh", "{relative}");
            assert!(argv.args.iter().all(|arg| arg != relative), "{relative}");

            let wrapped = shell_argv(
                HostOs::Linux,
                Some(OsStr::new(relative)),
                None,
                &["GIT_DIR"],
            )
            .expect("wrapped");
            assert_eq!(wrapped.program, "/usr/bin/env", "{relative}");
            let rendered = format!("{} {}", wrapped.program, wrapped.args.join(" "));
            assert!(
                !rendered.contains(relative),
                "{relative} leaked into {rendered}"
            );
            assert_eq!(
                wrapped.args,
                ["-u", "GIT_DIR", "--", "/bin/sh"],
                "{relative}"
            );
        }

        let bash =
            shell_argv(HostOs::Linux, Some(OsStr::new("/bin/bash")), None, &[]).expect("bash");
        assert_eq!(bash.program, "/bin/bash");
        let wrapped_bash = shell_argv(
            HostOs::Linux,
            Some(OsStr::new("/bin/bash")),
            None,
            &["GIT_DIR"],
        )
        .expect("wrapped bash");
        assert_eq!(wrapped_bash.program, "/usr/bin/env");
        assert_eq!(wrapped_bash.args, ["-u", "GIT_DIR", "--", "/bin/bash"]);

        let passwd = shell_argv(
            HostOs::Linux,
            Some(OsStr::new("./sh")),
            Some("/bin/bash"),
            &[],
        )
        .expect("passwd");
        assert_eq!(passwd.program, "/bin/bash");

        let relative_passwd = shell_argv(
            HostOs::Linux,
            Some(OsStr::new("bash")),
            Some("bin/zsh"),
            &[],
        )
        .expect("relative passwd");
        assert_eq!(relative_passwd.program, "/bin/sh");

        let windows = shell_argv(
            HostOs::Windows,
            Some(OsStr::new("./sh")),
            None,
            &["GIT_DIR"],
        );
        assert_windows_shell(windows);
    }

    fn assert_windows_shell(argv: Result<ShellArgv, String>) {
        match argv {
            Ok(argv) => {
                assert!(
                    windows_path_is_absolute(&argv.program),
                    "relative program {}",
                    argv.program
                );
                assert_eq!(argv.args, ["-NoLogo"]);
                assert_ne!(argv.program, "powershell.exe");
            }
            Err(error) => assert!(error.contains("PowerShell"), "{error}"),
        }
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

    #[cfg(unix)]
    #[test]
    fn relative_shell_does_not_execute_a_planted_binary() {
        use std::os::unix::fs::PermissionsExt;

        let shell_before = std::env::var_os("SHELL");
        let dir = tempfile::TempDir::new().expect("temp dir");
        let marker = dir.path().join("marker");
        let planted = dir.path().join("sh");
        let script = format!("#!/bin/sh\ntouch '{}'\n", marker.display());
        std::fs::write(&planted, script).expect("write planted shell");
        std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o755))
            .expect("chmod planted shell");

        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "ui::terminal::tests::child_relative_shell_does_not_execute_a_planted_binary",
                "--include-ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("SHELL", "./sh")
            .env("CTHULHU_TERMINAL_RELATIVE_SHELL_DIR", dir.path())
            .output()
            .expect("spawn child test");

        assert_eq!(std::env::var_os("SHELL"), shell_before);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "child failed:\n{stdout}\n{stderr}");
        assert!(
            stdout.contains("1 passed"),
            "child did not run:\n{stdout}\n{stderr}"
        );
    }

    /// Re-exec target for [`relative_shell_does_not_execute_a_planted_binary`].
    /// `SHELL=./sh` is set on this process only.
    #[cfg(unix)]
    #[test]
    #[ignore = "helper run by relative_shell_does_not_execute_a_planted_binary"]
    fn child_relative_shell_does_not_execute_a_planted_binary() {
        let Some(dir) = std::env::var_os("CTHULHU_TERMINAL_RELATIVE_SHELL_DIR") else {
            return;
        };
        let dir = PathBuf::from(dir);
        let marker = dir.join("marker");
        let ctx = egui::Context::default();
        let metrics = test_metrics();
        let session = match ShellSession::spawn(&dir, &ctx, &metrics) {
            Ok(session) => session,
            Err(error) => panic!("{error}"),
        };
        thread::sleep(Duration::from_millis(400));
        assert!(
            !marker.exists(),
            "relative SHELL executed the planted binary"
        );
        drop(session);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn dropped_shell_kills_a_job_that_left_the_session() {
        let shell_before = std::env::var_os("SHELL");
        let dir = tempfile::TempDir::new().expect("temp dir");
        let control = dir.path().join("control");
        let escaped = dir.path().join("escaped");
        let ctx = egui::Context::default();
        let metrics = test_metrics();
        let session = match ShellSession::spawn(dir.path(), &ctx, &metrics) {
            Ok(session) => session,
            Err(error) => panic!("{error}"),
        };
        assert_eq!(std::env::var_os("SHELL"), shell_before);

        wait_for_shell_output(&session);
        // Fork before setsid. An interactive shell makes a background job the
        // process-group leader, and setsid then returns EPERM. The parent
        // perl waits so both sleeps stay descendants of the session leader.
        let program = LEAVE_SESSION
            .replace("@@CONTROL@@", &control.display().to_string())
            .replace("@@ESCAPED@@", &escaped.display().to_string());
        session.write(program.as_bytes());

        let control_pid = wait_for_shell_pid(&control, &session);
        let escaped_pid = wait_for_shell_pid(&escaped, &session);
        assert!(
            process_is_running(control_pid),
            "control sleep was not alive"
        );
        assert!(
            process_is_running(escaped_pid),
            "setsid sleep was not alive"
        );
        assert_eq!(
            session_of(control_pid),
            session_of(session.pid),
            "control sleep left the shell session"
        );
        assert_ne!(
            session_of(escaped_pid),
            session_of(session.pid),
            "setsid did not leave the shell session"
        );
        assert!(
            is_descendant(escaped_pid, session.pid),
            "setsid sleep is not a descendant of the shell"
        );
        drop(session);

        let started = std::time::Instant::now();
        while started.elapsed() < Duration::from_secs(2)
            && (process_is_running(control_pid) || process_is_running(escaped_pid))
        {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !process_is_running(escaped_pid),
            "setsid sleep survived dropping the shell"
        );
        assert!(
            !process_is_running(control_pid),
            "same-session sleep survived dropping the shell"
        );
    }

    #[cfg(unix)]
    fn test_metrics() -> Metrics {
        Metrics {
            cols: 80,
            rows: 24,
            cell_w: 8.0,
            cell_h: 16.0,
            font: egui::FontId::monospace(14.0),
        }
    }

    #[cfg(target_os = "linux")]
    const LEAVE_SESSION: &str = r#"/usr/bin/perl <<'END'
use POSIX;
my $control_path = "@@CONTROL@@";
my $escaped_path = "@@ESCAPED@@";
my $control = fork();
die "fork control: $!" unless defined $control;
if ($control == 0) {
    open my $fh, ">", $control_path or die $!;
    print $fh $$;
    close $fh;
    exec "/bin/sleep", "30" or die $!;
}
my $escaped = fork();
die "fork escaped: $!" unless defined $escaped;
if ($escaped == 0) {
    defined POSIX::setsid() or die "setsid: $!";
    open my $fh, ">", $escaped_path or die $!;
    print $fh $$;
    close $fh;
    exec "/bin/sleep", "30" or die $!;
}
wait();
wait();
END
"#;

    #[cfg(target_os = "linux")]
    fn screen_text(session: &ShellSession) -> String {
        let term = session.term.lock();
        let mut text = String::new();
        for index in 0..24i32 {
            for column in 0..80 {
                text.push(term.grid()[Line(index)][Column(column)].c);
            }
            text.push('\n');
        }
        text
    }

    #[cfg(target_os = "linux")]
    fn wait_for_shell_output(session: &ShellSession) {
        let started = std::time::Instant::now();
        while started.elapsed() < Duration::from_secs(2) {
            if screen_text(session).chars().any(|ch| !ch.is_whitespace()) {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[cfg(target_os = "linux")]
    fn wait_for_shell_pid(path: &Path, session: &ShellSession) -> u32 {
        let started = std::time::Instant::now();
        loop {
            if let Ok(text) = std::fs::read_to_string(path)
                && let Ok(pid) = text.trim().parse::<u32>()
                && pid > 1
            {
                return pid;
            }
            if started.elapsed() > Duration::from_secs(2) {
                panic!(
                    "pid file {} was not written\n{}",
                    path.display(),
                    screen_text(session)
                );
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(target_os = "linux")]
    fn parent_pid(pid: u32) -> Option<u32> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        let line = text.lines().find(|line| line.starts_with("PPid:"))?;
        line["PPid:".len()..].trim().parse().ok()
    }

    #[cfg(target_os = "linux")]
    fn is_descendant(pid: u32, ancestor: u32) -> bool {
        let mut current = pid;
        for _ in 0..16 {
            if current == ancestor {
                return true;
            }
            let Some(parent) = parent_pid(current) else {
                return false;
            };
            if parent <= 1 {
                return false;
            }
            current = parent;
        }
        false
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

    fn grid_metrics(cols: usize, rows: usize, cell_w: f32, cell_h: f32) -> Metrics {
        Metrics {
            cols,
            rows,
            cell_w,
            cell_h,
            font: FontId::monospace(14.0),
        }
    }

    #[test]
    fn window_size_never_reports_a_zero_grid() {
        let metrics = grid_metrics(80, 24, 8.0, 16.0);
        for scale in [0.0_f32, -1.0, 2.0, f32::NAN, f32::INFINITY] {
            let size = window_size(&metrics, scale);
            assert!((1..=u16::MAX).contains(&size.num_cols), "cols at {scale}");
            assert!((1..=u16::MAX).contains(&size.num_lines), "rows at {scale}");
            assert!(pty_size_is_safe(size), "unsafe size at {scale}: {size:?}");
        }

        let huge = grid_metrics(1_000, 400, 1_000.0, 1_000.0);
        let size = window_size(&huge, 10.0);
        assert!(pty_size_is_safe(size), "{size:?}");
        assert!(size.cell_width < 1_000, "cell width was not clamped");
    }

    #[test]
    fn windows_shell_prefers_pwsh_then_windows_powershell() {
        let pwsh = r"C:\Program Files\PowerShell\7\pwsh.exe";
        let powershell = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
        let both = windows_shell_argv(
            Some(r"C:\Program Files"),
            Some(r"C:\Windows"),
            Some(r".\powershell.exe"),
            |path| path == pwsh || path == powershell,
        )
        .expect("pwsh");
        assert_eq!(both.program, pwsh);
        assert_eq!(both.args, ["-NoLogo"]);

        let second = windows_shell_argv(
            Some(r"C:\Program Files"),
            Some(r"C:\Windows"),
            Some(r"C:\Windows\System32\cmd.exe"),
            |path| path == powershell,
        )
        .expect("powershell");
        assert_eq!(second.program, powershell);
        assert_eq!(second.args, ["-NoLogo"]);

        let relative = windows_shell_argv(
            Some(r"Program Files"),
            Some("Windows"),
            Some("powershell.exe"),
            |_| true,
        );
        assert!(relative.is_err(), "{relative:?}");

        let missing =
            windows_shell_argv(Some(r"C:\Program Files"), Some(r"C:\Windows"), None, |_| {
                false
            });
        assert!(missing.is_err(), "{missing:?}");
    }

    #[test]
    fn windows_taskkill_requires_an_absolute_system_root() {
        let present = |path: &str| path == r"C:\Windows\System32\taskkill.exe";
        assert_eq!(
            windows_taskkill_path(Some(r"C:\Windows"), present).expect("taskkill"),
            r"C:\Windows\System32\taskkill.exe"
        );
        assert!(windows_taskkill_path(Some("Windows"), present).is_err());
        assert!(windows_taskkill_path(None, present).is_err());
        assert!(windows_taskkill_path(Some(r"C:\Windows"), |_| false).is_err());
    }

    #[test]
    fn dock_launch_shell_gets_utf8_locale() {
        let env = shell_env_from(false, None, None, None);
        assert_eq!(env.get("LANG").map(String::as_str), Some("en_US.UTF-8"));

        let kept = shell_env_from(false, Some("fr_FR.UTF-8"), None, None);
        assert_eq!(kept.get("LANG"), None);
        let lc_all = shell_env_from(false, None, Some("C"), None);
        assert_eq!(lc_all.get("LANG"), None);
        let lc_ctype = shell_env_from(false, None, None, Some("en_US.UTF-8"));
        assert_eq!(lc_ctype.get("LANG"), None);
    }

    #[cfg(unix)]
    #[test]
    fn non_executable_absolute_shell_falls_through() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().expect("temp dir");
        let locked = dir.path().join("locked-shell");
        let next = dir.path().join("next-shell");
        std::fs::write(&locked, "#!/bin/sh\n").expect("write locked");
        std::fs::write(&next, "#!/bin/sh\n").expect("write next");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644))
            .expect("mode 0644");
        std::fs::set_permissions(&next, std::fs::Permissions::from_mode(0o755)).expect("mode 0755");
        let locked = locked.to_str().expect("utf-8 locked path");
        let next = next.to_str().expect("utf-8 next path");

        let argv =
            shell_argv(HostOs::Linux, Some(OsStr::new(locked)), Some(next), &[]).expect("argv");
        assert_eq!(argv.program, next);
        assert_ne!(argv.program, locked);

        let macos = chosen_unix_shell_with(
            HostOs::Macos,
            Some(OsStr::new(locked)),
            Some(locked),
            |path| path == "/bin/zsh" || path == "/bin/sh",
        );
        assert_eq!(macos, "/bin/zsh");

        let linux = chosen_unix_shell_with(
            HostOs::Linux,
            Some(OsStr::new("/no/such/shell")),
            None,
            |path| path == "/bin/sh",
        );
        assert_eq!(linux, "/bin/sh");
    }

    #[test]
    fn option_text_is_the_composed_character() {
        let bytes = text_input_bytes(
            "å",
            Modifiers {
                alt: true,
                ..Modifiers::NONE
            },
        )
        .expect("composed text");
        assert_eq!(bytes, "å".as_bytes());
        assert!(!bytes.contains(&0x1b));
        assert!(!bytes.windows(2).any(|pair| pair == [0x1b, b'a']));
    }

    #[test]
    fn paste_bytes_rejects_bracketed_breakout() {
        let bytes = paste_bytes("\x1b[201~touch pwned\r", true);
        let open = b"\x1b[200~";
        let close = b"\x1b[201~";
        assert!(bytes.starts_with(open), "{bytes:?}");
        assert!(bytes.ends_with(close), "{bytes:?}");
        let payload = &bytes[open.len()..bytes.len() - close.len()];
        assert!(!payload.contains(&0x1b), "{payload:?}");
        let outside = [&bytes[..open.len()], &bytes[bytes.len() - close.len()..]];
        for region in outside {
            assert!(
                !region
                    .windows(b"touch pwned".len())
                    .any(|window| window == b"touch pwned"),
                "{region:?}"
            );
        }
        // The command text stays inside the paste, where the terminal will not execute it.
        assert!(
            payload
                .windows(b"touch pwned".len())
                .any(|window| window == b"touch pwned")
        );
        assert!(payload.ends_with(b"\r"));

        let kept = paste_bytes("a\tb\nc", false);
        assert!(kept.contains(&b'\t'));
        assert!(kept.contains(&b'\r'));
        assert!(!kept.contains(&b'\n'));
        assert!(!kept.contains(&0x1b));
    }

    #[test]
    fn osc52_store_is_disabled() {
        use std::sync::Mutex;

        struct Clipboard(Arc<Mutex<Vec<String>>>);

        impl EventListener for Clipboard {
            fn send_event(&self, event: TermEvent) {
                if let TermEvent::ClipboardStore(_, text) = event {
                    self.0.lock().expect("clipboard").push(text);
                }
            }
        }

        let copies = Arc::new(Mutex::new(Vec::new()));
        let grid = GridSize {
            columns: 8,
            screen_lines: 2,
        };
        let mut term = Term::new(term_config(), &grid, Clipboard(Arc::clone(&copies)));
        let mut parser = alacritty_terminal::vte::ansi::Processor::<
            alacritty_terminal::vte::ansi::StdSyncHandler,
        >::new();
        // "hi" is aGk= in standard base64.
        parser.advance(&mut term, b"\x1b]52;c;aGk=\x07");
        assert!(copies.lock().expect("clipboard").is_empty());
        assert_eq!(term_config().osc52, Osc52::Disabled);

        let accepted = Arc::new(Mutex::new(Vec::new()));
        let mut copying = Term::new(
            TermConfig {
                osc52: Osc52::OnlyCopy,
                ..term_config()
            },
            &grid,
            Clipboard(Arc::clone(&accepted)),
        );
        let mut parser = alacritty_terminal::vte::ansi::Processor::<
            alacritty_terminal::vte::ansi::StdSyncHandler,
        >::new();
        parser.advance(&mut copying, b"\x1b]52;c;aGk=\x07");
        assert_eq!(accepted.lock().expect("clipboard").as_slice(), ["hi"]);
    }

    #[test]
    fn retina_cell_pixels_are_rounded_points() {
        let metrics = grid_metrics(80, 24, 8.4, 16.0);
        let one = window_size(&metrics, 1.0);
        let two = window_size(&metrics, 2.0);
        assert_eq!(two.cell_width, 17);
        assert_eq!(two.num_cols, one.num_cols);
        assert_ne!(two.cell_width, one.cell_width);
        assert!(!same_window_size(one, two));
    }

    #[cfg(unix)]
    #[test]
    fn resize_after_shell_exit_does_not_kill_the_app() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let ctx = egui::Context::default();
        let metrics = test_metrics();
        let mut session = match ShellSession::spawn(dir.path(), &ctx, &metrics) {
            Ok(session) => session,
            Err(error) => panic!("{error}"),
        };
        session.write(b"exit\n");
        let started = std::time::Instant::now();
        let mut saw_exit = false;
        while started.elapsed() < Duration::from_secs(3) {
            if session.poll(&ctx) || session.child_exited || !unix_child_is_alive(session.pid) {
                saw_exit = true;
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(saw_exit, "shell did not exit");
        let pid = std::process::id();
        let mut bigger = metrics.clone();
        bigger.cols = 100;
        bigger.rows = 40;
        session.resize(&bigger, 2.0);
        session.resize(&bigger, 1.0);
        assert_eq!(std::process::id(), pid);
        drop(session);
    }

    fn prompt_on_screen(columns: usize, screen_lines: usize, bytes: &[u8]) -> Range<usize> {
        let grid = GridSize {
            columns,
            screen_lines,
        };
        let mut term = Term::new(
            term_config(),
            &grid,
            alacritty_terminal::event::VoidListener,
        );
        let mut parser = alacritty_terminal::vte::ansi::Processor::<
            alacritty_terminal::vte::ansi::StdSyncHandler,
        >::new();
        parser.advance(&mut term, bytes);
        prompt_span(&term)
    }

    #[test]
    fn prompt_span_keeps_a_one_line_prompt() {
        assert_eq!(prompt_on_screen(40, 16, b"$ "), 0..1);
    }

    #[test]
    fn prompt_span_skips_the_blank_line_above_a_two_line_prompt() {
        let bytes = b"\r\n~/repo on main\r\n> ";
        assert_eq!(prompt_on_screen(40, 16, bytes), 1..3);
    }

    #[test]
    fn prompt_span_counts_a_colored_space() {
        let bytes = b"\x1b[41m \x1b[0m\r\n>";
        assert_eq!(prompt_on_screen(40, 16, bytes), 0..2);
    }

    #[test]
    fn prompt_span_counts_an_inverse_space() {
        let bytes = b"\x1b[7m \x1b[0m\r\n>";
        assert_eq!(prompt_on_screen(40, 16, bytes), 0..2);
    }

    #[test]
    fn prompt_span_keeps_the_rows_nearest_the_cursor() {
        let mut bytes = Vec::new();
        for row in 0..10 {
            if row > 0 {
                bytes.extend_from_slice(b"\r\n");
            }
            bytes.push(b'x');
        }
        assert_eq!(prompt_on_screen(8, 16, &bytes), 2..10);
    }

    #[test]
    fn prompt_span_stops_at_a_blank_line_above_the_prompt() {
        let bytes = b"hello\r\n\r\n$ ";
        assert_eq!(prompt_on_screen(40, 16, bytes), 2..3);
    }
}
