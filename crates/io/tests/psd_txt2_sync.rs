//! The document's `Txt2` (Reference's document-wide copy of every type layer) must describe the
//! type layers: Reference reads it in preference to each layer's `TySh`, so a text edit written
//! with the old `Txt2` reverted in Reference (verified 2026-10-12). An edited layer's object is
//! updated, a new or duplicated layer gets an object of its own; when that can't be done (a style
//! its runs don't have, no object to start from), the file's `Txt2` is dropped and one is made
//! from the layers, as for a document that had none.

use std::sync::Arc;

use photosplice_doc::text::{CharStyle, TextRun};
use photosplice_doc::{Document, Layer, LayerContent, TextLayer};

const TXT2: &[u8] = b"/0 << /0 1 >> /1 << /0 2 >>";

fn has_txt2(bytes: &[u8]) -> bool {
    let f = photosplice_psd::PsdFile::from_bytes(bytes).unwrap();
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
    let mut doc = Document::new("t", photosplice_geom::Size::new(64, 32), photosplice_color::ColorMode::Rgb, photosplice_color::SampleType::U8);
    doc.layers.push(text_layer("Hello"));
    let bytes = photosplice_io::export(&doc, "t.psd", &Default::default()).unwrap().bytes;
    let mut doc = photosplice_io::import("t.psd", &bytes).unwrap().document;
    // In place of the one the save made from the layer.
    doc.metadata.psd_global_blocks.retain(|(_, k, _)| k != b"Txt2");
    doc.metadata.psd_global_blocks.push((*b"8BIM", *b"Txt2", Arc::new(TXT2.to_vec())));
    doc
}

fn export(doc: &Document) -> Vec<u8> {
    photosplice_io::export(doc, "t.psd", &Default::default()).unwrap().bytes
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
    t.psd_raw = Some(Arc::new(photosplice_text::psd::build_tysh(t, 72.0, None)));
    let bytes = export(&doc);
    assert_eq!(made_txt2_texts(&bytes), ["Hello!\r"], "the stale Txt2 replaced by one made from the layer");
    let back = photosplice_io::import("t.psd", &bytes).unwrap().document;
    let LayerContent::Text(b) = &back.layers[0].content else { panic!("text layer") };
    assert_eq!(b.text, "Hello!");
}

/// The texts of a saved file's `Txt2` objects, when the block is one the save made from the layers
/// (Reference 2026's layout, version 14, which the fixtures' blocks aren't); empty otherwise.
fn made_txt2_texts(bytes: &[u8]) -> Vec<String> {
    let Some(txt2) = txt2_of(bytes) else { return Vec::new() };
    if txt2.path(&["98", "0"]).and_then(|v| v.as_i64()) != Some(14) || txt2.get("DocumentObjects").is_some() {
        return Vec::new();
    }
    objects(&txt2).iter().filter_map(text_of).map(str::to_string).collect()
}

/// A `Txt2` whose objects can't be read (here none at all) can't take a new or duplicated layer:
/// it is dropped, and one is made from the layers.
#[test]
fn new_and_duplicated_type_layers_drop_an_unreadable_txt2() {
    // A new type layer has no object of its own in Txt2.
    let mut doc = imported_with_txt2();
    doc.layers.push(text_layer("New"));
    assert_eq!(made_txt2_texts(&export(&doc)), ["Hello\r", "New\r"]);
    // A duplicate shares its source's object: editing either would go stale.
    let mut doc = imported_with_txt2();
    let mut copy = doc.layers[0].clone();
    copy.id = photosplice_doc::LayerId(doc.layers.iter().map(|l| l.id.0).max().unwrap_or(0) + 1);
    doc.layers.push(copy);
    assert_eq!(made_txt2_texts(&export(&doc)), ["Hello\r", "Hello\r"]);
}

/// A `Txt2` as Reference lays it out for one type layer holding `text`: its font list, its
/// object's text, one paragraph and one style run (optical kerning, a `Txt2`-only setting, and a
/// black fill), and a view.
fn real_txt2(text: &str) -> Vec<u8> {
    let n = text.encode_utf16().count() + 1;
    let mut v = b" /0 << /1 << /0 [ << /0 << /99 /CoolTypeFont /0 << /0 (Inter-Regular) /2 1 >> >> >> ] >> /8 << /0 [ << /0 << /2 << /6 [ -1.0 -1.0 ] /11 << /4 -1 /18 -1.0 /22 0.025 >> >> >> >> ] >> >> /1 << /1 [ << /0 << /0 (".to_vec();
    v.extend_from_slice(&[0xFE, 0xFF]);
    for u in format!("{text}\r").encode_utf16() {
        for b in u.to_be_bytes() {
            if matches!(b, b'(' | b')' | b'\\') {
                v.push(b'\\');
            }
            v.push(b);
        }
    }
    v.extend_from_slice(
        format!(") /5 << /0 [ << /0 << /0 << /5 << /9 false >> /6 1 >> >> /1 {n} >> ] >> /6 << /0 [ << /0 << /0 << /6 << /0 0 /11 2 /53 << /99 /SimplePaint /0 << /0 1 /1 [ 1.0 0.0 0.0 0.0 ] >> >> >> >> >> /1 {n} >> ] >> >> /1 << /0 [ << /0 0 >> ] /1 << /0 [ << /0 << /1 1 >> /1 {n} >> ] >> /2 [ << /99 /PC /6 [ << /99 /F /10 0 >> ] >> ] >> >> ] >>")
            .as_bytes(),
    );
    v
}

/// One type layer read from a PSD whose `Txt2` describes it (so import applies it).
fn imported_with_real_txt2() -> Document {
    let mut doc = Document::new("t", photosplice_geom::Size::new(64, 32), photosplice_color::ColorMode::Rgb, photosplice_color::SampleType::U8);
    doc.layers.push(text_layer("Hello"));
    let mut f = photosplice_psd::PsdFile::from_bytes(&export(&doc)).unwrap();
    let data = real_txt2("Hello");
    let padding = Some(vec![0; (4 - data.len() % 4) % 4]);
    f.global_blocks.retain(|b| b.key != *b"Txt2");
    f.global_blocks.push(photosplice_psd::TaggedBlock { signature: *b"8BIM", key: *b"Txt2", data, padding });
    photosplice_io::import("t.psd", &f.to_bytes().unwrap()).unwrap().document
}

fn txt2_of(bytes: &[u8]) -> Option<photosplice_text::engine_data::Value> {
    let f = photosplice_psd::PsdFile::from_bytes(bytes).unwrap();
    f.global_blocks.iter().find(|b| &b.key == b"Txt2").and_then(|b| photosplice_text::psd::parse_txt2(&b.data))
}

/// A text edit updates the layer's `Txt2` object (text, run and view lengths) rather than dropping
/// `Txt2`, so what only `Txt2` holds survives (Reference-verified 2026-10-13 with text on a path:
/// the file opens as edited and the text stays on its path).
#[test]
fn a_text_edit_updates_its_txt2_object() {
    let mut doc = imported_with_real_txt2();
    let LayerContent::Text(t) = &mut doc.layers[0].content else { panic!("text layer") };
    assert_eq!(t.char_runs()[0].style.kerning, photosplice_doc::text::Kerning::Optical, "read from Txt2");
    t.text = "Hello\nthere".into();
    t.runs[0].len = t.text.len();
    t.paragraphs = vec![photosplice_doc::text::ParagraphRun { len: t.text.len(), style: t.paragraph_runs()[0].style.clone() }];
    t.sync_summary();
    t.psd_raw = Some(Arc::new(photosplice_text::psd::build_tysh(t, 72.0, None)));
    let bytes = export(&doc);
    let txt2 = txt2_of(&bytes).expect("Txt2 kept");
    let obj = txt2.path(&["1", "1"]).and_then(|o| o.as_array()).and_then(|o| o.first()).unwrap();
    assert_eq!(obj.path(&["0", "0"]).and_then(|v| v.as_str()), Some("Hello\rthere\r"));
    let len = |p: &[&str]| obj.path(p).and_then(|a| a.as_array()).and_then(|a| a.first()).and_then(|r| r.get("1")).and_then(|v| v.as_i64());
    assert_eq!(len(&["0", "6", "0"]), Some(12));
    // Paragraph runs and the view, one entry per paragraph (one run over both made them one line
    // when Reference updated the layer).
    let lens = |p: &[&str]| -> Vec<i64> { obj.path(p).and_then(|a| a.as_array()).unwrap().iter().filter_map(|v| v.get("1").and_then(|n| n.as_i64())).collect() };
    assert_eq!(lens(&["0", "5", "0"]), vec![6, 6]);
    assert_eq!(lens(&["1", "1", "0"]), vec![6, 6]);
    assert!(obj.path(&["1", "2"]).is_some(), "Photoshop's cached view kept");
    // Read back: the edit, and the optical kerning only Txt2 had.
    let back = photosplice_io::import("t.psd", &bytes).unwrap().document;
    let LayerContent::Text(b) = &back.layers[0].content else { panic!("text layer") };
    assert_eq!(b.text, "Hello\nthere");
    assert_eq!(b.char_runs()[0].style.kerning, photosplice_doc::text::Kerning::Optical);
}

/// A changed attribute is written into the run Reference's `Txt2` style keys hold it in (here the
/// fill colour, `53`), on a new run split from the old one; a font `Txt2` doesn't list yet is
/// added to its font list.
#[test]
fn style_changes_go_into_txt2() {
    let recolour = |colour: photosplice_doc::Color, family: Option<&str>| {
        let mut doc = imported_with_real_txt2();
        let LayerContent::Text(t) = &mut doc.layers[0].content else { panic!("text layer") };
        let mut red = t.runs[0].style.clone();
        red.color = colour;
        if let Some(f) = family {
            red.font_family = f.into();
            red.postscript_name = Some(format!("{}-Regular", f.replace(' ', "")));
        }
        t.runs = vec![TextRun { len: 2, style: t.runs[0].style.clone() }, TextRun { len: 3, style: red }];
        t.sync_summary();
        t.psd_raw = Some(Arc::new(photosplice_text::psd::build_tysh(t, 72.0, None)));
        txt2_of(&export(&doc))
    };
    let txt2 = recolour(photosplice_doc::Color::rgb(1.0, 0.0, 0.0), None).expect("Txt2 kept");
    let runs = txt2.path(&["1", "1"]).and_then(|o| o.as_array()).and_then(|o| o.first()).and_then(|o| o.path(&["0", "6", "0"])).and_then(|a| a.as_array()).unwrap();
    let fill = |r: &photosplice_text::engine_data::Value| r.path(&["0", "0", "6", "53", "0", "1"]).and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_f64()).collect::<Vec<_>>());
    let lens: Vec<i64> = runs.iter().filter_map(|r| r.get("1").and_then(|v| v.as_i64())).collect();
    assert_eq!(lens, vec![2, 4], "\"He\" and \"llo\" with the break");
    assert_eq!(fill(&runs[0]), Some(vec![1.0, 0.0, 0.0, 0.0]));
    assert_eq!(fill(&runs[1]), Some(vec![1.0, 1.0, 0.0, 0.0]), "a r g b");
    assert_eq!(runs[1].path(&["0", "0", "6", "11"]).and_then(|v| v.as_i64()), Some(2), "the optical kerning stays");
    let txt2 = recolour(photosplice_doc::Color::rgb(1.0, 0.0, 0.0), Some("JetBrains Mono")).expect("Txt2 kept");
    let fonts: Vec<&str> = txt2.path(&["0", "1", "0"]).and_then(|a| a.as_array()).unwrap().iter().filter_map(|f| f.path(&["0", "0", "0"]).and_then(|n| n.as_str())).collect();
    assert_eq!(fonts, vec!["Inter-Regular", "JetBrainsMono-Regular"]);
    let runs = txt2.path(&["1", "1"]).and_then(|o| o.as_array()).and_then(|o| o.first()).and_then(|o| o.path(&["0", "6", "0"])).and_then(|a| a.as_array()).unwrap();
    let font = |r: &photosplice_text::engine_data::Value| r.path(&["0", "0", "6", "0"]).and_then(|v| v.as_i64());
    assert_eq!((font(&runs[0]), font(&runs[1])), (Some(0), Some(1)));
}

/// A paragraph change is written into Reference's paragraph keys (`0`: justification, centre 2),
/// keeping the ones it had.
#[test]
fn paragraph_changes_go_into_txt2() {
    let mut doc = imported_with_real_txt2();
    let LayerContent::Text(t) = &mut doc.layers[0].content else { panic!("text layer") };
    for p in &mut t.paragraphs {
        p.style.align = photosplice_doc::text::TextAlign::Center;
    }
    t.psd_raw = Some(Arc::new(photosplice_text::psd::build_tysh(t, 72.0, None)));
    let txt2 = txt2_of(&export(&doc)).expect("Txt2 kept");
    let props = txt2.path(&["1", "1"]).and_then(|o| o.as_array()).and_then(|o| o.first()).and_then(|o| o.path(&["0", "5", "0"])).and_then(|a| a.as_array()).and_then(|a| a.first()).and_then(|r| r.path(&["0", "0", "5"])).unwrap();
    assert_eq!(props.get("0").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(props.get("9"), Some(&photosplice_text::engine_data::Value::Bool(false)));
}

fn objects(txt2: &photosplice_text::engine_data::Value) -> &[photosplice_text::engine_data::Value] {
    txt2.path(&["1", "1"]).and_then(|o| o.as_array()).unwrap()
}

fn text_of(obj: &photosplice_text::engine_data::Value) -> Option<&str> {
    obj.path(&["0", "0"]).and_then(|v| v.as_str())
}

/// The frame index a text object's view names (`/1 /2 [0] /6 [0] /10`) and its frame list
/// (`/1 /0`).
fn frame_refs(obj: &photosplice_text::engine_data::Value) -> (Option<i64>, Option<i64>) {
    let view = obj.path(&["1", "2"]).and_then(|v| v.as_array()).and_then(|v| v.first()).and_then(|v| v.get("6")).and_then(|v| v.as_array()).and_then(|v| v.first());
    let list = obj.path(&["1", "0"]).and_then(|v| v.as_array()).and_then(|v| v.first()).and_then(|v| v.get("0"));
    (view.and_then(|v| v.get("10")).and_then(|v| v.as_i64()), list.and_then(|v| v.as_i64()))
}

fn tysh_index(bytes: &[u8], layer: usize) -> Option<i64> {
    let f = photosplice_psd::PsdFile::from_bytes(bytes).unwrap();
    f.layers().get(layer).and_then(|l| l.block(b"TySh")).and_then(|b| photosplice_text::psd::text_index(&b.data))
}

/// A new type layer gets an object of its own, appended to `Txt2` with a frame of its own and
/// written as the next `TextIndex`, so `Txt2` is kept for the other layers (what only it holds,
/// such as text on a path or optical kerning, stays).
#[test]
fn a_new_type_layer_is_appended_to_txt2() {
    let mut doc = imported_with_real_txt2();
    doc.layers.push(text_layer("New"));
    let bytes = export(&doc);
    let txt2 = txt2_of(&bytes).expect("Txt2 kept");
    let objs = objects(&txt2);
    assert_eq!(objs.len(), 2);
    assert_eq!(text_of(&objs[0]), Some("Hello\r"));
    assert_eq!(text_of(&objs[1]), Some("New\r"));
    assert_eq!(frame_refs(&objs[0]), (Some(0), Some(0)));
    assert_eq!(frame_refs(&objs[1]), (Some(1), Some(1)), "a frame of its own");
    assert_eq!(txt2.path(&["0", "8", "0"]).and_then(|f| f.as_array()).map(<[_]>::len), Some(2));
    assert_eq!((tysh_index(&bytes, 0), tysh_index(&bytes, 1)), (Some(0), Some(1)));
    // One run per paragraph, the new text's length.
    let len = objs[1].path(&["0", "6", "0"]).and_then(|a| a.as_array()).and_then(|a| a.first()).and_then(|r| r.get("1")).and_then(|v| v.as_i64());
    assert_eq!(len, Some(4));
    let back = photosplice_io::import("t.psd", &bytes).unwrap().document;
    let texts: Vec<_> = back.layers.iter().filter_map(|l| if let LayerContent::Text(t) = &l.content { Some(t.text.clone()) } else { None }).collect();
    assert_eq!(texts, ["Hello", "New"]);
    let LayerContent::Text(b) = &back.layers[0].content else { panic!("text layer") };
    assert_eq!(b.char_runs()[0].style.kerning, photosplice_doc::text::Kerning::Optical, "Txt2 kept for the other layer");
    // Saved again unchanged, the file keeps its two objects as they are.
    assert_eq!(txt2_of(&export(&back)).map(|t| objects(&t).len()), Some(2));
}

/// A duplicate shares its source's `TextIndex`: it gets an object of its own (a copy of its
/// source's), so editing either one later updates only its own object.
#[test]
fn a_duplicated_type_layer_gets_its_own_txt2_object() {
    let mut doc = imported_with_real_txt2();
    let mut copy = doc.layers[0].clone();
    copy.id = photosplice_doc::LayerId(doc.layers.iter().map(|l| l.id.0).max().unwrap_or(0) + 1);
    doc.layers.push(copy);
    let bytes = export(&doc);
    let txt2 = txt2_of(&bytes).expect("Txt2 kept");
    assert_eq!(objects(&txt2).iter().map(text_of).collect::<Vec<_>>(), [Some("Hello\r"), Some("Hello\r")]);
    assert_eq!((tysh_index(&bytes, 0), tysh_index(&bytes, 1)), (Some(0), Some(1)));
    // Edit the copy (after a round trip, as its own layer now): only object 1 changes.
    let mut back = photosplice_io::import("t.psd", &bytes).unwrap().document;
    let LayerContent::Text(t) = &mut back.layers[1].content else { panic!("text layer") };
    t.text = "Copy".into();
    t.runs[0].len = 4;
    t.sync_summary();
    t.psd_raw = Some(Arc::new(photosplice_text::psd::build_tysh(t, 72.0, None)));
    let txt2 = txt2_of(&export(&back)).expect("Txt2 kept");
    assert_eq!(objects(&txt2).iter().map(text_of).collect::<Vec<_>>(), [Some("Hello\r"), Some("Copy\r")]);
}

/// A new layer of a kind no object in `Txt2` is (box text among point text) starts from another
/// kind's object, with a frame of its own kind made for it: a box outlining its box.
#[test]
fn a_new_box_layer_among_point_text_gets_a_box_frame() {
    let mut doc = imported_with_real_txt2();
    let mut boxed = text_layer("Boxed");
    if let LayerContent::Text(t) = &mut boxed.content {
        t.shape = photosplice_doc::text::TextShape::Box { x: 0.0, y: 0.0, width: 40.0, height: 20.0 };
    }
    doc.layers.push(boxed);
    let bytes = export(&doc);
    let txt2 = txt2_of(&bytes).expect("Txt2 kept");
    assert_eq!(text_of(&objects(&txt2)[1]), Some("Boxed\r"));
    let frame = txt2.path(&["0", "8", "0"]).and_then(|f| f.as_array()).and_then(|f| f.get(1)).unwrap();
    assert_eq!(frame.path(&["0", "2", "0"]).and_then(|v| v.as_i64()), Some(1), "a box frame");
    let pts: Vec<f64> = frame.path(&["0", "1", "0"]).and_then(|p| p.as_array()).unwrap().iter().filter_map(|v| v.as_f64()).collect();
    assert_eq!(pts.len(), 32);
    let back = photosplice_io::import("t.psd", &bytes).unwrap().document;
    let LayerContent::Text(b) = &back.layers[1].content else { panic!("text layer") };
    assert!(matches!(b.shape, photosplice_doc::text::TextShape::Box { .. }));
}

#[cfg(feature = "corpus")]
fn oracle(name: &str) -> Document {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/photoshop/text").join(name);
    let bytes = std::fs::read(&path).unwrap_or_else(|_| panic!("{} is missing: run `cargo xtask corpus --all`", path.display()));
    photosplice_io::import(name, &bytes).unwrap().document
}

/// On a Reference file with three point-text objects, a duplicate and a new layer are appended as
/// objects 3 and 4, each with its own frame, and read back with their text.
#[cfg(feature = "corpus")]
#[test]
fn reference_point_text_takes_new_and_duplicated_layers() {
    let mut doc = oracle("align-point-left-center-right.psd");
    let source = doc.layers.iter().position(|l| matches!(l.content, LayerContent::Text(_))).unwrap();
    let mut copy = doc.layers[source].clone();
    copy.id = photosplice_doc::LayerId(doc.walk().iter().map(|(_, _, l)| l.id.0).max().unwrap_or(0) + 1);
    doc.layers.push(copy);
    doc.layers.push(text_layer("Added"));
    let bytes = export(&doc);
    let txt2 = txt2_of(&bytes).expect("Txt2 kept");
    let objs = objects(&txt2);
    assert_eq!(objs.len(), 5);
    assert_eq!(text_of(&objs[3]), text_of(&objs[0]));
    assert_eq!(text_of(&objs[4]), Some("Added\r"));
    assert_eq!(frame_refs(&objs[4]), (Some(4), Some(4)));
    assert_eq!(txt2.path(&["0", "8", "0"]).and_then(|f| f.as_array()).map(<[_]>::len), Some(5));
    let back = photosplice_io::import("t.psd", &bytes).unwrap().document;
    let texts: Vec<String> = back.walk().iter().filter_map(|(_, _, l)| if let LayerContent::Text(t) = &l.content { Some(t.text.clone()) } else { None }).collect();
    assert_eq!(texts.len(), 5);
    assert_eq!(texts.last().map(String::as_str), Some("Added"));
}

/// Resizing a Reference text box updates its `Txt2` frame's outline to the new box (it kept the
/// old one).
#[cfg(feature = "corpus")]
#[test]
fn a_resized_text_box_updates_its_txt2_frame() {
    let mut doc = oracle("paragraph-box-left.psd");
    let l = doc.layers.iter_mut().find(|l| matches!(l.content, LayerContent::Text(_))).unwrap();
    let LayerContent::Text(t) = &mut l.content else { unreachable!() };
    let photosplice_doc::text::TextShape::Box { width, .. } = &mut t.shape else { panic!("box text") };
    *width += 40.0;
    t.psd_raw = Some(Arc::new(photosplice_text::psd::build_tysh(t, 72.0, None)));
    let txt2 = txt2_of(&export(&doc)).expect("Txt2 kept");
    let pts: Vec<f64> = txt2.path(&["0", "8", "0"]).and_then(|f| f.as_array()).and_then(|f| f.first()).and_then(|f| f.path(&["0", "1", "0"])).and_then(|p| p.as_array()).unwrap().iter().filter_map(|v| v.as_f64()).collect();
    assert_eq!(pts.len(), 32);
    let right = pts.iter().step_by(2).fold(f64::MIN, |m, v| m.max(*v));
    assert!((right - 280.0).abs() < 1e-3, "frame right edge {right}");
}

/// A point layer added to a file whose only type is text on a path starts from the path object
/// with a point frame of its own, so the path text keeps its path (dropping `Txt2` turned it into
/// box text).
#[cfg(feature = "corpus")]
#[test]
fn a_new_layer_beside_path_text_keeps_the_path() {
    let mut doc = oracle("path-circle.psd");
    doc.layers.push(text_layer("Beside"));
    let bytes = export(&doc);
    let txt2 = txt2_of(&bytes).expect("Txt2 kept");
    let frames = txt2.path(&["0", "8", "0"]).and_then(|f| f.as_array()).unwrap();
    let kind = |i: usize| frames.get(i).and_then(|f| f.path(&["0", "2", "0"])).and_then(|v| v.as_i64()).unwrap_or(0);
    assert_eq!((frames.len(), kind(0), kind(1)), (2, 2, 0));
    let back = photosplice_io::import("t.psd", &bytes).unwrap().document;
    let paths: Vec<bool> = back.walk().iter().filter_map(|(_, _, l)| if let LayerContent::Text(t) = &l.content { Some(t.path.is_some()) } else { None }).collect();
    assert_eq!(paths, [true, false]);
}

/// Older files (psd-tools' `type-layer.psd`): objects without a per-paragraph view list, and
/// paragraph runs with leading type 1 (top-to-top). A text edit updates the object and keeps the
/// run's leading type, so `Txt2` is kept (it was dropped).
#[cfg(feature = "corpus")]
#[test]
fn an_edit_in_an_older_file_keeps_txt2() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/psd-tools/layers/type-layer.psd");
    let bytes = std::fs::read(&path).unwrap_or_else(|_| panic!("{} is missing: run `cargo xtask corpus --all`", path.display()));
    let mut doc = photosplice_io::import("t.psd", &bytes).unwrap().document;
    let l = doc.layers.iter_mut().find(|l| matches!(l.content, LayerContent::Text(_))).unwrap();
    let LayerContent::Text(t) = &mut l.content else { unreachable!() };
    let leading_type = |tysh: &[u8]| -> Vec<i64> {
        let e = photosplice_text::psd::engine_data(&photosplice_text::psd::parse_tysh(tysh).unwrap().text).unwrap();
        let runs = e.path(&["EngineDict", "ParagraphRun", "RunArray"]).and_then(|r| r.as_array()).unwrap().to_vec();
        runs.iter().filter_map(|r| r.path(&["ParagraphSheet", "Properties", "LeadingType"]).and_then(|v| v.as_i64())).collect()
    };
    let before = leading_type(t.psd_raw.as_deref().unwrap());
    assert!(before.contains(&1), "{before:?}");
    t.text.push('!');
    if let Some(r) = t.runs.last_mut() {
        r.len += 1;
    }
    t.sync_summary();
    t.psd_raw = Some(Arc::new(photosplice_text::psd::build_tysh(t, 72.0, None)));
    assert_eq!(leading_type(t.psd_raw.as_deref().unwrap()), before, "leading type kept");
    let txt2 = txt2_of(&export(&doc)).expect("Txt2 kept");
    assert_eq!(text_of(&objects(&txt2)[0]).map(|s| s.ends_with("!\r")), Some(true));
}

/// The first path frame's points and matrix in a written `Txt2`.
#[cfg(feature = "corpus")]
fn path_frame(txt2: &photosplice_text::engine_data::Value, i: usize) -> (Vec<f64>, Vec<f64>) {
    let f = txt2.path(&["0", "8", "0"]).and_then(|f| f.as_array()).and_then(|f| f.get(i)).unwrap();
    let reals = |p: &[&str]| f.path(p).and_then(|v| v.as_array()).unwrap().iter().filter_map(|v| v.as_f64()).collect::<Vec<f64>>();
    (reals(&["0", "1", "0"]), reals(&["0", "2", "2"]))
}

/// Moving a path-text layer moves its `Txt2` path frame with it: the document-space points by
/// the move, the document → text matrix by its inverse (the path stays put in text space).
#[cfg(feature = "corpus")]
#[test]
fn a_moved_path_text_layer_moves_its_txt2_path() {
    let mut doc = oracle("path-circle.psd");
    let (pts0, m0) = path_frame(&txt2_of(&export(&doc)).unwrap(), 0);
    let l = doc.layers.iter_mut().find(|l| matches!(l.content, LayerContent::Text(_))).unwrap();
    let LayerContent::Text(t) = &mut l.content else { unreachable!() };
    assert!(t.path.is_some());
    t.transform = photosplice_geom::Affine::translate(20.0, 10.0).mul(&t.transform);
    t.psd_raw = Some(Arc::new(photosplice_text::psd::build_tysh(t, 72.0, None)));
    let bytes = export(&doc);
    let (pts, m) = path_frame(&txt2_of(&bytes).expect("Txt2 kept"), 0);
    assert_eq!(pts.len(), pts0.len());
    for (i, (a, b)) in pts.iter().zip(&pts0).enumerate() {
        assert!((a - b - if i % 2 == 0 { 20.0 } else { 10.0 }).abs() < 1e-6, "point {i}: {a} vs {b}");
    }
    assert!((m[4] - (m0[4] - 20.0)).abs() < 1e-6 && (m[5] - (m0[5] - 10.0)).abs() < 1e-6, "{m:?} vs {m0:?}");
    let back = photosplice_io::import("t.psd", &bytes).unwrap().document;
    let LayerContent::Text(b) = &back.layers.iter().find(|l| matches!(l.content, LayerContent::Text(_))).unwrap().content else { unreachable!() };
    assert!(b.path.is_some(), "still text on a path");
}

/// A moved duplicate of a path-text layer gets its own path frame, moved with it.
#[cfg(feature = "corpus")]
#[test]
fn a_moved_copy_of_path_text_gets_a_moved_path() {
    let mut doc = oracle("path-circle.psd");
    let i = doc.layers.iter().position(|l| matches!(l.content, LayerContent::Text(_))).unwrap();
    let mut copy = doc.layers[i].clone();
    copy.id = photosplice_doc::LayerId(doc.walk().iter().map(|(_, _, l)| l.id.0).max().unwrap_or(0) + 1);
    let LayerContent::Text(t) = &mut copy.content else { unreachable!() };
    t.transform = photosplice_geom::Affine::translate(0.0, 30.0).mul(&t.transform);
    t.psd_raw = Some(Arc::new(photosplice_text::psd::build_tysh(t, 72.0, None)));
    doc.layers.push(copy);
    let txt2 = txt2_of(&export(&doc)).expect("Txt2 kept");
    let ((p0, _), (p1, _)) = (path_frame(&txt2, 0), path_frame(&txt2, 1));
    assert_eq!(p0.len(), p1.len());
    assert!(p0.iter().zip(&p1).enumerate().all(|(i, (a, b))| (b - a - if i % 2 == 0 { 0.0 } else { 30.0 }).abs() < 1e-6));
}

/// Older Reference keeps its text objects at `/DocumentObjects /TextObjects` (named keys, run
/// lengths under `/Length`) and re-renders a type layer from that copy (psd-tools' `text.psd`,
/// `pen-text.psd`). A text edit or a style-only edit updates the copy, so `Txt2` is kept (it
/// was dropped, losing the path of `pen-text.psd`'s type), the edit shows, and an attribute the
/// legacy runs can't hold still drops it.
#[cfg(feature = "corpus")]
#[test]
fn an_edit_updates_older_reference_text_objects() {
    use photosplice_text::engine_data::Value as E;
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/psd-tools");
    let read = |name: &str| {
        let bytes = std::fs::read(dir.join(name)).unwrap_or_else(|_| panic!("{name} is missing: run `cargo xtask corpus --all`"));
        photosplice_io::import(name, &bytes).unwrap().document
    };
    let entry = |txt2: &E, i: usize| txt2.path(&["DocumentObjects", "TextObjects"]).and_then(|o| o.as_array()).and_then(|o| o.get(i)).cloned().unwrap();
    let lens = |e: &E, run: &str| -> Vec<i64> {
        e.path(&["Model", run, "RunArray"]).and_then(|a| a.as_array()).unwrap().iter().filter_map(|r| r.get("Length").and_then(|v| v.as_i64())).collect()
    };
    let edit = |doc: &mut Document, f: &dyn Fn(&mut TextLayer)| {
        let l = doc.layers.iter_mut().find(|l| matches!(l.content, LayerContent::Text(_))).unwrap();
        let LayerContent::Text(t) = &mut l.content else { unreachable!() };
        f(t);
        t.sync_summary();
        t.psd_raw = Some(Arc::new(photosplice_text::psd::build_tysh(t, 72.0, None)));
    };

    // A text edit: the copy's text, its paragraph runs (one per paragraph) and the other runs.
    let mut doc = read("text.psd");
    edit(&mut doc, &|t| {
        t.text.push('!');
        if let Some(r) = t.runs.last_mut() {
            r.len += 1;
        }
        if let Some(p) = t.paragraphs.last_mut() {
            p.len += 1;
        }
    });
    let bytes = export(&doc);
    let txt2 = txt2_of(&bytes).expect("Txt2 kept");
    let e = entry(&txt2, 0);
    assert_eq!(e.path(&["Model", "Text"]).and_then(|v| v.as_str()), Some("Line 1\rLine 2\rLine 3 and text!\r"));
    assert_eq!(lens(&e, "ParagraphRun"), vec![7, 7, 17]);
    assert_eq!(lens(&e, "StyleRun").iter().sum::<i64>(), 31);
    assert_eq!(lens(&e, "KernRun"), vec![31]);
    let back = photosplice_io::import("t.psd", &bytes).unwrap().document;
    assert!(back.walk().iter().any(|(_, _, l)| matches!(&l.content, LayerContent::Text(t) if t.text.ends_with("text!"))));

    // A style-only edit: the size goes into the copy's style runs, under its legacy name.
    let mut doc = read("text.psd");
    edit(&mut doc, &|t| {
        for r in &mut t.runs {
            r.style.size_pt = 30.0;
            r.style.faux_bold = true;
        }
    });
    let txt2 = txt2_of(&export(&doc)).expect("Txt2 kept");
    let e = entry(&txt2, 0);
    let runs = e.path(&["Model", "StyleRun", "RunArray"]).and_then(|a| a.as_array()).unwrap();
    for r in runs {
        let f = r.path(&["RunData", "StyleSheet", "Features"]).unwrap();
        assert_eq!(f.get("FontSize").and_then(|v| v.as_f64()), Some(30.0), "{f:?}");
        assert_eq!(f.get("FauxBold"), Some(&E::Bool(true)));
    }
    assert_eq!(e.path(&["Model", "Text"]).and_then(|v| v.as_str()), Some("Line 1\rLine 2\rLine 3 and text\r"));

    // Text on a path keeps its Txt2 (and so its path) through an edit.
    let mut doc = read("pen-text.psd");
    edit(&mut doc, &|t| {
        t.text.push('!');
        if let Some(r) = t.runs.last_mut() {
            r.len += 1;
        }
        if let Some(p) = t.paragraphs.last_mut() {
            p.len += 1;
        }
    });
    let txt2 = txt2_of(&export(&doc)).expect("Txt2 kept");
    assert!(entry(&txt2, 0).path(&["Model", "Text"]).and_then(|v| v.as_str()).is_some_and(|s| s.ends_with("!\r")));

    // Underline has no legacy key we know: the block is dropped, and one is made from the layers.
    let mut doc = read("text.psd");
    edit(&mut doc, &|t| {
        for r in &mut t.runs {
            r.style.underline = true;
        }
    });
    assert_eq!(made_txt2_texts(&export(&doc)), ["Line 1\rLine 2\rLine 3 and text\r"]);
}

fn font_list(txt2: &photosplice_text::engine_data::Value) -> Vec<photosplice_text::engine_data::Value> {
    txt2.path(&["0", "1", "0"]).and_then(|f| f.as_array()).unwrap().iter().map(|f| f.path(&["0", "0"]).unwrap().clone()).collect()
}

fn fresh_doc(layers: Vec<Layer>) -> Document {
    let mut doc = Document::new("t", photosplice_geom::Size::new(200, 120), photosplice_color::ColorMode::Rgb, photosplice_color::SampleType::U8);
    doc.layers = layers;
    doc
}

/// A document's `Txt2` holding a variable font's instance (`/4` beside the name, as Reference
/// lists one), built like a Reference file with one type layer.
fn imported_with_instance_txt2() -> Document {
    let mut f = photosplice_psd::PsdFile::from_bytes(&export(&fresh_doc(vec![text_layer("Hello")]))).unwrap();
    let plain = real_txt2("Hello");
    let (font, with_axes): (&[u8], &[u8]) = (b"(Inter-Regular) /2 1 >>", b"(Inter-Regular) /2 1 /4 [ 26214400 ] /5 (Version 4.0) >>");
    let at = plain.windows(font.len()).position(|w| w == font).unwrap();
    let data = [&plain[..at], with_axes, &plain[at + font.len()..]].concat();
    let padding = Some(vec![0; (4 - data.len() % 4) % 4]);
    f.global_blocks.retain(|b| b.key != *b"Txt2");
    f.global_blocks.push(photosplice_psd::TaggedBlock { signature: *b"8BIM", key: *b"Txt2", data, padding });
    photosplice_io::import("t.psd", &f.to_bytes().unwrap()).unwrap().document
}

/// Type layers rebuilt from the model (here the file's own, its `TySh` gone, beside a new Optical
/// layer and a new box) in a document whose `Txt2` lists a variable font's instance: that `Txt2`
/// can't take them and is dropped, and a fresh one is made from the layers, one object and frame
/// each, the layers numbered to their objects. It reads back, Optical kerning included. A new
/// document of the same layers gets one too.
#[test]
fn rebuilt_layers_get_a_fresh_txt2() {
    let mut optical = text_layer("Optical");
    let LayerContent::Text(t) = &mut optical.content else { unreachable!() };
    t.runs[0].style.kerning = photosplice_doc::text::Kerning::Optical;
    let mut boxed = text_layer("In a box");
    let LayerContent::Text(t) = &mut boxed.content else { unreachable!() };
    t.shape = photosplice_doc::text::TextShape::Box { x: 0.0, y: 0.0, width: 150.0, height: 60.0 };
    assert_eq!(made_txt2_texts(&export(&fresh_doc(vec![optical.clone(), boxed.clone()]))), ["Optical\r", "In a box\r"], "a new document too");
    let mut doc = imported_with_instance_txt2();
    let l = &mut doc.layers[0];
    l.psd_blocks.clear();
    let LayerContent::Text(t) = &mut l.content else { panic!("text layer") };
    t.psd_raw = None;
    let next = doc.layers.iter().map(|l| l.id.0).max().unwrap_or(0);
    for (i, mut l) in [optical, boxed].into_iter().enumerate() {
        l.id = photosplice_doc::LayerId(next + 1 + i as u64);
        doc.layers.push(l);
    }
    let bytes = export(&doc);
    let txt2 = txt2_of(&bytes).expect("a fresh Txt2");
    let objs = objects(&txt2);
    assert_eq!(objs.iter().map(text_of).collect::<Vec<_>>(), [Some("Hello\r"), Some("Optical\r"), Some("In a box\r")]);
    for (i, o) in objs.iter().enumerate() {
        assert_eq!(frame_refs(o), (Some(i as i64), Some(i as i64)));
        assert_eq!(tysh_index(&bytes, i), Some(i as i64));
    }
    let frames = txt2.path(&["0", "8", "0"]).and_then(|f| f.as_array()).unwrap();
    assert_eq!(frames.iter().map(|f| f.path(&["0", "2", "0"]).and_then(|v| v.as_i64()).unwrap_or(0)).collect::<Vec<_>>(), [0, 0, 1]);
    let names: Vec<String> = font_list(&txt2).iter().map(|f| f.get("0").and_then(|v| v.as_str()).unwrap().to_string()).collect();
    assert_eq!(names, ["Inter-Regular", "MyriadPro-Regular", "AdobeInvisFont", "TimesNewRomanPSMT"]);
    let back = photosplice_io::import("t.psd", &bytes).unwrap().document;
    let LayerContent::Text(b) = &back.layers[1].content else { panic!("text layer") };
    assert_eq!((b.text.as_str(), b.char_runs()[0].style.kerning), ("Optical", photosplice_doc::text::Kerning::Optical));
    let LayerContent::Text(b) = &back.layers[2].content else { panic!("text layer") };
    assert!(matches!(b.shape, photosplice_doc::text::TextShape::Box { .. }) && b.text == "In a box");
}

/// A type layer that lost its `TySh` (rebuilt from the model) in a document whose `Txt2` lists its
/// variable font's instance: that `Txt2` can't take the layer and is dropped, and the fresh one
/// keeps the instance's entry, axes included (Reference opened the font's default instance
/// without it: Reference kit 7, `SourceSerifRoman_389.344wght` for `SourceSerifRoman-Regular`).
#[test]
fn a_rebuilt_layer_keeps_its_variable_instance_in_a_fresh_txt2() {
    let mut doc = imported_with_instance_txt2();
    assert!(txt2_of(&export(&doc)).is_some(), "the file's own Txt2 is kept while unedited");
    let l = &mut doc.layers[0];
    l.psd_blocks.clear();
    let LayerContent::Text(t) = &mut l.content else { panic!("text layer") };
    t.psd_raw = None;
    let bytes = export(&doc);
    let txt2 = txt2_of(&bytes).expect("a fresh Txt2");
    let first = font_list(&txt2)[0].clone();
    assert_eq!(first.get("0").and_then(|v| v.as_str()), Some("Inter-Regular"));
    assert_eq!(first.get("4").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|v| v.as_i64()).collect::<Vec<_>>()), Some(vec![26_214_400]));
    assert_eq!(first.get("5").and_then(|v| v.as_str()), Some("Version 4.0"));
    assert_eq!(objects(&txt2).len(), 1);
    assert_eq!(tysh_index(&bytes, 0), Some(0));
}

/// A new layer in a font a Reference `Txt2` doesn't list yet: the entry added for it carries what
/// the font says (here the bundled Inter's version string); Reference's own entries stay as they
/// were.
#[test]
fn a_font_added_to_a_kept_txt2_gets_its_details() {
    let mut doc = imported_with_real_txt2();
    let mut added = text_layer("Added");
    let LayerContent::Text(t) = &mut added.content else { unreachable!() };
    t.runs[0].style.font_family = "JetBrains Mono".into();
    added.id = photosplice_doc::LayerId(doc.layers.iter().map(|l| l.id.0).max().unwrap_or(0) + 1);
    doc.layers.push(added);
    let txt2 = txt2_of(&export(&doc)).expect("Txt2 kept");
    let list = font_list(&txt2);
    assert_eq!(list[0].get("5"), None, "Reference's entry unchanged");
    let mono = list.iter().find(|f| f.get("0").and_then(|v| v.as_str()).is_some_and(|n| n.starts_with("JetBrainsMono"))).expect("the new font listed");
    assert!(mono.get("5").and_then(|v| v.as_str()).is_some_and(|v| v.starts_with("Version")), "{mono:?}");
    assert_eq!(mono.get("4"), None, "a static face has no axes");
}

/// A `Txt2` run array's value of `key` (under `style`, a path inside each run) at every UTF-16
/// unit, the run lengths at `/1`.
#[cfg(feature = "corpus")]
fn per_unit(runs: &[photosplice_text::engine_data::Value], style: &[&str], key: &str) -> Vec<Option<photosplice_text::engine_data::Value>> {
    let mut out = Vec::new();
    for r in runs {
        let n = r.get("1").and_then(|v| v.as_i64()).unwrap_or(0);
        let v = r.path(style).and_then(|s| s.get(key)).cloned();
        out.extend(std::iter::repeat_n(v, usize::try_from(n).unwrap_or(0)));
    }
    out
}

#[cfg(feature = "corpus")]
fn same(a: &photosplice_text::engine_data::Value, b: &photosplice_text::engine_data::Value) -> bool {
    use photosplice_text::engine_data::Value as E;
    match (a, b) {
        (E::Dict(x), E::Dict(y)) => x.len() == y.len() && x.iter().all(|(k, v)| b.get(k).is_some_and(|w| same(v, w))),
        (E::Array(x), E::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(v, w)| same(v, w)),
        _ => match (a.as_f64(), b.as_f64()) {
            (Some(x), Some(y)) => (x - y).abs() <= 1e-3 * x.abs().max(1.0),
            _ => a == b,
        },
    }
}

/// The oracle check of a fresh `Txt2`: made from each Reference type layer's own `TySh`, every
/// text object says what Reference's says, run for run, on every style key the patching maps
/// (the font by name, size, faux styles, leading, scales, tracking, baseline, kerning mode, caps,
/// position, underline, strikethrough, ligatures, the OpenType flags, baseline direction, fill),
/// and on the paragraph keys Reference wrote.
#[cfg(feature = "corpus")]
#[test]
fn a_fresh_txt2_says_what_reference_wrote() {
    use photosplice_text::engine_data::Value as E;
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/photoshop/text");
    let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap_or_else(|_| panic!("{} is missing: run `cargo xtask corpus --all`", dir.display())).flatten().map(|e| e.path()).collect();
    files.sort();
    let style_keys = ["1", "2", "3", "4", "6", "7", "8", "9", "11", "12", "13", "15", "16", "18", "19", "20", "23", "24", "25", "26", "28", "29", "35", "37", "53"];
    let mut problems = Vec::new();
    let mut checked = 0;
    for path in files.iter().filter(|p| p.extension().is_some_and(|x| x == "psd")) {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let bytes = std::fs::read(path).unwrap();
        let doc = photosplice_io::import(&name, &bytes).unwrap().document;
        let Some(theirs) = doc.metadata.psd_global_blocks.iter().find(|(_, k, _)| k == b"Txt2").and_then(|(_, _, d)| photosplice_text::psd::parse_txt2(d)) else { continue };
        let layers: Vec<(TextLayer, Vec<u8>, usize)> = doc
            .walk()
            .iter()
            .filter_map(|(_, _, l)| match &l.content {
                LayerContent::Text(t) => {
                    let raw = t.psd_raw.as_deref()?;
                    Some((t.clone(), raw.to_vec(), usize::try_from(photosplice_text::psd::text_index(raw)?).ok()?))
                }
                _ => None,
            })
            .collect();
        let renumbered: Vec<Vec<u8>> = layers.iter().enumerate().map(|(i, (_, raw, _))| photosplice_text::psd::set_text_index(raw, i as i64).unwrap()).collect();
        let objects: Vec<(&TextLayer, &[u8])> = layers.iter().zip(&renumbered).map(|((l, _, _), w)| (l, w.as_slice())).collect();
        let ours = photosplice_text::psd::new_txt2(&objects, None, &mut |_, _| Default::default()).unwrap_or_else(|| panic!("{name}: no Txt2"));
        let font_names = |t: &E| -> Vec<String> { t.path(&["0", "1", "0"]).and_then(|f| f.as_array()).unwrap().iter().map(|f| f.path(&["0", "0", "0"]).and_then(|v| v.as_str()).unwrap_or("").to_string()).collect() };
        let (their_fonts, our_fonts) = (font_names(&theirs), font_names(&ours));
        let (our_objects, their_objects) = (objects_of(&ours), objects_of(&theirs));
        for (i, (_, _, k)) in layers.iter().enumerate() {
            let (Some(a), Some(b)) = (our_objects.get(i), their_objects.get(*k)) else { continue };
            checked += 1;
            if text_of(a) != text_of(b) {
                problems.push(format!("{name} #{k}: text {:?} vs {:?}", text_of(a), text_of(b)));
                continue;
            }
            let runs = |o: &E, p: &str| o.path(&["0", p, "0"]).and_then(|r| r.as_array()).unwrap().to_vec();
            let (ours_s, theirs_s) = (runs(a, "6"), runs(b, "6"));
            let font = |o: &[E], names: &[String]| -> Vec<Option<String>> {
                per_unit(o, &["0", "0", "6"], "0").into_iter().map(|v| v.and_then(|v| v.as_i64()).and_then(|i| names.get(i as usize).cloned())).collect()
            };
            if font(&ours_s, &our_fonts) != font(&theirs_s, &their_fonts) {
                problems.push(format!("{name} #{k}: fonts"));
            }
            for key in style_keys {
                let (x, y) = (per_unit(&ours_s, &["0", "0", "6"], key), per_unit(&theirs_s, &["0", "0", "6"], key));
                let bad = x.iter().zip(&y).position(|(p, q)| match (p, q) {
                    (Some(p), Some(q)) => !same(p, q),
                    _ => false,
                });
                if x.len() != y.len() || bad.is_some() {
                    problems.push(format!("{name} #{k}: style /{key} at {bad:?}: {:?} vs {:?}", bad.and_then(|i| x.get(i)), bad.and_then(|i| y.get(i))));
                }
            }
            let (ours_p, theirs_p) = (runs(a, "5"), runs(b, "5"));
            for r in &theirs_p {
                let Some(E::Dict(props)) = r.path(&["0", "0", "5"]) else { continue };
                for (key, _) in props {
                    let (x, y) = (per_unit(&ours_p, &["0", "0", "5"], key), per_unit(&theirs_p, &["0", "0", "5"], key));
                    let bad = x.iter().zip(&y).position(|(p, q)| matches!((p, q), (Some(p), Some(q)) if !same(p, q)));
                    if let Some(i) = bad {
                        problems.push(format!("{name} #{k}: paragraph /{key}: {:?} vs {:?}", x.get(i), y.get(i)));
                    }
                }
            }
        }
    }
    problems.sort();
    problems.dedup();
    assert!(checked > 50, "{checked} objects checked");
    assert!(problems.is_empty(), "{} differences:\n{}", problems.len(), problems.join("\n"));
}

#[cfg(feature = "corpus")]
fn objects_of(txt2: &photosplice_text::engine_data::Value) -> Vec<photosplice_text::engine_data::Value> {
    txt2.path(&["1", "1"]).and_then(|o| o.as_array()).map(<[_]>::to_vec).unwrap_or_default()
}

/// Reference kit 7's `regen` of a variable-font oracle: the type layer rebuilt from the model in
/// a file whose `Txt2` can't take it. The fresh block keeps Reference's entry for the instance
/// (`SourceSerifRoman-Regular` at wght 400), so Reference opens that instance again rather than
/// the font's default (`SourceSerifRoman_389.344wght`), and the layer reads back as it was.
#[cfg(feature = "corpus")]
#[test]
fn a_regenerated_variable_font_layer_keeps_its_instance() {
    let mut doc = oracle("opentype-fractions.psd");
    let mut want = None;
    for l in &mut doc.layers {
        l.psd_blocks.clear();
        if let LayerContent::Text(t) = &mut l.content {
            t.psd_raw = None;
            want = Some(t.clone());
        }
    }
    let want = want.expect("a type layer");
    let bytes = export(&doc);
    let txt2 = txt2_of(&bytes).expect("a fresh Txt2");
    let first = font_list(&txt2)[0].clone();
    assert_eq!(first.get("0").and_then(|v| v.as_str()), Some("SourceSerifRoman-Regular"));
    assert_eq!(first.get("4"), Some(&photosplice_text::engine_data::Value::Array(vec![photosplice_text::engine_data::Value::Int(26_214_400)])));
    let back = photosplice_io::import("t.psd", &bytes).unwrap().document;
    let b = back.walk().iter().find_map(|(_, _, l)| if let LayerContent::Text(t) = &l.content { Some(t.clone()) } else { None }).unwrap();
    assert_eq!(b.text, want.text);
    let frac = |t: &TextLayer| t.char_runs().iter().map(|r| r.style.features.iter().any(|f| f.tag == "frac" && f.value > 0)).collect::<Vec<_>>();
    assert_eq!(frac(&b), frac(&want));
}
