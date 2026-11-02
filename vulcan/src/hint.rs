//! Branch hints and cold-path outlining.
//!
//! The optimizer lays out code for the common case only if it knows which case is common. These
//! helpers mark rare branches so the hot path stays dense in the instruction
//! cache and falls through without a jump.
//!
//! - [`likely`] and [`unlikely`] wrap a condition.
//! - [`cold_path`] marks the current branch as rarely taken.
//! - [`outline`] moves a rare block (error construction, diagnostics) out of the caller, so it does
//!   not bloat the caller's code or its inlining budget.
//!
//! ```
//! use vulcan::hint::{outline, unlikely};
//!
//! fn checked_div(a: u32, b: u32) -> Result<u32, String> {
//!     if unlikely(b == 0) {
//!         return Err(outline(|| format!("cannot divide {a} by zero")));
//!     }
//!     Ok(a / b)
//! }
//!
//! assert_eq!(checked_div(10, 2), Ok(5));
//! assert!(checked_div(1, 0).is_err());
//! ```
//!
//! On stable these work through a call to an empty `#[cold]` function, which LLVM treats as a
//! branch weight.

/// Marks the code path that calls it as unlikely to run.
#[inline(always)]
#[cold]
pub const fn cold_path() {}

/// Returns `b`, hinting that it is usually `true`.
#[inline(always)]
#[must_use]
pub const fn likely(b: bool) -> bool {
    if !b {
        cold_path();
    }
    b
}

/// Returns `b`, hinting that it is usually `false`.
#[inline(always)]
#[must_use]
pub const fn unlikely(b: bool) -> bool {
    if b {
        cold_path();
    }
    b
}

/// Calls `f` in a separate, never-inlined, cold function.
///
/// Use it around rarely executed code inside hot functions: formatting an error message,
/// building a diagnostic, growing a buffer.
#[inline(never)]
#[cold]
pub fn outline<R>(f: impl FnOnce() -> R) -> R {
    f()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_are_identity() {
        for b in [false, true] {
            assert_eq!(likely(b), b);
            assert_eq!(unlikely(b), b);
        }
        cold_path();
        assert_eq!(outline(|| 41 + 1), 42);
    }
}
