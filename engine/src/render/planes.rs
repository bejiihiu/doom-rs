//! Visplane bookkeeping, ported from the `R_FindPlane` / `R_CheckPlane`
//! half of `r_plane.c`. This is the plane-marking part; the span rasterizer
//! (`R_MapPlane` / `R_MakeSpans` / `R_DrawPlanes`) lives with the plane
//! renderer and builds on these structures.
//!
//! A visplane is one flat surface (floor or ceiling) the wall code reported
//! via per-column top/bottom markers. Columns are marked while segs draw;
//! the span renderer later sweeps each visplane's `top`/`bottom` arrays to
//! emit horizontal spans. Plane identity is the vanilla `(height, picnum,
//! lightlevel)` triple: when the wall code asks for a plane that already
//! exists, it gets the existing one back; `R_CheckPlane` splits the plane
//! when a new minx/maxx range collides with already-drawn columns.
//!
//! The sky plane-mapping (`height = 0`, `lightlevel = 0`) is applied inside
//! [`PlaneSet::find_plane`], exactly like the original does for
//! `picnum == skyflatnum`; the caller decides what counts as sky and passes
//! `is_sky`.

use crate::fixed::Fixed;

use super::framebuffer::SCREEN_WIDTH;

/// Vanilla `MAXVISPLANES`.
pub const MAX_VISPLANES: usize = 128;

/// One flat surface ready for span rasterizing.
///
/// `top` and `bottom` hold per-column row markers; `0xff` means "no plane
/// at this column" (an untouched `top`) — the same sentinel the original
/// fills at plane creation and writes back around `minx`/`maxx` before
/// scanning. The arrays carry one pad byte on each side so the
/// `top[minx - 1]` / `top[maxx + 1]` sentinel writes from the span
/// rasterizer never touch real memory; indices here are `column + 1`
/// internally and never escape the array.
#[derive(Clone, Debug)]
pub struct Visplane {
    /// Plane height in fixed point (`height = 0` for the sky plane).
    pub height: Fixed,
    /// Opaque flat id assigned by the caller (the original stores the
    /// flat's lump number; nothing in the plane code interprets it).
    pub picnum: u32,
    /// Sector light level (`lightlevel = 0` for the sky plane).
    pub lightlevel: i32,
    /// Leftmost marked column. `SCREEN_WIDTH` while nothing is marked.
    pub minx: i32,
    /// Rightmost marked column. `-1` while nothing is marked.
    pub maxx: i32,
    top: [u8; SCREEN_WIDTH + 2],
    bottom: [u8; SCREEN_WIDTH + 2],
}

impl Visplane {
    fn new(height: Fixed, picnum: u32, lightlevel: i32) -> Self {
        // top starts fully "empty" (0xff) like the original's memset; bottom
        // is zeroed for hygiene — the original leaves it uninitialized, but
        // only `top == 0xff` gates the span scan, so this never changes
        // behavior, just avoids reading indeterminate bytes.
        Self {
            height,
            picnum,
            lightlevel,
            minx: SCREEN_WIDTH as i32,
            maxx: -1,
            top: [0xff; SCREEN_WIDTH + 2],
            bottom: [0x00; SCREEN_WIDTH + 2],
        }
    }

    /// Column -> internal index with the left pad folded in.
    #[inline]
    fn index(x: i32) -> usize {
        // x range is -1..=SCREEN_WIDTH; see the type docs.
        (x + 1) as usize
    }

    /// The `top` marker at screen column `x` (`x` may be `-1` or
    /// `SCREEN_WIDTH`, the padded sentinel slots).
    pub fn top_at(&self, x: i32) -> u8 {
        self.top[Self::index(x)]
    }

    /// The `bottom` marker at screen column `x` (`x` may be `-1` or
    /// `SCREEN_WIDTH`).
    pub fn bottom_at(&self, x: i32) -> u8 {
        self.bottom[Self::index(x)]
    }

    /// Marks column `x` (`0..SCREEN_WIDTH`) as part of this plane.
    /// `top`/`bottom` are plane-spanning row markers; values outside
    /// `0..=0xff` clamp to the byte range (the markers are always rows, so
    /// the range is fully sufficient; the original truncates mod 256).
    pub fn mark(&mut self, x: i32, top: i32, bottom: i32) {
        assert!(
            (0..SCREEN_WIDTH as i32).contains(&x),
            "mark out of bounds: {x}"
        );
        self.top[Self::index(x)] = top.clamp(0, 0xff) as u8;
        self.bottom[Self::index(x)] = bottom.clamp(0, 0xff) as u8;
    }

    /// Writes a `top` sentinel at `x` (`-1` and `SCREEN_WIDTH` allowed).
    /// Used by the span rasterizer to fence the `minx`/`maxx` edges.
    pub fn write_top_sentinel(&mut self, x: i32) {
        assert!(
            (-1..=SCREEN_WIDTH as i32).contains(&x),
            "sentinel out of bounds: {x}"
        );
        self.top[Self::index(x)] = 0xff;
    }
}

/// The visplane pool, mirroring the vanilla `visplanes[]` array.
///
/// Cleared per frame with [`PlaneSet::clear`]; plane indices are stable
/// within a frame. The pool reuses its storage between frames.
#[derive(Clone, Debug, Default)]
pub struct PlaneSet {
    planes: Vec<Visplane>,
}

impl PlaneSet {
    /// An empty pool.
    pub fn new() -> Self {
        Self { planes: Vec::new() }
    }

    /// Mirrors the visplane part of `R_ClearPlanes`: forgets every plane
    /// from the last frame.
    pub fn clear(&mut self) {
        self.planes.clear();
    }

    /// Number of planes in the pool (`lastvisplane - visplanes`).
    pub fn len(&self) -> usize {
        self.planes.len()
    }

    /// Always false in a fresh pool; a pool always holds zero or more planes.
    pub fn is_empty(&self) -> bool {
        self.planes.is_empty()
    }

    /// The plane at `index`, if it exists.
    pub fn get(&self, index: usize) -> Option<&Visplane> {
        self.planes.get(index)
    }

    /// The plane at `index`, if it exists.
    pub fn get_mut(&mut self, index: usize) -> Option<&mut Visplane> {
        self.planes.get_mut(index)
    }

    /// `R_FindPlane`: returns the index of the plane matching
    /// `(height, picnum, lightlevel)`, creating it if needed.
    ///
    /// `is_sky` remaps `(height, lightlevel)` to `(0, 0)` so all sky
    /// ceilings share one plane; the caller passes `picnum == sky_picnum`.
    ///
    /// Returns `None` when the pool is full (`MAXVISPLANES`); the original
    /// aborts the process here. The renderer logs the error and skips
    /// marking rather than crash.
    pub fn find_plane(
        &mut self,
        height: Fixed,
        picnum: u32,
        lightlevel: i32,
        is_sky: bool,
    ) -> Option<usize> {
        let (height, lightlevel) = if is_sky {
            (Fixed::ZERO, 0)
        } else {
            (height, lightlevel)
        };

        for (index, plane) in self.planes.iter().enumerate() {
            if plane.height == height && plane.picnum == picnum && plane.lightlevel == lightlevel {
                return Some(index);
            }
        }

        if self.planes.len() >= MAX_VISPLANES {
            tracing::error!("visplane pool exhausted ({MAX_VISPLANES}); skipping plane marker");
            return None;
        }

        self.planes.push(Visplane::new(height, picnum, lightlevel));
        Some(self.planes.len() - 1)
    }

    /// `R_CheckPlane`: ensures `plane` can span the columns `[start, stop]`
    /// without overlapping an already-drawn span; when it cannot, a fresh
    /// visplane for the same `(height, picnum, lightlevel)` is created and
    /// its index returned. Returns `None` (and logs) on pool exhaustion.
    pub fn check_plane(&mut self, plane: usize, start: i32, stop: i32) -> Option<usize> {
        let current = self.planes.get(plane)?;
        let (intrl, unionl) = if start < current.minx {
            (current.minx, start)
        } else {
            (start, current.minx)
        };
        let (intrh, unionh) = if stop > current.maxx {
            (current.maxx, stop)
        } else {
            (stop, current.maxx)
        };

        // if every column in the intersection is still 0xff, reuse the plane
        let mut x = intrl;
        while x <= intrh {
            if self.planes[plane].top_at(x) != 0xff {
                break;
            }
            x += 1;
        }

        if x > intrh {
            self.planes[plane].minx = unionl;
            self.planes[plane].maxx = unionh;
            return Some(plane);
        }

        // make a new visplane
        if self.planes.len() >= MAX_VISPLANES {
            tracing::error!("visplane pool exhausted ({MAX_VISPLANES}); skipping plane marker");
            return None;
        }
        let (height, picnum, lightlevel) = {
            let pl = &self.planes[plane];
            (pl.height, pl.picnum, pl.lightlevel)
        };
        let mut fresh = Visplane::new(height, picnum, lightlevel);
        fresh.minx = start;
        fresh.maxx = stop;
        // top already 0xff from creation
        self.planes.push(fresh);
        Some(self.planes.len() - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_plane_matches_on_height_picnum_light() {
        let mut set = PlaneSet::new();
        let a = set.find_plane(Fixed::from_int(128), 7, 192, false).unwrap();
        let b = set.find_plane(Fixed::from_int(128), 7, 192, false).unwrap();
        assert_eq!(a, b, "same triple must reuse the same plane");
        let c = set.find_plane(Fixed::from_int(128), 8, 192, false).unwrap();
        assert_ne!(a, c, "different flat id must create a plane");
        let d = set.find_plane(Fixed::from_int(64), 7, 192, false).unwrap();
        assert_ne!(a, d, "different height must create a plane");
        let e = set.find_plane(Fixed::from_int(128), 7, 96, false).unwrap();
        assert_ne!(a, e, "different light level must create a plane");
    }

    #[test]
    fn sky_planes_collapse_onto_height_zero_light_zero() {
        let mut set = PlaneSet::new();
        let a = set.find_plane(Fixed::from_int(128), 3, 192, true).unwrap();
        let b = set.find_plane(Fixed::from_int(256), 3, 96, true).unwrap();
        assert_eq!(a, b, "all sky ceilings share one plane");
        assert_eq!(set.get(a).unwrap().height, Fixed::ZERO);
        assert_eq!(set.get(a).unwrap().lightlevel, 0);
    }

    #[test]
    fn clear_forgets_planes() {
        let mut set = PlaneSet::new();
        set.find_plane(Fixed::from_int(128), 7, 192, false);
        set.clear();
        assert!(set.is_empty());
    }

    #[test]
    fn check_plane_extends_into_untouched_columns() {
        let mut set = PlaneSet::new();
        let p = set.find_plane(Fixed::from_int(128), 7, 192, false).unwrap();
        let p = set.check_plane(p, 10, 20).unwrap();
        assert_eq!(p, 0);
        assert_eq!(set.get(p).unwrap().minx, 10);
        assert_eq!(set.get(p).unwrap().maxx, 20);
        // widening into virgin columns reuses the plane
        let p = set.check_plane(p, 5, 25).unwrap();
        assert_eq!(p, 0);
        assert_eq!(set.get(p).unwrap().minx, 5);
        assert_eq!(set.get(p).unwrap().maxx, 25);
    }

    #[test]
    fn check_plane_splits_on_drawn_columns() {
        let mut set = PlaneSet::new();
        let p = set.find_plane(Fixed::from_int(128), 7, 192, false).unwrap();
        let p = set.check_plane(p, 10, 20).unwrap();
        // column 12 gets marked by a wall
        set.get_mut(p).unwrap().mark(12, 100, 150);
        // a new range overlapping the drawn column must split
        let p2 = set.check_plane(p, 5, 15).unwrap();
        assert_ne!(p2, p);
        assert_eq!(set.get(p2).unwrap().minx, 5);
        assert_eq!(set.get(p2).unwrap().maxx, 15);
        // original plane unchanged
        assert_eq!(set.get(p).unwrap().minx, 10);
        assert_eq!(set.get(p).unwrap().maxx, 20);
    }

    #[test]
    fn check_plane_split_when_merely_touching_drawn_column() {
        let mut set = PlaneSet::new();
        let p = set.find_plane(Fixed::from_int(128), 7, 192, false).unwrap();
        let p = set.check_plane(p, 10, 20).unwrap();
        set.get_mut(p).unwrap().mark(10, 100, 150);
        // start == drawn column: intersection {10..10} is drawn -> split
        let p2 = set.check_plane(p, 10, 15).unwrap();
        assert_ne!(p2, p);
    }

    #[test]
    fn fresh_plane_columns_are_empty() {
        let set = PlaneSet::new();
        let mut set = set;
        let p = set.find_plane(Fixed::from_int(96), 1, 160, false).unwrap();
        let plane = set.get(p).unwrap();
        assert_eq!(plane.minx, SCREEN_WIDTH as i32);
        assert_eq!(plane.maxx, -1);
        assert_eq!(plane.top_at(0), 0xff);
        assert_eq!(plane.top_at(SCREEN_WIDTH as i32 - 1), 0xff);
    }

    #[test]
    fn mark_and_sentinel_round_trip() {
        let mut set = PlaneSet::new();
        let p = set.find_plane(Fixed::from_int(96), 1, 160, false).unwrap();
        set.get_mut(p).unwrap().mark(3, 40, 80);
        assert_eq!(set.get(p).unwrap().top_at(3), 40);
        assert_eq!(set.get(p).unwrap().bottom_at(3), 80);
        set.get_mut(p).unwrap().write_top_sentinel(3);
        assert_eq!(set.get(p).unwrap().top_at(3), 0xff);
        // padded sentinel slots work
        set.get_mut(p).unwrap().write_top_sentinel(-1);
        set.get_mut(p)
            .unwrap()
            .write_top_sentinel(SCREEN_WIDTH as i32);
        assert_eq!(set.get(p).unwrap().top_at(-1), 0xff);
        assert_eq!(set.get(p).unwrap().top_at(SCREEN_WIDTH as i32), 0xff);
    }

    #[test]
    fn find_plane_pool_exhaustion_returns_none() {
        let mut set = PlaneSet::new();
        for i in 0..MAX_VISPLANES {
            set.find_plane(Fixed::from_int(i as i32), i as u32, 192, false)
                .unwrap();
        }
        assert_eq!(
            set.find_plane(Fixed::from_int(9999), 9999, 192, false),
            None
        );
    }
}
