//! Align Edges: a shape drawn snapped to the pixel grid.
//!
//! Measured on Reference (the same paths saved with and without the flag, rendered by it): the
//! shape's bounding box (the curve's own extent, not its control points) moves to whole pixels,
//! its corner and its size each rounded, and the path is scaled onto that box. A rectangle at
//! (10.3, 10.6) sized 30.4 × 19.7 lands on (10, 11) sized 30 × 20, so its right edge at 40 (not
//! the 41 rounding 40.7 gives). The rounding is of 16.16 fixed-point values: a path coordinate
//! stored as 10.4999989 reads as 10.5 and rounds up. A centre or outside stroke snaps the
//! extent of the stroke's outline (miter tips included) instead, the path moving with it: a 2.5 px
//! centre stroke lands its outer edges, not the path, on the grid.

use photosplice_doc::Path;
use photosplice_geom::{Affine, Point};

/// The extent of `path`'s curves `(x0, y0, x1, y1)`: segment ends plus the extremes of each
/// cubic, where its derivative is zero.
pub fn tight_bounds(path: &Path) -> Option<(f64, f64, f64, f64)> {
    let mut b: Option<(f64, f64, f64, f64)> = None;
    let mut add = |p: Point| {
        if !(p.x.is_finite() && p.y.is_finite()) {
            return;
        }
        b = Some(match b {
            Some((x0, y0, x1, y1)) => (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y)),
            None => (p.x, p.y, p.x, p.y),
        });
    };
    for s in &path.subpaths {
        // A lone knot is a point.
        if s.knots.len() == 1 {
            if let Some(k) = s.knots.first() {
                add(k.anchor);
            }
            continue;
        }
        for [p0, p1, p2, p3] in s.segments() {
            add(p0);
            add(p3);
            let at = |t: f64| {
                let u = 1.0 - t;
                let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                Point::new(a * p0.x + b * p1.x + c * p2.x + d * p3.x, a * p0.y + b * p1.y + c * p2.y + d * p3.y)
            };
            for (v0, v1, v2, v3) in [(p0.x, p1.x, p2.x, p3.x), (p0.y, p1.y, p2.y, p3.y)] {
                for t in cubic_extrema(v0, v1, v2, v3) {
                    add(at(t));
                }
            }
        }
    }
    b
}

/// Parameters in (0, 1) where a cubic Bézier coordinate has a zero derivative.
fn cubic_extrema(v0: f64, v1: f64, v2: f64, v3: f64) -> Vec<f64> {
    // B'(t)/3 = a t² + b t + c.
    let a = -v0 + 3.0 * v1 - 3.0 * v2 + v3;
    let b = 2.0 * (v0 - 2.0 * v1 + v2);
    let c = v1 - v0;
    let mut out = Vec::new();
    if a.abs() < 1e-12 {
        if b.abs() > 1e-12 {
            out.push(-c / b);
        }
    } else {
        let disc = b * b - 4.0 * a * c;
        if disc >= 0.0 {
            let r = disc.sqrt();
            out.extend([(-b + r) / (2.0 * a), (-b - r) / (2.0 * a)]);
        }
    }
    out.retain(|t| t.is_finite() && *t > 0.0 && *t < 1.0);
    out
}

/// Rounds as Reference does, on the value quantised to 16.16 fixed point.
fn round_fixed(v: f64) -> f64 {
    ((v * 65536.0).round() / 65536.0).round()
}

/// The map Align Edges applies to a shape drawn over `extent` `(x0, y0, x1, y1)`: that box onto
/// whole pixels, its corner and size rounded. An axis whose size would round to nothing only
/// moves. `None` for a box that isn't finite.
pub fn align_transform(extent: (f64, f64, f64, f64)) -> Option<Affine> {
    let (x0, y0, x1, y1) = extent;
    let axis = |lo: f64, hi: f64| -> (f64, f64) {
        let (start, size) = (round_fixed(lo), round_fixed(hi - lo));
        if hi - lo > 1e-9 && size > 0.0 { (size / (hi - lo), start - lo * size / (hi - lo)) } else { (1.0, start - lo) }
    };
    let ((sx, tx), (sy, ty)) = (axis(x0, x1), axis(y0, y1));
    [sx, tx, sy, ty].iter().all(|v| v.is_finite()).then_some(Affine { m: [sx, 0.0, 0.0, sy, tx, ty] })
}

/// `path` with Align Edges applied to what it fills: its tight bounding box onto whole pixels.
/// A shape whose stroke reaches past the path snaps the stroke's extent instead (see
/// `CompiledShape::new`).
pub fn align_edges(path: &Path) -> Path {
    match tight_bounds(path).and_then(align_transform) {
        Some(a) => path.transform(&a),
        None => path.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shapes;

    fn bounds(p: &Path) -> (f64, f64, f64, f64) {
        tight_bounds(p).unwrap()
    }

    fn close(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)) -> bool {
        [(a.0, b.0), (a.1, b.1), (a.2, b.2), (a.3, b.3)].iter().all(|(x, y)| (x - y).abs() < 1e-6)
    }

    /// Reference corpus (align kit, synthetic.psd): "rect frac" lands on (10, 11)–(40, 31),
    /// "rect half" on (51, 11)–(81, 31).
    #[test]
    fn rectangles_snap_their_corner_and_size() {
        assert!(close(bounds(&align_edges(&shapes::rect(10.3, 10.6, 30.4, 19.7))), (10.0, 11.0, 40.0, 31.0)));
        assert!(close(bounds(&align_edges(&shapes::rect(50.5, 10.5, 30.0, 20.0))), (51.0, 11.0, 81.0, 31.0)));
        // As read back from a PSD's 8.24 path records on a 200 × 150 canvas: 10.4999989 → 11.
        assert!(close(bounds(&align_edges(&shapes::rect(50.499_999_5, 10.499_998_9, 30.0, 20.0))), (51.0, 11.0, 81.0, 31.0)));
    }

    #[test]
    fn curves_use_their_own_extent() {
        // An ellipse's control points stand outside its curve only along the tangents; the arc of
        // a circle through control points far out reaches less far than they do.
        let e = shapes::ellipse(130.3, 10.4, 30.6, 20.3);
        assert!(close(bounds(&e), (130.3, 10.4, 160.9, 30.7)));
        assert!(close(bounds(&align_edges(&e)), (130.0, 10.0, 161.0, 30.0)));
        let mut p = Path::new(vec![photosplice_doc::vector::Subpath::polygon(&[(0.0, 0.0), (10.0, 0.0)])]);
        p.subpaths[0].knots[0].out_ctrl = Point::new(0.0, -8.0);
        p.subpaths[0].knots[1].in_ctrl = Point::new(10.0, -8.0);
        let (_, top, _, _) = bounds(&p);
        assert!((top + 6.0).abs() < 1e-9, "{top}");
    }

    #[test]
    fn degenerate_paths_do_not_break() {
        assert_eq!(align_edges(&Path::default()), Path::default());
        // A horizontal line has no height: it only moves.
        let line = Path::new(vec![photosplice_doc::vector::Subpath::polyline(&[(0.4, 5.3), (9.6, 5.3)])]);
        assert!(close(bounds(&align_edges(&line)), (0.0, 5.0, 9.0, 5.0)));
        let mut nan = shapes::rect(1.0, 1.0, 2.0, 2.0);
        nan.subpaths[0].knots[0].anchor = Point::new(f64::NAN, 1.0);
        let _ = align_edges(&nan);
    }

    fn shape(path: Path, width: f32, align: photosplice_doc::vector::StrokeAlign, align_edges: bool) -> photosplice_doc::ShapeLayer {
        use photosplice_color::Color;
        use photosplice_doc::{Fill, vector::ShapeStroke};
        let stroke = ShapeStroke { width, align, paint: Fill::Solid(Color::rgb(1.0, 0.0, 0.0)), ..Default::default() };
        photosplice_doc::ShapeLayer { path, fill: Some(Fill::Solid(Color::BLACK)), stroke: Some(stroke), align_edges, ..Default::default() }
    }

    fn px(sh: &photosplice_doc::ShapeLayer, x: i32, y: i32) -> [f32; 4] {
        let r = photosplice_geom::Rect::new(x, y, x + 1, y + 1);
        crate::CompiledShape::new(sh, crate::DEFAULT_TOLERANCE, photosplice_geom::Rect::new(0, 0, 200, 150)).render_rgba(r)[0]
    }

    /// Align kit, synthetic.psd as Reference draws it: a 3 px inside stroke on a rectangle whose
    /// left edge is at 90.25 shows pure stroke colour at the path's coverage there (195/255),
    /// not the stroke composited over the partly covered fill (239).
    #[test]
    fn inside_and_outside_strokes_share_edge_pixels_with_the_fill() {
        use photosplice_doc::vector::StrokeAlign;
        let inside = shape(shapes::rect(90.25, 10.75, 29.9, 20.35), 3.0, StrokeAlign::Inside, false);
        let p = px(&inside, 90, 20);
        assert!((p[3] - 0.75).abs() < 0.01 && (p[0] - 1.0).abs() < 1e-3 && p[1] < 1e-3, "{p:?}");
        // An outside stroke fills the rest of a half-covered edge pixel: fully opaque, half each.
        let outside = shape(shapes::rect(10.5, 10.0, 40.0, 30.0), 1.5, StrokeAlign::Outside, false);
        let p = px(&outside, 10, 20);
        assert!((p[3] - 1.0).abs() < 0.01 && (p[0] - 0.5).abs() < 0.02, "{p:?}");
    }

    /// Align kit: a centre or outside stroke lands its outer edge on the grid, the path moving
    /// with it ("rect center 2.5" at (140.3, 50.4) sized 30.4 × 30.3: outer edges at whole pixels).
    #[test]
    fn stroked_shapes_snap_the_stroke_extent() {
        use photosplice_doc::vector::StrokeAlign;
        let sh = shape(shapes::rect(140.3, 50.4, 30.4, 30.3), 2.5, StrokeAlign::Center, true);
        // Outer box 139.05..171.95 → 139..172 (size 32.9 → 33): the first and last columns are
        // whole stroke pixels, the ones beyond empty.
        assert!(px(&sh, 139, 60)[3] > 0.99 && px(&sh, 171, 60)[3] > 0.99, "{:?} {:?}", px(&sh, 139, 60), px(&sh, 171, 60));
        assert!(px(&sh, 138, 60)[3] < 0.01 && px(&sh, 172, 60)[3] < 0.01);
        // Off, the same shape's outer edge pixel is partly covered.
        let free = photosplice_doc::ShapeLayer { align_edges: false, ..sh };
        assert!(px(&free, 139, 60)[3] < 0.99);
    }
}
