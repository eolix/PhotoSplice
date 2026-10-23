//! A document as a Photoshop PDF: one page drawn layer by layer as PDF content (krilla, MIT OR
//! Apache-2.0), plus Photoshop's editing data ([`super::photoshop`]), with which Photoshop and
//! PhotoSplice reopen it with every layer as it was.
//!
//! Drawn natively, so the PDF stays vector and searchable: shape layers (solid fills and centred
//! strokes) as paths, type layers as real text in their fonts (embedded and subset, with the text
//! copyable), pixel layers as images, groups as transparency groups, with each layer's opacity
//! and blend mode. A layer PDF has no equivalent for (effects, masks, gradient or pattern fills,
//! warped or vertical type, clipping groups, smart objects, fill layers) is composited on its own
//! and placed as an image at its place in the stack. Adjustment layers, and blend modes PDF lacks
//! (Linear Burn, Vivid Light, …), work on what is beneath them, so everything up to the topmost
//! one is flattened into one image first.

use krilla::geom::{Path as KPath, PathBuilder, Point as KPoint, Size as KSize, Transform};
use krilla::num::NormalizedF32;
use krilla::page::PageSettings;
use krilla::paint::{Fill as KFill, FillRule as KFillRule, LineCap as KCap, LineJoin as KJoin, Stroke as KStroke, StrokeDash};
use krilla::surface::Surface as KSurface;
use krilla::text::{Font, GlyphId, KrillaGlyph};
use photosplice_color::BlendMode;
use photosplice_doc::vector::{FillRule, LineCap, LineJoin, Path, PathOp, ShapeLayer, StrokeAlign};
use photosplice_doc::{Document, Fill, Layer, LayerContent, TextLayer};

use crate::{ExportResult, IoError};

/// Save Adobe PDF's Compatibility: the PDF version written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Compatibility {
    Pdf14,
    Pdf15,
    Pdf16,
    #[default]
    Pdf17,
    Pdf20,
}

impl Compatibility {
    /// Every version: (it, its id in command params, its label in Save Adobe PDF).
    pub const ALL: [(Compatibility, &'static str, &'static str); 5] = [
        (Compatibility::Pdf14, "1.4", "Acrobat 5 (PDF 1.4)"),
        (Compatibility::Pdf15, "1.5", "Acrobat 6 (PDF 1.5)"),
        (Compatibility::Pdf16, "1.6", "Acrobat 7 (PDF 1.6)"),
        (Compatibility::Pdf17, "1.7", "Acrobat 8 (PDF 1.7)"),
        (Compatibility::Pdf20, "2.0", "PDF 2.0"),
    ];

    pub fn from_id(id: &str) -> Option<Compatibility> {
        Self::ALL.iter().find(|(_, i, _)| *i == id).map(|(c, _, _)| *c)
    }

    pub fn id(self) -> &'static str {
        Self::ALL.iter().find(|(c, _, _)| *c == self).map_or("1.7", |(_, i, _)| i)
    }

    fn krilla(self) -> krilla::configure::PdfVersion {
        use krilla::configure::PdfVersion as V;
        match self {
            Compatibility::Pdf14 => V::Pdf14,
            Compatibility::Pdf15 => V::Pdf15,
            Compatibility::Pdf16 => V::Pdf16,
            Compatibility::Pdf17 => V::Pdf17,
            Compatibility::Pdf20 => V::Pdf20,
        }
    }
}

/// Save Adobe PDF's options beyond the editing data.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdfOptions {
    pub compatibility: Compatibility,
    /// Image Compression: JPEG at this quality (1–100) for images without transparency; `None`
    /// is ZIP (lossless), and images with transparency are always ZIP.
    pub jpeg_quality: Option<u8>,
    /// Downsampling: images in a document above `.1` ppi are resampled to `.0` ppi.
    pub downsample: Option<(f32, f32)>,
    /// Each visible top-level layer (or group) as a PDF layer of its name, which Acrobat and
    /// Illustrator show and can hide (Illustrator's "Create Acrobat Layers from Top-Level
    /// Layers"). On by default.
    pub pdf_layers: bool,
    /// Standard: the PDF/X standard the file conforms to, which fixes its compatibility (see
    /// [`super::pdfx`]).
    pub standard: super::pdfx::Standard,
}

impl Default for PdfOptions {
    fn default() -> Self {
        PdfOptions { compatibility: Compatibility::default(), jpeg_quality: None, downsample: None, pdf_layers: true, standard: super::pdfx::Standard::None }
    }
}

/// Saves `doc` as a one-page layered PDF; `editing` adds the Photoshop editing data (*Preserve
/// Photoshop Editing Capabilities*).
pub fn export_layered(doc: &Document, editing: bool) -> Result<ExportResult, IoError> {
    export_layered_with(doc, editing, &PdfOptions::default())
}

/// [`export_layered`] with Save Adobe PDF's options.
pub fn export_layered_with(doc: &Document, editing: bool, opts: &PdfOptions) -> Result<ExportResult, IoError> {
    use super::pdfx::Standard;
    let mut warnings = Vec::new();
    let standard = opts.standard;
    let mut opts = *opts;
    opts.compatibility = standard.compatibility(opts.compatibility);
    // The 2003 standards allow no transparency, nor PDF layers: the page is the composite on
    // paper white, one image (the layers stay in the editing data).
    let flat = standard.flattened().then(|| flattened(doc, standard));
    if flat.is_some() {
        opts.pdf_layers = false;
        warnings.push(format!("PDF: {} allows no transparency; the page is saved as one image", standard_label(standard)));
    }
    let page = flat.as_ref().unwrap_or(doc);
    let mut pdf = new_pdf(&opts, Some(doc))?;
    let marks = draw_page(&mut pdf, page, &opts, &mut warnings)?;
    if flat.is_some() {
        warnings.retain(|w| !w.contains("saved as images") && !w.contains("saved as one image (its layers"));
    }
    let mut bytes = finish(pdf, &doc.name)?;
    if !marks.is_empty() {
        match super::ocg::add_layers(bytes.clone(), &marks) {
            Ok(b) => bytes = b,
            Err(e) => warnings.push(format!("{e}; saved without PDF layers")),
        }
    }
    if editing {
        match super::photoshop::attach(bytes.clone(), doc, &mut warnings) {
            Ok(b) => bytes = b,
            Err(e) => warnings.push(format!("{e}; saved without Photoshop editing data")),
        }
    }
    if standard != Standard::None {
        bytes = super::pdfx::conform(bytes, doc, standard)?;
    }
    Ok(ExportResult { bytes, warnings })
}

fn standard_label(s: super::pdfx::Standard) -> &'static str {
    super::pdfx::Standard::ALL.iter().find(|(x, _, _)| *x == s).map_or("PDF/X", |(_, _, l)| l)
}

/// `doc`'s composite on paper white as a one-layer document, for a standard without
/// transparency: in CMYK (device CMYK, the output profile's) for PDF/X-1a, else in its own mode.
fn flattened(doc: &Document, standard: super::pdfx::Standard) -> Document {
    use photosplice_color::{ColorMode, PixelFormat, SampleType};
    let mut tmp = doc.clone();
    tmp.layers.insert(0, Layer::new("Paper", LayerContent::Fill(Fill::Solid(photosplice_color::Color::rgb(1.0, 1.0, 1.0)))));
    let cmyk = standard == super::pdfx::Standard::X1a2003;
    let (mode, fmt) = match (cmyk, super::ink::Ink::of(doc)) {
        (true, _) => (ColorMode::Cmyk, PixelFormat::CMYKA8),
        (false, Some(ink)) => (doc.mode, ink.format()),
        // Lab, Indexed…: RGB.
        (false, None) => (ColorMode::Rgb, PixelFormat::RGBA8),
    };
    let surface = photosplice_compose::flatten_to_surface(&tmp, fmt, None);
    let mut out = Document::new(&doc.name, doc.size, mode, SampleType::U8);
    out.resolution_dpi = doc.resolution_dpi;
    // Device CMYK for X-1a (no ICC-based colour); the document's own profile otherwise.
    out.icc_profile = if cmyk { None } else { doc.icc_profile.clone().filter(|_| mode == doc.mode) };
    out.layers.push(Layer::new("Composite", LayerContent::Raster(surface)));
    out
}

/// [`export_layered_with`] protected by `security` (the save options' Security) when it sets a
/// password. Protection that fails is an error: a file meant to be locked is never written open.
pub fn export_secured(doc: &Document, editing: bool, opts: &PdfOptions, security: Option<&super::security::Security>) -> Result<ExportResult, IoError> {
    let sec = security.filter(|s| !s.is_none());
    if sec.is_some() && opts.standard != super::pdfx::Standard::None {
        return Err(IoError::Unsupported(format!("PDF: a {} file can't have a password", standard_label(opts.standard))));
    }
    let mut r = export_layered_with(doc, editing, opts)?;
    if let Some(sec) = sec {
        r.bytes = super::security::encrypt(&r.bytes, sec, opts.compatibility)?;
    }
    Ok(r)
}

/// A PDF of one page per document, each drawn as [`export_layered`] draws its page (Artboards to
/// PDF, a page per board). No editing data: a Photoshop PDF holds one document.
pub fn export_pages(docs: &[Document], title: &str) -> Result<ExportResult, IoError> {
    if docs.is_empty() {
        return Err(IoError::Unsupported("PDF: no pages to write".into()));
    }
    let mut warnings = Vec::new();
    // One page per document: no PDF layers (they would be per page, with each page's names).
    let opts = PdfOptions { pdf_layers: false, ..PdfOptions::default() };
    let mut pdf = new_pdf(&opts, None)?;
    for doc in docs {
        draw_page(&mut pdf, doc, &opts, &mut warnings)?;
    }
    warnings.dedup();
    Ok(ExportResult { bytes: finish(pdf, title)?, warnings })
}

/// A krilla document for `opts`. Under a PDF/X standard with ICC-based colour (X-3, X-4), no
/// device colour (RGB and grey as ICC colour, CMYK in `doc`'s printing condition), and no XMP
/// of krilla's: [`super::pdfx::conform`] writes the standard's.
fn new_pdf(opts: &PdfOptions, doc: Option<&Document>) -> Result<krilla::Document, IoError> {
    let configuration = krilla::configure::ConfigurationBuilder::new()
        .with_version(opts.compatibility.krilla())
        .finish()
        .map_err(|e| IoError::Unsupported(format!("PDF: can't write PDF {} ({e:?})", opts.compatibility.id())))?;
    let mut settings = krilla::SerializeSettings { configuration, ..Default::default() };
    if opts.standard != super::pdfx::Standard::None {
        settings.xmp_metadata = false;
        if opts.standard.icc_colour() {
            settings.no_device_cs = true;
            settings.cmyk_profile = doc.and_then(|d| krilla::icc::ICCProfile::new(&super::pdfx::output_profile(d).0));
        }
    }
    Ok(krilla::Document::new_with(settings))
}

fn finish(mut pdf: krilla::Document, title: &str) -> Result<Vec<u8>, IoError> {
    let mut meta = krilla::metadata::Metadata::new().creator("PhotoSplice".into());
    if !title.is_empty() {
        meta = meta.title(title.to_string());
    }
    pdf.set_metadata(meta);
    pdf.finish().map_err(|e| IoError::Unsupported(format!("PDF: can't write the file ({e:?})")))
}

/// `doc` as one page of `pdf`, sized so it prints at its resolution.
/// Returns the PDF layers the page marked (see [`Writer::top_level`]).
fn draw_page(pdf: &mut krilla::Document, doc: &Document, opts: &PdfOptions, warnings: &mut Vec<String>) -> Result<Vec<super::ocg::Mark>, IoError> {
    let dpi = if doc.resolution_dpi > 0.0 && doc.resolution_dpi.is_finite() { doc.resolution_dpi } else { 72.0 };
    let k = 72.0 / dpi;
    let (w, h) = (doc.size.width.max(1) as f32, doc.size.height.max(1) as f32);
    let settings = PageSettings::from_wh(w * k, h * k).ok_or_else(|| IoError::Unsupported("PDF: the page has no size".into()))?;
    let mut page = pdf.start_page_with(settings);
    let mut surface = page.surface();
    surface.push_transform(&Transform::from_scale(k, k));
    let ink = super::ink::Ink::of(doc);
    let mut w = Writer { doc, rastered: 0, fonts: Vec::new(), ink: ink.clone().unwrap_or(super::ink::Ink::Rgb), opts: *opts, dpi, marks: Vec::new() };
    if ink.is_some() {
        // Everything up to the topmost layer that works on what is beneath it, as one image.
        let cut = doc.layers.iter().rposition(|l| l.visible && needs_backdrop(l));
        let rest = match cut {
            Some(i) => {
                w.image_of(&mut surface, &doc.layers[..=i]);
                &doc.layers[i + 1..]
            }
            None => &doc.layers[..],
        };
        if opts.pdf_layers {
            w.top_level(&mut surface, rest);
        } else {
            w.layers(&mut surface, rest);
        }
    } else {
        // Lab, Indexed, Duotone…: the composite, converted for display.
        w.image_of(&mut surface, &doc.layers);
        warnings.push(format!("PDF: a {:?} document is saved as one image (its layers are in the editing data)", doc.mode));
    }
    surface.pop();
    surface.finish();
    page.finish();
    let marks = std::mem::take(&mut w.marks);
    if w.rastered > 0 {
        warnings.push(format!("PDF: {} layer(s) with no PDF equivalent (effects, masks, gradients, adjustments, …) saved as images", w.rastered));
    }
    Ok(marks)
}

struct Writer<'d> {
    doc: &'d Document,
    rastered: usize,
    /// The colour space the page paints in (the document's own: RGB, Grayscale or CMYK).
    ink: super::ink::Ink,
    opts: PdfOptions,
    /// The document's resolution.
    dpi: f32,
    /// The PDF layers marked so far, in drawing order.
    marks: Vec<super::ocg::Mark>,
    /// Fonts already handed to krilla, by the face's data pointer and index.
    fonts: Vec<((usize, u32), Font)>,
}

impl Writer<'_> {
    /// [`Writer::layers`] with each visible top-level layer (and the layers clipped to it) in a
    /// marked-content sequence of its own, which [`super::ocg::add_layers`] makes a PDF layer.
    fn top_level(&mut self, s: &mut KSurface<'_>, layers: &[Layer]) {
        use krilla::tagging::{Artifact, ArtifactType, ContentTag};
        let mut i = 0;
        while let Some(l) = layers.get(i) {
            let mut j = i + 1;
            while layers.get(j).is_some_and(|c| c.clipped) && !l.clipped {
                j += 1;
            }
            if l.visible {
                // A bare `/Artifact BMC` (no properties): the marker the PDF layer replaces.
                s.start_tagged(ContentTag::Artifact(Artifact::with_kind(ArtifactType::Other)));
                self.layers(s, layers.get(i..j).unwrap_or_default());
                s.end_tagged();
                self.marks.push(super::ocg::Mark { name: l.name.clone(), locked: l.locks.all });
            }
            i = j;
        }
    }

    fn layers(&mut self, s: &mut KSurface<'_>, layers: &[Layer]) {
        let mut i = 0;
        while i < layers.len() {
            let l = &layers[i];
            // A clipping group (a layer and those clipped to it) is drawn as one.
            let mut j = i + 1;
            while j < layers.len() && layers[j].clipped && !l.clipped {
                j += 1;
            }
            if l.visible {
                if j > i + 1 && layers[i + 1..j].iter().any(|c| c.visible) {
                    self.rastered += 1;
                    self.with_blend(s, l, |w, s| w.image_of(s, &unblended(&layers[i..j])));
                } else {
                    self.layer(s, l);
                }
            }
            i = j.max(i + 1);
        }
    }

    fn layer(&mut self, s: &mut KSurface<'_>, l: &Layer) {
        if !native(l) {
            self.rastered += 1;
            self.with_blend(s, l, |w, s| w.image_of(s, &unblended(std::slice::from_ref(l))));
            return;
        }
        // A hard vector mask clips the layer.
        let clip = l.vector_mask.as_ref().filter(|m| m.enabled).and_then(|m| {
            let rule = match m.path.fill_rule {
                FillRule::NonZero => KFillRule::NonZero,
                FillRule::EvenOdd => KFillRule::EvenOdd,
            };
            Some((to_path(&m.path)?, rule))
        });
        if l.vector_mask.as_ref().is_some_and(|m| m.enabled) && clip.is_none() {
            // An empty vector mask hides everything.
            return;
        }
        if let Some((p, rule)) = &clip {
            s.push_clip_path(p, rule);
        }
        self.content(s, l);
        if clip.is_some() {
            s.pop();
        }
    }

    fn content(&mut self, s: &mut KSurface<'_>, l: &Layer) {
        match &l.content {
            // A fill layer covers the canvas, its gradient laid out in the layer's frame.
            LayerContent::Fill(f) => {
                let canvas = self.doc.bounds();
                let frame = photosplice_compose::fill_frame(l, canvas);
                let ink = self.ink.clone();
                let Some((paint, a)) = pdf_paint(f, &ink, |f| super::gradient::fill_paint(f, frame, &ink)) else {
                    self.rastered += 1;
                    self.with_blend(s, l, |w, s| w.image_of(s, &unblended(std::slice::from_ref(l))));
                    return;
                };
                let Some(rect) =
                    krilla::geom::Rect::from_xywh(canvas.x0 as f32, canvas.y0 as f32, canvas.width() as f32, canvas.height() as f32).and_then(|r| {
                        let mut b = PathBuilder::new();
                        b.push_rect(r);
                        b.finish()
                    })
                else {
                    return;
                };
                let opacity = NormalizedF32::new((a * l.fill_opacity).clamp(0.0, 1.0)).unwrap_or(NormalizedF32::ONE);
                self.with_blend(s, l, |_, s| {
                    s.set_fill(Some(KFill { paint, opacity, rule: KFillRule::NonZero }));
                    s.set_stroke(None);
                    s.draw_path(&rect);
                    s.set_fill(None);
                });
            }
            // An artboard: its background, then its contents, clipped to the board.
            LayerContent::Group(g) if g.artboard.is_some() => {
                let Some(ab) = &g.artboard else { return };
                let r = ab.rect;
                let Some(rect) = krilla::geom::Rect::from_xywh(r.x0 as f32, r.y0 as f32, r.width() as f32, r.height() as f32).and_then(|r| {
                    let mut b = PathBuilder::new();
                    b.push_rect(r);
                    b.finish()
                }) else {
                    return;
                };
                self.with_blend(s, l, |w, s| {
                    s.push_clip_path(&rect, &KFillRule::NonZero);
                    if let Some([r, g, b, a]) = ab.background.rgba() {
                        s.set_fill(Some(KFill {
                            paint: w.ink.rgb([r, g, b]).into(),
                            opacity: NormalizedF32::new(a.clamp(0.0, 1.0)).unwrap_or(NormalizedF32::ONE),
                            rule: KFillRule::NonZero,
                        }));
                        s.set_stroke(None);
                        s.draw_path(&rect);
                        s.set_fill(None);
                    }
                    w.layers(s, &g.children);
                    s.pop();
                });
            }
            LayerContent::Group(g) => {
                if l.blend == BlendMode::PassThrough && l.opacity >= 1.0 {
                    self.layers(s, &g.children);
                } else {
                    self.with_blend(s, l, |w, s| {
                        s.push_isolated();
                        w.layers(s, &g.children);
                        s.pop();
                    });
                }
            }
            LayerContent::Shape(sh) => self.with_blend(s, l, |w, s| draw_shape(s, sh, l.fill_opacity, &w.ink)),
            LayerContent::Text(t) => self.with_blend(s, l, |w, s| {
                if !w.draw_text(s, t, l.fill_opacity) {
                    w.rastered += 1;
                    w.image_of(s, &unblended(std::slice::from_ref(l)));
                }
            }),
            LayerContent::Raster(_) => self.with_blend(s, l, |w, s| w.image_of(s, &unblended(std::slice::from_ref(l)))),
            _ => {
                self.rastered += 1;
                self.with_blend(s, l, |w, s| w.image_of(s, &unblended(std::slice::from_ref(l))));
            }
        }
    }

    /// Runs `f` under the layer's opacity and blend mode.
    fn with_blend(&mut self, s: &mut KSurface<'_>, l: &Layer, f: impl FnOnce(&mut Self, &mut KSurface<'_>)) {
        let mut pushed = 0;
        if let Some(b) = pdf_blend(l.blend).filter(|b| *b != krilla::blend::BlendMode::Normal) {
            s.push_blend_mode(b);
            pushed += 1;
        }
        if l.opacity < 1.0 {
            s.push_opacity(NormalizedF32::new(l.opacity.clamp(0.0, 1.0)).unwrap_or(NormalizedF32::ONE));
            pushed += 1;
        }
        f(self, s);
        for _ in 0..pushed {
            s.pop();
        }
    }

    /// `layers` composited on their own (over transparency) and placed as an image, in the
    /// page's ink (a CMYK document's composite stays CMYK, with the document's profile).
    fn image_of(&mut self, s: &mut KSurface<'_>, layers: &[Layer]) {
        use super::ink::{Ink, InkImage};
        let mut tmp = self.doc.clone();
        tmp.layers = layers.to_vec();
        // Straight 8-bit pixels with alpha last, `n` channels, over `rect`.
        let (pixels, n, rect) = match &self.ink {
            Ink::Rgb => {
                let buf = photosplice_compose::flatten(&tmp);
                let img = buf.to_rgba8();
                (img.pixels, 4, buf.rect)
            }
            ink => {
                let fmt = ink.format();
                let surface = photosplice_compose::flatten_to_surface(&tmp, fmt, None);
                let rect = tmp.bounds();
                let mut f = Vec::new();
                surface.read_region_into(rect, &mut f);
                (f.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect(), fmt.channels(), rect)
            }
        };
        let (w, h) = (rect.width() as usize, rect.height() as usize);
        // Trim to what the layers cover.
        let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
        for (i, a) in pixels.iter().skip(n - 1).step_by(n).enumerate() {
            if *a != 0 {
                let (x, y) = (i % w.max(1), i / w.max(1));
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let mut px = Vec::with_capacity((x1 - x0) * (y1 - y0) * n);
        for y in y0..y1 {
            if let Some(row) = pixels.get((y * w + x0) * n..(y * w + x1) * n) {
                px.extend_from_slice(row);
            }
        }
        let (iw, ih) = ((x1 - x0) as u32, (y1 - y0) as u32);
        let origin = (rect.x0 as f32 + x0 as f32, rect.y0 as f32 + y0 as f32);
        // Downsampled pixels still fill the image's place on the page.
        let (px, pw, ph) = match self.opts.downsample {
            Some((to, above)) if self.dpi > above && to > 0.0 && to < self.dpi => {
                let k = to / self.dpi;
                let (pw, ph) = (((iw as f32 * k).round() as u32).max(1), ((ih as f32 * k).round() as u32).max(1));
                (downsample(&px, (iw, ih), (pw, ph), n), pw, ph)
            }
            _ => (px, iw, ih),
        };
        let opaque = px.chunks_exact(n).all(|p| p.last() == Some(&255));
        let jpeg = self.opts.jpeg_quality.filter(|_| opaque).and_then(|q| {
            let layout = match &self.ink {
                Ink::Rgb => photosplice_codecs::ChannelLayout::Rgb,
                Ink::Gray => photosplice_codecs::ChannelLayout::Gray,
                Ink::Cmyk(_) => photosplice_codecs::ChannelLayout::Cmyk,
            };
            let color: Vec<u8> = px.chunks_exact(n).flat_map(|p| p.iter().take(n - 1).copied()).collect();
            let img = photosplice_codecs::Image::from_u8(pw, ph, layout, color).ok()?;
            let enc = photosplice_codecs::EncodeOptions { jpeg_quality: q.clamp(1, 100), ..Default::default() };
            let data = photosplice_codecs::encode(&img, photosplice_codecs::Format::Jpeg, &enc).ok()?;
            let icc = if matches!(self.ink, Ink::Rgb) { None } else { self.doc.icc_profile.as_ref().map(|p| krilla::Data::from(p.to_vec())) };
            krilla::image::Image::from_jpeg_with_icc(data.into(), icc, false).ok()
        });
        let image = if let Some(jpeg) = jpeg {
            jpeg
        } else {
            match &self.ink {
                Ink::Rgb => krilla::image::Image::from_rgba8(px, pw, ph),
                ink => {
                    let (color, alpha): (Vec<u8>, Vec<u8>) =
                        (px.chunks_exact(n).flat_map(|p| p.iter().take(n - 1).copied()).collect(), px.chunks_exact(n).map(|p| p[n - 1]).collect());
                    let space = if matches!(ink, Ink::Cmyk(_)) { krilla::image::ImageColorspace::Cmyk } else { krilla::image::ImageColorspace::Luma };
                    let img = InkImage::new(color, (!opaque).then_some(alpha), (pw, ph), space, self.doc.icc_profile.clone());
                    match krilla::image::Image::from_custom(img, false) {
                        Ok(i) => i,
                        Err(_) => return,
                    }
                }
            }
        };
        let Some(size) = KSize::from_wh(iw as f32, ih as f32) else { return };
        s.push_transform(&Transform::from_translate(origin.0, origin.1));
        s.draw_image(image, size);
        s.pop();
    }

    /// The layer's text as PDF text in its fonts, laid out as the text engine lays it out.
    /// `false` when it can't be (warped, vertical or path type, a face krilla can't read).
    fn draw_text(&mut self, s: &mut KSurface<'_>, t: &TextLayer, fill_opacity: f32) -> bool {
        if t.warp.is_some() || t.path.is_some() || !matches!(t.orientation, photosplice_doc::text::Orientation::Horizontal) {
            return false;
        }
        let mut engine = photosplice_text::shared().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // A font that isn't installed would be set in the fallback face: the layer's own pixels
        // (from the PSD or PDF it came from) still show its font, as the canvas does.
        if t.cache.is_some() && t.char_runs().iter().any(|r| !engine.fonts.renders_as_named(&r.style.font_family, r.style.postscript_name.as_deref())) {
            return false;
        }
        let layout = engine.layout(t, self.doc.resolution_dpi.max(1.0));
        drop(engine);
        let m = t.transform.m;
        s.push_transform(&Transform::from_row(m[0] as f32, m[1] as f32, m[2] as f32, m[3] as f32, m[4] as f32, m[5] as f32));
        // Runs of glyphs sharing a face, a style and a baseline.
        let glyphs = &layout.glyphs;
        let mut i = 0;
        let mut ok = true;
        while i < glyphs.len() {
            let g0 = glyphs[i];
            let mut j = i + 1;
            while j < glyphs.len() && glyphs[j].face == g0.face && glyphs[j].style == g0.style && (glyphs[j].y - g0.y).abs() < 0.01 {
                j += 1;
            }
            let (Some(face), Some(style)) = (layout.faces.get(g0.face as usize), layout.styles.get(g0.style as usize)) else {
                i = j;
                continue;
            };
            let Some(font) = self.font(face) else {
                ok = false;
                break;
            };
            let size = face.size_px.max(0.01);
            let run: Vec<KrillaGlyph> = glyphs[i..j]
                .iter()
                .enumerate()
                .map(|(n, g)| {
                    let next = glyphs.get(i + n + 1).filter(|_| i + n + 1 < j).map_or(g.x + size * 0.5, |q| q.x);
                    let range = cluster_range(&layout, g.x, g.y);
                    KrillaGlyph::new(GlyphId::new(g.id), (next - g.x) / size, 0.0, 0.0, 0.0, range, None)
                })
                .collect();
            s.set_fill(Some(KFill {
                paint: self.ink.color(&style.color).into(),
                opacity: NormalizedF32::new((style.color.alpha * fill_opacity).clamp(0.0, 1.0)).unwrap_or(NormalizedF32::ONE),
                rule: KFillRule::NonZero,
            }));
            s.set_stroke(None);
            s.draw_glyphs(KPoint::from_xy(g0.x, g0.y), &run, font, &t.text, size, false);
            i = j;
        }
        s.pop();
        ok
    }

    fn font(&mut self, face: &photosplice_text::layout::GlyphFace) -> Option<Font> {
        let bytes: &[u8] = face.font.data.as_ref();
        let key = (bytes.as_ptr() as usize, face.font.index);
        if let Some((_, f)) = self.fonts.iter().find(|(k, _)| *k == key) {
            return Some(f.clone());
        }
        let f = Font::new(bytes.to_vec().into(), face.font.index)?;
        self.fonts.push((key, f.clone()));
        Some(f)
    }
}

/// The text a glyph at (`x`, `y`) belongs to (for copying text out of the PDF).
fn cluster_range(layout: &photosplice_text::layout::TextLayout, x: f32, y: f32) -> std::ops::Range<usize> {
    let line = layout.lines.iter().position(|l| (l.baseline - y).abs() < 0.5).unwrap_or(0);
    layout.clusters.iter().filter(|c| c.line == line).find(|c| x >= c.x - 0.01 && x < c.x + c.advance.max(0.01)).map_or(0..0, |c| c.range.clone())
}

/// The layers with the first one's blend and opacity left to the PDF (Normal, 100 %).
fn unblended(layers: &[Layer]) -> Vec<Layer> {
    let mut v = layers.to_vec();
    if let Some(l) = v.first_mut() {
        l.blend = if l.is_group() { BlendMode::PassThrough } else { BlendMode::Normal };
        l.opacity = 1.0;
    }
    v
}

/// Whether the layer works on what's beneath it, which PDF can't do.
fn needs_backdrop(l: &Layer) -> bool {
    matches!(l.content, LayerContent::Adjustment(_))
        || (l.blend != BlendMode::PassThrough && pdf_blend(l.blend).is_none())
        || l.children().is_some_and(|c| c.iter().any(|c| c.visible && needs_backdrop(c)))
}

/// Whether the layer can be drawn as PDF content (else it's composited into an image).
fn native(l: &Layer) -> bool {
    // A vector mask is a clip when it's hard-edged and fully on.
    let vmask = l.vector_mask.as_ref().is_none_or(|m| !m.enabled || (m.density >= 1.0 && m.feather <= 0.0 && plain_path(&m.path)));
    let plain = !photosplice_compose::effects::has_effects(l) && l.mask.is_none() && vmask && l.blend_if.is_default() && l.excluded_channels == 0;
    plain
        && match &l.content {
            LayerContent::Shape(s) => native_shape(s),
            LayerContent::Fill(f) => pdf_fill(f),
            LayerContent::Text(_) | LayerContent::Raster(_) => true,
            LayerContent::Group(_) => true,
            _ => false,
        }
}

/// Colours PDF paints natively: solid colour, and gradients but Diamond ones.
fn pdf_fill(f: &Fill) -> bool {
    match f {
        Fill::Solid(_) => true,
        Fill::Gradient { style, .. } => *style != photosplice_doc::GradientStyle::Diamond,
        Fill::Pattern { .. } => false,
    }
}

/// A path PDF fills as we do: shapes combined, not inverted.
fn plain_path(p: &Path) -> bool {
    p.subpaths.iter().all(|p| matches!(p.op, PathOp::Combine | PathOp::Join)) && !p.inverted
}

fn native_shape(s: &ShapeLayer) -> bool {
    s.fill.as_ref().is_none_or(pdf_fill) && s.stroke.as_ref().is_none_or(|st| pdf_fill(&st.paint) && st.align == StrokeAlign::Center) && plain_path(&s.path)
}

/// The krilla paint and opacity of `f` (solid, or a gradient laid out by `gradient`).
fn pdf_paint(f: &Fill, ink: &super::ink::Ink, gradient: impl FnOnce(&Fill) -> Option<krilla::paint::Paint>) -> Option<(krilla::paint::Paint, f32)> {
    match f {
        Fill::Solid(c) => Some((ink.color(c).into(), c.alpha.clamp(0.0, 1.0))),
        Fill::Gradient { .. } => gradient(f).map(|p| (p, 1.0)),
        Fill::Pattern { .. } => None,
    }
}

fn draw_shape(s: &mut KSurface<'_>, sh: &ShapeLayer, fill_opacity: f32, ink: &super::ink::Ink) {
    let Some(path) = to_path(&sh.path) else { return };
    let rule = match sh.path.fill_rule {
        FillRule::NonZero => KFillRule::NonZero,
        FillRule::EvenOdd => KFillRule::EvenOdd,
    };
    // Gradients laid out over the path's control bounds, as the shape renderer does.
    let bounds = sh.path.control_bounds().unwrap_or((0.0, 0.0, 0.0, 0.0));
    let color = |f: &Fill| pdf_paint(f, ink, |f| super::gradient::shape_paint(f, bounds, ink));
    s.set_fill(sh.fill.as_ref().and_then(|f| {
        let (c, a) = color(f)?;
        Some(KFill { paint: c, opacity: NormalizedF32::new((a * fill_opacity).clamp(0.0, 1.0)).unwrap_or(NormalizedF32::ONE), rule })
    }));
    s.set_stroke(sh.stroke.as_ref().filter(|st| st.width > 0.0).and_then(|st| {
        let (c, a) = color(&st.paint)?;
        Some(KStroke {
            paint: c,
            width: st.width,
            miter_limit: st.miter_limit.max(1.0),
            line_cap: match st.cap {
                LineCap::Butt => KCap::Butt,
                LineCap::Round => KCap::Round,
                LineCap::Square => KCap::Square,
            },
            line_join: match st.join {
                LineJoin::Miter => KJoin::Miter,
                LineJoin::Round => KJoin::Round,
                LineJoin::Bevel => KJoin::Bevel,
            },
            opacity: NormalizedF32::new((a * st.opacity * fill_opacity).clamp(0.0, 1.0)).unwrap_or(NormalizedF32::ONE),
            dash: (!st.dashes.is_empty()).then(|| StrokeDash { array: st.dashes.iter().map(|d| d * st.width).collect(), offset: st.dash_offset * st.width }),
        })
    }));
    s.draw_path(&path);
    s.set_fill(None);
    s.set_stroke(None);
}

/// Our cubic path as a krilla path.
fn to_path(p: &Path) -> Option<KPath> {
    let mut b = PathBuilder::new();
    for sp in &p.subpaths {
        let Some(first) = sp.knots.first() else { continue };
        b.move_to(first.anchor.x as f32, first.anchor.y as f32);
        let n = sp.knots.len();
        let segs = if sp.closed { n } else { n.saturating_sub(1) };
        for i in 0..segs {
            let (a, z) = (&sp.knots[i], &sp.knots[(i + 1) % n]);
            b.cubic_to(a.out_ctrl.x as f32, a.out_ctrl.y as f32, z.in_ctrl.x as f32, z.in_ctrl.y as f32, z.anchor.x as f32, z.anchor.y as f32);
        }
        if sp.closed {
            b.close();
        }
    }
    b.finish()
}

fn pdf_blend(b: BlendMode) -> Option<krilla::blend::BlendMode> {
    use krilla::blend::BlendMode as K;
    Some(match b {
        BlendMode::Normal | BlendMode::PassThrough => K::Normal,
        BlendMode::Multiply => K::Multiply,
        BlendMode::Screen => K::Screen,
        BlendMode::Overlay => K::Overlay,
        BlendMode::Darken => K::Darken,
        BlendMode::Lighten => K::Lighten,
        BlendMode::ColorDodge => K::ColorDodge,
        BlendMode::ColorBurn => K::ColorBurn,
        BlendMode::HardLight => K::HardLight,
        BlendMode::SoftLight => K::SoftLight,
        BlendMode::Difference => K::Difference,
        BlendMode::Exclusion => K::Exclusion,
        BlendMode::Hue => K::Hue,
        BlendMode::Saturation => K::Saturation,
        BlendMode::Color => K::Color,
        BlendMode::Luminosity => K::Luminosity,
        _ => return None,
    })
}

/// `px` (`n` straight 8-bit channels, alpha last) of `from` pixels averaged down to `to`: each
/// output pixel is the coverage-weighted mean of the source pixels it covers.
fn downsample(px: &[u8], from: (u32, u32), to: (u32, u32), n: usize) -> Vec<u8> {
    let (fw, fh) = (from.0 as usize, from.1 as usize);
    let (tw, th) = (to.0.max(1) as usize, to.1.max(1) as usize);
    let mut out = vec![0u8; tw * th * n];
    for ty in 0..th {
        let (sy0, sy1) = (ty * fh / th, ((ty + 1) * fh / th).max(ty * fh / th + 1).min(fh));
        for tx in 0..tw {
            let (sx0, sx1) = (tx * fw / tw, ((tx + 1) * fw / tw).max(tx * fw / tw + 1).min(fw));
            let mut sum = vec![0f64; n];
            let mut count = 0f64;
            for sy in sy0..sy1 {
                for sx in sx0..sx1 {
                    let Some(p) = px.get((sy * fw + sx) * n..(sy * fw + sx + 1) * n) else { continue };
                    let a = f64::from(p[n - 1]) / 255.0;
                    for c in 0..n - 1 {
                        sum[c] += f64::from(p[c]) * a;
                    }
                    sum[n - 1] += a;
                    count += 1.0;
                }
            }
            if let Some(o) = out.get_mut((ty * tw + tx) * n..(ty * tw + tx + 1) * n) {
                let a = sum[n - 1];
                for c in 0..n - 1 {
                    o[c] = if a > 0.0 { (sum[c] / a).round().clamp(0.0, 255.0) as u8 } else { 0 };
                }
                o[n - 1] = if count > 0.0 { (a / count * 255.0).round().clamp(0.0, 255.0) as u8 } else { 0 };
            }
        }
    }
    out
}
