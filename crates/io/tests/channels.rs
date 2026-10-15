//! Alpha / spot channels, channel options and the Quick Mask through PSD.

use photosplice_color::{Color, ColorMode, PixelFormat, SampleType};
use photosplice_doc::{AlphaChannel, ColorIndicates, Document};
use photosplice_geom::{Rect, Size};
use photosplice_io::{document_to_psd, psd_to_document};
use photosplice_raster::Surface;

fn channel(depth: SampleType, x1: i32) -> Surface {
    let mut s = Surface::new(PixelFormat::new(ColorMode::Grayscale, depth, false));
    s.fill_rect(Rect::new(0, 0, x1, 8), &[1.0]);
    s
}

#[test]
fn channel_options_spot_and_quick_mask_roundtrip() {
    for depth in SampleType::ALL {
        let mut d = Document::with_background("c", Size::new(16, 8), ColorMode::Rgb, depth, Color::WHITE);
        let mut a = AlphaChannel::new("Alpha 1", channel(depth, 4));
        a.color = Color::rgb(0.0, 0.0, 1.0);
        a.opacity = 0.7;
        a.indicates = ColorIndicates::SelectedAreas;
        d.channels.push(a);
        d.channels.push(AlphaChannel { spot: Some((Color::rgb(1.0, 0.0, 1.0), 0.25)), ..AlphaChannel::new("PANTONE X", channel(depth, 8)) });
        d.quick_mask = Some(AlphaChannel::new("Quick Mask", channel(depth, 12)));
        let (back, warnings) = psd_to_document(&document_to_psd(&d));
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(back.channels.len(), 2, "{depth:?}");
        let (a, s) = (&back.channels[0], &back.channels[1]);
        assert_eq!((a.name.as_str(), a.color, a.indicates), ("Alpha 1", Color::rgb(0.0, 0.0, 1.0), ColorIndicates::SelectedAreas));
        assert!((a.opacity - 0.7).abs() < 1e-6);
        assert_eq!(a.surface.sample_channel(2, 2, 0), 1.0);
        assert_eq!(a.surface.sample_channel(6, 2, 0), 0.0);
        assert_eq!(s.name, "PANTONE X");
        assert_eq!(s.spot, Some((Color::rgb(1.0, 0.0, 1.0), 0.25)));
        let q = back.quick_mask.as_ref().expect("quick mask restored");
        assert_eq!(q.surface.sample_channel(10, 2, 0), 1.0);
        assert_eq!(q.surface.sample_channel(14, 2, 0), 0.0);
        // Re-exporting doesn't duplicate the display-info resources.
        let f2 = document_to_psd(&back);
        assert_eq!(f2.resources.iter().filter(|r| r.id == 1077).count(), 1);
        assert_eq!(f2.resources.iter().filter(|r| r.id == 1022).count(), 1);
    }

    let mut full = Document::with_background("full", Size::new(1, 1), ColorMode::Rgb, SampleType::U8, Color::WHITE);
    full.channels = (0..53).map(|i| AlphaChannel::new(format!("Alpha {i}"), Surface::new(PixelFormat::GRAY8))).collect();
    full.quick_mask = Some(AlphaChannel::new("Quick Mask", Surface::new(PixelFormat::GRAY8)));
    let out = photosplice_io::export(&full, "full.psd", &Default::default()).unwrap();
    assert!(out.warnings.iter().any(|warning| warning.contains("Quick Mask")), "{:?}", out.warnings);
    let file = photosplice_psd::PsdFile::from_bytes(&out.bytes).unwrap();
    assert_eq!(file.header.channels, 56);
    assert!(file.resources.iter().all(|resource| resource.id != 1022));
}

/// A file that carries the older DisplayInfo (1007) beside 1077 gets it back on export, rebuilt
/// from the channels in its 14-byte-record form (psd-tools gray0.psd lost it).
#[test]
fn older_display_info_is_kept() {
    let mut d = Document::with_background("c", Size::new(16, 8), ColorMode::Rgb, SampleType::U8, Color::WHITE);
    d.channels.push(AlphaChannel { opacity: 0.5, ..AlphaChannel::new("Alpha 1", channel(SampleType::U8, 4)) });
    let mut f = document_to_psd(&d);
    assert!(f.resources.iter().all(|r| r.id != 1007), "not written unless the file had it");
    let new = f.resources.iter().find(|r| r.id == 1077).unwrap().data.clone();
    let old: Vec<u8> = new[4..].chunks(13).flat_map(|r| r.iter().copied().chain([0])).collect();
    f.resources.push(photosplice_psd::ImageResource::new(1007, old.clone()));
    let (back, _) = psd_to_document(&f);
    let again = document_to_psd(&back);
    let got: Vec<&photosplice_psd::ImageResource> = again.resources.iter().filter(|r| r.id == 1007).collect();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].data, old);
    assert_eq!(again.resources.iter().find(|r| r.id == 1077).unwrap().data, new);
}

/// A positive layer count with "Transparency" first in the channel names: the extra channel is
/// the merged transparency, not an alpha channel (psd-tools transparentbg.psd was written back
/// with it twice).
#[test]
fn transparency_named_channel_is_the_merged_alpha() {
    let mut d = Document::new("t", Size::new(16, 8), ColorMode::Rgb, SampleType::U8);
    let mut l = photosplice_doc::Layer::raster("l", d.pixel_format());
    l.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 8, 8), &[1.0, 0.0, 0.0, 1.0]);
    d.layers.push(l);
    let mut f = document_to_psd(&d);
    assert!(f.merged_has_alpha());
    let channels = f.header.channels;
    f.layer_info.as_mut().unwrap().merged_alpha = false;
    assert!(f.merged_has_alpha(), "the names say so");
    let (back, _) = psd_to_document(&f);
    assert!(back.channels.is_empty(), "{:?}", back.channels.iter().map(|c| &c.name).collect::<Vec<_>>());
    assert_eq!(document_to_psd(&back).header.channels, channels);
    // A localised name ("透明部分", psd-tools transparentbg.psb): its alpha id 0 still says so.
    let names = f.resources.iter_mut().find(|r| r.id == 1045).unwrap();
    let utf16: Vec<u16> = "透明部分".encode_utf16().chain([0]).collect();
    names.data = (utf16.len() as u32).to_be_bytes().into_iter().chain(utf16.iter().flat_map(|u| u.to_be_bytes())).collect();
    assert!(f.merged_has_alpha());
    let (back, _) = psd_to_document(&f);
    assert!(back.channels.is_empty());
}
