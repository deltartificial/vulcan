//! Allocates during initialization, seals the global allocator, then runs a steady state that
//! must not allocate: it reuses the buffers made at startup. The steady state fills and drains a
//! bounded queue, and maps batches on a pool of threads whose function reports diagnostics, so
//! the pool, its locks under contention and the diagnostic buffers are all checked.
//!
//! The binary runs without the test harness (`harness = false` in `Cargo.toml`), because the
//! harness allocates after every test. An allocation after `seal` aborts the process, which fails
//! the test with a non-zero exit status.
//!
//! The opposite case is checked too: before sealing, the binary runs itself with the environment
//! variable `ALLOCATE_AFTER_SEAL` set, and that child allocates after sealing, which must abort it.

use core::num::NonZeroU32;
use std::{env, process::Command, sync::OnceLock};
use vulcan::{
    allocator::{Allocator, BACKEND, StaticAllocator},
    bounded::BoundedQueue,
    convert::usize_from_u32,
    diagnostic::{DiagnosticBatch, DiagnosticContext, DiagnosticLimits},
    division::Divide as _,
    sync::{PoolConfig, ThreadCount, Work, WorkerPool},
};

fn main() {
    if env::var_os(ALLOCATE_AFTER_SEAL).is_some() {
        main_allocate_after_seal();
    } else {
        // Initialization: spawning the child allocates, so it happens before the seal.
        main_check_child_aborts();
        let mut queue = BoundedQueue::with_capacity(QUEUE_CAPACITY);
        let mut mapping = main_mapping();
        ALLOCATOR.seal();
        assert!(ALLOCATOR.is_sealed(), "initialization is over");
        main_steady_state(&mut queue);
        main_steady_state_mapping(&mut mapping);
    }
}

/// What the pool reads on every thread: the diagnostics of the program, which live as long as it.
type Context = &'static DiagnosticContext<u32>;

/// What the pool returns for an item: its square, and the batch of what it reported.
type Output = (u64, DiagnosticBatch<'static, u32>);

/// A pool and the buffers it maps with, all allocated during initialization.
struct Mapping {
    /// The threads and their slots.
    pool: WorkerPool<Context, u32, Output>,
    /// The items of one batch, refilled for every batch.
    items: Vec<u32>,
    /// The outputs of one batch, drained after every batch.
    outputs: Vec<Output>,
    /// The diagnostics of one batch, swapped with the buffer of the context.
    diagnostics: Vec<u32>,
}

/// The diagnostics of the program: a static, since the workers read them for as long as the
/// process lives.
static DIAGNOSTICS: OnceLock<DiagnosticContext<u32>> = OnceLock::new();

/// The number of threads of the pool: enough for the workers to contend on the locks.
const THREAD_COUNT: NonZeroU32 = NonZeroU32::MIN.saturating_add(3);

/// The number of items of one batch, well above the threshold from which a batch is shared.
const BATCH_ITEM_COUNT: u32 = 512;

/// One item in this many reports a diagnostic.
const REPORT_PERIOD: u32 = 64;

/// The number of batches mapped after the seal. Miri interprets every instruction, so it maps
/// a few batches only: enough to cross the threads, not to measure anything.
const BATCH_COUNT: u32 = if cfg!(miri) { 3 } else { 200 };

// A batch reports `BATCH_ITEM_COUNT / REPORT_PERIOD` diagnostics, one per batch buffer.
const _: () = assert!(BATCH_ITEM_COUNT.is_multiple_of(REPORT_PERIOD), "a whole number of reports");

/// Squares an item, and reports every item that is a multiple of [`REPORT_PERIOD`].
#[expect(clippy::trivially_copy_pass_by_ref, reason = "the signature of a pool function")]
fn main_mapping_square(context: &Context, item: u32) -> Output {
    let mut batch = context.batch();
    assert!(batch.is_empty(), "a new batch holds nothing");
    if item.remainder(REPORT_PERIOD) == 0 {
        batch.warn(item);
    }
    (u64::from(item) * u64::from(item), batch)
}

/// Creates the pool, its buffers and the diagnostic context: everything the mapping will use.
fn main_mapping() -> Mapping {
    let report_count = BATCH_ITEM_COUNT.divide_exact(REPORT_PERIOD);
    let limits = DiagnosticLimits {
        diagnostic_count_max: report_count,
        batch_buffer_count: report_count,
        batch_diagnostic_count_max: 1,
    };
    let context: Context = DIAGNOSTICS.get_or_init(|| DiagnosticContext::new(limits));
    let config = PoolConfig {
        thread_count: ThreadCount::Fixed(THREAD_COUNT),
        stack_size_bytes: 2 * 1024 * 1024,
        name_prefix: "static",
        batch_item_count_max: BATCH_ITEM_COUNT,
        parallel_item_count_min: 2,
    };
    let work = Work { context, map: main_mapping_square, start_handler: None };
    let created = WorkerPool::new(&config, work);
    assert!(created.is_ok(), "the system spawns the worker threads");
    let Ok(pool) = created else { unreachable!("asserted above") };
    let capacity = usize_from_u32(BATCH_ITEM_COUNT);
    Mapping {
        pool,
        items: Vec::with_capacity(capacity),
        outputs: Vec::with_capacity(capacity),
        diagnostics: Vec::with_capacity(usize_from_u32(report_count)),
    }
}

/// Maps batch after batch on the pool made during initialization, commits the diagnostics of
/// each in order and takes them out, which must not allocate: the items, the outputs and the
/// diagnostics all reuse the buffers made at startup.
fn main_steady_state_mapping(mapping: &mut Mapping) {
    let context = *mapping.pool.context();
    for round in 0..BATCH_COUNT {
        mapping.items.extend(round..round + BATCH_ITEM_COUNT);
        mapping.pool.map(&mut mapping.items, &mut mapping.outputs);
        let mut expected = round;
        for (square, batch) in mapping.outputs.drain(..) {
            assert_eq!(square, u64::from(expected) * u64::from(expected), "outputs in order");
            batch.commit();
            expected += 1;
        }
        assert_eq!(expected, round + BATCH_ITEM_COUNT, "one output per item");
        context.take(&mut mapping.diagnostics);
        let reports = mapping.diagnostics.iter().all(|item| item.remainder(REPORT_PERIOD) == 0);
        assert!(reports, "only the multiples are reported");
        let ordered = mapping.diagnostics.is_sorted();
        assert!(ordered, "the diagnostics come in input order");
        mapping.diagnostics.clear();
    }
}

/// The global allocator of this binary, sealed once initialization is over.
#[global_allocator]
static ALLOCATOR: StaticAllocator<Allocator> = StaticAllocator::new(BACKEND);

/// The number of items the queue holds at most, fixed before initialization.
const QUEUE_CAPACITY: u32 = 64;

/// The number of steady-state rounds, each filling and draining the queue once.
const ROUND_COUNT: u32 = 1_000;

/// The environment variable that makes the binary allocate after sealing.
const ALLOCATE_AFTER_SEAL: &str = "VULCAN_ALLOCATE_AFTER_SEAL";

/// Fills and drains the queue made during initialization, round after round, which must not
/// allocate: a full queue refuses an item instead of growing.
fn main_steady_state(queue: &mut BoundedQueue<u64>) {
    let mut checksum = 0_u64;
    for round in 0..ROUND_COUNT {
        for item in 0..QUEUE_CAPACITY {
            let pushed = queue.push_back(u64::from(round + item));
            assert!(pushed.is_ok(), "the queue has room for `QUEUE_CAPACITY` items");
        }
        assert!(queue.push_back(0).is_err(), "a full queue refuses instead of growing");
        for _ in 0..QUEUE_CAPACITY {
            let popped = queue.pop_front();
            assert!(popped.is_some(), "every pushed item comes back");
            checksum += popped.map_or(0, |value| value);
        }
        assert!(queue.is_empty(), "a round drains the queue");
    }
    assert!(checksum > 0, "the steady state did work");
}

/// Seals the allocator, then allocates, which must abort the process before this returns.
fn main_allocate_after_seal() {
    ALLOCATOR.seal();
    assert!(ALLOCATOR.is_sealed(), "the child sealed its allocator");
    let allocated: Vec<u8> = Vec::with_capacity(16);
    // Reached only if the allocator failed to abort: the parent sees a success and fails.
    assert!(allocated.capacity() >= 16, "the allocation happened, which it must not");
}

/// Runs this binary with `ALLOCATE_AFTER_SEAL` set and checks that it did not succeed.
fn main_check_child_aborts() {
    if cfg!(miri) {
        // Miri cannot spawn processes, so only the parent's steady state runs under it.
        assert!(!ALLOCATOR.is_sealed(), "the check runs before the seal");
    } else {
        let executable = env::current_exe();
        assert!(executable.is_ok(), "the test binary knows its own path");
        let status =
            executable.and_then(|path| Command::new(path).env(ALLOCATE_AFTER_SEAL, "1").status());
        assert!(status.is_ok(), "the child process starts");
        let succeeded = status.is_ok_and(|exit| exit.success());
        assert!(!succeeded, "an allocation after the seal aborts the process");
    }
}
