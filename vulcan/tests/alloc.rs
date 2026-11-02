//! Installs the selected allocator as the global allocator of this test binary.

#[global_allocator]
static ALLOC: vulcan::alloc::Allocator = vulcan::alloc::new_allocator();

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    #[test]
    fn global_allocator_serves_mixed_workload() {
        let mut map = BTreeMap::new();
        for i in 0_u32..1_000 {
            map.insert(i, vec![i; (i % 17) as usize]);
        }
        let total: usize = map.values().map(Vec::len).sum();
        assert_eq!(total, (0_u32..1_000).map(|i| (i % 17) as usize).sum::<usize>());
    }
}
