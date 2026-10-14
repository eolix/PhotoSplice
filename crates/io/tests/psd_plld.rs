//! Placed layers that carry only a CS2 `PlLd` record (no `SoLd`): source id, placement and warp
//! on import, and the record kept verbatim while unedited. Ported from
//! storytold/photocraft@0002d861.

use std::sync::Arc;

use photosuite_color::{ColorMode, SampleType};
use photosuite_doc::{Document, Layer, LayerContent, SmartObject, SmartSource};
use photosuite_geom::{Affine, Size};
use photosuite_io::{ExportOptions, export, import, smart_map};
use photosuite_psd::{PsdFile, TaggedBlock};
use photosuite_raster::Surface;

/// A document with one smart object embedding a 2 × 2 red PNG.
fn fixture() -> Document {
    let mut source = Document::new("contents", Size::new(2, 2), ColorMode::Rgb, SampleType::U8);
    let mut pixels = Surface::new(source.pixel_format());
    pixels.fill_rect(source.bounds(), &[1.0, 0.0, 0.0, 1.0]);
    source.layers.push(Layer::new("red", LayerContent::Raster(pixels.clone())));
    let bytes = export(&source, "png", &ExportOptions::default()).unwrap().bytes;
    let mut doc = Document::new("instances", Size::new(12, 4), ColorMode::Rgb, SampleType::U8);
    let smart = SmartObject::new(SmartSource::Embedded { file_name: "contents.png".into(), bytes: Arc::new(bytes) }, Affine::IDENTITY, Some(pixels));
    doc.layers.push(Layer::new("original", LayerContent::Smart(smart)));
    doc
}

fn round_trip(doc: &Document, extension: &str) -> (PsdFile, Document) {
    let out = export(doc, extension, &ExportOptions::default()).unwrap();
    assert!(!out.warnings.iter().any(|w| w.contains("smart object written as pixels")), "{:?}", out.warnings);
    let file = PsdFile::from_bytes(&out.bytes).unwrap();
    let back = import(extension, &out.bytes).unwrap().document;
    (file, back)
}

/// A `PlLd`-only placed layer (CS2) whose embedded file is missing still gets its
/// source id and placement (at an assumed 100 % scale); an unreadable `PlLd` is reported.
#[test]
fn plld_only_layers_without_their_file_or_with_a_broken_record() {
    let (mut file, _) = round_trip(&fixture(), "psd");
    file.global_blocks.retain(|b| !matches!(&b.key, b"lnk2" | b"lnk3" | b"lnkD"));
    let rotated = Affine { m: [0.0, 1.0, -1.0, 0.0, 6.0, 1.0] };
    let spec =
        smart_map::PlacedSpec { idnt: "missing", placed: "", transform: rotated, perspective: None, size: (3.0, 2.0), dpi: 72.0, warp: None, filter_fx: None };
    let mut record = file.layers()[0].clone();
    record.blocks.retain(|b| !matches!(&b.key, b"SoLd" | b"SoLE" | b"PlLd"));
    let mut broken = record.clone();
    record.blocks.push(TaggedBlock::new(*b"PlLd", smart_map::plld_bytes(&spec, &[], &mut Vec::new())));
    broken.blocks.push(TaggedBlock::new(*b"PlLd", b"plcL\0\0\0\x03\x05ab\0\0".to_vec()));
    *file.layers_mut() = vec![record, broken];
    let (doc, warnings) = photosuite_io::psd_to_document(&file);
    let LayerContent::Smart(a) = &doc.layers[0].content else { panic!("PlLd layer is not a smart object") };
    assert_eq!(a.source, SmartSource::Linked { path: "missing".into() });
    for (x, y) in a.transform.m.iter().zip(rotated.m) {
        assert!((x - y).abs() < 1e-12, "{:?} vs {rotated:?}", a.transform);
    }
    let LayerContent::Smart(b) = &doc.layers[1].content else { panic!("broken PlLd layer is not a smart object") };
    assert_eq!(b.source, SmartSource::Linked { path: String::new() });
    assert_eq!(warnings.iter().filter(|w| w.contains("(PlLd) could not be read")).count(), 1, "{warnings:?}");
}

/// CS2 can warp a smart object and keeps the warp in `PlLd`, after the corners. A
/// `PlLd`-only layer imports with that warp, so re-rendering it from its now-available source
/// (Free Transform, Image Size, mode changes) keeps it warped. A warp that can't be read leaves
/// the contents unavailable, so the warped pixels stay the rendering.
#[test]
fn plld_only_layers_keep_their_warp() {
    use photosuite_geom::warp::{Warp, WarpStyle};
    let (mut file, _) = round_trip(&fixture(), "psd");
    let bulge = Warp::preset(WarpStyle::Bulge, 35.0, [0.0, 0.0, 2.0, 2.0]);
    let spec = |warp| smart_map::PlacedSpec {
        idnt: "missing",
        placed: "",
        transform: Affine { m: [2.0, 0.0, 0.0, 2.0, 1.0, 0.0] },
        perspective: None,
        size: (2.0, 2.0),
        dpi: 72.0,
        warp,
        filter_fx: None,
    };
    let mut record = file.layers()[0].clone();
    record.blocks.retain(|b| !matches!(&b.key, b"SoLd" | b"SoLE" | b"PlLd"));
    let (mut warped, mut flat, mut broken) = (record.clone(), record.clone(), record);
    warped.blocks.push(TaggedBlock::new(*b"PlLd", smart_map::plld_bytes(&spec(Some(&bulge)), &[], &mut Vec::new())));
    flat.blocks.push(TaggedBlock::new(*b"PlLd", smart_map::plld_bytes(&spec(None), &[], &mut Vec::new())));
    // The warp descriptor cut short: a warp is stored but can't be read.
    let mut cut = smart_map::plld_bytes(&spec(Some(&bulge)), &[], &mut Vec::new());
    cut.truncate(cut.len() - 40);
    broken.blocks.push(TaggedBlock::new(*b"PlLd", cut));
    *file.layers_mut() = vec![warped, flat, broken];
    let (doc, warnings) = photosuite_io::psd_to_document(&file);
    let sm = |i: usize| match &doc.layers[i].content {
        LayerContent::Smart(s) => s,
        _ => panic!("layer {i} is not a smart object"),
    };
    assert_eq!(sm(0).warp, Some(bulge));
    assert_eq!(sm(0).source, SmartSource::Linked { path: "missing".into() });
    assert_eq!((sm(1).warp.as_ref(), &sm(1).source), (None, &SmartSource::Linked { path: "missing".into() }));
    assert_eq!(sm(2).source, SmartSource::Linked { path: String::new() }, "unreadable warp: contents unavailable");
    assert!(sm(2).cache.is_some());
    assert_eq!(warnings.iter().filter(|w| w.contains("warp (PlLd) could not be read")).count(), 1, "{warnings:?}");
}

/// A linked smart object with both `SoLE` and `PlLd` is read from `SoLE`: `PlLd` alone is
/// the CS2 record.
#[test]
fn sole_wins_over_plld() {
    let (mut file, _) = round_trip(&fixture(), "psd");
    let spec = smart_map::PlacedSpec {
        idnt: "plld-only",
        placed: "",
        transform: Affine::IDENTITY,
        perspective: None,
        size: (2.0, 2.0),
        dpi: 72.0,
        warp: None,
        filter_fx: None,
    };
    let mut record = file.layers()[0].clone();
    let sold = record.block(b"SoLd").expect("the fixture has SoLd").data.clone();
    let (id, _) = photosuite_io::blocks::parse_smart(b"SoLd", &sold);
    assert!(!id.is_empty() && id != "plld-only");
    record.blocks.retain(|b| !matches!(&b.key, b"SoLd" | b"SoLE" | b"PlLd"));
    record.blocks.push(TaggedBlock::new(*b"PlLd", smart_map::plld_bytes(&spec, &[], &mut Vec::new())));
    record.blocks.push(TaggedBlock::new(*b"SoLE", sold.clone()));
    *file.layers_mut() = vec![record];
    let (doc, _) = photosuite_io::psd_to_document(&file);
    let LayerContent::Smart(sm) = &doc.layers[0].content else { panic!("not a smart object") };
    assert_eq!(sm.source, SmartSource::Linked { path: id });
    assert_eq!(sm.psd_raw.as_deref(), Some(&sold));
}

/// A CS2 `PlLd`-only layer with a custom warp whose control points spill past its source frames
/// that control-point box with its corners, as `SoLd`'s `Trnf` does: it imports with the
/// same placement as the equivalent `SoLd` layer. Unedited, it saves its `PlLd` record verbatim
/// (no `SoLd` added) and reopens in the same place.
#[test]
fn plld_only_spilled_custom_warp_places_like_sold_and_round_trips_verbatim() {
    use photosuite_geom::warp::{BezierMesh, Warp};
    let (mut file, _) = round_trip(&fixture(), "psd");
    let mut record = file.layers()[0].clone();
    let (id, _) = photosuite_io::blocks::parse_smart(b"SoLd", &record.block(b"SoLd").expect("the fixture has SoLd").data);
    assert!(!id.is_empty());
    // The embedded source is 2 × 2; the mesh reaches from (-1, -0.5) to (3, 2.5).
    let bounds = [0.0, 0.0, 2.0, 2.0];
    let mut mesh = BezierMesh::identity(bounds, 1, 1);
    mesh.points[0] = [-1.0, -0.5];
    mesh.points[15] = [3.0, 2.5];
    let warp = Warp::custom(mesh, bounds);
    let t = Affine { m: [1.5, 0.0, 0.0, 1.5, 4.0, 1.0] };
    let spec =
        smart_map::PlacedSpec { idnt: &id, placed: "p", transform: t, perspective: None, size: (2.0, 2.0), dpi: 72.0, warp: Some(&warp), filter_fx: None };
    record.blocks.retain(|b| !matches!(&b.key, b"SoLd" | b"SoLE" | b"PlLd"));
    let (mut with_sold, mut cs2) = (record.clone(), record);
    with_sold.blocks.push(TaggedBlock::new(*b"SoLd", smart_map::sold_bytes(None, &spec, &mut Vec::new())));
    let plld = smart_map::plld_bytes(&spec, &[], &mut Vec::new());
    cs2.blocks.push(TaggedBlock::new(*b"PlLd", plld.clone()));
    *file.layers_mut() = vec![with_sold, cs2];

    let (doc, warnings) = photosuite_io::psd_to_document(&file);
    assert!(!warnings.iter().any(|w| w.contains("PlLd")), "{warnings:?}");
    let sm = |d: &Document, i: usize| match &d.layers[i].content {
        LayerContent::Smart(s) => s.clone(),
        _ => panic!("layer {i} is not a smart object"),
    };
    let close = |a: Affine, b: Affine| a.m.iter().zip(b.m).all(|(x, y)| (x - y).abs() < 1e-9);
    let (a, b) = (sm(&doc, 0), sm(&doc, 1));
    assert!(close(a.transform, t), "SoLd: {:?} vs {t:?}", a.transform);
    assert!(close(b.transform, a.transform), "PlLd {:?} vs SoLd {:?}", b.transform, a.transform);
    assert_eq!((&b.source, &b.warp, b.perspective), (&a.source, &a.warp, a.perspective));
    assert_eq!(b.warp.as_ref(), Some(&warp));
    assert_eq!(b.source, SmartSource::Linked { path: id.clone() });

    // Unedited: the CS2 record is written back as it was, with no SoLd, and reopens in place.
    let out = export(&doc, "psd", &ExportOptions::default()).unwrap();
    let saved = PsdFile::from_bytes(&out.bytes).unwrap();
    let rec = &saved.layers()[1];
    assert_eq!(rec.block(b"PlLd").map(|b| &b.data), Some(&plld));
    assert!(rec.block(b"SoLd").is_none(), "an unedited PlLd-only layer gained a SoLd");
    let again = import("psd", &out.bytes).unwrap().document;
    let c = sm(&again, 1);
    assert!(close(c.transform, t), "{:?} vs {t:?}", c.transform);
    assert_eq!(c.warp.as_ref(), Some(&warp));
}
