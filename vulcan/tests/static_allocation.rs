//! Allocates during initialization, seals the global allocator, then runs a steady state that
//! must not allocate: it reuses the buffers made at startup.
//!
//! The binary runs without the test harness (`harness = false` in `Cargo.toml`), because the
//! harness allocates after every test. An allocation after `seal` aborts the process, which fails
//! the test with a non-zero exit status.
//!
//! The opposite case is checked too: before sealing, the binary runs itself with the environment
//! variable `ALLOCATE_AFTER_SEAL` set, and that child allocates after sealing, which must abort it.

use std::{env, process::Command};
use vulcan::{
    allocator::{Allocator, BACKEND, StaticAllocator},
    bounded::BoundedQueue,
};

fn main() {
    if env::var_os(ALLOCATE_AFTER_SEAL).is_some() {
        main_allocate_after_seal();
    } else {
        // Initialization: spawning the child allocates, so it happens before the seal.
        main_check_child_aborts();
        let mut queue = BoundedQueue::with_capacity(QUEUE_CAPACITY);
        ALLOCATOR.seal();
        assert!(ALLOCATOR.is_sealed(), "initialization is over");
        main_steady_state(&mut queue);
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
