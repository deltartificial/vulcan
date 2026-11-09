# Vulcan

Ultra-strict, performance-first Rust workspace template for hot-path code, built for AI-assisted development.

## Who This Is For

Vulcan is not a general-purpose starter. Use it when speed is the product:

- Compilers, parsers, interpreters and static analyzers
- Trading, pricing and other low-latency financial systems
- Blockchain clients, execution engines, indexers and provers
- Any service or library where allocation, layout and parallelism decide the result

For a CRUD service, a CLI or a prototype, the lints and conventions here cost more than they give: pick a lighter template.

## Philosophy

Traditional Rust projects balance strictness with flexibility. Vulcan enforces a single correct pattern to prevent AI from generating inconsistent code, and ships the building blocks to make that code fast.

Key principles:

- Force simpler code through aggressive complexity thresholds
- Ban error handling footguns (`unwrap`, `expect`, `panic`)
- Require fast alternatives (`parking_lot::Mutex`, `rustc_hash::FxHashMap`)
- Enforce exhaustive pattern matching and explicit error handling
- Use `tracing` instead of `println!` for logging
- Allocate less, keep types small, keep the hot path hot, parallelize deterministically
- No `unsafe`: `unsafe_code` is forbidden for the whole workspace

Target use case: AI code generation on performance-critical code, where flexibility leads to bugs, inconsistent patterns and slow code.

## Quick Start

```bash
gh repo create my-project --template deltartificial/vulcan --private --clone
```

Or click "Use this template" on GitHub.

## Features

### Strict Linting

- 110+ clippy lints enabled (nursery, pedantic, restriction groups)
- Ban `unwrap_used`, `expect_used`, `panic` in production code
- Enforce explicit clones (`Rc::clone()`, `Arc::clone()`)
- Require exhaustive pattern matching (no wildcard enum arms)
- Prevent variable shadowing

### Aggressive Complexity Thresholds

```toml
cognitive-complexity-threshold = 15 # Default: 25
type-complexity-threshold = 100 # Default: 250
too-many-arguments-threshold = 5 # Default: 7
too-many-lines-threshold = 75 # Default: 100
```

### Disallowed Types

| Banned                         | Use instead                                      |
| ------------------------------ | ------------------------------------------------ |
| `std::sync::Mutex`             | `parking_lot::Mutex` (1.5x faster, no poisoning) |
| `std::sync::RwLock`            | `parking_lot::RwLock` (5x faster)                |
| `std::collections::HashMap`    | `rustc_hash::FxHashMap` or `foldhash::HashMap`   |
| `std::collections::LinkedList` | `Vec` or `VecDeque`                              |

### Disallowed Methods and Macros

| Banned                                   | Use instead                                  |
| ---------------------------------------- | -------------------------------------------- |
| `std::env::var`                          | `std::env::var_os` (explicit error handling) |
| `Option::unwrap_or`, `Result::unwrap_or` | `map_or`, `let`-`else`, or `?`               |
| `std::mem::drop`                         | `_ = expr`, or a scope block for guards      |
| `println!`, `eprintln!`                  | `tracing::info!`, `tracing::error!`          |

## Performance

The `vulcan` crate is a toolbox of safe, benchmarked building blocks. See [docs/PERFORMANCE.md](docs/PERFORMANCE.md) for the full list and the coding conventions that go with them.

| Module      | Building blocks                                                               |
| ----------- | ----------------------------------------------------------------------------- |
| `alloc`     | Optional `mimalloc` / `jemalloc` global allocator                             |
| `arena`     | Bump arenas (one per thread), `ArenaBox`, `SliceStack`, no `Drop` in arenas   |
| `index`     | `newtype_index!` (4-byte ids, `Option<Id>` still 4 bytes) and `IndexVec`      |
| `intern`    | `Symbol` interner, `symbols!` consts, slice and flagged interners, `Interned` |
| `layout`    | `static_assert_size!` budgets, 8-byte `Span` aligned to 4                     |
| `cache`     | Caches picked by key type (dense slots for indices, no hashing)               |
| `collect`   | `collect_and_apply`: up to 8 items without touching the heap                  |
| `bit_set`   | `DenseBitSet` over typed indices, inline up to 128 bits                       |
| `bytes`     | 256-entry byte class table, `memchr` cursor, `StaticFinder`                   |
| `hint`      | `likely`, `unlikely`, `cold_path`, `outline`                                  |
| `sync`      | Size thresholds, ordered `par_map`, sharded merge, `LazyPool`, `JobCounter`   |
| `diag`      | `ErrorGuaranteed`, diagnostics buffered per task and replayed in order        |
| `never`     | `Never` on stable, `into_ok` / `into_value` for infallible results            |
| `recursion` | Recursion bounded by a depth counter                                          |

Nightly-only fast paths are enabled with `RUSTFLAGS="--cfg vulcan_nightly"` (not a feature, so `--all-features` keeps working on stable).

## Tooling

| Tool                | Purpose                                  |
| ------------------- | ---------------------------------------- |
| clippy              | Nightly with 110+ lints                  |
| rustfmt             | Nightly, `imports_granularity = "Crate"` |
| cargo-deny          | License auditing, OpenSSL banned         |
| cargo-semver-checks | API stability verification               |
| cargo-fuzz          | Fuzz target built and smoke-run in CI    |
| dprint              | TOML/Markdown/JSON formatting            |
| typos               | Spell checking                           |
| nextest             | Test runner with retries                 |
| cross               | Local cross-compilation (`Cross.toml`)   |
| miri                | Undefined behavior detection             |
| zepter              | Feature propagation across crates        |
| CodSpeed            | Instruction-count benchmarks on every PR |

## CI

### Lint Workflow

Clippy, rustfmt, documentation, typos, cargo-deny, dprint, unused dependencies, feature powerset, feature propagation, cargo-sort, wasm32-wasip1 check, miri, fuzzing, `--cfg vulcan_nightly` build. Semver checks run as advisory until the crates are published.

### Test Workflow

Nextest on Linux, macOS and Windows, doctests, MSRV check (1.93), minimal versions, AddressSanitizer.

### Bench Workflow

CodSpeed runs `vulcan/benches` in simulation mode and reports the diff against `main` on each PR.

### PR Title Enforcement

Conventional commit format required.

Build jobs use sccache and the mold linker; `lint` and `test` end with an alls-green gate job.

## Configuration

| File                  | Purpose                                            |
| --------------------- | -------------------------------------------------- |
| `Cargo.toml`          | Workspace lints, profiles, MSRV (1.93)             |
| `clippy.toml`         | Strict thresholds, disallowed types/methods/macros |
| `Cross.toml`          | Cross-compilation setup (aarch64, riscv64)         |
| `dprint.json`         | Multi-format code formatting                       |
| `typos.toml`          | Spell checking configuration                       |
| `.editorconfig`       | Editor consistency                                 |
| `deny.toml`           | License auditing                                   |
| `docs/PERFORMANCE.md` | Performance building blocks and coding conventions |

## Build Profiles

| Profile     | Use Case                                                          |
| ----------- | ----------------------------------------------------------------- |
| `dev`       | Fast iteration with optimized dependencies                        |
| `release`   | Standard optimized builds                                         |
| `maxperf`   | Distribution binary (fat LTO, codegen-units=1, `panic = "abort"`) |
| `profiling` | Release with debug symbols                                        |
| `bench`     | Benchmarking                                                      |

## Development

```bash
make lint    # Run all linters
make test    # Run all tests
make pr      # Full pre-PR check
make bench   # Run benchmarks
make maxperf-nightly # maxperf with nightly fast paths
```

Commit format: `type(scope): description`

Types: `feat`, `fix`, `chore`, `test`, `bench`, `perf`, `refactor`, `docs`, `ci`, `revert`, `deps`

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.
