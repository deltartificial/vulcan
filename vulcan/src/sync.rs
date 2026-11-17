//! Deterministic parallelism on a pool of threads created at startup.
//!
//! Three rules shape this module:
//!
//! - **Nothing is allocated after the pool is created.** A [`WorkerPool`] spawns its threads and
//!   allocates one input slot and one output slot per item of its largest batch when it is created.
//!   A call of [`WorkerPool::map`] then moves the items through those slots and writes the results
//!   into a buffer of the caller, so it runs under a sealed allocator
//!   ([`crate::allocator::StaticAllocator::seal`]). That is why the work is a plain function over a
//!   shared context, fixed when the pool is created, and not a closure passed to each call: a
//!   closure that borrows from the caller cannot reach a thread that outlives the call.
//! - **Results are deterministic.** The outputs come back in input order whatever the thread
//!   interleaving, so diagnostics and outputs do not change from run to run.
//! - **Small inputs stay on the caller's thread.** Waking the workers costs more than a few cheap
//!   items, so a batch below `parallel_item_count_min` items runs inline, and so does every batch
//!   of a pool with one thread, which has no worker at all.
//!
//! Collections have a fixed capacity: a [`ShardedMap`] allocates every shard when it is created
//! and asserts on every insert that the shard stays within it.
//!
//! ```
//! use std::num::NonZeroU32;
//! use vulcan::sync::{PARALLEL_LEN_MIN, PoolConfig, ThreadCount, Work, WorkerPool};
//!
//! /// Multiplies an item by the factor that the pool shares between its threads.
//! fn scale(factor: &u32, item: u32) -> u32 {
//!     factor * item
//! }
//!
//! // Initialization: the threads, the slots and both buffers are allocated here.
//! let config = PoolConfig {
//!     thread_count: ThreadCount::Fixed(NonZeroU32::MIN.saturating_add(1)),
//!     stack_size_bytes: 2 * 1024 * 1024,
//!     name_prefix: "worker",
//!     batch_item_count_max: 256,
//!     parallel_item_count_min: PARALLEL_LEN_MIN,
//! };
//! let work = Work { context: 3, map: scale, start_handler: None };
//! let Ok(mut pool) = WorkerPool::new(&config, work) else { return };
//! let mut items = Vec::with_capacity(256);
//! let mut outputs = Vec::with_capacity(256);
//!
//! // Steady state: no allocation, the buffers are reused from batch to batch.
//! items.extend(0..200_u32);
//! pool.map(&mut items, &mut outputs);
//! assert!(items.is_empty(), "the items moved into the pool");
//! assert!(outputs.iter().copied().eq((0..200).map(|item| item * 3)), "in input order");
//! outputs.clear();
//! ```

use crate::convert::{u32_from_usize, usize_from_u32};
use alloc::sync::Arc;
use core::{
    array, fmt,
    hash::{BuildHasher as _, Hash},
    iter, mem,
    num::{NonZeroU32, NonZeroUsize},
    ops::Range,
    time::Duration,
};
use parking_lot::{Condvar, Mutex};
use rustc_hash::{FxBuildHasher, FxHashMap};
use std::{collections::hash_map::Entry, io, thread};

/// A suggested value for [`PoolConfig::parallel_item_count_min`]: below this many cheap items,
/// waking the workers and sharing the work costs more than mapping them on the current thread.
pub const PARALLEL_LEN_MIN: u32 = 64;

/// The number of chunks a batch is cut into for each thread: several, so that a thread that
/// finishes early takes over work from a slower one, yet few enough that claiming a chunk, which
/// takes a lock, stays rare.
const CHUNK_COUNT_PER_THREAD: u32 = 4;

// A pooled batch shares work between at least two items.
const _: () = assert!(PARALLEL_LEN_MIN >= 2, "a parallel run needs two items to share");
const _: () = assert!(CHUNK_COUNT_PER_THREAD >= 1, "every thread gets at least one chunk");

/// The number of threads of a [`WorkerPool`], the caller's thread included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadCount {
    /// As many threads as [`std::thread::available_parallelism`] reports, or one if the system
    /// cannot tell.
    Available,
    /// Exactly this many threads.
    Fixed(NonZeroU32),
}

/// The smallest stack a [`WorkerPool`] worker may have: below it, ordinary code overflows.
pub const STACK_SIZE_BYTES_MIN: u32 = 64 * 1024;

/// Settings of a [`WorkerPool`].
///
/// There is no `Default`: every pool states its thread count, stack size, name and limits at the
/// call site, so a change of library default can never change a program silently.
#[derive(Debug, Clone, Copy)]
pub struct PoolConfig {
    /// The number of threads that map a batch: the caller's thread and the workers.
    pub thread_count: ThreadCount,
    /// Stack size of each worker. Untouched stack pages cost no memory, so a large value only
    /// reserves address space and protects deep call chains.
    pub stack_size_bytes: u32,
    /// Worker threads are named `{name_prefix}-{index}`, which shows up in profilers.
    pub name_prefix: &'static str,
    /// The number of items one batch holds at most: the pool allocates that many slots.
    pub batch_item_count_max: u32,
    /// The number of items from which a batch is shared with the workers: 2 for few but
    /// expensive items, [`PARALLEL_LEN_MIN`] for many cheap ones.
    pub parallel_item_count_min: u32,
}

impl PoolConfig {
    /// Returns the thread count, asking the system for [`ThreadCount::Available`].
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

/// The function of a [`WorkerPool`]: it reads the shared context, takes an item and returns its
/// output.
pub type MapFunction<Context, Item, Output> = fn(&Context, Item) -> Output;

/// A function that a [`WorkerPool`] calls once on each new worker, with the shared context and
/// the index of the worker.
pub type StartHandler<Context> = fn(&Context, u32);

/// One slot per item of the largest batch of a [`WorkerPool`], each locked by the one thread
/// that fills or empties it.
type Slots<Value> = Box<[Mutex<Option<Value>>]>;

/// What a [`WorkerPool`] does: a function, the context it reads, and how a worker starts.
///
/// The context is shared by every thread for as long as the pool lives, so it holds what the
/// function needs besides its item: tables, an interner, a diagnostic context.
#[derive(Debug, Clone, Copy)]
pub struct Work<Context, Item, Output> {
    /// The state every call of `map` reads, shared between the threads.
    pub context: Context,
    /// The function applied to each item. It only computes: it gets the item and returns the
    /// output, and the pool moves both.
    pub map: MapFunction<Context, Item, Output>,
    /// Called once on each worker thread, with its index, before the pool is ready. Use it to
    /// make a thread allocate what it will need while the program still initializes: its arena
    /// of a [`ThreadArenas`](crate::arena::ThreadArenas), a thread-local table.
    pub start_handler: Option<StartHandler<Context>>,
}

/// The progress of the batch that a [`WorkerPool`] is mapping, guarded by one lock.
#[derive(Debug, Clone, Copy)]
struct Progress {
    /// The number of the batch, which tells a waiting worker that a new one started.
    generation: u64,
    /// The number of items of the batch.
    item_count: u32,
    /// The index of the first item no thread claimed yet.
    index_next: u32,
    /// The number of items whose output is written.
    done_count: u32,
    /// The number of items a thread claims at once.
    chunk_item_count: u32,
    /// The number of workers that finished starting.
    worker_ready_count: u32,
    /// Set when the pool is dropped, so the workers return.
    stopping: bool,
    /// Set when a worker panicked, so the caller stops waiting for it.
    panicked: bool,
}

/// What the threads of a [`WorkerPool`] share, allocated once when the pool is created.
struct Shared<Context, Item, Output> {
    /// The function, its context and the start handler.
    work: Work<Context, Item, Output>,
    /// One slot per item of the largest batch: the caller fills them, a thread empties the ones
    /// it claimed.
    inputs: Slots<Item>,
    /// One slot per item of the largest batch: the thread that claimed an item fills its slot,
    /// the caller empties them in order.
    outputs: Slots<Output>,
    /// Where the current batch stands.
    progress: Mutex<Progress>,
    /// Signalled when a batch starts or the pool stops: the workers wait on it.
    batch_started: Condvar,
    /// Signalled when a batch is complete, a worker is ready or a worker panicked: the caller
    /// waits on it.
    batch_advanced: Condvar,
}

/// The state of a [`WorkerPool`], owned jointly by the pool and by each of its workers.
type SharedHandle<Context, Item, Output> = Arc<Shared<Context, Item, Output>>;

/// A fixed set of threads that map batches of items with one function, in input order.
///
/// Everything is allocated by [`new`](Self::new): the threads, and one input and one output
/// slot per item of the largest batch. [`map`](Self::map) allocates nothing. Dropping the pool
/// stops and joins the workers.
pub struct WorkerPool<Context, Item, Output> {
    /// The slots, the progress and the work, shared with the workers.
    shared: SharedHandle<Context, Item, Output>,
    /// The worker threads: every thread of the pool but the caller's.
    workers: Vec<thread::JoinHandle<()>>,
    /// The thread that created the pool, the only one that may map with it.
    owner: thread::ThreadId,
    /// The number of items from which a batch is shared with the workers.
    parallel_item_count_min: u32,
}

impl<Context, Item, Output> fmt::Debug for WorkerPool<Context, Item, Output> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        assert!(
            self.parallel_item_count_min >= 2,
            "checked by `new`: a shared batch has two items"
        );
        formatter
            .debug_struct("WorkerPool")
            .field("worker_count", &self.workers.len())
            .field("batch_item_count_max", &self.shared.inputs.len())
            .field("parallel_item_count_min", &self.parallel_item_count_min)
            .finish_non_exhaustive()
    }
}

impl<Context, Item, Output> WorkerPool<Context, Item, Output>
where
    Context: Send + Sync + 'static,
    Item: Send + 'static,
    Output: Send + 'static,
{
    /// Creates the pool: allocates its slots, spawns its workers, runs the start handler on each
    /// and returns once every worker waits for its first batch.
    ///
    /// # Errors
    ///
    /// Returns the error of the operating system if it refuses to spawn a thread. The workers
    /// spawned before the error are stopped and joined.
    ///
    /// # Panics
    ///
    /// Panics on an empty `name_prefix`, a stack smaller than [`STACK_SIZE_BYTES_MIN`], a batch
    /// of no item, a threshold below two items, or if a start handler panics.
    pub fn new(config: &PoolConfig, work: Work<Context, Item, Output>) -> io::Result<Self> {
        assert!(!config.name_prefix.is_empty(), "worker threads need a name prefix");
        assert!(
            config.stack_size_bytes >= STACK_SIZE_BYTES_MIN,
            "worker stacks below 64 KiB overflow"
        );
        assert!(config.batch_item_count_max > 0, "a batch holds at least one item");
        assert!(config.parallel_item_count_min >= 2, "a shared batch has at least two items");
        let slot_count = usize_from_u32(config.batch_item_count_max);
        let worker_count = config.thread_count_resolved() - 1;
        let shared = Arc::new(Shared {
            work,
            inputs: iter::repeat_with(|| Mutex::new(None)).take(slot_count).collect(),
            outputs: iter::repeat_with(|| Mutex::new(None)).take(slot_count).collect(),
            progress: Mutex::new(Progress::IDLE),
            batch_started: Condvar::new(),
            batch_advanced: Condvar::new(),
        });
        let mut pool = Self {
            shared,
            workers: Vec::with_capacity(usize_from_u32(worker_count)),
            owner: thread::current().id(),
            parallel_item_count_min: config.parallel_item_count_min,
        };
        park_once();
        for worker_index in 0..worker_count {
            let worker_shared = Arc::clone(&pool.shared);
            let builder = thread::Builder::new()
                .name(format!("{}-{worker_index}", config.name_prefix))
                .stack_size(usize_from_u32(config.stack_size_bytes));
            // An error drops `pool`, which stops and joins the workers spawned so far.
            pool.workers.push(builder.spawn(move || new_worker(&worker_shared, worker_index))?);
        }

        pool.new_wait_ready(worker_count);
        Ok(pool)
    }

    /// Waits until every worker ran the start handler and waits for its first batch.
    fn new_wait_ready(&self, worker_count: u32) {
        let mut progress = self.shared.progress.lock();
        self.shared.batch_advanced.wait_while(&mut progress, |current| {
            if current.panicked { false } else { current.worker_ready_count < worker_count }
        });
        assert!(!progress.panicked, "a start handler panicked");
        assert_eq!(progress.worker_ready_count, worker_count, "every worker is ready");
    }

    /// Returns the number of threads that map a batch: the workers and the caller's thread.
    ///
    /// # Panics
    ///
    /// Panics if the pool holds more than `u32::MAX` workers, which `new` rules out.
    #[must_use = "the count is queried, not changed"]
    pub const fn thread_count(&self) -> u32 {
        u32_from_usize(self.workers.len()) + 1
    }

    /// Returns the context that the function of the pool reads.
    #[must_use = "the context is borrowed, not changed"]
    pub fn context(&self) -> &Context {
        &self.shared.work.context
    }

    /// Returns the number of items one batch holds at most.
    ///
    /// # Panics
    ///
    /// Panics if the pool lost its slots, a broken invariant.
    #[must_use = "the limit is queried, not changed"]
    pub fn batch_item_count_max(&self) -> u32 {
        let slot_count = u32_from_usize(self.shared.inputs.len());
        assert!(slot_count > 0, "checked by `new`: a batch holds at least one item");
        assert_eq!(self.shared.outputs.len(), self.shared.inputs.len(), "one output per input");
        slot_count
    }

    /// Moves every item out of `items`, applies the function of the pool to each, and appends
    /// the outputs to `outputs` in the order of the items. Allocates nothing: `items` keeps its
    /// buffer, empty, and `outputs` must already have room.
    ///
    /// A batch below `parallel_item_count_min` items, or any batch of a pool with one thread,
    /// runs on the caller's thread. Otherwise the caller and the workers claim chunks of the
    /// batch until none is left, and the caller returns once every output is written.
    ///
    /// # Panics
    ///
    /// Panics if `outputs` is not empty or has room for fewer outputs than there are items, if
    /// there are more items than [`batch_item_count_max`](Self::batch_item_count_max), if the
    /// caller is not the thread that created the pool, or if the function panicked on a worker.
    #[expect(
        clippy::needless_pass_by_ref_mut,
        reason = "a pool maps one batch at a time, and exclusive access rules out a second caller"
    )]
    pub fn map(&mut self, items: &mut Vec<Item>, outputs: &mut Vec<Output>) {
        let item_count = u32_from_usize(items.len());
        assert!(outputs.is_empty(), "the outputs start empty");
        assert!(outputs.capacity() >= items.len(), "the outputs have room for every item");
        assert!(item_count <= self.batch_item_count_max(), "a batch holds too many items");
        assert!(thread::current().id() == self.owner, "only the thread that created a pool maps");
        if self.workers.is_empty() {
            self.map_inline(items, outputs);
        } else {
            if item_count < self.parallel_item_count_min {
                self.map_inline(items, outputs);
            } else {
                self.map_pooled(items, outputs);
            }
        }
        assert!(items.is_empty(), "every item moved into the pool");
        assert_eq!(u32_from_usize(outputs.len()), item_count, "one output per item");
    }

    /// Maps the batch on the caller's thread, in order.
    fn map_inline(&self, items: &mut Vec<Item>, outputs: &mut Vec<Output>) {
        let allocation_before = outputs.capacity();
        assert!(allocation_before >= items.len(), "checked by `map`: the outputs have room");
        let work = &self.shared.work;
        for item in items.drain(..) {
            outputs.push((work.map)(&work.context, item));
        }
        assert_eq!(outputs.capacity(), allocation_before, "the outputs never reallocate");
    }

    /// Maps the batch with the workers: fills the input slots, starts the batch, takes part in
    /// it, waits for its last output, and empties the output slots in order.
    fn map_pooled(&self, items: &mut Vec<Item>, outputs: &mut Vec<Output>) {
        let shared = &*self.shared;
        let item_count = u32_from_usize(items.len());
        let allocation_before = outputs.capacity();
        for (slot, item) in shared.inputs.iter().zip(items.drain(..)) {
            let previous = slot.lock().replace(item);
            assert!(previous.is_none(), "an input slot is empty between batches");
        }
        {
            let mut progress = shared.progress.lock();
            assert_eq!(progress.done_count, progress.item_count, "the last batch is complete");
            let chunk_count = self.thread_count() * CHUNK_COUNT_PER_THREAD;
            *progress = Progress {
                generation: progress.generation + 1,
                item_count,
                index_next: 0,
                done_count: 0,
                chunk_item_count: item_count.div_ceil(chunk_count),
                ..*progress
            };
        }
        let _woken = shared.batch_started.notify_all();
        work_batch(shared);

        // The lock is kept while the outputs are taken: the workers are idle by then.
        let mut progress = shared.progress.lock();
        shared.batch_advanced.wait_while(&mut progress, |current| {
            if current.panicked { false } else { current.done_count < current.item_count }
        });
        assert!(!progress.panicked, "the function of a `WorkerPool` panicked on a worker");
        for slot in shared.outputs.iter().take(usize_from_u32(item_count)) {
            let Some(output) = slot.lock().take() else {
                unreachable!("a complete batch has every output");
            };
            outputs.push(output);
        }
        assert_eq!(outputs.capacity(), allocation_before, "the outputs never reallocate");
    }
}

impl<Context, Item, Output> Drop for WorkerPool<Context, Item, Output> {
    fn drop(&mut self) {
        {
            let mut progress = self.shared.progress.lock();
            progress.stopping = true;
        }
        let _woken = self.shared.batch_started.notify_all();
        for worker in self.workers.drain(..) {
            // A worker that panicked already reported it to the caller of `map` or `new`.
            let _joined = worker.join();
        }
        assert!(self.workers.is_empty(), "every worker is joined");
    }
}

impl Progress {
    /// No batch yet, and no worker ready.
    const IDLE: Self = Self {
        generation: 0,
        item_count: 0,
        index_next: 0,
        done_count: 0,
        chunk_item_count: 1,
        worker_ready_count: 0,
        stopping: false,
        panicked: false,
    };
}

/// Parks the calling thread once, for no time.
///
/// `parking_lot` allocates its table of parked threads when a thread parks for the first time.
/// Parking every thread of a pool while the program still initializes moves that allocation
/// before the allocator is sealed: afterwards a contended lock parks without allocating.
fn park_once() {
    let mutex = Mutex::new(());
    let condition = Condvar::new();
    let mut guard = mutex.lock();
    let waited = condition.wait_for(&mut guard, Duration::ZERO);
    assert!(waited.timed_out(), "nothing signals a private condition");
}

/// Reports a panic of its worker thread to the caller, who would otherwise wait forever for the
/// outputs of that worker. Release builds abort on a panic and never get here.
struct PanicReport<'shared, Context, Item, Output> {
    /// The pool the worker belongs to.
    shared: &'shared Shared<Context, Item, Output>,
}

impl<Context, Item, Output> Drop for PanicReport<'_, Context, Item, Output> {
    fn drop(&mut self) {
        if thread::panicking() {
            {
                let mut progress = self.shared.progress.lock();
                progress.panicked = true;
            }
            let _woken = self.shared.batch_advanced.notify_all();
        }
    }
}

/// The body of a worker thread: starts, then maps its share of every batch until the pool stops.
fn new_worker<Context, Item, Output>(shared: &Shared<Context, Item, Output>, worker_index: u32) {
    let _report = PanicReport { shared };
    park_once();
    if let Some(start_handler) = shared.work.start_handler {
        start_handler(&shared.work.context, worker_index);
    }
    {
        let mut progress = shared.progress.lock();
        progress.worker_ready_count += 1;
    }
    let _woken = shared.batch_advanced.notify_all();

    let mut generation_seen = 0_u64;
    // Bounded: one round per batch, and a `u64` numbers the batches.
    for _ in 0..u64::MAX {
        let Some(generation) = new_worker_wait(shared, generation_seen) else {
            return;
        };
        assert!(generation > generation_seen, "batches are numbered in order");
        generation_seen = generation;
        work_batch(shared);
    }
}

/// Waits for a batch newer than `generation_seen` and returns its number, or nothing once the
/// pool stops.
fn new_worker_wait<Context, Item, Output>(
    shared: &Shared<Context, Item, Output>,
    generation_seen: u64,
) -> Option<u64> {
    let mut progress = shared.progress.lock();
    shared.batch_started.wait_while(&mut progress, |current| {
        if current.stopping { false } else { current.generation == generation_seen }
    });
    assert!(progress.generation >= generation_seen, "the batch number only grows");
    if progress.stopping { None } else { Some(progress.generation) }
}

/// Claims chunks of the current batch and maps their items, until no chunk is left.
///
/// The caller of `map` and the workers all run this. A thread that arrives after the batch is
/// fully claimed finds no chunk and returns.
fn work_batch<Context, Item, Output>(shared: &Shared<Context, Item, Output>) {
    let work = &shared.work;
    assert_eq!(shared.inputs.len(), shared.outputs.len(), "one output slot per input slot");
    // Bounded: every round claims at least one item, and a batch has at most one per slot.
    for _ in 0..=shared.inputs.len() {
        let Some(chunk) = work_batch_claim(shared) else {
            return;
        };
        let chunk_item_count = u32_from_usize(chunk.len());
        for index in chunk {
            // The locks are never contended: an index belongs to the thread that claimed it.
            let Some(item) = shared.inputs[index].lock().take() else {
                unreachable!("a claimed item is in its slot");
            };
            let output = (work.map)(&work.context, item);
            let previous = shared.outputs[index].lock().replace(output);
            assert!(previous.is_none(), "an output is written once");
        }
        work_batch_finish(shared, chunk_item_count);
    }
}

/// Claims the next chunk of the current batch, or nothing if every item is claimed.
fn work_batch_claim<Context, Item, Output>(
    shared: &Shared<Context, Item, Output>,
) -> Option<Range<usize>> {
    let mut progress = shared.progress.lock();
    assert!(progress.index_next <= progress.item_count, "claims stay within the batch");
    assert!(progress.chunk_item_count > 0, "a chunk holds at least one item");
    (progress.index_next < progress.item_count).then(|| {
        let start = progress.index_next;
        let end = start + progress.chunk_item_count.min(progress.item_count - start);
        progress.index_next = end;
        usize_from_u32(start)..usize_from_u32(end)
    })
}

/// Counts the items of a mapped chunk as done, and wakes the caller after the last one.
fn work_batch_finish<Context, Item, Output>(
    shared: &Shared<Context, Item, Output>,
    chunk_item_count: u32,
) {
    assert!(chunk_item_count > 0, "a claimed chunk holds at least one item");
    let complete = {
        let mut progress = shared.progress.lock();
        progress.done_count += chunk_item_count;
        assert!(progress.done_count <= progress.item_count, "an item is done once");
        progress.done_count == progress.item_count
    };
    if complete {
        let _woken = shared.batch_advanced.notify_all();
    }
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
/// Built for the "accumulate per thread, then merge" pattern: each thread fills its own
/// `ShardedMap` with no locking, then [`merge_from`](Self::merge_from) moves the entries of one
/// map into another, shard by shard. Both maps keep their allocation, so the emptied one is
/// ready for the next round.
///
/// Each shard holds `capacity / SHARD_COUNT` entries, rounded up, allocated when the map is
/// created, and an insert asserts that its shard has room: the map never reallocates. Keys must
/// spread evenly over the shards, so leave headroom in the capacity for an uneven spread.
///
/// Keys must be unique across the merged maps: a key held by both is a broken invariant, which
/// the merge asserts.
#[derive(Debug, Clone)]
pub struct ShardedMap<Key, Value> {
    /// The shards, boxed so that moving the map moves one pointer.
    shards: Box<Shards<Key, Value>>,
    /// The number of entries each shard holds at most, fixed at construction.
    shard_capacity: u32,
}

/// One shard of a [`ShardedMap`].
type Shard<Key, Value> = FxHashMap<Key, Value>;

/// Every shard of a [`ShardedMap`].
type Shards<Key, Value> = [Shard<Key, Value>; SHARD_COUNT_USIZE];

impl<Key, Value> ShardedMap<Key, Value>
where
    Key: Hash + Eq,
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
        let shards: Box<Shards<Key, Value>> = Box::new(array::from_fn(|_| {
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
    fn shard_index(key: &Key) -> usize {
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
    pub fn insert(&mut self, key: Key, value: Value) -> Option<Value> {
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
    pub fn get(&self, key: &Key) -> Option<&Value> {
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
    pub fn iter(&self) -> impl Iterator<Item = (&Key, &Value)> {
        self.shards.iter().flat_map(|shard| shard.iter())
    }

    /// Moves every entry of `source` into this map, shard by shard, and leaves `source` empty.
    /// Allocates nothing: both maps keep the shards they were created with.
    ///
    /// # Panics
    ///
    /// Panics if the two maps have different capacities, if a key is in both, or if a shard of
    /// this map has no room for the entries of the same shard of `source`.
    #[expect(clippy::iter_over_hash_type, reason = "entries go into a map, so order is irrelevant")]
    pub fn merge_from(&mut self, source: &mut Self) {
        assert_eq!(self.shard_capacity, source.shard_capacity, "merged maps have one capacity");
        let shard_capacity = usize_from_u32(self.shard_capacity);
        let entry_count = self.len() + source.len();
        for (target, shard) in self.shards.iter_mut().zip(source.shards.iter_mut()) {
            let allocation_before = target.capacity();
            for (key, value) in shard.drain() {
                assert!(!target.contains_key(&key), "duplicate key in `ShardedMap::merge_from`");
                assert!(target.len() < shard_capacity, "a merged shard of a `ShardedMap` is full");
                let _previous = target.insert(key, value);
            }
            assert_eq!(target.capacity(), allocation_before, "a sharded map never reallocates");
        }
        assert!(source.is_empty(), "every entry left the source");
        assert_eq!(self.len(), entry_count, "every entry of both maps is kept");
    }
}

/// Skips running the destructor of `value`, leaking its memory.
///
/// Meant for the very end of a short-lived process: tearing down large graphs one node at a
/// time only to exit right after is wasted work, since the OS reclaims the memory anyway. Never
/// use it on values that own non-memory resources whose release matters (files to flush, locks).
#[expect(clippy::mem_forget, reason = "skipping teardown is the purpose of this function")]
pub const fn skip_drop<Item>(value: Item) {
    mem::forget(value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicU32, Ordering};

    /// Returns a pool configuration with `thread_count` threads, batches of up to 1024 items and
    /// the suggested threshold.
    fn config(thread_count: u32) -> PoolConfig {
        let Some(count) = NonZeroU32::new(thread_count) else {
            panic!("a test pool has at least one thread");
        };
        PoolConfig {
            thread_count: ThreadCount::Fixed(count),
            stack_size_bytes: 2 * 1024 * 1024,
            name_prefix: "test",
            batch_item_count_max: 1024,
            parallel_item_count_min: PARALLEL_LEN_MIN,
        }
    }

    /// Doubles an item. The context counts the calls.
    fn double(calls: &AtomicU32, item: u32) -> u32 {
        // Relaxed: the tests read the count after `map` returned, which joined the batch.
        let _calls_before = calls.fetch_add(1, Ordering::Relaxed);
        item * 2
    }

    /// Returns a pool that doubles, with `thread_count` threads.
    fn doubling_pool(thread_count: u32) -> WorkerPool<AtomicU32, u32, u32> {
        let work = Work { context: AtomicU32::new(0), map: double, start_handler: None };
        match WorkerPool::new(&config(thread_count), work) {
            Ok(pool) => pool,
            Err(error) => panic!("the system spawns the test threads: {error}"),
        }
    }

    /// Maps batches of every size around the threshold and up to the largest batch, on one, two
    /// and four threads, reusing the same buffers: the outputs come back in input order, the
    /// items are consumed, and the function runs once per item.
    #[test]
    fn outputs_keep_the_input_order() {
        let batch_sizes =
            [0, 1, PARALLEL_LEN_MIN - 1, PARALLEL_LEN_MIN, PARALLEL_LEN_MIN + 1, 1024];
        for thread_count in [1, 2, 4] {
            let mut pool = doubling_pool(thread_count);
            assert_eq!(pool.thread_count(), thread_count);
            assert_eq!(pool.batch_item_count_max(), 1024);
            let mut items = Vec::with_capacity(1024);
            let mut outputs = Vec::with_capacity(1024);
            let mut call_count = 0;
            for item_count in batch_sizes {
                items.extend(0..item_count);
                pool.map(&mut items, &mut outputs);
                assert!(items.is_empty(), "the items moved into the pool");
                assert!(outputs.iter().copied().eq((0..item_count).map(|item| item * 2)));
                outputs.clear();
                call_count += item_count;
                let calls = pool.shared.work.context.load(Ordering::Relaxed);
                assert_eq!(calls, call_count, "the function runs once per item");
            }
            assert_eq!(outputs.capacity(), 1024, "the outputs never reallocate");
        }
    }

    /// Maps many batches in a row on four threads, so workers that are still leaving one batch
    /// meet the next one.
    #[test]
    fn batches_in_a_row_stay_correct() {
        let mut pool = doubling_pool(4);
        let mut items = Vec::with_capacity(1024);
        let mut outputs = Vec::with_capacity(1024);
        for round in 0..200_u32 {
            let item_count = PARALLEL_LEN_MIN + round;
            items.extend(round..round + item_count);
            pool.map(&mut items, &mut outputs);
            assert!(outputs.iter().copied().eq((round..round + item_count).map(|item| item * 2)));
            outputs.clear();
        }
    }

    /// Records the thread that maps an item.
    #[expect(clippy::trivially_copy_pass_by_ref, reason = "the signature of a pool function")]
    fn thread_of(_context: &(), _item: u8) -> thread::ThreadId {
        thread::current().id()
    }

    /// Maps on a pool of one thread: there is no worker, so every item runs on the caller.
    #[test]
    fn a_pool_of_one_thread_maps_inline() {
        let work = Work { context: (), map: thread_of, start_handler: None };
        let mut pool = WorkerPool::new(&config(1), work).unwrap();
        assert!(pool.workers.is_empty(), "one thread means no worker");
        let mut items = vec![0_u8; 500];
        let mut outputs = Vec::with_capacity(500);
        pool.map(&mut items, &mut outputs);
        let caller = thread::current().id();
        assert!(outputs.iter().all(|thread| *thread == caller), "every item ran inline");
    }

    /// Counts the workers that started, and adds up their indices.
    fn count_start(started: &[AtomicU32; 2], worker_index: u32) {
        let named = thread::current().name().is_some_and(|name| name.starts_with("test-"));
        assert!(named, "a worker is named after the prefix");
        // Relaxed: the test reads the counts after `new` returned, which waited for the workers.
        let _count_before = started[0].fetch_add(1, Ordering::Relaxed);
        let _sum_before = started[1].fetch_add(worker_index, Ordering::Relaxed);
    }

    /// Creates a pool of four threads with a start handler: when `new` returns, the handler ran
    /// once on each of the three workers, named after the prefix, with the indices 0, 1 and 2.
    #[test]
    fn the_start_handler_runs_on_every_worker_before_the_pool_is_ready() {
        let context = [AtomicU32::new(0), AtomicU32::new(0)];
        let work = Work { context, map: |_, item: u8| item, start_handler: Some(count_start) };
        let pool = WorkerPool::new(&config(4), work).unwrap();
        let started = &pool.shared.work.context;
        assert_eq!(started[0].load(Ordering::Relaxed), 3, "three workers started");
        assert_eq!(started[1].load(Ordering::Relaxed), 3, "the indices are 0, 1 and 2");
    }

    /// Fails on one item of a shared batch. Whichever thread maps it, the caller panics instead
    /// of waiting forever, and dropping the pool still joins the workers.
    #[test]
    #[should_panic(expected = "panicked")]
    fn a_panic_in_the_function_reaches_the_caller() {
        let fail = |&(): &(), item: u32| {
            assert!(item != 700, "the function panicked on item 700");
            item
        };
        let work = Work { context: (), map: fail, start_handler: None };
        let mut pool = WorkerPool::new(&config(4), work).unwrap();
        let mut items: Vec<u32> = (0..1024).collect();
        let mut outputs = Vec::with_capacity(1024);
        pool.map(&mut items, &mut outputs);
    }

    /// Maps one item more than the largest batch, which must crash.
    #[test]
    #[should_panic(expected = "a batch holds too many items")]
    fn map_rejects_batches_above_the_limit() {
        let mut pool = doubling_pool(1);
        let mut items: Vec<u32> = (0..1025).collect();
        let mut outputs = Vec::with_capacity(1025);
        pool.map(&mut items, &mut outputs);
    }

    /// Maps into outputs with room for one output fewer than there are items, which must crash
    /// rather than grow the buffer.
    #[test]
    #[should_panic(expected = "the outputs have room for every item")]
    fn map_rejects_outputs_without_room() {
        let mut pool = doubling_pool(1);
        let mut items: Vec<u32> = (0..8).collect();
        let mut outputs = Vec::with_capacity(7);
        assert!(outputs.capacity() < 8, "the buffer is too small on purpose");
        pool.map(&mut items, &mut outputs);
    }

    /// Maps into outputs that still hold a value, which must crash.
    #[test]
    #[should_panic(expected = "the outputs start empty")]
    fn map_rejects_outputs_that_are_not_empty() {
        let mut pool = doubling_pool(1);
        let mut items: Vec<u32> = (0..8).collect();
        let mut outputs = Vec::with_capacity(16);
        outputs.push(1);
        pool.map(&mut items, &mut outputs);
    }

    /// Resolves the available thread count, which is at least one, and a fixed one, used as
    /// given.
    #[test]
    fn thread_counts_resolve() {
        let available = PoolConfig { thread_count: ThreadCount::Available, ..config(1) };
        assert!(available.thread_count_resolved() >= 1);
        assert_eq!(config(3).thread_count_resolved(), 3);
    }

    /// Accepts a stack of exactly the minimum size.
    #[test]
    fn a_pool_accepts_the_minimum_stack() {
        let smallest = PoolConfig { stack_size_bytes: STACK_SIZE_BYTES_MIN, ..config(2) };
        let work = Work { context: AtomicU32::new(0), map: double, start_handler: None };
        let pool = WorkerPool::new(&smallest, work).unwrap();
        assert_eq!(pool.thread_count(), 2);
    }

    /// Asks for a stack one byte below the minimum, which must crash.
    #[test]
    #[should_panic(expected = "worker stacks below 64 KiB overflow")]
    fn a_pool_rejects_tiny_stacks() {
        let tiny = PoolConfig { stack_size_bytes: STACK_SIZE_BYTES_MIN - 1, ..config(2) };
        let work = Work { context: AtomicU32::new(0), map: double, start_handler: None };
        let _pool = WorkerPool::new(&tiny, work);
    }

    /// Asks for workers without a name prefix, which must crash.
    #[test]
    #[should_panic(expected = "worker threads need a name prefix")]
    fn a_pool_rejects_empty_names() {
        let unnamed = PoolConfig { name_prefix: "", ..config(2) };
        let work = Work { context: AtomicU32::new(0), map: double, start_handler: None };
        let _pool = WorkerPool::new(&unnamed, work);
    }

    /// Asks for batches of no item, and for a threshold of one item, which must both crash.
    #[test]
    #[should_panic(expected = "a batch holds at least one item")]
    fn a_pool_rejects_empty_batches() {
        let empty = PoolConfig { batch_item_count_max: 0, ..config(2) };
        let work = Work { context: AtomicU32::new(0), map: double, start_handler: None };
        let _pool = WorkerPool::new(&empty, work);
    }

    /// Asks for a threshold of one item, which must crash: one item cannot be shared.
    #[test]
    #[should_panic(expected = "a shared batch has at least two items")]
    fn a_pool_rejects_a_threshold_of_one_item() {
        let unshared = PoolConfig { parallel_item_count_min: 1, ..config(2) };
        let work = Work { context: AtomicU32::new(0), map: double, start_handler: None };
        let _pool = WorkerPool::new(&unshared, work);
    }

    /// Returns `count` distinct keys that all land in shard 0.
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

    /// Fills two maps with the two halves of 10 000 keys and merges one into the other: the
    /// target holds every entry, the source is empty and keeps its capacity, and merging the
    /// emptied source again changes nothing.
    #[test]
    fn merge_from_moves_every_entry() {
        let capacity = 30_000;
        let mut target = ShardedMap::with_capacity(capacity);
        let mut source = ShardedMap::with_capacity(capacity);
        for key in 0..5_000_u32 {
            let _previous = target.insert(key, key * 3);
            let _other = source.insert(key + 5_000, (key + 5_000) * 3);
        }
        target.merge_from(&mut source);
        assert_eq!(target.len(), 10_000);
        assert!(source.is_empty(), "the source gave every entry away");
        assert_eq!(source.capacity(), target.capacity(), "the source keeps its shards");
        assert!((0..10_000_u32).all(|key| target.get(&key) == Some(&(key * 3))));
        target.merge_from(&mut source);
        assert_eq!(target.len(), 10_000, "an empty source adds nothing");
    }

    /// Merges two maps that share a key, which must crash.
    #[test]
    #[should_panic(expected = "duplicate key in `ShardedMap::merge_from`")]
    fn merge_from_rejects_duplicate_keys() {
        let mut target = ShardedMap::with_capacity(SHARD_COUNT * 2);
        let mut source = ShardedMap::with_capacity(SHARD_COUNT * 2);
        let _previous = target.insert(7_u8, 1_u8);
        let _other = source.insert(7_u8, 2_u8);
        target.merge_from(&mut source);
    }

    /// Merges two maps whose shard 0 entries together exceed the shard capacity, which must
    /// crash.
    #[test]
    #[should_panic(expected = "a merged shard of a `ShardedMap` is full")]
    fn merge_from_rejects_entries_above_capacity() {
        let keys = keys_of_shard_zero(2);
        let mut target = ShardedMap::with_capacity(SHARD_COUNT);
        let mut source = ShardedMap::with_capacity(SHARD_COUNT);
        let _previous = target.insert(keys[0], 0_u8);
        let _other = source.insert(keys[1], 1_u8);
        target.merge_from(&mut source);
    }

    /// Merges maps of different capacities, which must crash.
    #[test]
    #[should_panic(expected = "merged maps have one capacity")]
    fn merge_from_rejects_maps_of_different_capacities() {
        let mut target = ShardedMap::<u8, u8>::with_capacity(SHARD_COUNT);
        let mut source = ShardedMap::<u8, u8>::with_capacity(SHARD_COUNT * 2);
        target.merge_from(&mut source);
    }

    /// Skips the drop of an empty vector. An empty `Vec` owns no heap memory, so the skipped
    /// drop leaks nothing that Miri or the leak sanitizer would report.
    #[test]
    fn skip_drop_takes_its_value() {
        let empty = Vec::<u8>::new();
        assert_eq!(empty.capacity(), 0, "nothing to leak");
        skip_drop(empty);
    }
}
