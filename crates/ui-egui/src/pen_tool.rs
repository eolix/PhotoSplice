//! Pen tool (P), as in Photoshop: it draws on a path in place (the work path, the targeted vector
//! mask, or a shape layer in Shape mode), one undo step per click or drag, named as Photoshop
//! names them (New Work Path, Add Anchor Point, Close Path, Delete Anchor Point).
//!
//! * Click adds a corner point to the subpath being drawn, drag pulls out smooth handles (⌥ while
//!   dragging breaks them into a cusp, ⇧ keeps the point and the handle at 45° steps). With no
//!   subpath being drawn a click starts one, combined with the path by the path operation.
//! * Click the first point of the subpath being drawn to close it (a drag shapes the closing).
//! * Click an end point of an open subpath to go on drawing from it.
//! * Auto Add/Delete (on, as in Photoshop): click a segment to add a point that keeps the curve
//!   (drag moves it), click a point to delete it.
//! * ↩ ends the subpath, Esc too; ⌘/Ctrl and ⌥ are Direct Selection and Convert Point
//!   (`direct_select`). The pointer shows what a click would do.
//!
//! Each edit runs the engine's `path.pen` (or `shape.create` for a Shape mode's first point), so
//! the control channel drives the same edits.

use egui::{Color32, Modifiers, Pos2};
use photosuite_doc::vector::{Path, PathOp, Subpath};
use photosuite_doc::{Knot, LayerContent};
use photosuite_geom::Point;
use photosuite_vector::edit::{self, Hit};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::PhotosuiteApp;
use crate::direct_select::PathRef;

/// The subpath being drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Active {
    pub target: PathRef,
    pub subpath: usize,
    /// New points go before its first point (drawing on from its start).
    pub at_start: bool,
}

/// Pen state: the subpath being drawn and the drag in progress.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pen {
    pub active: Option<Active>,
    #[serde(skip)]
    pub drag: Option<PenDrag>,
}

impl Pen {
    /// Is a subpath being drawn (or a point being placed)?
    pub fn drawing(&self) -> bool {
        self.active.is_some() || self.drag.is_some()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Shaping {
    /// A new point's handles (`reverse`: drawn from the subpath's start, so the drag pulls the
    /// in handle).
    Handles { reverse: bool },
    /// The point being closed onto.
    Close { reverse: bool },
    /// An end point drawn on from: the drag pulls its outgoing handle alone.
    Continue { reverse: bool },
    /// A point added on a segment (or any point, with the Curvature Pen): the drag moves it.
    Move,
    /// The Freeform Pen's trail, fitted into a subpath on release.
    Freehand,
    Nothing,
}

/// A press in progress.
#[derive(Clone, Debug, PartialEq)]
pub struct PenDrag {
    target: PathRef,
    /// Shape mode's first point: no layer yet (`shape.create` on release).
    new_shape: bool,
    base: Path,
    path: Path,
    knot: [usize; 2],
    shaping: Shaping,
    /// The `path.pen` action, or `None` when the press changes nothing unless dragged.
    action: Option<&'static str>,
    start: [f64; 2],
    moved: bool,
    next: Option<Active>,
    /// `path` as the drag has shaped it so far.
    shaped: Option<Path>,
    /// Curvature Pen: smooth points keep curving through their neighbours.
    curvature: bool,
    /// Freeform Pen: the pointer's trail.
    trail: Vec<[f64; 2]>,
}

/// What a press at the pointer would do (the pointer shows it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    New,
    Add,
    Close,
    Continue,
    Insert,
    Delete,
    /// Curvature Pen: a press on a point drags it.
    MovePoint,
}

fn hex(c: [f32; 4]) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]))
}

/// The path `r` names in the active document.
fn path_of(app: &PhotosuiteApp, r: PathRef) -> Option<Path> {
    let st = app.session.active()?;
    match r {
        PathRef::Work => st.doc.work_path.clone(),
        PathRef::Layer(id) => {
            let l = st.doc.layer(photosuite_doc::LayerId(id))?;
            match &l.content {
                LayerContent::Shape(sh) => Some(sh.path.clone()),
                _ => l.vector_mask.as_ref().map(|m| m.path.clone()),
            }
        }
    }
}

fn op(app: &PhotosuiteApp) -> PathOp {
    match app.ui.tool_options.pen_op.as_str() {
        "subtract" => PathOp::Subtract,
        "intersect" => PathOp::Intersect,
        "exclude" => PathOp::Exclude,
        _ => PathOp::Combine,
    }
}

/// Where a new subpath goes: the targeted vector mask, else the work path (Path mode); the
/// active shape layer when a path operation other than New Layer is chosen, else a new shape
/// layer (Shape mode). `None` path: nothing there yet.
fn new_target(app: &PhotosuiteApp) -> (PathRef, Option<Path>, bool) {
    if app.ui.tool_options.vector_mode == "shape" {
        if app.ui.tool_options.pen_op != "new"
            && let Some((id, p)) = crate::vector_ui::active_shape_path(app)
        {
            return (PathRef::Layer(id), Some(p), false);
        }
        return (PathRef::Work, None, true);
    }
    if let Some((id, p)) = crate::vector_ui::targeted_vector_mask(app) {
        return (PathRef::Layer(id), Some(p), false);
    }
    (PathRef::Work, path_of(app, PathRef::Work), false)
}

/// The subpath being drawn, if it still is one (an undo can take it away).
fn active(app: &PhotosuiteApp) -> Option<(Active, Path)> {
    let a = app.ui.pen.active?;
    let p = path_of(app, a.target)?;
    p.subpaths.get(a.subpath).filter(|sp| !sp.closed && !sp.knots.is_empty())?;
    Some((a, p))
}

fn tolerance(app: &PhotosuiteApp) -> f64 {
    6.0 / f64::from(app.current_zoom().max(0.01))
}

/// The end point of the active subpath new points join (its last, or its first when drawing
/// from the start) and the point a click closes onto (the other end).
fn ends(a: &Active, p: &Path) -> Option<([usize; 2], [usize; 2])> {
    let n = p.subpaths.get(a.subpath)?.knots.len();
    let (first, last) = ([a.subpath, 0], [a.subpath, n.checked_sub(1)?]);
    Some(if a.at_start { (first, last) } else { (last, first) })
}

/// What a press at `p` does, and the parts it acts on.
fn intent(app: &PhotosuiteApp, p: [f64; 2]) -> (Intent, Option<Hit>) {
    let tol = tolerance(app);
    let at = Point::new(p[0], p[1]);
    let curvature = app.ui.tool == crate::state::Tool::CurvaturePen;
    let auto = app.ui.tool_options.auto_add_delete || curvature;
    if let Some((a, path)) = active(app) {
        let hit = edit::hit(&path, at, tol, &[]);
        let (_, close) = ends(&a, &path).unwrap_or(([0, 0], [0, 0]));
        let n = path.subpaths.get(a.subpath).map_or(0, |s| s.knots.len());
        return match hit {
            Some(Hit::Anchor(k)) if k == close && n >= 2 => (Intent::Close, hit),
            Some(Hit::Anchor(k)) if endpoint(&path, k) && k[0] != a.subpath => (Intent::Continue, hit),
            Some(Hit::Anchor(_)) if curvature => (Intent::MovePoint, hit),
            Some(Hit::Anchor(k)) if auto && !(k[0] == a.subpath && endpoint(&path, k)) => (Intent::Delete, hit),
            Some(Hit::Segment(..)) if auto => (Intent::Insert, hit),
            _ => (Intent::Add, None),
        };
    }
    let (_, path, new_shape) = new_target(app);
    let hit = path.as_ref().filter(|_| !new_shape).and_then(|p| edit::hit(p, at, tol, &[]));
    match (hit, &path) {
        (Some(Hit::Anchor(k)), Some(pth)) if endpoint(pth, k) => (Intent::Continue, hit),
        (Some(Hit::Anchor(_)), _) if curvature => (Intent::MovePoint, hit),
        (Some(Hit::Anchor(_)), _) if auto => (Intent::Delete, hit),
        (Some(Hit::Segment(..)), _) if auto => (Intent::Insert, hit),
        _ => (Intent::New, None),
    }
}

/// An end point of an open subpath.
fn endpoint(p: &Path, [s, k]: [usize; 2]) -> bool {
    p.subpaths.get(s).is_some_and(|sp| !sp.closed && (k == 0 || k + 1 == sp.knots.len()))
}

/// ⇧: `p` on the nearest 45° line through `from`.
fn constrain(from: Point, p: [f64; 2]) -> [f64; 2] {
    let (dx, dy) = (p[0] - from.x, p[1] - from.y);
    let len = dx.hypot(dy);
    if len < 1e-9 {
        return p;
    }
    let a = (dy.atan2(dx) / std::f64::consts::FRAC_PI_4).round() * std::f64::consts::FRAC_PI_4;
    [from.x + len * a.cos(), from.y + len * a.sin()]
}

/// Curvature Pen: the smooth points around knot `k` get handles that carry the curve through
/// their neighbours (a third of the way along the chord between them); corners stay.
fn resmooth(path: &mut Path, [s, k]: [usize; 2]) {
    let Some(sp) = path.subpaths.get_mut(s) else { return };
    let n = sp.knots.len();
    let closed = sp.closed;
    let neighbour = |i: usize, d: isize| -> Option<usize> {
        let j = i as isize + d;
        if (0..n as isize).contains(&j) {
            Some(j as usize)
        } else if closed && n > 2 {
            Some(j.rem_euclid(n as isize) as usize)
        } else {
            None
        }
    };
    let mut around: Vec<usize> = [-1isize, 0, 1].iter().filter_map(|d| neighbour(k, *d)).collect();
    around.dedup();
    let anchors: Vec<Point> = sp.knots.iter().map(|k| k.anchor).collect();
    for i in around {
        if !sp.knots[i].smooth {
            continue;
        }
        let a = anchors[i];
        let (prev, next) = (neighbour(i, -1).map(|j| anchors[j]), neighbour(i, 1).map(|j| anchors[j]));
        let kn = &mut sp.knots[i];
        match (prev, next) {
            (Some(p), Some(q)) => {
                let (dx, dy) = ((q.x - p.x) / 6.0, (q.y - p.y) / 6.0);
                kn.in_ctrl = Point::new(a.x - dx, a.y - dy);
                kn.out_ctrl = Point::new(a.x + dx, a.y + dy);
            }
            (None, Some(q)) => {
                kn.in_ctrl = a;
                kn.out_ctrl = Point::new(a.x + (q.x - a.x) / 3.0, a.y + (q.y - a.y) / 3.0);
            }
            (Some(p), None) => {
                kn.out_ctrl = a;
                kn.in_ctrl = Point::new(a.x + (p.x - a.x) / 3.0, a.y + (p.y - a.y) / 3.0);
            }
            (None, None) => {}
        }
    }
}

/// Add Anchor Point (`add`) and Delete Anchor Point tools: a press on a segment adds a point
/// that keeps the curve (a drag then moves it); on a point, deletes it. They act on the path
/// under the pointer (the targeted vector mask or the active shape's path, then the work path).
pub fn anchor_down(app: &mut PhotosuiteApp, x: f64, y: f64, add: bool) {
    let tol = tolerance(app);
    let at = Point::new(x, y);
    let active = app.ui.pen.active;
    for (target, base) in crate::direct_select::candidates(app) {
        let mut path = base.clone();
        let (knot, shaping, action) = match (add, edit::hit(&base, at, tol, &[])) {
            (true, Some(Hit::Segment(k, t))) => match edit::insert_anchor(&mut path, k, t) {
                Ok(new) => (new, Shaping::Move, "insert"),
                Err(_) => continue,
            },
            (false, Some(Hit::Anchor(k))) => match edit::delete_anchor(&mut path, k) {
                Ok(()) => (k, Shaping::Nothing, "delete"),
                Err(_) => continue,
            },
            _ => continue,
        };
        app.ui.pen.drag = Some(PenDrag {
            target,
            new_shape: false,
            base,
            path,
            knot,
            shaping,
            action: Some(action),
            start: [x, y],
            moved: false,
            next: active,
            shaped: None,
            curvature: false,
            trail: Vec::new(),
        });
        return;
    }
}

/// A press with the Pen (no ⌘ / ⌥: those are `direct_select`'s).
pub fn down(app: &mut PhotosuiteApp, x: f64, y: f64, mods: Modifiers) {
    let curvature = app.ui.tool == crate::state::Tool::CurvaturePen;
    if app.ui.tool == crate::state::Tool::FreeformPen {
        let (target, base, new_shape) = new_target(app);
        let base = base.unwrap_or_else(|| Path::new(Vec::new()));
        app.ui.pen.drag = Some(PenDrag {
            target,
            new_shape,
            path: base.clone(),
            base,
            knot: [0, 0],
            shaping: Shaping::Freehand,
            action: None,
            start: [x, y],
            moved: false,
            next: None,
            shaped: None,
            curvature: false,
            trail: vec![[x, y]],
        });
        return;
    }
    let (what, hit) = intent(app, [x, y]);
    let current = active(app);
    let (target, base, new_shape) = match &current {
        Some((a, p)) => (a.target, Some(p.clone()), false),
        None => new_target(app),
    };
    let base = base.unwrap_or_else(|| Path::new(Vec::new()));
    let mut path = base.clone();
    let start = [x, y];
    let drag = |knot, shaping, action, next, path: Path| PenDrag {
        target,
        new_shape,
        base: base.clone(),
        path,
        knot,
        shaping,
        action,
        start,
        moved: false,
        next,
        shaped: None,
        curvature,
        trail: Vec::new(),
    };
    let made = match (what, hit) {
        (Intent::Close, Some(Hit::Anchor(k))) => {
            let a = current.map(|c| c.0);
            if let Some(sp) = path.subpaths.get_mut(k[0]) {
                sp.closed = true;
            }
            Some(drag(k, Shaping::Close { reverse: a.is_some_and(|a| a.at_start) }, Some("close"), None, path))
        }
        (Intent::Continue, Some(Hit::Anchor(k))) => {
            let reverse = k[1] == 0 && path.subpaths.get(k[0]).is_some_and(|s| s.knots.len() > 1);
            let next = Some(Active { target, subpath: k[0], at_start: reverse });
            Some(drag(k, Shaping::Continue { reverse }, None, next, path))
        }
        (Intent::Delete, Some(Hit::Anchor(k))) => {
            let removes = path.subpaths.get(k[0]).is_some_and(|s| s.knots.len() == 1);
            if edit::delete_anchor(&mut path, k).is_err() {
                return;
            }
            let next = current.map(|c| c.0).filter(|a| !(removes && a.subpath == k[0])).map(|mut a| {
                if removes && a.subpath > k[0] {
                    a.subpath -= 1;
                }
                a
            });
            Some(drag(k, Shaping::Nothing, Some("delete"), next, path))
        }
        (Intent::Insert, Some(Hit::Segment(k, t))) => {
            let Ok(new) = edit::insert_anchor(&mut path, k, t) else { return };
            if curvature {
                resmooth(&mut path, new);
            }
            Some(drag(new, Shaping::Move, Some("insert"), current.map(|c| c.0), path))
        }
        (Intent::MovePoint, Some(Hit::Anchor(k))) => Some(drag(k, Shaping::Move, None, current.map(|c| c.0), path)),
        _ => {
            let a = current.map(|c| c.0);
            let mut start = start;
            // ⇧: the new point on a 45° line from the one it joins.
            if mods.shift
                && let Some((join, _)) = a.and_then(|a| ends(&a, &path))
                && let Some(j) = path.subpaths.get(join[0]).and_then(|s| s.knots.get(join[1]))
            {
                start = constrain(j.anchor, start);
            }
            let k = Knot::corner(start[0], start[1]);
            let (knot, next, reverse) = match a {
                Some(a) if a.at_start => {
                    if let Some(sp) = path.subpaths.get_mut(a.subpath) {
                        sp.knots.insert(0, k);
                    }
                    ([a.subpath, 0], a, true)
                }
                Some(a) => {
                    let n = path.subpaths.get_mut(a.subpath).map_or(0, |sp| {
                        sp.knots.push(k);
                        sp.knots.len()
                    });
                    ([a.subpath, n.saturating_sub(1)], a, false)
                }
                None => {
                    path.subpaths.push(Subpath { closed: false, knots: vec![k], op: if path.subpaths.is_empty() { PathOp::Combine } else { op(app) } });
                    let s = path.subpaths.len() - 1;
                    ([s, 0], Active { target, subpath: s, at_start: false }, false)
                }
            };
            let action = if base.subpaths.is_empty() { "new" } else { "add" };
            // Curvature Pen: a smooth point (⌥: a corner) the curve flows through; a drag moves it.
            let shaping = if curvature {
                if let Some(k) = path.subpaths.get_mut(knot[0]).and_then(|s| s.knots.get_mut(knot[1])) {
                    k.smooth = !mods.alt;
                }
                resmooth(&mut path, knot);
                Shaping::Move
            } else {
                Shaping::Handles { reverse }
            };
            let mut d = drag(knot, shaping, Some(action), Some(next), path);
            d.start = start;
            Some(d)
        }
    };
    app.ui.pen.drag = made;
}

/// Pointer moves: shape what the press made.
pub fn moved(app: &mut PhotosuiteApp, x: f64, y: f64, mods: Modifiers) {
    let zoom = f64::from(app.current_zoom().max(0.01));
    let Some(d) = app.ui.pen.drag.as_mut() else { return };
    if !d.moved && (x - d.start[0]).hypot(y - d.start[1]) * zoom < 2.0 {
        return;
    }
    d.moved = true;
    let mut path = d.path.clone();
    match d.shaping {
        Shaping::Move => {
            if edit::move_anchors(&mut path, &[d.knot], [x - d.start[0], y - d.start[1]]).is_ok() {
                if d.curvature {
                    resmooth(&mut path, d.knot);
                }
                d.shaped = Some(path);
            }
            return;
        }
        Shaping::Freehand => {
            d.trail.push([x, y]);
            return;
        }
        _ => {}
    }
    let Some(k) = path.subpaths.get_mut(d.knot[0]).and_then(|s| s.knots.get_mut(d.knot[1])) else { return };
    let a = k.anchor;
    let p = if mods.shift { constrain(a, [x, y]) } else { [x, y] };
    let pt = Point::new(p[0], p[1]);
    let mirror = Point::new(2.0 * a.x - p[0], 2.0 * a.y - p[1]);
    match d.shaping {
        Shaping::Handles { reverse } => {
            let (ahead, behind) = if reverse { (&mut k.in_ctrl, &mut k.out_ctrl) } else { (&mut k.out_ctrl, &mut k.in_ctrl) };
            *ahead = pt;
            // ⌥ while dragging: a cusp, the handle behind stays where it was.
            if !mods.alt {
                *behind = mirror;
            }
            k.smooth = !mods.alt;
        }
        Shaping::Close { reverse } => {
            // The drag continues the path through the point closed onto.
            let (ahead, behind) = if reverse { (&mut k.in_ctrl, &mut k.out_ctrl) } else { (&mut k.out_ctrl, &mut k.in_ctrl) };
            *behind = mirror;
            if !mods.alt {
                *ahead = pt;
            }
            k.smooth = !mods.alt;
        }
        Shaping::Continue { reverse } => {
            *(if reverse { &mut k.in_ctrl } else { &mut k.out_ctrl }) = pt;
            k.smooth = false;
        }
        Shaping::Move | Shaping::Freehand | Shaping::Nothing => {}
    }
    d.shaped = Some(path);
}

/// Release: commit the press as one step.
pub fn up(app: &mut PhotosuiteApp) {
    let Some(d) = app.ui.pen.drag.take() else { return };
    let mut path = d.shaped.clone().unwrap_or_else(|| d.path.clone());
    if d.shaping == Shaping::Freehand {
        // The trail, fitted within Photoshop's default Curve Fit (2 px), as a new subpath.
        let pts: Vec<(f64, f64)> = d.trail.iter().map(|p| (p[0], p[1])).collect();
        let Some(mut sp) = photosuite_vector::trace::fit_open(&pts, 2.0) else { return };
        if !path.subpaths.is_empty() {
            sp.op = op(app);
        }
        path.subpaths.push(sp);
    }
    let action = match d.action {
        Some(a) => a,
        None if d.shaping == Shaping::Freehand => {
            if d.base.subpaths.is_empty() {
                "new"
            } else {
                "add"
            }
        }
        // An end point drawn on from, or a point dragged: only a drag changes the path.
        None if d.moved && d.shaping == Shaping::Move => "move",
        None if d.moved => "add",
        None => {
            app.ui.pen.active = d.next;
            return;
        }
    };
    let pj = photosuite_engine::vector_cmds::path_json(&path);
    let r = if d.new_shape {
        let o = &app.ui.tool_options;
        let fill = if o.shape_fill { json!(hex(app.session.tools.foreground)) } else { Value::Null };
        let stroke = if o.stroke_width > 0.0 { json!({"width": o.stroke_width, "color": hex(app.session.tools.background)}) } else { Value::Null };
        app.run("shape.create", json!({"kind": "path", "path": pj, "fill": fill, "stroke": stroke})).map(|_| {
            let id = app.session.active().and_then(|st| st.active_layer).map(|id| id.0);
            id.map(|id| Active { target: PathRef::Layer(id), subpath: 0, at_start: false })
        })
    } else {
        let mut p = match d.target {
            PathRef::Work => json!({"name": "work"}),
            PathRef::Layer(id) => json!({"name": "layer", "layer": id}),
        };
        p["path"] = pj;
        p["action"] = json!(action);
        app.run("path.pen", p).map(|_| d.next)
    };
    match r {
        Ok(next) => app.ui.pen.active = next,
        Err(e) => {
            app.ui.status = e;
            app.ui.status_error = true;
        }
    }
}

/// ↩ / Esc / ⌘-click: the subpath being drawn ends (it stays as drawn).
pub fn end(app: &mut PhotosuiteApp) {
    app.ui.pen = Pen::default();
}

/// The path being drawn on and its active end, a rubber band to the pointer, and what a click
/// would do beside the pointer.
pub fn draw_overlay(app: &PhotosuiteApp, painter: &egui::Painter, to_scr: &dyn Fn([f64; 2]) -> Pos2, accent: Color32) {
    let pen = &app.ui.pen;
    let shown = match &pen.drag {
        Some(d) => Some((d.target, d.shaped.clone().unwrap_or_else(|| d.path.clone()), Some(d.knot))),
        None => active(app).map(|(a, p)| (a.target, p, None)),
    };
    if let Some(d) = pen.drag.as_ref().filter(|d| d.shaping == Shaping::Freehand) {
        let pts: Vec<Pos2> = d.trail.iter().map(|p| to_scr(*p)).collect();
        painter.add(egui::Shape::line(pts, egui::Stroke::new(1.5, accent)));
        return;
    }
    if let Some((_, path, knot)) = &shown {
        crate::vector_ui::draw_outline(painter, path, to_scr, accent);
        let end = pen.active.and_then(|a| ends(&a, path)).map(|e| e.0);
        for (s, sp) in path.subpaths.iter().enumerate() {
            for (k, kn) in sp.knots.iter().enumerate() {
                let c = to_scr([kn.anchor.x, kn.anchor.y]);
                let hot = Some([s, k]) == *knot || (knot.is_none() && Some([s, k]) == end);
                if hot {
                    for h in [kn.in_ctrl, kn.out_ctrl] {
                        if (h.x - kn.anchor.x).abs() + (h.y - kn.anchor.y).abs() > 1e-6 {
                            crate::vector_ui::draw_handle(painter, c, to_scr([h.x, h.y]), accent);
                        }
                    }
                }
                crate::vector_ui::draw_anchor(painter, c, hot, accent);
            }
        }
        // Rubber band from the end new points join to the pointer.
        if pen.drag.is_none()
            && let (Some(e), Some(h)) = (end, app.hover_doc)
            && let Some(kn) = path.subpaths.get(e[0]).and_then(|s| s.knots.get(e[1]))
        {
            painter.add(egui::Shape::dashed_line(&[to_scr([kn.anchor.x, kn.anchor.y]), to_scr(h)], egui::Stroke::new(1.0, accent), 4.0, 3.0));
        }
    }
    // Beside the pointer: * new path, + add, − delete, o close, / continue.
    if pen.drag.is_none()
        && let Some(h) = app.hover_doc
    {
        let glyph = match intent(app, h).0 {
            Intent::New => "✱",
            Intent::Insert => "+",
            Intent::Delete => "−",
            Intent::Close => "○",
            Intent::Continue => "/",
            Intent::Add | Intent::MovePoint => "",
        };
        if !glyph.is_empty() {
            let at = to_scr(h) + egui::vec2(10.0, 10.0);
            painter.text(at, egui::Align2::LEFT_TOP, glyph, egui::FontId::proportional(11.0), accent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Tool;

    fn app() -> PhotosuiteApp {
        let mut app = PhotosuiteApp::new(photosuite_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 300, "height": 200})).unwrap();
        app.ui.tool = Tool::Pen;
        app
    }

    fn click(app: &mut PhotosuiteApp, x: f64, y: f64) {
        down(app, x, y, Modifiers::NONE);
        up(app);
    }

    fn drag(app: &mut PhotosuiteApp, from: [f64; 2], to: [f64; 2], mods: Modifiers) {
        down(app, from[0], from[1], mods);
        moved(app, to[0], to[1], mods);
        up(app);
    }

    fn work(app: &PhotosuiteApp) -> Path {
        app.session.active().unwrap().doc.work_path.clone().unwrap()
    }

    fn labels(app: &PhotosuiteApp) -> Vec<String> {
        app.session.active().unwrap().history.entries().iter().map(|e| e.to_string()).collect()
    }

    #[test]
    fn the_pen_draws_closes_and_names_its_steps_as_photoshop() {
        let mut app = app();
        click(&mut app, 20.0, 20.0);
        click(&mut app, 120.0, 20.0);
        // A drag pulls smooth handles; ⌥ while dragging makes a cusp.
        drag(&mut app, [120.0, 120.0], [140.0, 140.0], Modifiers::NONE);
        let k = work(&app).subpaths[0].knots[2];
        assert!(k.smooth && k.out_ctrl == Point::new(140.0, 140.0) && k.in_ctrl == Point::new(100.0, 100.0));
        drag(&mut app, [60.0, 150.0], [80.0, 170.0], Modifiers::ALT);
        let k = work(&app).subpaths[0].knots[3];
        assert!(!k.smooth && k.in_ctrl == k.anchor, "a cusp: the handle behind stays");
        // The first point closes it, and the next click starts a new subpath.
        click(&mut app, 20.5, 20.5);
        assert!(work(&app).subpaths[0].closed && work(&app).subpaths[0].knots.len() == 4);
        assert!(app.ui.pen.active.is_none());
        assert!(labels(&app).ends_with(&["New Work Path".into(), "Add Anchor Point".into(), "Add Anchor Point".into(), "Add Anchor Point".into(), "Close Path".into()]), "{:?}", labels(&app));
        click(&mut app, 250.0, 20.0);
        assert_eq!(work(&app).subpaths.len(), 2);
        // ⇧: the next point on a 45° line from the last.
        down(&mut app, 280.0, 51.0, Modifiers::SHIFT);
        up(&mut app);
        let k = work(&app).subpaths[1].knots[1].anchor;
        assert!((k.x - k.y - 230.0).abs() < 1e-6, "on the 45° line: {k:?}");
    }

    #[test]
    fn auto_add_delete_and_drawing_on_from_an_end_point() {
        let mut app = app();
        click(&mut app, 20.0, 100.0);
        click(&mut app, 220.0, 100.0);
        crate::pen_tool::end(&mut app);
        // A click on the segment adds a point; a click on a point deletes it.
        click(&mut app, 120.0, 100.0);
        assert_eq!(work(&app).subpaths[0].knots.len(), 3);
        assert_eq!(work(&app).subpaths[0].knots[1].anchor, Point::new(120.0, 100.0));
        click(&mut app, 120.0, 100.0);
        assert_eq!(work(&app).subpaths[0].knots.len(), 2);
        assert!(labels(&app).ends_with(&["Add Anchor Point".into(), "Delete Anchor Point".into()]));
        // Clicking an end point goes on drawing from it.
        click(&mut app, 220.0, 100.0);
        assert!(app.ui.pen.active.is_some_and(|a| a.subpath == 0 && !a.at_start));
        click(&mut app, 220.0, 180.0);
        assert_eq!(work(&app).subpaths[0].knots.len(), 3);
        // From the first point, new points go before it.
        crate::pen_tool::end(&mut app);
        click(&mut app, 20.0, 100.0);
        click(&mut app, 20.0, 20.0);
        assert_eq!(work(&app).subpaths[0].knots[0].anchor, Point::new(20.0, 20.0));
        // With Auto Add/Delete off, a click on a segment draws instead.
        crate::pen_tool::end(&mut app);
        app.ui.tool_options.auto_add_delete = false;
        click(&mut app, 120.0, 100.0);
        assert_eq!(work(&app).subpaths.len(), 2, "a new subpath");
    }

    #[test]
    fn shape_mode_makes_a_shape_layer_and_draws_on_it() {
        let mut app = app();
        app.ui.tool_options.vector_mode = "shape".into();
        click(&mut app, 20.0, 20.0);
        click(&mut app, 120.0, 20.0);
        click(&mut app, 120.0, 120.0);
        let st = app.session.active().unwrap();
        let l = st.doc.layer(st.active_layer.unwrap()).unwrap();
        let LayerContent::Shape(sh) = &l.content else { panic!("no shape layer") };
        assert_eq!(sh.path.subpaths[0].knots.len(), 3);
        assert!(st.doc.work_path.is_none());
        // An undo takes the last point away and the Pen goes on from the one before.
        app.run("edit.undo", json!({})).unwrap();
        click(&mut app, 60.0, 160.0);
        let st = app.session.active().unwrap();
        let LayerContent::Shape(sh) = &st.doc.layer(st.active_layer.unwrap()).unwrap().content else { panic!() };
        assert_eq!(sh.path.subpaths[0].knots[2].anchor, Point::new(60.0, 160.0));
    }

    #[test]
    fn the_curvature_pen_curves_through_its_points() {
        let mut app = app();
        app.ui.tool = Tool::CurvaturePen;
        for (x, y) in [(20.0, 100.0), (120.0, 20.0), (220.0, 100.0)] {
            click(&mut app, x, y);
        }
        let k = work(&app).subpaths[0].knots[1];
        assert!(k.smooth);
        // The middle point's handles run parallel to the chord between its neighbours.
        assert!((k.out_ctrl.y - k.anchor.y).abs() < 1e-9 && k.out_ctrl.x > k.anchor.x, "{k:?}");
        // ⌥-click: a corner.
        down(&mut app, 220.0, 180.0, Modifiers::ALT);
        up(&mut app);
        let k = work(&app).subpaths[0].knots[3];
        assert!(!k.smooth && k.in_ctrl == k.anchor);
        // Dragging a point moves it (and the curve follows).
        drag(&mut app, [120.0, 20.0], [120.0, 40.0], Modifiers::NONE);
        assert_eq!(work(&app).subpaths[0].knots[1].anchor, Point::new(120.0, 40.0));
        assert_eq!(labels(&app).last().map(String::as_str), Some("Drag Anchor Points"));
    }

    #[test]
    fn freeform_and_anchor_point_tools() {
        let mut app = app();
        app.ui.tool = Tool::FreeformPen;
        down(&mut app, 20.0, 100.0, Modifiers::NONE);
        for i in 1..=50 {
            let a = f64::from(i) / 50.0 * std::f64::consts::PI;
            moved(&mut app, 120.0 - 100.0 * a.cos(), 100.0 - 60.0 * a.sin(), Modifiers::NONE);
        }
        up(&mut app);
        let sp = &work(&app).subpaths[0];
        assert!(!sp.closed && sp.knots.len() >= 2, "a fitted open subpath");
        assert!(sp.knots.len() < 10, "fitted, not one point per sample: {}", sp.knots.len());
        // Add Anchor Point on a straight work path, Delete Anchor Point takes it out again.
        let mut ed = self::app();
        ed.run("path.set", json!({"path": {"subpaths": [{"closed": false, "knots": [[20, 150], [220, 150]]}]}})).unwrap();
        ed.ui.tool = Tool::AddAnchorPoint;
        crate::pen_tool::anchor_down(&mut ed, 120.0, 150.0, true);
        up(&mut ed);
        assert_eq!(work(&ed).subpaths[0].knots.len(), 3);
        ed.ui.tool = Tool::DeleteAnchorPoint;
        crate::pen_tool::anchor_down(&mut ed, 120.0, 150.0, false);
        up(&mut ed);
        assert_eq!(work(&ed).subpaths[0].knots.len(), 2);
        // Off the path, nothing happens.
        crate::pen_tool::anchor_down(&mut ed, 10.0, 10.0, false);
        assert!(ed.ui.pen.drag.is_none());
    }
}