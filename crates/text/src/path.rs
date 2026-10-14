//! Type on a path: arc-length sampling of a [`TextPath`] and glyph placement along it.

use photosuite_doc::text::TextPath;

use crate::layout::{GlyphOrient, TextLayout};

/// Flattening steps per cubic segment.
const STEPS: usize = 48;
/// Segments beyond this are ignored (hostile files).
const MAX_SEGMENTS: usize = 4096;

/// A path flattened to a polyline with cumulative arc lengths.
#[derive(Clone, Debug)]
pub struct PathSampler {
    pts: Vec<[f32; 2]>,
    /// Arc length at each point (`cum[0] == 0`).
    cum: Vec<f32>,
    closed: bool,
}

fn cubic([p0, c1, c2, p1]: &[[f32; 2]; 4], t: f32) -> [f32; 2] {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    [a * p0[0] + b * c1[0] + c * c2[0] + d * p1[0], a * p0[1] + b * c1[1] + c * c2[1] + d * p1[1]]
}

impl PathSampler {
    /// `None` when the path has no finite length.
    pub fn new(path: &TextPath) -> Option<Self> {
        let mut pts: Vec<[f32; 2]> = Vec::new();
        for seg in path.segments.iter().take(MAX_SEGMENTS) {
            if !seg.iter().flatten().all(|v| v.is_finite()) {
                return None;
            }
            let first = if pts.is_empty() { 0 } else { 1 };
            pts.extend((first..=STEPS).map(|i| cubic(seg, i as f32 / STEPS as f32)));
        }
        let mut cum = Vec::with_capacity(pts.len());
        let mut total = 0.0f32;
        let mut prev: Option<[f32; 2]> = None;
        for p in &pts {
            if let Some(q) = prev {
                total += (p[0] - q[0]).hypot(p[1] - q[1]);
            }
            cum.push(total);
            prev = Some(*p);
        }
        (total.is_finite() && total > 1e-3).then_some(Self { pts, cum, closed: path.closed })
    }

    pub fn length(&self) -> f32 {
        self.cum.last().copied().unwrap_or(0.0)
    }

    /// The point and unit tangent at arc length `s`; `None` past the ends of an open path.
    pub fn at(&self, s: f32) -> Option<([f32; 2], [f32; 2])> {
        let len = self.length();
        let s = if self.closed { s.rem_euclid(len) } else { s };
        if !s.is_finite() || !(-1e-3..=len + 1e-3).contains(&s) {
            return None;
        }
        let i = self.cum.partition_point(|&c| c < s).clamp(1, self.pts.len().saturating_sub(1));
        let (a, b) = (*self.pts.get(i - 1)?, *self.pts.get(i)?);
        let (ca, cb) = (*self.cum.get(i - 1)?, *self.cum.get(i)?);
        let seg = cb - ca;
        let t = if seg > 0.0 { ((s - ca) / seg).clamp(0.0, 1.0) } else { 0.0 };
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let d = dx.hypot(dy);
        let tan = if d > 0.0 { [dx / d, dy / d] } else { [1.0, 0.0] };
        Some(([a[0] + dx * t, a[1] + dy * t], tan))
    }

    /// The arc length of the path point nearest to `(x, y)`, and its distance.
    pub fn project(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        let mut best: Option<(f32, f32)> = None;
        for (i, w) in self.pts.windows(2).enumerate() {
            let [a, b] = [w[0], w[1]];
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let l2 = dx * dx + dy * dy;
            let t = if l2 > 0.0 { (((x - a[0]) * dx + (y - a[1]) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
            let dist = (a[0] + dx * t - x).hypot(a[1] + dy * t - y);
            if best.is_none_or(|b| dist < b.1) {
                best = Some((self.cum.get(i).copied().unwrap_or(0.0) + l2.sqrt() * t, dist));
            }
        }
        best
    }
}

/// Bends a laid-out single-direction layout onto `path`: each glyph's advance centre goes to arc
/// length `start + x` on the path, the glyph turned to the path's direction with its baseline on
/// the path (lines below the first stand off it by their baseline). Glyphs past the ends of an
/// open path are dropped; underline and strikethrough are not drawn.
pub(crate) fn place_on_path(out: &mut TextLayout, advances: &[f32], path: &TextPath) {
    let Some(sampler) = PathSampler::new(path) else { return };
    let start = if path.start.is_finite() { path.start * sampler.length() } else { 0.0 };
    let base = out.lines.first().map_or(0.0, |l| l.baseline);
    let mut kept = Vec::with_capacity(out.glyphs.len());
    for (g, &adv) in out.glyphs.iter().zip(advances) {
        let half = adv * 0.5;
        let Some((p, [c, s])) = sampler.at(start + g.x + half) else { continue };
        let dy = g.y - base;
        let mut g = *g;
        // Rotation [c −s; s c] applied to the glyph-local offset (−half, dy).
        g.x = p[0] - c * half - s * dy;
        g.y = p[1] - s * half + c * dy;
        g.orient = GlyphOrient::Path { cos: c, sin: s };
        kept.push(g);
    }
    out.glyphs = kept;
    out.decorations.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TextEngine;
    use photosuite_doc::TextLayer;
    use photosuite_doc::text::{CharStyle, TextRun};

    fn layer(text: &str, path: Option<TextPath>) -> TextLayer {
        let style = CharStyle { size_pt: 20.0, ..CharStyle::default() };
        TextLayer { text: text.into(), runs: vec![TextRun { len: text.len(), style }], path, ..Default::default() }
    }

    fn line(x0: f32, x1: f32) -> TextPath {
        TextPath { segments: vec![[[x0, 0.0], [x0 + (x1 - x0) / 3.0, 0.0], [x0 + 2.0 * (x1 - x0) / 3.0, 0.0], [x1, 0.0]]], closed: false, start: 0.0 }
    }

    #[test]
    fn samples_a_straight_path_by_arc_length() {
        let s = PathSampler::new(&line(10.0, 110.0)).unwrap();
        assert!((s.length() - 100.0).abs() < 1e-3);
        let (p, t) = s.at(25.0).unwrap();
        assert!((p[0] - 35.0).abs() < 1e-3 && p[1].abs() < 1e-6 && (t[0] - 1.0).abs() < 1e-6);
        assert!(s.at(101.0).is_none(), "an open path ends");
        assert!((s.project(60.0, 7.0).unwrap().0 - 50.0).abs() < 1e-3);
    }

    #[test]
    fn closed_paths_wrap_and_bad_paths_are_rejected() {
        let mut p = line(0.0, 50.0);
        p.closed = true;
        let s = PathSampler::new(&p).unwrap();
        assert!((s.at(60.0).unwrap().0[0] - 10.0).abs() < 1e-3);
        assert!(PathSampler::new(&TextPath::default()).is_none());
        assert!(PathSampler::new(&TextPath { segments: vec![[[f32::NAN, 0.0]; 4]], ..Default::default() }).is_none());
        assert!(PathSampler::new(&TextPath { segments: vec![[[3.0, 3.0]; 4]], ..Default::default() }).is_none(), "zero length");
    }

    #[test]
    fn glyphs_follow_the_path() {
        let mut e = TextEngine::new();
        let flat = e.layout(&layer("Hamburg", None), 72.0);
        // A straight path from (10, 50): the point layout, moved.
        let mut p = line(10.0, 410.0);
        p.segments[0].iter_mut().for_each(|q| q[1] = 50.0);
        let on = e.layout(&layer("Hamburg", Some(p.clone())), 72.0);
        assert_eq!(on.glyphs.len(), flat.glyphs.len());
        for (a, b) in flat.glyphs.iter().zip(&on.glyphs) {
            assert!((b.x - a.x - 10.0).abs() < 0.05 && (b.y - a.y - 50.0).abs() < 0.05, "{a:?} → {b:?}");
            assert!(matches!(b.orient, GlyphOrient::Path { cos, sin } if (cos - 1.0).abs() < 1e-4 && sin.abs() < 1e-4));
        }
        // Running down the canvas, glyphs turn 90° clockwise; from the middle of the path on.
        let down = TextPath { segments: vec![[[0.0, 0.0], [0.0, 100.0], [0.0, 200.0], [0.0, 300.0]]], closed: false, start: 0.5 };
        let l = e.layout(&layer("H", Some(down)), 72.0);
        let g = l.glyphs.first().unwrap();
        assert!(matches!(g.orient, GlyphOrient::Path { cos, sin } if cos.abs() < 1e-4 && (sin - 1.0).abs() < 1e-4));
        assert!(g.x.abs() < 1e-3 && (g.y - 150.0).abs() < 1e-3, "{g:?}");
        // Past the end of an open path, glyphs are dropped.
        p.segments = vec![[[0.0, 0.0], [10.0, 0.0], [20.0, 0.0], [30.0, 0.0]]];
        assert!(e.layout(&layer("Hamburg", Some(p)), 72.0).glyphs.len() < flat.glyphs.len());
        // The glyphs render (rotated outlines included in the ink).
        let r = crate::render::ink_rect(&l, &photosuite_geom::Affine::IDENTITY);
        // Turned 90° clockwise, the glyph's top points right.
        assert!(r.width() > 0 && r.y0 >= 140 && r.x0 >= -2, "{r:?}");
    }

    #[test]
    fn degenerate_paths_leave_the_point_layout() {
        let mut e = TextEngine::new();
        let flat = e.layout(&layer("Hi", None), 72.0);
        for p in [TextPath::default(), TextPath { segments: vec![[[1.0, 1.0]; 4]], closed: true, start: f32::NAN }] {
            let l = e.layout(&layer("Hi", Some(p)), 72.0);
            assert_eq!(l.glyphs, flat.glyphs);
        }
        let l = e.layout(&layer("Hi", Some(TextPath { start: f32::INFINITY, ..line(0.0, 100.0) })), 72.0);
        assert_eq!(l.glyphs.len(), flat.glyphs.len(), "a bad start falls back to the path start");
    }
}
