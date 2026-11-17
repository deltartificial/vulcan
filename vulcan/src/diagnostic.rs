//! Diagnostics: a zero-cost proof that an error was reported, and deterministic collection from
//! parallel tasks.
//!
//! Parallel tasks never write to the shared diagnostic list while they work. Each task fills its
//! own [`DiagnosticBatch`], and the batches are committed in input order once the tasks are done,
//! so the output is byte-for-byte identical to a sequential run whatever the thread interleaving.
//! The error count is updated immediately, so [`DiagnosticContext::has_errors`] stays accurate
//! mid-run.
//!
//! Every buffer has a fixed capacity, stated in [`DiagnosticLimits`]:
//!
//! - The context allocates room for `diagnostic_count_max` diagnostics when it is created, and
//!   asserts on every emit and commit that it stays within it.
//! - A batch starts without a buffer, so a task that reports nothing, the hot path, takes no lock.
//!   The first diagnostic of a batch borrows one of the `batch_buffer_count` buffers that the
//!   context allocated when it was created, each with room for `batch_diagnostic_count_max`
//!   diagnostics, and every later one asserts that it fits: a batch never grows, and nothing is
//!   allocated after the context is created. The buffer goes back to the context when the batch is
//!   committed.
//!
//! To keep that order with a [`WorkerPool`](crate::sync::WorkerPool), give its function a context
//! that lives as long as the program, start a batch in each call and return it with the output:
//! the outputs come back in input order, and so do the batches, ready to be committed one after
//! the other.
//!
//! ```
//! use vulcan::diagnostic::{DiagnosticContext, DiagnosticLimits};
//!
//! let limits = DiagnosticLimits {
//!     diagnostic_count_max: 8,
//!     batch_buffer_count: 4,
//!     batch_diagnostic_count_max: 2,
//! };
//! let context = DiagnosticContext::new(limits);
//! let parsed: Vec<Option<u32>> = ["1", "x", "3", "y"]
//!     .into_iter()
//!     .map(|text| {
//!         let mut batch = context.batch();
//!         let number =
//!             text.parse().map_err(|_| batch.error(format!("invalid number `{text}`"))).ok();
//!         batch.commit();
//!         number
//!     })
//!     .collect();
//! assert_eq!(parsed, [Some(1), None, Some(3), None]);
//! let mut diagnostics = Vec::with_capacity(8);
//! context.take(&mut diagnostics);
//! assert_eq!(diagnostics, ["invalid number `x`", "invalid number `y`"]);
//! assert!(context.has_errors().is_some());
//! ```

use crate::convert::{u32_from_usize, usize_from_u32};
use core::{
    mem,
    sync::atomic::{AtomicU32, Ordering},
};
use parking_lot::Mutex;

/// Proof that an error diagnostic was emitted.
///
/// Zero-sized, so carrying it in `Result<Item, ErrorGuaranteed>` or in an error variant costs
/// nothing, yet it can only be obtained by reporting an error through [`DiagnosticContext`] or
/// a [`DiagnosticBatch`]. Code that receives one may skip re-reporting and fail silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ErrorGuaranteed(());

// The proof must stay free to carry.
const _: () = assert!(size_of::<ErrorGuaranteed>() == 0, "`ErrorGuaranteed` is zero-sized");

/// The capacities of a [`DiagnosticContext`] and of each of its batches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosticLimits {
    /// The number of committed diagnostics the context holds at most.
    pub diagnostic_count_max: u32,
    /// The number of batch buffers the context allocates: as many batches hold diagnostics at
    /// the same time, at most.
    pub batch_buffer_count: u32,
    /// The number of diagnostics one batch buffers at most.
    pub batch_diagnostic_count_max: u32,
}

/// Collects diagnostics of type `Diagnostic` from one or many threads.
#[derive(Debug)]
pub struct DiagnosticContext<Diagnostic> {
    /// The committed diagnostics, in emission order, allocated at construction.
    emitted: Mutex<Vec<Diagnostic>>,
    /// The buffers no batch is using, all allocated at construction.
    batch_buffers: Mutex<Vec<Vec<Diagnostic>>>,
    /// The number of errors reported, committed or not.
    error_count: AtomicU32,
    /// The capacities of the context and of its batches.
    limits: DiagnosticLimits,
}

impl<Diagnostic> DiagnosticContext<Diagnostic> {
    /// Creates an empty context and allocates everything it will ever use: room for
    /// `limits.diagnostic_count_max` diagnostics, and `limits.batch_buffer_count` batch buffers of
    /// `limits.batch_diagnostic_count_max` diagnostics each.
    ///
    /// # Panics
    ///
    /// Panics if a limit is zero, or if a batch may hold more than the context.
    #[must_use = "creating a context has no other effect"]
    pub fn new(limits: DiagnosticLimits) -> Self {
        assert!(limits.diagnostic_count_max > 0, "a context holds at least one diagnostic");
        assert!(limits.batch_buffer_count > 0, "a context has at least one batch buffer");
        assert!(limits.batch_diagnostic_count_max > 0, "a batch holds at least one diagnostic");
        assert!(
            limits.batch_diagnostic_count_max <= limits.diagnostic_count_max,
            "a batch fits in its context"
        );
        let emitted = Vec::with_capacity(usize_from_u32(limits.diagnostic_count_max));
        assert!(emitted.capacity() >= usize_from_u32(limits.diagnostic_count_max), "allocated");
        let batch_buffers = new_batch_buffers(limits);
        Self {
            emitted: Mutex::new(emitted),
            batch_buffers: Mutex::new(batch_buffers),
            error_count: AtomicU32::new(0),
            limits,
        }
    }

    /// Emits an error and returns the proof that it was reported.
    ///
    /// # Panics
    ///
    /// Panics if the context is full.
    pub fn error(&self, diagnostic: Diagnostic) -> ErrorGuaranteed {
        self.emit(diagnostic);
        let proof = self.count_error();
        assert!(self.error_count() > 0, "the error is counted");
        proof
    }

    /// Emits a non-error diagnostic (warning, note).
    ///
    /// # Panics
    ///
    /// Panics if the context is full.
    pub fn warn(&self, diagnostic: Diagnostic) {
        self.emit(diagnostic);
    }

    /// Returns the number of errors reported so far, including those in uncommitted batches.
    #[must_use = "the count is queried, not changed"]
    pub fn error_count(&self) -> u32 {
        // Relaxed: a count, not a synchronization point. Callers that need every task's errors
        // read it after joining the tasks, which already orders the accesses.
        self.error_count.load(Ordering::Relaxed)
    }

    /// Returns a proof if at least one error was reported.
    #[must_use = "the proof is returned, not stored"]
    pub fn has_errors(&self) -> Option<ErrorGuaranteed> {
        (self.error_count() > 0).then_some(ErrorGuaranteed(()))
    }

    /// Starts a batch whose diagnostics are added to this context when it is committed or
    /// dropped. The batch borrows a buffer of the context on its first diagnostic.
    ///
    /// # Panics
    ///
    /// Never: the assertions state invariants that the constructor established.
    #[must_use = "a batch commits its diagnostics when dropped, so an unused one does nothing"]
    pub const fn batch(&self) -> DiagnosticBatch<'_, Diagnostic> {
        assert!(self.limits.batch_diagnostic_count_max > 0, "checked by `new`: a batch holds one");
        assert!(
            self.limits.batch_diagnostic_count_max <= self.limits.diagnostic_count_max,
            "checked by `new`: a batch fits in its context"
        );
        DiagnosticBatch { context: self, diagnostics: None }
    }

    /// Moves every committed diagnostic, in emission order, into `target`, and gives the context
    /// the buffer of `target` in exchange, so no memory is allocated or freed.
    ///
    /// # Panics
    ///
    /// Panics if `target` is not empty, or if it has room for fewer diagnostics than the
    /// context's capacity.
    pub fn take(&self, target: &mut Vec<Diagnostic>) {
        let capacity = usize_from_u32(self.limits.diagnostic_count_max);
        assert!(target.is_empty(), "the target of `take` is empty");
        assert!(target.capacity() >= capacity, "the target of `take` holds the context capacity");

        let mut emitted = self.emitted.lock();
        mem::swap(&mut *emitted, target);
        assert!(target.len() <= capacity, "the context held no more than its capacity");
        assert!(emitted.is_empty(), "the context keeps an empty buffer");
        assert!(emitted.capacity() >= capacity, "the context keeps its capacity");
    }

    /// Appends one diagnostic to the committed list.
    fn emit(&self, diagnostic: Diagnostic) {
        let capacity = usize_from_u32(self.limits.diagnostic_count_max);

        let mut emitted = self.emitted.lock();
        let diagnostic_count_before = emitted.len();
        assert!(diagnostic_count_before < capacity, "a `DiagnosticContext` is full");
        emitted.push(diagnostic);
        assert_eq!(emitted.len(), diagnostic_count_before + 1, "one diagnostic is emitted");
        assert!(emitted.capacity() >= capacity, "a diagnostic context never reallocates");
    }

    /// Counts one error and returns its proof.
    fn count_error(&self) -> ErrorGuaranteed {
        // Relaxed: see `error_count`.
        let error_count_before = self.error_count.fetch_add(1, Ordering::Relaxed);
        assert!(error_count_before < u32::MAX, "error count overflow");
        // The counter only grows, and a thread sees its own increment.
        assert!(self.error_count() > error_count_before, "the error is counted");
        ErrorGuaranteed(())
    }
}

/// Diagnostics buffered by one task, committed to their [`DiagnosticContext`] as a block.
///
/// Dropping a batch commits it, so diagnostics are not lost if a task panics.
#[derive(Debug)]
pub struct DiagnosticBatch<'context, Diagnostic> {
    /// The context the diagnostics are committed to.
    context: &'context DiagnosticContext<Diagnostic>,
    /// The buffered diagnostics, in a buffer of the context: none until the first one.
    diagnostics: Option<Vec<Diagnostic>>,
}

impl<Diagnostic> DiagnosticBatch<'_, Diagnostic> {
    /// Buffers an error. The context's error count is updated immediately.
    ///
    /// # Panics
    ///
    /// Panics if the batch is full.
    pub fn error(&mut self, diagnostic: Diagnostic) -> ErrorGuaranteed {
        self.buffer(diagnostic);
        assert!(!self.is_empty(), "the error is buffered");
        let proof = self.context.count_error();
        assert!(self.context.error_count() > 0, "the error is counted before its commit");
        proof
    }

    /// Buffers a non-error diagnostic.
    ///
    /// # Panics
    ///
    /// Panics if the batch is full.
    pub fn warn(&mut self, diagnostic: Diagnostic) {
        self.buffer(diagnostic);
        assert!(!self.is_empty(), "the warning is buffered");
    }

    /// Returns the number of buffered diagnostics.
    ///
    /// # Panics
    ///
    /// Never: the assertion states that the batch stays within its capacity.
    #[must_use = "the length is queried, not changed"]
    pub fn len(&self) -> u32 {
        let diagnostic_count = u32_from_usize(self.diagnostics.as_ref().map_or(0, Vec::len));
        assert!(
            diagnostic_count <= self.context.limits.batch_diagnostic_count_max,
            "a batch stays within its capacity"
        );
        diagnostic_count
    }

    /// Returns `true` if nothing is buffered.
    #[must_use = "the length is queried, not changed"]
    pub const fn is_empty(&self) -> bool {
        match &self.diagnostics {
            Some(diagnostics) => diagnostics.is_empty(),
            None => true,
        }
    }

    /// Appends the buffered diagnostics to the context, taking its lock once.
    ///
    /// # Panics
    ///
    /// Panics if the context has no room for the buffered diagnostics.
    pub fn commit(self) {
        assert!(
            self.len() <= self.context.limits.diagnostic_count_max,
            "a batch fits in its context"
        );
        // The work happens in `Drop`, which also covers batches dropped during a panic.
        let _committed = self;
    }

    /// Appends one diagnostic to the buffer, borrowing the buffer on the first one.
    ///
    /// # Panics
    ///
    /// Panics if the batch is full, or if `batch_buffer_count` other batches hold diagnostics.
    fn buffer(&mut self, diagnostic: Diagnostic) {
        let capacity = usize_from_u32(self.context.limits.batch_diagnostic_count_max);
        let diagnostics = if let Some(diagnostics) = &mut self.diagnostics {
            diagnostics
        } else {
            let Some(buffer) = self.context.batch_buffers.lock().pop() else {
                unreachable!("every batch buffer of the context is in use");
            };
            assert!(buffer.is_empty(), "a lent buffer is empty");
            self.diagnostics.insert(buffer)
        };
        let diagnostic_count_before = diagnostics.len();
        assert!(diagnostic_count_before < capacity, "a `DiagnosticBatch` is full");
        diagnostics.push(diagnostic);
        assert_eq!(diagnostics.len(), diagnostic_count_before + 1, "one is buffered");
        assert!(diagnostics.capacity() >= capacity, "a batch never reallocates");
    }
}

/// Allocates the batch buffers of a context, once, when the context is created.
fn new_batch_buffers<Diagnostic>(limits: DiagnosticLimits) -> Vec<Vec<Diagnostic>> {
    let buffer_count = usize_from_u32(limits.batch_buffer_count);
    let capacity = usize_from_u32(limits.batch_diagnostic_count_max);
    let mut batch_buffers = Vec::with_capacity(buffer_count);
    for _ in 0..buffer_count {
        batch_buffers.push(Vec::with_capacity(capacity));
    }
    assert_eq!(batch_buffers.len(), buffer_count, "one buffer per batch");
    assert!(batch_buffers.iter().all(|buffer| buffer.capacity() >= capacity), "allocated");
    batch_buffers
}

impl<Diagnostic> Drop for DiagnosticBatch<'_, Diagnostic> {
    fn drop(&mut self) {
        let capacity = usize_from_u32(self.context.limits.diagnostic_count_max);
        // Reading the length checks that the batch stayed within its capacity.
        let diagnostic_count = usize_from_u32(self.len());
        if let Some(mut diagnostics) = self.diagnostics.take() {
            {
                let mut emitted = self.context.emitted.lock();
                let diagnostic_count_before = emitted.len();
                assert!(
                    diagnostic_count_before + diagnostic_count <= capacity,
                    "a `DiagnosticContext` is full"
                );
                emitted.append(&mut diagnostics);
                assert_eq!(
                    emitted.len(),
                    diagnostic_count_before + diagnostic_count,
                    "every buffered diagnostic is appended"
                );
                assert!(emitted.capacity() >= capacity, "a diagnostic context never reallocates");
            }

            assert!(diagnostics.is_empty(), "a returned buffer is empty");
            let mut batch_buffers = self.context.batch_buffers.lock();
            let buffer_count_before = batch_buffers.len();
            assert!(
                buffer_count_before < usize_from_u32(self.context.limits.batch_buffer_count),
                "a buffer is returned once"
            );
            batch_buffers.push(diagnostics);
            assert_eq!(batch_buffers.len(), buffer_count_before + 1, "one buffer is returned");
        }
        assert!(self.diagnostics.is_none(), "every buffered diagnostic is committed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::{PARALLEL_LEN_MIN, PoolConfig, ThreadCount, Work, WorkerPool};
    use core::num::NonZeroU32;
    use std::sync::OnceLock;

    /// Limits for most tests: room for eight diagnostics, two per batch.
    const LIMITS: DiagnosticLimits = DiagnosticLimits {
        diagnostic_count_max: 8,
        batch_buffer_count: 8,
        batch_diagnostic_count_max: 2,
    };

    /// Returns every committed diagnostic of `context`.
    fn take_all<Diagnostic>(context: &DiagnosticContext<Diagnostic>) -> Vec<Diagnostic> {
        let mut target = Vec::with_capacity(usize_from_u32(context.limits.diagnostic_count_max));
        context.take(&mut target);
        target
    }

    /// Checks the size of a result that carries the proof: the proof adds nothing.
    #[test]
    fn error_guaranteed_is_free() {
        assert_eq!(size_of::<Result<u32, ErrorGuaranteed>>(), 8);
    }

    /// Warns on multiples of 100 and fails on multiples of 250.
    fn report_round_numbers(value: u32, batch: &mut DiagnosticBatch<'_, String>) -> u32 {
        if value.is_multiple_of(100) {
            batch.warn(format!("w{value}"));
        }
        if value.is_multiple_of(250) {
            let _proof = batch.error(format!("e{value}"));
        }
        value
    }

    /// Returns `value` with the batch of what it reported.
    #[expect(clippy::trivially_copy_pass_by_ref, reason = "the signature of a pool function")]
    fn report_in_batch(
        context: &&'static DiagnosticContext<String>,
        value: u32,
    ) -> (u32, DiagnosticBatch<'static, String>) {
        let mut batch = context.batch();
        let reported = report_round_numbers(value, &mut batch);
        (reported, batch)
    }

    /// The context of the pool test: a static, since the workers read it for as long as their
    /// pool lives.
    static POOL_CONTEXT: OnceLock<DiagnosticContext<String>> = OnceLock::new();

    /// Maps 500 values on one thread and on four with a `WorkerPool` whose function returns a
    /// batch per value, commits the batches in output order, and compares the diagnostics with
    /// the sequential order.
    #[test]
    fn a_worker_pool_keeps_the_diagnostic_order() {
        let context = POOL_CONTEXT.get_or_init(|| DiagnosticContext::new(LIMITS));
        for (round, thread_count) in (1_u32..).zip([1_u32, 4]) {
            let config = PoolConfig {
                thread_count: ThreadCount::Fixed(NonZeroU32::MIN.saturating_add(thread_count - 1)),
                stack_size_bytes: 2 * 1024 * 1024,
                name_prefix: "diagnostic",
                batch_item_count_max: 500,
                parallel_item_count_min: PARALLEL_LEN_MIN,
            };
            let work = Work { context, map: report_in_batch, start_handler: None };
            let mut pool = WorkerPool::new(&config, work).unwrap();
            let mut items: Vec<u32> = (0..500).collect();
            let mut outputs = Vec::with_capacity(500);
            pool.map(&mut items, &mut outputs);
            assert_eq!(outputs.len(), 500);
            for (_, batch) in outputs {
                batch.commit();
            }
            assert_eq!(context.error_count(), 2 * round, "two errors per round");
            assert_eq!(take_all(context), ["w0", "e0", "w100", "w200", "e250", "w300", "w400"]);
        }
    }

    /// Emits warnings directly and through a batch, then an error: warnings alone report no
    /// error, and the first error flips the state.
    #[test]
    fn warnings_alone_report_no_error() {
        let context = DiagnosticContext::new(LIMITS);
        context.warn("note");
        let mut batch = context.batch();
        assert!(batch.is_empty(), "a new batch is empty");
        batch.warn("buffered");
        assert_eq!(batch.len(), 1);
        assert!(context.has_errors().is_none(), "warnings are not errors");
        batch.commit();
        assert_eq!(take_all(&context), ["note", "buffered"]);
        let _proof = context.error("fatal");
        assert!(context.has_errors().is_some(), "the first error flips the state");
        assert!(context.batch().is_empty());
    }

    /// Fills the context to its capacity, takes the diagnostics, and fills it again: `take`
    /// hands the context a buffer with the full capacity.
    #[test]
    fn context_fills_to_capacity_and_refills_after_take() {
        let context = DiagnosticContext::new(LIMITS);
        for round in 0..2_u32 {
            for value in 0..LIMITS.diagnostic_count_max {
                context.warn(value + round);
            }
            let taken = take_all(&context);
            assert_eq!(u32_from_usize(taken.len()), LIMITS.diagnostic_count_max);
            assert_eq!(taken.first(), Some(&round));
        }
        assert!(take_all(&context).is_empty(), "a second take finds nothing");
    }

    /// Emits one diagnostic above the context capacity, which must crash.
    #[test]
    #[should_panic(expected = "a `DiagnosticContext` is full")]
    fn context_rejects_diagnostics_above_capacity() {
        let context = DiagnosticContext::new(LIMITS);
        for value in 0..=LIMITS.diagnostic_count_max {
            context.warn(value);
        }
    }

    /// Commits a batch that would take the context past its capacity, which must crash.
    #[test]
    #[should_panic(expected = "a `DiagnosticContext` is full")]
    fn commit_rejects_diagnostics_above_capacity() {
        let context = DiagnosticContext::new(LIMITS);
        for value in 0..LIMITS.diagnostic_count_max - 1 {
            context.warn(value);
        }
        let mut batch = context.batch();
        batch.warn(100);
        batch.warn(101);
        batch.commit();
    }

    /// Fills a batch to its capacity, then commits it: a full batch is accepted.
    #[test]
    fn batch_fills_to_capacity() {
        let context = DiagnosticContext::new(LIMITS);
        let mut batch = context.batch();
        for value in 0..LIMITS.batch_diagnostic_count_max {
            batch.warn(value);
        }
        assert_eq!(batch.len(), LIMITS.batch_diagnostic_count_max);
        batch.commit();
        assert_eq!(take_all(&context), [0, 1]);
    }

    /// Buffers one diagnostic above the batch capacity, which must crash.
    #[test]
    #[should_panic(expected = "a `DiagnosticBatch` is full")]
    fn batch_rejects_diagnostics_above_capacity() {
        let context = DiagnosticContext::new(LIMITS);
        let mut batch = context.batch();
        for value in 0..=LIMITS.batch_diagnostic_count_max {
            batch.warn(value);
        }
    }

    /// Holds diagnostics in as many batches as the context has buffers, commits them and does it
    /// again: a committed batch hands its buffer back, so the buffers are reused, never
    /// reallocated.
    #[test]
    fn batch_buffers_are_reused_after_a_commit() {
        let limits = DiagnosticLimits { diagnostic_count_max: 8, batch_buffer_count: 2, ..LIMITS };
        let context = DiagnosticContext::new(limits);
        for round in 0..2_u32 {
            let mut first = context.batch();
            let mut second = context.batch();
            let idle = context.batch();
            first.warn(round);
            second.warn(round + 10);
            assert!(idle.is_empty(), "a batch without diagnostics holds no buffer");
            first.commit();
            second.commit();
        }
        assert_eq!(take_all(&context), [0, 10, 1, 11]);
    }

    /// Buffers a diagnostic in one batch more than the context has buffers, which must crash.
    #[test]
    #[should_panic(expected = "every batch buffer of the context is in use")]
    fn context_rejects_batches_above_its_buffer_count() {
        let limits = DiagnosticLimits { diagnostic_count_max: 8, batch_buffer_count: 1, ..LIMITS };
        let context = DiagnosticContext::new(limits);
        let mut first = context.batch();
        let mut second = context.batch();
        first.warn(1_u32);
        second.warn(2_u32);
    }

    /// Creates a context without a batch buffer, which must crash.
    #[test]
    #[should_panic(expected = "a context has at least one batch buffer")]
    fn context_rejects_a_zero_batch_count() {
        let limits = DiagnosticLimits { batch_buffer_count: 0, ..LIMITS };
        let _context = DiagnosticContext::<u8>::new(limits);
    }

    /// Creates a context whose batches hold nothing, which must crash.
    #[test]
    #[should_panic(expected = "a batch holds at least one diagnostic")]
    fn context_rejects_empty_batches() {
        let limits = DiagnosticLimits {
            diagnostic_count_max: 1,
            batch_buffer_count: 1,
            batch_diagnostic_count_max: 0,
        };
        let _context = DiagnosticContext::<u8>::new(limits);
    }

    /// Creates a context whose batches hold more than the context, which must crash.
    #[test]
    #[should_panic(expected = "a batch fits in its context")]
    fn context_rejects_batches_above_its_capacity() {
        let limits = DiagnosticLimits {
            diagnostic_count_max: 1,
            batch_buffer_count: 1,
            batch_diagnostic_count_max: 2,
        };
        let _context = DiagnosticContext::<u8>::new(limits);
    }

    /// Takes the diagnostics into a buffer smaller than the context capacity, which must crash.
    #[test]
    #[should_panic(expected = "holds the context capacity")]
    fn take_rejects_small_targets() {
        let context = DiagnosticContext::<u8>::new(LIMITS);
        let mut target = Vec::with_capacity(1);
        context.take(&mut target);
    }

    /// Takes the diagnostics into a buffer that is not empty, which must crash.
    #[test]
    #[should_panic(expected = "the target of `take` is empty")]
    fn take_rejects_full_targets() {
        let context = DiagnosticContext::<u8>::new(LIMITS);
        let mut target = Vec::with_capacity(8);
        target.push(1);
        context.take(&mut target);
    }
}
