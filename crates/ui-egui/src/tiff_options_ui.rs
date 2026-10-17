//! TIFF Options: the prompt File › Save As shows before writing a layered TIFF, as Photoshop
//! does when "Ask Before Saving Layered TIFF Files" is on in Preferences › File Handling.
//!
//! The save is parked in [`Prompt`] while the modal is up (egui is immediate-mode) and written
//! once answered: with the layers (Photoshop layer data in the file), or flattened ("Discard
//! Layers and Save a Copy"). "Don't show again" clears the preference. Cancel drops the save.

use crate::{ExportSettings, PhotospliceApp};

/// A save waiting on the TIFF Options answer.
pub struct Prompt {
    /// The document that requested the save, even if tabs change while this is open.
    doc: photosplice_doc::DocId,
    /// Where the document goes.
    pub path: String,
    /// "Discard Layers and Save a Copy".
    pub discard_layers: bool,
    /// Clears `ask_before_saving_layered_tiff` when the save goes ahead.
    pub dont_ask_again: bool,
}

/// `true` when `path` is a TIFF name (`.tif` / `.tiff`).
pub fn is_tiff_path(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name.rsplit_once('.').is_some_and(|(stem, ext)| !stem.is_empty() && (ext.eq_ignore_ascii_case("tif") || ext.eq_ignore_ascii_case("tiff")))
}

/// Whether saving the active document to `path` should go through the prompt: a TIFF that
/// would carry layer data, with the preference on.
pub fn wants_prompt(app: &PhotospliceApp, path: &str) -> bool {
    if !is_tiff_path(path) || !app.session.prefs().file_handling.ask_before_saving_layered_tiff {
        return false;
    }
    // A document without layers has none to keep: it saves straight back, like any flat file.
    app.session.active().is_some_and(|st| !st.doc.layers.is_empty() && photosplice_engine::file_cmds::tiff_would_write_layers(&st.doc))
}

/// Parks the save behind the prompt.
pub fn park(app: &mut PhotospliceApp, path: String) -> Result<(), String> {
    let doc = app.active_doc_id()?;
    app.tiff_options = Some(Prompt { doc, path, discard_layers: false, dont_ask_again: false });
    Ok(())
}

pub fn show(app: &mut PhotospliceApp, ctx: &egui::Context) {
    let Some(p) = &mut app.tiff_options else { return };
    let (mut ok, mut cancel) = (false, false);
    let (mut discard, mut dont_ask) = (p.discard_layers, p.dont_ask_again);
    // PhotoSplice's dialog frame: the same title bar, drag and placement as every other dialog.
    let id = egui::Id::new("tiff-options");
    let modal = crate::dialogs::frame(ctx, id, tl!("TIFF Options"), crate::dialogs::FrameSize::Width(380.0, 420.0), |ui, _| {
        ui.label(tl!("Including layers will increase file size."));
        ui.add_space(8.0);
        crate::widgets::checkbox(ui, &mut discard, tl!("Discard Layers and Save a Copy"));
        crate::widgets::checkbox(ui, &mut dont_ask, tl!("Don't show again"));
        ui.add_space(8.0);
        // Align::Min, as the other dialogs: a centred row keeps last frame's height.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            ok = crate::widgets::primary_button(ui, tl!("OK"), 84.0).clicked();
            cancel = crate::widgets::secondary_button(ui, tl!("Cancel"), 84.0).clicked();
        });
    });
    cancel |= modal.should_close();
    p.discard_layers = discard;
    p.dont_ask_again = dont_ask;
    if ok || ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
        let Some(p) = app.tiff_options.take() else { return };
        if p.dont_ask_again {
            app.session.edit_prefs(|prefs| prefs.file_handling.ask_before_saving_layered_tiff = false);
        }
        let settings = ExportSettings { tiff_layers: !p.discard_layers, ..app.save_settings() };
        // A flattened copy never replaces the document's own file (File › Save in place): it
        // goes beside it as "name copy.tif", as Reference names a copy.
        let own = app.session.documents().iter().find(|d| d.doc.id == p.doc).and_then(|d| d.path.clone());
        let path = if p.discard_layers && own.as_deref() == Some(p.path.as_str()) { copy_path(&p.path) } else { p.path.clone() };
        let step = serde_json::json!({"path": path, "tiffLayers": !p.discard_layers});
        let written = app.with_document(p.doc, |app| app.write_document(path, &settings, p.discard_layers));
        if written.is_ok() {
            // An action being recorded keeps the Save As with this answer (#2032).
            app.session.journal.push(("file.saveAs".into(), step));
        }
        match written {
            Err(e) => {
                app.ui.status = crate::i18n::fmt(tl!("Save failed: {error}"), &[("error", &e)]);
                app.ui.status_error = true;
            }
            Ok(Some(_)) if !p.discard_layers => crate::discard_ui::saved_document(app, ctx, p.doc),
            // A background save releases the close prompt when it finishes (`jobs_ui`).
            Ok(_) => {}
        }
    } else if cancel {
        app.tiff_options = None;
        app.ui.status = tl!("Save cancelled").into();
        app.ui.status_error = false;
    }
}

/// `dir/name.tif` → `dir/name copy.tif`.
fn copy_path(path: &str) -> String {
    let name_start = path.rfind(['/', '\\']).map_or(0, |i| i + 1);
    match path.rfind('.').filter(|&dot| dot > name_start).and_then(|dot| Some((path.get(..dot)?, path.get(dot..)?))) {
        Some((stem, ext)) => format!("{stem} copy{ext}"),
        None => format!("{path} copy"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_are_named_beside_the_file() {
        assert_eq!(copy_path("dir/a.tif"), "dir/a copy.tif");
        assert_eq!(copy_path("C:\\x.y\\b.TIFF"), "C:\\x.y\\b copy.TIFF");
        assert_eq!(copy_path("dir.v2/noext"), "dir.v2/noext copy");
        assert_eq!(copy_path("é.tif"), "é copy.tif");
    }

    #[test]
    fn tiff_names() {
        assert!(is_tiff_path("a.tif"));
        assert!(is_tiff_path("dir/sub/a.TIFF"));
        assert!(is_tiff_path("C:\\x\\y.tiff"));
        assert!(!is_tiff_path("a.tif.psd"));
        assert!(!is_tiff_path(".tif"));
        assert!(!is_tiff_path("tif"));
        assert!(!is_tiff_path("a.png"));
    }
}
