#![no_main]

use libfuzzer_sys::fuzz_target;
use vulcan::{
    bytes::{Cursor, is_id_continue, is_whitespace},
    intern::Interner,
};

// Lexes arbitrary bytes into identifier-like words and interns them. The cursor must account for
// every byte exactly once, and each interned word must resolve back to itself.
fuzz_target!(|data: &[u8]| {
    let mut cursor = Cursor::new(data);
    let mut interner = Interner::new();
    let mut consumed = 0;

    while !cursor.is_eof() {
        let before = cursor.pos();
        consumed += cursor.eat_while(is_whitespace).len();

        let word = cursor.eat_while(is_id_continue);
        consumed += word.len();
        if let Ok(word) = std::str::from_utf8(word) {
            let symbol = interner.intern(word);
            assert_eq!(interner.resolve(symbol), word);
        }

        // Neither class matched: skip one byte so the loop always makes progress.
        if cursor.pos() == before {
            consumed += usize::from(cursor.bump().is_some());
        }
        assert_eq!(cursor.pos(), consumed);
    }

    assert_eq!(consumed, data.len());
});
