//! Import PDF: the dialog File › Open shows for a PDF, as Photoshop does. The pages as
//! thumbnails (click to pick one, Cmd/Ctrl-click to add or remove one, Shift-click for a run;
//! each picked page opens as its own document) and the page options: Crop To, resolution, mode,
//! bit depth, and whether the page opens as pixels (Photoshop's way) or as editable layers.
//!
//! OK runs `file.importPdf` with those options and remembers them (preferences `dialogs`), so
//! the next PDF starts from them. A Photoshop PDF never gets here: it opens as its document.

use photosplice_io::pdf::{CropTo, Pages};
use serde_json::{Value, json};

use crate::PhotospliceApp;

const COMMAND: &str = "file.importPdf";
/// Where the dialog's choices are remembered (a new key: earlier choices defaulted to pixels).
const REMEMBERED: &str = "file.importPdf#2";
/// Longest thumbnail side, in points.
const THUMB: f32 = 112.0;

/// The Import PDF dialog's state while it is up.
pub struct ImportPdf {
    /// The PDF.
    pub path: String,
    pages: Option<Pages>,
    /// Number of pages.
    pub count: usize,
    /// Picked pages, 0-based, ascending.
    pub selected: Vec<usize>,
    anchor: usize,
    pub crop_to: CropTo,
    /// Pixels per inch.
    pub resolution: f32,
    /// `rgb`, `grayscale`, `cmyk` or `lab`.
    pub mode: String,
    /// 8 or 16.
    pub depth: u64,
    /// `pixels` or `layers`.
    pub open_as: String,
    thumbs: Vec<Option<egui::TextureHandle>>,
    /// The password of a protected file, once it opened it (sent with the import, never stored).
    pub password: String,
    /// A protected file's bytes until the right password is typed, and why the last try failed.
    locked: Option<(Vec<u8>, Option<String>)>,
}

impl ImportPdf {
    /// `file.importPdf`'s params for the current choices.
    pub fn params(&self) -> Value {
        let mut p = json!({
            "path": self.path,
            "pages": self.selected.iter().map(|p| p + 1).collect::<Vec<_>>(),
            "resolution": self.resolution,
            "cropTo": self.crop_to.id(),
            "mode": self.mode,
            "depth": self.depth,
            "as": self.open_as,
        });
        if !self.password.is_empty() {
            p["password"] = json!(self.password);
        }
        p
    }

    /// Tries `password` on the protected file: its pages show when it opens it.
    pub fn unlock(&mut self, password: &str) {
        let Some((bytes, why)) = &mut self.locked else { return };
        match Pages::open_with(bytes, password) {
            Ok(pages) => {
                self.count = pages.len();
                self.selected = (0..self.count).collect();
                self.pages = Some(pages);
                self.thumbs = (0..self.count.min(MAX_THUMBS)).map(|_| None).collect();
                self.password = password.to_string();
                self.locked = None;
            }
            Err(e) => *why = Some(e.to_string()),
        }
    }

    /// Whether the file waits for its password.
    pub fn is_locked(&self) -> bool {
        self.locked.is_some()
    }

    /// The options to remember for the next PDF (not the file or its pages).
    fn remembered(&self) -> Value {
        json!({"resolution": self.resolution, "cropTo": self.crop_to.id(), "mode": self.mode, "depth": self.depth, "as": self.open_as})
    }

    /// Clicking page `i`, with Cmd/Ctrl (`toggle`) or Shift (`extend`).
    pub fn click(&mut self, i: usize, toggle: bool, extend: bool) {
        if i >= self.count {
            return;
        }
        if extend {
            let (a, b) = (self.anchor.min(i), self.anchor.max(i));
            for p in a..=b {
                if !self.selected.contains(&p) {
                    self.selected.push(p);
                }
            }
        } else if toggle {
            match self.selected.iter().position(|&p| p == i) {
                // At least one page stays picked.
                Some(at) if self.selected.len() > 1 => {
                    self.selected.remove(at);
                }
                Some(_) => {}
                None => self.selected.push(i),
            }
            self.anchor = i;
        } else {
            self.selected = vec![i];
            self.anchor = i;
        }
        self.selected.sort_unstable();
    }
}

/// Puts up Import PDF for the PDF `bytes` read from `path`, from the last choices.
pub fn open(app: &mut PhotospliceApp, path: &str, bytes: &[u8]) {
    let opened = Pages::open(bytes);
    let locked = matches!(opened, Err(photosplice_io::IoError::PdfPassword { .. })).then(|| (bytes.to_vec(), None));
    let pages = opened.ok();
    let count = pages.as_ref().map_or(0, Pages::len);
    let mut d = ImportPdf {
        path: path.to_string(),
        pages,
        count,
        // Every page, as editable layers, a board per page, a point per pixel.
        selected: (0..count).collect(),
        anchor: 0,
        crop_to: CropTo::CropBox,
        resolution: photosplice_io::pdf::OPEN_DPI,
        mode: "rgb".into(),
        depth: 8,
        open_as: "layers".into(),
        thumbs: Vec::new(),
        password: String::new(),
        locked,
    };
    if let Some(Value::Object(saved)) = app.session.prefs().dialogs.get(REMEMBERED) {
        if let Some(c) = saved.get("cropTo").and_then(Value::as_str).and_then(CropTo::from_id) {
            d.crop_to = c;
        }
        if let Some(r) = saved.get("resolution").and_then(Value::as_f64).filter(|r| r.is_finite()) {
            d.resolution = (r as f32).clamp(1.0, 2400.0);
        }
        if let Some(m) = saved.get("mode").and_then(Value::as_str).filter(|m| matches!(*m, "rgb" | "grayscale" | "cmyk" | "lab")) {
            d.mode = m.into();
        }
        if let Some(b) = saved.get("depth").and_then(Value::as_u64).filter(|b| matches!(b, 8 | 16)) {
            d.depth = b;
        }
        if let Some(a) = saved.get("as").and_then(Value::as_str).filter(|a| matches!(*a, "pixels" | "layers")) {
            d.open_as = a.into();
        }
    }
    d.thumbs = (0..count.min(MAX_THUMBS)).map(|_| None).collect();
    app.import_pdf = Some(d);
}

/// Pages past this many are listed without thumbnails.
const MAX_THUMBS: usize = 2000;

/// Runs the import with the dialog's choices; the dialog closes.
pub fn confirm(app: &mut PhotospliceApp) -> Result<Value, String> {
    let d = app.import_pdf.take().ok_or("no Import PDF")?;
    let remembered = d.remembered();
    app.session.prefs.edit(|p| p.dialogs.insert(REMEMBERED.into(), remembered));
    app.run(COMMAND, d.params())
}

pub fn show(app: &mut PhotospliceApp, ctx: &egui::Context) {
    let Some(d) = &mut app.import_pdf else { return };
    let (mut ok, mut cancel) = (false, false);
    let tk = crate::theme::Tokens::get(ctx);
    let title = tl!("Import PDF");
    let modal = crate::dialogs::frame(ctx, egui::Id::new("import-pdf"), title, crate::dialogs::FrameSize::Fixed(egui::vec2(760.0, 540.0)), |ui, body| {
        let body = body.unwrap_or(egui::vec2(720.0, 460.0));
        let side = 260.0;
        ui.horizontal_top(|ui| {
            // The pages.
            ui.vertical(|ui| {
                ui.set_width(body.x - side - 16.0);
                crate::widgets::section_label(ui, tl!("Pages"));
                if let Some((_, why)) = &d.locked {
                    let why = why.clone();
                    ui.add_space(8.0);
                    ui.label(tl!("This PDF is password-protected. Enter its password to see its pages."));
                    ui.add_space(6.0);
                    let id = ui.id().with("import-pdf-password");
                    let mut typed = ui.data(|m| m.get_temp::<String>(id)).unwrap_or_default();
                    let mut unlock = false;
                    ui.horizontal(|ui| {
                        let r = ui.add(egui::TextEdit::singleline(&mut typed).password(true).desired_width(220.0).hint_text(tl!("Password")));
                        if !r.has_focus() && why.is_none() && typed.is_empty() {
                            r.request_focus();
                        }
                        unlock = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        unlock |= crate::widgets::secondary_button(ui, tl!("Unlock"), 84.0).clicked();
                    });
                    if let Some(why) = why {
                        ui.colored_label(tk.danger, why);
                    }
                    ui.data_mut(|m| m.insert_temp(id, typed.clone()));
                    if unlock {
                        d.unlock(&typed);
                        if !d.is_locked() {
                            ui.data_mut(|m| m.remove::<String>(id));
                        }
                    }
                    return;
                }
                let grid_h = body.y - 70.0;
                egui::ScrollArea::vertical().id_salt("import-pdf-pages").max_height(grid_h).auto_shrink([false, false]).show(ui, |ui| {
                    let cell = egui::vec2(THUMB + 16.0, THUMB + 30.0);
                    let per_row = ((ui.available_width() / cell.x).floor() as usize).max(1);
                    let mut made = false;
                    for row in 0..d.count.div_ceil(per_row) {
                        ui.horizontal(|ui| {
                            for i in row * per_row..((row + 1) * per_row).min(d.count) {
                                let (rect, resp) = ui.allocate_exact_size(cell, egui::Sense::click());
                                let picked = d.selected.contains(&i);
                                if picked {
                                    ui.painter().rect_filled(rect.shrink(2.0), tk.radius_sm, tk.accent_soft);
                                    ui.painter().rect_stroke(rect.shrink(2.0), tk.radius_sm, egui::Stroke::new(1.5, tk.accent), egui::StrokeKind::Inside);
                                }
                                let area = egui::Rect::from_min_size(rect.min + egui::vec2(8.0, 6.0), egui::vec2(THUMB, THUMB));
                                // One thumbnail rendered per frame, as pages scroll into view.
                                if ui.is_rect_visible(area)
                                    && !made
                                    && let (Some(slot), Some(pages)) = (d.thumbs.get_mut(i), d.pages.as_ref())
                                    && slot.is_none()
                                {
                                    made = true;
                                    let px = (THUMB * ui.ctx().pixels_per_point()).round() as u32;
                                    if let Some((w, h, rgba)) = pages.thumbnail(i, px) {
                                        let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                                        *slot = Some(ui.ctx().load_texture(format!("pdf-page-{i}"), img, egui::TextureOptions::LINEAR));
                                    }
                                }
                                if let Some(Some(tex)) = d.thumbs.get(i) {
                                    let size = tex.size_vec2();
                                    let k = (THUMB / size.x.max(size.y)).min(f32::MAX);
                                    let r = egui::Rect::from_center_size(area.center(), size * k);
                                    // Pages are paper: white under what's drawn on them.
                                    ui.painter().rect_filled(r, 0.0, egui::Color32::WHITE);
                                    ui.painter().image(tex.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                                    ui.painter().rect_stroke(r, 0.0, egui::Stroke::new(1.0, tk.separator), egui::StrokeKind::Outside);
                                } else {
                                    ui.painter().rect_filled(area.shrink(14.0), 0.0, tk.field);
                                }
                                ui.painter().text(
                                    egui::pos2(rect.center().x, rect.bottom() - 12.0),
                                    egui::Align2::CENTER_CENTER,
                                    format!("{}", i + 1),
                                    egui::FontId::proportional(11.0),
                                    if picked { tk.text } else { tk.text_dim },
                                );
                                if resp.clicked() {
                                    let m = ui.input(|i| i.modifiers);
                                    d.click(i, m.command, m.shift);
                                }
                                if resp.double_clicked() {
                                    d.click(i, false, false);
                                    ok = true;
                                }
                            }
                        });
                    }
                    if made {
                        ui.ctx().request_repaint();
                    }
                });
                let n = d.selected.len();
                let picked = crate::i18n::fmt(tl!("{n} of {count} pages selected"), &[("n", &n.to_string()), ("count", &d.count.to_string())]);
                ui.label(egui::RichText::new(picked).color(tk.text_dim));
            });
            // The page options.
            ui.vertical(|ui| {
                ui.set_width(side);
                crate::widgets::section_label(ui, tl!("Page Options"));
                egui::Grid::new("import-pdf-options").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                    ui.label(tl!("Crop To:"));
                    let crops: Vec<(CropTo, &str)> = vec![
                        (CropTo::BoundingBox, tl!("Bounding Box")),
                        (CropTo::MediaBox, tl!("Media Box")),
                        (CropTo::CropBox, tl!("Crop Box")),
                        (CropTo::BleedBox, tl!("Bleed Box")),
                        (CropTo::TrimBox, tl!("Trim Box")),
                        (CropTo::ArtBox, tl!("Art Box")),
                    ];
                    crate::widgets::dropdown(ui, "import-pdf-crop", &mut d.crop_to, &crops, 150.0);
                    ui.end_row();
                    ui.label(tl!("Resolution:"));
                    ui.horizontal(|ui| {
                        crate::widgets::value_field(ui, &mut d.resolution, 1.0..=2400.0, "", 70.0);
                        ui.label(egui::RichText::new(tl!("Pixels/Inch")).color(tk.text_dim));
                    });
                    ui.end_row();
                    ui.label(tl!("Image Size:"));
                    let size = match (d.crop_to, d.selected.as_slice(), d.pages.as_ref()) {
                        (CropTo::BoundingBox, ..) => tl!("fits what's on the page").to_string(),
                        (_, [one], Some(pages)) => pages.size_pt(*one, d.crop_to).map_or_else(String::new, |(w, h)| {
                            let k = d.resolution / 72.0;
                            format!("{} × {} px", ((w * k) as u32).max(1), ((h * k) as u32).max(1))
                        }),
                        _ => tl!("per page").to_string(),
                    };
                    ui.label(egui::RichText::new(size).color(tk.text_dim));
                    ui.end_row();
                    ui.label(tl!("Mode:"));
                    let modes = [("rgb".to_string(), tl!("RGB Color")), ("grayscale".to_string(), tl!("Grayscale")), ("cmyk".to_string(), tl!("CMYK Color")), ("lab".to_string(), tl!("Lab Color"))];
                    crate::widgets::dropdown(ui, "import-pdf-mode", &mut d.mode, &modes, 150.0);
                    ui.end_row();
                    ui.label(tl!("Bit Depth:"));
                    crate::widgets::dropdown(ui, "import-pdf-depth", &mut d.depth, &[(8, tl!("8 bit")), (16, tl!("16 bit"))], 150.0);
                    ui.end_row();
                    ui.label(tl!("Open As:"));
                    let ways = [("pixels".to_string(), tl!("Pixels")), ("layers".to_string(), tl!("Editable Layers"))];
                    crate::widgets::dropdown(ui, "import-pdf-as", &mut d.open_as, &ways, 150.0);
                    ui.end_row();
                });
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(if d.open_as == "layers" {
                        tl!("Text opens as type layers, vector art as shape layers, images as pixel layers.")
                    } else {
                        tl!("Each page opens as one pixel layer, as in Photoshop.")
                    })
                    .color(tk.text_dim),
                );
            });
        });
        ui.add_space(6.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            ok |= crate::widgets::primary_button(ui, tl!("OK"), 84.0).clicked();
            cancel = crate::widgets::secondary_button(ui, tl!("Cancel"), 84.0).clicked();
        });
    });
    cancel |= modal.should_close();
    if d.count == 0 {
        ok = false;
    }
    // Enter in the password field unlocks; it doesn't import.
    let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter)) && !app.import_pdf.as_ref().is_some_and(ImportPdf::is_locked);
    if ok || enter {
        if let Err(e) = confirm(app) {
            app.ui.status = e;
            app.ui.status_error = true;
        }
    } else if cancel {
        app.import_pdf = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog(count: usize) -> ImportPdf {
        ImportPdf {
            path: "a.pdf".into(),
            pages: None,
            count,
            selected: vec![0],
            anchor: 0,
            crop_to: CropTo::CropBox,
            resolution: 300.0,
            mode: "rgb".into(),
            depth: 8,
            open_as: "pixels".into(),
            thumbs: Vec::new(),
            password: String::new(),
            locked: None,
        }
    }

    #[test]
    fn picking_pages_as_in_a_file_list() {
        let mut d = dialog(6);
        d.click(2, false, false);
        assert_eq!(d.selected, [2]);
        d.click(4, false, true);
        assert_eq!(d.selected, [2, 3, 4], "shift: a run from the last click");
        d.click(3, true, false);
        assert_eq!(d.selected, [2, 4], "cmd: one out");
        d.click(0, true, false);
        assert_eq!(d.selected, [0, 2, 4], "cmd: one in");
        d.click(9, false, false);
        assert_eq!(d.selected, [0, 2, 4], "no such page");
        let mut one = dialog(3);
        one.click(0, true, false);
        assert_eq!(one.selected, [0], "the last page stays picked");
        assert_eq!(d.params()["pages"], json!([1, 3, 5]));
        assert_eq!(d.params()["cropTo"], "cropBox");
    }
}
