//! Every page of a PDF in one document: each page an artboard
//! named "Page N" on white (pages are paper), laid out left to right with a gap and wrapped into
//! rows, its content as editable layers (or pixels). Each page is built at its own size, then
//! its layers are moved to where its board is.

use photosplice_color::{ColorMode, SampleType};
use photosplice_doc::vector::{Path, Subpath, VectorMask};
use photosplice_doc::{Artboard, ArtboardBackground, Document, Layer, LayerContent};
use photosplice_geom::{Affine, Rect, Size};
use photosplice_raster::Surface;

use super::{OpenOptions, PageAs};
use crate::{ImportResult, IoError};

/// Gaps between pages and between rows, and the widest a row may get, at 72 ppi.
const PAGE_GAP: f32 = 30.0;
const ROW_GAP: f32 = 120.0;
const MAX_ROW: f32 = 8192.0;

/// Opens `pages` (0-based) of the PDF as one document of artboards, at `o`'s resolution, box and
/// way (layers or pixels). One page opens as [`super::open_page_with`] does (no board), and so
/// does a Photoshop PDF (its editing data is one document).
pub fn open_pages(name: &str, bytes: &[u8], pages: &[usize], o: &OpenOptions) -> Result<ImportResult, IoError> {
    let first = *pages.first().ok_or_else(|| IoError::Unsupported("PDF: no pages to open".into()))?;
    if pages.len() == 1 || super::has_editing_data_with(bytes, first, &o.password) {
        return super::open_page_with(name, bytes, &OpenOptions { page: first, ..o.clone() });
    }
    let mut boards = Vec::new();
    let mut warnings = Vec::new();
    let mut dpi = o.dpi;
    for &p in pages {
        let r = super::open_page_with(name, bytes, &OpenOptions { page: p, ..o.clone() })?;
        warnings.extend(r.warnings.into_iter().filter(|w| !w.contains("pages; opened page")));
        dpi = r.document.resolution_dpi;
        boards.push((p, r.document));
    }
    // Rows: pages left to right, a new row once one would pass the widest row.
    let k = dpi / 72.0;
    let (gap, row_gap, max_row) = ((PAGE_GAP * k).round() as i32, (ROW_GAP * k).round() as i32, (MAX_ROW * k).round() as i32);
    let mut spots = Vec::with_capacity(boards.len());
    let (mut x, mut y, mut row_h, mut width) = (0i32, 0i32, 0i32, 0i32);
    for (_, d) in &boards {
        let (w, h) = (d.size.width as i32, d.size.height as i32);
        if x > 0 && x.saturating_add(w) > max_row {
            x = 0;
            y = y.saturating_add(row_h).saturating_add(row_gap);
            row_h = 0;
        }
        spots.push((x, y));
        width = width.max(x.saturating_add(w));
        row_h = row_h.max(h);
        x = x.saturating_add(w).saturating_add(gap);
    }
    let height = y.saturating_add(row_h);
    let limits = photosplice_codecs::Limits::default();
    if width as u64 > u64::from(limits.max_width) || height as u64 > u64::from(limits.max_height) || (width as u64) * (height as u64) > limits.max_pixels {
        return Err(IoError::Unsupported(format!("PDF: {} pages at {dpi:.0} ppi make a {width} × {height} document, more than fits; open fewer pages or at a lower resolution", pages.len())));
    }
    let mut doc = Document::new(name, Size::new(width.max(1) as u32, height.max(1) as u32), ColorMode::Rgb, SampleType::U8);
    doc.resolution_dpi = dpi;
    for ((p, page), (x, y)) in boards.into_iter().zip(spots) {
        let rect = Rect::new(x, y, x + page.size.width as i32, y + page.size.height as i32);
        let mut layers = page.layers;
        for l in &mut layers {
            shift_layer(l, x, y, rect);
        }
        // A page opened as pixels is its one layer, named after the page.
        if o.how == PageAs::Pixels {
            for l in &mut layers {
                l.name = format!("Page {}", p + 1);
            }
        }
        let mut board = Layer::group(format!("Page {}", p + 1), layers);
        if let LayerContent::Group(g) = &mut board.content {
            let mut ab = Artboard::new(rect);
            ab.background = ArtboardBackground::White;
            g.artboard = Some(ab);
        }
        doc.layers.push(board);
    }
    let n = super::page_count_with(bytes, &o.password).unwrap_or(pages.len());
    if n > pages.len() {
        warnings.push(format!("PDF has {n} pages; opened {}", pages.len()));
    }
    warnings.sort();
    warnings.dedup();
    Ok(ImportResult { document: doc, warnings, selected: Vec::new() })
}

/// `s` with its pixels moved by (`dx`, `dy`).
fn shift_surface(s: &Surface, dx: i32, dy: i32) -> Surface {
    let mut out = Surface::with_default(s.format(), &s.default_pixel());
    let r = s.content_bounds();
    if !r.is_empty() {
        let mut v = Vec::new();
        s.read_region_into(r, &mut v);
        out.write_region(r.translate(dx, dy), &v);
    }
    out
}

/// Moves a layer the importer made by (`dx`, `dy`): its pixels, paths, type and masks. A fill
/// layer that covered its page gets the page (`page`, already moved) as its vector mask, so it
/// keeps to its board and its gradient keeps its frame.
fn shift_layer(l: &mut Layer, dx: i32, dy: i32, page: Rect) {
    let a = Affine::translate(f64::from(dx), f64::from(dy));
    if let Some(m) = &mut l.mask {
        m.surface = shift_surface(&m.surface, dx, dy);
    }
    if let Some(vm) = &mut l.vector_mask {
        vm.path = vm.path.transform(&a);
    }
    match &mut l.content {
        LayerContent::Raster(s) => *s = shift_surface(s, dx, dy),
        LayerContent::Shape(sh) => {
            sh.path = sh.path.transform(&a);
            sh.cache = sh.cache.as_ref().map(|c| shift_surface(c, dx, dy));
        }
        LayerContent::Text(t) => {
            t.transform = a.mul(&t.transform);
            t.cache = t.cache.as_ref().map(|c| shift_surface(c, dx, dy));
        }
        LayerContent::Fill(_) if l.vector_mask.is_none() => {
            let (x0, y0, x1, y1) = (f64::from(page.x0), f64::from(page.y0), f64::from(page.x1), f64::from(page.y1));
            l.vector_mask = Some(VectorMask::new(Path::new(vec![Subpath::polygon(&[(x0, y0), (x1, y0), (x1, y1), (x0, y1)])])));
        }
        LayerContent::Group(g) => {
            if let Some(ab) = &mut g.artboard {
                ab.rect = ab.rect.translate(dx, dy);
            }
            for c in &mut g.children {
                shift_layer(c, dx, dy, page);
            }
        }
        _ => {}
    }
}
