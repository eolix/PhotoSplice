//! Adjustment layers in Lab documents, on L, a and b as Photoshop applies them.
//!
//! Fitted on the Photoshop corpus `adjustments/lab8` (`photosplice-io` example `cmyk_probe`,
//! `ADJ=1`), against Photoshop's merged L, a, b:
//! - Invert and Posterize act on L, a and b alike (exact);
//! - Brightness/Contrast (both kinds) curve L only, a and b kept (a, b exact);
//! - Levels and Curves apply their Lab records (L, a, b curves);
//! - Threshold splits on L alone (exact) and leaves a neutral grey (a = b = 0);
//! - Gradient Map is driven by L (max error 0.46 → 0.04);
//! - Photo Filter filters the colour unclipped, and Preserve Luminosity keeps L (Reference kit
//!   lab-pf-warm-d50: a and b equal those without it, L the source's).
//!
//! The composite itself is kept in screen RGB, unclipped (colours outside sRGB go below 0 or
//! above 1, so every L, a, b survives), and these convert the backdrop to Lab and back around
//! the adjustment; the others (Hue/Saturation, …) stay on the clipped screen colour, which
//! matches Reference more closely than any Lab rule tried.

use photosplice_color::convert::{lab_to_srgb_unclamped, srgb_to_lab};
use photosplice_doc::Adjustment;
use photosplice_doc::adjust::ToneSpace;

use crate::Buffer;
use crate::adjust::{LUT_SIZE, Transfer, apply_depth, lut, tone_luts_depth};

/// Whether `adj` is applied on L, a, b in a Lab document.
pub fn native(adj: &Adjustment) -> bool {
    matches!(
        adj,
        Adjustment::Invert
            | Adjustment::Posterize { .. }
            | Adjustment::Threshold { .. }
            | Adjustment::GradientMap { .. }
            | Adjustment::BrightnessContrast { .. }
            | Adjustment::Levels { space: ToneSpace::Lab, .. }
            | Adjustment::Curves { space: ToneSpace::Lab, .. }
            | Adjustment::PhotoFilter { .. }
            | Adjustment::HueSaturation { .. }
    )
}

/// Hue/Saturation's hue in a Lab document: the place of (a*, b*) along the edge of the square
/// `max(|a*|, |b*|) = r`, in degrees from the corner (r, r), counterclockwise one corner per 90°
/// (Reference kit lab-col-*: Colorize at hue h, saturation S sets that square's point for r = S,
/// e.g. hue 30 → (13, 40), hue 200 at 100 → (−56, −100); lab-hue90 / -hue180 turn (a, b) to (−b, a) and
/// (−a, −b); hue 30 walks the edge). Returns `(hue, r)`.
pub fn square_hue(a: f32, b: f32) -> (f32, f32) {
    let r = a.abs().max(b.abs());
    if r <= 0.0 || !r.is_finite() {
        return (0.0, 0.0);
    }
    let along = |v: f32| (v + r) / (2.0 * r) * 90.0;
    let h = if b.abs() >= a.abs() {
        if b > 0.0 { 90.0 - along(a) } else { 180.0 + along(a) }
    } else if a < 0.0 {
        180.0 - along(b)
    } else {
        270.0 + along(b)
    };
    (h.rem_euclid(360.0), r)
}

/// The inverse of [`square_hue`]: (a*, b*) at `hue` degrees on the square of half-side `r`.
pub fn square_ab(hue: f32, r: f32) -> (f32, f32) {
    let h = if hue.is_finite() { hue.rem_euclid(360.0) } else { 0.0 };
    let side = ((h / 90.0).floor() as usize).min(3);
    let t = (h - side as f32 * 90.0) / 90.0 * 2.0 * r;
    match side {
        0 => (r - t, r),
        1 => (-r, r - t),
        2 => (-r + t, -r),
        _ => (r, -r + t),
    }
}

/// The hue (degrees) at which Hue/Saturation's colour ranges weigh a Lab pixel of square hue `h`
/// ([`square_hue`]): a little short of it, by 0.2425° steps per 45° (1/4, 1, 1, 2, 2 1/4..3, 3, 3,
/// 4 steps through the eighths, the first and fifth ramping), as Reference's own range hue does
/// for RGB per 60° (Reference kit lab-hues-*: the falloffs' midpoints 0.24° short on 45..135,
/// 0.49° on 135..180, 0.70° on 225..315, 0.97° on 315..360).
pub fn range_hue(h: f32) -> f32 {
    const STEP: f32 = 0.2425;
    let deg = if h.is_finite() { h.rem_euclid(360.0) } else { 0.0 };
    let k = ((deg / 45.0).floor() as usize).min(7);
    let f = deg / 45.0 - k as f32;
    let steps = [f, 1.0, 1.0, 2.0, 2.0 + f, 3.0, 3.0, 4.0][k];
    (deg - steps * STEP).rem_euclid(360.0)
}

/// Colorize's (a*, b*): the point of [`square_ab`] as whole values, even in 16-bit documents,
/// the distance along the side rounded (hues 0..180) or cut (180..360) and, past the side's
/// middle, one unit short (Reference kits lab-col*, 194 hues and saturations: 165 exact, all but
/// one of the rest within a unit; hue 60 at 40 gives −12 where the square has −13.3, hue 200
/// gives −23 for −22.2).
fn colorize_ab(hue: f32, r: f32) -> (f32, f32) {
    if r <= 0.0 || !r.is_finite() {
        return (0.0, 0.0);
    }
    let h = if hue.is_finite() { hue.rem_euclid(360.0) } else { 0.0 };
    let side = (h / 90.0).floor().min(3.0);
    let u = h / 90.0 - side;
    // Rounded on the sides that walk down from +r, cut on the two that walk up from −r.
    let x = 2.0 * r * u;
    let along = if side < 2.0 { x.round() } else { x.floor() } - if u > 0.5 { 1.0 } else { 0.0 };
    let (a, b) = square_ab((side + along / (2.0 * r)) * 90.0, r);
    (a.round(), b.round())
}

/// Hue/Saturation on a Lab document's own channels: Lightness moves L (towards white or black),
/// Hue walks (a*, b*) along their square ([`square_hue`]), Saturation scales them (1 + s below 0,
/// 255/256 / (1 − s) above, kept within −128..127 at their ratio), and Colorize sets the square's
/// point for the hue and saturation, whatever the pixel's own colour (Reference kit lab-*: L, a, b
/// exact to their rounding; Colorize see [`colorize_ab`]). Colour ranges are taken on the same
/// hue, not yet measured.
fn hue_saturation_lab(lab: [f32; 3], adj: &Adjustment, depth: photosplice_color::SampleType) -> [f32; 3] {
    let lab = hue_saturation_lab_exact(lab, adj, depth == photosplice_color::SampleType::U8);
    if depth != photosplice_color::SampleType::U8 {
        return lab;
    }
    // An 8-bit document keeps whole codes (lab8/hue-saturation-colorize.psd: L exact).
    [(lab[0] * 2.55).round() / 2.55, lab[1].round(), lab[2].round()]
}

/// [`hue_saturation_lab`] before an 8-bit document's rounding: there (`eight`), the Lightness
/// slider counts in whole 8-bit steps, cut toward zero (+5 % lightens by 12/255, −10 % darkens by
/// 25/255; Reference corpus lab8 hue-saturation and -colorize: every L code exact).
fn hue_saturation_lab_exact(lab: [f32; 3], adj: &Adjustment, eight: bool) -> [f32; 3] {
    let Adjustment::HueSaturation { hue, saturation, lightness, colorize, ranges } = adj else { return lab };
    // A slider's factor on (a*, b*).
    let factor = |s: f32| {
        let s = if s.is_finite() { (s / 100.0).clamp(-1.0, 1.0) } else { 0.0 };
        if s > 0.0 { (255.0 / 256.0) / (1.0 - s).max(1e-4) } else { 1.0 + s }
    };
    let lighten = |v: f32, l: f32| {
        let l = if l.is_finite() { l.clamp(-1.0, 1.0) } else { 0.0 };
        let l = if eight { (l * 255.0).trunc() / 255.0 } else { l };
        if l > 0.0 { v + (100.0 - v) * l } else { v * (1.0 + l) }
    };
    let big_l = lighten(lab[0], lightness / 100.0);
    if *colorize {
        let (a, b) = colorize_ab(*hue, saturation.abs().min(100.0));
        return [big_l, a, b];
    }
    let (h, r) = square_hue(lab[1], lab[2]);
    if r <= 0.0 {
        return [big_l, lab[1], lab[2]];
    }
    // Ranges on the square's hue ([`range_hue`]), weighted down their falloffs and added up:
    // hue w · h, the saturation factor's excess w · (k − 1) times the master's factor, Lightness
    // as the master's at w · l after it (Reference kit lab-hues-reds-h30, -yellows-s50,
    // -reds-s-100, -blues-l-40).
    let at = range_hue(h);
    let (dh, excess, dl) = ranges.iter().filter(|r| !r.is_neutral()).fold((0.0f32, 0.0f32, 0.0f32), |acc, r| {
        let w = r.weight(at);
        (acc.0 + w * r.hue, acc.1 + w * (factor(r.saturation) - 1.0), acc.2 + w * r.lightness / 100.0)
    });
    let big_l = lighten(big_l, dl);
    let k = (factor(*saturation) * (1.0 + excess)).max(0.0);
    // Kept within ±128 at their ratio, then within the channels' range (Reference kit lab-sat50).
    let (a, b) = square_ab(h + hue + dh, (r * k).min(128.0));
    [big_l, a.min(127.0), b.min(127.0)]
}

/// Legacy Brightness/Contrast on L, sampled like [`ramp`]: brightness in levels, then contrast
/// about level 127 with its slider in whole 8-bit steps, 255 / (255 − ⌊2.55 · c⌋) (Reference
/// corpus lab8 brightness-contrast-legacy.psd, +25 / +30: 129 of 131 unclipped L codes exact,
/// slope 255/179; the RGB rule's pivot 128 and 1 / 0.7 had 76 off).
fn legacy_l_ramp(brightness: f32, contrast: f32) -> Vec<f32> {
    const PIVOT: f32 = 127.0 / 255.0;
    let b = if brightness.is_finite() { brightness / 255.0 } else { 0.0 };
    let c = if contrast.is_finite() { contrast.clamp(-100.0, 99.0) } else { 0.0 };
    let q = (2.55 * c.abs()).trunc();
    let k = if c >= 0.0 { 255.0 / (255.0 - q) } else { (255.0 - q) / 255.0 };
    let (pre, post) = if c >= 0.0 { (b, 0.0) } else { (0.0, b) };
    (0..LUT_SIZE).map(|i| (((i as f32 / (LUT_SIZE - 1) as f32) + pre - PIVOT) * k + PIVOT + post).clamp(0.0, 1.0)).collect()
}

/// `adj`'s curve sampled on a gray ramp (the same function for every channel).
fn ramp(adj: &Adjustment, transfer: Transfer, depth: photosplice_color::SampleType) -> Vec<f32> {
    let n = LUT_SIZE;
    let mut b = Buffer { rect: photosplice_geom::Rect::new(0, 0, n as i32, 1), px: (0..n).map(|k| [k as f32 / (n - 1) as f32; 4]).collect() };
    b.px.iter_mut().for_each(|p| p[3] = 1.0);
    apply_depth(adj, &mut b, transfer, Some(depth));
    b.px.iter().map(|p| p[0]).collect()
}

/// Clips `buf`'s colours to sRGB, for the adjustments and blends that work on the screen colour.
pub(crate) fn clip(buf: &mut Buffer) {
    for p in &mut buf.px {
        for v in &mut p[..3] {
            *v = v.clamp(0.0, 1.0);
        }
    }
}

/// Rounds `buf` (screen RGB, unclipped) to the document's `q` levels on its stored L, a, b,
/// where Reference rounds a Lab document after each adjustment layer: rounding the screen colour
/// instead moved out-of-gamut colours by a level of L (an inverted L 153 came out 103, not 102).
/// Alpha is rounded as is.
pub(crate) fn quantize(buf: &mut Buffer, q: f32) {
    let step = |v: f32| (v * q + 0.5).floor() / q;
    for p in &mut buf.px {
        p[3] = step(p[3]);
        if p[3] <= 0.0 {
            continue;
        }
        let lab = srgb_to_lab([p[0], p[1], p[2]]);
        let n = [lab[0] / 100.0, (lab[1] + 128.0) / 255.0, (lab[2] + 128.0) / 255.0].map(step);
        p[..3].copy_from_slice(&lab_to_srgb_unclamped([n[0] * 100.0, n[1] * 255.0 - 128.0, n[2] * 255.0 - 128.0]));
    }
}

/// Applies a [`native`] adjustment to `buf` (screen RGB) on its L, a, b.
pub(crate) fn apply(adj: &Adjustment, buf: &mut Buffer, transfer: Transfer, depth: photosplice_color::SampleType) {
    let ident: Vec<f32> = (0..LUT_SIZE).map(|k| k as f32 / (LUT_SIZE - 1) as f32).collect();
    let luts: [Vec<f32>; 3] = match adj {
        Adjustment::Levels { .. } | Adjustment::Curves { .. } => {
            let [l, a, b, _] = tone_luts_depth(adj, Some(depth));
            [l, a, b]
        }
        Adjustment::BrightnessContrast { brightness, contrast, legacy: true } => [legacy_l_ramp(*brightness, *contrast), ident.clone(), ident],
        Adjustment::BrightnessContrast { .. } => [ramp(adj, transfer, depth), ident.clone(), ident],
        Adjustment::Threshold { .. } => [ident.clone(), ident.clone(), ident],
        _ => {
            let r = ramp(adj, transfer, depth);
            [r.clone(), r.clone(), r]
        }
    };
    if let Adjustment::HueSaturation { .. } = adj {
        for p in &mut buf.px {
            if p[3] <= 0.0 {
                continue;
            }
            let lab = hue_saturation_lab(srgb_to_lab([p[0], p[1], p[2]]), adj, depth);
            p[..3].copy_from_slice(&lab_to_srgb_unclamped(lab));
        }
        return;
    }
    if let Adjustment::PhotoFilter { color, density, preserve_luminosity } = adj {
        use photosplice_color::convert::{linear_to_srgb, mat3_mul, srgb_to_linear};
        let m = crate::adjust::photo_filter_matrix(*color, *density, false, false);
        for p in &mut buf.px {
            if p[3] <= 0.0 {
                continue;
            }
            let c = [p[0], p[1], p[2]];
            let f = mat3_mul(&m, c.map(srgb_to_linear)).map(linear_to_srgb);
            let f = if *preserve_luminosity {
                let mut lab = srgb_to_lab(f);
                lab[0] = srgb_to_lab(c)[0];
                lab_to_srgb_unclamped(lab)
            } else {
                f
            };
            p[..3].copy_from_slice(&f);
        }
        return;
    }
    // Gradient Map: driven by L, the stops' colour out.
    if let Adjustment::GradientMap { stops, reverse, .. } = adj {
        let stops = &crate::adjust::by_location(stops);
        for p in &mut buf.px {
            if p[3] <= 0.0 {
                continue;
            }
            let l = srgb_to_lab([p[0], p[1], p[2]])[0] / 100.0;
            let t = if *reverse { 1.0 - l } else { l };
            let mut o = crate::adjust::gradient(stops, t.clamp(0.0, 1.0));
            if depth == photosplice_color::SampleType::U8 {
                // An 8-bit document cuts the gradient's L, a* and b* to whole codes (Reference
                // corpus lab8/gradient-map.psd: b* 40.6 gives 40).
                let lab = srgb_to_lab(o);
                let cut = |v: f32, scale: f32| (v * scale + 1e-3).floor() / scale;
                o = lab_to_srgb_unclamped([cut(lab[0], 2.55), cut(lab[1], 1.0), cut(lab[2], 1.0)]);
            }
            p[..3].copy_from_slice(&o);
        }
        return;
    }
    // a* and b* on the 8-bit scale (0..255, 128 neutral), except that Invert flips 16-bit
    // samples about their true centre (Reference's 16-bit Lab stores 32768 + 256·a*): a* goes
    // to −a* − 1 at 8 bits and to −a* at 16 (Reference corpus lab8 / lab16 invert.psd).
    // Posterize still steps on the 8-bit scale at 16 bits (lab16 posterize-4.psd).
    let ab_span = if matches!(adj, Adjustment::Invert) && depth != photosplice_color::SampleType::U8 { 256.0 } else { 255.0 };
    // 16-bit Posterize steps on the 8-bit scale but lands on 16-bit levels: neutral a* goes to
    // 2/3 of 256 − 128 = 42.67, not 42, and the top level is 127 (lab16 posterize-4.psd).
    let out_span = if matches!(adj, Adjustment::Posterize { .. }) && depth != photosplice_color::SampleType::U8 { 256.0 } else { ab_span };
    let top = if out_span != ab_span { 127.0 } else { f32::MAX };
    let threshold = match adj {
        Adjustment::Threshold { level } => Some((level * 255.0).round()),
        _ => None,
    };
    for p in &mut buf.px {
        if p[3] <= 0.0 {
            continue;
        }
        let lab = srgb_to_lab([p[0], p[1], p[2]]);
        let n = [lab[0] / 100.0, (lab[1] + 128.0) / ab_span, (lab[2] + 128.0) / ab_span];
        let o: [f32; 3] = match threshold {
            // Compared on the 8-bit grid like the other modes.
            Some(t) => [if (n[0] * 255.0).round() >= t { 1.0 } else { 0.0 }, 128.0 / 255.0, 128.0 / 255.0],
            None => std::array::from_fn(|i| lut(&luts[i], n[i])),
        };
        let ab = |v: f32| (v * out_span - 128.0).min(top);
        let rgb = lab_to_srgb_unclamped([o[0] * 100.0, ab(o[1]), ab(o[2])]);
        p[..3].copy_from_slice(&rgb);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photosplice_color::convert::lab_to_srgb;

    /// Reference kit lab-pf-warm-d50 (Warming 85, 50 %): a colour outside sRGB is filtered
    /// unclipped, and Preserve Luminosity keeps its L with the a, b of the plain filter.
    #[test]
    fn photo_filter_works_unclipped_and_keeps_l() {
        let pf = |preserve_luminosity| Adjustment::PhotoFilter { color: [0.925_485_2, 0.541_191_8, 0.0], density: 0.5, preserve_luminosity };
        let lab = [54.29, 80.81, 69.89];
        let run = |adj: Adjustment| {
            let mut b = one(lab_to_srgb_unclamped(lab));
            apply(&adj, &mut b, Transfer::Srgb, photosplice_color::SampleType::U16);
            srgb_to_lab([b.px[0][0], b.px[0][1], b.px[0][2]])
        };
        let (plain, kept) = (run(pf(false)), run(pf(true)));
        for (got, want) in [(plain, [45.94, 80.19, 65.48]), (kept, [54.29, 80.19, 65.48])] {
            for i in 0..3 {
                assert!((got[i] - want[i]).abs() < 0.1, "{got:?} vs {want:?}");
            }
        }
    }

    fn hs(hue: f32, saturation: f32, lightness: f32, colorize: bool) -> Adjustment {
        Adjustment::HueSaturation { hue, saturation, lightness, colorize, ranges: photosplice_doc::adjust::HueRange::defaults() }
    }

    fn run_lab(adj: &Adjustment, lab: [f32; 3]) -> [f32; 3] {
        let mut b = one(lab_to_srgb_unclamped(lab));
        apply(adj, &mut b, Transfer::Srgb, photosplice_color::SampleType::U16);
        srgb_to_lab([b.px[0][0], b.px[0][1], b.px[0][2]])
    }

    fn near3(got: [f32; 3], want: [f32; 3], tol: f32) {
        assert!((0..3).all(|i| (got[i] - want[i]).abs() < tol), "{got:?} vs {want:?}");
    }

    #[test]
    fn square_hue_walks_the_edge_and_back() {
        assert_eq!(square_hue(40.0, 40.0), (0.0, 40.0));
        assert_eq!(square_hue(-40.0, 40.0), (90.0, 40.0));
        assert_eq!(square_hue(-40.0, -40.0), (180.0, 40.0));
        assert_eq!(square_hue(40.0, -40.0), (270.0, 40.0));
        assert_eq!(square_hue(0.0, 0.0), (0.0, 0.0));
        for (a, b) in [(13.0, 40.0), (-40.0, 7.5), (-22.0, -40.0), (40.0, -39.0), (80.81, 69.89), (-0.5, 0.25)] {
            let (h, r) = square_hue(a, b);
            let (a2, b2) = square_ab(h, r);
            assert!((a - a2).abs() < 1e-3 && (b - b2).abs() < 1e-3, "({a}, {b}) → {h}° → ({a2}, {b2})");
        }
        // Hostile hues stay on the square.
        let (a, b) = square_ab(f32::NAN, 10.0);
        assert!(a.is_finite() && b.is_finite());
    }

    /// Reference kit lab-col-*: Colorize sets the same whole a*, b* on the square everywhere and
    /// keeps the pixel's L, moved by Lightness.
    #[test]
    fn colorize_sets_the_squares_point() {
        let lab = [69.39, 80.81, 69.89];
        near3(run_lab(&hs(0.0, 40.0, 0.0, true), lab), [69.39, 40.0, 40.0], 0.05);
        near3(run_lab(&hs(30.0, 40.0, 0.0, true), lab), [69.39, 13.0, 40.0], 0.05);
        near3(run_lab(&hs(200.0, 100.0, 0.0, true), lab), [69.39, -56.0, -100.0], 0.05);
        near3(run_lab(&hs(200.0, 25.0, 0.0, true), lab), [69.39, -14.0, -25.0], 0.05);
        // Past a side's middle, one unit short of the square (lab-col16-h060-s40, -h240-s40).
        near3(run_lab(&hs(60.0, 40.0, 0.0, true), lab), [69.39, -12.0, 40.0], 0.05);
        near3(run_lab(&hs(240.0, 40.0, 0.0, true), lab), [69.39, 12.0, -40.0], 0.05);
        near3(run_lab(&hs(225.0, 40.0, 0.0, true), lab), [69.39, 0.0, -40.0], 0.05);
        near3(run_lab(&hs(200.0, 40.0, 0.0, true), lab), [69.39, -23.0, -40.0], 0.05);
        near3(run_lab(&hs(-160.0, 40.0, 0.0, true), lab), [69.39, -23.0, -40.0], 0.05);
        near3(run_lab(&hs(90.0, 40.0, 0.0, true), lab), [69.39, -40.0, 40.0], 0.05);
        near3(run_lab(&hs(0.0, 40.0, -50.0, true), lab), [34.70, 40.0, 40.0], 0.05);
        near3(run_lab(&hs(0.0, 40.0, 50.0, true), lab), [84.70, 40.0, 40.0], 0.05);
    }

    /// Reference kit lab-hue*, lab-sat*, lab-light*: the master sliders on L and the square.
    #[test]
    fn master_sliders_move_l_and_the_square() {
        let lab = [54.29, 80.81, 69.89];
        near3(run_lab(&hs(90.0, 0.0, 0.0, false), lab), [54.29, -69.89, 80.81], 0.05);
        near3(run_lab(&hs(180.0, 0.0, 0.0, false), lab), [54.29, -80.81, -69.89], 0.05);
        near3(run_lab(&hs(30.0, 0.0, 0.0, false), lab), [54.29, 37.88, 80.81], 0.05);
        near3(run_lab(&hs(0.0, -50.0, 0.0, false), lab), [54.29, 40.405, 34.945], 0.05);
        near3(run_lab(&hs(0.0, 0.0, -30.0, false), lab), [38.0, 80.81, 69.89], 0.05);
        near3(run_lab(&hs(0.0, 0.0, 30.0, false), lab), [68.0, 80.81, 69.89], 0.05);
        // Saturation +50 (×1.992) past the range: back to ±128 at the ratio, then 127 at most
        // (lab-sat50: (−68.67, 82.65) → (−106.28, 127)).
        near3(run_lab(&hs(0.0, 50.0, 0.0, false), [89.11, -68.67, 82.65]), [89.11, -106.35, 127.0], 0.05);
    }

    /// Reference kit lab-hues-*: ranges weigh the square's hue a little short of it, hue and
    /// lightness scale with the weight down the falloffs.
    #[test]
    fn lab_ranges_follow_the_square_hue() {
        assert_eq!(range_hue(0.0), 0.0);
        assert!((range_hue(60.0) - (60.0 - 0.2425)).abs() < 1e-4);
        assert!((range_hue(330.0) - (330.0 - 0.97)).abs() < 1e-4);
        assert!(range_hue(f32::NAN).is_finite());
        let range = |i: usize, hue: f32, lightness: f32| {
            let mut r = photosplice_doc::adjust::HueRange::defaults();
            r[i] = photosplice_doc::adjust::HueRange { hue, lightness, ..r[i] };
            Adjustment::HueSaturation { hue: 0.0, saturation: 0.0, lightness: 0.0, colorize: false, ranges: r }
        };
        // Blues lightness −40 in the core: L × 0.6, a* and b* kept.
        let (a, b) = square_ab(240.0, 40.0);
        near3(run_lab(&range(4, 0.0, -40.0), [50.0, a, b]), [30.0, a, b], 0.05);
        // Reds hue +30 at 330 (falloff 315..345): weighed at 329.03, so turned 14.03°.
        let (a, b) = square_ab(330.0, 40.0);
        let out = run_lab(&range(0, 30.0, 0.0), [50.0, a, b]);
        let (h, r) = square_hue(out[1], out[2]);
        assert!((h - 344.03).abs() < 0.05 && (r - 40.0).abs() < 0.05, "{h} {r}");
    }

    fn run_at(adj: &Adjustment, lab: [f32; 3], depth: photosplice_color::SampleType) -> [f32; 3] {
        let mut b = one(lab_to_srgb_unclamped(lab));
        apply(adj, &mut b, Transfer::Srgb, depth);
        srgb_to_lab([b.px[0][0], b.px[0][1], b.px[0][2]])
    }

    /// Reference corpus lab8 brightness-contrast-legacy.psd (+25, +30): L code 139 → 180 and
    /// 15 → 3, contrast pivoting at 127 with slope 255/179.
    #[test]
    fn legacy_brightness_contrast_on_l_pivots_at_127() {
        let adj = Adjustment::BrightnessContrast { brightness: 25.0, contrast: 30.0, legacy: true };
        for (code, want) in [(139.0, 180.0), (15.0, 3.0), (21.0, 12.0)] {
            let out = run_at(&adj, [code / 2.55, 10.0, -20.0], photosplice_color::SampleType::U8);
            assert!(((out[0] * 2.55).round() - want).abs() < 0.5, "{code}: {out:?}");
            assert!((out[1] - 10.0).abs() < 0.05 && (out[2] + 20.0).abs() < 0.05, "{out:?}");
        }
    }

    /// Reference corpus lab16 posterize-4.psd: neutral a*, b* land on 2/3 of the 16-bit scale,
    /// and the top level is 127.
    #[test]
    fn posterize_16_bit_lands_on_16_bit_levels() {
        let p4 = Adjustment::Posterize { levels: 4 };
        near3(run_at(&p4, [0.0, 0.0, 0.0], photosplice_color::SampleType::U16), [0.0, 42.67, 42.67], 0.05);
        near3(run_at(&p4, [33.33, 68.0, -112.0], photosplice_color::SampleType::U16), [33.33, 127.0, -128.0], 0.05);
        // 8-bit keeps its own levels.
        near3(run_at(&p4, [0.0, 0.0, 0.0], photosplice_color::SampleType::U8), [0.0, 42.0, 42.0], 0.05);
    }

    /// Reference corpus lab8/gradient-map.psd: an 8-bit document cuts the gradient colour to
    /// whole codes (b* 40.6 → 40).
    #[test]
    fn gradient_map_8_bit_cuts_to_whole_codes() {
        let c = lab_to_srgb_unclamped([54.9, 60.2, 40.6]);
        let gm = Adjustment::GradientMap { stops: vec![(0.0, c), (1.0, c)], reverse: false, dither: false, ink_stops: Vec::new() };
        let out = run_at(&gm, [50.0, 0.0, 0.0], photosplice_color::SampleType::U8);
        near3(out, [(54.9f32 * 2.55).floor() / 2.55, 60.0, 40.0], 0.05);
        let out = run_at(&gm, [50.0, 0.0, 0.0], photosplice_color::SampleType::U16);
        near3(out, [54.9, 60.2, 40.6], 0.05);
    }

    /// A Lab document keeps colours outside sRGB through its composite, so Invert flips the
    /// file's own L, a, b rather than a clipped screen colour (Reference corpus lab8/invert.psd:
    /// 52 % of pixels off before), and flipping twice gives the colour back.
    #[test]
    fn invert_keeps_colours_outside_srgb() {
        let lab = [46.0, 80.0, -110.0];
        let mut b = one(lab_to_srgb_unclamped(lab));
        apply(&Adjustment::Invert, &mut b, Transfer::Srgb, photosplice_color::SampleType::U8);
        let out = srgb_to_lab([b.px[0][0], b.px[0][1], b.px[0][2]]);
        let want = [100.0 - lab[0], -lab[1] - 1.0, -lab[2] - 1.0];
        for i in 0..3 {
            assert!((out[i] - want[i]).abs() < 0.1, "{out:?} vs {want:?}");
        }
        // 16-bit flips about the true centre: a* → −a*.
        let mut b = one(lab_to_srgb_unclamped(lab));
        apply(&Adjustment::Invert, &mut b, Transfer::Srgb, photosplice_color::SampleType::U16);
        apply(&Adjustment::Invert, &mut b, Transfer::Srgb, photosplice_color::SampleType::U16);
        let back = srgb_to_lab([b.px[0][0], b.px[0][1], b.px[0][2]]);
        for i in 0..3 {
            assert!((back[i] - lab[i]).abs() < 0.1, "{back:?} vs {lab:?}");
        }
    }

    fn one(rgb: [f32; 3]) -> Buffer {
        Buffer { rect: photosplice_geom::Rect::new(0, 0, 1, 1), px: vec![[rgb[0], rgb[1], rgb[2], 1.0]] }
    }

    /// Invert flips L, a and b (Photoshop corpus lab8/invert.psd: exact), not the screen colour.
    /// (A muted colour, so the inverted one is still inside sRGB.)
    #[test]
    fn invert_flips_l_a_and_b() {
        let c = [0.55, 0.45, 0.4];
        let mut b = one(c);
        apply(&Adjustment::Invert, &mut b, Transfer::Srgb, photosplice_color::SampleType::U16);
        let (lab, out) = (srgb_to_lab(c), srgb_to_lab([b.px[0][0], b.px[0][1], b.px[0][2]]));
        assert!((out[0] - (100.0 - lab[0])).abs() < 0.5, "{lab:?} → {out:?}");
        assert!((out[1] + lab[1]).abs() < 1.5 && (out[2] + lab[2]).abs() < 1.5, "{lab:?} → {out:?}");
    }

    /// Threshold gives black or white with no colour: a and b neutral.
    #[test]
    fn threshold_is_neutral() {
        for c in [[0.9, 0.2, 0.1], [0.1, 0.1, 0.6]] {
            let mut b = one(c);
            apply(&Adjustment::Threshold { level: 0.5 }, &mut b, Transfer::Srgb, photosplice_color::SampleType::U8);
            let p = b.px[0];
            assert!((p[0] - p[1]).abs() < 1e-3 && (p[1] - p[2]).abs() < 1e-3 && (p[0] < 0.01 || p[0] > 0.99), "{p:?}");
        }
    }

    /// Gradient Map is driven by L: a mid-grey and a saturated colour of the same L map to the
    /// same stop colour.
    #[test]
    fn gradient_map_follows_l() {
        let stops = vec![(0.0, [0.0, 0.0, 1.0]), (1.0, [1.0, 1.0, 0.0])];
        let gm = Adjustment::GradientMap { stops, reverse: false, dither: false, ink_stops: Vec::new() };
        let gray = lab_to_srgb([60.0, 0.0, 0.0]);
        let red = lab_to_srgb([60.0, 40.0, 30.0]);
        let (mut a, mut b) = (one(gray), one(red));
        apply(&gm, &mut a, Transfer::Srgb, photosplice_color::SampleType::U16);
        apply(&gm, &mut b, Transfer::Srgb, photosplice_color::SampleType::U16);
        for c in 0..3 {
            assert!((a.px[0][c] - b.px[0][c]).abs() < 1e-3, "{:?} vs {:?}", a.px[0], b.px[0]);
        }
    }

    /// Brightness/Contrast changes L only: a and b stay.
    #[test]
    fn brightness_contrast_keeps_a_and_b() {
        let c = [0.7, 0.4, 0.3];
        let mut b = one(c);
        apply(&Adjustment::BrightnessContrast { brightness: 30.0, contrast: 0.0, legacy: true }, &mut b, Transfer::Srgb, photosplice_color::SampleType::U16);
        let (lab, out) = (srgb_to_lab(c), srgb_to_lab([b.px[0][0], b.px[0][1], b.px[0][2]]));
        assert!(out[0] > lab[0] + 5.0, "{lab:?} → {out:?}");
        assert!((out[1] - lab[1]).abs() < 0.5 && (out[2] - lab[2]).abs() < 0.5, "{lab:?} → {out:?}");
    }
}
