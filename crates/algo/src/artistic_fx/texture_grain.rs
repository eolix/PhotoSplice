//! Grain: ten kinds of film grain from the seeded gallery generator.
//!
//! The app draws each type the way the reference does ([`reference`]). The spec's routines,
//! with its cross-wired menu, its truncations and its wrapping tone curve, stay reachable
//! behind [`px8::defects`] ([`spec`]).

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
    // Contrast: as set for the first four looks, halved around 50 for the others.
    let cc = if matches!(kind, Kind::Regular | Kind::Soft | Kind::Sprinkles | Kind::Clumped) { c } else { (c as i64 >> 1) as f64 + 50.0 };
    let b = if kind == Kind::Speckle { 75.0 } else { 50.0 };
    let spec_only = px8::defects();
    let lut = if spec_only { px8::bc_lut(b, cc) } else { bc_lut_exact(b, cc) };
    let g = Grain { kind, i, cc, lut, fg: px8::rgb8(e.foreground), bg: px8::rgb8(e.background) };
    if spec_only { spec(&g, e, img) } else { reference(&g, e, img) }
}

/// The brightness/contrast line of [`px8::bc_lut`] without its fixed-point truncations (which
/// darken it by about a level at low contrast), rounded: the reference's tones.
fn bc_lut_exact(b: f64, c: f64) -> [u8; 256] {
    let b2 = if b > 5.0 { (100.0 - b.max(4.0)) / 100.0 } else { 0.95 };
    let c2 = if c != 100.0 { c.max(4.0) / 100.0 } else { 0.9995 };
    let slope = if c2 > 0.5 { 0.5 / (1.0 - c2) } else { 2.0 * c2 };
    let mut lut = [0u8; 256];
    for (v, l) in lut.iter_mut().enumerate() {
        *l = px8::sat(256.0 * (0.5 - b2 * slope) + slope * v as f64);
    }
    lut
}

/// The settings both renderers share.
struct Grain {
    kind: Kind,
    /// Intensity, 0–100.
    i: f64,
    /// The contrast the type uses.
    cc: f64,
    /// The brightness/contrast table for `cc`.
    lut: [u8; 256],
    fg: [u8; 3],
    bg: [u8; 3],
}

/// The reference's grain, as measured on its renders: the additive types add noise truncated
/// toward zero (flat areas keep their mean); Contrasty is Clumped with the contrast boost, and
/// Enlarged boosts the noisy image before its 3 × 3 rank filter; the tone curve saturates
/// rather than wraps; Stippled puts the background colour where the image is light; the
/// streaks are smoothed less along their length; value replacements keep the hue exactly.
fn reference(g: &Grain, e: &GalleryEffect, img: &Img) -> Img {
    let (i, cc) = (g.i, g.cc);
    let f = i / 100.0;
    let n = img.n();
    let mut r = rng(e);
    let tone = |img: &mut Img| {
        if cc != 50.0 {
            tone_curve(img, &g.lut);
        }
    };
    match g.kind {
        Kind::Regular => {
            let mut out = img.clone();
            add_noise_exact(&mut out, &r.fill(4 * n), f);
            tone(&mut out);
            out
        }
        Kind::Soft => {
            let spread = (i as i64 >> 1) as f64;
            let mut out = img.clone();
            for p in out.d.chunks_exact_mut(4) {
                for v in &mut p[..3] {
                    let s: f64 = (0..12).map(|_| r.next()).sum();
                    *v = px8::clamp255(*v as f64 + (spread * (s - 6.0)).trunc());
                }
            }
            tone(&mut out);
            out
        }
        Kind::Sprinkles => {
            let mut out = img.clone();
            for p in out.d.chunks_exact_mut(4) {
                if r.next() < f {
                    p[..3].copy_from_slice(&g.bg);
                }
            }
            tone(&mut out);
            out
        }
        Kind::Clumped | Kind::Contrasty => {
            let noise = Img { w: img.w, h: img.h, d: r.fill(4 * n) };
            let noise = px8::rank_rect_rgb(&noise, 3, 3, 4.0);
            let mut a = img.clone();
            add_noise_exact(&mut a, &noise.d, f);
            let mut out = px8::rank_rect_rgb(&a, 5, 5, 13.0);
            tone(&mut out);
            out
        }
        Kind::Enlarged => {
            let mut a = img.clone();
            add_noise_exact(&mut a, &r.fill(4 * n), f);
            tone(&mut a);
            px8::rank_rect_rgb(&a, 3, 3, 4.0)
        }
        Kind::Stippled => {
            // The chance of the background colour, as measured on the reference: 20/256 even
            // in black, rising with a soft maximum (the quadratic mean) of per-channel ramps,
            // so a saturated colour reads lighter than its gray. Intensity slides the ramps
            // toward the light, as the spec did, and scales the floor.
            let shift = 2.0 * i - 80.0;
            let (lo, hi) = (52.0 - shift, 183.0 - shift);
            let floor = 0.195 * f; // 20/256 at the default intensity, 40
            let ramp = |v: u8| ((v as f64 - lo) / (hi - lo)).clamp(0.0, 1.0).powi(2);
            let mut out = img.clone();
            for p in out.d.chunks_exact_mut(4) {
                let m = ((ramp(p[0]) + ramp(p[1]) + ramp(p[2])) / 3.0).sqrt();
                let t = floor + (1.0 - floor) * m;
                p[..3].copy_from_slice(if r.next() < t { &g.bg } else { &g.fg });
            }
            out
        }
        Kind::Horizontal | Kind::Vertical => {
            let vertical = g.kind == Kind::Vertical;
            let (w, h) = (img.w, img.h);
            let mut a = px8::gray_mean(img);
            for (a, n) in a.iter_mut().zip(r.fill(n)) {
                *a = px8::clamp255(*a as f64 + (n as f64 - 128.0) * f);
            }
            let streak: Vec<f64> = (0..if vertical { w } else { h }).map(|_| (r.next() * 255.0).trunc()).collect();
            for (k, a) in a.iter_mut().enumerate() {
                let s = if vertical { streak[k % w] } else { streak[k / w] };
                *a = px8::clamp255(*a as f64 + (s - 128.0) * f);
            }
            // The colour comes from the twice 9 × 9 blurred image. The value is smoothed by
            // three across the streaks ([1 2 1]) but only two along them, as the reference's
            // autocorrelation shows.
            let y = px8::box_mean_rgb(&px8::box_mean_rgb(img, 9, 9), 9, 9);
            let mut b = px8::box_mean(&a, w, h, 2, 2);
            px8::apply_lut(&mut b, &px8::bc_lut(50.0, cc));
            let (kw, kh) = if vertical { (2, 1) } else { (1, 2) };
            scale_to_value(&y, &px8::box_mean(&b, w, h, kw, kh))
        }
        Kind::Speckle => {
            let (w, h) = (img.w, img.h);
            let v = px8::sharpen4(&px8::max_channel(img), w, h, 0.05 * i + 1.0);
            let mut v = px8::rank_rect(&v, w, h, 2, 2, 1.0);
            px8::apply_lut(&mut v, &g.lut);
            scale_to_value(img, &v)
        }
    }
}

/// The spec's grain, kept as written for the comparison against its renders.
fn spec(g: &Grain, e: &GalleryEffect, img: &Img) -> Img {
    let (kind, i, cc) = (g.kind, g.i, g.cc);
    let f = i / 100.0;
    let lut = &g.lut;
    let adjust = |img: &mut Img| {
        if cc != 50.0 {
            quirky_max_lut(img, lut);
        }
    };
    let (fg, bg) = (g.fg, g.bg);
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
            let mut out = img.clone();
            for p in out.d.chunks_exact_mut(4) {
                if r.next() < f {
                    p[..3].copy_from_slice(&fg);
                }
            }
            adjust(&mut out);
            out
        }
        // The spec drew Contrasty with the Clumped routine.
        Kind::Clumped | Kind::Contrasty => {
            let noise = Img { w: img.w, h: img.h, d: r.fill(4 * n) };
            let noise = px8::rank_rect_rgb(&noise, 3, 3, 4.0);
            let mut a = img.clone();
            add_noise(&mut a, &noise.d, f);
            let mut out = px8::rank_rect_rgb(&a, 5, 5, 13.0);
            adjust(&mut out);
            out
        }
        Kind::Enlarged => {
            let mut a = img.clone();
            add_noise(&mut a, &r.fill(4 * n), f);
            px8::rank_rect_rgb(&a, 3, 3, 4.0)
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
            px8::apply_lut(&mut v, lut);
            with_value(img, &v)
        }
    }
}

/// Adds `(n − 128)·f` to R, G and B, the spec's way: the sum truncated (`n` holds four bytes
/// per pixel).
fn add_noise(img: &mut Img, n: &[u8], f: f64) {
    for (p, n) in img.d.chunks_exact_mut(4).zip(n.chunks_exact(4)) {
        for c in 0..3 {
            p[c] = px8::clamp255(p[c] as f64 + (n[c] as f64 - 128.0) * f);
        }
    }
}

/// Adds `(n − 128)·f`, truncated toward zero, to R, G and B: the reference's rounding, which
/// leaves flat areas' mean unchanged (`n` holds four bytes per pixel).
fn add_noise_exact(img: &mut Img, n: &[u8], f: f64) {
    for (p, n) in img.d.chunks_exact_mut(4).zip(n.chunks_exact(4)) {
        for c in 0..3 {
            p[c] = px8::clamp255(p[c] as f64 + ((n[c] as f64 - 128.0) * f).trunc());
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

/// Hue-keeping tone curve: each channel scaled by `lut[m]/m`, `m` the largest channel,
/// truncated and kept in range (the spec's version wraps).
fn tone_curve(img: &mut Img, lut: &[u8; 256]) {
    for p in img.d.chunks_exact_mut(4) {
        let m = p[0].max(p[1]).max(p[2]);
        let l = lut[m as usize] as f64;
        for c in &mut p[..3] {
            *c = if m == 0 { l as u8 } else { px8::clamp255(l * *c as f64 / m as f64) };
        }
    }
}

/// `img` with its HSV value replaced by `v`, hue and saturation kept exactly: each channel
/// scaled by `v/max`, rounded; black becomes gray `v`. Alpha kept.
fn scale_to_value(img: &Img, v: &[u8]) -> Img {
    let mut out = img.clone();
    for (p, &v) in out.d.chunks_exact_mut(4).zip(v) {
        let m = p[0].max(p[1]).max(p[2]);
        if m == 0 {
            p[..3].fill(v);
        } else {
            let k = v as f64 / m as f64;
            for c in &mut p[..3] {
                *c = px8::sat(*c as f64 * k);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::super::px8::Img;
    use crate::artistic::{GalleryEffect, GalleryFilter};

    fn flat(w: usize, h: usize, v: [u8; 3]) -> Img {
        let mut img = Img::new(w, h);
        for p in img.d.chunks_exact_mut(4) {
            p.copy_from_slice(&[v[0], v[1], v[2], 255]);
        }
        img
    }

    fn effect(kind: &str) -> GalleryEffect {
        let mut e = GalleryEffect::new(GalleryFilter::Grain);
        assert!(e.set_choice("grainType", kind));
        e
    }

    fn mean(img: &Img, c: usize) -> f64 {
        img.d.chunks_exact(4).map(|p| p[c] as f64).sum::<f64>() / img.n() as f64
    }

    #[test]
    fn additive_grain_keeps_the_mean_of_flat_areas() {
        let img = flat(96, 96, [120, 128, 136]);
        for kind in ["regular", "soft"] {
            let out = super::grain(&effect(kind), &img);
            for c in 0..3 {
                let d = mean(&out, c) - img.d[c] as f64;
                assert!(d.abs() < 0.6, "{kind}: channel {c} moved by {d}");
            }
        }
    }

    #[test]
    fn stippled_draws_the_background_where_the_image_is_light() {
        let e = effect("stippled");
        let share = |v: u8| {
            let out = super::grain(&e, &flat(80, 80, [v; 3]));
            assert!(out.d.chunks_exact(4).all(|p| p[..3] == [0, 0, 0] || p[..3] == [255, 255, 255]));
            out.d.chunks_exact(4).filter(|p| p[0] == 255).count() as f64 / out.n() as f64
        };
        let (dark, mid, light) = (share(0), share(120), share(230));
        assert!((0.04..0.12).contains(&dark), "{dark}");
        assert!((0.4..0.7).contains(&mid), "{mid}");
        assert!(light > 0.99, "{light}");
    }

    #[test]
    fn streaks_are_smoothed_less_along_their_length() {
        // Neighbour correlation of the value along and across horizontal streaks.
        let img = flat(128, 128, [100, 140, 200]);
        let out = super::grain(&effect("horizontal"), &img);
        let v: Vec<f64> = out.d.chunks_exact(4).map(|p| p[2] as f64).collect();
        let m = v.iter().sum::<f64>() / v.len() as f64;
        let corr = |dx: usize, dy: usize| {
            let (mut s, mut n) = (0.0, 0.0);
            for y in 0..128 - dy {
                for x in 0..128 - dx {
                    s += (v[y * 128 + x] - m) * (v[(y + dy) * 128 + x + dx] - m);
                    n += 1.0;
                }
            }
            s / n
        };
        let var = corr(0, 0);
        assert!(corr(1, 0) / var > 0.7 && corr(0, 2) / var < 0.2, "{} {}", corr(1, 0) / var, corr(0, 2) / var);
    }
}
