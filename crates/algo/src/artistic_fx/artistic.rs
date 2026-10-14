//! Filter Gallery › Artistic (Cutout lives in [`crate::cutout`]).
//!
//! Every filter works on the whole 8-bit image: rank filters and box means, edits of the HSV
//! value channel, whole-image auto levels, seeded noise and the bundled relief textures.

use super::Render;
use super::px8::{self, Img};
use super::tables;
use crate::artistic::GalleryEffect;
use crate::artistic::GalleryFilter as F;
use crate::photo_util::{par_map, par_rows, par_rows2};

/// The renderer of a filter this module implements.
pub(super) fn render(f: F) -> Option<Render> {
    Some(match f {
        F::ColoredPencil => colored_pencil,
        F::DryBrush => dry_brush,
        F::FilmGrain => film_grain,
        F::Fresco => fresco,
        F::NeonGlow => neon_glow,
        F::PaintDaubs => paint_daubs,
        F::PaletteKnife => palette_knife,
        F::PlasticWrap => plastic_wrap,
        F::PosterEdges => poster_edges,
        F::RoughPastels => rough_pastels,
        F::SmudgeStick => smudge_stick,
        F::Sponge => sponge,
        F::Underpainting => underpainting,
        F::Watercolor => watercolor,
        _ => return None,
    })
}

// ---- helpers -----------------------------------------------------------------------------------

/// A slider value clamped to `lo..=hi` (NaN → `lo`).
fn num(e: &GalleryEffect, key: &str, lo: f64, hi: f64) -> f64 {
    let v = e.get(key) as f64;
    if v.is_nan() { lo } else { v.clamp(lo, hi) }
}

/// A slider value as a window size or count in `lo..=hi`.
fn int(e: &GalleryEffect, key: &str, lo: f64, hi: f64) -> usize {
    num(e, key, lo, hi).trunc() as usize
}

/// Light directions in kernel order (bottom = 0, then clockwise).
const LIGHTS: [&str; 8] = ["bottom", "bottomLeft", "left", "topLeft", "top", "topRight", "right", "bottomRight"];

/// The relief kernel (1..=8) of a light choice, by name.
fn light_kernel(e: &GalleryEffect) -> usize {
    LIGHTS.iter().position(|n| *n == e.choice("light")).unwrap_or(0) + 1
}

/// `img` with its HSV value channel replaced by `v`.
fn with_value(img: &Img, v: &[u8]) -> Img {
    let mut out = img.clone();
    px8::to_hsv(&mut out);
    out.set_plane(2, v);
    px8::from_hsv(&mut out);
    out
}

/// Light and dark ranks of the "brush" filters for window `b` and texture `txtr` (1..3).
fn light_dark_ranks(b: f64, txtr: f64) -> (f64, f64) {
    let t = 1.0 - (3.0 - txtr) / 10.0;
    let kl = (b * b * t - 1.0).trunc();
    let kd = (b * b * (1.0 - t)).trunc();
    (kl, if kd == 0.0 { 1.0 } else { kd })
}

/// Where a `k × k` rank of `g` stands above its box mean: the pixel sits on the light side of
/// an edge.
fn edge_mask(g: &[u8], w: usize, h: usize, k: usize, rank: f64) -> Vec<bool> {
    let md = px8::rank_rect(g, w, h, k, k, rank);
    let bm = px8::box_mean(g, w, h, k, k);
    md.iter().zip(&bm).map(|(m, b)| m > b).collect()
}

/// Runs `f(pixel index, RGBA)` over every pixel of `img`, rows in parallel.
fn par_px(img: &mut Img, f: impl Fn(usize, &mut [u8]) + Sync + Send) {
    let w = img.w;
    par_rows(&mut img.d, w, 4, |y, row| {
        for (x, px) in row.chunks_exact_mut(4).enumerate() {
            f(y * w + x, px);
        }
    });
}

/// `lt` where `mask`, else `dk` (colour; alpha from `dk`).
fn pick(mask: &[bool], lt: &Img, dk: &Img) -> Img {
    let mut out = dk.clone();
    par_px(&mut out, |i, px| {
        if mask.get(i).copied().unwrap_or(false) {
            px[..3].copy_from_slice(&lt.d[i * 4..i * 4 + 3]);
        }
    });
    out
}

/// Plane version of [`pick`].
fn pick_plane(mask: &[bool], lt: &[u8], dk: &[u8]) -> Vec<u8> {
    mask.iter().zip(lt.iter().zip(dk)).map(|(m, (l, d))| if *m { *l } else { *d }).collect()
}

/// Grain mix: `trunc(v·s + 0.5) + (n − trunc(n·s + 0.5))`, stored wrapping.
fn grain_mix(v: &[u8], n: &[u8], s: f64) -> Vec<u8> {
    v.iter().zip(n).map(|(&v, &n)| px8::wrap((v as f64 * s + 0.5).trunc() + (n as f64 - (n as f64 * s + 0.5).trunc()))).collect()
}

/// Film Grain / Smudge Stick tone: the grain-mixed value plus the auto-levelled value scaled
/// by intensity, saturating.
fn grain_tone(e: &GalleryEffect, img: &Img, noise: &[u8], mix: f64) -> Vec<u8> {
    let area = num(e, "highlightArea", 0.0, 20.0);
    let k = num(e, "intensity", 0.0, 10.0) / 10.0;
    let black = area * -2.75 + 60.0;
    let vmax = px8::max_channel(img);
    let lut = px8::levels_lut(&px8::histogram(&vmax), img.n() as f64, black, 65.0 - black);
    let mixed = grain_mix(&vmax, noise, mix);
    vmax.iter()
        .zip(&mixed)
        .map(|(&v, &m)| {
            let lv = (0.5 + lut[v as usize] as f64 * k).trunc().min(255.0);
            px8::wrap((m as f64 + lv).min(255.0))
        })
        .collect()
}

/// Each colour channel `trunc((m·top + (255 − m)·base)/255)` where `m > 0`; `m` is real.
fn blend_into(top: [f64; 3], base: &mut [u8], m: f64) {
    if m > 0.0 {
        for c in 0..3 {
            base[c] = px8::wrap((m * top[c] + (255.0 - m) * base[c] as f64) / 255.0);
        }
    }
}

// ---- filters -----------------------------------------------------------------------------------

/// Colored Pencil: the darker-than-surroundings detail, smeared along a diagonal, reveals the
/// colours over plain paper; then diagonal medians and a sharpen draw the pencil strokes.
fn colored_pencil(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let n = 2 * int(e, "pencilWidth", 1.0, 24.0) + 1;
    let gain = tables::STROKE_AMOUNT[int(e, "strokePressure", 0.0, 15.0).min(15)];
    let lp = (num(e, "paperBrightness", 0.0, 50.0) * 5.1).trunc();
    // Paper: the background colour at Paper Brightness (the reference ignores the colour).
    let bg = px8::rgb8(e.background);
    let paper_c = [0, 1, 2].map(|c| if px8::defects() { lp } else { (lp * bg[c] as f64 / 255.0).trunc() });

    let g = px8::gray_mean(img);
    let mut d = g.clone();
    px8::subtract(&px8::box_mean(&g, w, h, n, n), &mut d);
    px8::scale(&mut d, gain);
    let m = px8::rank_dir(&d, w, h, 9, 3, true, 20.0);

    let mut paper = img.clone();
    par_px(&mut paper, |i, px| {
        let mm = m[i] as f64;
        for c in 0..3 {
            px[c] = if mm > 0.0 { px8::wrap((mm * px[c] as f64 + (255.0 - mm) * paper_c[c]) / 255.0) } else { paper_c[c] as u8 };
        }
    });
    let q = px8::box_mean_rgb(&paper, 3, 3);
    let a = px8::rank_dir_rgb(&q, 15, 2, true, 16.0);
    let mut c = px8::rank_dir_rgb(&q, 15, 2, false, 16.0);
    px8::average(&a.d, &mut c.d);
    let mut out = px8::sharpen4_rgb(&c, 2.0);
    keep_alpha(&mut out, img);
    out
}

/// Dry Brush: per pixel a light or a dark rank of the colour, chosen by a gray edge mask.
fn dry_brush(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let d = num(e, "brushSize", 0.0, 10.0).trunc() + 5.0;
    let b = 15.0 - num(e, "brushDetail", 0.0, 10.0).trunc();
    let (kl, kd) = light_dark_ranks(b, num(e, "texture", 1.0, 3.0).trunc());
    let mask = edge_mask(&px8::gray_mean(img), w, h, d as usize, d * d / 2.0);
    let bu = b as usize;
    pick(&mask, &px8::rank_rect_rgb(img, bu, bu, kl), &px8::rank_rect_rgb(img, bu, bu, kd))
}

/// Film Grain: seeded noise mixed into the value plus the auto-levelled value.
fn film_grain(e: &GalleryEffect, img: &Img) -> Img {
    let s = (20.0 - num(e, "grain", 0.0, 20.0)) * 0.05;
    let noise = px8::Rng::new(e.seed() >> 1).fill(img.n());
    with_value(img, &grain_tone(e, img, &noise, s))
}

/// Fresco: a sharpened, levelled, boxed value plane rank-filtered light or dark by its own edge
/// mask; hue and saturation stay the source's.
fn fresco(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let d = num(e, "brushSize", 0.0, 10.0).trunc() + 5.0;
    let b = 15.0 - num(e, "brushDetail", 0.0, 10.0).trunc();
    let (kl, kd) = light_dark_ranks(b, num(e, "texture", 1.0, 3.0).trunc());
    let s = px8::sharpen4(&px8::max_channel(img), w, h, 3.0);
    let mut v1 = px8::rank_rect(&s, w, h, 2, 2, 1.0);
    px8::auto_levels(&mut v1, 20.0, 20.0, None);
    let base = px8::box_mean(&v1, w, h, 5, 5);
    let dd = d * d;
    let mask = edge_mask(&base, w, h, d as usize, dd / 2.0 + dd % 2.0);
    let bu = b as usize;
    let v = pick_plane(&mask, &px8::rank_rect(&base, w, h, bu, bu, kl), &px8::rank_rect(&base, w, h, bu, bu, kd));
    with_value(img, &v)
}

/// Neon Glow: two stages of double box blur of the (inverted) gray drive three cascaded blends:
/// glow colour over the foreground, the background over that, the foreground over that.
fn neon_glow(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let size = num(e, "glowSize", -24.0, 24.0).trunc();
    let a = size.abs() as usize;
    let beta = num(e, "glowBrightness", 0.0, 50.0) / 10.0;
    let wo = a + tables::NEON_GLOW_INCREMENT[a.min(24)] as usize;
    let wi = 2 * a + 1;
    let (fg, bg, glow) = (px8::rgb8(e.foreground), px8::rgb8(e.background), px8::rgb8(e.color));
    let f = |c: [u8; 3]| c.map(f64::from);

    let mut o = px8::gray_mean(img);
    if size >= 0.0 {
        px8::invert(&mut o);
    }
    let mut mid = px8::box_mean(&px8::box_mean(&o, w, h, wo, wo), w, h, wo, wo);
    px8::scale(&mut mid, beta);
    let mut g2 = px8::box_mean(&px8::box_mean(&mid, w, h, wi, wi), w, h, wi, wi);
    px8::scale(&mut g2, beta * 13.0 / 15.0);

    let mut out = img.clone();
    par_px(&mut out, |i, px| {
        let mut l1 = [0u8; 3];
        if g2[i] > 0 {
            l1 = fg;
            blend_into(f(glow), &mut l1, g2[i] as f64);
        }
        let mut l2 = [0u8; 3];
        if mid[i] > 0 {
            l2 = l1;
            blend_into(f(bg), &mut l2, mid[i] as f64);
        }
        if o[i] > 0 {
            blend_into(f(fg), &mut l2, o[i] as f64);
            px[..3].copy_from_slice(&l2);
        }
    });
    out
}

/// Paint Daubs: a per-channel percentile filter (fraction by brush type) with a fixed 3×3
/// sharpen blended back in by Sharpness.
fn paint_daubs(e: &GalleryEffect, img: &Img) -> Img {
    const BRUSHES: [&str; 6] = ["simple", "lightRough", "darkRough", "wideSharp", "wideBlurry", "sparkle"];
    let r = px8::rhu(num(e, "brushSize", 1.0, 50.0) * 0.5) as usize;
    let gain = num(e, "sharpness", 0.0, 40.0) * 0.4;
    let p = tables::PAINT_DAUBS_PERCENTILE[BRUSHES.iter().position(|b| *b == e.choice("brushType")).unwrap_or(0)];
    // The reference ranks a square; Photoshop's daubs are round and a little lighter.
    let d = if px8::defects() { px8::clipped_rank_rgb(img, r, px8::Clipped::Percentile(p)) } else { disc_percentile_rgb(img, r, p + 0.03) };
    let k = [-0.7, -1.0, -0.7, -1.0, 10.0, -1.0, -0.7, -1.0, -0.7];
    let sum: f64 = k.iter().sum();
    let s = px8::conv3_rgba(&d, &k.map(|v| v / sum), false, false);
    let mut out = d;
    par_px(&mut out, |i, o| {
        for c in 0..3 {
            let (dv, sv) = (o[c] as f64, s.d[i * 4 + c] as f64);
            o[c] = px8::clamp255(dv + gain * (sv - dv));
        }
    });
    keep_alpha(&mut out, img);
    out
}

/// Palette Knife: the value plane mode-filtered, the colour box-blurred, joined through HSV,
/// then a soft four-neighbour smoothing.
fn palette_knife(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let k = int(e, "strokeSize", 1.0, 50.0);
    let passes = 4usize.saturating_sub(int(e, "strokeDetail", 1.0, 5.0));
    let soft = num(e, "softness", 0.0, 10.0).trunc();
    let mut v = px8::max_channel(img);
    for _ in 0..passes {
        v = px8::mode(&v, w, h, k);
    }
    let c = with_value(&px8::box_mean_rgb(img, k, k), &v);
    let mut out = if soft > 0.0 { px8::smooth4_rgb(&c, 11.0 - soft) } else { c };
    keep_alpha(&mut out, img);
    out
}

/// Plastic Wrap: a highlight `c¹⁰` from the slope of the blurred luma (flat regions take a
/// coordinate ramp), composited in white over the image at Highlight Strength.
fn plastic_wrap(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    if img.n() == 0 {
        return img.clone();
    }
    let opacity = num(e, "highlightStrength", 0.0, 20.0) / 20.0;
    let detail = num(e, "detail", 1.0, 15.0).trunc();
    let smooth = num(e, "smoothness", 1.0, 15.0).trunc();
    let mut y = px8::gray_weighted(img);
    if detail != 15.0 {
        y = px8::gauss_u8(&y, w, h, px8::rhu((15.0 - detail) * 0.5), 3);
    }
    // Flat pixels take a slope ramping across the image (−ramp at the left/top edge, +ramp at
    // the right/bottom): flat regions shine near the centre only. Photoshop's falls off faster
    // than the reference's ±0.7.
    let ramp = if px8::defects() { 0.7 } else { 1.1 };
    let (mut gx, mut gy) = (vec![0f32; w * h], vec![0f32; w * h]);
    par_rows2(&mut gx, &mut gy, w, |yy, rx, ry| {
        for x in 0..w {
            let i = yy * w + x;
            // Forward difference; backward at the last column / row.
            let at = |j: usize| y[j] as f64;
            let slope = |pos: usize, n: usize, st: usize| match (pos + 1 < n, pos > 0) {
                (true, _) => at(i + st) - at(i),
                (false, true) => at(i) - at(i - st),
                _ => 0.0,
            };
            let (sx, sy) = (slope(x, w, 1), slope(yy, h, w));
            rx[x] = if sx != 0.0 { 0.4 * sx } else { x as f64 * (2.0 * ramp / w as f64) - ramp } as f32;
            ry[x] = if sy != 0.0 { 0.4 * sy } else { yy as f64 * (2.0 * ramp / h as f64) - ramp } as f32;
        }
    });
    if smooth > 1.0 {
        gx = px8::field_smooth(&gx, w, h, smooth - 1.0);
        gy = px8::field_smooth(&gy, w, h, smooth - 1.0);
    }
    let mut out = img.clone();
    par_px(&mut out, |i, px| {
        let (ax, ay) = (gx[i] as f64, gy[i] as f64);
        let c = 1.0 / ((1.0 + ax * ax).sqrt() * (1.0 + ay * ay).sqrt());
        let c2 = c * c;
        let c4 = c2 * c2;
        composite_white(px, (255.0 * (c4 * c4 * c2)).trunc(), opacity);
    });
    out
}

/// Normal composite of `(h, h, h)` with alpha `h` over `px` at `opacity` (colour only).
fn composite_white(px: &mut [u8], h: f64, opacity: f64) {
    if h <= 0.0 {
        return;
    }
    if h >= 255.0 && opacity >= 1.0 {
        px[..3].fill(255);
        return;
    }
    let sa = ((h * opacity).trunc() as i64 & 255) as f64;
    let da = px8::rhu(px[3] as f64 * (255.0 - sa) / 255.0);
    let oa = sa + da;
    for c in 0..3 {
        let v = (h * sa + px[c] as f64 * da) as u64;
        let mm = ((v + 1 + (v >> 8)) >> 8) as f64;
        px[c] = if oa == 0.0 { 0 } else { px8::wrap(px8::rhu(mm * 255.0 / oa)) };
    }
}

/// Poster Edges: the box-blurred value posterised in power-of-two steps, darkened by an
/// auto-levelled "darker than its surroundings" map.
fn poster_edges(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let k = int(e, "edgeThickness", 0.0, 10.0) + 5;
    let white = num(e, "edgeIntensity", 0.0, 10.0);
    let step = 2f64.powi(7 - num(e, "posterization", 0.0, 6.0).trunc() as i32);
    let b3 = px8::box_mean(&px8::max_channel(img), w, h, 3, 3);
    let mut d = b3.clone();
    px8::subtract(&px8::box_mean(&b3, w, h, k, k), &mut d);
    px8::auto_levels(&mut d, 1.0, white, None);
    let v: Vec<u8> = b3
        .iter()
        .zip(&d)
        .map(|(&b, &dv)| {
            let q = (0.5 + b as f64 * (1.0 / step)).trunc().min(255.0);
            let p = (0.5 + q * step).trunc().min(255.0);
            (p - dv as f64).max(0.0) as u8
        })
        .collect();
    with_value(img, &v)
}

/// Rough Pastels: an anti-diagonally smeared, embossed copy of the image shows through a mask
/// made from the slope of the texture.
fn rough_pastels(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let len = int(e, "strokeLength", 0.0, 40.0) + 9;
    let amount = num(e, "strokeDetail", 1.0, 20.0) / 2.0;
    let relief = px8::relief_factor(num(e, "relief", 0.0, 50.0));
    let tex = px8::pattern(px8::Texture::from_name(e.choice("texture")), num(e, "scaling", 50.0, 200.0), e.flag("invert"), w, h);

    let d2 = px8::box_dir_rgb(&px8::box_dir_rgb(img, len, 1, true), len, 1, true);
    let emb = d2.map_rgb(|p| {
        let mut v = px8::relief(p, p, w, h, 7, amount);
        px8::scale(&mut v, 1.2);
        px8::inverse_scale(&mut v, 1.2);
        v
    });
    let mut mt = px8::relief(&tex, &vec![0; w * h], w, h, light_kernel(e), relief);
    px8::scale(&mut mt, 3.0);
    let mut out = img.clone();
    px8::blend_mask_rgb(&emb, &mt, img, &mut out);
    keep_alpha(&mut out, img);
    out
}

/// Smudge Stick: the Film Grain tone (mix 0.8) smeared by a sheared rank filter whose rank
/// grows with the stroke length.
fn smudge_stick(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let m = num(e, "strokeLength", 0.0, 20.0).trunc() + 4.0;
    let noise = px8::Rng::new(e.seed()).fill(img.n());
    let vs = grain_tone(e, img, &noise, 0.8);
    // Photoshop's rank stays near an eighth of the window; the reference adds `m mod 5`.
    let rank = if px8::defects() { 2.0 * m / 5.0 + m % 5.0 } else { (2.0 * m / 5.0).floor() };
    let v = px8::rank_dir(&vs, w, h, m as usize - 1, 3, false, rank);
    with_value(img, &v)
}

/// Sponge: seeded noise in the value, boxed and sharpened, then light/dark 7×7 ranks by a gray
/// edge mask and a final median.
fn sponge(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let c = int(e, "brushSize", 0.0, 10.0) + 5;
    let a = num(e, "definition", 0.0, 25.0) / 10.0;
    let s = int(e, "smoothness", 1.0, 15.0);
    let half = |k: usize| ((k * k / 2) + k % 2) as f64;
    let noise = px8::Rng::new(e.seed()).fill(img.n());
    let mixed = grain_mix(&px8::max_channel(img), &noise, 0.8);
    let sh = px8::sharpen4(&px8::box_mean(&mixed, w, h, 3, 3), w, h, a);
    let rs = with_value(img, &sh);
    let mask = edge_mask(&px8::gray_mean(&rs), w, h, c, half(c));
    let m = pick(&mask, &px8::rank_rect_rgb(&rs, 7, 7, 40.0), &px8::rank_rect_rgb(&rs, 7, 7, 10.0));
    px8::rank_rect_rgb(&m, s, s, half(s))
}

/// Underpainting: the mode-filtered value, boxed, with the texture's relief blended in where
/// mode and box disagree; colour from a double 9×9 box blur.
fn underpainting(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let ms = int(e, "brushSize", 0.0, 40.0) + 9;
    let bs = int(e, "textureCoverage", 0.0, 40.0) + 9;
    let relief = px8::relief_factor(num(e, "relief", 0.0, 50.0));
    let tex = px8::pattern(px8::Texture::from_name(e.choice("texture")), num(e, "scaling", 50.0, 200.0), e.flag("invert"), w, h);

    let mut mo = px8::mode(&px8::max_channel(img), w, h, ms);
    let bx = px8::box_mean(&mo, w, h, bs, bs);
    let r = px8::relief(&tex, &bx, w, h, light_kernel(e), relief);
    par_rows(&mut mo, w, 1, |y, row| {
        for (x, v) in row.iter_mut().enumerate() {
            let i = y * w + x;
            let dt = (0.5 + 10.0 * v.abs_diff(bx[i]) as f64).trunc().min(255.0);
            if dt > 0.0 {
                let m = 0.8 * dt;
                *v = px8::wrap((m * r[i] as f64 + (255.0 - m) * *v as f64) / 255.0);
            }
        }
    });
    let c = px8::box_mean_rgb(&px8::box_mean_rgb(img, 9, 9), 9, 9);
    with_value(&c, &mo)
}

/// Watercolor: Dry Brush's light/dark selection, then the value darkened by twice its local
/// range and auto-levelled with a Shadow Intensity black clip.
fn watercolor(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let b = 16.0 - num(e, "brushDetail", 1.0, 14.0).trunc();
    let black = 8.0 * num(e, "shadowIntensity", 0.0, 10.0);
    let (kl, kd) = light_dark_ranks(b, num(e, "texture", 1.0, 3.0).trunc());
    let mask = edge_mask(&px8::gray_mean(img), w, h, 7, 25.0);
    let bu = b as usize;
    let m = pick(&mask, &px8::rank_rect_rgb(img, bu, bu, kl), &px8::rank_rect_rgb(img, bu, bu, kd));
    let v = px8::max_channel(&m);
    let mn = px8::rank_rect(&v, w, h, 3, 3, 1.0);
    let mx = px8::rank_rect(&v, w, h, 3, 3, 9.0);
    let q: Vec<u8> = (0..v.len())
        .map(|i| {
            let rg = mx[i].saturating_sub(mn[i]);
            v[i].saturating_sub(rg).saturating_sub(rg)
        })
        .collect();
    let mut q2 = px8::rank_rect(&q, w, h, 3, 3, 8.0);
    px8::auto_levels(&mut q2, black, 1.0, None);
    with_value(&m, &q2)
}

/// Percentile filter over a disc of radius `r` (offsets with `dx² + dy² ≤ (r + ½)²`),
/// clipped to the image, per colour channel: the element at 0-based position
/// `trunc(0.5 + p·n)` of the sorted window. On an image with any non-opaque pixel each pixel
/// counts with its alpha; a window of weight 0 keeps the pixel.
fn disc_percentile_rgb(img: &Img, r: usize, p: f64) -> Img {
    let (w, h) = (img.w, img.h);
    if r == 0 || img.n() == 0 {
        return img.clone();
    }
    let ri = r as i64;
    let rr = (r as f64 + 0.5).powi(2);
    let half: Vec<i64> = (-ri..=ri).map(|dy| (rr - (dy * dy) as f64).max(0.0).sqrt().floor() as i64).collect();
    let weight = (!img.opaque()).then(|| img.plane(3));
    let planes: Vec<Vec<Option<u8>>> = par_map(3, |c| {
        let src = img.plane(c);
        let at = |x: i64, y: i64| -> Option<(usize, u64)> {
            if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 {
                return None;
            }
            let i = y as usize * w + x as usize;
            Some((src[i] as usize, weight.as_ref().map_or(1, |a| a[i] as u64)))
        };
        let rows: Vec<Vec<Option<u8>>> = par_map(h, |y| {
            let mut hist = WHist::default();
            for (j, &hw) in half.iter().enumerate() {
                let yy = y as i64 + j as i64 - ri;
                for x in -hw..=hw {
                    hist.add(at(x, yy));
                }
            }
            let mut row = Vec::with_capacity(w);
            for x in 0..w as i64 {
                if x > 0 {
                    for (j, &hw) in half.iter().enumerate() {
                        let yy = y as i64 + j as i64 - ri;
                        hist.remove(at(x - 1 - hw, yy));
                        hist.add(at(x + hw, yy));
                    }
                }
                row.push(hist.percentile(p));
            }
            row
        });
        rows.into_iter().flatten().collect()
    });
    let mut out = img.clone();
    for (c, plane) in planes.iter().enumerate() {
        for (px, v) in out.d.chunks_exact_mut(4).zip(plane) {
            if let Some(v) = v {
                px[c] = *v;
            }
        }
    }
    out
}

/// A weighted 256-bin histogram with 16 coarse buckets.
struct WHist {
    fine: [u64; 256],
    coarse: [u64; 16],
    n: u64,
}

impl Default for WHist {
    fn default() -> Self {
        WHist { fine: [0; 256], coarse: [0; 16], n: 0 }
    }
}

impl WHist {
    fn add(&mut self, s: Option<(usize, u64)>) {
        if let Some((v, wt)) = s {
            self.fine[v] += wt;
            self.coarse[v >> 4] += wt;
            self.n += wt;
        }
    }
    fn remove(&mut self, s: Option<(usize, u64)>) {
        if let Some((v, wt)) = s {
            self.fine[v] -= wt;
            self.coarse[v >> 4] -= wt;
            self.n -= wt;
        }
    }
    /// The smallest value whose cumulative weight exceeds `trunc(0.5 + p·n)`; `None` when empty.
    fn percentile(&self, p: f64) -> Option<u8> {
        if self.n == 0 {
            return None;
        }
        let t = (0.5 + p * self.n as f64).trunc().max(0.0) as u64;
        let mut cum = 0u64;
        let mut bin = 0usize;
        while bin < 256 && cum + self.coarse[bin >> 4] <= t {
            cum += self.coarse[bin >> 4];
            bin += 16;
        }
        while bin < 256 {
            cum += self.fine[bin];
            if cum > t {
                return Some(bin as u8);
            }
            bin += 1;
        }
        Some(255)
    }
}

/// Copies `src`'s alpha into `out`.
fn keep_alpha(out: &mut Img, src: &Img) {
    for (o, s) in out.d.chunks_exact_mut(4).zip(src.d.chunks_exact(4)) {
        o[3] = s[3];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: usize, h: usize) -> Img {
        let mut m = Img::new(w, h);
        for (i, p) in m.d.chunks_exact_mut(4).enumerate() {
            let (x, y) = (i % w.max(1), i / w.max(1));
            p.copy_from_slice(&[
                ((37 * x + 11 * y) % 256) as u8,
                ((13 * x + 59 * y) % 256) as u8,
                ((7 * x * x + 23 * y) % 256) as u8,
                if i % 3 == 0 { 128 } else { 255 },
            ]);
        }
        m
    }

    #[test]
    fn mode_filter_matches_a_naive_window() {
        let (w, h) = (31, 17);
        let mut r = px8::Rng::new(7);
        for levels in [3u32, 256] {
            let src: Vec<u8> = (0..w * h).map(|_| (r.next() * levels as f64) as u8).collect();
            for k in [1usize, 2, 5, 8, 40] {
                let got = px8::mode(&src, w, h, k);
                let half = (k >> 1) as i64;
                for y in 0..h {
                    for x in 0..w {
                        let mut hist = [0u32; 256];
                        for j in 0..k as i64 {
                            for i in 0..k as i64 {
                                let yy = (y as i64 - half + j).clamp(0, h as i64 - 1) as usize;
                                let xx = (x as i64 - half + i).clamp(0, w as i64 - 1) as usize;
                                hist[src[yy * w + xx] as usize] += 1;
                            }
                        }
                        let max = hist.iter().max().copied().unwrap_or(0);
                        let want = hist.iter().position(|&n| n == max).unwrap_or(0) as u8;
                        assert_eq!(got[y * w + x], want, "levels {levels} k {k} at {x},{y}");
                    }
                }
            }
        }
    }

    #[test]
    fn disc_percentile_matches_a_naive_window() {
        let (w, h) = (23, 19);
        let mut im = img(w, h);
        for p in im.d.chunks_exact_mut(4) {
            p[3] = 255;
        }
        for (r, p) in [(1usize, 0.5), (3, 0.78), (6, 0.1)] {
            let got = disc_percentile_rgb(&im, r, p);
            let rr = (r as f64 + 0.5).powi(2);
            for y in 0..h as i64 {
                for x in 0..w as i64 {
                    let mut v: Vec<u8> = Vec::new();
                    for dy in -(r as i64)..=r as i64 {
                        for dx in -(r as i64)..=r as i64 {
                            let (xx, yy) = (x + dx, y + dy);
                            if (dx * dx + dy * dy) as f64 <= rr && xx >= 0 && yy >= 0 && xx < w as i64 && yy < h as i64 {
                                v.push(im.d[(yy as usize * w + xx as usize) * 4]);
                            }
                        }
                    }
                    v.sort();
                    let t = ((0.5 + p * v.len() as f64) as usize).min(v.len() - 1);
                    assert_eq!(got.d[(y as usize * w + x as usize) * 4], v[t], "r {r} at {x},{y}");
                }
            }
        }
    }

    #[test]
    fn tiny_images_and_extreme_values_do_not_panic() {
        let filters = [
            F::ColoredPencil,
            F::DryBrush,
            F::FilmGrain,
            F::Fresco,
            F::NeonGlow,
            F::PaintDaubs,
            F::PaletteKnife,
            F::PlasticWrap,
            F::PosterEdges,
            F::RoughPastels,
            F::SmudgeStick,
            F::Sponge,
            F::Underpainting,
            F::Watercolor,
        ];
        for f in filters {
            let Some(r) = render(f) else { panic!("{f:?} has no renderer") };
            for (w, h) in [(0, 0), (1, 1), (2, 2), (3, 1), (1, 5), (9, 7)] {
                for v in [f32::NAN, -1e9, 0.0, 1e9] {
                    let mut e = GalleryEffect::new(f);
                    for k in e.values.keys().cloned().collect::<Vec<_>>() {
                        e.values.insert(k, v);
                    }
                    let src = img(w, h);
                    let out = r(&e, &src);
                    assert_eq!((out.w, out.h, out.d.len()), (w, h, w * h * 4), "{f:?} {w}x{h} {v}");
                    assert!(out.d.chunks_exact(4).zip(src.d.chunks_exact(4)).all(|(o, s)| o[3] == s[3]), "{f:?} alpha");
                }
            }
        }
    }
}
