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
    let peak = li * LIGHT_PEAK;
    let light = |px: f64, py: f64| -> f64 {
        if li == 0.0 {
            return 0.0;
        }
        let (u, v) = ((px - cx).abs(), (py - cy).abs());
        peak * (1.0 - (u.max(v) + LIGHT_SKEW * u.min(v)) / spot).max(0.0)
    };
    let (gain, offset) = if px8::defects() { (1.0, 0.0) } else { (GLASS_GAIN, GLASS_OFFSET) };
    let (lead, soft) = leading(b);
    // Past this distance from the nearest bisector a pixel is clear glass.
    let clear = if px8::defects() { 1.0 + margin } else { 1.0 + 0.5 * (lead + soft) };
    let mut out = img.clone();
    par_rows(&mut out.d, w, 4, |y, row| {
        let py = y as f64 + 0.5;
        let home_r = (py * kappa).floor() as i64;
        let (mut n1, mut n2, mut n3) = (0usize, 0usize, 0usize);
        let mut k = 0.0;
        for x in 0..w {
            let px = x as f64 + 0.5;
            if k > clear {
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
            // The leading: a line `lead` px wide seen through a `soft` px wide pixel, so its
            // edges ramp over `soft` px and a line thinner than that never turns fully black.
            let mut wt =
                if px8::defects() || beta.is_nan() { beta - 0.5 * margin } else { 1.0 - lead.min(soft).min(0.5 * (lead + soft) - beta).max(0.0) / soft };
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
            let col = sites[n1].2.map(|v| v * gain + offset);
            let mut lit = light(px, py);
            // Where the brightest channel would pass white the reference lights the glass less.
            let over = col.iter().fold(0.0f64, |m, &v| m.max(v.clamp(0.0, 255.0))) + lit - 255.0;
            if over > 0.0 {
                lit *= 1.0 - LIGHT_OVERFLOW * (over / LIGHT_OVERFLOW_RAMP).min(1.0);
            }
            for ch in 0..3 {
                // The light goes on before the contrast's clamp: glass clipped to black still
                // lights up, a little less than the light.
                let c = if px8::defects() { sites[n1].2[ch] } else { (col[ch] + lit).clamp(0.0, 255.0) };
                row[x * 4 + ch] = px8::clamp255((0.5 + wt * c + (1.0 - wt) * fill[ch]).floor());
            }
        }
    });
    out
}

/// The reference's glass has more contrast than the source, at every Light Intensity.
const GLASS_GAIN: f64 = 1.156;
const GLASS_OFFSET: f64 = -14.0;
/// Light per unit of Light Intensity at the centre, the same on each channel, falling linearly
/// to nothing at `LIGHT_RADIUS` half-diagonals. The reference's cone is not round: its distance
/// is `max + LIGHT_SKEW·min` of the offsets from the centre, so the light reaches further along
/// the axes than towards the corners. Fitted on its renders at intensities 0, 3, 5 and 10 of two
/// images, which share their cells: outside the cone they are identical, and inside the glass
/// gains the same levels whatever its colour (the leading stays black).
const LIGHT_PEAK: f64 = 29.3;
const LIGHT_RADIUS: f64 = 0.597;
const LIGHT_SKEW: f64 = 0.5;
/// Where the glass's brightest channel would pass white, the reference adds this much less light
/// (ramped in over `LIGHT_OVERFLOW_RAMP` levels of overflow), measured on its unclipped channels.
const LIGHT_OVERFLOW: f64 = 0.13;
const LIGHT_OVERFLOW_RAMP: f64 = 16.0;

/// The distance between cell sites for Cell Size `c`, fitted on the reference's cell counts: 6 px
/// at 2, 22 at the default 10, and growing much faster towards the top of the range (at 50 a
/// 1536 × 2048 image holds four cells).
fn cell_size(c: f64) -> f64 {
    (2.4 + 1.8 * c + 1.37e-4 * c.powi(4)).max(1.0)
}

/// The width of the leading and of the ramp at its edges, in px, for Border Thickness `b`.
/// Measured on the reference's renders of two images: across a line at thickness 1 the darkness
/// spreads over about 2.6 px and peaks near 0.6 (a 1 px line seen through a 1.6 px pixel); at the
/// default 4 the lines measure 2.85 px with 1 px edges; at 10 it leaves 46 % of the lit centre as
/// glass and at 20 about 1 %. The width is the least-squares quadratic through those four (1.05,
/// 2.85, 8.08 and 21.6 px); between 1 and 4 the edges sharpen linearly.
fn leading(b: f64) -> (f64, f64) {
    let lead = 0.567 + 0.4504 * b + 0.03006 * b * b;
    let soft = 1.0 + 0.6 * ((4.0 - b) / 3.0).clamp(0.0, 1.0);
    (lead, soft)
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
