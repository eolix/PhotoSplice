//! The colour space a saved page paints in: the document's own when PDF has it (RGB, Grayscale,
//! CMYK), so a CMYK document's shapes and type stay CMYK vectors and its images CMYK images.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use krilla::color::{Color as KColor, cmyk, luma, rgb};
use krilla::image::{BitsPerComponent, CustomImage, ImageColorspace};
use photosplice_color::convert::CmykSpace;
use photosplice_color::{Color, ColorMode, PixelFormat};
use photosplice_doc::Document;

/// A page's colour space.
#[derive(Clone)]
pub(super) enum Ink {
    Rgb,
    Gray,
    /// With the document's CMYK profile (`None`: the built-in one).
    Cmyk(Option<Arc<CmykSpace>>),
}

impl Ink {
    /// The ink of `doc`'s mode; `None` for modes PDF has no space for here (Lab, Indexed, …).
    pub(super) fn of(doc: &Document) -> Option<Ink> {
        match doc.mode {
            ColorMode::Rgb => Some(Ink::Rgb),
            ColorMode::Grayscale => Some(Ink::Gray),
            ColorMode::Cmyk => Some(Ink::Cmyk(photosplice_compose::cmyk_space(doc))),
            _ => None,
        }
    }

    /// `c` in this ink: its own values when it is in the same mode, else converted (through
    /// RGB, with the document's CMYK profile).
    pub(super) fn color(&self, c: &Color) -> KColor {
        match (self, c.mode) {
            (Ink::Cmyk(_), ColorMode::Cmyk) => cmyk::Color::new(u8c(c.c[0]), u8c(c.c[1]), u8c(c.c[2]), u8c(c.c[3])).into(),
            (Ink::Gray, ColorMode::Grayscale) => luma::Color::new(u8c(c.c[0])).into(),
            (Ink::Cmyk(space), _) => {
                let rgb = photosplice_color::convert::with_cmyk_space(space.as_ref(), || c.to_rgb());
                self.rgb(rgb)
            }
            _ => self.rgb(c.to_rgb()),
        }
    }

    /// The sRGB colour `rgb` in this ink.
    pub(super) fn rgb(&self, rgb: [f32; 3]) -> KColor {
        match self {
            Ink::Rgb => rgb::Color::new(u8c(rgb[0]), u8c(rgb[1]), u8c(rgb[2])).into(),
            Ink::Gray => {
                let mut g = [0.0f32; 2];
                photosplice_raster::from_rgba_into(&PixelFormat::GRAYA8, [rgb[0], rgb[1], rgb[2], 1.0], &mut g);
                luma::Color::new(u8c(g[0])).into()
            }
            Ink::Cmyk(space) => {
                let k = photosplice_color::convert::with_cmyk_space(space.as_ref(), || photosplice_color::convert::rgb_to_cmyk(rgb));
                cmyk::Color::new(u8c(k[0]), u8c(k[1]), u8c(k[2]), u8c(k[3])).into()
            }
        }
    }

    /// The pixel format a layer image is composited into for this ink (with alpha).
    pub(super) fn format(&self) -> PixelFormat {
        match self {
            Ink::Rgb => PixelFormat::RGBA8,
            Ink::Gray => PixelFormat::GRAYA8,
            Ink::Cmyk(_) => PixelFormat::CMYKA8,
        }
    }
}

fn u8c(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Grey or CMYK pixels for krilla: the colour channels, the alpha, and the document's profile.
#[derive(Clone)]
pub(super) struct InkImage {
    pub color: Arc<Vec<u8>>,
    pub alpha: Option<Arc<Vec<u8>>>,
    pub size: (u32, u32),
    pub space: ImageColorspace,
    pub icc: Option<Arc<Vec<u8>>>,
    /// A hash of the pixels, computed once (krilla hashes the image often).
    pub key: u64,
}

impl InkImage {
    pub(super) fn new(color: Vec<u8>, alpha: Option<Vec<u8>>, size: (u32, u32), space: ImageColorspace, icc: Option<Arc<Vec<u8>>>) -> Self {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        color.hash(&mut h);
        alpha.hash(&mut h);
        size.hash(&mut h);
        InkImage { color: Arc::new(color), alpha: alpha.map(Arc::new), size, space, icc, key: h.finish() }
    }
}

impl Hash for InkImage {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key.hash(state);
        self.icc.as_ref().map(|p| p.len()).hash(state);
    }
}

impl CustomImage for InkImage {
    fn color_channel(&self) -> &[u8] {
        &self.color
    }
    fn alpha_channel(&self) -> Option<&[u8]> {
        self.alpha.as_deref().map(Vec::as_slice)
    }
    fn bits_per_component(&self) -> BitsPerComponent {
        BitsPerComponent::Eight
    }
    fn size(&self) -> (u32, u32) {
        self.size
    }
    fn icc_profile(&self) -> Option<&[u8]> {
        self.icc.as_deref().map(Vec::as_slice)
    }
    fn color_space(&self) -> ImageColorspace {
        self.space
    }
}
