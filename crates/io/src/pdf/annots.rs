//! A page's annotations (ISO 32000 §12.5) as layers of their own: comments (highlights,
//! notes, stamps, drawings, text boxes…) and form fields, each drawn from its appearance stream
//! as the page would show it, named for what it is, and gathered in an "Annotations" and a
//! "Form Fields" group above the page.
//!
//! A text field with a value but no appearance (a form filled by a tool that leaves the reader to
//! draw it, `/NeedAppearances`) gets one made from its value and default appearance (`/DA`),
//! so it opens as a type layer like the others.

use hayro::hayro_syntax::object::{Array, Dict, Name, Object, Rect as PdfRect, Stream, String as PdfString};
use hayro::hayro_syntax::page::Page;
use hayro::kurbo::{self, Affine as KAffine, Shape};

/// At most this many annotations of a page become layers (the rest are left out).
pub(super) const MAX_ANNOTATIONS: usize = 500;

/// Where an annotation's layer goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Comment,
    Field,
}

/// The content an annotation draws: an appearance stream's, or one made for a field's value.
pub(super) enum Content<'a> {
    Stream(Stream<'a>),
    Made(Vec<u8>),
}

/// One annotation to draw.
pub(super) struct Appearance<'a> {
    pub name: String,
    pub kind: Kind,
    pub content: Content<'a>,
    /// Its resources (the form's, the AcroForm's for a made appearance; `None`: the page's).
    pub resources: Option<Dict<'a>>,
    /// Appearance space → page space (the annotation's rectangle mapping times its `/Matrix`).
    pub transform: KAffine,
    /// The appearance's bounding box, in appearance space (it clips).
    pub bbox: kurbo::Rect,
}

fn numbers(a: &Array<'_>) -> Vec<f64> {
    a.iter::<f64>().collect()
}

fn text(s: &PdfString<'_>) -> String {
    super::ocg::text_string(s.as_bytes())
}

/// The normal appearance: the stream, or the one for the annotation's state (`/AS`).
fn appearance<'a>(annot: &Dict<'a>) -> Option<Stream<'a>> {
    match annot.get::<Dict<'_>>(b"AP")?.get::<Object<'_>>(b"N")? {
        Object::Stream(s) => Some(s),
        Object::Dict(states) => annot.get::<Name<'_>>(b"AS").and_then(|st| states.get::<Stream<'_>>(st.as_ref())).or_else(|| states.get::<Stream<'_>>(b"Off")),
        _ => None,
    }
}

/// A field's full name (`parent.child`), from the annotation up its `/Parent`s.
fn field_name(annot: &Dict<'_>) -> Option<String> {
    let mut parts = Vec::new();
    let mut d = Some(annot.clone());
    let mut depth = 0;
    while let Some(cur) = d {
        if let Some(t) = cur.get::<PdfString<'_>>(b"T") {
            parts.push(text(&t));
        }
        depth += 1;
        if depth > 32 {
            break;
        }
        d = cur.get::<Dict<'_>>(b"Parent");
    }
    parts.reverse();
    let name = parts.join(".");
    (!name.trim().is_empty()).then_some(name)
}

/// An inheritable field entry (`/FT`, `/V`, `/DA`, `/Ff`) `get` reads, from the annotation or
/// a parent.
fn inherited<'a, T>(annot: &Dict<'a>, get: impl Fn(&Dict<'a>) -> Option<T>) -> Option<T> {
    let mut d = Some(annot.clone());
    for _ in 0..32 {
        let cur = d?;
        if let Some(v) = get(&cur) {
            return Some(v);
        }
        d = cur.get::<Dict<'_>>(b"Parent");
    }
    None
}

/// What a comment is called: its kind, with the start of its text.
fn comment_name(subtype: &[u8], annot: &Dict<'_>) -> String {
    let kind = match subtype {
        b"Text" => "Note",
        b"FreeText" => "Text Box",
        b"Highlight" => "Highlight",
        b"Underline" => "Underline",
        b"StrikeOut" => "Strikethrough",
        b"Squiggly" => "Squiggly",
        b"Ink" => "Drawing",
        b"Stamp" => "Stamp",
        b"Line" => "Line",
        b"Square" => "Rectangle",
        b"Circle" => "Oval",
        b"Polygon" => "Polygon",
        b"PolyLine" => "Polyline",
        b"Link" => "Link",
        b"FileAttachment" => "Attachment",
        b"Caret" => "Caret",
        b"Redact" => "Redaction",
        b"Watermark" => "Watermark",
        other => return String::from_utf8_lossy(other).into_owned(),
    };
    let words = annot.get::<PdfString<'_>>(b"Contents").map(|s| text(&s)).unwrap_or_default();
    let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
    if words.is_empty() {
        kind.to_string()
    } else {
        let mut short: String = words.chars().take(40).collect();
        if words.chars().count() > 40 {
            short.push('…');
        }
        format!("{kind}: {short}")
    }
}

/// `s` as a PDF literal string.
fn literal(s: &str) -> String {
    let mut out = String::from("(");
    for c in s.chars() {
        match c {
            '(' | ')' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            // PDFDocEncoding for what Latin-1 holds, else a question mark.
            c if u32::from(c) < 256 => out.push(c),
            _ => out.push('?'),
        }
    }
    out.push(')');
    out
}

/// A text field's appearance for `value` in a `w` × `h` box: its default appearance (font,
/// size, colour; an automatic size fits the box), one line per line of the value, left-aligned
/// with the reader's 2-point padding.
fn field_content(value: &str, da: &str, w: f64, h: f64, multiline: bool) -> Vec<u8> {
    // `/Font size Tf`: the size, 0 meaning "fit".
    let words: Vec<&str> = da.split_whitespace().collect();
    let tf = words.iter().position(|w| *w == "Tf");
    let size = tf.and_then(|i| words.get(i.checked_sub(1)?)?.parse::<f64>().ok()).filter(|s| s.is_finite() && *s > 0.0);
    let lines: Vec<&str> = if multiline { value.lines().collect() } else { vec![value.lines().next().unwrap_or_default()] };
    let size = size.unwrap_or(if multiline { 12.0 } else { ((h - 4.0) * 0.8).clamp(4.0, 12.0) });
    let mut da_ops: Vec<String> = words.iter().map(|s| s.to_string()).collect();
    if let Some(i) = tf.and_then(|i| i.checked_sub(1))
        && let Some(slot) = da_ops.get_mut(i)
    {
        *slot = format!("{size}");
    }
    let leading = size * 1.15;
    let first = if multiline { h - 2.0 - size } else { ((h - size) / 2.0 + size * 0.22).max(0.0) };
    let mut s = format!("/Tx BMC q 1 1 {} {} re W n BT {} {leading} TL 2 {first} Td", (w - 2.0).max(0.0), (h - 2.0).max(0.0), da_ops.join(" "));
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            s.push_str(" T*");
        }
        s.push(' ');
        s.push_str(&literal(line));
        s.push_str(" Tj");
    }
    s.push_str(" ET Q EMC");
    s.into_bytes()
}

/// The page's annotations to draw, in order (bottom first), with the AcroForm's resources for
/// made appearances.
pub(super) fn appearances<'a>(page: &Page<'a>) -> Vec<Appearance<'a>> {
    let Some(annots) = page.raw().get::<Array<'_>>(b"Annots") else { return Vec::new() };
    let xref = page.xref();
    let form = xref.get::<Dict<'_>>(xref.root_id()).and_then(|c| c.get::<Dict<'_>>(b"AcroForm"));
    let need = form.as_ref().and_then(|f| f.get::<bool>(b"NeedAppearances")).unwrap_or(false);
    let form_da = form.as_ref().and_then(|f| f.get::<PdfString<'_>>(b"DA")).map(|s| text(&s));
    let form_res = form.as_ref().and_then(|f| f.get::<Dict<'_>>(b"DR"));
    let mut out = Vec::new();
    for annot in annots.iter::<Dict<'_>>().take(MAX_ANNOTATIONS) {
        let flags = annot.get::<u32>(b"F").unwrap_or(0);
        // Hidden, or not shown on screen.
        if flags & 2 != 0 || flags & 32 != 0 {
            continue;
        }
        let subtype = annot.get::<Name<'_>>(b"Subtype").map(|n| n.as_ref().to_vec()).unwrap_or_default();
        // A pop-up only shows its parent's text when opened.
        if subtype == b"Popup" {
            continue;
        }
        let Some(rect) = annot.get::<PdfRect>(b"Rect") else { continue };
        let r = kurbo::Rect::new(rect.x0, rect.y0, rect.x1, rect.y1).abs();
        if r.width() <= 0.0 || r.height() <= 0.0 || !r.is_finite() {
            continue;
        }
        let widget = subtype == b"Widget";
        let kind = if widget { Kind::Field } else { Kind::Comment };
        let name = if widget { field_name(&annot).unwrap_or_else(|| "Form Field".into()) } else { comment_name(&subtype, &annot) };
        let stream = appearance(&annot);
        // A text field with a value the reader is to draw.
        let field_type = widget.then(|| inherited(&annot, |d| d.get::<Name<'_>>(b"FT"))).flatten().map(|n| n.as_ref().to_vec());
        let value = inherited(&annot, |d| d.get::<PdfString<'_>>(b"V")).map(|s| text(&s)).filter(|v| !v.is_empty());
        if field_type.as_deref() == Some(b"Tx")
            && let Some(value) = value.filter(|_| need || stream.is_none())
        {
            let da =
                inherited(&annot, |d| d.get::<PdfString<'_>>(b"DA")).map(|s| text(&s)).or_else(|| form_da.clone()).unwrap_or_else(|| "/Helv 0 Tf 0 g".into());
            let multiline = inherited(&annot, |d| d.get::<u32>(b"Ff")).unwrap_or(0) & (1 << 12) != 0;
            let content = field_content(&value, &da, r.width(), r.height(), multiline);
            out.push(Appearance {
                name,
                kind,
                content: Content::Made(content),
                resources: form_res.clone(),
                transform: KAffine::translate((r.x0, r.y0)),
                bbox: kurbo::Rect::new(0.0, 0.0, r.width(), r.height()),
            });
            continue;
        }
        let Some(stream) = stream else { continue };
        let d = stream.dict();
        let bbox = d.get::<Array<'_>>(b"BBox").map(|a| numbers(&a)).unwrap_or_default();
        let [x0, y0, x1, y1] = bbox.as_slice() else { continue };
        let bbox = kurbo::Rect::new(*x0, *y0, *x1, *y1).abs();
        let m = d.get::<Array<'_>>(b"Matrix").map(|a| numbers(&a)).unwrap_or_default();
        let matrix = match m.as_slice() {
            [a, b, c, d, e, f] => KAffine::new([*a, *b, *c, *d, *e, *f]),
            _ => KAffine::IDENTITY,
        };
        // ISO 32000 §12.5.5: the box, transformed by the matrix, is fitted to the rectangle.
        let t = (matrix * bbox.to_path(0.1)).bounding_box();
        if t.width() <= 0.0 || t.height() <= 0.0 {
            continue;
        }
        let (sx, sy) = (r.width() / t.width(), r.height() / t.height());
        let fit = KAffine::new([sx, 0.0, 0.0, sy, r.x0 - sx * t.x0, r.y0 - sy * t.y0]);
        let resources = d.get::<Dict<'_>>(b"Resources");
        out.push(Appearance { name, kind, content: Content::Stream(stream), resources, transform: fit * matrix, bbox });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_field_value_becomes_text_in_its_appearance() {
        let c = String::from_utf8(field_content("Jo (Ann)", "/Helv 0 Tf 0 0 1 rg", 100.0, 20.0, false)).unwrap();
        assert!(c.contains("(Jo \\(Ann\\)) Tj") && c.contains("0 0 1 rg") && !c.contains(" 0 Tf"), "{c}");
        let m = String::from_utf8(field_content("a\nb", "/Helv 10 Tf 0 g", 100.0, 60.0, true)).unwrap();
        assert!(m.contains("(a) Tj T* (b) Tj") && m.contains("/Helv 10 Tf"), "{m}");
        assert!(literal("日本").contains("??"));
        // No `Tf` at all: still a valid appearance.
        assert!(String::from_utf8(field_content("x", "", 10.0, 10.0, false)).unwrap().contains("(x) Tj"));
    }
}
