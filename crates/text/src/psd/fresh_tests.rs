use super::*;
use crate::psd::{apply_txt2, build_tysh, parse_txt2, set_text_index, text_layer_from_tysh, txt2_object_count};
use photosplice_color::Color;
use photosplice_doc::text::{FontFeature, Kerning, ParagraphRun, ParagraphStyle, TextAlign, TextRun};

fn layer(text: &str, runs: Vec<(usize, CharStyle)>) -> TextLayer {
    TextLayer { text: text.into(), runs: runs.into_iter().map(|(len, style)| TextRun { len, style }).collect(), ..Default::default() }
}

fn written(l: &TextLayer, index: usize) -> Vec<u8> {
    set_text_index(&build_tysh(l, 72.0, None), i64::try_from(index).unwrap()).unwrap()
}

fn serif() -> CharStyle {
    CharStyle {
        font_family: "Source Serif 4".into(),
        postscript_name: Some("SourceSerifRoman-Regular".into()),
        font_type: Some(("SourceSerifRoman-Regular".into(), 0)),
        size_pt: 30.0,
        ..Default::default()
    }
}

/// A stub font database: Source Serif's Regular instance is a variable face at wght 400.
fn stub(calls: &mut Vec<String>) -> impl FnMut(&CharStyle, &str) -> FontDetails + '_ {
    move |_, name| {
        calls.push(name.to_string());
        if name == "SourceSerifRoman-Regular" { FontDetails { axes: vec![400.0], version: Some("Version 4.005".into()) } } else { FontDetails::default() }
    }
}

fn fonts(txt2: &E) -> Vec<(String, E)> {
    txt2.path(&["0", "1", "0"])
        .and_then(E::as_array)
        .unwrap()
        .iter()
        .map(|f| (f.path(&["0", "0", "0"]).and_then(E::as_str).unwrap().to_string(), f.path(&["0", "0"]).unwrap().clone()))
        .collect()
}

/// The block made for two new layers reads back (through the importer's own `Txt2` reader) with
/// each layer's text, kerning modes and OpenType settings, one object and one frame per layer.
#[test]
fn a_fresh_txt2_reads_back_text_kerning_and_features() {
    let optical = CharStyle { kerning: Kerning::Optical, features: vec![FontFeature { tag: "frac".into(), value: 1 }], ..serif() };
    let red = CharStyle { color: Color::rgb(1.0, 0.0, 0.0), size_pt: 18.0, ..serif() };
    let a = layer("1/2 cup\nof tea", vec![(8, optical.clone()), (6, red.clone())]);
    let boxed = TextLayer {
        shape: TextShape::Box { x: 0.0, y: 0.0, width: 200.0, height: 80.0 },
        orientation: Orientation::Vertical,
        paragraphs: vec![ParagraphRun { len: 5, style: ParagraphStyle { align: TextAlign::Center, ..Default::default() } }],
        ..layer("Boxed", vec![(5, CharStyle::default())])
    };
    let (wa, wb) = (written(&a, 0), written(&boxed, 1));
    let mut calls = Vec::new();
    let txt2 = new_txt2(&[(&a, &wa), (&boxed, &wb)], None, &mut stub(&mut calls)).unwrap();
    let bytes = ed::write_txt2(&txt2);
    assert_eq!(txt2_object_count(&bytes), Some(2));
    let back = parse_txt2(&bytes).unwrap();
    assert_eq!(back.path(&["0", "8", "0"]).and_then(E::as_array).map(<[E]>::len), Some(2), "one frame per object");
    for (want, w) in [(&a, &wa), (&boxed, &wb)] {
        let mut l = text_layer_from_tysh(w, 72.0).unwrap();
        apply_txt2(&mut l, w, &back);
        assert_eq!(l.text, want.text);
        let kerning = |t: &TextLayer| t.char_runs().iter().map(|r| r.style.kerning).collect::<Vec<_>>();
        assert_eq!(kerning(&l), kerning(want));
        assert!(l.char_runs().first().is_some_and(|r| r.style.features.iter().any(|f| f.tag == "frac" && f.value > 0) == (want.text == a.text)));
    }
    // Style and paragraph run values, as Reference keys them.
    let obj = back.path(&["1", "1"]).and_then(E::as_array).unwrap();
    let style = |o: usize, r: usize, key: &str| obj[o].path(&["0", "6", "0"]).and_then(E::as_array).unwrap()[r].path(&["0", "0", "6", key]).cloned();
    assert_eq!(style(0, 0, "1").and_then(|v| v.as_f64()), Some(30.0));
    assert_eq!(style(0, 0, "11").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(style(0, 0, "23"), Some(E::Bool(true)));
    let last = obj[0].path(&["0", "6", "0"]).and_then(E::as_array).unwrap().len() - 1;
    assert_eq!(style(0, last, "1").and_then(|v| v.as_f64()), Some(18.0));
    assert_eq!(style(0, last, "23"), Some(E::Bool(false)), "Fractions only where the layer has it");
    assert_eq!(style(0, last, "11").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(style(0, last, "53").and_then(|p| p.path(&["0", "1"]).cloned()), Some(E::Array(vec![E::Real(1.0), E::Real(1.0), E::Real(0.0), E::Real(0.0)])));
    let paras = obj[0].path(&["0", "5", "0"]).and_then(E::as_array).unwrap();
    assert_eq!(paras.iter().map(|p| p.get("1").and_then(E::as_i64).unwrap()).collect::<Vec<_>>(), vec![8, 7], "one run per paragraph");
    let centred = obj[1].path(&["0", "5", "0"]).and_then(E::as_array).unwrap()[0].path(&["0", "0", "5", "0"]).and_then(E::as_i64);
    assert_eq!(centred, Some(2));
    // Each view names its own frame; the box frame outlines the box, vertical type says so.
    for (i, o) in obj.iter().enumerate() {
        assert_eq!(o.path(&["1", "0"]).and_then(E::as_array).and_then(|f| f.first()).and_then(|f| f.get("0")).and_then(E::as_i64), Some(i as i64));
        let strike = o.path(&["1", "2"]).and_then(E::as_array).and_then(|s| s.first()).and_then(|s| s.get("6")).and_then(E::as_array).and_then(|s| s.first());
        assert_eq!(strike.and_then(|s| s.get("10")).and_then(E::as_i64), Some(i as i64));
    }
    let frame = back.path(&["0", "8", "0"]).and_then(E::as_array).unwrap()[1].get("0").unwrap();
    assert_eq!(frame.path(&["2", "0"]).and_then(E::as_i64), Some(1));
    assert_eq!(frame.path(&["2", "1"]).and_then(E::as_i64), Some(2));
    assert_eq!(frame.path(&["1", "0"]).and_then(E::as_array).map(|p| p.iter().filter_map(E::as_f64).fold(0.0f64, f64::max)), Some(200.0));
    assert_eq!(calls, vec!["SourceSerifRoman-Regular".to_string(), "Inter-Regular".to_string()], "each face looked up once");
}

/// A variable font's instance is kept beside its name in the font list (`/4`, × 65536, with the
/// font type from `FontSet`), the faces Reference always lists follow, and the defaults point at
/// them.
#[test]
fn a_fresh_txt2_lists_a_variable_instance_with_its_axes() {
    let a = layer("1/2", vec![(3, serif())]);
    let w = written(&a, 0);
    let mut calls = Vec::new();
    let txt2 = new_txt2(&[(&a, &w)], None, &mut stub(&mut calls)).unwrap();
    let list = fonts(&txt2);
    let names: Vec<&str> = list.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["SourceSerifRoman-Regular", "MyriadPro-Regular", "AdobeInvisFont", "TimesNewRomanPSMT"]);
    let first = &list[0].1;
    assert_eq!(first.get("2").and_then(E::as_i64), Some(0));
    assert_eq!(first.get("4"), Some(&E::Array(vec![E::Int(26_214_400)])));
    assert_eq!(first.get("5").and_then(E::as_str), Some("Version 4.005"));
    let keys: Vec<&str> = match first {
        E::Dict(d) => d.iter().map(|(k, _)| k.as_str()).collect(),
        _ => Vec::new(),
    };
    assert_eq!(keys, ["0", "2", "4", "5"], "Reference's key order");
    assert_eq!(txt2.path(&["0", "5", "0"]).and_then(E::as_array).unwrap()[0].path(&["0", "6", "0"]).and_then(E::as_i64), Some(1), "normal style in Myriad");
    assert_eq!(txt2.path(&["1", "0", "0", "0"]).and_then(E::as_i64), Some(2), "hidden glyphs in AdobeInvisFont");
    let run_font = txt2.path(&["1", "1"]).and_then(E::as_array).unwrap()[0].path(&["0", "6", "0"]).and_then(E::as_array).unwrap()[0]
        .path(&["0", "0", "6", "0"])
        .and_then(E::as_i64);
    assert_eq!(run_font, Some(0));
    // The font-use record lists the layer font's version.
    let record = txt2.path(&["1", "5"]).and_then(E::as_array).unwrap()[0].get("4").and_then(E::as_array).unwrap().to_vec();
    assert_eq!(record, vec![dict(vec![("0", E::Int(0)), ("1", E::String("Version 4.005".into()))])]);
    // Written as Reference writes the block: no enclosing dictionary, one token after another.
    let bytes = ed::write_txt2(&txt2);
    assert!(bytes.starts_with(b" /98 << /0 14 >> /0 << /1 << /0 [ << /0 << /99 /CoolTypeFont"));
}

/// A face the document's earlier block already lists keeps Reference's entry (its version and
/// axes), and nobody is asked about it.
#[test]
fn a_fresh_txt2_keeps_an_earlier_blocks_font_entry() {
    let a = layer("Hi", vec![(2, serif())]);
    let w = written(&a, 0);
    let theirs = font_list_entry("SourceSerifRoman-Regular", 0, 0, 0, &FontDetails { axes: vec![400.0], version: Some("Version 3.001;hotconv".into()) });
    let previous = dict(vec![("0", dict(vec![("1", dict(vec![("0", E::Array(vec![theirs.clone()]))]))]))]);
    let mut calls = Vec::new();
    let txt2 = new_txt2(&[(&a, &w)], Some(&previous), &mut stub(&mut calls)).unwrap();
    assert_eq!(txt2.path(&["0", "1", "0"]).and_then(E::as_array).and_then(|l| l.first()), Some(&theirs));
    assert!(calls.is_empty());
}

/// Only the entries a save added get axes and versions, in Reference's key order; Reference's
/// own entries are left alone.
#[test]
fn filling_fonts_touches_only_added_entries() {
    let a = layer("Hi", vec![(2, serif())]);
    let reference = font_list_entry("SourceSerifRoman-Regular", 0, 0, 0, &FontDetails::default());
    let added = font_list_entry("SourceSerifRoman-Regular", 1, 0, 2, &FontDetails::default());
    let mut txt2 = dict(vec![("0", dict(vec![("1", dict(vec![("0", E::Array(vec![reference.clone(), added]))]))]))]);
    let mut calls = Vec::new();
    fill_txt2_fonts(&mut txt2, 1, &[&a], &mut stub(&mut calls));
    let list = fonts(&txt2);
    assert_eq!(txt2.path(&["0", "1", "0"]).and_then(E::as_array).and_then(|l| l.first()), Some(&reference));
    let keys: Vec<&str> = match &list[1].1 {
        E::Dict(d) => d.iter().map(|(k, _)| k.as_str()).collect(),
        _ => Vec::new(),
    };
    assert_eq!(keys, ["0", "1", "2", "3", "4", "5"]);
    assert_eq!(list[1].1.get("4"), Some(&E::Array(vec![E::Int(26_214_400)])));
    // A face no layer names, or one already described, is not looked up.
    let mut calls = Vec::new();
    fill_txt2_fonts(&mut txt2, 0, &[], &mut stub(&mut calls));
    assert!(calls.is_empty());
}

/// Bad input gives `None`, never a panic: no layers, a layer not numbered by its position, a
/// `TySh` that doesn't parse.
#[test]
fn a_fresh_txt2_refuses_what_it_cannot_describe() {
    let a = layer("Hi", vec![(2, CharStyle::default())]);
    let mut calls = Vec::new();
    let mut d = stub(&mut calls);
    assert!(new_txt2(&[], None, &mut d).is_none());
    let w = written(&a, 3);
    assert!(new_txt2(&[(&a, &w)], None, &mut d).is_none());
    assert!(new_txt2(&[(&a, b"\0\x01junk")], None, &mut d).is_none());
    // A layer whose runs overrun its text still gets a block.
    let odd = layer("Hi", vec![(9, CharStyle::default())]);
    let w = written(&odd, 0);
    assert!(new_txt2(&[(&odd, &w)], None, &mut d).is_some());
}
