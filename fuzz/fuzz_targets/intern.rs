#![no_main]

use libfuzzer_sys::fuzz_target;
use std::collections::HashMap;
use vulcan::{
    arena::{Bump, BumpExtension as _},
    convert::{u32_from_usize, usize_from_u32},
    intern::{Interner, Symbol},
};

/// The longest word an operation interns, in bytes.
const WORD_SIZE_BYTES_MAX: u32 = 4;

// Drives the interner and a model (a `Vec<String>` of the strings in insertion order, and a map
// from string to position) with the same operations. The first byte picks the capacity; each
// following pair of bytes is an operation and its argument. Interning the same string twice
// gives the same symbol, resolving a symbol gives its string back, and a new string is interned
// only while the model holds fewer strings than the capacity.
fuzz_target!(|data: &[u8]| {
    let Some((&capacity_byte, operations)) = data.split_first() else { return };
    let capacity = u32::from(capacity_byte % 16) + 1;
    let arena = Bump::with_fixed_capacity(capacity * WORD_SIZE_BYTES_MAX);
    let mut interner = Interner::with_capacity(&arena, capacity);
    let mut strings: Vec<String> = Vec::new();
    let mut positions: HashMap<String, u32> = HashMap::new();

    for pair in operations.chunks_exact(2) {
        // Words from a three-letter alphabet, up to four letters long, so they repeat often.
        let argument = pair[1];
        let word_size_bytes = usize::from(argument % 5);
        let word: String =
            (0..word_size_bytes).map(|i| char::from(b'a' + (argument >> i) % 3)).collect();
        match pair[0] % 3 {
            0 => {
                let known = positions.get(&word).copied();
                let has_room = strings.len() < usize_from_u32(capacity);
                if let Some(position) = known {
                    assert_eq!(interner.intern(&word), Symbol::from_u32(position), "same symbol");
                } else {
                    if has_room {
                        let symbol = interner.intern(&word);
                        assert_eq!(usize_from_u32(symbol.as_u32()), strings.len(), "a new symbol");
                        let _previous = positions.insert(word.clone(), symbol.as_u32());
                        strings.push(word);
                    } else {
                        // A new string past the capacity crashes; check it was never stored.
                        assert!(interner.get(&word).is_none(), "a full interner has no new string");
                    }
                }
            }
            1 => {
                let expected = positions.get(&word).map(|&position| Symbol::from_u32(position));
                assert_eq!(interner.get(&word), expected, "a lookup matches the model");
            }
            _ => {
                if let Some(count) = u32::try_from(strings.len()).ok().filter(|&count| count > 0) {
                    let position = u32::from(argument) % count;
                    let resolved = interner.resolve(Symbol::from_u32(position));
                    assert_eq!(resolved, strings[usize_from_u32(position)], "resolve round-trips");
                }
            }
        }
        assert_eq!(interner.len(), u32_from_usize(strings.len()), "the counts match");
        assert!(interner.len() <= capacity, "the capacity is respected");
    }
});
