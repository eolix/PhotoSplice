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

/// The reference's frosted Glass, measured on its renders (median displacement 6.4 px at
/// distortion 5, smoothness 3): the gradient of the noise smoothed with σ = 0.58·smoothness,
/// normalised, `FROSTED_AMP` px at the default distortion, softly bounded by `FROSTED_SAT`.
const FROSTED_AMP: f64 = 5.8;
const FROSTED_SAT: f64 = 4.0;
const FROSTED_SIGMA: f64 = 0.58;
/// The reference's Ocean Ripple: the same field over texels of `1 + 0.1·(size − 1)` px smoothed
/// with σ = 1.67 texels, each axis bounded on its own (so the ripples lean diagonal and run flat,
/// a median of 6.3 px at the defaults), and the image scaled about the origin so that it starts
/// `RIPPLE_MARGIN`·amplitude pixels in (2 % at the defaults on a 444 px side), which keeps the
/// ripples inside it.
const RIPPLE_AMP: f64 = 6.6;
const RIPPLE_SAT: f64 = 1.4;
const RIPPLE_SIGMA: f64 = 1.67;
const RIPPLE_MARGIN: f64 = 0.75;

/// Glass: the image seen through the chosen texture's bumps.
pub(super) fn glass(e: &GalleryEffect, img: &Img) -> Img {
    let t = Texture::from_name(e.choice("texture"));
    let mut sc = 100.0 / param(e, "scaling", 50.0, 200.0);
    let smooth = param(e, "smoothness", 1.0, 15.0);
    let dist = param(e, "distortion", 0.0, 20.0);
    if t == Texture::Frosted && !px8::defects() {
        let a = FROSTED_AMP * strength(dist, 5.0);
        let (p, fx, fy) = frosted_field(1.0 / sc, FROSTED_SIGMA * smooth, FROSTED_SAT, false);
        return warp_tiled(img, p, &fx, &fy, if e.flag("invert") { -a } else { a }, 0.0);
    }
    if t == Texture::TinyLens {
        sc *= 32.0 / 22.2;
    }
    let k = GLASS_TEXTURE_FACTOR.get(t.index()).copied().unwrap_or(1.0);
    let mut s = k * ((0.155 * dist).exp() - 1.0);
    if e.flag("invert") {
        s = -s;
    }
    displace(img, t.plane(), sc, smooth, s)
}

/// Ocean Ripple: Glass through a magnified frosted texture, always smoothed.
pub(super) fn ocean_ripple(e: &GalleryEffect, img: &Img) -> Img {
    let size = param(e, "rippleSize", 1.0, 15.0);
    let mag = param(e, "rippleMagnitude", 0.0, 20.0);
    let texel = 1.0 + 0.1 * (size - 1.0);
    if !px8::defects() {
        let a = RIPPLE_AMP * strength(mag, 9.0);
        let (p, fx, fy) = frosted_field(texel, RIPPLE_SIGMA * texel, RIPPLE_SAT, true);
        return warp_tiled(img, p, &fx, &fy, a, RIPPLE_MARGIN * a);
    }
    // The spec: the forward texel gradient, clamped, smoothed and scaled.
    let sc = 1.0 / texel;
    let s = 0.5 * ((0.155 * mag).exp() - 1.0);
    displace(img, Texture::Frosted.plane(), sc, 3.5 / sc, s)
}

/// The exponential strength curve of a 0–20 slider, 1 at `at`.
fn strength(v: f64, at: f64) -> f64 {
    ((0.155 * v).exp() - 1.0) / ((0.155 * at).exp() - 1.0)
}

/// One period of the frosted displacement field: the frosted texture over texels of `texel`
/// pixels (nearest), Gaussian-smoothed with `sigma` (wrapping), its central-difference gradient
/// normalised to unit RMS and softly bounded by `sat` (`u / √(1 + |u|²/sat²)`, or each axis on
/// its own when `per_axis`). Returns the
/// period `p` and the `p × p` x and y planes; the field tiles the image from its origin.
fn frosted_field(texel: f64, sigma: f64, sat: f64, per_axis: bool) -> (usize, Vec<f32>, Vec<f32>) {
    let tex = Texture::Frosted.plane();
    let p = ((TEX as f64 * texel).round() as usize).clamp(TEX / 2, TEX * 4);
    let hgt: Vec<f64> = (0..p * p).map(|i| tex.get((i / p * TEX / p) * TEX + i % p * TEX / p).map_or(128.0, |&v| v as f64)).collect();
    let hb = blur_wrap(&blur_wrap(&hgt, p, sigma, true), p, sigma, false);
    let at = |x: usize, y: usize| hb.get((y % p) * p + x % p).copied().unwrap_or(0.0);
    let (mut gx, mut gy) = (vec![0.0; p * p], vec![0.0; p * p]);
    for y in 0..p {
        for x in 0..p {
            gx[y * p + x] = (at(x + 1, y) - at(x + p - 1, y)) * 0.5;
            gy[y * p + x] = (at(x, y + 1) - at(x, y + p - 1)) * 0.5;
        }
    }
    let ms = gx.iter().chain(&gy).map(|v| v * v).sum::<f64>() / (2 * p * p) as f64;
    let norm = if ms > 1e-12 && ms.is_finite() { 1.0 / ms.sqrt() } else { 0.0 };
    let (mut fx, mut fy) = (vec![0f32; p * p], vec![0f32; p * p]);
    for i in 0..p * p {
        let (u, v) = (gx[i] * norm, gy[i] * norm);
        let bound = |a: f64, n2: f64| a / (1.0 + n2 / (sat * sat)).sqrt();
        let (u, v) = if per_axis { (bound(u, u * u), bound(v, v * v)) } else { (bound(u, u * u + v * v), bound(v, u * u + v * v)) };
        (fx[i], fy[i]) = (u as f32, v as f32);
    }
    (p, fx, fy)
}

/// One pass of a Gaussian blur of a `p × p` plane, wrapping, along rows or columns.
fn blur_wrap(f: &[f64], p: usize, sigma: f64, rows: bool) -> Vec<f64> {
    if sigma.is_nan() || sigma <= 0.0 {
        return f.to_vec();
    }
    let r = ((3.0 * sigma).ceil() as usize).min(p);
    let k: Vec<f64> = (0..=2 * r).map(|o| (-0.5 * ((o as f64 - r as f64) / sigma).powi(2)).exp()).collect();
    let ks: f64 = k.iter().sum();
    let mut out = vec![0.0; p * p];
    for y in 0..p {
        for x in 0..p {
            let mut s = 0.0;
            for (o, kv) in k.iter().enumerate() {
                let t = (o + p * 2 - r) % p;
                let (xx, yy) = if rows { ((x + t) % p, y) } else { (x, (y + t) % p) };
                s += kv * f.get(yy * p + xx).copied().unwrap_or(0.0);
            }
            out[y * p + x] = s / ks;
        }
    }
    out
}

/// Each pixel takes the source pixel (nearest) at `margin + x·k + a·f(x)`, where `f` is the
/// tiled `p × p` field and `k = 1 − 2·margin/min(w, h)` scales the image into the margin.
fn warp_tiled(img: &Img, p: usize, fx: &[f32], fy: &[f32], a: f64, margin: f64) -> Img {
    let (w, h) = (img.w, img.h);
    let side = w.min(h) as f64;
    let m = if margin.is_finite() { margin.abs().min(side / 4.0) } else { 0.0 };
    let k = 1.0 - 2.0 * m / side.max(1.0);
    let (wm, hm) = (w.saturating_sub(1) as f64, h.saturating_sub(1) as f64);
    let mut out = img.clone();
    par_rows(&mut out.d, w, 4, |y, row| {
        let fy0 = (y % p) * p;
        let by = m + y as f64 * k;
        for x in 0..w {
            let i = fy0 + x % p;
            let (u, v) = (fx.get(i).copied().unwrap_or(0.0) as f64, fy.get(i).copied().unwrap_or(0.0) as f64);
            let sx = (m + x as f64 * k + u * a).clamp(0.0, wm) as usize;
            let sy = (by + v * a).clamp(0.0, hm) as usize;
            let j = (sy * w + sx) * 4;
            if let (Some(d), Some(s)) = (row.get_mut(x * 4..x * 4 + 3), img.d.get(j..j + 3)) {
                d.copy_from_slice(s);
            }
        }
    });
    out
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
        // The spec: magnitude saturated at 255 before a gain of b/10, one more pixel of width.
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
    // The reference: the raw magnitude times b/6 (fitted on its render), no widening at width 2.
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

#[cfg(test)]
mod tests {
    use super::super::super::px8::Img;
    use crate::artistic::{GalleryEffect, GalleryFilter as F};

    fn image(w: usize, h: usize) -> Img {
        let mut img = Img::new(w, h);
        for (i, p) in img.d.chunks_exact_mut(4).enumerate() {
            p.copy_from_slice(&[(i * 37 % 256) as u8, (i * 11 % 256) as u8, (i * 5 % 256) as u8, 255]);
        }
        img
    }

    #[test]
    fn zero_strength_leaves_the_image_alone() {
        let img = image(61, 47);
        let mut g = GalleryEffect::new(F::Glass);
        g.set_choice("texture", "frosted");
        g.set("distortion", 0.0);
        assert_eq!(super::glass(&g, &img), img);
        let mut o = GalleryEffect::new(F::OceanRipple);
        o.set("rippleMagnitude", 0.0);
        assert_eq!(super::ocean_ripple(&o, &img), img);
    }

    #[test]
    fn frosted_field_is_bounded_and_finite() {
        for (texel, sigma, sat, per_axis) in [(0.5, 0.58, 4.0, false), (2.4, 4.0, 1.4, true), (1.0, 8.7, 4.0, false)] {
            let (p, fx, fy) = super::frosted_field(texel, sigma, sat, per_axis);
            assert_eq!((fx.len(), fy.len()), (p * p, p * p));
            let lim = sat as f32 * 1.0001;
            assert!(fx.iter().chain(&fy).all(|v| v.is_finite() && v.abs() <= lim));
            assert!(fx.iter().any(|v| v.abs() > 0.1), "the field is not flat");
        }
    }
}
