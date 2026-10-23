//! Filter › Camera Raw Filter…: a large dialog like Adobe Camera Raw's (preview on the left,
//! edit panels on the right: Basic, Curve, Detail, Color Mixer, Color Grading, Effects).
//!
//! The dialog is modal and opens in a window of its own (moved and resized like any window,
//! at most 1400 × 900 pt to begin with) while the main window takes no input. Where the platform
//! has no extra windows (the web build) it is drawn over the document in the dialogs' own frame.
//! The preview runs [`photosplice_algo::camera_raw::develop`] on a CPU proxy of the layer, sized
//! to the preview (`pixel_scale` keeps pixel radii true to the full image), recomputed when a
//! control changes, and shown through the document's display transform. OK runs
//! `filter.cameraRaw` with the non-default settings, so the result is one history step (or a
//! smart filter on a smart object) and exactly replayable. Ctrl+Z / Shift+Ctrl+Z step through the
//! dialog's own edits.
//!
//! Control channel: `ui.menu.invoke {"id":"filter.cameraRaw","params":{"ui":{"set":{…},
//! "autoWhiteBalance":true, "whiteBalanceAt":[u, v] (0..=1 of the image), "undo":true,
//! "redo":true, "commit":true | "cancel":true}}}`; the reply describes the dialog.

use std::sync::Arc;

use egui::{Color32, FontId, Pos2, Rect as ERect, Sense, Stroke, TextureHandle, pos2, vec2};
use photosplice_algo::camera_raw::{CameraRaw, Wheel, curve_lut};
use photosplice_doc::LayerId;
use photosplice_engine::display_color::CanvasDisplay;
use photosplice_geom::Rect;
use serde_json::{Value, json};

use crate::PhotospliceApp;
use crate::theme::Tokens;
use crate::widgets;

/// The preview's long side is at most this (physical pixels), and at least `MIN_PROXY_SIDE`.
const MAX_PROXY_SIDE: usize = 4096;
const MIN_PROXY_SIDE: usize = 256;
const PANEL_W: f32 = 330.0;
const BANDS: [&str; 8] = ["Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta"];
/// The largest side of the area the dialog works on (the engine's filter domain), and the most
/// pixels: anything bigger is refused rather than allocated.
const MAX_AREA_SIDE: i32 = 1_048_576;
const MAX_AREA_PIXELS: usize = 512_000_000;
/// How many edits Ctrl+Z can step back inside the dialog.
const RAW_UNDO_LIMIT: usize = 40;

pub struct CameraRawDialog {
    pub layer: LayerId,
    layer_name: String,
    pub params: CameraRaw,
    /// Proxy pixels (straight RGBA) and size.
    proxy: Vec<[f32; 4]>,
    /// The document selection at each proxy pixel (0..=1), when there is one: the command only
    /// changes what is selected, so the preview blends the same way.
    selection: Option<Vec<f32>>,
    pw: usize,
    ph: usize,
    full_w: usize,
    full_h: usize,
    float: bool,
    /// How the document is shown on the monitor: the preview goes through the same transform.
    display: Arc<CanvasDisplay>,
    tex: Option<TextureHandle>,
    before_tex: Option<TextureHandle>,
    dirty: bool,
    pub show_before: bool,
    mixer_tab: usize,
    pub render_ms: f64,
    /// The White Balance eyedropper is armed: a click on the preview neutralises that spot.
    pub wb_pick: bool,
    /// Opened on a camera file as it was opened: OK says Open, Cancel closes the file again.
    pub opened_file: Option<photosplice_doc::DocId>,
    /// The one expanded panel (an accordion), by its shown title; Basic to begin with.
    pub open_section: Option<String>,
    /// The developed preview's red, green and blue histograms (alpha-weighted, display levels).
    hist: Box<[[f32; 256]; 3]>,
    /// The histogram's Channel menu: 0 RGB, 1 Red, 2 Green, 3 Blue, 4 Colours (the three overlaid).
    pub hist_channel: usize,
    /// Settings before each finished edit, so Ctrl+Z works inside the dialog.
    undo: Vec<CameraRaw>,
    redo: Vec<CameraRaw>,
    /// Settings at pointer-down: one drag is one undo step.
    edit_origin: Option<CameraRaw>,
}

/// Camera file extensions (the raw formats File › Open reads).
const RAW_EXTS: &[&str] = &["dng", "cr2", "cr3", "nef", "nrw", "arw", "pef", "orf", "rw2", "raf"];

/// Whether `name` is a camera raw file, by extension.
pub fn is_raw_name(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, e)| RAW_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Should an interactive open show Camera Raw? `warnings` are the import's: only a raw developed
/// from its sensor data qualifies (not a preview fallback), and the preference can turn it off.
pub fn wants_open_dialog(app: &PhotospliceApp, warnings: &[String]) -> bool {
    app.session.prefs().raw_defaults.open_in_camera_raw && warnings.iter().any(|w| w.contains(photosplice_io::raw::DEVELOPED_NOTE))
}

/// `warnings` without the "developed with default settings" note: Camera Raw shows how the raw
/// is developed.
pub fn without_develop_note(warnings: &[String]) -> Vec<String> {
    warnings.iter().filter(|w| !w.contains(photosplice_io::raw::DEVELOPED_NOTE)).cloned().collect()
}

/// Queues Camera Raw on the active document, a raw file just opened (shown on the next frame,
/// see `open_pending`). The file's name, path and bytes aren't needed: the dialog works on the
/// developed document.
pub fn queue_open_dialog(app: &mut PhotospliceApp, _name: &str, _path: Option<&str>, _bytes: Option<&[u8]>) {
    if let Some(st) = app.session.active() {
        app.camera_raw_on_open = Some(st.doc.id);
    }
}

/// A finished background job of Camera Raw's own. Opening a raw applies its settings as an
/// ordinary Camera Raw step (reported like any filter), so there is none: always `false`.
pub fn on_redevelop_event(_app: &mut PhotospliceApp, _e: &photosplice_engine::jobs::JobEvent) -> bool {
    false
}

impl CameraRawDialog {
    pub fn describe(&self) -> Value {
        json!({"layer": self.layer.0, "params": serde_json::to_value(&self.params).unwrap_or(Value::Null), "proxy": [self.pw, self.ph], "renderMs": self.render_ms, "before": self.show_before, "openSection": self.open_section, "histogramChannel": self.hist_channel, "hasSelection": self.selection.is_some(), "canUndo": !self.undo.is_empty(), "canRedo": !self.redo.is_empty()})
    }

    /// "Camera Raw (file name)" while a camera file is being opened, "Camera Raw Filter (layer)"
    /// otherwise.
    fn heading(&self) -> String {
        if self.opened_file.is_some() {
            crate::i18n::fmt(tl!("Camera Raw ({name})"), &[("name", &self.layer_name)])
        } else {
            crate::i18n::fmt(tl!("Camera Raw Filter ({layer})"), &[("layer", &self.layer_name)])
        }
    }

    /// Params for the engine: only the settings that differ from the defaults.
    pub fn command_params(&self) -> Value {
        let full = serde_json::to_value(&self.params).unwrap_or(json!({}));
        let def = serde_json::to_value(CameraRaw::default()).unwrap_or(json!({}));
        let mut out = serde_json::Map::new();
        if let (Value::Object(f), Value::Object(d)) = (full, def) {
            for (k, v) in f {
                if k != "pixelScale" && d.get(&k) != Some(&v) {
                    out.insert(k, v);
                }
            }
        }
        Value::Object(out)
    }

    /// Sets Temperature / Tint so the proxy's colour around (`x`, `y`) — or, with `None`, the
    /// whole image's average (Auto) — comes out neutral.
    pub fn neutralise(&mut self, at: Option<(usize, usize)>) {
        let (mut sum, mut n) = ([0.0f32; 3], 0.0f32);
        for (i, q) in self.proxy.iter().enumerate() {
            let (x, y) = (i % self.pw.max(1), i / self.pw.max(1));
            if at.is_some_and(|(cx, cy)| x.abs_diff(cx) > 1 || y.abs_diff(cy) > 1) || q[3] <= 0.0 {
                continue;
            }
            for c in 0..3 {
                sum[c] += q[c] * q[3];
            }
            n += q[3];
        }
        if n > 0.0 {
            let (t, tn) = photosplice_algo::camera_raw::neutral_white_balance(sum.map(|v| v / n));
            self.params.temperature = t;
            self.params.tint = tn;
            self.dirty = true;
        }
    }

    /// Straight-alpha pixels as the monitor shows them: through the document's display
    /// transform (a linear document is encoded first), as the canvas does.
    fn image(px: &[[f32; 4]], w: usize, h: usize, display: &CanvasDisplay) -> egui::ColorImage {
        let buf = photosplice_compose::Buffer { rect: Rect::new(0, 0, w as i32, h as i32), px: px.to_vec() };
        let img = display.to_rgba8(&buf);
        if img.pixels.len() == w.saturating_mul(h).saturating_mul(4) {
            egui::ColorImage::from_rgba_unmultiplied([w, h], &img.pixels)
        } else {
            egui::ColorImage::new([w, h], vec![Color32::TRANSPARENT; w.saturating_mul(h)])
        }
    }

    fn render(&mut self, ctx: &egui::Context) {
        let t0 = crate::gpu_canvas::now_ms();
        let mut px = self.proxy.clone();
        let mut p = self.params.clone();
        p.pixel_scale = (self.pw as f32 / self.full_w.max(1) as f32).min(1.0);
        photosplice_algo::camera_raw::develop(&mut px, self.pw, self.ph, &p, self.float);
        // Only the selection changes, as the command does.
        if let Some(mask) = &self.selection {
            for ((out, original), coverage) in px.iter_mut().zip(&self.proxy).zip(mask) {
                for (out, original) in out.iter_mut().zip(original) {
                    *out = *original + (*out - *original) * coverage;
                }
            }
        }
        let img = Self::image(&px, self.pw, self.ph, &self.display);
        *self.hist = colour_histogram(&px);
        match &mut self.tex {
            Some(t) => t.set(img, egui::TextureOptions::LINEAR),
            None => self.tex = Some(ctx.load_texture("camera-raw-after", img, egui::TextureOptions::LINEAR)),
        }
        if self.before_tex.is_none() {
            self.before_tex = Some(ctx.load_texture("camera-raw-before", Self::image(&self.proxy, self.pw, self.ph, &self.display), egui::TextureOptions::LINEAR));
        }
        self.render_ms = crate::gpu_canvas::now_ms() - t0;
        self.dirty = false;
    }

    /// Keeps the settings before an edit for Ctrl+Z (the redo steps are dropped).
    fn remember(&mut self, before: CameraRaw) {
        if self.undo.len() >= RAW_UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.undo.push(before);
        self.redo.clear();
    }

    fn undo_edit(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else { return false };
        self.redo.push(std::mem::replace(&mut self.params, prev));
        self.dirty = true;
        self.edit_origin = None;
        true
    }

    fn redo_edit(&mut self) -> bool {
        let Some(next) = self.redo.pop() else { return false };
        self.undo.push(std::mem::replace(&mut self.params, next));
        self.dirty = true;
        self.edit_origin = None;
        true
    }
}

/// The proxy's long side for an image of `w × h` in a dialog of `dialog` points: what the
/// preview draws it at, in physical pixels, so it is shown 1:1 — neither magnified (soft) nor
/// shrunk without mipmaps (grainy).
fn preview_side(dialog: egui::Vec2, ppp: f32, w: usize, h: usize) -> usize {
    let (vw, vh) = ((dialog.x - PANEL_W - 32.0).max(64.0), (dialog.y - 30.0 - 48.0 - 32.0).max(64.0));
    let fit = (vw / w.max(1) as f32).min(vh / h.max(1) as f32);
    let side = (w.max(h) as f32 * fit * ppp).round() as usize;
    side.clamp(MIN_PROXY_SIDE, MAX_PROXY_SIDE).min(w.max(h).max(1))
}

/// Area-averaged proxy of `area` with long side `side`: every source pixel lands in exactly one
/// proxy pixel, averaged premultiplied so transparent pixels don't bleed their colour.
pub(crate) fn build_proxy(surf: &photosplice_raster::Surface, area: Rect, side: usize) -> (Vec<[f32; 4]>, usize, usize) {
    let (w, h) = (area.width().max(1) as usize, area.height().max(1) as usize);
    let k = (w.max(h) as f64 / side.max(1) as f64).max(1.0);
    let (pw, ph) = (((w as f64 / k).ceil() as usize).max(1), ((h as f64 / k).ceil() as usize).max(1));
    let mut proxy = vec![[0.0f32; 4]; pw * ph];
    let mut row = vec![[0.0f32; 4]; w];
    let mut acc = vec![[0.0f32; 5]; pw];
    let mut py_prev = 0usize;
    let flush = |acc: &mut Vec<[f32; 5]>, line: &mut [[f32; 4]]| {
        for (q, a) in line.iter_mut().zip(acc.iter_mut()) {
            *q = if a[3] > 0.0 { [a[0] / a[3], a[1] / a[3], a[2] / a[3], a[3] / a[4].max(1.0)] } else { [0.0; 4] };
            *a = [0.0; 5];
        }
    };
    for y in 0..h {
        let py = ((y as f64 / k) as usize).min(ph - 1);
        if py != py_prev {
            flush(&mut acc, &mut proxy[py_prev * pw..(py_prev + 1) * pw]);
            py_prev = py;
        }
        let sy = area.y0 + y as i32;
        surf.read_rgba_into(Rect::new(area.x0, sy, area.x1, sy + 1), &mut row);
        for (x, q) in row.iter().enumerate() {
            let a = &mut acc[((x as f64 / k) as usize).min(pw - 1)];
            for c in 0..3 {
                a[c] += q[c] * q[3];
            }
            a[3] += q[3];
            a[4] += 1.0;
        }
    }
    flush(&mut acc, &mut proxy[py_prev * pw..(py_prev + 1) * pw]);
    (proxy, pw, ph)
}

/// What the dialog works on: the engine's filter domain, the canvas and any of the layer's pixels
/// beyond it, so spatial stages (vignette, grain, local contrast) agree with the committed result.
/// Absurd sizes are refused rather than allocated.
fn preview_area(surf: &photosplice_raster::Surface, canvas: Rect) -> Result<Rect, String> {
    let area = canvas.union(&surf.content_bounds());
    let side = |a: i32, b: i32| b.checked_sub(a).filter(|n| (1..=MAX_AREA_SIDE).contains(n));
    let (Some(w), Some(h)) = (side(area.x0, area.x1), side(area.y0, area.y1)) else {
        return Err("Camera Raw preview bounds are too large".into());
    };
    if (w as usize).checked_mul(h as usize).is_none_or(|n| n > MAX_AREA_PIXELS) {
        return Err("Camera Raw preview bounds are too large".into());
    }
    Ok(area)
}

/// The document selection sampled at the centre of each proxy pixel of `area` (`pw × ph`).
fn proxy_selection(selection: Option<&photosplice_raster::Surface>, area: Rect, pw: usize, ph: usize) -> Option<Vec<f32>> {
    let mask = selection?;
    let (w, h) = (area.width().max(1), area.height().max(1));
    let (kx, ky) = (f64::from(w) / pw.max(1) as f64, f64::from(h) / ph.max(1) as f64);
    // Offsets inside the area: at most `MAX_AREA_SIDE`, so they fit an `i32`.
    let at = |i: usize, k: f64, len: u32| i32::try_from((((i as f64 + 0.5) * k) as u32).min(len - 1)).unwrap_or(0);
    Some(
        (0..ph)
            .flat_map(|y| (0..pw).map(move |x| (x, y)))
            .map(|(x, y)| mask.sample_channel(area.x0.saturating_add(at(x, kx, w)), area.y0.saturating_add(at(y, ky, h)), 0).clamp(0.0, 1.0))
            .collect(),
    )
}

/// Rebuilds the proxy when the preview's size on screen (`dialog` points at `ppp`) has changed
/// noticeably (window resized, moved to a display of another density).
fn refit_proxy(app: &mut PhotospliceApp, dialog: egui::Vec2, ppp: f32) {
    let Some(d) = app.camera_raw.as_ref() else { return };
    let want = preview_side(dialog, ppp, d.full_w, d.full_h);
    let have = d.pw.max(d.ph);
    if (want as f64 - have as f64).abs() < have as f64 * 0.15 {
        return;
    }
    let layer = d.layer;
    let Ok((l, surf, _)) = crate::distort_ui::active_pixels(app) else { return };
    let Some(st) = app.session.active() else { return };
    if l != layer {
        return;
    }
    let Ok(area) = preview_area(&surf, st.doc.bounds()) else { return };
    let (proxy, pw, ph) = build_proxy(&surf, area, want);
    let selection = proxy_selection(st.doc.selection.as_ref(), area, pw, ph);
    if let Some(d) = app.camera_raw.as_mut() {
        (d.proxy, d.pw, d.ph, d.selection) = (proxy, pw, ph, selection);
        d.before_tex = None;
        d.dirty = true;
    }
}

/// Opens the dialog on the active layer.
pub fn open(app: &mut PhotospliceApp, ctx: &egui::Context) -> Result<(), String> {
    photosplice_engine::commands::find("filter.cameraRaw").map(|c| (c.enabled)(&app.session)).unwrap_or(Err("unknown command".into()))?;
    let (layer, surf, _) = crate::distort_ui::active_pixels(app)?;
    let st = app.session.active().ok_or("no document")?;
    let name = st.doc.layer(layer).map(|l| l.name.clone()).unwrap_or_default();
    let area = preview_area(&surf, st.doc.bounds())?;
    let (w, h) = (area.width() as usize, area.height() as usize);
    let dialog = widgets::tool_dialog_rect(ctx, DIALOG_MAX).size();
    let (proxy, pw, ph) = build_proxy(&surf, area, preview_side(dialog, ctx.pixels_per_point(), w, h));
    let selection = proxy_selection(st.doc.selection.as_ref(), area, pw, ph);
    let float = surf.format().sample == photosplice_color::SampleType::F32;
    let display = app.session.color.canvas_display(&st.doc).map_err(|e| e.to_string())?;
    // Whites and Blacks act on light: decode the document's own transfer curve, as the command does.
    let params = CameraRaw { encoding_gamma: photosplice_engine::lens_cmds::encoding_gamma(&st.doc), ..CameraRaw::default() };
    let mut d = CameraRawDialog {
        layer,
        layer_name: name,
        params,
        proxy,
        selection,
        pw,
        ph,
        full_w: w,
        full_h: h,
        float,
        display,
        tex: None,
        before_tex: None,
        dirty: true,
        show_before: false,
        mixer_tab: 1,
        render_ms: 0.0,
        wb_pick: false,
        opened_file: None,
        open_section: Some(tl!("Basic").to_string()),
        hist: Box::new([[0.0; 256]; 3]),
        hist_channel: HIST_COLOURS,
        undo: Vec::new(),
        redo: Vec::new(),
        edit_origin: None,
    };
    d.render(ctx);
    app.camera_raw = Some(d);
    Ok(())
}

/// The control channel's `ui` properties, and which of them take a boolean.
const UI_KEYS: &[&str] = &["set", "openSection", "autoWhiteBalance", "whiteBalanceAt", "before", "commit", "cancel", "undo", "redo"];
const UI_FLAGS: &[&str] = &["autoWhiteBalance", "before", "commit", "cancel", "undo", "redo"];

/// Menu / control-channel entry point. `None` when the call isn't for this dialog.
pub fn menu(app: &mut PhotospliceApp, ctx: &egui::Context, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if id != "filter.cameraRaw" {
        return None;
    }
    let Some(fields) = params.as_object() else {
        return Some(Err("Camera Raw params must be an object".into()));
    };
    if fields.is_empty() {
        return Some(open(app, ctx).map(|_| app.camera_raw.as_ref().map(|d| d.describe()).unwrap_or(Value::Null)));
    }
    let ui = params.get("ui")?;
    let Some(fields) = ui.as_object() else {
        return Some(Err("Camera Raw ui must be an object".into()));
    };
    for (key, value) in fields {
        if !UI_KEYS.contains(&key.as_str()) {
            return Some(Err(format!("unknown Camera Raw ui property {key}")));
        }
        if UI_FLAGS.contains(&key.as_str()) && !value.is_boolean() {
            return Some(Err(format!("Camera Raw {key} must be a boolean")));
        }
    }
    if ui.get("commit").and_then(Value::as_bool) == Some(true) && ui.get("cancel").and_then(Value::as_bool) == Some(true) {
        return Some(Err("Camera Raw cannot commit and cancel together".into()));
    }
    let was_closed = app.camera_raw.is_none();
    if was_closed && let Err(e) = open(app, ctx) {
        return Some(Err(e));
    }
    let result = menu_update(app, ctx, ui);
    // A request that opened the dialog and then failed leaves nothing open.
    if was_closed && result.as_ref().is_some_and(Result::is_err) {
        app.camera_raw = None;
    }
    result
}

fn menu_update(app: &mut PhotospliceApp, ctx: &egui::Context, ui: &Value) -> Option<Result<Value, String>> {
    let d = app.camera_raw.as_mut()?;
    let before = d.params.clone();
    if let Some(set) = ui.get("set") {
        let Some(set) = set.as_object() else {
            return Some(Err("bad Camera Raw settings: expected an object".into()));
        };
        let mut cur = serde_json::to_value(&d.params).unwrap_or(json!({}));
        if let Value::Object(c) = &mut cur {
            for (k, v) in set {
                if !c.contains_key(k) {
                    return Some(Err(format!("unknown Camera Raw setting {k}")));
                }
                c.insert(k.clone(), v.clone());
            }
        }
        let next = match serde_json::from_value::<CameraRaw>(cur) {
            Ok(p) => p,
            Err(e) => return Some(Err(format!("bad Camera Raw settings: {e}"))),
        };
        if let Err(e) = next.validate() {
            return Some(Err(format!("bad Camera Raw settings: {e}")));
        }
        d.dirty |= d.params != next;
        d.params = next;
    }
    if let Some(s) = ui.get("openSection") {
        d.open_section = s.as_str().map(str::to_string);
    }
    if ui.get("autoWhiteBalance").and_then(Value::as_bool) == Some(true) {
        d.neutralise(None);
    }
    if let Some(at) = ui.get("whiteBalanceAt") {
        let uv = at.as_array().and_then(|a| match a.as_slice() {
            [u, v] => Some((u.as_f64()?, v.as_f64()?)),
            _ => None,
        });
        let Some((u, v)) = uv.filter(|(u, v)| u.is_finite() && v.is_finite()) else {
            d.params = before;
            return Some(Err("whiteBalanceAt is [u, v] in 0..=1".into()));
        };
        let x = ((u.clamp(0.0, 1.0) * d.pw as f64) as usize).min(d.pw.saturating_sub(1));
        let y = ((v.clamp(0.0, 1.0) * d.ph as f64) as usize).min(d.ph.saturating_sub(1));
        d.neutralise(Some((x, y)));
    }
    if d.params != before {
        d.remember(before);
    }
    if ui.get("undo").and_then(Value::as_bool) == Some(true) {
        d.undo_edit();
    }
    if ui.get("redo").and_then(Value::as_bool) == Some(true) {
        d.redo_edit();
    }
    if d.dirty {
        d.render(ctx);
    }
    if let Some(b) = ui.get("before").and_then(Value::as_bool) {
        d.show_before = b;
    }
    if ui.get("cancel").and_then(Value::as_bool) == Some(true) {
        cancel(app, ctx);
        return Some(Ok(json!({"cancelled": true})));
    }
    if ui.get("commit").and_then(Value::as_bool) == Some(true) {
        return Some(commit(app));
    }
    Some(Ok(app.camera_raw.as_ref().map(|d| d.describe()).unwrap_or(Value::Null)))
}

/// OK: the settings as one `filter.cameraRaw` step. The dialog stays open when the command fails.
fn commit(app: &mut PhotospliceApp) -> Result<Value, String> {
    let d = app.camera_raw.as_ref().ok_or(tl!("Camera Raw isn't open"))?;
    let result = if d.params.is_identity() {
        json!({"unchanged": true})
    } else {
        let mut p = d.command_params();
        p["layer"] = json!(d.layer.0);
        app.run("filter.cameraRaw", p)?
    };
    app.camera_raw = None;
    Ok(result)
}

fn temp_stops() -> Vec<Color32> {
    vec![Color32::from_rgb(70, 120, 230), Color32::from_rgb(200, 200, 200), Color32::from_rgb(235, 200, 60)]
}
fn tint_stops() -> Vec<Color32> {
    vec![Color32::from_rgb(70, 190, 80), Color32::from_rgb(200, 200, 200), Color32::from_rgb(210, 80, 200)]
}

/// One labelled slider; marks the dialog dirty when it moves. Double-click resets it to 0.
fn row(ui: &mut egui::Ui, dirty: &mut bool, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, grad: Option<&[Color32]>) {
    row_reset(ui, dirty, label, v, range, grad, 0.0);
}

/// [`row`] for a slider whose rest value isn't 0.
fn row_reset(ui: &mut egui::Ui, dirty: &mut bool, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, grad: Option<&[Color32]>, rest: f32) {
    let r = widgets::slider_row(ui, label, v, range, "", grad);
    if r.changed() {
        *dirty = true;
    }
    if r.double_clicked() {
        *v = rest;
        *dirty = true;
    }
}

/// One panel of the accordion: only the section named by `open` is expanded; clicking a header
/// opens it and closes the rest (or closes it, when it was the open one).
fn section(ui: &mut egui::Ui, open: &mut Option<String>, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    let is_open = open.as_deref() == Some(title);
    let r = egui::CollapsingHeader::new(egui::RichText::new(tl!(&title)).font(FontId::proportional(13.0)))
        .id_salt(("cr-section", title))
        .open(Some(is_open))
        .show(ui, body);
    if r.header_response.clicked() {
        *open = if is_open { None } else { Some(title.to_string()) };
    }
}

fn wheel(ui: &mut egui::Ui, dirty: &mut bool, title: &str, w: &mut Wheel) {
    widgets::section_label(ui, title);
    let hs = widgets::hue_stops();
    row(ui, dirty, tl!("Hue"), &mut w.hue, 0.0..=360.0, Some(&hs));
    row(ui, dirty, tl!("Saturation"), &mut w.sat, 0.0..=100.0, None);
    row(ui, dirty, tl!("Luminance"), &mut w.lum, -100.0..=100.0, None);
}

/// Small point-curve editor (master channel): click to add, drag to move, right-click to delete.
fn curve_editor(ui: &mut egui::Ui, p: &mut CameraRaw, dirty: &mut bool) {
    let t = Tokens::get(ui.ctx());
    let side = ui.available_width().min(260.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(side, side), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, t.field);
    for i in 1..4 {
        let f = i as f32 / 4.0;
        painter.line_segment([pos2(rect.left() + f * side, rect.top()), pos2(rect.left() + f * side, rect.bottom())], Stroke::new(1.0, t.separator));
        painter.line_segment([pos2(rect.left(), rect.top() + f * side), pos2(rect.right(), rect.top() + f * side)], Stroke::new(1.0, t.separator));
    }
    let to_screen = |x: f32, y: f32| pos2(rect.left() + x / 255.0 * side, rect.bottom() - y / 255.0 * side);
    let from_screen = |q: Pos2| [((q.x - rect.left()) / side * 255.0).clamp(0.0, 255.0), ((rect.bottom() - q.y) / side * 255.0).clamp(0.0, 255.0)];
    if p.point_curve.len() < 2 {
        p.point_curve = vec![[0.0, 0.0], [255.0, 255.0]];
    }
    // Interaction.
    let drag_id = ui.id().with("cr-curve-drag");
    // Stored as `Option<usize>`, so read back as one: reading it as `usize` found nothing, and
    // the point stopped following the pointer after the drag's first frame.
    let mut dragging: Option<usize> = ui.data(|d| d.get_temp::<Option<usize>>(drag_id)).flatten();
    if let Some(pos) = resp.interact_pointer_pos() {
        let q = from_screen(pos);
        if resp.drag_started() || resp.clicked() {
            // Where the press started, not where the pointer is once it has moved far enough to
            // count as a drag: grabbing a point mustn't add a new one beside it.
            let at = ui.input(|i| i.pointer.press_origin()).filter(|o| rect.contains(*o)).unwrap_or(pos);
            let near = p.point_curve.iter().position(|c| (to_screen(c[0], c[1]) - at).length() < 8.0);
            if resp.secondary_clicked() {
                if let Some(i) = near.filter(|i| *i != 0 && *i + 1 != p.point_curve.len()) {
                    p.point_curve.remove(i);
                    *dirty = true;
                }
            } else {
                let q = from_screen(at);
                dragging = Some(near.unwrap_or_else(|| {
                    p.point_curve.push(q);
                    p.point_curve.sort_by(|a, b| a[0].total_cmp(&b[0]));
                    *dirty = true;
                    p.point_curve.iter().position(|c| *c == q).unwrap_or(0)
                }));
            }
        }
        if resp.dragged()
            && let Some(i) = dragging
        {
            // The curve can change under a drag (Ctrl+Z, the control channel): a point that is
            // gone is no longer dragged.
            let lo = match i.checked_sub(1) {
                Some(prev) => p.point_curve.get(prev).map_or(0.0, |c| c[0] + 1.0),
                None => 0.0,
            };
            let hi = p.point_curve.get(i + 1).map_or(255.0, |c| c[0] - 1.0);
            match p.point_curve.get_mut(i) {
                Some(pt) => {
                    *pt = [q[0].clamp(lo, hi.max(lo)), q[1]];
                    *dirty = true;
                }
                None => dragging = None,
            }
        }
    }
    if resp.drag_stopped() {
        dragging = None;
    }
    ui.data_mut(|d| d.insert_temp(drag_id, dragging));
    // Curve (point curve after the parametric one).
    let lut = curve_lut(&p.point_curve, 256);
    let pts: Vec<Pos2> = (0..256).map(|i| to_screen(i as f32, lut[i] * 255.0)).collect();
    painter.add(egui::Shape::line(pts, Stroke::new(1.5, t.text)));
    for c in &p.point_curve {
        painter.circle_stroke(to_screen(c[0], c[1]), 4.0, Stroke::new(1.5, t.accent));
    }
}

/// The dialog's largest size; smaller windows get a smaller one ([`widgets::tool_dialog_rect`]).
const DIALOG_MAX: egui::Vec2 = vec2(1400.0, 900.0);

pub fn show(app: &mut PhotospliceApp, ctx: &egui::Context) {
    open_pending(app, ctx);
    let Some(d) = app.camera_raw.as_ref() else { return };
    let heading = d.heading();
    let rect = widgets::tool_dialog_rect(ctx, DIALOG_MAX);
    // A window of its own (title bar, moved and resized like any window), like Adobe Camera Raw;
    // the dialogs' own frame over the document where the platform has no extra windows (the web
    // build). Modal either way, as in Reference: the main window takes no input meanwhile.
    let mut action = None;
    let modal = crate::dialogs::frame(ctx, egui::Id::new("camera-raw-dialog"), &heading, crate::dialogs::FrameSize::Panels(rect.size()), |ui, size| {
        action = body(app, ui, size.unwrap_or(rect.size()));
    });
    let action = keys(app, ctx).or(action).or_else(|| modal.should_close().then_some("cancel"));
    finish(app, ctx, action);
}

/// The dialog's keys, read where it is shown: Y toggles Before, Ctrl+Z / Shift+Ctrl+Z step
/// through its own edits, Esc cancels.
fn keys(app: &mut PhotospliceApp, ctx: &egui::Context) -> Option<&'static str> {
    let d = app.camera_raw.as_mut()?;
    let cmd_shift = egui::Modifiers::COMMAND | egui::Modifiers::SHIFT;
    if ctx.input_mut(|i| i.consume_key(cmd_shift, egui::Key::Z)) {
        d.redo_edit();
    } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z)) {
        d.undo_edit();
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Y)) {
        d.show_before = !d.show_before;
    }
    ctx.input(|i| i.key_pressed(egui::Key::Escape)).then_some("cancel")
}

/// OK or Cancel, from the buttons, the keys or the window's close box.
fn finish(app: &mut PhotospliceApp, ctx: &egui::Context, action: Option<&str>) {
    match action {
        Some("ok") => {
            if let Err(e) = commit(app) {
                app.ui.status = e;
                app.ui.status_error = true;
            }
        }
        Some("cancel") => cancel(app, ctx),
        _ => {}
    }
}

/// The dialog's contents in `size` points of `ui`: the preview, the panels and the footer.
/// Returns "ok" or "cancel" when a footer button was clicked.
fn body(app: &mut PhotospliceApp, ui: &mut egui::Ui, size: egui::Vec2) -> Option<&'static str> {
    let ctx = ui.ctx().clone();
    refit_proxy(app, size, ctx.pixels_per_point());
    // The document's display transform changed (monitor profile, Color Settings): show it.
    let current = app.session.active().and_then(|st| app.session.color.canvas_display(&st.doc).ok());
    let d = app.camera_raw.as_mut()?;
    if let Some(display) = current
        && display.key != d.display.key
    {
        d.display = display;
        d.before_tex = None;
        d.dirty = true;
    }
    // One drag (or click) is one undo step: the settings at pointer-down are kept until release.
    let pointer = ctx.input(|i| i.pointer.primary_down());
    if pointer && d.edit_origin.is_none() {
        d.edit_origin = Some(d.params.clone());
    }
    if d.dirty {
        d.render(&ctx);
    }
    let t = Tokens::get(&ctx);
    let mut return_action = None;
    let (full, _) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter().clone();
    let footer_h = widgets::TOOL_FOOTER_H;
    let body = ERect::from_min_max(full.min, pos2(full.right(), full.bottom() - footer_h));
    // Preview.
    let view = ERect::from_min_max(body.min, pos2(body.right() - PANEL_W, body.bottom())).shrink(16.0);
    painter.rect_filled(view, 0.0, t.canvas);
    let tex = if d.show_before { d.before_tex.as_ref() } else { d.tex.as_ref() };
    if let Some(tex) = tex {
        let s = (view.width() / d.pw as f32).min(view.height() / d.ph as f32);
        let r = ERect::from_center_size(view.center(), vec2(d.pw as f32 * s, d.ph as f32 * s));
        widgets::checker(&painter, r, 8.0);
        painter.image(tex.id(), r, ERect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        // White Balance eyedropper: click a neutral spot.
        if d.wb_pick
            && let Some(pos) = ui.ctx().pointer_hover_pos().filter(|p| r.contains(*p))
        {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            if ui.ctx().input(|i| i.pointer.primary_clicked()) {
                let x = (((pos.x - r.left()) / s) as usize).min(d.pw.saturating_sub(1));
                let y = (((pos.y - r.top()) / s) as usize).min(d.ph.saturating_sub(1));
                d.neutralise(Some((x, y)));
            }
        }
    }
    // Panels.
    let right = ERect::from_min_max(pos2(body.right() - PANEL_W, body.top()), body.max);
    painter.rect_filled(right, 0.0, t.dock);
    painter.line_segment([right.left_top(), right.left_bottom()], Stroke::new(1.0, t.separator));
    let inner = right.shrink2(widgets::TOOL_PANEL_PAD);
    // Histogram at the top, the panels scrolling below it.
    let head = ERect::from_min_size(inner.min, vec2(inner.width(), HIST_HEADER));
    let mut hu = ui.new_child(egui::UiBuilder::new().max_rect(head));
    histogram(&mut hu, d);
    painter.line_segment([pos2(right.left(), head.bottom() + 6.0), pos2(right.right(), head.bottom() + 6.0)], Stroke::new(1.0, t.separator));
    let mut props = ui.new_child(egui::UiBuilder::new().max_rect(ERect::from_min_max(pos2(inner.left(), head.bottom() + 12.0), inner.max)));
    let mut dirty = false;
    let (mut pick, mut auto) = (d.wb_pick, false);
    let mut open = d.open_section.clone();
    egui::ScrollArea::vertical().id_salt("camera-raw-props").show(&mut props, |ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        let p = &mut d.params;
        let (ts, tn) = (temp_stops(), tint_stops());
        section(ui, &mut open, tl!("Basic"), |ui| {
            ui.horizontal(|ui| {
                widgets::section_label(ui, tl!("White Balance: As Shot"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if crate::icons::button(ui, "tools-eyedropper", 22.0, pick, tl!("Eyedropper Tool")).clicked() {
                        pick = !pick;
                    }
                    auto = ui.small_button(tl!("Auto")).clicked();
                });
            });
            row(ui, &mut dirty, tl!("Temperature"), &mut p.temperature, -100.0..=100.0, Some(&ts));
            row(ui, &mut dirty, tl!("Tint"), &mut p.tint, -100.0..=100.0, Some(&tn));
            row(ui, &mut dirty, tl!("Exposure"), &mut p.exposure, -5.0..=5.0, None);
            row(ui, &mut dirty, tl!("Contrast"), &mut p.contrast, -100.0..=100.0, None);
            row(ui, &mut dirty, tl!("Highlights"), &mut p.highlights, -100.0..=100.0, None);
            row(ui, &mut dirty, tl!("Shadows"), &mut p.shadows, -100.0..=100.0, None);
            row(ui, &mut dirty, tl!("Whites"), &mut p.whites, -100.0..=100.0, None);
            row(ui, &mut dirty, tl!("Blacks"), &mut p.blacks, -100.0..=100.0, None);
            widgets::hairline(ui);
            row(ui, &mut dirty, tl!("Texture"), &mut p.texture, -100.0..=100.0, None);
            row(ui, &mut dirty, tl!("Clarity"), &mut p.clarity, -100.0..=100.0, None);
            row(ui, &mut dirty, tl!("Dehaze"), &mut p.dehaze, -100.0..=100.0, None);
            widgets::hairline(ui);
            row(ui, &mut dirty, tl!("Vibrance"), &mut p.vibrance, -100.0..=100.0, None);
            row(ui, &mut dirty, tl!("Saturation"), &mut p.saturation, -100.0..=100.0, None);
        });
        section(ui, &mut open, tl!("Curve"), |ui| {
            curve_editor(ui, p, &mut dirty);
            row(ui, &mut dirty, tl!("Highlights"), &mut p.curve_highlights, -100.0..=100.0, None);
            row(ui, &mut dirty, tl!("Lights"), &mut p.curve_lights, -100.0..=100.0, None);
            row(ui, &mut dirty, tl!("Darks"), &mut p.curve_darks, -100.0..=100.0, None);
            row(ui, &mut dirty, tl!("Shadows"), &mut p.curve_shadows, -100.0..=100.0, None);
        });
        section(ui, &mut open, tl!("Detail"), |ui| {
            widgets::section_label(ui, tl!("Sharpening"));
            row(ui, &mut dirty, tl!("Amount"), &mut p.sharpen_amount, 0.0..=150.0, None);
            row(ui, &mut dirty, tl!("Radius"), &mut p.sharpen_radius, 0.5..=3.0, None);
            row(ui, &mut dirty, tl!("Detail"), &mut p.sharpen_detail, 0.0..=100.0, None);
            row(ui, &mut dirty, tl!("Masking"), &mut p.sharpen_masking, 0.0..=100.0, None);
            widgets::section_label(ui, tl!("Noise Reduction"));
            row(ui, &mut dirty, tl!("Luminance"), &mut p.noise_luminance, 0.0..=100.0, None);
            row(ui, &mut dirty, tl!("Luminance Detail"), &mut p.noise_luminance_detail, 0.0..=100.0, None);
            row(ui, &mut dirty, tl!("Color"), &mut p.noise_color, 0.0..=100.0, None);
            row(ui, &mut dirty, tl!("Color Detail"), &mut p.noise_color_detail, 0.0..=100.0, None);
        });
        section(ui, &mut open, tl!("Color Mixer"), |ui| {
            ui.horizontal(|ui| {
                for (i, name) in [tl!("Hue"), tl!("Saturation"), tl!("Luminance")].iter().enumerate() {
                    if widgets::pill_tab(ui, name, d.mixer_tab == i).clicked() {
                        d.mixer_tab = i;
                    }
                }
            });
            let arr = match d.mixer_tab {
                0 => &mut p.hsl_hue,
                1 => &mut p.hsl_sat,
                _ => &mut p.hsl_lum,
            };
            for (k, name) in BANDS.iter().enumerate() {
                let c = photosplice_algo::camera_raw::HSL_BANDS[k];
                let base = hue_color(c);
                let grad = [Color32::from_gray(128), base];
                row(ui, &mut dirty, name, &mut arr[k], -100.0..=100.0, Some(&grad));
            }
        });
        section(ui, &mut open, tl!("Color Grading"), |ui| {
            wheel(ui, &mut dirty, tl!("Shadows"), &mut p.grade_shadows);
            wheel(ui, &mut dirty, tl!("Midtones"), &mut p.grade_midtones);
            wheel(ui, &mut dirty, tl!("Highlights"), &mut p.grade_highlights);
            wheel(ui, &mut dirty, tl!("Global"), &mut p.grade_global);
            row(ui, &mut dirty, tl!("Blending"), &mut p.grade_blending, 0.0..=100.0, None);
            row(ui, &mut dirty, tl!("Balance"), &mut p.grade_balance, -100.0..=100.0, None);
        });
        section(ui, &mut open, tl!("Optics"), |ui| {
            row(ui, &mut dirty, tl!("Distortion"), &mut p.optics_distortion, -100.0..=100.0, None);
            row(ui, &mut dirty, tl!("Vignetting"), &mut p.optics_vignetting, -100.0..=100.0, None);
        });
        section(ui, &mut open, tl!("Geometry"), |ui| {
            row(ui, &mut dirty, tl!("Vertical"), &mut p.geometry_vertical, -100.0..=100.0, None);
            row(ui, &mut dirty, tl!("Horizontal"), &mut p.geometry_horizontal, -100.0..=100.0, None);
            row(ui, &mut dirty, tl!("Rotate"), &mut p.geometry_rotate, -10.0..=10.0, None);
            row_reset(ui, &mut dirty, tl!("Scale"), &mut p.geometry_scale, 50.0..=150.0, None, 100.0);
        });
        section(ui, &mut open, tl!("Effects"), |ui| {
            widgets::section_label(ui, tl!("Grain"));
            row(ui, &mut dirty, tl!("Amount"), &mut p.grain_amount, 0.0..=100.0, None);
            row(ui, &mut dirty, tl!("Size"), &mut p.grain_size, 0.0..=100.0, None);
            row(ui, &mut dirty, tl!("Roughness"), &mut p.grain_roughness, 0.0..=100.0, None);
            widgets::section_label(ui, tl!("Vignetting"));
            let mut style = p.vignette_style.clone();
            if widgets::dropdown(
                ui,
                "cr-vig-style",
                &mut style,
                &[
                    ("highlightPriority".to_string(), tl!("Highlight Priority")),
                    ("colorPriority".to_string(), tl!("Color Priority")),
                    ("paintOverlay".to_string(), tl!("Paint Overlay")),
                ],
                200.0,
            ) {
                p.vignette_style = style;
                dirty = true;
            }
            row(ui, &mut dirty, tl!("Amount"), &mut p.vignette_amount, -100.0..=100.0, None);
            row(ui, &mut dirty, tl!("Midpoint"), &mut p.vignette_midpoint, 0.0..=100.0, None);
            row(ui, &mut dirty, tl!("Roundness"), &mut p.vignette_roundness, -100.0..=100.0, None);
            row(ui, &mut dirty, tl!("Feather"), &mut p.vignette_feather, 0.0..=100.0, None);
            row(ui, &mut dirty, tl!("Highlights"), &mut p.vignette_highlights, 0.0..=100.0, None);
        });
        section(ui, &mut open, tl!("Calibration"), |ui| {
            widgets::section_label(ui, tl!("Shadows"));
            let tint = tint_stops();
            row(ui, &mut dirty, tl!("Tint"), &mut p.calibration_shadow_tint, -100.0..=100.0, Some(&tint));
            for (title, hue, (h, s)) in [
                (tl!("Red Primary"), 0.0, (&mut p.calibration_red_hue, &mut p.calibration_red_saturation)),
                (tl!("Green Primary"), 120.0, (&mut p.calibration_green_hue, &mut p.calibration_green_saturation)),
                (tl!("Blue Primary"), 240.0, (&mut p.calibration_blue_hue, &mut p.calibration_blue_saturation)),
            ] {
                widgets::section_label(ui, title);
                let hues = [hue_color(hue - 30.0), hue_color(hue), hue_color(hue + 30.0)];
                row(ui, &mut dirty, tl!("Hue"), h, -100.0..=100.0, Some(&hues));
                let sats = [Color32::from_gray(128), hue_color(hue)];
                row(ui, &mut dirty, tl!("Saturation"), s, -100.0..=100.0, Some(&sats));
            }
        });
    });
    d.dirty |= dirty;
    d.wb_pick = pick;
    d.open_section = open;
    if auto {
        d.neutralise(None);
    }
    // Footer.
    let foot = ERect::from_min_max(pos2(full.left(), full.bottom() - footer_h), full.max);
    painter.rect_filled(foot, 0.0, t.dock);
    painter.line_segment([foot.left_top(), foot.right_top()], Stroke::new(1.0, t.separator));
    let mut fu = ui.new_child(
        egui::UiBuilder::new().max_rect(foot.shrink2(vec2(widgets::TOOL_FOOTER_INSET, 0.0))).layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    fu.spacing_mut().item_spacing.x = 10.0;
    // Opening a raw file says Open, as Photoshop does.
    let ok = if d.opened_file.is_some() { tl!("Open") } else { tl!("OK") };
    if let Some(role) = widgets::dialog_buttons(
        &mut fu,
        &[widgets::DialogButton::new(widgets::ButtonRole::Default, ok, 90.0), widgets::DialogButton::new(widgets::ButtonRole::Cancel, tl!("Cancel"), 90.0)],
    ) {
        return_action = Some(if role == widgets::ButtonRole::Default { "ok" } else { "cancel" });
    }
    // The view toggles at the other end, clear of OK and Cancel.
    let mut vu = ui.new_child(
        egui::UiBuilder::new().max_rect(foot.shrink2(vec2(widgets::TOOL_FOOTER_INSET, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    vu.spacing_mut().item_spacing.x = 14.0;
    widgets::checkbox(&mut vu, &mut d.show_before, tl!("Before (Y)"));
    vu.label(egui::RichText::new(format!("{:.0} ms", d.render_ms)).color(t.text_faint));

    if !pointer
        && let Some(origin) = d.edit_origin.take()
        && origin != d.params
    {
        d.remember(origin);
    }
    return_action
}

/// The Channel menu's Colours entry: red, green and blue overlaid.
const HIST_COLOURS: usize = 4;
/// Height of the histogram header (Channel menu and plot) above the panels.
const HIST_HEADER: f32 = 132.0;
/// The plot's height, and a bin holding this share of the pixels reaches its top.
const HIST_HEIGHT: f32 = 100.0;
const HIST_PEAK_SHARE: f32 = 1.0 / 60.0;
/// The develop output is 8-bit for display, so the bins comb; the plot smooths them over ±2.
const HIST_SMOOTH: usize = 2;

/// Red, green and blue histograms of straight-alpha pixels, at the 8-bit levels the preview
/// shows, each pixel counted by its alpha.
fn colour_histogram(px: &[[f32; 4]]) -> [[f32; 256]; 3] {
    let mut h = [[0.0f32; 256]; 3];
    let bin = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as usize;
    for q in px {
        let a = q[3].clamp(0.0, 1.0);
        if a <= 0.0 {
            continue;
        }
        for (c, hc) in h.iter_mut().enumerate() {
            if let Some(b) = hc.get_mut(bin(q[c])) {
                *b += a;
            }
        }
    }
    h
}

/// A triangular blur over ±`radius` bins (display only).
fn smooth_bins(bins: &[f32; 256], radius: usize) -> [f32; 256] {
    let r = radius as i64;
    let total: f32 = (-r..=r).map(|o| (r + 1 - o.abs()) as f32).sum();
    let mut out = [0.0f32; 256];
    for (i, o) in out.iter_mut().enumerate() {
        let sum: f32 = (-r..=r)
            .filter_map(|off| {
                let j = usize::try_from(i as i64 + off).ok()?;
                Some(bins.get(j)? * (r + 1 - off.abs()) as f32)
            })
            .sum();
        *o = sum / total.max(1.0);
    }
    out
}

/// One filled band: bin `i`'s height is `bins[i] * scale` of the plot, at most all of it.
/// Drawn with zero alpha (premultiplied), so bands add up: red and green overlap as yellow,
/// all three as white.
fn draw_band(p: &egui::Painter, r: ERect, bins: &[f32; 256], scale: f32, color: Color32) {
    let add = Color32::from_rgba_premultiplied(color.r(), color.g(), color.b(), 0);
    let mut mesh = egui::Mesh::default();
    let x = |i: usize| r.left() + i as f32 / 255.0 * r.width();
    let y = |v: f32| r.bottom() - (v * scale).clamp(0.0, 1.0) * r.height();
    for i in 0..255 {
        let (a, b) = (bins[i], bins[i + 1]);
        let n = mesh.vertices.len() as u32;
        for q in [pos2(x(i), r.bottom()), pos2(x(i), y(a)), pos2(x(i + 1), y(b)), pos2(x(i + 1), r.bottom())] {
            mesh.colored_vertex(q, add);
        }
        mesh.add_triangle(n, n + 1, n + 2);
        mesh.add_triangle(n, n + 2, n + 3);
    }
    p.add(mesh);
}

/// The histogram header: Channel menu, then the plot of the developed preview.
fn histogram(ui: &mut egui::Ui, d: &mut CameraRawDialog) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!("Channel:")).color(t.text_dim));
        let opts = [(0usize, "RGB"), (1, "Red"), (2, "Green"), (3, "Blue"), (HIST_COLOURS, "Colours")];
        widgets::dropdown(ui, "camera-raw-hist-channel", &mut d.hist_channel, &opts, ui.available_width().min(220.0));
    });
    ui.add_space(4.0);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), HIST_HEIGHT), Sense::hover());
    let p = ui.painter_at(r);
    // The bands add light, so the plot is dark and the bands light on every theme.
    p.rect_filled(r, t.radius_sm, Color32::from_gray(18));
    let fill = Color32::from_gray(205);
    let plot = r.shrink2(vec2(2.0, 1.0));
    let n = (d.pw * d.ph).max(1) as f32;
    let scale = 1.0 / (HIST_PEAK_SHARE * n);
    let h = d.hist.as_ref().map(|c| smooth_bins(&c, HIST_SMOOTH));
    match d.hist_channel {
        // RGB: the three channels' sum, at a third of the scale.
        0 => {
            let mut sum = [0.0f32; 256];
            for (i, v) in sum.iter_mut().enumerate() {
                *v = h[0][i] + h[1][i] + h[2][i];
            }
            draw_band(&p, plot, &sum, scale / 3.0, fill);
        }
        c @ 1..=3 => draw_band(&p, plot, &h[c - 1], scale, fill),
        _ => {
            for (c, color) in [Color32::from_rgb(255, 0, 0), Color32::from_rgb(0, 255, 0), Color32::from_rgb(0, 0, 255)].into_iter().enumerate() {
                draw_band(&p, plot, &h[c], scale, color);
            }
        }
    }
}

/// Opens Camera Raw on a camera file opened last frame, when it is still the active document.
fn open_pending(app: &mut PhotospliceApp, ctx: &egui::Context) {
    let Some(id) = app.camera_raw_on_open.take() else { return };
    // The preference (`rawDefaults.openInCameraRaw`) can open raws straight away.
    if !app.session.prefs().raw_defaults.open_in_camera_raw || app.camera_raw.is_some() || app.session.active().map(|d| d.doc.id) != Some(id) {
        return;
    }
    match open(app, ctx) {
        Ok(()) => {
            if let Some(d) = app.camera_raw.as_mut() {
                d.opened_file = Some(id);
                d.layer_name = app.session.active().map(|s| s.doc.name.clone()).unwrap_or_default();
            }
        }
        Err(e) => app.ui.status = e,
    }
}

/// Cancel: close the dialog; on a camera file being opened, close the file too (as Photoshop).
fn cancel(app: &mut PhotospliceApp, ctx: &egui::Context) {
    let Some(d) = app.camera_raw.take() else { return };
    if let Some(id) = d.opened_file
        && let Some(i) = app.session.documents().iter().position(|s| s.doc.id == id)
        && let Err(e) = crate::menus::invoke(app, ctx, "file.close", json!({"document": i}))
    {
        app.ui.status = e;
    }
}

fn hue_color(h: f32) -> Color32 {
    let h6 = (h.rem_euclid(360.0)) / 60.0;
    let x = 1.0 - ((h6 % 2.0) - 1.0).abs();
    let (r, g, b) = match h6 as u32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    Color32::from_rgb((r * 220.0) as u8, (g * 220.0) as u8, (b * 220.0) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_drives_the_engine() {
        let mut app = PhotospliceApp::new(photosplice_engine::Session::new(), Default::default());
        let ctx = egui::Context::default();
        app.run("file.new", json!({"width": 64, "height": 48})).unwrap();
        app.run("layer.new.layer", json!({})).unwrap();
        app.run("edit.fill", json!({"color": "#808080"})).unwrap();
        let r = menu(&mut app, &ctx, "filter.cameraRaw", &json!({})).unwrap().unwrap();
        assert_eq!(r["proxy"], json!([64, 48]));
        let r = menu(&mut app, &ctx, "filter.cameraRaw", &json!({"ui": {"set": {"exposure": 1.0, "vignetteAmount": -40}}})).unwrap().unwrap();
        assert_eq!(r["params"]["exposure"], 1.0);
        assert_eq!(app.camera_raw.as_ref().unwrap().command_params(), json!({"exposure": 1.0, "vignetteAmount": -40.0}));
        let before = app.session.active().unwrap().doc.layer(app.session.active().unwrap().active_layer.unwrap()).unwrap().surface().unwrap().rgba(32, 24);
        menu(&mut app, &ctx, "filter.cameraRaw", &json!({"ui": {"commit": true}})).unwrap().unwrap();
        assert!(app.camera_raw.is_none());
        let after = app.session.active().unwrap().doc.layer(app.session.active().unwrap().active_layer.unwrap()).unwrap().surface().unwrap().rgba(32, 24);
        assert!(after[0] > before[0] + 0.1, "{before:?} → {after:?}");
        // Cancel leaves the document alone.
        menu(&mut app, &ctx, "filter.cameraRaw", &json!({})).unwrap().unwrap();
        let r = menu(&mut app, &ctx, "filter.cameraRaw", &json!({"ui": {"cancel": true}})).unwrap().unwrap();
        assert_eq!(r["cancelled"], true);
        assert!(menu(&mut app, &ctx, "filter.cameraRaw", &json!({"ui": {"set": {"exposure": "x"}}})).unwrap().is_err());
    }

    /// A dialog over the document, not a takeover: its OK button sits inside the centred
    /// dialog, short of the window's corner.
    #[test]
    fn camera_raw_is_a_dialog_over_the_document() {
        use egui_kittest::kittest::Queryable;
        let mut app = PhotospliceApp::new(photosplice_engine::Session::new(), Default::default());
        app.run("file.new", json!({"width": 64, "height": 48})).unwrap();
        let mut h =
            egui_kittest::Harness::builder().with_size(egui::vec2(1600.0, 1000.0)).build_ui_state(|ui, app: &mut PhotospliceApp| show(app, ui.ctx()), app);
        PhotospliceApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
        let ctx = h.ctx.clone();
        menu(h.state_mut(), &ctx, "filter.cameraRaw", &json!({})).unwrap().unwrap();
        h.run_steps(3);
        let ok = h.get_by_label("OK").rect();
        let dialog = widgets::tool_dialog_rect(&ctx, DIALOG_MAX);
        assert!(dialog.contains_rect(ok), "OK {ok:?} inside the dialog {dialog:?}");
        assert!(ok.right() < 1600.0 - 32.0 && ok.bottom() < 1000.0 - 32.0, "not in the window's corner: {ok:?}");
        // Dragging the title bar moves it, like the other dialogs.
        let from = pos2(dialog.center().x, dialog.top() + 12.0);
        h.hover_at(from);
        h.drag_at(from);
        h.run_steps(2);
        for i in 1..=10 {
            h.hover_at(from + vec2(-8.0, 5.0) * i as f32);
            h.run_steps(1);
        }
        h.drop_at(from + vec2(-80.0, 50.0));
        h.run_steps(3);
        let moved = h.get_by_label("OK").rect();
        assert!(moved.left() < ok.left() - 40.0 && moved.top() > ok.top() + 20.0, "{ok:?} → {moved:?}");
        // However far it is dragged, it stays on screen.
        let from = moved.center() - (ok.center() - from);
        h.hover_at(from);
        h.drag_at(from);
        h.run_steps(2);
        for i in 1..=10 {
            h.hover_at(from + vec2(-200.0, 200.0) * i as f32);
            h.run_steps(1);
        }
        h.drop_at(from + vec2(-2000.0, 2000.0));
        h.run_steps(3);
        let far = h.get_by_label("OK").rect();
        assert!(far.bottom() <= 1000.0, "{far:?}");
        // Nor into the title bar: on macOS that is the system's, under the traffic lights, and a
        // drag there would move the whole window.
        let from = far.center() - (ok.center() - pos2(dialog.center().x, dialog.top() + 12.0));
        h.hover_at(from);
        h.drag_at(from);
        h.run_steps(2);
        for i in 1..=10 {
            h.hover_at(from + vec2(0.0, -200.0) * i as f32);
            h.run_steps(1);
        }
        h.drop_at(from + vec2(0.0, -2000.0));
        h.run_steps(3);
        // The title sits a frame margin (and a point) below the dialog's top edge.
        let margin = f32::from(ctx.global_style().spacing.menu_margin.top) + 1.0;
        let top = h.get_by_label_contains("Camera Raw Filter").rect().top() - margin;
        let bar = crate::panels::title_bar_height(&ctx);
        assert!((top - bar).abs() < 1.5, "dialog top {top} stops at the title bar ({bar})");
    }

    /// Dragging on the curve adds a point there and it follows the pointer for the whole drag
    /// (it used to stop after the first frame), and dragging it again moves it rather than
    /// adding another beside it.
    #[test]
    fn curve_point_follows_a_long_drag_and_can_be_dragged_again() {
        use egui_kittest::kittest::Queryable;
        let mut app = PhotospliceApp::new(photosplice_engine::Session::new(), Default::default());
        app.run("file.new", json!({"width": 64, "height": 48})).unwrap();
        let mut h =
            egui_kittest::Harness::builder().with_size(egui::vec2(1600.0, 1000.0)).build_ui_state(|ui, app: &mut PhotospliceApp| show(app, ui.ctx()), app);
        PhotospliceApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
        let ctx = h.ctx.clone();
        menu(h.state_mut(), &ctx, "filter.cameraRaw", &json!({})).unwrap().unwrap();
        h.state_mut().camera_raw.as_mut().unwrap().open_section = Some("Curve".into());
        h.run_steps(3);
        let curve = |h: &egui_kittest::Harness<'_, PhotospliceApp>| h.state().camera_raw.as_ref().unwrap().params.point_curve.clone();
        // Drags by `by` in 30 steps; returns the curve after the first step (the drag has begun).
        let drag = |h: &mut egui_kittest::Harness<'_, PhotospliceApp>, from: Pos2, by: egui::Vec2| {
            h.hover_at(from);
            h.drag_at(from);
            h.run_steps(2);
            let mut early = Vec::new();
            for i in 1..=30 {
                h.hover_at(from + by * (i as f32 / 30.0));
                h.run_steps(1);
                if i == 3 {
                    early = h.state().camera_raw.as_ref().unwrap().params.point_curve.clone();
                }
            }
            h.drop_at(from + by);
            h.run_steps(2);
            early
        };
        // The editor's square starts just under the header, at the panel's left edge.
        let header = h.get_by_label("Curve").rect();
        let from = pos2(header.left() + 100.0, header.bottom() + 130.0);
        let early = drag(&mut h, from, vec2(45.0, -60.0));
        let c = curve(&h);
        assert_eq!(c.len(), 3, "one point added: {c:?}");
        let (added, first) = (early[1], c[1]);
        // Most of the 45 x 60 pt came after the drag's first frames: the point kept following.
        assert!(first[0] - added[0] > 20.0 && first[1] - added[1] > 30.0, "followed the whole drag: {added:?} → {first:?}");
        // Grab it where it ended up and drag it back down: it moves, nothing is added.
        drag(&mut h, from + vec2(45.0, -60.0), vec2(-20.0, 40.0));
        let c = curve(&h);
        assert_eq!(c.len(), 3, "no point added beside it: {c:?}");
        assert!(c[1][0] < first[0] - 10.0 && c[1][1] < first[1] - 20.0, "{first:?} → {:?}", c[1]);
    }

    #[test]
    fn histogram_counts_the_preview_by_alpha_and_smooths_for_display() {
        let px = [[1.0, 0.0, 0.5, 1.0], [1.0, 0.0, 0.5, 0.5], [0.2, 0.2, 0.2, 0.0], [f32::NAN, 2.0, -1.0, 1.0]];
        let h = colour_histogram(&px);
        assert_eq!((h[0][255], h[1][0], h[2][128]), (1.5, 1.5, 1.5), "half-transparent pixels count half");
        assert_eq!(h[0][51], 0.0, "fully transparent pixels don't count");
        assert_eq!((h[0][0], h[1][255], h[2][0]), (1.0, 1.0, 1.0), "out-of-range and NaN values land in the end bins");
        let mut spike = [0.0; 256];
        spike[100] = 9.0;
        let sm = smooth_bins(&spike, 2);
        assert_eq!((sm[100], sm[99], sm[98], sm[97]), (3.0, 2.0, 1.0, 0.0));
        assert!((sm.iter().sum::<f32>() - 9.0).abs() < 1e-4, "smoothing keeps the total away from the ends");
        assert_eq!(smooth_bins(&spike, 0), spike);
        smooth_bins(&[1.0; 256], 300);
    }

    #[test]
    fn camera_files_open_into_camera_raw_and_cancel_closes_them() {
        let services = crate::Services {
            import: Some(Box::new(|name: &str, _: &[u8], _: usize| {
                let mut d = photosplice_doc::Document::new(
                    name,
                    photosplice_geom::Size::new(32, 24),
                    photosplice_color::ColorMode::Rgb,
                    photosplice_color::SampleType::U16,
                );
                d.layers
                    .push(photosplice_doc::Layer::new("Background", photosplice_doc::LayerContent::Raster(photosplice_raster::Surface::new(d.pixel_format()))));
                Ok((d, Vec::new()))
            })),
            ..Default::default()
        };
        let mut app = PhotospliceApp::new(photosplice_engine::Session::new(), services);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let frame = |app: &mut PhotospliceApp| {
            ctx.run_ui(egui::RawInput::default(), |ui| show(app, ui.ctx())).textures_delta.clear();
        };
        app.open_file("/shoot/IMG_0001.CR3", b"raw").unwrap();
        frame(&mut app);
        let d = app.camera_raw.as_ref().expect("Camera Raw opened on the camera file");
        assert!(d.opened_file.is_some());
        // Cancel closes the file again.
        menu(&mut app, &ctx, "filter.cameraRaw", &json!({"ui": {"cancel": true}})).unwrap().unwrap();
        assert!(app.camera_raw.is_none());
        assert!(app.session.documents().is_empty());
        // Ordinary files open as they always did.
        app.open_file("/shoot/flat.png", b"png").unwrap();
        frame(&mut app);
        assert!(app.camera_raw.is_none());
        assert!(is_raw_name("a.nef") && !is_raw_name("a.psd") && !is_raw_name("dng"));
    }

    #[test]
    fn white_balance_auto_and_eyedropper() {
        let mut app = PhotospliceApp::new(photosplice_engine::Session::new(), Default::default());
        let ctx = egui::Context::default();
        app.run("file.new", json!({"width": 64, "height": 48})).unwrap();
        app.run("layer.new.layer", json!({})).unwrap();
        app.run("edit.fill", json!({"color": "#8c8073"})).unwrap(); // a warm grey
        menu(&mut app, &ctx, "filter.cameraRaw", &json!({})).unwrap().unwrap();
        let r = menu(&mut app, &ctx, "filter.cameraRaw", &json!({"ui": {"autoWhiteBalance": true}})).unwrap().unwrap();
        let t = r["params"]["temperature"].as_f64().unwrap();
        assert!(t < -5.0, "a warm cast is cooled: {t}");
        // The eyedropper on the same colour lands on the same setting.
        menu(&mut app, &ctx, "filter.cameraRaw", &json!({"ui": {"set": {"temperature": 0, "tint": 0}}})).unwrap().unwrap();
        let r = menu(&mut app, &ctx, "filter.cameraRaw", &json!({"ui": {"whiteBalanceAt": [0.5, 0.5]}})).unwrap().unwrap();
        assert_eq!(r["params"]["temperature"].as_f64().unwrap(), t);
        assert!(menu(&mut app, &ctx, "filter.cameraRaw", &json!({"ui": {"whiteBalanceAt": ["a", 1]}})).unwrap().is_err());
    }

    /// In a window of its own, closing that window is Cancel. Without a multi-window backend
    /// egui embeds the "window" (`ViewportClass::EmbeddedWindow`) and the close request comes
    /// from the window it is drawn in; the in-window frame (`embed_viewports`) keeps it open.
    #[test]
    fn closing_the_camera_raw_window_cancels() {
        let mut app = PhotospliceApp::new(photosplice_engine::Session::new(), Default::default());
        let ctx = egui::Context::default();
        PhotospliceApp::setup_context(&ctx, crate::theme::ThemeKind::ALL[0]);
        app.run("file.new", json!({"width": 64, "height": 48})).unwrap();
        app.run("layer.new.layer", json!({})).unwrap();
        app.run("edit.fill", json!({"color": "#808080"})).unwrap();
        let steps = app.session.active().unwrap().history.past_len();
        let frame = |app: &mut PhotospliceApp, close: bool| {
            let mut input = egui::RawInput::default();
            if close {
                let info = input.viewports.entry(egui::ViewportId::ROOT).or_default();
                info.events.push(egui::ViewportEvent::Close);
            }
            ctx.run_ui(input, |ui| show(app, ui.ctx())).textures_delta.clear();
        };
        open(&mut app, &ctx).unwrap();
        frame(&mut app, false);
        assert!(app.camera_raw.is_some(), "drawn over the main window");
        ctx.set_embed_viewports(false);
        frame(&mut app, false);
        assert!(app.camera_raw.is_some(), "drawn in its window");
        frame(&mut app, true);
        assert!(app.camera_raw.is_none(), "closing the window is Cancel");
        assert_eq!(app.session.active().unwrap().history.past_len(), steps);
    }

    /// Bad control requests are errors, never panics, and leave the dialog as it was; a request
    /// that opened the dialog and failed leaves it closed. Ctrl+Z steps back through the edits.
    #[test]
    fn control_requests_are_validated_and_edits_undo() {
        let mut app = PhotospliceApp::new(photosplice_engine::Session::new(), Default::default());
        let ctx = egui::Context::default();
        app.run("file.new", json!({"width": 32, "height": 24})).unwrap();
        app.run("layer.new.layer", json!({})).unwrap();
        app.run("edit.fill", json!({"color": "#808080"})).unwrap();
        for bad in [
            json!([1, 2]),
            json!({"ui": 3}),
            json!({"ui": {"nope": true}}),
            json!({"ui": {"commit": 1}}),
            json!({"ui": {"commit": true, "cancel": true}}),
            json!({"ui": {"set": 5}}),
            json!({"ui": {"set": {"noSuchSetting": 1}}}),
            json!({"ui": {"whiteBalanceAt": [f64::MAX, "x"]}}),
            json!({"ui": {"whiteBalanceAt": [0.5]}}),
        ] {
            assert!(menu(&mut app, &ctx, "filter.cameraRaw", &bad).unwrap().is_err(), "{bad}");
            assert!(app.camera_raw.is_none(), "a failed request that opened the dialog closes it: {bad}");
        }
        menu(&mut app, &ctx, "filter.cameraRaw", &json!({"ui": {"set": {"exposure": 0.5}}})).unwrap().unwrap();
        menu(&mut app, &ctx, "filter.cameraRaw", &json!({"ui": {"set": {"exposure": 1.0}}})).unwrap().unwrap();
        assert!(menu(&mut app, &ctx, "filter.cameraRaw", &json!({"ui": {"set": {"exposure": "x"}}})).unwrap().is_err());
        assert!(app.camera_raw.is_some(), "an open dialog stays open on a bad request");
        let r = menu(&mut app, &ctx, "filter.cameraRaw", &json!({"ui": {"undo": true}})).unwrap().unwrap();
        assert_eq!(r["params"]["exposure"], 0.5);
        let r = menu(&mut app, &ctx, "filter.cameraRaw", &json!({"ui": {"undo": true}})).unwrap().unwrap();
        assert_eq!(r["params"]["exposure"], 0.0);
        assert_eq!(r["canUndo"], false);
        let r = menu(&mut app, &ctx, "filter.cameraRaw", &json!({"ui": {"undo": true, "redo": true}})).unwrap().unwrap();
        assert_eq!(r["params"]["exposure"], 0.5, "nothing to undo, then one redo");
    }
}
