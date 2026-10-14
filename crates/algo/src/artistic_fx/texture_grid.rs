//! Craquelure, Mosaic Tiles and Patchwork: grids bent by noise slopes, embossed into the HSV
//! value.

use super::super::px8::{self, Img};
use super::super::tables::RELIEF_FACTOR;
use super::{index, param, rng, with_value};
use crate::artistic::GalleryEffect;

/// Craquelure: a rectangular crack lattice bent by noise and image slopes, darkening a 5 × 5
/// median of the max channel, embossed from the top.
pub(super) fn craquelure(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let g = param(e, "crackSpacing", 2.0, 100.0) as i32;
    let depth = param(e, "crackDepth", 0.0, 10.0) / 10.0;
    let bright = param(e, "crackBrightness", 0.0, 10.0) / 10.0;
    let m0 = px8::max_channel(img);
    let m = box_twice(&m0, w, h, 9);
    let wm = if px8::defects() {
        let gn = noise_slopes(e, w, h);
        let mut gi = px8::grad_pairs(&m, w, h);
        gi = pair_box(&pair_box(&gi, w, h, 5), w, h, 5);
        let gn = coord_bias(&gn, w, 0.8);
        coord_blend(&gn, &mut gi, 0.8, 0.9);
        grid_mask(&gi, g)
    } else {
        // The reference's cracks are as far apart as the spacing says, bent by noise and image
        // slopes; the spec stretched them by 1/0.72 and drifted them with a ramp.
        let gn = slope_pairs(&box_twice(&rng(e).fill(w * h), w, h, 11), w, h);
        let gi = pair_box(&pair_box(&slope_pairs(&m, w, h), w, h, 5), w, h, 5);
        let jit: Vec<i32> = gn.iter().zip(&gi).map(|(n, i)| n + i - 128).collect();
        wobbly_grid(&jit, w, g, 0.2)
    };
    let s = px8::rank_rect(&wm, w, h, 2, 2, 1.0);
    let wm = px8::rank_rect(&s, w, h, 2, 2, 3.0);
    let s = px8::rank_rect(&wm, w, h, 2, 2, 3.0);
    let wm = px8::rank_rect(&m0, w, h, 5, 5, 13.0);
    let v = crack_value(&s, &wm, depth, bright, w, h, 1.0);
    with_value(img, &v)
}

/// Mosaic Tiles: a noise-jittered grid of grout lines widened by a minimum filter, darkening
/// the max channel, embossed from the top.
pub(super) fn mosaic_tiles(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let gw = param(e, "groutWidth", 1.0, 15.0) as usize;
    let g = param(e, "tileSize", 2.0, 100.0) as i32 + gw as i32;
    let lighten = param(e, "lightenGrout", 0.0, 10.0) / 10.0;
    let mask = if px8::defects() {
        let gn = pair_box(&noise_slopes(e, w, h), w, h, 5);
        grid_mask(&coord_bias(&gn, w, 0.97), g)
    } else {
        // The reference's grid keeps its period and wobbles in small steps; the spec's slope
        // fallback (a ramp over the image) stretched it and drifted its phase.
        let n = box_twice(&rng(e).fill(w * h), w, h, 7);
        wobbly_grid(&pair_box(&slope_pairs(&n, w, h), w, h, 5), w, g, 0.03)
    };
    let s = px8::rank_rect(&mask, w, h, gw, gw, 1.0);
    let v = crack_value(&s, &px8::max_channel(img), 0.6, lighten, w, h, 1.0);
    with_value(img, &v)
}

/// Patchwork: tile means with a noisy value, darkened seams, softened, dilated and embossed.
pub(super) fn patchwork(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let d = param(e, "squareSize", 0.0, 10.0) as usize + 5;
    let wr = RELIEF_FACTOR[index(e, "relief", RELIEF_FACTOR.len())];
    let (inset, ms, bs) = (d / 5 + 1, d / 2 + 1, d / 3);
    let mut t = block_average(img, d);
    px8::to_hsv(&mut t);
    let mut v = t.plane(2);
    for (v, n) in v.iter_mut().zip(rng(e).fill(img.n())) {
        *v = px8::clamp255(*v as f64 + (0.2 * (n as f64 - 128.0)).trunc());
    }
    let mut rm = edge_merge_mask(w, h, d, inset);
    px8::screen(&v, &mut rm, 1.0);
    let v = box_twice(&rm, w, h, bs);
    let g = px8::rank_rect(&v, w, h, ms, ms, (ms * ms) as f64);
    let v = px8::relief(&g, &g, w, h, 5, wr);
    t.set_plane(2, &v);
    px8::from_hsv(&mut t);
    t
}

/// Gradient angle pairs of twice 11 × 11 box-blurred random bytes.
fn noise_slopes(e: &GalleryEffect, w: usize, h: usize) -> Vec<i32> {
    let n = rng(e).fill(w * h);
    px8::grad_pairs(&box_twice(&n, w, h, 11), w, h)
}

/// Screen `s` over `b` at `depth`, multiply `b` back over that at `bright`, then emboss from the
/// top with weight `wt`.
fn crack_value(s: &[u8], b: &[u8], depth: f64, bright: f64, w: usize, h: usize, wt: f64) -> Vec<u8> {
    let mut v = b.to_vec();
    px8::screen(s, &mut v, depth);
    px8::multiply(b, &mut v, bright);
    px8::relief(&v, &v, w, h, 5, wt)
}

fn box_twice(p: &[u8], w: usize, h: usize, k: usize) -> Vec<u8> {
    px8::box_mean(&px8::box_mean(p, w, h, k, k), w, h, k, k)
}

/// Pair box mean (P-09): both values of a pair plane box-blurred as bytes.
fn pair_box(p: &[i32], w: usize, h: usize, k: usize) -> Vec<i32> {
    let half = |o: usize| -> Vec<u8> { p.iter().skip(o).step_by(2).map(|&v| v.clamp(0, 255) as u8).collect() };
    let (a, b) = (px8::box_mean(&half(0), w, h, k, k), px8::box_mean(&half(1), w, h, k, k));
    a.iter().zip(&b).flat_map(|(&a, &b)| [a as i32, b as i32]).collect()
}

/// Like [`px8::grad_pairs`], but a flat neighbourhood gives the neutral 128.
fn slope_pairs(p: &[u8], w: usize, h: usize) -> Vec<i32> {
    let k = (1.0 / std::f64::consts::PI) * 255.0;
    let ang = |d: f64| -> i32 {
        if d == 0.0 {
            return 128;
        }
        let a = ((1.0 / -d).atan2(1.0) * k + 0.5).trunc() as i32;
        if a < 0 { a + 255 } else { a }
    };
    let mut out = vec![0i32; w * h * 2];
    for y in 0..h {
        let yn = (y + 1).min(h - 1);
        for x in 0..w {
            let xn = (x + 1).min(w - 1);
            let c = p[y * w + x] as f64;
            out[(y * w + x) * 2] = ang(c - p[yn * w + x] as f64);
            out[(y * w + x) * 2 + 1] = ang(c - p[y * w + xn] as f64);
        }
    }
    out
}

/// Grid mask of period `g` whose lines are pushed by `j·(v − 128)` px, `v` the pair values.
fn wobbly_grid(p: &[i32], w: usize, g: i32, j: f64) -> Vec<u8> {
    let g = g.max(1);
    p.chunks_exact(2)
        .enumerate()
        .map(|(i, c)| {
            let (x, y) = ((i % w.max(1)) as f64, (i / w.max(1)) as f64);
            let cy = (y + (c[0] - 128) as f64 * j).round() as i64;
            let cx = (x + (c[1] - 128) as f64 * j).round() as i64;
            if cx.rem_euclid(g as i64) >= 2 && cy.rem_euclid(g as i64) >= 2 { 255 } else { 0 }
        })
        .collect()
}

/// Coordinate bias (P-23): `(trunc(y·f + ½) + trunc(v₀·(1 − f) + ½), trunc(x·f + ½) + trunc(v₁·(1 − f) + ½))`.
fn coord_bias(p: &[i32], w: usize, f: f64) -> Vec<i32> {
    let g = 1.0 - f;
    let mut out = p.to_vec();
    for (i, o) in out.chunks_exact_mut(2).enumerate() {
        let (x, y) = ((i % w.max(1)) as f64, (i / w.max(1)) as f64);
        o[0] = (y * f + 0.5) as i32 + (o[0] as f64 * g + 0.5) as i32;
        o[1] = (x * f + 0.5) as i32 + (o[1] as f64 * g + 0.5) as i32;
    }
    out
}

/// Coordinate blend (P-23): each `dst = trunc(dst·(1 − f) + ½) + trunc(src·wt + ½)`.
fn coord_blend(src: &[i32], dst: &mut [i32], f: f64, wt: f64) {
    let g = 1.0 - f;
    for (d, s) in dst.iter_mut().zip(src) {
        *d = (*d as f64 * g + 0.5) as i32 + (*s as f64 * wt + 0.5) as i32;
    }
}

/// Grid mask (P-23): 255 where both coordinates sit at least 2 past a multiple of `g`.
fn grid_mask(p: &[i32], g: i32) -> Vec<u8> {
    let g = g.max(1);
    p.chunks_exact(2).map(|c| if c[1] % g >= 2 && c[0] % g >= 2 { 255 } else { 0 }).collect()
}

/// Edge-merge mask (P-23): 0 on the first `inset` rows and columns of every `period` tile, except
/// along the image's own edges; 255 elsewhere.
fn edge_merge_mask(w: usize, h: usize, period: usize, inset: usize) -> Vec<u8> {
    let period = period.max(1);
    let keep = |v: usize, n: usize| v % period >= inset || v < inset || v + inset >= n;
    let cols: Vec<bool> = (0..w).map(|x| keep(x, w)).collect();
    (0..h)
        .flat_map(|y| {
            let row = keep(y, h);
            cols.iter().map(move |&c| if row && c { 255 } else { 0 })
        })
        .collect()
}

/// Block average (P-23): each `t × t` tile from (0, 0) filled with the mean colour of the `t × t`
/// window at its top-left, samples clamped at the right and bottom edges; alpha kept.
fn block_average(img: &Img, t: usize) -> Img {
    let (w, h) = (img.w, img.h);
    let t = t.max(1);
    let mut out = img.clone();
    let area = (t * t) as u64;
    for ty in (0..h).step_by(t) {
        for tx in (0..w).step_by(t) {
            let mut sum = [0u64; 3];
            for j in 0..t {
                let y = (ty + j).min(h - 1);
                for i in 0..t {
                    let p = (y * w + (tx + i).min(w - 1)) * 4;
                    for c in 0..3 {
                        sum[c] += img.d[p + c] as u64;
                    }
                }
            }
            let mean = sum.map(|s| (s / area) as u8);
            for y in ty..(ty + t).min(h) {
                for x in tx..(tx + t).min(w) {
                    out.d[(y * w + x) * 4..(y * w + x) * 4 + 3].copy_from_slice(&mean);
                }
            }
        }
    }
    out
}
