//! Bundled faces for the interface and the terminal.
//!
//! Nothing is read from the operating system. Each face is compiled into the
//! binary. egui installs `set_fonts` on the following pass, so a named family
//! is not measured on the pass that first queues it.
//!
//! The interface replaces egui's proportional face. The terminal uses the
//! named family `"terminal"` and, until that face is ready, the built-in
//! monospace. Hashes and diffs stay on that built-in monospace either way.

use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily, FontId, Id, TextStyle};

/// egui family that holds only the terminal face.
pub const TERMINAL_FAMILY: &str = "terminal";

/// Saved `ui_font` of `None` or an unknown id uses this face.
pub const DEFAULT_UI_ID: &str = "obel-mono";

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
        name: "Obel Mono",
        note: "Default",
        bytes: OBEL_MONO,
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
    /// A missing or unknown interface id is Obel Mono. A missing or unknown
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
            set_terminal_active(ctx, self.applied_terminal.is_some());
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
        ctx.set_fonts(definitions(ui_id, terminal_id));
        ctx.request_repaint();
        self.applied_ui = ui_id.to_owned();
        self.applied_terminal = terminal_id.map(str::to_owned);
        let first = !self.queued;
        self.queued = true;
        store_terminal_size(ctx, terminal_id.and_then(point_size));

        if first || !faces_ready(ctx) {
            self.activate_faces_next_pass = true;
        }
        match terminal_id {
            Some(_) if !had_terminal => {
                // The named family is not in the map egui has loaded yet.
                set_terminal_active(ctx, false);
                self.activate_terminal_next_pass = true;
            }
            Some(_) => set_terminal_active(ctx, true),
            None => {
                set_terminal_active(ctx, false);
                self.activate_terminal_next_pass = false;
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
    use super::*;

    #[test]
    fn obel_mono_is_shared_and_other_faces_stay_in_one_catalog() {
        assert!(face_by_id(UI_FACES, DEFAULT_UI_ID).is_some());
        let interface = face_by_id(UI_FACES, "obel-mono").expect("interface");
        let terminal = face_by_id(TERMINAL_FACES, "obel-mono").expect("terminal");
        assert_eq!(interface.bytes.as_ptr(), terminal.bytes.as_ptr());
        for face in UI_FACES {
            assert!(face.bytes.len() > 4, "{}", face.id);
            if face.id == "obel-mono" {
                continue;
            }
            assert!(face_by_id(TERMINAL_FACES, face.id).is_none(), "{}", face.id);
        }
        for face in TERMINAL_FACES {
            if face.id != "obel-mono" {
                assert!(face_by_id(UI_FACES, face.id).is_none(), "{}", face.id);
            }
            let tag = &face.bytes[..4];
            assert!(
                tag == b"\0\x01\0\0" || tag == b"OTTO" || tag == b"true",
                "{}",
                face.id
            );
        }
    }

    #[test]
    fn resolve_falls_back_when_the_saved_id_is_not_bundled() {
        assert_eq!(resolve_ui(None), DEFAULT_UI_ID);
        assert_eq!(resolve_ui(Some("  ")), DEFAULT_UI_ID);
        assert_eq!(resolve_ui(Some("Comic Sans")), DEFAULT_UI_ID);
        assert_eq!(resolve_ui(Some(" INTER ")), "inter");
        assert_eq!(resolve_terminal(None), Some(DEFAULT_TERMINAL_ID));
        assert_eq!(
            resolve_terminal(Some("Fira Code")),
            Some(DEFAULT_TERMINAL_ID)
        );
        assert_eq!(resolve_terminal(Some("builtin")), None);
        assert_eq!(resolve_terminal(Some(" BUILTIN ")), None);
        assert_eq!(resolve_terminal(Some("meslo-lgs-nf")), Some("meslo-lgs-nf"));
        assert_eq!(
            resolve_terminal(Some("Departure-Mono")),
            Some("departure-mono")
        );
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
        assert_eq!(terminal_size(&ctx), Some(DEPARTURE_SIZE));
        let named = FontId::new(DEPARTURE_SIZE, FontFamily::Name(TERMINAL_FAMILY.into()));
        ctx.fonts_mut(|fonts| {
            assert!(fonts.glyph_width(&named, 'M') > 0.0);
        });
        finish_pass(&ctx);

        ctx.begin_pass(egui::RawInput::default());
        service.sync(&ctx, None, None);
        assert!(faces_ready(&ctx));
        assert!(!terminal_active(&ctx));
        assert!(terminal_size(&ctx).is_none());
        finish_pass(&ctx);
    }

    /// nf-oct-terminal. Meslo LGS NF has it; Obel Mono does not.
    const PROMPT_ICON: char = '\u{f120}';

    #[test]
    fn hostile_ids_stay_inside_the_bundled_catalog_and_off_disk() {
        let missing =
            std::env::temp_dir().join(format!("cthulhu-font-probe-{}", std::process::id()));
        let _ = std::fs::remove_file(&missing);
        let path = missing.to_string_lossy().into_owned();
        let huge = "x".repeat(100_000);
        let samples = [
            "",
            " ",
            " \n\t ",
            "UBUNTU-SANS",
            " Obel-Mono ",
            "inter\n",
            "obel\nmono",
            "JetBrains Mono",
            "Fira Code",
            "\u{202e}obel-mono",
            path.as_str(),
            huge.as_str(),
        ];
        for name in samples {
            let ui = resolve_ui(Some(name));
            assert!(
                UI_FACES.iter().any(|face| face.id == ui),
                "{name:?} resolved to {ui}"
            );
            if let Some(terminal) = resolve_terminal(Some(name)) {
                assert!(
                    TERMINAL_FACES.iter().any(|face| face.id == terminal),
                    "{name:?} resolved to {terminal}"
                );
            }
        }
        assert_eq!(resolve_ui(Some("JetBrains Mono")), DEFAULT_UI_ID);
        assert_eq!(resolve_ui(Some(&path)), DEFAULT_UI_ID);
        assert_eq!(resolve_ui(Some("\u{202e}obel-mono")), DEFAULT_UI_ID);
        assert_eq!(
            resolve_terminal(Some("Fira Code")),
            Some(DEFAULT_TERMINAL_ID)
        );
        assert_eq!(resolve_terminal(Some(&path)), Some(DEFAULT_TERMINAL_ID));
        assert_eq!(resolve_terminal(Some("builtin")), None);
        assert_eq!(resolve_terminal(Some(" Obel-Mono ")), Some("obel-mono"));
        assert!(
            !missing.exists(),
            "font lookup created {}",
            missing.display()
        );

        let ctx = egui::Context::default();
        let mut service = FontService::default();
        settle(&ctx, &mut service, Some(&huge), Some(path.as_str()));
        assert_loaded(&ctx, DEFAULT_UI_ID, Some(DEFAULT_TERMINAL_ID));
        assert!(!missing.exists(), "sync created {}", missing.display());
        finish_pass(&ctx);
    }

    #[test]
    fn every_face_pair_applies_without_clobbering_the_other() {
        let ctx = egui::Context::default();
        let mut service = FontService::default();
        let terminals = [
            None,
            Some("builtin"),
            Some("obel-mono"),
            Some("meslo-lgs-nf"),
            Some("departure-mono"),
        ];
        for ui in ["ubuntu-sans", "obel-mono", "aspekta", "inter"] {
            for terminal in terminals {
                // The choice queued on the first pass is what the next pass loads.
                settle(&ctx, &mut service, Some(ui), terminal);
                assert_loaded(&ctx, ui, resolve_terminal(terminal));
                if ui == "ubuntu-sans" && terminal == Some("meslo-lgs-nf") {
                    ctx.fonts_mut(|fonts| {
                        let meslo = FontId::new(14.0, FontFamily::Name("meslo-lgs-nf".into()));
                        let obel = FontId::new(14.0, FontFamily::Name("obel-mono".into()));
                        assert!(fonts.glyph_width(&meslo, 'M') > 0.0);
                        assert!(fonts.glyph_width(&obel, 'M') > 0.0);
                        // A missing glyph is zero wide. Meslo's notdef is not used here:
                        // the same code point on Obel Mono measures zero.
                        assert!(fonts.glyph_width(&meslo, PROMPT_ICON) > 0.0);
                        assert_eq!(fonts.glyph_width(&obel, PROMPT_ICON), 0.0);
                    });
                }
                finish_pass(&ctx);
            }
        }
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
}
