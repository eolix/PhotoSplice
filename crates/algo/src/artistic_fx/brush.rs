//! Filter Gallery › Brush Strokes.
//!
//! Where Photoshop measurably differs from the reference renders, the Photoshop behaviour is
//! used and the reference one kept behind [`px8::defects`]: thick (two-pixel) Angled Strokes
//! with the true median, Crosshatch's truncated rank, rounded mask blends and sharpen terms,
//! Photoshop's placement of even-length sheared windows, and value edits that keep hue and
//! saturation exactly instead of the byte-HSV round trip.

use super::Render;
use super::px8::{self, Img};
use super::tables::{FIELD_SIGMA, SPRAYED_STRENGTH, STROKE_AMOUNT};
use crate::artistic::{GalleryEffect, GalleryFilter as F};
use crate::photo_util::par_rows;

/// The renderer of a filter this module implements.
pub(super) fn render(f: F) -> Option<Render> {
    match f {
        F::AccentedEdges => Some(accented_edges),
        F::AngledStrokes => Some(angled_strokes),
        F::Crosshatch => Some(crosshatch),
        F::DarkStrokes => Some(dark_strokes),
        F::InkOutlines => Some(ink_outlines),
        F::Spatter => Some(spatter),
        F::SprayedStrokes => Some(sprayed_strokes),
        F::SumiE => Some(sumi_e),
        _ => None,
    }
}

// ---- helpers -----------------------------------------------------------------------------------

/// A slider value rounded and clamped to `lo..=hi` (NaN → `lo`).
fn int(e: &GalleryEffect, key: &str, lo: usize, hi: usize) -> usize {
    let v = e.get(key);
    if v.is_nan() { lo } else { (v.round().max(0.0) as usize).clamp(lo, hi) }
}

/// A slider value clamped to `lo..=hi` (NaN → `lo`).
fn real(e: &GalleryEffect, key: &str, lo: f64, hi: f64) -> f64 {
    let v = e.get(key) as f64;
    if v.is_nan() { lo } else { v.clamp(lo, hi) }
}

/// `out` with `src`'s alpha.
fn keep_alpha(mut out: Img, src: &Img) -> Img {
    for (o, s) in out.d.chunks_exact_mut(4).zip(src.d.chunks_exact(4)) {
        o[3] = s[3];
    }
    out
}

/// Replaces `img`'s HSV value (its largest channel) through `f`. Photoshop keeps hue and
/// saturation exactly (each channel scaled by new/old value); the reference round-trips byte HSV.
fn edit_value(mut img: Img, f: impl FnOnce(&mut Vec<u8>)) -> Img {
    if px8::defects() {
        px8::to_hsv(&mut img);
        let mut v = img.plane(2);
        f(&mut v);
        img.set_plane(2, &v);
        px8::from_hsv(&mut img);
        return img;
    }
    let old = px8::max_channel(&img);
    let mut new = old.clone();
    f(&mut new);
    for ((px, &vo), &vn) in img.d.chunks_exact_mut(4).zip(&old).zip(&new) {
        if vo == 0 {
            px[..3].fill(vn);
        } else {
            let k = vn as f64 / vo as f64;
            for c in &mut px[..3] {
                *c = px8::clamp255(*c as f64 * k + 0.5);
            }
        }
    }
    img
}

/// Mask blend (P-17): where `m ≠ 0` each channel is `(m·top + (255 − m)·base)/255`, rounded in
/// Photoshop, truncated in the reference; alpha from `top`.
fn blend(top: &Img, m: &[u8], base: &Img) -> Img {
    let mut out = base.clone();
    if px8::defects() {
        px8::blend_mask_rgb(top, m, base, &mut out);
        return out;
    }
    for ((o, t), &mm) in out.d.chunks_exact_mut(4).zip(top.d.chunks_exact(4)).zip(m) {
        if mm != 0 {
            let mf = mm as f64;
            for c in 0..3 {
                o[c] = px8::clamp255((mf * t[c] as f64 + (255.0 - mf) * o[c] as f64) / 255.0 + 0.5);
            }
        }
        o[3] = t[3];
    }
    out
}

/// Four-neighbour sharpen of R, G and B with amount `a` (P-14). Photoshop rounds the centre and
/// neighbour terms to integers, `C + round(4·C·a) − round(Σ·a)`, and uses the pixel below.
fn sharpen(img: &Img, a: f64) -> Img {
    if px8::defects() {
        return px8::sharpen4_rgb(img, a);
    }
    let (w, h) = (img.w, img.h);
    img.map_rgb(|p| {
        let mut out = vec![0u8; w * h];
        par_rows(&mut out, w, 1, |y, row| {
            let up = y.saturating_sub(1);
            let down = (y + 1).min(h - 1);
            for (x, o) in row.iter_mut().enumerate() {
                let c = p[y * w + x] as f64;
                let l = p[y * w + x.saturating_sub(1)] as f64;
                let r = p[y * w + (x + 1).min(w - 1)] as f64;
                let s = l + r + p[up * w + x] as f64 + p[down * w + x] as f64;
                *o = px8::clamp255(c + px8::rhu(4.0 * c * a) - px8::rhu(s * a));
            }
        });
        out
    })
}

/// Sheared rank filter of R, G and B (P-12) with Photoshop's window placement: its rows start
/// `⌊L/2⌋ + ⌊T/2⌋` columns left of the pixel, one further than the reference's
/// `⌊(L + T − 1)/2⌋` when both `L` and `T` are even.
fn rank_dir_rgb(img: &Img, len: usize, span: usize, reverse: bool, rank: f64) -> Img {
    let extra = (len / 2 + span / 2).saturating_sub((len + span).saturating_sub(1) / 2);
    if extra == 0 || px8::defects() || img.n() == 0 {
        return px8::rank_dir_rgb(img, len, span, reverse, rank);
    }
    // Filtering a copy padded on the left with `extra` copies of the first column shifts every
    // window left by `extra`, edges still clamped.
    let (w, h) = (img.w, img.h);
    let pw = w + extra;
    let mut pad = Img::new(pw, h);
    for y in 0..h {
        for x in 0..pw {
            let s = (y * w + x.saturating_sub(extra)) * 4;
            let d = (y * pw + x) * 4;
            pad.d[d..d + 4].copy_from_slice(&img.d[s..s + 4]);
        }
    }
    let f = px8::rank_dir_rgb(&pad, len, span, reverse, rank);
    let mut out = img.clone();
    for y in 0..h {
        out.d[y * w * 4..(y + 1) * w * 4].copy_from_slice(&f.d[y * pw * 4..(y * pw + w) * 4]);
    }
    out
}

// ---- filters -----------------------------------------------------------------------------------

/// Rank-smoothed colour; edges (max − min of the HSV value) lift or sink the value.
fn accented_edges(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let ew = int(e, "edgeWidth", 1, 14) + 1;
    let s = int(e, "smoothness", 1, 15);
    let b = real(e, "edgeBrightness", 0.0, 50.0);
    let beta = (b - 25.0).abs() / 4.0;
    let smooth = px8::rank_rect_rgb(img, s, s, (((s % 2) + s * s) >> 1) as f64);
    let out = edit_value(smooth, |v| {
        let hi = px8::rank_rect(v, w, h, ew, ew, (ew * ew) as f64);
        let mut g = px8::rank_rect(v, w, h, ew, ew, 1.0);
        px8::subtract(&hi, &mut g);
        px8::scale(&mut g, beta);
        if b < 25.0 {
            px8::subtract(v, &mut g);
            *v = g;
        } else {
            px8::add(&g, v);
        }
    });
    keep_alpha(out, img)
}

/// Down-right strokes in light areas, up-right strokes in dark ones, then a sharpen.
/// Photoshop's strokes are two pixels thick (the median of `2L` samples); the reference's are
/// one pixel thick with the rank below the median.
fn angled_strokes(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let l = int(e, "strokeLength", 2, 50);
    let balance = real(e, "directionBalance", 0.0, 100.0) * 2.55;
    let (span, rank, t) = if px8::defects() { (1, l >> 1, balance) } else { (2, l, balance.floor()) };
    let down = rank_dir_rgb(img, l, span, false, rank as f64);
    let up = rank_dir_rgb(img, l, span, true, rank as f64);
    let mut m = px8::gray_mean(img);
    px8::threshold(&mut m, t, 0, 255);
    let m = px8::box_mean(&m, w, h, 3, 3);
    keep_alpha(sharpen(&blend(&down, &m, &up), real(e, "sharpness", 0.0, 10.0) * 0.25), img)
}

/// `strength` passes of: both diagonal rank strokes averaged, a 2 × 2 box, a sharpen. The rank
/// is `⌊L/2⌋` in Photoshop; the reference rounds the fractional `L/2` up.
fn crosshatch(e: &GalleryEffect, img: &Img) -> Img {
    let l = int(e, "strokeLength", 1, 50);
    let a = real(e, "sharpness", 0.0, 20.0) / 2.0;
    let rank = if px8::defects() { l as f64 / 2.0 } else { (l / 2) as f64 };
    let mut d = img.clone();
    for _ in 0..int(e, "strength", 1, 3) {
        let up = rank_dir_rgb(&d, l, 1, true, rank);
        let mut down = rank_dir_rgb(&d, l, 1, false, rank);
        px8::average(&up.d, &mut down.d);
        d = sharpen(&px8::box_mean_rgb(&down, 2, 2), a);
    }
    keep_alpha(d, img)
}

/// Light "/" medians above the balance percentile, dark "\" minima below, value auto-levels.
fn dark_strokes(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let mut m = px8::gray_mean(img);
    let t = px8::percentile(&px8::histogram(&m), img.n() as f64, real(e, "balance", 0.0, 10.0) * 10.0);
    px8::threshold(&mut m, t as f64, 0, 255);
    let m = px8::box_mean(&m, w, h, 3, 3);
    let dark = rank_dir_rgb(img, 5, 1, false, 1.0);
    let light = rank_dir_rgb(img, 15, 2, true, 16.0);
    let c = blend(&light, &m, &dark);
    let (cb, cw) = (real(e, "blackIntensity", 0.0, 10.0) * 4.9, real(e, "whiteIntensity", 0.0, 10.0) * 4.9);
    keep_alpha(edit_value(c, |v| px8::auto_levels(v, cb, cw, None)), img)
}

/// Diagonal median strokes inked through an auto-levelled edge mask, then value auto-levels.
fn ink_outlines(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let l = int(e, "strokeLength", 1, 50);
    let alpha = img.plane(3);
    let g = px8::gray_mean(img);
    let hi = px8::rank_rect(&g, w, h, 3, 3, 9.0);
    let mut edge = px8::rank_rect(&g, w, h, 3, 3, 1.0);
    px8::subtract(&hi, &mut edge);
    let mut edge = px8::box_mean(&edge, w, h, 3, 3);
    px8::auto_levels(&mut edge, 40.0, 10.0, Some(&alpha));
    let p = rank_dir_rgb(img, l, 2, false, l as f64);
    let q = rank_dir_rgb(img, l, 2, true, l as f64);
    // The ink comes from blending the strokes into opaque black.
    let mut black = Img::new(w, h);
    black.d.chunks_exact_mut(4).for_each(|px| px[3] = 255);
    let ink = blend(&p, &edge, &black);
    let c = sharpen(&blend(&ink, &edge, &q), 1.0);
    let (dk, lt) = (real(e, "darkIntensity", 0.0, 50.0), real(e, "lightIntensity", 0.0, 50.0));
    keep_alpha(edit_value(c, |v| px8::auto_levels(v, dk, lt, Some(&alpha))), img)
}

/// A stroked, auto-levelled max-channel value darkened where strokes beat their surroundings,
/// over the hue and saturation of a heavy blur.
fn sumi_e(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let wb = int(e, "strokeWidth", 0, 50);
    let kappa = STROKE_AMOUNT[int(e, "strokePressure", 0, 15)];
    let c = real(e, "contrast", 0.0, 50.0);
    let mut dm = px8::rank_dir(&px8::max_channel(img), w, h, 15, 2, true, 15.0);
    let mut st = dm.clone();
    px8::auto_levels(&mut st, c, 1.5 * c, None);
    let x = px8::box_mean(&st, w, h, 3, 3);
    let st = px8::box_mean(&dm, w, h, wb, wb);
    px8::subtract(&st, &mut dm);
    px8::scale(&mut dm, kappa);
    px8::subtract(&x, &mut dm);
    let x = px8::box_mean(&dm, w, h, 3, 3);
    let v = px8::rank_dir(&x, w, h, 3, 1, false, 1.0);
    keep_alpha(edit_value(px8::box_mean_rgb(img, 15, 15), |p| *p = v), img)
}

// ---- Spatter and Sprayed Strokes ---------------------------------------------------------------

/// The random displacement table (R-2, 7919 entries, amplitude `70·strength`) laid over the
/// image in raster order, two values per pixel.
fn spray_field(seed: u32, strength: f64, w: usize, h: usize) -> (Vec<f32>, Vec<f32>) {
    const PERIOD: u64 = 7919;
    let mut rng = px8::HashRng::new(seed);
    let table: Vec<f32> = (0..PERIOD).map(|_| ((-1.0 + 2.0 * rng.next()) * strength * 70.0) as f32).collect();
    let at = |i: u64| table.get((i % PERIOD) as usize).copied().unwrap_or(0.0);
    let n = (w * h) as u64;
    ((0..n).map(|p| at(2 * p)).collect(), (0..n).map(|p| at(2 * p + 1)).collect())
}

/// Random per-pixel jitter, smoothed by a 13-tap Gaussian.
fn spatter(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    if img.n() == 0 {
        return img.clone();
    }
    let r = real(e, "sprayRadius", 0.0, 25.0);
    let strength = if r < 20.0 { 0.018 * r } else { 0.36 + 0.128 * (r - 20.0) };
    let (mut fx, mut fy) = spray_field(e.seed(), strength, w, h);
    let j = int(e, "smoothness", 1, 15) - 1;
    if let Some(&sigma) = FIELD_SIGMA.get(j).filter(|_| j != 0) {
        fx = px8::field_gauss13(&fx, w, h, sigma);
        fy = px8::field_gauss13(&fy, w, h, sigma);
    }
    keep_alpha(px8::warp(img, &fx, &fy), img)
}

/// Random jitter averaged along the stroke direction over a trailing window.
fn sprayed_strokes(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    if img.n() == 0 {
        return img.clone();
    }
    let strength = 0.07 + SPRAYED_STRENGTH.get(int(e, "sprayRadius", 0, 25)).copied().unwrap_or(0.0);
    let (fx, fy) = spray_field(e.seed(), strength, w, h);
    let half = int(e, "strokeLength", 0, 20) >> 1;
    let sigma = FIELD_SIGMA[if half == 0 { 2 } else { 1 }];
    let (mut fx, mut fy) = (px8::field_gauss13(&fx, w, h, sigma), px8::field_gauss13(&fy, w, h, sigma));
    if half != 0 {
        let dir = match e.choice("strokeDirection") {
            "horizontal" => (1, 0),
            "leftDiagonal" => (1, 1),
            "vertical" => (0, 1),
            _ => (1, -1),
        };
        fx = trailing_mean(&fx, w, h, half, dir);
        fy = trailing_mean(&fy, w, h, half, dir);
    }
    keep_alpha(px8::warp(img, &fx, &fy), img)
}

/// Mean of the `2·half + 1` samples at offsets `half … 3·half` behind each pixel along
/// `(dx, dy)`, coordinates clamped.
fn trailing_mean(p: &[f32], w: usize, h: usize, half: usize, (dx, dy): (i64, i64)) -> Vec<f32> {
    let n = 2 * half + 1;
    let k = 1.0 / n as f64;
    let (wi, hi) = (w as i64, h as i64);
    let mut out = vec![0f32; w * h];
    par_rows(&mut out, w, 1, |y, row| {
        for (x, o) in row.iter_mut().enumerate() {
            let mut s = 0.0f64;
            for t in half as i64..(half + n) as i64 {
                let sx = (x as i64 - t * dx).clamp(0, wi - 1) as usize;
                let sy = (y as i64 - t * dy).clamp(0, hi - 1) as usize;
                s += p[sy * w + sx] as f64;
            }
            *o = (s * k) as f32;
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: usize, h: usize) -> Img {
        let mut i = Img::new(w, h);
        for (k, px) in i.d.chunks_exact_mut(4).enumerate() {
            let (x, y) = (k % w.max(1), k / w.max(1));
            px.copy_from_slice(&[((37 * x + 11 * y) % 256) as u8, ((13 * x + 59 * y) % 256) as u8, ((7 * x * x + 23 * y) % 256) as u8, (200 + k % 56) as u8]);
        }
        i
    }

    #[test]
    fn every_filter_survives_tiny_images_and_extreme_values() {
        let fs = [F::AccentedEdges, F::AngledStrokes, F::Crosshatch, F::DarkStrokes, F::InkOutlines, F::Spatter, F::SprayedStrokes, F::SumiE];
        for f in fs {
            let r = render(f).expect("brush filter");
            for (w, h) in [(0, 0), (0, 3), (3, 0), (1, 1), (2, 2), (5, 1), (1, 7), (9, 4)] {
                for v in [f32::NEG_INFINITY, -1e9, f32::NAN, 0.0, 1e9, f32::INFINITY] {
                    let mut e = GalleryEffect::new(f);
                    for p in f.params() {
                        e.values.insert(p.key.to_string(), v);
                    }
                    let src = img(w, h);
                    let out = r(&e, &src);
                    assert_eq!((out.w, out.h, out.d.len()), (w, h, w * h * 4), "{} {w}x{h}", f.key());
                    assert!(out.d.chunks_exact(4).zip(src.d.chunks_exact(4)).all(|(o, s)| o[3] == s[3]), "{} keeps alpha", f.key());
                }
            }
        }
    }

    #[test]
    fn photoshop_window_placement_moves_even_thick_windows_left() {
        let src = img(12, 9);
        let base = px8::rank_dir_rgb(&src, 4, 2, false, 4.0);
        let ps = rank_dir_rgb(&src, 4, 2, false, 4.0);
        for y in 0..9 {
            for x in 1..12 {
                assert_eq!(ps.d[(y * 12 + x) * 4], base.d[(y * 12 + x - 1) * 4], "{x},{y}");
            }
        }
        assert_eq!(rank_dir_rgb(&src, 5, 2, true, 5.0), px8::rank_dir_rgb(&src, 5, 2, true, 5.0));
    }

    #[test]
    fn value_edit_keeps_hue_and_saturation() {
        let mut i = Img::new(1, 1);
        i.d = vec![200, 100, 50, 255];
        let out = edit_value(i, |v| v[0] = 100);
        assert_eq!(out.d, vec![100, 50, 25, 255]);
    }
}
