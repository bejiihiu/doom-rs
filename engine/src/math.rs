//! Geometry and bounding-box helpers, ported from the original engine:
//! `P_AproxDistance`, `P_PointOnLineSide`, `P_PointOnDivlineSide`,
//! `P_BoxOnLineSide`, the `m_bbox.c` clear/add/enclose trio, and the
//! BAM angle difference helper the map slide code uses.
//!
//! Everything works on [`crate::fixed::Fixed`] 16.16 values, so results are
//! bit-identical to the original engine's integer math where the original
//! stayed in fixed point (division truncates toward zero there too).

use crate::fixed::{FRACBITS, Fixed};
use crate::tables::Angle;

/// A point in fixed-point world coordinates.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, Debug)]
pub struct Point {
    /// World X.
    pub x: Fixed,
    /// World Y.
    pub y: Fixed,
}

impl Point {
    /// Creates a point from raw fixed-point coordinates.
    pub const fn new(x: Fixed, y: Fixed) -> Point {
        Point { x, y }
    }
}

/// Direction vector with an origin: enough of `line_t`/`divline_t` to run the
/// side tests without depending on the map loader.
///
/// `x`/`y` is a point on the line, `dx`/`dy` the direction. BSP partition
/// lines use them straight from the node data; map lines build one with
/// [`Divline::between`].
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, Debug)]
pub struct Divline {
    /// A point on the line.
    pub x: Fixed,
    /// A point on the line, Y.
    pub y: Fixed,
    /// X direction of the line. Zero means vertical.
    pub dx: Fixed,
    /// Y direction of the line. Zero means horizontal.
    pub dy: Fixed,
}

impl Divline {
    /// Builds a divline through two points; origin is `from`. The direction
    /// wraps on overflow, same as the original `v2 - v1`.
    pub fn between(from: Point, to: Point) -> Divline {
        Divline {
            x: from.x,
            y: from.y,
            dx: to.x - from.x,
            dy: to.y - from.y,
        }
    }
}

/// Which of the four orientation classes a line falls into, as the original
/// `slopetype` in `line_t`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SlopeType {
    /// `dy` is zero.
    Horizontal,
    /// `dx` is zero.
    Vertical,
    /// `dx` and `dy` have the same sign.
    Positive,
    /// `dx` and `dy` have opposite signs.
    Negative,
}

/// Classifies a direction vector, same rules as the original `p_setup.c`:
/// zero `dx` is vertical, zero `dy` is horizontal, otherwise the signs decide.
pub const fn slope_type(dx: Fixed, dy: Fixed) -> SlopeType {
    if dx.to_raw() == 0 {
        SlopeType::Vertical
    } else if dy.to_raw() == 0 {
        SlopeType::Horizontal
    } else if (dy.to_raw() ^ dx.to_raw()) >= 0 {
        SlopeType::Positive
    } else {
        SlopeType::Negative
    }
}

/// Returns 0 when the point is on the front side (or the line degenerated to
/// a point), 1 when it is on the back side. Port of `P_PointOnLineSide`.
pub fn point_on_side(x: Fixed, y: Fixed, line: &Divline) -> i32 {
    if line.dx.is_zero() {
        if x <= line.x {
            return i32::from(line.dy > Fixed::ZERO);
        }
        return i32::from(line.dy < Fixed::ZERO);
    }
    if line.dy.is_zero() {
        if y <= line.y {
            return i32::from(line.dx < Fixed::ZERO);
        }
        return i32::from(line.dx > Fixed::ZERO);
    }

    let dx = x - line.x;
    let dy = y - line.y;

    let left = Fixed::from_raw(line.dy.to_raw() >> FRACBITS) * dx;
    let right = dy * Fixed::from_raw(line.dx.to_raw() >> FRACBITS);

    if right < left { 0 } else { 1 }
}

/// Same question, with the sign-bit quick path from `P_PointOnDivlineSide`.
/// Agrees with [`point_on_side`] on every input, just faster on big coords.
pub fn point_on_divline_side(x: Fixed, y: Fixed, line: &Divline) -> i32 {
    if line.dx.is_zero() {
        if x <= line.x {
            return i32::from(line.dy > Fixed::ZERO);
        }
        return i32::from(line.dy < Fixed::ZERO);
    }
    if line.dy.is_zero() {
        if y <= line.y {
            return i32::from(line.dx < Fixed::ZERO);
        }
        return i32::from(line.dx > Fixed::ZERO);
    }

    let dx = x - line.x;
    let dy = y - line.y;

    // quick sign-bit decision: if dx/dy/line directions disagree in sign,
    // the answer is decided by the signs alone
    if (line.dy.to_raw() ^ line.dx.to_raw() ^ dx.to_raw() ^ dy.to_raw()) & i32::MIN != 0 {
        return i32::from((line.dy.to_raw() ^ dx.to_raw()) & i32::MIN != 0);
    }

    let left = Fixed::from_raw(line.dy.to_raw() >> FRACBITS) * dx;
    let right = dy * Fixed::from_raw(line.dx.to_raw() >> FRACBITS);

    if right < left { 0 } else { 1 }
}

/// Axis-aligned bounding box, the m_bbox quartet: `BOXTOP`, `BOXBOTTOM`,
/// `BOXLEFT`, `BOXRIGHT`. `top`/`right` start at MIN, `bottom`/`left` at MAX,
/// so an empty box is upside down until the first point is added.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BBox {
    /// Maximum Y (inclusive).
    pub top: Fixed,
    /// Minimum Y (inclusive).
    pub bottom: Fixed,
    /// Minimum X (inclusive).
    pub left: Fixed,
    /// Maximum X (inclusive).
    pub right: Fixed,
}

impl BBox {
    /// Empty box: inverted limits, mirrors `M_ClearBox`.
    pub const fn cleared() -> BBox {
        BBox {
            top: Fixed::MIN,
            right: Fixed::MIN,
            bottom: Fixed::MAX,
            left: Fixed::MAX,
        }
    }

    /// Grows the box to include `(x, y)`, mirrors `M_AddToBox`.
    ///
    /// Uses independent comparisons; the original chained `if/else-if`,
    /// which drops the largest first-added coordinate on a cleared box
    /// (the screen dirty-box use case never hit it, but `enclose` does).
    pub fn add_to_box(&mut self, x: Fixed, y: Fixed) {
        if x < self.left {
            self.left = x;
        }
        if x > self.right {
            self.right = x;
        }
        if y < self.bottom {
            self.bottom = y;
        }
        if y > self.top {
            self.top = y;
        }
    }

    /// Builds the smallest box containing all `points` (clear + add in one pass).
    pub fn enclose<I: IntoIterator<Item = Point>>(points: I) -> BBox {
        let mut box_ = BBox::cleared();
        for point in points {
            box_.add_to_box(point.x, point.y);
        }
        box_
    }

    /// True when `x`/`y` is inside the box.
    pub fn contains(&self, x: Fixed, y: Fixed) -> bool {
        x >= self.left && x <= self.right && y >= self.bottom && y <= self.top
    }

    /// True when nothing was ever added to the box.
    pub fn is_empty(&self) -> bool {
        self.left > self.right || self.bottom > self.top
    }
}

impl Default for BBox {
    fn default() -> Self {
        BBox::cleared()
    }
}

/// Side of a line an entire bounding box is on. `Some(0)`/`Some(1)` mean the
/// whole box is on that side, `None` means the box straddles the line
/// (the original returns -1 in that case). Port of `P_BoxOnLineSide`.
///
/// The line is treated as infinite.
pub fn box_on_side(bbox: &BBox, line: &Divline) -> Option<i32> {
    let mut p1;
    let mut p2;

    match slope_type(line.dx, line.dy) {
        SlopeType::Horizontal => {
            p1 = i32::from(bbox.top > line.y);
            p2 = i32::from(bbox.bottom > line.y);
            if line.dx < Fixed::ZERO {
                p1 ^= 1;
                p2 ^= 1;
            }
        }
        SlopeType::Vertical => {
            p1 = i32::from(bbox.right < line.x);
            p2 = i32::from(bbox.left < line.x);
            if line.dy < Fixed::ZERO {
                p1 ^= 1;
                p2 ^= 1;
            }
        }
        SlopeType::Positive => {
            p1 = point_on_side(bbox.left, bbox.top, line);
            p2 = point_on_side(bbox.right, bbox.bottom, line);
        }
        SlopeType::Negative => {
            p1 = point_on_side(bbox.right, bbox.top, line);
            p2 = point_on_side(bbox.left, bbox.bottom, line);
        }
    }

    if p1 == p2 { Some(p1) } else { None }
}

/// Rough Euclidean distance, the original `P_AproxDistance`: no sqrt,
/// takes max + min/2. Never underestimates by more than ~13%.
pub fn aprox_distance(dx: Fixed, dy: Fixed) -> Fixed {
    // i64 abs so Fixed::MIN can't wrap to a negative magnitude
    let dx = (dx.to_raw() as i64).abs();
    let dy = (dy.to_raw() as i64).abs();
    let (max, min) = if dx >= dy { (dx, dy) } else { (dy, dx) };
    Fixed::from_raw((max + min - (min >> 1)) as i32)
}

/// Wrapped difference of two BAM angles, `a - b`. Compare the result against
/// [`Angle::ANG180`] to fold the range the way the slide code does.
pub const fn angle_delta(a: Angle, b: Angle) -> Angle {
    Angle::from_raw(a.to_raw().wrapping_sub(b.to_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixed::FRACUNIT;

    fn fixed(whole: i32) -> Fixed {
        Fixed::from_int(whole)
    }

    fn unit() -> Fixed {
        Fixed::ONE
    }

    #[test]
    fn point_on_side_horizontal_line() {
        // line from (0, 0) to (4, 0)
        let line = Divline::between(
            Point::new(Fixed::ZERO, Fixed::ZERO),
            Point::new(fixed(4), Fixed::ZERO),
        );
        assert_eq!(point_on_side(fixed(2), fixed(1), &line), 1);
        assert_eq!(point_on_side(fixed(2), fixed(-1), &line), 0);
        assert_eq!(point_on_side(fixed(2), Fixed::ZERO, &line), 0);

        // reversed direction flips the on-line answer, same as the original
        let rev = Divline::between(
            Point::new(fixed(4), Fixed::ZERO),
            Point::new(Fixed::ZERO, Fixed::ZERO),
        );
        assert_eq!(point_on_side(fixed(2), Fixed::ZERO, &rev), 1);
        assert_eq!(point_on_side(fixed(2), fixed(1), &rev), 0);
        assert_eq!(point_on_side(fixed(2), fixed(-1), &rev), 1);
    }

    #[test]
    fn point_on_side_vertical_line() {
        // line from (0, 0) to (0, 4)
        let line = Divline::between(
            Point::new(Fixed::ZERO, Fixed::ZERO),
            Point::new(Fixed::ZERO, fixed(4)),
        );
        assert_eq!(point_on_side(fixed(1), fixed(2), &line), 0);
        assert_eq!(point_on_side(fixed(-1), fixed(2), &line), 1);
        assert_eq!(point_on_side(Fixed::ZERO, fixed(2), &line), 1); // x <= v1.x, dy > 0
    }

    #[test]
    fn point_on_side_diagonal_line() {
        // line from (0, 0) to (4, 4): y = x
        let line = Divline::between(
            Point::new(Fixed::ZERO, Fixed::ZERO),
            Point::new(fixed(4), fixed(4)),
        );
        assert_eq!(point_on_side(fixed(4), fixed(1), &line), 0); // below the diagonal: front
        assert_eq!(point_on_side(fixed(1), fixed(4), &line), 1); // above: back
        // points exactly on the line produce right == left, so the
        // comparison lands on the "back side", same as the original
        assert_eq!(point_on_side(Fixed::ZERO, Fixed::ZERO, &line), 1);
    }

    #[test]
    fn divline_side_agrees_with_line_side() {
        let line = Divline::between(
            Point::new(fixed(1), Fixed::ZERO),
            Point::new(fixed(9), fixed(7)),
        );
        for x in [-3, 0, 2, 5, 20] {
            for y in [-4, 1, 6, 30] {
                let (fx, fy) = (fixed(x), fixed(y));
                assert_eq!(
                    point_on_side(fx, fy, &line),
                    point_on_divline_side(fx, fy, &line),
                    "mismatch at ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn slope_type_classification() {
        assert_eq!(slope_type(Fixed::ZERO, unit()), SlopeType::Vertical);
        assert_eq!(slope_type(unit(), Fixed::ZERO), SlopeType::Horizontal);
        assert_eq!(slope_type(unit(), unit()), SlopeType::Positive);
        assert_eq!(slope_type(unit(), -unit()), SlopeType::Negative);
    }

    #[test]
    fn bbox_clear_add_enclose() {
        let mut b = BBox::cleared();
        assert!(b.is_empty());
        b.add_to_box(fixed(10), fixed(-3));
        b.add_to_box(fixed(-5), fixed(7));
        assert_eq!(b.left, fixed(-5));
        assert_eq!(b.right, fixed(10));
        assert_eq!(b.top, fixed(7));
        assert_eq!(b.bottom, fixed(-3));
        assert!(!b.is_empty());

        let points = [
            Point::new(fixed(3), fixed(3)),
            Point::new(fixed(-2), fixed(8)),
            Point::new(fixed(6), fixed(-1)),
        ];
        let b = BBox::enclose(points);
        assert_eq!(b.left, fixed(-2));
        assert_eq!(b.right, fixed(6));
        assert_eq!(b.top, fixed(8));
        assert_eq!(b.bottom, fixed(-1));
        assert!(b.contains(fixed(0), fixed(0)));
        assert!(!b.contains(fixed(-3), fixed(0)));
    }

    #[test]
    fn box_on_side_horizontal() {
        // line on y = 2, going right
        let line = Divline::between(
            Point::new(Fixed::ZERO, fixed(2)),
            Point::new(fixed(4), fixed(2)),
        );
        let above = BBox::enclose([
            Point::new(fixed(0), fixed(5)),
            Point::new(fixed(1), fixed(3)),
        ]);
        assert_eq!(box_on_side(&above, &line), Some(1));
        let below = BBox::enclose([
            Point::new(fixed(0), fixed(1)),
            Point::new(fixed(1), fixed(-1)),
        ]);
        assert_eq!(box_on_side(&below, &line), Some(0));
        let straddling = BBox::enclose([
            Point::new(fixed(0), fixed(3)),
            Point::new(fixed(1), fixed(1)),
        ]);
        assert_eq!(box_on_side(&straddling, &line), None);

        // reversed direction flips the comparison, same as the original
        let rev = Divline::between(
            Point::new(fixed(4), fixed(2)),
            Point::new(Fixed::ZERO, fixed(2)),
        );
        assert_eq!(box_on_side(&above, &rev), Some(0));
    }

    #[test]
    fn box_on_side_vertical() {
        // line on x = 2, going up
        let line = Divline::between(
            Point::new(fixed(2), Fixed::ZERO),
            Point::new(fixed(2), fixed(4)),
        );
        let right = BBox::enclose([
            Point::new(fixed(3), fixed(0)),
            Point::new(fixed(6), fixed(1)),
        ]);
        assert_eq!(box_on_side(&right, &line), Some(0));
        let left = BBox::enclose([
            Point::new(fixed(-2), fixed(0)),
            Point::new(fixed(1), fixed(1)),
        ]);
        assert_eq!(box_on_side(&left, &line), Some(1));
        let straddling = BBox::enclose([
            Point::new(fixed(1), fixed(0)),
            Point::new(fixed(3), fixed(1)),
        ]);
        assert_eq!(box_on_side(&straddling, &line), None);
    }

    #[test]
    fn box_on_side_diagonal() {
        // line y = x
        let line = Divline::between(
            Point::new(Fixed::ZERO, Fixed::ZERO),
            Point::new(fixed(8), fixed(8)),
        );
        let below_right = BBox::enclose([
            Point::new(fixed(4), Fixed::ZERO),
            Point::new(fixed(8), fixed(2)),
        ]);
        assert_eq!(box_on_side(&below_right, &line), Some(0));
        let above_left = BBox::enclose([
            Point::new(fixed(0), fixed(6)),
            Point::new(fixed(2), fixed(8)),
        ]);
        assert_eq!(box_on_side(&above_left, &line), Some(1));
        let straddling = BBox::enclose([
            Point::new(fixed(0), fixed(8)),
            Point::new(fixed(8), fixed(0)),
        ]);
        assert_eq!(box_on_side(&straddling, &line), None);
    }

    #[test]
    fn aprox_distance_basic() {
        // fixed-point math: dx=3.0, dy=4.0 -> 3+4-1.5 = 5.5
        assert_eq!(aprox_distance(fixed(3), fixed(4)), Fixed::from_raw(360_448));
        assert_eq!(aprox_distance(fixed(4), fixed(3)), Fixed::from_raw(360_448));
        assert_eq!(
            aprox_distance(fixed(-3), fixed(-4)),
            Fixed::from_raw(360_448)
        );
        assert_eq!(aprox_distance(Fixed::ZERO, Fixed::ZERO), Fixed::ZERO);
        assert_eq!(aprox_distance(fixed(10), Fixed::ZERO), fixed(10));
    }

    #[test]
    fn angle_delta_wraps() {
        assert_eq!(angle_delta(Angle::ANG90, Angle::ANG45), Angle::ANG45);
        assert_eq!(
            angle_delta(Angle::ZERO, Angle::from_raw(1)),
            Angle::from_raw(u32::MAX)
        );
        assert_eq!(angle_delta(Angle::ANG90, Angle::ANG90), Angle::ZERO);
    }

    #[test]
    fn fixed_frac_constants() {
        // FRACUNIT == 65536, kept here so the test fails loudly if someone retunes it
        assert_eq!(FRACUNIT, 65_536);
    }
}
