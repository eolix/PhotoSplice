//! A gradient's definition as Reference stores it ([`GradientSource`]): read from a `Grdn`
//! descriptor beside the baked stops the model renders, written back as one, and the checks the
//! writers use to tell whether the stops still come from it (they weren't edited).
//!
//! The model bakes Reference's smoothness, midpoints, interpolation method and noise generator
//! into dense linear stops; written as those stops, a gradient renders differently in Reference
//! (round-trip kit 8: noise fills 17–41/255 off, a smooth Outer Glow gradient 8/255, Smooth-method
//! stroke gradients 10/255). A writer writes the definition instead whenever reading it back
//! gives the stops the model holds.

use photosplice_color::Color;
use photosplice_doc::{GradientColorStop, GradientForm, GradientMethod, GradientNoise, GradientOpacityStop, GradientSource, GradientStopKind, NoiseModel};
use photosplice_psd::descriptor::{Descriptor, Id, UnicodeString, Value};

use crate::blocks::{bool_of, color_from_desc, color_to_desc, enum_of, get_desc, num};

/// How far a re-read stop may be from the model's and still count as unedited: float noise of
/// the descriptor round trip (8-bit levels are 4e-3 apart).
const TOLERANCE: f32 = 1e-4;

/// The method a `gradientInterpolationMethodType` code names (`None` for none or an unknown one,
/// which the bake takes as Classic too).
pub(crate) fn method_from_code(code: Option<&[u8]>) -> Option<GradientMethod> {
    Some(match code? {
        b"Gcls" => GradientMethod::Classic,
        b"Perc" => GradientMethod::Perceptual,
        b"Lnr " => GradientMethod::Linear,
        b"Smoo" => GradientMethod::Smooth,
        _ => return None,
    })
}

/// The `gradientInterpolationMethodType` code of `m`.
pub(crate) fn method_code(m: GradientMethod) -> &'static str {
    match m {
        GradientMethod::Classic => "Gcls",
        GradientMethod::Perceptual => "Perc",
        GradientMethod::Linear => "Lnr ",
        GradientMethod::Smooth => "Smoo",
    }
}

/// `m` as the enumerated value Reference writes under `gradientsInterpolationMethod` / `gs99`.
pub(crate) fn method_value(m: GradientMethod) -> Value {
    Value::Enumerated { type_id: Id::new("gradientInterpolationMethodType"), value: Id::new(method_code(m)) }
}

/// The definition of a `Grdn` descriptor, `method` the parent's interpolation method code.
pub(crate) fn from_desc(grad: &Descriptor, method: Option<&[u8]>) -> GradientSource {
    let name = match grad.get("Nm  ") {
        Some(Value::Text(t)) => t.to_string_lossy(),
        _ => String::new(),
    };
    let method = method_from_code(method);
    if enum_of(grad, "GrdF") == Some(b"ClNs".as_slice()) {
        let bounds = |key: &str, default: f64| -> [f64; 4] {
            let list = match grad.get(key) {
                Some(Value::List(items)) => items.as_slice(),
                _ => &[],
            };
            std::array::from_fn(|k| list.get(k).and_then(|v| num(Some(v))).unwrap_or(default))
        };
        let noise = GradientNoise {
            // `RndS` is any 32-bit value; reinterpreting a negative integer keeps its bits.
            seed: num(grad.get("RndS")).map_or(0, |v| v as i64 as u32),
            roughness: num(grad.get("Smth")).map_or(0.5, |v| (v / 4096.0) as f32),
            model: match enum_of(grad, "ClrS") {
                Some(b"HSBl" | b"HSBC") => NoiseModel::Hsb,
                Some(b"LbCl") => NoiseModel::Lab,
                _ => NoiseModel::Rgb,
            },
            restrict: bool_of(grad, "VctC"),
            transparency: bool_of(grad, "ShTr"),
            min: bounds("Mnm ", 0.0),
            max: bounds("Mxm ", 100.0),
        };
        return GradientSource { name, method, form: GradientForm::Noise, noise: Some(noise), ..GradientSource::default() };
    }
    let items = |key: &str| match grad.get(key) {
        Some(Value::List(items)) => items.iter().filter_map(|v| if let Value::Descriptor(d) = v { Some(d) } else { None }).collect(),
        _ => Vec::new(),
    };
    let at = |s: &Descriptor| num(s.get("Lctn")).unwrap_or(0.0) as f32 / 4096.0;
    let mid = |s: &Descriptor| num(s.get("Mdpn")).unwrap_or(50.0) as f32 / 100.0;
    let colors = items("Clrs")
        .into_iter()
        .filter_map(|s| {
            let color = get_desc(s, "Clr ").and_then(color_from_desc)?;
            let kind = match enum_of(s, "Type") {
                Some(b"FrgC") => GradientStopKind::Foreground,
                Some(b"BckC") => GradientStopKind::Background,
                _ => GradientStopKind::User,
            };
            Some(GradientColorStop { location: at(s), midpoint: mid(s), color, kind })
        })
        .collect();
    let opacity = items("Trns")
        .into_iter()
        .map(|s| GradientOpacityStop { location: at(s), midpoint: mid(s), opacity: num(s.get("Opct")).unwrap_or(100.0) as f32 / 100.0 })
        .collect();
    let smoothness = num(grad.get("Intr")).map_or(0.0, |v| (v / 4096.0) as f32);
    GradientSource { name, method, form: GradientForm::Solid, smoothness, colors, opacity, noise: None }
}

/// A 0..4096 location (or roughness) of a `0..=1` value; non-finite values are 0.
fn loc(v: f32) -> i32 {
    if v.is_finite() { (v * 4096.0).round().clamp(-1e9, 1e9) as i32 } else { 0 }
}

/// A percentage of a `0..=1` value; non-finite values are 0.
fn pct(v: f32) -> i32 {
    if v.is_finite() { (v * 100.0).round().clamp(-1e9, 1e9) as i32 } else { 0 }
}

/// `src` as the `Grdn` descriptor Reference writes (key order and value types as its own).
pub(crate) fn to_desc(src: &GradientSource) -> Descriptor {
    let d = Descriptor::new("Grdn").with("Nm  ", Value::Text(UnicodeString::new_nul(&src.name)));
    let form = |v: &str| Value::Enumerated { type_id: Id::new("GrdF"), value: Id::new(v) };
    if let (GradientForm::Noise, Some(n)) = (src.form, &src.noise) {
        let model = match n.model {
            NoiseModel::Rgb => "RGBC",
            NoiseModel::Hsb => "HSBl",
            NoiseModel::Lab => "LbCl",
        };
        let list = |v: &[f64; 4]| Value::List(v.iter().map(|x| Value::Double(*x)).collect());
        return d
            .with("GrdF", form("ClNs"))
            .with("ShTr", Value::Boolean(n.transparency))
            .with("VctC", Value::Boolean(n.restrict))
            .with("ClrS", Value::Enumerated { type_id: Id::new("ClrS"), value: Id::new(model) })
            .with("RndS", Value::Integer(i32::from_ne_bytes(n.seed.to_ne_bytes())))
            .with("Smth", Value::Integer(loc(n.roughness)))
            .with("Mnm ", list(&n.min))
            .with("Mxm ", list(&n.max));
    }
    let clrs = src
        .colors
        .iter()
        .map(|s| {
            let kind = match s.kind {
                GradientStopKind::User => "UsrS",
                GradientStopKind::Foreground => "FrgC",
                GradientStopKind::Background => "BckC",
            };
            Value::Descriptor(
                Descriptor::new("Clrt")
                    .with("Clr ", Value::Descriptor(color_to_desc(&s.color)))
                    .with("Type", Value::Enumerated { type_id: Id::new("Clry"), value: Id::new(kind) })
                    .with("Lctn", Value::Integer(loc(s.location)))
                    .with("Mdpn", Value::Integer(pct(s.midpoint))),
            )
        })
        .collect();
    let trns = src
        .opacity
        .iter()
        .map(|s| {
            Value::Descriptor(
                Descriptor::new("TrnS")
                    .with("Opct", Value::UnitFloat { unit: *b"#Prc", value: f64::from(s.opacity * 100.0) })
                    .with("Lctn", Value::Integer(loc(s.location)))
                    .with("Mdpn", Value::Integer(pct(s.midpoint))),
            )
        })
        .collect();
    d.with("GrdF", form("CstS"))
        .with("Intr", Value::Double(if src.smoothness.is_finite() { f64::from(src.smoothness) * 4096.0 } else { 0.0 }))
        .with("Clrs", Value::List(clrs))
        .with("Trns", Value::List(trns))
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() <= TOLERANCE || a.to_bits() == b.to_bits()
}

/// Whether two stop lists agree (same count; locations and values within float noise).
pub(crate) fn same_stops<T>(a: &[(f32, T)], b: &[(f32, T)], same: impl Fn(&T, &T) -> bool) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| close(x.0, y.0) && same(&x.1, &y.1))
}

/// Whether two colours agree: the same model, channels and alpha within float noise.
pub(crate) fn same_color(a: &Color, b: &Color) -> bool {
    a.mode == b.mode && a.c.iter().zip(&b.c).all(|(x, y)| close(*x, *y)) && close(a.alpha, b.alpha)
}

/// [`same_stops`] for colour stops.
pub(crate) fn same_color_stops(a: &[(f32, Color)], b: &[(f32, Color)]) -> bool {
    same_stops(a, b, same_color)
}

/// [`same_stops`] for opacity stops.
pub(crate) fn same_opacity_stops(a: &[(f32, f32)], b: &[(f32, f32)]) -> bool {
    same_stops(a, b, |x, y| close(*x, *y))
}

/// Whether two lists of numbers agree within float noise.
pub(crate) fn same_floats(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| close(*x, *y))
}

/// Whether two value lists agree within float noise.
pub(crate) fn same_values<const N: usize>(a: &[[f32; N]], b: &[[f32; N]]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.iter().zip(y).all(|(p, q)| close(*p, *q)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grad() -> Descriptor {
        let stop = |c: Descriptor, at: i32, mid: i32| {
            Value::Descriptor(
                Descriptor::new("Clrt")
                    .with("Clr ", Value::Descriptor(c))
                    .with("Type", Value::Enumerated { type_id: Id::new("Clry"), value: Id::new("UsrS") })
                    .with("Lctn", Value::Integer(at))
                    .with("Mdpn", Value::Integer(mid)),
            )
        };
        let op = |o: f64, at: i32| {
            Value::Descriptor(
                Descriptor::new("TrnS")
                    .with("Opct", Value::UnitFloat { unit: *b"#Prc", value: o })
                    .with("Lctn", Value::Integer(at))
                    .with("Mdpn", Value::Integer(30)),
            )
        };
        let cmyk = Descriptor::new("CMYC")
            .with("Cyn ", Value::Double(10.0))
            .with("Mgnt", Value::Double(80.0))
            .with("Ylw ", Value::Double(0.0))
            .with("Blck", Value::Double(5.0));
        let lab = Descriptor::new("LbCl").with("Lmnc", Value::Double(50.0)).with("A   ", Value::Double(20.0)).with("B   ", Value::Double(-30.0));
        Descriptor::new("Grdn")
            .with("Nm  ", Value::Text(UnicodeString::new_nul("Заказная")))
            .with("GrdF", Value::Enumerated { type_id: Id::new("GrdF"), value: Id::new("CstS") })
            .with("Intr", Value::Double(3072.0))
            .with("Clrs", Value::List(vec![stop(cmyk, 0, 50), stop(lab, 2874, 25)]))
            .with("Trns", Value::List(vec![op(100.0, 0), op(40.0, 4096)]))
    }

    /// A definition read from a `Grdn` is written back as the same descriptor: colour models,
    /// stop types, midpoints of both kinds, smoothness and name.
    #[test]
    fn solid_definition_round_trips_exactly() {
        let d = grad();
        let src = from_desc(&d, Some(b"Smoo"));
        assert_eq!(src.method, Some(GradientMethod::Smooth));
        assert_eq!(src.colors.len(), 2);
        assert_eq!(src.colors[1].color.mode, photosplice_color::ColorMode::Lab);
        let back = to_desc(&src);
        assert_eq!(back.to_bytes().len(), d.to_bytes().len());
        assert_eq!(from_desc(&back, Some(b"Smoo")), src);
    }

    #[test]
    fn noise_definition_round_trips_exactly() {
        let list = |v: [f64; 4]| Value::List(v.iter().map(|x| Value::Double(*x)).collect());
        let d = Descriptor::new("Grdn")
            .with("Nm  ", Value::Text(UnicodeString::new_nul("Custom")))
            .with("GrdF", Value::Enumerated { type_id: Id::new("GrdF"), value: Id::new("ClNs") })
            .with("ShTr", Value::Boolean(true))
            .with("VctC", Value::Boolean(false))
            .with("ClrS", Value::Enumerated { type_id: Id::new("ClrS"), value: Id::new("HSBl") })
            .with("RndS", Value::Integer(-12345))
            .with("Smth", Value::Integer(2048))
            .with("Mnm ", list([8.3343505859375, 60.0006103515625, 0.0, 0.0]))
            .with("Mxm ", list([79.998779296875, 100.0, 100.0, 100.0]));
        let src = from_desc(&d, None);
        assert_eq!(src.form, GradientForm::Noise);
        assert_eq!(to_desc(&src), d);
    }

    #[test]
    fn hostile_values_do_not_panic() {
        let src = GradientSource {
            smoothness: f32::NAN,
            colors: vec![GradientColorStop { location: f32::INFINITY, midpoint: f32::NAN, color: Color::BLACK, kind: GradientStopKind::Background }],
            opacity: vec![GradientOpacityStop { location: -1e30, midpoint: 1e30, opacity: f32::NAN }],
            ..GradientSource::default()
        };
        let d = to_desc(&src);
        assert_eq!(from_desc(&d, None).colors.len(), 1);
        assert!(!same_color_stops(&[(0.0, Color::BLACK)], &[]));
    }
}
