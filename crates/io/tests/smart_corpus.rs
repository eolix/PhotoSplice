//! Smart objects and smart filters through PSD export, over every corpus file that has them
//! (feature `corpus`; run with `cargo xtask test-corpus`).
//!
//! For each file: import → export → import must give back, layer for layer, a live smart object
//! (not pixels) with the same embedded file (byte-identical), transform, filter list, filter
//! switches and filter mask; every exported block re-parses strictly (`strict_block_errors`:
//! `SoLd` with its `filterFX`, `PlLd`, `lnk2`, `FEid`). Then two edits must survive the same way:
//! the first filter hidden at 40 % opacity in Multiply (the placed-layer blocks and the filter
//! cache are regenerated), and every source re-embedded from its bytes (new `lnk2` items, the old
//! ones dropped).
#![cfg(feature = "corpus")]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use photosuite_doc::{Document, LayerContent, SmartObject, SmartSource};
use photosuite_psd::descriptor::{Descriptor, Value, VersionedDescriptor};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("psd") || e.eq_ignore_ascii_case("psb")) {
            out.push(p);
        }
    }
}

fn smarts(doc: &Document) -> Vec<(String, SmartObject)> {
    doc.walk()
        .into_iter()
        .filter_map(|(_, _, l)| match &l.content {
            LayerContent::Smart(sm) => Some((l.name.clone(), sm.clone())),
            _ => None,
        })
        .collect()
}

fn source_file(doc: &Document, sm: &SmartObject) -> Option<Vec<u8>> {
    match &sm.source {
        SmartSource::Embedded { bytes, .. } => Some(bytes.to_vec()),
        SmartSource::Linked { path } => photosuite_io::linked::find_linked_file(&doc.metadata, path).map(|f| f.bytes),
    }
}

/// Mask coverage over `area` (None = no mask: all ones).
fn mask_values(sm: &SmartObject, area: photosuite_geom::Rect) -> Vec<f32> {
    let mut v = Vec::new();
    match &sm.filter_mask {
        Some(m) => m.values_into(area, &mut v),
        None => v.resize(area.width() as usize * area.height() as usize, 1.0),
    }
    v
}

/// Compares the smart objects of `a` and `b`; returns problems.
fn compare(a: &Document, b: &Document, same_files: bool) -> Vec<String> {
    let (sa, sb) = (smarts(a), smarts(b));
    if sa.len() != sb.len() {
        return vec![format!("{} smart objects became {}", sa.len(), sb.len())];
    }
    let mut errs = Vec::new();
    for ((name, x), (_, y)) in sa.iter().zip(&sb) {
        let fa = source_file(a, x);
        let fb = source_file(b, y);
        match (&fa, &fb) {
            (Some(p), Some(q)) if same_files && p != q => errs.push(format!("{name}: embedded file changed")),
            (Some(_), None) => errs.push(format!("{name}: embedded file lost")),
            _ => {}
        }
        if x.smart_filters != y.smart_filters {
            errs.push(format!("{name}: filters {:?} became {:?}", x.smart_filters, y.smart_filters));
        }
        if x.filters_enabled != y.filters_enabled {
            errs.push(format!("{name}: filters enabled changed"));
        }
        if x.transform.m.iter().zip(y.transform.m).any(|(p, q)| (p - q).abs() > 1e-6) {
            errs.push(format!("{name}: transform {:?} became {:?}", x.transform, y.transform));
        }
        if x.warp != y.warp {
            errs.push(format!("{name}: warp changed"));
        }
        let area = a.bounds();
        if !x.smart_filters.is_empty() && mask_values(x, area) != mask_values(y, area) {
            errs.push(format!("{name}: filter mask changed"));
        }
        if x.filter_mask.as_ref().map(|m| (m.enabled, m.linked)) != y.filter_mask.as_ref().map(|m| (m.enabled, m.linked)) {
            errs.push(format!("{name}: filter mask switches changed"));
        }
    }
    errs
}

fn round_trip(doc: &Document) -> Result<(Document, Vec<u8>), String> {
    let out = photosuite_io::export(doc, "psd", &Default::default()).map_err(|e| e.to_string())?;
    if let Some(w) = out.warnings.iter().find(|w| w.contains("smart object written as pixels")) {
        return Err(w.clone());
    }
    let errs = common::strict_block_errors(&out.bytes);
    if !errs.is_empty() {
        return Err(format!("strict structure: {errs:?}"));
    }
    let back = photosuite_io::import("x.psd", &out.bytes).map_err(|e| e.to_string())?.document;
    Ok((back, out.bytes))
}

fn edit_filters(doc: &mut Document) -> bool {
    let mut any = false;
    for id in doc.walk().into_iter().map(|(_, _, l)| l.id).collect::<Vec<_>>() {
        if let Some(l) = doc.layer_mut(id)
            && let LayerContent::Smart(sm) = &mut l.content
            && let Some(f) = sm.smart_filters.first_mut()
        {
            f.visible = !f.visible;
            f.opacity = 0.4;
            f.blend = photosuite_color::BlendMode::Multiply;
            any = true;
        }
    }
    any
}

fn re_embed(doc: &mut Document) -> bool {
    let meta = doc.metadata.clone();
    let mut any = false;
    for id in doc.walk().into_iter().map(|(_, _, l)| l.id).collect::<Vec<_>>() {
        if let Some(l) = doc.layer_mut(id)
            && let LayerContent::Smart(sm) = &mut l.content
            && let SmartSource::Linked { path } = &sm.source
            && let Some(f) = photosuite_io::linked::find_linked_file(&meta, path)
        {
            sm.source = SmartSource::Embedded { file_name: f.file_name, bytes: Arc::new(f.bytes) };
            sm.psd_raw = None;
            l.psd_blocks.retain(|(k, _)| !matches!(k, b"SoLd" | b"PlLd" | b"SoLE"));
            any = true;
        }
    }
    any
}

/// Every layer's placed-layer blocks: its name, which of `PlLd` / `SoLd` / `SoLE` it has, and the
/// `PlLd` and `SoLd`/`SoLE` warp descriptors.
type PlacedRow = (String, Vec<[u8; 4]>, Option<Descriptor>, Option<Descriptor>);

fn placed_rows(bytes: &[u8]) -> Vec<PlacedRow> {
    let Ok(f) = photosuite_psd::PsdFile::from_bytes(bytes) else { return Vec::new() };
    let warp = |d: Descriptor| match d.get("warp") {
        Some(Value::Descriptor(w)) => Some(w.clone()),
        _ => None,
    };
    let mut rows = Vec::new();
    for r in f.layers() {
        let keys: Vec<[u8; 4]> = r.blocks.iter().map(|b| b.key).filter(|k| matches!(k, b"PlLd" | b"SoLd" | b"SoLE")).collect();
        if keys.is_empty() {
            continue;
        }
        let (mut pl, mut so) = (None, None);
        for b in &r.blocks {
            match &b.key {
                // plcL, version, Pascal id, page / pages / anti-alias / type, 8 doubles, warp version.
                b"PlLd" => {
                    let at = b.data.get(8).map_or(usize::MAX, |n| 8 + 1 + usize::from(*n) + 16 + 64 + 4);
                    pl = b.data.get(at..).and_then(|d| VersionedDescriptor::parse_prefix(d).ok()).map(|(v, _)| v.descriptor);
                }
                b"SoLd" | b"SoLE" => so = b.data.get(8..).and_then(|d| VersionedDescriptor::parse_prefix(d).ok()).and_then(|(v, _)| warp(v.descriptor)),
                _ => {}
            }
        }
        rows.push((String::from_utf8_lossy(&r.name).into_owned(), keys, pl, so));
    }
    rows
}

fn keys_str(k: &[[u8; 4]]) -> Vec<String> {
    k.iter().map(|k| String::from_utf8_lossy(k).into_owned()).collect()
}

fn nudge(doc: &mut Document) {
    for id in doc.walk().into_iter().map(|(_, _, l)| l.id).collect::<Vec<_>>() {
        if let Some(l) = doc.layer_mut(id)
            && let LayerContent::Smart(sm) = &mut l.content
        {
            sm.transform.m[4] += 0.25;
        }
    }
}

#[test]
fn smart_objects_round_trip_through_psd() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus");
    assert!(root.join("photoshop").is_dir(), "{} is missing: run `cargo xtask corpus --all`", root.display());
    let mut files = Vec::new();
    for d in ["psd", "psd-tools", "photoshop"] {
        collect(&root.join(d), &mut files);
    }
    files.sort();
    let (mut checked, mut objects, mut with_filters, mut edited, mut embedded) = (0, 0, 0, 0, 0);
    let mut failures = Vec::new();
    for p in &files {
        let name = p.strip_prefix(&root).unwrap_or(p).display().to_string();
        let bytes = std::fs::read(p).unwrap_or_default();
        let Ok(imp) = photosuite_io::import(&name, &bytes) else { continue };
        let doc = imp.document;
        let n = smarts(&doc).len();
        if n == 0 {
            continue;
        }
        checked += 1;
        objects += n;
        with_filters += smarts(&doc).iter().filter(|(_, s)| !s.smart_filters.is_empty()).count();
        let mut fail = |what: &str, e: String| failures.push(format!("{name} ({what}): {e}"));
        match round_trip(&doc) {
            Ok((back, _)) => compare(&doc, &back, true).into_iter().for_each(|e| fail("as imported", e)),
            Err(e) => fail("as imported", e),
        }
        let mut ed = doc.clone();
        if edit_filters(&mut ed) {
            edited += 1;
            match round_trip(&ed) {
                Ok((back, _)) => compare(&ed, &back, true).into_iter().for_each(|e| fail("filters edited", e)),
                Err(e) => fail("filters edited", e),
            }
        }
        // Placed-layer data regenerated (moved by ¼ px): each layer keeps its own set of placed blocks (a linked
        // `SoLE` layer gets no `PlLd`), and `PlLd` carries the warp `SoLd`/`SoLE` carries.
        let mut moved = doc.clone();
        nudge(&mut moved);
        match round_trip(&moved) {
            Ok((back, out)) => {
                compare(&moved, &back, true).into_iter().for_each(|e| fail("moved", e));
                let (before, after) = (placed_rows(&bytes), placed_rows(&out));
                for ((name, keys, ..), (_, out_keys, pl, so)) in before.iter().zip(&after) {
                    let (mut a, mut b) = (keys.clone(), out_keys.clone());
                    a.sort();
                    b.sort();
                    if a != b {
                        fail("moved", format!("layer \"{name}\": placed blocks {:?} became {:?}", keys_str(&a), keys_str(&b)));
                    }
                    if pl.is_some() && so.is_some() && pl != so {
                        fail("moved", format!("layer \"{name}\": PlLd warp differs from the SoLd warp"));
                    }
                }
            }
            Err(e) => fail("moved", e),
        }
        // A filter mask survives the round trip as Photoshop shows it: the stored pixels go out
        // unmasked (Photoshop mixes them with the `FEid` content by the mask) and come back mixed.
        if smarts(&doc).iter().any(|(_, s)| s.filter_mask.is_some()) {
            match round_trip(&doc) {
                Ok((back, _)) => {
                    let (a, b) = (photosuite_compose::flatten(&doc).px, photosuite_compose::flatten(&back).px);
                    let worst = a.iter().zip(&b).flat_map(|(p, q)| (0..4).map(move |c| (p[c] * p[3] - q[c] * q[3]).abs())).fold(0.0f32, f32::max);
                    if worst > 3.0 / 255.0 {
                        fail("filter mask", format!("composite moved by {worst} through PSD"));
                    }
                }
                Err(e) => fail("filter mask", e),
            }
        }
        let mut em = doc.clone();
        if re_embed(&mut em) {
            embedded += 1;
            match round_trip(&em) {
                Ok((back, out)) => {
                    compare(&em, &back, true).into_iter().for_each(|e| fail("re-embedded", e));
                    // The old files are gone: one lnk2 item per distinct source.
                    let f = photosuite_psd::PsdFile::from_bytes(&out).unwrap();
                    let items: Vec<String> = f
                        .global_blocks
                        .iter()
                        .filter(|b| matches!(&b.key, b"lnk2" | b"lnk3" | b"lnkD" | b"lnkE"))
                        .flat_map(|b| photosuite_io::linked::block_uuids(&b.data))
                        .collect();
                    let mut used: Vec<String> = smarts(&back)
                        .iter()
                        .filter_map(|(_, s)| match &s.source {
                            SmartSource::Linked { path } => Some(path.clone()),
                            SmartSource::Embedded { .. } => None,
                        })
                        .collect();
                    used.sort();
                    used.dedup();
                    let mut items_sorted = items.clone();
                    items_sorted.sort();
                    if !used.iter().all(|u| items_sorted.contains(u)) || items_sorted.len() > used.len() {
                        fail("re-embedded", format!("lnk2 items {items:?} for sources {used:?}"));
                    }
                }
                Err(e) => fail("re-embedded", e),
            }
        }
        eprintln!("{name:<70} {n:>3} smart objects");
    }
    eprintln!(
        "smart objects through PSD: {checked} files, {objects} smart objects ({with_filters} with filters); {edited} filter edits, {embedded} re-embeds; {} failures",
        failures.len()
    );
    for f in &failures {
        eprintln!("  FAIL {f}");
    }
    assert!(checked >= 30, "only {checked} corpus files with smart objects");
    assert!(failures.is_empty(), "{} failures", failures.len());
}
