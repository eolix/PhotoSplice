//! XML parsing with a nesting cap. roxmltree recurses once per element level, so a few thousand
//! nested tags (a 30 KB file or command param) overflow a 2 MB thread stack, an abort no guard
//! can catch. Every roxmltree caller that reads a file or a param goes through [`parse`]
//! (storytold/photocraft@027bfb30).

use roxmltree::{Document, ParsingOptions};

/// Deepest element nesting accepted, the same as for SVG drawings: real files stay far below.
pub const MAX_DEPTH: usize = crate::svg::MAX_XML_DEPTH;

/// Parses `text`, refusing nesting deeper than [`MAX_DEPTH`] before roxmltree sees it.
///
/// With `opts.allow_dtd` an internal DTD subset is refused too: its entities can expand into
/// markup the depth check never sees. An external DOCTYPE (a public or system id) still parses.
pub fn parse(text: &str, opts: ParsingOptions) -> Result<Document<'_>, String> {
    if opts.allow_dtd && internal_subset(text.as_bytes()) {
        return Err("internal DTD subsets aren't supported".into());
    }
    if crate::svg::nesting_depth(text.as_bytes(), MAX_DEPTH) > MAX_DEPTH {
        return Err(format!("elements are nested more than {MAX_DEPTH} levels deep"));
    }
    Document::parse_with_options(text, opts).map_err(|e| e.to_string())
}

/// Does the prolog's DOCTYPE have an internal subset (`<!DOCTYPE x [ … ]>`)? Only the prolog can
/// hold a DOCTYPE; the parser rejects one anywhere else.
fn internal_subset(xml: &[u8]) -> bool {
    let mut rest = xml.strip_prefix("\u{feff}".as_bytes()).unwrap_or(xml);
    loop {
        rest = rest.trim_ascii_start();
        let skip = |rest: &[u8], open: usize, close: &[u8]| -> Option<usize> {
            let body = rest.get(open..)?;
            body.windows(close.len()).position(|w| w == close).map(|p| open.saturating_add(p).saturating_add(close.len()))
        };
        let next = if rest.starts_with(b"<?") {
            skip(rest, 2, b"?>")
        } else if rest.starts_with(b"<!--") {
            skip(rest, 4, b"-->")
        } else if rest.starts_with(b"<!DOCTYPE") {
            // The name and the quoted public/system ids come first; a `[` outside quotes opens
            // the subset, a `>` ends a DOCTYPE without one.
            let mut quote = None;
            for &b in rest.iter().skip(9) {
                match quote {
                    Some(q) if b == q => quote = None,
                    Some(_) => {}
                    None if b == b'"' || b == b'\'' => quote = Some(b),
                    None if b == b'[' => return true,
                    None if b == b'>' => return false,
                    None => {}
                }
            }
            return false;
        } else {
            return false;
        };
        let Some(next) = next else { return false };
        rest = rest.get(next..).unwrap_or_default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nested(levels: usize) -> String {
        format!("<a>{}{}</a>", "<g>".repeat(levels), "</g>".repeat(levels))
    }

    fn dtd() -> ParsingOptions {
        ParsingOptions { allow_dtd: true, ..Default::default() }
    }

    #[test]
    fn nesting_past_the_cap_is_an_error_not_a_stack_overflow() {
        let deep = nested(20_000);
        std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || {
                for opts in [ParsingOptions::default(), dtd()] {
                    let e = parse(&deep, opts).err().unwrap_or_else(|| panic!("parsed"));
                    assert!(e.contains("nested") && e.contains(&MAX_DEPTH.to_string()), "{e}");
                }
            })
            .unwrap()
            .join()
            .unwrap();
        // The root plus MAX_DEPTH - 1 levels is exactly MAX_DEPTH deep.
        assert!(parse(&nested(MAX_DEPTH - 1), ParsingOptions::default()).is_ok());
        assert!(parse(&nested(MAX_DEPTH), ParsingOptions::default()).is_err());
    }

    #[test]
    fn markup_in_comments_cdata_pis_and_attributes_adds_no_depth() {
        let g = "<g>".repeat(2 * MAX_DEPTH);
        let xml = format!("<?xml version=\"1.0\"?><!-- {g} --><a t=\"&lt;&gt; /&gt; >\"><![CDATA[ {g} ]]><?pi {g} ?><b/></a>");
        assert!(parse(&xml, ParsingOptions::default()).is_ok());
    }

    #[test]
    fn an_internal_subset_is_refused_an_external_doctype_parses() {
        let krita = "<?xml version=\"1.0\"?>\n<!-- c --><!DOCTYPE DOC PUBLIC '-//KDE//DTD krita 2.0//EN' 'http://www.calligra.org/DTD/[krita].dtd'>\n<DOC/>";
        assert!(parse(krita, dtd()).is_ok());
        let deep = format!("<!DOCTYPE a [<!ENTITY e \"{}{}\">]><a>&e;</a>", "<g>".repeat(20_000), "</g>".repeat(20_000));
        for xml in [
            "<!DOCTYPE a [<!ENTITY e \"x\">]><a>&e;</a>".to_string(),
            "\u{feff}<?xml version=\"1.0\"?><!-- ' --> <!DOCTYPE a SYSTEM \"a.dtd\" [<!-- ' -->]><a/>".to_string(),
            deep,
        ] {
            let e = parse(&xml, dtd()).err().unwrap_or_else(|| panic!("parsed {xml:.60}"));
            assert!(e.contains("internal DTD"), "{e}");
        }
        // Without allow_dtd roxmltree refuses any DOCTYPE itself.
        assert!(parse(krita, ParsingOptions::default()).is_err());
    }
}
