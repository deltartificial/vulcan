//! Opt-in, deterministic parallelism.
//!
//! Ported from Solar's `crates/data-structures/src/sync.rs`. Two rules shape it:
//!
//! - **Parallelism is a runtime switch.** Every helper takes `enabled`. When it is `false` the work
//!   runs inline on the caller's thread with no rayon scope, no job allocation and no
//!   synchronization, so the single-threaded path pays nothing.
//! - **Results are deterministic.** [`par_try_map`] returns outputs in input order and the first
//!   error in input order, whatever the thread interleaving, so diagnostics and outputs do not
//!   change from run to run.
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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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
/// A serial map (when `!enabled` or fewer than 2 items) stops at the first error.
pub fn par_try_map<T, R, E, F>(enabled: bool, items: Vec<T>, f: F) -> Result<Vec<R>, E>
where
    T: Send,
    R: Send,
    E: Send,
    F: Fn(T) -> Result<R, E> + Sync,
{
    let len = items.len();
    if !enabled || len < 2 {
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
}
