//! Sketch filters built from slopes and relief kernels: Bas Relief, Chrome, Conté Crayon, Note
//! Paper, Plaster.

use super::super::px8::{self, Img};
use super::super::tables;
use super::{ink_paper, int, light_index};
use crate::artistic::GalleryEffect;
use crate::photo_util::{par_rows, par_rows2};

/// Forward-difference slopes ×0.4 (the last column/row looks back); where a slope is exactly 0
/// it falls back to a ramp across the image, −1 … 1 as in Photoshop (the reference: −0.7 … 0.7).
/// Stored f32.
fn slopes(g: &[u8], w: usize, h: usize) -> (Vec<f32>, Vec<f32>) {
    let span = if px8::defects() { 1.4 } else { 2.0 };
    let (kx, ky) = (span / w.max(1) as f64, span / h.max(1) as f64);
    let ramp = |v: usize, k: f64, s: i32| if s == 0 { v as f64 * k - span / 2.0 } else { s as f64 * 0.4 } as f32;
    let mut gx = vec![0f32; w * h];
    let mut gy = vec![0f32; w * h];
    par_rows2(&mut gx, &mut gy, w, |y, rx, ry| {
        for x in 0..w {
            let i = y * w + x;
            let c = g[i] as i32;
            let sx = if x + 1 < w {
                g[i + 1] as i32 - c
            } else if x > 0 {
                c - g[i - 1] as i32
            } else {
                0
            };
            let sy = if y + 1 < h {
                g[i + w] as i32 - c
            } else if y > 0 {
                c - g[i - w] as i32
            } else {
                0
            };
            rx[x] = ramp(x, kx, sx);
            ry[x] = ramp(y, ky, sy);
        }
    });
    (gx, gy)
}

/// The pre-blur of Bas Relief and Chrome: a 3-pass byte Gaussian of σ `round((15 − detail)/2)`,
/// none at detail 15.
fn detail_blur(g: Vec<u8>, w: usize, h: usize, detail: f64) -> Vec<u8> {
    if detail >= 15.0 {
        return g;
    }
    px8::gauss_u8(&g, w, h, px8::rhu((15.0 - detail) * 0.5).min(500.0), 3)
}

/// Bas Relief: quantised forward-difference slopes lit by the sign of the light direction,
/// blurred before (detail) and after (smoothness).
pub(in super::super) fn bas_relief(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let g = detail_blur(px8::gray_weighted(img), w, h, e.get("detail") as f64);
    let k = light_index(e.choice("light")) as f64;
    let th = (2.0 + k) * std::f64::consts::FRAC_PI_4;
    let sign = |v: f64| if v.abs() < 0.1 { 0.0 } else { v.signum() };
    let (cx, sy) = (sign(th.cos()), sign(th.sin()));
    let norm = 1.0 / (cx * cx + sy * sy);
    let (gx, gy) = slopes(&g, w, h);
    let mut r: Vec<u8> = gx
        .iter()
        .zip(&gy)
        .map(|(&gx, &gy)| {
            let lx = (cx * gx as f64).clamp(-1.0, 1.0);
            let ly = (sy * gy as f64).clamp(-1.0, 1.0);
            px8::wrap(128.0 + 127.0 * (lx + ly) * norm)
        })
        .collect();
    // Photoshop smooths about twice as much as the reference's round((s − 1)/2).
    let smooth = e.get("smoothness") as f64;
    if smooth > 1.0 {
        let sigma = if px8::defects() { px8::rhu((smooth - 1.0) * 0.5) } else { smooth - 1.0 };
        r = px8::gauss_u8(&r, w, h, sigma.min(500.0), 3);
    }
    let (fg, bg) = ink_paper(e);
    px8::colourise_direct(&r, fg, bg, img)
}

/// Chrome: the blurred gray reflected through a heavily smoothed slope field, looked up around
/// the image centre.
pub(in super::super) fn chrome(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let g = detail_blur(px8::gray_weighted(img), w, h, e.get("detail") as f64);
    let (gx, gy) = slopes(&g, w, h);
    let sigma = ((e.get("smoothness") as f64 + 4.0) * 0.42).clamp(0.0, 500.0);
    let gx = px8::gauss_f32(&gx, w, h, sigma);
    let gy = px8::gauss_f32(&gy, w, h, sigma);
    let (hw, hh) = ((w / 2) as f64, (h / 2) as f64);
    let (wm, hm) = (w.saturating_sub(1) as f64, h.saturating_sub(1) as f64);
    let mut out = img.clone();
    par_rows(&mut out.d, w, 4, |y, row| {
        for (x, px) in row.chunks_exact_mut(4).enumerate() {
            let i = y * w + x;
            let (gx, gy) = (gx[i] as f64, gy[i] as f64);
            let ix = 1.0 / (1.0 + gx * gx).sqrt();
            let iy = 1.0 / (1.0 + gy * gy).sqrt();
            let nx = -((gx * ix) * iy);
            let ny = -(ix * (gy * iy));
            let q = 1.0 / (ix * iy);
            let rx = (hw - nx * q * hw).clamp(0.0, wm) as usize;
            let ry = (hh - ny * q * hh).clamp(0.0, hm) as usize;
            let v = g[ry * w + rx];
            px[..3].copy_from_slice(&[v, v, v]);
        }
    });
    out
}

/// Conté Crayon: the texture enters both the dark (foreground) and the light (paper) masks
/// through the directional relief kernel; the paper is the background tinted toward gray.
pub(in super::super) fn conte_crayon(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let pf = (15.0 - e.get("foregroundLevel") as f64) / 4.0 + 1.0;
    let pb = (15.0 - e.get("backgroundLevel") as f64) / 4.0 + 1.0;
    // Photoshop keeps more of the image's detail and shows the texture more strongly.
    let (boost, window) = if px8::defects() { (1.0, 5) } else { (1.5, 3) };
    let wt = px8::relief_factor(e.get("relief") as f64) * boost;
    let k = light_index(e.choice("light")) + 1;
    let tex = px8::Texture::from_name(e.choice("texture"));
    let p = px8::pattern(tex, e.get("scaling") as f64, e.get("invert") >= 0.5, w, h);
    let g = px8::gray_mean(img);
    let mut m = px8::box_mean(&g, w, h, window, window);
    let mut f = px8::relief(&p, &m, w, h, k, wt);
    px8::scale(&mut f, pf);
    px8::inverse_scale(&mut f, 2.0);
    px8::invert(&mut m);
    let mut bk = px8::relief(&p, &m, w, h, k, wt);
    px8::scale(&mut bk, pb);
    px8::inverse_scale(&mut bk, 2.0);
    let (fg, bg) = ink_paper(e);
    let mut out = img.clone();
    for (i, px) in out.d.chunks_exact_mut(4).enumerate() {
        for c in 0..3 {
            let mix = |m: u8, top: f64, base: f64| if m == 0 { base } else { ((m as f64 * top + (255.0 - m as f64) * base) / 255.0).trunc() };
            let paper = mix(bk[i], 128.0, bg[c] as f64);
            px[c] = mix(f[i], paper, fg[c] as f64) as u8;
        }
    }
    out
}

/// Note Paper: a thresholded image raised to a paper floor, grained, embossed from the top, then
/// coloured foreground/background with the emboss as the HSV value.
pub(in super::super) fn note_paper(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let t = 5.1 * e.get("imageBalance") as f64;
    let gamma = e.get("graininess") as f64 / 100.0;
    let wt = tables::RELIEF_FACTOR[int(e, "relief").min(25)];
    let gray = px8::gray_mean(img);
    let mut rng = px8::Rng::new(e.seed() >> 1);
    let noise = rng.fill(img.n());
    let mut m = gray.clone();
    px8::threshold(&mut m, t, 0, 255);
    let merge = |m: &mut [u8], f: &dyn Fn(usize) -> f64, k: f64| {
        for (i, v) in m.iter_mut().enumerate() {
            let mv = *v as f64;
            *v = px8::wrap((f(i) * k + 0.5).trunc() + (mv - (mv * k + 0.5).trunc()));
        }
    };
    merge(&mut m, &|_| 255.0, 0.7);
    merge(&mut m, &|i| noise[i] as f64, gamma);
    let p = px8::relief(&m, &m, w, h, 5, wt);
    let (fg, bg) = ink_paper(e);
    let mut out = img.clone();
    for (i, px) in out.d.chunks_exact_mut(4).enumerate() {
        let c = if gray[i] as f64 >= t { bg } else { fg };
        px[..3].copy_from_slice(&c);
    }
    px8::to_hsv(&mut out);
    out.set_plane(2, &p);
    px8::from_hsv(&mut out);
    out
}

/// Plaster: a thresholded, dilated and blurred mask shaded by its arctangent slopes from the
/// light position, screened onto itself.
pub(in super::super) fn plaster(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let sm = int(e, "smoothness").min(1000);
    let g = px8::gray_mean(img);
    let mut s = px8::box_mean(&g, w, h, sm, sm);
    px8::threshold(&mut s, 5.1 * e.get("imageBalance") as f64, 0, 255);
    let g = px8::rank_rect(&s, w, h, 2, 2, 4.0);
    let s = px8::box_mean(&g, w, h, 3, 3);
    let g = px8::box_mean(&s, w, h, 3, 3);
    let res = g.clone();
    let s = px8::box_mean(&g, w, h, 5, 5);
    let g = px8::box_mean(&s, w, h, 5, 5);
    let pairs = px8::grad_pairs(&g, w, h);
    let smooth = |c: usize| {
        let p: Vec<u8> = pairs.iter().skip(c).step_by(2).map(|v| (*v).clamp(0, 255) as u8).collect();
        let t = px8::box_mean(&p, w, h, 5, 5);
        px8::box_mean(&t, w, h, 5, 5)
    };
    let (a, bh) = (smooth(0), smooth(1));
    let mode = light_index(e.choice("light"));
    let out: Vec<u8> = (0..img.n())
        .map(|i| {
            let (a, b) = (a[i] as f64, bh[i] as f64);
            let phi = match mode {
                0 => a / 255.0,
                1 => (255.0 + a - b) / 510.0,
                2 => (255.0 - b) / 255.0,
                3 => (510.0 - a - b) / 510.0,
                4 => (255.0 - a) / 255.0,
                5 => (255.0 - a + b) / 510.0,
                6 => b / 255.0,
                _ => (a + b) / 510.0,
            };
            let s = 255.0 - (255.0 * phi).trunc();
            let r = res[i] as f64;
            px8::wrap(r * (s + 255.0 - r) / 255.0)
        })
        .collect();
    let (fg, bg) = ink_paper(e);
    px8::colourise(&out, fg, bg, img)
}
