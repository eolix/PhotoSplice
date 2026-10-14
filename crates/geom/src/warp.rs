//! Warp geometry shared by Edit › Transform › Warp, smart-object warps and Warp Text.
//!
//! Two kinds of warp, both forward maps from a source box to document space:
//! - **Styles** ([`WarpStyle`], Reference's fifteen presets plus bend and horizontal/vertical
//!   distortion, the PSD `warp` descriptor's `warpStyle`/`warpValue`/`warpPerspective*`). Each is
//!   the single Bézier patch Reference builds for it, as recorded from the warped placed layers it
//!   saves ([`StyleWarp`]). Every style is the identity at bend 0 and continuous in the bend.
//! - **Custom meshes** ([`BezierMesh`]): a grid of bicubic Bezier patches. The default is one
//!   patch (a 4×4 control grid); split warps add patch rows/columns by exact de Casteljau
//!   subdivision, so splitting never changes the shape.
//!
//! Pure geometry: resampling lives in `photosuite-algo::warp`.

use serde::{Deserialize, Serialize};

/// Warp style: none, a custom mesh, or one of Reference's presets (menu order).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WarpStyle {
    #[default]
    None,
    Custom,
    Arc,
    ArcLower,
    ArcUpper,
    Arch,
    Bulge,
    ShellLower,
    ShellUpper,
    Flag,
    Wave,
    Fish,
    Rise,
    Fisheye,
    Inflate,
    Squeeze,
    Twist,
}

impl WarpStyle {
    /// The fifteen presets in Reference's menu order.
    pub const PRESETS: [WarpStyle; 15] = [
        WarpStyle::Arc,
        WarpStyle::ArcLower,
        WarpStyle::ArcUpper,
        WarpStyle::Arch,
        WarpStyle::Bulge,
        WarpStyle::ShellLower,
        WarpStyle::ShellUpper,
        WarpStyle::Flag,
        WarpStyle::Wave,
        WarpStyle::Fish,
        WarpStyle::Rise,
        WarpStyle::Fisheye,
        WarpStyle::Inflate,
        WarpStyle::Squeeze,
        WarpStyle::Twist,
    ];

    /// Every style (none, custom, presets).
    pub fn all() -> impl Iterator<Item = WarpStyle> {
        [WarpStyle::None, WarpStyle::Custom].into_iter().chain(Self::PRESETS)
    }

    /// PSD `warpStyle` enum value (`warpArc`, `warpCustom`, `warpNone`…).
    pub fn psd_name(self) -> &'static str {
        match self {
            WarpStyle::None => "warpNone",
            WarpStyle::Custom => "warpCustom",
            WarpStyle::Arc => "warpArc",
            WarpStyle::ArcLower => "warpArcLower",
            WarpStyle::ArcUpper => "warpArcUpper",
            WarpStyle::Arch => "warpArch",
            WarpStyle::Bulge => "warpBulge",
            WarpStyle::ShellLower => "warpShellLower",
            WarpStyle::ShellUpper => "warpShellUpper",
            WarpStyle::Flag => "warpFlag",
            WarpStyle::Wave => "warpWave",
            WarpStyle::Fish => "warpFish",
            WarpStyle::Rise => "warpRise",
            WarpStyle::Fisheye => "warpFisheye",
            WarpStyle::Inflate => "warpInflate",
            WarpStyle::Squeeze => "warpSqueeze",
            WarpStyle::Twist => "warpTwist",
        }
    }

    /// Short id used by commands (`arc`, `arcLower`, `custom`, `none`).
    pub fn id(self) -> &'static str {
        let p = self.psd_name().trim_start_matches("warp");
        // Lower-case first letter without allocating: every id is listed.
        match p {
            "None" => "none",
            "Custom" => "custom",
            "Arc" => "arc",
            "ArcLower" => "arcLower",
            "ArcUpper" => "arcUpper",
            "Arch" => "arch",
            "Bulge" => "bulge",
            "ShellLower" => "shellLower",
            "ShellUpper" => "shellUpper",
            "Flag" => "flag",
            "Wave" => "wave",
            "Fish" => "fish",
            "Rise" => "rise",
            "Fisheye" => "fisheye",
            "Inflate" => "inflate",
            "Squeeze" => "squeeze",
            _ => "twist",
        }
    }

    /// Menu label (`Arc Lower`).
    pub fn label(self) -> &'static str {
        match self {
            WarpStyle::None => "None",
            WarpStyle::Custom => "Custom",
            WarpStyle::Arc => "Arc",
            WarpStyle::ArcLower => "Arc Lower",
            WarpStyle::ArcUpper => "Arc Upper",
            WarpStyle::Arch => "Arch",
            WarpStyle::Bulge => "Bulge",
            WarpStyle::ShellLower => "Shell Lower",
            WarpStyle::ShellUpper => "Shell Upper",
            WarpStyle::Flag => "Flag",
            WarpStyle::Wave => "Wave",
            WarpStyle::Fish => "Fish",
            WarpStyle::Rise => "Rise",
            WarpStyle::Fisheye => "Fisheye",
            WarpStyle::Inflate => "Inflate",
            WarpStyle::Squeeze => "Squeeze",
            WarpStyle::Twist => "Twist",
        }
    }

    /// Parses a short id, a PSD name or a label (case-insensitive, spaces ignored).
    pub fn parse(s: &str) -> Option<WarpStyle> {
        let k: String = s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
        Self::all().find(|w| w.id().to_ascii_lowercase() == k || w.psd_name().to_ascii_lowercase() == k)
    }

    pub fn is_preset(self) -> bool {
        !matches!(self, WarpStyle::None | WarpStyle::Custom)
    }
}

/// A preset style prepared for one box: the single Bézier patch Reference builds for it, raised
/// to the bicubic 4 × 4 grid (box coordinates).
///
/// Reference stores a warped placed layer's preset as that patch (`warpCustom` with `uOrder` /
/// `vOrder`), which gives every style's construction (fitted on 255 warped references made in
/// Reference: 15 styles, bends ±, both orientations, square and wide boxes, each distortion):
/// - Arc and Shells: concentric circular arcs (circular in pixels), half-angle `bend × 90°`, the
///   inner one through the box corners, each drawn as one cubic (`k = 4/3 tan(θ/2)`).
/// - Arc Lower / Upper, Arch, Bulge: arcs of that angle on the bottom / top edge chords.
/// - Flag, Fish, Rise, Wave: inner control points moved by `2 × bend` of the box height.
/// - Inflate, Squeeze: a 3 × 3 patch whose edge midpoints move by `bend / 2`.
/// - Fisheye: inner points towards the corners by `2 × bend`; Twist: inner points along the
///   edges by `bend` (the same for both orientations).
///
/// The bend shapes match Reference's control points to within 1e-4 of the box. Distortion scales
/// each column's offsets (about the average of its ends) by its height factor
/// `1 + h(2u − 1)`, and each row's (about the average of its ends) by its width factor
/// `1 − v(1 − 2y)`, around the distorted grid: exact with one slider, within a few pixels with
/// both at once.
#[derive(Clone, Copy, Debug)]
pub struct StyleWarp {
    /// Bicubic control points, row-major (`u` across), absolute coordinates.
    points: [[f64; 2]; 16],
    origin: [f64; 2],
    size: [f64; 2],
}

impl StyleWarp {
    /// Prepares `style` with bend / distortions in percent (-100..100) for `[x0, y0, x1, y1]`.
    /// `None` for non-presets, an all-zero warp or an empty box.
    pub fn new(style: WarpStyle, bend: f64, h_distort: f64, v_distort: f64, vertical: bool, bounds: [f64; 4]) -> Option<StyleWarp> {
        if !style.is_preset() {
            return None;
        }
        let [x0, y0, x1, y1] = bounds;
        let (w, h) = (x1 - x0, y1 - y0);
        if !(w > 0.0 && h > 0.0 && w.is_finite() && h.is_finite()) {
            return None;
        }
        let pct = |v: f64| if v.is_finite() { (v / 100.0).clamp(-1.0, 1.0) } else { 0.0 };
        let (b, hd, vd) = (pct(bend), pct(h_distort), pct(v_distort));
        if b == 0.0 && hd == 0.0 && vd == 0.0 {
            return None;
        }
        let (nu, nv, pts) = preset_patch(style, b, vertical, w, h);
        // Distortion works on the box-normalised patch.
        let norm: Vec<[f64; 2]> = pts.iter().map(|p| [p[0] / w, p[1] / h]).collect();
        let distorted = distort(&norm, nu, nv, hd, vd);
        let raised: Vec<[f64; 2]> = distorted.iter().map(|p| [x0 + p[0] * w, y0 + p[1] * h]).collect();
        let mesh = BezierMesh::from_patch(nu, nv, &raised)?;
        let points: [[f64; 2]; 16] = mesh.points.try_into().ok()?;
        Some(StyleWarp { points, origin: [x0, y0], size: [w, h] })
    }

    /// Longest segment (px) worth sending through [`StyleWarp::apply`] unsplit.
    pub fn max_segment(&self) -> f64 {
        (self.size[0].min(self.size[1]) / 32.0).max(0.5)
    }

    /// Maps a point.
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let (s, t) = ((x - self.origin[0]) / self.size[0], (y - self.origin[1]) / self.size[1]);
        let (bs, bt) = (bernstein(s), bernstein(t));
        let mut out = [0.0; 2];
        for (j, wt) in bt.iter().enumerate() {
            for (i, ws) in bs.iter().enumerate() {
                let p = self.points[j * 4 + i];
                out[0] += ws * wt * p[0];
                out[1] += ws * wt * p[1];
            }
        }
        (out[0], out[1])
    }

    /// The bicubic control points (row-major, `u` across).
    pub fn control_points(&self) -> &[[f64; 2]; 16] {
        &self.points
    }
}

/// The patch of `style` (bend `b`, -1..1) over a `w × h` box at the origin: (`u` order, `v`
/// order, control points row-major).
fn preset_patch(style: WarpStyle, b: f64, vertical: bool, w: f64, h: f64) -> (usize, usize, Vec<[f64; 2]>) {
    // Twist is built the same way in both orientations.
    if !vertical || style == WarpStyle::Twist {
        return horizontal_patch(style, b, w, h);
    }
    // Vertical: the horizontal patch of the transposed box, axes swapped back.
    let (nu, nv, p) = horizontal_patch(style, b, h, w);
    let mut q = Vec::with_capacity(p.len());
    for i in 0..nu {
        for j in 0..nv {
            let a = p.get(j * nu + i).copied().unwrap_or([0.0; 2]);
            q.push([a[1], a[0]]);
        }
    }
    (nv, nu, q)
}

/// An identity grid of `nu × nv` control points over `w × h`.
fn identity_grid(nu: usize, nv: usize, w: f64, h: f64) -> Vec<[f64; 2]> {
    (0..nv).flat_map(|j| (0..nu).map(move |i| [w * i as f64 / (nu - 1) as f64, h * j as f64 / (nv - 1) as f64])).collect()
}

/// The Arc style's top or bottom row: concentric circular arcs of half-angle `|b| × 90°`, the
/// inner one through the box corners (the bottom one for `b > 0`, centre below; the top one for
/// `b < 0`, centre above), the outer one `h` further out.
fn arc_row(w: f64, h: f64, b: f64, top: bool) -> [[f64; 2]; 4] {
    if b == 0.0 {
        let y = if top { 0.0 } else { h };
        return [[0.0, y], [w / 3.0, y], [2.0 * w / 3.0, y], [w, y]];
    }
    let th = b.abs() * std::f64::consts::FRAC_PI_2;
    let (sin, cos) = th.sin_cos();
    let r_in = (w / 2.0) / sin;
    let k = 4.0 / 3.0 * (th / 2.0).tan();
    // `up`: from the centre towards the box (-1: the centre is below).
    let (c, up, inner_top) = if b > 0.0 { ([w / 2.0, h + r_in * cos], -1.0, false) } else { ([w / 2.0, -r_in * cos], 1.0, true) };
    let r = if top == inner_top { r_in } else { r_in + h };
    let a0 = [c[0] - r * sin, c[1] + up * r * cos];
    let a3 = [c[0] + r * sin, c[1] + up * r * cos];
    // Unit tangents, heading towards the other end.
    let (n0, n3) = if up < 0.0 { ([cos, -sin], [-cos, -sin]) } else { ([cos, sin], [-cos, sin]) };
    [a0, [a0[0] + k * r * n0[0], a0[1] + k * r * n0[1]], [a3[0] + k * r * n3[0], a3[1] + k * r * n3[1]], a3]
}

/// An arc of half-angle `|b| × 90°` on the chord `(0, y)–(w, y)`, bulging towards `side` (-1 up,
/// 1 down) for a positive bend.
fn chord_arc(w: f64, y: f64, b: f64, side: f64) -> [[f64; 2]; 4] {
    if b == 0.0 {
        return [[0.0, y], [w / 3.0, y], [2.0 * w / 3.0, y], [w, y]];
    }
    let th = b.abs() * std::f64::consts::FRAC_PI_2;
    let (sin, cos) = th.sin_cos();
    let l = 4.0 / 3.0 * (th / 2.0).tan() * (w / 2.0) / sin;
    let dy = side * b.signum() * l * sin;
    [[0.0, y], [l * cos, y + dy], [w - l * cos, y + dy], [w, y]]
}

fn horizontal_patch(style: WarpStyle, b: f64, w: f64, h: f64) -> (usize, usize, Vec<[f64; 2]>) {
    let row = |r: [[f64; 2]; 4], dy: f64| r.map(|p| [p[0], p[1] + dy]);
    match style {
        WarpStyle::Arc => (4, 2, [arc_row(w, h, b, true), arc_row(w, h, b, false)].concat()),
        WarpStyle::ArcLower => (4, 2, [chord_arc(w, 0.0, 0.0, 1.0), chord_arc(w, h, b, 1.0)].concat()),
        WarpStyle::ArcUpper => (4, 2, [chord_arc(w, 0.0, b, -1.0), chord_arc(w, h, 0.0, 1.0)].concat()),
        WarpStyle::Arch => {
            let top = chord_arc(w, 0.0, b, -1.0);
            (4, 2, [top, row(top, h)].concat())
        }
        WarpStyle::Bulge => (4, 2, [chord_arc(w, 0.0, b, -1.0), chord_arc(w, h, b, 1.0)].concat()),
        WarpStyle::ShellUpper | WarpStyle::ShellLower => {
            let mut g = identity_grid(4, 4, w, h);
            let (top, bottom) = (arc_row(w, h, b, true), arc_row(w, h, b, false));
            for (i, p) in top.iter().enumerate() {
                g[i] = *p;
            }
            g[4] = lerp(top[0], bottom[0], 1.0 / 3.0);
            g[7] = lerp(top[3], bottom[3], 1.0 / 3.0);
            if style == WarpStyle::ShellLower {
                // The upper shell mirrored top to bottom.
                g = (0..4).rev().flat_map(|j| (0..4).map(move |i| (i, j))).map(|(i, j)| g[j * 4 + i]).map(|p| [p[0], h - p[1]]).collect();
            }
            (4, 4, g)
        }
        WarpStyle::Flag | WarpStyle::Fish | WarpStyle::Rise => {
            let d = 2.0 * b * h;
            let top = if style == WarpStyle::Rise {
                [[0.0, d], [w / 3.0, d], [2.0 * w / 3.0, 0.0], [w, 0.0]]
            } else {
                [[0.0, 0.0], [w / 3.0, -d], [2.0 * w / 3.0, d], [w, 0.0]]
            };
            let bottom = match style {
                WarpStyle::Fish => [[0.0, h], [w / 3.0, h + d], [2.0 * w / 3.0, h - d], [w, h]],
                _ => row(top, h),
            };
            (4, 2, [top, bottom].concat())
        }
        WarpStyle::Wave => {
            let mut g = identity_grid(4, 3, w, h);
            let d = 2.0 * b * h;
            g[5] = [w / 3.0, h / 2.0 + d];
            g[6] = [2.0 * w / 3.0, h / 2.0 - d];
            (4, 3, g)
        }
        WarpStyle::Inflate | WarpStyle::Squeeze => {
            let mut g = identity_grid(3, 3, w, h);
            let side = if style == WarpStyle::Inflate { 1.0 } else { -1.0 };
            g[1] = [w / 2.0, -b * h / 2.0];
            g[7] = [w / 2.0, h + b * h / 2.0];
            g[3] = [-side * b * w / 2.0, h / 2.0];
            g[5] = [w + side * b * w / 2.0, h / 2.0];
            (3, 3, g)
        }
        WarpStyle::Fisheye => {
            let mut g = identity_grid(4, 4, w, h);
            for (k, corner) in [(5, [0.0, 0.0]), (6, [w, 0.0]), (9, [0.0, h]), (10, [w, h])] {
                g[k] = lerp(g[k], corner, 2.0 * b);
            }
            (4, 4, g)
        }
        WarpStyle::Twist => {
            let mut g = identity_grid(4, 4, w, h);
            let a = b.abs();
            let moves = if b > 0.0 {
                [(5, [a * w, 0.0]), (6, [0.0, a * h]), (10, [-a * w, 0.0]), (9, [0.0, -a * h])]
            } else {
                [(5, [0.0, a * h]), (6, [-a * w, 0.0]), (10, [0.0, -a * h]), (9, [a * w, 0.0])]
            };
            for (k, d) in moves {
                g[k] = [g[k][0] + d[0], g[k][1] + d[1]];
            }
            (4, 4, g)
        }
        WarpStyle::None | WarpStyle::Custom => (2, 2, identity_grid(2, 2, w, h)),
    }
}

/// Applies the distortions to a box-normalised patch (`nu × nv` control points, row-major): each
/// column's offsets from the identity grid scale by `1 + h(2u − 1)` about the average of its
/// first and last, then each row's by `1 − v(1 − 2y)` about the average of its ends, added to
/// the grid distorted the same way.
fn distort(p: &[[f64; 2]], nu: usize, nv: usize, hd: f64, vd: f64) -> Vec<[f64; 2]> {
    if hd == 0.0 && vd == 0.0 {
        return p.to_vec();
    }
    let grid = identity_grid(nu, nv, 1.0, 1.0);
    let at = |v: &Vec<[f64; 2]>, k: usize| v.get(k).copied().unwrap_or([0.0; 2]);
    let mut d: Vec<[f64; 2]> = p.iter().zip(&grid).map(|(a, g)| [a[0] - g[0], a[1] - g[1]]).collect();
    // Columns (horizontal distortion).
    let mut dc = d.clone();
    for i in 0..nu {
        let (top, bottom) = (at(&d, i), at(&d, (nv - 1) * nu + i));
        let a = [(top[0] + bottom[0]) / 2.0, (top[1] + bottom[1]) / 2.0];
        let s = 1.0 + hd * (2.0 * (i as f64 / (nu - 1) as f64) - 1.0);
        for j in 0..nv {
            let k = j * nu + i;
            let o = at(&d, k);
            if let Some(slot) = dc.get_mut(k) {
                *slot = [a[0] + (o[0] - a[0]) * s, a[1] + (o[1] - a[1]) * s];
            }
        }
    }
    d = dc;
    // The distorted grid and its width factors.
    let mut base = Vec::with_capacity(grid.len());
    let mut sx = Vec::with_capacity(grid.len());
    for g in &grid {
        let y = 0.5 + (g[1] - 0.5) * (1.0 + hd * (2.0 * g[0] - 1.0));
        let s = 1.0 - vd * (1.0 - 2.0 * y);
        base.push([0.5 + (g[0] - 0.5) * s, y]);
        sx.push(s);
    }
    // Rows (vertical distortion).
    let mut out = Vec::with_capacity(grid.len());
    for j in 0..nv {
        let (first, last) = (at(&d, j * nu), at(&d, j * nu + nu - 1));
        let a = [(first[0] + last[0]) / 2.0, (first[1] + last[1]) / 2.0];
        for i in 0..nu {
            let k = j * nu + i;
            let (o, s, g) = (at(&d, k), sx.get(k).copied().unwrap_or(1.0), at(&base, k));
            out.push([g[0] + a[0] + (o[0] - a[0]) * s, g[1] + a[1] + (o[1] - a[1]) * s]);
        }
    }
    out
}

fn bernstein(t: f64) -> [f64; 4] {
    let s = 1.0 - t;
    [s * s * s, 3.0 * s * s * t, 3.0 * s * t * t, t * t * t]
}

fn lerp(a: [f64; 2], b: [f64; 2], t: f64) -> [f64; 2] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

/// De Casteljau split of a cubic at `t`: 7 points (left 0..=3, right 3..=6).
fn split_cubic(p: [[f64; 2]; 4], t: f64) -> [[f64; 2]; 7] {
    let a = lerp(p[0], p[1], t);
    let b = lerp(p[1], p[2], t);
    let c = lerp(p[2], p[3], t);
    let d = lerp(a, b, t);
    let e = lerp(b, c, t);
    let f = lerp(d, e, t);
    [p[0], a, d, f, e, c, p[3]]
}

/// Inverse of the Bernstein interpolation matrix at t = 0, 1/3, 2/3, 1 (rows: control point,
/// columns: sample). Exact rational values.
const BERN_INV: [[f64; 4]; 4] = [[1.0, 0.0, 0.0, 0.0], [-5.0 / 6.0, 3.0, -1.5, 1.0 / 3.0], [1.0 / 3.0, -1.5, 3.0, -5.0 / 6.0], [0.0, 0.0, 0.0, 1.0]];

/// The cubic Bézier equal to the linear, quadratic or cubic one with control points `p`.
fn elevate_to_cubic(p: &[[f64; 2]]) -> Option<[[f64; 2]; 4]> {
    let lerp = |a: [f64; 2], b: [f64; 2], t: f64| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
    match *p {
        [a, b] => Some([a, lerp(a, b, 1.0 / 3.0), lerp(a, b, 2.0 / 3.0), b]),
        [a, b, c] => Some([a, lerp(a, b, 2.0 / 3.0), lerp(c, b, 2.0 / 3.0), c]),
        [a, b, c, d] => Some([a, b, c, d]),
        _ => None,
    }
}

/// A grid of bicubic Bezier patches over the unit square of a source box.
///
/// `us`/`vs` are the patch boundaries in the box's normalised coordinates (`0..=1`, increasing,
/// first 0 and last 1). Control points are a `(3·(us.len()-1)+1) × (3·(vs.len()-1)+1)` grid in
/// row-major order, in output (document) coordinates; neighbouring patches share their edge row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BezierMesh {
    pub us: Vec<f64>,
    pub vs: Vec<f64>,
    pub points: Vec<[f64; 2]>,
}

impl BezierMesh {
    /// The identity mesh over `bounds` with `cols × rows` patches (Photoshop's default is 1 × 1,
    /// a 4 × 4 control grid).
    pub fn identity(bounds: [f64; 4], cols: usize, rows: usize) -> BezierMesh {
        let (cols, rows) = (cols.clamp(1, 64), rows.clamp(1, 64));
        let us: Vec<f64> = (0..=cols).map(|i| i as f64 / cols as f64).collect();
        let vs: Vec<f64> = (0..=rows).map(|i| i as f64 / rows as f64).collect();
        Self::fit(&|s, t| [bounds[0] + s * (bounds[2] - bounds[0]), bounds[1] + t * (bounds[3] - bounds[1])], us, vs)
    }

    /// A one-patch mesh from a Bézier patch of `u_order × v_order` control points (2 to 4 each:
    /// linear, quadratic or cubic; row-major, `u` across), raised to the bicubic 4 × 4 grid. Degree
    /// elevation is exact, so the surface is unchanged. Reference stores most preset warps of a
    /// placed layer this way (Arc 4 × 2, Inflate 3 × 3, Wave 4 × 3). `None` for other orders or a
    /// point count that doesn't match.
    pub fn from_patch(u_order: usize, v_order: usize, points: &[[f64; 2]]) -> Option<BezierMesh> {
        if !(2..=4).contains(&u_order) || !(2..=4).contains(&v_order) || points.len() != u_order.checked_mul(v_order)? {
            return None;
        }
        // Rows across, then columns down.
        let rows: Vec<[[f64; 2]; 4]> = points.chunks_exact(u_order).map(elevate_to_cubic).collect::<Option<_>>()?;
        let mut out = vec![[0.0; 2]; 16];
        for i in 0..4 {
            let column: Vec<[f64; 2]> = rows.iter().map(|r| r[i]).collect();
            for (j, p) in elevate_to_cubic(&column)?.into_iter().enumerate() {
                out[j * 4 + i] = p;
            }
        }
        let mesh = BezierMesh { us: vec![0.0, 1.0], vs: vec![0.0, 1.0], points: out };
        mesh.is_valid().then_some(mesh)
    }

    /// Fits a mesh with patch boundaries `us`/`vs` to the map `f` (normalised → output) by
    /// interpolating it at the patch's thirds. Exact for bicubic maps (affine maps included).
    pub fn fit(f: &dyn Fn(f64, f64) -> [f64; 2], us: Vec<f64>, vs: Vec<f64>) -> BezierMesh {
        let (pc, pr) = (us.len() - 1, vs.len() - 1);
        let (nx, ny) = (3 * pc + 1, 3 * pr + 1);
        let mut points = vec![[0.0; 2]; nx * ny];
        for pj in 0..pr {
            for pi in 0..pc {
                let mut s = [[[0.0; 2]; 4]; 4]; // s[row j][col i]
                for (j, row) in s.iter_mut().enumerate() {
                    for (i, v) in row.iter_mut().enumerate() {
                        let u = us[pi] + (us[pi + 1] - us[pi]) * i as f64 / 3.0;
                        let t = vs[pj] + (vs[pj + 1] - vs[pj]) * j as f64 / 3.0;
                        *v = f(u, t);
                    }
                }
                for j in 0..4 {
                    for i in 0..4 {
                        let mut acc = [0.0; 2];
                        for (a, wa) in BERN_INV[j].iter().enumerate() {
                            for (b, wb) in BERN_INV[i].iter().enumerate() {
                                let k = wa * wb;
                                acc[0] += k * s[a][b][0];
                                acc[1] += k * s[a][b][1];
                            }
                        }
                        points[(3 * pj + j) * nx + 3 * pi + i] = acc;
                    }
                }
            }
        }
        BezierMesh { us, vs, points }
    }

    /// Control points per row.
    pub fn nx(&self) -> usize {
        3 * (self.us.len() - 1) + 1
    }
    /// Control point rows.
    pub fn ny(&self) -> usize {
        3 * (self.vs.len() - 1) + 1
    }
    pub fn point(&self, i: usize, j: usize) -> [f64; 2] {
        self.points[j * self.nx() + i]
    }

    /// Checks the invariants (knots and point count); malformed data from files is rejected.
    pub fn is_valid(&self) -> bool {
        let knots_ok = |k: &[f64]| k.len() >= 2 && k[0] == 0.0 && k[k.len() - 1] == 1.0 && k.windows(2).all(|w| w[1] > w[0]);
        knots_ok(&self.us)
            && knots_ok(&self.vs)
            && self.points.len() == self.nx() * self.ny()
            && self.points.iter().all(|p| p[0].is_finite() && p[1].is_finite())
    }

    fn locate(knots: &[f64], s: f64) -> (usize, f64) {
        let n = knots.len() - 1;
        let s = s.clamp(0.0, 1.0);
        let mut i = 0;
        while i + 1 < n && s > knots[i + 1] {
            i += 1;
        }
        let w = knots[i + 1] - knots[i];
        (i, if w > 0.0 { (s - knots[i]) / w } else { 0.0 })
    }

    /// Evaluates the surface at normalised `(s, t)`.
    pub fn eval(&self, s: f64, t: f64) -> [f64; 2] {
        let (pi, a) = Self::locate(&self.us, s);
        let (pj, b) = Self::locate(&self.vs, t);
        let (ba, bb) = (bernstein(a), bernstein(b));
        let nx = self.nx();
        let mut out = [0.0; 2];
        for (j, wb) in bb.iter().enumerate() {
            for (i, wa) in ba.iter().enumerate() {
                let p = self.points[(3 * pj + j) * nx + 3 * pi + i];
                let k = wa * wb;
                out[0] += p[0] * k;
                out[1] += p[1] * k;
            }
        }
        out
    }

    /// Splits the patch column containing `s` at `s` (a vertical split line). Exact: the surface
    /// is unchanged. Returns false when `s` is on or too close to an existing boundary.
    pub fn split_u(&mut self, s: f64) -> bool {
        let (pi, a) = Self::locate(&self.us, s);
        if !(0.01..=0.99).contains(&a) {
            return false;
        }
        let (nx, ny) = (self.nx(), self.ny());
        let mut pts = Vec::with_capacity((nx + 3) * ny);
        for j in 0..ny {
            let row = &self.points[j * nx..(j + 1) * nx];
            pts.extend_from_slice(&row[..3 * pi]);
            let seg = [row[3 * pi], row[3 * pi + 1], row[3 * pi + 2], row[3 * pi + 3]];
            pts.extend_from_slice(&split_cubic(seg, a));
            pts.extend_from_slice(&row[3 * pi + 4..]);
        }
        self.points = pts;
        self.us.insert(pi + 1, s.clamp(0.0, 1.0));
        true
    }

    /// Splits the patch row containing `t` at `t` (a horizontal split line). Exact.
    pub fn split_v(&mut self, t: f64) -> bool {
        self.transpose();
        let ok = self.split_u(t);
        self.transpose();
        ok
    }

    /// Removes interior vertical boundary `k` (1..us.len()-1), merging its two patch columns.
    /// Exact when the patches came from an unedited split; otherwise the outer control points are
    /// kept and the inner handles extrapolated.
    pub fn remove_split_u(&mut self, k: usize) -> bool {
        if k == 0 || k + 1 >= self.us.len() {
            return false;
        }
        let a = (self.us[k] - self.us[k - 1]) / (self.us[k + 1] - self.us[k - 1]);
        let (nx, ny) = (self.nx(), self.ny());
        let mut pts = Vec::with_capacity((nx - 3) * ny);
        let c0 = 3 * (k - 1);
        for j in 0..ny {
            let row = &self.points[j * nx..(j + 1) * nx];
            let (p0, q1, r2, p3) = (row[c0], row[c0 + 1], row[c0 + 5], row[c0 + 6]);
            // De Casteljau inverse: q1 = lerp(p0, p1, a), r2 = lerp(p2, p3, a).
            let p1 = [(q1[0] - (1.0 - a) * p0[0]) / a, (q1[1] - (1.0 - a) * p0[1]) / a];
            let p2 = [(r2[0] - a * p3[0]) / (1.0 - a), (r2[1] - a * p3[1]) / (1.0 - a)];
            pts.extend_from_slice(&row[..c0]);
            pts.extend_from_slice(&[p0, p1, p2, p3]);
            pts.extend_from_slice(&row[c0 + 7..]);
        }
        self.points = pts;
        self.us.remove(k);
        true
    }

    /// Removes interior horizontal boundary `k`.
    pub fn remove_split_v(&mut self, k: usize) -> bool {
        self.transpose();
        let ok = self.remove_split_u(k);
        self.transpose();
        ok
    }

    fn transpose(&mut self) {
        let (nx, ny) = (self.nx(), self.ny());
        let mut pts = vec![[0.0; 2]; nx * ny];
        for j in 0..ny {
            for i in 0..nx {
                pts[i * ny + j] = self.points[j * nx + i];
            }
        }
        self.points = pts;
        std::mem::swap(&mut self.us, &mut self.vs);
    }

    /// Applies `f` to every control point (exact for affine maps).
    pub fn map_points(&self, f: impl Fn([f64; 2]) -> [f64; 2]) -> BezierMesh {
        BezierMesh { us: self.us.clone(), vs: self.vs.clone(), points: self.points.iter().map(|p| f(*p)).collect() }
    }

    /// Normalised `(s, t)` whose surface point is nearest `p` (coarse search, then Newton
    /// steps on the patch). Used to place split lines where the user clicks.
    pub fn param_at(&self, p: [f64; 2]) -> (f64, f64) {
        let n = 48;
        let mut best = (0.5, 0.5, f64::MAX);
        for j in 0..=n {
            for i in 0..=n {
                let (s, t) = (i as f64 / n as f64, j as f64 / n as f64);
                let q = self.eval(s, t);
                let d = (q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2);
                if d < best.2 {
                    best = (s, t, d);
                }
            }
        }
        let (mut s, mut t) = (best.0, best.1);
        for _ in 0..12 {
            let h = 1e-5;
            let q = self.eval(s, t);
            let (qs, qt) = (self.eval(s + h, t), self.eval(s, t + h));
            let (a, b, c, d) = ((qs[0] - q[0]) / h, (qt[0] - q[0]) / h, (qs[1] - q[1]) / h, (qt[1] - q[1]) / h);
            let det = a * d - b * c;
            if det.abs() < 1e-12 {
                break;
            }
            let (ex, ey) = (p[0] - q[0], p[1] - q[1]);
            s = (s + (d * ex - b * ey) / det).clamp(0.0, 1.0);
            t = (t + (a * ey - c * ex) / det).clamp(0.0, 1.0);
        }
        (s, t)
    }

    /// Bounding box of the control points (the surface lies inside it).
    pub fn control_bounds(&self) -> [f64; 4] {
        self.points.iter().fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |b, p| [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])])
    }
}

/// A complete warp: what Photoshop's `warp` descriptor stores, plus the custom mesh.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Warp {
    pub style: WarpStyle,
    /// Bend in percent (-100..100), presets only.
    #[serde(default)]
    pub bend: f64,
    #[serde(default)]
    pub h_distort: f64,
    #[serde(default)]
    pub v_distort: f64,
    /// Warp along the vertical axis.
    #[serde(default)]
    pub vertical: bool,
    /// The source box `[x0, y0, x1, y1]` the warp is defined over.
    pub bounds: [f64; 4],
    /// Control mesh for [`WarpStyle::Custom`] (output coordinates).
    #[serde(default)]
    pub mesh: Option<BezierMesh>,
}

impl Warp {
    /// No warp over `bounds`.
    pub fn none(bounds: [f64; 4]) -> Warp {
        Warp { style: WarpStyle::None, bend: 0.0, h_distort: 0.0, v_distort: 0.0, vertical: false, bounds, mesh: None }
    }

    /// A preset style.
    pub fn preset(style: WarpStyle, bend: f64, bounds: [f64; 4]) -> Warp {
        Warp { style, bend, ..Warp::none(bounds) }
    }

    /// A custom warp from a mesh.
    pub fn custom(mesh: BezierMesh, bounds: [f64; 4]) -> Warp {
        Warp { style: WarpStyle::Custom, mesh: Some(mesh), ..Warp::none(bounds) }
    }

    fn style_warp(&self) -> Option<StyleWarp> {
        StyleWarp::new(self.style, self.bend, self.h_distort, self.v_distort, self.vertical, self.bounds)
    }

    /// Whether the warp maps every point of its box onto itself (within `1e-9` px).
    pub fn is_identity(&self) -> bool {
        match self.style {
            WarpStyle::None => true,
            WarpStyle::Custom => match &self.mesh {
                None => true,
                Some(m) => {
                    let id = BezierMesh::fit(&|s, t| self.box_point(s, t), m.us.clone(), m.vs.clone());
                    m.points.iter().zip(&id.points).all(|(a, b)| (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9)
                }
            },
            _ => self.style_warp().is_none(),
        }
    }

    fn box_point(&self, s: f64, t: f64) -> [f64; 2] {
        let b = self.bounds;
        [b[0] + s * (b[2] - b[0]), b[1] + t * (b[3] - b[1])]
    }

    /// Maps a source point (inside the box) to output space.
    pub fn map(&self, x: f64, y: f64) -> (f64, f64) {
        match self.style {
            WarpStyle::None => (x, y),
            WarpStyle::Custom => match &self.mesh {
                Some(m) => {
                    let b = self.bounds;
                    let (w, h) = (b[2] - b[0], b[3] - b[1]);
                    if w <= 0.0 || h <= 0.0 {
                        return (x, y);
                    }
                    let p = m.eval((x - b[0]) / w, (y - b[1]) / h);
                    (p[0], p[1])
                }
                None => (x, y),
            },
            _ => match self.style_warp() {
                Some(sw) => sw.apply(x, y),
                None => (x, y),
            },
        }
    }

    /// The warp as a control mesh (a preset is fitted with `cols × rows` patches; custom warps
    /// return their own mesh).
    pub fn to_mesh(&self, cols: usize, rows: usize) -> BezierMesh {
        if let (WarpStyle::Custom, Some(m)) = (self.style, &self.mesh) {
            return m.clone();
        }
        let (cols, rows) = (cols.clamp(1, 64), rows.clamp(1, 64));
        let us: Vec<f64> = (0..=cols).map(|i| i as f64 / cols as f64).collect();
        let vs: Vec<f64> = (0..=rows).map(|i| i as f64 / rows as f64).collect();
        BezierMesh::fit(
            &|s, t| {
                let p = self.box_point(s, t);
                let (x, y) = self.map(p[0], p[1]);
                [x, y]
            },
            us,
            vs,
        )
    }

    /// The same warp followed by the affine map `[a, b, c, d, e, f]` (`x' = a·x + c·y + e`,
    /// `y' = b·x + d·y + f`), as a custom mesh (exact for custom warps).
    pub fn then_affine(&self, m: [f64; 6]) -> Warp {
        let mesh = self.to_mesh(4, 4).map_points(|p| [m[0] * p[0] + m[2] * p[1] + m[4], m[1] * p[0] + m[3] * p[1] + m[5]]);
        Warp::custom(mesh, self.bounds)
    }

    /// Approximate output bounds (sampled; includes the control mesh for custom warps).
    pub fn output_bounds(&self) -> [f64; 4] {
        let n = 32;
        let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for j in 0..=n {
            for i in 0..=n {
                let p = self.box_point(i as f64 / n as f64, j as f64 / n as f64);
                let (x, y) = self.map(p[0], p[1]);
                b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
            }
        }
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: [f64; 4] = [10.0, 20.0, 110.0, 70.0];

    fn close(a: (f64, f64), b: (f64, f64), eps: f64) -> bool {
        (a.0 - b.0).abs() < eps && (a.1 - b.1).abs() < eps
    }

    #[test]
    fn styles_parse_and_round_trip_names() {
        for s in WarpStyle::all() {
            assert_eq!(WarpStyle::parse(s.id()), Some(s));
            assert_eq!(WarpStyle::parse(s.psd_name()), Some(s));
            assert_eq!(WarpStyle::parse(s.label()), Some(s), "{s:?}");
        }
        assert_eq!(WarpStyle::parse("spiral"), None);
    }

    /// Linear, quadratic and cubic patches raise to the same surface on a 4 × 4 grid; other
    /// orders and mismatched point counts are refused.
    #[test]
    fn patches_raise_exactly() {
        let pts: Vec<[f64; 2]> = (0..8).map(|k| [f64::from(k % 4) * 10.0 + f64::from(k / 4) * 3.0, f64::from(k / 4) * 20.0 + f64::from(k % 4 * k % 3)]).collect();
        let m = BezierMesh::from_patch(4, 2, &pts).unwrap();
        // Cubic across, linear down.
        let cubic = |s: f64| [(1.0 - s).powi(3), 3.0 * s * (1.0 - s).powi(2), 3.0 * s * s * (1.0 - s), s.powi(3)];
        for (s, t) in [(0.0, 0.0), (0.3, 0.7), (1.0, 1.0), (0.5, 0.25)] {
            let (bs, bt) = (cubic(s), [1.0 - t, t]);
            let mut want = [0.0; 2];
            for j in 0..2 {
                for i in 0..4 {
                    for c in 0..2 {
                        want[c] += bs[i] * bt[j] * pts[j * 4 + i][c];
                    }
                }
            }
            let got = m.eval(s, t);
            assert!((got[0] - want[0]).abs() < 1e-9 && (got[1] - want[1]).abs() < 1e-9, "{s},{t}");
        }
        assert!(BezierMesh::from_patch(5, 2, &[[0.0; 2]; 10]).is_none());
        assert!(BezierMesh::from_patch(4, 2, &pts[..7]).is_none());
        assert!(BezierMesh::from_patch(1, 4, &pts[..4]).is_none());
    }

    #[test]
    fn presets_at_bend_zero_are_identity() {
        for s in WarpStyle::PRESETS {
            let w = Warp::preset(s, 0.0, B);
            assert!(w.is_identity());
            assert_eq!(w.map(33.0, 44.0), (33.0, 44.0));
        }
    }

    #[test]
    fn identity_mesh_is_exact_and_fit_reproduces_affine() {
        let m = BezierMesh::identity(B, 1, 1);
        assert_eq!((m.nx(), m.ny()), (4, 4));
        let w = Warp::custom(m, B);
        assert!(w.is_identity());
        assert!(close(w.map(37.5, 61.25), (37.5, 61.25), 1e-9));
        // Affine maps fit exactly.
        let f = |s: f64, t: f64| [3.0 + 2.0 * s - t, 1.0 + 0.5 * s + 4.0 * t];
        let m = BezierMesh::fit(&f, vec![0.0, 1.0], vec![0.0, 0.4, 1.0]);
        for (s, t) in [(0.1, 0.9), (0.5, 0.2), (0.77, 0.41)] {
            let p = m.eval(s, t);
            let e = f(s, t);
            assert!((p[0] - e[0]).abs() < 1e-9 && (p[1] - e[1]).abs() < 1e-9);
        }
    }

    #[test]
    fn splits_are_exact_and_removable() {
        let mut m = BezierMesh::identity(B, 1, 1);
        // Bend it: move an inner control point.
        m.points[5] = [50.0, 10.0];
        let before: Vec<[f64; 2]> = (0..=10).flat_map(|j| (0..=10).map(move |i| (i, j))).map(|(i, j)| m.eval(i as f64 / 10.0, j as f64 / 10.0)).collect();
        let orig = m.clone();
        assert!(m.split_u(0.3));
        assert!(m.split_v(0.6));
        assert!(!m.split_u(0.3), "existing boundary");
        assert_eq!((m.nx(), m.ny()), (7, 7));
        assert!(m.is_valid());
        let after: Vec<[f64; 2]> = (0..=10).flat_map(|j| (0..=10).map(move |i| (i, j))).map(|(i, j)| m.eval(i as f64 / 10.0, j as f64 / 10.0)).collect();
        for (a, b) in before.iter().zip(&after) {
            assert!((a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9);
        }
        let (ls, lt) = m.param_at(m.eval(0.42, 0.77));
        assert!((ls - 0.42).abs() < 1e-6 && (lt - 0.77).abs() < 1e-6, "{ls},{lt}");
        assert!(m.remove_split_u(1));
        assert!(m.remove_split_v(1));
        assert_eq!(m.us, orig.us);
        for (a, b) in m.points.iter().zip(&orig.points) {
            assert!((a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9);
        }
    }

    /// Presets build the patch Reference builds: control points recorded from warped placed
    /// layers it saved (normalised to the box, 4 decimals), square and wide boxes, both bends,
    /// both orientations and single distortions.
    #[test]
    // Recorded control points, not approximations of constants.
    #[allow(clippy::approx_constant)]
    fn presets_match_reference_patches() {
        #[allow(clippy::type_complexity)]
        let cases: [(WarpStyle, f64, f64, f64, bool, [f64; 2], usize, usize, &[f64]); 18] = [
            (WarpStyle::Arc, 50.0, 0.0, 0.0, false, [256.0, 256.0], 4, 2, &[-0.7071, 0.2929, -0.0404, -0.3738, 1.0404, -0.3738, 1.7071, 0.2929, 0.0000, 1.0000, 0.2761, 0.7239, 0.7239, 0.7239, 1.0000, 1.0000][..]),
            (WarpStyle::Arc, 50.0, 0.0, 0.0, false, [384.0, 128.0], 4, 2, &[-0.2357, 0.2929, 0.1706, -0.9261, 0.8294, -0.9261, 1.2357, 0.2929, -0.0000, 1.0000, 0.2761, 0.1716, 0.7239, 0.1716, 1.0000, 1.0000][..]),
            (WarpStyle::ArcLower, -30.0, 0.0, 0.0, false, [384.0, 128.0], 4, 2, &[0.0000, 0.0000, 0.3333, 0.0000, 0.6667, 0.0000, 1.0000, 0.0000, 0.0000, 1.0000, 0.3141, 0.5198, 0.6859, 0.5198, 1.0000, 1.0000][..]),
            (WarpStyle::ShellLower, -30.0, 0.0, 0.0, false, [256.0, 256.0], 4, 4, &[0.0000, 0.0000, 0.3333, 0.0000, 0.6667, 0.0000, 1.0000, 0.0000, 0.0000, 0.3333, 0.3333, 0.3333, 0.6667, 0.3333, 1.0000, 0.3333, -0.1513, 0.7030, 0.3333, 0.6667, 0.6667, 0.6667, 1.1513, 0.7030, 0.0000, 1.0000, 0.3141, 0.8399, 0.6859, 0.8399, 1.0000, 1.0000][..]),
            (WarpStyle::ShellUpper, 50.0, 0.0, 0.0, false, [384.0, 128.0], 4, 4, &[-0.2357, 0.2929, 0.1706, -0.9261, 0.8294, -0.9261, 1.2357, 0.2929, -0.1571, 0.5286, 0.3333, 0.3333, 0.6667, 0.3333, 1.1571, 0.5286, 0.0000, 0.6667, 0.3333, 0.6667, 0.6667, 0.6667, 1.0000, 0.6667, 0.0000, 1.0000, 0.3333, 1.0000, 0.6667, 1.0000, 1.0000, 1.0000][..]),
            (WarpStyle::Rise, -30.0, 0.0, 0.0, false, [384.0, 128.0], 4, 2, &[0.0000, -0.6000, 0.3333, -0.6000, 0.6667, 0.0000, 1.0000, 0.0000, 0.0000, 0.4000, 0.3333, 0.4000, 0.6667, 1.0000, 1.0000, 1.0000][..]),
            (WarpStyle::Twist, -30.0, 0.0, 0.0, false, [256.0, 256.0], 4, 4, &[0.0000, 0.0000, 0.3333, 0.0000, 0.6667, 0.0000, 1.0000, 0.0000, 0.0000, 0.3333, 0.3333, 0.6333, 0.3667, 0.3333, 1.0000, 0.3333, 0.0000, 0.6667, 0.6333, 0.6667, 0.6667, 0.3667, 1.0000, 0.6667, 0.0000, 1.0000, 0.3333, 1.0000, 0.6667, 1.0000, 1.0000, 1.0000][..]),
            (WarpStyle::Twist, 50.0, 0.0, 0.0, true, [384.0, 128.0], 4, 4, &[0.0000, 0.0000, 0.3333, 0.0000, 0.6667, 0.0000, 1.0000, 0.0000, 0.0000, 0.3333, 0.8333, 0.3333, 0.6667, 0.8333, 1.0000, 0.3333, 0.0000, 0.6667, 0.3333, 0.1667, 0.1667, 0.6667, 1.0000, 0.6667, 0.0000, 1.0000, 0.3333, 1.0000, 0.6667, 1.0000, 1.0000, 1.0000][..]),
            (WarpStyle::Fisheye, -30.0, 0.0, 0.0, false, [256.0, 256.0], 4, 4, &[0.0000, 0.0000, 0.3333, 0.0000, 0.6667, 0.0000, 1.0000, 0.0000, 0.0000, 0.3333, 0.5333, 0.5333, 0.4667, 0.5333, 1.0000, 0.3333, 0.0000, 0.6667, 0.5333, 0.4667, 0.4667, 0.4667, 1.0000, 0.6667, 0.0000, 1.0000, 0.3333, 1.0000, 0.6667, 1.0000, 1.0000, 1.0000][..]),
            (WarpStyle::Inflate, 40.0, 30.0, 0.0, false, [256.0, 256.0], 3, 3, &[0.0000, 0.1500, 0.5000, -0.2000, 1.0000, -0.1500, -0.1400, 0.5000, 0.5000, 0.5000, 1.2600, 0.5000, 0.0000, 0.8500, 0.5000, 1.2000, 1.0000, 1.1500][..]),
            (WarpStyle::Arc, 40.0, 0.0, -20.0, false, [256.0, 256.0], 4, 2, &[-0.8053, 0.1910, -0.0270, -0.3745, 1.0270, -0.3745, 1.8053, 0.1910, 0.1000, 1.0000, 0.3385, 0.8267, 0.6615, 0.8267, 0.9000, 1.0000][..]),
            (WarpStyle::Arc, 40.0, 30.0, 0.0, false, [256.0, 256.0], 4, 2, &[-0.4996, 0.3123, 0.0727, -0.2271, 0.9510, -0.3335, 1.6760, 0.0696, -0.0882, 0.8786, 0.2863, 0.7302, 0.6900, 0.8366, 0.9118, 1.1214][..]),
            (WarpStyle::Wave, 50.0, 0.0, 0.0, true, [384.0, 128.0], 3, 4, &[-0.0000, 0.0000, 0.5000, 0.0000, 1.0000, 0.0000, -0.0000, 0.3333, 1.5000, 0.3333, 1.0000, 0.3333, -0.0000, 0.6667, -0.5000, 0.6667, 1.0000, 0.6667, -0.0000, 1.0000, 0.5000, 1.0000, 1.0000, 1.0000][..]),
            (WarpStyle::ArcLower, 50.0, 0.0, 0.0, true, [384.0, 128.0], 2, 4, &[0.0000, 0.0000, 1.0000, 0.0000, 0.0000, 0.3333, 1.0920, 0.2761, 0.0000, 0.6667, 1.0920, 0.7239, 0.0000, 1.0000, 1.0000, 1.0000][..]),
            (WarpStyle::Fish, -30.0, 0.0, 0.0, false, [384.0, 128.0], 4, 2, &[0.0000, 0.0000, 0.3333, 0.6000, 0.6667, -0.6000, 1.0000, 0.0000, 0.0000, 1.0000, 0.3333, 0.4000, 0.6667, 1.6000, 1.0000, 1.0000][..]),
            (WarpStyle::Rise, 40.0, 0.0, -20.0, false, [256.0, 256.0], 4, 2, &[-0.1000, 0.8800, 0.3000, 0.8800, 0.7000, -0.0800, 1.1000, -0.0800, 0.1000, 1.7200, 0.3667, 1.7200, 0.6333, 1.0800, 0.9000, 1.0800][..]),
            (WarpStyle::Bulge, -30.0, 0.0, 0.0, false, [384.0, 128.0], 4, 2, &[0.0000, 0.0000, 0.3141, 0.4802, 0.6859, 0.4802, 1.0000, 0.0000, 0.0000, 1.0000, 0.3141, 0.5198, 0.6859, 0.5198, 1.0000, 1.0000][..]),
            (WarpStyle::Squeeze, 50.0, 0.0, 0.0, false, [384.0, 128.0], 3, 3, &[0.0000, 0.0000, 0.5000, -0.2500, 1.0000, 0.0000, 0.2500, 0.5000, 0.5000, 0.5000, 0.7500, 0.5000, 0.0000, 1.0000, 0.5000, 1.2500, 1.0000, 1.0000][..]),
        ];
        for (style, bend, hd, vd, vertical, [w, h], nu, nv, flat) in cases {
            let pts: Vec<[f64; 2]> = flat.chunks_exact(2).map(|p| [10.0 + p[0] * w, 20.0 + p[1] * h]).collect();
            let want = BezierMesh::from_patch(nu, nv, &pts).unwrap();
            let got = StyleWarp::new(style, bend, hd, vd, vertical, [10.0, 20.0, 10.0 + w, 20.0 + h]).unwrap();
            for (k, (a, b)) in got.control_points().iter().zip(&want.points).enumerate() {
                assert!((a[0] - b[0]).abs() < 0.05 && (a[1] - b[1]).abs() < 0.05, "{style:?} {bend} h{hd} v{vd} vert {vertical} {w}x{h}: point {k} {a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn preset_fits_closely_as_mesh_and_affine_composes() {
        let w = Warp::preset(WarpStyle::Bulge, 40.0, B);
        let m = Warp::custom(w.to_mesh(4, 4), B);
        for (x, y) in [(20.0, 30.0), (60.0, 45.0), (100.0, 65.0)] {
            assert!(close(w.map(x, y), m.map(x, y), 0.5), "{:?} vs {:?}", w.map(x, y), m.map(x, y));
        }
        let t = Warp::custom(BezierMesh::identity(B, 1, 1), B).then_affine([1.0, 0.0, 0.0, 1.0, 5.0, -2.0]);
        assert!(close(t.map(50.0, 50.0), (55.0, 48.0), 1e-9));
        let ob = Warp::none(B).output_bounds();
        assert!((ob[0] - 10.0).abs() < 1e-9 && (ob[3] - 70.0).abs() < 1e-9);
    }
}
