//! A Softer glow's Range in whole 8-bit levels ([`glow_lut`]), measured in Reference.
//!
//! The document: 400 × 300 px, RGB 8 bit, white Background, an orange (255, 128, 0) pixel layer
//! covering x 100..=299, y 75..=224. A black Softer glow, Normal 100 %, Size 10, Spread 0, Linear
//! contour, Anti-aliased off, Noise 0. Profiles list the R channel from the layer's edge outwards
//! (255 = no glow) for an Outer Glow and the G channel inwards (128 = the layer's own) for an
//! Inner Glow, on all four edges.

use super::{Contour, Effect, FxPaint, Glow, GlowSource, GlowTechnique, Rect};
use photosplice_color::{BlendMode, Color, ColorMode, PixelFormat, SampleType};
use photosplice_doc::{Document, FxCommon, Layer};
use photosplice_geom::Size;

fn glow(range: f32) -> Glow {
    Glow {
        common: FxCommon::new(BlendMode::Normal, 1.0),
        paint: FxPaint::Color(Color::BLACK),
        technique: GlowTechnique::Softer,
        spread: 0.0,
        size: 10.0,
        contour: Contour::Linear,
        anti_alias: false,
        range,
        jitter: 0.0,
        noise: 0.0,
        source: GlowSource::Edge,
    }
}

fn doc(g: Glow, inner: bool, background: Color, fill: [f32; 4]) -> Document {
    let mut d = Document::with_background("range", Size::new(400, 300), ColorMode::Rgb, SampleType::U8, background);
    let mut l = Layer::raster("Shape", PixelFormat::RGBA8);
    l.surface_mut().unwrap().fill_rect(Rect::new(100, 75, 300, 225), &fill);
    l.effects.items = vec![if inner { Effect::InnerGlow(g) } else { Effect::OuterGlow(g) }];
    d.layers.push(l);
    d
}

/// The pixel `d` (1-based) from edge `e` (top, left, bottom, right), outwards or inwards.
fn point(e: usize, d: i32, inward: bool) -> (i32, i32) {
    let d = if inward { 1 - d } else { d };
    match e {
        0 => (200, 75 - d),
        1 => (100 - d, 150),
        2 => (200, 224 + d),
        _ => (299 + d, 150),
    }
}

/// Channel `ch` along every edge against `want`, within `tol` levels.
fn assert_all_edges(d: &Document, ch: usize, inward: bool, want: &[i32], tol: i32, what: &str) {
    let b = crate::render(d, d.bounds());
    for e in 0..4 {
        for (k, &want) in want.iter().enumerate() {
            let (x, y) = point(e, k as i32 + 1, inward);
            let got = (255.0 * b.get(x, y)[ch]).round() as i32;
            assert!((got - want).abs() <= tol, "{what}, edge {e}, {} px from the edge: expected {want}, got {got}", k + 1);
        }
    }
}

fn orange(g: Glow, inner: bool) -> Document {
    doc(g, inner, Color::WHITE, [1.0, 128.0 / 255.0, 0.0, 1.0])
}

/// Reference at Range 1 %: eight solid pixels, then 39, then no glow (the exact Range painted a
/// ninth solid pixel); the Inner Glow mirrors it with 19 on 128.
#[test]
fn range_one_gives_eight_solid_pixels_then_the_measured_step() {
    assert_all_edges(&orange(glow(0.01), false), 0, false, &[0, 0, 0, 0, 0, 0, 0, 0, 39, 255, 255], 1, "outer Range 1 %");
    assert_all_edges(&orange(glow(0.01), true), 1, true, &[0, 0, 0, 0, 0, 0, 0, 0, 19, 128, 128], 1, "inner Range 1 %");
}

/// Reference at Ranges 25, 50 and 100 %.
#[test]
fn ranges_25_50_and_100_match_the_measured_profiles() {
    for (range, want) in [
        (0.25, [0, 0, 0, 42, 103, 153, 194, 225, 245, 255]),
        (0.5, [26, 72, 113, 148, 179, 204, 225, 240, 250, 255]),
        (1.0, [140, 163, 184, 201, 217, 229, 240, 247, 252, 255]),
    ] {
        assert_all_edges(&orange(glow(range), false), 0, false, &want, 1, &format!("Range {range}"));
    }
}

/// Reference 2022 (a white glow over black at Fill 0 %, 16 bits): the Range rounds to whole
/// levels, 2 % to 5/255 (where rounding up would take 6) and 4 % to 10/255.
#[test]
fn small_ranges_round_to_whole_levels() {
    let white = |range: f32| {
        let mut d = doc(Glow { paint: FxPaint::Color(Color::WHITE), ..glow(range) }, false, Color::BLACK, [1.0, 128.0 / 255.0, 0.0, 1.0]);
        if let Some(l) = d.layers.last_mut() {
            l.fill_opacity = 0.0;
        }
        d
    };
    assert_all_edges(&white(0.02), 0, false, &[255, 255, 255, 255, 255, 255, 255, 255, 130, 0], 1, "Range 2 %");
    assert_all_edges(&white(0.04), 0, false, &[255, 255, 255, 255, 255, 255, 255, 195, 65, 0], 1, "Range 4 %");
    assert_all_edges(&white(0.08), 0, false, &[255, 255, 255, 255, 255, 255, 195, 98, 32, 0], 1, "Range 8 %");
}
