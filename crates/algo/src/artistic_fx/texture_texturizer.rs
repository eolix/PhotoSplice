//! Texturizer: the image lit through a texture's relief.

use super::super::px8::{self, Img, TEX, Texture};
use super::param;
use crate::artistic::GalleryEffect;
use crate::photo_util::par_rows;

pub(super) fn texturizer(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let t = Texture::from_name(e.choice("texture"));
    let tex = t.plane();
    let scaling = param(e, "scaling", 50.0, 200.0);
    let sc = 100.0 / scaling;
    let rlf = param(e, "relief", 0.0, 50.0);
    let spec = px8::defects();
    // The relief's curve: a quarter per step, steeper above 35 (the reference's, at 50, leaves
    // 4 % of the pixels short of black or white).
    let steep = if spec { 2.0 } else { 6.0 };
    let mut kr = rlf / 4.0 + (steep * (rlf - 35.0)).max(0.0);
    let theta = (2.0 + light_index(e.choice("light")) as f64) * std::f64::consts::FRAC_PI_4;
    let (lx, ly) = (theta.cos(), theta.sin());
    let sigma = if e.flag("invert") { -1.0 } else { 1.0 };
    if !spec {
        // The reference's relief is as deep whatever the light's direction and the texture's
        // scale, and set per texture: ours is brought to the depth of the canvas at 100 % lit
        // from the top, then to the reference's (measured on its renders).
        let probe = |tex: &[u8], sc: f64, lx: f64, ly: f64| {
            let (gx, gy) = gradient(tex, sc, PROBE, PROBE);
            let ms = gx.iter().zip(&gy).map(|(&x, &y)| shade(x as f64, y as f64, lx, ly).powi(2)).sum::<f64>();
            (ms / (PROBE * PROBE) as f64).sqrt()
        };
        let (rms, rms0) = (probe(tex, sc, lx, ly), probe(Texture::Canvas.plane(), 1.0, 0.0, -1.0));
        if rms > 1e-9 && rms0.is_finite() {
            kr *= rms0 / rms * relief_texture(t) * relief_scaling(scaling);
        }
    }
    let (gx, gy) = gradient(tex, sc, w, h);
    let mut out = img.clone();
    par_rows(&mut out.d, w, 4, |y, row| {
        for x in 0..w {
            let i = y * w + x;
            let dot = sigma * shade(gx[i] as f64, gy[i] as f64, lx, ly);
            if !spec {
                // The reference: the shading is added as it is, as strong toward white as toward black
                // (amplitude and mean fitted on its render).
                let add = 255.0 * dot * kr * RELIEF_GAIN;
                for c in 0..3 {
                    row[x * 4 + c] = px8::clamp255(img.d[i * 4 + c] as f64 + add);
                }
                continue;
            }
            // The spec: an overlay toward white or black, weighted quadratically.
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

/// The side of the window over which the relief's depth is measured.
const PROBE: usize = 256;

/// The smoothed forward texel gradient of `tex` tiled over `w × h` pixels with texels of `1/sc`.
fn gradient(tex: &[u8], sc: f64, w: usize, h: usize) -> (Vec<f32>, Vec<f32>) {
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
    (smooth(&gx, w, h), smooth(&gy, w, h))
}

/// The light's component along the surface normal for slopes `(sx, sy)`; the light has z = 0.
fn shade(sx: f64, sy: f64, lx: f64, ly: f64) -> f64 {
    let (na, nb) = (1.0 / (1.0 + sx * sx).sqrt(), 1.0 / (1.0 + sy * sy).sqrt());
    // Normal of the tangents (1, 0, gx)·na and (0, 1, gy)·nb.
    let (nx, ny) = (-(sx * na) * nb, -(na * (sy * nb)));
    nx * lx + ny * ly
}

/// The reference's relief depth per texture, relative to the canvas.
fn relief_texture(t: Texture) -> f64 {
    match t {
        Texture::Brick => 1.22,
        Texture::Burlap => 1.07,
        Texture::Sandstone => 0.635,
        _ => 1.0,
    }
}

/// The reference's relief depth over the scaling, relative to 100 %: the deepest at 100 %, about
/// half at 50 % and three quarters at 200 % (linear in the octave in between).
fn relief_scaling(scaling: f64) -> f64 {
    let o = (scaling / 100.0).log2();
    if !o.is_finite() {
        return 1.0;
    }
    if o < 0.0 { (1.0 + 0.48 * o).max(0.0) } else { 1.0 - 0.25 * o }
}

/// Strength of the reference's relief relative to the spec's curve.
const RELIEF_GAIN: f64 = 1.3;

/// The light direction's position in the reference's list (bottom, bottom left … bottom right).
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

#[cfg(test)]
mod tests {
    use super::super::super::px8::Img;
    use crate::artistic::{GalleryEffect, GalleryFilter};

    /// The spread of the shading on a flat gray image.
    fn depth(e: &GalleryEffect) -> f64 {
        let mut img = Img::new(96, 96);
        img.d.chunks_exact_mut(4).for_each(|p| p.copy_from_slice(&[128, 128, 128, 255]));
        let out = super::texturizer(e, &img);
        let v: Vec<f64> = out.d.chunks_exact(4).map(|p| p[1] as f64).collect();
        let m = v.iter().sum::<f64>() / v.len() as f64;
        (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len() as f64).sqrt()
    }

    #[test]
    fn relief_is_as_deep_from_every_side_and_deepest_at_full_scale() {
        let mut e = GalleryEffect::new(GalleryFilter::Texturizer);
        let top = depth(&e);
        assert!(top > 2.0, "{top}");
        for light in ["left", "topRight", "bottom"] {
            assert!(e.set_choice("light", light));
            let d = depth(&e);
            assert!((0.8..1.25).contains(&(d / top)), "{light}: {d} vs {top}");
        }
        assert!(e.set_choice("light", "top"));
        for scaling in [50.0, 200.0] {
            e.set("scaling", scaling);
            let d = depth(&e);
            assert!(d < top && d > 0.3 * top, "{scaling}: {d} vs {top}");
        }
    }
}
