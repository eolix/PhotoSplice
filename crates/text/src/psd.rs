//! PSD type layers: the `TySh` block (transform + text descriptor with `EngineData` + warp) ⇄ the
//! typed [`TextLayer`] model.
//!
//! Units: EngineData sizes are in text-space units, which the `TySh` transform maps to document
//! pixels; Photoshop shows them as points at the document resolution, so
//! `size_pt = FontSize × 72 / dpi` (and likewise leading, tracking-independent lengths, indents).
//! Box bounds stay in text-space pixels.
//!
//! Writing starts from the layer's original `TySh` (when present) and patches only the text, runs,
//! paragraphs, fonts, shape and transform, so Photoshop-only settings survive an edit.

use photosplice_color::{Color, ColorMode};
use photosplice_doc::TextLayer;
use photosplice_doc::text::{AntiAlias, Caps, CharStyle, Kerning, Orientation, ParagraphRun, ParagraphStyle, TextAlign, TextPath, TextRun, TextShape, TextWarp};
use photosplice_geom::{Affine, Point};
use photosplice_psd::descriptor::{Descriptor, Id, UnicodeString, Value as D, VersionedDescriptor};

use crate::engine_data::{self as ed, Value as E};
use crate::fonts::guess_from_postscript;

/// Parsed `TySh` contents.
#[derive(Clone, Debug, PartialEq)]
pub struct TySh {
    pub transform: Affine,
    pub text: Descriptor,
    pub warp: Option<Descriptor>,
    /// The trailing left, top, right, bottom: four big-endian `f32`s holding the `bounds`
    /// rectangle (every Photoshop type layer of the oracle corpus; the spec's "4 × 8 bytes" is not
    /// what Photoshop writes). Integers here read back as NaN and denormals, and Photoshop then
    /// asks to update the layer and loses its warp when it does (Photoshop kit case 07).
    pub bounds: [f32; 4],
}

/// Parses a `TySh` block.
pub fn parse_tysh(data: &[u8]) -> Option<TySh> {
    let f = |at: usize| data.get(at..at + 8).map(|b| f64::from_be_bytes(b.try_into().unwrap_or([0; 8])));
    let mut m = [0.0; 6];
    for (i, v) in m.iter_mut().enumerate() {
        *v = f(2 + i * 8)?;
    }
    let (text, used) = VersionedDescriptor::parse_prefix(data.get(52..)?).ok()?;
    let rest = data.get(52 + used..).unwrap_or(&[]);
    let (warp, wused) = match rest.get(2..).and_then(|r| VersionedDescriptor::parse_prefix(r).ok()) {
        Some((w, n)) => (Some(w.descriptor), n + 2),
        None => (None, 0),
    };
    let tail = rest.get(wused..).unwrap_or(&[]);
    let mut bounds = [0f32; 4];
    for (i, b) in bounds.iter_mut().enumerate() {
        if let Some(x) = tail.get(i * 4..i * 4 + 4) {
            *b = f32::from_be_bytes(x.try_into().unwrap_or([0; 4]));
        }
    }
    Some(TySh { transform: Affine { m }, text: text.descriptor, warp, bounds })
}

/// `tysh` moved by (`dx`, `dy`): only the transform's translation changes. The text, its styles,
/// the warp and the trailing bounds (which are in text space) stay byte for byte, so a moved
/// Photoshop type layer keeps everything Photoshop wrote. `None` when the block is too short.
pub fn translate_tysh(tysh: &[u8], dx: f64, dy: f64) -> Option<Vec<u8>> {
    let mut out = tysh.to_vec();
    // Version (2 bytes), then xx, xy, yx, yy, tx, ty as big-endian f64: tx at 34, ty at 42.
    for (at, d) in [(34, dx), (42, dy)] {
        let b = out.get_mut(at..at + 8)?;
        let v = f64::from_be_bytes(b.try_into().ok()?) + d;
        b.copy_from_slice(&v.to_be_bytes());
    }
    Some(out)
}

/// Serializes a `TySh` block.
pub fn write_tysh(t: &TySh) -> Vec<u8> {
    let mut v = 1u16.to_be_bytes().to_vec();
    for x in t.transform.m {
        v.extend_from_slice(&x.to_be_bytes());
    }
    v.extend_from_slice(&50u16.to_be_bytes());
    v.extend(VersionedDescriptor::new(t.text.clone()).to_bytes());
    v.extend_from_slice(&1u16.to_be_bytes());
    v.extend(VersionedDescriptor::new(t.warp.clone().unwrap_or_else(default_warp)).to_bytes());
    for b in t.bounds {
        v.extend_from_slice(&b.to_be_bytes());
    }
    v
}

fn default_warp() -> Descriptor {
    Descriptor { name: UnicodeString::new_nul(""), ..Descriptor::new("warp") }
        .with("warpStyle", enumv("warpStyle", "warpNone"))
        .with("warpValue", D::Double(0.0))
        .with("warpPerspective", D::Double(0.0))
        .with("warpPerspectiveOther", D::Double(0.0))
        .with("warpRotate", enumv("Ornt", "Hrzn"))
}

fn enumv(t: &str, v: &str) -> D {
    D::Enumerated { type_id: Id::new(t), value: Id::new(v) }
}

fn enum_value(d: &Descriptor, key: &str) -> Option<String> {
    match d.get(key) {
        Some(D::Enumerated { value, .. }) => Some(String::from_utf8_lossy(value.as_bytes()).into_owned()),
        _ => None,
    }
}

fn dnum(v: Option<&D>) -> Option<f64> {
    match v {
        Some(D::Double(x)) if x.is_finite() => Some(*x),
        Some(D::UnitFloat { value, .. }) if value.is_finite() => Some(*value),
        Some(D::Integer(i)) => Some(f64::from(*i)),
        _ => None,
    }
}

/// The EngineData of a text descriptor.
pub fn engine_data(text: &Descriptor) -> Option<E> {
    match text.get("EngineData") {
        Some(D::RawData(b)) => ed::parse(b).ok(),
        _ => None,
    }
}

/// Whether the text engine of a `TySh` block has nothing to draw: its EngineData text
/// (`EngineDict/Editor/Text`, what Reference lays out) is present and only whitespace. Some
/// layers carry their characters only in the descriptor's `Txt ` and render nothing
/// (storytold/photocraft@d2ad238e).
pub fn engine_text_is_blank(tysh: &[u8]) -> bool {
    parse_tysh(tysh)
        .and_then(|t| engine_data(&t.text))
        .and_then(|e| e.path(&["EngineDict", "Editor", "Text"]).and_then(E::as_str).map(|s| s.trim().is_empty()))
        .unwrap_or(false)
}

/// Parses the document's `Txt2` block (Photoshop's text engine data for all type layers).
pub fn parse_txt2(data: &[u8]) -> Option<E> {
    // Txt2 is a bare sequence of `/key value` pairs (no enclosing `<< >>`).
    let mut wrapped = Vec::with_capacity(data.len() + 4);
    wrapped.extend_from_slice(b"<<");
    wrapped.extend_from_slice(data);
    wrapped.extend_from_slice(b">>");
    ed::parse(&wrapped).ok().filter(|v| matches!(v, E::Dict(_)))
}

/// The number of text objects in a `Txt2` block (`/1 /1`, one per type layer, indexed by each
/// layer's `TextIndex`). `None` when the block doesn't parse.
pub fn txt2_object_count(data: &[u8]) -> Option<usize> {
    parse_txt2(data)?.path(&["1", "1"]).and_then(E::as_array).map(<[E]>::len)
}

/// A `TySh` block's `TextIndex`: its text object in the document's `Txt2`, when within
/// `0..=`[`MAX_TEXT_INDEX`] (a crafted file can name object `i32::MAX`; such a layer is
/// numbered afresh on export).
pub fn text_index(tysh: &[u8]) -> Option<i64> {
    match parse_tysh(tysh)?.text.get("TextIndex") {
        Some(D::Integer(i)) if (0..=MAX_TEXT_INDEX).contains(i) => Some(i64::from(*i)),
        _ => None,
    }
}

/// `tysh` with its `TextIndex` set to `index`: the integer patched in place (the block keeps
/// every other byte), else the block rewritten. `None` when it doesn't parse or `index` is not
/// an `i32` (any integer type converts, so callers pass `usize`-derived `i64`s or `i32`s alike).
pub fn set_text_index(tysh: &[u8], index: impl Into<i64>) -> Option<Vec<u8>> {
    let value = i32::try_from(index.into()).ok()?;
    const KEY: &[u8] = b"\0\0\0\x09TextIndexlong";
    let mut hits = tysh.windows(KEY.len()).enumerate().filter(|(_, w)| *w == KEY).map(|(i, _)| i);
    if let (Some(at), None) = (hits.next(), hits.next()) {
        let mut out = tysh.to_vec();
        let slot = out.get_mut(at + KEY.len()..at + KEY.len() + 4)?;
        slot.copy_from_slice(&value.to_be_bytes());
        return Some(out);
    }
    let mut t = parse_tysh(tysh)?;
    match t.text.items.iter_mut().find(|(k, _)| k.is("TextIndex")) {
        Some(item) => item.1 = D::Integer(value),
        None => t.text.items.push((photosplice_psd::descriptor::Id::new("TextIndex"), D::Integer(value))),
    }
    Some(write_tysh(&t))
}

/// A box type layer's box (`BoxBounds`: left, top, right, bottom in text space) from its `TySh`
/// EngineData; `None` for point and path text.
fn box_bounds(tysh: &[u8]) -> Option<[f64; 4]> {
    let e = engine_data(&parse_tysh(tysh)?.text)?;
    let shape = e.path(&["EngineDict", "Rendered", "Shapes", "Children"]).and_then(E::as_array)?.first()?;
    if shape.get("ShapeType").and_then(E::as_i64) != Some(1) {
        return None;
    }
    let b = arr_f(shape.path(&["Cookie", "Photoshop", "BoxBounds"]));
    let r = <[f64; 4]>::try_from(b.as_slice()).ok()?;
    (r.iter().all(|v| v.is_finite()) && r[2] > r[0] && r[3] > r[1]).then_some(r)
}

/// A box frame's outline (`/0 /1 /0`) for the box `[l, t, r, b]`, as Reference writes it: four
/// straight cubic segments, each with its control points on its ends, clockwise from the top
/// left.
fn box_frame_points([l, t, r, b]: [f64; 4]) -> E {
    let corners = [[l, t], [r, t], [r, b], [l, b]];
    let mut v = Vec::with_capacity(32);
    for i in 0..4 {
        let (p, q) = (corners[i], corners[(i + 1) % 4]);
        for c in [p, p, q, q] {
            v.extend(c.map(E::Real));
        }
    }
    E::Array(v)
}

/// The frame index (into `Txt2 /0 /8 /0`) text object `object` uses: its view's frame record.
fn object_frame(object: &E) -> Option<usize> {
    let f = object.path(&["1", "2"]).and_then(E::as_array)?.first()?.get("6").and_then(E::as_array)?.first()?.get("10").and_then(E::as_i64)?;
    usize::try_from(f).ok()
}

/// Points text object `object` at frame `frame`: its frame list (`/1 /0`) and its view's frame
/// record (`/1 /2 [0] /6 [0] /10`).
fn set_object_frame(object: &mut E, frame: usize) -> Option<()> {
    let f = E::Int(i64::try_from(frame).ok()?);
    let model = object.get_mut("1")?;
    model.set("0", E::Array(vec![E::Dict(vec![("0".into(), f.clone())])]));
    let view = match model.get_mut("2")? {
        E::Array(v) => v.first_mut()?,
        _ => return None,
    };
    match view.get_mut("6")? {
        E::Array(rows) => rows.first_mut()?.set("10", f),
        _ => return None,
    }
    Some(())
}

/// Moves a path frame (a `Txt2 /0 /8 /0` entry of kind 2) with its layer: its points, in
/// document space, go through `delta` (the layer's new transform after its old one's inverse),
/// and its document → text matrix (`/0 /2 /2`, the old transform's inverse) takes `delta`'s
/// inverse, so the path stays where it is in text space. Other frames are left as they are.
fn move_path_frame(frame: &mut E, delta: Affine) -> Option<()> {
    if delta == Affine::IDENTITY || frame.path(&["0", "2", "0"]).and_then(E::as_i64) != Some(2) {
        return Some(());
    }
    let undo = delta.inverse()?;
    let f = frame.get_mut("0")?;
    let pts = reals(f.path(&["1", "0"]));
    if !pts.len().is_multiple_of(2) {
        return None;
    }
    let moved: Vec<E> = pts.chunks_exact(2).flat_map(|p| {
        let q = delta.apply(Point::new(p[0], p[1]));
        [E::Real(q.x), E::Real(q.y)]
    }).collect();
    f.get_mut("1")?.set("0", E::Array(moved));
    let m = reals(f.path(&["2", "2"]));
    if let Ok([a, b, c, d, e, g]) = <[f64; 6]>::try_from(m.as_slice()) {
        let m2 = Affine { m: [a, b, c, d, e, g] }.mul(&undo);
        f.get_mut("2")?.set("2", E::Array(m2.m.iter().map(|v| E::Real(*v)).collect()));
    }
    Some(())
}

/// The transform change between two `TySh`s: `written`'s transform after `original`'s inverse.
fn tysh_delta(original: &[u8], written: &[u8]) -> Option<Affine> {
    let (a, b) = (parse_tysh(original)?.transform, parse_tysh(written)?.transform);
    Some(b.mul(&a.inverse()?))
}

/// Sets the outline of box text object `index`'s frame to the box `written` has, when it differs
/// from the one `original` had (a resized text box keeps a stale frame otherwise).
fn refresh_box_frame(txt2: &mut E, index: usize, original: &[u8], written: &[u8]) -> Option<()> {
    let (Some(was), Some(now)) = (box_bounds(original), box_bounds(written)) else { return Some(()) };
    if was == now {
        return Some(());
    }
    let frame = object_frame(txt2.path(&["1", "1"]).and_then(E::as_array)?.get(index)?)?;
    let frames = match txt2.get_mut("0")?.get_mut("8")?.get_mut("0")? {
        E::Array(f) => f,
        _ => return None,
    };
    let f = frames.get_mut(frame)?.get_mut("0")?;
    if f.path(&["2", "0"]).and_then(E::as_i64) != Some(1) {
        return None;
    }
    match f.get_mut("1") {
        Some(outline) => outline.set("0", box_frame_points(now)),
        None => return None,
    }
    Some(())
}

/// Appends a text object to `Txt2` (parsed) for a type layer that has none of its own yet: one
/// made here, or a duplicate sharing its source's `TextIndex`. The object is cloned from a
/// `template` (the `TySh`, as read, of a layer whose object `pristine` holds: `pristine` is the
/// `Txt2` as read, `txt2` the one being written) and patched like an edit ([`patch_txt2`]), with
/// a frame of its own: a point stub as the template's, a box outlining the layer's box, a path
/// only for an unedited copy of a path-text layer. The caller writes the returned index as the
/// layer's `TextIndex` (`written` already holds it). `None`, with `txt2` untouched, when no
/// template fits.
pub fn append_txt2(txt2: &mut E, pristine: &E, layer: &TextLayer, templates: &[&[u8]], written: &[u8], dpi: f32) -> Option<usize> {
    let index = txt2.path(&["1", "1"]).and_then(E::as_array)?.len();
    if text_index(written) != Some(i64::try_from(index).ok()?) {
        return None;
    }
    let boxed = matches!(layer.shape, TextShape::Box { .. });
    let want = if layer.path.is_some() { 2 } else { i64::from(boxed) };
    // Templates of the layer's own kind first; then any object, given a new frame.
    let candidates = [true, false].into_iter().flat_map(|same| templates.iter().map(move |t| (same, *t)));
    for (same, template) in candidates {
        let Some(t_index) = text_index(template).and_then(|i| usize::try_from(i).ok()) else { continue };
        let Some(t_obj) = pristine.path(&["1", "1"]).and_then(E::as_array).and_then(|o| o.get(t_index)) else { continue };
        let Some(frame) = object_frame(t_obj).and_then(|f| pristine.path(&["0", "8", "0"]).and_then(E::as_array)?.get(f).cloned()) else { continue };
        let kind = frame.path(&["0", "2", "0"]).and_then(E::as_i64).unwrap_or(0);
        if (kind == want) != same {
            continue;
        }
        // A path frame holds the path in document space: only a copy of a path layer can reuse
        // it (moved along with the copy), and none can be made.
        if want == 2 && !same {
            continue;
        }
        let mut frame = if same { frame } else { new_frame(boxed) };
        if want == 2 {
            let Some(delta) = tysh_delta(template, written) else { continue };
            if move_path_frame(&mut frame, delta).is_none() {
                continue;
            }
        }
        let mut next = txt2.clone();
        add_txt2_fonts(&mut next, written)?;
        // The template's object, patched as if the template had been edited into this layer.
        let mut scratch = pristine.clone();
        if let Some(fonts) = next.path(&["0", "1"]).cloned() {
            scratch.get_mut("0")?.set("1", fonts);
        }
        let Some((_, mut obj)) = patched_object(&scratch, layer, template, written, dpi) else { continue };
        if want == 1 {
            let bounds = box_bounds(written)?;
            frame.get_mut("0")?.get_mut("1")?.set("0", box_frame_points(bounds));
        }
        let frames = match next.get_mut("0")?.get_mut("8")?.get_mut("0")? {
            E::Array(f) => f,
            _ => return None,
        };
        frames.push(frame);
        let frame_index = frames.len() - 1;
        set_object_frame(&mut obj, frame_index)?;
        match next.get_mut("1")?.get_mut("1")? {
            E::Array(objects) => objects.push(obj),
            _ => return None,
        }
        *txt2 = next;
        return Some(index);
    }
    None
}

/// A frame (`Txt2 /0 /8 /0` entry) as Reference 2026 writes it for point text (a stub) or box
/// text (kind 1; its outline is set from the layer's box).
fn new_frame(boxed: bool) -> E {
    let (kind, flag) = if boxed { (Some(1), -2) } else { (None, -1) };
    let mut settings: Vec<(String, E)> = kind.map(|k| ("0".to_string(), E::Int(k))).into_iter().collect();
    settings.push(("6".into(), E::Array(vec![E::Real(flag as f64); 2])));
    settings.push(("11".into(), E::Dict(vec![("4".into(), E::Int(flag)), ("18".into(), E::Real(flag as f64)), ("22".into(), E::Real(0.025))])));
    let mut f = Vec::new();
    if boxed {
        f.push(("1".to_string(), E::Dict(vec![("0".into(), E::Array(Vec::new()))])));
    }
    f.push(("2".into(), E::Dict(settings)));
    E::Dict(vec![("0".into(), E::Dict(f))])
}

/// The point or box frame (a `Txt2 /0 /8 /0` entry's `/0`) of text object `index`: its view's
/// frame record `/10` names it. Path frames (kind 2) are [`txt2_text_path`]'s.
fn txt2_frame(txt2: &E, index: usize) -> Option<&E> {
    let object = txt2.path(&["1", "1"]).and_then(E::as_array)?.get(index)?;
    let frame = object.path(&["1", "2"]).and_then(E::as_array)?.first()?.get("6").and_then(E::as_array)?.iter().find_map(|f| f.get("10").and_then(E::as_i64))?;
    let f = txt2.path(&["0", "8", "0"]).and_then(E::as_array)?.get(usize::try_from(frame).ok()?)?.get("0")?;
    (f.path(&["2", "0"]).and_then(E::as_i64) != Some(2)).then_some(f)
}

/// Places `layer` where Reference draws it when its text object's frame in `Txt2` moved it
/// (storytold/photocraft@73ed08dd). Recent Reference keeps a type layer's position partly in its
/// frame: a matrix (`/2 /2`) from the frame's space to the `TySh` text space and, for point
/// text, the anchor (`/0`), while EngineData's `PointBase` stays at the origin and `BoxBounds`
/// stays in the frame's space. Ignoring them put box text far off the canvas once an edit
/// started, and point text a line too high and off its alignment anchor. Only when the layer's
/// `TextIndex` entry still holds the layer's text, and the frame agrees with the `TySh` `bounds`
/// (a `TySh` we rewrote already has the frame folded into its transform).
pub fn apply_txt2_frame(layer: &mut TextLayer, tysh: &[u8], txt2: &E) {
    let Some(t) = parse_tysh(tysh) else { return };
    let Some(index) = (match t.text.get("TextIndex") {
        Some(D::Integer(i)) => usize::try_from(*i).ok(),
        _ => None,
    }) else {
        return;
    };
    let text = txt2.path(&["1", "1"]).and_then(E::as_array).and_then(|a| a.get(index)).and_then(|o| o.path(&["0", "0"])).and_then(E::as_str);
    if text.is_none_or(|t| t.strip_suffix('\r').unwrap_or(t).replace('\r', "\n") != layer.text) || layer.path.is_some() {
        return;
    }
    let Some(frame) = txt2_frame(txt2, index) else { return };
    let m = arr_f(frame.path(&["2", "2"]));
    let matrix = if m.len() == 6 && m.iter().all(|v| v.is_finite()) { Affine { m: [m[0], m[1], m[2], m[3], m[4], m[5]] } } else { Affine::IDENTITY };
    let at = match layer.shape {
        TextShape::Point | TextShape::Path { .. } => {
            let p = arr_f(frame.get("0"));
            if p.len() == 2 && p.iter().all(|v| v.is_finite()) { Affine::translate(p[0], p[1]) } else { Affine::IDENTITY }
        }
        TextShape::Box { .. } => Affine::IDENTITY,
    };
    let to_text = matrix.mul(&at);
    if to_text == Affine::IDENTITY || to_text.determinant().abs() <= 1e-9 {
        return;
    }
    // The frame's origin (point text: the anchor; box text: the box's top left) must land in the
    // text-space `bounds` beside it, and not already be there without the frame.
    let origin = match layer.shape {
        TextShape::Point | TextShape::Path { .. } => Point::new(0.0, 0.0),
        TextShape::Box { .. } => {
            let b = engine_data(&t.text)
                .map(|e| {
                    arr_f(
                        e.path(&["EngineDict", "Rendered", "Shapes", "Children"])
                            .and_then(E::as_array)
                            .and_then(|c| c.first())
                            .and_then(|s| s.path(&["Cookie", "Photoshop", "BoxBounds"])),
                    )
                })
                .unwrap_or_default();
            let [x, y, ..] = b[..] else { return };
            Point::new(x, y)
        }
    };
    let Some([l, top, r, bottom]) = descriptor_rect(&t.text, "bounds") else { return };
    let inside = |p: Point| p.x >= l - 1.0 && p.x <= r + 1.0 && p.y >= top - 1.0 && p.y <= bottom + 1.0;
    if inside(origin) || !inside(to_text.apply(origin)) {
        return;
    }
    layer.transform = t.transform.mul(&to_text);
}

/// Depth limit for searching a text object's view tree.
const VIEW_DEPTH: usize = 24;

/// The first dictionary under `v` (depth-first, bounded) that holds `key`.
fn find_with<'a>(v: &'a E, key: &str, depth: usize) -> Option<&'a E> {
    if depth > VIEW_DEPTH {
        return None;
    }
    match v {
        E::Dict(d) => d.iter().find(|(k, _)| k == key).map(|_| v).or_else(|| d.iter().find_map(|(_, x)| find_with(x, key, depth + 1))),
        E::Array(a) => a.iter().find_map(|x| find_with(x, key, depth + 1)),
        _ => None,
    }
}

fn reals(v: Option<&E>) -> Vec<f64> {
    v.and_then(E::as_array).map(|a| a.iter().filter_map(E::as_f64).collect()).unwrap_or_default()
}

/// Type on a path, which Photoshop keeps only in `Txt2`: the frame list `/0 /8 /0` holds each
/// frame's points (`/0 /1 /0`, flat x,y pairs in document space, four per cubic segment), its
/// kind (`/0 /2 /0`: 0 point, 1 box, 2 path) and its document → text matrix (`/0 /2 /2`). The
/// object's view (`/1 /2 [0] /6 [0]`, an `F` record) names its frame (`/10`) and, deeper, the
/// laid-out glyph positions (`/21 /1`, `/21 /2`), from which the start point is found.
fn txt2_text_path(txt2: &E, object: &E) -> Option<TextPath> {
    let frames = txt2.path(&["0", "8", "0"]).and_then(E::as_array)?;
    let view = object.path(&["1", "2"]).and_then(E::as_array).and_then(|a| a.first());
    let frame_index = view.and_then(|v| v.get("6")).and_then(E::as_array).and_then(|a| a.first()).and_then(|f| f.get("10")).and_then(E::as_i64);
    let is_path = |f: &&E| f.path(&["0", "2", "0"]).and_then(E::as_i64) == Some(2);
    let frame = match frame_index {
        Some(i) => frames.get(usize::try_from(i).ok()?).filter(is_path)?,
        // No view (Photoshop omits it at times): only an unambiguous path frame.
        None => {
            let mut paths = frames.iter().filter(is_path);
            let f = paths.next()?;
            if paths.next().is_some() {
                return None;
            }
            f
        }
    };
    let pts = reals(frame.path(&["0", "1", "0"]));
    if pts.len() < 8 || !pts.len().is_multiple_of(8) {
        return None;
    }
    let m = reals(frame.path(&["0", "2", "2"]));
    let [a, b, c, d, e, f] = <[f64; 6]>::try_from(m.as_slice()).unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    let to_text = |x: f64, y: f64| [(a * x + c * y + e) as f32, (b * x + d * y + f) as f32];
    let segments: Vec<[[f32; 2]; 4]> = pts
        .chunks_exact(8)
        .map(|s| [to_text(s[0], s[1]), to_text(s[2], s[3]), to_text(s[4], s[5]), to_text(s[6], s[7])])
        .collect();
    let (first, last) = (segments.first()?[0], segments.last()?[3]);
    let closed = (first[0] - last[0]).abs() < 1e-3 && (first[1] - last[1]).abs() < 1e-3;
    let mut path = TextPath { segments, closed, start: 0.0 };
    // The first glyph's position, projected onto the path. Positions are offset by the line's
    // ascent on both axes in the files seen (path-circle.psd); the nearer reading wins.
    if let (Some(sampler), Some(glyphs)) = (crate::path::PathSampler::new(&path), view.and_then(|v| find_with(v, "21", 0)).and_then(|g| g.get("21"))) {
        let (xs, ys) = (reals(glyphs.get("1")), reals(glyphs.get("2")));
        let ascent = view.and_then(|v| find_with(v, "14", 0)).and_then(|l| l.get("14")).and_then(E::as_f64).unwrap_or(0.0);
        if let (Some(&x), Some(&y)) = (xs.first(), ys.first()) {
            let best = [0.0, ascent]
                .iter()
                .filter_map(|o| sampler.project((x + o) as f32, (y + o) as f32))
                .min_by(|p, q| p.1.total_cmp(&q.1));
            if let Some((s, _)) = best {
                path.start = s / sampler.length();
            }
        }
    }
    Some(path)
}

/// Updates the document's `Txt2` (parsed) for a type layer edited since it was read as
/// `original` (its `TySh` then) and now `written`, so Reference keeps what only `Txt2` holds
/// (text on a path, optical kerning…) instead of rebuilding the layer from `TySh`: the object's
/// text, its style runs (each from Reference's closest run, with the changed attributes set, see
/// [`TXT2_STYLE_KEYS`]), its paragraph runs (each from Reference's run with the same style) and
/// its view's length. Reference's cached layout stays: with it the file opens as edited and the
/// text stays on its path while typing; without it Reference asks to update the layer
/// (Reference-verified 2026-10-13). `false`, with `txt2` untouched, when the object can't be
/// matched: not the layer's text as read, a changed attribute `Txt2` keys aren't known for, a
/// font not in its font list, a paragraph style its runs don't have, several views.
pub fn patch_txt2(txt2: &mut E, layer: &TextLayer, original: &[u8], written: &[u8], dpi: f32) -> bool {
    let mut next = txt2.clone();
    if add_txt2_fonts(&mut next, written).is_none() {
        return false;
    }
    let Some((index, obj)) = patched_object(&next, layer, original, written, dpi) else {
        return false;
    };
    if refresh_box_frame(&mut next, index, original, written).is_none() {
        return false;
    }
    // A moved path-text layer takes its path with it.
    let frame = next.path(&["1", "1"]).and_then(E::as_array).and_then(|o| o.get(index)).and_then(object_frame);
    let moved = match (frame, tysh_delta(original, written)) {
        (Some(f), Some(delta)) => match next.get_mut("0").and_then(|v| v.get_mut("8")).and_then(|v| v.get_mut("0")) {
            Some(E::Array(frames)) => frames.get_mut(f).and_then(|fr| move_path_frame(fr, delta)),
            _ => None,
        },
        (None, _) => Some(()),
        (Some(_), None) => None,
    };
    if moved.is_none() {
        return false;
    }
    match next.get_mut("1").and_then(|v| v.get_mut("1")) {
        Some(E::Array(objects)) => match objects.get_mut(index) {
            Some(slot) => *slot = obj,
            None => return false,
        },
        _ => return false,
    }
    *txt2 = next;
    true
}

/// Adds to `Txt2`'s font list (`/0 /1 /0`) the fonts the `written` type layer's runs use that it
/// doesn't list yet: an entry in Reference's form (`<< /0 << /99 /CoolTypeFont /0 << /0 name /2
/// font type >> >> >>`, the type from the layer's `FontSet`), without the version string the
/// listed fonts carry, which we don't know.
fn add_txt2_fonts(txt2: &mut E, written: &[u8]) -> Option<()> {
    let e = engine_data(&parse_tysh(written)?.text)?;
    let set = e.path(&["ResourceDict", "FontSet"]).and_then(E::as_array).map(<[E]>::to_vec).unwrap_or_default();
    let used: Vec<String> = ed_runs(written, RunKind::Style)?
        .into_iter()
        .filter_map(|(_, a)| a.into_iter().find(|x| x.0 == "FontName").and_then(|x| x.1.as_str().map(str::to_string)))
        .collect();
    let list = match txt2.get_mut("0").and_then(|v| v.get_mut("1")).and_then(|v| v.get_mut("0")) {
        Some(E::Array(l)) => l,
        _ => return None,
    };
    for name in used {
        if name.is_empty() || list.iter().any(|f| f.path(&["0", "0", "0"]).and_then(E::as_str) == Some(name.as_str())) {
            continue;
        }
        let kind = set.iter().find(|f| f.get("Name").and_then(E::as_str) == Some(name.as_str())).and_then(|f| f.get("FontType")).and_then(E::as_i64).unwrap_or(1);
        let font = E::Dict(vec![("0".into(), E::String(name)), ("2".into(), E::Int(kind))]);
        list.push(E::Dict(vec![("0".into(), E::Dict(vec![("99".into(), E::Name("CoolTypeFont".into())), ("0".into(), font)]))]));
    }
    Some(())
}

fn patched_object(txt2: &E, layer: &TextLayer, original: &[u8], written: &[u8], dpi: f32) -> Option<(usize, E)> {
    let index = usize::try_from(text_index(original)?).ok()?;
    let mut was = text_layer_from_tysh(original, dpi)?;
    apply_txt2(&mut was, original, txt2);
    let mut obj = txt2.path(&["1", "1"])?.as_array()?.get(index)?.clone();
    let text = obj.path(&["0", "0"])?.as_str()?;
    if text.strip_suffix('\r').unwrap_or(text).replace('\r', "\n") != was.text {
        return None;
    }
    let total = i64::try_from(layer.text.encode_utf16().count()).ok()?.checked_add(1)?;
    // Paragraph runs and the view hold one entry per paragraph, even where paragraphs share a
    // style (one run over several made them one line when Reference updated the layer).
    let para_lens: Vec<usize> = layer.text.split('\n').map(|p| p.encode_utf16().count() + 1).collect();
    let style_runs = txt2_runs(txt2, obj.path(&["0", "6", "0"])?.as_array()?, RunKind::Style, original, written, None, total)?;
    let para_runs = txt2_runs(txt2, obj.path(&["0", "5", "0"])?.as_array()?, RunKind::Paragraph, original, written, Some(&para_lens), total)?;
    let model = obj.get_mut("0")?;
    model.set("0", E::String(format!("{}\r", layer.text.replace('\n', "\r"))));
    model.get_mut("6")?.set("0", E::Array(style_runs));
    model.get_mut("5")?.set("0", E::Array(para_runs));
    // The n-th new paragraph's view takes the n-th one's entry (the last's past them). Older
    // files (Reference CC 2015-era objects) have no per-paragraph view list.
    let Some(views) = obj.get_mut("1")?.get_mut("1").and_then(|v| v.get_mut("0")) else { return Some((index, obj)) };
    let E::Array(views) = views else { return None };
    if views.len() != was.text.split('\n').count() {
        return None;
    }
    let mut runs = Vec::with_capacity(para_lens.len());
    for (i, len) in para_lens.iter().enumerate() {
        let mut v = views.get(i).or(views.last())?.clone();
        v.set("1", E::Int(i64::try_from(*len).ok()?));
        runs.push(v);
    }
    *views = runs;
    Some((index, obj))
}

/// `TySh` style attributes and the `Txt2` style keys holding the same values (fitted on every
/// type layer of the corpora whose `Txt2` describes it: each pair agrees on all of 110-166 runs,
/// the rarer values included). Underline (`16`: 2 on, 0 off), fill and stroke colour (`53`,
/// `54`), the font (`0`, an index into `Txt2`'s font list) and automatic kerning (`11`: 0 off,
/// 1 metrics, 2 optical) are set apart.
pub const TXT2_STYLE_KEYS: [(&str, &str); 23] = [
    ("FontSize", "1"),
    ("FauxBold", "2"),
    ("FauxItalic", "3"),
    ("AutoLeading", "4"),
    ("Leading", "5"),
    ("HorizontalScale", "6"),
    ("VerticalScale", "7"),
    ("Tracking", "8"),
    ("BaselineShift", "9"),
    ("FontCaps", "12"),
    ("FontBaseline", "13"),
    ("Strikethrough", "15"),
    ("Ligatures", "18"),
    ("DLigatures", "19"),
    ("BaselineDirection", "35"),
    ("StyleRunAlignment", "37"),
    // The OpenType flags EngineData can also carry ([`TXT2_FEATURES`]).
    ("ContextualLigatures", "20"),
    ("Fractions", "23"),
    ("Ordinals", "24"),
    ("Swash", "25"),
    ("Titling", "26"),
    ("StylisticAlternates", "28"),
    ("Ornaments", "29"),
];

/// `TySh` paragraph properties and the `Txt2` paragraph keys holding the same values (fitted as
/// [`TXT2_STYLE_KEYS`] on the corpora's paragraph runs; Reference writes only some of them per
/// run, the rest default).
pub const TXT2_PARAGRAPH_KEYS: [(&str, &str); 16] = [
    ("Justification", "0"),
    ("FirstLineIndent", "1"),
    ("StartIndent", "2"),
    ("EndIndent", "3"),
    ("SpaceBefore", "4"),
    ("SpaceAfter", "5"),
    ("AutoLeading", "7"),
    ("AutoHyphenate", "9"),
    ("HyphenatedWordSize", "10"),
    ("PreHyphen", "11"),
    ("PostHyphen", "12"),
    ("ConsecutiveHyphens", "13"),
    ("Zone", "14"),
    ("WordSpacing", "17"),
    ("LetterSpacing", "18"),
    ("GlyphSpacing", "19"),
];

type Attrs = Vec<(String, E)>;

/// Character style runs or paragraph runs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RunKind {
    Style,
    Paragraph,
}

/// A `TySh`'s runs as EngineData resolves them: each run's UTF-16 length (the last with the
/// trailing `\r`) and its attributes (the normal style or paragraph sheet, the run's own over
/// it); a style's font as `FontName`.
fn ed_runs(tysh: &[u8], kind: RunKind) -> Option<Vec<(usize, Attrs)>> {
    let e = engine_data(&parse_tysh(tysh)?.text)?;
    let res = e.get("ResourceDict").or_else(|| e.get("DocumentResources"));
    let (normal, set, run, sheet, data) = match kind {
        RunKind::Style => ("TheNormalStyleSheet", "StyleSheetSet", "StyleRun", "StyleSheet", "StyleSheetData"),
        RunKind::Paragraph => ("TheNormalParagraphSheet", "ParagraphSheetSet", "ParagraphRun", "ParagraphSheet", "Properties"),
    };
    let fonts: Vec<&str> =
        res.and_then(|r| r.get("FontSet")).and_then(E::as_array).map(|a| a.iter().map(|f| f.get("Name").and_then(E::as_str).unwrap_or("")).collect()).unwrap_or_default();
    let n = usize::try_from(res.and_then(|r| r.get(normal)).and_then(E::as_i64).unwrap_or(0)).unwrap_or(0);
    let base = res.and_then(|r| r.get(set)).and_then(E::as_array).and_then(|a| a.get(n).or(a.first())).and_then(|s| s.get(data));
    let runs = e.path(&["EngineDict", run])?;
    let lens = runs.get("RunLengthArray")?.as_array()?;
    let items = runs.get("RunArray")?.as_array()?;
    let mut out = Vec::with_capacity(lens.len());
    for (len, d) in lens.iter().zip(items) {
        let mut attrs: Attrs = match base {
            Some(E::Dict(b)) => b.clone(),
            _ => Vec::new(),
        };
        if let Some(E::Dict(own)) = d.path(&[sheet, data]) {
            for (k, v) in own {
                match attrs.iter_mut().find(|a| a.0 == *k) {
                    Some(a) => a.1 = v.clone(),
                    None => attrs.push((k.clone(), v.clone())),
                }
            }
        }
        if kind == RunKind::Style
            && let Some(i) = attrs.iter().position(|a| a.0 == "Font")
        {
            let name = attrs.get(i).and_then(|a| a.1.as_i64()).and_then(|f| fonts.get(usize::try_from(f).ok()?).copied()).unwrap_or("").to_string();
            attrs.remove(i);
            attrs.push(("FontName".into(), E::String(name)));
        }
        let n = len.as_f64()?;
        if !(0.0..=1e9).contains(&n) {
            return None;
        }
        out.push((n as usize, attrs));
    }
    Some(out)
}

/// Equal attribute values, reals within float noise (a style regenerated from our model reads
/// 14.4 back as 14.399999…).
fn same_value(a: &E, b: &E) -> bool {
    match (a, b) {
        (E::Real(_) | E::Int(_), E::Real(_) | E::Int(_)) => match (a.as_f64(), b.as_f64()) {
            (Some(x), Some(y)) => (x - y).abs() <= 1e-4 * x.abs().max(y.abs()).max(1.0),
            _ => false,
        },
        (E::Array(x), E::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same_value(p, q)),
        (E::Dict(x), E::Dict(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.iter().any(|(k2, v2)| k == k2 && same_value(v, v2))),
        _ => a == b,
    }
}

/// `v` as the type `like` has in `Txt2` (`Bool`, `Int` or `Real`).
fn typed_like(v: &E, like: Option<&E>) -> Option<E> {
    let f = v.as_f64().or_else(|| v.as_bool().map(|b| f64::from(u8::from(b))))?;
    if !f.is_finite() {
        return None;
    }
    Some(match like {
        Some(E::Bool(_)) => E::Bool(f != 0.0),
        Some(E::Int(_)) => E::Int(f.round().clamp(-1e15, 1e15) as i64),
        Some(E::Real(_)) => E::Real(f),
        _ => v.clone(),
    })
}

/// Sets one changed attribute on a `Txt2` run (`<< /0 << /0 << … /6 style >> >> /1 len >>`, a
/// paragraph's properties under `/5`).
fn set_txt2_attr(txt2: &E, entry: &mut E, kind: RunKind, key: &str, v: &E) -> Option<()> {
    let (props, table): (&str, &[(&str, &str)]) = match kind {
        RunKind::Style => ("6", &TXT2_STYLE_KEYS),
        RunKind::Paragraph => ("5", &TXT2_PARAGRAPH_KEYS),
    };
    let inner = entry.get_mut("0")?.get_mut("0")?;
    if inner.get(props).is_none() {
        inner.set(props, E::dict());
    }
    let style = inner.get_mut(props)?;
    if let Some((_, num)) = table.iter().find(|(k, _)| *k == key) {
        let typed = match v {
            E::Array(_) => v.clone(),
            _ => typed_like(v, style.get(num))?,
        };
        style.set(num, typed);
        return Some(());
    }
    if kind == RunKind::Paragraph {
        return None;
    }
    match key {
        "Underline" => {
            let typed = typed_like(&E::Int(if v.as_bool()? { 2 } else { 0 }), style.get("16"))?;
            style.set("16", typed);
        }
        "FillColor" | "StrokeColor" => {
            let paint = style.get_mut(if key == "FillColor" { "53" } else { "54" })?.get_mut("0")?;
            paint.set("1", v.get("Values")?.clone());
            if let Some(t) = v.get("Type") {
                paint.set("0", t.clone());
            }
        }
        "FontName" => {
            let name = v.as_str()?;
            let i = txt2.path(&["0", "1", "0"])?.as_array()?.iter().position(|f| f.path(&["0", "0", "0"]).and_then(E::as_str) == Some(name))?;
            style.set("0", E::Int(i64::try_from(i).ok()?));
        }
        "AutoKerning" => {
            // On keeps the run's mode (metrics or optical, which EngineData doesn't tell apart).
            let now = style.get("11").and_then(E::as_i64).unwrap_or(1);
            style.set("11", E::Int(if !v.as_bool()? { 0 } else if now == 0 { 1 } else { now }));
        }
        _ => return None,
    }
    Some(())
}

/// The `Txt2` runs of `kind` for the layer as `written`: each starts from Reference's run (of the
/// layer as it was read, `original`) whose attributes differ least, with the differing ones set;
/// with `split`, one run per paragraph of those UTF-16 lengths. `None` when an attribute can't be
/// set or the lengths don't add up to `total`.
fn txt2_runs(txt2: &E, entries: &[E], kind: RunKind, original: &[u8], written: &[u8], split: Option<&[usize]>, total: i64) -> Option<Vec<E>> {
    let was = ed_runs(original, kind)?;
    // Reference's runs, each with the attributes of the `TySh` run it starts in.
    let covering = |runs: &[(usize, Attrs)], at: usize| -> Option<usize> {
        let mut end = 0usize;
        runs.iter()
            .position(|(len, _)| {
                end += len;
                at < end
            })
            .or_else(|| runs.len().checked_sub(1))
    };
    let mut templates: Vec<(&[(String, E)], &E)> = Vec::with_capacity(entries.len());
    let mut at = 0usize;
    for e in entries {
        templates.push((was.get(covering(&was, at)?)?.1.as_slice(), e));
        at = at.checked_add(usize::try_from(e.get("1")?.as_i64()?).ok()?)?;
    }
    let runs = ed_runs(written, kind)?;
    let runs: Vec<(usize, Attrs)> = match split {
        None => runs,
        Some(lens) => {
            let mut out = Vec::with_capacity(lens.len());
            let mut at = 0usize;
            for len in lens {
                out.push((*len, runs.get(covering(&runs, at)?)?.1.clone()));
                at += len;
            }
            out
        }
    };
    let differing = |a: &[(String, E)], b: &[(String, E)]| -> Vec<String> {
        let mut keys: Vec<String> = a.iter().chain(b).map(|x| x.0.clone()).collect();
        keys.sort();
        keys.dedup();
        keys.into_iter()
            .filter(|k| match (a.iter().find(|x| x.0 == *k), b.iter().find(|x| x.0 == *k)) {
                (Some(x), Some(y)) => !same_value(&x.1, &y.1),
                _ => true,
            })
            .collect()
    };
    let mut out = Vec::new();
    let mut sum = 0i64;
    for (len, attrs) in runs {
        if len == 0 {
            continue;
        }
        let (base, entry) = templates.iter().min_by_key(|(t, _)| differing(t, &attrs).len())?;
        let mut e = (*entry).clone();
        for key in differing(base, &attrs) {
            // An attribute the layer doesn't state keeps the template run's value.
            let Some((_, v)) = attrs.iter().find(|a| a.0 == key) else { continue };
            set_txt2_attr(txt2, &mut e, kind, &key, v)?;
        }
        let n = i64::try_from(len).ok()?;
        e.set("1", E::Int(n));
        sum = sum.checked_add(n)?;
        out.push(e);
    }
    (sum == total).then_some(out)
}

/// Applies what Photoshop keeps only in `Txt2` to `layer`, read from `tysh`: the automatic
/// kerning modes (EngineData writes Optical and "0" alike as `AutoKerning true`) and the
/// OpenType settings besides (discretionary) ligatures: stylistic sets, contextual alternates,
/// swashes and the rest ([`TXT2_FEATURES`]), which Photoshop leaves out of EngineData. Only when
/// the layer's `TextIndex` entry still holds the layer's text (an edit elsewhere leaves `Txt2`
/// stale).
///
/// Observed layout: `Txt2 /1` is the list of text objects; an object's `/0` is its text model
/// with `/0` the text (`\r` breaks, trailing `\r`) and `/6 /0` its style runs, each
/// `<< /0 << /0 << … /6 <<style>> >> >> /1 utf16-length >>`; style key `/11` is the auto-kern
/// mode of the pair after each character: 0 manual, 1 metrics, 2 optical.
pub fn apply_txt2(layer: &mut TextLayer, tysh: &[u8], txt2: &E) {
    let Some(index) = parse_tysh(tysh).and_then(|t| match t.text.get("TextIndex") {
        Some(D::Integer(i)) => usize::try_from(*i).ok(),
        _ => None,
    }) else {
        return;
    };
    let Some(model) = txt2.path(&["1", "1"]).and_then(E::as_array).and_then(|a| a.get(index)).and_then(|o| o.get("0")) else {
        return;
    };
    let Some(text) = model.get("0").and_then(E::as_str) else {
        return;
    };
    if text.strip_suffix('\r').unwrap_or(text).replace('\r', "\n") != layer.text {
        return;
    }
    if let Some(object) = txt2.path(&["1", "1"]).and_then(E::as_array).and_then(|a| a.get(index)) {
        layer.path = txt2_text_path(txt2, object);
    }
    let Some(style_runs) = model.path(&["6", "0"]).and_then(E::as_array) else {
        return;
    };
    let styles: Vec<(f64, Option<&E>)> = style_runs.iter().map(|r| (r.get("1").and_then(E::as_f64).unwrap_or(0.0), r.path(&["0", "0", "6"]))).collect();
    let lens = utf16_to_byte_lengths(text, &styles.iter().map(|m| m.0).collect::<Vec<_>>());
    // Txt2 style per byte segment, then split the model runs at segment boundaries.
    let mut segs: Vec<(usize, Option<&E>)> = lens.into_iter().zip(styles.iter().map(|m| m.1)).collect();
    segs.retain(|s| s.0 > 0);
    let runs = layer.char_runs();
    let mut out: Vec<TextRun> = Vec::new();
    let (mut si, mut seg_end) = (0usize, segs.first().map_or(usize::MAX, |s| s.0));
    let mut at = 0usize;
    for r in runs {
        let end = at + r.len;
        while at < end {
            while at >= seg_end && si + 1 < segs.len() {
                si += 1;
                seg_end = seg_end.saturating_add(segs.get(si).map_or(0, |s| s.0));
            }
            let piece_end = if at < seg_end { end.min(seg_end) } else { end };
            let mut style = r.style.clone();
            let txt2_style = segs.get(si).and_then(|s| s.1).filter(|_| at < seg_end);
            // Txt2 has the exact mode (EngineData's reading also marks neighbours of manual
            // kerns manual, like Photoshop's legacy reader).
            match txt2_style.and_then(|st| st.get("11")).and_then(E::as_i64) {
                Some(0) => style.kerning = Kerning::Off,
                Some(1) => style.kerning = Kerning::Metrics,
                Some(2) => style.kerning = Kerning::Optical,
                _ => {}
            }
            if let Some(st) = txt2_style {
                add_txt2_features(&mut style, st);
            }
            let len = piece_end - at;
            match out.last_mut() {
                Some(last) if last.style == style => last.len += len,
                _ => out.push(TextRun { len, style }),
            }
            at = piece_end;
        }
    }
    layer.runs = out;
    layer.sync_summary();
}

/// OpenType settings of a `Txt2` style that are on/off flags, by style key: `/20` Contextual
/// Alternates, `/23` Fractions, `/24` Ordinals, `/25` Swash, `/26` Titling Alternates, `/28`
/// Stylistic Alternates, `/29` Ornaments. Reference keeps them only in `Txt2`, so an edit drew
/// the default glyphs (storytold/photocraft@b071929e).
pub const TXT2_FEATURES: [(&str, &str); 7] = [("20", "calt"), ("23", "frac"), ("24", "ordn"), ("25", "swsh"), ("26", "titl"), ("28", "salt"), ("29", "ornm")];

/// The `Txt2` style key holding the Stylistic Sets as a bit mask: bit n is set n + 1
/// (`ss01`..`ss20`).
pub const TXT2_STYLISTIC_SETS: &str = "85";

/// Sets in `style` the OpenType features a `Txt2` style turns on ([`TXT2_FEATURES`] and the
/// Stylistic Sets mask). A feature the style already sets keeps its value. A flag that is off
/// changes nothing, except Contextual Alternates: shapers apply `calt` by default, Reference only
/// when it is on (its default is off), so off is written as `calt` 0.
fn add_txt2_features(style: &mut CharStyle, txt2_style: &E) {
    let mut set = |tag: String, value: u16| {
        if !style.features.iter().any(|f| f.tag == tag) {
            style.features.push(photosplice_doc::text::FontFeature { tag, value });
        }
    };
    for (key, tag) in TXT2_FEATURES {
        match txt2_style.get(key).and_then(E::as_bool) {
            Some(true) => set(tag.to_string(), 1),
            Some(false) if tag == "calt" => set(tag.to_string(), 0),
            _ => {}
        }
    }
    // A mask beyond the 20 sets is not Reference's: ignored.
    let sets = txt2_style.get(TXT2_STYLISTIC_SETS).and_then(E::as_i64).filter(|m| (0..1 << 20).contains(m)).unwrap_or(0);
    for n in 0..20 {
        if sets & (1 << n) != 0 {
            set(format!("ss{:02}", n + 1), 1);
        }
    }
}

/// Builds a [`TextLayer`] (model, text and transform; no cache, no `psd_raw`) from `TySh` data.
pub fn text_layer_from_tysh(data: &[u8], dpi: f32) -> Option<TextLayer> {
    let t = parse_tysh(data)?;
    let mut layer = TextLayer { transform: t.transform, ..Default::default() };
    let txt = match t.text.get("Txt ") {
        Some(D::Text(s)) => Some(s.to_string_lossy().trim_end_matches('\0').to_string()),
        _ => None,
    };
    layer.orientation = if enum_value(&t.text, "Ornt").as_deref() == Some("Vrtc") { Orientation::Vertical } else { Orientation::Horizontal };
    layer.antialias = match enum_value(&t.text, "AntA").as_deref() {
        Some("Anno" | "antiAliasNone") => AntiAlias::None,
        Some("antiAliasSharp" | "AnSh") => AntiAlias::Sharp,
        Some("AnCr" | "antiAliasCrisp") => AntiAlias::Crisp,
        Some("AnSt" | "antiAliasStrong") => AntiAlias::Strong,
        Some("antiAliasPlatformGray") => AntiAlias::Windows,
        Some("antiAliasPlatformLCD") => AntiAlias::WindowsLcd,
        Some(_) => AntiAlias::Smooth,
        // No `AntA`: the engine data's mode says it.
        None => {
            engine_data(&t.text).and_then(|e| e.path(&["EngineDict", "AntiAlias"]).and_then(E::as_f64)).and_then(engine_antialias).unwrap_or(AntiAlias::Smooth)
        }
    };
    if let Some(w) = &t.warp {
        let style = enum_value(w, "warpStyle").unwrap_or_else(|| "warpNone".into());
        if style != "warpNone" {
            layer.warp = Some(TextWarp {
                style,
                value: dnum(w.get("warpValue")).unwrap_or(0.0) as f32,
                horizontal_distortion: dnum(w.get("warpPerspective")).unwrap_or(0.0) as f32,
                vertical_distortion: dnum(w.get("warpPerspectiveOther")).unwrap_or(0.0) as f32,
                horizontal: enum_value(w, "warpRotate").as_deref() != Some("Vrtc"),
            });
        }
    }
    let k = 72.0 / if dpi.is_finite() && dpi > 0.0 { dpi } else { 72.0 };
    if let Some(e) = engine_data(&t.text) {
        // Without an `Ornt` key, the engine data's writing direction says it (2 = vertical).
        if enum_value(&t.text, "Ornt").is_none() && e.path(&["EngineDict", "Rendered", "Shapes", "WritingDirection"]).and_then(E::as_f64) == Some(2.0) {
            layer.orientation = Orientation::Vertical;
        }
        apply_engine_data(&mut layer, &e, txt.as_deref(), k);
    } else {
        layer.text = txt.unwrap_or_default().replace('\r', "\n");
    }
    // Some legacy PSDs store point text at a large local origin with a zero PointBase: their
    // transform maps that origin onto the canvas, and our zero-based layout far away. The layout
    // moves to the descriptor's ink origin, so editing, commands and the written TySh agree on the
    // visible position (storytold/photocraft@9b7349d0).
    if let Some([x, y]) = legacy_point_origin(&t) {
        let [a, b, c, d, e, f] = layer.transform.m;
        layer.transform.m = [a, b, c, d, a * x + c * y + e, b * x + d * y + f];
    }
    layer.sync_summary();
    Some(layer)
}

/// The local origin to fold into a legacy point-text transform, if any. Reference keeps point
/// text near its PointBase; the affected legacy files use a zero PointBase plus a very large
/// `boundingBox` origin, with an equally remote transform translation that cancels it out.
/// Ordinary point text is left alone: its transform origin already is the layout origin.
fn legacy_point_origin(t: &TySh) -> Option<[f64; 2]> {
    let e = engine_data(&t.text)?;
    let point_base = e
        .path(&["EngineDict", "Rendered", "Shapes", "Children"])
        .and_then(E::as_array)
        .and_then(|children| children.first())
        .and_then(|shape| shape.path(&["Cookie", "Photoshop", "PointBase"]))
        .and_then(E::as_array)?;
    let [px, py] = point_base else { return None };
    if px.as_f64() != Some(0.0) || py.as_f64() != Some(0.0) {
        return None;
    }
    let bounds = descriptor_rect(&t.text, "boundingBox")?;
    let [a, b, c, d, e, f] = t.transform.m;
    if ![a, b, c, d, e, f, bounds[0], bounds[1]].iter().all(|v| v.is_finite()) {
        return None;
    }
    let shifted = [a * bounds[0] + c * bounds[1] + e, b * bounds[0] + d * bounds[1] + f];
    let origin_distance = e.abs().max(f.abs());
    let shifted_distance = shifted[0].abs().max(shifted[1].abs());
    // A local origin in the thousands and a transform origin beyond any normal canvas are the
    // signature of this old writer; the shifted origin being nearer keeps a re-import from
    // applying it twice.
    (bounds[0].abs().max(bounds[1].abs()) > 1024.0 && origin_distance > 4096.0 && shifted_distance < origin_distance).then_some([bounds[0], bounds[1]])
}

/// The text-space box (`bounds`: left, top, right, bottom) a `TySh` records for its text, while
/// that text is still `text` (`None` once the layer's text differs, or without a box). Reference
/// warps type over this box: the advance width across, and down from a font-wide ascent to the
/// font's lowest point (its `head.yMin`), not the line's hhea ascent and descent.
pub fn tysh_text_box(tysh: &[u8], text: &str) -> Option<[f32; 4]> {
    let t = parse_tysh(tysh)?;
    let Some(D::Text(s)) = t.text.get("Txt ") else { return None };
    let stored = s.to_string_lossy();
    let stored = stored.trim_end_matches('\0');
    if stored.strip_suffix('\r').unwrap_or(stored).replace('\r', "\n") != text {
        return None;
    }
    descriptor_rect(&t.text, "bounds").map(|b| b.map(|v| v as f32))
}

/// The `[left, top, right, bottom]` of a `TySh` text-descriptor rectangle (`key`: `bounds`,
/// `boundingBox`).
fn descriptor_rect(text: &Descriptor, key: &str) -> Option<[f64; 4]> {
    let D::Descriptor(rect) = text.get(key)? else { return None };
    let b = [dnum(rect.get("Left"))?, dnum(rect.get("Top "))?, dnum(rect.get("Rght"))?, dnum(rect.get("Btom"))?];
    (b.iter().all(|v| v.is_finite()) && b[2] > b[0] && b[3] > b[1]).then_some(b)
}

pub(crate) fn arr_f(v: Option<&E>) -> Vec<f64> {
    v.and_then(E::as_array).map(|a| a.iter().filter_map(E::as_f64).collect()).unwrap_or_default()
}

/// Converts UTF-16 run lengths over `text` into UTF-8 byte lengths.
pub(crate) fn utf16_to_byte_lengths(text: &str, lens: &[f64]) -> Vec<usize> {
    let mut out = Vec::with_capacity(lens.len());
    let mut chars = text.chars().peekable();
    for &l in lens {
        let mut units = l.max(0.0) as usize;
        let mut bytes = 0;
        while units > 0 {
            match chars.next() {
                Some(c) => {
                    units = units.saturating_sub(c.len_utf16());
                    bytes += c.len_utf8();
                }
                None => break,
            }
        }
        out.push(bytes);
    }
    out
}

fn apply_engine_data(layer: &mut TextLayer, e: &E, txt: Option<&str>, k: f32) {
    let ed_text = e.path(&["EngineDict", "Editor", "Text"]).and_then(E::as_str).unwrap_or("").to_string();
    // Photoshop always ends the engine text with a paragraph break; the descriptor text doesn't.
    let text = txt.map(str::to_string).unwrap_or_else(|| ed_text.strip_suffix('\r').unwrap_or(&ed_text).to_string());
    layer.text = text.replace('\r', "\n");
    let res = e.get("ResourceDict").or_else(|| e.get("DocumentResources"));
    let fonts: Vec<String> = res
        .and_then(|r| r.get("FontSet"))
        .and_then(E::as_array)
        .map(|a| a.iter().map(|f| f.get("Name").and_then(E::as_str).unwrap_or("").to_string()).collect())
        .unwrap_or_default();
    // Style resolution (storytold/photocraft@73ed08dd). A run's own `StyleSheetData` /
    // `Properties` hold only what it overrides; the rest comes from its sheets, as in Reference:
    // - a paragraph run: its `Properties` → the `ParagraphSheetSet` entry its `Name` names (a
    //   paragraph style; or its `Parent` index) → `TheNormalParagraphSheet`;
    // - a character run: its `StyleSheetData` → the `StyleSheetSet` entry its `Parent` or `Name`
    //   names (a character style) → the default character style of its paragraph (the paragraph
    //   run's `DefaultStyleSheet`, else its paragraph style's) → `TheNormalStyleSheet`.
    // Missing or out-of-range indices fall through to the next level. Only the Normal sheets were
    // read, so a centred paragraph style came back Left and an edit re-laid the text.
    let style_set: &[E] = res.and_then(|r| r.get("StyleSheetSet")).and_then(E::as_array).unwrap_or(&[]);
    let para_set: &[E] = res.and_then(|r| r.get("ParagraphSheetSet")).and_then(E::as_array).unwrap_or(&[]);
    let index = |key: &str| res.and_then(|r| r.get(key)).and_then(E::as_i64).and_then(|i| usize::try_from(i).ok());
    let normal_style = index("TheNormalStyleSheet").and_then(|i| style_set.get(i)).or(style_set.first());
    let normal_para = index("TheNormalParagraphSheet").and_then(|i| para_set.get(i)).or(para_set.first());
    // A run names its sheet by a `Parent` index or by `Name` (see `psd_styles::reference`).
    let named = |set: &'_ [E], sheet: &E| -> Option<usize> {
        if let Some(i) = sheet.get("Parent").and_then(E::as_i64).and_then(|i| usize::try_from(i).ok()).filter(|i| *i < set.len()) {
            return Some(i);
        }
        let name = sheet.get("Name").and_then(E::as_str)?;
        set.iter().position(|s| s.get("Name").and_then(E::as_str) == Some(name))
    };

    // Paragraph runs first: a character run's defaults depend on its paragraph.
    let prun = e.path(&["EngineDict", "ParagraphRun"]);
    let psheets: Vec<&E> =
        prun.and_then(|r| r.get("RunArray")).and_then(E::as_array).map(|a| a.iter().filter_map(|x| x.get("ParagraphSheet")).collect()).unwrap_or_default();
    let plens16 = arr_f(prun.and_then(|r| r.get("RunLengthArray")));
    let plens = utf16_to_byte_lengths(&ed_text, &plens16);
    let empty = E::Dict(Vec::new());
    // Per paragraph run: its merged base properties and its default character style.
    let para_bases: Vec<(E, Option<&E>)> = psheets
        .iter()
        .map(|sheet| {
            let style = named(para_set, sheet).and_then(|i| para_set.get(i));
            let base = merged(&[style.and_then(|s| s.get("Properties")), normal_para.and_then(|s| s.get("Properties"))]);
            let char_default = [Some(*sheet), style]
                .into_iter()
                .flatten()
                .find_map(|s| s.get("DefaultStyleSheet").and_then(E::as_i64).and_then(|i| usize::try_from(i).ok()).and_then(|i| style_set.get(i)));
            (base, char_default)
        })
        .collect();
    layer.paragraphs = psheets
        .iter()
        .zip(&para_bases)
        .zip(plens)
        .map(|((sheet, (base, _)), len)| ParagraphRun { len, style: para_style(Some(base), sheet.get("Properties").unwrap_or(&empty), k) })
        .collect();

    // Character runs.
    let srun = e.path(&["EngineDict", "StyleRun"]);
    let ssheets: Vec<&E> =
        srun.and_then(|r| r.get("RunArray")).and_then(E::as_array).map(|a| a.iter().filter_map(|x| x.get("StyleSheet")).collect()).unwrap_or_default();
    let slens16 = arr_f(srun.and_then(|r| r.get("RunLengthArray")));
    let slens = utf16_to_byte_lengths(&ed_text, &slens16);
    // The paragraph run each character run starts in (UTF-16 offsets).
    let para_of = |at: f64| -> Option<usize> {
        let mut end = 0.0;
        for (i, n) in plens16.iter().enumerate() {
            end += n;
            if at < end {
                return Some(i);
            }
        }
        plens16.len().checked_sub(1)
    };
    let mut at16 = 0.0;
    let bases: Vec<E> = ssheets
        .iter()
        .enumerate()
        .map(|(i, sheet)| {
            let own = named(style_set, sheet).and_then(|i| style_set.get(i));
            let para_default = para_of(at16).and_then(|p| para_bases.get(p)).and_then(|(_, c)| *c);
            at16 += slens16.get(i).copied().unwrap_or(0.0);
            merged(&[own, para_default, normal_style].map(|s| s.and_then(|s| s.get("StyleSheetData"))))
        })
        .collect();
    let sdata: Vec<&E> = ssheets.iter().map(|s| s.get("StyleSheetData").unwrap_or(&empty)).collect();
    let incoming: Vec<(usize, bool, f32)> = sdata
        .iter()
        .zip(&bases)
        .zip(&slens)
        .map(|((d, base), len)| {
            let auto = lookup(d, Some(base), "AutoKerning").and_then(E::as_bool) != Some(false);
            let kern = lookup(d, Some(base), "Kerning").and_then(E::as_f64).filter(|v| v.is_finite()).unwrap_or(0.0) as f32;
            (*len, auto, kern)
        })
        .collect();
    layer.runs = sdata.iter().zip(&bases).zip(slens).map(|((d, base), len)| TextRun { len, style: char_style(Some(base), d, &fonts, k) }).collect();
    layer.runs = kerning_from_pairs(&layer.text, layer.char_runs(), &incoming);

    // Shape.
    let shape = e.path(&["EngineDict", "Rendered", "Shapes", "Children"]).and_then(E::as_array).and_then(|a| a.first());
    if let Some(sh) = shape
        && sh.get("ShapeType").and_then(E::as_i64) == Some(1)
    {
        let b = arr_f(sh.path(&["Cookie", "Photoshop", "BoxBounds"]));
        if b.len() == 4 {
            layer.shape = TextShape::Box { x: b[0] as f32, y: b[1] as f32, width: (b[2] - b[0]) as f32, height: (b[3] - b[1]) as f32 };
        }
    }
}

/// The dictionaries in `levels` as one: a key comes from the first level that has it.
fn merged(levels: &[Option<&E>]) -> E {
    let mut out: Vec<(String, E)> = Vec::new();
    for level in levels.iter().flatten() {
        if let E::Dict(kv) = level {
            for (k, v) in kv {
                if !out.iter().any(|(o, _)| o == k) {
                    out.push((k.clone(), v.clone()));
                }
            }
        }
    }
    E::Dict(out)
}

fn lookup<'a>(run: &'a E, base: Option<&'a E>, key: &str) -> Option<&'a E> {
    run.get(key).or_else(|| base.and_then(|b| b.get(key)))
}

pub(crate) fn char_style(base: Option<&E>, d: &E, fonts: &[String], k: f32) -> CharStyle {
    let g = |key: &str| lookup(d, base, key);
    let num = |key: &str| g(key).and_then(E::as_f64);
    // An f32 that a huge finite f64 overflowed to infinity is dropped too.
    let numk = |key: &str, k: f32| num(key).map(|v| v as f32 * k).filter(|v| v.is_finite());
    let flag = |key: &str| g(key).and_then(E::as_bool);
    let mut s = CharStyle::default();
    if let Some(ps) = g("Font").and_then(E::as_i64).and_then(|i| usize::try_from(i).ok()).and_then(|i| fonts.get(i)) {
        let r = guess_from_postscript(ps);
        s.font_family = r.family;
        s.weight = r.weight;
        s.italic = r.italic;
        s.postscript_name = Some(ps.clone());
    }
    if let Some(v) = numk("FontSize", k) {
        s.size_pt = v;
    }
    if flag("AutoLeading") == Some(false)
        && let Some(v) = numk("Leading", k)
    {
        s.leading_pt = Some(v);
    }
    s.tracking = numk("Tracking", 1.0).unwrap_or(0.0);
    s.baseline_shift_pt = numk("BaselineShift", k).unwrap_or(0.0);
    s.horizontal_scale = numk("HorizontalScale", 1.0).unwrap_or(1.0);
    s.vertical_scale = numk("VerticalScale", 1.0).unwrap_or(1.0);
    s.faux_bold = flag("FauxBold").unwrap_or(false);
    s.faux_italic = flag("FauxItalic").unwrap_or(false);
    s.underline = flag("Underline").unwrap_or(false);
    s.strikethrough = flag("Strikethrough").unwrap_or(false);
    s.ligatures = flag("Ligatures").unwrap_or(true);
    s.discretionary_ligatures = flag("DLigatures").unwrap_or(false);
    for (key, tag) in OPENTYPE_KEYS {
        if flag(key) == Some(true) {
            s.features.push(photosplice_doc::text::FontFeature { tag: tag.to_string(), value: 1 });
        }
    }
    s.kerning = if flag("AutoKerning") == Some(false) { Kerning::Off } else { Kerning::Metrics };
    s.kern = numk("Kerning", 1.0).unwrap_or(0.0);
    s.caps = match g("FontCaps").and_then(E::as_i64) {
        Some(1) => Caps::SmallCaps,
        Some(2) => Caps::AllCaps,
        _ => Caps::Normal,
    };
    // FontBaseline: 1 superscript, 2 subscript.
    s.position = match g("FontBaseline").and_then(E::as_i64) {
        Some(1) => photosplice_doc::text::BaselinePosition::Superscript,
        Some(2) => photosplice_doc::text::BaselinePosition::Subscript,
        _ => photosplice_doc::text::BaselinePosition::Normal,
    };
    if let Some(c) = g("FillColor") {
        // Positions matter (alpha, then the channels): a non-finite entry voids the colour.
        let v: Vec<f64> = c.get("Values").and_then(E::as_array).map(|a| a.iter().filter_map(E::as_f64).collect()).unwrap_or_default();
        let n_raw = c.get("Values").and_then(E::as_array).map_or(0, <[E]>::len);
        let v = if v.len() == n_raw && v.iter().all(|x| (*x as f32).is_finite()) { v } else { Vec::new() };
        let a = v.first().copied().unwrap_or(1.0) as f32;
        s.color = match v.len() {
            5 => Color { mode: ColorMode::Cmyk, c: [v[1] as f32, v[2] as f32, v[3] as f32, v[4] as f32], alpha: a },
            2 => Color { mode: ColorMode::Grayscale, c: [v[1] as f32, 0.0, 0.0, 0.0], alpha: a },
            4 => Color::rgba(v[1] as f32, v[2] as f32, v[3] as f32, a),
            _ => Color::BLACK,
        };
    }
    s
}

pub(crate) fn para_style(base: Option<&E>, d: &E, k: f32) -> ParagraphStyle {
    let num = |key: &str| lookup(d, base, key).and_then(E::as_f64);
    let numk = |key: &str, k: f32| num(key).map(|v| v as f32 * k).filter(|v| v.is_finite());
    ParagraphStyle {
        align: match num("Justification").map(|v| v as i64) {
            Some(1) => TextAlign::Right,
            Some(2) => TextAlign::Center,
            Some(3) => TextAlign::JustifyLeft,
            Some(4) => TextAlign::JustifyRight,
            Some(5) => TextAlign::JustifyCenter,
            Some(6) => TextAlign::JustifyAll,
            _ => TextAlign::Left,
        },
        first_line_indent_pt: numk("FirstLineIndent", k).unwrap_or(0.0),
        start_indent_pt: numk("StartIndent", k).unwrap_or(0.0),
        end_indent_pt: numk("EndIndent", k).unwrap_or(0.0),
        space_before_pt: numk("SpaceBefore", k).unwrap_or(0.0),
        space_after_pt: numk("SpaceAfter", k).unwrap_or(0.0),
        auto_leading: numk("AutoLeading", 1.0).unwrap_or(1.2),
        hyphenate: lookup(d, base, "AutoHyphenate").and_then(E::as_bool).unwrap_or(false),
        every_line_composer: lookup(d, base, "EveryLineComposer").and_then(E::as_bool).unwrap_or(true),
        ..Default::default()
    }
}

// ---------- writing ----------

fn real(v: f32) -> E {
    E::Real(f64::from(v))
}

/// Manual kerning as Photoshop stores it: a whole number of 1/1000 em (its UI range is ±1000;
/// anything non-finite writes 0).
pub(crate) fn kern_units(v: f32) -> i64 {
    if v.is_finite() { v.round().clamp(-10_000.0, 10_000.0) as i64 } else { 0 }
}

// Kerning in EngineData, as observed in Photoshop 2026 (writing, and reading files without
// `Txt2`): a manual kern k (1/1000 em) after character c is written on character c + 1 as
// `AutoKerning false` + `Kerning k`; everything else is `true` + 0. Photoshop's reader takes
// `Kerning k` on character j as the kern after j - 1 (making j - 1 manual), and
// `AutoKerning false` on j as "character j - 2 is manually kerned" (no automatic kerning). The
// automatic mode itself (Metrics or Optical) isn't in EngineData: Photoshop keeps it in the
// document's `Txt2` (see [`apply_txt2`]). We write and read the same way, so Photoshop reads
// our kerning as we do and Photoshop's own fields round-trip unchanged. (The mode of the last
// character has no slot and reads back as Metrics.)

fn manual(s: &CharStyle) -> bool {
    s.kerning == Kerning::Off || kern_units(s.kern) != 0
}

/// EngineData style runs for `ed_text` (the layer text with `\r` breaks and the trailing `\r`):
/// (model run index, `(AutoKerning, Kerning)`, UTF-16 length), split where those change.
fn pair_runs(ed_text: &str, runs: &[TextRun]) -> Vec<(usize, (bool, i64), usize)> {
    // Model run of each character (the trailing `\r` takes the last run).
    let mut owner: Vec<(usize, usize)> = Vec::with_capacity(ed_text.len());
    let (mut ri, mut run_end) = (0usize, runs.first().map_or(0, |r| r.len));
    for (b, ch) in ed_text.char_indices() {
        while b >= run_end && ri + 1 < runs.len() {
            ri += 1;
            run_end = run_end.saturating_add(runs.get(ri).map_or(0, |r| r.len));
        }
        owner.push((ri, ch.len_utf16()));
    }
    let style = |i: usize| owner.get(i).and_then(|o| runs.get(o.0)).map(|r| &r.style);
    let mut out: Vec<(usize, (bool, i64), usize)> = Vec::new();
    for (j, &(ri, n)) in owner.iter().enumerate() {
        let k = j.checked_sub(1).and_then(style).map_or(0, |s| kern_units(s.kern));
        // `false` marks character j - 2 manual: written when it is and its own kern (on j - 1)
        // doesn't say so already; harmless (and Photoshop's own form) next to a kern on j.
        let off = match j.checked_sub(2) {
            Some(p) => style(p).is_some_and(|s| manual(s) && (kern_units(s.kern) == 0 || k != 0)),
            None => k != 0,
        };
        let fields = (!off, k);
        match out.last_mut() {
            Some(last) if last.0 == ri && last.1 == fields => last.2 += n,
            _ => out.push((ri, fields, n)),
        }
    }
    out
}

/// Reads EngineData's kerning fields back into the model (see the notes above). `incoming` is
/// per EngineData style run: (UTF-8 length in the engine text, `AutoKerning`, `Kerning`).
fn kerning_from_pairs(text: &str, runs: Vec<TextRun>, incoming: &[(usize, bool, f32)]) -> Vec<TextRun> {
    if text.is_empty() || runs.is_empty() {
        return runs;
    }
    // Byte offset of every character, plus the trailing break.
    let starts: Vec<usize> = text.char_indices().map(|(b, _)| b).chain(std::iter::once(text.len())).collect();
    let n = starts.len().saturating_sub(1);
    let mut kern = vec![0.0f32; n];
    let mut off = vec![false; n];
    let (mut si, mut seg_end) = (0usize, incoming.first().map_or(0, |s| s.0));
    for (j, &b) in starts.iter().enumerate() {
        while b >= seg_end && si < incoming.len() {
            si += 1;
            seg_end = seg_end.saturating_add(incoming.get(si).map_or(0, |s| s.0));
        }
        let Some(&(_, auto, k)) = incoming.get(si) else { break };
        if k != 0.0
            && let Some(p) = j.checked_sub(1)
            && let (Some(kp), Some(op)) = (kern.get_mut(p), off.get_mut(p))
        {
            *kp = k;
            *op = true;
        }
        if !auto && let Some(op) = j.checked_sub(2).and_then(|p| off.get_mut(p)) {
            *op = true;
        }
    }
    let (mut ri, mut run_end) = (0usize, runs.first().map_or(0, |r| r.len));
    let mut out: Vec<TextRun> = Vec::new();
    for (c, w) in starts.windows(2).enumerate() {
        let (b, len) = (w[0], w[1] - w[0]);
        while b >= run_end && ri + 1 < runs.len() {
            ri += 1;
            run_end = run_end.saturating_add(runs.get(ri).map_or(0, |r| r.len));
        }
        let Some(r) = runs.get(ri) else { break };
        let k = kern.get(c).copied().unwrap_or(0.0);
        let kerning = if off.get(c).copied().unwrap_or(false) { Kerning::Off } else { Kerning::Metrics };
        match out.last_mut() {
            Some(last) if last.style.kern == k && last.style.kerning == kerning && same_but_kerning(&last.style, &r.style) => last.len += len,
            _ => {
                let mut style = r.style.clone();
                style.kerning = kerning;
                style.kern = k;
                out.push(TextRun { len, style });
            }
        }
    }
    out
}

/// Whether two styles differ at most in their kerning.
fn same_but_kerning(a: &CharStyle, b: &CharStyle) -> bool {
    CharStyle { kerning: b.kerning, kern: b.kern, ..a.clone() } == *b
}

/// PostScript name for a style (the PSD `FontSet` stores these).
pub(crate) fn postscript_for(s: &CharStyle) -> String {
    if let Some(ps) = &s.postscript_name {
        return ps.clone();
    }
    let fam = if s.font_family.is_empty() { crate::fonts::DEFAULT_FAMILY } else { &s.font_family };
    let w = match s.weight {
        0..=150 => "Thin",
        151..=250 => "ExtraLight",
        251..=350 => "Light",
        351..=450 => "Regular",
        451..=550 => "Medium",
        551..=650 => "SemiBold",
        651..=750 => "Bold",
        751..=850 => "ExtraBold",
        _ => "Black",
    };
    let style = match (w, s.italic) {
        ("Regular", true) => "Italic".to_string(),
        (w, true) => format!("{w}Italic"),
        (w, false) => w.to_string(),
    };
    format!("{}-{style}", fam.replace(' ', ""))
}

/// EngineData style keys for the Type › OpenType toggles and the OpenType feature each one turns
/// on (the standard and discretionary ligature keys map to dedicated [`CharStyle`] fields).
pub const OPENTYPE_KEYS: [(&str, &str); 8] = [
    ("ContextualLigatures", "calt"),
    ("Swash", "swsh"),
    ("OldStyle", "onum"),
    ("StylisticAlternates", "salt"),
    ("Titling", "titl"),
    ("Ornaments", "ornm"),
    ("Ordinals", "ordn"),
    ("Fractions", "frac"),
];

pub(crate) fn style_sheet_data(s: &CharStyle, font: usize, k: f32) -> E {
    let c = &s.color;
    let values = match c.mode {
        ColorMode::Cmyk => vec![real(c.alpha), real(c.c[0]), real(c.c[1]), real(c.c[2]), real(c.c[3])],
        ColorMode::Grayscale => vec![real(c.alpha), real(c.c[0])],
        _ => {
            let [r, g, b] = c.to_rgb();
            vec![real(c.alpha), real(r), real(g), real(b)]
        }
    };
    let color_type = match c.mode {
        ColorMode::Cmyk => 2,
        ColorMode::Grayscale => 0,
        _ => 1,
    };
    let mut opentype: Vec<(String, E)> = OPENTYPE_KEYS
        .iter()
        .filter(|(_, tag)| s.features.iter().any(|f| f.tag == *tag && f.value > 0))
        .map(|(key, _)| (key.to_string(), E::Bool(true)))
        .collect();
    let mut dict = vec![
        ("Font".into(), E::Int(font as i64)),
        ("FontSize".into(), real(s.size_pt / k)),
        ("FauxBold".into(), E::Bool(s.faux_bold)),
        ("FauxItalic".into(), E::Bool(s.faux_italic)),
        ("AutoLeading".into(), E::Bool(s.leading_pt.is_none())),
        // Auto leading keeps whatever `Leading` the run had (Photoshop stores the last manual
        // value there); written only when it is the one in use.
        ("HorizontalScale".into(), real(s.horizontal_scale)),
        ("VerticalScale".into(), real(s.vertical_scale)),
        ("Tracking".into(), E::Int(s.tracking.round() as i64)),
        ("AutoKerning".into(), E::Bool(s.kerning != Kerning::Off)),
        ("Kerning".into(), E::Int(kern_units(s.kern))),
        ("BaselineShift".into(), real(s.baseline_shift_pt / k)),
        (
            "FontCaps".into(),
            E::Int(match s.caps {
                Caps::Normal => 0,
                Caps::SmallCaps => 1,
                Caps::AllCaps => 2,
            }),
        ),
        (
            "FontBaseline".into(),
            E::Int(match s.position {
                photosplice_doc::text::BaselinePosition::Normal => 0,
                photosplice_doc::text::BaselinePosition::Superscript => 1,
                photosplice_doc::text::BaselinePosition::Subscript => 2,
            }),
        ),
        ("Underline".into(), E::Bool(s.underline)),
        ("Strikethrough".into(), E::Bool(s.strikethrough)),
        ("Ligatures".into(), E::Bool(s.ligatures)),
        ("DLigatures".into(), E::Bool(s.discretionary_ligatures)),
        // Some readers lay out edited text without painting it unless its fill is enabled.
        ("FillFlag".into(), E::Bool(true)),
        ("FillColor".into(), E::Dict(vec![("Type".into(), E::Int(color_type)), ("Values".into(), E::Array(values))])),
        // Some readers lay edited text out without painting it unless its fill is on
        // (storytold/photocraft@7bb971f7).
        ("FillFlag".into(), E::Bool(true)),
    ];
    if let Some(l) = s.leading_pt {
        dict.insert(5, ("Leading".into(), real(l / k)));
    }
    dict.append(&mut opentype);
    E::Dict(dict)
}

pub(crate) fn paragraph_properties(p: &ParagraphStyle, k: f32) -> E {
    let j = match p.align {
        TextAlign::Left => 0,
        TextAlign::Right => 1,
        TextAlign::Center => 2,
        TextAlign::JustifyLeft => 3,
        TextAlign::JustifyRight => 4,
        TextAlign::JustifyCenter => 5,
        TextAlign::JustifyAll => 6,
    };
    E::Dict(vec![
        ("Justification".into(), E::Int(j)),
        ("FirstLineIndent".into(), real(p.first_line_indent_pt / k)),
        ("StartIndent".into(), real(p.start_indent_pt / k)),
        ("EndIndent".into(), real(p.end_indent_pt / k)),
        ("SpaceBefore".into(), real(p.space_before_pt / k)),
        ("SpaceAfter".into(), real(p.space_after_pt / k)),
        ("AutoHyphenate".into(), E::Bool(p.hyphenate)),
        ("AutoLeading".into(), real(p.auto_leading)),
        ("LeadingType".into(), E::Int(0)),
        ("EveryLineComposer".into(), E::Bool(p.every_line_composer)),
    ])
}

fn utf16_len(s: &str) -> i64 {
    s.encode_utf16().count() as i64
}

pub(crate) fn font_entry(name: &str) -> E {
    E::Dict(vec![("Name".into(), E::String(name.into())), ("Script".into(), E::Int(0)), ("FontType".into(), E::Int(1)), ("Synthetic".into(), E::Int(0))])
}

/// The `Txt2` auto-kern mode of a style: 0 manual, 1 metrics, 2 optical — the values
/// [`apply_txt2`] maps back to [`Kerning`]. EngineData's `AutoKerning` can't say Metrics from
/// Optical, so the mode only lives in `Txt2`.
fn txt2_mode(s: &CharStyle) -> i64 {
    if manual(s) {
        0
    } else if s.kerning == Kerning::Optical {
        2
    } else {
        1
    }
}

/// `Txt2` style runs for `layer`: `(mode, UTF-16 length)` over the engine text (`\r` breaks and
/// the trailing `\r`, which takes the last run's mode), split where the auto-kern mode changes.
fn txt2_mode_runs(layer: &TextLayer) -> Vec<(i64, usize)> {
    let runs = layer.char_runs();
    let ed_text = format!("{}\r", layer.text.replace('\n', "\r"));
    let mut out: Vec<(i64, usize)> = Vec::new();
    let (mut ri, mut run_end) = (0usize, runs.first().map_or(0, |r| r.len));
    for (b, ch) in ed_text.char_indices() {
        while b >= run_end && ri + 1 < runs.len() {
            ri += 1;
            run_end = run_end.saturating_add(runs.get(ri).map_or(0, |r| r.len));
        }
        let mode = runs.get(ri).map_or(1, |r| txt2_mode(&r.style));
        match out.last_mut() {
            Some(last) if last.0 == mode => last.1 += ch.len_utf16(),
            _ => out.push((mode, ch.len_utf16())),
        }
    }
    out
}

/// The highest text-object number honoured here (`TextIndex`, and the slot array [`build_txt2`]
/// sizes). PSD text objects are numbered densely from 0; a crafted file can name object
/// `i32::MAX`, and sizing the array by it would make every save allocate gigabytes.
pub const MAX_TEXT_INDEX: i32 = 0xFFFF;

/// Whether the document `Txt2` block is needed for `objects`: only a non-Metrics auto-kern mode
/// (`Optical`) is exclusive to `Txt2` — EngineData's `AutoKerning` cannot distinguish Metrics from
/// Optical — so a fresh document of ordinary Metrics text has nothing to put in the block.
/// Writing Photoshop's empty `<< /0 << >> >>` stub anyway makes some readers fail
/// (#3514); the export omits the block for such a document.
pub fn needs_txt2(objects: &[(i32, &TextLayer)]) -> bool {
    objects.iter().any(|(_, l)| l.char_runs().iter().any(|r| r.style.kerning == Kerning::Optical))
}

/// The document's `Txt2` block for `objects`: `(TextIndex, layer)` pairs, one text object each
/// at the slot its `TextIndex` names (see [`set_text_index`] and [`text_index`]; unused slots are
/// empty dictionaries), carrying the automatic kerning mode of every pair (`/11`: 0 manual,
/// 1 metrics, 2 optical) — the one thing EngineData can't say (`AutoKerning true` covers Metrics
/// and Optical alike). PSD export writes this block for the type layers it writes `TySh` data
/// for; [`apply_txt2`] reads it back. Numbers outside `0..=`[`MAX_TEXT_INDEX`] are skipped: the
/// slot array is sized by real text objects, never by a file's numbers.
///
/// `previous` is the block this replaces (the file's own). A kept object — one whose text is
/// unchanged — keeps everything the file held beyond the two regenerated keys (its model's text
/// and style runs): Photoshop stores glyph pen positions under `/21 /1` there, and a save with
/// no text edit must not drop them. Its own style runs keep every style key too: only their
/// auto-kern mode is set (split where a mode changes inside a run). A changed text invalidates
/// those extras along with the old runs, so they are dropped rather than left stale. Everything
/// else the file's block held — the other keys beside the text objects, objects no layer names —
/// is kept, and a block whose content comes out unchanged is returned byte for byte (#2374:
/// Photoshop refused files whose `Txt2` an unedited save had stripped).
pub fn build_txt2(objects: &[(i32, &TextLayer)], previous: Option<&[u8]>) -> Vec<u8> {
    let prev = previous.and_then(parse_txt2);
    let prev_objects: &[E] = prev.as_ref().and_then(|p| p.path(&["1", "1"]).and_then(E::as_array)).unwrap_or(&[]);
    let mut slots: Vec<E> = prev_objects.to_vec();
    for (index, layer) in objects {
        if !(0..=MAX_TEXT_INDEX).contains(index) {
            continue;
        }
        let text = format!("{}\r", layer.text.replace('\n', "\r"));
        let modes = txt2_mode_runs(layer);
        let at = *index as usize;
        let kept = prev_objects.get(at).filter(|o| o.get("0").and_then(|m| m.get("0")).and_then(E::as_str) == Some(text.as_str()));
        let mut object = kept.cloned().unwrap_or_else(E::dict);
        let mut model = object.get("0").cloned().filter(|m| matches!(m, E::Dict(_))).unwrap_or_else(E::dict);
        let file_runs = kept.and_then(|_| model.path(&["6", "0"]).and_then(E::as_array)).and_then(|runs| with_modes(runs, &modes));
        let runs = file_runs.unwrap_or_else(|| {
            modes
                .iter()
                .map(|&(mode, len)| {
                    // `<< /0 << /0 << /0 () /6 << /0 0 /11 mode >> >> >> /1 len >>`.
                    let style = E::Dict(vec![("0".into(), E::Int(0)), ("11".into(), E::Int(mode))]);
                    let data = E::Dict(vec![("0".into(), E::String(String::new())), ("6".into(), style)]);
                    E::Dict(vec![("0".into(), E::Dict(vec![("0".into(), data)])), ("1".into(), E::Int(len as i64))])
                })
                .collect()
        });
        model.set("0", E::String(text));
        match model.get_mut("6") {
            Some(style_run @ E::Dict(_)) => style_run.set("0", E::Array(runs)),
            _ => model.set("6", E::Dict(vec![("0".into(), E::Array(runs))])),
        }
        object.set("0", model);
        if slots.len() <= at {
            slots.resize(at + 1, E::dict());
        }
        if let Some(slot) = slots.get_mut(at) {
            *slot = object;
        }
    }
    // The outer dictionary keeps whatever the block held (`/98` version, block extras, …), and
    // `/1` whatever it holds beside the text objects.
    let mut outer = prev.clone().unwrap_or_else(|| E::Dict(vec![("98".into(), E::Dict(vec![("0".into(), E::Int(14))])), ("0".into(), E::dict())]));
    match outer.get_mut("1") {
        Some(objs @ E::Dict(_)) => objs.set("1", E::Array(slots)),
        _ => outer.set("1", E::Dict(vec![("1".into(), E::Array(slots))])),
    }
    // Nothing changed: the file's own bytes (writing them back would re-encode strings and numbers).
    if let (Some(bytes), Some(p)) = (previous, &prev)
        && *p == outer
    {
        return bytes.to_vec();
    }
    match outer {
        E::Dict(items) => ed::write_bare(&items),
        // Unreachable: `parse_txt2` and the fallback above both yield a dictionary.
        _ => ed::write_bare(&[]),
    }
}

/// The file's own `Txt2` style runs `runs` with the auto-kern modes `modes` (`(mode, UTF-16
/// length)`, see [`txt2_mode_runs`]) set at `/0 /0 /6 /11`, keeping every other style key. A
/// run a mode change falls inside is split there. `None` when the runs don't cover the text or
/// lack a style dictionary: the caller writes its own runs instead.
fn with_modes(runs: &[E], modes: &[(i64, usize)]) -> Option<Vec<E>> {
    let lens: Vec<usize> = runs.iter().map(|r| r.get("1").and_then(E::as_i64).and_then(|l| usize::try_from(l).ok())).collect::<Option<_>>()?;
    let total = modes.iter().try_fold(0usize, |a, m| a.checked_add(m.1))?;
    if lens.iter().try_fold(0usize, |a, l| a.checked_add(*l))? != total {
        return None;
    }
    let mut out = Vec::with_capacity(runs.len());
    let (mut mi, mut mode_end) = (0usize, modes.first()?.1);
    let mut at = 0usize;
    for (run, len) in runs.iter().zip(lens) {
        if len == 0 {
            out.push(run.clone());
            continue;
        }
        let end = at + len;
        while at < end {
            while at >= mode_end && mi + 1 < modes.len() {
                mi += 1;
                mode_end = mode_end.saturating_add(modes.get(mi)?.1);
            }
            let piece_end = end.min(mode_end);
            if piece_end <= at {
                return None;
            }
            let mut piece = run.clone();
            if piece_end - at != len {
                piece.set("1", E::Int(i64::try_from(piece_end - at).ok()?));
            }
            let style = piece.get_mut("0").and_then(|v| v.get_mut("0")).and_then(|v| v.get_mut("6")).filter(|v| matches!(v, E::Dict(_)))?;
            let mode = modes.get(mi)?.0;
            // Keep the file's value (and its number type) when the mode is the same.
            if style.get("11").and_then(E::as_i64) != Some(mode) {
                style.set("11", E::Int(mode));
            }
            out.push(piece);
            at = piece_end;
        }
    }
    Some(out)
}

/// Updates (or creates) an EngineData tree for `layer`.
pub fn build_engine_data(layer: &TextLayer, template: Option<E>, dpi: f32) -> E {
    let k = 72.0 / if dpi.is_finite() && dpi > 0.0 { dpi } else { 72.0 };
    let fresh = || E::Dict(vec![("EngineDict".into(), E::dict()), ("ResourceDict".into(), E::dict())]);
    // The template comes from the PSD: it may not be a dictionary or may lack `EngineDict`.
    let mut e = match template {
        Some(t @ E::Dict(_)) => t,
        _ => fresh(),
    };
    if !matches!(e.get("EngineDict"), Some(E::Dict(_))) {
        e.set("EngineDict", E::dict());
    }
    let runs = layer.char_runs();
    let paras = layer.paragraph_runs();
    let text = layer.text.replace('\n', "\r");
    let ed_text = format!("{text}\r");

    // The runs the file had, by the UTF-16 offset where each starts: regenerated runs are merged
    // onto the one they start in, so attributes the model doesn't carry survive an edit.
    let template_runs = |key: &str| -> Vec<(i64, E)> {
        let arr = e.path(&["EngineDict", key, "RunArray"]).and_then(E::as_array).unwrap_or(&[]);
        let lens = e.path(&["EngineDict", key, "RunLengthArray"]).and_then(E::as_array).unwrap_or(&[]);
        let mut at = 0i64;
        arr.iter()
            .zip(lens)
            .map(|(r, n)| {
                let start = at;
                at = at.saturating_add(n.as_i64().unwrap_or(0).max(0));
                (start, r.clone())
            })
            .collect()
    };
    let run_at = |runs: &[(i64, E)], offset: i64| runs.iter().rev().find(|(start, _)| *start <= offset).map(|(_, r)| r.clone());
    let (old_styles, old_paras) = (template_runs("StyleRun"), template_runs("ParagraphRun"));
    let old_fonts: Vec<E> = e.path(&["ResourceDict", "FontSet"]).and_then(E::as_array).map(<[E]>::to_vec).unwrap_or_default();
    // Fonts: keep the template's font list and append new PostScript names.
    let mut fonts: Vec<String> = e
        .path(&["ResourceDict", "FontSet"])
        .and_then(E::as_array)
        .map(|a| a.iter().map(|f| f.get("Name").and_then(E::as_str).unwrap_or("").to_string()).collect())
        .unwrap_or_default();
    let mut font_index = |ps: String| match fonts.iter().position(|f| *f == ps) {
        Some(i) => i,
        None => {
            fonts.push(ps);
            fonts.len() - 1
        }
    };
    let mut sruns = Vec::new();
    let mut slens = Vec::new();
    let mut offset = 0i64;
    for (ri, pair, n) in pair_runs(&ed_text, &runs) {
        let Some(r) = runs.get(ri) else { continue };
        let fi = font_index(postscript_for(&r.style));
        let mut data = style_sheet_data(&r.style, fi, k);
        data.set("AutoKerning", E::Bool(pair.0));
        data.set("Kerning", E::Int(pair.1));
        let run = E::Dict(vec![("StyleSheet".into(), E::Dict(vec![("StyleSheetData".into(), data)]))]);
        let mut run = match run_at(&old_styles, offset) {
            Some(old) => ed::merge(&old, run),
            None => run,
        };
        // OpenType keys are written only when on; one the file had on but the model has off is
        // switched off rather than kept from the merge.
        if let Some(data) = run.get_mut("StyleSheet").and_then(|s| s.get_mut("StyleSheetData")) {
            for (key, tag) in OPENTYPE_KEYS {
                if data.get(key).is_some() && !r.style.features.iter().any(|f| f.tag == tag && f.value > 0) {
                    data.set(key, E::Bool(false));
                }
            }
        }
        sruns.push(run);
        slens.push(E::Int(n as i64));
        offset = offset.saturating_add(n as i64);
    }
    let mut pruns = Vec::new();
    let mut plens = Vec::new();
    let mut at = 0usize;
    for (i, p) in paras.iter().enumerate() {
        let piece = &text[at..at + p.len];
        at += p.len;
        let mut n = utf16_len(piece);
        if i + 1 == paras.len() {
            n += 1;
        }
        // The model has no leading type: a run keeps the one it was read with (older files use
        // 1, top-to-top), else the normal sheet's.
        let mut props = paragraph_properties(&p.style, k);
        if let E::Dict(d) = &mut props {
            d.retain(|(key, _)| key != "LeadingType");
        }
        let run = E::Dict(vec![("ParagraphSheet".into(), E::Dict(vec![("DefaultStyleSheet".into(), E::Int(0)), ("Properties".into(), props)]))]);
        let start = plens.iter().filter_map(E::as_i64).sum::<i64>();
        pruns.push(match run_at(&old_paras, start) {
            Some(old) => ed::merge(&old, run),
            None => run,
        });
        plens.push(E::Int(n));
    }
    // Fonts the file had keep their entry (script, font type, synthetic flag).
    let fontset = E::Array(
        fonts
            .iter()
            .map(|f| old_fonts.iter().find(|o| o.get("Name").and_then(E::as_str) == Some(f.as_str())).cloned().unwrap_or_else(|| font_entry(f)))
            .collect(),
    );

    let Some(dict) = e.get_mut("EngineDict") else {
        return fresh();
    };
    let mut editor = dict.get("Editor").cloned().unwrap_or_else(E::dict);
    editor.set("Text", E::String(ed_text));
    dict.set("Editor", editor);
    let mut pr = dict.get("ParagraphRun").cloned().unwrap_or_else(E::dict);
    pr.set("RunArray", E::Array(pruns));
    pr.set("RunLengthArray", E::Array(plens));
    if pr.get("IsJoinable").is_none() {
        pr.set("IsJoinable", E::Int(1));
    }
    dict.set("ParagraphRun", pr);
    let mut sr = dict.get("StyleRun").cloned().unwrap_or_else(E::dict);
    sr.set("RunArray", E::Array(sruns));
    sr.set("RunLengthArray", E::Array(slens));
    if sr.get("IsJoinable").is_none() {
        sr.set("IsJoinable", E::Int(2));
    }
    dict.set("StyleRun", sr);
    dict.set(
        "AntiAlias",
        E::Int(match layer.antialias {
            // As Photoshop numbers them (every type layer of the oracle corpus: `AnCr` 1,
            // `AnSt` 2, `AnSm` 3, `antiAliasSharp` 4, `antiAliasPlatformLCD` 6).
            AntiAlias::None => 0,
            AntiAlias::Crisp => 1,
            AntiAlias::Strong => 2,
            AntiAlias::Smooth => 3,
            AntiAlias::Sharp => 4,
            AntiAlias::Windows => 5,
            AntiAlias::WindowsLcd => 6,
        }),
    );
    if dict.get("UseFractionalGlyphWidths").is_none() {
        dict.set("UseFractionalGlyphWidths", E::Bool(true));
    }
    let (shape_type, box_bounds) = match layer.shape {
        // PSD path type is not written yet: the layer saves as point text.
        TextShape::Point | TextShape::Path { .. } => (0, None),
        TextShape::Box { x, y, width, height } => (1, Some([x, y, x + width, y + height])),
    };
    let mut photoshop = E::Dict(vec![("ShapeType".into(), E::Int(shape_type))]);
    match box_bounds {
        None => photoshop.set("PointBase", E::Array(vec![real(0.0), real(0.0)])),
        Some(b) => photoshop.set("BoxBounds", E::Array(b.iter().map(|v| real(*v)).collect())),
    }
    photoshop.set(
        "Base",
        E::Dict(vec![
            ("ShapeType".into(), E::Int(shape_type)),
            ("TransformPoint0".into(), E::Array(vec![real(1.0), real(0.0)])),
            ("TransformPoint1".into(), E::Array(vec![real(0.0), real(1.0)])),
            ("TransformPoint2".into(), E::Array(vec![real(0.0), real(0.0)])),
        ]),
    );
    // EngineData writing direction and procession, as Photoshop writes vertical type: 2 and 1
    // (horizontal: 0 and 0). The descriptor's `Ornt` agrees.
    let writing = if layer.orientation == Orientation::Vertical { 2 } else { 0 };
    let child = E::Dict(vec![
        ("ShapeType".into(), E::Int(shape_type)),
        ("Procession".into(), E::Int(i64::from(writing == 2))),
        ("Lines".into(), E::Dict(vec![("WritingDirection".into(), E::Int(writing)), ("Children".into(), E::Array(vec![]))])),
        ("Cookie".into(), E::Dict(vec![("Photoshop".into(), photoshop)])),
    ]);
    let rendered = E::Dict(vec![
        ("Version".into(), E::Int(1)),
        ("Shapes".into(), E::Dict(vec![("WritingDirection".into(), E::Int(writing)), ("Children".into(), E::Array(vec![child]))])),
    ]);
    // Merged onto the file's own when the shape kind is the same, keeping its `Base` transform
    // (an identity `Base` would drop a transformed layer's frame when Photoshop re-lays it out).
    let old_rendered = dict.get("Rendered").cloned();
    let old_child = old_rendered.as_ref().and_then(|r| r.path(&["Shapes", "Children"])).and_then(E::as_array).and_then(<[E]>::first).cloned();
    let same_kind = old_child.as_ref().and_then(|c| c.get("ShapeType")).and_then(E::as_i64) == Some(shape_type);
    let rendered = match old_rendered {
        Some(old) if same_kind => {
            let mut merged = ed::merge(&old, rendered);
            if let Some(base) = old_child.as_ref().and_then(|c| c.path(&["Cookie", "Photoshop", "Base"])).cloned()
                && let Some(E::Array(children)) = merged.get_mut("Shapes").and_then(|s| s.get_mut("Children"))
                && let Some(ps) = children.first_mut().and_then(|c| c.get_mut("Cookie")).and_then(|c| c.get_mut("Photoshop"))
            {
                ps.set("Base", base);
            }
            merged
        }
        _ => rendered,
    };
    dict.set("Rendered", rendered);

    for key in ["ResourceDict", "DocumentResources"] {
        if key == "DocumentResources" && e.get(key).is_none() {
            let rd = e.get("ResourceDict").cloned().unwrap_or_else(E::dict);
            e.set(key, rd);
            continue;
        }
        let Some(rd) = e.get_mut(key) else { continue };
        rd.set("FontSet", fontset.clone());
        if rd.get("StyleSheetSet").is_none() {
            let base = runs.first().map(|r| r.style.clone()).unwrap_or_default();
            rd.set(
                "StyleSheetSet",
                E::Array(vec![E::Dict(vec![("Name".into(), E::String("Normal RGB".into())), ("StyleSheetData".into(), style_sheet_data(&base, 0, k))])]),
            );
        }
        if rd.get("ParagraphSheetSet").is_none() {
            rd.set(
                "ParagraphSheetSet",
                E::Array(vec![E::Dict(vec![
                    ("Name".into(), E::String("Normal RGB".into())),
                    ("DefaultStyleSheet".into(), E::Int(0)),
                    ("Properties".into(), paragraph_properties(&ParagraphStyle::default(), k)),
                ])]),
            );
        }
        for (k2, v) in [("TheNormalStyleSheet", 0), ("TheNormalParagraphSheet", 0)] {
            if rd.get(k2).is_none() {
                rd.set(k2, E::Int(v));
            }
        }
    }
    e
}

/// The anti-aliasing mode for EngineData's `AntiAlias` number, as Photoshop writes it: 0 None,
/// 1 Crisp, 2 Strong, 3 Smooth, 4 Sharp. Other numbers (platform modes) are left to `AntA`.
fn engine_antialias(n: f64) -> Option<AntiAlias> {
    Some(match n as i64 {
        0 => AntiAlias::None,
        1 => AntiAlias::Crisp,
        2 => AntiAlias::Strong,
        3 => AntiAlias::Smooth,
        4 => AntiAlias::Sharp,
        _ => return None,
    })
}

/// Builds the `TySh` block for `layer`, patching its current `psd_raw` when present.
/// `ink` is the layer's rendered bounds in text space (for `bounds`/`boundingBox`).
pub fn build_tysh(layer: &TextLayer, dpi: f32, ink: Option<[f32; 4]>) -> Vec<u8> {
    let old = layer.psd_raw.as_deref().and_then(|d| parse_tysh(d));
    let template = old.as_ref().and_then(|t| engine_data(&t.text));
    let e = build_engine_data(layer, template, dpi);
    let mut text = old.as_ref().map(|t| t.text.clone()).unwrap_or_else(|| Descriptor::new("TxLr"));
    let set = |d: &mut Descriptor, key: &str, v: D| match d.items.iter_mut().find(|(k, _)| k.is(key)) {
        Some(e) => e.1 = v,
        None => d.items.push((Id::new(key), v)),
    };
    set(&mut text, "Txt ", D::Text(UnicodeString::new_nul(&layer.text.replace('\n', "\r"))));
    if text.get("textGridding").is_none() {
        set(&mut text, "textGridding", enumv("textGridding", "None"));
    }
    set(&mut text, "Ornt", enumv("Ornt", if layer.orientation == Orientation::Vertical { "Vrtc" } else { "Hrzn" }));
    set(
        &mut text,
        "AntA",
        enumv(
            "Annt",
            match layer.antialias {
                AntiAlias::None => "Anno",
                AntiAlias::Sharp => "antiAliasSharp",
                AntiAlias::Crisp => "AnCr",
                AntiAlias::Strong => "AnSt",
                AntiAlias::Smooth => "AnSm",
                AntiAlias::Windows => "antiAliasPlatformGray",
                AntiAlias::WindowsLcd => "antiAliasPlatformLCD",
            },
        ),
    );
    // Point text without a layout to measure (a move with a missing font) keeps the bounds it
    // had: they are in text space, and Photoshop lays warped type out against them.
    let old_rect = |key: &str| -> Option<[f32; 4]> {
        let d = match old.as_ref()?.text.get(key)? {
            D::Descriptor(d) => d,
            _ => return None,
        };
        let v = |k: &str| match d.get(k) {
            Some(D::UnitFloat { value, .. } | D::Double(value)) => Some(*value as f32),
            _ => None,
        };
        Some([v("Left")?, v("Top ")?, v("Rght")?, v("Btom")?]).filter(|r| r.iter().any(|x| *x != 0.0))
    };
    let kept = |key: &str| if ink.is_none() && matches!(layer.shape, TextShape::Point) { old_rect(key) } else { None };
    let [l, t, r, b] = match (&layer.shape, ink) {
        (&TextShape::Box { x, y, width, height }, _) => [x, y, x + width, y + height],
        (_, Some(i)) => i,
        _ => kept("bounds").unwrap_or([0.0; 4]),
    };
    let bounding = kept("boundingBox").unwrap_or([l, t, r, b]);
    // Named with one NUL character, as Photoshop writes these rectangles, in the unit the file
    // used (Photoshop writes `#Pxl` or `#Pnt`; both hold the same text-space numbers).
    let rect = |cls: &str, [l, t, r, b]: [f32; 4]| {
        let unit = match old.as_ref().and_then(|o| o.text.get(cls)) {
            Some(D::Descriptor(d)) => match d.get("Left") {
                Some(D::UnitFloat { unit, .. }) => *unit,
                _ => *b"#Pnt",
            },
            _ => *b"#Pnt",
        };
        D::Descriptor(
            Descriptor { name: UnicodeString::new_nul(""), ..Descriptor::new(cls) }
                .with("Left", D::UnitFloat { unit, value: f64::from(l) })
                .with("Top ", D::UnitFloat { unit, value: f64::from(t) })
                .with("Rght", D::UnitFloat { unit, value: f64::from(r) })
                .with("Btom", D::UnitFloat { unit, value: f64::from(b) }),
        )
    };
    set(&mut text, "bounds", rect("bounds", [l, t, r, b]));
    set(&mut text, "boundingBox", rect("boundingBox", bounding));
    if text.get("TextIndex").is_none() {
        set(&mut text, "TextIndex", D::Integer(0));
    }
    set(&mut text, "EngineData", D::RawData(ed::write(&e)));
    let warp = match (&layer.warp, old.as_ref().and_then(|o| o.warp.clone())) {
        (Some(w), _) => Some(
            Descriptor { name: UnicodeString::new_nul(""), ..Descriptor::new("warp") }
                .with("warpStyle", enumv("warpStyle", &w.style))
                .with("warpValue", D::Double(f64::from(w.value)))
                .with("warpPerspective", D::Double(f64::from(w.horizontal_distortion)))
                .with("warpPerspectiveOther", D::Double(f64::from(w.vertical_distortion)))
                .with("warpRotate", enumv("Ornt", if w.horizontal { "Hrzn" } else { "Vrtc" })),
        ),
        (None, Some(old)) if enum_value(&old, "warpStyle").as_deref() == Some("warpNone") => Some(old),
        _ => None,
    };
    write_tysh(&TySh { transform: layer.transform, text, warp, bounds: [l, t, r, b] })
}

#[cfg(test)]
mod nonfinite_tests {
    use super::*;
    use crate::engine_data::parse;

    #[test]
    fn non_finite_style_numbers_fall_back_to_defaults() {
        let d = parse(b"<< /FontSize 1e999 /Tracking nan /HorizontalScale inf /Kerning -inf /FillColor << /Values [1.0 nan 0.0 0.0] >> >>").unwrap();
        let s = char_style(None, &d, &[], 1.0);
        let dflt = CharStyle::default();
        assert_eq!(s.size_pt, dflt.size_pt);
        assert_eq!((s.tracking, s.horizontal_scale, s.kern), (0.0, 1.0, 0.0));
        assert_eq!(s.color, Color::BLACK);
        // A finite f64 too big for f32 is dropped as well.
        let d = parse(b"<< /FontSize 1e300 /StartIndent inf >> ").unwrap();
        assert_eq!(char_style(None, &d, &[], 1.0).size_pt, dflt.size_pt);
        assert_eq!(para_style(None, &d, 1.0).start_indent_pt, 0.0);
        let d = parse(b"<< /FontSize 24.0 >>").unwrap();
        assert_eq!(char_style(None, &d, &[], 1.0).size_pt, 24.0);
    }

    #[test]
    fn infinite_dpi_does_not_write_a_zero_size() {
        let layer =
            TextLayer { text: "x".into(), runs: vec![TextRun { len: 1, style: CharStyle { size_pt: 12.0, ..Default::default() } }], ..Default::default() };
        let out = String::from_utf8_lossy(&crate::engine_data::write(&build_engine_data(&layer, None, f32::INFINITY))).into_owned();
        assert!(out.contains("/FontSize 12.0"), "{out}");
    }
}
