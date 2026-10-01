//! Color themes. A theme is only data (a [`Palette`]); [`apply`] is the one
//! place that turns it into egui visuals. The map for changing this module
//! is `docs/THEMES.md`.

mod dark;

use eframe::egui::{self, Color32, Context, Id, Stroke, Visuals};

/// Semantic colors. Views ask for a role (`text_muted`, `hash`), never for a
/// literal color, so a new theme only has to fill in this struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// Window and panel background.
    pub background: Color32,
    /// Cards, list rows, buttons at rest.
    pub surface: Color32,
    /// Hovered buttons and list rows.
    pub surface_hover: Color32,
    /// Pressed buttons, open menus and headers.
    pub surface_active: Color32,
    /// Text fields and scroll-area wells, darker (or lighter) than `surface`.
    pub input_background: Color32,
    /// Card outlines, separators, widget borders.
    pub border: Color32,
    pub text: Color32,
    /// Secondary text: field labels, paths, hints.
    pub text_muted: Color32,
    /// Primary buttons, focus rings, links.
    pub accent: Color32,
    /// Text drawn on top of `accent`.
    pub on_accent: Color32,
    /// Background of selected text.
    pub selection: Color32,
    /// Commit hashes.
    pub hash: Color32,
    /// Detached HEAD and other cautions.
    pub warning: Color32,
    pub error: Color32,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Theme {
    /// Stable key stored in `settings.json`; never rename a published id.
    pub id: &'static str,
    /// Human-readable name for a future theme picker.
    pub name: &'static str,
    /// Selects egui's dark or light base style under the palette.
    pub dark: bool,
    pub palette: Palette,
}

/// Every theme the app knows. Add new themes here.
pub const THEMES: &[Theme] = &[dark::THEME];

pub const DEFAULT_THEME: &Theme = &dark::THEME;

pub fn by_id(id: &str) -> Option<&'static Theme> {
    THEMES.iter().find(|theme| theme.id == id)
}

/// The theme saved in the settings, or the default when none or unknown.
pub fn resolve(id: Option<&str>) -> &'static Theme {
    id.and_then(by_id).unwrap_or(DEFAULT_THEME)
}

fn storage_id() -> Id {
    Id::new("cthulhu-git-theme")
}

/// Makes `theme` the active one. The OS light/dark preference is ignored:
/// the theme decides.
pub fn apply(ctx: &Context, theme: &'static Theme) {
    let base = if theme.dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    };
    ctx.set_theme(base);
    ctx.set_visuals_of(base, visuals(theme));
    ctx.data_mut(|data| data.insert_temp(storage_id(), theme));
}

/// The theme set by the last [`apply`].
pub fn current(ctx: &Context) -> &'static Theme {
    ctx.data(|data| data.get_temp::<&'static Theme>(storage_id()))
        .unwrap_or(DEFAULT_THEME)
}

fn visuals(theme: &Theme) -> Visuals {
    let palette = &theme.palette;
    let mut visuals = if theme.dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };

    let text = Stroke::new(1.0, palette.text);
    let widgets = &mut visuals.widgets;

    widgets.noninteractive.bg_fill = palette.background;
    widgets.noninteractive.weak_bg_fill = palette.surface;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, palette.border);
    widgets.noninteractive.fg_stroke = text;

    widgets.inactive.bg_fill = palette.surface;
    widgets.inactive.weak_bg_fill = palette.surface;
    widgets.inactive.bg_stroke = Stroke::new(1.0, palette.border);
    widgets.inactive.fg_stroke = text;

    widgets.hovered.bg_fill = palette.surface_hover;
    widgets.hovered.weak_bg_fill = palette.surface_hover;
    widgets.hovered.bg_stroke = Stroke::new(1.0, palette.accent);
    widgets.hovered.fg_stroke = text;

    widgets.active.bg_fill = palette.surface_active;
    widgets.active.weak_bg_fill = palette.surface_active;
    widgets.active.bg_stroke = Stroke::new(1.0, palette.accent);
    widgets.active.fg_stroke = text;

    widgets.open.bg_fill = palette.surface_active;
    widgets.open.weak_bg_fill = palette.surface_active;
    widgets.open.bg_stroke = Stroke::new(1.0, palette.border);
    widgets.open.fg_stroke = text;

    visuals.selection.bg_fill = palette.selection;
    visuals.selection.stroke = Stroke::new(1.0, palette.accent);

    visuals.override_text_color = None;
    visuals.weak_text_color = Some(palette.text_muted);
    visuals.hyperlink_color = palette.accent;
    visuals.faint_bg_color = palette.surface;
    visuals.extreme_bg_color = palette.input_background;
    visuals.text_edit_bg_color = Some(palette.input_background);
    visuals.code_bg_color = palette.surface;
    visuals.warn_fg_color = palette.warning;
    visuals.error_fg_color = palette.error;

    visuals.panel_fill = palette.background;
    visuals.window_fill = palette.surface;
    visuals.window_stroke = Stroke::new(1.0, palette.border);

    visuals
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_ids_are_unique() {
        for (index, theme) in THEMES.iter().enumerate() {
            assert!(
                THEMES[index + 1..].iter().all(|other| other.id != theme.id),
                "duplicate theme id {}",
                theme.id
            );
        }
    }

    #[test]
    fn default_theme_is_registered() {
        assert_eq!(by_id(DEFAULT_THEME.id), Some(DEFAULT_THEME));
    }

    #[test]
    fn unknown_or_missing_id_falls_back_to_default() {
        assert_eq!(resolve(None), DEFAULT_THEME);
        assert_eq!(resolve(Some("no-such-theme")), DEFAULT_THEME);
    }

    #[test]
    fn apply_sets_visuals_and_current() {
        let ctx = Context::default();
        let theme = DEFAULT_THEME;
        apply(&ctx, theme);
        assert_eq!(current(&ctx), theme);
        let style = ctx.global_style();
        assert_eq!(style.visuals.dark_mode, theme.dark);
        assert_eq!(style.visuals.panel_fill, theme.palette.background);
        assert_eq!(style.visuals.error_fg_color, theme.palette.error);
    }
}
