# vulcan

Opinionated Rust workspace template with strict tooling, CI, and conventional commits.

## Usage

Click **"Use this template"** on GitHub, or:

```bash
gh repo create my-project --template deltartificial/vulcan --private --clone
```

## What's Included

### Tooling

| Tool | Purpose |
|------|---------|
| **clippy** | Nightly, 70+ nursery lints enabled |
| **rustfmt** | Nightly, `imports_granularity = "Crate"` |
| **cargo-deny** | License auditing, advisory checking, OpenSSL banned |
| **dprint** | TOML / Markdown / JSON formatting |
| **typos** | Spell checking with hex address exclusions |
| **nextest** | Test runner with retry and timeout policies |
| **cross** | Cross-compilation (aarch64, riscv64) |
| **git-cliff** | Changelog generation from conventional commits |
| **cargo-release** | Shared workspace versioning |

### CI

| Workflow | Jobs |
|----------|------|
| **lint.yml** | clippy, fmt, docs, typos, deny, dprint |
| **test.yml** | nextest, doctest, MSRV (1.93) |
| **pr-title.yml** | Conventional commit enforcement |

All workflows use `sccache`, `mold` linker, and `alls-green` gate jobs.

### Build Profiles

| Profile | Use Case |
|---------|----------|
| `dev` | Fast iteration, line-tables-only debug info |
| `release` | Optimized, `panic = "unwind"`, thin LTO |
| `maxperf` | Distribution binary, fat LTO, `panic = "abort"` |
| `profiling` | Release + full debug symbols |
| `hivetests` | Integration tests with optimizations |

### Git Workflow

- All changes go through **pull requests** (squash merge only)
- Commit format: `type(scope): description (#PR)`
- Types: `feat`, `fix`, `chore`, `test`, `bench`, `perf`, `refactor`, `docs`, `ci`, `revert`, `deps`
- Auto-delete branches on merge, auto-merge enabled

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.
