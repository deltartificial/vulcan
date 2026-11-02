//! Collecting into a slice without a heap allocation in the common cases.
//!
//! Many call sites gather a few items only to pass them as `&[T]` (hash a type list, look up an
//! interned tuple). Measured in Solar, lists of 0, 1 or 2 items cover about 95% of such calls.
//! [`CollectAndApply::collect_and_apply`] builds those on the stack, and anything longer in a
//! [`SmallVec`] with 8 inline slots, so the heap is touched only beyond 8 items.
//!
//! ```
//! use vulcan::collect::CollectAndApply;
//!
//! let sum = [3_u32, 4].into_iter().collect_and_apply(|items| items.iter().sum::<u32>());
//! assert_eq!(sum, 7);
//! let len = (0..20).collect_and_apply(<[i32]>::len);
//! assert_eq!(len, 20);
//! ```

use smallvec::SmallVec;

/// Extension for iterators: gather the items and call a function on them as a slice.
pub trait CollectAndApply: Iterator + Sized {
    /// Calls `f` with the items of the iterator, allocating only beyond 8 items.
    #[inline]
    fn collect_and_apply<R>(mut self, f: impl FnOnce(&[Self::Item]) -> R) -> R {
        let Some(t0) = self.next() else { return f(&[]) };
        let Some(t1) = self.next() else { return f(&[t0]) };
        let Some(t2) = self.next() else { return f(&[t0, t1]) };
        let mut items: SmallVec<[Self::Item; 8]> = smallvec::smallvec![t0, t1, t2];
        items.extend(self);
        f(&items)
    }
}

impl<I: Iterator> CollectAndApply for I {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_length_reaches_the_callback() {
        for len in 0..20_u32 {
            let seen = (0..len).collect_and_apply(<[u32]>::to_vec);
            assert_eq!(seen, (0..len).collect::<Vec<_>>());
        }
    }
}
