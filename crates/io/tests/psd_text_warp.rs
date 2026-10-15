//! Warped type re-rendered from the model matches Reference's own rendering of it: the corpus's
//! warped-text oracles (Arc, Bulge with distortion, Flag, vertical Rise, Twist), whose layer
//! pixels are Reference's.
#![cfg(feature = "corpus")]

use photosplice_doc::LayerContent;

#[test]
fn warped_text_matches_reference_pixels() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/photoshop/text");
    assert!(dir.is_dir(), "{} is missing: run `cargo xtask corpus --all`", dir.display());
    for name in ["warp-arc-50.psd", "warp-bulge-60-persp.psd", "warp-flag-40.psd", "warp-rise-vertical-30.psd", "warp-twist-35.psd"] {
        let doc = photosplice_io::import(name, &std::fs::read(dir.join(name)).unwrap()).unwrap().document;
        let l = doc.walk().into_iter().find(|(_, _, l)| matches!(l.content, LayerContent::Text(_))).map(|(_, _, l)| l.clone()).unwrap();
        let LayerContent::Text(t) = &l.content else { unreachable!() };
        let theirs = t.cache.clone().expect("Reference's pixels");
        let (_, r) = photosplice_text::shared().lock().unwrap().render(t, doc.resolution_dpi, doc.pixel_format());
        let ours = r.surface;
        let b = theirs.content_bounds().union(&ours.content_bounds());
        let n = theirs.channels();
        let (mut inter, mut union) = (0.0f64, 0.0f64);
        for (p, q) in theirs.read_region(b).chunks_exact(n).zip(ours.read_region(b).chunks_exact(n)) {
            let (x, y) = (f64::from(p[n - 1]), f64::from(q[n - 1]));
            inter += x.min(y);
            union += x.max(y);
        }
        let iou = inter / union.max(1e-9);
        // Glyph rasterisation differs slightly; the warp shape and box must agree (it was 0.0
        // to 0.86 with our own approximations and the line box).
        assert!(iou > 0.9, "{name}: alpha overlap {iou:.3}");
        let (tb, ob) = (theirs.content_bounds(), ours.content_bounds());
        assert!((tb.x0 - ob.x0).abs() <= 2 && (tb.y0 - ob.y0).abs() <= 2 && (tb.x1 - ob.x1).abs() <= 2 && (tb.y1 - ob.y1).abs() <= 2, "{name}: {tb:?} vs {ob:?}");
    }
}
