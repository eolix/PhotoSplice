//! Layer effects ↔ PSD `lfx2` (object-based effects descriptor) and legacy `lrFX`.
//!
//! Key names follow the descriptors Photoshop writes (documented by
//! reverse-engineering in MIT-licensed psd-tools / ag-psd).

use photosplice_color::{BlendMode, Color};
use photosplice_doc::adjust::CurvePoint;
use photosplice_doc::{
    Bevel, BevelContour, BevelStyle, BevelTechnique, BevelTexture, Contour, Effect, FxCommon, FxPaint, Glow, GlowSource, GlowTechnique, Gradient, Satin,
    Shadow, StrokeFx, StrokePosition,
};
use photosplice_psd::descriptor::{Descriptor, Id, UnicodeString, Value, VersionedDescriptor};

use crate::blocks::{
    bool_of, color_from_desc, color_to_desc, enum_of, get_desc, gradient_desc, gradient_style, gradient_style_value, num, pattern_placement, pattern_ref,
    pattern_ref_desc, with_pattern_placement,
};

const BLEND_NAMES: [(BlendMode, &str); 28] = [
    (BlendMode::Normal, "Nrml"),
    (BlendMode::Dissolve, "Dslv"),
    (BlendMode::Darken, "Drkn"),
    (BlendMode::Multiply, "Mltp"),
    (BlendMode::ColorBurn, "CBrn"),
    (BlendMode::LinearBurn, "linearBurn"),
    (BlendMode::DarkerColor, "darkerColor"),
    (BlendMode::Lighten, "Lghn"),
    (BlendMode::Screen, "Scrn"),
    (BlendMode::ColorDodge, "CDdg"),
    (BlendMode::LinearDodge, "linearDodge"),
    (BlendMode::LighterColor, "lighterColor"),
    (BlendMode::Overlay, "Ovrl"),
    (BlendMode::SoftLight, "SftL"),
    (BlendMode::HardLight, "HrdL"),
    (BlendMode::VividLight, "vividLight"),
    (BlendMode::LinearLight, "linearLight"),
    (BlendMode::PinLight, "pinLight"),
    (BlendMode::HardMix, "hardMix"),
    (BlendMode::Difference, "Dfrn"),
    (BlendMode::Exclusion, "Xclu"),
    (BlendMode::Subtract, "blendSubtraction"),
    (BlendMode::Divide, "blendDivide"),
    (BlendMode::Hue, "H   "),
    (BlendMode::Saturation, "Strt"),
    (BlendMode::Color, "Clr "),
    (BlendMode::Luminosity, "Lmns"),
    (BlendMode::PassThrough, "passThrough"),
];

/// Long (string-ID) names some writers use instead of the four-character codes, for the modes
/// whose code is a short ID (`BlnM` `multiply` = `Mltp`). The others already use their long name.
const BLEND_LONG_NAMES: [(BlendMode, &str); 17] = [
    (BlendMode::Normal, "normal"),
    (BlendMode::Dissolve, "dissolve"),
    (BlendMode::Darken, "darken"),
    (BlendMode::Multiply, "multiply"),
    (BlendMode::ColorBurn, "colorBurn"),
    (BlendMode::Lighten, "lighten"),
    (BlendMode::Screen, "screen"),
    (BlendMode::ColorDodge, "colorDodge"),
    (BlendMode::Overlay, "overlay"),
    (BlendMode::SoftLight, "softLight"),
    (BlendMode::HardLight, "hardLight"),
    (BlendMode::Difference, "difference"),
    (BlendMode::Exclusion, "exclusion"),
    (BlendMode::Hue, "hue"),
    (BlendMode::Saturation, "saturation"),
    (BlendMode::Color, "color"),
    (BlendMode::Luminosity, "luminosity"),
];

/// A blend-mode enum from its four-character code, its long string ID, or the layer record's key:
/// Reference 2026 writes `vLit`, `lddg`, `lbrn`… as an effect's `Md  ` for some modes.
pub(crate) fn blend_from_id(v: &[u8]) -> Option<BlendMode> {
    BLEND_NAMES
        .iter()
        .chain(&BLEND_LONG_NAMES)
        .find(|(_, n)| n.as_bytes() == v)
        .map(|(m, _)| *m)
        .or_else(|| <[u8; 4]>::try_from(v).ok().and_then(BlendMode::from_psd_key))
}

pub(crate) fn blend_of(d: &Descriptor, key: &str, default: BlendMode) -> BlendMode {
    enum_of(d, key).and_then(blend_from_id).unwrap_or(default)
}

/// A `BlnM` enum with the long string ID where the mode has one (`multiply`, `overlay`), as
/// Photoshop writes smart-filter blend options; the four-character code otherwise.
pub(crate) fn blend_long_value(m: BlendMode) -> Value {
    let n = BLEND_LONG_NAMES.iter().chain(&BLEND_NAMES).find(|(b, _)| *b == m).map_or("normal", |(_, n)| n);
    Value::Enumerated { type_id: Id::new("BlnM"), value: Id::new(n) }
}

pub(crate) fn blend_value(m: BlendMode) -> Value {
    let n = BLEND_NAMES.iter().find(|(b, _)| *b == m).map_or("Nrml", |(_, n)| n);
    Value::Enumerated { type_id: Id::new("BlnM"), value: Id::new(n) }
}

fn f(d: &Descriptor, key: &str, default: f32) -> f32 {
    num(d.get(key)).map_or(default, |v| v as f32)
}
fn pct(d: &Descriptor, key: &str, default: f32) -> f32 {
    f(d, key, default * 100.0) / 100.0
}
fn color(d: &Descriptor, key: &str) -> Color {
    get_desc(d, key).and_then(color_from_desc).unwrap_or(Color::BLACK)
}
fn contour(d: &Descriptor, key: &str) -> Contour {
    let Some(c) = get_desc(d, key) else { return Contour::Linear };
    let name = match c.get("Nm  ") {
        Some(Value::Text(t)) => t.to_string_lossy(),
        _ => String::new(),
    };
    let mut points = Vec::new();
    let mut corners = Vec::new();
    if let Some(Value::List(items)) = c.get("Crv ") {
        for it in items {
            if let Value::Descriptor(p) = it {
                points.push(CurvePoint { input: f(p, "Hrzn", 0.0) / 255.0, output: f(p, "Vrtc", 0.0) / 255.0 });
                // `Cnty` (continuity): false marks a corner point; absent means smooth.
                corners.push(matches!(p.get("Cnty"), Some(Value::Boolean(false))));
            }
        }
    }
    if !corners.contains(&true) {
        corners.clear();
    }
    let linear = points.len() == 2 && points[0].input == 0.0 && points[0].output == 0.0 && points[1].input == 1.0 && points[1].output == 1.0;
    if points.is_empty() || (linear && (name.is_empty() || name == "Linear")) { Contour::Linear } else { Contour::Custom { name, points, corners } }
}
/// The most points Reference reads in an effect's contour (`ShpC` › `Crv `): a layer whose effects
/// carry a longer one fails to load ("A problem was encountered reading layer … program error"),
/// and Reference then drops all of its effects; 16 loads.
const PSD_CONTOUR_POINTS: usize = 16;

/// Indices of the contour points a PSD keeps, in order: all of them up to [`PSD_CONTOUR_POINTS`],
/// else both ends and then, one at a time, the point farthest (in output) from the line between
/// the kept points around it, so peaks and valleys (Ring - Double's two rings) survive.
fn psd_contour_points(points: &[CurvePoint]) -> Vec<usize> {
    if points.len() <= PSD_CONTOUR_POINTS {
        return (0..points.len()).collect();
    }
    let mut kept = vec![0, points.len() - 1];
    while kept.len() < PSD_CONTOUR_POINTS {
        let mut farthest: Option<(f32, usize)> = None;
        for pair in kept.windows(2) {
            let &[a, b] = pair else { continue };
            let (Some(pa), Some(pb)) = (points.get(a), points.get(b)) else { continue };
            let span = pb.input - pa.input;
            for (i, p) in points.iter().enumerate().take(b).skip(a + 1) {
                // Only equal inputs (a vertical step) have no span: inputs a few ulps apart still do.
                let t = if span != 0.0 && span.is_finite() { (p.input - pa.input) / span } else { 0.0 };
                let off = (p.output - (pa.output + t * (pb.output - pa.output))).abs();
                if off.is_finite() && farthest.is_none_or(|(f, _)| off > f) {
                    farthest = Some((off, i));
                }
            }
        }
        let Some((_, i)) = farthest else { break };
        kept.insert(kept.partition_point(|&k| k < i), i);
    }
    kept
}

fn contour_value(c: &Contour) -> Value {
    let (name, points) = match c {
        Contour::Linear => ("Linear".to_string(), vec![CurvePoint { input: 0.0, output: 0.0 }, CurvePoint { input: 1.0, output: 1.0 }]),
        Contour::Custom { name, points, .. } => (name.clone(), points.clone()),
    };
    let pts = psd_contour_points(&points)
        .into_iter()
        .filter_map(|i| points.get(i).map(|p| (i, p)))
        .map(|(i, p)| {
            let d = Descriptor::new("CrPt").with("Hrzn", Value::Double(f64::from(p.input * 255.0))).with("Vrtc", Value::Double(f64::from(p.output * 255.0)));
            // Photoshop writes no `Cnty` (continuity) on smooth points; a corner carries `Cnty` false.
            Value::Descriptor(if c.is_corner(i) { d.with("Cnty", Value::Boolean(false)) } else { d })
        })
        .collect();
    Value::Descriptor(Descriptor::new("ShpC").with("Nm  ", Value::Text(UnicodeString::new_nul(&name))).with("Crv ", Value::List(pts)))
}

fn common(d: &Descriptor, blend: BlendMode, opacity: f32) -> FxCommon {
    FxCommon { enabled: !matches!(d.get("enab"), Some(Value::Boolean(false))), blend: blend_of(d, "Md  ", blend), opacity: pct(d, "Opct", opacity) }
}

fn gradient_from(d: &Descriptor) -> Gradient {
    let (stops, opacity_stops) = get_desc(d, "Grad").map(|g| crate::blocks::gradient_stops_with(g, crate::blocks::gradient_method(d))).unwrap_or_default();
    // A baked gradient keeps the count of its own colour stops (glows sample by it).
    let color_stops = get_desc(d, "Grad").map(crate::blocks::gradient_stops_raw).filter(|r| !r.noise && r.stops.len() != stops.len()).map_or(0, |r| u16::try_from(r.stops.len()).unwrap_or(u16::MAX));
    let off = get_desc(d, "Ofst");
    Gradient {
        stops: if stops.is_empty() { Gradient::default().stops } else { stops },
        opacity_stops,
        style: gradient_style(d),
        angle: f(d, "Angl", 90.0),
        scale: pct(d, "Scl ", 1.0),
        reverse: bool_of(d, "Rvrs"),
        align: !matches!(d.get("Algn"), Some(Value::Boolean(false))),
        offset: off.map_or((0.0, 0.0), |o| (pct(o, "Hrzn", 0.0), pct(o, "Vrtc", 0.0))),
        color_stops,
        source: get_desc(d, "Grad").map(|g| Box::new(crate::gradient_source::from_desc(g, crate::blocks::gradient_method(d)))),
    }
}

fn parse_shadow(d: &Descriptor, inner: bool) -> Shadow {
    Shadow {
        common: common(d, BlendMode::Multiply, 0.75),
        color: color(d, "Clr "),
        angle: f(d, "lagl", 120.0),
        use_global_light: !matches!(d.get("uglg"), Some(Value::Boolean(false))),
        distance: f(d, "Dstn", 5.0),
        spread: pct(d, "Ckmt", 0.0),
        size: f(d, "blur", 5.0),
        contour: contour(d, "TrnS"),
        anti_alias: bool_of(d, "AntA"),
        noise: pct(d, "Nose", 0.0),
        knocks_out: !inner && !matches!(d.get("layerConceals"), Some(Value::Boolean(false))),
    }
}

fn parse_glow(d: &Descriptor, inner: bool) -> Glow {
    // A gradient's opacity follows its smoothness here too (temp kit 15: an opacity ramp from 0
    // to 100 % at Smoothness 100 % eases as baked, 16.8 → 0.1 % of pixels off against the stops
    // as stored).
    let paint = match get_desc(d, "Grad") {
        Some(_) => FxPaint::Gradient(gradient_from(d)),
        None => FxPaint::Color(color(d, "Clr ")),
    };
    Glow {
        common: common(d, BlendMode::Screen, 0.75),
        paint,
        technique: if enum_of(d, "GlwT") == Some(b"PrBL") { GlowTechnique::Precise } else { GlowTechnique::Softer },
        spread: pct(d, "Ckmt", 0.0),
        size: f(d, "blur", 5.0),
        contour: contour(d, "TrnS"),
        anti_alias: bool_of(d, "AntA"),
        range: pct(d, "Inpr", 0.5),
        jitter: pct(d, "ShdN", 0.0),
        noise: pct(d, "Nose", 0.0),
        source: if inner && enum_of(d, "glwS") == Some(b"SrcC") { GlowSource::Center } else { GlowSource::Edge },
    }
}

fn parse_stroke(d: &Descriptor) -> StrokeFx {
    let paint = match enum_of(d, "PntT") {
        Some(b"GrFl") => FxPaint::Gradient(gradient_from(d)),
        Some(b"Ptrn") => {
            let p = get_desc(d, "Ptrn");
            let text = |k: &str| match p.and_then(|p| p.get(k)) {
                Some(Value::Text(t)) => t.to_string_lossy(),
                _ => String::new(),
            };
            FxPaint::Pattern { name: text("Nm  "), id: text("Idnt"), scale: pct(d, "Scl ", 1.0) }
        }
        _ => FxPaint::Color(color(d, "Clr ")),
    };
    StrokeFx {
        common: common(d, BlendMode::Normal, 1.0),
        size: f(d, "Sz  ", 3.0),
        position: match enum_of(d, "Styl") {
            Some(b"InsF") => StrokePosition::Inside,
            Some(b"CtrF") => StrokePosition::Center,
            _ => StrokePosition::Outside,
        },
        paint,
    }
}

fn parse_bevel(d: &Descriptor) -> Bevel {
    Bevel {
        enabled: !matches!(d.get("enab"), Some(Value::Boolean(false))),
        style: match enum_of(d, "bvlS") {
            Some(b"OtrB") => BevelStyle::OuterBevel,
            Some(b"Embs") => BevelStyle::Emboss,
            Some(b"PlEb") => BevelStyle::PillowEmboss,
            Some(b"strokeEmboss") => BevelStyle::StrokeEmboss,
            _ => BevelStyle::InnerBevel,
        },
        technique: match enum_of(d, "bvlT") {
            Some(b"PrBL") => BevelTechnique::ChiselHard,
            Some(b"Slmt") => BevelTechnique::ChiselSoft,
            _ => BevelTechnique::Smooth,
        },
        depth: pct(d, "srgR", 1.0),
        up: enum_of(d, "bvlD") != Some(b"Out "),
        size: f(d, "blur", 5.0),
        soften: f(d, "Sftn", 0.0),
        angle: f(d, "lagl", 120.0),
        altitude: f(d, "Lald", 30.0),
        use_global_light: !matches!(d.get("uglg"), Some(Value::Boolean(false))),
        gloss_contour: contour(d, "TrnS"),
        gloss_anti_alias: bool_of(d, "antialiasGloss"),
        highlight: FxCommon { enabled: true, blend: blend_of(d, "hglM", BlendMode::Screen), opacity: pct(d, "hglO", 0.75) },
        highlight_color: get_desc(d, "hglC").and_then(color_from_desc).unwrap_or(Color::WHITE),
        shadow: FxCommon { enabled: true, blend: blend_of(d, "sdwM", BlendMode::Multiply), opacity: pct(d, "sdwO", 0.75) },
        shadow_color: color(d, "sdwC"),
        contour: bool_of(d, "useShape").then(|| BevelContour { contour: contour(d, "MpgS"), range: pct(d, "Inpr", 0.5), anti_alias: bool_of(d, "AntA") }),
        texture: bool_of(d, "useTexture").then(|| {
            let (name, id) = pattern_ref(d);
            let (_, link, phase) = pattern_placement(d);
            BevelTexture { name, id, scale: pct(d, "Scl ", 1.0), depth: pct(d, "textureDepth", 1.0), invert: bool_of(d, "InvT"), link, phase }
        }),
    }
}

fn parse_one(key: &str, d: &Descriptor) -> Option<Effect> {
    Some(match key {
        "DrSh" => Effect::DropShadow(parse_shadow(d, false)),
        "IrSh" => Effect::InnerShadow(parse_shadow(d, true)),
        "OrGl" => Effect::OuterGlow(parse_glow(d, false)),
        "IrGl" => Effect::InnerGlow(parse_glow(d, true)),
        "FrFX" => Effect::Stroke(parse_stroke(d)),
        "SoFi" => Effect::ColorOverlay { common: common(d, BlendMode::Normal, 1.0), color: color(d, "Clr ") },
        "GrFl" => Effect::GradientOverlay { common: common(d, BlendMode::Normal, 1.0), gradient: gradient_from(d), dither: bool_of(d, "Dthr") },
        "patternFill" => {
            let (name, id) = pattern_ref(d);
            let (angle, link, phase) = pattern_placement(d);
            Effect::PatternOverlay { common: common(d, BlendMode::Normal, 1.0), name, id, scale: pct(d, "Scl ", 1.0), angle, link, phase }
        }
        "ChFX" => Effect::Satin(Satin {
            common: common(d, BlendMode::Multiply, 0.5),
            color: color(d, "Clr "),
            angle: f(d, "lagl", 19.0),
            distance: f(d, "Dstn", 11.0),
            size: f(d, "blur", 14.0),
            contour: contour(d, "MpgS"),
            anti_alias: bool_of(d, "AntA"),
            invert: bool_of(d, "Invr"),
        }),
        "ebbl" => Effect::BevelEmboss(parse_bevel(d)),
        _ => return None,
    })
}

/// Single-instance key and multi-instance list key for each kind.
const KINDS: [(&str, &str); 10] = [
    ("DrSh", "dropShadowMulti"),
    ("IrSh", "innerShadowMulti"),
    ("OrGl", ""),
    ("IrGl", ""),
    ("ebbl", ""),
    ("ChFX", ""),
    ("SoFi", "solidFillMulti"),
    ("GrFl", "gradientFillMulti"),
    ("patternFill", ""),
    ("FrFX", "frameFXMulti"),
];

/// An effect Photoshop keeps only so the Layer Style dialog remembers its settings
/// (`present: false`): current Photoshop writes one for every kind the layer doesn't use. It is
/// not part of the layer's effects.
fn placeholder(d: &Descriptor) -> bool {
    matches!(d.get("present"), Some(Value::Boolean(false)))
}

/// Parses `lfx2` block data: `(master switch, effects)`.
pub fn parse_lfx2(data: &[u8]) -> Option<(bool, Vec<Effect>)> {
    let (vd, _) = VersionedDescriptor::parse_prefix(data.get(4..)?).ok()?;
    Some(effects_from_desc(&vd.descriptor))
}

/// Effects of an `lfx2`-shaped descriptor (also a layer comp's `Lefx`): `(master switch, effects)`.
pub(crate) fn effects_from_desc(d: &Descriptor) -> (bool, Vec<Effect>) {
    let master = !matches!(d.get("masterFXSwitch"), Some(Value::Boolean(false)));
    let mut out = Vec::new();
    for (single, multi) in KINDS {
        let from_multi = (!multi.is_empty()).then(|| d.get(multi)).flatten();
        match from_multi {
            Some(Value::List(items)) => {
                for it in items {
                    if let Value::Descriptor(x) = it
                        && !placeholder(x)
                        && let Some(e) = parse_one(single, x)
                    {
                        out.push(e);
                    }
                }
            }
            _ => {
                if let Some(x) = get_desc(d, single)
                    && !placeholder(x)
                    && let Some(e) = parse_one(single, x)
                {
                    out.push(e);
                }
            }
        }
    }
    (master, out)
}

fn unit(u: &[u8; 4], v: f32) -> Value {
    Value::UnitFloat { unit: *u, value: f64::from(v) }
}

fn with_common(d: Descriptor, c: &FxCommon) -> Descriptor {
    d.with("enab", Value::Boolean(c.enabled))
        .with("present", Value::Boolean(true))
        .with("showInDialog", Value::Boolean(true))
        .with("Md  ", blend_value(c.blend))
        .with("Opct", unit(b"#Prc", c.opacity * 100.0))
}

/// The gradient keys of an effect: its definition when `source` (see [`write_one`]), naming its
/// method under `method_key` (Reference writes `gs99` in Gradient Overlays,
/// `gradientsInterpolationMethod` in strokes and glows), else its stops.
fn gradient_keys(mut d: Descriptor, g: &Gradient, method_key: &str, source: bool) -> Descriptor {
    match g.source.as_deref().filter(|_| source) {
        Some(src) => {
            d = d.with("Grad", Value::Descriptor(crate::gradient_source::to_desc(src)));
            if let Some(m) = src.method {
                d = d.with(method_key, crate::gradient_source::method_value(m));
            }
        }
        None => d = d.with("Grad", Value::Descriptor(gradient_desc(&g.stops, &g.opacity_stops))),
    }
    d = d
        .with("Angl", unit(b"#Ang", g.angle))
        .with("Type", gradient_style_value(g.style))
        .with("Rvrs", Value::Boolean(g.reverse))
        .with("Algn", Value::Boolean(g.align))
        .with("Scl ", unit(b"#Prc", g.scale * 100.0));
    d.with("Ofst", Value::Descriptor(Descriptor::new("Pnt ").with("Hrzn", unit(b"#Prc", g.offset.0 * 100.0)).with("Vrtc", unit(b"#Prc", g.offset.1 * 100.0))))
}

/// The descriptor key an effect is written under.
fn kind_key(e: &Effect) -> &'static str {
    match e {
        Effect::DropShadow(_) => "DrSh",
        Effect::InnerShadow(_) => "IrSh",
        Effect::OuterGlow(_) => "OrGl",
        Effect::InnerGlow(_) => "IrGl",
        Effect::Stroke(_) => "FrFX",
        Effect::ColorOverlay { .. } => "SoFi",
        Effect::GradientOverlay { .. } => "GrFl",
        Effect::PatternOverlay { .. } => "patternFill",
        Effect::Satin(_) => "ChFX",
        Effect::BevelEmboss(_) => "ebbl",
    }
}

/// An effect as (key, descriptor). A gradient is written as the definition it was read from
/// ([`Gradient::source`]) while that still reads back as its stops (they weren't edited), so
/// Reference renders it as it did; else as its (baked) stops.
fn write_one(e: &Effect) -> (&'static str, Descriptor) {
    if let Some(model) = gradient_stops(e).filter(|_| gradient_of(e).is_some_and(|g| g.source.is_some())) {
        let (k, d) = write_one_with(e, true);
        let back = parse_one(k, &d);
        if back
            .as_ref()
            .and_then(gradient_stops)
            .is_some_and(|(s, o)| crate::gradient_source::same_color_stops(s, model.0) && crate::gradient_source::same_opacity_stops(o, model.1))
        {
            return (k, d);
        }
    }
    write_one_with(e, false)
}

fn write_one_with(e: &Effect, source: bool) -> (&'static str, Descriptor) {
    match e {
        Effect::DropShadow(s) | Effect::InnerShadow(s) => {
            let inner = matches!(e, Effect::InnerShadow(_));
            let mut d = with_common(Descriptor::new(if inner { "IrSh" } else { "DrSh" }), &s.common)
                .with("Clr ", Value::Descriptor(color_to_desc(&s.color)))
                .with("uglg", Value::Boolean(s.use_global_light))
                .with("lagl", unit(b"#Ang", s.angle))
                .with("Dstn", unit(b"#Pxl", s.distance))
                .with("Ckmt", unit(b"#Pxl", s.spread * 100.0))
                .with("blur", unit(b"#Pxl", s.size))
                .with("Nose", unit(b"#Prc", s.noise * 100.0))
                .with("AntA", Value::Boolean(s.anti_alias))
                .with("TrnS", contour_value(&s.contour));
            if !inner {
                d = d.with("layerConceals", Value::Boolean(s.knocks_out));
            }
            (if inner { "IrSh" } else { "DrSh" }, d)
        }
        Effect::OuterGlow(g) | Effect::InnerGlow(g) => {
            let inner = matches!(e, Effect::InnerGlow(_));
            let mut d = with_common(Descriptor::new(if inner { "IrGl" } else { "OrGl" }), &g.common);
            d = match &g.paint {
                FxPaint::Gradient(gr) => gradient_keys(d, gr, "gradientsInterpolationMethod", source),
                FxPaint::Color(c) => d.with("Clr ", Value::Descriptor(color_to_desc(c))),
                FxPaint::Pattern { .. } => d.with("Clr ", Value::Descriptor(color_to_desc(&Color::WHITE))),
            };
            d = d
                .with(
                    "GlwT",
                    Value::Enumerated { type_id: Id::new("BETE"), value: Id::new(if g.technique == GlowTechnique::Precise { "PrBL" } else { "SfBL" }) },
                )
                .with("Ckmt", unit(b"#Pxl", g.spread * 100.0))
                .with("blur", unit(b"#Pxl", g.size))
                .with("Nose", unit(b"#Prc", g.noise * 100.0))
                .with("ShdN", unit(b"#Prc", g.jitter * 100.0))
                .with("AntA", Value::Boolean(g.anti_alias))
                .with("TrnS", contour_value(&g.contour))
                .with("Inpr", unit(b"#Prc", g.range * 100.0));
            if inner {
                d = d
                    .with("glwS", Value::Enumerated { type_id: Id::new("IGSr"), value: Id::new(if g.source == GlowSource::Center { "SrcC" } else { "SrcE" }) });
            }
            (if inner { "IrGl" } else { "OrGl" }, d)
        }
        Effect::Stroke(s) => {
            let styl = match s.position {
                StrokePosition::Outside => "OutF",
                StrokePosition::Inside => "InsF",
                StrokePosition::Center => "CtrF",
            };
            let mut d = with_common(Descriptor::new("FrFX"), &s.common)
                .with("Styl", Value::Enumerated { type_id: Id::new("FStl"), value: Id::new(styl) })
                .with("Sz  ", unit(b"#Pxl", s.size));
            d = match &s.paint {
                FxPaint::Color(c) => {
                    d.with("PntT", Value::Enumerated { type_id: Id::new("FrFl"), value: Id::new("SClr") }).with("Clr ", Value::Descriptor(color_to_desc(c)))
                }
                FxPaint::Gradient(g) => gradient_keys(
                    d.with("PntT", Value::Enumerated { type_id: Id::new("FrFl"), value: Id::new("GrFl") }),
                    g,
                    "gradientsInterpolationMethod",
                    source,
                ),
                FxPaint::Pattern { name, id, scale } => d
                    .with("PntT", Value::Enumerated { type_id: Id::new("FrFl"), value: Id::new("Ptrn") })
                    .with(
                        "Ptrn",
                        Value::Descriptor(
                            Descriptor::new("Ptrn")
                                .with("Nm  ", Value::Text(UnicodeString::new_nul(name)))
                                .with("Idnt", Value::Text(UnicodeString::new_nul(id))),
                        ),
                    )
                    .with("Scl ", unit(b"#Prc", scale * 100.0)),
            };
            ("FrFX", d)
        }
        Effect::ColorOverlay { common: c, color } => ("SoFi", with_common(Descriptor::new("SoFi"), c).with("Clr ", Value::Descriptor(color_to_desc(color)))),
        Effect::GradientOverlay { common: c, gradient, dither } => {
            ("GrFl", gradient_keys(with_common(Descriptor::new("GrFl"), c), gradient, "gs99", source).with("Dthr", Value::Boolean(*dither)))
        }
        Effect::PatternOverlay { common: c, name, id, scale, angle, link, phase } => (
            "patternFill",
            with_pattern_placement(
                with_common(Descriptor::new("patternFill"), c)
                    .with("Ptrn", Value::Descriptor(pattern_ref_desc(name, id)))
                    .with("Scl ", unit(b"#Prc", scale * 100.0)),
                *angle,
                *link,
                *phase,
            ),
        ),
        Effect::Satin(s) => (
            "ChFX",
            with_common(Descriptor::new("ChFX"), &s.common)
                .with("Clr ", Value::Descriptor(color_to_desc(&s.color)))
                .with("AntA", Value::Boolean(s.anti_alias))
                .with("Invr", Value::Boolean(s.invert))
                .with("lagl", unit(b"#Ang", s.angle))
                .with("Dstn", unit(b"#Pxl", s.distance))
                .with("blur", unit(b"#Pxl", s.size))
                .with("MpgS", contour_value(&s.contour)),
        ),
        Effect::BevelEmboss(b) => {
            let style = match b.style {
                BevelStyle::OuterBevel => "OtrB",
                BevelStyle::InnerBevel => "InrB",
                BevelStyle::Emboss => "Embs",
                BevelStyle::PillowEmboss => "PlEb",
                BevelStyle::StrokeEmboss => "strokeEmboss",
            };
            let tech = match b.technique {
                BevelTechnique::Smooth => "SfBL",
                BevelTechnique::ChiselHard => "PrBL",
                BevelTechnique::ChiselSoft => "Slmt",
            };
            let d = Descriptor::new("ebbl")
                .with("enab", Value::Boolean(b.enabled))
                .with("present", Value::Boolean(true))
                .with("showInDialog", Value::Boolean(true))
                .with("hglM", blend_value(b.highlight.blend))
                .with("hglC", Value::Descriptor(color_to_desc(&b.highlight_color)))
                .with("hglO", unit(b"#Prc", b.highlight.opacity * 100.0))
                .with("sdwM", blend_value(b.shadow.blend))
                .with("sdwC", Value::Descriptor(color_to_desc(&b.shadow_color)))
                .with("sdwO", unit(b"#Prc", b.shadow.opacity * 100.0))
                .with("bvlT", Value::Enumerated { type_id: Id::new("bvlT"), value: Id::new(tech) })
                .with("bvlS", Value::Enumerated { type_id: Id::new("BESl"), value: Id::new(style) })
                .with("uglg", Value::Boolean(b.use_global_light))
                .with("lagl", unit(b"#Ang", b.angle))
                .with("Lald", unit(b"#Ang", b.altitude))
                .with("srgR", unit(b"#Prc", b.depth * 100.0))
                .with("blur", unit(b"#Pxl", b.size))
                .with("bvlD", Value::Enumerated { type_id: Id::new("BESs"), value: Id::new(if b.up { "In  " } else { "Out " }) })
                .with("TrnS", contour_value(&b.gloss_contour))
                .with("Sftn", unit(b"#Pxl", b.soften))
                .with("antialiasGloss", Value::Boolean(b.gloss_anti_alias));
            let d = match &b.contour {
                Some(c) => d
                    .with("useShape", Value::Boolean(true))
                    .with("MpgS", contour_value(&c.contour))
                    .with("AntA", Value::Boolean(c.anti_alias))
                    .with("Inpr", unit(b"#Prc", c.range * 100.0)),
                None => d.with("useShape", Value::Boolean(false)),
            };
            let d = match &b.texture {
                Some(t) => with_pattern_placement(
                    d.with("useTexture", Value::Boolean(true))
                        .with("InvT", Value::Boolean(t.invert))
                        .with("Scl ", unit(b"#Prc", t.scale * 100.0))
                        .with("textureDepth", unit(b"#Prc", t.depth * 100.0))
                        .with("Ptrn", Value::Descriptor(pattern_ref_desc(&t.name, &t.id))),
                    0.0,
                    t.link,
                    t.phase,
                ),
                None => d.with("useTexture", Value::Boolean(false)),
            };
            ("ebbl", d)
        }
    }
}

/// Serializes effects as `lfx2` block data.
pub fn write_lfx2(master: bool, items: &[Effect]) -> Vec<u8> {
    lfx2_data(lfx2_descriptor(master, items))
}

/// Serializes effects onto the `lfx2` or `lmfx` block data the file had (`template`), keeping
/// what the document model doesn't carry: unmodelled keys of each effect, the effects scale,
/// Photoshop's placeholders for unused kinds (an effect removed here becomes one), and the
/// order. `numModifyingFX` is recomputed. Returns the data and whether a kind has several
/// instances, in which case Photoshop stores the block as `lmfx`.
pub fn write_lfx2_onto(template: Option<&[u8]>, master: bool, items: &[Effect]) -> (Vec<u8>, bool) {
    write_lfx2_onto_at(template, master, items, 72.0)
}

/// [`write_lfx2_onto`] for a document of `dpi`: a block written from scratch carries the
/// effects scale Reference writes, the resolution over 72 (208.33 % at 150 dpi). Reference sizes
/// effects by that scale over its own: 100 % in a 150 dpi file drew every effect at 48 % of its
/// size (psd-tools layer_effects saved by us: its bevel and stroke 33/255 off).
pub fn write_lfx2_onto_at(template: Option<&[u8]>, master: bool, items: &[Effect], dpi: f64) -> (Vec<u8>, bool) {
    let mut fresh = lfx2_descriptor(master, items);
    let scale = if dpi.is_finite() && dpi > 0.0 { dpi / 72.0 * 100.0 } else { 100.0 };
    set_key(&mut fresh, "Scl ", Value::UnitFloat { unit: *b"#Prc", value: scale });
    let multi = KINDS.iter().any(|(single, multi)| !multi.is_empty() && instances(&fresh, single, multi).len() > 1);
    let base = template.and_then(|t| VersionedDescriptor::parse_prefix(t.get(4..)?).ok()).map(|(v, _)| v.descriptor);
    let d = match base {
        Some(base) => merge_lfx2(&base, fresh, items),
        None => fresh,
    };
    (lfx2_data(d), multi)
}

fn lfx2_data(d: Descriptor) -> Vec<u8> {
    let mut out = 0u32.to_be_bytes().to_vec();
    out.extend(VersionedDescriptor::new(d).to_bytes());
    out
}

/// Index into [`KINDS`] of a single or multi key.
fn kind_index(k: &Id) -> Option<usize> {
    KINDS.iter().position(|(single, multi)| k.is(single) || (!multi.is_empty() && k.is(multi)))
}

/// Every instance of one kind, from its multi list and its single key.
fn instances(d: &Descriptor, single: &str, multi: &str) -> Vec<Descriptor> {
    let mut out = Vec::new();
    if !multi.is_empty()
        && let Some(Value::List(xs)) = d.get(multi)
    {
        out.extend(xs.iter().filter_map(|x| if let Value::Descriptor(x) = x { Some(x.clone()) } else { None }));
    }
    out.extend(get_desc(d, single).cloned());
    out
}

fn set_key(d: &mut Descriptor, key: &str, v: Value) {
    match d.items.iter_mut().find(|(k, _)| k.is(key)) {
        Some((_, old)) => *old = v,
        None => d.items.push((Id::new(key), v)),
    }
}

/// See [`write_lfx2_onto`].
fn merge_lfx2(base: &Descriptor, fresh: Descriptor, items: &[Effect]) -> Descriptor {
    let mut out = Descriptor { name: base.name.clone(), class_id: base.class_id.clone(), items: Vec::new() };
    let mut done = [false; KINDS.len()];
    for (k, v) in &base.items {
        if let Some(i) = kind_index(k) {
            if !done[i] {
                done[i] = true;
                out.items.extend(merge_kind(i, base, &fresh, items));
            }
        } else if k.is("Scl ") {
            // The effects scale isn't modelled.
            out.items.push((k.clone(), v.clone()));
        } else if let Some((_, fv)) = fresh.items.iter().find(|(fk, _)| fk == k) {
            out.items.push((k.clone(), fv.clone()));
        } else {
            out.items.push((k.clone(), v.clone()));
        }
    }
    for (i, d) in done.iter().enumerate() {
        if !d {
            out.items.extend(merge_kind(i, base, &fresh, items));
        }
    }
    for (k, v) in fresh.items {
        if kind_index(&k).is_none() && !out.items.iter().any(|(ok, _)| *ok == k) {
            out.items.push((k, v));
        }
    }
    // The number of effects in use (present and enabled), whatever the master switch says.
    let active = KINDS
        .iter()
        .flat_map(|(single, multi)| instances(&out, single, multi))
        .filter(|d| !placeholder(d) && !matches!(d.get("enab"), Some(Value::Boolean(false))))
        .count();
    if let Some((_, v)) = out.items.iter_mut().find(|(k, _)| k.is("numModifyingFX")) {
        *v = Value::Integer(i32::try_from(active).unwrap_or(i32::MAX));
    }
    out
}

/// Entries for kind `i`: the document's instances merged onto the file's (pairwise when the
/// counts match), or the file's instances as placeholders when the document has none.
fn merge_kind(i: usize, base: &Descriptor, fresh: &Descriptor, items: &[Effect]) -> Vec<(Id, Value)> {
    let Some(&(single, multi)) = KINDS.get(i) else { return Vec::new() };
    let old = instances(base, single, multi);
    let new = instances(fresh, single, multi);
    // The document's effects of this kind, in the order `new` holds them.
    let model: Vec<&Effect> = items.iter().filter(|e| kind_key(e) == single).collect();
    let (items, as_list) = if new.is_empty() {
        let kept: Vec<Descriptor> = old
            .into_iter()
            .map(|mut d| {
                set_key(&mut d, "enab", Value::Boolean(false));
                set_key(&mut d, "present", Value::Boolean(false));
                d
            })
            .collect();
        // Placeholders keep the file's form (Photoshop lists them under the multi key).
        let as_list = !multi.is_empty() && matches!(base.get(multi), Some(Value::List(_)));
        (kept, as_list)
    } else {
        let merged = if old.len() == new.len() {
            old.iter()
                .zip(new)
                .enumerate()
                .map(|(k, (o, n))| {
                    // `showInDialog` isn't modelled; a placeholder's says nothing about a live
                    // effect. A gradient whose stops weren't edited keeps the file's definition
                    // (colour model, noise gradient, name, interpolation).
                    let file = parse_one(single, o);
                    let file_stops = file.as_ref().and_then(gradient_stops);
                    let model_stops = model.get(k).and_then(|e| gradient_stops(e));
                    let same_gradient = file_stops.is_some() && file_stops == model_stops;
                    // A regenerated gradient is written baked (Classic): the file's method goes.
                    let n = if model_stops.is_some() && !same_gradient { crate::blocks::with_baked_method(o, n) } else { n };
                    let keep: &[&str] = match (placeholder(o), same_gradient) {
                        (true, _) => &[],
                        (false, true) => &["showInDialog", "Grad"],
                        (false, false) => &["showInDialog"],
                    };
                    crate::blocks::merge_descriptor(o, n, keep)
                })
                .collect()
        } else {
            new
        };
        let as_list = merged.len() > 1 && !multi.is_empty();
        (merged, as_list)
    };
    match (items.len(), as_list) {
        (0, _) => Vec::new(),
        (_, true) => vec![(Id::new(multi), Value::List(items.into_iter().map(Value::Descriptor).collect()))],
        _ => items.into_iter().take(1).map(|d| (Id::new(single), Value::Descriptor(d))).collect(),
    }
}

/// A gradient's definition: its colour stops and opacity stops.
type GradientStops<'a> = (&'a [(f32, Color)], &'a [(f32, f32)]);

/// The gradient definition an effect paints with, if any.
fn gradient_stops(e: &Effect) -> Option<GradientStops<'_>> {
    gradient_of(e).map(|g| (g.stops.as_slice(), g.opacity_stops.as_slice()))
}

/// The gradient an effect paints with, if any.
fn gradient_of(e: &Effect) -> Option<&Gradient> {
    match e {
        Effect::GradientOverlay { gradient, .. } => Some(gradient),
        Effect::Stroke(StrokeFx { paint: FxPaint::Gradient(g), .. })
        | Effect::OuterGlow(Glow { paint: FxPaint::Gradient(g), .. })
        | Effect::InnerGlow(Glow { paint: FxPaint::Gradient(g), .. }) => Some(g),
        _ => None,
    }
}

pub(crate) fn lfx2_descriptor(master: bool, items: &[Effect]) -> Descriptor {
    let mut d = Descriptor::new("null").with("Scl ", unit(b"#Prc", 100.0)).with("masterFXSwitch", Value::Boolean(master));
    for (single, multi) in KINDS {
        let of_kind: Vec<Descriptor> = items.iter().filter(|e| kind_key(e) == single).map(|e| write_one(e).1).collect();
        match of_kind.len() {
            0 => {}
            1 => d = d.with(single, Value::Descriptor(of_kind.into_iter().next().unwrap_or_default())),
            _ if !multi.is_empty() => d = d.with(multi, Value::List(of_kind.into_iter().map(Value::Descriptor).collect())),
            // Kinds without a multi key keep only the first instance.
            _ => d = d.with(single, Value::Descriptor(of_kind.into_iter().next().unwrap_or_default())),
        }
    }
    d
}

fn fixed(v: u32) -> f32 {
    v as i32 as f32 / 65536.0
}

/// Parses legacy `lrFX` effects (Photoshop 5 layout): drop/inner shadows,
/// outer/inner glows and solid fill. Bevels are skipped.
pub fn parse_lrfx(data: &[u8]) -> Option<(bool, Vec<Effect>)> {
    let be16 = |at: usize| data.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    let be32 = |at: usize| data.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    let color_at = |at: usize| -> Option<Color> {
        // space (2) + 4 × u16 components (RGB uses the first three).
        let c = |k: usize| be16(at + 2 + k * 2).map(|v| f32::from(v >> 8) / 255.0);
        Some(Color::rgb(c(0)?, c(1)?, c(2)?))
    };
    let blend_at = |at: usize| -> BlendMode {
        data.get(at + 4..at + 8).and_then(|k| photosplice_color::BlendMode::from_psd_key([k[0], k[1], k[2], k[3]])).unwrap_or(BlendMode::Normal)
    };
    let count = be16(2)?;
    let mut at = 4;
    let mut out = Vec::new();
    for _ in 0..count {
        let sig = data.get(at..at + 4)?;
        if sig != b"8BIM" {
            return None;
        }
        let key: [u8; 4] = data.get(at + 4..at + 8)?.try_into().ok()?;
        let size = be32(at + 8)? as usize;
        let b = at + 12; // start of this effect's data (after the size field)
        match &key {
            b"dsdw" | b"isdw" => {
                // Blur, angle and distance are 16.16 fixed point in practice.
                let blur = fixed(be32(b + 4)?);
                let angle = fixed(be32(b + 12)?);
                let distance = fixed(be32(b + 16)?);
                let color = color_at(b + 20)?;
                let blend = blend_at(b + 30);
                let enabled = *data.get(b + 38)? != 0;
                let use_global = *data.get(b + 39)? != 0;
                let opacity = f32::from(*data.get(b + 40)?) / 255.0;
                let s = Shadow {
                    common: FxCommon { enabled, blend, opacity },
                    color,
                    angle,
                    use_global_light: use_global,
                    distance,
                    spread: 0.0,
                    size: blur,
                    contour: Contour::Linear,
                    anti_alias: false,
                    noise: 0.0,
                    knocks_out: &key == b"dsdw",
                };
                out.push(if &key == b"dsdw" { Effect::DropShadow(s) } else { Effect::InnerShadow(s) });
            }
            b"oglw" | b"iglw" => {
                let blur = fixed(be32(b + 4)?);
                let color = color_at(b + 12)?;
                let blend = blend_at(b + 22);
                let enabled = *data.get(b + 30)? != 0;
                let opacity = f32::from(*data.get(b + 31)?) / 255.0;
                let g = Glow {
                    common: FxCommon { enabled, blend, opacity },
                    paint: FxPaint::Color(color),
                    technique: GlowTechnique::Softer,
                    spread: 0.0,
                    size: blur,
                    contour: Contour::Linear,
                    anti_alias: false,
                    range: 0.5,
                    jitter: 0.0,
                    noise: 0.0,
                    source: GlowSource::Edge,
                };
                out.push(if &key == b"oglw" { Effect::OuterGlow(g) } else { Effect::InnerGlow(g) });
            }
            b"sofi" => {
                let blend = blend_at(b + 4);
                let color = color_at(b + 12)?;
                let opacity = f32::from(*data.get(b + 22)?) / 255.0;
                let enabled = *data.get(b + 23)? != 0;
                out.push(Effect::ColorOverlay { common: FxCommon { enabled, blend, opacity }, color });
            }
            _ => {}
        }
        at = b + size;
    }
    Some((true, out))
}

#[cfg(test)]
#[allow(clippy::unreachable)] // clippy.toml exempts unwrap/expect/panic in tests, not unreachable!
mod tests {
    use super::*;

    fn crv(v: &Value) -> Vec<(f64, f64)> {
        let Value::Descriptor(d) = v else { panic!("ShpC") };
        let Some(Value::List(pts)) = d.get("Crv ") else { panic!("Crv ") };
        pts.iter()
            .map(|p| {
                let Value::Descriptor(p) = p else { panic!("CrPt") };
                let n = |k: &str| match p.get(k) {
                    Some(Value::Double(x)) => *x,
                    _ => panic!("{k}"),
                };
                (n("Hrzn"), n("Vrtc"))
            })
            .collect()
    }

    /// Reference drops every effect of a layer whose contour has more than 16 points. A 29-point
    /// contour with two rings is written as 16 points that keep both ends and both peaks; up to 16
    /// points are written as they are.
    #[test]
    fn psd_contours_keep_at_most_16_points_and_their_peaks() {
        let points: Vec<CurvePoint> = (0..29)
            .map(|i| {
                let x = i as f32 / 28.0;
                // Rings at x = 0.25 and 0.75, small wiggles between.
                let y = (1.0 - ((x - 0.25).abs() * 8.0)).max(0.0).max(1.0 - (x - 0.75).abs() * 8.0) + if i % 2 == 0 { 0.0 } else { 0.01 };
                CurvePoint { input: x, output: y.clamp(0.0, 1.0) }
            })
            .collect();
        let c = Contour::Custom { name: "Two rings".into(), points: points.clone(), corners: vec![false; 29] };
        let out = crv(&contour_value(&c));
        assert_eq!(out.len(), 16);
        assert_eq!(out.first().copied(), Some((0.0, f64::from(points[0].output) * 255.0)));
        assert_eq!(out.last().map(|p| p.0), Some(255.0));
        let peak = |x: f32| out.iter().any(|p| ((p.0 / 255.0) as f32 - x).abs() < 0.03 && p.1 > 240.0);
        assert!(peak(0.25) && peak(0.75), "both rings kept: {out:?}");
        assert!(out.windows(2).all(|w| w[0].0 < w[1].0), "in order: {out:?}");

        let short = Contour::Custom { name: "Short".into(), points: points[..16].to_vec(), corners: vec![false; 16] };
        assert_eq!(crv(&contour_value(&short)).len(), 16);
    }

    /// Inputs closer than `f32::EPSILON` but distinct are still a span: a point on the straight line
    /// between two such inputs isn't mistaken for a bend and kept in place of a genuine one.
    #[test]
    fn a_point_on_a_tiny_straight_span_is_not_a_bend() {
        let p = |input: f32, output: f32| CurvePoint { input, output };
        // Three distinct inputs a few ulps apart near 0.5, on a straight rise from 0 to 1.
        let (a, b) = (0.5_f32, f32::from_bits(0.5_f32.to_bits() + 2));
        let mid = f32::from_bits(0.5_f32.to_bits() + 1);
        let mut points = vec![p(0.0, 0.0), p(0.2, 0.0)];
        points.extend([p(a, 0.0), p(mid, 0.5), p(b, 1.0)]);
        // Fourteen gentle bends after it, each one a real (if small) peak.
        points.extend((0..14).map(|k| p(0.55 + k as f32 * 0.03, if k % 2 == 0 { 1.0 } else { 0.9 })));
        points.push(p(1.0, 1.0));
        let kept = psd_contour_points(&points);
        assert_eq!(kept.len(), 16);
        assert!(!kept.contains(&3), "the point on the tiny straight span was kept over a real bend: {kept:?}");
    }

    /// A block written from scratch carries the document's effects scale (resolution / 72); a
    /// file's own block keeps the scale it had.
    #[test]
    fn written_effects_carry_the_documents_scale() {
        let scale = |data: &[u8]| match VersionedDescriptor::parse_prefix(&data[4..]).unwrap().0.descriptor.get("Scl ") {
            Some(Value::UnitFloat { value, .. }) => *value,
            other => panic!("{other:?}"),
        };
        let items = vec![Effect::default_drop_shadow()];
        let (at150, _) = write_lfx2_onto_at(None, true, &items, 150.0);
        assert!((scale(&at150) - 208.333).abs() < 0.01, "{}", scale(&at150));
        let (at72, _) = write_lfx2_onto_at(None, true, &items, 72.0);
        assert!((scale(&at72) - 100.0).abs() < 1e-6);
        // A file's block keeps its scale, whatever the document's resolution now.
        let (kept, _) = write_lfx2_onto_at(Some(&at150), true, &items, 300.0);
        assert!((scale(&kept) - 208.333).abs() < 0.01, "{}", scale(&kept));
        // Hostile resolutions fall back to 100 %.
        for dpi in [0.0, -5.0, f64::NAN, f64::INFINITY] {
            assert!((scale(&write_lfx2_onto_at(None, true, &items, dpi).0) - 100.0).abs() < 1e-6, "{dpi}");
        }
    }

    fn sample_effects() -> Vec<Effect> {
        let g = Gradient {
            stops: vec![(0.0, Color::rgb(1.0, 0.0, 0.0)), (1.0, Color::rgb(0.0, 0.0, 1.0))],
            opacity_stops: vec![(0.0, 1.0), (1.0, 0.5)],
            ..Gradient::default()
        };
        vec![
            Effect::default_drop_shadow(),
            Effect::DropShadow(Shadow {
                distance: 9.0,
                spread: 0.25,
                contour: Contour::Custom {
                    name: "Cone".into(),
                    points: vec![CurvePoint { input: 0.0, output: 0.0 }, CurvePoint { input: 0.5, output: 1.0 }, CurvePoint { input: 1.0, output: 0.0 }],
                    corners: vec![false, true, false],
                },
                ..match Effect::default_drop_shadow() {
                    Effect::DropShadow(s) => s,
                    _ => unreachable!(),
                }
            }),
            Effect::InnerShadow(Shadow {
                knocks_out: false,
                ..match Effect::default_drop_shadow() {
                    Effect::DropShadow(s) => s,
                    _ => unreachable!(),
                }
            }),
            Effect::OuterGlow(Glow {
                common: FxCommon::new(BlendMode::Screen, 0.5),
                paint: FxPaint::Color(Color::rgb(1.0, 1.0, 0.0)),
                technique: GlowTechnique::Precise,
                spread: 0.1,
                size: 8.0,
                contour: Contour::Linear,
                anti_alias: true,
                range: 0.5,
                jitter: 0.0,
                noise: 0.0,
                source: GlowSource::Edge,
            }),
            Effect::InnerGlow(Glow {
                common: FxCommon::new(BlendMode::Screen, 0.75),
                paint: FxPaint::Gradient(g.clone()),
                technique: GlowTechnique::Softer,
                spread: 0.0,
                size: 5.0,
                contour: Contour::Linear,
                anti_alias: false,
                range: 0.5,
                jitter: 0.0,
                noise: 0.0,
                source: GlowSource::Center,
            }),
            Effect::Stroke(StrokeFx {
                common: FxCommon::new(BlendMode::Normal, 1.0),
                size: 3.0,
                position: StrokePosition::Inside,
                paint: FxPaint::Color(Color::rgb(0.0, 1.0, 0.0)),
            }),
            Effect::Stroke(StrokeFx {
                common: FxCommon::new(BlendMode::Multiply, 0.5),
                size: 6.0,
                position: StrokePosition::Center,
                paint: FxPaint::Gradient(g.clone()),
            }),
            Effect::ColorOverlay { common: FxCommon::new(BlendMode::Overlay, 0.5), color: Color::rgb(0.0, 0.0, 1.0) },
            Effect::GradientOverlay { common: FxCommon::new(BlendMode::Normal, 1.0), gradient: g, dither: true },
            Effect::PatternOverlay {
                common: FxCommon::new(BlendMode::Normal, 1.0),
                name: "Bubbles".into(),
                id: "abc".into(),
                scale: 1.0,
                angle: 15.0,
                link: false,
                phase: (4.0, 2.0),
            },
            Effect::Satin(Satin {
                common: FxCommon::new(BlendMode::Multiply, 0.5),
                color: Color::BLACK,
                angle: 19.0,
                distance: 11.0,
                size: 14.0,
                contour: Contour::Linear,
                anti_alias: true,
                invert: true,
            }),
            Effect::BevelEmboss(Bevel {
                enabled: true,
                style: BevelStyle::Emboss,
                technique: BevelTechnique::ChiselSoft,
                depth: 1.5,
                up: false,
                size: 7.0,
                soften: 2.0,
                angle: 45.0,
                altitude: 40.0,
                use_global_light: false,
                gloss_contour: Contour::Linear,
                gloss_anti_alias: true,
                highlight: FxCommon::new(BlendMode::Screen, 0.75),
                highlight_color: Color::WHITE,
                shadow: FxCommon::new(BlendMode::Multiply, 0.6),
                shadow_color: Color::BLACK,
                contour: Some(BevelContour { contour: Contour::Linear, range: 0.7, anti_alias: true }),
                texture: Some(BevelTexture { name: "Bubbles".into(), id: "abc".into(), scale: 0.5, depth: -2.0, invert: true, link: false, phase: (3.0, 1.0) }),
            }),
        ]
    }

    fn approx(a: &[Effect], b: &[Effect]) {
        // Colours go through 0..255 doubles; compare via re-serialization.
        assert_eq!(a.len(), b.len());
        assert_eq!(write_lfx2(true, a), write_lfx2(true, b));
    }

    #[test]
    fn lfx2_roundtrip_all_kinds() {
        let fx = sample_effects();
        let data = write_lfx2(false, &fx);
        let (master, back) = parse_lfx2(&data).unwrap();
        assert!(!master);
        approx(&fx, &back);
        // Gloss Contour › Anti-aliased (`antialiasGloss`) survives.
        assert!(back.iter().any(|e| matches!(e, Effect::BevelEmboss(b) if b.gloss_anti_alias)));
        // Kind order is canonical: shadows first ... strokes last.
        assert!(matches!(back[0], Effect::DropShadow(_)));
        assert!(matches!(back.last().unwrap(), Effect::Stroke(_)));
    }

    #[test]
    fn multi_instances_use_multi_keys() {
        let fx = sample_effects();
        let data = write_lfx2(true, &fx);
        let (vd, _) = VersionedDescriptor::parse_prefix(&data[4..]).unwrap();
        assert!(vd.descriptor.get("dropShadowMulti").is_some());
        assert!(vd.descriptor.get("frameFXMulti").is_some());
        assert!(vd.descriptor.get("SoFi").is_some());
    }

    #[test]
    fn blend_names_are_unique() {
        let mut s = std::collections::HashSet::new();
        for (_, n) in BLEND_NAMES {
            assert!(s.insert(n));
        }
    }

    /// A Gradient Overlay `lfx2` laid out as Reference writes it: one black colour, opacity
    /// 100 → 0 % with the opacity midpoint `mid` (%), Smoothness `intr` (0..4096) and the method
    /// enum `gs99` (Reference 25.1's key in layer effects; codes `Gcls`, `Lnr `, `Perc`).
    fn overlay_lfx2(mid: i32, intr: f64, method: &str) -> Vec<u8> {
        let black = || Descriptor::new("RGBC").with("Rd  ", Value::Double(0.0)).with("Grn ", Value::Double(0.0)).with("Bl  ", Value::Double(0.0));
        let clr = |loc: i32| {
            Value::Descriptor(
                Descriptor::new("Clrt")
                    .with("Clr ", Value::Descriptor(black()))
                    .with("Type", Value::Enumerated { type_id: Id::new("Clry"), value: Id::new("UsrS") })
                    .with("Lctn", Value::Integer(loc))
                    .with("Mdpn", Value::Integer(50)),
            )
        };
        let trn = |loc: i32, op: f32, mid: i32| {
            Value::Descriptor(Descriptor::new("TrnS").with("Opct", unit(b"#Prc", op)).with("Lctn", Value::Integer(loc)).with("Mdpn", Value::Integer(mid)))
        };
        let grad = Descriptor::new("Grdn")
            .with("GrdF", Value::Enumerated { type_id: Id::new("GrdF"), value: Id::new("CstS") })
            .with("Intr", Value::Double(intr))
            .with("Clrs", Value::List(vec![clr(0), clr(4096)]))
            .with("Trns", Value::List(vec![trn(0, 100.0, 50), trn(4096, 0.0, mid)]));
        let fx = Descriptor::new("GrFl")
            .with("enab", Value::Boolean(true))
            .with("Grad", Value::Descriptor(grad))
            .with("Angl", unit(b"#Ang", 0.0))
            .with("gs99", Value::Enumerated { type_id: Id::new("gradientInterpolationMethodType"), value: Id::new(method) });
        let d = Descriptor::new("null").with("masterFXSwitch", Value::Boolean(true)).with("GrFl", Value::Descriptor(fx));
        let mut data = 0u32.to_be_bytes().to_vec();
        data.extend(VersionedDescriptor { version: 16, descriptor: d }.to_bytes());
        data
    }

    fn overlay_opacity(data: &[u8]) -> Vec<(f32, f32)> {
        let (_, fx) = parse_lfx2(data).unwrap();
        let Some(Effect::GradientOverlay { gradient, .. }) = fx.first() else { panic!("overlay: {fx:?}") };
        gradient.opacity_stops.clone()
    }

    fn opacity_at(o: &[(f32, f32)], t: f32) -> f32 {
        let i = o.windows(2).position(|w| t >= w[0].0 && t <= w[1].0).unwrap();
        o[i].1 + (o[i + 1].1 - o[i].1) * (t - o[i].0) / (o[i + 1].0 - o[i].0)
    }

    /// The overlay's transparency follows Smoothness, the opacity midpoints and the method like a
    /// gradient fill layer's (Reference 25.1 renders both identically; the values are its
    /// composites at t = 0.25 and 0.5).
    #[test]
    fn gradient_overlay_opacity_follows_smoothness_midpoint_and_method() {
        let at = |mid, intr, method: &str, t| opacity_at(&overlay_opacity(&overlay_lfx2(mid, intr, method)), t);
        // Plain Classic stays the linear fade it is stored as.
        assert_eq!(overlay_opacity(&overlay_lfx2(50, 0.0, "Gcls")), vec![(0.0, 1.0), (1.0, 0.0)]);
        // Midpoint 25 %: half transparent a quarter of the way.
        assert!((at(25, 0.0, "Gcls", 0.25) - 0.5).abs() < 0.01);
        // Smoothness 100 % eases the ends (Reference: 0.796 at a quarter).
        assert!((at(50, 4096.0, "Gcls", 0.25) - 0.796).abs() < 0.015);
        // The method bends the transparency ramp too, not only the colours (Reference at the
        // middle: 0.631 Linear, 0.255 Perceptual).
        assert!((at(50, 0.0, "Lnr ", 0.5) - 0.631).abs() < 0.015);
        assert!((at(50, 0.0, "Perc", 0.5) - 0.255).abs() < 0.015);
        assert!((at(25, 4096.0, "Perc", 0.25) - 0.255).abs() < 0.015);
    }

    /// A regenerated `lfx2` carries the baked ramp as plain (Classic, Smoothness 0) stops, so it
    /// reads back unchanged: written fresh, or merged onto the file's block (whose Perceptual
    /// method is then written as Classic, or it would bend the baked ramp again on every open).
    #[test]
    fn baked_overlay_opacity_round_trips() {
        let data = overlay_lfx2(25, 4096.0, "Perc");
        let (master, fx) = parse_lfx2(&data).unwrap();
        let a = overlay_opacity(&data);
        assert!(a.len() > 2);
        let close = |b: &[(f32, f32)]| {
            assert_eq!(a.len(), b.len());
            for (x, y) in a.iter().zip(b) {
                assert!((x.0 - y.0).abs() < 1e-3 && (x.1 - y.1).abs() < 1e-4, "{x:?} vs {y:?}");
            }
        };
        close(&overlay_opacity(&write_lfx2(master, &fx)));
        // Edited (the colours), merged onto the file's own block.
        let mut edited = fx.clone();
        if let Some(Effect::GradientOverlay { gradient, .. }) = edited.first_mut() {
            gradient.stops = vec![(0.0, Color::rgb(1.0, 0.0, 0.0)), (1.0, Color::rgb(1.0, 0.0, 0.0))];
        }
        let (merged, _) = write_lfx2_onto(Some(&data), master, &edited);
        close(&overlay_opacity(&merged));
        let d = VersionedDescriptor::parse_prefix(&merged[4..]).unwrap().0.descriptor;
        assert_eq!(get_desc(&d, "GrFl").and_then(|g| enum_of(g, "gs99")), Some(&b"Gcls"[..]));
        // Unedited, the file's gradient and method stay.
        let (kept, _) = write_lfx2_onto(Some(&data), master, &fx);
        let d = VersionedDescriptor::parse_prefix(&kept[4..]).unwrap().0.descriptor;
        assert_eq!(get_desc(&d, "GrFl").and_then(|g| enum_of(g, "gs99")), Some(&b"Perc"[..]));
    }

    #[test]
    fn layer_record_blend_keys_parse() {
        for (key, mode) in [
            (b"vLit", BlendMode::VividLight),
            (b"lLit", BlendMode::LinearLight),
            (b"pLit", BlendMode::PinLight),
            (b"hMix", BlendMode::HardMix),
            (b"lbrn", BlendMode::LinearBurn),
            (b"lddg", BlendMode::LinearDodge),
            (b"dkCl", BlendMode::DarkerColor),
            (b"lgCl", BlendMode::LighterColor),
            (b"fsub", BlendMode::Subtract),
            (b"fdiv", BlendMode::Divide),
        ] {
            assert_eq!(blend_from_id(key), Some(mode), "{}", String::from_utf8_lossy(key));
        }
        assert_eq!(blend_from_id(b"zzzz"), None);
        // A Color Overlay in Vivid Light stored as `vLit`, not left as Normal.
        let key = |v: &str| Value::Enumerated { type_id: Id::new("BlnM"), value: Id::new(v) };
        let d = Descriptor::new("null")
            .with("masterFXSwitch", Value::Boolean(true))
            .with("SoFi", Value::Descriptor(Descriptor::new("SoFi").with("enab", Value::Boolean(true)).with("Md  ", key("vLit"))));
        let mut data = 0u32.to_be_bytes().to_vec();
        data.extend(VersionedDescriptor { version: 16, descriptor: d }.to_bytes());
        let (_, fx) = parse_lfx2(&data).unwrap();
        let modes: Vec<BlendMode> = fx.iter().filter_map(|e| if let Effect::ColorOverlay { common, .. } = e { Some(common.blend) } else { None }).collect();
        assert_eq!(modes, vec![BlendMode::VividLight]);
    }

    #[test]
    fn long_string_blend_ids_parse() {
        // Some writers store effect modes as long string IDs (`BlnM` `multiply`, `colorDodge`)
        // instead of the four-character codes (psd-tools effects/blend-modes.psd).
        let long = |v: &str| Value::Enumerated { type_id: Id::new("BlnM"), value: Id::new(v) };
        let d = Descriptor::new("null")
            .with("masterFXSwitch", Value::Boolean(true))
            .with("DrSh", Value::Descriptor(Descriptor::new("DrSh").with("enab", Value::Boolean(true)).with("Md  ", long("linearBurn"))))
            .with("ebbl", Value::Descriptor(Descriptor::new("ebbl").with("hglM", long("colorDodge")).with("sdwM", long("colorBurn"))))
            .with("SoFi", Value::Descriptor(Descriptor::new("SoFi").with("Md  ", long("hue"))));
        let mut data = 0u32.to_be_bytes().to_vec();
        data.extend(VersionedDescriptor { version: 16, descriptor: d }.to_bytes());
        let (_, fx) = parse_lfx2(&data).unwrap();
        let modes: Vec<BlendMode> = fx
            .iter()
            .flat_map(|e| match e {
                Effect::DropShadow(s) => vec![s.common.blend],
                Effect::BevelEmboss(b) => vec![b.highlight.blend, b.shadow.blend],
                Effect::ColorOverlay { common, .. } => vec![common.blend],
                _ => vec![],
            })
            .collect();
        assert_eq!(modes, vec![BlendMode::LinearBurn, BlendMode::ColorDodge, BlendMode::ColorBurn, BlendMode::Hue]);
        for (m, n) in BLEND_LONG_NAMES {
            assert_eq!(blend_from_id(n.as_bytes()), Some(m));
        }
    }

    #[test]
    fn garbage_is_none() {
        assert!(parse_lfx2(&[0, 0]).is_none());
        assert!(parse_lrfx(&[0, 0, 0, 1, b'X']).is_none());
    }

    #[test]
    fn lrfx_drop_shadow() {
        // version 0, count 1, 'dsdw' v2 record.
        let mut d = vec![0, 0, 0, 1];
        d.extend_from_slice(b"8BIMdsdw");
        let mut rec = Vec::new();
        rec.extend_from_slice(&2u32.to_be_bytes()); // version
        rec.extend_from_slice(&(7u32 << 16).to_be_bytes()); // blur (16.16)
        rec.extend_from_slice(&0u32.to_be_bytes()); // intensity
        rec.extend_from_slice(&(90u32 << 16).to_be_bytes()); // angle
        rec.extend_from_slice(&(4u32 << 16).to_be_bytes()); // distance
        rec.extend_from_slice(&[0, 0, 0xff, 0xff, 0, 0, 0, 0, 0, 0]); // color: red
        rec.extend_from_slice(b"8BIMmul ");
        rec.extend_from_slice(&[1, 1, 128]);
        rec.extend_from_slice(&[0; 10]);
        d.extend_from_slice(&(rec.len() as u32).to_be_bytes());
        d.extend(rec);
        let (_, fx) = parse_lrfx(&d).unwrap();
        match &fx[0] {
            Effect::DropShadow(s) => {
                assert_eq!(s.size, 7.0);
                assert_eq!(s.distance, 4.0);
                assert_eq!(s.common.blend, BlendMode::Multiply);
                assert!((s.common.opacity - 128.0 / 255.0).abs() < 1e-6);
                assert_eq!(s.color.c[0], 1.0);
            }
            other => panic!("{other:?}"),
        }
    }

    /// lfx2 data as current Photoshop writes it: a live drop shadow carrying a key we don't model,
    /// placeholders for unused kinds, an effects scale and an effect count.
    fn photoshop_style_lfx2() -> Vec<u8> {
        let placeholder = |class: &str| Descriptor::new(class).with("enab", Value::Boolean(false)).with("present", Value::Boolean(false));
        let (_, shadow) = write_one(&Effect::default_drop_shadow());
        let d = Descriptor::new("null")
            .with("Scl ", unit(b"#Prc", 50.0))
            .with("masterFXSwitch", Value::Boolean(true))
            .with("DrSh", Value::Descriptor(shadow.with("unmodelled", Value::Boolean(true))))
            .with("OrGl", Value::Descriptor(placeholder("OrGl")))
            .with("frameFXMulti", Value::List(vec![Value::Descriptor(placeholder("FrFX"))]))
            .with("numModifyingFX", Value::Integer(1));
        lfx2_data(d)
    }

    fn stroke() -> Effect {
        Effect::Stroke(StrokeFx {
            common: FxCommon::new(BlendMode::Normal, 1.0),
            size: 3.0,
            position: StrokePosition::Center,
            paint: FxPaint::Color(Color::rgb(0.0, 1.0, 0.0)),
        })
    }

    #[test]
    fn placeholders_are_not_effects() {
        let (_, fx) = parse_lfx2(&photoshop_style_lfx2()).unwrap();
        assert_eq!(fx.len(), 1);
        assert!(matches!(fx[0], Effect::DropShadow(_)));
    }

    #[test]
    fn rewrite_merges_onto_the_original() {
        let src = photoshop_style_lfx2();
        let (_, mut fx) = parse_lfx2(&src).unwrap();
        fx.push(stroke());
        let (data, multi) = write_lfx2_onto(Some(&src), true, &fx);
        assert!(!multi);
        let d = VersionedDescriptor::parse_prefix(&data[4..]).unwrap().0.descriptor;
        // Unmodelled values survive: the effects scale and the shadow's extra key.
        assert_eq!(d.get("Scl "), Some(&unit(b"#Prc", 50.0)));
        assert_eq!(get_desc(&d, "DrSh").unwrap().get("unmodelled"), Some(&Value::Boolean(true)));
        // The new stroke replaces its placeholder in place; the outer glow stays one.
        let keys: Vec<_> = d.items.iter().map(|(k, _)| String::from_utf8_lossy(k.as_bytes()).into_owned()).collect();
        assert_eq!(keys, ["Scl ", "masterFXSwitch", "DrSh", "OrGl", "FrFX", "numModifyingFX"]);
        assert_eq!(get_desc(&d, "FrFX").unwrap().get("present"), Some(&Value::Boolean(true)));
        assert!(placeholder(get_desc(&d, "OrGl").unwrap()));
        assert_eq!(d.get("numModifyingFX"), Some(&Value::Integer(2)));
        assert_eq!(parse_lfx2(&data).unwrap().1, fx);

        // Removing the shadow leaves a placeholder that keeps its settings.
        fx.remove(0);
        let (data, _) = write_lfx2_onto(Some(&src), true, &fx);
        let d = VersionedDescriptor::parse_prefix(&data[4..]).unwrap().0.descriptor;
        let shadow = get_desc(&d, "DrSh").unwrap();
        assert!(placeholder(shadow) && shadow.get("unmodelled").is_some());
        assert_eq!(d.get("numModifyingFX"), Some(&Value::Integer(1)));
        assert_eq!(parse_lfx2(&data).unwrap().1, fx);
    }

    /// Smoothness shapes an overlay's opacity stops as it does its colours (psd-tools
    /// clipping-mask2.psd: 40 % of pixels off with a linear ramp), and a glow's (temp kit 15).
    #[test]
    fn gradient_opacity_follows_smoothness_in_overlays_and_glows() {
        let enumv = |t: &str, v: &str| Value::Enumerated { type_id: Id::new(t), value: Id::new(v) };
        let rgb = |v: f64| Value::Descriptor(Descriptor::new("RGBC").with("Rd  ", Value::Double(v)).with("Grn ", Value::Double(v)).with("Bl  ", Value::Double(v)));
        let stop = |l: i32| {
            Value::Descriptor(Descriptor::new("Clrt").with("Clr ", rgb(255.0)).with("Type", enumv("Clry", "UsrS")).with("Lctn", Value::Integer(l)).with("Mdpn", Value::Integer(50)))
        };
        let alpha = |l: i32, o: f64| Value::Descriptor(Descriptor::new("TrnS").with("Opct", Value::UnitFloat { unit: *b"#Prc", value: o }).with("Lctn", Value::Integer(l)).with("Mdpn", Value::Integer(50)));
        let grad = Descriptor::new("Grdn")
            .with("GrdF", enumv("GrdF", "CstS"))
            .with("Intr", Value::Double(4096.0))
            .with("Clrs", Value::List(vec![stop(0), stop(4096)]))
            .with("Trns", Value::List(vec![alpha(0, 100.0), alpha(4096, 0.0)]));
        let at = |stops: &[(f32, f32)], t: f32| {
            let i = stops.iter().position(|s| s.0 >= t).unwrap();
            stops[i].1
        };
        let overlay = gradient_from(&Descriptor::new("GrFl").with("Grad", Value::Descriptor(grad.clone())));
        // Eased: past the linear ramp before the middle, below it after.
        assert!(at(&overlay.opacity_stops, 0.25) > 0.76 && at(&overlay.opacity_stops, 0.75) < 0.24, "{:?}", overlay.opacity_stops);
        let glow = parse_glow(&Descriptor::new("OrGl").with("Grad", Value::Descriptor(grad)), false);
        let FxPaint::Gradient(g) = glow.paint else { panic!("{:?}", glow.paint) };
        assert_eq!(g.opacity_stops, overlay.opacity_stops);
    }

    /// A gradient baked to dense stops remembers how many colour stops it had (glows sample a
    /// two-stop gradient differently from a longer one); an unbaked one doesn't need to.
    #[test]
    fn baked_gradients_keep_their_colour_stop_count() {
        let enumv = |t: &str, v: &str| Value::Enumerated { type_id: Id::new(t), value: Id::new(v) };
        let rgb = |v: f64| Value::Descriptor(Descriptor::new("RGBC").with("Rd  ", Value::Double(v)).with("Grn ", Value::Double(v)).with("Bl  ", Value::Double(v)));
        let stop = |l: i32, v: f64| {
            Value::Descriptor(Descriptor::new("Clrt").with("Clr ", rgb(v)).with("Type", enumv("Clry", "UsrS")).with("Lctn", Value::Integer(l)).with("Mdpn", Value::Integer(50)))
        };
        let grad = |smooth: f64| {
            Descriptor::new("Grdn").with("GrdF", enumv("GrdF", "CstS")).with("Intr", Value::Double(smooth)).with("Clrs", Value::List(vec![stop(0, 0.0), stop(4096, 255.0)]))
        };
        let parse = |smooth: f64| {
            let glow = parse_glow(&Descriptor::new("OrGl").with("Grad", Value::Descriptor(grad(smooth))), false);
            let FxPaint::Gradient(g) = glow.paint else { panic!("{:?}", glow.paint) };
            g
        };
        let smooth = parse(4096.0);
        assert!(smooth.stops.len() > 2 && smooth.color_stops == 2 && smooth.color_stop_count() == 2, "{} {}", smooth.stops.len(), smooth.color_stops);
        let plain = parse(0.0);
        assert_eq!((plain.stops.len(), plain.color_stops, plain.color_stop_count()), (2, 0, 2));
    }

    /// A gradient overlay whose opacity is edited keeps its gradient definition verbatim (gray
    /// stops and keys the model doesn't hold); its stops used to be rewritten as RGB.
    #[test]
    fn edited_gradient_overlay_keeps_its_gradient() {
        let enumv = |t: &str, v: &str| Value::Enumerated { type_id: Id::new(t), value: Id::new(v) };
        let gray = |g: f64| Value::Descriptor(Descriptor::new("Grsc").with("Gry ", Value::Double(g)));
        let stop = |l: i32, c: Value| {
            Value::Descriptor(Descriptor::new("Clrt").with("Clr ", c).with("Type", enumv("Clry", "UsrS")).with("Lctn", Value::Integer(l)).with("Mdpn", Value::Integer(50)))
        };
        let grad = Descriptor::new("Grdn")
            .with("Nm  ", Value::Text(UnicodeString::new_nul("Grays")))
            .with("GrdF", enumv("GrdF", "CstS"))
            .with("Intr", Value::Double(4096.0))
            .with("Clrs", Value::List(vec![stop(0, gray(0.0)), stop(4096, gray(100.0))]))
            .with("unmodelled", Value::Boolean(true));
        let (_, mut overlay) = write_one(&Effect::GradientOverlay { common: FxCommon::new(BlendMode::Normal, 1.0), gradient: Gradient::default(), dither: false });
        set_key(&mut overlay, "Grad", Value::Descriptor(grad.clone()));
        let src = lfx2_data(Descriptor::new("null").with("masterFXSwitch", Value::Boolean(true)).with("GrFl", Value::Descriptor(overlay)));
        let (_, mut fx) = parse_lfx2(&src).unwrap();
        if let Effect::GradientOverlay { common, .. } = &mut fx[0] {
            common.opacity = 0.4;
        }
        let (data, _) = write_lfx2_onto(Some(&src), true, &fx);
        let d = VersionedDescriptor::parse_prefix(&data[4..]).unwrap().0.descriptor;
        let gr = get_desc(&d, "GrFl").unwrap();
        assert_eq!(get_desc(gr, "Grad"), Some(&grad));
        assert_eq!(gr.get("Opct"), Some(&unit(b"#Prc", 40.0)));
    }

    #[test]
    fn several_of_a_kind_are_multi() {
        let fx = vec![Effect::default_drop_shadow(), Effect::default_drop_shadow()];
        let (data, multi) = write_lfx2_onto(None, true, &fx);
        assert!(multi);
        assert_eq!(parse_lfx2(&data).unwrap().1.len(), 2);
        // A template that doesn't parse is ignored.
        assert_eq!(write_lfx2_onto(Some(&[1, 2, 3]), true, &fx).0, data);
    }

    /// Every effect is written under the key its kind is listed with.
    #[test]
    fn kind_keys_match_the_written_keys() {
        for e in sample_effects() {
            assert_eq!(kind_key(&e), write_one(&e).0);
        }
    }

    /// The gradient of each effect kind, read from Reference's definition (smoothness, Smooth
    /// method) and written with no template, is that definition with its method again; edited, it
    /// is written as its stops.
    #[test]
    fn fresh_effect_gradients_are_written_as_their_definition() {
        let data = overlay_lfx2(30, 4096.0, "Smoo");
        let (_, fx) = parse_lfx2(&data).unwrap();
        let Some(Effect::GradientOverlay { gradient, .. }) = fx.first() else { panic!("{fx:?}") };
        let (Some(src), true) = (gradient.source.clone(), gradient.stops.len() > 2) else { panic!("baked, with its definition") };
        let paint = FxPaint::Gradient(gradient.clone());
        let glow = Glow {
            common: FxCommon::new(BlendMode::Screen, 0.75),
            paint: paint.clone(),
            technique: GlowTechnique::Softer,
            spread: 0.0,
            size: 5.0,
            contour: Contour::Linear,
            anti_alias: false,
            range: 0.5,
            jitter: 0.0,
            noise: 0.0,
            source: GlowSource::Edge,
        };
        let effects = [
            fx[0].clone(),
            Effect::OuterGlow(glow.clone()),
            Effect::InnerGlow(glow),
            Effect::Stroke(StrokeFx { common: FxCommon::new(BlendMode::Normal, 1.0), size: 3.0, position: StrokePosition::Outside, paint }),
        ];
        for e in &effects {
            let (key, d) = write_one(e);
            assert_eq!(get_desc(&d, "Grad"), Some(&crate::gradient_source::to_desc(&src)), "{key}");
            let method_key = if key == "GrFl" { "gs99" } else { "gradientsInterpolationMethod" };
            assert!(matches!(d.get(method_key), Some(Value::Enumerated { value, .. }) if value.is("Smoo")), "{key}");
        }
        // Edited stops: written as stops, without the method.
        let mut edited = fx[0].clone();
        if let Effect::GradientOverlay { gradient, .. } = &mut edited {
            gradient.stops[0].1 = Color::rgb(1.0, 0.0, 0.0);
        }
        let (_, d) = write_one(&edited);
        assert!(d.get("gs99").is_none());
        assert_ne!(get_desc(&d, "Grad"), Some(&crate::gradient_source::to_desc(&src)));
    }
}
