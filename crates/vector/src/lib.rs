//! # photosplice-vector
//!
//! Rasterization of vector data from `photosplice-doc`: paths (with Photoshop path operations),
//! shape layers (fill + stroke), vector masks, and the inverse direction (tracing a coverage
//! mask back into a path for "Make Work Path").
//!
//! * [`align`]: Align Edges (a shape snapped to the pixel grid).
//! * [`edit`]: point-level path editing (Direct Selection, Convert Point) and hit testing.
//! * [`feather`]: the Gaussian blur of a feathered mask (a soft shape's fill).
//! * [`flatten`]: cubic Bézier flattening with a distance tolerance.
//! * [`map`]: non-affine maps of paths (Distort, Perspective, Warp).
//! * [`raster`]: exact area-coverage scanline rasterizer (non-zero / even-odd per component,
//!   boolean combination of components, inversion).
//! * [`stroke`]: polygon stroking with miter/round/bevel joins, butt/round/square caps, dashes.
//! * [`shapes`]: live shapes (rectangles with corner radii, ellipses, polygons, stars, lines).
//! * [`trace`]: contour tracing + curve fitting (selection → path).
//!
//! Output is either raw coverage (`f32` per pixel) or sparse tiled [`Surface`]s in any
//! [`PixelFormat`] (only tiles that differ from the default are allocated). Pure Rust, no
//! platform dependencies; builds for `wasm32` (single-threaded there, rayon elsewhere).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod align;
pub mod edit;
pub mod feather;
pub mod flatten;
pub mod map;
pub mod raster;
pub mod shapes;
pub mod stroke;
pub mod trace;

use photosplice_color::{ColorMode, PixelFormat};
use photosplice_doc::{Fill, FillRule, GradientStyle, Path, PathOp, ShapeLayer, ShapeStroke, StrokeAlign, VectorMask};
use photosplice_geom::{Rect, TILE_SIZE};
use photosplice_raster::Surface;

pub mod gradient;

pub use flatten::{Polyline, flatten_path, flatten_subpath};
pub use raster::Rasterizer;
pub use stroke::{StrokeStyle, stroke_polygons};

/// Default flattening tolerance in pixels (curve-to-chord distance).
pub const DEFAULT_TOLERANCE: f64 = 0.01;

/// Compiles the fill area of `path`: one component per subpath (filled with the path's rule)
/// folded with the subpath operations; open subpaths are closed implicitly.
pub fn fill_rasterizer(path: &Path, tol: f64) -> Rasterizer {
    let mut r = Rasterizer::new(path.inverted);
    // One point budget for the whole path, then one edge sort for all components.
    let polys: Vec<Vec<(f64, f64)>> = flatten_path(path, tol).into_iter().map(|l| l.pts).collect();
    for c in path.components() {
        r.push_component(polys.get(c.clone()).unwrap_or_default(), path.effective_op(c.start), path.fill_rule);
    }
    r.sort_edges();
    r
}

/// Compiles the stroke outline of `path` (all subpaths, one non-zero component).
pub fn stroke_rasterizer(path: &Path, style: &StrokeStyle, tol: f64) -> Rasterizer {
    // Stroke outlines tolerate a coarser flattening than fills (the offset hides chord error).
    let lines = flatten_path(path, (tol * 4.0).min(style.width.max(0.01) * 0.1).max(1e-3));
    let polys = stroke_polygons(&lines, style, tol);
    let mut r = Rasterizer::new(false);
    r.add_component(&polys, PathOp::Combine, FillRule::NonZero);
    r
}

/// Stroke geometry for a shape stroke (dash lengths are multiples of the width in the model).
pub fn stroke_style(s: &ShapeStroke) -> StrokeStyle {
    let w = f64::from(s.width.max(0.0));
    StrokeStyle {
        width: w,
        cap: s.cap,
        join: s.join,
        miter_limit: f64::from(s.miter_limit.max(1.0)),
        dashes: s.dashes.iter().map(|d| f64::from(*d) * w).collect(),
        dash_offset: f64::from(s.dash_offset) * w,
    }
}

/// Fill coverage of `path` over `rect` (row-major, `0..=1`).
pub fn path_coverage(path: &Path, rect: Rect) -> Vec<f32> {
    fill_rasterizer(path, DEFAULT_TOLERANCE).render(rect)
}

enum MaskGeometry {
    Constant(f32),
    Path(Rasterizer),
}

/// A vector mask's immutable geometry and density, reusable across render rectangles.
pub struct CompiledVectorMask {
    geometry: MaskGeometry,
    density: Option<f32>,
}

impl CompiledVectorMask {
    /// Compiles only enabled, nonempty paths using the default flattening tolerance.
    pub fn new(m: &VectorMask) -> Self {
        let geometry = if !m.enabled {
            MaskGeometry::Constant(1.0)
        } else if m.path.is_empty() {
            // An empty Photoshop vector mask reveals all, unlike an empty fill path.
            MaskGeometry::Constant(if m.path.inverted { 0.0 } else { 1.0 })
        } else {
            MaskGeometry::Path(fill_rasterizer(&m.path, DEFAULT_TOLERANCE))
        };
        Self { geometry, density: (m.enabled && m.density < 1.0).then(|| m.density.clamp(0.0, 1.0)) }
    }

    /// Effective mask values over `rect`, with invocation-local rasterization state.
    pub fn render(&self, rect: Rect) -> Vec<f32> {
        self.render_with(rect, Rasterizer::render)
    }

    /// Effective mask values without entering a thread pool. Use this while
    /// initializing a shared result that other Rayon workers may wait for.
    pub fn render_sequential(&self, rect: Rect) -> Vec<f32> {
        self.render_with(rect, |rasterizer, rect| {
            let mut v = vec![0.0; rect.width() as usize * rect.height() as usize];
            if !v.is_empty() {
                rasterizer.render_into(rect, &mut v);
            }
            v
        })
    }

    /// Mask values over `rect`, with a path's coverage from `coverage`.
    fn render_with(&self, rect: Rect, coverage: impl FnOnce(&Rasterizer, Rect) -> Vec<f32>) -> Vec<f32> {
        let mut v = match &self.geometry {
            MaskGeometry::Constant(value) => vec![*value; rect.width() as usize * rect.height() as usize],
            MaskGeometry::Path(rasterizer) => coverage(rasterizer, rect),
        };
        self.apply_density(&mut v);
        v
    }

    fn apply_density(&self, v: &mut [f32]) {
        if let Some(d) = self.density {
            for x in v {
                *x = 1.0 - d * (1.0 - *x);
            }
        }
    }
}

/// Effective vector-mask values over `rect` (density applied; all ones when disabled). Like
/// Photoshop, a vector mask without any subpath reveals everything (hides everything when
/// inverted): "Add Vector Mask" starts from an empty, revealing mask.
pub fn vector_mask_values(m: &VectorMask, rect: Rect) -> Vec<f32> {
    CompiledVectorMask::new(m).render(rect)
}

/// Tile-aligned bands (rows of tiles) covering `r`.
fn tile_bands(r: Rect) -> Vec<Rect> {
    let mut out = Vec::new();
    if r.is_empty() {
        return out;
    }
    // In i64: a band near `i32::MAX` would overflow stepping past it.
    let step = i64::from(TILE_SIZE);
    let (y0, y1) = (i64::from(r.y0), i64::from(r.y1));
    let mut y = y0.div_euclid(step) * step;
    while y < y1 {
        let next = (y + step).min(y1);
        out.push(Rect::new(r.x0, y.max(y0) as i32, r.x1, next as i32));
        y = next;
    }
    out
}

/// Splits a band buffer (`channels` floats per pixel) into tile-aligned pieces, skipping
/// pieces where `keep` is false for every pixel.
fn band_tiles(band: Rect, data: &[f32], channels: usize, keep: impl Fn(&[f32]) -> bool) -> Vec<(Rect, Vec<f32>)> {
    let w = band.width() as usize;
    let mut out = Vec::new();
    if band.is_empty() {
        return out;
    }
    let step = i64::from(TILE_SIZE);
    let (x0, x1) = (i64::from(band.x0), i64::from(band.x1));
    let mut x = x0.div_euclid(step) * step;
    while x < x1 {
        let next = (x + step).min(x1);
        let tr = Rect::new(x.max(x0) as i32, band.y0, next as i32, band.y1);
        let tw = tr.width() as usize;
        let mut buf = Vec::with_capacity(tw * tr.height() as usize * channels);
        let mut any = false;
        for row in 0..tr.height() as usize {
            let o = (row * w + (tr.x0 - band.x0) as usize) * channels;
            let slice = &data[o..o + tw * channels];
            if !any && slice.chunks_exact(channels).any(&keep) {
                any = true;
            }
            buf.extend_from_slice(slice);
        }
        if any {
            out.push((tr, buf));
        }
        x = next;
    }
    out
}

/// Runs `f` over the bands of `r` (in parallel groups on native targets) and writes the
/// resulting pieces into `surface`.
fn render_bands(surface: &mut Surface, r: Rect, f: &(dyn Fn(Rect) -> Vec<(Rect, Vec<f32>)> + Sync)) {
    let bands = tile_bands(r);
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;
        let group = rayon::current_num_threads().max(1) * 2;
        for chunk in bands.chunks(group) {
            let parts: Vec<Vec<(Rect, Vec<f32>)>> = chunk.par_iter().map(|b| f(*b)).collect();
            for (tr, v) in parts.into_iter().flatten() {
                surface.write_region(tr, &v);
            }
        }
    }
    #[cfg(target_arch = "wasm32")]
    for b in bands {
        for (tr, v) in f(b) {
            surface.write_region(tr, &v);
        }
    }
}

/// Renders a rasterizer's coverage into a single-channel surface of `format` (grayscale
/// model, no alpha, e.g. a selection or mask) over `clip`. Outside `clip` the surface reads 0,
/// or 1 for inverted paths whose coverage is written everywhere inside `clip`.
pub fn coverage_surface(r: &Rasterizer, format: PixelFormat, clip: Rect) -> Surface {
    let fmt = PixelFormat::new(ColorMode::Grayscale, format.sample, false);
    let mut s = Surface::new(fmt);
    let area = if r.is_inverted() { clip } else { r.pixel_bounds().map_or(Rect::EMPTY, |b| b.intersect(&clip)) };
    render_bands(&mut s, area, &|band| {
        let cov = r.render(band);
        band_tiles(band, &cov, 1, |p| p[0] > 0.0)
    });
    s
}

/// A pattern as a shape paints it: tiles, placement and sampling live above this crate (the
/// document's pattern table and the compositor's sampler), so callers that have them hand them
/// in through [`CompiledShape::with_patterns`].
pub trait PatternPaint: Send + Sync {
    /// Straight RGBA of the pattern for the pixel whose top-left corner is `(x, y)`.
    fn at(&self, x: i32, y: i32) -> [f32; 4];
}

/// A shared [`PatternPaint`] (cheap to clone, printable).
#[derive(Clone)]
pub struct PatternSource(pub std::sync::Arc<dyn PatternPaint>);

impl std::fmt::Debug for PatternSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PatternSource")
    }
}

/// Resolves a shape's pattern paint (`Fill::Pattern`) into the pattern it samples; `None` when
/// the pattern is missing or can't be painted (the paint is then transparent).
pub type PatternResolver<'a> = &'a dyn Fn(&Fill) -> Option<PatternSource>;

/// Colour source for fills and strokes.
#[derive(Clone, Debug)]
pub enum Paint {
    None,
    /// A pattern ([`PatternResolver`]).
    Pattern(PatternSource),
    /// Straight RGBA.
    Solid([f32; 4]),
    Gradient {
        /// Colour stops (sorted by location, straight RGBA).
        stops: Vec<(f32, [f32; 4])>,
        /// Midpoint of each segment between colour stops (0.5 = halfway).
        mids: Vec<f32>,
        /// Opacity stops (sorted; empty = opaque).
        opacity: Vec<(f32, f32)>,
        style: GradientStyle,
        /// Angle, scale and centre offset (fractions of the frame) after the whole-pixel layout
        /// ([`gradient::gradient_layout`]).
        angle: f32,
        scale: f32,
        reverse: bool,
        offset: (f32, f32),
        /// Dither: one level of the shared noise breaks 8-bit banding.
        dither: bool,
        /// The frame the gradient is laid out in: the shape's bounds, or the canvas when the fill
        /// does not align with the layer.
        frame: Rect,
    },
}

impl Paint {
    /// Paint for a model fill, with gradients laid out over `bounds` (`x0, y0, x1, y1`) — the
    /// layer's frame — or over `canvas` when the fill is not aligned with the layer. Every
    /// gradient field is honoured (midpoints, opacity stops, centre offset, dither, unsorted
    /// stops), rendering the same pixels as the compositor's fill layers; the parity tests in
    /// `photosplice-compose` (which this crate cannot depend on) keep the two in step. Pattern
    /// fills are not rendered (transparent): they need the document's pattern table, which
    /// lives above this crate ([`Paint::from_fill_with`] takes it).
    pub fn from_fill(f: &Fill, bounds: (f64, f64, f64, f64), canvas: (f64, f64, f64, f64)) -> Paint {
        Paint::from_fill_with(f, bounds, canvas, &|_| None)
    }

    /// [`Paint::from_fill`] with pattern fills painted by `patterns`.
    pub fn from_fill_with(f: &Fill, bounds: (f64, f64, f64, f64), canvas: (f64, f64, f64, f64), patterns: PatternResolver<'_>) -> Paint {
        let rgba = |c: &photosplice_color::Color| {
            let v = c.to_rgb();
            [v[0], v[1], v[2], c.alpha]
        };
        match f {
            Fill::Solid(c) => Paint::Solid(rgba(c)),
            // No `..` here on purpose: a new gradient field must fail to compile until the
            // shape renderer honours it (the same sentinel the document/compositor pair uses).
            Fill::Gradient { stops, angle, scale, style, reverse, opacity_stops, midpoints, offset, dither, align } => {
                let frame = frame_of(if *align { bounds } else { canvas });
                // Linear and Reflected end points on whole pixels, as fill layers lay them out.
                let (angle, scale, offset) = gradient::gradient_layout(*style, *angle, *scale, *offset, frame);
                let mut stops: Vec<(f32, [f32; 4])> = stops.iter().map(|(p, c)| (*p, rgba(c))).collect();
                stops.sort_by(|a, b| a.0.total_cmp(&b.0));
                let mut opacity = opacity_stops.clone();
                opacity.sort_by(|a, b| a.0.total_cmp(&b.0));
                Paint::Gradient {
                    stops,
                    mids: midpoints.clone(),
                    opacity,
                    style: *style,
                    angle,
                    scale,
                    reverse: *reverse,
                    offset,
                    dither: *dither,
                    frame,
                }
            }
            Fill::Pattern { .. } => patterns(f).map_or(Paint::None, Paint::Pattern),
        }
    }

    /// Straight RGBA at pixel centre `(x, y)`.
    pub fn at(&self, x: f64, y: f64) -> [f32; 4] {
        match self {
            Paint::None => [0.0; 4],
            Paint::Pattern(p) => p.0.at(px_of(x), px_of(y)),
            Paint::Solid(c) => *c,
            Paint::Gradient { style, angle, scale, reverse, offset, dither: dith, frame, .. } => {
                let mut c = self.sample(gradient::gradient_t(*style, *angle, *scale, *reverse, *offset, *frame, x as f32, y as f32));
                if *dith {
                    dither(&mut c, x.floor() as i32, y.floor() as i32);
                }
                c
            }
        }
    }

    /// Straight RGBA at gradient position `t` (`0..=1`); a solid paint's colour (a pattern,
    /// which has no gradient position, is transparent).
    pub fn sample(&self, t: f32) -> [f32; 4] {
        match self {
            Paint::None | Paint::Pattern(_) => [0.0; 4],
            Paint::Solid(c) => *c,
            Paint::Gradient { stops, mids, opacity, .. } => gradient::sample(stops, mids, opacity, t),
        }
    }
}

/// The pixel holding coordinate `v` (a pixel centre maps to its pixel), saturating far off.
fn px_of(v: f64) -> i32 {
    if v.is_finite() { v.floor().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32 } else { 0 }
}

/// The whole-pixel frame of shape bounds (`x0, y0, x1, y1`), as Photoshop lays a shape layer's
/// gradient out over the layer's pixel bounds.
fn frame_of(b: (f64, f64, f64, f64)) -> Rect {
    let px = |v: f64| if v.is_finite() { v.clamp(-1e9, 1e9) } else { 0.0 };
    Rect::new(px(b.0).floor() as i32, px(b.1).floor() as i32, px(b.2).ceil() as i32, px(b.3).ceil() as i32)
}

/// Adds one level of the shared dither noise to a colour's channels (as the Gradient tool and
/// the compositor's fill layers do).
fn dither(c: &mut [f32; 4], x: i32, y: i32) {
    let n = (photosplice_color::dither_noise(x, y) - 0.5) / 255.0;
    for ch in c.iter_mut().take(3) {
        *ch = (*ch + n).clamp(0.0, 1.0);
    }
}

/// How much of a shape's fill shows at each pixel: the path's coverage, or (with a density or
/// feather) the path as the fill's soft vector mask.
enum FillCoverage {
    Path(Rasterizer),
    /// Density without feather: `1 - density × (1 - coverage)`.
    Dense(Rasterizer, f32),
    /// Feathered (and maybe less dense): the mask over `area`, `outside` everywhere else.
    Plane { area: Rect, values: Vec<f32>, outside: f32 },
}

impl FillCoverage {
    /// The fill's coverage for `shape` drawn along `path` (its path, or that path under Align
    /// Edges) over the frame `canvas`: a feather is blurred within it, as the compositor blurs a
    /// vector mask within the canvas.
    fn new(shape: &ShapeLayer, path: &Path, area: &Rasterizer, canvas: Rect) -> Self {
        if !shape.has_soft_mask() {
            return FillCoverage::Path(area.clone());
        }
        let density = if shape.density.is_finite() { shape.density.clamp(0.0, 1.0) } else { 1.0 };
        let sigma = feather::feather_sigma(shape.feather);
        if sigma <= 0.0 {
            return FillCoverage::Dense(area.clone(), density);
        }
        // What the compositor does for a vector mask with these settings
        // (`photosplice-compose::masks`): coverage with density, blurred.
        let soften = |v: &mut [f32]| {
            if density < 1.0 {
                for x in v.iter_mut() {
                    *x = 1.0 - density * (1.0 - *x);
                }
            }
        };
        let mut far = area.render(Rect::from_xywh(canvas.x0.saturating_sub(1_000_000), canvas.y0.saturating_sub(1_000_000), 1, 1));
        soften(&mut far);
        let outside = far.first().copied().unwrap_or(0.0);
        let region = feather::path_area(path, sigma).intersect(&canvas);
        let (w, h) = (region.width() as usize, region.height() as usize);
        let mut values = Vec::new();
        if values.try_reserve_exact(w.saturating_mul(h)).is_err() {
            // Too large to blur: keep the edge hard rather than fail the render.
            return FillCoverage::Dense(area.clone(), density);
        }
        values.resize(w * h, 0.0);
        if !values.is_empty() {
            area.render_into(region, &mut values);
            soften(&mut values);
            feather::gaussian(&mut values, w, h, sigma);
        }
        FillCoverage::Plane { area: region, values, outside }
    }

    /// Pixel bounds of the visible fill (`None` = nothing).
    fn bounds(&self) -> Option<Rect> {
        const ALL: Rect = Rect::new(i32::MIN / 4, i32::MIN / 4, i32::MAX / 4, i32::MAX / 4);
        match self {
            FillCoverage::Path(r) => r.pixel_bounds(),
            FillCoverage::Dense(_, d) if *d < 1.0 => Some(ALL),
            FillCoverage::Dense(r, _) => r.pixel_bounds(),
            FillCoverage::Plane { outside, .. } if *outside > 0.0 => Some(ALL),
            FillCoverage::Plane { area, .. } => (!area.is_empty()).then_some(*area),
        }
    }

    fn render(&self, rect: Rect) -> Vec<f32> {
        match self {
            FillCoverage::Path(r) => r.render(rect),
            FillCoverage::Dense(r, d) => {
                let mut v = r.render(rect);
                for x in &mut v {
                    *x = 1.0 - d * (1.0 - *x);
                }
                v
            }
            FillCoverage::Plane { area, values, outside } => {
                let w = area.width() as usize;
                let mut v = Vec::with_capacity(rect.width() as usize * rect.height() as usize);
                for y in rect.y0..rect.y1 {
                    for x in rect.x0..rect.x1 {
                        let i = if area.contains(x, y) { (y - area.y0) as usize * w + (x - area.x0) as usize } else { usize::MAX };
                        v.push(values.get(i).copied().unwrap_or(*outside));
                    }
                }
                v
            }
        }
    }

    /// Whether the fill reaches past the path's own coverage (a density or feather).
    fn is_soft(&self) -> bool {
        !matches!(self, FillCoverage::Path(_))
    }
}

/// Everything needed to render a shape layer, compiled once.
pub struct CompiledShape {
    fill: Option<(FillCoverage, Paint)>,
    stroke: Option<(Rasterizer, Paint, f32)>,
    /// Fill area used to clip inside/outside strokes.
    align: Option<(Rasterizer, bool)>,
    bounds: Option<Rect>,
}

impl CompiledShape {
    /// Compiles the shape at flattening tolerance `tol` on a `canvas`: the frame of gradients
    /// without "Align with layer", and of an inverted path (or one without points), which fills
    /// the whole canvas, as Photoshop lays a shape layer's gradient over the layer's bounds
    /// (psd-tools vector-mask2.psd).
    pub fn new(shape: &ShapeLayer, tol: f64, canvas: Rect) -> Self {
        Self::with_patterns(shape, tol, canvas, &|_| None)
    }

    /// [`CompiledShape::new`] with pattern fills and strokes painted by `patterns` (without it
    /// they are transparent).
    pub fn with_patterns(shape: &ShapeLayer, tol: f64, canvas: Rect, patterns: PatternResolver<'_>) -> Self {
        let aligned = shape.align_edges.then(|| aligned_path(shape, tol));
        let path = aligned.as_ref().unwrap_or(&shape.path);
        let cb = (f64::from(canvas.x0), f64::from(canvas.y0), f64::from(canvas.x1), f64::from(canvas.y1));
        let pb = match path.control_bounds() {
            Some(b) if !path.inverted => b,
            _ => cb,
        };
        let area = fill_rasterizer(path, tol);
        let mut bounds = None;
        let fill = shape.fill.as_ref().map(|f| {
            // Density and Feather soften the fill only (the path is its vector mask); the stroke
            // stays crisp.
            let coverage = FillCoverage::new(shape, path, &area, canvas);
            bounds = coverage.bounds();
            (coverage, Paint::from_fill_with(f, pb, cb, patterns))
        });
        let all_closed = path.subpaths.iter().all(|s| s.closed);
        let mut align = None;
        let stroke = shape.stroke.as_ref().filter(|s| s.width > 0.0 && s.opacity > 0.0).map(|s| {
            let mut st = stroke_style(s);
            if all_closed && s.align != StrokeAlign::Center {
                // Inside/outside strokes: a centred stroke twice as wide, clipped by the fill area.
                st.width *= 2.0;
                align = Some((area.clone(), s.align == StrokeAlign::Inside));
            }
            let r = stroke_rasterizer(path, &st, tol);
            let sb = r.pixel_bounds();
            bounds = match (bounds, sb) {
                (Some(a), Some(b)) => Some(a.union(&b)),
                (a, b) => a.or(b),
            };
            (r, Paint::from_fill_with(&s.paint, pb, cb, patterns), s.opacity.clamp(0.0, 1.0))
        });
        if path.inverted && shape.fill.is_some() {
            bounds = Some(Rect::new(i32::MIN / 4, i32::MIN / 4, i32::MAX / 4, i32::MAX / 4));
        }
        CompiledShape { fill, stroke, align, bounds }
    }

    /// Pixel bounds of the rendering (`None` = nothing visible).
    pub fn bounds(&self) -> Option<Rect> {
        self.bounds
    }

    /// Straight RGBA for every pixel of `rect`.
    pub fn render_rgba(&self, rect: Rect) -> Vec<[f32; 4]> {
        let n = rect.width() as usize * rect.height() as usize;
        let mut out = vec![[0.0f32; 4]; n];
        let w = rect.width() as usize;
        if let Some((r, paint)) = &self.fill {
            let cov = r.render(rect);
            for (i, (o, c)) in out.iter_mut().zip(&cov).enumerate() {
                if *c > 0.0 {
                    let (x, y) = ((rect.x0 + (i % w) as i32) as f64 + 0.5, (rect.y0 + (i / w) as i32) as f64 + 0.5);
                    let mut p = paint.at(x, y);
                    p[3] *= c;
                    *o = p;
                }
            }
        }
        if let Some((r, paint, opacity)) = &self.stroke {
            let cov = r.render(rect);
            // Inside and outside strokes split each edge pixel with the fill by the path's
            // coverage there, as Reference draws them: an inside stroke goes atop the fill (the
            // pixel keeps the path's coverage, its colour mixed by the stroke's), an outside one
            // fills the rest of the pixel beside it. Composited one over the other, both halves
            // of a partly covered pixel would show the background through (align kit, a 3 px
            // inside stroke at x = 90.25: alpha 239 where Reference has the path's 195).
            let area = self.align.as_ref().map(|(area, inside)| (area.render(rect), *inside));
            // A soft fill (density, feather) isn't the path's coverage, so it can't share edge
            // pixels by it: the stroke, clipped to its side of the path, goes over it.
            let soft = self.fill.as_ref().is_some_and(|(f, _)| f.is_soft());
            for (i, (o, c)) in out.iter_mut().zip(&cov).enumerate() {
                if *c <= 0.0 {
                    continue;
                }
                let (x, y) = ((rect.x0 + (i % w) as i32) as f64 + 0.5, (rect.y0 + (i / w) as i32) as f64 + 0.5);
                let s = paint.at(x, y);
                let mut sa = s[3] * c * opacity;
                let split = area.as_ref().map(|(a, inside)| (a.get(i).copied().unwrap_or(0.0), *inside));
                let split = match split {
                    Some((a, inside)) if soft => {
                        sa *= if inside { a } else { (1.0 - a).max(0.0) };
                        None
                    }
                    other => other,
                };
                match split {
                    Some((a, true)) => {
                        if a <= 0.0 {
                            continue;
                        }
                        // The fill's own alpha before the path's coverage scaled it.
                        let fa = (o[3] / a).min(1.0);
                        let ia = sa + fa * (1.0 - sa);
                        if ia > 0.0 {
                            for k in 0..3 {
                                o[k] = (s[k] * sa + o[k] * fa * (1.0 - sa)) / ia;
                            }
                        }
                        o[3] = ia * a;
                    }
                    Some((a, false)) => {
                        let sa = sa * (1.0 - a).max(0.0);
                        let t = o[3] + sa;
                        if t > 0.0 {
                            for k in 0..3 {
                                o[k] = (o[k] * o[3] + s[k] * sa) / t;
                            }
                        }
                        o[3] = t.min(1.0);
                    }
                    None => {
                        let da = o[3];
                        let a = sa + da * (1.0 - sa);
                        if a > 0.0 {
                            for k in 0..3 {
                                o[k] = (s[k] * sa + o[k] * da * (1.0 - sa)) / a;
                            }
                        }
                        o[3] = a;
                    }
                }
            }
        }
        out
    }

    /// Renders into a new surface of `format` over `clip` (typically the canvas: it bounds the
    /// render and anchors gradients whose fill is not aligned with the layer).
    pub fn render(&self, format: PixelFormat, clip: Rect) -> Surface {
        let mut s = Surface::new(format);
        let Some(b) = self.bounds else { return s };
        let area = b.intersect(&clip);
        let ch = format.channels();
        render_bands(&mut s, area, &|band| {
            let rgba = self.render_rgba(band);
            let mut vals = vec![0.0f32; rgba.len() * ch];
            for (p, o) in rgba.iter().zip(vals.chunks_exact_mut(ch)) {
                if p[3] > 0.0 {
                    photosplice_raster::from_rgba_into(&format, *p, o);
                }
            }
            let alpha = format.alpha;
            band_tiles(band, &vals, ch, |px| if alpha { px[ch - 1] > 0.0 } else { px.iter().any(|v| *v != 0.0) })
        });
        s
    }
}

/// A shape's path under Align Edges: what it draws (the path, or the outline of a centre or
/// outside stroke, which reaches past it) snapped to the pixel grid, the path moved with it.
fn aligned_path(shape: &ShapeLayer, tol: f64) -> Path {
    let mut extent = align::tight_bounds(&shape.path);
    let all_closed = shape.path.subpaths.iter().all(|s| s.closed);
    if let Some(s) = shape.stroke.as_ref().filter(|s| s.width > 0.0 && s.opacity > 0.0 && !(all_closed && s.align == StrokeAlign::Inside)) {
        let mut st = stroke_style(s);
        if all_closed && s.align == StrokeAlign::Outside {
            st.width *= 2.0;
        }
        let lines = flatten_path(&shape.path, tol);
        for p in stroke_polygons(&lines, &st, tol).iter().flatten() {
            if p.0.is_finite() && p.1.is_finite() {
                extent = Some(match extent {
                    Some((x0, y0, x1, y1)) => (x0.min(p.0), y0.min(p.1), x1.max(p.0), y1.max(p.1)),
                    None => (p.0, p.1, p.0, p.1),
                });
            }
        }
    }
    match extent.and_then(align::align_transform) {
        Some(a) => shape.path.transform(&a),
        None => shape.path.clone(),
    }
}

/// Renders a shape layer's appearance (fill, then stroke over it) in `format`, clipped to
/// `clip` — the canvas, which also lays out gradients whose fill is not aligned with the layer.
pub fn render_shape(shape: &ShapeLayer, format: PixelFormat, clip: Rect) -> Surface {
    CompiledShape::new(shape, DEFAULT_TOLERANCE, clip).render(format, clip)
}

/// [`render_shape`] with pattern fills and strokes painted by `patterns`.
pub fn render_shape_with(shape: &ShapeLayer, format: PixelFormat, clip: Rect, patterns: PatternResolver<'_>) -> Surface {
    CompiledShape::with_patterns(shape, DEFAULT_TOLERANCE, clip, patterns).render(format, clip)
}

#[cfg(test)]
mod tests;
