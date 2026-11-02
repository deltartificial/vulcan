//! The never type on stable, and helpers to unwrap infallible results.
//!
//! A `Result<T, Never>` or `ControlFlow<Never, T>` cannot hold its failure variant, so the
//! compiler removes that path entirely: an infallible visitor or fold written against a generic
//! `BreakValue` pays nothing for the early-exit plumbing it does not use.
//!
//! ```
//! use std::ops::ControlFlow;
//! use vulcan::never::{IntoOk, Never, UnwrapInfallible};
//!
//! fn sum(values: &[u32]) -> ControlFlow<Never, u32> {
//!     ControlFlow::Continue(values.iter().sum())
//! }
//!
//! assert_eq!(sum(&[1, 2, 3]).into_value(), 6);
//! let parsed: Result<u8, Never> = Ok(7);
//! assert_eq!(parsed.into_ok(), 7);
//! ```

/// Names the return type of a function pointer, which is how `!` can be spelled on stable.
#[doc(hidden)]
pub trait FnReturn {
    /// The return type.
    type Output;
}

impl<T> FnReturn for fn() -> T {
    type Output = T;
}

/// The [`!`](primitive@never) type, usable on stable Rust.
pub type Never = <fn() -> ! as FnReturn>::Output;

/// Extracts the value of a container whose failure case is uninhabited.
pub trait UnwrapInfallible {
    /// The value type.
    type Value;

    /// Returns the value. Never panics: the other variant cannot exist.
    fn into_value(self) -> Self::Value;
}

impl<T> UnwrapInfallible for std::ops::ControlFlow<Never, T> {
    type Value = T;

    #[inline(always)]
    fn into_value(self) -> T {
        match self {
            Self::Continue(value) => value,
            Self::Break(never) => never,
        }
    }
}

/// Extension for `Result<T, Never>`, named like the unstable `Result::into_ok`.
pub trait IntoOk<T> {
    /// Returns the `Ok` value. Never panics: the `Err` variant cannot exist.
    fn into_ok(self) -> T;
}

impl<T> IntoOk<T> for Result<T, Never> {
    #[inline(always)]
    fn into_ok(self) -> T {
        match self {
            Ok(value) => value,
            Err(never) => never,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_is_zero_sized_and_free() {
        assert_eq!(size_of::<Never>(), 0);
        assert_eq!(size_of::<Result<u32, Never>>(), 4);
        assert_eq!(Ok::<_, Never>(3).into_ok(), 3);
    }
}
