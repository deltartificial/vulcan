# Performance roadmap

Vulcan exists to be fast. This roadmap tracks the performance patterns we port from
[Solar](https://github.com/paradigmxyz/solar), Paradigm's Solidity compiler, which outruns `solc`
by an order of magnitude largely through allocation, layout and parallelism discipline.

Each item lands as its own pull request stacked on this branch.

| #  | Pattern                                               | Module / file    |
| -- | ----------------------------------------------------- | ---------------- |
| 1  | Optional global allocator (mimalloc, jemalloc)        | `vulcan::alloc`  |
| 2  | Bump arenas, one per thread                           | `vulcan::arena`  |
| 3  | Niche-optimized index newtypes and `IndexVec`         | `vulcan::index`  |
| 4  | Interning (`Symbol`, pointer-identity `Interned`)     | `vulcan::intern` |
| 5  | Compile-time type size assertions                     | `vulcan::layout` |
| 6  | Branch hints and cold-path outlining                  | `vulcan::hint`   |
| 7  | Lookup-table byte classification, `memchr` scans      | `vulcan::bytes`  |
| 8  | Opt-in rayon scope and ordered `par_try_map`          | `vulcan::sync`   |
| 9  | Instruction-count benchmarks with CodSpeed            | `vulcan/benches` |
| 10 | Nightly-only fast paths behind `--cfg vulcan_nightly` | `vulcan::hint`   |

## Principles

- **Allocate less, free in bulk.** Short-lived graphs live in arenas and are dropped at once.
- **Small types.** Every hot type has its size pinned at compile time; growth is a review event.
- **Integers over pointers.** Indices and symbols are `u32` newtypes with a niche, so
  `Option<Id>` costs nothing.
- **Keep the hot path hot.** Rare branches are marked cold and moved out of line.
- **Parallelism is opt-in and deterministic.** Results come back in input order, and the
  single-threaded path pays no rayon overhead.
- **Measure instructions, not wall time.** CI benchmarks must be stable enough to gate PRs.

## Deliberate differences from Solar

- Vulcan keeps `#![forbid(unsafe_code)]`. Solar's `ThinSlice` and unchecked index constructors
  are not ported; every pattern here is implemented in safe Rust.
- Nightly fast paths use a `--cfg` flag rather than a Cargo feature, so CI jobs that build with
  `--all-features` on stable keep working.
