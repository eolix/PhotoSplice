//! Warp Text: bends the glyph outlines of a type layer with one of Reference's fifteen warp
//! styles (the PSD `warp` descriptor's `warpStyle`).
//!
//! The style math lives in `photosplice_geom::warp` (shared with Edit › Transform › Warp): the
//! Bézier patch Reference builds for each style, the identity at `bend = 0` and continuous in
//! `bend`. The mapping is evaluated per point; the renderer subdivides long segments so straight
//! edges bend.

use photosplice_doc::text::TextWarp;
use photosplice_geom::warp::{StyleWarp, WarpStyle};

/// Warp styles in Reference's menu order: (PSD `warpStyle` value, short id used by commands).
pub const STYLES: [(&str, &str); 15] = [
    ("warpArc", "arc"),
    ("warpArcLower", "arcLower"),
    ("warpArcUpper", "arcUpper"),
    ("warpArch", "arch"),
    ("warpBulge", "bulge"),
    ("warpShellLower", "shellLower"),
    ("warpShellUpper", "shellUpper"),
    ("warpFlag", "flag"),
    ("warpWave", "wave"),
    ("warpFish", "fish"),
    ("warpRise", "rise"),
    ("warpFisheye", "fisheye"),
    ("warpInflate", "inflate"),
    ("warpSqueeze", "squeeze"),
    ("warpTwist", "twist"),
];

/// PSD style name (`warpArc`) for a short id (`arc`, case-insensitive) or a PSD name.
pub fn psd_style(id: &str) -> Option<&'static str> {
    let l = id.to_ascii_lowercase();
    if l == "none" || l == "warpnone" {
        return Some("warpNone");
    }
    STYLES.iter().find(|(psd, short)| psd.to_ascii_lowercase() == l || short.to_ascii_lowercase() == l).map(|(psd, _)| *psd)
}

/// A warp prepared for one layout: the shared style math of
/// [`photosplice_geom::warp::StyleWarp`] (also used by Edit › Transform › Warp) over the text box.
#[derive(Clone, Copy, Debug)]
pub struct Warp(StyleWarp);

impl Warp {
    /// Prepares `w` for text whose bounds are `[x0, y0, x1, y1]` (text space). `None` for
    /// `warpNone`, unknown styles, an all-zero warp or empty bounds.
    pub fn new(w: &TextWarp, bounds: [f32; 4]) -> Option<Warp> {
        let style = WarpStyle::parse(&w.style).filter(|s| s.is_preset() && s.psd_name() == w.style)?;
        StyleWarp::new(style, f64::from(w.value), f64::from(w.horizontal_distortion), f64::from(w.vertical_distortion), !w.horizontal, bounds.map(f64::from))
            .map(Warp)
    }

    /// Longest segment (text-space px) worth sending through [`Warp::apply`] unsplit.
    pub fn max_segment(&self) -> f64 {
        self.0.max_segment()
    }

    /// Maps a text-space point.
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        self.0.apply(x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn warp(style: &str, value: f32) -> Warp {
        let w = TextWarp { style: psd_style(style).unwrap().into(), value, horizontal_distortion: 0.0, vertical_distortion: 0.0, horizontal: true };
        Warp::new(&w, [0.0, -40.0, 200.0, 10.0]).unwrap()
    }

    #[test]
    fn styles_resolve_and_none_is_identity() {
        assert_eq!(psd_style("Arc"), Some("warpArc"));
        assert_eq!(psd_style("warpTwist"), Some("warpTwist"));
        assert_eq!(psd_style("none"), Some("warpNone"));
        assert_eq!(psd_style("spiral"), None);
        let none = TextWarp { style: "warpNone".into(), value: 50.0, ..Default::default() };
        assert!(Warp::new(&none, [0.0, 0.0, 10.0, 10.0]).is_none());
        let flat = TextWarp { style: "warpArc".into(), value: 0.0, ..Default::default() };
        assert!(Warp::new(&flat, [0.0, 0.0, 10.0, 10.0]).is_none());
    }

    #[test]
    fn every_style_is_continuous_and_fixes_the_centre_column() {
        for (_, id) in STYLES {
            let w = warp(id, 50.0);
            let (x, y) = w.apply(100.0, -15.0);
            assert!(x.is_finite() && y.is_finite(), "{id}");
            // Tiny bends barely move anything.
            let t = warp(id, 0.5);
            let (x2, y2) = t.apply(20.0, 0.0);
            assert!((x2 - 20.0).abs() < 2.0 && y2.abs() < 2.0, "{id}: {x2},{y2}");
        }
    }

    /// Reference's Arc keeps the inner edge's corners (the bottom one for a positive bend) and
    /// lifts the rest onto concentric arcs; a negative bend keeps the top corners instead.
    #[test]
    fn arc_keeps_the_inner_corners_and_lifts_the_middle() {
        let close = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6;
        let w = warp("arc", 50.0);
        assert!(close(w.apply(0.0, 10.0), (0.0, 10.0)) && close(w.apply(200.0, 10.0), (200.0, 10.0)));
        assert!(w.apply(100.0, -40.0).1 < -40.0, "the top centre rises");
        assert!(w.apply(0.0, -40.0).0 < 0.0, "the top corners spread along the outer arc");
        let s = warp("arc", -50.0);
        assert!(close(s.apply(0.0, -40.0), (0.0, -40.0)));
        assert!(s.apply(100.0, 10.0).1 > 10.0, "the bottom centre drops");
    }

    /// Bulge swells the middle; Squeeze pulls the sides in and pushes the top and bottom out.
    #[test]
    fn bulge_and_squeeze_reshape_the_middle() {
        let b = warp("bulge", 50.0);
        assert!(b.apply(100.0, -40.0).1 < -50.0 && b.apply(100.0, 10.0).1 > 20.0);
        let s = warp("squeeze", 50.0);
        assert!(s.apply(100.0, -40.0).1 < -40.0);
        assert!(s.apply(0.0, -15.0).0 > 0.0 && s.apply(200.0, -15.0).0 < 200.0);
    }
}
