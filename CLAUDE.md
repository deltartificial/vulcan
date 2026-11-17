# Vulcan

Vulcan is a performance-first Rust workspace template for hot-path code: financial systems,
high-frequency trading, blockchain clients and execution engines, compilers and parsers. The
`vulcan` crate holds the performance building blocks; speed and strictness come before convenience.

## Development Workflow

All automation is Rust code in `xtask/`: `cargo xtask help` lists every task.

### Build

```bash
cargo build --workspace      # entire workspace
cargo xtask maxperf          # max performance build (fat LTO, codegen-units=1, native CPU)
```

### Test

```bash
cargo xtask test             # unit tests (nextest) and doctests
```

### Lint

```bash
cargo xtask lint             # rustfmt, clippy, typos, dprint, style checks
cargo xtask style            # text and code rules: width, comments, assertions, loops, names
cargo xtask pr               # full pre-PR check (adds deny, udeps, hack, zepter, tests, docs)
```

### Pre-PR Checklist

1. `cargo xtask pr` passes
2. PR title follows conventional commits: `type(scope): description`
3. Every commit has a body that says what changed and why (`cargo xtask commits`)

## Code Style

The full rules are in [docs/CODING.md](docs/CODING.md). In short:

- Safety first, performance second, developer experience third
- Assert arguments, return values and invariants: at least two assertions per function on average,
  at least one in every function of several statements, one condition per assertion, each with a
  message; `debug_assert!` is banned
- Put a limit on everything: no recursion, no `while`/`loop`, fixed-capacity collections
- Allocate at start-up; collections take their capacity when created and never grow past it; a
  reused buffer is zeroed or sliced to its filled part before it leaves the program
- `u32` for indices, counts and capacities; `usize` only at the slice boundary, via
  `vulcan::convert`, never `as`
- Functions of 70 lines at most; branch in the parent, loop in the leaves; leaf functions pure
- No `&&` and no `||`: nest `if`s or match on the cases; no `if !`; `else if` chains end with `else`
- Pass options explicitly: no `Default`, never `Default::default`; at most one argument per type and
  no positional `Option` (use a struct); enums instead of `bool` arguments
- No abbreviations, type parameters included (`Item`, not `T`); units and qualifiers last
  (`len_min`, `stack_size_bytes`); a helper called by one function is named after it (`parse` calls
  `parse_digits`)
- Every test opens with a doc comment that says what it checks and how
- Division states its rounding: `divide_exact`, `divide_floor`, `div_ceil`
- Comments are sentences that explain **why**; 100 columns at most in every file
- Use `tracing` for logging, never `println!` or `eprintln!` in library code
- Keep public APIs documented with `///` doc comments

## Performance Conventions

Vulcan's purpose is speed. Follow [docs/PERFORMANCE.md](docs/PERFORMANCE.md#coding-conventions) on
every hot path, in short:

- Inline the common-case check, move the rest to a `#[cold]` function
- Use `#[inline(never)]` for rare cases with large locals in recursive code, and say why
- No `format!`/`to_string` before an error branch: format after `hint::cold_path()` or in a
  `#[cold]` function
- Keep `Result` small: box large errors, return `diagnostic::ErrorGuaranteed` once reported
- Reuse buffers (`clear`, `mem::swap`) instead of allocating per iteration
- Go parallel only through a `vulcan::sync::WorkerPool` created at start-up (size threshold, ordered
  results, no allocation per batch), and emit diagnostics from parallel code only through a
  `DiagnosticBatch` returned with the output and committed in order
- Pin the size of hot types with `static_assert_size!`
- No `String`/`Vec`/`Rc`/`Arc`/`Box<dyn>` in hot nodes: use `Symbol`, arena slices, `ArenaBox`
- No `SeqCst`; justify every atomic `Ordering` in a comment
- `dyn` only on cold paths; build searchers (`StaticFinder`, regexes) once, never in a loop

## Commit Convention

Conventional Commits format: `type(scope): description`, followed by a body that says what changed
and why. The pull request description does not reach the history, so the commit message carries the
explanation.

Allowed types: `feat`, `fix`, `chore`, `test`, `bench`, `perf`, `refactor`, `docs`, `ci`, `revert`,
`deps`

Breaking changes: append `!` after type, e.g. `feat!: remove deprecated API`

## Git Workflow

- All changes go through pull requests — never push directly to main
- PRs are squash-merged so the commit shows: `type(scope): description (#PR)`
- Keep commit history clean and linear

## Architecture

Rust workspace with strict tooling:

- **clippy** (nightly; every group, `restriction` included)
- **xtask** (project automation in Rust, and the style checks clippy lacks)
- **rustfmt** (nightly, `imports_granularity = "Crate"`)
- **cargo-deny** (license auditing, ban openssl)
- **dprint** (TOML, Markdown, JSON and YAML formatting)
- **typos** (spell checking with hex address exclusions)
- **nextest** (test runner with retry/timeout policies)
- **cross** (local cross-compilation for aarch64, riscv64; not run in CI)

## CI

CI runs on every PR and push to main:

- **lint.yml**: clippy, fmt, docs, style, typos, deny, dprint, udeps, feature checks, wasm, miri,
  fuzzing, commit messages, with `lint-success` gate
- **test.yml**: nextest (Linux/macOS/Windows), doctest, MSRV (1.93), minimal versions, ASan, with
  `test-success` gate
- **bench.yml**: CodSpeed instruction-count benchmarks
- **pr-title.yml**: conventional commit enforcement on PR titles
