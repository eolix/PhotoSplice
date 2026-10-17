pub(crate) mod dds;
pub(crate) mod deep_exr;
pub(crate) mod dpx;
pub(crate) mod exr;
pub(crate) mod exr_cryptomatte;
pub(crate) mod fits;
pub(crate) mod gif_frames;
pub(crate) mod heic;
pub(crate) mod jpeg;
pub(crate) mod pcx;
pub(crate) mod png;
pub(crate) mod pnm;
pub(crate) mod tiff;
pub(crate) mod tiff_exif;
pub(crate) mod tiff_ifd;
pub(crate) mod via_image;
pub(crate) mod webp;
pub(crate) mod xpm;

/// The VP8 quantizer curve that `source_settings` reads a lossy WebP's quality back with
/// (PhotoSplice encodes lossy WebP through `webp-rust`, so only the curve is kept here).
pub(crate) mod vp8 {
    /// Quality 0..=100 → base quantizer index 0..=127 (0 is finest), the linear mapping
    /// `webp-rust` encodes with: quality 90 ≈ 13, 75 ≈ 32, 50 ≈ 64, 25 ≈ 95.
    pub fn quantizer_index(quality: u8) -> u8 {
        let q = u32::from(quality.min(100));
        let qi = ((100 - q) * 127 + 50) / 100;
        u8::try_from(qi.min(127)).unwrap_or(127)
    }
}
