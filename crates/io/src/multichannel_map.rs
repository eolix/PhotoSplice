//! Multichannel documents <-> PSD colour mode 7.
//!
//! A Multichannel document has no layers: its image is a set of ink (spot) channels, stored in
//! [`Document::channels`] with ink density as the value (1 = solid ink, like every spot channel).
//! A PSD stores the same planes the way Photoshop shows them in the Channels panel: 0 (black) =
//! solid ink, max = paper. Channel names come from resources 1006 / 1045 and ink colours from
//! DisplayInfo (1077), exactly as for spot channels in the other modes.

use photosplice_color::{Color, ColorMode, PixelFormat, SampleType};
use photosplice_doc::{AlphaChannel, Document};
use photosplice_geom::Rect;
use photosplice_psd::file::LayerInfoPlacement;
use photosplice_psd::resources::{ImageResource, ids, version_info_resource};
use photosplice_psd::{ColorMode as PsdMode, Compression, Header, ImageData, PsdFile};
use photosplice_raster::Surface;

use crate::pixels::{encode_be, psd_depth};

/// Resources that only describe layers (target and selected layers, link groups, layer groups
/// enabled, the layer id seed, alpha ids): a Multichannel file has no layers.
const LAYER_RESOURCES: [u16; 6] = [1024, 1026, 1069, 1072, ids::DOCUMENT_IDS_SEED, ids::ALPHA_IDS];

/// The document-level tagged blocks kept from the file (`FMsk`, `Patt`, …), padded to 4 as
/// Photoshop writes them; not layer data.
fn global_blocks(doc: &Document, version: photosplice_psd::Version) -> Vec<photosplice_psd::TaggedBlock> {
    doc.metadata
        .psd_global_blocks
        .iter()
        .filter(|(_, key, _)| !matches!(key, b"Layr" | b"Lr16" | b"Lr32"))
        .map(|(sig, key, data)| {
            let mut tb = photosplice_psd::TaggedBlock::new(*key, data.to_vec());
            tb.signature = crate::psd_export::block_signature(*sig, version);
            tb.padding = Some(vec![0; (4 - data.len() % 4) % 4]);
            tb
        })
        .collect()
}

/// Value of sample `i` of a big-endian plane.
fn decode(plane: &[u8], i: usize, s: SampleType) -> f32 {
    match s {
        SampleType::U8 => f32::from(plane[i]) / 255.0,
        SampleType::U16 => f32::from(u16::from_be_bytes([plane[2 * i], plane[2 * i + 1]])) / 65535.0,
        SampleType::F32 => f32::from_be_bytes([plane[4 * i], plane[4 * i + 1], plane[4 * i + 2], plane[4 * i + 3]]),
    }
}

/// Fills `doc` (already in Multichannel mode at the file depth) from the merged planes `all`.
pub(crate) fn import(file: &PsdFile, all: &[u8], doc: &mut Document, names: &[String], warnings: &mut Vec<String>) {
    let h = &file.header;
    let depth = doc.depth;
    let (w, hh) = (h.width as usize, h.height as usize);
    let n = w * hh;
    let plane = h.row_bytes() * hh;
    let canvas = Rect::new(0, 0, h.width as i32, h.height as i32);
    let fmt = PixelFormat::new(ColorMode::Grayscale, depth, false);
    for k in 0..usize::from(h.channels) {
        let Some(p) = all.get(k * plane..(k + 1) * plane) else {
            warnings.push(format!("multichannel: channel {} is missing", k + 1));
            break;
        };
        let vals: Vec<f32> = (0..n).map(|i| 1.0 - decode(p, i, depth)).collect();
        let mut s = Surface::new(fmt);
        s.write_region(canvas, &vals);
        s.prune();
        let name = names.get(k).cloned().filter(|s| !s.is_empty()).unwrap_or_else(|| format!("Channel {}", k + 1));
        // Without DisplayInfo the channels print in black, as Photoshop shows them.
        doc.channels.push(AlphaChannel { spot: Some((Color::BLACK, 0.0)), ..AlphaChannel::new(name, s) });
    }
    if let Some(r) = file.resource(crate::channel_map::DISPLAY_INFO) {
        crate::channel_map::apply_display_info(&r.data, true, 0, &mut doc.channels);
    } else if let Some(r) = file.resource(crate::channel_map::DISPLAY_INFO_OLD) {
        crate::channel_map::apply_display_info(&r.data, false, 0, &mut doc.channels);
    }
}

/// Writes a Multichannel document as a mode-7 PSD (no layer section; every channel a plane).
pub(crate) fn document_to_psd(doc: &Document, force_psb: bool) -> (PsdFile, Vec<String>) {
    let mut warnings = Vec::new();
    // Photoshop's Multichannel mode is 8 or 16 bits per channel.
    let sample = match doc.depth {
        SampleType::F32 => {
            warnings.push("32-bit Multichannel written as 16 bits/channel (Photoshop has no 32-bit Multichannel)".into());
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
        for v in ch.surface.read_region(canvas).chunks_exact(ch.surface.channels()) {
            encode_be(1.0 - v[0], sample, &mut planes);
        }
    }
    let big = doc.size.width > 30_000 || doc.size.height > 30_000;
    let size_exceeds_limit = !force_psb && !big && crate::psd_export::psd_size_exceeds_limit(doc);
    let version = crate::psd_export::psd_version(doc, force_psb, size_exceeds_limit);
    if big && !force_psb {
        warnings.push("document exceeds 30000 px; written as PSB".into());
    } else if size_exceeds_limit && !force_psb {
        warnings.push("estimated encoded size exceeds 2 GB; written as PSB".into());
    }
    let header = Header::new(version, doc.size.width, doc.size.height, channels.len().max(1) as u16, psd_depth(sample), PsdMode::Multichannel);
    let image_data = ImageData::encode(Compression::Rle, &planes, &header)
        .or_else(|_| ImageData::encode(Compression::Raw, &planes, &header))
        .unwrap_or(ImageData { compression: Compression::Raw, data: planes });
    let mut resources = vec![
        ImageResource::new(ids::RESOLUTION_INFO, crate::psd_export::resolution_resource(doc)),
        ImageResource::new(ids::GLOBAL_ANGLE, (doc.global_light.angle.round() as i32).to_be_bytes().to_vec()),
        ImageResource::new(ids::GLOBAL_ALTITUDE, (doc.global_light.altitude.round() as i32).to_be_bytes().to_vec()),
        // Photoshop writes the grid and guides resource in every file.
        ImageResource::new(1032, crate::psd_export::guides_resource(doc)),
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
    if let Some(x) = &doc.metadata.xmp {
        resources.push(ImageResource::new(ids::XMP, x.as_bytes().to_vec()));
    }
    if let Some(e) = &doc.metadata.exif {
        resources.push(ImageResource::new(ids::EXIF, photosplice_codecs::upright_exif(e).into_owned()));
    }
    // The file's other resources (print settings, IPTC, guides, …) as imported; not the ones that
    // only describe layers, nor any written above.
    for (id, name, data) in &doc.metadata.psd_resources {
        if LAYER_RESOURCES.contains(id) || resources.iter().any(|r| r.id == *id) || *id == ids::VERSION_INFO {
            continue;
        }
        // A preserved clipping-path resource only while it names the current clipping path.
        if *id == crate::vector_map::CLIPPING_PATH {
            let n = usize::from(data.first().copied().unwrap_or(0));
            let raw_name = data.get(1..1 + n).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
            if doc.clipping_path.as_ref().is_none_or(|c| c.name != raw_name) {
                continue;
            }
        }
        let mut r = ImageResource::new(*id, data.to_vec());
        r.name = crate::psd_export::legacy_name(name);
        resources.push(r);
    }
    resources.push(version_info_resource(true));
    let color_mode_data = photosplice_psd::hdr::color_mode_data_for_depth(header.depth);
    let file = PsdFile {
        header,
        color_mode_data,
        resources,
        layer_info: None,
        layer_info_placement: LayerInfoPlacement::Section,
        global_layer_mask: None,
        global_blocks: global_blocks(doc, version),
        layer_mask_trailing: Vec::new(),
        image_data,
    };
    (file, warnings)
}
