//! Interning.
//!
//! Interning stores each distinct value once and hands out a small handle. Comparing or hashing
//! two handles is then a single integer or pointer operation, whatever the size of the value.
//! Compilers typically intern every identifier into a 4-byte `Symbol`, and their types into
//! pointer-identity handles.
//!
//! - [`Symbol`] and [`Interner`]: string interning with `u32` handles.
//! - [`Interned`]: a reference whose equality and hash use the address, for values interned in an
//!   arena.
//! - [`SliceInterner`]: interns `[T]` lists in an arena, looking them up by borrowed slice so a hit
//!   allocates nothing.
//! - [`FlaggedInterner`]: interns values together with flags computed once at insertion, so
//!   properties like "contains an error" are a bit test instead of a walk.
//! - [`symbols!`](crate::symbols): well-known symbols as `const`s, so keyword checks are integer
//!   comparisons.
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

use crate::{
    arena::{Bump, BumpExt},
    collect::CollectAndApply,
    index::Idx,
};
use indexmap::IndexSet;
use rustc_hash::{FxBuildHasher, FxHashSet};
use std::{
    borrow::Borrow,
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
/// Modeled on `rustc_data_structures::intern::Interned`.
pub struct Interned<'a, T: ?Sized>(&'a T);

impl<'a, T: ?Sized> Interned<'a, T> {
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

impl<T: ?Sized> Clone for Interned<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: ?Sized> Copy for Interned<'_, T> {}

impl<T: ?Sized> Deref for Interned<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        self.0
    }
}

impl<T: ?Sized> PartialEq for Interned<'_, T> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        ptr::eq(self.0, other.0)
    }
}

impl<T: ?Sized> Eq for Interned<'_, T> {}

impl<T: ?Sized + PartialOrd> PartialOrd for Interned<'_, T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if ptr::eq(self.0, other.0) { Some(Ordering::Equal) } else { self.0.partial_cmp(other.0) }
    }
}

impl<T: ?Sized + Ord> Ord for Interned<'_, T> {
    fn cmp(&self, other: &Self) -> Ordering {
        if ptr::eq(self.0, other.0) { Ordering::Equal } else { self.0.cmp(other.0) }
    }
}

impl<T: ?Sized> Hash for Interned<'_, T> {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        ptr::hash(self.0, state);
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for Interned<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Interns slices of `T` in an arena.
///
/// The lookup hashes the borrowed slice, so interning a list that already exists allocates
/// nothing; only a miss copies it into the arena. Combined with
/// [`collect_and_apply`](CollectAndApply::collect_and_apply), interning from an iterator of up to
/// 8 items never touches the heap.
///
/// ```
/// use vulcan::{arena::Bump, intern::SliceInterner};
///
/// let arena = Bump::new();
/// let mut lists = SliceInterner::new(&arena);
/// let a = lists.intern(&[1_u32, 2, 3]);
/// let b = lists.intern_iter([1_u32, 2, 3].into_iter());
/// assert_eq!(a, b, "same list, same address");
/// assert_eq!(&*lists.intern(&[]), &[] as &[u32]);
/// ```
#[derive(Debug)]
pub struct SliceInterner<'a, T> {
    arena: &'a Bump,
    set: FxHashSet<&'a [T]>,
}

impl<'a, T: Copy + Hash + Eq> SliceInterner<'a, T> {
    /// Creates an empty interner that allocates in `arena`.
    #[must_use]
    pub fn new(arena: &'a Bump) -> Self {
        Self { arena, set: FxHashSet::default() }
    }

    /// Returns the unique copy of `items`, copying it into the arena on first sight.
    pub fn intern(&mut self, items: &[T]) -> Interned<'a, [T]> {
        if items.is_empty() {
            // Every empty list is the same static slice: no hashing, no allocation.
            return Interned::new_unchecked(&[]);
        }
        if let Some(&existing) = self.set.get(items) {
            return Interned::new_unchecked(existing);
        }
        let stored: &'a [T] = self.arena.alloc_exact(items.iter().copied());
        self.set.insert(stored);
        Interned::new_unchecked(stored)
    }

    /// Interns the items of an iterator, gathering up to 8 of them on the stack.
    pub fn intern_iter(&mut self, items: impl Iterator<Item = T>) -> Interned<'a, [T]> {
        items.collect_and_apply(|slice| self.intern(slice))
    }

    /// Returns the number of distinct non-empty lists.
    #[must_use]
    pub fn len(&self) -> usize {
        self.set.len()
    }

    /// Returns `true` if no non-empty list was interned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }
}

/// A value stored with flags computed once, when it was interned.
///
/// Equality and hashing use `value` only, so a [`FlaggedInterner`] can be probed with a plain `&T`.
#[derive(Debug, Clone, Copy)]
pub struct WithFlags<T, F> {
    /// The interned value.
    pub value: T,
    /// Properties of `value`, computed at insertion.
    pub flags: F,
}

impl<T: PartialEq, F> PartialEq for WithFlags<T, F> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl<T: Eq, F> Eq for WithFlags<T, F> {}

impl<T: Hash, F> Hash for WithFlags<T, F> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.value.hash(state);
    }
}

// Lets the interner's set of `&WithFlags` be searched with a `&T`, without building a key.
impl<T, F> Borrow<T> for &WithFlags<T, F> {
    fn borrow(&self) -> &T {
        &self.value
    }
}

/// Interns values in an arena together with flags derived from them.
///
/// The flags are computed once per distinct value, typically by OR-ing the flags of its children,
/// so later queries ("does this type reference an error?") are a single bit test.
///
/// ```
/// use vulcan::{arena::Bump, intern::FlaggedInterner};
///
/// const HAS_ERROR: u8 = 1;
///
/// let arena = Bump::new();
/// let mut types = FlaggedInterner::new(&arena);
/// let ok = types.intern("uint256", |_| 0);
/// let bad = types.intern("<error>", |name| if name.starts_with('<') { HAS_ERROR } else { 0 });
/// assert_eq!(ok.flags & HAS_ERROR, 0);
/// assert_ne!(bad.flags & HAS_ERROR, 0);
/// assert_eq!(types.intern("uint256", |_| unreachable!("computed once")), ok);
/// ```
#[derive(Debug)]
pub struct FlaggedInterner<'a, T, F> {
    arena: &'a Bump,
    set: FxHashSet<&'a WithFlags<T, F>>,
}

impl<'a, T: Hash + Eq, F> FlaggedInterner<'a, T, F> {
    /// Creates an empty interner that allocates in `arena`.
    #[must_use]
    pub fn new(arena: &'a Bump) -> Self {
        Self { arena, set: FxHashSet::default() }
    }

    /// Returns the unique copy of `value`, computing its flags with `compute_flags` on first
    /// sight.
    pub fn intern(
        &mut self,
        value: T,
        compute_flags: impl FnOnce(&T) -> F,
    ) -> Interned<'a, WithFlags<T, F>> {
        const {
            assert!(!std::mem::needs_drop::<WithFlags<T, F>>(), "arena values must not need `Drop`")
        };
        if let Some(&existing) = self.set.get(&value) {
            return Interned::new_unchecked(existing);
        }
        let flags = compute_flags(&value);
        let stored: &'a WithFlags<T, F> = self.arena.alloc(WithFlags { value, flags });
        self.set.insert(stored);
        Interned::new_unchecked(stored)
    }

    /// Returns the number of distinct values.
    #[must_use]
    pub fn len(&self) -> usize {
        self.set.len()
    }

    /// Returns `true` if nothing was interned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }
}

/// Declares well-known symbols as `const`s, in a module.
///
/// Each name becomes a `const` [`Symbol`] numbered in declaration order, and the module exposes
/// `ALL` (the strings, to pre-fill an [`Interner`] with [`interner`]) and `LEN`. Because the
/// numbering is contiguous, "is this one of our keywords" is `symbol.as_u32() < LEN`, and a
/// sub-range is two comparisons. Duplicate strings fail compilation.
///
/// ```
/// vulcan::symbols! {
///     /// Language keywords.
///     pub mod kw {
///         CONTRACT = "contract",
///         FUNCTION = "function",
///         RETURN = "return",
///     }
/// }
///
/// let mut interner = kw::interner();
/// assert_eq!(interner.intern("function"), kw::FUNCTION);
/// assert!(kw::contains(interner.intern("return")));
/// assert!(!kw::contains(interner.intern("owner")));
/// ```
///
/// [`interner`]: Interner::prefilled
#[macro_export]
macro_rules! symbols {
    ($(#[$attr:meta])* $vis:vis mod $module:ident { $($name:ident = $text:literal),* $(,)? }) => {
        $(#[$attr])*
        // The items are `pub` so the module's own visibility decides who sees them.
        #[allow(unreachable_pub, reason = "visibility is controlled by the module")]
        $vis mod $module {
            #[allow(non_camel_case_types, clippy::upper_case_acronyms, reason = "variants mirror the const names")]
            #[repr(u32)]
            enum Position { $($name),* }

            $(
                #[doc = concat!("The pre-interned symbol for `", $text, "`.")]
                pub const $name: $crate::intern::Symbol =
                    $crate::intern::Symbol::from_u32(Position::$name as u32);
            )*

            /// Every symbol string, in declaration order.
            pub const ALL: &[&str] = &[$($text),*];

            /// Number of symbols declared in this module.
            #[allow(clippy::cast_possible_truncation, reason = "a symbol list never exceeds `u32`")]
            pub const LEN: u32 = ALL.len() as u32;

            const _: () = assert!(!$crate::intern::has_duplicates(ALL), "duplicate symbol string");

            /// Returns an interner pre-filled so that every `const` above resolves to its string.
            #[must_use]
            pub fn interner() -> $crate::intern::Interner {
                $crate::intern::Interner::prefilled(ALL)
            }

            /// Returns `true` if `symbol` is one of the symbols of this module.
            #[must_use]
            pub const fn contains(symbol: $crate::intern::Symbol) -> bool {
                symbol.as_u32() < LEN
            }
        }
    };
}

/// Returns `true` if two strings of `strings` are equal. `const`, for [`symbols!`](crate::symbols).
#[doc(hidden)]
#[must_use]
pub const fn has_duplicates(strings: &[&str]) -> bool {
    let mut i = 0;
    while i < strings.len() {
        let mut j = i + 1;
        while j < strings.len() {
            if bytes_eq(strings[i], strings[j]) {
                return true;
            }
            j += 1;
        }
        i += 1;
    }
    false
}

const fn bytes_eq(left: &str, right: &str) -> bool {
    let (a, b) = (left.as_bytes(), right.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
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

    #[test]
    fn slice_interner_dedupes_without_reallocating() {
        let arena = Bump::new();
        let mut lists = SliceInterner::new(&arena);
        let a = lists.intern(&[1_u8, 2]);
        let used = arena.allocated_bytes();
        let b = lists.intern_iter([1_u8, 2].into_iter());
        assert_eq!(a, b);
        assert_eq!(arena.allocated_bytes(), used, "a hit must not allocate");
        assert_ne!(lists.intern(&[2_u8, 1]), a);
        assert_eq!(lists.len(), 2);
    }

    #[test]
    fn flags_are_computed_once() {
        let arena = Bump::new();
        let mut values = FlaggedInterner::new(&arena);
        let mut calls = 0;
        for _ in 0..3 {
            let interned = values.intern(7_u32, |v| {
                calls += 1;
                v % 2
            });
            assert_eq!(interned.flags, 1);
        }
        assert_eq!(calls, 1);
        assert_eq!(values.len(), 1);
    }

    crate::symbols! {
        /// Test symbols.
        mod test_kw {
            ALPHA = "alpha",
            BETA = "beta",
        }
    }

    #[test]
    fn symbols_are_contiguous_consts() {
        assert_eq!((test_kw::ALPHA.as_u32(), test_kw::BETA.as_u32(), test_kw::LEN), (0, 1, 2));
        let mut interner = test_kw::interner();
        assert_eq!(interner.intern("beta"), test_kw::BETA);
        assert!(!test_kw::contains(interner.intern("gamma")));
        assert!(has_duplicates(&["a", "b", "a"]));
        assert!(!has_duplicates(&["a", "ab"]));
    }
}
