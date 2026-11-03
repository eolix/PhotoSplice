//! Pattern paint for shape layers: a shape whose fill or stroke is a pattern samples it with the
//! same sampler as pattern fill layers and effects ([`crate::pattern`]), handed to
//! `photosplice-vector` (which rasterises the shape but can't see the document's patterns).
//!
//! Placement: Reference tiles a linked pattern from the layer's effects reference point (PSD
//! `fxrp`) plus the phase, an unlinked one from the canvas origin plus the phase, never from the
//! path's bounds. Measured on the pattern kit (a rectangle at (20, 20) painted with an asymmetric
//! 13 × 9 tile: linked / unlinked, phase (5, 3), `fxrp` (10, 6), an 8 px pattern stroke, each
//! opened and saved by Reference, then moved by (13, 7)): every interior pixel matches exactly,
//! before and after the move, which moves `fxrp` by the same amount and keeps the phase (so a
//! linked pattern follows the layer, an unlinked one stays on the canvas). psd-tools stroke.psd
//! agrees (its `fxrp` is the canvas origin): anchored at the path's bounds, no pixel of its
//! pattern stroke matches.

use std::sync::Arc;

use photosplice_color::{ColorMode, PixelFormat};
use photosplice_doc::vector::ShapeLayer;
use photosplice_doc::{Document, Fill, Layer, Pattern, Rect};
use photosplice_raster::Surface;
use photosplice_vector::{PatternPaint, PatternSource};

use crate::pattern::{Placement, Tile};

/// What a shape's pattern paint samples: the document's patterns, the colour mode they paint
/// into, and the layer's effects reference point (the linked anchor).
#[derive(Clone, Copy, Debug)]
pub struct ShapePatterns<'a> {
    pub patterns: &'a [Pattern],
    pub mode: ColorMode,
    /// The layer's effects reference point (PSD `fxrp`); `None` = the canvas origin.
    pub reference: Option<(f64, f64)>,
}

impl<'a> ShapePatterns<'a> {
    /// No patterns: pattern paint is transparent.
    pub const NONE: ShapePatterns<'static> = ShapePatterns { patterns: &[], mode: ColorMode::Rgb, reference: None };

    /// The patterns `layer` (of `doc`) paints with.
    pub fn of(doc: &'a Document, layer: &Layer) -> Self {
        ShapePatterns { patterns: &doc.patterns, mode: doc.mode, reference: layer.effects.reference }
    }

    /// The patterns of `doc` for a layer whose effects reference point is `reference`.
    pub fn in_doc(doc: &'a Document, reference: Option<(f64, f64)>) -> Self {
        ShapePatterns { patterns: &doc.patterns, mode: doc.mode, reference }
    }

    /// The pattern `f` paints, placed; `None` for other paints and for a pattern that is missing
    /// or can't be painted (the paint is then transparent, as a pattern fill layer's).
    pub fn resolve(&self, f: &Fill) -> Option<PatternSource> {
        let Fill::Pattern { name, scale, id, angle, link, phase } = f else { return None };
        let p = photosplice_doc::pattern::find(self.patterns, id, name)?;
        // A CMYK document's shape pixels are separated from RGB ([`crate::pattern::PreparedPatterns::in_mode`]).
        let mode = if self.mode == ColorMode::Cmyk { ColorMode::Rgb } else { self.mode };
        let tile = Tile::new_in(p, mode)?;
        let anchor = self.reference.filter(|(x, y)| x.is_finite() && y.is_finite()).unwrap_or((0.0, 0.0));
        let place = Placement::anchored(anchor, *link, *phase, *scale, *angle);
        Some(PatternSource(Arc::new(Placed { tile, place })))
    }

    /// The patterns `sh` paints with (by id or name), for cache keys: empty when it paints none.
    pub fn used_by(&self, sh: &ShapeLayer) -> Vec<&'a Pattern> {
        paints(sh).filter_map(|f| if let Fill::Pattern { name, id, .. } = f { photosplice_doc::pattern::find(self.patterns, id, name) } else { None }).collect()
    }
}

/// Whether `sh` paints its fill or stroke with a pattern.
pub fn uses_patterns(sh: &ShapeLayer) -> bool {
    paints(sh).any(|f| matches!(f, Fill::Pattern { .. }))
}

/// Whether `sh`'s pixels moved with its layer are still its rendering: not when a pattern paint
/// stays where it is (unlinked, or linked without a reference point to move with the layer).
pub fn moves_with_layer(sh: &ShapeLayer, reference: Option<(f64, f64)>) -> bool {
    paints(sh).all(|f| !matches!(f, Fill::Pattern { link, .. } if !*link || reference.is_none()))
}

fn paints(sh: &ShapeLayer) -> impl Iterator<Item = &Fill> {
    sh.fill.iter().chain(sh.stroke.iter().map(|s| &s.paint))
}

/// A pattern tile at its placement in the document.
struct Placed {
    tile: Tile,
    place: Placement,
}

impl PatternPaint for Placed {
    fn at(&self, x: i32, y: i32) -> [f32; 4] {
        // The pixel's corner, as pattern fill layers map it ([`crate::pattern::render`]).
        self.tile.sample_area(&self.place, f64::from(x), f64::from(y))
    }
}

/// Renders a shape layer's appearance (fill, then stroke) in `format` over `clip` (the canvas),
/// pattern paint included: [`photosplice_vector::render_shape`] with the document's patterns.
pub fn render_shape(sh: &ShapeLayer, format: PixelFormat, clip: Rect, patterns: ShapePatterns<'_>) -> Surface {
    photosplice_vector::render_shape_with(sh, format, clip, &|f| patterns.resolve(f))
}

/// [`render_shape`] for the shape layer `layer` of `doc`, in the document's pixel format over its
/// canvas (what a shape layer's pixels hold). `None` when `layer` isn't a shape layer.
pub fn render_layer_shape(doc: &Document, layer: &Layer) -> Option<Surface> {
    let photosplice_doc::LayerContent::Shape(sh) = &layer.content else { return None };
    Some(render_shape(sh, doc.pixel_format(), doc.bounds(), ShapePatterns::of(doc, layer)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use photosplice_color::Color;
    use photosplice_doc::vector::{Path, ShapeStroke, Subpath};
    use photosplice_doc::{LayerContent, StrokeAlign};

    /// 4 × 4: blue top-left and bottom-right quarters, red elsewhere.
    fn checker() -> Pattern {
        let mut s = Surface::new(PixelFormat::RGBA8);
        s.fill_rect(Rect::new(0, 0, 4, 4), &[1.0, 0.0, 0.0, 1.0]);
        s.fill_rect(Rect::new(0, 0, 2, 2), &[0.0, 0.0, 1.0, 1.0]);
        s.fill_rect(Rect::new(2, 2, 4, 4), &[0.0, 0.0, 1.0, 1.0]);
        Pattern::new("checker", s, 4, 4)
    }

    fn pattern_fill(p: &Pattern, link: bool, phase: (f32, f32)) -> Fill {
        Fill::Pattern { name: p.name.clone(), scale: 1.0, id: p.id.clone(), angle: 0.0, link, phase }
    }

    fn shape(fill: Option<Fill>, stroke: Option<Fill>) -> ShapeLayer {
        ShapeLayer {
            path: Path::new(vec![Subpath::polygon(&[(5.0, 3.0), (15.0, 3.0), (15.0, 13.0), (5.0, 13.0)])]),
            fill,
            stroke: stroke.map(|paint| ShapeStroke { width: 2.0, paint, align: StrokeAlign::Inside, ..ShapeStroke::default() }),
            ..ShapeLayer::default()
        }
    }

    fn blue(px: &[f32]) -> bool {
        px.get(2).is_some_and(|v| *v > 0.99) && px.first().is_some_and(|v| *v < 0.01)
    }

    /// The fill samples the pattern exactly at 100 %, tiled from the canvas origin (no `fxrp`)
    /// whatever the path's bounds, and from the effects reference point plus the phase.
    #[test]
    fn shape_patterns_tile_from_the_reference_point() {
        let p = checker();
        let canvas = Rect::new(0, 0, 20, 16);
        let pats = [p.clone()];
        let at = |sh: &ShapeLayer, reference: Option<(f64, f64)>, x: i32, y: i32| {
            let ctx = ShapePatterns { patterns: &pats, mode: ColorMode::Rgb, reference };
            render_shape(sh, PixelFormat::RGBA8, canvas, ctx).pixel(x, y)
        };
        let sh = shape(Some(pattern_fill(&p, true, (0.0, 0.0))), None);
        // Canvas origin: (8, 4) is tile (0, 0), blue; (10, 4) tile (2, 0), red.
        assert!(blue(&at(&sh, None, 8, 4)));
        assert!(!blue(&at(&sh, None, 10, 4)));
        // fxrp (2, 0) moves the tiling two pixels right.
        assert!(!blue(&at(&sh, Some((2.0, 0.0)), 8, 4)));
        assert!(blue(&at(&sh, Some((2.0, 0.0)), 10, 4)));
        // The phase adds to it; an unlinked pattern ignores the reference point.
        let phased = shape(Some(pattern_fill(&p, true, (0.0, 2.0))), None);
        assert!(blue(&at(&phased, Some((2.0, 0.0)), 10, 6)));
        let unlinked = shape(Some(pattern_fill(&p, false, (0.0, 0.0))), None);
        assert!(blue(&at(&unlinked, Some((2.0, 0.0)), 8, 4)));
        // A hostile reference point falls back to the canvas origin.
        assert!(blue(&at(&sh, Some((f64::NAN, 0.0)), 8, 4)));
    }

    /// A pattern fill layer clipped by a vector mask tiles a linked pattern from its effects
    /// reference point plus the phase, not from the mask's bounds (the pattern kit: a mask at
    /// (20, 20), `fxrp` (10, 6)); without a reference point it keeps the mask's top-left.
    #[test]
    fn fill_layers_tile_linked_patterns_from_the_reference_point() {
        let p = checker();
        let pats = [p.clone()];
        let canvas = Rect::new(0, 0, 40, 30);
        let mut l = Layer::new("f", LayerContent::Fill(pattern_fill(&p, true, (1.0, 0.0))));
        l.vector_mask = Some(photosplice_doc::VectorMask::new(Path::new(vec![Subpath::polygon(&[(21.0, 21.0), (35.0, 21.0), (35.0, 28.0), (21.0, 28.0)])])));
        let px = |l: &Layer, x: i32, y: i32| {
            let LayerContent::Fill(f) = &l.content else { panic!("a fill layer") };
            let b = crate::render_fill_content(l, f, canvas, &pats);
            b.px[(y * 40 + x) as usize]
        };
        let is_blue = |q: [f32; 4]| q[2] > 0.99 && q[0] < 0.01;
        // No reference point: the mask's top-left (21, 21) plus the phase → tile (0, 0) at (22, 21).
        assert!(is_blue(px(&l, 22, 21)) && !is_blue(px(&l, 24, 21)));
        // fxrp (10, 6) plus the phase: tile (0, 0) at (11, 6), so (23, 22) is tile (0, 0) again.
        l.effects.reference = Some((10.0, 6.0));
        assert_eq!(crate::fill_pattern_anchor(&l, canvas), (10.0, 6.0));
        assert!(is_blue(px(&l, 23, 22)) && !is_blue(px(&l, 25, 22)));
        // Unlinked: the canvas origin plus the phase.
        l.content = LayerContent::Fill(pattern_fill(&p, false, (1.0, 0.0)));
        assert!(is_blue(px(&l, 21, 24)) && !is_blue(px(&l, 23, 24)));
    }

    /// Pattern strokes paint too; a missing pattern paints nothing (as a pattern fill layer).
    #[test]
    fn pattern_strokes_paint_and_missing_patterns_are_transparent() {
        let p = checker();
        let pats = [p.clone()];
        let ctx = ShapePatterns { patterns: &pats, mode: ColorMode::Rgb, reference: None };
        let canvas = Rect::new(0, 0, 20, 16);
        let solid = Fill::Solid(Color::rgb(0.0, 1.0, 0.0));
        let sh = shape(Some(solid.clone()), Some(pattern_fill(&p, true, (0.0, 0.0))));
        let s = render_shape(&sh, PixelFormat::RGBA8, canvas, ctx);
        // (5, 4): inside the 2 px inside stroke; tile (1, 0), blue. (10, 8): the green fill.
        assert!(blue(&s.pixel(5, 4)), "{:?}", s.pixel(5, 4));
        assert_eq!(s.pixel(10, 8), vec![0.0, 1.0, 0.0, 1.0]);
        let missing = Fill::Pattern { name: "gone".into(), scale: 1.0, id: "gone".into(), angle: 0.0, link: true, phase: (0.0, 0.0) };
        let none = render_shape(&shape(Some(missing), None), PixelFormat::RGBA8, canvas, ctx);
        assert!(none.content_bounds().is_empty());
        assert!(uses_patterns(&sh) && !uses_patterns(&shape(Some(solid.clone()), None)));
        assert!(moves_with_layer(&sh, Some((0.0, 0.0))) && !moves_with_layer(&sh, None));
        let unlinked = shape(None, Some(pattern_fill(&p, false, (0.0, 0.0))));
        assert!(!moves_with_layer(&unlinked, Some((0.0, 0.0))) && moves_with_layer(&shape(Some(solid), None), None));
        assert_eq!(ctx.used_by(&sh).len(), 1);
        // No pattern table: transparent, as photosplice-vector renders it.
        let bare = render_shape(&shape(Some(pattern_fill(&p, true, (0.0, 0.0))), None), PixelFormat::RGBA8, canvas, ShapePatterns::NONE);
        assert!(bare.content_bounds().is_empty());
    }

    /// A stroked pattern shape with a clipped layer composites from its split fill and vector
    /// stroke (`shape_split`): the fill there samples the pattern too, at every depth, matching
    /// the plain shape's own pixels, with the clipped layer over it (a fill without the pattern
    /// would leave the pattern to the stroke part, drawn above the clipped layer).
    #[test]
    fn split_shapes_paint_their_patterns() {
        let p = checker();
        for depth in photosplice_color::SampleType::ALL {
            let mut d = Document::with_background("t", photosplice_doc::Size::new(20, 16), ColorMode::Rgb, depth, Color::WHITE);
            d.patterns.push(p.clone());
            let sh = shape(Some(pattern_fill(&p, true, (1.0, 0.0))), Some(Fill::Solid(Color::rgb(0.0, 1.0, 0.0))));
            let mut l = Layer::new("s", LayerContent::Shape(sh.clone()));
            l.effects.reference = Some((1.0, 0.0));
            let plain = render_layer_shape(&d, &l).expect("shape");
            if let LayerContent::Shape(s) = &mut l.content {
                s.cache = Some(plain.clone());
            }
            d.layers.push(l);
            // A clipped pixel inside the fill, which splits the shape.
            let mut c = Layer::raster("clip", d.pixel_format());
            if let Some(s) = c.surface_mut() {
                s.fill_rect(Rect::new(8, 6, 9, 7), &[1.0, 0.0, 1.0, 1.0][..s.format().channels().min(4)]);
            }
            c.clipped = true;
            d.layers.push(c);
            let out = crate::render(&d, d.bounds());
            for (x, y) in [(10, 7), (11, 7), (10, 9), (12, 9)] {
                let want = plain.pixel(x, y);
                let got = out.px[(y * 20 + x) as usize];
                assert!((0..3).all(|k| (want[k] - got[k]).abs() < 1.5 / 255.0), "{depth:?} ({x},{y}): {got:?} vs {want:?}");
            }
            let clip = out.px[6 * 20 + 8];
            assert!(clip[0] > 0.99 && clip[1] < 0.01 && clip[2] > 0.99, "{depth:?}: clipped layer over the fill {clip:?}");
            // The two probes in different tile quarters differ: the pattern shows, not a flat fill.
            assert!((out.px[7 * 20 + 10][0] - out.px[7 * 20 + 12][0]).abs() > 0.5, "{depth:?}");
        }
    }

    /// In a Grayscale document a pattern paints its grey (what Edit › Fill writes), at every depth.
    #[test]
    fn shape_patterns_paint_in_the_document_mode() {
        let p = checker();
        let mut doc = Document::new("d", photosplice_doc::Size::new(20, 16), ColorMode::Grayscale, photosplice_color::SampleType::U16);
        doc.patterns.push(p.clone());
        let layer = Layer::new("s", LayerContent::Shape(shape(Some(pattern_fill(&p, true, (0.0, 0.0))), None)));
        let s = render_layer_shape(&doc, &layer).expect("shape layer");
        assert_eq!(s.format(), doc.pixel_format());
        let (b, r) = (s.pixel(8, 4), s.pixel(10, 4));
        assert!(b.first().zip(r.first()).is_some_and(|(b, r)| (b - r).abs() > 0.05), "{b:?} {r:?}");
        assert!(render_layer_shape(&doc, &Layer::new("r", LayerContent::Raster(Surface::new(doc.pixel_format())))).is_none());
    }
}
