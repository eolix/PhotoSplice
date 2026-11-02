//! Vertical type (#199): orientation survives a PSD round trip (descriptor `Ornt` and the
//! engine data's writing direction), point and paragraph text, and the re-rendered pixels after
//! import are the vertical layout's (taller than wide).

use photosplice_color::{ColorMode, SampleType};
use photosplice_doc::text::{CharStyle, Orientation, TextRun, TextShape};
use photosplice_doc::{Document, Layer, LayerContent, Size, TextLayer};
use photosplice_geom::Affine;

fn doc_with(shape: TextShape) -> Document {
    let mut doc = Document::new("v", Size::new(300, 400), ColorMode::Rgb, SampleType::U8);
    let text = "Vertical type\nColumns";
    let mut t = TextLayer {
        text: text.into(),
        runs: vec![TextRun { len: text.len(), style: CharStyle { font_family: "Inter".into(), size_pt: 24.0, ..Default::default() } }],
        orientation: Orientation::Vertical,
        shape,
        transform: Affine::translate(250.0, 20.0),
        ..Default::default()
    };
    t.sync_summary();
    photosplice_text::TextEngine::new().render_layer(&mut t, doc.resolution_dpi, doc.pixel_format());
    doc.layers.push(Layer::new("v", LayerContent::Text(t)));
    doc
}

fn text(doc: &Document) -> &TextLayer {
    match &doc.layers[0].content {
        LayerContent::Text(t) => t,
        _ => panic!("not text"),
    }
}

#[test]
fn vertical_orientation_round_trips_through_psd() {
    for shape in [TextShape::Point, TextShape::Box { x: -200.0, y: 0.0, width: 220.0, height: 300.0 }] {
        let doc = doc_with(shape.clone());
        let ink = text(&doc).cache.as_ref().unwrap().content_bounds();
        assert!(ink.height() > ink.width(), "{shape:?}: {ink:?}");
        let out = photosplice_io::export(&doc, "v.psd", &Default::default()).unwrap();
        let back = photosplice_io::import("v.psd", &out.bytes).unwrap().document;
        let t = text(&back);
        assert_eq!(t.orientation, Orientation::Vertical, "{shape:?}");
        assert_eq!(t.shape, shape);
        assert_eq!(t.text, text(&doc).text);
        // Re-rendering the imported model gives the same vertical layout.
        let mut again = t.clone();
        photosplice_text::TextEngine::new().render_layer(&mut again, back.resolution_dpi, back.pixel_format());
        let r = again.cache.as_ref().unwrap().content_bounds();
        assert!(
            (r.x0 - ink.x0).abs() <= 1 && (r.y0 - ink.y0).abs() <= 1 && (r.x1 - ink.x1).abs() <= 1 && (r.y1 - ink.y1).abs() <= 1,
            "{shape:?}: {ink:?} → {r:?}"
        );
        // A second trip keeps it, and horizontal stays horizontal.
        let out2 = photosplice_io::export(&back, "v.psd", &Default::default()).unwrap();
        assert_eq!(text(&photosplice_io::import("v.psd", &out2.bytes).unwrap().document).orientation, Orientation::Vertical);
    }
    let mut doc = doc_with(TextShape::Point);
    if let LayerContent::Text(t) = &mut doc.layers[0].content {
        t.orientation = Orientation::Horizontal;
        t.psd_raw = None;
    }
    let out = photosplice_io::export(&doc, "h.psd", &Default::default()).unwrap();
    assert_eq!(text(&photosplice_io::import("h.psd", &out.bytes).unwrap().document).orientation, Orientation::Horizontal);
}

/// Roman text in vertical type: Reference's oracles carry `BaselineDirection` 1 in the run
/// (`vertical-latin-upright.psd`: letters stacked upright, one per em) or 2
/// (`vertical-latin-arial.psd`: the word turned along the column). Both read into
/// `rotate_roman`, and our re-render lands on Reference's pixels.
#[cfg(feature = "corpus")]
#[test]
fn roman_vertical_type_matches_reference_upright_and_rotated() {
    use photosplice_geom::Rect;
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/photoshop/text");
    assert!(dir.is_dir(), "{} is missing: run `cargo xtask corpus --all`", dir.display());
    let mut engine = photosplice_text::TextEngine::with_system_fonts();
    if !engine.fonts.resolve_postscript("ArialMT").exact {
        eprintln!("Arial is not installed: skipping the pixel comparison");
        return;
    }
    for (file, rotate) in [("vertical-latin-upright.psd", false), ("vertical-latin-arial.psd", true)] {
        let bytes = std::fs::read(dir.join(file)).unwrap();
        let doc = photosplice_io::import(file, &bytes).unwrap().document;
        let t = doc
            .layers
            .iter()
            .find_map(|l| match &l.content {
                LayerContent::Text(t) => Some(t),
                _ => None,
            })
            .unwrap();
        assert_eq!(t.orientation, Orientation::Vertical, "{file}");
        assert!(t.runs.iter().all(|r| r.style.rotate_roman == rotate), "{file}");
        let ps = t.cache.as_ref().unwrap();
        let (_, ours) = engine.render(t, doc.resolution_dpi, doc.pixel_format());
        let (a, b) = (ps.content_bounds(), ours.surface.content_bounds());
        let u = Rect::new(a.x0.min(b.x0), a.y0.min(b.y0), a.x1.max(b.x1), a.y1.max(b.y1));
        let alpha = |s: &photosplice_raster::Surface| -> Vec<f32> {
            let n = s.channels();
            s.read_region(u).chunks_exact(n).map(|p| p[n - 1]).collect()
        };
        let (pa, oa) = (alpha(ps), alpha(&ours.surface));
        let inter: f32 = pa.iter().zip(&oa).map(|(x, y)| x.min(*y)).sum();
        let union: f32 = pa.iter().zip(&oa).map(|(x, y)| x.max(*y)).sum();
        // Upright: 0.08 when centred by the em-wide advance on the font's ascent share.
        assert!(inter / union > 0.75, "{file}: IoU {} ({a:?} vs ours {b:?})", inter / union);
    }
}
