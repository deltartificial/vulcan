# Vulcan 🔒

**Ultra-strict Rust workspace template designed for AI-assisted development.**

Vulcan enforces **120+ lints** with **80% restrictiveness** to eliminate creative freedom and force code to follow **one correct pattern**. Built to prevent AI from choosing between multiple implementations by making only one choice valid.

## Philosophy

Traditional Rust projects balance strictness with flexibility. Vulcan takes a different approach:

- **Force simpler code** — aggressive complexity thresholds (15 cognitive complexity, 5 max arguments, 75 max lines per function)
- **Eliminate footguns** — ban `unwrap`, `expect`, `panic`, `std::sync::Mutex`, `HashMap` without fast alternatives
- **One correct way** — disallow slow types (`LinkedList`), force `parking_lot` sync primitives, require `tracing` instead of `println!`
- **Zero ambiguity** — exhaustive pattern matching, explicit error handling, no shadowing, no wildcards

**Target use case**: AI code generation where flexibility leads to bugs and inconsistent patterns.

## Quick Start

```bash
# Create a new project from this template
gh repo create my-project --template deltartificial/vulcan --private --clone

# Or click "Use this template" on GitHub
```

## Lint System

### Summary

| Category | Count | Description |
|----------|-------|-------------|
| **TIER 1** | 7 lints | Critical enforcement (no unwrap/expect/panic, explicit clones) |
| **TIER 2** | 11 lints | Best practices (exhaustive matching, no shadowing) |
| **Nursery** | 70+ lints | Enabled clippy nursery group |
| **Pedantic** | 15+ lints | Selected pedantic lints |
| **Standard** | 20+ lints | Rust + clippy default groups |
| **Total** | **~120 lints** | **80% restrictiveness** |

### TIER 1: Critical Enforcement (Restriction Group)

```toml
unwrap_used = "deny"                    # Force Result/Option handling
expect_used = "deny"                    # Force Result/Option handling
panic = "warn"                          # Use Result<T, E> instead
unimplemented = "warn"                  # No placeholder code
clone_on_ref_ptr = "warn"               # Explicit Rc::clone(), Arc::clone()
missing_assert_message = "warn"         # Document invariants
allow_attributes_without_reason = "warn" # Document why lints are disabled
```

### TIER 2: Best Practices (Restriction + Pedantic)

```toml
wildcard_enum_match_arm = "warn"        # Exhaustive pattern matching
map_err_ignore = "warn"                 # Preserve error context
let_underscore_must_use = "warn"        # Don't ignore important results
tests_outside_test_module = "warn"      # Proper test organization
shadow_reuse = "warn"                   # No variable shadowing
shadow_same = "warn"                    # No variable shadowing
empty_drop = "warn"                     # Drop implementations must do work
deref_by_slicing = "warn"               # Explicit slice conversions
create_dir = "warn"                     # Use create_dir_all for robustness
checked_conversions = "warn"            # Safe numeric conversions
default_trait_access = "warn"           # Explicit Default::default()
```

## Strict Thresholds (`clippy.toml`)

### Complexity Limits

```toml
cognitive-complexity-threshold = 15     # Default: 25 — Force simpler logic
type-complexity-threshold = 100         # Default: 250 — Force simpler types
too-many-arguments-threshold = 5        # Default: 7 — Fewer parameters
too-many-lines-threshold = 75           # Default: 100 — Shorter functions
```

### Memory and Performance

```toml
pass-by-value-size-limit = 128          # Force &T for types >128 bytes
trivial-copy-size-limit = 32            # Force Copy bound for generics >32 bytes
array-size-threshold = 8192             # Prevent huge stack arrays (>8KB)
```

### Naming

```toml
min-ident-chars-threshold = 2           # Reject 1-char identifiers
allowed-idents-below-min-chars = [      # Allow common loop/math vars
  "i", "j", "k", "x", "y", "z", "w", "h", "n", "m", "_"
]
```

## Disallowed Types and Methods

### Sync Primitives (Use `parking_lot`)

```toml
# std::sync::Mutex → parking_lot::Mutex (1.5x faster, no poisoning)
# std::sync::RwLock → parking_lot::RwLock (5x faster, no poisoning)
```

### Collections (Use Fast Alternatives)

```toml
# std::collections::HashMap → foldhash::HashMap or rustc_hash::FxHashMap
# std::collections::HashSet → foldhash::HashSet or rustc_hash::FxHashSet
# std::collections::LinkedList → Vec or VecDeque (cache-friendly)
```

### Macros (Use `tracing`)

```toml
# Disallowed: println!, eprintln!, print!, eprint!
# Required: tracing::info!, tracing::warn!, tracing::error!, etc.
```

### Methods

```toml
# std::env::var → std::env::var_os (explicit error handling)
```

## Tooling

| Tool | Purpose |
|------|---------|
| **clippy** | Nightly, 120+ lints (nursery, pedantic, restriction) |
| **rustfmt** | Nightly, `imports_granularity = "Crate"` |
| **cargo-deny** | License auditing, advisory checking, OpenSSL banned |
| **cargo-semver-checks** | API stability verification |
| **cargo-fuzz** | Fuzzing infrastructure |
| **dprint** | TOML / Markdown / JSON formatting |
| **typos** | Spell checking with hex address exclusions |
| **nextest** | Test runner with retry and timeout policies |
| **cross** | Cross-compilation (aarch64, riscv64) |
| **miri** | Undefined behavior detection |
| **git-cliff** | Changelog generation from conventional commits |

## CI Workflows

### Lint (`lint.yml`)

- ✅ Clippy (all targets, all features)
- ✅ Rustfmt check
- ✅ Documentation build (no warnings)
- ✅ Typos spell checking
- ✅ cargo-deny (licenses, advisories, banned deps)
- ✅ dprint formatting
- ✅ Miri (undefined behavior detection)
- ✅ Fuzzing (cargo-fuzz check)
- ✅ Semver checks (API compatibility)
- ✅ External types (no foreign types in public API)
- ✅ cargo-sort (dependency ordering)

### Test (`test.yml`)

- ✅ Nextest (unit tests with retries)
- ✅ Doctests
- ✅ MSRV check (Rust 1.93)
- ✅ Valgrind (memory leak detection)
- ✅ AddressSanitizer (memory safety)
- ✅ Minimal versions (lower bound testing)
- ✅ Multi-platform (Linux, macOS, Windows)
- ✅ Cross-compilation (aarch64, riscv64)

### PR Title (`pr-title.yml`)

- ✅ Conventional commit format enforcement

All workflows use:
- **sccache** for incremental compilation
- **mold** linker for faster builds
- **alls-green** gate jobs for required checks

## Build Profiles

| Profile | Use Case |
|---------|----------|
| `dev` | Fast iteration, line-tables-only debug info, heavy deps optimized (serde, regex) |
| `release` | Optimized, `panic = "unwind"`, thin LTO, cold crates optimized for size |
| `maxperf` | Distribution binary, fat LTO, `panic = "abort"`, codegen-units = 1 |
| `profiling` | Release + full debug symbols for profiling |
| `hivetests` | Integration tests with optimizations |
| `bench` | Benchmarking (inherits profiling) |

## Configuration Files

| File | Purpose |
|------|---------|
| `Cargo.toml` | Workspace lints, profiles, MSRV (1.93) |
| `clippy.toml` | Strict thresholds, disallowed types/methods/macros |
| `Cross.toml` | Cross-compilation setup for aarch64, riscv64 |
| `dprint.json` | Multi-format code formatting (rustfmt exec plugin) |
| `typos.toml` | Spell checking with hex address exclusions |
| `.editorconfig` | Editor consistency (4 spaces, 100 line width) |
| `deny.toml` | License auditing, OpenSSL ban |

## Git Workflow

- All changes go through **pull requests** (squash merge only)
- Commit format: `type(scope): description (#PR)`
- Types: `feat`, `fix`, `chore`, `test`, `bench`, `perf`, `refactor`, `docs`, `ci`, `revert`, `deps`
- Auto-delete branches on merge
- Linear history enforced

## Pre-PR Checklist

```bash
# Run all linters
make lint

# Run all tests
make test

# Build documentation
cargo doc --workspace --all-features --no-deps --document-private-items

# Verify PR title follows conventional commits
# type(scope): description
```

## Why Vulcan?

Traditional Rust projects allow multiple valid implementations:
- Error handling: `unwrap()`, `expect()`, `?`, `match`, `if let`, `unwrap_or()`
- Collections: `HashMap`, `FxHashMap`, `FnvHashMap`, `foldhash::HashMap`
- Sync primitives: `Mutex`, `RwLock`, `parking_lot` equivalents
- Iteration: `for`, `while`, `loop`, iterators, recursion

**This flexibility is great for humans, problematic for AI.** AI models often choose inconsistent patterns, leading to:
- Mixed error handling styles in the same codebase
- Performance regressions from slow defaults (`std::HashMap`)
- Bugs from panics in production code
- Unnecessary complexity from over-engineering

**Vulcan eliminates choice.** By banning slow/unsafe options and enforcing strict thresholds, it forces AI to generate consistent, high-quality code by default.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.
