//! Fuzz target: the dense and the map cache against an ordered map.

#![no_main]

extern crate alloc;

use alloc::collections::BTreeMap;
use libfuzzer_sys::fuzz_target;
use vulcan::{
    cache::{DenseCache, MapCache, QueryCache as _},
    convert::usize_from_u32,
    division::Divide as _,
};

vulcan::newtype_index! {
    /// The key type of the fuzzed dense cache.
    struct Key;
}

// Drives a dense cache and a map cache with the same lookups, against an ordered map as the
// model. The first byte picks the capacity; every following pair of bytes is an operation and a
// key. Keys stay below the capacity of the dense cache, and a lookup that would insert a key past
// the capacity of the map cache is skipped, since both panic by design (the unit tests cover it).
fuzz_target!(|data: &[u8]| {
    let Some((&capacity_byte, operations)) = data.split_first() else { return };
    let capacity = u32::from(capacity_byte) + 1;
    let dense = DenseCache::<Key, u16>::with_capacity(capacity);
    let map = MapCache::<u32, u16>::with_capacity(capacity);
    let mut model: BTreeMap<u32, u16> = BTreeMap::new();

    for &[operation, operand] in operations.as_chunks::<2>().0 {
        // The value a miss stores: the whole pair, so values differ between lookups.
        let step = u16::from(operand) * 256 + u16::from(operation);
        let key = u32::from(operand).remainder(capacity);
        let expected = model.get(&key).copied();
        if operation.remainder(2) == 0 {
            assert_eq!(dense.get(Key::from_u32(key)), expected, "a dense read matches the model");
            assert_eq!(map.get(key), expected, "a map read matches the model");
        } else {
            let fits = usize_from_u32(capacity) > model.len();
            let known = expected.is_some();
            let insertable = if fits { true } else { known };
            if insertable {
                let stored = *model.entry(key).or_insert(step);
                let dense_value = dense.get_or_insert_with(Key::from_u32(key), || step);
                let map_value = map.get_or_insert_with(key, || step);
                assert_eq!(dense_value, stored, "the first value stored wins in the dense cache");
                assert_eq!(map_value, stored, "the first value stored wins in the map cache");
            }
        }
    }
});
