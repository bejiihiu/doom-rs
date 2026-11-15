//! 16.16 fixed-point arithmetic, ported from the original `m_fixed.c`.
//!
//! The engine does its integer-based math in 16.16 fixed point: the low 16
//! bits hold the fractional part, the high 16 bits the integer part. This
//! type wraps the raw `i32` so mixing raw ints and fixed-point values is a
//! compile-time error instead of a silent 65536x mistake.
//!
//! Addition, subtraction and negation wrap on overflow, exactly like the
//! two's complement arithmetic the original engine relied on everywhere.
//! Multiplication takes a 64-bit intermediate and truncates back to 32 bits,
//! bit for bit identical to the original
//! `((long long)a * (long long)b) >> FRACBITS`.
//! Division uses a 64-bit left shift and C-style division that truncates
//! toward zero, with the same clamp-to-MIN/MAX behavior on overflow as the
//! original `FixedDiv`.

use core::fmt;
use core::ops;

/// Number of fractional bits in a [`Fixed`].
pub const FRACBITS: u32 = 16;
/// `1 << FRACBITS`, the fixed-point representation of 1.0.
pub const FRACUNIT: i32 = 1 << FRACBITS;
/// One as a [`Fixed`].
pub const FRAC_UNIT: Fixed = Fixed(FRACUNIT);

/// 16.16 fixed-point number.
///
/// The raw bit pattern is monotonic as a signed integer, so the derived
/// [`Ord`] matches the numeric order — no manual comparison needed.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Fixed(i32);

impl Fixed {
    /// Smallest representable value.
    pub const MIN: Fixed = Fixed(i32::MIN);
    /// Largest representable value.
    pub const MAX: Fixed = Fixed(i32::MAX);
    /// Zero.
    pub const ZERO: Fixed = Fixed(0);
    /// One.
    pub const ONE: Fixed = FRAC_UNIT;

    /// Wraps a raw 16.16 bit pattern.
    pub const fn from_raw(raw: i32) -> Self {
        Fixed(raw)
    }

    /// Returns the raw 16.16 bit pattern.
    pub const fn to_raw(self) -> i32 {
        self.0
    }

    /// Converts a whole number to fixed point. Bits shifted out past the
    /// integer range are lost, same as the original `n * FRACUNIT`.
    pub const fn from_int(value: i32) -> Self {
        Fixed(value << FRACBITS)
    }

    /// Converts to a whole number, truncating toward zero. Matches `/ FRACUNIT`
    /// in the original, not a floor.
    pub const fn to_int(self) -> i32 {
        self.0 / FRACUNIT
    }

    /// Absolute value. Saturates to [`Fixed::MAX`] for [`Fixed::MIN`] instead
    /// of wrapping to a negative number like a naive `abs` would.
    pub fn abs(self) -> Self {
        Fixed((self.0 as i64).abs().min(i64::from(i32::MAX)) as i32)
    }

    /// True when the value is zero.
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }
}

impl ops::Add for Fixed {
    type Output = Fixed;

    /// Wraps on overflow, like the original two's complement math.
    fn add(self, rhs: Self) -> Self {
        Fixed(self.0.wrapping_add(rhs.0))
    }
}

impl ops::Sub for Fixed {
    type Output = Fixed;

    /// Wraps on overflow, like the original two's complement math.
    fn sub(self, rhs: Self) -> Self {
        Fixed(self.0.wrapping_sub(rhs.0))
    }
}

impl ops::Neg for Fixed {
    type Output = Fixed;

    /// Wraps on overflow; negating [`Fixed::MIN`] gives [`Fixed::MIN`].
    fn neg(self) -> Self {
        Fixed(self.0.wrapping_neg())
    }
}

impl ops::Mul for Fixed {
    type Output = Fixed;

    /// Takes the 64-bit product and keeps the middle 32 bits; overflow wraps,
    /// it does not saturate. Identical bit pattern to the original `FixedMul`.
    fn mul(self, rhs: Self) -> Self {
        Fixed((((self.0 as i64) * (rhs.0 as i64)) >> FRACBITS) as i32)
    }
}

impl ops::Div for Fixed {
    type Output = Fixed;

    /// Ports the original `FixedDiv`: when the quotient cannot fit, clamps to
    /// [`Fixed::MIN`] or [`Fixed::MAX`] depending on the sign, and a zero
    /// divisor lands on the same clamp instead of panicking. The actual
    /// division truncates toward zero, same as C.
    fn div(self, rhs: Self) -> Self {
        // the original clamps when |a| >> 14 reaches |b|; i64 math keeps
        // abs(MIN) from going negative the way a 32-bit abs would
        if ((self.0 as i64).abs() >> 14) >= (rhs.0 as i64).abs() {
            return if (self.0 ^ rhs.0) < 0 {
                Fixed::MIN
            } else {
                Fixed::MAX
            };
        }
        Fixed((((self.0 as i64) << FRACBITS) / (rhs.0 as i64)) as i32)
    }
}

impl ops::AddAssign for Fixed {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl ops::SubAssign for Fixed {
    fn sub_assign(&mut self, rhs: Self) {
        *self = *self - rhs;
    }
}

impl ops::MulAssign for Fixed {
    fn mul_assign(&mut self, rhs: Self) {
        *self = *self * rhs;
    }
}

impl ops::DivAssign for Fixed {
    fn div_assign(&mut self, rhs: Self) {
        *self = *self / rhs;
    }
}

impl fmt::Display for Fixed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let raw = self.0;
        if raw < 0 {
            f.write_str("-")?;
        }
        let mag = (raw as i64).unsigned_abs();
        let whole = mag >> FRACBITS;
        let frac = mag & 0xFFFF;
        if frac == 0 {
            return write!(f, "{whole}");
        }
        // 2^16 divides 10^16 exactly, so the decimal fraction terminates:
        // frac/2^16 == (frac * 5^16) / 10^16
        let digits = frac * 152_587_890_625;
        let digits = format!("{digits:016}");
        let trimmed = digits.trim_end_matches('0');
        write!(f, "{whole}.{trimmed}")
    }
}

impl fmt::Debug for Fixed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fixed({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_int_round_trip() {
        for value in [0, 1, -1, 42, -42, 32767, -32768, 1000, -1000] {
            assert_eq!(Fixed::from_int(value).to_int(), value);
        }
    }

    #[test]
    fn to_int_truncates_toward_zero() {
        assert_eq!(Fixed::from_raw(98_304).to_int(), 1); // 1.5 -> 1
        assert_eq!(Fixed::from_raw(-98_304).to_int(), -1); // -1.5 -> -1, not -2
        assert_eq!(Fixed::from_raw(-65_536).to_int(), -1);
    }

    #[test]
    fn from_raw_keeps_bits() {
        assert_eq!(Fixed::from_raw(0x1234_ABCD).to_raw(), 0x1234_ABCD);
    }

    #[test]
    fn mul_basic() {
        let a = Fixed::from_int(3);
        let b = Fixed::from_int(2);
        assert_eq!(a * b, Fixed::from_int(6));

        let half = Fixed::from_raw(32_768); // 0.5
        assert_eq!(Fixed::from_int(4) * half, Fixed::from_int(2));
    }

    #[test]
    fn mul_one_and_minus_one() {
        let one = Fixed::ONE;
        assert_eq!(one * Fixed(-65_536), Fixed(-65_536));
        assert_eq!(Fixed(-65_536) * one, Fixed(-65_536));
        assert_eq!(Fixed::MIN * one, Fixed::MIN);
        assert_eq!(one * Fixed::MIN, Fixed::MIN);
    }

    #[test]
    fn mul_matches_original_wrap_semantics() {
        // bit pattern the original produces: ((i64)a * (i64)b) >> 16 as i32
        for (a, b) in [
            (0x7FFF_FFFFi32, 0x4000_0000i32),
            (0x4000_0000, 0x4000_0000),
            (-0x4000_0000, 0x4000_0000),
            (0x7FFF_FFF0, 0x0001_0000),
        ] {
            let expect = (((a as i64) * (b as i64)) >> FRACBITS) as i32;
            assert_eq!(
                Fixed::from_raw(a) * Fixed::from_raw(b),
                Fixed::from_raw(expect)
            );
        }
    }

    #[test]
    fn div_truncates_toward_zero() {
        // 1 / 3 == 0.3333 -> 21845 in 16.16, not 21846
        let third = Fixed::ONE / Fixed::from_int(3);
        assert_eq!(third.to_raw(), 21_845);
        // -1 / 3 -> -21845, C truncates toward zero, not floor
        let neg_third = Fixed::from_int(-1) / Fixed::from_int(3);
        assert_eq!(neg_third.to_raw(), -21_845);
    }

    #[test]
    fn div_overflow_clamps_like_original() {
        assert_eq!(Fixed::MAX / Fixed::ONE, Fixed::MAX);
        assert_eq!(Fixed::MIN / Fixed::ONE, Fixed::MIN);
        // sign of the denominator flips the clamp
        assert_eq!(Fixed::MAX / Fixed::from_int(-1), Fixed::MIN);
        assert_eq!(Fixed::MIN / Fixed::from_int(-1), Fixed::MAX);
    }

    #[test]
    fn div_by_zero_clamps_instead_of_panicking() {
        assert_eq!(Fixed::from_int(1) / Fixed::ZERO, Fixed::MAX);
        assert_eq!(Fixed::from_int(-1) / Fixed::ZERO, Fixed::MIN);
        assert_eq!(Fixed::ZERO / Fixed::ZERO, Fixed::MAX);
    }

    #[test]
    fn div_exact() {
        assert_eq!(Fixed::from_int(6) / Fixed::from_int(3), Fixed::from_int(2));
        assert_eq!(Fixed::ONE / Fixed::ONE, Fixed::ONE);
    }

    #[test]
    fn abs_handles_min() {
        assert_eq!(Fixed::from_int(-5).abs(), Fixed::from_int(5));
        assert_eq!(Fixed::MIN.abs(), Fixed::MAX); // saturates
    }

    #[test]
    fn ordering_is_numeric() {
        let mut values = vec![Fixed::ONE, Fixed::MIN, Fixed::ZERO, Fixed::MAX, Fixed(-1)];
        values.sort();
        assert_eq!(
            values,
            vec![Fixed::MIN, Fixed(-1), Fixed::ZERO, Fixed::ONE, Fixed::MAX]
        );
    }

    #[test]
    fn add_sub_wrap_like_original() {
        assert_eq!(
            (Fixed::MAX + Fixed::ONE).to_raw(),
            i32::MAX.wrapping_add(FRACUNIT)
        );
        assert_eq!(Fixed::MAX + Fixed::from_raw(1), Fixed::MIN);
        assert_eq!(Fixed::MIN - Fixed::from_raw(1), Fixed::MAX);
        assert_eq!(-Fixed::MIN, Fixed::MIN); // wraps, not UB
    }

    #[test]
    fn display_formats_decimals() {
        assert_eq!(Fixed::ONE.to_string(), "1");
        assert_eq!(Fixed::ZERO.to_string(), "0");
        assert_eq!(Fixed::from_raw(32_768).to_string(), "0.5");
        assert_eq!(Fixed::from_int(-2).to_string(), "-2");
        assert_eq!(Fixed::from_raw(-98_304).to_string(), "-1.5");
        assert_eq!(Fixed::from_raw(1).to_string(), "0.0000152587890625");
        assert_eq!(format!("{}", Fixed::ONE / Fixed::from_int(4)), "0.25");
    }

    #[test]
    fn debug_shows_value() {
        assert_eq!(format!("{:?}", Fixed::from_raw(32_768)), "Fixed(0.5)");
    }
}
