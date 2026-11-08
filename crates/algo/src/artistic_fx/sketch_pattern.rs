//! Sketch filters drawing a pattern over the image: Graphic Pen, Halftone Pattern, Water Paper.

use super::super::px8::{self, Img};
use super::{ink_paper, int};
use crate::artistic::GalleryEffect;
use crate::photo_util::par_rows;

/// Graphic Pen: a sequential random pen walking the image along the stroke direction. Each
/// streak starts inked or blank by comparing a random number with the pixel's darkness, and
/// continues while the gray stays close, ending at random or after a random length.
///
/// A streak inks when `u·ramp ≤ c + ramp/2 − gray`: a draw `u` in 0–1 spread over a ramp of
/// grays centred on `c`. The spec used a uniform `u`, a ramp `1/rs` wide with `rs = 1 + length/10`
/// and a tabled threshold, and streaks of `trunc(r·length)` more pixels.
///
/// The Reference's ramp has the same width wherever it sits and only its centre moves with the
/// Light/Dark Balance: the centre is where the expected share of inked pixels is the balance (in
/// percent), so the ink share grows linearly with the balance on any image, up to a centre of
/// mid-gray ([`GP_CAP`]), past which it stays put. A dark image reaches it late (the parrot at 88,
/// with 87.5 % ink), a bright one early (the boat at 45, with 45 % ink, so its 50 and 100 render
/// the same). The ramp is [`GP_RAMP`] wide at length 15, and `u` is the mean of two uniforms there,
/// which softens the ends of the ramp the way the Reference's are. Its streaks are mostly short
/// with a long tail (four in ten are one pixel long, some run past the length setting):
/// `trunc(r³·1.3·(length − 1))` more pixels, so that at length 1 every pixel draws on its own,
/// as the Reference's do (its streaks there are no longer along the stroke direction than across).
pub(in super::super) fn graphic_pen(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let len = int(e, "strokeLength").min(1000);
    let bal = int(e, "lightDarkBalance").min(100);
    let quirk = px8::defects();
    // The Reference's ramp narrows with the stroke length, from the whole gray range at length 1
    // (ink in proportion to darkness) to [`GP_RAMP`] at 15, most of the way by 8 (its ramp there is
    // 0.68 wide, measured on how far apart the grays inked a quarter and three quarters of the
    // time are, and on the 10 % and 90 % points); its draw goes from one uniform to the mean of two
    // at the same pace.
    let k = ((len.clamp(1, 15) - 1) as f64 / 14.0).sqrt();
    let (ramp, mix) = if quirk { (1.0 / (1.0 + 0.1 * len as f64), 0.0) } else { (1.0 + (GP_RAMP - 1.0) * k, k / 2.0) };
    let gray = px8::gray_mean(img);
    let n: Vec<f32> = gray.iter().map(|&g| (g as f64 / 255.0) as f32).collect();
    let top = gp_centre(&gray, img, bal as f64 / 100.0, ramp, mix) + ramp / 2.0;
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
        let d = top - nv as f64;
        let r = rng.next();
        let jump = st.pg.is_some_and(|pg| ((pg - nv) as f64).abs() > 0.0625);
        if st.c == 0 || jump {
            let u = if quirk { r } else { r + (rng.next() - r) * mix };
            st.pv = if u * ramp <= d { 0 } else { 255 };
            let r2 = rng.next();
            st.c = if quirk { (r2 * len as f64) as i64 } else { (r2 * r2 * r2 * 1.3 * len.saturating_sub(1) as f64) as i64 };
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

/// Graphic Pen's ink ramp width (gray 0–1) at stroke length 15 and over.
const GP_RAMP: f64 = 0.55;
/// The darkest Graphic Pen's ramp centre goes (gray 0–1): mid-gray.
const GP_CAP: f64 = 0.5;

/// Graphic Pen's ramp centre: where the alpha-weighted expected share of inked pixels is `share`,
/// for a draw `(1 − mix)·U₁ + mix·U₂` (two uniforms) spread over `ramp`, capped at [`GP_CAP`].
/// Below any ink at share 0 or less, or on an empty image.
fn gp_centre(gray: &[u8], img: &Img, share: f64, ramp: f64, mix: f64) -> f64 {
    let none = -ramp;
    let mut hist = [0f64; 256];
    for (v, px) in gray.iter().zip(img.d.chunks_exact(4)) {
        hist[*v as usize] += px[3] as f64;
    }
    let total: f64 = hist.iter().sum();
    // NaN fails every comparison, so it lands here too.
    let usable = share > 0.0 && total > 0.0 && ramp > 0.0;
    if !usable {
        return none;
    }
    let ink = |c: f64| {
        let s: f64 = hist.iter().enumerate().map(|(g, w)| w * draw_cdf((c + ramp / 2.0 - g as f64 / 255.0) / ramp, mix)).sum();
        s / total
    };
    if ink(GP_CAP) <= share {
        return GP_CAP;
    }
    let (mut lo, mut hi) = (none, GP_CAP);
    for _ in 0..40 {
        let m = (lo + hi) / 2.0;
        if ink(m) < share { lo = m } else { hi = m }
    }
    (lo + hi) / 2.0
}

/// `P((1 − m)·U₁ + m·U₂ ≤ x)` for two uniforms on 0–1, `m` in 0–½ (a trapezoid's CDF).
fn draw_cdf(x: f64, m: f64) -> f64 {
    let (a, b) = (1.0 - m, m.clamp(0.0, 0.5));
    if x.is_nan() || x <= 0.0 {
        0.0
    } else if x >= 1.0 {
        1.0
    } else if b <= 0.0 {
        x
    } else if x < b {
        x * x / (2.0 * a * b)
    } else if x < a {
        (x - b / 2.0) / a
    } else {
        1.0 - (1.0 - x) * (1.0 - x) / (2.0 * a * b)
    }
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
    let mut fine = vec![0f64; w * h];
    for (y, row) in fine.chunks_exact_mut(w.max(1)).enumerate() {
        for (x, p) in row.iter_mut().enumerate() {
            let b = blurred.get((y + r2) * pw + x + r2).copied().unwrap_or(0.0);
            *p = b as f64 * scale;
        }
    }
    let v: Vec<u8> = fine.iter().map(|&b| px8::clamp255(b.floor())).collect();
    // The clip levels, from a histogram weighted by alpha.
    let mut hist = [0f64; 256];
    for (p, px) in v.iter().zip(img.d.chunks_exact(4)) {
        hist[*p as usize] += px[3] as f64;
    }
    let cut = clip * hist.iter().sum::<f64>();
    let lo = clip_level(&hist, cut, 0..256).unwrap_or(0) as f64;
    let hi = clip_level(&hist, cut, (0..256).rev()).unwrap_or(255) as f64;
    let out: Vec<u8> = if hi <= lo {
        // Two tones: the Reference splits at the median of the unrounded plane (half the pixels
        // light, not every pixel of the median level dark).
        let t = weighted_median(&fine, img).unwrap_or(lo + 1.0);
        fine.iter().map(|&p| if p > t { 255 } else { 0 }).collect()
    } else {
        v.iter().map(|&p| px8::clamp255(((p as f64 - lo).clamp(0.0, hi - lo) * 255.0 / (hi - lo)).round())).collect()
    };
    let (fg, bg) = ink_paper(e);
    px8::colourise_direct(&out, fg, bg, img)
}

/// Halftone Pattern's screen amplitude at size 1 (dots and lines).
const HT_AMP: f64 = 0.374;
/// How much the screen amplitude grows per size step (relative): 0.374 at 1, 0.8 at 12.
const HT_AMP_GROWTH: f64 = (0.8 / HT_AMP - 1.0) / 11.0;
/// The exponent narrowing the dot and line screens' peaks.
const HT_PEAK: f64 = 1.64;

/// The alpha-weighted median of a plane (None when fully transparent).
fn weighted_median(p: &[f64], img: &Img) -> Option<f64> {
    let mut v: Vec<(f64, f64)> = p.iter().zip(img.d.chunks_exact(4)).filter(|(_, px)| px[3] > 0).map(|(&p, px)| (p, px[3] as f64)).collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    let half = v.iter().map(|x| x.1).sum::<f64>() / 2.0;
    let mut acc = 0.0;
    v.into_iter()
        .find(|&(_, a)| {
            acc += a;
            acc >= half
        })
        .map(|x| x.0)
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artistic::GalleryFilter as F;

    /// An opaque `w × h` image whose gray runs over `lo..=hi` in a smooth diagonal ramp.
    fn ramp_image(w: usize, h: usize, lo: u8, hi: u8) -> Img {
        let mut img = Img::new(w, h);
        for (i, px) in img.d.chunks_exact_mut(4).enumerate() {
            let t = ((i % w) + (i / w)) as f64 / (w + h - 2) as f64;
            let v = (lo as f64 + t * (hi - lo) as f64).round() as u8;
            px.copy_from_slice(&[v, v, v, 255]);
        }
        img
    }

    fn ink_share(out: &Img) -> f64 {
        out.d.chunks_exact(4).filter(|p| p[0] < 128).count() as f64 / out.d.len().max(1) as f64 * 4.0
    }

    fn pen(img: &Img, balance: f32) -> Img {
        let mut e = GalleryEffect::new(F::GraphicPen);
        e.set("lightDarkBalance", balance);
        graphic_pen(&e, img)
    }

    #[test]
    fn graphic_pen_inks_the_balance_share_until_mid_gray() {
        // A dark image: the ink share follows the balance, as the Reference's does.
        let dark = ramp_image(160, 120, 0, 160);
        for b in [10.0, 30.0, 50.0, 70.0] {
            let s = ink_share(&pen(&dark, b));
            assert!((s - b as f64 / 100.0).abs() < 0.04, "balance {b}: ink {s}");
        }
        assert_eq!(ink_share(&pen(&dark, 0.0)), 0.0);
        // A bright image reaches the mid-gray ceiling before 50: 50 and 100 draw the same.
        let bright = ramp_image(160, 120, 90, 255);
        assert_eq!(pen(&bright, 50.0).d, pen(&bright, 100.0).d);
        assert!(ink_share(&pen(&bright, 50.0)) < 0.4);
    }

    #[test]
    fn graphic_pen_centre_and_draw_stay_bounded() {
        let img = ramp_image(8, 8, 0, 255);
        let gray = px8::gray_mean(&img);
        for share in [f64::NAN, -1.0, 0.0, 0.3, 1.0, 1e9] {
            let c = gp_centre(&gray, &img, share, GP_RAMP, 0.5);
            assert!(c.is_finite() && c <= GP_CAP, "{share}: {c}");
        }
        let mut prev = 0.0;
        for m in [0.0, 0.25, 0.5] {
            for i in -10..=110 {
                let p = draw_cdf(i as f64 / 100.0, m);
                assert!((0.0..=1.0).contains(&p) && (i <= -10 || p >= prev), "m {m} at {i}: {p}");
                prev = p;
            }
            prev = 0.0;
        }
        assert_eq!(draw_cdf(f64::NAN, 0.5), 0.0);
        assert!((draw_cdf(0.5, 0.5) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn halftone_at_full_contrast_splits_at_the_median() {
        let img = ramp_image(90, 60, 20, 230);
        let mut e = GalleryEffect::new(F::HalftonePattern);
        e.set("contrast", 50.0);
        let out = halftone(&e, &img);
        let light = 1.0 - ink_share(&out);
        assert!((light - 0.5).abs() < 0.02, "light share {light}");
        assert!(out.d.chunks_exact(4).all(|p| p[0] == 0 || p[0] == 255));
    }
}
