//! Fuzz target: two `ShardedMap`s and their merge against ordered maps.

#![no_main]

extern crate alloc;

use alloc::collections::BTreeMap;
use libfuzzer_sys::fuzz_target;
use vulcan::{
    convert::u32_from_usize,
    sync::{SHARD_COUNT, ShardedMap},
};

/// Room for every `u8` key in every shard, so no key distribution can fill a shard.
const CAPACITY: u32 = 256 * SHARD_COUNT;

// Drives two sharded maps and ordered models with the same operations, then merges the maps.
// Each operation is two bytes: a key and a value. Even keys go to the first map, odd keys to the
// second, so the merged maps hold disjoint keys. An odd value turns the operation into a lookup.
fuzz_target!(|data: &[u8]| {
    let mut maps = [ShardedMap::with_capacity(CAPACITY), ShardedMap::with_capacity(CAPACITY)];
    let mut models = [BTreeMap::new(), BTreeMap::new()];

    for &[key, value] in data.as_chunks::<2>().0 {
        let side = usize::from(key & 1);
        if value & 1 == 0 {
            let previous = maps[side].insert(key, value);
            assert_eq!(previous, models[side].insert(key, value), "insert matches the model");
        } else {
            assert_eq!(maps[side].get(&key), models[side].get(&key), "get matches the model");
        }
        let model_len = u32_from_usize(models[side].len());
        assert_eq!(maps[side].len(), model_len, "len matches the model");
    }

    let [mut merged, mut second] = maps;
    merged.merge_from(&mut second);
    assert!(second.is_empty(), "the merged map took every entry");
    let mut entries: Vec<(u8, u8)> = merged.iter().map(|(&key, &value)| (key, value)).collect();
    entries.sort_unstable();
    let mut model: Vec<(u8, u8)> =
        models.iter().flat_map(|model| model.iter().map(|(&key, &value)| (key, value))).collect();
    model.sort_unstable();
    assert_eq!(entries, model, "the merged map holds every entry of both maps");
});
