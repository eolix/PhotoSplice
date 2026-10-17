//! Alpha / spot channel options <-> PSD `DisplayInfo` image resources (1077, and the older 1007).
//!
//! Layout (Adobe PSD spec, "DisplayInfo"): resource 1077 starts with a 4-byte version (1), then
//! one 13-byte record per extra channel: colour space (u16), four u16 colour components, opacity
//! (u16, 0..100) and kind (u8: 0 = colour indicates selected areas, 1 = masked ("protected")
//! areas, 2 = spot channel), with no padding (Photoshop's files: a 14-byte reading misread every
//! channel after the first). Resource 1007 has 14-byte records (a padding byte), no version and
//! no spot kind. Photoshop lists the merged transparency first.

use photosplice_color::{Color, ColorMode};
use photosplice_doc::{AlphaChannel, ColorIndicates};

pub(crate) const DISPLAY_INFO: u16 = 1077;
pub(crate) const DISPLAY_INFO_OLD: u16 = 1007;
/// Quick Mask information: channel id (u16) and an "initially empty" flag (u8).
pub(crate) const QUICK_MASK_INFO: u16 = 1022;

const SPACE_RGB: u16 = 0;
const SPACE_HSB: u16 = 1;
const SPACE_CMYK: u16 = 2;
const SPACE_LAB: u16 = 7;
const SPACE_GRAY: u16 = 8;

/// Resource 1077 for `channels` (in file order), after a record for the merged transparency when
/// `transparency` (Photoshop writes it as red, 100 %, masked areas).
pub(crate) fn display_info(transparency: bool, channels: &[&AlphaChannel]) -> Vec<u8> {
    let mut out = 1u32.to_be_bytes().to_vec();
    let mut record = |color: &Color, opacity: f32, kind: u8| {
        let (space, comps) = encode_color(color);
        out.extend_from_slice(&space.to_be_bytes());
        for v in comps {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out.extend_from_slice(&((opacity.clamp(0.0, 1.0) * 100.0).round() as u16).to_be_bytes());
        out.push(kind);
    };
    if transparency {
        record(&Color::rgb(1.0, 0.0, 0.0), 1.0, 1);
    }
    for c in channels {
        match c.spot {
            Some((ink, solidity)) => record(&ink, solidity, 2),
            None => record(&c.color, c.opacity, if c.indicates == ColorIndicates::SelectedAreas { 0 } else { 1 }),
        }
    }
    out
}

/// Resource 1007, the older DisplayInfo: the records of [`display_info`] without its version,
/// each padded to 14 bytes.
pub(crate) fn display_info_old(transparency: bool, channels: &[&AlphaChannel]) -> Vec<u8> {
    let new = display_info(transparency, channels);
    new.get(4..).unwrap_or_default().chunks(13).flat_map(|r| r.iter().copied().chain([0])).collect()
}

/// A colour as a Photoshop colour structure in its own model (inverse of [`decode_color`]): spot
/// inks defined in CMYK or Lab keep that definition.
pub(crate) fn encode_color(c: &Color) -> (u16, [u16; 4]) {
    let w = |v: f32| (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
    let hundredths = |v: f32, lo: f32, hi: f32| (v.clamp(lo, hi) * 100.0).round() as i16 as u16;
    match c.mode {
        ColorMode::Cmyk => (SPACE_CMYK, [w(1.0 - c.c[0]), w(1.0 - c.c[1]), w(1.0 - c.c[2]), w(1.0 - c.c[3])]),
        ColorMode::Lab => {
            let (a, b) = (c.c[1] * 255.0 - 128.0, c.c[2] * 255.0 - 128.0);
            (SPACE_LAB, [(c.c[0].clamp(0.0, 1.0) * 10000.0).round() as u16, hundredths(a, -128.0, 127.0), hundredths(b, -128.0, 127.0), 0])
        }
        ColorMode::Grayscale => (SPACE_GRAY, [((1.0 - c.c[0].clamp(0.0, 1.0)) * 10000.0).round() as u16, 0, 0, 0]),
        _ => {
            let [r, g, b] = c.to_rgb();
            (SPACE_RGB, [w(r), w(g), w(b), 0])
        }
    }
}

/// Applies a DisplayInfo resource (`versioned` = 1077) to the imported channels in order, after
/// the first `skip` records (the merged transparency's).
pub(crate) fn apply_display_info(data: &[u8], versioned: bool, skip: usize, channels: &mut [AlphaChannel]) {
    let body = if versioned { data.get(4..).unwrap_or(&[]) } else { data };
    let size = if versioned { 13 } else { 14 };
    for (c, rec) in channels.iter_mut().zip(body.chunks_exact(size).skip(skip)) {
        let u = |i: usize| u16::from_be_bytes([rec[i], rec[i + 1]]);
        let comps = [u(2), u(4), u(6), u(8)];
        let color = decode_color(u(0), comps);
        let opacity = f32::from(u(10).min(100)) / 100.0;
        match rec[12] {
            2 => c.spot = Some((color, opacity)),
            k => {
                // An alpha-type channel (a Multichannel import starts every channel as an ink;
                // Photoshop shows these as "Masked / Selected Areas" channels and saves them so).
                c.spot = None;
                c.color = color;
                c.opacity = opacity;
                c.indicates = if k == 0 { ColorIndicates::SelectedAreas } else { ColorIndicates::MaskedAreas };
            }
        }
    }
}

/// A PSD colour structure (space id + four u16 components) as a [`Color`]; unknown spaces
/// (book colours…) read the first three components as RGB. See [`decode_known_color`].
pub(crate) fn decode_color(space: u16, c: [u16; 4]) -> Color {
    let n = |v: u16| f32::from(v) / 65535.0;
    decode_known_color(space, c).unwrap_or_else(|| Color::rgb(n(c[0]), n(c[1]), n(c[2])))
}

/// A PSD colour structure in one of the spaces Photoshop writes for plain colours (0 RGB,
/// 1 HSB, 2 CMYK, 7 Lab, 8 Grayscale) as a [`Color`], or `None` for any other space id. CMYK,
/// Lab and Grayscale stay in their own model (so a spot ink is written back as it was defined);
/// `to_rgb` converts them (CMYK through the active document CMYK profile, see
/// `photosplice_color::convert::with_cmyk_space`).
pub(crate) fn decode_known_color(space: u16, c: [u16; 4]) -> Option<Color> {
    let n = |v: u16| f32::from(v) / 65535.0;
    Some(match space {
        SPACE_RGB => Color::rgb(n(c[0]), n(c[1]), n(c[2])),
        SPACE_HSB => {
            let rgb = hsb_to_rgb(n(c[0]) * 360.0, n(c[1]), n(c[2]));
            Color::rgb(rgb[0], rgb[1], rgb[2])
        }
        // Photoshop colour structures store CMYK inverted (65535 = no ink).
        // Kept in their own model, so a CMYK or Lab spot ink is written back as it was defined.
        SPACE_CMYK => Color { mode: ColorMode::Cmyk, c: [1.0 - n(c[0]), 1.0 - n(c[1]), 1.0 - n(c[2]), 1.0 - n(c[3])], alpha: 1.0 },
        SPACE_LAB => {
            let l = f32::from(c[0].min(10000)) / 10000.0;
            let a = f32::from(c[1] as i16) / 100.0;
            let b = f32::from(c[2] as i16) / 100.0;
            Color { mode: ColorMode::Lab, c: [l, (a + 128.0) / 255.0, (b + 128.0) / 255.0, 0.0], alpha: 1.0 }
        }
        SPACE_GRAY => Color::gray(1.0 - f32::from(c[0].min(10000)) / 10000.0),
        _ => return None,
    })
}

/// HSB (hue in degrees, saturation and brightness 0..1) → RGB.
fn hsb_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h = if h.is_finite() { h.rem_euclid(360.0) / 60.0 } else { 0.0 };
    let (s, v) = (s.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
    let f = h - h.floor();
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match h.floor() as u8 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photosplice_color::PixelFormat;
    use photosplice_raster::Surface;

    #[test]
    fn display_info_roundtrip() {
        let mut a = AlphaChannel::new("A", Surface::new(PixelFormat::GRAY8));
        a.color = Color::rgb(0.0, 1.0, 0.0);
        a.opacity = 0.3;
        a.indicates = ColorIndicates::SelectedAreas;
        let mut s = AlphaChannel::new("S", Surface::new(PixelFormat::GRAY8));
        s.spot = Some((Color::rgb(1.0, 0.0, 1.0), 0.8));
        let bytes = display_info(false, &[&a, &s]);
        assert_eq!(bytes.len(), 4 + 26);
        let mut back = vec![AlphaChannel::new("A", Surface::new(PixelFormat::GRAY8)), AlphaChannel::new("S", Surface::new(PixelFormat::GRAY8))];
        apply_display_info(&bytes, true, 0, &mut back);
        assert_eq!(back[0].color, a.color);
        assert!((back[0].opacity - 0.3).abs() < 1e-6);
        assert_eq!(back[0].indicates, ColorIndicates::SelectedAreas);
        assert_eq!(back[1].spot, s.spot);
        // With the merged transparency's record first, the channels still get their own.
        let with_t = display_info(true, &[&a, &s]);
        assert_eq!(with_t.len(), 4 + 3 * 13);
        let mut back = vec![AlphaChannel::new("A", Surface::new(PixelFormat::GRAY8)), AlphaChannel::new("S", Surface::new(PixelFormat::GRAY8))];
        apply_display_info(&with_t, true, 1, &mut back);
        assert_eq!(back[0].indicates, ColorIndicates::SelectedAreas);
        assert_eq!(back[1].spot, s.spot);
        // Truncated data is ignored rather than misread.
        let mut c = vec![AlphaChannel::new("x", Surface::new(PixelFormat::GRAY8))];
        apply_display_info(&bytes[..10], true, 0, &mut c);
        assert_eq!(c[0].opacity, 0.5);
    }

    /// Resource 1077 as Photoshop wrote it (psd-tools stroke-without-vector-mask.psd): the merged
    /// transparency (red, 100 %, masked areas), then "Alpha 1" (red, 50 %), 13 bytes each.
    #[test]
    fn photoshop_records_are_13_bytes_after_the_transparency() {
        let ps: &[u8] = &[
            0, 0, 0, 1, //
            0, 0, 0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0, 100, 1, //
            0, 0, 0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0, 50, 1,
        ];
        let mut c = vec![AlphaChannel::new("Alpha 1", Surface::new(PixelFormat::GRAY8))];
        c[0].opacity = 0.9;
        apply_display_info(ps, true, 1, &mut c);
        assert_eq!((c[0].opacity, c[0].color.to_rgb()), (0.5, [1.0, 0.0, 0.0]));
        let a = AlphaChannel::new("Alpha 1", Surface::new(PixelFormat::GRAY8));
        assert_eq!(display_info(true, &[&a]), ps, "written back byte for byte");
    }

    #[test]
    fn spot_inks_keep_their_colour_model() {
        let cmyk = Color { mode: ColorMode::Cmyk, c: [0.1, 0.8, 0.0, 0.2], alpha: 1.0 };
        let lab = Color { mode: ColorMode::Lab, c: [0.5, (40.0 + 128.0) / 255.0, (-30.0 + 128.0) / 255.0, 0.0], alpha: 1.0 };
        for ink in [cmyk, lab, Color::gray(0.25)] {
            let (space, comps) = encode_color(&ink);
            let back = decode_color(space, comps);
            assert_eq!(back.mode, ink.mode);
            assert!(back.c.iter().zip(&ink.c).all(|(a, b)| (a - b).abs() < 1e-3), "{ink:?} -> {back:?}");
        }
    }

    #[test]
    fn decodes_other_spaces() {
        let g = decode_color(SPACE_GRAY, [10000, 0, 0, 0]);
        assert_eq!(g.to_rgb(), [0.0; 3]);
        let lab = decode_color(SPACE_LAB, [10000, 0, 0, 0]).to_rgb();
        assert!(lab.iter().all(|v| *v > 0.95), "{lab:?}");
        // HSB: hue 120 degrees (65535 / 3), full saturation and brightness is pure green.
        let g = decode_color(SPACE_HSB, [21845, 65535, 65535, 0]).to_rgb();
        assert!((g[0]).abs() < 1e-3 && (g[1] - 1.0).abs() < 1e-3 && g[2].abs() < 1e-3, "{g:?}");
        let half = decode_color(SPACE_HSB, [0, 0, 32768, 0]).to_rgb();
        assert!(half.iter().all(|v| (v - 0.5).abs() < 1e-3), "{half:?}");
        // Unknown spaces (book colours) are not guessed.
        assert_eq!(decode_known_color(3, [1, 2, 3, 4]), None);
        assert_eq!(decode_known_color(SPACE_RGB, [65535, 0, 0, 0]), Some(Color::rgb(1.0, 0.0, 0.0)));
    }
}
