//! Blending Options › Advanced Blending › Knockout.
//!
//! A knockout layer's shape replaces the backdrop with what it knocks out to, then its content
//! lands there at its fill opacity. Fitted on psd-tools `transparency/knockout-*.psd`:
//! - Deep with a Background: the Background shows through (deep-normal: blue at 50 % fill over
//!   white, the red layer between gone); without one, transparency (deep-nobg);
//! - inside an isolated (non pass-through) group both Shallow and Deep stop at the group's
//!   start, which is transparent (shallow-nested, deep-nested: the green layer in the group is
//!   gone, the red beneath the group stays);
//! - Shallow reaches the start of its own group; Deep goes on out through pass-through groups.
//!
//! Compositing keeps a stack of "knockout frames" (one per group entered: its starting backdrop
//! and whether it is isolated), on top of the document's own (the Background alone, or
//! transparency). It lives in a thread local: a tile is composited on one thread, top to bottom.
//! Documents without a knockout layer push nothing.

use std::cell::RefCell;

use photosuite_doc::{Document, Knockout, Layer, LayerContent};
use photosuite_geom::Rect;

use crate::{Buffer, Ctx};

/// Frames deeper than this are not pushed (knockouts below fall back to the deepest frame).
const MAX_FRAMES: usize = 64;

thread_local! {
    static FRAMES: RefCell<Vec<(Buffer, bool)>> = const { RefCell::new(Vec::new()) };
}

/// Whether any layer in `layers` (groups included) knocks out.
pub(crate) fn any(layers: &[Layer], depth: u32) -> bool {
    depth < 64
        && layers.iter().any(|l| {
            l.knockout != Knockout::None || matches!(&l.content, LayerContent::Group(g) if any(&g.children, depth + 1))
        })
}

/// The Background layer, Photoshop's: the bottom layer, a locked plain pixel layer.
pub(crate) fn background(layers: &[Layer]) -> Option<&Layer> {
    let l = layers.first()?;
    (matches!(l.content, LayerContent::Raster(_)) && l.locks.transparency && l.locks.position && l.visible).then_some(l)
}

/// Starts a document's composite of `rect`: its frame is the Background alone (Deep knocks
/// out to it), or transparency. `false` (nothing pushed) when no layer knocks out.
pub(crate) fn enter_document(doc: &Document, rect: Rect, cx: &Ctx) -> bool {
    if !any(&doc.layers, 0) {
        return false;
    }
    let mut base = Buffer::transparent(rect);
    if let Some(bg) = background(&doc.layers) {
        crate::composite_stack(std::slice::from_ref(bg), &mut base, cx);
    }
    FRAMES.with(|f| f.borrow_mut().push((base, true)));
    true
}

/// Enters a group whose children composite onto `start` (`isolated`: a group of its own rather
/// than pass-through). `false` (nothing pushed) outside a knockout document.
pub(crate) fn enter(start: &Buffer, isolated: bool) -> bool {
    FRAMES.with(|f| {
        let mut f = f.borrow_mut();
        if f.is_empty() || f.len() >= MAX_FRAMES {
            return false;
        }
        f.push((start.clone(), isolated));
        true
    })
}

/// Leaves the innermost frame.
pub(crate) fn leave() {
    FRAMES.with(|f| {
        f.borrow_mut().pop();
    });
}

/// What a `kind` knockout reveals over `rect`: Shallow, the innermost frame's start; Deep, the
/// innermost isolated one's (the document's at the bottom). `None` outside a knockout document.
pub(crate) fn backdrop(kind: Knockout, rect: Rect) -> Option<Buffer> {
    FRAMES.with(|f| {
        let f = f.borrow();
        let frame = match kind {
            Knockout::None => return None,
            Knockout::Shallow => f.last(),
            Knockout::Deep => f.iter().rev().find(|(_, isolated)| *isolated),
        }?;
        Some(crop(&frame.0, rect))
    })
}

/// `b` over `rect` (transparent where it doesn't reach).
fn crop(b: &Buffer, rect: Rect) -> Buffer {
    if b.rect == rect {
        return b.clone();
    }
    let mut out = Buffer::transparent(rect);
    let both = b.rect.intersect(&rect);
    let (bw, ow) = (b.rect.width() as usize, rect.width() as usize);
    for y in both.y0..both.y1 {
        for x in both.x0..both.x1 {
            let (i, o) = ((y - b.rect.y0) as usize * bw + (x - b.rect.x0) as usize, (y - rect.y0) as usize * ow + (x - rect.x0) as usize);
            if let (Some(src), Some(dst)) = (b.px.get(i), out.px.get_mut(o)) {
                *dst = *src;
            }
        }
    }
    out
}

/// `a` replaced by `b` over coverage `k` (premultiplied mix; the buffers stay straight alpha).
pub(crate) fn mix(a: [f32; 4], b: [f32; 4], k: f32) -> [f32; 4] {
    let k = k.clamp(0.0, 1.0);
    let (wa, wb) = (a[3] * (1.0 - k), b[3] * k);
    let alpha = wa + wb;
    if alpha <= 0.0 {
        return [0.0; 4];
    }
    let c = |i: usize| (a[i] * wa + b[i] * wb) / alpha;
    [c(0), c(1), c(2), alpha]
}

#[cfg(test)]
mod tests {
    use photosuite_color::{BlendMode, ColorMode, SampleType};
    use photosuite_doc::{Document, Knockout, Layer, Size};
    use photosuite_geom::Rect;

    fn solid(name: &str, d: &Document, rgba: [f32; 4]) -> Layer {
        let mut l = Layer::raster(name, d.pixel_format());
        if let Some(s) = l.surface_mut() {
            s.fill_rect(Rect::new(0, 0, 2, 2), &rgba);
        }
        l
    }

    /// psd-tools knockout-deep-normal / -nobg / -deep-nested, in miniature: white Background,
    /// red layer, a 50 %-fill group (knockout `kind`) with blue in it.
    fn doc(kind: Knockout, background: bool, isolated_outer: bool) -> [f32; 4] {
        let mut d = Document::new("k", Size::new(2, 2), ColorMode::Rgb, SampleType::F32);
        let mut bg = solid("Background", &d, [1.0; 4]);
        (bg.locks.transparency, bg.locks.position) = (background, background);
        let red = solid("red", &d, [1.0, 0.0, 0.0, 1.0]);
        let mut ko = Layer::group("ko", vec![solid("blue", &d, [0.0, 0.0, 1.0, 1.0])]);
        (ko.knockout, ko.fill_opacity, ko.blend) = (kind, 0.5, BlendMode::Normal);
        if isolated_outer {
            let mut outer = Layer::group("outer", vec![red, ko]);
            outer.blend = BlendMode::Normal;
            d.layers = vec![bg, outer];
        } else {
            d.layers = vec![bg, red, ko];
        }
        crate::flatten(&d).px[0]
    }

    fn close(a: [f32; 4], b: [f32; 4]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-3)
    }

    #[test]
    fn knockout_reveals_what_photoshop_shows() {
        // None: blue at 50 % over red.
        assert!(close(doc(Knockout::None, true, false), [0.5, 0.0, 0.5, 1.0]));
        // Deep: the red is knocked out to the white Background.
        assert!(close(doc(Knockout::Deep, true, false), [0.5, 0.5, 1.0, 1.0]), "{:?}", doc(Knockout::Deep, true, false));
        // Without a Background: to transparency.
        assert!(close(doc(Knockout::Deep, false, false), [0.0, 0.0, 1.0, 0.5]), "{:?}", doc(Knockout::Deep, false, false));
        // Inside an isolated group, Deep stops at the group's (transparent) start: the red,
        // inside the group, goes; the white beneath it stays.
        assert!(close(doc(Knockout::Deep, true, true), [0.5, 0.5, 1.0, 1.0]), "{:?}", doc(Knockout::Deep, true, true));
        assert!(close(doc(Knockout::Shallow, true, true), [0.5, 0.5, 1.0, 1.0]));
    }
}

#[cfg(test)]
mod clip_tests {
    use photosuite_color::{BlendMode, ColorMode, SampleType};
    use photosuite_doc::{Color, Document, Effect, FxCommon, Layer, Size};
    use photosuite_geom::Rect;

    fn solid(name: &str, d: &Document, rgba: [f32; 4], w: i32) -> Layer {
        let mut l = Layer::raster(name, d.pixel_format());
        if let Some(s) = l.surface_mut() {
            s.fill_rect(Rect::new(0, 0, w, 1), &rgba);
        }
        l
    }

    fn close(a: [f32; 4], b: [f32; 4]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-3)
    }

    /// White backdrop; a red base over pixel 0 at `fill`; a blue layer clipped to it, Multiply.
    fn clipped(fill: f32, blend_clipped: bool) -> Document {
        let mut d = Document::new("c", Size::new(2, 1), ColorMode::Rgb, SampleType::F32);
        let bg = solid("bg", &d, [1.0, 1.0, 1.0, 1.0], 2);
        let mut base = solid("base", &d, [1.0, 0.0, 0.0, 1.0], 1);
        (base.fill_opacity, base.blend_clipped) = (fill, blend_clipped);
        let mut top = solid("top", &d, [0.0, 0.0, 1.0, 1.0], 2);
        (top.clipped, top.blend) = (true, BlendMode::Multiply);
        d.layers = vec![bg, base, top];
        d
    }

    /// Clipped layers take the base's shape, not its fill: over a 0 %-fill base the clipped
    /// layer still shows, in its own mode over what the group holds (nothing: blue).
    #[test]
    fn clipped_layers_ignore_the_base_fill() {
        let px = crate::flatten(&clipped(0.0, true)).px;
        assert!(close(px[0], [0.0, 0.0, 1.0, 1.0]), "{:?}", px[0]);
        // Outside the base's shape: untouched.
        assert!(close(px[1], [1.0; 4]), "{:?}", px[1]);
        // Full fill is unchanged: blue multiplied onto red.
        assert!(close(crate::flatten(&clipped(1.0, true)).px[0], [0.0, 0.0, 0.0, 1.0]));
    }

    /// Blend Clipped Layers as Group off: the clipped layer blends on its own with what is
    /// beneath the base too (Multiply over white at 0 % fill: blue).
    #[test]
    fn unblended_clipped_layers_reach_the_backdrop() {
        let px = crate::flatten(&clipped(0.0, false)).px;
        assert!(close(px[0], [0.0, 0.0, 1.0, 1.0]), "{:?}", px[0]);
        // At 50 % fill, the base (red over white at 50 %) is multiplied by blue.
        let px = crate::flatten(&clipped(0.5, false)).px;
        assert!(close(px[0], [0.0, 0.0, 0.5, 1.0]), "{:?}", px[0]);
    }

    /// Blend Interior Effects as Group: a colour overlay fades with the layer's fill opacity.
    #[test]
    fn interior_effects_fade_with_fill_when_grouped() {
        let mut d = Document::new("i", Size::new(2, 1), ColorMode::Rgb, SampleType::F32);
        let bg = solid("bg", &d, [1.0, 1.0, 1.0, 1.0], 2);
        let mut l = solid("l", &d, [1.0, 0.0, 0.0, 1.0], 1);
        l.fill_opacity = 0.25;
        l.effects.items = vec![Effect::ColorOverlay { common: FxCommon::new(BlendMode::Normal, 1.0), color: Color::BLACK }];
        d.layers = vec![bg, l];
        // Default: the overlay covers fully.
        assert!(close(crate::flatten(&d).px[0], [0.0, 0.0, 0.0, 1.0]), "{:?}", crate::flatten(&d).px[0]);
        d.layers[1].blend_interior = true;
        assert!(close(crate::flatten(&d).px[0], [0.75, 0.75, 0.75, 1.0]), "{:?}", crate::flatten(&d).px[0]);
    }
}
