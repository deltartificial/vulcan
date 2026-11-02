//! Interning.
//!
//! Interning stores each distinct value once and hands out a small handle. Comparing or hashing
//! two handles is then a single integer or pointer operation, whatever the size of the value.
//! Solar interns every identifier into a 4-byte `Symbol`, and its types into pointer-identity
//! handles.
//!
//! - [`Symbol`] and [`Interner`]: string interning with `u32` handles.
//! - [`Interned`]: a reference whose equality and hash use the address, for values interned in an
//!   arena.
//!
//! ```
//! use vulcan::intern::Interner;
//!
//! let mut interner = Interner::new();
//! let a = interner.intern("transfer");
//! let b = interner.intern("transfer");
//! assert_eq!(a, b);
//! assert_eq!(interner.resolve(a), "transfer");
//! assert_eq!(size_of::<Option<vulcan::intern::Symbol>>(), 4);
//! ```

use crate::index::Idx;
use indexmap::IndexSet;
use rustc_hash::FxBuildHasher;
use std::{
    cmp::Ordering,
    fmt,
    hash::{Hash, Hasher},
    ops::Deref,
    ptr,
};

crate::newtype_index! {
    /// A handle to a string stored in an [`Interner`].
    ///
    /// Only meaningful together with the interner that produced it.
    pub struct Symbol;
}

/// A string interner that maps each distinct string to a [`Symbol`].
///
/// Backed by an insertion-ordered set with the `FxHash` hasher (fast on short keys like
/// identifiers), so a symbol is the string's position in the set and resolving it is an index.
#[derive(Debug, Default, Clone)]
pub struct Interner {
    strings: IndexSet<Box<str>, FxBuildHasher>,
}

impl Interner {
    /// Creates an empty interner.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an empty interner with room for `capacity` strings.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self { strings: IndexSet::with_capacity_and_hasher(capacity, FxBuildHasher) }
    }

    /// Creates an interner pre-filled with `strings`, so that well-known symbols (keywords) get
    /// stable, predictable values in declaration order.
    #[must_use]
    pub fn prefilled(strings: &[&str]) -> Self {
        let mut interner = Self::with_capacity(strings.len());
        for string in strings {
            let _symbol: Symbol = interner.intern(string);
        }
        interner
    }

    /// Returns the symbol for `string`, inserting it if needed.
    ///
    /// Allocates only the first time a string is seen.
    #[inline]
    pub fn intern(&mut self, string: &str) -> Symbol {
        if let Some(index) = self.strings.get_index_of(string) {
            return Symbol::from_usize(index);
        }
        let (index, _) = self.strings.insert_full(string.into());
        Symbol::from_usize(index)
    }

    /// Returns the symbol for `string` if it was already interned.
    #[inline]
    #[must_use]
    pub fn get(&self, string: &str) -> Option<Symbol> {
        self.strings.get_index_of(string).map(Symbol::from_usize)
    }

    /// Returns the string behind `symbol`, or `None` if it comes from another interner.
    #[inline]
    #[must_use]
    pub fn try_resolve(&self, symbol: Symbol) -> Option<&str> {
        self.strings.get_index(symbol.as_usize()).map(|string| &**string)
    }

    /// Returns the string behind `symbol`.
    ///
    /// # Panics
    ///
    /// Panics if `symbol` was produced by another interner with more strings.
    #[inline]
    #[must_use]
    #[track_caller]
    pub fn resolve(&self, symbol: Symbol) -> &str {
        &self.strings[symbol.as_usize()]
    }

    /// Returns the number of distinct strings.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    /// Returns `true` if no string was interned.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }
}

/// A reference to a value that is known to be unique, so equality and hashing use its address.
///
/// Values interned in an arena (types, constant pools) are stored once; two `Interned`
/// references to equal values are then the same pointer, and comparing them never reads the
/// value. Ordering still falls back to the value, since addresses carry no meaningful order.
///
/// Ported from `rustc_data_structures::intern::Interned` through Solar.
pub struct Interned<'a, T>(&'a T);

impl<'a, T> Interned<'a, T> {
    /// Wraps a reference to an interned value.
    ///
    /// The value must be unique: no other `Interned` may point to an equal value at another
    /// address. Breaking this makes equal values compare unequal, but cannot cause memory
    /// unsafety, so the function is not `unsafe`.
    #[inline]
    #[must_use]
    pub const fn new_unchecked(value: &'a T) -> Self {
        Self(value)
    }

    /// Returns the inner reference with its full lifetime.
    #[inline]
    #[must_use]
    pub const fn get(self) -> &'a T {
        self.0
    }
}

impl<T> Clone for Interned<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Interned<'_, T> {}

impl<T> Deref for Interned<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        self.0
    }
}

impl<T> PartialEq for Interned<'_, T> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        ptr::eq(self.0, other.0)
    }
}

impl<T> Eq for Interned<'_, T> {}

impl<T: PartialOrd> PartialOrd for Interned<'_, T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if ptr::eq(self.0, other.0) { Some(Ordering::Equal) } else { self.0.partial_cmp(other.0) }
    }
}

impl<T: Ord> Ord for Interned<'_, T> {
    fn cmp(&self, other: &Self) -> Ordering {
        if ptr::eq(self.0, other.0) { Ordering::Equal } else { self.0.cmp(other.0) }
    }
}

impl<T> Hash for Interned<'_, T> {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        ptr::hash(self.0, state);
    }
}

impl<T: fmt::Debug> fmt::Debug for Interned<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interner_deduplicates_and_resolves() {
        let mut interner = Interner::prefilled(&["contract", "function"]);
        assert_eq!(interner.get("contract").map(Symbol::as_u32), Some(0));
        assert_eq!(interner.get("function").map(Symbol::as_u32), Some(1));
        let owner = interner.intern("owner");
        assert_eq!(interner.intern("owner"), owner);
        assert_eq!(interner.resolve(owner), "owner");
        assert_eq!(interner.len(), 3);
        assert!(interner.get("missing").is_none());
        assert!(interner.try_resolve(Symbol::from_u32(99)).is_none());
    }

    #[derive(Debug)]
    struct NoEq(u32);

    impl PartialEq for NoEq {
        fn eq(&self, _: &Self) -> bool {
            unreachable!("`Interned` must compare by address")
        }
    }

    #[test]
    fn interned_compares_by_address() {
        let (one, two) = (NoEq(1), NoEq(1));
        let a = Interned::new_unchecked(&one);
        let b = Interned::new_unchecked(&one);
        let c = Interned::new_unchecked(&two);
        assert_eq!(a, b);
        assert_ne!(a, c, "equal content at another address is a different value");
        assert_eq!(a.get().0, 1);
    }
}
