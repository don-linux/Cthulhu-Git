//! The window: a home screen, a repository view, and a settings screen.
//!
//! On launch, a folder passed on the command line opens first, then the last
//! repository from the settings, otherwise the home screen shows. The
//! repository chrome is ready before the window appears; the history graph
//! follows on the worker. A later open stays on a worker because the window
//! is visible then. Settings are described in `docs/SETTINGS.md`, themes in
//! `docs/THEMES.md`, bundled faces in `docs/FONTS.md`.

#[cfg(test)]
mod adversarial_tests;
mod avatars;
mod changes;
mod folder_picker;
mod fonts;
mod github_auth;
mod history;
#[cfg(test)]
mod history_tests;
mod home;
mod hover;
mod icons;
#[cfg(test)]
mod network_tests;
mod repo_view;
mod settings;
mod terminal;
#[cfg(test)]
mod test_support;
pub mod theme;
mod widgets;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

use cthulhu_git::git::{
    self, Branch, CommitDetail, Git, GitError, GraphCandidate, GraphTip, Head, HistoryGraph,
    RepoInfo,
};
use cthulhu_git::github::{self as gh, GitHubRepo};
use cthulhu_git::settings::{HistoryColumns, Settings, SettingsError};
use eframe::egui;

use avatars::AvatarService;
use folder_picker::FolderPicker;
use github_auth::GitHubAuth;

/// Newest commits read for the branch graph; the view says when there are more.
const HISTORY_LIMIT: usize = 1000;

pub struct OpenedRepo {
    /// The git that opened the repository. Every later read and write uses it.
    pub git: Git,
    pub info: RepoInfo,
    pub graph: HistoryGraph,
    /// Tips the displayed graph was built from.
    pub graph_selection: Vec<GraphTip>,
    /// Last selection asked of git. After a failure it stays, so the next
    /// frame does not ask again until the checkboxes change.
    pub graph_requested: Vec<GraphTip>,
    /// Full hash when HEAD is detached. That commit stays in the graph.
    pub detached_oid: Option<String>,
    pub branches: Vec<ListedBranch>,
    pub latest: Option<CommitDetail>,
    /// New on every open, so the history starts scrolled to the top each time.
    pub view_id: egui::Id,
    /// Shell for this repository. `None` until the terminal strip is shown.
    pub terminal: Option<terminal::Terminal>,
    /// The GitHub repository behind `upstream`, `origin` or the first GitHub
    /// remote, for avatars and links.
    pub github: Option<GitHubRepo>,
    /// Commit details read for the history hover.
    pub hover: hover::HoverCache,
    /// Patches for the changes modal. Kept across opens of the same file.
    pub diffs: changes::DiffCache,
    /// The file list and diff, when View changes is open.
    pub changes: Option<changes::Changes>,
}

/// A local branch and whether it is drawn in the history graph.
pub struct ListedBranch {
    pub branch: Branch,
    pub checked: bool,
}

/// What a screen asks the app to do after drawing itself.
#[derive(Debug, PartialEq)]
pub enum Action {
    Browse,
    Open(PathBuf),
    Home,
    OpenSettings,
    CloseSettings,
    ToggleBranchesSidebar,
    ToggleDetailSidebar,
    ToggleTerminal,
    SetUiFont(Option<String>),
    SetTerminalFont(Option<String>),
    /// The local branch whose upstream is fetched.
    Fetch(String),
    Pull,
    /// Local branch to check out. The name is the short ref, unchanged.
    Checkout(String),
    SetHistoryColumns(HistoryColumns),
    SetAuthorAvatars(bool),
}

/// A fetch, pull, or branch switch running against the open repository.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SyncKind {
    Fetch,
    Pull,
    Checkout,
}

enum Screen {
    Home,
    Repo(Box<OpenedRepo>),
}

type OpenResult = Result<Refreshed, String>;

struct Opening {
    path: PathBuf,
    receiver: Receiver<OpenResult>,
}

/// One home-screen scan. Dropping it disconnects the worker, which stops
/// after the git command it is already in.
struct GlanceScan {
    receiver: Receiver<GlanceMsg>,
}

struct GlanceMsg {
    path: PathBuf,
    glance: home::Glance,
}

pub struct CthulhuApp {
    settings: Settings,
    /// `None` when the OS reports no home folder; settings are then not saved.
    settings_path: Option<PathBuf>,
    /// `save_settings` writes the file only when this is true. A missing file
    /// still counts as loaded. A read or parse error does not, so the next
    /// action cannot replace the user's file with defaults.
    loaded_ok: bool,
    screen: Screen,
    opening: Option<Opening>,
    picker: Option<FolderPicker>,
    error: Option<String>,
    opened_count: u64,
    /// Covers the current screen. A repository underneath stays open.
    settings_open: bool,
    fonts: fonts::FontService,
    /// Shell drawn on the Terminal settings page. Separate from the repository
    /// terminal so the preview size does not resize that session. Dropped when
    /// settings closes; not saved.
    font_preview: Option<terminal::Terminal>,
    graph_load: Option<GraphLoad>,
    graph_generation: u64,
    sync_job: Option<SyncJob>,
    avatars: AvatarService,
    github: GitHubAuth,
    find_git: FindGit,
    /// `git --version` for the About page. `None` until Settings has asked.
    git_version: Option<String>,
    git_probe: Option<Receiver<String>>,
    /// Branch and latest commit for the recent list. Missing means the scan
    /// has not reported that path yet.
    glances: HashMap<PathBuf, home::Glance>,
    /// Paths the current or previous home scan was started with, in order.
    glance_paths: Vec<PathBuf>,
    glance_scan: Option<GlanceScan>,
    /// The previous frame was Home. Returning to it scans again.
    was_home: bool,
}

/// Downloaded avatars, beside the settings in the per-user cache folder.
fn avatar_cache_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("io.github", "don-linux", "cthulhu-git")
        .map(|dirs| dirs.cache_dir().join("avatars"))
}

/// What the app takes from the machine when it starts. [`Startup::system`] is
/// the user's; tests point it at their own files.
pub struct Startup {
    /// A folder named on the command line. It wins over the last repository.
    pub command_line: Option<PathBuf>,
    /// Where settings are read and saved. `None` when there is no home folder.
    pub settings_path: Option<PathBuf>,
    /// Builds the avatar service once the settings say whether avatars are on.
    pub avatars: fn(enabled: bool) -> AvatarService,
    /// Finds git for each open, the recent list and the About page.
    pub find_git: FindGit,
}

/// How the app finds git. [`Git::discover`] reads `CTHULHU_GIT` and `PATH`.
pub type FindGit = Arc<dyn Fn() -> Result<Git, GitError> + Send + Sync>;

impl Startup {
    /// The user's settings file and avatars from the network.
    pub fn system(command_line: Option<PathBuf>) -> Self {
        Self {
            command_line,
            settings_path: Settings::default_path(),
            avatars: |enabled| AvatarService::new(avatar_cache_dir(), enabled),
            find_git: Arc::new(Git::discover),
        }
    }
}

impl CthulhuApp {
    pub fn new(ctx: &egui::Context, startup: Startup) -> Self {
        let Startup {
            command_line,
            settings_path,
            avatars,
            find_git,
        } = startup;
        let (settings, loaded_ok, error) =
            loaded_settings(settings_path.as_deref().map(Settings::load_from));
        theme::apply(ctx, theme::resolve(settings.theme.as_deref()));
        let mut fonts = fonts::FontService::default();
        fonts.sync(
            ctx,
            settings.ui_font.as_deref(),
            settings.terminal_font.as_deref(),
        );

        let first_open = command_line.or_else(|| settings.last_repository.clone());
        let avatars = avatars(settings.author_avatars);
        let mut github = GitHubAuth::default();
        // Only a user who saved a token ever reaches the keychain.
        if settings.github_token_in_keyring {
            github.load(ctx);
        }
        let mut app = Self {
            settings,
            settings_path,
            loaded_ok,
            screen: Screen::Home,
            opening: None,
            picker: None,
            error,
            opened_count: 0,
            settings_open: false,
            fonts,
            font_preview: None,
            graph_load: None,
            graph_generation: 0,
            sync_job: None,
            avatars,
            github,
            find_git,
            git_version: None,
            git_probe: None,
            glances: HashMap::new(),
            glance_paths: Vec::new(),
            glance_scan: None,
            was_home: false,
        };
        if let Some(path) = first_open {
            // The window stays hidden until the first frame is painted.
            // Finishing this open here means that frame is the repository
            // chrome, or the home screen with the error. The history graph
            // follows on the worker and is not part of that wait. A later
            // open stays on a worker thread: the window is visible then,
            // and blocking it would freeze the UI.
            let result = load(&app.find_git, &path);
            if app.finish_open(path, result) {
                app.start_open_graph(ctx);
            }
        }
        app
    }

    /// One `git --version` the first time Settings opens. Later visits reuse it.
    fn ensure_git_version(&mut self, ctx: &egui::Context) {
        if self.git_version.is_some() || self.git_probe.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        let find_git = Arc::clone(&self.find_git);
        thread::spawn(move || {
            let text = match find_git() {
                Ok(git) => git.version.to_string(),
                Err(_) => "Not found".to_owned(),
            };
            let _ = sender.send(text);
            ctx.request_repaint();
        });
        self.git_probe = Some(receiver);
    }

    fn poll_git_version(&mut self) {
        let Some(probe) = &self.git_probe else {
            return;
        };
        let text = match probe.try_recv() {
            Ok(text) => text,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => "Not found".to_owned(),
        };
        self.git_version = Some(text);
        self.git_probe = None;
    }

    /// Starts a scan when Home appears or the recent list changes. Repos are
    /// read one after another on one thread, and each result repaints.
    fn sync_glances(&mut self, ctx: &egui::Context) {
        self.poll_glances();
        let on_home = matches!(self.screen, Screen::Home);
        let entered = on_home && !self.was_home;
        self.was_home = on_home;
        if !on_home {
            return;
        }
        if !entered && self.settings.recent_repositories == self.glance_paths {
            return;
        }
        self.start_glance(ctx);
    }

    fn start_glance(&mut self, ctx: &egui::Context) {
        let paths = self.settings.recent_repositories.clone();
        self.glances.clear();
        self.glance_paths.clone_from(&paths);
        // Drop the previous receiver before the new thread starts, so the old
        // worker notices and does not keep reading after the list changed.
        self.glance_scan = None;
        if paths.is_empty() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        let find_git = Arc::clone(&self.find_git);
        thread::spawn(move || scan_glances(&find_git, paths, sender, ctx));
        self.glance_scan = Some(GlanceScan { receiver });
    }

    fn poll_glances(&mut self) {
        let Some(scan) = &self.glance_scan else {
            return;
        };
        let mut incoming = Vec::new();
        let mut disconnected = false;
        loop {
            match scan.receiver.try_recv() {
                Ok(msg) => incoming.push(msg),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if disconnected {
            self.glance_scan = None;
        }
        for msg in incoming {
            if self.glance_paths.iter().any(|path| path == &msg.path) {
                self.glances.insert(msg.path, msg.glance);
            }
        }
    }

    fn poll_github(&mut self, ctx: &egui::Context) {
        for change in self.github.poll(ctx) {
            match change {
                github_auth::Change::Token(token) => self.avatars.set_token(token),
                github_auth::Change::InKeychain(stored) => {
                    if self.settings.github_token_in_keyring != stored {
                        self.settings.github_token_in_keyring = stored;
                        self.save_settings();
                    }
                }
            }
        }
    }

    /// Chrome runs on a worker thread: blocking the UI thread would freeze the
    /// window. The history graph is a second worker, started once this open
    /// has installed the repository.
    fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.graph_generation = self.graph_generation.wrapping_add(1);
        self.graph_load = None;
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        let worker_path = path.clone();
        let find_git = Arc::clone(&self.find_git);
        thread::spawn(move || {
            let _ = sender.send(load(&find_git, &worker_path));
            ctx.request_repaint();
        });
        self.opening = Some(Opening { path, receiver });
    }

    fn poll_opening(&mut self, ctx: &egui::Context) {
        let Some(opening) = &self.opening else {
            return;
        };
        let result = match opening.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                Err("Opening stopped unexpectedly. Try again.".to_owned())
            }
        };
        let Some(opening) = self.opening.take() else {
            return;
        };
        if self.finish_open(opening.path, result) {
            self.start_open_graph(ctx);
        }
    }

    /// Installs the repository, or Home when opening failed.
    ///
    /// The history graph is not part of this result. A later log failure stays
    /// on the repository screen and keeps `last_repository`.
    fn finish_open(&mut self, path: PathBuf, result: OpenResult) -> bool {
        match result {
            Ok(refreshed) => {
                self.error = None;
                self.settings.remember_repository(&refreshed.info.root);
                self.save_settings();
                self.opened_count += 1;
                self.screen = Screen::Repo(Box::new(OpenedRepo {
                    git: refreshed.git,
                    info: refreshed.info,
                    // Empty until the history worker reports. Filling
                    // `graph_selection` would make `sync_graph` treat this
                    // blank graph as the answer, and filling `graph_requested`
                    // would latch a failure before the load starts.
                    graph: HistoryGraph::default(),
                    graph_selection: Vec::new(),
                    graph_requested: Vec::new(),
                    detached_oid: refreshed.detached_oid,
                    branches: refreshed
                        .branches
                        .into_iter()
                        // Only the current branch starts checked, so the graph
                        // is one line until the user marks more.
                        .map(|branch| ListedBranch {
                            checked: branch.current,
                            branch,
                        })
                        .collect(),
                    latest: refreshed.latest,
                    view_id: egui::Id::new(("repo-view", self.opened_count)),
                    terminal: None,
                    github: refreshed.github,
                    hover: hover::HoverCache::default(),
                    diffs: changes::DiffCache::default(),
                    changes: None,
                }));
                true
            }
            Err(message) => {
                self.error = Some(message);
                if self.settings.last_repository.as_deref() == Some(path.as_path()) {
                    self.settings.forget_last_repository();
                    self.save_settings();
                }
                self.screen = Screen::Home;
                false
            }
        }
    }

    /// One history worker for the repository just installed, when it has commits.
    ///
    /// No tips means an unborn repository, so the center stays "No commits yet."
    fn start_open_graph(&mut self, ctx: &egui::Context) {
        let tips = {
            let Screen::Repo(repo) = &self.screen else {
                return;
            };
            selected_tips(&repo.branches, repo.detached_oid.as_deref())
        };
        if tips.is_empty() {
            return;
        }
        self.start_graph_load(ctx, tips);
    }

    fn poll_picker(&mut self, ctx: &egui::Context) {
        let Some(choice) = self.picker.as_ref().and_then(FolderPicker::poll) else {
            return;
        };
        self.picker = None;
        if let Some(path) = choice {
            self.open(ctx, path);
        }
    }

    /// A failed save keeps the app running; the message joins any current error.
    ///
    /// A failed load leaves `loaded_ok` false. Writing then would replace the
    /// file the user still has, including recents, the theme, and
    /// `github_token_in_keyring`.
    fn save_settings(&mut self) {
        if !self.loaded_ok {
            return;
        }
        let Some(path) = &self.settings_path else {
            return;
        };
        if let Err(save_error) = self.settings.save_to(path) {
            let message = save_error.to_string();
            self.error = Some(match self.error.take() {
                Some(previous) => format!("{previous}\n{message}"),
                None => message,
            });
        }
    }

    /// The folder dialog starts next to the most recent repository.
    fn browse_start(&self) -> Option<&Path> {
        self.settings
            .recent_repositories
            .first()
            .and_then(|recent| recent.parent())
    }
}

/// Settings kept after startup, and whether a later save may write them.
///
/// `None` is "no home folder". `Err` is a file that could not be read or
/// parsed: the window shows the error and keeps the defaults in memory, and
/// `loaded_ok` stays false so the file is left as it is.
fn loaded_settings(
    loaded: Option<Result<Settings, SettingsError>>,
) -> (Settings, bool, Option<String>) {
    match loaded {
        Some(Ok(settings)) => (settings, true, None),
        Some(Err(error)) => (Settings::default(), false, Some(error.to_string())),
        None => (
            Settings::default(),
            false,
            Some("No home folder was found, so settings will not be saved.".to_owned()),
        ),
    }
}

impl eframe::App for CthulhuApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.poll_opening(ui.ctx());
        self.poll_graph();
        self.poll_sync();
        self.poll_picker(ui.ctx());
        self.poll_github(ui.ctx());
        self.avatars.poll(ui.ctx());
        self.fonts.sync(
            ui.ctx(),
            self.settings.ui_font.as_deref(),
            self.settings.terminal_font.as_deref(),
        );
        if self.settings_open {
            self.ensure_git_version(ui.ctx());
        }
        self.poll_git_version();
        self.sync_glances(ui.ctx());

        let syncing = self.sync_for_open_repo();
        let history_loading = self.history_loading();
        let actions = if self.settings_open {
            // The repository stays underneath. Keep the shell alive and take
            // keystrokes away from the hidden terminal.
            if let Screen::Repo(repo) = &mut self.screen {
                terminal::service(ui.ctx(), &mut repo.terminal);
                terminal::surrender_focus(ui.ctx(), repo.view_id);
            }
            let cwd = match &self.screen {
                Screen::Repo(repo) => Some(repo.info.root.clone()),
                Screen::Home => None,
            };
            terminal::service(ui.ctx(), &mut self.font_preview);
            let git_label = self
                .git_version
                .as_deref()
                .unwrap_or(if self.git_probe.is_some() {
                    "Checking…"
                } else {
                    "Not found"
                });
            let view = settings::SettingsView {
                author_avatars: self.settings.author_avatars,
                api_status: self.avatars.api_status(),
                busy: self.opening.is_some() || self.picker.is_some(),
                about: settings::AboutInfo::current(git_label),
            };
            settings::show(
                ui,
                self.settings.ui_font.as_deref(),
                self.settings.terminal_font.as_deref(),
                &mut self.font_preview,
                cwd.as_deref(),
                &view,
                &mut self.github,
            )
        } else {
            match &mut self.screen {
                Screen::Home => home::show(
                    ui,
                    &home::HomeState {
                        recent: &self.settings.recent_repositories,
                        glances: &self.glances,
                        busy: self.opening.is_some() || self.picker.is_some(),
                        opening: self.opening.as_ref().map(|opening| opening.path.as_path()),
                        error: self.error.as_deref(),
                    },
                )
                .into_iter()
                .collect(),
                Screen::Repo(repo) => repo_view::show(
                    ui,
                    repo,
                    &repo_view::ViewState {
                        error: self.error.as_deref(),
                        branches_open: !self.settings.history_sidebar_hidden,
                        detail_open: !self.settings.detail_sidebar_hidden,
                        terminal_open: !self.settings.terminal_hidden,
                        syncing,
                        columns: self.settings.history_columns,
                        history_loading,
                        busy: self.opening.is_some() || self.picker.is_some(),
                    },
                    &mut self.avatars,
                ),
            }
        };

        for action in actions {
            match action {
                // The folder dialog needs the window's frame.
                Action::Browse => {
                    if self.picker.is_none() && self.opening.is_none() {
                        self.error = None;
                        self.picker =
                            Some(FolderPicker::open(ui.ctx(), frame, self.browse_start()));
                    }
                }
                action => self.apply_action(ui.ctx(), action),
            }
        }
        self.sync_graph(ui.ctx());
    }
}

impl CthulhuApp {
    /// Every action except [`Action::Browse`], which [`eframe::App::ui`] handles
    /// because it needs the frame.
    fn apply_action(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::Browse => {}
            Action::Open(path) => {
                self.error = None;
                self.open(ctx, path);
            }
            Action::Home => {
                self.error = None;
                self.settings_open = false;
                self.font_preview = None;
                self.screen = Screen::Home;
            }
            Action::OpenSettings => {
                self.settings_open = true;
            }
            Action::CloseSettings => {
                self.settings_open = false;
                self.font_preview = None;
            }
            Action::ToggleBranchesSidebar => {
                self.settings.history_sidebar_hidden = !self.settings.history_sidebar_hidden;
                self.save_settings();
            }
            Action::ToggleDetailSidebar => {
                self.settings.detail_sidebar_hidden = !self.settings.detail_sidebar_hidden;
                self.save_settings();
            }
            Action::ToggleTerminal => {
                self.settings.terminal_hidden = !self.settings.terminal_hidden;
                if !self.settings.terminal_hidden {
                    self.settings.detail_sidebar_hidden = false;
                }
                self.save_settings();
            }
            Action::SetUiFont(name) => {
                self.settings.set_ui_font(name);
                self.save_settings();
            }
            Action::SetTerminalFont(name) => {
                self.settings.set_terminal_font(name);
                self.save_settings();
            }
            Action::Fetch(branch) => {
                self.start_sync(ctx, SyncCommand::Fetch(branch));
            }
            Action::Pull => {
                self.start_sync(ctx, SyncCommand::Pull);
            }
            Action::Checkout(name) => {
                self.start_sync(ctx, SyncCommand::Checkout(name));
            }
            Action::SetHistoryColumns(columns) => {
                let columns = columns.normalized();
                if self.settings.history_columns != columns {
                    self.settings.history_columns = columns;
                    self.save_settings();
                }
            }
            Action::SetAuthorAvatars(on) => {
                self.settings.author_avatars = on;
                self.avatars.set_enabled(on);
                self.save_settings();
            }
        }
    }
}

struct Refreshed {
    git: Git,
    info: RepoInfo,
    branches: Vec<Branch>,
    latest: Option<CommitDetail>,
    detached_oid: Option<String>,
    github: Option<GitHubRepo>,
}

/// One repository at a time. A dropped receiver stops the loop; the git
/// command already running is left to finish under its own time limit.
fn scan_glances(
    find_git: &FindGit,
    paths: Vec<PathBuf>,
    sender: Sender<GlanceMsg>,
    ctx: egui::Context,
) {
    let git = find_git().ok();
    for path in paths {
        let glance = match &git {
            Some(git) => match git::glance(git, &path) {
                Ok(ready) => home::Glance::Ready(ready),
                Err(_) => home::Glance::Unreadable,
            },
            None => home::Glance::Unreadable,
        };
        if sender.send(GlanceMsg { path, glance }).is_err() {
            break;
        }
        ctx.request_repaint();
    }
}

/// Repository chrome. The history graph is not built here, so a log failure
/// does not reject the open or forget the repository.
fn load(find_git: &FindGit, path: &Path) -> OpenResult {
    let git = find_git().map_err(|error| error.to_string())?;
    reload_repo(&git, path)
}

/// HEAD, branches and the latest commit, without rebuilding the graph.
fn reload_repo(git: &Git, path: &Path) -> Result<Refreshed, String> {
    let info = git::inspect(git, path).map_err(|error| error.to_string())?;
    let branches = git::branches(git, &info.root, &info.head).map_err(|error| error.to_string())?;
    let latest =
        git::latest_commit(git, &info.root, &info.head).map_err(|error| error.to_string())?;
    let detached_oid = match &info.head {
        Head::Detached { .. } => {
            Some(git::head_commit_oid(git, &info.root).map_err(|error| error.to_string())?)
        }
        Head::Branch(_) | Head::Unborn(_) => None,
    };
    // Remotes only enable avatars and links, so an unreadable config is not an error.
    let github = git::remotes(git, &info.root)
        .ok()
        .and_then(|remotes| gh::github_repo(&remotes));
    Ok(Refreshed {
        git: git.clone(),
        info,
        branches,
        latest,
        detached_oid,
        github,
    })
}

enum SyncCommand {
    Fetch(String),
    Pull,
    Checkout(String),
}

struct SyncJob {
    view_id: egui::Id,
    kind: SyncKind,
    receiver: Receiver<Result<Refreshed, String>>,
}

fn run_sync(git: &Git, root: &Path, command: SyncCommand) -> Result<Refreshed, String> {
    match &command {
        SyncCommand::Fetch(branch) => {
            git::fetch(git, root, branch).map_err(|error| error.to_string())?;
        }
        SyncCommand::Pull => {
            git::pull_ff_only(git, root).map_err(|error| error.to_string())?;
        }
        SyncCommand::Checkout(name) => {
            git::switch_branch(git, root, name).map_err(|error| error.to_string())?;
        }
    }
    reload_repo(git, root)
}

/// Keeps the sidebar checkboxes. A branch that was not in the list yet stays
/// unchecked, so a fetch does not pull it into the graph.
fn apply_refresh(repo: &mut OpenedRepo, refreshed: Refreshed) {
    let previous = std::mem::take(&mut repo.branches);
    repo.info = refreshed.info;
    repo.latest = refreshed.latest;
    repo.detached_oid = refreshed.detached_oid;
    repo.github = refreshed.github;
    repo.branches = refreshed
        .branches
        .into_iter()
        .map(|branch| {
            let checked = previous
                .iter()
                .find(|listed| listed.branch.name == branch.name)
                .map(|listed| listed.checked)
                .unwrap_or(false);
            ListedBranch { branch, checked }
        })
        .collect();
}

/// Checked branches that have commits, in sidebar order, plus detached HEAD.
///
/// Colors and the principal lane come from [`git::graph_tips`], which the
/// branch list uses as well.
fn selected_tips(branches: &[ListedBranch], detached_oid: Option<&str>) -> Vec<GraphTip> {
    let candidates: Vec<GraphCandidate<'_>> = branches
        .iter()
        .map(|listed| GraphCandidate {
            name: listed.branch.name.as_str(),
            oid: listed.branch.oid.as_deref(),
            current: listed.branch.current,
            committer_time: listed.branch.committer_time,
            checked: listed.checked,
        })
        .collect();
    git::graph_tips(&candidates, detached_oid)
}

struct GraphLoad {
    generation: u64,
    view_id: egui::Id,
    selection: Vec<GraphTip>,
    receiver: Receiver<Result<HistoryGraph, String>>,
}

impl CthulhuApp {
    /// This view's history worker is in flight. A load for another repository
    /// does not change the line in the center.
    fn history_loading(&self) -> bool {
        let Screen::Repo(repo) = &self.screen else {
            return false;
        };
        self.graph_load
            .as_ref()
            .is_some_and(|load| load.view_id == repo.view_id)
    }

    fn sync_for_open_repo(&self) -> Option<SyncKind> {
        let Screen::Repo(repo) = &self.screen else {
            return None;
        };
        self.sync_job
            .as_ref()
            .and_then(|job| (job.view_id == repo.view_id).then_some(job.kind))
    }

    fn start_sync(&mut self, ctx: &egui::Context, command: SyncCommand) {
        let Screen::Repo(repo) = &self.screen else {
            return;
        };
        if self
            .sync_job
            .as_ref()
            .is_some_and(|job| job.view_id == repo.view_id)
        {
            return;
        }
        let view_id = repo.view_id;
        let root = repo.info.root.clone();
        let git = repo.git.clone();
        let kind = match &command {
            SyncCommand::Fetch(_) => SyncKind::Fetch,
            SyncCommand::Pull => SyncKind::Pull,
            SyncCommand::Checkout(_) => SyncKind::Checkout,
        };
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let result = run_sync(&git, &root, command);
            let _ = sender.send(result);
            ctx.request_repaint();
        });
        self.error = None;
        self.sync_job = Some(SyncJob {
            view_id,
            kind,
            receiver,
        });
    }

    /// Applies a finished fetch or pull. A result for a repository that is no
    /// longer on screen is dropped, including when the user went Home.
    fn poll_sync(&mut self) {
        let Some(job) = &self.sync_job else {
            return;
        };
        let result = match job.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                Err("The Git command stopped unexpectedly. Try again.".to_owned())
            }
        };
        let Some(job) = self.sync_job.take() else {
            return;
        };
        let Screen::Repo(repo) = &mut self.screen else {
            return;
        };
        if repo.view_id != job.view_id {
            return;
        }
        match result {
            Ok(refreshed) => {
                let switched = job.kind == SyncKind::Checkout;
                apply_refresh(repo, refreshed);
                if switched {
                    for listed in &mut repo.branches {
                        if listed.branch.current {
                            listed.checked = true;
                        }
                    }
                }
                self.error = None;
            }
            Err(message) => self.error = Some(message),
        }
    }

    fn poll_graph(&mut self) {
        if !matches!(self.screen, Screen::Repo(_)) {
            if self.graph_load.is_some() {
                self.graph_generation = self.graph_generation.wrapping_add(1);
                self.graph_load = None;
            }
            return;
        }

        let result = {
            let Some(load) = &self.graph_load else {
                return;
            };
            match load.receiver.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    Err("The history graph stopped unexpectedly. Try again.".to_owned())
                }
            }
        };
        let Some(load) = self.graph_load.take() else {
            return;
        };
        if load.generation != self.graph_generation {
            return;
        }

        let mut clear_error = false;
        let mut failure = None;
        if let Screen::Repo(repo) = &mut self.screen
            && repo.view_id == load.view_id
            && selected_tips(&repo.branches, repo.detached_oid.as_deref()) == load.selection
        {
            match result {
                Ok(graph) => {
                    repo.graph = graph;
                    repo.graph_selection = load.selection.clone();
                    repo.graph_requested = load.selection;
                    clear_error = true;
                }
                Err(message) => {
                    repo.graph_requested = load.selection;
                    failure = Some(message);
                }
            }
        }
        if clear_error {
            self.error = None;
        }
        if let Some(message) = failure {
            self.error = Some(message);
        }
    }

    fn sync_graph(&mut self, ctx: &egui::Context) {
        enum Next {
            Ignore,
            Drop,
            Clear,
            Start(Vec<GraphTip>),
        }

        let next = match &self.screen {
            Screen::Repo(repo) => {
                let desired = selected_tips(&repo.branches, repo.detached_oid.as_deref());
                if desired == repo.graph_selection {
                    Next::Ignore
                } else if desired.is_empty() {
                    Next::Clear
                } else if self
                    .graph_load
                    .as_ref()
                    .is_some_and(|load| load.selection == desired)
                    || (desired == repo.graph_requested && self.graph_load.is_none())
                {
                    Next::Ignore
                } else {
                    Next::Start(desired)
                }
            }
            Screen::Home => {
                if self.graph_load.is_some() {
                    Next::Drop
                } else {
                    Next::Ignore
                }
            }
        };

        match next {
            Next::Ignore => {}
            Next::Drop => {
                self.graph_generation = self.graph_generation.wrapping_add(1);
                self.graph_load = None;
            }
            Next::Clear => {
                self.graph_generation = self.graph_generation.wrapping_add(1);
                self.graph_load = None;
                if let Screen::Repo(repo) = &mut self.screen {
                    repo.graph = HistoryGraph::default();
                    repo.graph_selection.clear();
                    repo.graph_requested.clear();
                }
            }
            Next::Start(tips) => self.start_graph_load(ctx, tips),
        }
    }

    fn start_graph_load(&mut self, ctx: &egui::Context, tips: Vec<GraphTip>) {
        let (git, root, view_id) = {
            let Screen::Repo(repo) = &self.screen else {
                return;
            };
            (repo.git.clone(), repo.info.root.clone(), repo.view_id)
        };
        self.graph_generation = self.graph_generation.wrapping_add(1);
        let generation = self.graph_generation;
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        let requested = tips.clone();
        thread::spawn(move || {
            let result = reload_graph(&git, &root, &requested);
            let _ = sender.send(result);
            ctx.request_repaint();
        });
        self.graph_load = Some(GraphLoad {
            generation,
            view_id,
            selection: tips,
            receiver,
        });
    }
}

fn reload_graph(git: &Git, root: &Path, tips: &[GraphTip]) -> Result<HistoryGraph, String> {
    git::branch_graph(git, root, tips, HISTORY_LIMIT).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, mpsc};

    use cthulhu_git::git::HistoryGraph;
    use cthulhu_git::settings::HistoryColumns;
    use eframe::egui;
    use serde_json::json;
    use tempfile::TempDir;

    use super::avatars::AvatarService;
    use super::test_support::{absent_git, commit, listed, opened};
    use super::{Action, CthulhuApp, GraphLoad, Opening, Screen, Startup, selected_tips};

    /// The app as it starts on `settings_path`, with a git that is installed nowhere.
    fn app(ctx: &egui::Context, settings_path: Option<PathBuf>) -> CthulhuApp {
        CthulhuApp::new(
            ctx,
            Startup {
                command_line: None,
                settings_path,
                avatars: |_| AvatarService::disabled(),
                find_git: Arc::new(|| Ok(absent_git())),
            },
        )
    }

    #[test]
    fn checking_main_makes_it_the_principal_lane() {
        let mut branches = vec![listed("feature", true), listed("main", false)];
        let alone = selected_tips(&branches, None);
        assert_eq!(alone.len(), 1);
        assert!(alone[0].principal);
        assert!(alone[0].current);
        assert_eq!(alone[0].name, "feature");

        branches[1].checked = true;
        let both = selected_tips(&branches, None);
        assert_eq!(both.len(), 2);
        assert!(
            both.iter()
                .any(|tip| tip.name == "main" && tip.principal && tip.color == 0 && !tip.current)
        );
        assert!(
            both.iter()
                .any(|tip| tip.name == "feature" && !tip.principal && tip.current)
        );
    }

    /// How the settings file stands when the app starts.
    #[derive(Debug, Clone, Copy)]
    enum Start {
        Missing,
        Bytes(&'static [u8]),
        Directory,
        NoHomeFolder,
    }

    /// Each action that changes a saved setting, in order, and fields the
    /// file holds after it.
    fn persisting_actions() -> [(Action, serde_json::Value); 7] {
        let columns = HistoryColumns {
            branch: 5,
            graph: 300,
            ..HistoryColumns::default()
        };
        [
            (
                Action::ToggleBranchesSidebar,
                json!({ "history_sidebar_hidden": true }),
            ),
            (
                Action::ToggleDetailSidebar,
                json!({ "detail_sidebar_hidden": true }),
            ),
            // Showing the terminal opens the sidebar that holds it.
            (
                Action::ToggleTerminal,
                json!({ "terminal_hidden": false, "detail_sidebar_hidden": false }),
            ),
            (
                Action::SetUiFont(Some("inter".to_owned())),
                json!({ "ui_font": "inter" }),
            ),
            (
                Action::SetTerminalFont(Some("obel-mono".to_owned())),
                json!({ "terminal_font": "obel-mono" }),
            ),
            // A width out of range goes back to its default.
            (
                Action::SetHistoryColumns(columns),
                json!({ "history_columns": {
                    "branch": 180, "graph": 300, "hash": 110, "message": 370, "author": 200
                } }),
            ),
            (
                Action::SetAuthorAvatars(false),
                json!({ "author_avatars": false }),
            ),
        ]
    }

    /// A file that could not be read is the user's: no later action replaces
    /// it with defaults. A missing file counts as read.
    #[test]
    fn settings_are_written_only_after_a_good_load() {
        // (start, loaded)
        const STARTS: [(Start, bool); 5] = [
            (Start::Missing, true),
            (Start::Bytes(b"{}"), true),
            (Start::Bytes(b"{"), false),
            (Start::Directory, false),
            (Start::NoHomeFolder, false),
        ];
        let ctx = egui::Context::default();
        for (start, loaded) in STARTS {
            let dir = TempDir::new().expect("temp dir");
            let path = dir.path().join("settings.json");
            match start {
                Start::Bytes(bytes) => fs::write(&path, bytes).expect("seed settings"),
                Start::Directory => fs::create_dir(&path).expect("directory"),
                Start::Missing | Start::NoHomeFolder => {}
            }
            let home = !matches!(start, Start::NoHomeFolder);
            let mut app = app(&ctx, home.then(|| path.clone()));
            assert_eq!(app.loaded_ok, loaded, "{start:?}");
            assert_eq!(app.error.is_none(), loaded, "{start:?}: {:?}", app.error);
            let error = app.error.clone();
            for (action, fields) in persisting_actions() {
                let name = format!("{action:?}");
                let before = fs::read(&path).ok();
                app.apply_action(&ctx, action);
                let after = fs::read(&path).ok();
                if !loaded {
                    assert_eq!(after, before, "{start:?}: {name} wrote the file");
                    continue;
                }
                let saved: serde_json::Value = serde_json::from_slice(&after.unwrap_or_default())
                    .unwrap_or_else(|error| panic!("{start:?}: {name}: {error}"));
                for (field, value) in fields.as_object().expect("fields") {
                    assert_eq!(&saved[field], value, "{start:?}: {name} saved {field}");
                }
            }
            assert_eq!(app.error, error, "{start:?}");
        }
    }

    /// An open waits for its worker. A worker that dies is an error on Home.
    #[test]
    fn an_open_waits_for_its_worker() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx, None);
        let (sender, receiver) = mpsc::channel();
        let path = PathBuf::from("/repos/slow");
        app.opening = Some(Opening { path, receiver });
        app.poll_opening(&ctx);
        assert!(app.opening.is_some(), "the open ended before its worker");
        drop(sender);
        app.poll_opening(&ctx);
        assert!(app.opening.is_none(), "a dead worker left the open running");
        let error = app.error.as_deref();
        assert_eq!(error, Some("Opening stopped unexpectedly. Try again."));
        assert!(matches!(app.screen, Screen::Home));
    }

    /// What happens on screen while the history worker reads the graph.
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Meanwhile {
        Nothing,
        OpenAnother,
        GoHome,
        AnotherView,
        UncheckFeature,
        UncheckAll,
    }

    /// How the worker ends.
    #[derive(Debug, Clone, Copy)]
    enum Worker {
        Sends,
        Fails,
        Dies,
        Busy,
    }

    /// The drawn graph after the next frame.
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Graph {
        Kept,
        Applied,
        Cleared,
    }

    /// The history load after the next frame.
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Load {
        Idle,
        Same,
        Restarted,
    }

    const EARLIER: &str = "an earlier error";
    const FAILED: &str = "fatal: the log failed";
    const STOPPED: &str = "The history graph stopped unexpectedly. Try again.";

    /// Meanwhile, worker, loading, graph, error, requested, load.
    #[rustfmt::skip]
    type GraphCase = (Meanwhile, Worker, bool, Graph, Option<&'static str>, bool, Load);

    /// The repository shows the graph of `main`; the user has just checked
    /// `feature`, and the worker is reading `main` and `feature`.
    #[test]
    fn a_graph_result_lands_only_on_the_selection_that_asked() {
        #[rustfmt::skip]
        const CASES: [GraphCase; 9] = [
            (Meanwhile::Nothing, Worker::Sends, true, Graph::Applied, None, true, Load::Idle),
            // A failure is not asked again until the checkboxes change.
            (Meanwhile::Nothing, Worker::Fails, true, Graph::Kept, Some(FAILED), true, Load::Idle),
            (Meanwhile::Nothing, Worker::Dies, true, Graph::Kept, Some(STOPPED), true, Load::Idle),
            // The same selection is not asked twice while it is in flight.
            (Meanwhile::Nothing, Worker::Busy, true, Graph::Kept, Some(EARLIER), false, Load::Same),
            (Meanwhile::OpenAnother, Worker::Sends, false, Graph::Kept, None, false, Load::Restarted),
            (Meanwhile::GoHome, Worker::Sends, false, Graph::Kept, None, false, Load::Idle),
            (Meanwhile::AnotherView, Worker::Sends, false, Graph::Kept, Some(EARLIER), false, Load::Restarted),
            (Meanwhile::UncheckFeature, Worker::Sends, true, Graph::Kept, Some(EARLIER), false, Load::Idle),
            (Meanwhile::UncheckAll, Worker::Sends, true, Graph::Cleared, Some(EARLIER), false, Load::Idle),
        ];
        let ctx = egui::Context::default();
        for (meanwhile, worker, loading, graph, error, requested, load) in CASES {
            let case = format!("{meanwhile:?}, then the worker {worker:?}");
            let dir = TempDir::new().expect("temp dir");
            let settings = dir.path().join("settings.json");
            let mut app = app(&ctx, Some(settings.clone()));
            let last = Some(PathBuf::from("/repos/history"));
            app.settings.last_repository.clone_from(&last);
            app.error = Some(EARLIER.to_owned());
            let mut repo = opened("history", commit("one"));
            let mut feature = listed("feature", false);
            feature.checked = true;
            feature.branch.oid = Some("b".repeat(40));
            repo.branches.push(feature);
            let selection = selected_tips(&repo.branches, None);
            let (sender, receiver) = mpsc::channel();
            app.graph_generation += 1;
            let generation = app.graph_generation;
            app.graph_load = Some(GraphLoad {
                generation,
                view_id: repo.view_id,
                selection: selection.clone(),
                receiver,
            });
            match meanwhile {
                Meanwhile::AnotherView => repo.view_id = egui::Id::new("another view"),
                Meanwhile::UncheckFeature => repo.branches[1].checked = false,
                Meanwhile::UncheckAll => {
                    for branch in &mut repo.branches {
                        branch.checked = false;
                    }
                }
                _ => {}
            }
            app.screen = Screen::Repo(Box::new(repo));
            match meanwhile {
                Meanwhile::OpenAnother => {
                    app.apply_action(&ctx, Action::Open(PathBuf::from("/repos/another")));
                }
                Meanwhile::GoHome => {
                    // The frame that handles Home ends with `sync_graph`.
                    app.apply_action(&ctx, Action::Home);
                    app.sync_graph(&ctx);
                }
                _ => {}
            }
            assert_eq!(app.history_loading(), loading, "{case}");

            let result = match worker {
                Worker::Sends => Some(Ok(HistoryGraph {
                    truncated: true,
                    ..HistoryGraph::default()
                })),
                Worker::Fails => Some(Err(FAILED.to_owned())),
                Worker::Dies | Worker::Busy => None,
            };
            if let Some(result) = result {
                // A dropped load has no receiver left.
                let _ = sender.send(result);
            }
            let _running = matches!(worker, Worker::Busy).then_some(sender);
            app.poll_graph();
            app.sync_graph(&ctx);

            assert_eq!(app.error.as_deref(), error, "{case}");
            let now = match &app.graph_load {
                None => Load::Idle,
                Some(running) if running.generation == generation => Load::Same,
                Some(_) => Load::Restarted,
            };
            assert_eq!(now, load, "{case}");
            match &app.screen {
                Screen::Repo(repo) => {
                    let shown = if repo.graph.truncated {
                        Graph::Applied
                    } else if repo.graph.rows.is_empty() {
                        Graph::Cleared
                    } else {
                        Graph::Kept
                    };
                    assert_eq!(shown, graph, "{case}");
                    assert_eq!(repo.graph_requested == selection, requested, "{case}");
                }
                Screen::Home => assert_eq!(meanwhile, Meanwhile::GoHome, "{case}"),
            }
            // Leaving the repository keeps it for the next launch, and nothing
            // here is a reason to write the settings.
            assert_eq!(app.settings.last_repository, last, "{case}");
            assert!(!settings.exists(), "{case}: settings were written");
        }
    }
}
