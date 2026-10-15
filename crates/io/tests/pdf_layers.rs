//! PDF pages open as editable layers (`photosplice_io::pdf::open_page`): synthetic one-page PDFs
//! written here, one feature each.

use photosplice_doc::{Document, Fill, LayerContent};
use photosplice_io::pdf::{PageAs, open_page};

/// A one-page PDF, 200 × 100 pt, with `content` as its content stream and `resources` (the
/// inside of the page's resource dictionary). `extra` objects are numbered from 6.
fn pdf(content: &str, resources: &str, extra: &[String]) -> Vec<u8> {
    pdf_page("", content, resources, extra)
}

/// [`pdf`] with `page` added to the page dictionary (boxes, `/Rotate`, …).
fn pdf_page(page: &str, content: &str, resources: &str, extra: &[String]) -> Vec<u8> {
    pdf_full("", page, content, resources, extra)
}

/// [`pdf_page`] with `catalog` added to the catalog dictionary (`/OCProperties`, …).
fn pdf_full(catalog: &str, page: &str, content: &str, resources: &str, extra: &[String]) -> Vec<u8> {
    let mut objs: Vec<String> = vec![
        format!("<< /Type /Catalog /Pages 2 0 R {catalog} >>"),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] {page} /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> {resources} >> >>"),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
    ];
    objs.extend(extra.iter().cloned());
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

fn open(bytes: &[u8]) -> Document {
    open_page("t.pdf", bytes, 0, 72.0, PageAs::Layers).expect("opens").document
}

fn kinds(d: &Document) -> Vec<&'static str> {
    d.layers
        .iter()
        .map(|l| match &l.content {
            LayerContent::Text(_) => "text",
            LayerContent::Shape(_) => "shape",
            LayerContent::Raster(_) => "pixels",
            LayerContent::Group(_) => "group",
            LayerContent::Fill(_) => "fill",
            _ => "other",
        })
        .collect()
}

#[test]
fn text_becomes_a_type_layer() {
    let d = open(&pdf("BT /F1 24 Tf 1 0 0 rg 20 50 Td (Hello World) Tj ET", "", &[]));
    assert_eq!((d.size.width, d.size.height), (200, 100));
    assert_eq!(kinds(&d), ["text"], "{:?}", d.layers.iter().map(|l| &l.name).collect::<Vec<_>>());
    let LayerContent::Text(t) = &d.layers[0].content else { unreachable!() };
    assert_eq!(t.text, "Hello World");
    assert!((t.size_pt - 24.0).abs() < 0.1, "{}", t.size_pt);
    assert_eq!(t.color.to_rgba8(), [255, 0, 0, 255]);
    let [.., e, f] = t.transform.m;
    assert!((e - 20.0).abs() < 0.5 && (f - 50.0).abs() < 0.5, "baseline at ({e}, {f})");
    let px = t.cache.as_ref().expect("the PDF's glyphs as the layer's pixels").content_bounds();
    assert!(!px.is_empty() && px.x0 >= 19 && px.y1 <= 56, "{px:?}");
}

fn texts(d: &Document) -> Vec<String> {
    d.layers.iter().filter_map(|l| if let LayerContent::Text(t) = &l.content { Some(t.text.clone()) } else { None }).collect()
}

#[test]
fn words_set_apart_get_their_space_and_a_paragraphs_lines_one_layer() {
    // TJ kerning that moves a word along instead of drawing a space, then two more lines 20 pt
    // apart; a line indented elsewhere starts a layer of its own.
    let d = open(&pdf("BT /F1 12 Tf 10 80 Td [(Hello) -300 (there)] TJ 0 -20 Td (Next) Tj 0 -20 Td (Last) Tj 90 -20 Td (Aside) Tj ET", "", &[]));
    assert_eq!(texts(&d), ["Hello there\rNext\rLast", "Aside"]);
    let LayerContent::Text(t) = &d.layers[0].content else { unreachable!() };
    assert_eq!(t.runs.first().and_then(|r| r.style.leading_pt), Some(20.0));
    assert_eq!(d.layers[0].name, "Hello there");
}

#[test]
fn fill_and_stroke_make_one_shape_and_same_colour_fills_merge() {
    let d = open(&pdf(
        "0 0 1 rg 1 0 0 RG 4 w 10 10 50 30 re B  0 1 0 rg 100 10 20 20 re f 130 10 20 20 re f  0 0 0 rg 160 10 20 20 re f",
        "",
        &[],
    ));
    assert_eq!(kinds(&d), ["shape", "shape", "shape"]);
    let shape = |i: usize| match &d.layers[i].content {
        LayerContent::Shape(s) => s,
        _ => unreachable!(),
    };
    let s = shape(0);
    assert!(matches!(&s.fill, Some(Fill::Solid(c)) if c.to_rgba8() == [0, 0, 255, 255]));
    let st = s.stroke.as_ref().expect("the B stroke joins its fill");
    assert!((st.width - 4.0).abs() < 0.01);
    assert!(matches!(&st.paint, Fill::Solid(c) if c.to_rgba8() == [255, 0, 0, 255]));
    assert_eq!(shape(1).path.subpaths.len(), 2, "two green squares, one layer");
    assert!(s.cache.as_ref().is_some_and(|c| !c.content_bounds().is_empty()), "rendered");
}

#[test]
fn fill_opacity_is_kept() {
    let d = open(&pdf("/G gs 1 0 0 rg 10 10 50 50 re f", "/ExtGState << /G << /ca 0.5 >> >>", &[]));
    let LayerContent::Shape(s) = &d.layers[0].content else { panic!("{:?}", kinds(&d)) };
    let Some(Fill::Solid(c)) = &s.fill else { panic!() };
    assert!((c.alpha - 0.5).abs() < 0.01, "{}", c.alpha);
}

#[test]
fn images_become_pixel_layers_where_they_land() {
    let raw = [255u8, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255];
    let bytes = pdf_with_image("q 40 0 0 20 30 40 cm /Im0 Do Q", &raw);
    let d = open(&bytes);
    assert_eq!(kinds(&d), ["pixels"]);
    let LayerContent::Raster(s) = &d.layers[0].content else { unreachable!() };
    let b = s.content_bounds();
    assert_eq!((b.x0, b.y0, b.width(), b.height()), (30, 40, 40, 20), "{b:?}");
}

/// [`pdf`] with an image XObject `Im0` whose samples are `raw` (2 × 2 RGB).
fn pdf_with_image(content: &str, raw: &[u8]) -> Vec<u8> {
    let page = pdf(content, "/XObject << /Im0 6 0 R >>", &["@IMAGE@".into()]);
    let head = format!("<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length {} >>\nstream\n", raw.len());
    let mut obj = head.into_bytes();
    obj.extend_from_slice(raw);
    obj.extend_from_slice(b"\nendstream");
    // Splice the object in and rebuild the cross-reference table.
    let text = String::from_utf8_lossy(&page).into_owned();
    let body_end = text.find("xref\n").unwrap();
    let mut out = page[..body_end].to_vec();
    let at = out.windows(7).position(|w| w == b"@IMAGE@").unwrap();
    out.splice(at..at + 7, obj);
    let mut offsets = Vec::new();
    for n in 1..=6 {
        let tag = format!("{n} 0 obj\n");
        offsets.push(out.windows(tag.len()).position(|w| w == tag.as_bytes()).unwrap());
    }
    let xref = out.len();
    out.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes());
    out
}

#[test]
fn clips_become_vector_masks() {
    let d = open(&pdf("q 20 20 40 40 re W n 1 0 0 rg 0 0 200 100 re f Q", "", &[]));
    assert_eq!(kinds(&d), ["shape"]);
    let mask = d.layers[0].vector_mask.as_ref().expect("the clip");
    assert_eq!(mask.path.subpaths.len(), 1);
}

/// Opened as layers and as pixels, the page looks the same (premultiplied mean difference).
fn looks_like_the_page(bytes: &[u8]) -> f32 {
    let layers = open_page("t.pdf", bytes, 0, 72.0, PageAs::Layers).unwrap().document;
    let pixels = open_page("t.pdf", bytes, 0, 72.0, PageAs::Pixels).unwrap().document;
    let (a, b) = (photosplice_compose::flatten(&layers), photosplice_compose::flatten(&pixels));
    let pm = |p: &[f32; 4], c: usize| if c == 3 { p[3] } else { p[c] * p[3] };
    a.px.iter().zip(&b.px).map(|(p, q)| (0..4).map(|c| (pm(p, c) - pm(q, c)).abs()).sum::<f32>() / 4.0).sum::<f32>() / a.px.len() as f32
}

#[test]
fn shadings_become_gradient_fill_layers_in_their_place() {
    let shading = "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 200 0] /Function << /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >> >>";
    let bytes = pdf("0 1 0 rg 0 0 50 50 re f /Sh0 sh 0 0 0 rg 150 50 40 40 re f", "/Shading << /Sh0 6 0 R >>", &[shading.into()]);
    let d = open(&bytes);
    assert_eq!(kinds(&d), ["shape", "fill", "shape"], "the shading keeps its place in the stack");
    let LayerContent::Fill(Fill::Gradient { style, stops, .. }) = &d.layers[1].content else { panic!("{:?}", d.layers[1].content) };
    assert_eq!(*style, photosplice_doc::GradientStyle::Linear);
    assert_eq!(stops.len(), 2, "a linear ramp needs two stops: {stops:?}");
    assert!(d.layers[1].vector_mask.is_none(), "it fills the page");
    let diff = looks_like_the_page(&bytes);
    assert!(diff < 0.01, "{diff}");
}

#[test]
fn a_radial_pattern_fill_is_a_masked_radial_gradient() {
    let pattern = "<< /PatternType 2 /Shading << /ShadingType 3 /ColorSpace /DeviceRGB /Coords [80 50 0 80 50 45] /Extend [true true] /Function << /FunctionType 2 /Domain [0 1] /C0 [1 1 0] /C1 [0 0.5 1] /N 1 >> >> >>";
    let bytes = pdf("/Pattern cs /P0 scn 20 10 120 80 re f", "/Pattern << /P0 6 0 R >>", &[pattern.into()]);
    let d = open(&bytes);
    assert_eq!(kinds(&d), ["fill"]);
    let LayerContent::Fill(Fill::Gradient { style, .. }) = &d.layers[0].content else { panic!() };
    assert_eq!(*style, photosplice_doc::GradientStyle::Radial);
    assert!(d.layers[0].vector_mask.is_some(), "the rectangle masks it");
    let diff = looks_like_the_page(&bytes);
    assert!(diff < 0.01, "{diff}");
}

#[test]
fn shadings_a_photoshop_gradient_cant_hold_stay_pixels() {
    // A radial whose focus moved, and a shading under a clip with another fill rule.
    let focal = "<< /PatternType 2 /Shading << /ShadingType 3 /ColorSpace /DeviceRGB /Coords [60 50 0 90 50 45] /Extend [true true] /Function << /FunctionType 2 /Domain [0 1] /C0 [1 1 0] /C1 [0 0.5 1] /N 1 >> >> >>";
    let d = open(&pdf("/Pattern cs /P0 scn 20 10 120 80 re f", "/Pattern << /P0 6 0 R >>", &[focal.into()]));
    assert_eq!(kinds(&d), ["pixels"]);
    // A focus nudged by less than a pixel, as Illustrator often writes one, is a plain radial.
    let nudged = focal.replace("[60 50 0 90 50 45]", "[90.6 50 0 90 50 45]");
    let d = open(&pdf("/Pattern cs /P0 scn 20 10 120 80 re f", "/Pattern << /P0 6 0 R >>", &[nudged]));
    assert_eq!(kinds(&d), ["fill"]);
}

#[test]
fn a_clipped_shading_is_masked_by_the_clip() {
    let pattern = "<< /PatternType 2 /Shading << /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 0 100] /Extend [true true] /Function << /FunctionType 2 /Domain [0 1] /C0 [0 1 0] /C1 [1 0 1] /N 1 >> >> >>";
    let bytes = pdf("q 30 20 m 170 20 l 100 90 h W n /Pattern cs /P0 scn 10 10 180 80 re f Q", "/Pattern << /P0 6 0 R >>", &[pattern.into()]);
    let d = open(&bytes);
    assert_eq!(kinds(&d), ["fill"]);
    let m = d.layers[0].vector_mask.as_ref().expect("masked");
    assert!(m.path.subpaths.iter().any(|s| s.op == photosplice_doc::vector::PathOp::Intersect), "the rectangle, intersected with the clip");
    let diff = looks_like_the_page(&bytes);
    assert!(diff < 0.01, "{diff}");
}

#[test]
fn garbage_is_an_error_not_a_panic() {
    assert!(open_page("x.pdf", b"%PDF-1.7\nnot really", 0, 72.0, PageAs::Layers).is_err());
    let good = pdf("BT /F1 12 Tf 10 10 Td (x) Tj ET", "", &[]);
    for cut in [10, good.len() / 2, good.len() - 20] {
        let _ = open_page("x.pdf", &good[..cut], 0, 72.0, PageAs::Layers);
    }
}

#[test]
fn a_page_sized_clip_masks_nothing() {
    let d = open(&pdf("q 0 0 200 100 re W n 1 0 0 rg 10 10 50 50 re f Q", "", &[]));
    assert_eq!(kinds(&d), ["shape"]);
    assert!(d.layers[0].vector_mask.is_none());
}

/// The layers, composited, look like the page rendered as pixels.
#[test]
fn layers_look_like_the_page() {
    let shading = "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 200 0] /Function << /FunctionType 2 /Domain [0 1] /C0 [1 1 0] /C1 [0 1 1] /N 1 >> >>";
    let content = "q 0 0 200 40 re W n /Sh0 sh Q  0 0 1 rg 1 0 0 RG 3 w 10 50 60 40 re B  q 120 50 40 40 re W n 0 0.5 0 rg 100 40 80 80 re f Q  BT /F1 18 Tf 0 0 0 rg 20 15 Td (PhotoSplice PDF) Tj ET";
    let bytes = pdf(content, "/Shading << /Sh0 6 0 R >>", &[shading.into()]);
    let layers = open(&bytes);
    let pixels = open_page("t.pdf", &bytes, 0, 144.0, PageAs::Pixels).unwrap().document;
    let layers2 = open_page("t.pdf", &bytes, 0, 144.0, PageAs::Layers).unwrap().document;
    assert!(layers.layers.len() >= 4, "{:?}", kinds(&layers));
    let (a, b) = (photosplice_compose::flatten(&layers2), photosplice_compose::flatten(&pixels));
    assert_eq!(a.px.len(), b.px.len());
    let diff = a.px.iter().zip(&b.px).map(|(p, q)| (0..4).map(|c| (p[c] - q[c]).abs()).sum::<f32>()).sum::<f32>() / a.px.len() as f32;
    assert!(diff < 0.01, "mean difference {diff}");
}

/// `PHOTOSPLICE_PDF_PROBE=<file.pdf> cargo test -p photosplice-io --test pdf_layers probe -- --ignored --nocapture`:
/// how a real file opens (layer kinds, fonts, and how far the layers are from the pixels).
#[test]
#[ignore]
fn probe() {
    let Some(path) = std::env::var_os("PHOTOSPLICE_PDF_PROBE") else { return };
    let bytes = std::fs::read(&path).unwrap();
    let dpi = 150.0;
    let t = std::time::Instant::now();
    let layers = open_page("p.pdf", &bytes, 0, dpi, PageAs::Layers).unwrap();
    let took = t.elapsed();
    let pixels = open_page("p.pdf", &bytes, 0, dpi, PageAs::Pixels).unwrap().document;
    let mut count = std::collections::BTreeMap::new();
    let mut fonts = std::collections::BTreeMap::new();
    fn walk(ls: &[photosplice_doc::Layer], count: &mut std::collections::BTreeMap<&'static str, usize>, fonts: &mut std::collections::BTreeMap<String, usize>) {
        for l in ls {
            let k = match &l.content {
                LayerContent::Text(t) => {
                    *fonts.entry(format!("{} / {:?}", t.font_family, t.runs.first().and_then(|r| r.style.postscript_name.clone()))).or_default() += 1;
                    "text"
                }
                LayerContent::Shape(_) => "shape",
                LayerContent::Raster(_) => "pixels",
                LayerContent::Group(g) => {
                    walk(&g.children, count, fonts);
                    "group"
                }
                _ => "other",
            };
            *count.entry(k).or_default() += 1;
        }
    }
    walk(&layers.document.layers, &mut count, &mut fonts);
    let top: Vec<String> = layers.document.layers.iter().map(|l| format!("{}{}", l.name, if l.visible { "" } else { " (hidden)" })).collect();
    eprintln!("top level: {top:?}");
    let (a, b) = (photosplice_compose::flatten(&layers.document), photosplice_compose::flatten(&pixels));
    assert_eq!(a.rect, b.rect, "both ways open at one size");
    // Premultiplied, so colour under no coverage doesn't count.
    let pm = |p: &[f32; 4], c: usize| if c == 3 { p[3] } else { p[c] * p[3] };
    let diff = a.px.iter().zip(&b.px).map(|(p, q)| (0..4).map(|c| (pm(p, c) - pm(q, c)).abs()).sum::<f32>()).sum::<f32>() / a.px.len() as f32;
    let off = a.px.iter().zip(&b.px).filter(|(p, q)| (0..4).any(|c| (pm(p, c) - pm(q, c)).abs() > 0.1)).count() as f32 / a.px.len() as f32;
    if let Some(dir) = std::env::var_os("PHOTOSPLICE_PDF_PROBE_OUT") {
        let save = |buf: &photosplice_compose::Buffer, name: &str| {
            let img = buf.to_rgba8();
            let im = photosplice_codecs::Image::from_u8(img.width, img.height, photosplice_codecs::ChannelLayout::Rgba, img.pixels).unwrap();
            let png = photosplice_codecs::encode(&im, photosplice_codecs::Format::Png, &Default::default()).unwrap();
            std::fs::write(std::path::Path::new(&dir).join(name), png).unwrap();
        };
        save(&a, "layers.png");
        save(&b, "pixels.png");
    }
    eprintln!("{} in {took:?}: {count:?}\nfonts: {fonts:#?}\nwarnings: {:?}\nmean diff {diff:.4}, pixels off by >10%: {:.2}%", path.to_string_lossy(), layers.warnings, off * 100.0);
}

// ---- saving ----

/// A 200 × 100 document at 72 ppi: a blue rectangle with a red stroke, a line of type, an image.
fn sample() -> Document {
    use photosplice_doc::text::{CharStyle, TextRun, TextShape};
    use photosplice_doc::vector::{Path, ShapeLayer, ShapeStroke, Subpath};
    use photosplice_doc::{Layer, TextLayer};
    let mut doc = Document::new("sample", photosplice_geom::Size::new(200, 100), photosplice_color::ColorMode::Rgb, photosplice_color::SampleType::U8);
    let fmt = doc.pixel_format();
    let mut sh = ShapeLayer {
        path: Path::new(vec![Subpath::polygon(&[(10.0, 10.0), (90.0, 10.0), (90.0, 60.0), (10.0, 60.0)])]),
        fill: Some(Fill::Solid(photosplice_color::Color::rgb(0.0, 0.0, 1.0))),
        stroke: Some(ShapeStroke { width: 4.0, paint: Fill::Solid(photosplice_color::Color::rgb(1.0, 0.0, 0.0)), ..Default::default() }),
        ..Default::default()
    };
    sh.cache = Some(photosplice_vector::render_shape(&sh, fmt, doc.bounds()));
    doc.layers.push(Layer::new("Box", LayerContent::Shape(sh)));
    let mut img = photosplice_raster::Surface::new(fmt);
    img.fill_rect(photosplice_geom::Rect::new(120, 10, 180, 50), &[0.0, 0.8, 0.0, 1.0]);
    doc.layers.push(Layer::new("Green", LayerContent::Raster(img)));
    let style = CharStyle { font_family: "Inter".into(), size_pt: 18.0, color: photosplice_color::Color::rgb(0.0, 0.0, 0.0), ..Default::default() };
    let mut t = TextLayer { text: "Saved".into(), transform: photosplice_geom::Affine::translate(20.0, 90.0), runs: vec![TextRun { len: 5, style }], shape: TextShape::Point, ..Default::default() };
    t.sync_summary();
    photosplice_text::shared().lock().unwrap().render_layer(&mut t, 72.0, fmt);
    doc.layers.push(Layer::new("Saved", LayerContent::Text(t)));
    doc
}

fn mean_diff(a: &Document, b: &Document) -> f32 {
    let (a, b) = (photosplice_compose::flatten(a), photosplice_compose::flatten(b));
    assert_eq!(a.px.len(), b.px.len());
    a.px.iter().zip(&b.px).map(|(p, q)| (0..4).map(|c| (p[c] - q[c]).abs()).sum::<f32>()).sum::<f32>() / a.px.len() as f32
}

#[test]
fn a_saved_pdf_reopens_with_its_layers() {
    let doc = sample();
    let r = photosplice_io::pdf::export::export_layered(&doc, true).unwrap();
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    let bytes = r.bytes;
    // Photoshop's layout: the page's PieceInfo, its private data in Photoshop's keys.
    let text = String::from_utf8_lossy(&bytes);
    for key in ["/PieceInfo", "/AdobePhotoshop", "/StandardImageFileData", "/ImageResources", "/CompositeImage", "/RoundTrip true"] {
        assert!(text.contains(key), "{key}");
    }
    assert!(photosplice_io::pdf::has_editing_data(&bytes, 0));
    // As Photoshop does, the editing data wins over how the page was asked to open.
    for how in [PageAs::Layers, PageAs::Pixels] {
        let back = open_page("s.pdf", &bytes, 0, 300.0, how).unwrap();
        assert!(back.warnings.is_empty(), "{:?}", back.warnings);
        let back = back.document;
        let names: Vec<&str> = back.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Box", "Green", "Saved"]);
        assert_eq!(kinds(&back), ["shape", "pixels", "text"]);
        assert_eq!((back.size.width, back.size.height), (200, 100), "the document's own size, not the page at 300 ppi");
        assert!(mean_diff(&doc, &back) < 0.01);
    }
    // Other readers still see the one page (the editing data is an incremental update).
    assert_eq!(photosplice_io::pdf::page_count(&bytes).unwrap(), 1);
}

#[test]
fn broken_editing_data_opens_the_page_instead() {
    let bytes = photosplice_io::pdf::export::export_layered(&sample(), true).unwrap().bytes;
    // Overwrite the start of the layer data's Flate stream.
    let find = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).position(|w| w == needle).unwrap();
    let key = b"/StandardImageFileData ";
    let at = find(&bytes, key) + key.len();
    let num: Vec<u8> = bytes[at..].iter().copied().take_while(u8::is_ascii_digit).collect();
    let obj = find(&bytes, &[b"\n", &num[..], b" 0 obj"].concat());
    let data = obj + find(&bytes[obj..], b"stream\n") + b"stream\n".len();
    let mut bad = bytes.clone();
    bad[data..data + 16].fill(0xAB);
    assert!(!photosplice_io::pdf::has_editing_data(b"%PDF-1.4 nonsense", 0));
    let r = open_page("s.pdf", &bad, 0, 72.0, PageAs::Layers).unwrap();
    assert!(r.warnings.iter().any(|w| w.contains("editing data")), "{:?}", r.warnings);
    assert_eq!(kinds(&r.document), ["shape", "pixels", "text"], "the page's own layers");
}

#[test]
fn a_saved_pdf_is_vector_and_text_and_looks_like_the_document() {
    let doc = sample();
    let r = photosplice_io::pdf::export::export_layered(&doc, false).unwrap();
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    let text = String::from_utf8_lossy(&r.bytes);
    assert!(text.contains("/ToUnicode"), "the text can be copied");
    // Opened by any reader: the shape, the image and the type are there as such.
    let back = open_page("s.pdf", &r.bytes, 0, 72.0, PageAs::Layers).unwrap().document;
    assert_eq!(kinds(&back), ["shape", "pixels", "text"], "{:?}", back.layers.iter().map(|l| &l.name).collect::<Vec<_>>());
    assert_eq!(texts(&back), ["Saved"]);
    let pixels = open_page("s.pdf", &r.bytes, 0, 72.0, PageAs::Pixels).unwrap().document;
    let d = mean_diff(&doc, &pixels);
    assert!(d < 0.02, "mean difference {d}");
}

#[test]
fn adjustment_layers_flatten_what_they_work_on() {
    let mut doc = sample();
    let mut adj = photosplice_doc::Layer::new("Invert", LayerContent::Adjustment(photosplice_doc::Adjustment::Invert));
    adj.opacity = 1.0;
    doc.layers.insert(2, adj);
    let r = photosplice_io::pdf::export::export_layered(&doc, false).unwrap();
    let back = open_page("s.pdf", &r.bytes, 0, 72.0, PageAs::Layers).unwrap().document;
    assert_eq!(kinds(&back), ["pixels", "text"], "box, image and the inversion as one image; the type above stays type");
    let pixels = open_page("s.pdf", &r.bytes, 0, 72.0, PageAs::Pixels).unwrap().document;
    assert!(mean_diff(&doc, &pixels) < 0.02);
}

/// `PHOTOSPLICE_PDF_STRESS=<out.pdf>`: writes a heavy page (thousands of paths and text runs).
#[test]
#[ignore]
fn stress_page() {
    let Some(out) = std::env::var_os("PHOTOSPLICE_PDF_STRESS") else { return };
    let mut c = String::new();
    for i in 0..6000 {
        let (x, y) = ((i % 100) as f32 * 2.0, (i / 100) as f32 * 1.6);
        c.push_str(&format!("{} {} {} rg {x} {y} 1.5 1.2 re f\n", (i % 7) as f32 / 7.0, (i % 5) as f32 / 5.0, (i % 3) as f32 / 3.0));
    }
    for i in 0..600 {
        c.push_str(&format!("0 0 0 RG 0.3 w {} 0 m {} 100 l S\n", i as f32 / 3.0, i as f32 / 3.0 + 5.0));
    }
    c.push_str("BT /F1 2 Tf 0 0 0 rg\n");
    for i in 0..1500 {
        c.push_str(&format!("1 0 0 1 {} {} Tm (Line {i} of text) Tj\n", (i % 4) as f32 * 50.0, 99.0 - (i / 4) as f32 * 0.26));
    }
    c.push_str("ET\n");
    std::fs::write(out, pdf(&c, "", &[])).unwrap();
}


#[test]
fn editing_data_keeps_every_mode_and_depth() {
    use photosplice_color::{ColorMode, SampleType};
    for (mode, sample) in [(ColorMode::Cmyk, SampleType::U8), (ColorMode::Grayscale, SampleType::U16), (ColorMode::Lab, SampleType::U8), (ColorMode::Rgb, SampleType::F32)] {
        let mut doc = Document::with_background("m", photosplice_geom::Size::new(40, 30), mode, sample, photosplice_doc::Color::WHITE);
        let fmt = doc.pixel_format();
        let mut s = photosplice_raster::Surface::new(fmt);
        let ink = vec![0.25; fmt.channels()];
        s.fill_rect(photosplice_geom::Rect::new(5, 5, 25, 20), &ink);
        doc.layers.push(photosplice_doc::Layer::new("Ink", LayerContent::Raster(s)));
        let r = photosplice_io::pdf::export::export_layered(&doc, true).unwrap();
        assert!(photosplice_io::pdf::has_editing_data(&r.bytes, 0), "{mode:?} {sample:?}: {:?}", r.warnings);
        let back = open_page("m.pdf", &r.bytes, 0, 72.0, PageAs::Layers).unwrap().document;
        assert_eq!((back.mode, back.pixel_format().sample), (mode, sample), "{mode:?} {sample:?}");
        assert_eq!(back.layers.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["Background", "Ink"]);
        assert!(mean_diff(&doc, &back) < 0.01, "{mode:?} {sample:?}");
    }
}

#[test]
fn both_ways_open_a_page_at_one_size() {
    // 200 pt at 150 ppi is 416.67 px: the layers and the rendered page agree on 416.
    let mut c = String::new();
    for i in 0..300 {
        let (x, y) = ((i % 100) as f32 * 2.0, (i / 100) as f32 * 1.6);
        c.push_str(&format!("{} 0 0 rg {x} {y} 1.5 1.2 re f\n", (i % 7) as f32 / 7.0));
    }
    let bytes = pdf(&c, "", &[]);
    let layers = open_page("s.pdf", &bytes, 0, 150.0, PageAs::Layers).unwrap().document;
    let pixels = open_page("s.pdf", &bytes, 0, 150.0, PageAs::Pixels).unwrap().document;
    assert_eq!((layers.size.width, layers.size.height), (pixels.size.width, pixels.size.height));
    assert!(mean_diff(&layers, &pixels) < 0.01);
}

#[test]
fn without_editing_data_a_pdf_is_only_its_page() {
    let opts = photosplice_io::ExportOptions { pdf_discard_editing: true, ..Default::default() };
    let r = photosplice_io::export(&sample(), "s.pdf", &opts).unwrap();
    assert!(!photosplice_io::pdf::has_editing_data(&r.bytes, 0));
    let kept = photosplice_io::export(&sample(), "s.pdf", &Default::default()).unwrap();
    assert!(photosplice_io::pdf::has_editing_data(&kept.bytes, 0), "kept by default, as in Photoshop");
}

#[test]
fn crop_to_picks_the_page_box() {
    use photosplice_io::pdf::{CropTo, OpenOptions, open_page_with};
    // A red 20 × 10 pt mark at (120, 60)–(140, 70) in PDF space (y up).
    let boxes = "/CropBox [10 10 190 90] /TrimBox [20 20 180 80] /ArtBox [100 50 160 80]";
    let bytes = pdf_page(boxes, "1 0 0 rg 120 60 20 10 re f", "", &[]);
    for how in [PageAs::Pixels, PageAs::Layers] {
        let size = |crop_to| {
            let d = open_page_with("b.pdf", &bytes, &OpenOptions { page: 0, dpi: 72.0, how, crop_to }).unwrap().document;
            (d.size.width, d.size.height, d)
        };
        assert_eq!((size(CropTo::MediaBox).0, size(CropTo::MediaBox).1), (200, 100), "{how:?}");
        assert_eq!((size(CropTo::CropBox).0, size(CropTo::CropBox).1), (180, 80));
        assert_eq!((size(CropTo::TrimBox).0, size(CropTo::TrimBox).1), (160, 60));
        assert_eq!((size(CropTo::ArtBox).0, size(CropTo::ArtBox).1), (60, 30));
        assert_eq!((size(CropTo::BleedBox).0, size(CropTo::BleedBox).1), (180, 80), "no bleed box: the crop box");
        // The bounding box is the mark, and the mark fills it.
        let (w, h, d) = size(CropTo::BoundingBox);
        assert_eq!((w, h), (20, 10), "{how:?}");
        let px = photosplice_compose::flatten(&d).px[5 * 20 + 10];
        assert!(px[0] > 0.9 && px[3] > 0.9, "{how:?} {px:?}");
        // In the art box (top left at 100, 80) the mark sits 20 pt right and 10 pt down.
        let (_, _, d) = size(CropTo::ArtBox);
        let buf = photosplice_compose::flatten(&d);
        assert!(buf.px[15 * 60 + 30][3] > 0.9 && buf.px[5 * 60 + 30][3] < 0.1, "{how:?}");
    }
}

#[test]
fn crop_to_follows_the_page_rotation() {
    use photosplice_io::pdf::{CropTo, OpenOptions, open_page_with};
    let bytes = pdf_page("/Rotate 90 /CropBox [0 0 100 100]", "0 0 1 rg 0 0 10 10 re f", "", &[]);
    for crop_to in [CropTo::MediaBox, CropTo::CropBox, CropTo::BoundingBox] {
        let d = open_page_with("r.pdf", &bytes, &OpenOptions { page: 0, dpi: 72.0, how: PageAs::Pixels, crop_to }).unwrap().document;
        let want = match crop_to {
            CropTo::MediaBox => (100, 200),
            CropTo::CropBox => (100, 100),
            _ => (10, 10),
        };
        assert_eq!((d.size.width, d.size.height), want, "{crop_to:?}");
    }
    assert!(open_page_with("e.pdf", &pdf("", "", &[]), &OpenOptions { crop_to: CropTo::BoundingBox, dpi: 72.0, ..Default::default() }).unwrap().warnings.iter().any(|w| w.contains("empty")));
}

#[test]
fn artboards_save_as_vectors_on_their_background() {
    use photosplice_doc::{Artboard, ArtboardBackground, Layer};
    let mut doc = sample();
    let shapes: Vec<Layer> = doc.layers.drain(..1).collect();
    let mut board = Layer::group("Board", shapes);
    if let LayerContent::Group(g) = &mut board.content {
        let mut ab = Artboard::new(photosplice_geom::Rect::new(0, 0, 120, 80));
        ab.background = ArtboardBackground::Black;
        g.artboard = Some(ab);
    }
    doc.layers.insert(0, board);
    let r = photosplice_io::pdf::export::export_layered(&doc, false).unwrap();
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    let back = open_page("a.pdf", &r.bytes, 0, 72.0, PageAs::Layers).unwrap().document;
    // The board's black background and the box on it as shapes, not one image.
    assert!(kinds(&back).iter().filter(|k| **k == "shape").count() >= 2, "{:?}", kinds(&back));
    let px = photosplice_compose::flatten(&back).px[70 * 200 + 110];
    assert!(px[3] > 0.9 && px[0] < 0.1 && px[2] < 0.1, "black board: {px:?}");
}

// ---- gradients ----

fn gradient(style: photosplice_doc::GradientStyle, angle: f32, reverse: bool) -> Fill {
    use photosplice_color::Color as C;
    Fill::Gradient {
        stops: vec![(0.0, C::rgb(1.0, 0.0, 0.0)), (0.6, C::rgb(0.0, 0.2, 1.0)), (1.0, C::rgb(1.0, 0.9, 0.0))],
        angle,
        scale: 0.8,
        style,
        reverse,
        opacity_stops: Vec::new(),
        midpoints: Vec::new(),
        offset: (0.0, 0.0),
        dither: false,
        align: true,
    }
}

/// `doc` saved as a PDF (no editing data) and rendered back: no layer became an image, and it
/// looks like the compositor's rendering. (hayro samples shadings about half a pixel off the
/// pixel centre, which steep gradients show as a small even difference; macOS's renderer
/// draws these PDFs centred where the compositor does.)
fn saves_natively(doc: &Document, what: &str) {
    let r = photosplice_io::pdf::export::export_layered(doc, false).unwrap();
    assert!(r.warnings.is_empty(), "{what}: {:?}", r.warnings);
    let back = open_page("g.pdf", &r.bytes, 0, 72.0, PageAs::Pixels).unwrap().document;
    let (a, b) = (photosplice_compose::flatten(doc), photosplice_compose::flatten(&back));
    assert_eq!(a.rect, b.rect);
    let pm = |p: &[f32; 4], c: usize| if c == 3 { p[3] } else { p[c] * p[3] };
    let n = a.px.len() as f32;
    let mean = a.px.iter().zip(&b.px).map(|(p, q)| (0..4).map(|c| (pm(p, c) - pm(q, c)).abs()).sum::<f32>() / 4.0).sum::<f32>() / n;
    let off = a.px.iter().zip(&b.px).filter(|(p, q)| (0..4).any(|c| (pm(p, c) - pm(q, c)).abs() > 0.06)).count() as f32 / n;
    assert!(mean < 0.015 && off < 0.04, "{what}: mean {mean:.4}, {:.2}% of pixels off", off * 100.0);
}

#[test]
fn gradient_shapes_save_as_pdf_shadings() {
    use photosplice_doc::GradientStyle as G;
    use photosplice_doc::vector::{Path, ShapeLayer, ShapeStroke, Subpath};
    for (style, angle, reverse) in [(G::Linear, 0.0, false), (G::Linear, 30.0, true), (G::Reflected, 115.0, false), (G::Radial, 0.0, false), (G::Radial, 0.0, true), (G::Angle, 40.0, false)] {
        let mut doc = Document::new("g", photosplice_geom::Size::new(160, 100), photosplice_color::ColorMode::Rgb, photosplice_color::SampleType::U8);
        let fmt = doc.pixel_format();
        let mut sh = ShapeLayer {
            path: Path::new(vec![Subpath::polygon(&[(12.0, 10.0), (148.0, 14.0), (140.0, 90.0), (20.0, 84.0)])]),
            fill: Some(gradient(style, angle, reverse)),
            stroke: Some(ShapeStroke { width: 3.0, paint: gradient(G::Linear, 90.0, false), ..Default::default() }),
            ..Default::default()
        };
        sh.cache = Some(photosplice_vector::render_shape(&sh, fmt, doc.bounds()));
        doc.layers.push(photosplice_doc::Layer::new("Shape", LayerContent::Shape(sh)));
        saves_natively(&doc, &format!("shape {style:?} {angle}° reverse {reverse}"));
    }
}

#[test]
fn gradient_fill_layers_save_as_pdf_shadings() {
    use photosplice_doc::GradientStyle as G;
    use photosplice_doc::vector::{Path, Subpath, VectorMask};
    for (style, angle, reverse, masked) in [(G::Linear, 0.0, false, false), (G::Linear, 37.0, true, true), (G::Reflected, 90.0, false, false), (G::Radial, 0.0, false, true), (G::Angle, 200.0, true, false)] {
        let mut doc = Document::new("g", photosplice_geom::Size::new(160, 100), photosplice_color::ColorMode::Rgb, photosplice_color::SampleType::U8);
        let mut f = gradient(style, angle, reverse);
        if let Fill::Gradient { opacity_stops, midpoints, offset, .. } = &mut f {
            *opacity_stops = vec![(0.0, 1.0), (0.7, 0.4), (1.0, 0.9)];
            *midpoints = vec![0.3, 0.75];
            *offset = (0.1, -0.15);
        }
        let mut l = photosplice_doc::Layer::new("Gradient Fill", LayerContent::Fill(f));
        if masked {
            l.vector_mask = Some(VectorMask::new(Path::new(vec![Subpath::polygon(&[(30.0, 20.0), (130.0, 20.0), (120.0, 80.0), (40.0, 85.0)])])));
        }
        doc.layers.push(l);
        saves_natively(&doc, &format!("fill {style:?} {angle}° reverse {reverse} masked {masked}"));
    }
    // A solid colour fill layer, and a Diamond gradient (no PDF shading: an image).
    let mut doc = Document::new("s", photosplice_geom::Size::new(40, 30), photosplice_color::ColorMode::Rgb, photosplice_color::SampleType::U8);
    doc.layers.push(photosplice_doc::Layer::new("Color Fill", LayerContent::Fill(Fill::Solid(photosplice_color::Color::rgb(0.2, 0.6, 0.3)))));
    saves_natively(&doc, "solid fill");
    doc.layers[0].content = LayerContent::Fill(gradient(G::Diamond, 0.0, false));
    let r = photosplice_io::pdf::export::export_layered(&doc, false).unwrap();
    assert!(r.warnings.iter().any(|w| w.contains("as images")), "{:?}", r.warnings);
}




#[test]
fn a_gradient_fill_saved_to_pdf_reopens_as_a_gradient_fill() {
    use photosplice_doc::GradientStyle as G;
    for style in [G::Linear, G::Radial] {
        let mut doc = Document::new("g", photosplice_geom::Size::new(160, 100), photosplice_color::ColorMode::Rgb, photosplice_color::SampleType::U8);
        doc.layers.push(photosplice_doc::Layer::new("Gradient Fill", LayerContent::Fill(gradient(style, 25.0, false))));
        let bytes = photosplice_io::pdf::export::export_layered(&doc, false).unwrap().bytes;
        let back = open_page("g.pdf", &bytes, 0, 72.0, PageAs::Layers).unwrap().document;
        assert_eq!(kinds(&back), ["fill"], "{style:?}");
        let LayerContent::Fill(Fill::Gradient { style: s, .. }) = &back.layers[0].content else { panic!() };
        assert_eq!(*s, style);
        let diff = looks_like_the_page(&bytes);
        assert!(diff < 0.012, "{style:?}: {diff}");
    }
}

/// Three PDF layers (objects 6, 7, 8): "Back" on, "Hidden" off, "Locked art" on and locked, and
/// a grey square outside them all.
fn pdf_with_layers(content: &str, more: &[String]) -> Vec<u8> {
    let catalog = "/OCProperties << /OCGs [6 0 R 7 0 R 8 0 R] /D << /Order [6 0 R 7 0 R 8 0 R] /OFF [7 0 R] /Locked [8 0 R] >> >>";
    let mut extra = vec![
        "<< /Type /OCG /Name (Back) >>".to_string(),
        "<< /Type /OCG /Name <FEFF00480069006400640065006E> >>".to_string(),
        "<< /Type /OCG /Name (Locked art) >>".to_string(),
    ];
    extra.extend(more.iter().cloned());
    pdf_full(catalog, "", content, "/Properties << /oc1 6 0 R /oc2 7 0 R /oc3 8 0 R >> /XObject << /Fm 9 0 R >>", &extra)
}

fn group_children(d: &Document, i: usize) -> Vec<&'static str> {
    let Some(children) = d.layers[i].children() else { return Vec::new() };
    let sub = Document { layers: children.to_vec(), ..Document::new("g", photosplice_geom::Size::new(1, 1), photosplice_color::ColorMode::Rgb, photosplice_color::SampleType::U8) };
    kinds(&sub)
}

#[test]
fn pdf_layers_become_named_groups_hidden_ones_included() {
    let content = "0.5 g 0 0 10 10 re f \
        /OC /oc1 BDC 1 0 0 rg 20 20 50 50 re f EMC \
        /OC /oc2 BDC 0 0 1 rg 100 20 50 50 re f EMC \
        /OC /oc3 BDC BT /F1 12 Tf 0 g 10 80 Td (Hi) Tj ET EMC";
    let d = open(&pdf_with_layers(&content.replace('\\', ""), &["<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /Length 0 >>\nstream\n\nendstream".into()]));
    assert_eq!(kinds(&d), ["shape", "group", "group", "group"], "outside every layer at the bottom, then one group per layer");
    let names: Vec<&str> = d.layers.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(&names[1..], ["Back", "Hidden", "Locked art"], "names, a UTF-16 one too");
    assert_eq!(d.layers.iter().map(|l| l.visible).collect::<Vec<_>>(), [true, true, false, true]);
    assert!(d.layers[3].locks.all && !d.layers[1].locks.all);
    // Each group holds just its layer's content; the hidden layer's square came in too.
    assert_eq!(group_children(&d, 1), ["shape"]);
    assert_eq!(group_children(&d, 2), ["shape"]);
    assert_eq!(group_children(&d, 3), ["text"]);
}

#[test]
fn a_form_in_a_pdf_layer_lands_in_its_group() {
    let form = "<< /Type /XObject /Subtype /Form /BBox [0 0 200 100] /OC 7 0 R /Length 22 >>\nstream\n0 1 0 rg 0 0 40 40 re f\nendstream";
    let d = open(&pdf_with_layers("/Fm Do", &[form.into()]));
    assert_eq!(kinds(&d), ["group"], "only the layer the form is in, which is off");
    assert_eq!(d.layers[0].name, "Hidden");
    assert!(!d.layers[0].visible);
    assert_eq!(group_children(&d, 0), ["shape"]);
    // With content outside every layer too, the form's layer holds only the form's content.
    let d = open(&pdf_with_layers("0.5 g 0 0 10 10 re f /Fm Do", &[form.into()]));
    assert_eq!(kinds(&d), ["shape", "group"]);
    assert_eq!(group_children(&d, 1), ["shape"]);
}

#[test]
fn broken_layer_lists_open_as_one_stack() {
    // `/OCGs` that isn't an array, and a layer the page never uses: no groups, no panic.
    for catalog in ["/OCProperties << /OCGs 5 /D << >> >>", "/OCProperties << /OCGs [6 0 R] /D 3 >>", "/OCProperties 7"] {
        let bytes = pdf_full(catalog, "", "1 0 0 rg 20 20 50 50 re f", "", &["<< /Type /OCG /Name (Unused) >>".into()]);
        assert_eq!(kinds(&open(&bytes)), ["shape"], "{catalog}");
    }
}

/// A luminosity soft mask (object 7): white over the left half of the page, so only that half
/// shows. In `/GS0`.
fn soft_masked(content: &str) -> Vec<u8> {
    let gs = "/ExtGState << /GS0 << /SMask << /Type /Mask /S /Luminosity /G 7 0 R >> >> >>";
    let form = "<< /Type /XObject /Subtype /Form /BBox [0 0 200 100] /Group << /S /Transparency /CS /DeviceRGB >> /Length 24 >>\nstream\n1 g 0 0 100 100 re f\nendstream";
    pdf(content, gs, &["null".into(), form.into()])
}

#[test]
fn a_soft_masked_fill_is_a_shape_with_a_layer_mask() {
    let bytes = soft_masked("/GS0 gs 1 0 0 rg 0 0 200 100 re f");
    let d = open(&bytes);
    assert_eq!(kinds(&d), ["shape"]);
    let m = d.layers[0].mask.as_ref().expect("the soft mask is its layer mask");
    assert!(m.value(50, 50) > 0.99 && m.value(150, 50) < 0.01, "shows the left half only");
    assert!(looks_like_the_page(&bytes) < 0.01);
}

#[test]
fn a_soft_masked_group_is_a_masked_layer_group() {
    let group = "<< /Type /XObject /Subtype /Form /BBox [0 0 200 100] /Group << /S /Transparency >> /Length 46 >>\nstream\n0 0 1 rg 0 0 200 50 re f 1 0 0 rg 0 50 200 50 re f\nendstream";
    let gs = "/ExtGState << /GS0 << /SMask << /Type /Mask /S /Luminosity /G 7 0 R >> >> >> /XObject << /Fm 8 0 R >>";
    let form = "<< /Type /XObject /Subtype /Form /BBox [0 0 200 100] /Group << /S /Transparency /CS /DeviceRGB >> /Length 24 >>\nstream\n1 g 0 0 100 100 re f\nendstream";
    let bytes = pdf("/GS0 gs /Fm Do", gs, &["null".into(), form.into(), group.into()]);
    let d = open(&bytes);
    assert_eq!(kinds(&d), ["group"]);
    assert!(d.layers[0].mask.is_some());
    assert_eq!(group_children(&d, 0), ["shape", "shape"], "the group's content stays editable");
    assert!(looks_like_the_page(&bytes) < 0.01);
}
