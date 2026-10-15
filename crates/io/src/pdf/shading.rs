//! PDF axial and radial shadings as Photoshop gradients (a Gradient Fill layer the path masks).
//!
//! hayro-interpret turns the shading into an SVG-like gradient (`as_svg_gradient`): its line or
//! circles, the transform into page pixels and sRGB stops within a colour tolerance of the
//! shading's function. It declines what such a gradient can't hold (an end that doesn't extend
//! over the path, a radial background); here, too, what a Photoshop gradient can't hold is
//! declined (radials with a moved focus or an inner circle, skewed or squashed circles), and the
//! caller draws those as pixels.

use hayro::hayro_interpret::encode::EncodedShadingType;
use hayro::hayro_interpret::gradient::SvgGradientKind;
use hayro::hayro_interpret::pattern::ShadingPattern;
use hayro::kurbo::{self, Point};
use photosplice_color::Color;
use photosplice_doc::{Fill, GradientStyle};

/// How far apart (pixels) two points or radii may be and count as the same.
const SAME: f64 = 0.5;

/// A gradient fill's geometry in page pixels: `(style, from, to, stops)` for
/// `photosplice_compose::gradient_fill::from_handles` (Linear: the ends; Radial: the centre and a
/// point on the circle where t = 1).
pub(super) struct Gradient {
    pub style: GradientStyle,
    pub from: [f32; 2],
    pub to: [f32; 2],
    pub stops: Vec<(f32, Color)>,
}

/// The shading `sp` over a path whose bounds (page pixels) are `bbox`, as a gradient.
pub(super) fn gradient(sp: &ShadingPattern, bbox: kurbo::Rect) -> Option<Gradient> {
    let encoded = sp.encode();
    let EncodedShadingType::RadialAxial(g) = &encoded.shading_type else { return None };
    let native = g.as_svg_gradient(&encoded, bbox, 0.004)?;
    let t = native.transform;
    let p = |q: Point| {
        let q = t * q;
        [q.x as f32, q.y as f32]
    };
    let (style, from, to) = match native.kind {
        SvgGradientKind::Linear { start, end } => (GradientStyle::Linear, p(start), p(end)),
        SvgGradientKind::Radial { start_center, start_radius, end_center, end_radius } => {
            // Circles stay circles only under a scale and rotation.
            let [a, b, c, d, _, _] = t.as_coeffs();
            let (sx, sy) = (a.hypot(b), c.hypot(d));
            if (sx - sy).abs() > 1e-3 * sx.max(sy) || (a * c + b * d).abs() > 1e-3 * sx * sy {
                return None;
            }
            let (c0, c1) = (t * start_center, t * end_center);
            let (r0, r1) = (f64::from(start_radius) * sx, f64::from(end_radius) * sx);
            // Photoshop's radial grows from its centre: same centres, no inner circle.
            if c0.distance(c1) > SAME || r0 > SAME || r1 <= SAME {
                return None;
            }
            (GradientStyle::Radial, [c1.x as f32, c1.y as f32], [(c1.x + r1) as f32, c1.y as f32])
        }
    };
    let stops = native.stops.iter().map(|s| (s.offset.clamp(0.0, 1.0), Color::rgba(s.color[0], s.color[1], s.color[2], s.color[3]))).collect();
    Some(Gradient { style, from, to, stops })
}

/// The `Fill` of `g` laid out in `frame` (the layer's frame, `photosplice_compose::fill_frame`).
pub(super) fn fill(g: Gradient, frame: photosplice_geom::Rect) -> Fill {
    let (angle, scale, offset) = photosplice_compose::gradient_fill::from_handles(g.style, g.from, g.to, frame, 0.0);
    Fill::Gradient {
        stops: g.stops,
        angle,
        scale,
        style: g.style,
        reverse: false,
        opacity_stops: Vec::new(),
        midpoints: Vec::new(),
        offset,
        dither: false,
        align: true,
    }
}
