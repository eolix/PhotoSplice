//! Indexed Color and Bitmap documents saved back in their own mode.
//!
//! Photoshop keeps both modes flat: no layers, no alpha channels, one plane of palette indices
//! (Indexed, 8-bit, the palette as colour-mode data: 256 reds, 256 greens, 256 blues) or of
//! 1-bit pixels (Bitmap: 1 = black, rows padded to whole bytes). Our model edits them as an
//! RGB 8-bit / Gray 8-bit image ([`photosuite_doc::Document::pixel_format`]); while the document
//! is still a single plain image it is written back that way, else as RGB / Grayscale with
//! layers (with the export's "written as" warning).

use photosuite_color::{ColorMode, SampleType};
use photosuite_doc::{Document, LayerContent};
use photosuite_psd::{ColorMode as PsdMode, Compression, Header, ImageData, PsdFile};

/// Resource 1046: the number of palette entries in use (u16).
pub(crate) const COLOR_COUNT: u16 = 1046;
/// Resource 1047: the palette index shown as transparent (u16).
pub(crate) const TRANSPARENCY_INDEX: u16 = 1047;

/// Resources that only describe layers (not written for a flat file).
const LAYER_RESOURCES: [u16; 4] = [1024, 1026, 1069, 1072];

/// Whether `doc` can be written in its own Indexed or Bitmap mode: one plain, visible pixel layer
/// (what Photoshop's flat modes can hold), no alpha channels or Quick Mask, 8 bits.
pub(crate) fn eligible(doc: &Document) -> bool {
    let mode_ok = match doc.mode {
        ColorMode::Indexed => doc.color_table.as_ref().is_some_and(|t| !t.colors.is_empty()),
        ColorMode::Bitmap => true,
        _ => false,
    };
    let [l] = doc.layers.as_slice() else { return false };
    mode_ok
        && doc.depth == SampleType::U8
        && doc.channels.is_empty()
        && doc.quick_mask.is_none()
        && matches!(l.content, LayerContent::Raster(_))
        && l.visible
        && l.opacity >= 1.0
        && l.fill_opacity >= 1.0
        && l.blend == photosuite_color::BlendMode::Normal
        && l.mask.is_none()
        && l.vector_mask.is_none()
        && l.effects.items.is_empty()
}

/// Rewrites the exported `file` of an [`eligible`] document as a flat Indexed / Bitmap PSD.
pub(crate) fn apply(doc: &Document, file: &mut PsdFile) {
    let px = photosuite_compose::flatten(doc).px;
    let (w, h) = (doc.size.width as usize, doc.size.height as usize);
    // The image over white, as Photoshop shows a flat file; `None` where transparent.
    let over = |p: &[f32; 4]| -> Option<[f32; 3]> { (p[3] >= 0.5).then(|| [0, 1, 2].map(|c| p[c] * p[3] + 1.0 - p[3])) };
    let (mode, depth, plane, color_mode_data) = match (doc.mode, &doc.color_table) {
        (ColorMode::Indexed, Some(table)) => {
            let palette: Vec<[f32; 3]> = table.colors.iter().map(|c| c.map(|v| f32::from(v) / 255.0)).collect();
            let nearest = |rgb: [f32; 3]| -> u8 {
                let d = |c: &[f32; 3]| (0..3).map(|k| (c[k] - rgb[k]).powi(2)).sum::<f32>();
                let best = palette.iter().enumerate().filter(|(i, _)| Some(*i as u8) != table.transparent).min_by(|a, b| d(a.1).total_cmp(&d(b.1)));
                best.map_or(0, |(i, _)| i as u8)
            };
            let plane: Vec<u8> = px.iter().map(|p| over(p).map_or(table.transparent.unwrap_or_else(|| nearest([1.0; 3])), nearest)).collect();
            let mut data = vec![0u8; 768];
            for (i, c) in table.colors.iter().take(256).enumerate() {
                (data[i], data[256 + i], data[512 + i]) = (c[0], c[1], c[2]);
            }
            (PsdMode::Indexed, 8, plane, data)
        }
        _ => {
            // 1 = black: a pixel darker than mid-gray (the model keeps Bitmap pixels at 0 or 1).
            let row = w.div_ceil(8);
            let mut plane = vec![0u8; row * h];
            for (i, p) in px.iter().enumerate() {
                let v = over(p).map_or(1.0, |c| 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]);
                if v < 0.5
                    && let Some(b) = plane.get_mut((i / w) * row + (i % w) / 8)
                {
                    *b |= 0x80 >> (i % w % 8);
                }
            }
            (PsdMode::Bitmap, 1, plane, Vec::new())
        }
    };
    let header = Header::new(file.header.version, doc.size.width, doc.size.height, 1, depth, mode);
    file.image_data = ImageData::encode(Compression::Rle, &plane, &header)
        .or_else(|_| ImageData::encode(Compression::Raw, &plane, &header))
        .unwrap_or(ImageData { compression: Compression::Raw, data: plane });
    file.header = header;
    file.color_mode_data = color_mode_data;
    file.layer_info = None;
    file.global_layer_mask = None;
    file.global_blocks.retain(|b| !matches!(&b.key, b"Lr16" | b"Lr32" | b"Layr"));
    file.resources.retain(|r| !LAYER_RESOURCES.contains(&r.id) && !matches!(r.id, 1006 | 1045 | 1053 | 1077 | COLOR_COUNT | TRANSPARENCY_INDEX));
    if let (PsdMode::Indexed, Some(table)) = (mode, &doc.color_table) {
        let count = table.colors.len().min(256) as u16;
        if count < 256 {
            file.resources.push(photosuite_psd::ImageResource::new(COLOR_COUNT, count.to_be_bytes().to_vec()));
        }
        if let Some(t) = table.transparent {
            file.resources.push(photosuite_psd::ImageResource::new(TRANSPARENCY_INDEX, u16::from(t).to_be_bytes().to_vec()));
        }
    }
}
