//! Grain: ten kinds of film grain from the seeded gallery generator.

use super::super::px8::{self, Img};
use super::{param, rng, with_value};
use crate::artistic::GalleryEffect;

/// The grain looks, in menu order.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Regular,
    Soft,
    Sprinkles,
    Clumped,
    Contrasty,
    Enlarged,
    Stippled,
    Horizontal,
    Vertical,
    Speckle,
}

pub(super) fn grain(e: &GalleryEffect, img: &Img) -> Img {
    let kind = match e.choice("grainType") {
        "soft" => Kind::Soft,
        "sprinkles" => Kind::Sprinkles,
        "clumped" => Kind::Clumped,
        "contrasty" => Kind::Contrasty,
        "enlarged" => Kind::Enlarged,
        "stippled" => Kind::Stippled,
        "horizontal" => Kind::Horizontal,
        "vertical" => Kind::Vertical,
        "speckle" => Kind::Speckle,
        _ => Kind::Regular,
    };
    let i = param(e, "intensity", 0.0, 100.0).trunc();
    let c = param(e, "contrast", 0.0, 100.0).trunc();
    let f = i / 100.0;
    let speckle = kind == Kind::Speckle;
    // Contrast: as set for the first four looks, halved around 50 for the others.
    let cc = if matches!(kind, Kind::Regular | Kind::Soft | Kind::Sprinkles | Kind::Clumped) { c } else { (c as i64 >> 1) as f64 + 50.0 };
    let lut = px8::bc_lut(if speckle { 75.0 } else { 50.0 }, cc);
    let adjust = |img: &mut Img| {
        if cc != 50.0 {
            quirky_max_lut(img, &lut);
        }
    };
    let (fg, bg) = (px8::rgb8(e.foreground), px8::rgb8(e.background));
    let n = img.n();
    let mut r = rng(e);
    match kind {
        Kind::Regular => {
            let mut out = img.clone();
            add_noise(&mut out, &r.fill(4 * n), f);
            adjust(&mut out);
            out
        }
        Kind::Soft => {
            let spread = (i as i64 >> 1) as f64;
            let mut out = img.clone();
            for p in out.d.chunks_exact_mut(4) {
                for v in &mut p[..3] {
                    let s: f64 = (0..12).map(|_| r.next()).sum();
                    *v = px8::clamp255(spread * (s - 6.0) + *v as f64);
                }
            }
            adjust(&mut out);
            out
        }
        Kind::Sprinkles => {
            // The spec sprinkled the foreground; the reference sprinkles the background colour.
            let col = if px8::defects() { fg } else { bg };
            let mut out = img.clone();
            for p in out.d.chunks_exact_mut(4) {
                if r.next() < f {
                    p[..3].copy_from_slice(&col);
                }
            }
            adjust(&mut out);
            out
        }
        // The spec drew Contrasty with the Clumped routine.
        Kind::Clumped | Kind::Contrasty if kind == Kind::Clumped || px8::defects() => {
            let noise = Img { w: img.w, h: img.h, d: r.fill(4 * n) };
            let noise = px8::rank_rect_rgb(&noise, 3, 3, 4.0);
            let mut a = img.clone();
            add_noise(&mut a, &noise.d, f);
            let mut out = px8::rank_rect_rgb(&a, 5, 5, 13.0);
            adjust(&mut out);
            out
        }
        Kind::Clumped | Kind::Enlarged | Kind::Contrasty => {
            let mut a = img.clone();
            add_noise(&mut a, &r.fill(4 * n), f);
            let mut out = px8::rank_rect_rgb(&a, 3, 3, 4.0);
            if kind == Kind::Contrasty {
                adjust(&mut out);
            }
            out
        }
        Kind::Stippled => {
            let bias = 2.0 * i - 100.0;
            let g = px8::gray_mean(img);
            let mut out = img.clone();
            for (p, g) in out.d.chunks_exact_mut(4).zip(g) {
                let t = ((bias - g as f64 + 255.0) * 128.5 + 0.5) / 32767.0;
                p[..3].copy_from_slice(if r.next() <= t { &bg } else { &fg });
            }
            out
        }
        Kind::Horizontal | Kind::Vertical => {
            let (w, h) = (img.w, img.h);
            let mut a = px8::gray_mean(img);
            for (a, n) in a.iter_mut().zip(r.fill(n)) {
                *a = px8::clamp255(*a as f64 + (n as f64 - 128.0) * f);
            }
            let streak: Vec<f64> = if kind == Kind::Vertical {
                (0..w).map(|_| (r.next() * 255.0).trunc()).collect()
            } else {
                (0..h).map(|_| (r.next() * 255.0).trunc()).collect()
            };
            for (k, a) in a.iter_mut().enumerate() {
                let s = if kind == Kind::Vertical { streak[k % w] } else { streak[k / w] };
                *a = px8::clamp255(*a as f64 + (s - 128.0) * f);
            }
            let y = px8::box_mean_rgb(&px8::box_mean_rgb(img, 9, 9), 9, 9);
            let mut b = px8::box_mean(&a, w, h, 2, 2);
            px8::apply_lut(&mut b, &px8::bc_lut(50.0, cc));
            with_value(&y, &px8::box_mean(&b, w, h, 2, 2))
        }
        Kind::Speckle => {
            let (w, h) = (img.w, img.h);
            let v = px8::sharpen4(&px8::max_channel(img), w, h, 0.05 * i + 1.0);
            let mut v = px8::rank_rect(&v, w, h, 2, 2, 1.0);
            px8::apply_lut(&mut v, &lut);
            with_value(img, &v)
        }
    }
}

/// Adds `(n − 128)·f` to R, G and B (`n` holds four bytes per pixel).
fn add_noise(img: &mut Img, n: &[u8], f: f64) {
    for (p, n) in img.d.chunks_exact_mut(4).zip(n.chunks_exact(4)) {
        for c in 0..3 {
            p[c] = px8::clamp255(p[c] as f64 + (n[c] as f64 - 128.0) * f);
        }
    }
}

/// Hue-keeping tone curve (P-17 quirky-max LUT): each channel scaled by `lut[m]/m`, where `m`
/// is R, raised to G or B when either exceeds R.
fn quirky_max_lut(img: &mut Img, lut: &[u8; 256]) {
    for p in img.d.chunks_exact_mut(4) {
        let mut m = p[0];
        if p[1] > p[0] {
            m = p[1];
        }
        if p[2] > p[0] {
            m = p[2];
        }
        let l = lut[m as usize] as f64;
        for c in &mut p[..3] {
            *c = if l != 0.0 && m != 0 { px8::wrap((l * *c as f64 / m as f64).trunc()) } else { 0 };
        }
    }
}
