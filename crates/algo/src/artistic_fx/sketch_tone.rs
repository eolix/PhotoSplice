//! Sketch filters built from thresholds, box means and rank filters: Chalk & Charcoal, Charcoal,
//! Photocopy, Reticulation, Stamp, Torn Edges.

use super::super::px8::{self, Img};
use super::super::tables;
use super::{ink_paper, int};
use crate::artistic::GalleryEffect;

/// Chalk & Charcoal: dropout-textured percentile masks dilated along the two diagonals, chalk
/// (background) on the light side and charcoal (foreground) on the dark side of a gray 128 base.
pub(in super::super) fn chalk_charcoal(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let n = img.n();
    let p_dark = 80.0 - 3.0 * e.get("charcoalArea") as f64;
    let p_light = 80.0 - 3.0 * e.get("chalkArea") as f64;
    let s = e.get("strokePressure") as f64 / 2.0 + 0.5;
    let g = px8::gray_mean(img);
    let mut rng = px8::Rng::new(e.seed() >> 1);

    let mut light = g.clone();
    px8::auto_levels(&mut light, p_light, 5.0, None);
    dropout(&mut light, &mut rng, 0.4);
    let t = px8::box_mean(&light, w, h, 3, 3);
    let mut light = px8::rank_dir(&t, w, h, 9, 3, true, 27.0);

    let inv: Vec<u8> = g.iter().map(|v| 255 - v).collect();
    let lut = px8::levels_lut(&px8::histogram(&inv), n as f64, p_dark, 5.0);
    let mut dark: Vec<u8> = inv.iter().map(|v| lut[*v as usize]).collect();
    dropout(&mut dark, &mut rng, 0.4);
    let t = px8::box_mean(&dark, w, h, 2, 2);
    let mut dark = px8::rank_dir(&t, w, h, 5, 3, false, 15.0);

    px8::scale(&mut light, s);
    px8::scale(&mut dark, s);
    let (fg, bg) = ink_paper(e);
    let (chalk, charcoal) = (bg, fg);
    let mut out = img.clone();
    for (i, px) in out.d.chunks_exact_mut(4).enumerate() {
        for c in 0..3 {
            let mut v = 128.0;
            for (m, col) in [(light[i], chalk[c]), (dark[i], charcoal[c])] {
                if m != 0 {
                    let m = m as f64;
                    v = ((m * col as f64 + (255.0 - m) * v) / 255.0).trunc();
                }
            }
            px[c] = v as u8;
        }
    }
    out
}

/// Sets each value to 0 with probability `p`, one draw per pixel in raster order.
fn dropout(p: &mut [u8], rng: &mut px8::Rng, prob: f64) {
    for v in p.iter_mut() {
        if rng.next() < prob {
            *v = 0;
        }
    }
}

/// Charcoal: dark detail stretched and smeared by sheared rank filters along both diagonals,
/// mixed toward gray, sharpened, then a hard contrast window set by balance and detail. The
/// window starts at `trunc(0.78·balance) + 44`; the spec had `trunc(0.73·balance) + 50`, which
/// left balance 0 too dark.
pub(in super::super) fn charcoal(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let thick = int(e, "charcoalThickness").min(1000);
    let detail = e.get("detail") as f64;
    let balance = e.get("lightDarkBalance") as f64;
    let spread = 5.0 + 10.0 * detail + if detail > 3.0 { 10.0 * (detail - 3.0) } else { 0.0 };
    // The reference's window starts lower and moves a little faster with the balance.
    let (k, m0) = if px8::defects() { (0.73, 50.0) } else { (0.78, 44.0) };
    let mid = (k * balance).trunc() + m0;
    let upper = (mid + spread).min(128.0);
    let range = upper - mid;
    let mut lut = [0u8; 256];
    for (i, v) in lut.iter_mut().enumerate() {
        *v = px8::clamp255(255.0 * (i as f64 - mid) / range);
    }

    let orig = px8::gray_mean(img);
    let b = px8::box_mean(&orig, w, h, 2 * thick + 1, 2 * thick + 1);
    let mut g = orig.clone();
    px8::subtract(&b, &mut g);
    px8::scale(&mut g, 65.0);
    let b = px8::rank_dir(&g, w, h, 9, 3, true, 20.0);
    let g: Vec<u8> = b.iter().zip(&orig).map(|(&b, &o)| if b > 0 { ((b as u32 * o as u32 + (255 - b as u32) * 128) / 255) as u8 } else { 128 }).collect();
    let b = px8::box_mean(&g, w, h, 3, 3);
    let up = px8::rank_dir(&b, w, h, 15, 2, true, 16.0);
    let mut o = px8::rank_dir(&b, w, h, 15, 2, false, 15.0);
    px8::average(&up, &mut o);
    let mut b = px8::sharpen4(&o, w, h, 2.0);
    px8::apply_lut(&mut b, &lut);
    let g = px8::box_mean(&b, w, h, 2, 2);
    let (fg, bg) = ink_paper(e);
    px8::colourise(&g, fg, bg, img)
}

/// Photocopy: a difference of two Gaussians (σ 1 against σ = detail) scaled by darkness inks
/// the dark side of edges.
///
/// The reference works on the plain mean gray, with a difference of Gaussians a little narrower
/// than the spec's, and lets a difference of one level through only faintly: the ink is
/// `(d − 0.7)·darkness` where the spec had `d·darkness`.
pub(in super::super) fn photocopy(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let quirk = px8::defects();
    let mut detail = e.get("detail") as f64;
    let dark = e.get("darkness") as f64;
    let mut dark = if dark.is_finite() { dark } else { 0.0 };
    if detail <= 1.0 || !detail.is_finite() {
        detail = 2.0;
        dark = px8::rhu(dark / 4.0);
    }
    let floor = if quirk { 0.0 } else { PC_FLOOR };
    if !quirk {
        detail *= 6.0 / 7.0;
    }
    let detail = detail.min(500.0);
    let g = if quirk { px8::gray_weighted(img) } else { px8::gray_mean(img) };
    let b1 = px8::gauss_u8(&g, w, h, 1.0, 2);
    let bd = px8::gauss_u8(&g, w, h, detail, 2);
    let v: Vec<u8> = (0..img.n())
        .map(|i| {
            let gv = g[i] as f64;
            let l = (128.0 + gv - b1[i] as f64).clamp(0.0, 255.0);
            let hh = (128.0 + gv - bd[i] as f64).clamp(0.0, 255.0);
            let d = l - hh;
            let ink = if d > 0.0 { (d - floor) * dark } else { d * dark };
            px8::clamp255(255.0 - ink)
        })
        .collect();
    let (fg, bg) = ink_paper(e);
    px8::colourise_direct(&v, fg, bg, img)
}

/// How much of Photocopy's edge difference the reference ignores, in gray levels.
const PC_FLOOR: f64 = 0.7;

/// Reticulation: a median-smoothed, auto-levelled image combined with blurred binary noise
/// through screen and multiply blends, then rank, sharpen and box passes.
pub(in super::super) fn reticulation(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let p = e.get("density") as f64 / 50.0;
    let g = px8::gray_mean(img);
    let mut rng = px8::Rng::new(e.seed() >> 1);
    let noise: Vec<u8> = (0..img.n()).map(|_| if rng.next() < p { 255 } else { 0 }).collect();
    let mut r = px8::box_mean(&noise, w, h, 3, 3);
    let mut n = px8::rank_rect(&g, w, h, 9, 9, 41.0);
    px8::auto_levels(&mut n, e.get("foregroundLevel") as f64, e.get("backgroundLevel") as f64, None);
    px8::screen(&n, &mut r, 0.75);
    let mut t = r.clone();
    px8::multiply(&n, &mut t, 0.5);
    let n = t;
    let r = px8::rank_rect(&n, w, h, 3, 3, 8.0);
    let mut t = n.clone();
    px8::screen(&r, &mut t, 0.75);
    let mut r = t;
    px8::scale(&mut r, 1.2);
    let g = px8::sharpen4(&r, w, h, 1.0);
    let r = px8::box_mean(&g, w, h, 2, 2);
    let mut g = px8::sharpen4(&r, w, h, 2.0);
    px8::scale(&mut g, 1.2);
    let r = px8::box_mean(&g, w, h, 2, 2);
    let (fg, bg) = ink_paper(e);
    px8::colourise(&r, fg, bg, img)
}

/// Stamp: local contrast boost, a piecewise threshold from the balance, a despeckle, a smoothing
/// box twice and a steep output ramp.
pub(in super::super) fn stamp(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let b = e.get("lightDarkBalance") as f64;
    let t = if b < 2.0 {
        b
    } else if b < 12.0 {
        5.0 * b - 5.0
    } else if b < 47.0 {
        2.0 * b + 28.0
    } else if b < 50.0 {
        let k = b - 47.0;
        125.0 + 12.5 * k + 12.5 * k * k
    } else {
        255.0
    };
    let smooth = int(e, "smoothness").min(1000);
    let g = px8::gray_mean(img);
    let b1 = px8::box_mean(&g, w, h, 11, 11);
    let b2 = px8::box_mean(&b1, w, h, 11, 11);
    let mut v = g;
    px8::subtract(&b2, &mut v);
    px8::scale(&mut v, 8.0);
    px8::subtract(&b2, &mut v);
    px8::threshold(&mut v, t, 0, 255);
    px8::invert(&mut v);
    let mut b1 = px8::box_mean(&v, w, h, 5, 5);
    px8::threshold(&mut b1, 51.0, 0, 255);
    let v = px8::box_mean(&b1, w, h, smooth, smooth);
    let b1 = px8::box_mean(&v, w, h, smooth, smooth);
    let out: Vec<u8> = b1
        .iter()
        .map(|&v| {
            let i = 255 - v as i32;
            if i < 75 { 0 } else { (15 + 24 * (i - 75)).min(255) as u8 }
        })
        .collect();
    let (fg, bg) = ink_paper(e);
    px8::colourise(&out, fg, bg, img)
}

/// Torn Edges: a thresholded mask softened by a double box blur, uniform grain mixed into the
/// soft edge, a two-sided contrast stretch, a 2 × 2 rank.
pub(in super::super) fn torn_edges(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let t = 5.1 * e.get("imageBalance") as f64;
    // The reference's threshold is a whole level: gray 127 is light at the default balance (25).
    let t = if px8::defects() { t } else { t.floor() };
    let bw = (16 - int(e, "smoothness").min(15)).max(1);
    let k = tables::TORN_EDGES_CONTRAST[int(e, "contrast").min(25)];
    let mut m = px8::gray_mean(img);
    px8::threshold(&mut m, t, 0, 255);
    let s = px8::box_mean(&m, w, h, bw, bw);
    let m = px8::box_mean(&s, w, h, bw, bw);
    let mut rng = px8::Rng::new(e.seed() >> 1);
    let mut n = rng.fill(img.n());
    px8::multiply(&m, &mut n, 1.0);
    let mut mm = n;
    px8::screen(&m, &mut mm, 0.75);
    px8::scale(&mut mm, k);
    px8::inverse_scale(&mut mm, k);
    let s = px8::rank_rect(&mm, w, h, 2, 2, 2.0);
    let (fg, bg) = ink_paper(e);
    px8::colourise(&s, fg, bg, img)
}
