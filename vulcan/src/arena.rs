//! Bump arenas with a fixed capacity.
//!
//! Short-lived graphs (ASTs, IR, per-request scratch data) are allocated in a [`Bump`] arena: an
//! allocation is a pointer bump, there is no per-object header, and the whole arena is freed at
//! once. With one arena per thread ([`ThreadArenas`]), parallel phases allocate without
//! contention.
//!
//! Every arena of this module gets its whole budget at construction
//! ([`BumpExtension::with_fixed_capacity`]) and never grows past it: the first chunk covers the
//! budget, and the allocation limit refuses any further chunk.
//!
//! The arena never runs destructors. The [`BumpExtension`] helpers reject element types that
//! need [`Drop`] at compile time, because their owned memory (a `String`, a `Vec`) would leak:
//!
//! ```compile_fail
//! use vulcan::arena::{Bump, BumpExtension as _};
//!
//! let arena = Bump::with_fixed_capacity(1024);
//! arena.allocate_exact(vec![String::from("leaks")]);
//! ```

use crate::convert::{u32_from_usize, usize_from_u32};
pub use bumpalo::Bump;
use core::{
    iter, mem,
    sync::atomic::{AtomicU32, Ordering},
};
use thread_local::ThreadLocal;

/// Extension methods for [`Bump`]: a constructor with a fixed capacity, and slice allocations
/// that refuse types that need [`Drop`].
pub trait BumpExtension {
    /// Creates an arena that holds `size_bytes` bytes and never allocates another chunk.
    ///
    /// # Panics
    ///
    /// Panics if `size_bytes` is zero: an arena that can hold nothing is a configuration error.
    #[must_use = "an arena that is never used wastes its allocation"]
    fn with_fixed_capacity(size_bytes: u32) -> Self;

    /// Moves the items of an exact-size iterator into the arena without an intermediate buffer.
    ///
    /// # Panics
    ///
    /// Panics if the arena has a fixed capacity and too little room is left for the items.
    #[expect(clippy::mut_from_ref, reason = "arena allocation hands out disjoint borrows")]
    fn allocate_exact<T, Items>(&self, items: Items) -> &mut [T]
    where
        Items: IntoIterator<Item = T>,
        Items::IntoIter: ExactSizeIterator;

    /// Allocates a single value as a one-element slice, for APIs that take `&[T]` when a node
    /// usually has one child.
    ///
    /// # Panics
    ///
    /// Panics if the arena has a fixed capacity and too little room is left for the value.
    #[expect(clippy::mut_from_ref, reason = "arena allocation hands out disjoint borrows")]
    fn allocate_one<T>(&self, value: T) -> &mut [T];
}

impl BumpExtension for Bump {
    fn with_fixed_capacity(size_bytes: u32) -> Self {
        assert!(size_bytes > 0, "an arena needs a capacity of at least one byte");
        let capacity_bytes = usize_from_u32(size_bytes);
        let arena = Self::with_capacity(capacity_bytes);
        arena.set_allocation_limit(Some(capacity_bytes));
        assert!(arena.chunk_capacity() >= capacity_bytes, "the first chunk covers the budget");
        assert!(arena.allocated_bytes() >= capacity_bytes, "no further chunk fits under the limit");
        arena
    }

    #[inline]
    fn allocate_exact<T, Items>(&self, items: Items) -> &mut [T]
    where
        Items: IntoIterator<Item = T>,
        Items::IntoIter: ExactSizeIterator,
    {
        const { assert!(!mem::needs_drop::<T>(), "arena values must not need `Drop`") };
        let items_exact = items.into_iter();
        let item_count = items_exact.len();
        if self.allocation_limit().is_some() {
            let size_bytes = size_of::<T>() * item_count;
            assert!(size_bytes <= self.chunk_capacity(), "the arena has room for the items");
        }
        let slice = self.alloc_slice_fill_iter(items_exact);
        assert_eq!(slice.len(), item_count, "the iterator reported its exact length");
        slice
    }

    #[inline]
    fn allocate_one<T>(&self, value: T) -> &mut [T] {
        let slice = self.allocate_exact(iter::once(value));
        assert_eq!(slice.len(), 1, "one value, one element");
        assert!(slice.as_ptr().is_aligned(), "the arena aligns the value");
        slice
    }
}

/// The "box" of arena-allocated trees: a unique reference into the arena.
///
/// One word with a non-null niche, so `Option<ArenaBox<'arena, T>>` is still one word. Creating
/// it is a pointer bump and dropping it does nothing; the arena frees every node at once. Use it
/// for child links in ASTs and IRs where a heap `Box<T>` would cost a `malloc` and a `free` per
/// node.
pub type ArenaBox<'arena, T> = &'arena mut T;

/// The capacity of a [`ThreadArenas`]: how many threads may allocate, and how much each may.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadArenasCapacity {
    /// The number of threads that may create an arena.
    pub thread_count_max: u32,
    /// The fixed capacity of each thread's arena, in bytes.
    pub arena_size_bytes: u32,
}

/// One [`Bump`] arena per thread, each with a fixed capacity.
///
/// Each thread allocates into its own arena, so parallel phases never contend on a lock. All
/// arenas live as long as `ThreadArenas`, and references into any of them can be shared across
/// threads once allocation is done. A thread that exits leaves its arena to the next thread
/// that starts, so `thread_count_max` bounds the threads alive at once.
///
/// ```
/// use vulcan::arena::{BumpExtension as _, ThreadArenas, ThreadArenasCapacity};
///
/// let capacity = ThreadArenasCapacity { thread_count_max: 4, arena_size_bytes: 1024 };
/// let mut arenas = ThreadArenas::with_capacity(capacity);
/// let shared = &arenas;
/// let slices: Vec<&[u32]> = std::thread::scope(|scope| {
///     let handles: Vec<_> = (0..4_u32)
///         .map(|thread| {
///             scope.spawn(move || &*shared.local().allocate_exact((0..8).map(|i| i * thread)))
///         })
///         .collect();
///     handles.into_iter().filter_map(|handle| handle.join().ok()).collect()
/// });
/// assert_eq!(slices[3], &[0, 3, 6, 9, 12, 15, 18, 21]);
/// assert!(arenas.allocated_bytes() >= 1024, "at least one arena of 1 KiB");
/// ```
#[derive(Debug)]
pub struct ThreadArenas {
    /// The arena of each thread that allocated so far.
    arenas: ThreadLocal<Bump>,
    /// The limits every arena and the set of arenas stay within.
    capacity: ThreadArenasCapacity,
    /// The number of arenas created so far, at most `capacity.thread_count_max`.
    arena_count: AtomicU32,
}

impl ThreadArenas {
    /// Creates an empty set of arenas. Each thread's arena is created and allocated on first use.
    ///
    /// # Panics
    ///
    /// Panics if either capacity is zero.
    #[must_use = "a set of arenas that is never used wastes its allocation"]
    pub fn with_capacity(capacity: ThreadArenasCapacity) -> Self {
        assert!(capacity.thread_count_max > 0, "thread arenas need at least one thread");
        assert!(capacity.arena_size_bytes > 0, "a thread arena needs at least one byte");
        let arenas = ThreadLocal::with_capacity(usize_from_u32(capacity.thread_count_max));
        Self { arenas, capacity, arena_count: AtomicU32::new(0) }
    }

    /// Returns the calling thread's arena, creating it on first use.
    ///
    /// # Panics
    ///
    /// Panics if more threads than `thread_count_max` ask for an arena.
    #[inline]
    #[must_use = "the arena is only useful to allocate in"]
    pub fn local(&self) -> &Bump {
        let arena = self.arenas.get_or(|| {
            // Relaxed: the counter only bounds how many arenas exist, it publishes no memory.
            let arena_count_before = self.arena_count.fetch_add(1, Ordering::Relaxed);
            assert!(
                arena_count_before < self.capacity.thread_count_max,
                "more threads than the thread arenas were sized for"
            );
            Bump::with_fixed_capacity(self.capacity.arena_size_bytes)
        });
        let size_bytes = usize_from_u32(self.capacity.arena_size_bytes);
        assert_eq!(arena.allocation_limit(), Some(size_bytes), "every arena has a fixed capacity");
        arena
    }

    /// Returns the total bytes reserved by all arenas, including unused capacity.
    ///
    /// Takes `&mut self` because [`Bump`] is not `Sync`: reading another thread's arena is only
    /// sound once every thread is done allocating.
    ///
    /// # Panics
    ///
    /// Panics if an arena lost part of its budget or was not counted, a broken invariant.
    #[must_use = "reading the size has no side effect"]
    pub fn allocated_bytes(&mut self) -> u64 {
        let arena_size_bytes = usize_from_u32(self.capacity.arena_size_bytes);
        let mut size_bytes = 0_u64;
        let mut arena_count = 0_u32;
        for arena in &mut self.arenas {
            assert!(arena.allocated_bytes() >= arena_size_bytes, "an arena keeps its budget");
            size_bytes += u64::from(u32_from_usize(arena.allocated_bytes()));
            arena_count += 1;
        }
        assert_eq!(arena_count, *self.arena_count.get_mut(), "every created arena is counted");
        size_bytes
    }

    /// Frees every allocation while keeping each arena's fixed chunk for reuse.
    ///
    /// Requires `&mut self`, so no reference into the arenas can outlive the reset.
    ///
    /// # Panics
    ///
    /// Panics if a reset drops an arena's fixed chunk, a broken invariant.
    pub fn reset(&mut self) {
        let arena_size_bytes = usize_from_u32(self.capacity.arena_size_bytes);
        for arena in &mut self.arenas {
            let allocated_bytes_before = arena.allocated_bytes();
            arena.reset();
            assert_eq!(arena.allocated_bytes(), allocated_bytes_before, "the chunk is kept");
            assert!(arena.chunk_capacity() >= arena_size_bytes, "the whole budget is free again");
        }
    }
}

/// A position in a [`SliceStack`], returned by [`SliceStack::mark`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "a mark is needed to finish the slice"]
pub struct Mark(u32);

/// One buffer with a fixed capacity, shared by every level of a nested builder.
///
/// A parser that collects children into a local `Vec` (or a `SmallVec` on the stack) pays an
/// allocation or stack space at every nesting level. Instead, each level records its start
/// ([`mark`](Self::mark)), pushes its children onto the shared stack, then moves everything
/// above its mark into the arena with [`finish`](Self::finish). Nested levels finish first, so
/// the stack unwinds in order and its single allocation is reused for the whole run.
///
/// ```
/// use vulcan::arena::{Bump, BumpExtension as _, SliceStack};
///
/// let arena = Bump::with_fixed_capacity(1024);
/// let mut stack = SliceStack::with_capacity(4);
/// let outer = stack.mark();
/// stack.push(1_u32);
/// let inner = stack.mark();
/// stack.push(10);
/// stack.push(11);
/// let children = stack.finish(inner, &arena);
/// stack.push(2);
/// assert_eq!(children, &[10, 11]);
/// assert_eq!(stack.finish(outer, &arena), &[1, 2]);
/// ```
#[derive(Debug)]
pub struct SliceStack<T> {
    /// The items of every unfinished level, outermost first.
    items: Vec<T>,
    /// The number of items the stack may hold across all levels.
    capacity: u32,
}

impl<T> SliceStack<T> {
    /// Creates an empty stack and allocates room for `capacity` items across all levels.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero: a stack that can hold nothing is a configuration error.
    #[must_use = "a stack that is never used wastes its allocation"]
    pub fn with_capacity(capacity: u32) -> Self {
        assert!(capacity > 0, "a slice stack needs a capacity of at least one item");
        let items = Vec::with_capacity(usize_from_u32(capacity));
        assert!(items.capacity() >= usize_from_u32(capacity), "the allocation covers the capacity");
        Self { items, capacity }
    }

    /// Records the current top of the stack.
    ///
    /// # Panics
    ///
    /// Panics if the stack holds more items than its capacity, a broken invariant.
    pub const fn mark(&self) -> Mark {
        let top = u32_from_usize(self.items.len());
        assert!(top <= self.capacity, "the stack stays within its capacity");
        Mark(top)
    }

    /// Pushes an item for the current level.
    ///
    /// # Panics
    ///
    /// Panics if the stack already holds `capacity` items.
    #[inline]
    #[track_caller]
    pub fn push(&mut self, item: T) {
        assert!(self.items.len() < usize_from_u32(self.capacity), "the slice stack is full");
        let allocation_before = self.items.capacity();
        self.items.push(item);
        assert_eq!(self.items.capacity(), allocation_before, "the stack never reallocates");
    }

    /// Returns the items pushed since `mark`.
    ///
    /// # Panics
    ///
    /// Panics if `mark` lies above the top of the stack: a level above it was finished first.
    #[must_use = "reading the items has no side effect"]
    #[track_caller]
    pub fn since(&self, mark: Mark) -> &[T] {
        self.assert_mark(mark);
        let items = &self.items[usize_from_u32(mark.0)..];
        assert!(items.len() <= usize_from_u32(self.capacity), "a level fits in the capacity");
        items
    }

    /// Moves the items pushed since `mark` into `arena` and pops them.
    ///
    /// # Panics
    ///
    /// Panics if `mark` lies above the top of the stack: levels must finish innermost first.
    #[track_caller]
    pub fn finish<'arena>(&mut self, mark: Mark, arena: &'arena Bump) -> &'arena mut [T] {
        self.assert_mark(mark);
        let start = usize_from_u32(mark.0);
        let item_count = self.items.len() - start;
        let slice = arena.allocate_exact(self.items.drain(start..));
        assert_eq!(slice.len(), item_count, "every item of the level moves to the arena");
        assert_eq!(self.items.len(), start, "the stack is back at the mark");
        slice
    }

    /// Asserts that `mark` lies at or below the top of the stack.
    #[track_caller]
    fn assert_mark(&self, mark: Mark) {
        assert!(mark.0 <= self.capacity, "a mark lies within the capacity");
        assert!(
            usize_from_u32(mark.0) <= self.items.len(),
            "mark {} above the stack top {}: levels finished out of order",
            mark.0,
            self.items.len()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{panic, sync::Barrier, thread};

    /// Builds an arena of 1 KiB and allocates through every helper: the slices hold the moved
    /// values, and an empty iterator gives an empty slice.
    #[test]
    fn allocations_hold_their_values() {
        let arena = Bump::with_fixed_capacity(1024);
        assert_eq!(arena.allocate_exact(0_u32..4), &[0, 1, 2, 3]);
        assert_eq!(arena.allocate_exact(vec![1_u8, 2, 3]), &[1, 2, 3]);
        assert_eq!(arena.allocate_exact([4_i32, 5_i32]), &[4_i32, 5_i32]);
        assert_eq!(arena.allocate_one('x'), &['x']);
        assert!(arena.allocate_exact(iter::empty::<u32>()).is_empty(), "nothing in, nothing out");
    }

    /// Fills an arena to the last byte of its chunk: the last fitting allocation succeeds and
    /// the arena never allocates a second chunk.
    #[test]
    fn a_fixed_arena_fills_up_to_its_chunk() {
        let arena = Bump::with_fixed_capacity(64);
        let allocated_bytes = arena.allocated_bytes();
        let room_bytes = arena.chunk_capacity();
        assert!(room_bytes >= 64, "the chunk covers the requested capacity");
        let filled = arena.allocate_exact(iter::repeat_n(7_u8, room_bytes));
        assert_eq!(filled.len(), room_bytes);
        assert_eq!(arena.chunk_capacity(), 0, "the chunk is full");
        assert_eq!(arena.allocated_bytes(), allocated_bytes, "no second chunk");
    }

    /// Allocates one byte past a full arena, which must crash instead of growing the arena.
    #[test]
    #[should_panic(expected = "the arena has room for the items")]
    fn a_fixed_arena_refuses_the_byte_past_its_chunk() {
        let arena = Bump::with_fixed_capacity(64);
        let _filled = arena.allocate_exact(iter::repeat_n(7_u8, arena.chunk_capacity()));
        let _overflow = arena.allocate_one(8_u8);
    }

    /// Asks for an arena of zero bytes, a configuration error.
    #[test]
    #[should_panic(expected = "at least one byte")]
    fn a_fixed_arena_refuses_a_zero_capacity() {
        let _arena = Bump::with_fixed_capacity(0);
    }

    /// Nests two levels on a stack of exactly their size: inner levels finish first, a fresh
    /// mark sees nothing, and finishing at the top gives an empty slice.
    #[test]
    fn slice_stack_nests_up_to_its_capacity() {
        let arena = Bump::with_fixed_capacity(1024);
        let mut stack = SliceStack::with_capacity(2);
        let outer = stack.mark();
        stack.push('a');
        let inner = stack.mark();
        assert!(stack.since(inner).is_empty(), "a fresh mark sees nothing");
        stack.push('b');
        assert_eq!(stack.since(outer), &['a', 'b']);
        assert_eq!(stack.finish(inner, &arena), &['b']);
        assert_eq!(stack.finish(outer, &arena), &['a']);
        assert!(stack.finish(stack.mark(), &arena).is_empty(), "an empty level is empty");
    }

    /// Pushes one item more than the capacity, which must crash instead of reallocating.
    #[test]
    #[should_panic(expected = "the slice stack is full")]
    fn slice_stack_refuses_the_item_past_its_capacity() {
        let mut stack = SliceStack::with_capacity(2);
        stack.push(1_u8);
        stack.push(2_u8);
        stack.push(3_u8);
    }

    /// Finishes an outer level before an inner one, which leaves the inner mark above the top.
    #[test]
    #[should_panic(expected = "levels finished out of order")]
    fn slice_stack_panics_on_a_stale_mark() {
        let arena = Bump::with_fixed_capacity(64);
        let mut stack = SliceStack::with_capacity(4);
        let outer = stack.mark();
        stack.push(1_u8);
        let inner = stack.mark();
        let _outer_items = stack.finish(outer, &arena);
        let _inner_items = stack.finish(inner, &arena);
    }

    /// Allocates from as many live threads as the capacity allows, held together by a barrier
    /// so none can hand its arena to the next: each gets its own arena with the full budget, and
    /// a reset keeps every arena.
    #[test]
    fn thread_arenas_are_per_thread() {
        let capacity = ThreadArenasCapacity { thread_count_max: 3, arena_size_bytes: 2048 };
        let mut arenas = ThreadArenas::with_capacity(capacity);
        let barrier = Barrier::new(3);
        let allocate = || {
            let slice = arenas.local().allocate_exact(0_u32..256);
            assert_eq!(slice.len(), 256);
            let _leader = barrier.wait();
        };
        thread::scope(|threads| {
            for _ in 0..3_u8 {
                let _handle = threads.spawn(allocate);
            }
        });
        let allocated_bytes = arenas.allocated_bytes();
        assert!(allocated_bytes >= 3 * 2048, "three arenas of 2 KiB");
        arenas.reset();
        assert_eq!(arenas.allocated_bytes(), allocated_bytes, "a reset keeps the chunks");
    }

    /// Asks for an arena from one thread more than the capacity, which must crash.
    #[test]
    #[should_panic(expected = "more threads than the thread arenas were sized for")]
    fn thread_arenas_refuse_the_thread_past_their_capacity() {
        let capacity = ThreadArenasCapacity { thread_count_max: 1, arena_size_bytes: 64 };
        let arenas = ThreadArenas::with_capacity(capacity);
        let _main_arena = arenas.local();
        let joined =
            thread::scope(|threads| threads.spawn(|| arenas.local().allocated_bytes()).join());
        if let Err(payload) = joined {
            panic::resume_unwind(payload);
        }
    }
}
