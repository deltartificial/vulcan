//! Opt-in, deterministic parallelism.
//!
//! Two rules shape this module:
//!
//! - **Parallelism is a runtime switch.** Every helper takes `enabled`. When it is `false` the work
//!   runs inline on the caller's thread with no rayon scope, no job allocation and no
//!   synchronization, so the single-threaded path pays nothing.
//! - **Results are deterministic.** [`par_try_map`] returns outputs in input order and the first
//!   error in input order, whatever the thread interleaving, so diagnostics and outputs do not
//!   change from run to run.
//!
//! A third rule: **small inputs stay serial.** Waking the pool costs more than a few cheap items,
//! so every helper falls back to a plain loop below [`PAR_MIN_LEN`] items.
//!
//! ```
//! use vulcan::sync::par_try_map;
//!
//! // rayon's `crossbeam-epoch` trips Miri's Stacked Borrows checker, so Miri runs it serially.
//! let parallel = !cfg!(miri);
//!
//! let parsed = par_try_map(parallel, vec!["1", "2", "3"], |s| s.parse::<u32>());
//! assert_eq!(parsed, Ok(vec![1, 2, 3]));
//!
//! let failed = par_try_map(parallel, vec!["1", "x", "y"], |s| s.parse::<u32>().map_err(|_| s));
//! assert_eq!(failed, Err("x"));
//! ```

use parking_lot::Mutex;
use rayon::prelude::*;
use rustc_hash::{FxBuildHasher, FxHashMap};
use std::{
    hash::{BuildHasher, Hash},
    num::NonZeroUsize,
    sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

/// Below this many items, parallel helpers run serially: waking the pool and splitting the work
/// costs more than processing a few cheap items on the current thread.
pub const PAR_MIN_LEN: usize = 64;

/// Below this many entries in total, [`ShardedMap::merge`] runs serially.
pub const PAR_MERGE_LEN: usize = 4096;

/// Maps `f` over `items`, in parallel when `enabled` and `items.len() >= PAR_MIN_LEN`.
///
/// Results are in input order.
pub fn par_map<T, R, F>(enabled: bool, items: Vec<T>, f: F) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(T) -> R + Sync + Send,
{
    if !enabled || items.len() < PAR_MIN_LEN {
        return items.into_iter().map(f).collect();
    }
    items.into_par_iter().map(f).collect()
}

/// A fork-join scope that is either a rayon scope or runs jobs inline.
#[derive(Debug, Clone, Copy)]
pub struct Scope<'r, 'scope>(Option<&'r rayon::Scope<'scope>>);

impl<'scope> Scope<'_, 'scope> {
    /// Spawns `body` into the scope, or runs it immediately when parallelism is disabled.
    #[inline]
    pub fn spawn<F>(&self, body: F)
    where
        F: FnOnce(Scope<'_, 'scope>) + Send + 'scope,
    {
        match self.0 {
            Some(outer) => outer.spawn(|inner| body(Scope(Some(inner)))),
            None => body(Scope(None)),
        }
    }

    /// Returns `true` if jobs run on the rayon pool.
    #[inline]
    #[must_use]
    pub const fn is_parallel(&self) -> bool {
        self.0.is_some()
    }
}

/// Runs `op` with a fork-join [`Scope`]; returns once every spawned job has finished.
///
/// With `enabled == false`, no rayon scope is created and spawned jobs run inline, in order.
#[inline]
pub fn scope<'scope, OP, R>(enabled: bool, op: OP) -> R
where
    OP: FnOnce(Scope<'_, 'scope>) -> R + Send,
    R: Send,
{
    if enabled { rayon::scope(|s| op(Scope(Some(s)))) } else { op(Scope(None)) }
}

/// A value handed between threads exactly once.
type Slot<V> = Mutex<Option<V>>;

/// Leaves the active-worker count on drop and, on a panic, stops the other workers, so the
/// caller's wait ends and the scope can rethrow the panic.
struct ActiveGuard<'a> {
    active: &'a AtomicUsize,
    failed: &'a AtomicBool,
}

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.failed.store(true, Ordering::Relaxed);
        }
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Maps `f` over `items` and returns the results in item order, or the first error in item order.
///
/// When `enabled`, the caller and up to `current_num_threads() - 1` rayon helpers claim items
/// from a shared counter. Unlike a parallel iterator, the caller waits for busy helpers by
/// yielding rather than inside rayon, so it never picks up an unrelated queued job before its own
/// continuation. After the first error no new item is claimed.
///
/// Runs serially, stopping at the first error, when `!enabled` or with fewer than
/// [`PAR_MIN_LEN`] items. Use [`par_try_map_min_len`] for few but expensive items.
pub fn par_try_map<T, R, E, F>(enabled: bool, items: Vec<T>, f: F) -> Result<Vec<R>, E>
where
    T: Send,
    R: Send,
    E: Send,
    F: Fn(T) -> Result<R, E> + Sync,
{
    par_try_map_min_len(enabled, PAR_MIN_LEN, items, f)
}

/// [`par_try_map`] with an explicit threshold: runs serially below `min_len` items (and always
/// below 2).
pub fn par_try_map_min_len<T, R, E, F>(
    enabled: bool,
    min_len: usize,
    items: Vec<T>,
    f: F,
) -> Result<Vec<R>, E>
where
    T: Send,
    R: Send,
    E: Send,
    F: Fn(T) -> Result<R, E> + Sync,
{
    let len = items.len();
    if !enabled || len < min_len.max(2) {
        return items.into_iter().map(f).collect();
    }

    let inputs: Vec<Slot<T>> = items.into_iter().map(|item| Mutex::new(Some(item))).collect();
    let outputs = (0..len).map(|_| Mutex::new(None)).collect::<Vec<_>>();
    let next = AtomicUsize::new(0);
    let active = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);

    let work = || {
        // The read-modify-writes on `active` order the relaxed accesses to `next` and `failed`: a
        // helper that starts after the caller's wait ended finds the counter exhausted.
        active.fetch_add(1, Ordering::AcqRel);
        let _guard = ActiveGuard { active: &active, failed: &failed };
        while !failed.load(Ordering::Relaxed) {
            let index = next.fetch_add(1, Ordering::Relaxed);
            let (Some(input), Some(output)) = (inputs.get(index), outputs.get(index)) else {
                break;
            };
            // Each index is claimed exactly once, so the item is always present.
            let Some(item) = input.lock().take() else { continue };
            let result = f(item);
            if result.is_err() {
                failed.store(true, Ordering::Relaxed);
            }
            *output.lock() = Some(result);
        }
    };

    rayon::scope(|s| {
        for _ in 1..len.min(rayon::current_num_threads()) {
            s.spawn(|_| work());
        }
        work();
        // A helper that claimed an item counted itself in `active` first.
        while active.load(Ordering::Acquire) != 0 {
            std::thread::yield_now();
        }
    });

    // Items are claimed in order, so every item before the first error has run. Items after it
    // may be missing (`None`); they are skipped, because the error is returned before them.
    let mut results = Vec::with_capacity(len);
    for output in outputs {
        match output.into_inner() {
            Some(Ok(value)) => results.push(value),
            Some(Err(err)) => return Err(err),
            None => {}
        }
    }
    Ok(results)
}

/// Number of shards in a [`ShardedMap`]. A power of two, so the shard is a mask of the hash.
pub const SHARDS: usize = 16;

/// A hash map split into [`SHARDS`] independent maps by key hash.
///
/// Built for the "accumulate per thread, then merge" pattern: each rayon task fills its own
/// `ShardedMap` with no locking ([`par_fold_sharded`]), then [`merge`](Self::merge) combines
/// shard `i` of every map in one task per shard. The shards are disjoint, so the merge needs no
/// lock either, and the largest map of each shard is reused instead of rehashed.
///
/// Keys must be unique across the merged maps: which value wins on a duplicate depends on how
/// rayon split the work. Debug builds assert this.
#[derive(Debug, Clone)]
pub struct ShardedMap<K, V> {
    shards: Box<Shards<K, V>>,
}

/// One shard of a [`ShardedMap`].
type Shard<K, V> = FxHashMap<K, V>;

/// Every shard of a [`ShardedMap`], boxed so moving the map moves one pointer.
type Shards<K, V> = [Shard<K, V>; SHARDS];

/// Shard `i` of every map being merged, at index `i`.
type Columns<K, V> = [Vec<Shard<K, V>>; SHARDS];

impl<K, V> Default for ShardedMap<K, V> {
    fn default() -> Self {
        Self { shards: Box::new(std::array::from_fn(|_| FxHashMap::default())) }
    }
}

impl<K: Hash + Eq, V> ShardedMap<K, V> {
    /// Creates an empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    fn shard_of(key: &K) -> usize {
        // Truncation is intended: only the low bits select the shard.
        (FxBuildHasher.hash_one(key) as usize) & (SHARDS - 1)
    }

    /// Inserts a value, returning the previous one for this key.
    #[inline]
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.shards[Self::shard_of(&key)].insert(key, value)
    }

    /// Returns the value for `key`.
    #[inline]
    #[must_use]
    pub fn get(&self, key: &K) -> Option<&V> {
        self.shards[Self::shard_of(key)].get(key)
    }

    /// Returns the number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.shards.iter().map(FxHashMap::len).sum()
    }

    /// Returns `true` if the map is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.shards.iter().all(FxHashMap::is_empty)
    }

    /// Iterates over all entries, shard by shard.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.shards.iter().flat_map(|shard| shard.iter())
    }
}

impl<K: Hash + Eq + Send, V: Send> ShardedMap<K, V> {
    /// Merges `maps` into one, one task per shard when `enabled` and the maps hold at least
    /// [`PAR_MERGE_LEN`] entries in total.
    #[must_use]
    pub fn merge(enabled: bool, maps: Vec<Self>) -> Self {
        let total: usize = maps.iter().map(Self::len).sum();
        // Transpose: `columns[i]` holds shard `i` of every map.
        let mut columns: Columns<K, V> = std::array::from_fn(|_| Vec::with_capacity(maps.len()));
        for map in maps {
            for (column, shard) in columns.iter_mut().zip(*map.shards) {
                column.push(shard);
            }
        }
        let merged: Vec<FxHashMap<K, V>> = if enabled && total >= PAR_MERGE_LEN {
            columns.into_par_iter().map(merge_shard).collect()
        } else {
            columns.into_iter().map(merge_shard).collect()
        };
        let mut shards = merged.into_iter();
        Self { shards: Box::new(std::array::from_fn(|_| shards.next().unwrap_or_default())) }
    }
}

/// Merges one shard of every map into the largest of them, which keeps its allocation.
fn merge_shard<K: Hash + Eq, V>(mut maps: Vec<FxHashMap<K, V>>) -> FxHashMap<K, V> {
    let Some(largest) = maps.iter().enumerate().max_by_key(|(_, map)| map.len()).map(|(i, _)| i)
    else {
        return FxHashMap::default();
    };
    let mut merged = maps.swap_remove(largest);
    merged.reserve(maps.iter().map(FxHashMap::len).sum());
    for map in maps {
        for (key, value) in map {
            let previous = merged.insert(key, value);
            debug_assert!(previous.is_none(), "duplicate key in `ShardedMap::merge`");
        }
    }
    merged
}

/// Folds `items` into a [`ShardedMap`]: one map per rayon task, merged at the end.
///
/// Runs serially into a single map when `!enabled` or below [`PAR_MIN_LEN`] items.
pub fn par_fold_sharded<T, K, V, F>(enabled: bool, items: &[T], f: F) -> ShardedMap<K, V>
where
    T: Sync,
    K: Hash + Eq + Send,
    V: Send,
    F: Fn(&T, &mut ShardedMap<K, V>) + Sync + Send,
{
    if !enabled || items.len() < PAR_MIN_LEN {
        let mut map = ShardedMap::new();
        for item in items {
            f(item, &mut map);
        }
        return map;
    }
    let maps = items
        .par_iter()
        .fold(ShardedMap::new, |mut map, item| {
            f(item, &mut map);
            map
        })
        .collect();
    ShardedMap::merge(true, maps)
}

/// A function run on each new worker thread, with the worker's index.
pub type StartHandler = fn(usize);

/// Settings for a dedicated rayon pool, built on first use by [`LazyPool`].
#[derive(Debug, Clone, Copy)]
pub struct PoolConfig {
    /// Worker count. `None` uses [`std::thread::available_parallelism`].
    pub threads: Option<NonZeroUsize>,
    /// Stack size of each worker. Untouched stack pages cost no memory, so a large value only
    /// reserves address space and protects deep recursion.
    pub stack_size: usize,
    /// Worker threads are named `{name_prefix}-{index}`, which shows up in profilers.
    pub name_prefix: &'static str,
    /// Called on each worker thread, with its index, before it runs any job. Use it to install
    /// thread-local context (an interner, a session) once per worker instead of passing it to
    /// every task.
    pub start_handler: Option<StartHandler>,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            threads: None,
            stack_size: 16 * 1024 * 1024,
            name_prefix: "vulcan",
            start_handler: None,
        }
    }
}

impl PoolConfig {
    /// Returns the effective worker count.
    #[must_use]
    pub fn thread_count(&self) -> usize {
        self.threads
            .or_else(|| std::thread::available_parallelism().ok())
            .map_or(1, NonZeroUsize::get)
    }
}

/// A rayon pool created on first use, or no pool at all for a single thread.
///
/// With one thread there is no pool: [`install`](Self::install) runs inline, so the program never
/// falls back to rayon's global pool. Pass [`is_parallel`](Self::is_parallel) as the `enabled`
/// argument of the helpers in this module so they run serially too.
///
/// ```
/// use std::num::NonZeroUsize;
/// use vulcan::sync::{LazyPool, PoolConfig, par_map};
///
/// let pool = LazyPool::new(PoolConfig { threads: NonZeroUsize::new(1), ..PoolConfig::default() });
/// let doubled = pool.install(|| par_map(pool.is_parallel(), vec![1, 2, 3], |x| x * 2));
/// assert_eq!(doubled.ok(), Some(vec![2, 4, 6]));
/// ```
#[derive(Debug)]
pub struct LazyPool {
    config: PoolConfig,
    pool: OnceLock<Result<rayon::ThreadPool, rayon::ThreadPoolBuildError>>,
}

impl LazyPool {
    /// Creates the handle. No thread is started until the first [`install`](Self::install).
    #[must_use]
    pub const fn new(config: PoolConfig) -> Self {
        Self { config, pool: OnceLock::new() }
    }

    /// Returns `true` if work runs on more than one thread.
    #[must_use]
    pub fn is_parallel(&self) -> bool {
        self.config.thread_count() > 1
    }

    /// Runs `op` inside the pool, building it on first use.
    ///
    /// # Errors
    ///
    /// Returns the build error if the pool could not be created (for example, the OS refused to
    /// spawn threads). The error is kept, so later calls fail the same way.
    pub fn install<R, OP>(&self, op: OP) -> Result<R, &rayon::ThreadPoolBuildError>
    where
        R: Send,
        OP: FnOnce() -> R + Send,
    {
        if !self.is_parallel() {
            return Ok(op());
        }
        let pool = self.pool.get_or_init(|| {
            let prefix = self.config.name_prefix;
            let mut builder = rayon::ThreadPoolBuilder::new()
                .num_threads(self.config.thread_count())
                .stack_size(self.config.stack_size)
                .thread_name(move |index| format!("{prefix}-{index}"));
            if let Some(handler) = self.config.start_handler {
                builder = builder.start_handler(handler);
            }
            builder.build()
        });
        pool.as_ref().map(|built| built.install(op))
    }
}

/// Counts jobs waiting to run, so nested work can yield the pool to them.
///
/// An outer scheduler increments the count while a job is queued; an inner pass checks
/// [`has_queued`](Self::has_queued) and stays serial when outer jobs are waiting, because they
/// need the workers more. The counter is only a scheduling hint, so relaxed atomics are enough.
///
/// ```
/// use vulcan::sync::{JobCounter, par_map};
///
/// let jobs = JobCounter::new();
/// let ticket = jobs.enqueue();
/// assert!(jobs.has_queued());
/// // Inner work stays serial while an outer job waits.
/// let out = par_map(!jobs.has_queued(), (0..100).collect(), |x: u32| x + 1);
/// assert_eq!(out.len(), 100);
/// drop(ticket);
/// assert!(!jobs.has_queued());
/// ```
#[derive(Debug, Default)]
pub struct JobCounter {
    queued: AtomicUsize,
}

impl JobCounter {
    /// Creates a counter with no queued job.
    #[must_use]
    pub const fn new() -> Self {
        Self { queued: AtomicUsize::new(0) }
    }

    /// Marks one job as queued until the returned ticket is dropped (when the job starts).
    pub fn enqueue(&self) -> QueuedJob<'_> {
        self.queued.fetch_add(1, Ordering::Relaxed);
        QueuedJob { counter: self }
    }

    /// Returns `true` if at least one job is waiting.
    #[must_use]
    pub fn has_queued(&self) -> bool {
        self.queued.load(Ordering::Relaxed) > 0
    }
}

/// A queued job; dropping it (when the job starts) removes it from its [`JobCounter`].
#[derive(Debug)]
#[must_use = "the job counts as queued until this ticket is dropped"]
pub struct QueuedJob<'a> {
    counter: &'a JobCounter,
}

impl Drop for QueuedJob<'_> {
    fn drop(&mut self) {
        self.counter.queued.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Drops `value` off the current thread, so freeing a large structure (an AST, its arenas) does
/// not block the critical path. Drops inline when `!enabled`.
pub fn drop_in_background<T: Send + 'static>(enabled: bool, value: T) {
    if enabled {
        rayon::spawn(move || {
            let _dropped = value;
        });
    } else {
        let _dropped = value;
    }
}

/// Skips running the destructor of `value`, leaking its memory.
///
/// Meant for the very end of a short-lived process: tearing down large graphs one node at a
/// time only to exit right after is wasted work, since the OS reclaims the memory anyway. Never
/// use it on values that own non-memory resources whose release matters (files to flush, locks).
#[expect(clippy::mem_forget, reason = "skipping teardown is the purpose of this function")]
pub const fn skip_drop<T>(value: T) {
    std::mem::forget(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Modes to test. Under Miri only the serial mode runs: rayon's `crossbeam-epoch` dependency
    /// violates Stacked Borrows, which Miri reports as undefined behavior outside this crate.
    const MODES: &[bool] = if cfg!(miri) { &[false] } else { &[false, true] };

    #[test]
    fn keeps_item_order() {
        let items: Vec<u32> = (0..200).collect();
        for &enabled in MODES {
            let mapped = par_try_map(enabled, items.clone(), |item| Ok::<_, ()>(item * 2));
            assert_eq!(mapped, Ok(items.iter().map(|item| item * 2).collect()));
        }
    }

    #[test]
    fn returns_first_error_in_item_order() {
        for &enabled in MODES {
            let items: Vec<u32> = (0..64).collect();
            let mapped = par_try_map(enabled, items, |item| {
                if item % 16 == 7 {
                    // Later failures finish first, so a naive "first to fail" would be wrong.
                    for _ in 0..(64 - item) * 100 {
                        std::hint::spin_loop();
                    }
                    Err(item)
                } else {
                    Ok(item)
                }
            });
            assert_eq!(mapped, Err(7));
        }
    }

    #[test]
    fn scope_runs_inline_when_disabled() {
        let counter = AtomicUsize::new(0);
        for &enabled in MODES {
            scope(enabled, |s| {
                assert_eq!(s.is_parallel(), enabled);
                for _ in 0..8 {
                    s.spawn(|_| {
                        counter.fetch_add(1, Ordering::Relaxed);
                    });
                }
            });
        }
        assert_eq!(counter.load(Ordering::Relaxed), 8 * MODES.len());
    }

    #[test]
    fn small_inputs_stay_serial_but_correct() {
        let out = par_map(true, vec![1, 2, 3], |x| x + 1);
        assert_eq!(out, [2, 3, 4]);
        let out = par_try_map_min_len(!cfg!(miri), 2, vec![1, 2, 3], |x| Ok::<_, ()>(x * 3));
        assert_eq!(out, Ok(vec![3, 6, 9]));
    }

    #[test]
    fn sharded_fold_and_merge_match_a_plain_map() {
        let items: Vec<u32> = (0..10_000).collect();
        for &enabled in MODES {
            let map = par_fold_sharded(enabled, &items, |&n, map| {
                let _previous = map.insert(n, n * 2);
            });
            assert_eq!(map.len(), items.len());
            assert!(items.iter().all(|n| map.get(n) == Some(&(n * 2))));
        }
        let halves: Vec<ShardedMap<u32, u32>> = [0..5_000_u32, 5_000..10_000]
            .into_iter()
            .map(|range| {
                let mut map = ShardedMap::new();
                for n in range {
                    let _previous = map.insert(n, n);
                }
                map
            })
            .collect();
        let merged = ShardedMap::merge(MODES.len() > 1, halves);
        assert_eq!(merged.len(), 10_000);
        assert_eq!(merged.iter().count(), 10_000);
        assert!(ShardedMap::<u32, u32>::merge(false, Vec::new()).is_empty());
    }

    #[test]
    #[cfg_attr(miri, ignore = "spawns rayon threads, see `MODES`")]
    fn lazy_pool_runs_on_named_threads() {
        let config = PoolConfig {
            threads: NonZeroUsize::new(2),
            name_prefix: "test",
            ..PoolConfig::default()
        };
        let pool = LazyPool::new(config);
        assert!(pool.is_parallel());
        let name = pool.install(|| std::thread::current().name().map(str::to_owned));
        assert!(name.ok().flatten().is_some_and(|thread| thread.starts_with("test-")));
    }

    #[test]
    fn job_counter_and_background_drop() {
        let jobs = JobCounter::new();
        {
            let _first = jobs.enqueue();
            let _second = jobs.enqueue();
            assert!(jobs.has_queued());
        }
        assert!(!jobs.has_queued());
        drop_in_background(!cfg!(miri), vec![String::from("freed elsewhere"); 16]);
        // An empty `Vec` owns no heap memory, so skipping its drop leaks nothing that Miri or
        // LeakSanitizer would report.
        skip_drop(Vec::<u8>::new());
    }

    #[test]
    #[cfg_attr(miri, ignore = "spawns rayon threads, see `MODES`")]
    fn start_handler_runs_on_workers() {
        static STARTED: AtomicUsize = AtomicUsize::new(0);
        fn on_start(_index: usize) {
            STARTED.fetch_add(1, Ordering::Relaxed);
        }
        let config = PoolConfig {
            threads: NonZeroUsize::new(2),
            start_handler: Some(on_start),
            ..PoolConfig::default()
        };
        let pool = LazyPool::new(config);
        assert_eq!(pool.install(|| 1).ok(), Some(1));
        // Workers start asynchronously; at least the one that ran the job has started.
        assert!(STARTED.load(Ordering::Relaxed) >= 1);
    }
}
