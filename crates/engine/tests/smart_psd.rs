//! Smart objects and smart filters made in PhotoSuite survive a PSD save and open (#215): the
//! layer comes back as a live smart object (not pixels) with the same nested document (embedded
//! in `lnk2` as a PSB), transform, filter list and filter mask, and re-renders like before.
//!
//! Set `PHOTOSUITE_SMART_PSD_OUT=<dir>` to keep the exported files (for opening in Photoshop).

use photosuite_doc::{Document, LayerContent, SmartObject, SmartSource};
use photosuite_engine::Session;
use photosuite_engine::smart_cmds::{decode_source, refresh_layer, source_bytes};
use photosuite_geom::Rect;
use serde_json::json;

const W: i32 = 96;
const H: i32 = 64;

fn session(depth: u64) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": W, "height": H, "depth": depth})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    s.edit("paint", |doc, active| {
        let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
        let fmt = surf.format();
        let r = Rect::new(8, 6, 80, 58);
        let mut data = Vec::new();
        for y in r.y0..r.y1 {
            for x in r.x0..r.x1 {
                let v = ((x * 7 + y * 13) % 23) as f32 / 22.0;
                data.extend(photosuite_raster::from_rgba(&fmt, [v, 1.0 - v, ((x * y) % 7) as f32 / 6.0, 1.0]));
            }
        }
        surf.write_region(r, &data);
        Ok(())
    })
    .unwrap();
    s
}

fn smart(doc: &Document) -> (u64, SmartObject) {
    doc.walk()
        .into_iter()
        .find_map(|(_, _, l)| match &l.content {
            LayerContent::Smart(sm) => Some((l.id.0, sm.clone())),
            _ => None,
        })
        .expect("a smart object")
}

fn max_diff(a: &[[f32; 4]], b: &[[f32; 4]]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).flat_map(|(p, q)| (0..4).map(move |c| (p[c] * p[3].max(0.0) - q[c] * q[3].max(0.0)).abs())).fold(0.0, f32::max)
}

/// Every block with inner structure re-parses strictly (`TaggedBlock::check_structure`).
fn structure_errors(bytes: &[u8]) -> Vec<String> {
    let f = photosuite_psd::PsdFile::from_bytes(bytes).unwrap();
    let mut errs = Vec::new();
    for b in &f.global_blocks {
        if let Err(e) = b.check_structure() {
            errs.push(format!("global {}: {e}", b.key_str()));
        }
    }
    for l in f.layers() {
        for b in &l.blocks {
            if let Err(e) = b.check_structure() {
                errs.push(format!("layer {}: {e}", b.key_str()));
            }
        }
    }
    errs
}

fn keep(name: &str, bytes: &[u8]) {
    if let Ok(dir) = std::env::var("PHOTOSUITE_SMART_PSD_OUT") {
        std::fs::write(std::path::Path::new(&dir).join(name), bytes).unwrap();
    }
}

#[test]
fn smart_objects_and_filters_survive_psd() {
    for depth in [8, 16, 32] {
        let mut s = session(depth);
        s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
        s.execute("filter.blur.gaussianBlur", json!({"radius": 2})).unwrap();
        s.execute("select.rect", json!({"x": 0, "y": 0, "width": 40, "height": 64})).unwrap();
        s.execute("filter.noise.addNoise", json!({"amount": 20, "seed": 3, "distribution": "gaussian"})).unwrap();
        s.execute("select.deselect", json!({})).unwrap();
        s.execute("layer.smartFilter.blendingOptions", json!({"opacity": 0.5, "blend": "screen"})).unwrap();
        s.execute("filter.stylize.emboss", json!({"angle": 120, "height": 2, "amount": 80})).unwrap();
        s.execute("layer.smartFilter.setVisible", json!({"index": 2, "visible": false})).unwrap();
        let id = s.active().unwrap().active_layer.unwrap().0;
        s.execute("edit.transform", json!({"layer": id, "matrix": [0.75, 0, 0, 0.75, 10, 4]})).unwrap();
        let doc = s.active().unwrap().doc.clone();
        let (_, before) = smart(&doc);
        let SmartSource::Embedded { bytes: pcraft, .. } = &before.source else { panic!("embedded") };
        let nested = decode_source("x.pcraft", pcraft).unwrap();

        let out = photosuite_io::export(&doc, "psd", &Default::default()).unwrap();
        assert!(!out.warnings.iter().any(|w| w.contains("pixels")), "{:?}", out.warnings);
        assert_eq!(structure_errors(&out.bytes), Vec::<String>::new());
        keep(&format!("smart-{depth}.psd"), &out.bytes);

        let back = photosuite_io::import("x.psd", &out.bytes).unwrap().document;
        let (bid, after) = smart(&back);
        assert!(matches!(after.source, SmartSource::Linked { .. }), "depth {depth}: source in lnk2");
        // Same filters, blending and settings (numbers may come back as floats, defaults spelled out).
        assert_eq!(after.smart_filters.len(), before.smart_filters.len());
        for (a, b) in after.smart_filters.iter().zip(&before.smart_filters) {
            assert_eq!((&a.command, a.blend, a.opacity, a.visible), (&b.command, b.blend, b.opacity, b.visible), "depth {depth}");
            for (k, v) in b.params.as_object().unwrap() {
                let w = &a.params[k];
                assert!(w == v || w.as_f64().zip(v.as_f64()).is_some_and(|(x, y)| x == y), "{}: {k} {v} came back as {w}", b.command);
            }
        }
        assert_eq!(after.filters_enabled, before.filters_enabled);
        // The filter mask covers the same pixels (PSD extends it with white past its bounds).
        let (ma, mb) = (after.filter_mask.as_ref().unwrap(), before.filter_mask.as_ref().unwrap());
        assert_eq!((ma.enabled, ma.linked), (mb.enabled, mb.linked));
        let area = doc.bounds().union(&before.cache.as_ref().unwrap().content_bounds());
        let (mut va, mut vb) = (Vec::new(), Vec::new());
        ma.values_into(area, &mut va);
        mb.values_into(area, &mut vb);
        assert_eq!(va, vb, "depth {depth}: filter mask");
        for (a, b) in after.transform.m.iter().zip(before.transform.m) {
            assert!((a - b).abs() < 1e-9, "depth {depth}: transform {:?} vs {:?}", after.transform, before.transform);
        }
        // The nested document came back (as the embedded PSB), layer for layer.
        let (name, src) = source_bytes(&back.metadata, &after.source).unwrap();
        assert!(name.ends_with(".psb"), "{name}");
        let inner = decode_source(&name, &src).unwrap();
        assert_eq!((inner.size, inner.layers.len(), inner.depth), (nested.size, nested.layers.len(), nested.depth));
        assert!(max_diff(&photosuite_compose::flatten(&inner).px, &photosuite_compose::flatten(&nested).px) <= 1.0 / 255.0 + 1e-6);
        // And it re-renders from that source like the original did.
        let mut l = back.layer(photosuite_doc::LayerId(bid)).unwrap().clone();
        assert!(refresh_layer(&back, &mut l).unwrap());
        let mut rerendered = back.clone();
        *rerendered.layer_mut(photosuite_doc::LayerId(bid)).unwrap() = l;
        let d = max_diff(&photosuite_compose::flatten(&rerendered).px, &photosuite_compose::flatten(&doc).px);
        assert!(d <= 2.0 / 255.0, "depth {depth}: re-render differs by {d}");

        // Saving the re-imported file again keeps the same embedded file (no growth).
        let again = photosuite_io::export(&back, "psd", &Default::default()).unwrap();
        assert_eq!(structure_errors(&again.bytes), Vec::<String>::new());
        assert!(again.bytes.len() <= out.bytes.len() + 64, "{} vs {}", again.bytes.len(), out.bytes.len());
    }
}

#[test]
fn camera_raw_psd_filters_survive_re_editing_native_save_and_undo() {
    for depth in [8, 16, 32] {
        let mut s = session(depth);
        s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
        s.execute("filter.blur.gaussianBlur", json!({"radius": 1})).unwrap();
        s.execute("select.rect", json!({"x": 10, "y": 8, "width": 50, "height": 40})).unwrap();
        s.execute(
            "filter.cameraRaw",
            json!({
                "exposure": 1.15, "contrast": -38, "highlights": 35, "shadows": -50, "whites": 49, "blacks": -34,
                "curveDarks": -21, "curveLights": 38, "hslHue": [-50, 0, 29, 1, 0, 0, 0, 0],
                "temperature": -26, "tint": -25, "texture": -31, "clarity": 30, "dehaze": -34, "vibrance": 33, "saturation": 33,
                "sharpenAmount": 43, "sharpenRadius": 1.2, "sharpenDetail": 36, "sharpenMasking": 30,
                "noiseLuminance": 47, "noiseLuminanceDetail": 50, "noiseColor": 25, "noiseColorDetail": 50,
                "gradeShadows": {"hue": 117, "sat": 52, "lum": -24}, "gradeMidtones": {"hue": 51, "sat": 51, "lum": 41},
                "gradeHighlights": {"hue": 316, "sat": 70, "lum": 28}, "gradeGlobal": {"hue": 20, "sat": 61, "lum": 45},
                "gradeBlending": 36, "gradeBalance": 35, "grainAmount": 45, "grainSize": 25, "grainRoughness": 50,
                "pointCurve": [[0, 0], [146, 102], [255, 255]], "pointCurveRed": [[0, 0], [90, 174], [255, 255]],
                "pointCurveGreen": [[0, 0], [162, 122], [255, 255]], "pointCurveBlue": [[0, 0], [129, 202], [255, 255]]
            }),
        )
        .unwrap();
        s.execute("select.deselect", json!({})).unwrap();
        s.execute("filter.blur.gaussianBlur", json!({"radius": 2})).unwrap();
        let before = s.active().unwrap().doc.clone();
        let out = photosuite_io::export(&before, "psd", &Default::default()).unwrap();
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        assert!(structure_errors(&out.bytes).is_empty());
        let back = photosuite_io::import("camera-raw.psd", &out.bytes).unwrap().document;
        let (layer, imported) = smart(&back);
        let (_, original) = smart(&before);
        let mask_values = |sm: &SmartObject| {
            let mut values = Vec::new();
            sm.filter_mask.as_ref().unwrap().values_into(before.bounds(), &mut values);
            values
        };
        assert_eq!(imported.smart_filters.len(), 3);
        assert_eq!(imported.smart_filters[1].command, "filter.cameraRaw");
        assert_eq!(mask_values(&imported), mask_values(&original));
        let expected = photosuite_engine::lens_cmds::raw_params("filter.cameraRaw", &original.smart_filters[1].params).unwrap();
        let actual = photosuite_engine::lens_cmds::raw_params("filter.cameraRaw", &imported.smart_filters[1].params).unwrap();
        assert_eq!(actual, expected, "{depth}-bit");
        let template = imported.smart_filters[1].params["__cameraRawPsd"].clone();
        let mut edited = Session::new();
        edited.add_document(back, None);
        edited.execute("layer.smartFilter.setParams", json!({"layer": layer, "index": 1, "params": {"exposure": 0, "curveLights": 0}})).unwrap();
        assert_eq!(smart(&edited.active().unwrap().doc).1.smart_filters[1].params["__cameraRawPsd"], template);
        let changed = edited.active().unwrap().doc.clone();
        edited.undo();
        assert_eq!(smart(&edited.active().unwrap().doc).1.smart_filters[1].params["exposure"], imported.smart_filters[1].params["exposure"]);
        edited.redo();
        assert_eq!(*edited.active().unwrap().doc, *changed);
        let native = photosuite_format::save_to_bytes(&changed, &Default::default()).unwrap();
        let native_back = photosuite_format::load_from_bytes(&native).unwrap();
        assert_eq!(smart(&native_back).1.smart_filters, smart(&changed).1.smart_filters);
        let out = photosuite_io::export(&native_back, "psd", &Default::default()).unwrap();
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        assert!(structure_errors(&out.bytes).is_empty());
        let back = photosuite_io::import("edited.psd", &out.bytes).unwrap().document;
        let (_, after) = smart(&back);
        assert_eq!(after.smart_filters.len(), 3);
        let params = photosuite_engine::lens_cmds::raw_params("filter.cameraRaw", &after.smart_filters[1].params).unwrap();
        assert_eq!((params.exposure, params.curve_lights), (0.0, 0.0));
        assert_eq!(params.hsl_hue, expected.hsl_hue);
        assert_eq!(mask_values(&after), mask_values(&original));
        let mut unfiltered = after.clone();
        unfiltered.smart_filters.clear();
        assert!(photosuite_engine::smart_cmds::render(&back, &unfiltered).unwrap().is_some());
    }
}

/// Private, user-supplied files remain outside the repository. Set PHOTOSUITE_CAMERA_RAW_PSD
/// to check a Camera Raw fixture and PHOTOSUITE_SMART_PSD_OUT to retain exports for manual checks.
#[test]
#[ignore = "requires a user-supplied Camera Raw PSD"]
fn camera_raw_psd_fixture() {
    let path = std::env::var("PHOTOSUITE_CAMERA_RAW_PSD").expect("set PHOTOSUITE_CAMERA_RAW_PSD");
    let bytes = std::fs::read(&path).unwrap();
    let imported = photosuite_io::import("fixture.psd", &bytes).unwrap();
    let (_, sm) = smart(&imported.document);
    let index = sm
        .smart_filters
        .iter()
        .position(|f| {
            photosuite_io::smart_map::item_for_filter(f).ok().is_some_and(
                |item| matches!(item.get("Fltr"), Some(photosuite_psd::descriptor::Value::Descriptor(d)) if d.class_id.is("Adobe Camera Raw Filter")),
            )
        })
        .expect("a Camera Raw filter");
    let original_item = photosuite_io::smart_map::item_for_filter(&sm.smart_filters[index]).unwrap();
    let out = photosuite_io::export(&imported.document, "psd", &Default::default()).unwrap();
    assert!(!out.warnings.iter().any(|w| w.contains("smart filter")), "{:?}", out.warnings);
    assert!(structure_errors(&out.bytes).is_empty());
    keep("camera-raw-roundtrip.psd", &out.bytes);
    let back = photosuite_io::import("roundtrip.psd", &out.bytes).unwrap().document;
    let (layer, _) = smart(&back);
    assert_eq!(photosuite_io::smart_map::item_for_filter(&smart(&back).1.smart_filters[index]).unwrap(), original_item);
    let native = photosuite_format::save_to_bytes(&back, &Default::default()).unwrap();
    assert_eq!(smart(&photosuite_format::load_from_bytes(&native).unwrap()).1.smart_filters, smart(&back).1.smart_filters);
    if sm.smart_filters[index].command == photosuite_io::smart_map::UNSUPPORTED_FILTER {
        assert_eq!(smart(&back).1.smart_filters[index].command, photosuite_io::smart_map::UNSUPPORTED_FILTER);
        println!("Camera Raw settings survive PSD and native saves; active unmapped controls keep this filter opaque");
        return;
    }
    let mut s = Session::new();
    s.add_document(back, None);
    s.execute("layer.smartFilter.setParams", json!({"layer": layer, "index": index, "params": {"exposure": 0.5}})).unwrap();
    let out = photosuite_io::export(&s.active().unwrap().doc, "psd", &Default::default()).unwrap();
    assert!(!out.warnings.iter().any(|w| w.contains("smart filter")), "{:?}", out.warnings);
    assert!(structure_errors(&out.bytes).is_empty());
    keep("camera-raw-edited.psd", &out.bytes);
    let back = photosuite_io::import("edited.psd", &out.bytes).unwrap().document;
    let after = &smart(&back).1.smart_filters[index];
    assert_eq!(after.params["exposure"], 0.5);
    let mut expected = original_item;
    let mut fltr = match expected.get("Fltr").unwrap() {
        photosuite_psd::descriptor::Value::Descriptor(d) => d.clone(),
        _ => panic!("Fltr must be a descriptor"),
    };
    fltr.items.iter_mut().find(|(key, _)| key.is("Ex12")).unwrap().1 = photosuite_psd::descriptor::Value::Double(0.5);
    expected.items.iter_mut().find(|(key, _)| key.is("Fltr")).unwrap().1 = photosuite_psd::descriptor::Value::Descriptor(fltr);
    assert_eq!(photosuite_io::smart_map::item_for_filter(after).unwrap(), expected, "only Exposure changes");
}

/// Every colour model: the smart object, its filters and the filter cache's unfiltered pixels
/// (in the document's model, CMYK inverted and 16-bit Lab scaled as in layer channels) survive.
#[test]
fn smart_filters_survive_psd_in_every_colour_model() {
    for (mode, depth) in [("cmyk", 8), ("cmyk", 16), ("grayscale", 8), ("lab", 8), ("lab", 16)] {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 32, "height": 24, "mode": mode, "depth": depth})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        s.execute("select.rect", json!({"x": 4, "y": 4, "width": 20, "height": 12})).unwrap();
        s.execute("edit.fill", json!({"color": "#c83c28"})).unwrap();
        s.execute("select.deselect", json!({})).unwrap();
        s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
        s.execute("filter.blur.gaussianBlur", json!({"radius": 1.5})).unwrap();
        let doc = s.active().unwrap().doc.clone();
        let out = photosuite_io::export(&doc, "psd", &Default::default()).unwrap();
        assert!(!out.warnings.iter().any(|w| w.contains("smart")), "{mode} {depth}: {:?}", out.warnings);
        assert_eq!(structure_errors(&out.bytes), Vec::<String>::new());
        let back = photosuite_io::import("x.psd", &out.bytes).unwrap().document;
        let ((_, a), (_, b)) = (smart(&doc), smart(&back));
        assert_eq!(b.smart_filters.len(), 1, "{mode} {depth}");
        assert_eq!(b.smart_filters[0].command, a.smart_filters[0].command);
        // The cache's unfiltered planes decode back to the unfiltered rendering.
        let f = photosuite_psd::PsdFile::from_bytes(&out.bytes).unwrap();
        let fx = f.global_blocks.iter().find(|g| &g.key == b"FEid").map(|g| photosuite_psd::filter_effects::FilterEffects::parse(&g.data).unwrap()).unwrap();
        let item = &fx.items[0];
        let (w, h) = item.rect.size().unwrap();
        let bits = u16::try_from(item.depth).unwrap();
        let first = item.slots[0].as_ref().unwrap().decode(w, h, bits).unwrap();
        let mut unfiltered = a.clone();
        unfiltered.smart_filters.clear();
        let px = photosuite_engine::smart_cmds::render(&doc, &unfiltered).unwrap().unwrap();
        let (x, y) = (10 - item.rect.left, 8 - item.rect.top);
        let at = (y as usize * w + x as usize) * usize::from(bits / 8);
        let stored = if bits == 8 { f32::from(first[at]) / 255.0 } else { f32::from(u16::from_be_bytes([first[at], first[at + 1]])) / 65535.0 };
        let v = px.sample_channel(10, 8, 0);
        let expect = if mode == "cmyk" { 1.0 - v } else { v };
        assert!((stored - expect).abs() < 0.01, "{mode} {depth}: slot 0 holds {stored}, expected {expect}");
    }
}

/// Writes sample files for checking in Photoshop (`PHOTOSUITE_SMART_PSD_OUT`): per depth, a
/// plain layer, a smart object without filters, and one with filters.
#[test]
#[ignore = "writes files for a manual Photoshop check"]
fn photoshop_samples() {
    for (mode, depth) in [("rgb", 8), ("rgb", 16), ("rgb", 32), ("cmyk", 8), ("lab", 16), ("grayscale", 8)] {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": W, "height": H, "mode": mode, "depth": depth})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        s.execute("select.rect", json!({"x": 10, "y": 8, "width": 50, "height": 40})).unwrap();
        s.execute("edit.fill", json!({"color": "#c83c28"})).unwrap();
        s.execute("select.deselect", json!({})).unwrap();
        let tag = format!("{mode}{depth}");
        keep(&format!("plain-{tag}.psd"), &photosuite_io::export(&s.active().unwrap().doc, "psd", &Default::default()).unwrap().bytes);
        s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
        keep(&format!("nofilter-{tag}.psd"), &photosuite_io::export(&s.active().unwrap().doc, "psd", &Default::default()).unwrap().bytes);
        s.execute("filter.blur.gaussianBlur", json!({"radius": 3})).unwrap();
        keep(&format!("gauss-{tag}.psd"), &photosuite_io::export(&s.active().unwrap().doc, "psd", &Default::default()).unwrap().bytes);
    }
}

/// PSD export of a 24 MP smart object versus the same layer as pixels (release:
/// `cargo test --release -p photosuite-engine --test smart_psd -- --ignored --nocapture perf`).
#[test]
#[ignore = "benchmark"]
fn perf_24mp_smart_object_export() {
    let (w, h) = (6000, 4000);
    let mut s = Session::new();
    s.execute("file.new", json!({"width": w, "height": h, "depth": 8})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    s.edit("paint", |doc, active| {
        let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
        let fmt = surf.format();
        let r = Rect::new(0, 0, w, h);
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for y in r.y0..r.y1 {
            for x in r.x0..r.x1 {
                // Photo-like: smooth gradients plus a little texture (RLE can't collapse it).
                let v = (x as f32 / w as f32) * 0.7 + (((x * 31 + y * 17) % 29) as f32 / 29.0) * 0.1;
                data.extend(photosuite_raster::from_rgba(&fmt, [v, (y as f32 / h as f32) * 0.8, 1.0 - v, 1.0]));
            }
        }
        surf.write_region(r, &data);
        Ok(())
    })
    .unwrap();
    let time = |doc: &Document| {
        let t = std::time::Instant::now();
        let out = photosuite_io::export(doc, "psd", &Default::default()).unwrap();
        (t.elapsed().as_secs_f64(), out.bytes.len(), out.warnings)
    };
    let (tf, sf, _) = time(&s.active().unwrap().doc);
    s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
    let (ts, ss, ws) = time(&s.active().unwrap().doc);
    assert!(ws.is_empty(), "{ws:?}");
    s.execute("filter.blur.gaussianBlur", json!({"radius": 3})).unwrap();
    let (tg, sg, wg) = time(&s.active().unwrap().doc);
    assert!(wg.is_empty(), "{wg:?}");
    let mb = |b: usize| b as f64 / 1e6;
    eprintln!(
        "24 MP PSD export: pixels {tf:.2} s {:.1} MB | smart object {ts:.2} s {:.1} MB | + Gaussian Blur smart filter {tg:.2} s {:.1} MB",
        mb(sf),
        mb(ss),
        mb(sg)
    );
    // The embedded file is about one more copy of the layer; the filter cache one more again.
    assert!(ss < sf * 5 / 2, "smart object {ss} vs pixels {sf}");
    assert!(sg < sf * 4, "with filters {sg} vs pixels {sf}");
}

#[test]
fn edited_psd_smart_objects_drop_their_old_file() {
    let mut s = session(8);
    s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
    let doc = s.active().unwrap().doc.clone();
    let first = photosuite_io::export(&doc, "psd", &Default::default()).unwrap().bytes;
    // Re-open, edit the contents (a new embedded file), save: one embedded file, the new one.
    let mut s2 = Session::new();
    s2.add_document(photosuite_io::import("a.psd", &first).unwrap().document, None);
    let (id, _) = smart(&s2.active().unwrap().doc);
    s2.select_layer(photosuite_doc::LayerId(id)).unwrap();
    s2.execute("layer.smartObjects.editContents", json!({})).unwrap();
    s2.execute("image.adjustments.invert", json!({})).unwrap();
    s2.execute("layer.smartObjects.saveContents", json!({})).unwrap();
    let parent = s2.documents().iter().position(|d| d.doc.layer(photosuite_doc::LayerId(id)).is_some()).unwrap();
    let doc2 = s2.documents()[parent].doc.clone();
    let second = photosuite_io::export(&doc2, "psd", &Default::default()).unwrap().bytes;
    let f = photosuite_psd::PsdFile::from_bytes(&second).unwrap();
    let uuids: Vec<String> = f.global_blocks.iter().filter(|b| &b.key == b"lnk2").flat_map(|b| photosuite_io::linked::block_uuids(&b.data)).collect();
    let back = photosuite_io::import("b.psd", &second).unwrap().document;
    let (_, sm) = smart(&back);
    let SmartSource::Linked { path } = &sm.source else { panic!() };
    assert_eq!(uuids, vec![path.clone()]);
}

/// Duplicated smart objects share their file in the PSD (Photoshop updates both when one is
/// edited); a "New Smart Object via Copy" gets a file of its own even with equal contents. After
/// re-opening, editing one duplicate updates the other and leaves the copy alone.
#[test]
fn shared_and_independent_smart_objects_through_psd() {
    let mut s = session(8);
    let a = s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap()["layer"].as_u64().unwrap();
    let b = s.execute("layer.duplicate", json!({})).unwrap()["layer"].as_u64().unwrap();
    s.select_layer(photosuite_doc::LayerId(a)).unwrap();
    let c = s.execute("layer.smartObjects.newSmartObjectViaCopy", json!({})).unwrap()["layer"].as_u64().unwrap();
    for (id, name) in [(a, "original"), (b, "duplicate"), (c, "via copy")] {
        s.execute("layer.setProps", json!({"layer": id, "name": name})).unwrap();
    }
    let doc = s.active().unwrap().doc.clone();
    let bytes = photosuite_io::export(&doc, "psd", &Default::default()).unwrap().bytes;
    let back = photosuite_io::import("a.psd", &bytes).unwrap().document;
    let path = |doc: &Document, name_of: u64| -> String {
        let name = &s.active().unwrap().doc.layer(photosuite_doc::LayerId(name_of)).unwrap().name;
        let l = doc.walk().into_iter().map(|(_, _, l)| l).find(|l| &l.name == name).unwrap().clone();
        match &l.content {
            LayerContent::Smart(SmartObject { source: SmartSource::Linked { path }, .. }) => path.clone(),
            _ => panic!("{name}: not a placed smart object"),
        }
    };
    let (pa, pb, pc) = (path(&back, a), path(&back, b), path(&back, c));
    assert_eq!(pa, pb, "the duplicate shares the file");
    assert_ne!(pa, pc, "the copy has its own file");
}

/// A duplicated placed layer (arriving with its source's `placed` id) is written with an id of
/// its own, as Photoshop gives every instance.
#[test]
fn duplicated_placed_layers_get_their_own_placed_id() {
    let mut s = session(8);
    s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
    let doc = s.active().unwrap().doc.clone();
    let first = photosuite_io::export(&doc, "psd", &Default::default()).unwrap().bytes;
    let mut s2 = Session::new();
    s2.add_document(photosuite_io::import("a.psd", &first).unwrap().document, None);
    let (id, _) = smart(&s2.active().unwrap().doc);
    s2.select_layer(photosuite_doc::LayerId(id)).unwrap();
    s2.execute("layer.duplicate", json!({})).unwrap();
    let second = photosuite_io::export(&s2.active().unwrap().doc, "psd", &Default::default()).unwrap().bytes;
    let f = photosuite_psd::PsdFile::from_bytes(&second).unwrap();
    let placed: Vec<(String, String)> = f
        .layers()
        .iter()
        .filter_map(|r| r.blocks.iter().find(|b| &b.key == b"SoLd").and_then(|b| photosuite_io::smart_map::parse_sold(&b.data)))
        .map(|p| (p.placed, p.idnt))
        .collect();
    assert_eq!(placed.len(), 2);
    assert_ne!(placed[0].0, placed[1].0, "own placed ids");
    assert_eq!(placed[0].1, placed[1].1, "one shared file");
}

/// A version-2 `liFD` item, as in CS2's `lnkD` block: the fields every version has, and
/// none of the version 5–7 fields after the file data.
fn lifd_v2(f: &photosuite_io::linked::LinkedFile) -> Vec<u8> {
    let mut item = b"liFD".to_vec();
    item.extend_from_slice(&2u32.to_be_bytes());
    item.push(f.uuid.len() as u8);
    item.extend_from_slice(f.uuid.as_bytes());
    let name: Vec<u16> = f.file_name.encode_utf16().collect();
    item.extend_from_slice(&(name.len() as u32).to_be_bytes());
    item.extend(name.iter().flat_map(|u| u.to_be_bytes()));
    item.extend_from_slice(&photosuite_io::linked::file_type(&f.bytes));
    item.extend_from_slice(b"8BIM");
    item.extend_from_slice(&(f.bytes.len() as u64).to_be_bytes());
    item.push(0); // no open-file descriptor
    item.extend_from_slice(&f.bytes);
    let mut out = (item.len() as u64).to_be_bytes().to_vec();
    out.extend(item);
    out.resize(out.len().next_multiple_of(4), 0);
    out
}

/// `doc` saved as PSD, then rewritten into CS2's layout: no `SoLd`, and `lnkD` with
/// version-2 items.
fn cs2_layout(doc: &Document) -> Vec<u8> {
    let out = photosuite_io::export(doc, "psd", &Default::default()).unwrap();
    let mut file = photosuite_psd::PsdFile::from_bytes(&out.bytes).unwrap();
    for rec in file.layers_mut() {
        rec.blocks.retain(|b| !matches!(&b.key, b"SoLd" | b"SoLE"));
    }
    assert!(file.layers().iter().any(|l| l.block(b"PlLd").is_some()));
    let links = file.global_blocks.iter_mut().find(|b| &b.key == b"lnk2").unwrap();
    let files = photosuite_io::linked::parse_linked_files(&links.data);
    assert_eq!(files.len(), 1);
    links.key = *b"lnkD";
    links.data = files.iter().flat_map(lifd_v2).collect();
    file.to_bytes().unwrap()
}

/// CS2 predates `SoLd`: its placed layers carry a `PlLd` block alone, and the embedded
/// file sits in a global `lnkD` block of older `liFD` items. Such a smart object's contents open
/// (they were "unavailable") and it re-renders in place from them.
#[test]
fn cs2_plld_only_smart_object_opens_its_contents() {
    let mut s = session(8);
    s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
    let id = s.active().unwrap().active_layer.unwrap().0;
    s.execute("edit.transform", json!({"layer": id, "matrix": [0.75, 0, 0, 0.75, 10, 4]})).unwrap();
    let doc = s.active().unwrap().doc.clone();
    let (_, before) = smart(&doc);
    let (name, src) = source_bytes(&doc.metadata, &before.source).unwrap();
    let nested = decode_source(&name, &src).unwrap();

    let back = photosuite_io::import("cs2.psd", &cs2_layout(&doc)).unwrap().document;
    let (bid, after) = smart(&back);
    for (a, b) in after.transform.m.iter().zip(before.transform.m) {
        assert!((a - b).abs() < 1e-9, "transform {:?} vs {:?}", after.transform, before.transform);
    }
    let (name, src) = source_bytes(&back.metadata, &after.source).expect("the contents are available");
    let inner = decode_source(&name, &src).unwrap();
    assert_eq!((inner.size, inner.layers.len()), (nested.size, nested.layers.len()));
    // It re-renders where it was placed.
    let mut l = back.layer(photosuite_doc::LayerId(bid)).unwrap().clone();
    assert!(refresh_layer(&back, &mut l).unwrap());
    let mut rerendered = back.clone();
    *rerendered.layer_mut(photosuite_doc::LayerId(bid)).unwrap() = l;
    let d = max_diff(&photosuite_compose::flatten(&rerendered).px, &photosuite_compose::flatten(&doc).px);
    assert!(d <= 2.0 / 255.0, "re-render differs by {d}");
    // And Edit Contents (the double-click) opens the nested document.
    let mut s2 = Session::new();
    s2.add_document(back, None);
    s2.select_layer(photosuite_doc::LayerId(bid)).unwrap();
    s2.execute("layer.smartObjects.editContents", json!({})).unwrap();
    assert_eq!(s2.active().unwrap().doc.size, nested.size);
}

/// CS2 can warp a smart object, and `PlLd` keeps the warp after the corners. Now that a
/// `PlLd`-only layer's contents are available, Free Transform, Image Size and mode changes
/// re-render it from them: it must come back warped, not silently unwarped.
#[test]
fn cs2_plld_only_warped_smart_object_stays_warped() {
    let mut s = session(8);
    s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
    let id = s.active().unwrap().active_layer.unwrap().0;
    s.execute("edit.transform", json!({"layer": id, "matrix": [0.75, 0, 0, 0.75, 10, 4]})).unwrap();
    s.execute("layer.smartObjects.warp", json!({"style": "wave", "bend": 60})).unwrap();
    let doc = s.active().unwrap().doc.clone();
    let (_, before) = smart(&doc);
    assert!(before.warp.is_some());

    let back = photosuite_io::import("cs2.psd", &cs2_layout(&doc)).unwrap().document;
    let (bid, after) = smart(&back);
    assert_eq!(after.warp, before.warp);
    assert!(source_bytes(&back.metadata, &after.source).is_some(), "the contents are available");
    // Re-rendered from its source, it is still warped where it was placed.
    let rerender = |warp| {
        let mut l = back.layer(photosuite_doc::LayerId(bid)).unwrap().clone();
        let LayerContent::Smart(sm) = &mut l.content else { panic!() };
        sm.warp = warp;
        assert!(refresh_layer(&back, &mut l).unwrap());
        let mut d = back.clone();
        *d.layer_mut(photosuite_doc::LayerId(bid)).unwrap() = l;
        max_diff(&photosuite_compose::flatten(&d).px, &photosuite_compose::flatten(&doc).px)
    };
    let d = rerender(after.warp.clone());
    assert!(d <= 2.0 / 255.0, "re-render differs by {d}");
    assert!(rerender(None) > 0.1, "an unwarped re-render would differ visibly");
}

/// A CS2 `PlLd`-only smart object is saved as it was while nothing changed it, and with its
/// current placement once it is moved: the old record was written back unchanged, so the save
/// reopened at the old place.
#[test]
fn cs2_plld_only_smart_object_saves_its_new_transform() {
    let mut s = session(8);
    s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
    let id = s.active().unwrap().active_layer.unwrap().0;
    s.execute("edit.transform", json!({"layer": id, "matrix": [0.75, 0, 0, 0.75, 10, 4]})).unwrap();
    let cs2 = cs2_layout(&s.active().unwrap().doc);
    let plld = |bytes: &[u8]| {
        let f = photosuite_psd::PsdFile::from_bytes(bytes).unwrap();
        let rec = f.layers().iter().find(|l| l.block(b"PlLd").is_some()).cloned().expect("a placed layer");
        (rec.block(b"PlLd").unwrap().data.clone(), rec.block(b"SoLd").is_some())
    };
    let back = photosuite_io::import("cs2.psd", &cs2).unwrap().document;
    let (bid, _) = smart(&back);

    // Unedited: the CS2 record is written back as it was.
    let same = photosuite_io::export(&back, "psd", &Default::default()).unwrap().bytes;
    assert_eq!(plld(&same), (plld(&cs2).0, false));

    // Moved and scaled: written with the new placement, and it reopens there.
    let mut s2 = Session::new();
    s2.add_document(back, None);
    s2.execute("edit.transform", json!({"layer": bid, "matrix": [0.5, 0, 0, 0.5, 20, 12]})).unwrap();
    let edited = s2.active().unwrap().doc.clone();
    let (_, want) = smart(&edited);
    let saved = photosuite_io::export(&edited, "psd", &Default::default()).unwrap().bytes;
    assert!(structure_errors(&saved).is_empty(), "{:?}", structure_errors(&saved));
    assert_ne!(plld(&saved).0, plld(&cs2).0, "the old PlLd record was written back");
    let again = photosuite_io::import("again.psd", &saved).unwrap().document;
    let (_, got) = smart(&again);
    for (a, b) in got.transform.m.iter().zip(want.transform.m) {
        assert!((a - b).abs() < 1e-9, "transform {:?} vs {:?}", got.transform, want.transform);
    }
    assert!(source_bytes(&again.metadata, &got.source).is_some(), "the contents are available");
}
