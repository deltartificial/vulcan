//! Compile-time size budgets and compact source spans.
//!
//! Hot types are copied, stored in vectors and passed in registers; a few extra bytes multiply
//! across millions of nodes and push data out of cache. Pinning the size of hot types makes growth
//! a review decision instead of a silent regression. Vulcan does it at compile time with the
//! `static_assert_size!` macro: the build fails as soon as a pinned type changes size.
//!
//! A [`Span`] holds two `u32` offsets in 8 bytes, so a span travels in a single register and
//! hashes as one integer. It is 4-aligned, so it adds no padding next to other `u32` fields.

// `Interned` holds a pointer: its size is pinned on 64-bit targets only.
#[cfg(target_pointer_width = "64")]
use crate::intern::Interned;
use crate::{convert::usize_from_u32, intern::Symbol};
use core::{
    fmt,
    hash::{Hash, Hasher},
    ops::Range,
};

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

/// A byte range `start..end` into a source, in 8 bytes.
///
/// ```
/// use vulcan::layout::{Span, SpanBounds};
///
/// let span = Span::new(SpanBounds { start: 4, end: 10 });
/// assert_eq!((span.start(), span.end(), span.len()), (4, 10, 6));
/// let later = Span::new(SpanBounds { start: 12, end: 20 });
/// assert_eq!(span.cover(later), Span::new(SpanBounds { start: 4, end: 20 }));
/// assert_eq!("let answer = 42;".get(span.range()), Some("answer"));
/// ```
///
/// Spans order by start, then by end, as source positions read: the derived order compares the
/// fields in declaration order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Span {
    /// The offset of the first byte. Invariant: `start <= end`.
    start: u32,
    /// The offset one past the last byte (exclusive).
    end: u32,
}

/// The offsets of a [`Span`], named so that they cannot be swapped at a call site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpanBounds {
    /// The offset of the first byte.
    pub start: u32,
    /// The offset one past the last byte (exclusive).
    pub end: u32,
}

impl Span {
    /// The empty span at offset 0.
    pub const DUMMY: Self = Self::new(SpanBounds { start: 0, end: 0 });

    /// Creates the span `bounds.start..bounds.end`.
    ///
    /// # Panics
    ///
    /// Panics if `bounds.start > bounds.end`: swapped offsets are a bug at the call site, not an
    /// empty span.
    #[inline]
    #[must_use = "a span has no effect unless stored"]
    #[track_caller]
    pub const fn new(bounds: SpanBounds) -> Self {
        assert!(bounds.start <= bounds.end, "span start after its end");
        Self { start: bounds.start, end: bounds.end }
    }

    /// Returns the start offset.
    ///
    /// # Panics
    ///
    /// Panics only if the span ends before it starts, which [`new`](Self::new) rules out.
    #[inline]
    #[must_use = "an accessor has no side effect"]
    pub const fn start(self) -> u32 {
        assert!(self.start <= self.end, "a span never ends before it starts");
        self.start
    }

    /// Returns the end offset (exclusive).
    ///
    /// # Panics
    ///
    /// Panics only if the span ends before it starts, which [`new`](Self::new) rules out.
    #[inline]
    #[must_use = "an accessor has no side effect"]
    pub const fn end(self) -> u32 {
        assert!(self.start <= self.end, "a span never ends before it starts");
        self.end
    }

    /// Returns the length in bytes.
    ///
    /// # Panics
    ///
    /// Panics only if the span ends before it starts, which [`new`](Self::new) rules out.
    #[inline]
    #[must_use = "an accessor has no side effect"]
    pub const fn len(self) -> u32 {
        assert!(self.start <= self.end, "a span never ends before it starts");
        let length = self.end - self.start;
        assert!(length <= self.end, "a span is no longer than its end offset");
        length
    }

    /// Returns `true` if the span covers no bytes.
    ///
    /// # Panics
    ///
    /// Panics only if the span ends before it starts, which [`new`](Self::new) rules out.
    #[inline]
    #[must_use = "an accessor has no side effect"]
    pub const fn is_empty(self) -> bool {
        assert!(self.start <= self.end, "a span never ends before it starts");
        self.start == self.end
    }

    /// Returns the smallest span covering both `self` and `other`.
    ///
    /// # Panics
    ///
    /// Panics only if an input span ends before it starts, which [`new`](Self::new) rules out.
    #[inline]
    #[must_use = "the covering span is returned, `self` is unchanged"]
    pub const fn cover(self, other: Self) -> Self {
        let start = if self.start < other.start { self.start } else { other.start };
        let end = if self.end > other.end { self.end } else { other.end };
        let covering = Self::new(SpanBounds { start, end });
        assert!(covering.start <= self.start, "the cover starts no later than `self`");
        assert!(covering.start <= other.start, "the cover starts no later than `other`");
        assert!(covering.end >= self.end, "the cover ends no earlier than `self`");
        assert!(covering.end >= other.end, "the cover ends no earlier than `other`");
        covering
    }

    /// Returns the span as a `usize` range, to slice the source text.
    ///
    /// # Panics
    ///
    /// Panics only if the span ends before it starts, which [`new`](Self::new) rules out.
    #[inline]
    #[must_use = "a conversion has no side effect"]
    pub const fn range(self) -> Range<usize> {
        assert!(self.start <= self.end, "a span never ends before it starts");
        usize_from_u32(self.start)..usize_from_u32(self.end)
    }
}

impl Hash for Span {
    /// Hashes both offsets as one `u64`: one round of the hasher instead of two.
    #[inline]
    fn hash<State>(&self, state: &mut State)
    where
        State: Hasher,
    {
        assert!(self.start <= self.end, "a span never ends before it starts");
        // Equal spans have equal offsets, so they write equal integers, as `Hash` requires.
        state.write_u64((u64::from(self.end) << 32_u32) | u64::from(self.start));
    }
}

impl fmt::Debug for Span {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        assert!(self.start <= self.end, "a span never ends before it starts");
        write!(formatter, "Span({}..{})", self.start, self.end)
    }
}

// Size budgets for the hot types of the crate. Changing one of these numbers is a deliberate,
// reviewed decision.
const _: () = assert!(Span::DUMMY.is_empty(), "the dummy span covers nothing");
static_assert_size!(Span, 8);
static_assert_size!(Option<Span>, 12);
static_assert_size!(Symbol, 4);
static_assert_size!(Option<Symbol>, 4);
static_assert_size!((Symbol, Span), 12);
const _: () = assert!(align_of::<Span>() == 4, "`Span` must stay 4-aligned");
#[cfg(target_pointer_width = "64")]
static_assert_size!(Interned<'static, u128>, 8);
#[cfg(target_pointer_width = "64")]
static_assert_size!(Option<Interned<'static, u128>>, 8);

#[cfg(test)]
mod tests {
    use super::*;
    use core::hash::BuildHasher as _;
    use rustc_hash::FxBuildHasher;

    /// Returns the span `start..end`.
    fn span(start: u32, end: u32) -> Span {
        Span::new(SpanBounds { start, end })
    }

    /// Checks the accessors, the order and the debug output on ordinary spans, by comparing them
    /// with the offsets they were built from.
    #[test]
    fn spans_keep_their_offsets_and_order() {
        let nine = span(3, 9);
        assert_eq!((nine.start(), nine.end(), nine.len()), (3, 9, 6));
        assert!(span(1, 100) < span(2, 3), "ordered by start first");
        assert!(span(1, 2) < span(1, 3), "then by end");
        assert_eq!(format!("{nine:?}"), "Span(3..9)");
        assert_eq!("let answer = 42;".get(span(4, 10).range()), Some("answer"));
    }

    /// Checks the boundaries of a span: equal offsets give an empty span, offsets one apart a
    /// span of one byte, and the full `u32` range a span of `u32::MAX` bytes.
    #[test]
    fn spans_at_the_boundaries() {
        assert!(Span::DUMMY.is_empty(), "the dummy span is empty");
        assert!(span(7, 7).is_empty(), "equal offsets are a valid, empty span");
        assert_eq!(span(7, 8).len(), 1, "offsets one apart cover one byte");
        assert!(!span(7, 8).is_empty(), "a one-byte span is not empty");
        assert_eq!(span(0, u32::MAX).len(), u32::MAX, "the widest span");
        assert_eq!(
            span(u32::MAX, u32::MAX).range(),
            usize_from_u32(u32::MAX)..usize_from_u32(u32::MAX)
        );
    }

    /// Checks that `cover` spans disjoint, nested and touching inputs, in either order, by
    /// comparing with the expected covering span.
    #[test]
    fn cover_spans_both_inputs() {
        assert_eq!(span(4, 10).cover(span(12, 20)), span(4, 20), "disjoint");
        assert_eq!(span(12, 20).cover(span(4, 10)), span(4, 20), "disjoint, swapped");
        assert_eq!(span(2, 30).cover(span(4, 10)), span(2, 30), "nested");
        assert_eq!(span(2, 4).cover(span(4, 9)), span(2, 9), "touching");
        assert_eq!(Span::DUMMY.cover(Span::DUMMY), Span::DUMMY, "empty with empty");
    }

    /// Checks that equal spans hash equally and that the two offsets are not interchangeable, by
    /// hashing spans with the same offsets in both positions.
    #[test]
    fn hash_follows_equality() {
        let hasher = FxBuildHasher;
        assert_eq!(hasher.hash_one(span(3, 9)), hasher.hash_one(span(3, 9)), "equal spans");
        assert_ne!(hasher.hash_one(span(3, 9)), hasher.hash_one(span(9, 9)), "different spans");
        assert_ne!(hasher.hash_one(span(0, 1)), hasher.hash_one(span(1, 1)), "start matters");
    }

    /// Builds a span from swapped offsets, which must crash rather than produce an empty span.
    #[test]
    #[should_panic(expected = "span start after its end")]
    fn rejects_swapped_offsets() {
        let _span = span(8, 7);
    }

    /// Builds a span whose end is one below its start at the top of the `u32` range, the
    /// boundary of the invalid inputs.
    #[test]
    #[should_panic(expected = "span start after its end")]
    fn rejects_swapped_offsets_at_the_top() {
        let _span = span(u32::MAX, u32::MAX - 1);
    }
}
