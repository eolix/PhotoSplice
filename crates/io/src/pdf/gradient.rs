//! Gradients as PDF shadings, laid out exactly as PhotoSuite's renderers draw them.
//!
//! Two renderers draw gradients: shape layers through `photosuite_vector::Paint` (colour stops,
//! laid out over the path's control bounds) and Gradient Fill layers through
//! `photosuite_compose::gradient_fill` (colour and opacity stops with midpoints, laid out over the
//! layer's frame, Linear and Reflected end points on whole pixels). The geometry is read back
//! from the renderer itself (its gradient parameter at a few points) rather than re-derived, so
//! the PDF follows whatever the renderer does. Both ramps are piecewise linear in the parameter,
//! so sampling them at their breakpoints gives PDF stops that reproduce them exactly.
//!
//! Linear becomes an axial shading, Reflected an axial one twice as long with mirrored stops,
//! Radial a radial one, Angle a sweep. Diamond has no PDF shading: `None`, and the caller draws
//! the layer as an image.

use krilla::color::rgb;
use krilla::geom::Transform;
use krilla::num::NormalizedF32;
use krilla::paint::{LinearGradient, Paint, RadialGradient, SpreadMethod, Stop, SweepGradient};
use photosuite_doc::{Fill, GradientStyle};
use photosuite_geom::Rect;
use photosuite_vector::Paint as VPaint;

/// Where the parameter runs, in document pixels.
enum Layout {
    /// Linear parameter 0 at `p0`, 1 at `p1` (for Reflected: the reflected parameter is
    /// `|2 t − 1|` of this one).
    Axial { p0: [f64; 2], p1: [f64; 2] },
    /// Radial and Angle: the centre, the radius at t = 1 (Radial) and the start angle (Angle,
    /// degrees counter-clockwise from 3 o'clock, sweeping clockwise).
    Centred { c: [f64; 2], r: f64, angle: f64 },
}

/// The krilla paint of a shape layer's fill or stroke `f` (a gradient), as
/// `photosuite_vector::Paint::from_fill(f, bounds)` renders it.
pub(super) fn shape_paint(f: &Fill, bounds: (f64, f64, f64, f64)) -> Option<Paint> {
    let Fill::Gradient { stops, angle, scale, style, reverse, .. } = f else { return None };
    // The renderer's parameter: its own paint with a black → white ramp, read in the red channel.
    let probe = |style: GradientStyle| VPaint::Gradient {
        stops: vec![(0.0, [0.0, 0.0, 0.0, 1.0]), (1.0, [1.0, 1.0, 1.0, 1.0])],
        style,
        angle: *angle,
        scale: *scale,
        reverse: false,
        bounds,
    };
    let c = [(bounds.0 + bounds.2) / 2.0, (bounds.1 + bounds.3) / 2.0];
    let layout = match style {
        GradientStyle::Linear | GradientStyle::Reflected => {
            let p = probe(GradientStyle::Linear);
            axial(|x, y| f64::from(p.at(x, y)[0]), c)?
        }
        GradientStyle::Radial | GradientStyle::Angle => {
            let p = probe(GradientStyle::Radial);
            let t = |d: f64| f64::from(p.at(c[0] + d, c[1])[0]);
            // t = distance / r: read once near the centre, then again inside the radius.
            let r0 = 1.0 / t(1.0).max(1e-9);
            let d = (r0 / 2.0).max(1e-3);
            let r = d / t(d).max(1e-9);
            Layout::Centred { c, r, angle: f64::from(*angle) }
        }
        GradientStyle::Diamond => return None,
    };
    // The ramp: its own paint over a unit frame, where the parameter is x.
    let ramp = VPaint::Gradient {
        stops: stops.iter().map(|(p, c)| (*p, rgba(c))).collect(),
        style: GradientStyle::Linear,
        angle: 0.0,
        scale: 1.0,
        reverse: false,
        bounds: (0.0, 0.0, 1.0, 1.0),
    };
    let mut at: Vec<f32> = stops.iter().map(|s| s.0).collect();
    at.extend([0.0, 1.0]);
    let samples = sample(&at, |t| ramp.at(f64::from(t), 0.5));
    paint(*style, *reverse, layout, samples)
}

/// The krilla paint of a Gradient Fill layer's fill `f`, laid out in `frame` (see
/// `photosuite_compose::fill_frame`), as `photosuite_compose::gradient_fill::render` draws it.
pub(super) fn fill_paint(f: &Fill, frame: Rect) -> Option<Paint> {
    use photosuite_compose::{effects, fill_layout, gradient_fill};
    let Fill::Gradient { stops, angle, scale, style, reverse, opacity_stops, midpoints, offset, .. } = f else { return None };
    let layout = match style {
        GradientStyle::Linear | GradientStyle::Reflected => {
            // The renderer's own layout (whole-pixel end points), its parameter as Linear has it.
            let (a, s, o) = fill_layout::gradient_layout(*style, *angle, *scale, *offset, frame);
            let t = |x: f64, y: f64| f64::from(effects::gradient_t(GradientStyle::Linear, a, s, false, o, frame, x as f32, y as f32));
            let (h0, h1) = gradient_fill::handles(*style, a, s, o, frame);
            let mid = [f64::from(h0[0] + h1[0]) / 2.0, f64::from(h0[1] + h1[1]) / 2.0];
            axial(t, if *style == GradientStyle::Reflected { [f64::from(h0[0]), f64::from(h0[1])] } else { mid })?
        }
        GradientStyle::Radial | GradientStyle::Angle => {
            let (c, e) = gradient_fill::handles(*style, *angle, *scale, *offset, frame);
            let r = f64::from(e[0] - c[0]).hypot(f64::from(e[1] - c[1]));
            Layout::Centred { c: [f64::from(c[0]), f64::from(c[1])], r, angle: f64::from(*angle) }
        }
        GradientStyle::Diamond => return None,
    };
    let ramp = gradient_fill::Ramp::new(f)?;
    // Breakpoints: colour stops, the midpoints between them, opacity stops.
    let mut sorted: Vec<f32> = stops.iter().map(|s| s.0).collect();
    sorted.sort_by(f32::total_cmp);
    let mut at = sorted.clone();
    for (i, w) in sorted.windows(2).enumerate() {
        let m = midpoints.get(i).copied().unwrap_or(0.5).clamp(0.05, 0.95);
        at.push(w[0] + (w[1] - w[0]) * m);
    }
    at.extend(opacity_stops.iter().map(|s| s.0));
    at.extend([0.0, 1.0]);
    let samples = sample(&at, |t| ramp.sample(t));
    paint(*style, *reverse, layout, samples)
}

fn rgba(c: &photosuite_color::Color) -> [f32; 4] {
    let v = c.to_rgb();
    [v[0], v[1], v[2], c.alpha]
}

/// The axial layout of an affine parameter `t` (clamped to 0…1 by the renderer), read at points
/// around `c`, where it is inside that range.
fn axial(t: impl Fn(f64, f64) -> f64, c: [f64; 2]) -> Option<Layout> {
    // A step small enough to stay inside 0…1 for any gradient a pixel long or more.
    let d = 0.25;
    let t0 = t(c[0], c[1]);
    let (gx, gy) = ((t(c[0] + d, c[1]) - t(c[0] - d, c[1])) / (2.0 * d), (t(c[0], c[1] + d) - t(c[0], c[1] - d)) / (2.0 * d));
    let g2 = gx * gx + gy * gy;
    if !(g2.is_finite() && g2 > 1e-12) {
        return None;
    }
    // Along the gradient from `c`, the points where t is 0 and 1.
    let at = |v: f64| [c[0] + gx * (v - t0) / g2, c[1] + gy * (v - t0) / g2];
    Some(Layout::Axial { p0: at(0.0), p1: at(1.0) })
}

/// The ramp at `at` (sorted, deduplicated, inside 0…1).
fn sample(at: &[f32], f: impl Fn(f32) -> [f32; 4]) -> Vec<(f32, [f32; 4])> {
    let mut at: Vec<f32> = at.iter().map(|t| t.clamp(0.0, 1.0)).filter(|t| t.is_finite()).collect();
    at.sort_by(f32::total_cmp);
    at.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
    at.into_iter().map(|t| (t, f(t))).collect()
}

fn paint(style: GradientStyle, reverse: bool, layout: Layout, mut samples: Vec<(f32, [f32; 4])>) -> Option<Paint> {
    if reverse {
        samples = samples.into_iter().rev().map(|(t, c)| (1.0 - t, c)).collect();
    }
    let stops = |s: &[(f32, [f32; 4])]| -> Vec<Stop> { s.iter().map(|(t, c)| stop(*t, *c)).collect() };
    let f = |v: f64| v as f32;
    Some(match (style, layout) {
        (GradientStyle::Linear, Layout::Axial { p0, p1 }) => {
            LinearGradient { x1: f(p0[0]), y1: f(p0[1]), x2: f(p1[0]), y2: f(p1[1]), transform: Transform::default(), spread_method: SpreadMethod::Pad, stops: stops(&samples), anti_alias: false }
                .into()
        }
        // Reflected: t = |2 u − 1| along the axial u, so each stop at q sits at (1 ∓ q) / 2.
        (GradientStyle::Reflected, Layout::Axial { p0, p1 }) => {
            let mut mirrored: Vec<(f32, [f32; 4])> = samples.iter().map(|(q, c)| ((1.0 - q) / 2.0, *c)).chain(samples.iter().map(|(q, c)| ((1.0 + q) / 2.0, *c))).collect();
            mirrored.sort_by(|a, b| a.0.total_cmp(&b.0));
            mirrored.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-6);
            LinearGradient { x1: f(p0[0]), y1: f(p0[1]), x2: f(p1[0]), y2: f(p1[1]), transform: Transform::default(), spread_method: SpreadMethod::Pad, stops: stops(&mirrored), anti_alias: false }
                .into()
        }
        (GradientStyle::Radial, Layout::Centred { c, r, .. }) if r > 0.0 => RadialGradient {
            fx: f(c[0]),
            fy: f(c[1]),
            fr: 0.0,
            cx: f(c[0]),
            cy: f(c[1]),
            cr: f(r),
            transform: Transform::default(),
            spread_method: SpreadMethod::Pad,
            stops: stops(&samples),
            anti_alias: false,
        }
        .into(),
        // Angle: t runs clockwise on screen from the gradient's angle. krilla's sweep angles grow
        // from +x towards +y (clockwise on this y-down page) and pad outside start…end, so the
        // sweep runs 0…360 and is turned to start at the angle.
        (GradientStyle::Angle, Layout::Centred { c, angle, .. }) => SweepGradient {
            cx: f(c[0]),
            cy: f(c[1]),
            start_angle: 0.0,
            end_angle: 360.0,
            transform: Transform::from_rotate_at(f(-angle), f(c[0]), f(c[1])),
            spread_method: SpreadMethod::Pad,
            stops: stops(&samples),
            anti_alias: false,
        }
        .into(),
        _ => return None,
    })
}

fn stop(t: f32, c: [f32; 4]) -> Stop {
    let u = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Stop {
        offset: NormalizedF32::new(t.clamp(0.0, 1.0)).unwrap_or(NormalizedF32::ZERO),
        color: rgb::Color::new(u(c[0]), u(c[1]), u(c[2])).into(),
        opacity: NormalizedF32::new(c[3].clamp(0.0, 1.0)).unwrap_or(NormalizedF32::ONE),
    }
}
