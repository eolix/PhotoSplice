//! CMYK documents composited in CMYK, as Photoshop does.
//!
//! Photoshop blends a CMYK document channel by channel on its stored values (1 − ink), not on a
//! screen conversion: blending two layers' inks that way reproduces its merged CMYK image within
//! 1-3 levels for every separable mode (`photosuite-io` example `cmyk_probe` on psd-tools
//! `cmyk-blend-modes.psd`), while the same blends of their RGB conversions miss almost every
//! pixel. The working pixel here is `[1 − C, 1 − M, 1 − Y, 1 − K, alpha]` (straight alpha), and
//! the composite turns into screen RGB once, at the end, through the document's CMYK profile
//! (the caller's [`photosuite_color::convert::with_cmyk_space`] scope).
//!
//! Adjustments work on the inks too: Levels and Curves through their CMYK records, Invert,
//! Posterize and Brightness/Contrast through one curve for every ink, Threshold and Gradient Map
//! on the inks' own lightness ([`ink_lightness`]), Hue/Saturation, Color Balance, Selective
//! Color and Photo Filter on C, M, Y as an RGB colour with K kept. A layer this module doesn't composite in CMYK
//! yet (layer effects, Blend If, the other adjustments) is an "RGB island": the RGB compositor
//! draws it over the backdrop's RGB conversion, and only the pixels it changed take the result
//! back, separated through the profile. Untouched pixels keep their exact inks.

use photosuite_color::ColorMode;
use photosuite_color::blend::BlendMode;
use photosuite_color::convert::{cmyk_to_rgb, rgb_to_cmyk};
use photosuite_doc::{Adjustment, Color, Fill, Layer, LayerContent};
use photosuite_geom::Rect;
use photosuite_raster::Surface;

use crate::{Buffer, Ctx};

/// `[1 − C, 1 − M, 1 − Y, 1 − K, alpha]`, straight alpha.
pub(crate) type Px = [f32; 5];

/// A CMYK working buffer (see [`Px`]).
pub(crate) struct CBuf {
    pub rect: Rect,
    pub px: Vec<Px>,
}

impl CBuf {
    pub(crate) fn transparent(rect: Rect) -> Self {
        CBuf { rect, px: vec![[0.0; 5]; rect.width() as usize * rect.height() as usize] }
    }
}

#[inline]
fn from_inks(c: [f32; 4], alpha: f32) -> Px {
    [1.0 - c[0], 1.0 - c[1], 1.0 - c[2], 1.0 - c[3], alpha]
}

#[inline]
fn inks(p: &Px) -> [f32; 4] {
    [1.0 - p[0], 1.0 - p[1], 1.0 - p[2], 1.0 - p[3]]
}

/// A screen colour separated through the document's profile.
#[inline]
fn from_rgba(p: [f32; 4]) -> Px {
    from_inks(rgb_to_cmyk([p[0], p[1], p[2]]), p[3])
}

/// The screen colour of a working pixel.
#[inline]
pub(crate) fn to_rgba(p: &Px) -> [f32; 4] {
    if p[4] <= 0.0 {
        return [0.0; 4];
    }
    let r = cmyk_to_rgb(inks(p));
    [r[0], r[1], r[2], p[4]]
}

/// A colour's inks: its own when it is defined in CMYK, else separated through the profile.
fn color_inks(c: &Color) -> [f32; 4] {
    if c.mode == ColorMode::Cmyk { c.c } else { rgb_to_cmyk(c.to_rgb()) }
}

/// The composite of `layers` over `rect` (transparent backdrop) in CMYK.
pub(crate) fn composite(layers: &[Layer], rect: Rect, cx: &Ctx) -> CBuf {
    let mut b = CBuf::transparent(rect);
    stack(layers, &mut b, cx);
    b
}

/// The composite of `layers` over `rect` as screen RGB.
pub(crate) fn render(layers: &[Layer], rect: Rect, cx: &Ctx) -> Buffer {
    let c = composite(layers, rect, cx);
    Buffer { rect, px: c.px.iter().map(to_rgba).collect() }
}

/// Composite a sibling list (bottom → top) onto `backdrop`, as [`crate::composite_stack`].
fn stack(layers: &[Layer], backdrop: &mut CBuf, cx: &Ctx) {
    let mut i = 0;
    while i < layers.len() {
        let base = &layers[i];
        let mut j = i + 1;
        while j < layers.len() && layers[j].clipped && !base.clipped {
            j += 1;
        }
        let clipped = &layers[i + 1..j];
        if base.visible {
            layer(base, clipped, backdrop, cx);
            if let (LayerContent::Adjustment(_), Some(q)) = (&base.content, crate::adjustment_quantum(cx.depth)) {
                for p in &mut backdrop.px {
                    for v in p.iter_mut() {
                        *v = (*v * q + 0.5).floor() / q;
                    }
                }
            }
        }
        i = j.max(i + 1);
    }
}

/// Whether `l` is composited in CMYK here (else it is an RGB island). Clipped layers are decided
/// one by one when they are applied.
fn native(l: &Layer, clipped: &[Layer], cx: &Ctx) -> bool {
    if crate::blend_if_active(l, cx.mode) || crate::effects::has_effects(l) {
        return false;
    }
    let any_clipped = clipped.iter().any(|c| c.visible);
    match &l.content {
        LayerContent::Group(_) => !(l.blend == BlendMode::PassThrough && l.fill_opacity >= 1.0 && any_clipped),
        LayerContent::Adjustment(a) => native_adjustment(a),
        // A stroked shape with clipped layers keeps its vector stroke above them (`split_parts`).
        LayerContent::Shape(sh) => !(sh.stroke.is_some() && any_clipped),
        _ => true,
    }
}

/// Adjustments evaluated on the inks: each ink's complement through a curve of its own (Levels
/// and Curves in their CMYK records) or the same curve for all four.
fn native_adjustment(a: &Adjustment) -> bool {
    use photosuite_doc::adjust::ToneSpace;
    matches!(
        a,
        Adjustment::Invert
            | Adjustment::Threshold { .. }
            | Adjustment::GradientMap { .. }
            | Adjustment::HueSaturation { colorize: false, .. }
            | Adjustment::ColorBalance { .. }
            | Adjustment::SelectiveColor { .. }
            | Adjustment::PhotoFilter { .. }
            | Adjustment::Posterize { .. }
            | Adjustment::BrightnessContrast { .. }
            | Adjustment::Levels { space: ToneSpace::Cmyk, .. }
            | Adjustment::Curves { space: ToneSpace::Cmyk, .. }
    )
}

/// A pixel's lightness as Photoshop's CMYK adjustments measure it from the inks alone (no
/// profile): the complements of C, M, Y weighted 0.30 / 0.59 / 0.11, times 1 − K.
fn ink_lightness(p: &Px) -> f32 {
    (0.30 * p[0] + 0.59 * p[1] + 0.11 * p[2]) * p[3]
}

/// The four ink curves of a [`native_adjustment`] (on 1 − ink).
fn ink_luts(a: &Adjustment, cx: &Ctx) -> [Vec<f32>; 4] {
    if matches!(a, Adjustment::Levels { .. } | Adjustment::Curves { .. }) {
        return crate::adjust::tone_luts_depth(a, Some(cx.depth));
    }
    // One curve for every channel: the RGB adjustment sampled on a gray ramp.
    let n = crate::adjust::LUT_SIZE;
    let mut ramp = Buffer { rect: Rect::new(0, 0, n as i32, 1), px: (0..n).map(|k| [k as f32 / (n - 1) as f32; 4]).collect() };
    ramp.px.iter_mut().for_each(|p| p[3] = 1.0);
    crate::adjust::apply_depth(a, &mut ramp, cx.transfer, Some(cx.depth));
    let row: Vec<f32> = ramp.px.iter().map(|p| p[0]).collect();
    [row.clone(), row.clone(), row.clone(), row]
}

/// Applies a [`native_adjustment`] to every pixel's inks.
fn adjust(a: &Adjustment, buf: &mut CBuf, cx: &Ctx) {
    // Threshold: paper white or pure K black (Photoshop corpus `cmyk8/threshold-128.psd` has
    // only those two), split on the inks' own lightness, no profile involved: the complements of
    // C, M, Y weighted 0.30 / 0.59 / 0.11, times 1 − K, compared on the 8-bit grid. That splits
    // the file's 9216 pixels exactly at its level (`cmyk_probe` THRESH=1); screen luminance
    // misclassifies 100 of them.
    if let Adjustment::Threshold { level } = a {
        let t = (level * 255.0).round();
        for p in &mut buf.px {
            let white = (ink_lightness(p) * 255.0).round() >= t;
            let k = if white { 1.0 } else { 0.0 };
            p[..4].copy_from_slice(&[1.0, 1.0, 1.0, k]);
        }
        return;
    }
    // Colour adjustments: C, M, Y's complements through the RGB adjustment as an RGB colour, K
    // untouched (Photoshop corpus cmyk8: K exact in every one; Selective Color within 0.006, the
    // others carry the residual their RGB versions have; `cmyk_probe` ADJ=1).
    if matches!(a, Adjustment::HueSaturation { .. } | Adjustment::ColorBalance { .. } | Adjustment::SelectiveColor { .. } | Adjustment::PhotoFilter { .. }) {
        let mut rgb = Buffer { rect: buf.rect, px: buf.px.iter().map(|p| [p[0], p[1], p[2], 1.0]).collect() };
        crate::adjust::apply_depth(a, &mut rgb, cx.transfer, Some(cx.depth));
        for (p, o) in buf.px.iter_mut().zip(&rgb.px) {
            p[..3].copy_from_slice(&o[..3]);
        }
        return;
    }
    if let Adjustment::GradientMap { stops, reverse, .. } = a {
        for p in &mut buf.px {
            let mut t = ink_lightness(p);
            if *reverse {
                t = 1.0 - t;
            }
            let o = crate::adjust::gradient(stops, t);
            let inks = rgb_to_cmyk(o);
            p[..4].copy_from_slice(&[1.0 - inks[0], 1.0 - inks[1], 1.0 - inks[2], 1.0 - inks[3]]);
        }
        return;
    }
    let luts = ink_luts(a, cx);
    for p in &mut buf.px {
        for (v, t) in p.iter_mut().zip(&luts) {
            *v = crate::adjust::lut(t, *v);
        }
    }
}

fn layer(l: &Layer, clipped: &[Layer], backdrop: &mut CBuf, cx: &Ctx) {
    if !native(l, clipped, cx) {
        island(backdrop, |rgb| crate::composite_layer(l, clipped, rgb, cx));
        return;
    }
    // Channel restrictions (Blending Options › Channels: C, M, Y, K).
    let excluded = l.excluded_channels & 0xf;
    let before = (excluded != 0).then(|| backdrop.px.clone());
    match &l.content {
        LayerContent::Group(g) if g.artboard.is_some() => {
            if let Some(ab) = &g.artboard {
                artboard(l, ab, clipped, backdrop, cx);
            }
        }
        _ => plain(l, clipped, backdrop, cx),
    }
    if let Some(before) = before {
        for (p, b) in backdrop.px.iter_mut().zip(&before) {
            for c in (0..4).filter(|c| excluded & (1 << c) != 0) {
                p[c] = b[c];
            }
        }
    }
}

/// An artboard: its background and the group, composited only inside the board (as
/// [`crate::composite_artboard`]).
fn artboard(l: &Layer, ab: &photosuite_doc::Artboard, clipped: &[Layer], backdrop: &mut CBuf, cx: &Ctx) {
    let board = ab.rect.intersect(&backdrop.rect);
    if board.is_empty() {
        return;
    }
    let (bw, w) = (board.width() as usize, backdrop.rect.width() as usize);
    let row0 = |y: i32| (y - backdrop.rect.y0) as usize * w + (board.x0 - backdrop.rect.x0) as usize;
    let mut sub = CBuf::transparent(board);
    for y in board.y0..board.y1 {
        let (o, so) = (row0(y), (y - board.y0) as usize * bw);
        sub.px[so..so + bw].copy_from_slice(&backdrop.px[o..o + bw]);
    }
    if let Some(bg) = ab.background.rgba() {
        let bg = from_rgba(bg);
        for p in &mut sub.px {
            *p = composite_px(BlendMode::Normal, *p, bg, 1.0);
        }
    }
    plain(l, clipped, &mut sub, cx);
    for y in board.y0..board.y1 {
        let (o, so) = (row0(y), (y - board.y0) as usize * bw);
        backdrop.px[o..o + bw].copy_from_slice(&sub.px[so..so + bw]);
    }
}

fn plain(l: &Layer, clipped: &[Layer], backdrop: &mut CBuf, cx: &Ctx) {
    let rect = backdrop.rect;
    if crate::empty_in(l, rect) {
        return;
    }
    let opacity = l.opacity * l.fill_opacity;
    match &l.content {
        // Pass-through groups composite their children straight into the backdrop; below 100 %
        // fill they render isolated, like Normal (the arm below).
        LayerContent::Group(g) if l.blend == BlendMode::PassThrough && l.fill_opacity >= 1.0 => {
            let needs_mix = opacity < 1.0 || l.mask.is_some() || l.vector_mask.is_some();
            let before = needs_mix.then(|| backdrop.px.clone());
            stack(&g.children, backdrop, cx);
            if let Some(before) = before {
                let mv = crate::mask_vals(l, rect, cx);
                for (i, (p, a)) in backdrop.px.iter_mut().zip(&before).enumerate() {
                    let k = opacity * crate::mask_k(&mv, i);
                    // Coverage mixes premultiplied colour; the buffer stays straight alpha.
                    let (wa, wb) = (a[4] * (1.0 - k), p[4] * k);
                    let alpha = wa + wb;
                    *p = if alpha > 0.0 {
                        let mut o = [0.0; 5];
                        for c in 0..4 {
                            o[c] = (a[c] * wa + p[c] * wb) / alpha;
                        }
                        o[4] = alpha;
                        o
                    } else {
                        [0.0; 5]
                    };
                }
            }
        }
        LayerContent::Adjustment(a) => {
            let mut adjusted = CBuf { rect, px: backdrop.px.clone() };
            adjust(a, &mut adjusted, cx);
            for c in clipped.iter().filter(|c| c.visible) {
                atop(c, &mut adjusted, cx);
            }
            let mv = crate::mask_vals(l, rect, cx);
            for (i, (b, a)) in backdrop.px.iter_mut().zip(&adjusted.px).enumerate() {
                let k = opacity * crate::mask_k(&mv, i);
                if k <= 0.0 {
                    continue;
                }
                let bl = blend(l.blend, b, a);
                for c in 0..4 {
                    b[c] += (bl[c] - b[c]) * k;
                }
            }
        }
        _ => {
            let Some(mut src) = content(l, rect, cx) else { return };
            for c in clipped.iter().filter(|c| c.visible) {
                atop(c, &mut src, cx);
            }
            blend_into(backdrop, &src, l.blend, opacity);
        }
    }
}

/// A layer's own content (masks applied, not yet blended) in CMYK; `None` for adjustments.
fn content(l: &Layer, rect: Rect, cx: &Ctx) -> Option<CBuf> {
    let mut buf = match &l.content {
        LayerContent::Group(g) => composite(&g.children, rect, cx),
        LayerContent::Fill(f) => match &l.fill_cache {
            // Photoshop's own rendering, valid while the fill is unchanged.
            Some(c) if c.fill == *f => from_surface(&c.surface, rect),
            _ => fill(l, f, rect, cx),
        },
        LayerContent::Adjustment(_) => return None,
        _ => match l.surface() {
            Some(s) => from_surface(s, rect),
            None => CBuf::transparent(rect),
        },
    };
    if let Some(m) = crate::mask_vals(l, rect, cx) {
        for (p, k) in buf.px.iter_mut().zip(&m) {
            p[4] *= k;
        }
    }
    Some(buf)
}

/// A surface's pixels over `rect`: CMYK ones as they are, others separated through the profile.
fn from_surface(s: &Surface, rect: Rect) -> CBuf {
    let fmt = s.format();
    if fmt.mode != ColorMode::Cmyk {
        let rgb = crate::surface_to_buffer(s, rect);
        return CBuf { rect, px: rgb.px.into_iter().map(from_rgba).collect() };
    }
    let mut vals = Vec::new();
    s.read_region_into(rect, &mut vals);
    let n = s.channels().max(1);
    let px = vals
        .chunks_exact(n)
        .map(|v| {
            let ink = |i: usize| v.get(i).copied().unwrap_or(0.0);
            let a = if fmt.alpha { v.get(4).copied().unwrap_or(1.0) } else { 1.0 };
            from_inks([ink(0), ink(1), ink(2), ink(3)], a)
        })
        .collect();
    CBuf { rect, px }
}

/// An edited fill layer: a solid colour in its own inks; gradients and patterns rendered in RGB
/// and separated (Photoshop interpolates CMYK gradients in CMYK: not yet).
fn fill(l: &Layer, f: &Fill, rect: Rect, cx: &Ctx) -> CBuf {
    if let Fill::Solid(c) = f {
        let p = from_inks(color_inks(c), c.alpha);
        return CBuf { rect, px: vec![p; rect.width() as usize * rect.height() as usize] };
    }
    let rgb = crate::render_fill(f, rect, crate::fill_frame(l, cx.canvas), cx.patterns);
    CBuf { rect, px: rgb.px.into_iter().map(from_rgba).collect() }
}

/// Composites a clipped layer onto its base `base` (as if opaque, keeping the base's alpha).
fn atop(l: &Layer, base: &mut CBuf, cx: &Ctx) {
    let rect = base.rect;
    if !native(l, &[], cx) {
        island(base, |rgb| crate::composite_atop(l, rgb, cx));
        return;
    }
    let opacity = l.opacity * l.fill_opacity;
    if let LayerContent::Adjustment(a) = &l.content {
        let mut adjusted = CBuf { rect, px: base.px.clone() };
        adjust(a, &mut adjusted, cx);
        let mv = crate::mask_vals(l, rect, cx);
        for (i, (p, a)) in base.px.iter_mut().zip(&adjusted.px).enumerate() {
            let k = opacity * crate::mask_k(&mv, i);
            let bl = blend(l.blend, p, a);
            for c in 0..4 {
                p[c] += (bl[c] - p[c]) * k;
            }
        }
        return;
    }
    let Some(src) = content(l, rect, cx) else { return };
    for (p, s) in base.px.iter_mut().zip(&src.px) {
        let alpha = p[4];
        if alpha <= 0.0 {
            continue;
        }
        let mut opaque = *p;
        opaque[4] = 1.0;
        let mut r = composite_px(l.blend, opaque, *s, opacity);
        r[4] = alpha;
        *p = r;
    }
}

/// Blends an isolated layer buffer into the backdrop.
fn blend_into(backdrop: &mut CBuf, src: &CBuf, mode: BlendMode, opacity: f32) {
    let rect = backdrop.rect;
    let w = rect.width().max(1) as usize;
    for (i, (b, s)) in backdrop.px.iter_mut().zip(&src.px).enumerate() {
        if s[4] <= 0.0 {
            continue;
        }
        if mode == BlendMode::Dissolve {
            let (x, y) = (rect.x0 + (i % w) as i32, rect.y0 + (i / w) as i32);
            let mut s = *s;
            s[4] = if crate::dissolve_noise(x, y) < s[4] * opacity { 1.0 } else { 0.0 };
            *b = composite_px(BlendMode::Normal, *b, s, 1.0);
            continue;
        }
        *b = composite_px(mode, *b, *s, opacity);
    }
}

/// `B(Cb, Cs)` on the four inks' complements. Separable modes blend each on its own. The
/// non-separable ones (Hue, Saturation, Color, Luminosity, Darker / Lighter Color) take C, M, Y
/// as an RGB colour and K as a gray of its own: Photoshop's merged CMYK within half a level
/// (`cmyk_probe`; K exact).
pub(crate) fn blend(mode: BlendMode, b: &Px, s: &Px) -> [f32; 4] {
    if mode.is_separable() {
        return std::array::from_fn(|c| crate::psblend::blend_channel(mode, b[c], s[c]));
    }
    let cmy = crate::psblend::blend_rgb(mode, [b[0], b[1], b[2]], [s[0], s[1], s[2]]);
    let k = crate::psblend::blend_rgb(mode, [b[3]; 3], [s[3]; 3])[0];
    [cmy[0], cmy[1], cmy[2], k]
}

/// Straight-alpha source over straight-alpha backdrop (W3C/PDF general formula with
/// [`blend`]).
pub(crate) fn composite_px(mode: BlendMode, b: Px, s: Px, opacity: f32) -> Px {
    let (ab, as_) = (b[4], s[4] * opacity);
    if as_ <= 0.0 {
        return b;
    }
    let ao = as_ + ab * (1.0 - as_);
    if ao <= 0.0 {
        return [0.0; 5];
    }
    let bl = blend(mode, &b, &s);
    let mut out = [0.0; 5];
    for c in 0..4 {
        out[c] = ((1.0 - as_) * ab * b[c] + (1.0 - ab) * as_ * s[c] + as_ * ab * bl[c]) / ao;
    }
    out[4] = ao;
    out
}

/// Runs the RGB compositor `draw` over the backdrop's screen conversion; the pixels it changed
/// take its result back, separated through the profile.
fn island(backdrop: &mut CBuf, draw: impl FnOnce(&mut Buffer)) {
    let before: Vec<[f32; 4]> = backdrop.px.iter().map(to_rgba).collect();
    let mut rgb = Buffer { rect: backdrop.rect, px: before.clone() };
    draw(&mut rgb);
    for ((p, was), now) in backdrop.px.iter_mut().zip(&before).zip(&rgb.px) {
        if now != was {
            *p = from_rgba(*now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photosuite_color::SampleType;
    use photosuite_doc::{Document, Effect, Size};

    /// A 4 × 1 CMYK document with a bottom layer of `base` inks over all of it.
    fn doc(base: [f32; 4]) -> Document {
        let mut d = Document::new("c", Size::new(4, 1), ColorMode::Cmyk, SampleType::F32);
        let mut l = Layer::raster("base", d.pixel_format());
        if let Some(s) = l.surface_mut() {
            s.fill_rect(Rect::new(0, 0, 4, 1), &[base[0], base[1], base[2], base[3], 1.0]);
        }
        d.layers.push(l);
        d
    }

    /// A layer of `inks` over pixels 0..2, blended with `mode`.
    fn top(d: &Document, inks: [f32; 4], mode: BlendMode) -> Layer {
        let mut l = Layer::raster("top", d.pixel_format());
        if let Some(s) = l.surface_mut() {
            s.fill_rect(Rect::new(0, 0, 2, 1), &[inks[0], inks[1], inks[2], inks[3], 1.0]);
        }
        l.blend = mode;
        l
    }

    fn inks_at(d: &Document, x: usize) -> [f32; 4] {
        let patterns = crate::pattern::PreparedPatterns::new(&d.patterns, crate::pattern::PREPARED_PATTERN_BYTES);
        let cx = Ctx::for_doc(d, &patterns);
        inks(&composite(&d.layers, d.bounds(), &cx).px[x])
    }

    fn close(a: [f32; 4], b: [f32; 4]) -> bool {
        a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-5)
    }

    /// Multiply works on each ink's complement (Photoshop's merged CMYK, `cmyk_probe`): inks
    /// combine as 1 − (1 − a)(1 − b), K included.
    #[test]
    fn separable_modes_blend_each_ink() {
        let mut d = doc([0.2, 0.4, 0.0, 0.5]);
        let t = top(&d, [0.5, 0.5, 0.3, 0.2], BlendMode::Multiply);
        d.layers.push(t);
        let want = [1.0 - 0.8 * 0.5, 1.0 - 0.6 * 0.5, 1.0 - 0.7, 1.0 - 0.5 * 0.8];
        assert!(close(inks_at(&d, 0), want), "{:?}", inks_at(&d, 0));
        // Outside the top layer the base's inks are untouched.
        assert!(close(inks_at(&d, 3), [0.2, 0.4, 0.0, 0.5]));
    }

    /// Non-separable modes take C, M, Y as an RGB colour and K as a gray of its own: Hue keeps
    /// the backdrop's K, Luminosity takes the source's.
    #[test]
    fn non_separable_modes_treat_k_as_its_own_gray() {
        for (mode, k) in [(BlendMode::Hue, 0.6), (BlendMode::Luminosity, 0.1)] {
            let mut d = doc([0.1, 0.5, 0.9, 0.6]);
            let t = top(&d, [0.7, 0.2, 0.4, 0.1], mode);
            d.layers.push(t);
            let got = inks_at(&d, 0);
            let cmy = crate::psblend::blend_rgb(mode, [0.9, 0.5, 0.1], [0.3, 0.8, 0.6]);
            assert!(close(got, [1.0 - cmy[0], 1.0 - cmy[1], 1.0 - cmy[2], k]), "{mode:?}: {got:?}");
        }
    }

    /// Invert inverts each ink (Photoshop corpus `cmyk8/invert.psd`: exact).
    #[test]
    fn invert_inverts_each_ink() {
        let mut d = doc([0.2, 0.4, 0.0, 0.5]);
        d.layers.push(Layer::new("inv", LayerContent::Adjustment(Adjustment::Invert)));
        assert!(close(inks_at(&d, 2), [0.8, 0.6, 1.0, 0.5]));
    }

    /// Threshold gives paper white or pure K black, split on the inks' own lightness (Photoshop
    /// corpus cmyk8/threshold-128.psd: exact): 60 % cyan alone is light, 60 % K alone dark.
    #[test]
    fn threshold_is_white_or_k_black() {
        for (inks, black) in [([0.6, 0.0, 0.0, 0.0], false), ([0.0, 0.0, 0.0, 0.6], true), ([0.8, 0.8, 0.0, 0.0], true)] {
            let mut d = doc(inks);
            d.layers.push(Layer::new("t", LayerContent::Adjustment(Adjustment::Threshold { level: 128.0 / 255.0 })));
            let want = if black { [0.0, 0.0, 0.0, 1.0] } else { [0.0; 4] };
            assert!(close(inks_at(&d, 0), want), "{inks:?}: {:?}", inks_at(&d, 0));
        }
    }

    /// Posterize steps each ink on its own (Photoshop corpus cmyk8/posterize-4.psd: exact).
    #[test]
    fn posterize_steps_each_ink() {
        let mut d = doc([0.1, 0.4, 0.62, 0.9]);
        d.layers.push(Layer::new("p", LayerContent::Adjustment(Adjustment::Posterize { levels: 2 })));
        assert!(close(inks_at(&d, 0), [0.0, 0.0, 1.0, 1.0]), "{:?}", inks_at(&d, 0));
    }

    /// Colour adjustments change C, M, Y and leave K exactly as it was (Photoshop corpus cmyk8
    /// hue-saturation / color-balance / selective-color / photo-filter: K exact in all four).
    #[test]
    fn colour_adjustments_keep_k() {
        let mut d = doc([0.1, 0.6, 0.3, 0.45]);
        d.layers.push(Layer::new("h", LayerContent::Adjustment(Adjustment::HueSaturation { hue: 60.0, saturation: 20.0, lightness: 0.0, colorize: false, ranges: photosuite_doc::adjust::HueRange::defaults() })));
        let got = inks_at(&d, 0);
        assert!((got[3] - 0.45).abs() < 1e-6, "{got:?}");
        assert!((got[0] - 0.1).abs() > 0.05 || (got[1] - 0.6).abs() > 0.05, "hue moved the colour: {got:?}");
    }

    /// Blending Options › Channels: a layer that leaves K out keeps the backdrop's K.
    #[test]
    fn excluded_channels_keep_the_backdrop_ink() {
        let mut d = doc([0.2, 0.4, 0.0, 0.5]);
        let mut t = top(&d, [0.9, 0.9, 0.9, 0.1], BlendMode::Normal);
        t.excluded_channels = 1 << 3;
        d.layers.push(t);
        assert!(close(inks_at(&d, 0), [0.9, 0.9, 0.9, 0.5]));
    }

    /// A layer drawn as an RGB island (here one with a layer effect) changes only the pixels it
    /// covers; the others keep their exact inks (pure K stays pure K, not a rich-black mix).
    #[test]
    fn rgb_islands_leave_other_pixels_exact() {
        let mut d = doc([0.0, 0.0, 0.0, 0.75]);
        let mut t = top(&d, [0.0, 1.0, 1.0, 0.0], BlendMode::Normal);
        t.effects.items = vec![Effect::default_drop_shadow()];
        t.effects.items.iter_mut().for_each(|e| {
            if let Effect::DropShadow(s) = e {
                (s.distance, s.size) = (0.0, 0.0);
            }
        });
        d.layers.push(t);
        assert!(close(inks_at(&d, 3), [0.0, 0.0, 0.0, 0.75]));
        let over = inks_at(&d, 0);
        assert!(over[1] > 0.5 && over[2] > 0.5, "{over:?}");
    }
}
