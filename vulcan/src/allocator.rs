//! Global allocator selection, and memory allocated once at startup.
//!
//! A library cannot install the global allocator on behalf of its users, so this module only
//! resolves the [`Allocator`] type and its [`BACKEND`] handle to the fastest backend enabled
//! through Cargo features, and wraps it in a [`StaticAllocator`]. The final binary installs it,
//! allocates everything it needs while it starts, then seals it:
//!
//! ```
//! use vulcan::allocator::{Allocator, BACKEND, StaticAllocator};
//!
//! #[global_allocator]
//! static ALLOCATOR: StaticAllocator<Allocator> = StaticAllocator::new(BACKEND);
//!
//! fn main() {
//!     // Initialization: every buffer the program will ever use is allocated here.
//!     let words = vec![1_u64; 1024];
//!     assert_eq!(words.len(), 1024);
//!     // The doctest harness keeps allocating after `main`, so it does not call `seal` here. A
//!     // real program would now run with `ALLOCATOR.seal()` in force.
//!     assert!(!ALLOCATOR.is_sealed());
//! }
//! ```
//!
//! Allocation-heavy workloads (parsers, graph builders) commonly gain 5 to 20 percent from
//! `mimalloc` or `jemalloc` over the system allocator.
//!
//! Precedence when several features are enabled: `jemalloc`, then `mimalloc`, then the system
//! allocator. `jemalloc` is ignored on MSVC targets.

/// The `jemalloc` backend.
#[cfg(all(feature = "jemalloc", not(target_env = "msvc")))]
mod backend {
    /// The selected global allocator.
    pub type Allocator = tikv_jemallocator::Jemalloc;

    /// The handle of the selected global allocator.
    pub const BACKEND: Allocator = tikv_jemallocator::Jemalloc;

    /// The name of the selected global allocator, for logs and diagnostics.
    pub const BACKEND_NAME: &str = "jemalloc";
}

/// The `mimalloc` backend.
#[cfg(all(feature = "mimalloc", not(all(feature = "jemalloc", not(target_env = "msvc")))))]
mod backend {
    /// The selected global allocator.
    pub type Allocator = mimalloc::MiMalloc;

    /// The handle of the selected global allocator.
    pub const BACKEND: Allocator = mimalloc::MiMalloc;

    /// The name of the selected global allocator, for logs and diagnostics.
    pub const BACKEND_NAME: &str = "mimalloc";
}

/// The system allocator, when no faster backend is enabled.
#[cfg(not(any(feature = "mimalloc", all(feature = "jemalloc", not(target_env = "msvc")))))]
mod backend {
    use std::alloc::System;

    /// The selected global allocator.
    pub type Allocator = System;

    /// The handle of the selected global allocator.
    pub const BACKEND: Allocator = System;

    /// The name of the selected global allocator, for logs and diagnostics.
    pub const BACKEND_NAME: &str = "system";
}

pub use backend::{Allocator, BACKEND, BACKEND_NAME};
use core::{
    alloc::{GlobalAlloc, Layout},
    hint,
    sync::atomic::{AtomicBool, Ordering},
};
use std::process;

// Every backend is a zero-sized handle, so a `static` allocator costs no memory.
const _: () = assert!(size_of::<Allocator>() == 0, "allocator handles are zero-sized");

// With both features enabled (`--all-features`), jemalloc wins and `mimalloc` would be reported
// as an unused dependency.
#[cfg(all(feature = "mimalloc", feature = "jemalloc", not(target_env = "msvc")))]
use mimalloc as _;

/// Asserts a condition inside the allocator, aborting the process when it does not hold.
///
/// A panic is not an option here: unwinding out of a global allocator is undefined behavior, and
/// a panic may allocate, which would call back into the allocator. The message is handed to the
/// aborting function, where a debugger or a core dump shows it.
macro_rules! allocator_assert {
    ($condition:expr, $message:literal $(,)?) => {
        if $condition {
        } else {
            allocator_assert_abort($message)
        }
    };
}

/// A global allocator that serves allocations until it is sealed, and aborts the process on any
/// allocation after that.
///
/// All memory is allocated at startup: a program sizes its buffers, arenas and tables from its
/// configured limits, allocates them, and then calls [`seal`](Self::seal). From then on an
/// allocation or a reallocation is a programmer error, so it aborts the process at once instead
/// of adding unpredictable latency or running out of memory later. Freeing stays allowed, for the
/// teardown at exit.
///
/// The seal check is one relaxed load per allocation, and every call also checks the caller's
/// side of the `GlobalAlloc` contract (a non-zero size, a non-null pointer) and the backend's (an
/// aligned result): a few compares next to the cost of an allocation.
#[derive(Debug)]
pub struct StaticAllocator<A> {
    /// The backend that serves the allocations.
    inner: A,
    /// Whether initialization is over, after which every allocation aborts.
    sealed: AtomicBool,
}

impl<A> StaticAllocator<A> {
    /// Wraps `inner`, open for allocation.
    #[must_use = "an allocator does nothing until it is installed"]
    pub const fn new(inner: A) -> Self {
        Self { inner, sealed: AtomicBool::new(false) }
    }

    /// Ends initialization: every later allocation aborts the process.
    ///
    /// # Panics
    ///
    /// Panics if the allocator is already sealed: initialization ends once.
    pub fn seal(&self) {
        // Relaxed: the flag guards no other data. A thread that runs concurrently with `seal`
        // may complete one more allocation, which is still part of initialization.
        let sealed_before = self.sealed.swap(true, Ordering::Relaxed);
        assert!(!sealed_before, "initialization ends once");
        assert!(self.is_sealed(), "the allocator is sealed");
    }

    /// Returns `true` once [`seal`](Self::seal) has been called.
    #[must_use = "the query has no side effect"]
    pub fn is_sealed(&self) -> bool {
        // Relaxed: see `seal`.
        self.sealed.load(Ordering::Relaxed)
    }
}

/// Aborts the process on a failed [`allocator_assert!`].
///
/// Silently: a panic, or even a write to standard error, may allocate, which would call back into
/// this allocator. The allocating call is at the top of the stack in a debugger or a core dump.
#[cold]
#[inline(never)]
fn allocator_assert_abort(message: &'static str) -> ! {
    // Kept alive so the message names the broken invariant in a debugger or a core dump.
    let _message = hint::black_box(message);
    process::abort()
}

#[expect(unsafe_code, reason = "a global allocator implements the `unsafe` trait `GlobalAlloc`")]
#[expect(
    clippy::renamed_function_params,
    reason = "the trait's `ptr` and `new_size` are abbreviated or put the qualifier first"
)]
// SAFETY: every method forwards to `inner`, a `GlobalAlloc`, with the caller's arguments
// unchanged, so `inner`'s guarantees carry over. The added checks only read the arguments and the
// results, and abort the process when one fails: they never unwind.
unsafe impl<A: GlobalAlloc> GlobalAlloc for StaticAllocator<A> {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        allocator_assert!(!self.is_sealed(), "no allocation after the seal");
        allocator_assert!(layout.size() > 0, "an allocation has a non-zero size");
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract, which `inner` requires.
        let pointer = unsafe { self.inner.alloc(layout) };
        allocator_assert!(pointer.addr() & (layout.align() - 1) == 0, "the backend aligns");
        pointer
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        allocator_assert!(!self.is_sealed(), "no allocation after the seal");
        allocator_assert!(layout.size() > 0, "an allocation has a non-zero size");
        // SAFETY: the caller upholds `GlobalAlloc::alloc_zeroed`'s contract.
        let pointer = unsafe { self.inner.alloc_zeroed(layout) };
        allocator_assert!(pointer.addr() & (layout.align() - 1) == 0, "the backend aligns");
        pointer
    }

    #[inline]
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        allocator_assert!(!pointer.is_null(), "a freed pointer came from an allocation");
        allocator_assert!(layout.size() > 0, "a freed allocation has a non-zero size");
        allocator_assert!(pointer.addr() & (layout.align() - 1) == 0, "a freed pointer is aligned");
        // SAFETY: the caller upholds `GlobalAlloc::dealloc`'s contract: `pointer` came from this
        // allocator, which got it from `inner`, with this `layout`.
        unsafe { self.inner.dealloc(pointer, layout) }
    }

    #[inline]
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size_new: usize) -> *mut u8 {
        allocator_assert!(!self.is_sealed(), "no reallocation after the seal");
        allocator_assert!(!pointer.is_null(), "a reallocated pointer came from an allocation");
        allocator_assert!(size_new > 0, "a reallocation has a non-zero size");
        // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract, as for `dealloc`.
        let moved = unsafe { self.inner.realloc(pointer, layout, size_new) };
        allocator_assert!(moved.addr() & (layout.align() - 1) == 0, "the backend aligns");
        moved
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::any;

    /// Compares the backend name with the backend type, two facts set by the same `cfg` that
    /// must agree: the name appears, in lower case, in the type's path.
    #[test]
    fn backend_name_matches_backend_type() {
        let type_name = any::type_name::<Allocator>().to_lowercase();
        assert!(type_name.contains(BACKEND_NAME), "{type_name} is the {BACKEND_NAME} backend");
        let expected = if cfg!(all(feature = "jemalloc", not(target_env = "msvc"))) {
            "jemalloc"
        } else {
            if cfg!(feature = "mimalloc") { "mimalloc" } else { "system" }
        };
        assert_eq!(BACKEND_NAME, expected, "the features select the backend");
    }

    /// Seals a local allocator that is not installed: the flag flips once, and the allocator
    /// still serves the open-state calls made before it.
    #[test]
    #[expect(unsafe_code, reason = "calls the `GlobalAlloc` methods of an uninstalled allocator")]
    fn seal_flips_the_flag_once() {
        let allocator = StaticAllocator::new(BACKEND);
        assert!(!allocator.is_sealed(), "a new allocator is open");
        let layout = Layout::new::<u64>();
        // SAFETY: the layout has a non-zero size, and the pointer is freed with the same layout.
        let pointer = unsafe { allocator.alloc(layout) };
        assert!(!pointer.is_null(), "the open allocator serves the allocation");
        allocator.seal();
        assert!(allocator.is_sealed(), "a sealed allocator reports it");
        // SAFETY: `pointer` came from `allocator` with `layout`, and freeing after the seal is
        // allowed.
        unsafe {
            allocator.dealloc(pointer, layout);
        }
    }

    /// Seals an allocator twice, which is a programmer error: initialization ends once.
    #[test]
    #[should_panic(expected = "initialization ends once")]
    fn seal_rejects_a_second_seal() {
        let allocator = StaticAllocator::new(BACKEND);
        allocator.seal();
        allocator.seal();
    }
}
