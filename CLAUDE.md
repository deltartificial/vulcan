# Vulcan

Vulcan is a Rust workspace project.

## Development Workflow

### Build

```bash
cargo build                  # default members only
cargo build --workspace      # entire workspace
make build                   # via Makefile
make maxperf                 # max performance build (fat LTO, codegen-units=1)
```

### Test

```bash
cargo nextest run --workspace          # unit tests
cargo test --doc --workspace           # doctests (nextest doesn't run these)
make test                              # all tests via Makefile
```

### Lint

```bash
cargo +nightly fmt --all               # format
cargo +nightly clippy --workspace --all-targets --all-features  # lint
make lint                              # all linters (fmt, clippy, typos, deny, dprint)
make pr                                # full pre-PR check
```

### Pre-PR Checklist

1. `make lint` passes
2. `make test` passes
3. `cargo doc --workspace --all-features --no-deps --document-private-items` builds without warnings
4. PR title follows conventional commits: `type(scope): description`

## Code Style

- Follow Rust 2024 edition idioms
- Use `tracing` for logging, never `println!` or `eprintln!` in library code
- Prefer `#[must_use]` on functions returning values that should not be ignored
- Comments should explain **why**, not **what**
- Keep public APIs well-documented with `///` doc comments

## Performance Conventions

Vulcan's purpose is speed. Follow [docs/PERFORMANCE.md](docs/PERFORMANCE.md#coding-conventions) on
every hot path, in short:

- Inline the common-case check, move the rest to a `#[cold]` function
- Use `#[inline(never)]` for rare cases with large locals in recursive code, and say why
- No `format!`/`to_string` before an error branch: format after `hint::cold_path()` or in a
  `#[cold]` function
- Keep `Result` small: box large errors, return `diag::ErrorGuaranteed` once reported
- Reuse buffers (`clear`, `mem::swap`) instead of allocating per iteration
- Go parallel only through `vulcan::sync` helpers (size thresholds, ordered results), and emit
  diagnostics from parallel code only through `diag::DiagCtxt::par_map` / `DiagBatch`
- Pin the size of hot types with `static_assert_size!`
- No `String`/`Vec`/`Rc`/`Arc`/`Box<dyn>` in hot nodes: use `Symbol`, arena slices, `ArenaBox`
- No `SeqCst`; justify every atomic `Ordering` in a comment
- `dyn` only on cold paths; build searchers (`StaticFinder`, regexes) once, never in a loop

## Commit Convention

Conventional Commits format: `type(scope): description`

Allowed types: `feat`, `fix`, `chore`, `test`, `bench`, `perf`, `refactor`, `docs`, `ci`, `revert`, `deps`

Breaking changes: append `!` after type, e.g. `feat!: remove deprecated API`

## Git Workflow

- All changes go through pull requests — never push directly to main
- PRs are squash-merged so the commit shows: `type(scope): description (#PR)`
- Keep commit history clean and linear

## Architecture

Rust workspace with strict tooling:

- **clippy** (nightly, nursery lints enabled)
- **rustfmt** (nightly, `imports_granularity = "Crate"`)
- **cargo-deny** (license auditing, ban openssl)
- **dprint** (TOML/markdown/JSON formatting)
- **typos** (spell checking with hex address exclusions)
- **nextest** (test runner with retry/timeout policies)
- **cross** (cross-compilation for aarch64, riscv64)

## CI

CI runs on every PR and push to main:

- **lint.yml**: clippy, fmt, docs, typos, deny, dprint, with `lint-success` gate
- **test.yml**: nextest, doctest, MSRV (1.93) check, with `test-success` gate
- **pr-title.yml**: conventional commit enforcement on PR titles
