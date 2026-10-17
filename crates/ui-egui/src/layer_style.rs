//! Photoshop-style Layer Style dialog: effect list with checkboxes on the left, parameters on the right.
//! State lives in the dialog's `fields` (JSON), so automation can drive it like any other dialog.

use egui::{Color32, RichText, Sense, Stroke, StrokeKind, vec2};
use photosplice_color::ColorMode;
use photosplice_doc::blend_if::BlendRange;
use photosplice_doc::effects::{FxPaint, GlowSource, StrokePosition};
use photosplice_doc::{Effect, Layer};
use serde_json::{Map, Value, json};

use crate::theme::Tokens;
use crate::{PhotospliceApp, widgets};

/// The hand-off to the separate Color Picker dialog that the shared dialog plumbing (dialogs,
/// canvas preview, control channel) knows about. This dialog edits its colours in place, so a
/// picker is never bound to it: nothing owns a picker, nothing is previewed, and opening one is
/// reported as unsupported.
pub(crate) mod color_picker {
    use std::borrow::Cow;

    use serde_json::{Map, Value};

    use crate::PhotospliceApp;

    pub(crate) fn owns(_fields: &Map<String, Value>) -> bool {
        false
    }

    pub(crate) fn child_of(_fields: &Map<String, Value>, _parent: u64) -> bool {
        false
    }

    pub(crate) fn has_child(_app: &PhotospliceApp, _parent: u64) -> bool {
        false
    }

    pub(crate) fn open(_app: &mut PhotospliceApp, _parent: u64, _effect: &str, _field: &str) -> Result<u64, String> {
        Err("unsupported: Layer Style colours are edited in the dialog".into())
    }

    pub(crate) fn take_request(_app: &mut PhotospliceApp, _parent: u64) -> Result<(), String> {
        Ok(())
    }

    pub(crate) fn preview_fields<'a>(_app: &PhotospliceApp, _parent: u64, fields: &'a Map<String, Value>) -> Cow<'a, Map<String, Value>> {
        Cow::Borrowed(fields)
    }

    pub(crate) fn confirm(_app: &mut PhotospliceApp, _fields: &Map<String, Value>) -> Result<Value, String> {
        Err("unsupported: no Layer Style colour picker is open".into())
    }

    pub(crate) fn prune(_app: &mut PhotospliceApp) {}
}

#[derive(Clone, Copy)]
enum P {
    Slider(f32, f32, &'static str),
    Color,
    Blend,
    Choice(&'static [(&'static str, &'static str)]),
    Check,
    /// A pattern from the dialog's `patternList` field (`[[id, name], …]`).
    Pattern,
}

/// Effect kinds in Photoshop's list order: (command kind, label, params).
/// The Blending Options page id (not an effect).
pub const BLENDING: &str = "blendingOptions";

/// The kinds a layer can hold several of (the "+" on their rows), as in Photoshop, which allows
/// up to [`MAX_INSTANCES`] of each.
const MULTI: &[&str] = &["stroke", "innerShadow", "colorOverlay", "gradientOverlay", "dropShadow"];
const MAX_INSTANCES: usize = 10;

/// The effect kind of a list row: rows are keyed `stroke`, and `stroke#1`, `stroke#2`, … for
/// further instances. Each row has `on:{row}`, `p:{row}` (its params) and, when it came from
/// the layer or a style, `fx:{row}` (the effect as loaded, so what the dialog doesn't show is
/// kept on OK).
fn base_kind(row: &str) -> &str {
    row.split('#').next().unwrap_or(row)
}

/// A kind's rows, top first (the top one paints over the others): `rows:{kind}`, else the kind's
/// one row.
fn rows(f: &Map<String, Value>, kind: &str) -> Vec<String> {
    let listed: Vec<String> = f
        .get(&format!("rows:{kind}"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).filter(|r| base_kind(r) == kind).map(str::to_string).collect())
        .unwrap_or_default();
    if listed.is_empty() { vec![kind.to_string()] } else { listed }
}

/// A kind's factory params, lit by the document's light angle when it uses the global light.
fn fresh_params(kind: &str, light: f32) -> Value {
    let mut params = defaults(kind);
    if params.get("useGlobalLight").and_then(Value::as_bool) == Some(true) {
        params["angle"] = json!(light);
    }
    params
}

/// Replaces the dialog's effect rows with `effects`: each kind's instances in order, or one row
/// off at its defaults. Returns the first kind that has one.
fn set_effects(f: &mut Map<String, Value>, effects: &[Effect], light: f32) -> Option<&'static str> {
    let blending = format!("p:{BLENDING}");
    f.retain(|k, _| !(k.starts_with("rows:") || k.starts_with("fx:") || k.starts_with("on:") || (k.starts_with("p:") && *k != blending)));
    let mut first = None;
    for &(kind, _) in KINDS {
        let mine: Vec<&Effect> = effects.iter().filter(|e| kind_of(e) == kind).collect();
        if mine.is_empty() {
            f.insert(format!("on:{kind}"), json!(false));
            f.insert(format!("p:{kind}"), fresh_params(kind, light));
            continue;
        }
        first.get_or_insert(kind);
        let keys: Vec<String> = (0..mine.len()).map(|i| if i == 0 { kind.to_string() } else { format!("{kind}#{i}") }).collect();
        for (key, e) in keys.iter().zip(&mine) {
            f.insert(format!("on:{key}"), json!(e.enabled()));
            f.insert(format!("p:{key}"), values_of(e, light));
            if let Ok(fx) = serde_json::to_value(e) {
                f.insert(format!("fx:{key}"), fx);
            }
        }
        if keys.len() > 1 {
            f.insert(format!("rows:{kind}"), json!(keys));
        }
    }
    first
}

/// The params of `p` that differ from what the dialog loaded from `fx`: only those are applied
/// over the loaded effect, so the settings the dialog doesn't show (a bevel's technique, a
/// stroke's gradient) survive OK. A snapshot that doesn't read as an effect gets them all (the
/// engine then reports it).
fn changed_params(fx: &Value, p: &Value, light: f32) -> Value {
    let (Ok(e), Some(now)) = (serde_json::from_value::<Effect>(fx.clone()), p.as_object()) else {
        return p.clone();
    };
    let was = values_of(&e, light);
    Value::Object(now.iter().filter(|(k, v)| was.get(k.as_str()) != Some(*v)).map(|(k, v)| (k.clone(), v.clone())).collect())
}

pub const KINDS: &[(&str, &str)] = &[
    ("bevelEmboss", "Bevel & Emboss"),
    ("stroke", "Stroke"),
    ("innerShadow", "Inner Shadow"),
    ("innerGlow", "Inner Glow"),
    ("satin", "Satin"),
    ("colorOverlay", "Color Overlay"),
    ("gradientOverlay", "Gradient Overlay"),
    ("patternOverlay", "Pattern Overlay"),
    ("outerGlow", "Outer Glow"),
    ("dropShadow", "Drop Shadow"),
];

fn spec(kind: &str) -> &'static [(&'static str, &'static str, P)] {
    const POS: &[(&str, &str)] = &[("outside", "Outside"), ("inside", "Inside"), ("center", "Center")];
    const GSTYLE: &[(&str, &str)] = &[("linear", "Linear"), ("radial", "Radial"), ("angle", "Angle"), ("reflected", "Reflected"), ("diamond", "Diamond")];
    const BSTYLE: &[(&str, &str)] = &[("inner", "Inner Bevel"), ("outer", "Outer Bevel"), ("emboss", "Emboss"), ("pillow", "Pillow Emboss")];
    const DIR: &[(&str, &str)] = &[("up", "Up"), ("down", "Down")];
    const SRC: &[(&str, &str)] = &[("edge", "Edge"), ("center", "Center")];
    match kind {
        "dropShadow" => &[
            ("blend", "Blend Mode", P::Blend),
            ("color", "Color", P::Color),
            ("opacity", "Opacity", P::Slider(0.0, 100.0, "%")),
            ("angle", "Angle", P::Slider(-180.0, 180.0, "°")),
            ("useGlobalLight", "Use Global Light", P::Check),
            ("distance", "Distance", P::Slider(0.0, 300.0, "px")),
            ("spread", "Spread", P::Slider(0.0, 100.0, "%")),
            ("size", "Size", P::Slider(0.0, 250.0, "px")),
            ("knocksOut", "Layer Knocks Out Drop Shadow", P::Check),
        ],
        "innerShadow" => &[
            ("blend", "Blend Mode", P::Blend),
            ("color", "Color", P::Color),
            ("opacity", "Opacity", P::Slider(0.0, 100.0, "%")),
            ("angle", "Angle", P::Slider(-180.0, 180.0, "°")),
            ("useGlobalLight", "Use Global Light", P::Check),
            ("distance", "Distance", P::Slider(0.0, 300.0, "px")),
            ("choke", "Choke", P::Slider(0.0, 100.0, "%")),
            ("size", "Size", P::Slider(0.0, 250.0, "px")),
        ],
        "outerGlow" => &[
            ("blend", "Blend Mode", P::Blend),
            ("opacity", "Opacity", P::Slider(0.0, 100.0, "%")),
            ("color", "Color", P::Color),
            ("spread", "Spread", P::Slider(0.0, 100.0, "%")),
            ("size", "Size", P::Slider(0.0, 250.0, "px")),
            ("range", "Range", P::Slider(1.0, 100.0, "%")),
        ],
        "innerGlow" => &[
            ("blend", "Blend Mode", P::Blend),
            ("opacity", "Opacity", P::Slider(0.0, 100.0, "%")),
            ("color", "Color", P::Color),
            ("source", "Source", P::Choice(SRC)),
            ("choke", "Choke", P::Slider(0.0, 100.0, "%")),
            ("size", "Size", P::Slider(0.0, 250.0, "px")),
        ],
        "stroke" => &[
            ("size", "Size", P::Slider(1.0, 250.0, "px")),
            ("position", "Position", P::Choice(POS)),
            ("blend", "Blend Mode", P::Blend),
            ("opacity", "Opacity", P::Slider(0.0, 100.0, "%")),
            ("color", "Color", P::Color),
        ],
        "colorOverlay" => &[("blend", "Blend Mode", P::Blend), ("color", "Color", P::Color), ("opacity", "Opacity", P::Slider(0.0, 100.0, "%"))],
        "gradientOverlay" => &[
            ("blend", "Blend Mode", P::Blend),
            ("opacity", "Opacity", P::Slider(0.0, 100.0, "%")),
            ("from", "From", P::Color),
            ("to", "To", P::Color),
            ("reverse", "Reverse", P::Check),
            ("style", "Style", P::Choice(GSTYLE)),
            ("angle", "Angle", P::Slider(-180.0, 180.0, "°")),
            ("scale", "Scale", P::Slider(10.0, 150.0, "%")),
        ],
        "patternOverlay" => &[
            ("blend", "Blend Mode", P::Blend),
            ("opacity", "Opacity", P::Slider(0.0, 100.0, "%")),
            ("pattern", "Pattern", P::Pattern),
            ("angle", "Angle", P::Slider(-180.0, 180.0, "°")),
            ("scale", "Scale", P::Slider(1.0, 1000.0, "%")),
            ("link", "Link with Layer", P::Check),
        ],
        "bevelEmboss" => &[
            ("style", "Style", P::Choice(BSTYLE)),
            ("depth", "Depth", P::Slider(1.0, 1000.0, "%")),
            ("direction", "Direction", P::Choice(DIR)),
            ("size", "Size", P::Slider(0.0, 250.0, "px")),
            ("soften", "Soften", P::Slider(0.0, 16.0, "px")),
            ("angle", "Angle", P::Slider(-180.0, 180.0, "°")),
            ("useGlobalLight", "Use Global Light", P::Check),
            ("altitude", "Altitude", P::Slider(0.0, 90.0, "°")),
        ],
        "satin" => &[
            ("blend", "Blend Mode", P::Blend),
            ("color", "Color", P::Color),
            ("opacity", "Opacity", P::Slider(0.0, 100.0, "%")),
            ("angle", "Angle", P::Slider(-180.0, 180.0, "°")),
            ("distance", "Distance", P::Slider(0.0, 250.0, "px")),
            ("size", "Size", P::Slider(0.0, 250.0, "px")),
            ("invert", "Invert", P::Check),
        ],
        BLENDING => &[
            ("blend", "Blend Mode", P::Blend),
            ("opacity", "Opacity", P::Slider(0.0, 100.0, "%")),
            ("fillOpacity", "Fill Opacity", P::Slider(0.0, 100.0, "%")),
        ],
        _ => &[],
    }
}

fn defaults(kind: &str) -> Value {
    match kind {
        "dropShadow" => {
            json!({"blend": "Multiply", "color": "#000000", "opacity": 75, "angle": 120, "useGlobalLight": true, "distance": 5, "spread": 0, "size": 5, "knocksOut": true})
        }
        "innerShadow" => {
            json!({"blend": "Multiply", "color": "#000000", "opacity": 75, "angle": 120, "useGlobalLight": true, "distance": 5, "choke": 0, "size": 5})
        }
        "outerGlow" => json!({"blend": "Screen", "opacity": 75, "color": "#ffffbe", "spread": 0, "size": 5, "range": 50}),
        "innerGlow" => json!({"blend": "Screen", "opacity": 75, "color": "#ffffbe", "source": "edge", "choke": 0, "size": 5}),
        "stroke" => json!({"size": 3, "position": "outside", "blend": "Normal", "opacity": 100, "color": "#000000"}),
        "colorOverlay" => json!({"blend": "Normal", "color": "#ff0000", "opacity": 100}),
        "gradientOverlay" => {
            json!({"blend": "Normal", "opacity": 100, "from": "#000000", "to": "#ffffff", "reverse": false, "style": "linear", "angle": 90, "scale": 100})
        }
        "patternOverlay" => json!({"blend": "Normal", "opacity": 100, "pattern": "", "angle": 0, "scale": 100, "link": true}),
        "bevelEmboss" => {
            json!({"style": "inner", "depth": 100, "direction": "up", "size": 5, "soften": 0, "angle": 120, "useGlobalLight": true, "altitude": 30})
        }
        "satin" => json!({"blend": "Multiply", "color": "#000000", "opacity": 50, "angle": 19, "distance": 11, "size": 14, "invert": true}),
        _ => json!({}),
    }
}

fn hex(c: &photosplice_doc::Color) -> String {
    let [r, g, b, _] = c.to_rgba8();
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn kind_of(e: &Effect) -> &'static str {
    match e {
        Effect::DropShadow(_) => "dropShadow",
        Effect::InnerShadow(_) => "innerShadow",
        Effect::OuterGlow(_) => "outerGlow",
        Effect::InnerGlow(_) => "innerGlow",
        Effect::Stroke(_) => "stroke",
        Effect::ColorOverlay { .. } => "colorOverlay",
        Effect::GradientOverlay { .. } => "gradientOverlay",
        Effect::PatternOverlay { .. } => "patternOverlay",
        Effect::Satin(_) => "satin",
        Effect::BevelEmboss(_) => "bevelEmboss",
    }
}

/// Current values of an existing effect, in the command's parameter units.
/// `light` is the document's global light angle: an effect that uses it shows that angle.
fn values_of(e: &Effect, light: f32) -> Value {
    let mut v = defaults(kind_of(e));
    let set = |v: &mut Value, k: &str, x: Value| {
        v[k] = x;
    };
    match e {
        Effect::DropShadow(s) | Effect::InnerShadow(s) => {
            set(&mut v, "blend", json!(s.common.blend.label()));
            set(&mut v, "opacity", json!((s.common.opacity * 100.0).round()));
            set(&mut v, "color", json!(hex(&s.color)));
            set(&mut v, "angle", json!(if s.use_global_light { light } else { s.angle }));
            set(&mut v, "useGlobalLight", json!(s.use_global_light));
            set(&mut v, "distance", json!(s.distance));
            set(&mut v, if matches!(e, Effect::DropShadow(_)) { "spread" } else { "choke" }, json!((s.spread * 100.0).round()));
            set(&mut v, "size", json!(s.size));
        }
        Effect::OuterGlow(g) | Effect::InnerGlow(g) => {
            set(&mut v, "blend", json!(g.common.blend.label()));
            set(&mut v, "opacity", json!((g.common.opacity * 100.0).round()));
            if let FxPaint::Color(c) = &g.paint {
                set(&mut v, "color", json!(hex(c)));
            }
            set(&mut v, if matches!(e, Effect::OuterGlow(_)) { "spread" } else { "choke" }, json!((g.spread * 100.0).round()));
            set(&mut v, "size", json!(g.size));
            set(&mut v, "range", json!((g.range * 100.0).round()));
            if matches!(e, Effect::InnerGlow(_)) {
                // Shown, so a Center glow isn't turned into an Edge one on OK.
                set(&mut v, "source", json!(if g.source == GlowSource::Center { "center" } else { "edge" }));
            }
        }
        Effect::Stroke(s) => {
            set(&mut v, "blend", json!(s.common.blend.label()));
            set(&mut v, "opacity", json!((s.common.opacity * 100.0).round()));
            set(&mut v, "size", json!(s.size));
            set(
                &mut v,
                "position",
                json!(match s.position {
                    StrokePosition::Inside => "inside",
                    StrokePosition::Center => "center",
                    _ => "outside",
                }),
            );
            if let FxPaint::Color(c) = &s.paint {
                set(&mut v, "color", json!(hex(c)));
            }
        }
        Effect::ColorOverlay { common, color } => {
            set(&mut v, "blend", json!(common.blend.label()));
            set(&mut v, "opacity", json!((common.opacity * 100.0).round()));
            set(&mut v, "color", json!(hex(color)));
        }
        Effect::GradientOverlay { common, .. } => {
            set(&mut v, "blend", json!(common.blend.label()));
            set(&mut v, "opacity", json!((common.opacity * 100.0).round()));
        }
        Effect::Satin(s) => {
            set(&mut v, "blend", json!(s.common.blend.label()));
            set(&mut v, "opacity", json!((s.common.opacity * 100.0).round()));
            set(&mut v, "color", json!(hex(&s.color)));
            set(&mut v, "angle", json!(s.angle));
            set(&mut v, "distance", json!(s.distance));
            set(&mut v, "size", json!(s.size));
            set(&mut v, "invert", json!(s.invert));
        }
        Effect::BevelEmboss(b) => {
            set(&mut v, "depth", json!(b.depth));
            set(&mut v, "size", json!(b.size));
            set(&mut v, "soften", json!(b.soften));
            set(&mut v, "angle", json!(if b.use_global_light { light } else { b.angle }));
            set(&mut v, "useGlobalLight", json!(b.use_global_light));
            set(&mut v, "altitude", json!(b.altitude));
            set(&mut v, "direction", json!(if b.up { "up" } else { "down" }));
        }
        Effect::PatternOverlay { common, name, id, scale, angle, link, phase } => {
            set(&mut v, "blend", json!(common.blend.label()));
            set(&mut v, "opacity", json!((common.opacity * 100.0).round()));
            set(&mut v, "pattern", json!(if id.is_empty() { name } else { id }));
            set(&mut v, "scale", json!((scale * 100.0).round()));
            set(&mut v, "angle", json!(angle));
            set(&mut v, "link", json!(link));
            set(&mut v, "phaseX", json!(phase.0));
            set(&mut v, "phaseY", json!(phase.1));
        }
    }
    v
}

/// Which Advanced Blending controls a mode gets: the compositor applies Channels and Blend If to
/// RGB, Grayscale and Duotone documents only, so other modes don't show them.
fn blend_mode_key(mode: ColorMode) -> Option<&'static str> {
    match mode {
        ColorMode::Rgb => Some("rgb"),
        ColorMode::Grayscale | ColorMode::Duotone => Some("gray"),
        _ => None,
    }
}

/// A style the dialog can load (the Styles panel's presets, with their thumbnails).
pub struct StyleChoice {
    pub group: String,
    pub name: String,
    pub tex: egui::TextureHandle,
    pub effects: Vec<Effect>,
    pub blend: Option<photosplice_color::BlendMode>,
    pub fill_opacity: Option<f32>,
}

/// The Styles panel's presets as dialog choices.
pub fn style_choices(app: &PhotospliceApp, ctx: &egui::Context) -> Vec<StyleChoice> {
    app.session
        .presets
        .styles
        .iter()
        .flat_map(|g| {
            g.items.iter().map(|st| StyleChoice {
                group: g.name.clone(),
                name: st.name.clone(),
                tex: crate::preset_panels::style_texture(app, ctx, st),
                effects: st.effects.clone(),
                blend: st.blend,
                fill_opacity: st.fill_opacity,
            })
        })
        .collect()
}

/// Loads style `st` into the dialog, as picking it in Photoshop's Styles page does: its effects
/// replace the dialog's (each kind on with its settings, or off at its defaults), and the
/// blending options it was saved with replace those.
pub fn load_style(f: &mut Map<String, Value>, st: &StyleChoice) {
    let light = f.get("globalLight").and_then(Value::as_f64).unwrap_or(120.0) as f32;
    let first = set_effects(f, &st.effects, light);
    let key = format!("p:{BLENDING}");
    if let Some(Value::Object(bo)) = f.get_mut(&key) {
        if let Some(b) = st.blend {
            bo.insert("blend".into(), json!(b.label()));
        }
        if let Some(fill) = st.fill_opacity {
            bo.insert("fillOpacity".into(), json!((fill * 100.0).round()));
        }
    }
    if let Some(first) = first {
        f.insert("selected".into(), json!(first));
    }
}

/// Dialog fields for a layer: selected row, and per row {enabled, params, loaded effect}. `light` is the
/// document's global light angle, shown by effects that use it.
pub fn initial_fields(layer: &Layer, mode: ColorMode, select: Option<&str>, light: f32) -> Map<String, Value> {
    let mut f = Map::new();
    f.insert("layer".into(), json!(layer.id.0));
    f.insert("globalLight".into(), json!(light));
    let mut bo = json!({"blend": layer.blend.label(), "opacity": (layer.opacity * 100.0).round(), "fillOpacity": (layer.fill_opacity * 100.0).round()});
    if let Some(key) = blend_mode_key(mode) {
        let n = mode.color_channels();
        bo["channels"] = json!((0..n).map(|i| layer.excluded_channels & 1 << i == 0).collect::<Vec<_>>());
        let entry = |i: usize, [this, under]: [BlendRange; 2]| json!({"channel": i, "thisLayer": this.to_bytes(), "underlying": under.to_bytes()});
        bo["blendIf"] = if key == "gray" {
            // One channel: the dialog edits entry 1 (what the command calls "gray"); entry 0 (the
            // PSD composite, honoured too) is read when 1 is unset and reset on OK.
            let one = if layer.blend_if.get(1).iter().all(BlendRange::is_full) { layer.blend_if.get(0) } else { layer.blend_if.get(1) };
            json!([entry(0, [BlendRange::FULL; 2]), entry(1, one)])
        } else {
            Value::Array((0..=n).map(|i| entry(i, layer.blend_if.get(i))).collect())
        };
        f.insert("__mode".into(), json!(key));
        f.insert("__blendIfChannel".into(), json!(usize::from(key == "gray")));
    }
    f.insert(format!("p:{BLENDING}"), bo);
    f.insert("__preview".into(), json!(true));
    let first = set_effects(&mut f, &layer.effects.items, light);
    let sel = select.or(first).unwrap_or("dropShadow");
    f.insert("selected".into(), json!(sel));
    if select.is_some() {
        f.insert(format!("on:{sel}"), json!(true));
    }
    f
}

pub fn open(app: &mut PhotospliceApp, select: Option<&str>) -> Option<u64> {
    // An open Free Transform box is applied first, as in Photoshop (#4821): the canvas shows the
    // box's preview while it is open, so the dialog's preview would not show until Enter.
    if app.ui.transform.is_some() {
        crate::transform_tool::commit(app);
        if app.ui.transform.is_some() && crate::transform_tool::committing(app).is_none() {
            return None; // refused: the box stays open, with the reason in the status bar
        }
    }
    let st = app.session.active()?;
    let layer = st.doc.layer(st.active_layer?)?.clone();
    let light = st.doc.global_light.angle;
    let mut f = initial_fields(&layer, st.doc.mode, select, light);
    f.insert("patternList".into(), pattern_list(app));
    Some(app.ui.open_dialog(crate::state::DialogKind::LayerStyle, f))
}

/// Double-click on an effect row of the Layers panel: open Layer Style for that row's layer. A
/// row of another layer selects it first, as its right-click menu does (Photoshop does the same).
pub fn open_for_layer(app: &mut PhotospliceApp, layer: photosplice_doc::LayerId, select: Option<&str>) -> Option<u64> {
    let active = app.session.active().is_some_and(|s| s.active_layer == Some(layer));
    if !active {
        app.run("layer.select", json!({"layer": layer.0})).ok()?;
    }
    open(app, select)
}

/// Sets `key` in row `row`'s params, sharing the angle between globally lit rows
/// ([`share_global_light`]).
fn set_row_param(f: &mut Map<String, Value>, row: &str, key: &str, value: Value) {
    let Some(p) = f.get_mut(&format!("p:{row}")).and_then(Value::as_object_mut) else { return };
    p.insert(key.into(), value);
    share_global_light(f, row, key);
}

/// Every row that uses the global light shows one angle, as in Photoshop (off rows too): after
/// `key` changed on `row`, an edited angle becomes theirs, and a row switched to Use Global Light
/// takes the angle the others share (else the document's).
fn share_global_light(f: &mut Map<String, Value>, row: &str, key: &str) {
    let global = |v: &Value| v.get("useGlobalLight").and_then(Value::as_bool) == Some(true);
    let me = format!("p:{row}");
    let Some(p) = f.get(&me).filter(|p| global(p)) else { return };
    let angle = match key {
        "angle" => p.get("angle").cloned(),
        "useGlobalLight" => f
            .iter()
            .filter(|(k, v)| k.starts_with("p:") && **k != me && global(v))
            .find_map(|(_, v)| v.get("angle").cloned())
            .or_else(|| f.get("globalLight").cloned()),
        _ => None,
    };
    let Some(angle) = angle.filter(|a| a.as_f64().is_some_and(f64::is_finite)) else { return };
    for (k, v) in f.iter_mut() {
        if k.starts_with("p:")
            && global(v)
            && let Some(o) = v.as_object_mut()
        {
            o.insert("angle".into(), angle.clone());
        }
    }
}

/// Whether the selected page is a Drop Shadow or Inner Shadow, which a drag on the image moves
/// while the dialog is open, as in Photoshop (#5701; [`drag_shadow`]).
pub fn drags_shadow(f: &Map<String, Value>) -> bool {
    let selected = f.get("selected").and_then(Value::as_str).unwrap_or_default();
    matches!(base_kind(selected), "dropShadow" | "innerShadow") && f.get(&format!("p:{selected}")).is_some_and(Value::is_object)
}

/// A drag on the image with a shadow page selected: `start` is true on the press (it records the
/// shadow's angle and distance then), `total` the pointer's movement since in document pixels.
/// The shadow's offset follows the pointer, kept as a whole-degree angle and whole-pixel distance
/// (offset = (−cos angle, sin angle) × distance, the light opposite the shadow); with Use Global
/// Light the shared angle moves too.
pub fn drag_shadow(f: &mut Map<String, Value>, start: bool, total: [f64; 2]) {
    const KEY: &str = "__shadowDrag";
    let Some(row) = f.get("selected").and_then(Value::as_str).map(str::to_string).filter(|_| drags_shadow(f)) else { return };
    let Some(p) = f.get(&format!("p:{row}")) else { return };
    if start || f.get(KEY).is_none() {
        let angle = p.get("angle").and_then(Value::as_f64).unwrap_or(120.0);
        let distance = p.get("distance").and_then(Value::as_f64).unwrap_or(0.0);
        f.insert(KEY.into(), json!([angle, distance]));
    }
    let Some([angle, distance]) = f.get(KEY).and_then(Value::as_array).and_then(|a| Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?])) else { return };
    let a = angle.to_radians();
    let (ox, oy) = (-a.cos() * distance + total[0], a.sin() * distance + total[1]);
    let d = ox.hypot(oy);
    if !d.is_finite() {
        return;
    }
    let new_angle = if d >= 0.5 { oy.atan2(-ox).to_degrees().round() } else { angle };
    set_row_param(f, &row, "angle", json!(new_angle));
    set_row_param(f, &row, "distance", json!(d.round().clamp(0.0, 30000.0)));
}

/// `[[id, name], …]` of the patterns a style can use (document's, then the library's).
pub fn pattern_list(app: &PhotospliceApp) -> Value {
    let mut out: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let doc_pats = app.session.active().map(|d| d.doc.patterns.clone()).unwrap_or_default();
    for p in doc_pats.iter().chain(app.session.patterns.items.iter()) {
        if seen.insert(p.id.clone()) {
            out.push(json!([p.id, p.display_name()]));
        }
    }
    Value::Array(out)
}

/// Apply the dialog: replace the layer's effects with the enabled ones.
pub fn confirm(app: &mut PhotospliceApp, f: &Map<String, Value>) -> Result<Value, String> {
    apply(f, |id, p| app.run(id, p))
}

/// Every parameter group is edited as an object. Validate before preview or confirmation so
/// malformed values supplied through `ui.dialog.set` are reported before any command runs.
fn validate_params(f: &Map<String, Value>) -> Result<(), String> {
    for (key, value) in f {
        if key.starts_with("p:") && !value.is_object() {
            return Err(format!("invalid layer style parameters: `{key}` must be an object"));
        }
    }
    Ok(())
}

/// Runs the dialog's commands through `run`: blending options, then the layer's whole effect
/// list in one step (`layer.layerStyle.replace`): every row that is on, and the loaded effects
/// that are off (kept, switched off, as Photoshop keeps them).
fn apply(f: &Map<String, Value>, mut run: impl FnMut(&str, Value) -> Result<Value, String>) -> Result<Value, String> {
    validate_params(f)?;
    let layer = f.get("layer").cloned().unwrap_or(Value::Null);
    let initial_light_angle = f.get("globalLight").and_then(Value::as_f64);
    if let Some(bo) = f.get(&format!("p:{BLENDING}")).and_then(Value::as_object) {
        let mut p = Value::Object(bo.clone());
        if let Some(params) = p.as_object_mut() {
            params.insert("layer".into(), layer.clone());
        }
        run("layer.layerStyle.blendingOptions", p)?;
    }
    // An effect lit by the global light follows the document's light angle, so the Angle slider
    // drives that shared angle rather than the per-effect angle the compositor ignores.
    let mut light_angle: Option<f64> = None;
    let mut effects = Vec::new();
    for &(kind, _) in KINDS {
        for row in rows(f, kind) {
            let on = f.get(&format!("on:{row}")).and_then(Value::as_bool) == Some(true);
            let loaded = f.get(&format!("fx:{row}")).filter(|v| v.is_object());
            if !on && loaded.is_none() {
                continue;
            }
            let p = f.get(&format!("p:{row}")).cloned().unwrap_or_else(|| json!({}));
            if on
                && p.get("useGlobalLight").and_then(Value::as_bool) == Some(true)
                && let Some(angle) = p.get("angle").and_then(Value::as_f64)
                && initial_light_angle.is_none_or(|initial| angle != initial)
            {
                light_angle = Some(angle);
            }
            let mut params = match loaded {
                Some(fx) => changed_params(fx, &p, initial_light_angle.unwrap_or(120.0) as f32),
                None => p,
            };
            if let Some(o) = params.as_object_mut() {
                o.insert("enabled".into(), json!(on));
            }
            let mut entry = json!({"kind": kind, "params": params});
            if let Some(fx) = loaded {
                entry["fx"] = fx.clone();
            }
            effects.push(entry);
        }
    }
    run("layer.layerStyle.replace", json!({"layer": layer, "effects": effects}))?;
    if let Some(angle) = light_angle {
        run("layer.layerStyle.globalLight", json!({"angle": angle}))?;
    }
    Ok(Value::Null)
}

/// Hash of the fields that change the rendered style (not the selected page).
pub fn preview_hash(f: &Map<String, Value>) -> u64 {
    f.iter()
        .filter(|(k, _)| k.as_str() == "layer" || ["on:", "p:", "fx:", "rows:"].iter().any(|p| k.starts_with(p)))
        .flat_map(|(k, v)| k.bytes().chain(v.to_string().into_bytes()))
        .fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

/// `doc` with the dialog's style applied, run on a scratch session (no history) for the live preview.
pub fn preview_document(
    doc: &photosplice_doc::Document,
    patterns: &photosplice_engine::pattern_cmds::PatternLibrary,
    f: &Map<String, Value>,
) -> Result<photosplice_doc::Document, String> {
    let mut s = photosplice_engine::Session::new();
    s.patterns = patterns.clone();
    s.add_document(doc.clone(), None);
    apply(f, |id, p| s.execute(id, p).map_err(|e| e.to_string()))?;
    s.active().map(|d| (*d.doc).clone()).ok_or_else(|| "no preview document".into())
}

/// Dialog body (left list, right parameters).
pub fn body(ui: &mut egui::Ui, f: &mut Map<String, Value>, patterns: &photosplice_engine::pattern_cmds::PatternLibrary, styles: &[StyleChoice]) {
    let t = Tokens::get(ui.ctx());
    let selected = f.get("selected").and_then(Value::as_str).unwrap_or("dropShadow").to_string();
    let invalid = validate_params(f).err();
    // Column separators: gaps laid out with the columns, drawn afterwards over the row's full height.
    let mut seps = Vec::new();
    let gap = |ui: &mut egui::Ui| ui.allocate_exact_size(vec2(17.0, 1.0), Sense::hover()).0.center().x;
    let row = ui.horizontal_top(|ui| {
        // The same height on every page, so switching pages doesn't resize the dialog.
        ui.set_min_height(BODY_HEIGHT);
        // Left: effect list.
        ui.vertical(|ui| {
            ui.set_width(190.0);
            ui.label(RichText::new(tl!("Styles")).color(t.text_faint));
            // Blending Options page (layer blend mode, opacity and fill opacity).
            let bo = ui.add(
                egui::Label::new(RichText::new(tl!("Blending Options")).color(if selected == BLENDING { t.text } else { t.text_dim })).sense(Sense::click()),
            );
            if bo.clicked() {
                f.insert("selected".into(), json!(BLENDING));
            }
            ui.add_space(4.0);
            let mut action = None;
            // Grows with added instances; the up/down/delete buttons stay below it.
            egui::ScrollArea::vertical().id_salt("fx-list").max_height(BODY_HEIGHT - 96.0).auto_shrink([false, true]).show(ui, |ui| {
                for &(kind, label) in KINDS {
                    let keys = rows(f, kind);
                    let can_add = MULTI.contains(&kind) && keys.len() < MAX_INSTANCES;
                    for row in &keys {
                        let (rect, resp) = ui.allocate_exact_size(vec2(190.0, 26.0), Sense::click());
                        let is_sel = *row == selected;
                        if is_sel {
                            ui.painter().rect_filled(rect, t.radius_sm, t.row_selected.gamma_multiply(if t.pro { 1.0 } else { 0.0 }).max_alpha(t.hover));
                        } else if resp.hovered() {
                            ui.painter().rect_filled(rect, t.radius_sm, t.hover.gamma_multiply(0.5));
                        }
                        let key = format!("on:{row}");
                        let mut on = f.get(&key).and_then(Value::as_bool).unwrap_or(false);
                        let cb = egui::Rect::from_min_size(rect.min + vec2(6.0, 6.0), vec2(14.0, 14.0));
                        let cresp = ui.interact(cb, ui.id().with(("fxcb", row)), Sense::click());
                        // "+": another instance of this kind, with this one's settings, above it.
                        let plus = can_add.then(|| {
                            let r = egui::Rect::from_center_size(rect.right_center() - vec2(14.0, 0.0), vec2(20.0, 20.0));
                            let presp =
                                ui.interact(r, ui.id().with(("fxadd", row)), Sense::click()).on_hover_text(format!("{} {}", tl!("Add another"), tl!(label)));
                            if presp.hovered() {
                                ui.painter().rect_filled(r, t.radius_sm, t.hover);
                            }
                            crate::icons::paint(ui, r, "ui-plus", 14.0, if presp.hovered() { t.text } else { t.text_dim });
                            presp
                        });
                        if on {
                            ui.painter().rect_filled(cb, 2.0, t.accent);
                            ui.painter()
                                .line_segment([cb.left_center() + vec2(3.0, 0.5), cb.center_bottom() + vec2(-1.0, -3.5)], Stroke::new(1.8, Color32::WHITE));
                            ui.painter()
                                .line_segment([cb.center_bottom() + vec2(-1.0, -3.5), cb.right_top() + vec2(-3.0, 3.5)], Stroke::new(1.8, Color32::WHITE));
                        } else {
                            ui.painter().rect_stroke(cb, 2.0, Stroke::new(1.5, t.text_faint), StrokeKind::Inside);
                        }
                        ui.painter().text(
                            rect.left_center() + vec2(28.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            tl!(label),
                            egui::FontId::proportional(12.5),
                            fx_label_color(on, &t),
                        );
                        if plus.is_some_and(|p| p.clicked()) {
                            action = Some(RowAction::Add(row.clone()));
                        } else if cresp.clicked() {
                            on = !on;
                            f.insert(key, json!(on));
                            f.insert("selected".into(), json!(row));
                        } else if resp.clicked() {
                            f.insert("selected".into(), json!(row));
                            f.insert(key, json!(true));
                        }
                    }
                }
            });
            // Photoshop's buttons under the list: move the selected instance within its kind
            // (the top one paints over the others), or delete it while others are left.
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let keys = rows(f, base_kind(&selected));
                let at = keys.iter().position(|k| *k == selected);
                let buttons = [
                    ("chevron-up", "Move Effect Up", at.is_some_and(|i| i > 0), RowAction::Up),
                    ("chevron-down", "Move Effect Down", at.is_some_and(|i| i + 1 < keys.len()), RowAction::Down),
                    ("trash", "Delete Effect", at.is_some() && keys.len() > 1, RowAction::Delete),
                ];
                for (icon, tip, enabled, act) in buttons {
                    let r = ui.add_enabled_ui(enabled, |ui| crate::icons::button(ui, icon, 24.0, false, tl!(tip))).inner;
                    if r.clicked() {
                        action = Some(act);
                    }
                }
            });
            if let Some(a) = action {
                row_action(f, &selected, a);
            }
        });
        seps.push(gap(ui));
        // Right: parameters of the selected effect.
        ui.vertical(|ui| {
            ui.set_width(330.0);
            let kind = base_kind(&selected);
            let label = KINDS.iter().find(|k| k.0 == kind).map(|k| k.1).unwrap_or(if selected == BLENDING { "Blending Options" } else { "" });
            ui.label(RichText::new(tl!(&label)).font(crate::theme::semibold(14.0)).color(t.text));
            ui.add_space(6.0);
            let pkey = format!("p:{selected}");
            // Only reachable through `ui.dialog.set`: say what is wrong once, keep the bad value.
            if let Some(error) = &invalid {
                ui.colored_label(t.danger, error);
            }
            let params = f.get(&pkey).cloned().unwrap_or_else(|| defaults(kind));
            // A group that isn't an object keeps its value and draws no controls.
            if let Some(mut p) = params.as_object().map(|o| Value::Object(o.clone())) {
                let before = p.clone();
                for &(key, label, kind) in spec(kind) {
                    match (selected.as_str(), key) {
                        (BLENDING, "blend") => group(ui, "General Blending", false),
                        (BLENDING, "fillOpacity") => group(ui, "Advanced Blending", true),
                        _ => {}
                    }
                    match kind {
                        P::Slider(min, max, unit) => {
                            let mut v = p.get(key).and_then(Value::as_f64).unwrap_or(min as f64) as f32;
                            if widgets::slider_row(ui, label, &mut v, min..=max, unit, None).changed() {
                                p[key] = json!(v.round());
                            }
                        }
                        P::Color => {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(tl!(&label)).color(t.text_dim));
                                let hexs = p.get(key).and_then(Value::as_str).unwrap_or("#000000").to_string();
                                let mut c = parse_hex(&hexs);
                                if ui.color_edit_button_srgba(&mut c).changed() {
                                    p[key] = json!(format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b()));
                                }
                            });
                        }
                        P::Blend => {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(tl!(&label)).color(t.text_dim));
                                let mut cur = p.get(key).and_then(Value::as_str).unwrap_or("Normal").to_string();
                                // The reference's modes, plus the layer's own Paint.NET mode when it
                                // has one (an imported .pdn), so the list shows what it is.
                                let own = photosplice_color::BlendMode::PAINT_NET_MODES.into_iter().find(|m| m.label() == cur);
                                let opts: Vec<(String, &str)> =
                                    photosplice_color::BlendMode::LAYER_MODES.into_iter().chain(own).map(|m| (m.label().to_string(), m.label())).collect();
                                if widgets::dropdown(ui, &format!("fx-blend-{selected}"), &mut cur, &opts, 150.0) {
                                    p[key] = json!(cur);
                                }
                            });
                        }
                        P::Choice(options) => {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(tl!(&label)).color(t.text_dim));
                                let mut cur = p.get(key).and_then(Value::as_str).unwrap_or(options[0].0).to_string();
                                let opts: Vec<(String, &str)> = options.iter().map(|(v, l)| (v.to_string(), *l)).collect();
                                if widgets::dropdown(ui, &format!("fx-{selected}-{key}"), &mut cur, &opts, 150.0) {
                                    p[key] = json!(cur);
                                }
                            });
                        }
                        P::Pattern => {
                            let list: Vec<(String, String)> = f
                                .get("patternList")
                                .and_then(Value::as_array)
                                .map(|a| a.iter().filter_map(|e| Some((e.get(0)?.as_str()?.to_string(), e.get(1)?.as_str()?.to_string()))).collect())
                                .unwrap_or_default();
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(tl!(&label)).color(t.text_dim));
                                let mut cur = p
                                    .get(key)
                                    .and_then(Value::as_str)
                                    .filter(|c| !c.is_empty())
                                    .map(str::to_string)
                                    .or_else(|| list.first().map(|l| l.0.clone()))
                                    .unwrap_or_default();
                                let opts: Vec<(String, &str)> = list.iter().map(|(id, n)| (id.clone(), n.as_str())).collect();
                                if widgets::dropdown(ui, &format!("fx-{selected}-{key}"), &mut cur, &opts, 180.0)
                                    || p.get(key).and_then(Value::as_str).is_none_or(str::is_empty)
                                {
                                    p[key] = json!(cur);
                                }
                            });
                        }
                        P::Check => {
                            let mut b = p.get(key).and_then(Value::as_bool).unwrap_or(false);
                            if widgets::checkbox(ui, &mut b, label).changed() {
                                p[key] = json!(b);
                            }
                        }
                    }
                    ui.add_space(2.0);
                }
                if selected == BLENDING {
                    advanced_blending(ui, f, &mut p);
                }
                let shared = ["angle", "useGlobalLight"].into_iter().find(|k| before.get(*k) != p.get(*k));
                f.insert(pkey, p);
                if let Some(key) = shared {
                    share_global_light(f, &selected, key);
                }
            }
        });
        seps.push(gap(ui));
        // Preview: the live canvas preview, and a swatch of the style on a grey square.
        ui.vertical(|ui| {
            ui.set_width(SWATCH);
            let mut on = f.get("__preview").and_then(Value::as_bool).unwrap_or(true);
            if widgets::checkbox(ui, &mut on, "Preview").changed() {
                f.insert("__preview".into(), json!(on));
            }
            ui.add_space(6.0);
            swatch(ui, f, patterns);
            // The Styles panel's styles, beside the swatch: picking one loads it.
            ui.horizontal(|ui| {
                ui.label(RichText::new(tl!("Styles")).color(t.text_dim).size(11.0));
                let (cr, cresp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
                if cresp.hovered() {
                    ui.painter().rect_filled(cr, t.radius_sm, t.hover);
                }
                widgets::chevron_icon(ui, cr, ui.style().interact(&cresp), false);
                let mut picked = None;
                egui::Popup::from_toggle_button_response(&cresp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                    const THUMB: f32 = 36.0;
                    ui.set_width(6.0 * (THUMB + 4.0));
                    if styles.is_empty() {
                        ui.label(RichText::new(tl!("No styles (Window › Styles)")).color(t.text_dim));
                    }
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        let mut start = 0;
                        while start < styles.len() {
                            let group = styles[start].group.as_str();
                            let end = styles[start..].iter().position(|st| st.group != group).map_or(styles.len(), |n| start + n);
                            ui.label(RichText::new(group).color(t.text_dim).size(11.0));
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                                for (i, st) in styles.iter().enumerate().take(end).skip(start) {
                                    let (r, resp) = ui.allocate_exact_size(vec2(THUMB, THUMB), Sense::click());
                                    ui.painter().rect_filled(r, 2.0, t.field);
                                    ui.painter().image(st.tex.id(), r, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), Color32::WHITE);
                                    if resp.hovered() {
                                        ui.painter().rect_stroke(r, 2.0, Stroke::new(1.5, t.accent), StrokeKind::Inside);
                                    }
                                    if resp.on_hover_text(&st.name).clicked() {
                                        picked = Some(i);
                                    }
                                }
                            });
                            start = end;
                        }
                    });
                });
                if let Some(st) = picked.and_then(|i| styles.get(i)) {
                    load_style(f, st);
                    egui::Popup::close_all(ui.ctx());
                }
            });
        });
    });
    let r = row.response.rect;
    for x in seps {
        ui.painter().line_segment([egui::pos2(x, r.top()), egui::pos2(x, r.bottom())], Stroke::new(1.0, t.separator));
    }
}

/// Disabled effects keep their muted labels even when their page stays selected: selecting a
/// page is distinct from enabling its effect (#2288).
fn fx_label_color(enabled: bool, t: &Tokens) -> Color32 {
    if enabled { t.text } else { t.text_dim }
}

/// What the effect list's "+" and its buttons below do to a row.
#[derive(Clone, Debug, PartialEq)]
enum RowAction {
    /// Another instance of the row's kind, a copy of it, placed above it and selected.
    Add(String),
    Up,
    Down,
    Delete,
}

/// Applies `a` to the effect rows; `selected` is the selected row (Up, Down and Delete act on it).
fn row_action(f: &mut Map<String, Value>, selected: &str, a: RowAction) {
    let target = match &a {
        RowAction::Add(row) => row.as_str(),
        _ => selected,
    };
    let kind = base_kind(target).to_string();
    let mut keys = rows(f, &kind);
    let Some(at) = keys.iter().position(|k| k == target) else { return };
    match a {
        RowAction::Add(_) => {
            if !MULTI.contains(&kind.as_str()) || keys.len() >= MAX_INSTANCES {
                return;
            }
            let n = keys.iter().filter_map(|k| k.split_once('#')?.1.parse::<usize>().ok()).max().map_or(1, |n| n.saturating_add(1));
            let new = format!("{kind}#{n}");
            for prefix in ["p:", "fx:"] {
                if let Some(v) = f.get(&format!("{prefix}{target}")).cloned() {
                    f.insert(format!("{prefix}{new}"), v);
                }
            }
            f.insert(format!("on:{new}"), json!(true));
            keys.insert(at, new.clone());
            f.insert("selected".into(), json!(new));
        }
        RowAction::Up if at > 0 => keys.swap(at, at - 1),
        RowAction::Down if at + 1 < keys.len() => keys.swap(at, at + 1),
        RowAction::Delete if keys.len() > 1 => {
            keys.remove(at);
            for prefix in ["on:", "p:", "fx:"] {
                f.remove(&format!("{prefix}{target}"));
            }
            if let Some(next) = keys.get(at.min(keys.len() - 1)) {
                f.insert("selected".into(), json!(next));
            }
        }
        _ => return,
    }
    f.insert(format!("rows:{kind}"), json!(keys));
}

/// Height of the dialog body: the tallest page (Blending Options in an RGB document).
const BODY_HEIGHT: f32 = 500.0;

/// A group heading inside a page (Photoshop's General Blending, Advanced Blending, …), with a
/// rule above it unless it opens the page.
fn group(ui: &mut egui::Ui, title: &str, rule: bool) {
    if rule {
        ui.add_space(6.0);
        widgets::hairline(ui);
        ui.add_space(8.0);
    }
    widgets::section_label(ui, title);
    ui.add_space(4.0);
}

/// Side of the style swatch, in points (and in pixels of its scratch document).
const SWATCH: f32 = 96.0;

/// The swatch: the dialog's effects, opacity and fill on a grey square, re-rendered when they change.
fn swatch(ui: &mut egui::Ui, f: &Map<String, Value>, patterns: &photosplice_engine::pattern_cmds::PatternLibrary) {
    let t = Tokens::get(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(vec2(SWATCH, SWATCH), Sense::hover());
    ui.painter().rect_filled(rect, t.radius_sm, t.field);
    let id = egui::Id::new("layer-style-swatch");
    let key = preview_hash(f);
    let cached = ui.ctx().data(|d| d.get_temp::<(u64, Option<egui::TextureHandle>)>(id)).filter(|c| c.0 == key);
    let tex = match cached {
        Some((_, tex)) => tex,
        None => {
            let tex = swatch_image(f, patterns).map(|img| ui.ctx().load_texture("layer-style-swatch", img, egui::TextureOptions::LINEAR));
            ui.ctx().data_mut(|d| d.insert_temp(id, (key, tex.clone())));
            tex
        }
    };
    if let Some(tex) = tex {
        ui.painter().image(tex.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
    }
    ui.painter().rect_stroke(rect, t.radius_sm, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
}

/// Renders the swatch: a mid-grey square on a transparent RGB document, with the dialog's style.
/// The layer's blend mode, Channels and Blend If are left out: with nothing beneath they show nothing.
fn swatch_image(f: &Map<String, Value>, patterns: &photosplice_engine::pattern_cmds::PatternLibrary) -> Option<egui::ColorImage> {
    let side = SWATCH as u32;
    let mut doc = photosplice_doc::Document::new("Swatch", photosplice_geom::Size::new(side, side), ColorMode::Rgb, photosplice_color::SampleType::U8);
    let mut surface = photosplice_raster::Surface::new(doc.pixel_format());
    let q = (side / 4) as i32;
    surface.fill_rect(photosplice_geom::Rect::new(q, q, side as i32 - q, side as i32 - q), &[0.5, 0.5, 0.5, 1.0]);
    let layer = Layer::new("Swatch", photosplice_doc::LayerContent::Raster(surface));
    let lid = layer.id.0;
    doc.layers.push(layer);
    let mut sf = f.clone();
    sf.insert("layer".into(), json!(lid));
    if let Some(Value::Object(bo)) = sf.get_mut(&format!("p:{BLENDING}")) {
        for k in ["blend", "channels", "blendIf"] {
            bo.remove(k);
        }
    }
    let shown = preview_document(&doc, patterns, &sf).ok()?;
    Some(crate::canvas::buffer_to_image(&photosplice_compose::flatten(&shown)))
}

/// Blending Options › Advanced Blending (Channels) and Blend If, for the modes the compositor applies them to.
fn advanced_blending(ui: &mut egui::Ui, f: &mut Map<String, Value>, p: &mut Value) {
    let t = Tokens::get(ui.ctx());
    let Some(mode) = f.get("__mode").and_then(Value::as_str).map(str::to_string) else { return };
    let names: &[(&str, &str)] = if mode == "gray" { &[("Gray", "Gray")] } else { &[("R", "Red"), ("G", "Green"), ("B", "Blue")] };
    ui.horizontal(|ui| {
        ui.label(RichText::new(tl!("Channels:")).color(t.text_dim));
        for (i, (short, _)) in names.iter().enumerate() {
            let mut on = p.get("channels").and_then(|c| c.get(i)).and_then(Value::as_bool).unwrap_or(true);
            if widgets::checkbox(ui, &mut on, short).changed()
                && let Some(c) = p.get_mut("channels").and_then(|c| c.get_mut(i))
            {
                *c = json!(on);
            }
        }
    });
    ui.add_space(6.0);
    widgets::hairline(ui);
    ui.add_space(8.0);
    // Dropdown entries: (position in the `blendIf` list, label). Gray documents edit entry 1.
    let opts: Vec<(usize, &str)> =
        if mode == "gray" { vec![(1, "Gray")] } else { std::iter::once((0, "Gray")).chain(names.iter().enumerate().map(|(i, n)| (i + 1, n.1))).collect() };
    let mut ch = f.get("__blendIfChannel").and_then(Value::as_u64).and_then(|c| usize::try_from(c).ok()).unwrap_or(0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(tl!("Blend If:")).color(t.text_dim));
        if widgets::dropdown(ui, "fx-blendIf-channel", &mut ch, &opts, 150.0) {
            f.insert("__blendIfChannel".into(), json!(ch));
        }
    });
    ui.add_space(4.0);
    let ramp = match (mode.as_str(), ch) {
        ("rgb", 1) => Color32::from_rgb(255, 0, 0),
        ("rgb", 2) => Color32::from_rgb(0, 255, 0),
        ("rgb", 3) => Color32::from_rgb(0, 0, 255),
        _ => Color32::WHITE,
    };
    for (key, label) in [("thisLayer", "This Layer:"), ("underlying", "Underlying Layer:")] {
        let Some(slot) = p.get_mut("blendIf").and_then(|b| b.get_mut(ch)).and_then(|e| e.get_mut(key)) else { continue };
        let mut q = range_bytes(slot);
        if blend_if_slider(ui, key, label, &mut q, ramp) {
            *slot = json!(q);
        }
    }
    ui.label(RichText::new(tl!("Alt-drag a point to split it.")).color(t.text_faint).size(11.0));
}

/// `[blackLow, blackHigh, whiteLow, whiteHigh]` from a field, full range when malformed.
fn range_bytes(v: &Value) -> [u8; 4] {
    let a: Vec<u8> = v.as_array().map(|a| a.iter().filter_map(|x| x.as_u64().and_then(|x| u8::try_from(x).ok())).collect()).unwrap_or_default();
    match a.as_slice() {
        [a, b, c, d] if a <= b && b <= c && c <= d => [*a, *b, *c, *d],
        _ => BlendRange::FULL.to_bytes(),
    }
}

/// Which handle a Blend If drag holds: `point` 0 = black, 1 = white; `half` None = both halves,
/// Some(0|1) = the low or high half, Some(2) = split, half picked by the first move's direction.
#[derive(Clone, Copy)]
struct BlendIfDrag {
    point: usize,
    half: Option<usize>,
    start: [u8; 4],
    x: f32,
}

/// The values after dragging `drag`'s handle by `d` (0..=255 units) from where the drag started.
fn drag_points(drag: BlendIfDrag, d: i32) -> [u8; 4] {
    let s = drag.start.map(i32::from);
    let mut n = s;
    let point = drag.point.min(1);
    let (lo, hi) = (2 * point, 2 * point + 1);
    // Bounds of the point being moved: the black point can't pass the white one.
    let (min, max) = if point == 0 { (0, s[2]) } else { (s[1], 255) };
    let half = match drag.half {
        Some(2) if d != 0 => Some(usize::from(d > 0)),
        h => h,
    };
    match half {
        None => {
            let d = d.max(min - s[lo]).min(max - s[hi]);
            n[lo] = s[lo] + d;
            n[hi] = s[hi] + d;
        }
        Some(0) => n[lo] = (s[lo] + d).max(min).min(s[hi]),
        Some(1) => n[hi] = (s[hi] + d).max(s[lo]).min(max),
        Some(_) => {}
    }
    n.map(|v| v.clamp(0, 255) as u8)
}

/// Photoshop's Blend If slider: values above a gradient bar, a black and a white point below it.
/// Dragging a point moves both its halves; Alt-dragging moves one half, splitting the point.
fn blend_if_slider(ui: &mut egui::Ui, id: &str, label: &str, q: &mut [u8; 4], ramp: Color32) -> bool {
    let t = Tokens::get(ui.ctx());
    let show = |a: u8, b: u8| if a == b { a.to_string() } else { format!("{a}/{b}") };
    // Full width, inset like the sliders' tracks so the handles line up with their knobs.
    let width = ui.available_width().max(60.0);
    // Photoshop's header: the label right-aligned in a column as wide as the longest one, the
    // black point's value then the white point's at fixed places after it.
    let font = egui::FontId::proportional(12.0);
    let (head, _) = ui.allocate_exact_size(vec2(width, 18.0), Sense::hover());
    let column = ui.painter().layout_no_wrap(tl!("Underlying Layer:").to_string(), font.clone(), t.text_dim).size().x;
    let label_end = head.left() + 7.0 + column;
    let y = head.center().y;
    ui.painter().text(egui::pos2(label_end, y), egui::Align2::RIGHT_CENTER, tl!(label), font.clone(), t.text_dim);
    let span = (head.right() - 7.0 - label_end).max(0.0);
    ui.painter().text(egui::pos2(label_end + 0.16 * span, y), egui::Align2::CENTER_CENTER, show(q[0], q[1]), font.clone(), t.text);
    ui.painter().text(egui::pos2(label_end + 0.55 * span, y), egui::Align2::CENTER_CENTER, show(q[2], q[3]), font, t.text);
    ui.add_space(2.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 24.0), Sense::drag());
    let bar = egui::Rect::from_min_size(rect.min + vec2(7.0, 0.0), vec2((rect.width() - 14.0).max(1.0), 12.0));
    let x_of = |v: u8| bar.left() + bar.width() * f32::from(v) / 255.0;
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(bar.left_top(), Color32::BLACK);
    mesh.colored_vertex(bar.right_top(), ramp);
    mesh.colored_vertex(bar.right_bottom(), ramp);
    mesh.colored_vertex(bar.left_bottom(), Color32::BLACK);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    ui.painter().add(mesh);
    ui.painter().rect_stroke(bar, 0.0, Stroke::new(1.0, t.field_border), StrokeKind::Outside);
    let mut changed = false;
    let mem = ui.id().with(("blend-if-drag", id));
    if resp.drag_started()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let centre = |p: usize| (x_of(q[2 * p]) + x_of(q[2 * p + 1])) / 2.0;
        let point = usize::from((pos.x - centre(1)).abs() < (pos.x - centre(0)).abs());
        let half = ui.input(|i| i.modifiers.alt).then(|| {
            let (lo, hi) = (q[2 * point], q[2 * point + 1]);
            if lo == hi { 2 } else { usize::from((pos.x - x_of(hi)).abs() < (pos.x - x_of(lo)).abs()) }
        });
        ui.data_mut(|d| d.insert_temp(mem, BlendIfDrag { point, half, start: *q, x: pos.x }));
    }
    if resp.dragged()
        && let (Some(drag), Some(pos)) = (ui.data(|d| d.get_temp::<BlendIfDrag>(mem)), resp.interact_pointer_pos())
    {
        let d = ((pos.x - drag.x) / bar.width().max(1.0) * 255.0).round() as i32;
        let n = drag_points(drag, d);
        if n != *q {
            *q = n;
            changed = true;
        }
    }
    // Handles: a whole triangle per point, or a left and a right half when it's split.
    let y = bar.bottom() + 2.0;
    for (point, fill) in [(0, Color32::from_gray(20)), (1, Color32::from_gray(235))] {
        let (lo, hi) = (x_of(q[2 * point]), x_of(q[2 * point + 1]));
        let tri = |pts: Vec<egui::Pos2>| egui::Shape::convex_polygon(pts, fill, Stroke::new(1.0, t.text_dim));
        if q[2 * point] == q[2 * point + 1] {
            ui.painter().add(tri(vec![egui::pos2(lo, y), egui::pos2(lo + 5.0, y + 9.0), egui::pos2(lo - 5.0, y + 9.0)]));
        } else {
            ui.painter().add(tri(vec![egui::pos2(lo, y), egui::pos2(lo, y + 9.0), egui::pos2(lo - 5.0, y + 9.0)]));
            ui.painter().add(tri(vec![egui::pos2(hi, y), egui::pos2(hi + 5.0, y + 9.0), egui::pos2(hi, y + 9.0)]));
        }
    }
    ui.add_space(4.0);
    changed
}

fn parse_hex(s: &str) -> Color32 {
    let s = s.trim_start_matches('#');
    let b = |i: usize| u8::from_str_radix(s.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0);
    Color32::from_rgb(b(0), b(2), b(4))
}

trait MaxAlpha {
    fn max_alpha(self, other: Color32) -> Color32;
}

impl MaxAlpha for Color32 {
    /// Fall back to `other` when this colour is fully transparent (non-Pro themes have no row colour).
    fn max_alpha(self, other: Color32) -> Color32 {
        if self.a() == 0 { other } else { self }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styled_session(effects: Value) -> (photosplice_engine::Session, Layer) {
        let mut s = photosplice_engine::Session::new();
        s.execute("file.new", json!({"width": 32, "height": 32})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        s.execute("layer.layerStyle.replace", json!({ "effects": effects })).unwrap();
        let d = s.active().unwrap();
        let layer = d.doc.layer(d.active_layer.unwrap()).unwrap().clone();
        (s, layer)
    }

    #[test]
    fn instances_and_unshown_settings_survive_ok() {
        let (mut s, layer) = styled_session(json!([
            {"kind": "stroke", "params": {"size": 7}},
            {"kind": "stroke", "params": {"size": 2, "color": "#ffffff"}},
            {"kind": "bevelEmboss", "params": {"technique": "chiselHard"}},
            {"kind": "dropShadow", "params": {"enabled": false}},
        ]));
        let mut f = initial_fields(&layer, ColorMode::Rgb, None, 120.0);
        assert_eq!(f["rows:stroke"], json!(["stroke", "stroke#1"]));
        assert_eq!(f["p:stroke#1"]["size"], json!(2.0));
        assert_eq!(f["on:dropShadow"], json!(false));
        f.get_mut("p:stroke#1").unwrap()["size"] = json!(4.0);
        apply(&f, |id, p| s.execute(id, p).map_err(|e| e.to_string())).unwrap();
        let d = s.active().unwrap();
        let fx = &d.doc.layer(layer.id).unwrap().effects.items;
        let sizes: Vec<f32> = fx.iter().filter_map(|e| if let Effect::Stroke(st) = e { Some(st.size) } else { None }).collect();
        assert_eq!(sizes, [7.0, 4.0], "both strokes, in order, the edit applied");
        assert!(
            fx.iter().any(|e| matches!(e, Effect::BevelEmboss(b) if b.technique == photosplice_doc::effects::BevelTechnique::ChiselHard)),
            "the technique the dialog doesn't show is kept: {fx:?}"
        );
        assert!(fx.iter().any(|e| matches!(e, Effect::DropShadow(_)) && !e.enabled()), "a switched-off effect stays, off");
    }

    #[test]
    fn plus_copies_an_instance_above_and_the_buttons_reorder_and_delete() {
        let (_, layer) = styled_session(json!([{"kind": "stroke", "params": {"size": 7}}]));
        let mut f = initial_fields(&layer, ColorMode::Rgb, None, 120.0);
        row_action(&mut f, "stroke", RowAction::Add("stroke".into()));
        assert_eq!(f["rows:stroke"], json!(["stroke#1", "stroke"]));
        assert_eq!((&f["selected"], &f["p:stroke#1"]["size"], &f["on:stroke#1"]), (&json!("stroke#1"), &json!(7.0), &json!(true)));
        row_action(&mut f, "stroke#1", RowAction::Down);
        assert_eq!(f["rows:stroke"], json!(["stroke", "stroke#1"]));
        row_action(&mut f, "stroke#1", RowAction::Down);
        assert_eq!(f["rows:stroke"], json!(["stroke", "stroke#1"]), "the bottom one stays");
        row_action(&mut f, "stroke", RowAction::Delete);
        assert_eq!((&f["rows:stroke"], &f["selected"]), (&json!(["stroke#1"]), &json!("stroke#1")));
        assert!(!f.contains_key("p:stroke"));
        row_action(&mut f, "stroke#1", RowAction::Delete);
        assert_eq!(f["rows:stroke"], json!(["stroke#1"]), "the last one can't be deleted");
        // Kinds Photoshop holds one of get no copies; the others stop at ten.
        row_action(&mut f, "satin", RowAction::Add("satin".into()));
        assert!(!f.contains_key("rows:satin"));
        for _ in 0..20 {
            row_action(&mut f, "stroke#1", RowAction::Add("stroke#1".into()));
        }
        assert_eq!(rows(&f, "stroke").len(), MAX_INSTANCES);
        // Rows that don't belong to the kind, or a selection that isn't a row, change nothing.
        f.insert("rows:dropShadow".into(), json!(["stroke", 5, "dropShadow#x"]));
        assert_eq!(rows(&f, "dropShadow"), ["dropShadow#x"]);
        row_action(&mut f, "nope#3", RowAction::Up);
        row_action(&mut f, "", RowAction::Delete);
    }

    #[test]
    fn several_instances_render_and_preview() {
        let (s, layer) = styled_session(json!([
            {"kind": "dropShadow", "params": {"distance": 4}},
            {"kind": "dropShadow", "params": {"distance": 9}},
        ]));
        let mut f = initial_fields(&layer, ColorMode::Rgb, None, 120.0);
        f.insert("selected".into(), json!("dropShadow#1"));
        let ctx = egui::Context::default();
        PhotospliceApp::setup_context(&ctx, crate::theme::ThemeKind::ALL[0]);
        let mut out = ctx.run_ui(Default::default(), |ui| body(ui, &mut f, &s.patterns, &[]));
        out.textures_delta.clear();
        let doc = preview_document(&s.active().unwrap().doc, &s.patterns, &f).unwrap();
        assert_eq!(doc.layer(layer.id).unwrap().effects.items.len(), 2);
    }

    #[test]
    fn every_kind_has_spec_and_defaults() {
        for &(kind, _) in KINDS {
            assert!(!spec(kind).is_empty(), "{kind}");
            let d = defaults(kind);
            for &(key, _, _) in spec(kind) {
                assert!(d.get(key).is_some(), "{kind}.{key} has no default");
            }
        }
    }

    #[test]
    fn initial_fields_select_requested_kind() {
        let l = Layer::raster("x", photosplice_doc::PixelFormat::RGBA8);
        let f = initial_fields(&l, ColorMode::Rgb, Some("stroke"), 120.0);
        assert_eq!(f["globalLight"], json!(120.0));
        assert_eq!(f["selected"], "stroke");
        assert_eq!(f["on:stroke"], true);
        assert_eq!(f["on:dropShadow"], false);
    }

    #[test]
    fn preview_applies_the_style_without_touching_the_document() {
        let mut s = photosplice_engine::Session::new();
        s.execute("file.new", json!({"width": 16, "height": 16})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        let st = s.active().unwrap();
        let mut f = initial_fields(st.doc.layer(st.active_layer.unwrap()).unwrap(), st.doc.mode, Some("colorOverlay"), st.doc.global_light.angle);
        let h = preview_hash(&f);
        f.insert("selected".into(), json!("stroke"));
        assert_eq!(preview_hash(&f), h, "switching pages doesn't re-render");
        f.insert("on:stroke".into(), json!(true));
        assert_ne!(preview_hash(&f), h);
        let shown = preview_document(&st.doc, &s.patterns, &f).unwrap();
        let fx = |d: &photosplice_doc::Document| d.layer(st.active_layer.unwrap()).unwrap().effects.items.len();
        assert_eq!((fx(&shown), fx(&st.doc)), (2, 0));
    }

    /// The dialog's Channels and Blend If fields reach the layer through the command, in RGB and gray.
    #[test]
    fn channels_and_blend_if_round_trip_through_the_engine() {
        for (mode, entry) in [("rgb", 2usize), ("grayscale", 1)] {
            let mut s = photosplice_engine::Session::new();
            s.execute("file.new", json!({"width": 16, "height": 16, "mode": mode})).unwrap();
            s.execute("layer.new.layer", json!({})).unwrap();
            let st = s.active().unwrap();
            let id = st.active_layer.unwrap();
            let mut f = initial_fields(st.doc.layer(id).unwrap(), st.doc.mode, Some(BLENDING), st.doc.global_light.angle);
            let bo = f.get_mut(&format!("p:{BLENDING}")).unwrap();
            assert_eq!(bo["channels"].as_array().unwrap().len(), st.doc.mode.color_channels(), "{mode}");
            bo["channels"][0] = json!(false);
            bo["blendIf"][entry]["thisLayer"] = json!([10, 40, 200, 255]);
            bo["blendIf"][entry]["underlying"] = json!([0, 0, 128, 128]);
            apply(&f, |c, p| s.execute(c, p).map(|_| Value::Null).map_err(|e| e.to_string())).unwrap();
            let st = s.active().unwrap();
            let l = st.doc.layer(id).unwrap();
            assert_eq!(l.excluded_channels, 1, "{mode}");
            assert_eq!(l.blend_if.get(entry), [BlendRange::from_bytes([10, 40, 200, 255]), BlendRange::from_bytes([0, 0, 128, 128])], "{mode}");
            // Reopening shows what was set.
            let again = initial_fields(l, st.doc.mode, None, st.doc.global_light.angle);
            assert_eq!(again[&format!("p:{BLENDING}")]["blendIf"][entry]["thisLayer"], json!([10, 40, 200, 255]), "{mode}");
            assert_eq!(again[&format!("p:{BLENDING}")]["channels"][0], json!(false), "{mode}");
        }
        // CMYK and Lab: the compositor doesn't apply them, so the dialog doesn't offer them.
        let l = Layer::raster("x", photosplice_doc::PixelFormat::RGBA8);
        let f = initial_fields(&l, ColorMode::Cmyk, None, 120.0);
        assert!(f[&format!("p:{BLENDING}")].get("blendIf").is_none() && !f.contains_key("__mode"));
    }

    #[test]
    fn malformed_blend_if_fields_read_as_the_full_range() {
        assert_eq!(range_bytes(&json!([1, 2, 3, 4])), [1, 2, 3, 4]);
        for bad in [json!([4, 3, 2, 1]), json!([1, 2, 3]), json!([0, 0, 0, 300]), json!("x"), Value::Null] {
            assert_eq!(range_bytes(&bad), [0, 0, 255, 255], "{bad}");
        }
    }

    #[test]
    fn blend_if_drags_move_split_and_stop_at_the_other_point() {
        let drag = |point, half, start, d| drag_points(BlendIfDrag { point, half, start, x: 0.0 }, d);
        // A plain drag moves both halves; the black point stops at the white one, and at 0.
        assert_eq!(drag(0, None, [10, 40, 200, 255], 20), [30, 60, 200, 255]);
        assert_eq!(drag(0, None, [10, 40, 200, 255], 500), [170, 200, 200, 255]);
        assert_eq!(drag(0, None, [10, 40, 200, 255], -500), [0, 30, 200, 255]);
        // Alt on an unsplit point: the first move's direction picks the half.
        assert_eq!(drag(1, Some(2), [0, 0, 255, 255], -55), [0, 0, 200, 255]);
        assert_eq!(drag(0, Some(2), [0, 0, 255, 255], 30), [0, 30, 255, 255]);
        assert_eq!(drag(0, Some(2), [0, 0, 255, 255], 0), [0, 0, 255, 255]);
        // Alt on a split half: that half only, never past its partner or the other point.
        assert_eq!(drag(0, Some(0), [10, 40, 200, 255], 100), [40, 40, 200, 255]);
        assert_eq!(drag(1, Some(0), [10, 40, 200, 255], -500), [10, 40, 40, 255]);
        assert_eq!(drag(1, Some(1), [10, 40, 200, 230], 500), [10, 40, 200, 255]);
        // Out-of-range state from a hostile field doesn't panic.
        assert_eq!(drag(7, Some(9), [255, 0, 255, 0], i32::MAX / 2), [255, 0, 255, 0]);
        for half in [None, Some(0), Some(1), Some(2)] {
            drag(0, half, [255, 0, 255, 0], 9);
            drag(1, half, [200, 100, 50, 0], -9);
        }
    }

    #[test]
    fn swatch_renders_the_style() {
        let mut s = photosplice_engine::Session::new();
        s.execute("file.new", json!({"width": 16, "height": 16})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        let st = s.active().unwrap();
        let mut f = initial_fields(st.doc.layer(st.active_layer.unwrap()).unwrap(), st.doc.mode, Some("colorOverlay"), 120.0);
        f[&format!("p:{BLENDING}")]["blend"] = json!("Pass Through");
        let img = swatch_image(&f, &s.patterns).expect("swatch");
        let c = img.pixels[img.width() * img.height() / 2 + img.width() / 2];
        assert_eq!((c.r(), c.g(), c.b()), (255, 0, 0), "the red overlay on the square");
        assert_eq!(img.pixels[0].a(), 0, "transparent around it");
    }

    #[test]
    fn preview_rejects_non_object_effect_params() {
        let mut s = photosplice_engine::Session::new();
        s.execute("file.new", json!({"width": 16, "height": 16})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        let st = s.active().unwrap();
        let mut f = initial_fields(st.doc.layer(st.active_layer.unwrap()).unwrap(), st.doc.mode, Some("colorOverlay"), st.doc.global_light.angle);
        f.insert("p:colorOverlay".into(), json!("invalid"));

        assert!(preview_document(&st.doc, &s.patterns, &f).is_err());
    }

    #[test]
    fn apply_rejects_non_object_params_before_running_commands() {
        let mut f = Map::new();
        f.insert("p:stroke".into(), json!([1, 2]));
        f.insert("on:stroke".into(), json!(true));
        let mut calls = 0;

        let result = apply(&f, |_, _| {
            calls += 1;
            Ok(Value::Null)
        });

        assert!(result.is_err());
        assert_eq!(calls, 0, "invalid params must not partially apply the style");
    }

    #[test]
    fn body_keeps_invalid_params_and_renders_without_panicking() {
        let ctx = egui::Context::default();
        PhotospliceApp::setup_context(&ctx, crate::theme::ThemeKind::ALL[0]);
        let mut fields = Map::new();
        fields.insert("selected".into(), json!("dropShadow"));
        fields.insert("p:dropShadow".into(), json!(7));

        let patterns = photosplice_engine::pattern_cmds::PatternLibrary::default();
        let mut out = ctx.run_ui(Default::default(), |ui| body(ui, &mut fields, &patterns, &[]));
        out.textures_delta.clear();

        assert_eq!(fields.get("p:dropShadow"), Some(&json!(7)));
    }

    #[test]
    fn confirm_rejects_invalid_params_and_applies_valid_effects() {
        let mut app = PhotospliceApp::new(photosplice_engine::Session::new(), Default::default());
        app.run("file.new", json!({"width": 16, "height": 16})).unwrap();
        app.run("layer.new.layer", json!({})).unwrap();
        let layer = app.session.active().unwrap().doc.layer(app.session.active().unwrap().active_layer.unwrap()).unwrap().clone();
        let mut invalid = initial_fields(&layer, ColorMode::Rgb, Some("colorOverlay"), 0.0);
        invalid.insert("p:colorOverlay".into(), json!(null));
        assert!(confirm(&mut app, &invalid).is_err());
        assert!(app.session.active().unwrap().doc.layer(layer.id).unwrap().effects.items.is_empty());

        let valid = initial_fields(&layer, ColorMode::Rgb, Some("colorOverlay"), 0.0);
        confirm(&mut app, &valid).unwrap();
        assert_eq!(app.session.active().unwrap().doc.layer(layer.id).unwrap().effects.items.len(), 1);
    }

    #[test]
    fn percent_fields_round_trip_through_the_engine() {
        let mut s = photosplice_engine::Session::new();
        s.execute("file.new", json!({"width": 64, "height": 64})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        s.execute("layer.layerStyle.outerGlow", json!({"spread": 6, "range": 40})).unwrap();
        s.execute("layer.layerStyle.dropShadow", json!({"spread": 12, "add": true})).unwrap();
        let st = s.active().unwrap();
        let l = st.doc.layer(st.active_layer.unwrap()).unwrap();
        let f = initial_fields(l, st.doc.mode, None, st.doc.global_light.angle);
        assert_eq!(f["p:outerGlow"]["spread"], json!(6.0));
        assert_eq!(f["p:outerGlow"]["range"], json!(40.0));
        assert_eq!(f["p:dropShadow"]["spread"], json!(12.0));
    }

    #[test]
    fn drop_shadow_angle_reaches_the_effect() {
        // #350: the Angle slider was dead — the dialog never sent `useGlobalLight`, so the engine
        // defaulted it to true and the compositor used the fixed global light angle, ignoring the slider.
        let mut s = photosplice_engine::Session::new();
        s.execute("file.new", json!({"width": 64, "height": 64})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        let id = s.active().unwrap().active_layer.unwrap();
        let shadow = |s: &photosplice_engine::Session| {
            s.active().unwrap().doc.layer(id).unwrap().effects.items.iter().find_map(|e| match e {
                Effect::DropShadow(sh) => Some(sh.clone()),
                _ => None,
            })
        };

        // Use Global Light off: the per-effect angle is stored and used.
        let mut f = Map::new();
        f.insert("layer".into(), json!(id));
        f.insert("on:dropShadow".into(), json!(true));
        f.insert("p:dropShadow".into(), json!({"angle": 45.0, "useGlobalLight": false, "distance": 5, "size": 5}));
        apply(&f, |cmd, p| s.execute(cmd, p).map_err(|e| e.to_string())).unwrap();
        let eff = shadow(&s).unwrap();
        assert!(!eff.use_global_light, "Use Global Light off keeps the per-effect angle");
        assert_eq!(eff.angle, 45.0);

        // Use Global Light on: the Angle slider drives the document's shared light angle.
        let mut f = Map::new();
        f.insert("layer".into(), json!(id));
        f.insert("on:dropShadow".into(), json!(true));
        f.insert("p:dropShadow".into(), json!({"angle": 30.0, "useGlobalLight": true, "distance": 5, "size": 5}));
        apply(&f, |cmd, p| s.execute(cmd, p).map_err(|e| e.to_string())).unwrap();
        assert_eq!(s.active().unwrap().doc.global_light.angle, 30.0, "the Angle slider moves the shared light");
        assert!(shadow(&s).unwrap().use_global_light);

        // Reopening the dialog shows the effective angle and the checkbox state.
        let st = s.active().unwrap();
        let f = initial_fields(st.doc.layer(id).unwrap(), st.doc.mode, None, st.doc.global_light.angle);
        assert_eq!(f["p:dropShadow"]["useGlobalLight"], json!(true));
        assert_eq!(f["p:dropShadow"]["angle"], json!(30.0));
    }

    #[test]
    fn bevel_angle_change_is_not_overridden_by_unchanged_drop_shadow_angle() {
        let mut s = photosplice_engine::Session::new();
        s.execute("file.new", json!({"width": 64, "height": 64})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        let (id, mut f, initial_light) = {
            let st = s.active().unwrap();
            let id = st.active_layer.unwrap();
            let f = initial_fields(st.doc.layer(id).unwrap(), st.doc.mode, None, st.doc.global_light.angle);
            (id, f, st.doc.global_light.angle)
        };
        f.insert("on:bevelEmboss".into(), json!(true));
        f["p:bevelEmboss"]["angle"] = json!(45.0);
        f.insert("on:dropShadow".into(), json!(true));
        assert_eq!(f["p:dropShadow"]["angle"].as_f64(), Some(f64::from(initial_light)));

        apply(&f, |cmd, p| s.execute(cmd, p).map_err(|e| e.to_string())).unwrap();

        assert_eq!(s.active().unwrap().doc.global_light.angle, 45.0);
        assert_eq!(s.active().unwrap().doc.layer(id).unwrap().effects.items.len(), 2);
    }

    #[test]
    fn unchanged_global_light_angle_is_not_applied() {
        let layer = Layer::raster("x", photosplice_doc::PixelFormat::RGBA8);
        let mut f = initial_fields(&layer, ColorMode::Rgb, None, 47.5);
        f.insert("on:bevelEmboss".into(), json!(true));
        assert_eq!(f["p:bevelEmboss"]["angle"].as_f64(), Some(47.5));
        let mut commands = Vec::new();

        apply(&f, |cmd, _| {
            commands.push(cmd.to_string());
            Ok(Value::Null)
        })
        .unwrap();

        assert!(!commands.iter().any(|cmd| cmd == "layer.layerStyle.globalLight"));
    }

    #[test]
    fn dragging_on_the_image_moves_the_selected_shadow() {
        let l = Layer::raster("x", photosplice_doc::PixelFormat::RGBA8);
        let mut f = initial_fields(&l, ColorMode::Rgb, Some("dropShadow"), 120.0);
        assert!(drags_shadow(&f));
        f["p:dropShadow"]["useGlobalLight"] = json!(false);
        f["p:dropShadow"]["angle"] = json!(90.0);
        f["p:dropShadow"]["distance"] = json!(10.0);
        // The shadow sits 10 px below; dragging 10 px right puts it at (10, 10).
        drag_shadow(&mut f, true, [10.0, 0.0]);
        assert_eq!((f["p:dropShadow"]["angle"].as_f64(), f["p:dropShadow"]["distance"].as_f64()), (Some(135.0), Some(14.0)));
        // Movements are measured from the press, not added up.
        drag_shadow(&mut f, false, [0.0, 0.0]);
        assert_eq!((f["p:dropShadow"]["angle"].as_f64(), f["p:dropShadow"]["distance"].as_f64()), (Some(90.0), Some(10.0)));
        drag_shadow(&mut f, false, [f64::NAN, 0.0]);
        assert_eq!(f["p:dropShadow"]["distance"].as_f64(), Some(10.0), "no NaN reaches the params");
        // With Use Global Light the shared angle moves too.
        f["p:dropShadow"]["useGlobalLight"] = json!(true);
        drag_shadow(&mut f, true, [10.0, 0.0]);
        assert_eq!(f["p:bevelEmboss"]["angle"].as_f64(), Some(135.0));
        // Other pages don't take the drag.
        f.insert("selected".into(), json!("stroke"));
        assert!(!drags_shadow(&f));
        let before = f.clone();
        drag_shadow(&mut f, true, [50.0, 50.0]);
        assert_eq!(f, before);
    }

    #[test]
    fn globally_lit_rows_share_one_angle() {
        let l = Layer::raster("x", photosplice_doc::PixelFormat::RGBA8);
        let mut f = initial_fields(&l, ColorMode::Rgb, None, 120.0);
        set_row_param(&mut f, "dropShadow", "angle", json!(30.0));
        assert_eq!(f["p:bevelEmboss"]["angle"].as_f64(), Some(30.0));
        assert_eq!(f["p:innerShadow"]["angle"].as_f64(), Some(30.0));
        assert_eq!(f["p:satin"]["angle"].as_f64(), Some(19.0), "satin has no global light");
        // A local angle stays its own; switching back to the global light takes the shared one.
        set_row_param(&mut f, "innerShadow", "useGlobalLight", json!(false));
        set_row_param(&mut f, "innerShadow", "angle", json!(-45.0));
        assert_eq!(f["p:dropShadow"]["angle"].as_f64(), Some(30.0));
        set_row_param(&mut f, "innerShadow", "useGlobalLight", json!(true));
        assert_eq!(f["p:innerShadow"]["angle"].as_f64(), Some(30.0));
        // OK sets the document's light to the shared angle.
        let (mut s, _) = styled_session(json!([]));
        let id = s.active().unwrap().active_layer.unwrap();
        f.insert("layer".into(), json!(id.0));
        f.insert("on:dropShadow".into(), json!(true));
        apply(&f, |cmd, p| s.execute(cmd, p).map_err(|e| e.to_string())).unwrap();
        assert_eq!(s.active().unwrap().doc.global_light.angle, 30.0);
    }

    /// Double-clicking an effect row of a layer that isn't active selects that layer first, so
    /// the dialog holds its effects (and OK edits it), not the active layer's.
    #[test]
    fn effect_row_of_another_layer_opens_that_layers_style() {
        let mut app = PhotospliceApp::new(photosplice_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 32, "height": 32})).unwrap();
        app.run("layer.new.layer", json!({})).unwrap();
        let with_fx = app.session.active().unwrap().active_layer.unwrap();
        app.run("layer.layerStyle.replace", json!({"layer": with_fx.0, "effects": [{"kind": "dropShadow", "params": defaults("dropShadow")}]})).unwrap();
        app.run("layer.new.layer", json!({})).unwrap();
        let plain = app.session.active().unwrap().active_layer.unwrap();
        assert_ne!(plain, with_fx);

        let id = open_for_layer(&mut app, with_fx, Some("dropShadow")).unwrap();
        assert_eq!(app.session.active().unwrap().active_layer, Some(with_fx), "the row's layer becomes active");
        let d = app.ui.dialogs.iter().find(|d| d.id == id).unwrap();
        assert_eq!(d.fields["layer"], json!(with_fx.0));
        assert!(d.fields["fx:dropShadow"].is_object(), "the existing drop shadow");
        assert!(!d.fields.contains_key("rows:dropShadow"), "not a second instance");
    }

    /// #2288: the selected effect row must dim again when its checkbox is cleared.
    #[test]
    fn disabled_effect_row_uses_muted_text_even_when_selected() {
        for theme in crate::theme::ThemeKind::ALL {
            let tokens = Tokens::for_kind(theme);
            assert_eq!(fx_label_color(true, &tokens), tokens.text, "{theme:?}: checked");
            assert_eq!(fx_label_color(false, &tokens), tokens.text_dim, "{theme:?}: unchecked");
        }
    }

    /// The dialog shows a Center-source Inner Glow as Center, and OK keeps it.
    #[test]
    fn inner_glow_center_source_survives_ok() {
        let (mut s, layer) = styled_session(json!([{"kind": "innerGlow", "params": {"source": "center"}}]));
        let f = initial_fields(&layer, ColorMode::Rgb, None, 120.0);
        assert_eq!(f["p:innerGlow"]["source"], json!("center"));
        apply(&f, |cmd, p| s.execute(cmd, p).map_err(|e| e.to_string())).unwrap();
        let fx = &s.active().unwrap().doc.layer(layer.id).unwrap().effects.items;
        assert!(fx.iter().any(|e| matches!(e, Effect::InnerGlow(g) if g.source == GlowSource::Center)), "{fx:?}");
    }
}
