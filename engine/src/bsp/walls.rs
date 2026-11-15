//! Wall clipping against the view, ported from `r_bsp.c`:
//! `R_AddLine`, `R_CheckBBox`.
//!
//! [`add_line`] classifies a seg (back face? off screen? single sided /
//! closed door / window / trigger line), projects its endpoints to screen
//! columns through [`ViewTables`], and feeds the visible column span to the
//! clip list. [`check_bbox`] answers whether a BSP node child's bounding
//! box might be visible at all — the front-to-back culling the traversal
//! uses to skip whole subtrees.
//!
//! Neither function draws anything. Both take `emit` closures / the clip
//! list so the renderer can plug in column output later.

use crate::map::Sector;
use crate::math::Point;
use crate::tables::Angle;

use super::clip::ClipSegList;
use super::view::{ViewState, ViewTables, clamp_span_to_view, fine_index, point_to_angle};

/// What [`add_line`] decided to do with the seg.
///
/// `Solid` and `Window` carry `rw_angle1` — the view-absolute angle of
/// `v1`, stored as a global (`rw_angle1`) in the original and needed later
/// for texture scale computations.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AddLineOutcome {
    /// `span >= ANG180` — back-face, rejected before any clipping.
    BackFace,
    /// The whole seg lies beyond the left or right edge of the view.
    OutsideView,
    /// Both endpoints project to the same column; not a pixel.
    SubPixel,
    /// Identical sectors on both sides, same light, no middle texture:
    /// a trigger line, never drawn.
    TriggerLine,
    /// Single sided or closed door; the span went through the solid clipper.
    Solid(Angle),
    /// Window or a middle texture: the span went through the pass-through
    /// clipper without touching the clip list.
    Window(Angle),
}

/// Classifies and clips one seg, mirroring `R_AddLine`. `v1`/`v2` are the
/// seg's world vertices; `front`/`back` the adjacent sectors (`back` is
/// `None` for single sided lines); `has_midtexture` reports whether the
/// seg's sidedef has a middle texture (vanilla: `midtexture != 0`, i.e.
/// the texture name does not start with `-`).
///
/// Visible column fragments, as `(first, last_inclusive)`, go to `emit`.
/// The clip list gains coverage only on the [`AddLineOutcome::Solid`] path.
///
/// `R_AddLine` calls the two clip routines with `x1, x2-1` (last inclusive);
/// this port keeps that convention.
#[allow(clippy::too_many_arguments)]
pub fn add_line(
    view: &ViewState,
    tables: &ViewTables,
    clip: &mut ClipSegList,
    v1: Point,
    v2: Point,
    front: &Sector,
    back: Option<&Sector>,
    has_midtexture: bool,
    emit: &mut impl FnMut(i32, i32),
) -> AddLineOutcome {
    // angle1/angle2 are view-ABSOLUTE here, like the globals in the original
    let angle1 = point_to_angle(view, v1.x, v1.y);
    let angle2 = point_to_angle(view, v2.x, v2.y);

    // OPTIMIZE: quickly reject orthogonal back sides. (backface culling)
    let span = angle1 - angle2;
    if span.to_raw() >= Angle::ANG180.to_raw() {
        return AddLineOutcome::BackFace;
    }

    // Global angle needed by segcalc.
    let rw_angle1 = angle1;
    let angle1 = angle1 - view.angle;
    let angle2 = angle2 - view.angle;

    // Clip to view edges.
    // OPTIMIZE: make constant out of 2*clipangle (FIELDOFVIEW).
    let (angle1, angle2) = match clamp_span_to_view(angle1, angle2, tables.clip_angle, span) {
        Some(clamped) => clamped,
        None => return AddLineOutcome::OutsideView,
    };

    // The seg is in the view range,
    // but not necessarily visible.
    let x1 = tables.view_angle_to_x[fine_index(angle1)];
    let x2 = tables.view_angle_to_x[fine_index(angle2)];

    // Does not cross a pixel?
    if x1 == x2 {
        return AddLineOutcome::SubPixel;
    }

    // Single sided line? (or a closed door — back sector fully hidden)
    let back = match back {
        None => {
            clip.clip_solid_wall_segment(x1, x2 - 1, emit);
            return AddLineOutcome::Solid(rw_angle1);
        }
        Some(back) => back,
    };

    // Closed door.
    if back.ceiling_height <= front.floor_height || back.floor_height >= front.ceiling_height {
        clip.clip_solid_wall_segment(x1, x2 - 1, emit);
        return AddLineOutcome::Solid(rw_angle1);
    }

    // Window.
    if back.ceiling_height != front.ceiling_height || back.floor_height != front.floor_height {
        clip.clip_pass_wall_segment(x1, x2 - 1, emit);
        return AddLineOutcome::Window(rw_angle1);
    }

    // Reject empty lines used for triggers
    //  and special events.
    // Identical floor and ceiling on both sides,
    // identical light levels on both sides,
    // and no middle texture.
    // (the original compares flat lump indices; equal names play the same role here)
    if back.ceiling_pic == front.ceiling_pic
        && back.floor_pic == front.floor_pic
        && back.light_level == front.light_level
        && !has_midtexture
    {
        return AddLineOutcome::TriggerLine;
    }

    // As in original: falls through to clippass.
    clip.clip_pass_wall_segment(x1, x2 - 1, emit);
    AddLineOutcome::Window(rw_angle1)
}

/// `R_CheckBBox`: may some part of the bbox be visible? `bbox` is the
/// node's child bbox in `[top, bottom, left, right]` order (the on-disk
/// `mapnode_t` order, matching vanilla's `m_bbox.h` enum indices
/// `BOXTOP=0, BOXBOTTOM=1, BOXLEFT=2, BOXRIGHT=3`).
///
/// Returns true when the bbox might be visible — the traversal uses this
/// to decide whether to descend into a back-side subtree. Reading the
/// exact semantics from `r_bsp.c`:
///
/// - viewer inside the box returns true;
/// - a span of at least 180 degrees (box straddling the view) returns true;
/// - a span fully outside the frustum edges returns false;
/// - otherwise the box's columns are tested against the clip list and
///   false means "fully covered by solid walls already drawn".
pub fn check_bbox(
    view: &ViewState,
    tables: &ViewTables,
    clip: &ClipSegList,
    bbox: &[crate::fixed::Fixed; 4],
) -> bool {
    const BOXTOP: usize = 0;
    const BOXBOTTOM: usize = 1;
    const BOXLEFT: usize = 2;
    const BOXRIGHT: usize = 3;

    // Find the corners of the box
    // that define the edges from current viewpoint.
    let boxx = if view.x <= bbox[BOXLEFT] {
        0
    } else if view.x < bbox[BOXRIGHT] {
        1
    } else {
        2
    };

    let boxy = if view.y >= bbox[BOXTOP] {
        0
    } else if view.y > bbox[BOXBOTTOM] {
        1
    } else {
        2
    };

    // corner selection, straight from checkcoord[12][4]:
    //   boxpos 0 -> {3,0,2,1}   boxpos 1 -> {3,0,2,0}   boxpos 2 -> {3,1,2,0}
    //   boxpos 4 -> {2,0,2,1}   boxpos 5 is the inside case (return true)
    //   boxpos 6 -> {3,1,3,0}   boxpos 8 -> {2,0,3,1}
    //   boxpos 9 -> {2,1,3,1}   boxpos 10 -> {2,1,3,0}
    // (boxpos 3 and 7 are unreachable: boxx only reaches 0..=2)
    let (x1, y1, x2, y2) = match (boxy, boxx) {
        (1, 1) => return true,                          // viewer inside the box
        (0, 0) => (bbox[3], bbox[0], bbox[2], bbox[1]), // {3,0,2,1}
        (0, 1) => (bbox[3], bbox[0], bbox[2], bbox[0]), // {3,0,2,0}
        (0, 2) => (bbox[3], bbox[1], bbox[2], bbox[0]), // {3,1,2,0}
        (1, 0) => (bbox[2], bbox[0], bbox[2], bbox[1]), // {2,0,2,1}
        (1, 2) => (bbox[3], bbox[1], bbox[3], bbox[0]), // {3,1,3,0}
        (2, 0) => (bbox[2], bbox[0], bbox[3], bbox[1]), // {2,0,3,1}
        (2, 1) => (bbox[2], bbox[1], bbox[3], bbox[1]), // {2,1,3,1}
        (2, 2) => (bbox[2], bbox[1], bbox[3], bbox[0]), // {2,1,3,0}
        _ => unreachable!("boxy/boxx are 0..=2 by construction"),
    };

    // check clip list for an open space
    let angle1 = point_to_angle(view, x1, y1) - view.angle;
    let angle2 = point_to_angle(view, x2, y2) - view.angle;

    let span = angle1 - angle2;

    // Sitting on a line?
    if span.to_raw() >= Angle::ANG180.to_raw() {
        return true;
    }

    let (angle1, angle2) = match clamp_span_to_view(angle1, angle2, tables.clip_angle, span) {
        Some(clamped) => clamped,
        None => return false, // totally off the left edge
    };

    // Find the first clippost that touches the source post
    //  (adjacent pixels are touching).
    let sx1 = tables.view_angle_to_x[fine_index(angle1)];
    let sx2 = tables.view_angle_to_x[fine_index(angle2)];

    // Does not cross a pixel.
    if sx1 == sx2 {
        return false;
    }
    let sx2 = sx2 - 1;

    let start = clip.first_touching(sx2);

    if sx1 >= start.first && sx2 <= start.last {
        // The clippost contains the new span.
        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixed::Fixed;
    use crate::tables::Angle;

    fn sector(floor: i32, ceil: i32) -> Sector {
        Sector {
            floor_height: Fixed::from_int(floor),
            ceiling_height: Fixed::from_int(ceil),
            floor_pic: *b"FLAT1\0\0\0",
            ceiling_pic: *b"FLAT1\0\0\0",
            light_level: 192,
            special: 0,
            tag: 0,
        }
    }

    fn tables() -> ViewTables {
        ViewTables::new(64, Fixed::from_int(32))
    }

    fn v(x: i32, y: i32) -> Point {
        Point::new(Fixed::from_int(x), Fixed::from_int(y))
    }

    fn view() -> ViewState {
        ViewState::new(Fixed::ZERO, Fixed::ZERO, Angle::ZERO)
    }

    #[test]
    fn front_facing_wall_is_solid() {
        let t = tables();
        let mut clip = ClipSegList::cleared(64);
        let mut frags = Vec::new();
        let front = sector(0, 128);
        let outcome = add_line(
            &view(),
            &t,
            &mut clip,
            v(64, 16),
            v(64, -16),
            &front,
            None,
            false,
            &mut |a, b| frags.push((a, b)),
        );
        assert!(matches!(outcome, AddLineOutcome::Solid(_)));
        assert!(!frags.is_empty());
        // all fragments inside the view
        for &(a, b) in &frags {
            assert!(a >= 0 && b < 64 && a <= b);
        }
        // solid wall covered columns in the clip list
        assert!(clip.len() > 2);
    }

    #[test]
    fn back_facing_wall_is_culled() {
        let t = tables();
        let mut clip = ClipSegList::cleared(64);
        let mut frags = Vec::new();
        let front = sector(0, 128);
        let outcome = add_line(
            &view(),
            &t,
            &mut clip,
            v(64, -16),
            v(64, 16),
            &front,
            None,
            false,
            &mut |a, b| frags.push((a, b)),
        );
        assert_eq!(outcome, AddLineOutcome::BackFace);
        assert!(frags.is_empty());
        assert_eq!(clip.len(), 2); // untouched
    }

    #[test]
    fn wrap_around_span_across_zero_is_back_face() {
        let t = tables();
        let mut clip = ClipSegList::cleared(64);
        let front = sector(0, 128);
        // v1 at ~350deg (south-east), v2 at ~10deg (north-east): the seg's
        // span wraps the zero line and reads as > ANG180, so it is culled
        let outcome = add_line(
            &view(),
            &t,
            &mut clip,
            v(100, -17),
            v(100, 17),
            &front,
            None,
            false,
            &mut |_, _| panic!("wrap-around back face must not emit"),
        );
        assert_eq!(outcome, AddLineOutcome::BackFace);
    }

    #[test]
    fn degenerate_span_of_180_degrees_is_back_face() {
        let t = tables();
        let mut clip = ClipSegList::cleared(64);
        let front = sector(0, 128);
        // v1 east, v2 west: span wraps exactly to ANG180
        let outcome = add_line(
            &view(),
            &t,
            &mut clip,
            v(64, 0),
            v(-64, 0),
            &front,
            None,
            false,
            &mut |_, _| panic!("back-facing seg must not emit"),
        );
        assert_eq!(outcome, AddLineOutcome::BackFace);
    }

    #[test]
    fn wall_totally_off_left_edge_is_outside_view() {
        let t = tables();
        let mut clip = ClipSegList::cleared(64);
        let front = sector(0, 128);
        // both endpoints beyond the left edge of the 90 degree FOV
        let outcome = add_line(
            &view(),
            &t,
            &mut clip,
            v(10, 100),
            v(50, 80),
            &front,
            None,
            false,
            &mut |_, _| panic!("off-screen seg must not emit"),
        );
        assert_eq!(outcome, AddLineOutcome::OutsideView);
    }

    #[test]
    fn wall_totally_off_right_edge_is_outside_view() {
        let t = tables();
        let mut clip = ClipSegList::cleared(64);
        let front = sector(0, 128);
        let outcome = add_line(
            &view(),
            &t,
            &mut clip,
            v(50, -80),
            v(10, -100),
            &front,
            None,
            false,
            &mut |_, _| panic!("off-screen seg must not emit"),
        );
        assert_eq!(outcome, AddLineOutcome::OutsideView);
    }

    #[test]
    fn wall_straddling_left_edge_gets_clamped_and_drawn() {
        let t = tables();
        let mut clip = ClipSegList::cleared(64);
        let mut frags = Vec::new();
        let front = sector(0, 128);
        // angle1 beyond clipangle, angle2 inside: clamped, not rejected
        let outcome = add_line(
            &view(),
            &t,
            &mut clip,
            v(64, 96),
            v(64, 16),
            &front,
            None,
            false,
            &mut |a, b| frags.push((a, b)),
        );
        assert!(matches!(outcome, AddLineOutcome::Solid(_)));
        assert!(!frags.is_empty());
    }

    #[test]
    fn narrow_wall_is_subpixel() {
        let t = tables();
        let mut clip = ClipSegList::cleared(64);
        let front = sector(0, 128);
        // faraway wall one map unit wide: endpoints project to one column
        let outcome = add_line(
            &view(),
            &t,
            &mut clip,
            v(4096, 0),
            v(4096, 1),
            &front,
            None,
            false,
            &mut |_, _| panic!("subpixel seg must not emit"),
        );
        assert_eq!(outcome, AddLineOutcome::SubPixel);
    }

    #[test]
    fn two_sided_identical_without_midtexture_is_trigger() {
        let t = tables();
        let mut clip = ClipSegList::cleared(64);
        let front = sector(0, 128);
        let back = front.clone();
        let mut frags = Vec::new();
        let outcome = add_line(
            &view(),
            &t,
            &mut clip,
            v(64, 16),
            v(64, -16),
            &front,
            Some(&back),
            false,
            &mut |a, b| frags.push((a, b)),
        );
        assert_eq!(outcome, AddLineOutcome::TriggerLine);
        assert!(frags.is_empty());
        assert_eq!(clip.len(), 2);
    }

    #[test]
    fn two_sided_identical_with_midtexture_is_window() {
        let t = tables();
        let mut clip = ClipSegList::cleared(64);
        let front = sector(0, 128);
        let back = front.clone();
        let outcome = add_line(
            &view(),
            &t,
            &mut clip,
            v(64, 16),
            v(64, -16),
            &front,
            Some(&back),
            true,
            &mut |_, _| {},
        );
        assert!(matches!(outcome, AddLineOutcome::Window(_)));
        assert_eq!(clip.len(), 2); // pass-through: list untouched
    }

    #[test]
    fn two_sided_different_ceiling_is_window() {
        let t = tables();
        let mut clip = ClipSegList::cleared(64);
        let front = sector(0, 128);
        let back = sector(0, 96);
        let outcome = add_line(
            &view(),
            &t,
            &mut clip,
            v(64, 16),
            v(64, -16),
            &front,
            Some(&back),
            false,
            &mut |_, _| {},
        );
        assert!(matches!(outcome, AddLineOutcome::Window(_)));
    }

    #[test]
    fn two_sided_different_light_is_window_not_trigger() {
        let t = tables();
        let mut clip = ClipSegList::cleared(64);
        let front = sector(0, 128);
        let mut back = front.clone();
        back.light_level = 96;
        let outcome = add_line(
            &view(),
            &t,
            &mut clip,
            v(64, 16),
            v(64, -16),
            &front,
            Some(&back),
            false,
            &mut |_, _| {},
        );
        assert!(matches!(outcome, AddLineOutcome::Window(_)));
    }

    #[test]
    fn closed_door_ceiling_below_front_floor_is_solid() {
        let t = tables();
        let mut clip = ClipSegList::cleared(64);
        let front = sector(128, 256);
        let back = sector(0, 64); // back ceiling below front floor -> closed
        let mut frags = Vec::new();
        let outcome = add_line(
            &view(),
            &t,
            &mut clip,
            v(64, 16),
            v(64, -16),
            &front,
            Some(&back),
            false,
            &mut |a, b| frags.push((a, b)),
        );
        assert!(matches!(outcome, AddLineOutcome::Solid(_)));
        assert!(!frags.is_empty());
        assert!(clip.len() > 2);
    }

    #[test]
    fn closed_door_back_floor_above_front_ceiling_is_solid() {
        let t = tables();
        let mut clip = ClipSegList::cleared(64);
        let front = sector(0, 128);
        let back = sector(200, 300); // back floor above front ceiling -> closed
        let outcome = add_line(
            &view(),
            &t,
            &mut clip,
            v(64, 16),
            v(64, -16),
            &front,
            Some(&back),
            false,
            &mut |_, _| {},
        );
        assert!(matches!(outcome, AddLineOutcome::Solid(_)));
    }

    #[test]
    fn solid_fragments_cover_columns_monotonically() {
        let t = tables();
        let mut clip = ClipSegList::cleared(64);
        let front = sector(0, 128);
        let mut frags = Vec::new();
        add_line(
            &view(),
            &t,
            &mut clip,
            v(64, 32),
            v(64, -32),
            &front,
            None,
            false,
            &mut |a, b| frags.push((a, b)),
        );
        // a single solid wall emits one fragment, columns inside the seg span
        assert_eq!(frags.len(), 1);
        let (a, b) = frags[0];
        assert!(a < b);
        // clipping the same span again emits nothing: the columns are covered
        let mut frags2 = Vec::new();
        add_line(
            &view(),
            &t,
            &mut clip,
            v(64, 32),
            v(64, -32),
            &front,
            None,
            false,
            &mut |a, b| frags2.push((a, b)),
        );
        assert!(frags2.is_empty());
    }

    #[test]
    fn line_partially_behind_another_emits_only_the_visible_bit() {
        let t = tables();
        let mut clip = ClipSegList::cleared(64);
        let front = sector(0, 128);
        // first wall covers the middle of the view
        add_line(
            &view(),
            &t,
            &mut clip,
            v(64, 16),
            v(64, -16),
            &front,
            None,
            false,
            &mut |_, _| {},
        );
        // wider wall behind it: fragments must skip the covered middle
        let mut frags = Vec::new();
        add_line(
            &view(),
            &t,
            &mut clip,
            v(64, 48),
            v(64, -48),
            &front,
            None,
            false,
            &mut |a, b| frags.push((a, b)),
        );
        assert_eq!(frags.len(), 2);
        // fragments flank the covered middle with at least a one column gap
        assert!(frags[0].1 < frags[1].0);
    }

    // ------------------------------------------------------------- check_bbox

    fn bbox_from(left: i32, right: i32, bottom: i32, top: i32) -> [Fixed; 4] {
        // [top, bottom, left, right] — node bbox order
        [
            Fixed::from_int(top),
            Fixed::from_int(bottom),
            Fixed::from_int(left),
            Fixed::from_int(right),
        ]
    }

    #[test]
    fn bbox_containing_viewpoint_is_visible() {
        let t = tables();
        let clip = ClipSegList::cleared(64);
        let bbox = bbox_from(-100, 100, -100, 100);
        assert!(check_bbox(&view(), &t, &clip, &bbox));
    }

    #[test]
    fn bbox_ahead_in_open_view_is_visible() {
        let t = tables();
        let clip = ClipSegList::cleared(64);
        // tall, narrow slab straight ahead, beyond the walls
        let bbox = bbox_from(500, 700, -100, 100);
        assert!(check_bbox(&view(), &t, &clip, &bbox));
    }

    #[test]
    fn bbox_totally_off_left_edge_is_culled() {
        let t = tables();
        let clip = ClipSegList::cleared(64);
        // both corners at angles > clipangle
        let bbox = bbox_from(10, 50, 80, 100);
        assert!(!check_bbox(&view(), &t, &clip, &bbox));
    }

    #[test]
    fn bbox_totally_off_right_edge_is_culled() {
        let t = tables();
        let clip = ClipSegList::cleared(64);
        // both corners at angles < -clipangle
        let bbox = bbox_from(10, 50, -100, -80);
        assert!(!check_bbox(&view(), &t, &clip, &bbox));
    }

    #[test]
    fn bbox_covered_by_solid_wall_is_culled() {
        let t = tables();
        let mut clip = ClipSegList::cleared(64);
        // cover the whole view with a solid wall
        clip.clip_solid_wall_segment(-1000, 2000, |_, _| {});
        let bbox = bbox_from(500, 700, -100, 100);
        assert!(!check_bbox(&view(), &t, &clip, &bbox));
    }

    #[test]
    fn bbox_right_half_covered_is_still_visible() {
        let t = tables();
        let mut clip = ClipSegList::cleared(64);
        // cover the right half of the view; the slab's left part stays visible
        clip.clip_solid_wall_segment(32, 63, |_, _| {});
        let bbox = bbox_from(500, 700, -100, 100);
        assert!(check_bbox(&view(), &t, &clip, &bbox));
    }

    #[test]
    fn bbox_fully_covered_view_is_culled() {
        let t = tables();
        let mut clip = ClipSegList::cleared(64);
        clip.clip_solid_wall_segment(0, 63, |_, _| {});
        let bbox = bbox_from(500, 700, -100, 100);
        assert!(!check_bbox(&view(), &t, &clip, &bbox));
    }

    #[test]
    fn bbox_on_the_view_axis_ahead_is_projected_to_center() {
        let t = tables();
        let clip = ClipSegList::cleared(64);
        // symmetric slab ahead: must be classified visible in an open view
        let bbox = bbox_from(1000, 1000, -10, 10);
        assert!(check_bbox(&view(), &t, &clip, &bbox));
    }
}
