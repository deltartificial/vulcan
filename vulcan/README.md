# vulcan

Safe, benchmarked performance building blocks for hot-path Rust code: arenas, interning, typed
indices, bit sets, byte scanning, ordered parallelism and deferred diagnostics.

Each module is independent. Keep the ones your project needs, delete the rest, and add your own
crates next to this one in the workspace.

See [docs/PERFORMANCE.md](../docs/PERFORMANCE.md) for the full list and the coding conventions that
go with them.
