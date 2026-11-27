//! Text laid out to the width of the column it sits in.

use std::sync::Arc;

use eframe::egui::text::TextWrapping;
use eframe::egui::{FontSelection, Galley, Ui, WidgetText};

/// `text` in `ui`'s style, wrapped to the width left in `ui`. Each row breaks
/// at the last character that fits, inside a word too, so every row fills
/// the column and nothing paints past it. Nothing is elided.
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

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;

    /// A long word after a space starts on the first row and breaks where
    /// the column ends, instead of moving whole to the next row.
    #[test]
    fn rows_break_at_the_last_character_that_fits() {
        let mut first_row = 0;
        Harness::new_ui(|ui| {
            ui.set_max_width(80.0);
            let galley = super::wrapped(ui, format!("ab {}", "x".repeat(60)));
            assert!(galley.size().x <= 80.0, "{:?}", galley.size());
            first_row = galley.rows[0].row.glyphs.len();
        });
        assert!(first_row > 3, "the first row ends at the space");
    }
}
