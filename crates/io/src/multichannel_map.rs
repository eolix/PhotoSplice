//! Multichannel documents <-> PSD colour mode 7.
//!
//! A Multichannel document has no layers: its image is a set of ink (spot) channels, stored in
//! [`Document::channels`] with ink density as the value (1 = solid ink, like every spot channel).
//! A PSD stores the same planes the way Photoshop shows them in the Channels panel: 0 (black) =
//! solid ink, max = paper. Alpha-type channels (DisplayInfo kind 0 / 1) are stored as-is, like the
//! alpha channels of every other mode (max = selected; storytold/photocraft#5790, measured on a
//! file Reference saved). Channel names come from resources 1006 / 1045 and ink colours from
//! DisplayInfo (1077), exactly as for spot channels in the other modes.

use photosplice_color::{Color, ColorMode, PixelFormat, SampleType};
use photosplice_doc::{AlphaChannel, Document};
use photosplice_geom::{Rect, TILE_SIZE};
use photosplice_psd::file::{GlobalLayerMask, LayerInfoPlacement};
use photosplice_psd::resources::{ImageResource, ids, version_info_resource};
use photosplice_psd::{ColorMode as PsdMode, Compression, Header, ImageData, PsdFile, TaggedBlock};
use photosplice_raster::Surface;

use crate::pixels::{encode_be, psd_depth};

/// Grid and guides.
const GRID_AND_GUIDES: u16 = 1032;
/// Resources that only describe layers: target layer (1024), layer groups (1026), layer
/// selection ids (1069), layer groups enabled (1072), the layer id seed and the alpha ids (both
/// written fresh below). A Multichannel document has no layers.
const LAYER_ONLY_RESOURCES: [u16; 6] = [ids::LAYER_STATE, ids::LAYER_GROUP_INFO, 1069, 1072, ids::DOCUMENT_IDS_SEED, ids::ALPHA_IDS];

/// Value of sample `i` of a big-endian plane, `None` past its end.
fn decode(plane: &[u8], i: usize, s: SampleType) -> Option<f32> {
    let start = i.checked_mul(s.bytes())?;
    let bytes = plane.get(start..start.checked_add(s.bytes())?)?;
    Some(match s {
        SampleType::U8 => f32::from(*bytes.first()?) / 255.0,
        SampleType::U16 => f32::from(u16::from_be_bytes(bytes.try_into().ok()?)) / 65535.0,
        SampleType::F32 => f32::from_be_bytes(bytes.try_into().ok()?),
    })
}

/// Fills `doc` (already in Multichannel mode at the file depth) from the merged planes `all`.
/// Sizes are checked arithmetic and samples are read with bounds checks; each plane is converted
/// one tile row at a time, so the conversion buffer stays one tile row whatever the image size
/// (storytold/photocraft#10541).
pub(crate) fn import(file: &PsdFile, all: &[u8], doc: &mut Document, names: &[String], warnings: &mut Vec<String>) {
    let h = &file.header;
    let depth = doc.depth;
    let (w, hh) = (h.width as usize, h.height as usize);
    let layout = h.row_bytes().checked_mul(hh).zip(w.checked_mul(hh)).filter(|(plane, pixels)| pixels.checked_mul(depth.bytes()) == Some(*plane));
    let Some((plane, _)) = layout else {
        warnings.push("multichannel: the channel data does not match the image size; the channels were not imported".into());
        return;
    };
    let fmt = PixelFormat::new(ColorMode::Grayscale, depth, false);
    let tile = TILE_SIZE as usize;
    // Which planes are inks (stored as tone, 0 = solid ink) and which alpha-type channels
    // (stored as-is), known before the conversion; a plane without a record is an ink.
    let kinds = match (file.resource(crate::channel_map::DISPLAY_INFO), file.resource(crate::channel_map::DISPLAY_INFO_OLD)) {
        (Some(r), _) => crate::channel_map::display_kinds(&r.data, true),
        (None, Some(r)) => crate::channel_map::display_kinds(&r.data, false),
        (None, None) => Vec::new(),
    };
    for k in 0..usize::from(h.channels) {
        let Some(p) = k.checked_mul(plane).and_then(|start| all.get(start..start.checked_add(plane)?)) else {
            warnings.push(format!("multichannel: channel {} is missing", k + 1));
            break;
        };
        let ink = kinds.get(k).copied().flatten().is_none_or(|kind| kind == 2);
        let mut s = Surface::new(fmt);
        let mut vals = Vec::new();
        for y in (0..hh).step_by(tile) {
            let bottom = y.saturating_add(tile).min(hh);
            vals.clear();
            // `w * bottom` fits: it is at most `w * hh`, checked above.
            vals.extend((y * w..bottom * w).map(|i| {
                let v = decode(p, i, depth).unwrap_or(0.0);
                if ink { 1.0 - v } else { v }
            }));
            s.write_region(Rect::new(0, y as i32, h.width as i32, bottom as i32), &vals);
        }
        s.prune();
        let name = names.get(k).cloned().filter(|s| !s.is_empty()).unwrap_or_else(|| format!("Channel {}", k + 1));
        // Without DisplayInfo the channels print in black, as Photoshop shows them. DisplayInfo
        // turns alpha-type channels (selected / masked areas) back into alpha channels.
        doc.channels.push(AlphaChannel { spot: Some((Color::BLACK, 0.0)), ..AlphaChannel::new(name, s) });
    }
    if let Some(r) = file.resource(crate::channel_map::DISPLAY_INFO) {
        crate::channel_map::apply_display_info(&r.data, true, 0, &mut doc.channels);
    } else if let Some(r) = file.resource(crate::channel_map::DISPLAY_INFO_OLD) {
        crate::channel_map::apply_display_info(&r.data, false, 0, &mut doc.channels);
    }
}

/// Writes a Multichannel document as a mode-7 PSD (no layer section; every channel a plane).
/// A file past `size_limit` bytes is written as PSB (see `psd_export::fit_psd_size`).
pub(crate) fn document_to_psd(doc: &Document, force_psb: bool, size_limit: u64) -> (PsdFile, Vec<String>) {
    let mut warnings = Vec::new();
    // Photoshop's Multichannel mode is 8 or 16 bits per channel.
    let sample = match doc.depth {
        SampleType::F32 => {
            warnings.push("32-bit Multichannel written as 16 bits/channel (32-bit Multichannel isn't supported)".into());
            SampleType::U16
        }
        s => s,
    };
    if !doc.layers.is_empty() {
        warnings.push("Multichannel documents have no layers: the layers were not written".into());
    }
    let canvas = doc.bounds();
    let channels: Vec<&AlphaChannel> = doc.channels.iter().take(56).collect();
    if doc.channels.len() > 56 {
        warnings.push("only 56 channels fit in PSD; the rest were dropped".into());
    }
    let mut planes = Vec::new();
    if channels.is_empty() {
        // A PSD needs at least one channel: an empty (paper) plane.
        for _ in 0..canvas.width() as usize * canvas.height() as usize {
            encode_be(1.0, sample, &mut planes);
        }
    }
    for ch in &channels {
        // Inks are stored as tone (0 = solid ink), alpha-type channels as-is.
        let ink = ch.spot.is_some();
        for v in ch.surface.read_region(canvas).chunks_exact(ch.surface.channels()) {
            encode_be(if ink { 1.0 - v[0] } else { v[0] }, sample, &mut planes);
        }
    }
    let big = doc.size.width > 30_000 || doc.size.height > 30_000;
    let version = crate::psd_export::psd_version(doc, force_psb);
    if big && !force_psb {
        warnings.push("document exceeds 30000 px; written as PSB".into());
    }
    let header = Header::new(version, doc.size.width, doc.size.height, channels.len().max(1) as u16, psd_depth(sample), PsdMode::Multichannel);
    let image_data = ImageData::encode(Compression::Rle, &planes, &header)
        .or_else(|_| ImageData::encode(Compression::Raw, &planes, &header))
        .unwrap_or(ImageData { compression: Compression::Raw, data: planes });
    let mut resources = vec![
        ImageResource::new(ids::RESOLUTION_INFO, crate::psd_export::resolution_resource(doc)),
        ImageResource::new(ids::GLOBAL_ANGLE, (doc.global_light.angle.round() as i32).to_be_bytes().to_vec()),
        ImageResource::new(ids::GLOBAL_ALTITUDE, (doc.global_light.altitude.round() as i32).to_be_bytes().to_vec()),
        // Photoshop writes the grid and guides resource in every file, guides or not.
        ImageResource::new(GRID_AND_GUIDES, crate::psd_export::guides_resource(doc)),
    ];
    if !channels.is_empty() {
        let names: Vec<&str> = channels.iter().map(|c| c.name.as_str()).collect();
        resources.push(ImageResource::new(1006, crate::psd_export::pascal_names_resource(&names)));
        resources.push(ImageResource::new(1045, crate::psd_export::unicode_names_resource(&names)));
        let old = doc.metadata.psd_resources.iter().find(|r| r.0 == ids::ALPHA_IDS).map(|r| r.2.as_slice());
        resources.push(ImageResource::new(ids::ALPHA_IDS, crate::psd_export::alpha_ids_resource(old, false, names.len())));
        resources.push(ImageResource::new(crate::channel_map::DISPLAY_INFO, crate::channel_map::display_info(false, &channels)));
    }
    resources.extend(crate::psd_export::path_resources(doc, &mut warnings));
    // The layer id seed (Photoshop writes it in every file): the file's, as there are no layers.
    let seed = doc.metadata.psd_resources.iter().find(|r| r.0 == ids::DOCUMENT_IDS_SEED).and_then(|r| r.2.get(..4)).map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    resources.push(ImageResource::new(ids::DOCUMENT_IDS_SEED, seed.to_be_bytes().to_vec()));
    // The XMP and EXIF resolution follow the ResolutionInfo resource, so no copy contradicts it.
    let ppi = Some((doc.resolution_dpi, doc.resolution_dpi)).filter(|d| d.0 > 0.0);
    if let Some(x) = &doc.metadata.xmp {
        resources.push(ImageResource::new(ids::XMP, photosplice_codecs::export_xmp(x, ppi).as_bytes().to_vec()));
    }
    if let Some(e) = &doc.metadata.exif {
        resources.push(ImageResource::new(ids::EXIF, photosplice_codecs::export_exif(e, ppi).into_owned()));
    }
    if let Some(data) = crate::annotations_map::fresh_scale_resource(doc) {
        resources.push(ImageResource::new(crate::annotations_map::MEASUREMENT_SCALE, data));
    }
    // The file's other resources (print settings, IPTC, …) as imported; not the ones that only
    // describe layers, nor any written above, nor a stale clipping path or measurement scale.
    let written: Vec<u16> = resources.iter().map(|r| r.id).chain([ids::VERSION_INFO]).collect();
    for (id, name, data) in &doc.metadata.psd_resources {
        if LAYER_ONLY_RESOURCES.contains(id) || written.contains(id) || !crate::psd_export::keep_imported_resource(doc, *id, data) {
            continue;
        }
        let mut r = ImageResource::new(*id, data.to_vec());
        r.name = crate::psd_export::legacy_name(name);
        resources.push(r);
    }
    resources.push(version_info_resource(true));
    // Document-level tagged blocks (patterns, notes, ...) are valid without a layer section; a
    // kept layer block would describe layers the document doesn't have.
    // Padded to 4 as Photoshop writes them; a kept `8B64` block is `8BIM` in a PSD.
    let global_blocks: Vec<TaggedBlock> = crate::annotations_map::export_blocks(doc, crate::pattern_map::export_global_blocks(doc))
        .into_iter()
        .filter(|(_, key, _)| !matches!(key, b"Layr" | b"Lr16" | b"Lr32") && *key != crate::psd_export::GLOBAL_MASK_KEY)
        .map(|(sig, key, data)| crate::psd_export::global_block(crate::psd_export::block_signature(sig, key, version), key, &data))
        .collect();
    let color_mode_data = photosplice_psd::hdr::color_mode_data_for_depth(header.depth);
    let mut file = PsdFile {
        header,
        color_mode_data,
        resources,
        layer_info: None,
        layer_info_placement: LayerInfoPlacement::Section,
        // The file's global layer mask info when it was kept, else the default record.
        global_layer_mask: (!global_blocks.is_empty()).then(|| {
            let kept = doc.metadata.psd_global_blocks.iter().find(|(_, k, _)| *k == crate::psd_export::GLOBAL_MASK_KEY);
            kept.map_or_else(GlobalLayerMask::default, |(_, _, d)| GlobalLayerMask { data: d.to_vec() })
        }),
        global_blocks,
        layer_mask_trailing: Vec::new(),
        image_data,
    };
    warnings.extend(crate::psd_export::fit_psd_size(&mut file, size_limit));
    (file, warnings)
}

#[cfg(test)]
mod import_tests {
    use super::*;

    /// storytold/photocraft#10541: planes cut short stop the import with a warning, and no
    /// sample is read past the input.
    #[test]
    fn missing_planes_are_reported_without_reading_past_the_input() {
        let file = photosplice_psd::testgen::merged_only(photosplice_psd::Version::Psd, PsdMode::Multichannel, 8, Compression::Raw, 3, 2);
        let mut doc = Document::new("Partial", photosplice_geom::Size::new(3, 2), ColorMode::Multichannel, SampleType::U8);
        let mut warnings = Vec::new();
        import(&file, &[0; 6], &mut doc, &[], &mut warnings);
        assert_eq!(doc.channels.len(), 1);
        assert_eq!(doc.channels[0].surface.read_region(doc.bounds()), vec![1.0; 6]);
        assert_eq!(warnings.len(), 1);
        assert!(decode(&[], usize::MAX, SampleType::F32).is_none());
        assert!(decode(&[1, 2, 3], 1, SampleType::U16).is_none());
        // A depth that doesn't match the planes' layout is refused, not misread.
        let mut doc = Document::new("Wrong depth", photosplice_geom::Size::new(3, 2), ColorMode::Multichannel, SampleType::U16);
        let mut warnings = Vec::new();
        import(&file, &[0; 18], &mut doc, &[], &mut warnings);
        assert!(doc.channels.is_empty());
        assert_eq!(warnings.len(), 1);
    }

    /// A plane taller than a tile is converted in tile rows, every row in place.
    #[test]
    fn planes_are_converted_in_tile_rows() {
        let (w, h) = (3u32, 600u32);
        let file = photosplice_psd::testgen::merged_only(photosplice_psd::Version::Psd, PsdMode::Multichannel, 8, Compression::Raw, w, h);
        let all: Vec<u8> = (0..w * h).map(|i| (i % 251) as u8).collect();
        let mut doc = Document::new("Tall", photosplice_geom::Size::new(w, h), ColorMode::Multichannel, SampleType::U8);
        let mut warnings = Vec::new();
        import(&file, &all, &mut doc, &[], &mut warnings);
        let got = doc.channels[0].surface.read_region(doc.bounds());
        assert!(got.iter().zip(&all).all(|(v, s)| (v - (1.0 - f32::from(*s) / 255.0)).abs() < 1e-6));
    }
}
