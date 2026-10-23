//! Evaluation of adjustment layers on a composite buffer.
//!
//! Formulas are documented approximations of Photoshop behaviour. Exact matching is tuned in
//! milestone M7 against Photoshop-rendered PSD composites (the oracle in `testkit`).

use photosplice_color::convert::{D50, SRGB_TO_XYZ_D50, XYZ_D50_TO_SRGB, lab_to_srgb, mat3_mul, rgb_to_gray, srgb_to_lab, srgb_to_linear};
use photosplice_color::{ColorMode, SampleType};
use photosplice_doc::Adjustment;
use photosplice_doc::adjust::{CurvePoint, HueRange, LevelsChannel, ToneSpace, lut3d_len};

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
    pub fn for_document(mode: photosplice_color::ColorMode, depth: photosplice_color::SampleType) -> Self {
        if depth == photosplice_color::SampleType::F32 {
            return Transfer::Gamma(1.0);
        }
        match mode {
            photosplice_color::ColorMode::Grayscale | photosplice_color::ColorMode::Duotone | photosplice_color::ColorMode::Bitmap => Transfer::Gamma(1.732),
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
    /// Tone-encoded value → linear light.
    pub fn decode(self, v: f32) -> f32 {
        match self {
            Transfer::Srgb => photosplice_color::convert::srgb_to_linear(v.max(0.0)),
            Transfer::Gamma(1.0) => v.max(0.0),
            Transfer::Gamma(g) => v.max(0.0).powf(g),
        }
    }
    /// Linear light → tone-encoded value.
    pub fn encode(self, v: f32) -> f32 {
        match self {
            Transfer::Srgb => photosplice_color::convert::linear_to_srgb(v.max(0.0)),
            Transfer::Gamma(1.0) => v.max(0.0),
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
        Adjustment::Threshold { level } => map_rgb(buf, |c| [if threshold_white(c, *level, depth) { 1.0 } else { 0.0 }; 3]),
        Adjustment::Posterize { levels } => map_rgb(buf, |c| c.map(|v| posterize_in(v, *levels, depth))),
        Adjustment::BrightnessContrast { brightness, contrast, legacy: true } => {
            if depth == Some(SampleType::F32) {
                // 32-bit samples are unbounded: no clipping, contrast pivots at 127 of 255.
                let f = legacy_brightness_contrast_hdr(*brightness, *contrast);
                map_rgb(buf, |px| px.map(&f));
            } else {
                let f = legacy_brightness_contrast(*brightness, *contrast);
                map_rgb(buf, |px| px.map(|v| f(v).clamp(0.0, 1.0)))
            }
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
            let hdr = depth == Some(SampleType::F32);
            map_rgb(buf, |c| {
                c.map(|v| {
                    if hdr {
                        // Float documents store linear samples. Photoshop's 32-bit merged
                        // Exposure oracle also retains negative results with signed gamma.
                        let lin = v * m + offset;
                        return if g == 1.0 { lin } else { lin.signum() * lin.abs().powf(1.0 / g) };
                    }
                    let lin = (transfer.decode(v) * m + offset).max(0.0);
                    let lin = if g == 1.0 { lin } else { lin.powf(1.0 / g) };
                    transfer.encode(lin).clamp(0.0, 1.0)
                })
            })
        }
        Adjustment::Levels { master, per_channel, space: ToneSpace::Rgb, .. } if depth == Some(SampleType::F32) => {
            // A 0..1 lookup table cannot represent signed, unbounded float samples.
            let master_identity = *master == LevelsChannel::default();
            let channel_identity = per_channel.each_ref().map(|ch| *ch == LevelsChannel::default());
            if !master_identity || channel_identity.iter().any(|&v| !v) {
                map_rgb(buf, |c| {
                    std::array::from_fn(|i| {
                        let v = if channel_identity[i] { c[i] } else { levels_float(&per_channel[i], c[i]) };
                        if master_identity { v } else { levels_float(master, v) }
                    })
                });
            }
        }
        Adjustment::Levels { space, .. } | Adjustment::Curves { space, .. } => {
            let luts = tone_luts_depth(adj, depth);
            let identity = float_tone_identity(adj, depth);
            match space {
                ToneSpace::Rgb => map_rgb(buf, |c| std::array::from_fn(|i| if identity[i] { c[i] } else { lut(&luts[i], c[i]) })),
                ToneSpace::Cmyk | ToneSpace::Lab => map_rgb(buf, |c| tone_in_space(*space, &luts, c)),
            }
        }
        Adjustment::HueSaturation { hue, saturation, lightness, colorize, ranges } => {
            let table = (!*colorize && ranges.iter().any(|r| !r.is_neutral())).then(|| hue_range_tables(ranges));
            map_rgb(buf, |c| {
                let (dh, ds, dl) = match &table {
                    Some(t) => {
                        // Range edits follow the pixel's original hue (on Reference's hue scale),
                        // in full however dull the colour (Reference's merged images, #4089).
                        // Greys stay grey: no hue to shift, no saturation to scale, and the range
                        // Lightness moves towards a channel they share.
                        let h0 = range_hue(rgb_to_hsl(c).0);
                        (lut(&t[0], h0), lut(&t[1], h0), lut(&t[2], h0))
                    }
                    None => (0.0, 0.0, 0.0),
                };
                if *colorize {
                    let s = (*saturation / 100.0).clamp(-1.0, 1.0);
                    return hue_saturation_in(c, *hue + dh, s, (*lightness / 100.0 + dl).clamp(-1.0, 1.0), true, depth == Some(SampleType::F32));
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
        Adjustment::ChannelMixer { matrix, monochrome, .. } if depth == Some(SampleType::F32) => {
            // 32-bit: each source percentage is cut to 1/1024 (toward zero), the constant counts
            // 255/512 of its value, and nothing clips (photoshop corpus rgb32 channel-mixer.psd:
            // pure red, green, blue, black and white all exact).
            let cut = |v: f32| (v * 1024.0).trunc() / 1024.0;
            let m = matrix.map(|row| [cut(row[0]), cut(row[1]), cut(row[2]), row[3] * 255.0 / 512.0]);
            map_rgb(buf, |c| {
                let mix = |row: &[f32; 4]| row[0] * c[0] + row[1] * c[1] + row[2] * c[2] + row[3];
                if *monochrome { [mix(&m[0]); 3] } else { [mix(&m[0]), mix(&m[1]), mix(&m[2])] }
            })
        }
        Adjustment::ChannelMixer { matrix, monochrome, .. } => map_rgb(buf, |c| {
            let mix = |row: &[f32; 4]| (row[0] * c[0] + row[1] * c[1] + row[2] * c[2] + row[3]).clamp(0.0, 1.0);
            if *monochrome { [mix(&matrix[0]); 3] } else { [mix(&matrix[0]), mix(&matrix[1]), mix(&matrix[2])] }
        }),
        Adjustment::PhotoFilter { color, density, preserve_luminosity } => {
            let linear_doc = depth == Some(SampleType::F32);
            let m = photo_filter_matrix(*color, *density, *preserve_luminosity && linear_doc, linear_doc);
            let set_lum = *preserve_luminosity && !linear_doc;
            map_rgb(buf, |c| {
                // 32-bit samples are linear already, and may go negative.
                let f = if linear_doc { mat3_mul(&m, c) } else { mat3_mul(&m, c.map(|v| transfer.decode(v))).map(|v| transfer.encode(v)) };
                let f = if set_lum { photosplice_color::blend::set_lum(f, photosplice_color::blend::lum(c)) } else { f };
                // 32-bit results aren't clipped (photoshop corpus rgb32 photo-filter.psd: white
                // filters to 1.19 red).
                if linear_doc { f } else { f.map(|v| v.clamp(0.0, 1.0)) }
            })
        }
        Adjustment::BlackWhite { weights, tint } => {
            let hdr = depth == Some(SampleType::F32);
            map_rgb(buf, |c| {
                let g = black_white_gray(c, weights);
                match tint {
                    // Photoshop tints like the Color blend mode: the tint's hue and saturation at
                    // the grey's luminosity (SetLum + ClipColor), measured in Photoshop 27.11. In
                    // 32-bit documents nothing clips at 1, as with the blend mode there (photoshop
                    // corpus rgb32 black-white-tint.psd: white tints to 1.05, 1.00, 0.87).
                    Some(t) if hdr => photosplice_color::blend::blend_rgb_within(photosplice_color::blend::BlendMode::Color, [g; 3], *t, f32::MAX),
                    Some(t) => photosplice_color::blend::set_lum(*t, g).map(|v| v.clamp(0.0, 1.0)),
                    None => [g; 3],
                }
            })
        }
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

/// Legacy Brightness/Contrast on a 32-bit document's samples: unclipped, with contrast pivoting at
/// 127 of 255 (photoshop corpus rgb32 and gray32 brightness-contrast-legacy.psd: every pixel
/// exact, negative and over-white values included; the 8/16-bit pivot, 128, is 0.43 of a level
/// off there).
fn legacy_brightness_contrast_hdr(brightness: f32, contrast: f32) -> impl Fn(f32) -> f32 {
    const PIVOT: f32 = 127.0 / 255.0;
    let b = brightness / 255.0;
    let c = contrast.clamp(-100.0, 99.0);
    let k = if c >= 0.0 { 1.0 / (1.0 - c / 100.0) } else { 1.0 + c / 100.0 };
    let (pre, post) = if c >= 0.0 { (b, 0.0) } else { (0.0, b) };
    move |v: f32| (v + pre - PIVOT) * k + PIVOT + post
}

/// Legacy Brightness/Contrast as one channel's tone curve (see [`apply_depth`]).
fn legacy_brightness_contrast(brightness: f32, contrast: f32) -> impl Fn(f32) -> f32 {
    legacy_brightness_contrast_at(brightness, contrast, LEGACY_PIVOT)
}

/// [`legacy_brightness_contrast`] with contrast pivoting at `pivot`.
fn legacy_brightness_contrast_at(brightness: f32, contrast: f32, pivot: f32) -> impl Fn(f32) -> f32 {
    let b = brightness / 255.0;
    let c = contrast.clamp(-100.0, 99.0);
    let k = if c >= 0.0 { 1.0 / (1.0 - c / 100.0) } else { 1.0 + c / 100.0 };
    let (pre, post) = if c >= 0.0 { (b, 0.0) } else { (0.0, b) };
    move |v: f32| ((v + pre - pivot) * k + pivot + post).clamp(0.0, 1.0)
}

/// Whether Threshold at `level` turns colour `c` white in a document of `depth` (`None`: unknown,
/// treated as 8-bit). Photoshop splits on the luminosity 0.30 R + 0.59 G + 0.11 B (the blend
/// modes' `lum`), not the Rec. 601 grey, of the document's whole sample values:
/// - 8 bits: white when that luminosity, rounded to a level with halves down, reaches the level;
/// - 16 bits: white when it reaches the level converted to 16 bits (128 → 16448);
/// - 32 bits: white when it reaches the level.
///
/// Exact on every pixel of psd-tools threshold_rgb.psd, threshold_16bits_rgb.psd and
/// threshold_16bits_grayscale.psd and of the photoshop corpus's rgb16 threshold-128.psd, where the
/// Rec. 601 grey split 28 and 100 pixels of the first two the other way. The sums of
/// `30 R + 59 G + 11 B` over whole samples are exact in f32 (at most 3 276 800).
pub fn threshold_white(c: [f32; 3], level: f32, depth: Option<SampleType>) -> bool {
    let t = (level * 255.0).round();
    let sum = |q: f32| {
        let [r, g, b] = c.map(|v| (v.clamp(0.0, 1.0) * q).round());
        30.0 * r + 59.0 * g + 11.0 * b
    };
    match depth {
        None | Some(SampleType::U8) => sum(255.0) > 100.0 * t - 50.0,
        Some(SampleType::U16) => sum(32768.0) >= 100.0 * (t * 32768.0 / 255.0).round(),
        Some(SampleType::F32) => (0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]) * 255.0 >= t,
    }
}

/// Applies an adjustment in a document of colour `mode`: [`apply_depth`], except that in Lab
/// and CMYK documents the adjustments Photoshop evaluates on the document's own channels do so
/// (see [`lab_channel_adjustment`] and [`cmyk_channel_adjustment`]).
pub fn apply_in(adj: &Adjustment, buf: &mut Buffer, transfer: Transfer, depth: Option<SampleType>, mode: ColorMode) {
    if mode == ColorMode::Lab && lab_channel_adjustment(adj) {
        apply_lab(adj, buf, depth);
    } else if mode == ColorMode::Cmyk && cmyk_channel_adjustment(adj) {
        use photosplice_color::convert::{cmyk_to_rgb, rgb_to_cmyk};
        if let Adjustment::BrightnessContrast { brightness, contrast, legacy } = adj {
            let table = ink_brightness_contrast_lut(*brightness, *contrast, *legacy);
            map_rgb(buf, |c| cmyk_to_rgb(rgb_to_cmyk(c).map(|v| ink_through(&table, v))));
        } else if cmy_adjustment(adj) {
            let mut inks: Vec<[f32; 4]> = buf.px.iter().map(|p| rgb_to_cmyk([p[0], p[1], p[2]])).collect();
            apply_cmy(adj, &mut inks);
            for (p, ink) in buf.px.iter_mut().zip(&inks) {
                if p[3] > 0.0 {
                    let [r, g, b] = cmyk_to_rgb(*ink);
                    (p[0], p[1], p[2]) = (r, g, b);
                }
            }
        } else {
            map_rgb(buf, |c| {
                let mut ink = rgb_to_cmyk(c);
                stored_channels(adj, mode, depth, &mut ink);
                cmyk_to_rgb(ink)
            });
        }
    } else {
        apply_depth(adj, buf, transfer, depth);
    }
}

/// Whether a Lab document evaluates `adj` on its L, a and b channels rather than on the screen
/// colour: Invert and Posterize (L, a and b alike), Brightness/Contrast (L only), Threshold
/// (split on L, neutral result) and Gradient Map (driven by L). Levels and Curves carry their own
/// Lab records ([`ToneSpace::Lab`]); everything else stays on the screen colour.
pub fn lab_channel_adjustment(adj: &Adjustment) -> bool {
    matches!(
        adj,
        Adjustment::Invert
            | Adjustment::Posterize { .. }
            | Adjustment::Threshold { .. }
            | Adjustment::BrightnessContrast { .. }
            | Adjustment::GradientMap { .. }
    )
}

/// Whether a CMYK document evaluates `adj` on its inks rather than on the screen colour: Invert
/// and Posterize (each ink, black included, so paper inverts to 400% ink), Threshold (paper or
/// 100% black ink alone) and Brightness/Contrast (each ink's stored value, 1 - ink, through the
/// tone curve; see [`ink_through`]). Photoshop corpus cmyk8 and cmyk16 invert, posterize-4,
/// threshold-128 and brightness-contrast(-legacy) follow these rules on the files' own inks on
/// every pixel; here the inks are the screen colour separated through the document's CMYK
/// profile, so neutrals carry the black that profile generates rather than the file's.
pub fn cmyk_channel_adjustment(adj: &Adjustment) -> bool {
    stored_channel_adjustment(adj) || cmy_adjustment(adj) || matches!(adj, Adjustment::BrightnessContrast { .. })
}

/// Whether a CMYK document evaluates `adj` on C, M and Y as the complement of an RGB colour,
/// black kept ([`apply_cmy`]): Hue/Saturation (not Colorize, which Photoshop separates again,
/// black and all) and Color Balance (photoshop corpus cmyk16 hue-saturation.psd follows this on
/// every pixel within half a level, cmyk8 within two; color-balance within our RGB Color
/// Balance's own error).
pub fn cmy_adjustment(adj: &Adjustment) -> bool {
    matches!(adj, Adjustment::HueSaturation { colorize: false, .. } | Adjustment::ColorBalance { .. })
}

/// A [`cmy_adjustment`] on straight inks (C, M, Y, K; 0 none): the RGB adjustment on
/// `(1 - c, 1 - m, 1 - y)`, black left alone.
pub fn apply_cmy(adj: &Adjustment, inks: &mut [[f32; 4]]) {
    let width = i32::try_from(inks.len()).unwrap_or(i32::MAX);
    let mut buf = Buffer { rect: crate::Rect::new(0, 0, width, 1), px: inks.iter().map(|i| [1.0 - i[0], 1.0 - i[1], 1.0 - i[2], 1.0]).collect() };
    apply_depth(adj, &mut buf, Transfer::Srgb, None);
    for (ink, p) in inks.iter_mut().zip(&buf.px) {
        for c in 0..3 {
            ink[c] = (1.0 - p[c]).clamp(0.0, 1.0);
        }
    }
}

/// Brightness/Contrast as the lookup table (4096 samples over 0..1) a CMYK ink's stored value,
/// `1 - ink`, goes through ([`ink_through`]): [`modern_lut`], or the legacy linear curve with its
/// contrast pivoting at 128 in ink, which is 127 of 255 stored (photoshop corpus cmyk16
/// brightness-contrast-legacy.psd sits 0.43 of a level off around 128 stored, as a one-level
/// pivot shift times contrast +30's slope predicts).
pub fn ink_brightness_contrast_lut(brightness: f32, contrast: f32, legacy: bool) -> Vec<f32> {
    if !legacy {
        return modern_lut(brightness, contrast);
    }
    let curve = legacy_brightness_contrast_at(brightness, contrast, 1.0 - LEGACY_PIVOT);
    (0..LUT_SIZE).map(|k| curve(k as f32 / (LUT_SIZE - 1) as f32)).collect()
}

/// One CMYK ink (0: none) through a channel tone curve the way Photoshop applies one in CMYK: to
/// the stored value, `1 - ink`, so raising brightness takes ink away.
pub fn ink_through(table: &[f32], ink: f32) -> f32 {
    1.0 - lut(table, 1.0 - ink)
}

/// Whether [`stored_channels`] evaluates `adj`: Invert, Posterize and Threshold.
pub fn stored_channel_adjustment(adj: &Adjustment) -> bool {
    matches!(adj, Adjustment::Invert | Adjustment::Posterize { .. } | Adjustment::Threshold { .. })
}

/// Invert, Posterize or Threshold on one pixel's own channels in a Lab or CMYK document of
/// `depth` (`None`: unknown, treated as 8-bit): `ch` holds L, a, b normalised as [`lab_norm`]
/// gives them, or the C, M, Y, K inks (0: none). Lab Invert turns 8-bit bytes `v` into `255 - v`
/// (a becomes -1 - a) and mirrors a and b around neutral at 16 bits (photoshop corpus lab16
/// invert.psd: neutral stays neutral). CMYK Threshold compares [`ink_luma`].
pub fn stored_channels(adj: &Adjustment, mode: ColorMode, depth: Option<SampleType>, ch: &mut [f32]) {
    let lab = mode == ColorMode::Lab;
    match adj {
        Adjustment::Invert => {
            let mirror = if lab && depth.is_some_and(|d| d != SampleType::U8) { 256.0 / 255.0 } else { 1.0 };
            for (i, v) in ch.iter_mut().enumerate() {
                *v = if i > 0 { mirror } else { 1.0 } - v.clamp(0.0, 1.0);
            }
        }
        // CMYK posterizes the stored values (1 - ink), which its uneven levels make differ from
        // posterizing the ink: psd-tools posterize_cmyk.psd (3 to 21 levels) is exact so.
        Adjustment::Posterize { levels } if mode == ColorMode::Cmyk => ch.iter_mut().for_each(|v| *v = 1.0 - posterize_in(1.0 - *v, *levels, depth)),
        Adjustment::Posterize { levels } => ch.iter_mut().for_each(|v| *v = posterize_in(*v, *levels, depth)),
        Adjustment::Threshold { level } => {
            // On the 8-bit grid, like the RGB threshold.
            let t = (level * 255.0).round();
            let out = if lab {
                let white = ch.first().is_some_and(|l| (l.clamp(0.0, 1.0) * 255.0).round() >= t);
                [if white { 1.0 } else { 0.0 }, 128.0 / 255.0, 128.0 / 255.0, 0.0]
            } else {
                let ink = |i: usize| ch.get(i).copied().unwrap_or(0.0);
                let black = (ink_luma([ink(0), ink(1), ink(2), ink(3)]) * 255.0).round() < t;
                [0.0, 0.0, 0.0, if black { 1.0 } else { 0.0 }]
            };
            ch.iter_mut().zip(out).for_each(|(v, o)| *v = o);
        }
        _ => {}
    }
}

/// The luma Photoshop's CMYK Threshold compares: Rec. 601 weights on the inks' plain complement,
/// `(1 - c)(1 - k)` and so on, whatever the document's profile (photoshop corpus cmyk8 and cmyk16
/// threshold-128.psd: every pixel on the right side of level 128).
pub fn ink_luma(ink: [f32; 4]) -> f32 {
    let paper = 1.0 - ink[3].clamp(0.0, 1.0);
    rgb_to_gray([0, 1, 2].map(|i| (1.0 - ink[i].clamp(0.0, 1.0)) * paper))
}

/// Encoded sRGB → Lab normalised the way Photoshop's channels hold it: L/100, (a+128)/255,
/// (b+128)/255.
fn lab_norm(c: [f32; 3]) -> [f32; 3] {
    let l = srgb_to_lab(c);
    [l[0] / 100.0, (l[1] + 128.0) / 255.0, (l[2] + 128.0) / 255.0]
}

/// The inverse of [`lab_norm`].
fn lab_denorm(n: [f32; 3]) -> [f32; 3] {
    lab_to_srgb([n[0] * 100.0, n[1] * 255.0 - 128.0, n[2] * 255.0 - 128.0])
}

/// Neutral a or b as Lab surfaces store it ((0 + 128) / 255).
pub const LAB_NEUTRAL: f32 = 128.0 / 255.0;

/// An adjustment Photoshop evaluates on a Lab document's own channels, as an operation on one
/// pixel's stored L, a, b (normalised as Lab surfaces store them: L / 100, (a + 128) / 255,
/// (b + 128) / 255) in a document of `depth`: Invert and Posterize (each channel), Threshold
/// (split on L, neutral result), Brightness/Contrast (L) and Levels or Curves with Lab records.
/// `None` for any other adjustment. On the stored values colours outside the display gamut keep
/// theirs, where [`apply_in`] can only round-trip them through clipped display RGB.
pub fn lab_stored_op(adj: &Adjustment, depth: Option<SampleType>) -> Option<LabOp<'_>> {
    let op: LabOp<'_> = match adj {
        Adjustment::Invert | Adjustment::Posterize { .. } => Box::new(move |ch: &mut [f32; 3]| stored_channels(adj, ColorMode::Lab, depth, ch)),
        Adjustment::Threshold { level } => {
            // Split on L on the 8-bit grid, like the display path (`apply_lab`).
            let t = (level * 255.0).round();
            Box::new(move |ch: &mut [f32; 3]| {
                let white = (ch[0].clamp(0.0, 1.0) * 255.0).round() >= t;
                *ch = [if white { 1.0 } else { 0.0 }, LAB_NEUTRAL, LAB_NEUTRAL];
            })
        }
        Adjustment::BrightnessContrast { brightness, contrast, legacy } => {
            let (b, c, legacy) = (*brightness, *contrast, *legacy);
            let legacy_curve = legacy_brightness_contrast(b, c);
            Box::new(move |ch: &mut [f32; 3]| {
                ch[0] = if legacy { legacy_curve(ch[0]) } else { modern_contrast(modern_brightness(ch[0], b), c).clamp(0.0, 1.0) };
            })
        }
        Adjustment::Levels { space: ToneSpace::Lab, .. } | Adjustment::Curves { space: ToneSpace::Lab, .. } => {
            let luts = tone_luts_depth(adj, depth);
            Box::new(move |ch: &mut [f32; 3]| {
                for (v, t) in ch.iter_mut().zip(&luts) {
                    *v = lut(t, *v);
                }
            })
        }
        _ => return None,
    };
    Some(op)
}

/// One pixel's stored L, a, b through an adjustment ([`lab_stored_op`]).
pub type LabOp<'a> = Box<dyn Fn(&mut [f32; 3]) + Send + Sync + 'a>;

/// The Lab evaluation of a [`lab_channel_adjustment`] in a document of `depth` (no-op for any
/// other adjustment).
fn apply_lab(adj: &Adjustment, buf: &mut Buffer, depth: Option<SampleType>) {
    match adj {
        // L → 100 - L and a, b mirrored about neutral; each channel posterized (see `stored_channels`).
        Adjustment::Invert | Adjustment::Posterize { .. } => map_rgb(buf, |c| {
            let mut n = lab_norm(c);
            stored_channels(adj, ColorMode::Lab, depth, &mut n);
            lab_denorm(n)
        }),
        Adjustment::Threshold { level } => {
            // Split on L alone (on the 8-bit grid); the result is neutral black or white.
            let t = (level * 255.0).round();
            map_rgb(buf, |c| [if (lab_norm(c)[0].clamp(0.0, 1.0) * 255.0).round() >= t { 1.0 } else { 0.0 }; 3])
        }
        Adjustment::BrightnessContrast { brightness, contrast, legacy } => {
            let legacy_curve = legacy_brightness_contrast(*brightness, *contrast);
            let modern = (!*legacy).then(|| modern_lut(*brightness, *contrast));
            let curve = |v: f32| match &modern {
                Some(table) => lut(table, v),
                None => legacy_curve(v),
            };
            // The tone curve on L only. Added as the difference of two round trips (like
            // `tone_in_space`) so a and b, and colours a round trip moves, stay put.
            map_rgb(buf, |c| {
                let n = lab_norm(c);
                let l = curve(n[0]);
                if l == n[0] {
                    return c;
                }
                let (before, after) = (lab_denorm(n), lab_denorm([l, n[1], n[2]]));
                std::array::from_fn(|i| (c[i] + after[i] - before[i]).clamp(0.0, 1.0))
            })
        }
        Adjustment::GradientMap { stops, reverse, dither, .. } => {
            let (w, x0, y0) = (buf.rect.width().max(1) as usize, buf.rect.x0, buf.rect.y0);
            let stops = &by_location(stops);
            for (i, p) in buf.px.iter_mut().enumerate() {
                if p[3] <= 0.0 {
                    continue;
                }
                // Driven by L: equal lightness maps to the same stop colour whatever the chroma.
                let l = lab_norm([p[0], p[1], p[2]])[0].clamp(0.0, 1.0);
                let mut o = gradient(stops, if *reverse { 1.0 - l } else { l });
                if *dither {
                    let d = bayer4(x0 + (i % w) as i32, y0 + (i / w) as i32) / 255.0;
                    o = o.map(|v| (v + d).clamp(0.0, 1.0));
                }
                p[..3].copy_from_slice(&o);
            }
        }
        _ => {}
    }
}

/// Float RGB curve channels with no edit must preserve samples outside the LUT domain.
/// Edited curves retain the existing spline/LUT behaviour until HDR extrapolation has an oracle.
pub fn float_tone_identity(adj: &Adjustment, depth: Option<SampleType>) -> [bool; 3] {
    match adj {
        Adjustment::Curves { master, per_channel, space: ToneSpace::Rgb, .. } if depth == Some(SampleType::F32) => {
            std::array::from_fn(|i| photosplice_doc::adjust::is_identity_curve(master) && photosplice_doc::adjust::is_identity_curve(&per_channel[i]))
        }
        _ => [false; 3],
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
        Adjustment::Curves { master, per_channel, space, black, pencil } => {
            let m = if *space == ToneSpace::Lab { curve_lut(&[]) } else { curve_lut_mode(master, *pencil) };
            let row = |c: &[CurvePoint]| curve_lut_mode(c, *pencil).iter().map(|&v| lut(&m, v)).collect();
            [row(&per_channel[0]), row(&per_channel[1]), row(&per_channel[2]), row(if *space == ToneSpace::Cmyk { black } else { &[] })]
        }
        _ => std::array::from_fn(|_| (0..LUT_SIZE).map(x).collect()),
    }
}

/// [`tone_luts_q`] in a document of `depth` (`None`: unrounded). 32-bit Levels use
/// [`levels_float`], sampled on 0..1 and kept in 0..1 for LUT consumers. Float RGB rendering
/// evaluates the formula directly instead, so samples and results outside that domain survive.
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
    use photosplice_color::convert::{cmyk_to_rgb, rgb_to_cmyk};
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
/// Colorize sets the hue and saturation instead of shifting them; at 0% it leaves only the
/// lightness, as in Photoshop (#2326). Ticking Colorize in the dialog starts saturation at 25%.
pub fn hue_saturation(c: [f32; 3], hue: f32, s: f32, l: f32, colorize: bool) -> [f32; 3] {
    hue_saturation_in(c, hue, s, l, colorize, false)
}

/// [`hue_saturation`] in an 8/16-bit or a 32-bit (`float`) document.
pub fn hue_saturation_in(c: [f32; 3], hue: f32, s: f32, l: f32, colorize: bool, float: bool) -> [f32; 3] {
    if colorize {
        // Colorize: the hue and saturation as set, on the pixel's HSL lightness moved by the
        // Lightness slider (Photoshop corpus hue-saturation-colorize.psd, rgb16: within 0.5/255).
        let (_, _, l0) = rgb_to_hsl(c);
        let ll = if l > 0.0 { l0 + (1.0 - l0) * l } else { l0 * (1.0 + l) };
        let (h, s) = (hue.rem_euclid(360.0) / 360.0, s.abs().min(1.0));
        if float && l > 0.0 {
            // 32-bit: the chroma also shrinks by 1 − l about the moved lightness (Reference
            // corpus rgb32 hue-saturation-colorize.psd at hue 200, 40 %, +5: grey 0.4735 → 0.310
            // 0.563 0.690, red → 0.345 0.585 0.706).
            return hsl_to_rgb(h, s, ll).map(|v| (ll + (v - ll) * (1.0 - l)).clamp(0.0, 1.0));
        }
        return hsl_to_rgb(h, s, ll).map(|v| v.clamp(0.0, 1.0));
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
    let mut rgb = hue_saturation_scale(rgb, s, l, float, ranges);
    if l > 0.0 {
        rgb = rgb.map(|v| v + (1.0 - v) * l);
    } else if l < 0.0 {
        rgb = rgb.map(|v| v * (1.0 + l));
    }
    if float { rgb } else { rgb.map(|v| v.clamp(0.0, 1.0)) }
}

thread_local! {
    /// Set while a CMYK document runs a colour adjustment on its inks' complements (see
    /// `cmyk::adjust`): what Reference holds there is the ink, not the screen colour.
    static INK_SPACE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `f` with [`INK_SPACE`] set, then restores the value it had (also on unwind).
pub fn in_ink_space<R>(f: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            INK_SPACE.with(|l| l.set(self.0));
        }
    }
    let _restore = Restore(INK_SPACE.with(|l| l.replace(true)));
    f()
}

/// Hue/Saturation's saturation step on `rgb` (see [`hue_saturation`]). 32-bit documents don't
/// hold the smallest channel at 0 nor clip: channels leave 0..1 as Reference's float composite
/// keeps them (corpus rgb32 hue-saturation.psd: (1, 0, 0) at +25 % ends at (1.048, …, −0.148)).
fn hue_saturation_scale(rgb: [f32; 3], s: f32, l: f32, float: bool, hold_white: bool) -> [f32; 3] {
    let (max, min) = (rgb[0].max(rgb[1]).max(rgb[2]), rgb[0].min(rgb[1]).min(rgb[2]));
    let mid = (max + min) / 2.0;
    let mut k = if s > 0.0 { 1.0 / (1.0 - s).max(1e-6) } else { 1.0 + s };
    if s > 0.0 && INK_SPACE.with(|i| i.get()) {
        // On ink complements no ink leaves 0..100 % once the lightness step `l` has applied:
        // the scale stops where the first complement would end up past 0 or 1 (Reference kit
        // inks-hs-sat50: (78, 18, 58, 6) % → (96, 0, 64, 6) held at the smallest ink,
        // (22, 20, 91, 0) → (13.5, 11, 100, 0) at the largest; inks-hs-master, lightness −10,
        // exact on all 192 patches with the limits taken through it).
        let (lo, hi) = if l < 0.0 { (0.0, 1.0 / (1.0 + l).max(1e-3)) } else { (-l / (1.0 - l).max(1e-3), 1.0) };
        if max > mid {
            k = k.min(((hi - mid) / (max - mid)).max(1.0));
        }
        if min < mid {
            k = k.min(((mid - lo) / (mid - min)).max(1.0));
        }
        return rgb.map(|v| mid + (v - mid) * k);
    }
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

/// A colour range's Lightness, applied after the master edits. It moves each channel towards the
/// pixel's brightest channel when raised and towards its darkest when lowered, not towards white
/// or black: Blues +50 turns (0, 29, 115) into (57, 71, 115), Blues -50 turns (102, 128, 204) into
/// (102, 115, 153) (measured in Photoshop 27.11).
pub fn range_lightness(c: [f32; 3], l: f32) -> [f32; 3] {
    let l = l.clamp(-1.0, 1.0);
    let target = if l > 0.0 { c[0].max(c[1]).max(c[2]) } else { c[0].min(c[1]).min(c[2]) };
    c.map(|v| v + (target - v) * l.abs())
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
    // A degenerate or short table can't be sampled: leave the colour alone.
    if n < 2 || n.checked_pow(3).and_then(|c| c.checked_mul(3)).is_none_or(|len| lut.len() < len) {
        return c;
    }
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

/// Color Balance on channel `ch` (0 red, 1 green, 2 blue) of `v` (0..1); `tones` are the
/// shadows, midtones and highlights slider triples (cyan–red, magenta–green, yellow–blue,
/// −100..100).
///
/// Reverse-engineered from Photoshop (a 16-bit gray ramp through every tone at ±10..±100, pairs of
/// tones, and random settings of all nine sliders, Preserve Luminosity on and off): the three
/// tones fold into one Levels per channel (input black point, input white point, midtone gamma),
/// within 0.8 of 255 everywhere. Folding matters: a lowered shadows and a raised highlights
/// slider cancel each other's gamma, which applying the tones one after another doesn't (#3423).
/// - Preserve Luminosity off: a slider only moves its own channel. Lowered shadows set the black
///   point to |shadows| levels, raised highlights set the white point to 255 − highlights levels,
///   and the gamma is 2^(m / 100) with m = midtones + (shadows + highlights) / 2.
/// - Preserve Luminosity on: each channel's black point is (max − own) shadows levels, its white
///   point 255 − (own − min) highlights levels, and m = own − (max + min) / 2 midtones.
///
/// A gamma > 1 starts with slope 2^gamma at black ([`balance_gamma`]: up to 18 of 255 brighter
/// near black without it). When the black point reaches the white point, input
/// at or above the white point is white and the rest black.
pub fn color_balance_channel(tones: [[f32; 3]; 3], preserve_luminosity: bool, ch: usize, v: f32) -> f32 {
    let [shadows, midtones, highlights] = tones.map(|t| t.map(|s| if s.is_finite() { s.clamp(-100.0, 100.0) } else { 0.0 }));
    let own = |t: [f32; 3]| t.get(ch).copied().unwrap_or(0.0);
    let max = |t: [f32; 3]| t[0].max(t[1]).max(t[2]);
    let min = |t: [f32; 3]| t[0].min(t[1]).min(t[2]);
    let (black, white, m) = if preserve_luminosity {
        (max(shadows) - own(shadows), own(highlights) - min(highlights), own(midtones) - (max(midtones) + min(midtones)) / 2.0)
    } else {
        ((-own(shadows)).max(0.0), own(highlights).max(0.0), own(midtones) + (own(shadows) + own(highlights)) / 2.0)
    };
    let (b, w) = (black / 255.0, 1.0 - white / 255.0);
    let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    if v >= w {
        return 1.0;
    }
    if v <= b {
        return 0.0;
    }
    balance_gamma((v - b) / (w - b), 2f32.powf(m / 100.0))
}

/// The midtone gamma of Color Balance's per-channel Levels (`u` in 0..1): `u^(1/gamma)`, except
/// that a gamma > 1 (which would start with infinite slope) starts at slope 2^gamma and joins the
/// power curve in a cubic Hermite at twice the input where that line meets it (Photoshop,
/// measured on a 16-bit ramp at gamma 1.07..2: within 0.02 of 255).
fn balance_gamma(u: f32, gamma: f32) -> f32 {
    let e = 1.0 / gamma.max(0.01);
    let power = u.powf(e);
    if gamma <= 1.0 {
        return power;
    }
    let s0 = 2f32.powf(gamma);
    let u0 = (2.0 * s0.powf(1.0 / (e - 1.0))).min(1.0);
    if u0.is_nan() || u0 <= 0.0 || u >= u0 {
        return power;
    }
    let (p, d, t) = (u0.powf(e), e * u0.powf(e - 1.0), u / u0);
    let (t2, t3) = (t * t, t * t * t);
    ((t3 - 2.0 * t2 + t) * u0 * s0 + (3.0 * t2 - 2.0 * t3) * p + (t3 - t2) * u0 * d).clamp(0.0, 1.0)
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
/// by its percentage, plus its black percentage, and the colour moves by how much it belongs to
/// the range. Relative mode moves the ink to a target, clamped to 0..100 %: the ink scaled by its
/// percentage, then by the black percentage (`ink · (1 + a) · (1 + k)`, so it can't tint pure
/// white; Photoshop corpus selective-color.psd, rgb16: within 1/255, was 11/255). Absolute mode
/// scales the shift by the room the ink has: adding ink by min(1, 2 × (1 − ink)), removing it by
/// min(1, 2 × ink), so Neutrals Cyan +50 darkens red 64 by 32, not 64 (all 72 absolute range ×
/// ink cases within 1 level of Photoshop 27.11).
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
            if relative {
                let target = ink * (1.0 + a[i]) * (1.0 + a[3]);
                delta[i] += (target.clamp(0.0, 1.0) - ink) * wr;
            } else {
                let d = a[i] + a[3];
                let d = if d > 0.0 { d * (2.0 * (1.0 - ink)).min(1.0) } else { d * (2.0 * ink).min(1.0) };
                delta[i] += d * wr;
            }
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

/// Linear sRGB → its own D65 XYZ (IEC 61966-2-1, no chromatic adaptation).
const SRGB_TO_XYZ_D65: [[f32; 3]; 3] = [[0.412_390_8, 0.357_584_3, 0.180_480_8], [0.212_639, 0.715_168_7, 0.072_192_3], [0.019_330_8, 0.119_194_8, 0.950_532_2]];
/// The inverse of [`SRGB_TO_XYZ_D65`].
const XYZ_D65_TO_SRGB: [[f32; 3]; 3] = [[3.240_97, -1.537_383_2, -0.498_610_8], [-0.969_243_6, 1.875_967_5, 0.041_555_1], [0.055_630_1, -0.203_977, 1.056_971_5]];

/// Photo Filter's linear-light RGB matrix. Reference multiplies the pixel's XYZ, relative to
/// white, by the filter colour's D50 XYZ relative to D50 (mixed in by `density`): a fixed linear
/// map, so it is folded into one matrix. 8/16-bit documents scale D50 XYZ; 32-bit ones (`hdr`)
/// scale sRGB's own D65 XYZ (no chromatic adaptation), and with `normalize_y` keep luminance Y
/// (32-bit Preserve Luminosity; 8/16-bit documents instead restore the encoded luminosity like
/// the Luminosity blend mode). Fitted on the Reference corpus `photo-filter.psd`: rgb16 within
/// 0.6/255 everywhere; rgb32 the whole matrix within 0.0001 of the one its pixels give.
pub fn photo_filter_matrix(color: [f32; 3], density: f32, normalize_y: bool, hdr: bool) -> [[f32; 3]; 3] {
    let d = if density.is_finite() { density.clamp(0.0, 1.0) } else { 0.0 };
    let xyz = mat3_mul(&SRGB_TO_XYZ_D50, color.map(|v| srgb_to_linear(if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 })));
    let mut s: [f32; 3] = std::array::from_fn(|i| 1.0 - d + d * xyz[i] / D50[i]);
    if normalize_y && s[1] > 1e-6 {
        let y = s[1];
        s = s.map(|v| v / y);
    }
    let (to, from) = if hdr { (&SRGB_TO_XYZ_D65, &XYZ_D65_TO_SRGB) } else { (&SRGB_TO_XYZ_D50, &XYZ_D50_TO_SRGB) };
    // from · diag(s) · to
    std::array::from_fn(|r| std::array::from_fn(|c| (0..3).map(|k| from[r][k] * s[k] * to[k][c]).sum()))
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

/// Modern Brightness/Contrast as one channel's lookup table ([`LUT_SIZE`] samples over 0..1):
/// [`modern_brightness`] then [`modern_contrast`], as Photoshop applies them (the same table as
/// [`brightness_contrast_lut`]).
pub fn modern_lut(brightness: f32, contrast: f32) -> Vec<f32> {
    brightness_contrast_lut(brightness, contrast, false)
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

/// [`posterize`] in a document of `depth` (`None`: unknown, treated as 8-bit). At 16 bits
/// Photoshop bins the 16-bit sample (`⌊v · n / 32768⌋`) and lands on 8-bit levels through a
/// 16-bit step: `⌊b · 32768 / (n - 1)⌋`, converted with `⌊(o · 255 + 16384) / 32768⌋`, so
/// 127.5 comes out as 128 but 42.5 as 42 (8 bits floor them all). Exact on every sample of
/// psd-tools posterize_16bits_rgb.psd and posterize_16bits_grayscale.psd (levels 2 to 21),
/// which the 8-bit rule missed by up to half the range.
pub fn posterize_in(v: f32, levels: u32, depth: Option<SampleType>) -> f32 {
    if depth != Some(SampleType::U16) {
        return posterize(v, levels);
    }
    let n = f64::from(levels.clamp(2, 255));
    let x = (f64::from(v.clamp(0.0, 1.0)) * 32768.0).round();
    let bin = (x * n / 32768.0).floor().min(n - 1.0);
    let step = (bin * 32768.0 / (n - 1.0)).floor();
    (((step * 255.0 + 16384.0) / 32768.0).floor() / 255.0) as f32
}

pub fn levels(ch: &LevelsChannel, v: f32) -> f32 {
    levels_q(ch, v, None)
}

/// [`levels`] on samples with `quantum` steps per unit (8-bit: 255). Photoshop works on whole
/// levels in three steps, each rounded half up to a whole level: the input-range stretch, the
/// midtone gamma ([`midtone_gamma`]) and the output range. So a 44..214 range maps 45, 46, 47, 48
/// → 1.5, 3, 4.5, 6 → 2, 3, 5, 6 (psd-tools levels_grayscale, where the gamma toe magnifies the
/// steps), and an output range narrower than 0..255 maps neighbouring gamma levels to one level
/// (psd-tools levels_rgb: 43..48 → 58 through a 0.47 channel gamma and the master's 48..255).
/// In a LUT the result is a staircase that is exact at every level.
pub fn levels_q(ch: &LevelsChannel, v: f32, quantum: Option<f32>) -> f32 {
    let range = (ch.in_white - ch.in_black).max(1e-6);
    let quantum = quantum.filter(|q| q.is_finite() && *q >= 1.0);
    let whole = |x: f32| match quantum {
        Some(q) => (x * q + 0.5).floor() / q,
        None => x,
    };
    let u = match quantum {
        Some(q) => {
            let v = (v * q).round() / q;
            (((v - ch.in_black) / range).clamp(0.0, 1.0) * q + 0.5 + 1e-3).floor() / q
        }
        None => ((v - ch.in_black) / range).clamp(0.0, 1.0),
    };
    let t = whole(midtone_gamma(u, ch.gamma));
    whole(ch.out_black + t * (ch.out_white - ch.out_black))
}

/// Exponent of the shadow toe in [`levels_gamma`] (fitted; see there).
const LEVELS_TOE_POWER: f32 = 2.2;

/// An alternative fit of Levels' midtone gamma (Levels itself uses [`midtone_gamma`]), kept for
/// comparison: `g` on `u` in 0..1 is `u^(1/g)`, except that above 1, where that power
/// curve is infinitely steep at black, Photoshop starts along a toe `a·u − b·u^q` of slope
/// `a = 2^g` that joins the curve with the same value and slope. `q` = [`LEVELS_TOE_POWER`] is
/// fitted on whole-level oracles at gamma 1.16, 1.22, 1.3, 1.78 and 2.0 (psd-tools levels_rgb
/// and levels_grayscale, the photoshop corpus's rgb8 and gray8 levels.psd, a measured 0..255
/// ramp): all but 2 of their 1118 levels exact, the others within one. Below 1 the power curve is
/// exact (gamma 0.67 and 0.7: every level).
pub fn levels_gamma(u: f32, g: f32) -> f32 {
    let c = 1.0 / g;
    if g <= 1.0 {
        return u.powf(c);
    }
    let (a, q) = (2f32.powf(g), LEVELS_TOE_POWER);
    // Where the toe meets the curve: a·u1 − b·u1^q = u1^c and a − q·b·u1^(q−1) = c·u1^(c−1).
    let u1 = ((1.0 - c / q) / (a * (1.0 - 1.0 / q))).powf(1.0 / (1.0 - c));
    if u >= u1 || u1 <= 0.0 {
        return u.powf(c);
    }
    let b = (a * u1 - u1.powf(c)) / u1.powf(q);
    a * u - b * u.powf(q)
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
    let t = if ch.gamma == 1.0 { t } else { t.signum() * t.abs().powf(1.0 / ch.gamma.max(0.01)) };
    ch.out_black + t * (ch.out_white - ch.out_black)
}

/// The LUT of one Curves curve: [`curve_map_lut`] for a pencil-drawn map, else [`curve_lut`].
pub fn curve_lut_mode(points: &[CurvePoint], pencil: bool) -> Vec<f32> {
    if pencil { curve_map_lut(points) } else { curve_lut(points) }
}

/// A pencil-drawn Curves map (see [`photosplice_doc::adjust::curve_map`]): straight segments
/// between its entries, constant outside the first/last, clamped to 0..=1, sampled into a LUT.
pub fn curve_map_lut(points: &[CurvePoint]) -> Vec<f32> {
    let mut pts: Vec<(f32, f32)> = points.iter().filter(|p| p.input.is_finite() && p.output.is_finite()).map(|p| (p.input, p.output)).collect();
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    pts.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-6);
    let ident = |k: usize| k as f32 / (LUT_SIZE - 1) as f32;
    let (Some(&first), Some(&last)) = (pts.first(), pts.last()) else { return (0..LUT_SIZE).map(ident).collect() };
    let mut seg = 0;
    (0..LUT_SIZE)
        .map(|k| {
            let xv = ident(k);
            if xv <= first.0 {
                return first.1.clamp(0.0, 1.0);
            }
            if xv >= last.0 {
                return last.1.clamp(0.0, 1.0);
            }
            while pts.get(seg + 1).is_some_and(|p| p.0 < xv) {
                seg += 1;
            }
            let (Some(&(x0, y0)), Some(&(x1, y1))) = (pts.get(seg), pts.get(seg + 1)) else { return last.1.clamp(0.0, 1.0) };
            let t = if x1 > x0 { (xv - x0) / (x1 - x0) } else { 0.0 };
            (y0 + (y1 - y0) * t).clamp(0.0, 1.0)
        })
        .collect()
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

/// A layer-style contour curve: [`curve_lut`], except that the curve bends sharply at corner
/// points (`corners[i]` true; Photoshop's Contour Editor "Corner"): each run between corners is
/// its own natural spline, so all-corner points make a polyline. Missing flags are smooth.
pub fn contour_curve_lut(points: &[CurvePoint], corners: &[bool]) -> Vec<f32> {
    let mut pts: Vec<(CurvePoint, bool)> = points.iter().enumerate().map(|(i, p)| (p.clone(), corners.get(i).copied().unwrap_or(false))).collect();
    if !pts.iter().any(|p| p.1) {
        return curve_lut(points);
    }
    pts.sort_by(|a, b| a.0.input.partial_cmp(&b.0.input).unwrap_or(std::cmp::Ordering::Equal));
    pts.dedup_by(|a, b| (a.0.input - b.0.input).abs() < 1e-6);
    let mut out: Vec<f32> = Vec::new();
    let mut start = 0usize;
    let last = pts.len().saturating_sub(1);
    for i in 1..=last {
        let corner = pts.get(i).is_some_and(|p| p.1);
        if !(corner || i == last) {
            continue;
        }
        let Some(seg) = pts.get(start..=i) else { break };
        let from = seg.first().map_or(0.0, |p| p.0.input);
        let lut = curve_lut(&seg.iter().map(|p| p.0.clone()).collect::<Vec<_>>());
        if out.is_empty() {
            out = lut;
        } else {
            for (k, (o, v)) in out.iter_mut().zip(lut).enumerate() {
                if k as f32 / (LUT_SIZE - 1) as f32 >= from {
                    *o = v;
                }
            }
        }
        start = i;
    }
    if out.is_empty() { curve_lut(points) } else { out }
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

    /// Photoshop 27.11, measured: absolute Selective Color on greys (red channel after one range's
    /// cyan change).
    #[test]
    fn absolute_selective_color_matches_photoshop() {
        // (range index, cyan %, grey, Photoshop's red)
        let measured = [
            (7, 50.0, 64u8, 32u8),
            (7, 50.0, 192, 129),
            (7, -50.0, 192, 223),
            (7, -50.0, 64, 128),
            (6, -50.0, 192, 224),
            (6, -50.0, 240, 253),
            (8, 50.0, 64, 32),
            (8, 50.0, 16, 2),
        ];
        for (range, cyan, grey, want) in measured {
            let mut adj = [[0.0f32; 4]; 9];
            adj[range][0] = cyan;
            let g = grey as f32 / 255.0;
            let got = selective_color([g; 3], false, &adj)[0] * 255.0;
            assert!((got - want as f32).abs() <= 1.5, "range {range} cyan {cyan} grey {grey}: {got} vs {want}");
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
            let mut buf = Buffer { rect: photosplice_geom::Rect::new(0, 0, 1, 1), px: vec![[v, v, v, 1.0]] };
            apply_depth(&Adjustment::BrightnessContrast { brightness: b, contrast: c, legacy: true }, &mut buf, Transfer::Srgb, None);
            buf.px[0][0] * 255.0
        };
        assert!((at(25.0, 30.0, 85.0 / 255.0) - 103.0).abs() < 1.0);
        assert!((at(25.0, 30.0, 170.0 / 255.0) - 224.0).abs() < 1.0);
        let k = 1.0 - 26.0 / 100.0;
        // Contrast pivots at 128 of 255 (`LEGACY_PIVOT`).
        let want = (100.0 - 128.0) * k + 128.0 - 23.0;
        assert!((at(-23.0, -26.0, 100.0 / 255.0) - want).abs() < 0.01);
    }
}

#[cfg(test)]
mod tone_tests {
    use super::*;
    use photosplice_geom::Rect;

    /// Corpus rgb32 hue-saturation-colorize.psd (hue 200, 40 %, +5): in a float document the
    /// chroma also shrinks by 1 − l about the moved lightness; 16-bit keeps the HSL colour.
    #[test]
    fn float_colorize_shrinks_chroma_with_lightness() {
        for (c, want) in [([0.4735; 3], [0.3099, 0.5632, 0.6898]), ([1.0, 0.0, 0.0], [0.3445, 0.5852, 0.7055])] {
            let got = hue_saturation_in(c, 200.0, 0.4, 0.05, true, true);
            assert!((0..3).all(|i| (got[i] - want[i]).abs() < 0.001), "{c:?}: {got:?}");
        }
        let got = hue_saturation_in([0.4735; 3], 200.0, 0.4, 0.05, true, false);
        assert!((got[0] - 0.3).abs() < 0.001 && (got[2] - 0.7).abs() < 0.001, "{got:?}");
    }

    /// Corpus rgb32 photo-filter.psd (40 %, Preserve Luminosity): the matrix its pixels give,
    /// scaled in sRGB's D65 XYZ (in D50 XYZ blue → red was 0.095, not 0.128).
    #[test]
    fn float_photo_filter_scales_d65_xyz() {
        let m = photo_filter_matrix([0.966_445_6, 0.761_806_3, 0.0], 0.4, true, true);
        let want = [[1.0284, 0.0371, 0.1279], [-0.0079, 0.9921, -0.0132], [-0.0045, -0.0306, 0.7533]];
        for r in 0..3 {
            for c in 0..3 {
                assert!((m[r][c] - want[r][c]).abs() < 0.0006, "{m:?}");
            }
        }
    }

    /// Corpus rgb32 hue-saturation.psd (hue +40, saturation +25, lightness −10): float documents
    /// keep channels outside 0..1; 8/16-bit clip.
    #[test]
    fn float_hue_saturation_is_not_clipped() {
        let f = hue_saturation_ranged([1.0, 0.0, 0.0], 40.0, 0.25, -0.1, 0.0, true, false);
        assert!((f[0] - 1.0477).abs() < 0.004 && (f[1] - 0.649).abs() < 0.004 && (f[2] + 0.1477).abs() < 0.004, "{f:?}");
        let i = hue_saturation_ranged([1.0, 0.0, 0.0], 40.0, 0.25, -0.1, 0.0, false, false);
        assert!(i.iter().all(|v| (0.0..=1.0).contains(v)), "{i:?}");
    }

    fn ramp() -> Buffer {
        let rect = Rect::new(0, 0, 8, 8);
        let px = (0..64).map(|i| [((i % 8) as f32) / 7.0, ((i / 8) as f32) / 7.0, 0.3, 1.0]).collect();
        Buffer { rect, px }
    }

    /// Photoshop 27.11, measured: Brightness/Contrast with Use Legacy on, over a gray ramp
    /// (x = 0, 16, …, 240, 255), one row per (brightness, contrast).
    /// One pixel through `adj` in a 32-bit document.
    fn hdr(adj: &Adjustment, c: [f32; 3]) -> [f32; 3] {
        let mut b = Buffer::filled(crate::Rect::new(0, 0, 1, 1), [c[0], c[1], c[2], 1.0]);
        apply_depth(adj, &mut b, Transfer::Gamma(1.0), Some(SampleType::F32));
        [b.px[0][0], b.px[0][1], b.px[0][2]]
    }

    /// Photoshop's own 32-bit results (photoshop corpus rgb32): nothing clips, legacy contrast
    /// pivots at 127, the mixer cuts its percentages to 1/1024 and counts 255/512 of the
    /// constant, and the Black & White tint keeps values above 1.
    #[test]
    fn thirty_two_bit_adjustments_match_photoshop_unclipped() {
        let near = |got: [f32; 3], want: [f32; 3], tol: f32| got.iter().zip(want).all(|(g, w)| (g - w).abs() <= tol);
        let bc = Adjustment::BrightnessContrast { brightness: 25.0, contrast: 30.0, legacy: true };
        // brightness-contrast-legacy.psd
        assert!(near(hdr(&bc, [0.4910, 0.0009, 0.9301]), [0.6280, -0.0721, 1.2553], 2e-4), "{:?}", hdr(&bc, [0.4910, 0.0009, 0.9301]));
        // channel-mixer.psd: pure red, green, blue and black, exactly.
        let mixer = Adjustment::ChannelMixer { matrix: [[0.8, 0.3, -0.1, 0.0], [0.1, 0.9, 0.0, 0.05], [0.0, 0.2, 0.7, 0.0]], monochrome: false, cmyk: None };
        for (c, want) in [
            ([1.0, 0.0, 0.0], [0.7998047, 0.1245117, 0.0]),
            ([0.0, 1.0, 0.0], [0.2998047, 0.9243164, 0.1992188]),
            ([0.0, 0.0, 1.0], [-0.0996094, 0.0249023, 0.6992188]),
            ([0.0, 0.0, 0.0], [0.0, 0.0249023, 0.0]),
        ] {
            assert!(near(hdr(&mixer, c), want, 1e-6), "{c:?}: {:?}", hdr(&mixer, c));
        }
        // black-white-tint.psd: white tints past 1.
        let bw = Adjustment::BlackWhite { weights: [40.0, 60.0, 40.0, 60.0, 20.0, 80.0], tint: Some([0.88235295, 0.827451, 0.7019608]) };
        assert!(near(hdr(&bw, [1.0; 3]), [1.0522, 0.9973, 0.8718], 2e-4), "{:?}", hdr(&bw, [1.0; 3]));
        // 8/16-bit documents still clip.
        let mut b = Buffer::filled(crate::Rect::new(0, 0, 1, 1), [1.0, 1.0, 1.0, 1.0]);
        apply_depth(&bw, &mut b, Transfer::Srgb, None);
        assert!(b.px[0].iter().all(|v| *v <= 1.0));
    }

    #[test]
    fn legacy_brightness_contrast_matches_photoshop() {
        const X: [u8; 17] = [0, 16, 32, 48, 64, 80, 96, 112, 128, 144, 160, 176, 192, 208, 224, 240, 255];
        let measured: [(f32, f32, [u8; 17]); 7] = [
            (40.0, 0.0, [40, 56, 72, 88, 104, 120, 136, 152, 168, 184, 200, 216, 232, 248, 255, 255, 255]),
            (-40.0, 0.0, [0, 0, 0, 8, 24, 40, 56, 72, 88, 104, 120, 136, 152, 168, 184, 200, 215]),
            (0.0, 30.0, [0, 0, 0, 14, 37, 60, 82, 105, 128, 151, 174, 196, 219, 242, 255, 255, 255]),
            (0.0, -30.0, [38, 49, 61, 72, 83, 94, 106, 117, 128, 139, 150, 162, 173, 184, 195, 207, 217]),
            (40.0, 30.0, [3, 25, 48, 71, 94, 117, 139, 162, 185, 208, 231, 253, 255, 255, 255, 255, 255]),
            (100.0, 0.0, [100, 116, 132, 148, 164, 180, 196, 212, 228, 244, 255, 255, 255, 255, 255, 255, 255]),
            (0.0, 80.0, [0, 0, 0, 0, 0, 0, 0, 48, 128, 208, 255, 255, 255, 255, 255, 255, 255]),
        ];
        for (brightness, contrast, want) in measured {
            let rect = Rect::new(0, 0, 17, 1);
            let px = X.iter().map(|x| [f32::from(*x) / 255.0; 4]).map(|p| [p[0], p[1], p[2], 1.0]).collect();
            let mut b = Buffer { rect, px };
            apply(&Adjustment::BrightnessContrast { brightness, contrast, legacy: true }, &mut b);
            for (i, (p, w)) in b.px.iter().zip(want).enumerate() {
                let got = (p[0] * 255.0).round() as i32;
                assert!((got - i32::from(w)).abs() <= 1, "b {brightness} c {contrast} at {}: {got} vs Photoshop {w}", X[i]);
            }
        }
    }

    /// Pixel → Photoshop's split. 8-bit from psd-tools threshold_rgb.psd: the first three are
    /// white though their Rec. 601 grey rounds below 60, and (131, 109, 99) sits exactly on 114.5
    /// at level 115 (halves round down). 16-bit from threshold_16bits_rgb.psd and the photoshop
    /// corpus's rgb16 threshold-128.psd, whose mid grey 16448 is level 128 in 16 bits.
    #[test]
    fn threshold_splits_on_the_luminosity_like_photoshop() {
        let c8 = |r: u8, g: u8, b: u8| [r, g, b].map(|v| f32::from(v) / 255.0);
        for (c, level, white) in
            [(c8(87, 51, 31), 60u8, true), (c8(90, 49, 33), 60, true), (c8(68, 57, 50), 60, true), (c8(48, 63, 72), 60, false), (c8(131, 109, 99), 115, false)]
        {
            assert_eq!(threshold_white(c, f32::from(level) / 255.0, Some(SampleType::U8)), white, "8-bit {c:?} at {level}");
        }
        let c16 = |r: u16, g: u16, b: u16| [r, g, b].map(|v| f32::from(v) / 32768.0);
        for (c, level, white) in [
            (c16(8995, 7196, 6425), 60u8, false),
            (c16(11180, 6554, 3984), 60, false),
            (c16(16448, 16448, 16448), 128, true),
            (c16(16447, 16447, 16447), 128, false),
        ] {
            assert_eq!(threshold_white(c, f32::from(level) / 255.0, Some(SampleType::U16)), white, "16-bit {c:?} at {level}");
        }
        // Through the adjustment: neutral black or white.
        let mut b = Buffer::filled(Rect::new(0, 0, 1, 1), [87.0 / 255.0, 51.0 / 255.0, 31.0 / 255.0, 1.0]);
        apply_depth(&Adjustment::Threshold { level: 60.0 / 255.0 }, &mut b, Transfer::Srgb, Some(SampleType::U8));
        assert_eq!(b.px[0], [1.0, 1.0, 1.0, 1.0]);
    }

    /// 16-bit sample → Photoshop's 8-bit level, from psd-tools posterize_16bits_rgb.psd: bins
    /// split on the 16-bit sample (10794 and 10923 straddle a third of 32768), and the middle of 3
    /// levels is 128 where 8 bits give 127, while 21 levels keep 25 for 25.5.
    #[test]
    fn sixteen_bit_posterize_matches_photoshop() {
        let points: [(u32, u16, u8); 9] =
            [(3, 10794, 0), (3, 10923, 128), (3, 21845, 128), (3, 21974, 255), (13, 2504, 0), (13, 2542, 21), (13, 7453, 42), (13, 7582, 64), (21, 3341, 25)];
        for (levels, x, want) in points {
            let got = posterize_in(f32::from(x) / 32768.0, levels, Some(SampleType::U16));
            assert!((got * 255.0 - f32::from(want)).abs() < 1e-3, "{levels} levels at {x}: {} vs Photoshop {want}", got * 255.0);
        }
        // 8 bits keep their rule.
        assert_eq!(posterize_in(128.0 / 255.0, 3, Some(SampleType::U8)), posterize(128.0 / 255.0, 3));
    }

    #[test]
    fn color_lookup_with_an_overflowing_stored_size_is_the_identity() {
        // A stored document's size is untrusted: (2²²)³ × 3 overflows, which once wrapped past
        // the table-length guard and indexed out of bounds.
        for size in [1 << 22, u32::MAX] {
            let a = Adjustment::ColorLookup { name: "x".into(), lut: Some(std::sync::Arc::new(vec![0.5; 24])), size, tetrahedral: false, dither: false };
            let mut b = ramp();
            apply(&a, &mut b);
            assert_eq!(b.px, ramp().px, "size {size}");
        }
    }

    /// Photoshop 27.11, measured: Color Balance with one cyan–red slider moved, over a gray ramp
    /// (x = 0, 16, …, 240, 255); red and green out (blue follows green on gray).
    #[test]
    fn color_balance_matches_photoshop() {
        const X: [u8; 17] = [0, 16, 32, 48, 64, 80, 96, 112, 128, 144, 160, 176, 192, 208, 224, 240, 255];
        type Row = (&'static str, f32, bool, [u8; 17], [u8; 17]);
        let measured: [Row; 6] = [
            ("midtones", 50.0, false, [0, 36, 59, 78, 96, 112, 128, 142, 156, 170, 183, 196, 209, 221, 233, 244, 255], X),
            ("shadows", -100.0, false, [0, 0, 0, 0, 0, 0, 0, 7, 23, 43, 67, 93, 122, 154, 186, 221, 255], X),
            ("highlights", 100.0, false, [0, 51, 84, 111, 136, 160, 182, 202, 223, 242, 255, 255, 255, 255, 255, 255, 255], X),
            ("shadows", 50.0, true, X, [0, 0, 0, 0, 17, 37, 57, 77, 97, 117, 137, 157, 177, 197, 216, 236, 255]),
            (
                "midtones",
                100.0,
                true,
                [0, 36, 59, 78, 96, 112, 128, 142, 156, 170, 183, 196, 209, 221, 233, 244, 255],
                [0, 5, 14, 24, 36, 50, 64, 80, 97, 114, 132, 151, 171, 191, 212, 234, 255],
            ),
            ("highlights", -100.0, true, X, [0, 26, 53, 79, 105, 132, 158, 184, 211, 237, 255, 255, 255, 255, 255, 255, 255]),
        ];
        // Two sliders in one tone, Preserve Luminosity on (measured): shadows red and blue +50 darken
        // only green; highlights red −50 with green +50 lift green by 100 and blue by 50.
        let two =
            |tones: [[f32; 3]; 3], x: f32| -> [i32; 3] { std::array::from_fn(|ch| (color_balance_channel(tones, true, ch, x / 255.0) * 255.0).round() as i32) };
        assert_eq!(two([[50.0, 0.0, 50.0], [0.0; 3], [0.0; 3]], 128.0), [128, 97, 128]);
        let hi = two([[0.0; 3], [0.0; 3], [-50.0, 50.0, 0.0]], 128.0);
        assert!((hi[0] - 128).abs() <= 1 && (hi[1] - 211).abs() <= 1 && (hi[2] - 159).abs() <= 1, "{hi:?}");
        for (tone, amount, preserve, red, green) in measured {
            let mut tones = [[0.0f32; 3]; 3];
            tones[["shadows", "midtones", "highlights"].iter().position(|t| *t == tone).unwrap()][0] = amount;
            for (i, x) in X.iter().enumerate() {
                let v = f32::from(*x) / 255.0;
                let got = |ch| (color_balance_channel(tones, preserve, ch, v) * 255.0).round() as i32;
                let (r, g) = (got(0), got(1));
                assert!((r - i32::from(red[i])).abs() <= 1, "{tone} {amount} pl {preserve} at {x}: red {r} vs Photoshop {}", red[i]);
                assert!((g - i32::from(green[i])).abs() <= 1, "{tone} {amount} pl {preserve} at {x}: green {g} vs Photoshop {}", green[i]);
            }
        }
    }

    /// Photoshop, measured (8-bit gray ramp, every channel): whole settings, where the tones fold
    /// into one Levels per channel (#3423). The issue's midtones red +100 / magenta −100 (the
    /// shadow toe), its sequoia14 layer with Preserve Luminosity off and on, all nine sliders with
    /// Preserve Luminosity on, a black point past the white point, and lowered shadows with raised
    /// highlights (their gammas cancel: a straight line between the two points).
    #[test]
    fn color_balance_whole_settings_match_photoshop() {
        const X: [u8; 12] = [0, 4, 8, 16, 32, 64, 96, 128, 160, 192, 224, 255];
        type Case = ([[f32; 3]; 3], bool, [[u8; 12]; 3]);
        let cases: [Case; 6] = [
            (
                [[0.0; 3], [100.0, -100.0, 0.0], [0.0; 3]],
                false,
                [[0, 16, 30, 55, 90, 128, 156, 181, 202, 221, 239, 255], [0, 0, 0, 1, 4, 16, 36, 64, 100, 145, 197, 255], X],
            ),
            (
                [[-6.0, 22.0, 5.0], [-1.0, -3.0, -8.0], [-1.0, -4.0, -6.0]],
                false,
                [
                    [0, 0, 2, 9, 25, 56, 89, 122, 156, 188, 222, 255],
                    [0, 5, 9, 18, 35, 67, 100, 131, 163, 194, 225, 255],
                    [0, 3, 7, 14, 29, 60, 91, 123, 156, 189, 222, 255],
                ],
            ),
            (
                [[-6.0, 22.0, 5.0], [-1.0, -3.0, -8.0], [-1.0, -4.0, -6.0]],
                true,
                [
                    [0, 0, 0, 0, 5, 42, 80, 117, 154, 189, 226, 255],
                    [0, 4, 8, 16, 33, 66, 98, 130, 162, 195, 226, 255],
                    [0, 0, 0, 0, 15, 48, 82, 116, 151, 186, 221, 255],
                ],
            ),
            (
                [[40.0, -30.0, 20.0], [-50.0, 25.0, 60.0], [30.0, -70.0, 10.0]],
                true,
                [
                    [0, 1, 3, 9, 25, 69, 126, 193, 255, 255, 255, 255],
                    [0, 0, 0, 0, 0, 0, 46, 93, 136, 177, 217, 255],
                    [0, 0, 0, 0, 44, 107, 156, 199, 238, 255, 255, 255],
                ],
            ),
            (
                [[100.0, -100.0, 0.0], [0.0; 3], [-100.0, 100.0, 0.0]],
                true,
                [X, [0, 0, 0, 0, 0, 255, 255, 255, 255, 255, 255, 255], [0, 0, 0, 0, 0, 0, 0, 130, 255, 255, 255, 255]],
            ),
            (
                [[-76.0, -2.0, 60.0], [0.0; 3], [99.0, 0.0, -30.0]],
                false,
                [
                    [0, 0, 0, 0, 0, 0, 71, 171, 255, 255, 255, 255],
                    [0, 2, 6, 14, 29, 61, 94, 126, 158, 191, 224, 255],
                    [0, 6, 11, 21, 39, 73, 106, 137, 168, 197, 227, 255],
                ],
            ),
        ];
        for (tones, preserve, want) in cases {
            for (ch, row) in want.iter().enumerate() {
                for (x, w) in X.iter().zip(row) {
                    let got = (color_balance_channel(tones, preserve, ch, f32::from(*x) / 255.0) * 255.0).round() as i32;
                    assert!((got - i32::from(*w)).abs() <= 1, "{tones:?} pl {preserve} ch {ch} at {x}: {got} vs Photoshop {w}");
                }
            }
        }
        // Hostile input stays in range.
        for v in [f32::NAN, -1.0, 2.0, f32::INFINITY] {
            let out = color_balance_channel([[f32::NAN, 1e9, -1e9]; 3], true, 7, v);
            assert!((0.0..=1.0).contains(&out), "{v}: {out}");
        }
    }

    #[test]
    fn color_balance_midtones_with_multiple_sliders_preserve_luminosity() {
        // A shared offset depends on the two extremes, not the sum of moved sliders.
        // The old per-slider product leaves red/blue unchanged for [30, 0, 30].
        let tones = [[0.0; 3], [30.0, 0.0, 30.0], [0.0; 3]];
        for input in [0.25_f32, 0.5, 0.75] {
            for (ch, offset) in [-0.15_f32, 0.15, -0.15].into_iter().enumerate() {
                let expected = input.powf(2f32.powf(offset));
                let actual = color_balance_channel(tones, true, ch, input);
                assert!((actual - expected).abs() < 1e-6, "channel {ch}, input {input}: {actual} vs {expected}");
            }
            // Preserve Luminosity off must still affect only the moved channels.
            assert_eq!(color_balance_channel(tones, false, 1, input), input);
            // One moved slider retains its previous half-strength compensation.
            let single = [[0.0; 3], [30.0, 0.0, 0.0], [0.0; 3]];
            assert!((color_balance_channel(single, true, 0, input) - input.powf(2f32.powf(-0.15))).abs() < 1e-6);
            assert!((color_balance_channel(single, true, 1, input) - input.powf(2f32.powf(0.15))).abs() < 1e-6);
        }
    }

    #[test]
    fn levels_and_curves_apply_each_channel_before_the_master() {
        // These two affine tone adjustments describe the same mapping:
        // channel(x) = 1 / 2 + x / 2, then master(x) = x / 2, so 0 maps to 0.25.
        // Levels used to apply the master first (0 mapped to 0.5).
        let line = |a: f32, b: f32| vec![CurvePoint { input: 0.0, output: a }, CurvePoint { input: 1.0, output: b }];
        let levels = Adjustment::Levels {
            master: LevelsChannel { out_white: 0.5, ..Default::default() },
            per_channel: std::array::from_fn(|_| LevelsChannel { out_black: 0.5, ..Default::default() }),
            space: ToneSpace::Rgb,
            black: Default::default(),
        };
        let curves = Adjustment::Curves {
            master: line(0.0, 0.5),
            per_channel: std::array::from_fn(|_| line(0.5, 1.0)),
            space: ToneSpace::Rgb,
            black: Vec::new(),
            pencil: false,
        };
        for depth in [None, Some(SampleType::U8), Some(SampleType::U16), Some(SampleType::F32)] {
            let ll = tone_luts_depth(&levels, depth);
            let cl = tone_luts_depth(&curves, depth);
            for channel in 0..3 {
                for index in [0, LUT_SIZE / 4, LUT_SIZE / 2, 3 * LUT_SIZE / 4, LUT_SIZE - 1] {
                    assert!(
                        (ll[channel][index] - cl[channel][index]).abs() < 0.01,
                        "{depth:?} channel {channel} sample {index}: Levels {} vs Curves {}",
                        ll[channel][index],
                        cl[channel][index]
                    );
                }
            }
            assert!((cl[0][0] - 0.25).abs() < 0.01, "{depth:?}: {}", cl[0][0]);

            // Exercise the compositor's real adjustment path, not only its LUT builder.
            for adj in [&levels, &curves] {
                let mut pixel = Buffer { rect: Rect::new(0, 0, 1, 1), px: vec![[0.0, 0.0, 0.0, 1.0]] };
                apply_depth(adj, &mut pixel, Transfer::Srgb, depth);
                for channel in 0..3 {
                    assert!((pixel.px[0][channel] - 0.25).abs() < 0.01, "{depth:?}: {adj:?} rendered {:?}", pixel.px[0]);
                }
            }
        }

        // Lab has no composite/master tone record.
        let mut lab = curves.clone();
        let mut lab_without_master = curves;
        if let Adjustment::Curves { space, .. } = &mut lab {
            *space = ToneSpace::Lab;
        }
        if let Adjustment::Curves { master, space, .. } = &mut lab_without_master {
            *space = ToneSpace::Lab;
            *master = line(0.0, 1.0);
        }
        assert_eq!(tone_luts(&lab), tone_luts(&lab_without_master));
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
            pencil: false,
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
            pencil: false,
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
    fn colorize_at_zero_saturation_is_grey() {
        // Photoshop: Colorize at 0% saturation leaves no colour, only the pixel's lightness (#2326).
        let px = vec![[0.9, 0.2, 0.1, 1.0], [0.1, 0.6, 0.8, 1.0], [0.5, 0.5, 0.5, 1.0]];
        for depth in [None, Some(SampleType::U8), Some(SampleType::U16), Some(SampleType::F32)] {
            for lightness in [0.0, 30.0, -30.0] {
                let a = Adjustment::HueSaturation { hue: 200.0, saturation: 0.0, lightness, colorize: true, ranges: HueRange::defaults() };
                let mut b = Buffer { rect: Rect::new(0, 0, 3, 1), px: px.clone() };
                apply_depth(&a, &mut b, Transfer::Srgb, depth);
                for p in &b.px {
                    assert!((p[0] - p[1]).abs() < 1e-4 && (p[1] - p[2]).abs() < 1e-4, "{depth:?} lightness {lightness}: {p:?}");
                }
            }
            // Low saturations still tint: the old 25% floor no longer hides them.
            let tint = |saturation: f32| {
                let a = Adjustment::HueSaturation { hue: 0.0, saturation, lightness: 0.0, colorize: true, ranges: HueRange::defaults() };
                let mut b = Buffer { rect: Rect::new(0, 0, 1, 1), px: vec![[0.5, 0.5, 0.5, 1.0]] };
                apply_depth(&a, &mut b, Transfer::Srgb, depth);
                b.px[0][0] - b.px[0][2]
            };
            assert!(tint(10.0) > 0.05 && tint(10.0) < tint(25.0), "{depth:?}: 10% {} vs 25% {}", tint(10.0), tint(25.0));
        }
    }

    /// Photoshop 27.11, measured: Hue/Saturation raising saturation, and Colorize with Lightness.
    #[test]
    fn hue_saturation_matches_photoshop() {
        // (input, hue, saturation, lightness, colorize, Photoshop's output)
        type Case = ([u8; 3], f32, f32, f32, bool, [u8; 3]);
        let measured: [Case; 8] = [
            ([204, 102, 102], 0.0, 50.0, 0.0, false, [254, 51, 51]),
            ([204, 159, 102], 0.0, 50.0, 0.0, false, [254, 164, 51]),
            ([204, 102, 102], 0.0, -50.0, 0.0, false, [178, 127, 127]),
            ([0, 0, 0], 120.0, 100.0, 30.0, true, [0, 152, 0]),
            ([28, 28, 28], 120.0, 100.0, 30.0, true, [0, 192, 0]),
            ([160, 160, 160], 30.0, 25.0, -40.0, true, [120, 96, 73]),
            ([252, 252, 252], 30.0, 25.0, -40.0, true, [177, 151, 126]),
            ([255, 143, 0], 30.0, 25.0, -40.0, true, [95, 76, 58]),
        ];
        for (input, hue, saturation, lightness, colorize, want) in measured {
            let a = Adjustment::HueSaturation { hue, saturation, lightness, colorize, ranges: HueRange::defaults() };
            let c = input.map(|v| v as f32 / 255.0);
            let mut b = Buffer { rect: Rect::new(0, 0, 1, 1), px: vec![[c[0], c[1], c[2], 1.0]] };
            apply(&a, &mut b);
            for (i, w) in want.iter().enumerate() {
                let got = b.px[0][i] * 255.0;
                assert!((got - *w as f32).abs() <= 2.0, "{input:?} h{hue} s{saturation} l{lightness} colorize {colorize}, channel {i}: {got} vs {w}");
            }
        }
    }

    /// Photoshop 27.11, measured: a range's Lightness (Blues ±50) moves towards the brightest or darkest channel.
    #[test]
    fn range_lightness_matches_photoshop() {
        let measured = [
            ([0u8, 29, 115], 0.5, [57u8, 71, 115]),
            ([0, 64, 255], 0.5, [125, 157, 255]),
            ([102, 128, 204], 0.5, [152, 165, 204]),
            ([102, 128, 204], -0.5, [102, 115, 154]),
            ([0, 18, 115], -0.5, [0, 9, 58]),
        ];
        for (input, l, want) in measured {
            let got = range_lightness(input.map(|v| v as f32 / 255.0), l);
            for i in 0..3 {
                assert!((got[i] * 255.0 - want[i] as f32).abs() <= 2.5, "{input:?} channel {i}: {} vs {}", got[i] * 255.0, want[i]);
            }
        }
    }

    /// Photoshop's own render (merged image of psd-tools `adjustments/huesaturation_lightness_rgb.psd`,
    /// layer "Hue/Saturation 1"): a dull magenta gets the whole range edit, as a vivid one does (#4089).
    #[test]
    fn hue_ranges_apply_in_full_to_dull_colours() {
        let range = |lightness: f32, bounds: [f32; 4]| HueRange { hue: 0.0, saturation: 0.0, lightness, bounds };
        let ranges = [
            range(-11.0, [-57.0, 4.0, 82.0, 175.0]),
            range(100.0, [12.0, 45.0, 75.0, 105.0]),
            HueRange::neutral(2),
            HueRange::neutral(3),
            HueRange::neutral(4),
            range(100.0, [-117.0, -50.0, 39.0, 104.0]),
        ];
        let a = Adjustment::HueSaturation { hue: 0.0, saturation: 0.0, lightness: 17.0, colorize: false, ranges };
        let measured = [([126u8, 97, 120], [148u8, 148, 148]), ([131, 103, 125], [152, 152, 152]), ([95, 69, 90], [122, 122, 122])];
        for (input, want) in measured {
            let c = input.map(|v| v as f32 / 255.0);
            let mut b = Buffer { rect: Rect::new(0, 0, 1, 1), px: vec![[c[0], c[1], c[2], 1.0]] };
            apply(&a, &mut b);
            for (i, w) in want.iter().enumerate() {
                let got = b.px[0][i] * 255.0;
                assert!((got - *w as f32).abs() <= 2.0, "{input:?} channel {i}: {got} vs {w}");
            }
        }
        // Greys have no hue, so the ranges leave them alone: only the master Lightness moves them.
        let mut b = Buffer { rect: Rect::new(0, 0, 1, 1), px: vec![[0.4, 0.4, 0.4, 1.0]] };
        apply(&a, &mut b);
        let p = b.px[0];
        assert!((p[0] - p[1]).abs() < 1e-6 && (p[1] - p[2]).abs() < 1e-6 && (p[0] - (0.4 + 0.6 * 0.17)).abs() < 1e-4, "{p:?}");
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
        let mut b = Buffer { rect: photosplice_geom::Rect::new(0, 0, 1, 1), px: vec![[0.0, 0.0, 1.0, 1.0]] };
        apply(&Adjustment::GradientMap { stops: vec![(0.0, [0.0; 3]), (1.0, [1.0; 3])], reverse: false, dither: false, ink_stops: Vec::new() }, &mut b);
        assert!((b.px[0][0] - 0.11).abs() < 1e-3, "{:?}", b.px[0]);
    }

    /// Photoshop 27.11, measured: Black & White (default weights) with Tint over greys. The tint
    /// is blended like the Color blend mode: its hue and saturation at the grey's luminosity.
    #[test]
    fn black_white_tint_matches_photoshop() {
        let weights = [40.0, 60.0, 40.0, 60.0, 20.0, 80.0];
        let measured: [([u8; 3], u8, [u8; 3]); 6] = [
            ([255, 0, 0], 24, [81, 1, 1]),
            ([255, 0, 0], 120, [255, 64, 64]),
            ([255, 0, 0], 240, [255, 235, 235]),
            ([0xe1, 0xd3, 0xb3], 120, [133, 119, 87]),
            ([0xe1, 0xd3, 0xb3], 24, [33, 24, 1]),
            ([0x40, 0x20, 0x10], 120, [144, 112, 96]),
        ];
        for (tint, grey, want) in measured {
            let tint = Some(tint.map(|v| v as f32 / 255.0));
            let mut b = Buffer { rect: Rect::new(0, 0, 1, 1), px: vec![[grey as f32 / 255.0; 4]] };
            b.px[0][3] = 1.0;
            apply(&Adjustment::BlackWhite { weights, tint }, &mut b);
            for (i, w) in want.iter().enumerate() {
                let got = b.px[0][i] * 255.0;
                assert!((got - *w as f32).abs() <= 2.5, "tint {tint:?} grey {grey} channel {i}: {got} vs {w}");
            }
        }
    }

    #[test]
    fn gradient_map_follows_the_w3c_luminosity() {
        // Photoshop's Gradient Map is driven by 0.3 R + 0.59 G + 0.11 B (photoshop corpus rgb8 and
        // rgb16 gradient-map.psd), not the Rec. 601 grey.
        let stops = vec![(0.0, [0.0; 3]), (1.0, [1.0; 3])];
        for (c, t) in [([0.0, 1.0, 0.0], 0.59), ([1.0, 0.0, 0.0], 0.3), ([0.0, 0.0, 1.0], 0.11), ([0.2, 0.6, 0.9], 0.513)] {
            let mut b = Buffer::filled(crate::Rect::new(0, 0, 1, 1), [c[0], c[1], c[2], 1.0]);
            apply(&Adjustment::GradientMap { stops: stops.clone(), reverse: false, dither: false, ink_stops: Vec::new() }, &mut b);
            assert!((b.px[0][0] - t).abs() < 1e-5, "{c:?}: {}", b.px[0][0]);
        }
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

    /// `adj` applied in a Lab document to one encoded sRGB colour, returned as (L, a, b).
    fn in_lab(adj: &Adjustment, c: [f32; 3]) -> [f32; 3] {
        let mut b = Buffer::filled(crate::Rect::new(0, 0, 1, 1), [c[0], c[1], c[2], 1.0]);
        apply_in(adj, &mut b, Transfer::Srgb, Some(SampleType::U8), ColorMode::Lab);
        srgb_to_lab([b.px[0][0], b.px[0][1], b.px[0][2]])
    }

    /// A muted, in-gamut colour from (L, a, b).
    fn lab(l: f32, a: f32, b: f32) -> [f32; 3] {
        lab_to_srgb([l, a, b])
    }

    fn near(got: [f32; 3], want: [f32; 3], tol: f32) -> bool {
        got.iter().zip(want).all(|(g, w)| (g - w).abs() <= tol)
    }

    #[test]
    fn lab_invert_flips_l_a_and_b() {
        let got = in_lab(&Adjustment::Invert, lab(30.0, 12.0, -18.0));
        // n -> 1 - n: L -> 100 - L, a -> -a - 1, b -> -b - 1 (mirrored on the 0..255 grid).
        assert!(near(got, [70.0, -13.0, 17.0], 0.6), "{got:?}");
        // On the screen colour it would not be a mirror in Lab.
        let mut rgb = Buffer::filled(crate::Rect::new(0, 0, 1, 1), [0.0, 0.0, 0.0, 1.0]);
        let c = lab(30.0, 12.0, -18.0);
        rgb.px[0] = [c[0], c[1], c[2], 1.0];
        apply(&Adjustment::Invert, &mut rgb);
        assert!(!near(srgb_to_lab([rgb.px[0][0], rgb.px[0][1], rgb.px[0][2]]), got, 1.0));
    }

    #[test]
    fn sixteen_bit_lab_invert_keeps_greys_neutral() {
        // 16-bit Lab Invert mirrors a and b around neutral (photoshop corpus lab16 invert.psd);
        // 8-bit bytes land one step off it.
        let mut b = Buffer::filled(crate::Rect::new(0, 0, 1, 1), [0.0, 0.0, 0.0, 1.0]);
        let c = lab(40.0, 0.0, 0.0);
        b.px[0] = [c[0], c[1], c[2], 1.0];
        apply_in(&Adjustment::Invert, &mut b, Transfer::Srgb, Some(SampleType::U16), ColorMode::Lab);
        let got = srgb_to_lab([b.px[0][0], b.px[0][1], b.px[0][2]]);
        assert!(near(got, [60.0, 0.0, 0.0], 0.05), "{got:?}");
        assert!(near(in_lab(&Adjustment::Invert, c), [60.0, -1.0, -1.0], 0.1));
    }

    /// `adj` applied in a CMYK document to one encoded sRGB colour.
    fn in_cmyk(adj: &Adjustment, c: [f32; 3]) -> [f32; 3] {
        let mut b = Buffer::filled(crate::Rect::new(0, 0, 1, 1), [c[0], c[1], c[2], 1.0]);
        apply_in(adj, &mut b, Transfer::Srgb, Some(SampleType::U8), ColorMode::Cmyk);
        [b.px[0][0], b.px[0][1], b.px[0][2]]
    }

    #[test]
    fn cmyk_invert_posterize_and_threshold_work_on_the_inks() {
        use photosplice_color::convert::{cmyk_to_rgb, rgb_to_cmyk};
        // Paper inverts to all four inks at 100%, not to RGB black (photoshop corpus cmyk8 invert.psd).
        assert!(near(in_cmyk(&Adjustment::Invert, [1.0; 3]), cmyk_to_rgb([1.0; 4]), 2e-3));
        let red = [0.9, 0.2, 0.1];
        assert!(near(in_cmyk(&Adjustment::Invert, red), cmyk_to_rgb(rgb_to_cmyk(red).map(|v| 1.0 - v)), 2e-3));
        let two = cmyk_to_rgb(rgb_to_cmyk(red).map(|v| if v >= 0.5 { 1.0 } else { 0.0 }));
        assert!(near(in_cmyk(&Adjustment::Posterize { levels: 2 }, red), two, 2e-3));
        // Threshold's black is the black ink alone (threshold-128.psd), its white the paper.
        let t = Adjustment::Threshold { level: 0.5 };
        assert!(near(in_cmyk(&t, [0.2; 3]), cmyk_to_rgb([0.0, 0.0, 0.0, 1.0]), 2e-3));
        assert!(near(in_cmyk(&t, [0.8; 3]), cmyk_to_rgb([0.0; 4]), 2e-3));
    }

    /// CMYK posterizes the stored value 1 - ink: with 3 levels an ink of 85 (stored 170) lands on
    /// the middle step, 128 (stored 127), where posterizing the ink would drop it to none (psd-tools
    /// posterize_cmyk.psd, levels 3 to 21, exact so).
    #[test]
    fn cmyk_posterize_works_on_the_stored_values() {
        let mut ink = [85.0 / 255.0; 4];
        stored_channels(&Adjustment::Posterize { levels: 3 }, ColorMode::Cmyk, Some(SampleType::U8), &mut ink);
        assert!(ink.iter().all(|v| (v * 255.0 - 128.0).abs() < 1e-3), "{ink:?}");
        assert_eq!(posterize(85.0 / 255.0, 3), 0.0, "posterizing the ink itself");
    }

    #[test]
    fn cmyk_threshold_compares_the_inks_plain_luma() {
        assert_eq!(ink_luma([0.0; 4]), 1.0);
        assert_eq!(ink_luma([0.0, 0.0, 0.0, 1.0]), 0.0);
        // Two greys either side of level 128 in photoshop corpus cmyk8 threshold-128.psd: black,
        // then white (8-bit inks).
        let at = |i: [f32; 4]| {
            let mut ch = i.map(|v| v / 255.0);
            stored_channels(&Adjustment::Threshold { level: 128.0 / 255.0 }, ColorMode::Cmyk, Some(SampleType::U8), &mut ch);
            ch
        };
        assert_eq!(at([144.0, 122.0, 121.0, 35.0]), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(at([131.0, 110.0, 110.0, 19.0]), [0.0; 4]);
    }

    #[test]
    fn cmyk_brightness_contrast_curves_each_ink_from_paper() {
        use photosplice_color::convert::{cmyk_to_rgb, rgb_to_cmyk};
        // The tone curve runs on each ink's stored value, 1 - ink (photoshop corpus cmyk8
        // brightness-contrast.psd and -legacy.psd): raising brightness takes ink away, and paper
        // and solid ink stay put.
        let red = [0.9, 0.2, 0.1];
        for legacy in [false, true] {
            let adj = Adjustment::BrightnessContrast { brightness: 25.0, contrast: 30.0, legacy };
            let table = ink_brightness_contrast_lut(25.0, 30.0, legacy);
            let want = cmyk_to_rgb(rgb_to_cmyk(red).map(|v| 1.0 - lut(&table, 1.0 - v)));
            assert!(near(in_cmyk(&adj, red), want, 2e-3), "legacy {legacy}");
            assert!(near(in_cmyk(&adj, [1.0; 3]), [1.0; 3], 2e-3), "legacy {legacy}: paper");
            let solid = cmyk_to_rgb([1.0; 4]);
            assert!(near(in_cmyk(&adj, solid), cmyk_to_rgb(rgb_to_cmyk(solid).map(|v| ink_through(&table, v))), 2e-3));
        }
        // Legacy +25 / +30 on Photoshop's own 16-bit inks (photoshop corpus cmyk16
        // brightness-contrast-legacy.psd): contrast pivots at 128 in ink, 127 stored.
        let legacy = ink_brightness_contrast_lut(25.0, 30.0, true);
        for (ink, ps) in [(139.0, 108.0), (160.0, 137.99), (172.0, 155.13), (183.0, 170.85), (196.0, 189.42), (98.0, 49.42), (128.0, 92.28), (168.0, 149.42)] {
            let got = ink_through(&legacy, ink / 255.0) * 255.0;
            assert!((got - ps).abs() < 0.1, "ink {ink}: {got} vs Photoshop {ps}");
        }
        let brighter = Adjustment::BrightnessContrast { brightness: 60.0, contrast: 0.0, legacy: false };
        let ink = ink_brightness_contrast_lut(60.0, 0.0, false);
        assert!(ink_through(&ink, 0.5) < 0.5 && ink_through(&ink, 0.0) == 0.0 && (ink_through(&ink, 1.0) - 1.0).abs() < 1e-6);
        // On the screen colour it would land elsewhere.
        let mut rgb = Buffer::filled(crate::Rect::new(0, 0, 1, 1), [red[0], red[1], red[2], 1.0]);
        apply(&brighter, &mut rgb);
        assert!(!near(in_cmyk(&brighter, red), [rgb.px[0][0], rgb.px[0][1], rgb.px[0][2]], 0.01));
    }

    #[test]
    fn lab_posterize_steps_l_a_and_b() {
        let got = in_lab(&Adjustment::Posterize { levels: 2 }, lab(70.0, 20.0, -30.0));
        // Two levels: each normalised channel snaps to 0 or 1 (L 100, a and b at +127 / -128).
        assert!(near(got, srgb_to_lab(lab_denorm([1.0, 1.0, 0.0])), 1e-3), "{got:?}");
        let grey = in_lab(&Adjustment::Posterize { levels: 255 }, lab(50.0, 0.0, 0.0));
        assert!(near(grey, [50.0, 0.0, 0.0], 1.1), "255 levels is within a step of the identity: {grey:?}");
    }

    #[test]
    fn lab_threshold_splits_on_l_with_a_neutral_result() {
        let t = Adjustment::Threshold { level: 0.5 };
        // A saturated blue (L ≈ 32) and yellow (L ≈ 97): black and white, nothing in between.
        for (c, white) in [([0.0, 0.0, 1.0], false), ([1.0, 1.0, 0.0], true), (lab(52.0, 40.0, 30.0), true), (lab(48.0, -40.0, 30.0), false)] {
            let mut b = Buffer::filled(crate::Rect::new(0, 0, 1, 1), [c[0], c[1], c[2], 1.0]);
            apply_in(&t, &mut b, Transfer::Srgb, None, ColorMode::Lab);
            let v = if white { 1.0 } else { 0.0 };
            assert_eq!(b.px[0], [v, v, v, 1.0], "{c:?}");
        }
    }

    #[test]
    fn lab_brightness_contrast_moves_l_only() {
        for legacy in [false, true] {
            let adj = Adjustment::BrightnessContrast { brightness: 40.0, contrast: 30.0, legacy };
            let before = [45.0, 15.0, -20.0];
            let got = in_lab(&adj, lab(before[0], before[1], before[2]));
            assert!(got[0] > before[0] + 5.0, "{legacy}: L rises: {got:?}");
            assert!((got[1] - before[1]).abs() < 0.5 && (got[2] - before[2]).abs() < 0.5, "{legacy}: a, b kept: {got:?}");
        }
    }

    #[test]
    fn lab_gradient_map_follows_l() {
        let stops = vec![(0.0, [1.0, 0.0, 0.0]), (1.0, [0.0, 0.0, 1.0])];
        for reverse in [false, true] {
            let map = Adjustment::GradientMap { stops: stops.clone(), reverse, dither: false, ink_stops: Vec::new() };
            // A grey and a saturated colour with the same L map to the same colour.
            let grey = in_lab(&map, lab(60.0, 0.0, 0.0));
            let green = in_lab(&map, lab(60.0, -50.0, 40.0));
            assert!(near(grey, green, 1e-3), "{reverse}: {grey:?} vs {green:?}");
            let t = if reverse { 0.4 } else { 0.6 };
            let want = srgb_to_lab([1.0 - t, 0.0, t]);
            assert!(near(grey, want, 0.05), "{reverse}: {grey:?} vs {want:?}");
        }
    }

    #[test]
    fn other_modes_and_adjustments_keep_the_screen_colour_path() {
        let c = [0.2, 0.5, 0.7, 1.0];
        let lab_only = [Adjustment::Invert, Adjustment::Threshold { level: 0.47 }, Adjustment::Posterize { levels: 4 }];
        for adj in
            lab_only.iter().chain([&Adjustment::HueSaturation { hue: 30.0, saturation: 0.0, lightness: 0.0, colorize: false, ranges: HueRange::defaults() }])
        {
            for mode in [ColorMode::Rgb, ColorMode::Cmyk, ColorMode::Grayscale, ColorMode::Lab] {
                let (mut a, mut b) = (Buffer::filled(crate::Rect::new(0, 0, 1, 1), c), Buffer::filled(crate::Rect::new(0, 0, 1, 1), c));
                apply_in(adj, &mut a, Transfer::Srgb, None, mode);
                apply_depth(adj, &mut b, Transfer::Srgb, None);
                let own = (mode == ColorMode::Lab && lab_channel_adjustment(adj)) || (mode == ColorMode::Cmyk && cmyk_channel_adjustment(adj));
                assert_eq!(a.px == b.px, !own, "{adj:?} in {mode:?}");
            }
        }
        // Transparent pixels are left alone and odd values never panic.
        let mut t = Buffer::filled(crate::Rect::new(0, 0, 2, 1), [f32::NAN, 2.0, -1.0, 0.0]);
        for adj in lab_only.iter().chain([&Adjustment::GradientMap { stops: vec![], reverse: true, dither: true, ink_stops: Vec::new() }]) {
            for mode in [ColorMode::Lab, ColorMode::Cmyk] {
                apply_in(adj, &mut t, Transfer::Srgb, None, mode);
                let mut odd = Buffer::filled(crate::Rect::new(0, 0, 1, 1), [f32::NAN, f32::INFINITY, -3.0, 1.0]);
                apply_in(adj, &mut odd, Transfer::Srgb, Some(SampleType::U16), mode);
            }
        }
        assert!(t.px[0][1] == 2.0);
    }
}

/// Legacy Brightness/Contrast scales contrast around 128 of 255 (contrast 80 keeps 128 at 128,
/// measured on Photoshop 27.11; a 0.5 pivot moved it to 130).
pub const LEGACY_PIVOT: f32 = 128.0 / 255.0;

#[cfg(test)]
mod lut3d_guard_tests {
    use super::lut3d_sample;

    #[test]
    fn degenerate_or_short_tables_leave_the_colour_alone() {
        let c = [0.2, 0.4, 0.6];
        for tetra in [false, true] {
            assert_eq!(lut3d_sample(&[], 0, c, tetra), c);
            assert_eq!(lut3d_sample(&[0.0; 3], 1, c, tetra), c);
            assert_eq!(lut3d_sample(&[0.0; 3], 2, c, tetra), c);
            assert_eq!(lut3d_sample(&[0.0; 24], usize::MAX, c, tetra), c);
        }
    }
}
