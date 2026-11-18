//! Byte-level scanning: table-driven classification and SIMD searches.
//!
//! Fast lexers work on bytes, not `char`s. They classify each byte with a single load from a
//! 256-entry table instead of a chain of range comparisons, and jump over comments with
//! `memchr`/`memmem`, which use SIMD where available. This module packages both techniques for
//! any hand-written scanner.
//!
//! ```
//! use vulcan::bytes::{Cursor, is_identifier_continue, is_identifier_start, is_whitespace};
//!
//! let mut cursor = Cursor::new(b"owner /* skip me */ balance");
//! let identifier = cursor.eat_while(is_identifier_continue);
//! assert_eq!(identifier, b"owner");
//! let _whitespace = cursor.eat_while(is_whitespace);
//! assert!(cursor.eat_prefix(b"/*"));
//! assert!(cursor.skip_past(b"*/"));
//! let _whitespace = cursor.eat_while(is_whitespace);
//! assert!(cursor.first().is_some_and(is_identifier_start));
//! assert_eq!(cursor.eat_while(is_identifier_continue), b"balance");
//! assert!(cursor.is_eof());
//! ```

use crate::{
    convert::{u32_from_usize, usize_from_u32},
    hint::cold_path,
};
use core::ptr;
use memchr::memmem::{self, FinderBuilder, Prefilter};
use std::sync::OnceLock;

/// Bit flag: ASCII whitespace (space, `\t`, `\n`, `\r`, form feed, vertical tab).
pub const WHITESPACE: u8 = 1 << 0_u8;
/// Bit flag: valid first byte of an identifier (`a-z`, `A-Z`, `_`, `$`).
pub const IDENTIFIER_START: u8 = 1 << 1_u8;
/// Bit flag: valid byte inside an identifier (`IDENTIFIER_START` plus `0-9`).
pub const IDENTIFIER_CONTINUE: u8 = 1 << 2_u8;
/// Bit flag: decimal digit.
pub const DIGIT: u8 = 1 << 3_u8;
/// Bit flag: hexadecimal digit.
pub const HEX_DIGIT: u8 = 1 << 4_u8;

// Each class is its own bit, so classes combine with `|` and test with `&`.
const _: () = assert!(
    (WHITESPACE | IDENTIFIER_START | IDENTIFIER_CONTINUE | DIGIT | HEX_DIGIT).count_ones() == 5,
    "byte classes must be distinct bits"
);

// Spot checks of the classes, on both sides of each class boundary, where an off-by-one in a range
// would show.
const _: () = assert!(byte_classes(b' ') & WHITESPACE != 0, "a space is whitespace");
const _: () = assert!(byte_classes(b'\t') & WHITESPACE != 0, "a tab is whitespace");
const _: () = assert!(byte_classes(b'\r') & WHITESPACE != 0, "a carriage return is whitespace");
const _: () = assert!(byte_classes(0x08) & WHITESPACE == 0, "a backspace is no whitespace");
const _: () = assert!(byte_classes(0x0e) & WHITESPACE == 0, "shift out is no whitespace");
const _: () = assert!(byte_classes(b'a') & IDENTIFIER_START != 0, "`a` starts an identifier");
const _: () = assert!(byte_classes(b'z') & IDENTIFIER_START != 0, "`z` starts an identifier");
const _: () = assert!(byte_classes(b'`') & IDENTIFIER_START == 0, "`` ` `` starts no identifier");
const _: () = assert!(byte_classes(b'{') & IDENTIFIER_START == 0, "`{{` starts no identifier");
const _: () = assert!(byte_classes(b'@') & IDENTIFIER_START == 0, "`@` starts no identifier");
const _: () = assert!(byte_classes(b'[') & IDENTIFIER_START == 0, "`[` starts no identifier");
const _: () = assert!(byte_classes(b'9') & IDENTIFIER_START == 0, "a digit starts no identifier");
const _: () = assert!(byte_classes(b'9') & IDENTIFIER_CONTINUE != 0, "a digit continues one");
const _: () = assert!(byte_classes(b'/') & DIGIT == 0, "`/` is no digit");
const _: () = assert!(byte_classes(b':') & DIGIT == 0, "`:` is no digit");
const _: () = assert!(byte_classes(b'F') & HEX_DIGIT != 0, "`F` is a hex digit");
const _: () = assert!(byte_classes(b'G') & HEX_DIGIT == 0, "`G` is no hex digit");
const _: () = assert!(byte_classes(0x80) == 0, "non-ASCII bytes are in no class");

/// The classification table, computed at compile time, in one place in memory.
static TABLE: [u8; 256] = build_table();

/// Returns the classification flags of `byte`.
#[inline]
#[must_use = "a classification has no side effect"]
pub fn classify(byte: u8) -> u8 {
    // A `u8` index cannot leave a 256-entry table, so this compiles to one load.
    TABLE[usize::from(byte)]
}

/// Returns `true` if `byte` is ASCII whitespace.
#[inline]
#[must_use = "a classification has no side effect"]
pub fn is_whitespace(byte: u8) -> bool {
    classify(byte) & WHITESPACE != 0
}

/// Returns `true` if `byte` can start an identifier.
#[inline]
#[must_use = "a classification has no side effect"]
pub fn is_identifier_start(byte: u8) -> bool {
    classify(byte) & IDENTIFIER_START != 0
}

/// Returns `true` if `byte` can continue an identifier.
#[inline]
#[must_use = "a classification has no side effect"]
pub fn is_identifier_continue(byte: u8) -> bool {
    classify(byte) & IDENTIFIER_CONTINUE != 0
}

/// Returns `true` if `byte` is a hexadecimal digit.
#[inline]
#[must_use = "a classification has no side effect"]
pub fn is_hex_digit(byte: u8) -> bool {
    classify(byte) & HEX_DIGIT != 0
}

/// A forward-only cursor over a byte slice of at most `u32::MAX` bytes.
///
/// The offset is computed from the length of the remaining input, so advancing is a slice
/// re-borrow and no separate counter has to be kept in sync.
#[derive(Clone, Debug)]
pub struct Cursor<'input> {
    /// The whole input, kept to compute the offset.
    input: &'input [u8],
    /// The bytes not consumed yet. Invariant: a suffix of `input`.
    rest: &'input [u8],
}

impl<'input> Cursor<'input> {
    /// Creates a cursor at the start of `input`.
    ///
    /// # Panics
    ///
    /// Panics if `input` is longer than `u32::MAX` bytes, since offsets are `u32`.
    #[inline]
    #[must_use = "a cursor does nothing unless it is advanced"]
    #[track_caller]
    pub const fn new(input: &'input [u8]) -> Self {
        assert!(input.len() <= usize_from_u32(u32::MAX), "the input length fits in `u32`");
        let cursor = Self { input, rest: input };
        assert!(cursor.rest.len() == cursor.input.len(), "a new cursor has consumed nothing");
        cursor
    }

    /// Returns the number of bytes consumed so far.
    ///
    /// # Panics
    ///
    /// Panics only if the rest is not a suffix of the input, which every method preserves.
    #[inline]
    #[must_use = "an accessor has no side effect"]
    pub const fn offset(&self) -> u32 {
        assert!(self.rest.len() <= self.input.len(), "the rest is no longer than the input");
        u32_from_usize(self.input.len() - self.rest.len())
    }

    /// Returns the bytes not consumed yet.
    ///
    /// # Panics
    ///
    /// Panics only if the rest is not a suffix of the input, which every method preserves.
    #[inline]
    #[must_use = "an accessor has no side effect"]
    pub fn rest(&self) -> &'input [u8] {
        assert!(self.rest.len() <= self.input.len(), "the rest is no longer than the input");
        let suffix = &self.input[self.input.len() - self.rest.len()..];
        // Compares address and length, not bytes: the check costs the same for any input.
        assert!(ptr::eq(suffix, self.rest), "the rest is the end of the input");
        self.rest
    }

    /// Returns `true` once every byte has been consumed.
    ///
    /// An accessor on the hot path of every scan loop: it holds no assertion, since the rest is a
    /// suffix of the input by construction: a single private method shortens it.
    #[inline]
    #[must_use = "an accessor has no side effect"]
    pub const fn is_eof(&self) -> bool {
        self.rest.is_empty()
    }

    /// Returns the next byte without consuming it.
    #[inline]
    #[must_use = "an accessor has no side effect"]
    pub const fn first(&self) -> Option<u8> {
        self.rest.first().copied()
    }

    /// Consumes and returns the next byte, or returns `None` at the end of the input.
    ///
    /// # Panics
    ///
    /// Panics only if a split of the rest loses or gains bytes, which slices never do.
    #[inline]
    pub fn bump(&mut self) -> Option<u8> {
        let Some((&byte, rest)) = self.rest.split_first() else {
            assert!(self.rest.is_empty(), "only an empty rest has no next byte");
            return None;
        };
        let rest_length_before = self.rest.len();
        self.rest = rest;
        assert!(self.rest.len() + 1 == rest_length_before, "one byte consumed");
        Some(byte)
    }

    /// Consumes bytes while `predicate` holds and returns them.
    ///
    /// # Panics
    ///
    /// Panics only if the rest is not a suffix of the input, which every method preserves.
    #[inline]
    pub fn eat_while<Predicate>(&mut self, predicate: Predicate) -> &'input [u8]
    where
        Predicate: FnMut(u8) -> bool,
    {
        let matched_length = cursor_eat_while_length(self.rest, predicate);
        assert!(matched_length <= self.rest.len(), "the match lies in the rest");
        self.advance(matched_length)
    }

    /// Consumes `prefix` if the input starts with it, and returns whether it did.
    ///
    /// # Panics
    ///
    /// Panics if `prefix` is empty: it would match everywhere and consume nothing.
    #[inline]
    #[track_caller]
    pub fn eat_prefix(&mut self, prefix: &[u8]) -> bool {
        assert!(!prefix.is_empty(), "an empty prefix matches everywhere");
        if self.rest.starts_with(prefix) {
            // `starts_with` has compared the bytes, so no assertion compares them again.
            let _prefix = self.advance(prefix.len());
            true
        } else {
            false
        }
    }

    /// Consumes everything up to, but not including, the next `byte` (a line end, a quote).
    ///
    /// Returns the skipped bytes. Consumes the rest of the input if `byte` never occurs.
    ///
    /// # Panics
    ///
    /// Panics only if the rest is not a suffix of the input, which every method preserves.
    #[inline]
    pub fn skip_until_byte(&mut self, byte: u8) -> &'input [u8] {
        let skipped_length = match memchr::memchr(byte, self.rest) {
            Some(index) => index,
            None => self.rest.len(),
        };
        let skipped = self.advance(skipped_length);
        if let Some(&last) = skipped.last() {
            assert!(last != byte, "the stop byte is never skipped");
        }
        if let Some(next) = self.first() {
            assert!(next == byte, "the stop byte comes next");
        }
        skipped
    }

    /// Consumes everything up to and including the next `needle` (a block comment end).
    ///
    /// Returns `false`, consuming the rest of the input, if `needle` never occurs.
    ///
    /// Builds a new searcher on every call; in a loop, prefer a [`StaticFinder`] passed to
    /// the method [`skip_past_finder`](Self::skip_past_finder).
    ///
    /// # Panics
    ///
    /// Panics if `needle` is empty: it would match everywhere.
    #[inline]
    #[track_caller]
    pub fn skip_past(&mut self, needle: &[u8]) -> bool {
        self.skip_past_finder(&build_finder(needle))
    }

    /// Like [`skip_past`](Self::skip_past), with a searcher built once by the caller.
    ///
    /// # Panics
    ///
    /// Panics if the needle of `finder` is empty: it would match everywhere.
    #[inline]
    #[track_caller]
    pub fn skip_past_finder(&mut self, finder: &memmem::Finder<'_>) -> bool {
        let needle = finder.needle();
        assert!(!needle.is_empty(), "an empty needle matches everywhere");
        let Some(start) = finder.find(self.rest) else {
            cold_path();
            let _rest = self.advance(self.rest.len());
            assert!(self.rest.is_empty(), "a missing needle consumes the rest");
            return false;
        };
        // The finder has matched the needle at `start`: comparing its bytes again would check
        // nothing new, and cost a compare per comment.
        let _skipped = self.advance(start + needle.len());
        true
    }

    /// Consumes `byte_count` bytes and returns them.
    ///
    /// Every caller derives `byte_count` from the remaining input, so a larger count is a bug in
    /// this file, not bad input.
    #[inline]
    #[track_caller]
    fn advance(&mut self, byte_count: usize) -> &'input [u8] {
        // The only place that shortens the rest, so it stays a suffix of the input: re-checking
        // that on every call would cost a bounds check and a pointer compare per token.
        assert!(byte_count <= self.rest.len(), "advance past the end of the input");
        let (head, rest) = self.rest.split_at(byte_count);
        self.rest = rest;
        head
    }
}

/// Returns the length of the longest prefix of `bytes` whose bytes all satisfy `predicate`.
///
/// The hot loop of [`Cursor::eat_while`], over a plain slice so the compiler need not prove that
/// the cursor stays unchanged while the predicate runs.
#[inline]
fn cursor_eat_while_length<Predicate>(bytes: &[u8], mut predicate: Predicate) -> usize
where
    Predicate: FnMut(u8) -> bool,
{
    let length = bytes.iter().position(|&byte| !predicate(byte)).map_or(bytes.len(), |index| index);
    assert!(length <= bytes.len(), "the matched prefix lies in the bytes");
    length
}

/// A substring searcher for a fixed needle, built on first use and then shared.
///
/// Building a [`memmem::Finder`] analyzes the needle and picks a SIMD strategy; doing it on every
/// search wastes that work. Declare one `static` per fixed needle (a comment terminator, a
/// delimiter) instead:
///
/// ```
/// use vulcan::bytes::{Cursor, StaticFinder};
///
/// static BLOCK_END: StaticFinder = StaticFinder::new(b"*/");
///
/// let mut cursor = Cursor::new(b" a block */ rest");
/// assert!(cursor.skip_past_finder(BLOCK_END.get()));
/// assert_eq!(cursor.rest(), b" rest");
/// ```
#[derive(Debug)]
pub struct StaticFinder {
    /// The bytes to search for, never empty.
    needle: &'static [u8],
    /// The searcher for `needle`, built by the first [`get`](Self::get).
    finder: OnceLock<memmem::Finder<'static>>,
}

impl StaticFinder {
    /// Declares a searcher for `needle`. Nothing is computed until the first [`get`](Self::get).
    ///
    /// # Panics
    ///
    /// Panics if `needle` is empty, at compile time when used in a `static`.
    #[must_use = "a searcher does nothing unless it is used"]
    #[track_caller]
    pub const fn new(needle: &'static [u8]) -> Self {
        assert!(!needle.is_empty(), "an empty needle matches everywhere");
        Self { needle, finder: OnceLock::new() }
    }

    /// Returns the searcher, building it on first use.
    ///
    /// # Panics
    ///
    /// Panics only if the searcher was built for another needle, which cannot happen.
    #[inline]
    #[must_use = "an accessor has no side effect"]
    pub fn get(&self) -> &memmem::Finder<'static> {
        // The needle is checked once, where the searcher is built: checking it on every call
        // would cost a byte compare per search, in the loop the searcher was built for.
        self.finder.get_or_init(|| {
            let finder = build_finder(self.needle);
            assert!(finder.needle() == self.needle, "the searcher was built for this needle");
            finder
        })
    }
}

/// Builds a forward searcher for `needle`, with every option stated.
#[track_caller]
fn build_finder(needle: &[u8]) -> memmem::Finder<'_> {
    assert!(!needle.is_empty(), "an empty needle matches everywhere");
    // `Auto` runs a SIMD prefilter on the rarest bytes of the needle and turns itself off once it
    // stops paying, which suits short delimiters in long inputs. It is named so that a change of
    // the library default cannot change the search.
    let finder = FinderBuilder::new().prefilter(Prefilter::Auto).build_forward(needle);
    assert!(finder.needle() == needle, "the searcher keeps the needle");
    finder
}

/// Returns the classes of `byte`, as computed once at compile time.
const fn byte_classes(byte: u8) -> u8 {
    let mut classes = 0;
    if matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
        classes |= WHITESPACE;
    }
    if matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$') {
        classes |= IDENTIFIER_START | IDENTIFIER_CONTINUE;
    }
    if byte.is_ascii_digit() {
        classes |= IDENTIFIER_CONTINUE | DIGIT;
    }
    if byte.is_ascii_hexdigit() {
        classes |= HEX_DIGIT;
    }
    // Implications between classes, checked for every byte while the table is built.
    if classes & IDENTIFIER_START != 0 {
        assert!(classes & IDENTIFIER_CONTINUE != 0, "an identifier start also continues one");
    }
    if classes & DIGIT != 0 {
        assert!(classes & HEX_DIGIT != 0, "a decimal digit is a hex digit");
    }
    if classes & WHITESPACE != 0 {
        assert!(classes & IDENTIFIER_CONTINUE == 0, "whitespace is in no identifier");
    }
    classes
}

/// Builds the classification table: 16 rows of 16 byte values, flattened.
///
/// Rows of explicit entries instead of a loop: a `const fn` has no `for`, and every loop of the
/// workspace is a `for` over a range or a collection.
const fn build_table() -> [u8; 256] {
    let rows = [
        build_table_row(0x0),
        build_table_row(0x1),
        build_table_row(0x2),
        build_table_row(0x3),
        build_table_row(0x4),
        build_table_row(0x5),
        build_table_row(0x6),
        build_table_row(0x7),
        build_table_row(0x8),
        build_table_row(0x9),
        build_table_row(0xa),
        build_table_row(0xb),
        build_table_row(0xc),
        build_table_row(0xd),
        build_table_row(0xe),
        build_table_row(0xf),
    ];
    let flat = rows.as_flattened();
    assert!(flat.len() == 256, "16 rows of 16 entries cover every byte value");
    // Unreachable after the length assertion above. No message: a `const fn` cannot call the
    // formatting code behind `unreachable!("..")`, which the compiler rejects (E0015).
    let Some(table) = flat.first_chunk::<256>() else { unreachable!() };
    assert!(table[0xff] == byte_classes(u8::MAX), "the last entry is the last byte value");
    *table
}

/// Returns the classes of the 16 byte values whose high nibble is `high_nibble`.
const fn build_table_row(high_nibble: u8) -> [u8; 16] {
    assert!(high_nibble < 16, "a nibble has 16 values");
    let base = high_nibble * 16;
    [
        byte_classes(base),
        byte_classes(base + 0x1),
        byte_classes(base + 0x2),
        byte_classes(base + 0x3),
        byte_classes(base + 0x4),
        byte_classes(base + 0x5),
        byte_classes(base + 0x6),
        byte_classes(base + 0x7),
        byte_classes(base + 0x8),
        byte_classes(base + 0x9),
        byte_classes(base + 0xa),
        byte_classes(base + 0xb),
        byte_classes(base + 0xc),
        byte_classes(base + 0xd),
        byte_classes(base + 0xe),
        byte_classes(base + 0xf),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Checks every entry of the table against the standard library's ASCII predicates, by
    /// iterating over all 256 byte values.
    #[test]
    fn table_matches_reference_predicates() {
        for byte in 0..=u8::MAX {
            let whitespace = if byte.is_ascii_whitespace() { true } else { byte == 0x0b };
            assert_eq!(is_whitespace(byte), whitespace, "byte {byte}");
            let identifier_start =
                if byte.is_ascii_alphabetic() { true } else { matches!(byte, b'_' | b'$') };
            assert_eq!(is_identifier_start(byte), identifier_start, "byte {byte}");
            let identifier_continue = if identifier_start { true } else { byte.is_ascii_digit() };
            assert_eq!(is_identifier_continue(byte), identifier_continue, "byte {byte}");
            assert_eq!(is_hex_digit(byte), byte.is_ascii_hexdigit(), "byte {byte}");
            assert_eq!(classify(byte) & DIGIT != 0, byte.is_ascii_digit(), "byte {byte}");
        }
    }

    /// Checks that failed matches leave the cursor where it was, by trying prefixes that differ
    /// in their last byte or run past the end, then the whole input exactly.
    #[test]
    fn failed_matches_consume_nothing() {
        let mut cursor = Cursor::new(b"abc");
        assert!(!cursor.eat_prefix(b"abd"), "a prefix that differs in its last byte");
        assert!(!cursor.eat_prefix(b"abcd"), "a prefix one byte longer than the input");
        assert_eq!(cursor.eat_while(is_whitespace), b"", "no whitespace to eat");
        assert_eq!(cursor.offset(), 0, "nothing consumed");
        assert!(cursor.eat_prefix(b"abc"), "a prefix as long as the input");
        assert!(cursor.is_eof(), "the whole input consumed");
        assert_eq!(cursor.first(), None, "nothing left to peek");
        assert!(!cursor.eat_prefix(b"a"), "nothing matches at the end");
    }

    /// Checks the cursor on an empty input and on one byte, the smallest inputs, by calling each
    /// operation there.
    #[test]
    fn empty_and_one_byte_inputs() {
        let mut empty = Cursor::new(b"");
        assert!(empty.is_eof(), "an empty input starts at its end");
        assert_eq!(empty.bump(), None, "no byte to consume");
        assert_eq!(empty.skip_until_byte(b'x'), b"", "nothing to skip");
        assert!(!empty.skip_past(b"x"), "no needle in an empty input");
        assert_eq!(empty.offset(), 0, "an empty input has no offset to move to");

        let mut one = Cursor::new(b"x");
        assert_eq!(one.skip_until_byte(b'x'), b"", "the stop byte comes first");
        assert_eq!(one.bump(), Some(b'x'), "the only byte");
        assert_eq!((one.offset(), one.is_eof()), (1, true), "the last byte consumed");
        assert_eq!(one.bump(), None, "one past the last byte");
    }

    /// Checks `skip_past` at the boundaries of the input: a needle at the start, at the very end,
    /// cut off by the end, and longer than the rest.
    #[test]
    fn skip_past_at_the_boundaries() {
        let mut start = Cursor::new(b"*/tail");
        assert!(start.skip_past(b"*/"), "a needle at the start");
        assert_eq!(start.rest(), b"tail", "only the needle consumed");

        let mut end = Cursor::new(b"body*/");
        assert!(end.skip_past(b"*/"), "a needle at the very end");
        assert!(end.is_eof(), "the needle was the last bytes");

        let mut cut = Cursor::new(b"body*");
        assert!(!cut.skip_past(b"*/"), "a needle cut off by the end");
        assert!(cut.is_eof(), "a missing needle consumes the rest");

        let mut short = Cursor::new(b"*");
        assert!(!short.skip_past(b"*/*"), "a needle longer than the input");
        assert_eq!(short.offset(), 1, "the rest is consumed");
    }

    /// Scans a line comment, a string and an unterminated block comment, the way a lexer does,
    /// and checks every returned slice and offset.
    #[test]
    fn cursor_scans_comments_and_strings() {
        static BLOCK_END: StaticFinder = StaticFinder::new(b"*/");
        let mut cursor = Cursor::new(b"// line\n\"text\" /* unterminated");
        assert!(cursor.eat_prefix(b"//"), "a line comment opens");
        assert_eq!(cursor.skip_until_byte(b'\n'), b" line", "the comment body");
        assert_eq!(cursor.bump(), Some(b'\n'), "the line end");
        assert_eq!(cursor.first(), Some(b'"'), "a string opens");
        assert_eq!(cursor.offset(), 8, "eight bytes consumed");
        assert_eq!(cursor.eat_while(|byte| byte != b' '), b"\"text\"", "the string");
        assert_eq!(cursor.skip_until_byte(b'/'), b" ", "the space before the comment");
        assert!(!cursor.skip_past_finder(BLOCK_END.get()), "the comment never ends");
        assert!(cursor.is_eof(), "the unterminated comment runs to the end");
        assert_eq!(cursor.bump(), None, "nothing left");
    }

    /// Advances by exactly the rest, the largest valid count, which must reach the end.
    #[test]
    fn advance_by_the_whole_rest() {
        let mut cursor = Cursor::new(b"abc");
        assert_eq!(cursor.advance(3), b"abc", "the whole input returned");
        assert!(cursor.is_eof(), "the end reached");
    }

    /// Advances one byte past the end, which must crash: callers derive the count from the rest.
    #[test]
    #[should_panic(expected = "advance past the end of the input")]
    fn advance_past_the_end() {
        let mut cursor = Cursor::new(b"abc");
        let _bytes = cursor.advance(4);
    }

    /// Eats an empty prefix, which would match everywhere and must crash.
    #[test]
    #[should_panic(expected = "an empty prefix matches everywhere")]
    fn eat_prefix_rejects_an_empty_prefix() {
        let _matched = Cursor::new(b"abc").eat_prefix(b"");
    }

    /// Skips past an empty needle, which would match everywhere and must crash.
    #[test]
    #[should_panic(expected = "an empty needle matches everywhere")]
    fn skip_past_rejects_an_empty_needle() {
        let _found = Cursor::new(b"abc").skip_past(b"");
    }

    /// Declares a static searcher for an empty needle at run time, which must crash.
    #[test]
    #[should_panic(expected = "an empty needle matches everywhere")]
    fn static_finder_rejects_an_empty_needle() {
        let _finder = StaticFinder::new(b"");
    }
}
