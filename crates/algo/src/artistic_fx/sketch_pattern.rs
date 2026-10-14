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
/// A streak inks when `r / rs + ro ≤ τ − gray` (r uniform, rs = 1 + length/10). Photoshop's range
/// ends at 1 (ro = 1 − 1/rs), so at balance 50 the ink fades linearly from black to gray ≈ 100;
/// the reference centred it (ro = (1 − 1/rs)/2) and inked everything darker than mid-gray.
pub(in super::super) fn graphic_pen(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let len = int(e, "strokeLength").min(1000);
    let bal = int(e, "lightDarkBalance").min(100);
    let tau = if bal <= 50 { tables::GRAPHIC_PEN_DENSITY[bal] / 255.0 } else { 1.0 + (255.0 - tables::GRAPHIC_PEN_DENSITY[101 - bal]) / 255.0 };
    let rs = 1.0 + 0.1 * len as f64;
    let ro = if px8::defects() { (1.0 - 1.0 / rs) / 2.0 } else { 1.0 - 1.0 / rs };
    let n: Vec<f32> = px8::gray_mean(img).iter().map(|&g| (g as f64 / 255.0) as f32).collect();
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
        let d = tau - nv as f64;
        let r = rng.next();
        let r2 = r / rs + ro;
        let jump = st.pg.is_some_and(|pg| ((pg - nv) as f64).abs() > 0.0625);
        if st.c == 0 || jump {
            st.pv = if r2 <= d { 0 } else { 255 };
            st.c = (rng.next() * len as f64) as i64;
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

/// The pen's state carried along the walk.
struct Pen {
    pv: u8,
    pg: Option<f32>,
    c: i64,
}

/// Halftone Pattern: the box-blurred gray modulated by a dot, circle or line screen, then a
/// contrast stretch around the image's mean.
///
/// Photoshop's screen is gentle (±10 %) with its dark dots on the cell centres counted from the
/// image origin, and its output has 32 levels. The reference used a centred screen that only
/// darkens (×0.75 … ×1), which left the image a third darker.
pub(in super::super) fn halftone(e: &GalleryEffect, img: &Img) -> Img {
    let (w, h) = (img.w, img.h);
    let size = int(e, "size").min(1000);
    let contrast = e.get("contrast") as f64;
    let cell = (2 * size + 1) as f64;
    let f = std::f64::consts::PI / cell;
    let g = px8::box_pass_u8(&px8::gray_round(img), w, h, size);
    let pattern = e.choice("patternType");
    let quirk = px8::defects();
    // Screen origin, and the gray's multiplier for screen value s: `mul + amp·s`.
    let (ox, oy, mul, amp, k_base, k_spread) =
        if quirk { ((w / 2) as f64, (h / 2) as f64, 0.75, 0.25, 1.26, 0.25) } else { (size as f64, size as f64, 1.0, -0.1, 1.56, 0.215) };
    let (cx, cy) = ((w / 2) as f64, (h / 2) as f64);
    let mut v = vec![0f64; w * h];
    let (mut sum, mut wsum) = (0.0, 0.0);
    for y in 0..h {
        let ay = (y as f64 - oy) * f;
        for x in 0..w {
            let i = y * w + x;
            let ax = (x as f64 - ox) * f;
            let sigma = match pattern {
                "line" => Some(ay.cos()),
                "circle" => {
                    let (rx, ry) = ((x as f64 - cx) * f, (y as f64 - cy) * f);
                    let k = (0.5 + (rx * rx + ry * ry).sqrt() * 4.5).trunc();
                    // The reference reads a sine table of 2W entries: beyond it the pixel is 0.
                    (!quirk || k < 2.0 * w as f64).then(|| (k * (1.0 / 4.5)).sin())
                }
                _ => Some(ax.cos() * ay.cos()),
            };
            let s = sigma.map_or(0.0, |s| {
                let s = g[i] as f64 * (mul + amp * s);
                if quirk { px8::clamp255(s.trunc()) as f64 } else { s.clamp(0.0, 255.0) }
            });
            v[i] = s;
            let a = img.d[i * 4 + 3] as f64 / 255.0;
            sum += s * a;
            wsum += a;
        }
    }
    let mean = if wsum > 0.0 { sum / wsum } else { 128.0 };
    let base = 128.0 + k_base * (mean - 128.0);
    let spread = 0.08 + k_spread * ((128.0 - mean) / 128.0).abs();
    let gain = 1.0 + spread * contrast;
    let out: Vec<u8> = v
        .iter()
        .map(|&p| {
            let s = px8::clamp255((base + (p - base) * gain).trunc());
            if contrast > 46.0 {
                if s < 128 { 0 } else { 255 }
            } else if quirk {
                s
            } else {
                // 32 levels.
                px8::clamp255(((s as f64 * 31.0 / 255.0).round() * 255.0 / 31.0).round())
            }
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
        // The reference compares blue with red instead of with the running maximum.
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
