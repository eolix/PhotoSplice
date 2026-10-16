//! PDF, read and written natively.
//!
//! Opening a page ([`open_page`]) walks its operations with `hayro-interpret` (pure Rust,
//! Apache-2.0 OR MIT; its standard fonts are PDFium's Foxit fonts, BSD-3-Clause) into editable
//! layers: text as type layers, vectors as shape layers, images as pixel layers ([`layers`]), or,
//! with `PageAs::Pixels`, renders it at a resolution, as Photoshop's Import PDF does. A Photoshop
//! PDF (one saved with Photoshop's editing data, by Photoshop or PhotoSplice) opens as its
//! document, every layer as it was ([`photoshop`]).
//!
//! Saving ([`export::export_layered`]) writes the page layer by layer as PDF content, plus the
//! Photoshop editing data. [`raster_pdf`] writes pages of flattened JPEG images (prints, contact
//! sheets, artboards).

pub mod export;
mod gradient;
mod ink;
mod layers;
mod ocg;
mod pages;
mod photoshop;

pub use pages::open_pages;
mod render;
mod shading;
mod update;

use photosplice_codecs::{self as codecs, ChannelLayout};

use crate::{ImportResult, IoError};

/// Photoshop's Import PDF default resolution.
pub const DEFAULT_DPI: f32 = 300.0;

/// The resolution a PDF opens at without Import PDF (print resolution, as Import PDF's default).
pub const OPEN_DPI: f32 = DEFAULT_DPI;

/// Whether `bytes` start like a PDF.
pub fn is_pdf(bytes: &[u8]) -> bool {
    bytes.len() >= 5 && bytes.get(..1024.min(bytes.len())).is_some_and(|h| h.windows(5).any(|w| w == b"%PDF-"))
}

fn load(bytes: &[u8]) -> Result<hayro::hayro_syntax::Pdf, IoError> {
    load_with(bytes, "")
}

/// Reads `bytes`, decrypting with `password` (the empty one opens files that only restrict
/// printing or editing).
fn load_with(bytes: &[u8], password: &str) -> Result<hayro::hayro_syntax::Pdf, IoError> {
    use hayro::hayro_syntax::{DecryptionError, LoadPdfError};
    hayro::hayro_syntax::Pdf::new_with_password(bytes.to_vec(), password).map_err(|e| match e {
        LoadPdfError::Decryption(DecryptionError::PasswordProtected) => IoError::PdfPassword { given: !password.is_empty() },
        LoadPdfError::Decryption(e) => IoError::Unsupported(format!("PDF: can't decrypt this file ({e:?})")),
        e => IoError::Unsupported(format!("PDF: can't read this file ({e:?})")),
    })
}

/// Whether `bytes` are a PDF that needs a password to open.
pub fn needs_password(bytes: &[u8]) -> bool {
    matches!(load(bytes), Err(IoError::PdfPassword { .. }))
}

/// Number of pages.
pub fn page_count(bytes: &[u8]) -> Result<usize, IoError> {
    page_count_with(bytes, "")
}

/// [`page_count`] of a password-protected file.
pub fn page_count_with(bytes: &[u8], password: &str) -> Result<usize, IoError> {
    Ok(load_with(bytes, password)?.pages().len())
}

/// Whether page `page` (0-based) carries Photoshop editing data, so it opens as its document
/// whatever [`PageAs`] says, with no Import PDF options (as in Photoshop).
pub fn has_editing_data(bytes: &[u8], page: usize) -> bool {
    has_editing_data_with(bytes, page, "")
}

/// [`has_editing_data`] of a password-protected file.
pub fn has_editing_data_with(bytes: &[u8], page: usize, password: &str) -> bool {
    load_with(bytes, password).is_ok_and(|pdf| photoshop::has_editing_data(&pdf, page))
}

/// How a page opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageAs {
    /// Text as type layers, vectors as shape layers, images as pixel layers (see [`layers`]).
    Layers,
    /// One pixel layer, as Photoshop's Import PDF does.
    Pixels,
}

/// Which part of the page opens (Photoshop's Import PDF › Crop To).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CropTo {
    /// The smallest rectangle holding everything drawn on the page.
    BoundingBox,
    /// The whole sheet.
    MediaBox,
    /// What viewers show (the default of every PDF reader).
    #[default]
    CropBox,
    /// The crop box plus the bleed.
    BleedBox,
    /// The finished page after trimming.
    TrimBox,
    /// The meaningful content, as its author set it.
    ArtBox,
}

impl CropTo {
    /// Every choice with its command id, in Photoshop's menu order.
    pub const ALL: [(CropTo, &'static str); 6] = [
        (CropTo::BoundingBox, "boundingBox"),
        (CropTo::MediaBox, "mediaBox"),
        (CropTo::CropBox, "cropBox"),
        (CropTo::BleedBox, "bleedBox"),
        (CropTo::TrimBox, "trimBox"),
        (CropTo::ArtBox, "artBox"),
    ];

    /// From its command id (`"cropBox"`, …).
    pub fn from_id(id: &str) -> Option<CropTo> {
        CropTo::ALL.iter().find(|(_, n)| *n == id).map(|(c, _)| *c)
    }

    /// Its command id.
    pub fn id(self) -> &'static str {
        CropTo::ALL.iter().find(|(c, _)| *c == self).map_or("cropBox", |(_, n)| n)
    }
}

/// How to open a page: [`open_page_with`].
#[derive(Clone, Debug, PartialEq)]
pub struct OpenOptions {
    /// 0-based page.
    pub page: usize,
    /// Resolution, pixels per inch.
    pub dpi: f32,
    /// Layers or pixels.
    pub how: PageAs,
    /// Which box of the page.
    pub crop_to: CropTo,
    /// The password of a protected file (empty: none).
    pub password: String,
}

impl Default for OpenOptions {
    fn default() -> Self {
        OpenOptions { page: 0, dpi: DEFAULT_DPI, how: PageAs::Pixels, crop_to: CropTo::CropBox, password: String::new() }
    }
}

/// Where a page lands in the document: PDF user space → document pixels, the clip (user space)
/// and the canvas size.
pub(crate) struct Placement {
    pub transform: hayro::kurbo::Affine,
    pub clip: hayro::kurbo::Rect,
    pub w: u32,
    pub h: u32,
}

/// A page box from the page dictionary (`BleedBox`, …), inside the media box; `None` when the
/// page doesn't set it.
fn page_box(page: &hayro::hayro_syntax::page::Page<'_>, key: &[u8]) -> Option<hayro::kurbo::Rect> {
    use hayro::hayro_interpret::util::RectExt;
    let a = page.raw().get::<hayro::hayro_syntax::object::Array<'_>>(key)?;
    let v: Vec<f64> = a.iter::<f64>().collect();
    let [x0, y0, x1, y1] = <[f64; 4]>::try_from(v).ok()?;
    if ![x0, y0, x1, y1].iter().all(|n| n.is_finite()) {
        return None;
    }
    let r = hayro::kurbo::Rect::new(x0, y0, x1, y1).abs().intersect(page.media_box().to_kurbo());
    (r.width() > 0.0 && r.height() > 0.0).then_some(r)
}

/// The page's `crop_to` box (the bounding box starts from the media box) in user space, and the
/// transform taking it to points with the origin at its top left, the page's rotation applied.
fn frame(page: &hayro::hayro_syntax::page::Page<'_>, crop_to: CropTo) -> (hayro::kurbo::Rect, hayro::kurbo::Affine) {
    use hayro::hayro_interpret::util::{RectExt, TransformExt};
    let crop = page.intersected_crop_box().to_kurbo();
    let mut rect = match crop_to {
        CropTo::CropBox => crop,
        CropTo::MediaBox | CropTo::BoundingBox => page.media_box().to_kurbo(),
        CropTo::BleedBox => page_box(page, b"BleedBox").unwrap_or(crop),
        CropTo::TrimBox => page_box(page, b"TrimBox").unwrap_or(crop),
        CropTo::ArtBox => page_box(page, b"ArtBox").unwrap_or(crop),
    };
    // A zero-area crop box (some generators write one) means the sheet.
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        rect = page.media_box().to_kurbo();
    }
    // The crop box's transform (y down, rotated), moved so the chosen box starts at 0, 0.
    let t = page.initial_transform(true).to_kurbo();
    let r = t.transform_rect_bbox(rect);
    (rect, hayro::kurbo::Affine::translate((-r.x0, -r.y0)) * t)
}

/// Renders `place` as straight-alpha RGBA.
fn render_pixels(page: &hayro::hayro_syntax::page::Page<'_>, place: &Placement) -> Vec<u8> {
    let cache = render::RenderCache::new();
    let settings = hayro::hayro_interpret::InterpreterSettings::default();
    let mut ctx = hayro::vello_cpu::RenderContext::new(place.w.min(65_535) as u16, place.h.min(65_535) as u16);
    render::render_into(page, &cache, &settings, &render::RenderSettings::default(), &mut ctx, place.transform, place.clip);
    ctx.flush();
    let mut pix = hayro::vello_cpu::Pixmap::new(ctx.width(), ctx.height());
    ctx.render_with(
        &mut pix,
        &mut hayro::vello_cpu::Resources::default(),
        hayro::vello_cpu::RasterizerSettings { target_init: hayro::vello_cpu::TargetInit::Clear(hayro::vello_cpu::color::palette::css::TRANSPARENT), ..Default::default() },
    );
    // Premultiplied → straight.
    let mut rgba = pix.data_as_u8_slice().to_vec();
    for q in rgba.chunks_exact_mut(4) {
        let a = u32::from(q[3]);
        if a > 0 && a < 255 {
            for c in &mut q[..3] {
                *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    rgba
}

/// The pixel rectangle `[x0, y0, x1, y1)` of what is drawn in `rgba` (`w` wide); `None` when
/// nothing is.
fn drawn_bounds(rgba: &[u8], w: u32) -> Option<[u32; 4]> {
    let w = w.max(1) as usize;
    let mut b: Option<[usize; 4]> = None;
    for (i, px) in rgba.chunks_exact(4).enumerate() {
        if px[3] != 0 {
            let (x, y) = (i % w, i / w);
            let r = b.get_or_insert([x, y, x + 1, y + 1]);
            *r = [r[0].min(x), r[1].min(y), r[2].max(x + 1), r[3].max(y + 1)];
        }
    }
    b.map(|r| r.map(|v| v as u32))
}

/// A PDF kept open for Import PDF's page list: page sizes and thumbnails.
pub struct Pages {
    pdf: hayro::hayro_syntax::Pdf,
}

impl Pages {
    /// Reads `bytes` once for the whole dialog.
    pub fn open(bytes: &[u8]) -> Result<Pages, IoError> {
        Self::open_with(bytes, "")
    }

    /// [`Pages::open`] for a password-protected file.
    pub fn open_with(bytes: &[u8], password: &str) -> Result<Pages, IoError> {
        Ok(Pages { pdf: load_with(bytes, password)? })
    }

    /// Number of pages.
    pub fn len(&self) -> usize {
        self.pdf.pages().len()
    }

    /// Whether there are no pages.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Page `page`'s `crop_to` box in points (the bounding box is only known once rendered: the
    /// sheet's size is given for it).
    pub fn size_pt(&self, page: usize, crop_to: CropTo) -> Option<(f32, f32)> {
        let pages = self.pdf.pages();
        let p = pages.get(page)?;
        let (clip, t) = frame(p, crop_to);
        let r = t.transform_rect_bbox(clip);
        Some((r.width() as f32, r.height() as f32))
    }

    /// Page `page`'s crop box rendered to fit `max_side` pixels: (width, height, straight RGBA).
    pub fn thumbnail(&self, page: usize, max_side: u32) -> Option<(u32, u32, Vec<u8>)> {
        let pages = self.pdf.pages();
        let p = pages.get(page)?;
        let (clip, t) = frame(p, CropTo::CropBox);
        let r = t.transform_rect_bbox(clip);
        let longest = r.width().max(r.height());
        if !(longest > 0.0 && longest.is_finite()) {
            return None;
        }
        let k = f64::from(max_side.clamp(8, 1024)) / longest;
        let (w, h) = (((r.width() * k) as u32).max(1), ((r.height() * k) as u32).max(1));
        let place = Placement { transform: hayro::kurbo::Affine::scale(k) * t, clip, w, h };
        Some((w, h, render_pixels(p, &place)))
    }
}

/// Opens page `page` (0-based) at `dpi` as Photoshop's Import PDF does by default (pixels; a
/// Photoshop PDF as its document): see [`open_page_with`].
pub fn import_page(name: &str, bytes: &[u8], page: usize, dpi: f32) -> Result<ImportResult, IoError> {
    open_page(name, bytes, page, dpi, PageAs::Pixels)
}

/// Opens page `page` (0-based) of the crop box at `dpi`, as layers or as pixels: see
/// [`open_page_with`].
pub fn open_page(name: &str, bytes: &[u8], page: usize, dpi: f32, how: PageAs) -> Result<ImportResult, IoError> {
    open_page_with(name, bytes, &OpenOptions { page, dpi, how, crop_to: CropTo::CropBox, ..Default::default() })
}

/// Where page `p` lands at `o`'s resolution and box: its placement, the scale (pixels per
/// point) and, when Crop To Bounding Box had to render the page whole, those pixels.
fn placement(p: &hayro::hayro_syntax::page::Page<'_>, o: &OpenOptions, warnings: &mut Vec<String>) -> Result<(Placement, f32, Option<Vec<u8>>), IoError> {
    let (clip, to_points) = frame(p, o.crop_to);
    let size = to_points.transform_rect_bbox(clip);
    let (wpt, hpt) = (size.width() as f32, size.height() as f32);
    if !(wpt > 0.0 && hpt > 0.0 && wpt.is_finite() && hpt.is_finite()) {
        return Err(IoError::Unsupported("PDF page has no size".into()));
    }
    // Scale to `dpi`, then down so the page fits the decode limits and hayro's 16-bit sizes.
    let dpi = if o.dpi.is_finite() { o.dpi } else { DEFAULT_DPI };
    let limits = codecs::Limits::default();
    let mut scale = dpi.clamp(1.0, 2400.0) / 72.0;
    let max_side = (limits.max_width.min(limits.max_height) as f32).min(65_535.0);
    scale = scale.min(max_side / wpt.max(hpt));
    let px = f64::from(wpt * scale) * f64::from(hpt * scale);
    if px > limits.max_pixels as f64 {
        scale *= (limits.max_pixels as f64 / px).sqrt() as f32;
    }
    if (scale * 72.0 - dpi).abs() > 0.5 {
        warnings.push(format!("PDF page opened at {:.0} ppi (the most that fits), not {dpi:.0}", scale * 72.0));
    }
    // Truncated, as hayro sizes a page it renders.
    let mut place = Placement {
        transform: hayro::kurbo::Affine::scale(f64::from(scale)) * to_points,
        clip,
        w: ((wpt * scale) as u32).max(1),
        h: ((hpt * scale) as u32).max(1),
    };
    let mut rendered = None;
    if o.crop_to == CropTo::BoundingBox {
        let rgba = render_pixels(p, &place);
        match drawn_bounds(&rgba, place.w) {
            Some([x0, y0, x1, y1]) => {
                place.transform = hayro::kurbo::Affine::translate((-f64::from(x0), -f64::from(y0))) * place.transform;
                if (x0, y0, x1, y1) == (0, 0, place.w, place.h) {
                    rendered = Some(rgba);
                }
                (place.w, place.h) = (x1 - x0, y1 - y0);
            }
            None => warnings.push("PDF page is empty: Crop To Bounding Box kept the whole sheet".into()),
        }
    }
    Ok((place, scale, rendered))
}

/// Opens a page as a document named `name`, as Import PDF does: the chosen box at a resolution,
/// as layers or as pixels. A page with the Reference's editing data opens as that document instead.
pub fn open_page_with(name: &str, bytes: &[u8], o: &OpenOptions) -> Result<ImportResult, IoError> {
    let pdf = load_with(bytes, &o.password)?;
    let pages = pdf.pages();
    let n = pages.len();
    let p = pages.get(o.page).ok_or_else(|| IoError::Unsupported(format!("PDF has {n} pages; there is no page {}", o.page.saturating_add(1))))?;
    let mut warnings = Vec::new();
    match photoshop::read(name, &pdf, p) {
        Some(Ok(r)) => return Ok(r),
        Some(Err(why)) => warnings.push(format!("PDF: the Photoshop editing data can't be used ({why}); opened the page instead")),
        None => {}
    }
    if n > 1 {
        warnings.push(format!("PDF has {n} pages; opened page {}", o.page + 1));
    }
    let (place, scale, rendered) = placement(p, o, &mut warnings)?;
    if o.how == PageAs::Layers {
        let pdf_layers = ocg::page_layers(&pdf, p);
        if !pdf_layers.is_empty()
            && let Some((document, more)) = layers::import_pdf_layers(name, bytes, &pdf, o.page, &pdf_layers, &place, scale * 72.0)
        {
            warnings.extend(more);
            return Ok(ImportResult { document, warnings, selected: Vec::new() });
        }
        let (document, more) = layers::import_layers(name, p, &place, scale * 72.0);
        warnings.extend(more);
        return Ok(ImportResult { document, warnings, selected: Vec::new() });
    }
    let rgba = match rendered {
        Some(r) => r,
        None => render_pixels(p, &place),
    };
    let img = codecs::Image::from_u8(place.w, place.h, ChannelLayout::Rgba, rgba).map_err(|e| IoError::Unsupported(e.to_string()))?;
    let mut r = crate::flat::image_to_document(name, &img)?;
    r.document.resolution_dpi = scale * 72.0;
    r.warnings.extend(warnings);
    Ok(r)
}

/// A minimal PDF with one page per JPEG image: (width px, height px, dpi, JPEG bytes). Pages are
/// sized so the image prints at its resolution.
pub fn raster_pdf(pages: &[(u32, u32, f32, Vec<u8>)]) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets: Vec<usize> = Vec::new();
    let mut obj = |out: &mut Vec<u8>, body: &[u8]| {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", offsets.len()).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    // Objects: 1 catalog, 2 page tree, then per page: page, contents, image.
    let kids: Vec<String> = (0..pages.len()).map(|i| format!("{} 0 R", 3 + i * 3)).collect();
    obj(&mut out, b"<< /Type /Catalog /Pages 2 0 R >>");
    obj(&mut out, format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pages.len()).as_bytes());
    for (i, (w, h, dpi, jpeg)) in pages.iter().enumerate() {
        let k = 72.0 / f64::from(dpi.max(1.0));
        let (pw, ph) = (f64::from(*w) * k, f64::from(*h) * k);
        let (contents, image) = (4 + i * 3, 5 + i * 3);
        obj(
            &mut out,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw:.3} {ph:.3}] /Resources << /XObject << /Im0 {image} 0 R >> >> /Contents {contents} 0 R >>"
            )
            .as_bytes(),
        );
        let stream = format!("q {pw:.3} 0 0 {ph:.3} 0 0 cm /Im0 Do Q");
        obj(&mut out, format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()).as_bytes());
        let mut img = format!(
            "<< /Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>\nstream\n",
            jpeg.len()
        )
        .into_bytes();
        img.extend_from_slice(jpeg);
        img.extend_from_slice(b"\nendstream");
        obj(&mut out, &img);
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes());
    for o in &offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", offsets.len() + 1).as_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> photosplice_doc::Document {
        let mut d = photosplice_doc::Document::with_background(
            "card",
            photosplice_geom::Size::new(144, 72),
            photosplice_color::ColorMode::Rgb,
            photosplice_color::SampleType::U8,
            photosplice_doc::Color::WHITE,
        );
        d.resolution_dpi = 72.0;
        if let Some(s) = d.layers.first_mut().and_then(|l| l.surface_mut()) {
            s.fill_rect(photosplice_geom::Rect::new(0, 0, 72, 72), &[1.0, 0.0, 0.0, 1.0]);
        }
        d
    }

    #[test]
    fn saved_pdfs_open_again_with_their_pixels() {
        let r = export::export_layered(&doc(), false).unwrap();
        assert!(is_pdf(&r.bytes));
        assert_eq!(page_count(&r.bytes).unwrap(), 1);
        // 2 in × 1 in page; at 72 ppi it comes back 144 × 72 with red on the left.
        let back = open_page("card.pdf", &r.bytes, 0, 72.0, PageAs::Pixels).unwrap().document;
        assert_eq!((back.size.width, back.size.height), (144, 72));
        let s = back.layers.first().and_then(|l| l.surface()).unwrap();
        let (l, rt) = (s.rgba(20, 36), s.rgba(120, 36));
        assert!(l[0] > 0.9 && l[1] < 0.15, "{l:?}");
        assert!(rt[0] > 0.9 && rt[1] > 0.9, "{rt:?}");
        // At 300 ppi the same page is 600 × 300.
        let big = open_page("card.pdf", &r.bytes, 0, 300.0, PageAs::Pixels).unwrap();
        assert_eq!((big.document.size.width, big.document.size.height), (600, 300));
        assert!((big.document.resolution_dpi - 300.0).abs() < 0.5);
    }

    #[test]
    fn bad_input_is_an_error() {
        assert!(!is_pdf(b"hello"));
        assert!(import_page("x.pdf", b"%PDF-1.4 garbage", 0, 72.0).is_err());
        let r = export::export_layered(&doc(), false).unwrap();
        assert!(import_page("x.pdf", &r.bytes, 3, 72.0).is_err(), "no such page");
    }
}
