//! How the gallery's sliders map to the values the effects work with (a gain, a box size, a
//! smoothing σ, a percentile …), where that mapping is not a plain formula of the slider.
//!
//! Every value here was measured from Reference renders: a sweep of each slider over the 600 × 444
//! test image (8-bit, foreground black, background white, random effects seeded with 1), and for
//! each slider position the value that brings our effect closest to the Reference's render,
//! found by searching that one value (per-pixel mean absolute difference; 8 × 8 block means for
//! effects that draw random numbers; where our pattern does not line up with the Reference's, the
//! relief tail is matched on how much the relief changes the image and the Spatter and Sprayed
//! Strokes jitter on how grainy the result is). The curves are piecewise linear through the
//! measured points that matter (`(slider, value)` knots, the measured curve simplified to within
//! its noise) or low-order formulas fitted to them. Where a position shows nothing to fit (it
//! renders the same for a range of values) the neighbouring slope is extended, as noted.

use super::px8::Texture;

/// Linear interpolation through `(x, y)` knots sorted by x; flat beyond the ends (NaN → first).
fn interpolate(knots: &[(f64, f64)], x: f64) -> f64 {
    if x.is_nan() || x <= knots[0].0 {
        return knots[0].1;
    }
    for w in knots.windows(2) {
        let ((x0, y0), (x1, y1)) = (w[0], w[1]);
        if x <= x1 {
            return y0 + (y1 - y0) * (x - x0) / (x1 - x0);
        }
    }
    knots[knots.len() - 1].1
}

/// Colored Pencil and Sumi-e › Stroke Pressure 0–15: the gain on the detail plane, about 4.9 per
/// step to 5, 9.8 per step to 7, then 20. Sumi-e takes 0.85 of it.
const STROKE_AMOUNT: [(f64, f64); 4] = [(0.0, 0.0), (5.0, 24.46), (7.0, 44.1), (15.0, 204.67)];

/// Sprayed Strokes' jitter smoothing (indices 1 and 2): the σ of the 13-tap smoothing first
/// measured on Spatter's Smoothness − 1, matched on how fine the grain is.
const FIELD_SIGMA: [(f64, f64); 8] = [(0.0, 0.3662), (1.0, 0.5254), (4.0, 0.8645), (6.0, 1.035), (7.0, 1.1), (8.0, 1.147), (10.0, 1.201), (14.0, 1.247)];

/// Spatter › Smoothness 1–15: the σ of the 13-tap smoothing of the jitter field and the gain
/// applied after it. Matched on how far the Reference's render moves from the source, per pixel
/// and over 8 × 8 blocks (the reach and the coarseness of the jitter), at every Smoothness of the
/// parrot sweep, then smoothed: its jitter keeps its reach while it coarsens to 4, then shrinks.
/// The boat renders at 1 and 15 confirm both ends.
const SPATTER_SIGMA: [(f64, f64); 9] = [(1.0, 0.6), (2.0, 0.9), (3.0, 1.0), (4.0, 1.1), (6.0, 1.3), (8.0, 1.45), (10.0, 1.55), (11.0, 1.6), (15.0, 1.6)];
const SPATTER_GAIN: [(f64, f64); 7] = [(1.0, 2.2), (4.0, 2.2), (6.0, 2.0), (8.0, 1.75), (10.0, 1.55), (12.0, 1.35), (15.0, 1.1)];

/// Sprayed Strokes › Spray Radius 0–25: the jitter strength added to the base 0.07, matched on
/// how fine the grain is: slow to 17, then much faster.
const SPRAYED_STRENGTH: [(f64, f64); 6] = [(0.0, 0.0709), (4.0, 0.1212), (17.0, 0.4514), (21.0, 0.9385), (22.0, 1.163), (25.0, 2.928)];

/// Relief 41–50 (Rough Pastels, Underpainting, Conté Crayon): `e^(rate·(r − 40))` above 40.
/// Measured on how far each render moves from the one at Relief 0, relative to Relief 40; the
/// same measure confirms `r/40` at 20 and 30.
const RELIEF_TAIL_RATE: f64 = 0.172;

/// Note Paper and Patchwork › Relief 0–25: the relief weight; ~0.1 per step to 13, then steeper.
const RELIEF_25: [(f64, f64); 10] =
    [(0.0, 0.0), (2.0, 0.3832), (13.0, 1.398), (14.0, 1.576), (15.0, 1.856), (18.0, 3.082), (19.0, 3.398), (21.0, 4.977), (22.0, 5.986), (25.0, 8.485)];

/// Torn Edges › Contrast 1–25: the contrast stretch, from 1 (none) to 10.
const TORN_EDGES: [(f64, f64); 9] =
    [(1.0, 1.0), (11.0, 1.5), (14.0, 1.777), (15.0, 1.969), (19.0, 2.935), (21.0, 3.896), (22.0, 4.83), (23.0, 5.863), (25.0, 10.04)];

/// Diffuse Glow › Glow Amount 0–20: the gain on the blurred gray. Glow Amount 0 renders the same
/// for any gain up to 0.6, so the 0–2 slope (0.25) is extended.
const DIFFUSE_GLOW_GLOW: [(f64, f64); 6] = [(0.0, 0.5), (2.0, 1.007), (12.0, 1.508), (15.0, 1.805), (16.0, 2.005), (20.0, 4.008)];

/// Diffuse Glow › Clear Amount 0–20: the inverse gain that clears the glow from the darks.
const DIFFUSE_GLOW_CLEAR: [(f64, f64); 6] = [(0.0, 0.0), (3.0, 0.7491), (4.0, 0.8964), (15.0, 1.986), (17.0, 2.477), (20.0, 3.941)];

/// Stroke Pressure 0–15 → detail gain.
pub(super) fn stroke_amount(p: usize) -> f64 {
    interpolate(&STROKE_AMOUNT, p as f64)
}

/// Neon Glow › |Glow Size| 0–24 → how much wider than |size| the first box blur is. The
/// Reference's width is always odd, `2·⌊(59·a + 30)/100⌋ + 1` (the rule with the least total
/// error over the measured error curves).
pub(super) fn neon_glow_increment(a: usize) -> usize {
    (2 * ((59 * a + 30) / 100) + 1).saturating_sub(a)
}

/// Spatter smoothness index 0–14 → σ.
pub(super) fn field_sigma(j: usize) -> f64 {
    interpolate(&FIELD_SIGMA, j as f64)
}

/// Spatter › Smoothness 1–15 → (σ, gain) of the jitter field.
pub(super) fn spatter_field(s: usize) -> (f64, f64) {
    (interpolate(&SPATTER_SIGMA, s as f64), interpolate(&SPATTER_GAIN, s as f64))
}

/// Sprayed Strokes › Spray Radius 0–25 → added jitter strength.
pub(super) fn sprayed_strength(r: usize) -> f64 {
    interpolate(&SPRAYED_STRENGTH, r as f64)
}

/// A 0–50 Relief slider (clamped) → relief factor (`φ(r)`, 1 at 40).
pub(super) fn relief_factor(r: f64) -> f64 {
    if r.is_nan() || r <= 0.0 {
        0.0
    } else if r <= 40.0 {
        r / 40.0
    } else {
        (RELIEF_TAIL_RATE * (r.min(50.0) - 40.0)).exp()
    }
}

/// A 0–25 Relief slider → relief weight.
pub(super) fn relief_factor_25(r: usize) -> f64 {
    interpolate(&RELIEF_25, r as f64)
}

/// Contrast 1–25 → stretch.
pub(super) fn torn_edges_contrast(c: usize) -> f64 {
    interpolate(&TORN_EDGES, c as f64)
}

/// Glow Amount 0–20 → gain.
pub(super) fn diffuse_glow_glow(g: usize) -> f64 {
    interpolate(&DIFFUSE_GLOW_GLOW, g as f64)
}

/// Clear Amount 0–20 → inverse gain.
pub(super) fn diffuse_glow_clear(c: usize) -> f64 {
    interpolate(&DIFFUSE_GLOW_CLEAR, c as f64)
}

/// Glass › the depth factor of a texture (Frosted has its own model outside the spec mode).
/// Measured on the Reference's boat renders (the spread of its displacements): Tiny Lens moves
/// pixels 0.69 (median) to 0.83 (spread) times as far as the plain depth; Blocks and Canvas
/// match it within 10 %.
pub(super) fn glass_texture_factor(texture: Texture) -> f64 {
    match texture {
        Texture::TinyLens => 0.75,
        _ => 1.0,
    }
}

/// Paint Daubs › brush type → percentile at Brush Size 8. Dark Rough measures below the disc
/// minimum (it is the minimum here). Unknown names rank like Simple.
pub(super) fn paint_daubs_percentile(brush: &str) -> f64 {
    match brush {
        "lightRough" => 0.98,
        "darkRough" => 0.0,
        "wideSharp" => 0.807,
        "wideBlurry" => 0.782,
        "sparkle" => 0.93,
        _ => 0.781,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asserts `f` is finite and strictly rising over `lo..=hi`.
    fn rising(name: &str, f: impl Fn(usize) -> f64, lo: usize, hi: usize) {
        let mut prev = f64::NEG_INFINITY;
        for x in lo..=hi {
            let v = f(x);
            assert!(v.is_finite() && v > prev, "{name} rises at {x}: {prev} → {v}");
            prev = v;
        }
    }

    #[test]
    fn interpolates_and_holds_the_ends() {
        let k = [(0.0, 1.0), (10.0, 3.0), (20.0, 0.0)];
        assert_eq!(interpolate(&k, -5.0), 1.0);
        assert_eq!(interpolate(&k, f64::NAN), 1.0);
        assert_eq!(interpolate(&k, 5.0), 2.0);
        assert_eq!(interpolate(&k, 15.0), 1.5);
        assert_eq!(interpolate(&k, 99.0), 0.0);
    }

    #[test]
    fn rises_where_the_measurements_rise() {
        rising("stroke_amount", stroke_amount, 0, 15);
        rising("field_sigma", field_sigma, 0, 14);
        rising("spatter σ", |s| spatter_field(s).0, 1, 11);
        rising("spatter gain falls", |s| -spatter_field(s).1, 4, 15);
        rising("sprayed_strength", sprayed_strength, 0, 25);
        rising("relief_factor", |r| relief_factor(r as f64), 0, 50);
        rising("relief_factor_25", relief_factor_25, 0, 25);
        rising("torn_edges_contrast", torn_edges_contrast, 1, 25);
        rising("diffuse_glow_glow", diffuse_glow_glow, 0, 20);
        rising("diffuse_glow_clear", diffuse_glow_clear, 0, 20);
    }

    #[test]
    fn starts_at_nothing_and_stays_bounded() {
        assert_eq!(stroke_amount(0), 0.0);
        assert_eq!(relief_factor(0.0), 0.0);
        assert_eq!(relief_factor(-1e6), 0.0);
        assert_eq!(relief_factor(f64::NAN), 0.0);
        assert_eq!(relief_factor(40.0), 1.0);
        assert!((relief_factor(40.0 + 1e-9) - 1.0).abs() < 1e-6);
        assert!(relief_factor(1e6) < 10.0);
        assert_eq!(relief_factor_25(0), 0.0);
        assert_eq!(diffuse_glow_clear(0), 0.0);
        assert_eq!(torn_edges_contrast(1), 1.0);
        assert!(stroke_amount(1000) <= 255.0);
        assert!(torn_edges_contrast(1000) < 12.0);
    }

    #[test]
    fn neon_glow_box_is_odd_and_widens() {
        let mut prev = 0;
        for a in 0..=24 {
            let width = a + neon_glow_increment(a);
            assert_eq!(width % 2, 1, "width at {a}");
            assert!(width >= a.max(1) && width >= prev, "width at {a}: {width}");
            prev = width;
        }
    }

    #[test]
    fn paint_daubs_percentiles_are_fractions() {
        for b in ["simple", "lightRough", "darkRough", "wideSharp", "wideBlurry", "sparkle", "nope"] {
            assert!((0.0..=1.0).contains(&paint_daubs_percentile(b)), "{b}");
        }
        assert!(paint_daubs_percentile("lightRough") > paint_daubs_percentile("simple"));
        assert!(paint_daubs_percentile("simple") > paint_daubs_percentile("darkRough"));
        assert_eq!(paint_daubs_percentile("nope"), paint_daubs_percentile("simple"));
        assert_eq!(glass_texture_factor(Texture::Canvas), 1.0);
        assert!((0.0..1.0).contains(&glass_texture_factor(Texture::TinyLens)));
    }
}
