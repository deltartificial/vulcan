# Coding Rules

These rules apply to every line of the workspace: library, automation, tests, benchmarks and fuzz
targets. Most are enforced by a tool, and the table at the end says which. Reviewers, human or AI,
check the rest.

## 1. Goals

- There are three goals, in a fixed order: **safety first, performance second, developer experience
  third**. All three matter; when they conflict, that order decides.
- Judge a change by one question: does it make the code safer, faster or easier to work with, or
  does it make it less so? That question is why there are rules at all.
- Style is more than readability. Readable code is the minimum, and only a means to correct, fast
  code.

## 2. Simplicity and Technical Debt

- **Simplicity is the last revision, not the first draft.** A simple design takes thought, several
  passes, many sketches and sometimes a rewrite. Budget for that.
- **Spend the thinking up front.** An hour or a day of design is worth weeks or months in
  production, where every problem costs many times more to fix than in design.
- **Ask "what could go wrong?" while the code is still being designed**, not "what is wrong?" once
  it runs. Code, like steel, is cheaper to change while it is hot.
- **Fix a showstopper when you find it.** A copy that can spike latency, an algorithm whose cost
  grows exponentially with the input: neither is left for later.
- **Merge no known defect.** Do it right the first time: there may be no second time. What ships may
  lack features, but what exists meets the goals, which is the only way to know that progress is
  progress.

## 3. Control Flow and Limits

- **Only simple, explicit control flow.** No hidden jumps, at most four nested blocks per function,
  and a cognitive complexity of 15 at most.
- **No recursion.** A function never calls itself, directly or through others. Rewrite recursion as
  a loop over an explicit stack with a fixed capacity, so every execution that should be bounded is
  bounded. The check follows the calls between the functions of one file; a cycle that crosses files
  is left to review.
- **A minimum of abstractions, and only excellent ones.** An abstraction is never free: it costs
  reading time, and it can leak. Add one only when it is the best model of the domain.
- **Put a limit on everything.** Every loop has a fixed upper bound and every queue a fixed capacity
  (`vulcan::bounded`, `std::sync::mpsc::sync_channel`), so a broken assumption fails fast instead of
  looping forever or spiking latency. Work handed to a thread pool is a queue too: a
  `sync::WorkerPool` takes one batch at a time, of at most `batch_item_count_max` items. Loops are
  `for` loops over a range or a collection: there is no `while` and no `loop`. A loop that must
  never end (an event loop) returns `!`, which asserts that it cannot terminate.
- **Explicitly sized integers.** Indices, counts and capacities are `u32` (or the type the domain
  needs), never the architecture-specific `usize`. `usize` appears only where Rust requires it, to
  index a slice or read its length, and it crosses into `u32` through `vulcan::convert` alone.

## 4. Assertions

Assertions find programmer errors. They are not error handling: an **operating error** (bad input, a
full disk, a refused connection) is expected and must be handled with `Result`; a **failed
assertion** means the program itself is wrong. The only correct way to handle code that is wrong is
to crash, so assertions run in every build, release included (`debug_assert!` is banned), release
builds abort on a panic, and integer overflow is checked in the workspace crates. An assertion turns
a catastrophic correctness bug into a crash, which is found sooner and costs less, and it gives
fuzzing something to find.

- **Assert every argument, return value, precondition, postcondition and invariant.** A function
  does not operate on data it has not checked. The code averages at least **two assertions per
  function**, and a function of several statements holds at least one: only a function that is a
  single expression, a delegation or an accessor, has no data of its own to check.
- **No assertion restates the line above it.** `let sum = a + b; assert!(sum == a + b)` checks
  nothing. Assert what could be wrong: an argument, an invariant, the state left behind.
- **Pair assertions.** For every property, find at least two code paths that can check it: before
  data is written and after it is read back, where an index is created and where it is used.
- **Assert what must hold and what must not.** Check the expected state and also what must not
  happen: the bits next to a changed bit are untouched, a refused push changed nothing. Bugs gather
  at the boundary between valid and invalid.
- **One condition per assertion.** `assert!(a); assert!(b);` is simpler to read and says which one
  failed; `assert!(a && b)` does not.
- **Implications as `if`.** Write "a implies b" as `if a { assert!(b, ..) }`, never as
  `assert!(!a || b, ..)`.
- **Assert what the program controls.** The state of the file system, the network or another process
  is not an invariant: check it and return an error.
- **Assert the relations of compile-time constants.** `const _: () = assert!(..)` and
  `static_assert_size!` check the design before the program runs: a mask is a power of two, a
  threshold is below another, a hot type keeps its size.
- **An obvious assertion beats a comment** when the fact it states is critical and surprising.
- **Every assertion has a message** that names the invariant.
- **Keep assertions cheap by design, not by switching them off.** Separate the control plane from
  the data plane: validate a batch once where it enters, then run the per-item loop with only the
  checks the loop itself needs (see section 11).
- **Assertions do not replace understanding.** Work in this order: build a precise model of the
  code, write the model down as assertions, write the code and comments that explain and justify it
  to the reviewer, and let fuzzing be the last line of defense. A fuzzer shows that bugs exist,
  never that they do not.

## 5. Memory

- **All memory is allocated at startup.** A program derives every capacity from its configured
  limits, allocates its buffers, tables and arenas once, and then seals the allocator
  (`vulcan::allocator::StaticAllocator::seal`). No memory is allocated, or freed and reallocated,
  after initialization: an allocation after the seal aborts the process. This avoids unpredictable
  latency and use-after-free, and it forces designs that consider every memory use up front.
- **Collections have a fixed capacity.** They are created with their capacity and never grow past
  it: `vulcan::bounded` refuses an item when full, and the other collections of `vulcan` assert that
  they stay within the capacity they were created with.
- **Reuse buffers.** `clear` and `mem::swap` with a scratch buffer, never a new `Vec` per iteration.
- **Pass values above 16 bytes by reference.** A by-value copy of a large struct is a hidden
  `memcpy` on every call, and passing by reference catches accidental copies. Values of 16 bytes or
  less go by value, in registers.
- **Build large values in place.** A value too large to move cheaply is created once and then reset
  or filled through `&mut` (`clear`, `reset`, `fill`), not rebuilt and moved. Once a field is built
  in place, the container that holds it is too.
- **No buffer bleeds.** A buffer that is reused or only partly filled still holds what it held
  before: the bytes past the new length, and the padding inside and between values. Zero them, or
  hand out the filled part only (`&buffer[..length]`), before the buffer is written to disk, sent,
  hashed or compared. Stale bytes leak what the previous use stored, and they make two runs on the
  same input differ.
- **Threads are made at startup too.** A `vulcan::sync::WorkerPool` spawns its threads and allocates
  its slots when it is created, and maps into buffers of the caller, so a batch allocates nothing.
  What a thread allocates on first use (its arena of a `ThreadArenas`, a thread-local table) is
  allocated by the start handler of the pool, before the seal.

## 6. Functions

- **70 lines at most.** A function fits on one screen; scrolling breaks the reader's view of it.
- **Few parameters, a simple return type, the logic in between.** Prefer `()` to `bool`, `bool` to
  an integer, an integer to `Option`, and `Option` to `Result`: each step adds a case every caller
  must handle, and that cost spreads up the call chain.
- **Centralize control flow: branch in the parent, loop in the leaves.** When splitting a function,
  keep every `if` and `match` in the parent and move straight-line work into helpers. One function
  owns the control flow; the others do not branch on it.
- **Centralize state: the parent owns it.** The parent keeps the state in local variables, helpers
  compute what should change and return it, and the parent applies the change. Leaf functions are
  pure.
- **Hot loops in standalone functions.** Extract a hot loop into a function that takes slices and
  primitives, not `self`, so the compiler need not prove that fields stay unchanged and the reader
  sees every input (`bit_set::union_words`).
- **Run to completion.** A function does not suspend halfway (no `async`), so the preconditions it
  asserted still hold at its end.

## 7. Variables

- **Smallest scope.** Declare a variable where it is first needed, and let it go out of scope when
  it is no longer needed. Keep few variables in scope at once.
- **Compute and check close to use.** The further a check is from the use it protects, in lines or
  in time, the more likely something changes in between.
- **No duplicated state, no aliases.** Two copies of one fact eventually disagree. Derive one from
  the other (`Cursor::offset` is computed from the remaining input, not counted separately).
- **No shadowing.** A name means one thing in a scope.

## 8. Conditions and Errors

- **Split compound conditions.** Nested `if`/`else` makes every case visible; `a && b || c` hides
  some. There is no `&&` and no `||`, anywhere: several conditions are nested `if`s, or a `match`
  over the cases, also where they only compute a value (`if open { idle } else { false }`). A
  condition therefore cannot be compound, in place, over several lines or behind a name. An
  `else if` chain is a tree of `else { if .. }`, and it ends with an `else`.
- **Consider the `else`.** For every `if`, decide what happens when the condition is false, and
  handle or assert it.
- **State conditions positively.** `if index < count` is easier to check than
  `if !(index >= count)`, and it matches the way loops compare indices. There is no `if !`.
- **Handle every error.** No `unwrap`, `expect` or `panic!`. Every returned value is used, or
  discarded on purpose with a named binding (`let _report = ..`).
- **Test the error paths.** Most catastrophic failures in production come from error handling that
  was never run. Every error branch has a test that takes it.

## 9. Interfaces

- **Pass options explicitly.** Configuration types have no `Default`, and code never calls
  `Default::default`: call sites name every option, including those of library calls (a `memchr`
  finder states its prefilter, an arena its capacity), so a change of library default can never
  change a program silently. A suggested value is a named constant passed on purpose.
- **Named arguments.** Arguments that can be mixed up go in a struct with named fields: a function
  takes at most one argument of each type (two `u32`, two `&str`, two `&Path`), and an optional
  argument is a named field, never a positional `Option`. Enums replace booleans:
  `ThreadCount::Available` says what `true` hides.
- **Singletons go first, positionally.** Dependencies with a unique type (an arena, a context) are
  passed to constructors positionally, from the most general to the most specific.
- **Callbacks go last** in a parameter list, since they run last.
- **Do not react to external events directly.** The program runs at its own pace: it takes input in
  batches, at points it chooses, so its control flow stays its own and its work per unit of time
  stays bounded.

## 10. Naming

- **Find the right nouns and verbs.** A good name captures what a thing is or does, gives a crisp
  model, and shows that its author understood the domain. Take the time to find names that work
  together.
- **`snake_case`** for functions, variables, modules and files.
- **No abbreviations.** `predicate`, not `pred`; `formatter`, not `f`; `'arena`, not `'a`;
  `PARALLEL_LEN_MIN`, not `PAR_LEN_MIN`; a type parameter is a word too, `Item`, not `T`. Single
  letters are kept for integer counters of sorting and matrix code (`i`, `j`, `k`). Scripts and CI
  use long flags: `--workspace`, not `-w`; single letters are for typing at a prompt.
- **Acronyms in capitals**, where Rust's casing allows it: `SIMD`, `UTF8`.
- **Units and qualifiers last, most significant word first:** `timeout_ms_max`, `len_min`,
  `stack_size_bytes`, `INDEX_MAX`. Related names then line up and sort together.
- **Index, count and size are different things.** An index is 0-based, a count 1-based: the count
  after an index is the index plus one. A size is a count times a unit. Name them so (`index`,
  `item_count`, `size_bytes`) and convert between them by those rules only.
- **Infuse names with meaning.** `arena: &Bump` says who frees the memory; `allocator` does not.
- **Related names of equal length**, so related lines align: `source` and `target`, rather than
  `src` and `dest`.
- **Name helpers after their caller**: `parse` calls `parse_digits` and `parse_exponent`, and a
  callback passed by `read_sector` is `read_sector_callback`. The name shows the call history.
- **One meaning per name.** Do not reuse a term from one part of the system for another concept.
- **Nouns that work in prose.** A field named `pipeline` can head a section of a design document;
  `preparing` needs rephrasing first. Nouns also compose: `pipeline_len_max`.
- **Important things first.** A file opens with its main types and entry points, and a binary with
  `main`; helpers follow. A struct lists its fields, then its methods. When no order stands out,
  sort alphabetically, which units-last naming makes meaningful.

## 11. Performance by Design

- **Design for performance from the start.** The thousandfold wins are in the design, exactly when
  nothing can be measured yet. Profiling a finished system finds smaller wins at a higher price.
- **Sketch on paper first.** Estimate the cost on the four resources, network, disk, memory and CPU,
  each by bandwidth and latency. Being roughly right is enough to land near the best design.
- **Optimize the slowest resource first** (network, disk, memory, CPU), after weighting each by how
  often it is used: a cache miss repeated a million times can cost more than one disk sync.
- **Separate the control plane from the data plane.** Decide and validate once per batch, then
  process the batch in a tight loop. This keeps assertions cheap without giving up speed.
- **Batch.** Amortize every fixed cost (a system call, a lock, a thread wake-up, a cache miss) over
  many items.
- **Let the CPU sprint.** Give it large, predictable chunks of work: fewer branches and longer
  straight runs.
- **Be explicit with the compiler.** Do not rely on it to find the fast version; write it.

The hot-path conventions that follow from these rules are in [PERFORMANCE.md](PERFORMANCE.md).

## 12. Division

- **Division states its rounding.** `divide_exact` (asserts no remainder), `divide_floor` and
  `div_ceil` from `vulcan::division`; a bare `/` or `%` on integers is flagged.

## 13. Comments and Commits

- **Say why.** Code alone is not documentation: comments explain why the code is written the way it
  is. Show your workings. Every lint exception, every atomic `Ordering` and every `#[must_use]` says
  why.
- **Say how.** A test opens with a doc comment that states its goal and its method, so a reader can
  follow it or skip it without reading the body.
- **Comments are sentences.** A space after the marker, a capital letter first, a full stop last, or
  a colon before what follows. This holds for `//` comments, doc comments, list items and the `#`
  comments of TOML and YAML. A comment after code on the same line may be a phrase.
- **Commit messages inform.** The title follows `type(scope): description`, and the body, after a
  blank line, says what changed and why. A pull request description is not stored in the repository
  and does not show in `git blame`, so it never replaces the commit message.

## 14. Formatting

- `rustfmt` formats Rust, and `dprint` formats TOML, Markdown, JSON and YAML.
- Four spaces of indentation in every language.
- **100 columns at most, in every file, with no exception.** Use the width, never go past it, and
  let the formatter wrap after a trailing comma.
- Every `if` has braces.
- Leave a blank line before a resource is acquired (a lock guard, a file) and after the scope that
  releases it, so the pair stands out and a leak is easy to spot.

## 15. Dependencies and Tools

- **Every dependency earns its place.** A dependency brings supply-chain risk, safety and
  performance risk, and build time, and in a foundational library that cost multiplies through every
  user. Prefer the standard library. Each entry in `Cargo.toml` says which module uses it.
- **One toolbox.** Project automation is Rust code in `xtask`, run with `cargo xtask <task>`, not
  shell scripts or a `Makefile`. It runs on every platform, the compiler checks it, and nobody needs
  another tool to read it. Every CI step calls a `cargo` command.
- **Keep trying things out, and have fun.** Small is a feature.

## Enforcement

| Rule                                        | Enforced by                                     |
| ------------------------------------------- | ----------------------------------------------- |
| Strictest compiler setting, no warnings     | every rustc lint, every clippy group            |
| Function length, parameters, nesting        | thresholds in `clippy.toml`                     |
| No recursion, no `while`/`loop`, no `if !`  | `cargo xtask style`                             |
| Bounded queues                              | `mpsc::channel` and `thread::sleep` banned      |
| The same lints on the fuzz targets          | `cargo xtask clippy-fuzz`, a test of `xtask`    |
| Sized integers, lossless casts              | `as_conversions`, `vulcan::convert`             |
| Two assertions per function, in every build | `cargo xtask style`; `debug_assert!` banned     |
| No function of several statements is blind  | `cargo xtask style`                             |
| One condition and a message per assertion   | `cargo xtask style`, `missing_assert_message`   |
| Static allocation, thread pool included     | `StaticAllocator`, `tests/static_allocation.rs` |
| Errors handled, results used                | `unwrap_used`, `panic`, `unused_results`        |
| No `&&`, no `\|\|`, no `if !`               | `cargo xtask style`, `else_if_without_else`     |
| Options passed explicitly                   | `Default::default` banned                       |
| Named arguments                             | `cargo xtask style`, `max-fn-params-bools`      |
| Large values by reference                   | `large_types_passed_by_value` at 16 bytes       |
| Explicit division                           | `integer_division`, `modulo_arithmetic`         |
| No abbreviations, no shadowing              | `disallowed-names`, `shadow_*`, lifetimes lint  |
| Type parameters are words                   | `cargo xtask style`                             |
| No buffer bleeds                            | review                                          |
| Helpers named after their caller, `main`    | `cargo xtask style`                             |
| Described tests, comments as sentences      | `cargo xtask style`, `missing_docs_*`           |
| Every exception says why                    | `allow_attributes_without_reason`               |
| 100 columns                                 | `cargo xtask style`, rustfmt, dprint            |
| Commit messages                             | `cargo xtask commits`, PR title check           |
| Crash on a broken invariant                 | `panic = "abort"`, `overflow-checks`            |
| Valid, invalid and boundary inputs          | unit tests, fuzz targets in `fuzz/`             |
