//! Fuzz target: a `Cursor` against a plain offset into the input.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vulcan::{
    bytes::{Cursor, is_identifier_continue, is_whitespace},
    convert::usize_from_u32,
    division::Divide as _,
};

/// Returns the length of the whitespace run at the start of `bytes`, by the standard library.
fn whitespace_length(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .take_while(|byte| if byte.is_ascii_whitespace() { true } else { **byte == 0x0b })
        .count()
}

/// Returns the length of the identifier run at the start of `bytes`, by the standard library.
fn identifier_length(bytes: &[u8]) -> usize {
    let continues = |byte: &&u8| {
        if byte.is_ascii_alphanumeric() { true } else { matches!(**byte, b'_' | b'$') }
    };
    bytes.iter().take_while(continues).count()
}

/// Bytes to search for in others.
struct Search<'input> {
    /// The bytes searched.
    haystack: &'input [u8],
    /// The bytes searched for.
    needle: &'input [u8],
}

/// Returns the index of the first occurrence of the needle in the haystack, by a naive search.
fn find(search: &Search<'_>) -> Option<usize> {
    search.haystack.windows(search.needle.len()).position(|window| window == search.needle)
}

/// Returns a needle of one to four bytes copied from `rest`, so that searches often succeed.
fn needle_from(rest: &[u8], selector: u8) -> &[u8] {
    let start = usize::from(selector >> 2_u32).remainder(rest.len().max(1));
    let length = usize::from(selector & 3) + 1;
    let needle: &[u8] = rest.get(start..).map_or(&[], |tail| &tail[..length.min(tail.len())]);
    assert!(needle.len() <= 4, "a needle of four bytes at most");
    needle
}

// Drives a cursor and a model, a plain offset into the input, with the same operations. The first
// byte says how many of the following bytes are operations; the bytes after them are the input.
// Each operation is a byte that picks the method and a byte that picks its argument. After each
// operation the cursor and the model must agree on the offset, the rest and the next byte.
fuzz_target!(|data: &[u8]| {
    let Some((&operation_size, tail)) = data.split_first() else { return };
    let (operations, input) = tail.split_at(usize::from(operation_size).min(tail.len()));
    let mut cursor = Cursor::new(input);
    let mut model = 0_usize;

    for pair in operations.chunks(2) {
        let selector = pair.get(1).copied().map_or(0, |byte| byte);
        let rest = &input[model..];
        match pair[0].remainder(7) {
            0 => {
                let expected = rest.first().copied();
                model += usize::from(expected.is_some());
                assert_eq!(cursor.bump(), expected, "`bump` returns the next byte");
            }
            1 => {
                let length = whitespace_length(rest);
                model += length;
                assert_eq!(cursor.eat_while(is_whitespace), &rest[..length], "the whitespace run");
            }
            2 => {
                let length = identifier_length(rest);
                model += length;
                assert_eq!(
                    cursor.eat_while(is_identifier_continue),
                    &rest[..length],
                    "the identifier run"
                );
            }
            3 => {
                // A prefix copied from the input matches when it was copied from its start.
                let prefix = needle_from(rest, selector);
                if prefix.is_empty() {
                    assert!(rest.is_empty(), "only an empty rest gives an empty prefix");
                } else {
                    let expected = rest.starts_with(prefix);
                    model += if expected { prefix.len() } else { 0 };
                    assert_eq!(cursor.eat_prefix(prefix), expected, "`eat_prefix` matches");
                }
            }
            4 => {
                let skipped_length = rest.iter().position(|byte| *byte == selector);
                model += skipped_length.map_or(rest.len(), |length| length);
                let skipped = cursor.skip_until_byte(selector);
                assert_eq!(skipped, &rest[..skipped.len()], "the skipped bytes come first");
            }
            5 => {
                let needle = needle_from(rest, selector);
                if needle.is_empty() {
                    assert!(rest.is_empty(), "only an empty rest gives an empty needle");
                } else {
                    let found = find(&Search { haystack: rest, needle });
                    model += found.map_or(rest.len(), |start| start + needle.len());
                    assert_eq!(cursor.skip_past(needle), found.is_some(), "`skip_past` finds it");
                }
            }
            _ => {
                // A needle chosen by the fuzzer, which is usually absent from the input.
                let needle = [selector, pair[0]];
                let found = find(&Search { haystack: rest, needle: &needle });
                model += found.map_or(rest.len(), |start| start + needle.len());
                assert_eq!(cursor.skip_past(&needle), found.is_some(), "`skip_past` finds it");
            }
        }
        assert_eq!(usize_from_u32(cursor.offset()), model, "the offset matches the model");
        assert_eq!(cursor.rest(), &input[model..], "the rest matches the model");
        assert_eq!(cursor.first(), input.get(model).copied(), "the next byte matches the model");
        assert_eq!(cursor.is_eof(), model == input.len(), "the end matches the model");
    }
});
