//! Save Adobe PDF: the options File › Save shows before writing a PDF, as Photoshop does.
//!
//! "Preserve Photoshop Editing Capabilities" (on by default, as in Photoshop) adds Photoshop's
//! editing data, with which Photoshop and PhotoSuite reopen the PDF with its layers; off, the PDF
//! is only its page. The choice is remembered for the next PDF save. The save is parked in
//! [`Prompt`] while the modal is up (egui is immediate-mode) and written once answered; Cancel
//! drops it.

use crate::{ExportSettings, PhotosuiteApp};

/// A save waiting on the Save Adobe PDF answer.
pub struct Prompt {
    /// Where the document goes.
    pub path: String,
    /// "Preserve Photoshop Editing Capabilities".
    pub preserve_editing: bool,
}

/// `true` when `path` is a PDF name.
pub fn is_pdf_path(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name.rsplit_once('.').is_some_and(|(stem, ext)| !stem.is_empty() && ext.eq_ignore_ascii_case("pdf"))
}

/// Parks the save behind the options, starting from the last PDF save's choice.
pub fn park(app: &mut PhotosuiteApp, path: String) {
    app.pdf_options = Some(Prompt { path, preserve_editing: app.pdf_preserve_editing });
}

pub fn show(app: &mut PhotosuiteApp, ctx: &egui::Context) {
    let Some(p) = &mut app.pdf_options else { return };
    let (mut ok, mut cancel) = (false, false);
    let mut preserve = p.preserve_editing;
    let id = egui::Id::new("pdf-options");
    let modal = crate::dialogs::frame(ctx, id, tl!("Save Adobe PDF"), crate::dialogs::FrameSize::Width(420.0, 460.0), |ui, _| {
        crate::widgets::checkbox(ui, &mut preserve, tl!("Preserve Photoshop Editing Capabilities"));
        ui.add_space(4.0);
        ui.label(if preserve {
            tl!("Photoshop and PhotoSuite reopen the PDF with its layers. Other applications show its page.")
        } else {
            tl!("The PDF keeps only its page: it reopens as an imported PDF, without its layers.")
        });
        ui.add_space(8.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            ok = crate::widgets::primary_button(ui, tl!("Save PDF"), 96.0).clicked();
            cancel = crate::widgets::secondary_button(ui, tl!("Cancel"), 84.0).clicked();
        });
    });
    cancel |= modal.should_close();
    p.preserve_editing = preserve;
    if ok || ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
        let Some(p) = app.pdf_options.take() else { return };
        app.pdf_preserve_editing = p.preserve_editing;
        let settings = ExportSettings { pdf_editing: p.preserve_editing, ..Default::default() };
        if let Err(e) = app.write_document(p.path.clone(), &settings) {
            app.ui.status = crate::i18n::fmt(tl!("Save failed: {error}"), &[("error", &e)]);
            app.ui.status_error = true;
        }
    } else if cancel {
        app.pdf_options = None;
        app.ui.status = tl!("Save cancelled").into();
        app.ui.status_error = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_names() {
        assert!(is_pdf_path("a.pdf"));
        assert!(is_pdf_path("dir/sub/a.PDF"));
        assert!(!is_pdf_path("a.pdf.psd"));
        assert!(!is_pdf_path(".pdf"));
        assert!(!is_pdf_path("a.png"));
    }
}
