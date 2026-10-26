//! Filter › Blur › Lens Blur…: Reference's large dialog. A big preview on the left with its zoom
//! under it; OK / Cancel, Preview (Faster or More Accurate), Depth Map (source, Set Focal Point,
//! Blur Focal Distance, Invert; the source is the layer's transparency, its mask or an alpha
//! channel), Iris (shape, radius, blade curvature, rotation), Specular
//! Highlights and Noise on the right.
//!
//! It is the schema filter dialog for `filter.blur.lensBlur` (`filter_dialog::open`) drawn in a
//! window of its own ([`crate::dialogs::frame`], which can leave the main window like Lens
//! Correction's): the same fields, live canvas preview, history, smart filters and control
//! channel (`ui.dialog.*`). Its private fields: `__lbAccurate` (More Accurate) and
//! `__lbFocalPick` (Set Focal Point armed: a click in the preview sets the focal distance).

use egui::{Rect, Sense, Stroke, pos2, vec2};
use photosplice_doc::{Document, LayerContent};
use serde_json::{Map, Value, json};

use crate::PhotospliceApp;
use crate::filter_preview_ui::ViewOpts;
use crate::theme::Tokens;
use crate::widgets;

pub(crate) const COMMAND: &str = "filter.blur.lensBlur";
const ACCURATE: &str = "__lbAccurate";
const FOCAL_PICK: &str = "__lbFocalPick";
/// The settings column's width and the zoom bar's height.
const PANEL_W: f32 = 300.0;
const ZOOM_BAR_H: f32 = 34.0;
/// The dialog's largest size; smaller windows get a smaller one.
const DIALOG_MAX: egui::Vec2 = vec2(1400.0, 900.0);
const VIEW: ViewOpts = ViewOpts { open_fitted: true, keep_clicks: false };

/// A remembered alpha channel the document no longer has opens as no depth map (None), rather
/// than as a source the preview can't read.
pub(crate) fn defaults(app: &PhotospliceApp, f: &mut Map<String, Value>) {
    if depth_source(f) == "channel" && channel_index(app, f).is_none() {
        f.insert("depthMap".into(), json!("none"));
    }
}

/// Whether the dialog with fields `f` is Lens Blur's.
pub(crate) fn owns(f: &Map<String, Value>) -> bool {
    f.contains_key("__filter") && f.get("__command").and_then(Value::as_str) == Some(COMMAND)
}

/// The proxy factor Lens Blur's preview runs at: Faster (the default, as in Reference) previews a
/// copy reduced to the screen-sized proxy, More Accurate the full image OK applies. Other dialogs
/// keep `k`.
pub(crate) fn preview_factor(f: &Map<String, Value>, doc: &Document, k: u32) -> u32 {
    if owns(f) && f.get(ACCURATE).and_then(Value::as_bool) != Some(true) { crate::proxy::factor(doc).max(k) } else { k }
}

/// Draw the dialog `dialog` (its fields `f`) in its own window. `Some(true)` for OK, `Some(false)`
/// for Cancel, Esc or the window's close box.
pub(crate) fn show(app: &mut PhotospliceApp, ctx: &egui::Context, dialog: u64, title: &str, f: &mut Map<String, Value>, interactive: bool) -> Option<bool> {
    let rect = widgets::tool_dialog_rect(ctx, DIALOG_MAX);
    let size = crate::dialogs::FrameSize::Panels(rect.size());
    let shown = crate::dialogs::frame(ctx, egui::Id::new("lens-blur-dialog"), title, size, |ui, size| {
        if !interactive {
            ui.disable();
        }
        body(app, ui, size.unwrap_or(rect.size()), dialog, f, interactive)
    });
    let outcome = shown.inner.flatten();
    if outcome.is_none() && interactive && shown.should_close() { Some(false) } else { outcome }
}

fn body(app: &mut PhotospliceApp, ui: &mut egui::Ui, size: egui::Vec2, dialog: u64, f: &mut Map<String, Value>, interactive: bool) -> Option<bool> {
    let t = Tokens::get(ui.ctx());
    let (body, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter().clone();
    let panel = Rect::from_min_max(pos2(body.right() - PANEL_W, body.top()), body.max);
    let area = Rect::from_min_max(body.min, pos2(panel.left(), body.bottom()));
    let bar = Rect::from_min_max(pos2(area.left(), area.bottom() - ZOOM_BAR_H), area.max);
    painter.rect_filled(area, 0.0, t.canvas);

    // Preview: the whole image to start with; Set Focal Point takes the clicks.
    let picking = depth_source(f) != "none" && flag(f, FOCAL_PICK);
    let view = Rect::from_min_max(area.min, pos2(area.right(), bar.top())).shrink(12.0);
    let opts = ViewOpts { keep_clicks: picking, ..VIEW };
    let shown = crate::filter_preview_ui::view(app, dialog, ui, f, view, opts);
    if picking {
        if ui.rect_contains_pointer(view) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        if let Some((x, y)) = shown.as_ref().and_then(|s| s.clicked)
            && let Some(d) = depth_at(app, f, x, y)
        {
            f.insert("focalDistance".into(), json!((d * 255.0).round()));
        }
    }
    painter.line_segment([bar.left_top(), bar.right_top()], Stroke::new(1.0, t.separator));
    let mut zu = ui.new_child(egui::UiBuilder::new().max_rect(bar.shrink2(vec2(10.0, 4.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    if let Some(s) = &shown {
        crate::filter_preview_ui::zoom_controls(&mut zu, f, s.fit, VIEW);
    }

    // Settings.
    painter.rect_filled(panel, 0.0, t.dock);
    painter.line_segment([panel.left_top(), panel.left_bottom()], Stroke::new(1.0, t.separator));
    let pad = widgets::TOOL_PANEL_PAD;
    let mut pu = ui.new_child(egui::UiBuilder::new().max_rect(panel.shrink2(pad)).layout(egui::Layout::top_down(egui::Align::Min)));
    let mut outcome = buttons(app, &mut pu, f);
    pu.add_space(8.0);
    egui::ScrollArea::vertical().id_salt("lens-blur-settings").auto_shrink([false, false]).show(&mut pu, |ui| settings(app, ui, f));
    // ↩ is OK, as in every dialog (not while a dropdown has it).
    if outcome.is_none() && interactive && !egui::Popup::is_any_open(ui.ctx()) && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        outcome = Some(true);
    }
    outcome
}

/// OK and Cancel stacked at the top of the column, as in Reference; Alt turns Cancel into Reset.
fn buttons(app: &mut PhotospliceApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) -> Option<bool> {
    let w = ui.available_width();
    let mut outcome = None;
    ui.vertical_centered(|ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        if widgets::primary_button(ui, tl!("OK"), w * 0.6).clicked() {
            outcome = Some(true);
        }
        let reset = ui.input(|i| i.modifiers.alt) && crate::dialog_alt_reset::supported(f);
        if widgets::secondary_button(ui, if reset { tl!("Reset") } else { tl!("Cancel") }, w * 0.6).clicked() {
            if reset {
                let ctx = ui.ctx().clone();
                crate::dialog_alt_reset::apply(app, &ctx, f);
            } else {
                outcome = Some(false);
            }
        }
    });
    outcome
}

fn settings(app: &PhotospliceApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    ui.spacing_mut().item_spacing.y = 6.0;
    group(ui, None, |ui| {
        let mut preview = flag(f, "__preview");
        if widgets::checkbox(ui, &mut preview, tl!("Preview")).changed() {
            f.insert("__preview".into(), json!(preview));
        }
        ui.add_enabled_ui(preview, |ui| {
            ui.horizontal(|ui| {
                let accurate = flag(f, ACCURATE);
                if crate::color_range_ui::radio(ui, !accurate, "Faster").clicked() {
                    f.insert(ACCURATE.into(), json!(false));
                }
                ui.add_space(10.0);
                if crate::color_range_ui::radio(ui, accurate, "More Accurate").clicked() {
                    f.insert(ACCURATE.into(), json!(true));
                }
            });
        });
    });

    group(ui, Some(tl!("Depth Map")), |ui| {
        let mut source = source_key(app, f);
        let channels = alpha_channels(app);
        let mut opts = vec![("none".to_string(), tl!("None").to_string()), ("transparency".to_string(), tl!("Transparency").to_string())];
        if has_mask(app) || source == "layerMask" {
            opts.push(("layerMask".to_string(), tl!("Layer Mask").to_string()));
        }
        opts.extend(channels.iter().map(|(i, name)| (format!("{CHANNEL_KEY}{i}"), name.clone())));
        let opts: Vec<(String, &str)> = opts.iter().map(|(k, l)| (k.clone(), l.as_str())).collect();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("{}:", tl!("Source"))).color(Tokens::get(ui.ctx()).text_dim));
            if widgets::dropdown(ui, "lens-blur-source", &mut source, &opts, ui.available_width()) {
                set_source(f, &source);
            }
        });
        ui.add_enabled_ui(source != "none", |ui| {
            ui.horizontal(|ui| {
                let on = flag(f, FOCAL_PICK);
                let r = crate::icons::button(ui, "scan", 24.0, on, tl!("Set Focal Point"));
                let l = ui.add(egui::Label::new(egui::RichText::new(tl!("Set Focal Point")).color(Tokens::get(ui.ctx()).text_dim)).sense(Sense::click()));
                if r.clicked() || l.clicked() {
                    f.insert(FOCAL_PICK.into(), json!(!on));
                }
            });
            slider(ui, f, "focalDistance", tl!("Blur Focal Distance"), 0.0..=255.0, 0.0);
            check(ui, f, "invert", tl!("Invert"));
        });
    });

    group(ui, Some(tl!("Iris")), |ui| {
        let mut shape = f.get("shape").and_then(Value::as_str).unwrap_or("hexagon").to_string();
        let opts = [
            ("triangle".to_string(), tl!("Triangle (3)")),
            ("square".to_string(), tl!("Square (4)")),
            ("pentagon".to_string(), tl!("Pentagon (5)")),
            ("hexagon".to_string(), tl!("Hexagon (6)")),
            ("heptagon".to_string(), tl!("Heptagon (7)")),
            ("octagon".to_string(), tl!("Octagon (8)")),
        ];
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("{}:", tl!("Shape"))).color(Tokens::get(ui.ctx()).text_dim));
            if widgets::dropdown(ui, "lens-blur-shape", &mut shape, &opts, ui.available_width()) {
                f.insert("shape".into(), json!(shape));
            }
        });
        slider(ui, f, "radius", tl!("Radius"), 0.0..=100.0, 15.0);
        slider(ui, f, "bladeCurvature", tl!("Blade Curvature"), 0.0..=100.0, 0.0);
        slider(ui, f, "rotation", tl!("Rotation"), 0.0..=360.0, 0.0);
    });

    group(ui, Some(tl!("Specular Highlights")), |ui| {
        slider(ui, f, "brightness", tl!("Brightness"), 0.0..=100.0, 0.0);
        slider(ui, f, "threshold", tl!("Threshold"), 0.0..=255.0, 255.0);
    });

    group(ui, Some(tl!("Noise")), |ui| {
        slider(ui, f, "noise", tl!("Amount"), 0.0..=100.0, 0.0);
        let gaussian = f.get("distribution").and_then(Value::as_str) == Some("gaussian");
        group(ui, Some(tl!("Distribution")), |ui| {
            if crate::color_range_ui::radio(ui, !gaussian, "Uniform").clicked() {
                f.insert("distribution".into(), json!("uniform"));
            }
            if crate::color_range_ui::radio(ui, gaussian, "Gaussian").clicked() {
                f.insert("distribution".into(), json!("gaussian"));
            }
        });
        check(ui, f, "monochromatic", tl!("Monochromatic"));
    });
}

/// A titled box of settings, as Reference groups them.
fn group(ui: &mut egui::Ui, title: Option<&str>, add: impl FnOnce(&mut egui::Ui)) {
    let t = Tokens::get(ui.ctx());
    if let Some(title) = title {
        ui.label(egui::RichText::new(title).font(crate::theme::semibold(12.5)).color(t.text));
    }
    egui::Frame::NONE.stroke(Stroke::new(1.0, t.separator)).corner_radius(t.radius_sm).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui);
    });
    ui.add_space(4.0);
}

/// A whole-number setting (Reference's Lens Blur fields take integers); double-click resets it.
fn slider(ui: &mut egui::Ui, f: &mut Map<String, Value>, key: &str, label: &str, range: std::ops::RangeInclusive<f32>, rest: f32) {
    let mut v = f.get(key).and_then(Value::as_f64).map_or(rest, |x| x as f32);
    let r = widgets::slider_row(ui, label, &mut v, range.clone(), "", None);
    if r.double_clicked() {
        v = rest;
    }
    if r.changed() || r.double_clicked() {
        let v = if v.is_finite() { v.round().clamp(*range.start(), *range.end()) } else { rest };
        f.insert(key.into(), json!(v));
    }
}

fn check(ui: &mut egui::Ui, f: &mut Map<String, Value>, key: &str, label: &str) {
    let mut on = flag(f, key);
    if widgets::checkbox(ui, &mut on, label).changed() {
        f.insert(key.into(), json!(on));
    }
}

fn flag(f: &Map<String, Value>, key: &str) -> bool {
    f.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn depth_source(f: &Map<String, Value>) -> &str {
    f.get("depthMap").and_then(Value::as_str).unwrap_or("none")
}

/// The Source dropdown's key for an alpha channel: this prefix and the channel's index.
const CHANNEL_KEY: &str = "channel:";

/// The depth source as the Source dropdown names it: `depthMap`, or [`CHANNEL_KEY`] and the
/// channel's index for an alpha channel.
fn source_key(app: &PhotospliceApp, f: &Map<String, Value>) -> String {
    match depth_source(f) {
        "channel" => channel_index(app, f).map_or_else(|| "none".to_string(), |i| format!("{CHANNEL_KEY}{i}")),
        other => other.to_string(),
    }
}

/// The document channel `depthChannel` names, as the engine reads it: an index among the extra
/// channels (the first when not given) or a name (the first with it, from a recorded filter).
fn channel_index(app: &PhotospliceApp, f: &Map<String, Value>) -> Option<usize> {
    let channels = &app.session.active()?.doc.channels;
    match f.get("depthChannel") {
        None => (!channels.is_empty()).then_some(0),
        Some(Value::Number(n)) => usize::try_from(n.as_u64()?).ok().filter(|i| *i < channels.len()),
        Some(Value::String(name)) => channels.iter().position(|c| c.name == *name),
        Some(_) => None,
    }
}

/// Sets the params for a Source dropdown `key` ([`source_key`]).
fn set_source(f: &mut Map<String, Value>, key: &str) {
    match key.strip_prefix(CHANNEL_KEY).and_then(|i| i.parse::<u64>().ok()) {
        Some(i) => {
            f.insert("depthMap".into(), json!("channel"));
            f.insert("depthChannel".into(), json!(i));
        }
        None => {
            f.insert("depthMap".into(), json!(key));
        }
    }
}

/// The document's alpha channels (not spot colours), with their index among its extra channels:
/// Reference's Depth Map Source lists them after the layer's own sources.
fn alpha_channels(app: &PhotospliceApp) -> Vec<(usize, String)> {
    let Some(st) = app.session.active() else { return Vec::new() };
    st.doc.channels.iter().enumerate().filter(|(_, c)| c.spot.is_none()).map(|(i, c)| (i, c.name.clone())).collect()
}

/// Whether the layer the filter applies to has a layer mask (Lens Blur's Layer Mask depth).
fn has_mask(app: &PhotospliceApp) -> bool {
    app.session.active().and_then(|st| st.doc.layer(st.active_layer?)).is_some_and(|l| l.mask.is_some())
}

/// The depth (0..=1, after Invert) the filter reads at document point `(x, y)`, as the focal
/// distance that keeps that point sharp: the layer's opacity (Transparency), its mask or an alpha
/// channel. `None`
/// without a depth source or with nothing to read it from.
fn depth_at(app: &PhotospliceApp, f: &Map<String, Value>, x: f32, y: f32) -> Option<f32> {
    let st = app.session.active()?;
    let layer = st.doc.layer(st.active_layer?)?;
    if !(x.is_finite() && y.is_finite()) {
        return None;
    }
    let max_x = st.doc.size.width.saturating_sub(1) as f32;
    let max_y = st.doc.size.height.saturating_sub(1) as f32;
    let (px, py) = (x.floor().clamp(0.0, max_x) as i32, y.floor().clamp(0.0, max_y) as i32);
    let d = match depth_source(f) {
        "transparency" => {
            let surface = match &layer.content {
                LayerContent::Raster(s) => s,
                LayerContent::Smart(sm) => sm.cache.as_ref()?,
                _ => return None,
            };
            if !surface.format().alpha {
                return None;
            }
            *surface.pixel(px, py).last()?
        }
        "layerMask" => *layer.mask.as_ref()?.surface.pixel(px, py).first()?,
        "channel" => *st.doc.channels.get(channel_index(app, f)?)?.surface.pixel(px, py).first()?,
        _ => return None,
    };
    let d = d.clamp(0.0, 1.0);
    Some(if flag(f, "invert") { 1.0 - d } else { d })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lens_blur_dialog(app: &mut PhotospliceApp) -> (u64, Map<String, Value>) {
        let id = crate::filter_dialog::open(app, COMMAND).expect("Lens Blur opens a dialog");
        let fields = app.ui.dialogs.iter().find(|d| d.id == id).expect("open").fields.clone();
        (id, fields)
    }

    #[test]
    fn only_lens_blur_gets_the_large_dialog() {
        let mut app = PhotospliceApp::new(photosplice_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 64, "height": 48})).unwrap();
        let (_, f) = lens_blur_dialog(&mut app);
        assert!(owns(&f));
        let id = crate::filter_dialog::open(&mut app, "filter.blur.gaussianBlur").unwrap();
        assert!(!owns(&app.ui.dialogs.iter().find(|d| d.id == id).unwrap().fields));
        assert!(!owns(json!({"__command": COMMAND}).as_object().unwrap()), "not a filter dialog");
    }

    #[test]
    fn faster_previews_a_reduced_copy_and_more_accurate_the_full_image() {
        let doc = Document::new("big", photosplice_doc::Size::new(6000, 4000), photosplice_color::ColorMode::Rgb, photosplice_color::SampleType::U8);
        let mut f = json!({"__filter": true, "__command": COMMAND}).as_object().cloned().unwrap();
        let k = crate::proxy::factor(&doc);
        assert!(k > 1);
        assert_eq!(preview_factor(&f, &doc, 1), k, "Faster by default");
        f.insert(ACCURATE.into(), json!(true));
        assert_eq!(preview_factor(&f, &doc, 1), 1);
        // Other dialogs keep their factor.
        let other = json!({"__filter": true, "__command": "filter.blur.gaussianBlur"}).as_object().cloned().unwrap();
        assert_eq!(preview_factor(&other, &doc, 1), 1);
        // The private fields never reach the command.
        f.insert(FOCAL_PICK.into(), json!(true));
        let params = crate::filter_dialog::params_of(&f);
        assert!(params.get(ACCURATE).is_none() && params.get(FOCAL_PICK).is_none());
    }

    #[test]
    fn set_focal_point_reads_the_depth_under_the_click() {
        let mut app = PhotospliceApp::new(photosplice_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 40, "height": 20, "background": "transparent"})).unwrap();
        app.session
            .edit("Fixture", |doc, id| {
                let layer = id.expect("an active layer");
                let Some(LayerContent::Raster(s)) = doc.layer_mut(layer).map(|l| &mut l.content) else { panic!("a pixel layer") };
                s.fill_rect(photosplice_geom::Rect::new(0, 0, 20, 20), &[1.0, 0.0, 0.0, 1.0]);
                s.fill_rect(photosplice_geom::Rect::new(20, 0, 40, 20), &[1.0, 0.0, 0.0, 0.2]);
                Ok(())
            })
            .unwrap();
        let (_, mut f) = lens_blur_dialog(&mut app);
        assert_eq!(depth_at(&app, &f, 5.0, 5.0), None, "no depth source");
        f.insert("depthMap".into(), json!("transparency"));
        assert_eq!(depth_at(&app, &f, 5.0, 5.0), Some(1.0));
        let d = depth_at(&app, &f, 30.0, 5.0).unwrap();
        assert!((d - 0.2).abs() < 0.01, "{d}");
        f.insert("invert".into(), json!(true));
        assert!((depth_at(&app, &f, 30.0, 5.0).unwrap() - 0.8).abs() < 0.01);
        // Hostile points stay on the image or give nothing, never a panic.
        assert_eq!(depth_at(&app, &f, -1e9, 1e9), Some(0.0));
        assert_eq!(depth_at(&app, &f, f32::NAN, 0.0), None);
        // No mask: nothing to read.
        f.insert("depthMap".into(), json!("layerMask"));
        assert_eq!(depth_at(&app, &f, 5.0, 5.0), None);
    }

    #[test]
    fn alpha_channels_are_depth_sources() {
        let mut app = PhotospliceApp::new(photosplice_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 40, "height": 20})).unwrap();
        app.run("channel.new", json!({"name": "Depth", "fill": "black"})).unwrap();
        app.run("channel.new", json!({"name": "Sky", "fill": "white"})).unwrap();
        assert_eq!(alpha_channels(&app), vec![(0, "Depth".to_string()), (1, "Sky".to_string())]);
        let (_, mut f) = lens_blur_dialog(&mut app);
        // The dropdown's key round-trips to the command's params.
        set_source(&mut f, "channel:1");
        assert_eq!((f["depthMap"].clone(), f["depthChannel"].clone()), (json!("channel"), json!(1)));
        assert_eq!(source_key(&app, &f), "channel:1");
        assert_eq!(depth_at(&app, &f, 5.0, 5.0), Some(1.0));
        set_source(&mut f, "channel:0");
        assert_eq!(depth_at(&app, &f, 5.0, 5.0), Some(0.0));
        // A channel that is gone gives nothing to read.
        f.insert("depthChannel".into(), json!(7));
        assert_eq!(depth_at(&app, &f, 5.0, 5.0), None);
        // A recorded filter may name its channel.
        f.insert("depthChannel".into(), json!("Sky"));
        assert_eq!(source_key(&app, &f), "channel:1");
        assert_eq!(depth_at(&app, &f, 5.0, 5.0), Some(1.0));
        f.insert("depthChannel".into(), json!("Nowhere"));
        assert_eq!(source_key(&app, &f), "none", "a missing channel shows as no depth map");
        set_source(&mut f, "transparency");
        assert_eq!(source_key(&app, &f), "transparency");
    }

    #[test]
    fn a_remembered_channel_that_is_gone_opens_as_none() {
        let mut app = PhotospliceApp::new(photosplice_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 40, "height": 20})).unwrap();
        app.run("channel.new", json!({"name": "Depth", "fill": "black"})).unwrap();
        app.run("channel.new", json!({"name": "Sky", "fill": "white"})).unwrap();
        let mut f = json!({"depthMap": "channel", "depthChannel": 1}).as_object().cloned().unwrap();
        defaults(&app, &mut f);
        assert_eq!(f["depthMap"], json!("channel"), "still there: kept");
        app.run("channel.delete", json!({"channel": "Sky"})).unwrap();
        defaults(&app, &mut f);
        assert_eq!(f["depthMap"], json!("none"));
        // Hostile values never panic and fall back to None too.
        for bad in [json!(u64::MAX), json!(-1), json!(true), json!("Sky")] {
            let mut f = json!({"depthMap": "channel", "depthChannel": bad}).as_object().cloned().unwrap();
            defaults(&app, &mut f);
            assert_eq!(f["depthMap"], json!("none"), "{bad}");
        }
        // Through the dialog: remembered values pass through `defaults` when it opens.
        app.session.prefs.edit(|p| p.dialogs.insert(COMMAND.into(), json!({"depthMap": "channel", "depthChannel": 0, "radius": 30})));
        let (_, f) = lens_blur_dialog(&mut app);
        assert_eq!((f["depthMap"].clone(), f["radius"].clone()), (json!("channel"), json!(30)), "the channel is still there");
        app.session.prefs.edit(|p| p.dialogs.insert(COMMAND.into(), json!({"depthMap": "channel", "depthChannel": 1, "radius": 30})));
        let (_, f) = lens_blur_dialog(&mut app);
        assert_eq!((f["depthMap"].clone(), f["radius"].clone()), (json!("none"), json!(30)), "the deleted one opens as None");
    }

    #[test]
    fn the_dialog_draws_and_confirms_without_panicking() {
        let mut app = PhotospliceApp::new(photosplice_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 64, "height": 48})).unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 800.0)).build_ui_state(|ui, app| crate::dialogs::show(app, ui.ctx()), app);
        PhotospliceApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
        let id = crate::filter_dialog::open(h.state_mut(), COMMAND).unwrap();
        // Corrupt values are drawn, not panicked on.
        let fields = &mut h.state_mut().ui.dialog_mut(id).unwrap().fields;
        let good = fields.clone();
        fields.insert("radius".into(), json!("wide"));
        fields.insert("depthMap".into(), json!(42));
        fields.insert(FOCAL_PICK.into(), json!(true));
        fields.insert("__pvZoom".into(), json!(f64::NAN));
        h.run_steps(4);
        assert!(h.state().ui.dialogs.iter().any(|d| d.id == id), "still open");
        // OK applies the filter as one history step.
        h.state_mut().ui.dialog_mut(id).unwrap().fields = good;
        h.run_steps(2);
        egui_kittest::kittest::Queryable::get_by_label(&h, "OK").click();
        h.run_steps(2);
        assert!(h.state().ui.dialogs.is_empty(), "OK closes it");
        assert!(h.state_mut().session.undo(), "one history step to undo");
    }
}
