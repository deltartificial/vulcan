//! Global allocator selection.
//!
//! A library cannot install the global allocator on behalf of its users, so this module only
//! resolves [`Allocator`] to the fastest backend enabled through Cargo features. The final binary
//! installs it:
//!
//! ```
//! #[global_allocator]
//! static ALLOC: vulcan::alloc::Allocator = vulcan::alloc::new_allocator();
//!
//! fn main() {
//!     let words = vec![1_u64; 1024];
//!     assert_eq!(words.len(), 1024);
//! }
//! ```
//!
//! Allocation-heavy workloads (parsers, graph builders) commonly gain 5 to 20 percent from
//! `mimalloc` or `jemalloc` over the system allocator, which is why Solar enables `mimalloc` by
//! default in its binary.
//!
//! Precedence when several features are enabled: `jemalloc`, then `mimalloc`, then
//! [`System`](std::alloc::System). `jemalloc` is ignored on MSVC targets.

#[cfg(all(feature = "jemalloc", not(target_env = "msvc")))]
mod backend {
    /// The selected global allocator.
    pub type Allocator = tikv_jemallocator::Jemalloc;

    pub(super) const fn new() -> Allocator {
        tikv_jemallocator::Jemalloc
    }
}

#[cfg(all(feature = "mimalloc", not(all(feature = "jemalloc", not(target_env = "msvc")))))]
mod backend {
    /// The selected global allocator.
    pub type Allocator = mimalloc::MiMalloc;

    pub(super) const fn new() -> Allocator {
        mimalloc::MiMalloc
    }
}

#[cfg(not(any(feature = "mimalloc", all(feature = "jemalloc", not(target_env = "msvc")))))]
mod backend {
    /// The selected global allocator.
    pub type Allocator = std::alloc::System;

    pub(super) const fn new() -> Allocator {
        std::alloc::System
    }
}

pub use backend::Allocator;

/// Returns the allocator selected by the enabled features.
///
/// `const` so it can initialize a `#[global_allocator]` static.
#[must_use]
pub const fn new_allocator() -> Allocator {
    backend::new()
}

/// Returns the name of the selected allocator, for logs and diagnostics.
#[must_use]
pub const fn allocator_name() -> &'static str {
    if cfg!(all(feature = "jemalloc", not(target_env = "msvc"))) {
        "jemalloc"
    } else if cfg!(feature = "mimalloc") {
        "mimalloc"
    } else {
        "system"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_matches_features() {
        let expected = if cfg!(all(feature = "jemalloc", not(target_env = "msvc"))) {
            "jemalloc"
        } else if cfg!(feature = "mimalloc") {
            "mimalloc"
        } else {
            "system"
        };
        assert_eq!(allocator_name(), expected);
    }
}
