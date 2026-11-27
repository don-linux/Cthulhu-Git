//! Text laid out to the width of the column it sits in.

use std::sync::Arc;

use eframe::egui::text::TextWrapping;
use eframe::egui::{FontSelection, Galley, Ui, WidgetText};

/// `text` in `ui`'s style, wrapped to the width left in `ui`. A word that
/// does not fit is broken inside, so one long token (an address, a path, a
/// sentence in a narrow column) cannot paint past the column. Nothing is
/// elided.
pub fn wrapped(ui: &Ui, text: impl Into<WidgetText>) -> Arc<Galley> {
    let mut job = Arc::unwrap_or_clone(text.into().into_layout_job(
        ui.style(),
        FontSelection::Default,
        ui.text_valign(),
    ));
    job.wrap = TextWrapping {
        break_anywhere: true,
        ..TextWrapping::wrap_at_width(ui.available_width().max(0.0))
    };
    ui.fonts_mut(|fonts| fonts.layout_job(job))
}
