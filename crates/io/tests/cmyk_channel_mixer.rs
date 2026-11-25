//! A CMYK document's Channel Mixer (`mixr` with four ink records) through PSD: an imported block
//! nobody edited is saved back exactly as it was read, whatever its layout (a monochrome one
//! included, though no Reference sample confirms how it renders); an edited one is written in
//! Reference's own 44-byte layout. With the `corpus` feature, Reference's own CMYK mixers
//! (photoshop corpus cmyk8 and cmyk16 channel-mixer.psd) are read and saved back byte for byte.

use photosplice_color::{Color, ColorMode, SampleType};
use photosplice_doc::{Adjustment, Document, Layer, LayerContent, Size};
use photosplice_io::*;
use photosplice_psd::PsdFile;

/// The identity RGB matrix a CMYK mixer carries unused.
const RGB_IDENTITY: [[f32; 4]; 3] = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];

/// Big-endian i16 words, as a `mixr` block stores them.
fn i16s(v: &[i16]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_be_bytes()).collect()
}

/// The `mixr` block data of the first layer record that has one.
fn mixr(file: &PsdFile) -> Option<Vec<u8>> {
    file.layers().iter().find_map(|l| l.block(b"mixr")).map(|b| b.data.clone())
}

/// The document's first adjustment.
fn first_adjustment(d: &Document) -> Option<Adjustment> {
    d.layers.iter().find_map(|l| match &l.content {
        LayerContent::Adjustment(a) => Some(a.clone()),
        _ => None,
    })
}

/// Exports `d` and parses the PSD it became.
fn exported(d: &Document) -> PsdFile {
    PsdFile::from_bytes(&export(d, "x.psd", &ExportOptions::default()).expect("export").bytes).expect("reparse")
}

/// A CMYK PSD with a Channel Mixer layer over a white background whose `mixr` block is `block`,
/// as another writer could have stored it.
fn psd_with_mixr(block: &[u8]) -> Vec<u8> {
    let mut d = Document::with_background("mixer", Size::new(4, 4), ColorMode::Cmyk, SampleType::U8, Color::WHITE);
    let mut rows = photosplice_doc::adjust::cmyk_mixer_records(&RGB_IDENTITY);
    rows[0][3] = 0.2;
    let adj = Adjustment::ChannelMixer { matrix: RGB_IDENTITY, monochrome: false, cmyk: Some(rows) };
    d.layers.push(Layer::new("Channel Mixer 1", LayerContent::Adjustment(adj)));
    let mut file = exported(&d);
    let b = file.layers_mut().iter_mut().flat_map(|l| l.blocks.iter_mut()).find(|b| b.key == *b"mixr").expect("a mixr block");
    b.data = block.to_vec();
    file.to_bytes().expect("write psd")
}

/// A monochrome block carrying its first record alone (padded), and a four-record block followed
/// by bytes our writer never emits: both are saved back unchanged while unedited, and the
/// four-record one is rewritten as Reference's 44 bytes once
/// edited.
#[test]
fn unedited_cmyk_mixer_blocks_are_saved_back_as_read() {
    let mut mono = i16s(&[1, 1, 30, 30, 30, 100, 0]);
    mono.extend([0, 0]);
    let mut long = i16s(&[1, 0, 80, 30, -10, 0, 0, 10, 90, 0, 0, 5, 0, 20, 70, 0, 0, 0, 0, 0, 0, 0]);
    long.extend([0, 0, 0, 0]);
    for (block, monochrome) in [(mono, true), (long, false)] {
        let imp = import("x.psd", &psd_with_mixr(&block)).expect("import");
        let adj = first_adjustment(&imp.document);
        assert!(matches!(adj, Some(Adjustment::ChannelMixer { monochrome: m, .. }) if m == monochrome), "{adj:?}");
        assert_eq!(mixr(&exported(&imp.document)), Some(block.clone()), "monochrome {monochrome}: saved back as read");
        if monochrome {
            continue;
        }
        let mut doc = imp.document;
        for l in &mut doc.layers {
            if let LayerContent::Adjustment(Adjustment::ChannelMixer { cmyk: Some(rows), .. }) = &mut l.content {
                rows[1][4] = 0.1;
            }
        }
        let edited = mixr(&exported(&doc)).expect("a mixr block");
        assert_eq!(edited.len(), 44, "edited");
        assert_ne!(edited, block);
    }
}

#[cfg(feature = "corpus")]
mod corpus {
    use super::*;
    use std::path::Path;

    /// Reference's CMYK Channel Mixer (photoshop corpus cmyk8 and cmyk16 channel-mixer.psd, made
    /// with cyan 80/30/-10, magenta 10/90/0 +5 and yellow 0/20/70 and no black record): a 44-byte
    /// block, read as four ink rows with an empty black one, and saved back byte for byte. (Its
    /// render is checked against the merged image by the oracle corpus test.)
    #[test]
    fn reference_cmyk_channel_mixers_read_and_save_back() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/photoshop/adjustments");
        let rows = [[0.8, 0.3, -0.1, 0.0, 0.0], [0.1, 0.9, 0.0, 0.0, 0.05], [0.0, 0.2, 0.7, 0.0, 0.0], [0.0; 5]];
        for dir in ["cmyk8", "cmyk16"] {
            let path = root.join(dir).join("channel-mixer.psd");
            let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e} (run `cargo xtask corpus --all`)", path.display()));
            let file = PsdFile::from_bytes(&bytes).expect("parse");
            let block = mixr(&file);
            assert_eq!(block.as_ref().map(Vec::len), Some(44), "{dir}: Reference's block");
            let imp = import("channel-mixer.psd", &bytes).expect("import");
            assert!(!imp.warnings.iter().any(|w| w.contains("Channel Mixer")), "{dir}: {:?}", imp.warnings);
            assert!(
                matches!(first_adjustment(&imp.document), Some(Adjustment::ChannelMixer { monochrome: false, cmyk: Some(r), .. }) if r == rows),
                "{dir}: {:?}",
                first_adjustment(&imp.document)
            );
            assert_eq!(mixr(&exported(&imp.document)), block, "{dir}: saved back byte for byte");
        }
    }
}
