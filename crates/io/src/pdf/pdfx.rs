//! PDF/X (ISO 15930), the print exchange standards a saved PDF can conform to: PDF/X-1a:2003
//! (CMYK only, no transparency), PDF/X-3:2003 (colour-managed, no transparency) and PDF/X-4:2010
//! (colour-managed, with transparency and PDF layers).
//!
//! The page is drawn for the standard (see [`super::export`]: flattened and opaque for X-1a and
//! X-3, CMYK for X-1a, ICC-based colour for X-3 and X-4); [`conform`] then adds what the
//! standard asks of the file as an incremental update: the output intent (the printing
//! condition, as an ICC output profile), the Info entries (dates, `/Trapped`, the version), XMP
//! metadata matching them, and the page's trim box.

use std::fmt::Write as _;
use std::sync::Arc;

use photosplice_doc::Document;

use super::export::Compatibility;
use super::update::Update;
use crate::IoError;

/// The PDF/X standard a saved PDF conforms to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Standard {
    #[default]
    None,
    X1a2003,
    X32003,
    X42010,
}

impl Standard {
    /// Every standard: (it, its id in command params, its label in the PDF save options).
    pub const ALL: [(Standard, &'static str, &'static str); 4] = [
        (Standard::None, "none", "None"),
        (Standard::X1a2003, "x1a-2003", "PDF/X-1a:2003"),
        (Standard::X32003, "x3-2003", "PDF/X-3:2003"),
        (Standard::X42010, "x4-2010", "PDF/X-4:2010"),
    ];

    pub fn from_id(id: &str) -> Option<Standard> {
        Self::ALL.iter().find(|(_, i, _)| *i == id).map(|(c, _, _)| *c)
    }

    pub fn id(self) -> &'static str {
        Self::ALL.iter().find(|(c, _, _)| *c == self).map_or("none", |(_, i, _)| i)
    }

    /// The compatibility the standard fixes: PDF 1.4 for the 2003 standards, at most 1.6 for
    /// PDF/X-4 (1.6 itself, which its PDF layers need at least 1.5 of).
    pub fn compatibility(self, chosen: Compatibility) -> Compatibility {
        match self {
            Standard::None => chosen,
            Standard::X1a2003 | Standard::X32003 => Compatibility::Pdf14,
            Standard::X42010 => Compatibility::Pdf16,
        }
    }

    /// Whether the page is one opaque image (the 2003 standards allow no transparency).
    pub fn flattened(self) -> bool {
        matches!(self, Standard::X1a2003 | Standard::X32003)
    }

    /// Whether colour is ICC-based rather than device colour (X-3 and X-4; X-1a allows only
    /// device CMYK).
    pub fn icc_colour(self) -> bool {
        matches!(self, Standard::X32003 | Standard::X42010)
    }

    /// `GTS_PDFXVersion`.
    fn version(self) -> &'static str {
        match self {
            Standard::None => "",
            Standard::X1a2003 => "PDF/X-1a:2003",
            Standard::X32003 => "PDF/X-3:2003",
            Standard::X42010 => "PDF/X-4",
        }
    }
}

/// The printing condition a document is saved for: its own CMYK profile, else the built-in
/// coated CMYK (what PhotoSplice converts to CMYK with). Its bytes and description.
pub(super) fn output_profile(doc: &Document) -> (Arc<Vec<u8>>, String) {
    use photosplice_cms::{ColorSpace, Profile, ProfileClass};
    if doc.mode == photosplice_color::ColorMode::Cmyk
        && let Some(bytes) = &doc.icc_profile
        && let Ok(p) = Profile::parse(bytes)
        && p.color_space == ColorSpace::Cmyk
        && p.class == ProfileClass::Output
    {
        return (bytes.clone(), p.description);
    }
    let p = photosplice_cms::Builtin::CoatedCmyk.profile();
    (p.to_bytes(), p.description.clone())
}

/// A PDF text string: literal when it is printable ASCII, else UTF-16BE in hex.
fn text(s: &str) -> String {
    if s.bytes().all(|b| (0x20..0x7F).contains(&b)) {
        let mut out = String::from("(");
        for c in s.chars() {
            if matches!(c, '(' | ')' | '\\') {
                out.push('\\');
            }
            out.push(c);
        }
        out.push(')');
        out
    } else {
        let hex: String = std::iter::once(0xFEFF).chain(s.encode_utf16()).map(|u| format!("{u:04X}")).collect();
        format!("<{hex}>")
    }
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A random UUID (version 4) for the XMP document and instance ids.
fn uuid() -> String {
    let mut b = [0u8; 16];
    // Without random numbers the ids are still unique enough for one file.
    if getrandom::fill(&mut b).is_err() {
        b = *blake3::hash(format!("{:?}", super::photoshop::now_utc()).as_bytes()).as_bytes().first_chunk().unwrap_or(&[0; 16]);
    }
    b[6] = b[6] & 0x0F | 0x40;
    b[8] = b[8] & 0x3F | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let part = |r: std::ops::Range<usize>| h.get(r).unwrap_or_default().to_string();
    format!("uuid:{}-{}-{}-{}-{}", part(0..8), part(8..12), part(12..16), part(16..20), part(20..32))
}

/// The finished file `pdf` made to conform to `standard`: the output intent for `doc`'s printing
/// condition, Info and XMP metadata, and the trim box.
pub(super) fn conform(pdf: Vec<u8>, doc: &Document, standard: Standard) -> Result<Vec<u8>, IoError> {
    let bad = |why: &str| IoError::Unsupported(format!("PDF: can't make the file {} ({why})", standard.version()));
    if standard == Standard::None {
        return Ok(pdf);
    }
    let mut up = Update::new(pdf)?;
    let (y, mo, d, hh, mi, ss) = super::photoshop::now_utc();
    let pdf_date = format!("D:{y:04}{mo:02}{d:02}{hh:02}{mi:02}{ss:02}Z");
    let xmp_date = format!("{y:04}-{mo:02}-{d:02}T{hh:02}:{mi:02}:{ss:02}Z");
    let title = if doc.name.trim().is_empty() { "Untitled".to_string() } else { doc.name.clone() };
    let version = standard.version();

    // The output intent.
    let (profile, desc) = output_profile(doc);
    let n = match profile.get(16..20) {
        Some(b"GRAY") => 1,
        Some(b"RGB ") => 3,
        _ => 4,
    };
    let icc = up.stream(&format!("/N {n}"), &profile);
    let desc = if desc.trim().is_empty() { "Custom CMYK".to_string() } else { desc };
    let intent = up.object(&format!(
        "<< /Type /OutputIntent /S /GTS_PDFX /OutputConditionIdentifier (Custom) /OutputCondition {} /Info {} /DestOutputProfile {icc} 0 R >>",
        text(&desc),
        text(&desc)
    ));

    // XMP metadata, matching the Info entries.
    let pdfx = match standard {
        Standard::X42010 => format!("   <pdfxid:GTS_PDFXVersion>{version}</pdfxid:GTS_PDFXVersion>\n"),
        Standard::X1a2003 => {
            format!("   <pdfx:GTS_PDFXVersion>{version}</pdfx:GTS_PDFXVersion>\n   <pdfx:GTS_PDFXConformance>{version}</pdfx:GTS_PDFXConformance>\n")
        }
        _ => format!("   <pdfx:GTS_PDFXVersion>{version}</pdfx:GTS_PDFXVersion>\n"),
    };
    let mut x = String::new();
    let _ = write!(
        x,
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n\
 <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n\
  <rdf:Description rdf:about=\"\"\n\
    xmlns:dc=\"http://purl.org/dc/elements/1.1/\"\n\
    xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"\n\
    xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\"\n\
    xmlns:xmpMM=\"http://ns.adobe.com/xap/1.0/mm/\"\n\
    xmlns:pdfx=\"http://ns.adobe.com/pdfx/1.3/\"\n\
    xmlns:pdfxid=\"http://www.npes.org/pdfx/ns/id/\">\n\
   <dc:format>application/pdf</dc:format>\n\
   <dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:title>\n\
   <xmp:CreatorTool>PhotoSplice</xmp:CreatorTool>\n\
   <xmp:CreateDate>{xmp_date}</xmp:CreateDate>\n\
   <xmp:ModifyDate>{xmp_date}</xmp:ModifyDate>\n\
   <xmp:MetadataDate>{xmp_date}</xmp:MetadataDate>\n\
   <pdf:Producer>PhotoSplice</pdf:Producer>\n\
   <pdf:Trapped>False</pdf:Trapped>\n\
   <xmpMM:DocumentID>{}</xmpMM:DocumentID>\n\
   <xmpMM:InstanceID>{}</xmpMM:InstanceID>\n\
   <xmpMM:VersionID>1</xmpMM:VersionID>\n\
   <xmpMM:RenditionClass>default</xmpMM:RenditionClass>\n\
{pdfx}\
  </rdf:Description>\n\
 </rdf:RDF>\n\
</x:xmpmeta>\n\
<?xpacket end=\"w\"?>",
        xml(&title),
        uuid(),
        uuid()
    );
    let metadata = up.raw_stream("/Type /Metadata /Subtype /XML", x.as_bytes());

    // The Info dictionary, in full.
    let info = up.trailer_ref("/Info").ok_or_else(|| bad("no Info dictionary"))?;
    let conformance = if standard == Standard::X1a2003 { format!(" /GTS_PDFXConformance ({version})") } else { String::new() };
    up.replace(
        info,
        &format!(
            "<< /Title {} /Creator (PhotoSplice) /Producer (PhotoSplice) /CreationDate ({pdf_date}) /ModDate ({pdf_date}) /Trapped /False /GTS_PDFXVersion ({version}){conformance} >>",
            text(&title)
        ),
    );

    // The catalog: the output intent and the metadata (in place of the writer's).
    let (catalog, dict) = up.catalog()?;
    let (_, dict) = super::ocg::take_ref(&dict, "/Metadata");
    let body = dict.trim().strip_suffix(">>").ok_or_else(|| bad("the catalog is malformed"))?;
    if body.contains("/OutputIntents") {
        return Err(bad("the file has an output intent already"));
    }
    up.replace(catalog, &format!("{body} /OutputIntents [ {intent} 0 R ] /Metadata {metadata} 0 R >>"));

    // The page: its trim box is its media box.
    let page = up.first_page()?;
    let media = media_box(&page.1).ok_or_else(|| bad("the page has no media box"))?;
    if !page.1.contains("/TrimBox") {
        up.replace_page(page, &format!("/TrimBox {media}"))?;
    }
    up.finish()
}

/// The `/MediaBox [ … ]` array of a page dictionary's text.
fn media_box(dict: &str) -> Option<String> {
    let at = dict.find("/MediaBox")?;
    let rest = dict.get(at + 9..)?;
    let open = rest.find('[')?;
    let close = rest.find(']')?;
    (open < close && rest.get(..open)?.trim().is_empty()).then(|| rest.get(open..=close).map(str::to_string))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standards_round_trip_their_ids() {
        for (s, id, _) in Standard::ALL {
            assert_eq!(Standard::from_id(id), Some(s));
            assert_eq!(s.id(), id);
        }
        assert_eq!(Standard::X42010.compatibility(Compatibility::Pdf20), Compatibility::Pdf16);
        assert_eq!(Standard::X1a2003.compatibility(Compatibility::Pdf17), Compatibility::Pdf14);
    }

    #[test]
    fn strings_and_boxes() {
        assert_eq!(text("a(b)"), "(a\\(b\\))");
        assert_eq!(text("é"), "<FEFF00E9>");
        assert_eq!(media_box("<</Type/Page/MediaBox[0 0 40 30]/Parent 1 0 R>>").as_deref(), Some("[0 0 40 30]"));
        assert_eq!(media_box("<</Type/Page>>"), None);
        assert!(uuid().starts_with("uuid:") && uuid().len() == 41);
    }
}
