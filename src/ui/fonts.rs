//! Bundled faces for the interface and the terminal.
//!
//! Nothing is read from the operating system. Each face is compiled into the
//! binary. egui installs `set_fonts` on the following pass, so a named family
//! is not measured on the pass that first queues it. The terminal point
//! size changes on that same pass, with the face egui has actually loaded.
//!
//! The interface replaces egui's proportional face. The terminal uses the
//! named family `"terminal"` and, until that face is ready, the built-in
//! monospace. Hashes and diffs stay on that built-in monospace either way.

use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily, FontId, Id, TextStyle};

/// egui family that holds only the terminal face.
pub const TERMINAL_FAMILY: &str = "terminal";

/// Saved `ui_font` of `None` or an unknown id uses this face.
pub const DEFAULT_UI_ID: &str = "ubuntu";

/// Saved `terminal_font` of `None` or an unknown id uses this face.
pub const DEFAULT_TERMINAL_ID: &str = "meslo-lgs-nf";

/// Saved `terminal_font` that keeps egui's built-in monospace.
pub const BUILTIN_TERMINAL_ID: &str = "builtin";

/// Departure Mono is a pixel face. 11pt lands on its grid.
const DEPARTURE_SIZE: f32 = 11.0;

/// One copy of the file. Appearance and Terminal both offer this face.
const OBEL_MONO: &[u8] = include_bytes!("../../assets/fonts/obel-mono/ObelMono-Regular.ttf");

/// One face compiled into the app.
pub struct Face {
    pub id: &'static str,
    pub name: &'static str,
    pub note: &'static str,
    bytes: &'static [u8],
    /// Terminal cell size. `None` keeps the monospace text style.
    terminal_size: Option<f32>,
}

/// Interface faces, in the order the Appearance page lists them.
pub const UI_FACES: &[Face] = &[
    Face {
        id: DEFAULT_UI_ID,
        name: "Ubuntu",
        note: "Default",
        bytes: include_bytes!("../../assets/fonts/ubuntu/Ubuntu-Regular.ttf"),
        terminal_size: None,
    },
    Face {
        id: "ubuntu-mono",
        name: "Ubuntu Mono",
        note: "Monospace",
        bytes: include_bytes!("../../assets/fonts/ubuntu-mono/UbuntuMono-Regular.ttf"),
        terminal_size: None,
    },
    Face {
        id: "ubuntu-sans",
        name: "Ubuntu Sans",
        note: "Sans",
        bytes: include_bytes!("../../assets/fonts/ubuntu-sans/UbuntuSans-Regular.ttf"),
        terminal_size: None,
    },
    Face {
        id: "ibm-plex-sans",
        name: "IBM Plex Sans",
        note: "Sans",
        bytes: include_bytes!("../../assets/fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf"),
        terminal_size: None,
    },
    Face {
        id: "obel-mono",
        name: "Obel Mono",
        note: "Monospace",
        bytes: OBEL_MONO,
        terminal_size: None,
    },
    Face {
        id: "aspekta",
        name: "Aspekta",
        note: "Grotesque",
        bytes: include_bytes!("../../assets/fonts/aspekta/Aspekta-400.ttf"),
        terminal_size: None,
    },
    Face {
        id: "inter",
        name: "Inter",
        note: "Grotesque",
        bytes: include_bytes!("../../assets/fonts/inter/Inter-Regular.ttf"),
        terminal_size: None,
    },
];

/// Terminal faces, in the order the Terminal page lists them.
pub const TERMINAL_FACES: &[Face] = &[
    Face {
        id: DEFAULT_TERMINAL_ID,
        name: "Meslo LGS NF",
        note: "Default",
        bytes: include_bytes!("../../assets/fonts/meslo-lgs-nf/MesloLGS-NF-Regular.ttf"),
        terminal_size: None,
    },
    Face {
        id: "obel-mono",
        name: "Obel Mono",
        note: "Monospace",
        bytes: OBEL_MONO,
        terminal_size: None,
    },
    Face {
        id: "departure-mono",
        name: "Departure Mono",
        note: "Pixel, 11 pt",
        bytes: include_bytes!("../../assets/fonts/departure-mono/DepartureMono-Regular.otf"),
        terminal_size: Some(DEPARTURE_SIZE),
    },
];

/// A blank or unknown name is not a face.
pub fn face_by_id<'a>(faces: &'a [Face], name: &str) -> Option<&'a Face> {
    let name = name.trim();
    if name.is_empty() {
        None
    } else {
        faces.iter().find(|face| face.id.eq_ignore_ascii_case(name))
    }
}

/// Id actually drawn for the interface. Unknown names use [`DEFAULT_UI_ID`].
pub fn resolve_ui(saved: Option<&str>) -> &'static str {
    saved
        .and_then(|name| face_by_id(UI_FACES, name))
        .map(|face| face.id)
        .unwrap_or(DEFAULT_UI_ID)
}

/// `true` when `name` asks for egui's built-in monospace.
pub fn is_builtin_terminal(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case(BUILTIN_TERMINAL_ID)
}

/// Id drawn in the terminal. `None` means the built-in monospace, which is
/// stored as [`BUILTIN_TERMINAL_ID`]. A missing or unknown id uses
/// [`DEFAULT_TERMINAL_ID`].
pub fn resolve_terminal(saved: Option<&str>) -> Option<&'static str> {
    let Some(name) = saved.map(str::trim).filter(|name| !name.is_empty()) else {
        return Some(DEFAULT_TERMINAL_ID);
    };
    if is_builtin_terminal(name) {
        return None;
    }
    Some(
        face_by_id(TERMINAL_FACES, name)
            .map(|face| face.id)
            .unwrap_or(DEFAULT_TERMINAL_ID),
    )
}

/// The specimen font for a bundled face. Proportional until egui has loaded
/// the named families, which is the pass after they are first queued.
pub fn preview_font(ui: &egui::Ui, id: &str, size: f32) -> FontId {
    if faces_ready(ui.ctx()) {
        FontId::new(size, FontFamily::Name(id.into()))
    } else {
        FontId::proportional(size)
    }
}

/// The faces currently handed to egui.
#[derive(Default)]
pub struct FontService {
    applied_ui: String,
    applied_terminal: Option<String>,
    queued: bool,
    /// Named preview families are bound on the next pass.
    activate_faces_next_pass: bool,
    /// The terminal family is bound on the next pass.
    activate_terminal_next_pass: bool,
}

impl FontService {
    /// Installs `ui_font` and `terminal_font` when either id changed.
    ///
    /// A missing or unknown interface id is Ubuntu. A missing or unknown
    /// terminal id is Meslo LGS NF. [`BUILTIN_TERMINAL_ID`] keeps the built-in
    /// monospace.
    pub fn sync(
        &mut self,
        ctx: &egui::Context,
        ui_font: Option<&str>,
        terminal_font: Option<&str>,
    ) {
        // Fonts queued last pass are loaded before this call.
        if self.activate_faces_next_pass {
            set_faces_ready(ctx, true);
            self.activate_faces_next_pass = false;
        }
        if self.activate_terminal_next_pass {
            let active = self.applied_terminal.is_some();
            set_terminal_active(ctx, active);
            // Same pass that the queued face becomes measurable.
            store_terminal_size(ctx, self.applied_terminal.as_deref().and_then(point_size));
            self.activate_terminal_next_pass = false;
        }

        let ui_id = resolve_ui(ui_font);
        let terminal_id = resolve_terminal(terminal_font);
        if self.queued
            && self.applied_ui == ui_id
            && self.applied_terminal.as_deref() == terminal_id
        {
            return;
        }

        let had_terminal = self.queued && self.applied_terminal.is_some();
        let terminal_changed = self.applied_terminal.as_deref() != terminal_id;
        ctx.set_fonts(definitions(ui_id, terminal_id));
        ctx.request_repaint();
        self.applied_ui = ui_id.to_owned();
        self.applied_terminal = terminal_id.map(str::to_owned);
        let first = !self.queued;
        self.queued = true;

        if first || !faces_ready(ctx) {
            self.activate_faces_next_pass = true;
        }
        match terminal_id {
            // egui swaps the font map on the next pass. Until then the grid
            // keeps the previous point size; publishing the new one resizes
            // the PTY against the face that is still on screen.
            Some(_) if terminal_changed => {
                if !had_terminal {
                    set_terminal_active(ctx, false);
                }
                self.activate_terminal_next_pass = true;
            }
            Some(_) => set_terminal_active(ctx, true),
            None => {
                set_terminal_active(ctx, false);
                self.activate_terminal_next_pass = false;
                store_terminal_size(ctx, None);
            }
        }
    }
}

fn point_size(id: &str) -> Option<f32> {
    face_by_id(TERMINAL_FACES, id).and_then(|face| face.terminal_size)
}

/// The font the terminal grid measures. The named family is used only on a
/// pass where egui has already loaded it.
pub fn terminal_font_id(ui: &egui::Ui) -> FontId {
    let mut font = TextStyle::Monospace.resolve(ui.style());
    if terminal_active(ui.ctx()) {
        font.family = FontFamily::Name(TERMINAL_FAMILY.into());
        if let Some(size) = terminal_size(ui.ctx()) {
            font.size = size;
        }
    }
    font
}

fn faces_ready(ctx: &egui::Context) -> bool {
    ctx.data(|data| data.get_temp(faces_ready_id()).unwrap_or(false))
}

fn set_faces_ready(ctx: &egui::Context, ready: bool) {
    ctx.data_mut(|data| data.insert_temp(faces_ready_id(), ready));
}

fn faces_ready_id() -> Id {
    Id::new("cthulhu-git-bundled-faces")
}

fn terminal_active(ctx: &egui::Context) -> bool {
    ctx.data(|data| data.get_temp(terminal_active_id()).unwrap_or(false))
}

fn set_terminal_active(ctx: &egui::Context, active: bool) {
    ctx.data_mut(|data| data.insert_temp(terminal_active_id(), active));
}

fn terminal_active_id() -> Id {
    Id::new("cthulhu-git-terminal-font")
}

fn terminal_size(ctx: &egui::Context) -> Option<f32> {
    ctx.data(|data| data.get_temp(terminal_size_id()))
}

fn store_terminal_size(ctx: &egui::Context, size: Option<f32>) {
    ctx.data_mut(|data| {
        if let Some(size) = size {
            data.insert_temp(terminal_size_id(), size);
        } else {
            data.remove::<f32>(terminal_size_id());
        }
    });
}

fn terminal_size_id() -> Id {
    Id::new("cthulhu-git-terminal-font-size")
}

/// Proportional becomes `ui_id`. Every bundled face is also registered under
/// its own name so a row can draw a specimen before it is the one in use.
/// Emoji fallbacks stay at the end of each chain.
fn definitions(ui_id: &str, terminal_id: Option<&str>) -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let fallbacks = proportional_fallbacks(&fonts);
    let monospace = fonts
        .families
        .get(&FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();

    for face in UI_FACES.iter().chain(TERMINAL_FACES) {
        // Obel Mono is listed for the interface and the terminal. One id, one face.
        if fonts.font_data.contains_key(face.id) {
            continue;
        }
        fonts.font_data.insert(
            face.id.to_owned(),
            Arc::new(FontData::from_static(face.bytes)),
        );
        let mut chain = vec![face.id.to_owned()];
        chain.extend(fallbacks.iter().cloned());
        fonts
            .families
            .insert(FontFamily::Name(face.id.into()), chain);
    }

    let mut proportional = vec![ui_id.to_owned()];
    proportional.extend(fallbacks);
    fonts
        .families
        .insert(FontFamily::Proportional, proportional);

    if let Some(id) = terminal_id {
        let mut chain = vec![id.to_owned()];
        chain.extend(monospace);
        fonts
            .families
            .insert(FontFamily::Name(TERMINAL_FAMILY.into()), chain);
    }
    fonts
}

/// egui's proportional list starts with Ubuntu Light. The rest are emoji.
fn proportional_fallbacks(fonts: &FontDefinitions) -> Vec<String> {
    fonts
        .families
        .get(&FontFamily::Proportional)
        .map(|names| {
            names
                .iter()
                .filter(|name| name.as_str() != "Ubuntu-Light")
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// nf-oct-terminal. Meslo LGS NF has it; Obel Mono does not.
    const PROMPT_ICON: char = '\u{f120}';

    #[test]
    fn only_obel_mono_is_in_both_catalogs() {
        let shared: Vec<_> = UI_FACES
            .iter()
            .filter(|face| face_by_id(TERMINAL_FACES, face.id).is_some())
            .map(|face| face.id)
            .collect();
        assert_eq!(shared, ["obel-mono"]);
    }

    /// (saved id, interface face, terminal face). Case and the whitespace
    /// around an id do not matter; anything else that is not a bundled id is
    /// the default. Only `builtin` turns the terminal face off.
    #[test]
    fn saved_ids_resolve_to_a_bundled_face() {
        const UI: &str = "ubuntu";
        const TERMINAL: Option<&str> = Some("meslo-lgs-nf");
        for (saved, ui, terminal) in [
            (None, UI, TERMINAL),
            (Some(""), UI, TERMINAL),
            (Some(" \n\t "), UI, TERMINAL),
            (Some("Comic Sans"), UI, TERMINAL),
            (Some("Fira Code"), UI, TERMINAL),
            (Some("obel\nmono"), UI, TERMINAL),
            (Some("\u{202e}obel-mono"), UI, TERMINAL),
            (Some("/tmp/cthulhu-font-probe.ttf"), UI, TERMINAL),
            (Some(" INTER "), "inter", TERMINAL),
            (Some("inter\n"), "inter", TERMINAL),
            (Some("UBUNTU-SANS"), "ubuntu-sans", TERMINAL),
            (Some(" Obel-Mono "), "obel-mono", Some("obel-mono")),
            (Some("meslo-lgs-nf"), UI, Some("meslo-lgs-nf")),
            (Some("Departure-Mono"), UI, Some("departure-mono")),
            (Some("builtin"), UI, None),
            (Some(" BUILTIN "), UI, None),
        ] {
            assert_eq!(
                (resolve_ui(saved), resolve_terminal(saved)),
                (ui, terminal),
                "{saved:?}"
            );
        }
    }

    proptest! {
        #![proptest_config(crate::ui::test_support::proptest_config())]

        /// Whatever the settings file holds, both faces come from the catalog.
        #[test]
        fn every_saved_id_resolves_inside_the_catalog(
            saved in prop_oneof![
                any::<String>(),
                "[ \t\n]{0,2}(INTER|ubuntu-mono|Obel-Mono|meslo-lgs-nf|DEPARTURE-mono)[ \t\n]{0,2}",
                "[ \t\n]{0,2}(Builtin|built-in|BUILTIN)[ \t\n]{0,2}",
            ],
        ) {
            let ui = resolve_ui(Some(saved.as_str()));
            prop_assert!(UI_FACES.iter().any(|face| face.id == ui), "{saved:?} drew {ui}");
            match resolve_terminal(Some(saved.as_str())) {
                Some(id) => {
                    let bundled = TERMINAL_FACES.iter().any(|face| face.id == id);
                    prop_assert!(bundled, "{saved:?} drew {id}");
                }
                None => {
                    prop_assert!(is_builtin_terminal(&saved), "{saved:?} turned the face off");
                }
            }
        }
    }

    /// egui binds `set_fonts` at the start of the next pass. Measuring a named
    /// family on the install pass is the panic this guards.
    #[test]
    fn named_faces_are_measured_on_the_pass_after_they_are_queued() {
        let ctx = egui::Context::default();
        let mut service = FontService::default();

        ctx.begin_pass(egui::RawInput::default());
        service.sync(&ctx, Some("inter"), Some("meslo-lgs-nf"));
        assert!(!faces_ready(&ctx));
        assert!(!terminal_active(&ctx));
        finish_pass(&ctx);

        ctx.begin_pass(egui::RawInput::default());
        service.sync(&ctx, Some("inter"), Some("meslo-lgs-nf"));
        assert!(faces_ready(&ctx));
        assert!(terminal_active(&ctx));
        assert!(terminal_size(&ctx).is_none());
        ctx.fonts_mut(|fonts| {
            for face in UI_FACES.iter().chain(TERMINAL_FACES) {
                let id = FontId::new(16.0, FontFamily::Name(face.id.into()));
                assert!(fonts.glyph_width(&id, 'M') > 0.0, "{}", face.id);
            }
        });
        finish_pass(&ctx);

        ctx.begin_pass(egui::RawInput::default());
        service.sync(&ctx, Some("obel-mono"), Some("departure-mono"));
        assert!(faces_ready(&ctx));
        assert!(terminal_active(&ctx));
        // Meslo is still loaded. Its size (none) stays until the next pass.
        assert!(terminal_size(&ctx).is_none());
        ctx.fonts(|fonts| {
            let chain = fonts
                .definitions()
                .families
                .get(&FontFamily::Name(TERMINAL_FAMILY.into()))
                .expect("terminal family");
            assert_eq!(chain.first().map(String::as_str), Some("meslo-lgs-nf"));
        });
        finish_pass(&ctx);

        ctx.begin_pass(egui::RawInput::default());
        service.sync(&ctx, Some("obel-mono"), Some("departure-mono"));
        assert!(faces_ready(&ctx));
        assert!(terminal_active(&ctx));
        assert_eq!(terminal_size(&ctx), Some(DEPARTURE_SIZE));
        let named = FontId::new(DEPARTURE_SIZE, FontFamily::Name(TERMINAL_FAMILY.into()));
        ctx.fonts_mut(|fonts| {
            let chain = fonts
                .definitions()
                .families
                .get(&FontFamily::Name(TERMINAL_FAMILY.into()))
                .expect("terminal family");
            assert_eq!(chain.first().map(String::as_str), Some("departure-mono"));
            assert!(fonts.glyph_width(&named, 'M') > 0.0);
        });
        finish_pass(&ctx);

        // A blank setting is Meslo. The face egui has loaded is still Departure
        // until the next pass, so the 11 pt size stays on this one.
        ctx.begin_pass(egui::RawInput::default());
        service.sync(&ctx, None, None);
        assert!(faces_ready(&ctx));
        assert!(terminal_active(&ctx));
        assert_eq!(terminal_size(&ctx), Some(DEPARTURE_SIZE));
        let named = FontId::new(DEPARTURE_SIZE, FontFamily::Name(TERMINAL_FAMILY.into()));
        ctx.fonts_mut(|fonts| {
            assert!(fonts.glyph_width(&named, 'M') > 0.0);
            let chain = fonts
                .definitions()
                .families
                .get(&FontFamily::Name(TERMINAL_FAMILY.into()))
                .expect("terminal family");
            assert_eq!(chain.first().map(String::as_str), Some("departure-mono"));
        });
        finish_pass(&ctx);

        ctx.begin_pass(egui::RawInput::default());
        service.sync(&ctx, None, None);
        assert_loaded(&ctx, DEFAULT_UI_ID, Some(DEFAULT_TERMINAL_ID));
        finish_pass(&ctx);

        // `builtin` is the choice that drops the named family, on this pass.
        ctx.begin_pass(egui::RawInput::default());
        service.sync(&ctx, None, Some("builtin"));
        assert!(faces_ready(&ctx));
        assert!(!terminal_active(&ctx));
        assert!(terminal_size(&ctx).is_none());
        finish_pass(&ctx);
    }

    /// Switching Meslo to Departure must not publish 11 pt while Meslo is
    /// still the face egui has loaded.
    #[test]
    fn departure_install_pass_keeps_the_meslo_chain_and_previous_size() {
        let ctx = egui::Context::default();
        let mut service = FontService::default();
        settle(&ctx, &mut service, None, Some("meslo-lgs-nf"));
        assert!(terminal_active(&ctx));
        assert!(terminal_size(&ctx).is_none());
        let previous = terminal_font_size(&ctx);
        finish_pass(&ctx);

        ctx.begin_pass(egui::RawInput::default());
        service.sync(&ctx, None, Some("departure-mono"));
        assert!(terminal_active(&ctx));
        assert_eq!(terminal_size(&ctx), None);
        assert_eq!(terminal_chain(&ctx).as_deref(), Some("meslo-lgs-nf"));
        assert_eq!(terminal_font_size(&ctx), previous);
        finish_pass(&ctx);

        ctx.begin_pass(egui::RawInput::default());
        service.sync(&ctx, None, Some("departure-mono"));
        assert_eq!(terminal_size(&ctx), Some(DEPARTURE_SIZE));
        assert_eq!(terminal_chain(&ctx).as_deref(), Some("departure-mono"));
        assert_eq!(terminal_font_size(&ctx), DEPARTURE_SIZE);
        let named = FontId::new(DEPARTURE_SIZE, FontFamily::Name(TERMINAL_FAMILY.into()));
        ctx.fonts_mut(|fonts| {
            assert!(fonts.glyph_width(&named, 'M') > 0.0);
        });
        finish_pass(&ctx);
    }

    fn terminal_chain(ctx: &egui::Context) -> Option<String> {
        ctx.fonts(|fonts| {
            fonts
                .definitions()
                .families
                .get(&FontFamily::Name(TERMINAL_FAMILY.into()))
                .and_then(|chain| chain.first().cloned())
        })
    }

    fn terminal_font_size(ctx: &egui::Context) -> f32 {
        let mut size = 0.0;
        let mut ran = false;
        egui::Area::new(Id::new("cthulhu-terminal-size-probe")).show(ctx, |ui| {
            size = terminal_font_id(ui).size;
            ran = true;
        });
        assert!(ran, "terminal size probe did not run");
        size
    }

    /// Measuring `terminal` on the pass that first installs it panics. Blank
    /// and unknown names stay on Meslo. Only `builtin` turns that family off,
    /// and it turns off before the next pass drops it from the font map.
    #[test]
    fn hostile_terminal_names_wait_one_pass_and_only_builtin_turns_the_face_off() {
        let ctx = egui::Context::default();
        let mut service = FontService::default();

        for spelling in [" BUILTIN ", "Builtin"] {
            settle(&ctx, &mut service, None, Some("departure-mono"));
            finish_pass(&ctx);
            ctx.begin_pass(egui::RawInput::default());
            service.sync(&ctx, None, Some(spelling));
            assert!(!terminal_active(&ctx), "{spelling}");
            assert!(terminal_size(&ctx).is_none(), "{spelling}");
            assert_terminal_grid(&ctx, false);
            finish_pass(&ctx);
            ctx.begin_pass(egui::RawInput::default());
            service.sync(&ctx, None, Some(spelling));
            assert_loaded(&ctx, DEFAULT_UI_ID, None);
            assert_terminal_grid(&ctx, false);
            finish_pass(&ctx);
        }

        let lookalikes = [
            None,
            Some(""),
            Some(" \n\t "),
            Some("Fira Code"),
            Some("\u{202e}builtin"),
            Some("builtin.ttf"),
            Some("built-in"),
            Some("/tmp/cthulhu-font-probe.ttf"),
        ];
        for name in lookalikes {
            settle(&ctx, &mut service, None, Some("departure-mono"));
            finish_pass(&ctx);
            ctx.begin_pass(egui::RawInput::default());
            service.sync(&ctx, None, name);
            assert!(terminal_active(&ctx), "{name:?}");
            assert_eq!(terminal_size(&ctx), Some(DEPARTURE_SIZE), "{name:?}");
            assert_terminal_grid(&ctx, true);
            let named = FontId::new(16.0, FontFamily::Name(TERMINAL_FAMILY.into()));
            ctx.fonts_mut(|fonts| {
                assert!(fonts.glyph_width(&named, 'M') > 0.0, "{name:?}");
                let chain = fonts
                    .definitions()
                    .families
                    .get(&FontFamily::Name(TERMINAL_FAMILY.into()))
                    .expect("terminal family");
                assert_eq!(
                    chain.first().map(String::as_str),
                    Some("departure-mono"),
                    "{name:?}"
                );
            });
            finish_pass(&ctx);
            ctx.begin_pass(egui::RawInput::default());
            service.sync(&ctx, None, name);
            assert_loaded(&ctx, DEFAULT_UI_ID, Some(DEFAULT_TERMINAL_ID));
            assert_terminal_grid(&ctx, true);
            finish_pass(&ctx);
        }

        // From the built-in face, Departure's 11 pt arrives with the face.
        // The grid keeps the style monospace until that pass.
        settle(&ctx, &mut service, None, Some("builtin"));
        finish_pass(&ctx);
        ctx.begin_pass(egui::RawInput::default());
        service.sync(&ctx, None, Some("departure-mono"));
        assert!(!terminal_active(&ctx));
        assert!(terminal_size(&ctx).is_none());
        assert_terminal_grid(&ctx, false);
        finish_pass(&ctx);
        ctx.begin_pass(egui::RawInput::default());
        service.sync(&ctx, None, Some("departure-mono"));
        assert!(terminal_active(&ctx));
        assert_eq!(terminal_size(&ctx), Some(DEPARTURE_SIZE));
        assert_terminal_grid(&ctx, true);
        let named = FontId::new(DEPARTURE_SIZE, FontFamily::Name(TERMINAL_FAMILY.into()));
        ctx.fonts_mut(|fonts| {
            assert!(fonts.glyph_width(&named, 'M') > 0.0);
        });
        finish_pass(&ctx);

        for blank in [None, Some(""), Some(" \n\t ")] {
            settle(&ctx, &mut service, None, Some("builtin"));
            finish_pass(&ctx);
            ctx.begin_pass(egui::RawInput::default());
            service.sync(&ctx, None, blank);
            assert!(!terminal_active(&ctx), "{blank:?}");
            assert!(terminal_size(&ctx).is_none(), "{blank:?}");
            assert_terminal_grid(&ctx, false);
            finish_pass(&ctx);
            ctx.begin_pass(egui::RawInput::default());
            service.sync(&ctx, None, blank);
            assert_loaded(&ctx, DEFAULT_UI_ID, Some(DEFAULT_TERMINAL_ID));
            assert_terminal_grid(&ctx, true);
            let named = FontId::new(16.0, FontFamily::Name(TERMINAL_FAMILY.into()));
            ctx.fonts_mut(|fonts| {
                assert!(fonts.glyph_width(&named, 'M') > 0.0, "{blank:?}");
            });
            finish_pass(&ctx);
        }
    }

    /// Each pair puts its interface face first in the proportional chain and
    /// its terminal face first in the terminal chain, with egui's monospace
    /// behind it; no terminal face installs no terminal family. Every bundled
    /// face stays registered under its own name.
    #[test]
    fn every_face_pair_builds_both_chains_without_clobbering_the_other() {
        let terminal_family = FontFamily::Name(TERMINAL_FAMILY.into());
        let terminals =
            std::iter::once(None).chain(TERMINAL_FACES.iter().map(|face| Some(face.id)));
        for ui in UI_FACES {
            for terminal in terminals.clone() {
                let case = format!("{} with {terminal:?}", ui.id);
                let fonts = definitions(ui.id, terminal);
                let first = |family: &FontFamily| {
                    fonts
                        .families
                        .get(family)
                        .and_then(|chain| chain.first())
                        .map(String::as_str)
                };
                assert_eq!(first(&FontFamily::Proportional), Some(ui.id), "{case}");
                assert_eq!(first(&terminal_family), terminal, "{case}");
                if terminal.is_some() {
                    assert!(
                        fonts.families[&terminal_family].len() > 1,
                        "{case}: monospace fallback was dropped"
                    );
                }
                for face in UI_FACES.iter().chain(TERMINAL_FACES) {
                    let named = FontFamily::Name(face.id.into());
                    assert_eq!(first(&named), Some(face.id), "{case}: {}", face.id);
                    assert!(fonts.font_data.contains_key(face.id), "{case}: {}", face.id);
                }
            }
        }

        // One real load. A missing glyph measures zero wide, so Meslo's prompt
        // icon is its own glyph, not a fallback's.
        let ctx = egui::Context::default();
        let mut service = FontService::default();
        settle(&ctx, &mut service, None, Some(DEFAULT_TERMINAL_ID));
        ctx.fonts_mut(|fonts| {
            let meslo = FontId::new(14.0, FontFamily::Name(DEFAULT_TERMINAL_ID.into()));
            let obel = FontId::new(14.0, FontFamily::Name("obel-mono".into()));
            assert!(fonts.glyph_width(&obel, 'M') > 0.0);
            assert!(fonts.glyph_width(&meslo, PROMPT_ICON) > 0.0);
            assert_eq!(fonts.glyph_width(&obel, PROMPT_ICON), 0.0);
        });
        finish_pass(&ctx);
    }

    /// One pass queues the faces. The next pass is the one egui has loaded,
    /// and it is left open so the caller can measure.
    fn settle(
        ctx: &egui::Context,
        service: &mut FontService,
        ui: Option<&str>,
        terminal: Option<&str>,
    ) {
        ctx.begin_pass(egui::RawInput::default());
        service.sync(ctx, ui, terminal);
        finish_pass(ctx);
        ctx.begin_pass(egui::RawInput::default());
        service.sync(ctx, ui, terminal);
    }

    fn assert_loaded(ctx: &egui::Context, ui: &str, terminal: Option<&str>) {
        assert!(faces_ready(ctx));
        match terminal {
            Some("departure-mono") => {
                assert!(terminal_active(ctx));
                assert_eq!(terminal_size(ctx), Some(DEPARTURE_SIZE));
            }
            Some(_) => {
                assert!(terminal_active(ctx));
                assert!(terminal_size(ctx).is_none());
            }
            None => {
                assert!(!terminal_active(ctx));
                assert!(terminal_size(ctx).is_none());
            }
        }
        ctx.fonts(|fonts| {
            let definitions = fonts.definitions();
            let proportional = definitions
                .families
                .get(&FontFamily::Proportional)
                .expect("proportional");
            assert_eq!(proportional.first().map(String::as_str), Some(ui));
            let terminal_family = FontFamily::Name(TERMINAL_FAMILY.into());
            match terminal {
                Some(id) => {
                    let chain = definitions
                        .families
                        .get(&terminal_family)
                        .expect("terminal family");
                    assert_eq!(chain.first().map(String::as_str), Some(id));
                    assert!(chain.len() > 1, "monospace fallback was dropped");
                }
                None => assert!(
                    !definitions.families.contains_key(&terminal_family),
                    "built-in terminal still installed a named face"
                ),
            }
        });
    }

    fn finish_pass(ctx: &egui::Context) {
        // Dropping an unapplied texture delta panics. This test has no renderer.
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
    }

    /// `named` is the terminal family. Otherwise the grid stays on egui's
    /// monospace, including its size, even when a custom size is already stored.
    fn assert_terminal_grid(ctx: &egui::Context, named: bool) {
        let mut ran = false;
        egui::Area::new(Id::new("cthulhu-terminal-grid-probe")).show(ctx, |ui| {
            let font = terminal_font_id(ui);
            let monospace = TextStyle::Monospace.resolve(ui.style());
            if named {
                assert_eq!(font.family, FontFamily::Name(TERMINAL_FAMILY.into()));
                assert_eq!(font.size, terminal_size(ui.ctx()).unwrap_or(monospace.size));
            } else {
                assert_eq!(font.family, FontFamily::Monospace);
                assert_eq!(font.size, monospace.size);
            }
            ran = true;
        });
        assert!(ran, "terminal grid probe did not run");
    }
}
