//! Collections with a fixed capacity, allocated once.
//!
//! Everything has a limit. A collection that grows on demand turns a burst of input into a latency
//! spike (a reallocation and a copy of every item) or into unbounded memory use, and the failure
//! shows up far from its cause. These collections allocate their whole capacity when they are
//! created and never reallocate: a push beyond the capacity is refused, and the caller decides what
//! to do (push back on the producer, drop, report) at the point where the limit is hit.
//!
//! ```
//! use vulcan::bounded::{BoundedQueue, BoundedVec};
//!
//! let mut queue = BoundedQueue::with_capacity(2);
//! assert!(queue.push_back(1_u32).is_ok());
//! assert!(queue.push_back(2).is_ok());
//! assert_eq!(queue.push_back(3), Err(3), "full: the item is handed back");
//! assert_eq!(queue.pop_front(), Some(1));
//!
//! let mut stack = BoundedVec::with_capacity(1);
//! assert!(stack.push('a').is_ok());
//! assert_eq!(stack.push('b'), Err('b'));
//! assert_eq!(stack.as_slice(), &['a']);
//! ```

use crate::convert::{u32_from_usize, usize_from_u32};
use alloc::collections::{VecDeque, vec_deque};

/// A vector that never grows past the capacity it was created with.
#[derive(Debug, PartialEq, Eq)]
pub struct BoundedVec<Value> {
    /// The items, in an allocation made for `capacity` items up front.
    items: Vec<Value>,
    /// The maximum number of items, at least one.
    capacity: u32,
}

// A bounded vector sits in hot structures: its handle stays a `Vec` and a `u32`.
#[cfg(target_pointer_width = "64")]
crate::static_assert_size!(BoundedVec<u64>, 32);

impl<Value> BoundedVec<Value> {
    /// Creates an empty vector and allocates room for `capacity` items.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero: a collection that can hold nothing is a configuration error.
    #[must_use = "a collection does nothing until items are pushed"]
    pub fn with_capacity(capacity: u32) -> Self {
        assert!(capacity > 0, "a bounded collection needs a capacity of at least one");
        let items = Vec::with_capacity(usize_from_u32(capacity));
        assert!(items.capacity() >= usize_from_u32(capacity), "the allocation covers the capacity");
        let vector = Self { items, capacity };
        vector.assert_invariants();
        vector
    }

    /// Appends `item`, or hands it back if the vector is full.
    ///
    /// # Errors
    ///
    /// Returns `item` unchanged when the vector already holds `capacity` items; the vector is left
    /// as it was.
    ///
    /// # Panics
    ///
    /// Panics if an invariant is broken (the allocation moved or the count is off), which is a bug
    /// in this module.
    #[inline]
    pub fn push(&mut self, item: Value) -> Result<(), Value> {
        let count_before = self.len();
        if count_before < self.capacity {
            let allocation_before = self.items.capacity();
            self.items.push(item);
            assert_eq!(self.items.capacity(), allocation_before, "a bounded vector never grows");
            assert_eq!(self.len(), count_before + 1, "a push adds exactly one item");
            Ok(())
        } else {
            assert_eq!(count_before, self.capacity, "a refused push finds the vector exactly full");
            Err(item)
        }
    }

    /// Removes and returns the last item.
    ///
    /// # Panics
    ///
    /// Panics if an invariant is broken (the allocation moved or the count is off), which is a bug
    /// in this module.
    #[inline]
    pub fn pop(&mut self) -> Option<Value> {
        let count_before = self.len();
        let allocation_before = self.items.capacity();
        let item = self.items.pop();
        assert_eq!(self.items.capacity(), allocation_before, "a pop keeps the allocation");
        if item.is_some() {
            assert_eq!(self.len() + 1, count_before, "a pop removes exactly one item");
        } else {
            assert_eq!(count_before, 0, "only an empty vector yields nothing");
        }
        item
    }

    /// Removes every item, keeping the allocation.
    ///
    /// # Panics
    ///
    /// Panics if an invariant is broken (the allocation moved or an item is left), which is a bug
    /// in this module.
    pub fn clear(&mut self) {
        let allocation_before = self.items.capacity();
        self.items.clear();
        assert_eq!(self.items.capacity(), allocation_before, "a clear keeps the allocation");
        assert!(self.items.is_empty(), "a clear removes every item");
        self.assert_invariants();
    }

    /// Returns the items as a slice.
    #[inline]
    #[must_use = "the query has no side effect"]
    pub fn as_slice(&self) -> &[Value] {
        self.assert_invariants();
        &self.items
    }

    /// Returns the number of items.
    #[inline]
    #[must_use = "the query has no side effect"]
    pub fn len(&self) -> u32 {
        self.assert_invariants();
        u32_from_usize(self.items.len())
    }

    /// Returns the maximum number of items.
    ///
    /// # Panics
    ///
    /// Panics if an invariant is broken (the capacity is zero), which is a bug in this module.
    #[inline]
    #[must_use = "the query has no side effect"]
    pub const fn capacity(&self) -> u32 {
        assert!(self.capacity > 0, "a bounded collection holds at least one item");
        self.capacity
    }

    /// Returns `true` if the vector holds no item.
    #[inline]
    #[must_use = "the query has no side effect"]
    pub fn is_empty(&self) -> bool {
        self.assert_invariants();
        self.items.is_empty()
    }

    /// Returns `true` if a push would be refused.
    ///
    /// # Panics
    ///
    /// Panics if an invariant is broken (a full collection that holds nothing), which is a bug in
    /// this module.
    #[inline]
    #[must_use = "the query has no side effect"]
    pub fn is_full(&self) -> bool {
        let full = self.len() == self.capacity;
        if full {
            assert!(!self.items.is_empty(), "a full collection holds at least one item");
        }
        full
    }

    /// Checks the invariants: a capacity of at least one, at most `capacity` items, and an
    /// allocation that still covers the capacity.
    #[inline]
    fn assert_invariants(&self) {
        let capacity = usize_from_u32(self.capacity);
        assert!(capacity > 0, "a bounded collection holds at least one item");
        assert!(self.items.len() <= capacity, "the count stays within the capacity");
        assert!(self.items.capacity() >= capacity, "the allocation covers the capacity");
    }
}

/// Cloning allocates the full capacity, not only room for the current items, so the clone keeps
/// accepting pushes up to the same capacity without reallocating.
impl<Value: Clone> Clone for BoundedVec<Value> {
    fn clone(&self) -> Self {
        let mut copy = Self::with_capacity(self.capacity);
        copy.items.extend_from_slice(&self.items);
        assert_eq!(copy.items.len(), self.items.len(), "a clone holds every item");
        assert_eq!(copy.capacity, self.capacity, "a clone keeps the capacity");
        copy.assert_invariants();
        copy
    }
}

/// A first-in, first-out queue that never grows past the capacity it was created with.
#[derive(Debug, PartialEq, Eq)]
pub struct BoundedQueue<Value> {
    /// The items, front first, in a ring buffer allocated for `capacity` items up front.
    items: VecDeque<Value>,
    /// The maximum number of items, at least one.
    capacity: u32,
}

// A bounded queue sits in hot structures: its handle stays a `VecDeque` and a `u32`.
#[cfg(target_pointer_width = "64")]
crate::static_assert_size!(BoundedQueue<u64>, 40);

impl<Value> BoundedQueue<Value> {
    /// Creates an empty queue and allocates room for `capacity` items.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero: a queue that can hold nothing is a configuration error.
    #[must_use = "a collection does nothing until items are pushed"]
    pub fn with_capacity(capacity: u32) -> Self {
        assert!(capacity > 0, "a bounded collection needs a capacity of at least one");
        let items = VecDeque::with_capacity(usize_from_u32(capacity));
        assert!(items.capacity() >= usize_from_u32(capacity), "the allocation covers the capacity");
        let queue = Self { items, capacity };
        queue.assert_invariants();
        queue
    }

    /// Appends `item` at the back, or hands it back if the queue is full.
    ///
    /// # Errors
    ///
    /// Returns `item` unchanged when the queue already holds `capacity` items; the queue is left as
    /// it was.
    ///
    /// # Panics
    ///
    /// Panics if an invariant is broken (the allocation moved or the count is off), which is a bug
    /// in this module.
    #[inline]
    pub fn push_back(&mut self, item: Value) -> Result<(), Value> {
        let count_before = self.len();
        if count_before < self.capacity {
            let allocation_before = self.items.capacity();
            self.items.push_back(item);
            assert_eq!(self.items.capacity(), allocation_before, "a bounded queue never grows");
            assert_eq!(self.len(), count_before + 1, "a push adds exactly one item");
            Ok(())
        } else {
            assert_eq!(count_before, self.capacity, "a refused push finds the queue exactly full");
            Err(item)
        }
    }

    /// Removes and returns the item at the front.
    ///
    /// # Panics
    ///
    /// Panics if an invariant is broken (the allocation moved or the count is off), which is a bug
    /// in this module.
    #[inline]
    pub fn pop_front(&mut self) -> Option<Value> {
        let count_before = self.len();
        let allocation_before = self.items.capacity();
        let item = self.items.pop_front();
        assert_eq!(self.items.capacity(), allocation_before, "a pop keeps the allocation");
        if item.is_some() {
            assert_eq!(self.len() + 1, count_before, "a pop removes exactly one item");
        } else {
            assert_eq!(count_before, 0, "only an empty queue yields nothing");
        }
        item
    }

    /// Returns the item at the front without removing it.
    #[inline]
    #[must_use = "the query has no side effect"]
    pub fn front(&self) -> Option<&Value> {
        self.assert_invariants();
        self.items.front()
    }

    /// Removes every item, keeping the allocation.
    ///
    /// # Panics
    ///
    /// Panics if an invariant is broken (the allocation moved or an item is left), which is a bug
    /// in this module.
    pub fn clear(&mut self) {
        let allocation_before = self.items.capacity();
        self.items.clear();
        assert_eq!(self.items.capacity(), allocation_before, "a clear keeps the allocation");
        assert!(self.items.is_empty(), "a clear removes every item");
        self.assert_invariants();
    }

    /// Iterates over the items, front to back.
    #[must_use = "iterators are lazy and do nothing unless consumed"]
    pub fn iter(&self) -> vec_deque::Iter<'_, Value> {
        self.assert_invariants();
        self.items.iter()
    }

    /// Returns the number of items.
    #[inline]
    #[must_use = "the query has no side effect"]
    pub fn len(&self) -> u32 {
        self.assert_invariants();
        u32_from_usize(self.items.len())
    }

    /// Returns the maximum number of items.
    ///
    /// # Panics
    ///
    /// Panics if an invariant is broken (the capacity is zero), which is a bug in this module.
    #[inline]
    #[must_use = "the query has no side effect"]
    pub const fn capacity(&self) -> u32 {
        assert!(self.capacity > 0, "a bounded collection holds at least one item");
        self.capacity
    }

    /// Returns `true` if the queue holds no item.
    #[inline]
    #[must_use = "the query has no side effect"]
    pub fn is_empty(&self) -> bool {
        self.assert_invariants();
        self.items.is_empty()
    }

    /// Returns `true` if a push would be refused.
    ///
    /// # Panics
    ///
    /// Panics if an invariant is broken (a full collection that holds nothing), which is a bug in
    /// this module.
    #[inline]
    #[must_use = "the query has no side effect"]
    pub fn is_full(&self) -> bool {
        let full = self.len() == self.capacity;
        if full {
            assert!(!self.items.is_empty(), "a full collection holds at least one item");
        }
        full
    }

    /// Checks the invariants: a capacity of at least one, at most `capacity` items, and an
    /// allocation that still covers the capacity.
    #[inline]
    fn assert_invariants(&self) {
        let capacity = usize_from_u32(self.capacity);
        assert!(capacity > 0, "a bounded collection holds at least one item");
        assert!(self.items.len() <= capacity, "the count stays within the capacity");
        assert!(self.items.capacity() >= capacity, "the allocation covers the capacity");
    }
}

/// Cloning allocates the full capacity, not only room for the current items, so the clone keeps
/// accepting pushes up to the same capacity without reallocating.
impl<Value: Clone> Clone for BoundedQueue<Value> {
    fn clone(&self) -> Self {
        let mut copy = Self::with_capacity(self.capacity);
        copy.items.extend(self.items.iter().cloned());
        assert_eq!(copy.items.len(), self.items.len(), "a clone holds every item");
        assert_eq!(copy.capacity, self.capacity, "a clone keeps the capacity");
        copy.assert_invariants();
        copy
    }
}

impl<'queue, Value> IntoIterator for &'queue BoundedQueue<Value> {
    type Item = &'queue Value;
    type IntoIter = vec_deque::Iter<'queue, Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fills a vector to capacity, then crosses the boundary both ways: the push at capacity is
    /// refused and changes nothing, and one pop makes room for exactly one item.
    #[test]
    fn vec_fills_up_then_refuses() {
        let mut stack = BoundedVec::with_capacity(3);
        assert!(stack.is_empty(), "a new vector is empty");
        for item in 0..3_u8 {
            assert!(!stack.is_full(), "below capacity, the vector has room");
            assert_eq!(stack.push(item), Ok(()));
        }
        assert!(stack.is_full(), "at capacity, the vector is full");
        assert_eq!(stack.push(9), Err(9), "the push above capacity is refused");
        assert_eq!(stack.as_slice(), &[0, 1, 2], "a refused push changes nothing");
        assert_eq!(stack.len(), 3, "a refused push keeps the count");
        assert_eq!(stack.pop(), Some(2));
        assert_eq!(stack.push(7), Ok(()), "one pop makes room for one item");
        assert_eq!(stack.push(8), Err(8), "and for no more");
        stack.clear();
        assert!(stack.is_empty(), "a clear empties the vector");
        assert_eq!((stack.len(), stack.capacity()), (0, 3), "a clear keeps the capacity");
        assert_eq!(stack.pop(), None, "an empty vector yields nothing");
    }

    /// Fills a queue to capacity, checks the order and the refusal at the boundary, then reuses
    /// the room freed by one pop.
    #[test]
    fn queue_is_first_in_first_out_and_bounded() {
        let mut queue = BoundedQueue::with_capacity(2);
        assert_eq!(queue.pop_front(), None, "an empty queue yields nothing");
        assert_eq!(queue.front(), None, "an empty queue has no front");
        assert_eq!(queue.push_back('a'), Ok(()));
        assert_eq!(queue.push_back('b'), Ok(()));
        assert!(queue.is_full(), "at capacity, the queue is full");
        assert_eq!(queue.push_back('c'), Err('c'), "the push above capacity is refused");
        assert_eq!(queue.len(), 2, "a refused push keeps the count");
        assert_eq!(queue.front(), Some(&'a'));
        assert_eq!(queue.iter().copied().collect::<Vec<_>>(), ['a', 'b']);
        assert_eq!(queue.pop_front(), Some('a'));
        assert_eq!(queue.push_back('c'), Ok(()), "one pop makes room for one item");
        assert_eq!((&queue).into_iter().copied().collect::<Vec<_>>(), ['b', 'c']);
        queue.clear();
        assert!(queue.is_empty(), "a clear empties the queue");
        assert_eq!(queue.capacity(), 2, "a clear keeps the capacity");
    }

    /// Wraps the ring buffer around many times at a capacity of one, the smallest allowed, and
    /// checks that the queue never reallocates nor lets a second item in.
    #[test]
    fn queue_of_capacity_one_wraps_without_growing() {
        let mut queue = BoundedQueue::with_capacity(1);
        let allocation = queue.items.capacity();
        for item in 0..100_u32 {
            assert_eq!(queue.push_back(item), Ok(()));
            assert_eq!(queue.push_back(item + 1), Err(item + 1), "capacity one holds one item");
            assert_eq!(queue.pop_front(), Some(item));
        }
        assert_eq!(queue.items.capacity(), allocation, "the queue never reallocated");
    }

    /// Clones partly filled collections and fills the clones to capacity: a derived `Clone`
    /// would allocate only for the current items, and the pushes would then reallocate.
    #[test]
    fn clones_keep_the_full_capacity() {
        let mut stack = BoundedVec::with_capacity(4);
        assert_eq!(stack.push(1_u8), Ok(()));
        let mut stack_copy = stack.clone();
        assert_eq!(stack_copy, stack, "a clone equals the original");
        for item in 2..5_u8 {
            assert_eq!(stack_copy.push(item), Ok(()), "the clone has room up to the capacity");
        }
        assert_eq!(stack_copy.push(5), Err(5), "and no further");
        assert_eq!(stack.as_slice(), &[1], "the original is untouched");

        let mut queue = BoundedQueue::with_capacity(4);
        assert_eq!(queue.push_back(1_u8), Ok(()));
        let mut queue_copy = queue.clone();
        assert_eq!(queue_copy, queue, "a clone equals the original");
        for item in 2..5_u8 {
            assert_eq!(queue_copy.push_back(item), Ok(()), "the clone has room up to the capacity");
        }
        assert_eq!(queue_copy.push_back(5), Err(5), "and no further");
        assert_eq!(queue.len(), 1, "the original is untouched");
    }

    /// Creates a queue with no capacity, which is a configuration error.
    #[test]
    #[should_panic(expected = "capacity of at least one")]
    fn queue_rejects_zero_capacity() {
        let _queue = BoundedQueue::<u8>::with_capacity(0);
    }

    /// Creates a vector with no capacity, which is a configuration error.
    #[test]
    #[should_panic(expected = "capacity of at least one")]
    fn vec_rejects_zero_capacity() {
        let _stack = BoundedVec::<u8>::with_capacity(0);
    }
}
