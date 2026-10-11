//! Noise gradients (`Grdn` with `GrdF = ClNs`): Photoshop's random gradients, approximated as
//! [`ENTRIES`] dense stops so fills, overlays and gradient maps render them like any other ramp.
//!
//! Photoshop's generator is unpublished, so this is not its exact output: the same parameters give
//! a different, statistically similar ramp. Each channel of the colour model (`ClrS`: RGB, HSB or
//! Lab) is a clamped random walk (Mulberry32 draws, heavy-tailed steps) seeded from `RndS`. The walk
//! runs on twice the channel's range and is clipped to it, so about half the ramp sits pinned at an
//! end of the channel, as in Photoshop's. The step size follows Roughness (`Smth`), calibrated on
//! Photoshop references: about 3 levels between neighbouring entries at 50 %, about 20 at 100 %.
//! Restrict Colors (`VctC`) walks the plain range and caps chroma (max − min) at 181/255; Add
//! Transparency (`ShTr`) walks an alpha channel too. `Mnm`/`Mxm` clip each channel (percent).

use photosuite_color::{Color, ColorMode};
use photosuite_psd::descriptor::{Descriptor, Value};

use crate::blocks::{bool_of, enum_of, num};

/// Entries in a generated noise ramp.
pub(crate) const ENTRIES: usize = 1024;

/// Restrict Colors' chroma cap (max − min of R, G, B), measured on Photoshop output.
const RESTRICT_CHROMA: f32 = 181.0 / 255.0;

/// The colour model a noise gradient walks in.
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

    /// The ramp's [`ENTRIES`] RGBA values (`0..=1`).
    pub(crate) fn table(&self) -> Vec<[f32; 4]> {
        let walks: [Vec<f32>; 4] = std::array::from_fn(|c| {
            if c == 3 && !self.transparency {
                return vec![1.0; ENTRIES];
            }
            let seed = self.seed.wrapping_mul(2 + c as u32);
            // A minimum above the maximum leaves the channel at the minimum.
            let lo = self.min.get(c).copied().unwrap_or(0.0);
            let hi = self.max.get(c).copied().unwrap_or(1.0).max(lo);
            walk(seed, self.roughness, self.restrict).into_iter().map(|v| v.clamp(lo, hi)).collect()
        });
        (0..ENTRIES)
            .map(|i| {
                let ch = |c: usize| walks.get(c).and_then(|w| w.get(i)).copied().unwrap_or(1.0);
                let (a, b, c) = (ch(0), ch(1), ch(2));
                let mut rgb = match self.model {
                    Model::Rgb => [a, b, c],
                    Model::Hsb => crate::blocks::hsb_to_rgb(a * 360.0, b, c),
                    Model::Lab => Color { mode: ColorMode::Lab, c: [a, b, c, 0.0], alpha: 1.0 }.to_rgb(),
                };
                if self.restrict {
                    restrict(&mut rgb);
                }
                [rgb[0], rgb[1], rgb[2], ch(3)]
            })
            .collect()
    }
}

/// Pulls a colour towards its mean until its chroma (max − min) is at most [`RESTRICT_CHROMA`].
fn restrict(rgb: &mut [f32; 3]) {
    let (lo, hi) = (rgb[0].min(rgb[1]).min(rgb[2]), rgb[0].max(rgb[1]).max(rgb[2]));
    let chroma = hi - lo;
    if chroma > RESTRICT_CHROMA {
        let mean = (rgb[0] + rgb[1] + rgb[2]) / 3.0;
        let k = RESTRICT_CHROMA / chroma;
        rgb.iter_mut().for_each(|v| *v = (mean + (*v - mean) * k).clamp(0.0, 1.0));
    }
}

/// One channel's walk, [`ENTRIES`] values in `0..=1`. Steps are whole 8-bit levels: a heavy-tailed
/// draw (`tan` of a uniform angle times a cosine for sign) scaled by the roughness amplitude.
fn walk(seed: u32, roughness: f32, restrict: bool) -> Vec<f32> {
    // Step amplitude: 1 at Roughness 0, ~4 at 50 %, 31 at 100 % (Photoshop references).
    let amp = 1.0 + (30.0 * f64::from(roughness.clamp(0.0, 1.0)).powf(10.0 / 3.0)).round();
    // The walk's range in levels: the channel's own with Restrict Colors, else twice it (half
    // a range beyond each end), so it spends about half its time pinned.
    let (lo, hi) = if restrict { (0.0, 255.0) } else { (-128.0, 383.0) };
    let mut rng = Mulberry32(seed);
    let mut x = lo + (hi - lo) * rng.next();
    let mu = std::f64::consts::PI / 2.43;
    let mut step = || {
        let (u1, u2) = (rng.next(), rng.next());
        let v = (mu * (1.0 - u1)).tan() * 325.0 * (u2 * std::f64::consts::TAU).cos() * 256.0;
        // Whole levels, rounded to nearest so the walk has no drift.
        ((amp * v.trunc()) / 65536.0).round()
    };
    (0..ENTRIES)
        .map(|_| {
            x = (x + step()).clamp(lo, hi);
            (x.clamp(0.0, 255.0) / 255.0) as f32
        })
        .collect()
}

/// The Mulberry32 generator (public domain): uniform draws in `[0, 1)`.
struct Mulberry32(u32);

impl Mulberry32 {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x6D2B_79F5);
        let mut t = self.0;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        f64::from(t ^ (t >> 14)) / 4_294_967_296.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photosuite_psd::descriptor::Id;

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

    #[test]
    fn mulberry32_matches_reference_values() {
        let mut r = Mulberry32(0);
        let got: Vec<f64> = (0..4).map(|_| r.next()).collect();
        for (g, want) in got.iter().zip([0.266_429_208_7, 0.000_329_745_7, 0.223_272_027_4, 0.146_202_147_9]) {
            assert!((g - want).abs() < 1e-9, "{got:?}");
        }
    }

    #[test]
    fn noise_ramp_is_deterministic_and_rough_like_photoshop() {
        let n = Noise::from_desc(&desc("RGBC", 2048, false, false)).expect("noise");
        assert_eq!(n.table(), n.table());
        let (stops, opacity) = n.stops();
        assert_eq!(stops.len(), ENTRIES);
        assert!(opacity.is_empty());
        // Roughness 50 %: small steps between entries, about half the samples pinned at an end.
        let t = n.table();
        let mut pinned = 0;
        let mut steps = Vec::new();
        for c in 0..3 {
            for w in t.windows(2) {
                let (a, b) = (w[0][c] * 255.0, w[1][c] * 255.0);
                if a > 0.5 && a < 254.5 && b > 0.5 && b < 254.5 {
                    steps.push((b - a).abs());
                }
            }
            pinned += t.iter().filter(|p| p[c] <= 0.0 || p[c] >= 1.0).count();
        }
        let mean = steps.iter().sum::<f32>() / steps.len().max(1) as f32;
        assert!((1.5..6.0).contains(&mean), "mean step {mean}");
        let frac = pinned as f32 / (3 * ENTRIES) as f32;
        assert!((0.15..0.85).contains(&frac), "pinned {frac}");
    }

    #[test]
    fn restrict_caps_chroma_and_transparency_adds_alpha() {
        let n = Noise::from_desc(&desc("RGBC", 4096, true, true)).expect("noise");
        let t = n.table();
        assert!(t.iter().all(|p| p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2]) <= RESTRICT_CHROMA + 1e-4));
        assert!(t.iter().any(|p| p[3] < 0.9), "alpha varies");
        assert_eq!(n.stops().1.len(), ENTRIES);
        // HSB and Lab produce valid RGB.
        for m in ["HSBl", "LbCl"] {
            let n = Noise::from_desc(&desc(m, 3000, false, false)).expect("noise");
            assert!(n.table().iter().flatten().all(|v| (0.0..=1.0).contains(v)), "{m}");
        }
    }

    #[test]
    fn bounds_clip_and_hostile_values_stay_in_range() {
        let d = desc_with("RGBC", 99_999, false, false, -7, [40, 120, -5, 0], [60, 10, 100, 100]);
        let t = Noise::from_desc(&d).expect("noise").table();
        assert!(t.iter().all(|p| (0.4..=0.6).contains(&p[0])), "red within 40..60 %");
        assert!(t.iter().all(|p| p[1] == 1.0), "minimum above maximum: green stays at the minimum");
        assert!(t.iter().flatten().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
        // Not a noise gradient.
        assert!(Noise::from_desc(&Descriptor::new("Grdn")).is_none());
    }

    #[test]
    fn unedited_noise_fill_keeps_its_definition() {
        use photosuite_psd::descriptor::VersionedDescriptor;
        let grad = desc("RGBC", 2048, false, true);
        let gdfl = Descriptor::new("null").with("Grad", Value::Descriptor(grad.clone()));
        let fill = crate::blocks::fill_from_desc(b"GdFl", &gdfl).expect("fill");
        let photosuite_doc::Fill::Gradient { stops, opacity_stops, .. } = &fill else { panic!("gradient fill") };
        assert_eq!((stops.len(), opacity_stops.len()), (ENTRIES, ENTRIES));
        let old = VersionedDescriptor::new(gdfl).to_bytes();
        let (_, back) = crate::blocks::fill_to_desc_onto(&fill, Some((b"GdFl", &old)));
        assert_eq!(crate::blocks::get_desc(&back, "Grad"), Some(&grad), "noise definition written back verbatim");
    }
}
