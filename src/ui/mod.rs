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
mod repo_view;
mod settings;
mod task;
mod terminal;
#[cfg(test)]
mod test_support;
pub mod theme;
mod widgets;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use cthulhu_git::git::{
    self, Branch, CommitDetail, Git, GitError, GraphCandidate, GraphTip, Head, HistoryGraph,
    RepoInfo,
};
use cthulhu_git::github::{self as gh, GitHubRepo};
use cthulhu_git::settings::{HistoryColumns, Settings};
use eframe::egui;

use avatars::AvatarService;
use github_auth::GitHubAuth;
use task::{Stopped, Task};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncKind {
    Fetch,
    Pull,
    Checkout,
}

/// The work a screen starts belongs to it, so leaving the screen drops that
/// work and a late result cannot land on another screen.
enum Screen {
    Home(HomeScan),
    Repo {
        repo: Box<OpenedRepo>,
        graph_load: Option<GraphLoad>,
        sync: Option<SyncJob>,
    },
}

struct Opening {
    path: PathBuf,
    task: Task<Result<Refreshed, GitError>>,
}

/// Branch and latest commit of each recent repository, read one after
/// another on one thread. Dropping the scan disconnects that thread, which
/// stops once the repository it is reading is done.
#[derive(Default)]
struct HomeScan {
    /// Missing means the scan has not reported that path yet.
    glances: HashMap<PathBuf, home::Glance>,
    receiver: Option<Receiver<(PathBuf, home::Glance)>>,
}

impl HomeScan {
    fn start(ctx: &egui::Context, find_git: &Arc<GitFinder>, paths: Vec<PathBuf>) -> Self {
        if paths.is_empty() {
            return Self::default();
        }
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        let find_git = Arc::clone(find_git);
        thread::spawn(move || scan_glances(&find_git, paths, &sender, &ctx));
        Self {
            glances: HashMap::new(),
            receiver: Some(receiver),
        }
    }

    fn poll(&mut self) {
        if let Some(receiver) = &self.receiver {
            self.glances.extend(receiver.try_iter());
        }
    }
}

pub struct CthulhuApp {
    settings: Settings,
    /// Where `save_settings` writes. `None` without a home folder, or when the
    /// file could not be read: writing then would replace the user's file
    /// with defaults.
    save_path: Option<PathBuf>,
    screen: Screen,
    opening: Option<Opening>,
    /// The folder dialog: the chosen folder, or `None` when it was cancelled.
    picker: Option<Task<Option<PathBuf>>>,
    error: Option<String>,
    opened_count: u64,
    /// Open settings cover the current screen; a repository underneath stays
    /// open. The chosen page is kept for the session in egui's memory.
    settings_screen: Option<SettingsScreen>,
    fonts: fonts::FontService,
    avatars: AvatarService,
    github: GitHubAuth,
    find_git: Arc<GitFinder>,
    /// `git --version` for the About page, once the probe started by the
    /// first visit to Settings has answered.
    git_version: Option<String>,
    git_probe: Option<Task<Result<String, GitError>>>,
}

#[derive(Default)]
struct SettingsScreen {
    /// Shell drawn on the Terminal page. Separate from the repository
    /// terminal so the preview size does not resize that session. Not saved.
    preview: Option<terminal::Terminal>,
}

/// Downloaded avatars, in the per-user cache folder (the settings are in the
/// config folder).
fn avatar_cache_dir() -> Option<PathBuf> {
    cthulhu_git::settings::project_dirs().map(|dirs| dirs.cache_dir().join("avatars"))
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
    /// Finds git for opens, the recent list and the About page.
    /// [`Git::discover`] reads `CTHULHU_GIT` and `PATH`.
    pub find_git: fn() -> Result<Git, GitError>,
}

/// The git found first, kept so each open does not search and probe again.
/// A git that a command could not start (moved or uninstalled) is looked
/// for again on the next find.
struct GitFinder {
    find: fn() -> Result<Git, GitError>,
    found: Mutex<Option<Git>>,
}

impl GitFinder {
    fn new(find: fn() -> Result<Git, GitError>) -> Self {
        Self {
            find,
            found: Mutex::new(None),
        }
    }

    /// The lock is not held while searching, so the UI thread never waits
    /// for a search running on a worker.
    fn git(&self) -> Result<Git, GitError> {
        if let Some(git) = self.found().clone() {
            return Ok(git);
        }
        let git = (self.find)()?;
        *self.found() = Some(git.clone());
        Ok(git)
    }

    fn forget_if_gone(&self, error: &GitError) {
        if matches!(error, GitError::Spawn(_)) {
            *self.found() = None;
        }
    }

    /// The text the UI shows for `error`.
    fn describe(&self, error: GitError) -> String {
        self.forget_if_gone(&error);
        error.to_string()
    }

    /// A finished git job as the screen shows it: the error as text, and
    /// `stopped` when the worker died without an answer.
    fn outcome<T>(
        &self,
        done: Result<Result<T, GitError>, Stopped>,
        stopped: &str,
    ) -> Result<T, String> {
        match done {
            Ok(result) => result.map_err(|error| self.describe(error)),
            Err(Stopped) => Err(stopped.to_owned()),
        }
    }

    fn found(&self) -> std::sync::MutexGuard<'_, Option<Git>> {
        // A panic elsewhere cannot leave a half-written `Option<Git>` behind.
        self.found.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Startup {
    /// The user's settings file and avatars from the network.
    pub fn system(command_line: Option<PathBuf>) -> Self {
        Self {
            command_line,
            settings_path: Settings::default_path(),
            avatars: |enabled| AvatarService::new(avatar_cache_dir(), enabled),
            find_git: Git::discover,
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
        let (settings, save_path, error) = load_settings(settings_path);
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
            save_path,
            screen: Screen::Home(HomeScan::default()),
            opening: None,
            picker: None,
            error,
            opened_count: 0,
            settings_screen: None,
            fonts,
            avatars,
            github,
            find_git: Arc::new(GitFinder::new(find_git)),
            git_version: None,
            git_probe: None,
        };
        if let Some(path) = first_open {
            // The window stays hidden until its first frame is painted, so
            // opening here makes that frame the repository or the home
            // screen with the error, never a home screen that then flips.
            let result = load(&app.find_git, &path).map_err(|error| app.find_git.describe(error));
            app.finish_open(ctx, path, result);
        }
        // Home, with no first open or a failed one, scans from the start.
        if let Screen::Home(scan) = &mut app.screen {
            *scan = HomeScan::start(ctx, &app.find_git, app.settings.recent_repositories.clone());
        }
        app
    }

    /// One `git --version` the first time Settings opens. Later visits reuse it.
    fn ensure_git_version(&mut self, ctx: &egui::Context) {
        if self.git_version.is_some() || self.git_probe.is_some() {
            return;
        }
        let find_git = Arc::clone(&self.find_git);
        self.git_probe = Some(Task::spawn(ctx, move || {
            find_git.git().map(|git| git.version.to_string())
        }));
    }

    fn poll_git_version(&mut self) {
        let Some(done) = self.git_probe.as_ref().and_then(Task::poll) else {
            return;
        };
        self.git_probe = None;
        self.git_version = Some(match done {
            Ok(Ok(version)) => version,
            Ok(Err(_)) | Err(Stopped) => "Not found".to_owned(),
        });
    }

    /// Home, with a scan of the recent list. Home already on screen keeps
    /// the scan it has.
    fn go_home(&mut self, ctx: &egui::Context) {
        if !matches!(self.screen, Screen::Home(_)) {
            let recent = self.settings.recent_repositories.clone();
            self.screen = Screen::Home(HomeScan::start(ctx, &self.find_git, recent));
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

    /// On a worker: the window is visible now, and waiting would freeze it.
    fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        let find_git = Arc::clone(&self.find_git);
        let worker_path = path.clone();
        let task = Task::spawn(ctx, move || load(&find_git, &worker_path));
        self.opening = Some(Opening { path, task });
    }

    fn poll_opening(&mut self, ctx: &egui::Context) {
        let Some(opening) = self.opening.take() else {
            return;
        };
        let Some(done) = opening.task.poll() else {
            self.opening = Some(opening);
            return;
        };
        let result = self
            .find_git
            .outcome(done, "Opening stopped unexpectedly. Try again.");
        self.finish_open(ctx, opening.path, result);
    }

    /// Installs the repository and starts reading its history graph, or goes
    /// Home when opening failed.
    fn finish_open(
        &mut self,
        ctx: &egui::Context,
        path: PathBuf,
        result: Result<Refreshed, String>,
    ) {
        match result {
            Ok(refreshed) => {
                self.error = None;
                self.settings.remember_repository(&refreshed.info.root);
                self.save_settings();
                self.opened_count += 1;
                let repo = Box::new(OpenedRepo {
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
                    view_id: egui::Id::unique(("repo-view", self.opened_count)),
                    terminal: None,
                    github: refreshed.github,
                    hover: hover::HoverCache::default(),
                    diffs: changes::DiffCache::default(),
                    changes: None,
                });
                // No tips means an unborn repository, so the center stays
                // "No commits yet."
                let tips = selected_tips(&repo.branches, repo.detached_oid.as_deref());
                let graph_load = (!tips.is_empty()).then(|| GraphLoad::start(ctx, &repo, tips));
                self.screen = Screen::Repo {
                    repo,
                    graph_load,
                    sync: None,
                };
            }
            Err(message) => {
                self.error = Some(message);
                if self.settings.last_repository.as_deref() == Some(path.as_path()) {
                    self.settings.forget_last_repository();
                    self.save_settings();
                }
                self.go_home(ctx);
            }
        }
    }

    fn poll_picker(&mut self, ctx: &egui::Context) {
        let Some(done) = self.picker.as_ref().and_then(Task::poll) else {
            return;
        };
        self.picker = None;
        // A dialog that failed opens nothing, like a cancelled one.
        if let Ok(Some(path)) = done {
            self.open(ctx, path);
        }
    }

    /// A failed save keeps the app running; the message joins any current error.
    fn save_settings(&mut self) {
        let Some(path) = &self.save_path else {
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

    /// A folder dialog or an open is running, so another one cannot start.
    fn busy(&self) -> bool {
        self.opening.is_some() || self.picker.is_some()
    }
}

/// The settings at `path` (`None`: no home folder), where later saves may
/// write them, and the error to show.
///
/// A missing file is the defaults and is written on the first change. A file
/// that could not be read or parsed is the user's: the window shows the
/// error and keeps the defaults in memory, and nothing is ever written over
/// it, including recents, the theme, and `github_token_in_keyring`.
fn load_settings(path: Option<PathBuf>) -> (Settings, Option<PathBuf>, Option<String>) {
    let Some(path) = path else {
        let error = "No home folder was found, so settings will not be saved.";
        return (Settings::default(), None, Some(error.to_owned()));
    };
    match Settings::load_from(&path) {
        Ok(settings) => (settings, Some(path), None),
        Err(error) => (Settings::default(), None, Some(error.to_string())),
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
        if self.settings_screen.is_some() {
            self.ensure_git_version(ui.ctx());
        }
        self.poll_git_version();
        if let Screen::Home(scan) = &mut self.screen {
            scan.poll();
        }

        let syncing = self.sync_for_open_repo();
        let history_loading = self.history_loading();
        let busy = self.busy();
        let actions = if let Some(settings_screen) = &mut self.settings_screen {
            // The repository stays underneath. Keep the shell alive and take
            // keystrokes away from the hidden terminal.
            if let Screen::Repo { repo, .. } = &mut self.screen {
                terminal::service(ui.ctx(), &mut repo.terminal);
                terminal::surrender_focus(ui.ctx(), repo.view_id);
            }
            let cwd = match &self.screen {
                Screen::Repo { repo, .. } => Some(repo.info.root.clone()),
                Screen::Home(_) => None,
            };
            terminal::service(ui.ctx(), &mut settings_screen.preview);
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
                busy,
                about: settings::AboutInfo::current(git_label),
            };
            settings::show(
                ui,
                self.settings.ui_font.as_deref(),
                self.settings.terminal_font.as_deref(),
                &mut settings_screen.preview,
                cwd.as_deref(),
                &view,
                &mut self.github,
            )
        } else {
            match &mut self.screen {
                Screen::Home(scan) => home::show(
                    ui,
                    &home::HomeState {
                        recent: &self.settings.recent_repositories,
                        glances: &scan.glances,
                        busy,
                        opening: self.opening.as_ref().map(|opening| opening.path.as_path()),
                        error: self.error.as_deref(),
                    },
                )
                .into_iter()
                .collect(),
                Screen::Repo { repo, .. } => repo_view::show(
                    ui,
                    repo,
                    &repo_view::ViewState {
                        error: self.error.as_deref(),
                        branches_open: !self.settings.branches_sidebar_hidden,
                        detail_open: !self.settings.detail_sidebar_hidden,
                        terminal_open: !self.settings.terminal_hidden,
                        syncing,
                        columns: self.settings.history_columns,
                        history_loading,
                        busy,
                    },
                    &mut self.avatars,
                ),
            }
        };

        for action in actions {
            match action {
                // The folder dialog needs the window's frame.
                Action::Browse => {
                    if !self.busy() {
                        self.error = None;
                        self.picker =
                            Some(folder_picker::open(ui.ctx(), frame, self.browse_start()));
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
                self.settings_screen = None;
                self.go_home(ctx);
            }
            Action::OpenSettings => {
                self.settings_screen.get_or_insert_default();
            }
            Action::CloseSettings => {
                self.settings_screen = None;
            }
            Action::ToggleBranchesSidebar => {
                self.settings.branches_sidebar_hidden = !self.settings.branches_sidebar_hidden;
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

/// One repository at a time. A dropped receiver stops the loop once the
/// repository being read is done; each of its git commands keeps its own
/// time limit.
fn scan_glances(
    find_git: &GitFinder,
    paths: Vec<PathBuf>,
    sender: &Sender<(PathBuf, home::Glance)>,
    ctx: &egui::Context,
) {
    let git = find_git.git().ok();
    for path in paths {
        let glance = match &git {
            Some(git) => match git::glance(git, &path) {
                Ok(ready) => home::Glance::Ready(ready),
                Err(error) => {
                    find_git.forget_if_gone(&error);
                    home::Glance::Unreadable
                }
            },
            None => home::Glance::Unreadable,
        };
        if sender.send((path, glance)).is_err() {
            break;
        }
        ctx.request_repaint();
    }
}

/// Repository chrome. The history graph is read later, so a log failure
/// stays on the repository screen and keeps `last_repository`.
fn load(find_git: &GitFinder, path: &Path) -> Result<Refreshed, GitError> {
    reload_repo(&find_git.git()?, path)
}

/// HEAD, branches and the latest commit, without rebuilding the graph.
fn reload_repo(git: &Git, path: &Path) -> Result<Refreshed, GitError> {
    let info = git::inspect(git, path)?;
    let branches = git::branches(git, &info.root, &info.head)?;
    let latest = git::latest_commit(git, &info.root, &info.head)?;
    let detached_oid = match &info.head {
        Head::Detached { .. } => Some(git::head_commit_oid(git, &info.root)?),
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
    kind: SyncKind,
    task: Task<Result<Refreshed, GitError>>,
}

fn run_sync(git: &Git, root: &Path, command: SyncCommand) -> Result<Refreshed, GitError> {
    match &command {
        SyncCommand::Fetch(branch) => git::fetch(git, root, branch)?,
        SyncCommand::Pull => git::pull_ff_only(git, root)?,
        SyncCommand::Checkout(name) => git::switch_branch(git, root, name)?,
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

/// The history graph being read for `selection`.
struct GraphLoad {
    selection: Vec<GraphTip>,
    task: Task<Result<HistoryGraph, GitError>>,
}

impl GraphLoad {
    fn start(ctx: &egui::Context, repo: &OpenedRepo, selection: Vec<GraphTip>) -> Self {
        let (git, root, tips) = (repo.git.clone(), repo.info.root.clone(), selection.clone());
        let task = Task::spawn(ctx, move || {
            git::branch_graph(&git, &root, &tips, HISTORY_LIMIT)
        });
        Self { selection, task }
    }
}

impl CthulhuApp {
    fn history_loading(&self) -> bool {
        matches!(
            self.screen,
            Screen::Repo {
                graph_load: Some(_),
                ..
            }
        )
    }

    fn sync_for_open_repo(&self) -> Option<SyncKind> {
        match &self.screen {
            Screen::Repo { sync, .. } => sync.as_ref().map(|job| job.kind),
            Screen::Home(_) => None,
        }
    }

    fn start_sync(&mut self, ctx: &egui::Context, command: SyncCommand) {
        let Screen::Repo { repo, sync, .. } = &mut self.screen else {
            return;
        };
        if sync.is_some() {
            return;
        }
        let root = repo.info.root.clone();
        let git = repo.git.clone();
        let kind = match &command {
            SyncCommand::Fetch(_) => SyncKind::Fetch,
            SyncCommand::Pull => SyncKind::Pull,
            SyncCommand::Checkout(_) => SyncKind::Checkout,
        };
        let task = Task::spawn(ctx, move || run_sync(&git, &root, command));
        *sync = Some(SyncJob { kind, task });
        self.error = None;
    }

    /// Applies a finished fetch, pull or checkout.
    fn poll_sync(&mut self) {
        let Screen::Repo { repo, sync, .. } = &mut self.screen else {
            return;
        };
        let Some(job) = sync.take() else {
            return;
        };
        let Some(done) = job.task.poll() else {
            *sync = Some(job);
            return;
        };
        match self
            .find_git
            .outcome(done, "The Git command stopped unexpectedly. Try again.")
        {
            Ok(refreshed) => {
                apply_refresh(repo, refreshed);
                if job.kind == SyncKind::Checkout {
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

    /// A graph read for checkboxes that changed since is dropped;
    /// [`Self::sync_graph`] asks for the new selection.
    fn poll_graph(&mut self) {
        let Screen::Repo {
            repo, graph_load, ..
        } = &mut self.screen
        else {
            return;
        };
        let Some(load) = graph_load.take() else {
            return;
        };
        let Some(done) = load.task.poll() else {
            *graph_load = Some(load);
            return;
        };
        if selected_tips(&repo.branches, repo.detached_oid.as_deref()) != load.selection {
            return;
        }
        match self
            .find_git
            .outcome(done, "The history graph stopped unexpectedly. Try again.")
        {
            Ok(graph) => {
                repo.graph = graph;
                repo.graph_selection = load.selection.clone();
                repo.graph_requested = load.selection;
                self.error = None;
            }
            Err(message) => {
                repo.graph_requested = load.selection;
                self.error = Some(message);
            }
        }
    }

    /// Reads the graph again when the checkboxes changed. Neither the
    /// selection in flight nor the last one that failed is asked again.
    fn sync_graph(&mut self, ctx: &egui::Context) {
        let Screen::Repo {
            repo, graph_load, ..
        } = &mut self.screen
        else {
            return;
        };
        let desired = selected_tips(&repo.branches, repo.detached_oid.as_deref());
        if desired == repo.graph_selection {
            return;
        }
        if desired.is_empty() {
            *graph_load = None;
            repo.graph = HistoryGraph::default();
            repo.graph_selection.clear();
            repo.graph_requested.clear();
            return;
        }
        let asked = match graph_load {
            Some(load) => load.selection == desired,
            None => desired == repo.graph_requested,
        };
        if !asked {
            *graph_load = Some(GraphLoad::start(ctx, repo, desired));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::Duration;

    use cthulhu_git::git::{Git, GitError, Head, HistoryGraph, RepoInfo};
    use cthulhu_git::settings::HistoryColumns;
    use eframe::egui;
    use serde_json::json;
    use tempfile::TempDir;

    use super::avatars::AvatarService;
    use super::home::Glance;
    use super::task::Task;
    use super::test_support::{absent_git, commit, listed, opened};
    use super::{
        Action, CthulhuApp, GitFinder, GraphLoad, Opening, Refreshed, Screen, Startup, SyncJob,
        SyncKind, selected_tips,
    };

    /// The app as it starts on `settings_path`, with a git that is installed nowhere.
    fn app(ctx: &egui::Context, settings_path: Option<PathBuf>) -> CthulhuApp {
        CthulhuApp::new(
            ctx,
            Startup {
                command_line: None,
                settings_path,
                avatars: |_| AvatarService::disabled(),
                find_git: || Ok(absent_git()),
            },
        )
    }

    /// Git is looked for once, and again only after a command could not
    /// start it.
    #[test]
    fn git_is_searched_again_only_once_it_cannot_start() {
        static SEARCHES: AtomicUsize = AtomicUsize::new(0);
        fn search() -> Result<Git, GitError> {
            SEARCHES.fetch_add(1, Ordering::SeqCst);
            Ok(absent_git())
        }
        let rejected = GitError::OutputLimit {
            command: "git log".to_owned(),
        };
        let gone = GitError::Spawn(io::ErrorKind::NotFound.into());
        // (the error a command returned before this find, searches so far)
        let cases = [(None, 1), (None, 1), (Some(rejected), 1), (Some(gone), 2)];
        let finder = GitFinder::new(search);
        for (step, (error, searches)) in cases.into_iter().enumerate() {
            if let Some(error) = error {
                finder.describe(error);
            }
            assert!(finder.git().is_ok(), "step {step}");
            assert_eq!(SEARCHES.load(Ordering::SeqCst), searches, "step {step}");
        }
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
            assert_eq!(app.save_path.is_some(), loaded, "{start:?}");
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

    /// The folder chosen in the dialog opens, and an open waits for its
    /// worker. Either one keeps the app busy. A worker that dies is an error
    /// on Home.
    #[test]
    fn an_open_waits_for_its_worker() {
        let ctx = egui::Context::default();
        let mut app = app(&ctx, None);
        let path = PathBuf::from("/repos/slow");
        app.picker = Some(Task::ready(Some(path.clone())));
        assert!(app.busy(), "the dialog is open");
        app.poll_picker(&ctx);
        let opened = app.opening.as_ref().map(|opening| &opening.path);
        assert_eq!(opened, Some(&path), "the chosen folder");
        assert!(app.picker.is_none(), "the dialog is still open");
        let (task, sender) = Task::pending();
        app.opening = Some(Opening { path, task });
        app.poll_opening(&ctx);
        assert!(app.opening.is_some(), "the open ended before its worker");
        assert!(app.busy(), "opening");
        drop(sender);
        app.poll_opening(&ctx);
        assert!(app.opening.is_none(), "a dead worker left the open running");
        let error = app.error.as_deref();
        assert_eq!(error, Some("Opening stopped unexpectedly. Try again."));
        assert!(matches!(app.screen, Screen::Home(_)));
    }

    /// The About page's git version and the recent cards' facts come from
    /// workers.
    #[test]
    fn workers_fill_the_about_page_and_the_recent_cards() {
        let ctx = egui::Context::default();
        let dir = TempDir::new().expect("temp dir");
        let settings = dir.path().join("settings.json");
        let gone = dir.path().join("gone");
        let seed = json!({ "recent_repositories": [gone] }).to_string();
        fs::write(&settings, seed).expect("seed settings");
        let mut app = app(&ctx, Some(settings));
        app.ensure_git_version(&ctx);
        let reported = (0..500).any(|_| {
            app.poll_git_version();
            let Screen::Home(scan) = &mut app.screen else {
                return false;
            };
            scan.poll();
            let done = app.git_version.is_some() && scan.glances.contains_key(&gone);
            if !done {
                thread::sleep(Duration::from_millis(10));
            }
            done
        });
        assert!(reported, "the workers never reported");
        assert_eq!(app.git_version.as_deref(), Some("2.43.0"));
        let Screen::Home(scan) = &app.screen else {
            panic!("not on Home");
        };
        assert!(matches!(scan.glances.get(&gone), Some(Glance::Unreadable)));
    }

    /// How a sync worker ends.
    #[derive(Debug, Clone, Copy)]
    enum Answer {
        /// The repository, reloaded with this current branch.
        Current(&'static str),
        Fails,
        Dies,
    }

    /// Each open has its own view and starts reading its history at once, so
    /// its first frame says the history is loading. One sync runs at a time;
    /// its result reloads the branches, and only a switch checks the new
    /// current branch.
    #[test]
    fn an_opened_repository_reads_its_history_and_takes_sync_results() {
        const REFUSED: &str = "`git pull` failed (rejected): refused";
        const STOPPED: &str = "The Git command stopped unexpectedly. Try again.";
        // (the job, how it ends, the error shown, `feature` checked after)
        const CASES: [(SyncKind, Answer, Option<&str>, bool); 4] = [
            (SyncKind::Checkout, Answer::Current("feature"), None, true),
            (SyncKind::Fetch, Answer::Current("feature"), None, false),
            (SyncKind::Pull, Answer::Fails, Some(REFUSED), false),
            (SyncKind::Pull, Answer::Dies, Some(STOPPED), false),
        ];
        let refreshed = |current: &str| Refreshed {
            git: absent_git(),
            info: RepoInfo {
                root: PathBuf::from("/repos/sync"),
                name: "sync".to_owned(),
                head: Head::Branch(current.to_owned()),
            },
            branches: ["main", "feature"]
                .map(|name| listed(name, name == current).branch)
                .into(),
            latest: None,
            detached_oid: None,
            github: None,
        };
        let ctx = egui::Context::default();
        let mut app = app(&ctx, None);
        let mut views = Vec::new();
        for (kind, answer, error, checked) in CASES {
            let case = format!("{kind:?} that {answer:?}");
            app.finish_open(&ctx, PathBuf::from("/repos/sync"), Ok(refreshed("main")));
            assert!(app.history_loading(), "{case}: no history read at first");
            app.apply_action(&ctx, Action::Checkout("feature".to_owned()));
            app.apply_action(&ctx, Action::Pull);
            assert_eq!(app.sync_for_open_repo(), Some(SyncKind::Checkout), "{case}");
            let (task, sender) = Task::pending();
            let _ = match answer {
                Answer::Current(name) => sender.send(Ok(refreshed(name))),
                Answer::Fails => sender.send(Err(GitError::Rejected {
                    command: "git pull".to_owned(),
                    reason: "refused".to_owned(),
                })),
                Answer::Dies => Ok(()),
            };
            drop(sender);
            let Screen::Repo { repo, sync, .. } = &mut app.screen else {
                panic!("{case}: not opened: {:?}", app.error);
            };
            views.push(repo.view_id);
            *sync = Some(SyncJob { kind, task });
            app.poll_sync();
            assert_eq!(app.sync_for_open_repo(), None, "{case}");
            assert_eq!(app.error.as_deref(), error, "{case}");
            let Screen::Repo { repo, .. } = &app.screen else {
                panic!("{case}: not on the repository");
            };
            let feature = repo
                .branches
                .iter()
                .find(|listed| listed.branch.name == "feature");
            assert_eq!(
                feature.map(|listed| listed.checked),
                Some(checked),
                "{case}"
            );
        }
        views.dedup();
        assert_eq!(views.len(), CASES.len(), "opens share a view");
    }

    /// What happens on screen while the history worker reads the graph.
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Meanwhile {
        Nothing,
        OpenAnother,
        GoHome,
        CheckTopic,
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
    const FAILED: &str = "`git log` failed (rejected): the log failed";
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
            // The repository on screen keeps its load while another one opens.
            (Meanwhile::OpenAnother, Worker::Sends, true, Graph::Applied, None, true, Load::Idle),
            (Meanwhile::GoHome, Worker::Sends, false, Graph::Kept, None, false, Load::Idle),
            (Meanwhile::CheckTopic, Worker::Busy, true, Graph::Kept, Some(EARLIER), false, Load::Restarted),
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
            match meanwhile {
                Meanwhile::CheckTopic => {
                    let mut topic = listed("topic", false);
                    topic.checked = true;
                    topic.branch.oid = Some("c".repeat(40));
                    repo.branches.push(topic);
                }
                Meanwhile::UncheckFeature => repo.branches[1].checked = false,
                Meanwhile::UncheckAll => {
                    for branch in &mut repo.branches {
                        branch.checked = false;
                    }
                }
                _ => {}
            }
            let (task, sender) = Task::pending();
            let selection_load = GraphLoad {
                selection: selection.clone(),
                task,
            };
            let (repo, graph_load) = (Box::new(repo), Some(selection_load));
            app.screen = Screen::Repo {
                repo,
                graph_load,
                sync: None,
            };
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

            // A dropped load has no receiver left, so a send may fail.
            let _ = match worker {
                Worker::Sends => sender.send(Ok(HistoryGraph {
                    truncated: true,
                    ..HistoryGraph::default()
                })),
                Worker::Fails => sender.send(Err(GitError::Rejected {
                    command: "git log".to_owned(),
                    reason: "the log failed".to_owned(),
                })),
                Worker::Dies | Worker::Busy => Ok(()),
            };
            // Moved in either way, so a dead worker's sender is gone now.
            let sender = (!matches!(worker, Worker::Dies)).then_some(sender);
            app.poll_graph();
            app.sync_graph(&ctx);

            assert_eq!(app.error.as_deref(), error, "{case}");
            // Only the load the test started still listens to its sender.
            let same =
                sender.is_some_and(|sender| sender.send(Ok(HistoryGraph::default())).is_ok());
            let now = match (app.history_loading(), same) {
                (false, _) => Load::Idle,
                (true, true) => Load::Same,
                (true, false) => Load::Restarted,
            };
            assert_eq!(now, load, "{case}");
            match &app.screen {
                Screen::Repo { repo, .. } => {
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
                Screen::Home(_) => assert_eq!(meanwhile, Meanwhile::GoHome, "{case}"),
            }
            // Leaving the repository keeps it for the next launch, and nothing
            // here is a reason to write the settings.
            assert_eq!(app.settings.last_repository, last, "{case}");
            assert!(!settings.exists(), "{case}: settings were written");
        }
    }
}
