//! Sketch filters drawing a pattern over the image: Graphic Pen, Halftone Pattern, Water Paper.

use super::super::px8::{self, Img};
use super::super::tables;
use super::{ink_paper, int};
use crate::artistic::GalleryEffect;
use crate::photo_util::par_rows;

/// Graphic Pen: a sequential random pen walking the image along the stroke direction. Each
/// streak starts inked or blank by comparing a random number with the pixel's darkness, and
/// continues while the gray stays close, ending at random or after a random length.
///
/// A streak inks when `r / rs + ro ≤ τ − (gray + shift)`. The spec used the absolute gray
/// (shift 0), a uniform `r`, a ramp as wide as `1/rs` with `rs = 1 + length/10`, centred
/// (`ro = (1 − 1/rs)/2`), and streaks of `trunc(r·length)` more pixels.
///
/// The reference's ink follows the gray relative to the image's mean instead: at balance 50 a
/// little under half the pixels at the mean gray are inked, and none from about 65 levels above
/// it, on dark and bright images alike (the parrot, mean 60, and the beach, mean 130, give the
/// same curve). So the gray is shifted by [`GP_CENTRE`] − mean, the ramp is [`GP_RAMP`] wide
/// whatever the length, and `r` is the mean of two uniforms, which softens the ends of the ramp
/// the way the reference's are. Its streaks are mostly short with a long tail (four in ten are
/// one pixel long, some run past the length setting): `trunc(r³·1.3·length)` more pixels.
pub(in super::super) fn graphic_pen(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let len = int(e, "strokeLength").min(1000);
    let bal = int(e, "lightDarkBalance").min(100);
    let quirk = px8::defects();
    // The reference stops darkening at balance 50: 100 draws exactly what 50 does.
    let bal = if quirk { bal } else { bal.min(50) };
    let tau = if bal <= 50 { tables::GRAPHIC_PEN_DENSITY[bal] / 255.0 } else { 1.0 + (255.0 - tables::GRAPHIC_PEN_DENSITY[101 - bal]) / 255.0 };
    // The reference's ramp narrows with the stroke length, from the whole gray range at length 1
    // (ink in proportion to darkness) to [`GP_RAMP`] at 15, about the same centre; its draw goes
    // from one uniform to the mean of two.
    let k = (len.clamp(1, 15) - 1) as f64 / 14.0;
    let ramp = 1.0 + (GP_RAMP - 1.0) * k;
    let rs = if quirk { 1.0 + 0.1 * len as f64 } else { 1.0 / ramp };
    let ro = if quirk { (1.0 - 1.0 / rs) / 2.0 } else { 1.0 - (GP_RAMP + ramp) / 2.0 };
    let gray = px8::gray_mean(img);
    let n: Vec<f32> = gray.iter().map(|&g| (g as f64 / 255.0) as f32).collect();
    let shift = if quirk { 0.0 } else { GP_CENTRE - GP_FOLLOW * alpha_mean(&gray, img) / 255.0 };
    let mut rng = px8::Rng::new(e.seed() >> 1);
    // 0 = ink, 255 = paper, None = not drawn yet.
    let mut pen: Vec<Option<u8>> = vec![None; w * h];
    if len == 1 {
        for p in pen.iter_mut() {
            if rng.next() < 1.0 / 255.0 {
                *p = Some(255);
            }
        }
    }
    let mut st = Pen { pv: 255, pg: None, c: 0 };
    let mut visit = |st: &mut Pen, i: usize| {
        if pen[i].is_some() {
            return;
        }
        let nv = n[i];
        let d = tau - (nv as f64 + shift);
        let r = rng.next();
        let jump = st.pg.is_some_and(|pg| ((pg - nv) as f64).abs() > 0.0625);
        if st.c == 0 || jump {
            let u = if quirk { r } else { r + (rng.next() - r) * k / 2.0 };
            st.pv = if u / rs + ro <= d { 0 } else { 255 };
            let r2 = rng.next();
            st.c = if quirk { (r2 * len as f64) as i64 } else { (r2 * r2 * r2 * 1.3 * len as f64) as i64 };
        } else if r > 0.1 {
            st.c -= 1;
        } else {
            st.c = 0;
        }
        pen[i] = Some(st.pv);
        st.pg = Some(nv);
    };
    let last = w.saturating_sub(1);
    match e.choice("strokeDirection") {
        "horizontal" => {
            for y in 0..h {
                for x in 0..w {
                    visit(&mut st, y * w + x);
                }
                st.c = 0;
            }
        }
        "vertical" => {
            for x in 0..w {
                for y in 0..h {
                    visit(&mut st, y * w + x);
                }
                st.c = 0;
            }
        }
        dir => {
            let right = dir != "leftDiagonal";
            let col = |x: usize| if right { last - x } else { x };
            // Lines starting on the top row, then lines starting down the first column.
            for l in 0..w {
                for s in 0..(w - l).min(h) {
                    visit(&mut st, s * w + col(l + s));
                }
                st.c = 0;
            }
            for l in 1..h {
                for s in 0..(h - l).min(w) {
                    visit(&mut st, (l + s) * w + col(s));
                }
                st.c = 0;
            }
        }
    }
    let g: Vec<u8> = pen.iter().map(|p| p.unwrap_or(255)).collect();
    let (fg, bg) = ink_paper(e);
    px8::colourise(&g, fg, bg, img)
}

/// Graphic Pen's ink ramp width (gray 0–1) in the reference's model.
const GP_RAMP: f64 = 0.55;
/// Where a mid-dark image's mean gray (95) lands on Graphic Pen's ramp (just past its middle).
const GP_CENTRE: f64 = 0.321;
/// How far Graphic Pen's ramp follows the image's mean gray: a little further than the mean
/// moves (the beach, mean 130, inks a little more than the parrot, mean 61, at its mean).
const GP_FOLLOW: f64 = 1.07;

/// The alpha-weighted mean of a gray plane (128 when fully transparent).
fn alpha_mean(g: &[u8], img: &Img) -> f64 {
    let (mut s, mut ws) = (0.0, 0.0);
    for (v, px) in g.iter().zip(img.d.chunks_exact(4)) {
        let a = px[3] as f64;
        s += *v as f64 * a;
        ws += a;
    }
    if ws > 0.0 { s / ws } else { 128.0 }
}

/// The pen's state carried along the walk.
struct Pen {
    pv: u8,
    pg: Option<f32>,
    c: i64,
}

/// Halftone Pattern as the reference draws it: the gray box-blurred (radius 1), multiplied by a
/// screen (`1 + a·σ`), box-blurred again over about five sixths of the cell and scaled to about a
/// quarter (so the plane has 60-odd levels), then stretched to 0–255 between the levels that clip
/// `contrast` % of the pixels at each end. The output therefore has as many evenly spaced levels
/// as the stretched range spans: 32 on the parrot, 37 to 42 on the beach. Contrast 50 clips at the
/// median, a two-tone threshold.
///
/// The dot and line screens are dark at `(size, size)` and repeat every two cells from the image
/// origin, with a narrower peak than a cosine (`|cos|^1.64`: after the blur the pixels beside a
/// size-1 dot carry 0.61 of its swing, not 0.5); the circles are concentric about the image
/// centre, light there, two cells apart. The screen's amplitude grows with the size, from 0.374
/// at 1 to 0.8 at 12. The second blur reads the screened plane past the bottom and right edges
/// as if the screen went on, and before about half its radius from the top and left edges as the
/// first row or column inside it, which leaves the first rows and columns with a stronger pattern.
///
/// The spec ([`halftone_spec`]) screened once with a centred screen that only darkens
/// (×0.75 … ×1), stretched around the image's mean and thresholded above contrast 46, which left
/// dark images too dark and bright ones too flat.
pub(in super::super) fn halftone(e: &GalleryEffect, img: &Img) -> Img {
    if px8::defects() {
        return halftone_spec(e, img);
    }
    let (w, h) = (img.w, img.h);
    // Capped well past the slider (12): the blur pads the plane by most of a cell all round.
    let size = int(e, "size").clamp(1, 100);
    let clip = (e.get("contrast") as f64 / 100.0).clamp(0.0, 0.5);
    let f = std::f64::consts::PI / (2 * size + 1) as f64;
    let o = size as f64;
    let (cx, cy) = ((w / 2) as f64, (h / 2) as f64);
    let pattern = e.choice("patternType");
    let r2 = (5 * size + 3) / 6;
    let low = (r2 as i64 + 1) / 2;
    let grow = 1.0 + HT_AMP_GROWTH * (size - 1) as f64;
    // The circles keep a plain cosine.
    let (amp, peak) = if pattern == "circle" { (0.3 * grow, 1.0) } else { (HT_AMP * grow, HT_PEAK) };
    let shape = |c: f64| c.signum() * c.abs().powf(peak);
    let g: Vec<f32> = px8::gray_round(img).iter().map(|&v| v as f32).collect();
    let s = px8::box_pass_f32(&g, w, h, 1);
    // The screened plane with `r2` extra rows and columns all round, cropped after the blur.
    let (pw, ph) = (w + 2 * r2, h + 2 * r2);
    let at = |p: usize| (p as i64 - r2 as i64).max(low);
    let mut big = vec![0f32; pw * ph];
    par_rows(&mut big, pw, 1, |py, row| {
        let y = at(py);
        let sy = (y.max(0) as usize).min(h.saturating_sub(1));
        let ay = (y as f64 - o) * f;
        for (px, v) in row.iter_mut().enumerate() {
            let x = at(px);
            let sx = (x.max(0) as usize).min(w.saturating_sub(1));
            let sigma = match pattern {
                "line" => -shape(ay.cos()),
                "circle" => shape(((x as f64 - cx).hypot(y as f64 - cy) * f).cos()),
                _ => -shape(((x as f64 - o) * f).cos()) * shape(ay.cos()),
            };
            let base = s.get(sy * w + sx).copied().unwrap_or(0.0) as f64;
            *v = (base * (1.0 + amp * sigma)) as f32;
        }
    });
    // The scale fixes how many levels the stretch spreads: the reference's line and circle screens
    // come out a few percent brighter than its dots.
    let scale = match pattern {
        "line" => 0.25,
        "circle" => 0.2475,
        _ => 0.24,
    };
    let blurred = px8::box_pass_f32(&big, pw, ph, r2);
    let mut v = vec![0u8; w * h];
    for (y, row) in v.chunks_exact_mut(w.max(1)).enumerate() {
        for (x, p) in row.iter_mut().enumerate() {
            let b = blurred.get((y + r2) * pw + x + r2).copied().unwrap_or(0.0);
            *p = px8::clamp255((b as f64 * scale).floor());
        }
    }
    // The clip levels, from a histogram weighted by alpha.
    let mut hist = [0f64; 256];
    for (p, px) in v.iter().zip(img.d.chunks_exact(4)) {
        hist[*p as usize] += px[3] as f64;
    }
    let cut = clip * hist.iter().sum::<f64>();
    let lo = clip_level(&hist, cut, 0..256).unwrap_or(0) as f64;
    let hi = clip_level(&hist, cut, (0..256).rev()).unwrap_or(255) as f64;
    let out: Vec<u8> = v
        .iter()
        .map(|&p| {
            let p = p as f64;
            if hi <= lo { if p > lo { 255 } else { 0 } } else { px8::clamp255(((p - lo).clamp(0.0, hi - lo) * 255.0 / (hi - lo)).round()) }
        })
        .collect();
    let (fg, bg) = ink_paper(e);
    px8::colourise_direct(&out, fg, bg, img)
}

/// Halftone Pattern's screen amplitude at size 1 (dots and lines).
const HT_AMP: f64 = 0.374;
/// How much the screen amplitude grows per size step (relative): 0.374 at 1, 0.8 at 12.
const HT_AMP_GROWTH: f64 = (0.8 / HT_AMP - 1.0) / 11.0;
/// The exponent narrowing the dot and line screens' peaks.
const HT_PEAK: f64 = 1.64;

/// The first level in `order` past which the histogram's running sum exceeds `cut`.
fn clip_level(hist: &[f64; 256], cut: f64, order: impl Iterator<Item = usize>) -> Option<usize> {
    let mut acc = 0.0;
    for i in order {
        acc += hist.get(i).copied().unwrap_or(0.0);
        if acc > cut {
            return Some(i);
        }
    }
    None
}

/// Halftone Pattern as the spec has it: the box-blurred gray modulated by a centred dot, circle or
/// line screen, then a contrast stretch around the image's mean, a threshold above contrast 46.
fn halftone_spec(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let size = int(e, "size").min(1000);
    let contrast = e.get("contrast") as f64;
    let cell = (2 * size + 1) as f64;
    let f = std::f64::consts::PI / cell;
    let g = px8::box_pass_u8(&px8::gray_round(img), w, h, size);
    let pattern = e.choice("patternType");
    let (cx, cy) = ((w / 2) as f64, (h / 2) as f64);
    let mut v = vec![0f64; w * h];
    let (mut sum, mut wsum) = (0.0, 0.0);
    for y in 0..h {
        let ay = (y as f64 - cy) * f;
        for x in 0..w {
            let i = y * w + x;
            let ax = (x as f64 - cx) * f;
            let sigma = match pattern {
                "line" => Some(ay.cos()),
                "circle" => {
                    let k = (0.5 + (ax * ax + ay * ay).sqrt() * 4.5).trunc();
                    // The spec reads a sine table of 2W entries: beyond it the pixel is 0.
                    (k < 2.0 * w as f64).then(|| (k * (1.0 / 4.5)).sin())
                }
                _ => Some(ax.cos() * ay.cos()),
            };
            let s = sigma.map_or(0.0, |s| px8::clamp255((g[i] as f64 * (0.75 + 0.25 * s)).trunc()) as f64);
            v[i] = s;
            let a = img.d[i * 4 + 3] as f64 / 255.0;
            sum += s * a;
            wsum += a;
        }
    }
    let mean = if wsum > 0.0 { sum / wsum } else { 128.0 };
    let base = 128.0 + 1.26 * (mean - 128.0);
    let spread = 0.08 + 0.25 * ((128.0 - mean) / 128.0).abs();
    let gain = 1.0 + spread * contrast;
    let out: Vec<u8> = v
        .iter()
        .map(|&p| {
            let s = px8::clamp255((base + (p - base) * gain).trunc());
            if contrast > 46.0 { if s < 128 { 0 } else { 255 } } else { s }
        })
        .collect();
    let (fg, bg) = ink_paper(e);
    px8::colourise_direct(&out, fg, bg, img)
}

/// Water Paper: the image blurred by half the fibre length, its HSV value replaced by a fibre
/// plane (stripes, noise and the brightest channel smeared by long thin rank filters), then a
/// brightness/contrast table.
pub(in super::super) fn water_paper(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let l = int(e, "fiberLength").clamp(1, 1000);
    let bs = l / 2;
    let k1 = (l / 5).max(1) as f64;
    let k2 = (2 * l / 3).max(1) as f64;
    let tb = px8::bc_lut(e.get("brightness") as f64, e.get("contrast") as f64);
    let mx = px8::max_channel(img);
    let mut rng = px8::Rng::new(e.seed() >> 2);
    let noise = rng.fill(img.n());
    let stripe = |vertical: bool| -> Vec<u8> {
        let mut p = vec![0u8; w * h];
        par_rows(&mut p, w, 1, |y, row| {
            for (x, v) in row.iter_mut().enumerate() {
                let i = y * w + x;
                let edge = y < 2 || y + 2 >= h || x + 2 >= w || if vertical { x % 4 < 2 } else { y % 4 < 2 || x < 2 };
                let s = if edge { noise[i] } else { 255 };
                let (sf, m) = (s as f64, mx[i] as f64);
                *v = px8::wrap(((255.0 - sf) * m + sf * sf) / 255.0);
            }
        });
        p
    };
    let sv = stripe(true);
    let sh = stripe(false);
    let c = px8::box_mean_rgb(&px8::box_mean_rgb(img, bs, bs), bs, bs);
    // The tall windows run on the transposed plane, sliding along their length.
    let t = px8::rank_rect(&transpose(&sv, w, h), h, w, l, 1, k1);
    let sv = transpose(&px8::rank_rect(&t, h, w, l, 3, k2), h, w);
    let t = px8::rank_rect(&sh, w, h, l, 1, k1);
    let mut sh = px8::rank_rect(&t, w, h, l, 3, k2);
    px8::average(&sv, &mut sh);
    let mut out = c;
    px8::to_hsv(&mut out);
    out.set_plane(2, &sh);
    px8::from_hsv(&mut out);
    let quirk = px8::defects();
    par_rows(&mut out.d, w, 4, |_, row| tone(row, &tb, quirk));
    out
}

/// Water Paper's brightness/contrast: each pixel scaled by `tb[m]/m`, m its brightest channel.
fn tone(row: &mut [u8], tb: &[u8; 256], quirk: bool) {
    for px in row.chunks_exact_mut(4) {
        let (r, g, b) = (px[0], px[1], px[2]);
        // The spec compares blue with red instead of with the running maximum.
        let m = if quirk {
            let m = if g > r { g } else { r };
            if b > r { b } else { m }
        } else {
            r.max(g).max(b)
        };
        let lv = tb[m as usize];
        if lv == 0 || m == 0 {
            px[..3].fill(0);
            continue;
        }
        for v in px[..3].iter_mut() {
            let s = (lv as f64 * *v as f64 / m as f64).trunc();
            *v = if quirk { px8::wrap(s) } else { px8::clamp255(s) };
        }
    }
}

/// A `w × h` plane transposed (`h × w`).
fn transpose(p: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h];
    for (y, row) in p.chunks_exact(w.max(1)).enumerate().take(h) {
        for (x, v) in row.iter().enumerate() {
            out[x * h + y] = *v;
        }
    }
    out
}
