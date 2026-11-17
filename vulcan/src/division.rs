//! Integer division that states its rounding.
//!
//! A bare `/` on integers truncates, and the reader cannot tell whether the author expected an
//! exact result, wanted the floor, or forgot that rounding happens at all. Off-by-one errors hide
//! in that gap. Every integer division goes through one of three operations instead, and the
//! `integer_division` lint flags any other:
//!
//! - [`Divide::divide_exact`]: the remainder must be zero, and this is asserted.
//! - [`Divide::divide_floor`]: rounds down.
//! - `div_ceil`, the inherent method of the standard library: rounds up.
//!
//! ```
//! use vulcan::division::Divide;
//!
//! assert_eq!(4096_u32.divide_exact(64), 64);
//! assert_eq!(100_u32.divide_floor(64), 1);
//! assert_eq!(100_u32.div_ceil(64), 2);
//! assert_eq!(100_u32.remainder(64), 36);
//! ```

/// Division and remainder for unsigned integers, with the rounding in the name.
pub trait Divide: Copy {
    /// Divides, asserting that `divisor` divides `self` with no remainder.
    ///
    /// # Panics
    ///
    /// Panics if `divisor` is zero or if the division leaves a remainder.
    #[must_use = "a division has no side effect"]
    fn divide_exact(self, divisor: Self) -> Self;

    /// Divides, rounding down.
    ///
    /// # Panics
    ///
    /// Panics if `divisor` is zero.
    #[must_use = "a division has no side effect"]
    fn divide_floor(self, divisor: Self) -> Self;

    /// Returns what [`divide_floor`](Self::divide_floor) leaves over.
    ///
    /// # Panics
    ///
    /// Panics if `divisor` is zero.
    #[must_use = "a division has no side effect"]
    fn remainder(self, divisor: Self) -> Self;
}

/// Implements [`Divide`] for each unsigned integer type given.
///
/// Every operation asserts its precondition (a divisor above zero) and its postcondition, which
/// ties the quotient and the remainder back to the dividend without overflow: the product
/// `quotient * divisor` never exceeds `self`.
macro_rules! impl_divide {
    ($($integer:ty),*) => {$(
        impl Divide for $integer {
            #[inline]
            #[track_caller]
            fn divide_exact(self, divisor: Self) -> Self {
                assert!(divisor > 0, "division by zero");
                assert!(self.rem_euclid(divisor) == 0, "inexact division of {self} by {divisor}");
                let quotient = self.div_euclid(divisor);
                assert!(quotient * divisor == self, "an exact quotient multiplies back");
                quotient
            }

            #[inline]
            #[track_caller]
            fn divide_floor(self, divisor: Self) -> Self {
                assert!(divisor > 0, "division by zero");
                let quotient = self.div_euclid(divisor);
                assert!(self - quotient * divisor < divisor, "a floor leaves under one divisor");
                quotient
            }

            #[inline]
            #[track_caller]
            fn remainder(self, divisor: Self) -> Self {
                assert!(divisor > 0, "division by zero");
                let remainder = self.rem_euclid(divisor);
                assert!(remainder < divisor, "a remainder is below the divisor");
                remainder
            }
        }
    )*};
}

impl_divide!(u8, u16, u32, u64, u128, usize);

#[cfg(test)]
mod tests {
    use super::*;

    /// Divides typical values with each rounding, and checks that the floor and the ceiling
    /// differ by one exactly when there is a remainder.
    #[test]
    fn rounding_is_explicit() {
        assert_eq!(64_u32.divide_exact(8), 8);
        assert_eq!(63_u32.divide_floor(8), 7);
        assert_eq!(63_u32.div_ceil(8), 8);
        assert_eq!(63_u32.remainder(8), 7);
        assert_eq!(0_usize.divide_exact(8), 0);
        // The boundary where the floor and the ceiling meet again.
        assert_eq!(64_u64.divide_floor(8), 64_u64.div_ceil(8));
        assert_eq!(65_u64.divide_floor(8) + 1, 65_u64.div_ceil(8));
    }

    /// Divides at the edges of the domain, for every implemented type: zero and the maximum as
    /// dividends, one and the maximum as divisors.
    #[test]
    fn boundaries_hold_for_every_type() {
        assert_eq!(0_u8.divide_floor(u8::MAX), 0, "zero divided by anything is zero");
        assert_eq!(u8::MAX.divide_exact(1), u8::MAX, "one divides everything");
        assert_eq!(u8::MAX.divide_exact(u8::MAX), 1, "a value divides itself");
        assert_eq!(u8::MAX.divide_exact(17), 15, "255 is 15 times 17");
        assert_eq!(u16::MAX.remainder(u16::MAX), 0);
        assert_eq!((u16::MAX - 1).remainder(u16::MAX), u16::MAX - 1, "below the divisor");
        assert_eq!(u32::MAX.divide_floor(2), u32::MAX >> 1_u32, "halving is a shift");
        assert_eq!(u32::MAX.remainder(2), 1, "the maximum is odd");
        assert_eq!(u64::MAX.divide_floor(u64::MAX - 1), 1);
        assert_eq!(u64::MAX.remainder(u64::MAX - 1), 1);
        assert_eq!(u128::MAX.divide_exact(1), u128::MAX);
        assert_eq!(1_u128.divide_floor(u128::MAX), 0, "a smaller dividend floors to zero");
        assert_eq!(usize::MAX.divide_exact(usize::MAX), 1);
        assert_eq!(1_usize.remainder(1), 0, "one divides one");
    }

    /// Divides with a remainder of one, the smallest inexact case, which must crash.
    #[test]
    #[should_panic(expected = "inexact division of 65 by 8")]
    fn divide_exact_rejects_inexact_division() {
        let _quotient = 65_u32.divide_exact(8);
    }

    /// Divides the maximum by a value one above half of it, which leaves a large remainder and
    /// must crash.
    #[test]
    #[should_panic(expected = "inexact division of 255 by 128")]
    fn divide_exact_rejects_inexact_division_at_the_maximum() {
        let _quotient = u8::MAX.divide_exact(128);
    }

    /// Divides exactly by zero, which must crash rather than return zero.
    #[test]
    #[should_panic(expected = "division by zero")]
    fn divide_exact_rejects_zero_divisor() {
        let _quotient = 0_u32.divide_exact(0);
    }

    /// Divides with rounding down by zero, which must crash.
    #[test]
    #[should_panic(expected = "division by zero")]
    fn divide_floor_rejects_zero_divisor() {
        let _quotient = 1_u32.divide_floor(0);
    }

    /// Takes the remainder of a division by zero, which must crash.
    #[test]
    #[should_panic(expected = "division by zero")]
    fn remainder_rejects_zero_divisor() {
        let _remainder = u64::MAX.remainder(0);
    }
}
