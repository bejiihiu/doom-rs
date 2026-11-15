//! DOOM patch decoding, ported from `r_defs.h` (`patch_t`, `column_t`,
//! `post_t`), `v_video.c` (`V_DrawPatch`) and `st_stuff.c` etc.
//!
//! A patch is a vertical-column picture used for sprites, weapons, status
//! bar graphics and wall texture building blocks. Layout (all integers
//! little-endian):
//!
//! ```text
//! offset  size        field
//! 0       2           width, i16, must be > 0
//! 2       2           height, i16, must be > 0
//! 4       2           left offset, i16 (origin shift, may be negative)
//! 6       2           top offset, i16 (origin shift, may be negative)
//! 8       width*4     column offsets, i32 each, relative to lump start
//! ...     ...         column data
//! ```
//!
//! Each column is a list of posts, terminated by a post whose `topdelta`
//! byte is `0xff`. A post on disk is:
//!
//! ```text
//! byte  topdelta   vertical offset of the post within the column
//! byte  length     number of pixel bytes that follow
//! byte  unused     padding, skipped by every reader
//! byte  pixels[length]
//! byte  unused     trailing padding
//! ```
//!
//! So a post costs `length + 4` bytes and the scan always advances, which
//! is what keeps corrupt columns from looping forever.
//!
//! Everything is bounds-validated: pointers must stay inside the lump and
//! every post must fit before the scan can proceed, so corrupt patches
//! decode to `Err`, never to a panic or an out-of-bounds read. Heights over
//! 255 pixels are legal in the format (posts simply wrap past 256); they
//! are kept as raw rows here and are the renderer's concern.

use super::error::{GfxError, GfxResult};
use crate::swap;

/// Size of the fixed patch header (width, height, left/top offset).
pub const PATCH_HEADER_SIZE: usize = 8;
/// Size of one column pointer entry in the header.
pub const COLUMN_PTR_SIZE: usize = 4;
/// Bytes around the pixel data of one post (topdelta, length, pad, pixels,
/// pad).
pub const POST_OVERHEAD: usize = 4;
/// `topdelta` value that terminates a column.
pub const POST_TERMINATOR: u8 = 0xff;

/// The decoded patch header, exactly as validated on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchHeader {
    /// Bounding box width. Guaranteed positive.
    pub width: i16,
    /// Bounding box height. Guaranteed positive.
    pub height: i16,
    /// Pixels to the left of the patch origin. May be negative.
    pub left_offset: i16,
    /// Pixels below the patch origin. May be negative.
    pub top_offset: i16,
    /// Column offsets into the lump, one per column, already validated to
    /// point inside the lump.
    pub column_ptrs: Vec<i32>,
}

/// One post: a vertical run of non-transparent pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Post {
    /// Vertical offset of the run within the column (`topdelta`).
    pub top_delta: u8,
    /// The raw palette indices, `length` bytes.
    pub pixels: Vec<u8>,
}

/// One column of a patch: zero or more posts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchColumn {
    /// The posts in the order they appear in the lump.
    pub posts: Vec<Post>,
}

impl PatchColumn {
    /// Number of posts in the column.
    pub fn post_count(&self) -> usize {
        self.posts.len()
    }
}

/// A fully decoded and validated patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    /// The validated header.
    pub header: PatchHeader,
    /// The decoded columns, one per `column_ptrs` entry, in order.
    pub columns: Vec<PatchColumn>,
}

/// One decoded pixel, yielded by [`Patch::pixels`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pixel {
    /// Column index within the patch.
    pub column: usize,
    /// Row within the patch (`top_delta + index into the post`).
    pub row: usize,
    /// Raw palette index.
    pub value: u8,
}

impl Patch {
    /// Number of columns in the patch.
    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    /// The column at `index`, if it exists.
    pub fn column(&self, index: usize) -> Option<&PatchColumn> {
        self.columns.get(index)
    }

    /// Total number of pixels across all posts of all columns.
    pub fn pixel_count(&self) -> usize {
        self.columns
            .iter()
            .flat_map(|col| col.posts.iter())
            .map(|post| post.pixels.len())
            .sum()
    }

    /// Iterates over every non-transparent pixel, column by column, post by
    /// post, top to bottom — the order a column renderer consumes them in.
    pub fn pixels(&self) -> Pixels<'_> {
        Pixels {
            patch: self,
            column: 0,
            post: 0,
            pixel: 0,
        }
    }
}

/// Iterator over the non-transparent pixels of a [`Patch`]. See
/// [`Patch::pixels`].
#[derive(Debug, Clone)]
pub struct Pixels<'a> {
    patch: &'a Patch,
    column: usize,
    post: usize,
    pixel: usize,
}

impl Iterator for Pixels<'_> {
    type Item = Pixel;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let column = self.patch.columns.get(self.column)?;
            let post = match column.posts.get(self.post) {
                Some(post) => post,
                None => {
                    self.column += 1;
                    self.post = 0;
                    self.pixel = 0;
                    continue;
                }
            };
            let &value = match post.pixels.get(self.pixel) {
                Some(value) => value,
                None => {
                    self.post += 1;
                    self.pixel = 0;
                    continue;
                }
            };
            let pixel = Pixel {
                column: self.column,
                row: post.top_delta as usize + self.pixel,
                value,
            };
            self.pixel += 1;
            return Some(pixel);
        }
    }
}

/// Decodes a patch lump, validating every pointer and post bound against
/// the lump size. Corrupt data yields `Err`, never a panic.
pub fn decode_patch(data: &[u8]) -> GfxResult<Patch> {
    if data.len() < PATCH_HEADER_SIZE {
        return Err(GfxError::Truncated {
            what: "patch header",
            offset: 0,
            needed: PATCH_HEADER_SIZE,
            available: data.len(),
        });
    }
    let width = swap::read_i16(data, 0)?;
    let height = swap::read_i16(data, 2)?;
    let left_offset = swap::read_i16(data, 4)?;
    let top_offset = swap::read_i16(data, 6)?;
    if width <= 0 {
        return Err(GfxError::BadValue {
            what: "patch header",
            reason: format!("width must be positive, got {width}"),
        });
    }
    if height <= 0 {
        return Err(GfxError::BadValue {
            what: "patch header",
            reason: format!("height must be positive, got {height}"),
        });
    }

    let width_us = width as usize;
    let table_size = width_us
        .checked_mul(COLUMN_PTR_SIZE)
        .ok_or(GfxError::BadValue {
            what: "patch header",
            reason: "column pointer table size overflows usize".to_string(),
        })?;
    let table_end = PATCH_HEADER_SIZE
        .checked_add(table_size)
        .ok_or(GfxError::BadValue {
            what: "patch header",
            reason: "column pointer table size overflows usize".to_string(),
        })?;
    if data.len() < table_end {
        return Err(GfxError::Truncated {
            what: "patch column pointers",
            offset: PATCH_HEADER_SIZE,
            needed: table_size,
            available: data.len() - PATCH_HEADER_SIZE,
        });
    }

    let mut column_ptrs = Vec::with_capacity(width_us);
    for i in 0..width_us {
        let offset = swap::read_i32(data, PATCH_HEADER_SIZE + i * COLUMN_PTR_SIZE)?;
        if offset < 0 || offset as usize >= data.len() {
            return Err(GfxError::OutOfBounds {
                what: "patch column pointer",
                offset: offset as i64,
                size: data.len(),
            });
        }
        column_ptrs.push(offset);
    }

    let mut columns = Vec::with_capacity(column_ptrs.len());
    for &ptr in &column_ptrs {
        let mut pos = ptr as usize;
        let mut posts = Vec::new();
        loop {
            // every iteration lands here; a column needs at least the
            // terminator byte, so going past the end is always an error
            let top = *data.get(pos).ok_or(GfxError::OutOfBounds {
                what: "patch column data",
                offset: pos as i64,
                size: data.len(),
            })?;
            if top == POST_TERMINATOR {
                break;
            }
            if data.len() - pos < 2 {
                return Err(GfxError::Truncated {
                    what: "patch post header",
                    offset: pos,
                    needed: 2,
                    available: data.len() - pos,
                });
            }
            let length = data[pos + 1] as usize;
            let post_end = pos
                .checked_add(POST_OVERHEAD)
                .and_then(|end| end.checked_add(length))
                .ok_or(GfxError::BadValue {
                    what: "patch post",
                    reason: "post size overflows usize".to_string(),
                })?;
            if post_end > data.len() {
                return Err(GfxError::Truncated {
                    what: "patch post data",
                    offset: pos,
                    needed: POST_OVERHEAD + length,
                    available: data.len() - pos,
                });
            }
            let pixels = data[pos + 3..post_end - 1].to_vec();
            posts.push(Post {
                top_delta: top,
                pixels,
            });
            // pos strictly increases by at least POST_OVERHEAD, so this
            // loop always terminates
            pos = post_end;
        }
        columns.push(PatchColumn { posts });
    }

    Ok(Patch {
        header: PatchHeader {
            width,
            height,
            left_offset,
            top_offset,
            column_ptrs,
        },
        columns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a synthetic patch from per-column post lists.
    /// Each post is `(topdelta, pixels)`.
    fn build_patch(
        width: i16,
        height: i16,
        left_offset: i16,
        top_offset: i16,
        columns: &[Vec<(u8, Vec<u8>)>],
    ) -> Vec<u8> {
        assert_eq!(columns.len(), width as usize);

        let mut data = Vec::new();
        data.extend_from_slice(&width.to_le_bytes());
        data.extend_from_slice(&height.to_le_bytes());
        data.extend_from_slice(&left_offset.to_le_bytes());
        data.extend_from_slice(&top_offset.to_le_bytes());

        let table_start = data.len();
        data.resize(table_start + columns.len() * COLUMN_PTR_SIZE, 0);

        for (i, posts) in columns.iter().enumerate() {
            let column_offset = data.len() as i32;
            data[table_start + i * COLUMN_PTR_SIZE..table_start + (i + 1) * COLUMN_PTR_SIZE]
                .copy_from_slice(&column_offset.to_le_bytes());
            for &(topdelta, ref pixels) in posts {
                data.push(topdelta);
                data.push(pixels.len() as u8);
                data.push(0); // unused pad
                data.extend_from_slice(pixels);
                data.push(0); // unused pad
            }
            data.push(POST_TERMINATOR);
        }
        data
    }

    #[test]
    fn decodes_a_well_formed_patch() {
        let data = build_patch(
            2,
            5,
            -3,
            7,
            &[
                vec![(0, vec![1, 2, 3]), (4, vec![9])],
                vec![], // empty column: just the terminator
            ],
        );
        let patch = decode_patch(&data).unwrap();
        assert_eq!(patch.header.width, 2);
        assert_eq!(patch.header.height, 5);
        assert_eq!(patch.header.left_offset, -3);
        assert_eq!(patch.header.top_offset, 7);
        assert_eq!(patch.header.column_ptrs.len(), 2);

        assert_eq!(patch.column_count(), 2);
        assert_eq!(patch.columns[0].post_count(), 2);
        assert_eq!(patch.columns[0].posts[0].top_delta, 0);
        assert_eq!(patch.columns[0].posts[0].pixels, vec![1, 2, 3]);
        assert_eq!(patch.columns[0].posts[1].top_delta, 4);
        assert_eq!(patch.columns[0].posts[1].pixels, vec![9]);
        assert_eq!(patch.columns[1].post_count(), 0);
        assert_eq!(patch.pixel_count(), 4);
    }

    #[test]
    fn pixel_iterator_yields_rows_in_draw_order() {
        let data = build_patch(
            2,
            10,
            0,
            0,
            &[vec![(2, vec![42, 43])], vec![(0, vec![7]), (5, vec![8, 9])]],
        );
        let patch = decode_patch(&data).unwrap();
        let pixels: Vec<Pixel> = patch.pixels().collect();
        assert_eq!(
            pixels,
            vec![
                Pixel {
                    column: 0,
                    row: 2,
                    value: 42
                },
                Pixel {
                    column: 0,
                    row: 3,
                    value: 43
                },
                Pixel {
                    column: 1,
                    row: 0,
                    value: 7
                },
                Pixel {
                    column: 1,
                    row: 5,
                    value: 8
                },
                Pixel {
                    column: 1,
                    row: 6,
                    value: 9
                },
            ]
        );
    }

    #[test]
    fn pixel_iterator_handles_empty_patch() {
        let data = build_patch(1, 1, 0, 0, &[vec![]]);
        let patch = decode_patch(&data).unwrap();
        assert_eq!(patch.pixels().count(), 0);
    }

    #[test]
    fn zero_width_is_rejected() {
        let data = build_patch(2, 5, 0, 0, &[vec![], vec![]]);
        let mut bad = data.clone();
        bad[..2].copy_from_slice(&0i16.to_le_bytes());
        assert!(matches!(
            decode_patch(&bad).unwrap_err(),
            GfxError::BadValue { .. }
        ));
    }

    #[test]
    fn negative_width_and_height_are_rejected() {
        let data = build_patch(2, 5, 0, 0, &[vec![], vec![]]);

        let mut bad = data.clone();
        bad[..2].copy_from_slice(&(-1i16).to_le_bytes());
        assert!(matches!(
            decode_patch(&bad).unwrap_err(),
            GfxError::BadValue { .. }
        ));

        let mut bad = data.clone();
        bad[2..4].copy_from_slice(&0i16.to_le_bytes());
        assert!(matches!(
            decode_patch(&bad).unwrap_err(),
            GfxError::BadValue { .. }
        ));
    }

    #[test]
    fn truncated_header_is_rejected() {
        assert!(matches!(
            decode_patch(&[0u8; PATCH_HEADER_SIZE - 1]).unwrap_err(),
            GfxError::Truncated { .. }
        ));
        assert!(decode_patch(&[]).is_err());
    }

    #[test]
    fn column_pointer_past_the_end_is_rejected() {
        let data = build_patch(1, 1, 0, 0, &[vec![(0, vec![1])]]);
        let mut bad = data.clone();
        // point column 0 way past the end of the lump
        bad[8..12].copy_from_slice(&(i32::MAX).to_le_bytes());
        assert!(matches!(
            decode_patch(&bad).unwrap_err(),
            GfxError::OutOfBounds { .. }
        ));
    }

    #[test]
    fn negative_column_pointer_is_rejected() {
        let data = build_patch(1, 1, 0, 0, &[vec![(0, vec![1])]]);
        let mut bad = data.clone();
        bad[8..12].copy_from_slice(&(-1i32).to_le_bytes());
        assert!(matches!(
            decode_patch(&bad).unwrap_err(),
            GfxError::OutOfBounds { offset: -1, .. }
        ));
    }

    #[test]
    fn column_pointer_at_exact_end_is_rejected() {
        let data = build_patch(1, 1, 0, 0, &[vec![(0, vec![1])]]);
        let mut bad = data.clone();
        // one-past-the-end pointer: a column needs at least its terminator
        let len = bad.len() as i32;
        bad[8..12].copy_from_slice(&len.to_le_bytes());
        assert!(matches!(
            decode_patch(&bad).unwrap_err(),
            GfxError::OutOfBounds { .. }
        ));
    }

    #[test]
    fn column_without_terminator_is_rejected() {
        // post claims 200 pixels at the very end of the lump: the scan
        // must run out of bytes instead of reading garbage
        let mut data = Vec::new();
        data.extend_from_slice(&1i16.to_le_bytes());
        data.extend_from_slice(&1i16.to_le_bytes());
        data.extend_from_slice(&0i16.to_le_bytes());
        data.extend_from_slice(&0i16.to_le_bytes());
        data.extend_from_slice(&12i32.to_le_bytes()); // column starts at 12
        data.push(0); // topdelta = 0 (not the terminator)
        data.push(200); // length 200
        data.push(0);
        // no pixels, no terminator
        assert!(matches!(
            decode_patch(&data).unwrap_err(),
            GfxError::Truncated { .. }
        ));
    }

    #[test]
    fn post_pixels_running_past_the_end_are_rejected() {
        // same as above but with the lump sliced shorter than the post claims
        let data = build_patch(1, 1, 0, 0, &[vec![(0, vec![1, 2, 3, 4])]]);
        let truncated = &data[..data.len() - 3];
        assert!(decode_patch(truncated).is_err());
    }

    #[test]
    fn lone_terminator_column_is_fine() {
        let data = build_patch(1, 1, 0, 0, &[vec![]]);
        let patch = decode_patch(&data).unwrap();
        assert_eq!(patch.columns[0].post_count(), 0);
    }

    #[test]
    fn zero_length_post_is_fine() {
        let data = build_patch(1, 1, 0, 0, &[vec![(3, vec![])]]);
        let patch = decode_patch(&data).unwrap();
        assert_eq!(patch.columns[0].posts[0].pixels.len(), 0);
        assert_eq!(patch.columns[0].posts[0].top_delta, 3);
    }

    #[test]
    fn tall_patch_rows_keep_raw_topdelta() {
        // rows past 255 must not wrap or panic; the renderer clips later
        let data = build_patch(1, 300, 0, 0, &[vec![(200, vec![1; 100])]]);
        let patch = decode_patch(&data).unwrap();
        let last = patch.pixels().last().unwrap();
        assert_eq!(last.row, 299);
    }

    #[test]
    fn any_corruption_is_a_result_not_a_crash() {
        let data = build_patch(
            3,
            8,
            -1,
            2,
            &[
                vec![(0, vec![1, 2, 3, 4])],
                vec![(1, vec![5]), (3, vec![6, 7])],
                vec![],
            ],
        );
        let mut state = 0xC0FF_EE11u32;
        for i in 0..data.len() {
            for _ in 0..3 {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let mut bad = data.clone();
                bad[i] ^= (state >> 9) as u8 | 0x01;
                let _ = decode_patch(&bad);
            }
        }
    }

    #[test]
    fn any_truncation_is_a_result_not_a_crash() {
        let data = build_patch(2, 4, 0, 0, &[vec![(0, vec![1, 2])], vec![(1, vec![3])]]);
        for cut in 0..data.len() {
            let _ = decode_patch(&data[..cut]);
        }
    }
}
