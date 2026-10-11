//! Evaluation of adjustment layers on a composite buffer.
//!
//! Formulas are documented approximations of Photoshop behaviour. Exact matching is tuned in
//! milestone M7 against Photoshop-rendered PSD composites (the oracle in `testkit`).

use photosuite_color::SampleType;
use photosuite_color::convert::{D50, SRGB_TO_XYZ_D50, XYZ_D50_TO_SRGB, mat3_mul, rgb_to_gray, srgb_to_linear};
use photosuite_doc::Adjustment;
use photosuite_doc::adjust::{CurvePoint, HueRange, LevelsChannel, ToneSpace};

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
            // Modern (CS3+) Brightness/Contrast: smooth curves that pin pure black and white,
            // brightness then contrast. Brightness interpolates curves measured from Photoshop
            // (`modern_brightness`); contrast is a symmetric Hermite S-curve (≤0.5/255).
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
                        // Range edits follow the pixel's original hue, faded out towards grey (greys have no hue).
                        let h0 = rgb_to_hsl(c).0;
                        let chroma = ((c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])) * 4.0).min(1.0);
                        (lut(&t[0], h0) * chroma, lut(&t[1], h0) * chroma, lut(&t[2], h0) * chroma)
                    }
                    None => (0.0, 0.0, 0.0),
                };
                hue_saturation(c, *hue + dh, (*saturation / 100.0 + ds).clamp(-1.0, 1.0), (*lightness / 100.0 + dl).clamp(-1.0, 1.0), *colorize)
            })
        }
        Adjustment::Vibrance { vibrance, saturation } => {
            let (v, s) = (*vibrance / 100.0, *saturation / 100.0);
            map_rgb(buf, |c| {
                let (h, sat, l) = rgb_to_hsl(c);
                let boost = v * (1.0 - sat); // less saturated colours move more
                let ns = (sat * (1.0 + s) + boost * sat.max(0.1)).clamp(0.0, 1.0);
                hsl_to_rgb(h, ns, l)
            })
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
                Some(t) => std::array::from_fn(|i| (g * t[i] * 2.0).clamp(0.0, 1.0) * 0.5 + g * 0.5),
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
                let mut t = rgb_to_gray([p[0], p[1], p[2]]);
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
        Adjustment::ColorBalance { shadows, midtones, highlights, preserve_luminosity } => map_rgb(buf, |c| {
            let l = rgb_to_gray(c);
            let ws = (1.0 - l * 2.0).clamp(0.0, 1.0);
            let wh = (l * 2.0 - 1.0).clamp(0.0, 1.0);
            let wm = 1.0 - ws - wh;
            let out: [f32; 3] = std::array::from_fn(|i| (c[i] + (shadows[i] * ws + midtones[i] * wm + highlights[i] * wh) / 100.0 * 0.5).clamp(0.0, 1.0));
            if *preserve_luminosity {
                let l1 = rgb_to_gray(out).max(1e-6);
                out.map(|v| (v * l / l1).clamp(0.0, 1.0))
            } else {
                out
            }
        }),
        Adjustment::SelectiveColor { relative, adjustments } => map_rgb(buf, |c| selective_color(c, *relative, adjustments)),
        Adjustment::ColorLookup { lut: Some(table), size, tetrahedral, dither, .. } if *size >= 2 && table.len() >= (*size as usize).pow(3) * 3 => {
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
/// master (Photoshop applies the channel curve first, then the composite). Four rows: the three
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
            let row = |c: &LevelsChannel| (0..LUT_SIZE).map(|k| levels_q(c, levels_q(m, x(k), quantum), quantum)).collect();
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
            let row = |c: &LevelsChannel| (0..LUT_SIZE).map(|k| levels_float(c, levels_float(master, x(k))).clamp(0.0, 1.0)).collect();
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

/// Hue/Saturation's range edits as three hue-indexed tables (hue shift in degrees, saturation and
/// lightness as fractions), sampled at hue `k / (len - 1)` turns. Ranges add up where they overlap.
pub fn hue_range_tables(ranges: &[HueRange; 6]) -> [Vec<f32>; 3] {
    let at = |k: usize| {
        let deg = k as f32 / (LUT_SIZE - 1) as f32 * 360.0;
        ranges.iter().filter(|r| !r.is_neutral()).fold([0.0f32; 3], |acc, r| {
            let w = r.weight(deg);
            [acc[0] + w * r.hue, acc[1] + w * r.saturation / 100.0, acc[2] + w * r.lightness / 100.0]
        })
    };
    let rows: Vec<[f32; 3]> = (0..LUT_SIZE).map(at).collect();
    std::array::from_fn(|i| rows.iter().map(|r| r[i]).collect())
}

/// One Hue/Saturation evaluation: hue shift in degrees, saturation and lightness in -1..=1.
/// Colorize sets the hue and saturation instead of shifting them.
pub fn hue_saturation(c: [f32; 3], hue: f32, s: f32, l: f32, colorize: bool) -> [f32; 3] {
    let (mut hh, mut ss, ll) = rgb_to_hsl(c);
    if colorize {
        hh = hue.rem_euclid(360.0) / 360.0;
        ss = s.abs().max(0.25);
    } else {
        hh = (hh + hue / 360.0).rem_euclid(1.0);
        ss = (ss * (1.0 + s)).clamp(0.0, 1.0);
    }
    let mut rgb = hsl_to_rgb(hh, ss, ll);
    if l > 0.0 {
        rgb = rgb.map(|v| v + (1.0 - v) * l);
    } else if l < 0.0 {
        rgb = rgb.map(|v| v * (1.0 + l));
    }
    rgb
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

/// Photoshop Selective Color (approximation): each range shifts the cyan, magenta and yellow ink
/// (1 − R, G, B) by its percentage, plus its black percentage on every ink, weighted by how much
/// the colour belongs to the range. Relative mode scales the shift by the ink already present
/// (so it can't tint pure white); absolute adds it outright.
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
            let d = if relative { (a[i] + a[3]) * ink } else { a[i] + a[3] };
            delta[i] += d * wr;
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

/// Photoshop posterize: `n` equal input bins over 0..=255, output levels
/// `floor(k * 255 / (n - 1))` (exact on the corpus for 3, 7, 13 and 21 levels).
/// Input levels (0..255) of the measured modern Brightness knots: every 8 levels, denser near
/// white where the curves bend hardest.
const BRIGHT_X: [f32; 37] = [
    0.0, 8.0, 16.0, 24.0, 32.0, 40.0, 48.0, 56.0, 64.0, 72.0, 80.0, 88.0, 96.0, 104.0, 112.0, 120.0, 128.0, 136.0, 144.0, 152.0, 160.0, 168.0, 176.0,
    184.0, 192.0, 200.0, 208.0, 216.0, 224.0, 232.0, 240.0, 244.0, 248.0, 250.0, 252.0, 254.0, 255.0,
];
/// Slider values of the measured curves in [`BRIGHT_Y`].
const BRIGHT_B: [f32; 5] = [-150.0, -50.0, 50.0, 100.0, 150.0];
/// Photoshop's modern Brightness curves at [`BRIGHT_B`], output levels at [`BRIGHT_X`]. Measured
/// by pushing a 0..255 ramp through the real app (smoothed over ±2 levels).
const BRIGHT_Y: [[f32; 37]; 5] = [
    [
        0.0, 3.0, 6.1, 9.6, 12.5, 15.6, 18.4, 22.0, 24.6, 28.2, 30.8, 34.3, 37.1, 40.5, 43.8, 46.7, 49.6, 53.0, 56.1, 59.7, 62.8, 66.7, 71.0, 74.9, 79.4,
        84.5, 90.5, 97.0, 106.3, 116.0, 132.0, 143.0, 156.5, 164.0, 178.5, 214.0, 255.0,
    ],
    [
        0.0, 6.0, 11.6, 17.4, 23.4, 29.3, 35.3, 40.8, 46.3, 52.6, 58.4, 64.3, 70.2, 75.8, 81.7, 88.0, 93.3, 99.4, 105.0, 112.0, 118.2, 124.7, 132.8,
        140.3, 148.0, 155.8, 165.5, 175.0, 186.0, 198.0, 213.0, 219.0, 230.0, 234.5, 242.0, 252.5, 255.0,
    ],
    [
        0.0, 11.2, 21.8, 33.2, 43.6, 54.8, 66.2, 77.2, 87.6, 99.0, 109.0, 121.0, 131.4, 141.8, 152.0, 162.0, 171.0, 180.2, 187.8, 196.2, 203.2, 210.0,
        216.8, 223.0, 228.2, 233.2, 237.6, 242.0, 245.8, 249.0, 252.0, 252.0, 254.0, 254.0, 254.0, 255.0, 255.0,
    ],
    [
        0.0, 15.2, 29.6, 44.6, 60.8, 75.0, 90.0, 105.2, 119.6, 135.6, 148.8, 161.8, 172.4, 183.2, 193.2, 201.6, 208.6, 216.0, 221.6, 227.4, 232.2, 236.0,
        239.6, 242.8, 244.8, 247.2, 248.8, 250.2, 251.8, 253.0, 254.0, 254.0, 254.0, 254.0, 255.0, 255.0, 255.0,
    ],
    [
        0.0, 21.4, 41.2, 62.8, 81.0, 103.4, 124.4, 144.4, 162.2, 179.8, 192.4, 205.0, 214.0, 222.0, 229.4, 234.4, 238.6, 242.4, 244.4, 247.2, 248.8, 250.0,
        251.2, 252.0, 253.0, 253.0, 254.0, 254.0, 254.0, 255.0, 255.0, 255.0, 255.0, 255.0, 255.0, 255.0, 255.0,
    ],
];

/// Monotone cubic (Fritsch–Carlson PCHIP) through the knots `(xs, ys)` at `x`, clamped to the ends.
fn pchip(xs: &[f32], ys: &[f32], x: f32) -> f32 {
    let n = xs.len().min(ys.len());
    let (Some(&x0), Some(&xn), Some(&y0), Some(&yn)) = (xs.first(), xs.get(n.wrapping_sub(1)), ys.first(), ys.get(n.wrapping_sub(1))) else {
        return x;
    };
    if n < 2 || x <= x0 {
        return y0;
    }
    if x >= xn {
        return yn;
    }
    let i = xs.iter().take(n - 1).rposition(|&k| k <= x).unwrap_or(0);
    let h = |k: usize| xs.get(k + 1).zip(xs.get(k)).map_or(1.0, |(a, b)| a - b);
    let d = |k: usize| ys.get(k + 1).zip(ys.get(k)).map_or(0.0, |(a, b)| a - b) / h(k);
    let slope = |k: usize| {
        if k == 0 {
            return d(0);
        }
        if k >= n - 1 {
            return d(n - 2);
        }
        let (d0, d1) = (d(k - 1), d(k));
        if d0 * d1 <= 0.0 {
            return 0.0;
        }
        let (w1, w2) = (2.0 * h(k) + h(k - 1), h(k) + 2.0 * h(k - 1));
        (w1 + w2) / (w1 / d0 + w2 / d1)
    };
    let (hi, t) = (h(i), (x - xs.get(i).copied().unwrap_or(x0)) / h(i));
    let (t2, t3) = (t * t, t * t * t);
    let (ya, yb) = (ys.get(i).copied().unwrap_or(y0), ys.get(i + 1).copied().unwrap_or(yn));
    (2.0 * t3 - 3.0 * t2 + 1.0) * ya + (t3 - 2.0 * t2 + t) * hi * slope(i) + (-2.0 * t3 + 3.0 * t2) * yb + (t3 - t2) * hi * slope(i + 1)
}

/// Modern Brightness curve (one channel, `brightness` in [-150, 150]). Photoshop's curve is a
/// spline we don't have the formula for, so this interpolates curves measured from the real app
/// at -150, -50, 50, 100 and 150 (identity at 0), with a monotone cubic across the sliders.
/// Pins pure black and white; at most 2–5/255 off Photoshop at the measured sliders (the old fitted
/// formula was up to 53/255 off at -150), about 4/255 between them.
pub fn modern_brightness(v: f32, brightness: f32) -> f32 {
    if brightness == 0.0 || !brightness.is_finite() {
        return v;
    }
    let b = brightness.clamp(-150.0, 150.0);
    let x = v.clamp(0.0, 1.0) * 255.0;
    let curve = |k: usize| BRIGHT_Y.get(k).map_or(x, |ys| pchip(&BRIGHT_X, ys, x));
    // Output across the sliders at this level (identity at 0), then a monotone cubic in `b`: near
    // white, Photoshop's output rises much faster over the first slider steps than a linear blend
    // (b = 25 lands ~3/4 of the way to the b = 50 curve).
    let [n0, n1, p0, p1, p2] = BRIGHT_B;
    let y = pchip(&[n0, n1, 0.0, p0, p1, p2], &[curve(0), curve(1), x, curve(2), curve(3), curve(4)], b);
    (y / 255.0).clamp(0.0, 1.0)
}

/// The Brightness/Contrast transfer curve sampled at [`LUT_SIZE`] points: the CPU applies it
/// through [`lut`] and the GPU uploads it as a LUT row, so both draw the same curve.
pub fn brightness_contrast_lut(brightness: f32, contrast: f32, legacy: bool) -> Vec<f32> {
    let x = |k: usize| k as f32 / (LUT_SIZE - 1) as f32;
    if legacy {
        let f = legacy_brightness_contrast(brightness, contrast);
        (0..LUT_SIZE).map(|k| f(x(k)).clamp(0.0, 1.0)).collect()
    } else {
        // Blending the measured curves can dip by a few thousandths of a level where they flatten
        // near white; a running max keeps the curve monotone.
        let mut top = 0.0f32;
        (0..LUT_SIZE)
            .map(|k| {
                top = top.max(modern_brightness(x(k), brightness));
                modern_contrast(top, contrast).clamp(0.0, 1.0)
            })
            .collect()
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

/// Modern Contrast curve (one channel, `contrast` in [-50, 100]). A symmetric cubic-Hermite S-curve
/// pivoting at 0.5: endpoint slope `1 - c/128`, centre slope `1 + c/128`. Near-exact vs Photoshop
/// (≤0.5/255). `c = 0` is the identity; pins 0, 0.5 and 1.
pub fn modern_contrast(v: f32, contrast: f32) -> f32 {
    if contrast == 0.0 {
        return v;
    }
    let k = contrast / 128.0;
    let (end, mid) = (1.0 - k, 1.0 + k);
    // Cubic Hermite on [0, 0.5]: pinned (0,0) slope `end`, (0.5,0.5) slope `mid`. `t` in [0,1].
    let half = |t: f32| end * 0.5 * (t * t * t - 2.0 * t * t + t) + (-2.0 * t * t * t + 3.0 * t * t) * 0.5 + mid * 0.5 * (t * t * t - t * t);
    let v = v.clamp(0.0, 1.0);
    if v <= 0.5 { half(v / 0.5) } else { 1.0 - half((1.0 - v) / 0.5) }
}

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
    let g = ch.gamma.max(0.01);
    let t = if g > 1.0 {
        // A midtone gamma > 1 lifts shadows, and a pure `u^(1/g)` has infinite slope at black —
        // which Photoshop bounds, giving a soft shadow toe (initial slope ≈ 2^gamma, verified against
        // the real app). Soft-min (p-norm) of the power curve with that line; reduces to the exact
        // power curve in the body, so highlights are unchanged. gamma <= 1 is untouched. Slope and
        // sharpness fitted on whole-level input (`levels_q`): a 0..255 ramp at gamma 2.0 and
        // psd-tools levels_grayscale's gamma 1.78 (within one level of Photoshop on both).
        let power = u.powf(1.0 / g);
        let line = 0.93 * 2.0f32.powf(g) * u;
        if power <= 1e-6 || line <= 1e-6 {
            power.min(line)
        } else {
            let p = 10.0; // p-norm sharpness, fit to Photoshop ground truth
            (power.powf(-p) + line.powf(-p)).powf(-1.0 / p)
        }
    } else {
        u.powf(1.0 / g)
    };
    ch.out_black + t * (ch.out_white - ch.out_black)
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
