//! Compares the gallery against reference renders (binary PPMs).
//!
//! * `against_spec`: `GALLERY_ORACLE` names a folder with `manifest.json` (an array of
//!   `{"filter", "image", "variant", "file", "params", "foreground", "background", "glowColor"}`)
//!   and `src-<image>.ppm`. Each entry is re-rendered with the spec's edge defects
//!   reproduced ([`super::px8::defects`]); this checks a filter does what its specification says.
//! * `against_reference`: `GALLERY_REF` names a folder with the source (`src.ppm`, or
//!   `src-parrot.ppm`), the reference's render of each filter at its defaults as `<key>.ppm`, and
//!   variants with one setting changed as `<key>__<param>-<value>.ppm` (black foreground, white
//!   background).
//!
//! Both print the mean absolute difference per channel in 0–255 levels, per pixel and over 8 × 8
//! block means (which ignores grain). `GALLERY_FILTER` (comma-separated keys) limits the run;
//! `GALLERY_OUT` (a folder) receives our renders.
//!
//! `GALLERY_REF=dir cargo test -p photosuite-algo --lib artistic_fx::oracle -- --ignored --nocapture`

use photosuite_color::{ColorMode, PixelFormat, SampleType};
use photosuite_geom::Rect;
use photosuite_raster::Surface;
use serde_json::Value;

use crate::artistic::{GalleryEffect, GalleryFilter, GalleryParamKind};
use crate::{FilterParams, apply_in};

pub(crate) fn read_ppm(path: &std::path::Path) -> (usize, usize, Vec<u8>) {
    let b = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut toks = Vec::new();
    let mut i = 0;
    while toks.len() < 4 {
        while b[i].is_ascii_whitespace() {
            i += 1;
        }
        if b[i] == b'#' {
            while b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let j = i;
        while !b[i].is_ascii_whitespace() {
            i += 1;
        }
        toks.push(String::from_utf8_lossy(&b[j..i]).to_string());
    }
    let (w, h): (usize, usize) = (toks[1].parse().unwrap(), toks[2].parse().unwrap());
    (w, h, b[i + 1..i + 1 + w * h * 3].to_vec())
}

fn write_ppm(path: &std::path::Path, w: usize, h: usize, px: &[u8]) {
    let mut b = format!("P6\n{w} {h}\n255\n").into_bytes();
    b.extend(px);
    std::fs::write(path, b).unwrap();
}

fn colour(v: Option<&Value>, d: [f32; 4]) -> [f32; 4] {
    match v.and_then(Value::as_array) {
        Some(a) if a.len() >= 3 => {
            let c = |j: usize| a.get(j).and_then(Value::as_f64).unwrap_or(0.0) as f32;
            [c(0), c(1), c(2), 1.0]
        }
        _ => d,
    }
}

/// The effect a manifest entry describes.
fn effect(entry: &Value) -> Option<GalleryEffect> {
    let f = GalleryFilter::from_key(entry.get("filter")?.as_str()?)?;
    let mut e = GalleryEffect::new(f);
    if let Some(p) = entry.get("params") {
        for prm in f.params() {
            match (&prm.kind, p.get(prm.key)) {
                (GalleryParamKind::Choice(_), Some(Value::String(s))) => {
                    assert!(e.set_choice(prm.key, s), "{}: unknown choice {s} for {}", f.key(), prm.key);
                }
                (_, Some(Value::Bool(b))) => e.set(prm.key, *b as u8 as f32),
                (_, Some(v)) => {
                    if let Some(n) = v.as_f64() {
                        // The spec's own ranges are sometimes wider than ours.
                        e.values.insert(prm.key.to_string(), n as f32);
                    }
                }
                _ => {}
            }
        }
        if let Some(s) = p.get("seed").and_then(Value::as_u64) {
            e.seed = Some(s as u32);
        }
    }
    // The spec seeded its unseeded randomness with this before every render.
    if f == GalleryFilter::StainedGlass && e.seed.is_none() {
        e.seed = Some(1_234_567);
    }
    e.foreground = colour(entry.get("foreground"), e.foreground);
    e.background = colour(entry.get("background"), e.background);
    e.color = colour(entry.get("glowColor"), e.color);
    Some(e)
}

/// Renders `src` (RGB8, opaque) through one effect; RGB8 out.
pub(crate) fn render(e: &GalleryEffect, w: usize, h: usize, src: &[u8]) -> Vec<u8> {
    let mut s = Surface::new(PixelFormat::new(ColorMode::Rgb, SampleType::U8, true));
    let b = Rect::new(0, 0, w as i32, h as i32);
    let data: Vec<f32> = src.chunks_exact(3).flat_map(|c| [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, 1.0]).collect();
    s.write_region(b, &data);
    let fp = FilterParams::FilterGallery { effects: vec![e.clone()] };
    let out = apply_in(&s, &fp, b, b, None, b).read_region(b);
    out.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]].map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)).collect()
}

/// (per-pixel, 8 × 8 block) mean absolute difference.
pub(crate) fn diff(a: &[u8], b: &[u8], w: usize, h: usize) -> (f64, f64) {
    let px = a.iter().zip(b).map(|(x, y)| f64::from(x.abs_diff(*y))).sum::<f64>() / a.len() as f64;
    let mut blocks = 0.0;
    let mut n = 0.0;
    for by in (0..h - 7).step_by(8) {
        for bx in (0..w - 7).step_by(8) {
            for c in 0..3 {
                let mean = |p: &[u8]| (0..64).map(|k| f64::from(p[((by + k / 8) * w + bx + k % 8) * 3 + c])).sum::<f64>() / 64.0;
                blocks += (mean(a) - mean(b)).abs();
                n += 1.0;
            }
        }
    }
    (px, blocks / n)
}

fn only() -> Option<Vec<String>> {
    std::env::var("GALLERY_FILTER").ok().map(|s| s.split(',').map(str::to_owned).collect())
}

#[test]
#[ignore = "needs GALLERY_ORACLE"]
fn against_spec() {
    let Ok(dir) = std::env::var("GALLERY_ORACLE") else { return };
    let dir = std::path::PathBuf::from(dir);
    let manifest: Value = serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
    let only = only();
    let out_dir = std::env::var("GALLERY_OUT").ok().map(std::path::PathBuf::from);
    super::px8::set_defects(true);
    eprintln!("{:<48} {:>8} {:>8} {:>8}", "render", "pixel", "8x8", "ms");
    for entry in manifest.as_array().unwrap() {
        let key = entry["filter"].as_str().unwrap_or_default();
        if only.as_ref().is_some_and(|o| !o.iter().any(|k| k == key)) {
            continue;
        }
        let Some(e) = effect(entry) else { continue };
        let file = entry["file"].as_str().unwrap();
        let (w, h, src) = read_ppm(&dir.join(format!("src-{}.ppm", entry["image"].as_str().unwrap())));
        let (_, _, want) = read_ppm(&dir.join(file));
        let t = std::time::Instant::now();
        let got = render(&e, w, h, &src);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let (px, bl) = diff(&got, &want, w, h);
        eprintln!("{file:<48} {px:>8.2} {bl:>8.2} {ms:>8.0}");
        if let Some(o) = &out_dir {
            std::fs::create_dir_all(o).unwrap();
            write_ppm(&o.join(file), w, h, &got);
        }
    }
    super::px8::set_defects(false);
}

/// The renders in a reference folder: `<key>.ppm` (defaults) and `<key>__<param>-<value>.ppm`
/// (one setting changed: a choice name or a number), sorted by file name.
fn reference_renders(dir: &std::path::Path) -> Vec<(String, GalleryEffect)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".ppm") else { continue };
        let (key, setting) = stem.split_once("__").map_or((stem, None), |(k, s)| (k, Some(s)));
        let Some(f) = GalleryFilter::from_key(key).filter(|f| f.key() == key) else { continue };
        let mut e = GalleryEffect::new(f);
        if let Some((param, value)) = setting.and_then(|s| s.split_once('-'))
            && !e.set_choice(param, value)
        {
            e.set(param, value.parse().unwrap_or_else(|_| panic!("{name}: {param} = {value}?")));
        }
        out.push((stem.to_string(), e));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
#[ignore = "needs GALLERY_REF"]
fn against_reference() {
    let Ok(dir) = std::env::var("GALLERY_REF") else { return };
    let dir = std::path::PathBuf::from(dir);
    let only = only();
    let out_dir = std::env::var("GALLERY_OUT").ok().map(std::path::PathBuf::from);
    // The source: `src.ppm`, else the first reference set's `src-parrot.ppm`.
    let src_file = ["src.ppm", "src-parrot.ppm"].into_iter().map(|f| dir.join(f)).find(|p| p.exists()).expect("no src.ppm in GALLERY_REF");
    let (w, h, src) = read_ppm(&src_file);
    eprintln!("{:<40} {:>8} {:>8} {:>8}", "render", "pixel", "8x8", "ms");
    let (mut tp, mut tb, mut n) = (0.0, 0.0, 0.0);
    for (stem, e) in reference_renders(&dir) {
        if only.as_ref().is_some_and(|o| !o.iter().any(|k| k == e.filter.key())) {
            continue;
        }
        let (rw, rh, want) = read_ppm(&dir.join(format!("{stem}.ppm")));
        assert_eq!((rw, rh), (w, h), "{stem}: size");
        let t = std::time::Instant::now();
        let got = render(&e, w, h, &src);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let (px, bl) = diff(&got, &want, w, h);
        (tp, tb, n) = (tp + px, tb + bl, n + 1.0);
        eprintln!("{stem:<40} {px:>8.2} {bl:>8.2} {ms:>8.0}");
        if let Some(o) = &out_dir {
            std::fs::create_dir_all(o).unwrap();
            write_ppm(&o.join(format!("{stem}.ppm")), w, h, &got);
        }
    }
    if n > 0.0 {
        eprintln!("{:<40} {:>8.2} {:>8.2}", "mean", tp / n, tb / n);
    }
}
