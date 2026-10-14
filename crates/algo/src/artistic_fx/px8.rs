//! The Filter Gallery's 8-bit toolkit: an RGBA8 working image, byte planes, the random number
//! generators and the shared primitives the category modules build their filters from.
//!
//! Every filter in the gallery works the way the reference's does: on 8-bit straight-alpha RGBA,
//! over the whole image at once (whole-image histograms, random numbers drawn in raster order,
//! textures and grids anchored at the image origin). Planes are row-major, `w × h` bytes.
//! Neighbourhood primitives clamp coordinates to the image unless they say otherwise.
//!
//! The box mean and the two rank filters are ports of the PhotoSuite authors' WebAssembly
//! kernels; their cost does not grow with the window. Rows run in parallel where the rows of a
//! pass are independent.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::photo_util::{par_map, par_rows};

/// Test switch: reproduce the edge defects of the spec renders (see [`defects`]).
static DEFECTS: AtomicBool = AtomicBool::new(false);

/// Whether the edge defects of the spec renders are reproduced: the four-neighbour
/// sharpen/smoothing ignoring the pixel below, the relief kernel's black last row, the gray
/// writer's nudged first pixel. Off in the app (the reference has none of them); the comparison
/// against the spec renders turns it on.
pub(super) fn defects() -> bool {
    DEFECTS.load(Ordering::Relaxed)
}

#[cfg(test)]
pub(super) fn set_defects(on: bool) {
    DEFECTS.store(on, Ordering::Relaxed);
}

// ---- numbers -----------------------------------------------------------------------------------

/// Stores `v` into a byte the wrapping way: truncate toward zero, then modulo 256; NaN/∞ → 0.
#[inline]
pub(super) fn wrap(v: f64) -> u8 {
    if !v.is_finite() {
        return 0;
    }
    (v.trunc() % 256.0 + 256.0) as u32 as u8
}

/// Stores `v` into a byte the saturating way: nearest (ties to even), clamped; NaN → 0.
#[inline]
pub(super) fn sat(v: f64) -> u8 {
    if v.is_nan() {
        return 0;
    }
    v.round_ties_even().clamp(0.0, 255.0) as u8
}

/// `trunc(min(255, max(0, v)))`, NaN → 0.
#[inline]
pub(super) fn clamp255(v: f64) -> u8 {
    if v.is_nan() {
        return 0;
    }
    v.clamp(0.0, 255.0) as u8
}

/// Round half up: `floor(x + 0.5)`.
#[inline]
pub(super) fn rhu(x: f64) -> f64 {
    (x + 0.5).floor()
}

// ---- images ------------------------------------------------------------------------------------

/// A straight-alpha RGBA8 image.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Img {
    pub w: usize,
    pub h: usize,
    /// `w·h·4` bytes, R G B A per pixel.
    pub d: Vec<u8>,
}

impl Img {
    pub fn new(w: usize, h: usize) -> Img {
        Img { w, h, d: vec![0; w * h * 4] }
    }
    pub fn n(&self) -> usize {
        self.w * self.h
    }
    /// Channel `c` (0..4) as a plane.
    pub fn plane(&self, c: usize) -> Vec<u8> {
        self.d.chunks_exact(4).map(|p| p[c]).collect()
    }
    /// Writes `p` into channel `c`.
    pub fn set_plane(&mut self, c: usize, p: &[u8]) {
        for (px, v) in self.d.chunks_exact_mut(4).zip(p) {
            px[c] = *v;
        }
    }
    /// An image with this one's alpha and `rgb` (three planes) as colour.
    pub fn with_rgb(&self, r: &[u8], g: &[u8], b: &[u8]) -> Img {
        let mut out = self.clone();
        for (i, px) in out.d.chunks_exact_mut(4).enumerate() {
            px[0] = r[i];
            px[1] = g[i];
            px[2] = b[i];
        }
        out
    }
    /// Applies `f` to each of R, G and B as planes, keeping alpha.
    pub fn map_rgb(&self, f: impl Fn(&[u8]) -> Vec<u8> + Sync) -> Img {
        let ch: Vec<Vec<u8>> = par_map(3, |c| f(&self.plane(c)));
        self.with_rgb(&ch[0], &ch[1], &ch[2])
    }
    /// Whether every pixel is fully opaque.
    pub fn opaque(&self) -> bool {
        self.d.chunks_exact(4).all(|p| p[3] == 255)
    }
}

/// An 8-bit colour from a straight 0–1 one.
pub(super) fn rgb8(c: [f32; 4]) -> [u8; 3] {
    [0, 1, 2].map(|i| (c[i].clamp(0.0, 1.0) * 255.0).round() as u8)
}

// ---- gray --------------------------------------------------------------------------------------

/// Mean gray `trunc((R + G + B + 2)/3)` (P-05).
pub(super) fn gray_mean(img: &Img) -> Vec<u8> {
    img.d.chunks_exact(4).map(|p| (p[0] as u32 + p[1] as u32 + p[2] as u32).div_ceil(3) as u8).collect()
}

/// Weighted gray `trunc(0.3R + 0.59G + 0.11B + 0.5)` (P-06).
pub(super) fn gray_weighted(img: &Img) -> Vec<u8> {
    img.d.chunks_exact(4).map(|p| wrap(0.3 * p[0] as f64 + 0.59 * p[1] as f64 + 0.11 * p[2] as f64 + 0.5)).collect()
}

/// Rounded mean gray `trunc(0.5 + (R + G + B)/3)` (P-06b).
pub(super) fn gray_round(img: &Img) -> Vec<u8> {
    img.d.chunks_exact(4).map(|p| wrap(0.5 + (p[0] as f64 + p[1] as f64 + p[2] as f64) * (1.0 / 3.0))).collect()
}

/// Largest of R, G, B (P-06c).
pub(super) fn max_channel(img: &Img) -> Vec<u8> {
    img.d.chunks_exact(4).map(|p| p[0].max(p[1]).max(p[2])).collect()
}

// ---- HSV ---------------------------------------------------------------------------------------

/// RGB → byte HSV in place (P-08): H in 0..255 (43 per sextant), S and V 0..255. Alpha kept.
pub(super) fn to_hsv(img: &mut Img) {
    let w = img.w;
    par_rows(&mut img.d, w, 4, |_, row| {
        for p in row.chunks_exact_mut(4) {
            let (r, g, b) = (p[0] as i32, p[1] as i32, p[2] as i32);
            let (mut max, mut min, mut idx) = (r, r, 0);
            if g > r {
                max = g;
                idx = 1;
            } else {
                min = g;
            }
            if b > max {
                max = b;
                idx = 2;
            } else if b < min {
                min = b;
            }
            let d = max - min;
            let (h, s) = if d == 0 {
                (0u8, 0u8)
            } else {
                let df = d as f64;
                let h = match idx {
                    0 => (43.0 * (g - b) as f64 / df).trunc(),
                    1 => (43.0 * (b + 2 * d - r) as f64 / df).trunc(),
                    _ => 43.0 * (r + 4 * d - g) as f64 / df,
                };
                let h = if h < 0.0 { (((h as i32) - 1) & 255) as f64 } else { h };
                (wrap(h), wrap(255.0 * df / max as f64))
            };
            p[0] = h;
            p[1] = s;
            p[2] = max as u8;
        }
    });
}

/// Byte HSV → RGB in place (P-08). Alpha kept.
pub(super) fn from_hsv(img: &mut Img) {
    let w = img.w;
    par_rows(&mut img.d, w, 4, |_, row| {
        for p in row.chunks_exact_mut(4) {
            let (h, s, v) = (p[0] as f64, p[1] as f64, p[2] as f64);
            if p[1] == 0 {
                p[0] = p[2];
                p[1] = p[2];
                continue;
            }
            let h = if p[0] == 255 { 0.0 } else { h };
            let k = (6.0 * h / 255.0).trunc();
            let f = (6.0 * h - 255.0 * k).trunc();
            let pp = (v * (255.0 - s) / 255.0).trunc();
            let q = (v * (255.0 - s * f / 255.0) / 255.0).trunc();
            let t = (v * (255.0 - s * (255.0 - f) / 255.0) / 255.0).trunc();
            let (r, g, b) = match k as i32 {
                0 => (v, t, pp),
                1 => (q, v, pp),
                2 => (pp, v, t),
                3 => (pp, q, v),
                4 => (t, pp, v),
                _ => (v, pp, q),
            };
            p[0] = wrap(r);
            p[1] = wrap(g);
            p[2] = wrap(b);
        }
    });
}

// ---- box mean (port of the authors' `box_blur`) ------------------------------------------------

#[inline]
fn clampi(v: i64, n: usize) -> usize {
    v.clamp(0, n as i64 - 1) as usize
}

/// Clamped-edge box mean of a plane over a `kw × kh` window (P-07), `trunc(sum / area)`. Even
/// windows reach one further up/left (half = k >> 1). A zero-size window gives zeros.
pub(super) fn box_mean(src: &[u8], w: usize, h: usize, kw: usize, kh: usize) -> Vec<u8> {
    if kw == 0 || kh == 0 || w == 0 || h == 0 {
        return vec![0; w * h];
    }
    let (hw, hh) = ((kw >> 1) as i64, (kh >> 1) as i64);
    let area = (kw * kh) as u64;
    // Vertical running sums, row by row (each row is one pass over the width).
    let mut col = vec![0u32; w * h];
    let mut sum = vec![0u32; w];
    for k in 0..kh as i64 {
        let sy = clampi(-hh + k, h);
        for x in 0..w {
            sum[x] += src[sy * w + x] as u32;
        }
    }
    col[..w].copy_from_slice(&sum);
    for row in 1..h {
        let drop = clampi(row as i64 - 1 - hh, h);
        let take = clampi(row as i64 - 1 - hh + kh as i64, h);
        for x in 0..w {
            sum[x] = sum[x] + src[take * w + x] as u32 - src[drop * w + x] as u32;
        }
        col[row * w..(row + 1) * w].copy_from_slice(&sum);
    }
    let mut dst = vec![0u8; w * h];
    par_rows(&mut dst, w, 1, |row, out| {
        let c = &col[row * w..(row + 1) * w];
        let mut s: u64 = (0..kw as i64).map(|k| c[clampi(-hw + k, w)] as u64).sum();
        out[0] = (s / area) as u8;
        for x in 1..w {
            let drop = clampi(x as i64 - 1 - hw, w);
            let take = clampi(x as i64 - 1 - hw + kw as i64, w);
            s = s + c[take] as u64 - c[drop] as u64;
            out[x] = (s / area) as u8;
        }
    });
    dst
}

/// [`box_mean`] of R, G and B; alpha kept.
pub(super) fn box_mean_rgb(img: &Img, kw: usize, kh: usize) -> Img {
    img.map_rgb(|p| box_mean(p, img.w, img.h, kw, kh))
}

// ---- rank filters (port of the authors' `median_h` / `median_dir`) -----------------------------

/// Running histogram: 256 bins plus 16 coarse buckets.
struct Hist {
    fine: [u32; 256],
    coarse: [u32; 16],
}

impl Hist {
    fn new() -> Hist {
        Hist { fine: [0; 256], coarse: [0; 16] }
    }
    #[inline]
    fn add(&mut self, v: u8) {
        self.fine[v as usize] += 1;
        self.coarse[(v >> 4) as usize] += 1;
    }
    #[inline]
    fn remove(&mut self, v: u8) {
        self.fine[v as usize] -= 1;
        self.coarse[(v >> 4) as usize] -= 1;
    }
    /// The smallest value whose cumulative count reaches `target` (1 ≤ target ≤ count).
    #[inline]
    fn rank(&self, target: u32) -> u8 {
        let mut cum = 0u32;
        let mut bin = 0usize;
        while bin < 256 && cum + self.coarse[bin >> 4] < target {
            cum += self.coarse[bin >> 4];
            bin += 16;
        }
        while bin < 256 {
            cum += self.fine[bin];
            if cum >= target {
                break;
            }
            bin += 1;
        }
        bin.min(255) as u8
    }
}

/// The integer rank a fractional one stands for: the smallest value whose cumulative count is
/// at least `rank`, i.e. `ceil(rank)`; ≤ 0 means "output 0".
fn rank_target(rank: f64) -> u32 {
    if rank.is_nan() || rank <= 0.0 { 0 } else { rank.ceil().min(u32::MAX as f64) as u32 }
}

/// Axis-aligned rank filter (P-11): the `rank`-th smallest value of the `kw × kh` window
/// (edges clamped; `rank` = area/2 is the median). Fractional ranks round up; rank ≤ 0 gives 0;
/// a rank above the area is the area.
pub(super) fn rank_rect(src: &[u8], w: usize, h: usize, kw: usize, kh: usize, rank: f64) -> Vec<u8> {
    let mut dst = vec![0u8; w * h];
    let target = rank_target(rank).min((kw * kh) as u32);
    if target == 0 || w == 0 || h == 0 {
        return dst;
    }
    let (hw, hh) = ((kw >> 1) as i64, (kh >> 1) as i64);
    par_rows(&mut dst, w, 1, |row, out| {
        let mut hist = Hist::new();
        let rows: Vec<usize> = (0..kh as i64).map(|k| clampi(row as i64 - hh + k, h)).collect();
        for &sy in &rows {
            for k in 0..kw as i64 {
                hist.add(src[sy * w + clampi(-hw + k, w)]);
            }
        }
        out[0] = hist.rank(target);
        for x in 1..w {
            let drop = clampi(x as i64 - 1 - hw, w);
            let take = clampi(x as i64 - 1 - hw + kw as i64, w);
            for &sy in &rows {
                let (old, new) = (src[sy * w + drop], src[sy * w + take]);
                if old != new {
                    hist.remove(old);
                    hist.add(new);
                }
            }
            out[x] = hist.rank(target);
        }
    });
    dst
}

/// Sheared rank filter (P-12): `len` rows, each a run of `span` pixels shifted one column per
/// row, the parallelogram leaning right (or left when `reverse`). Same rank rules as
/// [`rank_rect`], except that a rank above the window gives 0.
pub(super) fn rank_dir(src: &[u8], w: usize, h: usize, len: usize, span: usize, reverse: bool, rank: f64) -> Vec<u8> {
    let mut dst = vec![0u8; w * h];
    let target = rank_target(rank);
    if target == 0 || target as usize > len * span || w == 0 || h == 0 {
        return dst;
    }
    let (len_i, span_i) = (len as i64, span as i64);
    let diag = len_i + span_i - 1;
    let (hl, hd) = (len_i >> 1, diag >> 1);
    // Rows padded by `m` replicated edge pixels on both sides: no window run leaves them.
    let m = diag as usize + 1;
    let pw = w + 2 * m;
    let pad = pad_rows(src, w, h, m);
    par_rows(&mut dst, w, 1, |row, out| {
        let mut hist = Hist::new();
        // For each step: the padded index of its run's first sample at column 0.
        let steps: Vec<usize> = (0..len_i)
            .map(|s| {
                let start = if reverse { diag - s - span_i } else { s };
                (clampi(row as i64 - hl + s, h) * pw) as i64 + m as i64 - hd + start
            })
            .map(|i| i.max(0) as usize)
            .collect();
        for &b in &steps {
            for o in 0..span {
                hist.add(pad[b + o]);
            }
        }
        out[0] = hist.rank(target);
        for x in 1..w {
            for &b in &steps {
                let (old, new) = (pad[b + x - 1], pad[b + x - 1 + span]);
                if old != new {
                    hist.remove(old);
                    hist.add(new);
                }
            }
            out[x] = hist.rank(target);
        }
    });
    dst
}

/// `p` with every row extended by `m` copies of its first and last pixel on either side.
fn pad_rows(p: &[u8], w: usize, h: usize, m: usize) -> Vec<u8> {
    let pw = w + 2 * m;
    let mut out = vec![0u8; pw * h];
    for (src, dst) in p.chunks_exact(w).zip(out.chunks_exact_mut(pw)) {
        let (first, last) = (src[0], src[w - 1]);
        dst[..m].fill(first);
        dst[m..m + w].copy_from_slice(src);
        dst[m + w..].fill(last);
    }
    out
}

/// [`rank_rect`] of R, G and B; alpha kept.
pub(super) fn rank_rect_rgb(img: &Img, kw: usize, kh: usize, rank: f64) -> Img {
    img.map_rgb(|p| rank_rect(p, img.w, img.h, kw, kh, rank))
}

/// [`rank_dir`] of R, G and B; alpha kept.
pub(super) fn rank_dir_rgb(img: &Img, len: usize, span: usize, reverse: bool, rank: f64) -> Img {
    img.map_rgb(|p| rank_dir(p, img.w, img.h, len, span, reverse, rank))
}

/// Mean over the same parallelogram as [`rank_dir`] (P-10), each of R, G, B; alpha kept.
pub(super) fn box_dir_rgb(img: &Img, len: usize, span: usize, reverse: bool) -> Img {
    let (w, h) = (img.w, img.h);
    let n = (len * span) as u64;
    if n == 0 {
        return img.clone();
    }
    let (len_i, span_i) = (len as i64, span as i64);
    let diag = len_i + span_i - 1;
    let (hl, hd) = (len_i >> 1, diag >> 1);
    img.map_rgb(|src| {
        let mut dst = vec![0u8; w * h];
        par_rows(&mut dst, w, 1, |row, out| {
            let steps: Vec<(usize, i64)> = (0..len_i)
                .map(|s| {
                    let start = if reverse { diag - s - span_i } else { s };
                    (clampi(row as i64 - hl + s, h), -hd + start)
                })
                .collect();
            let mut sum: u64 = 0;
            for &(sy, x0) in &steps {
                for o in 0..span_i {
                    sum += src[sy * w + clampi(x0 + o, w)] as u64;
                }
            }
            out[0] = (sum / n) as u8;
            for x in 1..w as i64 {
                for &(sy, x0) in &steps {
                    sum -= src[sy * w + clampi(x0 + x - 1, w)] as u64;
                    sum += src[sy * w + clampi(x0 + x - 1 + span_i, w)] as u64;
                }
                out[x as usize] = (sum / n) as u8;
            }
        });
        dst
    })
}

/// Mode filter over a `k × k` window (P-13): the most frequent value, ties to the smallest.
pub(super) fn mode(src: &[u8], w: usize, h: usize, k: usize) -> Vec<u8> {
    let mut dst = vec![0u8; w * h];
    if k == 0 || w == 0 || h == 0 {
        return dst;
    }
    let half = (k >> 1) as i64;
    par_rows(&mut dst, w, 1, |row, out| {
        let mut hist = [0u32; 256];
        let rows: Vec<usize> = (0..k as i64).map(|i| clampi(row as i64 - half + i, h)).collect();
        for &sy in &rows {
            for i in 0..k as i64 {
                hist[src[sy * w + clampi(-half + i, w)] as usize] += 1;
            }
        }
        let best = |hist: &[u32; 256]| {
            let (mut b, mut c) = (0u8, 0u32);
            for (v, &n) in hist.iter().enumerate() {
                if n > c {
                    c = n;
                    b = v as u8;
                }
            }
            b
        };
        let mut b = best(&hist);
        let mut c = hist[b as usize];
        out[0] = b;
        for x in 1..w {
            let drop = clampi(x as i64 - 1 - half, w);
            let take = clampi(x as i64 - 1 - half + k as i64, w);
            for &sy in &rows {
                hist[src[sy * w + drop] as usize] -= 1;
                hist[src[sy * w + take] as usize] += 1;
            }
            // Unless the mode lost count, only a value just added can overtake it.
            if hist[b as usize] >= c {
                c = hist[b as usize];
                for &sy in &rows {
                    let v = src[sy * w + take];
                    let n = hist[v as usize];
                    if n > c || (n == c && v < b) {
                        (b, c) = (v, n);
                    }
                }
            } else {
                b = best(&hist);
                c = hist[b as usize];
            }
            out[x] = b;
        }
    });
    dst
}

/// Square rank filter whose window is clipped to the image, not clamped (P-27).
#[derive(Clone, Copy, Debug)]
pub(super) enum Clipped {
    /// The element at 0-based position `trunc(0.5 + p·n)` of the sorted window.
    Percentile(f64),
    Max,
    Min,
}

fn clipped_plane(src: &[u8], weight: Option<&[u8]>, w: usize, h: usize, r: usize, sel: Clipped) -> Vec<Option<u8>> {
    let ri = r as i64;
    par_map(h, |y| {
        let y0 = (y as i64 - ri).max(0) as usize;
        let y1 = ((y as i64 + ri) as usize).min(h - 1);
        // Fine bins, 16 coarse buckets and the total, kept up to date as columns enter and leave.
        let mut hist = [0u64; 256];
        let mut coarse = [0u64; 16];
        let mut n = 0u64;
        let mut row = Vec::with_capacity(w);
        let mut add = |hist: &mut [u64; 256], coarse: &mut [u64; 16], x: usize, plus: bool| {
            for yy in y0..=y1 {
                let i = yy * w + x;
                let c = weight.map_or(1, |a| a[i] as u64);
                let v = src[i] as usize;
                if plus {
                    hist[v] += c;
                    coarse[v >> 4] += c;
                    n += c;
                } else {
                    hist[v] -= c;
                    coarse[v >> 4] -= c;
                    n -= c;
                }
            }
            n
        };
        let mut total = 0;
        for x in 0..=r.min(w - 1) {
            total = add(&mut hist, &mut coarse, x, true);
        }
        for x in 0..w {
            if x > 0 {
                if x + r < w {
                    total = add(&mut hist, &mut coarse, x + r, true);
                }
                if x > r {
                    total = add(&mut hist, &mut coarse, x - r - 1, false);
                }
            }
            if total == 0 {
                row.push(None);
                continue;
            }
            let v = match sel {
                Clipped::Max => {
                    let b = coarse.iter().rposition(|&c| c > 0).unwrap_or(0) * 16;
                    (b + hist[b..b + 16].iter().rposition(|&c| c > 0).unwrap_or(0)) as u8
                }
                Clipped::Min => {
                    let b = coarse.iter().position(|&c| c > 0).unwrap_or(0) * 16;
                    (b + hist[b..b + 16].iter().position(|&c| c > 0).unwrap_or(0)) as u8
                }
                Clipped::Percentile(p) => {
                    let t = (0.5 + p * total as f64).trunc().max(0.0) as u64;
                    let mut cum = 0u64;
                    let mut bin = 0usize;
                    while bin < 256 && cum + coarse[bin >> 4] <= t {
                        cum += coarse[bin >> 4];
                        bin += 16;
                    }
                    let mut v = 255u8;
                    while bin < 256 {
                        cum += hist[bin];
                        if cum > t {
                            v = bin as u8;
                            break;
                        }
                        bin += 1;
                    }
                    v
                }
            };
            row.push(Some(v));
        }
        row
    })
    .into_iter()
    .flatten()
    .collect()
}

/// [`Clipped`] rank filter of radius `r` over R, G and B (P-27). On an image with any
/// non-opaque pixel the histograms are weighted by alpha, and alpha itself is filtered with the
/// opposite selector (max ↔ min).
pub(super) fn clipped_rank_rgb(img: &Img, r: usize, sel: Clipped) -> Img {
    if r == 0 || img.n() == 0 {
        return img.clone();
    }
    let (w, h) = (img.w, img.h);
    let opaque = img.opaque();
    let alpha = img.plane(3);
    let weight = (!opaque).then_some(alpha.as_slice());
    let mut out = img.clone();
    let ch: Vec<Vec<Option<u8>>> = par_map(3, |c| clipped_plane(&img.plane(c), weight, w, h, r, sel));
    for (c, plane) in ch.iter().enumerate() {
        for (i, v) in plane.iter().enumerate() {
            if let Some(v) = v {
                out.d[i * 4 + c] = *v;
            }
        }
    }
    if !opaque {
        let asel = match sel {
            Clipped::Max => Clipped::Min,
            Clipped::Min => Clipped::Max,
            p => p,
        };
        for (i, v) in clipped_plane(&alpha, None, w, h, r, asel).iter().enumerate() {
            if let Some(v) = v {
                out.d[i * 4 + 3] = *v;
            }
        }
    }
    out
}

// ---- four-neighbour filters --------------------------------------------------------------------

/// The four neighbours (left, right, top, bottom) of `(x, y)`; edges give the pixel itself.
#[inline]
fn neighbours(p: &[u8], w: usize, h: usize, x: usize, y: usize) -> [f64; 4] {
    let i = y * w + x;
    let c = p[i];
    let l = if x == 0 { c } else { p[i - 1] };
    let r = if x + 1 >= w { c } else { p[i + 1] };
    let t = if y == 0 { c } else { p[i - w] };
    let below = if y + 1 >= h || (defects() && (y + 1) * w >= h) { c } else { p[i + w] };
    [l as f64, r as f64, t as f64, below as f64]
}

/// Four-neighbour sharpen of a plane with amount `a` (P-14).
pub(super) fn sharpen4(p: &[u8], w: usize, h: usize, a: f64) -> Vec<u8> {
    let mut out = vec![0u8; w * h];
    par_rows(&mut out, w, 1, |y, row| {
        for x in 0..w {
            let c = p[y * w + x] as f64;
            let [l, r, t, b] = neighbours(p, w, h, x, y);
            let v = c + (4.0 * c * a + 0.5) - ((l + r + b + t) * a + 0.5);
            row[x] = clamp255(v);
        }
    });
    out
}

/// [`sharpen4`] of R, G and B; alpha kept.
pub(super) fn sharpen4_rgb(img: &Img, a: f64) -> Img {
    img.map_rgb(|p| sharpen4(p, img.w, img.h, a))
}

/// Four-neighbour smoothing with centre weight `c` (P-15): `(c·C + L + R + T + B)/(c + 4)`.
pub(super) fn smooth4(p: &[u8], w: usize, h: usize, c: f64) -> Vec<u8> {
    let mut out = vec![0u8; w * h];
    par_rows(&mut out, w, 1, |y, row| {
        for x in 0..w {
            let ctr = p[y * w + x] as f64;
            let [l, r, t, b] = neighbours(p, w, h, x, y);
            row[x] = wrap((c * ctr + l + r + b + t) / (c + 4.0));
        }
    });
    out
}

/// [`smooth4`] of R, G and B; alpha kept.
pub(super) fn smooth4_rgb(img: &Img, c: f64) -> Img {
    img.map_rgb(|p| smooth4(p, img.w, img.h, c))
}

// ---- directional relief ------------------------------------------------------------------------

/// Directional relief (P-16): `C + w·(three neighbours on the lit side) − w·(three opposite)`,
/// neighbours from `p`, centre from `q`, kernel `k` in 1..=8 (1 = lit from the top, then
/// clockwise in 45° steps: 2 top-right, 3 right … 8 top-left).
pub(super) fn relief(p: &[u8], q: &[u8], w: usize, h: usize, k: usize, wt: f64) -> Vec<u8> {
    // Offsets (dx, dy) of NW N NE E SE S SW W, clockwise from the top-left.
    const RING: [(i64, i64); 8] = [(-1, -1), (0, -1), (1, -1), (1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0)];
    let k = k.clamp(1, 8);
    // Kernel 1 adds NW N NE (ring 0..2) and subtracts SE S SW (ring 4..6); each next kernel
    // turns both sets one step clockwise.
    let plus = [(k - 1) % 8, k % 8, (k + 1) % 8];
    let minus = [(k + 3) % 8, (k + 4) % 8, (k + 5) % 8];
    let quirk = defects();
    let mut out = vec![0u8; w * h];
    par_rows(&mut out, w, 1, |y, row| {
        if quirk && y + 1 >= h {
            return; // the spec reads past the plane: its last row is 0
        }
        let at = |x: usize, dx: i64, dy: i64| -> f64 {
            if quirk {
                // Left/up clamp; right and down are linear (x + 1 = w is the next row's first).
                let yy = if dy < 0 { y.saturating_sub(1) } else { y + dy as usize };
                let xx = if dx < 0 { x.saturating_sub(1) as i64 } else { x as i64 + dx };
                let i = (yy * w) as i64 + xx;
                p.get(i as usize).map_or(f64::NAN, |&v| v as f64)
            } else {
                let xx = clampi(x as i64 + dx, w);
                let yy = clampi(y as i64 + dy, h);
                p[yy * w + xx] as f64
            }
        };
        for x in 0..w {
            let c = q[y * w + x] as f64;
            let sp: f64 = plus.iter().map(|&j| at(x, RING[j].0, RING[j].1)).sum();
            let sm: f64 = minus.iter().map(|&j| at(x, RING[j].0, RING[j].1)).sum();
            row[x] = clamp255(c + sp * wt - sm * wt);
        }
    });
    out
}

// ---- plane arithmetic (P-17) -------------------------------------------------------------------

/// `b = max(a − b, 0)`.
pub(super) fn subtract(a: &[u8], b: &mut [u8]) {
    b.iter_mut().zip(a).for_each(|(b, a)| *b = a.saturating_sub(*b));
}
/// `b = min(a + b, 255)`.
pub(super) fn add(a: &[u8], b: &mut [u8]) {
    b.iter_mut().zip(a).for_each(|(b, a)| *b = a.saturating_add(*b));
}
/// `b = (a + b) >> 1`.
pub(super) fn average(a: &[u8], b: &mut [u8]) {
    b.iter_mut().zip(a).for_each(|(b, a)| *b = ((*a as u16 + *b as u16) >> 1) as u8);
}
/// `v = min(255, trunc(0.5 + v·k))`.
pub(super) fn scale(p: &mut [u8], k: f64) {
    p.iter_mut().for_each(|v| *v = wrap((0.5 + *v as f64 * k).trunc().min(255.0)));
}
/// `v = 255 − min(255, trunc(0.5 + (255 − v)·k))`.
pub(super) fn inverse_scale(p: &mut [u8], k: f64) {
    p.iter_mut().for_each(|v| *v = wrap(255.0 - (0.5 + (255.0 - *v as f64) * k).trunc().min(255.0)));
}
/// `v = 255 − v`.
pub(super) fn invert(p: &mut [u8]) {
    p.iter_mut().for_each(|v| *v = 255 - *v);
}
/// `v = hi if v ≥ t else lo`.
pub(super) fn threshold(p: &mut [u8], t: f64, lo: u8, hi: u8) {
    p.iter_mut().for_each(|v| *v = if *v as f64 >= t { hi } else { lo });
}
/// `v = lut[v]`.
pub(super) fn apply_lut(p: &mut [u8], lut: &[u8; 256]) {
    p.iter_mut().for_each(|v| *v = lut[*v as usize]);
}
/// Screen blend of `s` over `b` at opacity `a`, into `b`.
pub(super) fn screen(s: &[u8], b: &mut [u8], a: f64) {
    let o = (255.0 * a).trunc();
    for (b, s) in b.iter_mut().zip(s) {
        let (s, bv) = (*s as f64, *b as f64);
        let k = o * (255.0 - s) / 255.0;
        *b = wrap(((255.0 - k) * bv + s * k) / 255.0);
    }
}
/// Multiply blend of `s` over `b` at opacity `a`, into `b`.
pub(super) fn multiply(s: &[u8], b: &mut [u8], a: f64) {
    let o = (255.0 * a).trunc();
    for (b, s) in b.iter_mut().zip(s) {
        let (s, bv) = (*s as f64, *b as f64);
        let k = o * s / 255.0;
        *b = wrap(((255.0 - k) * bv + s * k) / 255.0);
    }
}
/// RGB blend of `s` over `b` through mask `m`, into `out` (P-17 "RGBA blend by mask"): where
/// `m ≠ 0` each channel is `trunc((m·s + (255 − m)·b)/255)`; elsewhere `out`'s colour is kept.
/// `out`'s alpha becomes `s`'s.
pub(super) fn blend_mask_rgb(s: &Img, m: &[u8], b: &Img, out: &mut Img) {
    for i in 0..out.n() {
        let mm = m[i] as f64;
        if mm != 0.0 {
            for c in 0..3 {
                out.d[i * 4 + c] = wrap((mm * s.d[i * 4 + c] as f64 + (255.0 - mm) * b.d[i * 4 + c] as f64) / 255.0);
            }
        }
        out.d[i * 4 + 3] = s.d[i * 4 + 3];
    }
}

// ---- histograms and levels (P-18, P-19) --------------------------------------------------------

/// 256-bin histogram of a plane.
pub(super) fn histogram(p: &[u8]) -> [f64; 256] {
    let mut h = [0f64; 256];
    p.iter().for_each(|v| h[*v as usize] += 1.0);
    h
}

/// Histogram of `p` where `mask` (0–255) weights each pixel; returns (count, bins).
pub(super) fn histogram_masked(p: &[u8], mask: &[u8]) -> (f64, [f64; 256]) {
    let mut bins = [0f64; 256];
    let mut acc = [0f32; 256];
    let (mut count, mut frac) = (0f64, 0f64);
    for (v, m) in p.iter().zip(mask) {
        match *m {
            0 => {}
            255 => {
                bins[*v as usize] += 1.0;
                count += 1.0;
            }
            m => {
                let wgt = m as f64 / 255.0;
                frac += wgt;
                acc[*v as usize] += wgt as f32;
                if frac >= 1.0 {
                    frac -= 1.0;
                    count += 1.0;
                }
                if acc[*v as usize] >= 1.0 {
                    acc[*v as usize] -= 1.0;
                    bins[*v as usize] += 1.0;
                }
            }
        }
    }
    (count, bins)
}

/// The first value whose cumulative count exceeds `p` percent of `n` (256 if none does).
pub(super) fn percentile(bins: &[f64; 256], n: f64, p: f64) -> usize {
    let t = n * p / 100.0;
    let mut cum = 0.0;
    for (i, c) in bins.iter().enumerate() {
        cum += c;
        if cum > t {
            return i;
        }
    }
    256
}

/// Auto-levels table clipping `black` percent of `n` pixels at the dark end and `white`
/// percent at the light end.
pub(super) fn levels_lut(bins: &[f64; 256], n: f64, black: f64, white: f64) -> [u8; 256] {
    let u = n / 100.0;
    let (tb, tw) = (u * black, u * white);
    let mut lo: i64 = -1;
    let mut cum = 0.0;
    loop {
        lo += 1;
        if lo > 255 {
            break;
        }
        cum += bins[lo as usize];
        if cum > tb {
            break;
        }
    }
    let mut hi: i64 = 256;
    cum = 0.0;
    loop {
        hi -= 1;
        if hi < 0 {
            break;
        }
        cum += bins[hi as usize];
        if cum > tw {
            break;
        }
    }
    if hi - lo <= 0 {
        hi = lo + 1;
    }
    if hi > 255 {
        hi = 255;
        lo = 254;
    }
    let s = 255.0 / (hi - lo) as f64;
    let mut lut = [0u8; 256];
    for (i, v) in lut.iter_mut().enumerate() {
        *v = clamp255(s * (i as f64 - lo as f64) + 0.5);
    }
    lut
}

/// Auto levels of a plane in place, optionally weighted by `mask`.
pub(super) fn auto_levels(p: &mut [u8], black: f64, white: f64, mask: Option<&[u8]>) {
    let (n, bins) = match mask {
        Some(m) => histogram_masked(p, m),
        None => (p.len() as f64, histogram(p)),
    };
    let lut = levels_lut(&bins, n, black, white);
    apply_lut(p, &lut);
}

/// Brightness/contrast table from two slider values (P-19).
pub(super) fn bc_lut(b: f64, c: f64) -> [u8; 256] {
    let b2 = if b > 5.0 { (100.0 - b.max(4.0)) / 100.0 } else { 0.95 };
    let c2 = if c != 100.0 { c.max(4.0) / 100.0 } else { 0.9995 };
    let slope = if c2 > 0.5 { 0.5 / (1.0 - c2) } else { 2.0 * c2 };
    let q = (slope * 256.0).trunc() as i64 as i32;
    let mut acc = (((0.5 - b2 * slope) * 256.0).trunc() as i64 as i32).wrapping_shl(8);
    let mut lut = [0u8; 256];
    for v in lut.iter_mut() {
        *v = clamp255((acc >> 8) as f64);
        acc = acc.wrapping_add(q);
    }
    lut
}

// ---- gray output -------------------------------------------------------------------------------

/// Colours a gray plane: 0 → `fg`, 255 → `bg` (P-21). Alpha comes from `like`.
pub(super) fn colourise(g: &[u8], fg: [u8; 3], bg: [u8; 3], like: &Img) -> Img {
    colourise_direct(&gray_out(g), fg, bg, like)
}

/// The gray plane as the spec's gray writer leaves it (P-20): its first pixel nudged by
/// one when reproducing [`defects`].
fn gray_out(g: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    if !defects() || g.is_empty() {
        return std::borrow::Cow::Borrowed(g);
    }
    let mut v = g.to_vec();
    v[0] = if v[0] == 255 { 254 } else { v[0] + 1 };
    std::borrow::Cow::Owned(v)
}

/// [`colourise`] without the gray writer's first-pixel nudge (filters that colourise their own
/// plane directly: Photocopy, Bas Relief, Halftone Pattern).
/// The mix is rounded; the spec truncated it, which turns some grays one level darker
/// even between black and white (41 → 40).
pub(super) fn colourise_direct(g: &[u8], fg: [u8; 3], bg: [u8; 3], like: &Img) -> Img {
    let mut out = like.clone();
    let exact = !defects();
    for (i, px) in out.d.chunks_exact_mut(4).enumerate() {
        let gv = g[i] as f64;
        let t = gv * (1.0 / 255.0);
        for c in 0..3 {
            px[c] = if exact {
                ((fg[c] as f64 * (255.0 - gv) + bg[c] as f64 * gv) / 255.0).round() as u8
            } else {
                wrap(fg[c] as f64 * (1.0 - t) + bg[c] as f64 * t)
            };
        }
    }
    out
}

// ---- gradients ---------------------------------------------------------------------------------

/// Gradient angle pairs (P-22): per pixel (vertical, horizontal) as integers 0..255.
pub(super) fn grad_pairs(p: &[u8], w: usize, h: usize) -> Vec<i32> {
    let k = (1.0 / std::f64::consts::PI) * 255.0;
    let mut out = vec![0i32; w * h * 2];
    par_rows(&mut out, w, 2, |y, row| {
        let yn = (y + 1).min(h - 1);
        for x in 0..w {
            let xn = (x + 1).min(w - 1);
            let c = p[y * w + x] as f64;
            let ang = |d: f64, zero: f64| -> i32 {
                if d == 0.0 {
                    return zero.trunc() as i32;
                }
                let a = ((1.0 / -d).atan2(1.0) * k + 0.5).trunc() as i32;
                if a < 0 { a + 255 } else { a }
            };
            row[x * 2] = ang(c - p[yn * w + x] as f64, (1.0 - (y + 1) as f64 / h as f64) * 255.0);
            row[x * 2 + 1] = ang(c - p[y * w + xn] as f64, (1.0 - (x + 1) as f64 / w as f64) * 255.0);
        }
    });
    out
}

// ---- convolutions ------------------------------------------------------------------------------

/// RGBA 3×3 convolution with clamped edges (P-26), un-normalised kernel; `trunc(0.5 + Σ)` with a
/// saturating store; alpha written only when `alpha`.
pub(super) fn conv3_rgba(img: &Img, k: &[f64; 9], alpha: bool, abs: bool) -> Img {
    let (w, h) = (img.w, img.h);
    let mut out = img.clone();
    par_rows(&mut out.d, w, 4, |y, row| {
        for x in 0..w {
            let mut acc = [0f64; 4];
            for j in 0..3 {
                for i in 0..3 {
                    let kv = k[j * 3 + i];
                    if kv == 0.0 {
                        continue;
                    }
                    let xx = clampi(x as i64 + i as i64 - 1, w);
                    let yy = clampi(y as i64 + j as i64 - 1, h);
                    for c in 0..4 {
                        acc[c] += kv * img.d[(yy * w + xx) * 4 + c] as f64;
                    }
                }
            }
            for c in 0..if alpha { 4 } else { 3 } {
                let v = if abs { acc[c].abs() } else { acc[c] };
                row[x * 4 + c] = sat((0.5 + v).trunc());
            }
        }
    });
    out
}

/// Sobel edges of R, G and B (P-26): `255 − |∇|` per channel; alpha kept.
pub(super) fn sobel_rgb(img: &Img) -> Img {
    let hx = conv3_rgba(img, &[-1.0, 0.0, 1.0, -2.0, 0.0, 2.0, -1.0, 0.0, 1.0], false, true);
    let vy = conv3_rgba(img, &[1.0, 2.0, 1.0, 0.0, 0.0, 0.0, -1.0, -2.0, -1.0], false, true);
    let mut out = img.clone();
    for i in 0..img.n() {
        for c in 0..3 {
            let (a, b) = (hx.d[i * 4 + c] as f64, vy.d[i * 4 + c] as f64);
            out.d[i * 4 + c] = wrap(255.0 - (a * a + b * b).sqrt().clamp(0.0, 255.0));
        }
    }
    out
}

// ---- Gaussian by boxes (P-28) ------------------------------------------------------------------

/// Box radii for `n` passes approximating a Gaussian of `sigma`.
pub(super) fn gauss_radii(sigma: f64, n: usize) -> Vec<usize> {
    let nf = n as f64;
    let ideal = (12.0 * sigma * sigma / nf + 1.0).sqrt();
    let mut ws = ideal.floor() as i64;
    if ws % 2 == 0 {
        ws -= 1;
    }
    let wl = ws + 2;
    let wsf = ws as f64;
    let m = rhu((12.0 * sigma * sigma - nf * wsf * wsf - 4.0 * nf * wsf - 3.0 * nf) / (-4.0 * wsf - 4.0));
    (0..n).map(|i| (((if (i as f64) < m { ws } else { wl }) - 1) / 2).max(0) as usize).collect()
}

/// One running-sum pass along rows (horizontal) with clamped edges; `f` turns the window sum
/// into the stored value.
fn box_pass_h<T: Copy + Send + Sync>(p: &[T], w: usize, r: usize, val: impl Fn(T) -> f64 + Sync, f: impl Fn(f64) -> T + Sync) -> Vec<T> {
    let mut out = p.to_vec();
    if r == 0 || w == 0 {
        return out;
    }
    let ri = r as i64;
    par_rows(&mut out, w, 1, |y, row| {
        let src = &p[y * w..(y + 1) * w];
        let mut s: f64 = (-ri..=ri).map(|k| val(src[clampi(k, w)])).sum();
        row[0] = f(s);
        for x in 1..w {
            s += val(src[clampi(x as i64 + ri, w)]) - val(src[clampi(x as i64 - ri - 1, w)]);
            row[x] = f(s);
        }
    });
    out
}

fn transpose<T: Copy + Default + Send + Sync>(p: &[T], w: usize, h: usize) -> Vec<T> {
    let mut out = vec![T::default(); w * h];
    par_rows(&mut out, h, 1, |x, row| {
        for (y, v) in row.iter_mut().enumerate() {
            *v = p[y * w + x];
        }
    });
    out
}

/// One byte box pass of radius `r` (horizontal then vertical), `round½↑(sum/(2r + 1))`.
pub(super) fn box_pass_u8(p: &[u8], w: usize, h: usize, r: usize) -> Vec<u8> {
    if r == 0 {
        return p.to_vec();
    }
    let d = (2 * r + 1) as f64;
    let f = |s: f64| rhu(s / d) as u8;
    let a = box_pass_h(p, w, r, |v| v as f64, f);
    let t = box_pass_h(&transpose(&a, w, h), h, r, |v| v as f64, f);
    transpose(&t, h, w)
}

/// One float box pass of radius `r` (horizontal then vertical), `sum/(2r + 1)` stored f32.
pub(super) fn box_pass_f32(p: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    if r == 0 {
        return p.to_vec();
    }
    let k = 1.0 / (2 * r + 1) as f64;
    let f = |s: f64| (s * k) as f32;
    let a = box_pass_h(p, w, r, |v| v as f64, f);
    let t = box_pass_h(&transpose(&a, w, h), h, r, |v| v as f64, f);
    transpose(&t, h, w)
}

/// Gaussian blur of a byte plane by `n` box passes (P-28).
pub(super) fn gauss_u8(p: &[u8], w: usize, h: usize, sigma: f64, n: usize) -> Vec<u8> {
    gauss_radii(sigma, n).into_iter().fold(p.to_vec(), |acc, r| box_pass_u8(&acc, w, h, r))
}

/// Gaussian blur of an f32 plane by three box passes (P-28).
pub(super) fn gauss_f32(p: &[f32], w: usize, h: usize, sigma: f64) -> Vec<f32> {
    gauss_radii(sigma, 3).into_iter().fold(p.to_vec(), |acc, r| box_pass_f32(&acc, w, h, r))
}

// ---- displacement fields and warping (P-29, P-31) ----------------------------------------------

/// Warps `img` by the displacement field `(dx, dy)` (f32 planes): each pixel samples the source
/// bilinearly at its own position plus the displacement, alpha-weighted, clamped (P-29).
pub(super) fn warp(img: &Img, dx: &[f32], dy: &[f32]) -> Img {
    let (w, h) = (img.w, img.h);
    let off = 0.5 - 0.499999;
    let mut out = img.clone();
    let field = |p: &[f32], fx: f64, fy: f64| -> f64 {
        let (x0, y0) = (fx.trunc() as usize, fy.trunc() as usize);
        let (x0, y0) = (x0.min(w - 1), y0.min(h - 1));
        let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
        let (ax, ay) = (fx - fx.trunc(), fy - fy.trunc());
        let g = |x: usize, y: usize| p[y * w + x] as f64;
        (1.0 - ay) * ((1.0 - ax) * g(x0, y0) + ax * g(x1, y0)) + ay * ((1.0 - ax) * g(x0, y1) + ax * g(x1, y1))
    };
    par_rows(&mut out.d, w, 4, |y, row| {
        for x in 0..w {
            let (fx, fy) = (x as f64 + off, y as f64 + off);
            let (ddx, ddy) = (field(dx, fx, fy), field(dy, fx, fy));
            if ddx == 0.0 && ddy == 0.0 {
                continue;
            }
            let (sx, sy) = (x as f64 + ddx + off, y as f64 + ddy + off);
            let (x0, y0) = (sx.floor(), sy.floor());
            let (ax, ay) = (sx - x0, sy - y0);
            let cx = |v: f64| v.clamp(0.0, (w - 1) as f64) as usize;
            let cy = |v: f64| v.clamp(0.0, (h - 1) as f64) as usize;
            let corners = [
                (cx(x0), cy(y0), (1.0 - ay) * (1.0 - ax)),
                (cx(x0 + 1.0), cy(y0), (1.0 - ay) * ax),
                (cx(x0), cy(y0 + 1.0), ay * (1.0 - ax)),
                (cx(x0 + 1.0), cy(y0 + 1.0), ay * ax),
            ];
            let mut acc = [0f64; 3];
            let mut alpha = 0.0;
            for (xx, yy, k) in corners {
                let i = (yy * w + xx) * 4;
                let wgt = k * img.d[i + 3] as f64;
                alpha += wgt;
                for c in 0..3 {
                    acc[c] += wgt * img.d[i + c] as f64;
                }
            }
            let o = &mut row[x * 4..x * 4 + 4];
            if alpha == 0.0 {
                o.copy_from_slice(&[0, 0, 0, 0]);
            } else {
                for c in 0..3 {
                    o[c] = wrap(acc[c] / alpha + 0.5);
                }
                o[3] = wrap(alpha + 0.5);
            }
        }
    });
    out
}

/// 13-tap Gaussian of an f32 plane, not normalised (P-31), horizontal then vertical, clamped.
pub(super) fn field_gauss13(p: &[f32], w: usize, h: usize, sigma: f64) -> Vec<f32> {
    if sigma <= 0.0 {
        return p.to_vec();
    }
    let wts: Vec<f64> = (-6..=6).map(|o: i32| 1.0 / (sigma * (2.0 * std::f64::consts::PI).sqrt()) * (-0.5 * (o as f64 / sigma).powi(2)).exp()).collect();
    let pass = |src: &[f32], w: usize, h: usize| -> Vec<f32> {
        let mut out = vec![0f32; w * h];
        par_rows(&mut out, w, 1, |y, row| {
            for x in 0..w {
                let mut s = 0.0;
                for (k, wt) in wts.iter().enumerate() {
                    s += wt * src[y * w + clampi(x as i64 + k as i64 - 6, w)] as f64;
                }
                row[x] = s as f32;
            }
        });
        out
    };
    let a = pass(p, w, h);
    transpose(&pass(&transpose(&a, w, h), h, w), h, w)
}

/// Field smoothing of an f32 plane by `amount` (P-31).
pub(super) fn field_smooth(p: &[f32], w: usize, h: usize, amount: f64) -> Vec<f32> {
    if amount > 1.0 {
        return gauss_f32(p, w, h, 0.42 * amount);
    }
    let c = if rhu(amount) >= 1.0 { 16.0 } else { 1.0 };
    let k = [1.0, 2.0, 1.0, 2.0, c, 2.0, 1.0, 2.0, 1.0];
    let s: f64 = k.iter().sum();
    let mut out = p.to_vec();
    if w < 3 || h < 3 {
        return out;
    }
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let mut acc = 0.0;
            for j in 0..3 {
                for i in 0..3 {
                    acc += k[j * 3 + i] / s * p[(y + j - 1) * w + x + i - 1] as f64;
                }
            }
            out[y * w + x] = acc as f32;
        }
    }
    out
}

// ---- random numbers (R-1, R-2, R-3) ------------------------------------------------------------

#[inline]
fn mulberry_mix(t: u32) -> f64 {
    let a = (t ^ (t >> 15)).wrapping_mul(t | 1);
    let a = a ^ a.wrapping_add((a ^ (a >> 7)).wrapping_mul(a | 61));
    (a ^ (a >> 14)) as f64 / 4_294_967_296.0
}

/// The gallery's random generator (R-1): Mulberry32 whose state is kept as a double, so it
/// drifts from the 32-bit generator once the state passes 2^53, exactly as the spec does.
pub(super) struct Rng {
    s: f64,
}

impl Rng {
    pub fn new(seed: u32) -> Rng {
        Rng { s: seed as f64 }
    }
    /// A value in [0, 1).
    #[inline]
    pub fn next(&mut self) -> f64 {
        self.s += 1_831_565_813.0;
        mulberry_mix(self.s as u64 as u32)
    }
    /// A plane of random bytes `trunc(r·256)`, row-major.
    pub fn fill(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| (self.next() * 256.0) as u8).collect()
    }
}

/// Canonical 32-bit Mulberry32 (R-3), for the filters that used unseeded randomness.
pub(super) struct Mulberry32 {
    s: u32,
}

impl Mulberry32 {
    pub fn new(seed: u32) -> Mulberry32 {
        Mulberry32 { s: seed }
    }
    #[inline]
    pub fn next(&mut self) -> f64 {
        self.s = self.s.wrapping_add(0x6D2B_79F5);
        mulberry_mix(self.s)
    }
}

/// The hash generator of Spatter and Sprayed Strokes (R-2).
pub(super) struct HashRng {
    s: i64,
}

impl HashRng {
    pub fn new(seed: u32) -> HashRng {
        HashRng { s: seed as i64 }
    }
    /// A value in [0, 1].
    #[inline]
    pub fn next(&mut self) -> f64 {
        let x = self.s as u32;
        let a = (x ^ 61 ^ (x >> 16)) as i32;
        let b = a as i64 + (a as u32).wrapping_shl(3) as i32 as i64;
        let bl = b as u32;
        let c = (bl ^ (bl >> 4)) as i32;
        let d = (c as f64 * 668_265_261.0) as i64 as u32;
        let hsh = (d ^ (d >> 15)) as i32;
        self.s = hsh as i64;
        (hsh & 0x00FF_FFFF) as f64 / 16_777_215.0
    }
}

// ---- textures (T-1 … T-3, P-24, P-30) ----------------------------------------------------------

/// The gallery's textures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Texture {
    Blocks,
    Canvas,
    Frosted,
    TinyLens,
    Brick,
    Burlap,
    Sandstone,
}

impl Texture {
    /// From a choice name (`canvas`, `tinyLens` …); canvas when unknown.
    pub fn from_name(n: &str) -> Texture {
        match n {
            "blocks" => Texture::Blocks,
            "frosted" => Texture::Frosted,
            "tinyLens" => Texture::TinyLens,
            "brick" => Texture::Brick,
            "burlap" => Texture::Burlap,
            "sandstone" => Texture::Sandstone,
            _ => Texture::Canvas,
        }
    }
    /// Position in the global list (blocks, canvas, frosted, tinyLens, brick, burlap, sandstone).
    pub fn index(self) -> usize {
        self as usize
    }
    /// The 128 × 128 gray height map.
    pub fn plane(self) -> &'static [u8] {
        match self {
            Texture::Blocks => include_bytes!("../../../../resources/textures/gallery/blocks.r8"),
            Texture::Canvas => include_bytes!("../../../../resources/textures/gallery/canvas.r8"),
            Texture::TinyLens => include_bytes!("../../../../resources/textures/gallery/tinylens.r8"),
            Texture::Brick => include_bytes!("../../../../resources/textures/gallery/brick.r8"),
            Texture::Burlap => include_bytes!("../../../../resources/textures/gallery/burlap.r8"),
            Texture::Sandstone => include_bytes!("../../../../resources/textures/gallery/sandstone.r8"),
            Texture::Frosted => frosted(),
        }
    }
}

/// Side of every texture.
pub(super) const TEX: usize = 128;

/// The procedural frosted texture (T-3): mid gray plus Gaussian monochrome noise of amount 50 %,
/// from a fixed seed.
fn frosted() -> &'static [u8] {
    static F: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    F.get_or_init(|| {
        let mut r = Mulberry32::new(777);
        (0..TEX * TEX)
            .map(|_| {
                let n = (r.next() + r.next() + r.next() + r.next() - 2.0) * 2.0;
                sat(128.0 + 127.5 * n)
            })
            .collect()
    })
}

/// Bilinear sample of a `w × h` plane at `(u, v)`, wrapping (P-30).
pub(super) fn sample_wrap(p: &[u8], w: usize, h: usize, u: f64, v: f64) -> f64 {
    let (px, py) = (u - 0.499999, v - 0.499999);
    let (x0, y0) = (px.floor(), py.floor());
    let (fx, fy) = (px - x0, py - y0);
    let wi = |x: f64, n: usize| (x as i64).rem_euclid(n as i64) as usize;
    let (xa, xb, ya, yb) = (wi(x0, w), wi(x0 + 1.0, w), wi(y0, h), wi(y0 + 1.0, h));
    let g = |x: usize, y: usize| p[y * w + x] as f64;
    (1.0 - fy) * (1.0 - fx) * g(xa, ya) + (1.0 - fy) * fx * g(xb, ya) + fy * (1.0 - fx) * g(xa, yb) + fy * fx * g(xb, yb)
}

/// The texture tiled over a `w × h` image at `scale` percent (P-24), optionally inverted.
pub(super) fn pattern(t: Texture, scale: f64, invert: bool, w: usize, h: usize) -> Vec<u8> {
    let k = 100.0 / scale.max(1.0);
    let tex = t.plane();
    let mut out = vec![0u8; w * h];
    par_rows(&mut out, w, 1, |y, row| {
        for x in 0..w {
            let v = sample_wrap(tex, TEX, TEX, x as f64 * k, y as f64 * k);
            row[x] = wrap(if invert { 255.0 - v } else { v });
        }
    });
    out
}

/// The relief factor of a 0–50 slider (`φ(r)`): `r/40` up to 40, then the table.
pub(super) fn relief_factor(r: f64) -> f64 {
    if r <= 40.0 {
        r / 40.0
    } else {
        let i = (r - 41.0).round().clamp(0.0, 9.0) as usize;
        super::tables::RELIEF_TAIL[i]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(w: usize, h: usize) -> Vec<u8> {
        (0..w * h).map(|i| ((i * 37 + i / w * 11) % 256) as u8).collect()
    }

    /// Plain O(window) references for the sliding-window kernels.
    fn naive_box(src: &[u8], w: usize, h: usize, kw: usize, kh: usize) -> Vec<u8> {
        let (hw, hh) = ((kw >> 1) as i64, (kh >> 1) as i64);
        let mut out = vec![0; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut s = 0u64;
                for j in 0..kh as i64 {
                    for i in 0..kw as i64 {
                        s += src[clampi(y as i64 - hh + j, h) * w + clampi(x as i64 - hw + i, w)] as u64;
                    }
                }
                out[y * w + x] = (s / (kw * kh) as u64) as u8;
            }
        }
        out
    }

    #[test]
    fn box_and_rank_match_naive_windows() {
        let (w, h) = (23, 17);
        let src = ramp(w, h);
        for (kw, kh) in [(1, 1), (2, 3), (5, 5), (8, 2), (30, 30)] {
            assert_eq!(box_mean(&src, w, h, kw, kh), naive_box(&src, w, h, kw, kh), "{kw}x{kh}");
            let rank = (kw * kh) as f64 / 2.0;
            let got = rank_rect(&src, w, h, kw, kh, rank);
            let (hw, hh) = ((kw >> 1) as i64, (kh >> 1) as i64);
            for y in 0..h {
                for x in 0..w {
                    let mut v: Vec<u8> = (0..kh as i64)
                        .flat_map(|j| (0..kw as i64).map(move |i| (j, i)))
                        .map(|(j, i)| src[clampi(y as i64 - hh + j, h) * w + clampi(x as i64 - hw + i, w)])
                        .collect();
                    v.sort();
                    assert_eq!(got[y * w + x], v[rank.ceil() as usize - 1], "{kw}x{kh} at {x},{y}");
                }
            }
        }
        assert!(box_mean(&src, w, h, 0, 3).iter().all(|&v| v == 0));
        assert!(rank_rect(&src, w, h, 3, 3, 0.0).iter().all(|&v| v == 0));
        assert!(rank_dir(&src, w, h, 3, 2, false, 7.0).iter().all(|&v| v == 0), "rank above the window");
    }

    #[test]
    fn sheared_box_is_the_mean_of_the_sheared_window() {
        let (w, h) = (19, 13);
        let src = ramp(w, h);
        let mut img = Img::new(w, h);
        img.set_plane(0, &src);
        let (len, span) = (5usize, 3usize);
        for reverse in [false, true] {
            let got = box_dir_rgb(&img, len, span, reverse).plane(0);
            let max = rank_dir(&src, w, h, len, span, reverse, (len * span) as f64);
            for y in 0..h {
                for x in 0..w {
                    let diag = (len + span - 1) as i64;
                    let mut s = 0u64;
                    let mut m = 0u8;
                    for st in 0..len as i64 {
                        let a = if reverse { len as i64 - 1 - st } else { st };
                        let yy = clampi(y as i64 - (len as i64 >> 1) + st, h);
                        for o in 0..span as i64 {
                            let v = src[yy * w + clampi(x as i64 - (diag >> 1) + a + o, w)];
                            s += v as u64;
                            m = m.max(v);
                        }
                    }
                    assert_eq!(got[y * w + x], (s / (len * span) as u64) as u8);
                    assert_eq!(max[y * w + x], m);
                }
            }
        }
    }

    #[test]
    fn hsv_round_trips_grays_and_primaries() {
        let mut img = Img::new(4, 1);
        img.d = vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 90, 90, 90, 255];
        let src = img.clone();
        to_hsv(&mut img);
        assert_eq!(&img.d[..12], &[0, 255, 255, 255, 86, 255, 255, 255, 172, 255, 255, 255]);
        assert_eq!(&img.d[12..15], &[0, 0, 90]);
        from_hsv(&mut img);
        // Red and grays come back exactly; the 43-step hue is lossy for green and blue.
        assert_eq!(&img.d[..4], &src.d[..4]);
        assert_eq!(&img.d[4..7], &[0, 255, 6]);
        assert_eq!(&img.d[12..], &src.d[12..]);
    }

    #[test]
    fn generators_are_reproducible() {
        // Below 2^53 the double-state generator is plain 32-bit Mulberry32.
        let (mut r, mut m) = (Rng::new(42), Mulberry32::new(42));
        for _ in 0..1000 {
            let (a, b) = (r.next(), m.next());
            assert_eq!(a, b);
            assert!((0.0..1.0).contains(&a));
        }
        let mut h = HashRng::new(10_738_420);
        let v: Vec<f64> = (0..100).map(|_| h.next()).collect();
        assert!(v.iter().all(|v| (0.0..=1.0).contains(v)));
        assert!(v.windows(2).any(|p| p[0] != p[1]));
    }

    #[test]
    fn levels_stretch_to_full_range() {
        let p: Vec<u8> = (0..100).map(|i| 50 + i).collect();
        let lut = levels_lut(&histogram(&p), 100.0, 0.0, 0.0);
        assert_eq!(lut[50], 0);
        assert_eq!(lut[149], 255);
    }

    #[test]
    fn textures_are_embedded() {
        for t in [Texture::Blocks, Texture::Canvas, Texture::Frosted, Texture::TinyLens, Texture::Brick, Texture::Burlap, Texture::Sandstone] {
            let p = t.plane();
            assert_eq!(p.len(), TEX * TEX, "{t:?}");
            assert!(p.iter().any(|&v| v != p[0]), "{t:?} is flat");
        }
    }
}
