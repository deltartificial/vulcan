//! Fuzz target: a `DenseBitSet` against a `Vec<bool>` model.

#![no_main]

use core::mem;
use libfuzzer_sys::fuzz_target;
use vulcan::{bit_set::DenseBitSet, convert::usize_from_u32, division::Divide as _};

vulcan::newtype_index! {
    /// The index type of the fuzzed sets.
    struct Node;
}

/// Checks that the set and the reference model hold the same indices.
fn assert_same(set: &DenseBitSet<Node>, model: &[bool]) {
    let expected: Vec<u32> =
        (0_u32..).zip(model).filter(|(_, present)| **present).map(|(index, _)| index).collect();
    let actual: Vec<u32> = set.iter().map(Node::as_u32).collect();
    assert_eq!(actual, expected, "the set matches the model");
    assert_eq!(usize_from_u32(set.count()), expected.len(), "the count matches the model");
    assert_eq!(set.is_empty(), expected.is_empty(), "emptiness matches the model");
}

/// Applies `operation` to every bit of `model` with the matching bit of `other_model`.
fn combine_model<Operation>(model: &mut [bool], other_model: &[bool], operation: Operation)
where
    Operation: Fn(bool, bool) -> bool,
{
    assert_eq!(model.len(), other_model.len(), "models over one domain");
    for (bit, &other_bit) in model.iter_mut().zip(other_model) {
        *bit = operation(*bit, other_bit);
    }
}

// Drives a bit set and a `Vec<bool>` model with the same operations. The first byte picks the
// domain count; every following pair of bytes is an operation and an index. Domains that end
// inside a word and domains that fill it exactly are both reached, which is where excess bits
// would leak.
fuzz_target!(|data: &[u8]| {
    let Some((&domain_byte, operations)) = data.split_first() else { return };
    let domain_count = u32::from(domain_byte) + 1;
    let mut set = DenseBitSet::<Node>::new_empty(domain_count);
    let mut other = DenseBitSet::<Node>::new_empty(domain_count);
    let mut model = vec![false; usize_from_u32(domain_count)];
    let mut other_model = vec![false; usize_from_u32(domain_count)];
    assert_eq!(set.domain_count(), domain_count, "the set keeps its domain");

    for &[operation, operand] in operations.as_chunks::<2>().0 {
        let index = u32::from(operand).remainder(domain_count);
        let node = Node::from_u32(index);
        let slot = usize_from_u32(index);
        match operation.remainder(10) {
            0 => {
                let absent = !mem::replace(&mut model[slot], true);
                assert_eq!(set.insert(node), absent, "an insert reports a new index");
            }
            1 => {
                let present = mem::replace(&mut model[slot], false);
                assert_eq!(set.remove(node), present, "a removal reports a held index");
            }
            2 => assert_eq!(set.contains(node), model[slot], "a lookup matches the model"),
            3 => {
                let _inserted = other.insert(node);
                other_model[slot] = true;
            }
            4 => {
                let changed = set.union(&other);
                let before = model.clone();
                combine_model(
                    &mut model,
                    &other_model,
                    |bit, other_bit| if bit { true } else { other_bit },
                );
                assert_eq!(changed, before != model, "a union reports a change");
            }
            5 => {
                let changed = set.intersect(&other);
                let before = model.clone();
                combine_model(
                    &mut model,
                    &other_model,
                    |bit, other_bit| if bit { other_bit } else { false },
                );
                assert_eq!(changed, before != model, "an intersection reports a change");
            }
            6 => {
                let changed = set.subtract(&other);
                let before = model.clone();
                combine_model(
                    &mut model,
                    &other_model,
                    |bit, other_bit| if bit { !other_bit } else { false },
                );
                assert_eq!(changed, before != model, "a difference reports a change");
            }
            7 => {
                set.clear();
                model.fill(false);
            }
            8 => {
                let _removed = other.remove(node);
                other_model[slot] = false;
            }
            _ => {
                set.insert_all();
                model.fill(true);
            }
        }
        assert_same(&set, &model);
        assert_same(&other, &other_model);
    }
});
