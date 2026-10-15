//! A PDF page as editable layers: hayro-interpret walks the page's operations into
//! [`LayerBuilder`], a [`Device`] that turns them into a layer stack, bottom to top as drawn:
//!
//! * **text** (outline fonts, filled) → type layers: the Unicode text, font (the installed face
//!   when there is one, else the PDF's embedded font, registered with the text engine), size,
//!   colour and baseline transform. A run on the same baseline continues the previous layer.
//!   The layer's pixels start as the PDF's own glyphs, drawn by the vendored hayro renderer, so it
//!   looks exactly as in the PDF until it is edited;
//! * **fills and strokes** in a solid colour → shape layers. Consecutive opaque fills of one
//!   colour share a layer, and a stroke of the path just filled joins its fill (`B`);
//! * **axial and radial shadings** (`sh`, and paths filled with a shading pattern) → Gradient
//!   Fill layers the path and its clips mask, as Photoshop keeps a gradient-filled shape
//!   ([`super::shading`]);
//! * **images** → pixel layers, drawn by the renderer as the PDF places them;
//! * **clips** → vector masks; **transparency groups** with an opacity or a blend mode → layer
//!   groups;
//! * anything else (shadings a Photoshop gradient can't hold, tiling patterns, soft masks, Type 3
//!   glyphs, stroked or clipping text) → pixels, drawn by the renderer at its place in the stack, so nothing is lost.
//!
//! Coordinates are document pixels: the page's initial transform scaled to the resolution.

use std::collections::HashMap;

use hayro::hayro_interpret::font::{Glyph, GlyphRun};
use hayro::hayro_interpret::hayro_cmap::BfString;
use hayro::hayro_interpret::{
    BlendMode as PdfBlend, ClipPath, Context, Device, DrawMode, DrawProps, FillRule as PdfFillRule, Image, ImageDrawProps, InterpreterSettings,
    Paint, SoftMask, StrokeProps, interpret_page,
};
use hayro::hayro_syntax::page::Page;
use hayro::kurbo::{self, Affine as KAffine, BezPath, PathEl, Shape};
use hayro::vello_cpu::{self, Pixmap, RenderContext};
use photosplice_color::{BlendMode, Color, ColorMode, PixelFormat, SampleType};
use photosplice_doc::text::{CharStyle, TextRun, TextShape};
use photosplice_doc::vector::{FillRule, Knot, LineCap, LineJoin, Path, PathOp, ShapeLayer, ShapeStroke, StrokeAlign, Subpath, VectorMask};
use photosplice_doc::{Document, Fill, Layer, LayerContent, TextLayer};
use photosplice_geom::{Affine, Point, Rect, Size};
use photosplice_raster::Surface;

use super::render::{GlobalState, RenderCache, RenderSettings, Renderer};

/// Above this many layers the rest of the page comes in as pixels (a page of map tiles or a
/// chart of thousands of marks would otherwise make the Layers panel unusable).
const MAX_LAYERS: usize = 4000;

/// A page as a layered document, placed by `place`, at `dpi`.
pub(crate) fn import_layers(name: &str, page: &Page<'_>, place: &super::Placement, dpi: f32) -> (Document, Vec<String>) {
    let (w, h) = (place.w.max(1), place.h.max(1));
    let mut doc = Document::new(name, Size::new(w, h), ColorMode::Rgb, SampleType::U8);
    doc.resolution_dpi = dpi;
    let fmt = doc.pixel_format();
    let cache = RenderCache::new();
    let global = GlobalState::new(&cache, &RenderSettings::default());
    let transform = place.transform;
    let mut builder = LayerBuilder::new(&global, w, h, fmt, doc.resolution_dpi);
    builder.font_names = font_names(page);
    // The chosen page box clips everything, as in the renderer.
    let mut crop = place.clip.to_path(0.1);
    crop.apply_affine(transform);
    builder.push_clip_path(&ClipPath { path: crop, fill: PdfFillRule::NonZero });
    let mut ctx = Context::new(transform, kurbo::Rect::new(0.0, 0.0, f64::from(w), f64::from(h)), &cache.interpreter_cache, page.xref(), InterpreterSettings::default());
    interpret_page(page, &mut ctx, &mut builder);
    builder.pop_clip();
    let (layers, warnings) = builder.finish();
    doc.layers = layers;
    if doc.layers.is_empty() {
        doc.layers.push(Layer::raster("Layer 1", fmt));
    }
    (doc, warnings)
}

/// One level of the layer stack being built: the page itself, or a transparency group.
struct Frame {
    layers: Vec<Layer>,
    /// `Some` for a transparency group that becomes a layer group; `None` for the page, and for
    /// a group with nothing to keep (opacity 1, Normal), whose layers go to the parent.
    group: Option<(f32, BlendMode)>,
}

/// Pixels being drawn by the renderer for a run of operations with no editable form.
struct Segment {
    ctx: RenderContext,
    /// Clips pushed into `ctx` since the segment began (the ones before were replayed).
    clips: usize,
}

/// The text layer being built from consecutive runs on one baseline.
struct TextAcc {
    text: String,
    style: CharStyle,
    /// Baseline origin, rotation and horizontal scale of the first glyph (document pixels).
    origin: Point,
    /// Where the current line's baseline starts, and the distance between lines once known.
    line: Point,
    leading: Option<f64>,
    angle: f64,
    size_px: f64,
    /// How far along the baseline the last glyph ended.
    end: f64,
    cache: Surface,
    clip: Option<u64>,
}

/// How a glyph run joins the text layer being built.
#[derive(Clone, Copy, PartialEq)]
enum Join {
    /// Further along the same line.
    Line,
    /// The next line of the same paragraph, this far below.
    NextLine(f64),
    New,
}

/// The shape layer being built.
struct ShapeAcc {
    shape: ShapeLayer,
    blend: BlendMode,
    clip: Option<u64>,
    /// Fill colour when it is an opaque fill that more fills of the same colour may join.
    joinable: Option<[u8; 4]>,
    /// The last path drawn, for a stroke of the same path (`B`) to join its fill.
    last: Option<BezPath>,
    paths: usize,
}

pub(crate) struct LayerBuilder<'g> {
    global: &'g GlobalState,
    w: u32,
    h: u32,
    fmt: PixelFormat,
    dpi: f32,
    /// Active clips (document pixels), with an id each (0 for one that clips nothing on the
    /// page: the crop box, a page-sized rectangle); `clips[0]` is the crop box.
    clips: Vec<(ClipPath, u64)>,
    next_clip_id: u64,
    frames: Vec<Frame>,
    segment: Option<Segment>,
    /// Transparency groups (with soft masks) being drawn wholesale into `segment`.
    raster_groups: u32,
    text: Option<TextAcc>,
    shape: Option<ShapeAcc>,
    layer_count: usize,
    /// Font (by hayro's cache key) → the family, weight and italic its type layers name.
    fonts: HashMap<u128, Option<(String, u16, bool)>>,
    /// Font (by hayro's cache key) → its `BaseFont`, for fonts hayro has no data for (Type 1).
    font_names: HashMap<u128, String>,
    warnings: Vec<String>,
    pixel_items: usize,
}

impl<'g> LayerBuilder<'g> {
    fn new(global: &'g GlobalState, w: u32, h: u32, fmt: PixelFormat, dpi: f32) -> Self {
        LayerBuilder {
            global,
            w,
            h,
            fmt,
            dpi,
            clips: Vec::new(),
            next_clip_id: 1,
            frames: vec![Frame { layers: Vec::new(), group: None }],
            segment: None,
            raster_groups: 0,
            text: None,
            shape: None,
            layer_count: 0,
            fonts: HashMap::new(),
            font_names: HashMap::new(),
            warnings: Vec::new(),
            pixel_items: 0,
        }
    }

    fn finish(mut self) -> (Vec<Layer>, Vec<String>) {
        self.flush_all();
        while self.frames.len() > 1 {
            self.close_frame();
        }
        if self.layer_count >= MAX_LAYERS {
            self.warnings.push(format!("PDF: more than {MAX_LAYERS} layers; the rest of the page came in as pixels"));
        }
        if self.pixel_items > 0 {
            self.warnings.push(format!("PDF: {} item(s) with no editable equivalent (patterns, soft masks, …) came in as pixels", self.pixel_items));
        }
        let layers = self.frames.pop().map(|f| f.layers).unwrap_or_default();
        (layers, self.warnings)
    }

    fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.w as i32, self.h as i32)
    }

    fn push_layer(&mut self, layer: Layer) {
        self.layer_count += 1;
        if let Some(f) = self.frames.last_mut() {
            f.layers.push(layer);
        }
    }

    /// The clips that clip something: what a layer's vector mask is made of.
    fn active_clips(&self) -> impl Iterator<Item = &(ClipPath, u64)> {
        self.clips.iter().filter(|c| c.1 != 0)
    }

    /// Identifies the clips a layer is under (the innermost one that clips anything).
    fn clip_key(&self) -> Option<u64> {
        self.active_clips().last().map(|c| c.1)
    }

    fn vector_mask(&self) -> Option<VectorMask> {
        let clips: Vec<&ClipPath> = self.active_clips().map(|c| &c.0).collect();
        if clips.is_empty() {
            return None;
        }
        let mut subpaths = Vec::new();
        for (i, c) in clips.iter().enumerate() {
            let mut p = bez_to_path(&c.path);
            let op = if i == 0 { PathOp::Combine } else { PathOp::Intersect };
            for (j, s) in p.subpaths.iter_mut().enumerate() {
                s.op = if j == 0 { op } else { PathOp::Join };
            }
            subpaths.extend(p.subpaths);
        }
        let mut path = Path::new(subpaths);
        if let Some(c) = clips.last() {
            path.fill_rule = fill_rule(c.fill);
        }
        Some(VectorMask::new(path))
    }

    fn full(&self) -> bool {
        self.layer_count >= MAX_LAYERS
    }

    /// `path` filled with the shading `sp` as a Gradient Fill layer the path (and the clips it is
    /// under) masks. `false`, having done nothing, when a Photoshop gradient can't hold it.
    fn gradient_layer(&mut self, path: &BezPath, props: &DrawProps<'_>, rule: &PdfFillRule, sp: &hayro::hayro_interpret::pattern::ShadingPattern) -> bool {
        if sp.shading.clip_path.is_some() {
            return false;
        }
        let mut dev = path.clone();
        dev.apply_affine(props.transform);
        let page = kurbo::Rect::new(0.0, 0.0, f64::from(self.w), f64::from(self.h));
        let bbox = dev.bounding_box().intersect(page);
        if bbox.width() <= 0.0 || bbox.height() <= 0.0 {
            return false;
        }
        let Some(g) = super::shading::gradient(sp, bbox) else { return false };
        // The mask: the path, intersected with the clips it is under (one fill rule for both).
        let mut own = bez_to_path(&dev);
        own.fill_rule = fill_rule(*rule);
        let clips = self.vector_mask();
        if clips.as_ref().is_some_and(|c| c.path.fill_rule != own.fill_rule) {
            return false;
        }
        let mask = match clips {
            None if covers(&dev, page.inflate(-0.5, -0.5)) => None,
            None => Some(VectorMask::new(own)),
            Some(c) => {
                let mut subpaths = own.subpaths;
                for (j, sp) in subpaths.iter_mut().enumerate() {
                    sp.op = if j == 0 { PathOp::Combine } else { PathOp::Join };
                }
                let mut clip = c.path.subpaths;
                if let Some(first) = clip.first_mut() {
                    first.op = PathOp::Intersect;
                }
                subpaths.extend(clip);
                let mut p = Path::new(subpaths);
                p.fill_rule = own.fill_rule;
                Some(VectorMask::new(p))
            }
        };
        self.flush_all();
        let mut l = Layer::new("Gradient Fill", LayerContent::Fill(Fill::Solid(Color::BLACK)));
        l.vector_mask = mask;
        l.blend = to_blend(props.blend_mode);
        let frame = photosplice_compose::fill_frame(&l, self.bounds());
        l.content = LayerContent::Fill(super::shading::fill(g, frame));
        self.push_layer(l);
        true
    }

    // ---- flushing ----

    fn flush_all(&mut self) {
        self.flush_text();
        self.flush_shape();
        self.flush_segment();
    }

    fn flush_text(&mut self) {
        let Some(t) = self.text.take() else { return };
        let len = t.text.len();
        let mut style = t.style;
        style.size_pt = (t.size_px * 72.0 / f64::from(self.dpi)) as f32;
        style.leading_pt = t.leading.map(|l| (l * 72.0 / f64::from(self.dpi)) as f32);
        let mut layer = TextLayer {
            text: t.text.clone(),
            transform: Affine::translate(t.origin.x, t.origin.y).mul(&Affine::rotate(t.angle)),
            runs: vec![TextRun { len, style }],
            shape: TextShape::Point,
            cache: Some(t.cache),
            ..Default::default()
        };
        layer.sync_summary();
        let name: String = t.text.chars().take_while(|c| *c != '\r').take(40).collect();
        let mut l = Layer::new(if name.trim().is_empty() { "Text".to_string() } else { name }, LayerContent::Text(layer));
        l.vector_mask = if t.clip.is_some() { self.vector_mask() } else { None };
        self.push_layer(l);
    }

    fn flush_shape(&mut self) {
        let Some(mut s) = self.shape.take() else { return };
        s.shape.cache = Some(photosplice_vector::render_shape(&s.shape, self.fmt, self.bounds()));
        let name = match (&s.shape.fill, &s.shape.stroke) {
            (Some(_), Some(_)) | (Some(_), None) => "Shape",
            (None, _) => "Stroke",
        };
        let mut l = Layer::new(name, LayerContent::Shape(s.shape));
        l.blend = s.blend;
        l.vector_mask = if s.clip.is_some() { self.vector_mask() } else { None };
        self.push_layer(l);
    }

    fn flush_segment(&mut self) {
        let Some(mut seg) = self.segment.take() else { return };
        let mut r = Renderer::new(&mut seg.ctx, self.global);
        for _ in 0..seg.clips {
            Device::pop_clip(&mut r);
        }
        for _ in 1..self.clips.len().saturating_sub(seg.clips) {
            Device::pop_clip(&mut r);
        }
        drop(r);
        if let Some((rect, rgba)) = finish_pixels(seg.ctx, 0, 0)
            && let Some(l) = self.pixel_layer("Pixels", rect, &rgba)
        {
            self.push_layer(l);
        }
    }

    /// A pixel layer from straight RGBA8 pixels at `rect`, trimmed to what they cover.
    fn pixel_layer(&self, name: &str, rect: Rect, rgba: &[u8]) -> Option<Layer> {
        let (rect, rgba) = trim(rect, rgba)?;
        let rect = rect.intersect(&self.bounds());
        if rect.is_empty() {
            return None;
        }
        Some(Layer::new(name, LayerContent::Raster(Surface::from_interleaved(self.fmt, rect, &rgba))))
    }

    /// Starts (or continues) drawing into a pixel segment: the current clips are replayed.
    fn segment(&mut self) -> &mut Segment {
        if self.segment.is_none() {
            self.flush_text();
            self.flush_shape();
            let mut ctx = RenderContext::new(self.w.min(u32::from(u16::MAX)) as u16, self.h.min(u32::from(u16::MAX)) as u16);
            let mut r = Renderer::new(&mut ctx, self.global);
            for (c, _) in self.clips.iter().skip(1) {
                Device::push_clip_path(&mut r, c);
            }
            drop(r);
            self.segment = Some(Segment { ctx, clips: 0 });
        }
        // Just created above when missing.
        #[allow(clippy::unwrap_used)]
        self.segment.as_mut().unwrap()
    }

    fn with_segment(&mut self, f: impl FnOnce(&mut Renderer<'_>)) {
        self.pixel_items += 1;
        let global = self.global;
        let seg = self.segment();
        let mut r = Renderer::new(&mut seg.ctx, global);
        f(&mut r);
    }

    /// Draws `f` alone into pixels covering `area` (document pixels), with the current clips.
    fn isolated(&self, area: kurbo::Rect, f: impl FnOnce(&mut Renderer<'_>, KAffine)) -> Option<(Rect, Vec<u8>)> {
        let page = kurbo::Rect::new(0.0, 0.0, f64::from(self.w), f64::from(self.h));
        let area = area.intersect(page);
        if !(area.width() > 0.0 && area.height() > 0.0) {
            return None;
        }
        let (x0, y0) = (area.x0.floor().max(0.0), area.y0.floor().max(0.0));
        let (x1, y1) = (area.x1.ceil().min(f64::from(self.w)), area.y1.ceil().min(f64::from(self.h)));
        let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
        if w == 0 || h == 0 || w > u32::from(u16::MAX) || h > u32::from(u16::MAX) {
            return None;
        }
        let shift = KAffine::translate((-x0, -y0));
        let mut ctx = RenderContext::new(w as u16, h as u16);
        let mut r = Renderer::new(&mut ctx, self.global);
        let mut pushed = 0;
        for (c, _) in &self.clips {
            let mut path = c.path.clone();
            path.apply_affine(shift);
            Device::push_clip_path(&mut r, &ClipPath { path, fill: c.fill });
            pushed += 1;
        }
        f(&mut r, shift);
        for _ in 0..pushed {
            Device::pop_clip(&mut r);
        }
        drop(r);
        finish_pixels(ctx, x0 as i32, y0 as i32)
    }

    // ---- items ----

    fn add_path(&mut self, path: &BezPath, props: &DrawProps<'_>, mode: &DrawMode, color: [u8; 4]) {
        let mut dev = path.clone();
        dev.apply_affine(props.transform);
        let blend = to_blend(props.blend_mode);
        let clip = self.clip_key();
        let rgba = Color::rgba(f32::from(color[0]) / 255.0, f32::from(color[1]) / 255.0, f32::from(color[2]) / 255.0, f32::from(color[3]) / 255.0);
        self.flush_text();
        self.flush_segment();
        match mode {
            DrawMode::Fill(rule) => {
                // An opaque fill of the colour being collected joins the shape.
                if let Some(s) = self.shape.as_mut()
                    && s.joinable == Some(color)
                    && s.blend == blend
                    && s.clip == clip
                    && s.shape.stroke.is_none()
                    && *rule == PdfFillRule::NonZero
                {
                    let mut p = bez_to_path(&dev);
                    for (j, sp) in p.subpaths.iter_mut().enumerate() {
                        sp.op = if j == 0 { PathOp::Combine } else { PathOp::Join };
                    }
                    s.shape.path.subpaths.extend(p.subpaths);
                    s.last = Some(dev);
                    s.paths += 1;
                    return;
                }
                self.flush_shape();
                let mut p = bez_to_path(&dev);
                p.fill_rule = fill_rule(*rule);
                let shape = ShapeLayer { path: p, fill: Some(Fill::Solid(rgba)), ..Default::default() };
                let joinable = (color[3] == 255 && *rule == PdfFillRule::NonZero).then_some(color);
                self.shape = Some(ShapeAcc { shape, blend, clip, joinable, last: Some(dev), paths: 1 });
            }
            DrawMode::Stroke(sp) | DrawMode::FillAndStroke(_, sp) => {
                let stroke = shape_stroke(sp, props.transform, rgba);
                // `B`: the stroke of the path just filled joins its fill.
                if let Some(s) = self.shape.as_mut()
                    && s.paths == 1
                    && s.shape.stroke.is_none()
                    && s.blend == blend
                    && s.clip == clip
                    && s.last.as_ref().is_some_and(|l| l.elements() == dev.elements())
                {
                    s.shape.stroke = Some(stroke);
                    s.joinable = None;
                    return;
                }
                self.flush_shape();
                let shape = ShapeLayer { path: bez_to_path(&dev), fill: None, stroke: Some(stroke), ..Default::default() };
                self.shape = Some(ShapeAcc { shape, blend, clip, joinable: None, last: Some(dev), paths: 1 });
            }
            DrawMode::Invisible => {}
        }
    }

    fn add_text<'a>(&mut self, run: &GlyphRun<'_, 'a>, props: &DrawProps<'a>, mode: &DrawMode, color: [u8; 4]) {
        let glyphs = run.glyphs();
        let Some(first) = glyphs.first() else { return };
        let Glyph::Outline(fo) = &**first else { return };
        // Size, angle and horizontal scale from the em box (outlines are 1000 units per em).
        let m = props.transform * first.transform();
        let up = m * kurbo::Point::new(0.0, 1000.0) - m * kurbo::Point::ZERO;
        let right = m * kurbo::Point::new(1000.0, 0.0) - m * kurbo::Point::ZERO;
        let size_px = up.hypot();
        if !(size_px > 0.05 && size_px.is_finite()) {
            return;
        }
        let angle = right.y.atan2(right.x);
        let hscale = (right.hypot() / size_px) as f32;
        let font_key = fo.font_cache_key();
        let face = self.font_family(fo);
        let ps = self.postscript_name(fo);
        let rgba = Color::rgba(f32::from(color[0]) / 255.0, f32::from(color[1]) / 255.0, f32::from(color[2]) / 255.0, f32::from(color[3]) / 255.0);
        let style = CharStyle {
            font_family: face.as_ref().map(|f| f.0.clone()).unwrap_or_default(),
            postscript_name: ps,
            size_pt: (size_px * 72.0 / f64::from(self.dpi)) as f32,
            color: rgba,
            weight: face.as_ref().map_or(400, |f| f.1),
            italic: face.as_ref().is_some_and(|f| f.2),
            horizontal_scale: if (hscale - 1.0).abs() > 0.01 { hscale } else { 1.0 },
            ..Default::default()
        };
        // The run's text, with spaces where the PDF positions words apart instead of drawing one.
        let (u, n) = (kurbo::Vec2::new(angle.cos(), angle.sin()), kurbo::Vec2::new(-angle.sin(), angle.cos()));
        let origin = m * kurbo::Point::ZERO;
        let clip = self.clip_key();
        self.flush_shape();
        self.flush_segment();
        // Continue the line, start the paragraph's next line, or start a new layer.
        let join = self.text.as_ref().map_or(Join::New, |t| {
            let like = same_style(&t.style, &style, font_key) && (t.size_px - size_px).abs() < 0.02 * size_px && (t.angle - angle).abs() < 0.01 && t.clip == clip;
            let d = origin - kurbo::Point::new(t.line.x, t.line.y);
            let (along, across) = (d.dot(u), d.dot(n));
            let left = (origin - kurbo::Point::new(t.origin.x, t.origin.y)).dot(u);
            if !like {
                Join::New
            } else if across.abs() < 0.2 * size_px && along > t.end - 0.6 * size_px && along < t.end + 4.0 * size_px {
                Join::Line
            } else if left.abs() < 0.5 * size_px && across > 0.8 * size_px && across < 2.5 * size_px && t.leading.is_none_or(|l| (across - l).abs() < 0.05 * l) {
                Join::NextLine(across)
            } else {
                Join::New
            }
        });
        if join == Join::New {
            self.flush_text();
        }
        if let (Join::NextLine(leading), Some(t)) = (join, self.text.as_mut()) {
            t.text.push('\r');
            t.line = Point::new(origin.x, origin.y);
            t.end = 0.0;
            t.leading = Some(leading);
        }
        let continues = join == Join::Line;
        let base = self.text.as_ref().map_or(origin, |t| kurbo::Point::new(t.line.x, t.line.y));
        let mut text = String::new();
        let mut end = self.text.as_ref().map_or(0.0, |t| t.end);
        let mut first_glyph = !continues;
        for g in glyphs {
            let gm = props.transform * g.transform();
            let along = (gm * kurbo::Point::ZERO - base).dot(u);
            let s = match g.as_unicode() {
                Some(BfString::Char(c)) => c.to_string(),
                Some(BfString::String(s)) => s,
                None => String::new(),
            };
            let prev_space = text.ends_with(char::is_whitespace) || (text.is_empty() && self.text.as_ref().is_some_and(|t| t.text.ends_with(char::is_whitespace)));
            if !first_glyph && along - end > 0.25 * size_px && !prev_space && !s.starts_with(char::is_whitespace) {
                text.push(' ');
            }
            first_glyph = false;
            text.push_str(&s);
            let adv = match &**g {
                Glyph::Outline(o) => o.advance_width().map_or(0.5, |a| f64::from(a) / 1000.0),
                Glyph::Type3(_) => 0.5,
            };
            end = along + adv * size_px * f64::from(hscale);
        }
        // The PDF's own glyphs, drawn into the layer's pixels.
        let area = glyphs
            .iter()
            .filter_map(|g| match &**g {
                Glyph::Outline(o) => {
                    let mut p = o.outline();
                    p.apply_affine(props.transform * g.transform());
                    Some(p.bounding_box())
                }
                Glyph::Type3(_) => None,
            })
            .reduce(|a, b| a.union(b))
            .map(|r| r.inflate(2.0, 2.0));
        let pixels = area.and_then(|a| {
            self.isolated(a, |r, shift| {
                let p = DrawProps { transform: shift * props.transform, ..props.clone() };
                Device::draw_glyph_run(r, run, p, mode);
            })
        });
        let acc = self.text.get_or_insert_with(|| TextAcc {
            text: String::new(),
            style: style.clone(),
            origin: Point::new(origin.x, origin.y),
            line: Point::new(origin.x, origin.y),
            leading: None,
            angle,
            size_px,
            end: 0.0,
            cache: Surface::new(self.fmt),
            clip,
        });
        acc.text.push_str(&text);
        acc.end = end;
        if let Some((rect, rgba)) = pixels {
            over(&mut acc.cache, rect, &rgba);
        }
    }

    /// The family, weight and italic a type layer names for this font: the installed face when
    /// there is one, else the PDF's embedded font registered with the text engine, else the
    /// family the PostScript name suggests (cached per font).
    fn font_family(&mut self, g: &hayro::hayro_interpret::font::OutlineGlyph) -> Option<(String, u16, bool)> {
        let key = g.font_cache_key();
        if let Some(f) = self.fonts.get(&key) {
            return f.clone();
        }
        let data = g.font_data();
        let ps = self.postscript_name(g);
        let mut engine = photosplice_text::shared().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let guess = ps.as_ref().map(|ps| engine.fonts.resolve_postscript(ps));
        let installed = guess.as_ref().filter(|r| r.exact || engine.fonts.has_family(&r.family)).map(|r| (r.family.clone(), r.weight, r.italic));
        let face = installed.or_else(|| {
            let d = data.as_ref()?;
            let bytes: &[u8] = d.data.as_ref().as_ref();
            let family = engine.fonts.register_font_data(bytes.to_vec()).into_iter().next()?;
            let weight = d.weight.map(|w| w.clamp(100, 900) as u16).or(guess.as_ref().map(|r| r.weight)).unwrap_or(400);
            Some((family, weight, d.is_italic || guess.as_ref().is_some_and(|r| r.italic)))
        });
        let face = face.or_else(|| guess.map(|r| (r.family, r.weight, r.italic)));
        drop(engine);
        self.fonts.insert(key, face.clone());
        face
    }

    /// The font's PostScript name, without a subset prefix: from its data, else its `BaseFont`.
    fn postscript_name(&self, g: &hayro::hayro_interpret::font::OutlineGlyph) -> Option<String> {
        g.font_data()
            .and_then(|d| d.postscript_name)
            .or_else(|| self.font_names.get(&g.font_cache_key()).cloned())
            .map(|n| strip_subset(&n).to_string())
            .filter(|n| !n.is_empty())
    }

    fn close_frame(&mut self) {
        self.flush_all();
        let Some(frame) = self.frames.pop() else { return };
        match frame.group {
            Some((opacity, blend)) if !frame.layers.is_empty() => {
                let mut g = Layer::group("Group", frame.layers);
                g.opacity = opacity.clamp(0.0, 1.0);
                g.blend = if blend == BlendMode::Normal { BlendMode::Normal } else { blend };
                self.push_layer(g);
            }
            _ => {
                if let Some(parent) = self.frames.last_mut() {
                    parent.layers.extend(frame.layers);
                }
            }
        }
    }
}

impl<'a> Device<'a> for LayerBuilder<'_> {
    fn draw_path(&mut self, path: &BezPath, props: DrawProps<'a>, draw_mode: &DrawMode) {
        match (&props.paint, &props.soft_mask) {
            (Paint::Color(c), None) if self.raster_groups == 0 && !self.full() => {
                let color = c.to_rgba().to_rgba8();
                self.add_path(path, &props, draw_mode, color);
            }
            // An axial or radial shading a Photoshop gradient can hold: a Gradient Fill layer.
            (Paint::Pattern(p), None)
                if self.raster_groups == 0
                    && !self.full()
                    && let (hayro::hayro_interpret::pattern::Pattern::Shading(sp), DrawMode::Fill(rule)) = (&**p, draw_mode)
                    && self.gradient_layer(path, &props, rule, sp) => {}
            _ => self.with_segment(|r| Device::draw_path(r, path, props, draw_mode)),
        }
    }

    fn push_clip_path(&mut self, clip_path: &ClipPath) {
        if let Some(seg) = self.segment.as_mut() {
            Device::push_clip_path(&mut Renderer::new(&mut seg.ctx, self.global), clip_path);
            seg.clips += 1;
        }
        let page = kurbo::Rect::new(0.0, 0.0, f64::from(self.w), f64::from(self.h)).inflate(-0.5, -0.5);
        let id = if self.clips.is_empty() || covers(&clip_path.path, page) {
            0
        } else {
            self.next_clip_id += 1;
            self.next_clip_id
        };
        self.clips.push((clip_path.clone(), id));
    }

    fn push_transparency_group(&mut self, opacity: f32, mask: Option<SoftMask<'a>>, blend_mode: PdfBlend) {
        if self.raster_groups > 0 || mask.is_some() {
            self.raster_groups += 1;
            self.with_segment(|r| Device::push_transparency_group(r, opacity, mask, blend_mode));
            return;
        }
        self.flush_all();
        let blend = to_blend(blend_mode);
        let group = (opacity < 0.999 || blend != BlendMode::Normal).then_some((opacity, blend));
        self.frames.push(Frame { layers: Vec::new(), group });
    }

    fn draw_glyph_run(&mut self, glyph_run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, draw_mode: &DrawMode) {
        let editable = self.raster_groups == 0
            && !self.full()
            && props.soft_mask.is_none()
            && matches!(draw_mode, DrawMode::Fill(_))
            && glyph_run.glyphs().iter().all(|g| matches!(&**g, Glyph::Outline(_)));
        match &props.paint {
            Paint::Color(c) if editable => {
                let color = c.to_rgba().to_rgba8();
                self.add_text(glyph_run, &props, draw_mode, color);
            }
            _ if matches!(draw_mode, DrawMode::Invisible) => {}
            _ => self.with_segment(|r| Device::draw_glyph_run(r, glyph_run, props, draw_mode)),
        }
    }

    fn draw_image(&mut self, image: Image<'a, '_>, props: ImageDrawProps<'a>) {
        if self.raster_groups > 0 || self.full() {
            self.with_segment(|r| Device::draw_image(r, image, props));
            return;
        }
        self.flush_all();
        // The unit square, placed: where the image lands.
        let (iw, ih) = (f64::from(image.width().max(1)), f64::from(image.height().max(1)));
        let area = (props.transform * kurbo::Rect::new(0.0, 0.0, iw, ih).to_path(0.1)).bounding_box();
        let pixels = self.isolated(area, |r, shift| {
            let p = ImageDrawProps { transform: shift * props.transform, ..props.clone() };
            Device::draw_image(r, image, p);
        });
        if let Some((rect, rgba)) = pixels
            && let Some(mut l) = self.pixel_layer("Image", rect, &rgba)
        {
            l.blend = to_blend(props.blend_mode);
            self.push_layer(l);
        }
    }

    fn pop_clip(&mut self) {
        if let Some(seg) = self.segment.as_mut() {
            if seg.clips > 0 {
                Device::pop_clip(&mut Renderer::new(&mut seg.ctx, self.global));
                seg.clips -= 1;
            } else if self.raster_groups > 0 {
                // Inside a group drawn as pixels the segment can't end: pop the replayed clip.
                Device::pop_clip(&mut Renderer::new(&mut seg.ctx, self.global));
            } else {
                // The segment began inside this clip: it ends here.
                self.flush_segment();
            }
        }
        // A layer collecting under this clip is finished before the clip goes.
        let top = self.clips.last().map(|c| c.1).filter(|id| *id != 0);
        if top.is_some() && self.text.as_ref().is_some_and(|t| t.clip == top) {
            self.flush_text();
        }
        if top.is_some() && self.shape.as_ref().is_some_and(|s| s.clip == top) {
            self.flush_shape();
        }
        self.clips.pop();
    }

    fn pop_transparency_group(&mut self) {
        if self.raster_groups > 0 {
            self.raster_groups -= 1;
            self.with_segment(|r| Device::pop_transparency_group(r));
            return;
        }
        self.close_frame();
    }
}

/// Whether `path` is a rectangle (straight, axis-aligned edges) containing `area`: a clip that
/// clips nothing there.
fn covers(path: &BezPath, area: kurbo::Rect) -> bool {
    let pts: Vec<kurbo::Point> = path
        .elements()
        .iter()
        .filter_map(|e| match e {
            PathEl::MoveTo(p) | PathEl::LineTo(p) => Some(*p),
            _ => None,
        })
        .collect();
    let curved = path.elements().iter().any(|e| matches!(e, PathEl::QuadTo(..) | PathEl::CurveTo(..)));
    let axis = pts.windows(2).all(|w| (w[0].x - w[1].x).abs() < 1e-3 || (w[0].y - w[1].y).abs() < 1e-3);
    let subpaths = path.elements().iter().filter(|e| matches!(e, PathEl::MoveTo(_))).count();
    !curved && axis && subpaths == 1 && (4..=5).contains(&pts.len()) && path.bounding_box().inflate(0.5, 0.5).contains_rect(area)
}

fn same_style(a: &CharStyle, b: &CharStyle, _font: u128) -> bool {
    a.font_family == b.font_family && a.postscript_name == b.postscript_name && a.color.to_rgba8() == b.color.to_rgba8() && a.horizontal_scale == b.horizontal_scale
}

/// Every font the page can draw with (its resources and its form XObjects'), by hayro's cache
/// key for the font (a hash of the font dictionary) → its `BaseFont`.
fn font_names(page: &Page<'_>) -> HashMap<u128, String> {
    use hayro::hayro_interpret::CacheKey;
    use hayro::hayro_syntax::object::{Dict, Name, Stream};
    fn walk(fonts: &Dict<'_>, xobjects: &Dict<'_>, depth: u32, out: &mut HashMap<u128, String>) {
        for key in fonts.keys() {
            if let Some(f) = fonts.get::<Dict<'_>>(key.as_ref())
                && let Some(name) = f.get::<Name<'_>>(b"BaseFont")
            {
                out.insert(f.cache_key(), String::from_utf8_lossy(name.as_ref()).into_owned());
            }
        }
        if depth >= 8 {
            return;
        }
        for key in xobjects.keys() {
            if let Some(x) = xobjects.get::<Stream<'_>>(key.as_ref())
                && let Some(res) = x.dict().get::<Dict<'_>>(b"Resources")
            {
                let fonts = res.get::<Dict<'_>>(b"Font").unwrap_or_default();
                let xo = res.get::<Dict<'_>>(b"XObject").unwrap_or_default();
                walk(&fonts, &xo, depth + 1, out);
            }
        }
    }
    let mut out = HashMap::new();
    let r = page.resources();
    walk(&r.fonts, &r.x_objects, 0, &mut out);
    out
}

/// `ABCDEF+Name` → `Name` (PDF subset prefixes are six capitals and a plus).
fn strip_subset(name: &str) -> &str {
    match name.split_once('+') {
        Some((p, rest)) if p.len() == 6 && p.bytes().all(|b| b.is_ascii_uppercase()) => rest,
        _ => name,
    }
}

fn fill_rule(r: PdfFillRule) -> FillRule {
    match r {
        PdfFillRule::NonZero => FillRule::NonZero,
        PdfFillRule::EvenOdd => FillRule::EvenOdd,
    }
}

fn to_blend(b: PdfBlend) -> BlendMode {
    match b {
        PdfBlend::Normal => BlendMode::Normal,
        PdfBlend::Multiply => BlendMode::Multiply,
        PdfBlend::Screen => BlendMode::Screen,
        PdfBlend::Overlay => BlendMode::Overlay,
        PdfBlend::Darken => BlendMode::Darken,
        PdfBlend::Lighten => BlendMode::Lighten,
        PdfBlend::ColorDodge => BlendMode::ColorDodge,
        PdfBlend::ColorBurn => BlendMode::ColorBurn,
        PdfBlend::HardLight => BlendMode::HardLight,
        PdfBlend::SoftLight => BlendMode::SoftLight,
        PdfBlend::Difference => BlendMode::Difference,
        PdfBlend::Exclusion => BlendMode::Exclusion,
        PdfBlend::Hue => BlendMode::Hue,
        PdfBlend::Saturation => BlendMode::Saturation,
        PdfBlend::Color => BlendMode::Color,
        PdfBlend::Luminosity => BlendMode::Luminosity,
    }
}

/// A PDF stroke in document pixels: the width scaled by the transform, dashes in widths.
fn shape_stroke(sp: &StrokeProps, t: KAffine, color: Color) -> ShapeStroke {
    let k = t.determinant().abs().sqrt();
    let width = (f64::from(sp.line_width) * k).max(1.0) as f32;
    let user = sp.line_width.max(f32::EPSILON);
    ShapeStroke {
        width,
        paint: Fill::Solid(color),
        opacity: 1.0,
        align: StrokeAlign::Center,
        cap: match sp.line_cap {
            kurbo::Cap::Butt => LineCap::Butt,
            kurbo::Cap::Round => LineCap::Round,
            kurbo::Cap::Square => LineCap::Square,
        },
        join: match sp.line_join {
            kurbo::Join::Miter => LineJoin::Miter,
            kurbo::Join::Round => LineJoin::Round,
            kurbo::Join::Bevel => LineJoin::Bevel,
        },
        miter_limit: sp.miter_limit,
        dashes: sp.dash_array.iter().map(|d| d / user).collect(),
        dash_offset: sp.dash_offset / user,
    }
}

/// A kurbo path (document pixels) as our cubic path: every subpath filled together.
fn bez_to_path(p: &BezPath) -> Path {
    let mut subs: Vec<Subpath> = Vec::new();
    let mut cur: Option<Subpath> = None;
    let pt = |p: kurbo::Point| Point::new(p.x, p.y);
    let finish = |s: Option<Subpath>, subs: &mut Vec<Subpath>| {
        if let Some(mut s) = s
            && !s.knots.is_empty()
        {
            if s.closed && s.knots.len() > 1 {
                // A closing segment that returns to the start: fold its last knot into the first.
                let (Some(first), Some(last)) = (s.knots.first().copied(), s.knots.last().copied()) else { return };
                if (first.anchor.x - last.anchor.x).abs() < 1e-6 && (first.anchor.y - last.anchor.y).abs() < 1e-6 {
                    s.knots.pop();
                    if let Some(f) = s.knots.first_mut() {
                        f.in_ctrl = last.in_ctrl;
                    }
                }
            }
            let op = if subs.is_empty() { PathOp::Combine } else { PathOp::Join };
            subs.push(s.with_op(op));
        }
    };
    for el in p.elements() {
        match *el {
            PathEl::MoveTo(a) => {
                finish(cur.take(), &mut subs);
                cur = Some(Subpath { closed: false, knots: vec![Knot::corner(a.x, a.y)], op: PathOp::Combine });
            }
            PathEl::LineTo(a) => {
                if let Some(s) = cur.as_mut() {
                    s.knots.push(Knot::corner(a.x, a.y));
                }
            }
            PathEl::QuadTo(c, a) => {
                if let Some(s) = cur.as_mut()
                    && let Some(prev) = s.knots.last_mut()
                {
                    let p0 = kurbo::Point::new(prev.anchor.x, prev.anchor.y);
                    let c1 = p0 + (c - p0) * (2.0 / 3.0);
                    let c2 = a + (c - a) * (2.0 / 3.0);
                    prev.out_ctrl = pt(c1);
                    s.knots.push(Knot { anchor: pt(a), in_ctrl: pt(c2), out_ctrl: pt(a), smooth: false });
                }
            }
            PathEl::CurveTo(c1, c2, a) => {
                if let Some(s) = cur.as_mut()
                    && let Some(prev) = s.knots.last_mut()
                {
                    prev.out_ctrl = pt(c1);
                    s.knots.push(Knot { anchor: pt(a), in_ctrl: pt(c2), out_ctrl: pt(a), smooth: false });
                }
            }
            PathEl::ClosePath => {
                if let Some(s) = cur.as_mut() {
                    s.closed = true;
                }
            }
        }
    }
    finish(cur, &mut subs);
    Path::new(subs)
}

/// Renders the context's drawing and returns straight RGBA8 pixels placed at (`x`, `y`).
fn finish_pixels(mut ctx: RenderContext, x: i32, y: i32) -> Option<(Rect, Vec<u8>)> {
    ctx.flush();
    let (w, h) = (ctx.width(), ctx.height());
    let mut pixmap = Pixmap::new(w, h);
    ctx.render_with(
        &mut pixmap,
        &mut vello_cpu::Resources::default(),
        vello_cpu::RasterizerSettings { target_init: vello_cpu::TargetInit::Clear(vello_cpu::color::palette::css::TRANSPARENT), ..Default::default() },
    );
    let mut rgba = pixmap.data_as_u8_slice().to_vec();
    if rgba.iter().skip(3).step_by(4).all(|a| *a == 0) {
        return None;
    }
    for q in rgba.chunks_exact_mut(4) {
        let a = u32::from(q[3]);
        if a > 0 && a < 255 {
            for c in &mut q[..3] {
                *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    Some((Rect::from_xywh(x, y, u32::from(w), u32::from(h)), rgba))
}

/// Trims transparent margins.
fn trim(rect: Rect, rgba: &[u8]) -> Option<(Rect, Vec<u8>)> {
    let (w, h) = (rect.width() as usize, rect.height() as usize);
    if w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return None;
    }
    let alpha = |x: usize, y: usize| rgba.get((y * w + x) * 4 + 3).copied().unwrap_or(0);
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for y in 0..h {
        for x in 0..w {
            if alpha(x, y) != 0 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let mut out = Vec::with_capacity((x1 - x0) * (y1 - y0) * 4);
    for y in y0..y1 {
        out.extend_from_slice(rgba.get((y * w + x0) * 4..(y * w + x1) * 4)?);
    }
    Some((Rect::new(rect.x0 + x0 as i32, rect.y0 + y0 as i32, rect.x0 + x1 as i32, rect.y0 + y1 as i32), out))
}

/// Draws straight RGBA8 `rgba` at `rect` over what `surface` already holds there.
fn over(surface: &mut Surface, rect: Rect, rgba: &[u8]) {
    let n = rect.width() as usize * rect.height() as usize;
    let mut below = vec![[0u8; 4]; n];
    surface.read_rgba8_into(rect, &mut below);
    let mut out = Vec::with_capacity(n * 4);
    for (b, t) in below.iter().zip(rgba.chunks_exact(4)) {
        let (ta, ba) = (f32::from(t[3]) / 255.0, f32::from(b[3]) / 255.0);
        let a = ta + ba * (1.0 - ta);
        if a <= 0.0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        for c in 0..3 {
            let v = (f32::from(t[c]) * ta + f32::from(b[c]) * ba * (1.0 - ta)) / a;
            out.push(v.round().clamp(0.0, 255.0) as u8);
        }
        out.push((a * 255.0).round() as u8);
    }
    surface.write_interleaved(rect, &out);
}
