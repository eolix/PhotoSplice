//! Noise gradients (`Grdn` with `GrdF = ClNs`): Reference's random gradients, generated as
//! [`ENTRIES`] dense stops so fills, overlays and gradient maps render them like any other ramp.
//!
//! The generator was decoded clean-room from Reference's own renders (noise fills varying one
//! setting at a time: seeds, roughness, models, ranges, Restrict Colors, Add Transparency, widths
//! and depths); it reproduces them within one 8-bit level, most entries exactly:
//!
//! - Draws come from the Park–Miller minimal standard generator (`x ← 16807·x mod 2³¹ − 1`,
//!   started from `RndS`, uniform `u = x / (2³¹ − 1)`; seed 0 stays at 0). Each of [`TERMS`]
//!   terms takes eight draws: the amplitudes of the four channels (the model's three, then
//!   transparency), then their phases.
//! - A channel is a sum of sines at geometric frequencies over the ramp's 4096 locations:
//!   `127.5 + Σⱼ 127.5·qʲ·(1 − 2aⱼ)·sin 2π(0.12·1.18ʲ·p/4096 + φⱼ)`, `p` the location (a pixel `x`
//!   of a `w`-wide fill reads location `x·4096/w`). Roughness (`Smth / 4096`) sets the falloff
//!   `q = (0.2 + 0.7·roughness)^0.15`. The sine is a 4096-step table whose step is truncated
//!   towards the half turn.
//! - Restrict Colors (`VctC`) pulls the colour back onto the sphere of radius 127.5 around mid
//!   grey when it lies outside; channels then clip to 0..255 and scale into their `Mnm`..`Mxm`
//!   range (percent).
//! - HSB maps the channels to hue (0..360°), saturation and brightness, Lab to L (0..100) and
//!   a, b (−128..127); the ramp is stored in 8-bit levels, as Reference's is in every depth.

use photosplice_color::{Color, ColorMode};
use photosplice_psd::descriptor::{Descriptor, Value};

use crate::blocks::{bool_of, enum_of, num};

/// Entries in a generated noise ramp: Reference's locations 0..=4096.
pub(crate) const ENTRIES: usize = 4097;

/// Sine terms summed per channel.
const TERMS: usize = 51;

/// The minimal standard generator's modulus, 2³¹ − 1.
const MODULUS: u64 = 2_147_483_647;

/// Steps of the sine table per turn.
const STEPS: usize = 4096;

/// The colour model a noise gradient generates in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Model {
    Rgb,
    Hsb,
    Lab,
}

/// A noise gradient's settings.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Noise {
    pub seed: u32,
    /// Roughness, `0..=1` (`Smth` / 4096).
    pub roughness: f32,
    pub model: Model,
    pub restrict: bool,
    pub transparency: bool,
    /// Per-channel bounds, `0..=1` (the model's three channels, then transparency).
    pub min: [f32; 4],
    pub max: [f32; 4],
}

impl Noise {
    /// The settings of a `ClNs` gradient descriptor (`None` for any other kind).
    pub(crate) fn from_desc(grad: &Descriptor) -> Option<Noise> {
        if enum_of(grad, "GrdF") != Some(b"ClNs".as_slice()) {
            return None;
        }
        let bounds = |key: &str, default: f32| -> [f32; 4] {
            let list = match grad.get(key) {
                Some(Value::List(items)) => items.as_slice(),
                _ => &[],
            };
            std::array::from_fn(|k| list.get(k).and_then(|v| num(Some(v))).map_or(default, |v| (v as f32 / 100.0).clamp(0.0, 1.0)))
        };
        let model = match enum_of(grad, "ClrS") {
            Some(b"HSBl" | b"HSBC") => Model::Hsb,
            Some(b"LbCl") => Model::Lab,
            _ => Model::Rgb,
        };
        Some(Noise {
            // `RndS` is any 32-bit value; reinterpreting a negative integer keeps its bits.
            seed: num(grad.get("RndS")).map_or(0, |v| v as i64 as u32),
            roughness: num(grad.get("Smth")).map_or(0.5, |v| (v / 4096.0) as f32).clamp(0.0, 1.0),
            model,
            restrict: bool_of(grad, "VctC"),
            transparency: bool_of(grad, "ShTr"),
            min: bounds("Mnm ", 0.0),
            max: bounds("Mxm ", 1.0),
        })
    }

    /// The ramp as stops: [`ENTRIES`] colour stops, and opacity stops when Add Transparency is on.
    pub(crate) fn stops(&self) -> crate::blocks::Stops {
        let table = self.table();
        let at = |i: usize| i as f32 / (ENTRIES - 1) as f32;
        let colors = table.iter().enumerate().map(|(i, p)| (at(i), Color::rgb(p[0], p[1], p[2]))).collect();
        let opacity = if self.transparency { table.iter().enumerate().map(|(i, p)| (at(i), p[3])).collect() } else { Vec::new() };
        (colors, opacity)
    }

    /// The ramp's [`ENTRIES`] RGBA values (`0..=1`, whole 8-bit levels), entry `i` at location `i`.
    pub(crate) fn table(&self) -> Vec<[f32; 4]> {
        let draws = draws(self.seed);
        let sine = sine_table();
        let roughness = if self.roughness.is_finite() { f64::from(self.roughness).clamp(0.0, 1.0) } else { 0.5 };
        let falloff = (0.2 + 0.7 * roughness).powf(0.15);
        // Reference's lacunarity is the single-precision 1.18.
        let lacunarity = f64::from(1.18f32);
        let terms: Vec<(f64, f64)> = (0..TERMS).map(|j| (0.12 * lacunarity.powi(j as i32) / 4096.0, 127.5 * falloff.powi(j as i32))).collect();
        let draw = |k: usize| draws.get(k).copied().unwrap_or(0.0);
        let channel = |c: usize, p: f64| -> f64 {
            terms.iter().enumerate().fold(127.5, |v, (j, (freq, weight))| {
                let (a, phase) = (draw(8 * j + c), draw(8 * j + 4 + c));
                v + weight * (1.0 - 2.0 * a) * sine_at(&sine, freq * p + phase)
            })
        };
        let bound = |c: usize| {
            let finite = |v: Option<&f32>, d: f32| v.copied().filter(|v| v.is_finite()).unwrap_or(d).clamp(0.0, 1.0);
            let lo = f64::from(finite(self.min.get(c), 0.0));
            // A minimum above the maximum leaves the channel at the minimum.
            (lo, f64::from(finite(self.max.get(c), 1.0)).max(lo))
        };
        let scale = |c: usize, raw: f64| {
            let (lo, hi) = bound(c);
            lo + (hi - lo) * raw.clamp(0.0, 255.0) / 255.0
        };
        (0..ENTRIES)
            .map(|i| {
                let p = i as f64;
                let mut v = [channel(0, p), channel(1, p), channel(2, p)];
                if self.restrict {
                    restrict(&mut v);
                }
                let [a, b, c] = std::array::from_fn(|k| scale(k, v[k]) as f32);
                let rgb = match self.model {
                    Model::Rgb => [a, b, c],
                    Model::Hsb => crate::blocks::hsb_to_rgb(a * 360.0, b, c),
                    Model::Lab => Color { mode: ColorMode::Lab, c: [a, b, c, 0.0], alpha: 1.0 }.to_rgb(),
                };
                let alpha = if self.transparency { scale(3, channel(3, p)) as f32 } else { 1.0 };
                [level(rgb[0]), level(rgb[1]), level(rgb[2]), level(alpha)]
            })
            .collect()
    }
}

/// `v` (`0..=1`) on whole 8-bit levels; a value half-way between two takes the lower one (as
/// Reference's mid grey 127.5 at a seed-0 ramp's start reads 127).
fn level(v: f32) -> f32 {
    let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    (v * 255.0 - 0.5).ceil().clamp(0.0, 255.0) / 255.0
}

/// Restrict Colors: a colour (0..255 channels, unclipped) farther than 127.5 from mid grey is
/// pulled back onto that sphere, the largest inside the channel cube.
fn restrict(v: &mut [f64; 3]) {
    let d = v.map(|c| c - 127.5);
    let len = d.iter().map(|c| c * c).sum::<f64>().sqrt();
    if len > 127.5 {
        let k = 127.5 / len;
        *v = std::array::from_fn(|i| 127.5 + d[i] * k);
    }
}

/// [`TERMS`] × 8 uniform draws of the minimal standard generator from `seed`.
fn draws(seed: u32) -> Vec<f64> {
    let mut x = u64::from(seed) % MODULUS;
    (0..TERMS * 8)
        .map(|_| {
            x = x * 16_807 % MODULUS;
            x as f64 / MODULUS as f64
        })
        .collect()
}

/// The sine table, indexed by the truncated step from the half turn: entry `k + STEPS/2` holds
/// `sin 2π(k/STEPS + 1/2)`, `k` in `−STEPS/2..STEPS/2`.
fn sine_table() -> Vec<f64> {
    let half = (STEPS / 2) as f64;
    (0..STEPS).map(|i| (std::f64::consts::TAU * ((i as f64 - half) / STEPS as f64 + 0.5)).sin()).collect()
}

/// The table sine of `turns`: the step counted from the half turn, truncated towards it.
fn sine_at(table: &[f64], turns: f64) -> f64 {
    if !turns.is_finite() {
        return 0.0;
    }
    let f = turns - turns.floor();
    let k = ((f - 0.5) * STEPS as f64).trunc() + (STEPS / 2) as f64;
    // `f` rounding up to a whole turn lands past the end: the turn's start, sin 0.
    table.get(k as usize).copied().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use photosplice_psd::descriptor::Id;

    fn desc(model: &str, smth: i32, restrict: bool, transparency: bool) -> Descriptor {
        desc_with(model, smth, restrict, transparency, 632_413_682, [0; 4], [100; 4])
    }

    fn desc_with(model: &str, smth: i32, restrict: bool, transparency: bool, seed: i32, min: [i32; 4], max: [i32; 4]) -> Descriptor {
        let list = |v: [i32; 4]| Value::List(v.iter().map(|x| Value::Integer(*x)).collect());
        Descriptor::new("Grdn")
            .with("GrdF", Value::Enumerated { type_id: Id::new("GrdF"), value: Id::new("ClNs") })
            .with("ShTr", Value::Boolean(transparency))
            .with("VctC", Value::Boolean(restrict))
            .with("ClrS", Value::Enumerated { type_id: Id::new("ClrS"), value: Id::new(model) })
            .with("RndS", Value::Integer(seed))
            .with("Smth", Value::Integer(smth))
            .with("Mnm ", list(min))
            .with("Mxm ", list(max))
    }

    /// Entries of a ramp as 8-bit levels at the locations Reference's 4096-px renders were
    /// sampled at.
    const AT: [usize; 7] = [0, 300, 1000, 2047, 3000, 4000, 4095];

    fn levels(d: &Descriptor) -> Vec<[i32; 4]> {
        let t = Noise::from_desc(d).expect("noise").table();
        AT.iter().map(|&i| t[i].map(|v| (v * 255.0).round() as i32)).collect()
    }

    /// Our ramp within one level of Reference's (its 4096-px render of the same settings).
    fn assert_matches_reference(d: &Descriptor, want: [[i32; 3]; 7]) {
        let got = levels(d);
        for (g, w) in got.iter().zip(want) {
            assert!((0..3).all(|c| (g[c] - w[c]).abs() <= 1), "{got:?}\nvs Reference {want:?}");
        }
    }

    #[test]
    fn minimal_standard_draws() {
        // x₁ = 16807, x₁₀₀₀₀ = 1043618065 (Park and Miller's check value).
        let d = draws(1);
        assert_eq!(d[0], 16_807.0 / MODULUS as f64);
        let mut x = 1u64;
        for _ in 0..10_000 {
            x = x * 16_807 % MODULUS;
        }
        assert_eq!(x, 1_043_618_065);
        // Seed 0 (and 2³¹ − 1) is a fixed point.
        assert!(draws(0).iter().all(|u| *u == 0.0));
        assert_eq!(draws(2_147_483_647), draws(0));
    }

    #[test]
    fn rgb_ramps_match_reference() {
        let rgb = |seed: i32, smth: i32| desc_with("RGBC", smth, false, false, seed, [0; 4], [100; 4]);
        assert_matches_reference(&rgb(9_052_369, 2048), [[196, 139, 255], [194, 81, 255], [255, 147, 196], [255, 248, 170], [248, 189, 114], [59, 255, 137], [69, 255, 127]]);
        assert_matches_reference(&rgb(1, 4096), [[0, 0, 76], [102, 0, 0], [0, 107, 117], [0, 27, 0], [119, 0, 255], [0, 107, 255], [0, 97, 255]]);
        assert_matches_reference(&rgb(9_052_369, 0), [[199, 92, 255], [202, 101, 255], [208, 125, 231], [201, 155, 199], [185, 169, 173], [149, 187, 157], [147, 188, 155]]);
        assert_matches_reference(&rgb(2_126_732, 2048), [[91, 227, 255], [11, 191, 255], [0, 235, 255], [0, 68, 195], [20, 81, 106], [114, 86, 140], [123, 117, 74]]);
        // Seed 0: every draw 0, all channels alike, starting at mid grey.
        let t = Noise::from_desc(&rgb(0, 2048)).expect("noise").table();
        let start: Vec<i32> = t[..10].iter().map(|p| (p[0] * 255.0).round() as i32).collect();
        assert_eq!(start, [127, 143, 155, 164, 171, 176, 179, 182, 185, 189]);
        assert!(t.iter().all(|p| p[0] == p[1] && p[1] == p[2]));
    }

    #[test]
    fn restrict_ranges_hsb_and_transparency_match_reference() {
        let seed = 9_052_369;
        assert_matches_reference(&desc_with("RGBC", 2048, true, false, seed, [0; 4], [100; 4]), [[167, 134, 249], [170, 98, 244], [244, 141, 178], [221, 210, 157], [240, 185, 115], [72, 242, 135], [82, 247, 127]]);
        assert_matches_reference(&desc_with("RGBC", 2048, false, false, seed, [25, 25, 25, 0], [75, 75, 75, 100]), [[162, 133, 191], [161, 104, 191], [191, 137, 162], [191, 188, 149], [188, 158, 121], [93, 191, 132], [98, 191, 127]]);
        assert_matches_reference(&desc_with("HSBl", 2048, false, false, seed, [0; 4], [100; 4]), [[202, 116, 255], [219, 174, 255], [196, 84, 84], [170, 5, 5], [114, 29, 44], [84, 137, 0], [48, 127, 0]]);
        // Add Transparency: a fourth channel (black colour, its alpha over white in Reference).
        let alpha: Vec<i32> = levels(&desc_with("RGBC", 2048, false, true, seed, [0; 4], [0, 0, 0, 100])).iter().map(|p| p[3]).collect();
        for (g, w) in alpha.iter().zip([255, 243, 149, 171, 0, 5, 11]) {
            assert!((g - w).abs() <= 1, "alpha {alpha:?}");
        }
    }

    #[test]
    fn ramp_shape_and_stop_count() {
        let n = Noise::from_desc(&desc("RGBC", 2048, false, false)).expect("noise");
        assert_eq!(n.table(), n.table());
        let (stops, opacity) = n.stops();
        assert_eq!(stops.len(), ENTRIES);
        assert!(opacity.is_empty());
        assert_eq!((stops[0].0, stops[ENTRIES - 1].0, stops[4096 / 4].0), (0.0, 1.0, 0.25));
        let n = Noise::from_desc(&desc("RGBC", 4096, true, true)).expect("noise");
        let t = n.table();
        // Restrict Colors keeps every colour within 127.5 of mid grey.
        assert!(t.iter().all(|p| p[..3].iter().map(|c| (c * 255.0 - 127.5).powi(2)).sum::<f32>().sqrt() <= 128.5));
        assert!(t.iter().any(|p| p[3] < 0.9), "alpha varies");
        assert_eq!(n.stops().1.len(), ENTRIES);
        // Lab produces valid RGB.
        let n = Noise::from_desc(&desc("LbCl", 3000, false, false)).expect("noise");
        assert!(n.table().iter().flatten().all(|v| (0.0..=1.0).contains(v)));
    }

    #[test]
    fn bounds_clip_and_hostile_values_stay_in_range() {
        let d = desc_with("RGBC", 99_999, false, false, -7, [40, 120, -5, 0], [60, 10, 100, 100]);
        let t = Noise::from_desc(&d).expect("noise").table();
        assert!(t.iter().all(|p| (0.39..=0.61).contains(&p[0])), "red within 40..60 %");
        assert!(t.iter().all(|p| p[1] == 1.0), "minimum above maximum: green stays at the minimum");
        assert!(t.iter().flatten().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
        let odd = Noise { seed: u32::MAX, roughness: f32::NAN, model: Model::Hsb, restrict: true, transparency: true, min: [f32::NAN; 4], max: [f32::INFINITY; 4] };
        assert!(odd.table().iter().flatten().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
        assert_eq!(sine_at(&sine_table(), f64::NAN), 0.0);
        // Not a noise gradient.
        assert!(Noise::from_desc(&Descriptor::new("Grdn")).is_none());
    }

    #[test]
    fn unedited_noise_fill_keeps_its_definition() {
        use photosplice_psd::descriptor::VersionedDescriptor;
        let grad = desc("RGBC", 2048, false, true);
        let gdfl = Descriptor::new("null").with("Grad", Value::Descriptor(grad.clone()));
        let fill = crate::blocks::fill_from_desc(b"GdFl", &gdfl).expect("fill");
        let photosplice_doc::Fill::Gradient { stops, opacity_stops, .. } = &fill else { panic!("gradient fill") };
        assert_eq!((stops.len(), opacity_stops.len()), (ENTRIES, ENTRIES));
        let old = VersionedDescriptor::new(gdfl).to_bytes();
        let (_, back) = crate::blocks::fill_to_desc_onto(&fill, Some((b"GdFl", &old)));
        assert_eq!(crate::blocks::get_desc(&back, "Grad"), Some(&grad), "noise definition written back verbatim");
    }
}
