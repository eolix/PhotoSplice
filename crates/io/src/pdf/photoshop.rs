//! Photoshop's editing data in a PDF (Save As › Photoshop PDF › *Preserve Photoshop Editing
//! Capabilities*): what lets a Photoshop PDF reopen with its layers, in Photoshop and here.
//!
//! The layout, as Photoshop writes it (observed in its own files): the page's
//! `/PieceInfo << /AdobePhotoshop << /LastModified (…) /Private R >> >>` (ISO 32000 §14.5,
//! page-piece dictionaries), whose private dictionary holds
//!
//! * `StandardImageFileData`: a stream of the tagged blocks a layered TIFF keeps in its 37724
//!   tag, without that tag's header string (`Layr` / `Lr16` / `Lr32` with the layers, `LMsk`,
//!   then the global blocks: patterns, linked files, …), see [`photosplice_psd::tiff`];
//!   `StandardImageFileLength` is its decoded length;
//! * `ImageResources`: a stream of the PSD image resources, as in the TIFF 34377 tag;
//! * `CompositeImage`: the merged image as an image XObject, its colour space carrying the ICC
//!   profile and its `SMask` the transparency (matted against white, `Matte [1 1 1]`);
//! * the save options: `RoundTrip`, `SaveTransparency`, `EmbedFonts`, `ExportCrispy`,
//!   `UseTextOutlines`.
//!
//! The page's own content is only what other PDF readers show; reopening uses this data, which is
//! rebuilt into a PSD model and opened by the PSD importer, so a Photoshop PDF opens with
//! everything a PSD does. Saving runs the PSD exporter and stores its parts the same way, so
//! Photoshop reopens PhotoSplice's PDFs with their layers too. krilla writes the page; the editing
//! data is added to it as an incremental update (ISO 32000 §7.5.6), as Acrobat saves edits.

use std::sync::Arc;

use hayro::hayro_syntax::Pdf;
use hayro::hayro_syntax::object::{Array, Dict, Name, Object, Stream};
use hayro::hayro_syntax::page::Page;
use photosplice_doc::Document;
use photosplice_psd::resources::ids;
use photosplice_psd::tiff::{ByteOrder, ImageSourceData, SIGNATURE, SIGNATURE_PSB, resources_from_bytes, resources_to_bytes};
use photosplice_psd::{ColorMode as PsdMode, Compression, Header, ImageData, ImageResource};

use super::update::Update;
use crate::psd_export::{PsdExportOptions, document_to_psd_with};
use crate::{ImportResult, IoError};

/// Largest merged image read back (the PSD importer's own pixel limit applies after).
const MAX_PIXELS: u64 = 1 << 31;

/// The private dictionary of the page's Photoshop data, if it has some.
fn private<'a>(page: &Page<'a>) -> Option<Dict<'a>> {
    page.raw().get::<Dict<'_>>(b"PieceInfo")?.get::<Dict<'_>>(b"AdobePhotoshop")?.get::<Dict<'_>>(b"Private")
}

/// Whether page `page` (0-based) carries Photoshop editing data: it opens as its document, with
/// no Import PDF options, as in Photoshop.
pub fn has_editing_data(pdf: &Pdf, page: usize) -> bool {
    pdf.pages().get(page).and_then(|p| private(p)).is_some_and(|d| d.get::<Stream<'_>>(b"StandardImageFileData").is_some())
}

/// Opens the document in page `page`'s Photoshop editing data. `None` when there is none;
/// `Some(Err(reason))` when it is there but unreadable (the caller opens the page instead).
pub(super) fn read(name: &str, pdf: &Pdf, page: &Page<'_>) -> Option<Result<ImportResult, String>> {
    let private = private(page)?;
    let layers = private.get::<Stream<'_>>(b"StandardImageFileData")?;
    Some(read_private(name, pdf, &private, &layers))
}

fn read_private(name: &str, pdf: &Pdf, private: &Dict<'_>, layers: &Stream<'_>) -> Result<ImportResult, String> {
    let mut warnings = Vec::new();
    let blocks = layers.decoded().map_err(|e| format!("the layer data can't be decoded ({e:?})"))?;
    // A damaged stream can decode to less than was written, which would open as fewer layers.
    let expected = private.get::<Object<'_>>(b"StandardImageFileLength").and_then(|o| o.into_number()).map(|n| n.as_f64());
    if blocks.is_empty() || expected.is_some_and(|n| n != blocks.len() as f64) {
        return Err("the layer data is damaged".into());
    }
    // The stream is a 37724 tag's blocks without its header string; PSB-sized data (8-byte
    // lengths) is told apart only by failing to read as PSD.
    let isd = if blocks.starts_with(SIGNATURE) || blocks.starts_with(SIGNATURE_PSB) {
        ImageSourceData::from_bytes(&blocks)
    } else {
        let with = |sig: &[u8]| [sig, &blocks[..]].concat();
        ImageSourceData::from_bytes(&with(SIGNATURE)).or_else(|_| ImageSourceData::from_bytes(&with(SIGNATURE_PSB)))
    };
    let (isd, w) = isd.map_err(|e| format!("the layer data can't be read ({e})"))?;
    warnings.extend(w.into_iter().map(|w| format!("layer data: {w}")));

    let mut resources = match private.get::<Stream<'_>>(b"ImageResources").map(|s| s.decoded()) {
        Some(Ok(data)) if !data.is_empty() => match resources_from_bytes(&data) {
            Ok((r, w)) => {
                warnings.extend(w.into_iter().map(|w| format!("image resources: {w}")));
                r
            }
            Err(e) => {
                warnings.push(format!("the Photoshop image resources could not be read: {e}"));
                Vec::new()
            }
        },
        _ => Vec::new(),
    };

    let composite = private.get::<Stream<'_>>(b"CompositeImage").ok_or("there is no composite image")?;
    let c = composite.dict();
    let num = |k: &[u8]| c.get::<Object<'_>>(k).and_then(|o| o.into_i32()).and_then(|v| u32::try_from(v).ok());
    let (w, h) = (num(b"Width").ok_or("the composite has no width")?, num(b"Height").ok_or("the composite has no height")?);
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err(format!("the composite's size ({w} × {h}) is out of range"));
    }
    let bpc = num(b"BitsPerComponent").unwrap_or(8);
    if !matches!(bpc, 8 | 16) {
        return Err(format!("{bpc}-bit composites are not supported"));
    }
    let space = color_space(c).ok_or("the composite's colour space is not supported")?;
    if let Some(icc) = &space.icc
        && !resources.iter().any(|r| r.id == ids::ICC_PROFILE)
    {
        resources.push(ImageResource::new(ids::ICC_PROFILE, icc.clone()));
    }
    // Photoshop keeps the document's XMP as the catalog's metadata rather than as a resource.
    if !resources.iter().any(|r| r.id == ids::XMP)
        && let Some(xmp) = catalog_xmp(pdf)
    {
        resources.push(ImageResource::new(ids::XMP, xmp));
    }

    let depth = match &isd.layer_info_key {
        b"Lr16" => 16,
        b"Lr32" => 32,
        _ if isd.layer_info.is_none() => bpc as u16,
        _ => 8,
    };
    let alpha = isd.layer_info.as_ref().is_some_and(|l| l.merged_alpha);
    let cc = space.channels;
    let n = w as usize * h as usize;
    let samples = composite.decoded().map_err(|e| format!("the composite can't be decoded ({e:?})"))?;
    let bytes = (bpc / 8) as usize;
    let mut planes = Vec::new();
    planes.try_reserve_exact(n * (cc + usize::from(alpha)) * usize::from(depth / 8)).map_err(|_| "the composite is too large".to_string())?;
    // Interleaved PDF samples → planar PSD planes at the document's depth; PDF CMYK is ink
    // (0 = none), PSD CMYK the inverse.
    let invert = space.mode == PsdMode::Cmyk;
    let sample = |data: &[u8], i: usize| -> f64 {
        let at = i * bytes;
        match data.get(at..at + bytes) {
            Some([v]) => f64::from(*v) / 255.0,
            Some([a, b]) => f64::from(u16::from_be_bytes([*a, *b])) / 65535.0,
            _ => 0.0,
        }
    };
    let put = |planes: &mut Vec<u8>, v: f64| match depth {
        16 => planes.extend_from_slice(&((v.clamp(0.0, 1.0) * 65535.0).round() as u16).to_be_bytes()),
        32 => planes.extend_from_slice(&(v as f32).to_be_bytes()),
        _ => planes.push((v.clamp(0.0, 1.0) * 255.0).round() as u8),
    };
    if samples.len() < n * cc * bytes {
        warnings.push("the composite image is shorter than its size; the rest is blank".into());
    }
    for ch in 0..cc {
        for i in 0..n {
            let v = sample(&samples, i * cc + ch);
            put(&mut planes, if invert { 1.0 - v } else { v });
        }
    }
    if alpha {
        let mask = c.get::<Stream<'_>>(b"SMask").and_then(|m| m.decoded().ok());
        for i in 0..n {
            put(&mut planes, mask.as_deref().map_or(1.0, |m| if m.len() >= n * bytes { sample(m, i) } else { 1.0 }));
        }
    }
    let channels = u16::try_from(cc + usize::from(alpha)).map_err(|_| "too many channels".to_string())?;
    let header = Header::new(isd.version, w, h, channels, depth, space.mode);
    let file = isd.into_psd(header, resources, ImageData { compression: Compression::Raw, data: planes });
    let (mut document, w) = crate::psd_to_document(&file);
    warnings.extend(w);
    document.name = name.to_string();
    if document.icc_profile.is_none() {
        document.icc_profile = space.icc.map(Arc::new);
    }
    Ok(ImportResult { document, warnings, selected: Vec::new(), source_read_only: false, preview_only: false })
}

struct Space {
    mode: PsdMode,
    channels: usize,
    icc: Option<Vec<u8>>,
}

fn color_space(c: &Dict<'_>) -> Option<Space> {
    let device = |n: &[u8]| match n {
        b"DeviceGray" | b"G" => Some((PsdMode::Grayscale, 1)),
        b"DeviceRGB" | b"RGB" => Some((PsdMode::Rgb, 3)),
        b"DeviceCMYK" | b"CMYK" => Some((PsdMode::Cmyk, 4)),
        _ => None,
    };
    if let Some(n) = c.get::<Name<'_>>(b"ColorSpace") {
        let (mode, channels) = device(n.as_ref())?;
        return Some(Space { mode, channels, icc: None });
    }
    let a = c.get::<Array<'_>>(b"ColorSpace")?;
    let mut it = a.iter::<Object<'_>>();
    let kind = it.next()?.into_name()?;
    match kind.as_ref() {
        b"ICCBased" => {
            let s = it.next()?.into_stream()?;
            let channels = s.dict().get::<Object<'_>>(b"N").and_then(|o| o.into_i32())?;
            let (mode, channels) = match channels {
                1 => (PsdMode::Grayscale, 1),
                3 => (PsdMode::Rgb, 3),
                4 => (PsdMode::Cmyk, 4),
                _ => return None,
            };
            Some(Space { mode, channels, icc: s.decoded().ok().map(|d| d.into_owned()) })
        }
        // Photoshop's Lab: L 0…100 and a, b −128…127 over the sample range, the PSD encoding.
        b"Lab" => Some(Space { mode: PsdMode::Lab, channels: 3, icc: None }),
        other => device(other).map(|(mode, channels)| Space { mode, channels, icc: None }),
    }
}

fn catalog_xmp(pdf: &Pdf) -> Option<Vec<u8>> {
    let xref = pdf.xref();
    let root = xref.get::<Dict<'_>>(xref.root_id())?;
    root.get::<Stream<'_>>(b"Metadata")?.decoded().ok().map(|d| d.into_owned())
}

/// Adds `doc` as Photoshop editing data to the first page of `pdf` (a file krilla wrote).
pub(super) fn attach(pdf: Vec<u8>, doc: &Document, warnings: &mut Vec<String>) -> Result<Vec<u8>, IoError> {
    let big = doc.size.width > 30_000 || doc.size.height > 30_000;
    // Raw so the planes can be interleaved; matted against white as Photoshop's composite is.
    let opts = PsdExportOptions { force_psb: big, merged_matte: true, merged_compression: Compression::Raw, native_modes: false, selected_layers: Vec::new(), ..PsdExportOptions::default() };
    let (file, w) = document_to_psd_with(doc, &opts);
    warnings.extend(w);
    let (mode, cc) = match file.header.color_mode {
        PsdMode::Grayscale => (PsdMode::Grayscale, 1),
        PsdMode::Rgb => (PsdMode::Rgb, 3),
        PsdMode::Cmyk => (PsdMode::Cmyk, 4),
        PsdMode::Lab => (PsdMode::Lab, 3),
        other => return Err(IoError::Unsupported(format!("PDF: {other:?} documents can't keep Photoshop editing data"))),
    };
    let (blocks, w) = ImageSourceData::from_psd(&file).to_bytes(ByteOrder::Big)?;
    warnings.extend(w.into_iter().map(|w| format!("layer data: {w}")));
    let blocks = blocks.get(SIGNATURE.len()..).unwrap_or_default();
    // The profile goes with the composite's colour space, as in Photoshop's PDFs.
    let resources: Vec<ImageResource> = file.resources.iter().filter(|r| r.id != ids::ICC_PROFILE).cloned().collect();
    let resources = resources_to_bytes(&resources)?;

    let (w, h) = (file.header.width as usize, file.header.height as usize);
    let n = w * h;
    let src = usize::from(file.header.depth / 8).max(1);
    let alpha = file.layer_info.as_ref().is_some_and(|l| l.merged_alpha);
    let extra = usize::from(file.header.channels).saturating_sub(cc + usize::from(alpha));
    if extra > 0 {
        warnings.push(format!("PDF: {extra} alpha channel(s) are not kept in the Photoshop editing data"));
    }
    // PSD planes → interleaved PDF samples (8 bits, or 16 for 16- and 32-bit documents).
    let bpc = if file.header.depth == 8 { 1 } else { 2 };
    let planes = &file.image_data.data;
    let value = |c: usize, i: usize| -> f64 {
        let at = (c * n + i) * src;
        match planes.get(at..at + src) {
            Some([v]) => f64::from(*v) / 255.0,
            Some([a, b]) => f64::from(u16::from_be_bytes([*a, *b])) / 65535.0,
            Some([a, b, c, d]) => f64::from(f32::from_be_bytes([*a, *b, *c, *d])),
            _ => 1.0,
        }
    };
    let put = |out: &mut Vec<u8>, v: f64| {
        let v = v.clamp(0.0, 1.0);
        if bpc == 1 { out.push((v * 255.0).round() as u8) } else { out.extend_from_slice(&((v * 65535.0).round() as u16).to_be_bytes()) }
    };
    let mut color = Vec::new();
    color.try_reserve_exact(n * cc * bpc).map_err(|_| IoError::Unsupported("PDF: the composite is too large".into()))?;
    for i in 0..n {
        for c in 0..cc {
            let v = value(c, i);
            put(&mut color, if mode == PsdMode::Cmyk { 1.0 - v } else { v });
        }
    }
    let mask = alpha.then(|| {
        let mut m = Vec::with_capacity(n * bpc);
        for i in 0..n {
            put(&mut m, value(cc, i));
        }
        m
    });

    let mut up = Update::new(pdf)?;
    let page = up.first_page()?;
    let icc = doc.icc_profile.as_ref().filter(|_| mode != PsdMode::Lab).map(|p| up.stream(&format!("/N {cc}"), p));
    let space = match (mode, icc) {
        (_, Some(r)) => format!("[/ICCBased {r} 0 R]"),
        (PsdMode::Lab, _) => "[/Lab << /WhitePoint [0.9642 1.0 0.8249] /Range [-128 127 -128 127] >>]".into(),
        (PsdMode::Grayscale, _) => "/DeviceGray".into(),
        (PsdMode::Cmyk, _) => "/DeviceCMYK".into(),
        _ => "/DeviceRGB".into(),
    };
    let bits = bpc * 8;
    let image = |extra: &str| format!("/Type /XObject /Subtype /Image /Width {w} /Height {h} /BitsPerComponent {bits}{extra}");
    let smask = mask.map(|m| up.stream(&image(" /ColorSpace /DeviceGray /Matte [1 1 1]"), &m));
    let smask = smask.map(|r| format!(" /SMask {r} 0 R /Matte [1 1 1]")).unwrap_or_default();
    let composite = up.stream(&image(&format!(" /ColorSpace {space}{smask}")), &color);
    let layers = up.stream("", blocks);
    let resources = up.stream("", &resources);
    let date = pdf_date();
    let private = up.object(&format!(
        "<< /CompositeImage {composite} 0 R /EmbedFonts true /ExportCrispy true /ImageResources {resources} 0 R /RoundTrip true \
         /SaveTransparency {alpha} /StandardImageFileData {layers} 0 R /StandardImageFileLength {} /UseTextOutlines false >>",
        blocks.len()
    ));
    let piece = format!("/PieceInfo << /AdobePhotoshop << /LastModified ({date}) /Private {private} 0 R >> >> /LastModified ({date})");
    up.replace_page(page, &piece)?;
    up.finish()
}

/// Now as a PDF date (UTC); a fixed date where the platform has no clock.
fn pdf_date() -> String {
    let (y, m, d, hh, mm, ss) = now_utc();
    format!("D:{y:04}{m:02}{d:02}{hh:02}{mm:02}{ss:02}Z")
}

/// Now in UTC: year, month, day, hours, minutes, seconds (1970-01-01 where the platform has no
/// clock).
pub(super) fn now_utc() -> (i64, i64, i64, u64, u64, u64) {
    #[cfg(not(target_arch = "wasm32"))]
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    #[cfg(target_arch = "wasm32")]
    let secs = 0u64;
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Days since 1970-01-01 → civil date (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, rest / 3600, rest % 3600 / 60, rest % 60)
}

