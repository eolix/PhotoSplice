//! Icon rendering. SVGs are embedded (see `icon_data.rs`), recoloured to white, rasterized by the
//! egui_extras SVG loader at the exact on-screen pixel size (crisp at any DPI), and tinted per use.
//!
//! (History: the first shell used Unicode glyphs; half of them rendered as tofu boxes because the
//! bundled fonts lacked them. Vector icons fix that for good.)

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use craft_ui::buttons::{ButtonVisuals, IconButton, IconButtonStyle};
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

/// An icon's whitened SVG as an egui image source; an unknown name draws the fallback square.
fn source(name: &str) -> egui::ImageSource<'static> {
    let bytes = white_icons().get(name).or_else(|| white_icons().get("square")).cloned().unwrap_or_default();
    egui::ImageSource::Bytes { uri: format!("bytes://icons/{name}.svg").into(), bytes: egui::load::Bytes::Shared(bytes) }
}

pub fn exists(name: &str) -> bool {
    white_icons().contains_key(name)
}

/// An egui image for an icon, tinted.
pub fn image(name: &str, size: f32, tint: Color32) -> egui::Image<'static> {
    egui::Image::new(source(name)).fit_to_exact_size(Vec2::splat(size)).tint(tint)
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

/// A small icon beside the OS arrow pointer at `p` (Photoshop's cursor badges: scissors for
/// cutting the selected pixels). Below-right of the arrow, white with a dark outline. The caller
/// keeps the arrow (`CursorIcon::Default`).
pub fn cursor_badge(ctx: &egui::Context, name: &str, p: egui::Pos2) {
    badge(ctx, name, p, false);
}

/// Photoshop's duplicate badge: a second, dark arrowhead behind a white one.
pub fn cursor_copy_badge(ctx: &egui::Context, p: egui::Pos2) {
    badge(ctx, "mouse-pointer-2", p, true);
}

fn badge(ctx: &egui::Context, name: &str, p: egui::Pos2, doubled: bool) {
    const SIZE: f32 = 12.0;
    let rect = Rect::from_center_size(p + egui::vec2(20.0, 19.0), Vec2::splat(SIZE));
    let area = egui::Area::new(egui::Id::new("pc-cursor-badge")).order(egui::Order::Tooltip).fixed_pos(rect.min).constrain(false).interactable(false);
    area.show(ctx, |ui| {
        let outline = image(name, SIZE, Color32::from_black_alpha(220));
        let halo = [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0), (-1.0, -1.0), (1.0, 1.0), (-1.0, 1.0), (1.0, -1.0)];
        if doubled {
            let behind = rect.translate(egui::vec2(4.0, 4.0));
            for (dx, dy) in halo {
                image(name, SIZE, Color32::WHITE).paint_at(ui, behind.translate(egui::vec2(dx, dy)));
            }
            image(name, SIZE, Color32::BLACK).paint_at(ui, behind);
        }
        for (dx, dy) in halo {
            outline.paint_at(ui, rect.translate(egui::vec2(dx, dy)));
        }
        image(name, SIZE, Color32::WHITE).paint_at(ui, rect);
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
        // Tools PhotoSplice's library has no glyph for keep their Lucide icons.
        Tool::Artboard => "artboard",
        Tool::SingleRowMarquee => "marquee-row",
        Tool::SingleColumnMarquee => "marquee-column",
        Tool::ColorReplacement => "brush-replace",
        Tool::ColorSampler => "sampler",
        Tool::PerspectiveCrop => "perspective-crop",
        Tool::Frame => "frame-placeholder",
        Tool::HorizontalTypeMask => "type-mask",
        Tool::VerticalTypeMask => "type-mask-vertical",
        Tool::RotateView => "compass",
        Tool::Remove => "bandage-sparkle",
        Tool::RedEye => "eye",
        Tool::PatternStamp => "grid-3x3",
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
    button_with_icon_size(ui, name, box_size, (box_size * 0.52).round(), selected, tooltip)
}

/// Square icon button with an explicit glyph size, independent of its hit area.
pub fn button_with_icon_size(ui: &mut egui::Ui, name: &str, box_size: f32, icon_size: f32, selected: bool, tooltip: &str) -> Response {
    button_with_icon_size_tip(ui, name, box_size, icon_size, selected, tooltip, true)
}

/// [`button_with_icon_size`] whose tooltip shows only when `show_tip`; the tooltip text still
/// names the button (for screen readers and tests) when it is hidden.
pub fn button_with_icon_size_tip(ui: &mut egui::Ui, name: &str, box_size: f32, icon_size: f32, selected: bool, tooltip: &str, show_tip: bool) -> Response {
    let t = Tokens::get(ui.ctx());
    let style = IconButtonStyle {
        normal: ButtonVisuals { fill: Color32::TRANSPARENT, stroke: egui::Stroke::NONE, icon: t.icon },
        hovered: ButtonVisuals { fill: t.hover, stroke: egui::Stroke::NONE, icon: t.text },
        pressed: None,
        selected: ButtonVisuals { fill: t.accent_soft, stroke: egui::Stroke::new(1.0, t.accent_border), icon: t.accent_text },
        corner_radius: t.radius_sm.into(),
        focus_stroke: egui::Stroke::new(1.0, t.accent),
    };
    let label = if tooltip.is_empty() { name } else { tl!(tooltip) };
    let resp = IconButton::new(label, Vec2::splat(box_size), &style).selected(selected).show(ui, |ui, rect, tint| {
        paint(ui, rect, name, icon_size, tint);
    });
    if tooltip.is_empty() || !show_tip { resp } else { resp.on_hover_text(label) }
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
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), on, tl!(tooltip)));
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
        assert_eq!(tool_icon(Tool::Gradient), "tools-gradient");
        assert_ne!(tool_icon(Tool::Gradient), tool_icon(Tool::PaintBucket));
    }

    /// Rasterize an icon to its ink mask at `size` px.
    fn ink(name: &str, size: u32) -> Vec<bool> {
        let egui::ImageSource::Bytes { bytes, .. } = source(name) else { panic!("icon must be embedded bytes") };
        let image = egui_extras::image::load_svg_bytes_with_size(
            bytes.as_ref(),
            egui::load::SizeHint::Size { width: size, height: size, maintain_aspect_ratio: true },
            &Default::default(),
        )
        .unwrap();
        assert_eq!(image.size, [size as usize; 2]);
        image.pixels.iter().map(|p| p.a() > 16).collect()
    }

    #[test]
    fn quick_selection_icon_differs_from_ellipse_marquee() {
        // #5116: Quick Selection used to reuse the Ellipse Marquee's dashed circle.
        assert_ne!(tool_icon(Tool::QuickSelection), tool_icon(Tool::EllipseMarquee));
        for size in [14_u32, 28] {
            let (quick, ellipse) = (ink(tool_icon(Tool::QuickSelection), size), ink(tool_icon(Tool::EllipseMarquee), size));
            let shared = quick.iter().zip(&ellipse).filter(|(a, b)| **a && **b).count();
            let union = quick.iter().zip(&ellipse).filter(|(a, b)| **a || **b).count();
            assert!(union > 0, "icons must have ink at {size}px");
            let overlap = shared as f32 / union as f32;
            assert!(overlap < 0.5, "the two glyphs must read as distinct: {overlap:.2} of their ink overlaps at {size}px");
        }
    }

    #[test]
    fn catalog_serves_every_icon_whitened() {
        // Every real asset keeps its cache key and goes through `whiten` on the way in.
        assert!(ICONS.len() >= 60);
        for (name, original) in ICONS {
            let expected = whiten(&String::from_utf8_lossy(original));
            let egui::ImageSource::Bytes { uri, bytes } = source(name) else { panic!("expected SVG bytes") };
            assert_eq!(uri.as_ref(), format!("bytes://icons/{name}.svg"));
            assert_eq!(bytes.as_ref(), expected.as_bytes(), "{name}");
            assert!(!std::str::from_utf8(&bytes).unwrap().contains("currentColor"), "{name}");
        }
    }

    #[test]
    fn shared_catalog_preserves_fallback() {
        let egui::ImageSource::Bytes { uri, bytes } = source("missing-icon") else { panic!("expected SVG bytes") };
        let egui::ImageSource::Bytes { bytes: fallback, .. } = source("square") else { panic!("expected SVG bytes") };
        assert_eq!(uri.as_ref(), "bytes://icons/missing-icon.svg");
        assert_eq!(bytes.as_ref(), fallback.as_ref());
        assert!(!exists("missing-icon"));
    }

    #[test]
    fn shared_button_preserves_theme_painting_and_pointer_responses() {
        // The unchanged chrome helper is the pre-extraction paint oracle. Keyboard focus
        // and accessibility metadata are deliberate additions, covered in craft-ui.
        fn frame(
            ctx: &egui::Context,
            events: Vec<egui::Event>,
            selected: bool,
            enabled: bool,
            legacy: bool,
        ) -> (egui::Response, Vec<egui::epaint::ClippedShape>) {
            let mut response = None;
            let mut output = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| {
                response = Some(
                    ui.add_enabled_ui(enabled, |ui| {
                        if legacy {
                            let (rect, response) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::click());
                            let tint = button_chrome(ui, rect, selected, response.hovered());
                            paint(ui, rect, "square", (28.0_f32 * 0.52).round(), tint);
                            response
                        } else {
                            button(ui, "square", 28.0, selected, "")
                        }
                    })
                    .inner,
                );
            });
            output.textures_delta.clear();
            (response.unwrap(), output.shapes)
        }
        for theme in crate::theme::ThemeKind::ALL {
            for selected in [false, true] {
                for enabled in [false, true] {
                    let old = egui::Context::default();
                    let new = egui::Context::default();
                    for ctx in [&old, &new] {
                        crate::theme::apply(ctx, theme);
                        egui_extras::install_image_loaders(ctx);
                    }
                    let mut center = egui::Pos2::ZERO;
                    for step in 0..7 {
                        let events = match step {
                            2 => vec![egui::Event::PointerMoved(center)],
                            3 | 4 => vec![egui::Event::PointerButton {
                                pos: center,
                                button: egui::PointerButton::Primary,
                                pressed: step == 3,
                                modifiers: egui::Modifiers::NONE,
                            }],
                            5 => vec![egui::Event::PointerMoved(egui::pos2(300.0, 200.0))],
                            _ => vec![],
                        };
                        let (a, before) = frame(&old, events.clone(), selected, enabled, true);
                        let (b, after) = frame(&new, events, selected, enabled, false);
                        center = a.rect.center();
                        assert_eq!(a.rect, b.rect);
                        assert_eq!((a.clicked(), a.hovered(), a.is_pointer_button_down_on()), (b.clicked(), b.hovered(), b.is_pointer_button_down_on()));
                        assert_eq!(before, after, "{theme:?}, selected={selected}, enabled={enabled}, step={step}");
                    }
                }
            }
        }
    }

    #[test]
    fn symmetry_axes_rasterize_centred_with_matching_line_weight_at_both_dpis() {
        for size in [14_u32, 28] {
            let mut coverage = Vec::new();
            for name in ["symmetry-vertical", "symmetry-horizontal", "symmetry-diagonal", "symmetry-dual"] {
                assert!(exists(name));
                let egui::ImageSource::Bytes { bytes, .. } = source(name) else { panic!("icon source must be embedded bytes") };
                let image = egui_extras::image::load_svg_bytes_with_size(
                    bytes.as_ref(),
                    egui::load::SizeHint::Size { width: size, height: size, maintain_aspect_ratio: true },
                    &Default::default(),
                )
                .unwrap();
                assert_eq!(image.size, [size as usize; 2]);
                let ink: Vec<_> = image.pixels.iter().enumerate().filter(|(_, p)| p.a() > 16).collect();
                assert!(!ink.is_empty(), "{name}");
                let xs: Vec<_> = ink.iter().map(|(i, _)| i % size as usize).collect();
                let ys: Vec<_> = ink.iter().map(|(i, _)| i / size as usize).collect();
                for coords in [&xs, &ys] {
                    let centre = (*coords.iter().min().unwrap() + *coords.iter().max().unwrap()) as f32 / 2.0;
                    assert!((centre - (size as f32 - 1.0) / 2.0).abs() <= 0.5, "{name}: off-centre");
                }
                assert!(ink.iter().all(|(_, p)| p.r() > 0 && p.r() == p.g() && p.g() == p.b()), "{name}: tinting mask must be white");
                coverage.push(image.pixels.iter().map(|p| f32::from(p.a()) / 255.0).sum::<f32>());
            }
            let vertical = coverage[0];
            // At 14px a diagonal's antialiasing can differ by nearly two covered pixels.
            let tolerance = if size == 14 { 0.15 } else { 0.1 };
            for single in &coverage[1..3] {
                assert!((single - vertical).abs() / vertical < tolerance, "inconsistent axis stroke weight at {size}px: {coverage:?}");
            }
            assert!(coverage[3] > vertical * 1.7 && coverage[3] < vertical * 2.1);
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
