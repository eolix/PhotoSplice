//! Evaluation of adjustment layers on a composite buffer.
//!
//! Formulas are documented approximations of Photoshop behaviour. Exact matching is tuned in
//! milestone M7 against Photoshop-rendered PSD composites (the oracle in `testkit`).

use photosuite_color::SampleType;
use photosuite_color::convert::{D50, SRGB_TO_XYZ_D50, XYZ_D50_TO_SRGB, mat3_mul, rgb_to_gray, srgb_to_linear};
use photosuite_doc::Adjustment;
use photosuite_doc::adjust::{CurvePoint, HueRange, LevelsChannel, ToneSpace, lut3d_len};

use crate::Buffer;

/// The document tone curve used to linearize values for adjustments that
/// work in linear light (Exposure). Until ICC profiles are wired in (M8) this
/// approximates the Photoshop defaults: sRGB for colour documents and the
/// "Dot Gain 20%" grey profile (≈ gamma 1.73, fitted on the corpus) for
/// grayscale documents. 32-bit documents store linear light already.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Transfer {
    /// sRGB piecewise curve.
    Srgb,
    /// Pure power law with this exponent.
    Gamma(f32),
}

impl Transfer {
    /// Default transfer for a document's colour mode and depth. 32-bit samples are linear, and
    /// Photoshop's Exposure works on them as stored (photoshop corpus rgb32 and gray32
    /// exposure.psd: within 0.5/255, against 69/255 through the 2.2 curve).
    pub fn for_document(mode: photosuite_color::ColorMode, depth: photosuite_color::SampleType) -> Self {
        if depth == photosuite_color::SampleType::F32 {
            return Transfer::Gamma(1.0);
        }
        match mode {
            photosuite_color::ColorMode::Grayscale | photosuite_color::ColorMode::Duotone | photosuite_color::ColorMode::Bitmap => Transfer::Gamma(1.732),
            _ => Transfer::Srgb,
        }
    }
    /// The tone curve Exposure linearises through: RGB documents use a pure 2.2 power, not the
    /// sRGB curve (psd-tools adjustment_nested_composition_4: an offset of 0.1738 lifts 76 to 134,
    /// not 136; ag-psd masks), grey documents their dot-gain gamma.
    pub fn for_exposure(self) -> Self {
        match self {
            Transfer::Srgb => Transfer::Gamma(2.2),
            t => t,
        }
    }
    fn decode(self, v: f32) -> f32 {
        match self {
            Transfer::Srgb => photosuite_color::convert::srgb_to_linear(v.max(0.0)),
            Transfer::Gamma(g) => v.max(0.0).powf(g),
        }
    }
    fn encode(self, v: f32) -> f32 {
        match self {
            Transfer::Srgb => photosuite_color::convert::linear_to_srgb(v.max(0.0)),
            Transfer::Gamma(g) => v.max(0.0).powf(1.0 / g),
        }
    }
}

/// Applies an adjustment assuming an sRGB document.
pub fn apply(adj: &Adjustment, buf: &mut Buffer) {
    apply_with(adj, buf, Transfer::Srgb);
}

/// Applies an adjustment with the document's tone transfer.
pub fn apply_with(adj: &Adjustment, buf: &mut Buffer, transfer: Transfer) {
    apply_depth(adj, buf, transfer, None);
}

/// Applies an adjustment with the document's tone transfer, in a document of `depth` (`None`:
/// unrounded). Integer depths work on whole levels like Photoshop's (see [`levels_q`] and
/// `adjustment_quantum`), 32-bit documents get its float Levels (see [`levels_float`]).
pub fn apply_depth(adj: &Adjustment, buf: &mut Buffer, transfer: Transfer, depth: Option<SampleType>) {
    match adj {
        Adjustment::Invert => map_rgb(buf, |c| [1.0 - c[0], 1.0 - c[1], 1.0 - c[2]]),
        Adjustment::Threshold { level } => {
            // Compared on the 8-bit grid like Photoshop (avoids float ties).
            let t = (level * 255.0).round();
            map_rgb(buf, |c| {
                let v = if (rgb_to_gray(c) * 255.0).round() >= t { 1.0 } else { 0.0 };
                [v; 3]
            })
        }
        Adjustment::Posterize { levels } => map_rgb(buf, |c| c.map(|v| posterize(v, *levels))),
        Adjustment::BrightnessContrast { brightness, contrast, legacy: true } => {
            let f = legacy_brightness_contrast(*brightness, *contrast);
            map_rgb(buf, |px| px.map(|v| f(v).clamp(0.0, 1.0)))
        }
        Adjustment::BrightnessContrast { brightness, contrast, .. } => {
            // Modern Brightness/Contrast: smooth curves that pin pure black and white, brightness
            // then contrast (`modern_brightness`, `modern_contrast`; both exact to Reference).
            let t = brightness_contrast_lut(*brightness, *contrast, false);
            map_rgb(buf, |px| px.map(|v| lut(&t, v)))
        }
        Adjustment::Exposure { exposure, offset, gamma } => {
            // In linear light: (lin·2^exposure + offset)^(1/gamma), then back
            // through the document tone curve (fitted on the corpus).
            let m = 2f32.powf(*exposure);
            let g = gamma.max(0.01);
            let transfer = transfer.for_exposure();
            map_rgb(buf, |c| {
                c.map(|v| {
                    let lin = (transfer.decode(v) * m + offset).max(0.0).powf(1.0 / g);
                    transfer.encode(lin).clamp(0.0, 1.0)
                })
            })
        }
        Adjustment::Levels { space, .. } | Adjustment::Curves { space, .. } => {
            let luts = tone_luts_depth(adj, depth);
            match space {
                ToneSpace::Rgb => map_rgb(buf, |c| std::array::from_fn(|i| lut(&luts[i], c[i]))),
                ToneSpace::Cmyk | ToneSpace::Lab => map_rgb(buf, |c| tone_in_space(*space, &luts, c)),
            }
        }
        Adjustment::HueSaturation { hue, saturation, lightness, colorize, ranges } => {
            let table = (!*colorize && ranges.iter().any(|r| !r.is_neutral())).then(|| hue_range_tables(ranges));
            map_rgb(buf, |c| {
                let (dh, ds, dl) = match &table {
                    Some(t) => {
                        // Range edits follow the pixel's original hue (on Reference's hue scale),
                        // faded out towards grey (greys have no hue).
                        let h0 = range_hue(rgb_to_hsl(c).0);
                        let chroma = ((c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])) * 4.0).min(1.0);
                        (lut(&t[0], h0) * chroma, lut(&t[1], h0) * chroma, lut(&t[2], h0) * chroma)
                    }
                    None => (0.0, 0.0, 0.0),
                };
                if *colorize {
                    let s = (*saturation / 100.0).clamp(-1.0, 1.0);
                    return hue_saturation_in(c, *hue + dh, s, (*lightness / 100.0 + dl).clamp(-1.0, 1.0), true, false);
                }
                // The master's saturation factor times the ranges' (Reference 16-bit master ±50 %
                // with Reds ∓50 %: unchanged in the core; adding the sliders was 31/255 off).
                let s = master_and_range_saturation(*saturation / 100.0, ds);
                hue_saturation_ranged(c, *hue + dh, s, (*lightness / 100.0).clamp(-1.0, 1.0), dl.clamp(-1.0, 1.0), depth == Some(SampleType::F32), table.is_some())
            })
        }
        Adjustment::Vibrance { vibrance, saturation } => {
            let table = vibrance_lut(*vibrance);
            let k = 1.0 + (saturation / 100.0).clamp(-1.0, 1.0);
            map_rgb(buf, |c| vibrance_px(c, &table, k, transfer))
        }
        Adjustment::ChannelMixer { matrix, monochrome, .. } => map_rgb(buf, |c| {
            let mix = |row: &[f32; 4]| (row[0] * c[0] + row[1] * c[1] + row[2] * c[2] + row[3]).clamp(0.0, 1.0);
            if *monochrome { [mix(&matrix[0]); 3] } else { [mix(&matrix[0]), mix(&matrix[1]), mix(&matrix[2])] }
        }),
        Adjustment::PhotoFilter { color, density, preserve_luminosity } => {
            let linear_doc = depth == Some(SampleType::F32);
            let m = photo_filter_matrix(*color, *density, *preserve_luminosity && linear_doc);
            let set_lum = *preserve_luminosity && !linear_doc;
            map_rgb(buf, |c| {
                let f = mat3_mul(&m, c.map(|v| transfer.decode(v))).map(|v| transfer.encode(v));
                let f = if set_lum { photosuite_color::blend::set_lum(f, photosuite_color::blend::lum(c)) } else { f };
                f.map(|v| v.clamp(0.0, 1.0))
            })
        }
        Adjustment::BlackWhite { weights, tint } => map_rgb(buf, |c| {
            let g = black_white_gray(c, weights);
            match tint {
                // The tint colour at the grey's luminosity, as the Color blend mode does
                // (Photoshop corpus black-white-tint.psd, rgb16).
                Some(t) => photosuite_color::blend::set_lum(*t, g).map(|v| v.clamp(0.0, 1.0)),
                None => [g; 3],
            }
        }),
        Adjustment::GradientMap { stops, reverse, dither, .. } => {
            let (w, x0, y0) = (buf.rect.width().max(1) as usize, buf.rect.x0, buf.rect.y0);
            let stops = &by_location(stops);
            for (i, p) in buf.px.iter_mut().enumerate() {
                if p[3] <= 0.0 {
                    continue;
                }
                let mut t = gradient_map_level([p[0], p[1], p[2]]);
                if *reverse {
                    t = 1.0 - t;
                }
                let mut o = gradient(stops, t);
                if *dither {
                    let d = bayer4(x0 + (i % w) as i32, y0 + (i / w) as i32) / 255.0;
                    o = o.map(|v| (v + d).clamp(0.0, 1.0));
                }
                p[..3].copy_from_slice(&o);
            }
        }
        Adjustment::ColorBalance { shadows, midtones, highlights, preserve_luminosity } => {
            let t = color_balance_luts(shadows, midtones, highlights, *preserve_luminosity);
            map_rgb(buf, |c| std::array::from_fn(|i| t.get(i).map_or(c[i], |row| lut(row, c[i]))))
        }
        Adjustment::SelectiveColor { relative, adjustments } => map_rgb(buf, |c| selective_color(c, *relative, adjustments)),
        Adjustment::ColorLookup { lut: Some(table), size, tetrahedral, dither, .. } if lut3d_len(*size).is_some_and(|len| table.len() >= len) => {
            let (n, w, x0, y0) = (*size as usize, buf.rect.width().max(1) as usize, buf.rect.x0, buf.rect.y0);
            for (i, p) in buf.px.iter_mut().enumerate() {
                if p[3] <= 0.0 {
                    continue;
                }
                let mut o = lut3d_sample(table, n, [p[0], p[1], p[2]], *tetrahedral);
                if *dither {
                    let d = bayer4(x0 + (i % w) as i32, y0 + (i / w) as i32) / 255.0;
                    o = o.map(|v| (v + d).clamp(0.0, 1.0));
                }
                p[..3].copy_from_slice(&o);
            }
        }
        // Not evaluated: identity (still round-trips through PSD).
        Adjustment::ColorLookup { .. } | Adjustment::Unsupported { .. } => {}
    }
}

/// Per-channel LUTs of a Levels or Curves adjustment, each channel's record composed with the
/// master. Photoshop's two differ: Levels applies the channel first, then the composite (psd-tools
/// levels_rgb.psd: 114/255 off the other way round), Curves the composite first, then the channel
/// (psd-tools curves_rgb.psd, Photoshop corpus curves-per-channel.psd). Four rows: the three
/// `per_channel` channels then black (identity unless the space is CMYK). In Lab there is no
/// composite record, so the master is ignored.
pub fn tone_luts(adj: &Adjustment) -> [Vec<f32>; 4] {
    tone_luts_q(adj, None)
}

/// [`tone_luts`] for samples with `quantum` steps per unit (Levels works on whole levels; see
/// [`levels_q`]).
pub fn tone_luts_q(adj: &Adjustment, quantum: Option<f32>) -> [Vec<f32>; 4] {
    let x = |k: usize| k as f32 / (LUT_SIZE - 1) as f32;
    match adj {
        Adjustment::Levels { master, per_channel, space, black } => {
            let ident = LevelsChannel::default();
            let m = if *space == ToneSpace::Lab { &ident } else { master };
            let row = |c: &LevelsChannel| (0..LUT_SIZE).map(|k| levels_q(m, levels_q(c, x(k), quantum), quantum)).collect();
            [row(&per_channel[0]), row(&per_channel[1]), row(&per_channel[2]), row(if *space == ToneSpace::Cmyk { black } else { &ident })]
        }
        Adjustment::Curves { master, per_channel, space, black } => {
            let m = if *space == ToneSpace::Lab { curve_lut(&[]) } else { curve_lut(master) };
            let row = |c: &[CurvePoint]| curve_lut(c).iter().map(|&v| lut(&m, v)).collect();
            [row(&per_channel[0]), row(&per_channel[1]), row(&per_channel[2]), row(if *space == ToneSpace::Cmyk { black } else { &[] })]
        }
        _ => std::array::from_fn(|_| (0..LUT_SIZE).map(x).collect()),
    }
}

/// [`tone_luts_q`] in a document of `depth` (`None`: unrounded). 32-bit Levels use
/// [`levels_float`], sampled on 0..1 and kept in 0..1 like every adjustment result here.
pub fn tone_luts_depth(adj: &Adjustment, depth: Option<SampleType>) -> [Vec<f32>; 4] {
    match (adj, depth) {
        (Adjustment::Levels { master, per_channel, space: ToneSpace::Rgb, .. }, Some(SampleType::F32)) => {
            let x = |k: usize| k as f32 / (LUT_SIZE - 1) as f32;
            let row = |c: &LevelsChannel| (0..LUT_SIZE).map(|k| levels_float(master, levels_float(c, x(k))).clamp(0.0, 1.0)).collect();
            [row(&per_channel[0]), row(&per_channel[1]), row(&per_channel[2]), (0..LUT_SIZE).map(x).collect()]
        }
        _ => tone_luts_q(adj, depth.and_then(crate::adjustment_quantum)),
    }
}

/// Applies channel LUTs to a display-RGB colour in CMYK (ink brightness, 1 - ink) or Lab space,
/// through the conversions the document's surfaces use. The change is added as a difference of
/// two round trips, so channels a curve leaves alone (and out-of-gamut colours) stay exact.
fn tone_in_space(space: ToneSpace, luts: &[Vec<f32>; 4], c: [f32; 3]) -> [f32; 3] {
    use photosuite_color::convert::{cmyk_to_rgb, lab_to_srgb, rgb_to_cmyk, srgb_to_lab};
    let (before, after) = match space {
        ToneSpace::Cmyk => {
            let ink = rgb_to_cmyk(c);
            let out: [f32; 4] = std::array::from_fn(|i| 1.0 - lut(&luts[i], 1.0 - ink[i]));
            if out == ink {
                return c;
            }
            (cmyk_to_rgb(ink), cmyk_to_rgb(out))
        }
        ToneSpace::Lab => {
            let l = srgb_to_lab(c);
            let n = [l[0] / 100.0, (l[1] + 128.0) / 255.0, (l[2] + 128.0) / 255.0];
            let o: [f32; 3] = std::array::from_fn(|i| lut(&luts[i], n[i]));
            if o == n {
                return c;
            }
            let back = |v: [f32; 3]| lab_to_srgb([v[0] * 100.0, v[1] * 255.0 - 128.0, v[2] * 255.0 - 128.0]);
            (back(n), back(o))
        }
        ToneSpace::Rgb => return std::array::from_fn(|i| lut(&luts[i], c[i])),
    };
    std::array::from_fn(|i| (c[i] + after[i] - before[i]).clamp(0.0, 1.0))
}

/// Where a hue (in turns) falls among Hue/Saturation's colour ranges, in turns. Reference's hue
/// runs 60/255° short per sextant: steady 1, 2, 3 and 4 steps behind in the sextants whose middle
/// channel is red or blue, sliding by one step across the two where it is green (0–60°,
/// 180–240°), whatever the saturation and lightness (16-bit Lightness +40 curves of all six
/// ranges: the falloffs move by up to 0.94°).
pub fn range_hue(h: f32) -> f32 {
    let deg = h.rem_euclid(1.0) * 360.0;
    let k = ((deg / 60.0).floor() as usize).min(5);
    let f = deg / 60.0 - k as f32;
    let steps = [f, 1.0, 2.0, 2.0 + f, 3.0, 4.0][k];
    (deg - steps * 60.0 / 255.0).rem_euclid(360.0) / 360.0
}

/// Hue/Saturation's range edits as three hue-indexed tables (hue shift in degrees, saturation as
/// its factor's excess `k − 1`, lightness as a fraction), sampled at hue `k / (len - 1)` turns of
/// [`range_hue`]. A range's weight scales the saturation factor's excess, not the slider: +40 %
/// at half weight is 1 + ½ · (1/0.6 − 1), not 1/0.8 (Reference 16-bit Reds falloffs: 6/255 →
/// 0.9/255). Ranges add up where they overlap.
pub fn hue_range_tables(ranges: &[HueRange; 6]) -> [Vec<f32>; 3] {
    let at = |k: usize| {
        let deg = k as f32 / (LUT_SIZE - 1) as f32 * 360.0;
        ranges.iter().filter(|r| !r.is_neutral()).fold([0.0f32; 3], |acc, r| {
            let w = r.weight(deg);
            [acc[0] + w * r.hue, acc[1] + w * saturation_excess(r.saturation / 100.0), acc[2] + w * r.lightness / 100.0]
        })
    };
    let rows: Vec<[f32; 3]> = (0..LUT_SIZE).map(at).collect();
    std::array::from_fn(|i| rows.iter().map(|r| r[i]).collect())
}

/// A saturation slider (−1..=1) as its factor's excess `k − 1` ([`hue_saturation_scale`]).
fn saturation_excess(s: f32) -> f32 {
    let s = if s.is_finite() { s.clamp(-1.0, 0.999) } else { 0.0 };
    if s > 0.0 { s / (1.0 - s) } else { s }
}

/// The saturation slider equivalent to the master slider `master` (−1..=1) combined with the
/// ranges' factor excess `range_excess`: their factors multiply.
pub fn master_and_range_saturation(master: f32, range_excess: f32) -> f32 {
    saturation_of_excess((1.0 + saturation_excess(master)) * (1.0 + range_excess.max(-1.0)) - 1.0)
}

/// The saturation slider whose factor's excess is `e` (inverse of [`saturation_excess`]).
pub fn saturation_of_excess(e: f32) -> f32 {
    if e > 0.0 { 1.0 - 1.0 / (1.0 + e) } else { e.max(-1.0) }
}

/// One Hue/Saturation evaluation: hue shift in degrees, saturation and lightness in -1..=1.
/// Colorize sets the hue and saturation instead of shifting them.
pub fn hue_saturation(c: [f32; 3], hue: f32, s: f32, l: f32, colorize: bool) -> [f32; 3] {
    hue_saturation_in(c, hue, s, l, colorize, false)
}

/// [`hue_saturation`] in an 8/16-bit or a 32-bit (`float`) document.
pub fn hue_saturation_in(c: [f32; 3], hue: f32, s: f32, l: f32, colorize: bool, float: bool) -> [f32; 3] {
    if colorize {
        // Colorize: the hue and saturation as set, on the pixel's HSL lightness moved by the
        // Lightness slider (Photoshop corpus hue-saturation-colorize.psd, rgb16: within 0.5/255).
        let (_, _, ll) = rgb_to_hsl(c);
        let ll = if l > 0.0 { ll + (1.0 - ll) * l } else { ll * (1.0 + l) };
        return hsl_to_rgb(hue.rem_euclid(360.0) / 360.0, s.abs().min(1.0), ll).map(|v| v.clamp(0.0, 1.0));
    }
    hue_saturation_ranged(c, hue, s, l, 0.0, float, false)
}

/// [`hue_saturation_in`] (not colorized) with a range edit's lightness `range_l` (already
/// weighted by the pixel's membership): a raise moves the channels towards the brightest one,
/// not towards white like the master slider (Photoshop corpus hs-blues-light40.psd: the blue
/// square 20 90 200 → 71 121 200; 17/255 → 2/255), a cut towards the darkest one.
/// `ranges`: colour ranges are in use, which also holds the largest channel at 1.
pub fn hue_saturation_ranged(c: [f32; 3], hue: f32, s: f32, l: f32, range_l: f32, float: bool, ranges: bool) -> [f32; 3] {
    // Hue turns the HSL hue; saturation then scales each channel about the HSL lightness by
    // 1 / (1 − s) (s > 0) or 1 + s, held only where the smallest channel would go below 0: the
    // largest may overshoot 1 until the lightness step (Photoshop corpus hue-saturation.psd,
    // rgb16: within 0.9/255; was 17/255 with a clamped HSL saturation).
    let (hh, ss, ll) = rgb_to_hsl(c);
    let mut rgb = hsl_to_rgb((hh + hue / 360.0).rem_euclid(1.0), ss, ll);
    // A range's lightness comes before the saturation step: a raise pulls the channels towards
    // the brightest one, a cut towards the darkest (Reference 16-bit Reds −30 with hue −90 and
    // saturation +40: within 1/255; scaling towards black after saturating was 39/255 off).
    let (max, min) = (rgb[0].max(rgb[1]).max(rgb[2]), rgb[0].min(rgb[1]).min(rgb[2]));
    if range_l > 0.0 {
        rgb = rgb.map(|v| v + (max - v) * range_l);
    } else if range_l < 0.0 {
        rgb = rgb.map(|v| v + (min - v) * -range_l);
    }
    let mut rgb = hue_saturation_scale(rgb, s, float, ranges);
    if l > 0.0 {
        rgb = rgb.map(|v| v + (1.0 - v) * l);
    } else if l < 0.0 {
        rgb = rgb.map(|v| v * (1.0 + l));
    }
    rgb.map(|v| v.clamp(0.0, 1.0))
}

/// Hue/Saturation's saturation step on `rgb` (see [`hue_saturation`]). 32-bit documents don't
/// hold the smallest channel at 0: it may go below and is clipped at the end (Photoshop corpus
/// rgb32 hue-saturation.psd: within 1/255, was 34/255 held).
fn hue_saturation_scale(rgb: [f32; 3], s: f32, float: bool, hold_white: bool) -> [f32; 3] {
    let (max, min) = (rgb[0].max(rgb[1]).max(rgb[2]), rgb[0].min(rgb[1]).min(rgb[2]));
    let mid = (max + min) / 2.0;
    let mut k = if s > 0.0 { 1.0 / (1.0 - s).max(1e-6) } else { 1.0 + s };
    if s > 0.0 && !float && min < mid {
        k = k.min(mid / (mid - min));
    }
    if s > 0.0 && !float && hold_white && max > mid {
        // With colour ranges in use, nor the largest above 1: a colour at full HSL saturation
        // stays as it is (Reference 16-bit Reds +100 %, master +30 % with Reds +40 %); the
        // master alone lets it overshoot until the lightness step.
        k = k.max(1.0).min(((1.0 - mid) / (max - mid)).max(1.0));
    }
    rgb.map(|v| mid + (v - mid) * k)
}

/// Color Balance as one curve per channel (each output channel depends only on its own value;
/// [`LUT_SIZE`] samples): a Levels per channel. Each slider value `s` (−100..100: Cyan–Red,
/// Magenta–Green, Yellow–Blue) moves its channel against the other two:
/// * Shadows: `s > 0` raises the input black of the other two channels by `s`/255 (they darken),
///   `s < 0` that of the slider's own channel.
/// * Highlights: `s > 0` lowers the input white of the slider's channel by `s`/255 (it
///   brightens), `s < 0` that of the other two.
/// * Midtones: the channel's gamma (see the midtones step below).
///
/// That is with Preserve Luminosity; without it see the code. Sliders add up per channel. Exact
/// (within 0.1 levels) on Reference's 16-bit curves, both ways (143 settings).
pub fn color_balance_luts(shadows: &[f32; 3], midtones: &[f32; 3], highlights: &[f32; 3], preserve_luminosity: bool) -> [Vec<f32>; 3] {
    let amount = |s: f32| if s.is_finite() { s.clamp(-100.0, 100.0) } else { 0.0 };
    let (sh, mids, hi) = (shadows.map(amount), midtones.map(amount), highlights.map(amount));
    let (mut black, mut white) = ([0.0f32; 3], [0.0f32; 3]);
    // Midtones: a Levels gamma per channel, `2^(s/100)` rounded to hundredths as Levels keeps it.
    // Preserve Luminosity measures each slider from the middle of the three, (max + min) / 2, so
    // equal sliders cancel.
    let gamma = if preserve_luminosity {
        for ch in 0..3 {
            let others = [(ch + 1) % 3, (ch + 2) % 3];
            if sh[ch] > 0.0 {
                others.iter().for_each(|&k| black[k] += sh[ch] / 255.0);
            } else {
                black[ch] -= sh[ch] / 255.0;
            }
            if hi[ch] > 0.0 {
                white[ch] += hi[ch] / 255.0;
            } else {
                others.iter().for_each(|&k| white[k] -= hi[ch] / 255.0);
            }
        }
        let shift = (mids.iter().copied().fold(f32::MIN, f32::max) + mids.iter().copied().fold(f32::MAX, f32::min)) / 2.0;
        mids.map(|s| (s - shift) / 100.0)
    } else {
        // Without it every slider stays on its own channel: a shadows cut and a highlights raise
        // still move the black and white points, and each shadows and highlights slider also
        // bends the gamma by half a midtones step; the exponents add before the rounding.
        for ch in 0..3 {
            black[ch] = (-sh[ch]).max(0.0) / 255.0;
            white[ch] = hi[ch].max(0.0) / 255.0;
        }
        std::array::from_fn(|ch| mids[ch] / 100.0 + (sh[ch] + hi[ch]) / 200.0)
    }
    .map(|e: f32| (e.exp2() * 100.0).round() / 100.0);
    std::array::from_fn(|k| {
        // Shadows and highlights set the input black and white points, as in Levels.
        let (b, w) = (black[k].min(0.99), white[k].min(0.99));
        let range = (1.0 - w - b).max(1e-3);
        (0..LUT_SIZE)
            .map(|i| {
                let v = i as f32 / (LUT_SIZE - 1) as f32;
                midtone_gamma(((v - b) / range).clamp(0.0, 1.0), gamma[k]).clamp(0.0, 1.0)
            })
            .collect()
    })
}

/// Saturation measured on the Vibrance factor tables ([`VIB_ROWS`]): `1 − min / max` of the
/// linear-light channels.
const VIB_S: [f32; 31] = [
    0.0000, 0.1537, 0.2639, 0.2788, 0.2946, 0.3092, 0.3520, 0.4013, 0.5812, 0.6238, 0.6557, 0.6938, 0.7268, 0.7600, 0.7813, 0.8174,
    0.8259, 0.8575, 0.8707, 0.9194, 0.9277, 0.9440, 0.9555, 0.9599, 0.9664, 0.9788, 0.9816, 0.9834, 0.9851, 0.9879, 1.0000,
];
/// Photoshop's Vibrance factor `f` at each [`VIB_S`], per slider value: the channels move away
/// from (`f > 1`) or towards (`f < 1`) the brightest one. Measured on the Photoshop corpus
/// vibrance-*.psd (±50, +100; −100 extrapolated from −50).
const VIB_ROWS: [(f32, [f32; 31]); 5] = [
    (
        -100.0,
        [
            0.0000, 0.0000, 0.0446, 0.0476, 0.0000, 0.0000, 0.0572, 0.0879, 0.2644, 0.3087, 0.3490, 0.3844, 0.4095, 0.4552, 0.4771, 0.5165,
            0.5175, 0.5544, 0.5783, 0.6312, 0.6425, 0.6586, 0.6770, 0.7033, 0.7097, 0.7253, 0.7267, 0.7300, 0.7333, 0.7361, 0.7504,
        ],
    ),
    (
        -50.0,
        [
            0.4032, 0.4565, 0.5223, 0.5238, 0.4948, 0.4978, 0.5286, 0.5440, 0.6322, 0.6544, 0.6745, 0.6922, 0.7047, 0.7276, 0.7385, 0.7582,
            0.7588, 0.7772, 0.7891, 0.8156, 0.8213, 0.8293, 0.8385, 0.8517, 0.8549, 0.8627, 0.8634, 0.8650, 0.8667, 0.8681, 0.8752,
        ],
    ),
    (0.0, [1.0; 31]),
    (
        50.0,
        [
            1.0000, 1.0527, 1.0284, 1.0530, 1.0497, 1.0467, 1.0591, 1.0661, 1.0644, 1.0641, 1.0650, 1.0572, 1.0566, 1.0548, 1.0506, 1.0470,
            1.0453, 1.0416, 1.0387, 1.0274, 1.0253, 1.0198, 1.0169, 1.0146, 1.0130, 1.0081, 1.0073, 1.0069, 1.0058, 1.0052, 1.0000,
        ],
    ),
    (
        100.0,
        [
            1.0000, 1.1051, 1.1126, 1.1312, 1.1472, 1.1385, 1.1552, 1.1775, 1.2064, 1.1945, 1.1942, 1.1764, 1.1668, 1.1551, 1.1465, 1.1309,
            1.1262, 1.1095, 1.1018, 1.0608, 1.0557, 1.0438, 1.0352, 1.0317, 1.0271, 1.0174, 1.0152, 1.0137, 1.0123, 1.0101, 1.0000,
        ],
    ),
];

/// Luminance weights of Vibrance's Saturation slider (linear light; fitted on the Photoshop
/// corpus vibrance-0-±50.psd: within 0.8/255 at −50).
const VIB_LUMA: [f32; 3] = [0.29, 0.71, 0.0];

/// The Vibrance factor over saturation `S` (`0..=1`, [`LUT_SIZE`] samples) for slider `vibrance`
/// (−100..100), interpolated between the measured sliders. The CPU reads it with [`lut`]; the GPU
/// uploads it as a LUT row.
pub fn vibrance_lut(vibrance: f32) -> Vec<f32> {
    let v = if vibrance.is_finite() { vibrance.clamp(-100.0, 100.0) } else { 0.0 };
    let row = |r: &[f32; 31], s: f32| {
        let i = VIB_S.iter().take(VIB_S.len() - 1).rposition(|&k| k <= s).unwrap_or(0);
        let (s0, s1) = (VIB_S.get(i).copied().unwrap_or(0.0), VIB_S.get(i + 1).copied().unwrap_or(1.0));
        let (f0, f1) = (r.get(i).copied().unwrap_or(1.0), r.get(i + 1).copied().unwrap_or(1.0));
        f0 + (f1 - f0) * ((s - s0) / (s1 - s0).max(1e-6)).clamp(0.0, 1.0)
    };
    let pair = VIB_ROWS.windows(2).find(|w| matches!(w, [a, b] if v >= a.0 && v <= b.0));
    (0..LUT_SIZE)
        .map(|k| {
            let s = k as f32 / (LUT_SIZE - 1) as f32;
            match pair {
                Some([a, b]) => {
                    let t = (v - a.0) / (b.0 - a.0);
                    row(&a.1, s) * (1.0 - t) + row(&b.1, s) * t
                }
                _ => 1.0,
            }
        })
        .collect()
}

/// Vibrance on one colour, in linear light: the channels move from the brightest by the factor
/// for their saturation (`table`, see [`vibrance_lut`]), then Saturation blends them with the
/// luminance by `k` (`1 + saturation`). Photoshop corpus vibrance-*.psd: within 1.5/255 at
/// ±50, 6/255 at +100 (the old HSL guess was 89/255 off).
fn vibrance_px(c: [f32; 3], table: &[f32], k: f32, transfer: Transfer) -> [f32; 3] {
    let mut l = c.map(|v| transfer.decode(v));
    let (max, min) = (l[0].max(l[1]).max(l[2]), l[0].min(l[1]).min(l[2]));
    if max > 0.0 {
        let f = lut(table, 1.0 - min / max);
        l = l.map(|x| (max - (max - x) * f).max(0.0));
    }
    let y = VIB_LUMA[0] * l[0] + VIB_LUMA[1] * l[1] + VIB_LUMA[2] * l[2];
    l.map(|x| transfer.encode(y + (x - y) * k).clamp(0.0, 1.0))
}

/// Black & White grey value. Each colour is the sum of a grey part (its smallest channel), a
/// secondary-colour part (yellow, cyan or magenta: middle − smallest) and a primary part (red,
/// green or blue: largest − middle); the sliders (percent, Photoshop defaults 40 60 40 60 20 80 for
/// reds, yellows, greens, cyans, blues, magentas) weight the two colour parts.
pub fn black_white_gray(c: [f32; 3], weights: &[f32; 6]) -> f32 {
    let (r, g, b) = (c[0], c[1], c[2]);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let mid = r + g + b - max - min;
    // Primary: the largest channel; secondary: the two largest channels together.
    let primary = if r >= g && r >= b {
        0
    } else if g >= b {
        2
    } else {
        4
    };
    let secondary = if b <= r && b <= g {
        1 // red + green = yellow
    } else if r <= g {
        3 // green + blue = cyan
    } else {
        5 // red + blue = magenta
    };
    (min + (mid - min) * weights[secondary] / 100.0 + (max - mid) * weights[primary] / 100.0).clamp(0.0, 1.0)
}

/// Ordered-dither offset in -0.5..0.5 (4×4 Bayer matrix) for document pixel (x, y).
pub fn bayer4(x: i32, y: i32) -> f32 {
    const M: [f32; 16] = [0.0, 8.0, 2.0, 10.0, 12.0, 4.0, 14.0, 6.0, 3.0, 11.0, 1.0, 9.0, 15.0, 7.0, 13.0, 5.0];
    (M[((y & 3) * 4 + (x & 3)) as usize] + 0.5) / 16.0 - 0.5
}

/// Samples a 3D LUT (`n`³ RGB triplets, red fastest) at `c`, trilinear or tetrahedral.
pub fn lut3d_sample(lut: &[f32], n: usize, c: [f32; 3], tetrahedral: bool) -> [f32; 3] {
    let m = (n - 1) as f32;
    let pos = c.map(|v| v.clamp(0.0, 1.0) * m);
    let i0 = pos.map(|p| (p.floor() as usize).min(n - 2));
    let f: [f32; 3] = std::array::from_fn(|k| pos[k] - i0[k] as f32);
    let at = |dr: usize, dg: usize, db: usize| -> [f32; 3] {
        let idx = (((i0[2] + db) * n + i0[1] + dg) * n + i0[0] + dr) * 3;
        [lut[idx], lut[idx + 1], lut[idx + 2]]
    };
    let mix = |w: &[(f32, [f32; 3])]| -> [f32; 3] { std::array::from_fn(|k| w.iter().map(|(a, v)| a * v[k]).sum()) };
    let (fr, fg, fb) = (f[0], f[1], f[2]);
    if tetrahedral {
        let (c000, c111) = (at(0, 0, 0), at(1, 1, 1));
        if fr > fg {
            if fg > fb {
                mix(&[(1.0 - fr, c000), (fr - fg, at(1, 0, 0)), (fg - fb, at(1, 1, 0)), (fb, c111)])
            } else if fr > fb {
                mix(&[(1.0 - fr, c000), (fr - fb, at(1, 0, 0)), (fb - fg, at(1, 0, 1)), (fg, c111)])
            } else {
                mix(&[(1.0 - fb, c000), (fb - fr, at(0, 0, 1)), (fr - fg, at(1, 0, 1)), (fg, c111)])
            }
        } else if fb > fg {
            mix(&[(1.0 - fb, c000), (fb - fg, at(0, 0, 1)), (fg - fr, at(0, 1, 1)), (fr, c111)])
        } else if fb > fr {
            mix(&[(1.0 - fg, c000), (fg - fb, at(0, 1, 0)), (fb - fr, at(0, 1, 1)), (fr, c111)])
        } else {
            mix(&[(1.0 - fg, c000), (fg - fr, at(0, 1, 0)), (fr - fb, at(1, 1, 0)), (fb, c111)])
        }
    } else {
        let lerp = |a: [f32; 3], b: [f32; 3], t: f32| -> [f32; 3] { std::array::from_fn(|k| a[k] + (b[k] - a[k]) * t) };
        let c00 = lerp(at(0, 0, 0), at(1, 0, 0), fr);
        let c10 = lerp(at(0, 1, 0), at(1, 1, 0), fr);
        let c01 = lerp(at(0, 0, 1), at(1, 0, 1), fr);
        let c11 = lerp(at(0, 1, 1), at(1, 1, 1), fr);
        lerp(lerp(c00, c10, fg), lerp(c01, c11, fg), fb)
    }
}

/// How much a colour belongs to each Selective Color range: reds, yellows, greens, cyans,
/// blues, magentas (by chroma within the hue sector), whites, neutrals and blacks (by lightness).
pub fn selective_color_weights(c: [f32; 3]) -> [f32; 9] {
    let max = c[0].max(c[1]).max(c[2]);
    let min = c[0].min(c[1]).min(c[2]);
    let mid = c[0] + c[1] + c[2] - max - min;
    let top = |i: usize| if c[i] >= max { max - mid } else { 0.0 };
    let bottom = |i: usize| if c[i] <= min { mid - min } else { 0.0 };
    [
        top(0),
        bottom(2),
        top(1),
        bottom(0),
        top(2),
        bottom(1),
        ((min - 0.5) * 2.0).max(0.0),
        (1.0 - (max - 0.5).abs() - (min - 0.5).abs()).clamp(0.0, 1.0),
        ((0.5 - max) * 2.0).max(0.0),
    ]
}

/// Photoshop Selective Color: each range moves the cyan, magenta and yellow ink (1 − R, G, B)
/// to a target, clamped to 0..100 %, and the colour moves towards it by how much it belongs to
/// the range. Relative mode scales the ink by its percentage, then by the black percentage
/// (`ink · (1 + a) · (1 + k)`, so it can't tint pure white); absolute adds both outright.
/// Fitted on Photoshop corpus selective-color.psd (relative), rgb16: within 1/255, was 11/255.
pub fn selective_color(c: [f32; 3], relative: bool, adj: &[[f32; 4]; 9]) -> [f32; 3] {
    let w = selective_color_weights(c);
    let mut delta = [0.0f32; 3];
    for (r, wr) in w.iter().enumerate() {
        if *wr <= 0.0 {
            continue;
        }
        let a = adj[r].map(|v| v / 100.0);
        for i in 0..3 {
            let ink = 1.0 - c[i];
            let target = if relative { ink * (1.0 + a[i]) * (1.0 + a[3]) } else { ink + a[i] + a[3] };
            delta[i] += (target.clamp(0.0, 1.0) - ink) * wr;
        }
    }
    std::array::from_fn(|i| (c[i] - delta[i]).clamp(0.0, 1.0))
}

pub(crate) const LUT_SIZE: usize = 4096;

fn map_rgb(buf: &mut Buffer, f: impl Fn([f32; 3]) -> [f32; 3]) {
    for p in &mut buf.px {
        if p[3] <= 0.0 {
            continue;
        }
        let r = f([p[0], p[1], p[2]]);
        p[0] = r[0];
        p[1] = r[1];
        p[2] = r[2];
    }
}

#[inline]
pub(crate) fn lut(table: &[f32], v: f32) -> f32 {
    let x = v.clamp(0.0, 1.0) * (table.len() - 1) as f32;
    let i = x.floor() as usize;
    let j = (i + 1).min(table.len() - 1);
    let f = x - i as f32;
    table[i] * (1.0 - f) + table[j] * f
}

/// Photo Filter's linear-light RGB matrix. Photoshop multiplies the pixel's D50 XYZ, relative to
/// white, by the filter colour's (mixed in by `density`): a fixed linear map, so it is folded into
/// one matrix. With `normalize_y` the map keeps luminance Y (32-bit Preserve Luminosity; 8/16-bit
/// documents instead restore the encoded luminosity like the Luminosity blend mode). Fitted on the
/// photoshop corpus `photo-filter.psd`: rgb16 within 0.6/255 everywhere.
pub fn photo_filter_matrix(color: [f32; 3], density: f32, normalize_y: bool) -> [[f32; 3]; 3] {
    let d = if density.is_finite() { density.clamp(0.0, 1.0) } else { 0.0 };
    let xyz = mat3_mul(&SRGB_TO_XYZ_D50, color.map(|v| srgb_to_linear(v.clamp(0.0, 1.0))));
    let mut s: [f32; 3] = std::array::from_fn(|i| 1.0 - d + d * xyz[i] / D50[i]);
    if normalize_y && s[1] > 1e-6 {
        let y = s[1];
        s = s.map(|v| v / y);
    }
    // XYZ_D50_TO_SRGB · diag(s) · SRGB_TO_XYZ_D50
    std::array::from_fn(|r| std::array::from_fn(|c| (0..3).map(|k| XYZ_D50_TO_SRGB[r][k] * s[k] * SRGB_TO_XYZ_D50[k][c]).sum()))
}

/// Modern Brightness's base curve for `b` in [0, 100] (`x`, output in 0..=1): a line of slope
/// `s = 2^(b/110)` from black up to mid grey, then a cubic Hermite to white leaving at slope `s`
/// and arriving at `max(1 / (1 + 12 (s − 1)), 0.1)`. Fitted on Reference's exact 16-bit curves
/// (a 4096-step ramp at every 10 from −150 to 150): within 0.011 levels of all of them.
fn brightness_base(x: f32, b: f32) -> f32 {
    let s = (b / 110.0).exp2();
    let knee = 0.5 / s;
    if x <= knee {
        return s * x;
    }
    let end = (1.0 / (1.0 + 12.0 * (s - 1.0))).max(0.1);
    let span = 1.0 - knee;
    let t = (x - knee) / span;
    let (t2, t3) = (t * t, t * t * t);
    (2.0 * t3 - 3.0 * t2 + 1.0) * 0.5 + (t3 - 2.0 * t2 + t) * span * s + (-2.0 * t3 + 3.0 * t2) + (t3 - t2) * span * end
}

/// The inverse of [`brightness_base`] (it rises strictly), by bisection.
fn brightness_base_inverse(y: f32, b: f32) -> f32 {
    if y <= 0.0 || y >= 1.0 {
        return y.clamp(0.0, 1.0);
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..32 {
        let mid = 0.5 * (lo + hi);
        if brightness_base(mid, b) < y {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// Modern Brightness curve (one channel, `brightness` in [-150, 150]). Reference builds it from
/// one family ([`brightness_base`], 0..=100): past 100 it applies the 100 curve and then the
/// rest, and a negative slider is the exact inverse of the positive one (both checked on its
/// curves to 0.03 levels). Pins black and white.
pub fn modern_brightness(v: f32, brightness: f32) -> f32 {
    if brightness == 0.0 || !brightness.is_finite() {
        return v;
    }
    let b = brightness.clamp(-150.0, 150.0);
    let x = v.clamp(0.0, 1.0);
    let y = if b > 100.0 {
        brightness_base(brightness_base(x, 100.0), b - 100.0)
    } else if b >= 0.0 {
        brightness_base(x, b)
    } else if b < -100.0 {
        brightness_base_inverse(brightness_base_inverse(x, -b - 100.0), 100.0)
    } else {
        brightness_base_inverse(x, -b)
    };
    y.clamp(0.0, 1.0)
}

/// Where a colour falls on a Gradient Map: 0.30 R + 0.59 G + 0.11 B (fitted on the Reference
/// corpus rgb8/gradient-map.psd, 130 colours: the Rec.601 weights were up to 2 levels off on
/// saturated blues and yellows).
pub fn gradient_map_level(c: [f32; 3]) -> f32 {
    0.30 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

/// The Brightness/Contrast transfer curve sampled at [`LUT_SIZE`] points: the CPU applies it
/// through [`lut`] and the GPU uploads it as a LUT row, so both draw the same curve.
pub fn brightness_contrast_lut(brightness: f32, contrast: f32, legacy: bool) -> Vec<f32> {
    let x = |k: usize| k as f32 / (LUT_SIZE - 1) as f32;
    if legacy {
        let f = legacy_brightness_contrast(brightness, contrast);
        (0..LUT_SIZE).map(|k| f(x(k)).clamp(0.0, 1.0)).collect()
    } else {
        (0..LUT_SIZE).map(|k| modern_contrast(modern_brightness(x(k), brightness), contrast).clamp(0.0, 1.0)).collect()
    }
}

/// Legacy Brightness/Contrast on one channel (unclamped).
fn legacy_brightness_contrast(brightness: f32, contrast: f32) -> impl Fn(f32) -> f32 {
    // Legacy: contrast scales around mid-grey. A positive contrast stretches the input
    // (brightness shifts the input first: Photoshop corpus rgb8 / cmyk8
    // brightness-contrast-legacy.psd, +25 / +30); a negative one compresses the output
    // (brightness is added after: psd-tools brightnesscontrast_legacy_grayscale, −23 / −26).
    let b = brightness / 255.0;
    let c = contrast.clamp(-100.0, 99.0);
    let k = if c >= 0.0 { 1.0 / (1.0 - c / 100.0) } else { 1.0 + c / 100.0 };
    move |v: f32| if c >= 0.0 { (v + b - 0.5) * k + 0.5 } else { (v - 0.5) * k + 0.5 + b }
}

/// Modern Contrast curve (one channel, `contrast` in [-50, 100]), applied after Brightness. A
/// symmetric cubic-Hermite S-curve pivoting at 0.5: endpoint slope `1 − 0.0076 c`, centre slope
/// `1 + 0.0076 c` (Reference's 16-bit curves at every 10 from −50 to 100, and seven brightness and
/// contrast pairs: within 0.01 levels). `c = 0` is the identity; pins 0, 0.5 and 1.
pub fn modern_contrast(v: f32, contrast: f32) -> f32 {
    if contrast == 0.0 || !contrast.is_finite() {
        return v;
    }
    let k = contrast.clamp(-50.0, 100.0) * 0.0076;
    let (end, mid) = (1.0 - k, 1.0 + k);
    // Cubic Hermite on [0, 0.5]: pinned (0,0) slope `end`, (0.5,0.5) slope `mid`. `t` in [0,1].
    let half = |t: f32| end * 0.5 * (t * t * t - 2.0 * t * t + t) + (-2.0 * t * t * t + 3.0 * t * t) * 0.5 + mid * 0.5 * (t * t * t - t * t);
    let v = v.clamp(0.0, 1.0);
    if v <= 0.5 { half(v / 0.5) } else { 1.0 - half((1.0 - v) / 0.5) }
}

/// Reference's Posterize: `n` equal input bins over 0..=255, output levels
/// `floor(k * 255 / (n - 1))` (exact on the corpus for 3, 7, 13 and 21 levels).
pub fn posterize(v: f32, levels: u32) -> f32 {
    let n = levels.clamp(2, 255) as f32;
    let x = (v.clamp(0.0, 1.0) * 255.0).round();
    let bin = (x * n / 256.0).floor().min(n - 1.0);
    (bin * 255.0 / (n - 1.0)).floor() / 255.0
}

pub fn levels(ch: &LevelsChannel, v: f32) -> f32 {
    levels_q(ch, v, None)
}

/// [`levels`] on samples with `quantum` steps per unit (8-bit: 255). Photoshop works on whole
/// levels: the input is a level, and the input-range stretch is rounded (half up) to a whole
/// level before the midtone gamma, so a 44..214 range maps 45, 46, 47, 48 → 1.5, 3, 4.5, 6 →
/// 2, 3, 5, 6 (psd-tools levels_grayscale, where the gamma toe magnifies the steps). In a LUT
/// the result is a staircase that is exact at every level.
pub fn levels_q(ch: &LevelsChannel, v: f32, quantum: Option<f32>) -> f32 {
    let range = (ch.in_white - ch.in_black).max(1e-6);
    let u = match quantum.filter(|q| q.is_finite() && *q >= 1.0) {
        Some(q) => {
            let v = (v * q).round() / q;
            (((v - ch.in_black) / range).clamp(0.0, 1.0) * q + 0.5 + 1e-3).floor() / q
        }
        None => ((v - ch.in_black) / range).clamp(0.0, 1.0),
    };
    let t = midtone_gamma(u, ch.gamma);
    ch.out_black + t * (ch.out_white - ch.out_black)
}

/// Reference's midtone gamma curve (Levels, Color Balance midtones): `u^(1/g)`, except that a
/// gamma above 1 starts at slope `2^g` instead of an infinite one and joins the power curve with
/// a cubic Hermite at `x0 = 2 · (2^g)^(−g/(g−1))`, twice where that line crosses it (16-bit
/// Color Balance curves at gammas 1.19, 1.23, 1.41 and 2.0: within 0.015 levels).
pub fn midtone_gamma(u: f32, gamma: f32) -> f32 {
    let g = if gamma.is_finite() { gamma.max(0.01) } else { 1.0 };
    let u = u.clamp(0.0, 1.0);
    if g <= 1.0 + 1e-4 {
        return u.powf(1.0 / g);
    }
    let s0 = g.exp2();
    let x0 = 2.0 * s0.powf(-g / (g - 1.0));
    if u >= x0 || x0 <= 0.0 {
        return u.powf(1.0 / g);
    }
    let (y0, s1) = (x0.powf(1.0 / g), x0.powf(1.0 / g - 1.0) / g);
    let t = u / x0;
    let (t2, t3) = (t * t, t * t * t);
    (t3 - 2.0 * t2 + t) * x0 * s0 + (-2.0 * t3 + 3.0 * t2) * y0 + (t3 - t2) * x0 * s1
}

/// [`levels`] in a 32-bit document. Neither the input is clipped to the input range nor the result
/// to the output range there, and the midtone gamma is a plain power curve, mirrored below the
/// black point (the oracle corpus' rgb32 and gray32 levels.psd: within 0.5/255, 10/255 off with
/// the clipped curve).
pub fn levels_float(ch: &LevelsChannel, v: f32) -> f32 {
    let t = (v - ch.in_black) / (ch.in_white - ch.in_black).max(1e-6);
    let t = t.signum() * t.abs().powf(1.0 / ch.gamma.max(0.01));
    ch.out_black + t * (ch.out_white - ch.out_black)
}

/// Photoshop curve: a natural cubic spline through the points (it may
/// overshoot between points, as Photoshop's does), constant outside the
/// first/last point, clamped to 0..=1, sampled into a LUT.
pub fn curve_lut(points: &[CurvePoint]) -> Vec<f32> {
    let mut pts: Vec<(f32, f32)> = points.iter().map(|p| (p.input, p.output)).collect();
    pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    pts.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-6);
    if pts.len() < 2 {
        return (0..LUT_SIZE).map(|i| i as f32 / (LUT_SIZE - 1) as f32).collect();
    }
    let n = pts.len();
    // Second derivatives of the natural spline (tridiagonal solve).
    let mut m2 = vec![0.0f64; n];
    if n > 2 {
        let x: Vec<f64> = pts.iter().map(|p| f64::from(p.0)).collect();
        let y: Vec<f64> = pts.iter().map(|p| f64::from(p.1)).collect();
        let mut c = vec![0.0f64; n];
        let mut d = vec![0.0f64; n];
        for i in 1..n - 1 {
            let (h0, h1) = (x[i] - x[i - 1], x[i + 1] - x[i]);
            let a = h0 / 6.0;
            let b = (h0 + h1) / 3.0;
            let cc = h1 / 6.0;
            let r = (y[i + 1] - y[i]) / h1 - (y[i] - y[i - 1]) / h0;
            let denom = b - a * c[i - 1];
            c[i] = cc / denom;
            d[i] = (r - a * d[i - 1]) / denom;
        }
        for i in (1..n - 1).rev() {
            m2[i] = d[i] - c[i] * m2[i + 1];
        }
    }
    (0..LUT_SIZE)
        .map(|k| {
            let xv = k as f32 / (LUT_SIZE - 1) as f32;
            if xv <= pts[0].0 {
                return pts[0].1.clamp(0.0, 1.0);
            }
            if xv >= pts[n - 1].0 {
                return pts[n - 1].1.clamp(0.0, 1.0);
            }
            let i = pts.windows(2).position(|w| xv <= w[1].0).unwrap_or(n - 2);
            let (x0, y0) = (f64::from(pts[i].0), f64::from(pts[i].1));
            let (x1, y1) = (f64::from(pts[i + 1].0), f64::from(pts[i + 1].1));
            let h = x1 - x0;
            let t = f64::from(xv);
            let a = (x1 - t) / h;
            let b = (t - x0) / h;
            let y = a * y0 + b * y1 + ((a * a * a - a) * m2[i] + (b * b * b - b) * m2[i + 1]) * h * h / 6.0;
            (y as f32).clamp(0.0, 1.0)
        })
        .collect()
}

pub(crate) fn gradient(stops: &[(f32, [f32; 3])], t: f32) -> [f32; 3] {
    if stops.is_empty() {
        return [t; 3];
    }
    lerp_stops(stops, t)
}

/// The colour of `stops` (sorted by location) at `t`, linear between neighbours and clamped to
/// the end stops. `[0; N]` when there are none.
pub(crate) fn lerp_stops<const N: usize>(stops: &[(f32, [f32; N])], t: f32) -> [f32; N] {
    match locate(stops, t) {
        Ok((i, k)) => match (stops.get(i), stops.get(i + 1)) {
            (Some(a), Some(b)) => std::array::from_fn(|c| a.1[c] + (b.1[c] - a.1[c]) * k),
            _ => [0.0; N],
        },
        Err(j) => stops.get(j).map_or([0.0; N], |s| s.1),
    }
}

/// Where `t` falls among `stops` sorted by location: `Ok((i, k))` inside the segment from stop
/// `i` to `i + 1` at fraction `k`, or `Err(j)`, the end stop it clamps to (before the first, past
/// the last). A binary search, so dense ramps (a noise gradient's 1024 stops) stay cheap per pixel.
pub(crate) fn locate<T>(stops: &[(f32, T)], t: f32) -> Result<(usize, f32), usize> {
    let j = stops.partition_point(|s| s.0 < t);
    match (j.checked_sub(1).and_then(|i| stops.get(i)), stops.get(j)) {
        (Some(a), Some(b)) => Ok((j - 1, if b.0 > a.0 { (t - a.0) / (b.0 - a.0) } else { 0.0 })),
        (None, _) => Err(0),
        (Some(_), None) => Err(stops.len() - 1),
    }
}

/// `stops` ordered by location (borrowed when they already are), for [`locate`]'s search.
pub(crate) fn by_location<T: Clone>(stops: &[(f32, T)]) -> std::borrow::Cow<'_, [(f32, T)]> {
    if stops.windows(2).all(|w| w[0].0 <= w[1].0) {
        return std::borrow::Cow::Borrowed(stops);
    }
    let mut v = stops.to_vec();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    std::borrow::Cow::Owned(v)
}

pub fn rgb_to_hsl(c: [f32; 3]) -> (f32, f32, f32) {
    let max = c[0].max(c[1]).max(c[2]);
    let min = c[0].min(c[1]).min(c[2]);
    let l = (max + min) / 2.0;
    if (max - min).abs() < 1e-7 {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == c[0] {
        ((c[1] - c[2]) / d).rem_euclid(6.0)
    } else if max == c[1] {
        (c[2] - c[0]) / d + 2.0
    } else {
        (c[0] - c[1]) / d + 4.0
    };
    (h / 6.0, s, l)
}

pub fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [f32; 3] {
    if s <= 0.0 {
        return [l; 3];
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let f = |mut t: f32| {
        t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0)]
}

#[cfg(test)]
mod lookup_tests {
    use super::*;

    fn identity(n: usize) -> Vec<f32> {
        let m = (n - 1) as f32;
        (0..n * n * n).flat_map(|i| [(i % n) as f32 / m, ((i / n) % n) as f32 / m, (i / (n * n)) as f32 / m]).collect()
    }

    #[test]
    fn identity_lut_is_identity_both_interpolations() {
        let t = identity(5);
        for c in [[0.1, 0.5, 0.9], [0.33, 0.77, 0.0], [1.0, 1.0, 1.0]] {
            for tet in [false, true] {
                let o = lut3d_sample(&t, 5, c, tet);
                for k in 0..3 {
                    assert!((o[k] - c[k]).abs() < 1e-5, "{c:?} {tet} {o:?}");
                }
            }
        }
    }

    #[test]
    fn selective_color_zero_is_identity_and_ranges_are_local() {
        let zero = [[0.0f32; 4]; 9];
        for c in [[0.9, 0.1, 0.1], [0.5, 0.5, 0.5], [0.2, 0.4, 0.8]] {
            assert_eq!(selective_color(c, true, &zero), c);
        }
        // +100 % cyan on reds only: red loses red, blue is untouched.
        let mut a = zero;
        a[0][0] = 100.0;
        let red = selective_color([0.9, 0.1, 0.1], false, &a);
        assert!(red[0] < 0.2, "{red:?}");
        assert_eq!(selective_color([0.1, 0.1, 0.9], false, &a), [0.1, 0.1, 0.9]);
        // Relative mode can't tint pure white; absolute can (via whites).
        let mut w = zero;
        w[6] = [0.0, 0.0, 50.0, 0.0];
        assert_eq!(selective_color([1.0; 3], true, &w), [1.0; 3]);
        assert!(selective_color([1.0; 3], false, &w)[2] < 0.6);
    }

    #[test]
    fn dither_offsets_are_balanced() {
        let sum: f32 = (0..4).flat_map(|y| (0..4).map(move |x| bayer4(x, y))).sum();
        assert!(sum.abs() < 1e-5);
        assert_eq!(bayer4(-4, -4), bayer4(0, 0));
    }
}

#[cfg(test)]
mod brightness_contrast_tests {
    use super::*;

    /// Legacy Brightness/Contrast: with positive contrast brightness shifts the input first
    /// (Photoshop corpus rgb8 / cmyk8 brightness-contrast-legacy.psd, +25 / +30: 85 → 103,
    /// 170 → 224); with negative contrast it is added after (psd-tools
    /// brightnesscontrast_legacy_grayscale, −23 / −26).
    #[test]
    fn legacy_order_depends_on_the_contrast_sign() {
        let at = |b: f32, c: f32, v: f32| {
            let mut buf = Buffer { rect: photosuite_geom::Rect::new(0, 0, 1, 1), px: vec![[v, v, v, 1.0]] };
            apply_depth(&Adjustment::BrightnessContrast { brightness: b, contrast: c, legacy: true }, &mut buf, Transfer::Srgb, None);
            buf.px[0][0] * 255.0
        };
        assert!((at(25.0, 30.0, 85.0 / 255.0) - 103.0).abs() < 1.0);
        assert!((at(25.0, 30.0, 170.0 / 255.0) - 224.0).abs() < 1.0);
        let k = 1.0 - 26.0 / 100.0;
        let want = (100.0 - 127.5) * k + 127.5 - 23.0;
        assert!((at(-23.0, -26.0, 100.0 / 255.0) - want).abs() < 0.01);
    }
}

#[cfg(test)]
mod tone_tests {
    use super::*;
    use photosuite_geom::Rect;

    fn ramp() -> Buffer {
        let rect = Rect::new(0, 0, 8, 8);
        let px = (0..64).map(|i| [((i % 8) as f32) / 7.0, ((i / 8) as f32) / 7.0, 0.3, 1.0]).collect();
        Buffer { rect, px }
    }

    #[test]
    fn identity_tone_in_every_space_is_identity() {
        for space in [ToneSpace::Rgb, ToneSpace::Cmyk, ToneSpace::Lab] {
            let mut a = Adjustment::identity_curves();
            if let Adjustment::Curves { space: s, .. } = &mut a {
                *s = space;
            }
            let mut b = ramp();
            apply(&a, &mut b);
            for (x, y) in b.px.iter().zip(ramp().px.iter()) {
                for k in 0..3 {
                    assert!((x[k] - y[k]).abs() < 0.02, "{space:?} {x:?} {y:?}");
                }
            }
        }
    }

    #[test]
    fn cmyk_black_curve_darkens_and_lab_lightness_lifts() {
        let line = |a: f32, b: f32| vec![CurvePoint { input: 0.0, output: a }, CurvePoint { input: 1.0, output: b }];
        // Black brightness 1 -> 0.5: more black ink everywhere.
        let a = Adjustment::Curves {
            master: line(0.0, 1.0),
            per_channel: [line(0.0, 1.0), line(0.0, 1.0), line(0.0, 1.0)],
            space: ToneSpace::Cmyk,
            black: line(0.0, 0.5),
        };
        let mut b = Buffer { rect: Rect::new(0, 0, 1, 1), px: vec![[0.8, 0.8, 0.8, 1.0]] };
        apply(&a, &mut b);
        assert!(b.px[0][0] < 0.7, "{:?}", b.px[0]);
        // Lab lightness 0..1 -> 0.2..1 lifts a dark grey; a/b untouched keeps it grey.
        let a = Adjustment::Curves {
            master: line(1.0, 0.0), // ignored in Lab
            per_channel: [line(0.2, 1.0), line(0.0, 1.0), line(0.0, 1.0)],
            space: ToneSpace::Lab,
            black: Vec::new(),
        };
        let mut b = Buffer { rect: Rect::new(0, 0, 1, 1), px: vec![[0.2, 0.2, 0.2, 1.0]] };
        apply(&a, &mut b);
        let p = b.px[0];
        assert!(p[0] > 0.3 && (p[0] - p[2]).abs() < 0.02, "{p:?}");
    }

    /// Range edits against Reference's 16-bit composites of a hue sweep (temp kit
    /// `psd-verify-adjust`): Reds with hue −90, saturation +40, lightness −30 on pale and capped
    /// colours, in the core and both falloffs, and Reds lightness +40 in each falloff.
    #[test]
    fn hue_ranges_match_reference() {
        let reds = |h: f32, s: f32, l: f32| {
            let mut r = HueRange::defaults();
            r[0] = HueRange { hue: h, saturation: s, lightness: l, ..r[0] };
            Adjustment::HueSaturation { hue: 0.0, saturation: 0.0, lightness: 0.0, colorize: false, ranges: r }
        };
        let cases: [(Adjustment, [f32; 3], [f32; 3]); 11] = [
            (reds(-90.0, 40.0, -30.0), [0.874998, 0.125002, 0.125002], [98.87, 0.0, 197.65]),
            (reds(-90.0, 40.0, -30.0), [0.874998, 0.500008, 0.125002], [220.87, 5.24, 60.36]),
            (reds(-90.0, 40.0, -30.0), [0.874998, 0.125002, 0.500008], [177.72, 6.83, 221.27]),
            (reds(-90.0, 40.0, -30.0), [0.749996, 0.250004, 0.250004], [108.42, 34.63, 182.13]),
            (reds(-90.0, 40.0, -30.0), [0.749996, 0.500008, 0.250004], [189.75, 45.99, 82.74]),
            (reds(-90.0, 40.0, -30.0), [0.749996, 0.250004, 0.500008], [160.98, 47.06, 190.02]),
            (reds(-90.0, 40.0, -30.0), [1.0, 0.500008, 0.500008], [172.17, 98.38, 245.88]),
            (reds(-90.0, 40.0, -30.0), [1.0, 0.749996, 0.500008], [253.5, 109.74, 146.49]),
            (reds(-90.0, 40.0, -30.0), [1.0, 0.500008, 0.749996], [224.73, 110.81, 253.77]),
            (reds(0.0, 0.0, 40.0), [1.0, 0.333349, 0.0], [255.0, 141.76, 85.17]),
            (reds(0.0, 0.0, 40.0), [1.0, 0.0, 0.500008], [255.0, 47.81, 151.4]),
        ];
        // Two edits on one range, the master with a range, and +100 % on a colour already at
        // full HSL saturation (round 2 of the kit).
        let mut two = HueRange::defaults();
        two[0] = HueRange { saturation: 60.0, ..two[0] };
        two[1] = HueRange { saturation: 60.0, bounds: two[0].bounds, ..two[1] };
        let with_master = |m: f32, r: f32| {
            let mut ranges = HueRange::defaults();
            ranges[0] = HueRange { saturation: r, ..ranges[0] };
            Adjustment::HueSaturation { hue: 0.0, saturation: m, lightness: 0.0, colorize: false, ranges }
        };
        let more: [(Adjustment, [f32; 3], [f32; 3]); 4] = [
            (Adjustment::HueSaturation { hue: 0.0, saturation: 0.0, lightness: 0.0, colorize: false, ranges: two }, [0.624994, 0.375006, 0.375006], [253.26, 1.74, 1.74]),
            (with_master(-50.0, 50.0), [1.0, 0.533318, 0.0], [218.64, 133.59, 36.36]),
            (with_master(30.0, 40.0), [0.624994, 0.375006, 0.375006], [202.33, 52.67, 52.67]),
            (reds(0.0, 100.0, 0.0), [1.0, 0.500008, 0.500008], [255.0, 127.5, 127.5]),
        ];
        // The overlap and the master pair sit within 2/255 of Reference, the rest within 1.
        for (k, (a, c, want)) in cases.into_iter().chain(more).enumerate() {
            let tol = if k == 11 || k == 13 { 2.0 } else { 1.0 };
            let mut b = Buffer { rect: Rect::new(0, 0, 1, 1), px: vec![[c[0], c[1], c[2], 1.0]] };
            apply(&a, &mut b);
            for i in 0..3 {
                assert!((b.px[0][i] * 255.0 - want[i]).abs() < tol, "{c:?}: {:?} vs {want:?}", b.px[0].map(|v| v * 255.0));
            }
        }
    }

    #[test]
    fn hue_ranges_only_touch_their_colours() {
        let mut ranges = HueRange::defaults();
        ranges[0].saturation = -100.0; // desaturate reds
        let a = Adjustment::HueSaturation { hue: 0.0, saturation: 0.0, lightness: 0.0, colorize: false, ranges };
        let mut b = Buffer { rect: Rect::new(0, 0, 3, 1), px: vec![[0.9, 0.1, 0.1, 1.0], [0.1, 0.1, 0.9, 1.0], [0.5, 0.5, 0.5, 1.0]] };
        apply(&a, &mut b);
        let red = b.px[0];
        assert!((red[0] - red[1]).abs() < 0.05, "red desaturated {red:?}");
        assert!((b.px[1][2] - 0.9).abs() < 1e-4 && (b.px[1][0] - 0.1).abs() < 1e-4, "blue untouched {:?}", b.px[1]);
        assert_eq!(b.px[2], [0.5, 0.5, 0.5, 1.0]);
    }

    #[test]
    fn black_white_weights() {
        let d = [40.0, 60.0, 40.0, 60.0, 20.0, 80.0];
        assert!((black_white_gray([0.5; 3], &d) - 0.5).abs() < 1e-6, "greys keep their value");
        assert!((black_white_gray([1.0, 0.0, 0.0], &d) - 0.4).abs() < 1e-6);
        assert!((black_white_gray([1.0, 1.0, 0.0], &d) - 0.6).abs() < 1e-6);
        let mut w = d;
        w[0] = 100.0;
        assert!(black_white_gray([1.0, 0.0, 0.0], &w) > black_white_gray([1.0, 0.0, 0.0], &d));
    }

    /// Gradient Map places colours at 0.30 R + 0.59 G + 0.11 B (Reference corpus
    /// rgb8/gradient-map.psd: pure blue at 28/255 on the black → white map, not Rec.601's 29).
    #[test]
    fn gradient_map_level_weights() {
        let mut b = Buffer { rect: photosuite_geom::Rect::new(0, 0, 1, 1), px: vec![[0.0, 0.0, 1.0, 1.0]] };
        apply(&Adjustment::GradientMap { stops: vec![(0.0, [0.0; 3]), (1.0, [1.0; 3])], reverse: false, dither: false, ink_stops: Vec::new() }, &mut b);
        assert!((b.px[0][0] - 0.11).abs() < 1e-3, "{:?}", b.px[0]);
    }

    #[test]
    fn gradient_map_dither_stays_close() {
        let stops = vec![(0.0, [0.0; 3]), (1.0, [1.0; 3])];
        let mut a = ramp();
        let mut b = ramp();
        apply(&Adjustment::GradientMap { stops: stops.clone(), reverse: false, dither: false, ink_stops: Vec::new() }, &mut a);
        apply(&Adjustment::GradientMap { stops, reverse: false, dither: true, ink_stops: Vec::new() }, &mut b);
        assert!(a.px.iter().zip(&b.px).all(|(x, y)| (x[0] - y[0]).abs() <= 0.5 / 255.0 + 1e-6));
        assert!(a.px.iter().zip(&b.px).any(|(x, y)| x[0] != y[0]));
    }
}
