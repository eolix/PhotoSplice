//! Adjustment layers in Lab documents, on L, a and b as Photoshop applies them.
//!
//! Fitted on the Photoshop corpus `adjustments/lab8` (`photosuite-io` example `cmyk_probe`,
//! `ADJ=1`), against Photoshop's merged L, a, b:
//! - Invert and Posterize act on L, a and b alike (exact);
//! - Brightness/Contrast (both kinds) curve L only, a and b kept (a, b exact);
//! - Levels and Curves apply their Lab records (L, a, b curves);
//! - Threshold splits on L alone (exact) and leaves a neutral grey (a = b = 0);
//! - Gradient Map is driven by L (max error 0.46 → 0.04).
//!
//! The composite itself is kept in screen RGB, unclipped (colours outside sRGB go below 0 or
//! above 1, so every L, a, b survives), and these convert the backdrop to Lab and back around
//! the adjustment; the others (Hue/Saturation, Photo Filter, …) stay on the clipped screen
//! colour, which matches Reference more closely than any Lab rule tried.

use photosuite_color::convert::{lab_to_srgb_unclamped, srgb_to_lab};
use photosuite_doc::Adjustment;
use photosuite_doc::adjust::ToneSpace;

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
    )
}

/// `adj`'s curve sampled on a gray ramp (the same function for every channel).
fn ramp(adj: &Adjustment, transfer: Transfer, depth: photosuite_color::SampleType) -> Vec<f32> {
    let n = LUT_SIZE;
    let mut b = Buffer { rect: photosuite_geom::Rect::new(0, 0, n as i32, 1), px: (0..n).map(|k| [k as f32 / (n - 1) as f32; 4]).collect() };
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

/// Applies a [`native`] adjustment to `buf` (screen RGB) on its L, a, b.
pub(crate) fn apply(adj: &Adjustment, buf: &mut Buffer, transfer: Transfer, depth: photosuite_color::SampleType) {
    let ident: Vec<f32> = (0..LUT_SIZE).map(|k| k as f32 / (LUT_SIZE - 1) as f32).collect();
    let luts: [Vec<f32>; 3] = match adj {
        Adjustment::Levels { .. } | Adjustment::Curves { .. } => {
            let [l, a, b, _] = tone_luts_depth(adj, Some(depth));
            [l, a, b]
        }
        Adjustment::BrightnessContrast { .. } => [ramp(adj, transfer, depth), ident.clone(), ident],
        Adjustment::Threshold { .. } => [ident.clone(), ident.clone(), ident],
        _ => {
            let r = ramp(adj, transfer, depth);
            [r.clone(), r.clone(), r]
        }
    };
    // Gradient Map: driven by L, the stops' colour out.
    if let Adjustment::GradientMap { stops, reverse, .. } = adj {
        let stops = &crate::adjust::by_location(stops);
        for p in &mut buf.px {
            if p[3] <= 0.0 {
                continue;
            }
            let l = srgb_to_lab([p[0], p[1], p[2]])[0] / 100.0;
            let t = if *reverse { 1.0 - l } else { l };
            p[..3].copy_from_slice(&crate::adjust::gradient(stops, t.clamp(0.0, 1.0)));
        }
        return;
    }
    // a* and b* on the 8-bit scale (0..255, 128 neutral), except that Invert flips 16-bit
    // samples about their true centre (Reference's 16-bit Lab stores 32768 + 256·a*): a* goes
    // to −a* − 1 at 8 bits and to −a* at 16 (Reference corpus lab8 / lab16 invert.psd).
    // Posterize still steps on the 8-bit scale at 16 bits (lab16 posterize-4.psd).
    let ab_span = if matches!(adj, Adjustment::Invert) && depth != photosuite_color::SampleType::U8 { 256.0 } else { 255.0 };
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
        let rgb = lab_to_srgb_unclamped([o[0] * 100.0, o[1] * ab_span - 128.0, o[2] * ab_span - 128.0]);
        p[..3].copy_from_slice(&rgb);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photosuite_color::convert::lab_to_srgb;

    /// A Lab document keeps colours outside sRGB through its composite, so Invert flips the
    /// file's own L, a, b rather than a clipped screen colour (Reference corpus lab8/invert.psd:
    /// 52 % of pixels off before), and flipping twice gives the colour back.
    #[test]
    fn invert_keeps_colours_outside_srgb() {
        let lab = [46.0, 80.0, -110.0];
        let mut b = one(lab_to_srgb_unclamped(lab));
        apply(&Adjustment::Invert, &mut b, Transfer::Srgb, photosuite_color::SampleType::U8);
        let out = srgb_to_lab([b.px[0][0], b.px[0][1], b.px[0][2]]);
        let want = [100.0 - lab[0], -lab[1] - 1.0, -lab[2] - 1.0];
        for i in 0..3 {
            assert!((out[i] - want[i]).abs() < 0.1, "{out:?} vs {want:?}");
        }
        // 16-bit flips about the true centre: a* → −a*.
        let mut b = one(lab_to_srgb_unclamped(lab));
        apply(&Adjustment::Invert, &mut b, Transfer::Srgb, photosuite_color::SampleType::U16);
        apply(&Adjustment::Invert, &mut b, Transfer::Srgb, photosuite_color::SampleType::U16);
        let back = srgb_to_lab([b.px[0][0], b.px[0][1], b.px[0][2]]);
        for i in 0..3 {
            assert!((back[i] - lab[i]).abs() < 0.1, "{back:?} vs {lab:?}");
        }
    }

    fn one(rgb: [f32; 3]) -> Buffer {
        Buffer { rect: photosuite_geom::Rect::new(0, 0, 1, 1), px: vec![[rgb[0], rgb[1], rgb[2], 1.0]] }
    }

    /// Invert flips L, a and b (Photoshop corpus lab8/invert.psd: exact), not the screen colour.
    /// (A muted colour, so the inverted one is still inside sRGB.)
    #[test]
    fn invert_flips_l_a_and_b() {
        let c = [0.55, 0.45, 0.4];
        let mut b = one(c);
        apply(&Adjustment::Invert, &mut b, Transfer::Srgb, photosuite_color::SampleType::U16);
        let (lab, out) = (srgb_to_lab(c), srgb_to_lab([b.px[0][0], b.px[0][1], b.px[0][2]]));
        assert!((out[0] - (100.0 - lab[0])).abs() < 0.5, "{lab:?} → {out:?}");
        assert!((out[1] + lab[1]).abs() < 1.5 && (out[2] + lab[2]).abs() < 1.5, "{lab:?} → {out:?}");
    }

    /// Threshold gives black or white with no colour: a and b neutral.
    #[test]
    fn threshold_is_neutral() {
        for c in [[0.9, 0.2, 0.1], [0.1, 0.1, 0.6]] {
            let mut b = one(c);
            apply(&Adjustment::Threshold { level: 0.5 }, &mut b, Transfer::Srgb, photosuite_color::SampleType::U8);
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
        apply(&gm, &mut a, Transfer::Srgb, photosuite_color::SampleType::U16);
        apply(&gm, &mut b, Transfer::Srgb, photosuite_color::SampleType::U16);
        for c in 0..3 {
            assert!((a.px[0][c] - b.px[0][c]).abs() < 1e-3, "{:?} vs {:?}", a.px[0], b.px[0]);
        }
    }

    /// Brightness/Contrast changes L only: a and b stay.
    #[test]
    fn brightness_contrast_keeps_a_and_b() {
        let c = [0.7, 0.4, 0.3];
        let mut b = one(c);
        apply(&Adjustment::BrightnessContrast { brightness: 30.0, contrast: 0.0, legacy: true }, &mut b, Transfer::Srgb, photosuite_color::SampleType::U16);
        let (lab, out) = (srgb_to_lab(c), srgb_to_lab([b.px[0][0], b.px[0][1], b.px[0][2]]));
        assert!(out[0] > lab[0] + 5.0, "{lab:?} → {out:?}");
        assert!((out[1] - lab[1]).abs() < 0.5 && (out[2] - lab[2]).abs() < 0.5, "{lab:?} → {out:?}");
    }
}
