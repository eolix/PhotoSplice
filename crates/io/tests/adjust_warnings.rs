//! Opening a file whose adjustment layer can't be read warns, naming the layer and the reason,
//! instead of opening it silently without its effect (#1763).

use photosplice_color::{ColorMode, SampleType};
use photosplice_doc::{Adjustment, Document, Layer, LayerContent};
use photosplice_geom::Size;
use photosplice_io::*;

fn reopen(adj: Adjustment, path: &str, opts: &ExportOptions) -> (Document, Vec<String>) {
    let mut d = Document::new("a", Size::new(16, 16), ColorMode::Rgb, SampleType::U8);
    d.layers.push(Layer::new("Teal Look", LayerContent::Adjustment(adj)));
    let r = export(&d, path, opts).expect("export");
    let imp = import(path, &r.bytes).expect("import");
    (imp.document, imp.warnings)
}

#[test]
fn unreadable_adjustments_warn_on_open() {
    // A Color Lookup that uses a profile, not a LUT file: kept verbatim, not applied.
    let profile = Adjustment::Unsupported { psd_key: "clrL".into(), raw: vec![0, 2, 0, 0] };
    for (path, opts) in [("x.psd", ExportOptions::default()), ("x.psb", ExportOptions { force_psb: true, ..Default::default() })] {
        let (doc, warnings) = reopen(profile.clone(), path, &opts);
        assert!(matches!(&doc.layers[0].content, LayerContent::Adjustment(Adjustment::Unsupported { .. })), "{path}");
        let w: Vec<&String> = warnings.iter().filter(|w| w.contains("Teal Look")).collect();
        assert_eq!(w.len(), 1, "{path}: {warnings:?}");
        assert!(w[0].contains("Color Lookup") && w[0].contains("version 2") && w[0].contains("no effect"), "{path}: {}", w[0]);
    }
    // Another kind without a specific reason still names the layer and the adjustment.
    let (_, warnings) = reopen(Adjustment::Unsupported { psd_key: "levl".into(), raw: vec![0, 9] }, "x.psd", &ExportOptions::default());
    assert!(warnings.iter().any(|w| w.contains("Teal Look") && w.contains("Levels settings could not be read;")), "{warnings:?}");
}

#[test]
fn readable_adjustments_open_without_warnings() {
    let lut: Vec<f32> = (0..8).flat_map(|i| [(i & 1) as f32, ((i >> 1) & 1) as f32, ((i >> 2) & 1) as f32]).collect();
    let lookup = Adjustment::ColorLookup { name: "id.cube".into(), size: 2, lut: Some(std::sync::Arc::new(lut)), tetrahedral: false, dither: false };
    for adj in [lookup, Adjustment::Invert] {
        let (doc, warnings) = reopen(adj, "x.psd", &ExportOptions::default());
        assert!(!matches!(&doc.layers[0].content, LayerContent::Adjustment(Adjustment::Unsupported { .. })));
        assert!(!warnings.iter().any(|w| w.contains("Teal Look")), "{warnings:?}");
    }
}

/// A `clrL` block in Reference's layout whose `.cube` has a non-default input domain.
fn custom_domain_lookup_block(domain: &str) -> Vec<u8> {
    use photosplice_psd::descriptor::{Descriptor, Id, UnicodeString, Value, VersionedDescriptor};
    let cube = format!("{domain}LUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n");
    let desc = Descriptor::new("null")
        .with("LUTFormat", Value::Enumerated { type_id: Id::new("LUTFormatType"), value: Id::new("LUTFormatCUBE") })
        .with("LUT3DFileName", Value::Text(UnicodeString::new_nul("domain.cube")))
        .with("LUT3DFileData", Value::RawData(cube.into_bytes()));
    let mut raw = vec![0, 1];
    raw.extend(VersionedDescriptor::new(desc).to_bytes());
    raw.resize(raw.len().next_multiple_of(4), 0);
    raw
}

#[test]
fn custom_domain_lookup_warns_and_round_trips_verbatim() {
    // The table samples 0..1 inputs: a LUT with another domain is kept as saved (not rendered
    // wrong and rewritten with the default domain), with a warning naming the reason.
    for domain in ["DOMAIN_MIN 0 0 0\nDOMAIN_MAX 0.5 0.5 0.5\n", "LUT_3D_INPUT_RANGE 0 0.5\n"] {
        let raw = custom_domain_lookup_block(domain);
        for (path, opts) in [("x.psd", ExportOptions::default()), ("x.psb", ExportOptions { force_psb: true, ..Default::default() })] {
            let (doc, warnings) = reopen(Adjustment::Unsupported { psd_key: "clrL".into(), raw: raw.clone() }, path, &opts);
            let LayerContent::Adjustment(Adjustment::Unsupported { psd_key, raw: kept }) = &doc.layers[0].content else {
                panic!("{path}: custom-domain LUT was read: {:?}", doc.layers[0].content)
            };
            assert_eq!((psd_key.as_str(), kept), ("clrL", &raw), "{path}");
            let w: Vec<&String> = warnings.iter().filter(|w| w.contains("Teal Look")).collect();
            assert_eq!(w.len(), 1, "{path}: {warnings:?}");
            assert!(w[0].contains("Color Lookup") && w[0].contains("DOMAIN_MIN") && w[0].contains("not supported"), "{path}: {}", w[0]);
            // Saved again, the block is still the file's own.
            let again = export(&doc, path, &opts).expect("export");
            let reopened = import(path, &again.bytes).expect("import");
            assert!(
                matches!(&reopened.document.layers[0].content, LayerContent::Adjustment(Adjustment::Unsupported { raw: r, .. }) if *r == raw),
                "{path}"
            );
        }
    }
}
