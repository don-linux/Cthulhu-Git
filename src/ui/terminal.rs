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
#[cfg(unix)]
use std::ffi::{OsStr, OsString};
use std::io;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};
#[cfg(unix)]
use std::time::Duration;

use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg, State};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{
    Config as TermConfig, Osc52, RenderableCursor, Term, TermMode, color, point_to_viewport,
    viewport_to_point,
};
use alacritty_terminal::tty::{self, Pty};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};
use eframe::egui::{
    self, Align2, Color32, CursorIcon, Event, EventFilter, FocusDirection, FontId, Id, Key,
    Modifiers, Painter, PointerButton, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2,
};

use super::theme::Palette;

const DETAIL_MIN_HEIGHT: f32 = 120.0;
const TERMINAL_MIN_HEIGHT: f32 = 80.0;
const SPLITTER_HEIGHT: f32 = 6.0;
const INITIAL_FRACTION: f32 = 0.40;

/// Program and arguments for the PTY child.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ShellArgv {
    program: String,
    args: Vec<String>,
}

/// The shell for the PTY, from this process's environment.
#[cfg(unix)]
fn shell() -> Result<ShellArgv, String> {
    let unset = vars_to_unset(std::env::vars_os());
    let unset: Vec<&str> = unset.iter().map(String::as_str).collect();
    Ok(shell_argv(
        std::env::var_os("SHELL").as_deref(),
        passwd_shell().as_deref(),
        &unset,
    ))
}

/// PowerShell, quoted for ConPTY. ConPTY merges `Options.env` into the
/// parent environment, so no inherited name can be removed here.
#[cfg(windows)]
fn shell() -> Result<ShellArgv, String> {
    let argv = windows_shell_argv(
        std::env::var("ProgramFiles").ok().as_deref(),
        std::env::var("SystemRoot").ok().as_deref(),
        |path| Path::new(path).is_file(),
    )?;
    Ok(ShellArgv {
        program: conpty_program(&argv.program),
        args: argv.args,
    })
}

/// Argv for the embedded shell.
///
/// macOS is a login shell (`-l`) so `/etc/zprofile` and `~/.zprofile` run.
/// Linux stays non-login, so an interactive bash still reads `~/.bashrc`.
/// A relative, empty, non-Unicode, missing, or non-executable `SHELL` uses
/// an absolute executable `passwd_shell`. macOS then tries `/bin/zsh`.
/// `/bin/sh` is the last resort.
/// `unset` is applied with `/usr/bin/env -u`, which can remove names.
/// `Options.env` can only add them.
#[cfg(unix)]
fn shell_argv(shell: Option<&OsStr>, passwd_shell: Option<&str>, unset: &[&str]) -> ShellArgv {
    let program = [
        shell.and_then(OsStr::to_str),
        passwd_shell,
        cfg!(target_os = "macos").then_some("/bin/zsh"),
    ]
    .into_iter()
    .flatten()
    .find(|candidate| Path::new(candidate).is_absolute() && is_executable(candidate))
    .unwrap_or("/bin/sh")
    .to_owned();
    let login = cfg!(target_os = "macos").then(|| "-l".to_owned());
    let mut args: Vec<String> = unset
        .iter()
        .filter(|name| env_name_is_safe(name))
        .flat_map(|name| ["-u".to_owned(), (*name).to_owned()])
        .collect();
    if args.is_empty() {
        return ShellArgv {
            program,
            args: login.into_iter().collect(),
        };
    }
    args.push("--".to_owned());
    args.push(program);
    args.extend(login);
    ShellArgv {
        program: "/usr/bin/env".to_owned(),
        args,
    }
}

/// A file with an execute bit. A missing path or a mode without one falls
/// through, so the PTY is not pointed at a file `execve` will reject.
#[cfg(unix)]
fn is_executable(path: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// PowerShell, as a pure function of the Windows environment.
///
/// `Command` searches the working directory before `PATH`, so a bare
/// `powershell.exe` would run a binary planted in the repository. Order is
/// `%ProgramFiles%\PowerShell\7\pwsh.exe`, then
/// `%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe`.
/// Relative paths are skipped. `COMSPEC` is not a candidate: a relative
/// value would search the repository, and an absolute one is often `cmd.exe`.
#[cfg(any(windows, test))]
fn windows_shell_argv(
    program_files: Option<&str>,
    system_root: Option<&str>,
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

/// alacritty builds the ConPTY command line by pasting the program in
/// unquoted, and `CreateProcessW` then splits `C:\Program Files\...` at the
/// space and tries `C:\Program.exe` first. Windows paths cannot contain `"`.
#[cfg(any(windows, test))]
fn conpty_program(path: &str) -> String {
    format!("\"{path}\"")
}

#[cfg(any(windows, test))]
fn windows_join(base: &str, child: &str) -> String {
    let base = base.trim_end_matches(['\\', '/']);
    format!("{base}\\{child}")
}

/// Drive path (`C:\...`) or UNC (`\\server\share`). `Path::is_absolute` is
/// false for both when this code is tested on Unix.
#[cfg(any(windows, test))]
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

#[cfg(unix)]
fn env_name_is_safe(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// Inherited names the shell must not see: what git commands scrub, plus the
/// GitHub token variables. A name that is not Unicode is skipped, as the git
/// scrub does; a value that is not Unicode does not matter.
#[cfg(unix)]
fn vars_to_unset(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Vec<String> {
    let mut names: Vec<String> = vars
        .into_iter()
        .filter_map(|(name, _)| name.into_string().ok())
        .filter(|name| {
            cthulhu_git::git::inherited_git_var_is_cleared(name)
                || ["GH_TOKEN", "GITHUB_TOKEN"]
                    .iter()
                    .any(|token| cthulhu_git::git::env_names_match(token, name))
        })
        .collect();
    names.sort();
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
/// `TERM` is `xterm-256color`, whose terminfo every system ships. `clear` and
/// the shell binding for Ctrl+L (`clear-screen`) write its `clear`
/// capability; a parent `TERM` of `dumb`, or an empty one, writes nothing.
/// Alacritty's `setup_env` also sets `TERM` and `COLORTERM`, but on the whole
/// process; `tty::Options.env` sets them for the child only.
///
/// A Dock launch leaves `LANG`, `LC_ALL`, and `LC_CTYPE` unset, and the shell
/// then treats the stream as ASCII. An existing locale is left alone so the
/// child inherits it.
fn shell_env(
    lang: Option<&str>,
    lc_all: Option<&str>,
    lc_ctype: Option<&str>,
) -> HashMap<String, String> {
    let mut env = HashMap::from([
        ("TERM".to_owned(), "xterm-256color".to_owned()),
        ("COLORTERM".to_owned(), "truecolor".to_owned()),
    ]);
    let blank = |value: Option<&str>| value.is_none_or(str::is_empty);
    if blank(lang) && blank(lc_all) && blank(lc_ctype) {
        env.insert("LANG".to_owned(), "en_US.UTF-8".to_owned());
    }
    env
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
        let argv = shell()?;
        let options = tty::Options {
            shell: Some(tty::Shell::new(argv.program, argv.args)),
            working_directory: Some(cwd.to_path_buf()),
            env: shell_env(
                std::env::var("LANG").ok().as_deref(),
                std::env::var("LC_ALL").ok().as_deref(),
                std::env::var("LC_CTYPE").ok().as_deref(),
            ),
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
                    let palette = super::theme::current(ctx).palette;
                    let rgb = reported_color(self.term.lock().colors(), index, &palette);
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

    fn write(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let _ = self.io.send(Msg::Input(Cow::Owned(bytes.to_vec())));
    }

    fn resize(&mut self, metrics: &Metrics, pixels_per_point: f32) {
        let size = window_size(metrics, pixels_per_point);
        if !should_resize(self.size, size, self.child_exited, || {
            child_can_be_resized(self.pid)
        }) {
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

/// Rounded pixels, at least one. `as u16` would turn NaN into 0, and a zero
/// cell size makes `TIOCSWINSZ` fail and alacritty exit the process.
fn px(points: f32) -> u16 {
    if points.is_finite() {
        points.round().clamp(1.0, f32::from(u16::MAX)) as u16
    } else {
        1
    }
}

/// `cols * cell_px` is stored in a `u16` by alacritty's `Winsize`. Clamp the
/// product so it cannot wrap.
fn clamp_cell_px(cells: u16, cell_px: u16) -> u16 {
    let cells = u32::from(cells.max(1));
    let max_cell = u16::try_from(u32::from(u16::MAX) / cells).unwrap_or(1);
    cell_px.min(max_cell.max(1)).max(1)
}

/// Whether the PTY should take `size`. `Msg::Resize` runs `TIOCSWINSZ` on
/// the event-loop thread, and alacritty exits the process when that ioctl
/// fails: EINVAL for a 0-size, which [`window_size`] never reports, and EIO
/// after the slave hangs up, so an exited or dead child is left alone. Cell
/// pixels count, so a move between 1x and 2x still updates `ws_xpixel`. The
/// size is compared first: it is the same on almost every frame, and `alive`
/// reads `/proc`.
fn should_resize(
    current: WindowSize,
    size: WindowSize,
    child_exited: bool,
    alive: impl FnOnce() -> bool,
) -> bool {
    let same = current.num_cols == size.num_cols
        && current.num_lines == size.num_lines
        && current.cell_width == size.cell_width
        && current.cell_height == size.cell_height;
    !same && !child_exited && alive()
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
    let content = term.renderable_content();
    let offset = content.display_offset;
    // prompt_rows keeps the row inside the screen.
    let cursor_row = point_to_viewport(offset, content.cursor.point).map_or(0, |point| point.line);
    let mut filled = vec![false; term.screen_lines()];
    for indexed in content.display_iter {
        if indexed
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        if let Some(point) = point_to_viewport(offset, indexed.point)
            && cell_holds_prompt(indexed.cell)
            && let Some(row) = filled.get_mut(point.line)
        {
            *row = true;
        }
    }
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

/// Paints the strip and, the first time, starts the shell. The strip takes
/// the pointer and, once clicked, the keyboard.
pub fn show(
    ui: &mut Ui,
    terminal: &mut Option<Terminal>,
    cwd: &Path,
    view_id: Id,
    palette: &Palette,
) {
    let visible = ui.available_rect_before_wrap();
    let metrics = metrics(ui, visible);
    let painter = start(ui, terminal, cwd, visible, &metrics, palette);
    match terminal {
        Some(Terminal::Exited { message }) => {
            if exited_prompt(ui, visible, message, palette) {
                *terminal = Some(begin_shell(ui.ctx(), cwd, &metrics));
            }
        }
        Some(Terminal::Live(session)) => {
            // Before `interact`: egui moves focus when the focused widget
            // registers, and Tab itself belongs to the shell.
            if ui.memory(|memory| memory.has_focus(focus_id(view_id)))
                && let Some(direction) = leave_direction(ui)
            {
                ui.memory_mut(|memory| memory.move_focus(direction));
            }
            let response = ui.interact(visible, focus_id(view_id), Sense::click_and_drag());
            let response = response.on_hover_cursor(CursorIcon::Text);
            if response.hovered() && ui.input(|input| input.pointer.primary_pressed()) {
                response.request_focus();
            }
            session.resize(&metrics, ui.pixels_per_point());
            let focused = response.has_focus();
            handle_pointer(ui, session, &response, visible, &metrics);
            if focused {
                // egui moves focus on Tab and the arrows and drops it on
                // Escape. In a terminal those keys belong to the shell.
                let shell_keys = EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                };
                ui.memory_mut(|memory| memory.set_focus_lock_filter(response.id, shell_keys));
                handle_keys(ui, session);
            }
            paint(&painter, session, visible.min, &metrics, palette);
        }
        Some(Terminal::Starting(_)) | None => {}
    }
}

/// The settings preview: the same shell in the current face, drawn as the
/// prompt block that ends at the cursor. Pointer and keyboard are ignored.
/// The PTY stays [`PREVIEW_SCREEN_ROWS`] tall so that block is not scrolled
/// away.
pub fn show_preview(ui: &mut Ui, terminal: &mut Option<Terminal>, cwd: &Path, palette: &Palette) {
    let visible = ui.available_rect_before_wrap();
    let metrics = Metrics {
        rows: PREVIEW_SCREEN_ROWS,
        ..metrics(ui, visible)
    };
    let painter = start(ui, terminal, cwd, visible, &metrics, palette);
    match terminal {
        Some(Terminal::Exited { message }) => {
            let font = egui::TextStyle::Body.resolve(ui.style());
            ui.painter().text(
                visible.left_top(),
                Align2::LEFT_TOP,
                message,
                font,
                palette.text_muted,
            );
        }
        Some(Terminal::Live(session)) => {
            session.resize(&metrics, ui.pixels_per_point());
            let span = {
                let term = session.term.lock();
                prompt_span(&term)
            };
            let (origin, resized) = place_prompt(visible, &span, metrics.cell_h);
            if resized {
                ui.ctx().request_repaint();
            }
            paint(&painter, session, origin, &metrics, palette);
        }
        Some(Terminal::Starting(_)) | None => {}
    }
}

/// Where the screen is drawn so the prompt block `span` starts at the top of
/// `visible`, and whether `visible` is more than half a row off the block's
/// height. A column change reflows the prompt after the settings page sized
/// the box, so one more frame is needed.
fn place_prompt(visible: Rect, span: &Range<usize>, cell_h: f32) -> (Pos2, bool) {
    let origin = visible.min - Vec2::new(0.0, span.start as f32 * cell_h);
    let resized = (visible.height() / cell_h - span.len() as f32).abs() > 0.5;
    (origin, resized)
}

/// Fills the strip, starts the shell the first time, and says so until it
/// runs.
fn start(
    ui: &Ui,
    terminal: &mut Option<Terminal>,
    cwd: &Path,
    visible: Rect,
    metrics: &Metrics,
    palette: &Palette,
) -> Painter {
    let painter = ui.painter().with_clip_rect(visible);
    painter.rect_filled(visible, 0.0, palette.background);
    adopt_starting(terminal);
    if terminal.is_none() {
        *terminal = Some(begin_shell(ui.ctx(), cwd, metrics));
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
    painter
}

/// Ctrl+Tab and Ctrl+Shift+Tab move focus out of the terminal, as they do
/// out of GTK and Qt text views, which keep Tab for themselves.
fn leave_direction(ui: &mut Ui) -> Option<FocusDirection> {
    ui.input_mut(|input| {
        if input.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::Tab) {
            Some(FocusDirection::Previous)
        } else if input.consume_key(Modifiers::CTRL, Key::Tab) {
            Some(FocusDirection::Next)
        } else {
            None
        }
    })
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

/// The exit message and a button to start again. `true` on a click.
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

/// The screen, its top-left cell at `origin`.
fn paint(
    painter: &Painter,
    session: &ShellSession,
    origin: Pos2,
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
        // The grid has the strip's size, and the painter clips to the strip.
        let Some(Point {
            line: row,
            column: Column(col),
        }) = point_to_viewport(offset, indexed.point)
        else {
            continue;
        };
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
        let cell = cell_rect(origin, metrics, col, row, wide);
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

fn cell_rect(origin: Pos2, metrics: &Metrics, col: usize, row: usize, wide: bool) -> Rect {
    let span = if wide { 2.0 } else { 1.0 };
    let min = Pos2::new(
        origin.x + col as f32 * metrics.cell_w,
        origin.y + row as f32 * metrics.cell_h,
    );
    Rect::from_min_size(min, Vec2::new(metrics.cell_w * span, metrics.cell_h))
}

/// What `color` paints with: the color the program set, else this widget's.
/// Text, bright text and the cursor are the palette's text; the ANSI colors
/// and the 256-color cube are [`ansi_color`], the dim ones at normal strength.
fn resolve_color(color: Color, colors: &color::Colors, palette: &Palette) -> Color32 {
    let index = match color {
        Color::Spec(rgb) => return Color32::from_rgb(rgb.r, rgb.g, rgb.b),
        Color::Indexed(index) => usize::from(index),
        Color::Named(named) => named as usize,
    };
    if let Some(rgb) = colors[index] {
        return Color32::from_rgb(rgb.r, rgb.g, rgb.b);
    }
    let dim = NamedColor::DimBlack as usize;
    match color {
        Color::Named(NamedColor::Background) => palette.background,
        Color::Named(NamedColor::DimForeground) => palette.text_muted,
        // Past the 256 colors: read as a color number, the cursor would be
        // green and the bright text yellow.
        Color::Named(
            NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::Cursor,
        ) => palette.text,
        Color::Named(_) if index >= dim => ansi_color((index - dim) as u8),
        // A color number: below 256.
        _ => ansi_color(index as u8),
    }
}

/// The answer to a color query (OSC 4, 10, 11, 12): the color the program set,
/// or else the one this widget paints.
fn reported_color(colors: &color::Colors, index: usize, palette: &Palette) -> Rgb {
    let color = match u8::try_from(index) {
        Ok(ansi) => Color::Indexed(ansi),
        Err(_) if index == NamedColor::Background as usize => Color::Named(NamedColor::Background),
        Err(_) if index == NamedColor::Cursor as usize => Color::Named(NamedColor::Cursor),
        Err(_) => Color::Named(NamedColor::Foreground),
    };
    let painted = resolve_color(color, colors, palette);
    Rgb {
        r: painted.r(),
        g: painted.g(),
        b: painted.b(),
    }
}

/// The sixteen ANSI colors, the 6×6×6 cube and the gray ramp, as xterm
/// numbers them.
fn ansi_color(index: u8) -> Color32 {
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
        let [r, g, b] = ANSI[usize::from(index)];
        return Color32::from_rgb(r, g, b);
    }
    if index < 232 {
        let cube = index - 16;
        let level = |step: u8| {
            if step == 0 { 0 } else { 55 + step * 40 }
        };
        return Color32::from_rgb(level(cube / 36), level((cube / 6) % 6), level(cube % 6));
    }
    let gray = 8 + 10 * (index - 232);
    Color32::from_gray(gray)
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
    let (mouse_mode, offset) = {
        let term = session.term.lock();
        let mouse_mode = term.mode().intersects(TermMode::MOUSE_MODE);
        (mouse_mode, term.grid().display_offset())
    };
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
    let hit = hit_at(rect, pos, metrics, offset);

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

/// The cell under `pos`, clamped to the screen, and its grid point while the
/// view is scrolled `offset` lines back.
fn hit_at(rect: Rect, pos: Pos2, metrics: &Metrics, offset: usize) -> CellHit {
    let col = ((pos.x - rect.min.x) / metrics.cell_w).floor();
    let row = ((pos.y - rect.min.y) / metrics.cell_h).floor();
    let col = col.clamp(0.0, (metrics.cols - 1) as f32) as usize;
    let row = row.clamp(0.0, (metrics.rows - 1) as f32) as usize;
    CellHit {
        point: viewport_to_point(offset, Point::new(row, Column(col))),
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
            pressed: true,
            modifiers,
            ..
        } => {
            let app_cursor = session.term.lock().mode().contains(TermMode::APP_CURSOR);
            if let Some(bytes) = encode_key(key, modifiers, app_cursor) {
                session.write(&bytes);
            }
        }
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

/// The bytes xterm sends for `key`. `app_cursor` is the terminal's
/// application cursor mode (DECCKM). A plain letter is text, not a key, so it
/// sends nothing here.
fn encode_key(key: Key, modifiers: Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    let modified = xterm_mod(modifiers);
    let bytes = match key {
        Key::Enter => enter_bytes(modified),
        Key::Backspace => backspace_bytes(modifiers),
        Key::Tab => tab_bytes(modified),
        Key::Escape => vec![0x1b],
        Key::ArrowUp => cursor_key(b'A', modified, app_cursor),
        Key::ArrowDown => cursor_key(b'B', modified, app_cursor),
        Key::ArrowRight => cursor_key(b'C', modified, app_cursor),
        Key::ArrowLeft => cursor_key(b'D', modified, app_cursor),
        Key::Home => cursor_key(b'H', modified, app_cursor),
        Key::End => cursor_key(b'F', modified, app_cursor),
        Key::PageUp => csi_tilde(5, modified),
        Key::PageDown => csi_tilde(6, modified),
        Key::Delete => csi_tilde(3, modified),
        Key::Insert => csi_tilde(2, modified),
        // F1–F4 are SS3 alone (`kf1`) and `CSI 1;<mod>` with modifiers
        // (`kf13`, `kf25`, …), as xterm's terminfo describes them.
        Key::F1 => cursor_key(b'P', modified, true),
        Key::F2 => cursor_key(b'Q', modified, true),
        Key::F3 => cursor_key(b'R', modified, true),
        Key::F4 => cursor_key(b'S', modified, true),
        Key::F5 => csi_tilde(15, modified),
        Key::F6 => csi_tilde(17, modified),
        Key::F7 => csi_tilde(18, modified),
        Key::F8 => csi_tilde(19, modified),
        Key::F9 => csi_tilde(20, modified),
        Key::F10 => csi_tilde(21, modified),
        Key::F11 => csi_tilde(23, modified),
        Key::F12 => csi_tilde(24, modified),
        _ => return letter_key(key, modifiers),
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

/// The arrows, Home and End, as xterm sends them: `CSI <letter>`, or SS3
/// (`ESC O <letter>`) in application cursor mode. With modifiers,
/// `CSI 1;<mod> <letter>` in either mode.
fn cursor_key(letter: u8, modified: u8, app_cursor: bool) -> Vec<u8> {
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

fn csi_tilde(number: u8, modified: u8) -> Vec<u8> {
    if modified == 1 {
        format!("\x1b[{number}~").into_bytes()
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

/// Ctrl and a letter is that letter's control code, A as 1 through Z as 26;
/// Alt puts ESC before it, or before the letter itself.
fn letter_key(key: Key, modifiers: Modifiers) -> Option<Vec<u8>> {
    let letter = letter(key)?;
    let escape = modifiers.alt.then_some(0x1b);
    if modifiers.ctrl {
        return Some(escape.into_iter().chain([letter - b'a' + 1]).collect());
    }
    let letter = if modifiers.shift {
        letter.to_ascii_uppercase()
    } else {
        letter
    };
    escape.map(|escape| vec![escape, letter])
}

/// The lowercase letter of `A` to `Z`, which egui declares in order.
fn letter(key: Key) -> Option<u8> {
    (Key::A..=Key::Z)
        .contains(&key)
        .then(|| b'a' + (key as u8 - Key::A as u8))
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

    /// What runs when neither `SHELL` nor the passwd shell can.
    #[cfg(unix)]
    const FALLBACK: &str = if cfg!(target_os = "macos") {
        "/bin/zsh"
    } else {
        "/bin/sh"
    };

    /// Program, then arguments.
    #[cfg(unix)]
    fn argv(shell: Option<&OsStr>, passwd: Option<&str>, unset: &[&str]) -> Vec<String> {
        let argv = shell_argv(shell, passwd, unset);
        std::iter::once(argv.program).chain(argv.args).collect()
    }

    /// Names are unset through `env -u`, and an unsafe name is skipped. A
    /// relative `SHELL` is never run: an absolute passwd shell comes next,
    /// then the fallback. macOS logs in (`-l`), Linux does not.
    #[cfg(unix)]
    #[test]
    fn shell_argv_runs_an_absolute_shell_without_the_unset_names() {
        use std::os::unix::ffi::OsStrExt;

        // Case, `SHELL`, passwd shell, names to unset, program then arguments.
        type Row = (
            &'static str,
            Option<&'static str>,
            Option<&'static str>,
            &'static [&'static str],
            &'static [&'static str],
        );
        const ENV: &str = "/usr/bin/env";
        #[rustfmt::skip]
        const SHELLS: &[Row] = &[
            ("bash", Some("/bin/bash"), None, &[], &["/bin/bash"]),
            ("scrubbed", Some("/bin/bash"), None, &["GIT_DIR", "GH_TOKEN"],
                &[ENV, "-u", "GIT_DIR", "-u", "GH_TOKEN", "--", "/bin/bash"]),
            ("passwd shell", None, Some("/bin/bash"), &["GIT_EXEC_PATH"],
                &[ENV, "-u", "GIT_EXEC_PATH", "--", "/bin/bash"]),
            ("empty SHELL", Some(""), None, &[], &[FALLBACK]),
            ("unsafe name", Some("/bin/sh"), None,
                &["GIT_DIR;touch /tmp/x", "GH_TOKEN"], &[ENV, "-u", "GH_TOKEN", "--", "/bin/sh"]),
            ("relative SHELL", Some("./sh"), Some("/bin/bash"), &[], &["/bin/bash"]),
            ("relative passwd", Some("bash"), Some("bin/zsh"), &[], &[FALLBACK]),
            ("parent SHELL", Some("../sh"), None, &["GIT_DIR"], &[ENV, "-u", "GIT_DIR", "--", FALLBACK]),
        ];
        let login: &[&str] = if cfg!(target_os = "macos") {
            &["-l"]
        } else {
            &[]
        };
        for &(case, shell, passwd, unset, expected) in SHELLS {
            assert_eq!(
                argv(shell.map(OsStr::new), passwd, unset),
                [expected, login].concat(),
                "{case}"
            );
        }
        assert_eq!(
            argv(Some(OsStr::from_bytes(b"\xff")), None, &[]),
            [&[FALLBACK], login].concat(),
            "non-Unicode SHELL"
        );
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
        let dir = std::path::PathBuf::from(dir);
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
        let mut session = match ShellSession::spawn(dir.path(), &ctx, &metrics) {
            Ok(session) => session,
            Err(error) => panic!("{error}"),
        };
        assert_eq!(std::env::var_os("SHELL"), shell_before);

        wait_for_shell_output(&session);
        // The shell runs in the repository, with the terminal's TERM, and
        // follows the strip's size.
        let cwd = std::fs::read_link(format!("/proc/{}/cwd", session.pid));
        assert_eq!(cwd.ok(), dir.path().canonicalize().ok(), "shell directory");
        let environ = std::fs::read(format!("/proc/{}/environ", session.pid));
        let term = b"TERM=xterm-256color".as_slice();
        let named = environ.is_ok_and(|vars| vars.split(|byte| *byte == 0).any(|var| var == term));
        assert!(named, "the shell has no TERM=xterm-256color");
        let taller = Metrics {
            rows: 40,
            ..metrics
        };
        session.resize(&taller, 1.0);
        assert_eq!(session.term.lock().screen_lines(), 40);
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
        use alacritty_terminal::index::Line;
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

    /// (LANG, LC_ALL, LC_CTYPE, LANG for the shell). A Dock launch has no
    /// locale, so the shell gets UTF-8; a locale that is set is inherited.
    #[test]
    fn shell_env_names_the_terminal_and_fills_a_missing_locale() {
        const UTF8: Option<&str> = Some("en_US.UTF-8");
        #[rustfmt::skip]
        let cases = [
            (None, None, None, UTF8),
            (Some(""), Some(""), Some(""), UTF8),
            (Some("fr_FR.UTF-8"), None, None, None),
            (None, Some("C"), None, None),
            (None, None, UTF8, None),
        ];
        for (lang, lc_all, lc_ctype, given) in cases {
            let env = shell_env(lang, lc_all, lc_ctype);
            let case = format!("{lang:?} {lc_all:?} {lc_ctype:?}");
            assert_eq!(env["TERM"], "xterm-256color", "{case}");
            assert_eq!(env["COLORTERM"], "truecolor", "{case}");
            assert_eq!(env.get("LANG").map(String::as_str), given, "{case}");
        }
    }

    /// (key, modifiers, application cursor mode, bytes), as xterm sends them.
    /// A plain letter is text, not a key, so it sends nothing here.
    #[test]
    fn keys_send_the_xterm_sequences() {
        const NONE: Modifiers = Modifiers::NONE;
        const SHIFT: Modifiers = Modifiers::SHIFT;
        const ALT: Modifiers = Modifiers::ALT;
        const CTRL: Modifiers = Modifiers::CTRL;
        #[rustfmt::skip]
        const KEYS: &[(Key, Modifiers, bool, &[u8])] = &[
            (Key::Enter, NONE, false, b"\r"),
            (Key::Enter, CTRL.plus(ALT).plus(SHIFT), false, b"\x1b[27;8;13~"),
            (Key::Backspace, NONE, false, b"\x7f"),
            (Key::Backspace, ALT, false, b"\x1b\x7f"),
            (Key::Tab, NONE, false, b"\t"),
            (Key::Tab, SHIFT, false, b"\x1b[Z"),
            (Key::Tab, CTRL, false, b"\x1b[27;5;9~"),
            (Key::Escape, NONE, false, b"\x1b"),
            (Key::ArrowUp, NONE, false, b"\x1b[A"),
            (Key::ArrowDown, NONE, true, b"\x1bOB"),
            (Key::ArrowRight, NONE, false, b"\x1b[C"),
            (Key::ArrowLeft, NONE, true, b"\x1bOD"),
            (Key::ArrowLeft, CTRL, true, b"\x1b[1;5D"),
            (Key::Home, NONE, false, b"\x1b[H"),
            (Key::Home, NONE, true, b"\x1bOH"),
            (Key::End, NONE, true, b"\x1bOF"),
            (Key::End, NONE, false, b"\x1b[F"),
            (Key::Home, CTRL, true, b"\x1b[1;5H"),
            (Key::End, SHIFT, false, b"\x1b[1;2F"),
            (Key::PageUp, NONE, false, b"\x1b[5~"),
            (Key::PageDown, ALT, false, b"\x1b[6;3~"),
            (Key::Delete, NONE, false, b"\x1b[3~"),
            (Key::Insert, NONE, false, b"\x1b[2~"),
            (Key::F1, NONE, true, b"\x1bOP"),
            (Key::F1, SHIFT, false, b"\x1b[1;2P"),
            (Key::F1, CTRL, false, b"\x1b[1;5P"),
            (Key::F2, NONE, false, b"\x1bOQ"),
            (Key::F2, SHIFT, false, b"\x1b[1;2Q"),
            (Key::F3, NONE, false, b"\x1bOR"),
            (Key::F4, NONE, false, b"\x1bOS"),
            (Key::F4, SHIFT, false, b"\x1b[1;2S"),
            (Key::F5, NONE, false, b"\x1b[15~"),
            (Key::F6, NONE, false, b"\x1b[17~"),
            (Key::F7, NONE, false, b"\x1b[18~"),
            (Key::F8, NONE, false, b"\x1b[19~"),
            (Key::F9, NONE, false, b"\x1b[20~"),
            (Key::F10, NONE, false, b"\x1b[21~"),
            (Key::F11, NONE, false, b"\x1b[23~"),
            (Key::F12, CTRL.plus(SHIFT), false, b"\x1b[24;6~"),
            (Key::A, CTRL.plus(ALT), false, b"\x1b\x01"),
            (Key::B, ALT, false, b"\x1bb"),
            (Key::B, ALT.plus(SHIFT), false, b"\x1bB"),
            (Key::C, NONE, false, b""),
            (Key::Space, NONE, false, b""),
        ];
        for &(key, modifiers, app_cursor, expected) in KEYS {
            let bytes = encode_key(key, modifiers, app_cursor);
            assert_eq!(
                bytes.unwrap_or_default(),
                expected,
                "{key:?} {modifiers:?}, application cursor {app_cursor}"
            );
        }
        // Ctrl and a letter is that letter's control code, A as 1 through Z as 26.
        let letters: Vec<(Key, u8)> = Key::ALL
            .iter()
            .filter_map(|key| match key.name().as_bytes() {
                [byte] if byte.is_ascii_uppercase() => Some((*key, byte - b'@')),
                _ => None,
            })
            .collect();
        assert_eq!(letters.len(), 26);
        for (key, code) in letters {
            assert_eq!(letter_key(key, CTRL), Some(vec![code]), "Ctrl+{key:?}");
        }
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

    /// At every scale each side is at least one cell of at least one pixel,
    /// and its pixel product fits the `u16` alacritty stores it in.
    #[test]
    fn window_size_never_reports_a_zero_grid() {
        let fits = |cells: u16, px: u16| {
            cells >= 1 && px >= 1 && u32::from(cells) * u32::from(px) <= u32::from(u16::MAX)
        };
        let small = grid_metrics(80, 24, 8.0, 16.0);
        let huge = grid_metrics(1_000, 400, 1_000.0, 1_000.0);
        for (metrics, scale) in [
            (&small, 0.0_f32),
            (&small, -1.0),
            (&small, 2.0),
            (&small, f32::NAN),
            (&small, f32::INFINITY),
            (&huge, 10.0),
        ] {
            let size = window_size(metrics, scale);
            assert!(
                fits(size.num_cols, size.cell_width) && fits(size.num_lines, size.cell_height),
                "{scale}: {size:?}"
            );
        }
        assert!(
            window_size(&huge, 10.0).cell_width < 1_000,
            "cell width was not clamped"
        );
    }

    #[test]
    fn windows_shell_prefers_pwsh_then_windows_powershell() {
        let pwsh = r"C:\Program Files\PowerShell\7\pwsh.exe";
        let powershell = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
        let both = windows_shell_argv(Some(r"C:\Program Files"), Some(r"C:\Windows"), |path| {
            path == pwsh || path == powershell
        })
        .expect("pwsh");
        assert_eq!(both.program, pwsh);
        assert_eq!(both.args, ["-NoLogo"]);
        assert_eq!(
            conpty_program(&both.program),
            r#""C:\Program Files\PowerShell\7\pwsh.exe""#
        );

        let second = windows_shell_argv(Some(r"C:\Program Files"), Some(r"C:\Windows"), |path| {
            path == powershell
        })
        .expect("powershell");
        assert_eq!(second.program, powershell);
        assert_eq!(second.args, ["-NoLogo"]);

        let relative = windows_shell_argv(Some(r"Program Files"), Some("Windows"), |_| true);
        assert!(relative.is_err(), "{relative:?}");

        let missing = windows_shell_argv(Some(r"C:\Program Files"), Some(r"C:\Windows"), |_| false);
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
        for (passwd, program) in [(next, next), (locked, FALLBACK)] {
            let argv = shell_argv(Some(OsStr::new(locked)), Some(passwd), &[]);
            assert_eq!(argv.program, program, "passwd shell {passwd}");
        }
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

    /// (case, new size, child exited, child alive, resize). Cell pixels are
    /// rounded per scale, so a move from 1x to 2x is a new size. An unchanged
    /// size never asks whether the child is alive: that reads `/proc`.
    #[test]
    fn the_pty_takes_only_a_new_size_and_only_while_the_child_lives() {
        let metrics = grid_metrics(80, 24, 8.4, 16.0);
        let one = window_size(&metrics, 1.0);
        let two = window_size(&metrics, 2.0);
        assert_eq!((two.num_cols, two.cell_width), (one.num_cols, 17));
        let wider = WindowSize {
            cell_width: 9,
            ..one
        };
        for (case, size, exited, alive, resize) in [
            ("same size", one, false, true, false),
            ("2x", two, false, true, true),
            ("wider cells", wider, false, true, true),
            ("exited", two, true, true, false),
            ("hung up", two, false, false, false),
        ] {
            let asked = std::cell::Cell::new(false);
            let decided = should_resize(one, size, exited, || {
                asked.set(true);
                alive
            });
            assert_eq!(decided, resize, "{case}");
            assert!(!(asked.get() && case == "same size"), "{case}");
        }
    }

    /// Typing reaches the shell. Escape, Tab and the arrows belong to it too
    /// (vim, completion, history), so they must not move focus out of the
    /// terminal. Ctrl+Tab does, so the keyboard is never trapped.
    #[cfg(unix)]
    #[test]
    fn focused_terminal_keeps_escape_tab_and_arrows_until_ctrl_tab() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cwd = dir.path().to_owned();
        let view_id = Id::unique("focus test");
        // The user's prompt may use glyphs the test fonts lack.
        let mut harness = egui_kittest::Harness::builder()
            .with_size(Vec2::new(640.0, 320.0))
            .with_step_dt(1.0 / 60.0)
            .allow_missing_glyphs()
            .build_ui_state(
                move |ui, terminal: &mut Option<Terminal>| {
                    let _ = ui.button("Above");
                    let palette = super::super::theme::DEFAULT_THEME.palette;
                    show(ui, terminal, &cwd, view_id, &palette);
                },
                None,
            );
        let started = std::time::Instant::now();
        while !matches!(harness.state(), Some(Terminal::Live(_))) {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "shell did not start"
            );
            harness.step();
            thread::sleep(Duration::from_millis(10));
        }
        let inside = Pos2::new(320.0, 200.0);
        harness.hover_at(inside);
        for pressed in [true, false] {
            harness.event(Event::PointerButton {
                pos: inside,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            });
            harness.step();
        }
        harness.step();
        let focused = |harness: &egui_kittest::Harness<'_, Option<Terminal>>| {
            harness
                .ctx
                .memory(|memory| memory.has_focus(focus_id(view_id)))
        };
        assert!(focused(&harness), "a click focuses the terminal");
        // Text and Enter reach the shell: only the command's output has 42.
        harness.event(Event::Text("echo cthulhu-$((6 * 7))".to_owned()));
        harness.key_press(Key::Enter);
        let shown = |harness: &egui_kittest::Harness<'_, Option<Terminal>>| match harness.state() {
            Some(Terminal::Live(session)) => {
                let term = session.term.lock();
                let cells = term.renderable_content().display_iter;
                cells.map(|cell| cell.c).collect::<String>()
            }
            _ => String::new(),
        };
        while !shown(&harness).contains("cthulhu-42") {
            let late = started.elapsed() > Duration::from_secs(10);
            assert!(!late, "{}", shown(&harness));
            harness.step();
            thread::sleep(Duration::from_millis(10));
        }
        // A drag over the screen's top rows selects their text.
        for (at, pressed) in [(Pos2::new(20.0, 40.0), true), (inside, false)] {
            harness.hover_at(at);
            harness.step();
            harness.event(Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            });
            harness.step();
        }
        let selected = match harness.state() {
            Some(Terminal::Live(session)) => session.term.lock().selection_to_string(),
            _ => None,
        };
        assert!(selected.is_some_and(|text| text.contains("cthulhu-42")));
        for key in [Key::Escape, Key::Tab, Key::ArrowUp, Key::ArrowLeft] {
            harness.key_press(key);
            harness.step();
            harness.step();
            assert!(focused(&harness), "{key:?} moved focus out of the terminal");
        }
        for modifiers in [Modifiers::CTRL, Modifiers::CTRL | Modifiers::SHIFT] {
            harness.key_press_modifiers(modifiers, Key::Tab);
            harness.step();
            harness.step();
            assert!(
                !focused(&harness),
                "{modifiers:?}+Tab kept focus in the terminal"
            );
            assert!(
                harness.ctx.memory(|memory| memory.focused()).is_some(),
                "{modifiers:?}+Tab dropped focus instead of moving it"
            );
            harness.hover_at(inside);
            for pressed in [true, false] {
                harness.event(Event::PointerButton {
                    pos: inside,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Modifiers::NONE,
                });
                harness.step();
            }
            harness.step();
        }
    }

    #[cfg(unix)]
    #[test]
    fn unset_names_ignore_values_and_names_that_are_not_unicode() {
        use std::os::unix::ffi::OsStringExt;

        let raw = |bytes: &[u8]| OsString::from_vec(bytes.to_vec());
        let vars = [
            (raw(b"GIT_DIR"), raw(b"\xff")),
            (raw(b"LANG"), raw(b"\xff")),
            (raw(b"GIT_\xff"), raw(b"x")),
            (raw(b"GITHUB_TOKEN"), raw(b"\xff")),
            (raw(b"GIT_SSH_COMMAND"), raw(b"ssh")),
            // Another variable on Unix, which git and gh never read.
            (raw(b"github_token"), raw(b"mine")),
            (raw(b"git_prompt_style"), raw(b"short")),
        ];
        assert_eq!(vars_to_unset(vars), ["GITHUB_TOKEN", "GIT_DIR"]);
    }

    /// Colors the program did not set are this widget's. Programs such as vim
    /// ask for the background (OSC 11) to pick light or dark colors, so an
    /// unset color is answered with what is painted.
    #[test]
    fn unset_colors_paint_and_report_the_widgets_own() {
        let palette = super::super::theme::DEFAULT_THEME.palette;
        let unset = color::Colors::default();
        for (color, expected) in [
            (Color::Named(NamedColor::Foreground), palette.text),
            (Color::Named(NamedColor::BrightForeground), palette.text),
            (Color::Named(NamedColor::Cursor), palette.text),
            (Color::Named(NamedColor::DimForeground), palette.text_muted),
            (Color::Named(NamedColor::Background), palette.background),
            (Color::Named(NamedColor::BrightWhite), ansi_color(15)),
            (Color::Named(NamedColor::DimWhite), ansi_color(7)),
            // The table, the corners of the 6x6x6 cube, one inner step on
            // each axis (xterm's #5f87af), and the ends of the gray ramp.
            (Color::Indexed(1), Color32::from_rgb(0xf7, 0x76, 0x8e)),
            (Color::Indexed(16), Color32::from_rgb(0, 0, 0)),
            (Color::Indexed(67), Color32::from_rgb(95, 135, 175)),
            (Color::Indexed(231), Color32::from_rgb(255, 255, 255)),
            (Color::Indexed(232), Color32::from_gray(8)),
            (Color::Indexed(255), Color32::from_gray(238)),
            (
                Color::Spec(Rgb { r: 1, g: 2, b: 3 }),
                Color32::from_rgb(1, 2, 3),
            ),
        ] {
            assert_eq!(
                resolve_color(color, &unset, &palette),
                expected,
                "{color:?}"
            );
        }

        let rgb = |color: Color32| Rgb {
            r: color.r(),
            g: color.g(),
            b: color.b(),
        };
        let set = Rgb { r: 1, g: 2, b: 3 };
        let mut colors = color::Colors::default();
        colors[NamedColor::Cursor] = Some(set);
        colors[5] = Some(set);
        for (index, expected) in [
            (NamedColor::Foreground as usize, rgb(palette.text)),
            (NamedColor::Background as usize, rgb(palette.background)),
            (NamedColor::Cursor as usize, set),
            (1, rgb(ansi_color(1))),
            (5, set),
        ] {
            assert_eq!(
                reported_color(&colors, index, &palette),
                expected,
                "{index}"
            );
        }
    }

    /// (column, row, wide, cell) for 8x16 cells from (100, 50). A wide
    /// character takes two columns.
    #[test]
    fn cells_are_laid_out_from_the_origin() {
        let metrics = grid_metrics(80, 24, 8.0, 16.0);
        for (col, row, wide, [left, top, right]) in [
            (0, 0, false, [100.0, 50.0, 108.0]),
            (3, 2, false, [124.0, 82.0, 132.0]),
            (3, 2, true, [124.0, 82.0, 140.0]),
        ] {
            let cell = cell_rect(Pos2::new(100.0, 50.0), &metrics, col, row, wide);
            let expected = Rect::from_x_y_ranges(left..=right, top..=top + 16.0);
            assert_eq!(cell, expected, "{col},{row} wide {wide}");
        }
    }

    /// (pointer, row, column, grid line) on an 80x24 screen of 8x16 cells
    /// at (100, 50), scrolled 2 lines back. Outside the screen the hit
    /// clamps to the nearest cell.
    #[test]
    fn a_pointer_hits_the_cell_under_it() {
        let metrics = grid_metrics(80, 24, 8.0, 16.0);
        let rect = Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(640.0, 390.0));
        for (x, y, row, col, line) in [
            (100.0, 50.0, 0, 0, -2),
            (125.0, 90.0, 2, 3, 0),
            (0.0, 0.0, 0, 0, -2),
            (900.0, 437.0, 23, 79, 21),
        ] {
            let hit = hit_at(rect, Pos2::new(x, y), &metrics, 2);
            let point = Point::new(alacritty_terminal::index::Line(line), Column(col));
            assert_eq!((hit.row, hit.col, hit.point), (row, col, point), "{x},{y}");
        }
    }

    /// (box height, prompt rows, top of the screen, repaint), with 16-point
    /// rows in a box whose top is at 100. The block starts at the top of the
    /// box; a box more than half a row off its height asks for a frame.
    #[test]
    fn the_preview_puts_the_prompt_block_at_the_top() {
        for (height, span, top, repaint) in [
            (32.0, 0..2, 100.0, false),
            (32.0, 3..5, 52.0, false),
            (40.0, 3..5, 52.0, false),
            (48.0, 3..5, 52.0, true),
            (16.0, 3..5, 52.0, true),
        ] {
            let visible = Rect::from_min_size(Pos2::new(10.0, 100.0), Vec2::new(200.0, height));
            let placed = place_prompt(visible, &span, 16.0);
            let case = format!("{height} {span:?}");
            assert_eq!(placed, (Pos2::new(10.0, top), repaint), "{case}");
        }
    }

    /// The Settings preview runs a real shell, on a screen taller than its
    /// box so a long prompt is not scrolled away, and draws its prompt.
    #[cfg(unix)]
    #[test]
    fn the_preview_draws_the_prompt_of_a_tall_shell() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let cwd = dir.path().to_owned();
        // The user's prompt may use glyphs the test fonts lack.
        let mut harness = egui_kittest::Harness::builder()
            .with_size(Vec2::new(640.0, 120.0))
            .with_step_dt(1.0 / 60.0)
            .allow_missing_glyphs()
            .build_ui_state(
                move |ui, terminal: &mut Option<Terminal>| {
                    let palette = super::super::theme::DEFAULT_THEME.palette;
                    show_preview(ui, terminal, &cwd, &palette);
                },
                None,
            );
        let started = std::time::Instant::now();
        let prompt = |harness: &egui_kittest::Harness<'_, Option<Terminal>>| {
            let texts = crate::ui::test_support::painted_texts(harness);
            texts
                .iter()
                .any(|(_, galley)| galley.text() != "Starting shell…")
        };
        while !prompt(&harness) {
            assert!(started.elapsed() < Duration::from_secs(5), "no prompt");
            harness.step();
            thread::sleep(Duration::from_millis(10));
        }
        let Some(Terminal::Live(session)) = harness.state() else {
            panic!("the preview has no live shell");
        };
        assert_eq!(session.term.lock().screen_lines(), PREVIEW_SCREEN_ROWS);
    }

    /// (case, screen columns, bytes, prompt rows). A blank row ends the
    /// prompt above the cursor; a colored or inverse space does not. A block
    /// taller than the preview keeps the rows nearest the cursor.
    #[test]
    fn prompt_span_is_the_block_that_ends_at_the_cursor() {
        #[rustfmt::skip]
        let cases: [(&str, usize, &[u8], Range<usize>); 6] = [
            ("one-line prompt", 40, b"$ ", 0..1),
            ("blank line above", 40, b"\r\n~/repo on main\r\n> ", 1..3),
            ("colored space", 40, b"\x1b[41m \x1b[0m\r\n>", 0..2),
            ("inverse space", 40, b"\x1b[7m \x1b[0m\r\n>", 0..2),
            ("ten rows", 8, b"x\r\nx\r\nx\r\nx\r\nx\r\nx\r\nx\r\nx\r\nx\r\nx", 2..10),
            ("output, blank, prompt", 40, b"hello\r\n\r\n$ ", 2..3),
        ];
        for (case, columns, bytes, expected) in cases {
            let grid = GridSize {
                columns,
                screen_lines: 16,
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
            assert_eq!(prompt_span(&term), expected, "{case}");
        }
    }
}
