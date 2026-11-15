//! View-dependent angle and projection math, ported from `r_main.c`:
//! `R_PointToAngle` (BSP-side angle computation) and the table-building
//! half of `R_InitTextureMapping` (`viewangletox`, `clipangle`).
//!
//! Nothing here draws anything; the clipping code in [`super::walls`] uses
//! the tables to turn view-relative angles into screen columns.

use crate::fixed::{FRACBITS, FRACUNIT, Fixed};
use crate::tables::{ANGLE_TO_FINE_SHIFT, Angle, FINE_ANGLES, FINE_TANGENT};

use super::tantoangle::{SLOPE_RANGE, TANTOANGLE};

/// Vanilla `FIELDOFVIEW`, in fine angles: 2048 fine units == 90 degrees.
pub const FIELD_OF_VIEW: u32 = 2048;

/// A viewpoint in world space: position plus facing direction.
///
/// The clipping code needs the position for line-of-sight angle tests and
/// the facing for view-relative clamping. `angle` is the view's BAM angle.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ViewState {
    /// View X in fixed point.
    pub x: Fixed,
    /// View Y in fixed point.
    pub y: Fixed,
    /// View facing direction.
    pub angle: Angle,
}

impl ViewState {
    /// Creates a viewpoint.
    pub const fn new(x: Fixed, y: Fixed, angle: Angle) -> ViewState {
        ViewState { x, y, angle }
    }
}

/// Projection tables for the clipping code: `viewangletox` and `clipangle`,
/// ported from `R_InitTextureMapping` (everything except the dead
/// `xtoviewangle[]` array, of which only entry 0 — `clipangle` — is kept).
///
/// Rebuild when the view width or FOV changes (vanilla does this from
/// `R_SetViewSize`). FOV is fixed at [`FIELD_OF_VIEW`] for now.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ViewTables {
    /// `viewangletox`: fine angle index -> screen column, fencepost cases
    /// resolved. Indexed with `(angle + ANG90) >> ANGLE_TO_FINE_SHIFT`.
    pub view_angle_to_x: [i32; FINE_ANGLES / 2],
    /// `clipangle`: half the horizontal FOV.
    pub clip_angle: Angle,
}

impl ViewTables {
    /// Builds the tables for a view `view_width` pixels wide whose center
    /// column is `center_x_frac` (fixed point) to the left edge.
    ///
    /// # Panics
    ///
    /// Panics when `view_width` is not positive — that is a programmer error,
    /// not data-driven input.
    pub fn new(view_width: i32, center_x_frac: Fixed) -> ViewTables {
        assert!(
            view_width > 0,
            "ViewTables::new requires a positive view width"
        );

        // Calc focallength so FIELDOFVIEW angles covers SCREENWIDTH.
        let focallength =
            center_x_frac / FINE_TANGENT[FINE_ANGLES / 4 + (FIELD_OF_VIEW / 2) as usize];

        let mut view_angle_to_x = [0i32; FINE_ANGLES / 2];
        for (i, slot) in view_angle_to_x.iter_mut().enumerate() {
            let ft = FINE_TANGENT[i];
            let t = if ft.to_raw() > 2 * FRACUNIT {
                -1
            } else if ft.to_raw() < -2 * FRACUNIT {
                view_width + 1
            } else {
                let t = ft * focallength;
                // C: (centerxfrac - t+FRACUNIT-1)>>FRACBITS, arithmetic shift
                let t = (center_x_frac - t + Fixed::from_raw(FRACUNIT - 1)).to_raw() >> FRACBITS;
                t.clamp(-1, view_width + 1)
            };
            *slot = t;
        }

        // xtoviewangle[0]: the first fine angle that maps to column 0 is
        // clipangle. The original builds the whole xtoviewangle array and
        // only ever reads entry 0.
        let i = view_angle_to_x
            .iter()
            .position(|&t| t <= 0)
            .expect("viewangletox always crosses zero within the table");
        let clip_angle = Angle::from_raw(
            ((i as u32) << ANGLE_TO_FINE_SHIFT).wrapping_sub(Angle::ANG90.to_raw()),
        );

        // Take out the fencepost cases from viewangletox.
        // (the original also computes t = centerx - ft*focallength here and
        //  never uses it; only the sentinel rewrite matters)
        for entry in view_angle_to_x.iter_mut() {
            if *entry == -1 {
                *entry = 0;
            } else if *entry == view_width + 1 {
                *entry = view_width;
            }
        }

        ViewTables {
            view_angle_to_x,
            clip_angle,
        }
    }
}

/// BAM angle from the view to a world point, `R_PointToAngle`.
///
/// `view` supplies the origin the way the globals `viewx`/`viewy` do in the
/// original. Coordinates are absolute world coordinates. Returns BAM
/// `ANGLE` 0 (east), 90 (north), etc.; a point exactly on the view returns
/// 0, same as the original.
pub fn point_to_angle(view: &ViewState, x: Fixed, y: Fixed) -> Angle {
    point_to_angle_rel(view.x, view.y, x, y)
}

/// `R_PointToAngle2`: angle from `(x1, y1)` to `(x2, y2)`.
pub fn point_to_angle_2(x1: Fixed, y1: Fixed, x2: Fixed, y2: Fixed) -> Angle {
    point_to_angle_rel(x1, y1, x2, y2)
}

fn point_to_angle_rel(view_x: Fixed, view_y: Fixed, x: Fixed, y: Fixed) -> Angle {
    let mut x = x - view_x;
    let mut y = y - view_y;

    if x.is_zero() && y.is_zero() {
        return Angle::ZERO; // as in original: if (!x && !y) return 0;
    }

    if x >= Fixed::ZERO {
        if y >= Fixed::ZERO {
            if x > y {
                // octant 0
                Angle::from_raw(TANTOANGLE[slope_div(y.to_raw() as u32, x.to_raw() as u32)])
            } else {
                // octant 1
                Angle::ANG90
                    - Angle::from_raw(1)
                    - Angle::from_raw(TANTOANGLE[slope_div(x.to_raw() as u32, y.to_raw() as u32)])
            }
        } else {
            y = -y;
            if x > y {
                // octant 8
                Angle::ZERO
                    - Angle::from_raw(TANTOANGLE[slope_div(y.to_raw() as u32, x.to_raw() as u32)])
            } else {
                // octant 7
                Angle::ANG270
                    + Angle::from_raw(TANTOANGLE[slope_div(x.to_raw() as u32, y.to_raw() as u32)])
            }
        }
    } else {
        x = -x;
        if y >= Fixed::ZERO {
            if x > y {
                // octant 3
                Angle::ANG180
                    - Angle::from_raw(1)
                    - Angle::from_raw(TANTOANGLE[slope_div(y.to_raw() as u32, x.to_raw() as u32)])
            } else {
                // octant 2
                Angle::ANG90
                    + Angle::from_raw(TANTOANGLE[slope_div(x.to_raw() as u32, y.to_raw() as u32)])
            }
        } else {
            y = -y;
            if x > y {
                // octant 4
                Angle::ANG180
                    + Angle::from_raw(TANTOANGLE[slope_div(y.to_raw() as u32, x.to_raw() as u32)])
            } else {
                // octant 5
                Angle::ANG270
                    - Angle::from_raw(1)
                    - Angle::from_raw(TANTOANGLE[slope_div(x.to_raw() as u32, y.to_raw() as u32)])
            }
        }
    }
}

/// `SlopeDiv`: tangent estimate for octant-space coordinates, clamped to
/// `SLOPERANGE`. Works on raw unsigned bit patterns, same as the original.
fn slope_div(num: u32, den: u32) -> usize {
    if den < 512 {
        return SLOPE_RANGE as usize;
    }

    let ans = (num << 3) / (den >> 8);

    ans.min(SLOPE_RANGE) as usize
}

/// Clamps an angle pair to the view edges, mirroring the two identical
/// `tspan` blocks in `R_AddLine` and `R_CheckBBox`. Both callers have
/// already subtracted the view angle and rejected spans >= ANG180.
///
/// Returns `None` when the whole span is beyond either edge of the frustum
/// (the original `return false` / silent `return` path).
pub(crate) fn clamp_span_to_view(
    angle1: Angle,
    angle2: Angle,
    clip_angle: Angle,
    span: Angle,
) -> Option<(Angle, Angle)> {
    let two_clip = clip_angle + clip_angle;
    let mut angle1 = angle1;
    let mut angle2 = angle2;

    // tspan here is `angle1 + clipangle`
    let mut tspan = angle1 + clip_angle;
    if tspan.to_raw() > two_clip.to_raw() {
        // tspan -= 2*clipangle
        tspan -= two_clip;

        // Totally off the left edge?
        if tspan.to_raw() >= span.to_raw() {
            return None;
        }

        angle1 = clip_angle;
    }
    // tspan here is `clipangle - angle2`
    let mut tspan = clip_angle - angle2;
    if tspan.to_raw() > two_clip.to_raw() {
        tspan -= two_clip;

        // Totally off the left edge?
        if tspan.to_raw() >= span.to_raw() {
            return None;
        }

        angle2 = Angle::ZERO - clip_angle;
    }

    Some((angle1, angle2))
}

/// Fine index for a view-relative angle: `(angle + ANG90) >> ANGLETOFINESHIFT`,
/// shared by the angle→column lookups in `R_AddLine` and `R_CheckBBox`.
/// Indices land in `0..FINE_ANGLES`; for angles that survived clamping the
/// index is always below `FINE_ANGLES / 2`, matching `view_angle_to_x`'s size.
pub(crate) fn fine_index(view_relative: Angle) -> usize {
    (view_relative + Angle::ANG90).fine_angle()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables::FINE_TANGENT;

    fn view_east() -> ViewState {
        ViewState::new(Fixed::ZERO, Fixed::ZERO, Angle::ZERO)
    }

    #[test]
    fn point_to_angle_cardinals() {
        let v = view_east();
        let east = point_to_angle(&v, Fixed::from_int(10), Fixed::ZERO);
        let north = point_to_angle(&v, Fixed::ZERO, Fixed::from_int(10));
        let west = point_to_angle(&v, Fixed::from_int(-10), Fixed::ZERO);
        let south = point_to_angle(&v, Fixed::ZERO, Fixed::from_int(-10));
        assert_eq!(east, Angle::ZERO);
        // as in original: exact-north lands one ulp below ANG90 (octant 1 path),
        // exact-west one ulp below ANG180 (octant 3 path)
        assert_eq!(north, Angle::ANG90 - Angle::from_raw(1));
        assert_eq!(west, Angle::ANG180 - Angle::from_raw(1));
        assert_eq!(south, Angle::ANG270);
    }

    #[test]
    fn point_to_angle_exactly_on_view_is_zero() {
        let v = view_east();
        assert_eq!(point_to_angle(&v, Fixed::ZERO, Fixed::ZERO), Angle::ZERO);
    }

    #[test]
    fn point_to_angle_diagonal() {
        let v = view_east();
        // slope 1: octant 1 = ANG90 - 1 - tantoangle[SLOPERANGE] = ANG90 - 1 - ANG45
        let angle = point_to_angle(&v, Fixed::from_int(100), Fixed::from_int(100));
        assert_eq!(angle, Angle::ANG90 - Angle::from_raw(1) - Angle::ANG45);
        // slope 1 to the south-east: octant 8 = -(tantoangle[SLOPERANGE])
        let angle = point_to_angle(&v, Fixed::from_int(100), Fixed::from_int(-100));
        assert_eq!(angle, Angle::ZERO - Angle::ANG45);
        // north-west: octant 2 = ANG90 + tantoangle[SLOPERANGE]
        let angle = point_to_angle(&v, Fixed::from_int(-100), Fixed::from_int(100));
        assert_eq!(angle, Angle::ANG90 + Angle::ANG45);
        // south-west: octant 5 = ANG270 - 1 - tantoangle[SLOPERANGE]
        let angle = point_to_angle(&v, Fixed::from_int(-100), Fixed::from_int(-100));
        assert_eq!(angle, Angle::ANG270 - Angle::from_raw(1) - Angle::ANG45);
    }

    #[test]
    fn point_to_angle_octant_8_and_4() {
        // octant 8 (x>=0, y<0, x>|y|): -tantoangle[SlopeDiv(|y|, x)]
        // octant 4 (x<0, y<0, |x|>|y|): ANG180 + tantoangle[SlopeDiv(|y|, |x|)]
        let v = view_east();
        let slope = super::slope_div((50 << 16) as u32, (100 << 16) as u32);
        let angle = point_to_angle(&v, Fixed::from_int(100), Fixed::from_int(-50));
        assert_eq!(angle, Angle::ZERO - Angle::from_raw(TANTOANGLE[slope]));
        let angle = point_to_angle(&v, Fixed::from_int(-100), Fixed::from_int(-50));
        assert_eq!(angle, Angle::ANG180 + Angle::from_raw(TANTOANGLE[slope]));
    }

    #[test]
    fn point_to_angle_matches_c_reference() {
        // reference values produced by running the original engine's
        // R_PointToAngle (with its tantoangle table) on the same inputs
        for (vx, vy, tx, ty, expected) in [
            (64 << 16, 16 << 16, 64 << 16, -16 << 16, 3_221_225_472u32),
            (100 << 16, -17 << 16, 100 << 16, 17 << 16, 1_073_741_823u32),
            (10 << 16, 100 << 16, 50 << 16, 80 << 16, 3_978_033_888u32),
            (128 << 16, 64 << 16, -77 << 16, -200 << 16, 2_769_960_511u32),
            (-50 << 16, -150 << 16, 300 << 16, 10 << 16, 293_034_656u32),
            (
                1000 << 16,
                1000 << 16,
                -1000 << 16,
                -1000 << 16,
                2_684_354_559u32,
            ),
        ] {
            let view = ViewState::new(Fixed::from_raw(vx), Fixed::from_raw(vy), Angle::ZERO);
            assert_eq!(
                point_to_angle(&view, Fixed::from_raw(tx), Fixed::from_raw(ty)).to_raw(),
                expected,
                "mismatch for view=({vx},{vy}) target=({tx},{ty})"
            );
        }
    }

    #[test]
    fn point_to_angle_matches_tantoangle_table() {
        // every octant agrees with the embedded LUT for a spread of slopes
        let v = view_east();
        let mut y = 3;
        while y < 40 {
            let x = 100;
            let expected =
                Angle::from_raw(TANTOANGLE[slope_div((y * 65536) as u32, (x * 65536) as u32)]);
            assert_eq!(
                point_to_angle(&v, Fixed::from_int(x), Fixed::from_int(y)),
                expected,
                "octant 0 mismatch at slope {y}/{x}"
            );
            y += 7;
        }
    }

    #[test]
    fn slope_div_clamps() {
        assert_eq!(super::slope_div(0, 0), SLOPE_RANGE as usize);
        assert_eq!(super::slope_div(1000, 511), SLOPE_RANGE as usize);
        assert_eq!(super::slope_div(0, 512), 0);
        // den >= 512: ans = num<<3/den>>8; for num == den the result is 2048 (45 deg)
        assert_eq!(super::slope_div(2048, 2048), SLOPE_RANGE as usize);
    }

    #[test]
    fn view_tables_half_fov_is_just_over_45_degrees() {
        let tables = ViewTables::new(64, Fixed::from_int(32));
        // clipangle is NOT exactly ANG45 in the original: the first fine angle
        // with viewangletox[i] <= 0 is fine index 3073, one step past ANG45,
        // for every view width (verified against the original tables)
        assert_eq!(tables.clip_angle, Angle::from_raw(0x2008_0000));
        // and that value is within one fine angle (2^19 raw units) of ANG45
        assert!(tables.clip_angle.to_raw() - Angle::ANG45.to_raw() <= 1 << ANGLE_TO_FINE_SHIFT);
    }

    #[test]
    fn view_tables_table_is_monotone_nonincreasing() {
        let tables = ViewTables::new(128, Fixed::from_int(64));
        // viewangletox is non-increasing across back-to-back entries
        for pair in tables.view_angle_to_x.windows(2) {
            assert!(pair[0] >= pair[1], "not monotone: {pair:?}");
        }
        // fencepost cases resolved: nothing left at -1 or view_width+1
        assert!(!tables.view_angle_to_x.contains(&-1));
        assert!(!tables.view_angle_to_x.contains(&(128 + 1)));
    }

    #[test]
    fn view_tables_center_column_is_view_center() {
        let tables = ViewTables::new(128, Fixed::from_int(64));
        // straight ahead (fine angle FINE_ANGLES/4) lands on the center column
        let center = tables.view_angle_to_x[FINE_ANGLES / 4];
        assert_eq!(center, 64);
    }

    #[test]
    fn view_tables_focallength_uses_tangent_at_fov_edge() {
        // sanity: ft[FINE_ANGLES/4 + FIELD_OF_VIEW/2] must be tan(45 deg) ~= 1.0
        let ft = FINE_TANGENT[FINE_ANGLES / 4 + (FIELD_OF_VIEW / 2) as usize];
        assert!((ft.to_raw() - FRACUNIT).abs() < FRACUNIT / 100);
    }

    fn ang45_quarter() -> Angle {
        Angle::from_raw(Angle::ANG45.to_raw() / 4)
    }

    #[test]
    fn clamp_span_inside_edges_returns_some() {
        // span fully inside the frustum: no clamping, angles pass through
        let clip = Angle::ANG45;
        let angle1 = ang45_quarter(); // 11.25 deg
        let angle2 = Angle::ZERO - ang45_quarter();
        let span = angle1 - angle2;
        let out = clamp_span_to_view(angle1, angle2, clip, span);
        assert_eq!(out, Some((angle1, angle2)));
    }

    #[test]
    fn clamp_span_off_left_edge_returns_none() {
        // whole span beyond the left edge: tspan - 2*clip >= span
        let clip = Angle::ANG45;
        let angle1 = Angle::ANG90 - ang45_quarter(); // 67.5 deg
        let angle2 = Angle::ANG90 - Angle::ANG45; // 45 deg
        let span = angle1 - angle2; // 22.5 deg
        // tspan = angle1 + clip - 2*clip = 22.5 >= span (22.5) -> rejected
        assert_eq!(clamp_span_to_view(angle1, angle2, clip, span), None);
    }

    #[test]
    fn fine_index_spot_checks() {
        assert_eq!(fine_index(Angle::ZERO), 2048); // ANG90 >> 19 == 2048
        assert_eq!(fine_index(Angle::ANG45), 3072);
        assert_eq!(fine_index(Angle::ZERO - Angle::ANG45), 1024);
    }
}
