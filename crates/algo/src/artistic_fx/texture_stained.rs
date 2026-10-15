//! Stained Glass: a jittered cell quantizer with leading along the cell bisectors.

use super::super::px8::{self, Img};
use super::param;
use crate::artistic::GalleryEffect;
use crate::photo_util::par_rows;

pub(super) fn stained_glass(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let c = param(e, "cellSize", 2.0, 50.0);
    let b = param(e, "borderThickness", 1.0, 20.0);
    let fill = px8::rgb8(e.foreground).map(|v| v as f64);
    let margin = b * 0.5;
    let cell = if px8::defects() { px8::rhu(c * 2.2).max(1.0) } else { cell_size(c) };
    let kappa = 1.0 / cell;
    let nc = (w as f64 * kappa).floor() as usize + 1;
    let nr = (h as f64 * kappa).floor() as usize + 1;
    let mut rnd = px8::Mulberry32::new(e.seed());
    let mut sites = Vec::with_capacity(nc * nr);
    // The reference's cells are less regular than the spec's (a wider spread of areas and
    // neighbour distances).
    let jit = if px8::defects() { 0.5 } else { SITE_JITTER };
    // Jitter past half a cell can bring a site from two columns or rows back.
    let reach = if jit > 0.5 { -2 } else { -1 };
    for r in 0..nr {
        for q in 0..nc {
            let sx = (q as f64 + jit * rnd.next() + 0.5 * (r & 1) as f64) * cell;
            let sy = (r as f64 + jit * rnd.next()) * cell;
            let p = ((sy.floor() as usize).min(h - 1) * w + (sx.floor() as usize).min(w - 1)) * 4;
            for _ in 0..4 {
                rnd.next();
            }
            sites.push((sx, sy, [img.d[p] as f64, img.d[p + 1] as f64, img.d[p + 2] as f64]));
        }
    }
    let total = sites.len() as i64;
    // Light Intensity: white light added to the glass in a cone around the image centre (the
    // spec ignored it).
    let li = if px8::defects() { 0.0 } else { param(e, "lightIntensity", 0.0, 10.0) };
    let (cx, cy) = (w as f64 * 0.5, h as f64 * 0.5);
    let spot = (cx * cx + cy * cy).sqrt().max(1.0) * LIGHT_RADIUS;
    let (l1, l2) = (li * LIGHT_LINEAR, li * LIGHT_SQUARE);
    let light = |px: f64, py: f64| -> f64 {
        if li == 0.0 {
            return 0.0;
        }
        let cone = (1.0 - ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() / spot).max(0.0);
        cone * (l1 + l2 * cone)
    };
    let (gain, offset) = if px8::defects() { (1.0, 0.0) } else { (GLASS_GAIN, GLASS_OFFSET) };
    let mut out = img.clone();
    par_rows(&mut out.d, w, 4, |y, row| {
        let py = y as f64 + 0.5;
        let home_r = (py * kappa).floor() as i64;
        let (mut n1, mut n2, mut n3) = (0usize, 0usize, 0usize);
        let mut k = 0.0;
        for x in 0..w {
            let px = x as f64 + 0.5;
            if k > 1.0 + margin {
                k -= 1.0;
            } else {
                let home = home_r * nc as i64 + (px * kappa).floor() as i64;
                let (mut e1, mut e2, mut e3) = (1e9, 1e9, 1e9);
                (n1, n2, n3) = (0, 0, 0);
                for dr in reach..=1i64 {
                    for dc in reach..=1i64 {
                        let j = home + dr * nc as i64 + dc;
                        if j < 0 || j >= total {
                            continue;
                        }
                        let s = sites[j as usize];
                        let d = (s.0 - px).powi(2) + (s.1 - py).powi(2);
                        let j = j as usize;
                        if d < e1 {
                            (e3, n3, e2, n2, e1, n1) = (e2, n2, e1, n1, d, j);
                        } else if d < e2 {
                            (e3, n3, e2, n2) = (e2, n2, d, j);
                        } else if d < e3 {
                            (e3, n3) = (d, j);
                        }
                    }
                }
                let (d1, d2) = (e1.sqrt(), e2.sqrt());
                k = d2 - (d1 + d2) * 0.5;
            }
            let (b2, b3) = (bisector(&sites, n1, n2, px, py), bisector(&sites, n1, n3, px, py));
            let beta = if b2.is_nan() || b3.is_nan() { f64::NAN } else { b2.min(b3) };
            // The reference's leading: a solid core b − 2 px wide plus a soft pixel each side, so a
            // thickness of 1 gives a thin grey line, 4 the default 2- and 3-pixel cores and 20
            // leaves only specks of glass at the default cell size.
            let mut wt = if px8::defects() { beta - 0.5 * margin } else { beta - (margin - 1.0).max(0.0) };
            if !wt.is_nan() {
                wt = wt.clamp(0.0, 1.0);
            }
            if px8::defects() {
                if px < margin || w as f64 - margin < px || py < margin || h as f64 - margin < py {
                    wt = 0.0;
                }
            } else {
                // The reference's frame is a little thinner, with a soft inner pixel.
                let edge = px.min(w as f64 - px).min(py).min(h as f64 - py);
                let frame = (edge - margin + FRAME_SOFT).clamp(0.0, 1.0);
                wt = if wt.is_nan() { frame } else { wt.min(frame) };
            }
            let col = sites[n1].2;
            let lit = light(px, py);
            for ch in 0..3 {
                let c = if px8::defects() { col[ch] } else { ((col[ch] * gain + offset).clamp(0.0, 255.0) + lit).min(255.0) };
                row[x * 4 + ch] = px8::clamp255((0.5 + wt * c + (1.0 - wt) * fill[ch]).floor());
            }
        }
    });
    out
}

/// The reference's glass has more contrast than the source, at every Light Intensity.
const GLASS_GAIN: f64 = 1.156;
const GLASS_OFFSET: f64 = -14.0;
/// Light per unit of Light Intensity, the same on each channel: `cone·(LIGHT_LINEAR +
/// LIGHT_SQUARE·cone)`, `cone` falling from 1 at the centre to 0 at `LIGHT_RADIUS` half-diagonals.
/// Fitted on the reference's renders at intensities 0, 3 and 10, which share their cells: outside
/// the cone they are identical, and inside the glass gains the same levels whatever its colour
/// (the leading stays black).
const LIGHT_LINEAR: f64 = 29.7;
const LIGHT_SQUARE: f64 = -5.8;
const LIGHT_RADIUS: f64 = 0.5625;

/// The distance between cell sites for Cell Size `c`, fitted on the reference's cell counts: 6 px
/// at 2, 22 at the default 10, and growing much faster towards the top of the range (at 50 a
/// 1536 × 2048 image holds four cells).
fn cell_size(c: f64) -> f64 {
    (2.4 + 1.8 * c + 1.37e-4 * c.powi(4)).max(1.0)
}

/// How far each site is jittered, in cells (the spec's 0.5).
const SITE_JITTER: f64 = 0.6;
/// How far into the frame the reference's soft inner pixel starts.
const FRAME_SOFT: f64 = 0.9;

/// Distance from `(px, py)` to the perpendicular bisector of sites `a` and `b`.
fn bisector(sites: &[(f64, f64, [f64; 3])], a: usize, b: usize, px: f64, py: f64) -> f64 {
    let ((ax, ay, _), (bx, by, _)) = (sites[a], sites[b]);
    let (mx, my) = ((ax + bx) * 0.5, (ay + by) * 0.5);
    let (qx, qy) = (mx + (by - ay), my - (bx - ax));
    let (lx, ly) = (qx - mx, qy - my);
    (ly * px - lx * py + qx * my - qy * mx).abs() / (lx * lx + ly * ly).sqrt()
}
