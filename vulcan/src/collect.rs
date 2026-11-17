//! Collecting into a slice without a heap allocation in the common cases.
//!
//! Many call sites gather a few items only to pass them as `&[T]` (hash a type list, look up an
//! interned tuple). Measured in Solar, lists of 0, 1 or 2 items cover about 95% of such calls.
//! [`CollectAndApply::collect_and_apply`] builds those on the stack, and anything longer in a
//! [`SmallVec`] with [`INLINE_ITEM_COUNT`] inline slots, so the heap is touched only beyond that.
//!
//! ```
//! use vulcan::collect::CollectAndApply as _;
//!
//! let sum = [3_u32, 4].into_iter().collect_and_apply(|items| items.iter().sum::<u32>());
//! assert_eq!(sum, 7);
//! let item_count = (0..20).collect_and_apply(<[i32]>::len);
//! assert_eq!(item_count, 20);
//! ```

use smallvec::{SmallVec, smallvec};

/// The number of items gathered without a heap allocation.
///
/// A `usize` because it is the length of the inline array of the [`SmallVec`].
pub const INLINE_ITEM_COUNT: usize = 8;

// The three items read one by one before the `SmallVec` is built must fit inline.
const _: () = assert!(INLINE_ITEM_COUNT >= 3, "the first three items fit inline");

/// Extension for iterators: gather the items and call a function on them as a slice.
pub trait CollectAndApply: Iterator + Sized {
    /// Calls `apply` with the items of the iterator, allocating only beyond the inline items
    /// ([`INLINE_ITEM_COUNT`]).
    ///
    /// # Panics
    ///
    /// Panics if the iterator yields fewer or more items than its `size_hint` promised.
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
        items.extend(self);
        assert!(items.len() >= item_count_min, "the iterator yields its lower bound");
        if let Some(count_max) = item_count_max {
            assert!(items.len() <= count_max, "the iterator stays within its upper bound");
        }
        assert_eq!(items.spilled(), items.len() > INLINE_ITEM_COUNT, "the heap only beyond inline");
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

    /// Collects every length from 0 to 19: the stack cases (0 to 2), the inline cases (3 to 8)
    /// and the spill to the heap all hand the items over in order.
    #[test]
    fn every_length_reaches_the_callback() {
        for item_count in 0..20_u32 {
            let seen = (0..item_count).collect_and_apply(<[u32]>::to_vec);
            assert_eq!(seen, (0..item_count).collect::<Vec<_>>());
        }
    }

    /// Collects the empty input and the boundary of the inline capacity, 8 and 9 items, with an
    /// iterator without an exact size hint, so the `SmallVec` cannot reserve up front.
    #[test]
    fn the_inline_boundary_and_the_empty_input_keep_every_item() {
        let empty = iter::empty::<u8>().collect_and_apply(<[u8]>::len);
        assert_eq!(empty, 0, "the empty input gives the empty slice");
        for item_count in [8_u8, 9_u8] {
            let items = (0..item_count).filter(|_| true).collect_and_apply(<[u8]>::to_vec);
            assert_eq!(items, (0..item_count).collect::<Vec<_>>());
        }
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
