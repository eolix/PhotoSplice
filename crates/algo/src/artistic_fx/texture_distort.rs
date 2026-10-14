//! Diffuse Glow, Glass, Ocean Ripple (Distort) and Glowing Edges (Stylize).

use super::super::px8::{self, Clipped, Img, TEX, Texture};
use super::super::tables::{DIFFUSE_GLOW_CLEAR, DIFFUSE_GLOW_GLOW, GLASS_TEXTURE_FACTOR};
use super::{index, param, rng};
use crate::artistic::GalleryEffect;
use crate::photo_util::par_rows;

/// Diffuse Glow: the background colour glows through a doubly box-blurred, gained gray mask
/// with signed grain.
pub(super) fn diffuse_glow(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let gs = param(e, "graininess", 0.0, 10.0) / 20.0;
    let ga = DIFFUSE_GLOW_GLOW[index(e, "glowAmount", DIFFUSE_GLOW_GLOW.len())];
    let ca = DIFFUSE_GLOW_CLEAR[index(e, "clearAmount", DIFFUSE_GLOW_CLEAR.len())];
    let mut m = px8::box_mean(&px8::gray_mean(img), w, h, 25, 25);
    px8::scale(&mut m, ga);
    px8::inverse_scale(&mut m, ca);
    let mut g = px8::box_mean(&m, w, h, 25, 25);
    let noise = rng(e).fill(img.n());
    for (g, n) in g.iter_mut().zip(&noise) {
        *g = px8::clamp255(*g as f64 + ((*n as f64 - 128.0) * gs).trunc());
    }
    let bg = px8::rgb8(e.background);
    let mut solid = img.clone();
    solid.d.chunks_exact_mut(4).for_each(|p| p[..3].copy_from_slice(&bg));
    let mut out = img.clone();
    px8::blend_mask_rgb(&solid, &g, img, &mut out);
    out
}

/// Photoshop displaces about three times further through Glass's frosted texture than the
/// reference did (median displacement 6 px at the defaults, measured on its render).
const FROSTED_GAIN: f64 = 3.0;
/// Likewise for Ocean Ripple (median 5.7 px at the defaults).
const RIPPLE_GAIN: f64 = 1.7;

/// Glass: the image seen through the chosen texture's bumps.
pub(super) fn glass(e: &GalleryEffect, img: &Img) -> Img {
    let t = Texture::from_name(e.choice("texture"));
    let mut sc = 100.0 / param(e, "scaling", 50.0, 200.0);
    if t == Texture::TinyLens {
        sc *= 32.0 / 22.2;
    }
    let mut k = GLASS_TEXTURE_FACTOR.get(t.index()).copied().unwrap_or(1.0);
    if t == Texture::Frosted && !px8::defects() {
        k *= FROSTED_GAIN;
    }
    let mut s = k * ((0.155 * param(e, "distortion", 0.0, 20.0)).exp() - 1.0);
    if e.flag("invert") {
        s = -s;
    }
    displace(img, t.plane(), sc, param(e, "smoothness", 1.0, 15.0), s)
}

/// Ocean Ripple: Glass through a magnified frosted texture, always Gaussian-smoothed.
pub(super) fn ocean_ripple(e: &GalleryEffect, img: &Img) -> Img {
    let sc = 1.0 / (1.0 + 0.1 * (param(e, "rippleSize", 1.0, 15.0) - 1.0));
    let s = 0.5 * ((0.155 * param(e, "rippleMagnitude", 0.0, 20.0)).exp() - 1.0);
    let s = if px8::defects() { s } else { s * RIPPLE_GAIN };
    displace(img, Texture::Frosted.plane(), sc, 3.5 / sc, s)
}

/// Displaces `img` by the smoothed forward texel gradient of `tex` (128 × 128, texels of
/// `1/sc` pixels) times `s`, sampling the nearest pixel.
fn displace(img: &Img, tex: &[u8], sc: f64, smooth: f64, s: f64) -> Img {
    let (w, h) = (img.w, img.h);
    let texel = |v: usize| ((sc * v as f64 + 8192.0).trunc() as i64 & (TEX as i64 - 1)) as usize;
    let tx: Vec<usize> = (0..w).map(texel).collect();
    let ty: Vec<usize> = (0..h).map(texel).collect();
    let at = |x: usize, y: usize| tex.get(y * TEX + x).copied().unwrap_or(0) as f64;
    let mut g = vec![0f32; w * h * 2];
    par_rows(&mut g, w, 2, |y, row| {
        let (y0, y1) = (ty[y], (ty[y] + 1) & (TEX - 1));
        for x in 0..w {
            let (x0, x1) = (tx[x], (tx[x] + 1) & (TEX - 1));
            let c = at(x0, y0);
            row[x * 2] = ((at(x1, y0) - c) * sc).clamp(-24.0, 24.0) as f32;
            row[x * 2 + 1] = ((at(x0, y1) - c) * sc).clamp(-24.0, 24.0) as f32;
        }
    });
    let (gx, gy) = unzip(&g);
    let gx = px8::field_smooth(&gx, w, h, smooth);
    let gy = px8::field_smooth(&gy, w, h, smooth);
    let (wm, hm) = ((w - 1) as f64, (h - 1) as f64);
    let mut out = img.clone();
    par_rows(&mut out.d, w, 4, |y, row| {
        for x in 0..w {
            let i = y * w + x;
            let sx = (x as f64 + gx[i] as f64 * s).clamp(0.0, wm) as usize;
            let sy = (y as f64 + gy[i] as f64 * s).clamp(0.0, hm) as usize;
            let j = (sy * w + sx) * 4;
            row[x * 4..x * 4 + 3].copy_from_slice(&img.d[j..j + 3]);
        }
    });
    out
}

/// Splits an interleaved pair plane.
pub(super) fn unzip(p: &[f32]) -> (Vec<f32>, Vec<f32>) {
    p.chunks_exact(2).map(|c| (c[0], c[1])).unzip()
}

/// Glowing Edges: median pre-filter, per-channel Sobel magnitude times the brightness, widened
/// by a maximum filter.
pub(super) fn glowing_edges(e: &GalleryEffect, img: &Img) -> Img {
    let r1 = param(e, "smoothness", 1.0, 15.0) as usize >> 1;
    let width = param(e, "edgeWidth", 1.0, 14.0) as usize;
    let b = param(e, "edgeBrightness", 0.0, 20.0);
    let a = px8::clipped_rank_rgb(img, r1, Clipped::Percentile(0.5));
    if px8::defects() {
        // The reference: magnitude saturated at 255 before a gain of b/10, one more pixel of width.
        let (r2, k) = (width >> 1, b / 10.0);
        let mut edges = px8::sobel_rgb(&a);
        edges.d.chunks_exact_mut(4).for_each(|p| {
            for c in &mut p[..3] {
                *c = 255 - *c;
            }
            p[3] = 255;
        });
        let f = px8::clipped_rank_rgb(&edges, r2, Clipped::Max);
        let mut out = img.clone();
        for (o, f) in out.d.chunks_exact_mut(4).zip(f.d.chunks_exact(4)) {
            for c in 0..3 {
                o[c] = px8::clamp255(f[c] as f64 * k);
            }
        }
        return out;
    }
    // Photoshop: the raw magnitude times b/6 (fitted on its render), no widening at width 2.
    let mut edges = sobel_mag(&a, |m| px8::clamp255(m * b / 6.0));
    edges.d.chunks_exact_mut(4).for_each(|p| p[3] = 255);
    let f = px8::clipped_rank_rgb(&edges, width.saturating_sub(1) >> 1, Clipped::Max);
    let mut out = img.clone();
    for (o, f) in out.d.chunks_exact_mut(4).zip(f.d.chunks_exact(4)) {
        o[..3].copy_from_slice(&f[..3]);
    }
    out
}

/// Per-channel Sobel gradient magnitude (un-normalised, clamped edges) through `f`; alpha kept.
fn sobel_mag(img: &Img, f: impl Fn(f64) -> u8 + Sync) -> Img {
    let (w, h) = (img.w, img.h);
    let mut out = img.clone();
    par_rows(&mut out.d, w, 4, |y, row| {
        let (ya, yb) = (y.saturating_sub(1), (y + 1).min(h - 1));
        for x in 0..w {
            let (xa, xb) = (x.saturating_sub(1), (x + 1).min(w - 1));
            let at = |xx: usize, yy: usize, c: usize| img.d[(yy * w + xx) * 4 + c] as f64;
            for c in 0..3 {
                let gx = at(xb, ya, c) + 2.0 * at(xb, y, c) + at(xb, yb, c) - at(xa, ya, c) - 2.0 * at(xa, y, c) - at(xa, yb, c);
                let gy = at(xa, ya, c) + 2.0 * at(x, ya, c) + at(xb, ya, c) - at(xa, yb, c) - 2.0 * at(x, yb, c) - at(xb, yb, c);
                row[x * 4 + c] = f((gx * gx + gy * gy).sqrt());
            }
        }
    });
    out
}
