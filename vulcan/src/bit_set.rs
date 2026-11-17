//! A fixed-size bit set indexed by typed indices.
//!
//! Dataflow, liveness and "visited" sets over AST or IR nodes are dense: most indices in a range
//! are candidates. A bit per index beats a hash set by an order of magnitude in memory, and set
//! operations work 64 indices at a time. Up to 128 indices fit inline, with no heap allocation.
//! The words are allocated once, when the set is created, and never reallocated.
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

use crate::{
    convert::{u32_from_usize, usize_from_u32},
    division::Divide as _,
    index::TypedIndex,
};
use core::{fmt, marker::PhantomData};
use smallvec::SmallVec;

/// The unit of storage: one bit per index, 64 indices per word.
type Word = u64;

/// Bits per word.
const WORD_BITS: u32 = Word::BITS;

/// Bits per word, as a slice offset.
const WORD_BITS_USIZE: usize = usize_from_u32(WORD_BITS);

/// Words stored inline before the set spills to the heap: 128 indices.
const WORDS_INLINE: usize = 2;

// The bit position inside a word is a remainder by `WORD_BITS`, which must be a mask.
const _: () = assert!(WORD_BITS.is_power_of_two(), "`WORD_BITS` must be a power of two");
const _: () = assert!(WORDS_INLINE * WORD_BITS_USIZE == 128, "128 indices fit inline");

/// Ties a set to its index type without storing an index.
type IndexMarker<I> = PhantomData<fn() -> I>;

/// A set of indices in `0..domain_count`, one bit each.
///
/// Invariant: the bits of the last word beyond `domain_count` are always zero, so counting and
/// comparing sets never sees indices outside the domain.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct DenseBitSet<I> {
    /// The number of indices the set can hold: its indices are `0..domain_count`.
    domain_count: u32,
    /// One bit per index of the domain, lowest index in the lowest bit of the first word.
    words: SmallVec<[Word; WORDS_INLINE]>,
    /// The index type.
    _index: IndexMarker<I>,
}

impl<I: TypedIndex> DenseBitSet<I> {
    /// Creates an empty set for indices in `0..domain_count`.
    ///
    /// # Panics
    ///
    /// Panics only if an invariant of the set is broken, which is a bug in this module.
    #[must_use = "a new set does nothing until used"]
    pub fn new_empty(domain_count: u32) -> Self {
        let word_count = usize_from_u32(domain_count.div_ceil(WORD_BITS));
        let words = smallvec::smallvec![0; word_count];
        let set = Self { domain_count, words, _index: PhantomData };
        set.assert_invariants();
        assert!(set.is_empty(), "a new set starts empty");
        if domain_count <= 128 {
            assert!(!set.words.spilled(), "a set of up to 128 indices stays inline");
        }
        set
    }

    /// Creates a set containing every index in `0..domain_count`.
    ///
    /// # Panics
    ///
    /// Panics only if an invariant of the set is broken, which is a bug in this module.
    #[must_use = "a new set does nothing until used"]
    pub fn new_filled(domain_count: u32) -> Self {
        let mut set = Self::new_empty(domain_count);
        set.insert_all();
        assert_eq!(set.count(), domain_count, "a filled set holds the whole domain");
        assert_eq!(set.domain_count, domain_count, "filling keeps the domain");
        set
    }

    /// Returns the number of indices in the domain, present or not.
    ///
    /// # Panics
    ///
    /// Panics only if an invariant of the set is broken, which is a bug in this module.
    #[must_use = "the count is the only result"]
    pub fn domain_count(&self) -> u32 {
        self.assert_invariants();
        self.domain_count
    }

    /// Inserts `index`; returns `true` if it was absent.
    ///
    /// # Panics
    ///
    /// Panics if `index` is outside the domain.
    #[inline]
    #[track_caller]
    pub fn insert(&mut self, index: I) -> bool {
        let (word_index, mask) = self.locate(index);
        let before = self.words[word_index];
        self.words[word_index] = before | mask;
        assert!(self.words[word_index] & mask == mask, "the bit is set after an insert");
        assert_eq!(self.words[word_index] & !mask, before & !mask, "other bits are untouched");
        before & mask == 0
    }

    /// Removes `index`; returns `true` if it was present.
    ///
    /// # Panics
    ///
    /// Panics if `index` is outside the domain.
    #[inline]
    #[track_caller]
    pub fn remove(&mut self, index: I) -> bool {
        let (word_index, mask) = self.locate(index);
        let before = self.words[word_index];
        self.words[word_index] = before & !mask;
        assert!(self.words[word_index] & mask == 0, "the bit is clear after a remove");
        assert_eq!(self.words[word_index] & !mask, before & !mask, "other bits are untouched");
        before & mask != 0
    }

    /// Returns `true` if `index` is in the set.
    ///
    /// # Panics
    ///
    /// Panics if `index` is outside the domain.
    #[inline]
    #[must_use = "the answer is the only result"]
    #[track_caller]
    pub fn contains(&self, index: I) -> bool {
        let (word_index, mask) = self.locate(index);
        self.words[word_index] & mask != 0
    }

    /// Returns the number of indices in the set.
    ///
    /// # Panics
    ///
    /// Panics only if an invariant of the set is broken, which is a bug in this module.
    #[must_use = "the count is the only result"]
    pub fn count(&self) -> u32 {
        let count = count_ones(&self.words);
        assert!(count <= self.domain_count, "no bit is set outside the domain");
        count
    }

    /// Returns `true` if the set is empty.
    ///
    /// # Panics
    ///
    /// Panics only if an invariant of the set is broken, which is a bug in this module.
    #[must_use = "the answer is the only result"]
    pub fn is_empty(&self) -> bool {
        self.assert_invariants();
        self.words.iter().all(|&word| word == 0)
    }

    /// Removes every index, keeping the allocation.
    ///
    /// # Panics
    ///
    /// Panics only if an invariant of the set is broken, which is a bug in this module.
    pub fn clear(&mut self) {
        let word_count = self.words.len();
        self.words.fill(0);
        assert_eq!(self.words.len(), word_count, "clearing keeps every word");
        self.assert_invariants();
        assert!(self.is_empty(), "a cleared set is empty");
    }

    /// Inserts every index of the domain.
    ///
    /// # Panics
    ///
    /// Panics only if an invariant of the set is broken, which is a bug in this module.
    pub fn insert_all(&mut self) {
        self.words.fill(Word::MAX);
        let mask = last_word_mask(self.domain_count);
        // An empty domain has no word, and nothing beyond it to clear.
        if let Some(last) = self.words.last_mut() {
            *last &= mask;
        }
        self.assert_invariants();
        assert_eq!(self.count(), self.domain_count, "every index of the domain is present");
        // The last index sits next to the cleared excess bits, where an off-by-one would show.
        if let Some(last) = self.domain_count.checked_sub(1) {
            let last_index = I::from_usize(usize_from_u32(last));
            assert!(self.contains(last_index), "the last index of the domain is present");
        }
    }

    /// Adds every index of `other`; returns `true` if `self` changed.
    ///
    /// # Panics
    ///
    /// Panics if the domains differ.
    #[track_caller]
    pub fn union(&mut self, other: &Self) -> bool {
        assert_eq!(self.domain_count, other.domain_count, "bit sets over different domains");
        let changed = union_words(&mut self.words, &other.words);
        self.assert_invariants();
        changed
    }

    /// Keeps only the indices also in `other`; returns `true` if `self` changed.
    ///
    /// # Panics
    ///
    /// Panics if the domains differ.
    #[track_caller]
    pub fn intersect(&mut self, other: &Self) -> bool {
        assert_eq!(self.domain_count, other.domain_count, "bit sets over different domains");
        let changed = intersect_words(&mut self.words, &other.words);
        self.assert_invariants();
        changed
    }

    /// Removes every index of `other`; returns `true` if `self` changed.
    ///
    /// # Panics
    ///
    /// Panics if the domains differ.
    #[track_caller]
    pub fn subtract(&mut self, other: &Self) -> bool {
        assert_eq!(self.domain_count, other.domain_count, "bit sets over different domains");
        let changed = subtract_words(&mut self.words, &other.words);
        self.assert_invariants();
        changed
    }

    /// Iterates over the indices in the set, in increasing order.
    ///
    /// # Panics
    ///
    /// Panics only if an invariant of the set is broken, which is a bug in this module.
    pub fn iter(&self) -> impl Iterator<Item = I> + '_ {
        self.assert_invariants();
        let domain_count = self.domain_count;
        (0_u32..).zip(self.words.iter()).flat_map(move |(word_index, &word)| {
            let base = word_index * WORD_BITS;
            BitIndices(word).map(move |bit| {
                let value = base + bit;
                assert!(value < domain_count, "a present index lies in the domain");
                I::from_usize(usize_from_u32(value))
            })
        })
    }

    /// Returns the word holding `index` and the mask of its bit.
    #[inline]
    #[track_caller]
    fn locate(&self, index: I) -> (usize, Word) {
        let bit_index = u32_from_usize(index.index());
        assert!(
            bit_index < self.domain_count,
            "index {bit_index} out of bit set domain {}",
            self.domain_count
        );
        let word_index = usize_from_u32(bit_index.divide_floor(WORD_BITS));
        let mask: Word = 1 << bit_index.remainder(WORD_BITS);
        assert!(word_index < self.words.len(), "an index in the domain has a word");
        assert_eq!(mask.count_ones(), 1, "a mask selects one bit");
        (word_index, mask)
    }

    /// Checks the word count and that no bit beyond the domain is set.
    fn assert_invariants(&self) {
        assert_eq!(
            u32_from_usize(self.words.len()),
            self.domain_count.div_ceil(WORD_BITS),
            "one word per started block of 64 indices"
        );
        if let Some(&last) = self.words.last() {
            let excess = last & !last_word_mask(self.domain_count);
            assert_eq!(excess, 0, "bits beyond the domain must stay clear");
        }
    }
}

/// Returns the mask of the bits of the last word that lie in a domain of `domain_count` indices.
///
/// Branch-free: a domain that fills its last word shifts by zero and keeps every bit.
fn last_word_mask(domain_count: u32) -> Word {
    let used_bits = domain_count.remainder(WORD_BITS);
    let mask = Word::MAX >> (WORD_BITS - used_bits).remainder(WORD_BITS);
    assert!(mask != 0, "the last word holds at least one index");
    assert_eq!(mask.trailing_ones(), mask.count_ones(), "the mask is a run of low bits");
    mask
}

// The word loops below are free functions over plain slices, so the loop does not reload fields
// through `self`. Each checks its postcondition word by word in a form the compiler proves from
// the line above, so the assertion costs nothing until someone breaks the operation.

/// Sets in `target` every bit of `source`; returns `true` if any word of `target` changed.
fn union_words(target: &mut [Word], source: &[Word]) -> bool {
    assert_eq!(target.len(), source.len(), "word slices of equal length");
    let mut changed = false;
    for (target_word, &source_word) in target.iter_mut().zip(source) {
        let combined = *target_word | source_word;
        assert!(combined & source_word == source_word, "a union contains the other set");
        changed |= combined != *target_word;
        *target_word = combined;
    }
    changed
}

/// Clears in `target` every bit absent from `source`; returns `true` if any word changed.
fn intersect_words(target: &mut [Word], source: &[Word]) -> bool {
    assert_eq!(target.len(), source.len(), "word slices of equal length");
    let mut changed = false;
    for (target_word, &source_word) in target.iter_mut().zip(source) {
        let combined = *target_word & source_word;
        assert!(combined & !source_word == 0, "an intersection lies in the other set");
        changed |= combined != *target_word;
        *target_word = combined;
    }
    changed
}

/// Clears in `target` every bit of `source`; returns `true` if any word of `target` changed.
fn subtract_words(target: &mut [Word], source: &[Word]) -> bool {
    assert_eq!(target.len(), source.len(), "word slices of equal length");
    let mut changed = false;
    for (target_word, &source_word) in target.iter_mut().zip(source) {
        let combined = *target_word & !source_word;
        assert!(combined & source_word == 0, "a difference shares no index");
        changed |= combined != *target_word;
        *target_word = combined;
    }
    changed
}

/// Counts the set bits of `words`.
fn count_ones(words: &[Word]) -> u32 {
    let count: u32 = words.iter().map(|word| word.count_ones()).sum();
    assert!(usize_from_u32(count) <= words.len() * WORD_BITS_USIZE, "at most every bit is set");
    count
}

/// Yields the positions of the set bits of a word, lowest first.
struct BitIndices(Word);

impl Iterator for BitIndices {
    type Item = u32;

    #[inline]
    fn next(&mut self) -> Option<u32> {
        if self.0 == 0 {
            return None;
        }
        let bit = self.0.trailing_zeros();
        assert!(bit < WORD_BITS, "a non-zero word has a set bit");
        // Clears the lowest set bit.
        self.0 &= self.0 - 1;
        assert!(self.0 & (1 << bit) == 0, "a yielded bit is cleared");
        Some(bit)
    }
}

impl<I: TypedIndex> fmt::Debug for DenseBitSet<I> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.assert_invariants();
        assert!(self.count() <= self.domain_count, "no bit is set outside the domain");
        formatter.debug_set().entries(self.iter()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    crate::newtype_index! {
        /// Test index.
        struct Node;
    }

    /// Returns the node with index `value`.
    fn node(value: u32) -> Node {
        Node::from_u32(value)
    }

    /// Creates sets at the inline capacity and one index past it, and checks where the words
    /// live.
    #[test]
    fn small_sets_stay_inline() {
        let inline = DenseBitSet::<Node>::new_empty(128);
        assert!(!inline.words.spilled());
        let spilled = DenseBitSet::<Node>::new_empty(129);
        assert!(spilled.words.spilled(), "one index past the inline capacity spills");
    }

    /// Builds the multiples of three and of five, and compares union, intersection, difference
    /// and removal with the arithmetic answer.
    #[test]
    fn operations_match_a_reference_set() {
        let mut threes = DenseBitSet::<Node>::new_empty(200);
        let mut fives = DenseBitSet::<Node>::new_empty(200);
        for value in (0..200).step_by(3) {
            assert!(threes.insert(node(value)));
        }
        for value in (0..200).step_by(5) {
            assert!(fives.insert(node(value)));
        }
        let mut union = threes.clone();
        assert!(union.union(&fives));
        assert!(!union.union(&fives), "second union changes nothing");
        let mut intersection = threes.clone();
        assert!(intersection.intersect(&fives));
        assert!(!intersection.intersect(&fives), "second intersection changes nothing");
        assert_eq!(
            intersection.iter().map(Node::as_u32).collect::<Vec<_>>(),
            (0..200).step_by(15).collect::<Vec<_>>()
        );
        let mut difference = threes.clone();
        assert!(difference.subtract(&fives));
        assert!(!difference.subtract(&fives), "second difference changes nothing");
        assert_eq!(difference.count() + intersection.count(), threes.count());
        assert_eq!(union.count() + intersection.count(), threes.count() + fives.count());
        assert!(threes.remove(node(3)));
        assert!(!threes.contains(node(3)));
        assert!(!threes.remove(node(3)), "a second remove finds nothing");
    }

    /// Fills, empties and unites sets whose domain is empty, ends inside a word, fills it exactly
    /// or spills by one bit, where excess bits would leak.
    #[test]
    fn domain_boundaries() {
        for domain_count in [0, 1, 63, 64, 65, 128] {
            let full = DenseBitSet::<Node>::new_filled(domain_count);
            assert_eq!(full.count(), domain_count);
            assert_eq!(full.domain_count(), domain_count);
            assert_eq!(full.iter().count(), usize_from_u32(domain_count));
            let mut empty = DenseBitSet::<Node>::new_empty(domain_count);
            assert!(empty.is_empty());
            let changed = empty.union(&full);
            assert_eq!(changed, domain_count > 0, "only an empty domain stays unchanged");
            assert_eq!(empty, full);
            empty.clear();
            assert!(empty.is_empty());
        }
        let mut set = DenseBitSet::<Node>::new_empty(65);
        assert!(set.insert(node(0)), "the first index of the domain is valid");
        assert!(set.insert(node(64)), "the last index of the domain is valid");
        assert_eq!(format!("{set:?}"), "{Node(0), Node(64)}");
    }

    /// Inserts the index equal to the domain count, the first one outside the domain.
    #[test]
    #[should_panic(expected = "index 65 out of bit set domain 65")]
    fn rejects_index_at_domain_count() {
        let mut set = DenseBitSet::<Node>::new_empty(65);
        let _inserted = set.insert(node(65));
    }

    /// Looks up index zero in an empty domain, which has no valid index at all.
    #[test]
    #[should_panic(expected = "index 0 out of bit set domain 0")]
    fn rejects_every_index_of_an_empty_domain() {
        let set = DenseBitSet::<Node>::new_empty(0);
        let _present = set.contains(node(0));
    }

    /// Unites sets whose domains differ by one index, which must panic.
    #[test]
    #[should_panic(expected = "bit sets over different domains")]
    fn rejects_mismatched_domains() {
        let mut small = DenseBitSet::<Node>::new_empty(10);
        let _changed = small.union(&DenseBitSet::new_empty(11));
    }
}
