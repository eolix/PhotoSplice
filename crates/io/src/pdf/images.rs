//! Import PDF › Images: the raster images a PDF's pages draw, each opened as a document of its
//! own at its own pixel size (not the page's resolution). An image drawn on several pages, or
//! several times, is listed once, on the first page that draws it.

use std::collections::HashSet;

use hayro::hayro_interpret::font::GlyphRun;
use hayro::hayro_interpret::{
    BlendMode, CacheKey, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageData, ImageDrawProps, InterpreterSettings, LumaData, SoftMask,
    interpret_page,
};
use hayro::kurbo::{self, BezPath};
use photosplice_color::{ColorMode, PixelFormat, SampleType};
use photosplice_doc::{Document, Layer, LayerContent};
use photosplice_geom::{Rect, Size};
use photosplice_raster::Surface;

use super::render::RenderCache;
use crate::{ImportResult, IoError};

/// At most this many images are listed.
const MAX_IMAGES: usize = 10_000;

/// One image of a PDF.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PdfImage {
    /// The first page (0-based) that draws it.
    pub page: usize,
    pub width: u32,
    pub height: u32,
}

/// What a walk over the pages collects: the images in order, and the pixels of the one wanted.
pub(super) struct Collect {
    page: usize,
    seen: HashSet<u128>,
    pub(super) list: Vec<PdfImage>,
    want: Option<usize>,
    /// The decoder's size hint for the wanted image (a thumbnail's).
    hint: Option<(u32, u32)>,
    got: Option<(ImageData, Option<LumaData>)>,
}

impl<'a> Device<'a> for Collect {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_glyph_run(&mut self, _: &GlyphRun<'_, 'a>, _: DrawProps<'a>, _: &DrawMode) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}

    fn draw_image(&mut self, image: Image<'a, '_>, _: ImageDrawProps<'a>) {
        // Stencil masks are a colour painted through a shape, not an image of their own.
        let Image::Raster(r) = image else { return };
        if self.list.len() >= MAX_IMAGES || !self.seen.insert(r.cache_key()) {
            return;
        }
        if self.want == Some(self.list.len()) {
            let mut got = None;
            r.with_rgba(|data, alpha| got = Some((data, alpha)), self.hint);
            self.got = got;
        }
        self.list.push(PdfImage { page: self.page, width: r.width(), height: r.height() });
    }
}

/// Walks the pages until image `want` is decoded (or all of them, to list them).
fn walk(bytes: &[u8], password: &str, want: Option<usize>, hint: Option<(u32, u32)>) -> Result<Collect, IoError> {
    Ok(walk_pdf(&super::load_with(bytes, password)?, want, hint))
}

pub(super) fn walk_pdf(pdf: &hayro::hayro_syntax::Pdf, want: Option<usize>, hint: Option<(u32, u32)>) -> Collect {
    let cache = RenderCache::new();
    let mut c = Collect { page: 0, seen: HashSet::new(), list: Vec::new(), want, hint, got: None };
    for (i, page) in pdf.pages().iter().enumerate() {
        c.page = i;
        let (w, h) = page.render_dimensions();
        let bbox = kurbo::Rect::new(0.0, 0.0, f64::from(w.max(1.0)), f64::from(h.max(1.0)));
        let mut ctx = Context::new(kurbo::Affine::IDENTITY, bbox, &cache.interpreter_cache, page.xref(), InterpreterSettings::default());
        interpret_page(page, &mut ctx, &mut c);
        if c.got.is_some() || c.list.len() >= MAX_IMAGES {
            break;
        }
    }
    c
}

/// The images `bytes` draw, in the order its pages first draw them (`password`: a protected
/// file's).
pub fn list_images(bytes: &[u8], password: &str) -> Result<Vec<PdfImage>, IoError> {
    Ok(walk(bytes, password, None, None)?.list)
}

/// Straight RGBA (or grey and alpha) of a decoded image, its alpha resampled to its size when the
/// mask has another.
fn interleave(data: &ImageData, alpha: Option<&LumaData>) -> (u32, u32, bool, Vec<u8>) {
    let (w, h) = (data.width(), data.height());
    let (gray, color, n): (bool, &[u8], usize) = match data {
        ImageData::Rgb(d) => (false, &d.data, 3),
        ImageData::Luma(d) => (true, &d.data, 1),
    };
    let a_at = |x: u32, y: u32| -> u8 {
        match alpha {
            Some(a) if a.width > 0 && a.height > 0 => {
                let (ax, ay) = (u64::from(x) * u64::from(a.width) / u64::from(w.max(1)), u64::from(y) * u64::from(a.height) / u64::from(h.max(1)));
                a.data.get((ay * u64::from(a.width) + ax) as usize).copied().unwrap_or(255)
            }
            _ => 255,
        }
    };
    let mut out = Vec::with_capacity((w as usize) * (h as usize) * (n + 1));
    for y in 0..h {
        for x in 0..w {
            let i = (y as usize * w as usize + x as usize) * n;
            out.extend_from_slice(color.get(i..i + n).unwrap_or(&[0, 0, 0][..n]));
            out.push(a_at(x, y));
        }
    }
    (w, h, gray, out)
}

/// Image `index` of [`list_images`] as a document of its own: its pixels at its own size (RGB, or
/// Grayscale for a grey image), with its transparency, at 72 ppi.
pub fn open_image(name: &str, bytes: &[u8], password: &str, index: usize) -> Result<ImportResult, IoError> {
    let c = walk(bytes, password, Some(index), None)?;
    let n = c.list.len();
    let (data, alpha) = c.got.ok_or_else(|| IoError::Unsupported(format!("PDF has {n} images; there is no image {}", index.saturating_add(1))))?;
    let (w, h, gray, px) = interleave(&data, alpha.as_ref());
    if w == 0 || h == 0 {
        return Err(IoError::Unsupported("PDF: the image is empty".into()));
    }
    let (mode, fmt) = if gray { (ColorMode::Grayscale, PixelFormat::GRAYA8) } else { (ColorMode::Rgb, PixelFormat::RGBA8) };
    let mut doc = Document::new(name, Size::new(w, h), mode, SampleType::U8);
    doc.resolution_dpi = 72.0;
    let surface = Surface::from_interleaved(fmt, Rect::new(0, 0, w as i32, h as i32), &px);
    doc.layers.push(Layer::new("Layer 1", LayerContent::Raster(surface)));
    Ok(ImportResult { document: doc, warnings: Vec::new(), selected: Vec::new(), source_read_only: false, preview_only: false })
}

/// A thumbnail of image `index` fitting `max` × `max`: its size and straight RGBA.
pub fn image_thumbnail(bytes: &[u8], password: &str, index: usize, max: u32) -> Option<(u32, u32, Vec<u8>)> {
    thumbnail_of(walk(bytes, password, Some(index), Some((max, max))).ok()?, max)
}

pub(super) fn thumbnail_of(c: Collect, max: u32) -> Option<(u32, u32, Vec<u8>)> {
    let (data, alpha) = c.got?;
    let (w, h, gray, px) = interleave(&data, alpha.as_ref());
    let k = (f64::from(max.max(1)) / f64::from(w.max(h).max(1))).min(1.0);
    let (tw, th) = (((f64::from(w) * k).round() as u32).max(1), ((f64::from(h) * k).round() as u32).max(1));
    let n = if gray { 2 } else { 4 };
    let mut out = Vec::with_capacity((tw * th * 4) as usize);
    for y in 0..th {
        for x in 0..tw {
            let (sx, sy) = (u64::from(x) * u64::from(w) / u64::from(tw), u64::from(y) * u64::from(h) / u64::from(th));
            let i = (sy * u64::from(w) + sx) as usize * n;
            match px.get(i..i + n) {
                Some(&[g, a]) => out.extend_from_slice(&[g, g, g, a]),
                Some(p) if p.len() == 4 => out.extend_from_slice(p),
                _ => out.extend_from_slice(&[0, 0, 0, 0]),
            }
        }
    }
    Some((tw, th, out))
}
