# Vulcan

Ultra-strict, performance-first Rust workspace template for financial and blockchain systems, and
any code where every microsecond and every allocation counts.

**[TigerStyle], enforced.** Vulcan takes the coding rules of the [TigerBeetle] database, translates
them to Rust, and turns every rule a tool can see into a lint or a check. Built for AI-assisted
development.

[TigerStyle]: https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/TIGER_STYLE.md
[TigerBeetle]: https://github.com/tigerbeetle/tigerbeetle
[Power of Ten]: https://spinroot.com/gerard/pdf/P10.pdf

## Who This Is For

Vulcan is not a general-purpose starter. Use it when speed is the product:

- **Financial systems**: matching engines, order management, pricing and risk engines, market data
  feed handlers, payment and settlement rails
- **High-frequency and algorithmic trading**: strategy engines, order routers, exchange gateways,
  tick-to-trade paths where tail latency decides the fill
- **Blockchain**: clients and nodes, execution engines and VMs, mempools, indexers, provers, MEV
  searchers and builders
- **Compilers and language tooling**: parsers, type checkers, interpreters, static analyzers,
  formatters and linters
- Any service or library where allocation, layout and parallelism decide the result

These domains share the same constraints: predictable latency, no allocation on the hot path,
deterministic output, and a crash rather than a silently wrong result. Vulcan encodes them as lints,
style checks and building blocks.

For a CRUD service, a CLI or a prototype, the lints and conventions here cost more than they give:
pick a lighter template.

## Philosophy

Traditional Rust projects balance strictness with flexibility. Vulcan enforces a single correct
pattern to prevent AI from generating inconsistent code, and ships the building blocks to make that
code fast.

The rules start from [TigerStyle], the style TigerBeetle wrote for a financial transactions database
that must never lose or corrupt a balance, itself grounded in NASA's [Power of Ten] rules for
safety-critical code. Where TigerStyle relies on discipline, Vulcan relies on tools: each rule is a
rustc or clippy lint, a `cargo xtask style` check, or, when no tool can see it, a review item in
[docs/CODING.md](docs/CODING.md).

Key principles, in priority order: safety, then performance, then developer experience.

- Assert arguments, results and invariants: two assertions per function on average, in every build
- Put a limit on everything: no recursion, `for` loops only, fixed-capacity collections
- Allocate at start-up, then seal the allocator: a later allocation aborts
- Crash on a broken invariant: release builds abort on panic and check integer overflow
- Force simpler code: 70-line functions, shallow nesting, one condition per `if`
- Ban error handling footguns (`unwrap`, `expect`, `panic`) and ignored results
- Pass options explicitly: no `Default`, named arguments, enums instead of `bool` arguments
- Require fast alternatives (`parking_lot::Mutex`, `rustc_hash::FxHashMap`)
- Keep types small, keep the hot path hot, parallelize deterministically
- No `unsafe`, except the one `GlobalAlloc` implementation that seals the allocator

The full rules, and the tool that enforces each one, are in [docs/CODING.md](docs/CODING.md).

Target use case: AI code generation on performance-critical code, where flexibility leads to bugs,
inconsistent patterns and slow code.

## Quick Start

```bash
gh repo create my-project --template deltartificial/vulcan --private --clone
```

Or click "Use this template" on GitHub.

## Features

### Strict Linting

- Every clippy group, `restriction` included, and every rustc lint that fits the 2024 edition; the
  few lints turned off contradict another one, and each says why; every warning is denied
- Ban `unwrap_used`, `expect_used`, `panic` in library code, and unused results
- Explicitly sized integers and no `as` casts: `u32` crosses into `usize` through `vulcan::convert`
- Division with explicit rounding (`integer_division`, `modulo_arithmetic`)
- `else if` chains end with `else`; no single-letter names; no shadowing
- Deterministic output: iterating a hash map in a `for` loop is flagged
- Require exhaustive pattern matching (no wildcard enum arms)

### Aggressive Thresholds

```toml
too-many-lines-threshold = 70 # Default: 100
excessive-nesting-threshold = 4 # Default: off
cognitive-complexity-threshold = 15 # Default: 25
too-many-arguments-threshold = 5 # Default: 7
max-fn-params-bools = 1 # Default: 3
pass-by-value-size-limit = 16 # Default: 256
```

### Code and Style Checks

`cargo xtask style` covers what rustfmt and clippy do not: 100 columns in every tracked file,
comments and doc comments written as sentences, at least two assertions per function on average, no
recursion, no `while` or `loop`, no `if !`, one condition per `if`, a description on every test,
helpers named after their caller, named arguments, and `main` first. `cargo xtask commits` checks
that every commit has a conventional title and a body that explains it.

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
| `unwrap_or_default`                      | handle the missing value explicitly          |
| `std::sync::mpsc::channel`               | `sync_channel` with a capacity               |
| `std::thread::sleep`                     | wait on a condition, with a bound            |
| `Default::default`                       | name every option at the call site           |
| `debug_assert!` and its variants         | `assert!`: assertions run in every build     |
| `println!`, `eprintln!`                  | `tracing::info!`, `tracing::error!`          |

## Performance

The `vulcan` crate is a toolbox of safe, benchmarked building blocks. See
[docs/PERFORMANCE.md](docs/PERFORMANCE.md) for the full list and the coding conventions that go with
them.

| Module       | Building blocks                                                               |
| ------------ | ----------------------------------------------------------------------------- |
| `allocator`  | `mimalloc` / `jemalloc` backend, `StaticAllocator` sealed after start-up      |
| `arena`      | Bump arenas (one per thread), `ArenaBox`, `SliceStack`, no `Drop` in arenas   |
| `index`      | `newtype_index!` (4-byte ids, `Option<Id>` still 4 bytes) and `IndexVec`      |
| `intern`     | `Symbol` interner, `symbols!` consts, slice and flagged interners, `Interned` |
| `layout`     | `static_assert_size!` budgets, 8-byte `Span` aligned to 4                     |
| `cache`      | Caches picked by key type (dense slots for indices, no hashing)               |
| `collect`    | `collect_and_apply`: 8 items at most, never touching the heap                 |
| `bit_set`    | `DenseBitSet` over typed indices, inline up to 128 bits                       |
| `bytes`      | 256-entry byte class table, `memchr` cursor, `StaticFinder`                   |
| `hint`       | `likely`, `unlikely`, `cold_path`, `outline`                                  |
| `sync`       | `WorkerPool` made at start-up, ordered `map`, size threshold, `ShardedMap`    |
| `diagnostic` | `ErrorGuaranteed`, diagnostics buffered per task and replayed in order        |
| `never`      | `Never` on stable, `into_ok` / `into_value` for infallible results            |
| `bounded`    | `BoundedVec` and `BoundedQueue`: fixed capacity, never reallocate             |
| `division`   | `divide_exact`, `divide_floor`: integer division with explicit rounding       |
| `convert`    | The only conversions between `u32` and `usize`, checked                       |

Nightly-only fast paths are enabled with `RUSTFLAGS="--cfg vulcan_nightly"` (not a feature, so
`--all-features` keeps working on stable).

## Tooling

| Tool                | Purpose                                  |
| ------------------- | ---------------------------------------- |
| clippy              | Nightly, every group                     |
| xtask               | Automation and style checks, in Rust     |
| rustfmt             | Nightly, `imports_granularity = "Crate"` |
| cargo-deny          | License auditing, OpenSSL banned         |
| cargo-semver-checks | API stability verification               |
| cargo-fuzz          | Fuzz targets built and smoke-run in CI   |
| dprint              | TOML, Markdown, JSON, YAML formatting    |
| typos               | Spell checking                           |
| nextest             | Test runner with retries                 |
| cross               | Local cross-compilation (`Cross.toml`)   |
| miri                | Undefined behavior detection             |
| zepter              | Feature propagation across crates        |
| CodSpeed            | Instruction-count benchmarks on every PR |

## CI

### Lint Workflow

Clippy, rustfmt, documentation, style checks, typos, cargo-deny, dprint, unused dependencies,
feature powerset, feature propagation, cargo-sort, wasm32-wasip1 check, miri, fuzzing,
`--cfg vulcan_nightly` build, commit messages. Every step that is not a published action is a
`cargo xtask` task, so CI and a local run do the same thing. Semver checks run as advisory until the
crates are published.

### Test Workflow

Nextest on Linux, macOS and Windows, doctests, MSRV check (1.93), minimal versions,
AddressSanitizer.

### Bench Workflow

CodSpeed runs `vulcan/benches` in simulation mode and reports the diff against `main` on each PR.

### PR Title Enforcement

Conventional commit format required.

Build jobs use kache and the mold linker; `lint` and `test` end with an alls-green gate job.

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
| `xtask/`              | Build, lint and test tasks; style checks           |
| `docs/CODING.md`      | Coding rules and how each one is enforced          |
| `docs/PERFORMANCE.md` | Performance building blocks and hot-path rules     |

## Build Profiles

| Profile     | Use Case                                               |
| ----------- | ------------------------------------------------------ |
| `dev`       | Fast iteration with optimized dependencies             |
| `release`   | Optimized; `panic = "abort"`, integer overflow checked |
| `maxperf`   | Distribution binary (fat LTO, codegen-units=1)         |
| `profiling` | Release with debug symbols                             |
| `bench`     | Benchmarking                                           |

## Development

```bash
cargo xtask help             # List every task
cargo xtask lint             # Format, clippy, typos, dprint, style checks
cargo xtask test             # Unit tests and doctests
cargo xtask pr               # Full pre-PR check
cargo xtask bench            # Run benchmarks
cargo xtask maxperf-nightly  # maxperf with nightly fast paths
```

Commit format: `type(scope): description`, with a body that says what changed and why.

Types: `feat`, `fix`, `chore`, `test`, `bench`, `perf`, `refactor`, `docs`, `ci`, `revert`, `deps`

## Credits

- [TigerStyle] and [TigerBeetle], for the rules this template enforces
- Gerard J. Holzmann, for NASA's [Power of Ten]

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT)
at your option.
