//! Incremental updates (ISO 32000 §7.5.6): objects appended to a PDF after its end, with a new
//! cross-reference section that points back to the old one, as Acrobat saves edits. Used to add
//! the Photoshop editing data and the PDF layers to the page krilla writes.

use std::io::Write as _;

use hayro::hayro_syntax::Pdf;

use crate::IoError;

/// Removes `key` and the number after it from a dictionary's entries (outside strings, so a key's
/// spelling inside a string is left alone); returns the number.
pub(super) fn take_number(dict: &str, key: &str) -> (Option<usize>, String) {
    let b = dict.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' if depth > 0 => i += 1,
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            b'/' if depth == 0 && b[i..].starts_with(key.as_bytes()) && !b.get(i + key.len()).is_some_and(u8::is_ascii_alphanumeric) => {
                let rest = &dict[i + key.len()..];
                let num = rest.trim_start();
                let digits = num.bytes().take_while(u8::is_ascii_digit).count();
                let end = dict.len() - num.len() + digits;
                return (num[..digits].parse().ok(), format!("{} {}", &dict[..i], &dict[end..]));
            }
            _ => {}
        }
        i += 1;
    }
    (None, dict.to_string())
}

/// An incremental update appended to a PDF with a classic cross-reference table (what krilla's
/// pdf-writer produces).
pub(super) struct Update {
    out: Vec<u8>,
    /// The original's cross-reference offset and trailer entries other than `/Size`.
    prev: usize,
    trailer: String,
    next: usize,
    entries: Vec<(usize, usize)>,
}

impl Update {
    pub(super) fn new(pdf: Vec<u8>) -> Result<Self, IoError> {
        let bad = || IoError::Unsupported("PDF: can't add the Photoshop editing data to this file".into());
        let find_last = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).rposition(|w| w == needle);
        let sx = find_last(&pdf, b"startxref").ok_or_else(bad)?;
        let prev: usize = std::str::from_utf8(pdf.get(sx + 9..).unwrap_or_default())
            .ok()
            .and_then(|s| s.split_whitespace().next())
            .and_then(|s| s.parse().ok())
            .ok_or_else(bad)?;
        let tr = find_last(pdf.get(..sx).unwrap_or_default(), b"trailer").ok_or_else(bad)?;
        let dict = std::str::from_utf8(pdf.get(tr + 7..sx).unwrap_or_default()).map_err(|_| bad())?.trim();
        let dict = dict.strip_prefix("<<").and_then(|d| d.strip_suffix(">>")).ok_or_else(bad)?;
        // Drop `/Size n` (rewritten) and any `/Prev n`; keep `/Root`, `/Info`, `/ID`.
        let (size, dict) = take_number(dict, "/Size");
        let (_, dict) = take_number(&dict, "/Prev");
        let next = size.ok_or_else(bad)?;
        let trailer = dict.trim().to_string();
        let mut out = pdf;
        if !out.ends_with(b"\n") {
            out.push(b'\n');
        }
        Ok(Update { out, prev, trailer, next, entries: Vec::new() })
    }

    /// The first page's object number and its dictionary (krilla writes one page).
    pub(super) fn first_page(&self) -> Result<(usize, String), IoError> {
        let bad = || IoError::Unsupported("PDF: can't find the page to add the Photoshop editing data to".into());
        let pdf = Pdf::new(self.out.clone()).map_err(|_| bad())?;
        let pages = pdf.pages();
        let page = pages.first().ok_or_else(bad)?;
        let id = page.raw().obj_id().ok_or_else(bad)?;
        let num = usize::try_from(id.obj_number).map_err(|_| bad())?;
        let dict = std::str::from_utf8(page.raw().data()).map_err(|_| bad())?.trim().to_string();
        Ok((num, dict))
    }

    pub(super) fn begin(&mut self, num: usize) {
        self.entries.push((num, self.out.len()));
        let _ = writeln!(self.out, "{num} 0 obj");
    }

    pub(super) fn object(&mut self, body: &str) -> usize {
        let num = self.next;
        self.next += 1;
        self.begin(num);
        self.out.extend_from_slice(body.as_bytes());
        self.out.extend_from_slice(b"\nendobj\n");
        num
    }

    /// A Flate-compressed stream with the dictionary entries `dict`.
    pub(super) fn stream(&mut self, dict: &str, data: &[u8]) -> usize {
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        let data = match z.write_all(data).and_then(|()| z.finish()) {
            Ok(d) => d,
            Err(_) => return self.object("null"),
        };
        let num = self.next;
        self.next += 1;
        self.begin(num);
        let _ = write!(self.out, "<< {dict} /Filter /FlateDecode /Length {} >>\nstream\n", data.len());
        self.out.extend_from_slice(&data);
        self.out.extend_from_slice(b"\nendstream\nendobj\n");
        num
    }

    /// An unfiltered stream (XMP metadata, which readers look for as plain text).
    pub(super) fn raw_stream(&mut self, dict: &str, data: &[u8]) -> usize {
        let num = self.next;
        self.next += 1;
        self.begin(num);
        let _ = write!(self.out, "<< {dict} /Length {} >>\nstream\n", data.len());
        self.out.extend_from_slice(data);
        self.out.extend_from_slice(b"\nendstream\nendobj\n");
        num
    }

    /// The object number the trailer's `key` (`/Info`, `/Root`) refers to.
    pub(super) fn trailer_ref(&self, key: &str) -> Option<usize> {
        take_number(&self.trailer, key).0
    }

    /// The catalog's object number and its dictionary.
    pub(super) fn catalog(&self) -> Result<(usize, String), IoError> {
        let bad = || IoError::Unsupported("PDF: can't find the document catalog".into());
        let pdf = Pdf::new(self.out.clone()).map_err(|_| bad())?;
        let xref = pdf.xref();
        let root = xref.root_id();
        let catalog = xref.get::<hayro::hayro_syntax::object::Dict<'_>>(root).ok_or_else(bad)?;
        let text = std::str::from_utf8(catalog.data()).map_err(|_| bad())?.trim().to_string();
        Ok((usize::try_from(root.obj_number).map_err(|_| bad())?, text))
    }

    /// Writes object `num` again as `body`.
    pub(super) fn replace(&mut self, num: usize, body: &str) {
        self.begin(num);
        self.out.extend_from_slice(body.as_bytes());
        self.out.extend_from_slice(b"\nendobj\n");
    }

    /// Writes page `num` again with `entries` added to its dictionary.
    pub(super) fn replace_page(&mut self, (num, dict): (usize, String), entries: &str) -> Result<(), IoError> {
        let body = dict.strip_suffix(">>").ok_or_else(|| IoError::Unsupported("PDF: the page dictionary is malformed".into()))?;
        let page = format!("{body} {entries} >>");
        self.begin(num);
        self.out.extend_from_slice(page.as_bytes());
        self.out.extend_from_slice(b"\nendobj\n");
        Ok(())
    }

    pub(super) fn finish(mut self) -> Result<Vec<u8>, IoError> {
        let xref = self.out.len();
        self.entries.sort_unstable();
        self.out.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
        // One subsection per run of consecutive object numbers.
        let mut i = 0;
        while i < self.entries.len() {
            let mut j = i + 1;
            while j < self.entries.len() && self.entries[j].0 == self.entries[j - 1].0 + 1 {
                j += 1;
            }
            let _ = writeln!(self.out, "{} {}", self.entries[i].0, j - i);
            for (_, off) in &self.entries[i..j] {
                let _ = writeln!(self.out, "{off:010} 00000 n ");
            }
            i = j;
        }
        let _ = write!(self.out, "trailer\n<< /Size {} /Prev {} {} >>\nstartxref\n{xref}\n%%EOF\n", self.next, self.prev, self.trailer);
        Ok(self.out)
    }
}
