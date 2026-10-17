//! `Surface::content_bounds`, which costs a lookup per unchanged tile: each copy-on-write tile
//! keeps its own content bounds until it is written (`photosplice_raster::Tile::content_bounds`),
//! so the CPU compositor asking for an effect layer's bounds in every tile, and the GPU planner
//! every frame, don't rescan pixels.

use photosplice_geom::Rect;
use photosplice_raster::Surface;

/// `Surface::content_bounds` (cached per tile).
pub fn content_bounds(s: &Surface) -> Rect {
    s.content_bounds()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shared_tile_reads_the_same_bounds_at_every_coordinate() {
        let mut s = Surface::new(photosplice_color::PixelFormat::RGBA8);
        s.fill_rect(Rect::new(-300, 10, 1300, 900), &[0.2, 0.4, 0.6, 1.0]);
        assert_eq!(content_bounds(&s), Rect::new(-300, 10, 1300, 900));
        assert_eq!(content_bounds(&s), s.content_bounds(), "cached");
    }

    #[test]
    fn a_written_tile_forgets_its_bounds() {
        let mut s = Surface::new(photosplice_color::PixelFormat::RGBA8);
        s.fill_rect(Rect::new(10, 10, 20, 20), &[1.0, 0.0, 0.0, 1.0]);
        assert_eq!(content_bounds(&s), Rect::new(10, 10, 20, 20));
        // Every way of writing a tile: a fill, a pixel, a region, the bytes themselves.
        s.fill_rect(Rect::new(30, 5, 40, 12), &[0.0, 1.0, 0.0, 1.0]);
        assert_eq!(content_bounds(&s), Rect::new(10, 5, 40, 20));
        s.write_pixel(100, 100, &[0.0, 0.0, 1.0, 1.0]);
        assert_eq!(content_bounds(&s), Rect::new(10, 5, 101, 101));
        s.write_region(Rect::new(0, 0, 200, 200), &vec![0.0; 200 * 200 * 4]);
        assert_eq!(content_bounds(&s), Rect::EMPTY, "cleared");
        let c = photosplice_geom::TileCoord::new(0, 0);
        s.tile_mut(c).bytes_mut()[(5 * 256 + 7) * 4 + 3] = 255;
        assert_eq!(content_bounds(&s), Rect::new(7, 5, 8, 6));
        // A clone shares the cache until either side writes.
        let mut copy = s.clone();
        copy.write_pixel(50, 60, &[1.0, 1.0, 1.0, 1.0]);
        assert_eq!((content_bounds(&s), content_bounds(&copy)), (Rect::new(7, 5, 8, 6), Rect::new(7, 5, 51, 61)));
        // A different default pixel scans again rather than reusing bounds made for another.
        let g = Surface::with_default(photosplice_color::PixelFormat::GRAY8, &[1.0]);
        assert_eq!(content_bounds(&g), Rect::EMPTY);
    }
}
