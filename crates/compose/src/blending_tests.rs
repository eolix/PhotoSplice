//! Blend modes, Fill, Blend If and Dissolve across the compositor's paths (adjustment layers,
//! clipped layers, effects, pass-through groups).

use super::*;
use photosplice_color::{Color, ColorMode, PixelFormat, SampleType};
use photosplice_doc::{Adjustment, Document, Layer, LayerContent};
use photosplice_geom::Size;

fn px(doc: &Document, x: i32, y: i32) -> [f32; 4] {
    render(doc, Rect::from_xywh(x, y, 1, 1)).px[0]
}

fn levels(p: [f32; 4]) -> [i32; 3] {
    [0, 1, 2].map(|c| (p[c] * 255.0).round() as i32)
}

/// An adjustment layer at 40% Fill in Color Dodge or Linear Burn blends a result pulled toward the
/// mode's neutral colour, as a pixel layer does, instead of fading the blend by Fill. Reference: a
/// constant 0.5 look over (64, 128, 192) gives (80, 160, 240) and (13, 77, 141).
#[test]
fn adjustment_fill_changes_the_special_blend_modes() {
    for (mode, want) in [(BlendMode::ColorDodge, [80, 160, 240]), (BlendMode::LinearBurn, [13, 77, 141])] {
        for clipped in [false, true] {
            let mut d = Document::with_background("fill", Size::new(1, 1), ColorMode::Rgb, SampleType::U8, Color::WHITE);
            let mut base = Layer::raster("b", PixelFormat::RGBA8);
            base.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 1, 1), &[64.0 / 255.0, 128.0 / 255.0, 192.0 / 255.0, 1.0]);
            let look = Adjustment::ColorLookup { name: "half".into(), lut: Some(std::sync::Arc::new(vec![0.5; 24])), size: 2, tetrahedral: false, dither: false };
            let mut adj = Layer::new("look", LayerContent::Adjustment(look));
            adj.blend = mode;
            adj.fill_opacity = 0.4;
            // Clipped to the base, the adjustment blends the same way.
            adj.clipped = clipped;
            d.layers.push(base);
            d.layers.push(adj);
            let got = levels(px(&d, 0, 0));
            assert!(got.iter().zip(want).all(|(g, w)| (g - w).abs() <= 1), "{mode:?} clipped {clipped}: {got:?} vs Reference {want:?}");
        }
    }
}

/// One opaque pixel of `rgb` (0..255) as a raster layer.
fn solid(name: &str, rgb: [f32; 3]) -> Layer {
    let mut l = Layer::raster(name, PixelFormat::RGBA8);
    l.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 1, 1), &[rgb[0] / 255.0, rgb[1] / 255.0, rgb[2] / 255.0, 1.0]);
    l
}

/// A pixel layer clipped to an opaque base at 70% Opacity and 40% Fill in Linear Light takes
/// Fill inside the blend, as unclipped: Reference gives (86, 35, 12) for (210, 120, 50) over
/// (40, 40, 40); Fill as coverage gave (86, 36, 29).
#[test]
fn clipped_pixel_layer_fill_changes_the_special_blend_modes() {
    for depth in DEPTHS {
        let mut d = Document::new("fill", Size::new(1, 1), ColorMode::Rgb, depth);
        d.layers.push(solid("base", [40.0; 3]));
        let mut top = solid("top", [210.0, 120.0, 50.0]);
        top.blend = BlendMode::LinearLight;
        top.opacity = 0.7;
        top.fill_opacity = 0.4;
        top.clipped = true;
        d.layers.push(top);
        let got = levels(px(&d, 0, 0));
        assert!(got.iter().zip([86, 35, 12]).all(|(g, w)| (g - w).abs() <= 1), "{depth:?}: {got:?} vs Reference [86, 35, 12]");
    }
}

/// A clipped pixel layer in one of the special eight at zero Fill leaves its base exactly as it
/// was (32-bit, where nothing rounds it afterwards).
#[test]
fn clipped_pixel_layer_at_zero_fill_keeps_the_base_exact() {
    for mode in BlendMode::LAYER_MODES.into_iter().filter(|m| psblend::fill_is_special(*m)) {
        let mut d = Document::new("fill", Size::new(1, 1), ColorMode::Rgb, SampleType::F32);
        d.layers.push(solid("base", [40.0, 100.0, 170.0]));
        let want = px(&d, 0, 0);
        let mut top = solid("top", [210.0, 120.0, 50.0]);
        top.blend = mode;
        top.fill_opacity = 0.0;
        top.clipped = true;
        d.layers.push(top);
        assert_eq!(px(&d, 0, 0), want, "{mode:?}");
    }
}

/// With Transparency Shapes Layer off, a clipped layer in one of the special eight takes its own
/// alpha inside the blend as Fill, as unclipped ([`blend_layer_into`]): a half-transparent pixel
/// at Fill 100% is an opaque one at Fill 50%.
#[test]
fn clipped_pixel_layer_without_transparency_shapes_takes_alpha_as_fill() {
    let doc_with = |alpha: f32, fill: f32, shapes: bool| {
        let mut d = Document::new("fill", Size::new(1, 1), ColorMode::Rgb, SampleType::F32);
        d.layers.push(solid("base", [40.0, 100.0, 170.0]));
        let mut top = Layer::raster("top", PixelFormat::RGBA8);
        top.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 1, 1), &[210.0 / 255.0, 120.0 / 255.0, 50.0 / 255.0, alpha]);
        top.blend = BlendMode::ColorDodge;
        top.opacity = 0.7;
        top.fill_opacity = fill;
        top.transparency_shapes = shapes;
        top.clipped = true;
        d.layers.push(top);
        px(&d, 0, 0)
    };
    // 128 / 255: what an 8-bit surface stores for half alpha.
    let (half, opaque_at_half_fill) = (doc_with(128.0 / 255.0, 1.0, false), doc_with(1.0, 128.0 / 255.0, true));
    assert!(half.iter().zip(opaque_at_half_fill).all(|(a, b)| (a - b).abs() < 1e-5), "{half:?} vs {opaque_at_half_fill:?}");
}

const DEPTHS: [SampleType; 3] = [SampleType::U8, SampleType::U16, SampleType::F32];

fn doc_at(depth: SampleType) -> Document {
    Document::new("t", Size::new(12, 8), ColorMode::Rgb, depth)
}

fn solid_at(d: &Document, name: &str, rect: Rect, rgba: [f32; 4]) -> Layer {
    let mut l = Layer::raster(name, d.pixel_format());
    l.surface_mut().unwrap().fill_rect(rect, &photosplice_raster::from_rgba(&d.pixel_format(), rgba));
    l
}

fn assert_px(d: &Document, x: i32, y: i32, want: [f32; 4], what: &str) {
    let got = px(d, x, y);
    assert!(got.iter().zip(want).all(|(g, w)| (g - w).abs() <= 2.0 / 255.0), "{what} ({:?}): {got:?} vs {want:?}", d.depth);
}

#[test]
fn interior_effects_blend_with_the_backdrop_through_a_see_through_normal_layer() {
    use photosplice_doc::{Effect, FxCommon};
    // #04170F under a Normal layer at fill 0 with a yellow Soft Light colour overlay. The overlay
    // soft-lights the dark green it sees through the layer (Reference's soft light of yellow: the
    // W3C D(b) for red and green, the cubic ((16b − 12)b + 4)b below a quarter, and b² for blue),
    // not transparency, which would leave plain yellow. A half-transparent part of the layer
    // mixes it in at half strength.
    let bg: [f32; 3] = [4.0 / 255.0, 23.0 / 255.0, 15.0 / 255.0];
    let dark = |b: f32| ((16.0 * b - 12.0) * b + 4.0) * b;
    let lit = [dark(bg[0]), dark(bg[1]), bg[2] * bg[2]];
    let half = |c: usize| (bg[c] + lit[c]) / 2.0;
    for depth in DEPTHS {
        let mut d = doc_at(depth);
        let full = d.bounds();
        d.layers.push(solid_at(&d, "green", full, [bg[0], bg[1], bg[2], 1.0]));
        let mut l = solid_at(&d, "fade", Rect::new(0, 0, 6, 8), [0.2, 0.6, 0.9, 1.0]);
        l.surface_mut().unwrap().fill_rect(Rect::new(6, 0, 12, 8), &photosplice_raster::from_rgba(&d.pixel_format(), [0.2, 0.6, 0.9, 0.5]));
        l.fill_opacity = 0.0;
        l.effects.items = vec![Effect::ColorOverlay { common: FxCommon::new(BlendMode::SoftLight, 1.0), color: Color::rgb(1.0, 1.0, 0.0) }];
        d.layers.push(l);
        assert_px(&d, 2, 2, [lit[0], lit[1], lit[2], 1.0], "opaque part");
        assert_px(&d, 9, 2, [half(0), half(1), half(2), 1.0], "half-transparent part");
    }
}

#[test]
fn a_see_through_normal_layer_without_interior_effects_counts_the_backdrop_once() {
    use photosplice_doc::{Effect, FxCommon};
    // A half-transparent backdrop under a Normal layer at fill 50 % whose only effect is a drop
    // shadow (cast off the canvas): plain Normal compositing, blue at 50 % over red at 50 %. The
    // layer must not merge into the backdrop first, or the backdrop shows twice (alpha 0.875).
    for depth in DEPTHS {
        let mut d = doc_at(depth);
        let full = d.bounds();
        d.layers.push(solid_at(&d, "red", full, [1.0, 0.0, 0.0, 0.5]));
        let mut l = solid_at(&d, "blue", Rect::new(0, 0, 12, 4), [0.0, 0.0, 1.0, 1.0]);
        l.fill_opacity = 0.5;
        l.effects.items = vec![Effect::DropShadow(photosplice_doc::Shadow {
            common: FxCommon::new(BlendMode::Normal, 1.0),
            color: Color::BLACK,
            angle: 90.0,
            use_global_light: false,
            distance: 20.0,
            spread: 1.0,
            size: 0.0,
            contour: photosplice_doc::Contour::Linear,
            anti_alias: false,
            noise: 0.0,
            knocks_out: true,
        })];
        d.layers.push(l);
        assert_px(&d, 2, 2, [1.0 / 3.0, 0.0, 2.0 / 3.0, 0.75], "inside the layer");
    }
}

fn close(a: [f32; 4], b: [f32; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() <= 2.0 / 255.0)
}

#[track_caller]
fn check(d: &Document, x: i32, y: i32, want: [f32; 4], what: &str) {
    assert_px(d, x, y, want, what);
    // Tiled and whole-canvas renders agree.
    let tiled = render_tiled(d, d.bounds(), 3);
    let whole = render(d, d.bounds());
    assert!(tiled.px.iter().zip(&whole.px).all(|(a, b)| close(*a, *b)), "{what}: tiles differ from the whole render");
}

fn c8(c: [u8; 3]) -> [f32; 3] {
    [f32::from(c[0]) / 255.0, f32::from(c[1]) / 255.0, f32::from(c[2]) / 255.0]
}

fn px8(c: [u8; 3], a: f32) -> [f32; 4] {
    let c = c8(c);
    [c[0], c[1], c[2], a]
}

/// Transparency Shapes Layer off in the special eight: the layer's own transparency blends as
/// Fill does. Measured in Reference: Background (90, 110, 130), a layer (200, 150, 80) at alpha
/// 30 % (77) and 70 % (179) in columns 0–5 and 6–11, read from the flattened 8-bit PNG.
#[test]
fn transparency_shapes_off_blends_alpha_as_fill() {
    const A30: f32 = 77.0 / 255.0;
    const A70: f32 = 179.0 / 255.0;
    let make = |depth: SampleType, mode: BlendMode, opacity: f32, fill: f32, tsl: bool| {
        let b = c8([90, 110, 130]);
        let mut d = Document::with_background("t", Size::new(12, 8), ColorMode::Rgb, depth, Color::rgb(b[0], b[1], b[2]));
        let mut l = solid_at(&d, "a30", Rect::new(0, 0, 6, 8), px8([200, 150, 80], A30));
        l.surface_mut().unwrap().fill_rect(Rect::new(6, 0, 12, 8), &photosplice_raster::from_rgba(&d.pixel_format(), px8([200, 150, 80], A70)));
        l.blend = mode;
        l.opacity = opacity;
        l.fill_opacity = fill;
        l.transparency_shapes = tsl;
        d.layers.push(l);
        d
    };
    let (o53, f50) = (135.0 / 255.0, 128.0 / 255.0);
    // (mode, opacity, fill, off at 30 %, off at 70 %, on at 30 %, on at 70 %)
    type Rgb = [u8; 3];
    let measured: [(BlendMode, f32, f32, Rgb, Rgb, Rgb, Rgb); 7] = [
        (BlendMode::ColorDodge, o53, 1.0, [105, 123, 137], [148, 151, 150], [117, 133, 139], [151, 164, 152]),
        (BlendMode::ColorBurn, o53, 1.0, [84, 99, 113], [74, 79, 69], [83, 94, 109], [73, 72, 82]),
        (BlendMode::LinearDodge, o53, 1.0, [122, 134, 143], [164, 166, 160], [117, 133, 143], [151, 164, 160]),
        (BlendMode::HardMix, o53, 1.0, [98, 111, 119], [133, 117, 75], [117, 133, 109], [151, 164, 82]),
        (BlendMode::Difference, o53, 1.0, [58, 86, 117], [69, 54, 100], [93, 99, 117], [97, 84, 100]),
        (BlendMode::ColorDodge, 1.0, f50, [102, 121, 136], [125, 139, 146], [108, 124, 137], [131, 142, 147]),
        (BlendMode::Difference, 1.0, f50, [59, 87, 118], [19, 57, 102], [66, 87, 118], [34, 57, 102]),
    ];
    for depth in DEPTHS {
        for (mode, opacity, fill, off30, off70, on30, on70) in measured {
            // A 32-bit document's Linear Dodge doesn't clip at 1 (psblend::HDR).
            if depth == SampleType::F32 && mode == BlendMode::LinearDodge {
                continue;
            }
            let what = format!("{mode:?} opacity {opacity} fill {fill}");
            let off = make(depth, mode, opacity, fill, false);
            check(&off, 2, 4, px8(off30, 1.0), &format!("{what}, off, alpha 30 %"));
            check(&off, 9, 4, px8(off70, 1.0), &format!("{what}, off, alpha 70 %"));
            let on = make(depth, mode, opacity, fill, true);
            check(&on, 2, 4, px8(on30, 1.0), &format!("{what}, on, alpha 30 %"));
            check(&on, 9, 4, px8(on70, 1.0), &format!("{what}, on, alpha 70 %"));
            assert!(advanced_active(&off.layers[1], &[]), "{what}: the GPU leaves it to the CPU");
        }
        // Modes outside the special eight look the same either way, and stay on the plain path.
        for mode in [BlendMode::Normal, BlendMode::Multiply, BlendMode::Screen, BlendMode::Overlay] {
            let (off, on) = (make(depth, mode, o53, f50, false), make(depth, mode, o53, f50, true));
            assert_eq!(render(&off, off.bounds()).px, render(&on, on.bounds()).px, "{mode:?}");
            assert!(!advanced_active(&off.layers[1], &[]), "{mode:?}");
        }
    }
}

/// A mask is not the layer's transparency: Reference draws an opaque layer under a 30 % mask the
/// same with the option on or off (measured: Color Dodge, Hard Mix, Difference and the rest of the
/// special eight, every Opacity and Fill tried). Over a half-transparent backdrop and for an
/// isolated group the alpha still blends as Fill (measured: Color Dodge at 53 %).
#[test]
fn transparency_shapes_off_ignores_masks_and_reaches_groups() {
    use photosplice_doc::LayerMask;
    let o53 = 135.0 / 255.0;
    for depth in DEPTHS {
        for mode in [BlendMode::ColorDodge, BlendMode::HardMix, BlendMode::Difference] {
            let render_with = |tsl: bool| {
                let b = c8([90, 110, 130]);
                let mut d = Document::with_background("t", Size::new(12, 8), ColorMode::Rgb, depth, Color::rgb(b[0], b[1], b[2]));
                let full = d.bounds();
                let mut l = solid_at(&d, "l", full, px8([200, 150, 80], 1.0));
                l.blend = mode;
                l.opacity = o53;
                l.mask = Some(LayerMask { surface: photosplice_raster::Surface::with_default(PixelFormat::GRAY8, &[77.0 / 255.0]), ..LayerMask::reveal_all() });
                l.transparency_shapes = tsl;
                d.layers.push(l);
                render(&d, d.bounds()).px
            };
            let (off, on) = (render_with(false), render_with(true));
            assert!(off.iter().zip(&on).all(|(a, b)| close(*a, *b)), "{mode:?} ({depth:?}): a mask isn't the layer's transparency");
        }
        // Color Dodge at 53 % over a backdrop of alpha 50 % (128): (118, 127, 129, 148) at
        // alpha 30 %, (162, 151, 131, 175) at 70 %.
        let mut d = doc_at(depth);
        let full = d.bounds();
        d.layers.push(solid_at(&d, "under", full, px8([90, 110, 130], 128.0 / 255.0)));
        let mut l = solid_at(&d, "a30", Rect::new(0, 0, 6, 8), px8([200, 150, 80], 77.0 / 255.0));
        l.surface_mut().unwrap().fill_rect(Rect::new(6, 0, 12, 8), &photosplice_raster::from_rgba(&d.pixel_format(), px8([200, 150, 80], 179.0 / 255.0)));
        l.blend = BlendMode::ColorDodge;
        l.opacity = o53;
        l.transparency_shapes = false;
        d.layers.push(l.clone());
        check(&d, 2, 4, px8([118, 127, 129], 148.0 / 255.0), "half-transparent backdrop, alpha 30 %");
        check(&d, 9, 4, px8([162, 151, 131], 175.0 / 255.0), "half-transparent backdrop, alpha 70 %");
        // An isolated Color Dodge group at 53 % holding the Normal layer: the group's alpha blends
        // as Fill, as a layer's does.
        let b = c8([90, 110, 130]);
        let mut d = Document::with_background("t", Size::new(12, 8), ColorMode::Rgb, depth, Color::rgb(b[0], b[1], b[2]));
        l.blend = BlendMode::Normal;
        l.opacity = 1.0;
        l.transparency_shapes = true;
        let mut g = Layer::group("g", vec![l]);
        g.blend = BlendMode::ColorDodge;
        g.opacity = o53;
        g.transparency_shapes = false;
        d.layers.push(g);
        check(&d, 2, 4, px8([105, 123, 137], 1.0), "group, alpha 30 %");
        check(&d, 9, 4, px8([148, 151, 150], 1.0), "group, alpha 70 %");
    }
}

fn range(black: [u8; 2], white: [u8; 2]) -> photosplice_doc::BlendRange {
    photosplice_doc::BlendRange { black, white }
}

const FULL: photosplice_doc::BlendRange = photosplice_doc::BlendRange::FULL;

fn opaque_px(rect: Rect, rgba: [f32; 4]) -> Layer {
    let mut l = Layer::raster("px", PixelFormat::RGBA8);
    l.surface_mut().unwrap().fill_rect(rect, &rgba);
    l
}

/// A Pass Through group with Blend If renders isolated, as Reference does: an Invert inside it
/// leaves the layer beneath alone. Reference: (64, 128, 192) under such a group with Red ›
/// Underlying Layer white at 110 stays (64, 128, 192); without the Blend If it is inverted.
#[test]
fn pass_through_group_with_blend_if_is_isolated() {
    for (gated, want) in [(true, [64, 128, 192]), (false, [191, 127, 63])] {
        let mut d = Document::with_background("pt", Size::new(1, 1), ColorMode::Rgb, SampleType::U8, Color::WHITE);
        d.layers.push(opaque_px(Rect::new(0, 0, 1, 1), [64.0 / 255.0, 128.0 / 255.0, 192.0 / 255.0, 1.0]));
        let mut g = Layer::group("group", vec![Layer::new("invert", LayerContent::Adjustment(Adjustment::Invert))]);
        g.blend = BlendMode::PassThrough;
        if gated {
            g.blend_if.set(1, [FULL, range([0, 0], [110, 110])]);
        }
        d.layers.push(g);
        let got = levels(px(&d, 0, 0));
        assert!(got.iter().zip(want).all(|(g, w)| (g - w).abs() <= 1), "Blend If {gated}: {got:?} vs Reference {want:?}");
    }
}

/// Reference compares Blend If at its 16-bit precision (0..32768). A hard point at level `c` sits
/// on the step `round(c · 32768 / 255)`, inclusive for both black and white points: measured at
/// 1, 64, 128 and 200 (black first shown at steps 129, 8224, 16448, 25700; white last shown at the
/// same steps). RGB16 32895 is step 16448, so a white layer with Underlying Blue black at 128 shows
/// over it; 32894 (step 16447) hides it.
#[test]
fn blend_if_16_bit_cutoffs_follow_reference_steps() {
    let shown = |v16: u16, black: u8, white: u8| {
        let v = f32::from(v16) / 65535.0;
        let mut d = Document::new("t", Size::new(1, 1), ColorMode::Rgb, SampleType::U16);
        let layer16 = |name: &str, c: [f32; 4]| {
            let mut l = Layer::raster(name, PixelFormat::RGBA16);
            l.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 1, 1), &c);
            l
        };
        d.layers.push(layer16("under", [v, v, v, 1.0]));
        let mut l = layer16("top", [1.0, 1.0, 1.0, 1.0]);
        l.blend_if.set(3, [FULL, range([black, black], [white, white])]);
        d.layers.push(l);
        px(&d, 0, 0)[0] > 0.999
    };
    // 16-bit file values for Reference's steps n: round(n · 65535 / 32768).
    let at = |n: u32| ((n * 65535 + 16384) / 32768) as u16;
    for (c, step) in [(1u8, 129u32), (64, 8224), (128, 16448), (200, 25700)] {
        assert!(shown(at(step), c, 255), "black {c}: step {step} shows");
        assert!(!shown(at(step - 1), c, 255), "black {c}: step {} hides", step - 1);
        assert!(shown(at(step), 0, c), "white {c}: step {step} shows");
        assert!(!shown(at(step + 1), 0, c), "white {c}: step {} hides", step + 1);
    }
    assert!(shown(32895, 128, 255) && !shown(32894, 128, 255), "the RGB16 boundary at 128");
}

/// Blend If's Gray is Reference's 30/59/11 weighting on its 16-bit scale (9830, 19334, 3604),
/// rounded to a level, measured with a white layer over one 8-bit backdrop colour and a hard Gray
/// point on Underlying Layer. Rec. 601 luma hid both mixed colours below; (30 R + 59 G + 11 B) /
/// 100 rounded half down missed the half-level ties that round up (kit of 389 803 colours).
#[test]
fn blend_if_gray_follows_reference_weights_and_rounding() {
    let shown = |rgb: [u8; 3], black: u8, white: u8| {
        let mut d = Document::new("t", Size::new(1, 1), ColorMode::Rgb, SampleType::U8);
        let c = rgb.map(|v| f32::from(v) / 255.0);
        d.layers.push(opaque_px(Rect::new(0, 0, 1, 1), [c[0], c[1], c[2], 1.0]));
        let mut l = opaque_px(Rect::new(0, 0, 1, 1), [1.0, 1.0, 1.0, 1.0]);
        l.blend_if.set(0, [FULL, range([black, black], [white, white])]);
        d.layers.push(l);
        px(&d, 0, 0)[0] > 0.999 && px(&d, 0, 0)[2] > 0.999
    };
    // (colour hidden by a black point at `cut`, the next colour shown), single channels.
    for (cut, below, at) in [
        (20, [65, 0, 0], [66, 0, 0]),
        (25, [81, 0, 0], [82, 0, 0]),
        (28, [91, 0, 0], [92, 0, 0]),
        (20, [0, 33, 0], [0, 34, 0]),
        (28, [0, 46, 0], [0, 47, 0]),
        (20, [0, 0, 177], [0, 0, 178]),
        (28, [0, 0, 250], [0, 0, 251]),
    ] {
        assert!(!shown(below, cut, 255), "{below:?} hidden by black {cut}");
        assert!(shown(at, cut, 255), "{at:?} shown by black {cut}");
    }
    // Mixed colours, and the white point's half-down rounding.
    assert!(shown([123, 126, 151], 128, 255), "127.85 is 128");
    assert!(shown([120, 129, 149], 0, 128), "128.5 is 128");
    assert!(!shown([121, 129, 149], 0, 128), "128.8 is 129");
    // Half-level ties that round up: 11 G > 5 R + 6 B.
    assert!(shown([216, 230, 0], 201, 255), "200.5 is 201");
    assert!(shown([221, 180, 0], 173, 255), "172.5 is 173");
    assert!(!shown([0, 100, 50], 0, 64), "64.5 is 65");
}

/// 8-bit split points fade one level wider than the sliders, and a range's black and white ramps
/// meet by their minimum. Reference, grey over black with This Layer's Gray split: 32/96 shows 17
/// of level 52 (21/65), 160/224 shows 30 of level 216 (9/65); split 0/255 on both points shows
/// 64 of level 128 (the product of the ramps gave 32).
#[test]
fn blend_if_8_bit_split_ramps_follow_reference() {
    let over_black = |v: u8, r: photosplice_doc::BlendRange| {
        let mut d = Document::new("t", Size::new(1, 1), ColorMode::Rgb, SampleType::U8);
        d.layers.push(opaque_px(Rect::new(0, 0, 1, 1), [0.0, 0.0, 0.0, 1.0]));
        let c = f32::from(v) / 255.0;
        let mut l = opaque_px(Rect::new(0, 0, 1, 1), [c, c, c, 1.0]);
        l.blend_if.set(0, [r, FULL]);
        d.layers.push(l);
        levels(px(&d, 0, 0))[0]
    };
    let split = range([32, 96], [160, 224]);
    assert_eq!(over_black(52, split), 17);
    assert_eq!(over_black(216, split), 30);
    assert_eq!(over_black(32, split), 0, "1/65 of level 32 rounds to 0");
    assert_eq!(over_black(225, split), 0);
    assert_eq!(over_black(128, range([0, 255], [0, 255])), 64);
}

/// One CMYK pixel's stored levels (255 = no ink) after `top` (inks) with Blend If entry `e` on
/// This Layer goes over blank paper.
fn cmyk_over_paper(top: [f32; 4], e: usize, r: photosplice_doc::BlendRange) -> [i32; 4] {
    let mut d = Document::new("c", Size::new(1, 1), ColorMode::Cmyk, SampleType::U8);
    let fmt = PixelFormat::new(ColorMode::Cmyk, SampleType::U8, true);
    let mut paper = Layer::raster("paper", fmt);
    paper.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 1, 1), &[0.0, 0.0, 0.0, 0.0, 1.0]);
    let mut l = Layer::raster("top", fmt);
    l.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 1, 1), &[top[0], top[1], top[2], top[3], 1.0]);
    l.blend_if.set(e, [r, FULL]);
    d.layers = vec![paper, l];
    let mut out = Vec::new();
    render_ink_bands::<()>(&d, d.bounds(), 64, |b| {
        out.extend(b.px);
        Ok(())
    })
    .unwrap();
    [0, 1, 2, 3].map(|c| 255 - (out[0][c] * 255.0).round() as i32)
}

/// CMYK Blend If tests the stored values (255 = no ink) with the RGB ramps; Gray is the Gray of
/// C, M, Y scaled by K. Reference kit 29: Magenta hard 64/192 shows stored 191 and 79, hides 207
/// and 63; Gray split 32/96 160/224 shows cyan ink 128 as stored 239 and black ink 176 as 125.
#[test]
fn cmyk_blend_if_tests_stored_inks() {
    let hard = range([64, 64], [192, 192]);
    let split = range([32, 96], [160, 224]);
    let m = |ink: f32| cmyk_over_paper([0.0, ink / 255.0, 0.0, 0.0], 2, hard)[1];
    assert_eq!([m(64.0), m(176.0), m(48.0), m(192.0)], [191, 79, 255, 255]);
    assert_eq!(cmyk_over_paper([128.0 / 255.0, 0.0, 0.0, 0.0], 0, split)[0], 239);
    assert_eq!(cmyk_over_paper([0.0, 0.0, 0.0, 176.0 / 255.0], 0, split)[3], 125);
    assert_eq!(cmyk_over_paper([0.0, 0.0, 0.0, 176.0 / 255.0], 4, split)[3], 125);
}

/// Lab Blend If tests the stored L, a, b; Gray weighs them as R, G, B. Reference kit 29, a layer
/// over (L 50 %, a −128): a split 32/96 160/224 on This Layer's a shows a stored 176 as 133; the
/// same split on Gray, over white, partly hides L 0 and 16 (Gray 90 and 94 with neutral a, b:
/// stored 24 and 23).
#[test]
fn lab_blend_if_tests_stored_channels() {
    let split = range([32, 96], [160, 224]);
    let stored = |under: [f32; 3], top: [f32; 3], e: usize| {
        let mut d = Document::new("l", Size::new(1, 1), ColorMode::Lab, SampleType::U8);
        let fmt = PixelFormat::new(ColorMode::Lab, SampleType::U8, true);
        let mut b = Layer::raster("b", fmt);
        b.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 1, 1), &[under[0], under[1], under[2], 1.0]);
        let mut l = Layer::raster("t", fmt);
        l.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 1, 1), &[top[0], top[1], top[2], 1.0]);
        l.blend_if.set(e, [split, FULL]);
        d.layers = vec![b, l];
        let p = px(&d, 0, 0);
        let lab = photosplice_color::convert::srgb_to_lab([p[0], p[1], p[2]]);
        [lab[0] / 100.0 * 255.0, lab[1] + 128.0, lab[2] + 128.0].map(|v| v.round() as i32)
    };
    let n = 128.0 / 255.0;
    assert_eq!(stored([0.5, 0.0, n], [0.5, 176.0 / 255.0, n], 2)[1], 133);
    assert_eq!(stored([1.0, n, n], [0.0, n, n], 0)[0], 24);
    assert_eq!(stored([1.0, n, n], [16.0 / 255.0, n, n], 0)[0], 23);
}

/// An adjustment layer's "This Layer" range judges its result at full strength, before Opacity
/// mixes it in. Reference: Invert at 40% over (64, 128, 192) with Red / This Layer black at 150
/// keeps the effect, (115, 128, 140), as Invert's red (191) passes 150; with white at 150 it
/// drops it. Judged after Opacity (red 115) the two came out the other way round.
#[test]
fn adjustment_blend_if_judges_the_result_before_opacity() {
    for (black, white, want) in [(150u8, 255u8, [115, 128, 140]), (0, 150, [64, 128, 192])] {
        let mut d = Document::with_background("bi", Size::new(1, 1), ColorMode::Rgb, SampleType::U8, Color::WHITE);
        d.layers.push(opaque_px(Rect::new(0, 0, 1, 1), [64.0 / 255.0, 128.0 / 255.0, 192.0 / 255.0, 1.0]));
        let mut inv = Layer::new("invert", LayerContent::Adjustment(Adjustment::Invert));
        inv.opacity = 0.4;
        inv.blend_if.set(1, [range([black, black], [white, white]), FULL]);
        d.layers.push(inv);
        let got = levels(px(&d, 0, 0));
        assert!(got.iter().zip(want).all(|(g, w)| (g - w).abs() <= 1), "black {black} white {white}: {got:?} vs Reference {want:?}");
    }
}

/// A CMYK layer without an alpha channel is opaque everywhere, its untouched pixels blank paper
/// (no ink): the ink compositor keeps it under an adjustment rather than skipping it as empty.
#[test]
fn cmyk_ink_compositing_keeps_blank_paper_without_alpha() {
    let mut d = Document::new("paper", Size::new(4, 4), ColorMode::Cmyk, SampleType::U8);
    d.layers.push(Layer::raster("paper", PixelFormat::new(ColorMode::Cmyk, SampleType::U8, false)));
    d.layers.push(Layer::new("t", LayerContent::Adjustment(Adjustment::Threshold { level: 0.5 })));
    assert!(native::render_cmyk(&d, d.bounds(), 64).is_some());
    // Paper is white, so Threshold keeps it paper.
    let white = photosplice_color::convert::cmyk_to_rgb([0.0; 4]);
    assert!(close(px(&d, 2, 2), [white[0], white[1], white[2], 1.0]), "{:?}", px(&d, 2, 2));
}

fn white_doc(w: u32, h: u32) -> Document {
    Document::with_background("t", Size::new(w, h), ColorMode::Rgb, SampleType::U8, Color::WHITE)
}

/// Counts of (black, white, anything else) pixels.
fn black_white(px: &[[f32; 4]]) -> (usize, usize, usize) {
    px.iter().fold((0, 0, 0), |(k, w, o), p| {
        if close(*p, [0.0, 0.0, 0.0, 1.0]) {
            (k + 1, w, o)
        } else if close(*p, [1.0; 4]) {
            (k, w + 1, o)
        } else {
            (k, w, o + 1)
        }
    })
}

/// Invert at 50 % Dissolve is black or white, not the gray of a Normal mix; clipped to an opaque
/// base it dissolves the same pixels.
#[test]
fn dissolve_adjustment_is_a_per_pixel_choice() {
    let mut d = white_doc(16, 16);
    let mut inv = Layer::new("inv", LayerContent::Adjustment(Adjustment::Invert));
    inv.blend = BlendMode::Dissolve;
    inv.opacity = 0.5;
    d.layers.push(inv);
    let plain = render(&d, d.bounds()).px;
    let (black, white, other) = black_white(&plain);
    assert_eq!(other, 0, "a partial mix would be gray");
    assert!(black > 40 && white > 40, "black {black} white {white}");
    d.layers.last_mut().unwrap().clipped = true;
    assert_eq!(render(&d, d.bounds()).px, plain, "clipped to the opaque background");
    d.layers.last_mut().unwrap().blend = BlendMode::Normal;
    assert!(render(&d, d.bounds()).px.iter().all(|p| (p[0] - 0.5).abs() < 2.0 / 255.0), "Normal stays a continuous mix");
}

/// A clipped Dissolve layer keeps binary coverage, Opacity × Fill of it, inside the base's shape;
/// over an opaque base it matches the unclipped layer.
#[test]
fn clipped_dissolve_is_binary_and_keeps_the_base_shape() {
    let mut d = white_doc(16, 16);
    let mut fill = Layer::new("black", LayerContent::Fill(Fill::Solid(Color::BLACK)));
    fill.blend = BlendMode::Dissolve;
    fill.opacity = 0.5;
    d.layers.push(fill);
    let control = render(&d, d.bounds()).px;
    d.layers.last_mut().unwrap().clipped = true;
    assert_eq!(render(&d, d.bounds()).px, control, "clipping over an opaque base keeps the dissolve pixels");

    let mut d = white_doc(40, 12);
    d.layers.push(opaque_px(Rect::new(0, 0, 20, 12), [0.0, 0.0, 1.0, 1.0]));
    let mut clip = opaque_px(d.bounds(), [1.0, 0.0, 0.0, 1.0]);
    clip.clipped = true;
    clip.blend = BlendMode::Dissolve;
    clip.opacity = 0.6;
    clip.fill_opacity = 0.5;
    d.layers.push(clip);
    let (mut red, mut blue) = (0, 0);
    for y in 0..12 {
        for x in 0..20 {
            let p = px(&d, x, y);
            if close(p, [1.0, 0.0, 0.0, 1.0]) {
                red += 1;
            } else {
                assert!(close(p, [0.0, 0.0, 1.0, 1.0]), "Dissolve must not interpolate: {p:?} at ({x},{y})");
                blue += 1;
            }
        }
        assert!(close(px(&d, 30, y), [1.0; 4]), "outside the base the clip is invisible");
    }
    assert!(red > 0 && blue > 0, "both source and base show: {red}/{blue}");
    assert!((red as f32 / (red + blue) as f32 - 0.3).abs() < 0.1, "coverage follows 60 % opacity × 50 % fill: {red}/{blue}");
}

/// Layer effects don't turn a Dissolve layer's coverage into a Normal mix: an identity Color
/// Overlay leaves the dissolved pixels as they are.
#[test]
fn dissolve_layer_with_effects_keeps_its_coverage() {
    use photosplice_doc::{Effect, FxCommon};
    let mut d = white_doc(16, 16);
    let mut l = Layer::new("ink", LayerContent::Fill(Fill::Solid(Color::BLACK)));
    l.blend = BlendMode::Dissolve;
    l.opacity = 0.5;
    d.layers.push(l);
    let control = render(&d, d.bounds()).px;
    d.layers.last_mut().unwrap().effects.items.push(Effect::ColorOverlay { common: FxCommon::new(BlendMode::Normal, 1.0), color: Color::BLACK });
    let styled = render(&d, d.bounds()).px;
    assert_eq!(styled, control, "an identity overlay keeps the dissolve pixels");
    let (black, white, other) = black_white(&styled);
    assert!(other == 0 && black > 0 && white > 0, "{black}/{white}/{other}");
}

/// An effect in Dissolve mode speckles: each pixel takes the effect fully or not at all, with its
/// coverage as the chance, instead of painting it as Normal.
#[test]
fn dissolve_effects_speckle() {
    use photosplice_doc::{Effect, FxCommon};
    let mut d = white_doc(40, 40);
    let mut l = opaque_px(Rect::new(14, 14, 26, 26), [0.0, 0.0, 1.0, 1.0]);
    let stroke = photosplice_doc::StrokeFx {
        common: FxCommon::new(BlendMode::Dissolve, 0.5),
        size: 6.0,
        position: photosplice_doc::StrokePosition::Outside,
        paint: photosplice_doc::FxPaint::Color(Color::BLACK),
    };
    l.effects.items = vec![Effect::ColorOverlay { common: FxCommon::new(BlendMode::Dissolve, 0.5), color: Color::BLACK }, Effect::Stroke(stroke)];
    d.layers.push(l);
    let out = render(&d, d.bounds()).px;
    let at = |x: i32, y: i32| out[(y * 40 + x) as usize];
    // Inside: the overlay is black or the blue layer; in the stroke band black or white.
    let (mut inside, mut band) = ((0, 0), (0, 0));
    for y in 0..40 {
        for x in 0..40 {
            let p = at(x, y);
            if (15..25).contains(&x) && (15..25).contains(&y) {
                if close(p, [0.0, 0.0, 0.0, 1.0]) {
                    inside.0 += 1;
                } else {
                    assert!(close(p, [0.0, 0.0, 1.0, 1.0]), "overlay at ({x},{y}): {p:?}");
                    inside.1 += 1;
                }
            } else if (!(13..27).contains(&x) || !(13..27).contains(&y)) && (9..31).contains(&x) && (9..31).contains(&y) {
                if close(p, [0.0, 0.0, 0.0, 1.0]) {
                    band.0 += 1;
                } else {
                    assert!(close(p, [1.0; 4]), "stroke at ({x},{y}): {p:?}");
                    band.1 += 1;
                }
            }
        }
    }
    assert!(inside.0 > 10 && inside.1 > 10, "overlay {inside:?}");
    assert!(band.0 > 20 && band.1 > 20, "stroke {band:?}");
    // Tiles agree (the speckle follows document coordinates).
    let tiled = render_tiled(&d, d.bounds(), 7);
    assert_eq!(tiled.px, out);
}
