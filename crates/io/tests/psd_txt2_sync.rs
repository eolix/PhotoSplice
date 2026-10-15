//! The document's `Txt2` (Photoshop's document-wide copy of every type layer) must describe the
//! type layers: Photoshop reads it in preference to each layer's `TySh`, so a text edit written
//! with the old `Txt2` reverted in Photoshop (verified 2026-10-12). An edited layer's object is
//! updated, a new or duplicated layer gets an object of its own; when that can't be done (a style
//! its runs don't have, no object to start from), `Txt2` is dropped and the editor rebuilds it
//! from the layers.

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
    assert!(!has_txt2(&bytes), "Txt2 dropped");
    let back = photosplice_io::import("t.psd", &bytes).unwrap().document;
    let LayerContent::Text(b) = &back.layers[0].content else { panic!("text layer") };
    assert_eq!(b.text, "Hello!");
}

/// A `Txt2` whose objects can't be read (here none at all) can't take a new or duplicated layer.
#[test]
fn new_and_duplicated_type_layers_drop_an_unreadable_txt2() {
    // A new type layer has no object of its own in Txt2.
    let mut doc = imported_with_txt2();
    doc.layers.push(text_layer("New"));
    assert!(!has_txt2(&export(&doc)));
    // A duplicate shares its source's object: editing either would go stale.
    let mut doc = imported_with_txt2();
    let mut copy = doc.layers[0].clone();
    copy.id = photosplice_doc::LayerId(doc.layers.iter().map(|l| l.id.0).max().unwrap_or(0) + 1);
    doc.layers.push(copy);
    assert!(!has_txt2(&export(&doc)));
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
