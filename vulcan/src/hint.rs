//! Branch hints and cold-path outlining.
//!
//! The optimizer lays out code for the common case only if it knows which case is common. These
//! helpers mark rare branches so the hot path stays dense in the instruction cache and falls
//! through without a jump:
//!
//! - [`likely`] and [`unlikely`] wrap a condition.
//! - [`cold_path`] marks the current branch as rarely taken.
//! - [`outline`] moves a rare block (error construction, diagnostics) out of the caller, so it does
//!   not bloat the caller's code or its inlining budget.
//!
//! ```
//! use vulcan::{
//!     division::Divide,
//!     hint::{outline, unlikely},
//! };
//!
//! fn checked_divide(numerator: u32, divisor: u32) -> Result<u32, String> {
//!     if unlikely(divisor == 0) {
//!         return Err(outline(|| format!("cannot divide {numerator} by zero")));
//!     }
//!     Ok(numerator.divide_floor(divisor))
//! }
//!
//! assert_eq!(checked_divide(10, 2), Ok(5));
//! assert!(checked_divide(1, 0).is_err());
//! ```
//!
//! On stable these work through a call to an empty `#[cold]` function, which LLVM treats as a
//! branch weight. Built with `RUSTFLAGS="--cfg vulcan_nightly"` on a nightly toolchain, they
//! lower to the `core::hint` intrinsics instead, which emit the branch weight directly and work
//! inside expressions where a call cannot be placed.

// The intrinsics, renamed so they cannot be mistaken for the functions of this module.
#[cfg(vulcan_nightly)]
use core::hint::{
    cold_path as intrinsic_cold_path, likely as intrinsic_likely, unlikely as intrinsic_unlikely,
};

// A hint only weighs a branch: it returns its condition unchanged, for both values. Checked at
// compile time on whichever implementation, stable or nightly, the build selected.
const _: () = assert!(likely(true), "`likely` keeps a true condition");
const _: () = assert!(!likely(false), "`likely` keeps a false condition");
const _: () = assert!(unlikely(true), "`unlikely` keeps a true condition");
const _: () = assert!(!unlikely(false), "`unlikely` keeps a false condition");

// The two hints mirror each other: `likely` of the negated condition always differs from
// `unlikely` of the condition, so a caller may swap one for the other when it swaps the branches.
const _: () = assert!(likely(!true) != unlikely(true), "the hints mirror on a true condition");
const _: () = assert!(likely(!false) != unlikely(false), "the hints mirror on a false condition");

/// Marks the code path that calls it as unlikely to run.
#[cfg_attr(
    vulcan_nightly,
    expect(clippy::inline_always, reason = "it forwards to the intrinsic, which must be inlined")
)]
#[inline(always)]
#[cfg_attr(not(vulcan_nightly), cold)]
pub const fn cold_path() {
    // `core::hint::cold_path` is stable since 1.95, above the 1.93 MSRV, so it stays behind the
    // nightly flag until the MSRV moves.
    #[cfg(vulcan_nightly)]
    #[expect(clippy::incompatible_msrv, reason = "only compiled with `--cfg vulcan_nightly`")]
    intrinsic_cold_path();
}

/// Returns `condition`, hinting that it is usually `true`.
///
/// # Panics
///
/// Panics only if the hint changed its condition, which neither implementation does.
#[expect(clippy::inline_always, reason = "a hint that is not inlined hints nothing")]
#[inline(always)]
#[must_use = "a hint has no effect unless its condition is branched on"]
pub const fn likely(condition: bool) -> bool {
    #[cfg(vulcan_nightly)]
    let hinted = intrinsic_likely(condition);

    #[cfg(not(vulcan_nightly))]
    let hinted = if condition {
        true
    } else {
        cold_path();
        false
    };

    // Free once inlined: the optimizer sees that `hinted` is `condition`.
    assert!(hinted == condition, "a hint returns its condition unchanged");
    hinted
}

/// Returns `condition`, hinting that it is usually `false`.
///
/// # Panics
///
/// Panics only if the hint changed its condition, which neither implementation does.
#[expect(clippy::inline_always, reason = "a hint that is not inlined hints nothing")]
#[inline(always)]
#[must_use = "a hint has no effect unless its condition is branched on"]
pub const fn unlikely(condition: bool) -> bool {
    #[cfg(vulcan_nightly)]
    let hinted = intrinsic_unlikely(condition);

    #[cfg(not(vulcan_nightly))]
    let hinted = if condition {
        cold_path();
        true
    } else {
        false
    };

    // Free once inlined: the optimizer sees that `hinted` is `condition`.
    assert!(hinted == condition, "a hint returns its condition unchanged");
    hinted
}

/// Calls `rare` in a separate, never-inlined, cold function, and returns its value.
///
/// Use it around rarely executed code inside hot functions: formatting an error message,
/// building a diagnostic, growing a buffer.
#[inline(never)]
#[cold]
pub fn outline<Value, Rare>(rare: Rare) -> Value
where
    Rare: FnOnce() -> Value,
{
    rare()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Checks that the hints return their condition unchanged, by calling each at run time with
    /// both values, in both branch positions.
    #[test]
    fn hints_are_identity() {
        for condition in [false, true] {
            assert_eq!(likely(condition), condition, "`likely` keeps the condition");
            assert_eq!(unlikely(condition), condition, "`unlikely` keeps the condition");
            if unlikely(condition) {
                cold_path();
                assert!(condition, "the rare branch runs only on a true condition");
            } else {
                assert!(!condition, "the common branch runs only on a false condition");
            }
        }
    }

    /// Checks that `outline` returns the value of its closure and runs it exactly once, by
    /// counting the calls through a captured counter.
    #[test]
    fn outline_returns_the_closure_value() {
        let mut call_count = 0_u32;
        let value = outline(|| {
            call_count += 1;
            41_u8 + 1
        });
        assert_eq!(value, 42, "the closure value is returned");
        assert_eq!(call_count, 1, "the closure runs once");
        assert_eq!(outline(String::new), "", "a function path works as the closure");
    }
}
