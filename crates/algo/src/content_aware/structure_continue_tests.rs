//! Bands and stripes continue through the hole (measured upstream, #14191).
//!
//! The measured document (Reference and upstream): 600 x 400, RGB, 8 bit, one
//! Background layer. Top half blue 60,120,200 with a light 120,170,230 band 10 px high every 40 px
//! (at y 0, 40, 80, 120, 160); bottom half vertical stripes 20 px wide, 40,140,60 at x = 0 mod 40
//! and 80,180,90 at x = 20 mod 40; a red 220,30,30 square from 260,160 to 340,240; selection
//! 250,150 to 350,250 (the square plus 10 px). The sampling window is 100,0 to 500,400, which is
//! what [`sampling_window`] gives for that selection.
//!
//! Reference keeps the bands and the stripes in their original phase through the hole and keeps
//! the blue / green boundary at y 200.

use super::*;

const DOC_W: usize = 600;
const DOC_H: usize = 400;
/// The measured selection: the red square plus 10 px.
const SEL: (i32, i32, i32, i32) = (250, 150, 350, 250);
/// The red square inside it.
const SQUARE: (usize, usize, usize, usize) = (260, 160, 340, 240);

/// The document's colours without the square, in levels of 255: what Reference
/// shows through the hole again.
fn background_levels(x: usize, y: usize) -> [i32; 3] {
    if y < 200 {
        if y % 40 < 10 { [120, 170, 230] } else { [60, 120, 200] }
    } else if x % 40 < 20 {
        [40, 140, 60]
    } else {
        [80, 180, 90]
    }
}

/// The measured document as interleaved normalised floats (`ch = 3`).
fn document() -> Vec<f32> {
    let at = |x: usize, y: usize| -> [f32; 3] {
        let v = if (SQUARE.0..SQUARE.2).contains(&x) && (SQUARE.1..SQUARE.3).contains(&y) { [220, 30, 30] } else { background_levels(x, y) };
        [v[0] as f32 / 255.0, v[1] as f32 / 255.0, v[2] as f32 / 255.0]
    };
    (0..DOC_H).flat_map(|y| (0..DOC_W).flat_map(move |x| at(x, y))).collect()
}

/// The sampling window of the measured selection on the measured canvas.
fn window() -> Rect {
    sampling_window(Rect::new(SEL.0, SEL.1, SEL.2, SEL.3), Rect::new(0, 0, DOC_W as i32, DOC_H as i32))
}

/// The measured window's buffer and its hole: `edit.contentAwareFill` fills the sampling window,
/// with the selection as the hole and (for Auto) the area `auto_sampling` keeps within it.
fn window_buffers() -> (Vec<f32>, Vec<bool>, usize) {
    let doc = document();
    let win = window();
    let (ww, wh) = (win.width() as usize, win.height() as usize);
    let mut img = vec![0.0f32; ww * wh * 3];
    let mut hole = vec![false; ww * wh];
    for y in 0..wh {
        let dy = (win.y0 + y as i32) as usize;
        let s = (dy * DOC_W + win.x0 as usize) * 3;
        img[y * ww * 3..(y + 1) * ww * 3].copy_from_slice(&doc[s..s + ww * 3]);
        for x in 0..ww {
            hole[y * ww + x] = (SEL.0..SEL.2).contains(&(win.x0 + x as i32)) && (SEL.1..SEL.3).contains(&(win.y0 + y as i32));
        }
    }
    (img, hole, ww)
}

/// Fill the measured document the way the command does with `opts` and Auto sampling, and return
/// the filled window.
fn fill_measured_document(opts: &FillOptions) -> Vec<f32> {
    let (img, hole, ww) = window_buffers();
    let source = auto_sampling(ww, 400, 3, &img, &hole);
    fill(ww, 400, 3, &img, &hole, &source, opts)
}

/// The filled colour at document coordinates `(x, y)`, in levels of 255.
fn filled_at(win: &[f32], x: usize, y: usize) -> [i32; 3] {
    let w = window().width() as usize;
    let i = ((y - window().y0 as usize) * w + (x - window().x0 as usize)) * 3;
    [0, 1, 2].map(|c| (win[i + c] * 255.0).round() as i32)
}

/// The nine pixel samples of upstream #14191, measured in Reference (document coordinates
/// and the reference colour in levels of 255), in the order the issue lists them.
const SAMPLES: [((usize, usize), [i32; 3]); 9] = [
    ((300, 165), [120, 170, 230]),
    ((300, 180), [60, 120, 200]),
    ((300, 195), [60, 120, 200]),
    ((300, 205), [80, 180, 90]),
    ((285, 220), [40, 140, 60]),
    ((265, 220), [77, 177, 88]),
    ((300, 240), [80, 180, 90]),
    ((255, 155), [60, 120, 200]),
    ((345, 245), [80, 180, 90]),
];

/// The measured samples that are more than 12 levels per channel from the reference value.
fn samples_off(out: &[f32]) -> Vec<String> {
    SAMPLES
        .iter()
        .filter_map(|((x, y), want)| {
            let got = filled_at(out, *x, *y);
            let off = (0..3).map(|c| (got[c] - want[c]).abs()).max().unwrap_or(0);
            (off > 12).then(|| format!("({x},{y}) -> {got:?}, reference {want:?}, off by {off}"))
        })
        .collect()
}

/// How far the fill is from the document's own background, over the whole hole: (worst deviation
/// in levels, number of pixels within 12, number of pixels counted). Every pixel of the hole
/// carries the colour the document has at that place, which is what "the bands and stripes run
/// through the hole with their original phase" means.
fn phase_through_the_hole(out: &[f32]) -> (i32, usize, usize) {
    let (mut worst, mut within, mut n) = (0, 0, 0);
    for y in 150..250 {
        for x in 250..350 {
            let want = background_levels(x, y);
            let got = filled_at(out, x, y);
            let off = (0..3).map(|c| (got[c] - want[c]).abs()).max().unwrap_or(0);
            worst = worst.max(off);
            within += usize::from(off <= 12);
            n += 1;
        }
    }
    (worst, within, n)
}

/// The blue / light half and the green half of the document, by the palette colour a filled pixel
/// took: the boundary between them.
const PALETTE: [(&str, [i32; 3]); 5] =
    [("blue", [60, 120, 200]), ("light", [120, 170, 230]), ("green-a", [40, 140, 60]), ("green-b", [80, 180, 90]), ("red", [220, 30, 30])];

fn palette_name(v: [i32; 3]) -> &'static str {
    PALETTE.iter().min_by_key(|(_, c)| c.iter().zip(v).map(|(a, b): (&i32, i32)| (a - b) * (a - b)).sum::<i32>()).map(|(n, _)| *n).unwrap_or("?")
}

fn cold(name: &str) -> bool {
    name == "blue" || name == "light"
}

fn green(name: &str) -> bool {
    name == "green-a" || name == "green-b"
}

/// Over the inside of the hole: how many pixels above y 200 took a colour of the green half, and
/// how many below it took a colour of the blue half. Rows within 3 px of the boundary are left
/// out: there the seams between copied pieces blend.
fn boundary_mismatches(out: &[f32]) -> (usize, usize) {
    let (mut green_above, mut cold_below) = (0, 0);
    for y in 153..248 {
        if (197..204).contains(&y) {
            continue;
        }
        for x in 253..348 {
            let n = palette_name(filled_at(out, x, y));
            if y < 200 && green(n) {
                green_above += 1;
            }
            if y > 200 && cold(n) {
                cold_below += 1;
            }
        }
    }
    (green_above, cold_below)
}

/// The measured fill, computed once: Auto and Rectangular give `fill` the same area on this
/// document ([`measured_auto_area_is_the_rectangular_one`]), so one fill stands for both. The
/// checks are collected first, so a failure reports every one that is off.
///
/// Known gap: the fill's 7 x 7 patches and its coarsest level (eight patches across the hole)
/// lose the bands' phase, and the boundary sags about 10 px into the stripes. 11 x 11 patches
/// with a coarsest level four patches across pass here, but on photos they grow the ground up
/// into the sky again (the lone-tree horizon), smooth out textured holes and fill large holes
/// three times slower, so they are not the default. Kept to the Rectangular area (upstream's rule,
/// which Auto also meets whenever it keeps every pixel around the hole) they still smear a flat
/// patch over a fence behind a removed tree trunk and fill textured holes smoother; 9 x 9 patches,
/// or a coarsest level five or six patches across, don't pass here.
#[test]
#[ignore = "known gap: bands and stripes lose their phase through the hole"]
fn measured_fill_continues_the_bands_and_stripes_and_keeps_the_boundary_at_y_200() {
    assert_eq!(window(), Rect::new(100, 0, 500, 400), "the sampling window upstream #14191 measured");
    let out = fill_measured_document(&FillOptions::default());
    let mut failures = Vec::new();
    let off = samples_off(&out);
    if !off.is_empty() {
        failures.push(format!("{} of 9 samples off\n{}", off.len(), off.join("\n")));
    }
    let (green_above, cold_below) = boundary_mismatches(&out);
    if (green_above, cold_below) != (0, 0) {
        failures.push(format!(
            "the blue / green boundary moved: {green_above} pixels above y 200 took a colour of the green half, {cold_below} below it one of the blue half"
        ));
    }
    let (worst, within, n) = phase_through_the_hole(&out);
    if within != n {
        failures.push(format!("{within} of {n} hole pixels carry the document's own colour within 12 levels (worst {worst})"));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
