//! The window: a home screen, a repository view, and a settings screen.
//!
//! On launch, a folder passed on the command line opens first, then the last
//! repository from the settings, otherwise the home screen shows. Settings
//! are described in `docs/SETTINGS.md`, themes in `docs/THEMES.md`.

#[cfg(test)]
mod adversarial_tests;
mod folder_picker;
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

use cthulhu_git::git::{self, Branch, CommitDetail, Git, History, RepoInfo};
use cthulhu_git::settings::Settings;
use eframe::egui;

use folder_picker::FolderPicker;

/// Newest commits read when a repository opens; the view says when there are more.
const HISTORY_LIMIT: usize = 1000;

pub struct OpenedRepo {
    pub info: RepoInfo,
    pub history: History,
    pub branches: Vec<ListedBranch>,
    pub latest: Option<CommitDetail>,
    /// New on every open, so the history starts scrolled to the top each time.
    pub view_id: egui::Id,
    /// Shell for this repository. `None` until the terminal strip is shown.
    pub terminal: Option<terminal::Terminal>,
}

/// A local branch and whether its checkbox is on.
///
/// `checked` is kept for a later history filter. The commit list does not read it.
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
}

enum Screen {
    Home,
    Repo(Box<OpenedRepo>),
}

struct LoadedRepo {
    info: RepoInfo,
    history: History,
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
        };
        if let Some(path) = startup {
            app.open(ctx, path);
        }
        app
    }

    /// Git runs on a worker thread: blocking the UI thread would freeze the window.
    fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
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
                    history: loaded.history,
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
        self.poll_picker(ui.ctx());

        let actions = if self.settings_open {
            // The repository stays underneath. Keep the shell alive and take
            // keystrokes away from the hidden terminal.
            if let Screen::Repo(repo) = &mut self.screen {
                terminal::service(ui.ctx(), &mut repo.terminal);
                terminal::surrender_focus(ui.ctx(), repo.view_id);
            }
            settings::show(ui)
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
                    self.screen = Screen::Home;
                }
                Action::OpenSettings => {
                    self.settings_open = true;
                }
                Action::CloseSettings => {
                    self.settings_open = false;
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
            }
        }
    }
}

fn load(path: &Path) -> OpenResult {
    let git = Git::discover().map_err(|error| error.to_string())?;
    let info = git::inspect(&git, path).map_err(|error| error.to_string())?;
    let history = git::history(&git, &info.root, &info.head, HISTORY_LIMIT)
        .map_err(|error| error.to_string())?;
    let branches =
        git::branches(&git, &info.root, &info.head).map_err(|error| error.to_string())?;
    let latest =
        git::latest_commit(&git, &info.root, &info.head).map_err(|error| error.to_string())?;
    Ok(LoadedRepo {
        info,
        history,
        branches,
        latest,
    })
}
