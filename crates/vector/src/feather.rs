//! Mask feathering: the Gaussian blur Properties › Feather applies to a mask. The compositor's
//! vector and pixel masks (`photosplice-compose::masks`) and a shape layer's own path (its Density
//! and Feather, the shape renderer's soft fill) share it, so a soft shape and a fill layer with an
//! equal vector mask render the same pixels. Pure `f32` planes.

use photosplice_doc::Path;
use photosplice_geom::Rect;

/// Gaussian sigma of a mask feather of `px` pixels (Properties › Feather): the feather itself.
/// Blurred with exactly that variance, psd-tools mask-parameters-no-real-channel.psd (6 px vector
/// and 3 px pixel feather) matches Reference within 1.5 levels and layer_mask_data.psd's
/// feathered edges are 27 % → 4 % off (the rest are its shapes' antialiased edges).
pub fn feather_sigma(px: f32) -> f32 {
    if px.is_finite() { (px * FEATHER_SIGMA).clamp(0.0, MAX_FEATHER_SIGMA) } else { 0.0 }
}

const FEATHER_SIGMA: f32 = 1.0;
/// Reference's feather tops out at 1000 px.
const MAX_FEATHER_SIGMA: f32 = 1000.0 * FEATHER_SIGMA;

/// `r` grown by a feather of Gaussian `sigma`: how far the blur carries a mask's edge (three
/// sigmas and two pixels, beyond which it is constant to well under a level).
pub fn grow(r: Rect, sigma: f32) -> Rect {
    let m = (sigma * 3.0).ceil() as i32 + 2;
    if r.is_empty() { r } else { Rect::new(r.x0.saturating_sub(m), r.y0.saturating_sub(m), r.x1.saturating_add(m), r.y1.saturating_add(m)) }
}

/// Where a feathered path mask can differ from its value far outside the path: the path's control
/// bounds (and two pixels of anti-aliasing) grown by the feather, as the compositor bounds a
/// feathered vector mask. Empty for a path without knots.
pub fn path_area(path: &Path, sigma: f32) -> Rect {
    match path.control_bounds() {
        Some((x0, y0, x1, y1)) => grow(
            Rect::new(
                (x0.floor() as i32).saturating_sub(2),
                (y0.floor() as i32).saturating_sub(2),
                (x1.ceil() as i32).saturating_add(2),
                (y1.ceil() as i32).saturating_add(2),
            ),
            sigma,
        ),
        None => Rect::EMPTY,
    }
}

/// One box pass: weight 1 on `±r` around each sample and `end` (0..1) on the two samples just
/// beyond, so the pass's variance is any real number, not only the `r(r+1)/3` of whole boxes.
#[derive(Clone, Copy, Debug, PartialEq)]
struct BoxPass {
    r: usize,
    end: f32,
}

impl BoxPass {
    /// The pass whose three-fold repetition has variance `sigma²` (an "extended box", Gwosdek et
    /// al. 2011), or `None` when there is nothing to blur.
    fn for_sigma(sigma: f32) -> Option<BoxPass> {
        if !sigma.is_finite() || sigma <= 0.0 {
            return None;
        }
        // Feathers top out at 1000 px; the cap keeps the radius arithmetic far from overflow.
        let v = f64::from(sigma.min(MAX_FEATHER_SIGMA)).powi(2) / 3.0;
        // The largest whole radius whose box variance r(r+1)/3 is at most v.
        let mut r = (((12.0 * v + 1.0).sqrt() - 1.0) / 2.0).floor().max(0.0) as usize;
        let var = |r: usize| (r as f64) * (r as f64 + 1.0) / 3.0;
        while r > 0 && var(r) > v {
            r -= 1;
        }
        while var(r + 1) <= v {
            r += 1;
        }
        let (rf, n) = (r as f64, 2.0 * r as f64 + 1.0);
        let end = (n * (rf * (rf + 1.0) / 3.0 - v)) / (2.0 * v - 2.0 * (rf + 1.0) * (rf + 1.0));
        Some(BoxPass { r, end: end.clamp(0.0, 1.0) as f32 })
    }
}

/// Apply one of the three box passes to a single line. `scratch` preserves the input while
/// the sliding sum writes the output, matching the previous sequential implementation exactly.
fn blur_line(line: &mut [f32], scratch: &mut [f32], pass: BoxPass) {
    if line.is_empty() {
        return;
    }
    let Ok(radius) = isize::try_from(pass.r) else { return };
    scratch.copy_from_slice(line);
    let get = |i: isize| scratch[i.clamp(0, scratch.len() as isize - 1) as usize];
    let mut acc: f32 = (-radius..=radius).map(get).sum();
    let n = (2 * pass.r + 1) as f32 + 2.0 * pass.end;
    // The sample leaving the window is the next one's low end weight.
    let mut below = get(-radius - 1);
    for (i, dst) in line.iter_mut().enumerate() {
        let i = i as isize;
        let (above, leaving) = (get(i + radius + 1), get(i - radius));
        *dst = (acc + pass.end * (below + above)) / n;
        acc += above - leaving;
        below = leaving;
    }
}

/// Samples the three passes of a line read past each of its ends: `r + 1` per pass.
fn line_pad(pass: BoxPass) -> usize {
    pass.r.saturating_add(1).saturating_mul(3)
}

/// The three box passes over one line, which continues past each end with its edge value, as
/// Reference feathers a mask the canvas cuts off: a mask white up to the edge stays white there,
/// one black there stays black. Clamping every pass at the line's own end instead repeated the
/// blurred values there, which brightened a 32 px feather by up to 12 levels where the canvas
/// cut it off (and made the result depend on the canvas size). `padded` and `scratch` are reused
/// buffers; the line is left as it is when they can't grow.
fn blur_line3(line: &mut [f32], padded: &mut Vec<f32>, scratch: &mut Vec<f32>, pass: BoxPass) {
    let (Some(&first), Some(&last)) = (line.first(), line.last()) else { return };
    let m = line_pad(pass);
    let Some(len) = m.checked_mul(2).and_then(|p| p.checked_add(line.len())) else { return };
    padded.clear();
    if padded.try_reserve_exact(len).is_err() {
        return;
    }
    padded.extend(std::iter::repeat_n(first, m));
    padded.extend_from_slice(line);
    padded.extend(std::iter::repeat_n(last, m));
    if scratch.len() < len {
        if scratch.try_reserve_exact(len - scratch.len()).is_err() {
            return;
        }
        scratch.resize(len, 0.0);
    }
    let Some(scratch) = scratch.get_mut(..len) else { return };
    for _ in 0..3 {
        blur_line(padded, scratch, pass);
    }
    if let Some(mid) = padded.get(m..m + line.len()) {
        line.copy_from_slice(mid);
    }
}

/// Apply the three box passes ([`blur_line3`]) to independent contiguous rows. Scoped OS threads
/// are used instead of Rayon because mask construction can happen inside a Rayon tile job; nested
/// Rayon work here can deadlock when every worker waits on the same OnceLock (#276).
fn blur_rows_chunk(v: &mut [f32], w: usize, pass: BoxPass) {
    let (mut padded, mut scratch) = (Vec::new(), Vec::new());
    for row in v.chunks_mut(w) {
        blur_line3(row, &mut padded, &mut scratch, pass);
    }
}

fn blur_rows(v: &mut [f32], w: usize, pass: BoxPass) {
    if w == 0 || v.is_empty() {
        return;
    }
    let h = v.len() / w;
    let threads = if cfg!(target_arch = "wasm32") || v.len() < 1 << 16 {
        1
    } else {
        std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 16).min(h.max(1))
    };
    if threads == 1 {
        blur_rows_chunk(v, w, pass);
        return;
    }
    let rows_per_worker = h.div_ceil(threads);
    let chunk_len = rows_per_worker.saturating_mul(w);
    std::thread::scope(|scope| {
        for chunk in v.chunks_mut(chunk_len) {
            scope.spawn(move || blur_rows_chunk(chunk, w, pass));
        }
    });
}

/// Cache-friendly blocked transpose. It makes the vertical blur contiguous without a Rayon job
/// or unsafe disjoint-column access; the temporary is released before the next mask is built.
fn transpose_into(v: &[f32], out: &mut [f32], w: usize, h: usize) {
    const BLOCK: usize = 32;
    for by in (0..h).step_by(BLOCK) {
        for bx in (0..w).step_by(BLOCK) {
            for y in by..(by + BLOCK).min(h) {
                for x in bx..(bx + BLOCK).min(w) {
                    out[x * h + y] = v[y * w + x];
                }
            }
        }
    }
}

/// Approximate Gaussian blur of a `w`×`h` plane: three box passes per axis whose variance is
/// exactly `sigma²` ([`BoxPass`]). Beyond its edges the plane continues with its edge values
/// ([`blur_line3`]), as Reference feathers a mask at the canvas edge. Whole-pixel boxes rounded
/// the variance (a 6 px feather blurred like 6.48 px, a 3 px one like 3.46) and missed
/// Reference's feathered edges by up to 4 levels
/// (psd-tools mask-parameters-no-real-channel.psd); the exact variance is within 1.5. The axes
/// are processed in parallel on bounded OS threads, preserving each line's floating-point
/// operation order.
pub fn gaussian(v: &mut [f32], w: usize, h: usize, sigma: f32) {
    if w == 0 || h == 0 || v.len() != w.saturating_mul(h) {
        return;
    }
    let Some(pass) = BoxPass::for_sigma(sigma) else { return };
    // Reserve before modifying the input so allocation failure can use the sequential fallback
    // without leaving a half-blurred mask.
    let mut transposed = Vec::new();
    if transposed.try_reserve_exact(v.len()).is_err() {
        gaussian_sequential(v, w, h, pass);
        return;
    }
    transposed.resize(v.len(), 0.0);
    blur_rows(v, w, pass);
    transpose_into(v, &mut transposed, w, h);
    blur_rows(&mut transposed, h, pass);
    transpose_into(&transposed, v, h, w);
}

/// Allocation-light reference path used if parallel transpose storage cannot be reserved.
fn gaussian_sequential(v: &mut [f32], w: usize, h: usize, pass: BoxPass) {
    let mut line = Vec::new();
    let (mut padded, mut scratch) = (Vec::new(), Vec::new());
    let len = w.max(h);
    if line.try_reserve_exact(len).is_err() {
        return;
    }
    line.resize(len, 0.0);
    for axis in [false, true] {
        let (lines, line_len) = if axis { (w, h) } else { (h, w) };
        for k in 0..lines {
            for i in 0..line_len {
                line[i] = if axis { v[i * w + k] } else { v[k * w + i] };
            }
            blur_line3(&mut line[..line_len], &mut padded, &mut scratch, pass);
            for i in 0..line_len {
                if axis {
                    v[i * w + k] = line[i];
                } else {
                    v[k * w + i] = line[i];
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigma_is_the_feather_clamped_and_finite() {
        assert_eq!(feather_sigma(6.5), 6.5);
        assert_eq!(feather_sigma(-3.0), 0.0);
        assert_eq!(feather_sigma(f32::NAN), 0.0);
        assert_eq!(feather_sigma(f32::INFINITY), 0.0);
        assert_eq!(feather_sigma(1.0e9), 1000.0);
    }

    #[test]
    fn a_blur_keeps_a_constant_plane_and_a_step_symmetric() {
        let mut flat = vec![0.75f32; 40 * 30];
        gaussian(&mut flat, 40, 30, 5.0);
        assert!(flat.iter().all(|v| (v - 0.75).abs() < 1e-5));
        // A vertical step at x = 20: half way between pixels 19 and 20, symmetric around it.
        let mut step: Vec<f32> = (0..40 * 4).map(|i| if i % 40 < 20 { 1.0 } else { 0.0 }).collect();
        gaussian(&mut step, 40, 4, 4.0);
        assert!((step[19] + step[20] - 1.0).abs() < 1e-4, "{} {}", step[19], step[20]);
        assert!((step[10] + step[29] - 1.0).abs() < 1e-4);
        // Mismatched sizes and nothing to blur leave the plane as it is.
        let mut v = vec![0.5f32; 7];
        gaussian(&mut v, 4, 4, 3.0);
        gaussian(&mut v, 7, 1, 0.0);
        assert!(v.iter().all(|x| *x == 0.5));
    }

    #[test]
    fn the_area_grows_by_three_sigmas() {
        let p = Path::new(vec![photosplice_doc::Subpath::polygon(&[(10.0, 10.0), (20.0, 10.0), (20.0, 20.0)])]);
        assert_eq!(path_area(&p, 0.0), Rect::new(6, 6, 24, 24));
        assert_eq!(path_area(&p, 10.0), Rect::new(-24, -24, 54, 54));
        assert!(path_area(&Path::default(), 5.0).is_empty());
        assert_eq!(grow(Rect::new(i32::MAX - 1, 0, i32::MAX, 1), 1000.0).x1, i32::MAX);
    }

    /// Three passes have the variance sigma² exactly, for whole and fractional sigmas alike (whole
    /// boxes rounded a 6 px feather to 6.48 and a 3 px one to 3.46).
    #[test]
    fn feather_blur_has_the_exact_variance() {
        for sigma in [0.3f32, 1.0, 2.6, 3.0, 6.0, 13.7, 40.0] {
            let (w, c) = (801usize, 400usize);
            let mut v = vec![0.0f32; w];
            v[c] = 1.0;
            gaussian(&mut v, w, 1, sigma);
            // Within the kernel's reach (the running sums leave ~1e-8 behind further out).
            let reach = (8.0 * sigma) as usize + 4;
            let var: f64 = v.iter().enumerate().skip(c - reach).take(2 * reach + 1).map(|(i, &x)| f64::from(x) * ((i as f64) - c as f64).powi(2)).sum();
            assert!((var.sqrt() - f64::from(sigma)).abs() < 1e-3 * f64::from(sigma).max(1.0), "sigma {sigma}: {}", var.sqrt());
            assert!((v.iter().sum::<f32>() - 1.0).abs() < 1e-4, "sigma {sigma} keeps the total");
        }
        assert_eq!(BoxPass::for_sigma(0.0), None);
        assert_eq!(BoxPass::for_sigma(f32::NAN), None);
        assert!(BoxPass::for_sigma(f32::MAX).is_some_and(|p| p.r < 2000 && (0.0..1.0).contains(&p.end)));
    }

    /// A feathered edge follows the Gaussian Reference draws with sigma = feather: psd-tools
    /// mask-parameters-no-real-channel.psd (a 6 px vector and 3 px pixel feather) matches it
    /// within 1.5 levels, where whole-pixel boxes were 4 off.
    #[test]
    fn a_feathered_edge_is_a_gaussian_step() {
        // erf, Abramowitz & Stegun 7.1.26 (1.5e-7).
        let erf = |x: f64| {
            let t = 1.0 / (1.0 + 0.327_591_1 * x.abs());
            let y = 1.0 - (((((1.061_405_429 * t - 1.453_152_027) * t) + 1.421_413_741) * t - 0.284_496_736) * t + 0.254_829_592) * t * (-x * x).exp();
            y.copysign(x)
        };
        let (w, edge, sigma) = (240usize, 120usize, 6.0f32);
        let mut v: Vec<f32> = (0..w).map(|i| if i >= edge { 1.0 } else { 0.0 }).collect();
        gaussian(&mut v, w, 1, sigma);
        for (i, &got) in v.iter().enumerate().skip(edge - 24).take(48) {
            let d = (i as f64 - (edge as f64 - 0.5)) / f64::from(sigma);
            let want = 0.5 * (1.0 + erf(d / std::f64::consts::SQRT_2));
            // Three boxes are not quite a Gaussian: within 2.5 levels (whole boxes: 4.6).
            assert!((f64::from(got) - want).abs() < 0.01, "{i}: {got} vs Gaussian {want}");
        }
    }

    #[test]
    fn parallel_gaussian_matches_the_sequential_reference() {
        let (w, h, sigma): (usize, usize, f32) = (320, 256, 13.0);
        let mut parallel: Vec<_> = (0..w * h).map(|i| ((i * 37 % 1009) as f32) / 1008.0).collect();
        let mut sequential = parallel.clone();
        let pass = BoxPass::for_sigma(sigma).unwrap();
        gaussian_sequential(&mut sequential, w, h, pass);
        gaussian(&mut parallel, w, h, sigma);
        assert_eq!(parallel, sequential, "parallel blur must retain the exact per-line arithmetic");
    }

    #[test]
    #[ignore = "manual 50 MP sequential-vs-parallel mask blur measurement"]
    fn benchmark_50mp_mask_blur() {
        use std::time::Instant;

        let (w, h, sigma): (usize, usize, f32) = (6336, 7920, 1000.0);
        let pass = BoxPass::for_sigma(sigma).unwrap();
        let source: Vec<_> = (0..w * h).map(|i| ((i * 37 % 1009) as f32) / 1008.0).collect();
        let mut sequential = source.clone();
        let start = Instant::now();
        gaussian_sequential(&mut sequential, w, h, pass);
        let sequential_time = start.elapsed();
        let mut parallel = source;
        let start = Instant::now();
        gaussian(&mut parallel, w, h, sigma);
        let parallel_time = start.elapsed();
        assert_eq!(parallel, sequential);
        eprintln!("50 MP mask blur: sequential={sequential_time:?}, parallel={parallel_time:?}");
    }
}
