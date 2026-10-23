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
use hayro::hayro_syntax::object::ObjectIdentifier;

use crate::IoError;
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
/// Empty when the file has none or more than [`MAX_LAYERS`].
pub(super) fn page_layers(pdf: &Pdf, page: &Page<'_>) -> Vec<PdfLayer> {
    let xref = pdf.xref();
    let Some(catalog) = xref.get::<Dict<'_>>(xref.root_id()) else { return Vec::new() };
    let Some(props) = catalog.get::<Dict<'_>>(b"OCProperties") else { return Vec::new() };
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
        "xref\n0 1\n0000000000 65535 f \n{NUM} 1\n{at:010} 00000 n \ntrailer\n<< /Size {} /Root {NUM} 0 R /Prev {prev}{} >>\nstartxref\n{xref_at}\n%%EOF\n",
        NUM + 1,
        encryption(bytes)
    );
    Some(out)
}

/// A protected file's `/Encrypt` and `/ID` trailer entries (from its newest trailer or
/// cross-reference stream, which are never enciphered), for the appended trailer: the reader
/// takes the encryption from the newest one. The appended catalog has no strings or streams, so
/// it needs no enciphering itself. Empty for a file without them.
fn encryption(bytes: &[u8]) -> String {
    let after = |key: &[u8]| -> Option<&[u8]> {
        let mut end = bytes.len();
        // The last use followed by what the entry needs (not `/IDTree`, say).
        loop {
            let at = bytes.get(..end)?.windows(key.len()).rposition(|w| w == key)?;
            let rest = bytes.get(at + key.len()..)?;
            let rest = rest.get(rest.iter().position(|c| !c.is_ascii_whitespace())?..)?;
            if rest.first().is_some_and(|c| c.is_ascii_digit() || *c == b'[') {
                return Some(rest);
            }
            end = at;
        }
    };
    let mut out = String::new();
    if let Some(rest) = after(b"/Encrypt") {
        let words: Vec<&str> = std::str::from_utf8(rest.get(..rest.len().min(40)).unwrap_or_default()).unwrap_or_default().split_whitespace().take(3).collect();
        if let [n, g, r] = words.as_slice()
            && n.parse::<u32>().is_ok()
            && g.parse::<u16>().is_ok()
            && r.starts_with('R')
        {
            out = format!(" /Encrypt {n} {g} R");
        } else {
            return String::new();
        }
    }
    if !out.is_empty()
        && let Some(rest) = after(b"/ID")
        && rest.first() == Some(&b'[')
        && let Some(end) = rest.iter().take(1024).position(|&c| c == b']')
        && let Ok(id) = std::str::from_utf8(rest.get(..=end).unwrap_or_default())
    {
        out.push_str(" /ID ");
        out.push_str(id);
    }
    out
}

/// The offset the last `startxref` points to.
fn last_startxref(bytes: &[u8]) -> Option<usize> {
    let at = bytes.windows(9).rposition(|w| w == b"startxref")?;
    std::str::from_utf8(bytes.get(at + 9..)?.get(..32.min(bytes.len() - at - 9))?).ok()?.split_whitespace().next()?.parse().ok()
}

// ---- writing ----

/// A PDF layer the exporter marked: its name, and whether the layer is locked.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Mark {
    pub name: String,
    pub locked: bool,
}

/// What the exporter wraps each layer's content in (krilla's untyped artifact).
const MARKER: &[u8] = b"/Artifact BMC";

/// Makes the page's marked sequences (one per [`Mark`], in order) PDF layers, as an incremental
/// update: the layers, the page's content with each marker replaced by `/OC /psN BDC`, its
/// resources naming the layers, and the catalog listing them (top layer first, as the Layers
/// panel shows them; locked ones locked). For the one-page PDF krilla writes.
pub(super) fn add_layers(pdf: Vec<u8>, marks: &[Mark]) -> Result<Vec<u8>, IoError> {
    let bad = |why: &str| IoError::Unsupported(format!("PDF: can't add the PDF layers ({why})"));
    let (content, page_num, page_dict, res_text, catalog_num, catalog_text) = {
        let doc = Pdf::new(pdf.clone()).map_err(|_| bad("the page can't be read"))?;
        let xref = doc.xref();
        let pages = doc.pages();
        let page = pages.first().ok_or_else(|| bad("no page"))?;
        let content = page.page_stream().ok_or_else(|| bad("no content"))?.to_vec();
        let page_id = page.raw().obj_id().ok_or_else(|| bad("the page isn't an object"))?;
        let dict = std::str::from_utf8(page.raw().data()).map_err(|_| bad("the page dictionary"))?;
        let (res, dict) = take_ref(dict, "/Resources");
        let (_, dict) = take_ref(&dict, "/Contents");
        let res = res.ok_or_else(|| bad("the resources aren't an object"))?;
        let resources = xref.get::<hayro::hayro_syntax::object::Dict<'_>>(ObjectIdentifier::new(res.0, res.1)).ok_or_else(|| bad("no resources"))?;
        let res_text = std::str::from_utf8(resources.data()).map_err(|_| bad("the resources"))?.trim().to_string();
        let root = xref.root_id();
        let catalog = xref.get::<hayro::hayro_syntax::object::Dict<'_>>(root).ok_or_else(|| bad("no catalog"))?;
        let catalog_text = std::str::from_utf8(catalog.data()).map_err(|_| bad("the catalog"))?.trim().to_string();
        if res_text.contains("/Properties") || catalog_text.contains("/OCProperties") {
            return Err(bad("the file has some already"));
        }
        let num = |n: i32| usize::try_from(n).map_err(|_| bad("an object number"));
        (content, num(page_id.obj_number)?, dict, res_text, num(root.obj_number)?, catalog_text)
    };
    // Each marker, in order, becomes its layer's.
    let mut out = Vec::with_capacity(content.len() + marks.len() * 8);
    let mut rest = content.as_slice();
    let mut n = 0;
    while let Some(at) = rest.windows(MARKER.len()).position(|w| w == MARKER) {
        out.extend_from_slice(rest.get(..at).unwrap_or_default());
        let _ = write!(out, "/OC /ps{n} BDC");
        n += 1;
        rest = rest.get(at + MARKER.len()..).unwrap_or_default();
    }
    out.extend_from_slice(rest);
    if n != marks.len() {
        return Err(bad("the page's marks don't match the layers"));
    }
    let open = |d: &str| d.strip_suffix(">>").map(str::to_string).ok_or_else(|| bad("a malformed dictionary"));
    let mut up = super::update::Update::new(pdf)?;
    let ocgs: Vec<usize> = marks.iter().map(|m| up.object(&format!("<< /Type /OCG /Name <{}> >>", utf16_hex(&m.name)))).collect();
    let contents = up.stream("", &out);
    let props: String = ocgs.iter().enumerate().map(|(i, o)| format!("/ps{i} {o} 0 R ")).collect();
    let resources = up.object(&format!("{} /Properties << {props}>> >>", open(&res_text)?));
    up.replace_page((page_num, page_dict), &format!("/Resources {resources} 0 R /Contents {contents} 0 R"))?;
    let refs = |ids: &mut dyn Iterator<Item = &usize>| ids.map(|o| format!("{o} 0 R ")).collect::<String>();
    let all = refs(&mut ocgs.iter());
    let order = refs(&mut ocgs.iter().rev());
    let locked = refs(&mut ocgs.iter().zip(marks).filter(|(_, m)| m.locked).map(|(o, _)| o));
    let d = format!("<< /Name (Layers) /BaseState /ON /Order [ {order}] /Locked [ {locked}] >>");
    up.replace(catalog_num, &format!("{} /OCProperties << /OCGs [ {all}] /D {d} >> >>", open(&catalog_text)?));
    up.finish()
}

/// `name` as a PDF text string in hex: UTF-16BE with its byte-order mark.
fn utf16_hex(name: &str) -> String {
    std::iter::once(0xFEFF).chain(name.encode_utf16()).map(|u| format!("{u:04X}")).collect()
}

/// Removes `key` and the reference after it (`n g R`) from a dictionary's text; returns the
/// reference. The text is krilla's (no strings in the page or resources dictionaries).
pub(super) fn take_ref(dict: &str, key: &str) -> (Option<(i32, i32)>, String) {
    let mut from = 0;
    while let Some(at) = dict.get(from..).and_then(|d| d.find(key)).map(|i| i + from) {
        let after = at + key.len();
        // A longer name that starts the same (`/ResourcesX`) isn't this key.
        if dict.get(after..).and_then(|r| r.chars().next()).is_some_and(|c| c.is_ascii_alphanumeric()) {
            from = after;
            continue;
        }
        let tail = dict.get(after..).unwrap_or_default();
        let mut it = tail.split_whitespace();
        let (Some(n), Some(g)) = (it.next(), it.next()) else { return (None, dict.to_string()) };
        let (Ok(n), Ok(g)) = (n.parse::<i32>(), g.parse::<i32>()) else { return (None, dict.to_string()) };
        // Where the `R` ends, in the original text.
        let Some(r) = tail.find('R') else { return (None, dict.to_string()) };
        let end = after + r + 1;
        return (Some((n, g)), format!("{}{}", dict.get(..at).unwrap_or_default(), dict.get(end..).unwrap_or_default()));
    }
    (None, dict.to_string())
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
    fn references_come_out_of_a_dictionary() {
        let d = "<</Type/Page/Resources 2 0 R/MediaBox[0 0 200 100]/Parent 1 0 R/Contents 10 0 R>>";
        let (r, d) = take_ref(d, "/Resources");
        assert_eq!(r, Some((2, 0)));
        let (c, d) = take_ref(&d, "/Contents");
        assert_eq!(c, Some((10, 0)));
        assert_eq!(d, "<</Type/Page/MediaBox[0 0 200 100]/Parent 1 0 R>>");
        assert_eq!(take_ref("<</ResourcesX 3 0 R>>", "/Resources").0, None);
        assert_eq!(take_ref("<</Resources <</Font 1 0 R>> >>", "/Resources").0, None, "inline: not a reference");
        assert_eq!(take_ref("<</Resources", "/Resources").0, None);
        assert_eq!(utf16_hex("Aé"), "FEFF004100E9");
    }

    #[test]
    fn startxref_is_read_from_the_end() {
        assert_eq!(last_startxref(b"%PDF\nstartxref\n12\n%%EOF\nstartxref\n345\n%%EOF"), Some(345));
        assert_eq!(last_startxref(b"startxref"), None);
        assert_eq!(last_startxref(b"nothing"), None);
    }
}
