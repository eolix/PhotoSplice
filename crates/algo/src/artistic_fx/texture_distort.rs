//! Diffuse Glow, Glass, Ocean Ripple (Distort) and Glowing Edges (Stylize).

use super::super::px8::{self, Clipped, Img, TEX, Texture};
use super::super::response;
use super::{index, param, rng};
use crate::artistic::GalleryEffect;
use crate::photo_util::par_rows;

/// Diffuse Glow: the background colour glows through a doubly box-blurred, gained gray mask
/// with signed grain.
pub(super) fn diffuse_glow(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let gs = param(e, "graininess", 0.0, 10.0) / 20.0;
    let ga = response::diffuse_glow_glow(index(e, "glowAmount", 21));
    let ca = response::diffuse_glow_clear(index(e, "clearAmount", 21));
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
/// Where the reference's glass bumps are flat (smoothness 15, distortion 1) its render is the
/// image shifted one pixel down and right: it samples half a pixel further up and left than the
/// nearest-below pixel of ours. Where the bumps are deep its displacements are no longer biased
/// that way, so the offset fades out between 1 and 4 px of depth.
const GLASS_OFFSET: f64 = -0.5;
/// The rate of the reference's distortion curve: 7.2 times the default depth at 20.
const FROSTED_RATE: f64 = 0.069;
/// The reference's Ocean Ripple: the same field over texels of `1 + 0.1·(size − 1)` px smoothed
/// with σ = 1.67 texels, each axis bounded on its own (so the ripples lean diagonal and run flat,
/// a median of 6.3 px at the defaults), its depth set by the size ([`ripple_depth_size`]). The
/// reference also magnifies the image about a point `RIPPLE_CENTER` px in from the origin, by a
/// fraction that follows the magnitude's curve (2.3 % at the defaults, about 13 % at magnitude
/// 20, on any image size), less for larger ripples ([`ripple_zoom_size`]).
const RIPPLE_AMP: f64 = 6.6;
const RIPPLE_SAT: f64 = 1.4;
const RIPPLE_SIGMA: f64 = 1.67;
const RIPPLE_ZOOM: f64 = 0.023 / RIPPLE_AMP;
const RIPPLE_CENTER: f64 = 230.0;
/// The rate of the reference's magnitude curve: 6.5 times the default depth at 20 (its spread of
/// displacements there, measured, is 7 % below the 0.155 rate's).
const RIPPLE_RATE: f64 = 0.147;

/// Glass: the image seen through the chosen texture's bumps.
pub(super) fn glass(e: &GalleryEffect, img: &Img) -> Img {
    let t = Texture::from_name(e.choice("texture"));
    let mut sc = 100.0 / param(e, "scaling", 50.0, 200.0);
    let smooth = param(e, "smoothness", 1.0, 15.0);
    let dist = param(e, "distortion", 0.0, 20.0);
    if t == Texture::Frosted && !px8::defects() {
        // Deeper for larger bumps (the square root of the scaling) and for sharper ones.
        let a = FROSTED_AMP * curve(dist, 5.0, FROSTED_RATE) * frosted_smooth_depth(smooth) / sc.sqrt();
        let (p, fx, fy) = frosted_field(1.0 / sc, FROSTED_SIGMA * smooth, FROSTED_SAT, false);
        let off = if a == 0.0 { 0.0 } else { GLASS_OFFSET * ((4.0 - a) / 3.0).clamp(0.0, 1.0) };
        return warp_tiled(img, p, &fx, &fy, if e.flag("invert") { -a } else { a }, 0.0, off);
    }
    if t == Texture::TinyLens {
        sc *= 32.0 / 22.2;
    }
    let k = response::glass_texture_factor(t);
    let mut s = k * ((0.155 * dist).exp() - 1.0);
    if e.flag("invert") {
        s = -s;
    }
    displace(img, t.plane(), sc, smooth, s)
}

/// The depth of the reference's frosted glass relative to the default smoothness, 3: the
/// smoother the bumps, the flatter they are (about 4 times deeper at 1, nearly flat at 15).
fn frosted_smooth_depth(smooth: f64) -> f64 {
    const C: f64 = 5.0 / 3.0;
    (C + 9.0) / (C + smooth * smooth)
}

/// Ocean Ripple: Glass through a magnified frosted texture, always smoothed.
pub(super) fn ocean_ripple(e: &GalleryEffect, img: &Img) -> Img {
    let size = param(e, "rippleSize", 1.0, 15.0);
    let mag = param(e, "rippleMagnitude", 0.0, 20.0);
    let texel = 1.0 + 0.1 * (size - 1.0);
    if !px8::defects() {
        // The ripples' depth and the magnification follow the magnitude's curve, and each
        // changes with the size on its own curve.
        let k = RIPPLE_AMP * curve(mag, 9.0, RIPPLE_RATE);
        let (p, fx, fy) = frosted_field(texel, RIPPLE_SIGMA * texel, RIPPLE_SAT, true);
        return warp_tiled(img, p, &fx, &fy, k * ripple_depth_size(size), RIPPLE_ZOOM * k * ripple_zoom_size(size), 0.0);
    }
    // The spec: the forward texel gradient, clamped, smoothed and scaled.
    let sc = 1.0 / texel;
    let s = 0.5 * ((0.155 * mag).exp() - 1.0);
    displace(img, Texture::Frosted.plane(), sc, 3.5 / sc, s)
}

/// The reference's ripple depth relative to the default size, 9: the spread of its displacements,
/// measured on its parrot renders, is 0.70 times the default's at size 1, the same at 8 and 0.85
/// at 15. The boat's renders favour a deeper ripple at 15, so the curve meets them at 0.935.
fn ripple_depth_size(size: f64) -> f64 {
    (0.6314 + 0.07202 * size - 0.003452 * size * size).max(0.0)
}

/// The reference's magnification relative to the default ripple size (measured 1.25 at size 1,
/// 1 at 9 and 0.45 at 15).
fn ripple_zoom_size(size: f64) -> f64 {
    (1.2424 + 0.0119 * size - 0.004315 * size * size).max(0.0)
}

/// `(e^(rate·v) − 1) / (e^(rate·at) − 1)`: 0 at 0, 1 at `at`.
fn curve(v: f64, at: f64, rate: f64) -> f64 {
    ((rate * v).exp() - 1.0) / ((rate * at).exp() - 1.0)
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

/// Each pixel takes the source pixel (nearest below) at `c·z + x·(1 − z) + a·f(x) + off`, where
/// `f` is the tiled `p × p` field, `z` the magnification and `c` = `RIPPLE_CENTER` its fixed
/// point.
fn warp_tiled(img: &Img, p: usize, fx: &[f32], fy: &[f32], a: f64, zoom: f64, off: f64) -> Img {
    let (w, h) = (img.w, img.h);
    let z = if zoom.is_finite() { zoom.clamp(0.0, 0.5) } else { 0.0 };
    let (m, k) = (RIPPLE_CENTER * z + off, 1.0 - z);
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

/// Glowing Edges: a median pre-filter, then each channel's local range (maximum − minimum) times
/// the brightness.
pub(super) fn glowing_edges(e: &GalleryEffect, img: &Img) -> Img {
    let smooth = param(e, "smoothness", 1.0, 15.0) as usize;
    let width = param(e, "edgeWidth", 1.0, 14.0) as usize;
    let b = param(e, "edgeBrightness", 0.0, 20.0);
    if px8::defects() {
        // The spec: a clipped median, the Sobel magnitude saturated at 255 before a gain of
        // b/10, widened by a maximum filter one pixel wider.
        let a = px8::clipped_rank_rgb(img, smooth >> 1, Clipped::Percentile(0.5));
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
    // The reference (matched to within 0.1 level per pixel on its renders at every slider's
    // ends): the median over a `smoothness`-square window, then the range over a window one
    // wider than the edge width, times half the brightness. Both windows clamp at the edges, and
    // an even one reaches one further up and left.
    let a = if smooth > 1 { px8::rank_rect_rgb(img, smooth, smooth, (smooth * smooth) as f64 * 0.5) } else { img.clone() };
    let f = morph_range(&a, width + 1, b / 2.0);
    let mut out = img.clone();
    for (o, f) in out.d.chunks_exact_mut(4).zip(f.d.chunks_exact(4)) {
        o[..3].copy_from_slice(&f[..3]);
    }
    out
}

/// The per-channel range (maximum − minimum) of `img` over an `n × n` window (an even window
/// reaches one further up and left; edges clamped), times `gain`, truncated; alpha kept.
fn morph_range(img: &Img, n: usize, gain: f64) -> Img {
    let (w, h) = (img.w, img.h);
    let lo = (n >> 1) as i64;
    let hi = n.saturating_sub(1) as i64 - lo;
    let clampi = |v: i64, m: usize| v.clamp(0, m as i64 - 1) as usize;
    // Rows first: each channel's maximum and minimum over the row's window.
    let mut hx = vec![[0u8; 6]; w * h];
    par_rows(&mut hx, w, 1, |y, row| {
        for (x, m) in row.iter_mut().enumerate() {
            *m = [0, 0, 0, 255, 255, 255];
            for k in -lo..=hi {
                let j = (y * w + clampi(x as i64 + k, w)) * 4;
                for c in 0..3 {
                    let v = img.d.get(j + c).copied().unwrap_or(0);
                    m[c] = m[c].max(v);
                    m[c + 3] = m[c + 3].min(v);
                }
            }
        }
    });
    let mut out = img.clone();
    par_rows(&mut out.d, w, 4, |y, row| {
        for x in 0..w {
            let mut m = [0u8, 0, 0, 255, 255, 255];
            for k in -lo..=hi {
                if let Some(v) = hx.get(clampi(y as i64 + k, h) * w + x) {
                    for c in 0..3 {
                        m[c] = m[c].max(v[c]);
                        m[c + 3] = m[c + 3].min(v[c + 3]);
                    }
                }
            }
            for c in 0..3 {
                row[x * 4 + c] = px8::clamp255(m[c].saturating_sub(m[c + 3]) as f64 * gain);
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
    fn frosted_glass_flattens_as_it_smooths() {
        let img = image(80, 64);
        // How far each pixel came from (the nearest source pixel with its colour).
        let (w, h) = (img.w as i64, img.h as i64);
        let reach = |smooth: f32| {
            let mut g = GalleryEffect::new(F::Glass);
            g.set_choice("texture", "frosted");
            g.set("smoothness", smooth);
            let out = super::glass(&g, &img);
            let mut total = 0i64;
            for y in 0..h {
                for x in 0..w {
                    let at = |xx: i64, yy: i64| ((yy * w + xx) * 4) as usize;
                    let px = &out.d[at(x, y)..at(x, y) + 3];
                    let d = (0..=12i64)
                        .find(|&r| {
                            (-r..=r).any(|dy| {
                                (-r..=r).any(|dx| {
                                    let (sx, sy) = (x + dx, y + dy);
                                    (0..w).contains(&sx) && (0..h).contains(&sy) && &img.d[at(sx, sy)..at(sx, sy) + 3] == px
                                })
                            })
                        })
                        .unwrap_or(13);
                    total += d;
                }
            }
            total
        };
        let (sharp, default, smooth) = (reach(1.0), reach(3.0), reach(15.0));
        assert!(sharp > default && default > smooth, "{sharp} {default} {smooth}");
        // The flattest glass moves pixels about one pixel on average.
        assert!(smooth * 5 <= w * h * 6, "{smooth}");
    }

    #[test]
    fn glowing_edges_lights_a_square_one_wider_than_the_edge() {
        let mut img = Img::new(21, 21);
        img.d.chunks_exact_mut(4).for_each(|p| p.copy_from_slice(&[40, 40, 40, 255]));
        img.d[(10 * 21 + 10) * 4..(10 * 21 + 10) * 4 + 3].copy_from_slice(&[140, 140, 140]);
        let mut g = GalleryEffect::new(F::GlowingEdges);
        g.set("smoothness", 1.0);
        g.set("edgeWidth", 4.0);
        g.set("edgeBrightness", 2.0);
        let out = super::glowing_edges(&g, &img);
        let lit: Vec<(usize, usize)> = (0..21 * 21).filter(|i| out.d[i * 4] > 0).map(|i| (i % 21, i / 21)).collect();
        assert_eq!(lit.len(), 25);
        assert!(lit.iter().all(|&(x, y)| (8..=12).contains(&x) && (8..=12).contains(&y)));
        assert!(lit.iter().all(|&(x, y)| out.d[(y * 21 + x) * 4] == 100));
    }

    #[test]
    fn ocean_ripple_magnifies_less_for_larger_ripples() {
        assert!((super::ripple_zoom_size(9.0) - 1.0).abs() < 1e-3);
        assert!(super::ripple_zoom_size(1.0) > 1.0 && super::ripple_zoom_size(15.0) < 1.0);
        assert!((1..=15).all(|s| super::ripple_zoom_size(s as f64) >= 0.0));
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
