//! Vector tools: Shape tools (U), Pen (P), Path Selection (A), path overlays and the Paths panel.
//! Direct Selection (A) lives in `direct_select`.
//! All edits go through the engine's `shape.*` / `path.*` commands.

use egui::{Color32, Pos2, Rect, Sense, Stroke, pos2, vec2};
use photosplice_doc::vector::Path;
use photosplice_doc::{Document, LayerContent};
use photosplice_geom::Affine;

use serde_json::{Value, json};

use crate::PhotospliceApp;
use crate::canvas::ViewXform;
use crate::state::{Tool, ToolOptions};
use crate::theme::Tokens;

pub fn is_shape_tool(t: Tool) -> bool {
    matches!(t, Tool::Rectangle | Tool::EllipseShape | Tool::Triangle | Tool::Polygon | Tool::Line | Tool::CustomShape)
}

fn rgb32(c: [f32; 4]) -> Color32 {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgb(b(c[0]), b(c[1]), b(c[2]))
}

fn hex(c: [f32; 4]) -> String {
    let c = rgb32(c);
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

fn stroke_param(app: &PhotospliceApp) -> Value {
    let o = &app.ui.tool_options;
    if o.stroke_width <= 0.0 {
        return Value::Null;
    }
    let mut p = o.shape_stroke.params();
    p["width"] = json!(o.stroke_width);
    p["color"] = json!(app.session.tools.background);
    p
}

/// Geometry of a Shape-tool drag as `shape.create` params, without fill and stroke (None when too
/// small): ⇧ constrains proportions, ⌥ draws from the centre. Custom shapes give their box as a `rect`.
fn shape_geometry(o: &ToolOptions, tool: Tool, start: [f64; 2], end: [f64; 2], mods: egui::Modifiers) -> Option<Value> {
    Some(if tool == Tool::Line {
        let (mut dx, mut dy) = (end[0] - start[0], end[1] - start[1]);
        if mods.shift {
            // Snap to 45°.
            let a = (dy.atan2(dx) / std::f64::consts::FRAC_PI_4).round() * std::f64::consts::FRAC_PI_4;
            let len = dx.hypot(dy);
            (dx, dy) = (len * a.cos(), len * a.sin());
        }
        if dx.hypot(dy) < 1.0 {
            return None;
        }
        json!({"kind": "line", "from": start, "to": [start[0] + dx, start[1] + dy], "weight": o.line_weight.max(1.0)})
    } else {
        let (mut w, mut h) = (end[0] - start[0], end[1] - start[1]);
        if mods.shift {
            let m = w.abs().max(h.abs());
            (w, h) = (m * w.signum(), m * h.signum());
        }
        let (x0, y0) = if mods.alt { (start[0] - w, start[1] - h) } else { (start[0], start[1]) };
        let (w, h) = if mods.alt { (w * 2.0, h * 2.0) } else { (w, h) };
        let rect = [x0.min(x0 + w).round(), y0.min(y0 + h).round(), w.abs().round(), h.abs().round()];
        if rect[2] < 1.0 || rect[3] < 1.0 {
            return None;
        }
        match tool {
            Tool::Rectangle if o.corner_radius > 0.0 => json!({"kind": "roundedRect", "rect": rect, "radii": vec![o.corner_radius; 4]}),
            Tool::Rectangle | Tool::CustomShape => json!({"kind": "rect", "rect": rect}),
            Tool::EllipseShape => json!({"kind": "ellipse", "rect": rect}),
            Tool::Triangle => json!({"kind": "polygon", "rect": rect, "sides": 3}),
            _ => json!({"kind": "polygon", "rect": rect, "sides": o.polygon_sides.max(3)}),
        }
    })
}

/// The box `[x0, y0, x1, y1]` a Shape-tool drag draws (a line's: its ends'), which snaps while
/// Space repositions it (`snap_ui`). None when too small to draw.
pub fn shape_bounds(o: &ToolOptions, tool: Tool, start: [f64; 2], end: [f64; 2], mods: egui::Modifiers) -> Option<[f64; 4]> {
    let g = shape_geometry(o, tool, start, end, mods)?;
    if tool == Tool::Line {
        let a: [f64; 2] = serde_json::from_value(g["from"].clone()).ok()?;
        let b: [f64; 2] = serde_json::from_value(g["to"].clone()).ok()?;
        return Some([a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1])]);
    }
    let [x, y, w, h]: [f64; 4] = serde_json::from_value(g["rect"].clone()).ok()?;
    Some([x, y, x + w, y + h])
}

/// Finish a Shape-tool drag: ⇧ constrains proportions, ⌥ draws from the centre.
pub fn finish_shape(app: &mut PhotospliceApp, tool: Tool, start: [f64; 2], end: [f64; 2], mods: egui::Modifiers) {
    let Some(mut p) = shape_geometry(&app.ui.tool_options, tool, start, end, mods) else { return };
    if tool == Tool::CustomShape {
        // ⇧ keeps the shape's proportions (the rect is already squared).
        if let Ok(rect) = serde_json::from_value(p["rect"].take()) {
            let (fill, stroke) = (fill_param(app), stroke_param(app));
            crate::preset_panels::finish_custom_shape(app, rect, mods.shift, fill, stroke);
        }
        return;
    }
    if let Err(e) = create_shape(app, p) {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

fn fill_param(app: &PhotospliceApp) -> Value {
    if app.ui.tool_options.shape_fill { json!(app.session.tools.foreground) } else { Value::Null }
}

/// Create a shape layer from `shape.create` geometry with the options bar's fill and stroke: the
/// end of a drag, and the Create Rectangle / Ellipse / … dialogs (`shape_dialog`).
pub fn create_shape(app: &mut PhotospliceApp, mut geometry: Value) -> Result<Value, String> {
    geometry["fill"] = fill_param(app);
    geometry["stroke"] = stroke_param(app);
    app.run("shape.create", geometry)
}

/// The preview and command both use the full float tool colours, including transparent paint.
fn shape_preview(app: &PhotospliceApp, tool: Tool, start: [f64; 2], end: [f64; 2], mods: egui::Modifiers) -> Option<photosplice_doc::ShapeLayer> {
    let o = &app.ui.tool_options;
    let geometry = shape_geometry(o, tool, start, end, mods)?;
    let path = if tool == Tool::CustomShape {
        let rect = geometry.get("rect").and_then(|r| serde_json::from_value::<[f64; 4]>(r.clone()).ok())?;
        let groups = photosplice_engine::presets::shapes::all_groups(&app.session);
        let shape = groups.iter().flat_map(|g| &g.items).find(|s| s.name == app.ui.presets_ui.shape())?;
        photosplice_engine::presets::shapes::fit(&shape.path, rect, mods.shift)
    } else {
        photosplice_engine::vector_cmds::shape_path(&geometry).ok()?
    };
    Some(photosplice_doc::ShapeLayer {
        path,
        fill: o.shape_fill.then(|| solid_paint(app.session.tools.foreground)),
        stroke: (o.stroke_width > 0.0).then(|| o.shape_stroke.stroke(o.stroke_width, solid_paint(app.session.tools.background))),
        ..Default::default()
    })
}

/// Shape-tool drag preview using the same path and vector stroke geometry as the commit.
pub fn draw_shape_preview(app: &PhotospliceApp, painter: &egui::Painter, xf: &ViewXform, tool: Tool, start: [f64; 2], end: [f64; 2], mods: egui::Modifiers) {
    let Some(mut shape) = shape_preview(app, tool, start, end, mods) else { return };
    let path = shape.path.clone();
    let origin = xf.to_screen(0.0, 0.0);
    let x = xf.to_screen(1.0, 0.0) - origin;
    let y = xf.to_screen(0.0, 1.0) - origin;
    let transform = Affine { m: [x.x as f64, x.y as f64, y.x as f64, y.y as f64, origin.x as f64, origin.y as f64] };
    shape.path = shape.path.transform(&transform);
    if let Some(stroke) = &mut shape.stroke {
        stroke.width *= xf.zoom;
    }
    // This lightweight screen overlay retains the existing display-colour behaviour; document
    // rendering still goes through its format/profile path when the shape is committed.
    crate::shape_stroke_ui::paint_shape_preview(painter, &shape);
    let accent = Tokens::get(painter.ctx()).accent;
    for (pts, _) in path_lines(&path, &|q| xf.to_screen(q[0] as f32, q[1] as f32)) {
        // The accent path over a dark halo: visible on any pixels (#172).
        painter.extend(crate::tool_feedback::contrast_path(pts, true, accent));
    }
}

// ---------------------------------------------------------------------------------------------
// Pen

/// ↩ / a tool switch / ⌘-click: end the subpath the Pen is drawing. PhotoSplice's Pen
/// (`pen_tool`) edits the document path at every click, so ending it commits nothing more;
/// `closed` is kept for callers of the older draft-path design.
pub fn pen_commit(app: &mut PhotospliceApp, _closed: bool) {
    crate::pen_tool::end(app);
}

/// Edit › Undo while drawing: false, so Undo is the document's. Every Pen click is its own
/// history step (`pen_tool`), so undoing the last point is a plain Undo.
pub fn pen_undo_last_point(_app: &mut PhotospliceApp) -> bool {
    false
}

/// The path that path commands act on without a name: the one selected in the Paths panel, else
/// the work path, else the selected layer's shape path or vector mask; `None` when there's none.
pub fn active_path_name(app: &PhotospliceApp) -> Option<String> {
    let st = app.session.active()?;
    let rows = path_rows(&st.doc, st.active_layer);
    let key = |r: &PathEntry| match r.kind {
        PathRow::Work => "work".to_string(),
        PathRow::Layer => "layer".to_string(),
        PathRow::Saved => r.name.clone(),
    };
    let selected = app.ui.selected_path.as_deref().filter(|s| rows.iter().any(|r| key(r) == *s)).map(str::to_string);
    selected.or_else(|| rows.iter().find(|r| r.kind == PathRow::Work).map(key)).or_else(|| rows.iter().find(|r| r.kind == PathRow::Layer).map(key))
}

/// The commands that create a fill or adjustment layer and take a `"path"` as its vector mask.
pub fn takes_path_mask(id: &str) -> bool {
    id.starts_with("layer.newFillLayer.") || id.starts_with("layer.newAdjustmentLayer.")
}

/// The work or saved path explicitly selected in the Paths panel (not a layer's own path).
pub fn selected_document_path(app: &PhotospliceApp) -> Option<String> {
    let st = app.session.active()?;
    let sel = app.ui.selected_path.as_deref()?;
    let listed = path_rows(&st.doc, st.active_layer).iter().any(|r| match r.kind {
        PathRow::Work => sel == "work",
        PathRow::Saved => r.name == sel,
        PathRow::Layer => false,
    });
    listed.then(|| sel.to_string())
}

/// New fill and adjustment layers take the path selected in the Paths panel as their vector mask,
/// as in Photoshop (#1419): the command gets it as `"path"` unless the caller set one (`null`
/// opts out). Only an explicitly selected work or saved path counts; deselect it by clicking the
/// panel's empty area.
pub fn with_active_path(app: &PhotospliceApp, id: &str, params: Value) -> Value {
    if !takes_path_mask(id) || params.get("path").is_some() {
        return params;
    }
    let Some(sel) = selected_document_path(app) else { return params };
    match params {
        Value::Object(mut m) => {
            m.insert("path".into(), json!(sel));
            Value::Object(m)
        }
        _ => json!({ "path": sel }),
    }
}

/// ⌘↩ / Ctrl+Enter (#306): load a path as a selection, like Photoshop with a Pen or Path
/// Selection tool or a path selected in the Paths panel. A path being drawn with the Pen is
/// finished first and loads; otherwise [`active_path_name`]. `params` may set feather, mode…
pub fn path_to_selection(app: &mut PhotospliceApp, params: Value) -> Result<Value, String> {
    // The path being drawn with the Pen is the one that loads.
    if let Some(a) = app.ui.pen.active {
        app.ui.selected_path = Some(if matches!(a.target, crate::direct_select::PathRef::Work) { "work" } else { "layer" }.into());
        crate::pen_tool::end(app);
    }
    let name = active_path_name(app)
        .ok_or_else(|| tl!("No path to make a selection from: draw one with the Pen tool or select one in the Paths panel.").to_string())?;
    let mut p = params.as_object().cloned().unwrap_or_default();
    p.insert("name".into(), json!(name));
    app.run("path.toSelection", Value::Object(p))
}

// ---------------------------------------------------------------------------------------------
// Path Selection

/// What Path Selection edits.
enum PathTarget {
    Shape(u64),
    /// The active layer's vector mask, when its Layers thumbnail is targeted (#196).
    VectorMask(u64),
    Work,
}

/// The targeted vector mask of the active layer, if the Layers panel targets it.
pub(crate) fn targeted_vector_mask(app: &PhotospliceApp) -> Option<(u64, Path)> {
    let st = app.session.active()?;
    let l = st.active_layer.and_then(|id| st.doc.layer(id))?;
    let targeted = app.ui.vector_mask_target || st.channel_view.target == photosplice_engine::channel_cmds::ChannelTarget::VectorMask;
    (targeted && !matches!(l.content, LayerContent::Shape(_))).then_some(())?;
    l.vector_mask.as_ref().map(|m| (l.id.0, m.path.clone()))
}

/// The active shape layer's path, if the active layer is a shape layer.
pub(crate) fn active_shape_path(app: &PhotospliceApp) -> Option<(u64, Path)> {
    let st = app.session.active()?;
    let l = st.active_layer.and_then(|id| st.doc.layer(id))?;
    match &l.content {
        LayerContent::Shape(sh) => Some((l.id.0, sh.path.clone())),
        _ => None,
    }
}

/// Does ⌫ / Delete delete the work path? With the Pen, Freeform Pen or Path Selection tool, no
/// path being drawn and the work path the one these tools show and edit (no shape layer or
/// targeted vector mask in front of it), as in Photoshop (#5702). It was drawn whenever one of
/// these tools was active, but the key reached Edit › Clear, so it looked impossible to delete.
/// Direct Selection and the Curvature Pen delete selected anchors in Photoshop, not the path.
pub(crate) fn delete_key_deletes_work_path(app: &PhotospliceApp) -> bool {
    matches!(app.ui.tool, Tool::Pen | Tool::FreeformPen | Tool::PathSelection)
        && !app.ui.pen.drawing()
        && app.ui.transform.is_none()
        && app.ui.view.shows(app.ui.view.show.target_path)
        && matches!(target_path(app), Some((PathTarget::Work, _)))
}

/// The path Path Selection edits: the targeted vector mask, the active shape layer's path, else
/// the work path.
fn target_path(app: &PhotospliceApp) -> Option<(PathTarget, Path)> {
    if let Some((id, p)) = targeted_vector_mask(app) {
        return Some((PathTarget::VectorMask(id), p));
    }
    if let Some((id, p)) = active_shape_path(app) {
        return Some((PathTarget::Shape(id), p));
    }
    app.session.active()?.doc.work_path.clone().map(|p| (PathTarget::Work, p))
}

/// Edit › Free Transform Path (⌘T with Path Selection or Direct Selection): the path the
/// box transforms (the one Path Selection edits) and `path.transform`'s params that name it.
pub(crate) fn free_transform_path(app: &PhotospliceApp) -> Option<(Value, Path)> {
    if !matches!(app.ui.tool, Tool::PathSelection | Tool::DirectSelection) {
        return None;
    }
    let (target, path) = target_path(app)?;
    path.control_bounds()?;
    let params = match target {
        PathTarget::Shape(id) | PathTarget::VectorMask(id) => json!({"name": "layer", "layer": id}),
        PathTarget::Work => json!({"name": "work"}),
    };
    Some((params, path))
}

fn point_in_subpath(sp: &photosplice_doc::vector::Subpath, pt: [f64; 2]) -> bool {
    if !sp.closed || sp.knots.len() < 3 {
        return false;
    }
    let mut inside = false;
    let n = sp.knots.len();
    for i in 0..n {
        let j = (i + 1) % n;
        let xi = sp.knots[i].anchor.x;
        let yi = sp.knots[i].anchor.y;
        let xj = sp.knots[j].anchor.x;
        let yj = sp.knots[j].anchor.y;
        let intersect = ((yi > pt[1]) != (yj > pt[1])) && (pt[0] < (xj - xi) * (pt[1] - yi) / (yj - yi) + xi);
        if intersect {
            inside = !inside;
        }
    }
    inside
}

/// The topmost visible shape layer under `at` (document coordinates):
/// hits the shape path or anchor within `tol`, or any non-transparent pixel of the shape's raster surface,
/// or point inside the shape's filled geometry.
pub fn pick_shape_at(doc: &Document, at: [f64; 2], tol: f64) -> Option<photosplice_doc::LayerId> {
    let (x, y) = (at[0].floor() as i32, at[1].floor() as i32);
    for (path, _, l) in doc.walk().iter().rev() {
        if !l.visible || !(1..=path.len()).all(|n| doc.layer_at(&path[..n]).is_some_and(|a| a.visible)) {
            continue;
        }
        let board = path.first().and_then(|i| doc.layers.get(*i)).and_then(|t| t.artboard());
        if board.is_some_and(|a| !a.rect.contains(x, y)) {
            continue;
        }
        if let LayerContent::Shape(sh) = &l.content {
            if photosplice_vector::edit::hit(&sh.path, photosplice_geom::Point::new(at[0], at[1]), tol, &[]).is_some() {
                return Some(l.id);
            }
            if sh.fill.is_some() {
                if let Some(surf) = l.surface() {
                    let mut px = [[0.0f32; 4]; 1];
                    surf.read_rgba_into(photosplice_doc::Rect::from_xywh(x, y, 1, 1), &mut px);
                    if px[0][3] > 0.0 {
                        return Some(l.id);
                    }
                }
                if let Some(photosplice_doc::LiveShape::Rect { rect, .. }) = &sh.live {
                    let x0 = rect[0].min(rect[0] + rect[2]);
                    let x1 = rect[0].max(rect[0] + rect[2]);
                    let y0 = rect[1].min(rect[1] + rect[3]);
                    let y1 = rect[1].max(rect[1] + rect[3]);
                    if at[0] >= x0 && at[0] <= x1 && at[1] >= y0 && at[1] <= y1 {
                        return Some(l.id);
                    }
                }
                if let Some(photosplice_doc::LiveShape::Ellipse { rect }) = &sh.live {
                    let rx = rect[2].abs() / 2.0;
                    let ry = rect[3].abs() / 2.0;
                    if rx > 1e-6 && ry > 1e-6 {
                        let cx = rect[0] + rect[2] / 2.0;
                        let cy = rect[1] + rect[3] / 2.0;
                        let dx = (at[0] - cx) / rx;
                        let dy = (at[1] - cy) / ry;
                        if dx * dx + dy * dy <= 1.0 {
                            return Some(l.id);
                        }
                    }
                }
                if sh.path.subpaths.iter().any(|sp| point_in_subpath(sp, at)) {
                    return Some(l.id);
                }
            }
        }
    }
    None
}

pub fn path_selection_finish(app: &mut PhotospliceApp, start: [f64; 2], end: [f64; 2]) {
    let (dx, dy) = (end[0] - start[0], end[1] - start[1]);
    let tol = 6.0 / f64::from(app.point_zoom().max(0.01));
    if dx.abs() + dy.abs() < 0.5 {
        let Some(st) = app.session.active() else { return };
        if let Some(id) = pick_shape_at(&st.doc, start, tol) {
            let _ = app.run("layer.select", json!({"layer": id.0}));
        }
        return;
    }
    if target_path(app).is_none() {
        let Some(st) = app.session.active() else { return };
        if let Some(id) = pick_shape_at(&st.doc, start, tol) {
            let _ = app.run("layer.select", json!({"layer": id.0}));
        }
    }
    let Some((target, path)) = target_path(app) else { return };
    let _ = match target {
        PathTarget::Shape(id) => app.run("shape.edit", json!({"layer": id, "move": [dx.round(), dy.round()]})),
        PathTarget::VectorMask(id) => {
            let moved = path.transform(&Affine::translate(dx.round(), dy.round()));
            app.run("layer.vectorMask.edit", json!({"layer": id, "path": photosplice_engine::vector_cmds::path_json(&moved)}))
        }
        PathTarget::Work => {
            let moved = path.transform(&Affine::translate(dx.round(), dy.round()));
            app.run("path.set", json!({"name": "work", "path": photosplice_engine::vector_cmds::path_json(&moved)}))
        }
    };
}

// ---------------------------------------------------------------------------------------------
// Drawing

fn bezier(p0: [f64; 2], c0: [f64; 2], c1: [f64; 2], p1: [f64; 2], t: f64) -> [f64; 2] {
    let u = 1.0 - t;
    let f = |a: f64, b: f64, c: f64, d: f64| u * u * u * a + 3.0 * u * u * t * b + 3.0 * u * t * t * c + t * t * t * d;
    [f(p0[0], c0[0], c1[0], p1[0]), f(p0[1], c0[1], c1[1], p1[1])]
}

/// `q` lies on the chord from `a` to `d` (within a thousandth of a pixel).
fn on_chord(a: [f64; 2], q: [f64; 2], d: [f64; 2]) -> bool {
    let (dx, dy, vx, vy) = (d[0] - a[0], d[1] - a[1], q[0] - a[0], q[1] - a[1]);
    let len = dx.hypot(dy);
    if len < 1e-9 {
        return vx.hypot(vy) < 1e-3;
    }
    let t = (vx * dx + vy * dy) / (len * len);
    (vx * dy - vy * dx).abs() / len < 1e-3 && (-1e-9..=1.0 + 1e-9).contains(&t)
}

/// How far (in screen pixels) a drawn chord may stray from the true curve.
const CURVE_TOLERANCE: f64 = 0.3;
/// Pieces per segment at most: 0.3 px holds for curves bending up to about 1.4 million px on screen.
const MAX_CURVE_PIECES: usize = 1024;

/// Screen points along one cubic segment (both ends included). The control points are mapped to
/// the screen first (`xf` is affine, so the curve maps exactly), and the number of chords follows
/// how much the curve bends there: a straight segment is one chord, and a curve zoomed in stays
/// smooth instead of showing a fixed 16 chords (#4084). Uses the bound
/// `max|B''| / 8n² <= tol` with `max|B''| <= 6·max(|p0−2c0+c1|, |c0−2c1+p1|)`.
pub(crate) fn curve_points(p0: [f64; 2], c0: [f64; 2], c1: [f64; 2], p1: [f64; 2], xf: &dyn Fn([f64; 2]) -> Pos2) -> Vec<Pos2> {
    let [a, b, c, d] = [p0, c0, c1, p1].map(|q| {
        let s = xf(q);
        [f64::from(s.x), f64::from(s.y)]
    });
    let second = |p: [f64; 2], q: [f64; 2], r: [f64; 2]| (p[0] - 2.0 * q[0] + r[0]).hypot(p[1] - 2.0 * q[1] + r[1]);
    let n = (0.75 * second(a, b, c).max(second(b, c, d)) / CURVE_TOLERANCE).sqrt().ceil();
    // A straight segment (controls on the chord, like the Pen's retracted handles) is one chord;
    // NaN (a non-finite point) falls back to one too.
    let n = if n.is_finite() && !(on_chord(a, b, d) && on_chord(a, c, d)) { (n as usize).clamp(1, MAX_CURVE_PIECES) } else { 1 };
    (0..=n)
        .map(|k| {
            let q = bezier(a, b, c, d, k as f64 / n as f64);
            pos2(q[0] as f32, q[1] as f32)
        })
        .collect()
}

/// Screen polylines for a path (one per subpath).
pub(crate) fn path_lines(path: &Path, xf: &dyn Fn([f64; 2]) -> Pos2) -> Vec<(Vec<Pos2>, bool)> {
    path.subpaths
        .iter()
        .map(|s| {
            let n = s.knots.len();
            let mut pts = Vec::new();
            let segs = if s.closed { n } else { n.saturating_sub(1) };
            for i in 0..segs {
                let (a, b) = (&s.knots[i], &s.knots[(i + 1) % n]);
                pts.extend(curve_points([a.anchor.x, a.anchor.y], [a.out_ctrl.x, a.out_ctrl.y], [b.in_ctrl.x, b.in_ctrl.y], [b.anchor.x, b.anchor.y], xf));
            }
            if pts.is_empty()
                && let Some(k) = s.knots.first()
            {
                pts.push(xf([k.anchor.x, k.anchor.y]));
            }
            (pts, s.closed)
        })
        .collect()
}

/// A path's outline in the accent colour.
pub(crate) fn draw_outline(painter: &egui::Painter, p: &Path, to_scr: &dyn Fn([f64; 2]) -> Pos2, accent: Color32) {
    for (pts, closed) in path_lines(p, to_scr) {
        if closed {
            painter.add(egui::Shape::closed_line(pts, Stroke::new(1.0, accent)));
        } else {
            painter.add(egui::Shape::line(pts, Stroke::new(1.0, accent)));
        }
    }
}

/// An anchor point's square: filled with the accent when selected, else hollow (white).
pub(crate) fn draw_anchor(painter: &egui::Painter, c: Pos2, selected: bool, accent: Color32) {
    let r = Rect::from_center_size(c, vec2(6.0, 6.0));
    painter.rect_filled(r, 0.0, if selected { accent } else { Color32::WHITE });
    painter.rect_stroke(r, 0.0, Stroke::new(1.0, accent), egui::StrokeKind::Inside);
}

/// A direction handle: the line from its anchor and the round handle end.
pub(crate) fn draw_handle(painter: &egui::Painter, anchor: Pos2, handle: Pos2, accent: Color32) {
    painter.line_segment([anchor, handle], Stroke::new(1.0, accent));
    painter.circle_filled(handle, 3.0, accent);
}

/// Work path / active shape path outlines, anchors, and the pen path in progress.
pub fn draw_overlay(app: &PhotospliceApp, painter: &egui::Painter, xf: &ViewXform, doc: &Document) {
    let tool = app.ui.tool;
    let accent = Tokens::get(painter.ctx()).accent;
    let to_scr = |q: [f64; 2]| xf.to_screen(q[0] as f32, q[1] as f32);
    let draw_path = |p: &Path, anchors: bool| {
        draw_outline(painter, p, &to_scr, accent);
        if anchors {
            for k in p.subpaths.iter().flat_map(|s| &s.knots) {
                draw_anchor(painter, to_scr([k.anchor.x, k.anchor.y]), false, accent);
            }
        }
    };
    // View › Show › Target Path (under Extras) hides the paths; the Pen's path in progress stays.
    // Free Transform Path draws the path through its box instead (transform_tool).
    let paths = app.ui.view.shows(app.ui.view.show.target_path) && app.ui.transform.as_ref().is_none_or(|t| t.path.is_none());
    if crate::direct_select::shows(app, painter.ctx().input(|i| i.modifiers)) {
        // Direct Selection (or the Pen with ⌘/Ctrl held) draws the paths it edits (#790).
        crate::direct_select::draw_overlay(app, painter, &to_scr, accent, paths);
    } else if paths {
        // Paths remain visible as outlines with any tool. Path Selection adds anchor squares; Direct
        // Selection has its own overlay above.
        if let Some(wp) = &doc.work_path {
            draw_path(wp, tool == Tool::PathSelection);
        }
        let st = app.session.active();
        if let Some(l) = st.and_then(|s| s.active_layer.and_then(|id| s.doc.layer(id)))
            && let LayerContent::Shape(sh) = &l.content
        {
            draw_path(&sh.path, tool == Tool::PathSelection);
        }
        // A targeted vector mask shows its path with any tool (#196).
        if let Some((_, p)) = targeted_vector_mask(app) {
            draw_path(&p, tool == Tool::PathSelection);
        }
        crate::corner_radius::draw(app, painter, &to_scr, accent);
    }
    // The Pen: the path it draws on, the end new points join, and what a click would do.
    if matches!(tool, Tool::Pen | Tool::CurvaturePen | Tool::FreeformPen) {
        crate::pen_tool::draw_overlay(app, painter, &to_scr, accent);
    }
}

// ---------------------------------------------------------------------------------------------
// Options bars

/// Options bar for vector tools; false for other tools.
pub fn options_bar(app: &mut PhotospliceApp, ui: &mut egui::Ui, tool: Tool) -> bool {
    if !(is_shape_tool(tool) || tool.is_pen() || matches!(tool, Tool::PathSelection | Tool::DirectSelection)) {
        return false;
    }
    let t = Tokens::get(ui.ctx());
    let lbl = |ui: &mut egui::Ui, s: &str| {
        ui.label(egui::RichText::new(s).color(t.text_dim).size(12.0));
    };
    let o = &mut app.ui.tool_options;
    let target = selected_shape(&app.session);
    if tool == Tool::PathSelection && target.is_none() {
        lbl(ui, if app.ui.vector_mask_target { "Drag to move the targeted vector mask" } else { tl!("Drag to move the active shape's path or the Work Path") });
        return true;
    }
    if tool == Tool::DirectSelection && target.is_none() {
        let (shift, alt) = (crate::shortcuts::pretty("Shift"), crate::shortcuts::pretty("Alt"));
        lbl(
            ui,
            &crate::i18n::fmt(
                tl!("Click: select anchor · Drag: move anchor, handle or segment · {shift}-click: add · {alt}-click: whole subpath"),
                &[("shift", &shift), ("alt", &alt)],
            ),
        );
        return true;
    }
    match tool {
        Tool::AddAnchorPoint => {
            lbl(ui, tl!("Click a segment to add a point · Drag to move it"));
            return true;
        }
        Tool::DeleteAnchorPoint => {
            lbl(ui, tl!("Click a point to delete it"));
            return true;
        }
        Tool::ConvertPoint => {
            lbl(ui, tl!("Click a point: corner · Drag from a point: smooth · Drag a handle: move it alone"));
            return true;
        }
        _ => {}
    }
    if matches!(tool, Tool::Pen | Tool::CurvaturePen | Tool::FreeformPen) {
        let opts = [("path".to_string(), tl!("Path")), ("shape".to_string(), tl!("Shape"))];
        crate::widgets::dropdown(ui, "pen-mode", &mut o.vector_mode, &opts, 80.0);
        crate::widgets::vline(ui, 22.0);
        // Path operations: how a new subpath combines with the path (Shape mode: New Layer first).
        let mut ops = vec![("combine".to_string(), tl!("Combine Shapes")), ("subtract".to_string(), tl!("Subtract Front Shape")), ("intersect".to_string(), tl!("Intersect Shape Areas")), ("exclude".to_string(), tl!("Exclude Overlapping Shapes"))];
        if o.vector_mode == "shape" {
            ops.insert(0, ("new".to_string(), tl!("New Layer")));
        } else if o.pen_op == "new" {
            o.pen_op = "combine".into();
        }
        crate::widgets::dropdown(ui, "pen-op", &mut o.pen_op, &ops, 170.0);
        if tool == Tool::Pen {
            crate::widgets::vline(ui, 22.0);
            crate::widgets::checkbox(ui, &mut o.auto_add_delete, tl!("Auto Add/Delete"));
        }
        crate::widgets::vline(ui, 22.0);
        let hint = match tool {
            Tool::CurvaturePen => tl!("Click: smooth point · {alt}-click: corner · Drag a point to move it · Click first point: close · {key} finish"),
            Tool::FreeformPen => tl!("Drag to draw a path · {key} finish"),
            _ => tl!("Click: corner · Drag: smooth ({alt}: cusp) · Click first point: close · {key} finish"),
        };
        lbl(ui, &crate::i18n::fmt(hint, &[("key", &crate::shortcuts::pretty("Enter")), ("alt", &crate::shortcuts::pretty("Alt"))]));
        return true;
    }
    let mut mode = "shape".to_string();
    crate::widgets::dropdown(ui, "shape-mode", &mut mode, &[("shape".to_string(), tl!("Shape"))], 80.0);
    crate::widgets::vline(ui, 22.0);
    // Fill and Stroke edit the selected shape layer too (#3347), and still set the next shape's.
    let (fg, bg) = (app.session.tools.foreground, app.session.tools.background);
    let (mut has_fill, fill, stroke) = match &target {
        Some(s) => (s.fill.is_some(), s.fill.clone(), s.stroke.clone()),
        None => (o.shape_fill, o.shape_fill.then(|| solid_paint(fg)), (o.stroke_width > 0.0).then(|| o.shape_stroke.stroke(o.stroke_width, solid_paint(bg)))),
    };
    let mut width = stroke.as_ref().map_or(0.0, |s| s.width);
    let style = stroke.as_ref().map_or_else(|| o.shape_stroke.clone(), crate::shape_stroke_ui::StrokeOptions::from);
    let key = |k: &str| target.as_ref().map(|s| format!("shape-options-{:?}-{}-{k}", s.doc, s.layer.0));
    let mut edit: Option<Value> = None;
    lbl(ui, tl!("Fill:"));
    if crate::widgets::checkbox(ui, &mut has_fill, "").changed() {
        o.shape_fill = has_fill;
        edit = Some(json!({"fill": if has_fill { json!(fg) } else { Value::Null }}));
    }
    let fill_color = swatch(ui, fill.as_ref(), fg, tl!("Fill color"));
    lbl(ui, tl!("Stroke:"));
    let stroke_color = swatch(ui, stroke.as_ref().map(|s| &s.paint), bg, tl!("Stroke color"));
    if crate::widgets::value_field(ui, &mut width, 0.0..=288.0, "px", 58.0).changed() {
        o.stroke_width = width;
        let mut p = json!({"width": width});
        if stroke.is_none() {
            p["color"] = json!(bg);
        }
        edit = Some(if width <= 0.0 { json!({"stroke": null}) } else { json!({"stroke": p, "coalesce": key("stroke-w")}) });
    }
    let preview = target.as_ref().filter(|s| s.stroke.is_some()).map(|s| (s.doc, s.layer));
    if let Some(change) =
        crate::shape_stroke_ui::button(ui, "tool-stroke", &style, &mut app.ui.stroke_editor, true, tool != Tool::EllipseShape, preview, stroke.is_some())
    {
        let mut p = change.options.params();
        if stroke.is_none() {
            p["width"] = json!(3.0);
            p["color"] = json!(bg);
        }
        edit = Some(json!({"stroke": p, "coalesce": change.coalesce}));
        o.shape_stroke = change.options;
        o.stroke_width = enabled_stroke_width(o.stroke_width);
    }
    match tool {
        Tool::Rectangle => {
            crate::widgets::vline(ui, 22.0);
            lbl(ui, tl!("Radius:"));
            let mut r = match target.as_ref().and_then(|s| s.live.as_ref()) {
                Some(photosplice_doc::LiveShape::Rect { radii, .. }) => radii[0] as f32,
                _ => o.corner_radius,
            };
            if crate::widgets::value_field(ui, &mut r, 0.0..=10000.0, "px", 62.0).changed() {
                o.corner_radius = r;
                let rf = r as f64;
                edit = Some(json!({"radii": [rf, rf, rf, rf], "coalesce": key("radius")}));
            }
        }
        Tool::Polygon => {
            crate::widgets::vline(ui, 22.0);
            lbl(ui, tl!("Sides:"));
            let mut s = o.polygon_sides as f32;
            if crate::widgets::value_field(ui, &mut s, 3.0..=100.0, "", 48.0).changed() {
                o.polygon_sides = s.round().clamp(3.0, 100.0) as u32;
            }
        }
        Tool::Line => {
            crate::widgets::vline(ui, 22.0);
            lbl(ui, tl!("Weight:"));
            crate::widgets::value_field(ui, &mut o.line_weight, 1.0..=1000.0, "px", 58.0);
        }
        Tool::CustomShape => {
            crate::widgets::vline(ui, 22.0);
            crate::preset_panels::shape_picker(app, ui);
        }
        _ if matches!(tool, Tool::PathSelection | Tool::DirectSelection) => {
            if let Some(photosplice_doc::LiveShape::Rect { radii, .. }) = target.as_ref().and_then(|s| s.live.as_ref()) {
                crate::widgets::vline(ui, 22.0);
                lbl(ui, tl!("Radius:"));
                let mut r = radii[0] as f32;
                if crate::widgets::value_field(ui, &mut r, 0.0..=10000.0, "px", 62.0).changed() {
                    o.corner_radius = r;
                    let rf = r as f64;
                    edit = Some(json!({"radii": [rf, rf, rf, rf], "coalesce": key("radius")}));
                }
            }
        }
        _ => {}
    }
    for (is_stroke, change) in [(false, fill_color), (true, stroke_color)] {
        let Some(change) = change else { continue };
        if let Err(e) = set_tool_paint(app, is_stroke, change) {
            app.ui.status = e;
            app.ui.status_error = true;
        }
        edit = Some(match (is_stroke, change) {
            (false, PaintChange::None) => json!({"fill": null}),
            (false, PaintChange::Color(color)) => json!({"fill": color, "coalesce": key("fill")}),
            (true, PaintChange::None) => json!({"stroke": null}),
            (true, PaintChange::Color(color)) => json!({"stroke": {"color": color, "width": enabled_stroke_width(width)}, "coalesce": key("stroke")}),
        });
    }
    if let (Some(mut p), Some(s)) = (edit, &target) {
        p["layer"] = json!(s.layer.0);
        if let Err(e) = app.run("shape.edit", p) {
            app.ui.status = e;
            app.ui.status_error = true;
        }
    }
    true
}

/// The shape layer the options bar edits.
struct SelectedShape {
    doc: photosplice_doc::DocId,
    layer: photosplice_doc::LayerId,
    fill: Option<photosplice_doc::Fill>,
    stroke: Option<photosplice_doc::ShapeStroke>,
    live: Option<photosplice_doc::LiveShape>,
}

fn selected_shape(session: &photosplice_engine::Session) -> Option<SelectedShape> {
    let st = session.active()?;
    let layer = st.active_layer?;
    match &st.doc.layer(layer)?.content {
        LayerContent::Shape(sh) => Some(SelectedShape { doc: st.doc.id, layer, fill: sh.fill.clone(), stroke: sh.stroke.clone(), live: sh.live.clone() }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// Properties (shape layers)

fn solid_paint(c: [f32; 4]) -> photosplice_doc::Fill {
    photosplice_doc::Fill::Solid(photosplice_doc::Color::rgba(c[0], c[1], c[2], c[3]))
}

fn color_of(f: &photosplice_doc::Fill) -> Option<[f32; 4]> {
    match f {
        photosplice_doc::Fill::Solid(c) => {
            let [r, g, b] = c.to_rgb();
            Some([r, g, b, c.alpha])
        }
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum PaintChange {
    Color([f32; 4]),
    None,
}

fn enabled_stroke_width(width: f32) -> f32 {
    if width.is_finite() && width > 0.0 { width } else { 3.0 }
}

/// Tool-bar paint changes affect the next shape; Properties edits the selected shape instead.
fn set_tool_paint(app: &mut PhotospliceApp, stroke: bool, change: PaintChange) -> Result<(), String> {
    if let PaintChange::Color(color) = change {
        app.run("tools.setColors", if stroke { json!({"background": color}) } else { json!({"foreground": color}) })?;
    }
    if stroke {
        app.ui.tool_options.stroke_width = if change == PaintChange::None { 0.0 } else { enabled_stroke_width(app.ui.tool_options.stroke_width) };
    } else {
        app.ui.tool_options.shape_fill = change != PaintChange::None;
    }
    Ok(())
}

fn rgba32(c: [f32; 4]) -> Color32 {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgba_unmultiplied(b(c[0]), b(c[1]), b(c[2]), b(c[3]))
}

fn picker_color(c: [f32; 4]) -> egui::ecolor::Hsva {
    let linear = egui::ecolor::linear_from_gamma;
    let display = c.map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 });
    egui::ecolor::Hsva::from_rgba_unmultiplied(linear(display[0]), linear(display[1]), linear(display[2]), display[3])
}

fn picker_rgba(hsva: egui::ecolor::Hsva) -> [f32; 4] {
    let [r, g, b, a] = hsva.to_rgba_unmultiplied();
    let gamma = egui::ecolor::gamma_from_linear;
    [gamma(r), gamma(g), gamma(b), a]
}

/// The shared compact picker keeps the float RGB paint and its alpha. Color32 is only display
/// data: opening the picker must never quantize an existing high-depth colour or enable paint.
fn swatch(ui: &mut egui::Ui, fill: Option<&photosplice_doc::Fill>, fallback: [f32; 4], tip: &str) -> Option<PaintChange> {
    let t = Tokens::get(ui.ctx());
    let (r, resp) = ui.allocate_exact_size(vec2(30.0, 22.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ColorButton, ui.is_enabled(), tip));
    let current = fill.and_then(color_of);
    crate::widgets::checker(ui.painter(), r.shrink(1.0), 4.0);
    match (current, fill) {
        (Some(c), _) => {
            ui.painter().rect_filled(r.shrink(1.0), t.radius_sm, rgba32(c));
        }
        (None, Some(photosplice_doc::Fill::Gradient { stops, .. })) if !stops.is_empty() => {
            // Gradient fills preview as a left-to-right ramp through their stops.
            let rgb = |c: &photosplice_doc::Color| {
                let v = c.to_rgba8();
                Color32::from_rgba_unmultiplied(v[0], v[1], v[2], v[3])
            };
            let mut mesh = egui::Mesh::default();
            let mut ramp: Vec<(f32, Color32)> = stops.iter().map(|(p, c)| (p.clamp(0.0, 1.0), rgb(c))).collect();
            ramp.insert(0, (0.0, ramp[0].1));
            ramp.push((1.0, ramp[ramp.len() - 1].1));
            for (i, (p, c)) in ramp.iter().enumerate() {
                let x = r.left() + r.width() * p;
                mesh.colored_vertex(egui::pos2(x, r.top()), *c);
                mesh.colored_vertex(egui::pos2(x, r.bottom()), *c);
                if i > 0 {
                    let k = (i as u32) * 2;
                    mesh.add_triangle(k - 2, k - 1, k);
                    mesh.add_triangle(k - 1, k, k + 1);
                }
            }
            ui.painter().add(mesh);
        }
        (None, Some(_)) => {}
        (None, None) => {
            ui.painter().rect_filled(r.shrink(1.0), t.radius_sm, t.field);
            ui.painter().line_segment([r.shrink(3.0).left_bottom(), r.shrink(3.0).right_top()], Stroke::new(1.5, t.danger));
        }
    };
    let border = if resp.has_focus() {
        t.accent
    } else if resp.hovered() {
        t.text_dim
    } else {
        t.field_border
    };
    ui.painter().rect_stroke(r, t.radius_sm, Stroke::new(1.0, border), egui::StrokeKind::Inside);
    let resp = resp.on_hover_text(tip);
    let screen_id = resp.id.with("screen-color");
    let original = current.unwrap_or(fallback);
    let sampled = |rgb: [f32; 3]| PaintChange::Color([rgb[0], rgb[1], rgb[2], original[3]]);
    let mut out = crate::screen_picker::take(ui.ctx(), screen_id).map(sampled);
    let cache = resp.id.with("shape-paint-hsva");
    let mut hsva = ui
        .data(|d| d.get_temp::<([f32; 4], egui::ecolor::Hsva)>(cache))
        .filter(|(previous, _)| *previous == original)
        .map_or_else(|| picker_color(original), |(_, hsva)| hsva);
    if let Some(PaintChange::Color(color)) = out {
        hsva = picker_color(color);
    }
    crate::widgets::swatch_popup(&resp).show(|ui| {
        ui.horizontal(|ui| {
            if ui.selectable_label(fill.is_none(), tl!("No Color")).clicked() && fill.is_some() {
                out = Some(PaintChange::None);
            }
            if ui.selectable_label(current.is_some(), tl!("Solid")).clicked() && current.is_none() {
                out = Some(PaintChange::Color(original));
            }
        });
        ui.separator();
        let before = picker_rgba(hsva);
        if egui::color_picker::color_picker_hsva_2d(ui, &mut hsva, egui::color_picker::Alpha::OnlyBlend) {
            let color = picker_rgba(hsva);
            // egui's HSV gamma/linear round trip can move a float by an ulp without input.
            if color.iter().zip(before).any(|(a, b)| (a - b).abs() > 0.000001) {
                out = Some(PaintChange::Color(color));
            }
        }
        if let Some(rgb) = crate::screen_picker::button(ui, screen_id) {
            out = Some(sampled(rgb));
            hsva = picker_color([rgb[0], rgb[1], rgb[2], original[3]]);
        }
    });
    let cached_color = match out {
        Some(PaintChange::Color(color)) => color,
        _ => original,
    };
    ui.data_mut(|d| d.insert_temp(cache, (cached_color, hsva)));
    out
}

/// One corner of the live rectangle, preserving the other three radii.
/// The engine takes the full [top-left, top-right, bottom-right, bottom-left] array.
fn corner_radii_patch(radii: [f64; 4], corner: usize, radius: f32) -> Value {
    let mut next = radii;
    if let Some(r) = next.get_mut(corner) {
        *r = f64::from(radius.max(0.0));
    }
    json!({"radii": next})
}

/// Follow the existing brush/mask continuous-edit convention: one history state per press or
/// keyboard edit, with every value tick of a held-pointer gesture sharing its key.
fn paint_gesture(ui: &egui::Ui, key: egui::Id) -> u64 {
    let pass = ui.ctx().cumulative_pass_nr();
    let pressed = ui.input(|i| i.pointer.any_pressed());
    ui.data_mut(|d| {
        let gesture = d.get_temp_mut_or_default::<(u64, u64)>(key);
        if pressed && gesture.1 != pass {
            gesture.0 = gesture.0.wrapping_add(1);
            gesture.1 = pass;
        }
        gesture.0
    })
}

/// Properties panel for a shape layer: Appearance (fill, stroke) and live shape geometry.
pub fn shape_properties(app: &mut PhotospliceApp, ui: &mut egui::Ui, id: photosplice_doc::LayerId) {
    if app.session.active().is_some_and(|st| {
        let locks = st.doc.effective_locks(id);
        locks.all || locks.pixels
    }) {
        ui.disable();
    }
    let Some(sh) = app.session.active().and_then(|s| s.doc.layer(id)).and_then(|l| match &l.content {
        LayerContent::Shape(sh) => Some(sh.clone()),
        _ => None,
    }) else {
        return;
    };
    let t = Tokens::get(ui.ctx());
    let mut edit: Option<Value> = None;
    let doc_id = app.session.active().map(|st| st.doc.id);
    let key = |k: &str| format!("shape-{doc_id:?}-{}-{k}", id.0);
    let gesture_key = ui.id().with(key("paint-gesture"));
    let gesture = paint_gesture(ui, gesture_key);
    let coalesce = |field: &str| format!("{}:{}:{gesture}", key(field), gesture_key.value());
    // The shared collapsible section headers (#155).
    // Prefs "Appearance" is theme chrome (Giao diện); shape fill/stroke is Diện mạo.
    if crate::props_layout::section(ui, "appearance", crate::i18n::tr_ctx(crate::i18n::current(), "shape", "Appearance")) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tl!("Fill")).color(t.text_dim).size(12.0));
            if let Some(change) = swatch(ui, sh.fill.as_ref(), app.session.tools.foreground, tl!("Set shape fill type")) {
                let fill = match change {
                    PaintChange::None => Value::Null,
                    PaintChange::Color(color) => json!(color),
                };
                edit = Some(json!({"fill": fill, "coalesce": coalesce("fill")}));
            }
            ui.add_space(12.0);
            ui.label(egui::RichText::new(tl!("Stroke")).color(t.text_dim).size(12.0));
            if let Some(change) = swatch(ui, sh.stroke.as_ref().map(|s| &s.paint), app.session.tools.foreground, tl!("Set shape stroke type")) {
                let stroke = match change {
                    PaintChange::None => Value::Null,
                    PaintChange::Color(color) => json!({"color": color, "width": enabled_stroke_width(sh.stroke.as_ref().map_or(0.0, |s| s.width))}),
                };
                edit = Some(json!({"stroke": stroke, "coalesce": coalesce("stroke")}));
            }
            let mut w = sh.stroke.as_ref().map_or(0.0, |s| s.width);
            if crate::widgets::value_field(ui, &mut w, 0.0..=288.0, "px", 60.0).changed() {
                let stroke = if w <= 0.0 { Value::Null } else { json!({"width": w}) };
                edit = Some(json!({"stroke": stroke, "coalesce": coalesce("stroke-w")}));
            }
        });
        let style = sh.stroke.as_ref().map(crate::shape_stroke_ui::StrokeOptions::from).unwrap_or_default();
        let stroke_enabled = sh.stroke.as_ref().is_some_and(|stroke| stroke.width > 0.0);
        if let Some(change) = crate::shape_stroke_ui::button(
            ui,
            &key("style"),
            &style,
            &mut app.ui.stroke_editor,
            sh.path.subpaths.iter().all(|s| s.closed),
            !matches!(sh.live, Some(photosplice_doc::LiveShape::Ellipse { .. })),
            app.session.active().map(|st| (st.doc.id, id)),
            stroke_enabled,
        ) {
            let mut stroke = change.options.params();
            if !stroke_enabled {
                stroke["width"] = json!(3.0);
            }
            edit = Some(json!({"stroke": stroke, "coalesce": change.coalesce}));
        }
    }
    if let Some(live) = &sh.live
        && crate::props_layout::section(ui, "liveShape", tl!("Shape"))
    {
        let num = |ui: &mut egui::Ui, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, unit: &str| -> bool {
            ui.label(egui::RichText::new(tl!(&label)).color(t.text_dim).size(12.0));
            crate::widgets::value_field(ui, v, range, unit, 64.0).changed()
        };
        match live {
            photosplice_doc::vector::LiveShape::Rect { rect, .. }
            | photosplice_doc::vector::LiveShape::Ellipse { rect }
            | photosplice_doc::vector::LiveShape::Polygon { rect, .. } => {
                ui.horizontal(|ui| {
                    let (mut w, mut h) = (rect[2] as f32, rect[3] as f32);
                    let cw = num(ui, "W", &mut w, 1.0..=300000.0, "px");
                    let ch = num(ui, "H", &mut h, 1.0..=300000.0, "px");
                    if cw || ch {
                        edit = Some(json!({"rect": [rect[0], rect[1], w.round(), h.round()], "coalesce": key("wh")}));
                    }
                });
            }
            photosplice_doc::vector::LiveShape::Line { .. } => {}
        }
        ui.vertical(|ui| match live {
            photosplice_doc::vector::LiveShape::Rect { radii, .. } => {
                // Keep the uniform workflow available after independent corner edits too.
                ui.horizontal(|ui| {
                    let mut all = radii[0] as f32;
                    if num(ui, tl!("Corner radius"), &mut all, 0.0..=100000.0, "px") {
                        edit = Some(json!({"radii": all, "coalesce": key("radius-all")}));
                    }
                });
                // Separate rows also fit the narrow Properties dock. The labels use the
                // localized corner names, rather than untranslated abbreviations.
                for (index, label) in [(0, tl!("Top Left")), (1, tl!("Top Right")), (3, tl!("Bottom Left")), (2, tl!("Bottom Right"))] {
                    ui.horizontal(|ui| {
                        let mut radius = radii[index] as f32;
                        if num(ui, label, &mut radius, 0.0..=100000.0, "px") {
                            let mut patch = corner_radii_patch(*radii, index, radius);
                            patch["coalesce"] = json!(key(&format!("radius-{index}")));
                            edit = Some(patch);
                        }
                    });
                }
            }
            photosplice_doc::vector::LiveShape::Polygon { sides, star_ratio, .. } => {
                ui.horizontal(|ui| {
                    let mut n = *sides as f32;
                    if num(ui, tl!("Sides"), &mut n, 3.0..=100.0, "") {
                        edit = Some(json!({"sides": n.round() as u32, "coalesce": key("sides")}));
                    }
                    let mut sr = (*star_ratio * 100.0) as f32;
                    if num(ui, tl!("Star ratio"), &mut sr, 1.0..=100.0, "%") {
                        edit = Some(json!({"starRatio": sr as f64 / 100.0, "coalesce": key("star")}));
                    }
                })
                .inner
            }
            photosplice_doc::vector::LiveShape::Line { weight, .. } => {
                ui.horizontal(|ui| {
                    let mut w = *weight as f32;
                    if num(ui, tl!("Weight"), &mut w, 1.0..=10000.0, "px") {
                        edit = Some(json!({"weight": w, "coalesce": key("weight")}));
                    }
                })
                .inner
            }
            _ => {}
        });
    }
    if let Some(mut p) = edit {
        p["layer"] = json!(id.0);
        if let Err(e) = app.run("shape.edit", p) {
            app.ui.status = e;
            app.ui.status_error = true;
        }
        if !ui.input(|i| i.pointer.any_down()) {
            ui.data_mut(|d| {
                let gesture = d.get_temp_mut_or_default::<(u64, u64)>(gesture_key);
                gesture.0 = gesture.0.wrapping_add(1);
            });
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Paths panel

fn thumb(ui: &egui::Ui, r: Rect, path: &Path, doc: &Document) {
    let t = Tokens::get(ui.ctx());
    // At the document's aspect ratio, like the Channels thumbnails.
    let r = crate::channels_panel::fit_thumb(r, doc.size.width, doc.size.height).0;
    ui.painter().rect_filled(r, 0.0, Color32::from_gray(if t.pro { 222 } else { 240 }));
    let (w, h) = (doc.size.width.max(1) as f64, doc.size.height.max(1) as f64);
    let s = (r.width() as f64 / w).min(r.height() as f64 / h);
    let off = r.center() - vec2((w * s) as f32 / 2.0, (h * s) as f32 / 2.0);
    let map = |q: [f64; 2]| pos2(off.x + (q[0] * s) as f32, off.y + (q[1] * s) as f32);
    for (pts, closed) in path_lines(path, &map) {
        if closed && pts.len() >= 3 {
            ui.painter().add(egui::Shape::closed_line(pts, Stroke::new(1.0, Color32::from_gray(40))));
        } else {
            ui.painter().add(egui::Shape::line(pts, Stroke::new(1.0, Color32::from_gray(40))));
        }
    }
}

/// What a Paths panel row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathRow {
    Saved,
    Work,
    /// The selected layer's shape path or vector mask (temporary, italic).
    Layer,
}

/// A Paths panel row.
#[derive(Clone, Debug)]
pub struct PathEntry {
    pub name: String,
    pub path: Path,
    pub kind: PathRow,
}

/// The Paths panel's rows, top to bottom: saved paths, the work path, then the selected layer's
/// shape path ("<Layer> Shape Path") or vector mask ("<Layer> Vector Mask"), like Photoshop.
pub fn path_rows(doc: &Document, active: Option<photosplice_doc::LayerId>) -> Vec<PathEntry> {
    let mut rows: Vec<_> = doc.paths.iter().map(|p| PathEntry { name: p.name.clone(), path: p.path.clone(), kind: PathRow::Saved }).collect();
    if let Some(wp) = &doc.work_path {
        rows.push(PathEntry { name: "Work Path".into(), path: wp.clone(), kind: PathRow::Work });
    }
    if let Some(l) = active.and_then(|id| doc.layer(id)) {
        let layer_path = match &l.content {
            LayerContent::Shape(sh) => Some((format!("{} Shape Path", l.name), sh.path.clone())),
            _ => l.vector_mask.as_ref().map(|v| (format!("{} Vector Mask", l.name), v.path.clone())),
        };
        if let Some((name, path)) = layer_path {
            rows.push(PathEntry { name, path, kind: PathRow::Layer });
        }
    }
    rows
}

fn footer_id() -> egui::Id {
    egui::Id::new("paths-footer")
}

fn ctx_data_footer(ctx: &egui::Context, r: Rect) {
    ctx.data_mut(|d| d.insert_temp(footer_id(), r));
}

/// Where the Paths panel's footer buttons were drawn last frame (the rect around them all).
pub fn paths_footer(ctx: &egui::Context) -> Option<Rect> {
    ctx.data(|d| d.get_temp(footer_id()))
}

/// Commands offered by a Paths row's context menu. The row's path is passed explicitly so a
/// right-click acts on that row even when another path is selected in the panel.
fn path_context_actions(entry: &PathEntry, doc: &Document) -> Vec<(&'static str, &'static str, Value)> {
    let key = match entry.kind {
        PathRow::Work => "work".to_string(),
        PathRow::Layer => "layer".to_string(),
        PathRow::Saved => entry.name.clone(),
    };
    let mut actions = vec![
        ("Make Selection", "path.toSelection", json!({"name": key})),
        ("Fill Path", "path.fill", json!({"name": key})),
        ("Stroke Path…", "path.stroke", json!({"name": key})),
    ];
    if entry.kind == PathRow::Work {
        let mut n = doc.paths.len().saturating_add(1);
        while doc.paths.iter().any(|p| p.name == format!("Path {n}")) {
            n = n.saturating_add(1);
            if n == usize::MAX {
                break;
            }
        }
        actions.push(("Save Path", "path.rename", json!({"name": "work", "to": format!("Path {n}")})));
    } else {
        let mut n = 1usize;
        while doc.paths.iter().any(|p| p.name == format!("{} copy {n}", entry.name)) {
            n = n.saturating_add(1);
            if n == usize::MAX {
                break;
            }
        }
        let copy_name = format!("{} copy {n}", entry.name);
        actions.push(("Duplicate Path", "path.set", json!({"name": copy_name, "path": photosplice_engine::vector_cmds::path_json(&entry.path)})));
    }
    if entry.kind != PathRow::Layer {
        actions.push(("Delete Path", "path.delete", json!({"name": key})));
    } else if entry.name.ends_with(" Vector Mask") {
        actions.push(("Delete Vector Mask", "layer.vectorMask.delete", json!({})));
    }
    actions
}

pub fn paths_panel(app: &mut PhotospliceApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let Some(st) = app.session.active() else {
        ui.label(egui::RichText::new(tl!("No document")).color(t.text_faint));
        return;
    };
    let doc = st.doc.clone();
    let rows = path_rows(&doc, st.active_layer);
    let has_layer_path = rows.iter().any(|r| r.kind == PathRow::Layer);
    let mut action: Option<(&str, Value)> = None;
    let mut stroke_dialog: Option<String> = None;
    // The buttons sit in a footer at the panel's bottom, like Photoshop's.
    let footer = crate::widgets::footer_height(ui) + ui.spacing().item_spacing.y;
    let fill = ui.available_height() > footer + 60.0;
    let rows_h = if fill { ui.available_height() - footer } else { f32::INFINITY };
    egui::ScrollArea::vertical().id_salt("path-rows").max_height(rows_h).min_scrolled_height(if fill { rows_h } else { 0.0 }).auto_shrink([false, !fill]).show(
        ui,
        |ui| {
            if rows.is_empty() {
                ui.label(egui::RichText::new(tl!("Draw with the Pen tool (P) or make a work path from a selection.")).color(t.text_faint).size(11.5));
            }
            for PathEntry { name, path, kind } in &rows {
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
                let key = match kind {
                    PathRow::Work => "work".to_string(),
                    PathRow::Layer => "layer".to_string(),
                    PathRow::Saved => name.clone(),
                };
                // Rows are painted: name them for screen readers and UI tests.
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::SelectableLabel, true, name));
                let sel = app.ui.selected_path.as_deref() == Some(key.as_str());
                if sel {
                    ui.painter().rect_filled(r, 0.0, t.row_selected);
                } else if resp.hovered() {
                    ui.painter().rect_filled(r, 0.0, t.hover.gamma_multiply(0.5));
                }
                let thumbnail = Rect::from_min_size(pos2(r.left() + 8.0, r.top() + 4.0), vec2(28.0, 28.0));
                thumb(ui, thumbnail, path, &doc);
                let mut job = egui::text::LayoutJob::default();
                // Temporary paths (the work path, the selected layer's shape path or vector mask) are italic.
                let italics = *kind != PathRow::Saved;
                job.append(
                    if *kind == PathRow::Work { tl!(name) } else { name },
                    0.0,
                    egui::TextFormat { font_id: egui::FontId::proportional(12.0), color: t.text, italics, ..Default::default() },
                );
                let g = ui.painter().layout_job(job);
                ui.painter().galley(pos2(r.left() + 46.0, r.center().y - g.size().y / 2.0), g, t.text);
                // Ctrl/Cmd-click loads only the thumbnail's path, without changing the panel target.
                let modifiers = ui.input(|i| i.modifiers);
                let load = resp.clicked() && modifiers.command && resp.interact_pointer_pos().is_some_and(|p| thumbnail.contains(p));
                if load {
                    let mode = match crate::channels_panel::load_operation(modifiers) {
                        "new" => "replace",
                        mode => mode,
                    };
                    action = Some(("path.toSelection", json!({"name": key, "mode": mode})));
                } else if resp.clicked() {
                    app.ui.selected_path = Some(key.clone());
                }
                // Double-clicking the Work Path saves it (Photoshop's "Save Path").
                if !load && resp.double_clicked() && *kind == PathRow::Work {
                    let n = doc.paths.len() + 1;
                    action = Some(("path.rename", json!({"name": "work", "to": format!("Path {n}")})));
                }
                resp.context_menu(|ui| {
                    crate::widgets::menu_scroll(ui, |ui| {
                        ui.set_min_width(190.0);
                        let entry = PathEntry { name: name.clone(), path: path.clone(), kind: *kind };
                        let can_paint = app.session.active().and_then(|s| s.active_layer.and_then(|id| s.doc.layer(id))).is_some_and(|l| l.surface().is_some());
                        for (label, cmd, params) in path_context_actions(&entry, &doc) {
                            let enabled = !matches!(cmd, "path.fill" | "path.stroke") || can_paint;
                            if ui.add_enabled(enabled, egui::Button::new(tl!(label))).clicked() {
                                if cmd == "path.stroke" {
                                    // Stroke Path… asks for the tool first, as in Photoshop (#2978).
                                    stroke_dialog = Some(params.get("name").and_then(Value::as_str).unwrap_or("work").to_string());
                                } else {
                                    action = Some((cmd, params));
                                }
                                ui.close();
                            }
                        }
                    });
                });
                ui.painter().line_segment([r.left_bottom(), r.right_bottom()], Stroke::new(1.0, t.separator));
            }
            // Clicking the empty area deselects the path, as in Photoshop: new fill and
            // adjustment layers then take the selection as their mask instead (#1419).
            let rest = ui.available_rect_before_wrap();
            let rest = rest.intersect(Rect::from_min_size(rest.min, vec2(rest.width(), rest.height().min(4000.0))));
            if rest.is_positive() && ui.interact(rest, ui.id().with("paths-empty"), Sense::click()).clicked() {
                app.ui.selected_path = None;
            }
        },
    );
    let sel = app.ui.selected_path.clone().filter(|s| s != "layer" || has_layer_path).unwrap_or_else(|| "work".into());
    let buttons = crate::widgets::panel_footer(ui, |ui| {
        let fg = hex(app.session.tools.foreground);
        let mut n = doc.paths.len() + 1;
        while doc.paths.iter().any(|p| p.name == format!("Path {n}")) {
            n += 1;
        }
        let items: [(&str, &str, &str, Value); 7] = [
            ("paint-bucket", "Fill path with foreground color", "path.fill", json!({"name": sel, "color": fg})),
            ("circle", "Stroke path with brush", "path.stroke", json!({"name": sel, "tool": "brush"})),
            ("lrs-makesel", "Load path as a selection", "path.toSelection", json!({"name": sel})),
            ("lrs-makepath", "Make work path from selection", "select.toWorkPath", json!({"tolerance": 2.0})),
            ("lrs-mask", "Add vector mask", "layer.vectorMask.fromPath", json!({"name": sel})),
            ("lrs-newlayer", "Create new path", "path.set", json!({"name": format!("Path {n}"), "path": {"subpaths": []}})),
            ("lrs-bin", "Delete path", "path.delete", json!({"name": sel})),
        ];
        // The layer's own path: deleting it deletes the vector mask (a shape keeps its path).
        let shape = rows.iter().any(|r| r.kind == PathRow::Layer && r.name.ends_with(" Shape Path"));
        let delete_layer_path = sel == "layer" && !shape;
        let items = items.map(|(icon, tip, cmd, p)| {
            if cmd == "path.delete" && delete_layer_path { (icon, "Delete vector mask", "layer.vectorMask.delete", json!({})) } else { (icon, tip, cmd, p) }
        });
        // Laid out from the right, so the last button goes first.
        let mut buttons = Rect::NOTHING;
        for (icon, tip, cmd, p) in items.into_iter().rev() {
            let icon = if crate::icons::exists(icon) { icon } else { "square" };
            let b = crate::icons::button(ui, icon, 26.0, false, tip);
            if b.clicked() {
                if cmd == "path.stroke" && ui.input(|i| i.modifiers.alt) {
                    // Alt-click opens the Stroke Path dialog, as in Photoshop (#2978).
                    stroke_dialog = Some(sel.clone());
                } else {
                    action = Some((cmd, p));
                }
            }
            buttons = buttons.union(b.rect);
        }
        buttons
    });
    ctx_data_footer(ui.ctx(), buttons);
    if let Some(name) = stroke_dialog {
        crate::canvas_tool_menu::stroke_path_dialog(app, &name);
    }
    if let Some((cmd, p)) = action
        && let Err(e) = app.run(cmd, p)
    {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #4084: zoomed in, a curve's chords stay within a fraction of a pixel of the true curve
    /// (16 fixed chords showed corners), and a straight segment stays one chord.
    #[test]
    fn path_curves_stay_smooth_at_any_zoom() {
        let (p0, c0, c1, p1) = ([0.0, 100.0], [20.0, -60.0], [80.0, -60.0], [100.0, 100.0]);
        for zoom in [0.25, 1.0, 8.0, 64.0, 320.0] {
            let xf = move |q: [f64; 2]| pos2((q[0] * zoom) as f32 + 10.0, (q[1] * zoom) as f32 - 5.0);
            let pts = curve_points(p0, c0, c1, p1, &xf);
            let n = pts.len() - 1;
            assert_eq!((pts[0], pts[n]), (xf(p0), xf(p1)), "the ends are the anchors");
            for (k, w) in pts.windows(2).enumerate() {
                let curve = xf(bezier(p0, c0, c1, p1, (k as f64 + 0.5) / n as f64));
                let chord = w[0] + (w[1] - w[0]) * 0.5;
                assert!((chord - curve).length() <= 0.35, "zoom {zoom}: chord {k} of {n} is {} px off", (chord - curve).length());
            }
            // Zoomed out it stays cheap.
            if zoom <= 1.0 {
                assert!(n <= 32, "zoom {zoom}: {n} chords");
            }
        }
        let xf = |q: [f64; 2]| pos2((q[0] * 320.0) as f32, (q[1] * 320.0) as f32);
        // Retracted handles, and handles along the line: one chord.
        assert_eq!(curve_points([0.0, 0.0], [0.0, 0.0], [50.0, 50.0], [50.0, 50.0], &xf).len(), 2);
        assert_eq!(curve_points([0.0, 0.0], [10.0, 10.0], [40.0, 40.0], [50.0, 50.0], &xf).len(), 2);
        // A handle past the end is a curve that doubles back, not a line.
        assert!(curve_points([0.0, 0.0], [0.0, 0.0], [80.0, 80.0], [50.0, 50.0], &xf).len() > 2);
        // Hostile coordinates: no panic, a bounded number of points.
        for bad in [f64::NAN, f64::INFINITY, 1e300] {
            assert!(curve_points([0.0, 0.0], [bad, 0.0], [0.0, bad], [1.0, 1.0], &xf).len() <= MAX_CURVE_PIECES + 1);
        }
        // Through path_lines too: a closed subpath of two curved segments.
        let mut path = Path::default();
        path.subpaths.push(photosplice_doc::vector::Subpath {
            knots: vec![
                photosplice_doc::vector::Knot {
                    anchor: photosplice_geom::Point::new(0.0, 0.0),
                    in_ctrl: photosplice_geom::Point::new(0.0, 40.0),
                    out_ctrl: photosplice_geom::Point::new(0.0, -40.0),
                    smooth: true,
                },
                photosplice_doc::vector::Knot {
                    anchor: photosplice_geom::Point::new(60.0, 0.0),
                    in_ctrl: photosplice_geom::Point::new(60.0, -40.0),
                    out_ctrl: photosplice_geom::Point::new(60.0, 40.0),
                    smooth: true,
                },
            ],
            closed: true,
            ..Default::default()
        });
        let lines = path_lines(&path, &xf);
        assert!(lines[0].1 && lines[0].0.len() > 2 * 17, "zoomed in, more than the old 16 chords per segment: {}", lines[0].0.len());
    }

    #[test]
    fn all_shape_tools_and_open_pen_use_the_complete_stroke_defaults() {
        for tool in [Tool::Rectangle, Tool::EllipseShape, Tool::Triangle, Tool::Polygon, Tool::Line, Tool::CustomShape] {
            let mut a = app();
            a.ui.tool_options.stroke_width = 5.0;
            a.ui.tool_options.shape_stroke = crate::shape_stroke_ui::StrokeOptions {
                cap: photosplice_doc::LineCap::Square,
                join: photosplice_doc::LineJoin::Bevel,
                dashes: vec![4.0, 2.0, 1.0, 3.0],
                dash_offset: -0.5,
                opacity: 0.4,
                ..Default::default()
            };
            let history = a.session.active().unwrap().history.past_len();
            finish_shape(&mut a, tool, [20.0, 20.0], [120.0, 100.0], Default::default());
            let info = a.run("shape.info", json!({})).unwrap();
            assert_eq!(info["stroke"]["dashes"], json!([4.0, 2.0, 1.0, 3.0]));
            assert_eq!(info["stroke"]["cap"], "square");
            assert_eq!(info["stroke"]["width"], 5.0);
            assert_eq!(a.session.active().unwrap().history.past_len(), history + 1);
            a.run("edit.undo", json!({})).unwrap();
            a.run("edit.redo", json!({})).unwrap();
            assert_eq!(a.run("shape.info", json!({})).unwrap(), info);
        }
    }

    fn app() -> PhotospliceApp {
        let mut app = PhotospliceApp::new(photosplice_engine::Session::new(), crate::Services::default());
        app.session.execute("file.new", json!({"width": 200, "height": 200})).unwrap();
        app.sync_views();
        app
    }

    #[test]
    fn paths_context_menu_uses_the_clicked_row_and_dispatches_commands() {
        let mut app = app();
        let path = json!({"subpaths": [{"closed": true, "knots": [
            {"anchor": [10, 10], "in": [10, 10], "out": [10, 10]},
            {"anchor": [80, 10], "in": [80, 10], "out": [80, 10]},
            {"anchor": [80, 80], "in": [80, 80], "out": [80, 80]}
        ]}]});
        app.run("path.set", json!({"name": "First", "path": path})).unwrap();
        app.run("path.set", json!({"name": "Second", "path": path})).unwrap();
        let doc = &app.session.active().unwrap().doc;
        let first = path_rows(doc, None).into_iter().find(|r| r.name == "First").unwrap();
        let entries = path_context_actions(&first, doc);
        assert_eq!(entries[0].1, "path.toSelection");
        assert_eq!(entries[0].2["name"], "First");
        assert!(entries.iter().all(|(_, id, _)| photosplice_engine::commands::find(id).is_some()));
        let (_, id, params) = entries.into_iter().find(|(_, id, _)| *id == "path.toSelection").unwrap();
        app.run(id, params).unwrap();
        assert!(app.session.active().unwrap().doc.selection.is_some());
        assert_eq!(app.session.journal.last().map(|(id, _)| id.as_str()), Some("path.toSelection"));

        app.run("path.set", json!({"name": "First copy 1", "path": path})).unwrap();
        let doc = &app.session.active().unwrap().doc;
        let first = path_rows(doc, None).into_iter().find(|r| r.name == "First").unwrap();
        let (_, _, duplicate) = path_context_actions(&first, doc).into_iter().find(|(_, id, _)| *id == "path.set").unwrap();
        assert_eq!(duplicate["name"], "First copy 2", "duplicate must not overwrite an existing path");
    }

    #[test]
    fn work_path_context_save_and_layer_path_deletion_rules() {
        let mut app = app();
        let path = json!({"subpaths": []});
        app.run("path.set", json!({"name": "work", "path": path})).unwrap();
        let doc = &app.session.active().unwrap().doc;
        let work = path_rows(doc, None).into_iter().find(|r| r.kind == PathRow::Work).unwrap();
        let entries = path_context_actions(&work, doc);
        assert!(entries.iter().any(|(_, id, _)| *id == "path.rename"));
        assert!(!entries.iter().any(|(_, id, _)| *id == "path.set"));
        let layer = PathEntry { kind: PathRow::Layer, ..work };
        let entries = path_context_actions(&layer, doc);
        assert!(!entries.iter().any(|(_, id, _)| *id == "path.delete"));
    }

    /// How many shapes the path overlay paints for `app` this frame.
    fn overlay_shapes(app: &PhotospliceApp, ctx: &egui::Context) -> usize {
        let doc = app.session.active().unwrap().doc.clone();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let rect = Rect::from_min_size(Pos2::ZERO, vec2(400.0, 400.0));
            let painter = ui.ctx().layer_painter(egui::LayerId::background()).with_clip_rect(rect);
            let xf = ViewXform { rect, zoom: 1.0, center: [100.0, 100.0], flip: false, rotation: 0.0, aspect: 1.0 };
            draw_overlay(app, &painter, &xf, &doc);
        });
        out.textures_delta.clear();
        out.shapes.len()
    }

    #[test]
    fn show_target_path_and_extras_hide_the_path_outlines() {
        // #1119: View › Show › Target Path only flipped its checkmark; the canvas kept drawing the
        // work path and the active shape's path.
        let ctx = egui::Context::default();
        let mut app = app();
        let path = json!({"subpaths": [{"closed": true, "knots": [
            {"anchor": [10, 10], "in": [10, 10], "out": [10, 10]},
            {"anchor": [80, 10], "in": [80, 10], "out": [80, 10]},
            {"anchor": [80, 80], "in": [80, 80], "out": [80, 80]}
        ]}]});
        app.run("path.set", json!({"path": path})).unwrap();
        assert!(app.session.active().unwrap().doc.work_path.is_some());
        finish_shape(&mut app, Tool::Rectangle, [100.0, 100.0], [160.0, 150.0], egui::Modifiers::NONE);
        let toggle = |app: &mut PhotospliceApp, id: &str| crate::menus::invoke(app, &ctx, id, Value::Null).unwrap();
        for tool in [Tool::Pen, Tool::PathSelection, Tool::DirectSelection, Tool::Rectangle] {
            app.ui.tool = tool;
            assert!(app.ui.view.show.target_path && app.ui.view.extras);
            assert!(overlay_shapes(&app, &ctx) > 0, "{tool:?}: the paths are drawn by default");
            toggle(&mut app, "view.show.targetPath");
            assert!(!app.ui.view.show.target_path);
            assert_eq!(overlay_shapes(&app, &ctx), 0, "{tool:?}: Target Path off");
            toggle(&mut app, "view.show.targetPath");
            toggle(&mut app, "view.extras");
            assert!(!app.ui.view.extras);
            assert_eq!(overlay_shapes(&app, &ctx), 0, "{tool:?}: Extras off");
            toggle(&mut app, "view.extras");
            assert!(overlay_shapes(&app, &ctx) > 0, "{tool:?}: shown again");
        }
        // The Pen's path in progress is a gesture, not the target path: it stays visible.
        app.ui.tool = Tool::Pen;
        for (x, y) in [(20.0, 20.0), (60.0, 40.0)] {
            crate::pen_tool::down(&mut app, x, y, egui::Modifiers::NONE);
            crate::pen_tool::up(&mut app);
        }
        toggle(&mut app, "view.show.targetPath");
        assert!(overlay_shapes(&app, &ctx) > 0, "the pen path in progress is still drawn");
    }

    #[test]
    fn shape_drag_creates_layers_with_modifiers() {
        let mut app = app();
        finish_shape(&mut app, Tool::Rectangle, [10.0, 10.0], [60.0, 30.0], egui::Modifiers::SHIFT);
        let st = app.session.active().unwrap();
        let b = st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap().content_bounds();
        assert_eq!((b.width(), b.height()), (50, 50), "shift = square");
        finish_shape(&mut app, Tool::EllipseShape, [100.0, 100.0], [120.0, 110.0], egui::Modifiers::ALT);
        let st = app.session.active().unwrap();
        let b = st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap().content_bounds();
        assert!(b.x0.abs_diff(80) <= 1 && b.width().abs_diff(40) <= 1, "alt = from centre: {b:?}");
        finish_shape(&mut app, Tool::Line, [10.0, 150.0], [90.0, 152.0], egui::Modifiers::SHIFT);
        assert_eq!(app.session.active().unwrap().doc.layers.len(), 4);
    }

    #[test]
    fn shift_pressed_during_a_shape_drag_constrains_it() {
        // #668: ⇧ only counted when held at the press; Photoshop constrains when it is pressed
        // mid-drag (and stops when it is released).
        use crate::canvas::{ToolEvent, tool_event};
        for (tool, sides) in [(Tool::Polygon, 6), (Tool::Rectangle, 4), (Tool::EllipseShape, 0)] {
            let mut app = app();
            app.ui.tool = tool;
            app.ui.tool_options.polygon_sides = 6;
            let none = egui::Modifiers::NONE;
            tool_event(&mut app, ToolEvent::Down { x: 10.0, y: 10.0, pressure: 1.0 }, none);
            tool_event(&mut app, ToolEvent::Move { x: 40.0, y: 30.0, pressure: 1.0 }, none);
            tool_event(&mut app, ToolEvent::Move { x: 70.0, y: 40.0, pressure: 1.0 }, egui::Modifiers::SHIFT);
            tool_event(&mut app, ToolEvent::Up { x: 70.0, y: 40.0 }, egui::Modifiers::SHIFT);
            let st = app.session.active().unwrap();
            let l = st.doc.layer(st.active_layer.unwrap()).unwrap();
            assert!(matches!(l.content, LayerContent::Shape(_)), "{tool:?} ({sides} sides): a shape layer");
            let b = l.surface().unwrap().content_bounds();
            // The drag is 60 × 30; constrained, the shape's box is 60 × 60 (a hexagon is narrower
            // than its box, so the height tells).
            assert!(b.height().abs_diff(60) <= 1 && b.width() <= 61, "{tool:?}: ⇧ mid-drag keeps proportions: {b:?}");
        }
    }

    #[test]
    fn shape_preview_is_the_committed_path() {
        let mut app = app();
        app.ui.tool_options.corner_radius = 8.0;
        let preview = |app: &PhotospliceApp, tool, end| {
            shape_geometry(&app.ui.tool_options, tool, [10.0, 10.0], end, egui::Modifiers::ALT)
                .and_then(|p| photosplice_engine::vector_cmds::shape_path(&p).ok())
        };
        for tool in [Tool::Rectangle, Tool::EllipseShape, Tool::Triangle, Tool::Polygon, Tool::Line] {
            let shown = preview(&app, tool, [60.0, 30.0]).unwrap();
            finish_shape(&mut app, tool, [10.0, 10.0], [60.0, 30.0], egui::Modifiers::ALT);
            let st = app.session.active().unwrap();
            let LayerContent::Shape(sh) = &st.doc.layer(st.active_layer.unwrap()).unwrap().content else { panic!("{tool:?}: no shape layer") };
            assert_eq!(sh.path, shown, "{tool:?}");
        }
        assert!(preview(&app, Tool::Rectangle, [10.2, 40.0]).is_none(), "too thin to draw");
        assert!(photosplice_engine::vector_cmds::shape_path(&json!({"kind": "nope", "rect": [0, 0, 5, 5]})).is_err());
        assert!(photosplice_engine::vector_cmds::shape_path(&json!({"kind": "rect"})).is_err());
    }

#[test]
    fn new_fill_layer_takes_the_selected_path_as_its_vector_mask() {
        let mut app = app();
        let layer = |app: &PhotospliceApp, r: Result<Value, String>| {
            let id = photosplice_doc::LayerId(r.unwrap()["layer"].as_u64().unwrap());
            app.session.active().unwrap().doc.layer(id).unwrap().clone()
        };
        app.run("path.set", json!({"name": "work", "path": {"subpaths": [{"closed": true, "knots": [[20, 20], [120, 20], [120, 120]]}]}})).unwrap();
        app.ui.selected_path = Some("work".into());
        let wp = app.session.active().unwrap().doc.work_path.clone().unwrap();
        app.run("select.rect", json!({"x": 150, "y": 150, "width": 20, "height": 20})).unwrap();
        for id in ["layer.newFillLayer.solidColor", "layer.newFillLayer.gradient", "layer.newAdjustmentLayer.invert"] {
            app.ui.selected_path = Some("work".into());
            let r = app.run(id, json!({}));
            let l = layer(&app, r);
            assert_eq!(l.vector_mask.as_ref().map(|v| &v.path), Some(&wp), "{id}: the work path is the vector mask");
            assert!(l.mask.is_none(), "{id}: the path wins over the selection");
            // The new layer's vector mask is now the active path, not the work path.
            assert_eq!(app.ui.selected_path.as_deref(), Some("layer"), "{id}");
        }
        // So the next fill layer isn't masked by the work path again.
        let r = app.run("layer.newFillLayer.solidColor", json!({}));
        assert!(layer(&app, r).vector_mask.is_none());
        // A caller can opt out.
        let r = app.run("layer.newFillLayer.solidColor", json!({"path": null}));
        assert!(layer(&app, r).vector_mask.is_none());
        // A selected saved path is used.
        app.run("path.set", json!({"name": "Saved", "path": {"subpaths": [{"closed": true, "knots": [[0, 0], [9, 0], [9, 9]]}]}})).unwrap();
        app.ui.selected_path = Some("Saved".into());
        let saved = app.session.active().unwrap().doc.paths[0].path.clone();
        let r = app.run("layer.newFillLayer.solidColor", json!({}));
        assert_eq!(layer(&app, r).vector_mask.map(|v| v.path), Some(saved));
        // Deselected (or a stale name): the selection is the layer mask, no vector mask.
        for sel in [None, Some("Gone".to_string())] {
            app.ui.selected_path = sel;
            let r = app.run("layer.newFillLayer.solidColor", json!({}));
            let l = layer(&app, r);
            assert!(l.vector_mask.is_none() && l.mask.is_some());
        }
        // Make Work Path selects the new work path.
        app.run("select.toWorkPath", json!({"tolerance": 2.0})).unwrap();
        assert_eq!(app.ui.selected_path.as_deref(), Some("work"));
    }

#[test]
    fn live_rectangle_corner_radius_edit_preserves_other_corners_and_history() {
        let mut app = app();
        let created = app.run("shape.create", json!({"kind": "roundedRect", "rect": [10, 10, 150, 100], "radii": [5, 10, 15, 20]})).unwrap();
        let id = created["layer"].as_u64().unwrap();
        let get_radii = |app: &PhotospliceApp| {
            let st = app.session.active().unwrap();
            let shape = st.doc.layer(photosplice_doc::LayerId(id)).unwrap();
            match &shape.content {
                LayerContent::Shape(sh) => match &sh.live {
                    Some(photosplice_doc::vector::LiveShape::Rect { radii, .. }) => *radii,
                    _ => panic!("rectangle must remain a live shape"),
                },
                _ => panic!("expected shape layer"),
            }
        };
        assert_eq!(get_radii(&app), [5.0, 10.0, 15.0, 20.0]);
        let mut patch = corner_radii_patch(get_radii(&app), 1, 30.0);
        patch["layer"] = json!(id);
        app.run("shape.edit", patch).unwrap();
        assert_eq!(get_radii(&app), [5.0, 30.0, 15.0, 20.0]);
        app.run("edit.undo", json!({})).unwrap();
        assert_eq!(get_radii(&app), [5.0, 10.0, 15.0, 20.0]);
        app.run("edit.redo", json!({})).unwrap();
        assert_eq!(get_radii(&app), [5.0, 30.0, 15.0, 20.0]);
        for corner in 0..4 {
            let before = get_radii(&app);
            let radius = 35.0 + corner as f32;
            let mut expected = before;
            expected[corner] = f64::from(radius);
            let mut patch = corner_radii_patch(before, corner, radius);
            patch["layer"] = json!(id);
            app.run("shape.edit", patch).unwrap();
            assert_eq!(get_radii(&app), expected);
            app.run("edit.undo", json!({})).unwrap();
            assert_eq!(get_radii(&app), before);
            app.run("edit.redo", json!({})).unwrap();
            assert_eq!(get_radii(&app), expected);
        }
        app.run("shape.edit", json!({"layer": id, "radii": 12})).unwrap();
        assert_eq!(get_radii(&app), [12.0; 4]);
    }

    /// #534: dragging in a shape's fill picker, opened from the Properties panel at the right edge
    /// of the window, keeps the picker in place (it flipped from side to side as its width
    /// followed the colour readouts, which moved it under the pointer).
    #[test]
    fn shape_colour_picker_stays_put_while_dragging() {
        use egui::{PointerButton, vec2};
        use egui_kittest::kittest::Queryable;
        let mut app = app();
        let id = app.run("shape.create", json!({"kind": "rect", "rect": [10, 10, 100, 60], "fill": "#3070c0"})).unwrap()["layer"].as_u64().unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1440.0, 900.0)).build_ui_state(
            move |ui, app: &mut PhotospliceApp| {
                // Fonts set up after the first frame only apply from the next one.
                if !ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                    return;
                }
                egui::Panel::right("properties").resizable(false).exact_size(285.0).show(ui, |ui| shape_properties(app, ui, photosplice_doc::LayerId(id)));
            },
            app,
        );
        PhotospliceApp::setup_context(&h.ctx, crate::theme::ThemeKind::Midnight);
        h.run_steps(4);
        let label = h.query_all(egui_kittest::kittest::by().label("Fill").include_labels()).next().expect("the Fill label").rect();
        // The swatch is right of its label.
        let swatch = egui::pos2(label.right() + 17.0, label.center().y);
        let press = |h: &mut egui_kittest::Harness<'_, PhotospliceApp>, pos, pressed| {
            h.event(egui::Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
            h.step();
        };
        h.hover_at(swatch);
        h.step();
        press(&mut h, swatch, true);
        press(&mut h, swatch, false);
        h.run_steps(3);
        let popup = |h: &egui_kittest::Harness<'_, PhotospliceApp>| {
            h.ctx.memory(|m| m.areas().visible_layer_ids().into_iter().filter(|l| l.order == egui::Order::Foreground).find_map(|l| m.area_rect(l.id)))
        };
        let opened = popup(&h).expect("the picker opened");
        let fill = |h: &egui_kittest::Harness<'_, PhotospliceApp>| {
            let st = h.state().session.active().unwrap();
            match &st.doc.layer(photosplice_doc::LayerId(id)).unwrap().content {
                LayerContent::Shape(sh) => sh.fill.clone(),
                _ => None,
            }
        };
        let before = fill(&h);
        // Drag across the picker's colour area.
        let start = opened.min + vec2(60.0, opened.height() * 0.5);
        h.hover_at(start);
        h.step();
        press(&mut h, start, true);
        for i in 1..=10 {
            h.event(egui::Event::PointerMoved(start + vec2(i as f32 * 5.0, i as f32)));
            h.step();
            let r = popup(&h).expect("the picker stays open");
            assert_eq!(r.left_top(), opened.left_top(), "step {i}: the picker moved from {opened:?} to {r:?}");
            assert!(h.ctx.content_rect().contains_rect(r), "step {i}: {r:?} runs off the window");
        }
        press(&mut h, start, false);
        assert_ne!(fill(&h), before, "the drag picked a colour");
    }

    /// The options bar edits the selected shape's fill and stroke.
    #[test]
    fn shape_options_bar_edits_the_selected_shape() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        let mut app = app();
        app.run("tools.setColors", json!({"foreground": "#ff0000", "background": "#0000ff"})).unwrap();
        finish_shape(&mut app, Tool::Rectangle, [20.0, 20.0], [120.0, 100.0], Default::default());
        let shape = |app: &PhotospliceApp| {
            let st = app.session.active().unwrap();
            match &st.doc.layer(st.active_layer.unwrap()).unwrap().content {
                LayerContent::Shape(sh) => (sh.fill.clone(), sh.stroke.clone()),
                _ => panic!("not a shape layer"),
            }
        };
        assert!(shape(&app).0.is_some() && shape(&app).1.is_none());
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1100.0, 48.0)).build_ui_state(
            |ui, app: &mut PhotospliceApp| {
                if ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                    ui.horizontal_centered(|ui| options_bar(app, ui, Tool::Rectangle));
                }
            },
            app,
        );
        PhotospliceApp::setup_context(&h.ctx, crate::theme::ThemeKind::Anthracite);
        h.run_steps(4);
        let click_fill = |h: &mut egui_kittest::Harness<'_, PhotospliceApp>| {
            let label = h.query_all(egui_kittest::kittest::by().label("Fill:").include_labels()).next().expect("the Fill label").rect();
            let at = egui::pos2(label.right() + h.ctx.global_style().spacing.item_spacing.x + 7.0, label.center().y);
            h.hover_at(at);
            h.step();
            for pressed in [true, false] {
                h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
                h.step();
            }
            h.run_steps(2);
        };
        // Stroke width.
        h.get_all_by_role(Role::SpinButton).next().expect("the stroke width").click();
        h.run();
        for c in "6".chars() {
            h.event(egui::Event::Text(c.to_string()));
            h.run();
        }
        h.key_press(egui::Key::Enter);
        h.run_steps(2);
        let stroke = shape(h.state()).1.expect("the width field stroked the shape");
        assert_eq!(stroke.width, 6.0);
        assert_eq!(stroke.paint, photosplice_doc::Fill::Solid(photosplice_doc::Color::rgba(0.0, 0.0, 1.0, 1.0)));
        // Fill off.
        click_fill(&mut h);
        assert_eq!(shape(h.state()).0, None, "unchecking Fill removed the shape's fill");
        assert!(!h.state().ui.tool_options.shape_fill, "and the next shape has none either");
        h.state_mut().run("edit.undo", json!({})).unwrap();
        assert!(shape(h.state()).0.is_some());
        // No shape selected: no edit.
        h.state_mut().run("layer.new.layer", json!({})).unwrap();
        let history = h.state().session.active().unwrap().history.past_len();
        h.run_steps(2);
        click_fill(&mut h);
        assert_eq!(h.state().session.active().unwrap().history.past_len(), history);
    }

    /// #3915: Path Selection and Direct Selection options bars show Fill, Stroke, and Radius
    /// controls when a shape is selected, and editing them edits the selected shape.
    #[test]
    fn path_selection_options_bar_edits_the_selected_shape() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        let mut app = app();
        app.run("tools.setColors", json!({"foreground": "#ff0000", "background": "#0000ff"})).unwrap();
        finish_shape(&mut app, Tool::Rectangle, [20.0, 20.0], [120.0, 100.0], Default::default());
        let shape = |app: &PhotospliceApp| {
            let st = app.session.active().unwrap();
            match &st.doc.layer(st.active_layer.unwrap()).unwrap().content {
                LayerContent::Shape(sh) => (sh.fill.clone(), sh.stroke.clone()),
                _ => panic!("not a shape layer"),
            }
        };
        assert!(shape(&app).0.is_some() && shape(&app).1.is_none());
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1100.0, 48.0)).build_ui_state(
            |ui, app: &mut PhotospliceApp| {
                if ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                    ui.horizontal_centered(|ui| options_bar(app, ui, Tool::PathSelection));
                }
            },
            app,
        );
        PhotospliceApp::setup_context(&h.ctx, crate::theme::ThemeKind::Anthracite);
        h.run_steps(4);
        let click_fill = |h: &mut egui_kittest::Harness<'_, PhotospliceApp>| {
            let label = h.query_all(egui_kittest::kittest::by().label("Fill:").include_labels()).next().expect("the Fill label").rect();
            let at = egui::pos2(label.right() + h.ctx.global_style().spacing.item_spacing.x + 7.0, label.center().y);
            h.hover_at(at);
            h.step();
            for pressed in [true, false] {
                h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
                h.step();
            }
            h.run_steps(2);
        };
        // Stroke width field is available on Path Selection bar too.
        h.get_all_by_role(Role::SpinButton).next().expect("the stroke width").click();
        h.run();
        for c in "6".chars() {
            h.event(egui::Event::Text(c.to_string()));
            h.run();
        }
        h.key_press(egui::Key::Enter);
        h.run_steps(2);
        let stroke = shape(h.state()).1.expect("the width field stroked the shape");
        assert_eq!(stroke.width, 6.0);
        assert_eq!(stroke.paint, photosplice_doc::Fill::Solid(photosplice_doc::Color::rgba(0.0, 0.0, 1.0, 1.0)));
        // Fill toggle.
        click_fill(&mut h);
        assert_eq!(shape(h.state()).0, None, "unchecking Fill removed the shape's fill");
        // No shape selected: renders hint text instead.
        h.state_mut().run("layer.new.layer", json!({})).unwrap();
        h.run_steps(2);
        assert!(h.query_all(egui_kittest::kittest::by().label("Fill:").include_labels()).next().is_none());
    }
}

#[cfg(test)]
#[path = "path_selection_tests.rs"]
mod path_selection_tests;

#[cfg(test)]
#[path = "vector_paint_tests.rs"]
mod paint_tests;
