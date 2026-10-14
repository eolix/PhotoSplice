//! Rasterizes a [`TextLayout`] into a document-space [`Surface`] of any pixel format.

use photosuite_color::{Color, PixelFormat};
use photosuite_doc::text::AntiAlias;
use photosuite_geom::{Affine, Rect};
use photosuite_raster::Surface;
use skrifa::instance::{LocationRef, NormalizedCoord, Size};
use skrifa::outline::{DrawSettings, HintingInstance, HintingOptions, SmoothMode, Target};
use skrifa::{GlyphId, MetadataProvider};

use crate::layout::{GlyphOrient, PlacedGlyph, TextLayout};
use crate::raster::{Bounds, Coverage, LineSink, Pen, Xform, rect_to};
use crate::warp::Warp;

/// Faux-italic slant in degrees (Photoshop-like).
pub const FAUX_ITALIC_DEG: f32 = 12.0;
/// Faux-bold dilation radius as a fraction of the font size, and its floor in pixels: Reference
/// thickens strokes only slightly (fitted on `layer_effects.psd` at 62.5 px and
/// `faux-bold-italic-runs.psd` at 20 px, both about 0.2 px a side); the run widens mostly through
/// its advances ([`FAUX_BOLD_ADVANCE`]).
pub const FAUX_BOLD_RADIUS: f32 = 0.003;
pub const FAUX_BOLD_MIN_RADIUS_PX: f32 = 0.2;
/// Dilation radius (fraction of the font size) standing in for a bold face the family lacks.
const SYNTHETIC_BOLD_RADIUS: f32 = 0.018;
/// Faux bold widens every glyph's advance by this fraction of the font size (55/2048 em, from
/// the pen positions Reference stores in `Txt2`: `faux-bold-italic-runs.psd` at 20 px,
/// `layer_effects.psd` at 62.5 px).
pub const FAUX_BOLD_ADVANCE: f32 = 55.0 / 2048.0;
/// The same with faux italic too (59/2048 em, same oracle).
pub const FAUX_BOLD_ITALIC_ADVANCE: f32 = 59.0 / 2048.0;
/// Strong anti-aliasing: coverage raised to this power (Photoshop's Strong is heavier than its
/// Smooth; fitted on `antialias-strong.psd`, error 38 → 9.5 against Photoshop's pixels).
const STRONG_GAMMA: f32 = 0.58;
/// Sharp type at this size (px) and above sets each glyph on a whole pixel, below it glyphs
/// keep fractional positions (Photoshop corpus: 32, 44 and 64 px snap; 13-30 px don't).
const SNAP_X_PX: f32 = 31.0;
/// Largest raster we produce (pixels), as a guard against absurd sizes.
const MAX_PIXELS: u64 = 256 * 1024 * 1024;

/// Rendered text: pixels in document space plus the covered rectangle.
pub struct Rendered {
    pub surface: Surface,
    pub rect: Rect,
}

/// Bends text-space lines with a [`Warp`], then maps them through `post` into `inner`.
struct WarpSink<'a, S: LineSink> {
    inner: &'a mut S,
    warp: &'a Warp,
    post: Xform,
}

impl<S: LineSink> LineSink for WarpSink<'_, S> {
    fn line(&mut self, p0: (f64, f64), p1: (f64, f64)) {
        let len = ((p1.0 - p0.0).powi(2) + (p1.1 - p0.1).powi(2)).sqrt();
        let n = ((len / self.warp.max_segment()).ceil() as usize).clamp(1, 256);
        let map = |t: f64| {
            let (x, y) = self.warp.apply(p0.0 + (p1.0 - p0.0) * t, p0.1 + (p1.1 - p0.1) * t);
            self.post.apply(x, y)
        };
        let mut a = map(0.0);
        for i in 1..=n {
            let b = map(i as f64 / n as f64);
            self.inner.line(a, b);
            a = b;
        }
    }
}

/// How outlines meet the pixel grid, by anti-aliasing mode. Fitted on Photoshop's renders of the
/// same text (corpus `antialias-*.psd` and every Sharp file): Sharp is hinted with baselines on
/// whole pixels (error −80 % over 20 files), None is hinted for monochrome (mismatched pixels
/// 390 → 94); Crisp, Strong and Smooth match best unhinted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GridFit {
    Unhinted,
    Mono,
    Smooth,
}

impl GridFit {
    fn of(aa: AntiAlias) -> Self {
        match aa {
            AntiAlias::None => Self::Mono,
            AntiAlias::Sharp | AntiAlias::Windows | AntiAlias::WindowsLcd => Self::Smooth,
            AntiAlias::Crisp | AntiAlias::Strong | AntiAlias::Smooth => Self::Unhinted,
        }
    }
}

/// One outline command (font px, y up), as recorded from a hinted glyph.
#[derive(Clone, Copy, Debug)]
enum Seg {
    Move(f32, f32),
    Line(f32, f32),
    Quad(f32, f32, f32, f32),
    Cubic(f32, f32, f32, f32, f32, f32),
    Close,
}

#[derive(Default)]
struct SegRec(Vec<Seg>);

impl skrifa::outline::OutlinePen for SegRec {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.push(Seg::Move(x, y));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.push(Seg::Line(x, y));
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.0.push(Seg::Quad(cx, cy, x, y));
    }
    fn curve_to(&mut self, c0x: f32, c0y: f32, c1x: f32, c1y: f32, x: f32, y: f32) {
        self.0.push(Seg::Cubic(c0x, c0y, c1x, c1y, x, y));
    }
    fn close(&mut self) {
        self.0.push(Seg::Close);
    }
}

fn replay(segs: &[Seg], pen: &mut impl skrifa::outline::OutlinePen) {
    for s in segs {
        match *s {
            Seg::Move(x, y) => pen.move_to(x, y),
            Seg::Line(x, y) => pen.line_to(x, y),
            Seg::Quad(cx, cy, x, y) => pen.quad_to(cx, cy, x, y),
            Seg::Cubic(a, b, c, d, x, y) => pen.curve_to(a, b, c, d, x, y),
            Seg::Close => pen.close(),
        }
    }
}

/// Grid-fitted outlines for one raster: hinting runs each glyph's instructions, so every face
/// is set up and every glyph hinted once (5× slower than unhinted drawing without this).
#[derive(Default)]
struct Hinted {
    faces: Vec<(u32, Option<HintingInstance>)>,
    glyphs: std::collections::HashMap<(u32, u32), Option<Vec<Seg>>>,
}

impl Hinted {
    /// The hinted outline of glyph `id` of `layout` face `fi` (`None`: draw it unhinted).
    fn outline(&mut self, fit: GridFit, fi: u32, face: &crate::layout::GlyphFace, font: &skrifa::FontRef<'_>, id: u32) -> Option<&[Seg]> {
        if !self.faces.iter().any(|f| f.0 == fi) {
            let target = match fit {
                GridFit::Mono => Target::Mono,
                _ => Target::Smooth { mode: SmoothMode::Normal, symmetric_rendering: true, preserve_linear_metrics: false },
            };
            let coords: Vec<NormalizedCoord> = face.coords.iter().map(|&c| NormalizedCoord::from_bits(c)).collect();
            let h = HintingInstance::new(&font.outline_glyphs(), Size::new(face.size_px), LocationRef::new(&coords), HintingOptions::from(target)).ok();
            self.faces.push((fi, h));
        }
        let Self { faces, glyphs } = self;
        glyphs
            .entry((fi, id))
            .or_insert_with(|| {
                let h = faces.iter().find(|f| f.0 == fi).and_then(|f| f.1.as_ref())?;
                let outline = font.outline_glyphs().get(GlyphId::new(id))?;
                let mut rec = SegRec::default();
                outline.draw(DrawSettings::hinted(h, false), &mut rec).ok()?;
                Some(rec.0)
            })
            .as_deref()
    }
}

/// Draws every glyph and decoration of `layout` whose style colour is `color` (or all of them
/// when `color` is `None`) into `sink`, through `transform` (text space → sink space), bending
/// the outlines with `warp` first (in text space). Horizontal glyphs under an unscaled, unturned
/// `transform` are grid-fitted as `fit` asks (the sink's pixels must be the document's), from
/// `hinted`, which keeps them across calls.
fn draw(layout: &TextLayout, transform: &Xform, sink: &mut impl LineSink, only_color: Option<&Color>, warp: Option<&Warp>, fit: GridFit, hinted: &mut Hinted) {
    let [a, b, c, d, _, _] = transform.0;
    let upright = (a - 1.0).abs() < 1e-9 && b.abs() < 1e-9 && c.abs() < 1e-9 && (d - 1.0).abs() < 1e-9;
    let fit = if upright && warp.is_none() { fit } else { GridFit::Unhinted };
    for g in &layout.glyphs {
        let st = &layout.styles[g.style as usize];
        if only_color.is_some_and(|c| c != &st.color) {
            continue;
        }
        let face = &layout.faces[g.face as usize];
        let Ok(font) = skrifa::FontRef::from_index(face.font.data.as_ref(), face.font.index) else {
            continue;
        };
        let Some(outline) = font.outline_glyphs().get(GlyphId::new(g.id)) else {
            continue;
        };
        let coords: Vec<NormalizedCoord> = face.coords.iter().map(|&c| NormalizedCoord::from_bits(c)).collect();
        let mut glyph = glyph_xform(layout, g, face.skew_deg);
        let segs = if fit != GridFit::Unhinted && g.orient == GlyphOrient::Horizontal {
            // The baseline on a whole pixel (and, for large Sharp type, the glyph origin).
            let (dx, dy) = transform.apply(glyph.0[4], glyph.0[5]);
            glyph.0[5] += dy.round() - dy;
            if fit == GridFit::Smooth && face.size_px >= SNAP_X_PX {
                glyph.0[4] += dx.round() - dx;
            }
            hinted.outline(fit, g.face, face, &font, g.id)
        } else {
            None
        };
        let bold = st.faux_bold || face.embolden;
        let r = if face.embolden && !st.faux_bold {
            face.size_px * SYNTHETIC_BOLD_RADIUS
        } else {
            (face.size_px * FAUX_BOLD_RADIUS).max(FAUX_BOLD_MIN_RADIUS_PX)
        } as f64;
        let offsets: &[(f64, f64)] =
            if bold { &[(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0), (0.7, 0.7), (-0.7, -0.7), (0.7, -0.7), (-0.7, 0.7)] } else { &[(0.0, 0.0)] };
        for &(ox, oy) in offsets {
            let local = Xform([1.0, 0.0, 0.0, 1.0, ox * r, oy * r]).mul(&glyph);
            let settings = DrawSettings::unhinted(Size::new(face.size_px), LocationRef::new(&coords));
            match (segs, warp) {
                (Some(segs), _) => {
                    let mut pen = Pen::new(&mut *sink, transform.mul(&local));
                    replay(segs, &mut pen);
                    skrifa::outline::OutlinePen::close(&mut pen);
                }
                (None, Some(w)) => {
                    let mut ws = WarpSink { inner: &mut *sink, warp: w, post: *transform };
                    let mut pen = Pen::new(&mut ws, local);
                    if outline.draw(settings, &mut pen).is_ok() {
                        skrifa::outline::OutlinePen::close(&mut pen);
                    }
                }
                (None, None) => {
                    let mut pen = Pen::new(&mut *sink, transform.mul(&local));
                    if outline.draw(settings, &mut pen).is_ok() {
                        skrifa::outline::OutlinePen::close(&mut pen);
                    }
                }
            }
        }
    }
    for d in &layout.decorations {
        let st = &layout.styles[d.style as usize];
        if only_color.is_some_and(|c| c != &st.color) {
            continue;
        }
        match warp {
            Some(w) => {
                let mut ws = WarpSink { inner: &mut *sink, warp: w, post: *transform };
                rect_to(&mut ws, &Xform::IDENTITY, d.x0 as f64, d.y0 as f64, d.x1 as f64, d.y1 as f64);
            }
            None => rect_to(sink, transform, d.x0 as f64, d.y0 as f64, d.x1 as f64, d.y1 as f64),
        }
    }
}

/// Font units (scaled to px, y up) → text space for a placed glyph: scales, faux italic,
/// baseline shift, and the vertical-type orientation.
fn glyph_xform(layout: &TextLayout, g: &PlacedGlyph, face_skew_deg: f32) -> Xform {
    let Some(st) = layout.styles.get(g.style as usize) else { return Xform([1.0, 0.0, 0.0, -1.0, g.x as f64, g.y as f64]) };
    let hs = if st.horizontal_scale > 0.0 { st.horizontal_scale } else { 1.0 } as f64;
    let vs = if st.vertical_scale > 0.0 { st.vertical_scale } else { 1.0 } as f64;
    let skew_deg = if st.faux_italic { FAUX_ITALIC_DEG } else { 0.0 } + face_skew_deg;
    let skew = (skew_deg as f64).to_radians().tan() * vs;
    let shift = (st.baseline_shift_pt * layout.px_per_pt) as f64;
    let (x, y) = (g.x as f64, g.y as f64);
    match g.orient {
        GlyphOrient::Horizontal => Xform([hs, 0.0, skew, -vs, x, y - shift]),
        // Baseline shift moves upright glyphs in vertical type to the right.
        GlyphOrient::Upright => Xform([hs, 0.0, skew, -vs, x + shift, y]),
        // 90° clockwise: the glyph's up direction (and its baseline shift) points right.
        GlyphOrient::Rotated => Xform([0.0, 1.0, -1.0, 0.0, x, y]).mul(&Xform([hs, 0.0, skew, -vs, 0.0, -shift])),
        GlyphOrient::Path { cos, sin } => {
            let (c, s) = (cos as f64, sin as f64);
            Xform([c, s, -s, c, x, y]).mul(&Xform([hs, 0.0, skew, -vs, 0.0, -shift]))
        }
    }
}

/// The warp to apply to `layer` laid out as `layout` (None when it has no warp, `warpNone` or a
/// flat one), over the box its `TySh` records while that still holds its text (the box Reference
/// warps over), else the layout's line box.
pub fn layout_warp(layout: &TextLayout, layer: &photosuite_doc::TextLayer) -> Option<Warp> {
    let warp = layer.warp.as_ref()?;
    let stored = layer.psd_raw.as_deref().and_then(|d| crate::psd::tysh_text_box(d, &layer.text));
    Warp::new(warp, stored.or_else(|| layout.bounds())?)
}

/// Colour components in `format`'s model (without alpha).
fn color_in(format: &PixelFormat, c: &Color) -> Vec<f32> {
    let n = format.mode.color_channels();
    if c.mode == format.mode {
        c.c[..n].to_vec()
    } else {
        let [r, g, b] = c.to_rgb();
        let mut v = photosuite_raster::from_rgba(&PixelFormat { alpha: false, ..*format }, [r, g, b, 1.0]);
        v.truncate(n);
        v
    }
}

/// Document-space bounds of the drawn text (integer pixel rectangle).
pub fn ink_rect(layout: &TextLayout, transform: &Affine) -> Rect {
    ink_rect_warped(layout, transform, None)
}

fn rect_from_bounds([x0, y0, x1, y1]: [f64; 4]) -> Rect {
    if ![x0, y0, x1, y1].into_iter().all(f64::is_finite) {
        return Rect::EMPTY;
    }

    let min = f64::from(i32::MIN);
    let max = f64::from(i32::MAX);
    let (x0, y0, x1, y1) =
        ((x0.floor() - 1.0).clamp(min, max), (y0.floor() - 1.0).clamp(min, max), (x1.ceil() + 1.0).clamp(min, max), (y1.ceil() + 1.0).clamp(min, max));
    if x1 <= x0 || y1 <= y0 || x1 - x0 > MAX_PIXELS as f64 || y1 - y0 > MAX_PIXELS as f64 {
        return Rect::EMPTY;
    }

    Rect::new(x0 as i32, y0 as i32, x1 as i32, y1 as i32)
}

/// [`ink_rect`] of warped text.
pub fn ink_rect_warped(layout: &TextLayout, transform: &Affine, warp: Option<&Warp>) -> Rect {
    ink_rect_fitted(layout, transform, warp, GridFit::Unhinted, &mut Hinted::default())
}

fn ink_rect_fitted(layout: &TextLayout, transform: &Affine, warp: Option<&Warp>, fit: GridFit, hinted: &mut Hinted) -> Rect {
    let mut b = Bounds::default();
    draw(layout, &Xform(transform.m), &mut b, None, warp, fit, hinted);
    b.rect.map(rect_from_bounds).unwrap_or(Rect::EMPTY)
}

/// Rasterizes `layout` through `transform` (text space → document pixels). The result always
/// has an alpha channel; colour is written in `format`'s colour model and sample depth.
pub fn rasterize(layout: &TextLayout, transform: &Affine, format: PixelFormat, antialias: AntiAlias) -> Rendered {
    rasterize_warped(layout, transform, format, antialias, None)
}

/// [`rasterize`] with the glyph outlines bent by `warp` (Type › Warp Text).
pub fn rasterize_warped(layout: &TextLayout, transform: &Affine, format: PixelFormat, antialias: AntiAlias, warp: Option<&Warp>) -> Rendered {
    let format = PixelFormat { alpha: true, ..format };
    let fit = GridFit::of(antialias);
    let mut hinted = Hinted::default();
    let rect = ink_rect_fitted(layout, transform, warp, fit, &mut hinted);
    let (w, h) = (rect.width() as usize, rect.height() as usize);
    let mut surface = Surface::new(format);
    if w == 0 || h == 0 || (w as u64) * (h as u64) > MAX_PIXELS {
        return Rendered { surface, rect: Rect::new(0, 0, 0, 0) };
    }
    let n = format.mode.color_channels();
    let stride = n + 1;
    // Premultiplied accumulation.
    let mut acc = vec![0.0f32; w * h * stride];
    let xf = Xform([1.0, 0.0, 0.0, 1.0, -rect.x0 as f64, -rect.y0 as f64]).mul(&Xform(transform.m));
    let mut colors: Vec<Color> = Vec::new();
    for st in &layout.styles {
        if !colors.contains(&st.color) {
            colors.push(st.color);
        }
    }
    for c in &colors {
        let mut cov = Coverage::new(w, h);
        // A whole-pixel offset: grid fitting in `xf` space is grid fitting in the document.
        draw(layout, &xf, &mut cov, Some(c), warp, fit, &mut hinted);
        let cov = cov.finish();
        let comps = color_in(&format, c);
        let a = c.alpha.clamp(0.0, 1.0);
        for (i, &cv) in cov.iter().enumerate() {
            let cv = match antialias {
                AntiAlias::None => f32::from(u8::from(cv >= 0.5)),
                AntiAlias::Strong => cv.max(0.0).powf(STRONG_GAMMA),
                _ => cv,
            };
            let s = cv * a;
            if s <= 0.0 {
                continue;
            }
            let px = &mut acc[i * stride..(i + 1) * stride];
            let keep = 1.0 - s;
            for j in 0..n {
                px[j] = comps[j] * s + px[j] * keep;
            }
            px[n] = s + px[n] * keep;
        }
    }
    // Unpremultiply.
    for px in acc.chunks_exact_mut(stride) {
        let a = px[n];
        if a > 0.0 {
            for v in &mut px[..n] {
                *v = (*v / a).clamp(0.0, 1.0);
            }
        }
    }
    surface.write_region(rect, &acc);
    surface.prune();
    Rendered { surface, rect }
}

/// One element of a glyph outline in document space (see [`outlines`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathEl {
    MoveTo([f64; 2]),
    LineTo([f64; 2]),
    QuadTo([f64; 2], [f64; 2]),
    CurveTo([f64; 2], [f64; 2], [f64; 2]),
    Close,
}

/// Records outline commands through a text→document map (and an optional warp).
struct Recorder<'a> {
    glyph: Xform,
    post: Xform,
    warp: Option<&'a Warp>,
    out: Vec<PathEl>,
}

impl Recorder<'_> {
    fn map(&self, x: f32, y: f32) -> [f64; 2] {
        let (tx, ty) = self.glyph.apply(f64::from(x), f64::from(y));
        let (tx, ty) = match self.warp {
            Some(w) => w.apply(tx, ty),
            None => (tx, ty),
        };
        let (dx, dy) = self.post.apply(tx, ty);
        [dx, dy]
    }
}

impl skrifa::outline::OutlinePen for Recorder<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.out.push(PathEl::MoveTo(p));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.out.push(PathEl::LineTo(p));
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let (c, p) = (self.map(cx0, cy0), self.map(x, y));
        self.out.push(PathEl::QuadTo(c, p));
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (a, b, p) = (self.map(cx0, cy0), self.map(cx1, cy1), self.map(x, y));
        self.out.push(PathEl::CurveTo(a, b, p));
    }
    fn close(&mut self) {
        self.out.push(PathEl::Close);
    }
}

/// Glyph outlines of `layout` in document space (through `transform`, bent by `warp`), one
/// element list per glyph, for Type › Create Work Path and Convert to Shape. Faux bold is not
/// applied (Photoshop also converts the regular outline); faux italic and scaling are. Under a
/// warp, control points are mapped directly, which is exact for straight segments and a close
/// approximation for curves.
pub fn outlines(layout: &TextLayout, transform: &Affine, warp: Option<&Warp>) -> Vec<Vec<PathEl>> {
    let mut glyphs = Vec::new();
    for g in &layout.glyphs {
        let face = &layout.faces[g.face as usize];
        let Ok(font) = skrifa::FontRef::from_index(face.font.data.as_ref(), face.font.index) else {
            continue;
        };
        let Some(outline) = font.outline_glyphs().get(GlyphId::new(g.id)) else {
            continue;
        };
        let coords: Vec<NormalizedCoord> = face.coords.iter().map(|&c| NormalizedCoord::from_bits(c)).collect();
        let glyph = glyph_xform(layout, g, face.skew_deg);
        let mut rec = Recorder { glyph, post: Xform(transform.m), warp, out: Vec::new() };
        let settings = DrawSettings::unhinted(Size::new(face.size_px), LocationRef::new(&coords));
        if outline.draw(settings, &mut rec).is_ok() && !rec.out.is_empty() {
            glyphs.push(rec.out);
        }
    }
    glyphs
}

#[cfg(test)]
mod tests {
    use super::{MAX_PIXELS, rect_from_bounds};
    use photosuite_geom::Rect;

    #[test]
    fn bounds_are_clipped_without_overflow_and_oversized_rectangles_rejected() {
        let min = f64::from(i32::MIN);
        let max = f64::from(i32::MAX);
        let cases = [
            ([min, 0.0, min + 8.0, 8.0], Rect::new(i32::MIN, -1, i32::MIN + 9, 9)),
            ([max - 8.0, 0.0, max, 8.0], Rect::new(i32::MAX - 9, -1, i32::MAX, 9)),
            ([-f64::MAX, 0.0, f64::MAX, 8.0], Rect::EMPTY),
            ([0.0, 0.0, MAX_PIXELS as f64, 8.0], Rect::EMPTY),
            ([0.25, -3.25, 10.1, 8.8], Rect::new(-1, -5, 12, 10)),
        ];

        for (bounds, expected) in cases {
            let first = rect_from_bounds(bounds);
            assert_eq!(first, expected, "bounds: {bounds:?}");
            assert_eq!(rect_from_bounds(bounds), first, "bounds: {bounds:?}");
        }
    }
}
