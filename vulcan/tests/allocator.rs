//! Installs the selected allocator, wrapped in a `StaticAllocator`, as the global allocator of
//! this test binary, and checks that it serves allocations while it is open.
//!
//! Sealing is tested in `tests/static_allocation.rs`, a binary without the test harness, since
//! the harness itself allocates after every test.

extern crate alloc;

use vulcan::allocator::{Allocator, BACKEND, StaticAllocator};

/// The global allocator of this binary, never sealed.
#[global_allocator]
static ALLOCATOR: StaticAllocator<Allocator> = StaticAllocator::new(BACKEND);

#[cfg(test)]
mod tests {
    use super::ALLOCATOR;
    use alloc::collections::BTreeMap;
    use vulcan::{convert::usize_from_u32, division::Divide as _};

    /// Fills a map with vectors of varied lengths, which exercises allocation, reallocation and
    /// freeing through the wrapper, then checks every byte count.
    #[test]
    fn global_allocator_serves_mixed_workload() {
        assert!(!ALLOCATOR.is_sealed(), "the harness never seals the allocator");
        let mut map = BTreeMap::new();
        for key in 0_u32..1_000 {
            let previous = map.insert(key, vec![key; usize_from_u32(key.remainder(17))]);
            assert!(previous.is_none(), "every key is new");
        }
        let total: usize = map.values().map(Vec::len).sum();
        let expected: usize = (0_u32..1_000).map(|key| usize_from_u32(key.remainder(17))).sum();
        assert_eq!(total, expected, "every vector kept its length");
    }

    /// Grows one vector from empty through many reallocations, with an alignment above the
    /// default, and checks that the contents survive every move.
    #[test]
    fn global_allocator_keeps_contents_across_reallocations() {
        /// A value aligned to a cache line, so every move must keep a 64-byte alignment.
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        #[repr(align(64))]
        struct Aligned(u32);

        let mut values = Vec::new();
        for value in 0_u32..512 {
            values.push(Aligned(value));
        }
        assert_eq!(values.as_ptr().addr() & 63, 0, "the allocation keeps the alignment");
        assert!(values.iter().zip(0_u32..).all(|(aligned, value)| aligned.0 == value));
    }
}
