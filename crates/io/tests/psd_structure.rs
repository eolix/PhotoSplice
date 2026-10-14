//! PSD (testgen / builder) → Document → PSD structural equality.

use photosuite_doc::LayerContent;
use photosuite_io::*;
use photosuite_psd::testgen;
use photosuite_psd::{ColorMode, Compression, LayerNode, PsdFile, SectionType, Version};

/// Sample of a layer channel in document coordinates (0 outside the rect).
fn sample(f: &PsdFile, li: usize, id: i16, x: i32, y: i32, cache: &mut std::collections::HashMap<(usize, i16), Vec<u8>>) -> Vec<u8> {
    let rec = &f.layers()[li];
    let r = rec.channel_rect(id);
    let bps = usize::from(f.header.depth / 8);
    if x < r.left || x >= r.right || y < r.top || y >= r.bottom || rec.channel(id).is_none() {
        let default = if id == -2 { rec.layer_mask().map_or(0, |m| m.default_color) } else { 0 };
        return if default == 0 || bps != 1 { vec![0; bps] } else { vec![default] };
    }
    let plane = cache.entry((li, id)).or_insert_with(|| rec.decode_channel(id, f.header.depth, f.header.version).unwrap_or_default());
    let w = (r.right - r.left) as usize;
    let i = ((y - r.top) as usize * w + (x - r.left) as usize) * bps;
    plane.get(i..i + bps).map(<[u8]>::to_vec).unwrap_or(vec![0; bps])
}

fn shape(f: &PsdFile, nodes: &[LayerNode]) -> String {
    nodes
        .iter()
        .map(|n| {
            let r = &f.layers()[n.index()];
            let st = r.section_type();
            let kids = if n.is_group() { format!("[{}]", shape(f, n.children())) } else { String::new() };
            format!("{}{}{}", r.name(), if st == SectionType::ClosedFolder { "(c)" } else { "" }, kids)
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn leaves(nodes: &[LayerNode], out: &mut Vec<usize>) {
    for n in nodes {
        out.push(n.index());
        leaves(n.children(), out);
    }
}

fn assert_structurally_equal(a: &PsdFile, b: &PsdFile) {
    assert_eq!(a.header.width, b.header.width);
    assert_eq!(a.header.height, b.header.height);
    assert_eq!(a.header.depth, b.header.depth);
    assert_eq!(a.header.color_mode, b.header.color_mode);
    let (ta, tb) = (a.layer_tree(), b.layer_tree());
    assert_eq!(shape(a, &ta), shape(b, &tb), "tree shape");
    let (mut la, mut lb) = (Vec::new(), Vec::new());
    leaves(&ta, &mut la);
    leaves(&tb, &mut lb);
    let cc = testgen::mode_channels(a.header.color_mode) as i16;
    let (mut ca, mut cb) = (Default::default(), Default::default());
    for (&ia, &ib) in la.iter().zip(&lb) {
        let (ra, rb) = (&a.layers()[ia], &b.layers()[ib]);
        let n = ra.name();
        assert_eq!(n, rb.name());
        let blend = if matches!(ra.blend_mode, photosuite_psd::BlendMode::Unknown(_)) { photosuite_psd::BlendMode::Normal } else { ra.blend_mode };
        if ra.section_type().is_folder() {
            assert_eq!(ra.section_divider().and_then(|s| s.blend_mode), rb.section_divider().and_then(|s| s.blend_mode), "{n}");
        } else {
            assert_eq!(blend, rb.blend_mode, "{n}: blend");
        }
        assert_eq!(ra.opacity, rb.opacity, "{n}: opacity");
        assert_eq!(ra.fill_opacity(), rb.fill_opacity(), "{n}: fill");
        assert_eq!(ra.is_visible(), rb.is_visible(), "{n}: visible");
        assert_eq!(ra.clipping, rb.clipping, "{n}: clipping");
        assert_eq!(ra.block(b"lfx2").map(|x| &x.data), rb.block(b"lfx2").map(|x| &x.data), "{n}: lfx2");
        if ra.section_type().is_folder() {
            continue;
        }
        // Pixels: compare every channel over the union of both rects.
        let u = |r: photosuite_psd::Rect, s: photosuite_psd::Rect| (r.left.min(s.left), r.top.min(s.top), r.right.max(s.right), r.bottom.max(s.bottom));
        let mut ids: Vec<i16> = (-1..cc).collect();
        if ra.layer_mask().is_some() && (ra.channel(-2).is_some() || ra.channel(-3).is_some()) {
            ids.push(-2);
        }
        for id in ids {
            // The exporter writes the real mask (-3) as the user mask (-2).
            let src_id = if id == -2 && ra.channel(-3).is_some() && ra.layer_mask().is_some_and(|m| m.real.is_some()) { -3 } else { id };
            let (x0, y0, x1, y1) = u(ra.channel_rect(src_id), rb.channel_rect(id));
            for y in y0..y1 {
                for x in x0..x1 {
                    let pa = sample(a, ia, src_id, x, y, &mut ca);
                    let pb = sample(b, ib, id, x, y, &mut cb);
                    assert_eq!(pa, pb, "{n}: channel {id} at ({x},{y})");
                }
            }
        }
    }
}

fn via_doc(f: &PsdFile) -> PsdFile {
    let bytes = f.to_bytes().unwrap();
    let imp = import("t.psd", &bytes).unwrap();
    let out = export(&imp.document, "t.psd", &ExportOptions::default()).unwrap();
    PsdFile::from_bytes(&out.bytes).unwrap()
}

macro_rules! tg {
    ($name:ident, $v:expr, $mode:expr, $depth:expr, $c:expr) => {
        #[test]
        fn $name() {
            let f = testgen::layered($v, $mode, $depth, $c);
            let g = via_doc(&f);
            assert_structurally_equal(&f, &g);
        }
    };
}

tg!(testgen_rgb8_rle, Version::Psd, ColorMode::Rgb, 8, Compression::Rle);
tg!(testgen_rgb8_raw, Version::Psd, ColorMode::Rgb, 8, Compression::Raw);
tg!(testgen_rgb16_zip, Version::Psd, ColorMode::Rgb, 16, Compression::Zip);
tg!(testgen_rgb32_zipp, Version::Psd, ColorMode::Rgb, 32, Compression::ZipPrediction);
tg!(testgen_gray8_zipp, Version::Psd, ColorMode::Grayscale, 8, Compression::ZipPrediction);
tg!(testgen_gray16_rle, Version::Psd, ColorMode::Grayscale, 16, Compression::Rle);
tg!(testgen_gray32_raw, Version::Psd, ColorMode::Grayscale, 32, Compression::Raw);
tg!(testgen_cmyk8_rle, Version::Psd, ColorMode::Cmyk, 8, Compression::Rle);
tg!(testgen_cmyk16_zip, Version::Psd, ColorMode::Cmyk, 16, Compression::Zip);
tg!(testgen_lab8_rle, Version::Psd, ColorMode::Lab, 8, Compression::Rle);
tg!(testgen_psb_rgb8_rle, Version::Psb, ColorMode::Rgb, 8, Compression::Rle);
tg!(testgen_psb_cmyk16_zipp, Version::Psb, ColorMode::Cmyk, 16, Compression::ZipPrediction);

#[test]
fn testgen_small_all_compressions() {
    for v in [Version::Psd, Version::Psb] {
        for c in Compression::ALL {
            let f = testgen::small(v, c);
            assert_structurally_equal(&f, &via_doc(&f));
        }
    }
}

#[test]
fn testgen_import_details() {
    let f = testgen::layered(Version::Psd, ColorMode::Rgb, 8, Compression::Rle);
    let (d, warnings) = psd_to_document(&f);
    // Unknown blend mode warned about.
    assert!(warnings.iter().any(|w| w.contains("unknown blend mode")), "{warnings:?}");
    // Groups: Outer (open) containing Inner (closed).
    let outer = d.layers.iter().find(|l| l.name == "Outer").expect("outer");
    let LayerContent::Group(g) = &outer.content else { panic!() };
    assert!(g.expanded);
    assert_eq!(outer.blend, photosuite_color::BlendMode::PassThrough);
    let inner = g.children.iter().find(|l| l.name == "Inner").unwrap();
    let LayerContent::Group(gi) = &inner.content else { panic!() };
    assert!(!gi.expanded);
    // Negative offsets preserved.
    let neg = d.layers.iter().find(|l| l.name.starts_with("Neg offset")).unwrap();
    let b = neg.surface().unwrap().content_bounds();
    assert!(b.x0 < 0 && b.y0 < 0);
    assert!(neg.mask.is_some());
    assert!(d.icc_profile.is_some());
    assert!((d.resolution_dpi - 72.0).abs() < 1e-3);
    assert!(d.metadata.xmp.is_some());
    // Named saved-path resource (2000) becomes a document path, raw bytes kept for export.
    assert!(d.paths.iter().any(|p| p.name == "Path 1" && p.psd_raw.as_deref() == Some(&vec![1u8, 2, 3])));
    // Global blocks preserved.
    assert!(d.metadata.psd_global_blocks.iter().any(|b| &b.1 == b"Patt"));
    // lfx2 kept on effects.
    assert!(d.walk().iter().any(|(_, _, l)| l.effects.psd_raw.is_some()));
}

#[test]
fn fallback_modes_import_flattened() {
    for mode in [ColorMode::Indexed, ColorMode::Bitmap, ColorMode::Duotone] {
        let depth = testgen::mode_depths(mode)[0];
        let f = testgen::merged_only(Version::Psd, mode, depth, Compression::Rle, 9, 5);
        let (d, w) = psd_to_document(&f);
        assert_eq!(d.layers.len(), 1, "{mode:?}");
        assert!(!w.is_empty(), "{mode:?} should warn");
        assert_eq!(d.depth, photosuite_color::SampleType::U8);
        // Re-export produces a valid PSD.
        let out = export(&d, "x.psd", &ExportOptions::default()).unwrap();
        assert!(PsdFile::from_bytes(&out.bytes).is_ok());
    }
}

#[test]
fn multichannel_imports_ink_channels() {
    for depth in testgen::mode_depths(ColorMode::Multichannel) {
        let f = testgen::merged_only(Version::Psd, ColorMode::Multichannel, *depth, Compression::Rle, 9, 5);
        let (d, _) = psd_to_document(&f);
        assert_eq!(d.mode, photosuite_color::ColorMode::Multichannel);
        assert!(d.layers.is_empty());
        assert_eq!(d.channels.len(), usize::from(f.header.channels));
        assert!(d.channels.iter().all(|c| c.spot.is_some()));
        // Stored dark = ink: the channel value is 1 − the stored sample.
        let merged = f.decode_merged().unwrap();
        let first = if *depth == 8 { f32::from(merged[0]) / 255.0 } else { f32::from(u16::from_be_bytes([merged[0], merged[1]])) / 65535.0 };
        assert!((d.channels[0].surface.pixel(0, 0)[0] - (1.0 - first)).abs() < 1e-3);
        // Re-export writes the same planes.
        let out = export(&d, "x.psd", &ExportOptions::default()).unwrap();
        let back = PsdFile::from_bytes(&out.bytes).unwrap();
        assert_eq!(back.header.color_mode, ColorMode::Multichannel);
        assert_eq!(back.decode_merged().unwrap(), merged, "{depth}");
    }
}

/// A Multichannel file keeps its other resources and document-level blocks (only resolution,
/// channel names and XMP were written back), but not the layer-only resources.
#[test]
fn multichannel_keeps_resources_and_global_blocks() {
    use photosuite_psd::{ImageResource, TaggedBlock};
    let mut f = testgen::merged_only(Version::Psd, ColorMode::Multichannel, 8, Compression::Rle, 9, 5);
    f.resources.push(ImageResource::new(1028, b"iptc".to_vec()));
    f.resources.push(ImageResource::new(1011, vec![1]));
    f.resources.push(ImageResource::new(1069, vec![0, 1, 0, 0]));
    f.global_blocks.push(TaggedBlock::new(*b"FMsk", vec![0, 0, 0, 0, 0, 50]));
    // DisplayInfo: version 1, then per channel RGB red at 50 % as Masked Areas (kind 1), as
    // psd-tools' 4x4_16bit_multichannel.psd has it (Photoshop's Channel Options: "Alpha 1",
    // Masked Areas, red, 50 %); it came back as black spot inks.
    let mut info = 1u32.to_be_bytes().to_vec();
    for _ in 0..f.header.channels {
        info.extend_from_slice(&[0, 0, 0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 50, 1]);
    }
    f.resources.push(ImageResource::new(1077, info.clone()));
    let (d, _) = psd_to_document(&f);
    // Shown as before: gray, the channels printed as black ink.
    let shown = photosuite_compose::flatten(&d).px[0];
    assert!(shown[0] < 1.0 && (shown[0] - shown[1]).abs() < 1e-6, "{shown:?}");
    let back = PsdFile::from_bytes(&export(&d, "x.psd", &ExportOptions::default()).unwrap().bytes).unwrap();
    let res = |id: u16| back.resources.iter().find(|r| r.id == id).map(|r| r.data.clone());
    assert_eq!(res(1028), Some(b"iptc".to_vec()));
    assert_eq!(res(1011), Some(vec![1]));
    assert_eq!(res(1069), None);
    assert_eq!(res(1077), Some(info));
    assert_eq!(back.resources.iter().filter(|r| r.id == 1005).count(), 1);
    let fmsk = back.global_blocks.iter().find(|b| &b.key == b"FMsk").map(|b| b.data.clone());
    assert_eq!(fmsk, Some(vec![0, 0, 0, 0, 0, 50]));
}

#[test]
fn indexed_colors_match_palette() {
    let f = testgen::merged_only(Version::Psd, ColorMode::Indexed, 8, Compression::Raw, 4, 2);
    let (d, _) = psd_to_document(&f);
    let s = d.layers[0].surface().unwrap();
    let rgba = f.composite_rgba8().unwrap();
    for y in 0..2 {
        for x in 0..4 {
            let p = s.pixel(x, y);
            let i = ((y * 4 + x) * 4) as usize;
            for (c, v) in p.iter().take(3).enumerate() {
                assert_eq!((v * 255.0).round() as u8, rgba.data[i + c]);
            }
        }
    }
}

#[test]
fn flattened_psd_becomes_background() {
    let f = testgen::merged_only(Version::Psd, ColorMode::Rgb, 16, Compression::ZipPrediction, 7, 3);
    let (d, _) = psd_to_document(&f);
    assert_eq!(d.layers.len(), 1);
    assert_eq!(d.layers[0].name, "Background");
    assert_eq!(d.depth, photosuite_color::SampleType::U16);
    let s = d.layers[0].surface().unwrap();
    let merged = f.decode_merged().unwrap();
    let v = u16::from_be_bytes([merged[0], merged[1]]);
    assert_eq!((s.pixel(0, 0)[0] * 65535.0).round() as u16, v);
}

#[test]
fn builder_psd_imports() {
    use photosuite_psd::{GroupSpec, LayerSpec, MaskSpec, PixelData, PsdBuilder, Rect};
    let mut b = PsdBuilder::new(4, 4);
    let mut s = LayerSpec::new("a", 0, 0, 2, 2, PixelData::Rgba8(vec![255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 9, 9, 9, 9]));
    s.mask = Some(MaskSpec { rect: Rect::from_xywh(0, 0, 1, 1), data: vec![100], default_color: 255, disabled: true });
    s.fill_opacity = Some(77);
    b.push_layer(s);
    b.begin_group(GroupSpec { open: false, ..GroupSpec::new("G") });
    b.push_layer(LayerSpec::new("b", 1, 1, 1, 1, PixelData::Rgba8(vec![1, 2, 3, 4])));
    b.end_group().unwrap();
    let bytes = b.to_bytes().unwrap();
    let d = import("b.psd", &bytes).unwrap().document;
    assert_eq!(d.layers.len(), 2);
    let a = &d.layers[0];
    assert_eq!(a.fill_opacity, 77.0 / 255.0);
    let m = a.mask.as_ref().unwrap();
    assert!(!m.enabled);
    assert_eq!(m.surface.pixel(0, 0), vec![100.0 / 255.0]);
    assert_eq!(m.surface.pixel(3, 3), vec![1.0]);
    let s = a.surface().unwrap();
    assert_eq!(s.pixel(1, 0), vec![0.0, 1.0, 0.0, 128.0 / 255.0]);
    assert!(matches!(&d.layers[1].content, LayerContent::Group(g) if !g.expanded && g.children.len() == 1));
}

#[test]
fn group_nesting_past_the_cap_is_rejected() {
    use photosuite_psd::{GroupSpec, LayerSpec, PixelData, PsdBuilder};
    let mut b = PsdBuilder::new(4, 4);
    for _ in 0..101 {
        b.begin_group(GroupSpec::new("g"));
    }
    b.push_layer(LayerSpec::new("deep", 0, 0, 1, 1, PixelData::Rgba8(vec![1, 2, 3, 4])));
    for _ in 0..101 {
        b.end_group().unwrap();
    }
    let bytes = b.to_bytes().unwrap();
    let err = import("deep.psd", &bytes).unwrap_err();
    assert!(err.to_string().contains("nested deeper than 100"), "{err}");
    // The never-fail converter still returns a document — capped, with a warning
    // about the part that was not imported (it used to recurse to the file's depth).
    let (d, warnings) = psd_to_document(&photosuite_psd::PsdFile::from_bytes(&bytes).unwrap());
    assert_eq!(d.max_group_depth(), 100);
    assert!(warnings.iter().any(|w| w.contains("deeper than 100")), "{warnings:?}");
}

#[test]
fn group_nesting_at_the_cap_imports() {
    use photosuite_psd::{GroupSpec, LayerSpec, PixelData, PsdBuilder};
    let mut b = PsdBuilder::new(4, 4);
    for _ in 0..100 {
        b.begin_group(GroupSpec::new("g"));
    }
    b.push_layer(LayerSpec::new("deep", 0, 0, 1, 1, PixelData::Rgba8(vec![1, 2, 3, 4])));
    for _ in 0..100 {
        b.end_group().unwrap();
    }
    let r = import("hundred.psd", &b.to_bytes().unwrap()).unwrap();
    assert_eq!(r.document.max_group_depth(), 100);
    assert!(!r.warnings.iter().any(|w| w.contains("deeper")), "{:?}", r.warnings);
}

#[test]
fn layered_import_skips_the_unused_merged_composite() {
    use photosuite_psd::{LayerSpec, PixelData, PsdBuilder};
    // A layered file whose merged composite carries nothing behind the colour channels never
    // decodes it - that composite is roughly half a Photoshop save's bytes. Here its data is
    // garbage: the layered import does not touch it, while a file with an extra channel does
    // decode (and warn), which is what makes the skip observable.
    let mut b = PsdBuilder::new(4, 4);
    b.push_layer(LayerSpec::new("a", 0, 0, 2, 2, PixelData::Rgba8(vec![7u8; 16])));
    b.composite(PixelData::Rgba8(vec![128; 64]));
    let mut f = photosuite_psd::PsdFile::from_bytes(&b.to_bytes().unwrap()).unwrap();
    f.image_data.data.clear();
    f.layer_info.as_mut().unwrap().merged_alpha = true;
    let (d, warnings) = psd_to_document(&f);
    assert_eq!(d.layers.len(), 1);
    assert!(!warnings.iter().any(|w| w.contains("merged image")), "an unused composite is not decoded: {warnings:?}");
    // A real extra channel behind the colour ones is part of the composite: it is decoded,
    // and the garbage above surfaces as a warning.
    f.layer_info.as_mut().unwrap().merged_alpha = false;
    let (_, warnings) = psd_to_document(&f);
    assert!(warnings.iter().any(|w| w.contains("merged image")), "{warnings:?}");
}

#[test]
fn locks_and_labels_from_psd() {
    use photosuite_psd::TaggedBlock;
    let mut f = testgen::small(Version::Psd, Compression::Raw);
    f.layers_mut()[0].blocks.push(TaggedBlock::protection(0b101));
    f.layers_mut()[0].blocks.push(TaggedBlock::sheet_color(3));
    let (d, _) = psd_to_document(&f);
    let l = &d.layers[0];
    assert!(l.locks.transparency && l.locks.position && !l.locks.pixels);
    assert_eq!(l.label, photosuite_doc::LabelColor::Yellow);
}

#[test]
fn artboard_lock_round_trips_byte_for_byte() {
    // Reference files write a Background layer's locks as lspf = 0x0D (transparency + position +
    // the bit-3 lock). PhotoSuite read the artboard lock at bit 4, so 0x0D came back as 0x05
    // and the lock bytes drifted on every re-save.
    use photosuite_psd::TaggedBlock;
    let mut f = testgen::small(Version::Psd, Compression::Raw);
    f.layers_mut()[0].blocks.push(TaggedBlock::protection(0x0D));
    let g = via_doc(&f);
    let lspf = g.layers().iter().find_map(|l| l.block(b"lspf")).expect("exported layer should carry an lspf block");
    assert_eq!(lspf.data, 0x0Du32.to_be_bytes());
}

#[test]
fn adjustment_and_fill_layers_from_psd() {
    use photosuite_psd::{LayerRecord, TaggedBlock};
    let mut f = testgen::small(Version::Psd, Compression::Raw);
    let mk = |name: &str, key: &[u8; 4], data: Vec<u8>| LayerRecord {
        name: name.as_bytes().to_vec(),
        blocks: vec![TaggedBlock::unicode_name(name), TaggedBlock::new(*key, data)],
        ..Default::default()
    };
    let soco = {
        use photosuite_psd::descriptor::*;
        let c = Descriptor::new("RGBC").with("Rd  ", Value::Double(255.0)).with("Grn ", Value::Double(0.0)).with("Bl  ", Value::Double(0.0));
        VersionedDescriptor::new(Descriptor::new("null").with("Clr ", Value::Descriptor(c))).to_bytes()
    };
    f.layers_mut().push(mk("inv", b"nvrt", vec![]));
    f.layers_mut().push(mk("post", b"post", vec![0, 4, 0, 0]));
    f.layers_mut().push(mk("sel", b"selc", vec![1, 2, 3]));
    f.layers_mut().push(mk("red", b"SoCo", soco));
    let (d, _) = psd_to_document(&f);
    let n = d.layers.len();
    use photosuite_doc::{Adjustment, Fill};
    assert!(matches!(d.layers[n - 4].content, LayerContent::Adjustment(Adjustment::Invert)));
    assert!(matches!(d.layers[n - 3].content, LayerContent::Adjustment(Adjustment::Posterize { levels: 4 })));
    assert!(
        matches!(&d.layers[n - 2].content, LayerContent::Adjustment(Adjustment::Unsupported { psd_key, raw }) if psd_key == "selc" && raw == &vec![1, 2, 3])
    );
    assert!(matches!(&d.layers[n - 1].content, LayerContent::Fill(Fill::Solid(c)) if c.c[0] == 1.0));
}

#[test]
fn unmodelled_blocks_are_preserved() {
    use photosuite_psd::TaggedBlock;
    let mut f = testgen::small(Version::Psd, Compression::Raw);
    f.layers_mut()[0].blocks.push(TaggedBlock::new(*b"vmsk", vec![0; 8]));
    let (d, _) = psd_to_document(&f);
    assert!(d.layers[0].psd_blocks.iter().any(|b| &b.0 == b"vmsk"));
    assert!(!d.layers[0].psd_blocks.iter().any(|b| &b.0 == b"luni" || &b.0 == b"lyid"));
    assert_eq!(d.layers[0].psd_id, f.layers()[0].layer_id());
}

#[test]
fn fill_layer_keeps_photoshop_pixels() {
    use photosuite_psd::{ChannelData, LayerRecord, TaggedBlock};
    let mut f = testgen::small(Version::Psd, Compression::Raw);
    let soco = {
        use photosuite_psd::descriptor::*;
        let c = Descriptor::new("RGBC").with("Rd  ", Value::Double(255.0)).with("Grn ", Value::Double(0.0)).with("Bl  ", Value::Double(0.0));
        VersionedDescriptor::new(Descriptor::new("null").with("Clr ", Value::Descriptor(c))).to_bytes()
    };
    // Photoshop-rendered pixels deliberately differ from the parametric fill.
    let plane = |v: u8| ChannelData { id: 0, compression: Some(Compression::Raw), data: vec![v; 4] };
    let mut chans = vec![plane(255), plane(10), plane(20), plane(30)];
    for (i, c) in chans.iter_mut().enumerate() {
        c.id = i as i16 - 1;
    }
    f.layers_mut().push(LayerRecord {
        rect: photosuite_psd::Rect::from_xywh(0, 0, 2, 2),
        name: b"fill".to_vec(),
        channels: chans,
        blocks: vec![TaggedBlock::unicode_name("fill"), TaggedBlock::new(*b"SoCo", soco)],
        ..Default::default()
    });
    let (d, _) = psd_to_document(&f);
    let l = d.layers.last().unwrap();
    assert!(matches!(l.content, LayerContent::Fill(_)));
    let fc = l.fill_cache.as_ref().expect("cache");
    assert_eq!(fc.surface.pixel(1, 1), vec![10.0 / 255.0, 20.0 / 255.0, 30.0 / 255.0, 1.0]);
    // The compositor uses the cache while the fill is unchanged.
    let flat = photosuite_compose::flatten(&d);
    let p = flat.get(1, 1);
    assert!((p[0] - 10.0 / 255.0).abs() < 1e-3, "{p:?}");
    // Editing the fill invalidates the cache.
    let mut d2 = d.clone();
    let n = d2.layers.len();
    d2.layers[n - 1].content = LayerContent::Fill(photosuite_doc::Fill::Solid(photosuite_color::Color::rgb(0.0, 0.0, 1.0)));
    let p = photosuite_compose::flatten(&d2).get(1, 1);
    assert!(p[2] > 0.99 && p[0] < 0.01, "{p:?}");
}

#[test]
fn text_and_smart_detected() {
    use photosuite_psd::{LayerRecord, TaggedBlock};
    let mut f = testgen::small(Version::Psd, Compression::Raw);
    for (name, key) in [("t", b"TySh"), ("s", b"PlLd"), ("sh", b"vscg")] {
        f.layers_mut().push(LayerRecord {
            name: name.as_bytes().to_vec(),
            blocks: vec![TaggedBlock::unicode_name(name), TaggedBlock::new(*key, vec![0; 8])],
            ..Default::default()
        });
    }
    let (d, _) = psd_to_document(&f);
    let n = d.layers.len();
    assert_eq!(d.layers[n - 3].content.kind_name(), "Type");
    assert_eq!(d.layers[n - 2].content.kind_name(), "Smart Object");
    assert_eq!(d.layers[n - 1].content.kind_name(), "Shape");
}

#[test]
fn guides_and_alpha_names_roundtrip() {
    let mut d = photosuite_doc::Document::new("g", photosuite_geom::Size::new(8, 8), photosuite_color::ColorMode::Rgb, photosuite_color::SampleType::U8);
    d.guides.horizontal = vec![1.0, 2.5];
    d.guides.vertical = vec![7.03125];
    for name in ["Spot \u{e9}", "Alpha 2"] {
        d.channels.push(photosuite_doc::AlphaChannel::new(
            name,
            photosuite_raster::Surface::new(photosuite_color::PixelFormat::new(
                photosuite_color::ColorMode::Grayscale,
                photosuite_color::SampleType::U8,
                false,
            )),
        ));
    }
    let f = document_to_psd(&d);
    // RGB + merged transparency (the empty document is transparent) + 2 alpha channels.
    assert_eq!(f.header.channels, 6);
    let (d2, _) = psd_to_document(&f);
    assert_eq!(d2.guides, d.guides);
    let names: Vec<_> = d2.channels.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Spot \u{e9}", "Alpha 2"]);
}

#[test]
fn resolution_in_cm_converted() {
    let mut f = testgen::small(Version::Psd, Compression::Raw);
    let mut ri = photosuite_psd::ResolutionInfo::from_dpi(100.0);
    ri.h_res_unit = 2;
    f.resources[0] = photosuite_psd::ImageResource::new(1005, ri.to_bytes());
    let (d, _) = psd_to_document(&f);
    assert!((d.resolution_dpi - 254.0).abs() < 1e-3);
}

#[test]
fn stale_index_resources_dropped() {
    let mut f = testgen::small(Version::Psd, Compression::Raw);
    for id in [1024u16, 1026, 1036, 1057, 1069, 1072] {
        f.resources.push(photosuite_psd::ImageResource::new(id, vec![0, 0]));
    }
    let (d, _) = psd_to_document(&f);
    assert!(d.metadata.psd_resources.iter().all(|r| ![1024u16, 1026, 1036, 1057, 1069, 1072].contains(&r.0)));
}

#[test]
fn merged_composite_unmattes_photoshop_white() {
    // A 1x1 RGB+alpha flattened file with color matted against white.
    let mut f = testgen::merged_only(Version::Psd, ColorMode::Rgb, 8, Compression::Raw, 1, 1);
    f.header.channels = 4;
    // straight red at alpha 0.5, matted: r = 1, g = b = 0.5
    f.image_data = photosuite_psd::ImageData { compression: Compression::Raw, data: vec![255, 128, 128, 128] };
    let m = merged_composite(&f).unwrap();
    assert!((m[0][0] - 1.0).abs() < 0.01 && m[0][1] < 0.01 && m[0][2] < 0.01, "{:?}", m[0]);
    let (d, _) = psd_to_document(&f);
    let p = d.layers[0].surface().unwrap().pixel(0, 0);
    assert!(p[1] < 0.01 && (p[3] - 128.0 / 255.0).abs() < 1e-6, "{p:?}");

    // HDR and negative straight samples must survive the actual F32 PSD import boundary.
    let mut hdr = testgen::merged_only(Version::Psd, ColorMode::Rgb, 32, Compression::Raw, 1, 1);
    hdr.header.channels = 4;
    hdr.image_data =
        photosuite_psd::ImageData { compression: Compression::Raw, data: [1.5_f32, 0.25, 0.5, 0.5].into_iter().flat_map(f32::to_be_bytes).collect() };
    let imported = import("hdr.psd", &hdr.to_bytes().unwrap()).unwrap().document;
    assert_eq!(imported.depth, photosuite_color::SampleType::F32);
    assert_eq!(imported.layers[0].surface().unwrap().pixel(0, 0), vec![2.0, -0.5, 0.0, 0.5]);
}

#[test]
fn vector_rendered_mask_not_doubled_on_shapes() {
    use photosuite_psd::{LayerRecord, TaggedBlock};
    let mut f = testgen::small(Version::Psd, Compression::Raw);
    let mut mask = photosuite_psd::LayerMask::new(photosuite_psd::Rect::default(), 0, 8);
    mask.flags = 8;
    f.layers_mut().push(LayerRecord {
        name: b"shape".to_vec(),
        mask: photosuite_psd::MaskData::Mask(mask),
        channels: vec![photosuite_psd::ChannelData { id: -2, compression: Some(Compression::Raw), data: vec![] }],
        blocks: vec![TaggedBlock::unicode_name("shape"), TaggedBlock::new(*b"SoCo", vec![]), TaggedBlock::new(*b"vmsk", vec![0; 8])],
        ..Default::default()
    });
    let (d, _) = psd_to_document(&f);
    let l = d.layers.last().unwrap();
    assert_eq!(l.content.kind_name(), "Shape");
    assert!(l.mask.is_none());

    let record = f.layers_mut().last_mut().unwrap();
    let photosuite_psd::MaskData::Mask(mask) = &mut record.mask else { panic!("fixture has a mask") };
    mask.real = Some(photosuite_psd::RealMask { flags: 0, background: 255, rect: photosuite_psd::Rect { top: 0, left: 0, bottom: 1, right: 1 } });
    record.channels.push(photosuite_psd::ChannelData { id: -3, compression: Some(Compression::Raw), data: vec![64] });
    let (imported, _) = psd_to_document(&f);
    assert_eq!(imported.layers.last().unwrap().content.kind_name(), "Shape");
    let mask = imported.layers.last().unwrap().mask.as_ref().expect("selected real mask must survive synthetic cleanup");
    assert!((mask.surface.sample_channel(0, 0, 0) - 64.0 / 255.0).abs() < 1e-6);
    let exported = export(&imported, "real-mask.psd", &Default::default()).unwrap();
    let reopened = import("real-mask.psd", &exported.bytes).unwrap().document;
    assert_eq!(reopened.layers.last().unwrap().mask.as_ref().unwrap().surface.pixel(0, 0), mask.surface.pixel(0, 0));
}

#[test]
fn every_blend_mode_maps_both_ways() {
    for m in std::iter::once(photosuite_color::BlendMode::PassThrough).chain(photosuite_color::BlendMode::LAYER_MODES) {
        let k = photosuite_psd::BlendMode::from_key(m.psd_key());
        assert!(!matches!(k, photosuite_psd::BlendMode::Unknown(_)), "{m:?}");
        assert_eq!(photosuite_color::BlendMode::from_psd_key(k.key()), Some(m));
    }
}

#[test]
fn export_writes_a_thumbnail() {
    use photosuite_color::ColorMode as Mode;
    use photosuite_color::SampleType;
    for (mode, sample) in [(Mode::Rgb, SampleType::U8), (Mode::Cmyk, SampleType::U16), (Mode::Grayscale, SampleType::F32)] {
        let mut d = photosuite_doc::Document::new("t", photosuite_geom::Size::new(320, 160), mode, sample);
        let mut l = photosuite_doc::Layer::raster("red", d.pixel_format());
        // Left half dark, right half transparent (white in the thumbnail).
        l.surface_mut().unwrap().fill_rect(photosuite_geom::Rect::new(0, 0, 160, 160), &photosuite_raster::from_rgba(&d.pixel_format(), [0.1, 0.1, 0.1, 1.0]));
        d.layers.push(l);
        let (f, _) = document_to_psd_with(&d, &PsdExportOptions::default());
        let r = f.resource(1036).expect("thumbnail");
        let be = |at: usize| u32::from_be_bytes(r.data[at..at + 4].try_into().unwrap());
        assert_eq!((be(0), be(4), be(8)), (1, 160, 80), "{mode:?}");
        assert_eq!(be(20) as usize, r.data.len() - 28);
        let img = photosuite_codecs::decode(&r.data[28..]).unwrap();
        assert_eq!((img.width(), img.height()), (160, 80));
        let px = |x: usize, y: usize| img.data()[(y * 160 + x) * 3];
        assert!(px(20, 40) < 60 && px(140, 40) > 200, "{mode:?}: {} {}", px(20, 40), px(140, 40));
    }
}

#[test]
fn background_layer_has_no_transparency_channel() {
    use photosuite_color::{Color, ColorMode as Mode, SampleType};
    for sample in [SampleType::U8, SampleType::U16, SampleType::F32] {
        let mut d = photosuite_doc::Document::with_background("t", photosuite_geom::Size::new(8, 6), Mode::Rgb, sample, Color::WHITE);
        d.layers.push(photosuite_doc::Layer::raster("top", d.pixel_format()));
        let (f, _) = document_to_psd_with(&d, &PsdExportOptions::default());
        let bg = &f.layers()[0];
        assert!(bg.channel(-1).is_none(), "{sample:?}");
        assert_eq!(bg.flags.0, 9, "{sample:?}: transparency protected + bit 3, as Photoshop writes it");
        assert!(f.layers()[1].channel(-1).is_some());
        let (back, _) = psd_to_document(&f);
        assert_eq!(back.layers[0].surface().unwrap().pixel(3, 3)[3], 1.0);
        // A hole in it: no longer a Background, the transparency channel is back.
        d.layers[0].surface_mut().unwrap().fill_rect(photosuite_geom::Rect::new(0, 0, 1, 1), &[0.0; 4]);
        let (f, _) = document_to_psd_with(&d, &PsdExportOptions::default());
        assert!(f.layers()[0].channel(-1).is_some(), "{sample:?}");
    }
}

/// Photoshop lists the merged transparency first in the alpha channel resources ("Transparency",
/// id 0, red 100 %). Read as the first alpha channel, it renamed "Alpha 1" to "Transparency" and
/// gave it the transparency's overlay (psd-tools stroke-without-vector-mask.psd).
#[test]
fn merged_transparency_is_listed_first_and_skipped_on_import() {
    use photosuite_color::{ColorMode as Mode, SampleType};
    let mut d = photosuite_doc::Document::new("t", photosuite_geom::Size::new(8, 6), Mode::Rgb, SampleType::U8);
    let fmt = d.pixel_format();
    let mut l = photosuite_doc::Layer::raster("half", fmt);
    l.surface_mut().unwrap().fill_rect(photosuite_geom::Rect::new(0, 0, 4, 6), &photosuite_raster::from_rgba(&fmt, [1.0, 0.0, 0.0, 1.0]));
    d.layers.push(l);
    let mut a = photosuite_doc::AlphaChannel::new("Alpha 1", photosuite_raster::Surface::new(photosuite_color::PixelFormat::new(Mode::Grayscale, SampleType::U8, false)));
    a.opacity = 0.3;
    d.channels.push(a);
    let (f, _) = document_to_psd_with(&d, &PsdExportOptions::default());
    let names = photosuite_psd::resources::unicode_names(&f.resource(1045).unwrap().data);
    assert_eq!(names, ["Transparency", "Alpha 1"]);
    assert_eq!(f.resource(1053).unwrap().data.len(), 8, "one id per name");
    let (back, _) = psd_to_document(&f);
    assert_eq!(back.channels.len(), 1);
    assert_eq!((back.channels[0].name.as_str(), back.channels[0].opacity), ("Alpha 1", 0.3));
    // Without layer info, the names decide whether the merged image has a transparency channel:
    // here they cover every extra channel and don't start with "Transparency".
    let mut flat = f.clone();
    flat.layer_info = None;
    flat.header.channels = 4;
    for r in flat.resources.iter_mut().filter(|r| r.id == 1045) {
        r.data = r.data[4 + 2 * 13..].to_vec();
    }
    assert_eq!(photosuite_psd::resources::unicode_names(&flat.resources.iter().find(|r| r.id == 1045).unwrap().data), ["Alpha 1"]);
    assert!(!flat.merged_has_alpha());
}

/// A Duotone PSD keeps its layers and goes back as Duotone with its (undocumented) ink data;
/// layers used to be dropped and the file written as Grayscale.
#[test]
fn duotone_keeps_layers_mode_and_inks() {
    use photosuite_color::{ColorMode as Mode, SampleType};
    let mut d = photosuite_doc::Document::new("t", photosuite_geom::Size::new(8, 6), Mode::Duotone, SampleType::U8);
    let inks = vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
    d.duotone = Some(photosuite_doc::Duotone { inks: vec![photosuite_doc::DuotoneInk::new("Black", [0.0; 3])], psd_raw: Some(inks.clone()) });
    let fmt = d.pixel_format();
    for name in ["one", "two"] {
        let mut l = photosuite_doc::Layer::raster(name, fmt);
        l.surface_mut().unwrap().fill_rect(photosuite_geom::Rect::new(0, 0, 4, 3), &photosuite_raster::from_rgba(&fmt, [0.5, 0.5, 0.5, 1.0]));
        d.layers.push(l);
    }
    let (f, warnings) = document_to_psd_with(&d, &PsdExportOptions::default());
    assert_eq!(f.header.color_mode, ColorMode::Duotone);
    assert_eq!(f.color_mode_data, inks);
    assert!(warnings.iter().all(|w| !w.contains("written as")), "{warnings:?}");
    let (back, _) = psd_to_document(&f);
    assert_eq!(back.mode, Mode::Duotone);
    assert_eq!(back.layers.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["one", "two"]);
    assert_eq!(back.duotone.and_then(|d| d.psd_raw), Some(inks));
    // Made here without Photoshop's ink data: its gray plate, as Grayscale.
    d.duotone = Some(photosuite_doc::Duotone { inks: vec![photosuite_doc::DuotoneInk::new("Black", [0.0; 3])], psd_raw: None });
    assert_eq!(document_to_psd(&d).header.color_mode, ColorMode::Grayscale);
}

/// Indexed Color and Bitmap files go back in their own mode while they are one plain image:
/// palette, colour count and transparent index for Indexed, 1-bit pixels for Bitmap. They used
/// to be written as RGB / 8-bit Grayscale.
#[test]
fn indexed_and_bitmap_keep_their_mode() {
    for (mode, depth, w) in [(ColorMode::Indexed, 8, 7u32), (ColorMode::Bitmap, 1, 13)] {
        let mut f = testgen::merged_only(Version::Psd, mode, depth, Compression::Rle, w, 3);
        // Indexed: 200 colours, the transparent one an entry the image uses.
        let t = f.decode_merged().unwrap()[1];
        if mode == ColorMode::Indexed {
            assert!(t < 200);
            f.resources.push(photosuite_psd::ImageResource::new(1046, 200u16.to_be_bytes().to_vec()));
            f.resources.push(photosuite_psd::ImageResource::new(1047, u16::from(t).to_be_bytes().to_vec()));
        }
        let (d, _) = psd_to_document(&f);
        let (back, warnings) = document_to_psd_with(&d, &PsdExportOptions::default());
        assert_eq!((back.header.color_mode, back.header.depth, back.header.channels), (mode, depth, 1), "{mode:?}");
        assert!(warnings.iter().all(|w| !w.contains("written as")), "{warnings:?}");
        assert!(back.layer_info.is_none());
        let (a, b) = (f.decode_merged().unwrap(), back.decode_merged().unwrap());
        let row = f.header.row_bytes();
        for y in 0..3 {
            let (ra, rb) = (&a[y * row..(y + 1) * row], &b[y * row..(y + 1) * row]);
            if mode == ColorMode::Bitmap {
                // Pixels only: the padding bits past the width are free.
                let pad = (8 - w as usize % 8) % 8;
                assert_eq!(ra[..row - 1], rb[..row - 1]);
                assert_eq!(ra[row - 1] >> pad, rb[row - 1] >> pad);
            } else {
                assert_eq!(ra, rb, "{mode:?} row {y}");
            }
        }
        if mode == ColorMode::Indexed {
            assert_eq!(back.color_mode_data[..200], f.color_mode_data[..200]);
            let table = d.color_table.as_ref().unwrap();
            assert_eq!((table.colors.len(), table.transparent), (200, Some(t)));
            // Pixels of the transparent entry come in transparent (and go back as that entry,
            // checked by the plane comparison above).
            let s = d.layers[0].surface().unwrap();
            let plane = f.decode_merged().unwrap();
            let mut seen = false;
            for (i, idx) in plane.iter().enumerate() {
                let a = s.pixel((i % w as usize) as i32, (i / w as usize) as i32)[3];
                assert_eq!(a == 0.0, *idx == t, "pixel {i} index {idx} alpha {a}");
                seen |= *idx == t;
            }
            assert!(seen, "the sample image uses the transparent entry");
            assert_eq!(back.resource(1046).unwrap().data, 200u16.to_be_bytes());
            assert_eq!(back.resource(1047).unwrap().data, u16::from(t).to_be_bytes());
        }
        // With a second layer it can't stay flat: RGB / Grayscale, with the warning.
        let mut layered = d.clone();
        layered.layers.push(photosuite_doc::Layer::raster("more", d.pixel_format()));
        let (f2, w2) = document_to_psd_with(&layered, &PsdExportOptions::default());
        assert_ne!(f2.header.color_mode, mode);
        assert!(w2.iter().any(|w| w.contains("written as")));
    }
}

/// Knockout (`knko`) reads into the layer and is written back from it (groups too).
#[test]
fn knockout_round_trips() {
    use photosuite_doc::{Document, Knockout, Layer, Size};
    let mut d = Document::new("k", Size::new(4, 4), photosuite_color::ColorMode::Rgb, photosuite_color::SampleType::U8);
    let mut a = Layer::raster("a", d.pixel_format());
    a.knockout = Knockout::Shallow;
    let mut g = Layer::group("g", vec![Layer::raster("in", d.pixel_format())]);
    g.knockout = Knockout::Deep;
    d.layers = vec![a, g];
    let back = import("x.psd", &export(&d, "x.psd", &ExportOptions::default()).unwrap().bytes).unwrap().document;
    assert_eq!(back.layers[0].knockout, Knockout::Shallow);
    assert_eq!(back.layers[1].knockout, Knockout::Deep);
    // Turned off, it is written as off.
    // Blend Clipped / Interior Effects as Group too.
    let mut flags = back.clone();
    flags.layers[0].blend_clipped = false;
    flags.layers[0].blend_interior = true;
    flags.layers[0].mask_hides_effects = true;
    flags.layers[0].vector_mask_hides_effects = true;
    let f = import("x.psd", &export(&flags, "x.psd", &ExportOptions::default()).unwrap().bytes).unwrap().document;
    assert!(!f.layers[0].blend_clipped && f.layers[0].blend_interior);
    assert!(f.layers[0].mask_hides_effects && f.layers[0].vector_mask_hides_effects);
    assert!(!f.layers[1].mask_hides_effects);
    assert!(f.layers[1].blend_clipped && !f.layers[1].blend_interior);
    let mut off = back.clone();
    off.layers[0].knockout = Knockout::None;
    let again = import("x.psd", &export(&off, "x.psd", &ExportOptions::default()).unwrap().bytes).unwrap().document;
    assert_eq!(again.layers[0].knockout, Knockout::None);
}

/// A group's end-divider record keeps the blocks Photoshop wrote on it (`shmd` layer time,
/// `fxrp`): through import, PSD export and the native format.
#[test]
fn group_divider_blocks_round_trip() {
    use std::sync::Arc;
    let mut d = photosuite_doc::Document::new("t", photosuite_geom::Size::new(8, 8), photosuite_color::ColorMode::Rgb, photosuite_color::SampleType::U8);
    let mut g = photosuite_doc::Layer::group("G", vec![photosuite_doc::Layer::raster("A", photosuite_color::PixelFormat::RGBA8)]);
    let fxrp = vec![0u8; 16];
    if let LayerContent::Group(grp) = &mut g.content {
        grp.divider_blocks = vec![(*b"fxrp", Arc::new(fxrp.clone()))];
    }
    d.layers.push(g);
    let bytes = export(&d, "t.psd", &Default::default()).unwrap().bytes;
    let f = PsdFile::from_bytes(&bytes).unwrap();
    let divider = f.layers().iter().find(|r| r.section_type() == SectionType::BoundingDivider).expect("divider record");
    assert_eq!(divider.blocks.iter().find(|b| &b.key == b"fxrp").map(|b| b.data.clone()), Some(fxrp.clone()));
    // Import reads them back; the native format keeps them.
    let back = import("t.psd", &bytes).unwrap().document;
    let LayerContent::Group(gb) = &back.layers[0].content else { panic!("group") };
    assert_eq!(gb.divider_blocks.iter().find(|b| &b.0 == b"fxrp").map(|b| b.1.to_vec()), Some(fxrp.clone()));
    let pc = photosuite_format::save_to_bytes(&back, &Default::default()).unwrap();
    let again = photosuite_format::load_from_bytes(&pc).unwrap();
    let LayerContent::Group(ga) = &again.layers[0].content else { panic!("group") };
    assert_eq!(ga.divider_blocks, gb.divider_blocks);
}

/// A group's end divider keeps its layer id (`lyid`) through a round trip; a copy of the group
/// (same divider blocks) and the layers get ids of their own, all unique.
#[test]
fn group_divider_ids_round_trip() {
    use std::sync::Arc;
    let mut d = photosuite_doc::Document::new("t", photosuite_geom::Size::new(8, 8), photosuite_color::ColorMode::Rgb, photosuite_color::SampleType::U8);
    let group = |psd_id: u32| {
        let mut g = photosuite_doc::Layer::group("G", vec![photosuite_doc::Layer::raster("A", photosuite_color::PixelFormat::RGBA8)]);
        g.psd_id = Some(psd_id);
        if let LayerContent::Group(grp) = &mut g.content {
            grp.divider_blocks = vec![(*b"lyid", Arc::new(9u32.to_be_bytes().to_vec()))];
        }
        g
    };
    d.layers.push(group(4));
    d.layers.push(group(9));
    let ids = |bytes: &[u8]| -> Vec<(bool, u32)> {
        let f = PsdFile::from_bytes(bytes).unwrap();
        f.layers().iter().map(|r| (r.section_type() == SectionType::BoundingDivider, r.layer_id().unwrap())).collect()
    };
    let bytes = export(&d, "t.psd", &Default::default()).unwrap().bytes;
    let first = ids(&bytes);
    let mut all: Vec<u32> = first.iter().map(|i| i.1).collect();
    all.sort_unstable();
    all.dedup();
    assert_eq!(all.len(), first.len(), "unique ids: {first:?}");
    // The second group's own id (9) wins over the dividers' kept 9.
    assert!(first.iter().any(|&(div, id)| !div && id == 9) && !first.iter().any(|&(div, id)| div && id == 9), "{first:?}");
    // Without the clash, the divider keeps its id.
    let mut one = photosuite_doc::Document::new("t", photosuite_geom::Size::new(8, 8), photosuite_color::ColorMode::Rgb, photosuite_color::SampleType::U8);
    one.layers.push(group(4));
    assert!(ids(&export(&one, "t.psd", &Default::default()).unwrap().bytes).contains(&(true, 9)));
    // What was written comes back the same.
    let back = import("t.psd", &bytes).unwrap().document;
    assert_eq!(ids(&export(&back, "t.psd", &Default::default()).unwrap().bytes), first);
}

/// The resolution keeps the file's units (pixels/cm, width and height shown in cm): verbatim
/// while unchanged, else the new resolution in those units.
#[test]
fn resolution_keeps_its_units() {
    use photosuite_psd::ResolutionInfo;
    let d = photosuite_doc::Document::new("t", photosuite_geom::Size::new(8, 8), photosuite_color::ColorMode::Rgb, photosuite_color::SampleType::U8);
    let mut f = PsdFile::from_bytes(&export(&d, "t.psd", &Default::default()).unwrap().bytes).unwrap();
    // 72 dpi as Photoshop writes it for a document set up in centimetres.
    let cm = ResolutionInfo { h_res_fixed: 0x001C_58AF, h_res_unit: 2, width_unit: 2, v_res_fixed: 0x001C_58AF, v_res_unit: 2, height_unit: 2 };
    let r = f.resources.iter_mut().find(|r| r.id == 1005).unwrap();
    r.data = cm.to_bytes();
    let mut doc = import("t.psd", &f.to_bytes().unwrap()).unwrap().document;
    assert!((doc.resolution_dpi - 72.0).abs() < 0.01, "{}", doc.resolution_dpi);
    let written = |doc: &photosuite_doc::Document| {
        let f = PsdFile::from_bytes(&export(doc, "t.psd", &Default::default()).unwrap().bytes).unwrap();
        assert_eq!(f.resources.iter().filter(|r| r.id == 1005).count(), 1);
        ResolutionInfo::from_bytes(&f.resource(1005).unwrap().data).unwrap()
    };
    assert_eq!(written(&doc), cm);
    doc.resolution_dpi = 254.0;
    let w = written(&doc);
    assert_eq!((w.h_res_unit, w.width_unit, w.v_res_unit, w.height_unit), (2, 2, 2, 2));
    assert!((w.h_res() - 100.0).abs() < 1e-3 && (w.v_res() - 100.0).abs() < 1e-3, "{w:?}");
}

/// A layer added to an imported document gets an id past the file's seed (1044) and every id in
/// use, never a free lower one a deleted layer may have had.
#[test]
fn new_layer_ids_follow_the_seed() {
    let mut d = photosuite_doc::Document::new("t", photosuite_geom::Size::new(8, 8), photosuite_color::ColorMode::Rgb, photosuite_color::SampleType::U8);
    let mut a = photosuite_doc::Layer::raster("A", photosuite_color::PixelFormat::RGBA8);
    a.psd_id = Some(7);
    d.layers.push(a);
    d.layers.push(photosuite_doc::Layer::raster("new", photosuite_color::PixelFormat::RGBA8));
    let ids = |d: &photosuite_doc::Document| -> Vec<u32> { PsdFile::from_bytes(&export(d, "t.psd", &Default::default()).unwrap().bytes).unwrap().layers().iter().map(|r| r.layer_id().unwrap()).collect() };
    assert_eq!(ids(&d), vec![7, 8]);
    d.metadata.psd_resources.push((1044, String::new(), std::sync::Arc::new(20u32.to_be_bytes().to_vec())));
    assert_eq!(ids(&d), vec![7, 21]);
}

#[test]
fn layer_bounds_without_channel_data_allocate_nothing() {
    // A record can declare a 300000x300000 rectangle in a few bytes. With no channel data to
    // back it, import used to interleave a ~360 GB buffer and abort on the allocation
    // (storytold/photocraft@dc256b8b).
    use photosuite_psd::LayerRecord;
    let mut f = testgen::small(Version::Psd, Compression::Raw);
    f.layers_mut().push(LayerRecord { rect: photosuite_psd::Rect::from_xywh(0, 0, 300_000, 300_000), name: b"huge".to_vec(), ..Default::default() });
    let (d, warnings) = psd_to_document(&f);
    let l = d.layers.last().unwrap();
    assert!(matches!(&l.content, LayerContent::Raster(s) if s.tile_count() == 0));
    assert!(warnings.iter().any(|w| w.contains("\"huge\"")), "{warnings:?}");
}

/// A group keeping the id u32::MAX: the ids given to its child and its end divider wrap to the
/// smallest free ones instead of overflowing, and none collides (storytold/photocraft@ccb33741).
#[test]
fn layer_ids_wrap_without_colliding() {
    let mut d = photosuite_doc::Document::new("t", photosuite_geom::Size::new(8, 8), photosuite_color::ColorMode::Rgb, photosuite_color::SampleType::U8);
    let mut g = photosuite_doc::Layer::group("G", vec![photosuite_doc::Layer::raster("A", photosuite_color::PixelFormat::RGBA8)]);
    g.psd_id = Some(u32::MAX);
    d.layers.push(g);
    let f = PsdFile::from_bytes(&export(&d, "t.psd", &Default::default()).unwrap().bytes).unwrap();
    let ids: Vec<u32> = f.layers().iter().filter_map(|r| r.layer_id()).collect();
    let unique: std::collections::HashSet<u32> = ids.iter().copied().collect();
    assert_eq!((ids.len(), unique.len()), (3, 3), "{ids:?}");
    assert!(unique.contains(&u32::MAX) && unique.contains(&1), "{ids:?}");
}

/// Blocks kept with the PSB-only `8B64` signature are written as `8BIM` in a PSD (Reference
/// refuses the file otherwise) and stay `8B64` in a PSB (storytold/photocraft@3f72158f).
#[test]
fn kept_psb_blocks_are_written_as_8bim_in_a_psd() {
    let mut d = photosuite_doc::Document::new("t", photosuite_geom::Size::new(4, 4), photosuite_color::ColorMode::Rgb, photosuite_color::SampleType::U8);
    d.metadata.psd_global_blocks = vec![(*b"8B64", *b"FMsk", std::sync::Arc::new(vec![0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0, 50])), (*b"8BIM", *b"Patt", std::sync::Arc::new(Vec::new()))];
    let sigs = |force_psb| {
        let bytes = export(&d, "t.psd", &ExportOptions { force_psb, ..Default::default() }).unwrap().bytes;
        PsdFile::from_bytes(&bytes).unwrap().global_blocks.iter().filter(|b| matches!(&b.key, b"FMsk" | b"Patt")).map(|b| (b.signature, b.key)).collect::<Vec<_>>()
    };
    assert_eq!(sigs(false), vec![(*b"8BIM", *b"FMsk"), (*b"8BIM", *b"Patt")]);
    assert_eq!(sigs(true), vec![(*b"8B64", *b"FMsk"), (*b"8BIM", *b"Patt")], "a PSB keeps 8B64");
}

/// A 16-bit file's layers go in a global `Lr16` block whose length is the data's own, padded to 4
/// after it (storytold/photocraft@481b939f).
#[test]
fn lr16_padding_follows_the_block() {
    let mut d = photosuite_doc::Document::new("t", photosuite_geom::Size::new(5, 3), photosuite_color::ColorMode::Rgb, photosuite_color::SampleType::U16);
    d.layers.push(photosuite_doc::Layer::raster("A", photosuite_color::PixelFormat::new(photosuite_color::ColorMode::Rgb, photosuite_color::SampleType::U16, true)));
    let f = PsdFile::from_bytes(&export(&d, "t.psd", &Default::default()).unwrap().bytes).unwrap();
    let photosuite_psd::file::LayerInfoPlacement::GlobalBlock { key, padding, .. } = &f.layer_info_placement else { panic!("Lr16 block") };
    assert_eq!(key, b"Lr16");
    let body = f.layer_info.as_ref().unwrap().unpadded_len(Version::Psd).unwrap();
    assert_eq!(padding.as_ref().map_or(body % 2, |p| p.len() as u64), (4 - body % 4) % 4, "padding after the block, to 4");
}

/// A type layer saved without pixels (ag-psd, GIMP and other writers leave them to Reference to
/// draw) opens rendered from its model, not blank (storytold/photocraft@d2ad238e).
#[test]
fn type_layers_without_pixels_open_rendered() {
    let mut d = photosuite_doc::Document::new("t", photosuite_geom::Size::new(120, 40), photosuite_color::ColorMode::Rgb, photosuite_color::SampleType::U8);
    let mut t = photosuite_doc::TextLayer { text: "Hello".into(), size_pt: 24.0, transform: photosuite_geom::Affine::translate(4.0, 30.0), ..Default::default() };
    t.runs = vec![photosuite_doc::text::TextRun { len: 5, style: photosuite_doc::text::CharStyle { font_family: "Inter".into(), size_pt: 24.0, ..Default::default() } }];
    t.sync_summary();
    d.layers.push(photosuite_doc::Layer::new("Hello", LayerContent::Text(t)));
    let bytes = export(&d, "t.psd", &Default::default()).unwrap().bytes;
    let back = import("t.psd", &bytes).unwrap().document;
    let LayerContent::Text(b) = &back.layers[0].content else { panic!("text layer") };
    assert!(b.cache.as_ref().is_some_and(|c| !c.content_bounds().is_empty()), "rendered on open");
}

/// The render on open is capped: an absurd type size or scale in a hostile file keeps the layer
/// empty instead of allocating gigabytes (storytold/photocraft@d2ad238e).
#[test]
fn hostile_type_sizes_without_pixels_open_without_a_huge_render() {
    for (size, scale) in [(6000.0f32, 1.0f64), (1e30, 1.0), (24.0, 1e12)] {
        let mut d = photosuite_doc::Document::new("t", photosuite_geom::Size::new(64, 64), photosuite_color::ColorMode::Rgb, photosuite_color::SampleType::U8);
        let mut t = photosuite_doc::TextLayer { text: "Hostile".into(), size_pt: size, transform: photosuite_geom::Affine { m: [scale, 0.0, 0.0, scale, 0.0, 30.0] }, ..Default::default() };
        t.runs = vec![photosuite_doc::text::TextRun { len: 7, style: photosuite_doc::text::CharStyle { font_family: "Inter".into(), size_pt: size, ..Default::default() } }];
        t.sync_summary();
        d.layers.push(photosuite_doc::Layer::new("Hostile", LayerContent::Text(t)));
        let Ok(r) = export(&d, "t.psd", &Default::default()) else { continue };
        let started = std::time::Instant::now();
        let _ = import("t.psd", &r.bytes);
        assert!(started.elapsed().as_secs() < 20, "{size} pt × {scale}");
    }
}

/// Empty layer channels are written raw, as Reference does: a ZIP stream for a zero-sized channel
/// made readers decompress an invalid extent (storytold/photocraft@c833294e).
#[test]
fn empty_channels_are_written_raw() {
    let mut d = photosuite_doc::Document::new("t", photosuite_geom::Size::new(8, 8), photosuite_color::ColorMode::Rgb, photosuite_color::SampleType::F32);
    d.layers.push(photosuite_doc::Layer::raster("empty", d.pixel_format()));
    let f = PsdFile::from_bytes(&export(&d, "t.psd", &Default::default()).unwrap().bytes).unwrap();
    let rec = f.layers().iter().find(|r| r.name() == "empty").unwrap();
    assert!(rec.channels.iter().all(|c| c.compression.unwrap_or(Compression::Raw) == Compression::Raw), "{:?}", rec.channels.iter().map(|c| c.compression).collect::<Vec<_>>());
}

/// A regenerated fill block is padded to a multiple of 4 inside its length, as Reference pads
/// layer blocks (storytold/photocraft@e2aa1934).
#[test]
fn regenerated_fill_blocks_are_padded_to_four() {
    for name in ["P", "Pa", "Pat", "Patt"] {
        let mut d = photosuite_doc::Document::new("t", photosuite_geom::Size::new(8, 8), photosuite_color::ColorMode::Rgb, photosuite_color::SampleType::U8);
        let fill = photosuite_doc::Fill::Pattern { name: name.into(), scale: 1.0, id: String::new(), angle: 0.0, link: true, phase: (0.0, 0.0) };
        d.layers.push(photosuite_doc::Layer::new("fill", LayerContent::Fill(fill)));
        let f = PsdFile::from_bytes(&export(&d, "t.psd", &Default::default()).unwrap().bytes).unwrap();
        let b = f.layers().iter().flat_map(|r| &r.blocks).find(|b| &b.key == b"PtFl").expect("PtFl");
        assert_eq!(b.data.len() % 4, 0, "{name}: {}", b.data.len());
    }
}

/// A bottom layer stored without a transparency channel opens as the locked Background, whatever
/// its name and even without lock data (other writers' files; storytold/photocraft@57624bd0).
#[test]
fn a_bottom_record_without_transparency_opens_as_the_background() {
    let d = photosuite_doc::Document::with_background("bg", photosuite_geom::Size::new(6, 4), photosuite_color::ColorMode::Rgb, photosuite_color::SampleType::U8, photosuite_color::Color::rgb(0.2, 0.4, 0.6));
    let mut f = PsdFile::from_bytes(&export(&d, "t.psd", &Default::default()).unwrap().bytes).unwrap();
    let rec = &mut f.layers_mut()[0];
    assert!(rec.channels.iter().all(|c| c.id >= 0), "written as Reference's Background");
    rec.blocks.retain(|b| &b.key != b"lspf");
    rec.flags = photosuite_psd::LayerFlags(photosuite_psd::LayerFlags::BIT4_USEFUL);
    rec.name = b"Layer 1".to_vec();
    let back = import("t.psd", &f.to_bytes().unwrap()).unwrap().document;
    let bg = &back.layers[0];
    assert!(bg.locks.transparency && bg.locks.position, "{:?}", bg.locks);
}

/// Flag bit 5 (undocumented) is set on a layer with effects or smart filters and on no other, as
/// the reference editor writes it (every such layer of its corpus files).
#[test]
fn effects_set_layer_flag_bit_5() {
    let mut d = photosuite_doc::Document::new("t", photosuite_geom::Size::new(8, 8), photosuite_color::ColorMode::Rgb, photosuite_color::SampleType::U8);
    let fmt = d.pixel_format();
    let mut fx = photosuite_doc::Layer::raster("fx", fmt);
    fx.effects.items.push(photosuite_doc::Effect::default_drop_shadow());
    d.layers.push(photosuite_doc::Layer::raster("plain", fmt));
    d.layers.push(fx);
    let bytes = export(&d, "t.psd", &ExportOptions::default()).unwrap().bytes;
    let f = PsdFile::from_bytes(&bytes).unwrap();
    let bit5 = |i: usize| f.layers()[i].flags.0 & photosuite_psd::LayerFlags::EFFECTS != 0;
    assert_eq!((bit5(0), bit5(1)), (false, true));
}

/// The selected layers go into resources 1024 (target record) and 1069 (layer ids) and come back
/// on import, the target first; a selected group is its own record, not its closing divider.
/// Without a selection the top record is the target, as before.
#[test]
fn selected_layers_round_trip() {
    use photosuite_doc::{Document, Layer};
    let mut d = Document::new("t", photosuite_geom::Size::new(8, 8), photosuite_color::ColorMode::Rgb, photosuite_color::SampleType::U8);
    let fmt = d.pixel_format();
    let (a, b) = (Layer::raster("A", fmt), Layer::raster("B", fmt));
    let inner = Layer::raster("inner", fmt);
    let group = Layer::group("G", vec![inner]);
    let (ia, ib, ig) = (a.id, b.id, group.id);
    d.layers.extend([a, b, group]);
    let names = |r: &ImportResult| -> Vec<String> { r.selected.iter().map(|id| r.document.layer(*id).unwrap().name.clone()).collect() };
    for (sel, want) in [(vec![ib, ia], vec!["B", "A"]), (vec![ig], vec!["G"]), (vec![], vec!["G"])] {
        let opts = ExportOptions { selected_layers: sel.clone(), ..Default::default() };
        let bytes = export(&d, "t.psd", &opts).unwrap().bytes;
        let back = import("t.psd", &bytes).unwrap();
        assert_eq!(names(&back), want, "{sel:?}");
        let f = PsdFile::from_bytes(&bytes).unwrap();
        let target = f.resource(1024).map(|r| u16::from_be_bytes([r.data[0], r.data[1]]) as usize).unwrap();
        assert_ne!(f.layers()[target].section_type(), SectionType::BoundingDivider);
    }
    // Ids the file names that aren't layers, or a target past the records: no selection, no panic.
    let mut f = PsdFile::from_bytes(&export(&d, "t.psd", &ExportOptions::default()).unwrap().bytes).unwrap();
    for r in &mut f.resources {
        match r.id {
            1024 => r.data = vec![0xFF, 0xFF],
            1069 => r.data = vec![0, 9, 0, 0, 0, 77],
            _ => {}
        }
    }
    assert!(import("t.psd", &f.to_bytes().unwrap()).unwrap().selected.is_empty());
}
