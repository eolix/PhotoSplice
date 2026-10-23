//! CPU reference renderer for layer effects (layer styles).
//!
//! Render order, bottom to top (Photoshop's layer-style stack read upwards):
//! drop shadows, outer glows, outer bevel, then the layer itself (content at
//! fill opacity) with pattern/gradient/colour overlays, satin, inner glows,
//! inner shadows, strokes and the inner bevel on top. Exterior effects blend
//! straight into the backdrop with their own modes; the layer blends with its
//! mode and the interior effects paint over the result, or blend with it as a
//! group (Blend Interior Effects as Group, [`premerges_layer`]). Emboss and pillow emboss then
//! shade the composited result (inside and outside halves). Layer opacity
//! applies to the whole stack; fill opacity only to the layer's own pixels.
//!
//! Interior effects are painted relative to the layer's shape and then take the shape's alpha
//! (an overlay recolours a half-transparent edge without adding coverage). The outside parts
//! of strokes blend onto the backdrop with their own modes, an upper stroke covering the
//! lower ones. Drop shadows are knocked out through see-through fill, and beneath the whole shape
//! when the layer's mode lets what's under it show through (see [`shadow_see_through`]).
//!
//! Shapes come from the layer's alpha (after its mask). A stroke's outer edge, and precise glows /
//! chiselled bevels, use an exact Euclidean transform: a 5 × 5 chamfer's level sets are polygons,
//! which a wide stroke shows as flat facets (#3646). Inside stroke edges and spread / choke keep the
//! cheaper chamfer. Soft falloffs are two box blurs (a tent).

use photosplice_color::blend::BlendMode;
use photosplice_doc::Pattern;
use photosplice_doc::{
    Bevel, BevelStyle, BevelTechnique, Contour, Effect, FxPaint, GlobalLight, Glow, GlowSource, GlowTechnique, Gradient, Layer, Shadow,
    StrokePosition,
};
use photosplice_geom::Rect;

use crate::pattern::{PREPARED_PATTERN_BYTES, Placement, PreparedPatterns};
use crate::{Buffer, psblend};

/// `true` if the layer has at least one enabled effect and the master switch is on.
pub fn has_effects(layer: &Layer) -> bool {
    layer.effects.enabled && layer.effects.items.iter().any(Effect::enabled)
}

/// Pixels an effect stack can reach beyond the layer's shape.
pub fn margin(layer: &Layer) -> i32 {
    let mut m = 0.0f32;
    for e in layer.effects.items.iter().filter(|e| e.enabled()) {
        let r = match e {
            Effect::DropShadow(s) | Effect::InnerShadow(s) => s.distance + s.size,
            Effect::OuterGlow(g) | Effect::InnerGlow(g) => g.size,
            Effect::Stroke(s) => s.size,
            Effect::Satin(s) => satin_distance(s) + s.size,
            Effect::BevelEmboss(b) => b.size + b.soften,
            _ => 0.0,
        };
        if r.is_finite() {
            m = m.max(r);
        }
    }
    // Bounded so malformed values can't request huge buffers; effects that
    // reach further are clipped at this distance.
    m.clamp(0.0, MAX_REACH).ceil() as i32 + 2
}

/// Maximum distance (px) an effect is rendered from the layer's shape.
pub const MAX_REACH: f32 = 512.0;

/// Whether every enabled effect's maps are local: each map pixel depends only on the shape
/// within [`margin`] of it (shifts, chamfer distances compared with a size, tent blurs), so
/// maps built over a part of the layer's region padded by twice the margin equal the
/// whole-region maps over that part (#1909). Not local: gradient strokes (laid out over the whole
/// shape's extent), Precise glows and chiselled bevels (exact Euclidean distances, whose f32
/// arithmetic depends on where the region starts) and noise (it speckles any coverage above
/// zero, so it would pick up the blur's float noise, which depends on where the region starts).
pub(crate) fn maps_are_local(layer: &Layer) -> bool {
    layer.effects.items.iter().filter(|e| e.enabled()).all(|e| match e {
        Effect::Stroke(s) => !matches!(s.paint, FxPaint::Gradient(_)),
        Effect::DropShadow(s) | Effect::InnerShadow(s) => s.noise <= 0.0,
        Effect::OuterGlow(g) | Effect::InnerGlow(g) => g.technique != GlowTechnique::Precise && g.noise <= 0.0,
        Effect::BevelEmboss(b) => b.technique == BevelTechnique::Smooth,
        _ => true,
    })
}

/// A single-channel map over a rectangle.
#[derive(Clone)]
struct Map {
    w: usize,
    h: usize,
    v: Vec<f32>,
}

impl Map {
    fn new(w: usize, h: usize, fill: f32) -> Self {
        Map { w, h, v: vec![fill; w * h] }
    }
    fn get(&self, x: i64, y: i64) -> f32 {
        if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 { 0.0 } else { self.v[y as usize * self.w + x as usize] }
    }
    /// Shifted copy (bilinear), outside reads `outside`.
    fn shifted(&self, dx: f32, dy: f32, outside: f32) -> Map {
        let mut out = Map::new(self.w, self.h, 0.0);
        // A hostile distance (±1e30, inf, NaN) would overflow the i64 offsets below; no shift beyond
        // 2^30 px can land inside a map, so clamp (non-finite shifts read as none).
        let reach = |d: f32| if d.is_finite() { d.clamp(-1.0e9, 1.0e9) } else { 0.0 };
        let (dx, dy) = (reach(dx), reach(dy));
        let (fx, fy) = (dx.floor(), dy.floor());
        let (ax, ay) = (dx - fx, dy - fy);
        let s = |x: i64, y: i64| if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 { outside } else { self.v[y as usize * self.w + x as usize] };
        for y in 0..self.h as i64 {
            for x in 0..self.w as i64 {
                let (sx, sy) = (x - fx as i64, y - fy as i64);
                let v00 = s(sx, sy);
                let v10 = s(sx - 1, sy);
                let v01 = s(sx, sy - 1);
                let v11 = s(sx - 1, sy - 1);
                let top = v00 * (1.0 - ax) + v10 * ax;
                let bot = v01 * (1.0 - ax) + v11 * ax;
                out.v[y as usize * self.w + x as usize] = top * (1.0 - ay) + bot * ay;
            }
        }
        out
    }
    fn map(mut self, f: impl Fn(f32) -> f32) -> Self {
        for v in &mut self.v {
            *v = f(*v);
        }
        self
    }
}

/// 1D squared distance transform (Felzenszwalb & Huttenlocher).
fn dt1(f: &[f32], out: &mut [f32], v: &mut [usize], z: &mut [f32], near: &mut [usize]) {
    let n = f.len();
    let mut k = 0usize;
    v[0] = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    for q in 1..n {
        loop {
            let p = v[k];
            let s = ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32)) / (2.0 * q as f32 - 2.0 * p as f32);
            if s <= z[k] && k > 0 {
                k -= 1;
                continue;
            }
            if s <= z[k] {
                // k == 0 and parabola dominates: replace.
                v[0] = q;
                z[0] = f32::NEG_INFINITY;
                z[1] = f32::INFINITY;
                break;
            }
            k += 1;
            v[k] = q;
            z[k] = s;
            z[k + 1] = f32::INFINITY;
            break;
        }
    }
    k = 0;
    for (q, o) in out.iter_mut().enumerate() {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        let p = v[k];
        let d = q as f32 - p as f32;
        *o = d * d + f[p];
        near[q] = p;
    }
}

/// Exact Euclidean distance from every pixel to the nearest pixel where
/// `inside` is true (0 on those pixels).
#[cfg(test)]
fn edt(inside: &[bool], w: usize, h: usize) -> Vec<f32> {
    edt_nearest(inside, w, h).0
}

/// Maps with fewer pixels (or other units of work) than this are not worth spawning threads for.
const PAR_MIN: usize = 1 << 16;

/// Most threads one pass uses (as for the blurs, [`rows_par`]).
const PAR_THREADS: usize = 16;

#[cfg(test)]
thread_local! {
    /// Test hook: the thread count [`par_threads`] gives on this thread, whatever the work.
    static THREADS: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// How many threads a pass over `work` pixels (or similar units) uses.
fn par_threads(work: usize) -> usize {
    #[cfg(test)]
    if let Some(n) = THREADS.with(std::cell::Cell::get) {
        return n;
    }
    if cfg!(target_arch = "wasm32") || work < PAR_MIN { 1 } else { std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, PAR_THREADS) }
}

/// Runs `f(scratch, u, a_u, b_u)` for every unit `u` of two slices: unit `u` of `a` is
/// `a[u * unit_a..][..unit_a]` (the last may be shorter), the same of `b` with `unit_b`, which is 0
/// when there is no second slice. `work` says how big the whole pass is ([`par_threads`]). Groups of
/// units go to scoped OS threads, each with a scratch value from `init`; not to rayon, as for
/// [`rows_par`]: maps are built from inside rayon tiles, where a waiting worker steals other tiles.
/// A unit is computed from its own arguments alone, so the result does not depend on the threads.
fn par_units<S, A: Send, B: Send>(
    work: usize,
    a: &mut [A],
    unit_a: usize,
    b: &mut [B],
    unit_b: usize,
    init: impl Fn() -> S + Sync,
    f: impl Fn(&mut S, usize, &mut [A], &mut [B]) + Sync,
) {
    if unit_a == 0 {
        return;
    }
    let units = a.len().div_ceil(unit_a);
    let threads = par_threads(work).min(units);
    if threads <= 1 {
        run_units(&mut init(), 0, a, unit_a, b, unit_b, &f);
        return;
    }
    // Many more groups than threads, so that a heavy one (an edge-rich stretch) does not hold the rest up.
    let group = units.div_ceil(threads * 8).max(1);
    let mut groups = Vec::new();
    let (mut rest_a, mut rest_b, mut first) = (a, b, 0usize);
    while !rest_a.is_empty() {
        let (ga, na) = rest_a.split_at_mut((group * unit_a).min(rest_a.len()));
        let (gb, nb) = rest_b.split_at_mut((group * unit_b).min(rest_b.len()));
        groups.push((first, ga, gb));
        (rest_a, rest_b, first) = (na, nb, first + group);
    }
    let queue = std::sync::Mutex::new(groups.into_iter());
    let worker = || {
        let mut scratch = init();
        loop {
            let next = queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner).next();
            let Some((first, ga, gb)) = next else { break };
            run_units(&mut scratch, first, ga, unit_a, gb, unit_b, &f);
        }
    };
    std::thread::scope(|sc| {
        // A thread that cannot be spawned leaves its share to the others.
        for _ in 1..threads {
            let _ = std::thread::Builder::new().spawn_scoped(sc, worker);
        }
        worker();
    });
}

fn run_units<S, A, B>(scratch: &mut S, first: usize, a: &mut [A], unit_a: usize, b: &mut [B], unit_b: usize, f: &impl Fn(&mut S, usize, &mut [A], &mut [B])) {
    for (k, ua) in a.chunks_mut(unit_a).enumerate() {
        let ub = b.get_mut(k * unit_b..((k + 1) * unit_b).min(b.len())).unwrap_or(&mut []);
        f(scratch, first + k, ua, ub);
    }
}

/// A `w × h` buffer of `fill`, then filled row by row through `f(y, row)`.
fn par_fill<T: Clone + Send>(w: usize, h: usize, fill: T, f: impl Fn(usize, &mut [T]) + Sync) -> Vec<T> {
    let mut out = vec![fill; w.saturating_mul(h)];
    par_units(out.len(), &mut out, w.max(1), &mut [] as &mut [()], 0, || (), |_, y, row, _| f(y, row));
    out
}

/// Working buffers of the 1D distance transform, for lines of up to `n` pixels.
struct Dt1Scratch {
    f: Vec<f32>,
    o: Vec<f32>,
    v: Vec<usize>,
    z: Vec<f32>,
    near: Vec<usize>,
}

impl Dt1Scratch {
    fn new(n: usize) -> Self {
        Dt1Scratch { f: vec![0.0; n], o: vec![0.0; n], v: vec![0; n], z: vec![0.0; n + 1], near: vec![0; n] }
    }
}

/// [`edt`] plus the index of the nearest `inside` pixel (as `u32`: a map past 2³² pixels has no
/// nearest pixel to give, and every distance is then "far").
///
/// Both passes run line by line in parallel: the columns, then the rows. The columns' results are
/// kept column by column (`gc[x * h + y]`) so that each thread writes its own stretch of them.
fn edt_nearest(inside: &[bool], w: usize, h: usize) -> (Vec<f32>, Vec<u32>) {
    const INF: f32 = 1e20;
    let total = w.saturating_mul(h);
    if total != inside.len() || total > u32::MAX as usize {
        return (vec![INF; inside.len()], vec![0; inside.len()]);
    }
    let n = w.max(h);
    let mut gc = vec![0.0f32; total];
    let mut nc = vec![0u32; total];
    par_units(
        total,
        &mut gc,
        h.max(1),
        &mut nc,
        h.max(1),
        || Dt1Scratch::new(n),
        |t, x, g, near| {
            for (y, f) in t.f[..h].iter_mut().enumerate() {
                *f = if inside[y * w + x] { 0.0 } else { INF };
            }
            dt1(&t.f[..h], &mut t.o[..h], &mut t.v, &mut t.z, &mut t.near[..h]);
            g.copy_from_slice(&t.o[..h]);
            for (d, &p) in near.iter_mut().zip(&t.near[..h]) {
                *d = p as u32;
            }
        },
    );
    let mut g = vec![0.0f32; total];
    let mut nearest = vec![0u32; total];
    par_units(
        total,
        &mut g,
        w.max(1),
        &mut nearest,
        w.max(1),
        || Dt1Scratch::new(n),
        |t, y, out, near| {
            for (x, f) in t.f[..w].iter_mut().enumerate() {
                *f = gc[x * h + y];
            }
            dt1(&t.f[..w], &mut t.o[..w], &mut t.v, &mut t.z, &mut t.near[..w]);
            for (x, (d, ne)) in out.iter_mut().zip(near.iter_mut()).enumerate() {
                *d = t.o[x].sqrt();
                let px = t.near[x];
                *ne = (nc[px * h + y] as usize * w + px) as u32;
            }
        },
    );
    (g, nearest)
}

/// Chamfer distance transform with a 5 × 5 mask of exact Euclidean step lengths (1, √2, √5),
/// plus the index of the nearest `inside` pixel (0 on inside pixels). Photoshop's effect
/// distances follow this metric: its stroke corners measure 3.236 = √5 + 1 at offset (1, 3)
/// and 3.650 = √5 + √2 at (2, 3), where the exact distances are 3.162 and 3.606.
#[cfg(test)]
fn chamfer_nearest(inside: &[bool], w: usize, h: usize) -> (Vec<f32>, Vec<usize>) {
    chamfer_from(inside.iter().map(|&b| if b { 0.0 } else { CHAMFER_INF }).collect(), w, h)
}

const CHAMFER_INF: f32 = 1e20;

/// [`chamfer_nearest`] from per-pixel start values (`CHAMFER_INF` = not a seed): the result is
/// the minimum over seeds of start value + path length, so partly covered seeds can start at
/// their sub-pixel edge offset.
fn chamfer_from(mut d: Vec<f32>, w: usize, h: usize) -> (Vec<f32>, Vec<usize>) {
    const S2: f32 = std::f32::consts::SQRT_2;
    const S5: f32 = 2.236_068;
    // Forward-pass neighbours (already visited in raster order); the backward pass mirrors them.
    const FWD: [(i64, i64, f32); 8] = [(-1, 0, 1.0), (-1, -1, S2), (0, -1, 1.0), (1, -1, S2), (-1, -2, S5), (1, -2, S5), (-2, -1, S5), (2, -1, S5)];
    let mut near: Vec<usize> = (0..w * h).collect();
    let (wi, hi) = (w as i64, h as i64);
    let relax = |x: i64, y: i64, sign: i64, d: &mut Vec<f32>, near: &mut Vec<usize>| {
        let i = (y * wi + x) as usize;
        let mut best = d[i];
        let mut bn = near[i];
        for &(ox, oy, wt) in &FWD {
            let (nx, ny) = (x + ox * sign, y + oy * sign);
            if nx < 0 || ny < 0 || nx >= wi || ny >= hi {
                continue;
            }
            let j = (ny * wi + nx) as usize;
            let c = d[j] + wt;
            if c < best {
                best = c;
                bn = near[j];
            }
        }
        d[i] = best;
        near[i] = bn;
    };
    for y in 0..hi {
        for x in 0..wi {
            relax(x, y, 1, &mut d, &mut near);
        }
    }
    for y in (0..hi).rev() {
        for x in (0..wi).rev() {
            relax(x, y, -1, &mut d, &mut near);
        }
    }
    (d, near)
}

/// Distance from each pixel centre outside the shape to the shape's edge.
/// Pixels with non-zero alpha are inside; the edge inside the nearest such
/// pixel is placed by its alpha (coverage), as Photoshop does. Inside pixels
/// get ≤ 0.
fn dist_outside(s: &Map) -> Vec<f32> {
    dist_outside_by(s, Metric::Euclidean)
}

/// Distance metric for effect distance fields.
#[derive(Clone, Copy, PartialEq)]
enum Metric {
    /// Exact Euclidean (effect edges).
    Euclidean,
    /// 5 × 5 chamfer (inside strokes, spread / choke; see [`chamfer_nearest`]).
    Chamfer,
}

/// Outside width from which a stroke's outer edge uses the exact Euclidean transform instead of
/// the fitted 5 × 5 chamfer.
///
/// The chamfer's level sets are octagons: measured against a point source, its distance exceeds
/// the Euclidean radius by up to `0.027 · r` px — 0.48 px at r = 18 (the widest outside stroke any
/// Photoshop oracle matches with the chamfer) and 1.6 px at r = 60, where a wide stroke's boundary
/// visibly facets (#3646). Half a pixel of deviation is reached at r ≈ 19, so strokes narrower
/// than 20 px keep the chamfer, whose sub-pixel edge seeding Photoshop matches at small offsets,
/// and wider ones stay round.
pub const STROKE_CHAMFER_MAX: f32 = 20.0;

/// Whether a stroke whose outside edge is `out_w` px wide uses the exact Euclidean transform (round)
/// rather than the 5 × 5 chamfer's octagon ([`STROKE_CHAMFER_MAX`]). The CPU compositor and the GPU
/// compositor's `program_with` both call this, so a stroke picks the same field in each.
pub fn stroke_outside_exact(out_w: f32) -> bool {
    out_w >= STROKE_CHAMFER_MAX
}

fn dist_outside_by(s: &Map, m: Metric) -> Vec<f32> {
    if m == Metric::Chamfer {
        // Seeds start at their sub-pixel edge offset (1 − coverage).
        let start = s.v.iter().map(|&a| if a > INSIDE_EPS { 1.0 - a.min(1.0) } else { CHAMFER_INF }).collect();
        let (d, _) = chamfer_from(start, s.w, s.h);
        return d.iter().zip(&s.v).map(|(d, &a)| if a > INSIDE_EPS { -0.5 } else { d - 0.5 }).collect();
    }
    let inside = covered(s, |a| a > INSIDE_EPS);
    let (d, near) = edt_nearest(&inside, s.w, s.h);
    par_fill(s.w, s.h, 0.0f32, |y, row| {
        for (x, out) in row.iter_mut().enumerate() {
            let i = y * s.w + x;
            *out = if inside[i] { -0.5 } else { d[i] - 0.5 + (1.0 - s.v[near[i] as usize].min(1.0)) };
        }
    })
}

/// Which pixels of `s` satisfy `test`, row by row in parallel.
fn covered(s: &Map, test: impl Fn(f32) -> bool + Sync) -> Vec<bool> {
    par_fill(s.w, s.h, false, |y, row| {
        for (e, &a) in row.iter_mut().zip(s.v.get(y * s.w..).unwrap_or_default()) {
            *e = test(a);
        }
    })
}

/// Distance from inside pixel `i` to the shape's edge as seen from its nearest not fully covered
/// pixel `seed`, at sub-pixel precision: the edge is taken as the straight line across the seed,
/// perpendicular to the alpha gradient there, `offset` pixels from the seed's centre (its
/// coverage places it). Measured along that normal rather than to the seed's centre, so curved
/// anti-aliased edges give a smooth field instead of the pixel staircase whose ripples,
/// differentiated and scaled by a bevel's depth, streaked chiselled bevels (the anti-aliased
/// Euclidean distance transform of Gustavson & Strand, 2011). Falls back to the seed-centre
/// distance `d` where the gradient is flat or the projection disagrees with it by more than a
/// pixel. (Outside distances keep the seed-centre distance: the projection measured no better
/// there on the oracles and slightly worse on precise glows.)
fn edge_distance(s: &Map, i: usize, seed: usize, d: f32, offset: f32) -> f32 {
    edge_distance_dir(s, i, seed, d, 1.0, offset)
}

/// [`edge_distance`] looking inwards (`sign` 1, alpha rising) or outwards (−1).
fn edge_distance_dir(s: &Map, i: usize, seed: usize, d: f32, sign: f32, offset: f32) -> f32 {
    let w = s.w.max(1);
    let (sx, sy) = ((seed % w) as i64, (seed / w) as i64);
    let plain = d - offset;
    let Some((nx, ny, len)) = edge_normal(s, sx, sy) else { return plain };
    let (px, py) = ((i % w) as f32 - sx as f32, (i / w) as f32 - sy as f32);
    let r = (px * px + py * py).sqrt();
    if r < 1e-3 {
        return plain;
    }
    let along = sign * (px * nx + py * ny);
    // The edge line is the right model for a seed on an edge or a gentle curve. At a corner the
    // distance is radial: a convex corner's facets fan around it, as Photoshop draws them
    // (projected on the corner pixel's diagonal normal, the whole corner wedge was one flat facet
    // with a seam). A seed whose 4-neighbours' edge directions differ from its own by more than
    // 30° (a square corner: 45°; a curve of radius 3 px: 19°) is a corner; below 15°, an edge.
    // Neighbours with a weak gradient (past the edge) say nothing about its direction.
    let straight = [(1, 0), (-1, 0), (0, 1), (0, -1)]
        .iter()
        .filter_map(|(dx, dy)| edge_normal(s, sx + dx, sy + dy))
        .filter(|n| n.2 >= 0.5 * len)
        .map(|(mx, my, _)| mx * nx + my * ny)
        .fold(1.0f32, f32::min);
    let t = ((straight - 0.866) / 0.1).clamp(0.0, 1.0);
    let projected = along - offset;
    if (projected - plain).abs() > 1.0 { plain } else { plain + t * (projected - plain) }
}

/// Unit direction of rising alpha at pixel (x, y) (3 × 3 Sobel) and the gradient's length, if the
/// alpha isn't flat there.
fn edge_normal(s: &Map, x: i64, y: i64) -> Option<(f32, f32, f32)> {
    let a = |dx: i64, dy: i64| s.get(x + dx, y + dy).min(1.0);
    let gx = (a(1, -1) + 2.0 * a(1, 0) + a(1, 1)) - (a(-1, -1) + 2.0 * a(-1, 0) + a(-1, 1));
    let gy = (a(-1, 1) + 2.0 * a(0, 1) + a(1, 1)) - (a(-1, -1) + 2.0 * a(0, -1) + a(1, -1));
    let len = (gx * gx + gy * gy).sqrt();
    (len >= 1e-3).then(|| (gx / len, gy / len, len))
}

/// [`dist_outside`] measured to the anti-aliased edge across its local direction (see
/// [`edge_distance`]), for bevels: their outside halves shade with the field's gradient, which the
/// seed-centre staircase streaked around curves. Glows keep [`dist_outside`], which matched
/// Photoshop's precise glows slightly better.
fn dist_outside_smooth(s: &Map) -> Vec<f32> {
    let d = aa_edt(&s.v, s.w, s.h);
    s.v.iter().zip(d).map(|(&a, d)| if a > INSIDE_EPS { -0.5 } else { d }).collect()
}

/// Distance from each pixel centre with no coverage in `cov` to the edge of what is covered, the
/// edge placed inside the anti-aliased pixels from their coverage and the direction the coverage
/// rises in (the anti-aliased Euclidean distance transform, Gustavson and Strand 2011). Covered
/// pixels read their own centre's distance to the edge (negative inside). A chisel bevel's
/// facets follow these distances: Reference's slopes are exactly radial around a disc, where a
/// nearest-pixel distance tilted them by up to 4° and changed their steepness by 6 %.
fn aa_edt(cov: &[f32], w: usize, h: usize) -> Vec<f32> {
    const FAR: f32 = 1.0e6;
    let n = w * h;
    let at = |x: i64, y: i64| if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 { 0.0 } else { cov[y as usize * w + x as usize].clamp(0.0, 1.0) };
    // The coverage gradient (3 × 3, √2-weighted), normalised: the edge's normal at each pixel.
    let r2 = std::f32::consts::SQRT_2;
    let grad: Vec<(f32, f32)> = (0..n)
        .map(|i| {
            let (x, y) = ((i % w) as i64, (i / w) as i64);
            let gx = -at(x - 1, y - 1) - r2 * at(x - 1, y) - at(x - 1, y + 1) + at(x + 1, y - 1) + r2 * at(x + 1, y) + at(x + 1, y + 1);
            let gy = -at(x - 1, y - 1) - r2 * at(x, y - 1) - at(x + 1, y - 1) + at(x - 1, y + 1) + r2 * at(x, y + 1) + at(x + 1, y + 1);
            let len = gx.hypot(gy);
            if len > 1e-6 { (gx / len, gy / len) } else { (0.0, 0.0) }
        })
        .collect();
    // Distance from a pixel's centre to the edge line through it (normal `(gx, gy)`, area `a`
    // covered): positive where the centre lies outside the covered part.
    let edge_df = |gx: f32, gy: f32, a: f32| -> f32 {
        if gx == 0.0 || gy == 0.0 {
            return 0.5 - a;
        }
        let len = gx.hypot(gy);
        let (mut gx, mut gy) = ((gx / len).abs(), (gy / len).abs());
        if gx < gy {
            std::mem::swap(&mut gx, &mut gy);
        }
        let a1 = 0.5 * gy / gx;
        if a < a1 {
            0.5 * (gx + gy) - (2.0 * gx * gy * a).sqrt()
        } else if a < 1.0 - a1 {
            (0.5 - a) * gx
        } else {
            -0.5 * (gx + gy) + (2.0 * gx * gy * (1.0 - a)).sqrt()
        }
    };
    let mut near: Vec<u32> = vec![u32::MAX; n];
    let mut dist = vec![FAR; n];
    for i in 0..n {
        let a = cov[i].clamp(0.0, 1.0);
        if a > 0.0 {
            near[i] = i as u32;
            dist[i] = if a < 1.0 { edge_df(grad[i].0, grad[i].1, a) } else { 0.0 };
        }
    }
    // Distance from pixel `i` to the edge at covered pixel `c`: along the vector between them,
    // through `c`'s own edge offset in that direction.
    let through = |i: usize, c: usize| -> f32 {
        let a = cov[c].clamp(0.0, 1.0);
        if a <= 0.0 {
            return FAR;
        }
        let (dx, dy) = ((i % w) as f32 - (c % w) as f32, (i / w) as f32 - (c / w) as f32);
        let di = dx.hypot(dy);
        if di == 0.0 { edge_df(grad[c].0, grad[c].1, a) } else { di + edge_df(dx, dy, a) }
    };
    let relax = |i: usize, j: usize, dist: &mut Vec<f32>, near: &mut Vec<u32>| -> bool {
        let c = near[j];
        if c == u32::MAX || c == near[i] {
            return false;
        }
        let d = through(i, c as usize);
        if d < dist[i] - 1e-3 {
            dist[i] = d;
            near[i] = c;
            return true;
        }
        false
    };
    // Sweeps down and up (each with a row pass both ways) until nothing changes.
    for _ in 0..16 {
        let mut changed = false;
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if cov[i] >= 1.0 {
                    continue;
                }
                if y > 0 {
                    changed |= relax(i, i - w, &mut dist, &mut near);
                    if x > 0 {
                        changed |= relax(i, i - w - 1, &mut dist, &mut near);
                    }
                    if x + 1 < w {
                        changed |= relax(i, i - w + 1, &mut dist, &mut near);
                    }
                }
                if x > 0 {
                    changed |= relax(i, i - 1, &mut dist, &mut near);
                }
            }
            for x in (0..w.saturating_sub(1)).rev() {
                let i = y * w + x;
                if cov[i] < 1.0 {
                    changed |= relax(i, i + 1, &mut dist, &mut near);
                }
            }
        }
        for y in (0..h).rev() {
            for x in (0..w).rev() {
                let i = y * w + x;
                if cov[i] >= 1.0 {
                    continue;
                }
                if y + 1 < h {
                    changed |= relax(i, i + w, &mut dist, &mut near);
                    if x + 1 < w {
                        changed |= relax(i, i + w + 1, &mut dist, &mut near);
                    }
                    if x > 0 {
                        changed |= relax(i, i + w - 1, &mut dist, &mut near);
                    }
                }
                if x + 1 < w {
                    changed |= relax(i, i + 1, &mut dist, &mut near);
                }
            }
            for x in 1..w {
                let i = y * w + x;
                if cov[i] < 1.0 {
                    changed |= relax(i, i - 1, &mut dist, &mut near);
                }
            }
        }
        if !changed {
            break;
        }
    }
    dist
}

/// Distance from each pixel centre inside the shape to the shape's edge
/// (≤ 0 outside).
fn dist_inside(s: &Map) -> Vec<f32> {
    let outside: Vec<f32> = s.v.iter().map(|a| 1.0 - a.clamp(0.0, 1.0)).collect();
    let d = aa_edt(&outside, s.w, s.h);
    s.v.iter().zip(d).map(|(&a, d)| if a <= INSIDE_EPS { -0.5 } else { d }).collect()
}

fn dist_inside_by(s: &Map, m: Metric) -> Vec<f32> {
    if m == Metric::Chamfer {
        // Partly covered edge pixels (alpha below their neighbourhood's) are seeds starting at
        // their coverage, mirroring `dist_outside`: a 25 % edge column puts the edge 0.25 px in.
        let cov = local_coverage(s);
        let start =
            s.v.iter()
                .zip(&cov.v)
                .map(|(&a, &c)| {
                    if a <= INSIDE_EPS {
                        0.0
                    } else if c < 1.0 - INSIDE_EPS {
                        c
                    } else {
                        CHAMFER_INF
                    }
                })
                .collect();
        let (d, _) = chamfer_from(start, s.w, s.h);
        return d.iter().zip(&s.v).map(|(d, &a)| if a <= INSIDE_EPS { -0.5 } else { d - 0.5 }).collect();
    }
    let outside = covered(s, |a| a <= INSIDE_EPS);
    let (d, _) = edt_nearest(&outside, s.w, s.h);
    // Fully covered pixels measure to the anti-aliased edge: seeds are the pixels not fully
    // covered, the edge `½ − coverage` beyond their centre (see `edge_distance`). Edge pixels
    // keep the distance to the nearest uncovered pixel.
    let seeds: Vec<bool> = s.v.iter().map(|&a| a < 1.0 - INSIDE_EPS).collect();
    let (ds, near) = edt_nearest(&seeds, s.w, s.h);
    par_fill(s.w, s.h, 0.0f32, |y, row| {
        for (x, out) in row.iter_mut().enumerate() {
            let i = y * s.w + x;
            *out = if outside[i] {
                -0.5
            } else if seeds[i] {
                d[i] - 0.5
            } else {
                let q = near.get(i).map_or(i, |&q| q as usize);
                edge_distance(s, i, q, ds[i], 0.5 - s.v.get(q).map_or(1.0, |a| a.min(1.0))).max(0.0)
            };
        }
    })
}

/// Alpha relative to the 3×3 neighbourhood maximum: separates edge coverage
/// from the fill's own opacity on shape layers.
fn local_coverage(s: &Map) -> Map {
    let mut out = s.clone();
    for y in 0..s.h as i64 {
        for x in 0..s.w as i64 {
            let a = s.get(x, y);
            if a <= INSIDE_EPS {
                continue;
            }
            let mut mx = a;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    mx = mx.max(s.get(x + dx, y + dy));
                }
            }
            out.v[y as usize * s.w + x as usize] = (a / mx).min(1.0);
        }
    }
    out
}

/// Alpha above which a pixel belongs to the layer's shape (half an 8-bit step).
const INSIDE_EPS: f32 = 0.5 / 255.0;

/// The soft falloff of an effect of `size` pixels: two box passes of width `size` (a tent),
/// as Photoshop does. Fitted on psd-tools layer_effects.psd against the earlier Gaussian
/// (sigma = 0.4 × size): drop shadow coverage error 0.0064 → 0.0038, satin 0.014 → 0.002.
fn blur(m: &mut Map, size: f32) {
    if size.is_finite() {
        tent(m, size);
    }
}

/// Grows the shape by `r` pixels (anti-aliased), keeping the original soft edge. Distances use
/// the 5 × 5 chamfer metric, as for strokes: Photoshop's spread of a group drop shadow around an
/// ellipse (ag-psd group-drop-shadows) is round at 0° and 45° and falls short in between, peaking
/// near 15° where the chamfer metric overestimates most (5/255 lighter with an exact Euclidean
/// dilation, within 2/255 with the chamfer).
fn dilate(s: &Map, r: f32) -> Map {
    if r <= 0.0 {
        return s.clone();
    }
    let d = dist_outside_by(s, Metric::Chamfer);
    let mut out = s.clone();
    for (o, d) in out.v.iter_mut().zip(d) {
        // Partly covered pixels (distance sentinel < 0) grow by `r` from their own coverage, so a
        // 2 % spread doesn't make every anti-aliased edge pixel opaque (ag-psd drop shadow).
        *o = if d < 0.0 { (*o + r).min(1.0) } else { o.max((r + 0.5 - d).clamp(0.0, 1.0)) };
    }
    out
}

fn contour_lut(c: &Contour) -> Option<Vec<f32>> {
    match c {
        Contour::Linear => None,
        // A custom curve from (0, 0) straight to (1, 1) is the linear contour: no lookup, so no
        // anti-aliasing shift either (Reference: such a Satin draws exactly as the linear one).
        Contour::Custom { points, .. }
            if points.len() == 2 && points.iter().zip([(0.0, 0.0), (1.0, 1.0)]).all(|(p, (i, o))| (p.input - i).abs() < 1e-4 && (p.output - o).abs() < 1e-4) =>
        {
            None
        }
        Contour::Custom { points, corners, .. } => Some(crate::adjust::contour_curve_lut(points, corners)),
    }
}

/// Satin's coverage transfer: the contour read from its far end, `|c(1 − v) − c(1)|`. Measured in
/// Photoshop 27.11: Cone and Cone − Inverted Satins are identical, and a Gaussian Satin runs under
/// the diagonal (0.36 at 0.5) while the same Gaussian on a glow or gloss runs over it (0.65).
/// `None` = identity.
pub fn satin_lut(c: &Contour) -> Option<Vec<f32>> {
    let lut = contour_lut(c)?;
    let end = lut.last().copied().unwrap_or(1.0);
    Some(lut.iter().rev().map(|v| (v - end).abs()).collect())
}

/// Interpolated lookup in a 0..=1 table (as the GPU's `lut()`).
fn lut_at(l: &[f32], v: f32) -> f32 {
    let x = v.clamp(0.0, 1.0) * (l.len() - 1) as f32;
    let i = x.floor() as usize;
    let j = (i + 1).min(l.len() - 1);
    l[i] + (l[j] - l[i]) * (x - i as f32)
}

/// Whether `e` reads a contour with the Anti-aliased option on, which takes its input a third of a
/// pixel away ([`CONTOUR_AA_SHIFT`]; the GPU leaves such layers to the CPU).
pub fn has_anti_aliased_contour(e: &Effect) -> bool {
    match e {
        Effect::OuterGlow(g) | Effect::InnerGlow(g) => g.anti_alias && glow_lut(g).is_some(),
        Effect::DropShadow(s) | Effect::InnerShadow(s) => s.anti_alias && contour_lut(&s.contour).is_some(),
        Effect::Satin(s) => s.anti_alias && contour_lut(&s.contour).is_some(),
        Effect::BevelEmboss(b) => b.contour.as_ref().is_some_and(|c| c.anti_alias) || (b.gloss_anti_alias && contour_lut(&b.gloss_contour).is_some()),
        _ => false,
    }
}

/// A glow's coverage transfer: its contour applied over the Range (Photoshop maps a Softer
/// glow's coverage `v` through `contour(min(v / range, 1))`, so the default 50 % doubles a
/// blurred edge's 0.5 to full strength). A Precise glow's ramp takes the contour as it is
/// (Photoshop corpus outer-glow-precise-spread40.psd: 94/255 → 20/255 with Range 50 %).
/// `None` = identity.
pub fn glow_lut(g: &Glow) -> Option<Vec<f32>> {
    match g.technique {
        GlowTechnique::Precise => contour_lut(&g.contour),
        GlowTechnique::Softer => ranged_lut(&g.contour, g.range),
    }
}

/// The bevel's Contour element as a height transfer over the Range. The height is first taken
/// as a place `u` on the style's own profile, 0 at its foot and 1 at its top: a Smooth inner bevel
/// runs from ½ at the edge to 1 (`u = 2h − 1`), a Smooth outer one from 0 to ½ at the edge
/// (`u = 2h`), Smooth emboss styles and every Chisel profile from 0 to 1 (`u = h`). The Range then
/// picks the top of it, `contour((u − 1 + 2·range) / (2·range))`, so the default 50 % spans the
/// whole profile. Reference corpus bevel-inner-contour-ramp-peak: lit for 4 px from the edge,
/// shaded to 14 px (the size), as a ramp peak over the inner bevel gives;
/// bevel-pillow-chisel-hard-gloss-and-contour: the contour spans both halves of the pillow, out
/// past the edge (taken over the inner half only, its bands sat 2 px inside).
pub fn bevel_contour_lut(c: &photosplice_doc::effects::BevelContour, b: &Bevel) -> Vec<f32> {
    let base = contour_lut(&c.contour);
    let r = if c.range.is_finite() { c.range.clamp(0.01, 1.0) } else { 1.0 };
    let (scale, offset) = match (b.technique == BevelTechnique::Smooth, bevel_geom(b).paint) {
        (true, BevelPaint::Inner | BevelPaint::Stroke) => (2.0, -1.0),
        (true, BevelPaint::Outer) => (2.0, 0.0),
        _ => (1.0, 0.0),
    };
    let at = |v: f32| base.as_ref().map_or(v, |l| lut_at(l, v));
    // Below the range's foot (a Smooth bevel's corners dip under the edge's ½) the contour goes
    // on at its starting slope rather than flattening out.
    let slope = (at(1.0 / 255.0) - at(0.0)) * 255.0;
    let n = 4096;
    (0..n)
        .map(|k| {
            let u = scale * (k as f32 / (n - 1) as f32) + offset;
            let v = ((u - 1.0 + 2.0 * r) / (2.0 * r)).min(1.0);
            if v < 0.0 { at(0.0) + v * slope } else { at(v) }
        })
        .collect()
}

/// A bevel's lighting through its Gloss Contour. The shading runs 0 (full shadow) .. ½ (facing
/// the light as a flat surface does) .. 1 (full highlight); the contour maps it, and what lands
/// above ½ is highlight, below shadow. A flat area under a peaked contour turns highlight
/// (Photoshop corpus bevel-inner-gloss-ramp-peak: the interior 0.35 → 0.84, Screen white at
/// 75 %); a linear contour leaves the shading as it is. Returns `(highlight, shadow)`.
pub fn gloss(k: f32, se: f32, lut: Option<&[f32]>) -> (f32, f32) {
    let s = if k > 0.0 { 0.5 + 0.5 * (k / (1.0 - se).max(1e-3)).clamp(0.0, 1.0) } else { 0.5 - 0.5 * (-k / se.max(1e-3)).clamp(0.0, 1.0) };
    let c = lut.map_or(s, |l| lut_at(l, s));
    ((2.0 * c - 1.0).max(0.0), (1.0 - 2.0 * c).max(0.0))
}

/// A contour applied over a Range (`contour(min(v / range, 1))`); `None` = identity.
pub fn ranged_lut(contour: &Contour, range: f32) -> Option<Vec<f32>> {
    let base = contour_lut(contour);
    let range = if range.is_finite() { range.clamp(0.01, 1.0) } else { 1.0 };
    if range >= 0.999 {
        return base;
    }
    let n = 4096;
    Some(
        (0..n)
            .map(|k| {
                let v = (k as f32 / (n - 1) as f32 / range).min(1.0);
                base.as_ref().map_or(v, |l| lut_at(l, v))
            })
            .collect(),
    )
}

/// Where an anti-aliased contour reads its input: Reference samples the coverage a third of a
/// pixel down and right of each pixel, so the shaped effect sits that far up and left (Reference
/// 16-bit glows and satins, linear, ramp, double-ring and Gaussian contours: a linear anti-aliased
/// glow 12.8 % of pixels off → 0, a Gaussian satin 5 % → 0.02 %). Without a contour lookup there is
/// no shift (a linear satin matches unshifted).
const CONTOUR_AA_SHIFT: f32 = 1.0 / 3.0;

/// [`apply_lut`] with the contour's Anti-aliased option ([`CONTOUR_AA_SHIFT`]).
fn apply_lut_aa(m: Map, l: Option<Vec<f32>>, anti_alias: bool) -> Map {
    match l {
        Some(l) if anti_alias => m.shifted(-CONTOUR_AA_SHIFT, -CONTOUR_AA_SHIFT, 0.0).map(|v| lut_at(&l, v)),
        l => apply_lut(m, l),
    }
}

fn apply_lut(m: Map, l: Option<Vec<f32>>) -> Map {
    match l {
        None => m,
        Some(l) => m.map(|v| lut_at(&l, v)),
    }
}

/// Deterministic per-pixel value in 0..=1, hashed from document coordinates
/// (the same hash as the GPU's `dissolve_noise`), so the speckle is stable
/// across renders, frames and tiles.
pub fn hash_noise(x: i64, y: i64) -> f32 {
    let mut h = (x as i32 as u32).wrapping_mul(0x8da6_b343) ^ (y as i32 as u32).wrapping_mul(0xd816_3841) ^ 0x9e37_79b9;
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    (h & 0xffff) as f32 / 65536.0
}

/// Effect noise (Photoshop's Noise slider): monochrome speckle in the coverage,
/// applied after the contour. `amount` is 0..=1; 0 leaves the map untouched.
fn noise(m: &mut Map, amount: f32, x0: i32, y0: i32) {
    if !amount.is_finite() || amount <= 0.0 {
        return;
    }
    for y in 0..m.h {
        for x in 0..m.w {
            let v = &mut m.v[y * m.w + x];
            if *v <= 0.0 {
                continue;
            }
            let n = hash_noise(i64::from(x0 + x as i32), i64::from(y0 + y as i32));
            *v = (*v + (n - 0.5) * 2.0 * amount).clamp(0.0, 1.0);
        }
    }
}

fn apply_contour(m: Map, c: &Contour, anti_alias: bool) -> Map {
    match contour_lut(c) {
        None => m,
        Some(l) => (if anti_alias { m.shifted(-CONTOUR_AA_SHIFT, -CONTOUR_AA_SHIFT, 0.0) } else { m }).map(|v| {
            let x = v.clamp(0.0, 1.0) * (l.len() - 1) as f32;
            let i = x.floor() as usize;
            let j = (i + 1).min(l.len() - 1);
            l[i] + (l[j] - l[i]) * (x - i as f32)
        }),
    }
}

fn rgb(c: &photosplice_color::Color) -> [f32; 3] {
    // In a CMYK document's projected passes, the colour's inks stand in (see `cmyk`).
    crate::cmyk::project_color(c).unwrap_or_else(|| c.to_rgb_unclamped())
}

/// Offset of an effect lit from `angle` at `distance` (shadow falls away from
/// the light), rounded to whole pixels as Photoshop does.
fn offset(angle: f32, distance: f32) -> (f32, f32) {
    let a = angle.to_radians();
    ((-a.cos() * distance).round(), (a.sin() * distance).round())
}

// The gradient geometry is shared with the vector renderer (shape layers).
pub use photosplice_vector::gradient::{gradient_t, gradient_units};

/// Samples colour and opacity stops at `t`.
pub fn sample_gradient(g: &Gradient, t: f32) -> [f32; 4] {
    PreparedGradient::new(g).sample(t)
}

// Prepared only for one paint call: CMYK conversions must use that call's active profile.
struct PreparedGradient {
    color: Vec<(f32, [f32; 3])>,
    opacity: Vec<(f32, [f32; 3])>,
}

impl PreparedGradient {
    fn new(g: &Gradient) -> Self {
        let mut color: Vec<(f32, [f32; 3])> = g.stops.iter().map(|(p, c)| (*p, rgb(c))).collect();
        let mut opacity: Vec<(f32, [f32; 3])> = g.opacity_stops.iter().map(|(p, a)| (*p, [*a; 3])).collect();
        color.sort_by(|a, b| a.0.total_cmp(&b.0));
        opacity.sort_by(|a, b| a.0.total_cmp(&b.0));
        Self { color, opacity }
    }

    fn sample(&self, t: f32) -> [f32; 4] {
        let color = sample_stops(&self.color, t);
        let alpha = if self.opacity.is_empty() { 1.0 } else { sample_stops(&self.opacity, t)[0] };
        [color[0], color[1], color[2], alpha]
    }
}

fn sample_stops(stops: &[(f32, [f32; 3])], t: f32) -> [f32; 3] {
    crate::adjust::gradient(stops, t)
}

/// Composite `color × coverage` into `dst` (both over the same rectangle). The special eight
/// modes (`psblend::fill_is_special`) take the strength as Reference takes a layer's Fill: the
/// colour pulled toward the mode's neutral, blended at full coverage (a 15 % Linear Dodge
/// highlight adds 15 % of its colour and clips, rather than mixing 15 % of the clipped sum).
fn paint(dst: &mut Buffer, m: &Map, color: impl Fn(usize) -> [f32; 4], blend: BlendMode, opacity: f32) {
    for (i, p) in dst.px.iter_mut().enumerate() {
        let a = m.v[i] * opacity;
        if a <= 0.0 {
            continue;
        }
        *p = psblend::composite_fill(blend, *p, color(i), 1.0, a, 1.0);
    }
}

fn paint_color(dst: &mut Buffer, m: &Map, c: [f32; 3], blend: BlendMode, opacity: f32) {
    paint(dst, m, |_| [c[0], c[1], c[2], 1.0], blend, opacity);
}

/// Spread / choke of a soft effect: (dilation radius, blur width). The dilation is whole pixels
/// (`size × spread` rounded: a 2 % spread of a 5 px shadow doesn't grow it at all), the blur
/// keeps `size × (1 − spread)` (ag-psd effects 10.6 → 3.8 % of pixels off).
pub fn spread_split(size: f32, spread: f32) -> (f32, f32) {
    ((size * spread).round(), size * (1.0 - spread))
}

fn shadow_map(shape: &Map, s: &Shadow, light: &GlobalLight, inner: bool, origin: (i32, i32)) -> Map {
    let angle = if s.use_global_light { light.angle } else { s.angle };
    let (dx, dy) = offset(angle, s.distance);
    let src = if inner { shape.clone().map(|a| 1.0 - a) } else { shape.clone() };
    let mut m = src.shifted(dx, dy, if inner { 1.0 } else { 0.0 });
    let (r, bw) = spread_split(s.size, s.spread);
    m = dilate(&m, r);
    blur(&mut m, bw);
    let mut m = apply_contour(m, &s.contour, s.anti_alias);
    noise(&mut m, s.noise, origin.0, origin.1);
    if inner {
        for (v, a) in m.v.iter_mut().zip(&shape.v) {
            *v *= a;
        }
    }
    m
}

fn glow_map(shape: &Map, g: &Glow, inner: bool, origin: (i32, i32)) -> Map {
    // An inner glow is built from the edge inwards; a centre-source glow is its inverse, taken
    // after the contour and Range (Photoshop 27.11, measured: the centre glow is exactly
    // 1 − the edge glow, for both techniques; brightest deep inside: Photoshop corpus
    // inner-glow-center-precise-choke30.psd).
    let src = if inner { shape.clone().map(|a| 1.0 - a) } else { shape.clone() };
    let mut m = match g.technique {
        GlowTechnique::Precise => {
            // Inner glows measure from the edge inwards, for either source.
            // Reference measures from pixel centre to pixel centre (half a pixel past the edge
            // distance), holds full strength over the whole pixels of the Spread and fades to
            // nothing one pixel past the Size (corpus outer-glow-precise-spread40.psd, size 16,
            // spread 40 %: full to 6, (17 − d) / 11 beyond).
            let d: Vec<f32> = if inner { dist_inside_by(shape, Metric::Chamfer) } else { dist_outside_by(&src, Metric::Chamfer) }.into_iter().map(|d| d + 0.5).collect();
            let solid = (g.size * g.spread).floor();
            let soft = (g.size + 1.0 - solid).max(1e-3);
            let mut m = Map::new(shape.w, shape.h, 0.0);
            for (o, d) in m.v.iter_mut().zip(d) {
                *o = if d <= solid { 1.0 } else { (1.0 - (d - solid) / soft).clamp(0.0, 1.0) };
            }
            m
        }
        GlowTechnique::Softer => {
            let (r, bw) = spread_split(g.size, g.spread);
            let mut m = dilate(&src, r);
            blur(&mut m, bw);
            m
        }
    };
    m = apply_lut_aa(m, glow_lut(g), g.anti_alias);
    if inner && g.source == GlowSource::Center {
        m = m.map(|v| 1.0 - v);
    }
    noise(&mut m, g.noise, origin.0, origin.1);
    if inner {
        for (v, a) in m.v.iter_mut().zip(&shape.v) {
            *v *= a;
        }
    }
    m
}

/// Paints a pattern (looked up in `patterns`) through coverage `m`; missing patterns paint nothing.
#[allow(clippy::too_many_arguments)]
fn paint_pattern(
    dst: &mut Buffer,
    m: &Map,
    patterns: &PreparedPatterns<'_>,
    name: &str,
    id: &str,
    place: Placement,
    big: Rect,
    blend: BlendMode,
    opacity: f32,
) {
    let Some(tile) = patterns.get(id, name) else { return };
    let w = big.width() as usize;
    paint(
        dst,
        m,
        |i| {
            // The pixel's corner, as `pattern::render`.
            let (x, y) = (f64::from(big.x0 + (i % w) as i32), f64::from(big.y0 + (i / w) as i32));
            let (u, v) = place.map(x, y);
            tile.sample(u, v)
        },
        blend,
        opacity,
    );
}

#[allow(clippy::too_many_arguments)]
fn paint_fx(
    dst: &mut Buffer,
    m: &Map,
    p: &FxPaint,
    shape_bounds: Rect,
    canvas: Rect,
    anchor: (f64, f64),
    big: Rect,
    blend: BlendMode,
    opacity: f32,
    patterns: &PreparedPatterns<'_>,
) {
    match p {
        FxPaint::Color(c) => paint_color(dst, m, rgb(c), blend, opacity),
        FxPaint::Gradient(g) => {
            let prepared = PreparedGradient::new(g);
            // "Align with Layer" off lays the gradient out over the canvas instead of the layer
            // (psd-tools layer_params: a radial stroke gradient centred on the document).
            let shape_bounds = if g.align { shape_bounds } else { canvas };
            let (angle, scale, offset) = crate::fill_layout::gradient_layout(g.style, g.angle, g.scale, g.offset, shape_bounds);
            let w = big.width() as usize;
            paint(
                dst,
                m,
                |i| {
                    let (x, y) = ((big.x0 + (i % w) as i32) as f32 + 0.5, (big.y0 + (i / w) as i32) as f32 + 0.5);
                    prepared.sample(gradient_t(g.style, angle, scale, g.reverse, offset, shape_bounds, x, y))
                },
                blend,
                opacity,
            )
        }
        FxPaint::Pattern { name, id, scale } => {
            paint_pattern(dst, m, patterns, name, id, Placement::anchored(anchor, true, (0.0, 0.0), *scale, 0.0), big, blend, opacity)
        }
    }
}

/// The opacity gain of a gradient glow: it is opaque from strength `range²` on (see [`paint_glow`]).
pub fn glow_gradient_gain(range: f32) -> f32 {
    let r = if range.is_finite() { range.clamp(0.01, 1.0) } else { 1.0 };
    1.0 / (r * r)
}

/// A glow's paint over its map `m`. A gradient runs along the glow rather than across the
/// canvas: Photoshop colours the glow where its (ranged, contoured) strength is `v` with the
/// gradient at `1 - v`, so the first stop hugs the edge, and the glow is opaque from `v = range²`
/// on (photoshop corpus outer-glow-gradient.psd: 1.4/255 mean, against bands across the canvas;
/// psd-tools layer_params.psd). Glows have no gradient angle, style or Reverse.
#[allow(clippy::too_many_arguments)]
fn paint_glow(dst: &mut Buffer, m: &Map, g: &Glow, shape_bounds: Rect, anchor: (f64, f64), big: Rect, patterns: &PreparedPatterns<'_>) {
    let FxPaint::Gradient(gradient) = &g.paint else {
        return paint_fx(dst, m, &g.paint, shape_bounds, shape_bounds, anchor, big, g.common.blend, g.common.opacity, patterns);
    };
    let prepared = PreparedGradient::new(gradient);
    let gain = glow_gradient_gain(g.range);
    let strength = Map { w: m.w, h: m.h, v: m.v.iter().map(|v| (v * gain).min(1.0)).collect() };
    paint(dst, &strength, |i| prepared.sample(1.0 - m.v.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0)), g.common.blend, g.common.opacity);
}

/// A box width as the blurs use it: at least 1, at most [`MAX_REACH`] (an effect never reaches
/// further, see [`margin`]), and 1 for NaN. A size or softness from a command or a file can be
/// anything, and an unbounded width sizes the kernel and its buffers (#1543).
fn box_width(w: f32) -> f32 {
    if w.is_nan() { 1.0 } else { w.clamp(1.0, MAX_REACH) }
}

/// Normalised weights of a centred box of (fractional) width `w`: tap `i` gets the overlap of
/// `[i - 0.5, i + 0.5]` with `[-w/2, w/2]`.
fn box_weights(w: f32) -> Vec<f32> {
    let w = box_width(w);
    let half = w / 2.0;
    let r = (half - 0.5).ceil().max(0.0) as i64;
    let v: Vec<f32> = (-r..=r).map(|i| ((i as f32 + 0.5).min(half) - (i as f32 - 0.5).max(-half)).max(0.0)).collect();
    let sum: f32 = v.iter().sum();
    v.iter().map(|x| x / sum).collect()
}

/// The 1D kernel of two box passes of width `w` (a tent of half-width ≈ `w`): `(radius, weights)`.
/// Photoshop's bevel height maps are this blur of the shape (fitted on the corpus: a smooth
/// inner bevel of size 5 matches within 0.7 % mean, size 41 within 2.6 %).
pub fn tent_kernel(w: f32) -> (i32, Vec<f32>) {
    let b = box_weights(w);
    let n = b.len() * 2 - 1;
    let mut t = vec![0.0f32; n];
    for (i, x) in b.iter().enumerate() {
        for (j, y) in b.iter().enumerate() {
            t[i + j] += x * y;
        }
    }
    ((n as i32 - 1) / 2, t)
}

/// Box geometry for a (fractional) width: (`r`, end-tap weight `f`, 1 / width) — the
/// [`box_weights`] taps are `r - 1` full ones each side of the centre plus the two end taps at `f`.
fn box_geom(bw: f32) -> (i64, f64, f64) {
    let bw = box_width(bw);
    let half = bw / 2.0;
    let r = (half - 0.5).ceil().max(0.0) as i64;
    let f = f64::from((half - (r as f32 - 0.5)).clamp(0.0, 1.0));
    (r, f, 1.0 / f64::from(bw))
}

/// One box pass over `src` (zero outside it) evaluated at `x0 .. x0 + dst.len()`, as a running
/// sum (O(1) per sample whatever the width).
fn box_line(src: &[f64], x0: i64, dst: &mut [f64], bw: f32) {
    let n = src.len() as i64;
    let (r, f, norm) = box_geom(bw);
    let at = |i: i64| if i >= 0 && i < n { src[i as usize] } else { 0.0 };
    if r == 0 {
        for (k, d) in dst.iter_mut().enumerate() {
            *d = at(x0 + k as i64);
        }
        return;
    }
    let mut inner: f64 = (x0 - (r - 1)..=x0 + (r - 1)).map(at).sum();
    for (k, d) in dst.iter_mut().enumerate() {
        let x = x0 + k as i64;
        *d = (inner + f * (at(x - r) + at(x + r))) * norm;
        inner += at(x + r) - at(x - r + 1);
    }
}

/// Two box passes along a line, exactly the convolution with [`tent_kernel`] (zero outside): the
/// first pass is evaluated `r` samples beyond each end so the second sees what lies there.
fn tent_line(src: &[f32], dst: &mut [f32], bw: f32) {
    let (r, _, _) = box_geom(bw);
    let s: Vec<f64> = src.iter().map(|v| f64::from(*v)).collect();
    let mut mid = vec![0.0f64; src.len() + 2 * r as usize];
    box_line(&s, -r, &mut mid, bw);
    let mut out = vec![0.0f64; src.len()];
    // `mid[k]` holds position k - r: output position p reads index p + r.
    box_line(&mid, r, &mut out, bw);
    for (d, v) in dst.iter_mut().zip(out) {
        *d = v as f32;
    }
}

/// Rows of a `w`-wide row-major buffer, each through `f(src_row, dst_row)`, in parallel on
/// scoped OS threads. Not rayon: maps can be built from inside a rayon tile, where a waiting
/// worker steals other tiles. Map builds no longer block on each other (#276), but the blur
/// keeps out of the tile pool.
fn rows_par(src: &[f32], dst: &mut [f32], w: usize, f: impl Fn(&[f32], &mut [f32]) + Sync) {
    if w == 0 {
        return;
    }
    let threads =
        if cfg!(target_arch = "wasm32") || src.len() < 1 << 16 { 1 } else { std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 16) };
    if threads == 1 {
        dst.chunks_mut(w).zip(src.chunks(w)).for_each(|(d, s)| f(s, d));
        return;
    }
    let rows = (src.len() / w).div_ceil(threads);
    std::thread::scope(|sc| {
        for (d, s) in dst.chunks_mut(rows * w).zip(src.chunks(rows * w)) {
            let f = &f;
            sc.spawn(move || d.chunks_mut(w).zip(s.chunks(w)).for_each(|(d, s)| f(s, d)));
        }
    });
}

fn transpose_map(v: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; v.len()];
    const B: usize = 32;
    for by in (0..h).step_by(B) {
        for bx in (0..w).step_by(B) {
            for y in by..(by + B).min(h) {
                for x in bx..(bx + B).min(w) {
                    out[x * h + y] = v[y * w + x];
                }
            }
        }
    }
    out
}

/// Two box passes of width `w` along each axis (the tent [`tent_kernel`] describes, which the GPU
/// convolves directly).
fn tent(m: &mut Map, w: f32) {
    if tent_kernel(w).0 == 0 || m.v.is_empty() {
        return;
    }
    tent_fast(m, w);
}

/// [`tent`] as a direct convolution with [`tent_kernel`] (the GPU's way; tests compare).
#[cfg(test)]
fn tent_direct(m: &mut Map, w: f32) {
    {
        let (r, k) = tent_kernel(w);
        let (mw, mh) = (m.w as i64, m.h as i64);
        let mut tmp = vec![0.0f32; m.v.len()];
        for y in 0..mh {
            for x in 0..mw {
                let mut acc = 0.0;
                for (i, kv) in k.iter().enumerate() {
                    let xx = x + i as i64 - r as i64;
                    if xx >= 0 && xx < mw {
                        acc += m.v[(y * mw + xx) as usize] * kv;
                    }
                }
                tmp[(y * mw + x) as usize] = acc;
            }
        }
        for y in 0..mh {
            for x in 0..mw {
                let mut acc = 0.0;
                for (i, kv) in k.iter().enumerate() {
                    let yy = y + i as i64 - r as i64;
                    if yy >= 0 && yy < mh {
                        acc += tmp[(yy * mw + x) as usize] * kv;
                    }
                }
                m.v[(y * mw + x) as usize] = acc;
            }
        }
    }
}

fn tent_fast(m: &mut Map, w: f32) {
    let (mw, mh) = (m.w, m.h);
    let twice = |s: &[f32], d: &mut [f32]| tent_line(s, d, w);
    let mut a = vec![0.0f32; m.v.len()];
    rows_par(&m.v, &mut a, mw, twice);
    let t = transpose_map(&a, mw, mh);
    let mut b = vec![0.0f32; t.len()];
    rows_par(&t, &mut b, mh, twice);
    m.v = transpose_map(&b, mh, mw);
}

/// Where a bevel's maps paint: inside the shape (with the layer), outside it (onto the
/// backdrop), or both (emboss styles: maps 0–1 inside, 2–3 outside).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BevelPaint {
    Inner,
    Outer,
    Both,
    /// Stroke Emboss: shades the stroke effect's area, after the layer and its strokes.
    Stroke,
}

/// Height below which an outer bevel pixel counts as off the bevel (beyond the blur's reach):
/// above float noise of the running-sum blur (~3e-7), far below anything that shades.
pub const BEVEL_H_EPS: f32 = 1e-5;

/// Bevel geometry shared by the CPU maps and the GPU programs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BevelGeom {
    pub paint: BevelPaint,
    /// Box width of the smooth height blur (size; half the size, rounded up, for emboss styles).
    pub width: f32,
    /// Height-map gradient scale: depth × width (smooth) or depth × ramp length ×
    /// [`chisel_slope`] (chisel), signed by direction.
    pub depth: f32,
    /// Pillow emboss: the outside slopes face the other way.
    pub pillow: bool,
    /// Chisel: the inside and outside ramp lengths of the height map (see [`bevel_chisel_h`]).
    pub chisel: [f32; 2],
}

/// Steepness of a chisel bevel's facets at 100 % depth for facet width `w` (the size; half of it,
/// rounded up, for emboss styles): `w / (2 (w + 1))`. Fitted on Photoshop's own renderings through
/// the same lighting model: inner size 10 (w 10) 0.451-0.457 → 0.4545, emboss size 10 (w 5)
/// 0.415-0.422 → 0.4167, emboss size 13 (w 7) 0.435-0.437 → 0.4375, on both halves of an emboss.
/// (A 45° facet, slope 1, made the shadows near black.)
fn chisel_slope(w: f32) -> f32 {
    w / (2.0 * (w + 1.0))
}

pub fn bevel_geom(b: &Bevel) -> BevelGeom {
    let size = b.size.max(1.0);
    let emboss = matches!(b.style, BevelStyle::Emboss | BevelStyle::PillowEmboss);
    let paint = match b.style {
        BevelStyle::OuterBevel => BevelPaint::Outer,
        BevelStyle::Emboss | BevelStyle::PillowEmboss => BevelPaint::Both,
        BevelStyle::InnerBevel | BevelStyle::StrokeEmboss => BevelPaint::Inner,
    };
    // Emboss styles straddle the edge with half the size, rounded up to whole pixels: a 41 px
    // emboss blurs over 21 (psd-tools layer_effects: 20.5 left narrow letter parts up to
    // 5.7/255 dark, 21 is within 2.2).
    let width = if emboss { (size / 2.0).ceil() } else { size };
    let smooth = b.technique == BevelTechnique::Smooth;
    let sign = if b.up { 1.0 } else { -1.0 };
    // Chisel facets as Photoshop draws them (oracles at sizes 10 and 13, inner, outer, emboss):
    // full shading for pixel centres up to W − ½ inside and W + ½ outside the edge (W = the size,
    // half of it rounded up for emboss styles), then one partly shaded pixel. A height ramp that
    // ends half a pixel past the last full centre gives exactly that through the shading's central
    // differences. (A hair past it outside, so that pixel's height stays above `BEVEL_H_EPS`.)
    let chisel = [width + 0.5, width + 1.5 + 1e-3];
    // The height gradient scale that makes the facets [`chisel_slope`] steep at 100 % depth.
    let ramp = match paint {
        BevelPaint::Inner | BevelPaint::Stroke => chisel[0],
        BevelPaint::Outer => chisel[1],
        // Emboss halves share one slope (see `bevel_chisel_h`): the inside half rises
        // 1 / (chisel[0] + chisel[1]) per pixel.
        BevelPaint::Both => chisel[0] + chisel[1],
    };
    BevelGeom {
        paint,
        width,
        depth: b.depth * if smooth { width } else { ramp * chisel_slope(width) } * sign,
        pillow: b.style == BevelStyle::PillowEmboss,
        chisel,
    }
}

/// Bevel height map in `0..=1`. Smooth: the shape blurred by two box passes of the bevel
/// width. Chisel: linear ramps of the exact distance to the edge ([`BevelGeom::chisel`] long;
/// emboss styles straddle the edge). Chisel Soft uses the same profile: on straight edges
/// Reference's Chisel Soft and Hard facets are identical (Reference corpus bevel-inner-chisel-*:
/// the same values pixel for pixel across an edge); a blur over it softened them (pixels off
/// 10-18 % → 8-12 %; what remains are the radial streaks Reference's Soft draws on curves),
/// except that an inner Soft bevel drops a step further at the edge ([`chisel_soft_edge`]).
/// Soften blurs the result.
fn bevel_height(shape: &Map, b: &Bevel, g: &BevelGeom, tex: &TextureCtx, patterns: &PreparedPatterns<'_>) -> Map {
    let mut h = if b.technique == BevelTechnique::Smooth {
        let mut h = shape.clone();
        tent(&mut h, g.width);
        h
    } else {
        let (din, dout) = (dist_inside(shape), dist_outside_smooth(shape));
        let soft_edge = chisel_soft_edge(b, g);
        let mut h = Map::new(shape.w, shape.h, 0.0);
        for i in 0..h.v.len() {
            // A partly covered edge pixel lies `coverage − ½` inside the edge (outside below ½),
            // as the distances around it assume; taken as fully inside, its height broke the
            // slope across the edge and the shading's differences put a dark or light dotted line
            // along it (keep in sync with `fs_mbevelh`).
            let a = shape.v[i];
            let (mut di, dou) = if a > INSIDE_EPS && a < 1.0 - INSIDE_EPS { (a - 0.5, 0.5 - a) } else { (din[i], dout[i]) };
            if soft_edge {
                di = chisel_soft_inside(a, di);
            }
            h.v[i] = bevel_chisel_h(g.paint, g.chisel, di, dou);
        }
        h
    };
    // Contour element: the height profile through the contour over its range.
    if let Some(c) = &b.contour {
        h = apply_lut_aa(h, Some(bevel_contour_lut(c, b)), c.anti_alias);
    }
    // Texture element: the pattern's luminance as extra height, scaled so a full-contrast step
    // at 100 % depth slopes like the bevel at its depth.
    if let Some(t) = &b.texture
        && let Some(tile) = patterns.get(&t.id, &t.name)
    {
        let unit = if b.depth.abs() > 1e-6 { (g.depth / b.depth).abs().max(1e-3) } else { g.width.max(1.0) };
        let place = Placement::anchored(tex.anchor, t.link, t.phase, t.scale, 0.0);
        let k = t.depth / unit;
        let (x0, y0) = (tex.rect.x0, tex.rect.y0);
        for y in 0..h.h {
            for x in 0..h.w {
                let (u, v) = place.map(f64::from(x0 + x as i32), f64::from(y0 + y as i32));
                let p = tile.sample(u, v);
                let l = (0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2]) * p[3];
                h.v[y * h.w + x] += k * if t.invert { 1.0 - l } else { l };
            }
        }
    }
    if b.soften >= 1.0 {
        tent(&mut h, b.soften);
    }
    h
}

/// Where a bevel texture tiles: the maps' rectangle, the document's patterns and the linked
/// origin (the effects reference point, else the layer's top-left).
pub struct TextureCtx<'a> {
    pub rect: Rect,
    pub patterns: &'a [Pattern],
    pub anchor: (f64, f64),
}

/// Whether a chiselled bevel's ramp drops a step further at the shape's edge: Chisel Soft on
/// an inner or stroke bevel (Reference kit soft-inner-s10 / -s20, light overhead: the outermost
/// row slopes 1.5 × the facet, Chisel Hard's not at all; the other styles keep the plain ramp).
pub fn chisel_soft_edge(b: &Bevel, g: &BevelGeom) -> bool {
    b.technique == BevelTechnique::ChiselSoft && matches!(g.paint, BevelPaint::Inner | BevelPaint::Stroke)
}

/// The inside distance [`bevel_chisel_h`] takes under [`chisel_soft_edge`]: pixels with no
/// coverage a whole pixel further out, partly covered ones blended towards that (keep in sync
/// with `fs_mbevelh`).
pub fn chisel_soft_inside(a: f32, di: f32) -> f32 {
    if a <= INSIDE_EPS {
        di - 1.0
    } else if a < 1.0 - INSIDE_EPS {
        2.0 * a - 1.5
    } else {
        di
    }
}

/// Chiselled height from the inside / outside distances and the ramp lengths `[inside,
/// outside]` ([`BevelGeom::chisel`]; keep in sync with `fs_mbevelh`). Clamped only at the far
/// end of each ramp: across the shape's edge the ramp runs on, so the edge pixels shade at the full
/// facet slope as Photoshop's do.
pub fn bevel_chisel_h(paint: BevelPaint, ramp: [f32; 2], din: f32, dout: f32) -> f32 {
    let [rin, rout] = ramp.map(|r| r.max(1e-3));
    match paint {
        BevelPaint::Outer => (1.0 - dout / rout).max(0.0),
        // Emboss: one slope across both halves (Photoshop's outside facet is as steep as the
        // inside one, and longer), from 0 at the end of the outside ramp to 1 at the end of the
        // inside one.
        BevelPaint::Both => {
            if din > 0.0 {
                (rout + din.min(rin)) / (rin + rout)
            } else {
                (rout - dout.min(rout)) / (rin + rout)
            }
        }
        BevelPaint::Inner | BevelPaint::Stroke => (din / rin).min(1.0),
    }
}

/// Bevel highlight and shadow maps: `[hi, sh]` (inner or outer) or `[hi, sh, hi_out, sh_out]`
/// (emboss styles).
fn bevel_maps(shape: &Map, b: &Bevel, light: &GlobalLight, tex: &TextureCtx, patterns: &PreparedPatterns<'_>) -> (Vec<Map>, BevelPaint) {
    let g = bevel_geom(b);
    let hmap = bevel_height(shape, b, &g, tex, patterns);
    let (angle, altitude) = if b.use_global_light { (light.angle, light.altitude) } else { (b.angle, b.altitude) };
    let (sa, ca) = angle.to_radians().sin_cos();
    let (se, ce) = altitude.to_radians().sin_cos();
    let light_v = [ca * ce, -sa * ce, se];
    // Region: 0 = inside (× the shape's alpha), 1 = under the layer's edge and outside.
    // A pixel is on the bevel where its height is raised. Emboss styles also shade the last pixel
    // past the blur's reach (a raised 4-neighbour: it still slopes; psd-tools layer_effects'
    // emboss shadow runs one row past it), outer bevels don't (Photoshop oracle
    // bevel-outer-smooth).
    let reach_past = g.paint == BevelPaint::Both;
    let on_bevel = |x: i64, y: i64| {
        hmap.get(x, y) > BEVEL_H_EPS || (reach_past && [(1, 0), (-1, 0), (0, 1), (0, -1)].iter().any(|(dx, dy)| hmap.get(x + dx, y + dy) > BEVEL_H_EPS))
    };
    let gloss_lut = contour_lut(&b.gloss_contour);
    // Heights past the maps' edge continue the edge's (a contour can raise the whole outside: a
    // zero read there would be a slope along the region's border).
    let (mw, mh) = (hmap.w as i64 - 1, hmap.h as i64 - 1);
    let hc = |x: i64, y: i64| hmap.get(x.clamp(0, mw.max(0)), y.clamp(0, mh.max(0)));
    let shade_into = |depth: f32, region_kind: u8| -> (Map, Map) {
        let (mut hi, mut sh) = (Map::new(shape.w, shape.h, 0.0), Map::new(shape.w, shape.h, 0.0));
        // The shading and the region first, then the gloss over them.
        let (mut sm, mut rg) = (Map::new(shape.w, shape.h, 0.0), Map::new(shape.w, shape.h, 0.0));
        for y in 0..shape.h as i64 {
            for x in 0..shape.w as i64 {
                let i = y as usize * shape.w + x as usize;
                // The height's slope over the 3 × 3 neighbourhood, all six neighbours weighted
                // alike (Prewitt; Reference corpus bevel-emboss-smooth 1.7 % of pixels off → 0, a
                // pillow chisel 0.23 → 0.07 max; a centre difference along each axis left
                // diagonal edges and corners off).
                let gx = (hc(x + 1, y - 1) + hc(x + 1, y) + hc(x + 1, y + 1) - hc(x - 1, y - 1) - hc(x - 1, y) - hc(x - 1, y + 1)) / 6.0 * depth;
                let gy = (hc(x - 1, y + 1) + hc(x, y + 1) + hc(x + 1, y + 1) - hc(x - 1, y - 1) - hc(x, y - 1) - hc(x + 1, y - 1)) / 6.0 * depth;
                let n = [-gx, -gy, 1.0];
                let len = (n[0] * n[0] + n[1] * n[1] + 1.0).sqrt();
                let shade = (n[0] * light_v[0] + n[1] * light_v[1] + n[2] * light_v[2]) / len;
                let region = if region_kind == 0 { shape.v[i] } else { f32::from(shape.v[i] < 1.0 - INSIDE_EPS && on_bevel(x, y)) };
                sm.v[i] = shade;
                rg.v[i] = region;
            }
        }
        // Gloss Contour › Anti-aliased reads the shading a third of a pixel away, as the other
        // anti-aliased contours read their input; a linear contour has nothing to smooth (Reference corpus bevel-inner-gloss-double-ring-aa:
        // within 2/255 on 31 616 → 35 557 pixels, worst 55 → 25/255).
        let sm = if b.gloss_anti_alias && gloss_lut.is_some() { sm.shifted(-CONTOUR_AA_SHIFT, -CONTOUR_AA_SHIFT, 0.0) } else { sm };
        for i in 0..sm.v.len() {
            let (h, s) = gloss(sm.v[i] - se, se, gloss_lut.as_deref());
            hi.v[i] = h * rg.v[i];
            sh.v[i] = s * rg.v[i];
        }
        (hi, sh)
    };
    let maps = match g.paint {
        BevelPaint::Inner | BevelPaint::Outer | BevelPaint::Stroke => {
            let (hi, sh) = shade_into(g.depth, u8::from(g.paint == BevelPaint::Outer));
            vec![hi, sh]
        }
        BevelPaint::Both => {
            // One map per colour over the whole bevel: the inside half where the shape is (× its
            // alpha), the outside half (pillow: facing the other way) for the rest of each pixel.
            // A 1 % edge pixel of a pillow emboss takes the outside shading (Photoshop oracle
            // bevel-pillow-smooth), a 75 % one of an emboss mostly the inside (layer_effects).
            let (hi, sh) = shade_into(g.depth, 0);
            let (ho, so) = shade_into(if g.pillow { -g.depth } else { g.depth }, 1);
            let mix = |a: Map, b: Map| -> Map {
                let v = a.v.iter().zip(&b.v).zip(&shape.v).map(|((x, y), s)| if *s > INSIDE_EPS { *x } else { 0.0 } + y * (1.0 - s.min(1.0))).collect();
                Map { w: a.w, h: a.h, v }
            };
            vec![mix(hi, ho), mix(sh, so)]
        }
    };
    (maps, g.paint)
}

/// Effect maps derived from a layer's shape (its alpha), computed once over the layer's whole
/// region and cropped per render tile. They depend only on the shape, the effect settings and the
/// global light, never on the backdrop, so they can be cached across tiles and edits.
pub struct FxMaps {
    rect: Rect,
    shape: Map,
    /// Per enabled effect (in `items` order): the maps it paints through.
    per: Vec<Vec<Map>>,
    bevel_paint: Vec<BevelPaint>,
    din: Option<Vec<f32>>,
    /// Stroke outside distance below [`STROKE_CHAMFER_MAX`]: the fitted 5 × 5 chamfer.
    dout: Option<Vec<f32>>,
    /// Stroke outside distance at or above [`STROKE_CHAMFER_MAX`]: the exact Euclidean transform.
    dout_exact: Option<Vec<f32>>,
    /// Shape layers: chamfer distance outside the vector outline (from local coverage).
    vdout: Option<Vec<f32>>,
    /// Shape layers: exact Euclidean distance outside the vector outline (from local coverage).
    vdout_exact: Option<Vec<f32>>,
    /// `shape` joins the alpha with a filled shape's outline (`crate::effect_outline`).
    outline: bool,
    /// Frames of gradient strokes by outside width (bits): see [`stroke_frame`].
    frames: Vec<(u32, Rect)>,
}

impl FxMaps {
    /// The frame of a gradient stroke `st` of this layer (see [`stroke_frame`]), if built.
    pub fn stroke_frame(&self, st: &photosplice_doc::StrokeFx) -> Option<Rect> {
        self.frames.iter().find(|f| f.0 == stroke_widths(st).1.to_bits()).map(|f| f.1)
    }
}

impl FxMaps {
    /// Approximate heap size (for cache budgeting).
    pub fn bytes(&self) -> usize {
        let n = self.shape.v.len();
        let maps: usize = self.per.iter().map(Vec::len).sum();
        (1 + maps
            + usize::from(self.din.is_some())
            + usize::from(self.dout.is_some())
            + usize::from(self.dout_exact.is_some())
            + usize::from(self.vdout.is_some())
            + usize::from(self.vdout_exact.is_some()))
            * n
            * 4
    }

    fn crop_vec(&self, v: &[f32], to: Rect, fill: f32) -> Vec<f32> {
        let (w, h) = (to.width() as usize, to.height() as usize);
        let mut out = vec![fill; w * h];
        let r = self.rect;
        let inter = r.intersect(&to);
        if inter.is_empty() {
            return out;
        }
        let sw = r.width() as usize;
        for y in inter.y0..inter.y1 {
            let s0 = (y - r.y0) as usize * sw + (inter.x0 - r.x0) as usize;
            let d0 = (y - to.y0) as usize * w + (inter.x0 - to.x0) as usize;
            let n = inter.width() as usize;
            out[d0..d0 + n].copy_from_slice(&v[s0..s0 + n]);
        }
        out
    }

    fn crop(&self, m: &Map, to: Rect, fill: f32) -> Map {
        Map { w: to.width() as usize, h: to.height() as usize, v: self.crop_vec(&m.v, to, fill) }
    }
}

/// The distance the two satin copies are offset by. Photoshop draws Distance 0 as 1 px (an
/// edge band of 2/size, measured in Photoshop 27.11), not as two copies cancelling out.
pub fn satin_distance(s: &photosplice_doc::effects::Satin) -> f32 {
    s.distance.max(1.0)
}

fn satin_map(shape: &Map, s: &photosplice_doc::effects::Satin) -> Map {
    let (dx, dy) = offset(s.angle, satin_distance(s));
    let mut a = shape.shifted(dx, dy, 0.0);
    let mut b = shape.shifted(-dx, -dy, 0.0);
    // Two box passes of the satin size (fitted: 0.2 % mean error on psd-tools layer_effects).
    tent(&mut a, s.size);
    tent(&mut b, s.size);
    // The contour shapes each blurred copy before they are differenced (Photoshop corpus
    // satin-default.psd, an S contour: 27/255 → 1.1/255; satin-inverted-ramp-peak-aa.psd
    // 112/255 → 19/255); a linear contour is the same either way.
    let (a, b) = (apply_contour(a, &s.contour, s.anti_alias), apply_contour(b, &s.contour, s.anti_alias));
    let mut m = Map { w: shape.w, h: shape.h, v: a.v.iter().zip(&b.v).map(|(x, y)| (x - y).abs()).collect() };
    if s.invert {
        m = m.map(|v| 1.0 - v);
    }
    for (v, a) in m.v.iter_mut().zip(&shape.v) {
        *v *= a;
    }
    m
}

/// Build every effect map for `layer` from its alpha `shape` over `rect`.
pub fn build_maps(layer: &Layer, shape: Vec<f32>, rect: Rect, light: &GlobalLight, tex: &TextureCtx) -> FxMaps {
    let prepared = PreparedPatterns::new(tex.patterns, PREPARED_PATTERN_BYTES);
    build_maps_prepared(layer, shape, rect, light, tex, &prepared)
}

pub(crate) fn build_maps_prepared(
    layer: &Layer,
    shape: Vec<f32>,
    rect: Rect,
    light: &GlobalLight,
    tex: &TextureCtx,
    patterns: &PreparedPatterns<'_>,
) -> FxMaps {
    let (w, h) = (rect.width() as usize, rect.height() as usize);
    let shape = Map { w, h, v: shape };
    let items: Vec<&Effect> = layer.effects.items.iter().filter(|e| e.enabled()).collect();
    let has_stroke = items.iter().any(|e| matches!(e, Effect::Stroke(_)));
    // A stroke's outer edge keeps the 5 × 5 chamfer while its outside width is under
    // `STROKE_CHAMFER_MAX` — Photoshop matches the chamfer's sub-pixel edge seeding at small
    // offsets — and uses the exact Euclidean transform at or above it, where the chamfer's octagon
    // facets a wide stroke (#3646). Inside stroke distances always keep the chamfer. Each outer
    // field is built only when a stroke of that width is present; the compositor picks per stroke,
    // so one layer may hold both a narrow and a wide stroke.
    let out_widths: Vec<f32> =
        items.iter().filter_map(|e| if let Effect::Stroke(st) = e { Some(stroke_widths(st).1) } else { None }).filter(|w| *w > 0.0).collect();
    let chamfer_out = out_widths.iter().any(|&w| !stroke_outside_exact(w));
    let exact_out = out_widths.iter().any(|&w| stroke_outside_exact(w));
    let (din, dout, dout_exact) = if has_stroke {
        (
            Some(dist_inside_by(&shape, Metric::Chamfer)),
            chamfer_out.then(|| dist_outside_by(&shape, Metric::Chamfer)),
            exact_out.then(|| dist_outside_by(&shape, Metric::Euclidean)),
        )
    } else {
        (None, None, None)
    };
    let mut bevel_paint = vec![BevelPaint::Inner; items.len()];
    let per: Vec<Vec<Map>> = items
        .iter()
        .enumerate()
        .map(|(i, e)| match e {
            Effect::DropShadow(s) => vec![shadow_map(&shape, s, light, false, (rect.x0, rect.y0))],
            Effect::InnerShadow(s) => vec![shadow_map(&shape, s, light, true, (rect.x0, rect.y0))],
            Effect::OuterGlow(g) => vec![glow_map(&shape, g, false, (rect.x0, rect.y0))],
            Effect::InnerGlow(g) => vec![glow_map(&shape, g, true, (rect.x0, rect.y0))],
            Effect::Satin(s) => vec![satin_map(&shape, s)],
            // Stroke Emboss: the shape's own bevel, painted over the stroke effect's area
            // (Photoshop corpus bevel-stroke-*: the stroke is shaded, the layer stays flat). An
            // inside stroke is the shape's inner band, shaded as an inner bevel of the shape
            // (bevel-stroke-emboss-inside-stroke 3.6 %). Without a stroke there is nothing to emboss.
            Effect::BevelEmboss(b)
                if b.style == BevelStyle::StrokeEmboss
                    && items.iter().find_map(|e| if let Effect::Stroke(st) = e { Some(st.position) } else { None }) != Some(StrokePosition::Inside) =>
            {
                bevel_paint[i] = BevelPaint::Stroke;
                let stroke = items.iter().find_map(|e| if let Effect::Stroke(st) = e { Some(st) } else { None });
                match (stroke, &din, dout.as_ref().or(dout_exact.as_ref())) {
                    (Some(st), Some(di), Some(dou)) => {
                        let (in_w, out_w) = stroke_widths(st);
                        let ring = Map {
                            w,
                            h,
                            v: shape
                                .v
                                .iter()
                                .zip(di.iter().zip(dou))
                                .map(|(a, (di, dou))| if *a > INSIDE_EPS { (in_w + 0.5 - di).clamp(0.0, 1.0) } else { (out_w + 0.5 - dou).clamp(0.0, 1.0) })
                                .collect(),
                        };
                        // The slope is the shape's own, steepest at its edge: an outside stroke is
                        // shaded as an outer bevel of the shape, a centre stroke as an emboss of it,
                        // shown over the stroke only (Photoshop corpus, pixels off: bevel-stroke-smooth
                        // 13.1 → 1.7 %, -chisel-hard 10.3 → 4.0 %, centre stroke 20.3 → 12.3 %; as
                        // an inner bevel of the ring the slope fell and rose again across it).
                        let union = Map { w, h, v: shape.v.iter().zip(&ring.v).map(|(a, r)| a.max(*r)).collect() };
                        let style = if st.position == StrokePosition::Center { BevelStyle::Emboss } else { BevelStyle::OuterBevel };
                        let mut maps = bevel_maps(&shape, &Bevel { style, ..(*b).clone() }, light, tex, patterns).0;
                        // An outside stroke is shaded in its own colour before it is mixed over the
                        // backdrop ([`composite_with_effects_prepared`]), so the maps run at full
                        // strength wherever the ring reaches; a centre stroke is shaded over the
                        // composite, weighted by the ring's share.
                        let outside = st.position != StrokePosition::Inside;
                        for m in &mut maps {
                            for ((v, r), u) in m.v.iter_mut().zip(&ring.v).zip(&union.v) {
                                *v *= if *u <= 1e-6 || *r <= 1e-6 {
                                    0.0
                                } else if outside {
                                    1.0
                                } else {
                                    (r / u).min(1.0)
                                };
                            }
                        }
                        maps
                    }
                    _ => Vec::new(),
                }
            }
            Effect::BevelEmboss(b) => {
                let (maps, paint) = bevel_maps(&shape, b, light, tex, patterns);
                bevel_paint[i] = paint;
                maps
            }
            _ => Vec::new(),
        })
        .collect();
    let vector_shape = matches!(layer.content, photosplice_doc::LayerContent::Shape(_));
    // Without an outline (unfilled shapes), estimate the vector outline from local coverage; it
    // picks the chamfer or the exact transform the same way.
    let outline = crate::effect_outline(layer).is_some();
    let (vdout, vdout_exact) = if has_stroke && vector_shape && !outline {
        let lc = local_coverage(&shape);
        (chamfer_out.then(|| dist_outside_by(&lc, Metric::Chamfer)), exact_out.then(|| dist_outside_by(&lc, Metric::Euclidean)))
    } else {
        (None, None)
    };
    let mut frames: Vec<(u32, Rect)> = Vec::new();
    for e in &items {
        if let Effect::Stroke(st) = e
            && matches!(st.paint, FxPaint::Gradient(_))
        {
            let out_w = stroke_widths(st).1;
            if !frames.iter().any(|f| f.0 == out_w.to_bits()) {
                frames.push((out_w.to_bits(), stroke_frame(&shape, out_w, rect)));
            }
        }
    }
    FxMaps { rect, shape, per, bevel_paint, din, dout, dout_exact, vdout, vdout_exact, outline, frames }
}

/// Far outside any shape (distance fill for cropped distance fields).
const FAR: f32 = 1.0e9;

/// Keep an exterior effect behind the layer's shape when Fill is partly or wholly
/// transparent. The visible part beneath a fully opaque fill is already covered by the
/// layer, so avoid attenuating anti-aliased edges twice in that case.
fn knock_out_exterior(map: &mut Map, shape: &Map, fill_opacity: f32) {
    let see_through = 1.0 - fill_opacity.clamp(0.0, 1.0);
    for (coverage, alpha) in map.v.iter_mut().zip(&shape.v) {
        *coverage *= 1.0 - alpha * see_through;
    }
}

/// Whether a knocked-out drop shadow is also knocked out beneath an outside stroke (see
/// `stroke_knock` in `composite_with_effects_prepared`): the GPU compositor leaves such layers
/// to the CPU.
pub fn shadow_knocked_out_by_stroke(e: &[Effect]) -> bool {
    let on = || e.iter().filter(|e| e.enabled());
    on().any(|e| matches!(e, Effect::DropShadow(s) if s.knocks_out)) && on().any(|e| matches!(e, Effect::Stroke(st) if stroke_widths(st).1 > 0.0))
}

/// How far the layer lets a knocked-out drop shadow show beneath its own shape (Layer Knocks Out
/// Drop Shadow), 0 = covered, 1 = knocked out wholly. In a normal mode an opaque layer covers the
/// shadow anyway, so only see-through fill knocks it out (and anti-aliased edges are not
/// attenuated twice). In any other mode the layer blends with what's beneath it, shadow and all:
/// an Exclusion or Difference layer would invert it, Multiply darken by it, Screen show it as
/// grey. Photoshop keeps it out from under the shape, so here it's knocked out wholly.
pub fn shadow_see_through(layer: &Layer) -> f32 {
    if matches!(layer.blend, BlendMode::Normal | BlendMode::Dissolve | BlendMode::PassThrough) { 1.0 - layer.fill_opacity.clamp(0.0, 1.0) } else { 1.0 }
}

/// Blend Interior Effects as Group off (Photoshop's default): the layer's mode applies to its
/// own pixels only, and the interior effects paint over the blended result. The layer is merged
/// into the backdrop before they paint, so an Overlay colour overlay on a black Color-mode layer
/// tints the grey beneath it (#3996). Normal-like modes give the same result either way.
pub fn premerges_layer(layer: &Layer) -> bool {
    !layer.blend_interior && !matches!(layer.blend, BlendMode::Normal | BlendMode::PassThrough | BlendMode::Dissolve)
}

/// Composites `content` (the layer's own pixels over `big`, alpha already
/// masked, clipped layers applied) plus its effects into `backdrop`.
/// `canvas` is the document's bounds (gradients not aligned with the layer span it).
pub fn composite_with_effects(layer: &Layer, content: &Buffer, backdrop: &mut Buffer, maps: &FxMaps, layer_bounds: Rect, canvas: Rect, patterns: &[Pattern]) {
    let prepared = PreparedPatterns::new(patterns, PREPARED_PATTERN_BYTES);
    composite_with_effects_prepared(layer, content, backdrop, maps, layer_bounds, canvas, &prepared, None);
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn composite_with_effects_prepared(
    layer: &Layer,
    content: &Buffer,
    backdrop: &mut Buffer,
    maps: &FxMaps,
    layer_bounds: Rect,
    canvas: Rect,
    patterns: &PreparedPatterns<'_>,
    vstroke: Option<VectorStroke<'_>>,
) {
    let big = content.rect;
    let (w, h) = (big.width() as usize, big.height() as usize);
    // The effect shape: the content's alpha (with a split-off vector stroke, `content` is the
    // unmasked fill and the mask applies to fill ∪ stroke), joined with a filled shape's outline.
    let kmask = |i: usize| vstroke.as_ref().and_then(|v| v.mask).and_then(|m| m.get(i)).copied().unwrap_or(1.0);
    let union = |i: usize, a: f32| vstroke.as_ref().and_then(|v| v.stroke.px.get(i)).map_or(a, |s| kmask(i) * (a + s[3] * (1.0 - a)));
    // Transparency Shapes Layer off: the shape is the whole layer (its masks, as the maps were
    // built), and the content's own transparency acts like fill opacity within it.
    let shapeless = !layer.transparency_shapes;
    let shape = if shapeless {
        maps.crop(&maps.shape, big, 0.0)
    } else if maps.outline {
        let o = maps.crop(&maps.shape, big, 0.0);
        Map { w, h, v: o.v.iter().zip(&content.px).enumerate().map(|(i, (o, p))| o.max(union(i, p[3]))).collect() }
    } else {
        Map { w, h, v: content.px.iter().enumerate().map(|(i, p)| union(i, p[3])).collect() }
    };
    let relative = shapeless || maps.outline || vstroke.is_some();
    let fx = |i: usize, k: usize| maps.crop(&maps.per[i][k], big, 0.0);
    // Layer bounds (gradients aligned with the layer use the whole layer,
    // independent of the render rect).
    let sb = layer_bounds;
    // Linked patterns tile from the effects reference point (else the layer's top-left).
    let anchor = layer.effects.reference.unwrap_or((f64::from(sb.x0), f64::from(sb.y0)));
    let rect = backdrop.rect;
    let before = backdrop.clone();
    // Exterior effects are painted on a backdrop copy extended to `big`.
    let mut work = Buffer::transparent(big);
    for y in rect.y0.max(big.y0)..rect.y1.min(big.y1) {
        for x in rect.x0.max(big.x0)..rect.x1.min(big.x1) {
            work.px[((y - big.y0) as usize) * w + (x - big.x0) as usize] = before.get(x, y);
        }
    }
    let items: Vec<&Effect> = layer.effects.items.iter().filter(|e| e.enabled()).collect();
    let vector_shape = matches!(layer.content, photosplice_doc::LayerContent::Shape(_));
    // Multiple instances: the first listed is on top, so paint in reverse.
    let rev = || items.iter().copied().enumerate().rev();

    let strokes: Vec<&photosplice_doc::StrokeFx> = items.iter().filter_map(|e| if let Effect::Stroke(s) = e { Some(s) } else { None }).collect();
    let ready = !strokes.is_empty();
    let din = if ready { maps.din.as_ref().map(|a| maps.crop_vec(a, big, 0.0)).unwrap_or_default() } else { Vec::new() };
    // A stroke's outer edge picks the chamfer or the exact field by its width
    // ([`stroke_outside_exact`]); both are cropped here, and only the ones a stroke needs exist.
    let crop_out = |v: &Option<Vec<f32>>| v.as_ref().map(|b| maps.crop_vec(b, big, FAR));
    let (dout, dout_exact, vdout, vdout_exact) =
        if ready { (crop_out(&maps.dout), crop_out(&maps.dout_exact), crop_out(&maps.vdout), crop_out(&maps.vdout_exact)) } else { (None, None, None, None) };
    // Shape layers without a filled outline stroke their vector outline, estimated from local
    // coverage; every other layer strokes its shape. The width then picks the field.
    let vector_outline = vector_shape && !maps.outline;
    // The distance field an outside stroke of width `out_w` measures its band with.
    let stroke_field = |out_w: f32| -> &[f32] {
        let exact = stroke_outside_exact(out_w);
        if vector_outline {
            (if exact { vdout_exact.as_deref() } else { vdout.as_deref() }).unwrap_or_default()
        } else if exact {
            dout_exact.as_deref().unwrap_or_default()
        } else {
            dout.as_deref().unwrap_or_default()
        }
    };
    // Layer Knocks Out Drop Shadow counts the outside strokes as the layer's: a 10 % stroke
    // shows the bare backdrop beneath it, not the shadow (psd-tools masks.psd: the header's
    // stroke row was 0.08 dark).
    let stroke_knock: Option<Vec<f32>> = strokes.iter().any(|st| stroke_widths(st).1 > 0.0).then(|| {
        let mut k = vec![0.0f32; w * h];
        for st in &strokes {
            let out_w = stroke_widths(st).1;
            if out_w <= 0.0 {
                continue;
            }
            let d = stroke_field(out_w);
            for ((kv, a), dv) in k.iter_mut().zip(&shape.v).zip(d) {
                if *a <= INSIDE_EPS {
                    *kv = kv.max((out_w + 0.5 - dv).clamp(0.0, 1.0));
                }
            }
        }
        k
    });
    for (i, e) in rev() {
        if let Effect::DropShadow(s) = e {
            let mut m = fx(i, 0);
            if s.knocks_out {
                // The layer hides the shadow beneath it where its fill is see-through (at 100 %
                // fill in a normal mode it covers it anyway; at 0 % the shape shows the bare
                // backdrop), and wholly in a mode that blends with it ([`shadow_see_through`]).
                knock_out_exterior(&mut m, &shape, 1.0 - shadow_see_through(layer));
                if let Some(k) = &stroke_knock {
                    m.v.iter_mut().zip(k).for_each(|(v, k)| *v *= 1.0 - k);
                }
            }
            paint_color(&mut work, &m, rgb(&s.color), s.common.blend, s.common.opacity);
        }
    }
    for (i, e) in rev() {
        if let Effect::OuterGlow(g) = e {
            let mut m = fx(i, 0);
            // The glow is behind the shape, not painted across its interior when Fill is 0 %.
            // Preserve the existing anti-aliased edge when the fill is fully opaque.
            knock_out_exterior(&mut m, &shape, layer.fill_opacity);
            paint_glow(&mut work, &m, g, sb, anchor, big, patterns);
        }
    }

    // The layer: content at fill opacity, then interior effects. Interior effects are painted
    // relative to the layer's shape (alpha = coverage within the shape), then the shape's alpha
    // applies: a colour overlay at 100 % replaces the colour of a half-transparent edge pixel and
    // keeps its alpha, as in Photoshop.
    let fill = layer.fill_opacity;
    // Blend Interior Effects as Group: the interior effects join the layer before its fill
    // opacity fades it (psd-tools advanced-blending: a 100 % colour overlay on an 11 %-fill
    // layer comes out at 11 %).
    let (fill, fill_after) = if layer.blend_interior { (1.0, fill) } else { (fill, 1.0) };
    let inside = |a: f32| a > INSIDE_EPS;
    // Within an outline (or a split-off vector stroke) the content's own transparency (a fading
    // gradient fill) acts like fill opacity: the effects still cover the whole shape.
    let lay_alpha = |i: usize, p: &[f32; 4], a: f32| {
        if !inside(a) {
            0.0
        } else if relative {
            fill * (kmask(i) * p[3] / a).min(1.0)
        } else {
            fill
        }
    };
    let mut lay = Buffer { rect: big, px: content.px.iter().zip(&shape.v).enumerate().map(|(i, (p, a))| [p[0], p[1], p[2], lay_alpha(i, p, *a)]).collect() };
    let gamma = crate::text_gamma(layer);
    let premerge = premerges_layer(layer);
    if premerge {
        for (lp, wp) in lay.px.iter_mut().zip(&work.px) {
            *lp = psblend::composite_gamma(layer.blend, *wp, *lp, 1.0, gamma);
        }
    }
    let rel = |m: Map| -> Map {
        let v = m.v.iter().zip(&shape.v).map(|(m, a)| if inside(*a) { (m / a).min(1.0) } else { 0.0 }).collect();
        Map { w: m.w, h: m.h, v }
    };
    let full = Map { w, h, v: shape.v.iter().map(|a| if inside(*a) { 1.0 } else { 0.0 }).collect() };
    for (_, e) in rev() {
        if let Effect::PatternOverlay { common, name, id, scale, angle, link, phase } = e {
            paint_pattern(&mut lay, &full, patterns, name, id, Placement::anchored(anchor, *link, *phase, *scale, *angle), big, common.blend, common.opacity);
        }
    }
    for (_, e) in rev() {
        if let Effect::GradientOverlay { common, gradient, .. } = e {
            paint_fx(&mut lay, &full, &FxPaint::Gradient(gradient.clone()), sb, canvas, anchor, big, common.blend, common.opacity, patterns)
        }
    }
    for (_, e) in rev() {
        if let Effect::ColorOverlay { common, color } = e {
            paint_color(&mut lay, &full, rgb(color), common.blend, common.opacity);
        }
    }
    for (i, e) in rev() {
        if let Effect::Satin(s) = e {
            let m = rel(fx(i, 0));
            paint_color(&mut lay, &m, rgb(&s.color), s.common.blend, s.common.opacity);
        }
    }
    for (i, e) in rev() {
        if let Effect::InnerGlow(g) = e {
            paint_glow(&mut lay, &rel(fx(i, 0)), g, sb, anchor, big, patterns);
        }
    }
    for (i, e) in rev() {
        if let Effect::InnerShadow(s) = e {
            let m = rel(fx(i, 0));
            paint_color(&mut lay, &m, rgb(&s.color), s.common.blend, s.common.opacity);
        }
    }
    if fill_after < 1.0 {
        for p in &mut lay.px {
            p[3] *= fill_after;
        }
    }
    // A stroked shape's vector stroke: Photoshop draws it above the fill's clipped layers and
    // interior effects, below the stroke effect (psd-tools stroke-composite).
    if let Some(vs) = &vstroke {
        for (i, ((p, s), a)) in lay.px.iter_mut().zip(&vs.stroke.px).zip(&shape.v).enumerate() {
            if s[3] > 0.0 && inside(*a) {
                *p = psblend::composite(BlendMode::Normal, *p, [s[0], s[1], s[2], layer.fill_opacity * (kmask(i) * s[3] / a).min(1.0)], 1.0);
            }
        }
    }
    // Strokes. Inside parts are painted over the layer (bottom instance first); outside parts
    // are slid beneath it, so the first listed (top) instance is processed first.
    let widths = stroke_widths;
    // A gradient stroke aligned with the layer spans the stroke's own extent.
    let frame = |st: &photosplice_doc::StrokeFx| maps.stroke_frame(st).unwrap_or(sb);
    for st in strokes.iter().rev().copied() {
        let (in_w, _) = widths(st);
        if in_w <= 0.0 {
            continue;
        }
        let mut m = Map::new(w, h, 0.0);
        for ((mv, a), dv) in m.v.iter_mut().zip(&shape.v).zip(&din) {
            *mv = if inside(*a) { (in_w + 0.5 - dv).clamp(0.0, 1.0) } else { 0.0 };
        }
        let embossed: Vec<usize> = if st.position == StrokePosition::Center {
            rev().filter(|(i, e)| matches!(e, Effect::BevelEmboss(_)) && maps.bevel_paint[*i] == BevelPaint::Stroke && !maps.per.get(*i).is_none_or(Vec::is_empty)).map(|(i, _)| i).collect()
        } else {
            Vec::new()
        };
        if embossed.is_empty() {
            paint_fx(&mut lay, &m, &st.paint, frame(st), canvas, anchor, big, st.common.blend, st.common.opacity, patterns);
        } else {
            // Stroke Emboss on a centre stroke: the inside half shaded in its own colour too,
            // then laid over the layer by its coverage.
            let mut stroked = lay.clone();
            let ones = Map { w, h, v: m.v.iter().map(|&v| if v > 0.0 { 1.0 } else { 0.0 }).collect() };
            paint_fx(&mut stroked, &ones, &st.paint, frame(st), canvas, anchor, big, st.common.blend, 1.0, patterns);
            for i in embossed {
                if let Some(Effect::BevelEmboss(b)) = items.get(i) {
                    paint_color(&mut stroked, &rel(fx(i, 0)), rgb(&b.highlight_color), b.highlight.blend, b.highlight.opacity);
                    paint_color(&mut stroked, &rel(fx(i, 1)), rgb(&b.shadow_color), b.shadow.blend, b.shadow.opacity);
                }
            }
            for ((p, q), k) in lay.px.iter_mut().zip(&stroked.px).zip(&m.v) {
                let k = k * st.common.opacity;
                if k > 0.0 {
                    *p = std::array::from_fn(|c| p[c] + (q[c] - p[c]) * k);
                }
            }
        }
    }
    for (i, e) in rev() {
        if let Effect::BevelEmboss(b) = e
            && maps.bevel_paint[i] == BevelPaint::Inner
        {
            let (hi, sh) = (rel(fx(i, 0)), rel(fx(i, 1)));
            paint_color(&mut lay, &hi, rgb(&b.highlight_color), b.highlight.blend, b.highlight.opacity);
            paint_color(&mut lay, &sh, rgb(&b.shadow_color), b.shadow.blend, b.shadow.opacity);
        }
    }
    // The shape's own alpha.
    for (p, a) in lay.px.iter_mut().zip(&shape.v) {
        p[3] *= a.min(1.0);
    }
    // Outside parts lie beneath the layer and blend onto the backdrop with their own mode. A
    // higher stroke covers the ones below it (Photoshop: a Multiply stroke listed above a wider
    // Normal stroke multiplies the backdrop, not the lower stroke). Each stroke's share is its
    // coverage × opacity not yet taken by strokes above; for Normal strokes this equals stacking
    // them top over bottom.
    if strokes.iter().any(|st| widths(st).1 > 0.0) {
        let base = work.clone();
        let mut acc = vec![[0.0f32; 4]; w * h]; // premultiplied Σ share·blended
        let mut cover = vec![0.0f32; w * h];
        for st in strokes.iter().copied() {
            let (_, out_w) = widths(st);
            if out_w <= 0.0 {
                continue;
            }
            // Shape layers: Photoshop strokes the vector outline (estimated from local coverage
            // for unfilled shapes); the stroke never shows through the shape's pixels. Along a
            // filled shape's outline it covers the part of each edge pixel outside the path.
            let d = stroke_field(out_w);
            let mut share = vec![0.0f32; w * h];
            for (i, sh) in share.iter_mut().enumerate() {
                let band = (out_w + 0.5 - d[i]).clamp(0.0, 1.0);
                let k = if maps.outline {
                    band * outline_share(shape.v[i], lay.px[i][3])
                } else if inside(shape.v[i]) {
                    if vector_shape {
                        0.0
                    } else {
                        // The stroke lies outside the layer's pixels. Beneath the layer it may show
                        // only through the part of the pixel the shape doesn't cover (1 - a); the
                        // layer composited on top covers c = a × fill, so the share beneath is
                        // (1 - a) / (1 - c). At 100 % fill that is 1 (the layer hides the rest, as
                        // before); at 0 % fill the interior stays clear (Fill 0 % + Outside stroke).
                        let c = lay.px[i][3].clamp(0.0, 1.0);
                        if c >= 1.0 { 1.0 } else { ((1.0 - shape.v[i].clamp(0.0, 1.0)) / (1.0 - c)).clamp(0.0, 1.0) }
                    }
                } else {
                    band
                };
                *sh = k * st.common.opacity * (1.0 - cover[i]);
                cover[i] += *sh;
            }
            // The stroke blended over the backdrop at full coverage.
            let mut blended = base.clone();
            let ones = Map { w, h, v: share.iter().map(|&s| if s > 0.0 { 1.0 } else { 0.0 }).collect() };
            paint_fx(&mut blended, &ones, &st.paint, frame(st), canvas, anchor, big, st.common.blend, 1.0, patterns);
            // Stroke Emboss shades the stroke itself, so a partly covered edge pixel keeps its
            // backdrop share unshaded (Reference corpus bevel-stroke-smooth: edge pixels nearly
            // white where shading the composite made them up to 125/255 dark).
            if st.position != StrokePosition::Inside {
                for (i, e) in rev() {
                    if let Effect::BevelEmboss(b) = e
                        && maps.bevel_paint[i] == BevelPaint::Stroke
                        && !maps.per.get(i).is_none_or(Vec::is_empty)
                    {
                        paint_color(&mut blended, &fx(i, 0), rgb(&b.highlight_color), b.highlight.blend, b.highlight.opacity);
                        paint_color(&mut blended, &fx(i, 1), rgb(&b.shadow_color), b.shadow.blend, b.shadow.opacity);
                    }
                }
            }
            for ((a, b), s) in acc.iter_mut().zip(&blended.px).zip(&share) {
                if *s > 0.0 {
                    for c in 0..3 {
                        a[c] += s * b[c] * b[3];
                    }
                    a[3] += s * b[3];
                }
            }
        }
        for ((p, a), c) in work.px.iter_mut().zip(&acc).zip(&cover) {
            if *c <= 0.0 {
                continue;
            }
            let k = 1.0 - c.min(1.0);
            let alpha = p[3] * k + a[3];
            if alpha > 0.0 {
                *p = [(p[0] * p[3] * k + a[0]) / alpha, (p[1] * p[3] * k + a[1]) / alpha, (p[2] * p[3] * k + a[2]) / alpha, alpha.min(1.0)];
            }
        }
    }
    for (i, e) in rev() {
        if let Effect::BevelEmboss(b) = e
            && maps.bevel_paint[i] == BevelPaint::Outer
        {
            // Beneath the layer like a glow: a see-through fill shows the bare backdrop under its
            // shape, not the bevel (Reference kit hard-outer-s20 at Fill 0 %: an edge pixel half
            // covered shades half as much; it was shaded in full, up to 0.43 off).
            let (mut hi, mut sh) = (fx(i, 0), fx(i, 1));
            knock_out_exterior(&mut hi, &shape, layer.fill_opacity);
            knock_out_exterior(&mut sh, &shape, layer.fill_opacity);
            paint_color(&mut work, &hi, rgb(&b.highlight_color), b.highlight.blend, b.highlight.opacity);
            paint_color(&mut work, &sh, rgb(&b.shadow_color), b.shadow.blend, b.shadow.opacity);
        }
    }

    // Layer (with interior effects) onto the exterior result, in the layer's mode.
    let mode = if premerge || layer.blend == BlendMode::PassThrough { BlendMode::Normal } else { layer.blend };
    for (wp, lp) in work.px.iter_mut().zip(&lay.px) {
        if lp[3] > 0.0 {
            *wp = psblend::composite_gamma(mode, *wp, *lp, 1.0, gamma);
        }
    }
    // An outside stroke's Stroke Emboss was shaded with the stroke itself, above.
    let outside_stroke = strokes.first().is_some_and(|st| st.position != StrokePosition::Inside);
    // Emboss styles shade the composited layer (their maps cover inside and outside). Painting
    // them into the layer before its (text-gamma) composite left a type layer's lit top edges up
    // to 6/255 dark (psd-tools layer_effects Emboss: 7.8 → 5.7/255).
    for (i, e) in rev() {
        if let Effect::BevelEmboss(b) = e
            && (maps.bevel_paint[i] == BevelPaint::Both || (maps.bevel_paint[i] == BevelPaint::Stroke && !outside_stroke))
            && !maps.per.get(i).is_none_or(Vec::is_empty)
        {
            paint_color(&mut work, &fx(i, 0), rgb(&b.highlight_color), b.highlight.blend, b.highlight.opacity);
            paint_color(&mut work, &fx(i, 1), rgb(&b.shadow_color), b.shadow.blend, b.shadow.opacity);
        }
    }
    // Layer opacity applies to the whole stack.
    let op = layer.opacity;
    for y in rect.y0..rect.y1 {
        for x in rect.x0..rect.x1 {
            let i = ((y - rect.y0) as usize) * rect.width() as usize + (x - rect.x0) as usize;
            let a = before.px[i];
            let b = if x >= big.x0 && x < big.x1 && y >= big.y0 && y < big.y1 { work.px[((y - big.y0) as usize) * w + (x - big.x0) as usize] } else { a };
            backdrop.px[i] = mix_premul(a, b, op);
        }
    }
}

/// Coverage of an outside stroke beneath a filled shape's edge pixel, for a pixel the outline
/// covers `cov` of and the layer (with its interior effects) ends at alpha `l`. Photoshop treats
/// the stroke and the shape as disjoint areas: the stroke fills the part outside the path and
/// the layer adds its own alpha (psd-tools stroke-effects: a 58 % covered edge pixel of an
/// opaque fill ends opaque, one of a faded fill at 42 % + its alpha). Composited beneath the
/// layer, `share + l (1 - share) = (1 - cov) + l`.
pub fn outline_share(cov: f32, l: f32) -> f32 {
    let outside = (1.0 - cov.clamp(0.0, 1.0)).max(0.0);
    let rest = 1.0 - l.clamp(0.0, 1.0);
    if rest <= 1e-6 { 1.0 } else { (outside / rest).min(1.0) }
}

/// (inside width, outside width) of a stroke.
pub fn stroke_widths(st: &photosplice_doc::StrokeFx) -> (f32, f32) {
    match st.position {
        StrokePosition::Outside => (0.0, st.size),
        StrokePosition::Inside => (st.size, 0.0),
        StrokePosition::Center => (st.size / 2.0, st.size / 2.0),
    }
}

/// The frame a gradient stroke aligned with the layer is laid out in: the pixel bounds of the
/// effect shape (over `rect`) grown by the stroke's outside width less one pixel. Photoshop
/// spans the gradient over the stroke's extent, not the layer's: a 4 px outside stroke around a
/// 22 px square runs its 90° gradient over 28 px (psd-tools stroke-effects), a 3 px one around
/// 36 px of type over 40 px (psd-tools effect-stroke-gradient).
fn stroke_frame(shape: &Map, out_w: f32, rect: Rect) -> Rect {
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    for y in 0..shape.h {
        for x in 0..shape.w {
            if shape.v[y * shape.w + x] > INSIDE_EPS {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
        }
    }
    if x0 == usize::MAX {
        return rect;
    }
    let grow = if out_w.is_finite() { (out_w.min(MAX_REACH).ceil() as i32 - 1).max(0) } else { 0 };
    Rect::new(rect.x0 + x0 as i32, rect.y0 + y0 as i32, rect.x0 + x1 as i32, rect.y0 + y1 as i32).inflate(grow)
}

/// A stroked shape's vector stroke, composited apart from its fill (which the compositor then
/// passes unmasked): the stroke's pixels over the render rect and the layer's mask values.
pub(crate) struct VectorStroke<'a> {
    pub stroke: &'a Buffer,
    pub mask: Option<&'a [f32]>,
}

/// Premultiplied linear interpolation between two straight-alpha pixels.
fn mix_premul(a: [f32; 4], b: [f32; 4], k: f32) -> [f32; 4] {
    let alpha = a[3] + (b[3] - a[3]) * k;
    if alpha <= 0.0 {
        return [0.0; 4];
    }
    let mut out = [0.0; 4];
    for c in 0..3 {
        out[c] = (a[c] * a[3] + (b[c] * b[3] - a[c] * a[3]) * k) / alpha;
    }
    out[3] = alpha;
    out
}

/// A distance field of an effect shape, as the map builders above compute it (for the GPU
/// compositor, which builds the rest of the maps itself; keep in sync with `shadow_map`,
/// `glow_map`, `bevel_maps` and `build_maps`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FieldKind {
    /// `dist_outside` of the shape.
    Outside,
    /// `dist_outside_smooth` of the shape: bevels.
    BevelOutside,
    /// `dist_inside` of the shape: bevels.
    Inside,
    /// Chamfer distance outside the shape: outside strokes, precise outer glows, and the spread
    /// of drop shadows and softer outer glows ([`dilate`]).
    StrokeOutside,
    /// Chamfer distance outside `1 - alpha`: the choke of inner shadows and softer inner glows.
    ChokeInside,
    /// Stroke distance inside the shape: inside strokes, precise inner glows.
    StrokeInside,
    /// Exact Euclidean distance outside the shape: a wide raster stroke's outer edge (see
    /// [`stroke_outside_exact`]). The chamfer's level sets are polygons (an ~8-fold octagon), so a
    /// wide stroke's outer boundary facets; the shadow / glow spread above keeps the chamfer (#3646).
    StrokeOutsideExact,
    /// Stroke distance outside a shape layer's outline (from its local coverage), exact Euclidean.
    StrokeOutsideVector,
    /// Chamfer distance outside a shape layer's outline (from its local coverage): a narrow vector
    /// stroke's outer edge, where the chamfer matches Photoshop (see [`STROKE_CHAMFER_MAX`]).
    StrokeOutsideVectorChamfer,
}

/// The `kind` distance field of the shape `alpha` (row-major `w × h`). Values past `_reach` pixels
/// from the shape's edge are only meant to be large (every field here is computed in full).
pub fn distance_field(kind: FieldKind, alpha: Vec<f32>, w: usize, h: usize, _reach: f32) -> Vec<f32> {
    let s = Map { w, h, v: alpha };
    match kind {
        FieldKind::Outside => dist_outside(&s),
        FieldKind::BevelOutside => dist_outside_smooth(&s),
        FieldKind::Inside => dist_inside(&s),
        FieldKind::StrokeOutside => dist_outside_by(&s, Metric::Chamfer),
        FieldKind::ChokeInside => dist_outside_by(&s.map(|a| 1.0 - a), Metric::Chamfer),
        FieldKind::StrokeInside => dist_inside_by(&s, Metric::Chamfer),
        FieldKind::StrokeOutsideExact => dist_outside_by(&s, Metric::Euclidean),
        FieldKind::StrokeOutsideVector => dist_outside_by(&local_coverage(&s), Metric::Euclidean),
        FieldKind::StrokeOutsideVectorChamfer => dist_outside_by(&local_coverage(&s), Metric::Chamfer),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photosplice_doc::GradientStyle;

    /// Runs `f` with every parallel pass on this thread using `n` threads (1 = in order), whatever its size.
    fn with_threads<R>(n: usize, f: impl FnOnce() -> R) -> R {
        struct Restore(Option<usize>);
        impl Drop for Restore {
            fn drop(&mut self) {
                THREADS.with(|t| t.set(self.0));
            }
        }
        let _restore = Restore(THREADS.with(|t| t.replace(Some(n))));
        f()
    }

    fn bits(v: &[f32]) -> Vec<u32> {
        v.iter().map(|x| x.to_bits()).collect()
    }

    #[test]
    fn edt_is_exact() {
        let (w, h) = (7, 5);
        let mut inside = vec![false; w * h];
        inside[2 * w + 3] = true;
        let d = edt(&inside, w, h);
        assert_eq!(d[2 * w + 3], 0.0);
        assert!((d[2 * w + 6] - 3.0).abs() < 1e-6);
        assert!((d[0] - (9.0f32 + 4.0).sqrt()).abs() < 1e-5);
        assert!((d[4 * w + 5] - (4.0f32 + 4.0).sqrt()).abs() < 1e-5);
    }

    #[test]
    fn edt_two_seeds() {
        let (w, h) = (10, 1);
        let mut inside = vec![false; w];
        inside[0] = true;
        inside[9] = true;
        let d = edt(&inside, w, h);
        assert_eq!(d, vec![0.0, 1.0, 2.0, 3.0, 4.0, 4.0, 3.0, 2.0, 1.0, 0.0]);
    }

    #[test]
    fn blur_preserves_mass() {
        let mut m = Map::new(21, 21, 0.0);
        m.v[10 * 21 + 10] = 1.0;
        blur(&mut m, 6.0);
        let sum: f32 = m.v.iter().sum();
        assert!((sum - 1.0).abs() < 1e-4, "{sum}");
        assert!(m.v[10 * 21 + 10] < 1.0);
    }

    #[test]
    fn gradient_geometry() {
        let b = Rect::new(0, 0, 100, 100);
        let t = |s, x, y| gradient_t(s, 0.0, 1.0, false, (0.0, 0.0), b, x, y);
        assert!(t(GradientStyle::Linear, 0.0, 50.0) < 0.01);
        // Linear / reflected sample pixel corners: the centre is reached half a pixel later.
        assert!((t(GradientStyle::Linear, 50.5, 50.0) - 0.5).abs() < 1e-6);
        assert!(t(GradientStyle::Linear, 100.0, 50.0) > 0.99);
        assert!(t(GradientStyle::Reflected, 50.5, 50.0) < 1e-6);
        assert!((t(GradientStyle::Reflected, 0.0, 50.0) - 1.0).abs() < 1e-6);
        assert!(t(GradientStyle::Radial, 50.0, 50.0) < 1e-6);
        assert!((t(GradientStyle::Radial, 50.0, 0.0) - 1.0).abs() < 1e-6);
        assert!((t(GradientStyle::Diamond, 75.0, 75.0) - 1.0).abs() < 1e-6);
        let ang = t(GradientStyle::Angle, 50.0, 0.0);
        assert!((ang - 0.75).abs() < 1e-3, "{ang}");
        let rev = gradient_t(GradientStyle::Linear, 0.0, 1.0, true, (0.0, 0.0), b, 0.0, 50.0);
        assert!(rev > 0.99);
    }

    #[test]
    fn effect_gradients_snap_end_points_and_sample_corners() {
        // 87° over 600 × 60 at (99, 422): end points (397, 482) and (400, 422), so the overlay runs
        // along (3, -60) from its midpoint (398.5, 452), sampled at pixel corners.
        let frame = Rect::new(99, 422, 699, 482);
        let (a, sc, o) = crate::fill_layout::gradient_layout(GradientStyle::Linear, 87.0, 1.0, (0.0, 0.0), frame);
        assert!((a - (60f32).atan2(3.0).to_degrees()).abs() < 1e-3, "{a}");
        let t = |x: f32, y: f32| gradient_t(GradientStyle::Linear, a, sc, false, o, frame, x, y);
        assert!((t(399.0, 452.5) - 0.5).abs() < 1e-4, "{}", t(399.0, 452.5));
        assert!(t(400.5, 422.5) > 0.99 && t(397.5, 482.5) < 0.01);
    }

    #[test]
    fn chamfer_uses_euclidean_step_lengths() {
        let (w, h) = (9, 9);
        let mut inside = vec![false; w * h];
        inside[0] = true;
        let (d, near) = chamfer_nearest(&inside, w, h);
        let at = |x: usize, y: usize| d[y * w + x];
        assert!((at(3, 0) - 3.0).abs() < 1e-5);
        assert!((at(2, 2) - 2.0 * std::f32::consts::SQRT_2).abs() < 1e-5);
        assert!((at(1, 2) - 5f32.sqrt()).abs() < 1e-5);
        assert!((at(1, 3) - (5f32.sqrt() + 1.0)).abs() < 1e-5, "knight + straight, not √10");
        assert!((at(2, 3) - (5f32.sqrt() + std::f32::consts::SQRT_2)).abs() < 1e-5);
        assert!(near.iter().all(|&n| n == 0));
    }

    #[test]
    fn spread_dilates_with_the_chamfer_metric() {
        // One opaque pixel grown by 3 px: at offset (1, 3) the chamfer distance is √5 + 1 = 3.236
        // (Euclidean 3.162), so the anti-aliased rim keeps 3 + 0.5 − (3.236 − 0.5) = 0.764.
        let mut m = Map::new(9, 9, 0.0);
        m.v[4 * 9 + 4] = 1.0;
        let d = dilate(&m, 3.0);
        assert!((d.v[7 * 9 + 5] - 0.764).abs() < 1e-3, "{}", d.v[7 * 9 + 5]);
        // (2, 3): √5 + √2 = 3.650 (Euclidean 3.606) leaves 0.350; axis steps are exact.
        assert!((d.v[7 * 9 + 6] - (4.0 - 5f32.sqrt() - std::f32::consts::SQRT_2)).abs() < 1e-4, "{}", d.v[7 * 9 + 6]);
        assert_eq!(d.v[4 * 9 + 7], 1.0);
    }

    #[test]
    fn running_sum_tent_matches_the_kernel() {
        let (w, h) = (37usize, 23usize);
        let src: Vec<f32> = (0..w * h).map(|i| ((i * 7919) % 101) as f32 / 100.0).collect();
        for width in [1.0f32, 2.0, 3.5, 5.0, 8.25, 41.0] {
            let mut m = Map { w, h, v: src.clone() };
            tent(&mut m, width);
            let mut want = Map { w, h, v: src.clone() };
            tent_direct(&mut want, width);
            for (a, b) in m.v.iter().zip(&want.v) {
                assert!((a - b).abs() < 1e-5, "width {width}: {a} vs {b}");
            }
        }
    }

    #[test]
    fn tent_kernel_is_two_boxes() {
        let (r, k) = tent_kernel(5.0);
        assert_eq!(r, 4);
        let sum: f32 = k.iter().sum();
        assert!((sum - 1.0).abs() < 1e-6);
        assert!((k[4] - 5.0 / 25.0).abs() < 1e-6 && (k[0] - 1.0 / 25.0).abs() < 1e-6);
        // Fractional widths taper the end taps; width 1 is the identity.
        let (r, k) = tent_kernel(4.0);
        assert_eq!(r, 4);
        assert!(k[0] > 0.0 && k[0] < 1.0 / 25.0);
        assert_eq!(tent_kernel(1.0), (0, vec![1.0]));
        // #1543: a huge, infinite or NaN width is capped, not turned into a kernel of 2^62 taps.
        for w in [1e30, f32::INFINITY, f32::MAX] {
            assert_eq!(tent_kernel(w), tent_kernel(MAX_REACH), "{w}");
        }
        assert_eq!(tent_kernel(f32::NAN), (0, vec![1.0]));
    }

    fn no_tex() -> TextureCtx<'static> {
        TextureCtx { rect: Rect::new(0, 0, 1, 1), patterns: &[], anchor: (0.0, 0.0) }
    }

    fn square(n: usize, x0: usize, x1: usize) -> Map {
        let mut m = Map::new(n, n, 0.0);
        for y in x0..x1 {
            for x in x0..x1 {
                m.v[y * n + x] = 1.0;
            }
        }
        m
    }

    fn bevel_of(style: BevelStyle, technique: BevelTechnique) -> Bevel {
        Bevel {
            enabled: true,
            style,
            technique,
            depth: 1.0,
            up: true,
            size: 5.0,
            soften: 0.0,
            angle: 90.0,
            altitude: 30.0,
            use_global_light: false,
            gloss_contour: Contour::Linear,
            gloss_anti_alias: false,
            highlight: photosplice_doc::FxCommon::new(BlendMode::Screen, 0.75),
            highlight_color: photosplice_color::Color::WHITE,
            shadow: photosplice_doc::FxCommon::new(BlendMode::Multiply, 0.75),
            shadow_color: photosplice_color::Color::BLACK,
            contour: None,
            texture: None,
        }
    }

    #[test]
    fn smooth_inner_bevel_lights_the_top_edge_inside_only() {
        let shape = square(40, 10, 30);
        let (maps, paint) = bevel_maps(
            &shape,
            &bevel_of(BevelStyle::InnerBevel, BevelTechnique::Smooth),
            &GlobalLight::default(),
            &no_tex(),
            &PreparedPatterns::new(&[], PREPARED_PATTERN_BYTES),
        );
        assert_eq!(paint, BevelPaint::Inner);
        let at = |m: &Map, x: usize, y: usize| m.v[y * 40 + x];
        // Light from the top: highlight along the top edge, shadow along the bottom, flat middle.
        assert!(at(&maps[0], 20, 10) > 0.5 && at(&maps[1], 20, 10) == 0.0);
        assert!(at(&maps[1], 20, 29) > 0.5);
        assert!(at(&maps[0], 20, 20) < 1e-3 && at(&maps[1], 20, 20) < 1e-3);
        // Nothing outside the shape; the ramp reaches about `size` inside.
        assert_eq!(at(&maps[0], 20, 8), 0.0);
        assert!(at(&maps[0], 20, 15) < 0.05);
    }

    #[test]
    fn emboss_paints_both_sides_and_pillow_flips_the_outside() {
        let shape = square(40, 10, 30);
        let l = GlobalLight::default();
        let (e, paint) =
            bevel_maps(&shape, &bevel_of(BevelStyle::Emboss, BevelTechnique::Smooth), &l, &no_tex(), &PreparedPatterns::new(&[], PREPARED_PATTERN_BYTES));
        assert_eq!((paint, e.len()), (BevelPaint::Both, 2));
        let at = |m: &Map, x: usize, y: usize| m.v[y * 40 + x];
        // Emboss: the slope across the top edge faces the light on both sides.
        assert!(at(&e[0], 20, 10) > 0.3 && at(&e[0], 20, 9) > 0.3);
        let (p, _) =
            bevel_maps(&shape, &bevel_of(BevelStyle::PillowEmboss, BevelTechnique::Smooth), &l, &no_tex(), &PreparedPatterns::new(&[], PREPARED_PATTERN_BYTES));
        assert!(at(&p[0], 20, 10) > 0.3 && at(&p[1], 20, 9) > 0.3, "pillow: outside top edge in shadow");
    }

    #[test]
    fn emboss_width_rounds_up_and_shades_past_the_blur() {
        let mut b = bevel_of(BevelStyle::Emboss, BevelTechnique::Smooth);
        b.size = 41.0;
        let g = bevel_geom(&b);
        assert_eq!((g.width, g.depth), (21.0, 21.0));
        b.size = 5.0;
        assert_eq!(bevel_geom(&b).width, 3.0);
        // The first row past the height blur's reach still slopes (its upper neighbour is raised),
        // so it shades; the row after it does not.
        let shape = square(60, 20, 40);
        b.size = 9.0;
        b.angle = 90.0;
        let (m, _) = bevel_maps(&shape, &b, &GlobalLight::default(), &no_tex(), &PreparedPatterns::new(&[], PREPARED_PATTERN_BYTES));
        let reach = tent_kernel(bevel_geom(&b).width).0 as usize;
        let row = 39 + reach + 1; // last shape row + reach + 1: height 0, neighbour above raised
        assert!(m[1].v[row * 60 + 30] > 0.0, "{}", m[1].v[row * 60 + 30]);
        assert_eq!(m[1].v[(row + 1) * 60 + 30], 0.0);
    }

    #[test]
    fn par_units_visits_every_unit_once_with_its_own_slices() {
        // Lengths that leave a short last unit, no second slice, units of different sizes, and nothing at all.
        let cases: [(usize, usize, usize, usize); 8] =
            [(0, 3, 0, 0), (1, 1, 1, 1), (10, 3, 0, 0), (64, 8, 64, 8), (65, 8, 13, 1), (200, 1, 200, 1), (7, 7, 14, 2), (9, 4, 0, 5)];
        for (la, ua, lb, ub) in cases {
            for threads in [1, 2, 3, 8, 64] {
                let (mut a, mut b) = (vec![0u32; la], vec![0u32; lb]);
                let calls = std::sync::atomic::AtomicUsize::new(0);
                with_threads(threads, || {
                    par_units(
                        la,
                        &mut a,
                        ua,
                        &mut b,
                        ub,
                        || (),
                        |_, u, sa, sb| {
                            calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            assert!(sa.len() <= ua && sb.len() <= ub);
                            for (k, v) in sa.iter_mut().enumerate() {
                                *v = (u * ua + k) as u32 + 1;
                            }
                            for (k, v) in sb.iter_mut().enumerate() {
                                *v = (u * ub + k) as u32 + 1;
                            }
                        },
                    );
                });
                assert_eq!(calls.into_inner(), la.div_ceil(ua), "{la} {ua} {lb} {ub} on {threads}");
                assert!(a.iter().enumerate().all(|(i, &v)| v == i as u32 + 1), "a: {la} {ua} {lb} {ub} on {threads}");
                // `b` is written for the units that exist in `a` and no further.
                let written = (la.div_ceil(ua) * ub).min(lb);
                assert!(b.iter().enumerate().all(|(i, &v)| v == if i < written { i as u32 + 1 } else { 0 }), "b: {la} {ua} {lb} {ub} on {threads}");
            }
        }
        // A unit size of 0 does nothing.
        let mut a = vec![1u8; 4];
        par_units(4, &mut a, 0, &mut [] as &mut [()], 0, || (), |_, _, _, _| panic!("no units"));
    }

    #[test]
    fn nearest_distances_do_not_depend_on_the_threads() {
        // Sizes that do not divide into the thread groups, lines of 1 pixel, and a noisy map where
        // many pixels are equally far from two seeds (the nearest one must resolve the same way).
        for (w, h) in [(37usize, 23usize), (1, 40), (40, 1), (2, 2), (101, 3), (64, 64)] {
            let mut seed = 7u32;
            let inside: Vec<bool> = (0..w * h)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (seed >> 24).is_multiple_of(11)
                })
                .collect();
            let (d1, n1) = with_threads(1, || edt_nearest(&inside, w, h));
            for threads in [2, 3, 8] {
                let (d, n) = with_threads(threads, || edt_nearest(&inside, w, h));
                assert_eq!((bits(&d), n), (bits(&d1), n1.clone()), "{w}x{h} on {threads} threads");
            }
            // And the nearest pixel is a seed at exactly that distance.
            for (i, (&d, &q)) in d1.iter().zip(&n1).enumerate() {
                if inside.iter().any(|&b| b) {
                    let (dx, dy) = ((i % w) as f32 - (q as usize % w) as f32, (i / w) as f32 - (q as usize / w) as f32);
                    assert!(inside[q as usize] && (dx.hypot(dy) - d).abs() < 1e-3, "{w}x{h} at {i}: {d} vs seed {q}");
                }
            }
        }
    }

    #[test]
    fn nearest_distances_survive_degenerate_sizes() {
        // The slice does not match the size, or there is nothing: no panic, and a result per input pixel.
        for (len, w, h) in [(0usize, 0usize, 0usize), (0, 5, 0), (0, 0, 5), (6, 2, 2), (6, 4, 2), (1, 0, 1)] {
            let (d, n) = edt_nearest(&vec![true; len], w, h);
            assert_eq!((d.len(), n.len()), (len, len), "{len} px as {w}x{h}");
        }
    }

    #[test]
    fn chisel_hard_has_flat_facets() {
        let shape = square(48, 10, 38);
        let (m, _) = bevel_maps(
            &shape,
            &bevel_of(BevelStyle::InnerBevel, BevelTechnique::ChiselHard),
            &GlobalLight::default(),
            &no_tex(),
            &PreparedPatterns::new(&[], PREPARED_PATTERN_BYTES),
        );
        let at = |x: usize, y: usize| m[0].v[y * 48 + x];
        // A constant slope: equal highlight along the top facet.
        assert!(at(24, 12) > 0.1 && (at(24, 12) - at(24, 13)).abs() < 1e-3);
    }

    /// Photoshop's chisel facets (oracle bevel-inner-chisel-hard: size 10, depth 100 %, light at
    /// 120° / 30°) shade at highlight 0.27 (left), 0.53 (top) and shadow 0.44 (right), 0.71
    /// (bottom), as measured from its composite. A 45° facet gave 0.32 / 0.77 / 0.91 / 1.0.
    #[test]
    fn chisel_facets_match_photoshop() {
        let shape = square(64, 10, 54);
        let mut b = bevel_of(BevelStyle::InnerBevel, BevelTechnique::ChiselHard);
        (b.size, b.angle, b.altitude) = (10.0, 120.0, 30.0);
        let (m, _) = bevel_maps(&shape, &b, &GlobalLight::default(), &no_tex(), &PreparedPatterns::new(&[], PREPARED_PATTERN_BYTES));
        let at = |k: usize, x: usize, y: usize| m[k].v[y * 64 + x];
        for (got, want) in [(at(0, 13, 32), 0.266), (at(0, 32, 13), 0.534), (at(1, 50, 32), 0.445), (at(1, 32, 50), 0.71)] {
            assert!((got - want).abs() < 0.03, "{got} vs {want}");
        }
    }

    /// Stroke Emboss shades the stroke effect's area (an outside stroke: the ring outside the
    /// shape) and leaves the layer's interior flat; without a stroke there is nothing to shade.
    #[test]
    fn stroke_emboss_shades_the_stroke() {
        let n = 48;
        let shape = square(n, 12, 36);
        let stroke = Effect::Stroke(photosplice_doc::StrokeFx {
            common: photosplice_doc::FxCommon::new(BlendMode::Normal, 1.0),
            size: 6.0,
            position: StrokePosition::Outside,
            paint: FxPaint::Color(photosplice_doc::Color::BLACK),
        });
        let emboss = Effect::BevelEmboss(Bevel { size: 5.0, ..bevel_of(BevelStyle::StrokeEmboss, BevelTechnique::Smooth) });
        let mut layer = Layer::raster("l", photosplice_color::PixelFormat::RGBA8);
        layer.effects.items = vec![stroke, emboss.clone()];
        let maps = build_maps(&layer, shape.v.clone(), Rect::new(0, 0, n as i32, n as i32), &GlobalLight::default(), &no_tex());
        let (hi, sh) = (&maps.per[1][0], &maps.per[1][1]);
        let at = |m: &Map, x: usize, y: usize| m.v[y * n + x];
        // The ring's lit and shaded sides are painted; the middle of the shape isn't.
        assert!(at(hi, 8, 24) > 0.05 || at(sh, 8, 24) > 0.05 || at(hi, 24, 8) > 0.05 || at(sh, 24, 8) > 0.05);
        assert_eq!((at(hi, 24, 24), at(sh, 24, 24)), (0.0, 0.0));
        // Lit from above, the top of the ring is lit most by the shape's edge, where the shape's
        // own slope is steepest (Photoshop corpus bevel-stroke-smooth: 0.87 at the ring's outer
        // edge → 0.91 by the shape), and the ring past the size isn't shaded.
        assert!(at(hi, 24, 11) > at(hi, 24, 8) && at(hi, 24, 8) > 0.0, "{:?}", (5..12).map(|y| at(hi, 24, y)).collect::<Vec<_>>());
        assert!(at(sh, 24, 36) > at(sh, 24, 39) && at(sh, 24, 39) > 0.0, "shaded below, most by the shape: {:?}", (36..43).map(|y| at(sh, 24, y)).collect::<Vec<_>>());
        // No stroke, no emboss.
        layer.effects.items = vec![emboss];
        let maps = build_maps(&layer, shape.v.clone(), Rect::new(0, 0, n as i32, n as i32), &GlobalLight::default(), &no_tex());
        assert!(maps.per[0].is_empty());
    }

    /// Chisel Soft draws the same facets as Chisel Hard along straight edges (Reference corpus
    /// bevel-inner-chisel-hard / -soft: equal pixel for pixel across an edge); a blur used to
    /// soften them. Only the outermost row differs ([`chisel_soft_edge`]).
    #[test]
    fn chisel_soft_matches_hard_on_straight_edges() {
        let shape = square(64, 10, 54);
        let maps = |t: BevelTechnique| {
            let mut b = bevel_of(BevelStyle::InnerBevel, t);
            (b.size, b.angle, b.altitude) = (10.0, 120.0, 30.0);
            bevel_maps(&shape, &b, &GlobalLight::default(), &no_tex(), &PreparedPatterns::new(&[], PREPARED_PATTERN_BYTES)).0
        };
        let (hard, soft) = (maps(BevelTechnique::ChiselHard), maps(BevelTechnique::ChiselSoft));
        // The shape's outermost row and column (x or y at 10 or 53) take the steeper edge.
        let edge = |i: usize| [i % 64, i / 64].iter().any(|v| *v == 10 || *v == 53);
        for (h, s) in hard.iter().zip(&soft) {
            for (i, (a, b)) in h.v.iter().zip(&s.v).enumerate() {
                if !edge(i) {
                    assert_eq!(a, b, "pixel {i}");
                }
            }
        }
    }

    /// An anti-aliased disc: a chiselled bevel's shading follows the curve smoothly. The pixel
    /// staircase of a binary distance transform streaked it (neighbours along the ring differing
    /// by more than half the shading range).
    #[test]
    fn chisel_on_a_curve_has_no_streaks() {
        let n = 96;
        let mut shape = Map::new(n, n, 0.0);
        let (c, r) = (48.0f32, 36.0f32);
        for y in 0..n {
            for x in 0..n {
                // 4 × 4 supersampled coverage.
                let mut cov = 0.0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let (px, py) = (x as f32 + (sx as f32 + 0.5) / 4.0, y as f32 + (sy as f32 + 0.5) / 4.0);
                        cov += f32::from(((px - c).powi(2) + (py - c).powi(2)).sqrt() < r) / 16.0;
                    }
                }
                shape.v[y * n + x] = cov;
            }
        }
        let mut b = bevel_of(BevelStyle::InnerBevel, BevelTechnique::ChiselHard);
        b.size = 10.0;
        let (m, _) = bevel_maps(&shape, &b, &GlobalLight::default(), &no_tex(), &PreparedPatterns::new(&[], PREPARED_PATTERN_BYTES));
        // Walk the middle of the facet (radius r − 5) around the circle.
        let shade = |t: f32| {
            let (x, y) = ((c + (r - 5.0) * t.cos()) as usize, (c + (r - 5.0) * t.sin()) as usize);
            m[0].v[y * n + x] - m[1].v[y * n + x]
        };
        let steps = 720;
        let worst = (0..steps)
            .map(|k| {
                let t = k as f32 / steps as f32 * std::f32::consts::TAU;
                (shade(t) - shade(t + std::f32::consts::TAU / steps as f32)).abs()
            })
            .fold(0.0f32, f32::max);
        assert!(worst < 0.12, "largest jump between neighbours on the ring: {worst}");
    }

    /// A straight edge whose last column is 43 % covered: the emboss shades on across it at the
    /// facet's own value. Taking the partly covered column as fully inside broke the height ramp
    /// there, and the first outside pixel shaded a quarter darker: a dotted line along every
    /// anti-aliased edge (Photoshop kit, file 01).
    #[test]
    fn chisel_edge_pixels_continue_the_facet() {
        let n = 64;
        let mut shape = square(n, 10, 40);
        for y in 10..40 {
            shape.v[y * n + 40] = 0.43;
        }
        let mut b = bevel_of(BevelStyle::Emboss, BevelTechnique::ChiselHard);
        (b.size, b.angle) = (13.0, 180.0);
        let (m, _) = bevel_maps(&shape, &b, &GlobalLight::default(), &no_tex(), &PreparedPatterns::new(&[], PREPARED_PATTERN_BYTES));
        // Light from the left: the right-hand facets are in shadow, inside and outside alike.
        let sh = |x: usize| m[1].v[25 * n + x];
        assert!(sh(42) > 0.2);
        for x in [41, 43, 44] {
            assert!((sh(x) - sh(42)).abs() < 0.02, "x {x}: {} vs {}", sh(x), sh(42));
        }
    }

    #[test]
    fn bevel_contour_and_texture_shape_the_height() {
        let shape = square(40, 6, 34);
        let l = GlobalLight::default();
        let plain = bevel_of(BevelStyle::InnerBevel, BevelTechnique::Smooth);
        let (a, _) = bevel_maps(&shape, &plain, &l, &no_tex(), &PreparedPatterns::new(&[], PREPARED_PATTERN_BYTES));
        // A contour reshapes the profile (here a ramp folded at its middle).
        let mut c = plain.clone();
        let fold = vec![
            photosplice_doc::adjust::CurvePoint { input: 0.0, output: 0.0 },
            photosplice_doc::adjust::CurvePoint { input: 0.5, output: 1.0 },
            photosplice_doc::adjust::CurvePoint { input: 1.0, output: 0.0 },
        ];
        c.contour = Some(photosplice_doc::BevelContour {
            contour: Contour::Custom { name: "fold".into(), points: fold, corners: vec![] },
            range: 1.0,
            anti_alias: false,
        });
        let (b, _) = bevel_maps(&shape, &c, &l, &no_tex(), &PreparedPatterns::new(&[], PREPARED_PATTERN_BYTES));
        let diff: f32 = a[1].v.iter().zip(&b[1].v).map(|(x, y)| (x - y).abs()).sum();
        assert!(diff > 5.0, "{diff}");
        // A striped texture lights the flat middle.
        let mut sf = photosplice_raster::Surface::new(photosplice_color::PixelFormat::RGBA8);
        sf.fill_rect(Rect::new(0, 0, 4, 2), &[0.0, 0.0, 0.0, 1.0]);
        sf.fill_rect(Rect::new(0, 2, 4, 4), &[1.0, 1.0, 1.0, 1.0]);
        let pats = [Pattern::new("stripes", sf, 4, 4)];
        let mut t = plain.clone();
        t.texture = Some(photosplice_doc::BevelTexture {
            name: "stripes".into(),
            id: String::new(),
            scale: 1.0,
            depth: 1.0,
            invert: false,
            link: true,
            phase: (0.0, 0.0),
        });
        let tex = TextureCtx { rect: Rect::new(0, 0, 40, 40), patterns: &pats, anchor: (0.0, 0.0) };
        let (m, _) = bevel_maps(&shape, &t, &l, &tex, &PreparedPatterns::new(&pats, PREPARED_PATTERN_BYTES));
        let mid: f32 = (16..24).map(|y| m[0].v[y * 40 + 20] + m[1].v[y * 40 + 20]).sum();
        let flat: f32 = (16..24).map(|y| a[0].v[y * 40 + 20] + a[1].v[y * 40 + 20]).sum();
        assert!(mid > flat + 0.5, "{mid} vs {flat}");
    }

    /// Gloss Contour › Anti-aliased reads the shading a third of a pixel away (Reference corpus
    /// bevel-inner-gloss-double-ring-aa); with a linear contour it changes nothing, and either
    /// way such a layer is left to the CPU.
    #[test]
    fn gloss_anti_alias_reads_the_shading_a_third_of_a_pixel_away() {
        let shape = square(48, 8, 40);
        let l = GlobalLight::default();
        let pp = PreparedPatterns::new(&[], PREPARED_PATTERN_BYTES);
        let ring = Contour::Custom {
            name: "double ring".into(),
            points: [(0.0, 0.0), (0.25, 1.0), (0.5, 0.16), (0.75, 1.0), (1.0, 0.0)].iter().map(|(i, o)| photosplice_doc::adjust::CurvePoint { input: *i, output: *o }).collect(),
            corners: Vec::new(),
        };
        let mut b = bevel_of(BevelStyle::InnerBevel, BevelTechnique::Smooth);
        b.size = 12.0;
        for (contour, differs) in [(ring, true), (Contour::Linear, false)] {
            b.gloss_contour = contour;
            b.gloss_anti_alias = false;
            let (plain, _) = bevel_maps(&shape, &b, &l, &no_tex(), &pp);
            b.gloss_anti_alias = true;
            let (aa, _) = bevel_maps(&shape, &b, &l, &no_tex(), &pp);
            let most = plain[0].v.iter().zip(&aa[0].v).map(|(a, c)| (a - c).abs()).fold(0.0f32, f32::max);
            assert_eq!(most > 0.01, differs, "{most}");
            assert_eq!(has_anti_aliased_contour(&Effect::BevelEmboss(b.clone())), differs);
        }
    }

    /// The Contour's Range spans each style's own profile: a Smooth inner bevel's from ½ at the
    /// edge to 1, a Chisel or emboss profile's from 0 to 1 (Reference corpus
    /// bevel-pillow-chisel-hard-gloss-and-contour). Below the foot (a Smooth bevel's corners) the
    /// contour goes on at its starting slope (bevel-inner-contour-ramp-peak: the corner wedge was
    /// left unshaded, 127/255 off).
    #[test]
    fn bevel_contour_range_follows_the_style_profile() {
        let linear = photosplice_doc::BevelContour { contour: Contour::Linear, range: 0.5, anti_alias: false };
        let lut_at = |l: &[f32], h: f32| l[(h * (l.len() - 1) as f32).round() as usize];
        let inner = bevel_contour_lut(&linear, &bevel_of(BevelStyle::InnerBevel, BevelTechnique::Smooth));
        for (h, v) in [(0.5, 0.0), (0.75, 0.5), (1.0, 1.0), (0.25, -0.5)] {
            assert!((lut_at(&inner, h) - v).abs() < 1e-3, "smooth inner at {h}: {}", lut_at(&inner, h));
        }
        for style in [BevelStyle::PillowEmboss, BevelStyle::Emboss] {
            let l = bevel_contour_lut(&linear, &bevel_of(style, BevelTechnique::ChiselHard));
            assert!([0.0, 0.3, 0.7, 1.0].iter().all(|h| (lut_at(&l, *h) - h).abs() < 1e-3), "{style:?}");
        }
        let outer = bevel_contour_lut(&linear, &bevel_of(BevelStyle::OuterBevel, BevelTechnique::Smooth));
        assert!((lut_at(&outer, 0.25) - 0.5).abs() < 1e-3 && (lut_at(&outer, 0.5) - 1.0).abs() < 1e-3);
    }

    /// Photoshop corpus bevel-inner-contour-ramp-peak / bevel-inner-gloss-ramp-peak (inner smooth,
    /// size 14, angle 120°, altitude 30°), across the square's left edge: with a ramp-peak
    /// Contour at Range 50 % the bevel is lit for ~4 px, then in full shadow to ~12 px, flat
    /// past the size; with the ramp peak as Gloss Contour the flat interior is full highlight.
    #[test]
    fn bevel_contour_range_and_gloss_contour_match_photoshop() {
        let shape = square(64, 8, 56);
        let l = GlobalLight::default();
        let peak = Contour::Custom {
            name: "ramp peak".into(),
            points: vec![
                photosplice_doc::adjust::CurvePoint { input: 0.0, output: 0.0 },
                photosplice_doc::adjust::CurvePoint { input: 128.0 / 255.0, output: 1.0 },
                photosplice_doc::adjust::CurvePoint { input: 1.0, output: 0.0 },
            ],
            corners: Vec::new(),
        };
        let mut b = bevel_of(BevelStyle::InnerBevel, BevelTechnique::Smooth);
        (b.size, b.angle) = (14.0, 120.0);
        let mut c = b.clone();
        c.contour = Some(photosplice_doc::BevelContour { contour: peak.clone(), range: 0.5, anti_alias: false });
        let (m, _) = bevel_maps(&shape, &c, &l, &no_tex(), &PreparedPatterns::new(&[], PREPARED_PATTERN_BYTES));
        let at = |map: &Map, d: usize| map.v[32 * 64 + 8 + d];
        // Photoshop's highlight amounts at 1-3 px: (0.38, 0.42, 0.51 − 0.35 fill) / (0.65 × 75 %).
        for (d, ps) in [(1, 0.06), (2, 0.14), (3, 0.33)] {
            assert!((at(&m[0], d) - ps).abs() < 0.05 && at(&m[1], d) == 0.0, "lit by the edge: {:?}", (0..16).map(|d| at(&m[0], d)).collect::<Vec<_>>());
        }
        assert!((6..=10).all(|d| at(&m[1], d) > 0.9), "then full shadow: {:?}", (0..16).map(|d| at(&m[1], d)).collect::<Vec<_>>());
        assert!((16..24).all(|d| at(&m[0], d) + at(&m[1], d) < 1e-3), "flat past the size");
        let mut g = b.clone();
        g.gloss_contour = peak;
        let (m, _) = bevel_maps(&shape, &g, &l, &no_tex(), &PreparedPatterns::new(&[], PREPARED_PATTERN_BYTES));
        assert!((at(&m[0], 24) - 1.0).abs() < 1e-3 && at(&m[1], 24) == 0.0, "flat interior full highlight");
        // A linear gloss contour leaves the shading alone.
        let near = |(h, s): (f32, f32), (eh, es): (f32, f32)| (h - eh).abs() < 1e-5 && (s - es).abs() < 1e-5;
        assert!(near(gloss(0.2, 0.5, None), (0.4, 0.0)) && near(gloss(-0.25, 0.5, None), (0.0, 0.5)));
    }

    #[test]
    fn glow_range_stretches_the_contour() {
        let mut g = Glow {
            common: photosplice_doc::FxCommon::new(BlendMode::Screen, 1.0),
            paint: FxPaint::Color(photosplice_color::Color::WHITE),
            technique: GlowTechnique::Softer,
            spread: 0.0,
            size: 10.0,
            contour: Contour::Linear,
            anti_alias: false,
            range: 0.5,
            jitter: 0.0,
            noise: 0.0,
            source: GlowSource::Edge,
        };
        let l = glow_lut(&g).unwrap();
        assert!((lut_at(&l, 0.25) - 0.5).abs() < 1e-3 && (lut_at(&l, 0.75) - 1.0).abs() < 1e-6);
        g.range = 1.0;
        assert!(glow_lut(&g).is_none());
    }

    /// Photoshop 27.11, measured on a 128 px square (Normal, black, 100 %, Range 50 %): a precise
    /// glow's coverage across the edge, and a centre-source inner glow as 1 − the edge glow.
    #[test]
    fn glows_match_photoshop() {
        let glow = |technique, size: f32, spread: f32, source| Glow {
            common: photosplice_doc::FxCommon::new(BlendMode::Normal, 1.0),
            paint: FxPaint::Color(photosplice_color::Color::BLACK),
            technique,
            spread,
            size,
            contour: Contour::Linear,
            anti_alias: false,
            range: 0.5,
            jitter: 0.0,
            noise: 0.0,
            source,
        };
        let n = 60;
        let mut shape = Map::new(n, n, 0.0);
        for y in 20..40 {
            for x in 20..40 {
                shape.v[y * n + x] = 1.0;
            }
        }
        let at = |m: &Map, x: usize| m.v[30 * n + x];
        // Outer, precise, size 10: Photoshop's black glow over white reads 47, 93, 140, 186, 232
        // at 2, 4, 6, 8 and 10 px out from the edge pixel (alpha 1 − D / 11).
        let m = glow_map(&shape, &glow(GlowTechnique::Precise, 10.0, 0.0, GlowSource::Edge), false, (0, 0));
        for (d, ps) in [(2, 47.0f32), (4, 93.0), (6, 140.0), (8, 186.0), (10, 232.0)] {
            let got = 255.0 * (1.0 - at(&m, 20 - d));
            assert!((got - ps).abs() <= 1.5, "outer precise at {d} px: {got} vs Photoshop {ps}");
        }
        // Spread 40 %: solid for the first 4 px.
        let m = glow_map(&shape, &glow(GlowTechnique::Precise, 10.0, 0.4, GlowSource::Edge), false, (0, 0));
        assert!((at(&m, 16) - 1.0).abs() < 1e-6 && at(&m, 14) < 1.0);
        // Centre source = 1 − edge, for both techniques.
        for t in [GlowTechnique::Precise, GlowTechnique::Softer] {
            let e = glow_map(&shape, &glow(t, 6.0, 0.0, GlowSource::Edge), true, (0, 0));
            let c = glow_map(&shape, &glow(t, 6.0, 0.0, GlowSource::Center), true, (0, 0));
            for x in 20..40 {
                assert!((at(&c, x) - (1.0 - at(&e, x))).abs() < 1e-5, "{t:?} at {x}");
            }
        }
    }

    #[test]
    fn offsets_follow_light() {
        let (dx, dy) = offset(120.0, 10.0);
        assert_eq!((dx, dy), (5.0, 9.0));
    }

    fn square_shape(n: usize) -> Map {
        let mut m = Map::new(n, n, 0.0);
        for y in 5..n - 5 {
            for x in 5..n - 5 {
                m.v[y * n + x] = 1.0;
            }
        }
        m
    }

    #[test]
    fn satin_at_distance_zero_matches_photoshop() {
        // Photoshop 27.11, Satin black 100 %, angle 90°, Linear contour, distance 0, size 5 over a
        // #808080 square: the rows from the top edge read 82, 102, 123, 128.
        let s = photosplice_doc::effects::Satin {
            common: photosplice_doc::FxCommon::new(BlendMode::Normal, 1.0),
            color: photosplice_color::Color::BLACK,
            angle: 90.0,
            distance: 0.0,
            size: 5.0,
            contour: Contour::Linear,
            anti_alias: false,
            invert: false,
        };
        let m = satin_map(&square_shape(60), &s);
        for (row, gray) in [(5, 82.0), (7, 102.0), (9, 123.0), (11, 128.0), (30, 128.0)] {
            let want = 1.0 - gray / 128.0;
            let got = m.v[row * 60 + 30];
            assert!((got - want).abs() < 0.02, "row {row}: {got} vs {want}");
        }
    }

    fn shadow(noise: f32, origin: (i32, i32)) -> Map {
        let s = Shadow {
            common: photosplice_doc::FxCommon::new(BlendMode::Multiply, 1.0),
            color: photosplice_color::Color::BLACK,
            angle: 0.0,
            use_global_light: false,
            distance: 0.0,
            spread: 0.0,
            size: 6.0,
            contour: Contour::Linear,
            anti_alias: false,
            noise,
            knocks_out: true,
        };
        shadow_map(&square_shape(40), &s, &GlobalLight::default(), false, origin)
    }

    #[test]
    fn shadow_noise_speckles_soft_coverage_deterministically() {
        let plain = shadow(0.0, (0, 0));
        let noisy = shadow(0.5, (0, 0));
        let again = shadow(0.5, (0, 0));
        let (mut diff, mut identical) = (0.0f32, true);
        for i in 0..plain.v.len() {
            diff += (plain.v[i] - noisy.v[i]).abs();
            if noisy.v[i] != again.v[i] {
                identical = false;
            }
        }
        assert!(diff > 5.0, "noise changes the soft band: {diff}");
        assert!(identical, "same coords, same speckle");
        // Every value stays a coverage.
        assert!(noisy.v.iter().all(|v| (0.0..=1.0).contains(v)));
        // The speckle moves with the document origin, not the tile.
        let moved = shadow(0.5, (100, 100));
        assert_ne!(noisy.v, moved.v, "hashed from document coordinates");
        // Full noise can punch holes even in the solid part.
        let heavy = shadow(1.0, (0, 0));
        assert!(heavy.v.iter().any(|v| *v < 0.5) && heavy.v.iter().any(|v| *v > 0.5), "heavy noise spans the range",);
    }

    #[test]
    fn glow_noise_follows_the_contour_and_range() {
        let g = |noise: f32| Glow {
            common: photosplice_doc::FxCommon::new(BlendMode::Screen, 1.0),
            paint: FxPaint::Color(photosplice_color::Color::WHITE),
            technique: GlowTechnique::Softer,
            spread: 0.0,
            size: 6.0,
            contour: Contour::Linear,
            anti_alias: false,
            range: 0.5,
            jitter: 0.0,
            noise,
            source: GlowSource::Edge,
        };
        let plain = glow_map(&square_shape(40), &g(0.0), false, (0, 0));
        let noisy = glow_map(&square_shape(40), &g(0.6), false, (0, 0));
        let diff: f32 = plain.v.iter().zip(&noisy.v).map(|(a, b)| (a - b).abs()).sum();
        assert!(diff > 5.0, "glow noise changes the coverage: {diff}");
    }

    /// A Precise outer glow, size 16, spread 40 %, by a straight edge: full strength to 6 px
    /// (pixel centre to pixel centre), then (17 − d) / 11 (Reference corpus
    /// outer-glow-precise-spread40.psd, exact along its straight edges).
    #[test]
    fn precise_glow_profile_matches_reference() {
        let g = Glow {
            common: photosplice_doc::FxCommon::new(BlendMode::Screen, 1.0),
            paint: FxPaint::Color(photosplice_color::Color::WHITE),
            technique: GlowTechnique::Precise,
            spread: 0.4,
            size: 16.0,
            contour: Contour::Linear,
            anti_alias: false,
            range: 0.5,
            jitter: 0.0,
            noise: 0.0,
            source: GlowSource::Edge,
        };
        let n = 60;
        let mut shape = Map::new(n, n, 0.0);
        for y in 0..n {
            for x in 30..n {
                shape.v[y * n + x] = 1.0;
            }
        }
        let m = glow_map(&shape, &g, false, (0, 0));
        let at = |x: usize| m.v[30 * n + x];
        for d in 1..=6 {
            assert!((at(30 - d) - 1.0).abs() < 1e-4, "{d}: {}", at(30 - d));
        }
        for d in 7..=18 {
            let want = ((17.0 - d as f32) / 11.0).clamp(0.0, 1.0);
            assert!((at(30 - d) - want).abs() < 1e-3, "{d}: {} vs {want}", at(30 - d));
        }
    }

    #[test]
    fn shifting_by_hostile_distances_does_not_overflow() {
        let m = square_shape(12);
        for d in [1.0e30, -1.0e30, f32::INFINITY, f32::NAN] {
            let s = m.shifted(d, d, 0.0);
            assert_eq!(s.v.len(), 144);
        }
    }

    /// An anti-aliased contour reads the coverage a third of a pixel down and right, so the glow
    /// sits up and left of the plain one (Reference, the effects kit's square at x 40..80: an
    /// Outer Glow, size 24, range 60 %, a linear custom contour; 2 px out on either side).
    #[test]
    fn anti_aliased_contour_glow_matches_reference() {
        let (w, h) = (256usize, 128usize);
        let mut shape = Map::new(w, h, 0.0);
        for y in 44..84 {
            for x in 40..80 {
                shape.v[y * w + x] = 1.0;
            }
        }
        let linear = Contour::Custom { name: "kit".into(), points: vec![photosplice_doc::adjust::CurvePoint { input: 0.0, output: 0.0 }, photosplice_doc::adjust::CurvePoint { input: 1.0, output: 1.0 }], corners: Vec::new() };
        let g = |anti_alias: bool| Glow {
            common: photosplice_doc::FxCommon::new(BlendMode::Normal, 1.0),
            paint: FxPaint::Color(photosplice_color::Color::WHITE),
            technique: GlowTechnique::Softer,
            spread: 0.0,
            size: 24.0,
            contour: linear.clone(),
            anti_alias,
            range: 0.6,
            jitter: 0.0,
            noise: 0.0,
            source: GlowSource::Edge,
        };
        let aa = glow_map(&shape, &g(true), false, (0, 0));
        let plain = glow_map(&shape, &g(false), false, (0, 0));
        let at = |m: &Map, x: usize| m.v[64 * w + x];
        assert!((at(&aa, 38) - 0.7333).abs() < 0.005, "{}", at(&aa, 38));
        assert!((at(&aa, 81) - 0.6902).abs() < 0.005, "{}", at(&aa, 81));
        // The plain glow is symmetric about the square.
        assert!((at(&plain, 38) - at(&plain, 81)).abs() < 1e-4);
    }

    #[test]
    fn linear_dodge_and_burn_strength_acts_as_fill() {
        // psd-tools layer_comps: a 15 % Linear Dodge highlight and a 70 % Linear Burn shadow.
        let m = Map::new(2, 1, 1.0);
        let mut dst = Buffer { rect: Rect::new(0, 0, 2, 1), px: vec![[0.9, 0.9, 0.2, 1.0]; 2] };
        paint_color(&mut dst, &m, [0.984, 0.979, 0.537], BlendMode::LinearDodge, 0.15);
        // base + 15 % of the colour, clipped (not 15 % of the way to the clipped sum: 0.915).
        assert!((dst.px[0][0] - 1.0).abs() < 1e-5, "{:?}", dst.px[0]);
        assert!((dst.px[0][2] - (0.2 + 0.15 * 0.537)).abs() < 1e-5, "{:?}", dst.px[0]);
        paint_color(&mut dst, &m, [0.957, 0.823, 0.083], BlendMode::LinearBurn, 0.7);
        // base − 70 % of the colour's distance from white, clipped at 0.
        assert!(dst.px[1][2].abs() < 1e-5, "{:?}", dst.px[1]);
        assert!((dst.px[1][0] - (1.0 - 0.7 * (1.0 - 0.957))).abs() < 1e-5, "{:?}", dst.px[1]);
        // Normal keeps mixing by strength.
        let mut n = Buffer { rect: Rect::new(0, 0, 2, 1), px: vec![[0.0, 0.0, 0.0, 1.0]; 2] };
        paint_color(&mut n, &m, [1.0, 1.0, 1.0], BlendMode::Normal, 0.25);
        assert!((n.px[0][0] - 0.25).abs() < 1e-5);
    }

    #[test]
    fn knocked_out_shadow_stays_out_from_under_an_outside_stroke() {
        // psd-tools masks.psd: a 10 % outside stroke over a drop shadow shows the bare backdrop.
        use photosplice_doc::{Document, Shadow};
        let doc = Document::new("d", photosplice_geom::Size::new(40, 40), photosplice_color::ColorMode::Rgb, photosplice_color::SampleType::U8);
        let fmt = doc.pixel_format();
        let mut bg = Layer::raster("bg", fmt);
        bg.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 40, 40), &[1.0, 1.0, 1.0, 1.0]);
        let mut l = Layer::raster("l", fmt);
        l.surface_mut().unwrap().fill_rect(Rect::new(10, 10, 30, 30), &[0.0, 0.0, 1.0, 1.0]);
        let shadow = |knocks_out| {
            Effect::DropShadow(Shadow {
                common: photosplice_doc::FxCommon::new(BlendMode::Normal, 1.0),
                color: photosplice_doc::Color::BLACK,
                angle: 90.0,
                use_global_light: false,
                distance: 4.0,
                spread: 0.0,
                size: 0.0,
                contour: Contour::Linear,
                anti_alias: false,
                noise: 0.0,
                knocks_out,
            })
        };
        let stroke = Effect::Stroke(photosplice_doc::StrokeFx {
            common: photosplice_doc::FxCommon::new(BlendMode::Normal, 0.1),
            size: 2.0,
            position: StrokePosition::Outside,
            paint: FxPaint::Color(photosplice_doc::Color::rgb(1.0, 0.0, 0.0)),
        });
        let at = |knocks_out: bool| {
            let mut d = doc.clone();
            let mut l = l.clone();
            l.effects.items = vec![stroke.clone(), shadow(knocks_out)];
            d.layers = vec![bg.clone(), l];
            crate::flatten(&d).px[31 * 40 + 20]
        };
        // Below the square, in the stroke's band and the shadow (cast 4 px down).
        let out = at(true);
        assert!((out[0] - 1.0).abs() < 0.01 && (out[1] - 0.9).abs() < 0.01, "{out:?}");
        let kept = at(false);
        assert!(kept[1] < 0.05, "without knockout the shadow shows: {kept:?}");
    }

    /// Distances to an anti-aliased disc's edge, from its coverage alone: within 0.1 px of the
    /// true radius everywhere inside, where the nearest-pixel distance was 0.15 off (the facets of
    /// a chisel bevel follow them).
    #[test]
    fn aa_distances_follow_an_anti_aliased_edge() {
        let (n, c, r) = (80usize, 40.0f32, 27.3f32);
        let mut s = Map::new(n, n, 0.0);
        for y in 0..n {
            for x in 0..n {
                let mut k = 0;
                for sy in 0..8 {
                    for sx in 0..8 {
                        let (px, py) = (x as f32 + (sx as f32 + 0.5) / 8.0, y as f32 + (sy as f32 + 0.5) / 8.0);
                        k += usize::from((px - c).hypot(py - c) <= r);
                    }
                }
                s.v[y * n + x] = k as f32 / 64.0;
            }
        }
        let d = dist_inside(&s);
        let mut worst = 0.0f32;
        for y in 0..n {
            for x in 0..n {
                let i = y * n + x;
                if s.v[i] >= 1.0 {
                    let want = r - (x as f32 + 0.5 - c).hypot(y as f32 + 0.5 - c);
                    worst = worst.max((d[i] - want).abs());
                }
            }
        }
        let e = dist_inside_by(&s, Metric::Euclidean);
        let old = (0..n * n).filter(|&i| s.v[i] >= 1.0).map(|i| (e[i] - (r - ((i % n) as f32 + 0.5 - c).hypot((i / n) as f32 + 0.5 - c))).abs()).fold(0.0f32, f32::max);
        assert!(worst < 0.1 && worst < old * 0.7, "{worst} (nearest pixel: {old})");
    }

    /// An outer bevel lies beneath the layer like a glow: under a see-through fill a half-covered
    /// edge pixel shows half of it (Reference kit hard-outer-s20 at Fill 0 %).
    #[test]
    fn outer_bevel_is_knocked_out_under_a_see_through_fill() {
        use photosplice_doc::Document;
        let doc = Document::new("d", photosplice_geom::Size::new(40, 40), photosplice_color::ColorMode::Rgb, photosplice_color::SampleType::U8);
        let fmt = doc.pixel_format();
        let mut bg = Layer::raster("bg", fmt);
        bg.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 40, 40), &[0.0, 0.0, 0.0, 1.0]);
        let mut l = Layer::raster("l", fmt);
        l.surface_mut().unwrap().fill_rect(Rect::new(12, 10, 30, 30), &[0.5, 0.5, 0.5, 1.0]);
        // The left edge column half covered.
        l.surface_mut().unwrap().fill_rect(Rect::new(12, 10, 13, 30), &[0.5, 0.5, 0.5, 0.5]);
        let mut b = bevel_of(BevelStyle::OuterBevel, BevelTechnique::ChiselHard);
        b.angle = 180.0;
        b.highlight = photosplice_doc::FxCommon::new(BlendMode::Normal, 1.0);
        b.shadow = photosplice_doc::FxCommon::new(BlendMode::Normal, 0.0);
        l.effects.items = vec![Effect::BevelEmboss(b)];
        l.fill_opacity = 0.0;
        let mut d = doc.clone();
        d.layers = vec![bg, l];
        let px = crate::flatten(&d).px;
        // The highlight the bevel's map gives that pixel, before the layer knocks it out.
        let mut shape = Map::new(40, 40, 0.0);
        for y in 10..30 {
            for x in 12..30 {
                shape.v[y * 40 + x] = if x == 12 { 0.5 } else { 1.0 };
            }
        }
        let Effect::BevelEmboss(b) = &d.layers[1].effects.items[0] else { panic!("a bevel") };
        let (maps, _) = bevel_maps(&shape, b, &GlobalLight::default(), &no_tex(), &PreparedPatterns::new(&[], PREPARED_PATTERN_BYTES));
        let lit = maps[0].v[20 * 40 + 12];
        assert!(lit > 0.2, "the edge pixel is on the lit facet: {lit}");
        assert!((px[20 * 40 + 12][0] - 0.5 * lit).abs() < 0.01, "{} vs half of {lit}", px[20 * 40 + 12][0]);
    }

    /// Chisel Soft's inner bevel drops a step further at the edge: lit from straight above, its
    /// outermost row slopes 1.5 × the facet (Reference kit soft-inner-s10 / -s20, alt 90°: 0.679
    /// against 0.445), the rows inside as Chisel Hard's.
    #[test]
    fn chisel_soft_inner_edge_row_is_steeper() {
        let n = 48;
        let shape = square(n, 10, 38);
        let slope = |t: BevelTechnique, y: usize| {
            let mut b = bevel_of(BevelStyle::InnerBevel, t);
            (b.size, b.altitude) = (10.0, 90.0);
            let (maps, _) = bevel_maps(&shape, &b, &GlobalLight::default(), &no_tex(), &PreparedPatterns::new(&[], PREPARED_PATTERN_BYTES));
            let shade = 1.0 - maps[1].v[y * n + 24];
            (1.0 / (shade * shade) - 1.0).max(0.0).sqrt()
        };
        let (soft, hard) = (slope(BevelTechnique::ChiselSoft, 10), slope(BevelTechnique::ChiselHard, 10));
        assert!((soft / hard - 1.5).abs() < 0.05, "edge row: soft {soft} hard {hard}");
        let (soft, hard) = (slope(BevelTechnique::ChiselSoft, 14), slope(BevelTechnique::ChiselHard, 14));
        assert!((soft - hard).abs() < 1e-4, "inside: soft {soft} hard {hard}");
    }
}
