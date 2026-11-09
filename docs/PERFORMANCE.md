# Performance

Vulcan exists to be fast. This document lists the performance building blocks of the crate and
the coding conventions that go with them, all built on allocation, layout and parallelism
discipline.

| #  | Pattern                                                 | Module / file       |
| -- | ------------------------------------------------------- | ------------------- |
| 1  | Optional global allocator (mimalloc, jemalloc)          | `vulcan::alloc`     |
| 2  | Bump arenas, one per thread                             | `vulcan::arena`     |
| 3  | Niche-optimized index newtypes and `IndexVec`           | `vulcan::index`     |
| 4  | Interning (`Symbol`, pointer-identity `Interned`)       | `vulcan::intern`    |
| 5  | Compile-time type size assertions                       | `vulcan::layout`    |
| 6  | Branch hints and cold-path outlining                    | `vulcan::hint`      |
| 7  | Lookup-table byte classification, `memchr` scans        | `vulcan::bytes`     |
| 8  | Opt-in rayon scope and ordered `par_try_map`            | `vulcan::sync`      |
| 9  | Instruction-count benchmarks with CodSpeed              | `vulcan/benches`    |
| 10 | Nightly-only fast paths behind `--cfg vulcan_nightly`   | `vulcan::hint`      |
| 11 | Size thresholds before going parallel                   | `vulcan::sync`      |
| 12 | Per-thread fold, then sharded lock-free merge           | `vulcan::sync`      |
| 13 | Lazily built, named rayon pool; no pool for 1 thread    | `vulcan::sync`      |
| 14 | Diagnostics buffered per task, committed in order       | `vulcan::diag`      |
| 15 | `ErrorGuaranteed` zero-sized error proof                | `vulcan::diag`      |
| 16 | Caches picked by key type (dense slots for indices)     | `vulcan::cache`     |
| 17 | `collect_and_apply`: 0 to 8 items without the heap      | `vulcan::collect`   |
| 18 | `Never` on stable, infallible `ControlFlow`/`Result`    | `vulcan::never`     |
| 19 | `Span` aligned to 4 bytes (`packed(4)`)                 | `vulcan::layout`    |
| 20 | `symbols!` pre-interned constants                       | `vulcan::intern`    |
| 21 | Slice and flagged interners over an arena               | `vulcan::intern`    |
| 22 | `DenseBitSet` over typed indices, inline up to 128 bits | `vulcan::bit_set`   |
| 23 | Recursion bounded by a depth counter                    | `vulcan::recursion` |

## Principles

- **Allocate less, free in bulk.** Short-lived graphs live in arenas and are dropped at once.
- **Small types.** Every hot type has its size pinned at compile time; growth is a review event.
- **Integers over pointers.** Indices and symbols are `u32` newtypes with a niche, so
  `Option<Id>` costs nothing.
- **Keep the hot path hot.** Rare branches are marked cold and moved out of line.
- **Parallelism is opt-in and deterministic.** Results come back in input order, and the
  single-threaded path pays no rayon overhead.
- **Measure instructions, not wall time.** CI benchmarks must be stable enough to gate PRs.

## Coding conventions

These are rules for every hot path, not modules. Reviewers (human or AI) should enforce them.

- **Inline check, cold body.** Keep the common case a single inlined test and move the rest to a
  separate `#[cold]` function:

  ```rust
  #[inline]
  fn docs(&mut self) -> Docs {
      if self.pending.is_empty() { Docs::EMPTY } else { self.take_docs_slow() }
  }

  #[cold]
  fn take_docs_slow(&mut self) -> Docs { /* ... */ }
  ```

- **`#[inline(never)]` against recursion frames.** In recursive code (parsers, tree walks), a
  rare case with large locals inflates the stack frame of every recursion level. Move it to an
  `#[inline(never)]` function and say why: "kept out of line so that its locals take no space in
  other cases".
- **Nothing is formatted before the error branch.** No `format!`, `to_string` or diagnostic
  building on the happy path: build messages after `hint::cold_path()`, inside `outline(..)`, or
  in a `#[cold]` function. Prefer `Cow<'static, str>` messages so static text never allocates.
- **Keep `Result` small.** Box large error payloads (`Result<T, Box<Error>>`), or return
  `ErrorGuaranteed` once the error has been reported.
- **Bound recursion with a counter**, not with stack growth crates.
- **Reuse buffers.** Prefer `clear()` and `mem::swap` with a scratch buffer over allocating a new
  `Vec` per iteration; preallocate with ratios you have measured.
- **Parallel only past a threshold.** Use the helpers of `vulcan::sync`, which stay serial below
  `PAR_MIN_LEN`; never call `par_iter` on input whose size you do not know.
- **Deterministic output.** Parallel tasks never emit diagnostics directly; they use
  `DiagCtxt::par_map` or a `DiagBatch`.
- **Copy values in caches.** A cache hit returns a `Copy` value or an arena reference, never a
  clone or an `Arc`.
- **No owning fields in hot nodes.** No `String`, `Vec`, `Rc`, `Arc` or `Box<dyn _>` inside AST/IR
  nodes, and no `Vec<Vec<T>>`: names are `Symbol`s, lists are arena slices, children are
  `ArenaBox`es or indices.
- **Split kind and wrapper, box rare large variants.** `Expr { span, kind: ExprKind }`, and a
  variant much larger than the others goes behind an `ArenaBox` so it does not inflate every value
  of the enum. Pin the result with `static_assert_size!`.
- **Size `SmallVec`s from measurements.** Choose the inline capacity from observed percentiles
  and write them in a comment (for example "p80: 1, p95: 4"), instead of guessing.
- **Special-case the common small sizes.** Handle 0, 1 and 2 elements (or lookahead distances)
  explicitly when they dominate, so constants fold and no buffer is built.
- **Justify every atomic ordering, and avoid `SeqCst`.** State in a comment why `Relaxed`,
  `Acquire`/`Release` or `AcqRel` is enough; `SeqCst` is almost never needed.
- **Keep reference counts thread-local.** On hot paths prefer `Rc` confined to one task (moved in
  and out of parallel work as owned values) over `Arc`, whose atomic increments contend.
- **`dyn` only on cold paths.** Hot code takes `impl Fn`/generics; `&mut dyn FnMut` and
  `Box<dyn _>` are for callbacks that run rarely, where code size matters more than dispatch.
- **Compile tracing out, do not test a flag.** Instrumentation of hot loops sits behind `cfg`
  (or `tracing`'s `release_max_level_*` features), not behind a runtime boolean.
- **Build searchers once.** Fixed needles use a `StaticFinder`; regexes and other compiled
  matchers live in a `static` or a struct field, never in the loop.

## Deliberate constraints

- Vulcan keeps `#![forbid(unsafe_code)]`. Thin slices and unchecked index constructors are left
  out; every pattern here is implemented in safe Rust.
- Nightly fast paths use a `--cfg` flag rather than a Cargo feature, so CI jobs that build with
  `--all-features` on stable keep working.
