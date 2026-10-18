//! Smart objects in PSD: the placed-layer blocks (`SoLd`, `PlLd`) and the smart filter stack
//! (`filterFX` inside `SoLd`).
//!
//! Layouts follow the Adobe PSD spec ("Placed Layer", "Placed Layer Data") and Photoshop-authored
//! files (`corpus/photoshop/smart-filters`): a `soLD` descriptor holds the linked-file id (`Idnt`,
//! the uuid of the `lnk2` item), the instance id (`placed`, which keys the `FEid` filter cache),
//! the transform quad (`Trnf`), the source size (`Sz  `), the warp, and when filters are applied a
//! `filterFX` object:
//!
//! ```text
//! filterFX (filterFXStyle): enab, validAtPosition, filterMaskEnable, filterMaskLinked,
//!   filterMaskExtendWithWhite, filterFXList = [ filterFX: Nm, blendOptions {Opct, Md},
//!   enab, hasoptions, FrgC, BckC, Fltr (the filter's own descriptor), filterID ]
//! ```
//!
//! The list is in application order (first = bottom, applied first), like
//! [`SmartObject::smart_filters`](photosplice_doc::SmartObject). Filters PhotoSplice implements map
//! to their command id and dialog parameters; any other filter becomes a
//! [`UNSUPPORTED_FILTER`] entry that keeps its descriptor verbatim (hex in `params.psd`), so it is
//! listed, can be hidden, reordered or deleted, and is written back unchanged.
//!
//! Camera Raw and Displace keep their whole item as a template next to the editable settings
//! (Displace's carries the embedded displacement map, see `displace_map.rs`).

use photosplice_color::{BlendMode, ColorMode, PixelFormat, SampleType};
use photosplice_doc::{LayerMask, SmartFilter};
use photosplice_geom::warp::{Warp, WarpStyle};
use photosplice_geom::{Affine, Rect as GeomRect};
use photosplice_psd::Compression;
use photosplice_psd::Rect as PsdRect;
use photosplice_psd::descriptor::{Class, Descriptor, Id, ObjectArray, ReferenceItem, UnicodeString, Value, VersionedDescriptor};
use photosplice_psd::filter_effects::{EffectsPlane, FilterEffectsItem};
use photosplice_raster::Surface;
use serde_json::{Map, Value as J, json};

use crate::blocks::{enum_of, get_desc, num};

#[path = "camera_raw_map.rs"]
mod camera_raw;
#[path = "gallery_map.rs"]
mod gallery;
#[path = "displace_map.rs"]
mod displace;

pub use displace::embedded_map as displace_embedded_map;

/// Command id of a Photoshop smart filter PhotoSplice does not implement. Its params hold the
/// filter's name, Photoshop filter id and descriptor (`psd`, hex); it renders as a pass-through.
pub const UNSUPPORTED_FILTER: &str = "psd.unsupportedFilter";

/// The smart filter stack of a placed layer, as stored in `filterFX`.
#[derive(Clone, Debug, PartialEq)]
pub struct FilterStack {
    /// `enab`: Layer › Smart Filter › Disable Smart Filters clears it.
    pub enabled: bool,
    pub filters: Vec<SmartFilter>,
    /// `filterMaskEnable`.
    pub mask_enabled: bool,
    /// `filterMaskLinked`.
    pub mask_linked: bool,
}

impl Default for FilterStack {
    fn default() -> Self {
        FilterStack { enabled: true, filters: Vec::new(), mask_enabled: true, mask_linked: false }
    }
}

/// What a `soLD` descriptor says about a smart object.
#[derive(Clone, Debug)]
pub struct Placed {
    pub descriptor: Descriptor,
    /// `Idnt`: uuid of the embedded or linked file (`lnk2` item).
    pub idnt: String,
    /// `placed`: id of this instance (keys the `FEid` filter cache).
    pub placed: String,
    /// Source pixels → document pixels (from `Trnf` and `Sz  `).
    pub transform: Affine,
    /// `filterFX`, if any filters were ever applied.
    pub stack: Option<FilterStack>,
}

fn text_of(d: &Descriptor, key: &str) -> Option<String> {
    match d.get(key)? {
        Value::Text(t) => Some(t.to_string_lossy()),
        _ => None,
    }
}

fn bool_or(d: &Descriptor, key: &str, default: bool) -> bool {
    match d.get(key) {
        Some(Value::Boolean(b)) => *b,
        _ => default,
    }
}

/// The `soLD` descriptor of `SoLd`/`SoLE` block data.
pub fn sold_descriptor(data: &[u8]) -> Option<Descriptor> {
    if data.get(..4)? != b"soLD" {
        return None;
    }
    crate::blocks::parse_prefix_versioned(data.get(8..)?)
}

/// Parses `SoLd`/`SoLE` block data (best effort: `None` when it is not a `soLD` structure).
pub fn parse_sold(data: &[u8]) -> Option<Placed> {
    let d = sold_descriptor(data)?;
    let (idnt, transform) = crate::blocks::parse_smart(b"SoLd", data);
    let placed = text_of(&d, "placed").unwrap_or_default();
    let stack = get_desc(&d, "filterFX").map(filter_stack);
    Some(Placed { descriptor: d, idnt, placed, transform, stack })
}

/// Reads a `filterFX` object.
pub fn filter_stack(fx: &Descriptor) -> FilterStack {
    let filters = match fx.get("filterFXList") {
        Some(Value::List(items)) => items
            .iter()
            .filter_map(|v| match v {
                Value::Descriptor(d) => Some(filter_from_item(d)),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    FilterStack {
        enabled: bool_or(fx, "enab", true),
        filters,
        mask_enabled: bool_or(fx, "filterMaskEnable", true),
        mask_linked: bool_or(fx, "filterMaskLinked", false),
    }
}

// ---------- the filters we model ----------

/// Photoshop filter → PhotoSplice command, keyed by the `Fltr` class (and `filterID`).
struct Known {
    class: &'static str,
    filter_id: i32,
    /// `Nm  ` as Photoshop writes it (the menu item, with its ellipsis and mnemonic).
    name: &'static str,
    command: &'static str,
}

const fn code(c: &[u8; 4]) -> i32 {
    i32::from_be_bytes(*c)
}

const KNOWN: [Known; 24] = [
    Known { class: "GsnB", filter_id: code(b"GsnB"), name: "Gaussian Blur...", command: "filter.blur.gaussianBlur" },
    Known { class: "boxblur", filter_id: 840, name: "Box Blur...", command: "filter.blur.boxBlur" },
    Known { class: "MtnB", filter_id: code(b"MtnB"), name: "Motion Blur...", command: "filter.blur.motionBlur" },
    Known { class: "UnsM", filter_id: code(b"UnsM"), name: "Unsharp Mask...", command: "filter.sharpen.unsharpMask" },
    Known { class: "AdNs", filter_id: code(b"AdNs"), name: "Add Noise...", command: "filter.noise.addNoise" },
    Known { class: "Mdn ", filter_id: code(b"Mdn "), name: "Median...", command: "filter.noise.median" },
    Known { class: "HghP", filter_id: code(b"HghP"), name: "High Pass...", command: "filter.other.highPass" },
    Known { class: "Mxm ", filter_id: code(b"Mxm "), name: "Maximum...", command: "filter.other.maximum" },
    Known { class: "Mnm ", filter_id: code(b"Mnm "), name: "Minimum...", command: "filter.other.minimum" },
    Known { class: "Msc ", filter_id: code(b"Msc "), name: "Mosaic...", command: "filter.pixelate.mosaic" },
    Known { class: "Embs", filter_id: code(b"Embs"), name: "Emboss...", command: "filter.stylize.emboss" },
    Known { class: "Lvls", filter_id: code(b"Lvls"), name: "Levels...", command: "image.adjustments.levels" },
    Known { class: "Crvs", filter_id: code(b"Crvs"), name: "Curves...", command: "image.adjustments.curves" },
    // Shadows/Highlights at its defaults stores no `Fltr` (`hasoptions` false).
    Known { class: "adaptCorrect", filter_id: 781, name: "Shado&ws/Highlights...", command: "image.adjustments.shadowsHighlights" },
    Known { class: "HStr", filter_id: code(b"HStr"), name: "Hue/Saturation...", command: "image.adjustments.hueSaturation" },
    Known { class: "oilPaint", filter_id: 1065, name: "Oil Paint...", command: "filter.stylize.oilPaint" },
    // Filters without settings (no `Fltr`, `hasoptions` false).
    Known { class: "Invr", filter_id: code(b"Invr"), name: "Invert", command: "image.adjustments.invert" },
    Known { class: "Shrp", filter_id: code(b"Shrp"), name: "Sharpen", command: "filter.sharpen.sharpen" },
    Known { class: "ShrM", filter_id: code(b"ShrM"), name: "Sharpen More", command: "filter.sharpen.sharpenMore" },
    Known { class: "ShrE", filter_id: code(b"ShrE"), name: "Sharpen Edges", command: "filter.sharpen.sharpenEdges" },
    Known { class: "Blr ", filter_id: code(b"Blr "), name: "Blur", command: "filter.blur.blur" },
    Known { class: "BlrM", filter_id: code(b"BlrM"), name: "Blur More", command: "filter.blur.blurMore" },
    Known { class: "Dspc", filter_id: code(b"Dspc"), name: "Despeckle", command: "filter.noise.despeckle" },
    Known { class: "FndE", filter_id: code(b"FndE"), name: "Find Edges", command: "filter.stylize.findEdges" },
];

/// [`KNOWN`] filters that have no settings.
const PLAIN: [&str; 8] = ["Invr", "Shrp", "ShrM", "ShrE", "Blr ", "BlrM", "Dspc", "FndE"];

/// Hue/Saturation's colour ranges in `LclR` order (1 = reds … 6 = magentas), as the
/// `image.adjustments.hueSaturation` parameters name them.
const HUE_RANGES: [&str; 6] = ["reds", "yellows", "greens", "cyans", "blues", "magentas"];

/// `HStr` settings: `Clrz` and an `Adjs` list whose entry without `LclR` is the master (hue,
/// saturation, lightness) and whose `LclR` 1–6 entries are colour ranges with their four bounds.
fn hue_saturation_params(f: &Descriptor) -> Option<J> {
    let mut out = Map::new();
    out.insert("colorize".into(), J::Bool(bool_or(f, "Clrz", false)));
    for a in adjustment_list(f)? {
        let hsl = json!({"hue": int(a, "H   ")?, "saturation": int(a, "Strt")?, "lightness": int(a, "Lght")?});
        match int(a, "LclR") {
            None => {
                if let J::Object(m) = hsl {
                    out.extend(m);
                }
            }
            Some(r) => {
                let key = HUE_RANGES.get(usize::try_from(r).ok()?.checked_sub(1)?)?;
                let mut o = hsl;
                o["range"] = json!([int(a, "BgnR")?, int(a, "BgnS")?, int(a, "EndS")?, int(a, "EndR")?]);
                out.insert((*key).into(), o);
            }
        }
    }
    Some(J::Object(out))
}

/// The `HStr` descriptor for [`hue_saturation_params`]' parameters.
fn hue_saturation_fltr(d: Descriptor, p: &J) -> Result<Descriptor, String> {
    let obj = p.as_object().ok_or("parameters are not an object")?;
    let allowed = ["colorize", "hue", "saturation", "lightness"];
    if let Some(k) = obj.keys().find(|k| !is_meta(k) && !allowed.contains(&k.as_str()) && !HUE_RANGES.contains(&k.as_str())) {
        return Err(format!("setting `{k}` has no PSD equivalent"));
    }
    let hsl = |a: Descriptor, o: &J| a.with("H   ", pi(o, "hue", 0.0)).with("Strt", pi(o, "saturation", 0.0)).with("Lght", pi(o, "lightness", 0.0));
    let mut list = vec![Value::Descriptor(hsl(Descriptor::new("Hst2"), p))];
    for (i, key) in HUE_RANGES.iter().enumerate() {
        let Some(o) = p.get(*key) else { continue };
        let b: Vec<f64> = o.get("range").and_then(J::as_array).map(|v| v.iter().filter_map(J::as_f64).collect()).unwrap_or_default();
        let [b0, b1, b2, b3] = b.as_slice() else { return Err(format!("`{key}` has no four range bounds")) };
        let bound = |v: f64| Value::Integer(v.round().clamp(-720.0, 720.0) as i32);
        let a = Descriptor::new("Hst2").with("LclR", Value::Integer(i as i32 + 1)).with("BgnR", bound(*b0)).with("BgnS", bound(*b1)).with("EndS", bound(*b2)).with("EndR", bound(*b3));
        list.push(Value::Descriptor(hsl(a, o)));
    }
    Ok(d.with("presetKind", Value::Enumerated { type_id: Id::new("presetKindType"), value: Id::new("presetKindCustom") })
        .with("Clrz", Value::Boolean(p.get("colorize").and_then(J::as_bool).unwrap_or(false)))
        .with("Adjs", Value::List(list)))
}

fn known_by_command(command: &str) -> Option<&'static Known> {
    KNOWN.iter().find(|k| k.command == command)
}

/// RGB channel references in Levels/Curves (`Chnl` enum) and their parameter keys ("" = the
/// composite: top-level keys).
const CHANNELS: [(&[u8; 4], &str); 4] = [(b"Cmps", ""), (b"Rd  ", "red"), (b"Grn ", "green"), (b"Bl  ", "blue")];

fn px(d: &Descriptor, key: &str) -> Option<f64> {
    num(d.get(key)).filter(|v| v.is_finite())
}

fn int(d: &Descriptor, key: &str) -> Option<i32> {
    match d.get(key)? {
        Value::Integer(i) => Some(*i),
        _ => None,
    }
}

fn channel_key(d: &Descriptor) -> Option<&'static str> {
    let Some(Value::Reference(items)) = d.get("Chnl") else { return None };
    let Some(ReferenceItem::Enumerated { value, .. }) = items.first() else { return None };
    CHANNELS.iter().find(|(c, _)| value.as_bytes() == &c[..]).map(|(_, k)| *k)
}

fn adjustment_list(d: &Descriptor) -> Option<Vec<&Descriptor>> {
    let Some(Value::List(items)) = d.get("Adjs") else { return None };
    items
        .iter()
        .map(|v| match v {
            Value::Descriptor(x) => Some(x),
            _ => None,
        })
        .collect()
}

fn levels_params(f: &Descriptor) -> Option<J> {
    let mut out = Map::new();
    for a in adjustment_list(f)? {
        let key = channel_key(a)?;
        let mut o = Map::new();
        let pair = |k: &str| match a.get(k) {
            Some(Value::List(v)) if v.len() == 2 => Some((int_value(&v[0])?, int_value(&v[1])?)),
            _ => None,
        };
        if let Some((lo, hi)) = pair("Inpt") {
            o.insert("inBlack".into(), json!(lo));
            o.insert("inWhite".into(), json!(hi));
        }
        if let Some(g) = px(a, "Gmm ") {
            o.insert("gamma".into(), json!(g));
        }
        if let Some((lo, hi)) = pair("Otpt") {
            o.insert("outBlack".into(), json!(lo));
            o.insert("outWhite".into(), json!(hi));
        }
        if key.is_empty() {
            out.extend(o);
        } else {
            out.insert(key.into(), J::Object(o));
        }
    }
    Some(J::Object(out))
}

fn int_value(v: &Value) -> Option<i32> {
    match v {
        Value::Integer(i) => Some(*i),
        _ => None,
    }
}

fn curves_params(f: &Descriptor) -> Option<J> {
    let mut out = Map::new();
    for a in adjustment_list(f)? {
        let key = channel_key(a)?;
        // A pencil-drawn curve is a mapping of 256 output levels (`Mpng`) instead of points.
        if let Some(Value::List(map)) = a.get("Mpng") {
            let map: Option<Vec<J>> = map.iter().map(|v| int_value(v).map(|o| json!(o.clamp(0, 255)))).collect();
            out.insert(if key.is_empty() { "points".into() } else { key.into() }, J::Array(map.filter(|m| m.len() == 256)?));
            out.insert("pencil".into(), J::Bool(true));
            continue;
        }
        let Some(Value::List(pts)) = a.get("Crv ") else { return None };
        let pts: Option<Vec<J>> = pts
            .iter()
            .map(|p| match p {
                Value::Descriptor(p) => Some(json!([px(p, "Hrzn")?, px(p, "Vrtc")?])),
                _ => None,
            })
            .collect();
        out.insert(if key.is_empty() { "points".into() } else { key.into() }, J::Array(pts?));
    }
    Some(J::Object(out))
}

/// Our command and parameters for a known Photoshop filter; `None` when the filter (or one of its
/// settings) is not modelled.
fn known_params(class: &str, f: Option<&Descriptor>) -> Option<(&'static str, J)> {
    let k = KNOWN.iter().find(|k| k.class == class)?;
    let params = match (class, f) {
        ("adaptCorrect", None) => json!({}),
        (c, _) if PLAIN.contains(&c) => json!({}),
        (_, None) => return None,
        ("GsnB" | "boxblur" | "Mdn " | "HghP" | "Mxm " | "Mnm ", Some(f)) => json!({"radius": px(f, "Rds ")?}),
        ("MtnB", Some(f)) => json!({"angle": int(f, "Angl")?, "distance": px(f, "Dstn")?}),
        ("UnsM", Some(f)) => json!({"amount": px(f, "Amnt")?, "radius": px(f, "Rds ")?, "threshold": int(f, "Thsh")?}),
        ("AdNs", Some(f)) => {
            let dist = match enum_of(f, "Dstr")? {
                b"Unfr" => "uniform",
                b"Gsn " => "gaussian",
                _ => return None,
            };
            let mono = match f.get("Mnch") {
                Some(Value::Boolean(b)) => *b,
                _ => return None,
            };
            json!({"amount": px(f, "Nose")?, "distribution": dist, "monochromatic": mono, "seed": int(f, "FlRs").unwrap_or(0)})
        }
        ("Msc ", Some(f)) => json!({"cellSize": px(f, "ClSz")?}),
        ("Embs", Some(f)) => json!({"angle": int(f, "Angl")?, "height": int(f, "Hght")?, "amount": int(f, "Amnt")?}),
        ("Lvls", Some(f)) => levels_params(f)?,
        ("Crvs", Some(f)) => curves_params(f)?,
        ("HStr", Some(f)) => hue_saturation_params(f)?,
        ("oilPaint", Some(f)) => json!({
            "stylization": px(f, "stylization")?,
            "cleanliness": px(f, "cleanliness")?,
            "scale": px(f, "brushScale")?,
            "bristleDetail": px(f, "microBrush")?,
            "lighting": match f.get("lightingOn") { Some(Value::Boolean(b)) => *b, _ => return None },
            "angle": int(f, "LghD")?,
            "shine": px(f, "specularity")?,
        }),
        _ => return None,
    };
    Some((k.command, params))
}

// ---------- Filter Gallery ----------
//
// Filter › Filter Gallery stores its effects in a `GEfc` filter (filterID `GEfc`). The reference
// writes a single effect inline, whose settings `gallery_map.rs` maps effect by effect; other
// editors write a list (also for one effect), in application order, each with its visibility:
//
// ```text
// Fltr = GEfc { GEfk = GEft enum (the effect), <settings> }                          (inline)
// Fltr = GEfc { GEfs = [ GEfc { <settings>, GEfk = GEft enum, GELv = visible } ] }   (list)
// ```
//
// A list maps through the same per-effect table. A list with any effect the table can't read
// stays an [`UNSUPPORTED_FILTER`] with its descriptor kept verbatim.

const GALLERY_COMMAND: &str = "filter.filterGallery";

/// `filter.filterGallery` and its params for a `GEfc` filter, inline or a `GEfs` list; `None`
/// when an effect is not modelled.
fn gallery_import(item: &Descriptor, fltr: &Descriptor) -> Option<(&'static str, J)> {
    let list = match fltr.get("GEfs") {
        None => return gallery::import(item, fltr),
        Some(Value::List(list)) => list,
        Some(_) => return None,
    };
    let mut effects = Vec::with_capacity(list.len());
    let mut first = None;
    for v in list {
        let Value::Descriptor(e) = v else { return None };
        let (command, mut p) = gallery::import(item, e)?;
        let mut effect = p.get_mut("effects").and_then(J::as_array_mut).and_then(Vec::pop)?;
        effect.as_object_mut()?.insert("visible".into(), J::Bool(bool_or(e, "GELv", true)));
        effects.push(effect);
        first.get_or_insert((command, p));
    }
    let (command, mut p) = first?;
    p.as_object_mut()?.insert("effects".into(), J::Array(effects));
    Some((command, p))
}

/// The `filterFXList` item of a gallery smart filter, or `None` when `f` isn't one: one visible
/// effect inline (`gallery_map.rs`), several effects or a hidden one as a `GEfs` list.
fn gallery_export(f: &SmartFilter) -> Option<Result<Descriptor, String>> {
    let list = (f.command == GALLERY_COMMAND).then(|| f.params.get("effects").and_then(J::as_array)).flatten();
    match list {
        Some(list) if list.len() > 1 || list.iter().any(|e| e.get("visible").and_then(J::as_bool) == Some(false)) => Some(gallery_stack(f, list)),
        _ => gallery::export(f),
    }
}

/// A gallery stack as a `GEfs` list: each effect written as its one-effect item, its settings
/// object listed with `GELv`.
fn gallery_stack(f: &SmartFilter, list: &[J]) -> Result<Descriptor, String> {
    let mut first: Option<Descriptor> = None;
    let mut items = Vec::with_capacity(list.len());
    for e in list {
        let visible = e.get("visible").and_then(J::as_bool).unwrap_or(true);
        let mut one = e.clone();
        if let Some(o) = one.as_object_mut() {
            o.remove("visible");
        }
        let mut params = Map::new();
        params.insert("effects".into(), J::Array(vec![one]));
        for k in ["foreground", "background"] {
            if let Some(c) = f.params.get(k) {
                params.insert(k.into(), c.clone());
            }
        }
        let single = SmartFilter { command: f.command.clone(), params: J::Object(params), ..f.clone() };
        let item = gallery::export(&single).ok_or_else(|| "not a gallery effect".to_string())??;
        let fltr = get_desc(&item, "Fltr").cloned().ok_or("a gallery effect has no settings")?;
        items.push(Value::Descriptor(fltr.with("GELv", Value::Boolean(visible))));
        first.get_or_insert(item);
    }
    let mut d = first.ok_or("it has no effects")?;
    set(&mut d, "Fltr", Value::Descriptor(Descriptor::new(gallery::CLASS).with("GEfs", Value::List(items))));
    Ok(d)
}

/// Lowercase hex of `bytes`.
fn to_hex(bytes: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from(H[usize::from(b >> 4)]));
        s.push(char::from(H[usize::from(b & 15)]));
    }
    s
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    let d = |c: u8| (c as char).to_digit(16).map(|v| v as u8);
    let b = s.as_bytes();
    if !b.len().is_multiple_of(2) {
        return None;
    }
    b.as_chunks::<2>().0.iter().map(|c| Some(d(c[0])? << 4 | d(c[1])?)).collect()
}

/// One `filterFXList` item → [`SmartFilter`] (an [`UNSUPPORTED_FILTER`] when not modelled).
pub fn filter_from_item(item: &Descriptor) -> SmartFilter {
    let opts = get_desc(item, "blendOptions");
    let opacity = opts.and_then(|o| px(o, "Opct")).map_or(1.0, |v| (v / 100.0).clamp(0.0, 1.0) as f32);
    let blend = opts.and_then(|o| enum_of(o, "Md  ")).and_then(crate::effects_map::blend_from_id).unwrap_or(BlendMode::Normal);
    let visible = bool_or(item, "enab", true);
    let fltr = get_desc(item, "Fltr");
    let class = fltr.map(|f| String::from_utf8_lossy(f.class_id.as_bytes()).into_owned());
    let filter_id = int(item, "filterID").unwrap_or(0);
    let class = class.or_else(|| KNOWN.iter().find(|k| k.filter_id == filter_id).map(|k| k.class.to_string()));
    let known = match class.as_deref() {
        Some(camera_raw::CLASS) => camera_raw::import_params(item).map(|p| (camera_raw::COMMAND, p)),
        Some(displace::CLASS) => displace::import_params(item).map(|p| (displace::COMMAND, p)),
        Some(gallery::CLASS) => fltr.and_then(|f| gallery_import(item, f)),
        c => c.and_then(|c| known_params(c, fltr)),
    };
    let (command, params) = match known {
        Some((c, p)) => (c.to_string(), p),
        None => {
            let name = text_of(item, "Nm  ").unwrap_or_default();
            // The stored descriptor carries neutral blending (the model's own fields replace
            // them on export), so hiding or re-blending the filter leaves its params alone.
            let mut neutral = item.clone();
            for (k, v) in &mut neutral.items {
                if k.is("blendOptions") {
                    *v = blend_options(&SmartFilter { command: String::new(), params: J::Null, blend: BlendMode::Normal, opacity: 1.0, visible: true });
                } else if k.is("enab") {
                    *v = Value::Boolean(true);
                }
            }
            (UNSUPPORTED_FILTER.to_string(), json!({"name": name.replace('&', ""), "filterID": filter_id, "psd": to_hex(&neutral.to_bytes())}))
        }
    };
    SmartFilter { command, params, blend, opacity, visible }
}

// ---------- writing ----------

fn unit(u: &[u8; 4], v: f64) -> Value {
    Value::UnitFloat { unit: *u, value: v }
}

fn rgbc(v: f64) -> Value {
    Value::Descriptor(Descriptor::new("RGBC").with("Rd  ", Value::Double(v)).with("Grn ", Value::Double(v)).with("Bl  ", Value::Double(v)))
}

fn blend_options(f: &SmartFilter) -> Value {
    let opacity = if f.opacity.is_finite() { f64::from(f.opacity.clamp(0.0, 1.0)) * 100.0 } else { 100.0 };
    Value::Descriptor(Descriptor::new("blendOptions").with("Opct", unit(b"#Prc", opacity)).with("Md  ", crate::effects_map::blend_long_value(f.blend)))
}

/// Parameter keys that are bookkeeping, not filter settings.
fn is_meta(k: &str) -> bool {
    k == "layer" || k.starts_with("__")
}

fn pf(p: &J, key: &str, default: f64) -> f64 {
    p.get(key).and_then(J::as_f64).filter(|v| v.is_finite()).unwrap_or(default)
}

fn pi(p: &J, key: &str, default: f64) -> Value {
    Value::Integer(pf(p, key, default).round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32)
}

fn channel_ref(code: &[u8; 4]) -> Value {
    Value::Reference(vec![ReferenceItem::Enumerated {
        class: Class { name: UnicodeString::new_nul(""), class_id: Id::new("Chnl") },
        type_id: Id::new("Chnl"),
        value: Id::Code(*code),
    }])
}

/// The `Adjs` entries of a Levels or Curves filter: the composite first, then red, green, blue.
fn adjustments(p: &J, curves: bool) -> Result<Value, String> {
    let obj = p.as_object().ok_or("parameters are not an object")?;
    let allowed: &[&str] =
        if curves { &["points", "red", "green", "blue", "pencil"] } else { &["inBlack", "inWhite", "gamma", "outBlack", "outWhite", "red", "green", "blue"] };
    if let Some(k) = obj.keys().find(|k| !is_meta(k) && !allowed.contains(&k.as_str())) {
        return Err(format!("setting `{k}` has no PSD equivalent"));
    }
    let mut list = Vec::new();
    for (code, key) in CHANNELS {
        let o = if key.is_empty() { Some(p) } else { p.get(key) };
        let Some(o) = o else { continue };
        let mut a = Descriptor::new(if curves { "CrvA" } else { "LvlA" }).with("Chnl", channel_ref(code));
        if curves {
            let src = if key.is_empty() { p.get("points") } else { Some(o) };
            let Some(pts) = src.and_then(J::as_array) else { continue };
            if p.get("pencil").and_then(J::as_bool) == Some(true) {
                let map: Option<Vec<Value>> =
                    pts.iter().map(|o| o.as_f64().filter(|o| o.is_finite()).map(|o| Value::Integer(o.round().clamp(0.0, 255.0) as i32))).collect();
                a = a.with("Mpng", Value::List(map.filter(|m| m.len() == 256).ok_or("a pencil-drawn curve is not 256 output levels")?));
                list.push(Value::Descriptor(a));
                continue;
            }
            let pts: Option<Vec<Value>> = pts
                .iter()
                .map(|q| {
                    let x = q.get(0)?.as_f64().filter(|v| v.is_finite())?;
                    let y = q.get(1)?.as_f64().filter(|v| v.is_finite())?;
                    Some(Value::Descriptor(Descriptor::new("Pnt ").with("Hrzn", Value::Double(x)).with("Vrtc", Value::Double(y))))
                })
                .collect();
            a = a.with("Crv ", Value::List(pts.ok_or("a curve point is not [input, output]")?));
        } else {
            if key.is_empty() && !["inBlack", "inWhite", "gamma", "outBlack", "outWhite"].iter().any(|k| o.get(k).is_some()) {
                continue;
            }
            a = a.with("Inpt", Value::List(vec![pi(o, "inBlack", 0.0), pi(o, "inWhite", 255.0)])).with("Gmm ", Value::Double(pf(o, "gamma", 1.0)));
            if o.get("outBlack").is_some() || o.get("outWhite").is_some() {
                a = a.with("Otpt", Value::List(vec![pi(o, "outBlack", 0.0), pi(o, "outWhite", 255.0)]));
            }
        }
        list.push(Value::Descriptor(a));
    }
    Ok(Value::List(list))
}

/// The `Fltr` descriptor of a modelled filter (`None` = written without options).
fn fltr_for(k: &Known, p: &J) -> Result<Option<Descriptor>, String> {
    let d = Descriptor::new(k.class);
    Ok(Some(match k.class {
        "GsnB" | "boxblur" | "Mdn " | "HghP" | "Mxm " | "Mnm " => {
            if p.get("preserve").and_then(J::as_str).is_some_and(|s| s == "roundness") {
                return Err("“preserve roundness” has no PSD equivalent".into());
            }
            d.with("Rds ", unit(b"#Pxl", pf(p, "radius", 1.0)))
        }
        "MtnB" => d.with("Angl", pi(p, "angle", 0.0)).with("Dstn", unit(b"#Pxl", pf(p, "distance", 10.0))),
        "UnsM" => d.with("Amnt", unit(b"#Prc", pf(p, "amount", 50.0))).with("Rds ", unit(b"#Pxl", pf(p, "radius", 1.0))).with("Thsh", pi(p, "threshold", 0.0)),
        "AdNs" => {
            let gauss = p.get("distribution").and_then(J::as_str) == Some("gaussian");
            d.with("Dstr", Value::Enumerated { type_id: Id::new("Dstr"), value: Id::new(if gauss { "Gsn " } else { "Unfr" }) })
                .with("Nose", unit(b"#Prc", pf(p, "amount", 12.5)))
                .with("Mnch", Value::Boolean(p.get("monochromatic").and_then(J::as_bool).unwrap_or(false)))
                .with("FlRs", pi(p, "seed", 0.0))
        }
        "Msc " => d.with("ClSz", unit(b"#Pxl", pf(p, "cellSize", 10.0))),
        "Embs" => d.with("Angl", pi(p, "angle", 135.0)).with("Hght", pi(p, "height", 3.0)).with("Amnt", pi(p, "amount", 100.0)),
        "Lvls" => d.with("Adjs", adjustments(p, false)?),
        "Crvs" => d.with("Adjs", adjustments(p, true)?),
        "HStr" => hue_saturation_fltr(d, p)?,
        "oilPaint" => d
            .with("lightingOn", Value::Boolean(p.get("lighting").and_then(J::as_bool).unwrap_or(true)))
            .with("stylization", Value::Double(pf(p, "stylization", 4.0)))
            .with("brushScale", Value::Double(pf(p, "scale", 1.0)))
            .with("microBrush", Value::Double(pf(p, "bristleDetail", 5.0)))
            .with("LghD", pi(p, "angle", -60.0))
            .with("specularity", Value::Double(pf(p, "shine", 1.0)))
            .with("cleanliness", Value::Double(pf(p, "cleanliness", 5.0))),
        c if PLAIN.contains(&c) => {
            if p.as_object().is_some_and(|o| o.keys().any(|k| !is_meta(k))) {
                return Err(format!("{} has no settings in PSD", k.name));
            }
            return Ok(None);
        }
        "adaptCorrect" => {
            if p.as_object().is_some_and(|o| o.keys().any(|k| !is_meta(k))) {
                return Err("only default Shadows/Highlights settings are written to PSD".into());
            }
            return Ok(None);
        }
        _ => return Err("not a recognized filter".into()),
    }))
}

/// The `filterFXList` item for one smart filter, or why it can't be written.
pub fn item_for_filter(f: &SmartFilter) -> Result<Descriptor, String> {
    if f.command == camera_raw::COMMAND {
        let mut d = camera_raw::export_item(&f.params)?;
        set(&mut d, "blendOptions", blend_options(f));
        set(&mut d, "enab", Value::Boolean(f.visible));
        return Ok(d);
    }
    if f.command == displace::COMMAND {
        let mut d = displace::export_item(&f.params)?;
        set(&mut d, "blendOptions", blend_options(f));
        set(&mut d, "enab", Value::Boolean(f.visible));
        return Ok(d);
    }
    if let Some(d) = gallery_export(f) {
        return d;
    }
    if f.command == UNSUPPORTED_FILTER {
        let raw = f.params.get("psd").and_then(J::as_str).and_then(from_hex).ok_or("its filter data is missing")?;
        let mut d = Descriptor::from_bytes(&raw).map_err(|e| format!("its filter data is unreadable ({e})"))?;
        set(&mut d, "blendOptions", blend_options(f));
        set(&mut d, "enab", Value::Boolean(f.visible));
        return Ok(d);
    }
    let k = known_by_command(&f.command).ok_or("this filter has no PSD equivalent")?;
    let fltr = fltr_for(k, &f.params)?;
    let mut d = Descriptor::new("filterFX")
        .with("Nm  ", Value::Text(UnicodeString::new_nul(k.name)))
        .with("blendOptions", blend_options(f))
        .with("enab", Value::Boolean(f.visible))
        .with("hasoptions", Value::Boolean(fltr.is_some()))
        .with("FrgC", rgbc(0.0))
        .with("BckC", rgbc(255.0));
    if let Some(x) = fltr {
        d = d.with("Fltr", Value::Descriptor(x));
    }
    Ok(d.with("filterID", Value::Integer(k.filter_id)))
}

/// Builds a `filterFX` object; filters that can't be written are left out, with a message each.
pub fn filter_fx(stack: &FilterStack, layer: &str, warnings: &mut Vec<String>) -> Descriptor {
    let mut list = Vec::new();
    for f in &stack.filters {
        match item_for_filter(f) {
            Ok(d) => list.push(Value::Descriptor(d)),
            Err(e) => warnings.push(format!("layer \"{layer}\": smart filter {} was not written to PSD: {e}", f.command)),
        }
    }
    Descriptor::new("filterFXStyle")
        .with("enab", Value::Boolean(stack.enabled))
        .with("validAtPosition", Value::Boolean(true))
        .with("filterMaskEnable", Value::Boolean(stack.mask_enabled))
        .with("filterMaskLinked", Value::Boolean(stack.mask_linked))
        .with("filterMaskExtendWithWhite", Value::Boolean(true))
        .with("filterFXList", Value::List(list))
}

/// Replaces item `key` in place, or appends it.
fn set(d: &mut Descriptor, key: &str, v: Value) {
    match d.items.iter_mut().find(|(k, _)| k.is(key)) {
        Some(e) => e.1 = v,
        None => d.items.push((Id::new(key), v)),
    }
}

/// Everything needed to write the placed-layer blocks of a smart object.
#[derive(Clone, Debug)]
pub struct PlacedSpec<'a> {
    pub idnt: &'a str,
    pub placed: &'a str,
    /// Source pixels → document pixels.
    pub transform: Affine,
    /// Distort / Perspective: the full projective map (row-major 3×3), overriding `transform`.
    pub perspective: Option<[f64; 9]>,
    /// Source size in pixels.
    pub size: (f64, f64),
    /// Source resolution.
    pub dpi: f64,
    pub warp: Option<&'a Warp>,
    /// `filterFX`, when the smart object has filters.
    pub filter_fx: Option<Descriptor>,
}

/// Where the frame corners land (top-left, top-right, bottom-right, bottom-left), as x, y pairs.
/// The frame is [`crate::blocks::placed_frame`] for an affine placement (the inverse of
/// `parse_smart`), the source rect for a projective one (as `parse_smart_perspective` reads it).
fn quad_points(s: &PlacedSpec<'_>) -> [f64; 8] {
    let (w, h) = s.size;
    let [a, b, c, d, e, f] = s.transform.m;
    let map = |x: f64, y: f64| match &s.perspective {
        Some(p) => photosplice_algo::transform::Homography(*p).apply(x, y),
        None => (a * x + c * y + e, b * x + d * y + f),
    };
    // The frame Reference maps onto the quad: the custom warp's control-point box for an affine
    // placement (the inverse of `parse_smart`), the source rect for a projective one.
    let [x0, y0, x1, y1] = if s.perspective.is_some() { [0.0, 0.0, w, h] } else { crate::blocks::placed_frame(s.warp, w, h) };
    let mut out = [0.0; 8];
    for (i, (x, y)) in [(x0, y0), (x1, y0), (x1, y1), (x0, y1)].into_iter().enumerate() {
        (out[2 * i], out[2 * i + 1]) = map(x, y);
    }
    out
}

/// The transform quad: the source corners (top-left, top-right, bottom-right, bottom-left).
fn quad(s: &PlacedSpec<'_>) -> Value {
    Value::List(quad_points(s).into_iter().map(Value::Double).collect())
}

/// Has the placement moved from the quad a template descriptor stores (`Trnf`)?
fn quad_moved(template: &Descriptor, s: &PlacedSpec<'_>) -> bool {
    let Some(Value::List(pts)) = template.get("Trnf") else { return true };
    let old: Vec<f64> = pts.iter().filter_map(|v| crate::blocks::num(Some(v))).collect();
    let new = quad_points(s);
    old.len() != 8 || old.iter().zip(new).any(|(a, b)| (a - b).abs() > 1e-6 * (1.0 + a.abs()))
}

fn enumv(t: &str, v: &str) -> Value {
    Value::Enumerated { type_id: Id::new(t), value: Id::new(v) }
}

/// The `warp` descriptor (also the `PlLd` warp). Multi-patch meshes have no 1 × 1 envelope and
/// are written as no warp, with a message.
fn warp_desc(w: Option<&Warp>, size: (f64, f64), warnings: &mut Vec<String>) -> Value {
    let none = Warp::none([0.0, 0.0, size.0, size.1]);
    let mut w = w.unwrap_or(&none).clone();
    let mesh = match (&w.style, &w.mesh) {
        (WarpStyle::Custom, Some(m)) if m.points.len() == 16 => Some(m.clone()),
        (WarpStyle::Custom, _) => {
            warnings.push("a custom warp with more than one patch was written to PSD without its warp".into());
            w = none.clone();
            None
        }
        _ => None,
    };
    let [x0, y0, x1, y1] = w.bounds;
    let bounds = Descriptor::new("classFloatRect")
        .with("Top ", Value::Double(y0))
        .with("Left", Value::Double(x0))
        .with("Btom", Value::Double(y1))
        .with("Rght", Value::Double(x1));
    let mut d = Descriptor::new("warp")
        .with("warpStyle", enumv("warpStyle", w.style.psd_name()))
        .with("warpValue", Value::Double(w.bend))
        .with("warpPerspective", Value::Double(w.h_distort))
        .with("warpPerspectiveOther", Value::Double(w.v_distort))
        .with("warpRotate", enumv("Ornt", if w.vertical { "Vrtc" } else { "Hrzn" }))
        .with("bounds", Value::Descriptor(bounds))
        .with("uOrder", Value::Integer(4))
        .with("vOrder", Value::Integer(4));
    if let Some(m) = mesh {
        let body = Descriptor::new("rationalPoint")
            .with("Hrzn", Value::UnitFloats { unit: *b"#Pxl", values: m.points.iter().map(|p| p[0]).collect() })
            .with("Vrtc", Value::UnitFloats { unit: *b"#Pxl", values: m.points.iter().map(|p| p[1]).collect() });
        d = d.with(
            "customEnvelopeWarp",
            Value::Descriptor(Descriptor::new("customEnvelopeWarp").with("meshPoints", Value::ObjectArray(ObjectArray { prefix: 16, body }))),
        );
    }
    Value::Descriptor(d)
}

fn ratio(n: i32, d: i32) -> Value {
    Value::Descriptor(Descriptor::new("null").with("numerator", Value::Integer(n)).with("denominator", Value::Integer(d)))
}

fn pad4(mut v: Vec<u8>) -> Vec<u8> {
    while !v.len().is_multiple_of(4) {
        v.push(0);
    }
    v
}

/// `SoLd` block data. With `template` (the imported descriptor) every key it has is kept and only
/// the placed id (for a duplicate), transform (when it moved), warp and filter stack are rewritten;
/// otherwise a fresh descriptor in Photoshop's key order.
pub fn sold_bytes(template: Option<&Descriptor>, s: &PlacedSpec<'_>, warnings: &mut Vec<String>) -> Vec<u8> {
    let warp = warp_desc(s.warp, s.size, warnings);
    let d = match template {
        Some(t) => {
            let mut d = t.clone();
            if text_of(t, "placed").as_deref() != Some(s.placed) {
                set(&mut d, "placed", Value::Text(UnicodeString::new_nul(s.placed)));
            }
            if quad_moved(t, s) {
                set(&mut d, "Trnf", quad(s));
                set(&mut d, "nonAffineTransform", quad(s));
            }
            // The file's warp stays while it still reads as the layer's warp: one the model
            // can't hold (a multi-patch "quilt" mesh reads as none) survived a move only as
            // "no warp" before.
            let same_warp = get_desc(t, "warp").is_some_and(|w| crate::blocks::warp_from_desc(w).as_ref() == s.warp);
            if !same_warp {
                set(&mut d, "warp", warp);
            }
            match &s.filter_fx {
                Some(fx) => set(&mut d, "filterFX", Value::Descriptor(fx.clone())),
                None => d.items.retain(|(k, _)| !k.is("filterFX")),
            }
            d
        }
        None => {
            let mut d = Descriptor::new("null")
                .with("Idnt", Value::Text(UnicodeString::new_nul(s.idnt)))
                .with("placed", Value::Text(UnicodeString::new_nul(s.placed)))
                .with("PgNm", Value::Integer(1))
                .with("totalPages", Value::Integer(1))
                .with("Crop", Value::Integer(1))
                .with("frameStep", ratio(0, 600))
                .with("duration", ratio(0, 600))
                .with("frameCount", Value::Integer(1))
                .with("Annt", Value::Integer(16))
                .with("Type", Value::Integer(2))
                .with("Trnf", quad(s))
                .with("nonAffineTransform", quad(s))
                .with("warp", warp)
                .with("Sz  ", Value::Descriptor(Descriptor::new("Pnt ").with("Wdth", Value::Double(s.size.0)).with("Hght", Value::Double(s.size.1))))
                .with("Rslt", unit(b"#Rsl", s.dpi));
            if let Some(fx) = &s.filter_fx {
                d = d.with("filterFX", Value::Descriptor(fx.clone()));
            }
            d.with("comp", Value::Integer(-1))
                .with("compInfo", Value::Descriptor(Descriptor::new("null").with("compID", Value::Integer(-1)).with("originalCompID", Value::Integer(-1))))
        }
    };
    let mut out = b"soLD".to_vec();
    out.extend_from_slice(&4u32.to_be_bytes());
    out.extend(VersionedDescriptor::new(d).to_bytes());
    pad4(out)
}

/// `PlLd` block data (`plcL` version 3): the legacy placed-layer record Photoshop writes next to
/// `SoLd`. Its warp is the one `sold` (the `SoLd` data written beside it) carries, so a warp kept
/// from the file (one the model can't hold) is not reset to none here.
pub fn plld_bytes(s: &PlacedSpec<'_>, sold: &[u8], warnings: &mut Vec<String>) -> Vec<u8> {
    let mut out = b"plcL".to_vec();
    out.extend_from_slice(&3u32.to_be_bytes());
    let id = s.idnt.as_bytes();
    let n = id.len().min(255);
    out.push(n as u8);
    out.extend_from_slice(&id[..n]);
    for v in [1i32, 1, 16, 2] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    if let Value::List(q) = quad(s) {
        for v in q {
            if let Value::Double(x) = v {
                out.extend_from_slice(&x.to_be_bytes());
            }
        }
    }
    out.extend_from_slice(&0u32.to_be_bytes());
    let kept = sold_descriptor(sold).and_then(|d| get_desc(&d, "warp").cloned());
    let w = match kept {
        Some(w) => w,
        None => {
            let Value::Descriptor(w) = warp_desc(s.warp, s.size, warnings) else { return pad4(out) };
            w
        }
    };
    out.extend(VersionedDescriptor::new(w).to_bytes());
    pad4(out)
}

/// The source size a `soLD` descriptor stores (`Sz  `).
pub fn stored_size(d: &Descriptor) -> Option<(f64, f64)> {
    let sz = get_desc(d, "Sz  ")?;
    Some((num(sz.get("Wdth"))?, num(sz.get("Hght"))?)).filter(|(w, h)| *w > 0.0 && *h > 0.0)
}

// ---------- the filter mask (`FEid`) ----------

/// The filter mask stored in an `FEid` item, in document coordinates and white beyond its bounds
/// (`filterMaskExtendWithWhite`), as a grayscale mask of `sample` depth. `Ok(None)` when the
/// item has no mask or it is all white (no mask); `Err` when it can't be decoded.
pub fn mask_from_item(item: &FilterEffectsItem, sample: SampleType, stack: &FilterStack) -> Result<Option<LayerMask>, String> {
    if item.mask.is_none() {
        return Ok(None);
    }
    let (rect, plane) = item.decoded_mask().ok_or("the smart filter mask could not be decoded")?;
    if item.depth != u32::from(crate::pixels::psd_depth(sample)) {
        return Err(format!("the smart filter mask is {}-bit in a {:?} document", item.depth, sample));
    }
    let (w, h) = rect.size().map_err(|e| e.to_string())?;
    let fmt = PixelFormat::new(ColorMode::Grayscale, sample, false);
    let mut surface = Surface::with_default(fmt, &[1.0]);
    let n = w.checked_mul(h).ok_or("the smart filter mask is too large")?;
    let bytes = crate::pixels::interleave(&[Some(&plane)], &[crate::pixels::zero_sample(sample)], n, sample, &[false]);
    surface.write_interleaved(GeomRect::new(rect.left, rect.top, rect.right, rect.bottom), &bytes);
    surface.prune();
    if surface.content_bounds().is_empty() {
        return Ok(None);
    }
    Ok(Some(LayerMask { surface, enabled: stack.mask_enabled, linked: stack.mask_linked, density: 1.0, feather: 0.0 }))
}

/// The unfiltered pixels an `FEid` item caches (the smart object's placed content before its
/// filters; inverse of [`feid_item`]), in the document format `fmt`. `None` when the item holds
/// no image, its depth isn't the document's, or a plane doesn't decode.
pub fn unfiltered_from_item(item: &FilterEffectsItem, fmt: PixelFormat) -> Option<Surface> {
    let cc = fmt.mode.color_channels();
    let (w, h) = item.rect.size().ok()?;
    let depth = crate::pixels::psd_depth(fmt.sample);
    if w == 0 || h == 0 || item.depth != u32::from(depth) {
        return None;
    }
    let n = w.checked_mul(h)?;
    let plane = |slot: usize| item.slots.get(slot).and_then(Option::as_ref).and_then(|p| p.decode(w, h, depth).ok());
    let mut planes = Vec::with_capacity(cc + 1);
    for c in 0..cc {
        planes.push(Some(plane(c)?));
    }
    planes.push(plane(25));
    let refs: Vec<Option<&[u8]>> = planes.iter().map(|p| p.as_deref()).collect();
    let mut fill: Vec<Vec<u8>> = vec![crate::pixels::zero_sample(fmt.sample); cc];
    fill.push(crate::pixels::max_sample(fmt.sample));
    let mut invert = vec![fmt.mode == ColorMode::Cmyk; cc];
    invert.push(false);
    let mut bytes = crate::pixels::interleave(&refs, &fill, n, fmt.sample, &invert);
    if fmt.mode == ColorMode::Lab && fmt.sample == SampleType::U16 {
        crate::pixels::lab16_chroma(&mut bytes, cc + 1, true);
    }
    let r = item.rect;
    let mut s = Surface::from_interleaved(fmt, GeomRect::new(r.left, r.top, r.right, r.bottom), &bytes);
    s.prune();
    Some(s)
}

/// What Photoshop shows of a smart object with a filter mask: its `unfiltered` content and the
/// `filtered` result mixed by the mask (premultiplied, straight-alpha surfaces). Photoshop keeps
/// the layer's own pixels as the unmasked filter result and mixes at display time (Photoshop
/// corpus smart-filters/sf-filter-mask-*.psd).
pub fn filter_mask_mix(unfiltered: &Surface, filtered: &Surface, mask: &LayerMask) -> Surface {
    mask_combine(unfiltered, filtered, mask, false)
}

/// Inverse of [`filter_mask_mix`]: the unmasked filter result a `mixed` surface came from, as a
/// PSD stores a smart object with a filter mask. Where the mask hides the filter entirely the
/// mixed pixels are kept (Photoshop shows the unfiltered content there whatever is stored).
pub fn filter_mask_unmix(unfiltered: &Surface, mixed: &Surface, mask: &LayerMask) -> Surface {
    mask_combine(unfiltered, mixed, mask, true)
}

fn mask_combine(base: &Surface, other: &Surface, mask: &LayerMask, inverse: bool) -> Surface {
    let fmt = other.format();
    let base = if base.format() == fmt { std::borrow::Cow::Borrowed(base) } else { std::borrow::Cow::Owned(base.convert(fmt)) };
    let area = base.content_bounds().union(&other.content_bounds());
    let mut out = other.clone();
    if area.is_empty() {
        return out;
    }
    let n = fmt.channels();
    let (b, f) = (base.read_region(area), other.read_region(area));
    let mut k = Vec::new();
    mask.values_into(area, &mut k);
    let mut o = vec![0.0f32; b.len()];
    // mixed = base + m·(filtered − base), premultiplied; the inverse solves for `filtered`.
    let solve = |b: f32, x: f32, m: f32| if inverse { if m > 1e-3 { b + (x - b) / m } else { x } } else { b + (x - b) * m };
    for (i, ((bp, fp), op)) in b.chunks_exact(n).zip(f.chunks_exact(n)).zip(o.chunks_exact_mut(n)).enumerate() {
        let m = k.get(i).copied().unwrap_or(1.0).clamp(0.0, 1.0);
        if !fmt.alpha {
            for c in 0..n {
                op[c] = solve(bp[c], fp[c], m).clamp(0.0, 1.0);
            }
            continue;
        }
        let a = n - 1;
        let oa = solve(bp[a], fp[a], m).clamp(0.0, 1.0);
        op[a] = oa;
        for c in 0..a {
            let pm = solve(bp[c] * bp[a], fp[c] * fp[a], m);
            op[c] = if oa > 0.0 { (pm / oa).clamp(0.0, 1.0) } else { 0.0 };
        }
    }
    out.write_region(area, &o);
    out.prune();
    out
}

/// An `FEid` item for a smart object with filters: the unfiltered placed pixels Photoshop
/// filters from (colour channels from slot 0, transparency in the last slot; in the document's
/// pixel format, CMYK inverted as in layer channels) and the filter mask over `bounds` (white
/// when `mask` is `None`). Photoshop needs the planes: re-rendering the filters of an item
/// without them crashes it. `None` when there are no unfiltered pixels.
pub fn feid_item(placed: &str, unfiltered: &Surface, mask: Option<&LayerMask>, bounds: GeomRect, doc_fmt: PixelFormat) -> Option<FilterEffectsItem> {
    let sample = doc_fmt.sample;
    let px = if unfiltered.format() == doc_fmt { unfiltered.clone() } else { unfiltered.convert(doc_fmt) };
    let content = px.content_bounds();
    if content.is_empty() {
        return None;
    }
    // Over the whole area the filters may draw into: Photoshop clips the filtered result to these
    // planes, so the content bounds alone would cut blurs off at the source's edge.
    let r = content.union(&bounds);
    let cc = doc_fmt.mode.color_channels();
    let depth = crate::pixels::psd_depth(sample);
    let compression = if sample == SampleType::F32 { Compression::ZipPrediction } else { Compression::Rle };
    let encode = |plane: &[u8], w: usize, h: usize| {
        EffectsPlane::encode(compression, plane, w, h, depth)
            .or_else(|_| EffectsPlane::encode(Compression::Raw, plane, w, h, depth))
            .unwrap_or(EffectsPlane { compression: Compression::Raw, data: plane.to_vec() })
    };
    let mut invert = vec![doc_fmt.mode == ColorMode::Cmyk; cc];
    invert.push(false);
    // Fully transparent pixels carry white, as in Photoshop's own cache: filters that look at
    // colour regardless of alpha (Shadows/Highlights) read it.
    let mut vals = px.read_region(r);
    let white = photosplice_raster::from_rgba(&doc_fmt, [1.0, 1.0, 1.0, 1.0]);
    for p in vals.chunks_exact_mut(cc + 1) {
        if p[cc] <= 0.0 {
            p[..cc].copy_from_slice(&white[..cc]);
        }
    }
    let mut filled = Surface::new(doc_fmt);
    filled.write_region(r, &vals);
    drop(vals);
    let mut bytes = filled.to_interleaved(r);
    if doc_fmt.mode == ColorMode::Lab && sample == SampleType::U16 {
        // 16-bit Lab a*/b* in Photoshop's scale, as in layer channels.
        crate::pixels::lab16_chroma(&mut bytes, cc + 1, false);
    }
    let planes = crate::pixels::deinterleave(&bytes, cc + 1, sample, &invert);
    let (w, h) = (r.width() as usize, r.height() as usize);
    let mut slots = vec![None; 26];
    for (i, p) in planes.iter().enumerate() {
        let slot = if i == cc { 25 } else { i };
        if let Some(s) = slots.get_mut(slot) {
            *s = Some(encode(p, w, h));
        }
    }
    let mut item = filter_mask_item(placed, mask, bounds, sample);
    item.rect = PsdRect { top: r.y0, left: r.x0, bottom: r.y1, right: r.x1 };
    item.slots = slots;
    Some(item)
}

fn filter_mask_item(placed: &str, mask: Option<&LayerMask>, bounds: GeomRect, sample: SampleType) -> FilterEffectsItem {
    let fmt = PixelFormat::new(ColorMode::Grayscale, sample, false);
    let area = match mask {
        Some(m) => m.surface.content_bounds().union(&bounds),
        None => bounds,
    };
    let area = if area.is_empty() { GeomRect::new(0, 0, 1, 1) } else { area };
    let rect = PsdRect { top: area.y0, left: area.x0, bottom: area.y1, right: area.x1 };
    let (w, h) = (area.width() as usize, area.height() as usize);
    let plane = match mask {
        Some(m) => {
            let s = if m.surface.format() == fmt { m.surface.clone() } else { m.surface.convert(fmt) };
            crate::pixels::deinterleave(&s.to_interleaved(area), 1, sample, &[false]).into_iter().next().unwrap_or_default()
        }
        None => {
            let mut white = Vec::with_capacity(w * h * sample.bytes());
            for _ in 0..w * h {
                crate::pixels::encode_be(1.0, sample, &mut white);
            }
            white
        }
    };
    let depth = crate::pixels::psd_depth(sample);
    let compression = if sample == SampleType::F32 { Compression::ZipPrediction } else { Compression::Rle };
    let plane = EffectsPlane::encode(compression, &plane, w, h, depth)
        .or_else(|_| EffectsPlane::encode(Compression::Raw, &plane, w, h, depth))
        .unwrap_or(EffectsPlane { compression: Compression::Raw, data: plane });
    FilterEffectsItem { id: placed.to_string(), version: 1, rect, depth: u32::from(depth), max_channels: 24, slots: vec![None; 26], mask: Some((rect, plane)) }
}

/// A deterministic uuid-shaped id (8-4-4-4-12 hex digits) derived from `seed`.
pub fn uuid_from(seed: &[u8]) -> String {
    let h = blake3::hash(seed);
    let x = to_hex(&h.as_bytes()[..16]);
    format!("{}-{}-{}-{}-{}", &x[..8], &x[8..12], &x[12..16], &x[16..20], &x[20..32])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Trnf` is the image of a custom warp's control-point box, which may spill past the source:
    /// export and import are each other's inverse, so a spilled mesh comes back with the same
    /// transform and the quad Reference expects (storytold/photocraft@5ed75876).
    #[test]
    fn warp_control_box_quad_round_trips() {
        use photosplice_geom::warp::{BezierMesh, WarpStyle};
        let mut mesh = BezierMesh::identity([0.0, 0.0, 50.0, 25.0], 1, 1);
        mesh.points[0] = [-10.0, -5.0];
        mesh.points[15] = [60.0, 25.0];
        let mut warp = Warp::preset(WarpStyle::Custom, 0.0, [0.0, 0.0, 50.0, 25.0]);
        warp.mesh = Some(mesh);
        let transform = Affine { m: [2.0, 0.0, 0.0, 2.0, 120.0, 210.0] };
        let spec = PlacedSpec { idnt: "i", placed: "p", transform, perspective: None, size: (50.0, 25.0), dpi: 72.0, warp: Some(&warp), filter_fx: None };
        let sold = sold_bytes(None, &spec, &mut Vec::new());
        let p = parse_sold(&sold).expect("parses");
        let Some(Value::List(q)) = p.descriptor.get("Trnf") else { panic!("no Trnf") };
        let q: Vec<f64> = q.iter().filter_map(|v| if let Value::Double(d) = v { Some(*d) } else { None }).collect();
        assert_eq!(q, [100.0, 200.0, 240.0, 200.0, 240.0, 260.0, 100.0, 260.0]);
        assert_eq!(p.transform, transform);
        assert!(!quad_moved(&p.descriptor, &spec));
    }

    fn count(hay: &[u8], needle: &[u8]) -> usize {
        hay.windows(needle.len()).filter(|w| *w == needle).count()
    }

    /// A freshly built placed layer writes `warp` (key and class) as a length-4 string, as
    /// Photoshop does; the code form is a different key to Photoshop.
    #[test]
    fn fresh_placed_warp_is_a_string_id() {
        let spec =
            PlacedSpec { idnt: "id", placed: "id", transform: Affine::IDENTITY, perspective: None, size: (10.0, 10.0), dpi: 72.0, warp: None, filter_fx: None };
        let mut warnings = Vec::new();
        let sold = sold_bytes(None, &spec, &mut warnings);
        let plld = plld_bytes(&spec, &sold, &mut warnings);
        // `SoLd`: the `warp` key and the warp descriptor's class; `PlLd`: the descriptor's class.
        for (bytes, n) in [(&sold, 2), (&plld, 1)] {
            assert_eq!(count(bytes, b"\0\0\0\x04warp"), n);
            assert_eq!(count(bytes, b"\0\0\0\0warp"), 0);
        }
        let d = sold_descriptor(&sold).unwrap();
        assert!(matches!(d.get("warp"), Some(Value::Descriptor(w)) if w.class_id == Id::Str(b"warp".to_vec())));
    }

    fn sf(command: &str, params: J) -> SmartFilter {
        SmartFilter { command: command.into(), params, blend: BlendMode::Normal, opacity: 1.0, visible: true }
    }

    #[test]
    fn modelled_filters_round_trip() {
        let filters = vec![
            sf("filter.blur.gaussianBlur", json!({"radius": 4.0})),
            sf("filter.blur.boxBlur", json!({"radius": 5.0})),
            sf("filter.blur.motionBlur", json!({"angle": -20, "distance": 10.0})),
            sf("filter.sharpen.unsharpMask", json!({"amount": 150.0, "radius": 2.0, "threshold": 0})),
            sf("filter.noise.addNoise", json!({"amount": 12.0, "distribution": "uniform", "monochromatic": false, "seed": 1234})),
            sf("filter.noise.median", json!({"radius": 3.0})),
            sf("filter.other.highPass", json!({"radius": 3.0})),
            sf("filter.other.maximum", json!({"radius": 2.0})),
            sf("filter.other.minimum", json!({"radius": 2.0})),
            sf("filter.pixelate.mosaic", json!({"cellSize": 8.0})),
            sf("filter.stylize.emboss", json!({"angle": 135, "height": 3, "amount": 100})),
            sf("image.adjustments.levels", json!({"inBlack": 20, "inWhite": 220, "gamma": 1.4, "red": {"inBlack": 0, "inWhite": 255, "gamma": 0.8}})),
            sf("image.adjustments.curves", json!({"points": [[0.0, 0.0], [64.0, 40.0], [255.0, 255.0]], "blue": [[0.0, 20.0], [255.0, 235.0]]})),
            sf("image.adjustments.shadowsHighlights", json!({})),
            // A pencil-drawn curve: 256 output levels (`Mpng`).
            sf("image.adjustments.curves", json!({"pencil": true, "points": (0..256).rev().collect::<Vec<i32>>()})),
            sf("image.adjustments.hueSaturation", json!({"colorize": false, "hue": 20, "saturation": -100, "lightness": 5, "reds": {"hue": 10, "saturation": 30, "lightness": 0, "range": [315, 345, 15, 45]}})),
            sf("image.adjustments.hueSaturation", json!({"colorize": true, "hue": 200, "saturation": 25, "lightness": 0})),
            sf("filter.stylize.oilPaint", json!({"stylization": 1.2, "cleanliness": 10.0, "scale": 10.0, "bristleDetail": 10.0, "lighting": false, "angle": -60, "shine": 0.0})),
            sf("image.adjustments.invert", json!({})),
            sf("filter.sharpen.sharpen", json!({})),
            sf("filter.sharpen.sharpenMore", json!({})),
            sf("filter.sharpen.sharpenEdges", json!({})),
            sf("filter.blur.blur", json!({})),
            sf("filter.blur.blurMore", json!({})),
            sf("filter.noise.despeckle", json!({})),
            sf("filter.stylize.findEdges", json!({})),
        ];
        for mut f in filters {
            f.blend = BlendMode::Overlay;
            f.opacity = 0.75;
            f.visible = false;
            let d = item_for_filter(&f).unwrap();
            let bytes = d.to_bytes();
            let back = filter_from_item(&Descriptor::from_bytes(&bytes).unwrap());
            assert_eq!(back, f, "{}", f.command);
        }
    }

    /// Reference writes a filter without settings as its name and `filterID` alone (watercolor
    /// portrait template: Invert, Sharpen More); they read as our commands.
    #[test]
    fn filters_without_settings_read_from_their_id() {
        for (id, command) in [(b"Invr", "image.adjustments.invert"), (b"ShrM", "filter.sharpen.sharpenMore")] {
            let item = Descriptor::new("filterFX")
                .with("Nm  ", Value::Text(UnicodeString::new_nul("x")))
                .with("enab", Value::Boolean(true))
                .with("filterID", Value::Integer(code(id)));
            let f = filter_from_item(&item);
            assert_eq!((f.command.as_str(), &f.params), (command, &json!({})));
        }
        // Settings on a filter that has none can't be written.
        assert!(item_for_filter(&sf("image.adjustments.invert", json!({"amount": 2}))).is_err());
    }

    #[test]
    fn unknown_filters_keep_their_descriptor() {
        let item = Descriptor::new("filterFX")
            .with("Nm  ", Value::Text(UnicodeString::new_nul("Twirl...")))
            .with("blendOptions", Value::Descriptor(Descriptor::new("blendOptions").with("Opct", unit(b"#Prc", 100.0)).with("Md  ", enumv("BlnM", "normal"))))
            .with("enab", Value::Boolean(true))
            .with("Fltr", Value::Descriptor(Descriptor::new("Twrl").with("Angl", Value::Integer(50))))
            .with("filterID", Value::Integer(code(b"Twrl")));
        let f = filter_from_item(&item);
        assert_eq!(f.command, UNSUPPORTED_FILTER);
        assert_eq!(f.params["name"], "Twirl...");
        assert_eq!(item_for_filter(&f).unwrap(), item);
        // Hidden and re-blended: only those keys change.
        let mut g = f.clone();
        g.visible = false;
        g.blend = BlendMode::Screen;
        let back = filter_from_item(&item_for_filter(&g).unwrap());
        assert_eq!((back.visible, back.blend, &back.params), (false, BlendMode::Screen, &f.params));
        // A known filter with a setting we don't model is kept verbatim too.
        let odd = Descriptor::new("filterFX").with("Fltr", Value::Descriptor(Descriptor::new("GsnB"))).with("filterID", Value::Integer(code(b"GsnB")));
        assert_eq!(filter_from_item(&odd).command, UNSUPPORTED_FILTER);
    }

    /// A Filter Gallery item in the list form other editors write (here Chrome effects).
    fn gallery_item_with(effects: Vec<Descriptor>) -> Descriptor {
        let rgb =
            |v: f64| Value::Descriptor(Descriptor::new("RGBC").with("Rd  ", Value::Double(v)).with("Grn ", Value::Double(v)).with("Bl  ", Value::Double(v)));
        Descriptor::new("filterFX")
            .with("Nm  ", Value::Text(UnicodeString::new_nul("Filtergalerie")))
            .with("blendOptions", Value::Descriptor(Descriptor::new("blendOptions").with("Opct", unit(b"#Prc", 100.0)).with("Md  ", enumv("BlnM", "normal"))))
            .with("enab", Value::Boolean(true))
            .with("hasoptions", Value::Boolean(true))
            .with("FrgC", rgb(0.0))
            .with("BckC", rgb(255.0))
            .with("Fltr", Value::Descriptor(Descriptor::new("GEfc").with("GEfs", Value::List(effects.into_iter().map(Value::Descriptor).collect()))))
            .with("filterID", Value::Integer(code(b"GEfc")))
    }

    fn chrome(detail: i32, smoothness: i32, visible: bool) -> Descriptor {
        Descriptor::new("GEfc")
            .with("Dtl ", Value::Integer(detail))
            .with("Smth", Value::Integer(smoothness))
            .with("GEfk", enumv("GEft", "Chrm"))
            .with("GELv", Value::Boolean(visible))
    }

    /// A `GEfs` stack maps effect by effect through the gallery table, with each effect's
    /// visibility, and is written back as a list.
    #[test]
    fn filter_gallery_stack_imports_as_editable_gallery() {
        let f = filter_from_item(&gallery_item_with(vec![chrome(12, 4, true), chrome(3, 9, false)]));
        assert_eq!(f.command, "filter.filterGallery");
        assert_eq!(
            f.params,
            json!({
                "effects": [
                    {"filter": "chrome", "params": {"detail": 12.0, "smoothness": 4.0}, "visible": true},
                    {"filter": "chrome", "params": {"detail": 3.0, "smoothness": 9.0}, "visible": false},
                ],
                "foreground": [0.0, 0.0, 0.0],
                "background": [1.0, 1.0, 1.0],
            })
        );
        // Written back and read again: unchanged, also when re-blended and hidden.
        let mut g = f.clone();
        g.blend = BlendMode::Multiply;
        g.opacity = 0.5;
        g.visible = false;
        let d = item_for_filter(&g).unwrap();
        assert_eq!(filter_from_item(&Descriptor::from_bytes(&d.to_bytes()).unwrap()), g);
        let fltr = get_desc(&d, "Fltr").unwrap();
        assert!(fltr.class_id.is("GEfc"));
        assert!(matches!(fltr.get("GEfs"), Some(Value::List(l)) if l.len() == 2));
        assert_eq!(int(&d, "filterID"), Some(code(b"GEfc")));
    }

    /// A single effect as the reference writes it: inline in `Fltr`, no `GEfs` list or `GELv`.
    fn reference_chrome(detail: i32, smoothness: i32) -> Descriptor {
        let rgb =
            |v: f64| Value::Descriptor(Descriptor::new("RGBC").with("Rd  ", Value::Double(v)).with("Grn ", Value::Double(v)).with("Bl  ", Value::Double(v)));
        let fltr = Descriptor::new("GEfc").with("GEfk", enumv("GEft", "Chrm")).with("Dtl ", Value::Integer(detail)).with("Smth", Value::Integer(smoothness));
        Descriptor::new("filterFX")
            .with("Nm  ", Value::Text(UnicodeString::new_nul("Filter Gallery...")))
            .with("blendOptions", Value::Descriptor(Descriptor::new("blendOptions").with("Opct", unit(b"#Prc", 100.0)).with("Md  ", enumv("BlnM", "Nrml"))))
            .with("enab", Value::Boolean(true))
            .with("hasoptions", Value::Boolean(true))
            .with("FrgC", rgb(255.0))
            .with("BckC", rgb(0.0))
            .with("Fltr", Value::Descriptor(fltr))
            .with("filterID", Value::Integer(code(b"GEfc")))
    }

    #[test]
    fn filter_gallery_inline_effect_imports_and_writes_back_inline() {
        let item = reference_chrome(10, 4);
        let f = filter_from_item(&item);
        assert_eq!(f.command, "filter.filterGallery");
        assert_eq!(
            f.params,
            json!({
                "effects": [{"filter": "chrome", "params": {"detail": 10.0, "smoothness": 4.0}}],
                "foreground": [1.0, 1.0, 1.0],
                "background": [0.0, 0.0, 0.0],
            })
        );
        // Written back in the reference's own layout.
        let d = item_for_filter(&f).unwrap();
        assert_eq!(get_desc(&d, "Fltr").map(|f| &f.items), get_desc(&item, "Fltr").map(|f| &f.items));
        assert_eq!((get_desc(&d, "FrgC"), get_desc(&d, "BckC")), (get_desc(&item, "FrgC"), get_desc(&item, "BckC")));
        assert_eq!(filter_from_item(&Descriptor::from_bytes(&d.to_bytes()).unwrap()), f);
        // A hidden single effect needs the list (only it has a visibility flag).
        let mut hidden = f.clone();
        hidden.params["effects"][0]["visible"] = json!(false);
        let d = item_for_filter(&hidden).unwrap();
        assert!(get_desc(&d, "Fltr").unwrap().get("GEfs").is_some());
        assert_eq!(filter_from_item(&d), hidden);
        // Inline effects we don't model stay verbatim.
        let mut odd = reference_chrome(10, 4);
        set(&mut odd, "Fltr", Value::Descriptor(Descriptor::new("GEfc").with("GEfk", enumv("GEft", "Ctot")).with("Dtl ", Value::Integer(1))));
        assert_eq!(filter_from_item(&odd).command, UNSUPPORTED_FILTER);
    }

    #[test]
    fn filter_gallery_written_with_defaults_and_colours() {
        let f = sf("filter.filterGallery", json!({"effects": [{"filter": "chrome"}], "foreground": [1.0, 0.0, 0.0], "background": [0.0, 0.0, 1.0]}));
        let back = filter_from_item(&item_for_filter(&f).unwrap());
        assert_eq!(back.params["effects"][0]["params"], json!({"detail": 4.0, "smoothness": 7.0}));
        assert_eq!(back.params["foreground"], json!([1.0, 0.0, 0.0]));
        assert_eq!(back.params["background"], json!([0.0, 0.0, 1.0]));
    }

    #[test]
    fn filter_gallery_we_dont_model_is_kept_verbatim() {
        // A list with an effect the table doesn't know, an empty list, a non-list.
        let other = Descriptor::new("GEfc").with("GEfk", enumv("GEft", "Ctot")).with("GELv", Value::Boolean(true));
        for effects in [vec![chrome(4, 7, true), other], vec![]] {
            let item = gallery_item_with(effects);
            let f = filter_from_item(&item);
            assert_eq!(f.command, UNSUPPORTED_FILTER);
            assert_eq!(item_for_filter(&f).unwrap(), item);
        }
        let odd = Descriptor::new("filterFX").with("Fltr", Value::Descriptor(Descriptor::new("GEfc").with("GEfs", Value::Integer(3))));
        assert_eq!(filter_from_item(&odd).command, UNSUPPORTED_FILTER);
        let bare = Descriptor::new("filterFX").with("filterID", Value::Integer(code(b"GEfc")));
        assert_eq!(filter_from_item(&bare).command, UNSUPPORTED_FILTER);
    }

    #[test]
    fn filter_gallery_without_psd_equivalent_is_reported() {
        for p in [
            json!({"effects": [{"params": {}}]}),
            json!({"effects": [{"filter": "chrome"}, {"params": {}}]}),
            json!({"effects": [{"filter": "chrome"}, {"filter": "noSuchEffect"}]}),
            json!({"effects": []}),
            json!({}),
            json!(null),
        ] {
            assert!(item_for_filter(&sf("filter.filterGallery", p.clone())).is_err(), "{p}");
        }
    }

    #[test]
    fn unwritable_filters_are_reported_not_fatal() {
        let stack = FilterStack {
            filters: vec![
                sf("filter.distort.twirl", json!({"angle": 50})),
                sf("filter.blur.gaussianBlur", json!({"radius": 2.0})),
                sf(UNSUPPORTED_FILTER, json!({"psd": "zz"})),
                sf("image.adjustments.levels", json!({"cyan": {"gamma": 2.0}})),
                sf("image.adjustments.shadowsHighlights", json!({"shadowAmount": 50})),
            ],
            ..Default::default()
        };
        let mut w = Vec::new();
        let d = filter_fx(&stack, "L", &mut w);
        assert_eq!(w.len(), 4, "{w:?}");
        let back = filter_stack(&d);
        assert_eq!(back.filters, vec![stack.filters[1].clone()]);
    }

    #[test]
    fn placed_blocks_parse_back() {
        let t = Affine { m: [0.5, 0.1, -0.1, 0.5, 10.0, 20.0] };
        let stack =
            FilterStack { enabled: false, filters: vec![sf("filter.blur.gaussianBlur", json!({"radius": 3.0}))], mask_enabled: false, mask_linked: true };
        let mut w = Vec::new();
        let spec = PlacedSpec {
            idnt: "id-1",
            placed: "pl-2",
            transform: t,
            perspective: None,
            size: (64.0, 32.0),
            dpi: 72.0,
            warp: None,
            filter_fx: Some(filter_fx(&stack, "L", &mut w)),
        };
        let sold = sold_bytes(None, &spec, &mut w);
        assert!(w.is_empty());
        assert_eq!(sold.len() % 4, 0);
        let p = parse_sold(&sold).unwrap();
        assert_eq!((p.idnt.as_str(), p.placed.as_str()), ("id-1", "pl-2"));
        for (a, b) in p.transform.m.iter().zip(t.m) {
            assert!((a - b).abs() < 1e-9);
        }
        assert_eq!(p.stack, Some(stack));
        let tb = photosplice_psd::TaggedBlock::new(*b"SoLd", sold.clone());
        tb.check_structure().unwrap();
        let pl = photosplice_psd::TaggedBlock::new(*b"PlLd", plld_bytes(&spec, &sold, &mut w));
        pl.check_structure().unwrap();
        // A template keeps its other keys; an unmoved transform keeps its exact quad.
        let mut tmpl = sold_descriptor(&sold).unwrap();
        tmpl.items.push((Id::new("extraKey"), Value::Integer(7)));
        let again = sold_bytes(Some(&tmpl), &PlacedSpec { filter_fx: None, ..spec }, &mut w);
        let d = sold_descriptor(&again).unwrap();
        assert_eq!(d.get("extraKey"), Some(&Value::Integer(7)));
        assert!(d.get("filterFX").is_none());
        assert_eq!(d.get("Trnf"), tmpl.get("Trnf"));
    }

    /// A Distort / Perspective placement goes out as its four corners (`Trnf`, `nonAffineTransform`)
    /// and comes back as the same projective map; an affine one reads back as no perspective.
    #[test]
    fn perspective_placement_round_trips_through_sold() {
        let h = photosplice_algo::transform::Homography::rect_to_quad([0.0, 0.0, 64.0, 32.0], [[10.0, 5.0], [80.0, 8.0], [70.0, 50.0], [12.0, 40.0]]).unwrap();
        let mut w = Vec::new();
        let mut spec = PlacedSpec {
            idnt: "id",
            placed: "pl",
            transform: Affine::IDENTITY,
            perspective: Some(h.0),
            size: (64.0, 32.0),
            dpi: 72.0,
            warp: None,
            filter_fx: None,
        };
        let sold = sold_bytes(None, &spec, &mut w);
        let back = crate::blocks::parse_smart_perspective(b"SoLd", &sold).expect("a projective placement");
        let n = |m: [f64; 9]| m.map(|v| v / m[8]);
        for (a, b) in n(back).iter().zip(n(h.0)) {
            assert!((a - b).abs() < 1e-9 * (1.0 + b.abs()), "{back:?} vs {:?}", h.0);
        }
        // Affine placements carry no perspective.
        spec.perspective = None;
        spec.transform = Affine { m: [2.0, 0.0, 0.0, 2.0, 5.0, 7.0] };
        let sold = sold_bytes(None, &spec, &mut w);
        assert_eq!(crate::blocks::parse_smart_perspective(b"SoLd", &sold), None);
        assert_eq!(crate::blocks::parse_smart_perspective(b"PlLd", &sold), None);
    }

    /// `quad_moved` compares the corners a template stores with the placement's: unchanged keeps
    /// the template's quad, a moved corner (affine or projective) or a missing quad rewrites it.
    #[test]
    fn quad_moved_detects_corner_changes() {
        let t = Affine { m: [1.0, 0.0, 0.0, 1.0, 3.0, 4.0] };
        let mut w = Vec::new();
        let mut spec = PlacedSpec { idnt: "id", placed: "pl", transform: t, perspective: None, size: (20.0, 10.0), dpi: 72.0, warp: None, filter_fx: None };
        let template = parse_sold(&sold_bytes(None, &spec, &mut w)).expect("parses").descriptor;
        assert!(!quad_moved(&template, &spec), "same placement");
        spec.transform = Affine { m: [1.0, 0.0, 0.0, 1.0, 3.5, 4.0] };
        assert!(quad_moved(&template, &spec), "moved half a pixel");
        spec.transform = t;
        spec.perspective =
            photosplice_algo::transform::Homography::rect_to_quad([0.0, 0.0, 20.0, 10.0], [[3.0, 4.0], [23.0, 4.0], [20.0, 12.0], [3.0, 14.0]]).map(|h| h.0);
        assert!(quad_moved(&template, &spec), "one corner moved (Distort)");
        assert!(quad_moved(&Descriptor::new("null"), &spec), "no stored quad");
    }

    /// Corrupted placed-layer data and filter descriptors never panic: they parse to something
    /// (unknown filters stay verbatim) or to nothing.
    #[test]
    fn mutated_placed_data_never_panics() {
        let stack = FilterStack {
            filters: vec![
                sf("filter.blur.gaussianBlur", json!({"radius": 3.0})),
                sf("image.adjustments.curves", json!({"points": [[0.0, 0.0], [255.0, 255.0]]})),
                sf("image.adjustments.levels", json!({"inBlack": 3, "inWhite": 250, "gamma": 1.1})),
            ],
            ..Default::default()
        };
        let mut w = Vec::new();
        let spec = PlacedSpec {
            idnt: "id",
            placed: "pl",
            transform: Affine::IDENTITY,
            perspective: None,
            size: (8.0, 8.0),
            dpi: 72.0,
            warp: None,
            filter_fx: Some(filter_fx(&stack, "L", &mut w)),
        };
        let sold = sold_bytes(None, &spec, &mut w);
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..3000 {
            let mut b = sold.clone();
            for _ in 0..1 + next() % 3 {
                let i = (next() as usize) % b.len();
                b[i] = next() as u8;
            }
            if let Some(p) = parse_sold(&b) {
                for f in p.stack.iter().flat_map(|s| &s.filters) {
                    let _ = item_for_filter(f);
                }
                let _ = sold_bytes(Some(&p.descriptor), &spec, &mut w);
            }
        }
    }

    /// `PlLd` carries the warp its `SoLd` carries: a warp kept from the file because the model
    /// reads it as none (here an identity custom mesh) is not written as `warpNone` in `PlLd`
    /// (Photoshop then showed no warp).
    #[test]
    fn plld_warp_follows_the_sold_warp() {
        use photosplice_geom::warp::BezierMesh;
        let bounds = [0.0, 0.0, 64.0, 64.0];
        let mut identity = Warp::preset(WarpStyle::Custom, 0.0, bounds);
        identity.mesh = Some(BezierMesh::identity(bounds, 1, 1));
        let mut w = Vec::new();
        let spec = PlacedSpec {
            idnt: "i",
            placed: "p",
            transform: Affine::IDENTITY,
            perspective: None,
            size: (64.0, 64.0),
            dpi: 72.0,
            warp: Some(&identity),
            filter_fx: None,
        };
        let tmpl = sold_descriptor(&sold_bytes(None, &spec, &mut w)).unwrap();
        assert_eq!(crate::blocks::warp_from_desc(get_desc(&tmpl, "warp").unwrap()), None, "the model holds no warp");
        let moved = PlacedSpec { transform: Affine { m: [1.0, 0.0, 0.0, 1.0, 1e-6, 0.0] }, warp: None, ..spec };
        let sold = sold_bytes(Some(&tmpl), &moved, &mut w);
        let plld = plld_bytes(&moved, &sold, &mut w);
        // plcL, version, Pascal id, page / pages / anti-alias / type, 8 doubles, warp version.
        let at = 8 + 1 + moved.idnt.len() + 16 + 64 + 4;
        let pl_warp = crate::blocks::parse_prefix_versioned(&plld[at..]).unwrap();
        assert_eq!(Some(&pl_warp), get_desc(&sold_descriptor(&sold).unwrap(), "warp"));
        assert_eq!(enum_of(&pl_warp, "warpStyle"), Some(&b"warpCustom"[..]));
        // Without a `SoLd` warp to follow, the model's warp is written.
        let plld = plld_bytes(&moved, &[], &mut w);
        assert_eq!(enum_of(&crate::blocks::parse_prefix_versioned(&plld[at..]).unwrap(), "warpStyle"), Some(&b"warpNone"[..]));
    }

    #[test]
    fn warps_are_written_and_read_back() {
        use photosplice_geom::warp::BezierMesh;
        let bounds = [0.0, 0.0, 60.0, 30.0];
        let mut custom = BezierMesh::identity(bounds, 1, 1);
        custom.points[15] = [67.0, 30.0];
        let mut multi = Warp::preset(WarpStyle::Custom, 0.0, bounds);
        multi.mesh = Some(BezierMesh::identity(bounds, 2, 2));
        let mut arc = Warp::preset(WarpStyle::Arc, 40.0, bounds);
        arc.vertical = true;
        arc.h_distort = 10.0;
        let mut c = Warp::preset(WarpStyle::Custom, 0.0, bounds);
        c.mesh = Some(custom);
        for (w, back) in [(Some(arc.clone()), Some(arc)), (Some(c.clone()), Some(c)), (Some(multi), None), (None, None)] {
            let mut warnings = Vec::new();
            let spec = PlacedSpec {
                idnt: "i",
                placed: "p",
                transform: Affine::IDENTITY,
                perspective: None,
                size: (60.0, 30.0),
                dpi: 72.0,
                warp: w.as_ref(),
                filter_fx: None,
            };
            let sold = sold_bytes(None, &spec, &mut warnings);
            assert_eq!(crate::blocks::parse_placed_warp(b"SoLd", &sold), back);
            assert_eq!(warnings.len(), usize::from(w.is_some() && back.is_none()));
        }
    }

    #[test]
    fn hex_and_uuid() {
        assert_eq!(from_hex(&to_hex(&[0, 1, 0xab, 0xff])), Some(vec![0, 1, 0xab, 0xff]));
        assert_eq!(from_hex("abc"), None);
        assert_eq!(from_hex("zz"), None);
        let u = uuid_from(b"x");
        assert_eq!(u.len(), 36);
        assert_eq!(u, uuid_from(b"x"));
        assert_ne!(u, uuid_from(b"y"));
    }
}
