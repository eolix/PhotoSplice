//! Gradients written fresh (no preserved block to keep) are written as the definition Reference
//! stored, not as the stops the model bakes from it: re-baked, Reference's smoothness, midpoints,
//! interpolation method and noise generator render differently (round-trip kit 8: noise fills
//! 17–41/255 off, a smooth Outer Glow gradient 8/255, Smooth-method stroke gradients 10/255).
//! Edited stops are still written as stops.

use photosplice_color::Color;
use photosplice_doc::{Adjustment, Document, Effect, Fill, FxPaint, Gradient, Layer, LayerContent};
use photosplice_io::{ExportOptions, export, import};
use photosplice_psd::PsdFile;
use photosplice_psd::descriptor::{Descriptor, Value, VersionedDescriptor};

/// Drops every block the writer would keep, so it generates all of them (round-trip kit regen).
fn clear_raw(layers: &mut [Layer]) {
    for l in layers {
        l.effects.psd_raw = None;
        l.psd_blocks.clear();
        match &mut l.content {
            LayerContent::Text(t) => t.psd_raw = None,
            LayerContent::Smart(s) => s.psd_raw = None,
            LayerContent::Shape(s) => s.psd_raw = None,
            LayerContent::Group(g) => {
                g.divider_blocks.clear();
                clear_raw(&mut g.children);
            }
            _ => {}
        }
    }
}

fn regen(doc: &Document) -> Vec<u8> {
    let mut d = doc.clone();
    clear_raw(&mut d.layers);
    for p in &mut d.paths {
        p.psd_raw = None;
    }
    export(&d, "x.psd", &ExportOptions::default()).expect("export").bytes
}

/// One gradient definition of a file: where it is, then its `Grdn` descriptor with the
/// interpolation method key beside it, or a Gradient Map's `grdm` block.
#[cfg(feature = "corpus")]
#[derive(Debug)]
enum Def {
    Desc(Descriptor, Option<(String, Value)>),
    Grdm(Vec<u8>),
}

fn method_key(d: &Descriptor) -> Option<(String, Value)> {
    d.items
        .iter()
        .find(|(k, _)| k.is("gradientsInterpolationMethod") || k.is("gs99"))
        .map(|(k, v)| (String::from_utf8_lossy(k.as_bytes()).into_owned(), v.clone()))
}

fn sub<'a>(d: &'a Descriptor, key: &str) -> Option<&'a Descriptor> {
    match d.get(key)? {
        Value::Descriptor(x) | Value::GlobalObject(x) => Some(x),
        _ => None,
    }
}

/// The gradient definitions of a PSD, in layer order (`lmfx` over `lfx2`, as a read takes them;
/// effect placeholders left out).
#[cfg(feature = "corpus")]
fn defs(bytes: &[u8]) -> Vec<(String, Def)> {
    let f = PsdFile::from_bytes(bytes).expect("parse");
    let mut out = Vec::new();
    for (i, l) in f.layers().iter().enumerate() {
        let desc =
            |key: &[u8; 4], skip: usize| l.block(key).and_then(|b| VersionedDescriptor::parse_prefix(b.data.get(skip..)?).ok()).map(|(v, _)| v.descriptor);
        if let Some(d) = desc(b"GdFl", 0)
            && let Some(g) = sub(&d, "Grad")
        {
            out.push((format!("layer {i} fill"), Def::Desc(g.clone(), method_key(&d))));
        }
        if let Some(d) = desc(b"vscg", 4)
            && let Some(g) = sub(&d, "Grad")
        {
            // A shape's fill (an older file's shape is a fill layer with a vector mask).
            out.push((format!("layer {i} fill"), Def::Desc(g.clone(), method_key(&d))));
        }
        if let Some(d) = desc(b"vstk", 0).as_ref().and_then(|d| sub(d, "strokeStyleContent")).filter(|c| c.get("Grad").is_some())
            && let Some(g) = sub(d, "Grad")
        {
            out.push((format!("layer {i} vstk"), Def::Desc(g.clone(), method_key(d))));
        }
        if let Some(fx) = desc(b"lmfx", 4).or_else(|| desc(b"lfx2", 4)) {
            for (single, multi) in [("OrGl", ""), ("IrGl", ""), ("GrFl", "gradientFillMulti"), ("FrFX", "frameFXMulti")] {
                let items: Vec<&Descriptor> = match fx.get(multi) {
                    Some(Value::List(xs)) if !multi.is_empty() => xs.iter().filter_map(|x| if let Value::Descriptor(x) = x { Some(x) } else { None }).collect(),
                    _ => sub(&fx, single).into_iter().collect(),
                };
                let live = items.into_iter().filter(|e| !matches!(e.get("present"), Some(Value::Boolean(false))));
                for (k, e) in live.enumerate() {
                    if let Some(g) =
                        sub(e, "Grad").filter(|_| single != "FrFX" || matches!(e.get("PntT"), Some(Value::Enumerated { value, .. }) if value.is("GrFl")))
                    {
                        out.push((format!("layer {i} {single}#{k}"), Def::Desc(g.clone(), method_key(e))));
                    }
                }
            }
        }
        if let Some(b) = l.block(b"grdm") {
            out.push((format!("layer {i} grdm"), Def::Grdm(b.data.clone())));
        }
    }
    out
}

/// Whether `a` and `b` are the same descriptor value: the same keys in the same order, colours
/// the same colour, numbers within float noise, texts equal; `Err` says where they differ.
fn same(a: &Value, b: &Value, at: &str) -> Result<(), String> {
    let num = |v: &Value| match v {
        Value::Double(d) => Some(*d),
        Value::UnitFloat { value, .. } => Some(*value),
        Value::Integer(i) => Some(f64::from(*i)),
        _ => None,
    };
    match (a, b) {
        (Value::Descriptor(x), Value::Descriptor(y)) => same_desc(x, y, at),
        (Value::List(x), Value::List(y)) if x.len() == y.len() => x.iter().zip(y).enumerate().try_for_each(|(i, (p, q))| same(p, q, &format!("{at}[{i}]"))),
        (Value::Text(x), Value::Text(y)) if x.to_string_lossy() == y.to_string_lossy() => Ok(()),
        (Value::UnitFloat { unit: u, .. }, Value::UnitFloat { unit: v, .. }) if u != v => Err(format!("{at}: unit {u:?} vs {v:?}")),
        // Older Reference versions store a noise gradient's ranges as integers, newer as doubles.
        (Value::Integer(_) | Value::Double(_), Value::Integer(_) | Value::Double(_)) | (Value::UnitFloat { .. }, Value::UnitFloat { .. }) => {
            let (p, q) = (num(a).unwrap_or(0.0), num(b).unwrap_or(0.0));
            if (p - q).abs() <= 1e-3 { Ok(()) } else { Err(format!("{at}: {p} vs {q}")) }
        }
        (x, y) if x == y => Ok(()),
        (x, y) => Err(format!("{at}: {x:?} vs {y:?}")),
    }
}

fn same_desc(a: &Descriptor, b: &Descriptor, at: &str) -> Result<(), String> {
    // Colours: the same colour (Reference's own descriptors may spell RGB as floats, and an HSB
    // stop is kept as the RGB colour it is).
    let colour = |d: &Descriptor| ["RGBC", "CMYC", "LbCl", "Grsc", "HSBC"].iter().any(|c| d.class_id.is(c));
    if colour(a)
        && colour(b)
        && let (Some(x), Some(y)) = (photosplice_io::blocks::color_from_desc(a), photosplice_io::blocks::color_from_desc(b))
    {
        let close = x.mode == y.mode && x.c.iter().zip(&y.c).all(|(p, q)| (p - q).abs() < 1e-4);
        return if close { Ok(()) } else { Err(format!("{at}: colour {x:?} vs {y:?}")) };
    }
    if a.class_id != b.class_id {
        return Err(format!("{at}: class {:?} vs {:?}", a.class_id, b.class_id));
    }
    let keys = |d: &Descriptor| d.items.iter().map(|(k, _)| String::from_utf8_lossy(k.as_bytes()).into_owned()).collect::<Vec<_>>();
    if keys(a) != keys(b) {
        return Err(format!("{at}: keys {:?} vs {:?}", keys(a), keys(b)));
    }
    a.items.iter().zip(&b.items).try_for_each(|((k, x), (_, y))| same(x, y, &format!("{at}.{}", String::from_utf8_lossy(k.as_bytes()).trim())))
}

/// Every difference between the gradient definitions of `orig` and `written`.
#[cfg(feature = "corpus")]
fn differences(orig: &[u8], written: &[u8]) -> Vec<String> {
    let (a, b) = (defs(orig), defs(written));
    if a.len() != b.len() {
        return vec![format!(
            "{} definitions vs {}: {:?} vs {:?}",
            a.len(),
            b.len(),
            a.iter().map(|d| &d.0).collect::<Vec<_>>(),
            b.iter().map(|d| &d.0).collect::<Vec<_>>()
        )];
    }
    let mut out = Vec::new();
    for ((wa, da), (wb, db)) in a.iter().zip(&b) {
        let r = match (da, db) {
            _ if wa != wb => Err(format!("{wa} vs {wb}")),
            (Def::Desc(x, mx), Def::Desc(y, my)) => {
                same_desc(x, y, "Grad").and_then(|()| if mx == my { Ok(()) } else { Err(format!("method {mx:?} vs {my:?}")) })
            }
            (Def::Grdm(x), Def::Grdm(y)) => {
                // Bytes 2..4 are Reverse and Dither.
                if x == y {
                    Ok(())
                } else {
                    Err(format!("grdm {} bytes vs {}: first difference at {:?}", x.len(), y.len(), x.iter().zip(y).position(|(p, q)| p != q)))
                }
            }
            _ => Err("kind".into()),
        };
        if let Err(e) = r {
            out.push(format!("{wa}: {e}"));
        }
    }
    out
}

/// The gradients a document renders: every gradient fill's, effect's and Gradient Map's stops.
fn rendered(layers: &[Layer], out: &mut Vec<String>) {
    let g = |g: &Gradient| format!("{:?} {:?}", g.stops, g.opacity_stops);
    for l in layers {
        match &l.content {
            LayerContent::Fill(Fill::Gradient { stops, opacity_stops, midpoints, .. }) => out.push(format!("{stops:?} {opacity_stops:?} {midpoints:?}")),
            LayerContent::Adjustment(Adjustment::GradientMap { stops, ink_stops, .. }) => out.push(format!("{stops:?} {ink_stops:?}")),
            LayerContent::Shape(s) => {
                if let Some(Fill::Gradient { stops, opacity_stops, .. }) = &s.fill {
                    out.push(format!("{stops:?} {opacity_stops:?}"));
                }
                if let Some(photosplice_doc::ShapeStroke { paint: Fill::Gradient { stops, opacity_stops, .. }, .. }) = &s.stroke {
                    out.push(format!("{stops:?} {opacity_stops:?}"));
                }
            }
            LayerContent::Group(gr) => rendered(&gr.children, out),
            _ => {}
        }
        for e in &l.effects.items {
            match e {
                Effect::GradientOverlay { gradient, .. } => out.push(g(gradient)),
                Effect::Stroke(s) => {
                    if let FxPaint::Gradient(gr) = &s.paint {
                        out.push(g(gr));
                    }
                }
                Effect::OuterGlow(s) | Effect::InnerGlow(s) => {
                    if let FxPaint::Gradient(gr) = &s.paint {
                        out.push(g(gr));
                    }
                }
                _ => {}
            }
        }
    }
}

/// Parses `stops` as written by [`rendered`] back into numbers, to compare within float noise.
fn numbers(s: &str) -> Vec<f64> {
    s.split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == 'e')).filter_map(|t| t.parse().ok()).collect()
}

fn same_renders(a: &Document, b: &Document) -> Result<(), String> {
    let (mut x, mut y) = (Vec::new(), Vec::new());
    rendered(&a.layers, &mut x);
    rendered(&b.layers, &mut y);
    if x.len() != y.len() {
        return Err(format!("{} gradients vs {}", x.len(), y.len()));
    }
    for (i, (p, q)) in x.iter().zip(&y).enumerate() {
        let (p, q) = (numbers(p), numbers(q));
        if p.len() != q.len() || p.iter().zip(&q).any(|(u, v)| (u - v).abs() > 1e-4) {
            return Err(format!("gradient {i} renders differently ({} vs {} numbers)", p.len(), q.len()));
        }
    }
    Ok(())
}

/// A gradient fill layer as Reference writes one: CMYK and Lab stops, smoothness 75 %, the
/// Smooth method, a midpoint and opacity stops with their own midpoint.
fn reference_gradient_fill() -> Vec<u8> {
    use photosplice_psd::descriptor::{Id, UnicodeString};
    let stop = |c: Descriptor, at: i32, mid: i32| {
        Value::Descriptor(
            Descriptor::new("Clrt")
                .with("Clr ", Value::Descriptor(c))
                .with("Type", Value::Enumerated { type_id: Id::new("Clry"), value: Id::new("UsrS") })
                .with("Lctn", Value::Integer(at))
                .with("Mdpn", Value::Integer(mid)),
        )
    };
    let op = |o: f64, at: i32, mid: i32| {
        Value::Descriptor(
            Descriptor::new("TrnS")
                .with("Opct", Value::UnitFloat { unit: *b"#Prc", value: o })
                .with("Lctn", Value::Integer(at))
                .with("Mdpn", Value::Integer(mid)),
        )
    };
    let rgb = |r: f64, g: f64, b: f64| Descriptor::new("RGBC").with("Rd  ", Value::Double(r)).with("Grn ", Value::Double(g)).with("Bl  ", Value::Double(b));
    let lab = Descriptor::new("LbCl").with("Lmnc", Value::Double(50.0)).with("A   ", Value::Double(20.0)).with("B   ", Value::Double(-30.0));
    let grad = Descriptor::new("Grdn")
        .with("Nm  ", Value::Text(UnicodeString::new_nul("Oracle")))
        .with("GrdF", Value::Enumerated { type_id: Id::new("GrdF"), value: Id::new("CstS") })
        .with("Intr", Value::Double(3072.0))
        .with("Clrs", Value::List(vec![stop(rgb(255.0, 0.0, 0.0), 0, 50), stop(lab, 1500, 50), stop(rgb(0.0, 64.0, 255.0), 4096, 30)]))
        .with("Trns", Value::List(vec![op(100.0, 0, 50), op(20.0, 4096, 70)]));
    let d = Descriptor::new("null")
        .with("gradientsInterpolationMethod", Value::Enumerated { type_id: Id::new("gradientInterpolationMethodType"), value: Id::new("Smoo") })
        .with("Angl", Value::UnitFloat { unit: *b"#Ang", value: 30.0 })
        .with("Type", Value::Enumerated { type_id: Id::new("GrdT"), value: Id::new("Lnr ") })
        .with("Grad", Value::Descriptor(grad));
    VersionedDescriptor::new(d).to_bytes()
}

/// A document with one gradient fill layer read from `GdFl` data (the layer keeps no raw block).
fn doc_with_fill(data: &[u8]) -> Document {
    let fill = photosplice_io::blocks::parse_fill(b"GdFl", data).expect("fill");
    let mut doc = Document::new("g", photosplice_geom::Size::new(64, 16), photosplice_color::ColorMode::Rgb, photosplice_color::SampleType::U8);
    doc.layers.push(Layer::new("Gradient Fill", LayerContent::Fill(fill)));
    doc
}

/// A gradient fill read from Reference's definition and written fresh is written as that
/// definition (no file needed).
#[test]
fn fresh_gradient_fill_is_written_as_its_definition() {
    let data = reference_gradient_fill();
    let doc = doc_with_fill(&data);
    let LayerContent::Fill(Fill::Gradient { stops, source, .. }) = &doc.layers[0].content else { panic!() };
    assert!(source.is_some());
    assert!(stops.len() > 3, "the smoothness and method are baked into the rendered stops");
    let out = regen(&doc);
    let f = PsdFile::from_bytes(&out).expect("parse");
    let gdfl = f.layers().iter().find_map(|l| l.block(b"GdFl")).expect("GdFl");
    let (back, _) = VersionedDescriptor::parse_prefix(&gdfl.data).expect("descriptor");
    let (orig, _) = VersionedDescriptor::parse_prefix(&data).expect("descriptor");
    let g = |d: &Descriptor| sub(d, "Grad").cloned().expect("Grad");
    same_desc(&g(&orig.descriptor), &g(&back.descriptor), "Grad").expect("the definition");
    assert_eq!(method_key(&back.descriptor), method_key(&orig.descriptor));
    // Re-read, it renders as the original did.
    let again = import("x.psd", &out).expect("import").document;
    same_renders(&doc, &again).expect("renders");
}

/// An engine edit that changes the stops leaves the definition behind: the writer falls back to
/// the (edited) stops, Classic, as it always did.
#[test]
fn edited_gradient_fill_is_written_as_its_stops() {
    let mut doc = doc_with_fill(&reference_gradient_fill());
    if let LayerContent::Fill(Fill::Gradient { stops, source, .. }) = &mut doc.layers[0].content {
        assert!(source.is_some(), "the edit keeps the stale definition");
        stops[0].1 = Color::rgb(0.0, 1.0, 0.0);
    }
    let out = regen(&doc);
    let f = PsdFile::from_bytes(&out).expect("parse");
    let gdfl = f.layers().iter().find_map(|l| l.block(b"GdFl")).expect("GdFl");
    let (back, _) = VersionedDescriptor::parse_prefix(&gdfl.data).expect("descriptor");
    assert!(method_key(&back.descriptor).is_none(), "baked stops are Classic");
    let again = import("x.psd", &out).expect("import").document;
    let (LayerContent::Fill(Fill::Gradient { stops: a, .. }), LayerContent::Fill(Fill::Gradient { stops: b, .. })) =
        (&doc.layers[0].content, &again.layers[0].content)
    else {
        panic!()
    };
    assert_eq!(a.len(), b.len());
    assert!(a.iter().zip(b).all(|(p, q)| (p.0 - q.0).abs() < 1e-3 && p.1.c.iter().zip(&q.1.c).all(|(u, v)| (u - v).abs() < 1e-3)), "the edit is written");
    assert!(matches!(b.first(), Some((_, c)) if c.c[1] > 0.99), "the edited colour");
}

#[cfg(feature = "corpus")]
fn corpus(rel: &str) -> Vec<u8> {
    let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus").join(rel);
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}: run `cargo xtask corpus --all`", p.display()))
}

/// The corpus files whose gradients the regen round trip writes fresh: fills, glows, strokes,
/// overlays, noise gradients and Gradient Maps (with smoothness and methods).
#[cfg(feature = "corpus")]
const GRADIENT_FILES: &[&str] = &[
    "psd-tools/layer_params.psd",
    "psd-tools/effects/effect-enums.psd",
    "psd-tools/effect-stroke-gradient.psd",
    "psd-tools/gradient-styles.psd",
    "psd-tools/gradient-fill.psd",
    "psd-tools/layers/gradient-fill.psd",
    "psd-tools/layers/gradient-map.psd",
    "psd-tools/layers/gradient-map-v3-classic.psd",
    "psd-tools/layers/gradient-map-v3-linear.psd",
    "psd-tools/layers/gradient-map-v3-perceptual.psd",
    "psd-tools/gradients/noise-gradient-rgb.psd",
    "psd-tools/gradients/noise-gradient-cmyk.psd",
    "psd-tools/gradients/noise-gradient-grayscale.psd",
    "psd-tools/gradients/noise-gradient-lab.psd",
    "photoshop/effects/outer-glow-gradient.psd",
    "photoshop/adjustments/rgb8/gradient-map.psd",
    "photoshop/adjustments/rgb16/gradient-map.psd",
    "photoshop/adjustments/cmyk8/gradient-map.psd",
    "photoshop/adjustments/cmyk16/gradient-map.psd",
    "photoshop/adjustments/gray8/gradient-map.psd",
    "photoshop/adjustments/gray16/gradient-map.psd",
    "photoshop/adjustments/lab8/gradient-map.psd",
    "photoshop/adjustments/lab16/gradient-map.psd",
    "psd/ag-psd/read/gradient/src.psd",
    "psd/ag-psd/read/gradient-overlay-2/src.psd",
    "psd/ag-psd/read-write/gradient/src.psd",
    "psd/ag-psd/read-write/gradient-mode/src.psd",
    "psd/ag-psd/read/effects/src.psd",
    "psd/ag-psd/read-write/effects/src.psd",
];

/// Every gradient of the corpus files, written with no preserved block, is Reference's own
/// definition again, and the file re-reads to the same render.
#[cfg(feature = "corpus")]
#[test]
fn corpus_gradients_written_fresh_are_references_definitions() {
    let mut failures = Vec::new();
    let mut checked = 0;
    for rel in GRADIENT_FILES {
        let bytes = corpus(rel);
        let doc = import(rel, &bytes).expect("import").document;
        let out = regen(&doc);
        let n = defs(&bytes).len();
        checked += n;
        assert!(n > 0, "{rel}: no gradient");
        for d in differences(&bytes, &out) {
            failures.push(format!("{rel}: {d}"));
        }
        let again = import("x.psd", &out).expect("re-import").document;
        if let Err(e) = same_renders(&doc, &again) {
            failures.push(format!("{rel}: {e}"));
        }
    }
    println!("{checked} gradient definitions checked");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
