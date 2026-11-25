//! An Outer Glow whose contour starts above 0 stays within its reach ([`glow_paint_lut`]).
//!
//! Reference 2022: a white Outer Glow (Normal 100 %, Range 50 %, Spread 0) over black, in 16 bits,
//! on a layer at Fill 0 % with a hard rectangle x 100..=299, y 75..=224, read up from its top edge
//! (the first pixel outside is 1 px out). With the flat contour (always 100 %):
//! - Precise, Size 4: 255 255 255 213, then nothing;
//! - Softer, Size 4: 255 255 133, then nothing;
//! - Softer, Size 10: 255 for six pixels, 213 127 64 21, then nothing.
//!
//! The flat contour used to light the whole map rectangle, every pixel the glow never reached.
//! The band's last pixels still step down differently from Reference's (ours stays solid up to
//! the reach's end); what these tests pin is the reach and the solid band inside it.

use super::*;
use photosplice_color::Color;
use photosplice_doc::FxCommon;

const W: usize = 400;

fn rectangle() -> Map {
    let mut m = Map::new(W, 300, 0.0);
    for y in 75..=224 {
        for x in 100..=299 {
            m.v[y * W + x] = 1.0;
        }
    }
    m
}

fn contour(points: &[(f32, f32)]) -> Contour {
    Contour::Custom {
        name: "kit".into(),
        points: points.iter().map(|&(input, output)| photosplice_doc::adjust::CurvePoint { input, output }).collect(),
        corners: vec![],
    }
}

const FLAT: [(f32, f32); 2] = [(0.0, 1.0), (1.0, 1.0)];
const HALF: [(f32, f32); 2] = [(0.0, 0.5), (1.0, 1.0)];

fn glow(technique: GlowTechnique, size: f32, points: &[(f32, f32)]) -> Glow {
    Glow {
        common: FxCommon::new(BlendMode::Normal, 1.0),
        paint: FxPaint::Color(Color::WHITE),
        technique,
        spread: 0.0,
        size,
        contour: contour(points),
        anti_alias: false,
        range: 0.5,
        jitter: 0.0,
        noise: 0.0,
        source: GlowSource::Edge,
    }
}

/// The glow's strength (0..=255) `d` px above the rectangle's top edge, for d in 1..=n.
fn column(g: &Glow, n: i64) -> Vec<i32> {
    let m = glow_map(&rectangle(), g, false, (0, 0));
    (1..=n).map(|d| (m.get(200, 75 - d) * 255.0).round() as i32).collect()
}

#[test]
fn flat_contour_glows_end_where_reference_ends_them() {
    let precise = column(&glow(GlowTechnique::Precise, 4.0, &FLAT), 8);
    assert_eq!(&precise[..3], &[255; 3], "Precise Size 4, the solid band: {precise:?}");
    assert!(precise[3] > 0 && precise[4..].iter().all(|v| *v == 0), "Precise Size 4 reaches 4 px (Reference 213 there): {precise:?}");
    let soft = column(&glow(GlowTechnique::Softer, 4.0, &FLAT), 8);
    assert_eq!(&soft[..2], &[255; 2], "Softer Size 4, the solid band: {soft:?}");
    assert!(soft[2] > 0 && soft[3..].iter().all(|v| *v == 0), "Softer Size 4 reaches 3 px (Reference 133 there): {soft:?}");
    let wide = column(&glow(GlowTechnique::Softer, 10.0, &FLAT), 14);
    assert_eq!(&wide[..6], &[255; 6], "Softer Size 10, the solid band: {wide:?}");
    assert!(wide[10..].iter().all(|v| *v == 0), "nothing past 10 px: {wide:?}");
}

/// A contour rising from 50 % keeps its own transfer inside the reach (Reference 2022, Softer
/// Size 4: 223 175 at 1 and 2 px) and paints nothing beyond it; nowhere in the map does a high
/// contour paint past the reach.
#[test]
fn high_contours_paint_nothing_beyond_the_reach() {
    let half = column(&glow(GlowTechnique::Softer, 4.0, &HALF), 8);
    assert!((half[0] - 223).abs() <= 1 && (half[1] - 175).abs() <= 1, "{half:?}");
    assert!(half[3..].iter().all(|v| *v == 0), "{half:?}");
    for points in [&FLAT[..], &HALF[..]] {
        for (t, size) in [(GlowTechnique::Precise, 4.0f32), (GlowTechnique::Precise, 10.0), (GlowTechnique::Softer, 4.0), (GlowTechnique::Softer, 10.0)] {
            let m = glow_map(&rectangle(), &glow(t, size, points), false, (0, 0));
            for y in 0..300i64 {
                for x in 0..W as i64 {
                    let (dx, dy) = ((100 - x).max(x - 299).max(0) as f32, (75 - y).max(y - 224).max(0) as f32);
                    let beyond = match t {
                        GlowTechnique::Precise => dx.hypot(dy) > size + 1.0 + 1e-3,
                        GlowTechnique::Softer => dx.max(dy) > size,
                    };
                    if beyond {
                        assert_eq!(m.get(x, y), 0.0, "{t:?} size {size} {points:?}: ({x}, {y}) is beyond the reach");
                    }
                }
            }
        }
    }
}

/// The reach cap never paints below the Linear contour, so Linear glows (none, or a straight
/// custom curve as files store it) and Inner Glows keep their transfer; hostile sizes give a
/// finite table.
#[test]
fn glow_paint_lut_leaves_linear_and_inner_glows_alone() {
    for t in [GlowTechnique::Precise, GlowTechnique::Softer] {
        for size in [1.0f32, 4.0, 10.0, 60.0] {
            for range in [0.5f32, 1.0] {
                let linear = Glow { contour: Contour::Linear, range, ..glow(t, size, &FLAT) };
                assert_eq!(glow_paint_lut(&linear, false), glow_lut(&linear), "{t:?} {size} {range}");
                let straight = Glow { range, ..glow(t, size, &[(0.0, 0.0), (1.0, 1.0)]) };
                assert_eq!(glow_paint_lut(&straight, false), glow_lut(&straight), "a straight custom contour has no table either");
                let flat = Glow { range, ..glow(t, size, &FLAT) };
                assert_eq!(glow_paint_lut(&flat, true), glow_lut(&flat), "inner glows keep their transfer");
                let Some(f) = glow_paint_lut(&flat, false) else { panic!("a flat contour has a table") };
                assert_eq!(f[0], 0.0, "coverage 0 paints nothing");
                assert!((f[4095] - 1.0).abs() < 1e-6);
            }
        }
    }
    for size in [f32::NAN, f32::INFINITY, -5.0, 0.0, 1e9] {
        for t in [GlowTechnique::Precise, GlowTechnique::Softer] {
            let Some(f) = glow_paint_lut(&glow(t, size, &FLAT), false) else { panic!("table") };
            assert!(f.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)), "{t:?} size {size}");
        }
    }
}
