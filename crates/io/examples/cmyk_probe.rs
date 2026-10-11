//! Probes how Photoshop blends a CMYK document (rendering-fidelity research, not a test).
//!
//! For every layer with a non-Normal blend mode, takes the layer right below it as the backdrop
//! and, at pixels where both are opaque, compares Photoshop's merged CMYK value with the blend of
//! the two layers' raw CMYK values, per channel:
//! - `stored`: the blend applied to the stored values (PSD stores CMYK inverted: 1 − ink);
//! - `ink`: the blend applied to ink amounts (1 − stored), converted back.
//!
//! ```sh
//! cargo run --release -p photosuite-io --example cmyk_probe -- corpus/psd-tools/blend-modes/cmyk-blend-modes.psd
//! ```
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use photosuite_psd::{LayerRecord, PsdFile};

/// A named lightness measure of a pixel's inks.
type Measure<'a> = (&'a str, &'a dyn Fn([f32; 4]) -> f32);

struct Planes {
    rect: (i32, i32, i32, i32),
    chans: Vec<(i16, Vec<u8>)>,
}

impl Planes {
    fn of(f: &PsdFile, r: &LayerRecord) -> Self {
        let (t, l, b, rt) = (r.rect.top, r.rect.left, r.rect.bottom, r.rect.right);
        let (w, h) = ((rt - l).max(0) as usize, (b - t).max(0) as usize);
        let chans = r
            .channels
            .iter()
            .filter(|c| c.id >= -1)
            .map(|c| (c.id, c.decode(w, h, f.header.depth, f.header.version).unwrap_or_default()))
            .collect();
        Planes { rect: (t, l, b, rt), chans }
    }
    fn get(&self, id: i16, x: i32, y: i32) -> Option<f32> {
        let (t, l, b, r) = self.rect;
        if x < l || x >= r || y < t || y >= b {
            return None;
        }
        let w = (r - l) as usize;
        let p = &self.chans.iter().find(|(i, _)| *i == id)?.1;
        p.get((y - t) as usize * w + (x - l) as usize).map(|v| f32::from(*v) / 255.0)
    }
}

fn blend(mode: &[u8; 4], b: f32, s: f32) -> Option<f32> {
    Some(match mode {
        b"mul " => b * s,
        b"scrn" => b + s - b * s,
        b"dark" => b.min(s),
        b"lite" => b.max(s),
        b"diff" => (b - s).abs(),
        b"smud" => b + s - 2.0 * b * s,
        b"over" => {
            if b <= 0.5 {
                2.0 * b * s
            } else {
                1.0 - 2.0 * (1.0 - b) * (1.0 - s)
            }
        }
        b"hLit" => {
            if s <= 0.5 {
                2.0 * b * s
            } else {
                1.0 - 2.0 * (1.0 - b) * (1.0 - s)
            }
        }
        b"lddg" => (b + s).min(1.0),
        b"lbrn" => (b + s - 1.0).max(0.0),
        b"fsub" => (b - s).max(0.0),
        _ => return None,
    })
}

/// Non-separable modes: the complements of C, M, Y as an RGB colour.
fn nonsep(mode: &[u8; 4]) -> Option<photosuite_color::blend::BlendMode> {
    use photosuite_color::blend::BlendMode as M;
    Some(match mode {
        b"hue " => M::Hue,
        b"sat " => M::Saturation,
        b"colr" => M::Color,
        b"lum " => M::Luminosity,
        b"dkCl" => M::DarkerColor,
        b"lgCl" => M::LighterColor,
        _ => return None,
    })
}

/// For a non-separable mode: CMY error with the complements blended as RGB, and the K error of
/// each candidate rule (backdrop K, source K, and K blended like a gray channel).
fn probe_nonsep(f: &PsdFile, recs: &[LayerRecord], i: usize, m: &dyn Fn(usize, i32, i32) -> f32, w: usize, h: usize) {
    let Some(mode) = nonsep(&recs[i].blend_mode.key()) else { return };
    let (top, bot) = (Planes::of(f, &recs[i]), Planes::of(f, &recs[i - 1]));
    let (t, l, b, r) = top.rect;
    let (mut n, mut e_cmy, mut e_kb, mut e_ks, mut e_kmix) = (0usize, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for y in t.max(0)..b.min(h as i32) {
        for x in l.max(0)..r.min(w as i32) {
            if top.get(-1, x, y) != Some(1.0)
                || bot.get(-1, x, y).is_some_and(|a| a < 1.0)
                || bot.get(0, x - 3, y - 3).is_none()
                || bot.get(0, x + 3, y + 3).is_none()
            {
                continue;
            }
            let g = |p: &Planes, c: i16| p.get(c, x, y).unwrap_or(0.0);
            let cb = [g(&bot, 0), g(&bot, 1), g(&bot, 2)];
            let cs = [g(&top, 0), g(&top, 1), g(&top, 2)];
            let out = photosuite_color::blend::blend_rgb(mode, cb, cs);
            for (c, v) in out.iter().enumerate() {
                e_cmy += (v - m(c, x, y)).abs();
            }
            let (kb, ks, want) = (g(&bot, 3), g(&top, 3), m(3, x, y));
            let kmix = photosuite_color::blend::blend_rgb(mode, [kb; 3], [ks; 3])[0];
            e_kb += (kb - want).abs();
            e_ks += (ks - want).abs();
            e_kmix += (kmix - want).abs();
            n += 1;
        }
    }
    if n > 0 {
        let n = n as f32;
        println!(
            "{:<20} {:?}: CMY as RGB mean {:.4}; K = backdrop {:.4}, source {:.4}, gray blend {:.4}",
            recs[i].name(),
            mode,
            e_cmy / (3.0 * n),
            e_kb / n,
            e_ks / n,
            e_kmix / n
        );
    }
}

/// `CURVE=1`: the transfer curve of an adjustment over one pixel layer (the first record), per
/// channel: the mean merged value for each stored input value (8-bit CMYK or RGB files).
fn curve(f: &PsdFile, merged: &[u8], w: usize, h: usize) {
    let base = Planes::of(f, &f.layers()[0]);
    let colors = if f.header.color_mode == photosuite_psd::ColorMode::Cmyk { 4 } else { 3 };
    for c in 0..colors {
        let mut sum = vec![(0.0f64, 0usize); 256];
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let Some(v) = base.get(c as i16, x, y) else { continue };
                let i = (v * 255.0).round() as usize;
                sum[i].0 += f64::from(merged[c * w * h + y as usize * w + x as usize]);
                sum[i].1 += 1;
            }
        }
        let row: Vec<String> = (0..256).step_by(17).map(|i| if sum[i].1 > 0 { format!("{i}:{:.1}", sum[i].0 / sum[i].1 as f64) } else { format!("{i}:-") }).collect();
        println!("ch{c} {}", row.join(" "));
    }
}

/// `THRESH=1`: which lightness measure of the first layer's inks Photoshop's Threshold splits on:
/// for each candidate, the fewest pixels misclassified by any cut (black = merged K full ink).
fn thresh(f: &PsdFile, bytes: &[u8], merged: &[u8], w: usize, h: usize) {
    use photosuite_color::convert::{cmyk_to_rgb, rgb_to_gray, srgb_to_lab, with_cmyk_space};
    let doc = photosuite_io::import("x.psd", bytes).unwrap().document;
    let space = photosuite_compose::cmyk_space(&doc);
    let base = Planes::of(f, &f.layers()[0]);
    let mut rows: Vec<([f32; 4], bool)> = Vec::new();
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let Some(c) = (0..4).map(|c| base.get(c, x, y).map(|v| 1.0 - v)).collect::<Option<Vec<f32>>>() else { continue };
            let black = merged[3 * w * h + y as usize * w + x as usize] < 128;
            rows.push(([c[0], c[1], c[2], c[3]], black));
        }
    }
    let metrics: [Measure; 4] = [
        ("screen gray", &|c| rgb_to_gray(cmyk_to_rgb(c))),
        ("Lab L*", &|c| srgb_to_lab(cmyk_to_rgb(c))[0] / 100.0),
        ("naive", &|c| (1.0 - (0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]) ).max(0.0) * (1.0 - c[3])),
        ("1 - K - mean CMY / 3", &|c| 1.0 - c[3] - (c[0] + c[1] + c[2]) / 3.0),
    ];
    with_cmyk_space(space.as_ref(), || {
        for (name, m) in metrics {
            let mut v: Vec<(f32, bool)> = rows.iter().map(|(c, b)| (m(*c), *b)).collect();
            v.sort_by(|a, b| a.0.total_cmp(&b.0));
            // Cut at index i: below = black. Errors = whites below + blacks above.
            let total_black = v.iter().filter(|r| r.1).count();
            let (mut best, mut at, mut whites_below, mut blacks_below) = (usize::MAX, 0.0, 0usize, 0usize);
            for (i, r) in v.iter().enumerate() {
                let err = whites_below + (total_black - blacks_below);
                if err < best {
                    (best, at) = (err, r.0);
                }
                if r.1 { blacks_below += 1 } else { whites_below += 1 }
                let _ = i;
            }
            println!("{name:<22} best cut {:.4} ({:.1}/255): {best} of {} pixels misclassified", at, at * 255.0, v.len());
        }
    });
}

/// `GMAP=1`: a CMYK gradient map over one pixel layer: Photoshop's merged inks against the
/// file's CMYK stops interpolated linearly (on stored values, 1 − ink) at each lightness measure.
fn gmap(f: &PsdFile, bytes: &[u8], merged: &[u8], w: usize, h: usize) {
    use photosuite_color::convert::{cmyk_to_rgb, rgb_to_gray, with_cmyk_space};
    let rec = &f.layers()[1];
    let d = &rec.blocks.iter().find(|b| &b.key == b"grdm").expect("grdm").data;
    let be16 = |at: usize| u16::from_be_bytes([d[at], d[at + 1]]);
    let be32 = |at: usize| u32::from_be_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]]);
    let mut at = if be16(0) == 3 { 8 } else { 4 };
    at += 4 + be32(at) as usize * 2;
    let n = be16(at) as usize;
    at += 2;
    let mut stops = Vec::new();
    for _ in 0..n {
        let loc = be32(at) as f32 / 4096.0;
        let v: Vec<f32> = (0..4).map(|k| f32::from(be16(at + 10 + 2 * k)) / 65535.0).collect();
        stops.push((loc, [v[0], v[1], v[2], v[3]]));
        at += 20;
    }
    println!("stops (stored, 1 − ink): {stops:?}");
    let lerp = |t: f32| -> [f32; 4] {
        let t = t.clamp(0.0, 1.0);
        let i = stops.iter().rposition(|s| s.0 <= t).unwrap_or(0);
        let j = (i + 1).min(stops.len() - 1);
        let (a, b) = (stops[i], stops[j]);
        let k = if b.0 > a.0 { (t - a.0) / (b.0 - a.0) } else { 0.0 };
        std::array::from_fn(|c| a.1[c] + (b.1[c] - a.1[c]) * k)
    };
    let doc = photosuite_io::import("x.psd", bytes).unwrap().document;
    let space = photosuite_compose::cmyk_space(&doc);
    let base = Planes::of(f, &f.layers()[0]);
    let metrics: [Measure; 2] = [
        ("ink lightness", &|s| (0.30 * s[0] + 0.59 * s[1] + 0.11 * s[2]) * s[3]),
        ("screen gray", &|s| rgb_to_gray(cmyk_to_rgb([1.0 - s[0], 1.0 - s[1], 1.0 - s[2], 1.0 - s[3]]))),
    ];
    with_cmyk_space(space.as_ref(), || {
        for (name, m) in metrics {
            let (mut err, mut cnt) = ([0.0f32; 4], 0usize);
            for y in 0..h as i32 {
                for x in 0..w as i32 {
                    let Some(s) = (0..4).map(|c| base.get(c, x, y)).collect::<Option<Vec<f32>>>() else { continue };
                    let want = lerp(m([s[0], s[1], s[2], s[3]]));
                    for c in 0..4 {
                        err[c] += (want[c] - f32::from(merged[c * w * h + y as usize * w + x as usize]) / 255.0).abs();
                    }
                    cnt += 1;
                }
            }
            println!("{name:<14} mean error per channel {:?}", err.map(|e| e / cnt.max(1) as f32));
        }
    });
}

/// `ADJ=1`: an adjustment over one pixel layer, mean error per ink against Photoshop's merged
/// image for: (a) the RGB adjustment run on the C, M, Y complements as RGB with K kept, (b) the
/// same with K through it as a gray, (c) the screen colour through the profile, separated back.
fn adj(f: &PsdFile, bytes: &[u8], merged: &[u8], w: usize, h: usize) {
    use photosuite_color::convert::{cmyk_to_rgb, rgb_to_cmyk, with_cmyk_space};
    use photosuite_compose::adjust::{Transfer, apply_depth};
    let doc = photosuite_io::import("x.psd", bytes).unwrap().document;
    let Some(photosuite_doc::LayerContent::Adjustment(a)) = doc.layers.get(1).map(|l| &l.content) else { panic!("no adjustment") };
    println!("{a:?}");
    let space = photosuite_compose::cmyk_space(&doc);
    let transfer = Transfer::for_document(photosuite_color::ColorMode::Rgb, doc.depth);
    let base = Planes::of(f, &f.layers()[0]);
    let run = |rgb: [f32; 3]| {
        let mut b = photosuite_compose::Buffer { rect: photosuite_geom::Rect::new(0, 0, 1, 1), px: vec![[rgb[0], rgb[1], rgb[2], 1.0]] };
        apply_depth(a, &mut b, transfer, Some(doc.depth));
        [b.px[0][0], b.px[0][1], b.px[0][2]]
    };
    with_cmyk_space(space.as_ref(), || {
        let mut err = [[0.0f32; 4]; 3];
        let mut n = 0usize;
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let Some(s) = (0..4).map(|c| base.get(c, x, y)).collect::<Option<Vec<f32>>>() else { continue };
                let want: Vec<f32> = (0..4).map(|c| f32::from(merged[c * w * h + y as usize * w + x as usize]) / 255.0).collect();
                let cmy = run([s[0], s[1], s[2]]);
                let k = run([s[3]; 3])[0];
                let screen = run(cmyk_to_rgb([1.0 - s[0], 1.0 - s[1], 1.0 - s[2], 1.0 - s[3]]));
                let sep = rgb_to_cmyk(screen).map(|v| 1.0 - v);
                let cand = [[cmy[0], cmy[1], cmy[2], s[3]], [cmy[0], cmy[1], cmy[2], k], sep];
                for (e, c) in err.iter_mut().zip(cand) {
                    for i in 0..4 {
                        e[i] += (c[i] - want[i]).abs();
                    }
                }
                n += 1;
            }
        }
        for (name, e) in ["CMY as RGB, K kept", "CMY as RGB, K as gray", "screen, separated"].iter().zip(err) {
            println!("{name:<22} {:?}", e.map(|v| v / n.max(1) as f32));
        }
    });
}

/// `ADJ=1` on a Lab file: the adjustment over one pixel layer against Photoshop's merged L, a,
/// b (stored 0..1, a and b around 0.5) for: (a) run on L, a, b as an RGB colour, (b) on L only
/// (as a gray), a and b kept, (c) the screen colour through Lab ↔ sRGB (today's route).
fn adj_lab(f: &PsdFile, bytes: &[u8], merged: &[u8], w: usize, h: usize) {
    use photosuite_color::convert::{lab_to_srgb, srgb_to_lab};
    use photosuite_compose::adjust::{Transfer, apply_depth};
    let doc = photosuite_io::import("x.psd", bytes).unwrap().document;
    let Some(photosuite_doc::LayerContent::Adjustment(a)) = doc.layers.get(1).map(|l| &l.content) else { panic!("no adjustment") };
    println!("{a:?}");
    let transfer = Transfer::for_document(photosuite_color::ColorMode::Rgb, doc.depth);
    let base = Planes::of(f, &f.layers()[0]);
    let run = |rgb: [f32; 3]| {
        let mut b = photosuite_compose::Buffer { rect: photosuite_geom::Rect::new(0, 0, 1, 1), px: vec![[rgb[0], rgb[1], rgb[2], 1.0]] };
        apply_depth(a, &mut b, transfer, Some(doc.depth));
        [b.px[0][0], b.px[0][1], b.px[0][2]]
    };
    let (mut err, mut n) = ([[0.0f32; 3]; 3], 0usize);
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let Some(s) = (0..3).map(|c| base.get(c, x, y)).collect::<Option<Vec<f32>>>() else { continue };
            let want: Vec<f32> = (0..3).map(|c| f32::from(merged[c * w * h + y as usize * w + x as usize]) / 255.0).collect();
            let as_rgb = run([s[0], s[1], s[2]]);
            let l_only = [run([s[0]; 3])[0], s[1], s[2]];
            let screen = run(lab_to_srgb([s[0] * 100.0, s[1] * 255.0 - 128.0, s[2] * 255.0 - 128.0]));
            let back = srgb_to_lab(screen);
            let via = [back[0] / 100.0, (back[1] + 128.0) / 255.0, (back[2] + 128.0) / 255.0];
            for (e, c) in err.iter_mut().zip([as_rgb, l_only, via]) {
                for i in 0..3 {
                    e[i] += (c[i] - want[i]).abs();
                }
            }
            n += 1;
        }
    }
    for (name, e) in ["L, a, b as RGB", "L only, a b kept", "screen (today)"].iter().zip(err) {
        println!("{name:<18} {:?}", e.map(|v| v / n.max(1) as f32));
    }
}

/// `MIXR=1`: a CMYK Channel Mixer over one pixel layer: mean error per output ink for the mix on
/// ink amounts and on complements (records: C, M, Y, K inputs and a constant, in percent).
fn mixr(f: &PsdFile, merged: &[u8], w: usize, h: usize) {
    let d = &f.layers()[1].blocks.iter().find(|b| &b.key == b"mixr").expect("mixr").data;
    let v = |i: usize| f32::from(i16::from_be_bytes([d[i], d[i + 1]])) / 100.0;
    let recs: Vec<[f32; 5]> = (0..4).map(|r| std::array::from_fn(|k| v(4 + r * 10 + k * 2))).collect();
    println!("records {recs:?}");
    let base = Planes::of(f, &f.layers()[0]);
    let (mut e_ink, mut e_cmp, mut n) = ([0.0f32; 4], [0.0f32; 4], 0usize);
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let Some(s) = (0..4).map(|c| base.get(c, x, y)).collect::<Option<Vec<f32>>>() else { continue };
            let ink: Vec<f32> = s.iter().map(|v| 1.0 - v).collect();
            for (o, r) in recs.iter().enumerate() {
                let want = f32::from(merged[o * w * h + y as usize * w + x as usize]) / 255.0;
                let mi = (0..4).map(|i| r[i] * ink[i]).sum::<f32>() + r[4];
                let mc = (0..4).map(|i| r[i] * s[i]).sum::<f32>() + r[4];
                e_ink[o] += (1.0 - mi.clamp(0.0, 1.0) - want).abs();
                e_cmp[o] += (mc.clamp(0.0, 1.0) - want).abs();
            }
            n += 1;
        }
    }
    let k = n.max(1) as f32;
    println!("on inks        {:?}\non complements {:?}", e_ink.map(|v| v / k), e_cmp.map(|v| v / k));
}

fn main() {
    let path = std::env::args().nth(1).expect("file.psd");
    let bytes = std::fs::read(&path).unwrap();
    let f = PsdFile::from_bytes(&bytes).unwrap();
    if std::env::var_os("MIXR").is_some() {
        let merged = f.decode_merged().unwrap();
        mixr(&f, &merged, f.header.width as usize, f.header.height as usize);
        return;
    }
    if std::env::var_os("ADJ").is_some() {
        let merged = f.decode_merged().unwrap();
        if f.header.color_mode == photosuite_psd::ColorMode::Lab {
            adj_lab(&f, &bytes, &merged, f.header.width as usize, f.header.height as usize);
            return;
        }
        adj(&f, &bytes, &merged, f.header.width as usize, f.header.height as usize);
        return;
    }
    if std::env::var_os("GMAP").is_some() {
        let merged = f.decode_merged().unwrap();
        gmap(&f, &bytes, &merged, f.header.width as usize, f.header.height as usize);
        return;
    }
    if std::env::var_os("THRESH").is_some() {
        let merged = f.decode_merged().unwrap();
        thresh(&f, &bytes, &merged, f.header.width as usize, f.header.height as usize);
        return;
    }
    if std::env::var_os("CURVE").is_some() {
        let merged = f.decode_merged().unwrap();
        curve(&f, &merged, f.header.width as usize, f.header.height as usize);
        return;
    }
    assert_eq!(f.header.depth, 8, "8-bit files only");
    let merged = f.decode_merged().unwrap();
    let (w, h) = (f.header.width as usize, f.header.height as usize);
    let m = |c: usize, x: i32, y: i32| f32::from(merged[c * w * h + y as usize * w + x as usize]) / 255.0;
    // CMYK: four inks; Lab (and RGB): three channels.
    let colors = if f.header.color_mode == photosuite_psd::ColorMode::Cmyk { 4 } else { 3 };
    let recs = f.layers();
    for i in 1..recs.len() {
        probe_nonsep(&f, recs, i, &m, w, h);
        let mode = recs[i].blend_mode.key();
        if blend(&mode, 0.5, 0.5).is_none() {
            continue;
        }
        let (top, bot) = (Planes::of(&f, &recs[i]), Planes::of(&f, &recs[i - 1]));
        let (mut n, mut e_stored, mut e_ink, mut e_max) = (0usize, 0.0f32, 0.0f32, 0.0f32);
        let (mut e_srgb, mut n_srgb) = (0.0f32, 0usize);
        let lab = f.header.color_mode == photosuite_psd::ColorMode::Lab;
        let (t, l, b, r) = top.rect;
        for y in t.max(0)..b.min(h as i32) {
            for x in l.max(0)..r.min(w as i32) {
                // Opaque in both, away from the backdrop's (vector-masked) edges.
                let (Some(ta), Some(_)) = (top.get(-1, x, y), bot.get(0, x - 3, y - 3).and(bot.get(0, x + 3, y + 3))) else { continue };
                if ta < 1.0 || bot.get(-1, x, y).is_some_and(|a| a < 1.0) {
                    continue;
                }
                // Lab: the blend of the two colours' sRGB, back in Lab.
                if lab {
                    use photosuite_color::convert::{lab_to_srgb, srgb_to_lab};
                    let px = |p: &Planes| (0..3).map(|c| p.get(c, x, y)).collect::<Option<Vec<f32>>>();
                    if let (Some(sv), Some(bv)) = (px(&top), px(&bot)) {
                        let rgb = |v: &[f32]| lab_to_srgb([v[0] * 100.0, v[1] * 255.0 - 128.0, v[2] * 255.0 - 128.0]);
                        let (sr, br) = (rgb(&sv), rgb(&bv));
                        let out: [f32; 3] = std::array::from_fn(|c| blend(&mode, br[c], sr[c]).unwrap());
                        let o = srgb_to_lab(out);
                        let o = [o[0] / 100.0, (o[1] + 128.0) / 255.0, (o[2] + 128.0) / 255.0];
                        for (c, v) in o.iter().enumerate() {
                            e_srgb += (v - m(c, x, y)).abs();
                            n_srgb += 1;
                        }
                    }
                }
                for c in 0..colors {
                    let (Some(s), Some(bk)) = (top.get(c as i16, x, y), bot.get(c as i16, x, y)) else { continue };
                    let want = m(c, x, y);
                    let a = blend(&mode, bk, s).unwrap();
                    let k = 1.0 - blend(&mode, 1.0 - bk, 1.0 - s).unwrap();
                    e_stored += (a - want).abs();
                    e_ink += (k - want).abs();
                    e_max = e_max.max((a - want).abs());
                    n += 1;
                }
            }
        }
        // Lab: per channel, the error of the mode on stored values, of the source and of the
        // backdrop (which rule a and b follow).
        if lab {
            let mut e = [[0.0f32; 3]; 3];
            let mut cnt = 0usize;
            for y in t.max(0)..b.min(h as i32) {
                for x in l.max(0)..r.min(w as i32) {
                    if top.get(-1, x, y) != Some(1.0) || bot.get(-1, x, y).is_some_and(|a| a < 1.0) || bot.get(0, x - 3, y - 3).is_none() || bot.get(0, x + 3, y + 3).is_none() {
                        continue;
                    }
                    for (c, ec) in e.iter_mut().enumerate() {
                        let (Some(sv), Some(bv)) = (top.get(c as i16, x, y), bot.get(c as i16, x, y)) else { continue };
                        let want = m(c, x, y);
                        ec[0] += (blend(&mode, bv, sv).unwrap() - want).abs();
                        ec[1] += (sv - want).abs();
                        ec[2] += (bv - want).abs();
                    }
                    cnt += 1;
                }
            }
            if std::env::var_os("DUMP").is_some() && &mode == b"mul " {
                let mut shown = 0;
                for y in (t.max(0)..b.min(h as i32)).step_by(7) {
                    for x in (l.max(0)..r.min(w as i32)).step_by(7) {
                        if top.get(-1, x, y) != Some(1.0) || bot.get(-1, x, y).is_some_and(|a| a < 1.0) || shown > 24 {
                            continue;
                        }
                        let g = |p: &Planes, c: i16| (p.get(c, x, y).unwrap_or(0.0) * 255.0).round();
                        println!("  L {:>3}·{:>3}→{:>3}  a {:>3}·{:>3}→{:>3}  b {:>3}·{:>3}→{:>3}", g(&bot, 0), g(&top, 0), (m(0, x, y) * 255.0).round(), g(&bot, 1), g(&top, 1), (m(1, x, y) * 255.0).round(), g(&bot, 2), g(&top, 2), (m(2, x, y) * 255.0).round());
                        shown += 1;
                    }
                }
            }
            let k = cnt.max(1) as f32;
            println!("{:<20} {} per channel [mode, source, backdrop]: L {:?} a {:?} b {:?}", recs[i].name(), String::from_utf8_lossy(&mode), e[0].map(|v| v / k), e[1].map(|v| v / k), e[2].map(|v| v / k));
        }
        if n_srgb > 0 {
            println!("{:<20} {} via sRGB: mean {:.4}", recs[i].name(), String::from_utf8_lossy(&mode), e_srgb / n_srgb as f32);
        }
        if n > 0 {
            println!(
                "{:<20} {} samples {n:>6}  stored: mean {:.4} (max {:.3})  ink: mean {:.4}",
                recs[i].name(),
                String::from_utf8_lossy(&mode),
                e_stored / n as f32,
                e_max,
                e_ink / n as f32
            );
        }
    }
}
