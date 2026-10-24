//! Settings screen. It replaces the whole window, including the repository
//! bottom bar. Back and Home sit at the left of the top bar, and the title
//! at the right. Pages are listed on the left; the chosen page is drawn on
//! the right. Both columns scroll.
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
    self, Align, Button, Color32, CursorIcon, Frame, Id, Key, Layout, Margin, RichText, ScrollArea,
    Sense, Stroke, TextEdit, Ui, UiBuilder, Vec2,
};

use super::Action;
use super::fonts::{self, CatalogPhase, FacePhase, FontService};
use super::github_auth::{GitHubAuth, Notice, Stored, token_field_id};
use super::icons::{self, Icon};
use super::terminal::{self, Terminal};
use super::theme::{self, Palette};
use super::widgets;

const NAV_WIDTH: f32 = 200.0;
const FORM_WIDTH: f32 = 560.0;
const MODAL_WIDTH: f32 = 440.0;
const MODAL_FADE_SECS: f32 = 0.15;
const MODAL_BACKDROP_ALPHA: u8 = 160;
/// Space above and below the prompt, inside the preview border.
const PREVIEW_PAD: f32 = 8.0;

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
                            .size(15.0)
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
                let width = ui.available_width().min(FORM_WIDTH);
                ui.set_min_width(width);
                ui.set_max_width(width);
                match page {
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
                            ui.add_space(8.0);
                            shell_preview(ui, &palette, preview, cwd);
                        }
                    }
                    Page::History => {
                        history_page(ui, &palette, view_state.author_avatars, &mut actions);
                    }
                    Page::GitHubAuth => {
                        github_page(ui, &palette, github, &view_state.api_status);
                    }
                    Page::About => about_page(ui, &palette, &view_state.about),
                }
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
    ui.add_space(8.0);
    ui.label(
        RichText::new("Terminal Font")
            .size(16.0)
            .strong()
            .color(palette.text),
    );
    ui.label(
        RichText::new(
            "Family used by the terminal. Leave this empty to keep the built-in monospace.",
        )
        .small()
        .color(palette.text_muted),
    );
    ui.add_space(12.0);

    if let Some(name) = font_name_row(ui, saved, picker, modal_visible, actions) {
        actions.push(Action::SetTerminalFont(name));
    }
    ui.add_space(8.0);
    font_hint(ui, palette, view);
}

fn about_page(ui: &mut Ui, palette: &Palette, about: &AboutInfo) {
    page_title(ui, palette, "Cthulhu-Git", &about.description);
    about_row(ui, palette, "Version", &about.version, false);
    about_row(ui, palette, "Version date", &about.version_date, false);
    about_row(ui, palette, "Commit", &about.commit, true);
    about_row(ui, palette, "License", &about.license, false);
    about_row(ui, palette, "Git", &about.git_version, false);
    ui.add_space(8.0);

    ui.horizontal(|ui| {
        icons::icon(ui, Icon::GitHub, palette.text);
        ui.hyperlink_to(
            RichText::new(&about.repository).color(palette.accent),
            &about.repository,
        );
    });
    let releases = about.releases_url();
    ui.hyperlink_to(RichText::new(&releases).color(palette.accent), &releases);
    ui.add_space(12.0);

    if ui
        .add(Button::new("Copy version info"))
        .on_hover_cursor(CursorIcon::PointingHand)
        .clicked()
    {
        ui.ctx().copy_text(about.copy_text());
    }
    ui.add_space(16.0);
    muted(ui, palette, "Copyright 2026 Fernando Diaz");
}

fn about_row(ui: &mut Ui, palette: &Palette, label: &str, value: &str, monospace: bool) {
    ui.label(RichText::new(label).small().color(palette.text_muted));
    ui.add_space(2.0);
    let mut text = RichText::new(value).color(if monospace {
        palette.hash
    } else {
        palette.text
    });
    if monospace {
        text = text.monospace();
    }
    ui.label(text);
    ui.add_space(10.0);
}

fn page_title(ui: &mut Ui, palette: &Palette, title: &str, subtitle: &str) {
    ui.add_space(8.0);
    ui.label(RichText::new(title).size(16.0).strong().color(palette.text));
    ui.label(RichText::new(subtitle).small().color(palette.text_muted));
    ui.add_space(12.0);
}

fn paragraph(ui: &mut Ui, palette: &Palette, text: &str) {
    ui.label(RichText::new(text).color(palette.text));
    ui.add_space(6.0);
}

fn muted(ui: &mut Ui, palette: &Palette, text: &str) {
    ui.label(RichText::new(text).small().color(palette.text_muted));
}

fn history_page(ui: &mut Ui, palette: &Palette, avatars_on: bool, actions: &mut Vec<Action>) {
    page_title(
        ui,
        palette,
        "Commit History",
        "Author pictures and the width of the history columns.",
    );
    let mut on = avatars_on;
    if ui
        .checkbox(
            &mut on,
            RichText::new("Show author avatars").color(palette.text),
        )
        .changed()
    {
        actions.push(Action::SetAuthorAvatars(on));
    }
    ui.add_space(4.0);
    muted(
        ui,
        palette,
        "Pictures come from GitHub (no-reply addresses, and the GitHub API for repositories \
         hosted there) and from Gravatar, which receives a SHA-256 hash of the address, never \
         the address itself. They are kept for a week in the user cache folder. Off draws \
         initials and sends nothing.",
    );
    ui.add_space(16.0);
    ui.label(RichText::new("Columns").strong().color(palette.text));
    muted(
        ui,
        palette,
        "Drag a divider in the history header to resize that column only. Double-click a \
         divider to restore that column. Widths follow the window and the open sidebars.",
    );
    ui.add_space(6.0);
    if ui
        .button("Reset all column widths")
        .on_hover_cursor(CursorIcon::PointingHand)
        .clicked()
    {
        actions.push(Action::SetHistoryColumns(HistoryColumns::default()));
    }
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
    page_title(
        ui,
        palette,
        "GitHub Auth",
        "Optional. Without a token, public repositories still show avatars.",
    );
    paragraph(
        ui,
        palette,
        "A GitHub token lets Cthulhu Git read the authors and avatars of your private \
         repositories, and raises GitHub's limit from 60 to 5,000 requests an hour. It is only \
         used to read: Cthulhu Git never writes to GitHub with it.",
    );

    ui.label(
        RichText::new("1. Create a read-only token")
            .strong()
            .color(palette.text),
    );
    ui.hyperlink_to(
        RichText::new("Open GitHub's token form, already set to read-only").color(palette.accent),
        TOKEN_TEMPLATE_URL,
    );
    muted(
        ui,
        palette,
        "The form asks for Contents: read-only and Metadata: read-only. For an organization's \
         private repositories, choose that organization as the Resource owner.",
    );
    ui.add_space(10.0);

    ui.label(
        RichText::new("2. Paste it here")
            .strong()
            .color(palette.text),
    );
    let busy = github.busy();
    ui.horizontal(|ui| {
        let field = ui.add_enabled(
            !busy,
            TextEdit::singleline(&mut github.draft)
                .id(token_field_id())
                .password(true)
                .hint_text("github_pat_…")
                .desired_width(FORM_WIDTH - 90.0),
        );
        let enter = field.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
        let save = ui
            .add_enabled(
                !busy && !github.draft.trim().is_empty(),
                Button::new("Save"),
            )
            .on_hover_cursor(CursorIcon::PointingHand)
            .clicked();
        if (save || enter) && !github.draft.trim().is_empty() {
            github.save_draft(ui.ctx());
        }
    });
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!busy, Button::new("Import from GitHub CLI"))
            .on_hover_text("Runs `gh auth token` and fills the field. Nothing is saved yet.")
            .clicked()
        {
            github.import_gh(ui.ctx());
        }
        if cthulhu_git::github::token_from_env().is_some()
            && ui
                .add_enabled(!busy, Button::new("Use GH_TOKEN"))
                .on_hover_text("Fills the field from GH_TOKEN or GITHUB_TOKEN.")
                .clicked()
        {
            github.use_env();
        }
    });
    ui.add_space(10.0);

    ui.label(RichText::new("Status").strong().color(palette.text));
    let stored = match github.stored() {
        Stored::None => "No token. Public repositories only.",
        Stored::Keychain => "Token saved in the system keychain.",
        Stored::Session => "Token used for this session only; it is not saved.",
    };
    ui.label(RichText::new(stored).color(palette.text));
    if let Some(login) = github.user() {
        let limit = github
            .rate()
            .and_then(|rate| Some((rate.remaining?, rate.limit?)))
            .map(|(remaining, limit)| format!(" · {remaining}/{limit} requests left this hour"))
            .unwrap_or_default();
        ui.label(RichText::new(format!("Connected as {login}{limit}")).color(palette.diff_added));
    }
    if api.token_rejected {
        ui.label(
            RichText::new("GitHub rejected the token; requests continue without it.")
                .color(palette.warning),
        );
    }
    if let Some(until) = api.paused_until {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0);
        if until > now {
            let minutes = (until - now).div_ceil(60);
            ui.label(
                RichText::new(format!(
                    "GitHub's rate limit was reached. The API is asked again in {minutes} min."
                ))
                .color(palette.warning),
            );
        }
    }
    match github.notice() {
        Some(Notice::Info(text)) => muted(ui, palette, text),
        Some(Notice::Error(text)) => {
            ui.label(RichText::new(text).color(palette.error));
        }
        None => {}
    }
    if busy {
        ui.spinner();
    }
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!busy && github.has_token(), Button::new("Verify"))
            .on_hover_text("Asks GitHub who the token belongs to.")
            .clicked()
        {
            github.verify(ui.ctx());
        }
        if ui
            .add_enabled(!busy && github.has_token(), Button::new("Remove token"))
            .clicked()
        {
            github.remove(ui.ctx());
        }
    });
    muted(
        ui,
        palette,
        "Stored in Keychain on macOS, Credential Manager on Windows, or the Secret Service \
         (GNOME Keyring, KWallet) on Linux. Never in settings.json. It is sent only to \
         api.github.com, never to avatar hosts.",
    );
    ui.add_space(16.0);

    ui.label(
        RichText::new("Possible later with this token")
            .strong()
            .color(palette.text),
    );
    for (feature, permission) in POSSIBLE_LATER {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(format!("• {feature}")).color(palette.text));
            ui.label(
                RichText::new(format!("({permission})"))
                    .small()
                    .color(palette.text_muted),
            );
        });
    }
    ui.add_space(4.0);
    muted(
        ui,
        palette,
        "Writing to GitHub (pushing over HTTPS, opening pull requests) would need a different \
         token with write access, and is not part of this page.",
    );
}

/// One prompt line, clipped out of a normal-sized shell. A one-row PTY scrolls
/// the prompt away. The shell measures the terminal font each frame, so a new
/// family shows up here without touching the repository session. Pointer and
/// keyboard are ignored.
fn shell_preview(ui: &mut Ui, palette: &Palette, preview: &mut Option<Terminal>, cwd: &Path) {
    let row = terminal_row_height(ui);
    let size = Vec2::new(ui.available_width(), row + PREVIEW_PAD * 2.0);
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
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add(Button::new("Browse fonts"))
                .on_hover_cursor(CursorIcon::PointingHand)
                .clicked()
            {
                picker.open = true;
                actions.push(Action::EnsureFontCatalog);
            }
            // The button runs first (`right_to_left`), so a click this frame
            // already counts as the modal being up.
            font_editor(ui, saved, modal_visible || picker.open)
        })
        .inner
    })
    .inner
}

fn font_editor(ui: &mut Ui, saved: Option<&str>, modal_visible: bool) -> Option<Option<String>> {
    let saved_text = saved.unwrap_or("");
    let escape = !modal_visible && ui.input(|input| input.key_pressed(Key::Escape));
    let mut draft = if escape {
        saved_text.to_owned()
    } else {
        ui.ctx()
            .data(|data| data.get_temp(draft_id()))
            .unwrap_or_else(|| saved_text.to_owned())
    };
    let width = ui.available_width();
    let response = ui.add(
        TextEdit::singleline(&mut draft)
            .id(font_field_id())
            .hint_text("Monospace")
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
    match &view.face {
        FacePhase::Builtin { pending: true } => {
            note(ui, "Looking up installed fonts…", palette.text_muted);
        }
        FacePhase::Missing => {
            note(
                ui,
                "Font not found. The terminal is using the built-in monospace.",
                palette.text_muted,
            );
        }
        FacePhase::Ready { monospace: false } => {
            note(
                ui,
                "This font is not monospace. The terminal grid expects a fixed width.",
                palette.text_muted,
            );
        }
        FacePhase::Unreadable(message) | FacePhase::ScanFailed(message) => {
            note(ui, message, palette.error);
        }
        FacePhase::Builtin { pending: false } | FacePhase::Ready { monospace: true } => {}
    }
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
                .inner_margin(Margin::same(16))
                .show(ui, |ui| {
                    ui.set_min_width(MODAL_WIDTH);
                    ui.set_max_width(MODAL_WIDTH);
                    ui.label(
                        RichText::new("Terminal Font")
                            .size(16.0)
                            .strong()
                            .color(palette.text),
                    );
                    ui.add_space(12.0);
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
        CatalogPhase::Failed(message) => note(ui, message, palette.error),
        CatalogPhase::Ready(catalog) => {
            let mut query = picker.query.clone();
            ui.add(
                TextEdit::singleline(&mut query)
                    .id_salt("font-search")
                    .hint_text("Search fonts")
                    .desired_width(f32::INFINITY),
            );
            let query_changed = query != picker.query;
            if query_changed {
                picker.query = query;
            }
            ui.add_space(8.0);

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
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(
                        egui::Label::new(RichText::new(name).color(palette.text))
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
                    .inner_margin(Margin::symmetric(8, 6))
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
                                egui::Label::new(RichText::new(label).color(palette.text))
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
    ui.label(RichText::new(text).color(color));
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
        .inner_margin(Margin::symmetric(8, 8))
}
