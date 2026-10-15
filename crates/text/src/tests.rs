use photosplice_color::{Color, PixelFormat, SampleType};
use photosplice_doc::TextLayer;
use photosplice_doc::text::{CharStyle, FontFeature, Orientation, ParagraphRun, ParagraphStyle, TextAlign, TextDirection, TextRun, TextShape};
use photosplice_geom::Affine;

use crate::{TextEngine, fonts};

fn point(text: &str, size_pt: f32) -> TextLayer {
    TextLayer { text: text.into(), font_family: "Inter".into(), size_pt, ..Default::default() }
}

fn styled(text: &str, style: CharStyle) -> TextLayer {
    TextLayer { text: text.into(), runs: vec![TextRun { len: text.len(), style }], ..Default::default() }
}

fn with_para(mut t: TextLayer, p: ParagraphStyle) -> TextLayer {
    t.paragraphs = vec![ParagraphRun { len: t.text.len(), style: p }];
    t
}

fn width(l: &crate::TextLayout) -> f32 {
    l.lines.iter().map(|l| l.x1 - l.x0).fold(0.0, f32::max)
}

fn alpha_sum(s: &photosplice_raster::Surface, r: photosplice_geom::Rect) -> f64 {
    let n = s.channels();
    s.read_region(r).chunks_exact(n).map(|p| f64::from(p[n - 1])).sum()
}

#[test]
fn bundled_fonts_cover_latin() {
    let mut e = TextEngine::new();
    let fams = e.fonts.families();
    assert!(fams.iter().any(|f| f == "Inter"), "{fams:?}");
    assert!(fams.iter().any(|f| f == "JetBrains Mono"), "{fams:?}");
    let l = e.layout(&point("Hello, Wörld! 0123", 12.0), 72.0);
    assert_eq!(l.glyphs.len(), "Hello, Wörld! 0123".chars().count());
    assert!(l.glyphs.iter().all(|g| g.id != 0), "no .notdef");
    assert_eq!(e.fonts.faces("Inter").len(), 3);
}

#[test]
fn metrics_are_stable_and_scale_with_dpi() {
    let mut e = TextEngine::new();
    let a = e.layout(&point("Hamburgefonstiv", 12.0), 72.0);
    let w = width(&a);
    // Inter 12 px: stable to a tenth of a pixel across runs.
    let again = width(&e.layout(&point("Hamburgefonstiv", 12.0), 72.0));
    assert_eq!(w, again);
    assert!((80.0..110.0).contains(&w), "{w}");
    let b = e.layout(&point("Hamburgefonstiv", 12.0), 144.0);
    assert!((width(&b) - 2.0 * w).abs() < 0.05, "{} vs {}", width(&b), 2.0 * w);
    // Point text: first baseline at the anchor.
    assert_eq!(a.lines[0].baseline, 0.0);
    assert!(a.lines[0].ascent > 8.0 && a.lines[0].ascent < 13.0);
}

#[test]
fn auto_and_explicit_leading() {
    let mut e = TextEngine::new();
    let l = e.layout(&point("one\ntwo\rthree", 10.0), 72.0);
    assert_eq!(l.lines.len(), 3);
    assert!((l.lines[1].baseline - 12.0).abs() < 1e-3, "auto leading = 1.2 × size");
    assert!((l.lines[2].baseline - 24.0).abs() < 1e-3);
    assert_eq!(&"one\ntwo\rthree"[l.lines[2].range.clone()], "three");
    let t = styled("a\nb", CharStyle { size_pt: 10.0, leading_pt: Some(30.0), ..Default::default() });
    let l = e.layout(&t, 144.0);
    assert!((l.lines[1].baseline - 60.0).abs() < 1e-3, "{}", l.lines[1].baseline);
    // Space before/after in points.
    let t = with_para(point("a\nb", 10.0), ParagraphStyle { space_after_pt: 5.0, ..Default::default() });
    let l = e.layout(&t, 72.0);
    assert!((l.lines[1].baseline - 17.0).abs() < 1e-3, "{}", l.lines[1].baseline);
}

#[test]
fn box_text_wraps_inside_width() {
    let mut e = TextEngine::new();
    let mut t = point("The quick brown fox jumps over the lazy dog again and again", 12.0);
    t.shape = TextShape::Box { x: 10.0, y: 20.0, width: 100.0, height: 1000.0 };
    let l = e.layout(&t, 72.0);
    assert!(l.lines.len() >= 3, "{}", l.lines.len());
    for line in &l.lines {
        assert!(line.x0 >= 10.0 - 1e-3 && line.x1 <= 110.0 + 1e-3, "{line:?}");
    }
    // First baseline = top + ascender height ('d'), a bit less than the hhea ascent.
    let drop = l.lines[0].baseline - 20.0;
    assert!(drop > 0.65 * 12.0 && drop < l.lines[0].ascent, "{drop}");
    // Lines cover the text in order.
    assert_eq!(l.lines[0].range.start, 0);
    for w in l.lines.windows(2) {
        assert!(w[1].range.start >= w[0].range.end);
    }
    // Overflowing lines are hidden, like Photoshop.
    t.shape = TextShape::Box { x: 0.0, y: 0.0, width: 100.0, height: 30.0 };
    let l2 = e.layout(&t, 72.0);
    assert_eq!(l2.lines.len(), 2);
}

#[test]
fn alignment_point_and_box() {
    let mut e = TextEngine::new();
    let c = e.layout(&with_para(point("Centered", 20.0), ParagraphStyle { align: TextAlign::Center, ..Default::default() }), 72.0);
    let ln = &c.lines[0];
    assert!((ln.x0 + ln.x1).abs() < 0.01, "centered on anchor: {ln:?}");
    let r = e.layout(&with_para(point("Right", 20.0), ParagraphStyle { align: TextAlign::Right, ..Default::default() }), 72.0);
    assert!(r.lines[0].x1.abs() < 0.01 && r.lines[0].x0 < -10.0);

    let mut t = point("aaa bbb ccc ddd eee fff ggg hhh iii jjj kkk", 12.0);
    t.shape = TextShape::Box { x: 0.0, y: 0.0, width: 120.0, height: 500.0 };
    let t = with_para(t, ParagraphStyle { align: TextAlign::JustifyLeft, ..Default::default() });
    let l = e.layout(&t, 72.0);
    assert!(l.lines.len() > 1);
    let first = &l.lines[0];
    // Justified line: last glyph ends at the right edge (within a pixel).
    let last_glyph_x = l.glyphs.iter().filter(|g| (g.y - first.baseline).abs() < 1e-3).map(|g| g.x).fold(f32::MIN, f32::max);
    assert!(last_glyph_x > 110.0, "{last_glyph_x}");
    let right = with_para(
        TextLayer { shape: TextShape::Box { x: 0.0, y: 0.0, width: 200.0, height: 100.0 }, ..point("end", 12.0) },
        ParagraphStyle { align: TextAlign::Right, ..Default::default() },
    );
    let l = e.layout(&right, 72.0);
    assert!((l.lines[0].x1 - 200.0).abs() < 0.5, "{:?}", l.lines[0]);
}

#[test]
fn rtl_text_is_reordered() {
    let mut e = TextEngine::new();
    // Hebrew isn't in the bundled fonts, but bidi still resolves (glyphs may be .notdef).
    let text = "abc שלום def";
    let l = e.layout(&point(text, 12.0), 72.0);
    let heb: Vec<_> = l.clusters.iter().filter(|c| text[c.range.clone()].chars().all(|ch| ('\u{0590}'..='\u{05FF}').contains(&ch))).collect();
    assert_eq!(heb.len(), 4);
    assert!(heb.iter().all(|c| c.rtl));
    // Visual order: the first logical Hebrew letter is rightmost among them.
    let first = heb.iter().min_by_key(|c| c.range.start).unwrap();
    assert!(heb.iter().all(|c| c.x <= first.x + 1e-3));
    // Forced RTL paragraph: the Latin run ends up to the right of the Hebrew.
    let t = with_para(point("שלום abc", 12.0), ParagraphStyle { direction: TextDirection::Rtl, ..Default::default() });
    let l = e.layout(&t, 72.0);
    let a = l.clusters.iter().find(|c| &"שלום abc"[c.range.clone()] == "a").unwrap();
    let shin = l.clusters.iter().find(|c| c.range.start == 0).unwrap();
    assert!(a.x < shin.x, "abc left of the Hebrew in an RTL paragraph");
    assert!(l.clusters.iter().all(|c| c.range.end <= "שלום abc".len()));
}

/// Builds a TrueType collection from standalone fonts (table offsets rebased).
fn make_ttc(fonts: &[&[u8]]) -> Vec<u8> {
    let header_len = 12 + 4 * fonts.len();
    let mut out = Vec::new();
    out.extend_from_slice(b"ttcf");
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&(fonts.len() as u32).to_be_bytes());
    let mut offsets = Vec::new();
    let mut body: Vec<u8> = Vec::new();
    for f in fonts {
        while !(header_len + body.len()).is_multiple_of(4) {
            body.push(0);
        }
        let base = (header_len + body.len()) as u32;
        offsets.push(base);
        let mut copy = f.to_vec();
        let n = u16::from_be_bytes([copy[4], copy[5]]) as usize;
        for i in 0..n {
            let at = 12 + i * 16 + 8;
            let off = u32::from_be_bytes(copy[at..at + 4].try_into().unwrap());
            copy[at..at + 4].copy_from_slice(&(off + base).to_be_bytes());
        }
        body.extend(copy);
    }
    for o in offsets {
        out.extend_from_slice(&o.to_be_bytes());
    }
    out.extend(body);
    out
}

#[test]
fn truetype_collections_load() {
    let ttc = make_ttc(&[fonts::BUNDLED[0].1, fonts::BUNDLED[3].1]);
    assert_eq!(fonts::face_count(&ttc), 2);
    let mut db = fonts::FontDb::new();
    // A fresh DB without the bundled mono font would miss it; register the collection anyway
    // and check both faces are indexed (families already present gain faces).
    let before = db.faces("JetBrains Mono").len();
    let names = db.register_font_data(ttc);
    assert!(names.iter().any(|n| n == "Inter") && names.iter().any(|n| n == "JetBrains Mono"), "{names:?}");
    assert_eq!(db.faces("JetBrains Mono").len(), before + 1);
    // Layout with the TTC-backed face works.
    let mut e = TextEngine { fonts: db, layouter: crate::layout::Layouter::new() };
    let t = styled("mono", CharStyle { font_family: "JetBrains Mono".into(), ..Default::default() });
    let l = e.layout(&t, 72.0);
    assert!(l.glyphs.iter().all(|g| g.id != 0));
}

#[test]
fn opentype_features_and_tracking() {
    let mut e = TextEngine::new();
    let ids = |e: &mut TextEngine, feats: Vec<FontFeature>, lig: bool| {
        let t = styled("a0", CharStyle { features: feats, ligatures: lig, ..Default::default() });
        e.layout(&t, 72.0).glyphs.iter().map(|g| g.id).collect::<Vec<_>>()
    };
    let plain = ids(&mut e, vec![], true);
    let alt = ids(&mut e, vec![FontFeature { tag: "zero".into(), value: 1 }], true);
    assert_ne!(plain, alt, "Inter's `zero` feature selects the slashed zero");
    let a = width(&e.layout(&styled("tracking", CharStyle::default()), 72.0));
    let b = width(&e.layout(&styled("tracking", CharStyle { tracking: 100.0, ..Default::default() }), 72.0));
    // 100/1000 em at 12 px per letter (8 letters; trailing spacing counts too).
    assert!((b - a - 8.0 * 1.2).abs() < 1.3, "{a} → {b}");
}

#[test]
fn raster_coverage_scales_and_depths_agree() {
    let mut e = TextEngine::new();
    let t = |size: f32| TextLayer { transform: Affine::translate(10.0, 50.0), ..point("Ink", size) };
    let (_, r12) = e.render(&t(12.0), 72.0, PixelFormat::RGBA8);
    let (_, r24) = e.render(&t(24.0), 72.0, PixelFormat::RGBA8);
    let s12 = alpha_sum(&r12.surface, r12.rect);
    let s24 = alpha_sum(&r24.surface, r24.rect);
    assert!(s12 > 20.0, "{s12}");
    let ratio = s24 / s12;
    assert!((3.6..4.4).contains(&ratio), "ink ∝ size²: {ratio}");
    // Placement: anchored at (10, 50) baseline.
    assert!(r12.rect.x0 >= 8 && r12.rect.x0 <= 11 && r12.rect.y1 <= 53 && r12.rect.y1 >= 50, "{:?}", r12.rect);
    for fmt in [PixelFormat::RGBA16, PixelFormat::RGBA32F, PixelFormat::GRAYA8, PixelFormat::CMYKA8] {
        let (_, r) = e.render(&t(12.0), 72.0, fmt);
        assert_eq!(r.rect, r12.rect);
        let s = alpha_sum(&r.surface, r.rect);
        assert!((s - s12).abs() / s12 < 0.01, "{fmt:?}: {s} vs {s12}");
    }
    // Deterministic.
    let (_, again) = e.render(&t(12.0), 72.0, PixelFormat::RGBA8);
    assert_eq!(again.surface, r12.surface);
    // Golden-ish total ink for "Ink" in Inter 12 px (±3%).
    assert!((s12 - GOLDEN_INK_12).abs() / GOLDEN_INK_12 < 0.03, "ink sum {s12}");
}

/// Sum of alpha for "Ink", Inter Regular 12 px (measured once; guards rasterizer regressions).
const GOLDEN_INK_12: f64 = 44.4;

/// Faux bold widens each advance by 55/2048 em (59/2048 with faux italic), as the pen positions
/// Reference stores in `Txt2` show.
#[test]
fn faux_bold_widens_advances() {
    let mut e = TextEngine::new();
    let base = CharStyle { size_pt: 40.0, ..Default::default() };
    let xs = |e: &mut TextEngine, s: CharStyle| -> Vec<f32> { e.layout(&styled("Hiya", s), 72.0).glyphs.iter().map(|g| g.x).collect() };
    let plain = xs(&mut e, base.clone());
    let bold = xs(&mut e, CharStyle { faux_bold: true, ..base.clone() });
    let both = xs(&mut e, CharStyle { faux_bold: true, faux_italic: true, ..base.clone() });
    let italic = xs(&mut e, CharStyle { faux_italic: true, ..base.clone() });
    assert_eq!(plain.len(), 4);
    for i in 0..4 {
        let n = i as f32;
        assert!((bold[i] - plain[i] - n * 40.0 * 55.0 / 2048.0).abs() < 1e-3, "{plain:?} {bold:?}");
        assert!((both[i] - plain[i] - n * 40.0 * 59.0 / 2048.0).abs() < 1e-3, "{plain:?} {both:?}");
        assert!((italic[i] - plain[i]).abs() < 1e-3);
    }
}

#[test]
fn styles_change_pixels() {
    let mut e = TextEngine::new();
    let base = CharStyle { size_pt: 30.0, ..Default::default() };
    let ink = |e: &mut TextEngine, s: CharStyle| {
        let (_, r) = e.render(&TextLayer { transform: Affine::translate(5.0, 40.0), ..styled("Hi", s) }, 72.0, PixelFormat::RGBA8);
        (alpha_sum(&r.surface, r.rect), r.rect)
    };
    let (plain, prect) = ink(&mut e, base.clone());
    let (bold, _) = ink(&mut e, CharStyle { faux_bold: true, ..base.clone() });
    assert!(bold > plain * 1.02, "{plain} → {bold}");
    let (under, urect) = ink(&mut e, CharStyle { underline: true, ..base.clone() });
    assert!(under > plain && urect.y1 > prect.y1);
    let (_, irect) = ink(&mut e, CharStyle { faux_italic: true, ..base.clone() });
    assert!(irect.x1 > prect.x1, "slanted top extends right");
    let (_, srect) = ink(&mut e, CharStyle { baseline_shift_pt: 10.0, ..base.clone() });
    assert_eq!(srect.y0, prect.y0 - 10);
    let (_, hrect) = ink(&mut e, CharStyle { horizontal_scale: 2.0, ..base.clone() });
    assert!(hrect.width() as f32 > prect.width() as f32 * 1.7);
}

#[test]
fn multicolor_runs() {
    let mut e = TextEngine::new();
    let red = CharStyle { size_pt: 40.0, color: Color::rgb(1.0, 0.0, 0.0), ..Default::default() };
    let blue = CharStyle { color: Color::rgb(0.0, 0.0, 1.0), ..red.clone() };
    let t = TextLayer {
        text: "HH".into(),
        runs: vec![TextRun { len: 1, style: red }, TextRun { len: 1, style: blue }],
        transform: Affine::translate(0.0, 40.0),
        ..Default::default()
    };
    let (l, r) = e.render(&t, 72.0, PixelFormat::RGBA8);
    assert_eq!(l.glyphs.len(), 2);
    let px = r.surface.read_region(r.rect);
    let opaque: Vec<&[f32; 4]> = px.as_chunks::<4>().0.iter().filter(|p| p[3] > 0.99).collect();
    assert!(opaque.iter().any(|p| p[0] > 0.99 && p[2] < 0.01));
    assert!(opaque.iter().any(|p| p[2] > 0.99 && p[0] < 0.01));
    // CMYK target keeps colour in the document model.
    let (_, rc) = e.render(&t, 72.0, PixelFormat::CMYKA8);
    assert!(rc.surface.read_region(rc.rect).as_chunks::<5>().0.iter().any(|p| p[4] > 0.99 && p[1] > 0.9 && p[2] > 0.9));
}

#[test]
fn hit_test_and_caret() {
    let mut e = TextEngine::new();
    let l = e.layout(&point("abc\ndef", 20.0), 72.0);
    assert_eq!(l.hit_test(-5.0, -5.0), 0);
    assert_eq!(l.hit_test(1000.0, -5.0), 3);
    assert_eq!(l.hit_test(-5.0, 24.0), 4);
    let (x, top, bottom) = l.caret(1);
    assert!(x > 0.0 && top < 0.0 && bottom > 0.0);
    let (x2, ..) = l.caret(2);
    assert!(x2 > x);
    assert_eq!(l.caret(3).0, l.lines[0].x1);
}

#[test]
fn empty_text_and_empty_lines() {
    let mut e = TextEngine::new();
    let l = e.layout(&point("", 12.0), 72.0);
    assert_eq!(l.lines.len(), 1);
    assert!(l.glyphs.is_empty());
    let l = e.layout(&point("a\n\nb", 12.0), 72.0);
    assert_eq!(l.lines.len(), 3);
    assert!((l.lines[2].baseline - 2.0 * 14.4).abs() < 1e-3);
    let (_, r) = e.render(&point("", 12.0), 72.0, PixelFormat::RGBA8);
    assert_eq!(r.rect.width(), 0);
}

/// U+0003 is a forced line break inside a paragraph (as PSD type stores Shift+Return): a new
/// line with the paragraph's leading and no paragraph spacing, never a missing-glyph box.
#[test]
fn forced_line_break_starts_a_line_in_the_same_paragraph() {
    let mut e = TextEngine::new();
    let text = "one\u{3}two";
    let spaced = ParagraphStyle { space_before_pt: 5.0, space_after_pt: 5.0, ..Default::default() };
    let l = e.layout(&with_para(point(text, 10.0), spaced.clone()), 72.0);
    assert_eq!(l.lines.len(), 2, "{:?}", l.lines);
    assert_eq!(text[l.lines[0].range.clone()].trim_end_matches('\u{3}'), "one");
    assert_eq!(&text[l.lines[1].range.clone()], "two");
    assert_eq!((l.lines[0].paragraph, l.lines[1].paragraph), (0, 0));
    assert!((l.lines[1].baseline - 12.0).abs() < 1e-3, "auto leading, no paragraph spacing: {}", l.lines[1].baseline);
    assert!(l.glyphs.iter().all(|g| g.id != 0), "no .notdef for the break");
    // A paragraph break in the same place adds the spacing.
    let p = e.layout(&with_para(point("one\ntwo", 10.0), spaced), 72.0);
    assert!((p.lines[1].baseline - 22.0).abs() < 1e-3, "{}", p.lines[1].baseline);
    // Centred lines are centred on their own, and box text breaks there too.
    let c = e.layout(&with_para(point("a\u{3}wide line", 20.0), ParagraphStyle { align: TextAlign::Center, ..Default::default() }), 72.0);
    assert_eq!(c.lines.len(), 2);
    assert!((c.lines[0].x0 + c.lines[0].x1).abs() < 0.01 && (c.lines[1].x0 + c.lines[1].x1).abs() < 0.01, "{:?}", c.lines);
    let mut b = point("short\u{3}next", 12.0);
    b.shape = TextShape::Box { x: 0.0, y: 0.0, width: 500.0, height: 500.0 };
    assert_eq!(e.layout(&b, 72.0).lines.len(), 2);
    // Rendered at every depth: two rows of ink, and nothing after the first line's text.
    for fmt in [PixelFormat::RGBA8, PixelFormat::RGBA16, PixelFormat::RGBA32F] {
        let (l, r) = e.render(&TextLayer { transform: Affine::translate(0.0, 40.0), ..point(text, 20.0) }, 72.0, fmt);
        let row = |i: usize| {
            let ln = &l.lines[i];
            photosplice_geom::Rect::new(-2, (ln.baseline - ln.ascent + 40.0) as i32, 200, (ln.baseline + 40.0) as i32)
        };
        assert!(alpha_sum(&r.surface, row(0)) > 1.0 && alpha_sum(&r.surface, row(1)) > 1.0, "{fmt:?}");
        let one_end = l.lines[0].x1.ceil() as i32 + 2;
        assert!(alpha_sum(&r.surface, photosplice_geom::Rect::new(one_end, row(0).y0, 200, row(0).y1)) < 1e-3, "{fmt:?}: ink after the first line's text");
    }
}

#[test]
fn postscript_names() {
    let g = fonts::guess_from_postscript("MyriadPro-BoldIt");
    assert_eq!((g.family.as_str(), g.weight, g.italic), ("Myriad Pro", 700, true));
    let g = fonts::guess_from_postscript("TimesNewRomanPSMT");
    assert_eq!((g.family.as_str(), g.weight, g.italic), ("Times New Roman", 400, false));
    let g = fonts::guess_from_postscript("Arial-BoldMT");
    assert_eq!((g.family.as_str(), g.weight), ("Arial", 700));
    let mut db = fonts::FontDb::new();
    let r = db.resolve_postscript("Inter-SemiBold");
    assert_eq!((r.family.as_str(), r.weight, r.exact), ("Inter", 600, true));
    // Unknown fonts fall back gracefully in layout.
    let mut e = TextEngine::new();
    let t = styled("x", CharStyle { font_family: "Nonexistent Sans".into(), postscript_name: Some("Nope-Bold".into()), ..Default::default() });
    assert!(e.layout(&t, 72.0).glyphs[0].id != 0);
}

#[test]
fn depth_is_respected() {
    let mut e = TextEngine::new();
    let (_, r) = e.render(&TextLayer { transform: Affine::translate(2.0, 20.0), ..point("a", 20.0) }, 72.0, PixelFormat::RGBA16);
    assert_eq!(r.surface.format().sample, SampleType::U16);
    assert!(r.surface.format().alpha);
}

/// Visual check: `PHOTOSPLICE_TEXT_DUMP=/tmp/t.ppm cargo test -p photosplice-text dump -- --ignored`.
#[test]
#[ignore]
fn dump_sample() {
    use photosplice_doc::text::{Caps, TextAlign};
    let Some(path) = std::env::var_os("PHOTOSPLICE_TEXT_DUMP") else {
        return;
    };
    let mut e = if std::env::var_os("PHOTOSPLICE_TEXT_SYSTEM").is_some() { TextEngine::with_system_fonts() } else { TextEngine::new() };
    let s = CharStyle { size_pt: 28.0, ..Default::default() };
    let text =
        "Photosplice Type\nBold faux, italic faux, underline\nשלום عربي mixed ✓\nJustified paragraph text wraps inside the box nicely and evenly across lines.";
    let runs = vec![
        TextRun { len: 16, style: CharStyle { size_pt: 40.0, color: Color::rgb(0.1, 0.3, 0.9), caps: Caps::Normal, ..s.clone() } },
        TextRun { len: 5, style: CharStyle { faux_bold: true, ..s.clone() } },
        TextRun { len: 13, style: CharStyle { faux_italic: true, color: Color::rgb(0.8, 0.1, 0.1), ..s.clone() } },
        TextRun { len: 11, style: CharStyle { underline: true, strikethrough: false, ..s.clone() } },
        TextRun { len: 1000, style: CharStyle { size_pt: 20.0, tracking: 20.0, ..s.clone() } },
    ];
    let t = TextLayer {
        text: text.into(),
        runs,
        paragraphs: vec![
            ParagraphRun { len: 45, style: ParagraphStyle { align: TextAlign::Left, ..Default::default() } },
            ParagraphRun { len: 1000, style: ParagraphStyle { align: TextAlign::JustifyLeft, space_before_pt: 6.0, ..Default::default() } },
        ],
        shape: TextShape::Box { x: 0.0, y: 0.0, width: 560.0, height: 400.0 },
        transform: Affine::translate(20.0, 20.0),
        ..Default::default()
    };
    let (_, r) = e.render(&t, 72.0, PixelFormat::RGBA8);
    let (w, h) = (600u32, 360u32);
    let mut out = format!("P6 {w} {h} 255\n").into_bytes();
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let p = r.surface.pixel(x, y);
            let a = p[3];
            for c in &p[..3] {
                out.push(((c * a + (1.0 - a)) * 255.0).round() as u8);
            }
        }
    }
    std::fs::write(path, out).unwrap();
}

/// Variable-font axes reach the outlines (uses a system variable font when one is present).
#[test]
fn variable_font_axes() {
    let candidates =
        ["/System/Library/Fonts/SFNS.ttf", "/usr/share/fonts/truetype/noto/NotoSans-VariableFont_wdth,wght.ttf", "C:\\Windows\\Fonts\\bahnschrift.ttf"];
    let Some(bytes) = candidates.iter().find_map(|p| std::fs::read(p).ok()) else {
        return;
    };
    let mut e = TextEngine::new();
    let fams = e.fonts.register_font_data(bytes);
    let Some(fam) = fams.first().cloned() else {
        return;
    };
    if !e.fonts.faces(&fam).iter().any(|f| f.axes.iter().any(|a| a.0 == "wght")) {
        return;
    }
    let ink = |e: &mut TextEngine, w: f32| {
        let s = CharStyle {
            font_family: fam.clone(),
            size_pt: 40.0,
            variations: vec![photosplice_doc::text::FontVariation { axis: "wght".into(), value: w }],
            ..Default::default()
        };
        let (_, r) = e.render(&TextLayer { transform: Affine::translate(0.0, 50.0), ..styled("Weight", s) }, 72.0, PixelFormat::RGBA8);
        alpha_sum(&r.surface, r.rect)
    };
    let light = ink(&mut e, 200.0);
    let heavy = ink(&mut e, 800.0);
    assert!(heavy > light * 1.5, "{light} → {heavy}");
}

#[test]
fn warp_bends_rendered_text_and_outlines() {
    use photosplice_doc::text::TextWarp;
    let mut e = TextEngine::new();
    let mut t = point("WARPED TEXT", 24.0);
    t.transform = Affine::translate(20.0, 60.0);
    let fmt = PixelFormat::RGBA8;
    let (_, flat) = e.render(&t, 72.0, fmt);
    t.warp = Some(TextWarp { style: "warpArc".into(), value: 60.0, horizontal: true, ..Default::default() });
    let (layout, arced) = e.render(&t, 72.0, fmt);
    // An arc pushes the ends down: the warped ink is taller and pixels differ.
    assert!(arced.rect.height() > flat.rect.height() + 10, "{:?} vs {:?}", arced.rect, flat.rect);
    assert!(alpha_sum(&arced.surface, arced.rect) > 0.0);
    // Outlines follow the same warp: the first glyph's outline sits lower than the middle one's.
    let warp = crate::render::layout_warp(&layout, &t);
    let outs = crate::render::outlines(&layout, &t.transform, warp.as_ref());
    assert_eq!(outs.len(), layout.glyphs.iter().filter(|g| g.id != 0).count() - 1, "space has no outline");
    let low_y = |els: &Vec<crate::render::PathEl>| {
        els.iter()
            .filter_map(|e| match e {
                crate::render::PathEl::MoveTo(p) | crate::render::PathEl::LineTo(p) => Some(p[1]),
                _ => None,
            })
            .fold(f64::MIN, f64::max)
    };
    assert!(low_y(&outs[0]) > low_y(&outs[outs.len() / 2]) + 5.0);
    // Unwarped outlines stay within the flat raster's bounds.
    let flat_outs = crate::render::outlines(&layout, &t.transform, None);
    let r = flat.rect;
    for els in &flat_outs {
        for el in els {
            if let crate::render::PathEl::MoveTo(p) | crate::render::PathEl::LineTo(p) = el {
                assert!(p[0] >= f64::from(r.x0) && p[0] <= f64::from(r.x1) && p[1] >= f64::from(r.y0) && p[1] <= f64::from(r.y1));
            }
        }
    }
}

/// `Txt2` as Photoshop writes it (a bare `/key value` sequence; text objects at `/1 /1`, style
/// runs at `/0 /6 /0`, auto-kern mode at `/11`), for the layer text `text`.
fn txt2_with_modes(text: &str, modes: &[(usize, i64)]) -> Vec<u8> {
    txt2_with_styles(text, &modes.iter().map(|(len, m)| (*len, format!("/11 {m}"))).collect::<Vec<_>>())
}

/// [`txt2_with_modes`] with each style run's keys given as EngineData text (`"/11 1 /85 2"`).
fn txt2_with_styles(text: &str, styles: &[(usize, String)]) -> Vec<u8> {
    let mut v = b"\n\n/98 << /0 14 >> /0 << >> /1 << /1 [ << /0 << /0 (\xfe\xff".to_vec();
    for u in format!("{text}\r").encode_utf16() {
        let b = u.to_be_bytes();
        for x in b {
            if matches!(x, b'(' | b')' | b'\\') {
                v.push(b'\\');
            }
            v.push(x);
        }
    }
    v.extend_from_slice(b") /6 << /0 [ ");
    for (len, keys) in styles {
        v.extend_from_slice(format!("<< /0 << /0 << /0 (\u{fe}\u{ff}) /6 << /0 0 {keys} >> >> >> /1 {len} >> ").as_bytes());
    }
    v.extend_from_slice(b"] >> >> >> ] >>");
    v
}

fn with_text_index(tysh: &[u8], index: i32) -> Vec<u8> {
    let mut t = crate::psd::parse_tysh(tysh).unwrap();
    t.text.items.retain(|(k, _)| !k.is("TextIndex"));
    t.text.items.push((photosplice_psd::descriptor::Id::new("TextIndex"), photosplice_psd::descriptor::Value::Integer(index)));
    crate::psd::write_tysh(&t)
}

/// Optical (and "0") kerning only lives in `Txt2`: import applies it when the layer's
/// `TextIndex` object still holds the same text.
#[test]
fn txt2_carries_optical_kerning() {
    use photosplice_doc::text::Kerning;
    let t = styled("AVA", CharStyle::default());
    let tysh = with_text_index(&crate::psd::build_tysh(&t, 72.0, None), 0);
    let mut back = crate::psd::text_layer_from_tysh(&tysh, 72.0).unwrap();
    assert!(back.char_runs().iter().all(|r| r.style.kerning == Kerning::Metrics));
    // "AVA\r": two optical characters, then manual ("0") for the last and the break.
    let txt2 = crate::psd::parse_txt2(&txt2_with_modes("AVA", &[(2, 2), (2, 0)])).unwrap();
    crate::psd::apply_txt2(&mut back, &tysh, &txt2);
    let modes: Vec<(usize, Kerning)> = back.char_runs().iter().map(|r| (r.len, r.style.kerning)).collect();
    assert_eq!(modes, vec![(2, Kerning::Optical), (1, Kerning::Off)]);
    // Stale Txt2 (other text), another index or garbage: nothing changes, nothing panics.
    for (data, index) in [(txt2_with_modes("AVX", &[(4, 2)]), 0), (txt2_with_modes("AVA", &[(4, 2)]), 3), (b"<< /1 [ (x".to_vec(), 0), (Vec::new(), 0)] {
        let tysh = with_text_index(&crate::psd::build_tysh(&t, 72.0, None), index);
        let mut l = crate::psd::text_layer_from_tysh(&tysh, 72.0).unwrap();
        if let Some(txt2) = crate::psd::parse_txt2(&data) {
            crate::psd::apply_txt2(&mut l, &tysh, &txt2);
        }
        assert!(
            l.char_runs().iter().all(|r| r.style.kerning == Kerning::Metrics),
            "case {index}: {:?}",
            l.char_runs().iter().map(|r| r.style.kerning).collect::<Vec<_>>()
        );
    }
    // Run lengths longer than the text, zero lengths and odd modes are tolerated.
    let txt2 = crate::psd::parse_txt2(&txt2_with_modes("AVA", &[(0, 2), (100, 2), (5, 9)])).unwrap();
    let mut l = crate::psd::text_layer_from_tysh(&tysh, 72.0).unwrap();
    crate::psd::apply_txt2(&mut l, &tysh, &txt2);
    assert!(l.char_runs().iter().all(|r| r.style.kerning == Kerning::Optical));
}

/// EngineData pair fields: manual kerning and "no automatic kerning" round-trip exactly, in
/// Photoshop's form (see `psd::pair_runs`).
#[test]
fn psd_round_trips_manual_kerning() {
    use photosplice_doc::text::Kerning::{Metrics as M, Off as O};
    let s = CharStyle::default();
    let cases: Vec<Vec<(usize, photosplice_doc::text::Kerning, f32)>> = vec![
        vec![(1, O, 100.0), (1, O, -50.0), (2, M, 0.0)],
        vec![(1, M, 0.0), (1, O, 0.0), (2, M, 0.0)],
        vec![(4, O, 0.0)],
        vec![(2, M, 0.0), (1, O, 25.0), (1, M, 0.0)],
        vec![(3, M, 0.0), (1, O, 300.0)],
    ];
    for runs in cases {
        let t = runs_of("AVAT", &runs.iter().map(|&(len, kerning, kern)| (len, CharStyle { kerning, kern, ..s.clone() })).collect::<Vec<_>>());
        let back = crate::psd::text_layer_from_tysh(&crate::psd::build_tysh(&t, 72.0, None), 72.0).unwrap();
        let per = |t: &TextLayer| t.char_runs().iter().flat_map(|r| std::iter::repeat_n((r.style.kerning, r.style.kern), r.len)).collect::<Vec<_>>();
        let mut want = per(&t);
        // The last character's mode has no EngineData slot (it reads back as Metrics unless
        // it has a manual kern).
        if let Some(last) = want.last_mut()
            && last.1 == 0.0
        {
            last.0 = M;
        }
        assert_eq!(per(&back), want, "{runs:?}");
    }
}

#[test]
fn psd_round_trips_antialias_opentype_and_warp() {
    use photosplice_doc::text::{AntiAlias, TextWarp};
    let style = CharStyle {
        font_family: "Inter".into(),
        size_pt: 20.0,
        features: vec![FontFeature { tag: "swsh".into(), value: 1 }, FontFeature { tag: "frac".into(), value: 1 }],
        discretionary_ligatures: true,
        ..Default::default()
    };
    for aa in [AntiAlias::Windows, AntiAlias::WindowsLcd, AntiAlias::Crisp, AntiAlias::None] {
        let mut t = styled("1/2 Swash", style.clone());
        t.antialias = aa;
        t.warp = Some(TextWarp { style: "warpFlag".into(), value: -35.0, horizontal_distortion: 10.0, vertical_distortion: 0.0, horizontal: false });
        let bytes = crate::psd::build_tysh(&t, 72.0, None);
        let back = crate::psd::text_layer_from_tysh(&bytes, 72.0).unwrap();
        assert_eq!(back.antialias, aa);
        assert_eq!(back.warp, t.warp);
        let st = &back.char_runs()[0].style;
        assert!(st.discretionary_ligatures);
        let mut tags: Vec<&str> = st.features.iter().map(|f| f.tag.as_str()).collect();
        tags.sort_unstable();
        assert_eq!(tags, ["frac", "swsh"]);
    }
}

/// Regression: a PSD whose text engine data has no `EngineDict` (or isn't a dictionary)
/// panicked with `expect("EngineDict")` when the layer was written back (PSD export).
#[test]
fn engine_data_template_without_engine_dict() {
    use crate::engine_data::{self as ed, Value as E};
    let t = styled("Hi", CharStyle::default());
    let no_engine_dict = ed::parse(b"<< /ResourceDict << >> >>").unwrap();
    for template in [no_engine_dict, E::Dict(vec![]), E::Int(3)] {
        let e = crate::psd::build_engine_data(&t, Some(template), 72.0);
        let text = e.path(&["EngineDict", "Editor", "Text"]);
        assert!(matches!(text, Some(E::String(s)) if s == "Hi\r"), "{text:?}");
    }
}

/// #123: the caret geometry (clusters, lines) sits on the rendered glyphs, so a click on a glyph
/// lands next to it: its left part before it, its right part after it. Plain text, tracking,
/// horizontal scale, mixed sizes, and centred, wrapped paragraph text.
#[test]
fn clusters_sit_on_rendered_glyphs() {
    let mut e = TextEngine::new();
    let big = CharStyle { size_pt: 40.0, ..Default::default() };
    let small = CharStyle { size_pt: 18.0, ..Default::default() };
    let mixed =
        TextLayer { text: "HOHOHO".into(), runs: vec![TextRun { len: 3, style: big.clone() }, TextRun { len: 3, style: small.clone() }], ..Default::default() };
    let boxed = with_para(
        TextLayer { shape: TextShape::Box { x: 0.0, y: 0.0, width: 150.0, height: 200.0 }, ..styled("HOH HOH HOH HOH", small.clone()) },
        ParagraphStyle { align: TextAlign::Center, ..Default::default() },
    );
    let cases = [
        ("plain", styled("HOHOH", big.clone())),
        ("tracking", styled("HOHOH", CharStyle { tracking: 300.0, ..big.clone() })),
        ("hscale", styled("HOHOH", CharStyle { horizontal_scale: 1.6, ..big.clone() })),
        ("kerned", styled("HOHOH", CharStyle { kern: 250.0, kerning: photosplice_doc::text::Kerning::Optical, ..big.clone() })),
        ("mixed", mixed),
        ("box", boxed),
    ];
    for (name, t) in cases {
        let t = TextLayer { transform: Affine::translate(7.0, 60.0), ..t };
        let (l, r) = e.render(&t, 72.0, PixelFormat::RGBA8);
        assert!(l.lines.len() >= if name == "box" { 2 } else { 1 }, "{name}");
        for c in l.clusters.iter().filter(|c| !t.text[c.range.clone()].trim().is_empty()) {
            let ln = &l.lines[c.line];
            // Ink under the cluster's middle, between the line's ascent and descent.
            let col = photosplice_geom::Rect::new(
                (c.x + c.advance * 0.3 + 7.0).floor() as i32,
                (ln.baseline - ln.ascent + 60.0).floor() as i32,
                (c.x + c.advance * 0.55 + 7.0).ceil() as i32,
                (ln.baseline + ln.descent + 60.0).ceil() as i32,
            );
            assert!(alpha_sum(&r.surface, col) > 1.0, "{name}: no ink under cluster {:?} ({col:?})", c.range);
            let y = ln.baseline - ln.ascent * 0.4;
            assert_eq!(l.hit_test(c.x + c.advance * 0.2, y), c.range.start, "{name}: left part of {:?}", c.range);
            assert_eq!(l.hit_test(c.x + c.advance * 0.8, y), c.range.end, "{name}: right part of {:?}", c.range);
            let (cx, top, bottom) = l.caret(c.range.start);
            assert!((cx - c.x).abs() < 1e-3 && top < y && bottom > y, "{name}: caret at {:?}", c.range);
        }
    }
}

fn runs_of(text: &str, styles: &[(usize, CharStyle)]) -> TextLayer {
    TextLayer { text: text.into(), runs: styles.iter().map(|(len, style)| TextRun { len: *len, style: style.clone() }).collect(), ..Default::default() }
}

/// Manual kerning (1/1000 em) after a character moves everything after it by kern × size.
#[test]
fn manual_kerning_moves_the_next_glyph() {
    use photosplice_doc::text::Kerning;
    let mut e = TextEngine::new();
    let s = CharStyle { size_pt: 100.0, ..Default::default() };
    let plain = e.layout(&styled("HOH", s.clone()), 72.0);
    let kerned = e.layout(&runs_of("HOH", &[(1, CharStyle { kern: 100.0, ..s.clone() }), (2, s.clone())]), 72.0);
    let x = |l: &crate::TextLayout, i: usize| l.glyphs[i].x;
    assert_eq!(x(&kerned, 0), x(&plain, 0));
    // 100/1000 em at 100 px = 10 px, for the next glyph and everything after it.
    assert!((x(&kerned, 1) - x(&plain, 1) - 10.0).abs() < 1e-3, "{} vs {}", x(&kerned, 1), x(&plain, 1));
    assert!((x(&kerned, 2) - x(&plain, 2) - 10.0).abs() < 1e-3);
    assert!((width(&kerned) - width(&plain) - 10.0).abs() < 1e-3);
    // Carets follow: the cluster after the kerned pair starts 10 px later.
    assert!((kerned.caret(1).0 - plain.caret(1).0 - 10.0).abs() < 1e-3);
    // Negative kerning tightens; kerning on the last character doesn't move anything.
    let tight = e.layout(&runs_of("HOH", &[(1, CharStyle { kern: -50.0, ..s.clone() }), (2, s.clone())]), 72.0);
    assert!((x(&tight, 1) - x(&plain, 1) + 5.0).abs() < 1e-3);
    let last = e.layout(&runs_of("HOH", &[(2, s.clone()), (1, CharStyle { kern: 500.0, ..s.clone() })]), 72.0);
    assert!((width(&last) - width(&plain)).abs() < 1e-3);
    // Off replaces the font's pair kerning: "AV" with Off is wider than with Metrics.
    let mut av = |st: CharStyle| e.layout(&styled("AV", st), 72.0).glyphs[1].x;
    let metric = av(s.clone());
    let off = av(CharStyle { kerning: Kerning::Off, ..s.clone() });
    assert!(off > metric + 1.0, "Inter kerns AV: {off} vs {metric}");
    // Centred point text stays centred around the anchor with kerning.
    let centred = with_para(
        runs_of("HOH", &[(1, CharStyle { kern: 300.0, ..s.clone() }), (2, s.clone())]),
        ParagraphStyle { align: TextAlign::Center, ..Default::default() },
    );
    let l = e.layout(&centred, 72.0);
    assert!((l.lines[0].x0 + l.lines[0].x1).abs() < 0.5, "{:?}", l.lines[0]);
}

/// Optical kerning computes pair spacing from the outlines: tighter for open pairs ("AV", "To")
/// than the unkerned advance, about neutral for straight stems, and never absurd.
#[test]
fn optical_kerning_tightens_open_pairs() {
    use photosplice_doc::text::Kerning;
    let mut e = TextEngine::new();
    let s = CharStyle { size_pt: 100.0, ..Default::default() };
    let gap = |e: &mut TextEngine, text: &str, k: Kerning| {
        let l = e.layout(&styled(text, CharStyle { kerning: k, ..s.clone() }), 72.0);
        l.glyphs[1].x - l.glyphs[0].x
    };
    for pair in ["AV", "To", "LT", "Ty"] {
        let off = gap(&mut e, pair, Kerning::Off);
        let optical = gap(&mut e, pair, Kerning::Optical);
        assert!(optical < off - 3.0, "{pair}: optical {optical} vs unkerned {off}");
    }
    for pair in ["HH", "nn", "oo", "HO"] {
        let off = gap(&mut e, pair, Kerning::Off);
        let optical = gap(&mut e, pair, Kerning::Optical);
        assert!((optical - off).abs() < 6.0, "{pair}: optical {optical} vs unkerned {off}");
    }
    // A space breaks the pair; a manual kern replaces the automatic one (as in Photoshop).
    assert_eq!(gap(&mut e, "A V", Kerning::Optical), gap(&mut e, "A V", Kerning::Off));
    for mode in [Kerning::Optical, Kerning::Metrics] {
        let l = e.layout(&styled("AV", CharStyle { kerning: mode, kern: 100.0, ..s.clone() }), 72.0);
        let off = gap(&mut e, "AV", Kerning::Off);
        assert!((l.glyphs[1].x - l.glyphs[0].x - off - 10.0).abs() < 1e-3, "{mode:?}");
    }
}

/// A mode change splits shaping runs: the pairs on both sides of a manually kerned character
/// lose their automatic kerning (Photoshop renders it the same way).
#[test]
fn kerning_modes_split_pairs() {
    use photosplice_doc::text::Kerning;
    let mut e = TextEngine::new();
    let s = CharStyle { size_pt: 100.0, ..Default::default() };
    let off = CharStyle { kerning: Kerning::Off, ..s.clone() };
    let xs = |e: &mut TextEngine, t: &TextLayer| e.layout(t, 72.0).glyphs.iter().map(|g| g.x).collect::<Vec<_>>();
    let metric = xs(&mut e, &styled("AVAV", s.clone()));
    let plain = xs(&mut e, &styled("AVAV", off.clone()));
    let mixed = xs(&mut e, &runs_of("AVAV", &[(2, s.clone()), (1, off.clone()), (1, s.clone())]));
    let adv = |v: &[f32], i: usize| v[i + 1] - v[i];
    assert!((adv(&mixed, 0) - adv(&metric, 0)).abs() < 1e-3, "AV before stays kerned");
    assert!((adv(&mixed, 1) - adv(&plain, 1)).abs() < 1e-3, "VA into the manual character");
    assert!((adv(&mixed, 2) - adv(&plain, 2)).abs() < 1e-3, "AV out of it");
}

#[test]
fn japanese_dictionary_word_boundaries() {
    // Use only the existing bundled Latin fonts: segmentation must not require a CJK font.
    let text = "私は学生です";
    let mut fonts = fonts::FontDb::new();
    let mut context = parley::LayoutContext::<[u8; 4]>::new();
    let mut layout = parley::Layout::new();
    let mut builder = context.ranged_builder(&mut fonts.fcx, text, 1.0, false);
    builder.push_default(parley::StyleProperty::FontFamily(parley::FontFamily::named("Inter")));
    // Suppress CJK line-break opportunities so these flags expose word boundaries alone.
    builder.push_default(parley::StyleProperty::WordBreak(parley::WordBreak::KeepAll));
    builder.build_into(&mut layout, text);
    layout.break_all_lines(None);
    let mut boundaries = Vec::new();
    for line in layout.lines() {
        for run in line.runs() {
            for cluster in run.clusters() {
                if cluster.is_word_boundary() {
                    boundaries.push(cluster.text_range().start);
                }
            }
        }
    }
    assert_eq!(boundaries, vec![0, 3, 6, 12]);
}

#[test]
fn japanese_box_text_preserves_wrap_boundaries() {
    let mut engine = TextEngine::new();
    let mut text = point("日本語の文章を折り返します。", 12.0);
    text.shape = TextShape::Box { x: 0.0, y: 0.0, width: 40.0, height: 1000.0 };
    let layout = engine.layout(&text, 72.0);
    assert!(layout.lines.len() > 1);
    let mut end = 0;
    for line in &layout.lines {
        assert_eq!(line.range.start, end);
        let content = text.text.get(line.range.clone()).expect("UTF-8 line boundaries");
        assert!(!content.starts_with('。'), "closing punctuation must stay with its preceding text");
        end = line.range.end;
    }
    assert_eq!(end, text.text.len());
}

#[test]
fn word_and_line_navigation() {
    use crate::layout::{byte_index, char_index, hit_char, line_edge, line_index, line_step, word_boundary};
    assert_eq!(word_boundary("hello big world", 0, true), 5);
    assert_eq!(word_boundary("hello big world", 7, false), 6);
    assert_eq!(word_boundary("hello big world", 15, false), 10);
    assert_eq!(word_boundary("hello", 0, false), 0);
    assert_eq!(word_boundary("hello", 5, true), 5);
    assert_eq!(word_boundary("", 4, true), 0);
    assert_eq!(word_boundary("ab", 100, true), 2);
    assert_eq!(word_boundary("ab, cd", 0, true), 2);
    assert_eq!(word_boundary("ab, cd", 2, true), 6);
    assert_eq!(word_boundary("Größe", 0, true), 5);

    let mut e = TextEngine::new();
    let text = "AäB\ncd";
    for vertical in [false, true] {
        let mut t = point(text, 20.0);
        if vertical {
            t.orientation = Orientation::Vertical;
        }
        let l = e.layout(&t, 72.0);
        let n = text.chars().count();
        for i in 0..=n {
            let b = byte_index(text, i);
            let [(x0, y0), (x1, y1)] = l.caret_segment(b);
            let (idx, line) = hit_char(&l, text, (x0 + x1) / 2.0, (y0 + y1) / 2.0);
            assert_eq!(idx, i, "vertical {vertical} index {i}");
            assert_eq!(line, line_index(&l, b), "vertical {vertical} index {i}");
        }
        assert!(l.lines.len() >= 2, "vertical {vertical}");
        let end0 = char_index(text, l.lines[0].range.end);
        let start1 = char_index(text, l.lines[1].range.start);
        assert_eq!(line_edge(&l, text, 1, true), end0, "vertical {vertical}");
        assert_eq!(line_edge(&l, text, 1, false), 0, "vertical {vertical}");
        assert_eq!(line_edge(&l, text, start1, false), start1, "vertical {vertical}");
        assert_eq!(line_edge(&l, text, start1, true), n, "vertical {vertical}");
        let x = l.caret(byte_index(text, 0)).0;
        let next = line_step(&l, text, 0, x, 1);
        assert!((start1..=n).contains(&next), "vertical {vertical} line_step -> {next}");
        assert_eq!(line_step(&l, text, 0, x, -1), 0, "vertical {vertical}");
        assert_eq!(line_step(&l, text, n, x, 1), n, "vertical {vertical}");
    }

    // A remembered column stays on the short line's start; the caret's own x falls off its end.
    let text = "WWWWWW\nI";
    let l = e.layout(&point(text, 30.0), 72.0);
    let end0 = line_edge(&l, text, 0, true);
    // end0 is the first line's end, which is not the second line. Step from there.
    let kept = line_step(&l, text, end0, l.caret(0).0, 1);
    let jumped = line_step(&l, text, end0, l.caret(byte_index(text, end0)).0, 1);
    assert_eq!(kept, char_index(text, l.lines[1].range.start), "kept column");
    assert_eq!(jumped, char_index(text, l.lines[1].range.end), "own column");
    assert!(jumped > kept);
}

/// What a rewrite of a Photoshop type layer must keep (Photoshop verification kit, case 07: a
/// rewritten warped layer made Photoshop ask to update it and lose the warp when it did).
#[test]
fn rewrite_keeps_what_photoshop_wrote() {
    use crate::engine_data::{self as ed, Value as E};
    use photosplice_doc::text::{AntiAlias, TextWarp};
    use photosplice_psd::descriptor::{Id, Value as D};
    fn first<'a>(v: &'a mut E, path: &[&str]) -> Option<&'a mut E> {
        let mut v = v;
        for k in path {
            v = match v.get_mut(k)? {
                E::Array(a) => a.first_mut()?,
                x => x,
            };
        }
        Some(v)
    }
    let mut t = styled("Warped", CharStyle::default());
    t.antialias = AntiAlias::Sharp;
    t.warp = Some(TextWarp { style: "warpArc".into(), value: 50.0, horizontal_distortion: 0.0, vertical_distortion: 0.0, horizontal: true });
    let mut ty = crate::psd::parse_tysh(&crate::psd::build_tysh(&t, 72.0, Some([0.0, -34.0, 289.0, 15.0]))).unwrap();
    // Keys Photoshop writes that the model doesn't carry.
    let mut e = crate::psd::engine_data(&ty.text).unwrap();
    let data = first(&mut e, &["EngineDict", "StyleRun", "RunArray", "StyleSheet", "StyleSheetData"]).unwrap();
    data.set("Tsume", E::Real(0.25));
    data.set("Leading", E::Real(27.0));
    let run = first(&mut e, &["EngineDict", "ParagraphRun", "RunArray"]).unwrap();
    run.set("Adjustments", E::Dict(vec![("Axis".into(), E::Array(vec![E::Real(1.0), E::Real(0.0), E::Real(1.0)]))]));
    first(run, &["ParagraphSheet", "Properties"]).unwrap().set("Burasagari", E::Bool(true));
    let base = first(&mut e, &["EngineDict", "Rendered", "Shapes", "Children", "Cookie", "Photoshop", "Base"]).unwrap();
    base.set("TransformPoint0", E::Array(vec![E::Real(2.0), E::Real(0.0)]));
    ty.text.items.retain(|(k, _)| !k.is("EngineData"));
    ty.text.items.push((Id::new("EngineData"), D::RawData(ed::write(&e))));
    t.psd_raw = Some(std::sync::Arc::new(crate::psd::write_tysh(&ty)));

    // Rewritten the way a move with a missing font does it: no layout to measure.
    let back = crate::psd::parse_tysh(&crate::psd::build_tysh(&t, 72.0, None)).unwrap();
    let mut e = crate::psd::engine_data(&back.text).unwrap();
    let data = first(&mut e, &["EngineDict", "StyleRun", "RunArray", "StyleSheet", "StyleSheetData"]).unwrap().clone();
    assert_eq!(data.get("Tsume"), Some(&E::Real(0.25)));
    assert_eq!(data.get("Leading"), Some(&E::Real(27.0)), "auto leading keeps the stored value");
    let run = first(&mut e, &["EngineDict", "ParagraphRun", "RunArray"]).unwrap().clone();
    assert!(run.get("Adjustments").is_some());
    assert_eq!(run.path(&["ParagraphSheet", "Properties", "Burasagari"]), Some(&E::Bool(true)));
    let base = first(&mut e, &["EngineDict", "Rendered", "Shapes", "Children", "Cookie", "Photoshop", "Base"]).unwrap();
    assert_eq!(base.get("TransformPoint0"), Some(&E::Array(vec![E::Real(2.0), E::Real(0.0)])));
    assert_eq!(e.path(&["EngineDict", "AntiAlias"]), Some(&E::Int(4)), "Photoshop numbers Sharp 4");
    // The measured bounds stay; zero bounds made Photoshop re-lay out the type and drop its warp.
    let D::Descriptor(b) = back.text.get("bounds").unwrap() else { panic!() };
    assert!(matches!(b.get("Rght"), Some(D::UnitFloat { value, .. }) if *value == 289.0));
    assert!(back.warp.is_some_and(|w| w.items.iter().any(|(k, v)| k.is("warpValue") && *v == D::Double(50.0))));
    // Photoshop's layout: an array opens on its key's line.
    let raw = String::from_utf8_lossy(&ed::write(&e)).into_owned();
    assert!(raw.contains("/RunArray [") && !raw.contains("/RunArray\n"));
}

#[test]
fn superscript_and_subscript_shrink_and_shift() {
    use photosplice_doc::text::BaselinePosition;
    let base = CharStyle { font_family: "Inter".into(), size_pt: 20.0, ..Default::default() };
    let mut t = TextLayer {
        text: "x2y".into(),
        runs: vec![
            TextRun { len: 1, style: base.clone() },
            TextRun { len: 1, style: CharStyle { position: BaselinePosition::Superscript, ..base.clone() } },
            TextRun { len: 1, style: CharStyle { position: BaselinePosition::Subscript, ..base.clone() } },
        ],
        ..Default::default()
    };
    t.sync_summary();
    let mut e = TextEngine::new();
    let l = e.layout(&t, 72.0);
    let size = |i: usize| l.faces[l.glyphs[i].face as usize].size_px;
    assert!((size(1) - 20.0 * BaselinePosition::SIZE).abs() < 0.01, "{}", size(1));
    let shift = |i: usize| l.styles[l.glyphs[i].style as usize].baseline_shift_pt;
    assert!((shift(1) - 20.0 * BaselinePosition::OFFSET).abs() < 0.01 && (shift(2) + 20.0 * BaselinePosition::OFFSET).abs() < 0.01);
    // The model keeps the set size; FontBaseline survives the type data both ways.
    let back = crate::psd::text_layer_from_tysh(&crate::psd::build_tysh(&t, 72.0, None), 72.0).unwrap();
    let positions: Vec<_> = back.runs.iter().map(|r| r.style.position).collect();
    assert_eq!(positions, [BaselinePosition::Normal, BaselinePosition::Superscript, BaselinePosition::Subscript]);
    assert!(back.runs.iter().all(|r| (r.style.size_pt - 20.0).abs() < 1e-3));
}

#[test]
fn every_line_composer_evens_the_rag() {
    // The Photoshop corpus paragraph (paragraph-box-left.psd): Every-line breaks before
    // "legible," where greedy filling keeps it on line 2.
    let text = "Typography is the craft of arranging type to make written language legible, readable and appealing when displayed. Justification, hyphenation and leading shape the texture of a paragraph.";
    let mut e = TextEngine::new();
    let set = |every: bool| {
        let mut t = point(text, 14.0);
        t.shape = TextShape::Box { x: 0.0, y: 0.0, width: 240.0, height: 1000.0 };
        with_para(t, ParagraphStyle { every_line_composer: every, ..Default::default() })
    };
    let rag = |l: &crate::TextLayout| -> f32 { l.lines.iter().rev().skip(1).map(|ln| (240.0 - (ln.x1 - ln.x0)).powi(2)).sum() };
    let (every, single) = (e.layout(&set(true), 72.0), e.layout(&set(false), 72.0));
    for l in [&every, &single] {
        assert!(l.lines.iter().all(|ln| ln.x1 - ln.x0 <= 240.0 + 1e-3));
        assert_eq!(l.lines.first().map(|ln| ln.range.start), Some(0));
        assert_eq!(l.lines.last().map(|ln| ln.range.end), Some(text.len()));
    }
    assert!(rag(&every) <= rag(&single) + 1e-3, "{} vs {}", rag(&every), rag(&single));
    // Right alignment uses the whole box, whatever width steered the break.
    let mut right = set(true);
    right.paragraphs[0].style.align = TextAlign::Right;
    let r = e.layout(&right, 72.0);
    assert!(r.lines.iter().rev().skip(1).all(|ln| (ln.x1 - 240.0).abs() < 1.0), "{:?}", r.lines.iter().map(|l| l.x1).collect::<Vec<_>>());
    // The composer setting survives the PSD type data.
    let back = crate::psd::text_layer_from_tysh(&crate::psd::build_tysh(&set(false), 72.0, None), 72.0).unwrap();
    assert!(!back.paragraphs[0].style.every_line_composer);
}

/// `Txt2` with one path frame (`kind` 2: path) for the layer text `text`: a straight line from
/// (32, 132) to (332, 132) in the document, the text space 32 px left and up, and the first
/// glyph's position given as Photoshop records it (offset by the ascent, here −10).
fn txt2_with_path(text: &str, kind: i64, glyph: (f32, f32)) -> Vec<u8> {
    let mut v = b"/0 << /8 << /0 [ << /0 << /1 << /0 [ 32.0 132.0 132.0 132.0 232.0 132.0 332.0 132.0 ] >> ".to_vec();
    v.extend_from_slice(format!("/2 << /0 {kind} /2 [ 1.0 0.0 0.0 1.0 -32.0 -32.0 ] >> >> >> ] >> >> /1 << /1 [ << /0 << /0 (").as_bytes());
    v.extend_from_slice(&[0xfe, 0xff]);
    for u in format!("{text}\r").encode_utf16() {
        v.extend_from_slice(&u.to_be_bytes());
    }
    v.extend_from_slice(
        format!(") >> /1 << /2 [ << /6 [ << /10 0 /6 [ << /14 -10.0 /6 [ << /21 << /1 [ {} ] /2 [ {} ] >> >> ] >> ] >> ] >> ] >> >> ] >>", glyph.0, glyph.1).as_bytes(),
    );
    v
}

/// Type on a path lives only in `Txt2`: import reads the frame into text space and finds the
/// start from the first glyph's position; other frames, stale text and garbage are ignored.
#[test]
fn txt2_carries_type_on_a_path() {
    let t = styled("Path", CharStyle::default());
    let tysh = with_text_index(&crate::psd::build_tysh(&t, 72.0, None), 0);
    let read = |data: &[u8]| {
        let mut l = crate::psd::text_layer_from_tysh(&tysh, 72.0).unwrap();
        if let Some(txt2) = crate::psd::parse_txt2(data) {
            crate::psd::apply_txt2(&mut l, &tysh, &txt2);
        }
        l.path
    };
    let p = read(&txt2_with_path("Path", 2, (85.0, 110.0))).expect("path");
    assert_eq!(p.segments, vec![[[0.0, 100.0], [100.0, 100.0], [200.0, 100.0], [300.0, 100.0]]]);
    assert!(!p.closed);
    assert!((p.start - 0.25).abs() < 1e-3, "{}", p.start);
    // Positions without the ascent offset are read as they are.
    assert!((read(&txt2_with_path("Path", 2, (150.0, 100.0))).unwrap().start - 0.5).abs() < 1e-3);
    assert_eq!(read(&txt2_with_path("Path", 1, (85.0, 110.0))), None, "a box frame");
    assert_eq!(read(&txt2_with_path("Other", 2, (85.0, 110.0))), None, "stale Txt2");
    for bad in [&b"/0 << /8 << /0 [ << /0 << /1 << /0 [ 1.0 ] >> /2 << /0 2 >> >> >> ] >> >>"[..], b"/0 << /8 (x) >>", b""] {
        assert_eq!(read(bad), None);
    }
}

/// Grid fitting by anti-aliasing mode (fitted on Photoshop's renders, see `render::GridFit`):
/// Sharp and None put the baseline on a whole pixel, large Sharp type each glyph too; Crisp and
/// Smooth draw the exact outline; Strong is heavier than Smooth.
#[test]
fn anti_aliasing_modes_fit_the_pixel_grid_like_photoshop() {
    use photosplice_doc::text::AntiAlias;
    let mut e = TextEngine::new();
    let px = |l: &crate::TextLayout, x: f64, y: f64, aa: AntiAlias| {
        let r = crate::render::rasterize(l, &Affine::translate(x, y), PixelFormat::RGBA8, aa);
        (r.rect, r.surface.read_region(r.rect))
    };
    let small = e.layout(&point("Hamburg", 18.0), 72.0);
    let large = e.layout(&point("H", 40.0), 72.0);
    for aa in [AntiAlias::Sharp, AntiAlias::None] {
        assert!(px(&small, 10.0, 40.3, aa) == px(&small, 10.0, 40.0, aa), "{aa:?}: baseline snapped");
    }
    assert!(px(&small, 10.0, 40.3, AntiAlias::Smooth) != px(&small, 10.0, 40.0, AntiAlias::Smooth), "Smooth isn't snapped");
    assert!(px(&small, 10.3, 40.0, AntiAlias::Sharp) != px(&small, 10.0, 40.0, AntiAlias::Sharp), "small Sharp type keeps fractional x");
    assert!(px(&large, 10.3, 40.0, AntiAlias::Sharp) == px(&large, 10.0, 40.0, AntiAlias::Sharp), "large Sharp type on whole pixels");
    // Turned type isn't grid-fitted (its pixels aren't the glyphs' grid).
    let turned = Affine { m: [0.0, 1.0, -1.0, 0.0, 40.0, 10.0] };
    let r = crate::render::rasterize(&small, &turned, PixelFormat::RGBA8, AntiAlias::Sharp);
    assert_eq!(r.rect, crate::render::ink_rect(&small, &turned));
    let ink = |aa| {
        let r = crate::render::rasterize(&small, &Affine::translate(10.0, 40.0), PixelFormat::RGBA8, aa);
        alpha_sum(&r.surface, r.rect)
    };
    assert!(ink(AntiAlias::Strong) > ink(AntiAlias::Smooth) * 1.05);
}

/// A run's face by its real PostScript name (what a PSD's FontSet needs), when the family has
/// that face; a made-up Bold or Italic keeps the name the run asks for.
#[test]
fn faces_are_named_by_their_postscript_names() {
    let mut e = TextEngine::new();
    assert_eq!(e.fonts.postscript_of("Inter", 400, false).as_deref(), Some("Inter-Regular"));
    assert_eq!(e.fonts.postscript_of("Inter", 600, false).as_deref(), Some("Inter-SemiBold"));
    assert_eq!(e.fonts.postscript_of("JetBrains Mono", 400, false).as_deref(), Some("JetBrainsMono-Regular"));
    for (f, w, i) in [("Inter", 900, false), ("Inter", 400, true), ("No Such Family", 400, false)] {
        assert_eq!(e.fonts.postscript_of(f, w, i), None, "{f} {w} {i}");
    }
    // The TySh names a family-only run's face; the layer keeps no name of its own.
    let mut t = point("Ab", 20.0);
    t.runs = vec![TextRun { len: 2, style: CharStyle { font_family: "JetBrains Mono".into(), ..CharStyle::default() } }];
    let tysh = e.tysh(&t, 72.0, None);
    let back = crate::psd::text_layer_from_tysh(&tysh, 72.0).unwrap();
    assert_eq!(back.char_runs()[0].style.postscript_name.as_deref(), Some("JetBrainsMono-Regular"));
    assert_eq!(t.runs[0].style.postscript_name, None);
}

/// storytold/photocraft#1317: Reference keeps the last line of a text box whose baseline is inside the box even
/// when its descenders hang below the bottom edge (three 35 pt lines of 25 pt type in a
/// 91.3 pt box). A line whose baseline falls outside the box is still dropped.
#[test]
fn box_lines_whose_descent_hangs_below_the_box_are_kept() {
    let mut e = TextEngine::new();
    let mut t = styled("one\ntwo\nthree", CharStyle { size_pt: 25.0, leading_pt: Some(35.0), ..Default::default() });
    t.shape = TextShape::Box { x: 0.0, y: 0.0, width: 300.0, height: 91.27 };
    let l = e.layout(&t, 72.0);
    assert_eq!(l.lines.len(), 3, "{:?}", l.lines.iter().map(|l| l.baseline).collect::<Vec<_>>());
    assert!(l.lines[2].baseline <= 91.27 + 0.5);
    t.shape = TextShape::Box { x: 0.0, y: 0.0, width: 300.0, height: 80.0 };
    assert_eq!(e.layout(&t, 72.0).lines.len(), 2, "the third baseline (about 88) is below an 80 pt box");
}

/// `Txt2` with one text object for `text` whose rendered frame is frame 0, and that frame:
/// for point text an anchor `point`, and a frame-to-text matrix `matrix` (recent Reference,
/// storytold/photocraft#1317).
fn txt2_with_frame(text: &str, point: Option<[f64; 2]>, matrix: [f64; 6]) -> Vec<u8> {
    let mut v = txt2_with_modes(text, &[(text.encode_utf16().count() + 1, 1)]);
    // Close the object's model early: `… >> >> ] >>` → `… >> /1 << /2 [ frame ] >> >> ] >>`.
    let tail = b"] >> >> >> ] >>";
    assert!(v.ends_with(tail));
    v.truncate(v.len() - tail.len());
    v.extend_from_slice(b"] >> >> /1 << /2 [ << /6 [ << /10 0 >> ] >> ] >> >> ] >>");
    let pt = point.map(|[x, y]| format!("/0 [ {x:.5} {y:.5} ] ")).unwrap_or_default();
    let m = matrix.map(|x| format!("{x:.5}")).join(" ");
    let frames = format!("/0 << /8 << /0 [ << /0 << {pt}/2 << /2 [ {m} ] >> >> >> ] >> >>");
    let at = v.windows(9).position(|w| w == b"/0 << >> ").unwrap();
    v.splice(at..at + 9, frames.into_bytes().into_iter().chain(*b" "));
    v
}

/// storytold/photocraft#1317: point text whose anchor Reference keeps only in its `Txt2` frame (EngineData's
/// `PointBase` at the origin, the transform far away) is placed at that anchor, not at the
/// ink's top left. A rewritten `TySh` (bounds near the origin) is left alone.
#[test]
fn txt2_frame_places_point_text_at_its_anchor() {
    let mut t = point("2", 100.0);
    t.transform = Affine::translate(-1000.0, -2000.0);
    // Reference's text space: anchor at (1100, 2050), ink above its baseline.
    let tysh = with_text_index(&crate::psd::build_tysh(&t, 72.0, Some([1100.0, 1980.0, 1160.0, 2050.0])), 0);
    let txt2 = crate::psd::parse_txt2(&txt2_with_frame("2", Some([1000.0, 2100.0]), [1.0, 0.0, 0.0, 1.0, 100.0, -50.0])).unwrap();
    let mut l = crate::psd::text_layer_from_tysh(&tysh, 72.0).unwrap();
    crate::psd::apply_txt2_frame(&mut l, &tysh, &txt2);
    let o = l.transform.apply(photosplice_geom::Point::new(0.0, 0.0));
    assert!((o.x - 100.0).abs() < 1e-3 && (o.y - 50.0).abs() < 1e-3, "anchor at {o:?}");
    // Already folded in (bounds near the origin): unchanged; another text: unchanged.
    let tysh = with_text_index(&crate::psd::build_tysh(&t, 72.0, Some([0.0, -70.0, 60.0, 0.0])), 0);
    let mut l = crate::psd::text_layer_from_tysh(&tysh, 72.0).unwrap();
    let before = l.transform;
    crate::psd::apply_txt2_frame(&mut l, &tysh, &txt2);
    assert_eq!(l.transform, before);
    let tysh = with_text_index(&crate::psd::build_tysh(&t, 72.0, Some([1100.0, 1980.0, 1160.0, 2050.0])), 0);
    let other = crate::psd::parse_txt2(&txt2_with_frame("3", Some([1000.0, 2100.0]), [1.0, 0.0, 0.0, 1.0, 100.0, -50.0])).unwrap();
    let mut l = crate::psd::text_layer_from_tysh(&tysh, 72.0).unwrap();
    let before = l.transform;
    crate::psd::apply_txt2_frame(&mut l, &tysh, &other);
    assert_eq!(l.transform, before);
}

/// `tysh` with its text-space `bounds` set to `r` (Reference's, where it drew the text).
fn with_bounds(tysh: &[u8], r: [f64; 4]) -> Vec<u8> {
    use photosplice_psd::descriptor::{Descriptor, Id, Value};
    let mut t = crate::psd::parse_tysh(tysh).unwrap();
    let mut d = Descriptor::default();
    for (k, v) in ["Left", "Top ", "Rght", "Btom"].iter().zip(r) {
        d.items.push((Id::new(k), Value::Double(v)));
    }
    t.text.items.retain(|(k, _)| !k.is("bounds"));
    t.text.items.push((Id::new("bounds"), Value::Descriptor(d)));
    crate::psd::write_tysh(&t)
}

/// storytold/photocraft#1317: a text box whose `BoxBounds` are in its `Txt2` frame's space is moved by the frame's
/// matrix onto the `bounds` Reference drew it at (it used to land off the canvas, so its text
/// "disappeared" when an edit started).
#[test]
fn txt2_frame_moves_box_text_onto_its_bounds() {
    let mut t = styled("Casemiro 45", CharStyle { size_pt: 25.0, ..Default::default() });
    t.shape = TextShape::Box { x: 500.0, y: 600.0, width: 300.0, height: 90.0 };
    t.transform = Affine::translate(-700.0, -300.0);
    let built = with_text_index(&crate::psd::build_tysh(&t, 72.0, None), 0);
    let tysh = with_bounds(&built, [960.0, 300.0, 1260.0, 390.0]);
    let txt2 = crate::psd::parse_txt2(&txt2_with_frame("Casemiro 45", None, [1.0, 0.0, 0.0, 1.0, 460.0, -300.0])).unwrap();
    let mut l = crate::psd::text_layer_from_tysh(&tysh, 72.0).unwrap();
    assert!(matches!(l.shape, TextShape::Box { .. }));
    crate::psd::apply_txt2_frame(&mut l, &tysh, &txt2);
    let TextShape::Box { x, y, .. } = l.shape else { panic!("not a box") };
    let o = l.transform.apply(photosplice_geom::Point::new(f64::from(x), f64::from(y)));
    assert!((o.x - 260.0).abs() < 1e-3 && (o.y - 0.0).abs() < 1e-3, "box top left at {o:?}");
    // Our own `TySh` (bounds = the box, frame already folded in): unchanged.
    let mut l = crate::psd::text_layer_from_tysh(&built, 72.0).unwrap();
    let before = l.transform;
    crate::psd::apply_txt2_frame(&mut l, &built, &txt2);
    assert_eq!(l.transform, before);
}

/// storytold/photocraft#1469: Some older PSD writers combine a zero PointBase with an enormous local ink origin.
/// The TySh transform cancels that origin, so importing it as a zero-based layout puts text far
/// off-canvas. Fold the descriptor origin into the imported transform and preserve it on a TySh
/// round trip so every editing and export path uses the same position.
#[test]
fn legacy_tysh_point_origin_imports_and_round_trips_on_canvas() {
    use photosplice_psd::descriptor::{Descriptor, Id, Value as D};

    let source = styled("Name", CharStyle { font_family: "Inter".into(), size_pt: 120.0, ..Default::default() });
    let mut tysh = crate::psd::parse_tysh(&crate::psd::build_tysh(&source, 72.0, None)).unwrap();
    tysh.transform = Affine { m: [4.1667, 0.0, 0.0, 4.1667, -32375.0, -32887.5] };
    let ink = [8050.0, 8160.0, 8220.0, 8300.0];
    let rect = |class| {
        D::Descriptor(
            Descriptor::new(class)
                .with("Left", D::UnitFloat { unit: *b"#Pnt", value: ink[0] })
                .with("Top ", D::UnitFloat { unit: *b"#Pnt", value: ink[1] })
                .with("Rght", D::UnitFloat { unit: *b"#Pnt", value: ink[2] })
                .with("Btom", D::UnitFloat { unit: *b"#Pnt", value: ink[3] }),
        )
    };
    tysh.text.items.retain(|(key, _)| !key.is("bounds") && !key.is("boundingBox"));
    tysh.text.items.push((Id::new("bounds"), rect("bounds")));
    tysh.text.items.push((Id::new("boundingBox"), rect("boundingBox")));
    let data = crate::psd::write_tysh(&tysh);

    let imported = crate::psd::text_layer_from_tysh(&data, 72.0).unwrap();
    let expected = [4.1667, 0.0, 0.0, 4.1667, 1166.935, 1112.772];
    for (got, want) in imported.transform.m.iter().zip(expected) {
        assert!((got - want).abs() < 0.001, "{:?}", imported.transform.m);
    }
    let (_, rendered) = TextEngine::new().render(&imported, 72.0, PixelFormat::RGBA8);
    let placed = rendered.surface.content_bounds();
    assert!(!placed.is_empty(), "the normalized layer renders");
    assert!((1000..3000).contains(&placed.x0) && (500..2000).contains(&placed.y0), "{placed:?}");

    let round_trip = crate::psd::text_layer_from_tysh(&crate::psd::build_tysh(&imported, 72.0, Some(ink.map(|v| v as f32))), 72.0).unwrap();
    for (got, want) in round_trip.transform.m.iter().zip(expected) {
        assert!((got - want).abs() < 0.001, "{:?}", round_trip.transform.m);
    }
}

/// storytold/photocraft#3617: Reference keeps Stylistic Sets, Contextual Alternates, Swash and
/// the other OpenType settings besides (discretionary) ligatures only in `Txt2`. Import reads
/// them, so the layout our engine draws when the type is edited keeps Reference's glyphs.
#[test]
fn txt2_carries_opentype_features() {
    let feats = |l: &TextLayer| l.char_runs().iter().map(|r| r.style.features.iter().map(|f| (f.tag.clone(), f.value)).collect::<Vec<_>>()).collect::<Vec<_>>();
    let f = |tag: &str, value: u16| (tag.to_string(), value);
    let text = "Il->36";
    let plain = styled(text, CharStyle { font_family: "Inter".into(), ..Default::default() });
    let tysh = with_text_index(&crate::psd::build_tysh(&plain, 72.0, None), 0);
    let styles = [(2, "/11 1 /18 true /19 false /20 true /25 true /85 2".to_string()), (5, "/11 1 /20 false /23 true /85 1".to_string())];
    let txt2 = crate::psd::parse_txt2(&txt2_with_styles(text, &styles)).unwrap();
    let mut back = crate::psd::text_layer_from_tysh(&tysh, 72.0).unwrap();
    assert_eq!(feats(&back), vec![Vec::<(String, u16)>::new()], "EngineData alone doesn't have them");
    crate::psd::apply_txt2(&mut back, &tysh, &txt2);
    assert_eq!(feats(&back), vec![vec![f("calt", 1), f("swsh", 1), f("ss02", 1)], vec![f("calt", 0), f("frac", 1), f("ss01", 1)]]);
    assert_eq!(back.char_runs().iter().map(|r| r.len).collect::<Vec<_>>(), vec![2, 4]);
    // A feature the style already sets keeps its value; masks and flags of the wrong type, masks
    // beyond the 20 sets and unknown keys change nothing.
    let mut t = crate::psd::text_layer_from_tysh(&tysh, 72.0).unwrap();
    t.runs = vec![TextRun { len: text.len(), style: CharStyle { features: vec![FontFeature { tag: "ss01".into(), value: 0 }], ..Default::default() } }];
    let odd = [(1, "/85 1".to_string()), (1, "/85 (x) /20 (x) /25 (true)".to_string()), (1, "/85 -1".to_string()), (4, "/85 99999999999 /999 true".to_string())];
    crate::psd::apply_txt2(&mut t, &tysh, &crate::psd::parse_txt2(&txt2_with_styles(text, &odd)).unwrap());
    assert_eq!(feats(&t), vec![vec![f("ss01", 0)]]);
}

/// `set_text_index` patches the integer in place, leaving every other byte as it was.
#[test]
fn set_text_index_patches_in_place() {
    let layer = TextLayer { text: "Hi".into(), ..Default::default() };
    let tysh = crate::psd::build_tysh(&layer, 72.0, None);
    assert_eq!(crate::psd::text_index(&tysh), Some(0));
    let out = crate::psd::set_text_index(&tysh, 7).unwrap();
    assert_eq!(out.len(), tysh.len());
    assert_eq!(crate::psd::text_index(&out), Some(7));
    assert_eq!(out.iter().zip(&tysh).filter(|(a, b)| a != b).count(), 1);
    assert_eq!(crate::psd::set_text_index(&tysh, i64::MAX), None);
    assert_eq!(crate::psd::set_text_index(b"junk", 1), None);
}

/// A `TySh` gives its recorded text box only while it still holds the layer's text.
#[test]
fn tysh_text_box_follows_the_text() {
    let mut layer = TextLayer { text: "Warp me".into(), ..Default::default() };
    layer.runs = vec![TextRun { len: layer.text.len(), style: CharStyle { size_pt: 24.0, ..Default::default() } }];
    layer.sync_summary();
    let tysh = crate::psd::build_tysh(&layer, 72.0, Some([0.0, -20.0, 90.0, 8.0]));
    assert_eq!(crate::psd::tysh_text_box(&tysh, "Warp me"), Some([0.0, -20.0, 90.0, 8.0]));
    assert_eq!(crate::psd::tysh_text_box(&tysh, "Edited"), None);
    assert_eq!(crate::psd::tysh_text_box(b"junk", "Warp me"), None);
}
