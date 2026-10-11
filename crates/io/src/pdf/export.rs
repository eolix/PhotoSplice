//! A document as a Photoshop PDF: one page drawn layer by layer as PDF content (krilla, MIT OR
//! Apache-2.0), plus Photoshop's editing data ([`super::photoshop`]), with which Photoshop and
//! PhotoSuite reopen it with every layer as it was.
//!
//! Drawn natively, so the PDF stays vector and searchable: shape layers (solid fills and centred
//! strokes) as paths, type layers as real text in their fonts (embedded and subset, with the text
//! copyable), pixel layers as images, groups as transparency groups, with each layer's opacity
//! and blend mode. A layer PDF has no equivalent for (effects, masks, gradient or pattern fills,
//! warped or vertical type, clipping groups, smart objects, fill layers) is composited on its own
//! and placed as an image at its place in the stack. Adjustment layers, and blend modes PDF lacks
//! (Linear Burn, Vivid Light, …), work on what is beneath them, so everything up to the topmost
//! one is flattened into one image first.

use krilla::color::rgb;
use krilla::geom::{Path as KPath, PathBuilder, Point as KPoint, Size as KSize, Transform};
use krilla::num::NormalizedF32;
use krilla::page::PageSettings;
use krilla::paint::{Fill as KFill, FillRule as KFillRule, LineCap as KCap, LineJoin as KJoin, Stroke as KStroke, StrokeDash};
use krilla::surface::Surface as KSurface;
use krilla::text::{Font, GlyphId, KrillaGlyph};
use photosuite_color::{BlendMode, ColorMode};
use photosuite_doc::vector::{FillRule, LineCap, LineJoin, Path, PathOp, ShapeLayer, StrokeAlign};
use photosuite_doc::{Document, Fill, Layer, LayerContent, TextLayer};

use crate::{ExportResult, IoError};

/// Saves `doc` as a one-page layered PDF; `editing` adds the Photoshop editing data (*Preserve
/// Photoshop Editing Capabilities*).
pub fn export_layered(doc: &Document, editing: bool) -> Result<ExportResult, IoError> {
    let mut warnings = Vec::new();
    let mut pdf = krilla::Document::new();
    draw_page(&mut pdf, doc, &mut warnings)?;
    let mut bytes = finish(pdf, &doc.name)?;
    if editing {
        match super::photoshop::attach(bytes.clone(), doc, &mut warnings) {
            Ok(b) => bytes = b,
            Err(e) => warnings.push(format!("{e}; saved without Photoshop editing data")),
        }
    }
    Ok(ExportResult { bytes, warnings })
}

/// A PDF of one page per document, each drawn as [`export_layered`] draws its page (Artboards to
/// PDF, a page per board). No editing data: a Photoshop PDF holds one document.
pub fn export_pages(docs: &[Document], title: &str) -> Result<ExportResult, IoError> {
    if docs.is_empty() {
        return Err(IoError::Unsupported("PDF: no pages to write".into()));
    }
    let mut warnings = Vec::new();
    let mut pdf = krilla::Document::new();
    for doc in docs {
        draw_page(&mut pdf, doc, &mut warnings)?;
    }
    warnings.dedup();
    Ok(ExportResult { bytes: finish(pdf, title)?, warnings })
}

fn finish(mut pdf: krilla::Document, title: &str) -> Result<Vec<u8>, IoError> {
    let mut meta = krilla::metadata::Metadata::new().creator("PhotoSuite".into());
    if !title.is_empty() {
        meta = meta.title(title.to_string());
    }
    pdf.set_metadata(meta);
    pdf.finish().map_err(|e| IoError::Unsupported(format!("PDF: can't write the file ({e:?})")))
}

/// `doc` as one page of `pdf`, sized so it prints at its resolution.
fn draw_page(pdf: &mut krilla::Document, doc: &Document, warnings: &mut Vec<String>) -> Result<(), IoError> {
    let dpi = if doc.resolution_dpi > 0.0 && doc.resolution_dpi.is_finite() { doc.resolution_dpi } else { 72.0 };
    let k = 72.0 / dpi;
    let (w, h) = (doc.size.width.max(1) as f32, doc.size.height.max(1) as f32);
    let settings = PageSettings::from_wh(w * k, h * k).ok_or_else(|| IoError::Unsupported("PDF: the page has no size".into()))?;
    let mut page = pdf.start_page_with(settings);
    let mut surface = page.surface();
    surface.push_transform(&Transform::from_scale(k, k));
    let mut w = Writer { doc, rastered: 0, fonts: Vec::new() };
    if doc.mode == ColorMode::Rgb {
        // Everything up to the topmost layer that works on what is beneath it, as one image.
        let cut = doc.layers.iter().rposition(|l| l.visible && needs_backdrop(l));
        let rest = match cut {
            Some(i) => {
                w.image_of(&mut surface, &doc.layers[..=i]);
                &doc.layers[i + 1..]
            }
            None => &doc.layers[..],
        };
        w.layers(&mut surface, rest);
    } else {
        // Grayscale, CMYK, Lab…: the composite, converted for display.
        w.image_of(&mut surface, &doc.layers);
        warnings.push(format!("PDF: a {:?} document is saved as one image (its layers are in the editing data)", doc.mode));
    }
    surface.pop();
    surface.finish();
    page.finish();
    if w.rastered > 0 {
        warnings.push(format!("PDF: {} layer(s) with no PDF equivalent (effects, masks, gradients, adjustments, …) saved as images", w.rastered));
    }
    Ok(())
}

struct Writer<'d> {
    doc: &'d Document,
    rastered: usize,
    /// Fonts already handed to krilla, by the face's data pointer and index.
    fonts: Vec<((usize, u32), Font)>,
}

impl Writer<'_> {
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
                let frame = photosuite_compose::fill_frame(l, canvas);
                let Some((paint, a)) = pdf_paint(f, |f| super::gradient::fill_paint(f, frame)) else {
                    self.rastered += 1;
                    self.with_blend(s, l, |w, s| w.image_of(s, &unblended(std::slice::from_ref(l))));
                    return;
                };
                let Some(rect) = krilla::geom::Rect::from_xywh(canvas.x0 as f32, canvas.y0 as f32, canvas.width() as f32, canvas.height() as f32).and_then(|r| {
                    let mut b = PathBuilder::new();
                    b.push_rect(r);
                    b.finish()
                }) else {
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
                        let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                        s.set_fill(Some(KFill { paint: rgb::Color::new(c(r), c(g), c(b)).into(), opacity: NormalizedF32::new(a.clamp(0.0, 1.0)).unwrap_or(NormalizedF32::ONE), rule: KFillRule::NonZero }));
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
            LayerContent::Shape(sh) => self.with_blend(s, l, |_, s| draw_shape(s, sh, l.fill_opacity)),
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

    /// `layers` composited on their own (over transparency) and placed as an image.
    fn image_of(&mut self, s: &mut KSurface<'_>, layers: &[Layer]) {
        let mut tmp = self.doc.clone();
        tmp.layers = layers.to_vec();
        let buf = photosuite_compose::flatten(&tmp);
        let img = buf.to_rgba8();
        let (w, h) = (img.width as usize, img.height as usize);
        // Trim to what the layers cover.
        let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
        for (i, a) in img.pixels.iter().skip(3).step_by(4).enumerate() {
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
        let mut px = Vec::with_capacity((x1 - x0) * (y1 - y0) * 4);
        for y in y0..y1 {
            if let Some(row) = img.pixels.get((y * w + x0) * 4..(y * w + x1) * 4) {
                px.extend_from_slice(row);
            }
        }
        let (iw, ih) = ((x1 - x0) as u32, (y1 - y0) as u32);
        let origin = (buf.rect.x0 as f32 + x0 as f32, buf.rect.y0 as f32 + y0 as f32);
        let image = krilla::image::Image::from_rgba8(px, iw, ih);
        let Some(size) = KSize::from_wh(iw as f32, ih as f32) else { return };
        s.push_transform(&Transform::from_translate(origin.0, origin.1));
        s.draw_image(image, size);
        s.pop();
    }

    /// The layer's text as PDF text in its fonts, laid out as the text engine lays it out.
    /// `false` when it can't be (warped or vertical type, a face krilla can't read).
    fn draw_text(&mut self, s: &mut KSurface<'_>, t: &TextLayer, fill_opacity: f32) -> bool {
        if t.warp.is_some() || !matches!(t.orientation, photosuite_doc::text::Orientation::Horizontal) {
            return false;
        }
        let mut engine = photosuite_text::shared().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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
            let [r, gg, b, a] = style.color.to_rgba8();
            s.set_fill(Some(KFill {
                paint: rgb::Color::new(r, gg, b).into(),
                opacity: NormalizedF32::new((f32::from(a) / 255.0 * fill_opacity).clamp(0.0, 1.0)).unwrap_or(NormalizedF32::ONE),
                rule: KFillRule::NonZero,
            }));
            s.set_stroke(None);
            s.draw_glyphs(KPoint::from_xy(g0.x, g0.y), &run, font, &t.text, size, false);
            i = j;
        }
        s.pop();
        ok
    }

    fn font(&mut self, face: &photosuite_text::layout::GlyphFace) -> Option<Font> {
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
fn cluster_range(layout: &photosuite_text::layout::TextLayout, x: f32, y: f32) -> std::ops::Range<usize> {
    let line = layout.lines.iter().position(|l| (l.baseline - y).abs() < 0.5).unwrap_or(0);
    layout
        .clusters
        .iter()
        .filter(|c| c.line == line)
        .find(|c| x >= c.x - 0.01 && x < c.x + c.advance.max(0.01))
        .map_or(0..0, |c| c.range.clone())
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
    matches!(l.content, LayerContent::Adjustment(_)) || (l.blend != BlendMode::PassThrough && pdf_blend(l.blend).is_none()) || l.children().is_some_and(|c| c.iter().any(|c| c.visible && needs_backdrop(c)))
}

/// Whether the layer can be drawn as PDF content (else it's composited into an image).
fn native(l: &Layer) -> bool {
    // A vector mask is a clip when it's hard-edged and fully on.
    let vmask = l.vector_mask.as_ref().is_none_or(|m| !m.enabled || (m.density >= 1.0 && m.feather <= 0.0 && plain_path(&m.path)));
    let plain = !photosuite_compose::effects::has_effects(l) && l.mask.is_none() && vmask && l.blend_if.is_default() && l.excluded_channels == 0;
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
        Fill::Gradient { style, .. } => *style != photosuite_doc::GradientStyle::Diamond,
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
fn pdf_paint(f: &Fill, gradient: impl FnOnce(&Fill) -> Option<krilla::paint::Paint>) -> Option<(krilla::paint::Paint, f32)> {
    match f {
        Fill::Solid(c) => {
            let [r, g, b, a] = c.to_rgba8();
            Some((rgb::Color::new(r, g, b).into(), f32::from(a) / 255.0))
        }
        Fill::Gradient { .. } => gradient(f).map(|p| (p, 1.0)),
        Fill::Pattern { .. } => None,
    }
}

fn draw_shape(s: &mut KSurface<'_>, sh: &ShapeLayer, fill_opacity: f32) {
    let Some(path) = to_path(&sh.path) else { return };
    let rule = match sh.path.fill_rule {
        FillRule::NonZero => KFillRule::NonZero,
        FillRule::EvenOdd => KFillRule::EvenOdd,
    };
    // Gradients laid out over the path's control bounds, as the shape renderer does.
    let bounds = sh.path.control_bounds().unwrap_or((0.0, 0.0, 0.0, 0.0));
    let color = |f: &Fill| pdf_paint(f, |f| super::gradient::shape_paint(f, bounds));
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
