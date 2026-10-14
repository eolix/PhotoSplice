//! Filter Gallery › Distort, Stylize and Texture.

#[path = "texture_distort.rs"]
mod distort;
#[path = "texture_grain.rs"]
mod grain;
#[path = "texture_grid.rs"]
mod grid;
#[path = "texture_stained.rs"]
mod stained;
#[path = "texture_texturizer.rs"]
mod texturizer;

use super::Render;
use super::px8::{self, Img};
use crate::artistic::{GalleryEffect, GalleryFilter as F};

/// The renderer of a filter this module implements.
pub(super) fn render(f: F) -> Option<Render> {
    let r: Render = match f {
        F::DiffuseGlow => |e, i| guard(i, |i| distort::diffuse_glow(e, i)),
        F::Glass => |e, i| guard(i, |i| distort::glass(e, i)),
        F::OceanRipple => |e, i| guard(i, |i| distort::ocean_ripple(e, i)),
        F::GlowingEdges => |e, i| guard(i, |i| distort::glowing_edges(e, i)),
        F::Craquelure => |e, i| guard(i, |i| grid::craquelure(e, i)),
        F::Grain => |e, i| guard(i, |i| grain::grain(e, i)),
        F::MosaicTiles => |e, i| guard(i, |i| grid::mosaic_tiles(e, i)),
        F::Patchwork => |e, i| guard(i, |i| grid::patchwork(e, i)),
        F::StainedGlass => |e, i| guard(i, |i| stained::stained_glass(e, i)),
        F::Texturizer => |e, i| guard(i, |i| texturizer::texturizer(e, i)),
        _ => return None,
    };
    Some(r)
}

/// Runs `f` on a non-empty image; an empty one comes back as it is.
fn guard(img: &Img, f: impl Fn(&Img) -> Img) -> Img {
    if img.n() == 0 { img.clone() } else { f(img) }
}

/// A numeric parameter clamped to `lo..=hi` (NaN → `lo`).
fn param(e: &GalleryEffect, key: &str, lo: f64, hi: f64) -> f64 {
    let v = e.get(key) as f64;
    if v.is_nan() { lo } else { v.clamp(lo, hi) }
}

/// A table index from a parameter clamped to `0..len`.
fn index(e: &GalleryEffect, key: &str, len: usize) -> usize {
    param(e, key, 0.0, len.saturating_sub(1) as f64).round() as usize
}

/// The gallery RNG seeded the usual way (`FlRs >>> 1`).
fn rng(e: &GalleryEffect) -> px8::Rng {
    px8::Rng::new(e.seed() >> 1)
}

/// `img` with its byte-HSV value replaced by `v` (P-08 round trip); alpha kept.
fn with_value(img: &Img, v: &[u8]) -> Img {
    let mut out = img.clone();
    px8::to_hsv(&mut out);
    out.set_plane(2, v);
    px8::from_hsv(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::super::px8::Img;
    use crate::artistic::{GalleryEffect, GalleryFilter as F};

    const MINE: [F; 10] =
        [F::DiffuseGlow, F::Glass, F::OceanRipple, F::GlowingEdges, F::Craquelure, F::Grain, F::MosaicTiles, F::Patchwork, F::StainedGlass, F::Texturizer];

    fn image(w: usize, h: usize) -> Img {
        let mut img = Img::new(w, h);
        for (i, p) in img.d.chunks_exact_mut(4).enumerate() {
            p.copy_from_slice(&[(i * 37 % 256) as u8, (i * 11 % 256) as u8, (i * 5 % 256) as u8, (i * 3 % 256) as u8]);
        }
        img
    }

    /// Every effect variant this module renders, with its parameters at the defaults.
    fn variants() -> Vec<GalleryEffect> {
        let mut out = Vec::new();
        for f in MINE {
            let e = GalleryEffect::new(f);
            for p in f.params() {
                if let crate::artistic::GalleryParamKind::Choice(c) = p.kind {
                    for name in c {
                        let mut v = e.clone();
                        v.set_choice(p.key, name);
                        out.push(v);
                    }
                }
            }
            out.push(e);
        }
        out
    }

    #[test]
    fn tiny_images_and_wild_parameters_do_not_panic() {
        for e in variants() {
            let r = super::render(e.filter).unwrap();
            for (w, h) in [(0, 0), (0, 3), (3, 0), (1, 1), (2, 2), (3, 1), (1, 5), (17, 9)] {
                let img = image(w, h);
                let out = r(&e, &img);
                assert_eq!((out.w, out.h, out.d.len()), (w, h, img.d.len()), "{}", e.filter.key());
                assert!(out.d.chunks_exact(4).zip(img.d.chunks_exact(4)).all(|(a, b)| a[3] == b[3]), "{} alpha", e.filter.key());
                for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1e9, 1e9] {
                    let mut wild = e.clone();
                    wild.values.values_mut().for_each(|x| *x = v);
                    wild.seed = Some(u32::MAX);
                    let _ = r(&wild, &img);
                }
            }
        }
    }

    #[test]
    fn grain_types_differ() {
        let img = image(40, 30);
        let r = super::render(F::Grain).unwrap();
        let outs: Vec<Img> = variants().into_iter().filter(|e| e.filter == F::Grain).map(|e| r(&e, &img)).collect();
        assert_eq!(outs.len(), 11);
        for i in 0..10 {
            for j in i + 1..10 {
                assert_ne!(outs[i], outs[j], "grain types {i} and {j} look the same");
            }
        }
    }

    /// Times every filter on a 24 MP image.
    #[test]
    #[ignore = "slow"]
    fn timing_24mp() {
        let img = image(6000, 4000);
        for f in MINE {
            let r = super::render(f).unwrap();
            let mut e = GalleryEffect::new(f);
            for t in if f == F::Grain { vec!["regular", "soft", "clumped", "vertical"] } else { vec![""] } {
                if !t.is_empty() {
                    e.set_choice("grainType", t);
                }
                let s = std::time::Instant::now();
                let _ = r(&e, &img);
                eprintln!("{:<14} {t:<10} {:>6} ms", f.key(), s.elapsed().as_millis());
            }
        }
    }
}
