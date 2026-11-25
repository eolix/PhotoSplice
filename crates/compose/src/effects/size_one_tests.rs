//! Size 1 Inner Shadow, Inner Glow and Satin, measured in Reference on one test document:
//! 400 × 300 px, RGB 8 bit, white Background, an orange (255, 128, 0) pixel layer covering
//! x 100..=299, y 75..=224, a black effect in Normal at 100 %, Use Global Light off, Linear
//! contour, Noise 0. The tables are the G channel read from a layer edge inwards: 128 is the
//! layer's own orange, 0 full black.
//!
//! A width-1 box is the identity, so these effects used to keep the hard shape at Size 1 (the
//! clip to the layer then removed an Inner Shadow and an Inner Glow completely); Reference puts a
//! quarter of the coverage one pixel either side of the edge ([`SIZE_ONE_WIDTH`]). Reference 2022
//! renders the same Inner Shadow, Inner Glow and Satin, and the Drop Shadow and Outer Glow follow
//! it (white effects over black in 16 bits).

use super::*;
use photosplice_color::{Color, ColorMode, PixelFormat, SampleType};
use photosplice_doc::{Document, FxCommon, Satin};
use photosplice_geom::Size;

const DOC_W: usize = 400;
const DOC_H: usize = 300;

/// The layer's alpha over the whole document: solid over x 100..=299, y 75..=224.
fn document_shape() -> Map {
    let mut m = Map::new(DOC_W, DOC_H, 0.0);
    for y in 75..=224 {
        for x in 100..=299 {
            m.v[y * DOC_W + x] = 1.0;
        }
    }
    m
}

/// The measured Inner Shadow (or Drop Shadow) settings with Choke `spread`.
fn shadow(size: f32, distance: f32, angle: f32, spread: f32) -> Shadow {
    Shadow {
        common: FxCommon::new(BlendMode::Normal, 1.0),
        color: Color::BLACK,
        angle,
        use_global_light: false,
        distance,
        spread,
        size,
        contour: Contour::Linear,
        anti_alias: false,
        noise: 0.0,
        knocks_out: false,
    }
}

fn inner_shadow(size: f32, distance: f32, angle: f32, spread: f32) -> Map {
    shadow_map(&document_shape(), &shadow(size, distance, angle, spread), &GlobalLight::default(), true, (0, 0))
}

/// The G channel of the orange layer under black of coverage `c`.
fn g(c: f32) -> f32 {
    128.0 * (1.0 - c)
}

/// `n` G values inwards from each of the four edges: top (column x 200 from y 75 down), left
/// (row y 150 from x 100), bottom (column x 200 from y 224 up), right (row y 150 from x 299).
fn ramps(m: &Map, n: i64) -> [Vec<f32>; 4] {
    let line = |first: (i64, i64), step: (i64, i64)| (0..n).map(|k| g(m.get(first.0 + k * step.0, first.1 + k * step.1))).collect();
    [line((200, 75), (0, 1)), line((100, 150), (1, 0)), line((200, 224), (0, -1)), line((299, 150), (-1, 0))]
}

/// Every edge's ramp against the measured `want`, within 2 levels.
fn assert_edges(m: &Map, want: &[f32], what: &str) {
    for (side, r) in ["top", "left", "bottom", "right"].iter().zip(ramps(m, want.len() as i64)) {
        for (d, (got, w)) in r.iter().zip(want).enumerate() {
            assert!((got - w).abs() <= 2.0, "{what}, {side} edge, {d} px in: {got} vs measured {w}");
        }
    }
}

/// Reference: a Size 1 Inner Shadow (Distance 0, Choke 0) reads G 96 on the first pixel inside
/// every edge and the layer's own 128 one pixel deeper; nothing paints outside the layer.
#[test]
fn size_one_inner_shadow_draws_the_measured_edge_pixel() {
    let m = inner_shadow(1.0, 0.0, 0.0, 0.0);
    assert_edges(&m, &[96.0, 128.0], "Size 1");
    for (x, y) in [(200i64, 74), (99, 150), (200, 225), (300, 150)] {
        assert_eq!(m.get(x, y), 0.0, "no inner shadow outside the layer at ({x},{y})");
    }
}

/// Reference's profiles at Sizes 3, 10 and 20, which the tent already matched.
#[test]
fn inner_shadow_sizes_three_ten_and_twenty_keep_the_measured_profiles() {
    let table: [(f32, &[f32]); 3] = [
        (3.0, &[85.0, 114.0, 128.0]),
        (10.0, &[70.0, 82.0, 92.0, 101.0, 109.0, 115.0, 120.0, 124.0, 126.0, 128.0]),
        (20.0, &[67.0, 73.0, 79.0, 84.0, 89.0, 94.0, 99.0, 103.0, 107.0, 110.0, 113.0, 116.0, 119.0, 121.0, 123.0, 125.0, 126.0, 127.0, 127.0, 128.0]),
    ];
    for (size, want) in table {
        assert_edges(&inner_shadow(size, 0.0, 0.0, 0.0), want, &format!("Size {size}"));
    }
}

/// Reference: Size 0, Distance 5, Angle 90 is a hard band of five black pixels at the top edge.
#[test]
fn size_zero_inner_shadow_keeps_the_hard_band_of_distance() {
    let m = inner_shadow(0.0, 5.0, 90.0, 0.0);
    let col: Vec<f32> = (75..81).map(|y| g(m.get(200, y))).collect();
    assert_eq!(col, [0.0, 0.0, 0.0, 0.0, 0.0, 128.0], "five dark pixels then 128");
    for k in 0..6 {
        assert_eq!(g(m.get(100 + k, 150)), 128.0, "left edge, x {}", 100 + k);
        assert_eq!(g(m.get(200, 224 - k)), 128.0, "bottom edge, y {}", 224 - k);
    }
}

/// Reference 2022 (a white Size 1 Drop Shadow over black, Distance 0, in 16 bits): the first
/// pixel outside the layer reads 64/255, the second nothing.
#[test]
fn size_one_drop_shadow_softens_the_edge_by_one_pixel() {
    let drop = shadow_map(&document_shape(), &shadow(1.0, 0.0, 0.0, 0.0), &GlobalLight::default(), false, (0, 0));
    for (x, y) in [(200i64, 74), (99, 150), (200, 225), (300, 150)] {
        assert!((drop.get(x, y) * 255.0 - 64.0).abs() <= 1.0, "first pixel outside at ({x},{y}): {}", drop.get(x, y) * 255.0);
    }
    assert_eq!(drop.get(200, 73), 0.0, "nothing two pixels out");
}

/// The unmeasured neighbours keep their maps: a choked Size 1 Inner Shadow keeps its hard 1 px
/// band, and the kernel at a Distance only softens the end of the band of Distance pixels.
#[test]
fn size_one_kernel_stays_on_the_measured_maps() {
    let choked = inner_shadow(1.0, 0.0, 0.0, 0.5);
    assert_eq!([choked.get(200, 74), choked.get(200, 75), choked.get(200, 76)], [0.0, 1.0, 0.0], "a choked Size 1 inner shadow keeps its hard band");
    let m = inner_shadow(1.0, 5.0, 90.0, 0.0);
    let col: Vec<f32> = (75..83).map(|y| g(m.get(200, y))).collect();
    assert_eq!(&col[..4], &[0.0; 4], "the band of Distance pixels: {col:?}");
    assert!((col[4] - 32.0).abs() <= 2.0 && (col[5] - 96.0).abs() <= 2.0, "the quarter ramp where the band ends: {col:?}");
    assert_eq!(&col[6..], &[128.0; 2], "and nothing deeper: {col:?}");
}

/// The measured Satin: black, Angle 90, Distance 20, Linear contour, Anti-aliased, down the
/// layer's centre column as G values.
fn satin_column(size: f32) -> Vec<u8> {
    let satin = Satin {
        common: FxCommon::new(BlendMode::Normal, 1.0),
        color: Color::BLACK,
        angle: 90.0,
        distance: 20.0,
        size,
        contour: Contour::Linear,
        anti_alias: true,
        invert: false,
    };
    let coverage = satin_map(&document_shape(), &satin);
    (75..225).map(|y| g(coverage.get(200, y)).round() as u8).collect()
}

/// The column: `dark` black rows, then `ramp`, bright rows, the mirrored ramp and `dark` again.
fn assert_satin(size: f32, dark: usize, ramp: &[u8]) {
    let col = satin_column(size);
    let bright = 150 - 2 * (dark + ramp.len());
    let want = std::iter::repeat_n(0, dark)
        .chain(ramp.iter().copied())
        .chain(std::iter::repeat_n(128, bright))
        .chain(ramp.iter().rev().copied())
        .chain(std::iter::repeat_n(0, dark));
    for (i, (got, want)) in col.into_iter().zip(want).enumerate() {
        assert!(got.abs_diff(want) <= 2, "Satin size {size}, y {}: expected G {want}, got {got}", i + 75);
    }
}

/// Reference: a Size 1 Satin softens each band edge by one pixel, 19 black rows then 32, 96.
#[test]
fn satin_size_one_softens_each_band_edge_by_one_pixel() {
    assert_satin(1.0, 19, &[32, 96]);
}

/// Reference's Satin profiles at the other sizes, which the tent already matched.
#[test]
fn satin_other_sizes_keep_their_profiles() {
    assert_satin(0.0, 20, &[]);
    assert_satin(3.0, 18, &[15, 43, 85, 114]);
    assert_satin(10.0, 11, &[2, 4, 8, 13, 20, 27, 36, 46, 58, 71, 82, 92, 101, 109, 115, 120, 124, 127]);
}

fn inner_glow(size: f32) -> Glow {
    Glow {
        common: FxCommon::new(BlendMode::Normal, 1.0),
        paint: FxPaint::Color(Color::BLACK),
        technique: GlowTechnique::Softer,
        spread: 0.0,
        size,
        contour: Contour::Linear,
        anti_alias: false,
        range: 0.5,
        jitter: 0.0,
        noise: 0.0,
        source: GlowSource::Edge,
    }
}

fn glow_doc(glow: Glow) -> Document {
    let mut d = Document::with_background("Inner glow", Size::new(400, 300), ColorMode::Rgb, SampleType::U8, Color::WHITE);
    let mut l = Layer::raster("Rectangle", PixelFormat::RGBA8);
    l.surface_mut().unwrap().fill_rect(Rect::new(100, 75, 300, 225), &[1.0, 128.0 / 255.0, 0.0, 1.0]);
    l.effects.items.push(Effect::InnerGlow(glow));
    d.layers.push(l);
    d
}

fn green(b: &crate::Buffer, x: i32, y: i32) -> u8 {
    (b.get(x, y)[1] * 255.0).round() as u8
}

/// Reference: a Size 1 Softer Inner Glow (Edge, Range 50 %) reads G 64 on the first pixel inside
/// every edge and 128 one pixel deeper; nothing paints outside the layer.
#[test]
fn size_one_inner_glow_draws_the_measured_pixel_on_all_four_edges() {
    let b = crate::flatten(&glow_doc(inner_glow(1.0)));
    for ((x, y), (nx, ny)) in [((200, 75), (200, 76)), ((200, 224), (200, 223)), ((100, 150), (101, 150)), ((299, 150), (298, 150))] {
        let edge = green(&b, x, y);
        assert!(edge.abs_diff(64) <= 2, "edge ({x}, {y}): expected G 64, got {edge}");
        assert_eq!(green(&b, nx, ny), 128, "the glow stops after the first pixel");
    }
    assert_eq!(green(&b, 200, 150), 128);
    assert_eq!(green(&b, 200, 74), 255, "the glow stays inside the layer");
}

/// Reference 2022 (a white Size 1 Softer Outer Glow over black at Range 50 %, in 16 bits): the
/// first pixel outside reads 127/255 (the ¼ doubled by the Range), the second nothing.
#[test]
fn size_one_outer_glow_draws_the_measured_pixel() {
    let m = glow_map(&document_shape(), &inner_glow(1.0), false, (0, 0));
    for (x, y) in [(200i64, 74), (99, 150), (200, 225), (300, 150)] {
        assert!((m.get(x, y) * 255.0 - 127.0).abs() <= 1.0, "first pixel outside at ({x},{y}): {}", m.get(x, y) * 255.0);
    }
    assert_eq!(m.get(200, 73), 0.0, "nothing two pixels out");
}

/// Reference's Inner Glow profiles at the other sizes (from the top edge down), which the tent
/// already matched; the unmeasured Size 1 Center source and choked glow keep their maps.
#[test]
fn inner_glow_other_sizes_keep_their_profiles() {
    for (size, profile) in [(0.0, &[128][..]), (3.0, &[43, 100, 128]), (10.0, &[13, 36, 57, 74, 90, 102, 113, 120, 125, 128]), (20.0, &[6])] {
        let b = crate::flatten(&glow_doc(inner_glow(size)));
        for (offset, &want) in profile.iter().enumerate() {
            let got = green(&b, 200, 75 + offset as i32);
            assert!(got.abs_diff(want) <= 2, "size {size}, {offset} px in: expected G {want}, got {got}");
        }
    }
    let center = crate::flatten(&glow_doc(Glow { source: GlowSource::Center, ..inner_glow(1.0) }));
    assert_eq!([green(&center, 200, 75), green(&center, 200, 150)], [0, 0], "Size 1 from the Center keeps its map");
    let choked = crate::flatten(&glow_doc(Glow { spread: 0.25, ..inner_glow(1.0) }));
    assert_eq!(green(&choked, 200, 75), 128, "a choked Size 1 inner glow keeps its map");
}
