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

fn main() {
    let path = std::env::args().nth(1).expect("file.psd");
    let f = PsdFile::from_bytes(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(f.header.depth, 8, "8-bit files only");
    let merged = f.decode_merged().unwrap();
    let (w, h) = (f.header.width as usize, f.header.height as usize);
    let m = |c: usize, x: i32, y: i32| f32::from(merged[c * w * h + y as usize * w + x as usize]) / 255.0;
    let recs = f.layers();
    for i in 1..recs.len() {
        probe_nonsep(&f, recs, i, &m, w, h);
        let mode = recs[i].blend_mode.key();
        if blend(&mode, 0.5, 0.5).is_none() {
            continue;
        }
        let (top, bot) = (Planes::of(&f, &recs[i]), Planes::of(&f, &recs[i - 1]));
        let (mut n, mut e_stored, mut e_ink, mut e_max) = (0usize, 0.0f32, 0.0f32, 0.0f32);
        let (t, l, b, r) = top.rect;
        for y in t.max(0)..b.min(h as i32) {
            for x in l.max(0)..r.min(w as i32) {
                // Opaque in both, away from the backdrop's (vector-masked) edges.
                let (Some(ta), Some(_)) = (top.get(-1, x, y), bot.get(0, x - 3, y - 3).and(bot.get(0, x + 3, y + 3))) else { continue };
                if ta < 1.0 || bot.get(-1, x, y).is_some_and(|a| a < 1.0) {
                    continue;
                }
                for c in 0..4 {
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
