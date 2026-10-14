//! Tagged-block helpers: the preserved block-list encoding, fills, text,
//! smart objects, locks and label colors.

use photosuite_color::{Color, ColorMode};
use photosuite_doc::{BlendIf, BlendRange, Fill, GradientStyle, LabelColor, Locks};
use photosuite_geom::Affine;
use photosuite_psd::descriptor::{Descriptor, Id, UnicodeString, Value, VersionedDescriptor};
use photosuite_psd::layer::BlendingRanges;

/// `brst` (channel blending restrictions): a list of big-endian u32 channel indices left out of
/// blending → a bit mask (bit `i` = channel `i`; indices above 31 are ignored).
pub fn parse_brst(data: &[u8]) -> u32 {
    data.as_chunks::<4>().0.iter().map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]])).filter(|&i| i < 32).fold(0, |m, i| m | 1 << i)
}

/// Inverse of [`parse_brst`]; `None` when every channel blends (no block).
pub fn brst_data(mask: u32) -> Option<Vec<u8>> {
    (mask != 0).then(|| (0..32u32).filter(|i| mask & 1 << i != 0).flat_map(u32::to_be_bytes).collect())
}

/// Layer-record blending ranges (Adobe PSD spec, "Layer blending ranges data": the composite
/// gray source and destination ranges, then a source and destination range per channel, each
/// as black low, black high, white low, white high) → Blend If. Full ranges map to the
/// default, so files without Blend If keep an empty setting.
pub fn blend_if_from_ranges(r: &BlendingRanges) -> BlendIf {
    let mut ranges: Vec<[BlendRange; 2]> = r.ranges().iter().map(|e| [BlendRange::from_bytes(e.source), BlendRange::from_bytes(e.dest)]).collect();
    // Trailing full entries carry nothing (export pads them back), so they are dropped, as
    // `BlendIf::set` does.
    let keep = ranges.iter().rposition(|p| !p.iter().all(BlendRange::is_full)).map_or(0, |i| i + 1);
    ranges.truncate(keep);
    BlendIf { ranges }
}

/// Inverse of [`blend_if_from_ranges`] for a document with `channels` colour channels: one
/// entry for gray plus one per channel (more when the setting stores more), unset entries full.
/// Photoshop writes at least five entries in every colour mode, Grayscale included (observed on
/// every Photoshop-authored layer of the oracle corpus).
pub fn ranges_from_blend_if(b: &BlendIf, channels: usize) -> BlendingRanges {
    let channels = channels.max(4);
    if b.is_default() {
        return BlendingRanges::full(channels);
    }
    let n = b.ranges.len().max(channels + 1);
    let data = (0..n).flat_map(|i| {
        let [src, dst] = b.get(i);
        src.to_bytes().into_iter().chain(dst.to_bytes())
    });
    BlendingRanges { data: data.collect() }
}

/// `lspf` bits (Adobe spec: bit 0 transparency, 1 composite, 2 position).
/// Artboard (bit 3, `0x08`, as reference files write it) and "all" (bit 31) are undocumented.
/// Older PhotoSuite builds wrote the artboard lock at bit 4 (`0x10`); that value is still
/// accepted on read so those files keep their lock, and is rewritten at `0x08`.
pub fn locks_from_lspf(v: u32) -> Locks {
    Locks { transparency: v & 1 != 0, pixels: v & 2 != 0, position: v & 4 != 0, artboard: v & 0x08 != 0 || v & 0x10 != 0, all: v & 0x8000_0000 != 0 }
}

/// Inverse of [`locks_from_lspf`].
pub fn lspf_from_locks(l: &Locks) -> u32 {
    u32::from(l.transparency) | u32::from(l.pixels) << 1 | u32::from(l.position) << 2 | u32::from(l.artboard) << 3 | u32::from(l.all) << 31
}

/// `lclr` index → label.
pub fn label_from_index(v: u16) -> LabelColor {
    match v {
        1 => LabelColor::Red,
        2 => LabelColor::Orange,
        3 => LabelColor::Yellow,
        4 => LabelColor::Green,
        5 => LabelColor::Blue,
        6 => LabelColor::Violet,
        7 => LabelColor::Gray,
        _ => LabelColor::None,
    }
}

/// Label → `lclr` index.
pub fn label_index(l: LabelColor) -> u16 {
    match l {
        LabelColor::None => 0,
        LabelColor::Red => 1,
        LabelColor::Orange => 2,
        LabelColor::Yellow => 3,
        LabelColor::Green => 4,
        LabelColor::Blue => 5,
        LabelColor::Violet => 6,
        LabelColor::Gray => 7,
    }
}

pub(crate) fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Double(d) => Some(*d),
        Value::UnitFloat { value, .. } => Some(*value),
        Value::Integer(i) => Some(f64::from(*i)),
        _ => None,
    }
}

/// Descriptor color (`RGBC`, `CMYC`, `Grsc`, `HSBC`) → [`Color`].
pub fn color_from_desc(d: &Descriptor) -> Option<Color> {
    let id = d.class_id.as_bytes();
    let g = |k: &str| num(d.get(k)).map(|v| v as f32);
    Some(match id {
        // Newer Photoshop writes 0..1 floats (`RGBC` with redFloat / greenFloat / blueFloat).
        b"RGBC" if d.get("redFloat").is_some() => Color::rgb(g("redFloat")?, g("greenFloat")?, g("blueFloat")?),
        b"RGBC" => Color::rgb(g("Rd  ")? / 255.0, g("Grn ")? / 255.0, g("Bl  ")? / 255.0),
        b"CMYC" => Color { mode: ColorMode::Cmyk, c: [g("Cyn ")? / 100.0, g("Mgnt")? / 100.0, g("Ylw ")? / 100.0, g("Blck")? / 100.0], alpha: 1.0 },
        b"Grsc" => Color::gray(1.0 - g("Gry ")? / 100.0),
        // HSB: hue in degrees (`H   `, unit `#Ang`), saturation and brightness 0..100.
        b"HSBC" => {
            let [r, gr, b] = hsb_to_rgb(g("H   ")?, g("Strt")? / 100.0, g("Brgh")? / 100.0);
            Color::rgb(r, gr, b)
        }
        // CIE L*a*b*: L 0..100, a / b −128..127 (stored like Lab pixels: offset by 128).
        b"LbCl" => Color { mode: ColorMode::Lab, c: [g("Lmnc")? / 100.0, (g("A   ")? + 128.0) / 255.0, (g("B   ")? + 128.0) / 255.0, 0.0], alpha: 1.0 },
        // Newer Photoshop writes 0..1 floats.
        _ if d.get("redFloat").is_some() => Color::rgb(g("redFloat")?, g("greenFloat")?, g("blueFloat")?),
        _ => {
            // Fall back to any RGB-like triple.
            Color::rgb(g("Rd  ")? / 255.0, g("Grn ")? / 255.0, g("Bl  ")? / 255.0)
        }
    })
}

/// HSB (hue in degrees, saturation and brightness 0..1) → RGB 0..1.
pub(crate) fn hsb_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let (s, v) = (s.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
    let h = if h.is_finite() { h.rem_euclid(360.0) / 60.0 } else { 0.0 };
    let f = h - h.floor();
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match h as u32 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

/// [`Color`] → descriptor in its own model.
pub fn color_to_desc(c: &Color) -> Descriptor {
    let d = |v: f32| Value::Double(f64::from(v));
    match c.mode {
        ColorMode::Cmyk => Descriptor::new("CMYC")
            .with("Cyn ", d(c.c[0] * 100.0))
            .with("Mgnt", d(c.c[1] * 100.0))
            .with("Ylw ", d(c.c[2] * 100.0))
            .with("Blck", d(c.c[3] * 100.0)),
        ColorMode::Grayscale => Descriptor::new("Grsc").with("Gry ", d((1.0 - c.c[0]) * 100.0)),
        ColorMode::Lab => {
            Descriptor::new("LbCl").with("Lmnc", d(c.c[0] * 100.0)).with("A   ", d(c.c[1] * 255.0 - 128.0)).with("B   ", d(c.c[2] * 255.0 - 128.0))
        }
        _ => {
            let rgb = c.to_rgb();
            Descriptor::new("RGBC").with("Rd  ", d(rgb[0] * 255.0)).with("Grn ", d(rgb[1] * 255.0)).with("Bl  ", d(rgb[2] * 255.0))
        }
    }
}

/// Nesting depth past which [`merge_descriptor`] stops merging and takes the regenerated value.
const MERGE_DEPTH: usize = 32;

/// Merges a regenerated descriptor onto the one the file had, so a rewrite keeps what the
/// document model doesn't carry ("merge, don't rebuild"). Keys of `over` win, except those named
/// in `keep`, whose original value stays. Keys only in `base` are kept. The result follows
/// `base`'s key order, then `over`'s new keys. Nested descriptors of the same class merge
/// recursively, as do lists of the same length; anything else takes `over`'s value.
pub(crate) fn merge_descriptor(base: &Descriptor, over: Descriptor, keep: &[&str]) -> Descriptor {
    merge_desc(base, over, keep, 0)
}

fn merge_desc(base: &Descriptor, mut over: Descriptor, keep: &[&str], depth: usize) -> Descriptor {
    if depth > MERGE_DEPTH {
        return over;
    }
    let mut items = Vec::with_capacity(base.items.len().max(over.items.len()));
    for (k, bv) in &base.items {
        match over.items.iter().position(|(ok, _)| ok == k) {
            Some(i) => {
                let (ok, ov) = over.items.remove(i);
                let v = if keep.iter().any(|s| k.is(s)) { bv.clone() } else { merge_value(bv, ov, keep, depth + 1) };
                items.push((ok, v));
            }
            None => items.push((k.clone(), bv.clone())),
        }
    }
    items.append(&mut over.items);
    Descriptor { name: over.name, class_id: over.class_id, items }
}

fn merge_value(base: &Value, over: Value, keep: &[&str], depth: usize) -> Value {
    match (base, over) {
        (Value::Descriptor(b), Value::Descriptor(o)) if b.class_id == o.class_id => Value::Descriptor(merge_desc(b, o, keep, depth)),
        (Value::List(b), Value::List(o)) if b.len() == o.len() => {
            Value::List(b.iter().zip(o).map(|(bv, ov)| merge_value(bv, ov, keep, depth + 1)).collect())
        }
        (_, o) => o,
    }
}

pub(crate) fn get_desc<'a>(d: &'a Descriptor, key: &str) -> Option<&'a Descriptor> {
    match d.get(key)? {
        Value::Descriptor(x) | Value::GlobalObject(x) => Some(x),
        _ => None,
    }
}

/// Enumerated value of `key` (4-char or long id), if present.
pub(crate) fn enum_of<'a>(d: &'a Descriptor, key: &str) -> Option<&'a [u8]> {
    match d.get(key)? {
        Value::Enumerated { value, .. } => Some(value.as_bytes()),
        _ => None,
    }
}

/// A gradient's interpolation method code. Reference (27.11, checked upstream by saving gradient
/// fill layers) stores it as the enum `gradientsInterpolationMethod` (`Perc`, `Lnr `, `Smoo`,
/// `Gcls`); `gs99` stays a fallback. Only `gs99` was read, so every Perceptual, Linear or Smooth
/// gradient opened as Classic, up to 74 levels off once re-rendered (storytold/photocraft@9237a8bd).
pub(crate) fn gradient_method(d: &Descriptor) -> Option<&[u8]> {
    enum_of(d, "gradientsInterpolationMethod").or_else(|| enum_of(d, "gs99"))
}

pub(crate) fn bool_of(d: &Descriptor, key: &str) -> bool {
    matches!(d.get(key), Some(Value::Boolean(true)))
}

/// `GrdT` gradient type.
pub(crate) fn gradient_style(d: &Descriptor) -> GradientStyle {
    match enum_of(d, "Type") {
        Some(b"Rdl ") => GradientStyle::Radial,
        Some(b"Angl") => GradientStyle::Angle,
        Some(b"Rflc") => GradientStyle::Reflected,
        Some(b"Dmnd") => GradientStyle::Diamond,
        _ => GradientStyle::Linear,
    }
}

pub(crate) fn gradient_style_value(s: GradientStyle) -> Value {
    let v = match s {
        GradientStyle::Linear => "Lnr ",
        GradientStyle::Radial => "Rdl ",
        GradientStyle::Angle => "Angl",
        GradientStyle::Reflected => "Rflc",
        GradientStyle::Diamond => "Dmnd",
    };
    Value::Enumerated { type_id: Id::new("GrdT"), value: Id::new(v) }
}

/// Colour and opacity stops of a `Grdn` descriptor (locations 0..4096 → 0..1).
/// Colour stops and opacity stops of a gradient.
pub(crate) type Stops = (Vec<(f32, Color)>, Vec<(f32, f32)>);

/// Stops of a `Grdn` descriptor, baked to Photoshop's interpolation (smoothness `Intr`, stop
/// midpoints, and the parent's `gs99` interpolation method when given).
pub(crate) fn gradient_stops_with(grad: &Descriptor, method: Option<&[u8]>) -> Stops {
    let raw = gradient_stops_raw(grad);
    let stops = if raw.stops.len() >= 2 && !raw.noise {
        crate::gradient_bake::bake(raw.stops, &raw.mids, raw.smooth, crate::gradient_bake::Method::from_code(method))
    } else {
        raw.stops
    };
    (stops, raw.opacity)
}

/// A `Grdn` descriptor's stops as stored: colour stops (sorted), the midpoint of each segment
/// between them, opacity stops and the smoothness (`Intr`, `0..=1`).
pub(crate) struct RawStops {
    pub stops: Vec<(f32, Color)>,
    pub mids: Vec<f32>,
    pub opacity: Vec<(f32, f32)>,
    pub smooth: f32,
    /// A generated noise gradient (dense stops; never baked).
    pub noise: bool,
}

/// Stops of a gradient fill layer, kept editable: when Photoshop's interpolation is plain
/// (no smoothness, Classic method) the stops stay as they are with their midpoints, else they
/// are baked like [`gradient_stops_with`] (midpoints folded in). Opacity stops that are all
/// 100 % come back empty.
pub(crate) fn gradient_stops_editable(grad: &Descriptor, method: Option<&[u8]>) -> RawStops {
    let mut raw = gradient_stops_raw(grad);
    let m = crate::gradient_bake::Method::from_code(method);
    // Lab stops (Lab documents) interpolate in L*a*b*, which only baking reproduces.
    let lab = raw.stops.iter().all(|(_, c)| c.mode == photosuite_color::ColorMode::Lab);
    if raw.stops.len() >= 2 && !raw.noise && (raw.smooth > 0.0 || m != crate::gradient_bake::Method::Classic || lab) {
        raw.stops = crate::gradient_bake::bake_keeping_inks(std::mem::take(&mut raw.stops), &raw.mids, raw.smooth, m);
        raw.mids.clear();
    }
    if raw.mids.iter().all(|m| (m - 0.5).abs() < 1e-3) {
        raw.mids.clear();
    }
    if raw.opacity.iter().all(|o| (o.1 - 1.0).abs() < 1e-4) {
        raw.opacity.clear();
    }
    raw
}

fn gradient_stops_raw(grad: &Descriptor) -> RawStops {
    // Noise gradients have no stops: an approximation of Photoshop's random ramp, generated.
    if let Some(n) = crate::noise_gradient::Noise::from_desc(grad) {
        let (stops, opacity) = n.stops();
        return RawStops { stops, mids: Vec::new(), opacity, smooth: 0.0, noise: true };
    }
    let mut stops = Vec::new();
    let mut mids = Vec::new();
    let mut opacity = Vec::new();
    if let Some(Value::List(items)) = grad.get("Clrs") {
        for it in items {
            if let Value::Descriptor(s) = it {
                let loc = num(s.get("Lctn")).unwrap_or(0.0) as f32 / 4096.0;
                if let Some(c) = get_desc(s, "Clr ").and_then(color_from_desc) {
                    stops.push((loc, c));
                    mids.push(num(s.get("Mdpn")).unwrap_or(50.0) as f32 / 100.0);
                }
            }
        }
    }
    // Midpoint k applies to the segment after stop k.
    let mut order: Vec<usize> = (0..stops.len()).collect();
    order.sort_by(|a, b| stops[*a].0.total_cmp(&stops[*b].0));
    let mids: Vec<f32> = order.iter().skip(1).map(|i| mids.get(*i).copied().unwrap_or(0.5)).collect();
    let smooth = num(grad.get("Intr")).map_or(0.0, |v| (v / 4096.0) as f32);
    let mut sorted: Vec<(f32, Color)> = order.iter().filter_map(|i| stops.get(*i).cloned()).collect();
    if sorted.len() != stops.len() {
        sorted = stops;
    }
    if let Some(Value::List(items)) = grad.get("Trns") {
        for it in items {
            if let Value::Descriptor(s) = it {
                let loc = num(s.get("Lctn")).unwrap_or(0.0) as f32 / 4096.0;
                opacity.push((loc, num(s.get("Opct")).unwrap_or(100.0) as f32 / 100.0));
            }
        }
    }
    RawStops { stops: sorted, mids, opacity, smooth, noise: false }
}

/// A `Grdn` descriptor for colour and opacity stops.
pub(crate) fn gradient_desc(stops: &[(f32, Color)], opacity: &[(f32, f32)]) -> Descriptor {
    gradient_desc_with(stops, opacity, &[])
}

/// [`gradient_desc`] with colour midpoints (one per segment between the sorted stops; each is
/// stored on the stop that ends its segment, as [`gradient_stops_with`] reads it).
pub(crate) fn gradient_desc_with(stops: &[(f32, Color)], opacity: &[(f32, f32)], mids: &[f32]) -> Descriptor {
    let mut stops = stops.to_vec();
    if !mids.is_empty() {
        stops.sort_by(|a, b| a.0.total_cmp(&b.0));
    }
    let clrs = stops
        .iter()
        .enumerate()
        .map(|(i, (t, c))| {
            let mid = i.checked_sub(1).and_then(|k| mids.get(k)).copied().unwrap_or(0.5);
            Value::Descriptor(
                Descriptor::new("Clrt")
                    .with("Clr ", Value::Descriptor(color_to_desc(c)))
                    .with("Type", Value::Enumerated { type_id: Id::new("Clry"), value: Id::new("UsrS") })
                    .with("Lctn", Value::Integer((t * 4096.0).round() as i32))
                    .with("Mdpn", Value::Integer((mid.clamp(0.05, 0.95) * 100.0).round() as i32)),
            )
        })
        .collect();
    let op: Vec<(f32, f32)> = if opacity.is_empty() { stops.iter().map(|(t, c)| (*t, c.alpha)).collect() } else { opacity.to_vec() };
    let trns = op
        .iter()
        .map(|(t, a)| {
            Value::Descriptor(
                Descriptor::new("TrnS")
                    .with("Opct", Value::UnitFloat { unit: *b"#Prc", value: f64::from(a * 100.0) })
                    .with("Lctn", Value::Integer((t * 4096.0).round() as i32))
                    .with("Mdpn", Value::Integer(50)),
            )
        })
        .collect();
    Descriptor::new("Grdn")
        .with("Nm  ", Value::Text(UnicodeString::new_nul("Custom")))
        .with("GrdF", Value::Enumerated { type_id: Id::new("GrdF"), value: Id::new("CstS") })
        // Our stops interpolate linearly (Photoshop smoothness is baked into them on import).
        .with("Intr", Value::Double(0.0))
        .with("Clrs", Value::List(clrs))
        .with("Trns", Value::List(trns))
}

/// Opacity stops of a `Grdn` descriptor with Photoshop's smoothness and midpoints applied (the
/// same curve as the colour stops, Classic interpolation), densely sampled.
fn smoothed_opacity_stops(grad: &Descriptor) -> Vec<(f32, f32)> {
    if let Some(n) = crate::noise_gradient::Noise::from_desc(grad) {
        return n.stops().1;
    }
    let mut stops = Vec::new();
    let mut mids = Vec::new();
    if let Some(Value::List(items)) = grad.get("Trns") {
        for it in items {
            if let Value::Descriptor(s) = it {
                let a = num(s.get("Opct")).unwrap_or(100.0) as f32 / 100.0;
                stops.push((num(s.get("Lctn")).unwrap_or(0.0) as f32 / 4096.0, Color::rgba(a, a, a, 1.0)));
                mids.push(num(s.get("Mdpn")).unwrap_or(50.0) as f32 / 100.0);
            }
        }
    }
    let mut order: Vec<usize> = (0..stops.len()).collect();
    order.sort_by(|a, b| stops[*a].0.total_cmp(&stops[*b].0));
    let mids: Vec<f32> = order.iter().skip(1).filter_map(|i| mids.get(*i).copied()).collect();
    let smooth = num(grad.get("Intr")).map_or(0.0, |v| (v / 4096.0) as f32);
    let baked = if stops.len() >= 2 { crate::gradient_bake::bake(stops, &mids, smooth, crate::gradient_bake::Method::Classic) } else { stops };
    baked.into_iter().map(|(t, c)| (t, c.to_rgb()[0])).collect()
}

/// Parses a fill block (`SoCo`, `GdFl`, `PtFl`).
pub fn parse_fill(key: &[u8; 4], data: &[u8]) -> Option<Fill> {
    // Block data may carry trailing padding after the descriptor.
    let d = parse_prefix_versioned(data)?;
    fill_from_desc(key, &d)
}

/// A fill from its descriptor (`SoCo`/`GdFl`/`PtFl` layout; also the `solidColorLayer`,
/// `gradientLayer` and `patternLayer` contents of shape strokes).
pub fn fill_from_desc(key: &[u8; 4], d: &Descriptor) -> Option<Fill> {
    match key {
        b"SoCo" => Some(Fill::Solid(color_from_desc(get_desc(d, "Clr ")?)?)),
        b"GdFl" => {
            let angle = num(d.get("Angl")).unwrap_or(90.0) as f32;
            let scale = num(d.get("Scl ")).map_or(1.0, |v| v as f32 / 100.0);
            let raw = get_desc(d, "Grad").map(|g| gradient_stops_editable(g, gradient_method(d)));
            let (mut stops, midpoints) = raw.map(|r| (r.stops, r.mids)).unwrap_or_default();
            // Opacity stops with Photoshop's smoothness and opacity midpoints applied (raw when
            // the interpolation is plain); none when fully opaque.
            let mut opacity_stops = get_desc(d, "Grad").map(smoothed_opacity_stops).unwrap_or_default();
            if opacity_stops.iter().all(|o| o.1 >= 1.0 - 1e-4) {
                opacity_stops.clear();
            }
            if stops.is_empty() {
                stops = vec![(0.0, Color::BLACK), (1.0, Color::WHITE)];
            }
            let pct = |o: &Descriptor, k: &str| num(o.get(k)).map_or(0.0, |v| v as f32 / 100.0);
            let offset = get_desc(d, "Ofst").map_or((0.0, 0.0), |o| (pct(o, "Hrzn"), pct(o, "Vrtc")));
            Some(Fill::Gradient {
                stops,
                angle,
                scale,
                style: gradient_style(d),
                reverse: bool_of(d, "Rvrs"),
                opacity_stops,
                midpoints,
                offset,
                dither: bool_of(d, "Dthr"),
                align: !matches!(d.get("Algn"), Some(Value::Boolean(false))),
            })
        }
        b"PtFl" => {
            let (name, id) = pattern_ref(d);
            let scale = num(d.get("Scl ")).map_or(1.0, |v| v as f32 / 100.0);
            let (angle, link, phase) = pattern_placement(d);
            Some(Fill::Pattern { name, scale, id, angle, link, phase })
        }
        _ => None,
    }
}

/// Serializes a fill to its block.
pub fn write_fill(f: &Fill) -> ([u8; 4], Vec<u8>) {
    let (k, d) = fill_to_desc(f);
    (k, VersionedDescriptor::new(d).to_bytes())
}

/// A fill as (block key, descriptor), merged onto the descriptor of the block it replaces when
/// that is the same kind of fill (`old`: key and descriptor bytes, version prefix included), so
/// what the model doesn't carry survives (gradient interpolation method, noise seed…).
pub fn fill_to_desc_onto(f: &Fill, old: Option<(&[u8; 4], &[u8])>) -> ([u8; 4], Descriptor) {
    let (k, mut d) = fill_to_desc(f);
    let Some(base) = old.filter(|(ok, _)| **ok == k).and_then(|(_, data)| parse_prefix_versioned(data)) else { return (k, d) };
    // Keys written only when not at their default: a file that has one gets the default spelled
    // out, or the merge would keep its old value (a reversed gradient stayed reversed).
    let prc = |v: f64| Value::UnitFloat { unit: *b"#Prc", value: v };
    let defaults = match f {
        Fill::Gradient { .. } => vec![
            ("Scl ", prc(100.0)),
            ("Rvrs", Value::Boolean(false)),
            ("Dthr", Value::Boolean(false)),
            ("Algn", Value::Boolean(true)),
            ("Ofst", Value::Descriptor(Descriptor::new("Pnt ").with("Hrzn", prc(0.0)).with("Vrtc", prc(0.0)))),
        ],
        Fill::Pattern { .. } => vec![("Angl", Value::UnitFloat { unit: *b"#Ang", value: 0.0 })],
        Fill::Solid(_) => Vec::new(),
    };
    for (key, v) in defaults {
        if base.get(key).is_some() && d.get(key).is_none() {
            d.items.push((Id::new(key), v));
        }
    }
    // A pattern fill's scale, link and phase at their defaults stay out of a file that left them
    // out (older files and other writers do).
    if let Fill::Pattern { scale, link, phase, .. } = f {
        let unstated = [("Scl ", *scale == 1.0), ("Algn", *link), ("phase", *phase == (0.0, 0.0))];
        d.items.retain(|(k, _)| !unstated.iter().any(|(key, default)| *default && k.is(key) && base.get(key).is_none()));
    }
    // A gradient whose stops weren't edited keeps the file's definition verbatim: its colour
    // model (CMYK, gray, Lab stops), a noise gradient, the name and interpolation the model
    // bakes into its stops.
    let same_stops = match (fill_from_desc(&k, &base), f) {
        (
            Some(Fill::Gradient { stops: s0, opacity_stops: o0, midpoints: m0, .. }),
            Fill::Gradient { stops: s1, opacity_stops: o1, midpoints: m1, .. },
        ) => s0 == *s1 && o0 == *o1 && m0 == *m1,
        _ => false,
    };
    (k, merge_descriptor(&base, d, if same_stops { &["Grad"] } else { &[] }))
}

/// A fill as (block key, descriptor with class `null`).
pub fn fill_to_desc(f: &Fill) -> ([u8; 4], Descriptor) {
    match f {
        Fill::Solid(c) => (*b"SoCo", Descriptor::new("null").with("Clr ", Value::Descriptor(color_to_desc(c)))),
        Fill::Gradient { stops, angle, scale, style, reverse, opacity_stops, midpoints, offset, dither, align } => {
            let prc = |v: f32| Value::UnitFloat { unit: *b"#Prc", value: f64::from(v * 100.0) };
            let mut d = Descriptor::new("null")
                .with("Angl", Value::UnitFloat { unit: *b"#Ang", value: f64::from(*angle) })
                .with("Type", gradient_style_value(*style));
            // Photoshop writes the scale only when it isn't 100 %.
            if *scale != 1.0 {
                d = d.with("Scl ", prc(*scale));
            }
            d = d.with("Grad", Value::Descriptor(gradient_desc_with(stops, opacity_stops, midpoints)));
            if *reverse {
                d = d.with("Rvrs", Value::Boolean(true));
            }
            if *dither {
                d = d.with("Dthr", Value::Boolean(true));
            }
            if !*align {
                d = d.with("Algn", Value::Boolean(false));
            }
            if *offset != (0.0, 0.0) {
                d = d.with("Ofst", Value::Descriptor(Descriptor::new("Pnt ").with("Hrzn", prc(offset.0)).with("Vrtc", prc(offset.1))));
            }
            (*b"GdFl", d)
        }
        Fill::Pattern { name, scale, id, angle, link, phase } => {
            let d = Descriptor::new("null")
                .with("Scl ", Value::UnitFloat { unit: *b"#Prc", value: f64::from(scale * 100.0) })
                .with("Ptrn", Value::Descriptor(pattern_ref_desc(name, id)));
            (*b"PtFl", with_pattern_placement(d, *angle, *link, *phase))
        }
    }
}

/// Pattern name and id (`Ptrn` → `Nm  `, `Idnt`) of a pattern fill / overlay descriptor.
pub(crate) fn pattern_ref(d: &Descriptor) -> (String, String) {
    let p = get_desc(d, "Ptrn");
    let text = |k: &str| match p.and_then(|p| p.get(k)) {
        Some(Value::Text(t)) => t.to_string_lossy(),
        _ => String::new(),
    };
    (text("Nm  "), text("Idnt"))
}

/// The `Ptrn` reference descriptor (the id is omitted when unknown).
pub(crate) fn pattern_ref_desc(name: &str, id: &str) -> Descriptor {
    let p = Descriptor::new("Ptrn").with("Nm  ", Value::Text(UnicodeString::new_nul(name)));
    if id.is_empty() { p } else { p.with("Idnt", Value::Text(UnicodeString::new_nul(id))) }
}

/// Angle (`Angl`, degrees), link with layer (`Algn`, default on) and phase (`phase` point) of a
/// pattern fill / overlay descriptor.
pub(crate) fn pattern_placement(d: &Descriptor) -> (f32, bool, (f32, f32)) {
    let angle = num(d.get("Angl")).unwrap_or(0.0) as f32;
    let link = !matches!(d.get("Algn"), Some(Value::Boolean(false)));
    let phase = get_desc(d, "phase").map_or((0.0, 0.0), |p| (num(p.get("Hrzn")).unwrap_or(0.0) as f32, num(p.get("Vrtc")).unwrap_or(0.0) as f32));
    (angle, link, phase)
}

/// Adds the keys read by [`pattern_placement`] (angle only when non-zero).
pub(crate) fn with_pattern_placement(mut d: Descriptor, angle: f32, link: bool, phase: (f32, f32)) -> Descriptor {
    if angle != 0.0 {
        d = d.with("Angl", Value::UnitFloat { unit: *b"#Ang", value: f64::from(angle) });
    }
    d.with("Algn", Value::Boolean(link)).with(
        "phase",
        Value::Descriptor(Descriptor::new("Pnt ").with("Hrzn", Value::Double(f64::from(phase.0))).with("Vrtc", Value::Double(f64::from(phase.1)))),
    )
}

/// The warp of a placed layer (`SoLd`/`SoLE` `warp` descriptor): style, bend, distortions,
/// axis, bounds and, for `warpCustom`, the 4 × 4 `customEnvelopeWarp` mesh. Read-only: the raw
/// block is what gets written back. `None` when absent or `warpNone` with no mesh.
/// The source-space box Reference maps onto a placed layer's `Trnf` quad: the bounding box of a
/// single-patch custom warp's 16 control points, which may spill past the source, else the source
/// rect `0..w × 0..h`. Mapping the source rect instead counted the overhang twice and stretched
/// the placement (storytold/photocraft@5ed75876). A degenerate or non-finite box falls back to the
/// source rect.
pub(crate) fn placed_frame(warp: Option<&photosuite_geom::warp::Warp>, w: f64, h: f64) -> [f64; 4] {
    let rect = [0.0, 0.0, w, h];
    let Some(m) = warp.filter(|wp| wp.style == photosuite_geom::warp::WarpStyle::Custom).and_then(|wp| wp.mesh.as_ref()).filter(|m| m.points.len() == 16)
    else {
        return rect;
    };
    let inf = f64::INFINITY;
    let b = m.points.iter().fold([inf, inf, -inf, -inf], |b, [x, y]| [b[0].min(*x), b[1].min(*y), b[2].max(*x), b[3].max(*y)]);
    let ok = b.iter().all(|v| v.is_finite()) && b[2] - b[0] > 1e-9 && b[3] - b[1] > 1e-9;
    if ok { b } else { rect }
}

pub fn parse_placed_warp(key: &[u8; 4], data: &[u8]) -> Option<photosuite_geom::warp::Warp> {
    if key != b"SoLd" && key != b"SoLE" {
        return None;
    }
    let d = data.get(8..).and_then(parse_prefix_versioned)?;
    warp_from_desc(get_desc(&d, "warp")?)
}

/// A placed layer's `warp` descriptor as a [`photosuite_geom::warp::Warp`]: `None` for no warp
/// and for warps the model can't hold (a multi-patch mesh, "quilt").
pub(crate) fn warp_from_desc(w: &Descriptor) -> Option<photosuite_geom::warp::Warp> {
    warp_of(w).flatten()
}

/// A `warp` descriptor (the `SoLd` key, or the whole descriptor after a `PlLd` record's corners)
/// as a warp: `Some(None)` for no warp (`warpNone`, or a warp that moves nothing), `None` when it
/// can't be read (an unknown style, missing bounds, a mesh the model can't hold).
fn warp_of(w: &Descriptor) -> Option<Option<photosuite_geom::warp::Warp>> {
    use photosuite_geom::warp::{BezierMesh, Warp, WarpStyle};
    let style = enum_of(w, "warpStyle").and_then(|v| WarpStyle::parse(&String::from_utf8_lossy(v)))?;
    if style == WarpStyle::None {
        return Some(None);
    }
    let b = get_desc(w, "bounds")?;
    let g = |k: &str| num(b.get(k));
    let bounds = [g("Left")?, g("Top ")?, g("Rght")?, g("Btom")?];
    let mut warp = Warp::preset(style, num(w.get("warpValue")).unwrap_or(0.0), bounds);
    warp.h_distort = num(w.get("warpPerspective")).unwrap_or(0.0);
    warp.v_distort = num(w.get("warpPerspectiveOther")).unwrap_or(0.0);
    warp.vertical = enum_of(w, "warpRotate") == Some(b"Vrtc");
    if style == WarpStyle::Custom {
        let env = get_desc(w, "customEnvelopeWarp")?;
        let Some(Value::ObjectArray(arr)) = env.get("meshPoints") else { return None };
        let vals = |k: &str| match arr.body.get(k) {
            Some(Value::UnitFloats { values, .. }) => Some(values.clone()),
            _ => None,
        };
        let (xs, ys) = (vals("Hrzn")?, vals("Vrtc")?);
        if xs.len() != ys.len() {
            return None;
        }
        // Reference stores most preset warps as a lower-order patch (Arc: 4 × 2 control points,
        // Inflate 3 × 3, Wave 4 × 3), raised here to the 4 × 4 grid exactly.
        let order = |k: &str| match w.get(k) {
            Some(Value::Integer(n)) => usize::try_from(*n).ok(),
            None => Some(4),
            _ => None,
        };
        let points: Vec<[f64; 2]> = xs.iter().zip(&ys).map(|(x, y)| [*x, *y]).collect();
        warp.mesh = BezierMesh::from_patch(order("uOrder")?, order("vOrder")?, &points);
        warp.mesh.as_ref()?;
    }
    Some((!warp.is_identity()).then_some(warp))
}

/// Text content and transform from a `TySh` block (best effort).
pub fn parse_tysh(data: &[u8]) -> Option<(String, Affine)> {
    let f = |at: usize| data.get(at..at + 8).map(|b| f64::from_be_bytes(b.try_into().unwrap_or([0; 8])));
    let mut m = [0.0; 6];
    for (i, v) in m.iter_mut().enumerate() {
        *v = f(2 + i * 8)?;
    }
    // text version (2) at 50, descriptor version (4) at 52, descriptor at 56.
    let rest = data.get(52..)?;
    let text = parse_prefix_versioned(rest).and_then(|d| match d.get("Txt ") {
        Some(Value::Text(t)) => Some(t.to_string_lossy()),
        _ => None,
    });
    Some((text.unwrap_or_default(), Affine { m }))
}

/// Parses a version-16 descriptor at the start of `data` (trailing bytes allowed).
pub(crate) fn parse_prefix_versioned(data: &[u8]) -> Option<Descriptor> {
    VersionedDescriptor::parse_prefix(data).ok().map(|(v, _)| v.descriptor)
}

/// Smart object identifier and transform from `SoLd`/`SoLE` (best effort). A `PlLd` block
/// stores no source size, so its transform needs the embedded file: see [`parse_plld`].
pub fn parse_smart(key: &[u8; 4], data: &[u8]) -> (String, Affine) {
    // SoLd: 'soLD' + version(4) + versioned descriptor.
    let desc = if key == b"SoLd" || key == b"SoLE" { data.get(8..).and_then(parse_prefix_versioned) } else { None };
    let Some(d) = desc else { return (String::new(), Affine::IDENTITY) };
    let id = match d.get("Idnt") {
        Some(Value::Text(t)) => t.to_string_lossy(),
        _ => String::new(),
    };
    let mut affine = Affine::IDENTITY;
    if let (Some(Value::List(pts)), Some(sz)) = (d.get("Trnf"), get_desc(&d, "Sz  ")) {
        let p: Vec<f64> = pts.iter().filter_map(|v| num(Some(v))).collect();
        let (w, h) = (num(sz.get("Wdth")).unwrap_or(0.0), num(sz.get("Hght")).unwrap_or(0.0));
        if let (Ok(p), true) = (<[f64; 8]>::try_from(p), w > 0.0 && h > 0.0) {
            let warp = parse_placed_warp(key, data);
            // A custom warp frames the box of its control points as stored: a lower-order patch's
            // box, not the box of the cubic grid it is raised to (whose inner points move in).
            let frame = match (&warp, get_desc(&d, "warp").and_then(stored_mesh_box)) {
                (Some(wp), Some(b)) if wp.style == photosuite_geom::warp::WarpStyle::Custom => b,
                _ => placed_frame(warp.as_ref(), w, h),
            };
            affine = corners_affine(&p, frame).unwrap_or(Affine::IDENTITY);
        }
    }
    (id, affine)
}

/// The bounding box of a `warp` descriptor's custom mesh control points as stored (any patch
/// order); `None` without a readable, non-degenerate mesh.
fn stored_mesh_box(w: &Descriptor) -> Option<[f64; 4]> {
    let Some(Value::ObjectArray(arr)) = get_desc(w, "customEnvelopeWarp")?.get("meshPoints") else { return None };
    let vals = |k: &str| match arr.body.get(k) {
        Some(Value::UnitFloats { values, .. }) => Some(values.clone()),
        _ => None,
    };
    let (xs, ys) = (vals("Hrzn")?, vals("Vrtc")?);
    let inf = f64::INFINITY;
    let b = xs.iter().zip(&ys).fold([inf, inf, -inf, -inf], |b, (x, y)| [b[0].min(*x), b[1].min(*y), b[2].max(*x), b[3].max(*y)]);
    (b.iter().all(|v| v.is_finite()) && b[2] - b[0] > 1e-9 && b[3] - b[1] > 1e-9).then_some(b)
}

/// The affine that takes the source-space box `frame` (left, top, right, bottom; see
/// [`placed_frame`]) onto placed corners `p` (top-left, top-right, bottom-right, bottom-left as
/// x, y pairs). `None` for an empty or non-finite frame, or non-finite corners.
pub fn corners_affine(p: &[f64; 8], frame: [f64; 4]) -> Option<Affine> {
    let [x0, y0, x1, y1] = frame;
    let (bw, bh) = (x1 - x0, y1 - y0);
    if !(frame.iter().chain(p).all(|v| v.is_finite()) && bw > 0.0 && bh > 0.0) {
        return None;
    }
    let (a, b, c, d) = ((p[2] - p[0]) / bw, (p[3] - p[1]) / bw, (p[6] - p[0]) / bh, (p[7] - p[1]) / bh);
    let m = [a, b, c, d, p[0] - a * x0 - c * y0, p[1] - b * x0 - d * y0];
    m.iter().all(|v| v.is_finite()).then_some(Affine { m })
}

/// The transform and (Distort/Perspective) projective placement of a `PlLd` record's corners `p`
/// for a source of `size` warped by `warp`: the inverse of [`crate::smart_map::plld_bytes`]'s
/// quad. As for `SoLd`'s `Trnf`, affine corners frame a single-patch custom warp's control-point
/// box (which may spill past the source), else the source rect; projective corners frame the
/// source rect, as [`parse_smart_perspective`] reads them. `PlLd` stores no source size: without
/// the source file (`None`) the layer is taken as placed at 100 %, the edge lengths being its
/// size; position and rotation stay right, only Replace Contents and re-renders depend on the
/// scale. Import and export (which keeps an unedited record verbatim) both place a `PlLd` layer
/// with this.
pub fn plld_transform(p: &[f64; 8], size: Option<(f64, f64)>, warp: Option<&photosuite_geom::warp::Warp>) -> (Affine, Option<[f64; 9]>) {
    let (w, h) = size.unwrap_or_else(|| ((p[2] - p[0]).hypot(p[3] - p[1]), (p[6] - p[0]).hypot(p[7] - p[1])));
    let perspective = corners_perspective([[p[0], p[1]], [p[2], p[3]], [p[4], p[5]], [p[6], p[7]]], (w, h));
    let frame = if perspective.is_some() { [0.0, 0.0, w, h] } else { placed_frame(warp, w, h) };
    (corners_affine(p, frame).unwrap_or(Affine::IDENTITY), perspective)
}

/// The source identifier and placed corners of a `PlLd` block, the placed-layer record that
/// Reference CS2 and older write with no `SoLd` next to it. Adobe PSD spec, "Placed Layer":
/// `plcL`, version, Pascal unique id (unpadded), page, total pages, anti-alias policy, layer
/// type, then 8 doubles: the x, y of each corner (top-left, top-right, bottom-right,
/// bottom-left). The warp that follows is read by [`parse_plld_warp`]. `None` for anything else
/// or a truncated record. Ported from storytold/photocraft@0002d861.
pub fn parse_plld(data: &[u8]) -> Option<(String, [f64; 8])> {
    plld_record(data).map(|(id, corners, _)| (id, corners))
}

/// [`parse_plld`] and the offset just past the corners, where the warp starts.
fn plld_record(data: &[u8]) -> Option<(String, [f64; 8], usize)> {
    if data.get(..4)? != b"plcL" {
        return None;
    }
    let n = usize::from(*data.get(8)?);
    let id = String::from_utf8_lossy(data.get(9..9 + n)?).into_owned();
    // Page number, total pages, anti-alias policy and layer type (4 × u32).
    let at = 9 + n + 16;
    let mut corners = [0.0; 8];
    for (i, c) in corners.iter_mut().enumerate() {
        let b = data.get(at + i * 8..at + i * 8 + 8)?;
        *c = f64::from_be_bytes(b.try_into().ok()?);
    }
    corners.iter().all(|v| v.is_finite()).then_some((id, corners, at + 64))
}

/// The warp of a placed layer's `PlLd` record (CS2 can warp a smart object).
#[derive(Debug, Clone, PartialEq)]
pub enum PlldWarp {
    /// No warp: `warpNone`, a warp that moves nothing, or a record that ends at its corners.
    None,
    /// The warp, in source pixels like the `SoLd` one.
    Warp(photosuite_geom::warp::Warp),
    /// A warp is stored but can't be read (or the record itself can't be): re-rendering the
    /// layer from its source would drop it.
    Unreadable,
}

/// The warp after a `PlLd` record's corners: a u32 warp version, then a versioned `warp`
/// descriptor (Adobe PSD spec, "Placed Layer"; the layout [`crate::smart_map::plld_bytes`]
/// writes).
pub fn parse_plld_warp(data: &[u8]) -> PlldWarp {
    let Some(rest) = plld_record(data).and_then(|(_, _, end)| data.get(end..)) else { return PlldWarp::Unreadable };
    // Only padding after the corners: no warp stored.
    if rest.iter().all(|b| *b == 0) {
        return PlldWarp::None;
    }
    match rest.get(4..).and_then(parse_prefix_versioned).as_ref().and_then(warp_of) {
        Some(Some(w)) => PlldWarp::Warp(w),
        Some(None) => PlldWarp::None,
        None => PlldWarp::Unreadable,
    }
}

/// The projective placement of a `SoLd` / `SoLE` block whose corners aren't a parallelogram
/// (Distort, Perspective): source pixels → document pixels, row-major 3×3. Read from
/// `nonAffineTransform` (the placed corners), else `Trnf`. `None` for affine placements, which
/// [`parse_smart`]'s affine holds exactly.
pub fn parse_smart_perspective(key: &[u8; 4], data: &[u8]) -> Option<[f64; 9]> {
    if key != b"SoLd" && key != b"SoLE" {
        return None;
    }
    let d = data.get(8..).and_then(parse_prefix_versioned)?;
    let sz = get_desc(&d, "Sz  ")?;
    let (w, h) = (num(sz.get("Wdth"))?, num(sz.get("Hght"))?);
    let quad = |key: &str| -> Option<[[f64; 2]; 4]> {
        let Some(Value::List(pts)) = d.get(key) else { return None };
        let p: Vec<f64> = pts.iter().filter_map(|v| num(Some(v))).collect();
        (p.len() == 8 && p.iter().all(|v| v.is_finite())).then(|| [[p[0], p[1]], [p[2], p[3]], [p[4], p[5]], [p[6], p[7]]])
    };
    let q = quad("nonAffineTransform").or_else(|| quad("Trnf"))?;
    corners_perspective(q, (w, h))
}

/// The projective placement of a `w` × `h` source onto corners `q` that aren't a parallelogram;
/// `None` for affine corners (see [`corners_affine`]) or an empty size.
pub fn corners_perspective(q: [[f64; 2]; 4], (w, h): (f64, f64)) -> Option<[f64; 9]> {
    // A parallelogram (top-left + bottom-right = top-right + bottom-left) is affine.
    let tol = 1e-6 * (1.0 + q.iter().flatten().fold(0.0f64, |m, v| m.max(v.abs())));
    if (0..2).all(|i| (q[0][i] + q[2][i] - q[1][i] - q[3][i]).abs() <= tol) || w <= 0.0 || h <= 0.0 {
        return None;
    }
    photosuite_algo::transform::Homography::rect_to_quad([0.0, 0.0, w, h], q).map(|m| m.0)
}

/// `masterFXSwitch` from an `lfx2` block (defaults to `true`).
pub fn effects_enabled(lfx2: &[u8]) -> bool {
    lfx2.get(4..)
        .and_then(parse_prefix_versioned)
        .and_then(|d| match d.get("masterFXSwitch") {
            Some(Value::Boolean(b)) => Some(*b),
            _ => None,
        })
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    /// Reference 27.11 stores a gradient's method as the enum `gradientsInterpolationMethod`: a
    /// black→white Linear gradient is at 188 of 255 halfway (measured upstream), where Classic
    /// gives 128 (storytold/photocraft@9237a8bd).
    #[test]
    fn gradient_fill_reads_the_interpolation_method() {
        use photosuite_color::Color;
        use photosuite_doc::{Fill, GradientStyle};
        use photosuite_psd::descriptor::{Id, Value};
        let f = Fill::gradient(vec![(0.0, Color::BLACK), (1.0, Color::WHITE)], 0.0, 1.0, GradientStyle::Linear, false);
        let (k, data) = super::write_fill(&f);
        let d = super::parse_prefix_versioned(&data).unwrap();
        let mid = |code: &str| {
            let d = d.clone().with("gradientsInterpolationMethod", Value::Enumerated { type_id: Id::new("gradientInterpolationMethodType"), value: Id::new(code) });
            let back = super::fill_from_desc(&k, &d).unwrap();
            let ramp = photosuite_compose::gradient_fill::Ramp::new(&back).unwrap();
            (ramp.sample(0.5)[0] * 255.0).round()
        };
        assert_eq!(mid("Gcls"), 128.0);
        assert!((mid("Lnr ") - 188.0).abs() <= 1.0, "Linear: {}", mid("Lnr "));
        assert!((mid("Perc") - 99.0).abs() <= 2.0, "Perceptual: {}", mid("Perc"));
        assert!((mid("Smoo") - 99.0).abs() <= 2.0, "Smooth: {}", mid("Smoo"));
    }

    /// psd-tools layers-minimal/gradient-fill.psd ("Color to Transparent", no stored pixels): a
    /// gradient fill's opacity stops reach the model (as its opacity stops; written from stop
    /// alpha) and survive a write; the ramp's alpha follows them.
    #[test]
    fn gradient_fill_opacity_stops_become_alpha() {
        use photosuite_color::Color;
        use photosuite_doc::{Fill, GradientStyle};
        let red = Color::rgba(1.0, 0.0, 0.0, 1.0);
        let f = Fill::gradient(vec![(0.0, red), (1.0, Color { alpha: 0.0, ..red })], 90.0, 1.0, GradientStyle::Linear, false);
        let (k, data) = super::write_fill(&f);
        let back = super::parse_fill(&k, &data).unwrap();
        let Fill::Gradient { stops, opacity_stops, .. } = &back else { panic!("gradient") };
        assert_eq!(opacity_stops, &vec![(0.0, 1.0), (1.0, 0.0)]);
        assert!(stops.iter().all(|s| s.1.c[0] == 1.0 && s.1.c[1] == 0.0));
        let ramp = photosuite_compose::gradient_fill::Ramp::new(&back).unwrap();
        assert_eq!(ramp.sample(0.0)[3], 1.0);
        assert!(ramp.sample(1.0)[3] < 0.01);
        // Opaque gradients come back without opacity stops.
        let (k, data) = super::write_fill(&Fill::gradient(vec![(0.0, red), (1.0, Color::WHITE)], 0.0, 1.0, GradientStyle::Linear, false));
        let Some(Fill::Gradient { opacity_stops, .. }) = super::parse_fill(&k, &data) else { panic!("gradient") };
        assert!(opacity_stops.is_empty());
    }

    #[test]
    fn lab_descriptor_colours_round_trip() {
        use photosuite_color::ColorMode;
        use photosuite_psd::descriptor::{Descriptor, Value};
        let d = Descriptor::new("LbCl").with("Lmnc", Value::Double(19.07)).with("A   ", Value::Double(52.29)).with("B   ", Value::Double(-85.08));
        let c = super::color_from_desc(&d).unwrap();
        assert_eq!(c.mode, ColorMode::Lab);
        assert!((c.c[0] - 0.1907).abs() < 1e-5 && (c.c[1] * 255.0 - 128.0 - 52.29).abs() < 1e-3 && (c.c[2] * 255.0 - 128.0 + 85.08).abs() < 1e-3);
        let back = super::color_from_desc(&super::color_to_desc(&c)).unwrap();
        assert_eq!(back.mode, ColorMode::Lab);
        for i in 0..3 {
            assert!((back.c[i] - c.c[i]).abs() < 1e-6);
        }
        // Missing components are rejected, not defaulted.
        assert!(super::color_from_desc(&Descriptor::new("LbCl").with("Lmnc", Value::Double(50.0))).is_none());
    }

    #[test]
    fn blending_ranges_map_to_blend_if() {
        use photosuite_doc::{BlendIf, BlendRange};
        use photosuite_psd::layer::BlendingRanges;
        // Full ranges (what every layer without Blend If carries) → the default setting.
        assert_eq!(super::blend_if_from_ranges(&BlendingRanges::full(3)), BlendIf::default());
        assert_eq!(super::blend_if_from_ranges(&BlendingRanges::default()), BlendIf::default());
        // Gray: This Layer black split 10/40; Blue (entry 3): Underlying white at 200.
        let mut data = BlendingRanges::full(3).data;
        data[..4].copy_from_slice(&[10, 40, 255, 255]);
        data[3 * 8 + 4..3 * 8 + 8].copy_from_slice(&[0, 0, 200, 200]);
        let b = super::blend_if_from_ranges(&BlendingRanges { data: data.clone() });
        assert_eq!(b.get(0), [BlendRange { black: [10, 40], white: [255, 255] }, BlendRange::FULL]);
        assert_eq!(b.get(3), [BlendRange::FULL, BlendRange { black: [0, 0], white: [200, 200] }]);
        assert_eq!(b.ranges.len(), 4);
        // Written back padded to the five entries Photoshop writes in every mode.
        let mut five = data.clone();
        five.extend_from_slice(&[0, 0, 255, 255, 0, 0, 255, 255]);
        assert_eq!(super::ranges_from_blend_if(&b, 3).data, five);
        // The default writes full ranges for gray + four channels, Grayscale included.
        assert_eq!(super::ranges_from_blend_if(&BlendIf::default(), 4), BlendingRanges::full(4));
        assert_eq!(super::ranges_from_blend_if(&BlendIf::default(), 1), BlendingRanges::full(4));
        // A trimmed setting is padded the same way.
        let mut short = BlendIf::default();
        short.set(0, [BlendRange { black: [5, 5], white: [255, 255] }, BlendRange::FULL]);
        let r = super::ranges_from_blend_if(&short, 3);
        assert_eq!(r.data.len(), 5 * 8);
        assert_eq!(&r.data[..8], &[5, 5, 255, 255, 0, 0, 255, 255]);
        assert_eq!(&r.data[8..], &BlendingRanges::full(3).data[..]);
    }

    #[test]
    fn brst_round_trip() {
        assert_eq!(super::parse_brst(&[0, 0, 0, 2]), 0b100);
        assert_eq!(super::parse_brst(&[0, 0, 0, 0, 0, 0, 0, 1]), 0b11);
        assert_eq!(super::brst_data(0b101).unwrap(), vec![0, 0, 0, 0, 0, 0, 0, 2]);
        assert_eq!(super::brst_data(0), None);
    }

    use super::*;

    #[test]
    fn placed_layer_warp_is_read() {
        use photosuite_geom::warp::WarpStyle;
        use photosuite_psd::descriptor::ObjectArray;
        let e = |t: &str, v: &str| Value::Enumerated { type_id: Id::new(t), value: Id::new(v) };
        let bounds = Descriptor::new("Rctn")
            .with("Top ", Value::UnitFloat { unit: *b"#Pxl", value: 0.0 })
            .with("Left", Value::UnitFloat { unit: *b"#Pxl", value: 0.0 })
            .with("Btom", Value::UnitFloat { unit: *b"#Pxl", value: 30.0 })
            .with("Rght", Value::UnitFloat { unit: *b"#Pxl", value: 60.0 });
        let sold = |warp: Descriptor| {
            let mut v = b"soLD".to_vec();
            v.extend_from_slice(&4u32.to_be_bytes());
            v.extend(VersionedDescriptor::new(Descriptor::new("null").with("warp", Value::Descriptor(warp))).to_bytes());
            v
        };
        let arc = Descriptor::new("warp")
            .with("warpStyle", e("warpStyle", "warpArc"))
            .with("warpValue", Value::Double(40.0))
            .with("warpPerspective", Value::Double(0.0))
            .with("warpPerspectiveOther", Value::Double(0.0))
            .with("warpRotate", e("Ornt", "Hrzn"))
            .with("bounds", Value::Descriptor(bounds.clone()));
        let w = parse_placed_warp(b"SoLd", &sold(arc)).unwrap();
        assert_eq!((w.style, w.bend, w.bounds), (WarpStyle::Arc, 40.0, [0.0, 0.0, 60.0, 30.0]));
        let (mut xs, mut ys) = (Vec::new(), Vec::new());
        for j in 0..4 {
            for i in 0..4 {
                xs.push(f64::from(i) * 20.0 + if i == 3 && j == 3 { 7.0 } else { 0.0 });
                ys.push(f64::from(j) * 10.0);
            }
        }
        let mesh = Descriptor::new("null")
            .with("Hrzn", Value::UnitFloats { unit: *b"#Pxl", values: xs })
            .with("Vrtc", Value::UnitFloats { unit: *b"#Pxl", values: ys });
        let custom = Descriptor::new("warp").with("warpStyle", e("warpStyle", "warpCustom")).with("bounds", Value::Descriptor(bounds)).with(
            "customEnvelopeWarp",
            Value::Descriptor(Descriptor::new("customEnvelopeWarp").with("meshPoints", Value::ObjectArray(ObjectArray { prefix: 16, body: mesh }))),
        );
        let w = parse_placed_warp(b"SoLd", &sold(custom)).unwrap();
        assert_eq!(w.style, WarpStyle::Custom);
        assert_eq!(w.mesh.as_ref().unwrap().points[15], [67.0, 30.0]);
        assert!(parse_placed_warp(b"PlLd", &[0; 20]).is_none());
    }

    /// A custom warp stored as a lower-order patch (Reference converts most presets so: Inflate
    /// to 3 × 3 control points) is read, raised exactly to 4 × 4, and the placed quad frames the
    /// box of the points as stored, so the transform stays the identity here.
    #[test]
    fn lower_order_warp_patches_are_read() {
        use photosuite_psd::descriptor::ObjectArray;
        let e = |t: &str, v: &str| Value::Enumerated { type_id: Id::new(t), value: Id::new(v) };
        let px = |v: f64| Value::UnitFloat { unit: *b"#Pxl", value: v };
        let bounds = Descriptor::new("Rctn").with("Top ", px(0.0)).with("Left", px(0.0)).with("Btom", px(30.0)).with("Rght", px(60.0));
        let pts: [[f64; 2]; 9] = [[0.0, 0.0], [30.0, -10.0], [60.0, 0.0], [-10.0, 15.0], [30.0, 15.0], [70.0, 15.0], [0.0, 30.0], [30.0, 40.0], [60.0, 30.0]];
        let mesh = Descriptor::new("null")
            .with("Hrzn", Value::UnitFloats { unit: *b"#Pxl", values: pts.iter().map(|p| p[0]).collect() })
            .with("Vrtc", Value::UnitFloats { unit: *b"#Pxl", values: pts.iter().map(|p| p[1]).collect() });
        let warp = Descriptor::new("warp")
            .with("warpStyle", e("warpStyle", "warpCustom"))
            .with("bounds", Value::Descriptor(bounds))
            .with("uOrder", Value::Integer(3))
            .with("vOrder", Value::Integer(3))
            .with("customEnvelopeWarp", Value::Descriptor(Descriptor::new("customEnvelopeWarp").with("meshPoints", Value::ObjectArray(ObjectArray { prefix: 9, body: mesh }))));
        let quad = [-10.0, -10.0, 70.0, -10.0, 70.0, 40.0, -10.0, 40.0];
        let d = Descriptor::new("null")
            .with("Idnt", Value::Text(UnicodeString::new_nul("id")))
            .with("Trnf", Value::List(quad.iter().map(|v| Value::Double(*v)).collect()))
            .with("warp", Value::Descriptor(warp))
            .with("Sz  ", Value::Descriptor(Descriptor::new("Pnt ").with("Wdth", Value::Double(60.0)).with("Hght", Value::Double(30.0))));
        let mut data = b"soLD".to_vec();
        data.extend_from_slice(&4u32.to_be_bytes());
        data.extend(VersionedDescriptor::new(d).to_bytes());
        let w = parse_placed_warp(b"SoLd", &data).expect("a 3 × 3 patch reads");
        // The biquadratic patch, evaluated directly.
        let quadratic = |s: f64, t: f64| {
            let b = |u: f64| [(1.0 - u) * (1.0 - u), 2.0 * u * (1.0 - u), u * u];
            let (bs, bt) = (b(s), b(t));
            let mut p = [0.0; 2];
            for j in 0..3 {
                for i in 0..3 {
                    p[0] += bs[i] * bt[j] * pts[j * 3 + i][0];
                    p[1] += bs[i] * bt[j] * pts[j * 3 + i][1];
                }
            }
            p
        };
        for (s, t) in [(0.0, 0.0), (0.5, 0.5), (0.2, 0.9), (1.0, 0.3)] {
            let (got, want) = (w.map(s * 60.0, t * 30.0), quadratic(s, t));
            assert!((got.0 - want[0]).abs() < 1e-9 && (got.1 - want[1]).abs() < 1e-9, "{s},{t}: {got:?} vs {want:?}");
        }
        assert_eq!(parse_smart(b"SoLd", &data).1, Affine::IDENTITY);
    }

    #[test]
    fn locks_and_labels() {
        // Reference values round-trip byte for byte; Background layers carry 0x0D
        // (transparency + position + the bit-3 lock reference files write).
        for bits in [0u32, 1, 2, 4, 8, 0x0D, 0x8000_0000, 0x8000_000F] {
            assert_eq!(lspf_from_locks(&locks_from_lspf(bits)), bits);
        }
        // Older PhotoSuite files wrote the artboard lock at 0x10; it reads back as artboard
        // and is rewritten at the reference value 0x08.
        assert!(locks_from_lspf(0x10).artboard);
        assert_eq!(lspf_from_locks(&locks_from_lspf(0x10)), 0x08);
        for i in 0..8 {
            assert_eq!(label_index(label_from_index(i)), i);
        }
    }

    #[test]
    fn fills_roundtrip() {
        for f in [
            Fill::Solid(Color::rgb(1.0, 0.5, 0.0)),
            Fill::Solid(Color { mode: ColorMode::Cmyk, c: [0.1, 0.2, 0.3, 0.4], alpha: 1.0 }),
            Fill::Solid(Color::gray(0.25)),
            Fill::gradient(vec![(0.0, Color::rgb(1.0, 0.0, 0.0)), (1.0, Color::rgb(0.0, 0.0, 1.0))], 45.0, 1.0, GradientStyle::Reflected, true),
            // A live gradient (Gradient tool): canvas-aligned, offset, midpoints, opacity, dither.
            Fill::Gradient {
                stops: vec![(0.0, Color::rgb(1.0, 0.0, 0.0)), (0.5, Color::rgb(0.0, 1.0, 0.0)), (1.0, Color::rgb(0.0, 0.0, 1.0))],
                angle: -33.5,
                scale: 2.25,
                style: GradientStyle::Radial,
                reverse: false,
                opacity_stops: vec![(0.0, 1.0), (0.25, 0.5), (1.0, 0.0)],
                midpoints: vec![0.25, 0.75],
                offset: (0.125, -0.375),
                dither: true,
                align: false,
            },
            Fill::Pattern { name: "Bubbles".into(), scale: 0.5, id: "abc".into(), angle: 30.0, link: false, phase: (3.0, -2.0) },
        ] {
            let (k, d) = write_fill(&f);
            let back = parse_fill(&k, &d).unwrap();
            match (&f, &back) {
                (Fill::Solid(a), Fill::Solid(b)) => {
                    assert_eq!(a.mode, b.mode);
                    for i in 0..4 {
                        assert!((a.c[i] - b.c[i]).abs() < 1e-5);
                    }
                }
                _ => assert_eq!(back, f),
            }
        }
    }
}

#[cfg(test)]
mod more_tests {
    use super::*;

    #[test]
    fn float_rgb_colors_parse() {
        let d = Descriptor::new("RGBC").with("redFloat", Value::Double(1.0)).with("greenFloat", Value::Double(0.5)).with("blueFloat", Value::Double(0.25));
        let c = color_from_desc(&d).unwrap();
        assert_eq!(c.to_rgb(), [1.0, 0.5, 0.25]);
    }

    #[test]
    fn smart_transform_from_sold() {
        let pts = [10.0, 20.0, 110.0, 20.0, 110.0, 70.0, 10.0, 70.0].map(Value::Double).to_vec();
        let d = Descriptor::new("null")
            .with("Idnt", Value::Text(UnicodeString::new_nul("uuid-1")))
            .with("Trnf", Value::List(pts))
            .with("Sz  ", Value::Descriptor(Descriptor::new("Pnt ").with("Wdth", Value::Double(50.0)).with("Hght", Value::Double(25.0))));
        let mut data = b"soLD".to_vec();
        data.extend_from_slice(&4u32.to_be_bytes());
        data.extend(VersionedDescriptor::new(d).to_bytes());
        data.extend_from_slice(&[0, 0]);
        let (id, a) = parse_smart(b"SoLd", &data);
        assert_eq!(id, "uuid-1");
        assert_eq!(a.m, [2.0, 0.0, 0.0, 2.0, 10.0, 20.0]);
        assert_eq!(parse_smart(b"PlLd", &data).0, "");
        assert_eq!(parse_plld(&data), None, "soLD data is not a plcL record");
    }

    /// A `PlLd` record (as CS2 writes it, with no `SoLd`) gives its source id and
    /// placed corners; with the source size those are the same transform `SoLd` gives.
    #[test]
    fn plld_id_and_corners() {
        use crate::smart_map::{PlacedSpec, plld_bytes};
        let t = Affine { m: [1.5, 0.25, -0.5, 2.0, 10.0, 20.0] };
        let spec = PlacedSpec { idnt: "uuid-cs2", placed: "", transform: t, perspective: None, size: (40.0, 30.0), dpi: 72.0, warp: None, filter_fx: None };
        let data = plld_bytes(&spec, &[], &mut Vec::new());
        let (id, p) = parse_plld(&data).expect("a plcL record");
        assert_eq!(id, "uuid-cs2");
        assert_eq!(p, [10.0, 20.0, 70.0, 30.0, 55.0, 90.0, -5.0, 80.0]);
        let back = corners_affine(&p, [0.0, 0.0, 40.0, 30.0]).unwrap();
        for (a, b) in back.m.iter().zip(t.m) {
            assert!((a - b).abs() < 1e-12, "{back:?} vs {t:?}");
        }
        assert_eq!(corners_perspective([[p[0], p[1]], [p[2], p[3]], [p[4], p[5]], [p[6], p[7]]], (40.0, 30.0)), None, "affine corners");
        assert_eq!(corners_affine(&p, [0.0, 0.0, 0.0, 30.0]), None);
        assert_eq!(corners_affine(&p, [0.0, 0.0, f64::NAN, 30.0]), None);
        assert_eq!(corners_affine(&p, [5.0, 0.0, 5.0, 30.0]), None, "an empty frame");
        let mut nan = p;
        nan[3] = f64::INFINITY;
        assert_eq!(corners_affine(&nan, [0.0, 0.0, 40.0, 30.0]), None);
        // The placed-layer record is the one block that can be the principal one: PlLd-only
        // layers have no smart transform from `parse_smart`.
        assert_eq!(parse_smart(b"PlLd", &data), (String::new(), Affine::IDENTITY));
    }

    /// A `PlLd` record's corners frame the same box as `SoLd`'s `Trnf`: with a custom
    /// warp whose control points spill past the source, the control-point box, not the source
    /// rect. The CS2 record then places the layer exactly as the equivalent `SoLd` does, with or
    /// without the source size (the box needs none).
    #[test]
    fn plld_corners_frame_a_spilled_custom_warp_like_sold() {
        use crate::smart_map::{PlacedSpec, parse_sold, plld_bytes, sold_bytes};
        use photosuite_geom::warp::{BezierMesh, Warp};
        let bounds = [0.0, 0.0, 50.0, 25.0];
        let mut mesh = BezierMesh::identity(bounds, 1, 1);
        mesh.points[0] = [-10.0, -5.0];
        mesh.points[15] = [60.0, 25.0];
        let warp = Warp::custom(mesh, bounds);
        let t = Affine { m: [2.0, 0.5, -0.25, 2.0, 120.0, 210.0] };
        let spec = PlacedSpec { idnt: "cs2", placed: "p", transform: t, perspective: None, size: (50.0, 25.0), dpi: 72.0, warp: Some(&warp), filter_fx: None };
        let plld = plld_bytes(&spec, &[], &mut Vec::new());
        let sold = sold_bytes(None, &spec, &mut Vec::new());
        let (_, p) = parse_plld(&plld).expect("a plcL record");
        assert_eq!(parse_plld_warp(&plld), PlldWarp::Warp(warp.clone()));
        let from_sold = parse_smart(b"SoLd", &sold).1;
        assert_eq!(parse_sold(&sold).map(|s| s.transform), Some(from_sold));
        let close = |a: Affine, b: Affine| a.m.iter().zip(b.m).all(|(x, y)| (x - y).abs() < 1e-9);
        assert!(close(from_sold, t), "{from_sold:?} vs {t:?}");
        for size in [Some((50.0, 25.0)), None] {
            let (got, perspective) = plld_transform(&p, size, Some(&warp));
            assert!(close(got, from_sold), "size {size:?}: {got:?} vs SoLd {from_sold:?}");
            assert_eq!(perspective, None);
        }
        // Framing the source rect instead would stretch it (as it once did for SoLd).
        let rect = corners_affine(&p, [0.0, 0.0, 50.0, 25.0]).unwrap();
        assert!(!close(rect, t), "the test needs a mesh that spills: {rect:?}");
        // With no warp, or a preset one, the frame is the source rect, as before.
        let plain = PlacedSpec { warp: None, ..spec };
        let (_, q) = parse_plld(&plld_bytes(&plain, &[], &mut Vec::new())).unwrap();
        assert!(close(plld_transform(&q, Some((50.0, 25.0)), None).0, t));
    }

    /// Malformed `PlLd` data (wrong magic, truncated anywhere, a Pascal length past the end,
    /// non-finite corners) reads as `None`, never a panic.
    #[test]
    fn plld_malformed_is_none() {
        use crate::smart_map::{PlacedSpec, plld_bytes};
        let spec =
            PlacedSpec { idnt: "id", placed: "", transform: Affine::IDENTITY, perspective: None, size: (4.0, 4.0), dpi: 72.0, warp: None, filter_fx: None };
        let data = plld_bytes(&spec, &[], &mut Vec::new());
        // Magic + version + Pascal "id" + 4 × u32 + 8 × f64: everything after is the warp.
        let end = 4 + 4 + 1 + 2 + 16 + 64;
        assert!(parse_plld(&data[..end]).is_some());
        for n in 0..end {
            assert_eq!(parse_plld(&data[..n]), None, "truncated to {n} bytes");
        }
        let mut bad = data.clone();
        bad[..4].copy_from_slice(b"soLD");
        assert_eq!(parse_plld(&bad), None);
        let mut long = data.clone();
        long[8] = 255; // a Pascal id longer than the record
        assert_eq!(parse_plld(&long[..200.min(long.len())]), None);
        let mut nan = data.clone();
        nan[end - 8..end].copy_from_slice(&f64::NAN.to_be_bytes());
        assert_eq!(parse_plld(&nan), None);
        assert_eq!(parse_plld(&[]), None);
    }

    /// CS2 can warp a smart object; `PlLd` keeps that warp after the corners (a u32
    /// warp version, then the versioned `warp` descriptor). It reads back as written, and
    /// `warpNone`, an identity warp or a record that stops at its corners mean no warp.
    #[test]
    fn plld_warps_round_trip() {
        use crate::smart_map::{PlacedSpec, plld_bytes};
        use photosuite_geom::warp::{BezierMesh, Warp, WarpStyle};
        let bounds = [0.0, 0.0, 60.0, 30.0];
        let mut arc = Warp::preset(WarpStyle::Arc, 40.0, bounds);
        arc.vertical = true;
        arc.h_distort = 10.0;
        let mut mesh = BezierMesh::identity(bounds, 1, 1);
        mesh.points[15] = [67.0, 30.0];
        let custom = Warp::custom(mesh, bounds);
        let flat = Warp::preset(WarpStyle::Flag, 0.0, bounds);
        for (w, back) in
            [(Some(&arc), PlldWarp::Warp(arc.clone())), (Some(&custom), PlldWarp::Warp(custom.clone())), (Some(&flat), PlldWarp::None), (None, PlldWarp::None)]
        {
            let t = Affine { m: [0.5, 0.0, 0.0, 0.5, 3.0, 4.0] };
            let spec = PlacedSpec { idnt: "w", placed: "", transform: t, perspective: None, size: (60.0, 30.0), dpi: 72.0, warp: w, filter_fx: None };
            let data = plld_bytes(&spec, &[], &mut Vec::new());
            assert_eq!(parse_plld_warp(&data), back, "{w:?}");
            // The corners are unchanged by the warp that follows them.
            assert_eq!(parse_plld(&data).map(|(_, p)| p[..2].to_vec()), Some(vec![3.0, 4.0]));
        }
        let spec =
            PlacedSpec { idnt: "id", placed: "", transform: Affine::IDENTITY, perspective: None, size: (4.0, 4.0), dpi: 72.0, warp: None, filter_fx: None };
        let end = 4 + 4 + 1 + 2 + 16 + 64;
        let data = plld_bytes(&spec, &[], &mut Vec::new());
        assert_eq!(parse_plld_warp(&data[..end]), PlldWarp::None, "no warp stored");
        assert_eq!(parse_plld_warp(&[data[..end].to_vec(), vec![0; 3]].concat()), PlldWarp::None, "padding");
    }

    /// A `PlLd` warp that is stored but can't be read (truncated, an unknown style, a missing
    /// bounds rectangle or mesh) is `Unreadable`, so the import keeps the warped pixels.
    #[test]
    fn plld_unreadable_warps() {
        use crate::smart_map::{PlacedSpec, plld_bytes};
        use photosuite_geom::warp::{Warp, WarpStyle};
        let arc = Warp::preset(WarpStyle::Arc, 40.0, [0.0, 0.0, 4.0, 4.0]);
        let spec = PlacedSpec {
            idnt: "id",
            placed: "",
            transform: Affine::IDENTITY,
            perspective: None,
            size: (4.0, 4.0),
            dpi: 72.0,
            warp: Some(&arc),
            filter_fx: None,
        };
        let data = plld_bytes(&spec, &[], &mut Vec::new());
        let end = 4 + 4 + 1 + 2 + 16 + 64;
        assert!(matches!(parse_plld_warp(&data), PlldWarp::Warp(_)));
        for n in end + 1..data.len() - 4 {
            if data[end..n].iter().any(|b| *b != 0) {
                assert_eq!(parse_plld_warp(&data[..n]), PlldWarp::Unreadable, "truncated to {n} bytes");
            }
        }
        let with = |d: Descriptor| [data[..end].to_vec(), vec![0; 4], VersionedDescriptor::new(d).to_bytes()].concat();
        let style = |s: &str| Value::Enumerated { type_id: Id::new("warpStyle"), value: Id::new(s) };
        assert_eq!(parse_plld_warp(&with(Descriptor::new("warp").with("warpStyle", style("warpSomethingNew")))), PlldWarp::Unreadable);
        assert_eq!(parse_plld_warp(&with(Descriptor::new("warp").with("warpStyle", style("warpArc")))), PlldWarp::Unreadable, "no bounds");
        assert_eq!(parse_plld_warp(&with(Descriptor::new("warp").with("warpStyle", style("warpNone")))), PlldWarp::None);
        let bounds = Descriptor::new("classFloatRect")
            .with("Top ", Value::Double(0.0))
            .with("Left", Value::Double(0.0))
            .with("Btom", Value::Double(4.0))
            .with("Rght", Value::Double(4.0));
        let custom = Descriptor::new("warp").with("warpStyle", style("warpCustom")).with("bounds", Value::Descriptor(bounds));
        assert_eq!(parse_plld_warp(&with(custom)), PlldWarp::Unreadable, "custom with no mesh");
        assert_eq!(parse_plld_warp(&[data[..end].to_vec(), vec![0, 0, 0, 0, 0, 0, 0, 16, 1, 2]].concat()), PlldWarp::Unreadable);
        assert_eq!(parse_plld_warp(&data[..end - 1]), PlldWarp::Unreadable, "the record itself");
        assert_eq!(parse_plld_warp(&[]), PlldWarp::Unreadable);
    }

    #[test]
    fn effects_switch() {
        let mut lfx = vec![0, 0, 0, 0];
        lfx.extend(VersionedDescriptor::new(Descriptor::new("null").with("masterFXSwitch", Value::Boolean(false))).to_bytes());
        assert!(!effects_enabled(&lfx));
        assert!(effects_enabled(&[0, 0, 0, 0]));
    }

    #[test]
    fn tysh_garbage_is_none() {
        assert!(parse_tysh(&[0; 10]).is_none());
    }

    #[test]
    fn hsb_colours_decode() {
        let hsb = |h: f64, s: f64, b: f64| {
            Descriptor::new("HSBC")
                .with("H   ", Value::UnitFloat { unit: *b"#Ang", value: h })
                .with("Strt", Value::Double(s))
                .with("Brgh", Value::Double(b))
        };
        let rgb = |d: &Descriptor| color_from_desc(d).unwrap().to_rgb();
        assert_eq!(rgb(&hsb(0.0, 0.0, 100.0)), [1.0, 1.0, 1.0]);
        assert_eq!(rgb(&hsb(0.0, 100.0, 100.0)), [1.0, 0.0, 0.0]);
        assert_eq!(rgb(&hsb(120.0, 100.0, 50.0)), [0.0, 0.5, 0.0]);
        let c = rgb(&hsb(210.0, 50.0, 80.0));
        assert!((c[0] - 0.4).abs() < 1e-5 && (c[1] - 0.6).abs() < 1e-5 && (c[2] - 0.8).abs() < 1e-5, "{c:?}");
        // Hue wraps; missing keys are not a colour.
        assert_eq!(rgb(&hsb(360.0, 100.0, 100.0)), [1.0, 0.0, 0.0]);
        assert!(color_from_desc(&Descriptor::new("HSBC").with("H   ", Value::Double(1.0))).is_none());
    }

    /// A CMYK gradient fill whose angle is edited keeps its gradient definition verbatim (its
    /// stops used to be rewritten as RGB, and a noise gradient as black to white).
    #[test]
    fn unedited_gradient_definition_is_kept() {
        let cmyk = |c: f64, k: f64| {
            Value::Descriptor(
                Descriptor::new("CMYC")
                    .with("Cyn ", Value::Double(c))
                    .with("Mgnt", Value::Double(0.0))
                    .with("Ylw ", Value::Double(0.0))
                    .with("Blck", Value::Double(k)),
            )
        };
        let stop = |l: f64, c: Value| {
            Value::Descriptor(
                Descriptor::new("Clrt")
                    .with("Clr ", c)
                    .with("Type", Value::Enumerated { type_id: Id::new("Clry"), value: Id::new("UsrS") })
                    .with("Lctn", Value::Integer(l as i32))
                    .with("Mdpn", Value::Integer(50)),
            )
        };
        let grad = Descriptor::new("Grdn")
            .with("Nm  ", Value::Text(UnicodeString::new_nul("Cyan to black")))
            .with("GrdF", Value::Enumerated { type_id: Id::new("GrdF"), value: Id::new("CstS") })
            .with("Intr", Value::Double(4096.0))
            .with("Clrs", Value::List(vec![stop(0.0, cmyk(100.0, 0.0)), stop(4096.0, cmyk(0.0, 100.0))]));
        let base = Descriptor::new("null")
            .with("Angl", Value::UnitFloat { unit: *b"#Ang", value: 90.0 })
            .with("Type", Value::Enumerated { type_id: Id::new("GrdT"), value: Id::new("Lnr ") })
            .with("Grad", Value::Descriptor(grad.clone()));
        let data = VersionedDescriptor::new(base).to_bytes();
        let Some(Fill::Gradient { mut angle, stops, opacity_stops, midpoints, style, scale, reverse, offset, dither, align }) = parse_fill(b"GdFl", &data) else { panic!() };
        angle += 30.0;
        let edited = Fill::Gradient { angle, stops, opacity_stops, midpoints, style, scale, reverse, offset, dither, align };
        let (_, d) = fill_to_desc_onto(&edited, Some((b"GdFl", &data)));
        assert_eq!(get_desc(&d, "Grad"), Some(&grad));
        assert!(matches!(d.get("Angl"), Some(Value::UnitFloat { value, .. }) if (*value - 120.0).abs() < 1e-3));
        // Edited stops are written from the model.
        let Fill::Gradient { mut stops, angle, opacity_stops, midpoints, style, scale, reverse, offset, dither, align } = edited else { panic!() };
        stops[0].1 = Color::rgb(1.0, 0.0, 0.0);
        let changed = Fill::Gradient { angle, stops, opacity_stops, midpoints, style, scale, reverse, offset, dither, align };
        let (_, d) = fill_to_desc_onto(&changed, Some((b"GdFl", &data)));
        assert_ne!(get_desc(&d, "Grad"), Some(&grad));
    }

    /// A pattern fill rewritten onto a file that left its scale, link and phase out (at their
    /// defaults) leaves them out too; a changed one is written.
    #[test]
    fn pattern_defaults_stay_out_of_files_without_them() {
        let base = Descriptor::new("null").with("Ptrn", Value::Descriptor(pattern_ref_desc("P", "id-1")));
        let data = VersionedDescriptor::new(base).to_bytes();
        let fill = |scale: f32, link: bool| Fill::Pattern { name: "Q".into(), id: "id-2".into(), scale, angle: 0.0, link, phase: (0.0, 0.0) };
        let (_, d) = fill_to_desc_onto(&fill(1.0, true), Some((b"PtFl", &data)));
        assert!(d.get("Scl ").is_none() && d.get("Algn").is_none() && d.get("phase").is_none(), "{d:?}");
        let (_, d) = fill_to_desc_onto(&fill(0.5, false), Some((b"PtFl", &data)));
        assert!(d.get("Scl ").is_some() && matches!(d.get("Algn"), Some(Value::Boolean(false))));
    }

    /// Options set back to their defaults are written as defaults onto a file that had them set
    /// (the merge used to keep the old values: a gradient stayed reversed, a pattern rotated),
    /// and a 100 % scale adds no `Scl` to a file without one (as Photoshop writes it).
    #[test]
    fn fill_options_back_to_defaults_are_written() {
        let set = Fill::Gradient {
            stops: vec![(0.0, Color::rgb(1.0, 0.0, 0.0)), (1.0, Color::rgb(0.0, 0.0, 1.0))],
            angle: 10.0,
            scale: 1.5,
            style: GradientStyle::Linear,
            reverse: true,
            opacity_stops: Vec::new(),
            midpoints: Vec::new(),
            offset: (0.25, 0.5),
            dither: true,
            align: false,
        };
        let (k, data) = write_fill(&set);
        let Fill::Gradient { stops, angle, style, opacity_stops, midpoints, .. } = set.clone() else { panic!() };
        let plain = Fill::Gradient { stops, angle, scale: 1.0, style, reverse: false, opacity_stops, midpoints, offset: (0.0, 0.0), dither: false, align: true };
        let (_, d) = fill_to_desc_onto(&plain, Some((&k, &data)));
        let back = parse_fill(&k, &VersionedDescriptor::new(d).to_bytes()).unwrap();
        assert_eq!(back, plain);
        // No `Scl` where the file had none and the scale is 100 %.
        let (_, d) = fill_to_desc_onto(&plain, Some((&k, &write_fill(&plain).1)));
        assert!(d.get("Scl ").is_none());

        let rotated = Fill::Pattern { name: "P".into(), scale: 1.0, id: "id".into(), angle: 30.0, link: true, phase: (0.0, 0.0) };
        let (k, data) = write_fill(&rotated);
        let level = Fill::Pattern { name: "P".into(), scale: 1.0, id: "id".into(), angle: 0.0, link: true, phase: (0.0, 0.0) };
        let (_, d) = fill_to_desc_onto(&level, Some((&k, &data)));
        assert_eq!(parse_fill(&k, &VersionedDescriptor::new(d).to_bytes()), Some(level));
    }

    #[test]
    fn merge_keeps_unmodelled_keys() {
        let rgb = |r: f64| Value::Descriptor(Descriptor::new("RGBC").with("Rd  ", Value::Double(r)));
        let base = Descriptor::new("x")
            .with("a", Value::Integer(1))
            .with("old", Value::Integer(2))
            .with("Clr ", Value::Descriptor(Descriptor::new("Grsc").with("Gry ", Value::Double(5.0))))
            .with("list", Value::List(vec![Value::Descriptor(Descriptor::new("pt").with("extra", Value::Boolean(true)))]))
            .with("kept", Value::Integer(3));
        let over = Descriptor::new("x")
            .with("new", Value::Integer(4))
            .with("a", Value::Integer(10))
            .with("Clr ", rgb(1.0))
            .with("list", Value::List(vec![Value::Descriptor(Descriptor::new("pt").with("v", Value::Integer(7)))]))
            .with("kept", Value::Integer(30));
        let m = merge_descriptor(&base, over, &["kept"]);
        let keys: Vec<_> = m.items.iter().map(|(k, _)| String::from_utf8_lossy(k.as_bytes()).into_owned()).collect();
        assert_eq!(keys, ["a", "old", "Clr ", "list", "kept", "new"]);
        assert_eq!(m.get("a"), Some(&Value::Integer(10)));
        assert_eq!(m.get("kept"), Some(&Value::Integer(3)));
        // Another colour class replaces the old one outright.
        assert_eq!(m.get("Clr "), Some(&rgb(1.0)));
        let Some(Value::List(l)) = m.get("list") else { panic!() };
        let Value::Descriptor(pt) = &l[0] else { panic!() };
        assert_eq!(pt.items.len(), 2);
    }
}
