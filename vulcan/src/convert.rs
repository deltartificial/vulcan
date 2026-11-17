//! The two conversions between `u32` and `usize`, and nothing else.
//!
//! Indices, counts and capacities are `u32`; `usize` appears only where Rust needs it, to index a
//! slice or read its length. Every crossing of that boundary goes through this module, so there is
//! one place that states the rule and checks it:
//!
//! - [`usize_from_u32`] is lossless, because the crate refuses to build where `usize` is narrower
//!   than 32 bits.
//! - [`u32_from_usize`] asserts that the value fits: a slice longer than `u32::MAX` items is a
//!   broken limit, which is a programmer error, not an operating error.
//!
//! `as` casts are banned in the workspace (`clippy::as_conversions`), since they truncate
//! silently.
//!
//! ```
//! use vulcan::convert::{u32_from_usize, usize_from_u32};
//!
//! let items = [10_u8, 20, 30];
//! let count = u32_from_usize(items.len());
//! assert_eq!(count, 3);
//! assert_eq!(items[usize_from_u32(count - 1)], 30);
//! ```

// `usize_from_u32` is lossless only where `usize` holds every `u32`.
const _: () = assert!(usize::BITS >= u32::BITS, "`usize` must hold every `u32`");

/// `u32::MAX` as a `usize`: the largest value that [`u32_from_usize`] accepts.
#[expect(clippy::as_conversions, reason = "lossless: `usize` holds every `u32`, see above")]
const U32_MAX_AS_USIZE: usize = u32::MAX as usize;

// The bound keeps every bit of `u32::MAX`, so no `u32` loses a bit on its way to `usize`.
const _: () = assert!(U32_MAX_AS_USIZE.count_ones() == u32::BITS, "the bound keeps every bit");

/// Converts a `u32` to a `usize`, losslessly.
///
/// # Panics
///
/// Never: the assertions check that the conversion is lossless, which the compile-time assertion
/// on the width of `usize` guarantees.
#[inline]
#[must_use = "a conversion has no side effect"]
pub const fn usize_from_u32(value: u32) -> usize {
    #[expect(clippy::as_conversions, reason = "lossless: `usize` holds every `u32`, see above")]
    let converted = value as usize;
    assert!(converted <= U32_MAX_AS_USIZE, "a converted `u32` stays within `u32::MAX`");
    assert!(converted.count_ones() == value.count_ones(), "the conversion keeps every bit");
    converted
}

/// Converts a `usize` to a `u32`.
///
/// # Panics
///
/// Panics if `value` is above `u32::MAX`.
#[inline]
#[must_use = "a conversion has no side effect"]
#[track_caller]
pub const fn u32_from_usize(value: usize) -> u32 {
    assert!(value <= U32_MAX_AS_USIZE, "a count or an index fits in `u32`");
    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "the assertion above proves that the value fits"
    )]
    let converted = value as u32;
    assert!(usize_from_u32(converted) == value, "the conversion keeps the value");
    converted
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trips the boundary values through both conversions: zero, one, and the values at and
    /// just below `u32::MAX` keep their value both ways.
    #[test]
    fn conversions_round_trip_at_the_bounds() {
        for value in [0_u32, 1, 2, u32::MAX - 1, u32::MAX] {
            let widened = usize_from_u32(value);
            assert_eq!(u64::try_from(widened).ok(), Some(u64::from(value)), "widening is exact");
            assert_eq!(u32_from_usize(widened), value, "narrowing gives the value back");
        }
    }

    /// Converts slice lengths, the use the module exists for: an empty slice and a short one,
    /// whose last index is reached through the converted count.
    #[test]
    fn slice_lengths_convert_to_counts() {
        let empty: [u8; 0] = [];
        assert_eq!(u32_from_usize(empty.len()), 0, "an empty slice has a count of zero");
        let items = [7_u8, 8, 9];
        let count = u32_from_usize(items.len());
        assert_eq!(count, 3, "the count matches the length");
        assert_eq!(items[usize_from_u32(count - 1)], 9, "the last index is the count minus one");
    }

    /// Converts the first `usize` above `u32::MAX`, which must crash rather than wrap to zero.
    #[test]
    #[should_panic(expected = "fits in `u32`")]
    #[cfg(target_pointer_width = "64")]
    fn u32_from_usize_rejects_values_above_u32_max() {
        let above = usize_from_u32(u32::MAX) + 1;
        let _wrapped = u32_from_usize(above);
    }

    /// Converts `usize::MAX`, the far end of the invalid range, which must crash rather than
    /// truncate to `u32::MAX`.
    #[test]
    #[should_panic(expected = "fits in `u32`")]
    #[cfg(target_pointer_width = "64")]
    fn u32_from_usize_rejects_usize_max() {
        let _truncated = u32_from_usize(usize::MAX);
    }
}
