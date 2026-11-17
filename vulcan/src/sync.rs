//! Opt-in, deterministic parallelism.
//!
//! Two rules shape this module:
//!
//! - **Parallelism is a runtime switch.** Every helper takes a [`Parallelism`]. With the serial
//!   mode, [`Parallelism::Serial`], the work runs inline on the caller's thread with no rayon
//!   scope, no job allocation and no synchronization, so the single-threaded path pays nothing.
//! - **Results are deterministic.** The ordered map, [`par_try_map`], returns outputs in input
//!   order and the first error in input order, whatever the thread interleaving, so diagnostics and
//!   outputs do not change from run to run.
//!
//! A third rule: **small inputs stay serial.** Waking the pool costs more than a few cheap items,
//! so every helper falls back to a plain loop below [`PAR_LEN_MIN`] items.
//!
//! Collections have a fixed capacity: a [`ShardedMap`] allocates every shard when it is created
//! and asserts on every insert that the shard stays within it.
//!
//! ```
//! use vulcan::sync::{Parallelism, par_try_map};
//!
//! // rayon's `crossbeam-epoch` trips Miri's Stacked Borrows checker, so Miri runs it serially.
//! let parallelism = Parallelism::when(!cfg!(miri));
//!
//! let parsed = par_try_map(parallelism, vec!["1", "2", "3"], |text| text.parse::<u32>());
//! assert_eq!(parsed, Ok(vec![1, 2, 3]));
//!
//! let failed =
//!     par_try_map(parallelism, vec!["1", "x", "y"], |text| text.parse::<u32>().map_err(|_| text));
//! assert_eq!(failed, Err("x"));
//! ```

use crate::convert::{u32_from_usize, usize_from_u32};
use core::{
    array,
    hash::{BuildHasher as _, Hash},
    iter, mem,
    num::{NonZeroU32, NonZeroUsize},
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};
use parking_lot::Mutex;
use rayon::prelude::*;
use rustc_hash::{FxBuildHasher, FxHashMap};
use std::{collections::hash_map::Entry, sync::OnceLock, thread};

/// Below this many items, parallel helpers run serially: waking the pool and splitting the work
/// costs more than processing a few cheap items on the current thread.
pub const PAR_LEN_MIN: u32 = 64;

/// Below this many entries in total, [`ShardedMap::merge`] runs serially.
pub const PAR_MERGE_LEN_MIN: u32 = 4096;

// The pooled path of `par_try_map` shares work between at least two items.
const _: () = assert!(PAR_LEN_MIN >= 2, "a parallel run needs two items to share");

// A merge moves whole maps; it only pays off well above the per-item threshold.
const _: () = assert!(PAR_LEN_MIN < PAR_MERGE_LEN_MIN, "merging needs more work than mapping");

/// Whether a helper may run on the rayon pool.
///
/// An enum rather than a `bool`, so a call site reads `par_map(Parallelism::Parallel, ..)` instead
/// of an unexplained `true`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Parallelism {
    /// Run inline, on the caller's thread.
    Serial,
    /// Run on the pool when the input is large enough.
    Parallel,
}

/// The size of a piece of work and the size from which it goes to the pool.
#[derive(Debug, Clone, Copy)]
struct Workload {
    /// The number of items to process.
    item_count: u32,
    /// The number of items from which the work runs on the pool.
    item_count_min: u32,
}

impl Parallelism {
    /// Returns [`Parallel`](Self::Parallel) when `condition` holds, [`Serial`](Self::Serial)
    /// otherwise.
    ///
    /// # Panics
    ///
    /// Never: the assertion states that the mode follows `condition`.
    #[inline]
    #[must_use = "the mode is computed, not applied"]
    pub const fn when(condition: bool) -> Self {
        let parallelism = if condition { Self::Parallel } else { Self::Serial };
        assert!(parallelism.is_parallel() == condition, "the mode follows the condition");
        parallelism
    }

    /// Returns `true` for [`Parallel`](Self::Parallel).
    #[inline]
    #[must_use = "the mode is queried, not changed"]
    pub const fn is_parallel(self) -> bool {
        matches!(self, Self::Parallel)
    }

    /// Returns `true` if `workload` should go to the pool.
    #[inline]
    const fn runs_parallel(self, workload: Workload) -> bool {
        assert!(workload.item_count_min > 0, "a threshold counts at least one item");
        let parallel = match self {
            Self::Serial => false,
            Self::Parallel => workload.item_count >= workload.item_count_min,
        };
        if parallel {
            assert!(self.is_parallel(), "only the parallel mode reaches the pool");
        }
        parallel
    }
}

/// Maps `map` over `items`, on the pool when parallel and `items.len() >= PAR_LEN_MIN`.
///
/// Results are in input order.
///
/// # Panics
///
/// Panics if the result count differs from the item count, which would be a bug in rayon, or if
/// there are more than `u32::MAX` items.
pub fn par_map<T, R, F>(parallelism: Parallelism, items: Vec<T>, map: F) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(T) -> R + Sync + Send,
{
    let item_count = u32_from_usize(items.len());
    let workload = Workload { item_count, item_count_min: PAR_LEN_MIN };
    let results: Vec<R> = if parallelism.runs_parallel(workload) {
        items.into_par_iter().map(map).collect()
    } else {
        items.into_iter().map(map).collect()
    };
    assert_eq!(u32_from_usize(results.len()), item_count, "one result per item");
    results
}

/// A fork-join scope that is either a rayon scope or runs jobs inline.
#[derive(Debug, Clone, Copy)]
pub struct Scope<'outer, 'scope> {
    /// The rayon scope that runs the spawned jobs, or `None` to run them inline.
    rayon: Option<&'outer rayon::Scope<'scope>>,
}

impl<'scope> Scope<'_, 'scope> {
    /// Spawns `body` into the scope, or runs it immediately when the scope is serial.
    #[inline]
    pub fn spawn<F>(self, body: F)
    where
        F: FnOnce(Scope<'_, 'scope>) + Send + 'scope,
    {
        match self.rayon {
            Some(outer) => outer.spawn(|inner| body(Scope { rayon: Some(inner) })),
            None => body(Scope { rayon: None }),
        }
    }

    /// Returns `true` if jobs run on the rayon pool.
    #[inline]
    #[must_use = "the mode is queried, not changed"]
    pub const fn is_parallel(self) -> bool {
        self.rayon.is_some()
    }
}

/// Runs `operation` with a fork-join [`Scope`]; returns once every spawned job has finished.
///
/// With [`Parallelism::Serial`], no rayon scope is created and spawned jobs run inline, in order.
#[inline]
pub fn scope<'scope, OP, R>(parallelism: Parallelism, operation: OP) -> R
where
    OP: FnOnce(Scope<'_, 'scope>) -> R + Send,
    R: Send,
{
    match parallelism {
        Parallelism::Parallel => rayon::scope(|inner| operation(Scope { rayon: Some(inner) })),
        Parallelism::Serial => operation(Scope { rayon: None }),
    }
}

/// Maps `map` over `items` and returns the results in item order, or the first error in item
/// order.
///
/// When parallel, the caller and up to `current_num_threads() - 1` rayon helpers claim items
/// from a shared counter. After the first error no new item is claimed. The caller then waits
/// for the helpers inside rayon, which joins every helper and rethrows a panic; while it waits,
/// the caller's thread may run other queued rayon jobs, as any rayon join does.
///
/// Runs serially, stopping at the first error, when serial or with fewer than [`PAR_LEN_MIN`]
/// items. Use [`par_try_map_len_min`] for few but expensive items.
///
/// # Errors
///
/// Returns the error of the first failing item, in item order.
///
/// # Panics
///
/// Panics if there are more than `u32::MAX` items.
pub fn par_try_map<T, R, E, F>(parallelism: Parallelism, items: Vec<T>, map: F) -> Result<Vec<R>, E>
where
    T: Send,
    R: Send,
    E: Send,
    F: Fn(T) -> Result<R, E> + Sync,
{
    let item_count = items.len();
    let results = par_try_map_len_min(parallelism, PAR_LEN_MIN, items, map);
    if let Ok(values) = &results {
        assert_eq!(values.len(), item_count, "a success has one value per item");
    }
    results
}

/// The ordered map, [`par_try_map`], with an explicit threshold: runs serially below `len_min`
/// items (and always below 2).
///
/// # Errors
///
/// Returns the error of the first failing item, in item order.
///
/// # Panics
///
/// Panics if a success holds fewer values than there are items, which would be a bug in this
/// module, or if there are more than `u32::MAX` items.
pub fn par_try_map_len_min<T, R, E, F>(
    parallelism: Parallelism,
    len_min: u32,
    items: Vec<T>,
    map: F,
) -> Result<Vec<R>, E>
where
    T: Send,
    R: Send,
    E: Send,
    F: Fn(T) -> Result<R, E> + Sync,
{
    let item_count = u32_from_usize(items.len());
    let workload = Workload { item_count, item_count_min: len_min.max(2) };
    let results = if parallelism.runs_parallel(workload) {
        par_try_map_len_min_pooled(items, &map)
    } else {
        items.into_iter().map(map).collect()
    };
    if let Ok(values) = &results {
        assert_eq!(u32_from_usize(values.len()), item_count, "a success has one value per item");
    }
    assert!(workload.item_count_min >= 2, "the pooled path always shares work");
    results
}

/// A value handed between threads exactly once.
type Slot<V> = Mutex<Option<V>>;

/// One result slot per item of a pooled [`par_try_map_len_min`] call.
type Outputs<R, E> = Vec<Slot<Result<R, E>>>;

/// Shared state of the workers of one pooled [`par_try_map_len_min`] call.
struct Claims<'map, T, R, E, F> {
    /// The items, each taken by the worker that claims its index.
    inputs: Vec<Slot<T>>,
    /// The results, each written once by the worker that claimed its index.
    outputs: Outputs<R, E>,
    /// The next index to claim. It passes the item count once every item is claimed.
    index_next: AtomicU32,
    /// Set once an item has failed, so that the workers stop claiming.
    failed: AtomicBool,
    /// The function applied to each item.
    map: &'map F,
}

/// The parallel path of [`par_try_map_len_min`], for at least two items.
fn par_try_map_len_min_pooled<T, R, E, F>(items: Vec<T>, map: &F) -> Result<Vec<R>, E>
where
    T: Send,
    R: Send,
    E: Send,
    F: Fn(T) -> Result<R, E> + Sync,
{
    let item_count = u32_from_usize(items.len());
    assert!(item_count >= 2, "the pooled path needs work to share");
    let worker_count = u32_from_usize(rayon::current_num_threads()).min(item_count);
    assert!(worker_count >= 1, "the caller is a worker");
    // Each worker claims one index past the end before it stops, so the counter must not wrap.
    assert!(item_count <= u32::MAX - worker_count, "the claim counter does not wrap");
    let claims = Claims {
        inputs: items.into_iter().map(|item| Mutex::new(Some(item))).collect(),
        outputs: iter::repeat_with(|| Mutex::new(None)).take(usize_from_u32(item_count)).collect(),
        index_next: AtomicU32::new(0),
        failed: AtomicBool::new(false),
        map,
    };
    assert_eq!(claims.inputs.len(), claims.outputs.len(), "one output slot per item");

    // `rayon::scope` returns only once every spawned helper has finished, and rethrows the panic
    // of a helper, so the outputs are complete when it returns.
    rayon::scope(|inner| {
        for _ in 1..worker_count {
            inner.spawn(|_| par_try_map_len_min_pooled_work(&claims));
        }
        par_try_map_len_min_pooled_work(&claims);
    });

    // Relaxed: the scope above joined every worker, which orders their writes before these reads.
    let claimed = claims.index_next.load(Ordering::Relaxed);
    if claims.failed.load(Ordering::Relaxed) {
        assert!(claimed >= 1, "an error comes from a claimed item");
    } else {
        assert!(claimed >= item_count, "without an error every item is claimed");
    }
    par_try_map_len_min_pooled_collect(claims.outputs)
}

/// Claims and runs items until none is left or an item failed.
///
/// The loop owns every state change: it claims an index, takes the item, raises the failure flag
/// and writes the output. The map itself only computes.
fn par_try_map_len_min_pooled_work<T, R, E, F>(claims: &Claims<'_, T, R, E, F>)
where
    F: Fn(T) -> Result<R, E>,
{
    let input_count = claims.inputs.len();
    assert_eq!(claims.outputs.len(), input_count, "one output per input");
    // Bounded: every iteration claims a distinct index, and there are `input_count` of them.
    for _ in 0..input_count {
        // Relaxed: a stale `false` only runs one more item, whose result follows the first error
        // in item order and is therefore never returned.
        if claims.failed.load(Ordering::Relaxed) {
            break;
        }
        // Relaxed: the counter only hands out distinct indices; the mutexes publish the results.
        let index = usize_from_u32(claims.index_next.fetch_add(1, Ordering::Relaxed));
        let (Some(input), Some(output)) = (claims.inputs.get(index), claims.outputs.get(index))
        else {
            break;
        };
        let Some(item) = input.lock().take() else {
            unreachable!("an index is claimed at most once");
        };
        let result = (claims.map)(item);
        if result.is_err() {
            // Relaxed: see the load above.
            claims.failed.store(true, Ordering::Relaxed);
        }
        let previous = output.lock().replace(result);
        assert!(previous.is_none(), "an output is written at most once");
    }
}

/// Returns the values in order, or the first error in order.
fn par_try_map_len_min_pooled_collect<R, E>(outputs: Outputs<R, E>) -> Result<Vec<R>, E> {
    let output_count = outputs.len();
    assert!(output_count >= 2, "the pooled path has at least two outputs");
    let mut values = Vec::with_capacity(output_count);
    for output in outputs {
        // Items are claimed in order and a claimed item always runs, so the unclaimed items form
        // a suffix that starts after the first error. The loop returns at that error first.
        match output.into_inner() {
            Some(Ok(value)) => values.push(value),
            Some(Err(error)) => return Err(error),
            None => unreachable!("an item is left unclaimed only after an error"),
        }
    }
    assert_eq!(values.len(), output_count, "a success has one value per output");
    Ok(values)
}

/// Number of shards in a [`ShardedMap`].
pub const SHARD_COUNT: u32 = 16;

/// The largest total capacity of a [`ShardedMap`]: rounding it up to whole shards stays below
/// `u32::MAX`.
pub const SHARD_MAP_CAPACITY_MAX: u32 = u32::MAX - (SHARD_COUNT - 1);

/// [`SHARD_COUNT`] as an array length.
const SHARD_COUNT_USIZE: usize = usize_from_u32(SHARD_COUNT);

// The shard of a key is one byte of its hash masked with `SHARD_COUNT - 1`.
const _: () = assert!(SHARD_COUNT.is_power_of_two(), "`SHARD_COUNT` must be a power of two");
const _: () = assert!(SHARD_COUNT <= 256, "one byte of the hash selects the shard");

/// A hash map split into [`SHARD_COUNT`] independent maps by key hash, with a fixed capacity.
///
/// Built for the "accumulate per thread, then merge" pattern: each rayon task fills its own
/// `ShardedMap` with no locking ([`par_fold_sharded`]), then [`merge`](Self::merge) combines
/// shard `i` of every map in one task per shard. The shards are disjoint, so the merge needs no
/// lock either, and the largest map of each shard is reused instead of rehashed.
///
/// Each shard holds `capacity / SHARD_COUNT` entries, rounded up, allocated when the map is
/// created, and an insert asserts that its shard has room: the map never reallocates. Keys must
/// spread evenly over the shards, so leave headroom in the capacity for an uneven spread.
///
/// Keys must be unique across the merged maps: which value wins on a duplicate depends on how
/// rayon split the work. The merge asserts this.
#[derive(Debug, Clone)]
pub struct ShardedMap<K, V> {
    /// The shards, boxed so that moving the map moves one pointer.
    shards: Box<Shards<K, V>>,
    /// The number of entries each shard holds at most, fixed at construction.
    shard_capacity: u32,
}

/// One shard of a [`ShardedMap`].
type Shard<K, V> = FxHashMap<K, V>;

/// Every shard of a [`ShardedMap`].
type Shards<K, V> = [Shard<K, V>; SHARD_COUNT_USIZE];

/// Shard `i` of every map being merged, at index `i`.
type Columns<K, V> = [Vec<Shard<K, V>>; SHARD_COUNT_USIZE];

impl<K, V> ShardedMap<K, V>
where
    K: Hash + Eq,
{
    /// Creates an empty map with room for `capacity` entries, rounded up to whole shards, and
    /// allocates every shard.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero or above [`SHARD_MAP_CAPACITY_MAX`].
    #[must_use = "creating a map has no other effect"]
    pub fn with_capacity(capacity: u32) -> Self {
        assert!(capacity > 0, "a sharded map needs a capacity of at least one");
        assert!(capacity <= SHARD_MAP_CAPACITY_MAX, "the capacity rounds up within `u32`");
        let shard_capacity = capacity.div_ceil(SHARD_COUNT);
        let capacity_shard = usize_from_u32(shard_capacity);
        let shards: Box<Shards<K, V>> = Box::new(array::from_fn(|_| {
            FxHashMap::with_capacity_and_hasher(capacity_shard, FxBuildHasher)
        }));
        assert!(shards.iter().all(|shard| shard.capacity() >= capacity_shard), "allocated");
        assert!(shards.iter().all(FxHashMap::is_empty), "new shards are empty");
        let map = Self { shards, shard_capacity };
        assert!(map.capacity() >= capacity, "the shards cover the requested capacity");
        assert!(map.is_empty(), "a new map is empty");
        map
    }

    /// Returns the number of entries the map holds at most: the shard capacity times the shard
    /// count, [`SHARD_COUNT`].
    ///
    /// # Panics
    ///
    /// Never: the assertions state invariants that the constructor established.
    #[must_use = "the capacity is queried, not changed"]
    pub const fn capacity(&self) -> u32 {
        assert!(self.shard_capacity > 0, "a shard holds at least one entry");
        assert!(
            self.shard_capacity <= SHARD_MAP_CAPACITY_MAX.div_ceil(SHARD_COUNT),
            "the capacity fits in `u32`"
        );
        self.shard_capacity * SHARD_COUNT
    }

    /// Returns the index of the shard that holds `key`.
    #[inline]
    fn shard_index(key: &K) -> usize {
        let hash = FxBuildHasher.hash_one(key);
        // The bits from 32 up select the shard: the shard's own table picks a bucket from the low
        // bits and a tag from the top seven, and keys that share those bits would collide in it.
        let masked = (hash >> 32_u32) & u64::from(SHARD_COUNT - 1);
        let Ok(shard_index) = u32::try_from(masked) else {
            unreachable!("the mask keeps the shard index within `u32`");
        };
        assert!(shard_index < SHARD_COUNT, "the mask keeps the shard in range");
        usize_from_u32(shard_index)
    }

    /// Inserts a value, returning the previous one for this key.
    ///
    /// # Panics
    ///
    /// Panics if `key` is new and its shard is full.
    #[inline]
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        let shard_capacity = usize_from_u32(self.shard_capacity);
        let shard = &mut self.shards[Self::shard_index(&key)];
        let allocation_before = shard.capacity();
        let entry_count_before = shard.len();
        let previous = match shard.entry(key) {
            Entry::Occupied(mut occupied) => Some(occupied.insert(value)),
            Entry::Vacant(vacant) => {
                assert!(entry_count_before < shard_capacity, "a shard of a `ShardedMap` is full");
                let _inserted = vacant.insert(value);
                None
            }
        };
        assert!(shard.len() <= shard_capacity, "a shard stays within its capacity");
        if previous.is_some() {
            assert_eq!(shard.len(), entry_count_before, "a replaced value adds no entry");
        } else {
            assert_eq!(shard.len(), entry_count_before + 1, "a new key adds one entry");
        }
        assert_eq!(shard.capacity(), allocation_before, "a sharded map never reallocates");
        previous
    }

    /// Returns the value for `key`.
    ///
    /// # Panics
    ///
    /// Never: the assertions state invariants that the constructor established.
    #[inline]
    #[must_use = "a lookup has no side effect"]
    pub fn get(&self, key: &K) -> Option<&V> {
        let shard = &self.shards[Self::shard_index(key)];
        assert!(shard.len() <= usize_from_u32(self.shard_capacity), "a shard stays in capacity");
        shard.get(key)
    }

    /// Returns the number of entries.
    ///
    /// # Panics
    ///
    /// Never: the assertion states that the map stays within its capacity.
    #[must_use = "the length is queried, not changed"]
    pub fn len(&self) -> u32 {
        let entry_count = u32_from_usize(self.shards.iter().map(FxHashMap::len).sum());
        assert!(entry_count <= self.capacity(), "the map stays within its capacity");
        entry_count
    }

    /// Returns `true` if the map is empty.
    #[must_use = "the length is queried, not changed"]
    pub fn is_empty(&self) -> bool {
        self.shards.iter().all(FxHashMap::is_empty)
    }

    /// Iterates over all entries, shard by shard, in an order that changes between runs. Sort
    /// the entries before anything order-dependent (output, hashing).
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.shards.iter().flat_map(|shard| shard.iter())
    }
}

impl<K, V> ShardedMap<K, V>
where
    K: Hash + Eq + Send,
    V: Send,
{
    /// Merges `maps` into one map with room for `capacity` entries, one task per shard when
    /// parallel and the maps hold at least [`PAR_MERGE_LEN_MIN`] entries in total.
    ///
    /// # Panics
    ///
    /// Panics if a key appears in more than one map, or if a merged shard holds more entries than
    /// the shard capacity derived from `capacity`.
    #[must_use = "the merged map is returned, not stored"]
    pub fn merge(parallelism: Parallelism, capacity: u32, maps: Vec<Self>) -> Self {
        assert!(capacity > 0, "a sharded map needs a capacity of at least one");
        assert!(capacity <= SHARD_MAP_CAPACITY_MAX, "the capacity rounds up within `u32`");
        let shard_capacity = capacity.div_ceil(SHARD_COUNT);
        let entry_count: u32 = maps.iter().map(Self::len).sum();
        let map_count = maps.len();
        // Transpose: `columns[i]` holds shard `i` of every map.
        let mut columns: Columns<K, V> = array::from_fn(|_| Vec::with_capacity(map_count));
        for map in maps {
            for (column, shard) in columns.iter_mut().zip(*map.shards) {
                column.push(shard);
            }
        }
        assert!(columns.iter().all(|column| column.len() == map_count), "a full transpose");
        let workload = Workload { item_count: entry_count, item_count_min: PAR_MERGE_LEN_MIN };
        let shards: Shards<K, V> = if parallelism.runs_parallel(workload) {
            let merged: Vec<Shard<K, V>> =
                columns.into_par_iter().map(|column| merge_shard(column, shard_capacity)).collect();
            let Ok(shards) = merged.try_into() else {
                unreachable!("one merged map per shard");
            };
            shards
        } else {
            columns.map(|column| merge_shard(column, shard_capacity))
        };
        let merged = Self { shards: Box::new(shards), shard_capacity };
        assert!(merged.capacity() >= capacity, "the merged map has the requested capacity");
        assert_eq!(merged.len(), entry_count, "duplicate key across merged maps");
        merged
    }
}

/// Merges one shard of every map into the largest of them, which keeps its allocation and grows
/// once to `shard_capacity` entries.
#[expect(clippy::iter_over_hash_type, reason = "entries go into a map, so order is irrelevant")]
fn merge_shard<K, V>(mut maps: Vec<Shard<K, V>>, shard_capacity: u32) -> Shard<K, V>
where
    K: Hash + Eq,
{
    let capacity = usize_from_u32(shard_capacity);
    let entry_count: usize = maps.iter().map(FxHashMap::len).sum();
    assert!(entry_count <= capacity, "a merged shard of a `ShardedMap` is full");
    let largest =
        maps.iter().enumerate().max_by_key(|(_, map)| map.len()).map(|(position, _)| position);
    let mut merged = largest.map_or_else(
        || FxHashMap::with_capacity_and_hasher(capacity, FxBuildHasher),
        |position| maps.swap_remove(position),
    );
    merged.reserve(capacity - merged.len());
    assert!(merged.capacity() >= capacity, "the merged shard holds the shard capacity");
    for map in maps {
        for (key, value) in map {
            let previous = merged.insert(key, value);
            assert!(previous.is_none(), "duplicate key in `ShardedMap::merge`");
        }
    }
    assert_eq!(merged.len(), entry_count, "every entry of the shard is kept");
    assert!(merged.len() <= capacity, "the merged shard stays within its capacity");
    merged
}

/// Folds `items` into a [`ShardedMap`] with room for `capacity` entries: one map per worker,
/// merged at the end.
///
/// Runs serially into a single map when serial or below [`PAR_LEN_MIN`] items. When parallel,
/// the items are split into one chunk per rayon thread, and each chunk folds into its own map of
/// `capacity` entries, so the peak memory is that of `current_num_threads() + 1` maps.
///
/// # Panics
///
/// Panics if a shard of a map is full, if a key appears in two chunks, or if there are more than
/// `u32::MAX` items.
pub fn par_fold_sharded<T, K, V, F>(
    parallelism: Parallelism,
    capacity: u32,
    items: &[T],
    fold: F,
) -> ShardedMap<K, V>
where
    T: Sync,
    K: Hash + Eq + Send,
    V: Send,
    F: Fn(&T, &mut ShardedMap<K, V>) + Sync + Send,
{
    let item_count = u32_from_usize(items.len());
    let workload = Workload { item_count, item_count_min: PAR_LEN_MIN };
    let map = if parallelism.runs_parallel(workload) {
        let thread_count = u32_from_usize(rayon::current_num_threads());
        assert!(thread_count >= 1, "rayon runs at least one thread");
        let chunk_len = usize_from_u32(item_count.div_ceil(thread_count));
        assert!(chunk_len >= 1, "a chunk holds at least one item");
        let maps: Vec<ShardedMap<K, V>> = items
            .par_chunks(chunk_len)
            .map(|chunk| par_fold_sharded_chunk(capacity, chunk, &fold))
            .collect();
        assert!(u32_from_usize(maps.len()) <= thread_count, "one map per thread at most");
        ShardedMap::merge(Parallelism::Parallel, capacity, maps)
    } else {
        par_fold_sharded_chunk(capacity, items, &fold)
    };
    assert!(map.capacity() >= capacity, "the folded map has the requested capacity");
    map
}

/// Folds one chunk of items into a new map with room for `capacity` entries.
fn par_fold_sharded_chunk<T, K, V, F>(capacity: u32, items: &[T], fold: &F) -> ShardedMap<K, V>
where
    K: Hash + Eq,
    F: Fn(&T, &mut ShardedMap<K, V>),
{
    let mut map = ShardedMap::with_capacity(capacity);
    for item in items {
        fold(item, &mut map);
    }
    assert!(map.capacity() >= capacity, "the chunk map has the requested capacity");
    assert!(map.len() <= map.capacity(), "the chunk map stays within its capacity");
    map
}

/// A function run on each new worker thread, with the worker's index.
pub type StartHandler = fn(u32);

/// The number of worker threads of a [`LazyPool`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadCount {
    /// As many workers as [`std::thread::available_parallelism`] reports, or one worker if the
    /// system cannot tell.
    Available,
    /// Exactly this many workers.
    Fixed(NonZeroU32),
}

/// The smallest stack a [`LazyPool`] worker may have: below it, ordinary code overflows.
pub const STACK_SIZE_BYTES_MIN: u32 = 64 * 1024;

/// Settings for a dedicated rayon pool, built on first use by [`LazyPool`].
///
/// There is no `Default`: every pool states its thread count, stack size and name at the call
/// site, so a change of library default can never change a program silently.
#[derive(Debug, Clone, Copy)]
pub struct PoolConfig {
    /// Worker count.
    pub thread_count: ThreadCount,
    /// Stack size of each worker. Untouched stack pages cost no memory, so a large value only
    /// reserves address space and protects deep call chains.
    pub stack_size_bytes: u32,
    /// Worker threads are named `{name_prefix}-{index}`, which shows up in profilers.
    pub name_prefix: &'static str,
    /// Called on each worker thread, with its index, before it runs any job. Use it to install
    /// thread-local context (an interner, a session) once per worker instead of passing it to
    /// every task.
    pub start_handler: Option<StartHandler>,
}

impl PoolConfig {
    /// Returns the worker count, asking the system for [`ThreadCount::Available`].
    ///
    /// # Panics
    ///
    /// Panics if the system reports more than `u32::MAX` threads.
    #[must_use = "the count is computed, not applied"]
    pub fn thread_count_resolved(&self) -> u32 {
        let thread_count = match self.thread_count {
            ThreadCount::Fixed(count) => count.get(),
            ThreadCount::Available => {
                u32_from_usize(thread::available_parallelism().map_or(1, NonZeroUsize::get))
            }
        };
        assert!(thread_count > 0, "a pool has at least one thread");
        if let ThreadCount::Fixed(count) = self.thread_count {
            assert_eq!(thread_count, count.get(), "a fixed count is used as given");
        }
        thread_count
    }
}

/// A rayon pool created on first use, or no pool at all for a single thread.
///
/// With one thread there is no pool: [`install`](Self::install) runs inline, so the program never
/// falls back to rayon's global pool. Pass [`parallelism`](Self::parallelism) to the helpers of
/// this module so they run serially too.
///
/// ```
/// use std::num::NonZeroU32;
/// use vulcan::sync::{LazyPool, PoolConfig, ThreadCount, par_map};
///
/// let pool = LazyPool::new(&PoolConfig {
///     thread_count: ThreadCount::Fixed(NonZeroU32::MIN),
///     stack_size_bytes: 16 * 1024 * 1024,
///     name_prefix: "worker",
///     start_handler: None,
/// });
/// let doubled = pool.install(|| par_map(pool.parallelism(), vec![1, 2, 3], |value| value * 2));
/// assert_eq!(doubled.ok(), Some(vec![2, 4, 6]));
/// ```
#[derive(Debug)]
pub struct LazyPool {
    /// The settings the pool is built with.
    config: PoolConfig,
    /// The worker count, resolved once when the handle is created.
    thread_count: u32,
    /// The pool, or the error that kept it from being built, once first used.
    pool: OnceLock<Result<rayon::ThreadPool, rayon::ThreadPoolBuildError>>,
}

impl LazyPool {
    /// Creates the handle and resolves the worker count. No thread is started until the first
    /// call of [`install`](Self::install).
    ///
    /// # Panics
    ///
    /// Panics on an empty `name_prefix` or a stack smaller than [`STACK_SIZE_BYTES_MIN`].
    #[must_use = "creating a pool handle has no other effect"]
    #[track_caller]
    pub fn new(config: &PoolConfig) -> Self {
        assert!(!config.name_prefix.is_empty(), "worker threads need a name prefix");
        assert!(
            config.stack_size_bytes >= STACK_SIZE_BYTES_MIN,
            "worker stacks below 64 KiB overflow"
        );
        let thread_count = config.thread_count_resolved();
        assert!(thread_count > 0, "a pool has at least one thread");
        Self { config: *config, thread_count, pool: OnceLock::new() }
    }

    /// Returns [`Parallelism::Parallel`] if work runs on more than one thread.
    #[must_use = "the mode is queried, not changed"]
    pub const fn parallelism(&self) -> Parallelism {
        Parallelism::when(self.thread_count > 1)
    }

    /// Runs `operation` inside the pool, building it on first use.
    ///
    /// # Errors
    ///
    /// Returns the build error if the pool could not be created (for example, the OS refused to
    /// spawn threads). The error is kept, so later calls fail the same way.
    ///
    /// # Panics
    ///
    /// Panics if a built pool runs another number of threads than the resolved count.
    pub fn install<R, OP>(&self, operation: OP) -> Result<R, &rayon::ThreadPoolBuildError>
    where
        R: Send,
        OP: FnOnce() -> R + Send,
    {
        match self.parallelism() {
            Parallelism::Serial => {
                assert_eq!(self.thread_count, 1, "only a single thread runs inline");
                Ok(operation())
            }
            Parallelism::Parallel => {
                assert!(self.thread_count > 1, "a pool runs more than one thread");
                let pool = self.pool.get_or_init(|| self.install_build());
                if let Ok(built) = pool {
                    let thread_count = u32_from_usize(built.current_num_threads());
                    assert_eq!(thread_count, self.thread_count, "the pool has the resolved count");
                }
                pool.as_ref().map(|built| built.install(operation))
            }
        }
    }

    /// Builds the pool, on the first parallel [`install`](Self::install).
    #[cold]
    fn install_build(&self) -> Result<rayon::ThreadPool, rayon::ThreadPoolBuildError> {
        assert!(self.thread_count > 1, "a pool is built only for parallel work");
        assert!(self.config.stack_size_bytes >= STACK_SIZE_BYTES_MIN, "checked by `new`");
        let prefix = self.config.name_prefix;
        let builder = rayon::ThreadPoolBuilder::new()
            .num_threads(usize_from_u32(self.thread_count))
            .stack_size(usize_from_u32(self.config.stack_size_bytes))
            .thread_name(move |index| format!("{prefix}-{index}"));
        match self.config.start_handler {
            Some(handler) => {
                builder.start_handler(move |index| handler(u32_from_usize(index))).build()
            }
            None => builder.build(),
        }
    }
}

/// Counts jobs waiting to run, so nested work can yield the pool to them.
///
/// An outer scheduler increments the count while a job is queued; an inner pass checks the
/// count with [`has_queued`](Self::has_queued) and stays serial when outer jobs are waiting,
/// because they need the workers more. The counter is only a scheduling hint, so relaxed atomics
/// are enough.
///
/// ```
/// use vulcan::sync::{JobCounter, Parallelism, par_map};
///
/// let jobs = JobCounter::new();
/// {
///     let _ticket = jobs.enqueue();
///     assert!(jobs.has_queued());
///     // Inner work stays serial while an outer job waits.
///     let parallelism = Parallelism::when(!jobs.has_queued());
///     let out = par_map(parallelism, (0..100).collect(), |value: u32| value + 1);
///     assert_eq!(out.len(), 100);
/// }
/// // The ticket is dropped at the end of its scope, when the job starts.
/// assert!(!jobs.has_queued());
/// ```
#[derive(Debug)]
pub struct JobCounter {
    /// The number of jobs queued and not yet started.
    queued: AtomicU32,
}

impl JobCounter {
    /// Creates a counter with no queued job.
    #[must_use = "creating a counter has no other effect"]
    pub const fn new() -> Self {
        Self { queued: AtomicU32::new(0) }
    }

    /// Marks one job as queued until the returned ticket is dropped (when the job starts).
    ///
    /// # Panics
    ///
    /// Panics if `u32::MAX` jobs are already queued.
    pub fn enqueue(&self) -> QueuedJob<'_> {
        // Relaxed: a scheduling hint, see the type documentation.
        let queued_before = self.queued.fetch_add(1, Ordering::Relaxed);
        assert!(queued_before < u32::MAX, "job counter overflow");
        assert!(self.has_queued(), "the new job counts as queued");
        QueuedJob { counter: self }
    }

    /// Returns `true` if at least one job is waiting.
    #[must_use = "the count is queried, not changed"]
    pub fn has_queued(&self) -> bool {
        // Relaxed: a scheduling hint, see the type documentation.
        self.queued.load(Ordering::Relaxed) > 0
    }
}

/// A queued job; dropping it (when the job starts) removes it from its [`JobCounter`].
#[derive(Debug)]
#[must_use = "the job counts as queued until this ticket is dropped"]
pub struct QueuedJob<'counter> {
    /// The counter that counts this job.
    counter: &'counter JobCounter,
}

impl Drop for QueuedJob<'_> {
    fn drop(&mut self) {
        // Relaxed: a scheduling hint, see `JobCounter`.
        let queued_before = self.counter.queued.fetch_sub(1, Ordering::Relaxed);
        assert!(queued_before > 0, "each ticket was counted when it was issued");
    }
}

/// Drops `value` off the current thread, so freeing a large structure (an AST, its arenas) does
/// not block the critical path. Drops inline when serial.
pub fn drop_in_background<T>(parallelism: Parallelism, value: T)
where
    T: Send + 'static,
{
    match parallelism {
        Parallelism::Parallel => rayon::spawn(move || {
            let _dropped = value;
        }),
        Parallelism::Serial => {
            let _dropped = value;
        }
    }
}

/// Skips running the destructor of `value`, leaking its memory.
///
/// Meant for the very end of a short-lived process: tearing down large graphs one node at a
/// time only to exit right after is wasted work, since the OS reclaims the memory anyway. Never
/// use it on values that own non-memory resources whose release matters (files to flush, locks).
#[expect(clippy::mem_forget, reason = "skipping teardown is the purpose of this function")]
pub const fn skip_drop<T>(value: T) {
    mem::forget(value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::{hint, sync::atomic::AtomicUsize};

    /// Modes to test. Under Miri only the serial mode runs: rayon's `crossbeam-epoch` dependency
    /// violates Stacked Borrows, which Miri reports as undefined behavior outside this crate.
    const MODES: &[Parallelism] = if cfg!(miri) {
        &[Parallelism::Serial]
    } else {
        &[Parallelism::Serial, Parallelism::Parallel]
    };

    /// Returns a pool configuration with `thread_count` workers.
    fn config(thread_count: u32, start_handler: Option<StartHandler>) -> PoolConfig {
        let Some(count) = NonZeroU32::new(thread_count) else {
            panic!("a test pool has at least one thread");
        };
        PoolConfig {
            thread_count: ThreadCount::Fixed(count),
            stack_size_bytes: 2 * 1024 * 1024,
            name_prefix: "test",
            start_handler,
        }
    }

    /// Doubles 200 items in every mode and compares with a serial map, so the pooled path keeps
    /// the item order.
    #[test]
    fn keeps_item_order() {
        let items: Vec<u32> = (0..200).collect();
        for &parallelism in MODES {
            let mapped = par_try_map(parallelism, items.clone(), |item| Ok::<_, ()>(item * 2));
            assert_eq!(mapped, Ok(items.iter().map(|item| item * 2).collect()));
        }
    }

    /// Fails on every item whose low four bits are 7, later failures finishing first, so a
    /// naive "first to fail" would return the wrong error.
    fn fail_on_sevens(item: u32) -> Result<u32, u32> {
        if item & 15 == 7 {
            for _ in 0..(64 - item) * 100 {
                hint::spin_loop();
            }
            Err(item)
        } else {
            Ok(item)
        }
    }

    /// Maps 64 items with failures that finish in reverse order and expects the first failure in
    /// item order.
    #[test]
    fn returns_first_error_in_item_order() {
        for &parallelism in MODES {
            let items: Vec<u32> = (0..64).collect();
            assert_eq!(par_try_map(parallelism, items, fail_on_sevens), Err(7));
        }
    }

    /// Fails on the first, then on the last item of three, with a threshold of two, so both ends
    /// of the pooled path report their error.
    #[test]
    fn error_on_the_first_and_last_item() {
        let fail_on =
            |failing: u8| move |item: u8| if item == failing { Err(item) } else { Ok(item) };
        for &parallelism in MODES {
            let first = par_try_map_len_min(parallelism, 2, vec![0_u8, 1, 2], fail_on(0));
            assert_eq!(first, Err(0));
            let last = par_try_map_len_min(parallelism, 2, vec![0_u8, 1, 2], fail_on(2));
            assert_eq!(last, Err(2));
        }
    }

    /// Fails at once on item 0 and succeeds slowly on every other item.
    fn fail_first_slowly(item: u32) -> Result<u32, u32> {
        if item == 0 {
            return Err(item);
        }
        for _ in 0..10_000_u32 {
            hint::spin_loop();
        }
        Ok(item)
    }

    /// Fails at once on the first of 1000 slow items and counts the calls: the serial path stops
    /// after one call, and the pooled path stops claiming long before the end.
    #[test]
    fn stops_claiming_after_an_error() {
        for &parallelism in MODES {
            let call_count = AtomicUsize::new(0);
            let items: Vec<u32> = (0..1000).collect();
            let result = par_try_map(parallelism, items, |item| {
                let _count_before = call_count.fetch_add(1, Ordering::Relaxed);
                fail_first_slowly(item)
            });
            assert_eq!(result, Err(0));
            let calls = call_count.load(Ordering::Relaxed);
            match parallelism {
                Parallelism::Serial => assert_eq!(calls, 1, "the serial path stops at once"),
                Parallelism::Parallel => assert!(calls < 500, "the workers stop claiming"),
            }
        }
    }

    /// Maps one item below, at and above the threshold, so both paths and the transition
    /// between them return every value in order.
    #[test]
    fn threshold_boundary_keeps_results() {
        for &parallelism in MODES {
            for item_count in [PAR_LEN_MIN - 1, PAR_LEN_MIN, PAR_LEN_MIN + 1] {
                let items: Vec<u32> = (0..item_count).collect();
                let mapped = par_map(parallelism, items.clone(), |value| value + 1);
                assert!(mapped.iter().copied().eq(1..=item_count), "par_map keeps the order");
                let tried = par_try_map(parallelism, items.clone(), Ok::<_, ()>);
                assert_eq!(tried, Ok(items), "par_try_map keeps the order");
            }
        }
    }

    /// Spawns eight counting jobs into a scope and returns how many ran.
    fn count_spawned_jobs(parallelism: Parallelism) -> usize {
        let counter = AtomicUsize::new(0);
        let count = || {
            let _count_before = counter.fetch_add(1, Ordering::Relaxed);
        };
        scope(parallelism, |inner| {
            assert_eq!(inner.is_parallel(), parallelism.is_parallel());
            for _ in 0..8_u8 {
                inner.spawn(|_| count());
            }
        });
        counter.load(Ordering::Relaxed)
    }

    /// Spawns eight jobs in every mode and checks that the scope returns after all of them.
    #[test]
    fn scope_runs_every_job() {
        for &parallelism in MODES {
            assert_eq!(count_spawned_jobs(parallelism), 8, "every spawned job ran");
        }
    }

    /// Maps three items, then none, in parallel mode: short and empty inputs stay correct.
    #[test]
    fn small_inputs_stay_serial_but_correct() {
        let incremented = par_map(Parallelism::Parallel, vec![1_u8, 2, 3], |value| value + 1);
        assert_eq!(incremented, [2, 3, 4]);
        let parallelism = Parallelism::when(!cfg!(miri));
        let tripled =
            par_try_map_len_min(parallelism, 2, vec![1_u8, 2, 3], |value| Ok::<_, ()>(value * 3));
        assert_eq!(tripled, Ok(vec![3, 6, 9]));
        let empty = par_try_map(parallelism, Vec::<u8>::new(), Ok::<_, ()>);
        assert_eq!(empty, Ok(Vec::new()));
    }

    /// Folds 10 000 keys in every mode, then merges two halves, and compares both with the
    /// expected entries.
    #[test]
    fn sharded_fold_and_merge_match_a_plain_map() {
        let items: Vec<u32> = (0..10_000).collect();
        // Twice the key count: the keys need not spread perfectly evenly over the shards.
        let capacity = 20_000;
        for &parallelism in MODES {
            let map = par_fold_sharded(parallelism, capacity, &items, |&value, map| {
                let _previous = map.insert(value, value * 2);
            });
            assert_eq!(map.len(), 10_000);
            assert!(items.iter().all(|value| map.get(value) == Some(&(value * 2))));
        }
        let halves: Vec<ShardedMap<u32, u32>> = [0..5_000_u32, 5_000..10_000]
            .into_iter()
            .map(|range| {
                let mut map = ShardedMap::with_capacity(capacity);
                for value in range {
                    let _previous = map.insert(value, value);
                }
                map
            })
            .collect();
        let merged = ShardedMap::merge(MODES[MODES.len() - 1], capacity, halves);
        assert_eq!(merged.len(), 10_000);
        assert_eq!(merged.iter().count(), 10_000);
        let empty = ShardedMap::<u32, u32>::merge(Parallelism::Serial, 1, Vec::new());
        assert!(empty.is_empty());
    }

    /// Returns `count` distinct keys that all fall into shard 0.
    fn keys_of_shard_zero(count: usize) -> Vec<u32> {
        (0..u32::MAX)
            .filter(|key| ShardedMap::<u32, ()>::shard_index(key) == 0)
            .take(count)
            .collect()
    }

    /// Fills shard 0 of a map with two entries per shard to its capacity, then replaces a value:
    /// a full shard still accepts a key it already holds.
    #[test]
    fn full_shard_accepts_existing_keys() {
        let mut map = ShardedMap::with_capacity(SHARD_COUNT * 2);
        assert_eq!(map.capacity(), SHARD_COUNT * 2);
        let keys = keys_of_shard_zero(2);
        assert_eq!(map.insert(keys[0], 'a'), None);
        assert_eq!(map.insert(keys[1], 'b'), None, "the shard is now at its capacity");
        assert_eq!(map.insert(keys[1], 'c'), Some('b'), "an existing key is replaced");
        assert_eq!(map.len(), 2);
        assert_eq!(map.get(&keys[1]), Some(&'c'));
    }

    /// Inserts a third distinct key into a shard of capacity two, which must crash.
    #[test]
    #[should_panic(expected = "a shard of a `ShardedMap` is full")]
    fn full_shard_rejects_new_keys() {
        let mut map = ShardedMap::with_capacity(SHARD_COUNT * 2);
        for key in keys_of_shard_zero(3) {
            let _previous = map.insert(key, ());
        }
    }

    /// Rounds capacities up to whole shards at the boundaries: one, a shard count, and one more.
    #[test]
    fn capacity_rounds_up_to_whole_shards() {
        assert_eq!(ShardedMap::<u8, u8>::with_capacity(1).capacity(), SHARD_COUNT);
        assert_eq!(ShardedMap::<u8, u8>::with_capacity(SHARD_COUNT).capacity(), SHARD_COUNT);
        let above = ShardedMap::<u8, u8>::with_capacity(SHARD_COUNT + 1);
        assert_eq!(above.capacity(), SHARD_COUNT * 2);
    }

    /// Creates a map with no capacity, which must crash.
    #[test]
    #[should_panic(expected = "capacity of at least one")]
    fn sharded_map_rejects_zero_capacity() {
        let _map = ShardedMap::<u8, u8>::with_capacity(0);
    }

    /// Creates a map one entry above the largest capacity, which must crash.
    #[test]
    #[should_panic(expected = "rounds up within `u32`")]
    fn sharded_map_rejects_capacity_above_max() {
        let _map = ShardedMap::<u8, u8>::with_capacity(SHARD_MAP_CAPACITY_MAX + 1);
    }

    /// Merges two maps that share a key, which must crash.
    #[test]
    #[should_panic(expected = "duplicate key")]
    fn merge_rejects_duplicate_keys() {
        let maps: Vec<ShardedMap<u8, u8>> = iter::repeat_with(|| {
            let mut map = ShardedMap::with_capacity(SHARD_COUNT * 2);
            let _previous = map.insert(1, 1);
            map
        })
        .take(2)
        .collect();
        let _merged = ShardedMap::merge(Parallelism::Serial, SHARD_COUNT * 2, maps);
    }

    /// Merges two maps whose shard 0 entries together exceed the merged shard capacity, which
    /// must crash.
    #[test]
    #[should_panic(expected = "a merged shard of a `ShardedMap` is full")]
    fn merge_rejects_entries_above_capacity() {
        let keys = keys_of_shard_zero(2);
        let maps: Vec<ShardedMap<u32, u8>> = keys
            .iter()
            .map(|&key| {
                let mut map = ShardedMap::with_capacity(SHARD_COUNT);
                let _previous = map.insert(key, 0);
                map
            })
            .collect();
        let _merged = ShardedMap::merge(Parallelism::Serial, SHARD_COUNT, maps);
    }

    /// Builds a two-thread pool and checks that a job runs on a worker named after the prefix.
    #[test]
    #[cfg_attr(miri, ignore = "spawns rayon threads, see `MODES`")]
    fn lazy_pool_runs_on_named_threads() {
        let pool = LazyPool::new(&config(2, None));
        assert!(pool.parallelism().is_parallel());
        let name = pool.install(|| thread::current().name().map(str::to_owned));
        assert!(name.ok().flatten().is_some_and(|worker| worker.starts_with("test-")));
    }

    /// Builds a one-thread pool and checks that the job runs on the caller's thread.
    #[test]
    fn single_thread_pool_runs_inline() {
        let pool = LazyPool::new(&config(1, None));
        assert_eq!(pool.parallelism(), Parallelism::Serial);
        let caller = thread::current().id();
        assert_eq!(pool.install(|| thread::current().id()).ok(), Some(caller));
    }

    /// Resolves the available thread count, which is at least one.
    #[test]
    fn available_thread_count_is_positive() {
        let available = PoolConfig { thread_count: ThreadCount::Available, ..config(1, None) };
        assert!(available.thread_count_resolved() >= 1);
    }

    /// Accepts a stack of exactly the minimum size.
    #[test]
    fn lazy_pool_accepts_the_minimum_stack() {
        let pool = LazyPool::new(&PoolConfig {
            stack_size_bytes: STACK_SIZE_BYTES_MIN,
            ..config(1, None)
        });
        assert_eq!(pool.parallelism(), Parallelism::Serial);
    }

    /// Asks for a stack one byte below the minimum, which must crash.
    #[test]
    #[should_panic(expected = "worker stacks below 64 KiB")]
    fn lazy_pool_rejects_tiny_stacks() {
        let _pool = LazyPool::new(&PoolConfig {
            stack_size_bytes: STACK_SIZE_BYTES_MIN - 1,
            ..config(2, None)
        });
    }

    /// Asks for workers without a name prefix, which must crash.
    #[test]
    #[should_panic(expected = "need a name prefix")]
    fn lazy_pool_rejects_empty_names() {
        let _pool = LazyPool::new(&PoolConfig { name_prefix: "", ..config(2, None) });
    }

    /// Queues two jobs and starts them, then drops values in the background and skips a drop:
    /// the counter returns to zero, and neither drop helper loses track of its value.
    #[test]
    fn job_counter_and_background_drop() {
        let jobs = JobCounter::new();
        assert!(!jobs.has_queued(), "a new counter has no job");
        {
            let _first = jobs.enqueue();
            let _second = jobs.enqueue();
            assert!(jobs.has_queued());
        }
        assert!(!jobs.has_queued());
        let parallelism = Parallelism::when(!cfg!(miri));
        drop_in_background(parallelism, vec![String::from("freed elsewhere"); 16]);
        // An empty `Vec` owns no heap memory, so skipping its drop leaks nothing that Miri or
        // LeakSanitizer would report.
        skip_drop(Vec::<u8>::new());
    }

    /// Builds a two-thread pool with a start handler and checks that it ran with worker
    /// indices below the thread count.
    #[test]
    #[cfg_attr(miri, ignore = "spawns rayon threads, see `MODES`")]
    fn start_handler_runs_on_workers() {
        static STARTED: AtomicU32 = AtomicU32::new(0);
        static INDEX_MAX: AtomicU32 = AtomicU32::new(0);
        fn on_start(index: u32) {
            let _count = STARTED.fetch_add(1, Ordering::Relaxed);
            let _index_max = INDEX_MAX.fetch_max(index, Ordering::Relaxed);
        }
        let pool = LazyPool::new(&config(2, Some(on_start)));
        assert_eq!(pool.install(|| 1_u8).ok(), Some(1));
        // Workers start asynchronously; at least the one that ran the job has started.
        assert!(STARTED.load(Ordering::Relaxed) >= 1);
        assert!(INDEX_MAX.load(Ordering::Relaxed) < 2, "worker indices stay below the count");
    }
}
