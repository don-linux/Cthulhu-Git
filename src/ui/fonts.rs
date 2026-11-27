//! Bundled faces for the interface and the terminal.
//!
//! Each face is compiled into the binary. Behind it in every chain come
//! egui's bundled icon and monochrome emoji fonts, so emoji look the same on
//! every system. Only a character none of them has, such as CJK, comes from
//! the system fonts eframe looks up (its `system_fonts` feature).
//!
//! egui loads `set_fonts` at once when no text has been laid out yet in the
//! pass, otherwise on the next pass. A named family egui has not loaded draws
//! boxes, so the specimens and the terminal grid read the fonts egui has
//! loaded, never the ones last asked for. The terminal point size belongs to
//! the face loaded in the same font map.
//!
//! The interface replaces egui's proportional face. The terminal uses the
//! named family `"terminal"` and, until that face is loaded, the built-in
//! monospace. Hashes and diffs stay on that built-in monospace either way.

use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily, FontId, TextStyle};

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
/// the named family.
pub fn preview_font(ui: &egui::Ui, id: &str, size: f32) -> FontId {
    let family = FontFamily::Name(id.into());
    if ui.fonts(|fonts| fonts.definitions().families.contains_key(&family)) {
        FontId::new(size, family)
    } else {
        FontId::proportional(size)
    }
}

/// The faces last handed to egui.
#[derive(Default)]
pub struct FontService {
    requested: Option<(&'static str, Option<&'static str>)>,
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
        let wanted = (resolve_ui(ui_font), resolve_terminal(terminal_font));
        if self.requested != Some(wanted) {
            ctx.set_fonts(definitions(wanted.0, wanted.1));
            // Text already laid out on this pass leaves the change to the next.
            ctx.request_repaint();
            self.requested = Some(wanted);
        }
    }
}

/// The font the terminal grid measures: the terminal face egui has loaded, at
/// that face's point size, or else the monospace text style.
pub fn terminal_font_id(ui: &egui::Ui) -> FontId {
    let mut font = TextStyle::Monospace.resolve(ui.style());
    let family = FontFamily::Name(TERMINAL_FAMILY.into());
    let loaded = ui.fonts(|fonts| {
        let chain = fonts.definitions().families.get(&family)?;
        face_by_id(TERMINAL_FACES, chain.first()?)
    });
    if let Some(face) = loaded {
        font.family = family;
        if let Some(size) = face.terminal_size {
            font.size = size;
        }
    }
    font
}

/// Proportional becomes `ui_id`. Every bundled face is also registered under
/// its own name so a row can draw a specimen before it is the one in use.
/// egui's icon and emoji fallbacks stay at the end of each chain.
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

/// egui's proportional list starts with Ubuntu Light. The rest are its icon
/// and monochrome emoji fonts.
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
            (Some("\u{202e}builtin"), UI, TERMINAL),
            (Some("builtin.ttf"), UI, TERMINAL),
            (Some("built-in"), UI, TERMINAL),
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

    /// egui loads `set_fonts` on the same pass when no text has been laid out
    /// yet, otherwise on the next. A named family is only measured once it is
    /// loaded, and the terminal size belongs to the loaded face.
    #[test]
    fn faces_are_measured_only_once_egui_has_loaded_them() {
        const MESLO: Option<&str> = Some(DEFAULT_TERMINAL_ID);
        const DEPARTURE: Option<&str> = Some("departure-mono");
        let ctx = egui::Context::default();
        let mut service = FontService::default();
        // (case, interface, terminal, text laid out first, loaded proportional, loaded terminal)
        #[rustfmt::skip]
        let passes = [
            ("first faces after text", Some("inter"), MESLO, true, "Ubuntu-Light", None),
            ("next pass", Some("inter"), MESLO, false, "inter", MESLO),
            ("Departure after text keeps Meslo", Some("obel-mono"), DEPARTURE, true, "inter", MESLO),
            ("Departure at 11 pt", Some("obel-mono"), DEPARTURE, false, "obel-mono", DEPARTURE),
            ("blank before text", None, None, false, DEFAULT_UI_ID, MESLO),
            ("builtin after text keeps Meslo", None, Some("builtin"), true, DEFAULT_UI_ID, MESLO),
            ("builtin drops the family", None, Some("builtin"), false, DEFAULT_UI_ID, None),
            ("Departure from builtin", None, DEPARTURE, false, DEFAULT_UI_ID, DEPARTURE),
        ];
        for (case, ui_font, terminal_font, text_first, proportional, terminal) in passes {
            ctx.begin_pass(egui::RawInput::default());
            if text_first {
                ctx.fonts_mut(|fonts| {
                    fonts.layout_no_wrap("x".into(), FontId::default(), egui::Color32::WHITE)
                });
            }
            service.sync(&ctx, ui_font, terminal_font);
            let loaded = ctx.fonts(|fonts| {
                let chain = |family| fonts.definitions().families.get(&family)?.first().cloned();
                let terminal = chain(FontFamily::Name(TERMINAL_FAMILY.into()));
                (chain(FontFamily::Proportional), terminal)
            });
            assert_eq!(loaded.0.as_deref(), Some(proportional), "{case}");
            assert_eq!(loaded.1.as_deref(), terminal, "{case}");
            let bundled = face_by_id(UI_FACES, proportional).is_some();
            let mut ran = false;
            egui::Area::new(egui::Id::unique("cthulhu-font-probe")).show(&ctx, |ui| {
                for face in UI_FACES {
                    let font = preview_font(ui, face.id, 16.0);
                    let named = font.family != FontFamily::Proportional;
                    assert_eq!(named, bundled, "{case}: {}", face.id);
                    assert!(
                        ui.fonts_mut(|fonts| fonts.glyph_width(&font, 'M')) > 0.0,
                        "{case}"
                    );
                }
                let monospace = TextStyle::Monospace.resolve(ui.style());
                let grid = match terminal.and_then(|id| face_by_id(TERMINAL_FACES, id)) {
                    Some(face) => FontId::new(
                        face.terminal_size.unwrap_or(monospace.size),
                        FontFamily::Name(TERMINAL_FAMILY.into()),
                    ),
                    None => monospace,
                };
                assert_eq!(terminal_font_id(ui), grid, "{case}");
                assert!(
                    ui.fonts_mut(|fonts| fonts.glyph_width(&grid, 'M')) > 0.0,
                    "{case}"
                );
                ran = true;
            });
            assert!(ran, "{case}: the probe did not run");
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
        // icon is its own glyph, not a fallback's, and the emoji is egui's.
        let ctx = egui::Context::default();
        ctx.begin_pass(egui::RawInput::default());
        FontService::default().sync(&ctx, None, Some(DEFAULT_TERMINAL_ID));
        ctx.fonts_mut(|fonts| {
            let meslo = FontId::new(14.0, FontFamily::Name(DEFAULT_TERMINAL_ID.into()));
            let obel = FontId::new(14.0, FontFamily::Name("obel-mono".into()));
            assert!(fonts.glyph_width(&obel, 'M') > 0.0);
            assert!(fonts.glyph_width(&meslo, PROMPT_ICON) > 0.0);
            assert_eq!(fonts.glyph_width(&obel, PROMPT_ICON), 0.0);
            assert!(fonts.glyph_width(&FontId::proportional(14.0), '\u{1f419}') > 0.0);
        });
        finish_pass(&ctx);
    }

    fn finish_pass(ctx: &egui::Context) {
        // Dropping an unapplied texture delta panics. This test has no renderer.
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
    }
}
