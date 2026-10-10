//! Structural PSD round-trip census: what survives PSD → document → PSD (#215 round-trip work).
//!
//! The corpus test compares pixels; this compares structure. For every file it imports, exports
//! and re-parses, then compares image resources, global tagged blocks and each layer's tagged
//! blocks with the original: byte-identical, changed, or lost. For descriptor-bearing blocks it
//! also reports descriptor key paths (and EngineData `/Keys`) that were lost or added.
//!
//! ```sh
//! cargo run --release -p photosuite-io --example psd_fidelity -- corpus/photoshop corpus/psd-tools
//! cargo run --release -p photosuite-io --example psd_fidelity -- file.psd            # one file, per-layer detail
//! TOP=40 …                                                                            # descriptor paths listed per key
//! REGEN=1 …                                                                           # as if every layer were edited
//! PAIR=1 … a.psd b.psd                                                                # compare two files as they are (b against a)
//! OUT=x.psd … file.psd                                                                # also save our export (one file)
//! ```
//!
//! `REGEN=1` flips each layer's effects master switch, nudges fill colours / angles / scales,
//! drops the verbatim adjustment and shape-path blocks the importer keeps, rebuilds each type layer's `TySh` the way a text edit does, and
//! nudges each smart object's transform, so the export regenerates everything from the document model. The lost
//! descriptor paths are then what an edit in PhotoSuite would lose.
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use photosuite_psd::PsdFile;
use photosuite_psd::descriptor::{Descriptor, Value, VersionedDescriptor};
use photosuite_psd::layer::LayerRecord;
use photosuite_psd::tagged::TaggedBlock;

#[derive(Default)]
struct Stat {
    files: BTreeSet<String>,
    same: usize,
    changed: usize,
    lost: usize,
    added: usize,
    /// First file where the item was not byte-identical.
    example: Option<String>,
}

#[derive(Default)]
struct Census {
    items: BTreeMap<String, Stat>,
    /// Per block key: descriptor path → number of occurrences lost / added.
    lost_paths: BTreeMap<String, BTreeMap<String, usize>>,
    added_paths: BTreeMap<String, BTreeMap<String, usize>>,
    files: usize,
    failed: Vec<(String, String)>,
    layers_orig: usize,
    layers_matched: usize,
    layer_count_changed: Vec<(String, usize, usize)>,
}

fn collect(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_file() {
        out.push(p.to_path_buf());
        return;
    }
    let Ok(rd) = std::fs::read_dir(p) else { return };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for e in entries {
        if e.is_dir() {
            collect(&e, out);
        } else if e.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("psd") || e.eq_ignore_ascii_case("psb")) {
            out.push(e);
        }
    }
}

fn key_str(k: &[u8; 4]) -> String {
    String::from_utf8_lossy(k).into_owned()
}

/// Versioned descriptors found in a block: scans the first bytes (and the bytes after each
/// descriptor) for a version-16 prefix that parses. Good enough for a census; skips the big
/// embedded-file blocks.
fn descriptors(key: &str, data: &[u8]) -> Vec<Descriptor> {
    if matches!(key, "lnk2" | "lnk3" | "lnkD" | "lnkE" | "Patt" | "Pat2" | "Pat3" | "FEid" | "FXid" | "Txt2" | "Anno") {
        return Vec::new();
    }
    // `PlLd` by its layout (`plcL`, version, Pascal id, 4 integers, 8 doubles, warp version, then
    // the warp): scanning finds phantom version-16 prefixes in its integers and transform.
    if key == "PlLd" {
        let at = data.get(8).map(|n| 8 + 1 + usize::from(*n) + 16 + 64 + 4);
        return at.and_then(|a| data.get(a..)).and_then(|d| VersionedDescriptor::parse_prefix(d).ok()).map(|(d, _)| d.descriptor).into_iter().collect();
    }
    let mut out = Vec::new();
    let mut start = 0usize;
    'outer: while start < data.len() {
        let end = (start + 96).min(data.len().saturating_sub(4));
        let mut i = start;
        while i < end {
            if data[i..i + 4] == [0, 0, 0, 16]
                && let Ok((d, n)) = VersionedDescriptor::parse_prefix(&data[i..])
                && n > 12
            {
                out.push(d.descriptor);
                start = i + n;
                continue 'outer;
            }
            i += 1;
        }
        break;
    }
    out
}

fn engine_keys(data: &[u8], out: &mut BTreeSet<String>) {
    let mut i = 0;
    while i < data.len() {
        if data[i] == b'/' {
            let s = i + 1;
            let mut e = s;
            while e < data.len() && data[e].is_ascii_alphanumeric() {
                e += 1;
            }
            if e > s {
                out.insert(format!("EngineData:/{}", String::from_utf8_lossy(&data[s..e])));
            }
            i = e;
        } else {
            i += 1;
        }
    }
}

fn flatten(prefix: &str, v: &Value, depth: usize, out: &mut BTreeSet<String>) {
    if depth > 24 {
        return;
    }
    out.insert(prefix.to_string());
    match v {
        Value::Descriptor(d) | Value::GlobalObject(d) => flatten_desc(prefix, d, depth + 1, out),
        Value::List(items) => {
            for it in items {
                flatten(&format!("{prefix}[]"), it, depth + 1, out);
            }
        }
        Value::ObjectArray(a) => flatten_desc(&format!("{prefix}[ObAr]"), &a.body, depth + 1, out),
        Value::RawData(d) if prefix.ends_with("EngineData") => engine_keys(d, out),
        _ => {}
    }
}

fn flatten_desc(prefix: &str, d: &Descriptor, depth: usize, out: &mut BTreeSet<String>) {
    for (k, v) in &d.items {
        let k = String::from_utf8_lossy(k.as_bytes()).trim_end().to_string();
        let p = if prefix.is_empty() { k } else { format!("{prefix}.{k}") };
        flatten(&p, v, depth, out);
    }
}

fn paths_of(key: &str, data: &[u8]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (i, d) in descriptors(key, data).iter().enumerate() {
        let class = String::from_utf8_lossy(d.class_id.as_bytes()).trim_end().to_string();
        flatten_desc(&format!("#{i}<{class}>"), d, 0, &mut out);
    }
    out
}

impl Census {
    fn stat(&mut self, item: String, file: &str) -> &mut Stat {
        let s = self.items.entry(item).or_default();
        s.files.insert(file.to_string());
        s
    }

    fn bad(&mut self, item: String, file: &str) -> &mut Stat {
        let s = self.stat(item, file);
        s.example.get_or_insert_with(|| file.to_string());
        s
    }

    fn compare_paths(&mut self, key: &str, a: &[u8], b: &[u8]) {
        let (pa, pb) = (paths_of(key, a), paths_of(key, b));
        for p in pa.difference(&pb) {
            *self.lost_paths.entry(key.to_string()).or_default().entry(p.clone()).or_default() += 1;
        }
        for p in pb.difference(&pa) {
            *self.added_paths.entry(key.to_string()).or_default().entry(p.clone()).or_default() += 1;
        }
    }

    fn compare_blocks(&mut self, scope: &str, file: &str, orig: &[TaggedBlock], exp: &[TaggedBlock]) {
        let mut seen = BTreeSet::new();
        for b in orig {
            let k = key_str(&b.key);
            seen.insert(b.key);
            let item = format!("{scope}:{k}");
            match exp.iter().find(|e| e.key == b.key) {
                Some(e) if e.data == b.data => self.stat(item, file).same += 1,
                Some(e) => {
                    self.bad(item, file).changed += 1;
                    self.compare_paths(&k, &b.data, &e.data);
                }
                None => {
                    self.bad(item, file).lost += 1;
                    self.compare_paths(&k, &b.data, &[]);
                }
            }
        }
        for e in exp {
            if !seen.contains(&e.key) {
                let k = key_str(&e.key);
                self.bad(format!("{scope}:{k}"), file).added += 1;
            }
        }
    }

    fn compare_layers(&mut self, file: &str, orig: &[LayerRecord], exp: &[LayerRecord], verbose: bool) {
        self.layers_orig += orig.len();
        if orig.len() != exp.len() {
            self.layer_count_changed.push((file.to_string(), orig.len(), exp.len()));
        }
        let by_id: BTreeMap<u32, &LayerRecord> = exp.iter().filter_map(|l| l.layer_id().map(|id| (id, l))).collect();
        for (i, l) in orig.iter().enumerate() {
            let m = l.layer_id().and_then(|id| by_id.get(&id).copied()).or_else(|| (orig.len() == exp.len()).then(|| &exp[i]));
            let Some(e) = m else {
                self.bad("layer:<unmatched>".into(), file).lost += 1;
                continue;
            };
            self.layers_matched += 1;
            if verbose {
                let ko: Vec<_> = l.blocks.iter().map(|b| key_str(&b.key)).collect();
                let ke: Vec<_> = e.blocks.iter().map(|b| key_str(&b.key)).collect();
                println!("  layer {i} {:?}: {ko:?} -> {ke:?}", l.name());
            }
            if verbose {
                println!("    flags {:?}->{:?} blend {:?}->{:?} chans {:?}->{:?} rect {:?}->{:?}", l.flags, e.flags, l.blend_mode, e.blend_mode,
                    l.channels.iter().map(|c| c.id).collect::<Vec<_>>(), e.channels.iter().map(|c| c.id).collect::<Vec<_>>(), l.rect, e.rect);
                if l.blending_ranges != e.blending_ranges {
                    println!("    ranges {:?}\n        -> {:?}", l.blending_ranges, e.blending_ranges);
                }
                if l.mask != e.mask {
                    println!("    mask {:?}\n      -> {:?}", l.mask, e.mask);
                }
            }
            self.compare_record(file, l, e);
            self.compare_blocks("layer", file, &l.blocks, &e.blocks);
        }
    }

    fn compare_record(&mut self, file: &str, l: &LayerRecord, e: &LayerRecord) {
        let fields: [(&str, bool); 8] = [
            ("rect", l.rect == e.rect),
            ("blend_mode", l.blend_mode == e.blend_mode),
            ("opacity", l.opacity == e.opacity),
            ("clipping", l.clipping == e.clipping),
            ("flags", l.flags == e.flags),
            ("mask", l.mask == e.mask),
            ("blending_ranges", l.blending_ranges == e.blending_ranges),
            ("name", l.name() == e.name()),
        ];
        let chans = |r: &LayerRecord| r.channels.iter().map(|c| c.id).collect::<BTreeSet<_>>();
        for (f, same) in fields.into_iter().chain([("channel_ids", chans(l) == chans(e))]) {
            if same {
                self.stat(format!("record:{f}"), file).same += 1;
            } else {
                self.bad(format!("record:{f}"), file).changed += 1;
            }
        }
    }

    fn file(&mut self, path: &Path, verbose: bool) {
        let name = path.display().to_string();
        let Ok(bytes) = std::fs::read(path) else { return };
        let Ok(orig) = PsdFile::from_bytes(&bytes) else {
            self.failed.push((name, "original does not parse".into()));
            return;
        };
        let run = catch_unwind(AssertUnwindSafe(|| {
            let (mut doc, _) = photosuite_io::psd_to_document(&orig);
            if std::env::var_os("REGEN").is_some() {
                let dpi = doc.resolution_dpi;
                regenerate(&mut doc.layers, dpi);
            }
            let (exp, warnings) = photosuite_io::document_to_psd_with(&doc, &Default::default());
            let bytes = exp.to_bytes().map_err(|e| e.to_string())?;
            if let Some(out) = std::env::var_os("OUT") {
                std::fs::write(out, &bytes).map_err(|e| e.to_string())?;
            }
            PsdFile::from_bytes(&bytes).map(|f| (f, warnings)).map_err(|e| e.to_string())
        }));
        let (exp, warnings) = match run {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                self.failed.push((name, e));
                return;
            }
            Err(_) => {
                self.failed.push((name, "PANIC".into()));
                return;
            }
        };
        if verbose {
            println!("{name}: {} warnings {warnings:?}", warnings.len());
        }
        self.compare(&name, &orig, &exp, verbose);
    }

    /// Compares two parsed files: `orig` as the reference, `exp` as the one under test.
    fn compare(&mut self, name: &str, orig: &PsdFile, exp: &PsdFile, verbose: bool) {
        let name = name.to_string();
        self.files += 1;
        {
        }
        if orig.header.color_mode == exp.header.color_mode && orig.header.depth == exp.header.depth {
            self.stat("header:color_mode+depth".into(), &name).same += 1;
        } else {
            self.bad("header:color_mode+depth".into(), &name).changed += 1;
            if verbose {
                println!("  header {:?}/{} -> {:?}/{}", orig.header.color_mode, orig.header.depth, exp.header.color_mode, exp.header.depth);
            }
        }
        if orig.header.channels == exp.header.channels {
            self.stat("header:channels".into(), &name).same += 1;
        } else {
            self.bad("header:channels".into(), &name).changed += 1;
            if verbose {
                println!("  header channels {} -> {}", orig.header.channels, exp.header.channels);
            }
        }
        // The merged image's pixels (Photoshop's composite vs ours: equal for flat files).
        match (orig.decode_merged(), exp.decode_merged()) {
            (Ok(a), Ok(b)) if a == b => self.stat("image:merged".into(), &name).same += 1,
            _ => self.bad("image:merged".into(), &name).changed += 1,
        }
        if orig.color_mode_data == exp.color_mode_data {
            self.stat("header:color_mode_data".into(), &name).same += 1;
        } else {
            self.bad("header:color_mode_data".into(), &name).changed += 1;
        }
        let mut seen = BTreeSet::new();
        for r in &orig.resources {
            seen.insert(r.id);
            let item = format!("res:{}", r.id);
            match exp.resources.iter().find(|e| e.id == r.id) {
                Some(e) if e.data == r.data => self.stat(item, &name).same += 1,
                Some(_) => self.bad(item, &name).changed += 1,
                None => self.bad(item, &name).lost += 1,
            }
        }
        for e in &exp.resources {
            if !seen.contains(&e.id) {
                self.bad(format!("res:{}", e.id), &name).added += 1;
            }
        }
        self.compare_blocks("global", &name, &orig.global_blocks, &exp.global_blocks);
        let s = self.stat("global:<global layer mask>".into(), &name);
        match (&orig.global_layer_mask, &exp.global_layer_mask) {
            (Some(a), Some(b)) if a.data == b.data => s.same += 1,
            (Some(_), Some(_)) => s.changed += 1,
            (Some(_), None) => s.lost += 1,
            (None, Some(_)) => s.added += 1,
            (None, None) => {}
        }
        self.compare_layers(&name, orig.layers(), exp.layers(), verbose);
    }
}

/// A fill changed by a hair, so the export regenerates its block (merged onto the original, as
/// for a real edit).
fn nudge(f: &mut photosuite_doc::Fill) {
    use photosuite_doc::Fill;
    match f {
        Fill::Solid(c) => c.c[0] = (c.c[0] + 1e-4).min(1.0) - if c.c[0] >= 1.0 { 2e-4 } else { 0.0 },
        Fill::Gradient { angle, .. } => *angle += 1e-3,
        Fill::Pattern { scale, .. } => *scale *= 1.0001,
    }
}

/// See `REGEN=1` above.
fn regenerate(layers: &mut [photosuite_doc::Layer], dpi: f32) {
    use photosuite_doc::LayerContent;
    for l in layers {
        if !l.effects.items.is_empty() {
            l.effects.enabled = !l.effects.enabled;
        }
        l.psd_blocks.retain(|(k, _)| !photosuite_io::ADJUSTMENT_KEYS.contains(&k));
        match &mut l.content {
            LayerContent::Group(g) => regenerate(&mut g.children, dpi),
            LayerContent::Text(t) if t.psd_raw.is_some() => t.psd_raw = Some(std::sync::Arc::new(photosuite_text::psd::build_tysh(t, dpi, None))),
            LayerContent::Smart(sm) => sm.transform.m[4] += 1e-6,
            LayerContent::Fill(f) => nudge(f),
            LayerContent::Shape(sh) => {
                sh.psd_raw = None;
                if let Some(f) = &mut sh.fill {
                    nudge(f);
                }
            }
            _ => {}
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut files = Vec::new();
    for a in &args {
        collect(Path::new(a), &mut files);
    }
    let top: usize = std::env::var("TOP").ok().and_then(|v| v.parse().ok()).unwrap_or(25);
    let mut c = Census::default();
    if std::env::var_os("PAIR").is_some() {
        let [a, b] = &files[..] else { panic!("PAIR=1 takes exactly two files") };
        let parse = |p: &PathBuf| PsdFile::from_bytes(&std::fs::read(p).expect("read")).expect("parse");
        c.compare(&b.display().to_string(), &parse(a), &parse(b), true);
    } else {
        let verbose = files.len() == 1;
        for f in &files {
            c.file(f, verbose);
        }
    }
    println!("# PSD structural round-trip census\n");
    println!("{} files round-tripped, {} failed; layers {} (matched {})\n", c.files, c.failed.len(), c.layers_orig, c.layers_matched);
    for (f, e) in &c.failed {
        println!("- FAILED {f}: {e}");
    }
    if !c.layer_count_changed.is_empty() {
        println!("\nLayer count changed in {} files:", c.layer_count_changed.len());
        for (f, a, b) in c.layer_count_changed.iter().take(top) {
            println!("- {f}: {a} -> {b}");
        }
    }
    println!("\n| item | files | same | changed | lost | added | example |\n|---|--:|--:|--:|--:|--:|---|");
    for (k, s) in &c.items {
        println!("| {k} | {} | {} | {} | {} | {} | {} |", s.files.len(), s.same, s.changed, s.lost, s.added, s.example.as_deref().unwrap_or(""));
    }
    for (title, map) in [("Descriptor paths lost", &c.lost_paths), ("Descriptor paths added", &c.added_paths)] {
        println!("\n## {title} (per block key, top {top} by count)\n");
        for (k, paths) in map {
            let mut v: Vec<_> = paths.iter().collect();
            v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            println!("### {k} ({} distinct)", v.len());
            for (p, n) in v.into_iter().take(top) {
                println!("- {n} × {p}");
            }
        }
    }
}
