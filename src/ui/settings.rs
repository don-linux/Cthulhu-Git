//! Settings screen. It replaces the whole window, including the repository
//! bottom bar. Back and Home sit at the left of the top bar, and the title
//! at the right. Pages are listed on the left; the chosen page is a centered
//! column on the right. Both columns scroll.
//!
//! The font browser is a modal drawn after the panels, so its veil covers the
//! bar, the navigation, and the page.
//!
//! The Terminal page draws one prompt line in the chosen font. That shell
//! ignores pointer and keyboard, and it is not the repository terminal.
//!
//! The History page turns author avatars on or off. The GitHub Auth page
//! holds the optional read-only token.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use cthulhu_git::github::{ApiStatus, TOKEN_TEMPLATE_URL};
use cthulhu_git::settings::HistoryColumns;
use eframe::egui::{
    self, Align, Button, Color32, CursorIcon, FontId, Frame, Id, Key, Label, Layout, Margin,
    RichText, ScrollArea, Sense, Stroke, TextEdit, Ui, UiBuilder, Vec2,
};

use super::Action;
use super::fonts::{self, CatalogPhase, FacePhase, FontService};
use super::github_auth::{GitHubAuth, Notice, Stored, token_field_id};
use super::icons::{self, Icon};
use super::terminal::{self, Terminal};
use super::theme::{self, Palette};
use super::widgets;

const NAV_WIDTH: f32 = 260.0;
const PAGE_WIDTH: f32 = 780.0;
/// Half the settings column. The family field stays this wide, with Browse fonts beside it.
const FONT_FIELD_WIDTH: f32 = PAGE_WIDTH / 2.0;
const BROWSE_FONTS: &str = "Browse fonts";
const MODAL_WIDTH: f32 = 520.0;
const MODAL_FADE_SECS: f32 = 0.15;
const MODAL_BACKDROP_ALPHA: u8 = 160;
/// Space above and below the prompt, inside the preview border.
const PREVIEW_PAD: f32 = 8.0;
/// Scales the one-line preview so the prompt is not flush with the border.
const PREVIEW_HEIGHT_SCALE: f32 = 1.05;
const PAGE_TOP: f32 = 40.0;
/// Added under the last block. The column already keeps `COLUMN_SPACING` in the cursor.
const PAGE_END: f32 = 32.0;
/// Line and control spacing inside a page. Replaces egui's 3px default on this screen.
const COLUMN_SPACING: f32 = 8.0;
/// Extra gap before a section or the About card, on top of `COLUMN_SPACING`.
const BETWEEN_CARDS: f32 = 12.0;
const TITLE_SIZE: f32 = 24.0;
const SUBTITLE_SIZE: f32 = 16.0;
const SECTION_SIZE: f32 = 18.0;
const BODY_SIZE: f32 = 16.0;
const HELP_SIZE: f32 = 15.0;
const NAV_LABEL_SIZE: f32 = 16.0;
const BAR_TITLE_SIZE: f32 = 18.0;
const BUTTON_MIN: Vec2 = Vec2::new(0.0, 36.0);
const NAV_ITEM_GAP: f32 = 6.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Terminal,
    History,
    GitHubAuth,
    About,
}

#[derive(Clone, Default)]
struct FontPicker {
    open: bool,
    query: String,
}

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
    terminal_font: Option<&str>,
    fonts: &FontService,
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
        .unwrap_or(Page::Terminal);
    let mut picker = ui
        .ctx()
        .data(|data| data.get_temp::<FontPicker>(picker_id()))
        .unwrap_or_default();
    // Seeded at 0 the first time settings is shown, so opening fades in
    // instead of snapping to opaque. Closing keeps the modal up until 0.
    let fade = ui
        .ctx()
        .animate_bool_with_time(modal_fade_id(), picker.open, MODAL_FADE_SECS);
    let modal_visible = picker.open || fade > 0.0;

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
                    ui.label(
                        RichText::new("Settings")
                            .size(BAR_TITLE_SIZE)
                            .strong()
                            .color(palette.text),
                    );
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
                    if nav_button(ui, "Terminal", Icon::Terminal, page == Page::Terminal).clicked()
                    {
                        page = Page::Terminal;
                    }
                    if nav_button(ui, "History", Icon::CircleUserRound, page == Page::History)
                        .clicked()
                    {
                        page = Page::History;
                    }
                    if nav_button(ui, "GitHub Auth", Icon::KeyRound, page == Page::GitHubAuth)
                        .clicked()
                    {
                        page = Page::GitHubAuth;
                    }
                    if nav_button(ui, "About", Icon::Info, page == Page::About).clicked() {
                        page = Page::About;
                    }
                });
        });

    let view = fonts.view(terminal_font);
    egui::CentralPanel::default().show(ui, |ui| {
        ScrollArea::vertical()
            .id_salt("settings-page")
            .auto_shrink(false)
            .show(ui, |ui| {
                page_column(ui, |ui| match page {
                    Page::Terminal => {
                        terminal_page(
                            ui,
                            &palette,
                            terminal_font,
                            &view,
                            &mut picker,
                            modal_visible,
                            &mut actions,
                        );
                        if let Some(cwd) = cwd {
                            section(ui, |ui| {
                                centered_text(
                                    ui,
                                    RichText::new("Terminal Live Preview")
                                        .size(SUBTITLE_SIZE)
                                        .color(palette.text_muted),
                                );
                                shell_preview(ui, &palette, preview, cwd);
                            });
                        }
                    }
                    Page::History => {
                        history_page(ui, &palette, view_state.author_avatars, &mut actions);
                    }
                    Page::GitHubAuth => {
                        github_page(ui, &palette, github, &view_state.api_status);
                    }
                    Page::About => about_page(ui, &palette, &view_state.about),
                });
            });
    });

    if fade > 0.0 {
        font_modal(
            ui.ctx(),
            &palette,
            terminal_font,
            &view,
            &mut picker,
            fade,
            &mut actions,
        );
    }

    ui.ctx().data_mut(|data| {
        data.insert_temp(page_id(), page);
        data.insert_temp(picker_id(), picker);
    });
    actions
}

fn terminal_page(
    ui: &mut Ui,
    palette: &Palette,
    saved: Option<&str>,
    view: &fonts::FontView<'_>,
    picker: &mut FontPicker,
    modal_visible: bool,
    actions: &mut Vec<Action>,
) {
    page_header(
        ui,
        palette,
        "Terminal Font",
        "Family used by the terminal. Leave this empty to keep the built-in monospace.",
    );
    section(ui, |ui| {
        if let Some(name) = font_name_row(ui, saved, picker, modal_visible, actions) {
            actions.push(Action::SetTerminalFont(name));
        }
        font_hint(ui, palette, view);
    });
}

fn about_page(ui: &mut Ui, palette: &Palette, about: &AboutInfo) {
    ui.add_space(PAGE_TOP);
    centered_text(
        ui,
        RichText::new("Cthulhu-Git")
            .size(TITLE_SIZE)
            .strong()
            .color(palette.text),
    );
    about_logo(ui);
    centered_text(
        ui,
        RichText::new(&about.description)
            .size(SUBTITLE_SIZE)
            .color(palette.text_muted),
    );
    card(ui, palette, |ui| {
        about_row(ui, palette, "Version", &about.version, false);
        about_row(ui, palette, "Version date", &about.version_date, false);
        about_row(ui, palette, "Commit", &about.commit, true);
        about_row(ui, palette, "License", &about.license, false);
        about_row(ui, palette, "Git", &about.git_version, false);
        ui.horizontal(|ui| {
            icons::icon(ui, Icon::GitHub, palette.text);
            link(ui, palette, &about.repository, &about.repository);
        });
        let releases = about.releases_url();
        ui.horizontal(|ui| {
            icons::icon(ui, Icon::GitHub, palette.text);
            link(ui, palette, &releases, &releases);
        });
        // `horizontal` keeps this row at the button's height. A bare right-to-left
        // layout would claim the rest of the page and stretch the card.
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let copy = text_button(ui, "Copy version info", true);
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.set_min_width(ui.available_width());
                    help(ui, palette, "© 2026 Don Linux");
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
    let texture = logo_texture(ui.ctx());
    ui.vertical_centered(|ui| {
        ui.add(
            egui::Image::new(&texture)
                .fit_to_exact_size(Vec2::splat(ABOUT_LOGO))
                .alt_text("Cthulhu-Git logo"),
        );
    });
}

/// Uploaded once and kept for the life of the context, same as the interface icons.
fn logo_texture(ctx: &egui::Context) -> egui::TextureHandle {
    let id = Id::new("cthulhu-git-about-logo");
    if let Some(texture) = ctx.data(|data| data.get_temp(id)) {
        return texture;
    }
    let rgba = image::load_from_memory(include_bytes!(concat!(env!("OUT_DIR"), "/icon-256.png")))
        .expect("icon-256.png rendered by build.rs")
        .into_rgba8();
    let image = egui::ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    );
    let texture = ctx.load_texture(
        "cthulhu-git-about-logo",
        image,
        egui::TextureOptions::LINEAR,
    );
    ctx.data_mut(|data| data.insert_temp(id, texture.clone()));
    texture
}

fn about_row(ui: &mut Ui, palette: &Palette, label: &str, value: &str, monospace: bool) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(
            RichText::new(label)
                .size(HELP_SIZE)
                .color(palette.text_muted),
        );
        let mut text = RichText::new(value).size(BODY_SIZE).color(if monospace {
            palette.hash
        } else {
            palette.text
        });
        if monospace {
            text = text.monospace();
        }
        ui.label(text);
    });
}

fn history_page(ui: &mut Ui, palette: &Palette, avatars_on: bool, actions: &mut Vec<Action>) {
    page_header(
        ui,
        palette,
        "Commit History",
        "Author pictures and the width of the history columns.",
    );
    section(ui, |ui| {
        let mut on = avatars_on;
        if ui
            .checkbox(
                &mut on,
                RichText::new("Show author avatars")
                    .size(BODY_SIZE)
                    .color(palette.text),
            )
            .changed()
        {
            actions.push(Action::SetAuthorAvatars(on));
        }
        help(
            ui,
            palette,
            "Pictures come from GitHub (no-reply addresses, and the GitHub API for repositories \
             hosted there) and from Gravatar, which receives a SHA-256 hash of the address, never \
             the address itself. They are kept for a week in the user cache folder. Off draws \
             initials and sends nothing.",
        );
    });
    section(ui, |ui| {
        section_title(ui, palette, "Columns");
        help(
            ui,
            palette,
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
        palette,
        "GitHub Auth",
        "Optional. Without a token, public repositories still show avatars.",
    );
    section(ui, |ui| {
        body(
            ui,
            palette,
            "A GitHub token lets Cthulhu Git read the authors and avatars of your private \
             repositories, and raises GitHub's limit from 60 to 5,000 requests an hour. It is only \
             used to read: Cthulhu Git never writes to GitHub with it.",
        );
    });
    section(ui, |ui| {
        section_title(ui, palette, "1. Create a read-only token");
        link(
            ui,
            palette,
            "Open GitHub's token form, already set to read-only",
            TOKEN_TEMPLATE_URL,
        );
        help(
            ui,
            palette,
            "The form asks for Contents: read-only and Metadata: read-only. For an organization's \
             private repositories, choose that organization as the Resource owner.",
        );
    });

    let busy = github.busy();
    section(ui, |ui| {
        section_title(ui, palette, "2. Paste it here");
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
                        .font(field_font())
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
        section_title(ui, palette, "Status");
        let stored = match github.stored() {
            Stored::None => "No token. Public repositories only.",
            Stored::Keychain => "Token saved in the system keychain.",
            Stored::Session => "Token used for this session only; it is not saved.",
        };
        body(ui, palette, stored);
        if let Some(login) = github.user() {
            let limit = github
                .rate()
                .and_then(|rate| Some((rate.remaining?, rate.limit?)))
                .map(|(remaining, limit)| format!(" · {remaining}/{limit} requests left this hour"))
                .unwrap_or_default();
            colored(
                ui,
                &format!("Connected as {login}{limit}"),
                palette.diff_added,
            );
        }
        if api.token_rejected {
            colored(
                ui,
                "GitHub rejected the token; requests continue without it.",
                palette.warning,
            );
        }
        if let Some(until) = api.paused_until {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs())
                .unwrap_or(0);
            if until > now {
                let minutes = (until - now).div_ceil(60);
                colored(
                    ui,
                    &format!(
                        "GitHub's rate limit was reached. The API is asked again in {minutes} min."
                    ),
                    palette.warning,
                );
            }
        }
        match github.notice() {
            Some(Notice::Info(text)) => help(ui, palette, text),
            Some(Notice::Error(text)) => colored(ui, text, palette.error),
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
            palette,
            "Stored in Keychain on macOS, Credential Manager on Windows, or the Secret Service \
             (GNOME Keyring, KWallet) on Linux. Never in settings.json. It is sent only to \
             api.github.com, never to avatar hosts.",
        );
    });

    section(ui, |ui| {
        section_title(ui, palette, "Possible later with this token");
        for (feature, permission) in POSSIBLE_LATER {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("• {feature}"))
                        .size(BODY_SIZE)
                        .color(palette.text),
                );
                ui.label(
                    RichText::new(format!("({permission})"))
                        .size(HELP_SIZE)
                        .color(palette.text_muted),
                );
            });
        }
        help(
            ui,
            palette,
            "Writing to GitHub (pushing over HTTPS, opening pull requests) would need a different \
             token with write access, and is not part of this page.",
        );
    });
}

/// One prompt line, clipped out of a normal-sized shell. A one-row PTY scrolls
/// the prompt away. The shell measures the terminal font each frame, so a new
/// family shows up here without touching the repository session. Pointer and
/// keyboard are ignored.
fn shell_preview(ui: &mut Ui, palette: &Palette, preview: &mut Option<Terminal>, cwd: &Path) {
    let row = terminal_row_height(ui);
    let size = Vec2::new(
        ui.available_width(),
        (row + PREVIEW_PAD * 2.0) * PREVIEW_HEIGHT_SCALE,
    );
    let drawn = ui.allocate_ui(size, |ui| {
        ui.set_min_size(size);
        let outer = ui.max_rect();
        ui.painter().rect_filled(outer, 0.0, palette.background);
        let content = outer.shrink2(Vec2::new(0.0, PREVIEW_PAD));
        ui.scope_builder(UiBuilder::new().max_rect(content), |ui| {
            ui.shrink_clip_rect(content);
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
    Id::new("settings-font-preview")
}

/// Commits when the field loses focus, including Enter. Escape reverts.
/// Clicking another control reports the loss on the next frame, so the typed
/// text is kept in temporary data until then.
///
/// The modal is drawn after this field and consumes Escape to close. While it
/// is visible, Escape must not also revert the draft.
fn font_name_row(
    ui: &mut Ui,
    saved: Option<&str>,
    picker: &mut FontPicker,
    modal_visible: bool,
    actions: &mut Vec<Action>,
) -> Option<Option<String>> {
    let width = font_row_width(ui);
    let indent = ((ui.available_width() - width) / 2.0).max(0.0);
    // Right-to-left keeps the button ahead of the field, so a click this frame
    // already counts as the modal being up and Escape does not revert the draft.
    with_width_at(
        ui,
        indent,
        width,
        Layout::right_to_left(Align::Center),
        |ui| {
            if text_button(ui, BROWSE_FONTS, true).clicked() {
                picker.open = true;
                actions.push(Action::EnsureFontCatalog);
            }
            font_editor(ui, saved, modal_visible || picker.open, FONT_FIELD_WIDTH)
        },
    )
}

fn font_editor(
    ui: &mut Ui,
    saved: Option<&str>,
    modal_visible: bool,
    width: f32,
) -> Option<Option<String>> {
    let saved_text = saved.unwrap_or("");
    let escape = !modal_visible && ui.input(|input| input.key_pressed(Key::Escape));
    let mut draft = if escape {
        saved_text.to_owned()
    } else {
        ui.ctx()
            .data(|data| data.get_temp(draft_id()))
            .unwrap_or_else(|| saved_text.to_owned())
    };
    let response = ui.add(
        TextEdit::singleline(&mut draft)
            .id(font_field_id())
            .hint_text("Monospace")
            .font(field_font())
            .desired_width(width),
    );
    if response.has_focus() {
        ui.ctx()
            .data_mut(|data| data.insert_temp(draft_id(), draft.clone()));
        return None;
    }
    ui.ctx().data_mut(|data| data.remove::<String>(draft_id()));
    if response.lost_focus() && !escape {
        let name = normalize_draft(&draft);
        if name.as_deref() != saved {
            return Some(name);
        }
    }
    None
}

fn font_hint(ui: &mut Ui, palette: &Palette, view: &fonts::FontView<'_>) {
    let (text, error) = match &view.face {
        FacePhase::Builtin { pending: true } => ("Looking up installed fonts…", false),
        FacePhase::Missing => (
            "Font not found. The terminal is using the built-in monospace.",
            false,
        ),
        FacePhase::Ready { monospace: false } => (
            "This font is not monospace. The terminal grid expects a fixed width.",
            false,
        ),
        FacePhase::Unreadable(message) | FacePhase::ScanFailed(message) => (message.as_str(), true),
        FacePhase::Builtin { pending: false } | FacePhase::Ready { monospace: true } => return,
    };
    let indent = ((ui.available_width() - font_row_width(ui)) / 2.0).max(0.0);
    with_width_at(
        ui,
        indent,
        FONT_FIELD_WIDTH,
        Layout::top_down(Align::Min),
        |ui| {
            if error {
                colored(ui, text, palette.error);
            } else {
                note(ui, text, palette.text_muted);
            }
        },
    );
}

/// Width of Browse fonts, so the family field can stay `FONT_FIELD_WIDTH`
/// and the pair can be centered without laying the button out twice.
fn browse_button_width(ui: &Ui) -> f32 {
    let text = ui
        .painter()
        .layout_no_wrap(
            BROWSE_FONTS.to_owned(),
            FontId::proportional(BODY_SIZE),
            Color32::PLACEHOLDER,
        )
        .rect
        .width();
    text + ui.spacing().button_padding.x * 2.0
}

fn font_row_width(ui: &Ui) -> f32 {
    FONT_FIELD_WIDTH + ui.spacing().item_spacing.x + browse_button_width(ui)
}

/// A strip `width` wide, starting `indent` from the left of this row.
fn with_width_at<R>(
    ui: &mut Ui,
    indent: f32,
    width: f32,
    layout: Layout,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    ui.horizontal(|ui| {
        if indent > 0.0 {
            ui.add_space(indent);
        }
        let mut rect = ui.available_rect_before_wrap();
        rect.max.x = rect.min.x + width;
        ui.scope_builder(UiBuilder::new().max_rect(rect).layout(layout), add)
            .inner
    })
    .inner
}

fn font_modal(
    ctx: &egui::Context,
    palette: &Palette,
    saved: Option<&str>,
    view: &fonts::FontView<'_>,
    picker: &mut FontPicker,
    fade: f32,
    actions: &mut Vec<Action>,
) {
    let alpha = (f32::from(MODAL_BACKDROP_ALPHA) * fade)
        .round()
        .clamp(0.0, 255.0) as u8;
    let response = egui::Modal::new(modal_id())
        .frame(Frame::NONE)
        .backdrop_color(Color32::from_black_alpha(alpha))
        .show(ctx, |ui| {
            ui.set_opacity(fade);
            Frame::popup(ui.style())
                .fill(palette.surface)
                .stroke(Stroke::new(1.0, palette.border))
                .corner_radius(widgets::CORNER_RADIUS)
                .inner_margin(Margin::same(20))
                .show(ui, |ui| {
                    ui.set_min_width(MODAL_WIDTH);
                    ui.set_max_width(MODAL_WIDTH);
                    ui.spacing_mut().item_spacing.y = COLUMN_SPACING;
                    ui.label(
                        RichText::new("Terminal Font")
                            .size(TITLE_SIZE)
                            .strong()
                            .color(palette.text),
                    );
                    font_browser(ui, palette, saved, view, picker, actions);
                });
        });
    if response.should_close() {
        picker.open = false;
    }
}

fn font_browser(
    ui: &mut Ui,
    palette: &Palette,
    saved: Option<&str>,
    view: &fonts::FontView<'_>,
    picker: &mut FontPicker,
    actions: &mut Vec<Action>,
) {
    match view.catalog {
        CatalogPhase::Idle | CatalogPhase::Loading => {
            note(ui, "Looking up installed fonts…", palette.text_muted);
        }
        CatalogPhase::Failed(message) => colored(ui, message, palette.error),
        CatalogPhase::Ready(catalog) => {
            let mut query = picker.query.clone();
            ui.add(
                TextEdit::singleline(&mut query)
                    .id_salt("font-search")
                    .hint_text("Search fonts")
                    .font(field_font())
                    .desired_width(f32::INFINITY),
            );
            let query_changed = query != picker.query;
            if query_changed {
                picker.query = query;
            }

            if catalog.names().is_empty() {
                note(ui, "No installed fonts were found.", palette.text_muted);
                return;
            }
            // One page the size of the catalog is every match, so the modal can scroll.
            let listed = fonts::page_names(
                catalog.names(),
                &picker.query,
                0,
                catalog.names().len().max(1),
            );
            if listed.names.is_empty() {
                note(ui, "No fonts match.", palette.text_muted);
                return;
            }
            let list_height = (ui.ctx().content_rect().height() - 180.0).clamp(120.0, 360.0);
            ScrollArea::vertical()
                .id_salt("font-modal-list")
                .max_height(list_height)
                .auto_shrink([false, true])
                .animated(false)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if query_changed {
                        ui.scroll_to_cursor(Some(Align::TOP));
                    }
                    for name in &listed.names {
                        let selected = saved.is_some_and(|saved| same_family(saved, name));
                        if font_row(ui, palette, name, selected).clicked() {
                            ui.ctx().data_mut(|data| data.remove::<String>(draft_id()));
                            actions.push(Action::SetTerminalFont(Some(name.clone())));
                            picker.open = false;
                        }
                    }
                });
        }
    }
}

fn font_row(ui: &mut Ui, palette: &Palette, name: &str, selected: bool) -> egui::Response {
    let response = ui
        .scope_builder(UiBuilder::new().id_salt(name).sense(Sense::click()), |ui| {
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
                    ui.add(
                        egui::Label::new(RichText::new(name).size(BODY_SIZE).color(palette.text))
                            .selectable(false)
                            .truncate(),
                    );
                });
        })
        .response;
    response.on_hover_cursor(CursorIcon::PointingHand)
}

fn same_family(left: &str, right: &str) -> bool {
    left.to_lowercase() == right.to_lowercase()
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
                                egui::Label::new(
                                    RichText::new(label)
                                        .size(NAV_LABEL_SIZE)
                                        .color(palette.text),
                                )
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

fn note(ui: &mut Ui, text: &str, color: Color32) {
    ui.label(RichText::new(text).size(HELP_SIZE).color(color));
}

fn normalize_draft(draft: &str) -> Option<String> {
    let trimmed = draft.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn page_id() -> Id {
    Id::new("settings-page")
}

fn picker_id() -> Id {
    Id::new("settings-font-picker")
}

fn modal_id() -> Id {
    Id::new("settings-font-modal")
}

fn modal_fade_id() -> Id {
    Id::new("settings-font-modal-fade")
}

fn font_field_id() -> Id {
    Id::new("settings-terminal-font")
}

fn draft_id() -> Id {
    font_field_id().with("draft")
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

fn page_column(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    widgets::centered_column(ui, PAGE_WIDTH, |ui| {
        ui.spacing_mut().item_spacing.y = COLUMN_SPACING;
        add(ui);
        ui.add_space(PAGE_END);
    });
}

fn page_header(ui: &mut Ui, palette: &Palette, title: &str, subtitle: &str) {
    ui.add_space(PAGE_TOP);
    centered_text(
        ui,
        RichText::new(title)
            .size(TITLE_SIZE)
            .strong()
            .color(palette.text),
    );
    centered_text(
        ui,
        RichText::new(subtitle)
            .size(SUBTITLE_SIZE)
            .color(palette.text_muted),
    );
}

/// Centers each line in the column, including a subtitle that wraps.
fn centered_text(ui: &mut Ui, text: RichText) {
    ui.vertical_centered(|ui| {
        ui.add(Label::new(text).halign(Align::Center).selectable(false));
    });
}

fn section_title(ui: &mut Ui, palette: &Palette, title: &str) {
    ui.label(
        RichText::new(title)
            .size(SECTION_SIZE)
            .strong()
            .color(palette.text),
    );
}

fn body(ui: &mut Ui, palette: &Palette, text: &str) {
    colored(ui, text, palette.text);
}

fn help(ui: &mut Ui, palette: &Palette, text: &str) {
    note(ui, text, palette.text_muted);
}

fn colored(ui: &mut Ui, text: &str, color: Color32) {
    ui.label(RichText::new(text).size(BODY_SIZE).color(color));
}

fn link(ui: &mut Ui, palette: &Palette, label: &str, url: &str) {
    ui.hyperlink_to(
        RichText::new(label).size(BODY_SIZE).color(palette.accent),
        url,
    );
}

fn text_button(ui: &mut Ui, label: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(
        enabled,
        Button::new(RichText::new(label).size(BODY_SIZE)).min_size(BUTTON_MIN),
    )
    .on_hover_cursor(CursorIcon::PointingHand)
}

fn field_font() -> FontId {
    FontId::proportional(BODY_SIZE)
}

/// A block of the page with the gap of a card, and no fill or border.
fn section(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.add_space(BETWEEN_CARDS);
    ui.vertical(|ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = COLUMN_SPACING;
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
            ui.spacing_mut().item_spacing.y = COLUMN_SPACING;
            add(ui);
        });
}
