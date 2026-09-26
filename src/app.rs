use std::env;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use cthulhu_git::git::{self, Git, Head, RepoInfo};
use eframe::egui::{self, Button, Color32, Key, RichText, TextEdit};

/// Everything shown after one refresh. A new report replaces the previous one
/// wholesale, so stale repository data never sits next to a fresh error.
#[derive(Default)]
struct Report {
    git: Option<Git>,
    repo: Option<RepoInfo>,
    error: Option<String>,
}

pub struct CthulhuApp {
    path_input: String,
    pending: Option<Receiver<Report>>,
    report: Option<Report>,
}

impl CthulhuApp {
    pub fn new(ctx: &egui::Context, initial: &Path) -> Self {
        let mut app = Self {
            path_input: initial.display().to_string(),
            pending: None,
            report: None,
        };
        app.refresh(ctx);
        app
    }

    /// git runs on a worker thread: blocking the UI thread would freeze the window.
    fn refresh(&mut self, ctx: &egui::Context) {
        let input = self.path_input.trim();
        if input.is_empty() {
            self.pending = None;
            self.report = Some(Report {
                error: Some("Enter the path of a folder to inspect.".to_owned()),
                ..Report::default()
            });
            return;
        }

        let path = PathBuf::from(input);
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        thread::spawn(move || {
            // The receiver is gone if a newer refresh replaced this one.
            let _ = sender.send(build_report(&path));
            ctx.request_repaint();
        });
        self.pending = Some(receiver);
    }

    fn poll(&mut self) {
        let Some(receiver) = &self.pending else {
            return;
        };
        match receiver.try_recv() {
            Ok(report) => {
                self.report = Some(report);
                self.pending = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.report = Some(Report {
                    error: Some("Inspection stopped unexpectedly. Try again.".to_owned()),
                    ..Report::default()
                });
                self.pending = None;
            }
        }
    }

    fn path_row(&mut self, ui: &mut egui::Ui) {
        let loading = self.pending.is_some();
        let mut refresh = false;

        ui.horizontal_wrapped(|ui| {
            let reserved = 290.0;
            let edit = ui.add(
                TextEdit::singleline(&mut self.path_input)
                    .hint_text("Path to a folder inside a Git repository")
                    .desired_width((ui.available_width() - reserved).max(180.0)),
            );
            if edit.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter)) {
                refresh = true;
            }
            if ui.add_enabled(!loading, Button::new("Refresh")).clicked() {
                refresh = true;
            }
            if ui
                .add_enabled(!loading, Button::new("Use current directory"))
                .clicked()
                && let Ok(cwd) = env::current_dir()
            {
                self.path_input = cwd.display().to_string();
                refresh = true;
            }
        });

        if refresh {
            self.refresh(ui.ctx());
        }
    }

    fn details(&self, ui: &mut egui::Ui) {
        let report = self.report.as_ref();
        let repo = report.and_then(|report| report.repo.as_ref());
        let placeholder = || RichText::new("—").weak();

        egui::Grid::new("repository-details")
            .num_columns(2)
            .spacing([28.0, 12.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Repository").weak());
                ui.label(match repo {
                    Some(repo) => RichText::new(&repo.name).size(24.0).strong(),
                    None => placeholder(),
                });
                ui.end_row();

                ui.label(RichText::new("Branch").weak());
                ui.label(match repo {
                    Some(repo) => branch_text(&repo.head, ui.visuals().warn_fg_color),
                    None => placeholder(),
                });
                ui.end_row();

                ui.label(RichText::new("Root").weak());
                ui.label(match repo {
                    Some(repo) => RichText::new(repo.root.display().to_string()).monospace(),
                    None => placeholder(),
                });
                ui.end_row();

                ui.label(RichText::new("Git").weak());
                ui.label(match report.and_then(|report| report.git.as_ref()) {
                    Some(git) => RichText::new(format!("{} ({})", git.path.display(), git.version))
                        .monospace(),
                    None => placeholder(),
                });
                ui.end_row();
            });
    }
}

impl eframe::App for CthulhuApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll();

        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(4.0);
            ui.label(RichText::new("Cthulhu Git").size(28.0).strong());
            ui.label(
                RichText::new("Shows the repository and current branch using the git installed on this machine.")
                    .weak(),
            );
            ui.add_space(14.0);

            self.path_row(ui);
            ui.add_space(10.0);

            if self.pending.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading…");
                });
            } else {
                ui.add_space(ui.spacing().interact_size.y);
            }
            ui.add_space(6.0);

            egui::Frame::group(ui.style())
                .inner_margin(16.0)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    self.details(ui);
                });

            if let Some(error) = self.report.as_ref().and_then(|report| report.error.as_ref()) {
                ui.add_space(12.0);
                let color = ui.visuals().error_fg_color;
                egui::Frame::group(ui.style())
                    .stroke(egui::Stroke::new(1.0, color))
                    .inner_margin(12.0)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.colored_label(color, error);
                    });
            }
        });
    }
}

fn build_report(path: &Path) -> Report {
    let git = match Git::discover() {
        Ok(git) => git,
        Err(error) => {
            return Report {
                error: Some(error.to_string()),
                ..Report::default()
            };
        }
    };

    match git::inspect(&git, path) {
        Ok(repo) => Report {
            git: Some(git),
            repo: Some(repo),
            error: None,
        },
        Err(error) => Report {
            git: Some(git),
            repo: None,
            error: Some(error.to_string()),
        },
    }
}

fn branch_text(head: &Head, detached_color: Color32) -> RichText {
    let text = RichText::new(head.to_string()).size(18.0);
    match head {
        Head::Branch(_) => text.strong(),
        Head::Unborn(_) => text.italics(),
        Head::Detached { .. } => text.color(detached_color),
    }
}
