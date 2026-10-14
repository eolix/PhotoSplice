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
    let cell = px8::rhu(c * 2.2).max(1.0);
    let kappa = 1.0 / cell;
    let nc = (w as f64 * kappa).floor() as usize + 1;
    let nr = (h as f64 * kappa).floor() as usize + 1;
    let mut rnd = px8::Mulberry32::new(e.seed());
    let mut sites = Vec::with_capacity(nc * nr);
    for r in 0..nr {
        for q in 0..nc {
            let sx = (q as f64 + 0.5 * rnd.next() + 0.5 * (r & 1) as f64) * cell;
            let sy = (r as f64 + 0.5 * rnd.next()) * cell;
            let p = ((sy.floor() as usize).min(h - 1) * w + (sx.floor() as usize).min(w - 1)) * 4;
            for _ in 0..4 {
                rnd.next();
            }
            sites.push((sx, sy, [img.d[p] as f64, img.d[p + 1] as f64, img.d[p + 2] as f64]));
        }
    }
    let total = sites.len() as i64;
    // Light Intensity: a light through the middle of the image that raises the contrast and
    // darkens the rim (the spec ignored it).
    let li = if px8::defects() { 0.0 } else { param(e, "lightIntensity", 0.0, 10.0) };
    let (cx, cy) = (w as f64 * 0.5, h as f64 * 0.5);
    let half_diag = (cx * cx + cy * cy).sqrt().max(1.0);
    // (gain, offset) of the light at a point.
    let light = |px: f64, py: f64| -> (f64, f64) {
        if li == 0.0 {
            return (1.0, 0.0);
        }
        let r2 = ((px - cx).powi(2) + (py - cy).powi(2)) / (half_diag * half_diag);
        let core = (-r2 / (LIGHT_CORE * LIGHT_CORE)).exp();
        let pool = (-(r2 / (LIGHT_POOL * LIGHT_POOL)).powi(2)).exp();
        (1.0 + li * (LIGHT_CONTRAST + LIGHT_GAIN * core), li * (LIGHT_ADD * pool - LIGHT_RIM))
    };
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
                for dr in -1..=1i64 {
                    for dc in -1..=1i64 {
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
            let mut wt = beta - 0.5 * margin + if px8::defects() { 0.0 } else { LEAD_THIN };
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
            let (lg, lo) = light(px, py);
            for ch in 0..3 {
                let c = (col[ch] * lg + lo).clamp(0.0, 255.0);
                row[x * 4 + ch] = px8::clamp255((0.5 + wt * c + (1.0 - wt) * fill[ch]).floor());
            }
        }
    });
    out
}

/// Per unit of Light Intensity, fitted on the reference's cells (source-to-output levels by
/// distance from the centre): a contrast gain everywhere plus `LIGHT_GAIN` in a Gaussian core of
/// radius `LIGHT_CORE`, light added over a flat-topped pool (`exp(−(r/LIGHT_POOL)⁴)`) and taken
/// everywhere, so dark glass goes black towards the rim. `r` is relative to the half-diagonal.
const LIGHT_CONTRAST: f64 = 0.05;
const LIGHT_GAIN: f64 = 0.08;
const LIGHT_ADD: f64 = 19.0;
const LIGHT_RIM: f64 = 8.5;
const LIGHT_CORE: f64 = 0.35;
const LIGHT_POOL: f64 = 0.45;
/// How much thinner the reference's leading is on each side of a bisector, in pixels.
const LEAD_THIN: f64 = 0.15;
/// How far into the frame its soft inner pixel starts.
const FRAME_SOFT: f64 = 0.8;

/// Distance from `(px, py)` to the perpendicular bisector of sites `a` and `b`.
fn bisector(sites: &[(f64, f64, [f64; 3])], a: usize, b: usize, px: f64, py: f64) -> f64 {
    let ((ax, ay, _), (bx, by, _)) = (sites[a], sites[b]);
    let (mx, my) = ((ax + bx) * 0.5, (ay + by) * 0.5);
    let (qx, qy) = (mx + (by - ay), my - (bx - ax));
    let (lx, ly) = (qx - mx, qy - my);
    (ly * px - lx * py + qx * my - qy * mx).abs() / (lx * lx + ly * ly).sqrt()
}
