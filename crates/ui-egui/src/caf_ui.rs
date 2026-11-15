//! Edit › Content-Aware Fill…: Reference's Content-Aware Fill workspace, a large dialog of its
//! own (a window of its own where the platform has them, as Camera Raw and the Filter Gallery).
//!
//! A tool strip (Sampling Area, Hand, Zoom) and the document on the left, with the selection
//! outlined and the sampling area tinted; on the right the Preview of the fill and the panel:
//! Sampling Area Overlay, Sampling Area Options (Auto / Rectangular / Custom, Sample All Layers),
//! Fill Settings (Color and Rotation Adaptation, Scale, Mirror) and Output Settings (Output To).
//! The sampling area and the preview are the command's own
//! ([`photosplice_engine::edit_menu_cmds::content_aware_sampling`] and
//! [`photosplice_engine::edit_menu_cmds::content_aware_fill_preview`]), computed on a worker
//! thread whenever a setting changes. OK runs `edit.contentAwareFill` with the dialog's params
//! (one history step).
//!
//! Custom sampling is a rectangle (the command's `area`): drag it in the document with the
//! Sampling Area tool.
//!
//! Control channel: `ui.menu.invoke {"id":"edit.contentAwareFill","params":{"ui":{"set":{…},
//! "overlay":{"show":bool,"opacity":0..100,"excluded":bool}, "tool":"area|hand|zoom",
//! "wait":true, "commit":true | "cancel":true}}}`; the reply describes the dialog (`wait` blocks
//! until the sampling area and the preview are computed).

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};

use egui::{Color32, FontId, Pos2, Rect as ERect, Sense, Stroke, TextureHandle, pos2, vec2};
use photosplice_doc::{Document, LayerId};
use photosplice_engine::jobs::JobCtx;
use photosplice_geom::Rect;
use serde_json::{Map, Value, json};

use crate::PhotospliceApp;
use crate::theme::Tokens;
use crate::widgets;

const CMD: &str = "edit.contentAwareFill";
const PANEL_W: f32 = 300.0;
const STRIP_W: f32 = 40.0;
const PREVIEW_H: f32 = 210.0;
/// The dialog's largest size; smaller windows get a smaller one ([`widgets::tool_dialog_rect`]).
const DIALOG_MAX: egui::Vec2 = vec2(1400.0, 900.0);
/// Longest side of the Preview's image (its pixels, not points).
const PREVIEW_SIDE: u32 = 900;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// Drag a rectangle: the Custom sampling area.
    Area,
    Hand,
    Zoom,
}

/// Sampling Area Overlay's settings (view only: they don't change the fill).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Overlay {
    pub show: bool,
    /// 0..=100.
    pub opacity: f32,
    pub color: [u8; 3],
    /// Tint the excluded area instead of the sampling area.
    pub excluded: bool,
}

impl Default for Overlay {
    fn default() -> Self {
        Self { show: true, opacity: 50.0, color: [0, 200, 0], excluded: false }
    }
}

/// What the worker sends back for one request.
enum Msg {
    Sampling(Rect, Vec<bool>),
    Preview(Rect, egui::ColorImage),
    Failed(String),
}

/// The computation running for the params `key`.
struct Running {
    key: String,
    rx: Receiver<Msg>,
    job: JobCtx,
}

pub struct CafDialog {
    pub layer: LayerId,
    /// The document as the dialog opened (the dialog is modal, so it is the one OK fills).
    doc: Arc<Document>,
    /// The command's params as the dialog edits them.
    pub params: Map<String, Value>,
    pub overlay: Overlay,
    pub tool: Tool,
    /// Sections open: Sampling Area Overlay, Sampling Area Options, Fill Settings, Output Settings.
    open: [bool; 4],
    /// 0 = fit; otherwise screen points per proxy pixel.
    zoom: f32,
    pan: egui::Vec2,
    /// The document's composite, reduced: `pw × ph`, `scale` proxy pixels per document pixel.
    proxy: Option<TextureHandle>,
    proxy_img: Option<egui::ColorImage>,
    pw: usize,
    ph: usize,
    scale: f32,
    /// The selection at proxy size (coverage), for its outline.
    sel: Vec<f32>,
    outline: Option<TextureHandle>,
    /// The sampling area (window, mask over it) and its tint at proxy size.
    sampling: Option<(Rect, Vec<bool>)>,
    tint: Option<TextureHandle>,
    tint_dirty: bool,
    /// The Preview: the area shown (document pixels) and its image.
    preview: Option<(Rect, TextureHandle)>,
    /// The params the shown sampling area and preview belong to.
    shown: String,
    running: Option<Running>,
    pub error: Option<String>,
    /// A Custom area being dragged (proxy pixels).
    drag: Option<(Pos2, Pos2)>,
}

fn defaults() -> Map<String, Value> {
    // Output To starts on New Layer, as in Reference's workspace.
    let v = json!({"sampling": "auto", "colorAdaptation": "default", "rotationAdaptation": "none", "scale": false, "mirror": false, "output": "new"});
    v.as_object().cloned().unwrap_or_default()
}

impl CafDialog {
    pub fn describe(&self) -> Value {
        json!({
            "layer": self.layer.0,
            "params": Value::Object(self.params.clone()),
            "overlay": {"show": self.overlay.show, "opacity": self.overlay.opacity, "excluded": self.overlay.excluded},
            "tool": match self.tool { Tool::Area => "area", Tool::Hand => "hand", Tool::Zoom => "zoom" },
            "sampling": self.sampling.as_ref().map(|(w, m)| json!({"window": [w.x0, w.y0, w.width(), w.height()], "sampled": m.iter().filter(|s| **s).count()})),
            "preview": self.preview.as_ref().map(|(r, _)| json!([r.x0, r.y0, r.width(), r.height()])),
            "computing": self.running.is_some(),
            "error": self.error,
        })
    }

    fn key(&self) -> String {
        Value::Object(self.params.clone()).to_string()
    }

    fn str_param(&self, k: &str) -> String {
        self.params.get(k).and_then(Value::as_str).unwrap_or_default().to_string()
    }

    fn flag(&self, k: &str) -> bool {
        self.params.get(k).and_then(Value::as_bool).unwrap_or(false)
    }

    fn set(&mut self, k: &str, v: Value) {
        self.params.insert(k.into(), v);
    }

    /// Start computing the sampling area and the preview for the current params, cancelling
    /// any computation for older ones.
    fn request(&mut self, ctx: &egui::Context) {
        let key = self.key();
        if self.running.as_ref().is_some_and(|r| r.key == key) || (self.running.is_none() && self.shown == key) {
            return;
        }
        if let Some(r) = self.running.take() {
            r.job.cancel();
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let job = JobCtx::new();
        let work = job.clone();
        let (doc, layer, params, repaint) = (self.doc.clone(), self.layer, Value::Object(self.params.clone()), ctx.clone());
        let run = move || {
            let send = |m: Msg| {
                let _ = tx.send(m);
                repaint.request_repaint();
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| compute(&doc, layer, &params, &work, &send)));
            match result {
                Ok(Err(e)) if !work.cancelled() => send(Msg::Failed(e)),
                Err(_) => send(Msg::Failed("Content-Aware Fill preview failed".into())),
                _ => {}
            }
        };
        // The web build has no threads: there the preview is computed when a setting changes.
        if cfg!(target_arch = "wasm32") {
            run();
            self.running = Some(Running { key, rx, job });
            return;
        }
        match std::thread::Builder::new().name("content-aware-fill-preview".into()).spawn(run) {
            Ok(_) => self.running = Some(Running { key, rx, job }),
            Err(e) => self.error = Some(format!("Could not start the preview: {e}")),
        }
    }

    /// Take what the worker has sent.
    fn poll(&mut self, ctx: &egui::Context) {
        let Some(r) = self.running.as_ref() else { return };
        loop {
            match r.rx.try_recv() {
                Ok(Msg::Sampling(win, mask)) => {
                    self.sampling = Some((win, mask));
                    self.tint_dirty = true;
                }
                Ok(Msg::Preview(area, img)) => {
                    self.preview = Some((area, ctx.load_texture("caf-preview", img, egui::TextureOptions::LINEAR)));
                    self.error = None;
                    self.shown = r.key.clone();
                    self.running = None;
                    return;
                }
                Ok(Msg::Failed(e)) => {
                    self.error = Some(e);
                    self.shown = r.key.clone();
                    self.running = None;
                    return;
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.shown = r.key.clone();
                    self.running = None;
                    return;
                }
            }
        }
    }

    /// Rebuild the sampling-area tint at proxy size.
    fn rebuild_tint(&mut self, ctx: &egui::Context) {
        self.tint_dirty = false;
        let Some((win, mask)) = self.sampling.as_ref() else {
            self.tint = None;
            return;
        };
        let o = self.overlay;
        let alpha = (o.opacity.clamp(0.0, 100.0) / 100.0 * 255.0).round() as u8;
        let fill = Color32::from_rgba_unmultiplied(o.color[0], o.color[1], o.color[2], alpha);
        let (ww, wh) = (win.width() as usize, win.height() as usize);
        let mut px = vec![Color32::TRANSPARENT; self.pw * self.ph];
        for y in 0..self.ph {
            for x in 0..self.pw {
                let (dx, dy) = (((x as f32 + 0.5) / self.scale) as i32, ((y as f32 + 0.5) / self.scale) as i32);
                let (wx, wy) = (dx - win.x0, dy - win.y0);
                let sampled = wx >= 0 && wy >= 0 && (wx as usize) < ww && (wy as usize) < wh && mask.get(wy as usize * ww + wx as usize).copied().unwrap_or(false);
                let selected = self.sel.get(y * self.pw + x).is_some_and(|c| *c > 0.5);
                let tinted = if o.excluded { !sampled && !selected } else { sampled };
                if tinted {
                    px[y * self.pw + x] = fill;
                }
            }
        }
        let img = egui::ColorImage::new([self.pw, self.ph], px);
        self.tint = Some(ctx.load_texture("caf-sampling", img, egui::TextureOptions::NEAREST));
    }
}

/// The worker: the sampling area first (so the overlay updates at once), then the preview.
fn compute(doc: &Arc<Document>, layer: LayerId, params: &Value, job: &JobCtx, send: &dyn Fn(Msg)) -> Result<(), String> {
    use photosplice_engine::edit_menu_cmds::{content_aware_fill_preview, content_aware_sampling};
    let cancel = || job.cancelled();
    let (win, mask) = content_aware_sampling(doc, layer, params, &cancel).map_err(|e| e.to_string())?;
    send(Msg::Sampling(win, mask));
    let (window, _, filled) = content_aware_fill_preview(doc, layer, params, job).map_err(|e| e.to_string())?;
    // The fill written into a copy of the document, composited over the selection and some of
    // its surroundings: what the document will look like.
    let mut after = (**doc).clone();
    if let Some(s) = after.layer_mut(layer).and_then(|l| l.surface_mut()) {
        s.write_region(window, &filled);
    }
    let sel = doc.selection.as_ref().map(|s| s.content_bounds()).unwrap_or(window).intersect(&doc.bounds());
    let grow = (sel.width().max(sel.height()) / 4).max(16) as i32;
    let area = sel.inflate(grow).intersect(&doc.bounds());
    if area.is_empty() || job.cancelled() {
        return Ok(());
    }
    let k = (PREVIEW_SIDE as f32 / area.width().max(area.height()).max(1) as f32).min(1.0);
    let (w, h) = (((area.width() as f32 * k).round() as u32).max(1), ((area.height() as f32 * k).round() as u32).max(1));
    let img = photosplice_compose::render_reduced_rect(&after, area, w, h).to_rgba8();
    let image = egui::ColorImage::from_rgba_unmultiplied([img.width as usize, img.height as usize], &img.pixels);
    send(Msg::Preview(area, image));
    Ok(())
}

/// Opens the workspace on the active layer and its selection.
pub fn open(app: &mut PhotospliceApp, ctx: &egui::Context) -> Result<(), String> {
    photosplice_engine::commands::find(CMD).map(|c| (c.enabled)(&app.session)).unwrap_or(Err("unknown command".into()))?;
    let st = app.session.active().ok_or("no document")?;
    let layer = st.active_layer.ok_or("no active layer")?;
    let doc = st.doc.clone();
    let canvas = doc.bounds();
    let (w, h) = (canvas.width().max(1) as f32, canvas.height().max(1) as f32);
    let screen = widgets::tool_dialog_rect(ctx, DIALOG_MAX);
    let fit = ((screen.width() - PANEL_W - STRIP_W - 32.0) / w).min((screen.height() - 80.0) / h);
    let side = ((w.max(h) * fit * ctx.pixels_per_point()).round() as u32).clamp(256, 2400);
    let thumb = photosplice_compose::thumbnail(&doc, side);
    let (pw, ph) = (thumb.width.max(1) as usize, thumb.height.max(1) as usize);
    let proxy_img = egui::ColorImage::from_rgba_unmultiplied([pw, ph], &thumb.pixels);
    let scale = pw as f32 / w;
    // The selection's coverage at proxy size (sampled at each proxy pixel's centre).
    let mut sel = vec![0.0f32; pw * ph];
    if let Some(s) = doc.selection.as_ref() {
        let b = s.content_bounds().intersect(&canvas);
        if !b.is_empty() {
            let mut m = vec![0.0f32; b.width() as usize * b.height() as usize];
            s.read_region_into(b, &mut m);
            let bw = b.width() as usize;
            for y in 0..ph {
                for x in 0..pw {
                    let (dx, dy) = (((x as f32 + 0.5) / scale) as i32, ((y as f32 + 0.5) / scale) as i32);
                    if b.contains(dx, dy) {
                        sel[y * pw + x] = m.get((dy - b.y0) as usize * bw + (dx - b.x0) as usize).copied().unwrap_or(0.0);
                    }
                }
            }
        }
    }
    let mut d = CafDialog {
        layer,
        doc,
        params: defaults(),
        overlay: Overlay::default(),
        tool: Tool::Hand,
        open: [false, true, true, true],
        zoom: 0.0,
        pan: egui::Vec2::ZERO,
        proxy: None,
        proxy_img: Some(proxy_img),
        pw,
        ph,
        scale,
        sel,
        outline: None,
        sampling: None,
        tint: None,
        tint_dirty: false,
        preview: None,
        shown: String::new(),
        running: None,
        error: None,
        drag: None,
    };
    d.request(ctx);
    app.caf = Some(d);
    Ok(())
}

/// The selection's outline at proxy size: marching-ant pixels where it meets the unselected.
fn outline_image(sel: &[f32], w: usize, h: usize) -> egui::ColorImage {
    let on = |x: usize, y: usize| sel.get(y * w + x).is_some_and(|c| *c > 0.5);
    let mut px = vec![Color32::TRANSPARENT; w * h];
    for y in 0..h {
        for x in 0..w {
            if !on(x, y) {
                continue;
            }
            let edge = x == 0 || y == 0 || x + 1 == w || y + 1 == h || !on(x - 1, y) || !on(x + 1, y) || !on(x, y - 1) || !on(x, y + 1);
            if edge {
                px[y * w + x] = if (x + y) / 4 % 2 == 0 { Color32::BLACK } else { Color32::WHITE };
            }
        }
    }
    egui::ColorImage::new([w, h], px)
}

/// Menu / control-channel entry point. `None` when the call isn't for this dialog (a script
/// with the command's own params runs the command directly).
pub fn menu(app: &mut PhotospliceApp, ctx: &egui::Context, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if id != CMD {
        return None;
    }
    let empty = params.as_object().is_none_or(|o| o.is_empty());
    if empty {
        return Some(open(app, ctx).map(|_| app.caf.as_ref().map(CafDialog::describe).unwrap_or(Value::Null)));
    }
    let ui = params.get("ui")?;
    if app.caf.is_none()
        && let Err(e) = open(app, ctx)
    {
        return Some(Err(e));
    }
    let d = app.caf.as_mut()?;
    if let Some(Value::Object(set)) = ui.get("set") {
        for (k, v) in set {
            d.params.insert(k.clone(), v.clone());
        }
    }
    if let Some(o) = ui.get("overlay") {
        if let Some(b) = o.get("show").and_then(Value::as_bool) {
            d.overlay.show = b;
        }
        if let Some(v) = o.get("opacity").and_then(Value::as_f64).filter(|v| v.is_finite()) {
            d.overlay.opacity = (v as f32).clamp(0.0, 100.0);
        }
        if let Some(b) = o.get("excluded").and_then(Value::as_bool) {
            d.overlay.excluded = b;
        }
        d.tint_dirty = true;
    }
    match ui.get("tool").and_then(Value::as_str) {
        Some("area") => d.tool = Tool::Area,
        Some("hand") => d.tool = Tool::Hand,
        Some("zoom") => d.tool = Tool::Zoom,
        _ => {}
    }
    d.request(ctx);
    #[cfg(not(target_arch = "wasm32"))]
    if ui.get("wait").and_then(Value::as_bool) == Some(true) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while d.running.is_some() && std::time::Instant::now() < deadline {
            d.poll(ctx);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    if ui.get("cancel").and_then(Value::as_bool) == Some(true) {
        cancel(app);
        return Some(Ok(json!({"cancelled": true})));
    }
    if ui.get("commit").and_then(Value::as_bool) == Some(true) {
        return Some(commit(app));
    }
    Some(Ok(app.caf.as_ref().map(CafDialog::describe).unwrap_or(Value::Null)))
}

fn cancel(app: &mut PhotospliceApp) {
    if let Some(r) = app.caf.take().and_then(|d| d.running) {
        r.job.cancel();
    }
}

fn commit(app: &mut PhotospliceApp) -> Result<Value, String> {
    let d = app.caf.take().ok_or("Content-Aware Fill isn't open")?;
    if let Some(r) = &d.running {
        r.job.cancel();
    }
    app.run(CMD, Value::Object(d.params))
}

/// A section's header: its disclosure chevron and name, and (with `reset`) a reset button at the
/// right. Returns whether the reset button was clicked.
fn section_header(ui: &mut egui::Ui, open: &mut bool, title: &str, reset: bool) -> bool {
    let t = Tokens::get(ui.ctx());
    let (row, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    let c = pos2(row.left() + 10.0, row.center().y);
    let s = 4.0;
    let stroke = Stroke::new(1.4, t.text_dim);
    let pts = if *open { [c + vec2(-s, -s * 0.5), c + vec2(0.0, s * 0.5), c + vec2(s, -s * 0.5)] } else { [c + vec2(-s * 0.5, -s), c + vec2(s * 0.5, 0.0), c + vec2(-s * 0.5, s)] };
    ui.painter().line_segment([pts[0], pts[1]], stroke);
    ui.painter().line_segment([pts[1], pts[2]], stroke);
    ui.painter().text(pos2(row.left() + 26.0, row.center().y), egui::Align2::LEFT_CENTER, tl!(title), FontId::proportional(13.0), t.text);
    let mut clicked_reset = false;
    if reset {
        let r = ERect::from_center_size(pos2(row.right() - 14.0, row.center().y), vec2(24.0, 24.0));
        let mut ru = ui.new_child(egui::UiBuilder::new().max_rect(r));
        clicked_reset = crate::icons::button(&mut ru, "undo-2", 24.0, false, tl!("Reset")).clicked();
    }
    if resp.clicked() && !clicked_reset {
        *open = !*open;
    }
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::CollapsingHeader, true, tl!(title)));
    clicked_reset
}

/// Joined buttons, one of which is chosen (Reference's segmented control). Returns the clicked
/// option's index.
fn segmented(ui: &mut egui::Ui, options: &[&str], selected: usize) -> Option<usize> {
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width();
    let (row, _) = ui.allocate_exact_size(vec2(w, 30.0), Sense::hover());
    let n = options.len().max(1) as f32;
    let cell = row.width() / n;
    let mut clicked = None;
    for (i, label) in options.iter().enumerate() {
        let r = ERect::from_min_size(pos2(row.left() + cell * i as f32, row.top()), vec2(cell, row.height()));
        let resp = ui.interact(r, ui.id().with(("caf-seg", i)), Sense::click());
        let on = i == selected;
        let fill = if on {
            t.accent_soft
        } else if resp.hovered() {
            t.hover
        } else {
            t.field
        };
        ui.painter().rect_filled(r.shrink(0.5), 0.0, fill);
        ui.painter().rect_stroke(r.shrink(0.5), 0.0, Stroke::new(1.0, if on { t.accent_border } else { t.field_border }), egui::StrokeKind::Inside);
        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, tl!(label), FontId::proportional(13.0), if on { t.accent_text } else { t.text });
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, on, tl!(label)));
        if resp.clicked() {
            clicked = Some(i);
        }
    }
    clicked
}

/// A labelled row: the label right-aligned in `label_w`, then `add`.
fn labelled<R>(ui: &mut egui::Ui, label: &str, label_w: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal(|ui| {
        widgets::form_label(ui, label, label_w);
        add(ui)
    })
    .inner
}

const SAMPLING: [(&str, &str); 3] = [("auto", "Auto"), ("rectangular", "Rectangular"), ("custom", "Custom")];
const COLOR_ADAPTATION: [(&str, &str); 4] = [("none", "None"), ("default", "Default"), ("high", "High"), ("veryHigh", "Very High")];
const ROTATION_ADAPTATION: [(&str, &str); 5] = [("none", "None"), ("low", "Low"), ("medium", "Medium"), ("high", "High"), ("full", "Full")];
const OUTPUT: [(&str, &str); 3] = [("current", "Current Layer"), ("new", "New Layer"), ("duplicate", "Duplicate Layer")];

/// The panel's sections.
fn panel(ui: &mut egui::Ui, d: &mut CafDialog) {
    let label_w = widgets::form_label_width(ui, ["Color Adaptation", "Rotation Adaptation", "Output To", "Opacity"], 90.0, 140.0);
    // Sampling Area Overlay.
    let mut open = d.open[0];
    section_header(ui, &mut open, "Sampling Area Overlay", false);
    d.open[0] = open;
    if open {
        ui.indent("caf-overlay", |ui| {
            let mut o = d.overlay;
            widgets::checkbox(ui, &mut o.show, tl!("Show Sampling Area"));
            widgets::slider_row(ui, tl!("Opacity"), &mut o.opacity, 0.0..=100.0, "%", None);
            labelled(ui, tl!("Color"), label_w, |ui| widgets::color_edit_button_srgb(ui, &mut o.color));
            labelled(ui, tl!("Indicates"), label_w, |ui| {
                widgets::dropdown(ui, "caf-indicates", &mut o.excluded, &[(false, "Sampling Area"), (true, "Excluded Area")], 150.0);
            });
            if o != d.overlay {
                d.overlay = o;
                d.tint_dirty = true;
            }
        });
    }
    ui.add(egui::Separator::default().spacing(4.0));
    // Sampling Area Options.
    let mut open = d.open[1];
    if section_header(ui, &mut open, "Sampling Area Options", true) {
        d.set("sampling", json!("auto"));
        d.params.remove("area");
        d.params.remove("sampleAllLayers");
    }
    d.open[1] = open;
    if open {
        let cur = d.str_param("sampling");
        let at = SAMPLING.iter().position(|(k, _)| *k == cur).unwrap_or(0);
        let labels: Vec<&str> = SAMPLING.iter().map(|(_, l)| *l).collect();
        if let Some(i) = segmented(ui, &labels, at)
            && let Some((k, _)) = SAMPLING.get(i)
        {
            choose_sampling(d, k);
        }
        if cur == "custom" {
            ui.label(egui::RichText::new(tl!("Drag in the image with the Sampling Area tool to set the area.")).small().color(Tokens::get(ui.ctx()).text_dim));
        }
        ui.add_space(4.0);
        // Only meaningful with other layers to sample.
        let many = d.doc.layers.len() > 1;
        ui.add_enabled_ui(many, |ui| {
            let mut all = d.flag("sampleAllLayers");
            if widgets::checkbox(ui, &mut all, tl!("Sample All Layers")).changed() {
                d.set("sampleAllLayers", json!(all));
            }
        });
    }
    ui.add(egui::Separator::default().spacing(4.0));
    // Fill Settings.
    let mut open = d.open[2];
    if section_header(ui, &mut open, "Fill Settings", true) {
        for (k, v) in [("colorAdaptation", json!("default")), ("rotationAdaptation", json!("none")), ("scale", json!(false)), ("mirror", json!(false))] {
            d.set(k, v);
        }
    }
    d.open[2] = open;
    if open {
        for (key, label, options) in [("colorAdaptation", "Color Adaptation", &COLOR_ADAPTATION[..]), ("rotationAdaptation", "Rotation Adaptation", &ROTATION_ADAPTATION[..])] {
            let mut cur = d.str_param(key);
            let opts: Vec<(String, &str)> = options.iter().map(|(k, l)| (k.to_string(), *l)).collect();
            if labelled(ui, tl!(label), label_w, |ui| widgets::dropdown(ui, &format!("caf-{key}"), &mut cur, &opts, 120.0)) {
                d.set(key, json!(cur));
            }
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 40.0;
            for (key, label) in [("scale", "Scale"), ("mirror", "Mirror")] {
                let mut b = d.flag(key);
                if widgets::checkbox(ui, &mut b, tl!(label)).changed() {
                    d.set(key, json!(b));
                }
            }
        });
    }
    ui.add(egui::Separator::default().spacing(4.0));
    // Output Settings.
    let mut open = d.open[3];
    section_header(ui, &mut open, "Output Settings", false);
    d.open[3] = open;
    if open {
        let mut cur = d.str_param("output");
        let opts: Vec<(String, &str)> = OUTPUT.iter().map(|(k, l)| (k.to_string(), *l)).collect();
        if labelled(ui, tl!("Output To"), label_w, |ui| widgets::dropdown(ui, "caf-output", &mut cur, &opts, 150.0)) {
            d.set("output", json!(cur));
        }
    }
}

/// Switch the sampling mode. Custom starts from the current sampling window (the command needs
/// an area), and the Sampling Area tool is picked to change it.
fn choose_sampling(d: &mut CafDialog, mode: &str) {
    d.set("sampling", json!(mode));
    if mode == "custom" {
        if !d.params.contains_key("area")
            && let Some((w, _)) = &d.sampling
        {
            d.set("area", json!([w.x0, w.y0, w.width(), w.height()]));
        }
        d.tool = Tool::Area;
    } else {
        d.params.remove("area");
    }
}

pub fn show(app: &mut PhotospliceApp, ctx: &egui::Context) {
    let Some(d) = app.caf.as_mut() else { return };
    d.poll(ctx);
    d.request(ctx);
    if d.tint_dirty {
        d.rebuild_tint(ctx);
    }
    if let Some(img) = d.proxy_img.take() {
        d.proxy = Some(ctx.load_texture("caf-document", img, egui::TextureOptions::LINEAR));
        d.outline = Some(ctx.load_texture("caf-selection", outline_image(&d.sel, d.pw, d.ph), egui::TextureOptions::NEAREST));
    }
    let t = Tokens::get(ctx);
    let rect = widgets::tool_dialog_rect(ctx, DIALOG_MAX);
    let mut action: Option<&str> = None;
    let modal = crate::dialogs::frame(ctx, egui::Id::new("content-aware-fill-dialog"), tl!("Content-Aware Fill"), crate::dialogs::FrameSize::Panels(rect.size()), |ui, size| {
        let Some(d) = app.caf.as_mut() else { return };
        let (body, _) = ui.allocate_exact_size(size.unwrap_or(rect.size()), Sense::hover());
        let painter = ui.painter().clone();
        // Tool strip.
        let strip = ERect::from_min_max(body.min, pos2(body.left() + STRIP_W, body.bottom()));
        painter.rect_filled(strip, 0.0, t.dock);
        let mut su = ui.new_child(egui::UiBuilder::new().max_rect(strip.shrink2(vec2(6.0, widgets::TOOL_PANEL_PAD.y))).layout(egui::Layout::top_down(egui::Align::Center)));
        for (tool, icon, tip) in [(Tool::Area, "square-dashed", tl!("Sampling Area Tool")), (Tool::Hand, "tools-hand", tl!("Hand Tool")), (Tool::Zoom, "tools-zoom", tl!("Zoom Tool"))] {
            if crate::icons::button(&mut su, icon, 28.0, d.tool == tool, tip).clicked() {
                d.tool = tool;
            }
        }
        // The document.
        let view = ERect::from_min_max(pos2(strip.right(), body.top()), pos2(body.right() - PANEL_W, body.bottom()));
        painter.rect_filled(view, 0.0, t.canvas);
        let fit = (view.width() / d.pw as f32).min(view.height() / d.ph as f32) * 0.95;
        let k = if d.zoom > 0.0 { d.zoom } else { fit };
        let img = ERect::from_center_size(view.center() + d.pan, vec2(d.pw as f32 * k, d.ph as f32 * k));
        let vp = ui.interact(view, egui::Id::new("caf-view"), Sense::click_and_drag());
        let clip = painter.with_clip_rect(view);
        let uv = ERect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0));
        widgets::checker(&clip, img, 8.0);
        if let Some(tex) = &d.proxy {
            clip.image(tex.id(), img, uv, Color32::WHITE);
        }
        if d.overlay.show
            && let Some(tex) = &d.tint
        {
            clip.image(tex.id(), img, uv, Color32::WHITE);
        }
        if let Some(tex) = &d.outline {
            clip.image(tex.id(), img, uv, Color32::WHITE);
        }
        let to_px = |p: Pos2| pos2((p.x - img.left()) / k, (p.y - img.top()) / k);
        match d.tool {
            Tool::Hand => {
                if vp.dragged() {
                    d.pan += vp.drag_delta();
                }
            }
            Tool::Zoom => {
                if vp.clicked() {
                    let out = ui.input(|i| i.modifiers.alt);
                    d.zoom = if out { (k / 1.5).max(0.05) } else { (k * 1.5).min(32.0) };
                }
            }
            Tool::Area => {
                if let Some(p) = vp.interact_pointer_pos() {
                    if vp.drag_started() {
                        d.drag = Some((to_px(p), to_px(p)));
                    } else if vp.dragged()
                        && let Some(g) = d.drag.as_mut()
                    {
                        g.1 = to_px(p);
                    }
                }
                if vp.drag_stopped()
                    && let Some((a, b)) = d.drag.take()
                {
                    let (x0, y0) = ((a.x.min(b.x) / d.scale).floor() as i32, (a.y.min(b.y) / d.scale).floor() as i32);
                    let (x1, y1) = ((a.x.max(b.x) / d.scale).ceil() as i32, (a.y.max(b.y) / d.scale).ceil() as i32);
                    if x1 - x0 >= 2 && y1 - y0 >= 2 {
                        d.set("sampling", json!("custom"));
                        d.set("area", json!([x0, y0, x1 - x0, y1 - y0]));
                    }
                }
                if let Some((a, b)) = d.drag {
                    let s = |q: Pos2| pos2(img.left() + q.x * k, img.top() + q.y * k);
                    clip.rect_stroke(ERect::from_two_pos(s(a), s(b)), 0.0, Stroke::new(1.5, t.accent), egui::StrokeKind::Middle);
                }
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            }
        }
        // Zoom readout, as the canvas's (relative to the document's own size).
        let zr = ERect::from_min_size(pos2(view.left() + 8.0, view.bottom() - 34.0), vec2(118.0, 26.0));
        painter.rect_filled(zr, t.radius_sm, Color32::from_black_alpha(200));
        let mut zu = ui.new_child(egui::UiBuilder::new().max_rect(zr.shrink2(vec2(6.0, 3.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
        if zu.small_button("−").clicked() {
            d.zoom = (k / 1.25).max(0.05);
        }
        zu.label(egui::RichText::new(format!("{:.0}%", k * d.scale * 100.0)).color(t.text).font(FontId::monospace(11.0)));
        if zu.small_button("+").clicked() {
            d.zoom = (k * 1.25).min(32.0);
        }
        // Right column: the Preview, then the panel, then the footer.
        let right = ERect::from_min_max(pos2(body.right() - PANEL_W, body.top()), body.max);
        painter.rect_filled(right, 0.0, t.dock);
        painter.line_segment([right.left_top(), right.left_bottom()], Stroke::new(1.0, t.separator));
        let pad = widgets::TOOL_PANEL_PAD;
        let head = ERect::from_min_size(right.min + pad, vec2(right.width() - 2.0 * pad.x, 18.0));
        painter.text(head.left_center(), egui::Align2::LEFT_CENTER, tl!("Preview"), crate::theme::semibold(13.0), t.text);
        let pv = ERect::from_min_size(pos2(head.left(), head.bottom() + 6.0), vec2(head.width(), PREVIEW_H));
        painter.rect_filled(pv, t.radius_sm, t.canvas);
        if let Some((area, tex)) = &d.preview {
            let s = (pv.width() / area.width().max(1) as f32).min(pv.height() / area.height().max(1) as f32);
            let r = ERect::from_center_size(pv.center(), vec2(area.width() as f32 * s, area.height() as f32 * s));
            widgets::checker(&painter.with_clip_rect(pv), r, 6.0);
            painter.with_clip_rect(pv).image(tex.id(), r, uv, Color32::WHITE);
        }
        if d.running.is_some() {
            painter.text(pv.right_bottom() + vec2(-8.0, -8.0), egui::Align2::RIGHT_BOTTOM, tl!("Computing…"), FontId::proportional(11.0), t.text_dim);
        } else if let Some(e) = &d.error {
            painter.text(pv.center(), egui::Align2::CENTER_CENTER, e, FontId::proportional(11.0), t.danger);
        }
        let foot_h = 56.0;
        let foot = ERect::from_min_max(pos2(right.left(), right.bottom() - foot_h), right.max);
        let panel_rect = ERect::from_min_max(pos2(right.left() + pad.x, pv.bottom() + 12.0), pos2(right.right() - pad.x, foot.top() - 6.0));
        painter.line_segment([pos2(right.left(), pv.bottom() + 6.0), pos2(right.right(), pv.bottom() + 6.0)], Stroke::new(1.0, t.separator));
        let mut pu = ui.new_child(egui::UiBuilder::new().max_rect(panel_rect));
        egui::ScrollArea::vertical().id_salt("caf-panel").show(&mut pu, |ui| panel(ui, d));
        painter.line_segment([foot.left_top(), foot.right_top()], Stroke::new(1.0, t.separator));
        let mut fu = ui.new_child(egui::UiBuilder::new().max_rect(foot.shrink2(pad)).layout(egui::Layout::left_to_right(egui::Align::Center)));
        if crate::icons::button(&mut fu, "undo-2", 28.0, false, tl!("Reset all settings")).clicked() {
            d.params = defaults();
            d.overlay = Overlay::default();
            d.tint_dirty = true;
        }
        fu.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let ok = widgets::DialogButton::new(widgets::ButtonRole::Default, tl!("OK"), 84.0);
            let cancel = widgets::DialogButton::new(widgets::ButtonRole::Cancel, tl!("Cancel"), 84.0);
            match widgets::dialog_buttons(ui, &[ok, cancel]) {
                Some(widgets::ButtonRole::Default) => action = Some("ok"),
                Some(_) => action = Some("cancel"),
                None => {}
            }
        });
    });
    if modal.should_close() {
        action = action.or(Some("cancel"));
    }
    match action {
        Some("ok") => {
            if let Err(e) = commit(app) {
                app.ui.status = e;
                app.ui.status_error = true;
            }
        }
        Some("cancel") => cancel(app),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_selection() -> (PhotospliceApp, egui::Context) {
        let mut app = PhotospliceApp::new(photosplice_engine::Session::new(), Default::default());
        let ctx = egui::Context::default();
        app.run("file.new", json!({"width": 160, "height": 120})).unwrap();
        app.run("edit.fill", json!({"color": "#4080c0"})).unwrap();
        app.run("select.rect", json!({"x": 60, "y": 40, "width": 30, "height": 24})).unwrap();
        (app, ctx)
    }

    /// The workspace opens on the selection, shows the command's sampling area and preview, and
    /// OK fills as the command does, onto a new layer by default (Reference's Output To).
    #[test]
    fn workspace_previews_and_commits_the_command() {
        let (mut app, ctx) = app_with_selection();
        let r = menu(&mut app, &ctx, CMD, &json!({})).unwrap().unwrap();
        assert_eq!(r["params"]["sampling"], "auto");
        assert_eq!(r["params"]["output"], "new");
        let r = menu(&mut app, &ctx, CMD, &json!({"ui": {"wait": true}})).unwrap().unwrap();
        assert!(r["error"].is_null(), "{r}");
        assert!(r["sampling"]["sampled"].as_u64().unwrap() > 0, "{r}");
        let preview = r["preview"].as_array().unwrap();
        assert!(preview[2].as_i64().unwrap() >= 30 && preview[3].as_i64().unwrap() >= 24, "the preview covers the selection: {r}");
        let before = app.session.active().unwrap().doc.layers.len();
        menu(&mut app, &ctx, CMD, &json!({"ui": {"commit": true}})).unwrap().unwrap();
        assert!(app.caf.is_none());
        assert_eq!(app.session.active().unwrap().doc.layers.len(), before + 1, "filled onto a new layer");
        assert!(app.session.undo(), "one history step");
    }

    /// Settings go to the command; Rectangular samples the whole window, Custom only its area;
    /// Cancel leaves the document alone.
    #[test]
    fn sampling_modes_and_cancel() {
        let (mut app, ctx) = app_with_selection();
        menu(&mut app, &ctx, CMD, &json!({})).unwrap().unwrap();
        let rect = menu(&mut app, &ctx, CMD, &json!({"ui": {"set": {"sampling": "rectangular"}, "wait": true}})).unwrap().unwrap();
        let custom = menu(&mut app, &ctx, CMD, &json!({"ui": {"set": {"sampling": "custom", "area": [0, 0, 40, 40]}, "wait": true}})).unwrap().unwrap();
        let (r, c) = (rect["sampling"]["sampled"].as_u64().unwrap(), custom["sampling"]["sampled"].as_u64().unwrap());
        assert!(c > 0 && c <= 1600 && c < r, "custom {c} of rectangular {r}");
        let r = menu(&mut app, &ctx, CMD, &json!({"ui": {"overlay": {"opacity": 250, "excluded": true}, "tool": "area"}})).unwrap().unwrap();
        assert_eq!(r["overlay"]["opacity"], 100.0);
        assert_eq!(r["tool"], "area");
        let rev = app.session.active().unwrap().history.past_len();
        let r = menu(&mut app, &ctx, CMD, &json!({"ui": {"cancel": true}})).unwrap().unwrap();
        assert_eq!(r["cancelled"], true);
        assert_eq!(app.session.active().unwrap().history.past_len(), rev);
    }

    /// Hostile settings are the command's to refuse: the preview reports the error, never panics,
    /// and OK reports it too.
    #[test]
    fn bad_settings_report_an_error() {
        let (mut app, ctx) = app_with_selection();
        menu(&mut app, &ctx, CMD, &json!({})).unwrap().unwrap();
        let r = menu(&mut app, &ctx, CMD, &json!({"ui": {"set": {"sampling": "nonsense", "output": 7}, "wait": true}})).unwrap().unwrap();
        assert!(r["error"].as_str().is_some_and(|e| !e.is_empty()), "{r}");
        assert!(menu(&mut app, &ctx, CMD, &json!({"ui": {"commit": true}})).unwrap().is_err());
        // Without a selection the menu item can't open it.
        let (mut app, ctx) = app_with_selection();
        app.run("select.deselect", json!({})).unwrap();
        assert!(menu(&mut app, &ctx, CMD, &json!({})).unwrap().is_err());
    }

    /// The selection outline marks the selection's edge only.
    #[test]
    fn outline_marks_only_the_edge() {
        let (w, h) = (10, 8);
        let sel: Vec<f32> = (0..w * h).map(|i| if (2..7).contains(&(i % w)) && (2..6).contains(&(i / w)) { 1.0 } else { 0.0 }).collect();
        let img = outline_image(&sel, w, h);
        let set = |x: usize, y: usize| img.pixels[y * w + x] != Color32::TRANSPARENT;
        assert!(set(2, 2) && set(6, 5) && set(4, 2));
        assert!(!set(4, 3) && !set(0, 0) && !set(8, 4), "inside and outside stay clear");
    }
}
