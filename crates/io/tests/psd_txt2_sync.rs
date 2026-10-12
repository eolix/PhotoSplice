//! The document's `Txt2` (Photoshop's document-wide copy of every type layer) is kept only while it
//! still describes the type layers: Photoshop reads it in preference to each layer's `TySh`, so a
//! text edit written with the old `Txt2` reverted in Photoshop (verified 2026-10-12). Without it,
//! Photoshop rebuilds `Txt2` from the layers.

use std::sync::Arc;

use photosuite_doc::text::{CharStyle, TextRun};
use photosuite_doc::{Document, Layer, LayerContent, TextLayer};

const TXT2: &[u8] = b"/0 << /0 1 >> /1 << /0 2 >>";

fn has_txt2(bytes: &[u8]) -> bool {
    let f = photosuite_psd::PsdFile::from_bytes(bytes).unwrap();
    f.global_blocks.iter().any(|b| &b.key == b"Txt2")
}

fn text_layer(text: &str) -> Layer {
    let mut t = TextLayer {
        text: text.into(),
        runs: vec![TextRun { len: text.len(), style: CharStyle { font_family: "Inter".into(), size_pt: 24.0, ..Default::default() } }],
        ..Default::default()
    };
    t.sync_summary();
    Layer::new(text, LayerContent::Text(t))
}

/// A PSD with one type layer, re-imported (so its `TySh` is the file's), with a `Txt2` block.
fn imported_with_txt2() -> Document {
    let mut doc = Document::new("t", photosuite_geom::Size::new(64, 32), photosuite_color::ColorMode::Rgb, photosuite_color::SampleType::U8);
    doc.layers.push(text_layer("Hello"));
    let bytes = photosuite_io::export(&doc, "t.psd", &Default::default()).unwrap().bytes;
    let mut doc = photosuite_io::import("t.psd", &bytes).unwrap().document;
    doc.metadata.psd_global_blocks.push((*b"8BIM", *b"Txt2", Arc::new(TXT2.to_vec())));
    doc
}

fn export(doc: &Document) -> Vec<u8> {
    photosuite_io::export(doc, "t.psd", &Default::default()).unwrap().bytes
}

#[test]
fn unedited_type_layers_keep_txt2() {
    assert!(has_txt2(&export(&imported_with_txt2())));
}

#[test]
fn a_text_edit_drops_the_stale_txt2() {
    let mut doc = imported_with_txt2();
    let LayerContent::Text(t) = &mut doc.layers[0].content else { panic!("text layer") };
    // A text edit rebuilds the layer's type data (as the engine does).
    t.text.push('!');
    t.runs[0].len += 1;
    t.sync_summary();
    t.psd_raw = Some(Arc::new(photosuite_text::psd::build_tysh(t, 72.0, None)));
    let bytes = export(&doc);
    assert!(!has_txt2(&bytes), "Txt2 dropped");
    let back = photosuite_io::import("t.psd", &bytes).unwrap().document;
    let LayerContent::Text(b) = &back.layers[0].content else { panic!("text layer") };
    assert_eq!(b.text, "Hello!");
}

#[test]
fn new_and_duplicated_type_layers_drop_txt2() {
    // A new type layer has no object of its own in Txt2.
    let mut doc = imported_with_txt2();
    doc.layers.push(text_layer("New"));
    assert!(!has_txt2(&export(&doc)));
    // A duplicate shares its source's object: editing either would go stale.
    let mut doc = imported_with_txt2();
    let mut copy = doc.layers[0].clone();
    copy.id = photosuite_doc::LayerId(doc.layers.iter().map(|l| l.id.0).max().unwrap_or(0) + 1);
    doc.layers.push(copy);
    assert!(!has_txt2(&export(&doc)));
}
