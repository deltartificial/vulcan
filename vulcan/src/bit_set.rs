//! A fixed-size bit set indexed by typed indices.
//!
//! Dataflow, liveness and "visited" sets over AST or IR nodes are dense: most indices in a range
//! are candidates. A bit per index beats a hash set by an order of magnitude in memory, and set
//! operations work 64 indices at a time. Up to 128 indices fit inline, with no heap allocation.
//!
//! ```
//! use vulcan::bit_set::DenseBitSet;
//!
//! vulcan::newtype_index! {
//!     /// A basic block.
//!     pub struct BlockId;
//! }
//!
//! let mut live = DenseBitSet::<BlockId>::new_empty(100);
//! assert!(live.insert(BlockId::from_u32(3)));
//! assert!(!live.insert(BlockId::from_u32(3)), "already present");
//! assert!(live.contains(BlockId::from_u32(3)));
//! assert_eq!(live.iter().map(BlockId::as_u32).collect::<Vec<_>>(), [3]);
//! ```

use crate::index::Idx;
use smallvec::SmallVec;
use std::{fmt, marker::PhantomData};

type Word = u64;
const WORD_BITS: usize = Word::BITS as usize;

/// Ties a set to its index type without storing an index.
type IndexMarker<I> = PhantomData<fn() -> I>;

/// A set of indices in `0..domain_size`, one bit each.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct DenseBitSet<I> {
    domain_size: usize,
    words: SmallVec<[Word; 2]>,
    _index: IndexMarker<I>,
}

impl<I: Idx> DenseBitSet<I> {
    /// Creates an empty set for indices in `0..domain_size`.
    #[must_use]
    pub fn new_empty(domain_size: usize) -> Self {
        let words = smallvec::smallvec![0; domain_size.div_ceil(WORD_BITS)];
        Self { domain_size, words, _index: PhantomData }
    }

    /// Creates a set containing every index in `0..domain_size`.
    #[must_use]
    pub fn new_filled(domain_size: usize) -> Self {
        let mut set = Self::new_empty(domain_size);
        set.insert_all();
        set
    }

    /// Returns the size of the index domain.
    #[must_use]
    pub const fn domain_size(&self) -> usize {
        self.domain_size
    }

    #[inline]
    #[track_caller]
    fn locate(&self, index: I) -> (usize, Word) {
        let i = index.index();
        assert!(i < self.domain_size, "index {i} out of bit set domain {}", self.domain_size);
        (i / WORD_BITS, 1 << (i % WORD_BITS))
    }

    /// Inserts `index`; returns `true` if it was absent.
    #[inline]
    #[track_caller]
    pub fn insert(&mut self, index: I) -> bool {
        let (word, mask) = self.locate(index);
        let before = self.words[word];
        self.words[word] = before | mask;
        before & mask == 0
    }

    /// Removes `index`; returns `true` if it was present.
    #[inline]
    #[track_caller]
    pub fn remove(&mut self, index: I) -> bool {
        let (word, mask) = self.locate(index);
        let before = self.words[word];
        self.words[word] = before & !mask;
        before & mask != 0
    }

    /// Returns `true` if `index` is in the set.
    #[inline]
    #[must_use]
    #[track_caller]
    pub fn contains(&self, index: I) -> bool {
        let (word, mask) = self.locate(index);
        self.words[word] & mask != 0
    }

    /// Returns the number of indices in the set.
    #[must_use]
    pub fn count(&self) -> usize {
        self.words.iter().map(|word| word.count_ones() as usize).sum()
    }

    /// Returns `true` if the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.words.iter().all(|&word| word == 0)
    }

    /// Removes every index, keeping the allocation.
    pub fn clear(&mut self) {
        self.words.fill(0);
    }

    /// Inserts every index of the domain.
    pub fn insert_all(&mut self) {
        self.words.fill(Word::MAX);
        self.clear_excess_bits();
    }

    /// Adds every index of `other`; returns `true` if `self` changed.
    ///
    /// # Panics
    ///
    /// Panics if the domains differ.
    #[track_caller]
    pub fn union(&mut self, other: &Self) -> bool {
        self.combine(other, |a, b| a | b)
    }

    /// Keeps only the indices also in `other`; returns `true` if `self` changed.
    ///
    /// # Panics
    ///
    /// Panics if the domains differ.
    #[track_caller]
    pub fn intersect(&mut self, other: &Self) -> bool {
        self.combine(other, |a, b| a & b)
    }

    /// Removes every index of `other`; returns `true` if `self` changed.
    ///
    /// # Panics
    ///
    /// Panics if the domains differ.
    #[track_caller]
    pub fn subtract(&mut self, other: &Self) -> bool {
        self.combine(other, |a, b| a & !b)
    }

    #[track_caller]
    fn combine(&mut self, other: &Self, op: impl Fn(Word, Word) -> Word) -> bool {
        assert_eq!(self.domain_size, other.domain_size, "bit sets over different domains");
        let mut changed = false;
        for (word, &rhs) in self.words.iter_mut().zip(&other.words) {
            let new = op(*word, rhs);
            changed |= new != *word;
            *word = new;
        }
        changed
    }

    /// Iterates over the indices in the set, in increasing order.
    pub fn iter(&self) -> impl Iterator<Item = I> + '_ {
        self.words.iter().enumerate().flat_map(|(word_index, &word)| {
            let base = word_index * WORD_BITS;
            BitIter(word).map(move |bit| I::from_usize(base + bit))
        })
    }

    fn clear_excess_bits(&mut self) {
        let used = self.domain_size % WORD_BITS;
        if used != 0 &&
            let Some(last) = self.words.last_mut()
        {
            *last &= (1 << used) - 1;
        }
    }
}

/// Yields the positions of the set bits of a word, lowest first.
struct BitIter(Word);

impl Iterator for BitIter {
    type Item = usize;

    #[inline]
    fn next(&mut self) -> Option<usize> {
        if self.0 == 0 {
            return None;
        }
        let bit = self.0.trailing_zeros() as usize;
        // Clears the lowest set bit.
        self.0 &= self.0 - 1;
        Some(bit)
    }
}

impl<I: Idx> fmt::Debug for DenseBitSet<I> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    crate::newtype_index! {
        struct Node;
    }

    #[test]
    fn small_sets_stay_inline() {
        let set = DenseBitSet::<Node>::new_empty(128);
        assert!(!set.words.spilled());
    }

    #[test]
    fn operations_match_a_reference_set() {
        let mut a = DenseBitSet::<Node>::new_empty(200);
        let mut b = DenseBitSet::<Node>::new_empty(200);
        for i in (0..200).step_by(3) {
            a.insert(Node::from_u32(i));
        }
        for i in (0..200).step_by(5) {
            b.insert(Node::from_u32(i));
        }
        let mut union = a.clone();
        assert!(union.union(&b));
        assert!(!union.union(&b), "second union changes nothing");
        let mut inter = a.clone();
        inter.intersect(&b);
        assert_eq!(
            inter.iter().map(Node::as_u32).collect::<Vec<_>>(),
            (0..200).step_by(15).collect::<Vec<_>>()
        );
        let mut diff = a.clone();
        diff.subtract(&b);
        assert_eq!(diff.count() + inter.count(), a.count());
        let full = DenseBitSet::<Node>::new_filled(70);
        assert_eq!(full.count(), 70);
        assert!(a.remove(Node::from_u32(3)) && !a.contains(Node::from_u32(3)));
    }
}
