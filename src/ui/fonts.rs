//! Installed font families for the terminal.
//!
//! The scan runs once, on a background thread, the first time a family is
//! needed. The chosen face is registered under egui's named family
//! `"terminal"`, so the rest of the interface keeps its monospace. egui loads
//! `set_fonts` on the following pass, so the named family stays unused until
//! then. Until that face is ready — or when the name is blank or unknown —
//! the terminal uses the built-in monospace.
//!
//! `fontdb` reads fontconfig's file list through its Rust parser. It does not
//! link `libfontconfig`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily, FontId, Id, TextStyle};

/// egui family that holds only the terminal face.
pub const TERMINAL_FAMILY: &str = "terminal";

/// One installed face. Several faces can share a family (regular, bold, italic).
#[derive(Clone)]
struct FaceChoice {
    family: String,
    path: PathBuf,
    index: u32,
    weight: u16,
    italic: bool,
    monospace: bool,
}

/// Unique family names, in case-insensitive order, plus the faces behind them.
pub struct Catalog {
    faces: Vec<FaceChoice>,
    names: Vec<String>,
}

impl Catalog {
    pub fn names(&self) -> &[String] {
        &self.names
    }

    fn best_face(&self, family: &str) -> Option<&FaceChoice> {
        best_face(&self.faces, family)
    }
}

/// One page of the font browser.
#[derive(Debug, PartialEq, Eq)]
pub struct NamePage {
    pub names: Vec<String>,
    /// Zero-based page actually shown. Clamped when `page` runs past the end.
    pub page: usize,
    /// `0` when nothing matches.
    pub page_count: usize,
}

/// Where the system scan is.
#[derive(Clone, Copy)]
pub enum CatalogPhase<'a> {
    Idle,
    Loading,
    Ready(&'a Catalog),
    Failed(&'a str),
}

/// Whether the saved family is the face the terminal is using.
pub enum FacePhase {
    /// Built-in monospace. `pending` means a saved name is still being looked up.
    Builtin {
        pending: bool,
    },
    /// The catalog has no face with this family.
    Missing,
    Ready {
        monospace: bool,
    },
    Unreadable(String),
    ScanFailed(String),
}

pub struct FontView<'a> {
    pub catalog: CatalogPhase<'a>,
    pub face: FacePhase,
}

enum Slot {
    Idle,
    Loading(Receiver<Catalog>),
    Ready(Catalog),
    Failed(String),
}

enum Applied {
    Builtin,
    Loaded { name: String, monospace: bool },
    Missing { name: String },
    Unreadable { name: String, message: String },
}

/// Scan plus the face currently handed to egui.
pub struct FontService {
    slot: Slot,
    applied: Applied,
    /// A custom face is in egui's font map. Clearing it restores the defaults.
    installed: bool,
    /// `set_fonts` was queued this pass. The named family is bound on the next one.
    activate_next_pass: bool,
}

impl Default for FontService {
    fn default() -> Self {
        Self {
            slot: Slot::Idle,
            applied: Applied::Builtin,
            installed: false,
            activate_next_pass: false,
        }
    }
}

impl FontService {
    /// Starts a scan when none is running. `retry` starts again after a failure.
    pub fn ensure(&mut self, ctx: &egui::Context, retry: bool) {
        let start = match &self.slot {
            Slot::Idle => true,
            Slot::Failed(_) => retry,
            Slot::Loading(_) | Slot::Ready(_) => false,
        };
        if start {
            self.slot = Slot::Loading(spawn_scan(ctx));
        }
    }

    /// Picks up a finished scan and registers `wanted` when the catalog can answer.
    pub fn sync(&mut self, ctx: &egui::Context, wanted: Option<&str>) {
        self.poll();
        // Fonts queued last pass are loaded before this call.
        if self.activate_next_pass {
            set_active(ctx, true);
            self.activate_next_pass = false;
        }
        let Some(name) = wanted else {
            self.use_builtin(ctx);
            return;
        };
        if self.applied_for(name) {
            return;
        }
        self.ensure(ctx, false);
        let chosen = match &self.slot {
            Slot::Ready(catalog) => catalog.best_face(name).map(|face| ChosenFace {
                path: face.path.clone(),
                index: face.index,
                monospace: face.monospace,
            }),
            Slot::Failed(_) => {
                self.use_builtin(ctx);
                return;
            }
            Slot::Idle | Slot::Loading(_) => return,
        };
        match chosen {
            Some(face) => self.install_face(ctx, name, face),
            None => {
                self.clear_installed(ctx);
                self.applied = Applied::Missing {
                    name: name.to_owned(),
                };
            }
        }
    }

    pub fn view(&self, wanted: Option<&str>) -> FontView<'_> {
        FontView {
            face: self.face_phase(wanted),
            catalog: self.catalog_phase(),
        }
    }

    fn poll(&mut self) {
        let Slot::Loading(receiver) = &self.slot else {
            return;
        };
        let finished = match receiver.try_recv() {
            Ok(catalog) => Ok(catalog),
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("Font scan stopped unexpectedly.".to_owned()),
        };
        self.slot = match finished {
            Ok(catalog) => Slot::Ready(catalog),
            Err(message) => Slot::Failed(message),
        };
    }

    fn catalog_phase(&self) -> CatalogPhase<'_> {
        match &self.slot {
            Slot::Idle => CatalogPhase::Idle,
            Slot::Loading(_) => CatalogPhase::Loading,
            Slot::Ready(catalog) => CatalogPhase::Ready(catalog),
            Slot::Failed(message) => CatalogPhase::Failed(message),
        }
    }

    fn face_phase(&self, wanted: Option<&str>) -> FacePhase {
        let Some(name) = wanted else {
            return FacePhase::Builtin { pending: false };
        };
        if let Slot::Failed(message) = &self.slot
            && !self.applied_for(name)
        {
            return FacePhase::ScanFailed(message.clone());
        }
        match &self.applied {
            Applied::Loaded {
                name: applied,
                monospace,
            } if applied == name => FacePhase::Ready {
                monospace: *monospace,
            },
            Applied::Missing { name: applied } if applied == name => FacePhase::Missing,
            Applied::Unreadable {
                name: applied,
                message,
            } if applied == name => FacePhase::Unreadable(message.clone()),
            _ => FacePhase::Builtin {
                pending: matches!(self.slot, Slot::Idle | Slot::Loading(_)),
            },
        }
    }

    fn applied_for(&self, name: &str) -> bool {
        match &self.applied {
            Applied::Loaded { name: applied, .. }
            | Applied::Missing { name: applied }
            | Applied::Unreadable { name: applied, .. } => applied == name,
            Applied::Builtin => false,
        }
    }

    fn use_builtin(&mut self, ctx: &egui::Context) {
        if matches!(self.applied, Applied::Builtin) && !self.installed {
            return;
        }
        self.clear_installed(ctx);
        self.applied = Applied::Builtin;
    }

    fn install_face(&mut self, ctx: &egui::Context, name: &str, face: ChosenFace) {
        let monospace = face.monospace;
        match std::fs::read(&face.path) {
            Ok(bytes) => {
                register(ctx, bytes, face.index);
                self.installed = true;
                self.applied = Applied::Loaded {
                    name: name.to_owned(),
                    monospace,
                };
                // A replacement is already bound this pass. The first install
                // is not, so the named family stays off until the next one.
                if !family_active(ctx) {
                    self.activate_next_pass = true;
                }
            }
            Err(error) => {
                self.clear_installed(ctx);
                self.applied = Applied::Unreadable {
                    name: name.to_owned(),
                    message: format!("Could not read {}: {error}", face.path.display()),
                };
            }
        }
    }

    fn clear_installed(&mut self, ctx: &egui::Context) {
        self.activate_next_pass = false;
        if self.installed {
            ctx.set_fonts(FontDefinitions::default());
            self.installed = false;
            ctx.request_repaint();
        }
        set_active(ctx, false);
    }
}

struct ChosenFace {
    path: PathBuf,
    index: u32,
    monospace: bool,
}

/// The font the terminal grid measures. The named family is used only on a
/// pass where egui has already loaded it.
pub fn terminal_font_id(ui: &egui::Ui) -> FontId {
    let mut font = TextStyle::Monospace.resolve(ui.style());
    if family_active(ui.ctx()) {
        font.family = FontFamily::Name(TERMINAL_FAMILY.into());
    }
    font
}

pub fn family_active(ctx: &egui::Context) -> bool {
    ctx.data(|data| data.get_temp(active_id()).unwrap_or(false))
}

fn set_active(ctx: &egui::Context, active: bool) {
    ctx.data_mut(|data| data.insert_temp(active_id(), active));
}

fn active_id() -> Id {
    Id::new("cthulhu-git-terminal-font")
}

/// Regular weight, upright, wins. Otherwise the closest upright face, then any face.
fn best_face<'a>(faces: &'a [FaceChoice], family: &str) -> Option<&'a FaceChoice> {
    faces
        .iter()
        .filter(|face| same_family(&face.family, family))
        .min_by_key(|face| {
            let regular = face.weight == 400 && !face.italic;
            (!regular, face.italic, face.weight.abs_diff(400))
        })
}

fn same_family(left: &str, right: &str) -> bool {
    left.to_lowercase() == right.to_lowercase()
}

/// `page_size` of `0` is treated as one name per page.
pub fn page_names(names: &[String], query: &str, page: usize, page_size: usize) -> NamePage {
    let page_size = page_size.max(1);
    let needle = query.trim().to_lowercase();
    let matched: Vec<&str> = names
        .iter()
        .filter(|name| needle.is_empty() || name.to_lowercase().contains(&needle))
        .map(String::as_str)
        .collect();
    let page_count = matched.len().div_ceil(page_size);
    if page_count == 0 {
        return NamePage {
            names: Vec::new(),
            page: 0,
            page_count: 0,
        };
    }
    let page = page.min(page_count - 1);
    let start = page * page_size;
    NamePage {
        names: matched
            .iter()
            .skip(start)
            .take(page_size)
            .copied()
            .map(str::to_owned)
            .collect(),
        page,
        page_count,
    }
}

fn spawn_scan(ctx: &egui::Context) -> Receiver<Catalog> {
    let (sender, receiver) = mpsc::channel();
    let ctx = ctx.clone();
    thread::spawn(move || {
        let _ = sender.send(load_catalog());
        ctx.request_repaint();
    });
    receiver
}

fn load_catalog() -> Catalog {
    let mut database = fontdb::Database::new();
    database.load_system_fonts();
    let faces = database.faces().filter_map(face_choice).collect();
    catalog_from_faces(faces)
}

fn face_choice(face: &fontdb::FaceInfo) -> Option<FaceChoice> {
    let family = face.families.first()?.0.clone();
    if family.trim().is_empty() {
        return None;
    }
    Some(FaceChoice {
        family,
        path: face_path(&face.source)?,
        index: face.index,
        weight: face.weight.0,
        italic: matches!(face.style, fontdb::Style::Italic | fontdb::Style::Oblique),
        monospace: face.monospaced,
    })
}

fn face_path(source: &fontdb::Source) -> Option<PathBuf> {
    match source {
        fontdb::Source::File(path) | fontdb::Source::SharedFile(path, _) => Some(path.clone()),
        fontdb::Source::Binary(_) => None,
    }
}

fn catalog_from_faces(faces: Vec<FaceChoice>) -> Catalog {
    let faces: Vec<_> = faces
        .into_iter()
        .filter(|face| !face.family.trim().is_empty())
        .collect();
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    for face in &faces {
        if seen.insert(face.family.to_lowercase()) {
            names.push(face.family.clone());
        }
    }
    names.sort_by_key(|name| name.to_lowercase());
    Catalog { faces, names }
}

/// Puts `bytes` in the terminal family. Monospace stays the fallback for glyphs
/// this file does not have, and is not itself replaced. The named family is
/// bound on the next pass, not this one.
fn register(ctx: &egui::Context, bytes: Vec<u8>, index: u32) {
    let mut fonts = FontDefinitions::default();
    let mut data = FontData::from_owned(bytes);
    data.index = index;
    fonts
        .font_data
        .insert(TERMINAL_FAMILY.to_owned(), std::sync::Arc::new(data));
    let mut chain = vec![TERMINAL_FAMILY.to_owned()];
    if let Some(monospace) = fonts.families.get(&FontFamily::Monospace) {
        chain.extend(monospace.iter().cloned());
    }
    fonts
        .families
        .insert(FontFamily::Name(TERMINAL_FAMILY.into()), chain);
    ctx.set_fonts(fonts);
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(family: &str, weight: u16, italic: bool) -> FaceChoice {
        FaceChoice {
            family: family.to_owned(),
            path: PathBuf::from(family),
            index: 0,
            weight,
            italic,
            monospace: true,
        }
    }

    #[test]
    fn best_face_prefers_regular_then_upright() {
        let faces = vec![
            face("Fira Code", 700, false),
            face("Fira Code", 400, true),
            face("Fira Code", 400, false),
        ];
        let chosen = best_face(&faces, "fira code").expect("match");
        assert_eq!(chosen.weight, 400);
        assert!(!chosen.italic);

        let faces = vec![face("Fira Code", 400, true), face("Fira Code", 700, false)];
        let chosen = best_face(&faces, "Fira Code").expect("match");
        assert_eq!(chosen.weight, 700);
        assert!(!chosen.italic);

        let faces = vec![face("Fira Code", 700, true), face("Fira Code", 400, true)];
        let chosen = best_face(&faces, "FIRA CODE").expect("match");
        assert_eq!(chosen.weight, 400);
        assert!(chosen.italic);
    }

    #[test]
    fn best_face_misses_an_unknown_family() {
        let faces = vec![face("Fira Code", 400, false)];
        assert!(best_face(&faces, "Comic Sans MS").is_none());
        assert!(best_face(&faces, "").is_none());
    }

    #[test]
    fn page_names_filters_and_clamps() {
        let names = vec![
            "Apple".to_owned(),
            "Fira Code".to_owned(),
            "IBM Plex Mono".to_owned(),
            "JetBrains Mono".to_owned(),
            "Zebra".to_owned(),
        ];
        assert_eq!(
            page_names(&names, "", 0, 2),
            NamePage {
                names: vec!["Apple".to_owned(), "Fira Code".to_owned()],
                page: 0,
                page_count: 3,
            }
        );
        assert_eq!(
            page_names(&names, "", 1, 2).names,
            vec!["IBM Plex Mono".to_owned(), "JetBrains Mono".to_owned()]
        );
        let last = page_names(&names, "", 99, 2);
        assert_eq!(last.page, 2);
        assert_eq!(last.names, vec!["Zebra".to_owned()]);

        assert_eq!(
            page_names(&names, "  code ", 4, 12).names,
            vec!["Fira Code".to_owned()]
        );
        assert_eq!(page_names(&names, "nope", 0, 12).page_count, 0);
        assert_eq!(page_names(&[], " ", 3, 12).page_count, 0);

        // A page size of 0 is one name per page, so page 2 is the third family.
        let one = page_names(&names, "", 2, 0);
        assert_eq!(one.page, 2);
        assert_eq!(one.page_count, names.len());
        assert_eq!(one.names, vec!["IBM Plex Mono".to_owned()]);
    }

    #[test]
    fn catalog_lists_each_family_once_ignoring_case() {
        let catalog = catalog_from_faces(vec![
            face("Zebra", 400, false),
            face("apple", 400, false),
            face("ZEBRA", 700, false),
            face("  ", 400, false),
            face("", 400, false),
        ]);
        assert_eq!(catalog.names, vec!["apple".to_owned(), "Zebra".to_owned()]);
        assert_eq!(catalog.faces.len(), 3);
    }

    /// egui binds `set_fonts` at the start of the next pass. Measuring the
    /// named family on the install pass is the panic this guards.
    #[test]
    fn terminal_family_is_measured_on_the_pass_after_it_is_queued() {
        let bytes = FontDefinitions::default()
            .font_data
            .get("Hack")
            .expect("egui monospace")
            .font
            .clone()
            .into_owned();
        let scratch = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        std::fs::create_dir_all(&scratch).expect("target dir");
        let dir = tempfile::tempdir_in(&scratch).expect("temp dir");
        let path = dir.path().join("Hack.ttf");
        std::fs::write(&path, bytes).expect("write font");

        let ctx = egui::Context::default();
        let mut service = FontService {
            slot: Slot::Ready(catalog_from_faces(vec![stored_face("Test Mono", &path)])),
            ..FontService::default()
        };

        ctx.begin_pass(egui::RawInput::default());
        service.sync(&ctx, Some("Test Mono"));
        assert!(!family_active(&ctx));
        assert!(service.activate_next_pass);
        finish_pass(&ctx);

        ctx.begin_pass(egui::RawInput::default());
        service.sync(&ctx, Some("Test Mono"));
        assert!(family_active(&ctx));
        assert!(!service.activate_next_pass);
        let named = FontId::new(14.0, FontFamily::Name(TERMINAL_FAMILY.into()));
        ctx.fonts_mut(|fonts| {
            assert!(fonts.glyph_width(&named, 'M') > 0.0);
        });
        finish_pass(&ctx);

        service.slot = Slot::Ready(catalog_from_faces(vec![stored_face("Other Mono", &path)]));
        ctx.begin_pass(egui::RawInput::default());
        service.sync(&ctx, Some("Other Mono"));
        assert!(family_active(&ctx));
        assert!(!service.activate_next_pass);
        ctx.fonts_mut(|fonts| {
            assert!(fonts.glyph_width(&named, 'M') > 0.0);
        });
        finish_pass(&ctx);

        ctx.begin_pass(egui::RawInput::default());
        service.sync(&ctx, None);
        assert!(!family_active(&ctx));
        assert!(!service.activate_next_pass);
        finish_pass(&ctx);
    }

    fn finish_pass(ctx: &egui::Context) {
        // Dropping an unapplied texture delta panics. This test has no renderer.
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
    }

    fn stored_face(family: &str, path: &std::path::Path) -> FaceChoice {
        FaceChoice {
            family: family.to_owned(),
            path: path.to_owned(),
            index: 0,
            weight: 400,
            italic: false,
            monospace: true,
        }
    }
}
