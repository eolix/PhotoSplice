//! Icon rendering. SVGs are embedded (see `icon_data.rs`), recoloured to white, rasterized by the
//! egui_extras SVG loader at the exact on-screen pixel size (crisp at any DPI), and tinted per use.
//!
//! (History: the first shell used Unicode glyphs; half of them rendered as tofu boxes because the
//! bundled fonts lacked them. Vector icons fix that for good.)

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use egui::{Color32, Rect, Response, Sense, Vec2};

use crate::icon_data::ICONS;
use crate::state::Tool;
use crate::theme::Tokens;

fn white_icons() -> &'static HashMap<&'static str, Arc<[u8]>> {
    static MAP: OnceLock<HashMap<&'static str, Arc<[u8]>>> = OnceLock::new();
    MAP.get_or_init(|| ICONS.iter().map(|(name, bytes)| (*name, Arc::from(whiten(&String::from_utf8_lossy(bytes)).into_bytes().into_boxed_slice()))).collect())
}

/// Force an icon to draw white, whatever it was authored as, so it can be tinted per use.
///
/// Three styles ship here and each needs different handling; resvg resolves none of them
/// against a surrounding context, so the colour has to end up in the file:
///
/// * **Lucide** — `stroke="currentColor"`, which means nothing to a standalone renderer.
/// * **Tabler** (`assets/ico/`, 24 px) — an explicit `stroke="#000"` on the root.
/// * **Traced glyphs** (1000–1200 px) — either a `fill: #000000` rule in an embedded `<style>`
///   referenced by `class=`, or *no paint at all*, which SVG defines as black. That last one is
///   why some rendered as solid dark blocks: there was no colour in the file to replace.
///
/// The source files are left as their authors wrote them (they are MIT, and unmodified copies
/// keep the licence simple); all of this happens on the way into the texture.
fn whiten(svg: &str) -> String {
    let mut s = svg.to_string();
    // Resolve `.cls { fill: …; stroke: … }` rules into the same presentation attributes, then
    // drop the stylesheet, so nothing depends on the renderer's CSS support. Every declaration
    // is carried over as written (`fill: none` stays none: a 24 × 24 background path drawn
    // filled would cover the icon); the colours are whitened below like any other.
    if let (Some(a), Some(b)) = (s.find("<style>"), s.find("</style>")) {
        let sheet = s[a + 7..b].to_string();
        for rule in sheet.split('}') {
            let Some((sel, body)) = rule.split_once('{') else { continue };
            let Some(class) = sel.trim().strip_prefix('.') else { continue };
            let attrs: Vec<String> = body
                .split(';')
                .filter_map(|d| d.split_once(':'))
                .map(|(k, v)| (k.trim(), v.trim()))
                .filter(|(k, v)| !k.is_empty() && !v.is_empty() && k.bytes().all(|c| c.is_ascii_lowercase() || c == b'-'))
                .map(|(k, v)| format!("{k}=\"{}\"", v.replace('"', "")))
                .collect();
            if !attrs.is_empty() {
                s = s.replace(&format!("class=\"{class}\""), &attrs.join(" "));
            }
        }
        if let (Some(a), Some(b)) = (s.find("<style>"), s.find("</style>")) {
            s.replace_range(a..b + 8, "");
        }
    }
    s = s
        .replace("currentColor", "#ffffff")
        .replace("#000000", "#ffffff")
        .replace("#000\"", "#ffffff\"")
        .replace("#000;", "#ffffff;")
        .replace("#000 ", "#ffffff ")
        .replace(":black", ":#ffffff")
        .replace("=\"black\"", "=\"#ffffff\"")
        .replace("stroke-width=\"2\"", "stroke-width=\"1.75\"");
    // A file that paints nothing explicitly inherits SVG's default black fill. Give the root a
    // white fill so those shapes show; anything setting its own paint still wins. Find the
    // `<svg>` tag rather than the first '>', which may end an XML declaration.
    if let Some(open) = s.find("<svg")
        && let Some(end) = s[open..].find('>').map(|i| open + i)
        && !s[open..end].contains("fill=")
    {
        s.insert_str(end, " fill=\"#ffffff\"");
    }
    s
}

pub fn exists(name: &str) -> bool {
    white_icons().contains_key(name)
}

/// An egui image for an icon, tinted.
pub fn image(name: &str, size: f32, tint: Color32) -> egui::Image<'static> {
    let bytes = white_icons().get(name).or_else(|| white_icons().get("square")).cloned().unwrap_or_default();
    egui::Image::from_bytes(format!("bytes://icons/{name}.svg"), egui::load::Bytes::Shared(bytes)).fit_to_exact_size(Vec2::splat(size)).tint(tint)
}

/// Paint an icon centred in `rect`.
pub fn paint(ui: &egui::Ui, rect: Rect, name: &str, size: f32, tint: Color32) {
    let r = Rect::from_center_size(rect.center(), Vec2::splat(size));
    image(name, size, tint).paint_at(ui, r);
}

/// An icon as the pointer, above every window: `name` with its hotspot `hot` (a fraction of the
/// icon box) on `p`, white with a dark outline so it reads on any image. The caller hides the OS
/// cursor (`CursorIcon::None`).
pub fn cursor(ctx: &egui::Context, name: &str, p: egui::Pos2, hot: Vec2, size: f32) {
    let rect = Rect::from_min_size(p - hot * size, Vec2::splat(size));
    let area = egui::Area::new(egui::Id::new("pc-icon-cursor")).order(egui::Order::Tooltip).fixed_pos(rect.min).constrain(false).interactable(false);
    area.show(ctx, |ui| {
        let outline = image(name, size, Color32::from_black_alpha(200));
        for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0), (-1.0, -1.0), (1.0, 1.0), (-1.0, 1.0), (1.0, -1.0)] {
            outline.paint_at(ui, rect.translate(egui::vec2(dx, dy)));
        }
        image(name, size, Color32::WHITE).paint_at(ui, rect);
    });
}

pub fn tool_icon(t: Tool) -> &'static str {
    match t {
        Tool::Move => "tools-move",
        Tool::RectMarquee => "tools-rselect",
        Tool::EllipseMarquee => "tools-eselect",
        Tool::Lasso => "tools-lasso",
        Tool::PolygonLasso => "tools-plasso",
        Tool::MagneticLasso => "tools-mlasso",
        Tool::MagicWand => "tools-mwand",
        Tool::QuickSelection => "tools-qselect",
        Tool::ObjectSelection => "tools-oselect",
        Tool::Crop => "tools-rcrop",
        Tool::Slice => "tools-slice",
        Tool::SliceSelect => "tools-sselect",
        Tool::Eyedropper => "tools-eyedropper",
        Tool::Ruler => "tools-ruler",
        Tool::SpotHealing => "tools-shbrush",
        Tool::Healing => "tools-hbrush",
        Tool::Patch => "tools-patch",
        Tool::ContentAwareMove => "tools-camove",
        Tool::Brush => "tools-brush",
        Tool::Pencil => "tools-pencil",
        Tool::MixerBrush => "tools-mbrush",
        Tool::CloneStamp => "tools-clone",
        Tool::Eraser => "tools-eraser",
        Tool::BackgroundEraser => "tools-beraser",
        Tool::Gradient => "tools-gradient",
        Tool::PaintBucket => "tools-pbucket",
        Tool::Blur => "tools-blur",
        Tool::Sharpen => "tools-sharpen",
        Tool::Smudge => "tools-smudge",
        Tool::Dodge => "tools-dodge",
        Tool::Burn => "tools-burn",
        Tool::Sponge => "tools-sponge",
        Tool::Pen => "tools-pen",
        Tool::FreeformPen => "tools-fpen",
        Tool::CurvaturePen => "tools-cpen",
        Tool::AddAnchorPoint => "tools-addanchor",
        Tool::DeleteAnchorPoint => "tools-delanchor",
        Tool::ConvertPoint => "tools-convert",
        Tool::Type => "tools-htype",
        Tool::VerticalType => "tools-vtype",
        Tool::PathSelection => "tools-pselect",
        Tool::DirectSelection => "tools-dselect",
        Tool::Rectangle => "tools-rect",
        Tool::EllipseShape => "tools-ellipse",
        Tool::Line => "tools-line",
        Tool::CustomShape => "tools-cshape",
        Tool::Hand => "tools-hand",
        Tool::Zoom => "tools-zoom",
        // Hidden by default (PhotoSplice has no such tool), so its library has no glyph for
        // them; they keep a Lucide stand-in for anyone who re-shows one in Edit › Toolbar.
        Tool::MagicEraser => "eraser-magic",
        Tool::HistoryBrush => "clock",
        Tool::Note => "message-square",
        Tool::Count => "circle-dot",
        Tool::Triangle => "triangle",
        Tool::Polygon => "pentagon",
    }
}

/// Selected and hover fill shared by square icon buttons. Returns the icon tint.
pub fn button_chrome(ui: &egui::Ui, rect: Rect, selected: bool, hovered: bool) -> Color32 {
    let t = Tokens::get(ui.ctx());
    if selected {
        ui.painter().rect_filled(rect, t.radius_sm, t.accent_soft);
        ui.painter().rect_stroke(rect, t.radius_sm, egui::Stroke::new(1.0, t.accent_border), egui::StrokeKind::Inside);
    } else if hovered {
        ui.painter().rect_filled(rect, t.radius_sm, t.hover);
    }
    if selected {
        t.accent_text
    } else if hovered {
        t.text
    } else {
        t.icon
    }
}

/// Square icon button: transparent until hovered; `selected` gets the accent treatment.
pub fn button(ui: &mut egui::Ui, name: &str, box_size: f32, selected: bool, tooltip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(box_size), Sense::click());
    let tint = button_chrome(ui, rect, selected, resp.hovered());
    paint(ui, rect, name, (box_size * 0.52).round(), tint);
    if tooltip.is_empty() {
        return resp;
    }
    // The tooltip is the only text this control has, so it is also its accessible name:
    // without this a screen reader (and `get_by_label`) sees an unnamed button.
    let label = tl!(tooltip);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label));
    resp.on_hover_text(label)
}

/// Rail toggle: "on" gets a quiet filled background and full-strength icon (no accent).
pub fn rail_button(ui: &mut egui::Ui, name: &str, box_size: f32, on: bool, tooltip: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(box_size), Sense::click());
    if on {
        ui.painter().rect_filled(rect, t.radius_sm, t.card);
        ui.painter().rect_stroke(rect, t.radius_sm, egui::Stroke::new(1.0, t.card_border), egui::StrokeKind::Inside);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, t.radius_sm, t.hover);
    }
    let tint = if on || resp.hovered() { t.text } else { t.text_faint };
    paint(ui, rect, name, (box_size * 0.52).round(), tint);
    resp.on_hover_text(tl!(tooltip))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_an_icon() {
        for t in Tool::ALL {
            assert!(exists(tool_icon(t)), "{t:?}");
        }
    }

    #[test]
    fn icons_are_recoloured() {
        let m = white_icons();
        assert!(m.len() >= 60);
        for (name, b) in m.iter() {
            let s = std::str::from_utf8(b).unwrap();
            assert!(!s.contains("currentColor"), "{name}");
        }
    }

    /// Every icon, rendered the way the app renders it, is a drawing rather than a filled block:
    /// a stylesheet's `fill: none` used to come out as a white fill, so Dodge, Slice Select and
    /// the two eyedroppers showed as squares.
    #[test]
    fn no_icon_renders_as_a_solid_block() {
        let opts = resvg::usvg::Options::default();
        let mut worst = Vec::new();
        for (name, bytes) in white_icons().iter() {
            let tree = resvg::usvg::Tree::from_data(bytes, &opts).unwrap_or_else(|e| panic!("{name}: {e}"));
            let side = 48u32;
            let mut pm = resvg::tiny_skia::Pixmap::new(side, side).unwrap();
            let s = tree.size();
            let k = side as f32 / s.width().max(s.height());
            resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(k, k), &mut pm.as_mut());
            let painted = pm.pixels().iter().filter(|p| p.alpha() > 128).count() as f32 / (side * side) as f32;
            // Pixels are premultiplied: compare the colour of the opaque-enough ones.
            let white = |p: &resvg::tiny_skia::PremultipliedColorU8| {
                let c = p.demultiply();
                c.alpha() < 32 || (c.red() > 200 && c.green() > 200 && c.blue() > 200)
            };
            assert!(pm.pixels().iter().all(white), "{name}: not white");
            if painted > 0.6 {
                worst.push(format!("{name} ({:.0}%)", painted * 100.0));
            }
        }
        assert!(worst.is_empty(), "icons that paint most of their square: {worst:?}");
    }

    #[test]
    fn stylesheet_rules_become_attributes_as_written() {
        let svg = std::str::from_utf8(include_bytes!("../../../assets/ico/tools/dodge.svg")).unwrap();
        let w = whiten(svg);
        assert!(!w.contains("<style") && !w.contains("class="));
        assert!(w.contains(r#"fill="none" d="m0 0h24v24h-24z""#), "the background path stays unfilled: {w}");
        assert!(w.contains(r##"stroke="#ffffff""##) && w.contains(r#"stroke-width="1.3""#));
    }
}
