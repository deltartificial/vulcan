#![no_main]

use libfuzzer_sys::fuzz_target;
use vulcan::{
    convert::{u32_from_usize, usize_from_u32},
    index::IndexVec,
};

vulcan::newtype_index! {
    /// The index type of the fuzzed vector.
    struct Item;
}

/// Checks that the vector and the reference model hold the same elements.
fn assert_same(vector: &IndexVec<Item, u8>, model: &[u8]) {
    assert_eq!(vector.as_raw_slice(), model, "the vector matches the model");
    assert_eq!(usize_from_u32(vector.len()), model.len(), "the length matches the model");
    assert_eq!(vector.is_empty(), model.is_empty(), "emptiness matches the model");
    assert_eq!(vector.next_index().as_usize(), model.len(), "the next index is the length");
    let expected = (0..model.len()).map(|index| Item::from_u32(u32_from_usize(index)));
    assert!(vector.indices().eq(expected), "the indices are `0..len` in order");
}

// Drives an `IndexVec` and a `Vec` model with the same operations. The first byte picks the
// capacity; every following pair of bytes is an operation and an operand. A push on a full vector
// is skipped, since it panics by design (the unit tests cover that), and reads probe indices up to
// the capacity, both inside and past the length.
fuzz_target!(|data: &[u8]| {
    let Some((&capacity_byte, operations)) = data.split_first() else { return };
    let capacity = u32::from(capacity_byte) + 1;
    let mut vector = IndexVec::<Item, u8>::with_capacity(capacity);
    let mut model: Vec<u8> = Vec::with_capacity(usize_from_u32(capacity));
    assert_eq!(vector.capacity(), capacity, "the vector keeps its capacity");

    for pair in operations.chunks_exact(2) {
        let operand = pair[1];
        let probe = u32::from(operand) % capacity;
        let index = Item::from_u32(probe);
        let slot = usize_from_u32(probe);
        match pair[0] % 4 {
            0 => {
                if vector.len() < capacity {
                    let pushed = vector.push(operand);
                    model.push(operand);
                    assert_eq!(pushed.as_usize() + 1, model.len(), "a push returns the last index");
                }
            }
            1 => assert_eq!(vector.get(index), model.get(slot), "a read matches the model"),
            2 => {
                if let Some(element) = vector.get_mut(index) {
                    *element = operand;
                    model[slot] = operand;
                }
            }
            _ => {
                let copy = vector.clone();
                assert_eq!(copy.capacity(), vector.capacity(), "a clone keeps the capacity");
                assert_eq!(
                    copy.as_raw_slice(),
                    vector.as_raw_slice(),
                    "a clone keeps the elements"
                );
            }
        }
        assert_same(&vector, &model);
    }
});
