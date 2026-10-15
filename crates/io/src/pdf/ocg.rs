//! PDF layers (optional content groups, ISO 32000 §8.11): the layers Illustrator, InDesign and
//! Acrobat show, each a named group of the page's content, on or off.
//!
//! hayro-interpret skips the content of layers that are off, and doesn't say which layer it is
//! drawing. So each layer is drawn on its own: [`only`] gives the file again with an appended
//! catalog (an incremental update, ISO 32000 §7.5.6) whose default configuration has just that
//! layer on, and the page drawn from it is that layer's content. These bytes only ever go back
//! into hayro, which reads each cross-reference section by what it finds there, so the update
//! is a classic table whatever the file used.

use std::io::Write as _;

use hayro::hayro_syntax::Pdf;
use hayro::hayro_syntax::content::ops::TypedInstruction;
use hayro::hayro_syntax::object::{Array, Dict, Name};
use hayro::hayro_syntax::page::Page;

/// Above this many layers on a page, it opens as one stack (each layer is a pass over the page).
pub(super) const MAX_LAYERS: usize = 64;

/// An object's number and generation.
pub(super) type Id = (i32, i32);

/// One layer of the page.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PdfLayer {
    pub id: Id,
    pub name: String,
    /// On in the document's default configuration.
    pub visible: bool,
    /// Locked against changes (`/D /Locked`).
    pub locked: bool,
    /// A form or image on the page is in it (its own `/OC` entry, not marked content).
    pub via_form: bool,
}

/// The layers the page draws in, in drawing order (bottom first): the first time each is used
/// by a marked-content sequence (`/OC /name BDC`) or a form or image (`/OC` in its dictionary).
/// Empty when the file has none, is encrypted, or has more than [`MAX_LAYERS`].
pub(super) fn page_layers(pdf: &Pdf, page: &Page<'_>) -> Vec<PdfLayer> {
    let xref = pdf.xref();
    let Some(catalog) = xref.get::<Dict<'_>>(xref.root_id()) else { return Vec::new() };
    let Some(props) = catalog.get::<Dict<'_>>(b"OCProperties") else { return Vec::new() };
    // The appended catalog would have to be encrypted like the rest.
    if is_encrypted(pdf) {
        return Vec::new();
    }
    let config = props.get::<Dict<'_>>(b"D").unwrap_or_default();
    let refs = |key: &[u8]| -> Vec<Id> { config.get::<Array<'_>>(key).map(|a| array_ids(&a)).unwrap_or_default() };
    let all: Vec<Id> = props.get::<Array<'_>>(b"OCGs").map(|a| array_ids(&a)).unwrap_or_default();
    let base_off = config.get::<Name<'_>>(b"BaseState").is_some_and(|n| n.as_ref() == b"OFF");
    let (on, off, locked) = (refs(b"ON"), refs(b"OFF"), refs(b"Locked"));

    let mut order: Vec<Id> = Vec::new();
    let mut via_form: Vec<Id> = Vec::new();
    let resources = page.resources();
    let mut ops = page.typed_operations();
    while let Some(op) = ops.next() {
        let mut form = false;
        let used = match op {
            TypedInstruction::BeginMarkedContentWithProperties(bdc) if bdc.0.as_ref() == b"OC" => bdc
                .1
                .clone()
                .into_name()
                .and_then(|name| resources.properties.get_ref(name.as_ref()))
                .map(|r| members(pdf, (r.obj_number, r.gen_number))),
            TypedInstruction::XObject(x) => {
                form = true;
                resources.get_x_object(x.0).and_then(|s| s.dict().get_ref(b"OC")).map(|r| members(pdf, (r.obj_number, r.gen_number)))
            }
            _ => None,
        };
        for id in used.unwrap_or_default() {
            if form && !via_form.contains(&id) {
                via_form.push(id);
            }
            if all.contains(&id) && !order.contains(&id) {
                order.push(id);
            }
        }
        if order.len() > MAX_LAYERS {
            return Vec::new();
        }
    }
    order
        .into_iter()
        .map(|id| {
            let name = xref
                .get::<Dict<'_>>(hayro::hayro_syntax::object::ObjectIdentifier::new(id.0, id.1))
                .and_then(|d| d.get::<hayro::hayro_syntax::object::String<'_>>(b"Name"))
                .map(|s| text_string(s.as_bytes()))
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "Layer".to_string());
            let visible = if base_off { on.contains(&id) } else { !off.contains(&id) };
            PdfLayer { id, name, visible, locked: locked.contains(&id), via_form: via_form.contains(&id) }
        })
        .collect()
}

/// The layers an `/OC` entry names: itself when it is a layer, the layers a membership
/// dictionary (`/Type /OCMD`) combines.
fn members(pdf: &Pdf, id: Id) -> Vec<Id> {
    let d = pdf.xref().get::<Dict<'_>>(hayro::hayro_syntax::object::ObjectIdentifier::new(id.0, id.1));
    match d {
        Some(d) if d.get::<Name<'_>>(b"Type").is_some_and(|t| t.as_ref() == b"OCMD") => {
            if let Some(a) = d.get::<Array<'_>>(b"OCGs") {
                array_ids(&a)
            } else {
                d.get_ref(b"OCGs").map(|r| vec![(r.obj_number, r.gen_number)]).unwrap_or_default()
            }
        }
        _ => vec![id],
    }
}

fn array_ids(a: &Array<'_>) -> Vec<Id> {
    a.raw_iter().filter_map(|o| o.as_obj_ref()).map(|r| (r.obj_number, r.gen_number)).collect()
}

fn is_encrypted(pdf: &Pdf) -> bool {
    pdf.data().as_ref().windows(8).any(|w| w == b"/Encrypt")
}

/// A PDF text string: UTF-16BE or UTF-8 with a byte-order mark (and UTF-16LE, which some
/// writers use though the standard doesn't allow it), else PDFDocEncoding (taken as Latin-1,
/// which it matches for letters). Trailing NULs, which some writers add, are dropped.
fn text_string(b: &[u8]) -> String {
    let s = if let Some(rest) = b.strip_prefix(&[0xFE, 0xFF]) {
        String::from_utf16_lossy(&rest.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect::<Vec<_>>())
    } else if let Some(rest) = b.strip_prefix(&[0xFF, 0xFE]) {
        String::from_utf16_lossy(&rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>())
    } else if let Some(rest) = b.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(rest).into_owned()
    } else {
        b.iter().map(|&c| char::from(c)).collect()
    };
    s.trim_end_matches('\0').to_string()
}

/// `bytes` with an appended catalog whose default configuration shows only layer `on` (none:
/// just the content outside every layer). `None` when the file's trailer can't be found.
pub(super) fn only(bytes: &[u8], pdf: &Pdf, layers: &[PdfLayer], on: Option<Id>) -> Option<Vec<u8>> {
    let xref = pdf.xref();
    let catalog = xref.get::<Dict<'_>>(xref.root_id())?;
    let pages = catalog.get_ref(b"Pages")?;
    let prev = last_startxref(bytes)?;
    let refs: String = layers.iter().map(|l| format!("{} {} R ", l.id.0, l.id.1)).collect();
    let on = on.map(|(n, g)| format!("{n} {g} R")).unwrap_or_default();
    // Far above any object number a real file uses; hayro doesn't check `/Size`.
    const NUM: i32 = 0x3FFF_FFF0;
    let mut out = bytes.to_vec();
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    let at = out.len();
    let _ = write!(
        out,
        "{NUM} 0 obj\n<< /Type /Catalog /Pages {} {} R /OCProperties << /OCGs [ {refs}] /D << /BaseState /OFF /ON [ {on} ] >> >> >>\nendobj\n",
        pages.obj_number, pages.gen_number
    );
    let xref_at = out.len();
    let _ = write!(
        out,
        "xref\n0 1\n0000000000 65535 f \n{NUM} 1\n{at:010} 00000 n \ntrailer\n<< /Size {} /Root {NUM} 0 R /Prev {prev} >>\nstartxref\n{xref_at}\n%%EOF\n",
        NUM + 1
    );
    Some(out)
}

/// The offset the last `startxref` points to.
fn last_startxref(bytes: &[u8]) -> Option<usize> {
    let at = bytes.windows(9).rposition(|w| w == b"startxref")?;
    std::str::from_utf8(bytes.get(at + 9..)?.get(..32.min(bytes.len() - at - 9))?).ok()?.split_whitespace().next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_strings_decode() {
        assert_eq!(text_string(b"Background"), "Background");
        assert_eq!(text_string(&[0xFE, 0xFF, 0, b'A', 0x00, 0xE9]), "Aé");
        assert_eq!(text_string(&[0xFE, 0xFF, 0]), "", "an odd byte is dropped, not a panic");
        assert_eq!(text_string(&[0xEF, 0xBB, 0xBF, b'x']), "x");
        assert_eq!(text_string(&[0xFF, 0xFE, b'L', 0, b'a', 0, 0, 0]), "La", "UTF-16LE, its NUL dropped");
    }

    #[test]
    fn startxref_is_read_from_the_end() {
        assert_eq!(last_startxref(b"%PDF\nstartxref\n12\n%%EOF\nstartxref\n345\n%%EOF"), Some(345));
        assert_eq!(last_startxref(b"startxref"), None);
        assert_eq!(last_startxref(b"nothing"), None);
    }
}
