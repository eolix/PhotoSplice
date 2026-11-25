//! In an 8-bit document a Curves channel curve lands on a whole level before the composite
//! (master) curve is applied to it, as Levels' steps do (see [`super::levels_q`]). The values
//! below are measured on Reference, Red `0,0 128,64 255,255` with composite `0,0 128,200
//! 255,255`. 16-bit, float, CMYK and Lab curves were not measured and keep the continuous chain.

use super::*;
use photosplice_geom::Rect;

/// One 8-bit level as a sample.
fn level(v: u32) -> f32 {
    v as f32 / 255.0
}

/// Curve points given in 0..255 units.
fn pts(p: &[(u32, u32)]) -> Vec<CurvePoint> {
    p.iter().map(|&(i, o)| CurvePoint { input: level(i), output: level(o) }).collect()
}

/// The measured pair: a red channel curve that halves the midtones and a composite curve that
/// lifts them.
const RED: &[(u32, u32)] = &[(0, 0), (128, 64), (255, 255)];
const COMPOSITE: &[(u32, u32)] = &[(0, 0), (128, 200), (255, 255)];
const STRAIGHT: &[(u32, u32)] = &[(0, 0), (255, 255)];

fn measured() -> Adjustment {
    Adjustment::Curves { master: pts(COMPOSITE), per_channel: [pts(RED), pts(STRAIGHT), pts(STRAIGHT)], space: ToneSpace::Rgb, black: Vec::new(), pencil: false }
}

/// One gray through the compositor's adjustment path in an 8-bit document, as whole levels.
fn gray8(adj: &Adjustment, g: u32) -> [u32; 3] {
    let mut b = Buffer { rect: Rect::new(0, 0, 1, 1), px: vec![[level(g), level(g), level(g), 1.0]] };
    apply_depth(adj, &mut b, Transfer::Srgb, Some(SampleType::U8));
    [0, 1, 2].map(|i| (b.px[0][i] * 255.0 + 0.5).floor() as u32)
}

/// Reference's grays: 128 -> 64 -> 114, so the channel curve's output is a whole level on its
/// way to the composite curve (unrounded, gray 192 came out 218).
#[test]
fn eight_bit_curves_round_the_channel_before_the_composite() {
    let adj = measured();
    for (g, want) in [(64u32, [37, 114, 114]), (128, [114, 200, 200]), (192, [217, 241, 241])] {
        assert_eq!(gray8(&adj, g), want, "gray {g}");
    }
}

/// Every level of an 8-bit ramp: the red output is the composite curve at the channel curve's
/// whole level.
#[test]
fn eight_bit_curves_ramp_is_the_composite_of_the_rounded_channel() {
    let adj = measured();
    let (red, master) = (curve_lut(&pts(RED)), curve_lut(&pts(COMPOSITE)));
    let whole = |v: f32| (v * 255.0 + 0.5).floor();
    let mut b = Buffer { rect: Rect::new(0, 0, 256, 1), px: (0..256).map(|x| [level(x), level(x), level(x), 1.0]).collect() };
    apply_depth(&adj, &mut b, Transfer::Srgb, Some(SampleType::U8));
    let bad: Vec<String> = (0..256u32)
        .filter_map(|x| {
            let want = whole(lut(&master, whole(lut(&red, level(x))) / 255.0));
            let got = whole(b.px[x as usize][0]);
            (got != want).then(|| format!("{x}: {got} vs {want}"))
        })
        .collect();
    assert!(bad.is_empty(), "{} of 256 red levels differ: {bad:?}", bad.len());
}

/// Not measured, so unchanged: 16-bit and float documents (and no depth) feed the channel table
/// into the composite table unrounded; CMYK and Lab curves do so at every depth.
#[test]
fn other_depths_and_spaces_keep_the_continuous_curves_chain() {
    let chain = |adj: &Adjustment| -> [Vec<f32>; 4] {
        let Adjustment::Curves { master, per_channel, space, black, .. } = adj else { return tone_luts(adj) };
        let m = if *space == ToneSpace::Lab { curve_lut(&[]) } else { curve_lut(master) };
        let row = |c: &[CurvePoint]| curve_lut(c).iter().map(|&v| lut(&m, v)).collect::<Vec<f32>>();
        [row(&per_channel[0]), row(&per_channel[1]), row(&per_channel[2]), row(if *space == ToneSpace::Cmyk { black } else { &[] })]
    };
    let drift = |a: &[Vec<f32>; 4], b: &[Vec<f32>; 4]| a.iter().zip(b).flat_map(|(x, y)| x.iter().zip(y).map(|(p, q)| (p - q).abs())).fold(0.0f32, f32::max);
    let adj = measured();
    for depth in [None, Some(SampleType::U16), Some(SampleType::F32)] {
        let d = drift(&tone_luts_depth(&adj, depth), &chain(&adj));
        assert!(d < 1e-6, "{depth:?}: {d} off the continuous chain");
    }
    assert!(drift(&tone_luts_depth(&adj, Some(SampleType::U8)), &chain(&adj)) > 1e-4, "8 bits round");
    for space in [ToneSpace::Cmyk, ToneSpace::Lab] {
        let mut adj = measured();
        if let Adjustment::Curves { space: s, black, .. } = &mut adj {
            *s = space;
            *black = pts(RED);
        }
        for depth in [Some(SampleType::U8), Some(SampleType::U16), None] {
            let d = drift(&tone_luts_depth(&adj, depth), &chain(&adj));
            assert!(d < 1e-6, "{space:?} {depth:?}: {d} off the continuous chain");
        }
    }
}
