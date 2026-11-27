//! Settings screen. It replaces the whole window, including the repository
//! bottom bar. Back and Home sit at the left of the top bar, and the title
//! at the right. Pages are listed on the left; the chosen page is a centered
//! column on the right. Both columns scroll.
//!
//! Appearance chooses the interface typeface. Terminal chooses the typeface
//! of the terminal only. Both lists are the faces compiled into the app.
//!
//! The Terminal page draws the shell prompt in the chosen font. That shell
//! ignores pointer and keyboard, and it is not the repository terminal.
//!
//! The History page turns author avatars on or off. The GitHub Auth page
//! holds the optional read-only token.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use cthulhu_git::github::{ApiStatus, TOKEN_TEMPLATE_URL};
use cthulhu_git::settings::{HistoryColumns, sanitize_font_name};
use eframe::egui::{
    self, Align, Button, Color32, CursorIcon, FontId, Frame, Id, Key, Label, Layout, Margin,
    RichText, ScrollArea, Sense, Stroke, TextEdit, TextStyle, Ui, UiBuilder, Vec2,
};

use super::Action;
use super::fonts::{self, Face};
use super::github_auth::{GitHubAuth, Notice, Stored, token_field_id};
use super::icons::{self, Icon};
use super::terminal::{self, Terminal};
use super::theme::{self, Palette};
use super::widgets;

const NAV_WIDTH: f32 = 260.0;
const PAGE_WIDTH: f32 = 780.0;
const UI_SAMPLE: &str = "The quick brown fox jumps over the lazy dog.";
const TERMINAL_SAMPLE: &str = "main  21de8d4a3b7c  Rise from the sea";
/// Space above and below the prompt, inside the preview border.
const PREVIEW_PAD: f32 = 8.0;
const PAGE_TOP: f32 = 40.0;
/// Added under the last block. The column already keeps `COLUMN_SPACING` in the cursor.
const PAGE_END: f32 = 32.0;
/// Line and control spacing inside a page. Replaces egui's 3px default on this screen.
const COLUMN_SPACING: f32 = 8.0;
/// Extra gap before a section or the About card, on top of `COLUMN_SPACING`.
const BETWEEN_CARDS: f32 = 12.0;
const TITLE_SIZE: f32 = 24.0;
const SECTION_SIZE: f32 = 18.0;
/// Body text, buttons and fields on a page; subtitles too.
const BODY_SIZE: f32 = 16.0;
/// Small text on a page: hints and labels.
const HELP_SIZE: f32 = 15.0;
const NAV_LABEL_SIZE: f32 = 16.0;
const BAR_TITLE_SIZE: f32 = 18.0;
const BUTTON_MIN: Vec2 = Vec2::new(0.0, 36.0);
const NAV_ITEM_GAP: f32 = 6.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Appearance,
    Terminal,
    History,
    GitHubAuth,
    About,
}

/// The navigation, top to bottom.
const PAGES: [(Page, &str, Icon); 5] = [
    (Page::Appearance, "Appearance", Icon::Type),
    (Page::Terminal, "Terminal", Icon::Terminal),
    (Page::History, "History", Icon::CircleUserRound),
    (Page::GitHubAuth, "GitHub Auth", Icon::KeyRound),
    (Page::About, "About", Icon::Info),
];

/// What the History, GitHub Auth, and About pages read.
pub struct SettingsView {
    pub author_avatars: bool,
    pub api_status: ApiStatus,
    /// A folder dialog or a repository open is already in progress.
    pub busy: bool,
    pub about: AboutInfo,
}

/// Identity shown on the About page. Tests pass fixed strings; the app fills
/// them from the build and from `git --version`.
pub struct AboutInfo {
    pub version: String,
    pub version_date: String,
    pub commit: String,
    pub license: String,
    pub description: String,
    pub repository: String,
    pub git_version: String,
}

impl AboutInfo {
    pub fn current(git_version: &str) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            version_date: env!("CTHULHU_GIT_COMMIT_DATE").to_owned(),
            commit: env!("CTHULHU_GIT_COMMIT").to_owned(),
            license: env!("CARGO_PKG_LICENSE").to_owned(),
            description: env!("CARGO_PKG_DESCRIPTION").to_owned(),
            repository: env!("CARGO_PKG_REPOSITORY").to_owned(),
            git_version: git_version.to_owned(),
        }
    }

    /// One block for a bug report. The repository URL is included as stored.
    pub fn copy_text(&self) -> String {
        format!(
            "Cthulhu-Git {version}\nVersion date: {date}\nCommit: {commit}\nLicense: {license}\nGit: {git}\n{repository}",
            version = self.version,
            date = self.version_date,
            commit = self.commit,
            license = self.license,
            git = self.git_version,
            repository = self.repository,
        )
    }

    fn releases_url(&self) -> String {
        format!("{}/releases", self.repository.trim_end_matches('/'))
    }
}

pub fn show(
    ui: &mut Ui,
    ui_font: Option<&str>,
    terminal_font: Option<&str>,
    preview: &mut Option<Terminal>,
    cwd: Option<&Path>,
    view_state: &SettingsView,
    github: &mut GitHubAuth,
) -> Vec<Action> {
    let palette = theme::current(ui.ctx()).palette;
    let mut actions = Vec::new();
    let mut page = ui
        .ctx()
        .data(|data| data.get_temp(page_id()))
        .unwrap_or(Page::Appearance);

    egui::Panel::top("settings-top-bar")
        .frame(bar_frame(&palette))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if icons::icon_button(ui, Icon::ArrowLeft, palette.text, "Back").clicked() {
                    actions.push(Action::CloseSettings);
                }
                if icons::icon_button(ui, Icon::House, palette.text, "Open another repository")
                    .clicked()
                {
                    actions.push(Action::Home);
                }
                let folder_tint = if view_state.busy {
                    palette.text_muted
                } else {
                    palette.text
                };
                if icons::icon_button_enabled(
                    ui,
                    Icon::Folder,
                    folder_tint,
                    "Open a folder",
                    !view_state.busy,
                )
                .clicked()
                {
                    actions.push(Action::Browse);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new("Settings").size(BAR_TITLE_SIZE).strong());
                });
            });
        });

    egui::Panel::left("settings-nav")
        .exact_size(NAV_WIDTH)
        .resizable(false)
        .frame(nav_frame(&palette))
        .show(ui, |ui| {
            ScrollArea::vertical()
                .id_salt("settings-nav")
                .auto_shrink(false)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = NAV_ITEM_GAP;
                    for (entry, label, icon) in PAGES {
                        if nav_button(ui, label, icon, page == entry).clicked() {
                            page = entry;
                        }
                    }
                });
        });

    egui::CentralPanel::default().show(ui, |ui| {
        ScrollArea::vertical()
            .id_salt("settings-page")
            .auto_shrink(false)
            .show(ui, |ui| {
                page_column(ui, |ui| match page {
                    Page::Appearance => appearance_page(ui, ui_font, &mut actions),
                    Page::Terminal => {
                        terminal_page(ui, terminal_font, &mut actions);
                        if let Some(cwd) = cwd {
                            section(ui, |ui| {
                                centered_text(ui, RichText::new("Terminal Live Preview").weak());
                                shell_preview(ui, &palette, preview, cwd);
                            });
                        }
                    }
                    Page::History => history_page(ui, view_state.author_avatars, &mut actions),
                    Page::GitHubAuth => {
                        github_page(ui, &palette, github, &view_state.api_status);
                    }
                    Page::About => about_page(ui, &palette, &view_state.about),
                });
            });
    });

    ui.ctx().data_mut(|data| {
        data.insert_temp(page_id(), page);
    });
    actions
}

fn appearance_page(ui: &mut Ui, saved: Option<&str>, actions: &mut Vec<Action>) {
    page_header(
        ui,
        "Appearance",
        "Typeface for the interface. Every face here is bundled with the app.",
    );
    section(ui, |ui| {
        if let Some(text) = stale_font(saved, fonts::UI_FACES) {
            help(
                ui,
                &format!("\"{text}\" is not bundled. The interface is using Ubuntu."),
            );
        }
        for face in fonts::UI_FACES {
            let selected = fonts::resolve_ui(saved) == face.id;
            if choice_row(ui, face, UI_SAMPLE, selected).clicked() {
                let next = if face.id == fonts::DEFAULT_UI_ID {
                    None
                } else {
                    Some(face.id)
                };
                store_font(saved, next, actions, Action::SetUiFont);
            }
        }
        help(ui, "Commit hashes keep the built-in monospace.");
    });
}

fn terminal_page(ui: &mut Ui, saved: Option<&str>, actions: &mut Vec<Action>) {
    page_header(
        ui,
        "Terminal Font",
        "Typeface for the terminal only. Meslo LGS NF is the default.",
    );
    section(ui, |ui| {
        if let Some(text) = stale_terminal(saved) {
            help(
                ui,
                &format!("\"{text}\" is not bundled. The terminal is using Meslo LGS NF."),
            );
        }
        let builtin = saved.is_some_and(fonts::is_builtin_terminal);
        if choice_builtin(ui, builtin).clicked() {
            store_font(
                saved,
                Some(fonts::BUILTIN_TERMINAL_ID),
                actions,
                Action::SetTerminalFont,
            );
        }
        for face in fonts::TERMINAL_FACES {
            let selected = !builtin && fonts::resolve_terminal(saved) == Some(face.id);
            if choice_row(ui, face, TERMINAL_SAMPLE, selected).clicked() {
                let next = if face.id == fonts::DEFAULT_TERMINAL_ID {
                    None
                } else {
                    Some(face.id)
                };
                store_font(saved, next, actions, Action::SetTerminalFont);
            }
        }
        help(
            ui,
            "Prompt icons are in Meslo LGS NF. The other faces do not include them.",
        );
    });
}

/// The saved string when it names nothing in `faces`.
///
/// The label is the sanitized id. A newline or a name that sanitizes to
/// nothing is not shown.
fn stale_font(saved: Option<&str>, faces: &[Face]) -> Option<String> {
    let name = sanitize_font_name(saved?)?;
    if fonts::face_by_id(faces, &name).is_some() {
        None
    } else {
        Some(name)
    }
}

/// Like [`stale_font`] for the terminal. `builtin` is a real choice.
fn stale_terminal(saved: Option<&str>) -> Option<String> {
    let name = sanitize_font_name(saved?)?;
    if fonts::is_builtin_terminal(&name)
        || fonts::face_by_id(fonts::TERMINAL_FACES, &name).is_some()
    {
        None
    } else {
        Some(name)
    }
}

fn store_font(
    saved: Option<&str>,
    next: Option<&str>,
    actions: &mut Vec<Action>,
    make: impl Fn(Option<String>) -> Action,
) {
    let current = saved.and_then(sanitize_font_name);
    if current.as_deref() != next {
        actions.push(make(next.map(str::to_owned)));
    }
}

struct Specimen<'a> {
    id: &'a str,
    title: &'a str,
    note: &'a str,
    sample: &'a str,
    title_font: egui::FontId,
    sample_font: egui::FontId,
}

fn choice_row(ui: &mut Ui, face: &Face, sample: &str, selected: bool) -> egui::Response {
    specimen_row(
        ui,
        Specimen {
            id: face.id,
            title: face.name,
            note: face.note,
            sample,
            title_font: fonts::preview_font(ui, face.id, BODY_SIZE),
            sample_font: fonts::preview_font(ui, face.id, HELP_SIZE),
        },
        selected,
    )
}

fn choice_builtin(ui: &mut Ui, selected: bool) -> egui::Response {
    let mut title = TextStyle::Monospace.resolve(ui.style());
    title.size = BODY_SIZE;
    let mut sample = title.clone();
    sample.size = HELP_SIZE;
    specimen_row(
        ui,
        Specimen {
            id: "builtin",
            title: "Built-in",
            note: "Shipped with the app",
            sample: TERMINAL_SAMPLE,
            title_font: title,
            sample_font: sample,
        },
        selected,
    )
}

fn specimen_row(ui: &mut Ui, row: Specimen<'_>, selected: bool) -> egui::Response {
    let palette = theme::current(ui.ctx()).palette;
    let Specimen {
        id,
        title,
        note,
        sample,
        title_font,
        sample_font,
    } = row;
    let response = ui
        .scope_builder(UiBuilder::new().id_salt(id).sense(Sense::click()), |ui| {
            let hovered = ui.response().hovered();
            let fill = if selected {
                palette.surface_active
            } else if hovered {
                palette.surface_hover
            } else {
                Color32::TRANSPARENT
            };
            Frame::new()
                .fill(fill)
                .corner_radius(widgets::CORNER_RADIUS)
                .inner_margin(Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.add(Label::new(RichText::new(title).font(title_font)).selectable(false));
                        ui.add(Label::new(RichText::new(note).small().weak()).selectable(false));
                    });
                    ui.add(
                        Label::new(RichText::new(sample).font(sample_font).weak())
                            .selectable(false),
                    );
                });
        })
        .response;
    response.on_hover_cursor(CursorIcon::PointingHand)
}

fn about_page(ui: &mut Ui, palette: &Palette, about: &AboutInfo) {
    ui.add_space(PAGE_TOP);
    centered_text(ui, RichText::new("Cthulhu-Git").size(TITLE_SIZE).strong());
    about_logo(ui);
    centered_text(ui, RichText::new(&about.description).weak());
    card(ui, palette, |ui| {
        about_row(ui, palette, "Version", &about.version, false);
        about_row(ui, palette, "Version date", &about.version_date, false);
        about_row(ui, palette, "Commit", &about.commit, true);
        about_row(ui, palette, "License", &about.license, false);
        about_row(ui, palette, "Git", &about.git_version, false);
        for url in [about.repository.clone(), about.releases_url()] {
            ui.horizontal(|ui| {
                icons::icon(ui, Icon::GitHub, palette.text);
                ui.hyperlink_to(&url, &url);
            });
        }
        // `horizontal` keeps this row at the button's height. A bare right-to-left
        // layout would claim the rest of the page and stretch the card.
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let copy = text_button(ui, "Copy version info", true);
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.set_min_width(ui.available_width());
                    help(ui, "© 2026 Don Linux");
                });
                if copy.clicked() {
                    ui.ctx().copy_text(about.copy_text());
                }
            });
        });
    });
}

/// Side of the application icon under the About title. The source is 256 px.
const ABOUT_LOGO: f32 = 128.0;

fn about_logo(ui: &mut Ui) {
    ui.vertical_centered(|ui| {
        icons::app_logo(ui, ABOUT_LOGO);
    });
}

fn about_row(ui: &mut Ui, palette: &Palette, label: &str, value: &str, monospace: bool) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(RichText::new(label).small().weak());
        let mut text = RichText::new(value);
        if monospace {
            text = text.monospace().size(BODY_SIZE).color(palette.hash);
        }
        ui.label(text);
    });
}

fn history_page(ui: &mut Ui, avatars_on: bool, actions: &mut Vec<Action>) {
    page_header(
        ui,
        "Commit History",
        "Author pictures and the width of the history columns.",
    );
    section(ui, |ui| {
        let mut on = avatars_on;
        if ui.checkbox(&mut on, "Show author avatars").changed() {
            actions.push(Action::SetAuthorAvatars(on));
        }
        help(
            ui,
            "Pictures come from GitHub (no-reply addresses, and the GitHub API for repositories \
             hosted there) and from Gravatar, which receives a SHA-256 hash of the address, never \
             the address itself. They are kept for a week in the user cache folder. Off draws \
             initials and sends nothing.",
        );
    });
    section(ui, |ui| {
        section_title(ui, "Columns");
        help(
            ui,
            "Drag a divider in the history header to resize that column only. Double-click a \
             divider to restore that column. Widths follow the window and the open sidebars.",
        );
        if text_button(ui, "Reset all column widths", true).clicked() {
            actions.push(Action::SetHistoryColumns(HistoryColumns::default()));
        }
    });
}

/// Read-only uses the token has today, and what it could add later with the
/// extra read permission each one needs.
const POSSIBLE_LATER: [(&str, &str); 5] = [
    (
        "Pull requests on each branch label: number, title, state and reviews",
        "Pull requests: read",
    ),
    (
        "CI status for each commit, beside its hash",
        "Commit statuses: read, Checks: read",
    ),
    (
        "Titles of the issues and pull requests a message mentions, in the hover",
        "Issues: read, Pull requests: read",
    ),
    ("Releases and tags with their notes", "Contents: read"),
    (
        "Who approved or merged a pull request",
        "Pull requests: read",
    ),
];

fn github_page(ui: &mut Ui, palette: &Palette, github: &mut GitHubAuth, api: &ApiStatus) {
    page_header(
        ui,
        "GitHub Auth",
        "Optional. Without a token, public repositories still show avatars.",
    );
    section(ui, |ui| {
        ui.label(
            "A GitHub token lets Cthulhu Git read the authors and avatars of your private \
             repositories, and raises GitHub's limit from 60 to 5,000 requests an hour. It is only \
             used to read: Cthulhu Git never writes to GitHub with it.",
        );
    });
    section(ui, |ui| {
        section_title(ui, "1. Create a read-only token");
        ui.hyperlink_to(
            "Open GitHub's token form, already set to read-only",
            TOKEN_TEMPLATE_URL,
        );
        help(
            ui,
            "The form asks for Contents: read-only and Metadata: read-only. For an organization's \
             private repositories, choose that organization as the Resource owner.",
        );
    });

    let busy = github.busy();
    section(ui, |ui| {
        section_title(ui, "2. Paste it here");
        ui.horizontal(|ui| {
            // The button is placed first so the field takes whatever width remains.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let save = text_button(ui, "Save", !busy && !github.draft.trim().is_empty());
                let field = ui.add_enabled(
                    !busy,
                    TextEdit::singleline(&mut github.draft)
                        .id(token_field_id())
                        .password(true)
                        .hint_text("github_pat_…")
                        .desired_width(ui.available_width()),
                );
                let enter = field.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
                if (save.clicked() || enter) && !github.draft.trim().is_empty() {
                    github.save_draft(ui.ctx());
                }
            });
        });
        ui.horizontal(|ui| {
            if text_button(ui, "Import from GitHub CLI", !busy)
                .on_hover_text("Runs `gh auth token` and fills the field. Nothing is saved yet.")
                .clicked()
            {
                github.import_gh(ui.ctx());
            }
            if cthulhu_git::github::token_from_env().is_some()
                && text_button(ui, "Use GH_TOKEN", !busy)
                    .on_hover_text("Fills the field from GH_TOKEN or GITHUB_TOKEN.")
                    .clicked()
            {
                github.use_env();
            }
        });
    });

    section(ui, |ui| {
        section_title(ui, "Status");
        ui.label(match github.stored() {
            Stored::None => "No token. Public repositories only.",
            Stored::Keychain => "Token saved in the system keychain.",
            Stored::Session => "Token used for this session only; it is not saved.",
        });
        if let Some(login) = github.user() {
            let limit = github
                .rate()
                .and_then(|rate| Some((rate.remaining?, rate.limit?)))
                .map(|(remaining, limit)| format!(" · {remaining}/{limit} requests left this hour"))
                .unwrap_or_default();
            ui.colored_label(palette.diff_added, format!("Connected as {login}{limit}"));
        }
        if api.token_rejected {
            ui.colored_label(
                palette.warning,
                "GitHub rejected the token; requests continue without it.",
            );
        }
        if let Some(until) = api.paused_until {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs())
                .unwrap_or(0);
            if until > now {
                let minutes = (until - now).div_ceil(60);
                ui.colored_label(
                    palette.warning,
                    format!(
                        "GitHub's rate limit was reached. The API is asked again in {minutes} min."
                    ),
                );
            }
        }
        match github.notice() {
            Some(Notice::Info(text)) => help(ui, text),
            Some(Notice::Error(text)) => {
                ui.colored_label(palette.error, text);
            }
            None => {}
        }
        if busy {
            ui.spinner();
        }
        ui.horizontal(|ui| {
            if text_button(ui, "Verify", !busy && github.has_token())
                .on_hover_text("Asks GitHub who the token belongs to.")
                .clicked()
            {
                github.verify(ui.ctx());
            }
            if text_button(ui, "Remove token", !busy && github.has_token()).clicked() {
                github.remove(ui.ctx());
            }
        });
        help(
            ui,
            "Stored in Keychain on macOS, Credential Manager on Windows, or the Secret Service \
             (GNOME Keyring, KWallet) on Linux. Never in settings.json. It is sent only to \
             api.github.com, never to avatar hosts.",
        );
    });

    section(ui, |ui| {
        section_title(ui, "Possible later with this token");
        for (feature, permission) in POSSIBLE_LATER {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("• {feature}"));
                help(ui, &format!("({permission})"));
            });
        }
        help(
            ui,
            "Writing to GitHub (pushing over HTTPS, opening pull requests) would need a different \
             token with write access, and is not part of this page.",
        );
    });
}

/// The prompt block, clipped out of a wide shell. The PTY is many rows tall
/// so a multi-line prompt is not scrolled away; the box grows to the block
/// that ends at the cursor. The shell measures the terminal font each frame,
/// so a new family shows up here without touching the repository session.
/// Pointer and keyboard are ignored.
fn shell_preview(ui: &mut Ui, palette: &Palette, preview: &mut Option<Terminal>, cwd: &Path) {
    let row = terminal_row_height(ui);
    let lines = terminal::preview_line_count(preview) as f32;
    let size = Vec2::new(ui.available_width(), row * lines + PREVIEW_PAD * 2.0);
    let drawn = ui.allocate_ui(size, |ui| {
        ui.set_min_size(size);
        let outer = ui.max_rect();
        ui.painter().rect_filled(outer, 0.0, palette.background);
        let content = outer.shrink2(Vec2::new(0.0, PREVIEW_PAD));
        ui.scope_builder(UiBuilder::new().max_rect(content), |ui| {
            ui.shrink_clip_rect(content);
            // The page's larger text is not the terminal's.
            ui.style_mut().text_styles = ui.ctx().global_style().text_styles.clone();
            terminal::show(ui, preview, cwd, preview_view_id(), palette, false);
        });
        // Clicks land here, including the padding, and do nothing.
        let _ = ui.interact(outer, preview_hit_id(), Sense::CLICK | Sense::DRAG);
    });
    ui.painter().rect_stroke(
        drawn.response.rect,
        0.0,
        Stroke::new(1.0, palette.border),
        egui::StrokeKind::Inside,
    );
}

fn terminal_row_height(ui: &Ui) -> f32 {
    let font = fonts::terminal_font_id(ui);
    ui.ctx().fonts_mut(|fonts| fonts.row_height(&font).max(1.0))
}

fn preview_hit_id() -> Id {
    preview_view_id().with("hit")
}

fn preview_view_id() -> Id {
    Id::unique("settings-font-preview")
}

fn nav_button(ui: &mut Ui, label: &str, icon: Icon, selected: bool) -> egui::Response {
    let palette = theme::current(ui.ctx()).palette;
    let response = ui
        .scope_builder(
            UiBuilder::new().id_salt(label).sense(Sense::click()),
            |ui| {
                let hovered = ui.response().hovered();
                let fill = if selected {
                    palette.surface_active
                } else if hovered {
                    palette.surface_hover
                } else {
                    Color32::TRANSPARENT
                };
                Frame::new()
                    .fill(fill)
                    .corner_radius(widgets::CORNER_RADIUS)
                    .inner_margin(Margin::symmetric(14, 12))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let tint = if selected {
                                palette.accent
                            } else {
                                palette.text
                            };
                            icons::icon(ui, icon, tint);
                            ui.add(
                                egui::Label::new(RichText::new(label).size(NAV_LABEL_SIZE))
                                    .selectable(false)
                                    .truncate(),
                            );
                        });
                    });
            },
        )
        .response;
    response.on_hover_cursor(CursorIcon::PointingHand)
}

fn page_id() -> Id {
    Id::unique("settings-page")
}

/// Same chrome as the repository bars.
fn bar_frame(palette: &Palette) -> Frame {
    Frame::new()
        .fill(palette.surface)
        .stroke(Stroke::new(1.0, palette.border))
        .inner_margin(Margin::symmetric(12, 6))
}

fn nav_frame(palette: &Palette) -> Frame {
    Frame::new()
        .fill(palette.surface)
        .inner_margin(Margin::symmetric(10, 12))
}

/// The page's text sizes and spacing, which every block in it inherits.
fn page_column(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    widgets::centered_column(ui, PAGE_WIDTH, |ui| {
        let style = ui.style_mut();
        style.spacing.item_spacing.y = COLUMN_SPACING;
        // Buttons, checkboxes, links and fields draw their text in `Body`.
        for (text_style, size) in [(TextStyle::Body, BODY_SIZE), (TextStyle::Small, HELP_SIZE)] {
            style
                .text_styles
                .insert(text_style, FontId::proportional(size));
        }
        add(ui);
        ui.add_space(PAGE_END);
    });
}

fn page_header(ui: &mut Ui, title: &str, subtitle: &str) {
    ui.add_space(PAGE_TOP);
    centered_text(ui, RichText::new(title).size(TITLE_SIZE).strong());
    centered_text(ui, RichText::new(subtitle).weak());
}

/// Centers each line in the column, including a subtitle that wraps.
fn centered_text(ui: &mut Ui, text: RichText) {
    ui.vertical_centered(|ui| {
        ui.add(Label::new(text).halign(Align::Center).selectable(false));
    });
}

fn section_title(ui: &mut Ui, title: &str) {
    ui.label(RichText::new(title).size(SECTION_SIZE).strong());
}

fn help(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).small().weak());
}

fn text_button(ui: &mut Ui, label: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(enabled, Button::new(label).min_size(BUTTON_MIN))
        .on_hover_cursor(CursorIcon::PointingHand)
}

/// A block of the page with the gap of a card, and no fill or border.
fn section(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.add_space(BETWEEN_CARDS);
    ui.vertical(|ui| {
        ui.set_width(ui.available_width());
        add(ui);
    });
}

fn card(ui: &mut Ui, palette: &Palette, add: impl FnOnce(&mut Ui)) {
    ui.add_space(BETWEEN_CARDS);
    Frame::new()
        .fill(palette.surface)
        .stroke(Stroke::new(1.0, palette.border))
        .corner_radius(widgets::CORNER_RADIUS)
        .inner_margin(Margin::same(20))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use eframe::egui::accesskit::Role;
    use eframe::egui::output::OutputCommand;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};

    use super::*;
    use crate::ui::test_support::{painted_texts, tree_text};

    type Shared<T> = Rc<RefCell<T>>;

    /// Settings with `ui_font` and `terminal_font` saved and installed the way
    /// the app installs them, keeping the last frame's actions.
    fn settings(
        ui_font: Option<&'static str>,
        terminal_font: Option<&'static str>,
        about: AboutInfo,
    ) -> (Harness<'static>, Shared<GitHubAuth>, Shared<Vec<Action>>) {
        let github = Shared::<GitHubAuth>::default();
        let actions = Shared::<Vec<Action>>::default();
        let (github_ui, actions_ui) = (Rc::clone(&github), Rc::clone(&actions));
        let mut faces = fonts::FontService::default();
        let mut preview = None;
        let view = SettingsView {
            author_avatars: true,
            api_status: ApiStatus::default(),
            busy: false,
            about,
        };
        let harness = Harness::builder()
            .with_size(egui::vec2(1100.0, 1600.0))
            .with_step_dt(1.0 / 60.0)
            .build_ui(move |ui| {
                theme::apply(ui.ctx(), theme::DEFAULT_THEME);
                faces.sync(ui.ctx(), ui_font, terminal_font);
                let mut github = github_ui.borrow_mut();
                let shown = show(
                    ui,
                    ui_font,
                    terminal_font,
                    &mut preview,
                    None,
                    &view,
                    &mut github,
                );
                *actions_ui.borrow_mut() = shown;
            });
        (harness, github, actions)
    }

    fn click(harness: &mut Harness<'_>, label: &str) {
        harness.get_by_label(label).click();
        harness.step();
    }

    #[test]
    fn each_page_lists_its_own_faces_and_asks_only_on_a_click() {
        const INTERFACE: &[&str] = &[
            "Ubuntu",
            "Ubuntu Mono",
            "Ubuntu Sans",
            "IBM Plex Sans",
            "Obel Mono",
            "Aspekta",
            "Inter",
        ];
        const TERMINAL: &[&str] = &["Built-in", "Obel Mono", "Meslo LGS NF", "Departure Mono"];
        const NONE: &[&str] = &[];
        let unknown = (Some("JetBrains Mono"), Some("Fira Code"));
        let ui_font = |id: &str| Action::SetUiFont(Some(id.to_owned()));
        let terminal_font = |id: &str| Action::SetTerminalFont(Some(id.to_owned()));
        // (saved faces, page, faces listed, a face not listed, warning, clicked, asked for)
        #[rustfmt::skip]
        let cases = [
            ((None, None), "Appearance", INTERFACE, "Meslo LGS NF", None, "Inter", ui_font("inter")),
            ((None, None), "Terminal", TERMINAL, "Ubuntu Sans", None, "Departure Mono", terminal_font("departure-mono")),
            // Obel Mono is on both pages, and each sets only its own surface.
            ((Some("ubuntu-sans"), Some("builtin")), "Appearance", INTERFACE, "Meslo LGS NF", None, "Obel Mono", ui_font("obel-mono")),
            ((Some("obel-mono"), Some("builtin")), "Terminal", TERMINAL, "Ubuntu Sans", None, "Obel Mono", terminal_font("obel-mono")),
            // A face that is not bundled is reported, not rewritten.
            (unknown, "Appearance", INTERFACE, "Meslo LGS NF", Some("\"JetBrains Mono\" is not bundled. The interface is using Ubuntu."), "Inter", ui_font("inter")),
            (unknown, "Terminal", TERMINAL, "Ubuntu Sans", Some("\"Fira Code\" is not bundled. The terminal is using Meslo LGS NF."), "Departure Mono", terminal_font("departure-mono")),
            ((None, None), "History", NONE, "Inter", None, "Show author avatars", Action::SetAuthorAvatars(false)),
        ];
        for ((ui, terminal), page, listed, absent, warning, clicked, asked) in cases {
            let case = format!("{page} with {ui:?} and {terminal:?}");
            let (mut harness, _github, actions) =
                settings(ui, terminal, AboutInfo::current("2.43.0"));
            if page != "Appearance" {
                click(&mut harness, page);
            }
            assert!(
                actions.borrow().is_empty(),
                "{case}: {:?}",
                actions.borrow()
            );
            for face in listed.iter().chain(warning.as_ref()) {
                harness.get_by_label(face);
            }
            assert!(
                harness.query_by_label(absent).is_none(),
                "{case}: lists {absent}"
            );
            click(&mut harness, clicked);
            assert_eq!(*actions.borrow(), [asked], "{case}");
        }
    }

    /// `ghp_` and 36 characters: the shape of a classic personal access token.
    const FAKE_TOKEN: &str = "ghp_agentnetworktesttokenvalueXXXXXXXXXX";

    fn assert_no_token_material(text: &str) {
        for needle in ["?", "@", "ghp_", "github_pat_", FAKE_TOKEN] {
            assert!(!text.contains(needle), "{text:?} contains {needle}");
        }
    }

    /// egui appends output commands across passes. A click's commands are the
    /// suffix while the earlier ones are still in front.
    fn commands_since<'a>(
        previous: &[OutputCommand],
        harness: &'a Harness<'_>,
    ) -> &'a [OutputCommand] {
        let commands = &harness.output().platform_output.commands;
        commands.strip_prefix(previous).unwrap_or(commands)
    }

    #[test]
    fn a_typed_token_stays_masked_and_out_of_links_and_copied_text() {
        const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
        const DATE: &str = "2026-10-24";
        let version = env!("CARGO_PKG_VERSION");
        let license = env!("CARGO_PKG_LICENSE");
        let repository = env!("CARGO_PKG_REPOSITORY");
        let releases = format!("{repository}/releases");
        assert_no_token_material(repository);
        let about = AboutInfo {
            version: version.to_owned(),
            version_date: DATE.to_owned(),
            commit: COMMIT.to_owned(),
            license: license.to_owned(),
            description: env!("CARGO_PKG_DESCRIPTION").to_owned(),
            repository: repository.to_owned(),
            git_version: "2.43.0".to_owned(),
        };
        let (mut harness, github, _actions) = settings(None, None, about);

        // Save, Verify, Remove, Import and Use GH_TOKEN all leave the process.
        // This test only types into the masked field.
        click(&mut harness, "GitHub Auth");
        // The link's readable name is the requirement; egui exposes it as a label.
        harness.get_by_label("Open GitHub's token form, already set to read-only");
        harness.get_by_role(Role::PasswordInput).click();
        harness.step();
        harness
            .get_by_role(Role::PasswordInput)
            .type_text(FAKE_TOKEN);
        // The first keystroke into an empty field is painted a frame later.
        harness.step();
        harness.step();
        {
            let auth = github.borrow();
            assert_eq!(auth.draft, FAKE_TOKEN, "the field did not take the token");
            assert!(!auth.busy(), "typing started a keychain or network task");
            assert!(!auth.has_token(), "typing stored a token");
            assert_eq!(auth.stored(), Stored::None);
            assert!(auth.notice().is_none(), "typing produced a notice");
        }
        let field = harness.get_by_role(Role::PasswordInput);
        let mut shown = field.value().unwrap_or_default();
        if shown.is_empty() {
            shown = field.children().filter_map(|child| child.value()).collect();
        }
        let masked = "\u{2022}".repeat(FAKE_TOKEN.len());
        assert_eq!(shown, masked, "password display");
        for page in ["GitHub Auth", "History", "About"] {
            if page != "GitHub Auth" {
                click(&mut harness, page);
            }
            let tree = tree_text(&harness);
            assert!(
                !tree.contains(FAKE_TOKEN),
                "{page}: token in the accessibility tree"
            );
            // Painted text that no widget reports never reaches the tree.
            let painted: Vec<_> = painted_texts(&harness)
                .iter()
                .map(|(_, galley)| galley.text().to_owned())
                .collect();
            assert!(
                painted.iter().all(|text| !text.contains(FAKE_TOKEN)),
                "{page}: token painted"
            );
            assert_eq!(
                painted.contains(&masked),
                page == "GitHub Auth",
                "{page}: masked field"
            );
        }
        assert_eq!(
            github.borrow().draft,
            FAKE_TOKEN,
            "leaving GitHub Auth dropped the typed token"
        );

        let tops = ["Terminal", "History", "GitHub Auth", "About"]
            .map(|label| harness.get_by_label(label).rect().top());
        assert!(
            tops[..3].iter().all(|top| tops[3] > *top),
            "About is not last in the navigation: {tops:?}"
        );
        let about_labels = [version, DATE, COMMIT, license, "2.43.0", repository];
        for label in about_labels
            .into_iter()
            .chain(["© 2026 Don Linux", "Cthulhu-Git logo"])
        {
            harness.get_by_label(label);
        }
        for node in harness.root().children_recursive() {
            if let Some(url) = node.accesskit_node().url() {
                assert!(url == repository || url == releases, "link to {url:?}");
            }
        }
        for url in [repository, releases.as_str()] {
            let previous = harness.output().platform_output.commands.clone();
            click(&mut harness, url);
            let fresh = commands_since(&previous, &harness);
            assert!(
                fresh.iter().any(
                    |command| matches!(command, OutputCommand::OpenUrl(open) if open.url == url)
                ),
                "no OpenUrl for {url}: {fresh:?}"
            );
        }
        let previous = harness.output().platform_output.commands.clone();
        click(&mut harness, "Copy version info");
        let copied = commands_since(&previous, &harness)
            .iter()
            .find_map(|command| match command {
                OutputCommand::CopyText(text) => Some(text.clone()),
                _ => None,
            })
            .expect("Copy version info copies text");
        for part in [version, COMMIT, repository] {
            assert!(copied.contains(part), "{copied:?} lacks {part}");
        }
        assert_no_token_material(&copied);
    }

    #[test]
    fn font_warning_does_not_interpolate_unsanitized_text() {
        let huge = "x".repeat(100_000);
        let name = |text: &str| Some(text.to_owned());
        // (case, warning, the name it shows)
        let cases = [
            (
                "bidi interface",
                stale_font(Some("\u{202E}JetBrains Mono"), fonts::UI_FACES),
                name("JetBrains Mono"),
            ),
            (
                "huge interface",
                stale_font(Some(&huge), fonts::UI_FACES),
                Some("x".repeat(128)),
            ),
            (
                "newline interface",
                stale_font(Some("obel\nmono"), fonts::UI_FACES),
                None,
            ),
            ("newline terminal", stale_terminal(Some("fira\ncode")), None),
            (
                "bidi terminal",
                stale_terminal(Some("\u{202E}Fira Code")),
                name("Fira Code"),
            ),
        ];
        for (case, warning, shown) in cases {
            assert_eq!(warning, shown, "{case}");
        }
    }
}
