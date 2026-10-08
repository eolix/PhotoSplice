//! Builds a Photoshop verification kit: PSDs that exercise the PSD round-trip work, each next to
//! the original it came from, plus a checklist of what to look at in Photoshop.
//!
//! ```sh
//! cargo run --release -p photosuite-io --example psd_verify_kit -- <out dir>
//! ```
//!
//! Needs the corpora (`cargo xtask corpus --all`). Each case is one edit of the kind PhotoSuite
//! makes, so the export regenerates the block under test instead of copying it.
use std::path::Path;
use std::sync::Arc;

use photosuite_color::{BlendMode, Color, ColorMode, SampleType};
use photosuite_doc::{Adjustment, Document, Effect, Fill, Layer, LayerContent, ShapeLayer, ShapeStroke};
use photosuite_geom::{Rect, Size};

struct Case {
    file: &'static str,
    source: Option<&'static str>,
    edit: &'static str,
    expect: &'static str,
}

fn load(path: &str) -> Document {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{path}: {e} (run `cargo xtask corpus --all`)"));
    photosuite_io::import(path, &bytes).unwrap_or_else(|e| panic!("{path}: {e}")).document
}

fn save(doc: &Document, out: &Path, name: &str) -> Vec<String> {
    let (f, warnings) = photosuite_io::document_to_psd_with(doc, &Default::default());
    std::fs::write(out.join(name), f.to_bytes().expect("encode")).expect("write");
    warnings
}

fn for_each_layer(layers: &mut [Layer], f: &mut impl FnMut(&mut Layer)) {
    for l in layers {
        f(l);
        if let LayerContent::Group(g) = &mut l.content {
            for_each_layer(&mut g.children, f);
        }
    }
}

fn main() {
    let out = std::path::PathBuf::from(std::env::args().nth(1).expect("usage: psd_verify_kit <out dir>"));
    std::fs::create_dir_all(&out).expect("out dir");
    let mut cases: Vec<Case> = Vec::new();
    let mut log = Vec::new();

    // 1. One effect edited: the regenerated lfx2 is merged onto Photoshop's, placeholders kept.
    let src = "corpus/photoshop/effects/bevel-emboss-chisel-hard.psd";
    let mut d = load(src);
    for_each_layer(&mut d.layers, &mut |l| {
        for e in &mut l.effects.items {
            if let Effect::BevelEmboss(b) = e {
                b.size += 3.0;
            }
        }
    });
    log.push(save(&d, &out, "01-effect-edited.psd"));
    cases.push(Case {
        file: "01-effect-edited.psd",
        source: Some(src),
        edit: "Bevel & Emboss size +3 px",
        expect: "Layers panel lists only Bevel & Emboss under the layer (no disabled Drop Shadow, Glow, Stroke… rows). Layer Style dialog: Size is 3 px larger than in the original; every other bevel setting (Technique, Depth, Gloss Contour and its Anti-aliased box, highlight/shadow modes) matches the original.",
    });

    // 2. An effect added next to Photoshop's placeholders.
    let mut d = load(src);
    for_each_layer(&mut d.layers, &mut |l| {
        if !l.effects.items.is_empty() {
            l.effects.items.push(Effect::default_drop_shadow());
        }
    });
    log.push(save(&d, &out, "02-effect-added.psd"));
    cases.push(Case {
        file: "02-effect-added.psd",
        source: Some(src),
        edit: "Drop Shadow added next to the bevel",
        expect: "Layers panel lists exactly Drop Shadow and Bevel & Emboss, both on. The bevel is unchanged.",
    });

    // 3. Several effects of one kind: written as lmfx alone.
    let src3 = "corpus/psd-tools/effects/double-stroke-effects.psd";
    let mut d = load(src3);
    for_each_layer(&mut d.layers, &mut |l| {
        if let Some(Effect::Stroke(s)) = l.effects.items.iter_mut().find(|e| matches!(e, Effect::Stroke(_))) {
            s.size += 2.0;
        }
    });
    log.push(save(&d, &out, "03-two-strokes-edited.psd"));
    cases.push(Case {
        file: "03-two-strokes-edited.psd",
        source: Some(src3),
        edit: "first Stroke 2 px wider",
        expect: "Layers panel lists two Stroke effects (Photoshop's multiple-instance effects); the first is 2 px wider than in the original, the second unchanged.",
    });

    // 4. Background layer and the regenerated thumbnail, no edit.
    let src4 = "corpus/photoshop/adjustments/rgb8/baseline-layer-copy.psd";
    let d = load(src4);
    log.push(save(&d, &out, "04-background.psd"));
    cases.push(Case {
        file: "04-background.psd",
        source: Some(src4),
        edit: "none (opened and saved by PhotoSuite)",
        expect: "Bottom layer is Photoshop's real Background: name in italics, lock icon, can't be moved, no transparency. In File › Open (or Bridge) the file shows a thumbnail preview.",
    });

    // 5. A new document from PhotoSuite: Background, a pass-through group with an adjustment, a
    //    live shape layer and a gradient fill.
    let mut d = Document::with_background("kit", Size::new(400, 300), ColorMode::Rgb, SampleType::U8, Color::WHITE);
    let fmt = d.pixel_format();
    let mut px = Layer::raster("red square", fmt);
    if let Some(s) = px.surface_mut() {
        s.fill_rect(Rect::new(40, 40, 200, 200), &photosuite_raster::from_rgba(&fmt, [0.9, 0.1, 0.1, 1.0]));
    }
    let mut invert = Layer::new("Invert (inside)", LayerContent::Adjustment(Adjustment::Invert));
    invert.visible = true;
    let mut group = Layer::group("pass-through group", vec![invert]);
    group.blend = BlendMode::PassThrough;
    let mut shape = ShapeLayer {
        path: photosuite_vector::shapes::rect(220.0, 60.0, 140.0, 100.0),
        fill: Some(Fill::Solid(Color::rgb(0.1, 0.4, 0.9))),
        stroke: Some(ShapeStroke { width: 6.0, paint: Fill::Solid(Color::rgb(1.0, 0.8, 0.0)), ..Default::default() }),
        live: Some(photosuite_doc::LiveShape::Rect { rect: [220.0, 60.0, 360.0, 160.0], radii: [0.0; 4] }),
        ..Default::default()
    };
    shape.cache = Some(photosuite_vector::render_shape(&shape, fmt, d.bounds()));
    let shape = Layer::new("Rectangle 1", LayerContent::Shape(shape));
    d.layers.extend([px, shape, group]);
    log.push(save(&d, &out, "05-new-document.psd"));
    cases.push(Case {
        file: "05-new-document.psd",
        source: None,
        edit: "made in PhotoSuite: Background, red square, live rectangle shape, pass-through group holding an Invert adjustment",
        expect: "Background is a real Background. The group's blend mode reads Pass Through and the Invert inside it inverts the layers below (cyan square, inverted shape and background). The rectangle is a live shape: Properties panel shows W 140 / H 100, fill blue, stroke 6 px yellow, all editable.",
    });

    // 6. Smart object rewritten (placed-layer data regenerated; transform unchanged to the eye).
    let src6 = "corpus/photoshop/smart-filters/sf-blend-difference-50.psd";
    let mut d = load(src6);
    for_each_layer(&mut d.layers, &mut |l| {
        if let LayerContent::Smart(sm) = &mut l.content {
            sm.transform.m[4] += 1e-6;
        }
    });
    log.push(save(&d, &out, "06-smart-object-rewritten.psd"));
    cases.push(Case {
        file: "06-smart-object-rewritten.psd",
        source: Some(src6),
        edit: "placed-layer data (PlLd + SoLd) regenerated, transform moved by 0.000001 px",
        expect: "Still a smart object with its smart filter listed and live (double-click the filter: same settings as the original). Double-clicking the thumbnail opens the contents. Free Transform shows the same size and position as the original.",
    });

    // 7. Type layer rewritten: the TySh is rebuilt the way a text move does (warp kept).
    let src7 = "corpus/photoshop/text/warp-arc-50.psd";
    let mut d = load(src7);
    let dpi = d.resolution_dpi;
    for_each_layer(&mut d.layers, &mut |l| {
        if let LayerContent::Text(t) = &mut l.content
            && t.psd_raw.is_some()
        {
            t.psd_raw = Some(Arc::new(photosuite_text::psd::build_tysh(t, dpi, None)));
        }
    });
    log.push(save(&d, &out, "07-type-rewritten.psd"));
    cases.push(Case {
        file: "07-type-rewritten.psd",
        source: Some(src7),
        edit: "type layer data (TySh + EngineData) regenerated by PhotoSuite",
        expect: "Still an editable type layer with Warp Text = Arc, Bend +50 (Type › Warp Text). Click into the text and type a character: it re-renders with the same font, size and warp, without a warning about the text data.",
    });

    // 7b. Control: the same file through PhotoSuite with the type layer untouched (verbatim
    //     `TySh`): if this one also prompts, the cause is outside the type data.
    let d = load(src7);
    log.push(save(&d, &out, "07b-type-untouched.psd"));
    cases.push(Case {
        file: "07b-type-untouched.psd",
        source: Some(src7),
        edit: "none: opened and saved by PhotoSuite, type data written back verbatim (control for 07)",
        expect: "No \"update text layers\" prompt on open. Typing a character keeps the Arc warp.",
    });

    // 7c. A real text edit: one character appended, the type data rebuilt as an edit does.
    let mut d = load(src7);
    for_each_layer(&mut d.layers, &mut |l| {
        if let LayerContent::Text(t) = &mut l.content
            && t.psd_raw.is_some()
        {
            t.text.push('!');
            if let Some(r) = t.runs.last_mut() {
                r.len += 1;
            }
            if let Some(p) = t.paragraphs.last_mut() {
                p.len += 1;
            }
            t.psd_raw = Some(Arc::new(photosuite_text::psd::build_tysh(t, dpi, None)));
        }
    });
    log.push(save(&d, &out, "07c-type-text-edited.psd"));
    cases.push(Case {
        file: "07c-type-text-edited.psd",
        source: Some(src7),
        edit: "\"!\" appended to the text (type data rebuilt by PhotoSuite; the layer pixels are still the old rendering)",
        expect: "No prompt. The Type tool shows \"WARPED TEXT!\" with the Arc +50 warp, and typing another character keeps the warp. (Until Photoshop re-renders, the pixels may still show the text without \"!\".)",
    });

    // 7d. Experiment: Photoshop's own file with only its version info (1057) changed to what
    //     PhotoSuite wrote before ("Adobe Photoshop CS6" as the reader): if this prompts, 1057
    //     is the cause.
    {
        let bytes = std::fs::read(src7).expect("read");
        let mut f = photosuite_psd::PsdFile::from_bytes(&bytes).expect("parse");
        let old = photosuite_psd::resources::VersionInfo {
            version: 1,
            has_real_merged_data: true,
            writer: photosuite_psd::descriptor::UnicodeString::new_nul("Adobe Photoshop"),
            reader: photosuite_psd::descriptor::UnicodeString::new_nul("Adobe Photoshop CS6"),
            file_version: 1,
        };
        for r in f.resources.iter_mut().filter(|r| r.id == 1057) {
            r.data = old.to_bytes();
        }
        std::fs::write(out.join("07d-photoshop-file-old-version-info.psd"), f.to_bytes().expect("encode")).expect("write");
        log.push(Vec::new());
        cases.push(Case {
            file: "07d-photoshop-file-old-version-info.psd",
            source: Some(src7),
            edit: "experiment: Photoshop's own file, only resource 1057 (version info) changed to the old PhotoSuite value (reader \"Adobe Photoshop CS6\")",
            expect: "Tell me whether it shows the \"update text layers\" prompt (that would prove the version info is the trigger).",
        });
    }

    // 8. Layer comps rewritten (the comp list uses Photoshop's string key `list`).
    let src8 = "corpus/psd-tools/layer_comps.psd";
    let mut d = load(src8);
    if let Some(c) = d.layer_comps.first_mut() {
        c.name.push_str(" (renamed)");
    }
    log.push(save(&d, &out, "08-layer-comps-edited.psd"));
    cases.push(Case {
        file: "08-layer-comps-edited.psd",
        source: Some(src8),
        edit: "first layer comp renamed (layer comps rewritten)",
        expect: "Layer Comps panel lists the same comps as the original, the first one renamed \"… (renamed)\"; applying each comp shows the same layer visibility as in the original.",
    });

    // 9. Gradient fill edited: interpolation method and other unmodelled settings kept.
    let src9 = "corpus/psd-tools/adjustments/adjustment_backdrop_test.psd";
    let mut d = load(src9);
    for_each_layer(&mut d.layers, &mut |l| {
        if let LayerContent::Fill(Fill::Gradient { angle, .. }) = &mut l.content {
            *angle += 15.0;
        }
    });
    log.push(save(&d, &out, "09-gradient-fill-edited.psd"));
    cases.push(Case {
        file: "09-gradient-fill-edited.psd",
        source: Some(src9),
        edit: "each Gradient Fill layer's angle +15°",
        expect: "Double-click each gradient fill thumbnail: the angle is 15° more than in the original, and Method (Perceptual / Linear / Classic…), Style, Scale, Reverse, Dither, Align with layer match the original.",
    });

    for (c, w) in cases.iter().zip(&log) {
        if let Some(src) = c.source {
            std::fs::copy(src, out.join(c.file.replace(".psd", "-original.psd"))).expect("copy original");
        }
        if !w.is_empty() {
            eprintln!("{}: {w:?}", c.file);
        }
    }
    let mut md = String::from(
        "# PhotoSuite → Photoshop verification kit\n\nFor each file: open it in Photoshop, check the expectation, and note OK / not OK \
         (with what you saw). `*-original.psd` is the file it came from, for side-by-side comparison.\n\nThen please \
         **File › Save As** each `NN-….psd` into this folder as `NN-…-ps.psd` (Maximize Compatibility on), without other \
         changes: diffing Photoshop's re-save against ours shows exactly what Photoshop normalizes.\n\n",
    );
    for c in &cases {
        md.push_str(&format!("## {}\n\n- Source: {}\n- Edit: {}\n- Expect: {}\n- [ ] OK\n\n", c.file, c.source.unwrap_or("new document"), c.edit, c.expect));
    }
    std::fs::write(out.join("CHECKLIST.md"), md).expect("checklist");
    println!("{} cases written to {}", cases.len(), out.display());
}
