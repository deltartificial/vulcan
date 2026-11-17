//! Performance building blocks for hot-path Rust code.
//!
//! Safe, benchmarked primitives for code where allocation, layout and parallelism decide the
//! result: compilers and parsers, trading and pricing engines, blockchain clients, low-latency
//! services. Each module is independent: keep the ones you need and delete the rest.
//!
//! The coding rules that go with them are in `docs/CODING.md` and `docs/PERFORMANCE.md`.

// Nightly-only fast paths, enabled with `RUSTFLAGS="--cfg vulcan_nightly"`. A `--cfg` flag rather
// than a Cargo feature, so `--all-features` builds keep working on stable.
#![cfg_attr(vulcan_nightly, feature(likely_unlikely))]

// `alloc` paths instead of `std` ones for heap types (`clippy::std_instead_of_alloc`).
extern crate alloc;

pub mod allocator;
pub mod arena;
pub mod bit_set;
pub mod bounded;
pub mod bytes;
pub mod cache;
pub mod collect;
pub mod convert;
pub mod diagnostic;
pub mod division;
pub mod hint;
pub mod index;
pub mod intern;
pub mod layout;
pub mod never;
pub mod sync;
