//! Gradient geometry and stop sampling shared by gradient fills, gradient effects (in
//! `photosplice-compose`) and shape layers filled or stroked with a gradient (here), so every
//! gradient Photoshop draws from the same settings lands on the same pixels.

use photosplice_doc::GradientStyle;
use photosplice_geom::Rect;

/// The lengths a 100 % gradient at `angle` spans in a `w` × `h` frame: the chord (Linear and
/// Reflected) and the ellipse-norm length (Radial, Angle, Diamond; its half is the radius).
pub fn gradient_units(angle: f32, w: f32, h: f32) -> (f32, f32) {
    let (s, c) = angle.to_radians().sin_cos();
    // Gradient length: the bounds' extent along the angle as an ellipse
    // norm (a unit gradient scaled to the bounds); fitted on psd-tools
    // gradient-styles.psd (cached Photoshop renderings of every style).
    let len = ((c * w).powi(2) + (s * h).powi(2)).sqrt().max(1.0);
    // Linear / Reflected span the chord of the bounds through their centre along the angle:
    // min(w / |cos|, h / |sin|). A 45° gradient on a 29 px square runs 41 px (psd-tools
    // shape-fx2), an 87° one on a 600 × 60 text line 60 px (layer_effects); axis-aligned angles
    // span the width / height.
    let chord = (w / c.abs().max(1e-6)).min(h / s.abs().max(1e-6)).max(1.0);
    (chord, len)
}

/// Gradient parameter `t` in `0..=1` for pixel centre `(x, y)` inside `bounds`.
#[allow(clippy::too_many_arguments)]
pub fn gradient_t(style: GradientStyle, angle: f32, scale: f32, reverse: bool, offset: (f32, f32), bounds: Rect, x: f32, y: f32) -> f32 {
    let w = bounds.width().max(1) as f32;
    let h = bounds.height().max(1) as f32;
    let cx = bounds.x0 as f32 + w / 2.0 + offset.0 * w;
    let cy = bounds.y0 as f32 + h / 2.0 + offset.1 * h;
    let (s, c) = angle.to_radians().sin_cos();
    let (dx, dy) = (x - cx, y - cy);
    // Distance along the gradient direction (y axis points down).
    let along = dx * c - dy * s;
    let across = dx * s + dy * c;
    let (chord, len) = gradient_units(angle, w, h);
    let (chord, len) = (chord * scale.max(1e-3), len * scale.max(1e-3));
    // Linear / Reflected sample the pixel's top-left corner, half a pixel before its centre
    // (with the whole-pixel end points of `fill_layout`: layer_effects' overlay 5.8 → 1.8/255,
    // shape-fx2 2.8 → 1.1/255, gradient-fill.psd 1.9 → 0.5/255).
    let corner = 0.5 * (c - s);
    let mut t = match style {
        GradientStyle::Linear => (along - corner) / chord + 0.5,
        GradientStyle::Reflected => ((along - corner) / (chord / 2.0)).abs(),
        GradientStyle::Radial => (dx * dx + dy * dy).sqrt() / (len / 2.0),
        GradientStyle::Diamond => (along.abs() + across.abs()) / (len / 2.0),
        // Clockwise sweep starting at the gradient angle.
        GradientStyle::Angle => ((angle.to_radians() - (-dy).atan2(dx)) / std::f32::consts::TAU).rem_euclid(1.0),
    };
    t = t.clamp(0.0, 1.0);
    if reverse { 1.0 - t } else { t }
}

/// Where a segment of relative position `u` lands once its midpoint is `m` (0.5 = unchanged):
/// the colour is half-way at `m`, linear on either side.
pub fn midpoint_remap(u: f32, m: f32) -> f32 {
    let m = m.clamp(0.05, 0.95);
    if (m - 0.5).abs() < 1e-6 {
        u
    } else if u <= m {
        0.5 * u / m
    } else {
        0.5 + 0.5 * (u - m) / (1.0 - m)
    }
}

/// A gradient's colour at `t`: `stops` (sorted, straight RGBA) blended along each segment through
/// its midpoint (`mids[i]` for the segment after stop `i`; 0.5 when absent), the alpha scaled by
/// the opacity `opacity` stops (sorted; none = opaque).
pub fn sample(stops: &[(f32, [f32; 4])], mids: &[f32], opacity: &[(f32, f32)], t: f32) -> [f32; 4] {
    let mut c = match segment(stops, t) {
        Ok((i, u)) => match (stops.get(i), stops.get(i + 1)) {
            (Some(a), Some(b)) => {
                let k = midpoint_remap(u, mids.get(i).copied().unwrap_or(0.5));
                std::array::from_fn(|ch| a.1[ch] + (b.1[ch] - a.1[ch]) * k)
            }
            _ => [0.0; 4],
        },
        Err(j) => stops.get(j).map_or([0.0; 4], |s| s.1),
    };
    if !opacity.is_empty() {
        c[3] *= match segment(opacity, t) {
            Ok((i, u)) => match (opacity.get(i), opacity.get(i + 1)) {
                (Some(a), Some(b)) => a.1 + (b.1 - a.1) * u,
                _ => 1.0,
            },
            Err(j) => opacity.get(j).map_or(1.0, |s| s.1),
        };
    }
    c
}

/// Where `t` falls among `stops` sorted by location: `Ok((i, u))` inside the segment from stop `i`
/// to `i + 1` at fraction `u`, or `Err(j)`, the end stop it clamps to. A binary search.
fn segment<T>(stops: &[(f32, T)], t: f32) -> Result<(usize, f32), usize> {
    let j = stops.partition_point(|s| s.0 < t);
    match (j.checked_sub(1).and_then(|i| stops.get(i)), stops.get(j)) {
        (Some(a), Some(b)) => Ok((j - 1, if b.0 > a.0 { (t - a.0) / (b.0 - a.0) } else { 0.0 })),
        (None, _) => Err(0),
        (Some(_), None) => Err(stops.len().saturating_sub(1)),
    }
}
