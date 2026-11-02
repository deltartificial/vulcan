# Vulcan

Ultra-strict, performance-first Rust workspace template for AI-assisted development.

## Philosophy

Traditional Rust projects balance strictness with flexibility. Vulcan enforces a single correct pattern to prevent AI from generating inconsistent code, and ships the building blocks to make that code fast.

Key principles:

- Force simpler code through aggressive complexity thresholds
- Ban error handling footguns (`unwrap`, `expect`, `panic`)
- Require fast alternatives (`parking_lot::Mutex`, `foldhash::HashMap`)
- Enforce exhaustive pattern matching and explicit error handling
- Use `tracing` instead of `println!` for logging
- Allocate less, keep types small, keep the hot path hot, parallelize deterministically
- No `unsafe`: the `vulcan` crate is `#![forbid(unsafe_code)]`

Target use case: AI code generation where flexibility leads to bugs, inconsistent patterns and slow code.

## Quick Start

```bash
gh repo create my-project --template deltartificial/vulcan --private --clone
```

Or click "Use this template" on GitHub.

## Features

### Strict Linting

- 120+ clippy lints enabled (nursery, pedantic, restriction groups)
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
| `std::collections::HashMap`    | `foldhash::HashMap` or `rustc_hash::FxHashMap`   |
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
| clippy              | Nightly with 120+ lints                  |
| rustfmt             | Nightly, `imports_granularity = "Crate"` |
| cargo-deny          | License auditing, OpenSSL banned         |
| cargo-semver-checks | API stability verification               |
| cargo-fuzz          | Fuzzing infrastructure                   |
| dprint              | TOML/Markdown/JSON formatting            |
| typos               | Spell checking                           |
| nextest             | Test runner with retries                 |
| cross               | Cross-compilation (aarch64, riscv64)     |
| miri                | Undefined behavior detection             |
| CodSpeed            | Instruction-count benchmarks on every PR |

## CI

### Lint Workflow

Clippy, rustfmt, documentation, typos, cargo-deny, dprint, miri, fuzzing, semver checks, external types, cargo-sort, `--cfg vulcan_nightly` build.

### Test Workflow

Nextest, doctests, MSRV check (1.93), Valgrind, AddressSanitizer, minimal versions, multi-platform (Linux/macOS/Windows), cross-compilation.

### Bench Workflow

CodSpeed runs `vulcan/benches` in simulation mode and reports the diff against `main` on each PR.

### PR Title Enforcement

Conventional commit format required.

All workflows use sccache, mold linker, and alls-green gate jobs.

## Configuration

| File                  | Purpose                                            |
| --------------------- | -------------------------------------------------- |
| `Cargo.toml`          | Workspace lints, profiles, MSRV (1.93)             |
| `clippy.toml`         | Strict thresholds, disallowed types/methods/macros |
| `Cross.toml`          | Cross-compilation setup                            |
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
