//! Layer Style › Blending Options › Blend If: per-channel value ranges that hide a layer's
//! pixels by their own value ("This Layer") or by the value beneath them ("Underlying Layer").
//!
//! Each slider pair is stored as Photoshop stores it in a PSD layer record's blending ranges: a
//! black point and a white point, each split into a low and a high half (Alt-drag in the dialog),
//! on the 0..=255 scale whatever the document's depth. A value below the black low point or above
//! the white high point is hidden; between the two halves of a split point it fades linearly.

use serde::{Deserialize, Serialize};

/// One Blend If slider pair: `black` = (low, high), `white` = (low, high), all on 0..=255.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlendRange {
    pub black: [u8; 2],
    pub white: [u8; 2],
}

impl Default for BlendRange {
    fn default() -> Self {
        Self::FULL
    }
}

impl BlendRange {
    /// Every value blends (the sliders at the ends).
    pub const FULL: BlendRange = BlendRange { black: [0, 0], white: [255, 255] };

    /// Whether this range hides nothing.
    pub fn is_full(&self) -> bool {
        *self == Self::FULL
    }

    /// From PSD's byte order: black low, black high, white low, white high.
    pub fn from_bytes(b: [u8; 4]) -> Self {
        BlendRange { black: [b[0], b[1]], white: [b[2], b[3]] }
    }

    /// PSD's byte order (see [`BlendRange::from_bytes`]).
    pub fn to_bytes(self) -> [u8; 4] {
        [self.black[0], self.black[1], self.white[0], self.white[1]]
    }

    /// How much of a pixel with value `v` (0..=255, fractional for deep documents) shows:
    /// 0 below the black point or above the white point, 1 between them, and a linear ramp across
    /// a split point. An unsplit black point at `b` shows `v >= b`; an unsplit white point at `w`
    /// shows `v <= w`. The exact sliders, without the document's step ([`BlendRange::weight_at`]).
    pub fn weight(&self, v: f32) -> f32 {
        self.weight_at(v, 0.0)
    }

    /// [`BlendRange::weight`] as Reference evaluates it on a document whose smallest value step
    /// is `step` (on the 0..=255 scale: 1 for 8-bit, 255/32768 for 16-bit, 0 for 32-bit). The
    /// black ramp starts one step below its low half and the white ramp ends one step above its
    /// high half, so an 8-bit split black point at 32/96 fades over 31..96 and a split white point
    /// at 160/224 over 160..225 (measured on 256-level grids; the ends of the scale clamp, so 0/255
    /// fades over 0..255). The two ramps meet by their minimum: black and white both split across
    /// the whole scale show half of level 128, not a quarter.
    pub fn weight_at(&self, v: f32, step: f32) -> f32 {
        let ramp = |lo: f32, hi: f32, x: f32| -> f32 {
            // 0 at or below `lo`, 1 at or above `hi`.
            if hi <= lo { if x >= lo { 1.0 } else { 0.0 } } else { ((x - lo) / (hi - lo)).clamp(0.0, 1.0) }
        };
        let [b0, b1] = self.black.map(f32::from);
        let [w0, w1] = self.white.map(f32::from);
        let lo = ramp((b0 - step).max(0.0).min(b0), b1, v);
        // Mirror for the white point: 1 at or below its low half, 0 above its high half.
        let hi = ramp(255.0 - (w1 + step).min(255.0).max(w1), 255.0 - w0, 255.0 - v);
        lo.min(hi)
    }
}

/// A layer's Blend If settings. `ranges[0]` is the "Gray" entry (composite of the colour
/// channels), then one entry per colour channel of the document's mode (R, G, B / C, M, Y, K /
/// L, a, b / Gray), each as `[this layer, underlying layer]`. Empty = every pixel blends.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlendIf {
    pub ranges: Vec<[BlendRange; 2]>,
}

impl BlendIf {
    /// Whether no range hides anything (the Photoshop default).
    pub fn is_default(&self) -> bool {
        self.ranges.iter().flatten().all(BlendRange::is_full)
    }

    /// Entry `i` (0 = Gray), full when not stored.
    pub fn get(&self, i: usize) -> [BlendRange; 2] {
        self.ranges.get(i).copied().unwrap_or([BlendRange::FULL; 2])
    }

    /// Set entry `i` (0 = Gray), growing the list with full ranges as needed. Trailing full
    /// entries are dropped so an all-default setting compares equal to `BlendIf::default()`.
    pub fn set(&mut self, i: usize, pair: [BlendRange; 2]) {
        if self.ranges.len() <= i {
            self.ranges.resize(i + 1, [BlendRange::FULL; 2]);
        }
        self.ranges[i] = pair;
        while self.ranges.last().is_some_and(|p| p.iter().all(BlendRange::is_full)) {
            self.ranges.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_range_shows_everything() {
        let r = BlendRange::FULL;
        assert!(r.is_full());
        for v in [0.0, 0.5, 127.0, 254.9, 255.0] {
            assert_eq!(r.weight(v), 1.0);
        }
    }

    #[test]
    fn unsplit_points_are_hard_thresholds() {
        let r = BlendRange { black: [50, 50], white: [200, 200] };
        assert_eq!(r.weight(49.0), 0.0);
        assert_eq!(r.weight(49.9), 0.0);
        assert_eq!(r.weight(50.0), 1.0);
        assert_eq!(r.weight(200.0), 1.0);
        assert_eq!(r.weight(200.1), 0.0);
        assert!(!r.is_full());
    }

    #[test]
    fn split_points_ramp_linearly() {
        let r = BlendRange { black: [0, 100], white: [155, 255] };
        // A split black point fades from its low half (hidden) to its high half (shown).
        assert_eq!(r.weight(0.0), 0.0);
        assert!((r.weight(50.0) - 0.5).abs() < 1e-6);
        assert_eq!(r.weight(100.0), 1.0);
        assert_eq!(r.weight(155.0), 1.0);
        assert!((r.weight(205.0) - 0.5).abs() < 1e-6);
        assert_eq!(r.weight(255.0), 0.0);
        assert!(!r.is_full());
    }

    #[test]
    fn eight_bit_ramps_widen_by_a_level_and_meet_by_their_minimum() {
        // Reference grids: 32/96 fades over 31..96, 160/224 over 160..225.
        let r = BlendRange { black: [32, 96], white: [160, 224] };
        assert!((r.weight_at(64.0, 1.0) - 33.0 / 65.0).abs() < 1e-6);
        assert!((r.weight_at(32.0, 1.0) - 1.0 / 65.0).abs() < 1e-6);
        assert_eq!(r.weight_at(31.0, 1.0), 0.0);
        assert!((r.weight_at(224.0, 1.0) - 1.0 / 65.0).abs() < 1e-6);
        assert_eq!(r.weight_at(225.0, 1.0), 0.0);
        // Unsplit points stay hard thresholds on whole levels.
        let hard = BlendRange { black: [50, 50], white: [200, 200] };
        assert_eq!(hard.weight_at(49.0, 1.0), 0.0);
        assert_eq!(hard.weight_at(50.0, 1.0), 1.0);
        assert_eq!(hard.weight_at(201.0, 1.0), 0.0);
        // Split across the whole scale: the ends clamp and the two ramps meet by their minimum.
        let wide = BlendRange { black: [0, 255], white: [0, 255] };
        assert!((wide.weight_at(128.0, 1.0) - 127.0 / 255.0).abs() < 1e-6);
        assert!((wide.weight_at(64.0, 1.0) - 64.0 / 255.0).abs() < 1e-6);
        assert!((wide.weight_at(192.0, 1.0) - 63.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn bytes_round_trip_in_psd_order() {
        let r = BlendRange { black: [10, 20], white: [230, 240] };
        assert_eq!(r.to_bytes(), [10, 20, 230, 240]);
        assert_eq!(BlendRange::from_bytes(r.to_bytes()), r);
    }

    #[test]
    fn set_trims_trailing_full_entries() {
        let mut b = BlendIf::default();
        assert!(b.is_default());
        let r = BlendRange { black: [30, 30], white: [255, 255] };
        b.set(2, [BlendRange::FULL, r]);
        assert_eq!(b.ranges.len(), 3);
        assert_eq!(b.get(2), [BlendRange::FULL, r]);
        assert_eq!(b.get(7), [BlendRange::FULL; 2]);
        assert!(!b.is_default());
        b.set(2, [BlendRange::FULL; 2]);
        assert_eq!(b, BlendIf::default());
    }
}

/// Blending Options › Advanced Blending › Knockout: where the layer's shape "punches through"
/// to. Inside it the layers in between are removed and the layer's content (at its fill
/// opacity) lands on what is there. PSD `knko`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Knockout {
    #[default]
    None,
    /// To the start of the layer's own group.
    Shallow,
    /// Out through pass-through groups to the Background layer (or transparency without one),
    /// stopping at an isolated group's start.
    Deep,
}

impl Knockout {
    /// From the `knko` block's first byte (0 none, 1 shallow, 2 deep).
    pub fn from_psd(v: u8) -> Knockout {
        match v {
            1 => Knockout::Shallow,
            2 => Knockout::Deep,
            _ => Knockout::None,
        }
    }

    /// The `knko` byte.
    pub fn psd(self) -> u8 {
        match self {
            Knockout::None => 0,
            Knockout::Shallow => 1,
            Knockout::Deep => 2,
        }
    }
}
