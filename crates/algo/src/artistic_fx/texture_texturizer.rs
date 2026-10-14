//! Texturizer: the image lit through a texture's relief.

use super::super::px8::{self, Img, TEX, Texture};
use super::param;
use crate::artistic::GalleryEffect;
use crate::photo_util::par_rows;

pub(super) fn texturizer(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let tex = Texture::from_name(e.choice("texture")).plane();
    let sc = 100.0 / param(e, "scaling", 50.0, 200.0);
    let rlf = param(e, "relief", 0.0, 50.0);
    let kr = rlf / 4.0 + (2.0 * (rlf - 35.0)).max(0.0);
    let reference = px8::defects();
    let theta = (2.0 + light_index(e.choice("light")) as f64) * std::f64::consts::FRAC_PI_4;
    let (lx, ly) = (theta.cos(), theta.sin());
    let sigma = if e.flag("invert") { -1.0 } else { 1.0 };
    let samp = |u: f64, v: f64| px8::sample_wrap(tex, TEX, TEX, u, v);
    let mut g = vec![0f32; w * h * 2];
    par_rows(&mut g, w, 2, |y, row| {
        for x in 0..w {
            let (u, v) = (sc * x as f64, sc * y as f64);
            let c = samp(u, v);
            row[x * 2] = ((samp(u + 1.0, v) - c) * sc / 255.0).clamp(-256.0, 256.0) as f32;
            row[x * 2 + 1] = ((samp(u, v + 1.0) - c) * sc / 255.0).clamp(-256.0, 256.0) as f32;
        }
    });
    let (gx, gy) = super::distort::unzip(&g);
    let gx = smooth(&gx, w, h);
    let gy = smooth(&gy, w, h);
    let mut out = img.clone();
    par_rows(&mut out.d, w, 4, |y, row| {
        for x in 0..w {
            let i = y * w + x;
            let (sx, sy) = (gx[i] as f64, gy[i] as f64);
            let (na, nb) = (1.0 / (1.0 + sx * sx).sqrt(), 1.0 / (1.0 + sy * sy).sqrt());
            // Normal of the tangents (1, 0, gx)·na and (0, 1, gy)·nb; the light has z = 0.
            let (nx, ny) = (-(sx * na) * nb, -(na * (sy * nb)));
            let dot = sigma * (nx * lx + ny * ly);
            if !reference {
                // Photoshop: the shading is added as it is, as strong toward white as toward black
                // (amplitude and mean fitted on its render).
                let add = 255.0 * dot * kr * RELIEF_GAIN;
                for c in 0..3 {
                    row[x * 4 + c] = px8::clamp255(img.d[i * 4 + c] as f64 + add);
                }
                continue;
            }
            // The reference: an overlay toward white or black, weighted quadratically.
            let (lit, m) = if dot < 0.0 { (0.0, (-dot * kr).min(1.0)) } else { (1.0, (dot * kr).min(1.0)) };
            for c in 0..3 {
                let b = img.d[i * 4 + c] as f64 / 255.0;
                let hl = if b <= 0.5 { 2.0 * b * lit } else { lit + (2.0 * b - 1.0) - lit * (2.0 * b - 1.0) };
                row[x * 4 + c] = px8::wrap(255.0 * ((1.0 - m) * b + m * (m * lit + (1.0 - m) * hl)));
            }
        }
    });
    out
}

/// Strength of Photoshop's relief relative to the reference's curve.
const RELIEF_GAIN: f64 = 1.3;

/// The light direction's position in Photoshop's list (bottom, bottom left … bottom right).
fn light_index(name: &str) -> usize {
    match name {
        "bottomLeft" => 1,
        "left" => 2,
        "topLeft" => 3,
        "top" => 4,
        "topRight" => 5,
        "right" => 6,
        "bottomRight" => 7,
        _ => 0,
    }
}

/// Raw 3 × 3 smoothing (1 2 1 / 2 6 2 / 1 2 1, P-25) of interior pixels; the border is kept.
fn smooth(p: &[f32], w: usize, h: usize) -> Vec<f32> {
    const K: [f64; 9] = [1.0, 2.0, 1.0, 2.0, 6.0, 2.0, 1.0, 2.0, 1.0];
    let mut out = p.to_vec();
    if w < 3 || h < 3 {
        return out;
    }
    par_rows(&mut out, w, 1, |y, row| {
        if y == 0 || y + 1 >= h {
            return;
        }
        for x in 1..w - 1 {
            let mut acc = 0.0;
            for j in 0..3 {
                for i in 0..3 {
                    acc += K[j * 3 + i] / 18.0 * p[(y + j - 1) * w + x + i - 1] as f64;
                }
            }
            row[x] = acc as f32;
        }
    });
    out
}
