//! Front ends of the File-menu commands added with slices: the Save for Web (Legacy) dialog
//! (Original / Optimized / 2-Up / 4-Up previews, GIF / PNG-8 / PNG-24 / JPEG / WBMP settings,
//! image size, per-slice output), the Print dialog (page preview with position, scale, marks;
//! colour handling), and form dialogs for Contact Sheet II, Create Droplet, Statistics, Script
//! Events Manager, Package and Paths to Illustrator. Every OK runs the engine command with the
//! dialog's fields, so the control channel can drive them with `ui.dialog.set` / `confirm`.

use std::sync::Arc;

use photosplice_engine::web_cmds::{self, Optimized, WebSettings};
use serde_json::{Map, Value, json};

use crate::PhotospliceApp;
use crate::state::DialogKind;
use crate::theme::Tokens;

fn no_params(p: &Value) -> bool {
    p.as_object().is_none_or(|o| o.is_empty())
}

fn default_dir(app: &PhotospliceApp) -> String {
    app.session.active().and_then(|d| d.path.as_deref()).and_then(|p| p.rfind(['/', '\\']).map(|i| p[..i].to_string())).unwrap_or_else(|| ".".into())
}

/// A packaged macOS app does not start in a useful writable working directory. A saved
/// document keeps its own folder; otherwise prefer the user's Documents folder.
fn droplet_default_dir(app: &PhotospliceApp) -> String {
    let dir = default_dir(app);
    if dir != "." {
        return dir;
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        let home = std::path::PathBuf::from(home);
        let documents = home.join("Documents");
        return if documents.is_dir() { documents } else { home }.to_string_lossy().into_owned();
    }
    dir
}

fn doc_stem(app: &PhotospliceApp) -> String {
    app.session.active().map(|d| d.doc.name.rsplit_once('.').map_or(d.doc.name.clone(), |(a, _)| a.to_string())).unwrap_or_else(|| tl!("Untitled").into())
}

/// A generic form dialog for `command` (rendered by `view_cmds::form_body`).
fn form(app: &mut PhotospliceApp, command: &str, label: &str, fields: Value, choices: Value) -> Value {
    let mut f = Map::new();
    f.insert("__command".into(), json!(command));
    f.insert("__label".into(), json!(label));
    f.insert("__form".into(), json!(true));
    f.insert("__choices".into(), choices);
    if let Value::Object(m) = fields {
        f.extend(m);
    }
    json!({"dialog": app.ui.open_dialog(DialogKind::Command, f)})
}

/// File › Scripts › Browse… lists Photoshop's script types (matching its filter, #2452) plus the
/// JSON/text/droplet files the engine parses; selecting a Photoshop script gets the engine's
/// "not Photoshop ExtendScript" error instead of hiding those files from the picker.
const SCRIPT_EXTENSIONS: &[&str] = &["psjs", "ccjs", "jsx", "js", "jsxbin", "json", "txt", "pcdroplet"];

/// Menu items fronted here (dialogs before the engine command). `None` when not ours.
pub fn invoke(app: &mut PhotospliceApp, ctx: &egui::Context, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if !no_params(params) {
        return None;
    }
    let dir = default_dir(app);
    let stem = doc_stem(app);
    Some(Ok(match id {
        "file.export.saveForWebLegacy" => {
            app.session.active()?;
            json!({"dialog": open_web(app)})
        }
        "file.print" => {
            app.session.active()?;
            json!({"dialog": open_print(app)})
        }
        "file.printOneCopy" if app.session.file_menu.last_print.is_none() => {
            app.session.active()?;
            json!({"dialog": open_print(app)})
        }
        "file.export.exportPreferences" => return crate::prefs_ui::invoke(app, ctx, "edit.preferences.export", &json!({})),
        "file.automate.contactSheetII" => form(
            app,
            id,
            tl!("Contact Sheet II"),
            json!({"input": dir, "units": "inches", "width": 8.0, "height": 10.0, "resolution": 300.0, "mode": "rgb", "depth": 8, "columns": 5, "rows": 6, "placeAcrossFirst": true, "autoSpacing": true, "rotateForBestFit": false, "caption": true, "font": photosplice_text::fonts::DEFAULT_FAMILY, "fontSize": 12.0, "flatten": false}),
            json!({"units": ["inches", "cm", "pixels"], "mode": ["rgb", "gray", "cmyk", "lab"]}),
        ),
        "file.automate.createDroplet" => {
            let dir = droplet_default_dir(app);
            let Some(action) = crate::actions::selected_action(app) else {
                return Some(Err("record an action in the Actions panel first".into()));
            };
            let steps = crate::actions::action_steps(action);
            let name = action.name.clone();
            form(
                app,
                id,
                tl!("Create Droplet"),
                json!({"path": format!("{dir}/{name}.pcdroplet"), "name": name, "steps": steps, "output": format!("{dir}/droplet-output"), "format": "same"}),
                json!({"format": ["same", "png", "jpg", "psd", "tiff"]}),
            )
        }
        "file.scripts.statistics" => json!({"dialog": crate::stack_dialog::open_statistics(app)}),
        "file.scripts.browse" => {
            return Some(app.pick_file_bytes_filtered(SCRIPT_EXTENSIONS, |app, name, bytes| {
                let r = app.run("file.scripts.browse", json!({"name": name, "script": String::from_utf8_lossy(&bytes)}));
                if r.is_ok() {
                    let template = "Ran script {name}";
                    app.ui.status = crate::i18n::fmt(tl!(template), &[("name", &name)]);
                }
                r
            }));
        }
        "file.scripts.scriptEventsManager" => {
            let enabled = app.session.prefs().script_events.enabled;
            let events: Vec<&str> = photosplice_engine::automate_cmds::EVENTS.iter().map(|e| e.0).collect();
            form(app, id, "Script Events Manager", json!({"enabled": enabled, "event": "openDocument", "script": "", "name": ""}), json!({"event": events}))
        }
        "file.package" => form(app, id, "Package", json!({"dir": dir}), json!({})),
        "file.export.pathsToIllustrator" => {
            let mut names = vec!["all".to_string()];
            if let Some(d) = app.session.active() {
                if d.doc.work_path.is_some() {
                    names.push("work".into());
                }
                names.extend(d.doc.paths.iter().map(|p| p.name.clone()));
            }
            form(app, id, "Export Paths to File", json!({"path": format!("{dir}/{stem}.ai"), "paths": "all"}), json!({"paths": names}))
        }
        _ => return None,
    }))
}

/// Checked state of toggle items owned here.
pub fn checked(app: &PhotospliceApp, id: &str) -> Option<bool> {
    match id {
        "view.lockSlices" => Some(app.session.file_menu.slices_locked),
        "file.generate.imageAssets" => Some(app.session.active().is_some_and(|d| app.session.file_menu.image_assets.contains(&d.doc.id))),
        _ => None,
    }
}

/// Dialogs whose body and confirm live here.
pub fn owns(f: &Map<String, Value>) -> bool {
    f.contains_key("__web") || f.contains_key("__print")
}

pub fn dialog_width(f: &Map<String, Value>) -> Option<f32> {
    if f.contains_key("__web") {
        Some(1090.0)
    } else if f.contains_key("__print") {
        Some(720.0)
    } else {
        None
    }
}

pub fn ok_label(f: &Map<String, Value>) -> Option<&'static str> {
    if f.contains_key("__web") {
        Some(tl!("Save…"))
    } else if f.contains_key("__print") {
        Some(tl!("Print"))
    } else {
        None
    }
}

pub fn body(app: &mut PhotospliceApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    if f.contains_key("__web") {
        web_body(app, ui, f);
    } else {
        print_body(app, ui, f);
    }
}

pub fn confirm(app: &mut PhotospliceApp, f: &Map<String, Value>) -> Result<Value, String> {
    if f.contains_key("__web") { web_confirm(app, f) } else { print_confirm(app, f) }
}

fn params(f: &Map<String, Value>) -> Value {
    Value::Object(f.iter().filter(|(k, _)| !k.starts_with("__")).map(|(k, v)| (k.clone(), v.clone())).collect())
}

fn s(f: &Map<String, Value>, k: &str, d: &str) -> String {
    f.get(k).and_then(Value::as_str).unwrap_or(d).to_string()
}
fn n(f: &Map<String, Value>, k: &str, d: f64) -> f64 {
    f.get(k).and_then(Value::as_f64).unwrap_or(d)
}
fn b(f: &Map<String, Value>, k: &str, d: bool) -> bool {
    f.get(k).and_then(Value::as_bool).unwrap_or(d)
}

// ---------- Save for Web ----------

const WEB_COMMAND: &str = "file.export.saveForWebLegacy";
const WEB_FORMATS: [(&str, &str); 7] =
    [("gif", "GIF"), ("png8", "PNG-8"), ("png24", "PNG-24"), ("jpeg", "JPEG"), ("webp", "WebP"), ("webpLossless", "WebP (lossless)"), ("wbmp", "WBMP")];

/// File › Save As adds the whole-document formats, saved through [`PhotospliceApp::save_as`]: the
/// document takes the new path and is marked saved. The web formats below them write one
/// optimised file through `file.export.saveForWebLegacy`, then retarget the document the same way.
/// The native .pcraft format isn't offered (PSD is the document format); a .pcraft file that is
/// opened still saves back in place, and the engine still writes it.
const DOC_FORMATS: [(&str, &str); 5] = [("psd", "PSD"), ("psb", "PSB"), ("tif", "TIFF"), ("exr", "OpenEXR"), ("pdf", "PDF")];

/// File › Save As's formats: the document formats and the web ones, in alphabetical order of the
/// name shown. (Save for Web (Legacy) keeps Photoshop's order.)
fn save_as_formats() -> Vec<(&'static str, &'static str)> {
    let mut v: Vec<(&str, &str)> = DOC_FORMATS.iter().chain(WEB_FORMATS.iter()).copied().collect();
    v.sort_by_key(|(_, label)| label.to_lowercase());
    v
}

fn is_doc_format(fmt: &str) -> bool {
    DOC_FORMATS.iter().any(|(k, _)| *k == fmt)
}

/// The lower-case extension of `path`, with `.jpeg` / `.jpe` / `.jfif` read as `jpg` and
/// `.tiff` as `tif`.
fn ext_of(path: &str) -> String {
    let ext = path.rsplit(['/', '\\']).next().and_then(|n| n.rsplit_once('.')).map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "jpeg" | "jpe" | "jfif" => "jpg".into(),
        "tiff" => "tif".into(),
        _ => ext,
    }
}

/// Can File › Save write this kind of file back in place? Everything PhotoSplice writes. A PDF is
/// saved layered with its document attached, so nothing is lost; one of several pages opens
/// untitled (Import PDF), so Save can't drop the others.
pub(crate) fn writes_in_place(path: &str) -> bool {
    let ext = ext_of(path);
    matches!(ext.as_str(), "psd" | "psb" | "pcraft" | "pdf") || photosplice_codecs::from_extension(&ext).is_some_and(|f| photosplice_codecs::caps(f).write)
}

/// File › Save without a dialog: the document goes back to its own file, in its format and at
/// the settings it was opened or last saved with (a JPEG keeps its quality; a file last written
/// by the Save As screen is written with those settings again). `None` when there is nothing to
/// save in place (a new document, a PDF page, a camera raw file): Save As then asks.
pub fn save_in_place(app: &mut PhotospliceApp) -> Option<Result<Value, String>> {
    // Edit Contents documents save back into their smart object (the Save As path does that).
    if app.session.is_enabled("layer.smartObjects.saveContents") {
        return None;
    }
    let st = app.session.active()?;
    let path = st.path.clone()?;
    let web = st.save_options.web.clone().filter(|w| {
        WebSettings::from_params(w, "file.export.saveForWebLegacy").is_ok_and(|s| ext_of(&format!("x.{}", s.format.ext())) == ext_of(&path))
    });
    if let Some(mut p) = web {
        p["path"] = json!(path);
        return Some(app.run(WEB_COMMAND, p).inspect(|_| saved_as(app, &path)));
    }
    if !writes_in_place(&path) {
        return None;
    }
    // Recorded as a plain File › Save, so an action writes whichever file it is played on.
    let journaled = app.session.journal.len();
    let r = app.save_as(Some(path.clone()));
    if r.is_ok() && app.session.journal.len() > journaled {
        app.journal_as_plain_save(&path);
    }
    Some(r)
}

/// The document is now the file at `path`, saved.
fn saved_as(app: &mut PhotospliceApp, path: &str) {
    if let Some(st) = app.session.active_mut() {
        st.path = Some(path.to_string());
        st.saved_revision = st.revision;
    }
    app.ui.status = format!("Saved {path}");
    app.sync_views();
}

/// File › Save As (and Save on a document that cannot be saved in place): the Save for Web
/// screen, starting on the document's own format.
pub fn open_save_as(app: &mut PhotospliceApp) -> u64 {
    let ext = app.session.active().and_then(|d| d.path.clone()).and_then(|p| p.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())).unwrap_or_default();
    let format = match ext.as_str() {
        "tiff" => "tif".to_string(),
        "jpg" | "jpeg" => "jpeg".into(),
        "png" => "png24".into(),
        "gif" | "wbmp" | "webp" => ext,
        e if is_doc_format(e) => ext,
        _ => "psd".into(),
    };
    let id = open_web(app);
    if let Some(d) = app.ui.dialog_mut(id) {
        d.fields.insert("__saveAs".into(), json!(true));
        d.fields.insert("__label".into(), json!("Save As"));
        d.fields.insert("format".into(), json!(format));
    }
    id
}

pub fn open_web(app: &mut PhotospliceApp) -> u64 {
    let mut f = Map::new();
    f.insert("__web".into(), json!(true));
    f.insert("__command".into(), json!(WEB_COMMAND));
    f.insert("__label".into(), json!("Save for Web (Legacy)"));
    f.insert("__view".into(), json!("2up"));
    // Remember encoding choices, but each document starts at its own full size. This session's
    // last export lives in `file_menu`; the last one before a restart comes from the
    // preferences' remembered dialog settings (`web_cmds::remember_web_settings`).
    let last = app
        .session
        .file_menu
        .last_web
        .clone()
        .or_else(|| app.session.prefs().dialogs.get(WEB_COMMAND).cloned())
        .unwrap_or_else(|| json!({"format": "jpeg", "quality": 60}));
    if let Value::Object(m) = last {
        for (k, v) in m {
            if !matches!(k.as_str(), "path" | "dir" | "width" | "height" | "percent" | "colorTable" | "colorTableEdits") {
                f.insert(k, v);
            }
        }
    }
    for (k, v) in [
        ("format", json!("jpeg")),
        ("quality", json!(60)),
        ("palette", json!("selective")),
        ("colors", json!(128)),
        ("dither", json!("diffusion")),
        ("ditherAmount", json!(88)),
        ("transparency", json!(true)),
        ("matte", json!("#ffffff")),
        ("interlaced", json!(false)),
        ("progressive", json!(false)),
        ("optimized", json!(true)),
        ("embedIcc", json!(false)),
        ("convertToSrgb", json!(true)),
        ("metadata", json!("copyright")),
        ("percent", json!(100.0)),
        ("webSnap", json!(0)),
    ] {
        f.entry(k.to_string()).or_insert(v);
    }
    app.ui.open_dialog(DialogKind::Command, f)
}

/// Settings variants shown in 4-Up (current, then two smaller alternatives).
fn variants(p: &Value) -> Vec<Value> {
    let mut out = vec![p.clone()];
    let fmt = p["format"].as_str().unwrap_or("jpeg");
    for k in [2.0, 4.0] {
        let mut v = p.clone();
        if let Some(m) = v.as_object_mut() {
            m.remove("colorTable");
            m.remove("colorTableEdits");
        }
        if fmt == "jpeg" || fmt == "webp" {
            v["quality"] = json!((p["quality"].as_f64().unwrap_or(60.0) / k).round().max(1.0));
        } else if fmt == "gif" || fmt == "png8" {
            v["colors"] = json!((p["colors"].as_f64().unwrap_or(128.0) / k).round().max(2.0));
        } else {
            v["format"] = json!(if k == 2.0 { "png8" } else { "gif" });
            v["colors"] = json!(256 / k as u32);
        }
        out.push(v);
    }
    out
}

/// The flattened preview image: pixels, width, height, full-size / proxy pixel ratio.
type WebProxy = (Arc<Vec<[f32; 4]>>, u32, u32, f64);
/// The crop of the export a zoomed-in pane shows: (signature with the crop, its texture).
type WebCrop = (String, Arc<egui::TextureHandle>);

/// Photoshop's zoom levels for the Save for Web previews (its zoom menu and the −/+ steps).
const WEB_ZOOMS: [f32; 15] = [6.25, 12.5, 25.0, 100.0 / 3.0, 50.0, 200.0 / 3.0, 100.0, 200.0, 300.0, 400.0, 600.0, 800.0, 1000.0, 1200.0, 1600.0];

/// The previews' shared view, as Photoshop keeps them in step: zoom in screen points per
/// exported pixel (`None` = fit in the pane) and the centre in exported pixels (`None` = the
/// image's). Kept in the dialog's `__zoom` / `__cx` / `__cy` fields.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct WebView {
    zoom: Option<f32>,
    centre: Option<[f32; 2]>,
}

impl WebView {
    fn read(f: &Map<String, Value>) -> Self {
        let num = |k: &str| f.get(k).and_then(Value::as_f64).map(|v| v as f32).filter(|v| v.is_finite());
        let zoom = num("__zoom").filter(|z| *z > 0.0);
        let centre = num("__cx").zip(num("__cy")).map(|(x, y)| [x, y]);
        WebView { zoom, centre }
    }

    fn write(self, f: &mut Map<String, Value>) {
        match self.zoom {
            Some(z) => f.insert("__zoom".into(), json!(z)),
            None => f.remove("__zoom"),
        };
        match self.centre {
            Some([x, y]) => {
                f.insert("__cx".into(), json!(x));
                f.insert("__cy".into(), json!(y))
            }
            None => {
                f.remove("__cx");
                f.remove("__cy")
            }
        };
    }
}

/// Zoom and centre a pane of `pane` points shows an `ew` × `eh` export at, the centre kept
/// where the image still covers the pane.
fn web_layout(view: &WebView, pane: egui::Vec2, ew: f32, eh: f32) -> (f32, [f32; 2]) {
    let fit = (pane.x / ew).min(pane.y / eh).min(1.0);
    let z = view.zoom.unwrap_or(fit).clamp(0.005, 64.0);
    let c = view.centre.unwrap_or([ew / 2.0, eh / 2.0]);
    let keep = |c: f32, len: f32, extent: f32| {
        let half = extent / (2.0 * z);
        if len <= 2.0 * half { len / 2.0 } else { c.clamp(half, len - half) }
    };
    (z, [keep(c[0], ew, pane.x), keep(c[1], eh, pane.y)])
}
/// A pane's preview: (signature, texture, (estimated bytes, colours, extension), its RGBA for the
/// pixel readout).
type WebCache = (String, Arc<egui::TextureHandle>, Option<(usize, Option<usize>, &'static str)>, Arc<Vec<u8>>);

/// Renders one preview pane: the original (`None`) or an optimised variant.
/// One preview pane (the original when `p` is `None`): the image at the shared view, and below
/// it Photoshop's two columns of figures. Returns the pixel under the pointer (RGBA).
fn web_pane(app: &PhotospliceApp, ui: &mut egui::Ui, size: egui::Vec2, p: Option<&Value>, view: &mut WebView) -> Option<[u8; 4]> {
    let t = Tokens::get(ui.ctx());
    let st = app.session.active()?;
    let doc = st.doc.clone();
    let rev = st.revision;
    // The proxy: the export size, at most 640 px on the long side.
    let base = p.cloned().unwrap_or_else(|| json!({}));
    let size_sig = format!("{}|{}|{}", base["percent"], base["width"], base["height"]);
    let wkey = egui::Id::new(("web-proxy", doc.id.0, rev, size_sig.clone()));
    let proxy: Option<WebProxy> = ui.data(|d| d.get_temp(wkey));
    let (px, w, h, ratio) = match proxy {
        Some(v) => v,
        None => {
            let pct = base["percent"].as_f64().unwrap_or(100.0).max(1.0) / 100.0;
            let (fw, fh) = (f64::from(doc.size.width) * pct, f64::from(doc.size.height) * pct);
            let k = (640.0 / fw.max(fh)).min(1.0);
            let st = WebSettings::default();
            let (wd, _, _) =
                web_cmds::web_document(&doc, &json!({"width": (fw * k).round().max(1.0), "height": (fh * k).round().max(1.0), "resample": "bilinear"}), &st).ok()?;
            let buf = photosplice_compose::flatten(&wd);
            let v = (Arc::new(buf.px), wd.size.width, wd.size.height, (fw * fh) / (f64::from(wd.size.width) * f64::from(wd.size.height)).max(1.0));
            ui.data_mut(|d| d.insert_temp(wkey, v.clone()));
            v
        }
    };
    let sig = format!("{}{}", size_sig, p.map(|v| v.to_string()).unwrap_or_else(|| "original".into()));
    let key = egui::Id::new(("web-pane", doc.id.0, rev, sig.clone()));
    let cached: Option<WebCache> = ui.data(|d| d.get_temp(key));
    let (tex, info, pixels) = match cached.filter(|c| c.0 == sig) {
        Some((_, tex, info, pixels)) => (tex, info, pixels),
        None => {
            let (rgba, info) = match p {
                None => (px.iter().flat_map(|q| q.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)).collect::<Vec<u8>>(), None),
                Some(p) => match WebSettings::from_params(p, "preview")
                    .and_then(|st| web_cmds::optimize(&px, w as usize, photosplice_geom::Rect::new(0, 0, w as i32, h as i32), &st, None, None, 72.0, true))
                {
                    Ok(Optimized { preview, bytes, colors, ext, .. }) => (preview, Some(((bytes.len() as f64 * ratio) as usize, colors, ext))),
                    Err(_) => (vec![0; (w * h * 4) as usize], None),
                },
            };
            let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
            let tex = Arc::new(ui.ctx().load_texture("web-pane", img, egui::TextureOptions::LINEAR));
            let pixels = Arc::new(rgba);
            ui.data_mut(|d| d.insert_temp(key, (sig.clone(), tex.clone(), info, pixels.clone())));
            (tex, info, pixels)
        }
    };
    // The export's own size (the proxy is at most 640 px across).
    let pct = base["percent"].as_f64().unwrap_or(100.0).max(1.0) / 100.0;
    let (ew, eh) = ((f64::from(doc.size.width) * pct).round().max(1.0) as f32, (f64::from(doc.size.height) * pct).round().max(1.0) as f32);
    let info_h = 52.0;
    let mut hovered = None;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let (r, resp) = ui.allocate_exact_size(size - egui::vec2(0.0, info_h), egui::Sense::click_and_drag());
        let (z, c) = web_layout(view, r.size(), ew, eh);
        // Drag or scroll pans; pinch (or ⌘-scroll) zooms about the pointer, every pane alike.
        let (zoom_by, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta));
        if resp.dragged() || (resp.hovered() && scroll != egui::Vec2::ZERO) {
            let d = if resp.dragged() { resp.drag_delta() } else { scroll };
            *view = WebView { zoom: Some(z), centre: Some([c[0] - d.x / z, c[1] - d.y / z]) };
        }
        if resp.hovered()
            && (zoom_by - 1.0).abs() > 1e-4
            && let Some(at) = resp.hover_pos()
        {
            let nz = (z * zoom_by).clamp(0.005, 64.0);
            let off = at - r.center();
            let under = [c[0] + off.x / z, c[1] + off.y / z];
            *view = WebView { zoom: Some(nz), centre: Some([under[0] - off.x / nz, under[1] - off.y / nz]) };
        }
        ui.painter().rect_filled(r, 0.0, crate::theme::canvas_bg(&t));
        // The part of the export in view (export pixels) and where it lands.
        let (hx, hy) = (r.width() / (2.0 * z), r.height() / (2.0 * z));
        let vis = egui::Rect::from_min_max(egui::pos2((c[0] - hx).max(0.0), (c[1] - hy).max(0.0)), egui::pos2((c[0] + hx).min(ew), (c[1] + hy).min(eh)));
        let on_screen = |q: egui::Pos2| r.center() + (q - egui::pos2(c[0], c[1])) * z;
        let ppp = ui.ctx().pixels_per_point();
        let painter = ui.painter().with_clip_rect(r);
        let shown = egui::Rect::from_min_max(on_screen(vis.min), on_screen(vis.max));
        crate::widgets::checker(&painter, shown, 8.0);
        if w as f32 / ew >= z * ppp * 0.999 {
            // The proxy has the pixels for this zoom.
            let uv = egui::Rect::from_min_max(egui::pos2(vis.min.x / ew, vis.min.y / eh), egui::pos2(vis.max.x / ew, vis.max.y / eh));
            painter.image(tex.id(), shown, uv, egui::Color32::WHITE);
        } else if let Some((crop_tex, crop)) = web_crop(ui, &doc, rev, &size_sig, &sig, p, (ew, eh), vis) {
            painter.image(crop_tex.id(), egui::Rect::from_min_max(on_screen(crop.min), on_screen(crop.max)), egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        }
        // The pixel under the pointer (at the proxy's resolution).
        if let Some(at) = resp.hover_pos().filter(|a| shown.contains(*a)) {
            let q = egui::pos2(c[0], c[1]) + (at - r.center()) / z;
            let (x, y) = ((q.x / ew * w as f32) as usize, (q.y / eh * h as f32) as usize);
            let i = (y.min(h as usize - 1) * w as usize + x.min(w as usize - 1)) * 4;
            hovered = pixels.get(i..i + 4).map(|v| [v[0], v[1], v[2], v[3]]);
        }
        ui.painter().rect_stroke(r, 0.0, egui::Stroke::new(1.0, t.separator), egui::StrokeKind::Inside);
        // Photoshop's figures: format, size and download time on the left; how it was reduced
        // on the right.
        let (ir, _) = ui.allocate_exact_size(egui::vec2(size.x, info_h), egui::Sense::hover());
        ui.painter().rect_filled(ir, 0.0, t.field);
        ui.painter().rect_stroke(ir, 0.0, egui::Stroke::new(1.0, t.separator), egui::StrokeKind::Inside);
        let (left, right): (Vec<String>, Vec<String>) = match (p, info) {
            (None, _) => (
                vec![format!("{}: \"{}\"", tl!("Original"), doc.name), format!("{} × {} px", doc.size.width, doc.size.height)],
                Vec::new(),
            ),
            (Some(p), Some((bytes, colors, ext))) => {
                // Photoshop's download estimate at 56.6 Kbps.
                let secs = (bytes as f64 * 8.0 / 56_600.0).ceil();
                let mut right = Vec::new();
                match p["format"].as_str().unwrap_or("") {
                    "gif" | "png8" => {
                        let dither = if p["dither"].as_str() == Some("none") { 0.0 } else { p["ditherAmount"].as_f64().unwrap_or(88.0) };
                        right.push(crate::i18n::fmt(tl!("{n}% dither"), &[("n", &format!("{dither:.0}"))]));
                        right.push(crate::i18n::fmt(tl!("{palette} palette"), &[("palette", &title_case(p["palette"].as_str().unwrap_or("selective")))]));
                    }
                    "jpeg" | "webp" => right.push(crate::i18n::fmt(tl!("{n} quality"), &[("n", &format!("{:.0}", p["quality"].as_f64().unwrap_or(60.0)))])),
                    _ => {}
                }
                if let Some(c) = colors {
                    right.push(crate::i18n::fmt(tl!("{n} colors"), &[("n", &c.to_string())]));
                }
                (vec![ext.to_uppercase(), crate::sizing::human_bytes(bytes as f64), format!("{secs} sec @ 56.6 Kbps")], right)
            }
            (Some(_), None) => (vec!["—".into()], Vec::new()),
        };
        let font = egui::FontId::proportional(11.5);
        for (i, l) in left.iter().enumerate() {
            let at = egui::pos2(ir.left() + 8.0, ir.top() + 4.0 + i as f32 * 15.0);
            ui.painter().with_clip_rect(ir).text(at, egui::Align2::LEFT_TOP, l, font.clone(), if i == 0 { t.text } else { t.text_dim });
        }
        for (i, l) in right.iter().enumerate() {
            let at = egui::pos2(ir.right() - 8.0, ir.top() + 4.0 + i as f32 * 15.0);
            ui.painter().with_clip_rect(ir).text(at, egui::Align2::RIGHT_TOP, l, font.clone(), t.text_dim);
        }
    });
    hovered
}

/// `"selective"` → `"Selective"`.
fn title_case(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// The exported pixels in `vis` (export pixels, widened to whole ones) as `p` optimises them (the
/// original's when `None`): what a pane zoomed past the proxy's resolution shows. The export is
/// built once per document revision and size; only the crop in view is rendered and optimised
/// (so an indexed palette is the crop's own).
#[allow(clippy::too_many_arguments)]
fn web_crop(ui: &egui::Ui, doc: &photosplice_doc::Document, rev: u64, size_sig: &str, sig: &str, p: Option<&Value>, (ew, eh): (f32, f32), vis: egui::Rect) -> Option<(Arc<egui::TextureHandle>, egui::Rect)> {
    let crop = photosplice_geom::Rect::new(vis.min.x.floor() as i32, vis.min.y.floor() as i32, vis.max.x.ceil() as i32, vis.max.y.ceil() as i32);
    if crop.is_empty() {
        return None;
    }
    let fkey = egui::Id::new(("web-full", doc.id.0, rev, size_sig.to_string()));
    let full: Option<Arc<photosplice_doc::Document>> = ui.data(|d| d.get_temp(fkey));
    let full = match full {
        Some(f) => f,
        None => {
            let (wd, _, _) = web_cmds::web_document(doc, &json!({"width": ew, "height": eh, "resample": "bicubic"}), &WebSettings::default()).ok()?;
            let wd = Arc::new(wd);
            ui.data_mut(|d| d.insert_temp(fkey, wd.clone()));
            wd
        }
    };
    let ckey = egui::Id::new(("web-crop", doc.id.0, sig.to_string()));
    let want = format!("{rev}|{crop:?}");
    let cached: Option<WebCrop> = ui.data(|d| d.get_temp(ckey));
    let tex = match cached.filter(|c| c.0 == want) {
        Some((_, tex)) => tex,
        None => {
            let buf = photosplice_compose::render(&full, crop);
            let (w, h) = (crop.width() as usize, crop.height() as usize);
            let rgba = match p {
                None => buf.px.iter().flat_map(|q| q.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)).collect(),
                Some(p) => WebSettings::from_params(p, "preview")
                    .and_then(|st| web_cmds::optimize(&buf.px, w, photosplice_geom::Rect::new(0, 0, w as i32, h as i32), &st, None, None, 72.0, true))
                    .map(|o| o.preview)
                    .ok()?,
            };
            let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
            let tex = Arc::new(ui.ctx().load_texture("web-crop", img, egui::TextureOptions::NEAREST));
            ui.data_mut(|d| d.insert_temp(ckey, (want, tex.clone())));
            tex
        }
    };
    Some((tex, egui::Rect::from_min_max(egui::pos2(crop.x0 as f32, crop.y0 as f32), egui::pos2(crop.x1 as f32, crop.y1 as f32))))
}

/// Print's Printer menu: the default printer (value ""), then every printer `file.printers`
/// listed. A saved name the spooler no longer lists stays selectable rather than vanishing.
/// CUPS queue names stand in for spaces with `_`; the labels show the spaces.
fn printer_options(list: Option<&Value>, current: &str) -> Vec<(String, String)> {
    let printers: Vec<(&str, bool)> = list
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|p| Some((p.get("name")?.as_str()?, p.get("default").and_then(Value::as_bool).unwrap_or(false)))).collect())
        .unwrap_or_default();
    let pretty = |n: &str| n.replace('_', " ");
    let default = match printers.iter().find(|p| p.1) {
        Some((n, _)) => format!("{} ({})", tl!("Default printer"), pretty(n)),
        None => tl!("Default printer").to_string(),
    };
    let mut opts = vec![(String::new(), default)];
    opts.extend(printers.iter().map(|(n, _)| (n.to_string(), pretty(n))));
    if !current.is_empty() && !printers.iter().any(|p| p.0 == current) {
        opts.push((current.to_string(), pretty(current)));
    }
    opts
}

fn dropdown_str(ui: &mut egui::Ui, id: &str, f: &mut Map<String, Value>, key: &str, opts: &[(&str, &str)], w: f32) {
    let mut v = s(f, key, opts.first().map_or("", |o| o.0));
    let o: Vec<(String, &str)> = opts.iter().map(|(k, l)| (k.to_string(), *l)).collect();
    if crate::widgets::dropdown(ui, id, &mut v, &o, w) {
        f.insert(key.into(), json!(v));
    }
}

fn check(ui: &mut egui::Ui, f: &mut Map<String, Value>, key: &str, label: &str, d: bool) {
    let mut v = b(f, key, d);
    crate::widgets::checkbox(ui, &mut v, label);
    f.insert(key.into(), json!(v));
}

fn number(ui: &mut egui::Ui, f: &mut Map<String, Value>, key: &str, label: &str, range: std::ops::RangeInclusive<f32>, unit: &str, d: f64) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!(&label)).color(t.text_dim));
        let mut v = n(f, key, d) as f32;
        crate::widgets::value_field(ui, &mut v, range, unit, 64.0);
        f.insert(key.into(), json!(v.round()));
    });
}

/// The export size (pixels) the dialog's Image Size settings give `doc`.
fn export_size(doc: &photosplice_doc::Document, f: &Map<String, Value>) -> (f32, f32) {
    let pct = n(f, "percent", 100.0).max(1.0) / 100.0;
    ((f64::from(doc.size.width) * pct).round().max(1.0) as f32, (f64::from(doc.size.height) * pct).round().max(1.0) as f32)
}

/// A label right-aligned in a `w`-wide slot, as Photoshop lines up "Colors:", "Dither:", …
fn right_label(ui: &mut egui::Ui, text: &str, w: f32) {
    let t = Tokens::get(ui.ctx());
    ui.allocate_ui_with_layout(egui::vec2(w, 24.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.label(egui::RichText::new(text).color(t.text_dim));
    });
}

/// A settings row of Photoshop's grid: a control in the left column, a labelled one in the right.
fn web_row(
    ui: &mut egui::Ui,
    f: &mut Map<String, Value>,
    left: impl FnOnce(&mut egui::Ui, &mut Map<String, Value>),
    label: &str,
    right: impl FnOnce(&mut egui::Ui, &mut Map<String, Value>),
) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(egui::vec2(146.0, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| left(ui, f));
        if !label.is_empty() {
            right_label(ui, label, 70.0);
            right(ui, f);
        }
    });
}

/// A number field for `key` (no label: the row gives it).
fn web_number(ui: &mut egui::Ui, f: &mut Map<String, Value>, key: &str, range: std::ops::RangeInclusive<f32>, unit: &str, d: f64) {
    let mut v = n(f, key, d) as f32;
    crate::widgets::value_field(ui, &mut v, range, unit, 64.0);
    f.insert(key.into(), json!(v.round()));
}

fn web_dropdown(ui: &mut egui::Ui, id: &str, f: &mut Map<String, Value>, key: &str, opts: &[(&str, &str)]) {
    dropdown_str(ui, id, f, key, opts, 140.0);
}

fn web_matte(ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let mut m = s(f, "matte", "#ffffff");
    if ui.add(egui::TextEdit::singleline(&mut m).desired_width(64.0)).changed() {
        f.insert("matte".into(), json!(m));
    }
}

/// File › Export › Save for Web (Legacy) and File › Save As: Photoshop's layout. The previews
/// under Original / Optimized / 2-Up / 4-Up tabs with a zoom bar and the pixel readout below,
/// the settings in a column on the right.
fn web_body(app: &mut PhotospliceApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    let Some(doc) = app.session.active().map(|d| d.doc.clone()) else { return };
    let save_as = f.contains_key("__saveAs");
    // A whole-document format has nothing to optimise: just the image.
    let doc_fmt = save_as && is_doc_format(&s(f, "format", "psd"));
    let p = params(f);
    let mut web_view = WebView::read(f);
    let (ew, eh) = export_size(&doc, f);
    let ppp = ui.ctx().pixels_per_point();
    let preview = egui::vec2(760.0, 560.0);
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(preview.x);
            ui.spacing_mut().item_spacing.y = 0.0;
            // Tabs on a strip above the previews.
            let keys = ["original", "optimized", "2up", "4up"];
            let names = [tl!("Original"), tl!("Optimized"), tl!("2-Up"), tl!("4-Up")];
            let mut sel = if doc_fmt { 0 } else { keys.iter().position(|k| *k == s(f, "__view", "2up")).unwrap_or(2) };
            let (strip, _) = ui.allocate_exact_size(egui::vec2(preview.x, 28.0), egui::Sense::hover());
            ui.painter().rect_filled(strip, egui::CornerRadius { nw: 3, ne: 3, sw: 0, se: 0 }, t.tab_strip);
            ui.add_enabled_ui(!doc_fmt, |ui| {
                crate::tab_strip::pro_tabs(ui, ui.id().with("web-tabs"), strip, strip.right(), &names, &mut sel, false, false);
            });
            if !doc_fmt {
                f.insert("__view".into(), json!(keys[sel]));
            }
            // The panes.
            let inner = egui::vec2(preview.x - 16.0, preview.y - 16.0);
            let mut hovered = None;
            let mut main_pane = inner;
            egui::Frame::NONE.fill(t.card).inner_margin(8).show(ui, |ui| {
                ui.set_width(inner.x);
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
                let mut pane = |ui: &mut egui::Ui, size: egui::Vec2, p: Option<&Value>, hovered: &mut Option<[u8; 4]>| {
                    if let Some(px) = web_pane(app, ui, size, p, &mut web_view) {
                        *hovered = Some(px);
                    }
                };
                match keys[sel] {
                    "original" => pane(ui, inner, None, &mut hovered),
                    "optimized" => pane(ui, inner, Some(&p), &mut hovered),
                    "2up" => {
                        main_pane = egui::vec2((inner.x - 8.0) / 2.0, inner.y);
                        ui.horizontal(|ui| {
                            pane(ui, main_pane, None, &mut hovered);
                            pane(ui, main_pane, Some(&p), &mut hovered);
                        });
                    }
                    _ => {
                        let v = variants(&p);
                        main_pane = egui::vec2((inner.x - 8.0) / 2.0, (inner.y - 8.0) / 2.0);
                        ui.horizontal(|ui| {
                            pane(ui, main_pane, None, &mut hovered);
                            pane(ui, main_pane, Some(&v[0]), &mut hovered);
                        });
                        ui.horizontal(|ui| {
                            pane(ui, main_pane, Some(&v[1]), &mut hovered);
                            pane(ui, main_pane, Some(&v[2]), &mut hovered);
                        });
                    }
                }
            });
            // Zoom bar and pixel readout, as at the bottom of Photoshop's previews.
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let (z, _) = web_layout(&web_view, main_pane - egui::vec2(0.0, 52.0), ew, eh);
                let pct = z * ppp * 100.0;
                let to = |pct: f32| Some(pct / 100.0 / ppp);
                if crate::icons::button(ui, "minus", 22.0, false, tl!("Zoom out")).clicked() {
                    web_view.zoom = to(WEB_ZOOMS.iter().rev().copied().find(|v| *v < pct - 0.01).unwrap_or(WEB_ZOOMS[0]));
                }
                if crate::icons::button(ui, "plus", 22.0, false, tl!("Zoom in")).clicked() {
                    web_view.zoom = to(WEB_ZOOMS.iter().copied().find(|v| *v > pct + 0.01).unwrap_or(WEB_ZOOMS[WEB_ZOOMS.len() - 1]));
                }
                let mut typed = pct;
                if crate::widgets::value_field(ui, &mut typed, 1.0..=3200.0, "%", 64.0).changed() {
                    web_view.zoom = to(typed);
                }
                // Photoshop's zoom menu, beside the field.
                let (cr, cresp) = ui.allocate_exact_size(egui::vec2(18.0, 24.0), egui::Sense::click());
                crate::widgets::chevron_icon(ui, cr, ui.style().interact(&cresp), false);
                egui::Popup::menu(&cresp).show(|ui| {
                    ui.set_min_width(120.0);
                    for (i, z) in WEB_ZOOMS.iter().enumerate() {
                        if matches!(i, 6 | 7) {
                            ui.separator();
                        }
                        if ui.button(format!("{}%", z.floor())).clicked() {
                            web_view.zoom = to(*z);
                        }
                    }
                    ui.separator();
                    if ui.button(tl!("Actual Pixels")).clicked() {
                        web_view.zoom = to(100.0);
                    }
                    if ui.button(tl!("Fit in View")).clicked() {
                        // Fit, enlarging a small image too.
                        let pane = main_pane - egui::vec2(0.0, 52.0);
                        web_view = WebView { zoom: Some((pane.x / ew).min(pane.y / eh)), centre: None };
                    }
                    if ui.button(tl!("Fit on Screen")).clicked() {
                        web_view = WebView::default();
                    }
                });
                ui.add_space(14.0);
                let hex = hovered.map(|c| format!("{:02X}{:02X}{:02X}", c[0], c[1], c[2]));
                let v = |i: usize| hovered.map_or_else(|| "--".to_string(), |c| c[i].to_string());
                for (label, value) in [("R", v(0)), ("G", v(1)), ("B", v(2)), ("Alpha", v(3)), ("Hex", hex.unwrap_or_else(|| "--".into()))] {
                    ui.label(egui::RichText::new(format!("{}:", tl!(label))).color(t.text_dim));
                    ui.add_sized(egui::vec2(if label == "Hex" { 56.0 } else { 30.0 }, 20.0), egui::Label::new(egui::RichText::new(value).font(crate::theme::mono(12.0))));
                    ui.add_space(6.0);
                }
            });
        });
        ui.add_space(12.0);
        // Settings.
        ui.vertical(|ui| {
            ui.set_width(300.0);
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.horizontal(|ui| {
                right_label(ui, tl!("Preset:"), 52.0);
                let mut preset = s(f, "__preset", "");
                let mut opts: Vec<(String, &str)> = vec![("".into(), "[Unnamed]")];
                opts.extend(web_cmds::PRESETS.iter().map(|p| (p.to_string(), *p)));
                if crate::widgets::dropdown(ui, "web-preset", &mut preset, &opts, 236.0) && !preset.is_empty() {
                    if let Some(Value::Object(m)) = web_cmds::preset_settings(&preset).map(|st| st.to_params()) {
                        f.extend(m);
                    }
                    f.insert("__preset".into(), json!(preset));
                }
            });
            ui.horizontal(|ui| {
                ui.add_space(56.0);
                if save_as {
                    dropdown_str(ui, "web-format", f, "format", &save_as_formats(), 236.0);
                } else {
                    dropdown_str(ui, "web-format", f, "format", &WEB_FORMATS, 236.0);
                }
            });
            ui.add_space(4.0);
            if doc_fmt {
                // PDF: Photoshop's editing data, chosen here rather than in a dialog after Save.
                if s(f, "format", "psd") == "pdf" {
                    check(ui, f, "pdfEditing", tl!("Preserve Photoshop Editing Capabilities"), app.pdf_preserve_editing);
                    ui.label(egui::RichText::new(tl!("Photoshop and PhotoSplice reopen the PDF with its layers. Other applications show its page.")).color(t.text_dim));
                    ui.add_space(6.0);
                    pdf_options_ui(ui, f, app.pdf_options);
                }
                return;
            }
            let fmt = s(f, "format", "jpeg");
            let transparency = b(f, "transparency", true);
            match fmt.as_str() {
                "gif" | "png8" => {
                    let palettes = [
                        ("perceptual", tl!("Perceptual")),
                        ("selective", tl!("Selective")),
                        ("adaptive", tl!("Adaptive")),
                        ("restrictive", tl!("Restrictive (Web)")),
                        ("exact", tl!("Exact")),
                    ];
                    web_row(ui, f, |ui, f| web_dropdown(ui, "web-palette", f, "palette", &palettes), tl!("Colors:"), |ui, f| web_number(ui, f, "colors", 2.0..=256.0, "", 128.0));
                    let dithers = [("none", tl!("No Dither")), ("diffusion", tl!("Diffusion")), ("pattern", tl!("Pattern")), ("noise", tl!("Noise"))];
                    web_row(ui, f, |ui, f| web_dropdown(ui, "web-dither", f, "dither", &dithers), tl!("Dither:"), |ui, f| web_number(ui, f, "ditherAmount", 0.0..=100.0, "%", 88.0));
                    web_row(ui, f, |ui, f| check(ui, f, "transparency", tl!("Transparency"), true), tl!("Matte:"), web_matte);
                    web_row(ui, f, |ui, f| check(ui, f, "interlaced", tl!("Interlaced"), false), tl!("Web Snap:"), |ui, f| web_number(ui, f, "webSnap", 0.0..=100.0, "%", 0.0));
                }
                "jpeg" => {
                    web_row(ui, f, |ui, f| check(ui, f, "progressive", tl!("Progressive"), false), tl!("Quality:"), |ui, f| web_number(ui, f, "quality", 0.0..=100.0, "", 60.0));
                    web_row(ui, f, |ui, f| check(ui, f, "optimized", tl!("Optimized"), true), tl!("Matte:"), web_matte);
                    web_row(ui, f, |ui, f| check(ui, f, "embedIcc", tl!("Embed Color Profile"), false), "", |_, _| {});
                }
                "webp" | "webpLossless" => {
                    let lossy = fmt == "webp";
                    web_row(ui, f, |ui, f| check(ui, f, "transparency", tl!("Transparency"), true), if lossy { tl!("Quality:") } else { "" }, |ui, f| web_number(ui, f, "quality", 0.0..=100.0, "", 60.0));
                    web_row(ui, f, |ui, f| check(ui, f, "embedIcc", tl!("Embed Color Profile"), false), if transparency { "" } else { tl!("Matte:") }, web_matte);
                }
                "wbmp" => {
                    let dithers = [("none", tl!("No Dither")), ("diffusion", tl!("Diffusion")), ("pattern", tl!("Pattern"))];
                    web_row(ui, f, |ui, f| web_dropdown(ui, "web-dither", f, "dither", &dithers), "", |_, _| {});
                }
                _ => {
                    web_row(ui, f, |ui, f| check(ui, f, "transparency", tl!("Transparency"), true), if transparency { "" } else { tl!("Matte:") }, web_matte);
                    web_row(ui, f, |ui, f| check(ui, f, "interlaced", tl!("Interlaced"), false), "", |_, _| {});
                }
            }
            ui.add_space(4.0);
            crate::widgets::hairline(ui);
            check(ui, f, "convertToSrgb", tl!("Convert to sRGB"), true);
            ui.horizontal(|ui| {
                right_label(ui, tl!("Metadata:"), 64.0);
                dropdown_str(
                    ui,
                    "web-meta",
                    f,
                    "metadata",
                    &[("none", tl!("None")), ("copyright", tl!("Copyright")), ("copyrightAndContact", tl!("Copyright and Contact Info")), ("all", tl!("All"))],
                    224.0,
                );
            });
            ui.add_space(4.0);
            crate::widgets::hairline(ui);
            ui.label(egui::RichText::new(tl!("Image Size")).font(crate::theme::semibold(12.0)).color(t.text));
            // W and H stay in proportion (Photoshop's link); both set the percentage.
            let (dw, dh) = (doc.size.width.max(1) as f32, doc.size.height.max(1) as f32);
            ui.horizontal(|ui| {
                right_label(ui, tl!("W:"), 20.0);
                let mut w = ew;
                if crate::widgets::value_field(ui, &mut w, 1.0..=300_000.0, "px", 78.0).changed() {
                    f.insert("percent".into(), json!((w / dw * 100.0).clamp(1.0, 1000.0)));
                }
                right_label(ui, tl!("Percent:"), 90.0);
                web_number(ui, f, "percent", 1.0..=1000.0, "%", 100.0);
            });
            ui.horizontal(|ui| {
                right_label(ui, tl!("H:"), 20.0);
                let mut h = eh;
                if crate::widgets::value_field(ui, &mut h, 1.0..=300_000.0, "px", 78.0).changed() {
                    f.insert("percent".into(), json!((h / dh * 100.0).clamp(1.0, 1000.0)));
                }
                right_label(ui, tl!("Quality:"), 90.0);
                dropdown_str(ui, "web-resample", f, "resample", &[("bicubic", tl!("Bicubic")), ("bilinear", tl!("Bilinear")), ("nearest", tl!("Nearest Neighbor"))], 110.0);
            });
            let nslices = photosplice_doc::slices::resolve(&doc).len();
            if !doc.slices.is_empty() {
                ui.add_space(4.0);
                crate::widgets::hairline(ui);
                ui.label(egui::RichText::new(format!("{nslices} slices")).color(t.text_dim));
                check(ui, f, "html", tl!("Save HTML and Images"), true);
                dropdown_str(ui, "web-slices", f, "slices", &[("all", tl!("All Slices")), ("user", tl!("All User Slices"))], 150.0);
            }
        });
    });
    web_view.write(f);
}

/// Save Adobe PDF's Compatibility, Compression and Downsampling, as Photoshop lays them out.
fn pdf_options_ui(ui: &mut egui::Ui, f: &mut Map<String, Value>, cur: photosplice_io::pdf::export::PdfOptions) {
    use photosplice_io::pdf::export::Compatibility;
    let t = crate::theme::Tokens::get(ui.ctx());
    // Fields the dialog doesn't have yet start from the last PDF save.
    let (to, above) = cur.downsample.unwrap_or((300.0, 450.0));
    for (k, v) in [
        ("pdfCompatibility", json!(cur.compatibility.id())),
        ("pdfCompression", json!(if cur.jpeg_quality.is_some() { "jpeg" } else { "zip" })),
        ("pdfQuality", json!(cur.jpeg_quality.map_or("10".to_string(), |q| ((f32::from(q) - 1.0) / 99.0 * 12.0).round().to_string()))),
        ("pdfDownsampleMode", json!(if cur.downsample.is_some() { "bicubic" } else { "none" })),
        ("pdfDownsampleTo", json!(to.round())),
        ("pdfDownsampleAbove", json!(above.round())),
        ("pdfLayers", json!(cur.pdf_layers)),
    ] {
        f.entry(k).or_insert(v);
    }
    let label = |ui: &mut egui::Ui, s: &str| {
        ui.add_sized(egui::vec2(104.0, 22.0), egui::Label::new(egui::RichText::new(s).color(t.text_dim)));
    };
    let compat: Vec<(&str, &str)> = Compatibility::ALL.iter().map(|(_, id, l)| (*id, tl!(l))).collect();
    ui.horizontal(|ui| {
        label(ui, tl!("Compatibility:"));
        dropdown_str(ui, "pdf-compat", f, "pdfCompatibility", &compat, 190.0);
    });
    ui.horizontal(|ui| {
        label(ui, tl!("Compression:"));
        dropdown_str(ui, "pdf-compression", f, "pdfCompression", &[("zip", "ZIP"), ("jpeg", "JPEG")], 90.0);
        if s(f, "pdfCompression", "zip") == "jpeg" {
            ui.label(egui::RichText::new(tl!("Image Quality:")).color(t.text_dim));
            let q = [("2", tl!("Minimum")), ("4", tl!("Low")), ("6", tl!("Medium")), ("8", tl!("High")), ("10", tl!("Maximum"))];
            dropdown_str(ui, "pdf-quality", f, "pdfQuality", &q, 110.0);
        }
    });
    ui.horizontal(|ui| {
        label(ui, tl!("Downsampling:"));
        dropdown_str(ui, "pdf-downsample", f, "pdfDownsampleMode", &[("none", tl!("Do Not Downsample")), ("bicubic", tl!("Bicubic Downsampling To"))], 190.0);
    });
    if s(f, "pdfDownsampleMode", "none") == "bicubic" {
        ui.horizontal(|ui| {
            ui.add_space(108.0);
            web_number(ui, f, "pdfDownsampleTo", 1.0..=2400.0, "ppi", 300.0);
            ui.label(egui::RichText::new(tl!("for images above")).color(t.text_dim));
            web_number(ui, f, "pdfDownsampleAbove", 1.0..=4800.0, "ppi", 450.0);
        });
    }
    check(ui, f, "pdfLayers", tl!("Create PDF Layers from Top-Level Layers"), cur.pdf_layers);
}

/// The PDF options the dialog's fields set (missing ones keep `cur`).
pub(crate) fn pdf_options(f: &Map<String, Value>, cur: photosplice_io::pdf::export::PdfOptions) -> photosplice_io::pdf::export::PdfOptions {
    use photosplice_io::pdf::export::Compatibility;
    let mut o = cur;
    if let Some(c) = f.get("pdfCompatibility").and_then(Value::as_str).and_then(Compatibility::from_id) {
        o.compatibility = c;
    }
    if let Some(on) = f.get("pdfLayers").and_then(Value::as_bool) {
        o.pdf_layers = on;
    }
    let num = |k: &str| f.get(k).and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse().ok())).filter(|v: &f64| v.is_finite());
    match f.get("pdfCompression").and_then(Value::as_str) {
        Some("jpeg") => o.jpeg_quality = Some((num("pdfQuality").unwrap_or(10.0).clamp(0.0, 12.0) / 12.0 * 99.0 + 1.0).round() as u8),
        Some(_) => o.jpeg_quality = None,
        None => {}
    }
    match f.get("pdfDownsampleMode").and_then(Value::as_str) {
        Some("bicubic") => {
            let to = num("pdfDownsampleTo").unwrap_or(300.0).clamp(1.0, 2400.0);
            o.downsample = Some((to as f32, num("pdfDownsampleAbove").unwrap_or(450.0).clamp(to, 4800.0) as f32));
        }
        Some(_) => o.downsample = None,
        None => {}
    }
    o
}

fn web_confirm(app: &mut PhotospliceApp, f: &Map<String, Value>) -> Result<Value, String> {
    let save_as = f.contains_key("__saveAs");
    let fmt = s(f, "format", "jpeg");
    if save_as && is_doc_format(&fmt) {
        if fmt == "pdf" {
            app.pdf_preserve_editing = b(f, "pdfEditing", app.pdf_preserve_editing);
            app.pdf_options = pdf_options(f, app.pdf_options);
        }
        // A document format saves the document itself, which takes the new path.
        return match f.get("path").and_then(Value::as_str) {
            Some(p) => app.save_as(Some(p.to_string())),
            None => {
                let suggested = format!("{}.{fmt}", doc_stem(app));
                app.pick_save(&suggested, |app, path| app.save_as(Some(path)))
            }
        };
    }
    // Check while the control/input policy is still installed, before an asynchronous picker
    // can outlive the request. A browser download uses the interactive platform writer.
    let authorize = |p: &Value| -> Result<(), String> {
        if let Some(authorize) = app.session.authorize.or(if app.automation_input { app.services.automation_authorize } else { None }) {
            authorize(WEB_COMMAND, p).map_err(|e| e.to_string())?;
        }
        if app.automation_input
            && let Some(authorize) = app.services.automation_command.as_ref()
        {
            authorize(WEB_COMMAND, p)?;
        }
        Ok(())
    };
    let mut p = params(f);
    authorize(&p)?;
    if p.get("path").is_some() || p.get("dir").is_some() {
        return save_for_web(app, p, save_as);
    }
    let doc = app.session.active().ok_or("no document")?.doc.clone();
    let has_slices = !doc.slices.is_empty();
    let html = b(f, "html", true);
    let st = WebSettings::from_params(&p, WEB_COMMAND).map_err(|e| e.to_string())?;
    let base = photosplice_doc::slices::base_name(&doc);
    let suggested = if has_slices && html { format!("{base}.html") } else { format!("{base}.{}", st.format.ext()) };
    app.pick_save(&suggested, move |app, path| {
        app.refocus(doc.id)?;
        if has_slices {
            let dir = path.rfind(['/', '\\']).map(|i| path[..i].to_string()).unwrap_or_else(|| ".".into());
            p["dir"] = json!(dir);
            p["html"] = json!(html);
        } else {
            p["path"] = json!(path);
        }
        save_for_web(app, p, save_as)
    })
}

/// Runs Save for Web with `p`, which says where to write. From the Save As screen (`save_as`)
/// one file written means the document now is that file (sliced output stays an export), and
/// File › Save writes it again with these settings.
fn save_for_web(app: &mut PhotospliceApp, p: Value, save_as: bool) -> Result<Value, String> {
    // What File › Save writes this file with next time: these settings, at the document's size.
    let mut again = p.clone();
    if let Some(m) = again.as_object_mut() {
        for k in ["path", "dir", "html", "slices", "percent", "width", "height"] {
            m.remove(k);
        }
    }
    let retarget = save_as && p.get("path").is_some();
    #[cfg(target_arch = "wasm32")]
    let result = download_for_web(app, p);
    #[cfg(not(target_arch = "wasm32"))]
    let result = app.run(WEB_COMMAND, p);
    let r = result.inspect_err(|error| {
        app.ui.status = error.clone();
        app.ui.status_error = true;
    })?;
    app.sync_views();
    app.ui.status =
        crate::i18n::trn(crate::i18n::current(), r["files"].as_array().map_or(0, Vec::len) as u64, "Saved for Web: {n} file", "Saved for Web: {n} files");
    app.ui.status_error = false;
    if retarget && let Some(path) = r["files"].as_array().filter(|a| a.len() == 1).and_then(|a| a[0].as_str()).map(str::to_string) {
        saved_as(app, &path);
        if let Some(st) = app.session.active_mut() {
            st.save_options = photosplice_engine::SaveOptions { web: Some(again), ..Default::default() };
        }
    }
    Ok(r)
}

#[cfg(any(test, target_arch = "wasm32"))]
fn download_for_web(app: &mut PhotospliceApp, mut p: Value) -> Result<Value, String> {
    // Browser downloads have no directory hierarchy. Keep the HTML's image and spacer URLs
    // beside the downloaded page, matching the names handed to the platform service.
    p["imagesFolder"] = json!("");
    let write = app.services.write.as_mut().ok_or("no download service")?;
    web_cmds::save_for_web_with_writer(&mut app.session, &p, &mut |path, bytes| write(path, bytes).map_err(photosplice_engine::EngineError::Other))
        .map_err(|e| e.to_string())
}

// ---------- Print ----------

pub fn open_print(app: &mut PhotospliceApp) -> u64 {
    let mut f = Map::new();
    f.insert("__print".into(), json!(true));
    f.insert("__label".into(), json!("PhotoSplice Print Settings"));
    // Asked once per opening: the Printer menu lists what the spooler knows now.
    let printers = app.session.execute("file.printers", json!({})).unwrap_or_default();
    f.insert("__systemPrinters".into(), printers["printers"].clone());
    if let Some(Value::Object(m)) = app.session.file_menu.last_print.clone() {
        f.extend(m);
    }
    for (k, v) in [
        ("printer", json!("")),
        ("copies", json!(1)),
        ("paper", json!("letter")),
        ("orientation", json!("portrait")),
        ("colorHandling", json!("printerManages")),
        ("printerProfile", json!("coated-cmyk")),
        ("intent", json!("relative")),
        ("bpc", json!(true)),
        ("center", json!(true)),
        ("top", json!(0.0)),
        ("left", json!(0.0)),
        ("scale", json!(100.0)),
        ("scaleToFit", json!(false)),
        ("cornerCropMarks", json!(false)),
        ("centerCropMarks", json!(false)),
        ("registrationMarks", json!(false)),
        ("description", json!(false)),
        ("labels", json!(false)),
        ("emulsionDown", json!(false)),
        ("units", json!("cm")),
        ("output", json!("")),
    ] {
        f.entry(k.to_string()).or_insert(v);
    }
    // Windows: the system's printers, and the paper from the chosen printer's driver settings.
    if let Ok(list) = craft_print::rgb::printers() {
        f.insert("__printers".into(), json!(list.iter().map(|p| p.name.clone()).collect::<Vec<_>>()));
        let known = |n: &str| list.iter().any(|p| p.name == n);
        if !known(&s(&f, "printer", "")) {
            let fallback = list.iter().find(|p| p.default).or(list.first()).map(|p| p.name.clone()).unwrap_or_default();
            f.insert("printer".into(), json!(fallback));
            f.remove("printerSettings");
        }
    }
    app.ui.open_dialog(DialogKind::Command, f)
}

/// Takes the paper from the printer's driver settings (Windows), as Photoshop does.
fn driver_page(f: &mut Map<String, Value>) {
    let printer = s(f, "printer", "");
    let settings = f.get("printerSettings").and_then(Value::as_str).and_then(|h| craft_print::rgb::DevMode::from_hex(h).ok());
    match craft_print::rgb::page(&printer, settings.as_ref()) {
        Ok(pg) => {
            f.insert("paper".into(), json!([pg.paper.0, pg.paper.1]));
            f.insert("orientation".into(), json!("portrait"));
            f.remove("__pageError");
        }
        Err(e) => {
            f.insert("__pageError".into(), json!(e.to_string()));
        }
    }
}

/// Points per unit for the Position and Size fields.
fn unit_pt(units: &str) -> f64 {
    match units {
        "mm" => 72.0 / 25.4,
        "in" => 72.0,
        _ => 72.0 / 2.54,
    }
}

/// A number field in `units` for a value kept in points; true when edited.
fn length_field(ui: &mut egui::Ui, label: &str, pt: &mut f64, units: &str) -> bool {
    length_field_prec(ui, label, pt, units, 0)
}

fn length_field_prec(ui: &mut egui::Ui, label: &str, pt: &mut f64, units: &str, decimals: u32) -> bool {
    let t = Tokens::get(ui.ctx());
    let k = unit_pt(units);
    let mut v = (*pt / k) as f32;
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).color(t.text_dim));
        let before = v;
        crate::widgets::value_field_prec(ui, &mut v, 0.0..=10000.0, units, 70.0, decimals);
        if (v - before).abs() > 1e-4 && v.is_finite() {
            *pt = f64::from(v) * k;
            changed = true;
        }
    });
    changed
}

/// Unrotated paper dimensions in points, for either the current preset or a custom array.
fn print_paper_size(value: Option<&Value>) -> (f64, f64) {
    let default = (612.0, 792.0);
    match value {
        Some(Value::Array(a)) => (a.first().and_then(Value::as_f64).unwrap_or(default.0), a.get(1).and_then(Value::as_f64).unwrap_or(default.1)),
        Some(Value::String(name)) => photosplice_engine::print_cmds::PAPERS.iter().find(|p| p.0.eq_ignore_ascii_case(name)).map_or(default, |p| (p.1, p.2)),
        _ => default,
    }
}

fn print_paper_controls(ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let previous = f.get("paper").cloned();
    let mut selected = if previous.as_ref().is_some_and(Value::is_array) { "custom".to_string() } else { s(f, "paper", "letter") };
    ui.horizontal(|ui| {
        let mut papers: Vec<_> = photosplice_engine::print_cmds::PAPERS.iter().map(|p| (p.0.to_string(), p.0)).collect();
        papers.push(("custom".to_string(), tl!("Custom")));
        if crate::widgets::dropdown(ui, "print-paper", &mut selected, &papers, 110.0) {
            if selected == "custom" {
                let (w, h) = print_paper_size(f.get("__customPaper").or(previous.as_ref()));
                f.insert("paper".into(), json!([w, h]));
            } else {
                if let Some(paper @ Value::Array(_)) = previous {
                    f.insert("__customPaper".into(), paper);
                }
                f.insert("paper".into(), json!(selected));
            }
        }
        dropdown_str(ui, "print-orient", f, "orientation", &[("portrait", tl!("Portrait")), ("landscape", tl!("Landscape"))], 110.0);
    });
    if f.get("paper").is_some_and(Value::is_array) {
        let (mut w, mut h) = print_paper_size(f.get("paper"));
        let units = s(f, "units", "cm");
        let changed = ui
            .horizontal(|ui| {
                let wc = length_field_prec(ui, tl!("Paper Width"), &mut w, &units, 2);
                let hc = length_field_prec(ui, tl!("Paper Height"), &mut h, &units, 2);
                wc || hc
            })
            .inner;
        if changed {
            // Keep points in the command, and let the engine apply orientation and validation.
            f.insert("paper".into(), json!([w, h]));
            f.insert("__customPaper".into(), json!([w, h]));
        }
    }
}

fn print_body(app: &mut PhotospliceApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    let Some(doc) = app.session.active().map(|d| d.doc.clone()) else { return };
    // PDF exports and previews retain editable paper on Windows too. Only a real native
    // submission takes its media from the printer; entering that mode refreshes the driver
    // geometry after any export-only paper edits.
    let printers: Option<Vec<String>> = if !b(f, "dryRun", false) && s(f, "output", "").is_empty() {
        f.get("__printers").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
    } else {
        None
    };
    if printers.is_some() {
        if !b(f, "__driverPageInitialized", false) {
            driver_page(f);
            f.insert("__driverPageInitialized".into(), json!(true));
        }
    } else {
        f.remove("__driverPageInitialized");
    }
    let p = params(f);
    ui.horizontal_top(|ui| {
        // Page preview.
        ui.vertical(|ui| {
            let (area, _) = ui.allocate_exact_size(egui::vec2(330.0, 400.0), egui::Sense::hover());
            ui.painter().rect_filled(area, 0.0, t.canvas);
            match photosplice_engine::print_cmds::layout(&doc, &p, "file.print") {
                Ok(l) => {
                    let (pw, ph) = l.paper;
                    let k = ((area.width() - 20.0) / pw as f32).min((area.height() - 20.0) / ph as f32);
                    let page = egui::Rect::from_center_size(area.center(), egui::vec2(pw as f32 * k, ph as f32 * k));
                    ui.painter().rect_filled(page, 0.0, egui::Color32::WHITE);
                    ui.painter().rect_stroke(page, 0.0, egui::Stroke::new(1.0, t.separator), egui::StrokeKind::Outside);
                    let (x, y, w, h) = l.rect;
                    // Emulsion Down shows the page as it prints: mirrored.
                    let x = if l.emulsion_down { pw - x - w } else { x };
                    let img = egui::Rect::from_min_size(
                        egui::pos2(page.left() + x as f32 * k, page.bottom() - (y + h) as f32 * k),
                        egui::vec2(w as f32 * k, h as f32 * k),
                    );
                    let key = egui::Id::new(("print-thumb", doc.id.0, app.session.active().map_or(0, |d| d.revision)));
                    let tex: Option<Arc<egui::TextureHandle>> = ui.data(|d| d.get_temp(key));
                    let tex = tex.unwrap_or_else(|| {
                        let th = photosplice_compose::thumbnail(&doc, 400);
                        let ci = egui::ColorImage::from_rgba_unmultiplied([th.width as usize, th.height as usize], &th.pixels);
                        let tx = Arc::new(ui.ctx().load_texture("print-thumb", ci, egui::TextureOptions::LINEAR));
                        ui.data_mut(|d| d.insert_temp(key, tx.clone()));
                        tx
                    });
                    let clip = ui.painter().with_clip_rect(page);
                    let uv = if l.emulsion_down {
                        egui::Rect::from_min_max(egui::pos2(1.0, 0.0), egui::pos2(0.0, 1.0))
                    } else {
                        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0))
                    };
                    clip.image(tex.id(), img, uv, egui::Color32::WHITE);
                    let mark = egui::Stroke::new(1.0, egui::Color32::BLACK);
                    if l.marks.corner_crop {
                        for (c, dx, dy) in
                            [(img.left_top(), -1.0, -1.0), (img.right_top(), 1.0, -1.0), (img.left_bottom(), -1.0, 1.0), (img.right_bottom(), 1.0, 1.0)]
                        {
                            clip.line_segment([c + egui::vec2(dx * 3.0, 0.0), c + egui::vec2(dx * 12.0, 0.0)], mark);
                            clip.line_segment([c + egui::vec2(0.0, dy * 3.0), c + egui::vec2(0.0, dy * 12.0)], mark);
                        }
                    }
                    if l.marks.registration {
                        for c in [
                            img.center_top() - egui::vec2(0.0, 9.0),
                            img.center_bottom() + egui::vec2(0.0, 9.0),
                            img.left_center() - egui::vec2(9.0, 0.0),
                            img.right_center() + egui::vec2(9.0, 0.0),
                        ] {
                            clip.circle_stroke(c, 3.5, mark);
                        }
                    }
                    let fits = x >= 0.0 && y >= 0.0 && x + w <= pw && y + h <= ph;
                    let note = if fits { String::new() } else { "  ·  larger than the paper".to_string() };
                    ui.label(
                        egui::RichText::new(format!("Scale {:.1}%  ·  {:.2} × {:.2} in{note}", l.scale * 100.0, w / 72.0, h / 72.0))
                            .color(if fits { t.text_dim } else { t.warning })
                            .size(11.0),
                    );
                }
                Err(e) => {
                    ui.painter().text(area.center(), egui::Align2::CENTER_CENTER, e.to_string(), egui::FontId::proportional(12.0), t.text_dim);
                }
            }
        });
        ui.add_space(12.0);
        ui.vertical(|ui| {
            ui.set_width(340.0);
            let head = |ui: &mut egui::Ui, s: &str| {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(s).font(crate::theme::semibold(12.0)).color(t.text));
            };
            head(ui, tl!("Printer Setup"));
            if let Some(printers) = &printers {
                // Windows: Photoshop's Printer menu and Print Settings… (the driver's own dialog).
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(tl!("Printer")).color(t.text_dim));
                    let before = s(f, "printer", "");
                    let opts: Vec<(&str, &str)> = printers.iter().map(|n| (n.as_str(), n.as_str())).collect();
                    dropdown_str(ui, "print-printer", f, "printer", &opts, 220.0);
                    if s(f, "printer", "") != before {
                        f.remove("printerSettings");
                        driver_page(f);
                    }
                });
                ui.horizontal(|ui| {
                    number(ui, f, "copies", tl!("Copies"), 1.0..=999.0, "", 1.0);
                    if ui.button(tl!("Print Settings…")).clicked() {
                        let printer = s(f, "printer", "");
                        let current = f.get("printerSettings").and_then(Value::as_str).and_then(|h| craft_print::rgb::DevMode::from_hex(h).ok());
                        match craft_print::rgb::settings_dialog(&printer, current.as_ref(), None) {
                            Ok(Some(dm)) => {
                                f.insert("printerSettings".into(), json!(dm.to_hex()));
                                driver_page(f);
                            }
                            Ok(None) => {}
                            Err(e) => {
                                f.insert("__pageError".into(), json!(e.to_string()));
                            }
                        }
                    }
                });
                let paper = p.get("paper").and_then(Value::as_array).map(|a| (a.first().and_then(Value::as_f64), a.get(1).and_then(Value::as_f64)));
                let text = match paper {
                    Some((Some(w), Some(h))) => format!("{:.0} × {:.0} mm  ·  {}", w / 72.0 * 25.4, h / 72.0 * 25.4, tl!("set in Print Settings…")),
                    _ => String::new(),
                };
                ui.label(egui::RichText::new(text).color(t.text_dim).size(11.0));
                if let Some(e) = f.get("__pageError").and_then(Value::as_str) {
                    ui.label(egui::RichText::new(e).color(t.warning).size(11.0));
                }
            } else {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(tl!("Printer")).color(t.text_dim));
                    let mut pr = s(f, "printer", "");
                    let opts = printer_options(f.get("__systemPrinters"), &pr);
                    let o: Vec<(String, &str)> = opts.iter().map(|(k, l)| (k.clone(), l.as_str())).collect();
                    if crate::widgets::dropdown(ui, "print-printer", &mut pr, &o, 240.0) {
                        f.insert("printer".into(), json!(pr));
                    }
                });
                number(ui, f, "copies", tl!("Copies"), 1.0..=999.0, "", 1.0);
                print_paper_controls(ui, f);
            }
            head(ui, tl!("Color Management"));
            dropdown_str(
                ui,
                "print-color",
                f,
                "colorHandling",
                &[
                    ("printerManages", tl!("Printer Manages Colors")),
                    ("photospliceManages", tl!("PhotoSplice Manages Colors")),
                    ("noColorManagement", tl!("No Color Management")),
                ],
                240.0,
            );
            if s(f, "colorHandling", "") == "photospliceManages" {
                // Photoshop's Printer Profile menu: the built-ins, then every installed printer
                // (output class) profile by its description.
                let installed: Vec<_> = photosplice_engine::installed_profiles::installed()
                    .into_iter()
                    .filter(|p| p.is_destination() && p.class == photosplice_cms::ProfileClass::Output)
                    .collect();
                let mut profiles: Vec<(&str, &str)> = vec![
                    ("coated-cmyk", tl!("Coated CMYK")),
                    ("srgb", "sRGB IEC61966-2.1"),
                    ("adobe-rgb-compat", tl!("Adobe RGB (1998) compatible")),
                    ("display-p3", tl!("Display P3")),
                    ("gray-gamma-2.2", tl!("Gray Gamma 2.2")),
                ];
                profiles.extend(installed.iter().map(|p| (p.path.as_str(), p.description.as_str())));
                // A remembered profile that is no longer installed still shows, by its file name.
                let cur = s(f, "printerProfile", "coated-cmyk");
                let stem = std::path::Path::new(&cur).file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if !profiles.iter().any(|(v, _)| *v == cur) {
                    profiles.push((cur.as_str(), stem.as_str()));
                }
                dropdown_str(ui, "print-profile", f, "printerProfile", &profiles, 240.0);
                ui.horizontal(|ui| {
                    dropdown_str(
                        ui,
                        "print-intent",
                        f,
                        "intent",
                        &[
                            ("perceptual", tl!("Perceptual")),
                            ("relative", tl!("Relative Colorimetric")),
                            ("saturation", tl!("Saturation")),
                            ("absolute", tl!("Absolute Colorimetric")),
                        ],
                        170.0,
                    );
                    check(ui, f, "bpc", tl!("Black Point Compensation"), true);
                });
                if printers.is_some() {
                    ui.label(
                        egui::RichText::new(tl!("Remember to turn off the printer's own colour management in Print Settings…")).color(t.warning).size(11.0),
                    );
                }
            }
            head(ui, tl!("Position and Size"));
            dropdown_str(ui, "print-units", f, "units", &[("cm", tl!("Centimeters")), ("mm", tl!("Millimeters")), ("in", tl!("Inches"))], 130.0);
            let units = s(f, "units", "cm");
            check(ui, f, "center", tl!("Center"), true);
            if !b(f, "center", true) {
                ui.horizontal(|ui| {
                    // Kept in inches (Photoshop's Position fields), shown in the chosen units.
                    for (key, label) in [("top", tl!("Top")), ("left", tl!("Left"))] {
                        let mut pt = n(f, key, 0.0) * 72.0;
                        if length_field(ui, label, &mut pt, &units) {
                            f.insert(key.into(), json!(pt / 72.0));
                        }
                    }
                });
            }
            check(ui, f, "scaleToFit", tl!("Scale to Fit Media"), false);
            // Scaled Print Size: Scale, Height and Width move together; Print Resolution follows.
            if let Ok(l) = photosplice_engine::print_cmds::layout(&doc, &params(f), "file.print") {
                let (_, _, w, h) = l.rect;
                let (mut w2, mut h2) = (w, h);
                let fit = b(f, "scaleToFit", false);
                if !fit {
                    // Two decimals: Height and Width set fractional scales.
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(tl!("Scale")).color(t.text_dim));
                        let mut v = n(f, "scale", 100.0) as f32;
                        crate::widgets::value_field(ui, &mut v, 1.0..=1000.0, "%", 64.0);
                        f.insert("scale".into(), json!((f64::from(v) * 100.0).round() / 100.0));
                    });
                }
                ui.horizontal(|ui| {
                    let hc = length_field(ui, tl!("Height"), &mut h2, &units);
                    let wc = length_field(ui, tl!("Width"), &mut w2, &units);
                    let k = if wc && w > 0.0 {
                        Some(w2 / w)
                    } else if hc && h > 0.0 {
                        Some(h2 / h)
                    } else {
                        None
                    };
                    if let Some(k) = k.filter(|k| k.is_finite() && *k > 0.0) {
                        f.insert("scale".into(), json!(((l.scale * k * 100.0) * 100.0).round() / 100.0));
                        f.insert("scaleToFit".into(), json!(false));
                    }
                });
                if w > 0.0 {
                    let ppi = f64::from(doc.size.width) / (w / 72.0);
                    ui.label(egui::RichText::new(format!("{} {ppi:.0} PPI", tl!("Print Resolution:"))).color(t.text_dim).size(11.0));
                }
            }
            head(ui, tl!("Printing Marks"));
            ui.horizontal(|ui| {
                check(ui, f, "cornerCropMarks", tl!("Corner Crop Marks"), false);
                check(ui, f, "centerCropMarks", tl!("Center Crop Marks"), false);
            });
            ui.horizontal(|ui| {
                check(ui, f, "registrationMarks", tl!("Registration Marks"), false);
                check(ui, f, "description", tl!("Description"), false);
                check(ui, f, "labels", tl!("Labels"), false);
            });
            head(ui, tl!("Functions"));
            check(ui, f, "emulsionDown", tl!("Emulsion Down"), false);
            if b(f, "emulsionDown", false) && printers.is_some() {
                ui.label(egui::RichText::new(tl!("Don't also tick Mirror Image in Print Settings…: the two flips cancel out.")).color(t.text_dim).size(11.0));
            }
            head(ui, tl!("Save as PDF"));
            let mut out = s(f, "output", "");
            if ui.add(egui::TextEdit::singleline(&mut out).hint_text(tl!("(print to the printer)")).desired_width(300.0)).changed() {
                f.insert("output".into(), json!(out));
            }
        });
    });
}

fn print_confirm(app: &mut PhotospliceApp, f: &Map<String, Value>) -> Result<Value, String> {
    let r = app.run("file.print", params(f))?;
    if r["pending"] != json!(true) {
        print_status(app, &r);
    }
    Ok(r)
}

pub(crate) fn print_status(app: &mut PhotospliceApp, r: &Value) {
    app.ui.status = if let Some(printer) = r["spooler"]["printer"].as_str() {
        crate::i18n::fmt(tl!("Sent to {printer}"), &[("printer", printer)])
    } else if r["sent"] == json!(true) {
        let template = "Sent to the printer ({spooler})";
        crate::i18n::fmt(tl!(template), &[("spooler", r["spooler"].as_str().unwrap_or(""))])
    } else {
        let template = "Printed to {path}";
        crate::i18n::fmt(tl!(template), &[("path", r["pdf"].as_str().unwrap_or(""))])
    };
    app.ui.status_error = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_option_fields_set_the_options_and_keep_the_rest() {
        use photosplice_io::pdf::export::{Compatibility, PdfOptions};
        let cur = PdfOptions { compatibility: Compatibility::Pdf16, jpeg_quality: Some(50), downsample: None, pdf_layers: true };
        assert_eq!(pdf_options(&Map::new(), cur), cur, "no fields: as last time");
        let mut f = Map::new();
        f.insert("pdfCompatibility".into(), json!("2.0"));
        f.insert("pdfCompression".into(), json!("zip"));
        f.insert("pdfDownsampleMode".into(), json!("bicubic"));
        f.insert("pdfDownsampleTo".into(), json!(150));
        f.insert("pdfDownsampleAbove".into(), json!(100));
        let o = pdf_options(&f, cur);
        assert_eq!(o, PdfOptions { compatibility: Compatibility::Pdf20, jpeg_quality: None, downsample: Some((150.0, 150.0)), pdf_layers: true }, "the threshold is never below the target");
        f.insert("pdfLayers".into(), json!(false));
        assert!(!pdf_options(&f, cur).pdf_layers);
        f.insert("pdfCompression".into(), json!("jpeg"));
        f.insert("pdfQuality".into(), json!("6"));
        assert_eq!(pdf_options(&f, cur).jpeg_quality, Some(51));
        f.insert("pdfQuality".into(), json!("nonsense"));
        assert_eq!(pdf_options(&f, cur).jpeg_quality, Some(84), "unreadable quality: Maximum");
    }

    #[test]
    fn web_previews_fit_then_zoom_and_pan_inside_the_image() {
        let pane = egui::vec2(300.0, 200.0);
        // Fit: the 1200 × 400 export at a quarter, centred.
        let (z, c) = web_layout(&WebView::default(), pane, 1200.0, 400.0);
        assert_eq!((z, c), (0.25, [600.0, 200.0]));
        // A small export isn't enlarged to fit (Photoshop's 100% cap).
        assert_eq!(web_layout(&WebView::default(), pane, 100.0, 50.0).0, 1.0);
        // Zoomed in, the centre stays where the image still covers the pane.
        let v = WebView { zoom: Some(2.0), centre: Some([-50.0, 10_000.0]) };
        assert_eq!(web_layout(&v, pane, 1200.0, 400.0), (2.0, [75.0, 350.0]));
        // Zoomed out past the image, it sits in the middle.
        let v = WebView { zoom: Some(0.1), centre: Some([0.0, 0.0]) };
        assert_eq!(web_layout(&v, pane, 1200.0, 400.0).1, [600.0, 200.0]);
        // The view lives in the dialog's fields, out of the export's params.
        let mut f = Map::new();
        let v = WebView { zoom: Some(3.0), centre: Some([1.0, 2.0]) };
        v.write(&mut f);
        assert_eq!(WebView::read(&f), v);
        assert!(params(&f).as_object().unwrap().is_empty());
        WebView::default().write(&mut f);
        assert!(f.is_empty());
    }

    fn app() -> (PhotospliceApp, egui::Context) {
        let mut app = PhotospliceApp::new(photosplice_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 120, "height": 80, "name": "web"})).unwrap();
        (app, egui::Context::default())
    }

    #[test]
    fn save_as_lists_its_formats_alphabetically() {
        let labels: Vec<&str> = save_as_formats().iter().map(|(_, l)| *l).collect();
        let mut sorted = labels.clone();
        sorted.sort_by_key(|l| l.to_lowercase());
        assert_eq!(labels, sorted);
        assert_eq!(labels.len(), DOC_FORMATS.len() + WEB_FORMATS.len(), "every format is still offered");
        assert_eq!(labels.first(), Some(&"GIF"));
        assert!(save_as_formats().iter().all(|(k, _)| *k != "pcraft"), ".pcraft isn't offered");
    }

    #[test]
    fn save_for_web_dialog_runs_the_command() {
        let (mut app, ctx) = app();
        let r = crate::menus::invoke(&mut app, &ctx, "file.export.saveForWebLegacy", json!({})).unwrap();
        let id = r["dialog"].as_u64().unwrap();
        let dir = std::env::temp_dir().join(format!("pc-webui-{}", std::process::id()));
        let path = dir.join("web.gif").to_string_lossy().into_owned();
        let d = app.ui.dialog_mut(id).unwrap();
        d.fields.insert("format".into(), json!("gif"));
        d.fields.insert("path".into(), json!(path));
        let r = crate::dialogs::confirm(&mut app, id).unwrap();
        assert_eq!(r["files"][0], json!(path));
        assert!(std::fs::read(&path).unwrap().starts_with(b"GIF89a"));
        // The next dialog starts from those settings.
        let r = crate::menus::invoke(&mut app, &ctx, "file.export.saveForWebLegacy", json!({})).unwrap();
        let d = app.ui.dialog_mut(r["dialog"].as_u64().unwrap()).unwrap();
        assert_eq!(d.fields["format"], "gif");
        assert!(!d.fields.contains_key("path"));
    }

    /// A file written by the Save As screen is saved again with those settings (PNG-8, 16
    /// colours here), at the document's size, without a dialog.
    #[test]
    fn save_rewrites_a_save_as_screen_file_with_its_settings() {
        let (mut app, ctx) = app();
        let dir = std::env::temp_dir().join(format!("pc-saveagain-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("again.png").to_string_lossy().into_owned();
        let r = crate::menus::invoke(&mut app, &ctx, "file.saveAs", json!({})).unwrap();
        let id = r["dialog"].as_u64().unwrap();
        let d = app.ui.dialog_mut(id).unwrap();
        for (k, v) in [("format", json!("png8")), ("colors", json!(16)), ("percent", json!(50.0)), ("path", json!(path))] {
            d.fields.insert(k.into(), v);
        }
        crate::dialogs::confirm(&mut app, id).unwrap();
        assert_eq!(app.session.active().unwrap().path.as_deref(), Some(path.as_str()));
        std::fs::remove_file(&path).unwrap();
        app.run("layer.new.layer", json!({})).unwrap();
        assert!(app.session.active().unwrap().is_dirty());
        let r = crate::menus::invoke(&mut app, &ctx, "file.save", json!({})).unwrap();
        assert!(r.get("dialog").is_none(), "{r}");
        assert!(!app.session.active().unwrap().is_dirty());
        let png = std::fs::read(&path).unwrap();
        assert_eq!(png[25], 3, "PNG-8: indexed colour");
        // Full size again (120 × 80), not the 50 % the Save As screen wrote.
        assert_eq!(u32::from_be_bytes([png[16], png[17], png[18], png[19]]), 120);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writes_in_place_covers_what_photosplice_writes() {
        for p in ["a.psd", "a.PSB", "a.pcraft", "a.pdf", "a.png", "a.jpg", "a.jpeg", "a.tif", "a.tiff", "a.webp", "a.gif", "a.exr", "a.bmp"] {
            assert!(writes_in_place(p), "{p}");
        }
        for p in ["a.nef", "a.cr3", "a", "", ".", "dir.v2/file", "a.unknown"] {
            assert!(!writes_in_place(p), "{p}");
        }
    }

    #[test]
    fn droplet_folder_follows_the_document_or_uses_a_safe_fallback() {
        let (mut app, _) = app();
        let fallback = droplet_default_dir(&app);
        #[cfg(target_os = "macos")]
        if std::env::var_os("HOME").is_some() {
            assert!(std::path::Path::new(&fallback).is_absolute());
        }
        #[cfg(not(target_os = "macos"))]
        assert_eq!(fallback, ".");
        app.session.active_mut().unwrap().path = Some("/tmp/photosplice/drawing.pcraft".into());
        assert_eq!(droplet_default_dir(&app), "/tmp/photosplice");
    }

    /// A fresh session's dialog seeds from the preferences: the last export before a restart is
    /// remembered there (`web_cmds::remember_web_settings`), as `file_menu` is not saved.
    #[test]
    fn the_save_for_web_dialog_seeds_from_the_remembered_preferences() {
        let (mut app, _) = app();
        app.session.prefs.edit(|p| {
            p.dialogs.insert(WEB_COMMAND.into(), json!({"format": "png8", "colors": 64}));
        });
        let id = open_web(&mut app);
        let f = &app.ui.dialog_mut(id).unwrap().fields;
        assert_eq!(f["format"], "png8");
        assert_eq!(f["colors"], 64);
    }

    #[test]
    fn save_for_web_downloads_images_and_html_through_the_platform_writer() {
        let (mut app, _) = app();
        let downloads = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let captured = downloads.clone();
        app.services.write = Some(Box::new(move |path, bytes| {
            captured.borrow_mut().push((path.to_string(), bytes.to_vec()));
            Ok(())
        }));
        download_for_web(&mut app, json!({"path": "web.png", "format": "png24"})).unwrap();
        assert_eq!(downloads.borrow()[0].0, "web.png");
        assert!(downloads.borrow()[0].1.starts_with(b"\x89PNG\r\n\x1a\n"));
        downloads.borrow_mut().clear();

        app.run("slice.new", json!({"rect": [0, 0, 60, 80], "name": "left"})).unwrap();
        download_for_web(&mut app, json!({"dir": ".", "format": "png24", "html": true, "imagesFolder": "images"})).unwrap();
        let files = downloads.borrow();
        let html = std::str::from_utf8(&files.iter().find(|(path, _)| path.ends_with(".html")).unwrap().1).unwrap();
        assert!(!html.contains("images/"), "downloaded files are siblings, not an images directory");
        for (path, bytes) in files.iter().filter(|(path, _)| !path.ends_with(".html")) {
            let name = std::path::Path::new(path).file_name().unwrap().to_str().unwrap();
            assert!(html.contains(&format!("src=\"{name}\"")), "missing downloaded image {name}");
            assert!(!bytes.is_empty());
        }
        assert!(files.iter().any(|(path, _)| path.ends_with("spacer.gif")));
        drop(files);

        app.services.write = Some(Box::new(|_, _| Err("download failed".into())));
        assert_eq!(download_for_web(&mut app, json!({"path": "fail.png", "format": "png24"})).unwrap_err(), "download failed");
        app.services.write = None;
        assert_eq!(download_for_web(&mut app, json!({"path": "missing.png"})).unwrap_err(), "no download service");
    }

    #[test]
    fn save_for_web_authorizes_before_opening_a_picker() {
        fn deny(_: &str, _: &Value) -> photosplice_engine::Result<()> {
            Err(photosplice_engine::EngineError::Other("export denied".into()))
        }
        for explicit_path in [false, true] {
            for synthetic_input in [false, true] {
                let (mut app, ctx) = app();
                let id = open_web(&mut app);
                if explicit_path {
                    app.ui.dialog_mut(id).unwrap().fields.insert("path".into(), json!("denied.png"));
                }
                app.services.automation_authorize = Some(deny);
                app.services.file_dialog = Some(Box::new(|_, _, _| panic!("denied export must not show a picker")));
                app.services.write = Some(Box::new(|_, _| panic!("denied export must not write")));
                let error = if synthetic_input {
                    app.automation_input = true;
                    crate::dialogs::confirm(&mut app, id).unwrap_err()
                } else {
                    let (request, _) = crate::control::ControlRequest::new("ui.dialog.confirm", json!({"dialog": id}));
                    let crate::control::Outcome::Done(reply) = crate::control::handle(&mut app, &ctx, &request) else { panic!("expected immediate denial") };
                    assert_eq!(reply["ok"], false);
                    reply["error"].as_str().unwrap().to_string()
                };
                assert_eq!(error, "export denied");
                assert!(!app.file_dialog_open());
            }
        }
    }

    #[test]
    fn print_dialog_and_toggles() {
        let (mut app, ctx) = app();
        let r = crate::menus::invoke(&mut app, &ctx, "file.print", json!({})).unwrap();
        let id = r["dialog"].as_u64().unwrap();
        let out = std::env::temp_dir().join(format!("pc-printui-{}.pdf", std::process::id())).to_string_lossy().into_owned();
        app.ui.dialog_mut(id).unwrap().fields.insert("output".into(), json!(out));
        let r = crate::dialogs::confirm(&mut app, id).unwrap();
        assert_eq!(r["sent"], false);
        assert!(std::fs::read(&out).unwrap().starts_with(b"%PDF"));
        assert_eq!(checked(&app, "view.lockSlices"), Some(false));
        crate::menus::invoke(&mut app, &ctx, "view.lockSlices", json!({})).unwrap();
        assert_eq!(checked(&app, "view.lockSlices"), Some(true));
        crate::menus::invoke(&mut app, &ctx, "file.generate.imageAssets", json!({})).unwrap();
        assert_eq!(checked(&app, "file.generate.imageAssets"), Some(true));
        // Form dialogs open for the rest.
        for id in ["file.automate.contactSheetII", "file.scripts.statistics", "file.scripts.scriptEventsManager", "file.package"] {
            let r = crate::menus::invoke(&mut app, &ctx, id, json!({})).unwrap();
            assert!(r["dialog"].is_u64(), "{id}");
        }
    }

    #[test]
    fn printer_menu_lists_the_spooler_printers_after_the_default() {
        let list = json!([{"name": "HP_OfficeJet_3830_series", "default": true}, {"name": "DNP_DP_DS620", "default": false}]);
        let o = printer_options(Some(&list), "");
        let keys: Vec<&str> = o.iter().map(|p| p.0.as_str()).collect();
        assert_eq!(keys, ["", "HP_OfficeJet_3830_series", "DNP_DP_DS620"]);
        assert!(o[0].1.ends_with("(HP OfficeJet 3830 series)"), "{}", o[0].1);
        assert_eq!(o[2].1, "DNP DP DS620");
        // A remembered printer that's gone stays selectable; no list still offers the default.
        let o = printer_options(Some(&list), "Old_Queue");
        assert_eq!(o.last().unwrap().0, "Old_Queue");
        assert_eq!(printer_options(None, "").len(), 1);
        // The dialog asks the engine, and the list never reaches file.print's params.
        let (mut app, ctx) = app();
        let id = crate::menus::invoke(&mut app, &ctx, "file.print", json!({})).unwrap()["dialog"].as_u64().unwrap();
        let f = &app.ui.dialog_mut(id).unwrap().fields;
        assert!(f.get("__systemPrinters").is_some_and(Value::is_array));
        assert!(params(f).get("__systemPrinters").is_none());
        assert!(params(f).get("__printers").is_none());
    }

    #[test]
    fn print_dialog_does_not_inherit_scripted_send_override() {
        for send in [false, true] {
            let (mut app, ctx) = app();
            let out = std::env::temp_dir().join(format!("pc-printui-state-{}-{send}.pdf", std::process::id())).to_string_lossy().into_owned();
            app.session.execute("file.print", json!({"output": out, "send": send, "dryRun": true})).unwrap();
            assert!(app.session.file_menu.last_print.as_ref().unwrap().get("send").is_none());
            let r = crate::menus::invoke(&mut app, &ctx, "file.print", json!({})).unwrap();
            let id = r["dialog"].as_u64().unwrap();
            assert!(app.ui.dialog_mut(id).unwrap().fields.get("send").is_none());
            std::fs::remove_file(out).unwrap();
        }
    }
}
