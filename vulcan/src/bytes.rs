//! Byte-level scanning: table-driven classification and SIMD searches.
//!
//! Fast lexers work on bytes, not `char`s. They classify each byte with a single load from a
//! 256-entry table instead of a chain of range comparisons, and jump over comments with
//! `memchr`/`memmem`, which use SIMD where available. This module packages both techniques for
//! any hand-written scanner.
//!
//! ```
//! use vulcan::bytes::{Cursor, is_id_continue, is_id_start};
//!
//! let mut cursor = Cursor::new(b"owner /* skip me */ balance");
//! let ident = cursor.eat_while(is_id_continue);
//! assert_eq!(ident, b"owner");
//! cursor.eat_while(vulcan::bytes::is_whitespace);
//! assert!(cursor.eat_str(b"/*"));
//! assert!(cursor.skip_past(b"*/"));
//! cursor.eat_while(vulcan::bytes::is_whitespace);
//! assert!(cursor.first().is_some_and(is_id_start));
//! assert_eq!(cursor.eat_while(is_id_continue), b"balance");
//! assert!(cursor.is_eof());
//! ```

use crate::hint::cold_path;
use memchr::memmem;

/// Bit flag: ASCII whitespace (space, `\t`, `\n`, `\r`, form feed, vertical tab).
pub const WHITESPACE: u8 = 1 << 0;
/// Bit flag: valid first byte of an identifier (`a-z`, `A-Z`, `_`, `$`).
pub const ID_START: u8 = 1 << 1;
/// Bit flag: valid byte inside an identifier (`ID_START` plus `0-9`).
pub const ID_CONTINUE: u8 = 1 << 2;
/// Bit flag: decimal digit.
pub const DIGIT: u8 = 1 << 3;
/// Bit flag: hexadecimal digit.
pub const HEX_DIGIT: u8 = 1 << 4;

/// The classification table, built at compile time.
static TABLE: [u8; 256] = build_table();

const fn build_table() -> [u8; 256] {
    let mut table = [0_u8; 256];
    let mut i = 0;
    while i < 256 {
        let b = i as u8;
        let mut flags = 0;
        if matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
            flags |= WHITESPACE;
        }
        if b.is_ascii_alphabetic() || b == b'_' || b == b'$' {
            flags |= ID_START | ID_CONTINUE;
        }
        if b.is_ascii_digit() {
            flags |= ID_CONTINUE | DIGIT;
        }
        if b.is_ascii_hexdigit() {
            flags |= HEX_DIGIT;
        }
        table[i] = flags;
        i += 1;
    }
    table
}

/// Returns the classification flags of `b`.
#[inline(always)]
#[must_use]
pub fn classify(b: u8) -> u8 {
    TABLE[b as usize]
}

/// Returns `true` if `b` is ASCII whitespace.
#[inline(always)]
#[must_use]
pub fn is_whitespace(b: u8) -> bool {
    classify(b) & WHITESPACE != 0
}

/// Returns `true` if `b` can start an identifier.
#[inline(always)]
#[must_use]
pub fn is_id_start(b: u8) -> bool {
    classify(b) & ID_START != 0
}

/// Returns `true` if `b` can continue an identifier.
#[inline(always)]
#[must_use]
pub fn is_id_continue(b: u8) -> bool {
    classify(b) & ID_CONTINUE != 0
}

/// Returns `true` if `b` is a hexadecimal digit.
#[inline(always)]
#[must_use]
pub fn is_hex_digit(b: u8) -> bool {
    classify(b) & HEX_DIGIT != 0
}

/// A forward-only cursor over a byte slice.
///
/// Positions are computed from the length of the remaining input, so advancing is a slice
/// re-borrow and no separate counter has to be kept in sync.
#[derive(Clone, Debug)]
pub struct Cursor<'a> {
    input: &'a [u8],
    rest: &'a [u8],
}

impl<'a> Cursor<'a> {
    /// Creates a cursor at the start of `input`.
    #[inline]
    #[must_use]
    pub const fn new(input: &'a [u8]) -> Self {
        Self { input, rest: input }
    }

    /// Returns the number of bytes consumed so far.
    #[inline]
    #[must_use]
    pub const fn pos(&self) -> usize {
        self.input.len() - self.rest.len()
    }

    /// Returns the bytes not consumed yet.
    #[inline]
    #[must_use]
    pub const fn rest(&self) -> &'a [u8] {
        self.rest
    }

    /// Returns `true` once every byte has been consumed.
    #[inline]
    #[must_use]
    pub const fn is_eof(&self) -> bool {
        self.rest.is_empty()
    }

    /// Returns the next byte without consuming it.
    #[inline]
    #[must_use]
    pub const fn first(&self) -> Option<u8> {
        match self.rest {
            [b, ..] => Some(*b),
            [] => None,
        }
    }

    /// Returns the byte after the next one without consuming anything.
    #[inline]
    #[must_use]
    pub const fn second(&self) -> Option<u8> {
        match self.rest {
            [_, b, ..] => Some(*b),
            _ => None,
        }
    }

    /// Consumes and returns the next byte.
    #[inline]
    pub fn bump(&mut self) -> Option<u8> {
        let (&b, rest) = self.rest.split_first()?;
        self.rest = rest;
        Some(b)
    }

    /// Consumes bytes while `pred` holds and returns them.
    #[inline]
    pub fn eat_while(&mut self, mut pred: impl FnMut(u8) -> bool) -> &'a [u8] {
        let len = match self.rest.iter().position(|&b| !pred(b)) {
            Some(len) => len,
            None => self.rest.len(),
        };
        self.advance(len)
    }

    /// Consumes `prefix` if the input starts with it.
    #[inline]
    pub fn eat_str(&mut self, prefix: &[u8]) -> bool {
        let matched = self.rest.starts_with(prefix);
        if matched {
            self.advance(prefix.len());
        }
        matched
    }

    /// Consumes everything up to, but not including, the next `byte` (a line end, a quote).
    ///
    /// Returns the skipped bytes. Consumes the rest of the input if `byte` never occurs.
    #[inline]
    pub fn skip_until_byte(&mut self, byte: u8) -> &'a [u8] {
        let len = match memchr::memchr(byte, self.rest) {
            Some(len) => len,
            None => self.rest.len(),
        };
        self.advance(len)
    }

    /// Consumes everything up to and including the next `needle` (a block comment end).
    ///
    /// Returns `false`, consuming the rest of the input, if `needle` never occurs.
    #[inline]
    pub fn skip_past(&mut self, needle: &[u8]) -> bool {
        let found = memmem::find(self.rest, needle);
        let Some(start) = found else {
            cold_path();
            self.rest = &[];
            return false;
        };
        self.advance(start + needle.len());
        true
    }

    /// Consumes `len` bytes, clamped to the remaining input, and returns them.
    #[inline]
    fn advance(&mut self, len: usize) -> &'a [u8] {
        let (head, rest) = self.rest.split_at(len.min(self.rest.len()));
        self.rest = rest;
        head
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_matches_reference_predicates() {
        for b in 0..=u8::MAX {
            assert_eq!(is_whitespace(b), b.is_ascii_whitespace() || b == 0x0b, "byte {b}");
            assert_eq!(is_id_start(b), b.is_ascii_alphabetic() || b == b'_' || b == b'$');
            assert_eq!(is_id_continue(b), is_id_start(b) || b.is_ascii_digit());
            assert_eq!(is_hex_digit(b), b.is_ascii_hexdigit());
        }
    }

    #[test]
    fn cursor_scans_comments_and_strings() {
        let mut cursor = Cursor::new(b"// line\n\"str\" /* unterminated");
        assert!(cursor.eat_str(b"//"));
        assert_eq!(cursor.skip_until_byte(b'\n'), b" line");
        assert_eq!(cursor.bump(), Some(b'\n'));
        assert_eq!((cursor.first(), cursor.second()), (Some(b'"'), Some(b's')));
        assert_eq!(cursor.pos(), 8);
        assert_eq!(cursor.eat_while(|b| b != b' '), b"\"str\"");
        assert_eq!(cursor.skip_until_byte(b'/'), b" ");
        assert!(!cursor.skip_past(b"*/"));
        assert!(cursor.is_eof());
        assert_eq!(cursor.bump(), None);
    }
}
