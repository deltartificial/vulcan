//! The never type on stable, and helpers to unwrap infallible results.
//!
//! A `Result<T, Never>` or `ControlFlow<Never, T>` cannot hold its failure variant, so the
//! compiler removes that path entirely: an infallible visitor or fold written against a generic
//! `BreakValue` pays nothing for the early-exit plumbing it does not use.
//!
//! ```
//! use core::ops::ControlFlow;
//! use vulcan::never::{IntoOk as _, Never, UnwrapInfallible as _};
//!
//! fn sum(values: &[u32]) -> ControlFlow<Never, u32> {
//!     ControlFlow::Continue(values.iter().sum())
//! }
//!
//! assert_eq!(sum(&[1, 2, 3]).into_value(), 6);
//! let parsed: Result<u8, Never> = Ok(7);
//! assert_eq!(parsed.into_ok(), 7);
//! ```

use crate::static_assert_size;
use core::ops::ControlFlow;

/// Names the return type of a function pointer, which is how `!` can be spelled on stable.
#[doc(hidden)]
pub trait FnReturn {
    /// The return type.
    type Output;
}

impl<Return> FnReturn for fn() -> Return {
    type Output = Return;
}

/// The [`!`](primitive@never) type, usable on stable Rust.
pub type Never = <fn() -> ! as FnReturn>::Output;

// The promise of this module, checked at compile time: an uninhabited failure variant takes no
// space, so a container that cannot fail is exactly as large as its value.
static_assert_size!(Never, 0);
static_assert_size!(Result<u32, Never>, 4);
static_assert_size!(ControlFlow<Never, u32>, 4);
static_assert_size!(Result<(), Never>, 0);

/// Extracts the value of a container whose failure case is uninhabited.
pub trait UnwrapInfallible {
    /// The value type.
    type Value;

    /// Returns the value. Never panics: the other variant cannot exist.
    fn into_value(self) -> Self::Value;
}

impl<Value> UnwrapInfallible for ControlFlow<Never, Value> {
    type Value = Value;

    #[expect(
        clippy::inline_always,
        reason = "a match on an uninhabited variant compiles to nothing"
    )]
    #[inline(always)]
    fn into_value(self) -> Value {
        match self {
            Self::Continue(value) => value,
            Self::Break(never) => never,
        }
    }
}

/// Extension for `Result<T, Never>`, named like the unstable `Result::into_ok`.
pub trait IntoOk<Value> {
    /// Returns the `Ok` value. Never panics: the `Err` variant cannot exist.
    fn into_ok(self) -> Value;
}

impl<Value> IntoOk<Value> for Result<Value, Never> {
    #[expect(
        clippy::inline_always,
        reason = "a match on an uninhabited variant compiles to nothing"
    )]
    #[inline(always)]
    fn into_ok(self) -> Value {
        match self {
            Ok(value) => value,
            Err(never) => never,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Checks that both helpers return the wrapped value unchanged, at the bounds of `u32` and
    /// for a zero-sized value, by unwrapping containers built from known values.
    #[test]
    fn helpers_return_the_value() {
        for value in [0_u32, 1, u32::MAX] {
            assert_eq!(IntoOk::into_ok(Ok::<_, Never>(value)), value, "`into_ok` keeps the value");
            let flow = ControlFlow::<Never, u32>::Continue(value);
            assert_eq!(flow.into_value(), value, "`into_value` keeps the value");
        }
        let unit: () = IntoOk::into_ok(Ok::<(), Never>(()));
        assert_eq!(unit, (), "a zero-sized value unwraps too");
    }
}
