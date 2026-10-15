//! Embedded smart-object files from the PSD global `lnk2`/`lnk3`/`lnkD` blocks.
//!
//! PSD import keeps a placed layer's source as `SmartSource::Linked { path: <Idnt uuid> }` and the
//! file bytes stay inside the preserved global block (so PSD export stays byte-stable). This module
//! finds the bytes for a uuid when the engine needs to re-render the smart object from its source.
//!
//! Layout (Adobe PSD spec, "Linked Layer"): a sequence of items, each `u64 length` + item data,
//! padded to 4 bytes. Item: type (`liFD` data / `liFE` external / `liFA` alias), version (1–7),
//! Pascal uuid, Unicode file name, file type, creator, `u64` data length, open-descriptor flag (+
//! versioned descriptor), then for `liFD` the raw file bytes.

use photosplice_doc::Metadata;
use photosplice_psd::descriptor::VersionedDescriptor;

/// An embedded file found in a linked-layer block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkedFile {
    pub uuid: String,
    pub file_name: String,
    pub bytes: Vec<u8>,
}

struct Rd<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.d.get(self.p..self.p.checked_add(n)?)?;
        self.p += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Option<u64> {
        self.take(8).map(|b| u64::from_be_bytes(b.try_into().unwrap_or([0; 8])))
    }
    fn unicode(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        let raw = self.take(n.checked_mul(2)?)?;
        let units: Vec<u16> = raw.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        Some(String::from_utf16_lossy(&units).trim_end_matches('\0').to_string())
    }
}

/// Parses one item; `None` for malformed data or non-`liFD` items.
fn parse_item(item: &[u8]) -> Option<LinkedFile> {
    let mut r = Rd { d: item, p: 0 };
    let kind = r.take(4)?;
    let _version = r.u32()?;
    let n = r.u8()? as usize;
    let uuid = String::from_utf8_lossy(r.take(n)?).to_string();
    let file_name = r.unicode()?;
    r.take(8)?; // file type + creator
    let len = usize::try_from(r.u64()?).ok()?;
    if r.u8()? != 0 {
        let (_, used) = VersionedDescriptor::parse_prefix(item.get(r.p..)?).ok()?;
        r.p += used;
    }
    if kind != b"liFD" {
        return None;
    }
    let bytes = r.take(len)?.to_vec();
    Some(LinkedFile { uuid, file_name, bytes })
}

/// Every embedded (`liFD`) file in one linked-layer block's data.
pub fn parse_linked_files(data: &[u8]) -> Vec<LinkedFile> {
    let mut out = Vec::new();
    let mut r = Rd { d: data, p: 0 };
    while let Some(len) = r.u64() {
        let Ok(len) = usize::try_from(len) else { break };
        let Some(item) = r.take(len) else { break };
        out.extend(parse_item(item));
        r.p = r.p.next_multiple_of(4);
    }
    out
}

/// The embedded file for smart-object `uuid` among the document's preserved PSD global blocks.
pub fn find_linked_file(meta: &Metadata, uuid: &str) -> Option<LinkedFile> {
    find_in_blocks(meta.psd_global_blocks.iter().map(|(_, k, d)| (k, d.as_slice())), uuid)
}

/// The embedded file for smart-object `uuid` among PSD global blocks given as (key, data).
pub fn find_in_blocks<'a>(blocks: impl IntoIterator<Item = (&'a [u8; 4], &'a [u8])>, uuid: &str) -> Option<LinkedFile> {
    if uuid.is_empty() {
        return None;
    }
    blocks.into_iter().filter(|(k, _)| matches!(*k, b"lnk2" | b"lnk3" | b"lnkD")).flat_map(|(_, d)| parse_linked_files(d)).find(|f| f.uuid == uuid)
}

/// The size of embedded file `uuid` among PSD global blocks given as (key, data), when it is there
/// and readable with a non-empty size (the source size a `PlLd` record leaves out).
pub(crate) fn source_size_in_blocks<'a>(
    blocks: impl IntoIterator<Item = (&'a [u8; 4], &'a [u8])>,
    uuid: &str,
    ctl: &photosplice_raster::Interrupt,
) -> Option<(f64, f64)> {
    let f = find_in_blocks(blocks, uuid)?;
    if ctl.cancelled() {
        return None;
    }
    source_size_probe(&f.bytes).filter(|(w, h)| *w > 0.0 && *h > 0.0)
}

/// Pixel size of an embedded source file from its header: PSD/PSB, TIFF (the default page's
/// directory), PNG (`IHDR`) and JPEG (the frame header); any other image is decoded flat by
/// `photosplice-codecs`. It never imports a document: a layered TIFF can carry PSD layer data
/// whose `lnkD` holds another file, so a full import here would nest without bound.
/// Ported from storytold/photocraft@0002d861.
pub(crate) fn source_size_probe(bytes: &[u8]) -> Option<(f64, f64)> {
    let be32 = |at: usize| bytes.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    let size = |w: u32, h: u32| (f64::from(w), f64::from(h));
    match bytes {
        _ if crate::is_psd(bytes) => be32(18).zip(be32(14)).map(|(w, h)| size(w, h)),
        [b'I', b'I', 42, 0, ..] | [b'M', b'M', 0, 42, ..] | [b'I', b'I', 43, 0, ..] | [b'M', b'M', 0, 43, ..] => {
            let info = photosplice_codecs::tiff_info(bytes).ok()?;
            let page = info.pages.get(info.default_page()?)?;
            Some(size(page.width, page.height))
        }
        [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, ..] => be32(16).zip(be32(20)).map(|(w, h)| size(w, h)),
        [0xFF, 0xD8, ..] => jpeg_size(bytes),
        _ => photosplice_codecs::decode(bytes).ok().map(|img| {
            let (w, h) = img.dimensions();
            size(w, h)
        }),
    }
}

/// A JPEG's size from its first start-of-frame marker (ITU-T T.81 B.2.2), walking the marker
/// segments without decoding. Each step moves forward, so a malformed file ends the walk.
fn jpeg_size(bytes: &[u8]) -> Option<(f64, f64)> {
    let mut at = 2usize;
    loop {
        // Markers are 0xFF then a code; extra 0xFF fill bytes may come first.
        while bytes.get(at) == Some(&0xFF) && bytes.get(at + 1) == Some(&0xFF) {
            at += 1;
        }
        if *bytes.get(at)? != 0xFF {
            return None;
        }
        let code = *bytes.get(at + 1)?;
        match code {
            // SOF0..SOF15, except DHT (C4), JPG (C8) and DAC (CC).
            0xC0..=0xCF if !matches!(code, 0xC4 | 0xC8 | 0xCC) => {
                let h = bytes.get(at + 5..at + 7)?;
                let w = bytes.get(at + 7..at + 9)?;
                return Some((f64::from(u16::from_be_bytes([w[0], w[1]])), f64::from(u16::from_be_bytes([h[0], h[1]]))));
            }
            // Standalone markers have no length.
            0x01 | 0xD0..=0xD7 => at = at.checked_add(2)?,
            // Start of scan or end of image before any frame header.
            0xDA | 0xD9 => return None,
            _ => {
                let len = bytes.get(at + 2..at + 4).map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))?;
                if len < 2 {
                    return None;
                }
                at = at.checked_add(2 + len)?;
            }
        }
    }
}

/// The four-character file type Photoshop records for an embedded file, from its contents.
pub fn file_type(bytes: &[u8]) -> [u8; 4] {
    match bytes {
        [b'8', b'B', b'P', b'S', 0, 2, ..] => *b"8BPB",
        [b'8', b'B', b'P', b'S', ..] => *b"8BPS",
        [0x89, b'P', b'N', b'G', ..] => *b"PNGf",
        [0xFF, 0xD8, ..] => *b"JPEG",
        [b'I', b'I', 42, 0, ..] | [b'M', b'M', 0, 42, ..] => *b"TIFF",
        [b'G', b'I', b'F', ..] => *b"GIFf",
        _ => *b"    ",
    }
}

/// Encodes one `liFD` item (version 7: no open descriptor, empty child document id, no
/// modification time, unlocked), length-prefixed and padded to 4 bytes.
pub fn encode_linked_file(f: &LinkedFile) -> Vec<u8> {
    let mut item = Vec::new();
    item.extend_from_slice(b"liFD");
    item.extend_from_slice(&7u32.to_be_bytes());
    let id = f.uuid.as_bytes().get(..f.uuid.len().min(255)).unwrap_or_default();
    item.push(id.len() as u8);
    item.extend_from_slice(id);
    // NUL-terminated, the terminator counted (as Photoshop writes and reads it).
    let units: Vec<u16> = f.file_name.encode_utf16().chain(std::iter::once(0)).collect();
    item.extend_from_slice(&(units.len() as u32).to_be_bytes());
    for u in units {
        item.extend_from_slice(&u.to_be_bytes());
    }
    item.extend_from_slice(&file_type(&f.bytes));
    item.extend_from_slice(b"8BIM");
    item.extend_from_slice(&(f.bytes.len() as u64).to_be_bytes());
    item.push(0);
    item.extend_from_slice(&f.bytes);
    item.extend_from_slice(&0u32.to_be_bytes()); // child document id (version 5)
    item.extend_from_slice(&0f64.to_be_bytes()); // asset modification time (version 6)
    item.push(0); // asset locked (version 7)
    let mut out = (item.len() as u64).to_be_bytes().to_vec();
    out.extend_from_slice(&item);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
    out
}

/// The uuid of one linked-layer item (any type), if its header is readable.
fn item_uuid(item: &[u8]) -> Option<String> {
    let n = usize::from(*item.get(8)?);
    Some(String::from_utf8_lossy(item.get(9..9 + n)?).into_owned())
}

/// The items of a linked-layer block: each item's uuid (when readable) and its bytes including
/// the length prefix and padding. A malformed tail is returned as one unnamed piece.
fn split_items(data: &[u8]) -> Vec<(Option<String>, &[u8])> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Some(rest) = data.get(at..).filter(|r| !r.is_empty()) {
        let len = rest.get(..8).map(|b| u64::from_be_bytes(b.try_into().unwrap_or([0; 8])));
        let end = len.and_then(|l| usize::try_from(l).ok()).and_then(|l| l.checked_add(8)).filter(|&e| e <= rest.len());
        let (Some(end), Some(item)) = (end, end.and_then(|e| rest.get(8..e))) else {
            out.push((None, rest));
            break;
        };
        let padded = end.next_multiple_of(4).min(rest.len());
        out.push((item_uuid(item), rest.get(..padded).unwrap_or(rest)));
        at += padded;
    }
    out
}

/// Rewrites a linked-layer block: keeps the items `keep` accepts (and anything unreadable), then
/// appends `add`. `None` when nothing would change.
pub fn rebuild_block(data: &[u8], keep: &dyn Fn(&str) -> bool, add: &[LinkedFile]) -> Option<Vec<u8>> {
    let items = split_items(data);
    let dropped = items.iter().any(|(u, _)| u.as_deref().is_some_and(|u| !keep(u)));
    if !dropped && add.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(data.len());
    for (u, bytes) in items {
        if u.as_deref().is_none_or(keep) {
            out.extend_from_slice(bytes);
        }
    }
    for f in add {
        out.extend(encode_linked_file(f));
    }
    Some(out)
}

/// The uuids of every item in a linked-layer block.
pub fn block_uuids(data: &[u8]) -> Vec<String> {
    split_items(data).into_iter().filter_map(|(u, _)| u).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A placed source's size comes from its header, for every format a CS2 file embeds, and
    /// never by importing it as a document.
    #[test]
    fn source_size_probe_reads_headers() {
        use photosplice_codecs::{ChannelLayout, EncodeOptions, Format, Image, SampleType};
        let img = Image::new(7, 5, ChannelLayout::Rgb, SampleType::U8).unwrap();
        for format in [Format::Png, Format::Jpeg, Format::Tiff, Format::Bmp] {
            let bytes = photosplice_codecs::encode(&img, format, &EncodeOptions::default()).unwrap();
            assert_eq!(source_size_probe(&bytes), Some((7.0, 5.0)), "{format:?}");
        }
        let mut psd = b"8BPS\0\x01".to_vec();
        psd.extend([0; 6]);
        psd.extend(3u16.to_be_bytes());
        psd.extend(40u32.to_be_bytes()); // height
        psd.extend(30u32.to_be_bytes()); // width
        assert_eq!(source_size_probe(&psd), Some((30.0, 40.0)));
        assert_eq!(source_size_probe(b"not an image"), None);
        assert_eq!(source_size_probe(&[]), None);
    }

    /// Malformed JPEG marker walks end without a size and without panicking or looping.
    #[test]
    fn jpeg_size_rejects_malformed_marker_walks() {
        let sof = [0xFF, 0xC0, 0, 17, 8, 0, 5, 0, 7];
        let ok: Vec<u8> = [&[0xFF, 0xD8][..], &[0xFF, 0xE0, 0, 4, 1, 2], &[0xFF, 0xFF, 0xD0], &sof].concat();
        assert_eq!(jpeg_size(&ok), Some((7.0, 5.0)), "APP0, a fill byte and a standalone marker are skipped");
        for bad in [
            vec![0xFF, 0xD8],
            vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 1],       // length below 2
            vec![0xFF, 0xD8, 0xFF, 0xE0, 0xFF, 0xFF], // length past the end
            vec![0xFF, 0xD8, 0x00, 0x00],             // not a marker
            vec![0xFF, 0xD8, 0xFF, 0xDA, 0, 2],       // scan before any frame header
            vec![0xFF, 0xD8, 0xFF, 0xC0, 0, 17, 8],   // truncated frame header
            [&[0xFF, 0xD8][..], &[0xFF; 64]].concat(),
        ] {
            assert_eq!(jpeg_size(&bad), None, "{bad:?}");
        }
    }

    #[test]
    fn round_trips_and_finds_by_uuid() {
        let a = LinkedFile { uuid: "abc-1".into(), file_name: "a.png".into(), bytes: vec![1, 2, 3] };
        let b = LinkedFile { uuid: "def-2".into(), file_name: "b.psd".into(), bytes: vec![9; 10] };
        let mut data = encode_linked_file(&a);
        data.extend(encode_linked_file(&b));
        assert_eq!(parse_linked_files(&data), vec![a.clone(), b.clone()]);
        let mut meta = Metadata::default();
        meta.psd_global_blocks.push((*b"8BIM", *b"lnk2", Arc::new(data)));
        assert_eq!(find_linked_file(&meta, "def-2"), Some(b));
        assert_eq!(find_linked_file(&meta, "zzz"), None);
        assert_eq!(find_linked_file(&meta, ""), None);
    }

    #[test]
    fn blocks_are_pruned_and_extended() {
        let a = LinkedFile { uuid: "a".into(), file_name: "a.png".into(), bytes: vec![0x89, b'P', b'N', b'G', 1] };
        let b = LinkedFile { uuid: "b".into(), file_name: "b.psb".into(), bytes: b"8BPS\0\x02rest".to_vec() };
        let c = LinkedFile { uuid: "c".into(), file_name: "c".into(), bytes: vec![7; 3] };
        let mut data = encode_linked_file(&a);
        data.extend(encode_linked_file(&b));
        assert_eq!(block_uuids(&data), vec!["a", "b"]);
        assert_eq!(rebuild_block(&data, &|_| true, &[]), None);
        let out = rebuild_block(&data, &|u| u != "a", std::slice::from_ref(&c)).unwrap();
        assert_eq!(parse_linked_files(&out), vec![b.clone(), c.clone()]);
        photosplice_psd::TaggedBlock::new(*b"lnk2", out).check_structure().unwrap();
        assert_eq!(file_type(&b.bytes), *b"8BPB");
        assert_eq!(file_type(&a.bytes), *b"PNGf");
        // A malformed tail is kept as is.
        let mut bad = encode_linked_file(&a);
        bad.extend([0, 0, 0, 0, 0, 0, 1, 0, 9]);
        let out = rebuild_block(&bad, &|_| true, std::slice::from_ref(&c)).unwrap();
        assert!(out.starts_with(&bad));
    }

    #[test]
    fn malformed_data_is_ignored() {
        assert!(parse_linked_files(&[0, 0, 0, 0, 0, 0, 0, 200, 1, 2]).is_empty());
        assert!(parse_linked_files(&[]).is_empty());
        let mut d = encode_linked_file(&LinkedFile { uuid: "x".into(), file_name: "f".into(), bytes: vec![5; 4] });
        d.truncate(d.len() - 6);
        assert!(parse_linked_files(&d).is_empty());
    }
}
