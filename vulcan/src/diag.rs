//! Diagnostics: a zero-cost proof that an error was reported, and deterministic collection from
//! parallel tasks.
//!
//! Parallel tasks never write to the shared diagnostic list while they work. Each task fills its
//! own [`DiagBatch`], and the batches are committed in input order once the tasks are done, so the
//! output is byte-for-byte identical to a sequential run whatever the thread interleaving. The
//! error count is updated immediately, so [`DiagCtxt::has_errors`] stays accurate mid-run.
//!
//! ```
//! use vulcan::diag::DiagCtxt;
//!
//! let dcx = DiagCtxt::new();
//! let parsed = dcx.par_map(true, vec!["1", "x", "3", "y"], |s, diags| {
//!     s.parse::<u32>().map_err(|_| diags.error(format!("invalid number `{s}`"))).ok()
//! });
//! assert_eq!(parsed, [Some(1), None, Some(3), None]);
//! assert_eq!(dcx.take(), ["invalid number `x`", "invalid number `y`"]);
//! assert!(dcx.has_errors().is_some());
//! ```

use crate::sync::par_map;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Proof that an error diagnostic was emitted.
///
/// Zero-sized, so carrying it in `Result<T, ErrorGuaranteed>` or in an error variant costs
/// nothing, yet it can only be obtained by reporting an error through [`DiagCtxt`] or
/// [`DiagBatch`]. Code that receives one may skip re-reporting and fail silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ErrorGuaranteed(());

/// Collects diagnostics of type `D` from one or many threads.
#[derive(Debug, Default)]
pub struct DiagCtxt<D> {
    emitted: Mutex<Vec<D>>,
    errors: AtomicUsize,
}

impl<D> DiagCtxt<D> {
    /// Creates an empty context.
    #[must_use]
    pub const fn new() -> Self {
        Self { emitted: Mutex::new(Vec::new()), errors: AtomicUsize::new(0) }
    }

    /// Emits an error and returns the proof that it was reported.
    pub fn error(&self, diag: D) -> ErrorGuaranteed {
        self.emitted.lock().push(diag);
        self.count_error()
    }

    /// Emits a non-error diagnostic (warning, note).
    pub fn warn(&self, diag: D) {
        self.emitted.lock().push(diag);
    }

    /// Returns the number of errors reported so far, including those in uncommitted batches.
    #[must_use]
    pub fn error_count(&self) -> usize {
        // A count, not a synchronization point: callers that need every task's errors read it
        // after joining the tasks, which already orders the accesses.
        self.errors.load(Ordering::Relaxed)
    }

    /// Returns a proof if at least one error was reported.
    #[must_use]
    pub fn has_errors(&self) -> Option<ErrorGuaranteed> {
        (self.error_count() > 0).then_some(ErrorGuaranteed(()))
    }

    /// Starts a batch whose diagnostics are added to this context when it is committed or
    /// dropped.
    #[must_use]
    pub const fn batch(&self) -> DiagBatch<'_, D> {
        DiagBatch { dcx: self, diags: Vec::new() }
    }

    /// Removes and returns every committed diagnostic, in emission order.
    #[must_use]
    pub fn take(&self) -> Vec<D> {
        std::mem::take(&mut *self.emitted.lock())
    }

    fn count_error(&self) -> ErrorGuaranteed {
        self.errors.fetch_add(1, Ordering::Relaxed);
        ErrorGuaranteed(())
    }
}

impl<D: Send> DiagCtxt<D> {
    /// Maps `f` over `items`, possibly in parallel, giving each call its own [`DiagBatch`].
    ///
    /// Results come back in input order, and the batches are committed in input order, so the
    /// diagnostics are identical to a sequential run. Parallelism follows
    /// [`par_map`]: off when `!enabled` or for short inputs.
    pub fn par_map<T, R, F>(&self, enabled: bool, items: Vec<T>, f: F) -> Vec<R>
    where
        T: Send,
        R: Send,
        F: Fn(T, &mut DiagBatch<'_, D>) -> R + Sync,
    {
        let pairs = par_map(enabled, items, |item| {
            let mut batch = self.batch();
            let result = f(item, &mut batch);
            (result, batch)
        });
        pairs
            .into_iter()
            .map(|(result, batch)| {
                batch.commit();
                result
            })
            .collect()
    }
}

/// Diagnostics buffered by one task, committed to their [`DiagCtxt`] as a block.
///
/// Dropping a batch commits it, so diagnostics are not lost if a task panics.
#[derive(Debug)]
pub struct DiagBatch<'a, D> {
    dcx: &'a DiagCtxt<D>,
    diags: Vec<D>,
}

impl<D> DiagBatch<'_, D> {
    /// Buffers an error. The context's error count is updated immediately.
    pub fn error(&mut self, diag: D) -> ErrorGuaranteed {
        self.diags.push(diag);
        self.dcx.count_error()
    }

    /// Buffers a non-error diagnostic.
    pub fn warn(&mut self, diag: D) {
        self.diags.push(diag);
    }

    /// Returns the number of buffered diagnostics.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.diags.len()
    }

    /// Returns `true` if nothing is buffered.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.diags.is_empty()
    }

    /// Appends the buffered diagnostics to the context, taking its lock once.
    pub fn commit(self) {
        // The work happens in `Drop`, which also covers batches dropped during a panic.
        let _committed = self;
    }
}

impl<D> Drop for DiagBatch<'_, D> {
    fn drop(&mut self) {
        if !self.diags.is_empty() {
            self.dcx.emitted.lock().append(&mut self.diags);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODES: &[bool] = if cfg!(miri) { &[false] } else { &[false, true] };

    #[test]
    fn error_guaranteed_is_free() {
        assert_eq!(size_of::<ErrorGuaranteed>(), 0);
        assert_eq!(size_of::<Result<u32, ErrorGuaranteed>>(), 8);
    }

    #[test]
    fn par_map_keeps_diagnostic_order() {
        for &enabled in MODES {
            let dcx = DiagCtxt::new();
            let items: Vec<u32> = (0..500).collect();
            let out = dcx.par_map(enabled, items, |n, diags| {
                if n % 100 == 0 {
                    diags.warn(format!("w{n}"));
                }
                if n % 250 == 0 {
                    let _proof = diags.error(format!("e{n}"));
                }
                n
            });
            assert_eq!(out.len(), 500);
            assert_eq!(dcx.error_count(), 2);
            assert_eq!(dcx.take(), ["w0", "e0", "w100", "w200", "e250", "w300", "w400"]);
        }
    }
}
