//! Filter Gallery › Sketch (foreground = ink, background = paper).

use super::Render;
use super::px8;
use crate::artistic::GalleryEffect;
use crate::artistic::GalleryFilter as F;

#[path = "sketch_pattern.rs"]
mod pattern;
#[path = "sketch_relief.rs"]
mod relief;
#[path = "sketch_tone.rs"]
mod tone;

/// The renderer of a filter this module implements.
pub(super) fn render(f: F) -> Option<Render> {
    Some(match f {
        F::BasRelief => relief::bas_relief,
        F::ChalkCharcoal => tone::chalk_charcoal,
        F::Charcoal => tone::charcoal,
        F::Chrome => relief::chrome,
        F::ConteCrayon => relief::conte_crayon,
        F::GraphicPen => pattern::graphic_pen,
        F::HalftonePattern => pattern::halftone,
        F::NotePaper => relief::note_paper,
        F::Photocopy => tone::photocopy,
        F::Plaster => relief::plaster,
        F::Reticulation => tone::reticulation,
        F::Stamp => tone::stamp,
        F::TornEdges => tone::torn_edges,
        F::WaterPaper => pattern::water_paper,
        _ => return None,
    })
}

/// Light directions in the order the kernels index them (bottom = 0, then clockwise).
const LIGHTS: [&str; 8] = ["bottom", "bottomLeft", "left", "topLeft", "top", "topRight", "right", "bottomRight"];

/// The kernel index of a light choice, by name.
fn light_index(name: &str) -> usize {
    LIGHTS.iter().position(|n| *n == name).unwrap_or(0)
}

/// Foreground (ink) and background (paper) as 8-bit colours.
fn ink_paper(e: &GalleryEffect) -> ([u8; 3], [u8; 3]) {
    (px8::rgb8(e.foreground), px8::rgb8(e.background))
}

/// A slider value as a non-negative integer.
fn int(e: &GalleryEffect, key: &str) -> usize {
    let v = e.get(key);
    if v.is_finite() { v.max(0.0) as usize } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::super::{oracle, px8};
    use crate::artistic::{GalleryEffect, GalleryFilter as F, GalleryParamKind};

    #[test]
    fn tiny_images_and_wild_parameters_never_panic() {
        let sizes = [(0, 0), (0, 3), (3, 0), (1, 1), (2, 2), (1, 7), (7, 1), (5, 3)];
        let wild = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1e9, 1e9, -1.0, 0.0];
        for f in F::ALL.iter().copied().filter(|f| super::render(*f).is_some()) {
            let Some(run) = super::render(f) else { continue };
            for (w, h) in sizes {
                let mut img = px8::Img::new(w, h);
                for (i, v) in img.d.iter_mut().enumerate() {
                    *v = (i * 97 % 256) as u8;
                }
                let mut cases = vec![GalleryEffect::new(f)];
                for p in f.params() {
                    let vals: Vec<f32> = match p.kind {
                        GalleryParamKind::Range { min, max, .. } => [min, max].into_iter().chain(wild).collect(),
                        _ => vec![-1.0, 99.0, f32::NAN],
                    };
                    for v in vals {
                        let mut e = GalleryEffect::new(f);
                        e.values.insert(p.key.to_string(), v);
                        e.seed = Some(u32::MAX);
                        cases.push(e);
                    }
                }
                for e in &cases {
                    let out = run(e, &img);
                    assert_eq!((out.w, out.h, out.d.len()), (w, h, w * h * 4), "{} {w}x{h}", f.key());
                    let alpha_kept = out.d.chunks_exact(4).zip(img.d.chunks_exact(4)).all(|(a, b)| a[3] == b[3]);
                    assert!(alpha_kept, "{} {w}x{h} changed alpha", f.key());
                }
            }
        }
    }

    /// How far two seeds of the same filter are apart: the floor of a comparison with
    /// Photoshop, whose random numbers can't be matched.
    #[test]
    #[ignore = "needs GALLERY_PS"]
    fn seed_noise_floor() {
        let Ok(dir) = std::env::var("GALLERY_PS") else { return };
        let (w, h, src) = oracle::read_ppm(&std::path::Path::new(&dir).join("src-parrot.ppm"));
        for f in [F::ChalkCharcoal, F::GraphicPen, F::NotePaper, F::Reticulation, F::TornEdges, F::WaterPaper] {
            let a = oracle::render(&GalleryEffect::new(f), w, h, &src);
            let mut e = GalleryEffect::new(f);
            e.seed = Some(e.seed().wrapping_add(12_345_678));
            let b = oracle::render(&e, w, h, &src);
            let (px, bl) = oracle::diff(&a, &b, w, h);
            eprintln!("{:<20} {px:>8.2} {bl:>8.2}", f.key());
        }
    }
}
