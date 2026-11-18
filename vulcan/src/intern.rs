//! Interning with a fixed capacity.
//!
//! Interning stores each distinct value once and hands out a small handle. Comparing or hashing
//! two handles is then a single integer or pointer operation, whatever the size of the value.
//! Compilers typically intern every identifier into a 4-byte `Symbol`, and their types into
//! pointer-identity handles.
//!
//! - [`Symbol`] and [`Interner`]: string interning with `u32` handles.
//! - [`Interned`]: a reference whose equality and hash use the address, for values interned in an
//!   arena.
//! - [`SliceInterner`]: interns `[Value]` lists in an arena, looking them up by borrowed slice so a
//!   hit allocates nothing.
//! - [`FlaggedInterner`]: interns values together with flags computed once at insertion, so
//!   properties like "contains an error" are a bit test instead of a walk.
//! - [`symbols!`](crate::symbols): well-known symbols as `const`s, so keyword checks are integer
//!   comparisons.
//!
//! Every interner stores its values in an arena it borrows, and gets its entry capacity at
//! construction: its table is allocated once, and inserting past the capacity is a programmer
//! error that crashes. An arena with a fixed capacity
//! ([`with_fixed_capacity`](crate::arena::BumpExtension::with_fixed_capacity)) bounds the bytes
//! too.
//!
//! ```
//! use vulcan::{
//!     arena::{Bump, BumpExtension as _},
//!     intern::Interner,
//! };
//!
//! let arena = Bump::with_fixed_capacity(1024);
//! let mut interner = Interner::with_capacity(&arena, 16);
//! let first = interner.intern("transfer");
//! let again = interner.intern("transfer");
//! assert_eq!(first, again);
//! assert_eq!(interner.resolve(first), "transfer");
//! assert_eq!(size_of::<Option<vulcan::intern::Symbol>>(), 4);
//! ```

use crate::{
    arena::{Bump, BumpExtension as _},
    collect::CollectAndApply as _,
    convert::{u32_from_usize, usize_from_u32},
};
use core::{
    borrow::Borrow,
    cmp::Ordering,
    fmt,
    hash::{Hash, Hasher},
    mem,
    ops::Deref,
    ptr,
};
use indexmap::IndexSet;
use rustc_hash::{FxBuildHasher, FxHashSet};

crate::newtype_index! {
    /// A handle to a string stored in an [`Interner`].
    ///
    /// Only meaningful together with the interner that produced it.
    pub struct Symbol;
}

// A handle is the point of interning: it must stay as small as an integer and a pointer.
crate::static_assert_size!(Option<Symbol>, 4);
crate::static_assert_size!(Interned<'static, u8>, size_of::<&u8>());
crate::static_assert_size!(Option<Interned<'static, u8>>, size_of::<&u8>());

// Interners take a `u32` capacity, so every index below it must make a valid symbol.
const _: () =
    assert!(Symbol::INDEX_MAX >= u32::MAX - 1, "every index below a capacity is a symbol");

/// A string interner that maps each distinct string to a [`Symbol`].
///
/// The strings live in a borrowed arena, and an insertion-ordered set with the `FxHash` hasher
/// (fast on short keys like identifiers) indexes them, so a symbol is the string's position in
/// the set and resolving it is an index.
#[derive(Debug, Clone)]
pub struct Interner<'arena> {
    /// The arena that holds the bytes of every string.
    arena: &'arena Bump,
    /// The interned strings, in insertion order.
    strings: IndexSet<&'arena str, FxBuildHasher>,
    /// The number of distinct strings the interner may hold.
    capacity: u32,
}

impl<'arena> Interner<'arena> {
    /// Creates an empty interner that stores strings in `arena` and allocates its table for
    /// `capacity` strings.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero: an interner that can hold nothing is a configuration error.
    #[must_use = "an interner that is never used wastes its allocation"]
    pub fn with_capacity(arena: &'arena Bump, capacity: u32) -> Self {
        assert!(capacity > 0, "an interner needs a capacity of at least one string");
        let strings = IndexSet::with_capacity_and_hasher(usize_from_u32(capacity), FxBuildHasher);
        assert!(
            strings.capacity() >= usize_from_u32(capacity),
            "the allocation covers the capacity"
        );
        assert!(strings.is_empty(), "a new interner holds no string");
        Self { arena, strings, capacity }
    }

    /// Creates an interner for `capacity` strings, pre-filled with `strings`, so that well-known
    /// symbols (keywords) get stable, predictable values in declaration order.
    ///
    /// # Panics
    ///
    /// Panics if `strings` holds a duplicate, since every later symbol would be numbered one off,
    /// or more strings than `capacity`.
    #[must_use = "an interner that is never used wastes its allocation"]
    pub fn prefilled(arena: &'arena Bump, capacity: u32, strings: &[&str]) -> Self {
        assert!(
            strings.len() <= usize_from_u32(capacity),
            "the prefilled strings fit in the capacity"
        );
        let mut interner = Self::with_capacity(arena, capacity);
        for (position, string) in strings.iter().enumerate() {
            let symbol = interner.intern(string);
            assert_eq!(
                usize_from_u32(symbol.as_u32()),
                position,
                "duplicate prefilled string `{string}`"
            );
        }
        assert_eq!(interner.strings.len(), strings.len(), "one symbol per prefilled string");
        interner
    }

    /// Returns the symbol for `string`, inserting it if needed.
    ///
    /// Copies the string into the arena only the first time it is seen.
    ///
    /// # Panics
    ///
    /// Panics if `string` is new and the interner already holds `capacity` strings, or if the
    /// arena has no room left for it.
    #[inline]
    #[track_caller]
    pub fn intern(&mut self, string: &str) -> Symbol {
        let Some(index) = self.strings.get_index_of(string) else {
            return self.intern_missing(string);
        };
        // The lookup has already compared the strings: comparing them again would double the cost
        // of a hit, the common case, and check nothing new.
        assert!(index < self.strings.len(), "a found string has a valid index");
        Symbol::from_u32(u32_from_usize(index))
    }

    /// Copies a string seen for the first time into the arena and gives it the next symbol.
    ///
    /// Kept out of line and cold: a hit is the common case, and inlining this body with its checks
    /// into every caller of [`intern`](Self::intern) made the hit path a quarter slower.
    #[cold]
    #[track_caller]
    fn intern_missing(&mut self, string: &str) -> Symbol {
        assert!(self.len() < self.capacity, "the interner is full");
        let allocated_bytes_before = self.arena.allocated_bytes();
        let fixed = self.arena.allocation_limit().is_some();
        if fixed {
            assert!(string.len() <= self.arena.chunk_capacity(), "the arena has room for it");
        }
        let stored: &'arena str = self.arena.alloc_str(string);
        assert_eq!(stored, string, "the arena copy equals the input");
        if fixed {
            assert_eq!(
                self.arena.allocated_bytes(),
                allocated_bytes_before,
                "the arena never grows"
            );
        }
        let allocation_before = self.strings.capacity();
        let (index, inserted) = self.strings.insert_full(stored);
        assert!(inserted, "a string missing from the set is inserted");
        assert_eq!(index + 1, self.strings.len(), "a new string goes last");
        assert_eq!(self.strings.capacity(), allocation_before, "the table never reallocates");
        Symbol::from_u32(u32_from_usize(index))
    }

    /// Returns the symbol for `string` if it was already interned.
    ///
    /// # Panics
    ///
    /// Panics if the table reports an index past its length, a broken invariant.
    #[inline]
    #[must_use = "a lookup has no side effect"]
    pub fn get(&self, string: &str) -> Option<Symbol> {
        let index = self.strings.get_index_of(string)?;
        assert!(index < self.strings.len(), "a found string has a valid index");
        let symbol = Symbol::from_u32(u32_from_usize(index));
        assert!(symbol.as_u32() < self.capacity, "a symbol lies within the capacity");
        Some(symbol)
    }

    /// Returns the string behind `symbol`.
    ///
    /// # Panics
    ///
    /// Panics if `symbol` was produced by another interner with more strings.
    #[inline]
    #[must_use = "resolving has no side effect"]
    #[track_caller]
    pub fn resolve(&self, symbol: Symbol) -> &'arena str {
        let index = usize_from_u32(symbol.as_u32());
        assert!(index < usize_from_u32(self.capacity), "a symbol lies within the capacity");
        assert!(index < self.strings.len(), "the symbol comes from this interner");
        let string = self.strings[index];
        assert!(string.len() <= self.arena.allocated_bytes(), "the string lives in the arena");
        string
    }

    /// Returns the number of distinct strings.
    ///
    /// # Panics
    ///
    /// Panics if the interner holds more strings than its capacity, a broken invariant.
    #[inline]
    #[must_use = "reading the length has no side effect"]
    pub fn len(&self) -> u32 {
        let string_count = u32_from_usize(self.strings.len());
        assert!(string_count <= self.capacity, "the interner stays within its capacity");
        string_count
    }

    /// Returns the number of distinct strings the interner may hold.
    ///
    /// # Panics
    ///
    /// Panics if the table lost its allocation, a broken invariant.
    #[inline]
    #[must_use = "reading the capacity has no side effect"]
    pub fn capacity(&self) -> u32 {
        assert!(self.capacity > 0, "the capacity was checked at construction");
        assert!(
            self.strings.capacity() >= usize_from_u32(self.capacity),
            "the table keeps its allocation"
        );
        self.capacity
    }

    /// Returns `true` if no string was interned.
    ///
    /// # Panics
    ///
    /// Panics if the length disagrees with the table, a broken invariant.
    #[inline]
    #[must_use = "reading the length has no side effect"]
    pub fn is_empty(&self) -> bool {
        let empty = self.strings.is_empty();
        if empty {
            assert_eq!(self.len(), 0, "an empty interner holds no string");
        } else {
            assert!(self.len() > 0, "a non-empty interner holds a string");
        }
        empty
    }
}

/// A reference to a value that is known to be unique, so equality and hashing use its address.
///
/// Values interned in an arena (types, constant pools) are stored once; two `Interned`
/// references to equal values are then the same pointer, and comparing them never reads the
/// value. Ordering still falls back to the value, since addresses carry no meaningful order.
///
/// Modeled on `rustc_data_structures::intern::Interned`.
pub struct Interned<'arena, Value: ?Sized>(&'arena Value);

impl<'arena, Value: ?Sized> Interned<'arena, Value> {
    /// Wraps a reference to an interned value.
    ///
    /// The value must be unique: no other `Interned` may point to an equal value at another
    /// address. Breaking this makes equal values compare unequal, and [`Ord`] crashes when it
    /// finds two equal values at different addresses, but memory stays safe, so the function is
    /// not `unsafe`.
    #[inline]
    #[must_use = "wrapping has no side effect"]
    pub const fn new_unchecked(value: &'arena Value) -> Self {
        Self(value)
    }

    /// Returns the inner reference with its full lifetime.
    #[inline]
    #[must_use = "reading the reference has no side effect"]
    pub const fn get(self) -> &'arena Value {
        self.0
    }
}

impl<Value: ?Sized> Clone for Interned<'_, Value> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Value: ?Sized> Copy for Interned<'_, Value> {}

impl<Value: ?Sized> Deref for Interned<'_, Value> {
    type Target = Value;

    #[inline]
    fn deref(&self) -> &Value {
        self.0
    }
}

impl<Value: ?Sized> PartialEq for Interned<'_, Value> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        ptr::eq(self.0, other.0)
    }
}

impl<Value: ?Sized> Eq for Interned<'_, Value> {}

impl<Value> PartialOrd for Interned<'_, Value>
where
    Value: ?Sized + PartialOrd,
{
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if ptr::eq(self.0, other.0) {
            Some(Ordering::Equal)
        } else {
            let ordering = self.0.partial_cmp(other.0);
            assert_ne!(ordering, Some(Ordering::Equal), "an interned value is stored once");
            ordering
        }
    }
}

impl<Value> Ord for Interned<'_, Value>
where
    Value: ?Sized + Ord,
{
    fn cmp(&self, other: &Self) -> Ordering {
        if ptr::eq(self.0, other.0) {
            Ordering::Equal
        } else {
            let ordering = self.0.cmp(other.0);
            assert_ne!(ordering, Ordering::Equal, "an interned value is stored once");
            ordering
        }
    }
}

impl<Value: ?Sized> Hash for Interned<'_, Value> {
    #[inline]
    fn hash<State>(&self, state: &mut State)
    where
        State: Hasher,
    {
        ptr::hash(self.0, state);
    }
}

impl<Value> fmt::Debug for Interned<'_, Value>
where
    Value: ?Sized + fmt::Debug,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Interns slices of `Value` in an arena.
///
/// The lookup hashes the borrowed slice, so interning a list that already exists allocates
/// nothing; only a miss copies it into the arena. Combined with the stack buffer of
/// `collect_and_apply` ([`CollectAndApply`](crate::collect::CollectAndApply)), interning from
/// an iterator never touches the heap; such an iterator yields 8 items at most.
///
/// ```
/// use vulcan::{
///     arena::{Bump, BumpExtension as _},
///     intern::SliceInterner,
/// };
///
/// let arena = Bump::with_fixed_capacity(1024);
/// let mut lists = SliceInterner::with_capacity(&arena, 16);
/// let first = lists.intern(&[1_u32, 2, 3]);
/// let again = lists.intern_iter([1_u32, 2, 3].into_iter());
/// assert_eq!(first, again, "same list, same address");
/// assert!(lists.intern(&[]).is_empty());
/// ```
#[derive(Debug)]
pub struct SliceInterner<'arena, Value> {
    /// The arena that holds every interned list.
    arena: &'arena Bump,
    /// The interned non-empty lists.
    set: FxHashSet<&'arena [Value]>,
    /// The number of distinct non-empty lists the interner may hold.
    capacity: u32,
}

impl<'arena, Value> SliceInterner<'arena, Value>
where
    Value: Copy + Hash + Eq,
{
    /// Creates an empty interner that allocates in `arena` and allocates its table for
    /// `capacity` distinct non-empty lists.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero.
    #[must_use = "an interner that is never used wastes its allocation"]
    pub fn with_capacity(arena: &'arena Bump, capacity: u32) -> Self {
        assert!(capacity > 0, "an interner needs a capacity of at least one list");
        let set = FxHashSet::with_capacity_and_hasher(usize_from_u32(capacity), FxBuildHasher);
        assert!(set.capacity() >= usize_from_u32(capacity), "the allocation covers the capacity");
        assert!(set.is_empty(), "a new interner holds nothing");
        Self { arena, set, capacity }
    }

    /// Returns the unique copy of `items`, copying it into the arena on first sight.
    ///
    /// # Panics
    ///
    /// Panics if `items` is new and the interner already holds `capacity` lists.
    #[track_caller]
    pub fn intern(&mut self, items: &[Value]) -> Interned<'arena, [Value]> {
        if items.is_empty() {
            // Every empty list is the same static slice: no allocation, and never stored.
            assert!(!self.set.contains(items), "the empty list is never stored");
            return Interned::new_unchecked(&[]);
        }
        if let Some(&existing) = self.set.get(items) {
            // The lookup has already compared the lists: comparing them again would double the
            // cost of a hit and check nothing new.
            return Interned::new_unchecked(existing);
        }
        assert!(self.len() < self.capacity, "the slice interner is full");
        let allocated_bytes_before = self.arena.allocated_bytes();
        let stored: &'arena [Value] = self.arena.allocate_exact(items.iter().copied());
        if self.arena.allocation_limit().is_some() {
            assert_eq!(
                self.arena.allocated_bytes(),
                allocated_bytes_before,
                "the arena never grows"
            );
        }
        assert!(stored == items, "the arena copy equals the input");
        let allocation_before = self.set.capacity();
        let inserted = self.set.insert(stored);
        assert!(inserted, "a slice missing from the set is inserted");
        assert_eq!(self.set.capacity(), allocation_before, "the table never reallocates");
        Interned::new_unchecked(stored)
    }

    /// Interns the items of an iterator, gathered on the stack.
    ///
    /// # Panics
    ///
    /// Panics if the iterator yields more items than `collect_and_apply` gathers
    /// ([`INLINE_ITEM_COUNT`](crate::collect::INLINE_ITEM_COUNT)), or if the list is new and the
    /// interner already holds `capacity` lists.
    #[track_caller]
    pub fn intern_iter<Items>(&mut self, items: Items) -> Interned<'arena, [Value]>
    where
        Items: Iterator<Item = Value>,
    {
        let count_before = self.len();
        let interned = items.collect_and_apply(|slice| self.intern(slice));
        assert!(self.len() >= count_before, "interning never forgets a list");
        assert!(self.len() <= count_before + 1, "interning adds at most one list");
        interned
    }

    /// Returns the number of distinct non-empty lists.
    ///
    /// # Panics
    ///
    /// Panics if the interner holds more lists than its capacity, a broken invariant.
    #[must_use = "reading the length has no side effect"]
    pub fn len(&self) -> u32 {
        let list_count = u32_from_usize(self.set.len());
        assert!(list_count <= self.capacity, "the interner stays within its capacity");
        list_count
    }

    /// Returns `true` if no non-empty list was interned.
    ///
    /// # Panics
    ///
    /// Panics if the length disagrees with the table, a broken invariant.
    #[must_use = "reading the length has no side effect"]
    pub fn is_empty(&self) -> bool {
        let empty = self.set.is_empty();
        if empty {
            assert_eq!(self.len(), 0, "an empty interner holds no list");
        } else {
            assert!(self.len() > 0, "a non-empty interner holds a list");
        }
        empty
    }
}

/// A value stored with flags computed once, when it was interned.
///
/// Equality and hashing use `value` only, so a [`FlaggedInterner`] can be probed with a plain
/// `&Value`.
#[derive(Debug, Clone, Copy)]
pub struct WithFlags<Value, Flags> {
    /// The interned value.
    pub value: Value,
    /// Properties of `value`, computed at insertion.
    pub flags: Flags,
}

impl<Value, Flags> PartialEq for WithFlags<Value, Flags>
where
    Value: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl<Value, Flags> Eq for WithFlags<Value, Flags> where Value: Eq {}

impl<Value, Flags> Hash for WithFlags<Value, Flags>
where
    Value: Hash,
{
    fn hash<State>(&self, state: &mut State)
    where
        State: Hasher,
    {
        self.value.hash(state);
    }
}

// Lets the interner's set of `&WithFlags` be searched with a `&Value`, without building a key.
impl<Value, Flags> Borrow<Value> for &WithFlags<Value, Flags> {
    fn borrow(&self) -> &Value {
        &self.value
    }
}

/// Interns values in an arena together with flags derived from them.
///
/// The flags are computed once per distinct value, typically by OR-ing the flags of its children,
/// so later queries ("does this type reference an error?") are a single bit test.
///
/// ```
/// use vulcan::{
///     arena::{Bump, BumpExtension as _},
///     intern::FlaggedInterner,
/// };
///
/// const HAS_ERROR: u8 = 1;
///
/// let arena = Bump::with_fixed_capacity(1024);
/// let mut types = FlaggedInterner::with_capacity(&arena, 16);
/// let valid = types.intern("uint256", |_| 0);
/// let invalid = types.intern("<error>", |name| if name.starts_with('<') { HAS_ERROR } else { 0 });
/// assert_eq!(valid.flags & HAS_ERROR, 0);
/// assert_ne!(invalid.flags & HAS_ERROR, 0);
/// assert_eq!(types.intern("uint256", |_| unreachable!("computed once")), valid);
/// ```
#[derive(Debug)]
pub struct FlaggedInterner<'arena, Value, Flags> {
    /// The arena that holds every interned value.
    arena: &'arena Bump,
    /// The interned values with their flags.
    set: FxHashSet<&'arena WithFlags<Value, Flags>>,
    /// The number of distinct values the interner may hold.
    capacity: u32,
}

impl<'arena, Value, Flags> FlaggedInterner<'arena, Value, Flags>
where
    Value: Hash + Eq,
{
    /// Creates an empty interner that allocates in `arena` and allocates its table for
    /// `capacity` distinct values.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero.
    #[must_use = "an interner that is never used wastes its allocation"]
    pub fn with_capacity(arena: &'arena Bump, capacity: u32) -> Self {
        assert!(capacity > 0, "an interner needs a capacity of at least one value");
        let set = FxHashSet::with_capacity_and_hasher(usize_from_u32(capacity), FxBuildHasher);
        assert!(set.capacity() >= usize_from_u32(capacity), "the allocation covers the capacity");
        assert!(set.is_empty(), "a new interner holds nothing");
        Self { arena, set, capacity }
    }

    /// Returns the unique copy of `value`, computing its flags with `compute_flags` on first
    /// sight.
    ///
    /// # Panics
    ///
    /// Panics if `value` is new and the interner already holds `capacity` values.
    #[track_caller]
    pub fn intern<Compute>(
        &mut self,
        value: Value,
        compute_flags: Compute,
    ) -> Interned<'arena, WithFlags<Value, Flags>>
    where
        Compute: FnOnce(&Value) -> Flags,
    {
        const {
            assert!(
                !mem::needs_drop::<WithFlags<Value, Flags>>(),
                "arena values must not need `Drop`"
            );
        };
        if let Some(&existing) = self.set.get(&value) {
            // The lookup has already compared the values: comparing them again would check nothing.
            return Interned::new_unchecked(existing);
        }
        assert!(self.len() < self.capacity, "the flagged interner is full");
        let flags = compute_flags(&value);
        let allocated_bytes_before = self.arena.allocated_bytes();
        let stored: &'arena WithFlags<Value, Flags> =
            &self.arena.allocate_one(WithFlags { value, flags })[0];
        if self.arena.allocation_limit().is_some() {
            assert_eq!(
                self.arena.allocated_bytes(),
                allocated_bytes_before,
                "the arena never grows"
            );
        }
        let allocation_before = self.set.capacity();
        let inserted = self.set.insert(stored);
        assert!(inserted, "a value missing from the set is inserted");
        assert_eq!(self.set.capacity(), allocation_before, "the table never reallocates");
        Interned::new_unchecked(stored)
    }

    /// Returns the number of distinct values.
    ///
    /// # Panics
    ///
    /// Panics if the interner holds more values than its capacity, a broken invariant.
    #[must_use = "reading the length has no side effect"]
    pub fn len(&self) -> u32 {
        let value_count = u32_from_usize(self.set.len());
        assert!(value_count <= self.capacity, "the interner stays within its capacity");
        value_count
    }

    /// Returns `true` if nothing was interned.
    ///
    /// # Panics
    ///
    /// Panics if the length disagrees with the table, a broken invariant.
    #[must_use = "reading the length has no side effect"]
    pub fn is_empty(&self) -> bool {
        let empty = self.set.is_empty();
        if empty {
            assert_eq!(self.len(), 0, "an empty interner holds no value");
        } else {
            assert!(self.len() > 0, "a non-empty interner holds a value");
        }
        empty
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
/// use vulcan::arena::{Bump, BumpExtension as _};
///
/// vulcan::symbols! {
///     /// Language keywords.
///     pub mod keyword {
///         CONTRACT = "contract",
///         FUNCTION = "function",
///         RETURN = "return",
///     }
/// }
///
/// let arena = Bump::with_fixed_capacity(1024);
/// let mut interner = keyword::interner(&arena, 16);
/// assert_eq!(interner.intern("function"), keyword::FUNCTION);
/// assert!(keyword::contains(interner.intern("return")));
/// assert!(!keyword::contains(interner.intern("owner")));
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
            /// The position of each symbol, in declaration order.
            #[allow(
                non_camel_case_types,
                clippy::upper_case_acronyms,
                reason = "variants mirror the const names"
            )]
            #[repr(u32)]
            enum Position { $($name),* }

            $(
                #[doc = concat!("The pre-interned symbol for `", $text, "`.")]
                #[allow(
                    clippy::as_conversions,
                    reason = "a fieldless `repr(u32)` enum converts to `u32` losslessly"
                )]
                pub const $name: $crate::intern::Symbol =
                    $crate::intern::Symbol::from_u32(Position::$name as u32);
            )*

            /// Every symbol string, in declaration order.
            pub const ALL: &[&str] = &[$($text),*];

            /// Number of symbols declared in this module.
            pub const LEN: u32 = $crate::convert::u32_from_usize(ALL.len());

            // A duplicate string makes its second arm unreachable, which fails compilation.
            #[deny(unreachable_patterns)]
            const _: fn(&str) = |text| match text { $($text => (),)* _ => () };
            const _: () = assert!(
                LEN <= $crate::intern::Symbol::INDEX_MAX,
                "too many symbols for a `u32` index"
            );

            /// Returns an interner for `capacity` strings that stores them in `arena`, pre-filled
            /// so that every `const` above resolves to its string.
            ///
            /// # Panics
            ///
            /// Panics if `capacity` is below `LEN`.
            #[must_use = "an interner that is never used wastes its allocation"]
            pub fn interner(
                arena: &$crate::arena::Bump,
                capacity: u32,
            ) -> $crate::intern::Interner<'_> {
                assert!(capacity >= LEN, "the interner holds every well-known symbol");
                let interner = $crate::intern::Interner::prefilled(arena, capacity, ALL);
                assert_eq!(interner.len(), LEN, "every well-known symbol is interned once");
                interner
            }

            /// Returns `true` if `symbol` is one of the symbols of this module.
            #[must_use = "a test has no side effect"]
            pub const fn contains(symbol: $crate::intern::Symbol) -> bool {
                let contained = symbol.as_u32() < LEN;
                if contained {
                    assert!(LEN > 0, "only a non-empty module contains a symbol");
                }
                contained
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pre-fills an interner, then interns new and known strings: known strings keep their
    /// symbol, and the last symbol resolves to its string.
    #[test]
    fn interner_deduplicates_and_resolves() {
        let arena = Bump::with_fixed_capacity(1024);
        let mut interner = Interner::prefilled(&arena, 4, &["contract", "function"]);
        assert_eq!(interner.get("contract").map(Symbol::as_u32), Some(0));
        assert_eq!(interner.get("function").map(Symbol::as_u32), Some(1));
        let owner = interner.intern("owner");
        assert_eq!(interner.intern("owner"), owner);
        assert_eq!(interner.resolve(owner), "owner");
        assert_eq!(interner.len(), 3);
        assert!(interner.get("missing").is_none());
        assert_eq!(interner.intern(""), Symbol::from_u32(3), "the empty string is a string");
        assert_eq!(interner.resolve(Symbol::from_u32(3)), "");
    }

    /// Fills an interner to its capacity: known strings still intern once it is full.
    #[test]
    fn interner_fills_up_to_its_capacity() {
        let arena = Bump::with_fixed_capacity(64);
        let mut interner = Interner::with_capacity(&arena, 2);
        assert!(interner.is_empty());
        let first = interner.intern("a");
        let second = interner.intern("b");
        assert_eq!(interner.len(), interner.capacity(), "the interner is full");
        assert_eq!(interner.intern("a"), first, "a known string interns when full");
        assert_eq!(interner.intern("b"), second);
    }

    /// Interns one new string past the capacity, which must crash instead of growing the table.
    #[test]
    #[should_panic(expected = "the interner is full")]
    fn interner_refuses_the_string_past_its_capacity() {
        let arena = Bump::with_fixed_capacity(64);
        let mut interner = Interner::with_capacity(&arena, 2);
        let _first = interner.intern("a");
        let _second = interner.intern("b");
        let _third = interner.intern("c");
    }

    /// Resolves the first symbol past the last string, which belongs to no string here.
    #[test]
    #[should_panic(expected = "the symbol comes from this interner")]
    fn interner_refuses_a_foreign_symbol() {
        let arena = Bump::with_fixed_capacity(64);
        let mut interner = Interner::with_capacity(&arena, 2);
        let _first = interner.intern("a");
        let _string = interner.resolve(Symbol::from_u32(1));
    }

    /// Interns a string larger than the arena, which must crash instead of growing the arena.
    #[test]
    #[should_panic(expected = "the arena has room for it")]
    fn interner_refuses_a_string_past_the_arena() {
        let arena = Bump::with_fixed_capacity(8);
        let mut interner = Interner::with_capacity(&arena, 2);
        let long = "x".repeat(arena.chunk_capacity() + 1);
        let _symbol = interner.intern(&long);
    }

    /// Pre-fills an interner with a repeated string, which would shift every later symbol.
    #[test]
    #[should_panic(expected = "duplicate prefilled string `contract`")]
    fn prefilled_rejects_duplicates() {
        let arena = Bump::with_fixed_capacity(64);
        let _interner = Interner::prefilled(&arena, 3, &["contract", "function", "contract"]);
    }

    /// A value whose equality must never be called.
    #[derive(Debug)]
    struct NoEq(u32);

    impl PartialEq for NoEq {
        fn eq(&self, _: &Self) -> bool {
            unreachable!("`Interned` must compare by address")
        }
    }

    /// Wraps equal values at two addresses: equality looks at the address only.
    #[test]
    fn interned_compares_by_address() {
        let (one, two) = (NoEq(1), NoEq(1));
        let first = Interned::new_unchecked(&one);
        let again = Interned::new_unchecked(&one);
        let other = Interned::new_unchecked(&two);
        assert_eq!(first, again);
        assert_ne!(first, other, "equal content at another address is a different value");
        assert_eq!(first.get().0, 1);
    }

    /// Orders two equal values at different addresses, which breaks uniqueness and must crash.
    #[test]
    #[should_panic(expected = "an interned value is stored once")]
    fn interned_ordering_rejects_duplicates() {
        let (one, two) = (1_u32, 1_u32);
        let _ordering = Interned::new_unchecked(&one).cmp(&Interned::new_unchecked(&two));
    }

    /// Interns a list twice, once from an iterator: the hit returns the same address without
    /// allocating, and the empty list is never stored.
    #[test]
    fn slice_interner_dedupes_without_reallocating() {
        let arena = Bump::with_fixed_capacity(1024);
        let mut lists = SliceInterner::with_capacity(&arena, 2);
        let first = lists.intern(&[1_u8, 2]);
        let room_bytes = arena.chunk_capacity();
        let again = lists.intern_iter([1_u8, 2].into_iter());
        assert_eq!(first, again);
        assert_eq!(arena.chunk_capacity(), room_bytes, "a hit must not allocate");
        assert_ne!(lists.intern(&[2_u8, 1]), first);
        assert_eq!(lists.len(), 2);
        assert!(lists.intern(&[]).is_empty());
        assert_eq!(lists.len(), 2, "the empty list is never stored, even when full");
    }

    /// Interns one new list past the capacity, which must crash.
    #[test]
    #[should_panic(expected = "the slice interner is full")]
    fn slice_interner_refuses_the_list_past_its_capacity() {
        let arena = Bump::with_fixed_capacity(64);
        let mut lists = SliceInterner::with_capacity(&arena, 1);
        let _first = lists.intern(&[1_u8]);
        let _second = lists.intern(&[2_u8]);
    }

    /// Interns one value three times: the flags are computed on the first call only.
    #[test]
    fn flags_are_computed_once() {
        let arena = Bump::with_fixed_capacity(64);
        let mut values = FlaggedInterner::with_capacity(&arena, 1);
        let mut calls = 0_u32;
        for _ in 0..3_u8 {
            let interned = values.intern(7_u32, |value| {
                calls += 1;
                value & 1
            });
            assert_eq!(interned.flags, 1);
        }
        assert_eq!(calls, 1);
        assert_eq!(values.len(), 1);
    }

    /// Interns one new value past the capacity, which must crash.
    #[test]
    #[should_panic(expected = "the flagged interner is full")]
    fn flagged_interner_refuses_the_value_past_its_capacity() {
        let arena = Bump::with_fixed_capacity(64);
        let mut values = FlaggedInterner::with_capacity(&arena, 1);
        let _first = values.intern(1_u32, |_| 0_u8);
        let _second = values.intern(2_u32, |_| 0_u8);
    }

    crate::symbols! {
        /// Test symbols.
        mod test_keyword {
            ALPHA = "alpha",
            BETA = "beta",
        }
    }

    /// Builds an interner from declared symbols: the constants are contiguous and resolve to
    /// their strings, and a new string is not one of them.
    #[test]
    fn symbols_are_contiguous_consts() {
        assert_eq!(test_keyword::ALPHA.as_u32(), 0);
        assert_eq!(test_keyword::BETA.as_u32(), 1);
        assert_eq!(test_keyword::LEN, 2);
        let arena = Bump::with_fixed_capacity(64);
        let mut interner = test_keyword::interner(&arena, 3);
        assert_eq!(interner.intern("beta"), test_keyword::BETA);
        assert!(!test_keyword::contains(interner.intern("gamma")));
    }

    /// Builds an interner from declared symbols with a capacity below their count.
    #[test]
    #[should_panic(expected = "the interner holds every well-known symbol")]
    fn symbols_refuse_a_capacity_below_their_count() {
        let arena = Bump::with_fixed_capacity(64);
        let _interner = test_keyword::interner(&arena, 1);
    }
}
