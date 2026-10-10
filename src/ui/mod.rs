//! The window: a home screen, a repository view, and a settings screen.
//!
//! On launch, a folder passed on the command line opens first, then the last
//! repository from the settings, otherwise the home screen shows. Settings
//! are described in `docs/SETTINGS.md`, themes in `docs/THEMES.md`.

#[cfg(test)]
mod adversarial_tests;
mod folder_picker;
mod fonts;
mod home;
mod icons;
mod repo_view;
mod settings;
mod terminal;
pub mod theme;
mod widgets;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use cthulhu_git::git::{self, Branch, CommitDetail, Git, GraphTip, Head, HistoryGraph, RepoInfo};
use cthulhu_git::settings::Settings;
use eframe::egui;

use folder_picker::FolderPicker;

/// Newest commits read for the branch graph; the view says when there are more.
const HISTORY_LIMIT: usize = 1000;

pub struct OpenedRepo {
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
}

/// A local branch and whether it is drawn in the history graph.
pub struct ListedBranch {
    pub branch: Branch,
    pub checked: bool,
}

/// What a screen asks the app to do after drawing itself.
pub enum Action {
    Browse,
    Open(PathBuf),
    Home,
    OpenSettings,
    CloseSettings,
    ToggleBranchesSidebar,
    ToggleDetailSidebar,
    ToggleTerminal,
    SetTerminalFont(Option<String>),
    EnsureFontCatalog,
    /// `upstream` is `%(upstream:short)`, for example `origin/main`.
    Fetch(String),
    Pull,
}

/// A fetch or pull running against the open repository.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SyncKind {
    Fetch,
    Pull,
}

enum Screen {
    Home,
    Repo(Box<OpenedRepo>),
}

struct LoadedRepo {
    info: RepoInfo,
    graph: HistoryGraph,
    tips: Vec<GraphTip>,
    detached_oid: Option<String>,
    branches: Vec<Branch>,
    latest: Option<CommitDetail>,
}

type OpenResult = Result<LoadedRepo, String>;

struct Opening {
    path: PathBuf,
    receiver: Receiver<OpenResult>,
}

pub struct CthulhuApp {
    settings: Settings,
    /// `None` when the OS reports no home folder; settings are then not saved.
    settings_path: Option<PathBuf>,
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
}

impl CthulhuApp {
    pub fn new(ctx: &egui::Context, command_line: Option<PathBuf>) -> Self {
        let settings_path = Settings::default_path();
        let mut error = None;
        let settings = match settings_path.as_deref().map(Settings::load_from) {
            Some(Ok(settings)) => settings,
            Some(Err(load_error)) => {
                error = Some(load_error.to_string());
                Settings::default()
            }
            None => {
                error = Some("No home folder was found, so settings will not be saved.".to_owned());
                Settings::default()
            }
        };
        theme::apply(ctx, theme::resolve(settings.theme.as_deref()));

        let startup = command_line.or_else(|| settings.last_repository.clone());
        let mut app = Self {
            settings,
            settings_path,
            screen: Screen::Home,
            opening: None,
            picker: None,
            error,
            opened_count: 0,
            settings_open: false,
            fonts: fonts::FontService::default(),
            font_preview: None,
            graph_load: None,
            graph_generation: 0,
            sync_job: None,
        };
        if let Some(path) = startup {
            app.open(ctx, path);
        }
        app
    }

    /// Git runs on a worker thread: blocking the UI thread would freeze the window.
    fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.graph_generation = self.graph_generation.wrapping_add(1);
        self.graph_load = None;
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        let worker_path = path.clone();
        thread::spawn(move || {
            let _ = sender.send(load(&worker_path));
            ctx.request_repaint();
        });
        self.opening = Some(Opening { path, receiver });
    }

    fn poll_opening(&mut self) {
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

        match result {
            Ok(loaded) => {
                self.error = None;
                self.settings.remember_repository(&loaded.info.root);
                self.save_settings();
                self.opened_count += 1;
                self.screen = Screen::Repo(Box::new(OpenedRepo {
                    info: loaded.info,
                    graph: loaded.graph,
                    graph_selection: loaded.tips.clone(),
                    graph_requested: loaded.tips,
                    detached_oid: loaded.detached_oid,
                    branches: loaded
                        .branches
                        .into_iter()
                        .map(|branch| ListedBranch {
                            branch,
                            checked: true,
                        })
                        .collect(),
                    latest: loaded.latest,
                    view_id: egui::Id::new(("repo-view", self.opened_count)),
                    terminal: None,
                }));
            }
            Err(message) => {
                self.error = Some(message);
                if self.settings.last_repository.as_deref() == Some(opening.path.as_path()) {
                    self.settings.forget_last_repository();
                    self.save_settings();
                }
                self.screen = Screen::Home;
            }
        }
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
    fn save_settings(&mut self) {
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

impl eframe::App for CthulhuApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.poll_opening();
        self.poll_graph();
        self.poll_sync();
        self.poll_picker(ui.ctx());
        self.fonts
            .sync(ui.ctx(), self.settings.terminal_font.as_deref());

        let syncing = self.sync_for_open_repo();
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
            settings::show(
                ui,
                self.settings.terminal_font.as_deref(),
                &self.fonts,
                &mut self.font_preview,
                cwd.as_deref(),
            )
        } else {
            match &mut self.screen {
                Screen::Home => home::show(
                    ui,
                    &home::HomeState {
                        recent: &self.settings.recent_repositories,
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
                    self.error.as_deref(),
                    !self.settings.history_sidebar_hidden,
                    !self.settings.detail_sidebar_hidden,
                    !self.settings.terminal_hidden,
                    syncing,
                ),
            }
        };

        for action in actions {
            match action {
                Action::Browse => {
                    self.error = None;
                    self.picker = Some(FolderPicker::open(ui.ctx(), frame, self.browse_start()));
                }
                Action::Open(path) => {
                    self.error = None;
                    self.open(ui.ctx(), path);
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
                Action::SetTerminalFont(name) => {
                    self.settings.set_terminal_font(name);
                    self.save_settings();
                }
                Action::EnsureFontCatalog => {
                    self.fonts.ensure(ui.ctx(), true);
                }
                Action::Fetch(upstream) => {
                    self.start_sync(ui.ctx(), SyncCommand::Fetch(upstream));
                }
                Action::Pull => {
                    self.start_sync(ui.ctx(), SyncCommand::Pull);
                }
            }
        }
        self.sync_graph(ui.ctx());
    }
}

struct Refreshed {
    info: RepoInfo,
    branches: Vec<Branch>,
    latest: Option<CommitDetail>,
    detached_oid: Option<String>,
}

fn load(path: &Path) -> OpenResult {
    let git = Git::discover().map_err(|error| error.to_string())?;
    let refreshed = reload_repo(&git, path)?;
    let listed: Vec<ListedBranch> = refreshed
        .branches
        .iter()
        .cloned()
        .map(|branch| ListedBranch {
            branch,
            checked: true,
        })
        .collect();
    let tips = selected_tips(&listed, refreshed.detached_oid.as_deref());
    let graph = git::branch_graph(&git, &refreshed.info.root, &tips, HISTORY_LIMIT)
        .map_err(|error| error.to_string())?;
    Ok(LoadedRepo {
        info: refreshed.info,
        graph,
        tips,
        detached_oid: refreshed.detached_oid,
        branches: refreshed.branches,
        latest: refreshed.latest,
    })
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
    Ok(Refreshed {
        info,
        branches,
        latest,
        detached_oid,
    })
}

enum SyncCommand {
    Fetch(String),
    Pull,
}

struct SyncJob {
    view_id: egui::Id,
    kind: SyncKind,
    receiver: Receiver<Result<Refreshed, String>>,
}

fn run_sync(root: &Path, command: SyncCommand) -> Result<Refreshed, String> {
    let git = Git::discover().map_err(|error| error.to_string())?;
    match &command {
        SyncCommand::Fetch(upstream) => {
            git::fetch(&git, root, upstream).map_err(|error| error.to_string())?;
        }
        SyncCommand::Pull => {
            git::pull_ff_only(&git, root).map_err(|error| error.to_string())?;
        }
    }
    reload_repo(&git, root)
}

/// Keeps the sidebar checkboxes. New branches start checked, same as an open.
fn apply_refresh(repo: &mut OpenedRepo, refreshed: Refreshed) {
    let previous = std::mem::take(&mut repo.branches);
    repo.info = refreshed.info;
    repo.latest = refreshed.latest;
    repo.detached_oid = refreshed.detached_oid;
    repo.branches = refreshed
        .branches
        .into_iter()
        .map(|branch| {
            let checked = previous
                .iter()
                .find(|listed| listed.branch.name == branch.name)
                .map(|listed| listed.checked)
                .unwrap_or(true);
            ListedBranch { branch, checked }
        })
        .collect();
}

/// Checked branches that have commits, in sidebar order, plus detached HEAD.
fn selected_tips(branches: &[ListedBranch], detached_oid: Option<&str>) -> Vec<GraphTip> {
    let mut tips = Vec::new();
    for (index, listed) in branches.iter().enumerate() {
        if !listed.checked {
            continue;
        }
        if let Some(oid) = &listed.branch.oid {
            tips.push(GraphTip {
                name: listed.branch.name.clone(),
                oid: oid.clone(),
                color: index,
            });
        }
    }
    if let Some(oid) = detached_oid {
        tips.push(GraphTip {
            name: "HEAD".to_owned(),
            oid: oid.to_owned(),
            color: branches.len(),
        });
    }
    tips
}

struct GraphLoad {
    generation: u64,
    view_id: egui::Id,
    selection: Vec<GraphTip>,
    receiver: Receiver<Result<HistoryGraph, String>>,
}

impl CthulhuApp {
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
        let kind = match &command {
            SyncCommand::Fetch(_) => SyncKind::Fetch,
            SyncCommand::Pull => SyncKind::Pull,
        };
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let result = run_sync(&root, command);
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
                apply_refresh(repo, refreshed);
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
        let (root, view_id) = {
            let Screen::Repo(repo) = &self.screen else {
                return;
            };
            (repo.info.root.clone(), repo.view_id)
        };
        self.graph_generation = self.graph_generation.wrapping_add(1);
        let generation = self.graph_generation;
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        let requested = tips.clone();
        thread::spawn(move || {
            let result = reload_graph(&root, &requested);
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

fn reload_graph(root: &Path, tips: &[GraphTip]) -> Result<HistoryGraph, String> {
    let git = Git::discover().map_err(|error| error.to_string())?;
    git::branch_graph(&git, root, tips, HISTORY_LIMIT).map_err(|error| error.to_string())
}
