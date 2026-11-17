//! Collecting a few items into a slice without a heap allocation.
//!
//! Many call sites gather a few items only to pass them as a slice (hash a type list, look up an
//! interned tuple). Measured in Solar, lists of 0, 1 or 2 items cover about 95% of such calls.
//! The method [`CollectAndApply::collect_and_apply`] builds those on the stack, and anything
//! longer in the inline slots of a [`SmallVec`]. Their number, [`INLINE_ITEM_COUNT`], is a hard
//! limit: a longer iterator is a programmer error, so the heap is never touched. Gather a longer
//! list into a buffer allocated at startup and pass it as a slice instead.
//!
//! ```
//! use vulcan::collect::CollectAndApply as _;
//!
//! let sum = [3_u32, 4].into_iter().collect_and_apply(|items| items.iter().sum::<u32>());
//! assert_eq!(sum, 7);
//! let item_count = (0..8).collect_and_apply(<[i32]>::len);
//! assert_eq!(item_count, 8);
//! ```

use smallvec::{SmallVec, smallvec};

/// The number of items gathered at most, all of them without a heap allocation.
///
/// A `usize` because it is the length of the inline array of the [`SmallVec`].
pub const INLINE_ITEM_COUNT: usize = 8;

// The three items read one by one before the `SmallVec` is built must fit inline.
const _: () = assert!(INLINE_ITEM_COUNT >= 3, "the first three items fit inline");

/// Extension for iterators: gather the items and call a function on them as a slice.
pub trait CollectAndApply: Iterator + Sized {
    /// Calls `apply` with the items of the iterator, at most [`INLINE_ITEM_COUNT`] of them, without
    /// allocating.
    ///
    /// # Panics
    ///
    /// Panics if the iterator yields more than [`INLINE_ITEM_COUNT`] items, or fewer or more items
    /// than its `size_hint` promised.
    #[inline]
    #[track_caller]
    fn collect_and_apply<Output, Apply>(mut self, apply: Apply) -> Output
    where
        Apply: FnOnce(&[Self::Item]) -> Output,
    {
        let (item_count_min, item_count_max) = self.size_hint();
        if let Some(count_max) = item_count_max {
            assert!(item_count_min <= count_max, "the size hint is a valid range");
        }
        let Some(first) = self.next() else {
            assert_eq!(item_count_min, 0, "an empty iterator promised no item");
            return apply(&[]);
        };
        let Some(second) = self.next() else {
            assert!(item_count_min <= 1, "a one-item iterator promised at most one item");
            return apply(&[first]);
        };
        let Some(third) = self.next() else {
            assert!(item_count_min <= 2, "a two-item iterator promised at most two items");
            return apply(&[first, second]);
        };
        let mut items: SmallVec<[Self::Item; INLINE_ITEM_COUNT]> = smallvec![first, second, third];
        // Bounded: the inline slots left after the first three items.
        for item in self.by_ref().take(INLINE_ITEM_COUNT - 3) {
            items.push(item);
        }
        assert!(self.next().is_none(), "an iterator yields at most `INLINE_ITEM_COUNT` items");
        assert!(items.len() >= item_count_min, "the iterator yields its lower bound");
        if let Some(count_max) = item_count_max {
            assert!(items.len() <= count_max, "the iterator stays within its upper bound");
        }
        assert!(items.len() <= INLINE_ITEM_COUNT, "the items fit in the inline slots");
        assert!(!items.spilled(), "the heap is never touched");
        apply(&items)
    }
}

impl<Items> CollectAndApply for Items where Items: Iterator {}

#[cfg(test)]
mod tests {
    use super::*;
    use core::iter;

    /// An iterator that yields `yielded` items while its size hint promises `promised`.
    struct Lying {
        /// The number of items left to yield.
        yielded: u32,
        /// The exact item count the size hint reports.
        promised: usize,
    }

    impl Iterator for Lying {
        type Item = u32;

        fn next(&mut self) -> Option<u32> {
            self.yielded = self.yielded.checked_sub(1)?;
            Some(self.yielded)
        }

        fn size_hint(&self) -> (usize, Option<usize>) {
            (self.promised, Some(self.promised))
        }
    }

    /// Collects every length from 0 to 8: the stack cases (0 to 2) and the inline cases (3 to 8)
    /// all hand the items over in order.
    #[test]
    fn every_length_reaches_the_callback() {
        for item_count in 0..=8_u32 {
            let seen = (0..item_count).collect_and_apply(<[u32]>::to_vec);
            assert_eq!(seen, (0..item_count).collect::<Vec<_>>());
        }
    }

    /// Collects the empty input and the largest input, 8 items, with an iterator without an
    /// exact size hint: both ends of the valid range keep every item.
    #[test]
    fn the_inline_boundary_and_the_empty_input_keep_every_item() {
        let empty = iter::empty::<u8>().collect_and_apply(<[u8]>::len);
        assert_eq!(empty, 0, "the empty input gives the empty slice");
        let items = (0..8_u8).filter(|_| true).collect_and_apply(<[u8]>::to_vec);
        assert_eq!(items, (0..8_u8).collect::<Vec<_>>(), "the last count that fits");
    }

    /// Collects 9 items, one above the limit, which must crash rather than reach the heap.
    #[test]
    #[should_panic(expected = "an iterator yields at most `INLINE_ITEM_COUNT` items")]
    fn an_iterator_above_the_inline_count_panics() {
        let _item_count = (0..9_u8).filter(|_| true).collect_and_apply(<[u8]>::len);
    }

    /// Collects an iterator whose size hint promises more items than it yields, a broken
    /// iterator that must crash rather than pass on a short list.
    #[test]
    #[should_panic(expected = "the iterator yields its lower bound")]
    fn an_iterator_short_of_its_size_hint_panics() {
        let lying = Lying { yielded: 4, promised: 9 };
        let _item_count = lying.collect_and_apply(<[u32]>::len);
    }

    /// Collects an empty iterator whose size hint promises an item.
    #[test]
    #[should_panic(expected = "an empty iterator promised no item")]
    fn an_empty_iterator_that_promised_items_panics() {
        let lying = Lying { yielded: 0, promised: 1 };
        let _item_count = lying.collect_and_apply(<[u32]>::len);
    }
}
