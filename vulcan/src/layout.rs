//! Compile-time size budgets and compact source spans.
//!
//! Hot types are copied, stored in vectors and passed in registers; a few extra bytes multiply
//! across millions of nodes and push data out of cache. Solar pins the size of every AST node in a
//! test, so growth becomes a review decision instead of a silent regression. Vulcan does the same
//! at compile time with [`static_assert_size!`](crate::static_assert_size): the build fails as
//! soon as a pinned type changes size.
//!
//! [`Span`] follows Solar's layout: two `u32` offsets packed into one `u64`, so a span travels in
//! a single register and derived comparisons are a single integer comparison.

use std::{fmt, ops::Range};

/// Fails compilation unless `size_of::<$ty>() == $size`.
///
/// The error shows both sizes (as array lengths), which tells the reviewer by how much the type
/// changed.
///
/// ```
/// vulcan::static_assert_size!(u64, 8);
/// vulcan::static_assert_size!(Option<core::num::NonZeroU32>, 4);
/// ```
///
/// ```compile_fail
/// vulcan::static_assert_size!(Option<u32>, 4);
/// ```
#[macro_export]
macro_rules! static_assert_size {
    ($ty:ty, $size:expr $(,)?) => {
        const _: [(); $size] = [(); ::core::mem::size_of::<$ty>()];
    };
}

/// A byte range `lo..hi` into a source, packed into 8 bytes.
///
/// ```
/// use vulcan::layout::Span;
///
/// let span = Span::new(10, 4);
/// assert_eq!((span.lo(), span.hi()), (4, 10));
/// assert_eq!(span.to(Span::new(12, 20)), Span::new(4, 20));
/// assert_eq!(&"let answer = 42;"[Span::new(4, 10).range()], "answer");
/// ```
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Span {
    // `hi` in the high half and `lo` in the low half, so the derived `Eq` and `Hash` work on one
    // integer.
    data: u64,
}

impl Span {
    /// The empty span at offset 0.
    pub const DUMMY: Self = Self { data: 0 };

    /// Creates a span from two offsets in any order.
    #[inline]
    #[must_use]
    pub const fn new(a: u32, b: u32) -> Self {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        Self { data: ((hi as u64) << 32) | lo as u64 }
    }

    /// Returns the start offset.
    #[inline]
    #[must_use]
    pub const fn lo(self) -> u32 {
        self.data as u32
    }

    /// Returns the end offset (exclusive).
    #[inline]
    #[must_use]
    pub const fn hi(self) -> u32 {
        (self.data >> 32) as u32
    }

    /// Returns the length in bytes.
    #[inline]
    #[must_use]
    pub const fn len(self) -> u32 {
        self.hi() - self.lo()
    }

    /// Returns `true` if the span covers no bytes.
    #[inline]
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.lo() == self.hi()
    }

    /// Returns the smallest span covering both `self` and `other`.
    #[inline]
    #[must_use]
    pub const fn to(self, other: Self) -> Self {
        let lo = if self.lo() < other.lo() { self.lo() } else { other.lo() };
        let hi = if self.hi() > other.hi() { self.hi() } else { other.hi() };
        Self::new(lo, hi)
    }

    /// Returns the span as a `usize` range, to slice the source text.
    #[inline]
    #[must_use]
    pub const fn range(self) -> Range<usize> {
        self.lo() as usize..self.hi() as usize
    }
}

impl PartialOrd for Span {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Span {
    /// Orders by start, then by end, as source positions read.
    #[inline]
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.lo().cmp(&other.lo()).then(self.hi().cmp(&other.hi()))
    }
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Span({}..{})", self.lo(), self.hi())
    }
}

// Size budgets for the hot types of the crate. Changing one of these numbers is a deliberate,
// reviewed decision.
static_assert_size!(Span, 8);
static_assert_size!(Option<Span>, 16);
static_assert_size!(crate::intern::Symbol, 4);
static_assert_size!(Option<crate::intern::Symbol>, 4);
static_assert_size!((crate::intern::Symbol, Span), 16);
#[cfg(target_pointer_width = "64")]
static_assert_size!(crate::intern::Interned<'static, u128>, 8);
#[cfg(target_pointer_width = "64")]
static_assert_size!(Option<crate::intern::Interned<'static, u128>>, 8);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_and_orders() {
        let span = Span::new(3, 9);
        assert_eq!((span.lo(), span.hi(), span.len()), (3, 9, 6));
        assert!(Span::new(1, 100) < Span::new(2, 3), "ordered by start first");
        assert!(Span::new(1, 2) < Span::new(1, 3));
        assert!(Span::DUMMY.is_empty());
        assert_eq!(format!("{span:?}"), "Span(3..9)");
        assert_eq!(Span::new(u32::MAX, 0).len(), u32::MAX);
    }
}
