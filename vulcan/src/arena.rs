//! Bump arenas.
//!
//! Short-lived graphs (ASTs, IR, per-request scratch data) are allocated in a [`Bump`] arena: an
//! allocation is a pointer bump, there is no per-object header, and the whole arena is freed at
//! once. Solar allocates its AST and HIR this way, with one arena per thread so parallel phases
//! allocate without contention (see [`ThreadArenas`]).
//!
//! The arena never runs destructors. The [`BumpExt`] helpers reject element types that need
//! [`Drop`] at compile time, because their owned memory (a `String`, a `Vec`) would leak:
//!
//! ```compile_fail
//! use vulcan::arena::{Bump, BumpExt};
//!
//! let arena = Bump::new();
//! arena.alloc_vec(vec![String::from("leaks")]);
//! ```

use std::iter;

pub use bumpalo::Bump;
use thread_local::ThreadLocal;

/// Extension methods for [`Bump`] that allocate slices and refuse types that need [`Drop`].
pub trait BumpExt {
    /// Moves the elements of an exact-size iterator into the arena without an intermediate
    /// buffer.
    #[expect(clippy::mut_from_ref, reason = "arena allocation hands out disjoint borrows")]
    fn alloc_exact<T, I>(&self, iter: I) -> &mut [T]
    where
        I: IntoIterator<Item = T>,
        I::IntoIter: ExactSizeIterator;

    /// Moves the elements of any iterator into the arena.
    ///
    /// Collects into a temporary [`Vec`] first. Prefer [`alloc_exact`](Self::alloc_exact) when
    /// the length is known.
    #[expect(clippy::mut_from_ref, reason = "arena allocation hands out disjoint borrows")]
    fn alloc_from_iter<T>(&self, iter: impl IntoIterator<Item = T>) -> &mut [T];

    /// Moves the elements of a vector into the arena and frees the vector's buffer.
    #[expect(clippy::mut_from_ref, reason = "arena allocation hands out disjoint borrows")]
    fn alloc_vec<T>(&self, values: Vec<T>) -> &mut [T];

    /// Moves the elements of an array into the arena.
    #[expect(clippy::mut_from_ref, reason = "arena allocation hands out disjoint borrows")]
    fn alloc_array<T, const N: usize>(&self, values: [T; N]) -> &mut [T];
}

/// Fails compilation when `T` owns resources the arena would leak.
const fn assert_no_drop<T>() {
    const { assert!(!std::mem::needs_drop::<T>(), "arena values must not need `Drop`") }
}

impl BumpExt for Bump {
    #[inline]
    fn alloc_exact<T, I>(&self, iter: I) -> &mut [T]
    where
        I: IntoIterator<Item = T>,
        I::IntoIter: ExactSizeIterator,
    {
        assert_no_drop::<T>();
        self.alloc_slice_fill_iter(iter)
    }

    #[inline]
    fn alloc_from_iter<T>(&self, iter: impl IntoIterator<Item = T>) -> &mut [T] {
        assert_no_drop::<T>();
        let values: Vec<T> = iter.into_iter().collect();
        self.alloc_slice_fill_iter(values)
    }

    #[inline]
    fn alloc_vec<T>(&self, values: Vec<T>) -> &mut [T] {
        assert_no_drop::<T>();
        self.alloc_slice_fill_iter(values)
    }

    #[inline]
    fn alloc_array<T, const N: usize>(&self, values: [T; N]) -> &mut [T] {
        assert_no_drop::<T>();
        self.alloc_slice_fill_iter(values)
    }
}

/// One [`Bump`] arena per thread.
///
/// Each thread allocates into its own arena, so parallel phases never contend on a lock. All
/// arenas live as long as `ThreadArenas`, and references into any of them can be shared across
/// threads once allocation is done.
///
/// ```
/// use vulcan::arena::{BumpExt, ThreadArenas};
///
/// let mut arenas = ThreadArenas::new();
/// let shared = &arenas;
/// let slices: Vec<&[u32]> = std::thread::scope(|s| {
///     let handles: Vec<_> = (0..4_u32)
///         .map(|t| s.spawn(move || &*shared.local().alloc_exact((0..8).map(|i| i * t))))
///         .collect();
///     handles.into_iter().filter_map(|h| h.join().ok()).collect()
/// });
/// assert_eq!(slices[3], &[0, 3, 6, 9, 12, 15, 18, 21]);
/// assert!(arenas.allocated_bytes() > 0);
/// ```
#[derive(Debug, Default)]
pub struct ThreadArenas {
    arenas: ThreadLocal<Bump>,
}

impl ThreadArenas {
    /// Creates an empty set of arenas. Arenas are created lazily, on first use by each thread.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the calling thread's arena, creating it on first use.
    #[inline]
    #[must_use]
    pub fn local(&self) -> &Bump {
        self.arenas.get_or_default()
    }

    /// Returns the total bytes reserved by all arenas, including unused chunk capacity.
    ///
    /// Takes `&mut self` because [`Bump`] is not `Sync`: reading another thread's arena is only
    /// sound once every thread is done allocating.
    #[must_use]
    pub fn allocated_bytes(&mut self) -> usize {
        self.arenas.iter_mut().map(|arena| arena.allocated_bytes()).sum()
    }

    /// Frees every allocation while keeping the largest chunk of each arena for reuse.
    ///
    /// Requires `&mut self`, so no reference into the arenas can outlive the reset.
    pub fn reset(&mut self) {
        self.arenas.iter_mut().for_each(Bump::reset);
    }
}

/// Allocates a single value as a one-element slice.
///
/// Useful for APIs that take `&[T]` when a node usually has one child.
#[inline]
#[must_use]
pub fn alloc_one<T>(arena: &Bump, value: T) -> &mut [T] {
    arena.alloc_exact(iter::once(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_and_collected_allocations_match() {
        let arena = Bump::new();
        let exact = arena.alloc_exact(0_u32..100);
        let collected =
            arena.alloc_from_iter((0_u32..1000).filter(|v| v % 10 == 0).map(|v| v / 10));
        assert_eq!(exact, collected);
    }

    #[test]
    fn vec_and_array_round_trip() {
        let arena = Bump::new();
        assert_eq!(arena.alloc_vec(vec![1_u8, 2, 3]), &[1, 2, 3]);
        assert_eq!(arena.alloc_array([4_i32, 5]), &[4, 5]);
        assert_eq!(alloc_one(&arena, 'x'), &['x']);
    }

    #[test]
    fn thread_arenas_are_per_thread() {
        let mut arenas = ThreadArenas::new();
        std::thread::scope(|s| {
            for _ in 0..3 {
                s.spawn(|| {
                    let slice = arenas.local().alloc_exact(0_u32..256);
                    assert_eq!(slice.len(), 256);
                });
            }
        });
        assert!(arenas.allocated_bytes() >= 3 * 256 * 4);
        arenas.reset();
    }
}
