# Vulcan

Ultra-strict Rust workspace template for AI-assisted development.

## Philosophy

Traditional Rust projects balance strictness with flexibility. Vulcan enforces a single correct pattern to prevent AI from generating inconsistent code.

Key principles:

- Force simpler code through aggressive complexity thresholds
- Ban error handling footguns (`unwrap`, `expect`, `panic`)
- Require fast alternatives (`parking_lot::Mutex`, `foldhash::HashMap`)
- Enforce exhaustive pattern matching and explicit error handling
- Use `tracing` instead of `println!` for logging

Target use case: AI code generation where flexibility leads to bugs and inconsistent patterns.

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

- `std::sync::Mutex` → `parking_lot::Mutex` (1.5x faster, no poisoning)
- `std::sync::RwLock` → `parking_lot::RwLock` (5x faster)
- `std::collections::HashMap` → `foldhash::HashMap` or `rustc_hash::FxHashMap`
- `std::collections::LinkedList` → `Vec` or `VecDeque`

### Disallowed Methods and Macros

- `std::env::var` → `std::env::var_os` (explicit error handling)
- `println!`, `eprintln!` → `tracing::info!`, `tracing::error!`

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

## CI

### Lint Workflow

Clippy, rustfmt, documentation, typos, cargo-deny, dprint, miri, fuzzing, semver checks, external types, cargo-sort.

### Test Workflow

Nextest, doctests, MSRV check (1.93), Valgrind, AddressSanitizer, minimal versions, multi-platform (Linux/macOS/Windows), cross-compilation.

### PR Title Enforcement

Conventional commit format required.

All workflows use sccache, mold linker, and alls-green gate jobs.

## Configuration

| File            | Purpose                                            |
| --------------- | -------------------------------------------------- |
| `Cargo.toml`    | Workspace lints, profiles, MSRV (1.93)             |
| `clippy.toml`   | Strict thresholds, disallowed types/methods/macros |
| `Cross.toml`    | Cross-compilation setup                            |
| `dprint.json`   | Multi-format code formatting                       |
| `typos.toml`    | Spell checking configuration                       |
| `.editorconfig` | Editor consistency                                 |
| `deny.toml`     | License auditing                                   |

## Build Profiles

| Profile     | Use Case                                       |
| ----------- | ---------------------------------------------- |
| `dev`       | Fast iteration with optimized dependencies     |
| `release`   | Standard optimized builds                      |
| `maxperf`   | Distribution binary (fat LTO, codegen-units=1) |
| `profiling` | Release with debug symbols                     |
| `bench`     | Benchmarking                                   |

## Development

```bash
make lint    # Run all linters
make test    # Run all tests
make pr      # Full pre-PR check
```

Commit format: `type(scope): description`

Types: `feat`, `fix`, `chore`, `test`, `bench`, `perf`, `refactor`, `docs`, `ci`, `revert`, `deps`

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.
