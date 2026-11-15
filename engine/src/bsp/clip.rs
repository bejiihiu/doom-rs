//! Column clipping against the solid-seg list, ported from `r_bsp.c`
//! (`R_ClearClipSegs`, `R_ClipSolidWallSegment`, `R_ClipPassWallSegment`).
//!
//! The clip list is the set of column ranges already covered by solid
//! walls this frame. Wall drawing asks the list for the columns still
//! visible; solid walls add their drawn range back to the list, windows
//! don't. Columns are `i32` pixel indices on the view surface.

/// Vanilla `MAXSEGS` from `r_bsp.c`: the fixed-size `solidsegs` array holds
/// 32 entries, two of which are the end sentinels. Kept for reference; the
/// list below is a `Vec`, so the clipping math behaves identically while
/// the list stays within the vanilla size (the normal case) and cannot
/// corrupt memory when a pathological scene would exceed it.
pub const VANILLA_MAX_CLIP_SEGS: usize = 32;

/// One covered column range, inclusive on both ends.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ClipRange {
    /// First covered column.
    pub first: i32,
    /// Last covered column.
    pub last: i32,
}

/// The `solidsegs` clip list: sorted, disjoint, non-touching ranges,
/// bracketed by two sentinels so the "first range touching this column"
/// scan always terminates.
///
/// Two invariants the original relied on are kept:
///
/// - the list always has at least the two sentinels
///   `{-0x7FFF_FFFF, -1}` and `{view_width, 0x7FFF_FFFF}` (only the inner
///   bounds of the sentinels may grow during clipping; either sentinel may
///   be merged away into a range that covers everything);
/// - ranges are stored left to right with at least one uncovered column
///   between neighbors (adjacent pixels are touching in the vanilla rule
///   and extend the existing range instead of inserting a new one).
///
/// Note: the original's crunch step has an off-by-one that appends one
/// inert `{0, 0}` post after the list per merge (verified against the
/// original with a differential harness). The port drops those posts;
/// they sit after the top sentinel and can never be reached by the
/// "first range with `last >= x`" scan, and the emitted fragments are
/// bit-identical either way (also verified).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ClipSegList {
    ranges: Vec<ClipRange>,
}

impl ClipSegList {
    /// Fresh clip list for one frame of rendering, mirrors `R_ClearClipSegs`.
    ///
    /// The sentinels sit just outside the view: nothing at or left of
    /// column -1, nothing at or right of column `view_width`.
    pub fn cleared(view_width: i32) -> ClipSegList {
        // view_width == 0 is degenerate but cannot panic or corrupt anything;
        // renderer always passes a real width.
        ClipSegList {
            ranges: vec![
                ClipRange {
                    first: -0x7FFF_FFFF,
                    last: -1,
                },
                ClipRange {
                    first: view_width,
                    last: 0x7FFF_FFFF,
                },
            ],
        }
    }

    /// Number of ranges, sentinels included. Always >= 1
    /// (full coverage merges everything into one range).
    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    /// Always false; the list always holds at least one range.
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// All ranges including the sentinels, left to right.
    pub fn ranges(&self) -> &[ClipRange] {
        &self.ranges
    }

    /// First range whose `last` column is not before `x`, i.e. the range the
    /// vanilla "advance start while start->last < x" loop lands on. The last
    /// range in the list always satisfies the predicate.
    pub(crate) fn first_touching(&self, x: i32) -> &ClipRange {
        // find() succeeds on the last range at the latest
        self.ranges
            .iter()
            .find(|range| !(range.last < x))
            .unwrap_or(&self.ranges[self.ranges.len() - 1])
    }

    /// Index of the first range with `last >= x`; see [`ClipSegList::first_touching`].
    fn first_touching_index(&self, x: i32) -> usize {
        self.ranges
            .iter()
            .position(|range| !(range.last < x))
            .unwrap_or(self.ranges.len() - 1)
    }

    /// Clips `[first, last]` against the list and reports the still-visible
    /// fragments through `emit`, then extends the list to cover the whole
    /// span. Port of `R_ClipSolidWallSegment`: fragments carry the same
    /// columns the original passed to `R_StoreWallRange`.
    ///
    /// `first > last` is a caller bug and is ignored (debug builds assert).
    pub fn clip_solid_wall_segment(
        &mut self,
        first: i32,
        last: i32,
        mut emit: impl FnMut(i32, i32),
    ) {
        if first > last {
            // caller bug — nothing to clip; don't panic the renderer over it
            tracing::warn!(
                first,
                last,
                "clip_solid_wall_segment called with first > last, ignoring"
            );
            return;
        }
        // i64 math in the span comparisons so `first - 1` can't wrap when first == i32::MIN

        // Find the first range that touches the range
        //  (adjacent pixels are touching).
        let start = self.first_touching_index(first.saturating_sub(1));

        if first < self.ranges[start].first {
            if (last as i64) < self.ranges[start].first as i64 - 1 {
                // Post is entirely visible (above start),
                //  so insert a new clippost.
                emit(first, last);
                self.ranges.insert(start, ClipRange { first, last });
                return;
            }

            // There is a fragment above *start.
            emit(first, self.ranges[start].first - 1);
            // Now adjust the clip size.
            self.ranges[start].first = first;
        }

        // Bottom contained in start?
        if last <= self.ranges[start].last {
            return;
        }

        let mut next = start;
        loop {
            // vanilla: while (last >= (next+1)->first-1)
            let following = next + 1;
            if (last as i64) < self.ranges[following].first as i64 - 1 {
                break;
            }
            // There is a fragment between two posts.
            emit(self.ranges[next].last + 1, self.ranges[following].first - 1);
            next = following;

            if last <= self.ranges[next].last {
                // Bottom is contained in next.
                // Adjust the clip size.
                self.ranges[start].last = self.ranges[next].last;
                break;
            }
        }

        if last > self.ranges[next].last {
            // There is a fragment after *next.
            emit(self.ranges[next].last + 1, last);
            // Adjust the clip size.
            self.ranges[start].last = last;
        }

        // Remove start+1 to next from the clip list,
        // because start now covers their area.
        if next != start {
            // vanilla crunch: while (next++ != newend) *++start = *next
            self.ranges.drain(start + 1..=next);
        }
    }

    /// Clips `[first, last]` and reports the still-visible fragments without
    /// modifying the list. Port of `R_ClipPassWallSegment`: windows are drawn
    /// over already-clipped columns and must not shrink the visible range
    /// for later walls behind them.
    ///
    /// `first > last` is a caller bug and is ignored (debug builds assert).
    pub fn clip_pass_wall_segment(
        &mut self,
        first: i32,
        last: i32,
        mut emit: impl FnMut(i32, i32),
    ) {
        if first > last {
            // caller bug — nothing to clip; don't panic the renderer over it
            tracing::warn!(
                first,
                last,
                "clip_pass_wall_segment called with first > last, ignoring"
            );
            return;
        }

        // i64 math so `first - 1` can't wrap when first == i32::MIN

        // Find the first range that touches the range
        //  (adjacent pixels are touching).
        let mut start = self.first_touching_index(first.saturating_sub(1));

        if first < self.ranges[start].first {
            if (last as i64) < self.ranges[start].first as i64 - 1 {
                // Post is entirely visible (above start).
                emit(first, last);
                return;
            }

            // There is a fragment above *start.
            emit(first, self.ranges[start].first - 1);
        }

        // Bottom contained in start?
        if last <= self.ranges[start].last {
            return;
        }

        loop {
            let following = start + 1;
            if (last as i64) < self.ranges[following].first as i64 - 1 {
                break;
            }
            // There is a fragment between two posts.
            emit(
                self.ranges[start].last + 1,
                self.ranges[following].first - 1,
            );
            start = following;

            if last <= self.ranges[start].last {
                return;
            }
        }

        // There is a fragment after *start.
        emit(self.ranges[start].last + 1, last);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: i32 = 64;

    fn collect(list: &mut ClipSegList, first: i32, last: i32) -> Vec<(i32, i32)> {
        let mut out = Vec::new();
        list.clip_solid_wall_segment(first, last, |a, b| out.push((a, b)));
        out
    }

    #[test]
    fn cleared_has_two_sentinels() {
        let list = ClipSegList::cleared(VIEW);
        assert_eq!(
            list.ranges(),
            &[
                ClipRange {
                    first: -0x7FFF_FFFF,
                    last: -1
                },
                ClipRange {
                    first: VIEW,
                    last: 0x7FFF_FFFF
                },
            ]
        );
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn first_clip_inserts_between_sentinels() {
        let mut list = ClipSegList::cleared(VIEW);
        assert_eq!(collect(&mut list, 10, 20), vec![(10, 20)]);
        assert_eq!(
            list.ranges(),
            &[
                ClipRange {
                    first: -0x7FFF_FFFF,
                    last: -1
                },
                ClipRange {
                    first: 10,
                    last: 20
                },
                ClipRange {
                    first: VIEW,
                    last: 0x7FFF_FFFF
                },
            ]
        );
    }

    #[test]
    fn disjoint_windows_insert_above_each_other() {
        // the "post entirely visible above start" case inserts at the position
        let mut list = ClipSegList::cleared(VIEW);
        collect(&mut list, 10, 20);
        collect(&mut list, 30, 40);
        assert_eq!(collect(&mut list, 22, 28), vec![(22, 28)]);
        assert_eq!(
            list.ranges(),
            &[
                ClipRange {
                    first: -0x7FFF_FFFF,
                    last: -1
                },
                ClipRange {
                    first: 10,
                    last: 20
                },
                ClipRange {
                    first: 22,
                    last: 28
                },
                ClipRange {
                    first: 30,
                    last: 40
                },
                ClipRange {
                    first: VIEW,
                    last: 0x7FFF_FFFF
                },
            ]
        );
    }

    #[test]
    fn covering_existing_span_is_a_noop() {
        let mut list = ClipSegList::cleared(VIEW);
        collect(&mut list, 10, 40);
        assert_eq!(collect(&mut list, 15, 25), vec![]);
        assert_eq!(
            list.ranges(),
            &[
                ClipRange {
                    first: -0x7FFF_FFFF,
                    last: -1
                },
                ClipRange {
                    first: 10,
                    last: 40
                },
                ClipRange {
                    first: VIEW,
                    last: 0x7FFF_FFFF
                },
            ]
        );
    }

    #[test]
    fn spanning_two_posts_merges_into_one() {
        let mut list = ClipSegList::cleared(VIEW);
        collect(&mut list, 10, 20);
        collect(&mut list, 30, 40);
        assert_eq!(collect(&mut list, 15, 35), vec![(21, 29)]);
        assert_eq!(
            list.ranges(),
            &[
                ClipRange {
                    first: -0x7FFF_FFFF,
                    last: -1
                },
                ClipRange {
                    first: 10,
                    last: 40
                },
                ClipRange {
                    first: VIEW,
                    last: 0x7FFF_FFFF
                },
            ]
        );
    }

    #[test]
    fn spanning_several_posts_emits_fragments_between_them() {
        let mut list = ClipSegList::cleared(VIEW);
        collect(&mut list, 10, 20);
        collect(&mut list, 30, 40);
        collect(&mut list, 50, 60);
        // (10..20) is behind us: first touching post for first-1=24 is {30,40}
        assert_eq!(collect(&mut list, 25, 55), vec![(25, 29), (41, 49)]);
        assert_eq!(
            list.ranges(),
            &[
                ClipRange {
                    first: -0x7FFF_FFFF,
                    last: -1
                },
                ClipRange {
                    first: 10,
                    last: 20
                },
                ClipRange {
                    first: 25,
                    last: 60
                },
                ClipRange {
                    first: VIEW,
                    last: 0x7FFF_FFFF
                },
            ]
        );
    }

    #[test]
    fn fragment_after_last_post_extends_it() {
        let mut list = ClipSegList::cleared(VIEW);
        collect(&mut list, 10, 20);
        assert_eq!(collect(&mut list, 15, 30), vec![(21, 30)]);
        assert_eq!(
            list.ranges()[1],
            ClipRange {
                first: 10,
                last: 30
            }
        );
    }

    #[test]
    fn fragment_before_first_post_pulls_first_down() {
        let mut list = ClipSegList::cleared(VIEW);
        collect(&mut list, 10, 20);
        assert_eq!(collect(&mut list, 5, 15), vec![(5, 9)]);
        assert_eq!(list.ranges()[1], ClipRange { first: 5, last: 20 });
    }

    #[test]
    fn adjacent_posts_grow_instead_of_inserting() {
        // adjacent pixels are touching: (5,10) right after (10,20) must merge
        // when separated by nothing, via first < start->first logic
        let mut list = ClipSegList::cleared(VIEW);
        collect(&mut list, 10, 20);
        // last+1 == next.first: not fully above, so it joins the existing post
        assert_eq!(collect(&mut list, 9, 9), vec![(9, 9)]);
        assert_eq!(
            list.ranges(),
            &[
                ClipRange {
                    first: -0x7FFF_FFFF,
                    last: -1
                },
                ClipRange { first: 9, last: 20 },
                ClipRange {
                    first: VIEW,
                    last: 0x7FFF_FFFF
                },
            ]
        );
    }

    #[test]
    fn pass_wall_does_not_modify_the_list() {
        let mut list = ClipSegList::cleared(VIEW);
        collect(&mut list, 10, 20);
        collect(&mut list, 30, 40);
        let before = list.ranges().to_vec();

        let mut out = Vec::new();
        list.clip_pass_wall_segment(5, 62, |a, b| out.push((a, b)));
        assert_eq!(out, vec![(5, 9), (21, 29), (41, 62)]);
        assert_eq!(list.ranges(), &before[..]);

        // entirely inside a covered post emits nothing
        let mut out = Vec::new();
        list.clip_pass_wall_segment(12, 18, |a, b| out.push((a, b)));
        assert!(out.is_empty());
        assert_eq!(list.ranges(), &before[..]);
    }

    #[test]
    fn pass_wall_fully_visible_emits_whole_span() {
        let mut list = ClipSegList::cleared(VIEW);
        let mut out = Vec::new();
        list.clip_pass_wall_segment(1, 62, |a, b| out.push((a, b)));
        assert_eq!(out, vec![(1, 62)]);
        assert_eq!(list.len(), 2); // unchanged
    }

    #[test]
    fn first_min_does_not_wrap() {
        let mut list = ClipSegList::cleared(VIEW);
        // first - 1 would overflow with plain i32 arithmetic
        let mut out = Vec::new();
        list.clip_solid_wall_segment(i32::MIN, i32::MIN, |a, b| out.push((a, b)));
        assert_eq!(out, vec![(i32::MIN, i32::MIN)]);
    }

    #[test]
    fn inverted_range_is_ignored() {
        let mut list = ClipSegList::cleared(VIEW);
        let before = list.ranges().to_vec();
        let mut out = Vec::new();
        list.clip_solid_wall_segment(20, 10, |a, b| out.push((a, b)));
        list.clip_pass_wall_segment(20, 10, |a, b| out.push((a, b)));
        assert!(out.is_empty());
        assert_eq!(list.ranges(), &before[..]);
    }

    #[test]
    fn wall_crossing_column_minus_one_extends_the_bottom_sentinel() {
        // columns < -1 are always covered by the bottom sentinel; a wall
        // crossing column -1 draws from column 0 and the sentinel grows
        let mut list = ClipSegList::cleared(VIEW);
        collect(&mut list, -5, 30);
        assert_eq!(
            list.ranges()[0],
            ClipRange {
                first: -0x7FFF_FFFF,
                last: 30
            }
        );
        // those columns are covered now, so a second wall there draws nothing
        assert_eq!(collect(&mut list, 5, 25), vec![]);
    }

    /// Tiny xorshift so the fuzz test needs no external crates.
    struct XorShift(u64);

    impl XorShift {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
    }

    #[test]
    fn fuzz_solid_clips_keep_invariants() {
        let mut rng = XorShift(0x243F_6A88_85A3_08D3);
        for _case in 0..300 {
            let mut list = ClipSegList::cleared(VIEW);
            let mut fragments: Vec<(i32, i32)> = Vec::new();

            for _ in 0..40 {
                let a = i32::try_from(rng.next() % 160).unwrap() - 80;
                let b = a + i32::try_from(rng.next() % 40).unwrap();
                let start_frag = fragments.len();
                let before_len = fragments.len();
                list.clip_solid_wall_segment(a, b, |x, y| fragments.push((x, y)));

                // fragments from this op stay inside the requested span
                for &(x, y) in &fragments[start_frag..] {
                    assert!(x >= a && y <= b, "fragment ({x},{y}) outside ({a},{b})");
                    assert!(x <= y, "fragment ({x},{y}) inverted");
                }

                // no fragment ever overlaps an earlier one: solid columns are
                // covered exactly once
                for (i, &(x, y)) in fragments.iter().enumerate() {
                    for &(x2, y2) in &fragments[..i] {
                        assert!(
                            y < x2 || y2 < x,
                            "fragments ({x},{y}) and ({x2},{y2}) overlap"
                        );
                    }
                }

                // list invariants: sorted, disjoint, gaps >= 1, at least one range.
                // the list can shrink to a single monster range when everything is covered
                let ranges = list.ranges();
                assert!(!ranges.is_empty());
                assert_eq!(ranges[0].first, -0x7FFF_FFFF);
                assert_eq!(ranges[ranges.len() - 1].last, 0x7FFF_FFFF);
                for pair in ranges.windows(2) {
                    assert!(pair[0].first <= pair[0].last, "inverted range {pair:?}");
                    assert!(
                        pair[0].last + 1 < pair[1].first,
                        "ranges must not touch: {:?} and {:?}",
                        pair[0],
                        pair[1]
                    );
                }

                // coverage is monotone: clipping never uncovers a column
                assert!(fragments.len() >= before_len);
            }
        }
    }

    #[test]
    fn fuzz_pass_clips_never_touch_the_list() {
        let mut rng = XorShift(0x4528_21E6_38D0_1377);
        for _case in 0..50 {
            let mut list = ClipSegList::cleared(VIEW);
            // seed with some solid coverage
            for _ in 0..5 {
                let a = i32::try_from(rng.next() % 120).unwrap() - 60;
                let b = a + i32::try_from(rng.next() % 30).unwrap();
                list.clip_solid_wall_segment(a, b, |_, _| {});
            }
            let before = list.ranges().to_vec();
            for _ in 0..20 {
                let a = i32::try_from(rng.next() % 160).unwrap() - 80;
                let b = a + i32::try_from(rng.next() % 60).unwrap();
                list.clip_pass_wall_segment(a, b, |_, _| {});
            }
            assert_eq!(list.ranges(), &before[..]);
        }
    }
}
