//! Value tables of the Filter Gallery (the PhotoSplice authors' own data).

/// Indexed by strokePressure 0..15 (Colored Pencil, Sumi-e); value is the Scale factor applied to the detail plane.
pub(super) const STROKE_AMOUNT: [f64; 16] = [0.0, 5.0, 10.0, 15.0, 20.0, 25.0, 35.0, 45.0, 65.0, 85.0, 105.0, 125.0, 145.0, 165.0, 185.0, 205.0];

/// Indexed by relief 0..25 (Note Paper, Patchwork); value is the Directional relief weight.
pub(super) const RELIEF_FACTOR: [f64; 26] =
    [0.0, 0.2, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0, 1.1, 1.2, 1.3, 1.4, 1.5, 1.7, 2.0, 2.5, 3.0, 3.5, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];

/// Relief factor function phi(r) for r > 40: entry r - 41 for relief 41..50 (r <= 40 uses r/40); Rough Pastels, Underpainting, Conte Crayon.
pub(super) const RELIEF_TAIL: [f64; 10] = [1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0, 5.0, 7.5, 10.0];

/// 13-tap field Gaussian sigma, indexed by sigma index 0..14 (Spatter: smoothness - 1; Sprayed Strokes: 2 if strokeLength >>> 1 is 0, else 1); index 0 is never used.
pub(super) const FIELD_SIGMA: [f64; 15] = [0.0, 0.6, 0.7, 0.8, 1.0, 1.2, 1.4, 1.6, 1.8, 2.0, 2.2, 2.6, 3.0, 4.0, 5.0];

/// Percentile fraction for the Clipped square rank filter, indexed by brushType in order simple, lightRough, darkRough, wideSharp, wideBlurry, sparkle (0..5).
pub(super) const PAINT_DAUBS_PERCENTILE: [f64; 6] = [0.75, 0.85, 0.68, 0.75, 0.78, 0.62];

/// Indexed by sprayRadius 0..25 (Sprayed Strokes); strength = 0.07 + entry.
pub(super) const SPRAYED_STRENGTH: [f64; 26] =
    [0.0, 0.02, 0.04, 0.06, 0.08, 0.1, 0.12, 0.14, 0.16, 0.18, 0.2, 0.22, 0.24, 0.26, 0.28, 0.3, 0.34, 0.38, 0.5, 0.65, 0.75, 0.85, 1.0, 1.5, 2.2, 3.0];

/// Glass strength multiplier indexed by global texture index (0 blocks, 1 canvas, 2 frosted, 3 tinyLens, 4 brick, 5 burlap, 6 sandstone); entry 7 is never reached.
pub(super) const GLASS_TEXTURE_FACTOR: [f64; 8] = [1.0, 0.4, 0.5, 0.5, 0.5, 0.5, 0.5, 1.0];

/// Indexed by lightDarkBalance 0..50 directly; for balance > 50 the entry at 101 - balance is used (1..50); entry 0 is a sentinel.
pub(super) const GRAPHIC_PEN_DENSITY: [f64; 51] = [
    -1000.0, 34.0, 51.0, 61.0, 71.0, 80.0, 87.0, 95.0, 101.0, 108.0, 113.0, 119.0, 124.0, 129.0, 134.0, 139.0, 143.0, 148.0, 153.0, 157.0, 161.0, 165.0, 168.0,
    172.0, 175.0, 180.0, 183.0, 186.0, 190.0, 194.0, 197.0, 200.0, 203.0, 207.0, 210.0, 213.0, 216.0, 218.0, 222.0, 225.0, 228.0, 230.0, 233.0, 236.0, 239.0,
    241.0, 244.0, 247.0, 250.0, 252.0, 255.0,
];

/// Indexed by contrast 0..25 (Torn Edges); value is the Scale / Inverse scale factor.
pub(super) const TORN_EDGES_CONTRAST: [f64; 26] =
    [1.0, 1.0, 1.05, 1.1, 1.15, 1.2, 1.25, 1.3, 1.35, 1.4, 1.45, 1.5, 1.6, 1.7, 1.8, 2.0, 2.25, 2.5, 2.75, 3.0, 3.5, 4.0, 5.0, 6.0, 8.0, 10.0];

/// Indexed by |glowSize| 0..24 (Neon Glow); outer box size = |glowSize| + entry.
pub(super) const NEON_GLOW_INCREMENT: [f64; 25] =
    [1.0, 0.0, 1.0, 2.0, 1.0, 2.0, 1.0, 2.0, 3.0, 2.0, 3.0, 2.0, 3.0, 2.0, 3.0, 2.0, 3.0, 4.0, 3.0, 4.0, 5.0, 4.0, 5.0, 4.0, 5.0];

/// Diffuse Glow glow gain, indexed by glowAmount 0..20.
pub(super) const DIFFUSE_GLOW_GLOW: [f64; 21] = [0.0, 0.75, 1.0, 1.05, 1.1, 1.15, 1.2, 1.25, 1.3, 1.35, 1.4, 1.45, 1.5, 1.6, 1.7, 1.8, 2.0, 2.5, 3.0, 3.5, 4.0];

/// Diffuse Glow clear gain, indexed by clearAmount 0..20.
pub(super) const DIFFUSE_GLOW_CLEAR: [f64; 21] = [0.0, 0.25, 0.5, 0.75, 0.9, 1.0, 1.1, 1.2, 1.3, 1.4, 1.5, 1.6, 1.7, 1.8, 1.9, 2.0, 2.25, 2.5, 3.0, 3.5, 4.0];
